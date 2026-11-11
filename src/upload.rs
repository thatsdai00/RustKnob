// fuckingfast.net uploader (the old ffuploader.py, in Rust on WinHTTP).
// Protocol: POST /api/upload {name, locationId} -> {data: {id, location}}; the file goes to
// `location` in 100 MB parts (PATCH with Upload-Part-Number, five at a time); POST /api/upload/<id>
// finishes it and returns the file id of the download link.

use std::collections::VecDeque;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, SendMessageW, WM_COPYDATA};

use crate::{clipboard, history, json};

const ORIGIN: &str = "https://fuckingfast.net";
const PART_SIZE: u64 = 100 * 1024 * 1024;
const SEND_SIZE: usize = 1024 * 1024;
const CONCURRENCY: usize = 5;
const MAX_RETRIES: u32 = 5;
const RETRY_WAIT: Duration = Duration::from_secs(5);
const FALLBACK_LOCATION: &str = "5urmb0o0baxx"; // Eastern Europe

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Idle,
    Uploading,
    Done,
    Failed,
}

#[derive(Clone)]
pub struct Status {
    pub phase: Phase,
    pub name: String, // file being uploaded
    pub index: usize, // 1-based
    pub count: usize,
    pub total: u64, // bytes of the current file
    pub links: Vec<String>,
    pub error: String,
}

static STATUS: Mutex<Status> =
    Mutex::new(Status { phase: Phase::Idle, name: String::new(), index: 0, count: 0, total: 0, links: Vec::new(), error: String::new() });
static QUEUE: Mutex<VecDeque<PathBuf>> = Mutex::new(VecDeque::new());
static WORKER: AtomicBool = AtomicBool::new(false);
/// Bytes of the current file that have reached the server.
pub static SENT: AtomicU64 = AtomicU64::new(0);

pub fn status() -> Status {
    STATUS.lock().map(|s| s.clone()).unwrap_or_else(|e| e.into_inner().clone())
}

/// (bytes of the current file, its 1-based number, how many) without copying the rest: read every frame.
pub fn progress() -> (u64, usize, usize) {
    let s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    (s.total, s.index, s.count)
}

fn update(f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

/// Queues files; a worker uploads them one after another. `msg` is posted to `hwnd` whenever the
/// phase or the current file changes (progress itself is read from SENT).
pub fn enqueue(paths: Vec<PathBuf>, hwnd: isize, msg: u32) {
    if paths.is_empty() {
        return;
    }
    let added = paths.len();
    if let Ok(mut q) = QUEUE.lock() {
        q.extend(paths);
    }
    update(|s| {
        if s.phase == Phase::Uploading {
            s.count += added;
        } else {
            *s = Status { phase: Phase::Uploading, count: added, ..s.clone() };
            s.links.clear();
            s.error.clear();
            s.index = 0;
        }
    });
    if !WORKER.swap(true, Ordering::SeqCst) {
        std::thread::spawn(move || worker(hwnd, msg));
    }
}

fn notify(hwnd: isize, msg: u32) {
    unsafe {
        let _ = PostMessageW(HWND(hwnd as _), msg, WPARAM(0), LPARAM(0));
    }
}

fn worker(hwnd: isize, msg: u32) {
    unsafe {
        let session = WinHttpOpen(w!("RustKnob"), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        loop {
            let next = QUEUE.lock().ok().and_then(|mut q| q.pop_front());
            let Some(path) = next else { break };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            SENT.store(0, Ordering::SeqCst);
            update(|s| {
                s.index += 1;
                s.name = name.clone();
                s.total = total;
            });
            notify(hwnd, msg);
            match upload_file(session as usize, &path, &name, total) {
                Ok(link) => {
                    history::append(&name, &link);
                    update(|s| s.links.push(link));
                }
                Err(error) => update(|s| s.error = error),
            }
        }
        if !session.is_null() {
            let _ = WinHttpCloseHandle(session);
        }
    }
    let finished = status();
    if !finished.links.is_empty() {
        clipboard::set(&finished.links.join("\r\n"));
    }
    update(|s| s.phase = if s.links.is_empty() { Phase::Failed } else { Phase::Done });
    WORKER.store(false, Ordering::SeqCst);
    notify(hwnd, msg);
    // a file queued after the loop saw the queue empty (but before WORKER dropped) still needs a worker
    if QUEUE.lock().is_ok_and(|q| !q.is_empty()) && !WORKER.swap(true, Ordering::SeqCst) {
        worker(hwnd, msg);
    }
}

// ---- HTTP ------------------------------------------------------------------

struct Url {
    secure: bool,
    host: String,
    port: u16,
    target: String, // path and query
}

fn parse_url(url: &str) -> Option<Url> {
    let (secure, rest) = match url.strip_prefix("http://") {
        Some(r) => (false, r),
        None => (true, url.strip_prefix("https://")?),
    };
    let (authority, target) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':').and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p))) {
        Some((h, p)) => (h.to_string(), p),
        None => (authority.to_string(), if secure { 443 } else { 80 }),
    };
    Some(Url { secure, host, port, target })
}

