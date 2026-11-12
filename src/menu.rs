// What is left of the Explorer right-click entry: removing it. Earlier builds added an "Upload" verb to the
// classic menu and (on Windows 11) a small signed package for the first-level menu. Dragging a file onto the
// capsule replaced both, so installing, updating and uninstalling clean them up. Also removes the older
// FFUploader (see `remove_ffuploader`).

use std::path::PathBuf;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::System::Registry::*;

use crate::history;

const OLD_VERB: PCWSTR = w!(r"Software\Classes\*\shell\RustKnobUpload");
const OLD_VERB_COMMAND: PCWSTR = w!(r"Software\Classes\*\shell\RustKnobUpload\command");
const FFUPLOADER_KEY: PCWSTR = w!(r"Software\FFUploader");
const PACKAGE_NAME: &str = "RustKnob.Shell";

/// Removes the classic "Upload" verb and the first-level package, when they are there.
pub fn remove_old_entries() {
    unsafe {
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, OLD_VERB_COMMAND);
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, OLD_VERB);
    }
    if package_registered() {
        let script = format!("Get-AppxPackage -Name '{PACKAGE_NAME}' | Remove-AppxPackage");
        crate::install::start(&crate::install::powershell(), &["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-Command", &script], true, true);
    }
    notify_shell();
}

/// Is the package registered for this user? (A look at the registry: no PowerShell for the common case.)
fn package_registered() -> bool {
    unsafe {
        let mut key = HKEY::default();
        let path = w!(r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages");
        if RegOpenKeyExW(HKEY_CURRENT_USER, path, 0, KEY_ENUMERATE_SUB_KEYS, &mut key).is_err() {
            return false;
        }
        let mut found = false;
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, index, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
                break;
            }
            if String::from_utf16_lossy(&name[..len as usize]).starts_with(&format!("{PACKAGE_NAME}_")) {
                found = true;
                break;
            }
        }
        let _ = RegCloseKey(key);
        found
    }
}

/// The folder FFUploader was installed in, if any (its old link list lives there).
pub fn ffuploader_dir() -> Option<PathBuf> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, FFUPLOADER_KEY, 0, KEY_QUERY_VALUE, &mut key).ok().ok()?;
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        let ok = RegQueryValueExW(key, w!("InstallDir"), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size)).is_ok();
        let _ = RegCloseKey(key);
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        (ok && len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// Tells Explorer the context menu changed.
fn notify_shell() {
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Removes the old FFUploader completely: its Windows 11 menu package, registry entries and
/// install folder. Its link list is copied into ours first.
pub fn remove_ffuploader() {
    // the folder comes from the registry: delete it only when it looks like FFUploader's own
    let dir = ffuploader_dir()
        .filter(|d| d.join("linkler.txt").exists() || d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("ffinstaller") || n.eq_ignore_ascii_case("FFUploader")))
        .unwrap_or_else(|| PathBuf::from(r"C:\ffinstaller"));
    history::import_from(&dir.join("linkler.txt"));
    history::import_from(&PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("FFUploader").join("linkler.txt"));

    // the sparse package that put "Upload" into the Windows 11 menu
    let script = "Get-AppxPackage -Name 'FFUploader.Shell' | Remove-AppxPackage";
    crate::install::start(&crate::install::powershell(), &["-NoProfile", "-WindowStyle", "Hidden", "-Command", script], true, true);

    unsafe {
        for key in [
            w!(r"Software\Classes\*\shell\KarsiyaYukle\command"),
            w!(r"Software\Classes\*\shell\KarsiyaYukle"),
            w!(r"Software\Classes\CLSID\{C4E8A1D7-3B59-4F02-9A6E-71D2B8C04F15}\InProcServer32"),
            w!(r"Software\Classes\CLSID\{C4E8A1D7-3B59-4F02-9A6E-71D2B8C04F15}"),
            FFUPLOADER_KEY,
        ] {
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, key);
        }
    }
    notify_shell();

    if dir.exists() && std::fs::remove_dir_all(&dir).is_err() {
        crate::install::delete_later(&dir);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        let _ = std::fs::remove_dir_all(PathBuf::from(local).join("FFUploader")); // never a relative path
    }
}
