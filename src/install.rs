// One-click install / uninstall, no installer program needed: the exe copies itself into
// %LOCALAPPDATA%\Programs\RustKnob, starts with Windows, gets a Start menu shortcut and an
// "Apps & features" entry. All per-user: no admin rights.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::i18n::Strings;
use crate::{autostart, menu, CLASS};

const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\RustKnob");
const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_default())
}

pub fn install_dir() -> PathBuf {
    env_path("LOCALAPPDATA").join("Programs").join("RustKnob")
}

fn installed_exe() -> PathBuf {
    install_dir().join("RustKnob.exe")
}

fn shortcut_path() -> PathBuf {
    env_path("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs\RustKnob.lnk")
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// What an exe that is not the installed copy finds on this PC.
#[derive(PartialEq)]
pub enum Installed {
    No,
    Older,   // an older build is installed: this one updates it
    Current, // this very build (or a newer one) is installed already
}

fn build_stamp() -> u64 {
    env!("RK_BUILD").parse().unwrap_or(0)
}

pub fn installed() -> Installed {
    if !installed_exe().exists() {
        return Installed::No;
    }
    let theirs = unsafe { registered_build() }.unwrap_or(0); // no stamp: installed before builds were numbered
    if theirs < build_stamp() { Installed::Older } else { Installed::Current }
}

unsafe fn registered_build() -> Option<u64> {
    let mut key = HKEY::default();
    RegOpenKeyExW(HKEY_CURRENT_USER, UNINSTALL_KEY, 0, KEY_QUERY_VALUE, &mut key).ok().ok()?;
    let mut buf = [0u16; 32];
    let mut size = (buf.len() * 2) as u32;
    let ok = RegQueryValueExW(key, w!("BuildStamp"), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut size)).is_ok();
    let _ = RegCloseKey(key);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    ok.then(|| String::from_utf16_lossy(&buf[..len]).parse().ok()).flatten()
}

/// Is this process the installed copy?
pub fn running_installed() -> bool {
    std::env::current_exe().is_ok_and(|p| lower(&p).starts_with(&lower(&install_dir())))
}

/// A copy that is meant to run where it is: it has its settings file beside it, or the user said
/// "don't install" once before.
pub fn portable() -> bool {
    let dir = exe_dir();
    dir.join("rustknob.ini").exists() || dir.join("digitalknob.ini").exists() || dir.join("portable.flag").exists()
}

pub fn remember_portable() {
    let _ = std::fs::write(exe_dir().join("portable.flag"), "");
}

pub fn info(text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("RustKnob"), MB_OK | MB_ICONINFORMATION | MB_TOPMOST);
    }
}

/// Asks a running copy (not this process) to quit and gives it a moment.
pub fn close_running() {
    unsafe {
        if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == std::process::id() {
                return;
            }
            let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            // wait for it to be gone (it lets go of its exe then), not a fixed guess
            use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
            match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Ok(process) => {
                    WaitForSingleObject(process, 10_000);
                    let _ = CloseHandle(process);
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(1200)),
            }
        }
    }
}

/// Copies the exe (and a settings file beside it) into the install folder, registers everything
/// and returns the installed exe.
pub struct Options {
    pub autostart: bool,
}

pub fn install(opts: Options, _t: &Strings) -> std::io::Result<PathBuf> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    close_running();
    let (dir, target) = (install_dir(), installed_exe());
    std::fs::create_dir_all(&dir)?;
    let me = std::env::current_exe()?;
    if lower(&me) != lower(&target) {
        // the old copy may need a moment to release the file
        let mut copied = std::fs::copy(&me, &target);
        for _ in 0..5 {
            if copied.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
            copied = std::fs::copy(&me, &target);
        }
        copied?;
        // settings travel with it (the project's old file name counts too), but never replace the ones of an
        // installed copy that is being updated
        let kept = dir.join("rustknob.ini");
        if !kept.exists() {
            if let Some(ini) = ["rustknob.ini", "digitalknob.ini"].iter().map(|n| exe_dir().join(n)).find(|p| p.exists()) {
                let _ = std::fs::copy(ini, kept);
            }
        }
    }
    remove_legacy();
    autostart::set_for(opts.autostart, &target);
    menu::remove_old_entries(); // the Explorer right-click entry of earlier builds is gone (drag and drop replaced it)
    unsafe {
        let _ = create_shortcut(&target);
        register_uninstall(&target);
    }
    Ok(target)
}

