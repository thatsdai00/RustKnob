// Now-playing info and transport control through Windows SMTC.
// A listener thread sleeps until the media changes; it keeps the current Song bitmaps ready
// and tells the main window when a *different* song starts playing.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform, ColorManagementMode, ExifOrientationMode};
use windows::Media::Control::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::text::{raster, TextBmp};

pub struct Song {
    pub big: TextBmp,           // title (or the artist when there is no title)
    pub small: Option<TextBmp>, // artist
    pub line: TextBmp,          // "Artist — Title" on one line
    pub art: Option<Art>,       // the cover, only fetched while the media is pinned
    pub key: String,            // "artist|title": tells two songs apart
}

/// The cover as a small square picture and the color it is built around.
pub const ART_PX: usize = 64;

pub struct Art {
    pub px: Vec<u32>,     // ART_PX x ART_PX, premultiplied 0xAARRGGBB
    pub accent: [f32; 3], // 0..255, bright enough to read on black
}

/// The pinned capsule wants cover art: it is only fetched (and decoded) while this is set.
static PIN_ON: AtomicBool = AtomicBool::new(false);
static PLAYING: AtomicBool = AtomicBool::new(false);
static WAKE: Mutex<Option<Sender<()>>> = Mutex::new(None);
/// Synced lyrics wanted (only looked up while the media is pinned as well).
static LYRICS_ON: AtomicBool = AtomicBool::new(false);
/// Where the song was at the last timeline update: seconds, when that was read, and whether it was playing.
static TIMELINE: Mutex<Option<(f32, Instant, bool)>> = Mutex::new(None);

/// Turns the lyrics lookup on or off.
pub fn set_lyrics(on: bool) {
    if LYRICS_ON.swap(on, Ordering::Relaxed) != on && on {
        wake();
    }
}

fn wake() {
    if let Ok(g) = WAKE.lock() {
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(());
        }
    }
}

/// How far into the song it is right now, in seconds (the player's last report, carried on by the clock).
pub fn position() -> Option<f32> {
    let (pos, at, playing) = (*TIMELINE.lock().ok()?)?;
    Some(if playing { pos + at.elapsed().as_secs_f32() } else { pos })
}

pub fn playing() -> bool {
    PLAYING.load(Ordering::Relaxed)
}

/// Turns the cover art (and the messages the pinned capsule lives on) on or off.
pub fn set_pin(on: bool) {
    if PIN_ON.swap(on, Ordering::Relaxed) != on && on {
        wake(); // read the current song again, this time with its cover
    }
}

static SONG: Mutex<Option<Arc<Song>>> = Mutex::new(None);

/// Set after a play/pause toggle: 1 = now playing, 2 = now paused.
pub static PLAY_EVENT: AtomicU32 = AtomicU32::new(0);

pub fn song() -> Option<Arc<Song>> {
    SONG.lock().ok().and_then(|s| s.clone())
}

fn set_song(s: Option<Arc<Song>>) {
    if let Ok(mut g) = SONG.lock() {
        *g = s;
    }
}

/// Starts the listener. `song_msg` is posted to `hwnd` when a new song starts playing.
pub fn start(hwnd: isize, song_msg: u32, pin_msg: u32) {
    std::thread::spawn(move || {
        let _ = run(hwnd, song_msg, pin_msg);
    });
}

struct Listener {
    mgr: GlobalSystemMediaTransportControlsSessionManager,
    tx: Sender<()>,
    hwnd: isize,
    song_msg: u32,
    pin_msg: u32, // posted when what the pinned capsule shows changed
    has_art: bool,
    last_key: String,
    first: bool,
    app: String,
    sub: Option<(GlobalSystemMediaTransportControlsSession, EventRegistrationToken, EventRegistrationToken, EventRegistrationToken)>,
    lyric_key: String, // the song the lyrics were last asked for
}

fn run(hwnd: isize, song_msg: u32, pin_msg: u32) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
    let (tx, rx) = channel::<()>();
    {
        let tx = tx.clone();
        mgr.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            let _ = tx.send(());
            Ok(())
        }))?;
    }
    if let Ok(mut g) = WAKE.lock() {
        *g = Some(tx.clone());
    }
    let mut l = Listener { mgr, tx, hwnd, song_msg, pin_msg, has_art: false, last_key: String::new(), first: true, app: String::new(), sub: None, lyric_key: String::new() };
    let _ = l.update();
    while rx.recv().is_ok() {
        std::thread::sleep(Duration::from_millis(350)); // coalesce bursts
        while rx.try_recv().is_ok() {}
        let _ = l.update();
    }
    Ok(())
}

