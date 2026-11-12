// Draws the pictures of the README with the real renderer, no window involved:
//   cargo run --release --features demo -- --demo docs/img
// Only built with the `demo` feature; the release exe does not contain it.

use std::io::Write;

use crate::gfx::{cov, lerp, rrect, sstep};
use crate::i18n;
use crate::media::{Art, Song, ART_PX};
use crate::render::*;
use crate::text::{raster, raster_face, TextBmp};
use crate::theme::{Palette, DARK, LIGHT};
use crate::ui;

/// Large type: a lighter, more modern face where Windows has it (it falls back to Segoe UI).
const FACE: &str = "Segoe UI Variable Display";

type Rgb = [f32; 3];

fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0]
}

// ---- the capsules themselves ---------------------------------------------------

struct Image {
    w: usize,
    h: usize,
    px: Vec<u32>, // 0xAARRGGBB, premultiplied
}

/// The shape drawn by the renderer, cropped to what it covers.
fn shoot(scene: &Scene) -> Image {
    let unit = scene.unit;
    let (pw, ph) = (((scene.w + 2.0 * ui::SHADOW) * unit).ceil() as usize + 8, ((ui::TOP + scene.h + ui::SHADOW) * unit).ceil() as usize + 8);
    let mut buf = vec![0u32; pw * ph];
    let rect = bounds(pw, ph, unit, scene.w, scene.h);
    render(&mut buf, pw, ph, (rect.0, rect.1, rect.2, rect.3), scene);
    let (x0, x1, y1) = (rect.0, rect.2, rect.3);
    let y0 = ((ui::TOP - 6.0) * unit) as usize;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut px = Vec::with_capacity(w * h);
    for y in y0..y1 {
        px.extend_from_slice(&buf[y * pw + x0..y * pw + x1]);
    }
    Image { w, h, px }
}

/// `scene` in a fixed `w` x `h` image (for animations: every frame the same size).
fn shoot_in(scene: &Scene, w: usize, h: usize) -> Image {
    let unit = scene.unit;
    let y0 = ((ui::TOP - 6.0) * unit) as usize;
    let ph = h + y0;
    let mut buf = vec![0u32; w * ph];
    render(&mut buf, w, ph, (0, 0, w, ph), scene);
    Image { w, h, px: buf[y0 * w..(y0 + h) * w].to_vec() }
}

fn scene<'a>(w: f32, h: f32, radius: f32, kind: Kind, unit: f32) -> Scene<'a> {
    Scene {
        pal: DARK,
        unit,
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
        mouse: None,
    }
}

fn capsule(size: (f32, f32), kind: Kind, unit: f32) -> Scene<'static> {
    scene(size.0, size.1, (size.1 / 2.0).min(ui::CAPSULE_RADIUS), kind, unit)
}

fn song(artist: &str, title: &str, art: Option<Art>) -> Song {
    let line = format!("{title} — {artist}");
    Song { big: raster(title, -64, 700).unwrap(), small: raster(artist, -48, 400), line: raster(&line, -44, 400).unwrap(), art, key: line }
}

/// A made-up cover: a diagonal blend of two colors with a bright disc.
fn cover(a: [f32; 3], b: [f32; 3], accent: [f32; 3]) -> Art {
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
    Art { px, accent }
}

// ---- a canvas to compose on ------------------------------------------------------

