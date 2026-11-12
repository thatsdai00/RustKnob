// The capsule: a black, Dynamic-Island-like shape at the top center of the screen. One window,
// one shape that springs (bouncily) between contents: the volume bar, song / push notifications,
// the settings panel, and the round knob. Turning the wheel makes the pill drop its content,
// grow into a disc (still at the top of the screen) and bring in the knob's elements.
// Everything runs on the main thread; hooks and workers only post messages. This window's proc
// is also the app's message hub.

use std::sync::atomic::{AtomicPtr, Ordering::*};
use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::mouse;
use crate::dragsrc;
use crate::pace;
use crate::spectrum;
use crate::snip;
use crate::audio::{self, Clicker};
use crate::config::{Config, NOTIFY_VOLUMES, STEPS};
use crate::gfx::Icon;
use crate::hook::*;
use crate::media::{self, Song};
use crate::render::{self, HistoryView, Kind, Layer, Scene, SettingsView, SetupView, UploadPhase, UploadView, ICON_SECS};
use crate::surface::Surface;
use crate::text::{self, TextBmp};
use crate::i18n::{self, Lang, Strings};
use crate::theme::{self, Palette};
use crate::{autostart, clipboard, device, history, install, ntfy, tray, ui, upload};

const WM_APP_WARM: u32 = WM_APP + 25;
const WM_APP_MOUSE: u32 = WM_APP + 41; // razer.rs: wparam mouse::EV_*
const MOUSE_DPI_MS: u32 = 1700; // the DPI capsule stays this long after the last change
const MOUSE_LINK_MS: u32 = 3800; // "connected"
const MOUSE_LOW_MS: u32 = 5000; // battery low (the critical one stays)
const WM_APP_UPDATE: u32 = WM_APP + 28; // update.rs: a newer release is downloaded and checked
const T_UPDATE: usize = 8; // look for a new release
const T_UPDATE_APPLY: usize = 9; // install a downloaded one once nothing is going on
const UPDATE_EVERY_MS: u32 = 6 * 60 * 60 * 1000;
const UPDATE_FIRST_MS: u32 = 45_000;
/// Set when this start follows an automatic update: the capsule says so once.
pub static JUST_UPDATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const WM_APP_PIN: u32 = WM_APP + 26; // what the pinned capsule shows (song, cover, playing) changed
const T_PIN_POLL: usize = 7; // is a full-screen app in front? (only while the media is pinned and playing)
const PIN_POLL_MS: u32 = 2500;
const PIN_PAUSE_MS: u32 = 2500; // how long the pinned capsule stays after the music pauses
/// How far from the pinned capsule (logical units: scales with the screen) it starts to fade.
const PIN_FADE_REACH: f32 = 150.0;
const PIN_FADE_RATE: f32 = 9.0; // how quickly it follows (per second)
const T_LYRIC: usize = 11; // which lyric line is it? (only while the pinned capsule plays with lyrics on)
const LYRIC_MS: u32 = 100;
const LYRIC_LEAD: f32 = 0.4; // a line shows this long before it is sung (the sung part is not early)
const LYRIC_SWAP_SECS: f32 = 0.4; // one lyric line giving way to the next
const SONG_SWAP_SECS: f32 = 0.55; // the pinned capsule changing from one song to the next
const VIZ_FRAME_MS: u32 = 33; // the visualizer moves at about 30 frames per second
const T_HIDE: usize = 2;
const T_DRAG: usize = 5; // waits for OLE to take over the mouse, which tells a real drag from a plain press-and-move
const T_DROP_END: usize = 6; // closes the drop capsule a moment after the button is released
const T_TRAY: usize = 3; // retries adding the tray icon while the taskbar is not up yet

// shape springs: (stiffness, damping). Underdamped on purpose: the shape overshoots and settles.
const SPRING_W: (f32, f32) = (420.0, 22.5); // width and corner radius
const SPRING_H: (f32, f32) = (380.0, 21.0); // height lags a little: the pill stretches into the disc
const SPRING_CLOSE: (f32, f32) = (380.0, 38.0);
const SPRING_FADE: (f32, f32) = (500.0, 45.0);
// the shape counts as arrived when it is this close (logical units, under half a pixel) and this slow
const SETTLE_DIST: f32 = 0.3;
const SETTLE_SPEED: f32 = 3.0;
const SPRING_HOVER: (f32, f32) = (320.0, 15.0); // bouncy
const HOVER_SCALE: f32 = 1.1; // the capsule / knob grows this much under the pointer
const DROP_SAFETY_MS: u32 = 30_000; // the drop capsule never stays longer than this
const HIDDEN_BELOW: f32 = 0.02; // fade at which the closing shape disappears

const HIDE_SNIP_MS: u32 = 2500;
const HIDE_VOLUME_KEY_MS: u32 = 1500;
const HIDE_SONG_MS: u32 = 3500;
const HIDE_DEVICE_MS: u32 = 2500;
const HIDE_NTFY_MS: u32 = 5000;
const HIDE_MS_PER_PX: f32 = 0.5; // extra hide delay per pixel between cursor and capsule
const HIDE_DISTANCE_MAX_MS: f32 = 2000.0;
/// The mouse side buttons (previous / next song) also work this far (logical units) around the capsule.
const NEAR_PAD: f32 = 120.0;
const HIDE_EXTRA_MS: u32 = 2000; // extra time after a media button press
const MARQUEE_FRAME_MS: u32 = 16; // scrolling text moves at about 60 frames per second
const LOW_POWER_FRAME_MS: u32 = 33;
const HIDE_UPLOAD_DONE_MS: u32 = 15000; // the link stays up long enough to read it
const HIDE_UPLOAD_FAILED_MS: u32 = 20000;
const MORPH_DELAY: f32 = 0.07; // the shape starts moving while the old elements are still gathering
const EXIT_SECS: f32 = 0.2; // old elements bend, gather and vanish
const ENTER_SECS: f32 = 0.3; // new elements burst out from the center
const ENTER_AT: f32 = 0.4; // morph progress at which they start
const CLOSE_OUT_SPEED: f32 = 7.0; // elements fade out before the shape shrinks (per second)

type Rect = (usize, usize, usize, usize);

/// What the upload capsule shows (kept between frames).
struct UploadUi {
    phase: UploadPhase,
    progress: f32, // smoothed 0..1
    last_sent: u64,
    speed: f32, // bytes per second, smoothed
    clock: f32, // seconds since the speed text was refreshed
    name_key: String,
    name: Option<TextBmp>,
    info: Option<TextBmp>,
    msg: Option<TextBmp>,
    line: Option<TextBmp>,
}

impl UploadUi {
    fn new() -> UploadUi {
        UploadUi { phase: UploadPhase::Uploading, progress: 0.0, last_sent: 0, speed: 0.0, clock: 0.0, name_key: String::new(), name: None, info: None, msg: None, line: None }
    }
}

/// First-run setup choices.
struct SetupState {
    update: bool, // an older build is installed: the button updates it
    autostart: bool,
    auto_update: bool,
    toggles: [f32; 2], // animated switches
    hover: Option<usize>,
}

/// Work a panel needs but its first frame does not: done at the start of the next tick.
#[derive(Clone, Copy, PartialEq)]
enum Deferred {
    Nothing,
    Settings,
    History,
}

struct State {
    cfg: Config,
    lang: Lang,
    pal: Palette,
    surf: Surface,
    visible: bool,
    unit: f32,       // device px per logical unit (DPI scale, panels larger on big screens)
    unit_goal: f32,  // where `unit` is heading (it eases when the content changes between a panel and the rest)
    base: f32,       // DPI scale alone
    boost: f32,      // extra scale the panels get on this screen
    win: (i32, i32), // window top-left, screen px

    // shape: current value + velocity + target, for size and corner radius
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
    tw: f32,
    th: f32,
    rad: f32,
    vrad: f32,
    trad: f32,
    fade_in: f32, // overall shape fade, springs 0 -> 1 on open
    fade_v: f32,
    closing: bool,
    close_phase: u8, // 1 = contents fade out, 2 = the shape shrinks to the dot and vanishes

    // content: current kind fading in, previous kind fading out, how far the morph has got
    kind: Kind,
    content: f32,
    prev: Option<(Kind, f32)>,
    delay: f32,   // seconds before the shape starts a morph (old elements fading)
    queued: bool, // targets waiting for the delay
    hint: u8,     // morph kind for the next targets: 1 into the knob, 2 out of it
    staged: Option<(bool, (f32, f32, f32))>, // two-step morph: (into knob?, final width / height / radius)
    from: (f32, f32),
    reveal: f32,
    progress: f32, // how far the shape is into its morph, 0..1
    full_w: f32,
    manual: bool, // opened by the user: the wheel and mouse buttons are captured
    prev_rect: Option<Rect>,
    marq: f32,

    // audio
    ep: Option<IAudioEndpointVolume>,
    vol: f32,
    disp: f32, // smoothed volume that is drawn
    muted: bool,
    clicker: Option<Clicker>,

    // song / push notification / transport
    song: Option<Arc<Song>>,
    notice: Option<Arc<Song>>, // a push notification takes the song capsule's place
    device_line: Option<TextBmp>, // shown under the volume bar after a device change
    icon: Option<(Icon, f32)>,

    // settings
    labels: Vec<TextBmp>,
    values: Vec<(TextBmp, bool)>, // topic, server, token, sound; `true` = dim placeholder
    editing: Option<usize>,       // settings row whose text field takes typed characters
    edit_backup: String,          // the field's text when editing started (Esc restores it)

    // uploads
    up: UploadUi,

    // link history
    hist: Vec<history::Entry>,
    hist_scroll: usize,
    hist_rows: Vec<(TextBmp, TextBmp)>, // name, "date · link" of the visible entries
    hist_title: Option<TextBmp>,
    drop_hint: Option<TextBmp>, // "Drop to upload"
    deferred: Deferred,
    dirty: Option<(usize, usize, usize, usize)>, // what the last draw touched (device pixels), when it was only part
    pin_fade_goal: f32, // 0 = the pointer is far, 1 = it is over the pinned capsule
    pin_fade: f32,      // eased toward the goal: the capsule's opacity is 1 - this
    pending_update: Option<(String, std::path::PathBuf)>, // a downloaded release waiting for a quiet moment
    update_asked: bool, // the user pressed "check for updates": the answer shows in that row, a release installs at once
    song_prev: Option<Arc<Song>>, // the song the pinned capsule is changing away from
    song_swap: f32,               // 0..1 through that change
    lyric: Option<Arc<TextBmp>>,     // the current lyric line (None: the title shows)
    lyric_old: Option<Arc<TextBmp>>, // the line it is replacing, while that animates
    lyric_swap: f32,                 // 0..1 through that change
    lyric_idx: Option<usize>,        // index of the current line in the song's lyrics
    lyric_pts: Vec<(f32, f32)>,      // its words as (time, x) pairs: how far it has been sung at any moment (empty: line-timed)
    lyric_fill: Option<f32>,         // ... and the x it is at now
    lyric_drawn: Option<f32>,        // the x that is on screen (only the stretch between the two needs drawing)
    lyric_key: String,               // the song those belong to
    pin_tall: bool,                  // the pinned capsule is the tall one (this song has lyrics)
    pin_suspended: bool,                 // a full-screen app is in front: the pinned capsule steps aside
    viz: [f32; 5],                       // bar heights 0..1
    steady_drawn: bool,                  // the last frame was a full draw of a settled capsule
    notice_icon: u8,            // 0 bell (push), 1 clipboard (screenshot copied), 2 check (pasted)
    drag_polls: u8,              // checks left after a drag started
    hist_empty: Option<TextBmp>,
    hist_status: Option<(TextBmp, f32)>, // "Copied" and its remaining seconds
    hist_hover: Option<usize>,
    hist_flash: Option<(usize, f32)>,

    // first-run setup
    setup: Option<SetupState>,
    setup_texts: Vec<TextBmp>,
    setup_values: Vec<TextBmp>,
    toggles: [f32; ui::TOGGLES],
    hover: Option<usize>,
    pointer_in: bool, // the pointer is over the capsule / knob
    hs: f32,          // hover scale: springs between 1.0 and HOVER_SCALE
    hv: f32,
    hide_armed: bool, // an auto-hide timer is running
    autostart: bool,

    saved_fg: HWND,
    last_tick: Option<Instant>,
    _devices: Option<device::Subscription>,
    mouse: MouseUi,
}

static STATE: AtomicPtr<State> = AtomicPtr::new(std::ptr::null_mut());

