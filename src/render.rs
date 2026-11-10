// Software SDF renderer for the capsule window. Output: premultiplied BGRA (0xAARRGGBB).
// The window shows one animated shape (a pill that can grow into a disc) with different content:
// volume bar, song, settings or the round knob. Geometry is in logical units; `unit` converts to
// device px. Only a rectangle around the shape (plus its shadow) is redrawn each frame.

use crate::gfx::*;
use crate::media::{Song, ART_PX};
use crate::text::{TextBmp, TEXT_R};
use crate::theme::Palette;
use crate::ui::{self, SHADOW, TOP};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Volume,
    Song,
    Settings,
    Knob,
    Upload,
    History,
    Setup,
    Drop, // "drop to upload" while a file is dragged
    Pinned, // the media pinned: cover, title and artist, visualizer
}

/// What the pinned capsule shows.
pub struct PinView<'a> {
    pub song: Option<&'a Song>,
    pub prev: Option<&'a Song>, // the song being replaced, while the change animates
    pub swap: f32,              // 0..1 through that animation
    pub bars: [f32; 5],         // visualizer, 0..1
    pub lyric: Option<&'a TextBmp>,     // the current lyric line; it takes the title's place (None: title and artist)
    pub lyric_old: Option<&'a TextBmp>, // the line it replaces, while `lyric_swap` < 1 (None: the title)
    pub lyric_swap: f32,                // 0..1 through that change
}

pub struct SettingsView<'a> {
    pub labels: &'a [TextBmp],
    pub toggles: [f32; ui::TOGGLES], // 0..1, animated
    pub hover: Option<usize>,
    pub step: u32,
    pub notify_volume: u32,
    /// Text of the topic, server, token and sound rows; `true` marks a dim placeholder.
    pub values: &'a [(TextBmp, bool)],
    pub editing: Option<usize>, // settings row being typed into
    pub sound_set: bool,        // a custom notification sound is chosen (shows the reset "x")
}

#[derive(Clone, Copy, PartialEq)]
pub enum UploadPhase {
    Uploading,
    Done,
    Failed,
}

pub struct UploadView<'a> {
    pub phase: UploadPhase,
    pub progress: f32, // 0..1
    pub name: Option<&'a TextBmp>, // file being uploaded
    pub info: Option<&'a TextBmp>, // "12.3 MB/s · 2/5"
    pub msg: Option<&'a TextBmp>,  // result headline
    pub line: Option<&'a TextBmp>, // link or error under it
}

pub struct HistoryView<'a> {
    pub title: Option<&'a TextBmp>,
    pub status: Option<&'a TextBmp>, // "Copied"
    pub empty: Option<&'a TextBmp>,
    pub rows: &'a [(TextBmp, TextBmp)], // name, "date · link"
    pub hover: Option<usize>,
    pub flash: Option<(usize, f32)>, // row that was just copied, fading 1 -> 0
    pub scroll: usize,               // index of the first visible entry
    pub total: usize,                // all entries
}

pub struct SetupView<'a> {
    /// title, subtitle, autostart, update automatically, language, theme, install, run labels
    pub texts: &'a [TextBmp],
    pub values: &'a [TextBmp], // language and theme names
    pub toggles: [f32; 2],     // autostart, update automatically: 0..1
    pub hover: Option<usize>,  // 0..3 rows, 4 install, 5 run
}

pub struct Scene<'a> {
    pub pal: Palette,
    pub unit: f32,
    pub w: f32, // shape size, logical
    pub h: f32,
    pub radius: f32,
    pub alpha: f32, // whole shape
    pub layers: [Option<Layer>; 2], // content on show: the one leaving (drawn first) and the one arriving
    pub marq: f32,    // seconds, drives scrolling text
    pub volume: f32,
    pub muted: bool,
    pub icon: Option<(Icon, f32)>, // transport icon replacing the speaker / digits, with progress
    pub bell: bool,                // song capsule shows a bell (push notification) instead of a note
    pub frame: bool,               // ... or a clipboard (a screenshot was copied)
    pub check: bool,               // ... or a check mark (it was pasted)
    pub drop_hint: Option<&'a TextBmp>,
    pub pin: Option<PinView<'a>>,
    pub sub: Option<&'a TextBmp>,  // line under the volume bar
    pub song: Option<&'a Song>,
    pub settings: Option<SettingsView<'a>>,
    pub upload: Option<UploadView<'a>>,
    pub history: Option<HistoryView<'a>>,
    pub setup: Option<SetupView<'a>>,
}

/// One piece of content on show with its opacity: old content fades out, new content fades in.
#[derive(Clone, Copy)]
pub struct Layer {
    pub kind: Kind,
    pub alpha: f32,
}

impl Layer {
    /// `p`: 0 = in place, 1 = gone.
    pub fn leaving(kind: Kind, p: f32) -> Layer {
        Layer { kind, alpha: 1.0 - sstep(0.0, 1.0, p.clamp(0.0, 1.0)) }
    }

    /// `q`: 0 = just started, 1 = settled.
    pub fn entering(kind: Kind, q: f32) -> Layer {
        Layer { kind, alpha: sstep(0.0, 1.0, q.clamp(0.0, 1.0)) }
    }
}

pub const ICON_SECS: f32 = 0.9;

const MARQ_DELAY: f32 = 0.6;
const MARQ_SPEED: f32 = 40.0; // logical units per second
const MARQ_GAP: f32 = 36.0;
const MARQ_FADE: f32 = 14.0;
const SHADOW_DROP: f32 = 3.0;
const SHADOW_REACH: f32 = 14.0;
const SHADOW_ALPHA: f32 = 0.28;

// knob geometry, relative to the disc center
const RING_R: f32 = 108.0;
const RING_HW: f32 = 3.5;
const TICK_R: f32 = 88.0;
const KNOB_BIG_VIEW: f32 = 116.0;
const KNOB_SMALL_VIEW: f32 = 84.0;

/// Device-pixel rectangle (x0, y0, x1, y1) that a shape of this size touches. `slide` is how far
/// below the window top the shape sits in addition to TOP (the window itself moves, so it is 0).
pub fn bounds(pw: usize, ph: usize, unit: f32, w: f32, h: f32) -> (usize, usize, usize, usize) {
    let cx = pw as f32 / 2.0;
    let half = w / 2.0 + SHADOW;
    let x0 = ((cx - half * unit).floor().max(0.0) as usize).min(pw);
    let x1 = ((cx + half * unit).ceil().max(0.0) as usize).min(pw);
    let y1 = (((TOP + h + SHADOW) * unit).ceil().max(0.0) as usize).min(ph);
    (x0, 0, x1, y1)
}

