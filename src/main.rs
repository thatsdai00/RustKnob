#![windows_subsystem = "windows"]

mod audio;
mod autostart;
mod clipboard;
mod config;
#[cfg(feature = "demo")]
mod demo;
mod device;
mod dragsrc;
mod dropzone;
mod gfx;
mod history;
mod hook;
mod install;
mod json;
mod lyrics;
mod menu;
mod i18n;
mod island;
mod media;
mod ntfy;
mod pace;
mod render;
mod snip;
mod spectrum;
mod surface;
mod text;
mod theme;
mod tray;
mod ui;
mod update;
mod upload;

use std::ffi::c_void;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("RustKnobWnd");

fn main() {
    let mut upload_at_start: Vec<std::path::PathBuf> = Vec::new();
    unsafe {
        match std::env::args().nth(1).as_deref() {
            Some("--install") => {
                let t = i18n::strings(i18n::resolve("auto"));
                if let Ok(exe) = install::install(install::Options { autostart: true }, t) {
                    install::launch(&exe);
                }
                return;
            }
            Some("--remove-ffuploader") => return menu::remove_ffuploader(),
            #[cfg(feature = "demo")]
            Some("--demo") => {
                demo::run(&std::env::args().nth(2).unwrap_or_else(|| "docs/img".into()));
                return;
            }
            Some("--update") => {
                // started by the running copy with a downloaded release: install it over the old one, keep the
                // autostart choice, and start again
                let t = i18n::strings(i18n::resolve("auto"));
                if let Ok(exe) = install::install(install::Options { autostart: autostart::is_enabled() }, t) {
                    install::launch_with(&exe, "--updated");
                }
                return;
            }
            Some("--updated") => island::JUST_UPDATED.store(true, std::sync::atomic::Ordering::Relaxed),
            Some("--upload") => {
                let files: Vec<std::path::PathBuf> = std::env::args().skip(2).filter(|a| !a.is_empty()).map(Into::into).collect();
                if upload::forward_to_running(&files) {
                    return; // the running RustKnob takes them
                }
                upload_at_start = files;
            }
            Some("--uninstall") => {
                let ask = !std::env::args().any(|a| a == "--silent");
                return install::uninstall(i18n::strings(i18n::resolve("auto")), ask);
            }
            Some("--snip") => {
                // asks the running RustKnob for the screenshot overlay (handy for another shortcut)
                if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
                    let _ = PostMessageW(h, hook::WM_APP_SNIP, WPARAM(0), LPARAM(0));
                }
                return;
            }
            Some("--quit") => {
                if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
                    let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return;
            }
            _ => {}
        }

        // a crash leaves a note next to the link list (the release build aborts silently otherwise)
        std::panic::set_hook(Box::new(|info| {
            let dir = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("RustKnob");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("crash.txt"), info.to_string());
        }));

        // a downloaded copy that is double-clicked: offer a one-click install, or an update when an older
        // build is installed; when this very build is installed, just open that one
        let mut setup = None;
        if std::env::args().len() == 1 && !install::running_installed() && !install::portable() {
            match install::installed() {
                install::Installed::No => setup = Some(false),
                install::Installed::Older => {
                    install::close_running(); // the old build holds the single-instance lock; it is about to be replaced
                    setup = Some(true);
                }
                install::Installed::Current => {
                    install::launch_installed();
                    return;
                }
            }
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let _single_instance = CreateMutexW(None, true, w!("RustKnobSingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        efficiency_mode(true);
        let _ = OleInitialize(None); // COM (single-threaded apartment) plus drag and drop

        let cfg = config::Config::load();
        let strings = i18n::strings(i18n::resolve(&cfg.language));
        let open_key = config::parse_hotkey(&cfg.hotkey, 'K' as u32);
        let settings_key = config::parse_hotkey(&cfg.settings_hotkey, 'K' as u32);
        let history_key = config::parse_hotkey(&cfg.history_hotkey, 'H' as u32);

        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(island::wndproc),
            hInstance: hinst,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), // the capsule's own pointer, whatever was set last
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // click-through, never activated, never in Alt-Tab: it must not steal focus from a fullscreen app
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            CLASS,
            w!("RustKnob"),
            WS_POPUP,
            0,
            0,
            16,
            16,
            None,
            None,
            hinst,
            None,
        )
        .unwrap();
        island::init(cfg, hwnd);
        dropzone::register(hwnd);

        if RegisterHotKey(hwnd, 1, open_key.0 | MOD_NOREPEAT, open_key.1).is_err() {
            MessageBoxW(None, &HSTRING::from(strings.hotkey_busy), w!("RustKnob"), MB_OK | MB_ICONWARNING);
        }
        let _ = RegisterHotKey(hwnd, 2, settings_key.0 | MOD_NOREPEAT, settings_key.1);
        let _ = RegisterHotKey(hwnd, 3, history_key.0 | MOD_NOREPEAT, history_key.1);
        let hooks = hook::install(hinst, hwnd);

        if let Some(update) = setup {
            island::open_setup(hwnd, update);
        }
        upload::enqueue(upload_at_start, hwnd.0 as isize, hook::WM_APP_UPLOAD);
        island::trim_memory();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        for h in hooks.into_iter().flatten() {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

/// Idle: low priority + EcoQoS, so the capsule never competes with a game. While it is on screen (`on` false)
/// it runs at normal speed: a frame must not take long, the mouse hooks wait for it.
pub unsafe fn efficiency_mode(on: bool) {
    let process = GetCurrentProcess();
    let _ = SetPriorityClass(process, if on { IDLE_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS });
    let throttle = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };
    let _ = SetProcessInformation(
        process,
        ProcessPowerThrottling,
        &throttle as *const _ as *const c_void,
        std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
    );
}