pub fn launch(exe: &Path) {
    let _ = std::process::Command::new(exe).spawn();
}

/// Starts `exe` with one argument (the updater starts the new copy with `--updated`).
pub fn launch_with(exe: &Path, arg: &str) {
    let _ = std::process::Command::new(exe).arg(arg).spawn();
}

pub fn launch_installed() {
    launch(&installed_exe());
}

unsafe fn create_shortcut(target: &Path) -> windows::core::Result<()> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
    link.SetPath(&HSTRING::from(target.as_os_str()))?;
    link.SetWorkingDirectory(&HSTRING::from(install_dir().as_os_str()))?;
    link.SetDescription(w!("RustKnob"))?;
    let file: IPersistFile = link.cast()?;
    file.Save(&HSTRING::from(shortcut_path().as_os_str()), true)
}

unsafe fn set_str(key: HKEY, name: PCWSTR, value: &str) {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
    let _ = RegSetValueExW(key, name, 0, REG_SZ, Some(bytes));
}

/// The "Apps & features" entry, so it can be removed from Windows settings too.
unsafe fn register_uninstall(target: &Path) {
    let mut key = HKEY::default();
    if RegCreateKeyExW(HKEY_CURRENT_USER, UNINSTALL_KEY, 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None).is_err() {
        return;
    }
    let exe = target.to_string_lossy();
    set_str(key, w!("DisplayName"), "RustKnob");
    set_str(key, w!("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    set_str(key, w!("BuildStamp"), &build_stamp().to_string());
    set_str(key, w!("Publisher"), "RustKnob");
    set_str(key, w!("DisplayIcon"), &exe);
    set_str(key, w!("InstallLocation"), &install_dir().to_string_lossy());
    set_str(key, w!("UninstallString"), &format!("\"{exe}\" --uninstall"));
    for flag in [w!("NoModify"), w!("NoRepair")] {
        let _ = RegSetValueExW(key, flag, 0, REG_DWORD, Some(&1u32.to_le_bytes()));
    }
    let _ = RegCloseKey(key);
}

/// Removes autostart, shortcut, the Apps entry and (after this process ends) the install folder.
/// The settings file in %APPDATA% stays.
pub fn uninstall(t: &Strings, ask: bool) {
    unsafe {
        if ask && MessageBoxW(None, &HSTRING::from(t.uninstall_ask), w!("RustKnob"), MB_YESNO | MB_ICONQUESTION | MB_TOPMOST) != IDYES {
            return;
        }
        close_running();
        autostart::set_for(false, Path::new(""));
        menu::remove_old_entries();
        let _ = std::fs::remove_file(shortcut_path());
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, UNINSTALL_KEY);
    }
    // a running exe cannot delete itself: a detached shell does it a few seconds later
    delete_later(&install_dir());
    info(t.uninstalled);
}

/// Removes what the project left behind under its old name (DigitalKnob): the running copy,
/// autostart entry, shortcut, Apps entry, right-click verb and install folder.
fn remove_legacy() {
    unsafe {
        if let Ok(h) = FindWindowW(w!("DigitalKnobWnd"), PCWSTR::null()) {
            let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            std::thread::sleep(std::time::Duration::from_millis(1200));
        }
        let mut run = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), 0, KEY_SET_VALUE, &mut run).is_ok() {
            let _ = RegDeleteValueW(run, w!("DigitalKnob"));
            let _ = RegCloseKey(run);
        }
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\DigitalKnob"));
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, w!(r"Software\Classes\*\shell\DigitalKnobUpload\command"));
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, w!(r"Software\Classes\*\shell\DigitalKnobUpload"));
    }
    let _ = std::fs::remove_file(env_path("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs\DigitalKnob.lnk"));
    let old_dir = env_path("LOCALAPPDATA").join("Programs").join("DigitalKnob");
    if old_dir.exists() && std::fs::remove_dir_all(&old_dir).is_err() {
        delete_later(&old_dir); // still locked
    }
}

/// Deletes a folder a few seconds from now, from a detached shell (a running exe cannot delete itself).
/// Only an absolute path: with %LOCALAPPDATA% unset the path would be relative to wherever this runs.
pub fn delete_later(dir: &Path) {
    if !dir.is_absolute() {
        return;
    }
    let _ = std::process::Command::new("cmd")
        .raw_arg(format!("/C ping 127.0.0.1 -n 4 >nul & rmdir /S /Q \"{}\"", dir.display()))
        .creation_flags(NO_WINDOW)
        .spawn();
}
