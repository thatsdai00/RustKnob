// The demo video: every capsule is drawn by the real renderer, frame by frame, and cut to the music.
// scripts/demo-music.py prepares the music in the work folder: music.wav, levels.csv (per frame: the kick, five
// bands) and beat.txt (seconds per beat). The timeline below is written at 120 BPM and runs at the track's tempo.
// ffmpeg puts the frames and the music together.
//   cargo run --release --features demo --target-dir target/demo -- --video <work folder> <ffmpeg.exe> <out.mp4>
// Every `REVIEW_EVERY` frames a still goes to <work folder>/frames for a look without playing the video.

use std::io::Write;
use std::process::{Command, Stdio};

use super::{cover, plain_song, rgb, scene, song, type_bmp, wallpaper, write_png, Canvas, Rgb, GRAY, WHITE};
use crate::gfx::{cov, lerp, rrect, sstep};
use crate::i18n;
use crate::media::{Art, Song, ART_PX};
use crate::mouse::{self, Model};
use crate::render::*;
use crate::text::{raster, TextBmp, TEXT_R};
use crate::theme::{DARK, LIGHT};
use crate::ui;

const W: usize = 1920;
const H: usize = 1080;
const FPS: f32 = 30.0;
const BEAT: f32 = 0.5; // the timeline's beat (120 BPM)
const LENGTH: f32 = 52.0; // timeline seconds
const U: f32 = 3.3; // capsule scale: device px per logical unit
const MORPH: f32 = 0.7; // seconds a shape change takes
const CAP_H: usize = 900; // rows of the capsule's buffer
const REVIEW_EVERY: usize = 15;
const TASKBAR: f32 = 48.0;

/// One state of the capsule on the timeline: from `at` it morphs into this shape and content.
#[derive(Clone, Copy)]
struct Seg {
    at: f32,
    kind: Kind,
    size: (f32, f32),
    radius: f32,
    unit: f32,
    fade: bool, // false: the same content goes on, only the shape changes
}

fn seg(at: f32, kind: Kind, size: (f32, f32), unit: f32) -> Seg {
    Seg { at, kind, size, radius: (size.1 / 2.0).min(ui::CAPSULE_RADIUS), unit, fade: true }
}

fn segments() -> Vec<Seg> {
    let dot = ui::SMALL;
    vec![
        seg(0.0, Kind::Volume, dot, U),
        seg(4.0, Kind::Volume, (ui::VOLUME_W, ui::VOLUME_H), U),
        Seg { radius: ui::KNOB_RADIUS, ..seg(7.0, Kind::Knob, ui::KNOB, 2.4) },
        seg(10.0, Kind::Pinned, ui::PINNED, U),
        Seg { fade: false, ..seg(15.5, Kind::Pinned, ui::PINNED_TALL, U) },
        seg(18.0, Kind::MouseLink, ui::MOUSE_LINK, U),
        seg(20.5, Kind::MouseDpi, ui::MOUSE_DPI, U),
        seg(24.0, Kind::Song, ui::SONG, U),
        seg(25.5, Kind::Volume, (ui::VOLUME_W, ui::VOLUME_H_SUB), U),
        seg(27.0, Kind::Song, ui::SONG, U),
        seg(28.0, Kind::Drop, ui::DROP, U),
        seg(30.0, Kind::Upload, ui::UPLOAD, U),
        Seg { fade: false, ..seg(32.5, Kind::Upload, ui::UPLOAD, U) },
        seg(34.0, Kind::Song, dot, U),
        seg(36.0, Kind::Song, ui::SONG, U),
        Seg { radius: ui::CAPSULE_RADIUS, ..seg(38.0, Kind::Settings, ui::SETTINGS, 1.2) },
        seg(42.0, Kind::Settings, dot, U),
    ]
}