fn common_headers() -> String {
    format!("Accept: */*\r\nOrigin: {ORIGIN}\r\nReferer: {ORIGIN}/\r\nUser-Agent: Mozilla/5.0\r\n")
}

/// Opens a request on `url`; the caller closes both handles.
unsafe fn open_request(session: *mut c_void, method: PCWSTR, url: &Url) -> Option<(*mut c_void, *mut c_void)> {
    let connection = WinHttpConnect(session, &HSTRING::from(url.host.as_str()), url.port, 0);
    if connection.is_null() {
        return None;
    }
    let flags = if url.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
    let request = WinHttpOpenRequest(connection, method, &HSTRING::from(url.target.as_str()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags);
    if request.is_null() {
        let _ = WinHttpCloseHandle(connection);
        return None;
    }
    Some((connection, request))
}

unsafe fn status_code(request: *mut c_void) -> u32 {
    let mut code = 0u32;
    let mut size = 4u32;
    let _ = WinHttpQueryHeaders(
        request,
        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
        PCWSTR::null(),
        Some(&mut code as *mut u32 as *mut c_void),
        &mut size,
        std::ptr::null_mut(),
    );
    code
}

unsafe fn read_body(request: *mut c_void) -> String {
    let mut body = Vec::new();
    loop {
        let mut available = 0u32;
        if WinHttpQueryDataAvailable(request, &mut available).is_err() || available == 0 {
            break;
        }
        let mut chunk = vec![0u8; available.min(65536) as usize];
        let mut read = 0u32;
        if WinHttpReadData(request, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut read).is_err() || read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read as usize]);
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// A small request with a text body; returns (status, body). Status 0 means no connection.
unsafe fn call(session: usize, method: PCWSTR, url: &str, body: &[u8], json_body: bool) -> (u32, String) {
    let Some(parsed) = parse_url(url) else { return (0, String::new()) };
    let Some((connection, request)) = open_request(session as *mut c_void, method, &parsed) else { return (0, String::new()) };
    let mut headers = common_headers();
    if json_body {
        headers.push_str("Content-Type: application/json\r\n");
    }
    let wide: Vec<u16> = headers.encode_utf16().collect();
    let _ = WinHttpSetTimeouts(request, 0, 20_000, 30_000, 30_000);
    let mut result = (0, String::new());
    let sent = WinHttpSendRequest(request, Some(&wide), if body.is_empty() { None } else { Some(body.as_ptr() as *const c_void) }, body.len() as u32, body.len() as u32, 0);
    if sent.is_ok() && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok() {
        result = (status_code(request), read_body(request));
    }
    let _ = WinHttpCloseHandle(request);
    let _ = WinHttpCloseHandle(connection);
    result
}

fn api_error(status: u32, body: &str) -> String {
    json::str_field(body, "message").or_else(|| json::str_field(body, "error")).unwrap_or_else(|| if status == 0 { "no connection".to_string() } else { format!("HTTP {status}") })
}

unsafe fn init_once(session: usize, name: &str, location: &str) -> Result<(String, String), String> {
    let body = format!("{{\"name\":\"{}\",\"locationId\":\"{}\"}}", json::escape(name), json::escape(location));
    let (status, payload) = call(session, w!("POST"), &format!("{ORIGIN}/api/upload"), body.as_bytes(), true);
    if status >= 400 || status == 0 {
        return Err(api_error(status, &payload));
    }
    match (json::str_field(&payload, "id"), json::str_field(&payload, "location")) {
        (Some(id), Some(location)) => Ok((id, location)),
        _ => Err("the server gave no upload address".into()),
    }
}

/// The fallback region first; if the server refuses it, whatever region it lists first.
unsafe fn init_upload(session: usize, name: &str) -> Result<(String, String), String> {
    match init_once(session, name, FALLBACK_LOCATION) {
        Ok(v) => Ok(v),
        Err(first) => {
            let (status, payload) = call(session, w!("GET"), &format!("{ORIGIN}/api/locations"), &[], false);
            let other = if status < 400 { json::str_field(&payload, "id").filter(|id| id != FALLBACK_LOCATION) } else { None };
            match other {
                Some(id) => init_once(session, name, &id),
                None => Err(first),
            }
        }
    }
}

// ---- one part --------------------------------------------------------------

/// Sends bytes [start, start + size) of the file as one PATCH. Returns the HTTP status (0 = network error).
unsafe fn patch_part(session: usize, location: &str, path: &Path, start: u64, size: u64, part_no: u64, added: &mut u64, stop: &AtomicBool) -> (u32, String) {
    use std::io::{Read, Seek, SeekFrom};
    let Some(parsed) = parse_url(location) else { return (0, "bad upload address".into()) };
    let Some((connection, request)) = open_request(session as *mut c_void, w!("PATCH"), &parsed) else { return (0, "no connection".into()) };
    let headers = format!("{}Upload-Length: {size}\r\nUpload-Part-Number: {part_no}\r\n", common_headers());
    let wide: Vec<u16> = headers.encode_utf16().collect();
    let _ = WinHttpSetTimeouts(request, 0, 30_000, 60_000, 600_000);
    let mut result = (0, "network error".to_string());
    'send: {
        if WinHttpSendRequest(request, Some(&wide), None, 0, size as u32, 0).is_err() {
            break 'send;
        }
        let Ok(mut file) = std::fs::File::open(path) else { break 'send };
        if file.seek(SeekFrom::Start(start)).is_err() {
            break 'send;
        }
        let mut buf = vec![0u8; SEND_SIZE];
        let mut left = size;
        while left > 0 {
            if stop.load(Ordering::SeqCst) {
                result = (0, "stopped".into());
                break 'send;
            }
            let want = (left as usize).min(SEND_SIZE);
            let Ok(n) = file.read(&mut buf[..want]) else { break 'send };
            if n == 0 {
                break 'send;
            }
            let mut written = 0u32;
            if WinHttpWriteData(request, Some(buf.as_ptr() as *const c_void), n as u32, &mut written).is_err() {
                break 'send;
            }
            left -= n as u64;
            *added += n as u64;
            SENT.fetch_add(n as u64, Ordering::Relaxed);
        }
        if WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok() {
            result = (status_code(request), read_body(request));
        }
    }
    let _ = WinHttpCloseHandle(request);
    let _ = WinHttpCloseHandle(connection);
    result
}

/// One part with retries; progress made by a failed attempt is taken back.
fn upload_part(session: usize, location: &str, path: &Path, part_no: u64, start: u64, size: u64, stop: &AtomicBool) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err("stopped".into());
        }
        let mut added = 0u64;
        let (status, body) = unsafe { patch_part(session, location, path, start, size, part_no, &mut added, stop) };
        if status == 200 || status == 204 {
            return Ok(());
        }
        SENT.fetch_sub(added.min(SENT.load(Ordering::Relaxed)), Ordering::Relaxed);
        if status >= 500 || status == 0 {
            attempt += 1;
            if attempt > MAX_RETRIES {
                return Err(format!("part {part_no} failed: {}", api_error(status, &body)));
            }
            if stop.load(Ordering::SeqCst) {
                return Err("stopped".into()); // another part failed: no point waiting to retry
            }
            std::thread::sleep(RETRY_WAIT);
            continue;
        }
        return Err(format!("part {part_no} rejected: {}", api_error(status, &body)));
    }
}