// ---- text ------------------------------------------------------------------

/// Coverage of a left-aligned text bitmap. `x` runs from the text's left edge, `ty` from the row center.
/// Text wider than `view` scrolls and fades at both ends.
fn text_left(tb: &TextBmp, x: f32, ty: f32, view: f32, marq: f32, unit: f32) -> f32 {
    let (wl, hl) = tb.size();
    if x < -1.0 || x > view + 1.0 || ty.abs() > hl / 2.0 + 1.0 {
        return 0.0;
    }
    let scrolls = wl > view;
    if !scrolls && x > wl + 1.0 {
        return 0.0;
    }
    let o = 0.25 / unit; // 2x2 supersampling
    let mut acc = 0.0;
    for (ox, oy) in [(-o, -o), (o, -o), (-o, o), (o, o)] {
        let u = if scrolls {
            (x + ox + (marq - MARQ_DELAY).max(0.0) * MARQ_SPEED).rem_euclid(wl + MARQ_GAP)
        } else {
            x + ox
        };
        acc += tb.sample(u * TEXT_R, (ty + oy + hl / 2.0) * TEXT_R);
    }
    if acc == 0.0 {
        return 0.0;
    }
    let mut a = acc / 4.0;
    if scrolls {
        a *= sstep(0.0, MARQ_FADE, x) * sstep(view, view - MARQ_FADE, x);
    }
    a.powf(0.85)
}

/// Same, centered on `dx = 0` (the knob's rows).
fn text_center(tb: &TextBmp, dx: f32, ty: f32, view: f32, marq: f32, unit: f32) -> f32 {
    let (wl, _) = tb.size();
    if wl > view {
        text_left(tb, dx + view / 2.0, ty, view, marq, unit)
    } else {
        text_left(tb, dx + wl / 2.0, ty, wl, marq, unit)
    }
}

/// Centered inside the box that starts `left` units from the shape's left edge and is `view` wide
/// (text wider than the box scrolls like `text_left`).
fn text_mid(tb: &TextBmp, tx: f32, ty: f32, left: f32, view: f32, marq: f32, unit: f32) -> f32 {
    let wl = tb.size().0;
    if wl > view {
        text_left(tb, tx - left, ty, view, marq, unit)
    } else {
        text_left(tb, tx - left - (view - wl) / 2.0, ty, wl, 0.0, unit)
    }
}

// ---- scene -----------------------------------------------------------------

struct KnobGeom {
    track_caps: [(f32, f32); 2],
    prog_caps: [(f32, f32); 2],
    ticks: [(f32, f32); 28],
    prog: f32,
}

pub fn render(buf: &mut [u32], pw: usize, ph: usize, rect: (usize, usize, usize, usize), s: &Scene) {
    let unit = s.unit;
    let aa = 1.0 / unit;
    let cx = pw as f32 / 2.0;
    let (hw, hh) = (s.w / 2.0, s.h / 2.0);
    let radius = s.radius.min(hw).min(hh);
    let volume = Digits::percent(s.volume);
    let steppers = [
        Digits::number(s.settings.as_ref().map_or(0, |v| v.step)),
        Digits::number(s.settings.as_ref().map_or(0, |v| v.notify_volume)),
    ];

    let up_digits = Digits::percent(s.upload.as_ref().map_or(0.0, |u| u.progress));
    let prog = s.volume.clamp(0.0, 1.0) * ARC_SWEEP;
    let knob = KnobGeom {
        track_caps: arc_caps(RING_R, ARC_SWEEP),
        prog_caps: arc_caps(RING_R, prog),
        ticks: std::array::from_fn(|i| {
            let (sn, cs) = (ARC_START + i as f32 * 10.0).to_radians().sin_cos();
            (TICK_R * cs, TICK_R * sn)
        }),
        prog,
    };
    let (x0, y0, x1, y1) = rect;

    // what every layer of content draws at a point (lx, ly: from the shape's center); `spans` tell which
    // stretches of the current row a layer can touch at all
    macro_rules! content {
      ($p:expr, $lx:expr, $ly:expr, $spans:expr) => {{
        let (p, lx, ly, spans): (&mut Px, f32, f32, &[Option<Spans>; 2]) = ($p, $lx, $ly, $spans);
        let (tx, ty) = (lx + hw, ly + hh);
        for (layer, span) in s.layers.iter().zip(spans) {
            let Some(layer) = layer.filter(|l| l.alpha > 0.01) else { continue };
            if span.as_ref().is_some_and(|sp| !sp.contains(tx)) {
                continue;
            }
            let ca = layer.alpha;
            match layer.kind {
                Kind::Volume => volume_content(p, tx, ty, aa, s, ca, &volume),
                Kind::Song => song_content(p, tx, ty, aa, s, ca),
                Kind::Settings => settings_content(p, tx, ty, aa, s, ca, &steppers),
                Kind::Knob => knob_content(p, lx, ly, aa, s, ca, &knob, &volume),
                Kind::Upload => upload_content(p, tx, ty, aa, s, ca, &up_digits),
                Kind::History => history_content(p, tx, ty, aa, s, ca),
                Kind::Setup => setup_content(p, tx, ty, aa, s, ca),
                Kind::Drop => drop_content(p, tx, ty, aa, s, ca),
                Kind::Pinned => pinned_content(p, tx, ty, aa, s, ca),
            }
        }
      }};
    }

    // rows are independent: big areas are shaded by a few threads, each on its own band of rows
    let shade_rows = |rows: &mut [u32], first: usize| {
      for (i, row) in rows.chunks_mut(pw).enumerate() {
        let y = first + i;
        let ty = (y as f32 + 0.5) / unit - TOP; // from the shape's top edge
        let ly = ty - hh;
        // where this row is plain, opaque shape: no distance field needed there, only the content
        let (solid_from, solid_to) = solid_span(ly, hw, hh, radius, cx, unit);
        let spans = [
            s.layers[0].filter(|l| l.alpha > 0.01).and_then(|l| row_spans(l.kind, s, ty)),
            s.layers[1].filter(|l| l.alpha > 0.01).and_then(|l| row_spans(l.kind, s, ty)),
        ];
        for x in x0..x1 {
            let lx = (x as f32 + 0.5 - cx) / unit;
            if x >= solid_from && x < solid_to {
                let mut p = Px::default();
                p.over(s.pal.bg, 1.0);
                content!(&mut p, lx, ly, &spans);
                row[x] = p.pack(s.alpha);
                continue;
            }
            let sdf = rrect(lx, ly, hw, hh, radius);
            let mut p = Px::default();

            // faint, tight drop shadow: just enough to lift the shape off busy backgrounds
            // (not needed where the body is opaque: the rest of the shape, nearly all of it)
            let body = cov(sdf, aa);
            if body < 1.0 {
                let shadow_d = rrect(lx, ly - SHADOW_DROP, hw, hh, radius);
                if shadow_d < SHADOW_REACH {
                    p.over([0.0; 3], SHADOW_ALPHA * (1.0 - sstep(0.0, SHADOW_REACH, shadow_d)).powi(2));
                }
            }
            if body > 0.0 {
                p.over(s.pal.bg, body);
                if s.pal.light {
                    p.over([0.0; 3], 0.10 * cov(sdf.abs() - 0.5, aa));
                }
                if sdf < -1.0 {
                    content!(&mut p, lx, ly, &spans);
                }
            }
            row[x] = p.pack(s.alpha);
        }
      }
    };
    let rows = y1.saturating_sub(y0);
    let threads = if (x1 - x0) * rows > PARALLEL_MIN_PIXELS { worker_threads() } else { 1 };
    let band = &mut buf[y0 * pw..y1 * pw];
    if threads <= 1 {
        shade_rows(band, y0);
    } else {
        let per = rows.div_ceil(threads);
        std::thread::scope(|scope| {
            for (i, chunk) in band.chunks_mut(per * pw).enumerate() {
                let shade_rows = &shade_rows;
                scope.spawn(move || shade_rows(chunk, y0 + i * per));
            }
        });
    }
    lyric_pass(buf, pw, ph, rect, s);
}

