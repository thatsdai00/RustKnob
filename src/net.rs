// WinHTTP pieces shared by the uploader, the downloads (updates, lyrics, mouse pictures) and ntfy.

use std::ffi::c_void;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub target: String, // path and query
}

/// "https://host[:port]/path?query" (or plain http).
pub fn parse_url(url: &str) -> Option<Url> {
    let (secure, rest) = match url.strip_prefix("http://") {
        Some(r) => (false, r),
        None => (true, url.strip_prefix("https://")?),
    };
    let (authority, target) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = host_port(authority, secure);
    Some(Url { secure, host, port, target })
}

/// "host" or "host:port"; without a port, the scheme's own.
pub fn host_port(authority: &str, secure: bool) -> (String, u16) {
    match authority.rsplit_once(':').and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p))) {
        Some((h, p)) => (h.to_string(), p),
        None => (authority.to_string(), if secure { 443 } else { 80 }),
    }
}

/// A session that goes out through the system's proxy settings.
pub unsafe fn session(agent: PCWSTR) -> *mut c_void {
    WinHttpOpen(agent, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0)
}

/// Opens a request on `url`; the caller closes both handles.
pub unsafe fn open_request(session: *mut c_void, method: PCWSTR, url: &Url) -> Option<(*mut c_void, *mut c_void)> {
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

pub unsafe fn close(connection: *mut c_void, request: *mut c_void) {
    let _ = WinHttpCloseHandle(request);
    let _ = WinHttpCloseHandle(connection);
}

/// Sends the request with `headers` ("Name: value\r\n" lines) and `body`, and waits for the answer.
pub unsafe fn send(request: *mut c_void, headers: &str, body: &[u8]) -> bool {
    let wide: Vec<u16> = headers.encode_utf16().collect();
    let data = if body.is_empty() { None } else { Some(body.as_ptr() as *const c_void) };
    WinHttpSendRequest(request, Some(&wide), data, body.len() as u32, body.len() as u32, 0).is_ok() && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok()
}

pub unsafe fn status_code(request: *mut c_void) -> u32 {
    let (mut code, mut size) = (0u32, 4u32);
    let _ = WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut code as *mut u32 as *mut c_void), &mut size, std::ptr::null_mut());
    code
}

/// The answer's body, read to its end; empty when it grows past `limit` bytes.
pub unsafe fn read_body(request: *mut c_void, limit: usize) -> Vec<u8> {
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
    body
}
