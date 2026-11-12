// Capsule layout shared by the renderer and the click handling. Units are logical;
// (tx, ty) are measured from the capsule's top-left corner.

pub const TOP: f32 = 12.0; // gap between the screen edge and the capsule
pub const WIN_W: f32 = 460.0;
pub const WIN_H: f32 = 690.0;
pub const SHADOW: f32 = 22.0; // how far the shadow reaches around the capsule

// capsule sizes per content: one row is as big as the volume capsule, two rows share one size
pub const TWO_ROWS: (f32, f32) = (340.0, 72.0);
pub const SMALL: (f32, f32) = (26.0, 26.0); // what it grows from / shrinks to
pub const VOLUME_W: f32 = 300.0;
pub const VOLUME_H: f32 = 48.0;
pub const VOLUME_H_SUB: f32 = 72.0; // with the song / device line underneath
pub const SONG: (f32, f32) = TWO_ROWS;
pub const UPLOAD: (f32, f32) = TWO_ROWS;
pub const PINNED: (f32, f32) = (VOLUME_W, VOLUME_H); // cover | title and artist | visualizer
pub const PINNED_TALL: (f32, f32) = (340.0, 80.0); // ... with the current lyric line in a row of its own below
pub const DROP: (f32, f32) = (VOLUME_W, VOLUME_H); // shown while a file is dragged: drop it here
// the mouse's capsules are as big as the volume capsule (with its line underneath)
pub const MOUSE_DPI: (f32, f32) = (VOLUME_W, VOLUME_H_SUB); // picture | the DPI | stage markers
pub const MOUSE_BATTERY: (f32, f32) = (VOLUME_W, VOLUME_H_SUB); // battery | "battery low" | percent
pub const MOUSE_LINK: (f32, f32) = (VOLUME_W, VOLUME_H_SUB); // picture | name and "connected" | battery

pub const KNOB: (f32, f32) = (252.0, 252.0); // the capsule morphs into this disc
pub const KNOB_RADIUS: f32 = 126.0;
pub const CAPSULE_RADIUS: f32 = 34.0;

// ---- settings panel --------------------------------------------------------

pub const ROWS: usize = 24;
pub const ROW_H: f32 = 26.0;
pub const PAD: f32 = 12.0;
pub const SETTINGS: (f32, f32) = (380.0, PAD * 2.0 + ROWS as f32 * ROW_H);

pub const TOGGLES: usize = 14; // rows 0..13 are on/off switches
pub const TOGGLE_AUTOSTART: usize = 13;
pub const ROW_STEP: usize = 14;
pub const ROW_TOPIC: usize = 15; // ntfy topic, server and token are text fields
pub const ROW_TOKEN: usize = 17;
pub const ROW_SOUND: usize = 18; // notification sound file picker
pub const ROW_VOLUME: usize = 19; // notification sound level
pub const ROW_LANG: usize = 20; // click cycles auto / English / Türkçe
pub const ROW_THEME: usize = 21; // click cycles auto / dark / light
pub const ROW_UPDATE: usize = 22; // click: look for a new release now (and install it)
pub const ROW_QUIT: usize = 23;

// stepper control geometry, measured from the panel's right edge
pub const STEP_MINUS_X: f32 = 86.0;
pub const STEP_PLUS_X: f32 = 32.0;
pub const STEP_VALUE_X: f32 = 59.0;

// text fields and the sound picker show their value in this box
pub const VALUE_LEFT: f32 = 150.0;
pub const VALUE_VIEW: f32 = SETTINGS.0 - VALUE_LEFT - 24.0;
pub const SOUND_RESET_W: f32 = 40.0; // the "x" at the sound row's right end

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Toggle(usize),
    StepMinus,
    StepPlus,
    VolMinus,
    VolPlus,
    Text(usize), // 0 topic, 1 server, 2 token
    Sound,
    SoundReset,
    Language,
    Theme,
    CheckUpdate,
    Quit,
}

/// Settings row under a vertical offset from the panel top.
pub fn row_at(ty: f32) -> Option<usize> {
    let r = ((ty - PAD) / ROW_H).floor();
    (r >= 0.0 && (r as usize) < ROWS && ty < SETTINGS.1 - PAD + 0.1).then_some(r as usize)
}

