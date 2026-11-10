// rustknob.ini: "key=value" lines, ';' or '#' start a comment. The settings capsule writes it back.

use std::path::PathBuf;

use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub const STEPS: [u32; 5] = [1, 2, 3, 5, 10]; // wheel step choices, percent
pub const NOTIFY_VOLUMES: [u32; 7] = [5, 10, 20, 30, 50, 75, 100]; // notification sound level choices, percent

#[derive(Clone)]
pub struct Config {
    pub hotkey: String,
    pub settings_hotkey: String,
    pub history_hotkey: String,
    pub step: u32, // volume change per wheel notch / volume key, percent
    pub hide_ms: u32,
    pub notify_song: bool,
    pub notify_device: bool,
    pub volume_keys: bool,
    pub click_sound: bool,
    pub low_power: bool, // 30 fps instead of 60
    pub upload_sound: bool, // chime when an upload finishes
    pub screenshot: bool,   // Win+Shift+S opens RustKnob's own screenshot overlay
    pub pin_media: bool,    // the capsule stays up while music plays: cover, title and artist, visualizer
    pub pin_fullscreen: bool, // ... also over full-screen apps
    pub pin_lyrics: bool,     // ... with the synced lyrics (LiriQo, LRCLIB) in place of the title
    pub auto_update: bool,    // fetch new releases from GitHub by itself
    pub ntfy_server: String,
    pub ntfy_topic: String, // empty = ntfy off
    pub ntfy_token: String, // optional access token
    pub ntfy_sound: String, // mp3 / wav played for a notification; empty = built-in chime
    pub lyric_ahead_ms: i32, // lyrics show this much early (negative: late); the player clock is already right for Apple Music
    pub ntfy_volume: u32,   // notification sound level, percent (relative to the system volume)
    pub language: String,   // "auto" | "en" | "tr"
    pub theme: String,      // "auto" | "dark" | "light"
}

impl Default for Config {
    fn default() -> Config {
        Config {
            hotkey: "ctrl+alt+k".into(),
            settings_hotkey: "ctrl+alt+shift+k".into(),
            history_hotkey: "ctrl+alt+h".into(),
            step: 2,
            hide_ms: 1500,
            notify_song: true,
            notify_device: true,
            volume_keys: true,
            click_sound: true,
            low_power: false,
            upload_sound: false,
            screenshot: true,
            pin_media: false,
            pin_fullscreen: false,
            pin_lyrics: true,
            auto_update: true,
            ntfy_server: "https://ntfy.sh".into(),
            ntfy_topic: String::new(),
            ntfy_token: String::new(),
            ntfy_sound: String::new(),
            lyric_ahead_ms: 1100,
            ntfy_volume: 20,
            language: "auto".into(),
            theme: "auto".into(),
        }
    }
}

impl Config {
    /// auto → English → Türkçe → auto
    pub fn cycle_language(&mut self) {
        self.language = match self.language.as_str() { "auto" => "en", "en" => "tr", _ => "auto" }.to_string();
    }

    /// auto → dark → light → auto
    pub fn cycle_theme(&mut self) {
        self.theme = match self.theme.as_str() { "auto" => "dark", "dark" => "light", _ => "auto" }.to_string();
    }