/// The headline and the line under it for each part, and the color it glows in.
struct Caption {
    from: f32,
    to: f32,
    title: TextBmp,
    sub: Vec<TextBmp>,
    left: bool, // beside a tall capsule (settings) rather than under it
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_io(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// The capsule's own spring: overshoots a little and settles.
fn spring(u: f32) -> f32 {
    if u >= 1.0 { 1.0 } else { 1.0 - (-6.0 * u).exp() * ((u * 11.0).cos() + 0.55 * (u * 11.0).sin()) }
}

/// Opacity and rise (px) of something on show from `a` to `b`.
fn on_show(t: f32, a: f32, b: f32) -> (f32, f32) {
    if t < a || t > b {
        return (0.0, 0.0);
    }
    let i = ease((t - a) / 0.4);
    let o = 1.0 - ease_io((t - (b - 0.3)) / 0.3);
    (i.min(o), (1.0 - i) * 28.0)
}

/// Steps up on every beat from `from`, each step eased: n beats in, n (fractional while it moves).
fn beats(t: f32, from: f32) -> f32 {
    if t < from {
        return 0.0;
    }
    let k = (t - from) / BEAT;
    k.floor() + ease(k.fract() / 0.3)
}

/// A microphone with a slash, on a blurple square: the card a voice app's mute gets.
fn mic_art() -> Art {
    let n = ART_PX as f32;
    let mut px = Vec::with_capacity(ART_PX * ART_PX);
    for y in 0..ART_PX {
        for x in 0..ART_PX {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let bg = cov(rrect(fx - n / 2.0, fy - n / 2.0, n / 2.0, n / 2.0, 15.0), 1.0);
            let body = cov(rrect(fx - 32.0, fy - 25.0, 7.0, 12.5, 7.0), 1.0);
            let r = ((fx - 32.0).powi(2) + (fy - 28.0).powi(2)).sqrt();
            let arc = if fy > 28.0 { cov((r - 13.5).abs() - 2.2, 1.0) } else { 0.0 };
            let stem = cov(rrect(fx - 32.0, fy - 46.0, 2.2, 4.5, 1.0), 1.0);
            let foot = cov(rrect(fx - 32.0, fy - 50.5, 8.0, 2.2, 2.2), 1.0);
            // the slash, from top left to bottom right
            let d = ((fx - fy) / std::f32::consts::SQRT_2).abs();
            let along = (fx + fy) / std::f32::consts::SQRT_2;
            let span = (along - n / std::f32::consts::SQRT_2).abs() - 26.0;
            let slash = cov(d.max(span) - 2.6, 1.0);
            let gap = cov(d.max(span) - 5.6, 1.0);
            let white = (body.max(arc).max(stem).max(foot) * (1.0 - gap)).max(slash);
            let red = slash;
            let base = [88.0, 101.0, 242.0];
            let c = |i: usize| {
                let w = lerp(base[i], 255.0, white);
                lerp(w, [240.0, 71.0, 71.0][i], red) * bg
            };
            let a = (bg * 255.0) as u32;
            px.push(a << 24 | (c(0) as u32) << 16 | (c(1) as u32) << 8 | c(2) as u32);
        }
    }
    Art { px, accent: [140.0, 150.0, 255.0] }
}

fn keep<T>(t: T) -> &'static T {
    Box::leak(Box::new(t))
}

/// Composites a premultiplied 0xAARRGGBB buffer (W wide) onto the canvas, at `alpha`.
fn place_buf(c: &mut Canvas, buf: &[u32], rows: usize, top: i32, alpha: f32) {
    for y in 0..rows {
        let cy = y as i32 + top;
        if cy < 0 || cy as usize >= c.h {
            continue;
        }
        for x in 0..W {
            let p = buf[y * W + x];
            if p >> 24 == 0 {
                continue;
            }
            let a = (p >> 24) as f32 / 255.0 * alpha;
            let at = &mut c.px[cy as usize * c.w + x];
            let ch = |s: u32| ((p >> s) & 0xFF) as f32 / 255.0 * alpha;
            let inv = 1.0 - a;
            *at = [ch(16) + at[0] * inv, ch(8) + at[1] * inv, ch(0) + at[2] * inv, a + at[3] * inv];
        }
    }
}

/// A part of `src` (an opaque frame) drawn scaled around a center, for the screenshot flying into the island.
fn blit_scaled(c: &mut Canvas, src: &[[f32; 4]], rect: (f32, f32, f32, f32), center: (f32, f32), scale: f32, alpha: f32) {
    let (x0, y0, w, h) = rect;
    let (dw, dh) = (w * scale, h * scale);
    let (left, top) = (center.0 - dw / 2.0, center.1 - dh / 2.0);
    for py in (top.max(0.0) as usize)..((top + dh).ceil() as usize).min(H) {
        for px in (left.max(0.0) as usize)..((left + dw).ceil() as usize).min(W) {
            let (u, v) = ((px as f32 + 0.5 - left) / scale, (py as f32 + 0.5 - top) / scale);
            if u < 0.0 || v < 0.0 || u >= w || v >= h {
                continue;
            }
            let edge = cov(rrect(px as f32 + 0.5 - center.0, py as f32 + 0.5 - center.1, dw / 2.0, dh / 2.0, 10.0 * scale), 1.0);
            let s = src[(y0 + v) as usize * W + (x0 + u) as usize];
            c.over(px, py, [s[0], s[1], s[2]], alpha * edge);
        }
    }
}

fn levels(dir: &str) -> Vec<[f32; 6]> {
    let text = std::fs::read_to_string(format!("{dir}/levels.csv")).expect("levels.csv (run the music script first)");
    text.lines()
        .map(|l| {
            let mut v = [0.0; 6];
            for (slot, x) in v.iter_mut().zip(l.split(',')) {
                *slot = x.trim().parse().unwrap_or(0.0);
            }
            v
        })
        .collect()
}

/// The desktop: the wallpaper a little darker, darker still at the edges, and a taskbar.
fn backdrop() -> Vec<[f32; 4]> {
    let mut c = Canvas::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let (u, v) = (x as f32 / W as f32, y as f32 / H as f32);
            let w = wallpaper(u, v);
            let vig = 1.0 - 0.55 * (((u - 0.5) * 1.2).powi(2) + ((v - 0.45) * 1.4).powi(2)).min(1.0);
            let k = 0.58 * vig;
            c.px[y * W + x] = [w[0] * k, w[1] * k, w[2] * k, 1.0];
        }
    }
    let ty = H as f32 - TASKBAR;
    c.rrect(0.0, ty, W as f32, TASKBAR, 0.0, [0.03, 0.03, 0.05], 0.72);
    c.rrect(0.0, ty, W as f32, 1.0, 0.0, WHITE, 0.10);
    let icons = 8;
    for i in 0..icons {
        let x = W as f32 / 2.0 + (i as f32 - (icons as f32 - 1.0) / 2.0) * 50.0 - 15.0;
        c.rrect(x, ty + 9.0, 30.0, 30.0, 8.0, WHITE, if i == 0 { 0.55 } else { 0.20 });
    }
    let clock = type_bmp("16:20", 22, 400);
    c.text_flat(&clock, W as f32 - 92.0, ty + TASKBAR / 2.0, WHITE, 0.85);
    c.px
}