/// Premultiplied RGBA in floats; transparent to begin with.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![[0.0; 4]; w * h] }
    }

    fn solid(w: usize, h: usize, c: Rgb) -> Canvas {
        Canvas { w, h, px: vec![[c[0], c[1], c[2], 1.0]; w * h] }
    }

    #[inline]
    fn over(&mut self, x: usize, y: usize, c: Rgb, a: f32) {
        if a <= 0.0 || x >= self.w || y >= self.h {
            return;
        }
        let p = &mut self.px[y * self.w + x];
        let inv = 1.0 - a;
        *p = [c[0] * a + p[0] * inv, c[1] * a + p[1] * inv, c[2] * a + p[2] * inv, a + p[3] * inv];
    }

    /// A rounded rectangle, anti-aliased; `paint` gives the color at a pixel.
    fn rrect_with(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, alpha: f32, paint: impl Fn(f32, f32) -> Rgb) {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in (y.floor().max(0.0) as usize)..((y + h).ceil() as usize + 1).min(self.h) {
            for px in (x.floor().max(0.0) as usize)..((x + w).ceil() as usize + 1).min(self.w) {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let a = cov(rrect(fx - cx, fy - cy, w / 2.0, h / 2.0, r), 1.0) * alpha;
                if a > 0.0 {
                    self.over(px, py, paint(fx, fy), a);
                }
            }
        }
    }

    fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, alpha: f32) {
        self.rrect_with(x, y, w, h, r, alpha, |_, _| c);
    }

    /// A 2 px line just inside the edge, like the hairline Apple puts around dark cards.
    fn outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, alpha: f32) {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in (y.floor().max(0.0) as usize)..((y + h).ceil() as usize + 1).min(self.h) {
            for px in (x.floor().max(0.0) as usize)..((x + w).ceil() as usize + 1).min(self.w) {
                let d = rrect(px as f32 + 0.5 - cx, py as f32 + 0.5 - cy, w / 2.0, h / 2.0, r);
                let a = cov(d.abs() - 1.0, 1.0) * alpha;
                if a > 0.0 {
                    self.over(px, py, c, a);
                }
            }
        }
    }

    /// A soft drop shadow under a rounded rectangle.
    fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, reach: f32, alpha: f32) {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in ((y - reach).floor().max(0.0) as usize)..((y + h + reach).ceil() as usize).min(self.h) {
            for px in ((x - reach).floor().max(0.0) as usize)..((x + w + reach).ceil() as usize).min(self.w) {
                let d = rrect(px as f32 + 0.5 - cx, py as f32 + 0.5 - cy, w / 2.0, h / 2.0, r);
                let a = (1.0 - sstep(-reach * 0.2, reach, d)).powi(2) * alpha;
                if a > 0.0 {
                    self.over(px, py, [0.0; 3], a);
                }
            }
        }
    }

    /// A soft round glow.
    fn glow(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, c: Rgb, alpha: f32) {
        for py in ((cy - ry).floor().max(0.0) as usize)..((cy + ry).ceil() as usize).min(self.h) {
            for px in ((cx - rx).floor().max(0.0) as usize)..((cx + rx).ceil() as usize).min(self.w) {
                let d = (((px as f32 - cx) / rx).powi(2) + ((py as f32 - cy) / ry).powi(2)).sqrt();
                let a = (1.0 - sstep(0.0, 1.0, d)).powi(2) * alpha;
                if a > 0.0 {
                    self.over(px, py, c, a);
                }
            }
        }
    }

    /// A capsule picture (premultiplied 0xAARRGGBB) with its top left at (x, y).
    fn place(&mut self, img: &Image, x: i32, y: i32) {
        for j in 0..img.h {
            for i in 0..img.w {
                let p = img.px[j * img.w + i];
                let a = (p >> 24) as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let (px, py) = (x + i as i32, y + j as i32);
                if px < 0 || py < 0 || px as usize >= self.w || py as usize >= self.h {
                    continue;
                }
                let at = &mut self.px[py as usize * self.w + px as usize];
                let inv = 1.0 - a;
                let ch = |shift: u32| ((p >> shift) & 0xFF) as f32 / 255.0;
                *at = [ch(16) + at[0] * inv, ch(8) + at[1] * inv, ch(0) + at[2] * inv, a + at[3] * inv];
            }
        }
    }

    fn place_center(&mut self, img: &Image, cx: i32, y: i32) {
        self.place(img, cx - img.w as i32 / 2, y);
    }

    /// Text from a bitmap, left edge at x, vertical center at cy; the color may change from top to bottom.
    fn text(&mut self, tb: &TextBmp, x: f32, cy: f32, top: Rgb, bottom: Rgb, alpha: f32) {
        let (dw, dh) = (tb.w as f32, tb.h as f32);
        let y0 = cy - dh / 2.0;
        for py in (y0.floor().max(0.0) as usize)..((y0 + dh).ceil() as usize).min(self.h) {
            let t = ((py as f32 - y0) / dh).clamp(0.0, 1.0);
            let c = [lerp(top[0], bottom[0], t), lerp(top[1], bottom[1], t), lerp(top[2], bottom[2], t)];
            for px in (x.floor().max(0.0) as usize)..((x + dw).ceil() as usize).min(self.w) {
                let a = tb.sample(px as f32 + 0.5 - x, py as f32 + 0.5 - y0) * alpha;
                if a > 0.0 {
                    self.over(px, py, c, a);
                }
            }
        }
    }

    fn text_flat(&mut self, tb: &TextBmp, x: f32, cy: f32, c: Rgb, alpha: f32) {
        self.text(tb, x, cy, c, c, alpha);
    }

    fn text_center(&mut self, tb: &TextBmp, cx: f32, cy: f32, top: Rgb, bottom: Rgb, alpha: f32) {
        self.text(tb, cx - tb.w as f32 / 2.0, cy, top, bottom, alpha);
    }

    fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            let un = |v: f32| if a > 0.0 { (v / a).clamp(0.0, 1.0) } else { 0.0 };
            out.extend_from_slice(&[(un(p[0]) * 255.0 + 0.5) as u8, (un(p[1]) * 255.0 + 0.5) as u8, (un(p[2]) * 255.0 + 0.5) as u8, (a * 255.0 + 0.5) as u8]);
        }
        out
    }
}

