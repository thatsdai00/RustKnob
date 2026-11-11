// Text on the Windows clipboard.

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;

/// Another program may hold the clipboard for a moment: try a few times.
unsafe fn open() -> bool {
    for _ in 0..5 {
        if OpenClipboard(HWND::default()).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

pub fn get() -> Option<String> {
    unsafe {
        if !open() {
            return None;
        }
        let mut out = None;
        if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if !ptr.is_null() {
                let cap = (GlobalSize(hglobal) / 2).min(8192); // never past the end of the block
                let mut len = 0;
                while len < cap && *ptr.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
                let _ = GlobalUnlock(hglobal);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

pub fn set(text: &str) -> bool {
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = wide.len() * 2;
        if !open() {
            return false;
        }
        let _ = EmptyClipboard();
        let mut ok = false;
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(mem);
                // the clipboard owns the memory once SetClipboardData succeeds
                ok = SetClipboardData(CF_UNICODETEXT, HANDLE(mem.0)).is_ok();
            }
            if !ok {
                let _ = GlobalFree(mem); // still ours
            }
        }
        let _ = CloseClipboard();
        ok
    }
}
