// Notification-area icon: left click opens the settings capsule, right click shows a small menu.

use windows::core::HSTRING;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;

use crate::i18n::Strings;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 10;
pub const CMD_SETTINGS: usize = 1;
pub const CMD_QUIT: usize = 2;
pub const CMD_UNINSTALL: usize = 3;
pub const CMD_HISTORY: usize = 4;
const ICON_ID: u32 = 1;

/// 32x32 white ring with an indicator tick, drawn straight into an icon.
unsafe fn make_icon() -> Option<HICON> {
    const N: i32 = 32;
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: N,
            biHeight: -N,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let color = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (N * N) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
            let d = (dx * dx + dy * dy).sqrt();
            let ring = (1.6 - (d - 12.5).abs()).clamp(0.0, 1.0);
            let tick = if dx.abs() < 1.4 && dy < -3.0 && dy > -12.0 { 1.0 } else { 0.0 };
            let a = (ring.max(tick) * 255.0) as u32;
            px[(y * N + x) as usize] = a << 24 | a << 16 | a << 8 | a; // premultiplied white
        }
    }
    let mask = CreateBitmap(N, N, 1, 1, None);
    let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&info).ok();
    let _ = DeleteObject(color);
    let _ = DeleteObject(mask);
    icon
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
}

/// Explorer broadcasts this when its taskbar (re)starts: the icon has to be added again then.
pub fn taskbar_created() -> u32 {
    static MSG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) })
}

/// False when the taskbar is not there yet (RustKnob starts before it at login).
pub unsafe fn add(hwnd: HWND) -> bool {
    let mut nid = data(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = make_icon().unwrap_or_default();
    for (i, c) in "RustKnob".encode_utf16().enumerate() {
        nid.szTip[i] = c;
    }
    let added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
    if !nid.hIcon.is_invalid() {
        let _ = DestroyIcon(nid.hIcon); // the shell keeps its own copy (and this runs again on every retry)
    }
    added
}

pub unsafe fn remove(hwnd: HWND) {
    let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
    batteries(hwnd, &[], "");
}

// ---- a battery icon per device -------------------------------------------------

/// One device's icon: its model's number (the icon's id is BATTERY_ID + it), percent and charger.
#[derive(Clone, Copy, PartialEq)]
pub struct Battery {
    pub model: u32,
    pub percent: Option<u8>,
    pub charging: bool,
    pub name: &'static str,
}

pub const BATTERY_ID: u32 = 100;

/// The icons on show.
static SHOWN: std::sync::Mutex<Vec<Battery>> = std::sync::Mutex::new(Vec::new());

/// Shows one icon per device in `now` (the percent in figures: green while charging, white, yellow when low,
/// red when critical), changes the ones that differ, takes away the ones that are gone. `charging_word` goes
/// in the tooltip.
pub unsafe fn batteries(hwnd: HWND, now: &[Battery], charging_word: &str) {
    let Ok(mut shown) = SHOWN.lock() else { return };
    for old in shown.iter().filter(|o| !now.iter().any(|n| n.model == o.model)) {
        let mut nid = data(hwnd);
        nid.uID = BATTERY_ID + old.model;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
    for b in now {
        let before = shown.iter().find(|o| o.model == b.model);
        if before == Some(b) {
            continue;
        }
        let mut nid = data(hwnd);
        nid.uID = BATTERY_ID + b.model;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = battery_icon(b).unwrap_or_default();
        let tip = match (b.percent, b.charging) {
            (Some(p), true) => format!("{}: {p} % ({charging_word})", b.name),
            (Some(p), false) => format!("{}: {p} %", b.name),
            (None, _) => b.name.to_string(),
        };
        for (i, c) in tip.encode_utf16().take(nid.szTip.len() - 1).enumerate() {
            nid.szTip[i] = c;
        }
        // a new one (or the taskbar came back without it): added; one on show: changed
        if before.is_none() || !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
        if !nid.hIcon.is_invalid() {
            let _ = DestroyIcon(nid.hIcon);
        }
    }
    *shown = now.to_vec();
}

/// After the taskbar (re)starts: every battery icon is added again at the next look.
pub fn batteries_lost() {
    if let Ok(mut shown) = SHOWN.lock() {
        shown.clear();
    }
}

/// The percent in 7-segment figures (the volume capsule's), sized for the notification area.
unsafe fn battery_icon(b: &Battery) -> Option<HICON> {
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    let n = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16);
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: n, biHeight: -n, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let color = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (n * n) as usize);
    battery_pixels(px, n as usize, b);
    let mask = CreateBitmap(n, n, 1, 1, None);
    let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&info).ok();
    let _ = DeleteObject(color);
    let _ = DeleteObject(mask);
    icon
}