/// How far below the capsule the lyric line reaches (logical units): the redraw area grows by this much.
pub const LYRIC_BAND: f32 = 34.0;
const LYRIC_TRAVEL: f32 = 15.0; // how far a line slides while it changes

/// The lyric line under the pinned capsule, outside it: white with a soft shadow so it reads on any background,
/// at most 90% of the capsule's width (a longer line is scaled down to fit). Lines cross-fade as they change.
fn lyric_pass(buf: &mut [u32], pw: usize, ph: usize, rect: (usize, usize, usize, usize), s: &Scene) {
    let Some(v) = &s.pin else { return };
    let shown = s.layers.iter().flatten().filter(|l| l.kind == Kind::Pinned).map(|l| l.alpha).fold(0.0f32, f32::max);
    let base = shown * s.alpha;
    if base <= 0.003 || (v.lyric.is_none() && v.lyric_old.is_none()) {
        return;
    }
    let e = sstep(0.0, 1.0, v.lyric_swap);
    if v.lyric_swap < 1.0 {
        // the old line leaves upward, the new one rises in from below; both are cut off at the band's edges
        let new = 1.0 - (1.0 - v.lyric_swap).powi(3);
        lyric_line(buf, pw, ph, rect, s, v.lyric_old, base, -e * LYRIC_TRAVEL);
        lyric_line(buf, pw, ph, rect, s, v.lyric, base, (1.0 - new) * LYRIC_TRAVEL);
    } else {
        lyric_line(buf, pw, ph, rect, s, v.lyric, base, 0.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn lyric_line(buf: &mut [u32], pw: usize, ph: usize, rect: (usize, usize, usize, usize), s: &Scene, line: Option<&TextBmp>, alpha: f32, dy: f32) {
    let Some(tb) = line else { return };
    if alpha <= 0.003 {
        return;
    }
    let unit = s.unit;
    let (wl, hl) = tb.size();
    let k = (0.9 * s.w / wl).min(1.0);
    let (dw, dh) = (wl * k * unit, hl * k * unit);
    let left = pw as f32 / 2.0 - dw / 2.0;
    let top = (TOP + s.h + 17.0 + dy) * unit - dh / 2.0;
    let scale = TEXT_R / (unit * k); // device pixels -> raster pixels
    let cover = |x: f32, y: f32| tb.sample((x - left) * scale, (y - top) * scale);
    let (x0, y0, x1, y1) = rect;
    let rows = ((top - 4.0).floor().max(y0 as f32) as usize)..((top + dh + 4.0).ceil().min(y1.min(ph) as f32) as usize);
    let cols = ((left - 4.0).floor().max(x0 as f32) as usize)..((left + dw + 4.0).ceil().min(x1.min(pw) as f32) as usize);
    let blend = |px: &mut u32, rgb: f32, a: f32| {
        if a <= 0.0 {
            return;
        }
        let ch = |shift: u32| ((*px >> shift) & 0xFF) as f32;
        let out = |shift: u32| (rgb * 255.0 * a + ch(shift) * (1.0 - a) + 0.5).min(255.0) as u32;
        *px = out(24) << 24 | out(16) << 16 | out(8) << 8 | out(0);
    };
    let d = 1.6 * unit.max(1.0);
    // the band under the capsule: a line fades out where it meets either edge
    let (band_top, band_bottom) = ((TOP + s.h) * unit, (TOP + s.h + LYRIC_BAND) * unit);
    let edge = 6.0 * unit;
    for y in rows {
        let m = sstep(band_top, band_top + edge, y as f32 + 0.5) * sstep(band_bottom, band_bottom - edge, y as f32 + 0.5);
        if m <= 0.0 {
            continue;
        }
        let alpha = alpha * m;
        for x in cols.clone() {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let text = cover(fx, fy);
            let soft = (cover(fx - d, fy + d) + cover(fx + d, fy + d) + cover(fx, fy + 1.8 * d) + cover(fx, fy - 0.2 * d)) / 4.0;
            let px = &mut buf[y * pw + x];
            blend(px, 0.0, (soft * 0.75 * alpha).min(1.0));
            blend(px, 1.0, (text * alpha).min(1.0));
        }
    }
}

/// The device-pixel columns [from, to) of a row (`ly` from the shape's center) that lie well inside the
/// rounded rectangle: the distance field is below -1.5 there, so the body is opaque, the border and the
/// shadow are out of reach, and content may be drawn.
fn solid_span(ly: f32, hw: f32, hh: f32, radius: f32, cx: f32, unit: f32) -> (usize, usize) {
    let (dy, inner) = (ly.abs(), hh - radius);
    let half = if dy <= inner {
        hw - 1.5
    } else {
        let (t, r) = (dy - inner, radius - 1.5);
        if t >= r {
            return (0, 0);
        }
        (hw - radius) + (r * r - t * t).sqrt()
    };
    if half <= 0.0 {
        return (0, 0);
    }
    let from = (cx - half * unit - 0.5).ceil().max(0.0) as usize;
    let to = ((cx + half * unit - 0.5).floor() + 1.0).max(0.0) as usize;
    (from, to.max(from))
}

/// Stretches (in logical units from the shape's left edge) of one row that a content layer can draw on.
struct Spans {
    n: usize,
    at: [(f32, f32); 6],
}

impl Spans {
    fn new() -> Spans {
        Spans { n: 0, at: [(0.0, 0.0); 6] }
    }

    fn add(&mut self, from: f32, to: f32) {
        if self.n < self.at.len() {
            self.at[self.n] = (from, to);
            self.n += 1;
        }
    }

    fn contains(&self, x: f32) -> bool {
        self.at[..self.n].iter().any(|&(a, b)| x >= a && x <= b)
    }
}

/// `None`: no idea, draw everywhere. Every stretch is a little wider than what the layer can reach.
fn row_spans(kind: Kind, s: &Scene, ty: f32) -> Option<Spans> {
    match kind {
        Kind::Settings => {
            let v = s.settings.as_ref()?;
            let mut sp = Spans::new();
            let Some(row) = ui::row_at(ty) else { return Some(sp) };
            let ry = (ty - ui::PAD - row as f32 * ui::ROW_H - ui::ROW_H / 2.0).abs(); // from the row center
            let w = ui::SETTINGS.0;
            let field_row = ui::value_index(row).is_some();
            if v.hover == Some(row) {
                sp.add(6.0, w - 6.0);
            }
            if let Some(label) = v.labels.get(row) {
                let (wl, hl) = label.size();
                let view = if field_row { ui::VALUE_LEFT - 30.0 } else { 210.0 };
                if ry <= hl / 2.0 + 2.0 {
                    sp.add(20.0, 24.0 + wl.min(view));
                }
            }
            if row < ui::TOGGLES {
                if ry < 13.0 {
                    sp.add(w - 40.0 - 23.0, w - 40.0 + 23.0);
                }
            } else if row == ui::ROW_STEP || row == ui::ROW_VOLUME {
                if ry < 13.0 {
                    sp.add(w - ui::STEP_MINUS_X - 14.0, w - ui::STEP_MINUS_X + 14.0);
                    sp.add(w - ui::STEP_PLUS_X - 14.0, w - ui::STEP_PLUS_X + 14.0);
                    sp.add(w - ui::STEP_VALUE_X - 16.0, w - ui::STEP_VALUE_X + 16.0);
                }
            } else if field_row {
                let (c, half) = (ui::VALUE_LEFT + ui::VALUE_VIEW / 2.0, ui::VALUE_VIEW / 2.0 + 7.0 + 3.0);
                if ry < 13.0 {
                    sp.add(c - half, c + half);
                }
                if ry < 6.0 {
                    sp.add(w - 22.0 - 7.0, w - 22.0 + 7.0); // the "x" of the sound row
                }
            }
            Some(sp)
        }
        _ => None,
    }
}

/// Smaller areas are not worth the thread start-up.
const PARALLEL_MIN_PIXELS: usize = 40_000;

fn worker_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 6))
}