pub unsafe fn init(cfg: Config, hwnd: HWND) {
    VOLUME_KEYS.store(cfg.volume_keys, Relaxed);
    SNIP_KEY.store(cfg.screenshot, Relaxed);
    let state = State {
        lang: i18n::resolve(&cfg.language),
        pal: theme::resolve(&cfg.theme),
        cfg,
        surf: Surface::new(),
        visible: false,
        unit: 1.0,
        unit_goal: 1.0,
        base: 1.0,
        boost: 1.0,
        win: (0, 0),
        w: ui::SMALL.0,
        h: ui::SMALL.1,
        vw: 0.0,
        vh: 0.0,
        tw: ui::SMALL.0,
        th: ui::SMALL.1,
        rad: ui::SMALL.1 / 2.0,
        vrad: 0.0,
        trad: ui::SMALL.1 / 2.0,
        fade_in: 0.0,
        fade_v: 0.0,
        closing: false,
        close_phase: 0,
        kind: Kind::Volume,
        content: 0.0,
        prev: None,
        delay: 0.0,
        queued: false,
        hint: 0,
        staged: None,
        from: ui::SMALL,
        reveal: 0.0,
        progress: 0.0,
        full_w: ui::VOLUME_W,
        manual: false,
        prev_rect: None,
        marq: 0.0,
        ep: None,
        vol: 0.0,
        disp: 0.0,
        muted: false,
        clicker: None,
        song: None,
        notice: None,
        device_line: None,
        icon: None,
        labels: Vec::new(),
        values: Vec::new(),
        editing: None,
        edit_backup: String::new(),
        up: UploadUi::new(),
        hist: Vec::new(),
        hist_scroll: 0,
        hist_rows: Vec::new(),
        hist_title: None,
        drop_hint: None,
        deferred: Deferred::Nothing,
        dirty: None,
        pin_fade_goal: 0.0,
        pin_fade: 0.0,
        pending_update: None,
        update_asked: false,
        song_prev: None,
        song_swap: 1.0,
        lyric: None,
        lyric_old: None,
        lyric_swap: 1.0,
        lyric_idx: None,
        lyric_pts: Vec::new(),
        lyric_fill: None,
        lyric_drawn: None,
        lyric_key: String::new(),
        pin_tall: false,
        pin_suspended: false,
        viz: [0.0; 5],
        steady_drawn: false,
        notice_icon: 0,
        drag_polls: 0,
        hist_empty: None,
        hist_status: None,
        hist_hover: None,
        hist_flash: None,
        setup: None,
        setup_texts: Vec::new(),
        setup_values: Vec::new(),
        toggles: [0.0; ui::TOGGLES],
        hover: None,
        pointer_in: false,
        hs: 1.0,
        hv: 0.0,
        hide_armed: false,
        autostart: false,
        saved_fg: HWND::default(),
        last_tick: None,
        _devices: device::watch(hwnd.0 as isize, WM_APP_DEVICE),
        mouse: MouseUi::default(),
    };
    STATE.store(Box::into_raw(Box::new(state)), SeqCst);
    media::set_pin(st().cfg.pin_media);
    media::set_lyrics(st().cfg.pin_lyrics);
    media::start(hwnd.0 as isize, WM_APP_SONG, WM_APP_PIN);
    mouse::set_enabled(st().cfg.mouse, hwnd.0 as isize, WM_APP_MOUSE);
    ntfy::start(&st().cfg, hwnd.0 as isize, WM_APP_NTFY);
    if !tray::add(hwnd) {
        SetTimer(hwnd, T_TRAY, 2000, None);
    }
    let _ = PostMessageW(hwnd, WM_APP_WARM, WPARAM(0), LPARAM(0)); // once the message loop is running
}

/// What the first capsule would otherwise do while you wait for it: ask Windows for the audio endpoint and
/// make the fonts. Runs once, right after start-up.
unsafe fn warm_up(hwnd: HWND) {
    let s = st();
    read_volume(s);
    pin_refresh(hwnd);
    if s.cfg.auto_update && install::running_installed() {
        SetTimer(hwnd, T_UPDATE, UPDATE_FIRST_MS, None);
    }
    if JUST_UPDATED.swap(false, Relaxed) {
        announce_update(hwnd);
    }
    for (height, weight) in [(-64, 700), (-56, 700), (-52, 700), (-52, 400), (-48, 400), (-44, 400), (-42, 400)] {
        let _ = text::raster("0", height, weight);
    }
    trim_memory();
}

/// Only the main thread touches the state, and never re-entrantly.
fn st() -> &'static mut State {
    unsafe { &mut *STATE.load(Relaxed) }
}

fn settings_open() -> bool {
    let s = st();
    s.visible && !s.closing && s.kind == Kind::Settings
}

/// A clickable panel is up (settings, link history or setup).
fn panel_open() -> bool {
    let s = st();
    s.live() && matches!(s.kind, Kind::Settings | Kind::History | Kind::Setup)
}

impl State {
    fn strings(&self) -> &'static Strings {
        i18n::strings(self.lang)
    }

    /// Re-reads language and theme ("auto" follows Windows, which may have changed).
    fn refresh_look(&mut self) {
        self.lang = i18n::resolve(&self.cfg.language);
        self.pal = theme::resolve(&self.cfg.theme);
    }

    fn live(&self) -> bool {
        self.visible && !self.closing
    }

    fn uploading(&self) -> bool {
        self.visible && self.kind == Kind::Upload && self.up.phase == UploadPhase::Uploading
    }

    /// The upload capsule is up (working, or showing its result).
    fn upload_shown(&self) -> bool {
        self.live() && self.kind == Kind::Upload
    }

    /// Content that stays on show steps aside for the pointer: the pinned media, the red battery warning.
    fn fades(&self) -> bool {
        self.kind == Kind::Pinned || self.kind == Kind::MouseBattery && self.mouse.crit
    }

    /// Is the media pinned right now: the setting is on, something plays, and nothing in front asks us to stay out.
    fn pin_wanted(&self) -> bool {
        self.cfg.pin_media && !self.pin_suspended && media::playing() && media::song().is_some()
    }

    /// Opened by the user and showing a volume UI (bar or knob).
    fn volume_ui(&self) -> bool {
        self.live() && self.manual && matches!(self.kind, Kind::Volume | Kind::Knob)
    }

    fn want(&self) -> f32 {
        if self.muted { 0.0 } else { self.vol }
    }

    /// Song (or push notification) shown by the song capsule.
    fn shown_song(&self) -> Option<&Song> {
        self.notice.as_deref().or(self.song.as_deref())
    }

    /// The line under the volume bar: the device name after a device change, else the playing song.
    fn sub(&self) -> Option<&TextBmp> {
        self.device_line.as_ref().or_else(|| self.song.as_deref().map(|s| &s.line))
    }

    fn target_size(&self) -> (f32, f32) {
        match self.kind {
            Kind::Volume => (ui::VOLUME_W, if self.sub().is_some() { ui::VOLUME_H_SUB } else { ui::VOLUME_H }),
            Kind::Song => ui::SONG,
            Kind::Settings => ui::SETTINGS,
            Kind::Knob => ui::KNOB,
            Kind::Upload => ui::UPLOAD,
            Kind::Drop => ui::DROP,
            Kind::MouseDpi => ui::mouse_size(ui::MOUSE_DPI, self.mouse.others.len()),
            Kind::MouseBattery => ui::mouse_size(ui::MOUSE_BATTERY, self.mouse.others.len()),
            Kind::MouseLink => ui::mouse_size(ui::MOUSE_LINK, self.mouse.others.len()),
            Kind::Pinned => if self.pin_tall { ui::PINNED_TALL } else { ui::PINNED },
            Kind::History => (ui::HISTORY.0, ui::PAD * 2.0 + ui::HEAD_H + self.hist.len().clamp(2, ui::HIST_ROWS) as f32 * ui::HIST_ROW_H),
            Kind::Setup => ui::SETUP,
        }
    }

    /// Sets the shape targets for the current kind (not while closing). Morphs into or out of the
    /// knob run in two steps: into it the pill gets taller first and then rounds into the disc; out
    /// of it the disc loses its roundness first and then gets shorter.
    fn apply_targets(&mut self) {
        self.unit_goal = self.base * kind_scale(self, self.kind);
        if self.delay > 0.0 {
            self.queued = true; // wait for the old elements to fade
            return;
        }
        let (fw, fh) = self.target_size();
        let fr = match self.kind {
            Kind::Knob => ui::KNOB_RADIUS,
            _ => (fh / 2.0).min(ui::CAPSULE_RADIUS),
        };
        self.full_w = fw;
        if let Some((_, goal)) = self.staged.as_mut() {
            *goal = (fw, fh, fr); // already mid-morph: only the destination changes
            return;
        }
        match std::mem::take(&mut self.hint) {
            1 => {
                self.th = fh;
                self.trad = ui::CAPSULE_RADIUS;
                self.staged = Some((true, (fw, fh, fr)));
            }
            2 => {
                (self.tw, self.trad) = (fw, fr);
                self.staged = Some((false, (fw, fh, fr)));
            }
            _ => (self.tw, self.th, self.trad) = (fw, fh, fr),
        }
    }

    /// Switches content: the old one starts fading out, the new one fades in once the shape arrives.
    fn set_kind(&mut self, kind: Kind) {
        if self.kind == kind {
            return;
        }
        if self.content > 0.05 {
            self.prev = Some((self.kind, 0.0));
            self.delay = MORPH_DELAY;
        }
        self.hint = if kind == Kind::Knob { 1 } else if self.kind == Kind::Knob { 2 } else { 0 };
        self.kind = kind;
        self.content = 0.0;
        self.from = (self.w, self.h);
        self.reveal = 0.0;
    }

    /// Does any visible text row need to scroll?
    fn scrolls(&self) -> bool {
        let long = |t: &TextBmp, view: f32| t.size().0 > view;
        match self.kind {
            Kind::Volume => self.sub().is_some_and(|t| long(t, ui::VOLUME_W - 40.0)),
            Kind::Song => self.shown_song().is_some_and(|s| {
                let view = ui::SONG.0 - 70.0;
                long(&s.big, view) || s.small.as_ref().is_some_and(|t| long(t, view))
            }),
            Kind::Knob => self.song.as_ref().is_some_and(|s| long(&s.big, 116.0) || s.small.as_ref().is_some_and(|t| long(t, 84.0))),
            Kind::Upload => {
                self.up.name.as_ref().is_some_and(|t| long(t, ui::UPLOAD.0 - 152.0)) || self.up.line.as_ref().is_some_and(|t| long(t, ui::UPLOAD.0 - 70.0))
            }
            Kind::Pinned => self.song.as_ref().is_some_and(|s| long(&s.big, self.tw - 102.0) || s.small.as_ref().is_some_and(|t| long(t, self.tw - 102.0))),
            Kind::Settings | Kind::History | Kind::Setup | Kind::Drop | Kind::MouseDpi | Kind::MouseBattery | Kind::MouseLink => false,
        }
    }

    fn toggle_targets(&self) -> [f32; ui::TOGGLES] {
        let c = &self.cfg;
        [c.click_sound, c.notify_song, c.notify_device, c.volume_keys, c.low_power, c.upload_sound, c.screenshot, c.pin_media, c.pin_fullscreen, c.pin_lyrics, c.lyric_words, c.mouse, c.auto_update, self.autostart].map(|b| f32::from(u8::from(b)))
    }

    /// Screen-space rectangle of the shape at its target (for the mouse hook).
    fn area(&self) -> (i32, i32, i32, i32) {
        let (px, u) = (self.surf.pw as f32, self.unit_goal);
        let left = self.win.0 as f32 + (px - self.tw * u) / 2.0;
        let top = self.win.1 as f32 + ui::TOP * u;
        (left as i32, top as i32, (left + self.tw * u) as i32, (top + self.th * u) as i32)
    }

    fn sync_hooks(&self) {
        let ui_open = self.volume_ui();
        CAPTURE_WHEEL.store(ui_open || (self.live() && self.kind == Kind::History), Relaxed);
        CAPTURE_BUTTONS.store(ui_open, Relaxed);
        SETTINGS_OPEN.store(self.live() && matches!(self.kind, Kind::Settings | Kind::History | Kind::Setup), Relaxed);
        set_hover_track(self.live() && matches!(self.kind, Kind::Volume | Kind::Song | Kind::Knob | Kind::Upload));
        // the pinned capsule steps aside for the pointer (the reach scales with the screen)
        set_fade_track(self.live() && self.fades(), (PIN_FADE_REACH * self.unit_goal) as i32);
        let (l, t, r, b) = self.area();
        set_area(l, t, r, b, self.kind == Kind::Knob, (NEAR_PAD * self.unit_goal) as i32);
    }
}

unsafe fn frames(hwnd: HWND) {
    pace::start(hwnd, if st().cfg.low_power { LOW_POWER_FRAME_MS } else { 0 });
}

/// Hide delay grows with the cursor's distance from the top-center of the screen (the capsule), so a far
/// cursor gets time to travel there.
unsafe fn linger_ms(base: u32) -> u32 {
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let dx = (cur.x - GetSystemMetrics(SM_CXSCREEN) / 2) as f32;
    let dy = cur.y as f32;
    base + ((dx * dx + dy * dy).sqrt() * HIDE_MS_PER_PX).min(HIDE_DISTANCE_MAX_MS) as u32
}

unsafe fn hide_after(hwnd: HWND, ms: Option<u32>) {
    st().hide_armed = ms.is_some();
    match ms {
        Some(ms) => {
            SetTimer(hwnd, T_HIDE, ms, None);
        }
        None => {
            let _ = KillTimer(hwnd, T_HIDE);
        }
    }
}

fn read_volume(s: &mut State) {
    // the kept endpoint may have gone stale (device unplugged): ask again once
    for _ in 0..2 {
        if s.ep.is_none() {
            s.ep = unsafe { audio::default_endpoint() };
        }
        let Some(ep) = &s.ep else { return };
        unsafe {
            if let Ok(v) = ep.GetMasterVolumeLevelScalar() {
                s.vol = v;
                s.muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                return;
            }
        }
        s.ep = None;
    }
}

/// Settings, history and setup are dense, so they are drawn larger than the volume capsule and knob.
fn kind_scale(s: &State, kind: Kind) -> f32 {
    if matches!(kind, Kind::Settings | Kind::History | Kind::Setup) { s.boost } else { 1.0 }
}

// ---- lifecycle -------------------------------------------------------------

