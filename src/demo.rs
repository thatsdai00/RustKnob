// Draws the pictures of the README with the real renderer, no window involved:
//   cargo run --release --features demo -- --demo docs/img
// Only built with the `demo` feature; the release exe does not contain it.

use std::io::Write;

use crate::i18n;
use crate::media::{Art, Song, ART_PX};
use crate::render::*;
use crate::text::{raster, TextBmp};
use crate::theme::DARK;
use crate::ui;

const UNIT: f32 = 2.0;

struct Image {
    w: usize,
    h: usize,
    px: Vec<u32>, // 0xAARRGGBB, premultiplied
}

/// The shape drawn by the renderer, cropped to what it covers.
fn shoot(scene: &Scene) -> Image {
    let (pw, ph) = (((scene.w + 2.0 * ui::SHADOW) * UNIT).ceil() as usize + 8, ((ui::TOP + scene.h + ui::SHADOW) * UNIT).ceil() as usize + 8);
    let mut buf = vec![0u32; pw * ph];
    let rect = bounds(pw, ph, UNIT, scene.w, scene.h);
    render(&mut buf, pw, ph, (rect.0, rect.1, rect.2, rect.3), scene);
    let (x0, x1, y1) = (rect.0, rect.2, rect.3);
    let (y0, w, h) = (((ui::TOP - 6.0) * UNIT) as usize, x1 - x0, y1 - ((ui::TOP - 6.0) * UNIT) as usize);
    let mut px = Vec::with_capacity(w * h);
    for y in y0..y1 {
        px.extend_from_slice(&buf[y * pw + x0..y * pw + x1]);
    }
    Image { w, h, px }
}

fn scene<'a>(w: f32, h: f32, radius: f32, kind: Kind) -> Scene<'a> {
    Scene {
        pal: DARK,
        unit: UNIT,
        w,
        h,
        radius,
        alpha: 1.0,
        layers: [None, Some(Layer { kind, alpha: 1.0 })],
        marq: 0.0,
        volume: 0.62,
        muted: false,
        icon: None,
        bell: false,
        frame: false,
        check: false,
        drop_hint: None,
        pin: None,
        sub: None,
        song: None,
        settings: None,
        upload: None,
        history: None,
        setup: None,
    }
}

fn capsule(size: (f32, f32), kind: Kind) -> Scene<'static> {
    scene(size.0, size.1, (size.1 / 2.0).min(ui::CAPSULE_RADIUS), kind)
}

fn song(artist: &str, title: &str, art: Option<Art>) -> Song {
    let line = format!("{title} — {artist}");
    Song { big: raster(title, -64, 700).unwrap(), small: raster(artist, -48, 400), line: raster(&line, -44, 400).unwrap(), art, key: line }
}

/// A made-up cover: a diagonal blend of two colors with a bright disc.
fn cover(a: [f32; 3], b: [f32; 3]) -> Art {
    let mut px = Vec::with_capacity(ART_PX * ART_PX);
    for y in 0..ART_PX {
        for x in 0..ART_PX {
            let t = (x + y) as f32 / (2 * ART_PX) as f32;
            let d = (((x as f32 - 40.0).powi(2) + (y as f32 - 24.0).powi(2)).sqrt() / 18.0).min(1.0);
            let glow = (1.0 - d) * 0.55;
            let c = |i: usize| (a[i] + (b[i] - a[i]) * t + glow * 255.0).clamp(0.0, 255.0) as u32;
            px.push(0xFF00_0000 | c(0) << 16 | c(1) << 8 | c(2));
        }
    }
    Art { px, accent: [255.0, 128.0, 105.0] }
}

// ---- backdrop and composing --------------------------------------------------

