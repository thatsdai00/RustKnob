// ntfy.sh (or a self-hosted server) push notifications.
// A worker thread keeps one streaming HTTP request open (`/<topic>/json`, one JSON object per
// line) through WinHTTP, and wakes the main window for every message. It reconnects on its own.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::Config;
use crate::{json, net};

pub struct Notice {
    pub title: String,
    pub message: String,
}

static LATEST: Mutex<Option<Notice>> = Mutex::new(None);
// Each (re)start bumps GEN; an older listener sees the change and exits. ACTIVE is the open request,
// closed by a restart so the old listener's blocked read ends right away.
static GEN: AtomicU32 = AtomicU32::new(0);
static ACTIVE: AtomicIsize = AtomicIsize::new(0);
/// Wakes a listener waiting out its retry delay when a restart happens.
static RESTART: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// The most recent notification, taken once.
pub fn take() -> Option<Notice> {
    LATEST.lock().ok().and_then(|mut n| n.take())
}

/// (Re)starts listening for the configured topic; posts `msg` to `hwnd` for every notification.
/// Any earlier listener stops. Does nothing more when the topic is empty.
pub fn start(cfg: &Config, hwnd: isize, msg: u32) {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    RESTART.1.notify_all();
    let active = ACTIVE.swap(0, Ordering::SeqCst);
    if active != 0 {
        unsafe {
            let _ = WinHttpCloseHandle(active as *mut c_void);
        }
    }
    if cfg.ntfy_topic.trim().is_empty() {
        return;
    }
    let (server, topic, token) = (cfg.ntfy_server.clone(), cfg.ntfy_topic.trim().to_string(), cfg.ntfy_token.clone());
    std::thread::spawn(move || {
        let mut backoff = 3;
        while GEN.load(Ordering::SeqCst) == gen {
            let got_data = unsafe { stream(&server, &topic, &token, hwnd, msg, gen) };
            backoff = if got_data { 3 } else { (backoff * 2).min(60) };
            // wait out the delay, or until a restart says to stop
            if let Ok(g) = RESTART.0.lock() {
                let _ = RESTART.1.wait_timeout_while(g, Duration::from_secs(backoff), |_| GEN.load(Ordering::SeqCst) == gen);
            }
        }
    });
}

/// "https://ntfy.sh" (the scheme may be left out; a server under a path, "https://host/ntfy", works too)
/// -> the topic's stream on it
fn stream_url(server: &str, topic: &str) -> Option<net::Url> {
    let s = server.trim();
    let mut url = net::parse_url(s).or_else(|| net::parse_url(&format!("https://{s}")))?;
    url.target = format!("{}/{}/json", url.target.trim_end_matches('/'), crate::json::url_encode(topic));
    Some(url)
}

/// One connection; returns true when it received anything (so the retry delay can reset).
unsafe fn stream(server: &str, topic: &str, token: &str, hwnd: isize, msg: u32, gen: u32) -> bool {
    let session = net::session(w!("RustKnob"));
    if session.is_null() {
        return false;
    }
    let mut got_data = false;
    if let Some((connection, request)) = stream_url(server, topic).and_then(|url| net::open_request(session, w!("GET"), &url)) {
        if !token.is_empty() {
            let header: Vec<u16> = format!("Authorization: Bearer {token}").encode_utf16().collect();
            let _ = WinHttpAddRequestHeaders(request, &header, WINHTTP_ADDREQ_FLAG_ADD);
        }
        // keepalives arrive about every 45 s: two minutes of silence means the link is dead
        let _ = WinHttpSetTimeouts(request, 0, 30_000, 30_000, 120_000);
        // an older listener must not take the slot of a newer one (a restart could not close the newer one then)
        let mine = GEN.load(Ordering::SeqCst) == gen && ACTIVE.compare_exchange(0, request as isize, Ordering::SeqCst, Ordering::SeqCst).is_ok();
        if mine
            && GEN.load(Ordering::SeqCst) == gen
            && WinHttpSendRequest(request, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok()
            && net::status_code(request) == 200 // an error page (wrong token, no such topic) is not a stream: back off
        {
            got_data = read_lines(request, hwnd, msg, gen);
        }
        // a restart may already have closed it
        if !mine || ACTIVE.compare_exchange(request as isize, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            let _ = WinHttpCloseHandle(request);
        }
        let _ = WinHttpCloseHandle(connection);
    }
    let _ = WinHttpCloseHandle(session);
    got_data
}

unsafe fn read_lines(request: *mut c_void, hwnd: isize, msg: u32, gen: u32) -> bool {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut any = false;
    while GEN.load(Ordering::SeqCst) == gen {
        // waits until data has arrived; reading the whole buffer size would block until it filled up
        let mut available = 0u32;
        if WinHttpQueryDataAvailable(request, &mut available).is_err() || available == 0 {
            return any;
        }
        let mut read = 0u32;
        let want = available.min(chunk.len() as u32);
        if WinHttpReadData(request, chunk.as_mut_ptr() as *mut c_void, want, &mut read).is_err() || read == 0 {
            return any;
        }
        any = true;
        pending.extend_from_slice(&chunk[..read as usize]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if let Some(notice) = parse_line(&String::from_utf8_lossy(&line)) {
                if let Ok(mut latest) = LATEST.lock() {
                    *latest = Some(notice);
                }
                let _ = PostMessageW(HWND(hwnd as _), msg, WPARAM(0), LPARAM(0));
            }
        }
    }
    any
}

/// Only `"event":"message"` lines count (the stream also carries open / keepalive events).
fn parse_line(line: &str) -> Option<Notice> {
    if json::str_field(line, "event")? != "message" {
        return None;
    }
    let message = json::str_field(line, "message").unwrap_or_default();
    let title = json::str_field(line, "title").unwrap_or_default();
    if message.is_empty() && title.is_empty() {
        return None;
    }
    Some(Notice { title, message })
}