/// Brings the shape up (or morphs the visible one) to show `kind`.
/// `hide` is the auto-hide delay; None keeps it open (settings).
unsafe fn open(hwnd: HWND, kind: Kind, manual: bool, hide: Option<u32>) {
    let s = st();
    if s.setup.is_some() && kind != Kind::Setup {
        return;
    }
    let fresh = !s.visible;
    if fresh {
        s.saved_fg = GetForegroundWindow();

        // top center of the monitor under the cursor
        let mut cur = POINT::default();
        let _ = GetCursorPos(&mut cur);
        let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        s.base = dpi_x as f32 / 96.0;
        // panels grow with the screen: 1.0 up to 1080 logical px tall, 1.33 at 1440, at most 1.6
        s.boost = ((mi.rcMonitor.bottom - mi.rcMonitor.top) as f32 / s.base / 1080.0).clamp(1.0, 1.6);
        s.unit_goal = s.base * kind_scale(s, kind);
        s.unit = s.unit_goal;
        let (pw, ph) = ((ui::WIN_W * s.base * s.boost).round() as usize, (ui::WIN_H * s.base * s.boost).round() as usize);
        s.surf.ensure(pw, ph);
        let r = mi.rcMonitor;
        s.win = (r.left + ((r.right - r.left) - pw as i32) / 2, r.top);

        (s.w, s.h, s.vw, s.vh) = (ui::SMALL.0, ui::SMALL.1, 0.0, 0.0);
        (s.rad, s.vrad) = (ui::SMALL.1 / 2.0, 0.0);
        (s.fade_in, s.fade_v, s.content, s.prev, s.marq, s.prev_rect, s.icon) = (0.0, 0.0, 0.0, None, 0.0, None, None);
        s.from = ui::SMALL;
        s.reveal = 0.0;
        (s.delay, s.queued, s.hint, s.staged) = (0.0, false, 0, None);
        (s.pin_fade, s.pin_fade_goal) = (0.0, 0.0);
        s.refresh_look();
        s.song = media::song();
        read_volume(s);
        s.disp = s.want();
        crate::efficiency_mode(false); // full speed while it is on screen
    }
    if fresh {
        s.kind = kind;
    } else {
        s.set_kind(kind);
    }
    if kind == Kind::Settings {
        prepare_settings(s);
    }
    s.manual = manual;
    s.closing = false;
    s.close_phase = 0;
    click_through(hwnd, kind != Kind::Drop);
    s.apply_targets();

    if fresh {
        s.last_tick = Some(Instant::now());
        draw_and_present(hwnd);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, s.win.0, s.win.1, s.surf.pw as i32, s.surf.ph as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        s.visible = true;
    }
    s.sync_hooks();
    s.last_tick = s.last_tick.or(Some(Instant::now()));
    frames(hwnd);
    hide_after(hwnd, hide);
}

unsafe fn begin_close(hwnd: HWND) {
    let s = st();
    if !s.visible || s.closing {
        return;
    }
    end_edit(hwnd, true);
    // a nearly empty mouse battery stays on show: whatever else was up gives way to its warning
    if s.mouse.crit && s.cfg.mouse {
        if s.kind == Kind::MouseBattery {
            return; // it stays until the mouse charges (mouse_event clears crit first)
        }
        mouse_warning(s, true);
        s.set_kind(Kind::MouseBattery);
        s.manual = false;
        s.apply_targets();
        s.sync_hooks();
        hide_after(hwnd, None);
        s.last_tick = Some(Instant::now());
        frames(hwnd);
        return;
    }
    if std::mem::take(&mut s.mouse.low_pending) && s.cfg.mouse && s.kind == Kind::MouseLink {
        mouse_warning(s, false);
        s.set_kind(Kind::MouseBattery);
        s.apply_targets();
        s.sync_hooks();
        hide_after(hwnd, Some(MOUSE_LOW_MS));
        s.last_tick = Some(Instant::now());
        frames(hwnd);
        return;
    }
    // with the media pinned, whatever was on show gives way to it instead of shrinking to the dot
    if s.kind != Kind::Pinned && s.pin_wanted() {
        s.set_kind(Kind::Pinned);
        (s.manual, s.song, s.marq) = (false, media::song(), 0.0);
        s.apply_targets();
        s.sync_hooks();
        hide_after(hwnd, None);
        s.last_tick = Some(Instant::now());
        frames(hwnd);
        return;
    }
    // first the elements fade out, then the shape shrinks back to the dot (see tick)
    s.closing = true;
    s.close_phase = 1;
    (s.delay, s.queued, s.staged) = (0.0, false, None);
    s.sync_hooks();
    hide_after(hwnd, None);
    s.last_tick = Some(Instant::now());
    frames(hwnd);
}

/// The capsule lets mouse input through to what is behind it, except while it waits for a dropped file.
unsafe fn click_through(hwnd: HWND, on: bool) {
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let flag = WS_EX_TRANSPARENT.0 as isize;
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, if on { style | flag } else { style & !flag });
}

/// A drag started at `origin`: if it drags files out of Explorer or the desktop (and a program has started
/// an OLE drag, see `drag_poll`), offer the capsule.
unsafe fn drag_started(hwnd: HWND, origin: POINT) {
    let s = st();
    if panel_open() || s.setup.is_some() || !dragsrc::starts_on_files(origin) {
        return;
    }
    s.drag_polls = 15;
    SetTimer(hwnd, T_DRAG, 40, None);
}

/// While a program runs an OLE drag (files, text...), OLE holds the mouse capture in its hidden clipboard window.
unsafe fn ole_drag_running() -> bool {
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    if GetGUIThreadInfo(0, &mut info).is_err() || info.hwndCapture.0.is_null() {
        return false;
    }
    let mut class = [0u16; 32];
    let len = GetClassNameW(info.hwndCapture, &mut class) as usize;
    String::from_utf16_lossy(&class[..len]) == "CLIPBRDWNDCLASS"
}

unsafe fn drag_poll(hwnd: HWND) {
    let s = st();
    s.drag_polls = s.drag_polls.saturating_sub(1);
    if ole_drag_running() {
        let _ = KillTimer(hwnd, T_DRAG);
        if !panel_open() && s.setup.is_none() {
            s.drop_hint = text::raster(s.strings().drop_hint, -56, 700);
            open(hwnd, Kind::Drop, false, Some(DROP_SAFETY_MS));
        }
    } else if s.drag_polls == 0 {
        let _ = KillTimer(hwnd, T_DRAG);
    }
}

unsafe fn finish_hide(hwnd: HWND) {
    let s = st();
    pace::stop();
    let _ = KillTimer(hwnd, T_HIDE);
    let _ = ShowWindow(hwnd, SW_HIDE);
    click_through(hwnd, true);
    crate::efficiency_mode(true);
    s.visible = false;
    s.closing = false;
    s.close_phase = 0;
    s.manual = false;
    s.pointer_in = false;
    s.sync_hooks();
    s.surf.free();
    (s.device_line, s.notice, s.labels, s.hover, s.icon, s.prev) = (None, None, Vec::new(), None, None, None); // the audio endpoint stays: asking Windows for it costs a few ms
    (s.values, s.editing) = (Vec::new(), None);
    EDITING.store(false, Relaxed);

    // we never take focus. Only when nothing at all (or this window) has it, give it back to the window that had
    // it: the one you switched to meanwhile must keep it
    let saved = s.saved_fg;
    let cur = GetForegroundWindow();
    if !saved.0.is_null() && (cur.0.is_null() || cur == hwnd) && IsWindow(saved).as_bool() {
        let me = GetCurrentThreadId();
        let other = GetWindowThreadProcessId(cur, None);
        let attach = other != 0 && other != me;
        if attach {
            let _ = AttachThreadInput(me, other, true);
        }
        let _ = SetForegroundWindow(saved);
        if attach {
            let _ = AttachThreadInput(me, other, false);
        }
    }
    trim_memory();
}

pub unsafe fn trim_memory() {
    let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
}

// ---- volume ----------------------------------------------------------------

/// Applies a volume change from the wheel or a key. `delta` is in notches; `toggle_mute` flips mute instead.
unsafe fn change_volume(s: &mut State, delta: f32, toggle_mute: bool) {
    read_volume(s);
    let Some(ep) = &s.ep else { return };
    if toggle_mute {
        s.muted = !s.muted;
        let _ = ep.SetMute(s.muted, std::ptr::null());
        return;
    }
    if s.muted && delta > 0.0 {
        let _ = ep.SetMute(false, std::ptr::null());
        s.muted = false;
    }
    let old = s.vol;
    s.vol = (s.vol + delta * s.cfg.step_fraction()).clamp(0.0, 1.0);
    let _ = ep.SetMasterVolumeLevelScalar(s.vol, std::ptr::null());
    if s.vol != old && s.cfg.click_sound {
        s.clicker.get_or_insert_with(Clicker::new).play(ep);
    }
}

/// Wheel while the user's capsule is up: adjust the volume, and the bar turns into the knob.
unsafe fn wheel(hwnd: HWND, delta: i32) {
    let s = st();
    if s.live() && s.kind == Kind::History {
        history_scroll(hwnd, delta);
        return;
    }
    if !s.volume_ui() {
        return;
    }
    change_volume(s, delta as f32 / 120.0, false);
    if s.kind != Kind::Knob {
        s.set_kind(Kind::Knob);
        s.apply_targets();
        s.sync_hooks();
    }
    frames(hwnd);
    hide_after(hwnd, Some(linger_ms(s.cfg.hide_ms)));
}

/// A hardware volume key (swallowed by the hook): 1 up, 2 down, 3 mute.
unsafe fn volume_key(hwnd: HWND, which: usize) {
    let s = st();
    change_volume(s, if which == 1 { 1.0 } else { -1.0 }, which == 3);
    if panel_open() || s.upload_shown() {
        return;
    }
    let manual = s.live() && s.manual;
    if s.live() && s.kind == Kind::Knob {
        frames(hwnd);
        hide_after(hwnd, Some(if manual { linger_ms(s.cfg.hide_ms) } else { HIDE_VOLUME_KEY_MS }));
        return;
    }
    if !s.visible {
        s.device_line = None;
    }
    open(hwnd, Kind::Volume, manual, Some(if manual { linger_ms(s.cfg.hide_ms) } else { HIDE_VOLUME_KEY_MS }));
}

/// Hotkey / double middle click: the volume capsule. The hotkey also closes it again.
unsafe fn open_volume(hwnd: HWND, toggle: bool) {
    let s = st();
    if toggle && s.volume_ui() {
        begin_close(hwnd);
        return;
    }
    if !s.visible {
        s.device_line = None;
    }
    open(hwnd, Kind::Volume, true, Some(linger_ms(s.cfg.hide_ms)));
}

// ---- media -----------------------------------------------------------------

/// A mouse button over the capsule / knob: 1 = previous, 2 = next, 3 = play/pause.
unsafe fn media_button(hwnd: HWND, cmd: usize) {
    let s = st();
    if !s.volume_ui() {
        return;
    }
    media::command(cmd as u32, hwnd.0 as isize, WM_APP_PLAYSTATE);
    // skips animate right away; play/pause waits until the new state is known (WM_APP_PLAYSTATE)
    if cmd != 3 {
        start_icon(hwnd, if cmd == 1 { Icon::Prev } else { Icon::Next });
    }
    hide_after(hwnd, Some(linger_ms(s.cfg.hide_ms) + HIDE_EXTRA_MS));
}

unsafe fn start_icon(hwnd: HWND, icon: Icon) {
    st().icon = Some((icon, 0.0));
    frames(hwnd);
}

unsafe fn play_state(hwnd: HWND) {
    if !st().live() {
        return;
    }
    match media::PLAY_EVENT.swap(0, SeqCst) {
        1 => start_icon(hwnd, Icon::Play),
        2 => start_icon(hwnd, Icon::Pause),
        _ => {}
    }
}

// ---- notifications ---------------------------------------------------------

// ---- updates ----------------------------------------------------------------

/// Now and then (and shortly after start): look on GitHub for a newer release.
unsafe fn update_tick(hwnd: HWND) {
    let _ = KillTimer(hwnd, T_UPDATE);
    let s = st();
    if !s.cfg.auto_update || !install::running_installed() {
        return;
    }
    crate::update::check(hwnd.0 as isize, WM_APP_UPDATE);
    SetTimer(hwnd, T_UPDATE, UPDATE_EVERY_MS, None);
}

/// A release was downloaded and matches its checksum: install it when nothing is on screen.
unsafe fn update_ready(hwnd: HWND, outcome: usize) {
    let s = st();
    if let Some(found) = crate::update::take_ready() {
        s.pending_update = Some(found);
    }
    if std::mem::take(&mut s.update_asked) {
        // asked for: say how it went in the settings row, and install a new release right away
        let t = s.strings();
        let text = match outcome {
            crate::update::READY_TO_INSTALL => format!("{} v{}", t.update_installing, s.pending_update.as_ref().map_or("", |p| p.0.as_str())),
            crate::update::UP_TO_DATE => format!("{} · v{}", t.update_latest, crate::update::VERSION),
            _ => t.update_failed.to_string(),
        };
        update_row(hwnd, &text);
        if let Some((_, path)) = s.pending_update.clone() {
            crate::install::launch_with(&path, "--update"); // closes this copy, installs, starts again
        }
        return;
    }
    update_apply(hwnd);
}

/// The "check for updates" row shows `text` (until the panel opens again).
unsafe fn update_row(hwnd: HWND, text: &str) {
    let s = st();
    if let (Some(slot), Some(bmp)) = (s.labels.get_mut(ui::ROW_UPDATE), text::raster(text, -52, 400)) {
        *slot = bmp;
        if settings_open() {
            s.steady_drawn = false;
            frames(hwnd);
        }
    }
}

unsafe fn update_apply(hwnd: HWND) {
    let _ = KillTimer(hwnd, T_UPDATE_APPLY);
    let s = st();
    let Some((_, path)) = s.pending_update.clone() else { return };
    if !s.cfg.auto_update {
        s.pending_update = None;
        return;
    }
    let busy = (s.visible && !(s.kind == Kind::Pinned && s.live())) || upload::status().phase == upload::Phase::Uploading || snip::ACTIVE.load(Relaxed);
    if busy {
        SetTimer(hwnd, T_UPDATE_APPLY, 30_000, None); // not in the middle of something
        return;
    }
    // the new copy closes this one, installs over it and starts again
    crate::install::launch_with(&path, "--update");
}

