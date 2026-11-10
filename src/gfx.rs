// Drawing primitives: premultiplied pixels, SDF helpers, 7-segment digits, icons.
// Shape functions return a signed distance in logical units (negative inside).

#[derive(Clone, Copy, Default)]
pub struct Px {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Px {
    #[inline]
    pub fn over(&mut self, c: [f32; 3], a: f32) {
        if a <= 0.0 {
            return;
        }
        let inv = 1.0 - a;
        self.r = c[0] * a + self.r * inv;
        self.g = c[1] * a + self.g * inv;
        self.b = c[2] * a + self.b * inv;
        self.a = a + self.a * inv;
    }

    /// 0xAARRGGBB, every channel scaled by `fade`.
    #[inline]
    pub fn pack(self, fade: f32) -> u32 {
        let q = |v: f32| ((v * fade).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        q(self.a) << 24 | q(self.r) << 16 | q(self.g) << 8 | q(self.b)
    }
}

pub const ARC_START: f32 = 135.0; // degrees, clockwise from +x (screen space)
pub const ARC_SWEEP: f32 = 270.0;

#[inline]
pub fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Anti-aliased coverage from a signed distance; `aa` is one pixel in logical units.
#[inline]
pub fn cov(sdf: f32, aa: f32) -> f32 {
    (0.5 - sdf / aa).clamp(0.0, 1.0)
}

/// Rounded rectangle with half sizes `hw`,`hh` and corner radius `r`.
#[inline]
pub fn rrect(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (qx, qy) = (px.abs() - (hw - r), py.abs() - (hh - r));
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// Capsule line from a to b with half width `hw`.
#[inline]
pub fn seg(px: f32, py: f32, a: (f32, f32), b: (f32, f32), hw: f32) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (px - a.0, py - a.1);
    let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy).max(1e-6)).clamp(0.0, 1.0);
    ((wx - vx * t).powi(2) + (wy - vy * t).powi(2)).sqrt() - hw
}

/// Centers of the two round caps of an arc of angular extent `sweep` (degrees).
pub fn arc_caps(radius: f32, sweep: f32) -> [(f32, f32); 2] {
    let cap = |deg: f32| {
        let (s, c) = deg.to_radians().sin_cos();
        (radius * c, radius * s)
    };
    [cap(ARC_START), cap(ARC_START + sweep)]
}

/// Distance to a round-capped arc stroke of angular extent `sweep` starting at ARC_START.
/// `ang` is the pixel's angle from ARC_START in degrees (0..360); `caps` comes from `arc_caps`.
#[inline]
pub fn arc_sdf(dx: f32, dy: f32, d: f32, ang: f32, radius: f32, hw: f32, sweep: f32, caps: &[(f32, f32); 2]) -> f32 {
    if ang <= sweep {
        return (d - radius).abs() - hw;
    }
    let dist = |c: (f32, f32)| ((dx - c.0).powi(2) + (dy - c.1).powi(2)).sqrt();
    dist(caps[0]).min(dist(caps[1])) - hw
}

