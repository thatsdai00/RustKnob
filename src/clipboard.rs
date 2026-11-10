// Text on the Windows clipboard.

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;

pub fn get() -> Option<String> {
    unsafe {
        OpenClipboard(HWND::default()).ok()?;
        let mut out = None;
        if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if !ptr.is_null() {
                let mut len = 0;
                while *ptr.add(len) != 0 && len < 8192 {
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
        if OpenClipboard(HWND::default()).is_err() {
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
        }
        let _ = CloseClipboard();
        ok
    }
}
