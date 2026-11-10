// Synced lyrics from LRCLIB (https://lrclib.net): looked up once per song on a thread of its own, parsed from the
// LRC text ("[01:23.45] a line") into (seconds, line) pairs. The pinned capsule asks which line is current.

use std::sync::{Arc, Mutex};

use crate::json;
use crate::update;

pub struct Lyrics {
    pub key: String,
    lines: Vec<(f32, String)>, // sorted by time; empty when the song has none
}

impl Lyrics {
    /// The line sounding at `t` seconds: its index and its text (possibly empty: an instrumental break).
    pub fn line_at(&self, t: f32) -> Option<(usize, &str)> {
        let n = self.lines.partition_point(|(at, _)| *at <= t);
        (n > 0).then(|| (n - 1, self.lines[n - 1].1.as_str()))
    }
}

static CURRENT: Mutex<Option<Arc<Lyrics>>> = Mutex::new(None);

/// The lyrics of the song with this key, once they have arrived.
pub fn get(key: &str) -> Option<Arc<Lyrics>> {
    CURRENT.lock().ok().and_then(|g| g.clone()).filter(|l| l.key == key && !l.lines.is_empty())
}

/// Looks the song up. `duration` (seconds) narrows the match when the player reports it.
pub fn fetch(key: String, title: String, artist: String, duration: Option<u32>) {
    if let Ok(mut g) = CURRENT.lock() {
        *g = None;
    }
    std::thread::spawn(move || {
        let lines = unsafe { lookup(&title, &artist, duration) }.map(|t| parse(&t)).unwrap_or_default();
        if let Ok(mut g) = CURRENT.lock() {
            *g = Some(Arc::new(Lyrics { key, lines }));
        }
    });
}

unsafe fn lookup(title: &str, artist: &str, duration: Option<u32>) -> Option<String> {
    let base = "https://lrclib.net/api";
    let (t, a) = (json::url_encode(title), json::url_encode(artist));
    let mut exact = format!("{base}/get?track_name={t}&artist_name={a}");
    if let Some(d) = duration.filter(|d| *d > 0) {
        exact.push_str(&format!("&duration={d}"));
    }
    if let Some(body) = update::get(&exact, 1 << 20).and_then(|b| String::from_utf8(b).ok()) {
        if let Some(found) = synced(&body) {
            return Some(found);
        }
    }
    // the player's title often carries extras ("- Remastered", "(feat. ...)"): search for the plain one
    let plain = json::url_encode(clean_title(title));
    // then looser: the artist spelled differently (several artists, "&") matters less than the title
    let first_artist = json::url_encode(artist.split([',', '&']).next().unwrap_or(artist).trim());
    for query in [format!("track_name={plain}&artist_name={a}"), format!("q={plain}%20{first_artist}"), format!("q={plain}")] {
        let Some(body) = update::get(&format!("{base}/search?{query}"), 4 << 20).and_then(|b| String::from_utf8(b).ok()) else { continue };
        if let Some(found) = synced(&body) {
            return Some(found);
        }
    }
    None
}

/// The first non-null `syncedLyrics` string in a response (one object, or a list of them).
fn synced(body: &str) -> Option<String> {
    let mut rest = body;
    while let Some(i) = rest.find("\"syncedLyrics\"") {
        if let Some(v) = json::str_field(&rest[i..], "syncedLyrics").filter(|v| v.contains('[')) {
            return Some(v);
        }
        rest = &rest[i + 14..];
    }
    None
}

fn clean_title(title: &str) -> &str {
    let cut = [" - ", " (", " [", " – "].iter().filter_map(|m| title.find(m)).min().unwrap_or(title.len());
    title[..cut].trim()
}

/// "[mm:ss.xx]" stamps (a line may have several) in front of the text.
fn parse(text: &str) -> Vec<(f32, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { break };
            let tag = &inner[..end];
            rest = inner[end + 1..].trim_start();
            if let Some((m, s)) = tag.split_once(':') {
                if let (Ok(m), Ok(s)) = (m.parse::<f32>(), s.parse::<f32>()) {
                    stamps.push(m * 60.0 + s);
                }
            }
        }
        for t in stamps {
            out.push((t, rest.to_string()));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}