/// atan2 in degrees, error below 0.01 degree (the exact one dominates the ring's cost).
#[inline]
pub fn fast_atan2_deg(y: f32, x: f32) -> f32 {
    let (ax, ay) = (x.abs(), y.abs());
    let (mx, mn) = (ax.max(ay), ax.min(ay));
    if mx == 0.0 {
        return 0.0;
    }
    let a = mn / mx;
    let s = a * a;
    let mut r = (((-0.046_496_474 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a).to_degrees();
    if ay > ax {
        r = 90.0 - r;
    }
    if x < 0.0 {
        r = 180.0 - r;
    }
    if y < 0.0 { -r } else { r }
}

// ---- 7-segment digits ------------------------------------------------------

type Seg = (f32, f32, f32, f32);

const SEGMENTS: [Seg; 7] = [
    (0.0, 0.0, 1.0, 0.0),
    (1.0, 0.0, 1.0, 0.5),
    (1.0, 0.5, 1.0, 1.0),
    (0.0, 1.0, 1.0, 1.0),
    (0.0, 0.5, 0.0, 1.0),
    (0.0, 0.0, 0.0, 0.5),
    (0.0, 0.5, 1.0, 0.5),
];
const DIGIT_MASK: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

pub const DIGIT_STROKE: f32 = 2.0;
const DIGIT_W: f32 = 16.0;
const DIGIT_H: f32 = 30.0;
const DIGIT_GAP: f32 = 9.0;

/// A number as 7-segment digits, centered on the origin in a 30-unit-tall space.
pub struct Digits {
    segs: [Seg; 21],
    n: usize,
}

impl Digits {
    /// `value` is a fraction: 0.42 shows "42", 1.0 shows "100".
    pub fn percent(value: f32) -> Digits {
        Digits::number((value * 100.0).round().clamp(0.0, 100.0) as u32)
    }

    pub fn number(n: u32) -> Digits {
        let text = n.to_string();
        let (w, h) = (DIGIT_W, DIGIT_H);
        let total = text.len() as f32 * w + (text.len() as f32 - 1.0) * DIGIT_GAP;
        // a leading "1" only has ink on the right edge of its cell: center the ink, not the cell
        let ink_left = if text.starts_with('1') { w } else { 0.0 };
        let ox = -(ink_left + total) / 2.0;
        let oy = -h / 2.0;
        let mut d = Digits { segs: [(0.0, 0.0, 0.0, 0.0); 21], n: 0 };
        for (i, ch) in text.bytes().enumerate().take(3) {
            let mask = DIGIT_MASK[(ch - b'0') as usize];
            let bx = ox + i as f32 * (w + DIGIT_GAP);
            for (k, s) in SEGMENTS.iter().enumerate() {
                if mask & (1 << k) == 0 {
                    continue;
                }
                // pull each segment's ends in a little so corners do not blob together
                let (mx, my) = ((s.0 + s.2) / 2.0, (s.1 + s.3) / 2.0);
                let shrink = |v: f32, m: f32| m + (v - m) * 0.8;
                d.segs[d.n] = (
                    bx + shrink(s.0, mx) * w,
                    oy + shrink(s.1, my) * h,
                    bx + shrink(s.2, mx) * w,
                    oy + shrink(s.3, my) * h,
                );
                d.n += 1;
            }
        }
        d
    }

    /// Distance from (x, y) to the nearest segment centerline.
    #[inline]
    pub fn dist(&self, x: f32, y: f32) -> f32 {
        let mut best = f32::MAX;
        for s in &self.segs[..self.n] {
            let (vx, vy) = (s.2 - s.0, s.3 - s.1);
            let (wx, wy) = (x - s.0, y - s.1);
            let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy).max(1e-6)).clamp(0.0, 1.0);
            let (cx, cy) = (wx - vx * t, wy - vy * t);
            best = best.min(cx * cx + cy * cy);
        }
        best.sqrt()
    }
}

// ---- icons (about 24 units wide, centered on the origin) -------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Prev,
    Next,
    Play,
    Pause,
}

/// Convex triangle pointing +x with half sizes `w`,`h` and corner radius `rd`.
fn tri(px: f32, py: f32, w: f32, h: f32, rd: f32) -> f32 {
    let (w, h) = (w - rd, h - rd);
    let l = (4.0 * w * w + h * h).sqrt();
    let d1 = -px - w;
    let d2 = (h * (px + w) - 2.0 * w * (py + h)) / l;
    let d3 = (h * (px + w) + 2.0 * w * (py - h)) / l;
    d1.max(d2).max(d3) - rd
}

pub fn icon_sdf(icon: Icon, x: f32, y: f32) -> f32 {
    let double = |x: f32| tri(x - 7.0, y, 7.0, 9.0, 1.6).min(tri(x + 7.0, y, 7.0, 9.0, 1.6));
    match icon {
        Icon::Prev => double(-x),
        Icon::Next => double(x),
        Icon::Play => tri(x, y, 9.0, 11.0, 1.8),
        Icon::Pause => rrect(x - 6.0, y, 2.8, 9.5, 1.2).min(rrect(x + 6.0, y, 2.8, 9.5, 1.2)),
    }
}