/// Transport icon animation: (icon, opacity, scale, slide offset) for progress `u`.
fn icon_anim(s: &Scene, strength: f32) -> Option<(Icon, f32, f32, f32)> {
    let (icon, u) = s.icon?;
    let ia = sstep(0.0, 0.15, u) * (1.0 - sstep(0.72, 1.0, u));
    let e = u * u * (3.0 - 2.0 * u);
    let slide = (-1.0 + 2.0 * e) * strength;
    let (off, base) = match icon {
        Icon::Prev => (-slide, 0.85 + 0.15 * sstep(0.0, 0.2, u)),
        Icon::Next => (slide, 0.85 + 0.15 * sstep(0.0, 0.2, u)),
        _ => (0.0, 0.7 + 0.3 * sstep(0.0, 0.3, u)),
    };
    Some((icon, ia, base, off))
}

/// Speaker / note glyph in the left slot, or the transport icon that replaces it for a moment.
fn slot_icon(p: &mut Px, lx: f32, ly: f32, aa: f32, s: &Scene, ca: f32, glyph: f32) {
    let pal = &s.pal;
    let anim = icon_anim(s, 9.0);
    let ia = anim.map_or(0.0, |a| a.1);
    if ia < 0.99 {
        p.over(pal.fg, ca * (1.0 - ia) * cov(glyph, aa));
    }
    if let Some((icon, ia, base, off)) = anim.filter(|a| a.1 > 0.0) {
        let sc = 0.74 * base;
        p.over(pal.fg, ca * ia * cov(icon_sdf(icon, (lx - off) / sc, ly / sc) * sc, aa));
    }
}

fn volume_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32, digits: &Digits) {
    let pal = &s.pal;
    let row = 24.0;
    let glyph = speaker_sdf(tx - 26.0, ty - row, s.volume, s.muted);
    slot_icon(p, tx - 26.0, ty - row, aa, s, ca, glyph);

    let (x0, x1) = (52.0, (s.w - 70.0).max(100.0));
    let len = x1 - x0;
    let mid = |l: f32| x0 + l / 2.0;
    p.over(pal.mix(0.24), ca * cov(rrect(tx - mid(len), ty - row, len / 2.0, 3.0, 3.0), aa));
    let fill = (len * s.volume).max(6.0);
    if s.volume > 0.003 {
        p.over(pal.fg, ca * cov(rrect(tx - mid(fill), ty - row, fill / 2.0, 3.0, 3.0), aa));
    }

    let (xd, yd) = ((tx - (s.w - 34.0)) / 0.5, (ty - row) / 0.5);
    if xd.abs() < 44.0 && yd.abs() < 22.0 {
        p.over(pal.text, ca * 0.95 * cov((digits.dist(xd, yd) - DIGIT_STROKE) * 0.5, aa));
    }

    if let Some(sub) = s.sub {
        p.over(pal.dim, ca * 0.95 * text_mid(sub, tx, ty - 52.0, 20.0, s.w - 40.0, s.marq, 1.0 / aa));
    }
}

