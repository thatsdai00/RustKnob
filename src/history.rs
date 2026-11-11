// Upload link history. Same file format as FFUploader's linkler.txt ("time<TAB>name<TAB>link" per
// line), so its old links show up too. New ones go to %LOCALAPPDATA%\FFUploader\linkler.txt.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::menu;

#[derive(Clone)]
pub struct Entry {
    pub stamp: String,
    pub name: String,
    pub link: String,
}

/// The upload thread appends while the UI thread may remove: one at a time, or a link can get lost.
static FILES: Mutex<()> = Mutex::new(());

fn local_appdata() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
}

fn own_file() -> PathBuf {
    local_appdata().join("RustKnob").join("linkler.txt")
}

/// Every list that exists: ours and the old FFUploader's.
fn files() -> Vec<PathBuf> {
    let mut all = vec![own_file(), local_appdata().join("FFUploader").join("linkler.txt")];
    if let Some(dir) = menu::ffuploader_dir() {
        all.push(dir.join("linkler.txt"));
    }
    all.push(PathBuf::from(r"C:\ffinstaller\linkler.txt"));
    all.retain(|p| p.exists());
    all.dedup();
    all
}

/// Newest first, each link once.
pub fn load() -> Vec<Entry> {
    let _lock = FILES.lock();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in files() {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            if let (Some(stamp), Some(name), Some(link)) = (parts.next(), parts.next(), parts.next()) {
                if !link.is_empty() && seen.insert(link.to_string()) {
                    entries.push(Entry { stamp: stamp.to_string(), name: name.to_string(), link: link.trim().to_string() });
                }
            }
        }
    }
    entries.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    entries
}

pub fn append(name: &str, link: &str) {
    let stamp = unsafe { stamp_now() };
    let line = format!("{stamp}\t{}\t{link}\n", name.replace(['\t', '\n', '\r'], " "));
    let _lock = FILES.lock();
    let path = own_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

/// Drops every line with this link from all lists.
pub fn remove(link: &str) {
    let _lock = FILES.lock();
    for file in files() {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let kept: String = text.lines().filter(|l| l.split('\t').nth(2).map(str::trim) != Some(link)).map(|l| format!("{l}\n")).collect();
        if kept.len() == text.len() {
            continue; // the link is not in this list: leave the file alone
        }
        // written beside it, then swapped in: a crash halfway cannot cut the list short
        let tmp = file.with_extension("txt.tmp");
        if std::fs::write(&tmp, kept).is_ok() && std::fs::rename(&tmp, &file).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// "2026-11-08 14:02:11" in local time.
unsafe fn stamp_now() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = GetLocalTime();
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// "fuckingfast.net/abc123" from a full link.
pub fn short(link: &str) -> &str {
    link.trim_start_matches("https://").trim_start_matches("http://")
}

/// Copies the lines of another link list into ours (links we do not have yet).
pub fn import_from(path: &std::path::Path) {
    let _lock = FILES.lock();
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let known: HashSet<String> = std::fs::read_to_string(own_file())
        .map(|t| t.lines().filter_map(|l| l.split('\t').nth(2).map(|s| s.trim().to_string())).collect())
        .unwrap_or_default();
    let fresh: String = text
        .lines()
        .filter(|l| l.split('\t').nth(2).is_some_and(|link| !known.contains(link.trim())))
        .map(|l| format!("{l}\n"))
        .collect();
    if fresh.is_empty() {
        return;
    }
    if let Some(dir) = own_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(own_file())
        .and_then(|mut f| std::io::Write::write_all(&mut f, fresh.as_bytes()));
}
