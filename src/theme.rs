// Colors for the dark and light look. The setting is "auto" (follow Windows), "dark" or "light".

use windows::core::w;
use windows::Win32::System::Registry::*;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: [f32; 3],   // the capsule itself
    pub fg: [f32; 3],   // strong content: bar fill, icons, indicator
    pub text: [f32; 3], // body text
    pub dim: [f32; 3],  // secondary text
    pub red: [f32; 3],
    pub light: bool,
}

pub const DARK: Palette = Palette {
    bg: [0.0, 0.0, 0.0],
    fg: [1.0, 1.0, 1.0],
    text: [0.92, 0.94, 0.97],
    dim: [0.64, 0.64, 0.68],
    red: [1.0, 0.45, 0.45],
    light: false,
};

pub const LIGHT: Palette = Palette {
    bg: [0.97, 0.97, 0.98],
    fg: [0.07, 0.07, 0.09],
    text: [0.10, 0.10, 0.12],
    dim: [0.42, 0.42, 0.47],
    red: [0.80, 0.16, 0.16],
    light: true,
};

impl Palette {
    /// A color `t` of the way from the background to the foreground (tracks, fields, inactive things).
    pub fn mix(&self, t: f32) -> [f32; 3] {
        std::array::from_fn(|i| self.bg[i] + (self.fg[i] - self.bg[i]) * t)
    }
}

/// Windows' "apps" theme: true when it is set to light.
fn system_is_light() -> bool {
    unsafe {
        let mut key = HKEY::default();
        let path = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        if RegOpenKeyExW(HKEY_CURRENT_USER, path, 0, KEY_QUERY_VALUE, &mut key).is_err() {
            return false;
        }
        let mut data = 0u32;
        let mut size = 4u32;
        let ok = RegQueryValueExW(key, w!("AppsUseLightTheme"), None, None, Some(&mut data as *mut u32 as *mut u8), Some(&mut size)).is_ok();
        let _ = RegCloseKey(key);
        ok && data != 0
    }
}

pub fn resolve(setting: &str) -> Palette {
    match setting {
        "dark" => DARK,
        "light" => LIGHT,
        _ => if system_is_light() { LIGHT } else { DARK },
    }
}