/// A text of `px` device pixels (character height).
fn type_bmp(text: &str, px: i32, weight: i32) -> TextBmp {
    raster_face(text, -px, weight, FACE).unwrap_or_else(|| raster(text, -px, weight).unwrap())
}

/// Words flowed into lines of at most `max` device pixels.
fn wrap(text: &str, px: i32, weight: i32, max: f32) -> Vec<TextBmp> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if !line.is_empty() && type_bmp(&candidate, px, weight).w as f32 > max {
            lines.push(type_bmp(&line, px, weight));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(type_bmp(&line, px, weight));
    }
    lines
}

// ---- backdrops --------------------------------------------------------------------

/// A soft, colorful wallpaper: a few big blobs of color on a deep base.
fn wallpaper(u: f32, v: f32) -> Rgb {
    let blobs: [(f32, f32, f32, Rgb); 5] = [
        (0.12, 0.18, 0.55, rgb(255, 84, 150)),
        (0.88, 0.08, 0.45, rgb(255, 160, 72)),
        (0.55, 1.0, 0.65, rgb(54, 110, 255)),
        (0.95, 0.85, 0.5, rgb(160, 84, 255)),
        (0.35, 0.65, 0.4, rgb(255, 120, 90)),
    ];
    let mut c = rgb(22, 14, 64);
    for (bx, by, r, col) in blobs {
        let d2 = ((u - bx).powi(2) + (v - by).powi(2)) / (r * r);
        let w = (-d2 * 1.6).exp();
        for k in 0..3 {
            c[k] += (col[k] - c[k]) * w * 0.92;
        }
    }
    c
}

// ---- PNG / APNG ----------------------------------------------------------------------

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