fn drop_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32) {
    let row = s.h / 2.0;
    p.over(s.pal.fg, ca * cov(arrow_up_sdf(tx - 26.0, -(ty - row)), aa)); // an arrow pointing down into the capsule
    if let Some(hint) = s.drop_hint {
        p.over(s.pal.text, ca * text_mid(hint, tx, ty - row, 52.0, s.w - 52.0 - 18.0, 0.0, 1.0 / aa));
    }
}

/// The pinned media: the cover on the left, title and artist in the middle (in the cover's color), a
/// visualizer on the right. When the song changes, the old cover shrinks away while the new one pops in, and
/// the old text slides out to the left as the new one slides in from the right.
fn pinned_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32) {
    let (Some(v), pal) = (&s.pin, &s.pal) else { return };
    let row = s.h / 2.0;
    let ry = ty - row;
    let k = if pal.light { 0.55 / 255.0 } else { 1.0 / 255.0 }; // darker on the light theme
    let tint = |song: Option<&Song>| song.and_then(|x| x.art.as_ref()).map_or([235.0; 3], |a| a.accent);

    // 0 = the old song, 1 = the new one
    let swap = v.swap.clamp(0.0, 1.0);
    let (old, new) = (v.prev, v.song);
    let into = sstep(0.0, 1.0, swap);
    let accent_now = std::array::from_fn::<f32, 3, _>(|c| lerp(tint(old)[c], tint(new)[c], into) * k);

    let mut layers = [(new, 1.0f32, 0.0f32, 1.0f32), (old, 0.0, 0.0, 1.0)];
    if v.prev.is_some() {
        let back = |t: f32| {
            // overshoots a little before it settles
            let t = t.clamp(0.0, 1.0) - 1.0;
            1.0 + 2.70158 * t * t * t + 1.70158 * t * t
        };
        layers[0] = (new, sstep(0.25, 1.0, swap), (1.0 - sstep(0.0, 1.0, swap)) * 18.0, 0.78 + 0.22 * back(swap)); // arriving
        layers[1] = (old, 1.0 - sstep(0.0, 0.55, swap), -sstep(0.0, 1.0, swap) * 18.0, 1.0 - 0.18 * sstep(0.0, 0.6, swap)); // leaving
    }

    for (song, alpha, slide, scale) in layers {
        if alpha <= 0.003 {
            continue;
        }
        let accent = song.and_then(|x| x.art.as_ref()).map_or([235.0; 3], |a| a.accent).map(|c| c * k);
        let accent = if v.prev.is_some() { accent_now } else { accent };
        let dim = accent.map(|c| c * 0.72);
        let ca = ca * alpha;

        // cover: a rounded square, or a plain tile with a note while there is none
        let (ax, ay) = ((tx - 26.0) / scale, ry / scale);
        if ax.abs() < 18.5 && ay.abs() < 18.5 {
            let a = cov(rrect(ax, ay, 17.0, 17.0, 9.0) * scale, aa);
            if a > 0.0 {
                match song.and_then(|x| x.art.as_ref()) {
                    Some(art) => {
                        let n = ART_PX as f32;
                        let (u, w) = ((ax + 17.0) / 34.0 * n - 0.5, (ay + 17.0) / 34.0 * n - 0.5);
                        let (u, w) = (u.clamp(0.0, n - 1.001), w.clamp(0.0, n - 1.001));
                        let (x0, y0) = (u as usize, w as usize);
                        let (fx, fy) = (u - x0 as f32, w - y0 as f32);
                        let at = |x: usize, y: usize, shift: u32| ((art.px[y * ART_PX + x] >> shift) & 0xFF) as f32;
                        let mix = |shift: u32| {
                            let top = at(x0, y0, shift) * (1.0 - fx) + at(x0 + 1, y0, shift) * fx;
                            let bot = at(x0, y0 + 1, shift) * (1.0 - fx) + at(x0 + 1, y0 + 1, shift) * fx;
                            (top * (1.0 - fy) + bot * fy) / 255.0
                        };
                        p.over([mix(16), mix(8), mix(0)], ca * a);
                    }
                    None => {
                        p.over(pal.mix(0.16), ca * a);
                        p.over(accent, ca * 0.9 * cov(note_sdf(ax, ay) * scale, aa));
                    }
                }
            }
        }

        // title (bold) over artist, centered between the cover and the visualizer
        if let Some(song) = song {
            let (left, view, unit) = (52.0, s.w - 52.0 - 50.0, 1.0 / aa);
            let x = tx - slide;
            match &song.small {
                Some(small) => {
                    p.over(accent, ca * text_mid(&song.big, x, ry + 7.0, left, view, s.marq, unit));
                    p.over(dim, ca * 0.95 * text_mid(small, x, ry - 9.0, left, view, s.marq, unit));
                }
                None => p.over(accent, ca * text_mid(&song.big, x, ry, left, view, s.marq, unit)),
            }
        }
    }

    // visualizer: five rounded bars
    let accent = accent_now;
    let vx = tx - (s.w - 27.0);
    if vx.abs() < 18.0 && ry.abs() < 14.0 {
        for (i, level) in v.bars.iter().enumerate() {
            let half = 2.0 + 9.0 * level;
            p.over(accent, ca * cov(rrect(vx - (i as f32 - 2.0) * 5.5, ry, 1.35, half, 1.35), aa));
        }
    }
}

