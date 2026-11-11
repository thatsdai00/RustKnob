// Screenshot: Win+Shift+S freezes the screen, dims it and lets you drag a rectangle; letting go copies that
// part to the clipboard.
//
// Made to be cheap: one grab of the screen, and then only the rectangles that changed are
// redrawn (the dimmed rows computed from the grab, anti-aliased shapes and text on top) and blitted, a
// strip of rows at a time through one small buffer. The window
// fades in and out through the compositor, and everything is freed when it closes. The look follows the
// capsule: pure black pills, the same top margin, springy entrances.

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering::*};
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{cov, rrect, sstep};
use crate::hook::WM_APP_SNIP_DONE;
use crate::text::{raster, TextBmp, TEXT_R};

/// The overlay is up (the keyboard hook then turns Esc into a cancel).
pub static ACTIVE: AtomicBool = AtomicBool::new(false);

const CLASS: PCWSTR = w!("RustKnobSnip");
const T_FADE: usize = 1; // window opacity in / out
const T_ANIM: usize = 2; // pills, handles, the thumbnail's flight
const FADE_STEPS: f32 = 7.0; // frames of the fade in / out
const MIN_SIDE: i32 = 4; // a smaller drag counts as a mis-click
const STRIP_H: i32 = 128; // rows drawn at once
const WHITE: [f32; 3] = [255.0, 255.0, 255.0];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];

type Rect = (i32, i32, i32, i32); // x0, y0, x1, y1 (end exclusive)

/// A 32-bit top-down DIB with a memory DC: pixels can be read and written directly.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    px: *mut u32,
    len: usize,
}

impl Surface {
    unsafe fn new(w: i32, h: i32) -> Option<Surface> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut bits = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc); // a big virtual screen can run out of memory here
            return None;
        };
        if bits.is_null() {
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bmp);
        Some(Surface { dc, bmp, old, px: bits as *mut u32, len: (w * h) as usize })
    }

    /// The DIB's memory is owned by the OS bitmap, not by this struct, so a shared handle may write it.
    #[allow(clippy::mut_from_ref)]
    fn pixels(&self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.px, self.len) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The copied part on its way to the capsule.
struct Fly {
    from: Rect,
    t: f32,
    shown: Option<Rect>, // where it was drawn last
    told: bool,          // the capsule has been asked to show its message
}

struct Snip {
    hwnd: HWND,
    main: isize, // the capsule's window, told when a screenshot was taken
    w: i32,
    h: i32,
    orig: Surface, // the screen as it was
    back: Surface, // one strip of the window, drawn and blitted, then reused
    strip_y: i32,  // the screen row the strip starts at
    sel: Option<Rect>,
    anchor: (i32, i32),
    dragging: bool,
    drawn: Option<Rect>, // area covered by the last selection decoration
    fade: f32,           // 0..1 window opacity
    leaving: bool,       // fading out after a copy or a cancel
    scale: f32,          // device px per logical unit
    glyphs: Vec<TextBmp>, // "0123456789 ×" for the size label
    hint: Option<TextBmp>,
    taken: (i32, i32),
    // animation state
    hint_p: f32,  // hint pill: 0 = gone, 1 = in place
    label_p: f32, // size label and corner handles of the current selection
    fly: Option<Fly>, // the copied part shrinking toward the capsule
    target: (f32, f32), // where the capsule's icon sits (overlay coordinates)
    last: Instant,
    pending: Vec<Rect>, // what has to be redrawn
}

static STATE: AtomicPtr<Snip> = AtomicPtr::new(std::ptr::null_mut());

fn snip() -> Option<&'static mut Snip> {
    unsafe { STATE.load(SeqCst).as_mut() }
}

/// Overshoots a little before settling: the entrance spring.
fn ease_out_back(t: f32) -> f32 {
    let (c1, t) = (1.70158f32, t.clamp(0.0, 1.0) - 1.0);
    1.0 + (c1 + 1.0) * t.powi(3) + c1 * t * t
}

fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

// ---- start / finish --------------------------------------------------------

/// Grabs the screen and puts the selection overlay on top of it.
pub unsafe fn start(main: HWND, hint: &str) {
    if ACTIVE.swap(true, SeqCst) {
        return;
    }
    if !open(main.0 as isize, hint) {
        ACTIVE.store(false, SeqCst);
    }
}

unsafe fn open(main: isize, hint: &str) -> bool {
    let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
    let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
    if w <= 0 || h <= 0 {
        return false;
    }
    let (Some(orig), Some(back)) = (Surface::new(w, h), Surface::new(w, STRIP_H)) else { return false };

    let screen = GetDC(None);
    let grabbed = BitBlt(orig.dc, 0, 0, w, h, screen, vx, vy, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0));
    ReleaseDC(None, screen);
    if grabbed.is_err() {
        return false;
    }


    let hinst = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc),
        hInstance: hinst,
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc); // already registered the second time: fine
    // never takes the focus: the window you were in keeps it, and nothing jumps to the front afterwards
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        CLASS,
        w!("RustKnobSnip"),
        WS_POPUP,
        vx,
        vy,
        w,
        h,
        None,
        None,
        hinst,
        None,
    ) else {
        return false;
    };

    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    // the capsule opens at the top center of the monitor under the pointer, its icon a little left of center
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST), &mut mi);
    let scale_for_target = (GetDpiForSystem() as f32 / 96.0).max(1.0);
    let cx = ((mi.rcMonitor.left + mi.rcMonitor.right) / 2 - vx) as f32 - 144.0 * scale_for_target;
    let cy_target = (mi.rcMonitor.top - vy) as f32 + 48.0 * scale_for_target;
    let scale = (GetDpiForSystem() as f32 / 96.0).max(1.0);
    let glyphs = "0123456789 ×".chars().filter_map(|c| raster(&c.to_string(), -56, 700)).collect();
    let state = Box::new(Snip {
        hwnd,
        main,
        w,
        h,
        orig,
        back,
        strip_y: 0,
        sel: None,
        anchor: (0, 0),
        dragging: false,
        drawn: None,
        fade: 0.0,
        leaving: false,
        scale,
        glyphs,
        hint: raster(hint, -52, 700),
        taken: (0, 0),
        hint_p: 0.0,
        label_p: 0.0,
        fly: None,
        target: (cx, cy_target),
        last: Instant::now(),
        pending: Vec::new(),
    });
    STATE.store(Box::into_raw(state), SeqCst);
    let s = snip().unwrap();

    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, vx, vy, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
    s.draw((0, 0, w, h)); // fully transparent for now: the fade-in starts after this
    SetTimer(hwnd, T_FADE, 12, None);
    s.kick();
    true
}

/// Escape / right click.
pub fn cancel() {
    if let Some(s) = snip() {
        s.leave();
    }
}

unsafe fn close() {
    let p = STATE.swap(std::ptr::null_mut(), SeqCst);
    if p.is_null() {
        return;
    }
    let s = Box::from_raw(p);
    let (main, taken) = (s.main, s.taken);
    let _ = KillTimer(s.hwnd, T_FADE);
    let _ = KillTimer(s.hwnd, T_ANIM);
    let _ = DestroyWindow(s.hwnd);
    SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default()); // not the crosshair
    drop(s); // frees the screen grab
    ACTIVE.store(false, SeqCst);
    crate::island::trim_memory(); // and gives the pages back right away
    if taken.0 > 0 {
        let _ = PostMessageW(HWND(main as _), WM_APP_SNIP_DONE, WPARAM(taken.0 as usize), LPARAM(taken.1 as isize));
    }
}