    pub fn load() -> Config {
        let mut cfg = Config::default();
        let Some(text) = find_ini().and_then(|p| std::fs::read_to_string(p).ok()) else { return cfg };
        for line in text.trim_start_matches('\u{feff}').lines().map(str::trim).filter(|l| !l.starts_with([';', '#'])) {
            let Some((key, val)) = line.split_once('=') else { continue };
            let (key, val) = (key.trim().to_lowercase(), val.trim());
            let flag = val != "0";
            match key.as_str() {
                "hotkey" => cfg.hotkey = val.to_lowercase(),
                "settings_hotkey" => cfg.settings_hotkey = val.to_lowercase(),
                "history_hotkey" => cfg.history_hotkey = val.to_lowercase(),
                "step" => cfg.step = val.parse().unwrap_or(cfg.step).clamp(1, 25),
                "hide_ms" => cfg.hide_ms = val.parse().unwrap_or(cfg.hide_ms).clamp(200, 10000),
                "notify_song" | "title" => cfg.notify_song = flag,
                "notify_device" => cfg.notify_device = flag,
                "volume_keys" => cfg.volume_keys = flag,
                "click_sound" => cfg.click_sound = flag,
                "low_power" => cfg.low_power = flag,
                "upload_sound" => cfg.upload_sound = flag,
                "screenshot" => cfg.screenshot = flag,
                "pin_media" => cfg.pin_media = flag,
                "pin_fullscreen" => cfg.pin_fullscreen = flag,
                "pin_lyrics" => cfg.pin_lyrics = flag,
                "auto_update" => cfg.auto_update = flag,
                "ntfy_server" if !val.is_empty() => cfg.ntfy_server = val.to_string(),
                "ntfy_topic" => cfg.ntfy_topic = val.to_string(),
                "ntfy_token" => cfg.ntfy_token = val.to_string(),
                "ntfy_sound" => cfg.ntfy_sound = val.to_string(),
                "language" if matches!(val, "auto" | "en" | "tr") => cfg.language = val.to_string(),
                "theme" if matches!(val, "auto" | "dark" | "light") => cfg.theme = val.to_string(),
                "lyric_ahead_ms" => cfg.lyric_ahead_ms = val.parse().unwrap_or(cfg.lyric_ahead_ms).clamp(-3000, 3000),
                "ntfy_volume" => cfg.ntfy_volume = val.parse().unwrap_or(cfg.ntfy_volume).clamp(1, 100),
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) {
        let b = |v: bool| u8::from(v);
        let text = format!(
            "; RustKnob ayarlari (ayar kapsulunden de degisir)\n\
             ; Ornek kisayollar: ctrl+alt+k | ctrl+alt+shift+k | f13\n\
             hotkey={}\nsettings_hotkey={}\nhistory_hotkey={}\n\
             ; tekerlek / ses tusu basina ses adimi (yuzde)\nstep={}\n\
             ; tekerlek birakildiktan sonra kapanma suresi (ms)\nhide_ms={}\n\
             notify_song={}\nnotify_device={}\nvolume_keys={}\nclick_sound={}\nlow_power={}\nupload_sound={}\nscreenshot={}\npin_media={}\npin_fullscreen={}\npin_lyrics={}\nauto_update={}\n\
             ; ntfy bildirimleri: topic bos birakilirsa kapali. ntfy_sound: mp3/wav yolu (bos = yerlesik cin sesi).\n\
             ntfy_server={}\nntfy_topic={}\nntfy_token={}\nntfy_sound={}\nntfy_volume={}\n\
             ; sozler sesten ne kadar once gosterilsin (ms; gecikiyorsa artir, erken geliyorsa eksi yap)\nlyric_ahead_ms={}\n\
             ; language: auto | en | tr      theme: auto | dark | light\nlanguage={}\ntheme={}\n",
            self.hotkey,
            self.settings_hotkey,
            self.history_hotkey,
            self.step,
            self.hide_ms,
            b(self.notify_song),
            b(self.notify_device),
            b(self.volume_keys),
            b(self.click_sound),
            b(self.low_power),
            b(self.upload_sound),
            b(self.screenshot),
            b(self.pin_media),
            b(self.pin_fullscreen),
            b(self.pin_lyrics),
            b(self.auto_update),
            self.ntfy_server,
            self.ntfy_topic,
            self.ntfy_token,
            self.ntfy_sound,
            self.ntfy_volume,
            self.lyric_ahead_ms,
            self.language,
            self.theme,
        );
        let path = find_ini().unwrap_or_else(|| {
            let dir = appdata_dir();
            let _ = std::fs::create_dir_all(&dir);
            dir.join("rustknob.ini")
        });
        let _ = std::fs::write(path, text);
    }

    /// Wheel / key volume step as a 0..1 fraction.
    pub fn step_fraction(&self) -> f32 {
        self.step as f32 / 100.0
    }
}

/// "ctrl+alt+k", "f13", ... -> (modifiers, virtual key)
pub fn parse_hotkey(spec: &str, default_vk: u32) -> (HOT_KEY_MODIFIERS, u32) {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk = default_vk;
    for tok in spec.split('+').map(str::trim) {
        match tok {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            t if t.len() == 1 => vk = t.as_bytes()[0].to_ascii_uppercase() as u32,
            t => {
                if let Some(n) = t.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
                    vk = VK_F1.0 as u32 + n - 1;
                }
            }
        }
    }
    (mods, vk)
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default()
}

/// Where settings live when there is no file beside the exe: %APPDATA%\\RustKnob.
fn appdata_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("RustKnob")
}

/// Next to the exe (portable / installed copy), two levels up (cargo's target/release during
/// development), or in %APPDATA%. A settings file from the project's old name is renamed on the way.
fn find_ini() -> Option<PathBuf> {
    let dir = exe_dir();
    let up2 = dir.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    for d in [Some(dir), up2, Some(appdata_dir())].into_iter().flatten() {
        let (new, old) = (d.join("rustknob.ini"), d.join("digitalknob.ini"));
        if new.exists() {
            return Some(new);
        }
        if old.exists() && std::fs::rename(&old, &new).is_ok() {
            return Some(new);
        }
    }
    // %APPDATA%\DigitalKnob\digitalknob.ini from before the rename
    let legacy = PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("DigitalKnob").join("digitalknob.ini");
    if legacy.exists() {
        let target = appdata_dir().join("rustknob.ini");
        let _ = std::fs::create_dir_all(appdata_dir());
        if std::fs::copy(&legacy, &target).is_ok() {
            return Some(target);
        }
    }
    None
}
