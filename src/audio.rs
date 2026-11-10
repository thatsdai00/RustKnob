// Default output device volume + the detent click played on every volume step.

use std::f32::consts::TAU;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

const RATE: u32 = 44100;
const CLICK_LEVEL: f32 = 0.10; // wanted peak at the speakers, regardless of master volume

pub unsafe fn default_endpoint() -> Option<IAudioEndpointVolume> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
    device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
}

pub struct Clicker {
    base: Vec<f32>,
    // PlaySound(SND_ASYNC) reads the buffer while it plays, so keep the last few alive
    bufs: [Vec<u8>; 4],
    next: usize,
}

impl Clicker {
    pub fn new() -> Clicker {
        Clicker { base: synth_click(), bufs: Default::default(), next: 0 }
    }

    /// The system master volume scales everything we play; undo it so the click keeps one loudness.
    pub unsafe fn play(&mut self, ep: &IAudioEndpointVolume) {
        let gain = compensate(ep, CLICK_LEVEL);
        self.next = (self.next + 1) % self.bufs.len();
        let buf = &mut self.bufs[self.next];
        *buf = wav(&self.base, gain);
        let _ = PlaySoundW(PCWSTR(buf.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// Gain that makes a sound peak at `level` (0..1) at the speakers whatever the master volume is.
unsafe fn compensate(ep: &IAudioEndpointVolume, level: f32) -> f32 {
    let db = ep.GetMasterVolumeLevel().unwrap_or(0.0);
    let master = 10f32.powf(db / 20.0).max(0.001);
    (level / master).min(1.0)
}

/// Notification sound at `level` percent of the system volume: the chosen mp3 / wav (played
/// through MCI), or the built-in chime. The level is relative to the master volume on purpose:
/// a notification must never be louder than what the user has set.
pub unsafe fn play_notification(path: &str, level: u32) {
    if !path.is_empty() && std::path::Path::new(path).is_file() {
        let _ = mciSendStringW(w!("close dkns"), None, HWND::default());
        let open = HSTRING::from(format!("open \"{path}\" alias dkns"));
        if mciSendStringW(&open, None, HWND::default()) == 0 {
            let volume = HSTRING::from(format!("setaudio dkns volume to {}", level.clamp(1, 100) * 10));
            let _ = mciSendStringW(&volume, None, HWND::default());
            let _ = mciSendStringW(w!("play dkns"), None, HWND::default());
            return;
        }
    }
    play_chime(level);
}

const CHIME_PEAK: f32 = 0.5; // chime peak at 100 %, relative to full scale
static CHIME_BUF: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

/// Built-in chime: two soft bell notes (E5, then A5).
unsafe fn play_chime(level: u32) {
    let n = (RATE as f32 * 0.7) as usize;
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let note = |t0: f32, freq: f32| {
                let u = t - t0;
                if u < 0.0 {
                    return 0.0;
                }
                let attack = (u / 0.006).min(1.0);
                // fundamental plus a quiet octave: a small bell
                attack * (-u * 7.0).exp() * ((u * freq * TAU).sin() + 0.25 * (u * freq * 2.0 * TAU).sin())
            };
            0.8 * (note(0.0, 659.25) + 0.9 * note(0.16, 880.0))
        })
        .collect();
    let peak = samples.iter().fold(1e-6f32, |m, x| m.max(x.abs()));
    let normalized: Vec<f32> = samples.iter().map(|x| x / peak).collect();
    let gain = CHIME_PEAK * level.clamp(1, 100) as f32 / 100.0;
    if let Ok(mut buf) = CHIME_BUF.lock() {
        *buf = wav(&normalized, gain);
        let _ = PlaySoundW(PCWSTR(buf.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// Two quick ticks (sharp, then softer) over a short low body: a ratchet detent.
fn synth_click() -> Vec<f32> {
    let n = (RATE as f32 * 0.030) as usize;
    let mut seed = 0x9E37_79B9u32;
    let mut prev_noise = 0.0f32;
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (seed >> 16) as f32 / 32768.0 - 1.0;
            let hp = noise - prev_noise; // high-passed noise = crisp transient
            prev_noise = noise;
            let tick = |t0: f32, amp: f32, freq: f32| {
                let u = t - t0;
                if u < 0.0 { 0.0 } else { amp * (hp * 0.5 + (u * freq * TAU).sin()) * (-u * 900.0).exp() }
            };
            let body = (t * 230.0 * TAU).sin() * (-t * 140.0).exp() * 0.35;
            tick(0.0, 1.0, 2600.0) + tick(0.0075, 0.55, 1900.0) + body
        })
        .collect();
    let peak = out.iter().fold(1e-6f32, |m, x| m.max(x.abs()));
    out.iter_mut().for_each(|x| *x /= peak);
    out
}

/// 16-bit mono PCM WAV in memory.
fn wav(samples: &[f32], gain: f32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&(((s * gain).clamp(-1.0, 1.0) * 32000.0) as i16).to_le_bytes());
    }
    w
}
