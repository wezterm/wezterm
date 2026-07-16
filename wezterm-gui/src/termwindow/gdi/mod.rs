//! Windows-only GDI text rendering front_end.
//!
//! This backend paints terminal output directly to the window HDC using GDI
//! (`ExtTextOutW`), so that Remote Desktop (RDP) remotes the output as text /
//! glyph operations instead of screen-scraping and video-encoding an opaque
//! GPU swapchain. See `.kilo/plans/1784175286267-gdi-front-end-rdp.md`.
//!
//! Phase 1 (this module) establishes the GPU-free paint path. The current
//! implementation clears the client area to the palette background color; font
//! and cell/run drawing are layered on top in subsequent tasks.

use crate::termwindow::TermWindow;
use ::window::Window;
use mux::pane::PaneId;
use std::collections::HashMap;
use termwiz::surface::SequenceNo;
use wezterm_term::StableRowIndex;
use winapi::shared::windef::{HDC, RECT};
use winapi::um::wingdi::{
    CreateSolidBrush, DeleteObject, SetBkMode, SetTextAlign, OPAQUE, TA_LEFT, TA_TOP,
};
use winapi::um::winuser::FillRect;

pub mod draw;
pub mod font;

use font::GdiFonts;
/// Convert an `SrgbaTuple` to a GDI `COLORREF` (0x00BBGGRR).
pub(crate) fn colorref(color: wezterm_term::color::SrgbaTuple) -> u32 {
    let (r, g, b, _a) = color.as_rgba_u8();
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// The primary font family name from config, falling back to Consolas.
pub(crate) fn primary_family(config: &config::ConfigHandle) -> String {
    config
        .font
        .font
        .first()
        .map(|f| f.family.clone())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "Consolas".to_string())
}

/// GDI cell (width, height) in device pixels for the configured font at `dpi`
/// and effective point size `point_size` (i.e. `config.font_size * font_scale`),
/// or `None` when the GDI front_end is not active or measurement failed. Used to
/// make GDI metrics authoritative for terminal/window layout in GDI mode.
///
/// Measuring requires constructing GDI fonts, which is comparatively expensive
/// and is called on the resize path, so results are memoized by
/// (family, point_size, dpi) in a small single-entry cache.
pub(crate) fn gdi_cell_metrics(
    config: &config::ConfigHandle,
    point_size: f64,
    dpi: usize,
) -> Option<(usize, usize)> {
    if config.front_end != config::FrontEndSelection::Gdi {
        return None;
    }
    let family = primary_family(config);
    let size_bits = point_size.to_bits();

    thread_local! {
        static CACHE: std::cell::RefCell<Option<(String, u64, usize, (usize, usize))>> =
            std::cell::RefCell::new(None);
    }

    CACHE.with(|cache| {
        if let Some((f, s, d, wh)) = cache.borrow().as_ref() {
            if *f == family && *s == size_bits && *d == dpi {
                return Some(*wh);
            }
        }
        let wh = match GdiFonts::new(&family, point_size, dpi) {
            Ok(f) => (f.cell_width.max(1) as usize, f.cell_height.max(1) as usize),
            Err(err) => {
                log::error!("gdi_cell_metrics: {err:#}");
                return None;
            }
        };
        *cache.borrow_mut() = Some((family, size_bits, dpi, wh));
        Some(wh)
    })
}

/// Per-pane damage-tracking state used by the Phase 2 dirty-line optimization.
#[derive(Default, Clone)]
pub(crate) struct PanePaintState {
    /// The sequence number at the last successful paint of this pane.
    pub last_seqno: SequenceNo,
    /// The viewport top (stable row) at the last paint. If this changes the
    /// pane scrolled and must be fully repainted (seqno tracking alone can't
    /// detect lines that merely moved position).
    pub top: StableRowIndex,
    /// Cursor position (col, stable row) at the last paint, so we can repaint
    /// the cell the cursor left behind.
    pub cursor: Option<(usize, StableRowIndex)>,
    /// The selection range + rectangular flag at the last paint, so we can
    /// repaint rows whose selection changed (including horizontal changes on an
    /// already-selected row).
    pub selection: Option<(crate::selection::SelectionRange, bool)>,
}

/// State owned by a `TermWindow` when the GDI front_end is active.
///
/// Holds the GDI font set (created lazily on first paint / rebuilt when the
/// configured font or DPI changes) and per-pane damage-tracking state.
pub struct GdiState {
    fonts: Option<GdiFonts>,
    /// When true, the next paint clears and redraws the whole client area. Set
    /// on first paint, resize, focus change, and config reload.
    pub(crate) needs_full_paint: bool,
    pub(crate) pane_states: HashMap<PaneId, PanePaintState>,
    /// The tab-bar line at the last paint, so we only redraw the strip when it
    /// actually changes.
    pub(crate) last_tab_bar: Option<termwiz::surface::Line>,
    /// Signature of the pane layout at the last paint: (pane_id, left, top,
    /// width, height, is_active). A change means panes were split/closed/moved/
    /// zoomed, which can relocate unchanged lines, so we force a full redraw.
    pub(crate) last_layout: Vec<(PaneId, usize, usize, usize, usize, bool)>,
}

