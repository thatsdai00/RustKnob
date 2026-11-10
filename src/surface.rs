// A 32-bit top-down DIB that a window's scene is drawn into, then pushed to a layered window.

use std::ffi::c_void;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Surface {
    dib: Option<(HDC, HBITMAP, *mut u32)>,
    pub pw: usize,
    pub ph: usize,
}

impl Surface {
    pub const fn new() -> Surface {
        Surface { dib: None, pw: 0, ph: 0 }
    }

    pub unsafe fn ensure(&mut self, pw: usize, ph: usize) {
        if self.dib.is_some() && self.pw == pw && self.ph == ph {
            return;
        }
        self.free();
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pw as i32,
                biHeight: -(ph as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            SelectObject(dc, bmp);
            self.dib = Some((dc, bmp, bits as *mut u32));
            (self.pw, self.ph) = (pw, ph);
        }
        ReleaseDC(None, screen);
    }

    pub unsafe fn free(&mut self) {
        if let Some((dc, bmp, _)) = self.dib.take() {
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(dc);
        }
        (self.pw, self.ph) = (0, 0);
    }

    /// The pixels, valid until the next `ensure` / `free`.
    pub fn pixels(&self) -> Option<&'static mut [u32]> {
        let (_, _, bits) = self.dib?;
        Some(unsafe { std::slice::from_raw_parts_mut(bits, self.pw * self.ph) })
    }

    /// Like `present`, but tells the compositor which part of the surface changed (device pixels).
    pub unsafe fn present_dirty(&self, hwnd: HWND, x: i32, y: i32, dirty: (usize, usize, usize, usize)) {
        let Some((dc, _, _)) = self.dib else { return };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (pos, size, from) = (POINT { x, y }, SIZE { cx: self.pw as i32, cy: self.ph as i32 }, POINT { x: 0, y: 0 });
        let rect = RECT { left: dirty.0 as i32, top: dirty.1 as i32, right: dirty.2 as i32, bottom: dirty.3 as i32 };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &pos,
            psize: &size,
            hdcSrc: dc,
            pptSrc: &from,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: &rect,
        };
        let _ = UpdateLayeredWindowIndirect(hwnd, &info);
    }

    pub unsafe fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let Some((dc, _, _)) = self.dib else { return };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            HDC::default(),
            Some(&POINT { x, y }),
            Some(&SIZE { cx: self.pw as i32, cy: self.ph as i32 }),
            dc,
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}
