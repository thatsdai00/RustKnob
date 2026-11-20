// Starting at login, the two ways Windows itself offers and shows under Task Manager > Startup apps: the HKCU Run
// entry and a shortcut in the Startup folder. Explorer goes through them apart, so when one is skipped at a login
// the other still starts it (a second start finds the first one running and exits). Nothing hidden: no
// scheduled task, no PowerShell. Earlier builds also made a logon task through PowerShell; that is removed.

use std::path::{Path, PathBuf};

use windows::core::w;
use windows::Win32::System::Registry::*;

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

/// The shortcut in the user's Startup folder.
fn startup_link() -> PathBuf {
    crate::install::env_path("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs\Startup\RustKnob.lnk")
}

pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        let run = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_QUERY_VALUE, &mut key).is_ok() && {
            let found = RegQueryValueExW(key, w!("RustKnob"), None, None, None, None).is_ok();
            let _ = RegCloseKey(key);
            found
        };
        run || startup_link().exists()
    }
}

/// Starts this exe at login (or stops doing so): the settings switch.
pub fn set(on: bool) {
    match std::env::current_exe() {
        Ok(exe) => set_for(on, &exe),
        Err(_) => set_for(false, Path::new("")),
    }
}

/// Starts `exe` at login (or removes both entries).
pub fn set_for(on: bool, exe: &Path) {
    set_entry(on, exe);
    let link = startup_link();
    if on {
        unsafe {
            let _ = crate::install::create_shortcut(exe, &link);
        }
    } else {
        let _ = std::fs::remove_file(link);
    }
    remove_task();
}

/// The Run entry.
fn set_entry(on: bool, exe: &Path) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
            let _ = RegSetValueExW(key, w!("RustKnob"), 0, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, w!("RustKnob"));
        }
        let _ = RegCloseKey(key);
    }
}

/// The logon task earlier builds made, when it is still there (a look at the registry first: Windows' own
/// schtasks only runs when there is something to remove).
pub fn remove_task() {
    let there = unsafe {
        let mut key = HKEY::default();
        let open = RegOpenKeyExW(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\TaskCache\Tree\RustKnob"), 0, KEY_QUERY_VALUE, &mut key).is_ok();
        if open {
            let _ = RegCloseKey(key);
        }
        open
    };
    if there {
        let schtasks = crate::install::system_exe(r"System32\schtasks.exe");
        crate::install::start(&schtasks, &["/Delete", "/TN", "RustKnob", "/F"], false, true);
    }
}

#[cfg(test)]
mod tests {
    /// On, then off, on this PC's real Run key and Startup folder: `cargo test autostart -- --ignored`
    #[test]
    #[ignore]
    fn on_and_off() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let exe = std::env::temp_dir().join("rustknob-autostart-test.exe");
        super::set_for(true, &exe);
        assert!(super::is_enabled());
        assert!(super::startup_link().exists(), "startup shortcut");
        super::set_for(false, &exe);
        assert!(!super::is_enabled());
        assert!(!super::startup_link().exists());
    }
}