impl GdiState {
    pub fn new() -> Self {
        Self {
            fonts: None,
            needs_full_paint: true,
            pane_states: HashMap::new(),
            last_tab_bar: None,
            last_layout: Vec::new(),
        }
    }

    /// Force a full clear+redraw on the next paint (e.g. after a resize).
    pub(crate) fn invalidate_all(&mut self) {
        self.needs_full_paint = true;
    }

    /// Ensure the font set matches `family`/`point_size`/`dpi`, rebuilding it if
    /// necessary. Returns a reference to the current font set.
    fn ensure_fonts(
        &mut self,
        family: &str,
        point_size: f64,
        dpi: usize,
    ) -> anyhow::Result<&GdiFonts> {
        let rebuild = match &self.fonts {
            Some(fonts) => fonts.needs_rebuild(family, point_size, dpi),
            None => true,
        };
        if rebuild {
            self.fonts = Some(GdiFonts::new(family, point_size, dpi)?);
        }
        Ok(self.fonts.as_ref().unwrap())
    }
}

impl TermWindow {
    /// Force the next GDI paint to be a full clear+redraw. No-op unless the GDI
    /// front_end is active. Call on resize / focus change / config reload.
    pub(crate) fn gdi_invalidate_all(&mut self) {
        if let Some(gdi) = self.gdi.as_mut() {
            gdi.invalidate_all();
        }
    }

    /// Paint the window using GDI. Returns true if a frame was produced.
    ///
    /// Runs on the GUI thread. Obtains the client DC via `Window::with_gdi_dc`,
    /// which handles the `GetDC`/`ReleaseDC`/`ValidateRect` lifecycle for us.
    pub(crate) fn do_paint_gdi(&mut self, window: &Window) -> bool {
        // If the OS asked for a repaint (expose/resize) since the last paint,
        // force a full clear+redraw so we never leave stale/blank regions.
        if window.take_gdi_full_repaint() {
            self.gdi_invalidate_all();
        }

        // Resolve the background color up-front to avoid borrowing `self`
        // across the paint closure.
        let bg = self.palette().background;

        // Font parameters from the current config / window DPI. Use the
        // effective point size (config font size scaled by the runtime font
        // zoom) so GDI drawing matches the layout metrics.
        let family = primary_family(&self.config);
        let point_size = self.config.font_size * self.fonts.get_font_scale();
        let dpi = self.dimensions.dpi;

        // Ensure the GDI font set is built/current and copy out the cell metrics.
        let metrics = match self.gdi.as_mut() {
            Some(gdi) => match gdi.ensure_fonts(&family, point_size, dpi) {
                Ok(fonts) => Some((fonts.cell_width, fonts.cell_height)),
                Err(err) => {
                    log::error!("GDI font creation failed: {:#}", err);
                    None
                }
            },
            None => None,
        };
        let (cw, ch) = match metrics {
            Some(m) => m,
            None => return false,
        };

        // Collect draw commands (borrows &mut self for pane/selection/cursor
        // state), then render them under the client DC.
        let data = self.gdi_collect_frame(cw, ch);
        let gdi = self.gdi.as_ref().unwrap();

        let result = window.with_gdi_dc(|hdc: HDC, rect: RECT| {
            unsafe {
                // Neutral text rendering defaults for cell drawing.
                SetBkMode(hdc, OPAQUE as i32);
                SetTextAlign(hdc, TA_LEFT | TA_TOP);

                // Clear the whole client area to the terminal background only on
                // a full paint; incremental frames leave prior pixels in place
                // and let ETO_OPAQUE overwrite just the changed cells (so RDP
                // only remotes the changed text).
                if data.full_clear {
                    let brush = CreateSolidBrush(colorref(bg));
                    if !brush.is_null() {
                        FillRect(hdc, &rect, brush);
                        DeleteObject(brush as _);
                    }
                }

                gdi.render_frame(hdc, &data);
            }
            Ok(())
        });

        match result {
            Ok(()) => true,
            Err(err) => {
                log::error!("do_paint_gdi failed: {:#}", err);
                // The damage state was already advanced during collection; since
                // drawing failed, force a full redraw next frame so we recover
                // rather than leaving the failed content marked as painted.
                self.gdi_invalidate_all();
                false
            }
        }
    }
}