/// "RustKnob updated · v1.2.3", once, after an automatic update.
unsafe fn announce_update(hwnd: HWND) {
    let s = st();
    let big = text::raster(s.strings().updated_title, -64, 700);
    let small = text::raster(&format!("v{}", crate::update::VERSION), -48, 400);
    let line = text::raster(&format!("{} · v{}", s.strings().updated_title, crate::update::VERSION), -44, 400);
    s.notice = big.zip(line).map(|(big, line)| Arc::new(Song { big, small, line, art: None, key: String::new() }));
    s.notice_icon = 2;
    s.marq = 0.0;
    if !panel_open() {
        open(hwnd, Kind::Song, false, Some(HIDE_NTFY_MS));
    }
}

// ---- pinned media -----------------------------------------------------------

/// The pinned capsule follows the music: it appears when something plays (and the setting is on), shows the
/// song, and leaves a moment after the music stops. Called for every change the media listener reports.
unsafe fn pin_refresh(hwnd: HWND) {
    let s = st();
    if !s.cfg.pin_media {
        spectrum::stop();
        let _ = KillTimer(hwnd, T_PIN_POLL);
        let _ = KillTimer(hwnd, T_LYRIC);
        (s.pin_suspended, s.viz) = (false, [0.0; 5]);
        if s.kind == Kind::Pinned && s.live() {
            begin_close(hwnd);
        }
        return;
    }
    let old = std::mem::replace(&mut s.song, media::song());
    // a different song while the capsule is up: the change animates (the same song with its cover is no change)
    if s.kind == Kind::Pinned && s.live() {
        if let (Some(o), Some(n)) = (&old, &s.song) {
            if o.key != n.key {
                (s.song_prev, s.song_swap) = (old.clone(), 0.0);
            }
        }
    }
    if old.as_ref().map(|o| &o.key) != s.song.as_ref().map(|n| &n.key) {
        lyric_reset(s); // the lines belong to the song that was playing
    }
    if s.cfg.pin_lyrics && media::playing() && s.song.is_some() && s.pin_wanted() {
        SetTimer(hwnd, T_LYRIC, LYRIC_MS, None);
    } else {
        let _ = KillTimer(hwnd, T_LYRIC);
    }
    if media::playing() && s.song.is_some() {
        SetTimer(hwnd, T_PIN_POLL, PIN_POLL_MS, None);
    } else {
        let _ = KillTimer(hwnd, T_PIN_POLL);
    }
    if s.pin_wanted() {
        spectrum::start(); // the visualizer listens only while the capsule is pinned
        if !s.visible || s.closing {
            open(hwnd, Kind::Pinned, false, None);
        } else if s.kind == Kind::Pinned {
            s.steady_drawn = false;
            hide_after(hwnd, None);
            s.apply_targets();
            frames(hwnd);
        }
        // anything else on show goes back to the pinned capsule when it closes (see begin_close)
    } else {
        spectrum::stop();
    }
    if !s.pin_wanted() && s.kind == Kind::Pinned && s.live() {
        if s.pin_suspended {
            begin_close(hwnd); // a full-screen app: out of the way at once
        } else {
            hide_after(hwnd, Some(PIN_PAUSE_MS)); // paused: stays a moment
            frames(hwnd);
        }
    }
}

/// Forgets the lyric line on show (another song, or the lyrics were switched off).
fn lyric_reset(s: &mut State) {
    (s.lyric, s.lyric_old, s.lyric_swap, s.lyric_idx) = (None, None, 1.0, None);
    (s.lyric_pts, s.lyric_fill) = (Vec::new(), None);
    s.pin_tall = false;
    s.lyric_key.clear();
    s.marq = 0.0;
    s.steady_drawn = false;
}

/// Looks at the clock: when the song has moved on to another lyric line, it takes the place of the title.
unsafe fn lyric_tick(hwnd: HWND) {
    let s = st();
    if !s.cfg.pin_lyrics || s.kind != Kind::Pinned || !s.live() || s.song_prev.is_some() {
        return;
    }
    let Some(key) = s.song.as_ref().map(|x| x.key.clone()) else { return };
    let ahead = s.cfg.lyric_ahead_ms as f32 / 1000.0;
    let words_on = s.cfg.lyric_words; // beta: off, the lines are lit whole
    let lyrics = media::position().zip(crate::lyrics::get(&key));
    let shown = lyrics.as_ref().and_then(|(t, l)| l.line_shown(t + ahead, LYRIC_LEAD));
    let idx = shown.map(|(i, _)| i);
    if idx == s.lyric_idx && s.lyric_key == key {
        return; // the same line: nothing to do (this runs ten times a second)
    }
    let (text, pts) = match shown {
        Some((_, line)) => {
            let text = line.text.trim().to_string();
            // where each word starts and ends inside the rasterized line: the sung part grows along it
            let measure = |to: usize| text::measure(&line.text[..to], -56, 700);
            let mut pts: Vec<(f32, f32)> = Vec::new();
            for w in line.words.iter().filter(|_| words_on) {
                let (from, to) = (w.from.min(line.text.len()), w.to.min(line.text.len()));
                if line.text.is_char_boundary(from) && line.text.is_char_boundary(to) && text.len() == line.text.len() {
                    pts.push((w.start, measure(from).max(pts.last().map_or(0.0, |p| p.1))));
                    pts.push((w.end, measure(to)));
                }
            }
            (text, pts)
        }
        None => (String::new(), Vec::new()),
    };
    s.lyric_key = key;
    s.lyric_idx = idx;
    s.lyric_pts = pts;
    let bmp = Some(text).filter(|t| !t.is_empty()).and_then(|t| text::raster(&t, -56, 700)).map(Arc::new);
    s.lyric_fill = (!s.lyric_pts.is_empty() && bmp.is_some()).then_some(0.0);
    if bmp.is_none() && s.lyric.is_none() {
        return; // still the title: nothing to change
    }
    s.lyric_old = s.lyric.take();
    (s.lyric, s.lyric_swap, s.steady_drawn) = (bmp, 0.0, false);
    // the capsule is tall only while there is a line to show: it grows for one and shrinks when there is none
    s.pin_tall = s.lyric.is_some();
    s.apply_targets();
    frames(hwnd);
}

/// A word-timed lyric line at rest: only the stretch where the sung part moved since the last frame is drawn
/// again (the soft edge between lit and dim reaches `render::LYRIC_EDGE` raster px on each side).
unsafe fn lyric_redraw(hwnd: HWND, s: &mut State) {
    let (Some(fill), Some(line)) = (s.lyric_fill, s.lyric.as_ref()) else { return };
    let drawn = s.lyric_drawn.unwrap_or(fill);
    if (fill - drawn).abs() < 0.01 && s.lyric_drawn.is_some() {
        return;
    }
    // the same mapping as render::text_fit: raster px of the line to logical x in the capsule
    let (left, view) = (s.w * 0.05, s.w * 0.9);
    let wl = line.size().0;
    let k = (view / wl).min(1.0);
    let x = |u: f32| left + (view - wl * k) / 2.0 + u / text::TEXT_R * k;
    let (from, to) = (fill.min(drawn) - render::LYRIC_EDGE, fill.max(drawn) + render::LYRIC_EDGE);
    draw_and_present_region(hwnd, (45.0, 77.0), Some((x(from) - 1.0, x(to) + 1.0)));
    s.lyric_drawn = Some(fill);
}

// ---- the Razer mouse -----------------------------------------------------------

/// What the mouse capsules show, and their motions.
#[derive(Default)]
struct MouseUi {
    dpi: Option<TextBmp>,
    dpi_old: Option<TextBmp>, // rolling away
    roll: f32,                // 0..1 through that
    dpi_label: Option<TextBmp>,
    dpi_value: u16,
    stage: f32,
    stage_goal: f32,
    stages: usize,
    title: Option<TextBmp>,
    sub: Option<TextBmp>,
    warn: Option<TextBmp>,
    percent: Option<TextBmp>,
    fill: f32,
    fill_goal: f32,
    level: u8, // 0 fine, 1 low, 2 critical
    seen: u8,  // the level when the battery was last looked at (a warning shows when it is crossed)
    charging: bool,
    intro: f32,
    crit: bool, // the red warning is on: it stays until the mouse charges, sleeps or is gone
    model: Option<mouse::Model>, // the one the name was made for
    low_pending: bool, // a yellow warning waits for the "connected" card to close
    charging_seen: bool, // on the charger when last looked (the card shows when that turns on)
    battery_text: Option<(u8, bool)>, // what `percent` was made from
    others: Vec<render::MouseOther>, // the other plugged-in mice, a row each under the card
    others_from: Vec<mouse::Other>,  // what `others` was made from
}

impl MouseUi {
    fn settled(&self) -> bool {
        self.roll >= 1.0 && self.intro >= 1.0 && self.stage == self.stage_goal && self.fill == self.fill_goal
    }

    fn view(&self) -> render::MouseView<'_> {
        render::MouseView {
            picture: self.model.and_then(mouse::picture),
            others: &self.others,
            dpi: self.dpi.as_ref(),
            dpi_old: self.dpi_old.as_ref(),
            roll: self.roll,
            dpi_label: self.dpi_label.as_ref(),
            stage: self.stage,
            stages: self.stages,
            title: self.title.as_ref(),
            sub: self.sub.as_ref(),
            warn: self.warn.as_ref(),
            percent: self.percent.as_ref(),
            fill: self.fill,
            level: self.level,
            charging: self.charging,
            intro: self.intro,
        }
    }

    fn set_battery(&mut self, t: &i18n::Strings, now: &mouse::Status) {
        self.level = match now.battery {
            Some(p) if !now.charging && p <= 5 => 2,
            Some(p) if !now.charging && p < 25 => 1,
            _ => 0,
        };
        self.charging = now.charging;
        self.fill_goal = now.battery.map_or(0.0, |b| b as f32 / 100.0);
        if now.battery.map(|b| (b, t.percent_first)) != self.battery_text {
            self.battery_text = now.battery.map(|b| (b, t.percent_first));
            self.percent = now.battery.and_then(|b| text::raster(&if t.percent_first { format!("%{b}") } else { format!("{b}%") }, -60, 700));
        }
    }
}

impl MouseUi {
    /// The rows for the other plugged-in mice (made again only when what they say changed).
    fn set_others(&mut self, t: &i18n::Strings, now: Vec<mouse::Other>) {
        if now == self.others_from {
            return;
        }
        self.others = now
            .iter()
            .map(|o| render::MouseOther {
                name: text::raster(o.model.name(), -46, 400),
                percent: o.battery.and_then(|b| text::raster(&if t.percent_first { format!("%{b}") } else { format!("{b}%") }, -46, 700)),
                fill: o.battery.map_or(0.0, |b| b as f32 / 100.0),
                level: match o.battery {
                    Some(p) if !o.charging && p <= 5 => 2,
                    Some(p) if !o.charging && p < 25 => 1,
                    _ => 0,
                },
                charging: o.charging,
            })
            .collect();
        self.others_from = now;
    }
}

fn is_mouse(kind: Kind) -> bool {
    matches!(kind, Kind::MouseDpi | Kind::MouseBattery | Kind::MouseLink)
}

/// The battery warning's text: "charge your mouse" (red) or "battery low" (yellow).
fn mouse_warning(s: &mut State, critical: bool) {
    let t = s.strings();
    let headset = s.mouse.model.is_some_and(mouse::Model::is_headset);
    let text = match (critical, headset) {
        (true, false) => t.battery_critical,
        (false, false) => t.battery_low,
        (true, true) => t.headset_battery_critical,
        (false, true) => t.headset_battery_low,
    };
    s.mouse.warn = text::raster(text, -66, 700);
    s.mouse.intro = 0.0;
}

/// The mouse's capsules move on their own: the number rolls, the stage marker slides, the battery fills.
fn mouse_step(s: &mut State, dt: f32) {
    let m = &mut s.mouse;
    if !is_mouse(s.kind) && !s.prev.is_some_and(|(k, _)| is_mouse(k)) {
        // nothing of it on show: no motion to wait for
        (m.roll, m.intro, m.stage, m.fill, m.dpi_old) = (1.0, 1.0, m.stage_goal, m.fill_goal, None);
        return;
    }
    if m.roll < 1.0 {
        m.roll = (m.roll + dt / 0.38).min(1.0);
        if m.roll >= 1.0 {
            m.dpi_old = None;
        }
    }
    if m.intro < 1.0 {
        m.intro = (m.intro + dt / 0.6).min(1.0);
    }
    approach(&mut m.stage, m.stage_goal, 13.0, dt, 0.002);
    approach(&mut m.fill, m.fill_goal, 3.2, dt, 0.002);
}

/// Brings up a mouse capsule, unless the user has a panel or the volume open (a red warning still comes back
/// when that closes, see begin_close).
unsafe fn mouse_show(hwnd: HWND, kind: Kind, hide: Option<u32>) {
    let s = st();
    if panel_open() || s.setup.is_some() || s.volume_ui() {
        return;
    }
    if s.live() && s.kind == kind {
        // already on show: it only changes (and stays a little longer)
        s.steady_drawn = false;
        s.sync_hooks();
        hide_after(hwnd, hide);
        frames(hwnd);
        return;
    }
    s.mouse.intro = 0.0;
    open(hwnd, kind, false, hide);
}

