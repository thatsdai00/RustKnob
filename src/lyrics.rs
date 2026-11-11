// Synced lyrics from LRCLIB (https://lrclib.net): looked up once per song on a thread of its own, parsed from the
// LRC text ("[01:23.45] a line") into (seconds, line) pairs. The pinned capsule asks which line is current.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::json;
use crate::update;

/// One word of a line, timed (seconds) and located in the line's text (byte range).
pub struct Word {
    pub start: f32,
    pub end: f32,
    pub from: usize,
    pub to: usize,
}

pub struct Line {
    pub start: f32,
    pub text: String,
    pub words: Vec<Word>, // empty: only the line is timed
}

pub struct Lyrics {
    pub key: String,
    lines: Vec<Line>, // sorted by time; empty when the song has none
}

impl Lyrics {
    /// The line sounding at `t` seconds: its index and the line (its text may be empty: an instrumental break).
    pub fn line_at(&self, t: f32) -> Option<(usize, &Line)> {
        let n = self.lines.partition_point(|l| l.start <= t);
        (n > 0).then(|| (n - 1, &self.lines[n - 1]))
    }
}

impl Lyrics {
    /// The line to have on show at `t`: the next one appears `lead` seconds before it is sung (it needs a moment to
    /// slide in and to be read), but never while the one before it still has words to come.
    pub fn line_shown(&self, t: f32, lead: f32) -> Option<(usize, &Line)> {
        let (i, line) = self.line_at(t + lead)?;
        if i > 0 {
            let prev = &self.lines[i - 1];
            if prev.words.last().is_some_and(|w| w.end > t) && line.start > t {
                return Some((i - 1, prev));
            }
        }
        Some((i, line))
    }
}

/// Where the sung part of a line ends at `t`: `points` are rising (time, x) pairs made from a line's words.
pub fn fill_x(points: &[(f32, f32)], t: f32) -> f32 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return 0.0 };
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    let n = points.partition_point(|p| p.0 <= t);
    let ((t0, x0), (t1, x1)) = (points[n - 1], points[n]);
    x0 + (x1 - x0) * ((t - t0) / (t1 - t0).max(1e-4))
}

static CURRENT: Mutex<Option<Arc<Lyrics>>> = Mutex::new(None);
/// Bumped for every lookup: a lookup that finishes after a newer one started throws its result away.
static LOOKUP: AtomicU64 = AtomicU64::new(0);

/// The lyrics of the song with this key, once they have arrived.
pub fn get(key: &str) -> Option<Arc<Lyrics>> {
    CURRENT.lock().ok().and_then(|g| g.clone()).filter(|l| l.key == key && !l.lines.is_empty())
}

/// Looks the song up. `duration` (seconds) narrows the match when the player reports it.
pub fn fetch(key: String, title: String, artist: String, duration: Option<u32>) {
    let id = LOOKUP.fetch_add(1, Ordering::AcqRel) + 1;
    if let Ok(mut g) = CURRENT.lock() {
        *g = None;
    }
    std::thread::spawn(move || {
        let stale = || LOOKUP.load(Ordering::Acquire) != id; // another song since: stop asking
        // LiriQo first (word-timed lyrics from Apple Music and others), LRCLIB when it has nothing
        let lines = liriqo(&title, &artist, duration).or_else(|| (!stale()).then(|| lookup(&title, &artist, duration, &stale)).flatten().map(|t| parse(&t))).unwrap_or_default();
        if let Ok(mut g) = CURRENT.lock() {
            if !stale() {
                *g = Some(Arc::new(Lyrics { key, lines }));
            }
        }
    });
}