impl Listener {
    fn update(&mut self) -> windows::core::Result<()> {
        let Ok(ses) = self.mgr.GetCurrentSession() else {
            set_song(None);
            self.last_key.clear();
            self.has_art = false;
            if PLAYING.swap(false, Ordering::Relaxed) {
                self.tell_pin();
            }
            return Ok(());
        };
        let app = ses.SourceAppUserModelId()?.to_string();
        if app != self.app || self.sub.is_none() {
            if let Some((old, t1, t2, t3)) = self.sub.take() {
                let _ = old.RemoveMediaPropertiesChanged(t1);
                let _ = old.RemovePlaybackInfoChanged(t2);
                let _ = old.RemoveTimelinePropertiesChanged(t3);
            }
            let (tx1, tx2) = (self.tx.clone(), self.tx.clone());
            let t1 = ses.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx1.send(());
                Ok(())
            }))?;
            let t2 = ses.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx2.send(());
                Ok(())
            }))?;
            let tx3 = self.tx.clone();
            let t3 = ses.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                if LYRICS_ON.load(Ordering::Relaxed) {
                    let _ = tx3.send(()); // a seek: the lyrics follow
                }
                Ok(())
            }))?;
            self.sub = Some((ses.clone(), t1, t2, t3));
            self.app = app;
        }

        let props = ses.TryGetMediaPropertiesAsync()?.get()?;
        let title = props.Title()?.to_string().trim().to_string();
        let mut artist = props.Artist()?.to_string().trim().to_string();
        if artist.is_empty() {
            artist = props.AlbumArtist()?.to_string().trim().to_string();
        }
        // some players (Apple Music) send "Artist — Album" as the artist
        if let Some((a, _)) = artist.split_once(" — ") {
            artist = a.trim().to_string();
        }
        let playing = ses.GetPlaybackInfo()?.PlaybackStatus()? == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        let was_playing = PLAYING.swap(playing, Ordering::Relaxed);
        let lyrics = LYRICS_ON.load(Ordering::Relaxed) && PIN_ON.load(Ordering::Relaxed);
        let mut duration = None;
        if lyrics {
            if let Ok(tl) = ses.GetTimelineProperties() {
                let secs = |d: windows::Foundation::TimeSpan| d.Duration as f64 / 1e7;
                let mut pos = tl.Position().map_or(0.0, secs);
                if playing {
                    // the report is some time old: carry it on (DateTime and FILETIME both count 100 ns from 1601)
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() / 100) as i128 + 116_444_736_000_000_000;
                    let age = tl.LastUpdatedTime().map_or(0.0, |t| (now - t.UniversalTime as i128) as f64 / 1e7);
                    pos += age.clamp(0.0, 600.0);
                }
                if let Ok(mut g) = TIMELINE.lock() {
                    *g = Some((pos as f32, Instant::now(), playing));
                }
                let len = secs(tl.EndTime().unwrap_or_default()) - secs(tl.StartTime().unwrap_or_default());
                duration = (len > 1.0).then_some(len.round() as u32);
            }
        }
        if title.is_empty() && artist.is_empty() {
            set_song(None);
            self.last_key.clear();
            self.has_art = false;
            self.tell_pin();
            return Ok(());
        }
        let key = format!("{artist}|{title}");
        if lyrics && !title.is_empty() && key != self.lyric_key {
            self.lyric_key = key.clone();
            crate::lyrics::fetch(key.clone(), title.clone(), artist.clone(), duration);
        }
        let changed = key != self.last_key;
        let wants_art = PIN_ON.load(Ordering::Relaxed);
        if !changed && !(wants_art && !self.has_art) {
            if playing != was_playing {
                self.tell_pin(); // only play / pause
            }
            return Ok(());
        }
        self.last_key = key;

        let line = match (artist.is_empty(), title.is_empty()) {
            (false, false) => format!("{title} — {artist}"),
            (false, true) => artist.clone(),
            _ => title.clone(),
        };
        let art = if wants_art { fetch_art(&props) } else { None };
        let key = self.last_key.clone();
        self.has_art = art.is_some();
        let song = if !artist.is_empty() && !title.is_empty() {
            raster(&title, -64, 700).zip(raster(&line, -44, 400)).map(|(big, line)| Song { big, small: raster(&artist, -48, 400), line, art, key })
        } else {
            raster(&line, -64, 700).zip(raster(&line, -44, 400)).map(|(big, line)| Song { big, small: None, line, art, key })
        };
        set_song(song.map(Arc::new));
        self.tell_pin();
        if !changed {
            return Ok(()); // the same song, now with its cover: nothing to announce
        }

        if playing && !self.first {
            unsafe {
                let _ = PostMessageW(HWND(self.hwnd as _), self.song_msg, WPARAM(0), LPARAM(0));
            }
        }
        self.first = false;
        Ok(())
    }
}