fn song_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32) {
    let pal = &s.pal;
    let glyph = if s.check {
        check_sdf(tx - 26.0, ty - 36.0)
    } else if s.frame {
        clipboard_sdf(tx - 26.0, ty - 36.0)
    } else if s.bell {
        bell_sdf(tx - 26.0, ty - 36.0)
    } else {
        note_sdf(tx - 26.0, ty - 36.0)
    };
    slot_icon(p, tx - 26.0, ty - 36.0, aa, s, ca, glyph);
    let Some(song) = s.song else { return };
    let (left, view) = (52.0, s.w - 52.0 - 18.0);
    let unit = 1.0 / aa;
    match &song.small {
        Some(small) => {
            p.over(pal.fg, ca * text_mid(&song.big, tx, ty - 28.0, left, view, s.marq, unit));
            p.over(pal.dim, ca * 0.95 * text_mid(small, tx, ty - 51.0, left, view, s.marq, unit));
        }
        None => p.over(pal.fg, ca * text_mid(&song.big, tx, ty - 36.0, left, view, s.marq, unit)),
    }
}

/// Text that is wider than its box shows its end (where you are typing).
fn text_tail(tb: &TextBmp, x: f32, ty: f32, view: f32, unit: f32) -> f32 {
    if x < 0.0 || x > view {
        return 0.0;
    }
    let wl = tb.size().0;
    text_left(tb, x + (wl - view).max(0.0), ty, wl.max(1.0), 0.0, unit)
}

fn settings_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32, steppers: &[Digits; 2]) {
    let pal = &s.pal;
    let (Some(v), Some(row)) = (&s.settings, ui::row_at(ty)) else { return };
    let ry = ty - ui::PAD - row as f32 * ui::ROW_H - ui::ROW_H / 2.0; // from the row center
    let w = ui::SETTINGS.0;
    let unit = 1.0 / aa;
    let field_row = ui::value_index(row).is_some();

    if v.hover == Some(row) {
        p.over(pal.fg, ca * 0.13 * cov(rrect(tx - w / 2.0, ry, w / 2.0 - 8.0, ui::ROW_H / 2.0 - 2.0, 9.0), aa));
    }
    if let Some(label) = v.labels.get(row) {
        let color = if row == ui::ROW_QUIT { pal.red } else { pal.text };
        let view = if field_row { ui::VALUE_LEFT - 30.0 } else { 210.0 };
        p.over(color, ca * text_left(label, tx - 22.0, ry, view, 0.0, unit));
    }

    if row < ui::TOGGLES {
        let (lx, t) = (tx - (w - 40.0), v.toggles[row]);
        if lx.abs() < 22.0 {
            let track = pal.mix(lerp(0.26, 1.0, t));
            p.over(track, ca * cov(rrect(lx, ry, 20.0, 11.0, 11.0), aa));
            let off = pal.mix(0.72);
            let knob: [f32; 3] = std::array::from_fn(|i| lerp(off[i], pal.bg[i], t));
            p.over(knob, ca * cov((lx - lerp(-9.0, 9.0, t)).hypot(ry) - 8.5, aa));
        }
    } else if row == ui::ROW_STEP || row == ui::ROW_VOLUME {
        let step_digits = &steppers[usize::from(row == ui::ROW_VOLUME)];
        for (from_right, plus) in [(ui::STEP_MINUS_X, false), (ui::STEP_PLUS_X, true)] {
            let lx = tx - (w - from_right);
            if lx.abs() > 13.0 {
                continue;
            }
            p.over(pal.mix(0.22), ca * cov(lx.hypot(ry) - 11.0, aa));
            p.over(pal.fg, ca * cov(rrect(lx, ry, 4.5, 1.0, 1.0), aa));
            if plus {
                p.over(pal.fg, ca * cov(rrect(lx, ry, 1.0, 4.5, 1.0), aa));
            }
        }
        let (xd, yd) = ((tx - (w - ui::STEP_VALUE_X)) / 0.5, ry / 0.5);
        if xd.abs() < 30.0 && yd.abs() < 20.0 {
            p.over(pal.text, ca * 0.95 * cov((step_digits.dist(xd, yd) - DIGIT_STROKE) * 0.5, aa));
        }
    } else if field_row {
        // an input box with the value inside
        let editing = v.editing == Some(row);
        let box_x = tx - (ui::VALUE_LEFT + ui::VALUE_VIEW / 2.0);
        let wide = ui::VALUE_VIEW / 2.0 + 7.0;
        if box_x.abs() < wide + 2.0 {
            let field = rrect(box_x, ry, wide, 11.0, 6.0);
            p.over(pal.fg, ca * if editing { 0.20 } else { 0.07 } * cov(field, aa));
            if editing {
                p.over(pal.fg, ca * 0.55 * cov(field.abs() - 0.6, aa));
            }
        }
        if let Some((value, placeholder)) = ui::value_index(row).and_then(|i| v.values.get(i)) {
            let color = if *placeholder { pal.dim } else { pal.text };
            let x = tx - ui::VALUE_LEFT;
            p.over(color, ca * text_tail(value, x, ry, ui::VALUE_VIEW, unit));
            if editing && (s.marq * 1.7).fract() < 0.6 {
                let end = if *placeholder { 0.0 } else { value.size().0.min(ui::VALUE_VIEW) - 1.0 };
                p.over(pal.fg, ca * cov(rrect(x - end - 1.0, ry, 0.8, 7.0, 0.4), aa));
            }
        }
        if row == ui::ROW_SOUND && v.sound_set {
            // "x": back to the built-in chime
            let lx = tx - (w - 22.0);
            if lx.abs() < 6.0 {
                let cross = seg(lx, ry, (-3.5, -3.5), (3.5, 3.5), 0.9).min(seg(lx, ry, (-3.5, 3.5), (3.5, -3.5), 0.9));
                p.over(pal.fg, ca * 0.8 * cov(cross, aa));
            }
        }
    }
}