fn lookup(title: &str, artist: &str, duration: Option<u32>, stale: &dyn Fn() -> bool) -> Option<String> {
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
        if stale() {
            return None;
        }
        let Some(body) = update::get(&format!("{base}/search?{query}"), 4 << 20).and_then(|b| String::from_utf8(b).ok()) else { continue };
        if let Some(found) = best_synced(&body, duration) {
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

/// Among the results of a search, the synced lyrics of the recording whose length is closest to the one playing
/// (the same song comes in several versions, and their lyrics are timed differently).
fn best_synced(body: &str, duration: Option<u32>) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    for object in objects(body) {
        let Some(text) = json::str_field(object, "syncedLyrics").filter(|v| v.contains('[')) else { continue };
        let length = number_field(object, "duration");
        let diff = match (duration, length) {
            (Some(want), Some(have)) => (want as f32 - have).abs(),
            _ => 1000.0, // unknown: behind every known length
        };
        let max = if duration.is_some() { 6.0 } else { f32::INFINITY }; // a known length must be close
        if diff <= max && best.as_ref().is_none_or(|(d, _)| diff < *d) {
            best = Some((diff, text));
        }
    }
    best.map(|(_, text)| text)
}

/// The top-level `{...}` pieces of a JSON array (strings may contain braces).
fn objects(body: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    let (mut in_string, mut escaped) = (false, false);
    for (i, c) in body.char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(&body[start..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

fn number_field(object: &str, key: &str) -> Option<f32> {
    let after = &object[object.find(&format!("\"{key}\""))? + key.len() + 2..];
    let value = after.trim_start().strip_prefix(':')?.trim_start();
    let end = value.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(value.len());
    value[..end].parse().ok()
}

fn clean_title(title: &str) -> &str {
    let cut = [" - ", " (", " [", " – "].iter().filter_map(|m| title.find(m)).min().unwrap_or(title.len());
    title[..cut].trim()
}

/// "[mm:ss.xx]" stamps (a line may have several) in front of the text.
fn parse(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    // "[offset:+500]": the lines come that many milliseconds earlier (negative: later)
    let offset = text.lines().find_map(|l| l.trim().strip_prefix("[offset:")?.split(']').next()?.trim().parse::<f32>().ok()).unwrap_or(0.0) / 1000.0;
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
            out.push(Line { start: t - offset, text: rest.to_string(), words: Vec::new() });
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

// ---- LiriQo (https://github.com/AlFarrizi-Studio/LiriQo): one endpoint over Apple Music, Musixmatch, QQ, ... ----

/// The best timed lyrics it knows: word- or syllable-timed ones when there are any, else its first line-timed set.
fn liriqo(title: &str, artist: &str, duration: Option<u32>) -> Option<Vec<Line>> {
    let mut url = format!("https://api.liriqo-alfarrizi.my.id/v1/lyrics?title={}&artist={}", json::url_encode(title), json::url_encode(artist));
    if let Some(d) = duration.filter(|d| *d > 0) {
        url.push_str(&format!("&duration={d}"));
    }
    let body = String::from_utf8(update::get(&url, 8 << 20)?).ok()?;
    let root = J::parse(&body)?;
    let mut best: Option<(bool, Vec<Line>)> = None;
    for track in root.get("tracks")?.arr() {
        let lines = liriqo_lines(track);
        if lines.is_empty() {
            continue;
        }
        let wordy = lines.iter().filter(|l| !l.words.is_empty()).count() * 2 >= lines.len();
        if best.as_ref().is_none_or(|(w, _)| wordy && !*w) {
            best = Some((wordy, lines));
        }
    }
    best.map(|(_, lines)| lines)
}

fn liriqo_lines(track: &J) -> Vec<Line> {
    let mut out = Vec::new();
    for l in track.get("timed").map_or(&[][..], |t| t.arr()) {
        let (Some(text), Some(start)) = (l.get("text").and_then(J::str), l.get("start").and_then(J::num)) else { continue };
        let instrumental = matches!(l.get("isInstrumental"), Some(J::Bool(true)));
        let text = if instrumental { "" } else { text.trim() };
        let mut words = Vec::new();
        let mut pos = 0;
        for w in l.get("words").map_or(&[][..], |w| w.arr()) {
            let word = w.get("text").and_then(J::str).unwrap_or("").trim();
            if word.is_empty() {
                continue;
            }
            // each word is found in the line's own text, after the one before it
            let Some(at) = text[pos..].find(word) else {
                words.clear();
                break;
            };
            let (from, to) = (pos + at, pos + at + word.len());
            pos = to;
            let s = w.get("start").and_then(J::num).unwrap_or(0.0) as f32 / 1000.0;
            let e = w.get("end").and_then(J::num).unwrap_or(0.0) as f32 / 1000.0;
            words.push(Word { start: s, end: e.max(s), from, to });
        }
        out.push(Line { start: start as f32 / 1000.0, text: text.to_string(), words });
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Just enough JSON to read the response.
enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn parse(text: &str) -> Option<J> {
        Reader { s: text.as_bytes(), i: 0, depth: 0 }.value()
    }
    fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        if let J::Str(s) = self { Some(s) } else { None }
    }
    fn num(&self) -> Option<f64> {
        if let J::Num(n) = self { Some(*n) } else { None }
    }
    fn arr(&self) -> &[J] {
        if let J::Arr(v) = self { v } else { &[] }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32, // nesting: a hostile response must not run the stack out
}

impl Reader<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.s.get(self.i)?.is_ascii_whitespace() {
            self.i += 1;
        }
        self.s.get(self.i).copied()
    }

    fn value(&mut self) -> Option<J> {
        if self.depth > 64 {
            return None;
        }
        self.depth += 1;
        let v = self.value_inner();
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self) -> Option<J> {
        match self.peek()? {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    match self.peek()? {
                        b'}' => break,
                        b',' => self.i += 1,
                        _ => {
                            let k = self.string()?;
                            if self.peek()? != b':' {
                                return None;
                            }
                            self.i += 1;
                            v.push((k, self.value()?));
                        }
                    }
                }
                self.i += 1;
                Some(J::Obj(v))
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    match self.peek()? {
                        b']' => break,
                        b',' => self.i += 1,
                        _ => v.push(self.value()?),
                    }
                }
                self.i += 1;
                Some(J::Arr(v))
            }
            b'"' => self.string().map(J::Str),
            b't' => {
                self.i += 4;
                Some(J::Bool(true))
            }
            b'f' => {
                self.i += 5;
                Some(J::Bool(false))
            }
            b'n' => {
                self.i += 4;
                Some(J::Null)
            }
            _ => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|c| matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(J::Num)
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s.get(self.i) == Some(&b'\\') && self.s.get(self.i + 1) == Some(&b'u') {
                                self.i += 2;
                                let low = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(char::from_u32(cp).unwrap_or('\u{FFFD}').encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }
}