pub fn hit(tx: f32, ty: f32) -> Option<Hit> {
    let row = row_at(ty)?;
    let from_right = SETTINGS.0 - tx;
    Some(match row {
        r if r < TOGGLES => Hit::Toggle(r),
        ROW_STEP | ROW_VOLUME => {
            let step = row == ROW_STEP;
            if (from_right - STEP_MINUS_X).abs() < 14.0 {
                if step { Hit::StepMinus } else { Hit::VolMinus }
            } else if (from_right - STEP_PLUS_X).abs() < 14.0 {
                if step { Hit::StepPlus } else { Hit::VolPlus }
            } else {
                return None;
            }
        }
        ROW_TOPIC..=ROW_TOKEN => Hit::Text(row - ROW_TOPIC),
        ROW_SOUND => {
            if from_right < SOUND_RESET_W {
                Hit::SoundReset
            } else {
                Hit::Sound
            }
        }
        ROW_LANG => Hit::Language,
        ROW_THEME => Hit::Theme,
        ROW_UPDATE => Hit::CheckUpdate,
        _ => Hit::Quit,
    })
}

/// Index into the settings value bitmaps (topic, server, token, sound, language, theme) for a row.
pub fn value_index(row: usize) -> Option<usize> {
    match row {
        ROW_TOPIC..=ROW_SOUND => Some(row - ROW_TOPIC),
        ROW_LANG => Some(4),
        ROW_THEME => Some(5),
        _ => None,
    }
}

// ---- link history panel ----------------------------------------------------

pub const HEAD_H: f32 = 34.0;
pub const HIST_ROW_H: f32 = 40.0;
pub const HIST_ROWS: usize = 7;
pub const HISTORY: (f32, f32) = (400.0, PAD * 2.0 + HEAD_H + HIST_ROWS as f32 * HIST_ROW_H);
const HIST_DELETE_W: f32 = 38.0; // the "x" at a row's right end

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HistHit {
    Copy(usize),
    Delete(usize),
}

pub fn hist_row_at(ty: f32) -> Option<usize> {
    let r = ((ty - PAD - HEAD_H) / HIST_ROW_H).floor();
    (r >= 0.0 && (r as usize) < HIST_ROWS).then_some(r as usize)
}

pub fn hist_hit(tx: f32, ty: f32) -> Option<HistHit> {
    let row = hist_row_at(ty)?;
    Some(if HISTORY.0 - tx < HIST_DELETE_W { HistHit::Delete(row) } else { HistHit::Copy(row) })
}

// ---- first-run setup panel -------------------------------------------------

pub const SETUP: (f32, f32) = (380.0, 244.0);
pub const SETUP_HEAD: f32 = 64.0; // title and subtitle
pub const SETUP_ROW_H: f32 = 28.0; // rows: autostart, update automatically, language, theme
pub const SETUP_BTN_Y: f32 = SETUP_HEAD + 4.0 * SETUP_ROW_H + 12.0;
pub const SETUP_BTN_H: f32 = 36.0;
pub const SETUP_INSTALL_X: (f32, f32) = (20.0, 200.0);
pub const SETUP_PORTABLE_X: (f32, f32) = (210.0, 360.0);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SetupHit {
    Autostart,
    AutoUpdate,
    Language,
    Theme,
    Install,
    Portable,
}

/// Setup element index (0..3 rows, 4 install, 5 portable) under a point, for hover and clicks.
pub fn setup_element(tx: f32, ty: f32) -> Option<usize> {
    if (SETUP_BTN_Y..SETUP_BTN_Y + SETUP_BTN_H).contains(&ty) {
        if (SETUP_INSTALL_X.0..SETUP_INSTALL_X.1).contains(&tx) {
            return Some(4);
        }
        if (SETUP_PORTABLE_X.0..SETUP_PORTABLE_X.1).contains(&tx) {
            return Some(5);
        }
        return None;
    }
    let r = ((ty - SETUP_HEAD) / SETUP_ROW_H).floor();
    ((0.0..4.0).contains(&r)).then_some(r as usize)
}

pub fn setup_hit(tx: f32, ty: f32) -> Option<SetupHit> {
    Some(match setup_element(tx, ty)? {
        0 => SetupHit::Autostart,
        1 => SetupHit::AutoUpdate,
        2 => SetupHit::Language,
        3 => SetupHit::Theme,
        4 => SetupHit::Install,
        _ => SetupHit::Portable,
    })
}