// ---- window ----------------------------------------------------------------

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(s) = snip() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    let (x, y) = (lp.0 as i16 as i32, (lp.0 >> 16) as i16 as i32);
    match msg {
        WM_LBUTTONDOWN if !s.leaving && s.fly.is_none() => {
            SetCapture(hwnd);
            s.anchor = (x.clamp(0, s.w - 1), y.clamp(0, s.h - 1));
            s.dragging = true;
        }
        WM_MOUSEMOVE if s.dragging => s.drag_to(x, y),
        WM_LBUTTONUP if s.dragging => {
            let _ = ReleaseCapture();
            s.dragging = false;
            match s.sel.filter(|r| r.2 - r.0 >= MIN_SIDE && r.3 - r.1 >= MIN_SIDE) {
                Some(r) => s.copy(r),
                None => s.set_selection(None), // a click: start over
            }
        }
        WM_RBUTTONDOWN => s.leave(),
        WM_KEYDOWN if wp.0 == VK_ESC => s.leave(),
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_CROSS).unwrap_or_default());
            return LRESULT(1);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let r = ps.rcPaint;
            s.draw((r.left, r.top, r.right, r.bottom));
            let _ = EndPaint(hwnd, &ps);
        }
        WM_TIMER if wp.0 == T_FADE => {
            s.fade = (s.fade + if s.leaving { -1.0 } else { 1.0 } / FADE_STEPS).clamp(0.0, 1.0);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (ease_out(s.fade) * 255.0) as u8, LWA_ALPHA);
            if s.leaving && s.fade <= 0.0 {
                close();
            } else if !s.leaving && s.fade >= 1.0 {
                let _ = KillTimer(hwnd, T_FADE);
            }
        }
        WM_TIMER if wp.0 == T_ANIM => s.animate(),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

const VK_ESC: usize = 0x1B;

// ---- selection and animation -----------------------------------------------

impl Snip {
    fn kick(&self) {
        unsafe { SetTimer(self.hwnd, T_ANIM, 12, None) };
    }

    fn leave(&mut self) {
        if self.leaving {
            return;
        }
        self.land(); // a flight in progress ends here
        self.leaving = true;
        unsafe {
            let _ = ReleaseCapture();
            SetTimer(self.hwnd, T_FADE, 12, None);
        }
        self.kick();
    }