fn backdrop(x: usize, y: usize, w: usize, h: usize) -> [f32; 3] {
    let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
    let mix = |p: [f32; 3], q: [f32; 3], t: f32| [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t, p[2] + (q[2] - p[2]) * t];
    let top = mix([226.0, 120.0, 232.0], [120.0, 70.0, 235.0], u); // pink to violet
    let bottom = mix([50.0, 70.0, 210.0], [20.0, 20.0, 110.0], u); // blue to deep blue
    mix(top, bottom, (v * 1.1).min(1.0))
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>, // RGBA
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let c = backdrop(x, y, w, h);
                px.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
            }
        }
        Canvas { w, h, px }
    }

    /// `img` centered on `cx`, top edge at `y`.
    fn place(&mut self, img: &Image, cx: usize, y: usize) {
        let x0 = cx.saturating_sub(img.w / 2);
        for j in 0..img.h {
            for i in 0..img.w {
                let (x, yy) = (x0 + i, y + j);
                if x >= self.w || yy >= self.h {
                    continue;
                }
                let p = img.px[j * img.w + i];
                let a = (p >> 24) as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let at = (yy * self.w + x) * 4;
                for (k, shift) in [(0, 16u32), (1, 8), (2, 0)] {
                    let fg = ((p >> shift) & 0xFF) as f32; // premultiplied
                    self.px[at + k] = (fg + self.px[at + k] as f32 * (1.0 - a)).min(255.0) as u8;
                }
            }
        }
    }

    /// White text from a bitmap, left edge at `x`, vertical center at `cy`, scaled by `scale` device px per logical unit.
    fn text(&mut self, tb: &TextBmp, x: f32, cy: f32, scale: f32, alpha: f32) {
        let (lw, lh) = tb.size();
        let (dw, dh) = (lw * scale, lh * scale);
        let top = cy - dh / 2.0;
        for y in (top as usize)..((top + dh) as usize + 1).min(self.h) {
            for xx in (x as usize)..((x + dw) as usize + 1).min(self.w) {
                let a = tb.sample((xx as f32 + 0.5 - x) / scale * 4.0, (y as f32 + 0.5 - top) / scale * 4.0) * alpha;
                if a > 0.0 {
                    let at = (y * self.w + xx) * 4;
                    for k in 0..3 {
                        self.px[at + k] = (255.0 * a + self.px[at + k] as f32 * (1.0 - a)) as u8;
                    }
                }
            }
        }
    }
}

// ---- PNG ----------------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &d in data {
        a = (a + u32::from(d)) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= value << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes go out most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        let mut reversed = 0;
        for i in 0..bits {
            reversed |= ((code >> i) & 1) << (bits - 1 - i);
        }
        self.put(reversed, bits);
    }

    fn literal_or_length(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }
}