/// A tray battery icon as the program draws it (tray.rs), at `n` px.
fn tray_icon(percent: u8, charging: bool, n: usize) -> Vec<u32> {
    let mut px = vec![0u32; n * n];
    crate::tray::battery_pixels(&mut px, n, &crate::tray::Battery { model: 0, percent: Some(percent), charging, name: "" });
    px
}

fn place_icon(c: &mut Canvas, px: &[u32], n: usize, x: f32, y: f32, alpha: f32) {
    for j in 0..n {
        for i in 0..n {
            let p = px[j * n + i];
            let a = (p >> 24) as f32 / 255.0 * alpha;
            if a > 0.0 {
                let un = |s: u32| ((p >> s) & 0xFF) as f32 / 255.0 / ((p >> 24) as f32 / 255.0);
                c.over(x as usize + i, y as usize + j, [un(16), un(8), un(0)], a);
            }
        }
    }
}

pub fn video(dir: &str, ffmpeg: &str, out: &str) {
    let en = i18n::strings(i18n::resolve("en"));
    let lv = levels(dir);
    // timeline seconds per real second: the track's beat against the timeline's
    let speed = std::fs::read_to_string(format!("{dir}/beat.txt")).ok().and_then(|b| b.trim().parse::<f32>().ok()).map_or(1.0, |b| BEAT / b);
    let review = format!("{dir}/frames");
    std::fs::create_dir_all(&review).expect("frames folder");

    let red = rgb(255, 92, 78);
    let blue = rgb(70, 140, 255);
    let green = rgb(52, 211, 120);
    let orange = rgb(255, 159, 64);
    let violet = rgb(167, 105, 255);
    let teal = rgb(64, 200, 224);
    let indigo = rgb(110, 90, 255);

    // ---- what the capsules show
    let song_a = keep(song("Lumen Park", "Neon Tide", Some(cover([200.0, 40.0, 70.0], [24.0, 10.0, 30.0], [255.0, 120.0, 140.0]))));
    let song_b = keep(song("Northbound", "Afterglow", Some(cover([40.0, 110.0, 230.0], [10.0, 16.0, 60.0], [130.0, 190.0, 255.0]))));
    let knob_song = keep(song("Lumen Park", "Neon Tide", None));
    let lyric = keep(raster("Hold on to the night", -64, 700).unwrap());
    let bell_song = keep(plain_song("Deploy finished", "ntfy · production", "Deploy finished"));
    let mute_song = keep(plain_song(en.discord_muted, "Discord", "Discord"));
    let shot_song = keep(plain_song(en.snip_copied, "520 × 320", "Screenshot copied"));
    let mic = keep(mic_art());
    let device = keep(raster("Speakers (Realtek Audio)", -44, 400));
    let hint = keep(raster(en.drop_hint, -56, 700));
    let file_name = keep(raster("holiday-video.mp4", -56, 700));
    let speeds: Vec<&'static TextBmp> = ["36.8 MB/s · 1/1", "41.2 MB/s · 1/1", "44.9 MB/s · 1/1", "43.1 MB/s · 1/1", "46.4 MB/s · 1/1"].iter().map(|s| keep(raster(s, -44, 400).unwrap())).collect();
    let copied = keep(raster(en.copied_clipboard, -56, 700));
    let link = keep(raster("fuckingfast.net/k3x9d2", -44, 400));

    for m in [Model::ViperV3Pro, Model::Superlight2] {
        mouse::load_picture(m);
    }
    let viper = mouse::picture(Model::ViperV3Pro);
    let light = mouse::picture(Model::Superlight2);
    let viper_name = keep(raster("Viper V3 Pro", -64, 700));
    let linked = keep(raster(en.mouse_connected, -48, 400));
    let pct = keep(raster("82%", -60, 700));
    let dpis: Vec<&'static Option<TextBmp>> = ["800", "1600", "3200", "6400"].iter().map(|d| keep(raster(d, -100, 700))).collect();
    let dpi_label = keep(raster("DPI", -48, 700));
    let dpi_at = [21.5, 22.5, 23.5];

    let labels: &'static Vec<TextBmp> = keep(en.labels.iter().filter_map(|l| raster(l, -52, 400)).collect());
    let values: &'static Vec<(TextBmp, bool)> = keep(
        [("my-private-topic", false), ("https://ntfy.sh", false), ("click to type", true), ("notification.mp3", false), ("Auto", false), ("Auto", false)]
            .iter()
            .map(|(t, ph)| (raster(t, -48, 400).unwrap(), *ph))
            .collect(),
    );

    // ---- words on screen
    let caption = |from: f32, to: f32, title: &str, sub: &str| Caption { from, to, title: type_bmp(title, 80, 600), sub: vec![type_bmp(sub, 38, 400)], left: false };
    let side = |from: f32, to: f32, title: &str, sub: &str| Caption { from, to, title: type_bmp(title, 80, 600), sub: super::wrap(sub, 38, 400, 520.0), left: true };
    let captions = [
        (caption(4.5, 9.7, "Volume, reimagined.", "Scroll to turn it up. Double-click the middle button for the knob."), blue),
        (caption(10.5, 17.7, "Your music, always there.", "The cover's colors, a live visualizer, synced lyrics."), red),
        (caption(18.5, 23.7, "Your mouse, at a glance.", "Razer, Logitech, ASUS and ATK. Battery in the tray. No extra software."), green),
        (caption(24.4, 27.8, "Heard. Seen. Handled.", "ntfy pushes, audio devices, Discord mute and deafen."), violet),
        (caption(28.4, 33.8, "Drop it. It's uploaded.", "Drag a file to the top of the screen. The link lands on your clipboard."), green),
        (caption(34.3, 37.8, "Screenshots that feel native.", "Win+Shift+S, drag, let go. It flies into the island, copied."), orange),
        (side(38.3, 41.8, "Yours to shape.", "Light or dark, two languages, a switch for every feature."), teal),
    ];
    let tint_at = |t: f32| -> Rgb {
        match t {
            t if t < 10.0 => blue,
            t if t < 18.0 => if t < 13.5 { red } else { blue },
            t if t < 24.0 => green,
            t if t < 28.0 => violet,
            t if t < 34.0 => green,
            t if t < 38.0 => orange,
            t if t < 42.0 => teal,
            _ => indigo,
        }
    };
    let title = type_bmp("RustKnob", 190, 600);
    let tagline = type_bmp("A Dynamic Island for Windows.", 62, 300);
    let end_title = type_bmp("RustKnob", 170, 600);
    let end_free = type_bmp("Free and open source.", 58, 400);
    let end_facts = type_bmp("One 0.9 MB exe   ·   ~0% CPU at rest   ·   No telemetry", 34, 400);
    let end_link = type_bmp("github.com/thatsdai00/RustKnob", 40, 600);
    let credit = type_bmp("Music: “Night Owl” by Broke For Free (CC BY 3.0)", 24, 400);
    let file_label = type_bmp("holiday-video.mp4", 30, 600);
    let file_size = type_bmp("248 MB", 24, 400);

    let bg = backdrop();
    let tray_viper = tray_icon(82, false, 32);
    let tray_light = tray_icon(46, true, 32);
    let segs = segments();

    let mut child = Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{W}x{H}"), "-r", "30", "-i", "-"])
        .args(["-i", &format!("{dir}/music.wav"), "-c:v", "libx264", "-preset", "slow", "-crf", "16", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k", "-shortest", "-movflags", "+faststart", out])
        .stdin(Stdio::piped())
        .spawn()
        .expect("start ffmpeg");
    let mut pipe = child.stdin.take().unwrap();

    let mut cap = vec![0u32; W * CAP_H];
    let mut cap2 = vec![0u32; W * CAP_H];
    let mut bars = [0.06f32; 5];
    let mut rgb_out = vec![0u8; W * H * 3];
    let frames = (LENGTH / speed * FPS) as usize;
    let sel = (700.0f32, 330.0f32, 520.0f32, 320.0f32); // the screenshot's selection, final

    for f in 0..frames {
        let t = f as f32 / FPS * speed;
        let row = lv.get(f).copied().unwrap_or([0.0; 6]);
        let kick = row[0];
        for i in 0..5 {
            let target = 0.07 + 0.93 * row[i + 1];
            let rate = if target > bars[i] { 34.0 } else { 11.0 };
            bars[i] += (target - bars[i]) * (1.0 - (-rate / FPS).exp());
        }

        let mut c = Canvas { w: W, h: H, px: bg.clone() };

        // ---- the screenshot's dimmed screen (under the capsule, as on a real desktop)
        let dim = if (34.0..36.4).contains(&t) { ease((t - 34.0) / 0.3).min(1.0 - ease((t - 36.0) / 0.35)) } else { 0.0 };
        let grow = ease_io((t - 34.4) / 1.1);
        let (sw, sh) = (lerp(8.0, sel.2, grow), lerp(8.0, sel.3, grow));
        if dim > 0.0 {
            for y in 0..H {
                for x in 0..W {
                    let inside = t < 36.0 && t > 34.4 && (x as f32) >= sel.0 && (x as f32) < sel.0 + sw && (y as f32) >= sel.1 && (y as f32) < sel.1 + sh;
                    if !inside {
                        let p = &mut c.px[y * W + x];
                        let k = 1.0 - 0.55 * dim;
                        *p = [p[0] * k, p[1] * k, p[2] * k, 1.0];
                    }
                }
            }
        }

        // ---- the capsule: where the timeline is, and what it is morphing from
        let i = segs.iter().rposition(|s| s.at <= t).unwrap_or(0);
        let cur = segs[i];
        let prev = if i > 0 { segs[i - 1] } else { cur };
        let u = ((t - cur.at) / MORPH).clamp(0.0, 1.0);
        let shrinking = cur.size == ui::SMALL;
        let e = if shrinking { ease_io(u) } else { spring(u) };
        let w = lerp(prev.size.0, cur.size.0, e).max(ui::SMALL.0 * 0.6);
        let h = lerp(prev.size.1, cur.size.1, e.min(1.08)).max(ui::SMALL.1 * 0.6);
        let radius = lerp(prev.radius, cur.radius, e.clamp(0.0, 1.0)).min(h / 2.0).min(w / 2.0);
        let unit = lerp(prev.unit, cur.unit, ease_io(u));
        let shape_alpha = if t < 3.6 { 0.0 } else if t < 42.0 { ease((t - 3.6) / 0.4) } else { 1.0 - ease_io((t - 42.5) / 0.6) };

        let mut layers = if !cur.fade {
            [None, Some(Layer { kind: cur.kind, alpha: 1.0 })]
        } else {
            [(prev.size != ui::SMALL && u < 1.0).then(|| Layer::leaving(prev.kind, u / 0.35)), (!shrinking).then(|| Layer::entering(cur.kind, (u - 0.3) / 0.5))]
        };
        if i == 0 {
            layers = [None, None];
        }

        // the content, whichever kind is leaving or arriving
        let notice: &Song = if t < 25.5 { bell_song } else if t < 34.0 { mute_song } else { shot_song };
        let pin_song = if t < 13.0 { song_a } else { song_b };
        let swap = ((t - 13.0) / 1.0).clamp(0.0, 1.0);
        let lyric_frac = (beats(t, 16.0) / 5.0).clamp(0.0, 1.0);
        let mut sc = scene(w, h, radius, cur.kind, unit);
        sc.layers = layers;
        sc.alpha = shape_alpha;
        sc.marq = t - cur.at;
        sc.volume = if t < 25.0 { (0.28 + 0.035 * beats(t, 4.5)).min(0.9) } else { 0.45 };
        sc.song = Some(if t < 10.5 { knob_song } else { notice });
        sc.bell = t < 25.5;
        sc.frame = t >= 34.0;
        sc.app_icon = (25.5..34.0).contains(&t).then_some(mic);
        sc.sub = Some(device.as_ref().unwrap());
        sc.drop_hint = hint.as_ref();
        let tall = t >= 15.5;
        sc.pin = Some(PinView {
            song: Some(pin_song),
            prev: (t >= 13.0 && swap < 1.0).then_some(song_a),
            swap: if t < 13.0 { 1.0 } else { swap },
            bars,
            lyric: tall.then_some(lyric),
            lyric_old: None,
            lyric_swap: if tall { ease((t - 15.6) / 0.5) } else { 1.0 },
            lyric_fill: tall.then(|| lyric.size().0 * TEXT_R * lyric_frac),
            tall,
        });
        let dpi_i = dpi_at.iter().filter(|&&a| t >= a).count();
        let roll = if dpi_i == 0 { 1.0 } else { ease((t - dpi_at[dpi_i - 1]) / 0.35) };
        sc.mouse = Some(if t < 20.5 {
            MouseView { picture: viper, others: &[], dpi: None, dpi_old: None, roll: 1.0, dpi_label: None, stage: 1.0, stages: 4, title: viper_name.as_ref(), sub: linked.as_ref(), warn: None, percent: pct.as_ref(), fill: 0.82, level: 0, charging: false, intro: ((t - 18.0) / 1.0).clamp(0.0, 1.0) }
        } else {
            MouseView {
                picture: light,
                others: &[],
                dpi: dpis[dpi_i].as_ref(),
                dpi_old: if dpi_i > 0 && roll < 1.0 { dpis[dpi_i - 1].as_ref() } else { None },
                roll,
                dpi_label: dpi_label.as_ref(),
                stage: if dpi_i == 0 { 1.0 } else { dpi_i as f32 + roll },
                stages: 4,
                title: None,
                sub: None,
                warn: None,
                percent: None,
                fill: 0.82,
                level: 0,
                charging: false,
                intro: 1.0,
            }
        });
        let done = t >= 32.5;
        let up_p = ease_io((t - 30.3) / 2.1);
        sc.upload = Some(UploadView {
            phase: if done { UploadPhase::Done } else { UploadPhase::Uploading },
            progress: if done { 1.0 } else { up_p },
            name: if done { None } else { file_name.as_ref() },
            info: if done { None } else { Some(speeds[((t - 30.0) / 0.5).max(0.0) as usize % speeds.len()]) },
            msg: if done { copied.as_ref() } else { None },
            line: if done { link.as_ref() } else { None },
        });
        let mut toggles = [1.0f32; ui::TOGGLES];
        toggles[4] = sstep(38.75, 38.95, t);
        toggles[5] = sstep(39.25, 39.45, t) * (1.0 - sstep(41.0, 41.2, t));
        toggles[ui::TOGGLE_DISCORD] = sstep(39.75, 39.95, t);
        sc.settings = Some(SettingsView { labels, toggles, hover: None, step: 2, notify_volume: 20, values, editing: None, sound_set: true });

        // glow behind the capsule, on the beat
        let tint = tint_at(t);
        if shape_alpha > 0.0 {
            let (gw, gh) = (w * unit, h * unit);
            let cy = (ui::TOP + h / 2.0) * unit;
            c.glow(W as f32 / 2.0, cy, gw * 0.75 + 160.0, gh * 0.9 + 90.0, tint, (0.16 + 0.22 * kick) * shape_alpha);
        }

        if shape_alpha > 0.0 {
            for p in cap.iter_mut() {
                *p = 0;
            }
            let b = bounds(W, CAP_H, unit, w, h);
            render(&mut cap, W, CAP_H, b, &sc);
            // the settings turn light halfway: both drawn while they cross-fade
            let light_k = if cur.kind == Kind::Settings && !shrinking { ease_io((t - 40.0) / 0.4) } else if shrinking && cur.at == 42.0 { 1.0 } else { 0.0 };
            if light_k > 0.0 {
                sc.pal = LIGHT;
                for p in cap2.iter_mut() {
                    *p = 0;
                }
                render(&mut cap2, W, CAP_H, b, &sc);
                sc.pal = DARK;
                place_buf(&mut c, &cap, CAP_H, 0, 1.0 - light_k);
                place_buf(&mut c, &cap2, CAP_H, 0, light_k);
            } else {
                place_buf(&mut c, &cap, CAP_H, 0, 1.0);
            }
        }

        // ---- the screenshot: selection, then the picture flying up into the island
        if (34.4..36.0).contains(&t) {
            c.outline(sel.0 - 2.0, sel.1 - 2.0, sw + 4.0, sh + 4.0, 3.0, WHITE, 0.95);
            for (hx, hy) in [(sel.0, sel.1), (sel.0 + sw, sel.1), (sel.0, sel.1 + sh), (sel.0 + sw, sel.1 + sh)] {
                c.rrect(hx - 9.0, hy - 9.0, 18.0, 18.0, 9.0, WHITE, 1.0);
            }
            let dims = type_bmp(&format!("{} × {}", sw.round(), sh.round()), 26, 600);
            let pill = dims.w as f32 + 36.0;
            c.rrect(sel.0, sel.1 - 62.0, pill, 44.0, 22.0, [0.0; 3], 0.9);
            c.text_flat(&dims, sel.0 + 18.0, sel.1 - 40.0, WHITE, 1.0);
        }
        if (36.0..36.6).contains(&t) {
            let k = ease_io((t - 36.0) / 0.55);
            let from = (sel.0 + sel.2 / 2.0, sel.1 + sel.3 / 2.0);
            let to = (W as f32 / 2.0 - 110.0, (ui::TOP + ui::SONG.1 / 2.0) * U);
            let center = (lerp(from.0, to.0, k), lerp(from.1, to.1, k * k));
            blit_scaled(&mut c, &bg, sel, center, lerp(1.0, 0.07, k), 1.0 - k * k * 0.8);
            let flash = 1.0 - ease((t - 36.0) / 0.25);
            if flash > 0.0 {
                for p in c.px.iter_mut() {
                    *p = [lerp(p[0], 1.0, flash * 0.35), lerp(p[1], 1.0, flash * 0.35), lerp(p[2], 1.0, flash * 0.35), 1.0];
                }
            }
        }

        // ---- a file dragged up to the capsule
        if (28.1..30.1).contains(&t) {
            let k = ease_io((t - 28.2) / 1.4);
            let gone = ease((t - 29.75) / 0.3);
            let (cx, cy) = (W as f32 / 2.0 + lerp(240.0, 0.0, k), lerp(820.0, 175.0, k));
            let s = lerp(1.0, 0.92, k) * (1.0 - gone * 0.6);
            let a = ease((t - 28.1) / 0.25) * (1.0 - gone);
            let (fw, fh) = (380.0 * s, 92.0 * s);
            c.shadow(cx - fw / 2.0, cy - fh / 2.0, fw, fh, 18.0 * s, 30.0, 0.35 * a);
            c.rrect(cx - fw / 2.0, cy - fh / 2.0, fw, fh, 18.0 * s, rgb(245, 245, 248), 0.97 * a);
            c.rrect(cx - fw / 2.0 + 18.0 * s, cy - 28.0 * s, 44.0 * s, 56.0 * s, 8.0 * s, rgb(52, 140, 255), a);
            if s > 0.9 {
                c.text_flat(&file_label, cx - fw / 2.0 + 80.0 * s, cy - 12.0 * s, rgb(20, 20, 26), a);
                c.text_flat(&file_size, cx - fw / 2.0 + 80.0 * s, cy + 20.0 * s, rgb(110, 110, 120), a);
            }
            // the pointer holding it
            let (px0, py0) = (cx + fw / 2.0 - 30.0 * s, cy + fh / 2.0 - 16.0 * s);
            pointer(&mut c, px0, py0, a);
        }

        // ---- the tray's battery icons, from the mouse part on
        if t >= 18.4 {
            let ty = H as f32 - TASKBAR + 8.0;
            let a1 = ease((t - 18.4) / 0.3);
            let a2 = ease((t - 18.7) / 0.3);
            place_icon(&mut c, &tray_light, 32, W as f32 - 190.0, ty, a2);
            place_icon(&mut c, &tray_viper, 32, W as f32 - 150.0, ty, a1);
        }

        // ---- captions
        for (cap_text, _) in &captions {
            let (a, dy) = on_show(t, cap_text.from, cap_text.to);
            let sub = [0.84, 0.84, 0.88];
            if a > 0.0 && cap_text.left {
                c.text(&cap_text.title, 100.0, 430.0 + dy, WHITE, [0.80, 0.80, 0.86], a);
                for (k, line) in cap_text.sub.iter().enumerate() {
                    c.text_flat(line, 102.0, 510.0 + k as f32 * 52.0 + dy * 0.6, sub, a * 0.92);
                }
            } else if a > 0.0 {
                c.text_center(&cap_text.title, W as f32 / 2.0, 880.0 + dy, WHITE, [0.80, 0.80, 0.86], a);
                c.text_center(&cap_text.sub[0], W as f32 / 2.0, 950.0 + dy * 0.6, sub, sub, a * 0.92);
            }
        }

        // ---- the opening and the end, on darkened desktop
        let open_dark = 1.0 - ease_io((t - 3.4) / 0.6);
        let end_dark = ease_io((t - 43.2) / 0.8);
        let dark = open_dark.max(end_dark) * 0.78;
        if dark > 0.0 {
            for p in c.px.iter_mut() {
                let k = 1.0 - dark;
                *p = [p[0] * k, p[1] * k, p[2] * k, 1.0];
            }
        }
        if t < 4.0 {
            let (a, dy) = on_show(t, 0.15, 3.75);
            c.glow(W as f32 / 2.0, 480.0, 900.0, 300.0, indigo, 0.16 * a * (0.7 + 0.6 * kick.max(row[1] * 0.5)));
            c.text_center(&title, W as f32 / 2.0, 470.0 + dy, WHITE, [0.62, 0.62, 0.70], a);
            let (b, dy2) = on_show(t, 1.0, 3.75);
            c.text_center(&tagline, W as f32 / 2.0, 620.0 + dy2, [0.88, 0.88, 0.92], [0.88, 0.88, 0.92], b);
        }
        if t >= 44.0 {
            let pulse = kick * 0.5;
            c.glow(W as f32 / 2.0, 470.0, 1000.0, 360.0, indigo, (0.18 + 0.1 * pulse) * ease((t - 44.0) / 0.5));
            let (a, dy) = on_show(t, 44.0, 60.0);
            c.text_center(&end_title, W as f32 / 2.0, 400.0 + dy, WHITE, [0.62, 0.62, 0.70], a);
            let (b, dy) = on_show(t, 44.5, 60.0);
            c.text_center(&end_free, W as f32 / 2.0, 545.0 + dy, [0.92, 0.92, 0.95], [0.92, 0.92, 0.95], b);
            let (d, dy) = on_show(t, 45.0, 60.0);
            c.text_center(&end_facts, W as f32 / 2.0, 625.0 + dy, GRAY, GRAY, d);
            let (g, dy) = on_show(t, 45.5, 60.0);
            let pw = end_link.w as f32 + 90.0;
            c.rrect(W as f32 / 2.0 - pw / 2.0, 715.0 + dy, pw, 84.0, 42.0, rgb(0, 113, 227), g);
            c.rrect(W as f32 / 2.0 - pw / 2.0, 715.0 + dy, pw, 42.0, 42.0, WHITE, 0.07 * g);
            c.text_center(&end_link, W as f32 / 2.0, 757.0 + dy, WHITE, WHITE, g);
            let (m, _) = on_show(t, 46.0, 60.0);
            c.text_center(&credit, W as f32 / 2.0, 975.0, GRAY, GRAY, m * 0.8);
            // the final hit: a short flash
            let flash = (1.0 - (t - 44.0) / 0.35).max(0.0) * 0.22;
            if flash > 0.0 {
                for p in c.px.iter_mut() {
                    *p = [lerp(p[0], 1.0, flash), lerp(p[1], 1.0, flash), lerp(p[2], 1.0, flash), 1.0];
                }
            }
        }
        // in from black, out to black
        let fade = ease((t - 0.0) / 0.5).min(1.0 - ease_io((t - (LENGTH - 1.6)) / 1.5));

        for (o, p) in rgb_out.chunks_exact_mut(3).zip(&c.px) {
            o[0] = (p[0].clamp(0.0, 1.0) * fade * 255.0 + 0.5) as u8;
            o[1] = (p[1].clamp(0.0, 1.0) * fade * 255.0 + 0.5) as u8;
            o[2] = (p[2].clamp(0.0, 1.0) * fade * 255.0 + 0.5) as u8;
        }
        pipe.write_all(&rgb_out).expect("ffmpeg took no more frames");
        if f % REVIEW_EVERY == 0 {
            write_png(&format!("{review}/f{f:04}.png"), &c);
        }
    }
    drop(pipe);
    let status = child.wait().expect("ffmpeg");
    println!("video written to {out} ({frames} frames, ffmpeg {status})");
}

/// A mouse pointer (white arrow, dark edge) with its tip at (x, y).
fn pointer(c: &mut Canvas, x: f32, y: f32, alpha: f32) {
    let pts = [(0.0f32, 0.0f32), (0.0, 26.0), (6.5, 20.0), (11.0, 30.0), (15.0, 28.0), (10.5, 18.5), (19.0, 18.5)];
    let inside = |px: f32, py: f32| {
        let mut inn = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                inn = !inn;
            }
            j = i;
        }
        inn
    };
    for py in 0..34 {
        for px in 0..24 {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let core = inside(fx, fy);
            let edge = !core && [(-1.2, 0.0), (1.2, 0.0), (0.0, -1.2), (0.0, 1.2)].iter().any(|(dx, dy)| inside(fx + dx, fy + dy));
            if core || edge {
                let col = if core { WHITE } else { [0.05, 0.05, 0.07] };
                c.over((x + px as f32) as usize, (y + py as f32) as usize, col, alpha);
            }
        }
    }
}
