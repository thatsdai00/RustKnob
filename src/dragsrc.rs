// Which drags should offer the "drop to upload" capsule: only ones that start on files in Explorer or on
// the desktop. Text dragged out of a browser, folders and the like are left alone.

use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_STREAM};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

/// Is the drag that started at `origin` (screen px) a drag of selected files?
pub unsafe fn starts_on_files(origin: POINT) -> bool {
    let under = WindowFromPoint(origin);
    if under.0.is_null() {
        return false;
    }
    let root = GetAncestor(under, GA_ROOT);
    let desktop = match class_of(root).as_str() {
        "CabinetWClass" => false, // an Explorer window
        "Progman" | "WorkerW" => true, // the desktop
        _ => return false,
    };
    // when Explorer cannot be asked, trust the window and show the capsule
    selection_is_files(root, desktop).unwrap_or(true)
}

unsafe fn selection_is_files(root: HWND, desktop: bool) -> Option<bool> {
    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
    let dispatch = if desktop {
        let mut found = 0i32;
        let csidl = VARIANT::from(CSIDL_DESKTOP as i32);
        windows.FindWindowSW(&csidl, &VARIANT::default(), SWC_DESKTOP, &mut found, SWFO_NEEDDISPATCH).ok()?
    } else {
        let mut hit = None;
        for i in 0..windows.Count().ok()? {
            let Ok(d) = windows.Item(&VARIANT::from(i)) else { continue };
            if d.cast::<IWebBrowserApp>().ok().and_then(|b| b.HWND().ok()).is_some_and(|h| h.0 == root.0 as isize) {
                hit = Some(d);
                break;
            }
        }
        hit?
    };
    let provider: IServiceProvider = dispatch.cast().ok()?;
    let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser).ok()?;
    let view: IFolderView = browser.QueryActiveShellView().ok()?.cast().ok()?;
    let items: IShellItemArray = view.Items(SVGIO_SELECTION).ok()?;
    let count = items.GetCount().ok()?;
    if count == 0 {
        return Some(false); // a rubber band or an empty drag
    }
    for i in 0..count {
        let item = items.GetItemAt(i).ok()?;
        let attrs = item.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM).ok()?;
        // a real folder: yes folder, not a stream (a .zip is both and counts as a file)
        if attrs.contains(SFGAO_FOLDER) && !attrs.contains(SFGAO_STREAM) {
            return Some(false);
        }
    }
    Some(true)
}