/// The Razer mouse said something (razer.rs): its DPI or battery changed, it woke up, or it went quiet.
unsafe fn mouse_event(hwnd: HWND, ev: usize) {
    let s = st();
    if !s.cfg.mouse {
        return;
    }
    let Some(now) = mouse::status() else {
        // asleep or unplugged: the red warning goes with it (and comes back when it wakes)
        if s.mouse.crit {
            s.mouse.crit = false;
            if s.live() && s.kind == Kind::MouseBattery {
                begin_close(hwnd);
            }
        }
        return;
    };
    let t = s.strings();
    s.mouse.set_battery(t, &now);
    let rows = s.mouse.others.len();
    s.mouse.set_others(t, mouse::others());
    if rows != s.mouse.others.len() && s.live() && is_mouse(s.kind) {
        s.apply_targets(); // a row more or less: the card on show grows or shrinks
    }
    if s.mouse.model != Some(now.model) {
        s.mouse.model = Some(now.model);
        s.mouse.title = text::raster(now.model.name(), -64, 700);
    }
    let level = s.mouse.level;
    // a little slack before a warning counts as over: the reading jitters by a percent or so
    let pct = now.battery.unwrap_or(100);
    let crit_over = now.charging || pct >= 8;
    let low_over = now.charging || pct >= 30;
    let was = s.mouse.seen;
    if ev == mouse::EV_BATTERY || ev == mouse::EV_WAKE {
        s.mouse.seen = if level > 0 { level.max(was) } else if low_over { 0 } else { was };
    }
    let showing = s.live() && is_mouse(s.kind);
    let was_charging = std::mem::replace(&mut s.mouse.charging_seen, now.charging);
    let plugged = now.charging && !was_charging;
    let unplugged = !now.charging && was_charging && ev == mouse::EV_BATTERY;

    // the DPI and its stage, always up to date (a wake-up brings them too)
    let dpi_changed = now.dpi != s.mouse.dpi_value;
    if dpi_changed || s.mouse.dpi.is_none() {
        let rolls = ev == mouse::EV_DPI && s.live() && s.kind == Kind::MouseDpi;
        s.mouse.dpi_old = if rolls { s.mouse.dpi.take() } else { None };
        s.mouse.roll = if rolls { 0.0 } else { 1.0 };
        s.mouse.dpi = text::raster(&now.dpi.to_string(), -100, 700);
        s.mouse.dpi_value = now.dpi;
    }
    if s.mouse.dpi_label.is_none() {
        s.mouse.dpi_label = text::raster("DPI", -48, 700);
    }
    s.mouse.stage_goal = now.stage as f32;
    if s.mouse.stages != now.stages || ev != mouse::EV_DPI {
        s.mouse.stage = s.mouse.stage_goal; // a new set of stages: no slide from the old one
    }
    s.mouse.stages = now.stages;

    match ev {
        mouse::EV_WAKE => {
            s.mouse.crit = level == 2;
            s.mouse.low_pending = level == 1 && was == 0; // shown once, after the card
            s.mouse.sub = text::raster(if now.charging { t.mouse_charging } else { t.mouse_connected }, -48, 400);
            s.mouse.fill = 0.0; // it fills up as the card comes in
            if s.mouse.crit {
                mouse_warning(s, true);
            }
            mouse_show(hwnd, Kind::MouseLink, Some(MOUSE_LINK_MS));
        }
        mouse::EV_DPI => {
            if !(s.live() && s.kind == Kind::MouseDpi) {
                s.mouse.stage = s.mouse.stage_goal;
            }
            mouse_show(hwnd, Kind::MouseDpi, Some(MOUSE_DPI_MS));
        }
        mouse::EV_BATTERY if plugged => {
            // on the charger: the card again, saying so, its battery green with a bolt (any warning ends)
            (s.mouse.crit, s.mouse.low_pending) = (false, false);
            s.mouse.sub = text::raster(t.mouse_charging, -48, 400);
            s.mouse.fill = 0.0;
            if s.live() && s.kind == Kind::MouseBattery {
                s.set_kind(Kind::MouseLink); // the warning turns into the card
                s.mouse.intro = 0.0;
                s.apply_targets();
                s.sync_hooks();
                hide_after(hwnd, Some(MOUSE_LINK_MS));
                frames(hwnd);
            } else {
                if s.live() && s.kind == Kind::MouseLink {
                    s.mouse.intro = 0.0; // plays again with the new text
                }
                mouse_show(hwnd, Kind::MouseLink, Some(MOUSE_LINK_MS));
            }
        }
        mouse::EV_BATTERY if unplugged && level == 0 => {
            // off the charger (and not low: then the warning comes instead): the card says so
            s.mouse.sub = text::raster(t.mouse_unplugged, -48, 400);
            s.mouse.fill = 0.0;
            if s.live() && s.kind == Kind::MouseLink {
                s.mouse.intro = 0.0;
            }
            mouse_show(hwnd, Kind::MouseLink, Some(MOUSE_LINK_MS));
        }
        mouse::EV_BATTERY => {
            if level == 2 && !s.mouse.crit {
                s.mouse.crit = true;
                mouse_warning(s, true);
                mouse_show(hwnd, Kind::MouseBattery, None);
            } else if level == 1 && was == 0 {
                mouse_warning(s, false);
                mouse_show(hwnd, Kind::MouseBattery, Some(MOUSE_LOW_MS));
            } else if s.mouse.crit && crit_over {
                s.mouse.crit = false; // charging, or it climbed back
                if s.live() && s.kind == Kind::MouseBattery {
                    begin_close(hwnd);
                }
            } else if showing {
                s.steady_drawn = false;
                frames(hwnd);
            }
        }
        _ => {}
    }
}

/// Another plugged-in device went on or off the charger or ran low (`why`, mouse::OTHER_*): a card about it,
/// with every other device (the followed one too) in the rows under it.
unsafe fn other_event(hwnd: HWND, model: usize, why: isize) {
    let s = st();
    if !s.cfg.mouse {
        return;
    }
    let all = mouse::others();
    let Some(o) = all.iter().copied().find(|o| o.model as usize == model) else { return };
    let t = s.strings();
    let as_status = mouse::Status { model: o.model, battery: o.battery, charging: o.charging, ..Default::default() };
    s.mouse.set_battery(t, &as_status);
    if s.mouse.model != Some(o.model) {
        s.mouse.model = Some(o.model);
        s.mouse.title = text::raster(o.model.name(), -64, 700);
    }
    let followed = mouse::status().map(|p| mouse::Other { model: p.model, battery: p.battery, charging: p.charging });
    let rows = followed.into_iter().chain(all.into_iter().filter(|x| x.model != o.model)).collect();
    s.mouse.set_others(t, rows);
    if why != mouse::OTHER_LOW {
        s.mouse.sub = text::raster(if why == mouse::OTHER_CHARGING { t.mouse_charging } else { t.mouse_unplugged }, -48, 400);
        s.mouse.fill = 0.0;
        if s.live() && s.kind == Kind::MouseLink {
            s.mouse.intro = 0.0;
        }
        if s.live() && is_mouse(s.kind) {
            s.apply_targets(); // the rows may differ from the card on show
        }
        mouse_show(hwnd, Kind::MouseLink, Some(MOUSE_LINK_MS));
    } else {
        if s.live() && is_mouse(s.kind) {
            s.apply_targets();
        }
        let critical = s.mouse.level == 2;
        mouse_warning(s, critical);
        mouse_show(hwnd, Kind::MouseBattery, Some(MOUSE_LOW_MS));
    }
}

/// A full-screen app in front hides the pinned capsule (unless "pin over full-screen apps" is on); it comes
/// back when that app is gone.
unsafe fn pin_poll(hwnd: HWND) {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
    let s = st();
    let busy = !s.cfg.pin_fullscreen && matches!(SHQueryUserNotificationState(), Ok(QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE));
    if busy != s.pin_suspended {
        s.pin_suspended = busy;
        pin_refresh(hwnd);
    }
}

impl State {
    /// Are the visualizer bars still on their way somewhere?
    fn viz_moving(&self) -> bool {
        self.viz.iter().any(|&b| b > 0.09)
    }
}

/// Moves the five bars toward what the sound is doing right now: bass on the left, treble on the right (see
/// spectrum.rs). They rise fast and fall slower, and settle when nothing plays.
fn viz_step(s: &mut State, dt: f32) {
    let playing = media::playing();
    let heard = spectrum::levels();
    for i in 0..5 {
        let target = if playing { 0.07 + 0.93 * heard[i] } else { 0.06 };
        let rate = if target > s.viz[i] { 34.0 } else { 11.0 };
        s.viz[i] += (target - s.viz[i]) * (1.0 - (-dt * rate).exp());
    }
}

/// A different song started playing.
unsafe fn song_changed(hwnd: HWND) {
    let s = st();
    s.song = media::song();
    s.marq = 0.0;
    if s.pin_wanted() {
        return; // the pinned capsule already shows it (see pin_refresh, which the same change triggers)
    }
    if s.volume_ui() {
        // the user has the volume UI open: the line / rows under the control just change
        s.apply_targets();
        s.sync_hooks();
        frames(hwnd);
        return;
    }
    if !s.cfg.notify_song || panel_open() || s.upload_shown() {
        return;
    }
    s.notice = None;
    open(hwnd, Kind::Song, false, Some(HIDE_SONG_MS));
}

unsafe fn device_changed(hwnd: HWND) {
    let s = st();
    s.ep = None; // the default device is another one: read it next time
    if !s.cfg.notify_device || panel_open() || s.volume_ui() || s.upload_shown() {
        return;
    }
    s.device_line = device::default_name().and_then(|name| text::raster(&name, -44, 400));
    open(hwnd, Kind::Volume, false, Some(HIDE_DEVICE_MS));
}

/// The screenshot was pasted (Ctrl+V): say where, like a phone does.
unsafe fn snip_pasted(hwnd: HWND) {
    let s = st();
    let app = foreground_app_name();
    let big = text::raster(s.strings().snip_pasted, -64, 700);
    let small = text::raster(&app, -48, 400);
    let line = text::raster(&format!("{} · {app}", s.strings().snip_pasted), -44, 400);
    s.notice = big.zip(line).map(|(big, line)| Arc::new(Song { big, small, line, art: None, key: String::new() }));
    s.notice_icon = 2;
    s.marq = 0.0;
    if !panel_open() {
        open(hwnd, Kind::Song, false, Some(HIDE_SNIP_MS));
    }
}

/// "Discord", "Brave", ...: the program of the window that has the focus.
unsafe fn foreground_app_name() -> String {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let fg = GetForegroundWindow();
    let mut pid = 0u32;
    GetWindowThreadProcessId(fg, Some(&mut pid));
    let mut name = String::new();
    if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        }
        let _ = windows::Win32::Foundation::CloseHandle(process);
    }
    match name.to_lowercase().as_str() {
        "msedge" => "Edge".into(),
        "chrome" => "Chrome".into(),
        "whatsapp" | "whatsapp.root" => "WhatsApp".into(),
        "applicationframehost" | "" => "…".into(),
        other => other.chars().next().map(|c| c.to_uppercase().collect::<String>() + &other[c.len_utf8()..]).unwrap_or_default(),
    }
}

/// A screenshot went to the clipboard: say so in the capsule.
unsafe fn snip_taken(hwnd: HWND, w: i32, h: i32) {
    let s = st();
    let big = text::raster(s.strings().snip_copied, -64, 700);
    let small = text::raster(&format!("{w} × {h}"), -48, 400);
    let line = text::raster(&format!("{} · {w} × {h}", s.strings().snip_copied), -44, 400);
    s.notice = big.zip(line).map(|(big, line)| Arc::new(Song { big, small, line, art: None, key: String::new() }));
    s.notice_icon = 1;
    s.marq = 0.0;
    if !panel_open() {
        open(hwnd, Kind::Song, false, Some(HIDE_SNIP_MS));
    }
}

/// A push notification: chime, and the capsule stays up for five seconds.
unsafe fn push_notice(hwnd: HWND) {
    let Some(n) = ntfy::take() else { return };
    let s = st();
    audio::play_notification(&s.cfg.ntfy_sound, s.cfg.ntfy_volume);
    if panel_open() {
        return;
    }
    let title = if n.title.is_empty() { "ntfy".to_string() } else { n.title.clone() };
    let line = if n.message.is_empty() { title.clone() } else { format!("{title} — {}", n.message) };
    let song = if n.message.is_empty() {
        text::raster(&title, -64, 700).zip(text::raster(&line, -44, 400)).map(|(big, line)| Song { big, small: None, line, art: None, key: String::new() })
    } else {
        text::raster(&title, -64, 700)
            .zip(text::raster(&line, -44, 400))
            .map(|(big, line)| Song { big, small: text::raster(&n.message.replace('\n', " "), -48, 400), line, art: None, key: String::new() })
    };
    s.notice = song.map(Arc::new);
    s.notice_icon = 0;
    s.marq = 0.0;
    open(hwnd, Kind::Song, false, Some(HIDE_NTFY_MS));
}

// ---- settings --------------------------------------------------------------

impl State {
    /// The text behind a settings field: 0 topic, 1 server, 2 token.
    fn field_mut(&mut self, field: usize) -> &mut String {
        match field {
            0 => &mut self.cfg.ntfy_topic,
            1 => &mut self.cfg.ntfy_server,
            _ => &mut self.cfg.ntfy_token,
        }
    }

    /// Rebuilds the value bitmaps of the topic / server / token / sound rows.
    fn refresh_values(&mut self) {
        let sound = std::path::Path::new(&self.cfg.ntfy_sound).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let t = self.strings();
        let language = match self.cfg.language.as_str() {
            "en" => t.english,
            "tr" => t.turkish,
            _ => t.auto,
        };
        let theme = match self.cfg.theme.as_str() {
            "dark" => t.dark,
            "light" => t.light,
            _ => t.auto,
        };
        let fields = [
            (self.cfg.ntfy_topic.clone(), t.tap_to_type),
            (self.cfg.ntfy_server.clone(), "https://ntfy.sh"),
            ("*".repeat(self.cfg.ntfy_token.chars().count()), t.tap_to_type),
            (sound, t.builtin_sound),
            (language.to_string(), ""),
            (theme.to_string(), ""),
        ];
        self.values = fields
            .into_iter()
            .filter_map(|(text, hint)| match text.is_empty() {
                false => text::raster(&text, -48, 400).map(|b| (b, false)),
                true => text::raster(hint, -48, 400).map(|b| (b, true)),
            })
            .collect();
    }
}