/// The round volume dial: ticks, track, progress arc, indicator, percent / transport icon,
/// and the artist / title rows. (dx, dy) are relative to the disc center.
fn knob_content(p: &mut Px, dx: f32, dy: f32, aa: f32, s: &Scene, ca: f32, k: &KnobGeom, digits: &Digits) {
    let pal = &s.pal;
    let d = dx.hypot(dy);
    let v = s.volume.clamp(0.0, 1.0);

    // ticks, track and progress arc
    if d > 74.0 && d < RING_R + RING_HW + 4.0 {
        let ang = (fast_atan2_deg(dy, dx) - ARC_START).rem_euclid(360.0);

        if d > 82.0 && d < 94.0 {
            let idx = (ang / 10.0).round().clamp(0.0, 27.0);
            let (tx, ty) = k.ticks[idx as usize];
            let dd = ((dx - tx).powi(2) + (dy - ty).powi(2)).sqrt() - 1.5;
            if idx * 10.0 <= k.prog + 0.5 && v > 0.003 {
                p.over(pal.fg, ca * 0.95 * cov(dd, aa));
            } else {
                p.over(pal.mix(0.30), ca * 0.85 * cov(dd, aa));
            }
        }
        if d > 90.0 {
            p.over(pal.mix(0.20), ca * cov(arc_sdf(dx, dy, d, ang, RING_R, RING_HW, ARC_SWEEP, &k.track_caps), aa));
            if v > 0.003 {
                p.over(pal.fg, ca * cov(arc_sdf(dx, dy, d, ang, RING_R, RING_HW, k.prog, &k.prog_caps), aa));
            }
        }
    }

    // indicator
    let (isn, ics) = (ARC_START + k.prog).to_radians().sin_cos();
    let (lx, ly) = (dx * ics + dy * isn, -dx * isn + dy * ics);
    if d < 72.0 && lx > 28.0 && lx < 66.0 && ly.abs() < 14.0 {
        let px = lx.clamp(41.0, 57.0);
        p.over(pal.fg, ca * cov(((lx - px).powi(2) + ly * ly).sqrt() - 2.5, aa));
    }

    // percent, crossfading with the transport icon
    if dx.abs() < 46.0 && dy.abs() < 22.0 {
        let anim = icon_anim(s, 18.0);
        let ia = anim.map_or(0.0, |a| a.1);
        if ia < 0.99 {
            p.over(pal.text, ca * 0.95 * (1.0 - ia) * cov(digits.dist(dx, dy) - DIGIT_STROKE, aa));
        }
        if let Some((icon, ia, base, off)) = anim.filter(|a| a.1 > 0.0) {
            p.over(pal.fg, ca * ia * cov(icon_sdf(icon, (dx - off) / base, dy / base) * base, aa));
        }
    }

    // title (big, white) + artist (small, dim) below the dial
    if let Some(song) = s.song {
        if dy > 78.0 && dy < 126.0 && dx.abs() < 62.0 {
            let unit = 1.0 / aa;
            let (big_cy, small_cy) = if song.small.is_some() { (91.0, 111.0) } else { (100.0, 0.0) };
            p.over(pal.fg, ca * text_center(&song.big, dx, dy - big_cy, KNOB_BIG_VIEW, s.marq, unit));
            if let Some(small) = &song.small {
                p.over(pal.dim, ca * 0.95 * text_center(small, dx, dy - small_cy, KNOB_SMALL_VIEW, s.marq, unit));
            }
        }
    }
}


/// Right-aligned text ending `from_right` units before the shape's right edge.
fn text_right(tb: &TextBmp, tx: f32, ty: f32, width: f32, from_right: f32, unit: f32) -> f32 {
    let wl = tb.size().0;
    text_left(tb, tx - (width - from_right - wl), ty, wl, 0.0, unit)
}

fn upload_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32, digits: &Digits) {
    let (Some(u), pal) = (&s.upload, &s.pal) else { return };
    let unit = 1.0 / aa;
    let (gx, gy) = (tx - 26.0, ty - 36.0);
    match u.phase {
        UploadPhase::Uploading => {
            let bob = (s.marq * 5.0).sin() * 1.8; // the arrow floats while it works
            p.over(pal.fg, ca * cov(arrow_up_sdf(gx, gy - bob), aa));
        }
        UploadPhase::Done => p.over(pal.fg, ca * cov(check_sdf(gx, gy), aa)),
        UploadPhase::Failed => p.over(pal.red, ca * cov(warn_sdf(gx, gy), aa)),
    }

    if u.phase == UploadPhase::Uploading {
        if let Some(name) = u.name {
            p.over(pal.text, ca * text_mid(name, tx, ty - 24.0, 52.0, s.w - 52.0 - 100.0, s.marq, unit));
        }
        if let Some(info) = u.info {
            p.over(pal.dim, ca * 0.95 * text_right(info, tx, ty - 24.0, s.w, 18.0, unit));
        }
        let (x0, x1) = (52.0, (s.w - 70.0).max(120.0));
        let len = x1 - x0;
        let mid = |l: f32| x0 + l / 2.0;
        p.over(pal.mix(0.24), ca * cov(rrect(tx - mid(len), ty - 50.0, len / 2.0, 3.0, 3.0), aa));
        let fill = (len * u.progress).max(6.0);
        p.over(pal.fg, ca * cov(rrect(tx - mid(fill), ty - 50.0, fill / 2.0, 3.0, 3.0), aa));
        let (xd, yd) = ((tx - (s.w - 34.0)) / 0.5, (ty - 50.0) / 0.5);
        if xd.abs() < 44.0 && yd.abs() < 22.0 {
            p.over(pal.text, ca * 0.95 * cov((digits.dist(xd, yd) - DIGIT_STROKE) * 0.5, aa));
        }
    } else {
        let color = if u.phase == UploadPhase::Failed { pal.red } else { pal.text };
        if let Some(msg) = u.msg {
            p.over(color, ca * text_mid(msg, tx, ty - 26.0, 52.0, s.w - 52.0 - 18.0, 0.0, unit));
        }
        if let Some(line) = u.line {
            p.over(pal.dim, ca * 0.95 * text_mid(line, tx, ty - 50.0, 52.0, s.w - 52.0 - 18.0, s.marq, unit));
        }
    }
}

