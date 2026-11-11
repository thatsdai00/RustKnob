// HKCU Run entry that starts the knob at login.

use std::path::Path;

use windows::core::w;
use windows::Win32::System::Registry::*;

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_QUERY_VALUE, &mut key).is_err() {
            return false;
        }
        let found = RegQueryValueExW(key, w!("RustKnob"), None, None, None, None).is_ok();
        let _ = RegCloseKey(key);
        found
    }
}

/// Starts this exe at login (or stops doing so). For the settings switch: the logon task, which takes
/// PowerShell a second or two, is made on a thread of its own (the UI thread also runs the input hooks).
pub fn set(on: bool) {
    let exe = std::env::current_exe().ok();
    let on = on && exe.is_some();
    let exe = exe.unwrap_or_default();
    set_entry(on, &exe);
    std::thread::spawn(move || set_task(on, &exe));
}

/// Starts `exe` at login (or removes the entry), and waits until it is done (the installer exits right after).
pub fn set_for(on: bool, exe: &Path) {
    set_entry(on, exe);
    set_task(on, exe);
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

/// Windows did not always run the Run entry at login, so a logon task starts it too
/// (a second start just finds the first one and exits).
fn set_task(on: bool, exe: &Path) {
    use std::os::windows::process::CommandExt;
    // PowerShell also ends a '...' string at the typographic single quotes: double them all
    let exe = exe.to_string_lossy().replace(['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "''");
    let script = if on {
        format!(
            "$a = New-ScheduledTaskAction -Execute '{exe}';              $t = New-ScheduledTaskTrigger -AtLogOn -User ([Security.Principal.WindowsIdentity]::GetCurrent().Name);              $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew;              Register-ScheduledTask -TaskName RustKnob -Action $a -Trigger $t -Settings $s -Force | Out-Null"
        )
    } else {
        "Unregister-ScheduledTask -TaskName RustKnob -Confirm:$false -ErrorAction SilentlyContinue".to_string()
    };
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(0x0800_0000)
        .status();
}