/// Deflate with the fixed Huffman table and a small LZ77 search: plenty for flat-colored pictures.
fn deflate(data: &[u8]) -> Vec<u8> {
    const LEN_BASE: [u32; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const LEN_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    const DIST_BASE: [u32; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
    const DIST_EXTRA: [u32; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
    let mut bits = Bits { out: Vec::new(), acc: 0, n: 0 };
    bits.put(1, 1); // the only block, last
    bits.put(1, 2); // fixed Huffman
    let mut head = vec![-1i32; 1 << 15];
    let mut prev = vec![-1i32; data.len()];
    let hash = |i: usize| (((u32::from(data[i]) << 10) ^ (u32::from(data[i + 1]) << 5) ^ u32::from(data[i + 2])) & 0x7FFF) as usize;
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + 3 <= data.len() {
            let mut candidate = head[hash(i)];
            let mut tries = 24;
            while candidate >= 0 && tries > 0 && i - candidate as usize <= 32768 {
                let c = candidate as usize;
                let max = (data.len() - i).min(258);
                let mut l = 0;
                while l < max && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    (best_len, best_dist) = (l, i - c);
                    if l == max {
                        break;
                    }
                }
                candidate = prev[c];
                tries -= 1;
            }
        }
        let step = if best_len >= 3 {
            let li = LEN_BASE.iter().rposition(|&b| b <= best_len as u32).unwrap();
            bits.literal_or_length(257 + li as u32);
            bits.put(best_len as u32 - LEN_BASE[li], LEN_EXTRA[li]);
            let di = DIST_BASE.iter().rposition(|&b| b <= best_dist as u32).unwrap();
            bits.code(di as u32, 5);
            bits.put(best_dist as u32 - DIST_BASE[di], DIST_EXTRA[di]);
            best_len
        } else {
            bits.literal_or_length(u32::from(data[i]));
            1
        };
        for k in i..i + step {
            if k + 3 <= data.len() {
                let h = hash(k);
                prev[k] = head[h];
                head[h] = k as i32;
            }
        }
        i += step;
    }
    bits.literal_or_length(256);
    if bits.n > 0 {
        bits.out.push(bits.acc as u8);
    }
    bits.out
}

fn write_png(path: &str, w: usize, h: usize, rgba: &[u8]) {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(1); // Sub filter: each byte minus the one a pixel to the left
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        for (i, &b) in row.iter().enumerate() {
            raw.push(b.wrapping_sub(if i >= 4 { row[i - 4] } else { 0 }));
        }
    }
    let mut z = vec![0x78, 0x01];
    z.extend(deflate(&raw));
    z.extend(adler32(&raw).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        png.extend((body.len() as u32).to_be_bytes());
        let mut crc_in = kind.to_vec();
        crc_in.extend(body);
        png.extend(&crc_in);
        png.extend(crc32(&crc_in).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend((w as u32).to_be_bytes());
    header.extend((h as u32).to_be_bytes());
    header.extend([8, 6, 0, 0, 0]); // 8 bit RGBA
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::File::create(path).and_then(|mut f| f.write_all(&png)).expect("write png");
}

/// A capsule on the backdrop with some room around it.
fn framed(img: &Image) -> Canvas {
    let (w, h) = (img.w + 120, img.h + 80);
    let mut c = Canvas::new(w, h);
    c.place(img, w / 2, 40);
    c
}

// ---- the pictures ---------------------------------------------------------------

pub fn run(dir: &str) {
    std::fs::create_dir_all(dir).expect("create the output folder");
    let save = |name: &str, c: &Canvas| write_png(&format!("{dir}/{name}.png"), c.w, c.h, &c.px);
    let en = i18n::strings(i18n::resolve("en"));

    let cover_a = cover([190.0, 30.0, 25.0], [20.0, 12.0, 18.0]);
    let pinned_song = song("System Of A Down", "Spiders", Some(cover_a));
    let pinned = |bars: [f32; 5]| {
        let mut s = capsule(ui::PINNED, Kind::Pinned);
        s.pin = Some(PinView { song: Some(&pinned_song), prev: None, swap: 1.0, bars });
        shoot(&s)
    };
    let pinned_img = pinned([0.95, 0.55, 0.8, 0.4, 0.7]);
    save("pinned", &framed(&pinned_img));

    // volume capsule with the playing song underneath
    let line = raster("Spiders — System Of A Down", -44, 400);
    let mut volume = capsule((ui::VOLUME_W, ui::VOLUME_H_SUB), Kind::Volume);
    volume.sub = line.as_ref();
    let volume_img = shoot(&volume);
    save("volume", &framed(&volume_img));

    // the knob
    let plain_song = song("System Of A Down", "Spiders", None);
    let mut knob = scene(ui::KNOB.0, ui::KNOB.1, ui::KNOB_RADIUS, Kind::Knob);
    knob.volume = 0.72;
    knob.song = Some(&plain_song);
    let knob_img = shoot(&knob);
    save("knob", &framed(&knob_img));

    // an upload and its result
    let (name, info) = (raster("holiday-video.mp4", -56, 700), raster("12.4 MB/s · 1/2", -44, 400));
    let mut upload = capsule(ui::UPLOAD, Kind::Upload);
    upload.upload = Some(UploadView { phase: UploadPhase::Uploading, progress: 0.64, name: name.as_ref(), info: info.as_ref(), msg: None, line: None });
    let upload_img = shoot(&upload);
    save("upload", &framed(&upload_img));
    let (msg, link) = (raster(en.copied_clipboard, -56, 700), raster("fuckingfast.net/k3x9d2", -44, 400));
    let mut done = capsule(ui::UPLOAD, Kind::Upload);
    done.upload = Some(UploadView { phase: UploadPhase::Done, progress: 1.0, name: None, info: None, msg: msg.as_ref(), line: link.as_ref() });
    let done_img = shoot(&done);

    // drop target
    let hint = raster(en.drop_hint, -56, 700);
    let mut drop = capsule(ui::DROP, Kind::Drop);
    drop.drop_hint = hint.as_ref();
    let drop_img = shoot(&drop);
    save("drop", &framed(&drop_img));

    // screenshot confirmation
    let shot_song = Song { big: raster(en.snip_copied, -64, 700).unwrap(), small: raster("1920 × 1080", -48, 400), line: raster("Screenshot copied", -44, 400).unwrap(), art: None, key: String::new() };
    let mut copied = capsule(ui::SONG, Kind::Song);
    copied.frame = true;
    copied.song = Some(&shot_song);
    let copied_img = shoot(&copied);

    // history
    let rows: Vec<(TextBmp, TextBmp)> = [
        ("holiday-video.mp4", "2026-11-09 18:08 · fuckingfast.net/k3x9d2"),
        ("report-final.pdf", "2026-11-09 17:41 · fuckingfast.net/q81mzs"),
        ("project-backup.zip", "2026-11-08 22:15 · fuckingfast.net/b7tt0e"),
        ("screenshot.png", "2026-11-08 14:36 · fuckingfast.net/eib32y"),
        ("music-mix.mp3", "2026-11-08 02:50 · fuckingfast.net/hd0wus"),
        ("slides.pptx", "2026-11-07 19:03 · fuckingfast.net/zz51cl"),
        ("notes.txt", "2026-11-06 09:22 · fuckingfast.net/lp04ka"),
    ]
    .iter()
    .map(|(a, b)| (raster(a, -52, 700).unwrap(), raster(b, -42, 400).unwrap()))
    .collect();
    let (title, status) = (raster(&format!("{} (23)", en.history_title), -56, 700), raster(en.copied, -44, 400));
    let mut history = scene(ui::HISTORY.0, ui::PAD * 2.0 + ui::HEAD_H + ui::HIST_ROWS as f32 * ui::HIST_ROW_H, ui::CAPSULE_RADIUS, Kind::History);
    history.history = Some(HistoryView { title: title.as_ref(), status: status.as_ref(), empty: None, rows: &rows, hover: Some(1), flash: None, scroll: 0, total: 23 });
    let history_img = shoot(&history);
    save("history", &framed(&history_img));

    // settings
    let labels: Vec<TextBmp> = en.labels.iter().filter_map(|l| raster(l, -52, 400)).collect();
    let values: Vec<(TextBmp, bool)> = [("my-private-topic", false), ("https://ntfy.sh", false), ("click to type", true), ("notification.mp3", false), ("Auto", false), ("Auto", false)]
        .iter()
        .map(|(t, ph)| (raster(t, -48, 400).unwrap(), *ph))
        .collect();
    let mut toggles = [1.0f32; ui::TOGGLES];
    toggles[4] = 0.0; // low power
    toggles[5] = 0.0; // upload sound
    let mut settings = scene(ui::SETTINGS.0, ui::SETTINGS.1, ui::CAPSULE_RADIUS, Kind::Settings);
    settings.settings = Some(SettingsView { labels: &labels, toggles, hover: None, step: 2, notify_volume: 20, values: &values, editing: None, sound_set: true });
    let settings_img = shoot(&settings);
    save("settings", &framed(&settings_img));

    // setup
    let texts: Vec<TextBmp> = [en.setup_title, en.setup_sub, en.setup_autostart, en.setup_autoupdate, en.labels[ui::ROW_LANG], en.labels[ui::ROW_THEME], en.setup_install, en.setup_portable]
        .iter()
        .enumerate()
        .filter_map(|(i, t)| raster(t, if i == 0 { -64 } else if i == 1 { -44 } else if i == 6 { -52 } else { -52 }, if i == 0 || i == 6 { 700 } else { 400 }))
        .collect();
    let setup_values: Vec<TextBmp> = [en.auto, en.auto].iter().filter_map(|t| raster(t, -48, 400)).collect();
    let mut setup = scene(ui::SETUP.0, ui::SETUP.1, ui::CAPSULE_RADIUS, Kind::Setup);
    setup.setup = Some(SetupView { texts: &texts, values: &setup_values, toggles: [1.0, 1.0], hover: None });
    let setup_img = shoot(&setup);
    save("setup", &framed(&setup_img));

    // the banner
    let mut banner = Canvas::new(1600, 520);
    banner.place(&pinned_img, 800, 46);
    let (name_bmp, tag_bmp) = (raster("RustKnob", -128, 700).unwrap(), raster("A Dynamic Island for Windows. One tiny Rust exe.", -64, 400).unwrap());
    let (nw, _) = name_bmp.size();
    banner.text(&name_bmp, 800.0 - nw * 3.4 / 2.0, 300.0, 3.4, 1.0);
    let (tw, _) = tag_bmp.size();
    banner.text(&tag_bmp, 800.0 - tw * 1.7 / 2.0, 435.0, 1.7, 0.92);
    save("banner", &banner);

    // a strip with the little capsules side by side
    let mut strip = Canvas::new(1600, 660);
    strip.place(&volume_img, 400, 40);
    strip.place(&copied_img, 400, 250);
    strip.place(&drop_img, 400, 450);
    strip.place(&upload_img, 1180, 54);
    strip.place(&done_img, 1180, 250);
    strip.place(&pinned_img, 1180, 450);
    save("capsules", &strip);
    println!("pictures written to {dir}");
}