/// The icon's pixels, `n` x `n`, premultiplied 0xAARRGGBB.
pub(crate) fn battery_pixels(px: &mut [u32], n: usize, b: &Battery) {
    use crate::gfx::{cov, rrect, Digits};
    let pct = b.percent.unwrap_or(0);
    let rgb: [f32; 3] = match (b.charging, pct) {
        (true, _) => [0.32, 0.86, 0.47], // charging: green
        (_, 0..=5) => [1.0, 0.31, 0.27], // critical: red
        (_, 6..=24) => [1.0, 0.79, 0.20], // low: yellow
        _ => [1.0, 1.0, 1.0],
    };
    let digits = Digits::number(pct as u32);
    // the figures fill the width (two: 41 units wide, three: 66), never taller than the icon
    let wide = if pct >= 100 { 66.0 } else if pct >= 10 { 41.0 } else { 16.0 };
    let unit = (n as f32 * 0.98 / (wide + 6.0)).min(n as f32 * 0.9 / 36.0); // px per unit
    let (aa, stroke) = (1.0 / unit, 3.0);
    for y in 0..n {
        for x in 0..n {
            let (ux, uy) = ((x as f32 + 0.5 - n as f32 / 2.0) / unit, (y as f32 + 0.5 - n as f32 / 2.0) / unit);
            let a = match b.percent {
                // full: three figures do not read at 16 px, a full battery does (the tooltip says 100 %)
                Some(100..) => {
                    let (bx, by, k) = (x as f32 + 0.5 - n as f32 / 2.0 + n as f32 * 0.04, y as f32 + 0.5 - n as f32 / 2.0, n as f32 / 32.0);
                    let outline = cov(rrect(bx, by, 13.0 * k, 8.5 * k, 2.5 * k).abs() - 1.1 * k.max(0.6), 1.0);
                    let nub = cov(rrect(bx - 14.8 * k, by, 1.6 * k, 3.5 * k, 1.0 * k), 1.0);
                    let body = cov(rrect(bx, by, 10.0 * k, 5.5 * k, 1.2 * k), 1.0);
                    outline.max(nub).max(body)
                }
                Some(_) => cov(digits.dist(ux, uy) - stroke, aa),
                None => 0.0,
            };
            let ch = |c: f32| ((c * a * 255.0) as u32).min(255);
            px[y * n + x] = ((a * 255.0) as u32) << 24 | ch(rgb[0]) << 16 | ch(rgb[1]) << 8 | ch(rgb[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    /// Writes the icons as raw RGBA to %TEMP%\rustknob-tray-*.raw. `cargo test tray -- --ignored`
    #[test]
    #[ignore]
    fn battery_pixels() {
        for n in [16usize, 20, 24, 32] {
            for (pct, charging) in [(7u8, false), (46, false), (100, true), (18, false), (3, false)] {
                let b = super::Battery { model: 0, percent: Some(pct), charging, name: "" };
                let mut px = vec![0u32; n * n];
                super::battery_pixels(&mut px, n, &b);
                let raw: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]).collect();
                std::fs::write(std::env::temp_dir().join(format!("rustknob-tray-{n}-{pct}.raw")), raw).unwrap();
            }
        }
    }
}

/// Right-click menu at the cursor; returns CMD_SETTINGS, CMD_QUIT or 0.
pub unsafe fn menu(hwnd: HWND, strings: &Strings, installed: bool) -> usize {
    let Ok(menu) = CreatePopupMenu() else { return 0 };
    let item = |text: &str| HSTRING::from(text);
    let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, &item(strings.tray_settings));
    let _ = AppendMenuW(menu, MF_STRING, CMD_HISTORY, &item(strings.tray_history));
    if installed {
        let _ = AppendMenuW(menu, MF_STRING, CMD_UNINSTALL, &item(strings.tray_uninstall));
    }
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, &item(strings.tray_quit));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd); // required so the menu closes when clicking elsewhere
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    cmd.0 as usize
}
