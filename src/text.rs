// Text as alpha bitmaps: rasterized once with GDI (Segoe UI, grayscale AA), then sampled by the renderer.

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;

pub const TEXT_R: f32 = 4.0; // raster px per logical unit

pub struct TextBmp {
    pub w: usize,
    pub h: usize,
    a: Vec<u8>,
}

impl TextBmp {
    /// Width / height in logical units.
    pub fn size(&self) -> (f32, f32) {
        (self.w as f32 / TEXT_R, self.h as f32 / TEXT_R)
    }

    /// Coverage 0..1 at raster pixel (u, v).
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        let (w, h) = (self.w as f32, self.h as f32);
        if u < 0.0 || v < 0.0 || u >= w || v >= h {
            return 0.0;
        }
        let x = (u - 0.5).clamp(0.0, w - 1.0);
        let y = (v - 0.5).clamp(0.0, h - 1.0);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let p = |xx: usize, yy: usize| self.a[yy * self.w + xx] as f32;
        let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
        let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
        (top * (1.0 - fy) + bot * fy) / 255.0
    }
}

/// One memory DC and the fonts made so far, per thread: creating them is most of the cost of a small text.
struct Gdi {
    dc: HDC,
    fonts: Vec<((i32, i32, &'static str), HFONT)>,
}

thread_local! {
    static GDI: std::cell::RefCell<Option<Gdi>> = const { std::cell::RefCell::new(None) };
}

impl Gdi {
    unsafe fn font(&mut self, height: i32, weight: i32, face: &'static str) -> HFONT {
        if let Some((_, f)) = self.fonts.iter().find(|(k, _)| *k == (height, weight, face)) {
            return *f;
        }
        let font = CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            PCWSTR(windows::core::HSTRING::from(face).as_ptr()),
        );
        self.fonts.push(((height, weight, face), font));
        font
    }
}

/// `height` is the font height in raster px (negative = character height), `weight` 400 / 700.
pub fn raster(s: &str, height: i32, weight: i32) -> Option<TextBmp> {
    raster_face(s, height, weight, "Segoe UI")
}

/// Where text of this size ends inside the bitmap `raster` makes of it (raster px from its left edge, padding included).
pub fn measure(s: &str, height: i32, weight: i32) -> f32 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    GDI.with(|cell| unsafe {
        let mut cell = cell.borrow_mut();
        let gdi = cell.get_or_insert_with(|| Gdi { dc: CreateCompatibleDC(None), fonts: Vec::new() });
        let font = gdi.font(height, weight, "Segoe UI");
        let old = SelectObject(gdi.dc, font);
        let mut sz = SIZE::default();
        if !wide.is_empty() {
            let _ = GetTextExtentPoint32W(gdi.dc, &wide, &mut sz);
        }
        SelectObject(gdi.dc, old);
        (sz.cx + 4) as f32
    })
}

/// Same in another font face (the README pictures use a lighter one for large type).
pub fn raster_face(s: &str, height: i32, weight: i32, face: &'static str) -> Option<TextBmp> {
    let wide: Vec<u16> = s.encode_utf16().collect();
    if wide.is_empty() {
        return None;
    }
    GDI.with(|cell| unsafe {
        let mut cell = cell.borrow_mut();
        let gdi = cell.get_or_insert_with(|| Gdi { dc: CreateCompatibleDC(None), fonts: Vec::new() });
        let dc = gdi.dc;
        let font = gdi.font(height, weight, face);
        let old_font = SelectObject(dc, font);
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &wide, &mut sz);
        let pad = 4i32;
        let (bw, bh) = ((sz.cx + pad * 2).clamp(8, 9000), sz.cy.clamp(8, 480));

        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bw,
                biHeight: -bh,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut out = None;
        if let Ok(bmp) = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                let old_bmp = SelectObject(dc, bmp);
                SetBkMode(dc, OPAQUE);
                SetBkColor(dc, COLORREF(0));
                SetTextColor(dc, COLORREF(0x00FF_FFFF));
                let _ = TextOutW(dc, pad, 0, &wide);
                let _ = GdiFlush();
                // white on black: the green channel is the coverage
                let px = std::slice::from_raw_parts(bits as *const u8, (bw * bh * 4) as usize);
                let a: Vec<u8> = px.chunks_exact(4).map(|c| c[1]).collect();
                out = Some(TextBmp { w: bw as usize, h: bh as usize, a });
                SelectObject(dc, old_bmp);
            }
            let _ = DeleteObject(bmp);
        }
        SelectObject(dc, old_font);
        out
    })
}
