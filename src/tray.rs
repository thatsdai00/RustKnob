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
