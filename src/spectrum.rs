// What the pinned capsule's visualizer listens to: the sound that goes to the speakers, split into five bands
// (bass up to treble). A WASAPI loopback capture feeds a short window of samples to a few Goertzel filters per
// band (a handful of multiplications per sample: no FFT, no allocation). The thread exists only while the
// pinned capsule needs it: it sleeps on a condition variable otherwise and the capture is closed.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Condvar, Mutex, Once};
use std::time::Duration;

use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

pub const BANDS: usize = 5;
const WINDOW: usize = 2048; // samples looked at: about 43 ms
const POLL_MS: u64 = 24;
/// Frequencies (Hz) checked inside each band.
const BINS: [[f32; 2]; BANDS] = [[55.0, 95.0], [180.0, 320.0], [650.0, 1100.0], [2000.0, 3600.0], [6000.0, 10500.0]];
/// Higher bands carry less energy in music: lift them a little so all five bars move.
const TILT_DB: [f32; BANDS] = [0.0, 3.0, 7.0, 11.0, 15.0];

static LEVELS: [AtomicU32; BANDS] = [const { AtomicU32::new(0) }; BANDS]; // f32 bits, 0..1
static GATE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static SPAWN: Once = Once::new();

/// Band levels 0..1 as of the last look at the sound.
pub fn levels() -> [f32; BANDS] {
    std::array::from_fn(|i| f32::from_bits(LEVELS[i].load(Relaxed)))
}

/// Starts listening (idempotent).
pub fn start() {
    SPAWN.call_once(|| {
        std::thread::Builder::new().name("spectrum".into()).spawn(run).ok();
    });
    if let Ok(mut on) = GATE.0.lock() {
        if !*on {
            *on = true;
            GATE.1.notify_one();
        }
    }
}

/// Stops listening and lets go of the audio device.
pub fn stop() {
    if let Ok(mut on) = GATE.0.lock() {
        *on = false;
    }
}

fn wanted() -> bool {
    GATE.0.lock().map(|g| *g).unwrap_or(false)
}

fn publish(values: [f32; BANDS]) {
    for (slot, v) in LEVELS.iter().zip(values) {
        slot.store(v.to_bits(), Relaxed);
    }
}

fn run() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    loop {
        {
            let Ok(mut on) = GATE.0.lock() else { return };
            while !*on {
                publish([0.0; BANDS]);
                on = match GATE.1.wait(on) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        }
        if unsafe { capture() }.is_err() {
            std::thread::sleep(Duration::from_millis(600)); // no device right now (or it changed): try again
        }
    }
}

/// How to read the samples the device hands over.
struct Format {
    channels: usize,
    float: bool,
    bytes: usize, // per sample: 2, 3 or 4 (float is always 4)
    rate: f32,
}

impl Format {
    /// Sample `i` of a buffer, -1..1.
    unsafe fn sample(&self, data: *const u8, i: usize) -> f32 {
        let p = data.add(i * self.bytes);
        match (self.float, self.bytes) {
            (true, _) => (p as *const f32).read_unaligned(),
            (false, 2) => f32::from((p as *const i16).read_unaligned()) / 32768.0,
            (false, 3) => (i32::from_le_bytes([0, *p, *p.add(1), *p.add(2)]) >> 8) as f32 / 8_388_608.0,
            _ => (p as *const i32).read_unaligned() as f32 / 2_147_483_648.0,
        }
    }
}

unsafe fn capture() -> Result<()> {
    let device_now = crate::device::RENDER_CHANGES.load(Relaxed); // a new default device ends this capture
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let mix = client.GetMixFormat()?;
    let format = {
        let m = &*mix;
        let (tag, bits) = (m.wFormatTag, m.wBitsPerSample);
        let channels = usize::from(m.nChannels).max(1);
        // the container size: a 24-bit sample may sit in 4 bytes
        let bytes = (usize::from(m.nBlockAlign) / channels).max(usize::from(bits / 8));
        Format { channels, float: tag == 3 || (tag == 0xFFFE && bits == 32), bytes, rate: m.nSamplesPerSec as f32 }
    };
    if !format.float && !(2..=4).contains(&format.bytes) || format.float && format.bytes != 4 {
        CoTaskMemFree(Some(mix as *const _));
        return Err(windows::Win32::Foundation::E_FAIL.into()); // a format this cannot read
    }
    let started = client
        .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000, 0, mix, None)
        .and_then(|_| client.GetService::<IAudioCaptureClient>());
    CoTaskMemFree(Some(mix as *const _));
    let capture = started?;
    client.Start()?;
    let result = listen(&capture, &format, device_now);
    let _ = client.Stop();
    result
}

/// Reads the capture until the visualizer is no longer wanted (or the default device changes).
unsafe fn listen(capture: &IAudioCaptureClient, format: &Format, device_now: u32) -> Result<()> {

    let window: Vec<f32> = (0..WINDOW).map(|i| 0.5 - 0.5 * (TAU * i as f32 / WINDOW as f32).cos()).collect(); // Hann
    let coefficients: [[f32; 2]; BANDS] = std::array::from_fn(|b| std::array::from_fn(|k| 2.0 * (TAU * BINS[b][k] / format.rate).cos()));
    let mut ring = vec![0.0f32; WINDOW];
    let mut frame = vec![0.0f32; WINDOW];
    let mut at = 0usize;
    let mut shown = [0.0f32; BANDS];
    loop {
        if !wanted() || crate::device::RENDER_CHANGES.load(Relaxed) != device_now {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
        let mut fresh = false;
        loop {
            let frames = capture.GetNextPacketSize()?;
            if frames == 0 {
                break;
            }
            let (mut data, mut got, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
            capture.GetBuffer(&mut data, &mut got, &mut flags, None, None)?;
            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            for f in 0..got as usize {
                let mut sum = 0.0;
                if !silent {
                    for c in 0..format.channels {
                        sum += format.sample(data, f * format.channels + c);
                    }
                }
                ring[at] = sum / format.channels as f32;
                at = (at + 1) % WINDOW;
            }
            fresh |= got > 0;
            capture.ReleaseBuffer(got)?;
        }

        let mut now = [0.0f32; BANDS];
        if fresh {
            // oldest sample first, with the window applied once for all ten filters
            for i in 0..WINDOW {
                frame[i] = ring[(at + i) % WINDOW] * window[i];
            }
            for b in 0..BANDS {
                let mut power = 0.0;
                for k in 0..2 {
                    let coef = coefficients[b][k];
                    let (mut s1, mut s2) = (0.0f32, 0.0f32);
                    for &x in &frame {
                        let s0 = x + coef * s1 - s2;
                        s2 = s1;
                        s1 = s0;
                    }
                    power += s1 * s1 + s2 * s2 - coef * s1 * s2;
                }
                // amplitude of the tone (the Hann window halves it), in dB, then a fixed scale for a bar
                let amplitude = (power * 0.5).sqrt() * 4.0 / WINDOW as f32;
                let db = 20.0 * amplitude.max(1e-6).log10() + TILT_DB[b];
                now[b] = ((db + 62.0) / 44.0).clamp(0.0, 1.0);
            }
        }
        // nothing arrives while nothing plays: let the bars fall
        for b in 0..BANDS {
            shown[b] = if now[b] > shown[b] { now[b] } else { (shown[b] - 0.07).max(now[b]) };
        }
        publish(shown);
    }
}
