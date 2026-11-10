// Frame pacing: while something moves, a small thread waits for the compositor's vertical blank (DwmFlush)
// and posts one message per frame to the capsule. Frames then land exactly on the display's refresh rate,
// which a timer cannot do (a 15 ms timer against a 16.7 ms display drops or doubles a frame every few
// frames: that is the judder). The thread sleeps on a condition variable whenever nothing animates.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::*};
use std::sync::{Condvar, Mutex, Once};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Fast displays (144, 175 Hz) are driven at every vertical blank that keeps us at or under this.
const MAX_FPS: f32 = 100.0;

/// Posted once per frame while the pump runs.
pub const WM_APP_FRAME: u32 = WM_APP + 24;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static EVERY_MS: AtomicU32 = AtomicU32::new(0); // 0: as often as MAX_FPS allows
static PENDING: AtomicBool = AtomicBool::new(false); // a frame message is on its way
static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static SPAWN: Once = Once::new();

/// Starts (or retunes) the pump. `every_ms` 0 = the display's refresh rate (up to MAX_FPS), else at most that often.
pub fn start(hwnd: HWND, every_ms: u32) {
    TARGET.store(hwnd.0 as isize, Relaxed);
    EVERY_MS.store(every_ms, Relaxed);
    SPAWN.call_once(|| {
        std::thread::Builder::new().name("frames".into()).spawn(run).ok();
    });
    if let Ok(mut on) = GATE.0.lock() {
        if !*on {
            *on = true;
            GATE.1.notify_one();
        }
    }
}

pub fn stop() {
    if let Ok(mut on) = GATE.0.lock() {
        *on = false;
    }
}

/// The main thread took the frame message: the next one may be posted.
pub fn taken() {
    PENDING.store(false, Release);
}

fn run() {
    let mut last_post = Instant::now();
    let mut blank_ms = 16.7f32; // length of one vertical blank, measured
    let mut last_flush = Instant::now();
    loop {
        {
            let Ok(mut on) = GATE.0.lock() else { return };
            while !*on {
                on = match GATE.1.wait(on) {
                    Ok(g) => g,
                    Err(_) => return,
                };
                last_post = Instant::now();
            }
        }
        let before = Instant::now();
        if unsafe { DwmFlush() }.is_err() || before.elapsed() < Duration::from_micros(500) {
            std::thread::sleep(Duration::from_millis(8)); // no compositor: fall back to a steady beat
        }
        let now = Instant::now();
        let gap = (now - last_flush).as_secs_f32() * 1000.0;
        last_flush = now;
        if gap < 50.0 {
            blank_ms += (gap - blank_ms) * 0.2; // smoothed
        }
        let every_ms = match EVERY_MS.load(Relaxed) {
            0 => 1000.0 / MAX_FPS,
            ms => ms as f32,
        };
        // half a refresh of slack, so "every 33 ms" lands on every other vblank of a 60 Hz display
        if (now - last_post).as_secs_f32() * 1000.0 + blank_ms / 2.0 < every_ms {
            continue;
        }
        last_post = now;
        if !PENDING.swap(true, AcqRel) {
            unsafe {
                let _ = PostMessageW(HWND(TARGET.load(Relaxed) as _), WM_APP_FRAME, WPARAM(0), LPARAM(0));
            }
        }
    }
}