/// Deflate with the fixed Huffman table and a small LZ77 search.
fn deflate(data: &[u8]) -> Vec<u8> {
    const LEN_BASE: [u32; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const LEN_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    const DIST_BASE: [u32; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
    const DIST_EXTRA: [u32; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
    let mut bits = Bits { out: Vec::new(), acc: 0, n: 0 };
    bits.put(1, 1);
    bits.put(1, 2);
    let mut head = vec![-1i32; 1 << 15];
    let mut prev = vec![-1i32; data.len()];
    let hash = |i: usize| (((u32::from(data[i]) << 10) ^ (u32::from(data[i + 1]) << 5) ^ u32::from(data[i + 2])) & 0x7FFF) as usize;
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + 3 <= data.len() {
            let mut candidate = head[hash(i)];
            let mut tries = 32;
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

/// Filtered, deflated, zlib-wrapped pixel data.
fn zlib_pixels(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(1); // Sub filter
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        for (i, &b) in row.iter().enumerate() {
            raw.push(b.wrapping_sub(if i >= 4 { row[i - 4] } else { 0 }));
        }
    }
    let mut z = vec![0x78, 0x01];
    z.extend(deflate(&raw));
    z.extend(adler32(&raw).to_be_bytes());
    z
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    png.extend((body.len() as u32).to_be_bytes());
    let mut crc_in = kind.to_vec();
    crc_in.extend(body);
    png.extend(&crc_in);
    png.extend(crc32(&crc_in).to_be_bytes());
}

fn header(w: usize, h: usize) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend((w as u32).to_be_bytes());
    v.extend((h as u32).to_be_bytes());
    v.extend([8, 6, 0, 0, 0]);
    v
}

fn write_png(path: &str, c: &Canvas) {
    let rgba = c.to_rgba();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &header(c.w, c.h));
    chunk(&mut png, b"IDAT", &zlib_pixels(c.w, c.h, &rgba));
    chunk(&mut png, b"IEND", &[]);
    std::fs::File::create(path).and_then(|mut f| f.write_all(&png)).expect("write png");
}

/// An animated PNG (plays in browsers and on GitHub). `delays` are per frame, in milliseconds.
fn write_apng(path: &str, frames: &[Canvas], delays: &[u16]) {
    let (w, h) = (frames[0].w, frames[0].h);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &header(w, h));
    let mut actl = Vec::new();
    actl.extend((frames.len() as u32).to_be_bytes());
    actl.extend(0u32.to_be_bytes()); // loop forever
    chunk(&mut png, b"acTL", &actl);
    let mut seq = 0u32;
    for (i, frame) in frames.iter().enumerate() {
        let mut fctl = Vec::new();
        fctl.extend(seq.to_be_bytes());
        seq += 1;
        fctl.extend((w as u32).to_be_bytes());
        fctl.extend((h as u32).to_be_bytes());
        fctl.extend(0u32.to_be_bytes());
        fctl.extend(0u32.to_be_bytes());
        fctl.extend(delays[i].to_be_bytes());
        fctl.extend(1000u16.to_be_bytes());
        fctl.extend([0, 0]); // dispose none, blend source
        chunk(&mut png, b"fcTL", &fctl);
        let data = zlib_pixels(w, h, &frame.to_rgba());
        if i == 0 {
            chunk(&mut png, b"IDAT", &data);
        } else {
            let mut fdat = seq.to_be_bytes().to_vec();
            seq += 1;
            fdat.extend(data);
            chunk(&mut png, b"fdAT", &fdat);
        }
    }
    chunk(&mut png, b"IEND", &[]);
    std::fs::File::create(path).and_then(|mut f| f.write_all(&png)).expect("write apng");
}

// ---- the cards --------------------------------------------------------------------------

const CARD_W: usize = 1200;
const CARD_H: usize = 960;
const WHITE: Rgb = [1.0, 1.0, 1.0];
const GRAY: Rgb = [0.60, 0.60, 0.64];

/// An Apple-style tile: a headline, a few words, and the thing itself below, on a dark card with a colored glow.
fn card(title: &str, body: &str, tint: Rgb, draw: impl FnOnce(&mut Canvas, f32, f32)) -> Canvas {
    let mut c = Canvas::new(CARD_W, CARD_H);
    let h = CARD_H as f32;
    c.rrect_with(0.0, 0.0, CARD_W as f32, h, 56.0, 1.0, |_, y| {
        let t = y / h;
        [lerp(0.105, 0.075, t), lerp(0.105, 0.075, t), lerp(0.115, 0.085, t)]
    });
    c.glow(CARD_W as f32 / 2.0, 690.0, 620.0, 360.0, tint, 0.34);
    c.glow(CARD_W as f32 / 2.0, 720.0, 380.0, 200.0, tint, 0.20);
    let head = type_bmp(title, 66, 600);
    c.text(&head, 76.0, 118.0, WHITE, [0.80, 0.80, 0.84], 1.0);
    for (i, line) in wrap(body, 36, 400, CARD_W as f32 - 152.0).iter().enumerate() {
        c.text_flat(line, 76.0, 200.0 + i as f32 * 52.0, GRAY, 1.0);
    }
    draw(&mut c, CARD_W as f32 / 2.0, 690.0);
    c.outline(0.0, 0.0, CARD_W as f32, h, 56.0, WHITE, 0.07);
    c
}

fn show(c: &mut Canvas, img: &Image, cx: f32, top: f32) {
    c.place_center(img, cx as i32, top as i32);
}

pub fn run(dir: &str) {
    std::fs::create_dir_all(dir).expect("create the output folder");
    let save = |name: &str, c: &Canvas| write_png(&format!("{dir}/{name}.png"), c);
    let en = i18n::strings(i18n::resolve("en"));

    let red = rgb(255, 92, 78);
    let blue = rgb(70, 140, 255);
    let green = rgb(52, 211, 120);
    let orange = rgb(255, 159, 64);
    let violet = rgb(167, 105, 255);
    let teal = rgb(64, 200, 224);

    let song_red = song("System Of A Down", "Spiders", Some(cover([190.0, 30.0, 25.0], [20.0, 12.0, 18.0], [255.0, 120.0, 100.0])));
    let song_blue = song("Daft Punk", "Instant Crush", Some(cover([40.0, 90.0, 220.0], [12.0, 18.0, 60.0], [130.0, 190.0, 255.0])));
    let pinned = |s: &Song, bars: [f32; 5], unit: f32| {
        let mut sc = capsule(ui::PINNED, Kind::Pinned, unit);
        sc.pin = Some(PinView { song: Some(s), prev: None, swap: 1.0, bars, lyric: None, lyric_old: None, lyric_swap: 1.0, lyric_fill: None, tall: false });
        shoot(&sc)
    };

    // ---------------------------------------------------------------- hero
    let mut hero = Canvas::solid(2400, 1740, [0.0, 0.0, 0.0]);
    hero.glow(1200.0, 380.0, 1100.0, 420.0, rgb(110, 90, 255), 0.10);
    let title = type_bmp("RustKnob", 250, 600);
    hero.text_center(&title, 1200.0, 250.0, WHITE, [0.62, 0.62, 0.68], 1.0);
    let sub = type_bmp("A Dynamic Island for Windows.", 74, 300);
    hero.text_center(&sub, 1200.0, 470.0, [0.88, 0.88, 0.92], [0.88, 0.88, 0.92], 1.0);
    let sub2 = type_bmp("Volume. Music. Uploads. Screenshots. One tiny exe.", 46, 400);
    hero.text_center(&sub2, 1200.0, 556.0, GRAY, GRAY, 1.0);

    let (sx, sy, sw, sh) = (180.0, 660.0, 2040.0, 1020.0);
    hero.shadow(sx, sy, sw, sh, 44.0, 90.0, 0.55);
    hero.rrect_with(sx, sy, sw, sh, 44.0, 1.0, |x, y| wallpaper((x - sx) / sw, (y - sy) / sh));
    hero.shadow(300.0, 900.0, 980.0, 620.0, 26.0, 50.0, 0.30);
    hero.rrect(300.0, 900.0, 980.0, 620.0, 26.0, WHITE, 0.16);
    hero.rrect(300.0, 900.0, 980.0, 70.0, 26.0, WHITE, 0.10);
    for (i, col) in [rgb(255, 95, 87), rgb(254, 188, 46), rgb(40, 200, 64)].iter().enumerate() {
        hero.rrect(338.0 + i as f32 * 44.0, 922.0, 26.0, 26.0, 13.0, *col, 0.9);
    }
    for k in 0..6 {
        hero.rrect(350.0, 1020.0 + k as f32 * 66.0, 560.0 - (k % 3) as f32 * 90.0, 18.0, 9.0, WHITE, 0.30);
    }
    hero.shadow(1160.0, 1050.0, 880.0, 520.0, 26.0, 50.0, 0.30);
    hero.rrect(1160.0, 1050.0, 880.0, 520.0, 26.0, rgb(10, 10, 20), 0.46);
    hero.rrect(1160.0, 1050.0, 880.0, 64.0, 26.0, WHITE, 0.07);
    for k in 0..5 {
        hero.rrect(1210.0, 1160.0 + k as f32 * 66.0, 600.0 - (k % 2) as f32 * 160.0, 18.0, 9.0, WHITE, 0.22);
    }
    let hero_capsule = pinned(&song_red, [0.9, 0.55, 0.8, 0.4, 0.7], 4.4);
    hero.glow(1200.0, 770.0, 700.0, 150.0, red, 0.22);
    hero.place_center(&hero_capsule, 1200, 726);
    hero.outline(sx, sy, sw, sh, 44.0, WHITE, 0.12);
    save("hero", &hero);

    // ---------------------------------------------------------------- download button
    let mut button = Canvas::new(900, 300);
    button.rrect(0.0, 20.0, 900.0, 150.0, 75.0, rgb(0, 113, 227), 1.0);
    button.rrect(0.0, 20.0, 900.0, 75.0, 75.0, WHITE, 0.07);
    let label = type_bmp("Download Releases", 56, 600);
    button.text_center(&label, 450.0, 96.0, WHITE, WHITE, 1.0);
    let small = type_bmp("github.com/thatsdai00/RustKnob/releases", 34, 400);
    button.text_center(&small, 450.0, 232.0, [0.52, 0.52, 0.56], [0.52, 0.52, 0.56], 1.0);
    save("download", &button);

    // ---------------------------------------------------------------- cards
    // 1. volume and knob
    let knob_song = song("System Of A Down", "Spiders", None);
    let mut knob_scene = scene(ui::KNOB.0, ui::KNOB.1, ui::KNOB_RADIUS, Kind::Knob, 1.35);
    knob_scene.volume = 0.72;
    knob_scene.song = Some(&knob_song);
    let knob_img = shoot(&knob_scene);
    let mut volume_scene = capsule((ui::VOLUME_W, ui::VOLUME_H), Kind::Volume, 1.9);
    volume_scene.volume = 0.62;
    let volume_img = shoot(&volume_scene);
    save(
        "card-volume",
        &card("Volume, reimagined.", "Double-click the middle button. Scroll to turn it up. The capsule becomes a knob that feels like hardware.", blue, |c, cx, cy| {
            show(c, &knob_img, cx, cy - 345.0);
            show(c, &volume_img, cx, cy + 105.0);
        }),
    );

    // 2. pinned media
    let p1 = pinned(&song_red, [0.9, 0.5, 0.78, 0.42, 0.66], 2.4);
    let lyric = raster("Sing it with me now", -64, 700).unwrap();
    let p2 = {
        let mut sc = capsule(ui::PINNED_TALL, Kind::Pinned, 2.4);
        sc.pin = Some(PinView { song: Some(&song_blue), prev: None, swap: 1.0, bars: [0.55, 0.85, 0.4, 0.75, 0.5], lyric: Some(&lyric), lyric_old: None, lyric_swap: 1.0, lyric_fill: Some(lyric.size().0 * crate::text::TEXT_R * 0.55), tall: true });
        shoot(&sc)
    };
    save(
        "card-media",
        &card("Your music, always there.", "Pin the media. Cover, a real spectrum visualizer and synced lyrics, even word by word (beta). The colors follow the cover.", red, |c, cx, cy| {
            show(c, &p1, cx, cy - 190.0);
            show(c, &p2, cx, cy + 20.0);
        }),
    );

    // 3. drop to upload
    let hint = raster(en.drop_hint, -56, 700);
    let mut drop_scene = capsule(ui::DROP, Kind::Drop, 1.9);
    drop_scene.drop_hint = hint.as_ref();
    let drop_img = shoot(&drop_scene);
    let (name, info) = (raster("holiday-video.mp4", -56, 700), raster("12.4 MB/s · 1/2", -44, 400));
    let mut upload_scene = capsule(ui::UPLOAD, Kind::Upload, 1.9);
    upload_scene.upload = Some(UploadView { phase: UploadPhase::Uploading, progress: 0.64, name: name.as_ref(), info: info.as_ref(), msg: None, line: None });
    let upload_img = shoot(&upload_scene);
    let (msg, link) = (raster(en.copied_clipboard, -56, 700), raster("fuckingfast.net/k3x9d2", -44, 400));
    let mut done_scene = capsule(ui::UPLOAD, Kind::Upload, 1.9);
    done_scene.upload = Some(UploadView { phase: UploadPhase::Done, progress: 1.0, name: None, info: None, msg: msg.as_ref(), line: link.as_ref() });
    let done_img = shoot(&done_scene);
    save(
        "card-upload",
        &card("Drop it. It's uploaded.", "Drag a file toward the top of the screen and drop it on the capsule. The link lands on your clipboard.", green, |c, cx, cy| {
            show(c, &drop_img, cx, cy - 350.0);
            show(c, &upload_img, cx, cy - 215.0);
            show(c, &done_img, cx, cy - 60.0);
        }),
    );

    // 4. screenshots
    let shot_song = Song { big: raster(en.snip_copied, -64, 700).unwrap(), small: raster("1920 × 1080", -48, 400), line: raster("Screenshot copied", -44, 400).unwrap(), art: None, key: String::new() };
    let mut copied_scene = capsule(ui::SONG, Kind::Song, 1.9);
    copied_scene.frame = true;
    copied_scene.song = Some(&shot_song);
    let copied_img = shoot(&copied_scene);
    save(
        "card-screenshot",
        &card("Screenshots that feel native.", "Win+Shift+S dims the screen. Drag, let go, and the picture flies into the island, copied.", orange, |c, cx, cy| {
            let (fx, fy, fw, fh) = (cx - 400.0, cy - 310.0, 800.0, 300.0);
            c.rrect_with(fx, fy, fw, fh, 26.0, 1.0, |x, y| {
                let w = wallpaper((x - fx) / fw, (y - fy) / fh);
                [w[0] * 0.42, w[1] * 0.42, w[2] * 0.42]
            });
            let (sx, sy, sw, sh) = (fx + 170.0, fy + 60.0, 460.0, 190.0);
            c.rrect_with(sx, sy, sw, sh, 3.0, 1.0, |x, y| wallpaper((x - fx) / fw, (y - fy) / fh));
            c.outline(sx - 2.0, sy - 2.0, sw + 4.0, sh + 4.0, 4.0, WHITE, 0.95);
            for (hx, hy) in [(sx, sy), (sx + sw, sy), (sx, sy + sh), (sx + sw, sy + sh)] {
                c.rrect(hx - 11.0, hy - 11.0, 22.0, 22.0, 11.0, WHITE, 1.0);
            }
            c.shadow(sx, sy - 70.0, 200.0, 50.0, 25.0, 24.0, 0.35);
            c.rrect(sx, sy - 70.0, 200.0, 50.0, 25.0, [0.0; 3], 0.94);
            let dims = type_bmp("500 × 230", 28, 600);
            c.text_center(&dims, sx + 100.0, sy - 45.0, WHITE, WHITE, 1.0);
            show(c, &copied_img, cx, cy + 20.0);
        }),
    );

    // 5. notifications
    let note_song = Song { big: raster("Deploy finished", -64, 700).unwrap(), small: raster("production · 2 minutes ago", -48, 400), line: raster("Deploy finished", -44, 400).unwrap(), art: None, key: String::new() };
    let mut bell_scene = capsule(ui::SONG, Kind::Song, 2.2);
    bell_scene.bell = true;
    bell_scene.song = Some(&note_song);
    let bell_img = shoot(&bell_scene);
    let device_line = raster("Headphones (Arctis Nova)", -44, 400);
    let mut device_scene = capsule((ui::VOLUME_W, ui::VOLUME_H_SUB), Kind::Volume, 2.2);
    device_scene.volume = 0.4;
    device_scene.sub = device_line.as_ref();
    let device_img = shoot(&device_scene);
    save(
        "card-notify",
        &card("Heard. Seen. Handled.", "Push messages from ntfy, device changes and new songs arrive as calm little capsules.", violet, |c, cx, cy| {
            show(c, &bell_img, cx, cy - 330.0);
            show(c, &device_img, cx, cy - 100.0);
        }),
    );

    // 5b. the Razer mouse
    let pic = crate::mouse::picture(crate::mouse::Model::ViperV3Pro);
    let (dpi, dpi_label) = (raster("1600", -100, 700), raster("DPI", -48, 700));
    let (name, linked, pct) = (raster("Viper V3 Pro", -64, 700), raster(en.mouse_connected, -48, 400), raster("82%", -60, 700));
    let (low, low_pct, crit, crit_pct) = (raster(en.battery_low, -66, 700), raster("22%", -60, 700), raster(en.battery_critical, -66, 700), raster("4%", -60, 700));
    let mouse = |kind: Kind, size: (f32, f32), view: MouseView<'static>| {
        let mut sc = capsule(size, kind, 1.55);
        sc.mouse = Some(view);
        shoot(&sc)
    };
    let base = |level: u8, fill: f32| MouseView { picture: pic, dpi: None, dpi_old: None, roll: 1.0, dpi_label: None, stage: 1.0, stages: 3, title: None, sub: None, warn: None, percent: None, fill, level, charging: false, intro: 1.0 };
    // leak the bitmaps: the pictures live as long as the program anyway
    let keep = |t: Option<TextBmp>| -> Option<&'static TextBmp> { t.map(|t| &*Box::leak(Box::new(t))) };
    let (dpi, dpi_label, name, linked, pct, low, low_pct, crit, crit_pct) = (keep(dpi), keep(dpi_label), keep(name), keep(linked), keep(pct), keep(low), keep(low_pct), keep(crit), keep(crit_pct));
    let link_img = mouse(Kind::MouseLink, ui::MOUSE_LINK, MouseView { title: name, sub: linked, percent: pct, ..base(0, 0.82) });
    let charging_txt = keep(raster(en.mouse_charging, -48, 400));
    let charge_img = {
        let mut sc = capsule(ui::MOUSE_LINK, Kind::MouseLink, 1.55);
        sc.marq = 0.62;
        sc.mouse = Some(MouseView { picture: crate::mouse::picture(crate::mouse::Model::Superlight2), title: keep(raster("Superlight 2", -64, 700)), sub: charging_txt, percent: keep(raster("46%", -60, 700)), charging: true, ..base(0, 0.46) });
        shoot(&sc)
    };
    let dpi_img = mouse(Kind::MouseDpi, ui::MOUSE_DPI, MouseView { picture: crate::mouse::picture(crate::mouse::Model::AtkA9Plus), dpi, dpi_label, stage: 1.0, stages: 4, ..base(0, 0.82) });
    let low_img = mouse(Kind::MouseBattery, ui::MOUSE_BATTERY, MouseView { warn: low, percent: low_pct, ..base(1, 0.22) });
    let crit_img = mouse(Kind::MouseBattery, ui::MOUSE_BATTERY, MouseView { warn: crit, percent: crit_pct, ..base(2, 0.04) });
    save(
        "card-mouse",
        &card("Your mouse, at a glance.", "Razer, Logitech or ATK, no extra software: a hello on wake, the DPI as it changes, a warning before it runs out.", green, |c, cx, cy| {
            for (i, img) in [&link_img, &charge_img, &dpi_img, &low_img, &crit_img].into_iter().enumerate() {
                show(c, img, cx, cy - 410.0 + i as f32 * 120.0);
            }
        }),
    );

    // 6. themes and settings
    let labels: Vec<TextBmp> = en.labels.iter().filter_map(|l| raster(l, -52, 400)).collect();
    let values: Vec<(TextBmp, bool)> = [("my-private-topic", false), ("https://ntfy.sh", false), ("click to type", true), ("notification.mp3", false), ("Auto", false), ("Auto", false)]
        .iter()
        .map(|(t, ph)| (raster(t, -48, 400).unwrap(), *ph))
        .collect();
    let mut toggles = [1.0f32; ui::TOGGLES];
    toggles[4] = 0.0;
    toggles[5] = 0.0;
    let settings_scene = |pal: Palette, unit: f32| {
        let mut s = scene(ui::SETTINGS.0, ui::SETTINGS.1, ui::CAPSULE_RADIUS, Kind::Settings, unit);
        s.pal = pal;
        s.settings = Some(SettingsView { labels: &labels, toggles, hover: None, step: 2, notify_volume: 20, values: &values, editing: None, sound_set: true });
        shoot(&s)
    };
    let dark_settings = settings_scene(DARK, 0.86);
    let light_settings = settings_scene(LIGHT, 0.86);
    save(
        "card-settings",
        &card("Yours to shape.", "Light or dark. Two languages. Every feature has a switch, and nothing runs that you turned off.", teal, |c, cx, cy| {
            c.place_center(&dark_settings, (cx - 245.0) as i32, (cy - 385.0) as i32);
            c.place_center(&light_settings, (cx + 245.0) as i32, (cy - 385.0) as i32);
        }),
    );

    // ---------------------------------------------------------------- numbers
    let mut stats = Canvas::solid(2400, 640, [0.0, 0.0, 0.0]);
    let facts = [("0.7 MB", "one file. No installer,\nno runtime."), ("~0%", "CPU when nothing is\nhappening."), ("100 FPS", "paced by your display's\nvertical blank."), ("0", "telemetry. Nothing phones\nhome unless you ask.")];
    for (i, (big, small_text)) in facts.iter().enumerate() {
        let cx = 300.0 + i as f32 * 600.0;
        let b = type_bmp(big, 130, 600);
        stats.text_center(&b, cx, 250.0, WHITE, [0.55, 0.55, 0.62], 1.0);
        for (j, line) in small_text.lines().enumerate() {
            let l = type_bmp(line, 38, 400);
            stats.text_center(&l, cx, 380.0 + j as f32 * 54.0, GRAY, GRAY, 1.0);
        }
    }
    save("stats", &stats);

    // ---------------------------------------------------------------- animations
    // a song change in the pinned capsule: red -> blue -> red
    let (aw, ah) = (1500usize, 330usize);
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    for step in 0..110 {
        let (from, to, k, tint) = if step < 55 {
            (&song_red, &song_blue, ((step as f32 - 14.0) / 30.0).clamp(0.0, 1.0), if step < 30 { red } else { blue })
        } else {
            (&song_blue, &song_red, ((step as f32 - 69.0) / 30.0).clamp(0.0, 1.0), if step < 85 { blue } else { red })
        };
        let t = step as f32 / 30.0;
        let bars = std::array::from_fn(|i| 0.15 + 0.85 * (0.5 + 0.5 * (t * (5.0 + i as f32 * 1.7) + i as f32 * 1.3).sin()) * (0.55 + 0.45 * (t * 2.1 + i as f32).sin().abs()));
        let mut sc = capsule(ui::PINNED, Kind::Pinned, 4.2);
        sc.pin = Some(if k <= 0.0 {
            PinView { song: Some(from), prev: None, swap: 1.0, bars, lyric: None, lyric_old: None, lyric_swap: 1.0, lyric_fill: None, tall: false }
        } else {
            PinView { song: Some(to), prev: if k < 1.0 { Some(from) } else { None }, swap: k, bars, lyric: None, lyric_old: None, lyric_swap: 1.0, lyric_fill: None, tall: false }
        });
        let img = shoot_in(&sc, aw, ah);
        let mut c = Canvas::solid(aw, ah, [0.04, 0.04, 0.05]);
        c.glow(aw as f32 / 2.0, ah as f32 / 2.0, 700.0, 150.0, tint, 0.20);
        c.place(&img, 0, 0);
        frames.push(c);
        delays.push(33);
    }
    write_apng(&format!("{dir}/song-change.png"), &frames, &delays);

    // the volume capsule turning into the knob
    let (kw, kh) = (820usize, 780usize);
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let steps = 72;
    for step in 0..steps {
        let u = (step as f32 / 44.0).min(1.0);
        let e = if u >= 1.0 { 1.0 } else { 1.0 - (-6.0 * u).exp() * ((u * 11.0).cos() + 0.55 * (u * 11.0).sin()) };
        let (w, h) = (lerp(ui::VOLUME_W, ui::KNOB.0, e), lerp(ui::VOLUME_H, ui::KNOB.1, e.min(1.08)));
        let radius = lerp(ui::VOLUME_H / 2.0, ui::KNOB_RADIUS, e.clamp(0.0, 1.0)).min(h / 2.0).min(w / 2.0);
        let mut sc = scene(w, h, radius, Kind::Volume, 2.6);
        sc.volume = lerp(0.18, 0.78, sstep(0.0, 1.0, (step as f32 - 10.0) / 40.0));
        sc.layers = [Some(Layer::leaving(Kind::Volume, (u / 0.35).min(1.0))), Some(Layer::entering(Kind::Knob, ((u - 0.4) / 0.5).clamp(0.0, 1.0)))];
        sc.song = Some(&knob_song);
        let img = shoot_in(&sc, kw, kh);
        let mut c = Canvas::solid(kw, kh, [0.04, 0.04, 0.05]);
        c.glow(kw as f32 / 2.0, 330.0, 420.0, 330.0, blue, 0.18);
        c.place(&img, 0, 70);
        frames.push(c);
        delays.push(if step == steps - 1 { 1600 } else { 33 });
    }
    write_apng(&format!("{dir}/knob-morph.png"), &frames, &delays);

    println!("pictures written to {dir}");
}
