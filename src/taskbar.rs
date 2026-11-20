// The devices' batteries on the taskbar itself, just left of the notification area: "[mouse] 62%  [keyboard] 99%".
// A layered child window of the taskbar (Shell_TrayWnd), so it moves, hides and auto-hides with it and needs no
// polling for any of that; it only looks once a second whether the notification area moved (a new tray icon,
// the clock's width) and does nothing when it did not. It lives on a thread of its own: Explorer and the thread
// of a child window wait on each other, and the capsule's thread (drawing, input hooks) must never be the one
// Explorer waits for. On Windows 10 the task buttons are narrowed to make room; on Windows 11 they cannot be,
// and there is room on that side anyway.

use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::battery::Kind;
use crate::text::{raster_face, TextBmp};

/// One device as the strip shows it.
#[derive(Clone, PartialEq)]
pub struct Item {
    pub id: u32, // its card: a click posts tray::BATTERY_ID + id, as its tray icon does
    pub kind: Kind,
    pub bluetooth: bool,
    pub percent: Option<u8>,
    pub charging: bool,
    pub full: bool,
    pub dim: bool,
}

struct Shared {
    items: Vec<Item>,
    owner: isize, // clicks go there, as the tray icon's (tray::WM_TRAY)
    thread: u32,  // the strip's thread (0: none yet)
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { items: Vec::new(), owner: 0, thread: 0 });
/// Where each item is on the strip, as drawn last: (from, to) along the strip, its id.
static SPANS: Mutex<Vec<(i32, i32, u32)>> = Mutex::new(Vec::new());

/// The icon font: Segoe Fluent Icons (Windows 11), else Segoe MDL2 Assets (Windows 10), which share the glyphs
/// used here. GDI would quietly draw another font for a missing one: the installed fonts are asked once.
pub fn icon_face() -> &'static str {
    static FACE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FACE.get_or_init(|| {
        use windows::Win32::System::Registry::*;
        let fluent = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts"), w!("Segoe Fluent Icons (TrueType)"), RRF_RT_REG_SZ, None, None, None).is_ok() };
        if fluent {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        }
    })
}

/// The glyph a kind of device is drawn with.
pub fn glyph(kind: Kind, bluetooth: bool) -> char {
    match kind {
        Kind::Headset => '\u{E7F6}',
        Kind::Mouse => '\u{E962}',
        Kind::Keyboard => '\u{E765}',
        Kind::Gamepad => '\u{E7FC}',
        Kind::Phone => '\u{E8EA}',
        Kind::Other if bluetooth => '\u{E702}',
        Kind::Other => '\u{E772}',
    }
}
const WM_REDRAW: u32 = WM_APP + 1;
const TICK: usize = 1;

/// Shows `items` on the taskbar (none: the strip goes away; the thread stays, asleep in its message loop).
pub fn show(owner: HWND, items: Vec<Item>) {
    let Ok(mut s) = SHARED.lock() else { return };
    if s.items == items && s.thread != 0 {
        return;
    }
    (s.items, s.owner) = (items, owner.0 as isize);
    if s.thread == 0 {
        let Ok(handle) = std::thread::Builder::new().name("taskbar".into()).spawn(run) else { return };
        use std::os::windows::io::AsRawHandle;
        s.thread = unsafe { windows::Win32::System::Threading::GetThreadId(HANDLE(handle.as_raw_handle())) };
    } else {
        unsafe {
            let _ = PostThreadMessageW(s.thread, WM_REDRAW, WPARAM(0), LPARAM(0));
        }
    }
}

/// The strip's own state, on its thread.
struct Strip {
    hwnd: HWND,
    bar: HWND,
    rebar: HWND, // Windows 10's task buttons (null on Windows 11)
    width: i32,
    placed: (RECT, RECT, i32), // tray, bar, left: the last placement, to do nothing when nothing moved
    dpi: u32,
    light: bool,
}