/// Speaker with 0..2 sound waves, or a cross when muted.
pub fn speaker_sdf(x: f32, y: f32, level: f32, muted: bool) -> f32 {
    let body = rrect(x + 7.2, y, 2.4, 3.4, 1.0);
    let h = 3.4 + (x + 5.0) * (6.6 / 7.0);
    let cone = ((-(x + 5.0)).max(x - 2.0)).max(y.abs() - h) * 0.85;
    let mut d = body.min(cone);
    if muted || level <= 0.004 {
        d = d.min(seg(x, y, (5.5, -4.0), (11.0, 4.0), 1.2)).min(seg(x, y, (5.5, 4.0), (11.0, -4.0), 1.2));
    } else {
        let (dx, r) = (x - 3.0, (x - 3.0).hypot(y));
        let wave = |radius: f32| ((r - radius).abs() - 1.2).max(0.6 * r - dx);
        d = d.min(wave(6.5));
        if level > 0.45 {
            d = d.min(wave(11.0));
        }
    }
    d
}

/// Notification bell.
pub fn bell_sdf(x: f32, y: f32) -> f32 {
    let dome = (x.hypot(y + 2.0) - 6.5).max(y - 2.0);
    let skirt = rrect(x, y - 4.5, 9.0, 2.6, 1.6);
    let clapper = x.hypot(y - 9.5) - 2.2;
    let stem = rrect(x, y + 9.0, 1.1, 1.3, 0.6);
    dome.min(skirt).min(clapper).min(stem)
}

/// Arrow pointing up (uploading).
pub fn arrow_up_sdf(x: f32, y: f32) -> f32 {
    let stem = rrect(x, y - 1.5, 1.3, 6.5, 0.8);
    let head = seg(x, y, (-5.5, -1.0), (0.0, -7.0), 1.5).min(seg(x, y, (5.5, -1.0), (0.0, -7.0), 1.5));
    stem.min(head)
}

/// Check mark (done).
pub fn check_sdf(x: f32, y: f32) -> f32 {
    seg(x, y, (-6.0, 0.5), (-2.0, 5.0), 1.6).min(seg(x, y, (-2.0, 5.0), (6.5, -5.0), 1.6))
}

/// A clipboard: a board with text lines cut out and a clip on top (something was copied).
pub fn clipboard_sdf(x: f32, y: f32) -> f32 {
    let board = rrect(x, y - 1.2, 6.8, 8.0, 1.8);
    let gap = rrect(x, y + 6.6, 4.6, 2.9, 1.6); // a ring of background around the clip
    let lines = rrect(x, (y - 1.6 + 3.4).rem_euclid(3.4) - 1.7, 3.6, 0.65, 0.3).max(y - 7.0); // text lines, not on the clip
    let clip = rrect(x, y + 6.6, 3.3, 1.9, 1.0);
    board.max(-gap).max(-lines).min(clip)
}

/// Exclamation mark (failed).
pub fn warn_sdf(x: f32, y: f32) -> f32 {
    rrect(x, y + 2.0, 1.5, 5.5, 0.8).min(x.hypot(y - 6.5) - 1.8)
}

/// A small document (list rows): a page with a dog-eared corner.
pub fn file_sdf(x: f32, y: f32) -> f32 {
    let page = rrect(x, y, 6.5, 8.5, 1.6);
    page.max((x - y - 11.0) * std::f32::consts::FRAC_1_SQRT_2) // the diagonal cuts the top-right corner off
}

/// Eighth note.
pub fn note_sdf(x: f32, y: f32) -> f32 {
    let head = (x + 3.0).hypot(y - 6.0) - 3.9;
    let stem = rrect(x - 0.9, y + 1.5, 1.2, 7.8, 0.6);
    let flag = seg(x, y, (0.9, -8.6), (7.0, -3.6), 1.4);
    head.min(stem).min(flag)
}