/// The settings panel is opening: the rest (rasterizing 18 labels, asking the registry) is done in the first
/// frame, while its content is still invisible, so the capsule itself appears without waiting for it.
fn prepare_settings(s: &mut State) {
    s.editing = None;
    EDITING.store(false, Relaxed);
    s.hover = None;
    s.deferred = Deferred::Settings;
}

fn load_settings(s: &mut State) {
    s.refresh_values();
    if s.labels.is_empty() {
        s.labels = s.strings().labels.iter().filter_map(|l| text::raster(l, -52, 400)).collect();
    }
    s.autostart = autostart::is_enabled();
    s.toggles = s.toggle_targets();
}

unsafe fn toggle_settings(hwnd: HWND) {
    if settings_open() {
        begin_close(hwnd);
    } else {
        open(hwnd, Kind::Settings, false, None);
    }
}

/// Pointer position in shape coordinates (logical units from its top-left corner).
fn pointer_in_shape(s: &State, screen: POINT) -> (f32, f32) {
    let (l, t, _, _) = s.area();
    ((screen.x - l) as f32 / s.unit, (screen.y - t) as f32 / s.unit)
}

unsafe fn settings_click(hwnd: HWND, screen: POINT) {
    let s = st();
    if !settings_open() {
        return;
    }
    let (tx, ty) = pointer_in_shape(s, screen);
    let hit = ui::hit(tx, ty);
    // clicking anywhere but the field being edited ends the editing
    if s.editing.is_some() && !matches!(hit, Some(ui::Hit::Text(f)) if Some(ui::ROW_TOPIC + f) == s.editing) {
        end_edit(hwnd, true);
    }
    let Some(hit) = hit else { return };
    match hit {
        ui::Hit::Text(field) => {
            start_edit(s, field);
            frames(hwnd);
            return;
        }
        ui::Hit::Sound => {
            if let Some(path) = pick_sound() {
                s.cfg.ntfy_sound = path;
                audio::play_notification(&s.cfg.ntfy_sound, s.cfg.ntfy_volume); // preview
                s.refresh_values();
            }
        }
        ui::Hit::Language | ui::Hit::Theme => {
            if hit == ui::Hit::Language {
                s.cfg.cycle_language();
            } else {
                s.cfg.cycle_theme();
            }
            s.refresh_look();
            s.labels.clear();
            load_settings(s);
        }
        ui::Hit::SoundReset => {
            if !s.cfg.ntfy_sound.is_empty() {
                s.cfg.ntfy_sound.clear();
                s.refresh_values();
            }
        }
        ui::Hit::Toggle(0) => s.cfg.click_sound ^= true,
        ui::Hit::Toggle(1) => s.cfg.notify_song ^= true,
        ui::Hit::Toggle(2) => s.cfg.notify_device ^= true,
        ui::Hit::Toggle(3) => {
            s.cfg.volume_keys ^= true;
            VOLUME_KEYS.store(s.cfg.volume_keys, Relaxed);
        }
        ui::Hit::Toggle(4) => s.cfg.low_power ^= true,
        ui::Hit::Toggle(5) => s.cfg.upload_sound ^= true,
        ui::Hit::Toggle(6) => {
            s.cfg.screenshot ^= true;
            SNIP_KEY.store(s.cfg.screenshot, Relaxed);
        }
        ui::Hit::Toggle(7) => {
            s.cfg.pin_media ^= true;
            media::set_pin(s.cfg.pin_media);
            pin_refresh(hwnd);
        }
        ui::Hit::Toggle(11) => {
            s.cfg.mouse ^= true;
            mouse::set_enabled(s.cfg.mouse, hwnd.0 as isize, WM_APP_MOUSE);
            if !s.cfg.mouse {
                (s.mouse.crit, s.mouse.low_pending) = (false, false); // its warnings go with it
            } else if mouse::status().is_some() {
                s.mouse.crit = s.mouse.level == 2; // already awake: no wake-up will come to say so
            }
        }
        ui::Hit::Toggle(12) => {
            s.cfg.auto_update ^= true;
            if s.cfg.auto_update && install::running_installed() {
                SetTimer(hwnd, T_UPDATE, 3000, None); // look right away
            }
        }
        ui::Hit::Toggle(8) => {
            s.cfg.pin_fullscreen ^= true;
            pin_poll(hwnd); // in or out of full-screen apps right away
        }
        ui::Hit::Toggle(9) => {
            s.cfg.pin_lyrics ^= true;
            media::set_lyrics(s.cfg.pin_lyrics);
            lyric_reset(s);
            pin_refresh(hwnd);
        }
        ui::Hit::Toggle(10) => {
            s.cfg.lyric_words ^= true;
            lyric_reset(s);
            pin_refresh(hwnd);
        }
        ui::Hit::Toggle(ui::TOGGLE_AUTOSTART) => {
            s.autostart ^= true;
            autostart::set(s.autostart);
        }
        ui::Hit::Toggle(_) => {}
        ui::Hit::VolMinus | ui::Hit::VolPlus => {
            let at = NOTIFY_VOLUMES.iter().position(|&v| v >= s.cfg.ntfy_volume).unwrap_or(2) as i32;
            let next = (at + if hit == ui::Hit::VolPlus { 1 } else { -1 }).clamp(0, NOTIFY_VOLUMES.len() as i32 - 1);
            s.cfg.ntfy_volume = NOTIFY_VOLUMES[next as usize];
            audio::play_notification(&s.cfg.ntfy_sound, s.cfg.ntfy_volume); // preview at the new level
        }
        ui::Hit::StepMinus | ui::Hit::StepPlus => {
            let at = STEPS.iter().position(|&v| v >= s.cfg.step).unwrap_or(1) as i32;
            let next = (at + if hit == ui::Hit::StepPlus { 1 } else { -1 }).clamp(0, STEPS.len() as i32 - 1);
            s.cfg.step = STEPS[next as usize];
        }
        ui::Hit::Quit => {
            let _ = DestroyWindow(hwnd);
            return;
        }
        ui::Hit::CheckUpdate => {
            if !s.update_asked {
                s.update_asked = true;
                update_row(hwnd, s.strings().update_checking);
                crate::update::check(hwnd.0 as isize, WM_APP_UPDATE);
            }
        }
    }
    s.cfg.save();
    frames(hwnd);
}

fn start_edit(s: &mut State, field: usize) {
    if s.editing == Some(ui::ROW_TOPIC + field) {
        return;
    }
    s.edit_backup = s.field_mut(field).clone();
    s.editing = Some(ui::ROW_TOPIC + field);
    EDITING.store(true, Relaxed);
    s.refresh_values();
}

/// Ends editing: `commit` keeps the text, otherwise the old text comes back. A changed ntfy
/// field restarts the listener right away.
unsafe fn end_edit(hwnd: HWND, commit: bool) {
    let s = st();
    let Some(row) = s.editing.take() else { return };
    EDITING.store(false, Relaxed);
    let field = row - ui::ROW_TOPIC;
    if !commit {
        *s.field_mut(field) = s.edit_backup.clone();
    }
    s.refresh_values();
    s.cfg.save();
    ntfy::start(&s.cfg, hwnd.0 as isize, WM_APP_NTFY);
    frames(hwnd);
}

/// A typed character (or 8 backspace, 13 enter, 27 esc, 22 paste) for the field being edited.
unsafe fn settings_key(hwnd: HWND, ch: u32) {
    let s = st();
    let Some(row) = s.editing else { return };
    let field = row - ui::ROW_TOPIC;
    match ch {
        13 => return end_edit(hwnd, true),
        27 => return end_edit(hwnd, false),
        8 => {
            s.field_mut(field).pop();
        }
        22 => {
            if let Some(text) = clipboard::get() {
                let line = text.lines().next().unwrap_or("").trim().to_string();
                s.field_mut(field).push_str(&line);
            }
        }
        c => {
            if let Some(c) = char::from_u32(c) {
                s.field_mut(field).push(c);
            }
        }
    }
    let text = s.field_mut(field);
    if text.chars().count() > 200 {
        *text = text.chars().take(200).collect();
    }
    s.refresh_values();
    draw_and_present(hwnd);
}

/// Native "open file" dialog for the notification sound.
unsafe fn pick_sound() -> Option<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW};
    let mut file = [0u16; 520];
    let t = st().strings();
    let filter: Vec<u16> = format!("{}\0*.mp3;*.wav\0{}\0*.*\0\0", t.sound_files, t.all_files).encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut ofn).as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&file[..len]))
}

/// Left click inside whichever panel is up.
unsafe fn panel_click(hwnd: HWND, screen: POINT) {
    match st().kind {
        Kind::Settings => settings_click(hwnd, screen),
        Kind::History => history_click(hwnd, screen),
        Kind::Setup => setup_click(hwnd, screen),
        _ => {}
    }
}

unsafe fn panel_hover(hwnd: HWND) {
    let s = st();
    if !panel_open() {
        return;
    }
    let mut cur = POINT::default();
    let _ = GetCursorPos(&mut cur);
    let (tx, ty) = pointer_in_shape(s, cur);
    let inside = tx >= 0.0 && tx < s.tw;
    // (old, new) hover; the band of rows between them is all that has to be redrawn
    let change = match s.kind {
        Kind::Settings => {
            let hover = if inside { ui::row_at(ty) } else { None };
            let old = std::mem::replace(&mut s.hover, hover);
            (old != hover).then_some((old, hover, (|i| (ui::PAD + i as f32 * ui::ROW_H, ui::ROW_H)) as fn(usize) -> (f32, f32)))
        }
        Kind::History => {
            let hover = if inside { ui::hist_row_at(ty).filter(|&i| i < s.hist_rows.len()) } else { None };
            let old = std::mem::replace(&mut s.hist_hover, hover);
            (old != hover).then_some((old, hover, (|i| (ui::PAD + ui::HEAD_H + i as f32 * ui::HIST_ROW_H, ui::HIST_ROW_H)) as fn(usize) -> (f32, f32)))
        }
        Kind::Setup => {
            let hover = if inside { ui::setup_element(tx, ty) } else { None };
            let old = s.setup.as_mut().map(|x| std::mem::replace(&mut x.hover, hover));
            old.filter(|&o| o != hover).map(|o| {
                (o, hover, (|i| if i < 4 { (ui::SETUP_HEAD + i as f32 * ui::SETUP_ROW_H, ui::SETUP_ROW_H) } else { (ui::SETUP_BTN_Y, ui::SETUP_BTN_H) }) as fn(usize) -> (f32, f32))
            })
        }
        _ => None,
    };
    let Some((old, new, rows)) = change else { return };
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for i in [old, new].into_iter().flatten() {
        let (y, h) = rows(i);
        (top, bottom) = (top.min(y), bottom.max(y + h));
    }
    let steady = !s.closing && s.prev.is_none() && s.staged.is_none() && s.delay <= 0.0 && s.content > 0.99 && (s.w - s.tw).abs() < 0.5 && (s.h - s.th).abs() < 0.5;
    if top > bottom || !steady {
        draw_and_present(hwnd);
    } else {
        draw_and_present_rows(hwnd, top, bottom);
    }
}

// ---- uploads ---------------------------------------------------------------

fn speed_text(bytes_per_sec: f32) -> String {
    if bytes_per_sec >= 1_048_576.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_048_576.0)
    } else {
        format!("{:.0} KB/s", bytes_per_sec / 1024.0)
    }
}

/// The uploader started a file, finished the batch, or failed.
unsafe fn upload_event(hwnd: HWND) {
    let s = st();
    let status = upload::status();
    let t = s.strings();
    match status.phase {
        upload::Phase::Uploading => {
            if s.up.name_key != status.name || s.up.phase != UploadPhase::Uploading {
                s.up = UploadUi { name_key: status.name.clone(), name: text::raster(&status.name, -56, 700), ..UploadUi::new() };
            }
            s.marq = 0.0;
            open(hwnd, Kind::Upload, false, None);
        }
        upload::Phase::Done => {
            let line = if status.links.len() == 1 {
                history::short(&status.links[0]).to_string()
            } else {
                format!("{} {}", status.links.len(), t.links_word)
            };
            s.up.phase = UploadPhase::Done;
            s.up.progress = 1.0;
            s.up.msg = text::raster(t.copied_clipboard, -56, 700);
            s.up.line = text::raster(&line, -44, 400);
            s.marq = 0.0;
            if s.cfg.upload_sound {
                audio::play_notification(&s.cfg.ntfy_sound, s.cfg.ntfy_volume);
            }
            open(hwnd, Kind::Upload, false, Some(HIDE_UPLOAD_DONE_MS));
        }
        upload::Phase::Failed => {
            s.up.phase = UploadPhase::Failed;
            s.up.msg = text::raster(t.upload_failed, -56, 700);
            s.up.line = text::raster(&status.error, -44, 400);
            s.marq = 0.0;
            open(hwnd, Kind::Upload, false, Some(HIDE_UPLOAD_FAILED_MS));
        }
        upload::Phase::Idle => {}
    }
}

/// Per-frame upload progress and the speed text (refreshed a few times a second).
/// Eases `x` toward `goal` (`rate` per second, frame-rate independent) and snaps once within `snap` of it.
fn approach(x: &mut f32, goal: f32, rate: f32, dt: f32, snap: f32) {
    *x += (goal - *x) * (1.0 - (-dt * rate).exp());
    if (goal - *x).abs() < snap {
        *x = goal;
    }
}