// ---- one file --------------------------------------------------------------

unsafe fn upload_file(session: usize, path: &Path, name: &str, size: u64) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("file not found: {}", path.display()));
    }
    if name.chars().count() > 500 {
        return Err("file name is longer than 500 characters".into());
    }
    let (id, location) = init_upload(session, name)?;

    if size > 0 {
        let parts = size.div_ceil(PART_SIZE);
        let next = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let error: Mutex<Option<String>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..CONCURRENCY.min(parts as usize) {
                scope.spawn(|| loop {
                    let part = next.fetch_add(1, Ordering::SeqCst);
                    if part >= parts || stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let start = part * PART_SIZE;
                    let len = PART_SIZE.min(size - start);
                    if let Err(e) = upload_part(session, &location, path, part + 1, start, len, &stop) {
                        stop.store(true, Ordering::SeqCst);
                        if let Ok(mut slot) = error.lock() {
                            slot.get_or_insert(e);
                        }
                    }
                });
            }
        });
        if let Some(e) = error.into_inner().ok().flatten() {
            return Err(e);
        }
    }

    let (status, payload) = call(session, w!("POST"), &format!("{ORIGIN}/api/upload/{}", json::url_encode(&id)), &[], false);
    if status >= 400 || status == 0 {
        return Err(format!("could not finish: {}", api_error(status, &payload)));
    }
    // the response is {data: {id: <file id>}}; the first "id" is the file's
    let file_id = json::str_field(&payload, "id").ok_or("the server gave no download link")?;
    Ok(format!("{ORIGIN}/{}", json::url_encode(&file_id)))
}


// ---- handing files to an already running RustKnob -----------------------

/// Identifies our WM_COPYDATA payload (a list of NUL-terminated UTF-16 paths).
pub const COPY_MAGIC: usize = 0x444B_5550;

/// Sends the files to a running instance; false when there is none.
pub fn forward_to_running(paths: &[PathBuf]) -> bool {
    unsafe {
        let Ok(window) = FindWindowW(crate::CLASS, PCWSTR::null()) else { return false };
        let mut data: Vec<u16> = Vec::new();
        for path in paths {
            data.extend(path.as_os_str().encode_wide());
            data.push(0);
        }
        let cds = COPYDATASTRUCT { dwData: COPY_MAGIC, cbData: (data.len() * 2) as u32, lpData: data.as_mut_ptr() as *mut c_void };
        SendMessageW(window, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const COPYDATASTRUCT as isize));
        true
    }
}

/// Inverse of `forward_to_running`'s payload.
pub unsafe fn decode_paths(ptr: *const u16, len: usize) -> Vec<PathBuf> {
    if ptr.is_null() {
        return Vec::new();
    }
    std::slice::from_raw_parts(ptr, len)
        .split(|&c| c == 0)
        .filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(std::ffi::OsString::from_wide(part)))
        .collect()
}