impl Listener {
    fn tell_pin(&self) {
        if PIN_ON.load(Ordering::Relaxed) {
            unsafe {
                let _ = PostMessageW(HWND(self.hwnd as _), self.pin_msg, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// The cover, cropped to a square and scaled down by the decoder itself (cheap), plus its accent color.
fn fetch_art(props: &GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<Art> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.get().ok()?;
    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
    let (w, h) = (decoder.PixelWidth().ok()?, decoder.PixelHeight().ok()?);
    let side = w.min(h);
    if side == 0 {
        return None;
    }
    let transform = BitmapTransform::new().ok()?;
    // scaled so the shorter side is ART_PX, then the middle square is cut out (the bounds apply after scaling)
    let n = ART_PX as u32;
    let (sw, sh) = ((w * n / side).max(n), (h * n / side).max(n));
    transform.SetScaledWidth(sw).ok()?;
    transform.SetScaledHeight(sh).ok()?;
    transform.SetBounds(BitmapBounds { X: (sw - n) / 2, Y: (sh - n) / 2, Width: n, Height: n }).ok()?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant).ok()?;
    let data = decoder
        .GetPixelDataTransformedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied, &transform, ExifOrientationMode::IgnoreExifOrientation, ColorManagementMode::DoNotColorManage)
        .ok()?
        .get()
        .ok()?;
    let bytes = data.DetachPixelData().ok()?;
    if bytes.len() < ART_PX * ART_PX * 4 {
        return None;
    }
    let px: Vec<u32> = bytes.chunks_exact(4).take(ART_PX * ART_PX).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let accent = accent_of(&px);
    Some(Art { px, accent })
}

/// A color that stands for the cover: the average of its saturated, lit parts, pushed bright enough for text.
fn accent_of(px: &[u32]) -> [f32; 3] {
    let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
    for p in px.iter().step_by(3) {
        let (r, g, b) = (((p >> 16) & 0xFF) as f32 / 255.0, ((p >> 8) & 0xFF) as f32 / 255.0, (p & 0xFF) as f32 / 255.0);
        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if hi > 0.0 { (hi - lo) / hi } else { 0.0 };
        let w = (sat + 0.05) * hi * hi; // colorful and bright pixels count most
        sum = [sum[0] + r * w, sum[1] + g * w, sum[2] + b * w];
        weight += w;
    }
    if weight <= 0.0 {
        return [235.0; 3];
    }
    let c = [sum[0] / weight, sum[1] / weight, sum[2] / weight];
    let (hi, lo) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if hi - lo < 0.06 {
        return [235.0; 3]; // a gray cover: plain white text
    }
    // stretch to full brightness, keep the hue, soften a little toward white so it stays readable
    let k = 0.92 / hi;
    let lift = 0.18;
    [0, 1, 2].map(|i| ((c[i] * k) * (1.0 - lift) + lift).clamp(0.0, 1.0) * 255.0)
}

/// cmd: 1 = previous, 2 = next, 3 = play/pause (then posts `msg` so the icon can follow the new state).
pub fn command(cmd: u32, hwnd: isize, msg: u32) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let _ = (|| -> windows::core::Result<()> {
            let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
            let ses = mgr.GetCurrentSession()?;
            match cmd {
                1 => {
                    ses.TrySkipPreviousAsync()?.get()?;
                }
                2 => {
                    ses.TrySkipNextAsync()?.get()?;
                }
                _ => {
                    ses.TryTogglePlayPauseAsync()?.get()?;
                    std::thread::sleep(Duration::from_millis(200));
                    let playing = ses.GetPlaybackInfo()?.PlaybackStatus()?
                        == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
                    PLAY_EVENT.store(if playing { 1 } else { 2 }, Ordering::SeqCst);
                }
            }
            Ok(())
        })();
        unsafe {
            CoUninitialize();
            if cmd == 3 {
                let _ = PostMessageW(HWND(hwnd as _), msg, WPARAM(0), LPARAM(0));
            }
        }
    });
}