fn upload_tick(s: &mut State, dt: f32) {
    let (total, index, count) = upload::progress();
    let sent = upload::SENT.load(Relaxed);
    let target = if total > 0 { (sent as f32 / total as f32).min(1.0) } else { 0.0 };
    s.up.progress += (target - s.up.progress) * (1.0 - (-dt * 10.0).exp());
    s.up.clock += dt;
    if s.up.clock >= 0.3 {
        let rate = sent.saturating_sub(s.up.last_sent) as f32 / s.up.clock;
        s.up.speed += (rate - s.up.speed) * 0.5;
        s.up.last_sent = sent;
        s.up.clock = 0.0;
        s.up.info = text::raster(&format!("{} · {}/{}", speed_text(s.up.speed), index, count), -44, 400);
    }
}

// ---- link history ----------------------------------------------------------

fn rebuild_history(s: &mut State) {
    let t = s.strings();
    s.hist_title = text::raster(&format!("{} ({})", t.history_title, s.hist.len()), -56, 700);
    s.hist_empty = if s.hist.is_empty() { text::raster(t.history_empty, -48, 400) } else { None };
    s.hist_scroll = s.hist_scroll.min(s.hist.len().saturating_sub(ui::HIST_ROWS));
    s.hist_rows = s
        .hist
        .iter()
        .skip(s.hist_scroll)
        .take(ui::HIST_ROWS)
        .filter_map(|e| {
            let when: String = e.stamp.chars().take(16).collect();
            Some((text::raster(&e.name, -52, 700)?, text::raster(&format!("{when} · {}", history::short(&e.link)), -42, 400)?))
        })
        .collect();
}

unsafe fn toggle_history(hwnd: HWND) {
    let s = st();
    if s.live() && s.kind == Kind::History {
        begin_close(hwnd);
        return;
    }
    if s.setup.is_some() {
        return;
    }
    s.hist = history::load();
    (s.hist_scroll, s.hist_hover, s.hist_flash, s.hist_status) = (0, None, None, None);
    s.deferred = Deferred::History; // its rows are rasterized in the first frame
    open(hwnd, Kind::History, false, None);
}

unsafe fn history_scroll(hwnd: HWND, delta: i32) {
    let s = st();
    let max = s.hist.len().saturating_sub(ui::HIST_ROWS);
    let next = if delta > 0 { s.hist_scroll.saturating_sub(1) } else { (s.hist_scroll + 1).min(max) };
    if next != s.hist_scroll {
        s.hist_scroll = next;
        rebuild_history(s);
        draw_and_present(hwnd);
    }
}

unsafe fn history_click(hwnd: HWND, screen: POINT) {
    let s = st();
    let (tx, ty) = pointer_in_shape(s, screen);
    match ui::hist_hit(tx, ty) {
        Some(ui::HistHit::Copy(row)) => {
            let link = s.hist.get(s.hist_scroll + row).map(|e| e.link.clone());
            if link.is_some_and(|l| clipboard::set(&l)) {
                s.hist_flash = Some((row, 1.0));
                s.hist_status = text::raster(s.strings().copied, -44, 400).map(|b| (b, 1.6));
                frames(hwnd);
            }
        }
        Some(ui::HistHit::Delete(row)) => {
            if let Some(link) = s.hist.get(s.hist_scroll + row).map(|e| e.link.clone()) {
                history::remove(&link);
            }
            s.hist = history::load();
            rebuild_history(s);
            s.apply_targets(); // the panel is only as tall as its list
            s.sync_hooks();
            frames(hwnd);
        }
        None => {}
    }
}

// ---- first-run setup -------------------------------------------------------

fn build_setup_texts(s: &mut State) {
    let t = s.strings();
    let update = s.setup.as_ref().is_some_and(|x| x.update);
    let language = match s.cfg.language.as_str() {
        "en" => t.english,
        "tr" => t.turkish,
        _ => t.auto,
    };
    let theme = match s.cfg.theme.as_str() {
        "dark" => t.dark,
        "light" => t.light,
        _ => t.auto,
    };
    let texts = [
        (t.setup_title, -64, 700),
        (if update { t.setup_update_sub } else { t.setup_sub }, -44, 400),
        (t.setup_autostart, -52, 400),
        (t.setup_autoupdate, -52, 400),
        (t.labels[ui::ROW_LANG], -52, 400),
        (t.labels[ui::ROW_THEME], -52, 400),
        (if update { t.setup_update } else { t.setup_install }, -52, 700),
        (t.setup_portable, -52, 400),
    ];
    s.setup_texts = texts.iter().filter_map(|&(text, height, weight)| text::raster(text, height, weight)).collect();
    s.setup_values = [language, theme].iter().filter_map(|text| text::raster(text, -48, 400)).collect();
}

/// A downloaded copy that was started: the capsule offers to install it, or to update the installed build.
pub unsafe fn open_setup(hwnd: HWND, update: bool) {
    let s = st();
    // a fresh install starts with everything on; an update keeps what is switched on now
    let autostart = if update { autostart::is_enabled() } else { true };
    let auto_update = s.cfg.auto_update; // on unless it was switched off
    s.setup = Some(SetupState { update, autostart, auto_update, toggles: [f32::from(u8::from(autostart)), f32::from(u8::from(auto_update))], hover: None });
    build_setup_texts(s);
    open(hwnd, Kind::Setup, false, None);
}

unsafe fn setup_click(hwnd: HWND, screen: POINT) {
    let s = st();
    let (tx, ty) = pointer_in_shape(s, screen);
    match ui::setup_hit(tx, ty) {
        Some(ui::SetupHit::Autostart) => {
            if let Some(x) = s.setup.as_mut() {
                x.autostart ^= true;
            }
        }
        Some(ui::SetupHit::AutoUpdate) => {
            if let Some(x) = s.setup.as_mut() {
                x.auto_update ^= true;
            }
        }
        Some(ui::SetupHit::Language) => {
            s.cfg.cycle_language();
            s.refresh_look();
            build_setup_texts(s);
        }
        Some(ui::SetupHit::Theme) => {
            s.cfg.cycle_theme();
            s.refresh_look();
            build_setup_texts(s);
        }
        Some(ui::SetupHit::Install) => return run_install(hwnd),
        Some(ui::SetupHit::Portable) => {
            install::remember_portable();
            s.setup = None;
            begin_close(hwnd);
            return;
        }
        None => return,
    }
    frames(hwnd);
}

/// Installs with the chosen options, starts the installed copy and leaves.
unsafe fn run_install(hwnd: HWND) {
    let s = st();
    let (autostart, auto_update) = s.setup.as_ref().map_or((true, true), |x| (x.autostart, x.auto_update));
    s.cfg.auto_update = auto_update;
    s.cfg.save(); // language and theme chosen here carry over to the installed copy
    match install::install(install::Options { autostart }, s.strings()) {
        Ok(exe) => {
            install::launch(&exe);
            let _ = DestroyWindow(hwnd);
        }
        Err(e) => install::info(&e.to_string()),
    }
}

// ---- animation tick --------------------------------------------------------

/// Damped spring toward `target`, integrated in small steps so big frame gaps stay stable.
fn spring(x: &mut f32, v: &mut f32, target: f32, (k, c): (f32, f32), dt: f32) {
    let steps = ((dt * 240.0).ceil() as i32).max(1);
    let h = dt / steps as f32;
    for _ in 0..steps {
        *v += (k * (target - *x) - c * *v) * h;
        *x += *v * h;
    }
}

unsafe fn tick(hwnd: HWND) {
    let s = st();
    s.steady_drawn &= s.kind == Kind::Pinned;
    match std::mem::replace(&mut s.deferred, Deferred::Nothing) {
        Deferred::Settings => load_settings(s),
        Deferred::History => rebuild_history(s),
        Deferred::Nothing => {}
    }
    let now = Instant::now();
    let dt = s.last_tick.map_or(0.016, |l| (now - l).as_secs_f32()).min(0.05);
    s.last_tick = Some(now);
    s.marq += dt;
    if s.kind == Kind::Pinned && !s.closing {
        viz_step(s, dt);
    }
    if !s.fades() {
        s.pin_fade_goal = 0.0;
    }
    if s.pin_fade != s.pin_fade_goal {
        approach(&mut s.pin_fade, s.pin_fade_goal, PIN_FADE_RATE, dt, 0.004);
    }
    if !s.lyric_pts.is_empty() && s.kind == Kind::Pinned {
        let ahead = s.cfg.lyric_ahead_ms as f32 / 1000.0;
        s.lyric_fill = media::position().map(|t| crate::lyrics::fill_x(&s.lyric_pts, t + ahead));
    }
    mouse_step(s, dt);
    if s.lyric_swap < 1.0 {
        s.lyric_swap = (s.lyric_swap + dt / LYRIC_SWAP_SECS).min(1.0);
        if s.lyric_swap >= 1.0 {
            s.lyric_old = None;
        }
    }
    if s.song_prev.is_some() {
        s.song_swap = (s.song_swap + dt / SONG_SWAP_SECS).min(1.0);
        if s.song_swap >= 1.0 {
            s.song_prev = None;
        }
    }
    if let Some((icon, t)) = s.icon {
        let t = t + dt;
        s.icon = (t < ICON_SECS).then_some((icon, t));
    }

    if s.closing && s.close_phase == 1 && s.content <= 0.01 && s.prev.is_none() {
        s.close_phase = 2;
        (s.tw, s.th) = ui::SMALL;
        s.trad = ui::SMALL.1 / 2.0;
    }

    if s.delay > 0.0 {
        s.delay -= dt;
        if s.delay <= 0.0 && s.queued {
            s.queued = false;
            s.apply_targets();
            s.sync_hooks();
        }
    }
    if let Some((into_knob, (fw, fh, fr))) = s.staged {
        let ready = if into_knob { s.h >= fh * 0.7 } else { s.rad <= fr + 10.0 };
        if ready {
            (s.tw, s.th, s.trad) = (fw, fh, fr);
            (s.staged, s.from, s.reveal) = (None, (s.w, s.h), 0.0);
            s.sync_hooks();
        }
    }

    let (kw, kh) = if s.closing { (SPRING_CLOSE, SPRING_CLOSE) } else { (SPRING_W, SPRING_H) };
    spring(&mut s.w, &mut s.vw, s.tw, kw, dt);
    spring(&mut s.h, &mut s.vh, s.th, kh, dt);
    spring(&mut s.rad, &mut s.vrad, s.trad, kw, dt);
    if !matches!(s.kind, Kind::Volume | Kind::Song | Kind::Knob | Kind::Upload | Kind::Drop) {
        s.pointer_in = false;
    }
    let grow = if s.pointer_in && !s.closing && s.staged.is_none() && matches!(s.kind, Kind::Volume | Kind::Song | Kind::Knob | Kind::Drop) { HOVER_SCALE } else { 1.0 };
    spring(&mut s.hs, &mut s.hv, grow, SPRING_HOVER, dt);
    if (s.hs - grow).abs() < 0.0005 && s.hv.abs() < 0.01 {
        (s.hs, s.hv) = (grow, 0.0);
    }
    s.w = s.w.max(8.0);
    s.h = s.h.max(8.0);
    s.rad = s.rad.max(2.0);
    // the shape only fades once it is back to the dot
    let at_dot = s.w < ui::SMALL.0 + 8.0 && s.h < ui::SMALL.1 + 8.0;
    let vanish = s.closing && s.close_phase == 2 && at_dot;
    spring(&mut s.fade_in, &mut s.fade_v, if vanish { 0.0 } else { 1.0 }, SPRING_FADE, dt);

    // how far the shape has got; new content starts once it is mostly there
    s.progress = if s.closing {
        1.0
    } else if s.staged.is_some() || s.delay > 0.0 || s.queued {
        0.0
    } else {
        let miss = |now: f32, to: f32, from: f32| (now - to).abs() / (from - to).abs().max(1.0);
        let away = miss(s.w, s.tw, s.from.0).max(miss(s.h, s.th, s.from.1));
        s.reveal = s.reveal.max(1.0 - away.clamp(0.0, 1.0)); // never flickers back during the bounce
        s.reveal
    };
    if s.closing {
        s.content = (s.content - dt * CLOSE_OUT_SPEED).max(0.0);
    } else if s.progress >= ENTER_AT {
        s.content = (s.content + dt / ENTER_SECS).min(1.0);
    }
    if let Some((kind, p)) = s.prev {
        let p = p + dt / EXIT_SECS;
        s.prev = (p < 1.0).then_some((kind, p));
    }

    let want = s.want();
    approach(&mut s.disp, want, 14.0, dt, 0.0005);
    if s.kind == Kind::Upload && s.up.phase == UploadPhase::Uploading {
        upload_tick(s, dt);
    }
    if let Some((row, t)) = s.hist_flash {
        let t = t - dt * 2.5;
        s.hist_flash = (t > 0.0).then_some((row, t));
    }
    if let Some((bmp, t)) = s.hist_status.take() {
        if t - dt > 0.0 {
            s.hist_status = Some((bmp, t - dt));
        }
    }
    let mut setup_idle = true;
    if let Some(x) = s.setup.as_mut() {
        let goals = [f32::from(u8::from(x.autostart)), f32::from(u8::from(x.auto_update))];
        for (t, goal) in x.toggles.iter_mut().zip(goals) {
            approach(t, goal, 16.0, dt, 0.003);
        }
        setup_idle = x.toggles == goals;
    }
    approach(&mut s.unit, s.unit_goal, 14.0, dt, 0.002);
    let targets = s.toggle_targets();
    for (t, goal) in s.toggles.iter_mut().zip(targets) {
        approach(t, goal, 16.0, dt, 0.003);
    }

    if vanish && s.fade_in < HIDDEN_BELOW {
        finish_hide(hwnd);
        return;
    }

    let settled = !s.closing
        && (s.w - s.tw).abs() < SETTLE_DIST
        && (s.h - s.th).abs() < SETTLE_DIST
        && (s.rad - s.trad).abs() < SETTLE_DIST
        && s.vw.abs() + s.vh.abs() + s.vrad.abs() < SETTLE_SPEED
        && (s.fade_in - 1.0).abs() < 0.002
        && s.content > 0.995
        && s.prev.is_none()
        && s.staged.is_none()
        && s.delay <= 0.0
        && s.editing.is_none()
        && s.hist_flash.is_none()
        && s.hist_status.is_none()
        && setup_idle
        && !s.uploading()
        && s.icon.is_none()
        && s.disp == want
        && s.unit == s.unit_goal
        && s.hs == grow
        && s.song_prev.is_none()
        && s.lyric_swap >= 1.0
        && s.mouse.settled()
        && !((s.mouse.charging || s.mouse.others.iter().any(|o| o.charging)) && is_mouse(s.kind)) // the charging animation runs while its card is up
        && s.pin_fade == s.pin_fade_goal
        && s.toggles == targets;
    if settled {
        (s.w, s.h, s.rad) = (s.tw, s.th, s.trad);
        (s.vw, s.vh, s.vrad) = (0.0, 0.0, 0.0);
        (s.fade_in, s.fade_v, s.content) = (1.0, 0.0, 1.0);
    }
    // a pinned capsule at rest only has its visualizer moving: redraw just that
    let scrolling = s.scrolls();
    let viz = s.kind == Kind::Pinned && settled && (scrolling || media::playing() || s.viz_moving());
    if viz && s.steady_drawn {
        if s.pin_fade >= 1.0 {
            // faded out under the pointer: nothing to see, nothing to draw (a full draw follows when it comes back)
        } else if scrolling {
            draw_and_present_region(hwnd, (0.0, s.h), Some((46.0, s.w))); // the cover stays as it is
            s.lyric_drawn = s.lyric_fill;
        } else {
            let c = if s.pin_tall { 28.0 } else { ui::PINNED.1 / 2.0 };
            draw_and_present_region(hwnd, (c - 15.0, c + 15.0), Some((s.w - 48.0, s.w - 4.0)));
            lyric_redraw(hwnd, s);
        }
    } else {
        draw_and_present(hwnd);
        s.steady_drawn = settled;
        s.lyric_drawn = s.lyric_fill;
    }

    let frame_ms = if s.cfg.low_power { LOW_POWER_FRAME_MS } else { MARQUEE_FRAME_MS }; // "30 FPS" means it
    if viz {
        pace::start(hwnd, VIZ_FRAME_MS);
    } else if settled && !scrolling {
        pace::stop(); // nothing moves any more
    } else if settled {
        pace::start(hwnd, frame_ms);
    }
}

