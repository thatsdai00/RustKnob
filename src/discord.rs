// Discord's own mute and deafen, as its window tells them: no bot, no hook, nothing installed in Discord.
//
// Discord (Chromium) describes its interface to Windows' UI Automation, the interface screen readers read. The
// two switches in its user panel (bottom left: microphone, headphones) are buttons whose class names carry
// "audioButton" and that have an on / off state. They are looked for once (the whole tree is walked only then),
// kept, and only their state is asked for afterwards, every few hundred milliseconds. When the window goes,
// is hidden (in the tray) or they cannot be found, they are looked for again now and then.
//
// Chromium only builds that description once something asks for it: the first look after Discord starts may
// find nothing, the next ones do.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::VARIANT;
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowThreadProcessId, PostMessageW};

use crate::media::{Art, ART_PX};

const ASK_EVERY: Duration = Duration::from_millis(300);
const LOOK_EVERY: Duration = Duration::from_secs(5); // no switches found: look again this often

static ON: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static MSG: AtomicIsize = AtomicIsize::new(0);

/// Turns the watching on or off. While on, `msg` is posted to `hwnd` when the state changes: wparam bit 0
/// muted, bit 1 deafened, bit 2 set when it was the deafen switch that changed.
pub fn set_enabled(on: bool, hwnd: isize, msg: u32) {
    TARGET.store(hwnd, Ordering::Relaxed);
    MSG.store(msg as isize, Ordering::Relaxed);
    ON.store(on, Ordering::Relaxed);
    if on && !STARTED.swap(true, Ordering::Relaxed) {
        let _ = std::thread::Builder::new().name("discord".into()).spawn(run);
    }
}

fn run() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return };
        let mut switches: Option<[IUIAutomationElement; 2]> = None;
        let mut last: Option<(bool, bool)> = None;
        let mut looked = Instant::now() - LOOK_EVERY;
        loop {
            if !ON.load(Ordering::Relaxed) {
                (switches, last) = (None, None); // the elements keep Discord's description alive: let go
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            if switches.is_none() && looked.elapsed() >= LOOK_EVERY {
                looked = Instant::now();
                switches = find(&uia);
            }
            let now = switches.as_ref().and_then(|[mic, head]| Some((is_on(mic)?, is_on(head)?)));
            match now {
                Some(state) => {
                    if let Some(l) = last.filter(|l| *l != state) {
                        let w = usize::from(state.0) | usize::from(state.1) << 1 | usize::from(l.1 != state.1) << 2;
                        let _ = PostMessageW(HWND(TARGET.load(Ordering::Relaxed) as _), MSG.load(Ordering::Relaxed) as u32, WPARAM(w), LPARAM(0));
                    }
                    last = Some(state); // the first reading only sets where it starts: no card for it
                }
                None => (switches, last) = (None, None), // gone (closed, hidden, redrawn): look again
            }
            std::thread::sleep(if switches.is_some() { ASK_EVERY } else { Duration::from_millis(1000) });
        }
    }
}

/// Is the switch on (muted / deafened)? None: it is gone.
unsafe fn is_on(e: &IUIAutomationElement) -> Option<bool> {
    let v = e.GetCurrentPropertyValue(UIA_ToggleToggleStatePropertyId).ok()?;
    i32::try_from(&v).ok().map(|s| s == ToggleState_On.0)
}

/// The two switches of Discord's user panel, microphone first.
unsafe fn find(uia: &IUIAutomation) -> Option<[IUIAutomationElement; 2]> {
    let hwnd = discord_window()?;
    if ICON.get().is_none() {
        if let Some(art) = app_icon(hwnd) {
            let _ = ICON.set(art);
        }
    }
    let root = uia.ElementFromHandle(hwnd).ok()?;
    let buttons = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_ButtonControlTypeId.0)).ok()?;
    let all = root.FindAll(TreeScope_Descendants, &buttons).ok()?;
    let mut found = Vec::new();
    for i in 0..all.Length().unwrap_or(0) {
        let Ok(e) = all.GetElement(i) else { continue };
        let class = e.CurrentClassName().map(|b| b.to_string()).unwrap_or_default();
        if !class.contains("audioButton") || e.CurrentIsOffscreen().is_ok_and(|o| o.as_bool()) {
            continue;
        }
        let toggles = e.GetCurrentPropertyValue(UIA_IsTogglePatternAvailablePropertyId).ok().and_then(|v| bool::try_from(&v).ok()) == Some(true);
        if toggles {
            let left = e.CurrentBoundingRectangle().map(|r| r.left).unwrap_or(0);
            found.push((left, e));
        }
    }
    if found.len() != 2 {
        return None;
    }
    found.sort_by_key(|(left, _)| *left);
    let mut it = found.into_iter().map(|(_, e)| e);
    Some([it.next()?, it.next()?])
}

static ICON: std::sync::OnceLock<Art> = std::sync::OnceLock::new();

/// Discord's own icon (taken from its program file when its window is first found), for the card.
pub fn icon() -> Option<&'static Art> {
    ICON.get()
}

/// The icon of the program behind `hwnd`, ART_PX square, premultiplied.
unsafe fn app_icon(hwnd: HWND) -> Option<Art> {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
    let path = exe_path(hwnd)?;
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut icon = HICON::default();
    if ExtractIconExW(windows::core::PCWSTR(wide.as_ptr()), 0, Some(&mut icon), None, 1) == 0 || icon.is_invalid() {
        return None;
    }
    let mut info = ICONINFO::default();
    let got = GetIconInfo(icon, &mut info).is_ok();
    let _ = DestroyIcon(icon);
    if !got {
        return None;
    }
    let mut bm = BITMAP::default();
    GetObjectW(info.hbmColor, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
    let (w, h) = (bm.bmWidth.max(1) as usize, bm.bmHeight.max(1) as usize);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut src = vec![0u32; w * h];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, info.hbmColor, 0, h as u32, Some(src.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    let _ = DeleteObject(info.hbmColor);
    let _ = DeleteObject(info.hbmMask);
    if lines == 0 || src.iter().all(|p| p >> 24 == 0) {
        return None; // no alpha: not drawn rather than drawn as a black square
    }
    // nearest sample to ART_PX square, premultiplied
    let n = ART_PX;
    let px = (0..n * n)
        .map(|i| {
            let p = src[(i / n * h / n) * w + (i % n * w / n)];
            let a = p >> 24;
            let m = |c: u32| (c & 0xff) * a / 255;
            a << 24 | m(p >> 16) << 16 | m(p >> 8) << 8 | m(p)
        })
        .collect();
    Some(Art { px, accent: [88.0, 101.0, 242.0] })
}

unsafe fn exe_path(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(process);
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// Discord's main window (the stable, PTB or Canary build).
unsafe fn discord_window() -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        let mut class = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        if class[..n].iter().copied().eq("Chrome_WidgetWin_1".encode_utf16()) &&GetWindowTextLengthW(hwnd) > 0 && is_discord(hwnd) {
            *(found.0 as *mut Option<HWND>) = Some(hwnd);
            return false.into();
        }
        true.into()
    }
    let mut found: Option<HWND> = None;
    let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    found
}

unsafe fn is_discord(hwnd: HWND) -> bool {
    let name = exe_path(hwnd).unwrap_or_default().to_ascii_lowercase();
    ["\\discord.exe", "\\discordptb.exe", "\\discordcanary.exe"].iter().any(|n| name.ends_with(n))
}