    fn drag_to(&mut self, x: i32, y: i32) {
        let (x, y) = (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        let sel = (self.anchor.0.min(x), self.anchor.1.min(y), self.anchor.0.max(x) + 1, self.anchor.1.max(y) + 1);
        if self.sel != Some(sel) {
            if self.sel.is_none() {
                self.label_p = 0.0; // a new selection: its label and handles pop in
                self.kick();
            }
            self.set_selection(Some(sel));
        }
    }

    /// Redraws just the part of the screen the change touches.
    fn set_selection(&mut self, sel: Option<Rect>) {
        let old = self.drawn;
        self.sel = sel;
        let now = self.deco_rect();
        let dirty = match old {
            Some(o) => (o.0.min(now.0), o.1.min(now.1), o.2.max(now.2), o.3.max(now.3)),
            None => now,
        };
        self.drawn = sel.map(|_| now);
        self.pending.push(dirty);
        if sel.is_none() {
            self.pending.push(self.hint_dirty());
            self.kick(); // the hint comes back
        }
        self.flush();
    }

    /// One animation frame: the hint pill, the label and handles, the flash.
    fn animate(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;
        let mut busy = false;

        let want = if self.sel.is_none() && !self.leaving && self.fly.is_none() { 1.0 } else { 0.0 };
        if self.hint_p != want {
            self.hint_p = if want > self.hint_p { (self.hint_p + dt / 0.40).min(1.0) } else { (self.hint_p - dt / 0.14).max(0.0) };
            self.pending.push(self.hint_dirty());
            busy = true;
        }
        if self.sel.is_some() && self.label_p < 1.0 {
            self.label_p = (self.label_p + dt / 0.24).min(1.0);
            busy = true;
            if let Some(d) = self.drawn {
                self.pending.push(d);
            }
        }
        if self.fly.is_some() {
            busy = true;
            let (old, t, told) = {
                let f = self.fly.as_mut().unwrap();
                f.t = (f.t + dt / 0.46).min(1.0);
                (f.shown.take(), f.t, f.told)
            };
            if let Some(old) = old {
                self.pending.push(old);
            }
            let (now, _) = self.fly_rect();
            self.pending.push(now);
            if let Some(f) = self.fly.as_mut() {
                f.shown = Some(now);
                f.told = told || t > 0.82;
            }
            if !told && t > 0.82 {
                self.announce(); // the capsule starts dropping as the thumbnail lands
            }
            if t >= 1.0 {
                self.land();
                self.leave();
            }
        }
        self.flush();
        if !busy {
            unsafe {
                let _ = KillTimer(self.hwnd, T_ANIM);
            }
        }
    }

    fn flush(&mut self) {
        for r in std::mem::take(&mut self.pending) {
            let r = (r.0.max(0), r.1.max(0), r.2.min(self.w), r.3.min(self.h));
            self.draw(r);
        }
    }

    /// The thumbnail's current rectangle (with its shadow reach) and the corner radius.
    fn fly_rect(&self) -> (Rect, f32) {
        let Some(f) = &self.fly else { return ((0, 0, 0, 0), 0.0) };
        let e = ease_in_out(f.t);
        let (fw, fh) = ((f.from.2 - f.from.0) as f32, (f.from.3 - f.from.1) as f32);
        let end = 46.0 * self.scale / fw.max(fh); // the longer side ends up this big
        let (tw, th) = (fw * end, fh * end);
        let (fcx, fcy) = ((f.from.0 + f.from.2) as f32 / 2.0, (f.from.1 + f.from.3) as f32 / 2.0);
        let (cx, cy) = (fcx + (self.target.0 - fcx) * e, fcy + (self.target.1 - fcy) * e);
        let (w, h) = (fw + (tw - fw) * e, fh + (th - fh) * e);
        let m = (14.0 * self.scale) as i32;
        let r = ((cx - w / 2.0) as i32 - m, (cy - h / 2.0) as i32 - m, (cx + w / 2.0).ceil() as i32 + m, (cy + h / 2.0).ceil() as i32 + m);
        (r, (e * 10.0 * self.scale).min(w.min(h) / 2.0))
    }

    /// Tells the capsule a screenshot is on the clipboard.
    fn announce(&mut self) {
        let (main, taken) = (self.main, self.taken);
        unsafe {
            let _ = PostMessageW(HWND(main as _), WM_APP_SNIP_DONE, WPARAM(taken.0 as usize), LPARAM(taken.1 as isize));
        }
        self.taken = (0, 0); // once
    }

    /// The flight is over (or cut short): make sure the capsule was told, redraw without the thumbnail.
    fn land(&mut self) {
        if let Some(mut f) = self.fly.take() {
            if !f.told {
                f.told = true;
                self.announce();
            }
            if let Some(old) = f.shown {
                self.pending.push(old);
            }
            self.flush();
        }
    }

    /// Everything that is drawn for the current selection: border, handles, size label, shadows.
    fn deco_rect(&self) -> Rect {
        match self.sel {
            Some(s) => {
                let l = self.label_rect(s);
                let m = (14.0 * self.scale) as i32;
                (s.0.min(l.0) - m, s.1.min(l.1) - m, s.2.max(l.2) + m, s.3.max(l.3) + m)
            }
            None => self.hint_dirty(),
        }
    }

    /// Draws `r` (clipped to the screen) strip by strip and puts each strip on the window.
    fn draw(&mut self, r: Rect) {
        let r = (r.0.max(0), r.1.max(0), r.2.min(self.w), r.3.min(self.h));
        if r.0 >= r.2 || r.1 >= r.3 {
            return;
        }
        unsafe {
            let dc = GetDC(self.hwnd);
            let mut y = r.1;
            while y < r.3 {
                let strip = (r.0, y, r.2, (y + STRIP_H).min(r.3));
                self.strip_y = y;
                self.render(strip);
                let _ = BitBlt(dc, strip.0, strip.1, strip.2 - strip.0, strip.3 - strip.1, self.back.dc, strip.0, 0, SRCCOPY);
                y = strip.3;
            }
            ReleaseDC(self.hwnd, dc);
        }
    }

    // ---- drawing ------------------------------------------------------------

    fn render(&mut self, d: Rect) {
        let (w, back, orig, sy) = (self.w as usize, self.back.pixels(), self.orig.pixels(), self.strip_y);
        for y in d.1.max(0)..d.3.min(self.h) {
            let row = y as usize * w; // in the screen grab
            let out = (y - sy) as usize * w; // in the strip
            for (o, &p) in back[out + d.0 as usize..out + d.2 as usize].iter_mut().zip(&orig[row + d.0 as usize..row + d.2 as usize]) {
                *o = dimmed(p);
            }
            if let Some(s) = self.sel.filter(|s| y >= s.1 && y < s.3) {
                let (x0, x1) = (s.0.max(d.0), s.2.min(d.2));
                if x0 < x1 {
                    back[out + x0 as usize..out + x1 as usize].copy_from_slice(&orig[row + x0 as usize..row + x1 as usize]);
                }
            }
        }
        match self.sel {
            Some(s) => {
                self.border(s, d);
                self.handles(s, d);
                let e = ease_out_back(self.label_p);
                let text = format!("{} × {}", s.2 - s.0, s.3 - s.1);
                let r = self.label_rect(s);
                let lift = ((1.0 - e) * 8.0 * self.scale) as i32;
                let r = (r.0, r.1 + lift, r.2, r.3 + lift);
                let alpha = ease_out(self.label_p);
                self.pill(r, d, alpha);
                let (x, cy) = (r.0 as f32 + 14.0 * self.scale, (r.1 + r.3) as f32 / 2.0);
                self.glyph_text(&text, x, cy, d, alpha);
            }
            None if self.fly.is_some() => self.thumbnail(d),
            None => {
                if self.hint_p > 0.0 && self.hint.is_some() {
                    let e = ease_out_back(self.hint_p);
                    let r = self.hint_rect();
                    let slide = ((1.0 - e) * -34.0 * self.scale) as i32;
                    let r = (r.0, r.1 + slide, r.2, r.3 + slide);
                    let alpha = ease_out(self.hint_p);
                    self.pill(r, d, alpha);
                    let cy = (r.1 + r.3) as f32 / 2.0;
                    if let Some(h) = self.hint.as_ref().map(|h| h as *const TextBmp) {
                        self.blit_text(unsafe { &*h }, r.0 as f32 + 22.0 * self.scale, cy, d, alpha);
                    }
                }
            }
        }
    }

    /// The copied part, scaled down (box filtered from the original), rounded, with a hairline and a shadow.
    fn thumbnail(&mut self, d: Rect) {
        let Some(f) = &self.fly else { return };
        let (from, e) = (f.from, ease_in_out(f.t));
        let (fw, fh) = ((from.2 - from.0) as f32, (from.3 - from.1) as f32);
        let end = 46.0 * self.scale / fw.max(fh);
        let (fcx, fcy) = ((from.0 + from.2) as f32 / 2.0, (from.1 + from.3) as f32 / 2.0);
        let (cx, cy) = (fcx + (self.target.0 - fcx) * e, fcy + (self.target.1 - fcy) * e);
        let (w, h) = (fw + (fw * end - fw) * e, fh + (fh * end - fh) * e);
        let (rx, ry) = (cx - w / 2.0, cy - h / 2.0);
        let radius = (e * 10.0 * self.scale).min(w.min(h) / 2.0);
        let (hw, hh) = (w / 2.0, h / 2.0);
        let reach = (14.0 * self.scale) as i32;
        let (sx, sy) = (fw / w, fh / h); // source pixels per thumbnail pixel
        let (ww, back, orig, strip_y) = (self.w as usize, self.back.pixels(), self.orig.pixels(), self.strip_y);
        for y in ((ry as i32) - reach).max(d.1)..((ry + h).ceil() as i32 + reach).min(d.3) {
            for x in ((rx as i32) - reach).max(d.0)..((rx + w).ceil() as i32 + reach).min(d.2) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let sdf = rrect(px - cx, py - cy, hw, hh, radius);
                let target = &mut back[(y - strip_y) as usize * ww + x as usize];
                let shadow = (1.0 - sstep(-2.0, reach as f32, rrect(px - cx, py - cy - 3.0 * self.scale, hw, hh, radius))) * 0.34;
                if shadow > 0.0 {
                    mix(target, BLACK, shadow);
                }
                let a = cov(sdf, 1.0);
                if a > 0.0 {
                    // average a 2 x 2 patch of the source under this pixel
                    let (u, v) = ((px - rx) * sx + from.0 as f32, (py - ry) * sy + from.1 as f32);
                    let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
                    for (ox, oy) in [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)] {
                        let sxp = ((u + ox * sx) as i32).clamp(0, self.w - 1) as usize;
                        let syp = ((v + oy * sy) as i32).clamp(0, self.h - 1) as usize;
                        let p = orig[syp * ww + sxp];
                        r += ((p >> 16) & 0xFF) as f32;
                        g += ((p >> 8) & 0xFF) as f32;
                        b += (p & 0xFF) as f32;
                    }
                    mix(target, [r / 4.0, g / 4.0, b / 4.0], a);
                }
                let hair = cov(sdf.abs() - 0.75, 1.0) * 0.55;
                if hair > 0.0 {
                    mix(target, WHITE, hair);
                }
            }
        }
    }

    /// White 2 px line just outside the selection.
    fn border(&mut self, s: Rect, d: Rect) {
        let (cx, cy) = ((s.0 + s.2) as f32 / 2.0, (s.1 + s.3) as f32 / 2.0);
        let (hw, hh) = ((s.2 - s.0) as f32 / 2.0, (s.3 - s.1) as f32 / 2.0);
        let (w, back, sy) = (self.w as usize, self.back.pixels(), self.strip_y);
        for y in (s.1 - 3).max(d.1)..(s.3 + 3).min(d.3) {
            let inner_row = y >= s.1 + 2 && y < s.3 - 2;
            for x in (s.0 - 3).max(d.0)..(s.2 + 3).min(d.2) {
                if inner_row && x >= s.0 + 2 && x < s.2 - 2 {
                    continue; // the inside of the rectangle is left alone
                }
                let dist = rrect(x as f32 + 0.5 - cx, y as f32 + 0.5 - cy, hw, hh, 0.0);
                let a = cov((dist - 1.0).abs() - 1.0, 1.0) * 0.95;
                if a > 0.0 {
                    mix(&mut back[(y - sy) as usize * w + x as usize], WHITE, a);
                }
            }
        }
    }

    /// Round handles on the four corners; they pop in with the label.
    fn handles(&mut self, s: Rect, d: Rect) {
        let r = 5.0 * self.scale * ease_out_back(self.label_p).max(0.0);
        if r < 0.5 {
            return;
        }
        let (w, sy) = (self.w as usize, self.strip_y);
        let back = self.back.pixels();
        for (cx, cy) in [(s.0 as f32, s.1 as f32), (s.2 as f32, s.1 as f32), (s.0 as f32, s.3 as f32), (s.2 as f32, s.3 as f32)] {
            let m = r as i32 + 3;
            for y in ((cy as i32 - m).max(d.1))..((cy as i32 + m).min(d.3)) {
                for x in ((cx as i32 - m).max(d.0))..((cx as i32 + m).min(d.2)) {
                    let dist = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                    let ring = cov(dist - (r + 1.2), 1.0) * 0.38;
                    if ring > 0.0 {
                        mix(&mut back[(y - sy) as usize * w + x as usize], BLACK, ring);
                    }
                    let a = cov(dist - r, 1.0);
                    if a > 0.0 {
                        mix(&mut back[(y - sy) as usize * w + x as usize], WHITE, a);
                    }
                }
            }
        }
    }

    fn glyph(&self, c: char) -> Option<&TextBmp> {
        let i = "0123456789 ×".chars().position(|g| g == c)?;
        self.glyphs.get(i)
    }

    fn glyph_width(&self, text: &str) -> f32 {
        text.chars().filter_map(|c| self.glyph(c)).map(|g| g.size().0).sum::<f32>() * self.scale
    }

    /// The size label: above the selection's top left corner, or inside it when there is no room.
    fn label_rect(&self, s: Rect) -> Rect {
        let text = format!("{} × {}", s.2 - s.0, s.3 - s.1);
        let (pw, ph) = ((self.glyph_width(&text) + 28.0 * self.scale) as i32, (34.0 * self.scale) as i32);
        let gap = (12.0 * self.scale) as i32;
        let x = s.0.min(self.w - pw).max(0);
        let y = if s.1 - gap - ph >= 0 { s.1 - gap - ph } else { (s.1 + gap).min(self.h - ph) };
        (x, y, x + pw, y + ph)
    }

    /// The hint pill at rest: top center, where the capsule drops from.
    fn hint_rect(&self) -> Rect {
        let tw = self.hint.as_ref().map_or(0.0, |t| t.size().0) * self.scale;
        let (pw, ph) = ((tw + 44.0 * self.scale) as i32, (44.0 * self.scale) as i32);
        let x = (self.w - pw) / 2;
        let y = (12.0 * self.scale) as i32;
        (x, y, x + pw, y + ph)
    }

    /// Where the hint pill may be while it moves, shadow included.
    fn hint_dirty(&self) -> Rect {
        let r = self.hint_rect();
        let m = (16.0 * self.scale) as i32;
        (r.0 - m, 0, r.2 + m, r.3 + (m * 2))
    }

    /// A black capsule like the island's, with a faint shadow and hairline.
    fn pill(&mut self, r: Rect, d: Rect, alpha: f32) {
        let (cx, cy) = ((r.0 + r.2) as f32 / 2.0, (r.1 + r.3) as f32 / 2.0);
        let (hw, hh) = ((r.2 - r.0) as f32 / 2.0, (r.3 - r.1) as f32 / 2.0);
        let reach = (16.0 * self.scale) as i32;
        let drop = 3.0 * self.scale;
        let (w, back, sy) = (self.w as usize, self.back.pixels(), self.strip_y);
        for y in (r.1 - reach).max(d.1)..(r.3 + reach).min(d.3) {
            for x in (r.0 - reach).max(d.0)..(r.2 + reach).min(d.2) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let outside = rrect(px - cx, py - cy - drop, hw, hh, hh);
                let shadow = (1.0 - sstep(-2.0, reach as f32, outside)) * 0.30 * alpha;
                let target = &mut back[(y - sy) as usize * w + x as usize];
                if shadow > 0.0 {
                    mix(target, BLACK, shadow);
                }
                let sdf = rrect(px - cx, py - cy, hw, hh, hh);
                let body = cov(sdf, 1.0) * 0.94 * alpha;
                if body > 0.0 {
                    mix(target, BLACK, body);
                }
                let hair = cov(sdf.abs() - 0.5, 1.0) * 0.10 * alpha;
                if hair > 0.0 {
                    mix(target, WHITE, hair);
                }
            }
        }
    }

    fn glyph_text(&mut self, text: &str, mut x: f32, cy: f32, d: Rect, alpha: f32) {
        for c in text.chars() {
            let Some(g) = self.glyph(c).map(|g| g as *const TextBmp) else { continue };
            let g = unsafe { &*g };
            self.blit_text(g, x, cy, d, alpha);
            x += g.size().0 * self.scale;
        }
    }

    /// White text from an alpha bitmap, vertically centered on `cy`.
    fn blit_text(&mut self, t: &TextBmp, x: f32, cy: f32, d: Rect, alpha: f32) {
        let (lw, lh) = t.size();
        let (dw, dh) = (lw * self.scale, lh * self.scale);
        let (top, k) = (cy - dh / 2.0, TEXT_R / self.scale);
        let (w, back, sy) = (self.w as usize, self.back.pixels(), self.strip_y);
        for y in (top.floor() as i32).max(d.1)..((top + dh).ceil() as i32).min(d.3) {
            for px in (x.floor() as i32).max(d.0)..((x + dw).ceil() as i32).min(d.2) {
                let a = t.sample((px as f32 + 0.5 - x) * k, (y as f32 + 0.5 - top) * k).powf(0.85) * alpha;
                if a > 0.0 {
                    mix(&mut back[(y - sy) as usize * w + px as usize], WHITE, a);
                }
            }
        }
    }

    // ---- result ---------------------------------------------------------------

    /// Puts the selected part on the clipboard (as a bottom-up 24-bit bitmap) and fades out.
    fn copy(&mut self, r: Rect) {
        let (sw, sh) = ((r.2 - r.0) as usize, (r.3 - r.1) as usize);
        let stride = (sw * 3 + 3) & !3;
        let header = std::mem::size_of::<BITMAPINFOHEADER>();
        unsafe {
            if OpenClipboard(self.hwnd).is_ok() {
                let _ = EmptyClipboard();
                if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, header + stride * sh) {
                    let ptr = GlobalLock(mem) as *mut u8;
                    let mut ok = false;
                    if !ptr.is_null() {
                        let info = BITMAPINFOHEADER {
                            biSize: header as u32,
                            biWidth: sw as i32,
                            biHeight: sh as i32,
                            biPlanes: 1,
                            biBitCount: 24,
                            biCompression: BI_RGB.0,
                            biSizeImage: (stride * sh) as u32,
                            ..Default::default()
                        };
                        std::ptr::copy_nonoverlapping(&info as *const _ as *const u8, ptr, header);
                        let (src, out) = (self.orig.pixels(), std::slice::from_raw_parts_mut(ptr.add(header), stride * sh));
                        for row in 0..sh {
                            let line = &src[(r.1 as usize + row) * self.w as usize + r.0 as usize..][..sw];
                            let dst = &mut out[(sh - 1 - row) * stride..][..sw * 3];
                            for (px, d3) in line.iter().zip(dst.chunks_exact_mut(3)) {
                                d3.copy_from_slice(&px.to_le_bytes()[..3]); // B, G, R
                            }
                        }
                        let _ = GlobalUnlock(mem);
                        ok = SetClipboardData(CF_DIB.0 as u32, HANDLE(mem.0)).is_ok(); // the clipboard owns it now
                    }
                    if !ok {
                        let _ = GlobalFree(mem);
                    }
                }
                let _ = CloseClipboard();
            }
        }
        self.taken = (sw as i32, sh as i32);
        crate::hook::SNIP_SEQ.store(unsafe { GetClipboardSequenceNumber() }, SeqCst);
        // the selection lets go of the screen and flies to the capsule
        self.pending.push(self.drawn.unwrap_or(r));
        self.drawn = None;
        self.sel = None;
        self.fly = Some(Fly { from: r, t: 0.0, shown: None, told: false });
        self.last = Instant::now();
        self.kick();
    }
}

/// 5/8 of the brightness: x/2 + x/8 on all three channels at once.
#[inline(always)]
fn dimmed(p: u32) -> u32 {
    ((p >> 1) & 0x007F_7F7F) + ((p >> 3) & 0x001F_1F1F)
}

/// `px` moved toward `color` by `a`.
#[inline]
fn mix(px: &mut u32, color: [f32; 3], a: f32) {
    let c = |shift: u32, to: f32| {
        let from = ((*px >> shift) & 0xFF) as f32;
        (from + (to - from) * a) as u32
    };
    *px = (c(16, color[0]) << 16) | (c(8, color[1]) << 8) | c(0, color[2]);
}