// ---- drawing ---------------------------------------------------------------

unsafe fn draw_and_present(hwnd: HWND) {
    draw(None, None);
    let s = st();
    s.surf.present(hwnd, s.win.0, s.win.1);
}

/// Redraws only the rows between `top` and `bottom` (logical, from the shape's top edge): a hover moved.
unsafe fn draw_and_present_rows(hwnd: HWND, top: f32, bottom: f32) {
    draw(Some((top, bottom)), None);
    let s = st();
    s.surf.present(hwnd, s.win.0, s.win.1);
}

/// Redraws only a band of rows and, if given, a stretch of columns (logical units from the shape's top left).
unsafe fn draw_and_present_region(hwnd: HWND, rows: (f32, f32), cols: Option<(f32, f32)>) {
    draw(Some(rows), cols);
    let s = st();
    match s.dirty {
        Some(rect) => s.surf.present_dirty(hwnd, s.win.0, s.win.1, rect),
        None => s.surf.present(hwnd, s.win.0, s.win.1),
    }
}

/// `rows`: when only some rows of a settled shape changed, their span (logical units from its top edge).
fn draw(rows: Option<(f32, f32)>, cols: Option<(f32, f32)>) {
    let s = st();
    let Some(buf) = s.surf.pixels() else { return };
    let (pw, ph) = (s.surf.pw, s.surf.ph);

    // redraw the union of where the shape is now and where it was
    let now = render::bounds(pw, ph, s.unit * s.hs, s.w, s.h);
    let rect = match (rows, s.prev_rect) {
        (Some((top, bottom)), _) => {
            let u = s.unit * s.hs;
            let (x0, x1) = match cols {
                Some((left, right)) => {
                    let edge = pw as f32 / 2.0 - s.w / 2.0 * u; // the shape's left edge in device pixels
                    (((edge + left * u).floor() as usize).saturating_sub(2).max(now.0), ((edge + right * u).ceil() as usize + 2).min(now.2))
                }
                None => (now.0, now.2),
            };
            (x0, (((ui::TOP + top) * u).floor() as usize).saturating_sub(2), x1, (((ui::TOP + bottom) * u).ceil() as usize + 2).min(ph))
        }
        (None, Some(p)) => (p.0.min(now.0), p.1.min(now.1), p.2.max(now.2), p.3.max(now.3)),
        (None, None) => now,
    };
    if rows.is_none() {
        s.prev_rect = Some(now);
    }

    // old content leaves by bending and gathering; new content (or the closing one) follows the same motions
    let mut layers = [None, None];
    if let Some((kind, p)) = s.prev {
        layers[0] = Some(Layer::leaving(kind, p));
    }
    layers[1] = Some(if s.closing { Layer::leaving(s.kind, 1.0 - s.content) } else { Layer::entering(s.kind, s.content) });
    let shows = |k: Kind| s.kind == k || matches!(s.prev, Some((pk, _)) if pk == k);
    let scene = Scene {
        pal: s.pal,
        unit: s.unit * s.hs,
        w: s.w,
        h: s.h,
        radius: s.rad,
        alpha: s.fade_in.clamp(0.0, 1.0) * if s.fades() { 1.0 - s.pin_fade } else { 1.0 },
        layers,
        marq: s.marq,
        volume: if s.muted { 0.0 } else { s.disp },
        muted: s.muted,
        icon: s.icon.map(|(icon, t)| (icon, (t / ICON_SECS).min(1.0))),
        bell: s.notice.is_some() && s.notice_icon == 0,
        frame: s.notice.is_some() && s.notice_icon == 1,
        check: s.notice.is_some() && s.notice_icon == 2,
        drop_hint: s.drop_hint.as_ref(),
        pin: shows(Kind::Pinned).then_some({
            render::PinView { song: s.song.as_deref(), prev: s.song_prev.as_deref(), swap: s.song_swap, bars: s.viz, lyric: s.lyric.as_deref(), lyric_old: s.lyric_old.as_deref(), lyric_swap: s.lyric_swap, lyric_fill: s.lyric_fill, tall: s.pin_tall || s.h > 50.0 }
        }),
        sub: s.sub(),
        song: if s.kind == Kind::Knob { s.song.as_deref() } else { s.shown_song() },
        upload: shows(Kind::Upload).then_some(UploadView {
            phase: s.up.phase,
            progress: s.up.progress,
            name: s.up.name.as_ref(),
            info: s.up.info.as_ref(),
            msg: s.up.msg.as_ref(),
            line: s.up.line.as_ref(),
        }),
        history: shows(Kind::History).then(|| HistoryView {
            title: s.hist_title.as_ref(),
            status: s.hist_status.as_ref().map(|(b, _)| b),
            empty: s.hist_empty.as_ref(),
            rows: &s.hist_rows,
            hover: s.hist_hover,
            flash: s.hist_flash,
            scroll: s.hist_scroll,
            total: s.hist.len(),
        }),
        mouse: (shows(Kind::MouseDpi) || shows(Kind::MouseBattery) || shows(Kind::MouseLink)).then(|| s.mouse.view()),
        setup: s.setup.as_ref().filter(|_| shows(Kind::Setup)).map(|x| SetupView { texts: &s.setup_texts, values: &s.setup_values, toggles: x.toggles, hover: x.hover }),
        settings: shows(Kind::Settings).then(|| SettingsView {
            labels: &s.labels,
            toggles: s.toggles,
            hover: s.hover,
            step: s.cfg.step,
            notify_volume: s.cfg.ntfy_volume,
            values: &s.values,
            editing: s.editing,
            sound_set: !s.cfg.ntfy_sound.is_empty(),
        }),
    };
    render::render(buf, pw, ph, rect, &scene);
    s.dirty = (rows.is_some()).then_some(rect);
}

// ---- window proc -----------------------------------------------------------

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wp.0 == 2 => toggle_settings(hwnd),
        WM_HOTKEY if wp.0 == 3 => toggle_history(hwnd),
        WM_HOTKEY => open_volume(hwnd, true),
        WM_APP_OPEN => open_volume(hwnd, false),
        WM_APP_WHEEL => wheel(hwnd, wp.0 as isize as i32),
        WM_APP_VOLKEY => volume_key(hwnd, wp.0),
        WM_APP_MEDIA => media_button(hwnd, wp.0),
        WM_APP_PLAYSTATE => play_state(hwnd),
        WM_APP_SONG => song_changed(hwnd),
        WM_APP_DEVICE => device_changed(hwnd),
        WM_APP_NTFY => push_notice(hwnd),
        WM_APP_MOUSE if wp.0 == mouse::EV_OTHER => other_event(hwnd, (lp.0 & 0xff) as usize, lp.0 >> 8),
        WM_APP_MOUSE => mouse_event(hwnd, wp.0),
        WM_DEVICECHANGE => {
            if matches!(wp.0, 0x0007 | 0x8000 | 0x8004) {
                mouse::rescan(); // DBT_DEVNODES_CHANGED / arrival / removal: a receiver or cable may have come or gone
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_APP_KEY => settings_key(hwnd, wp.0 as u32),
        WM_APP_UPLOAD => upload_event(hwnd),
        WM_COPYDATA => {
            let cds = &*(lp.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT);
            if cds.dwData == upload::COPY_MAGIC {
                let paths = upload::decode_paths(cds.lpData as *const u16, cds.cbData as usize / 2);
                upload::enqueue(paths, hwnd.0 as isize, WM_APP_UPLOAD);
            }
            return LRESULT(1);
        }
        WM_APP_CLICK => panel_click(hwnd, POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }),
        WM_APP_SNIP => {
            let s = st();
            if s.cfg.screenshot && s.setup.is_none() {
                snip::start(hwnd, s.strings().snip_hint);
            }
        }
        WM_APP_SNIP_CANCEL => snip::cancel(),
        WM_APP_PASTED => snip_pasted(hwnd),
        WM_APP_SNIP_DONE => snip_taken(hwnd, wp.0 as i32, lp.0 as i32),
        WM_APP_DRAGCHECK => drag_started(hwnd, POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 }),
        WM_APP_DRAGEND => {
            let _ = KillTimer(hwnd, T_DRAG);
            if st().kind == Kind::Drop && st().live() {
                SetTimer(hwnd, T_DROP_END, 400, None); // a drop on the capsule still arrives in this moment
            }
        }
        crate::dropzone::WM_APP_DROPOVER => {
            st().pointer_in = wp.0 == 1;
            frames(hwnd);
        }
        WM_APP_HOVER => {
            let s = st();
            s.pointer_in = wp.0 != 0;
            frames(hwnd);
            if !s.pointer_in && s.hide_armed && !s.closing {
                hide_after(hwnd, Some(s.cfg.hide_ms)); // the full delay starts when the pointer leaves
            }
        }
        WM_APP_MOVE => {
            move_handled();
            panel_hover(hwnd);
        }
        WM_APP_DISMISS => {
            if panel_open() && st().setup.is_none() {
                begin_close(hwnd);
            }
        }
        tray::WM_TRAY => match (lp.0 & 0xFFFF) as u32 {
            WM_LBUTTONUP => toggle_settings(hwnd),
            WM_RBUTTONUP => match tray::menu(hwnd, st().strings(), install::running_installed()) {
                tray::CMD_SETTINGS => toggle_settings(hwnd),
                tray::CMD_HISTORY => toggle_history(hwnd),
                tray::CMD_QUIT => {
                    let _ = DestroyWindow(hwnd);
                }
                tray::CMD_UNINSTALL => {
                    // a separate process asks, closes this one and cleans up
                    if let Ok(exe) = std::env::current_exe() {
                        crate::install::launch_with(&exe, "--uninstall");
                    }
                }
                _ => {}
            },
            _ => {}
        },
        _ if msg == tray::taskbar_created() => {
            tray::add(hwnd);
            let _ = KillTimer(hwnd, T_TRAY);
        }
        WM_APP_WARM => warm_up(hwnd),
        WM_APP_PIN => pin_refresh(hwnd),
        WM_APP_UPDATE => update_ready(hwnd, wp.0),
        WM_APP_FADE => {
            let s = st();
            if s.fades() {
                s.pin_fade_goal = (wp.0 as f32 / FADE_STEPS as f32).clamp(0.0, 1.0);
                s.steady_drawn = false; // the whole capsule changes opacity
                frames(hwnd);
            }
        }
        pace::WM_APP_FRAME => {
            pace::taken();
            tick(hwnd);
        }
        WM_TIMER => match wp.0 {
            T_TRAY if tray::add(hwnd) => {
                let _ = KillTimer(hwnd, T_TRAY);
            }
            T_DRAG => drag_poll(hwnd),
            T_PIN_POLL => pin_poll(hwnd),
            T_LYRIC => lyric_tick(hwnd),
            T_UPDATE => update_tick(hwnd),
            T_UPDATE_APPLY => update_apply(hwnd),
            T_DROP_END => {
                let _ = KillTimer(hwnd, T_DROP_END);
                if st().kind == Kind::Drop {
                    begin_close(hwnd);
                }
            }
            T_HIDE if st().pointer_in => {} // the pointer is on it: stay
            T_HIDE => begin_close(hwnd),
            _ => {}
        },
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
            return LRESULT(1);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            tray::remove(hwnd);
            PostQuitMessage(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}