fn run() {
    unsafe {
        let class = w!("RustKnobTaskbar");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: GetModuleHandleW(None).unwrap_or_default().into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        let mut strip: Option<Strip> = None;
        let mut msg = MSG::default();
        // the message queue exists from the first call: a redraw posted before it is not lost
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        sync(&mut strip);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match (msg.hwnd.is_invalid() || msg.hwnd.0.is_null(), msg.message) {
                (true, WM_REDRAW) => sync(&mut strip),
                (_, WM_TIMER) if msg.wParam.0 == TICK => sync(&mut strip),
                _ => {
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Creates, redraws, moves or removes the strip, as the items and the taskbar now are.
unsafe fn sync(strip: &mut Option<Strip>) {
    let items = SHARED.lock().map(|s| s.items.clone()).unwrap_or_default();
    // gone with Explorer (it restarted): made again on its new taskbar
    if strip.as_ref().is_some_and(|s| !IsWindow(s.hwnd).as_bool() || !IsWindow(s.bar).as_bool()) {
        *strip = None;
    }
    if items.is_empty() {
        if let Some(s) = strip.take() {
            s.close();
        }
        return;
    }
    if strip.is_none() {
        *strip = Strip::open();
        if let Some(s) = strip {
            SetTimer(s.hwnd, TICK, 1000, None);
        }
    }
    if let Some(s) = strip {
        s.paint(&items);
        s.place();
    }
}

impl Strip {
    unsafe fn open() -> Option<Strip> {
        let bar = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        FindWindowExW(bar, None, w!("TrayNotifyWnd"), PCWSTR::null()).ok()?;
        let win11 = FindWindowExW(bar, None, w!("Windows.UI.Composition.DesktopWindowContentBridge"), PCWSTR::null()).is_ok();
        let rebar = if win11 { HWND::default() } else { FindWindowExW(bar, None, w!("ReBarWindow32"), PCWSTR::null()).unwrap_or_default() };
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_NOACTIVATE, w!("RustKnobTaskbar"), PCWSTR::null(), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 1, 1, bar, None, GetModuleHandleW(None).unwrap_or_default(), None).ok()?;
        Some(Strip { hwnd, bar, rebar, width: 0, placed: (RECT::default(), RECT::default(), -1), dpi: 0, light: false })
    }

    unsafe fn close(self) {
        if !self.rebar.is_invalid() && !self.rebar.0.is_null() {
            // the task buttons take the whole width again
            let mut rc = RECT::default();
            let _ = GetClientRect(self.bar, &mut rc);
            let _ = PostMessageW(self.bar, WM_SIZE, WPARAM(SIZE_RESTORED as usize), LPARAM(((rc.bottom as isize) << 16) | rc.right as isize));
        }
        let _ = DestroyWindow(self.hwnd);
    }

    fn px(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    /// Draws the items into the layered window (its size: theirs, by the taskbar's height).
    unsafe fn paint(&mut self, items: &[Item]) {
        self.dpi = GetDpiForWindow(self.bar).max(96);
        self.light = light_taskbar();
        let mut rc = RECT::default();
        let _ = GetClientRect(self.bar, &mut rc);
        let vertical = rc.bottom - rc.top > rc.right - rc.left;
        let h_bar = rc.bottom - rc.top;
        let face = icon_face();
        let (icon_px, text_px, bolt_px) = (self.px(16), self.px(13), self.px(11));
        // every piece rasterized once: (bitmap, alpha 0..255, color)
        let fg: [u8; 3] = if self.light { [0, 0, 0] } else { [255, 255, 255] };
        let red: [u8; 3] = if self.light { [196, 30, 40] } else { [255, 90, 90] };
        let blue: [u8; 3] = if self.light { [0, 120, 212] } else { [96, 205, 255] };
        let green: [u8; 3] = if self.light { [16, 124, 16] } else { [108, 203, 95] };
        let mut groups: Vec<Vec<(TextBmp, u8, [u8; 3])>> = Vec::new();
        for it in items {
            let alpha = if it.dim { 120 } else { 255 };
            let low = it.percent.is_some_and(|p| p <= 15) && !it.charging;
            let mut g = Vec::new();
            if let Some(b) = raster_face(&glyph(it.kind, it.bluetooth).to_string(), -icon_px, 400, face) {
                g.push((b, alpha, fg));
            }
            let text = it.percent.map_or_else(|| "?".to_string(), |p| format!("{p}%"));
            if let Some(b) = raster_face(&text, -text_px, 400, "Segoe UI") {
                g.push((b, alpha, if low { red } else { fg }));
            }
            if it.charging || it.full {
                let (mark, color) = if it.full { ('\u{E73E}', green) } else { ('\u{E945}', blue) };
                if let Some(b) = raster_face(&mark.to_string(), -bolt_px, 400, face) {
                    g.push((b, alpha, color));
                }
            }
            groups.push(g);
        }
        // the bitmaps carry 4 px of padding each side: the gaps below are between their ink-free boxes
        let (pad, gap, between) = (self.px(8) - 4, self.px(5) - 8, self.px(14) - 8);
        let (w, h) = if vertical {
            let rows: i32 = groups.iter().map(|g| g.iter().map(|b| b.0.h as i32).max().unwrap_or(0) * 2 + self.px(2)).sum::<i32>();
            (rc.right - rc.left, rows + self.px(6) * 2)
        } else {
            let wide: i32 = groups.iter().map(|g| g.iter().map(|b| b.0.w as i32).sum::<i32>() + gap * (g.len() as i32 - 1).max(0)).sum::<i32>();
            (wide + between * (groups.len() as i32 - 1).max(0) + pad * 2, h_bar)
        };
        let (w, h) = (w.max(1), h.max(1));
        // premultiplied BGRA, alpha 1 everywhere: the whole strip takes the mouse yet looks clear
        let mut px = vec![0x0100_0000u32; (w * h) as usize];
        let mut blit = |b: &TextBmp, x0: i32, y0: i32, alpha: u8, c: [u8; 3]| {
            for y in 0..b.h as i32 {
                for x in 0..b.w as i32 {
                    let (tx, ty) = (x0 + x, y0 + y);
                    if tx < 0 || ty < 0 || tx >= w || ty >= h {
                        continue;
                    }
                    let a = (b.sample(x as f32 + 0.5, y as f32 + 0.5) * alpha as f32) as u32;
                    if a == 0 {
                        continue;
                    }
                    let d = &mut px[(ty * w + tx) as usize];
                    let inv = 255 - a;
                    let ch = |s: u8, sh: u32| (s as u32 * a / 255 + ((*d >> sh) & 0xff) * inv / 255) << sh;
                    let da = (a + ((*d >> 24) & 0xff) * inv / 255) << 24;
                    *d = da | ch(c[0], 16) | ch(c[1], 8) | ch(c[2], 0);
                }
            }
        };
        let mut spans = Vec::new();
        if vertical {
            let mut y = self.px(6);
            for (g, it) in groups.iter().zip(items) {
                let top = y;
                let row_h = g.iter().map(|b| b.0.h as i32).max().unwrap_or(0);
                if let Some((icon, a, c)) = g.first() {
                    blit(icon, (w - icon.w as i32) / 2, y, *a, *c);
                }
                let rest: i32 = g.iter().skip(1).map(|b| b.0.w as i32).sum();
                let mut x = (w - rest) / 2;
                for (b, a, c) in g.iter().skip(1) {
                    blit(b, x, y + row_h + self.px(2), *a, *c);
                    x += b.w as i32;
                }
                y += row_h * 2 + self.px(2);
                spans.push((top, y, it.id));
            }
        } else {
            let mut x = pad;
            for (g, it) in groups.iter().zip(items) {
                let from = x;
                for (b, a, c) in g {
                    blit(b, x, (h - b.h as i32) / 2, *a, *c);
                    x += b.w as i32 + gap;
                }
                x += between - gap;
                spans.push((from - between / 2, x - between / 2, it.id));
            }
        }
        if let Ok(mut s) = SPANS.lock() {
            *s = spans;
        }
        present(self.hwnd, &px, w, h);
        self.width = w;
        self.placed.2 = -1; // placed again: the width may have changed
    }

    /// Puts the strip left of the notification area (above it on a vertical taskbar), when anything moved.
    unsafe fn place(&mut self) {
        let Ok(tray) = FindWindowExW(self.bar, None, w!("TrayNotifyWnd"), PCWSTR::null()) else { return };
        let (mut tr, mut br, mut cr) = (RECT::default(), RECT::default(), RECT::default());
        let _ = GetWindowRect(tray, &mut tr);
        let _ = GetWindowRect(self.bar, &mut br);
        let _ = GetClientRect(self.bar, &mut cr);
        let covered = GetWindow(self.hwnd, GW_HWNDPREV).is_ok_and(|h| !h.0.is_null());
        let vertical = br.bottom - br.top > br.right - br.left;
        let mut size = RECT::default();
        let _ = GetWindowRect(self.hwnd, &mut size);
        let (w, h) = (self.width, if vertical { size.bottom - size.top } else { cr.bottom - cr.top });
        let (x, y);
        if vertical {
            let mut p = POINT { x: br.left, y: tr.top };
            let _ = ScreenToClient(self.bar, &mut p);
            y = (p.y - h - self.px(2)).clamp(0, (cr.bottom - h).max(0));
            x = ((cr.right - w) / 2).max(0);
        } else {
            y = 0;
            let rtl = GetWindowLongW(self.bar, GWL_EXSTYLE) as u32 & WS_EX_LAYOUTRTL.0 != 0;
            let mut edge = if rtl { tr.right } else { tr.left };
            // Windows 10 News and Interests sits between the task buttons and the tray
            if !rtl {
                for class in [w!("ShellFeedsTaskbarViewWindow"), w!("DynamicContent2")] {
                    if let Ok(feeds) = FindWindowExW(self.bar, None, class, PCWSTR::null()) {
                        let mut fr = RECT::default();
                        if IsWindowVisible(feeds).as_bool() && GetWindowRect(feeds, &mut fr).is_ok() && fr.left < edge && fr.right > fr.left {
                            edge = fr.left;
                        }
                    }
                }
            }
            let mut p = POINT { x: edge, y: br.top };
            let _ = ScreenToClient(self.bar, &mut p);
            x = p.x - w - self.px(2);
            // Windows 10: Explorer widens its task buttons on every layout pass; narrowed again each time
            if !self.rebar.0.is_null() {
                let mut rr = RECT::default();
                if GetWindowRect(self.rebar, &mut rr).is_ok() {
                    let mut o = POINT { x: if rtl { rr.right } else { rr.left }, y: rr.top };
                    let _ = ScreenToClient(self.bar, &mut o);
                    if o.x + (rr.right - rr.left) > x && x > o.x {
                        let _ = MoveWindow(self.rebar, o.x, o.y, x - o.x, rr.bottom - rr.top, true);
                    }
                }
            }
        }
        if !covered && self.placed == (tr, br, x * 10000 + y) {
            return;
        }
        self.placed = (tr, br, x * 10000 + y);
        let _ = SetWindowPos(self.hwnd, HWND_TOP, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

/// The taskbar's own theme (not the apps'): it can be dark while windows are light.
fn light_taskbar() -> bool {
    use windows::Win32::System::Registry::*;
    unsafe {
        let (mut data, mut size) = (0u32, 4u32);
        RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("SystemUsesLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as *mut _), Some(&mut size)).is_ok() && data != 0
    }
}

/// Premultiplied pixels into the layered window.
unsafe fn present(hwnd: HWND, px: &[u32], w: i32, h: i32) {
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(screen);
    let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
            let old = SelectObject(mem, bmp);
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8, ..Default::default() };
            let size = SIZE { cx: w, cy: h };
            let _ = UpdateLayeredWindow(hwnd, screen, None, Some(&size), mem, Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(mem, old);
        }
        let _ = DeleteObject(bmp);
    }
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // left: the card of the device under the pointer (as a click on its tray icon); right: the tray menu
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let owner = SHARED.lock().map(|s| s.owner).unwrap_or(0);
            let (x, y) = ((lp.0 & 0xffff) as i16 as i32, (lp.0 >> 16 & 0xffff) as i16 as i32);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let along = if rc.bottom > rc.right { y } else { x }; // a vertical strip lists them top to bottom
            let id = SPANS.lock().ok().and_then(|s| s.iter().find(|(a, b, _)| along >= *a && along < *b).map(|s| s.2));
            let wp = match (msg, id) {
                (WM_LBUTTONUP, Some(id)) => crate::tray::BATTERY_ID + id,
                (WM_LBUTTONUP, None) => return LRESULT(0), // between two devices: nothing
                _ => 0,
            };
            let _ = PostMessageW(HWND(owner as _), crate::tray::WM_TRAY, WPARAM(wp as usize), LPARAM(msg as isize));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
