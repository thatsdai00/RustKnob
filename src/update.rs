// Updates from GitHub: looks at the latest release of the project, and when it is newer than this build
// downloads RustKnob.exe, checks it against the SHA-256 GitHub lists for that file, and hands it to the capsule,
// which starts it with `--update` (it closes this copy, installs itself over it and starts again).
//
// Runs on a thread of its own, now and then (see island.rs), and only when "update automatically" is on.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::json;

pub const REPO: &str = "thatsdai00/RustKnob";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const ASSET: &str = "RustKnob.exe";
const MAX_BYTES: usize = 40 * 1024 * 1024; // far above the real size: a guard against a runaway download

/// The downloaded, verified exe waiting to be installed.
static READY: Mutex<Option<(String, PathBuf)>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// (version, path) of a downloaded update, taken once.
pub fn take_ready() -> Option<(String, PathBuf)> {
    READY.lock().ok().and_then(|mut r| r.take())
}

/// wparam of the message a check posts.
pub const UP_TO_DATE: usize = 0;
pub const READY_TO_INSTALL: usize = 1; // take_ready() has it
pub const FAILED: usize = 2; // offline, or GitHub did not answer as expected

/// Checks GitHub and downloads a newer release if there is one; posts `msg` to `hwnd` with how it went.
pub fn check(hwnd: isize, msg: u32) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return; // a check is already under way (its answer comes)
    }
    std::thread::spawn(move || {
        let outcome = match unsafe { latest() } {
            None => FAILED,
            Some((tag, _)) if parse_version(&tag) <= parse_version(VERSION) => UP_TO_DATE,
            Some((tag, body)) => match unsafe { download(&tag, &body) } {
                Some(found) => {
                    if let Ok(mut r) = READY.lock() {
                        *r = Some(found);
                    }
                    READY_TO_INSTALL
                }
                None => FAILED,
            },
        };
        unsafe {
            let _ = PostMessageW(HWND(hwnd as _), msg, WPARAM(outcome), LPARAM(0));
        }
        BUSY.store(false, Ordering::Release);
    });
}

/// "1.2.3" or "v1.2.3" as numbers; extra parts and suffixes are ignored.
fn parse_version(text: &str) -> Option<[u32; 3]> {
    let mut parts = text.trim().trim_start_matches(['v', 'V']).split('.').map(|p| p.split(['-', '+']).next().unwrap_or("").parse::<u32>().ok());
    Some([parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)])
}

/// The latest release's tag, and GitHub's description of it.
unsafe fn latest() -> Option<(String, String)> {
    let body = String::from_utf8(get(&format!("https://api.github.com/repos/{REPO}/releases/latest"), 1 << 20)?).ok()?;
    let tag = json::str_field(&body, "tag_name")?;
    parse_version(&tag)?;
    Some((tag, body))
}

/// Downloads the release's exe and checks it against the SHA-256 GitHub lists for it.
unsafe fn download(tag: &str, body: &str) -> Option<(String, PathBuf)> {
    // the asset's own fields follow its name
    let at = body.find(&format!("\"name\":\"{ASSET}\"")).or_else(|| body.find(&format!("\"name\": \"{ASSET}\"")))?;
    let section = &body[at..];
    let url = json::str_field(section, "browser_download_url")?;
    let digest = json::str_field(section, "digest")?;
    let expected = digest.strip_prefix("sha256:")?.to_lowercase();
    if !url.starts_with("https://") || expected.len() != 64 {
        return None;
    }

    let bytes = get(&url, MAX_BYTES)?;
    if bytes.len() < 100_000 || !bytes.starts_with(b"MZ") || sha256(&bytes)? != expected {
        return None; // not what GitHub says it published
    }
    let dir = std::env::temp_dir().join("RustKnob-update");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(ASSET);
    std::fs::write(&path, &bytes).ok()?;
    Some((tag.trim_start_matches(['v', 'V']).to_string(), path))
}

/// SHA-256 as lowercase hex.
unsafe fn sha256(data: &[u8]) -> Option<String> {
    let mut alg = BCRYPT_ALG_HANDLE::default();
    if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, PCWSTR::null(), BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).is_err() {
        return None;
    }
    let mut hash = BCRYPT_HASH_HANDLE::default();
    let mut digest = [0u8; 32];
    let ok = BCryptCreateHash(alg, &mut hash, None, None, 0).is_ok()
        && BCryptHashData(hash, data, 0).is_ok()
        && BCryptFinishHash(hash, &mut digest, 0).is_ok();
    if !hash.is_invalid() {
        let _ = BCryptDestroyHash(hash);
    }
    let _ = BCryptCloseAlgorithmProvider(alg, 0);
    ok.then(|| digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// GET `url` (redirects are followed), at most `limit` bytes; None on any failure or a status other than 200.
pub fn get(url: &str, limit: usize) -> Option<Vec<u8>> {
    unsafe { get_raw(url, limit) }
}

unsafe fn get_raw(url: &str, limit: usize) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("https://")?;
    let (host, target) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let session = WinHttpOpen(&HSTRING::from(format!("RustKnob/{VERSION}")), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
    if session.is_null() {
        return None;
    }
    let mut result = None;
    let connection = WinHttpConnect(session, &HSTRING::from(host), 443, 0);
    if !connection.is_null() {
        let request = WinHttpOpenRequest(connection, w!("GET"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
        if !request.is_null() {
            let headers: Vec<u16> = "Accept: application/vnd.github+json, application/octet-stream\r\n".encode_utf16().collect();
            let _ = WinHttpSetTimeouts(request, 15_000, 20_000, 30_000, 60_000); // 0 would let a name lookup hang for ever
            if WinHttpSendRequest(request, Some(&headers), None, 0, 0, 0).is_ok() && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok() {
                let (mut code, mut size) = (0u32, 4u32);
                let _ = WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut code as *mut u32 as *mut c_void), &mut size, std::ptr::null_mut());
                if code == 200 {
                    let mut body = Vec::new();
                    let mut chunk = vec![0u8; 1 << 16];
                    loop {
                        let mut available = 0u32;
                        if WinHttpQueryDataAvailable(request, &mut available).is_err() || available == 0 {
                            break;
                        }
                        let want = available.min(chunk.len() as u32);
                        let mut read = 0u32;
                        if WinHttpReadData(request, chunk.as_mut_ptr() as *mut c_void, want, &mut read).is_err() || read == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..read as usize]);
                        if body.len() > limit {
                            body.clear();
                            break;
                        }
                    }
                    result = (!body.is_empty()).then_some(body);
                }
            }
            let _ = WinHttpCloseHandle(request);
        }
        let _ = WinHttpCloseHandle(connection);
    }
    let _ = WinHttpCloseHandle(session);
    result
}
