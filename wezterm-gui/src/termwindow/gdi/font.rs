//! GDI font management for the GDI text front_end.
//!
//! Creates the four `HFONT` style variants (regular / bold / italic /
//! bold-italic) from the configured primary font family and size, and derives
//! fixed cell metrics from GDI `GetTextMetrics`. In GDI mode we deliberately use
//! GDI's own metrics for cell sizing so that `ExtTextOutW` glyph placement is
//! guaranteed to fit the grid, at the cost of small differences from the GPU
//! path's freetype/harfbuzz metrics.

use std::ptr::null_mut;
use winapi::shared::windef::{HDC, HFONT};
use winapi::um::wingdi::{
    CreateFontW, DeleteObject, GetTextMetricsW, SelectObject, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    FF_MODERN, FIXED_PITCH, FW_BOLD, FW_NORMAL, OUT_TT_PRECIS, PROOF_QUALITY, TEXTMETRICW,
};
use winapi::um::winuser::{GetDC, ReleaseDC};

/// Convert a `&str` to a NUL-terminated UTF-16 buffer for the Win32 W APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The four style variants used when drawing cells.
#[derive(Copy, Clone, Debug)]
pub enum FontStyleKey {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

/// Owns the GDI `HFONT`s and the derived cell metrics.
pub struct GdiFonts {
    regular: HFONT,
    bold: HFONT,
    italic: HFONT,
    bold_italic: HFONT,

    /// Fixed advance width of a cell, in device pixels.
    pub cell_width: i32,
    /// Full line height of a cell, in device pixels.
    pub cell_height: i32,

    /// Remember how we were built so we can detect when a rebuild is required.
    family: String,
    point_size: f64,
    dpi: usize,
}

unsafe fn create_font(family_w: &[u16], height: i32, weight: i32, italic: u32) -> HFONT {
    CreateFontW(
        height,
        0, // width: 0 = choose based on aspect ratio
        0, // escapement
        0, // orientation
        weight,
        italic,
        0, // underline (drawn manually)
        0, // strikeout (drawn manually)
        DEFAULT_CHARSET,
        OUT_TT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        PROOF_QUALITY,
        FIXED_PITCH | FF_MODERN,
        family_w.as_ptr(),
    )
}

impl GdiFonts {
    /// Build the font set for `family` at `point_size` points and the given
    /// `dpi`. Returns an error only if the base regular font cannot be created.
    pub fn new(family: &str, point_size: f64, dpi: usize) -> anyhow::Result<Self> {
        // GDI logical height: negate to request a cell (character) height that
        // maps the requested point size at this DPI.
        let height = -((point_size * dpi as f64 / 72.0).round() as i32);
        let family_w = wide(family);

        let (regular, bold, italic, bold_italic);
        unsafe {
            regular = create_font(&family_w, height, FW_NORMAL, 0);
            if regular.is_null() {
                anyhow::bail!("CreateFontW failed for family {family:?}");
            }
            bold = create_font(&family_w, height, FW_BOLD, 0);
            italic = create_font(&family_w, height, FW_NORMAL, 1);
            bold_italic = create_font(&family_w, height, FW_BOLD, 1);
        }
        // Fall back to the regular face for any variant GDI could not create, so
        // SelectObject is never handed a null handle.
        let bold = if bold.is_null() { regular } else { bold };
        let italic = if italic.is_null() { regular } else { italic };
        let bold_italic = if bold_italic.is_null() {
            regular
        } else {
            bold_italic
        };

        // Measure cell metrics using a screen DC.
        let (cell_width, cell_height) = unsafe {
            let hdc = GetDC(null_mut());
            let prev = SelectObject(hdc, regular as _);
            let mut tm: TEXTMETRICW = std::mem::zeroed();
            GetTextMetricsW(hdc, &mut tm);
            SelectObject(hdc, prev);
            ReleaseDC(null_mut(), hdc);
            (
                tm.tmAveCharWidth.max(1),
                (tm.tmHeight + tm.tmExternalLeading).max(1),
            )
        };

        log::debug!(
            "GdiFonts: family={family:?} point_size={point_size} dpi={dpi} \
             cell={cell_width}x{cell_height}"
        );

        Ok(Self {
            regular,
            bold,
            italic,
            bold_italic,
            cell_width,
            cell_height,
            family: family.to_string(),
            point_size,
            dpi,
        })
    }

    /// Return the `HFONT` for the given style variant.
    pub fn hfont(&self, style: FontStyleKey) -> HFONT {
        match style {
            FontStyleKey::Regular => self.regular,
            FontStyleKey::Bold => self.bold,
            FontStyleKey::Italic => self.italic,
            FontStyleKey::BoldItalic => self.bold_italic,
        }
    }

    /// Map bold/italic attribute flags to the appropriate style variant.
    pub fn style_for(bold: bool, italic: bool) -> FontStyleKey {
        match (bold, italic) {
            (false, false) => FontStyleKey::Regular,
            (true, false) => FontStyleKey::Bold,
            (false, true) => FontStyleKey::Italic,
            (true, true) => FontStyleKey::BoldItalic,
        }
    }

    /// Select the requested style's `HFONT` into `hdc`, returning the previously
    /// selected object so the caller can restore it.
    pub unsafe fn select(&self, hdc: HDC, style: FontStyleKey) -> *mut winapi::ctypes::c_void {
        SelectObject(hdc, self.hfont(style) as _)
    }

    /// True if this font set was built for different parameters and should be
    /// recreated.
    pub fn needs_rebuild(&self, family: &str, point_size: f64, dpi: usize) -> bool {
        self.family != family || self.point_size != point_size || self.dpi != dpi
    }
}

impl Drop for GdiFonts {
    fn drop(&mut self) {
        unsafe {
            // Delete each distinct handle once; variants may alias `regular`
            // when GDI failed to create a bold/italic face.
            let mut seen: Vec<HFONT> = Vec::new();
            for f in [self.regular, self.bold, self.italic, self.bold_italic] {
                if !f.is_null() && !seen.contains(&f) {
                    DeleteObject(f as _);
                    seen.push(f);
                }
            }
        }
    }
}