fn history_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32) {
    let (Some(h), pal) = (&s.history, &s.pal) else { return };
    let unit = 1.0 / aa;
    let w = ui::HISTORY.0;
    let head_y = ty - (ui::PAD + ui::HEAD_H / 2.0);

    if let Some(title) = h.title {
        p.over(pal.text, ca * text_left(title, tx - 22.0, head_y, 240.0, 0.0, unit));
    }
    if let Some(status) = h.status {
        p.over(pal.dim, ca * text_right(status, tx, head_y, w, 22.0, unit));
    }

    let top = ui::PAD + ui::HEAD_H;
    if h.rows.is_empty() {
        if let Some(empty) = h.empty {
            let wl = empty.size().0;
            p.over(pal.dim, ca * text_left(empty, tx - (w - wl) / 2.0, ty - (top + 40.0), wl, 0.0, unit));
        }
        return;
    }

    // a thin scroll indicator when there is more than fits
    if h.total > h.rows.len() {
        let track = ui::HIST_ROW_H * h.rows.len() as f32 - 8.0;
        let len = (track * h.rows.len() as f32 / h.total as f32).max(16.0);
        let y0 = top + 4.0 + (track - len) * h.scroll as f32 / (h.total - h.rows.len()).max(1) as f32;
        if (tx - (w - 5.0)).abs() < 3.0 {
            p.over(pal.mix(0.28), ca * cov(rrect(tx - (w - 5.0), ty - (y0 + len / 2.0), 1.2, len / 2.0, 1.2), aa));
        }
    }

    let row = ((ty - top) / ui::HIST_ROW_H).floor();
    if row < 0.0 || row as usize >= h.rows.len() {
        return;
    }
    let i = row as usize;
    let ry = ty - top - i as f32 * ui::HIST_ROW_H - ui::HIST_ROW_H / 2.0; // from the row center

    // each entry is its own card, with a gap to the next
    let card = rrect(tx - w / 2.0, ry, w / 2.0 - 12.0, ui::HIST_ROW_H / 2.0 - 3.0, 11.0);
    let hot = h.hover == Some(i);
    p.over(pal.fg, ca * if hot { 0.17 } else { 0.07 } * cov(card, aa));
    if let Some((fi, amount)) = h.flash {
        if fi == i {
            p.over(pal.fg, ca * 0.28 * amount * cov(card, aa));
        }
    }

    // document icon, then the two text lines
    if (tx - 31.0).abs() < 8.0 {
        let icon = file_sdf(tx - 31.0, ry);
        p.over(pal.fg, ca * 0.80 * cov(icon, aa));
        for line in [-3.2, 0.4, 4.0] {
            p.over(pal.bg, ca * 0.55 * cov(rrect(tx - 31.0, ry - line, 3.2, 0.55, 0.5), aa));
        }
    }
    let (name, meta) = &h.rows[i];
    p.over(pal.text, ca * text_left(name, tx - 52.0, ry + 7.5, w - 52.0 - 44.0, 0.0, unit));
    p.over(pal.dim, ca * 0.95 * text_left(meta, tx - 52.0, ry - 8.5, w - 52.0 - 44.0, 0.0, unit));
    if hot {
        let lx = tx - (w - 28.0);
        if lx.abs() < 6.0 {
            let cross = seg(lx, ry, (-3.5, -3.5), (3.5, 3.5), 0.9).min(seg(lx, ry, (-3.5, 3.5), (3.5, -3.5), 0.9));
            p.over(pal.fg, ca * 0.8 * cov(cross, aa));
        }
    }
}

fn setup_content(p: &mut Px, tx: f32, ty: f32, aa: f32, s: &Scene, ca: f32) {
    let (Some(v), pal) = (&s.setup, &s.pal) else { return };
    let unit = 1.0 / aa;
    let w = ui::SETUP.0;
    let text = |i: usize| v.texts.get(i);

    if let Some(t) = text(0) {
        p.over(pal.text, ca * text_left(t, tx - 22.0, ty - 28.0, 300.0, 0.0, unit));
    }
    if let Some(t) = text(1) {
        p.over(pal.dim, ca * text_left(t, tx - 22.0, ty - 50.0, 300.0, 0.0, unit));
    }

    // option rows: two switches, two pickers
    let row = ((ty - ui::SETUP_HEAD) / ui::SETUP_ROW_H).floor();
    if (0.0..4.0).contains(&row) {
        let i = row as usize;
        let ry = ty - ui::SETUP_HEAD - i as f32 * ui::SETUP_ROW_H - ui::SETUP_ROW_H / 2.0;
        if v.hover == Some(i) {
            p.over(pal.fg, ca * 0.10 * cov(rrect(tx - w / 2.0, ry, w / 2.0 - 8.0, ui::SETUP_ROW_H / 2.0 - 2.0, 9.0), aa));
        }
        if let Some(label) = text(2 + i) {
            p.over(pal.text, ca * text_left(label, tx - 22.0, ry, 220.0, 0.0, unit));
        }
        if i < 2 {
            let (lx, t) = (tx - (w - 40.0), v.toggles[i]);
            if lx.abs() < 22.0 {
                p.over(pal.mix(lerp(0.26, 1.0, t)), ca * cov(rrect(lx, ry, 20.0, 11.0, 11.0), aa));
                let off = pal.mix(0.72);
                let knob: [f32; 3] = std::array::from_fn(|k| lerp(off[k], pal.bg[k], t));
                p.over(knob, ca * cov((lx - lerp(-9.0, 9.0, t)).hypot(ry) - 8.5, aa));
            }
        } else if let Some(value) = v.values.get(i - 2) {
            let box_x = tx - (w - 20.0 - 60.0);
            if box_x.abs() < 62.0 {
                p.over(pal.fg, ca * 0.07 * cov(rrect(box_x, ry, 60.0, 10.0, 6.0), aa));
            }
            p.over(pal.text, ca * text_left(value, tx - (w - 20.0 - 120.0 + 8.0), ry, 104.0, 0.0, unit));
        }
    }

    // buttons: install is filled, the other one is a quiet outline
    let by = ty - ui::SETUP_BTN_Y - ui::SETUP_BTN_H / 2.0;
    if by.abs() > ui::SETUP_BTN_H / 2.0 + 2.0 {
        return;
    }
    for (i, (x0, x1)) in [(4usize, ui::SETUP_INSTALL_X), (5usize, ui::SETUP_PORTABLE_X)] {
        let bx = tx - (x0 + x1) / 2.0;
        let button = rrect(bx, by, (x1 - x0) / 2.0, ui::SETUP_BTN_H / 2.0, 12.0);
        let hot = v.hover == Some(i);
        if i == 4 {
            p.over(pal.fg, ca * if hot { 1.0 } else { 0.92 } * cov(button, aa));
        } else {
            p.over(pal.fg, ca * if hot { 0.20 } else { 0.10 } * cov(button, aa));
        }
        if let Some(label) = text(6 + (i - 4)) {
            let wl = label.size().0;
            let color = if i == 4 { pal.bg } else { pal.text };
            p.over(color, ca * text_left(label, tx - ((x0 + x1) / 2.0 - wl / 2.0), by, wl, 0.0, unit));
        }
    }
}
