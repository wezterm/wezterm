//! Cairo2D software rendering backend for WezTerm
//!
//! This module implements a CPU-based software rendering backend using Cairo
//! for environments without GPU acceleration (e.g., remote X11 sessions, SSH,
//! VNC, or systems with problematic GPU drivers).
//!
//! ## Architecture
//!
//! The Cairo2D backend renders to an in-memory surface and transfers pixel data
//! to the window using XPutImage (or equivalent platform API). Key optimizations:
//!
//! - **Line-level dirty tracking**: Only re-renders lines that have changed
//! - **Glyph caching**: Pre-rendered glyphs are cached to avoid redundant rendering
//! - **Partial frame updates**: Only transfers changed screen regions to reduce bandwidth
//!
//! ## Usage
//!
//! Enable via `front_end = "Cairo2D"` in wezterm configuration.

use crate::quad::{Vertex, VERTICES_PER_CELL, V_BOT_LEFT, V_BOT_RIGHT, V_TOP_LEFT, V_TOP_RIGHT};
use crate::renderstate::{Cairo2DCacheData, VertexBuffer};
use crate::termwindow::cairo2d::CairoTexture;
use crate::termwindow::RenderState;
use ::window::bitmaps::Texture2d;
use ::window::WindowOps;
use anyhow::Context;
use cairo::{Format, ImageSurface, Operator};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;
use wezterm_color_types::srgb8_to_linear_u8;

/// Dirty region in cell coordinates for partial screen updates
#[derive(Clone, Debug)]
struct DirtyCellRect {
    /// Starting column (cell index)
    col: usize,
    /// Starting row (cell index)
    row: usize,
    /// Number of cells wide
    width: usize,
    /// Number of cells tall
    height: usize,
}

/// Tracks hash for a single cell in the grid
#[derive(Clone, Debug, Default)]
struct CellBucket {
    /// Hash of the cell's vertex data
    hash: u64,
}

/// Tracks percentage of frame area skipped (not updated) over a time window
struct SkipRatioWindow {
    bytes_sent: u64,
    bytes_total: u64,
    window_start: Instant,
    window_duration: std::time::Duration,
}

impl SkipRatioWindow {
    fn new(duration_secs: u64) -> Self {
        Self {
            bytes_sent: 0,
            bytes_total: 0,
            window_start: Instant::now(),
            window_duration: std::time::Duration::from_secs(duration_secs),
        }
    }

    /// Add bytes and return percentage of frame area skipped for this window
    fn add(&mut self, sent: u64, total: u64) -> f64 {
        // Reset window if expired
        if self.window_start.elapsed() >= self.window_duration {
            self.bytes_sent = 0;
            self.bytes_total = 0;
            self.window_start = Instant::now();
        }

        self.bytes_sent += sent;
        self.bytes_total += total;

        // Return percentage of frame area that was skipped (not sent)
        if self.bytes_total > 0 {
            ((self.bytes_total - self.bytes_sent) as f64 / self.bytes_total as f64) * 100.0
        } else {
            0.0
        }
    }
}


/// Persistent surface and frame state for incremental Cairo2D rendering.
/// This is stored in TermWindow rather than thread_local to properly scope
/// the state to each window instance.
pub struct Cairo2DRenderState {
    surface: Option<ImageSurface>,
    width: i32,
    height: i32,
    last_frame_hash: u64,
    /// Per-cell bucket data for detecting which cells changed (flattened 2D: row * num_cols + col)
    cell_buckets: Vec<CellBucket>,
    /// Number of columns in the cell grid
    num_cols: usize,
    /// Number of rows in the cell grid
    num_rows: usize,
    /// Cell width in pixels (cached from render_metrics)
    cell_width: usize,
    /// Cell height in pixels (cached from render_metrics)
    cell_height: usize,
    /// Tracks percentage of frame area skipped over time windows
    skip_ratio_1s: SkipRatioWindow,
    skip_ratio_10s: SkipRatioWindow,
    skip_ratio_60s: SkipRatioWindow,
    /// Previous cursor cell positions (col, row) for dirty tracking.
    /// Cursor quads may span multiple cells (wide characters), so we track all of them.
    /// When the cursor moves, all previous positions must be invalidated.
    prev_cursor_cells: Vec<(usize, usize)>,
}

impl Cairo2DRenderState {
    pub fn new() -> Self {
        Self {
            surface: None,
            width: 0,
            height: 0,
            last_frame_hash: 0,
            cell_buckets: Vec::new(),
            num_cols: 0,
            num_rows: 0,
            cell_width: 0,
            cell_height: 0,
            skip_ratio_1s: SkipRatioWindow::new(1),
            skip_ratio_10s: SkipRatioWindow::new(10),
            skip_ratio_60s: SkipRatioWindow::new(60),
            prev_cursor_cells: Vec::new(),
        }
    }
}

/// Convert linear RGB to sRGB (gamma correction)
#[inline]
fn linear_to_srgb(linear: f32) -> f64 {
    let srgb = if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    srgb as f64
}

// Cursor detection helpers - cursor glyphs have the high bit set in glyph_id
const CURSOR_FLAG_MASK: u32 = 0x8000_0000;

/// Check if a glyph_id represents a cursor glyph
#[inline]
fn is_cursor_glyph(glyph_id: u32) -> bool {
    glyph_id & CURSOR_FLAG_MASK != 0
}

/// Hash vertex fields for change detection (frame and cell level)
#[inline(always)]
fn hash_vertex(v: &Vertex, cache: &Cairo2DCacheData, hasher: &mut impl Hasher) {
    v.position[0].to_bits().hash(hasher);
    v.position[1].to_bits().hash(hasher);
    v.tex[0].to_bits().hash(hasher);
    v.tex[1].to_bits().hash(hasher);
    v.fg_color[0].to_bits().hash(hasher);
    v.fg_color[1].to_bits().hash(hasher);
    v.fg_color[2].to_bits().hash(hasher);
    v.fg_color[3].to_bits().hash(hasher);
    // Use cache data for bg_color instead of vertex fields
    cache.bg_color[0].to_bits().hash(hasher);
    cache.bg_color[1].to_bits().hash(hasher);
    cache.bg_color[2].to_bits().hash(hasher);
    cache.bg_color[3].to_bits().hash(hasher);
    v.has_color.to_bits().hash(hasher);
}

/// Pre-computed blend tables for fast glyph compositing.
/// Each table maps coverage values (0-255) to final pixel values,
/// avoiding per-pixel arithmetic during rendering.
struct BlendTables {
    r: [u8; 256],
    g: [u8; 256],
    b: [u8; 256],
}

/// Create blend tables for a specific fg/bg color pair.
/// The tables pre-compute: `(bg * (255 - cov_scaled) + fg * cov_scaled) / 255`
/// for all coverage values, where `cov_scaled = (cov * fg_alpha) / 255`.
fn compute_blend_tables(fg: (u8, u8, u8, u8), bg: (u8, u8, u8)) -> BlendTables {
    let mut tables = BlendTables {
        r: [0u8; 256],
        g: [0u8; 256],
        b: [0u8; 256],
    };
    let fg_a = fg.3 as u16;
    let bg_r = bg.0 as u16;
    let bg_g = bg.1 as u16;
    let bg_b = bg.2 as u16;

    for cov in 0..256u16 {
        let cov_scaled = (cov * fg_a) / 255;
        tables.r[cov as usize] =
            ((bg_r * (255 - cov_scaled) + fg.0 as u16 * cov_scaled) / 255).min(255) as u8;
        tables.g[cov as usize] =
            ((bg_g * (255 - cov_scaled) + fg.1 as u16 * cov_scaled) / 255).min(255) as u8;
        tables.b[cov as usize] =
            ((bg_b * (255 - cov_scaled) + fg.2 as u16 * cov_scaled) / 255).min(255) as u8;
    }
    tables
}

/// Fill a rectangular region with a solid background color.
#[inline]
fn fill_cell_background(
    dest_data: &mut [u8],
    dest_stride: usize,
    dest_width: usize,
    dest_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    bg: (u8, u8, u8),
) {
    for row in 0..height {
        let dest_row = y + row;
        if dest_row >= dest_height {
            break;
        }
        for col in 0..width {
            let dest_col = x + col;
            if dest_col >= dest_width {
                break;
            }
            let dest_offset = dest_row * dest_stride + dest_col * 4;
            dest_data[dest_offset + 0] = bg.2; // B
            dest_data[dest_offset + 1] = bg.1; // G
            dest_data[dest_offset + 2] = bg.0; // R
            dest_data[dest_offset + 3] = 255; // A
        }
    }
}

/// Render a non-cacheable glyph directly to the screen buffer.
/// Used for UI elements and selected text where caching is not beneficial.
#[allow(clippy::too_many_arguments)]
fn render_glyph_direct(
    atlas_data: &[u8],
    atlas_stride: usize,
    atlas_width: usize,
    atlas_height: usize,
    dest_data: &mut [u8],
    dest_stride: usize,
    dest_width: usize,
    dest_height: usize,
    job: &GlyphJob,
    blend_tables: &BlendTables,
) {
    for row in 0..job.height {
        let tex_row = job.tex_y + row;
        let dest_row = job.dest_y + row;

        if tex_row >= atlas_height || dest_row >= dest_height {
            continue;
        }

        for col in 0..job.width {
            let tex_col = job.tex_x + col;
            let dest_col = job.dest_x + col;

            if tex_col >= atlas_width || dest_col >= dest_width {
                continue;
            }

            let atlas_offset = tex_row * atlas_stride + tex_col * 4;
            let cov_a = atlas_data[atlas_offset + 3] as u16;

            if cov_a == 0 {
                continue;
            }

            let dest_offset = dest_row * dest_stride + dest_col * 4;

            // Blend foreground with background using blend tables
            let cov_b = atlas_data[atlas_offset + 0] as u16;
            let cov_g = atlas_data[atlas_offset + 1] as u16;
            let cov_r = atlas_data[atlas_offset + 2] as u16;

            let cov_r_lin = srgb8_to_linear_u8((cov_r * 255 / cov_a).min(255) as u8);
            let cov_g_lin = srgb8_to_linear_u8((cov_g * 255 / cov_a).min(255) as u8);
            let cov_b_lin = srgb8_to_linear_u8((cov_b * 255 / cov_a).min(255) as u8);

            dest_data[dest_offset + 0] = blend_tables.b[cov_b_lin as usize];
            dest_data[dest_offset + 1] = blend_tables.g[cov_g_lin as usize];
            dest_data[dest_offset + 2] = blend_tables.r[cov_r_lin as usize];
            dest_data[dest_offset + 3] = 255;
        }
    }
}

/// Render a cursor glyph (filled block or outline shapes).
/// Cursors are handled specially: filled cursors only fill default_bg pixels,
/// while outline cursors blend using the blend tables.
#[allow(clippy::too_many_arguments)]
fn render_cursor_glyph(
    atlas_data: &[u8],
    atlas_stride: usize,
    atlas_width: usize,
    atlas_height: usize,
    dest_data: &mut [u8],
    dest_stride: usize,
    dest_width: usize,
    dest_height: usize,
    job: &GlyphJob,
    blend_tables: &BlendTables,
    default_bg: (u8, u8, u8),
) {
    let cursor_shape = job.glyph_id & !CURSOR_FLAG_MASK;
    // CursorShape enum values:
    //   Default=0: filled block (used when focused)
    //   BlinkingBlock=1, SteadyBlock=2: outline only (used when unfocused)
    //   BlinkingUnderline=3, SteadyUnderline=4, BlinkingBar=5, SteadyBar=6: line cursors
    let is_filled_cursor = cursor_shape == 0;

    if is_filled_cursor {
        // Filled block cursor (Default shape, used when focused):
        // Glyph cells have cursor-modified background (cursor_bg or reversed),
        // so they DON'T match default_bg. Empty cells still have default_bg.
        // Fill ONLY pixels that match default_bg (empty cells).
        for row in 0..job.height {
            let dest_row = job.dest_y + row;
            if dest_row >= dest_height {
                continue;
            }
            for col in 0..job.width {
                let dest_col = job.dest_x + col;
                if dest_col >= dest_width {
                    continue;
                }
                let dest_offset = dest_row * dest_stride + dest_col * 4;
                let cur_b = dest_data[dest_offset + 0];
                let cur_g = dest_data[dest_offset + 1];
                let cur_r = dest_data[dest_offset + 2];
                // Only fill if pixel is default background (empty cell)
                if cur_r == default_bg.0 && cur_g == default_bg.1 && cur_b == default_bg.2 {
                    // Fill with cursor foreground color (cursor_border_color)
                    dest_data[dest_offset + 0] = job.fg_b;
                    dest_data[dest_offset + 1] = job.fg_g;
                    dest_data[dest_offset + 2] = job.fg_r;
                    dest_data[dest_offset + 3] = 255;
                }
            }
        }
    } else {
        // Non-filled cursor (outline, bar, underline):
        // Render cursor sprite using blend tables
        for row in 0..job.height {
            let tex_row = job.tex_y + row;
            let dest_row = job.dest_y + row;

            if tex_row >= atlas_height || dest_row >= dest_height {
                continue;
            }

            for col in 0..job.width {
                let tex_col = job.tex_x + col;
                let dest_col = job.dest_x + col;

                if tex_col >= atlas_width || dest_col >= dest_width {
                    continue;
                }

                let atlas_offset = tex_row * atlas_stride + tex_col * 4;
                let cov_a = atlas_data[atlas_offset + 3] as u16;

                if cov_a == 0 {
                    continue;
                }

                // Use blend tables to draw cursor_border_color (job.fg) over existing
                let cov_b = atlas_data[atlas_offset + 0] as u16;
                let cov_g = atlas_data[atlas_offset + 1] as u16;
                let cov_r = atlas_data[atlas_offset + 2] as u16;

                let cov_r_lin = srgb8_to_linear_u8((cov_r * 255 / cov_a).min(255) as u8);
                let cov_g_lin = srgb8_to_linear_u8((cov_g * 255 / cov_a).min(255) as u8);
                let cov_b_lin = srgb8_to_linear_u8((cov_b * 255 / cov_a).min(255) as u8);

                let dest_offset = dest_row * dest_stride + dest_col * 4;
                dest_data[dest_offset + 0] = blend_tables.b[cov_b_lin as usize];
                dest_data[dest_offset + 1] = blend_tables.g[cov_g_lin as usize];
                dest_data[dest_offset + 2] = blend_tables.r[cov_r_lin as usize];
                dest_data[dest_offset + 3] = 255;
            }
        }
    }
}

/// Job descriptor for batched glyph rendering
struct GlyphJob {
    dest_x: usize,
    dest_y: usize,
    dest_width: usize,
    cell_y: usize,
    cell_height: usize,
    tex_x: usize,
    tex_y: usize,
    width: usize,
    height: usize,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    fg_a: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
    bg_a: u8,
    glyph_id: u32,
    /// If true, skip background fill - the selection rectangle handles it
    is_selected: bool,
}

impl crate::TermWindow {
    /// Main Cairo2D software rendering entry point.
    ///
    /// This function implements a two-pass rendering architecture:
    ///
    /// 1. **Pass 1** (`render_cairo_pass1`): Uses Cairo for vector operations
    ///    - Fills backgrounds and selection rectangles
    ///    - Renders color emoji and images via Cairo's compositing
    ///    - Collects glyph rendering jobs for batch processing
    ///
    /// 2. **Pass 2** (`render_cairo_pass2`): CPU-based glyph compositing
    ///    - Pre-computed blend tables for fast alpha blending
    ///    - Proper gamma correction (linear → sRGB)
    ///
    /// The function also implements:
    /// - Cell-level dirty tracking to skip unchanged content
    /// - Partial frame updates to minimize bandwidth
    /// - Cursor position tracking for proper invalidation
    pub(super) fn call_draw_cairo2d(&mut self) -> anyhow::Result<()> {
        let width = self.dimensions.pixel_width as i32;
        let height = self.dimensions.pixel_height as i32;
        let width_f = width as f64;
        let height_f = height as f64;
        let half_width = width_f / 2.0;
        let half_height = height_f / 2.0;

        // Compute window padding offsets
        let h_context = config::DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.terminal_size.pixel_width as f32,
            pixel_cell: self.render_metrics.cell_size.width as f32,
        };
        let v_context = config::DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.terminal_size.pixel_height as f32,
            pixel_cell: self.render_metrics.cell_size.height as f32,
        };
        let padding_left = self
            .config
            .window_padding
            .left
            .evaluate_as_pixels(h_context) as usize;
        let padding_top = self.config.window_padding.top.evaluate_as_pixels(v_context) as usize;

        // Get the default background color from palette (for cells with transparent bg)
        let palette_bg = self.palette().background;
        let default_bg_r = (palette_bg.0 * 255.0) as u8;
        let default_bg_g = (palette_bg.1 * 255.0) as u8;
        let default_bg_b = (palette_bg.2 * 255.0) as u8;

        let render_state = self.render_state.as_ref().unwrap();

        // Get the glyph atlas texture
        let atlas_texture = render_state.glyph_cache.borrow().atlas.texture();
        let atlas = atlas_texture
            .downcast_ref::<CairoTexture>()
            .context("Expected CairoTexture for Cairo2D backend")?;
        let atlas_width = atlas.width() as f64;
        let atlas_height = atlas.height() as f64;

        // Compute per-cell hashes for detecting which specific cells changed
        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;
        // Use saturating arithmetic to prevent overflow for large windows
        let num_cols = if cell_width > 0 {
            (width as usize)
                .saturating_add(cell_width)
                .saturating_sub(1)
                / cell_width
        } else {
            1
        };
        let num_rows = if cell_height > 0 {
            (height as usize)
                .saturating_add(cell_height)
                .saturating_sub(1)
                / cell_height
        } else {
            1
        };
        let num_cells = num_rows.saturating_mul(num_cols);
        let mut cell_hashers: Vec<DefaultHasher> =
            (0..num_cells).map(|_| DefaultHasher::new()).collect();
        let mut frame_hasher = DefaultHasher::new();

        // Collect current cursor cell positions (cursor quads have glyph_id with high bit set)
        let mut current_cursor_cells: Vec<(usize, usize)> = Vec::new();

        // Hash all vertices for change detection
        for layer in render_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb_cell = &layer.vb.borrow()[idx];
                let (vertex_count, _) = vb_cell.vertex_index_count();
                if vertex_count > 0 {
                    let vertices_ref = vb_cell.current_vb_mut();
                    if let VertexBuffer::Cairo2D(cairo_vb) = &*vertices_ref {
                        let vertices = cairo_vb.vertices.borrow();
                        let cache_data = cairo_vb.cache_data.borrow();
                        let num_quads = vertex_count / 4;
                        for quad_idx in 0..num_quads {
                            let base = quad_idx * 4;
                            if base + 4 > vertices.len() {
                                break;
                            }

                            let quad_verts = &vertices[base..base + 4];
                            let cache = cache_data.get(quad_idx).copied().unwrap_or_default();

                            // Hash for frame-level change detection
                            for v in quad_verts {
                                hash_vertex(v, &cache, &mut frame_hasher);
                            }

                            // Get cell range - for glyphs use cache, for multi-cell quads
                            // (like selection rectangles) calculate from bounding box
                            let (start_col, end_col, start_row, end_row) = if cache.cell_height
                                > 0.0
                            {
                                // set_cell_bounds was called - single cell glyph
                                // Subtract padding to get position relative to cell grid origin
                                // (consistent with multi-cell quad handling and present_partial_frame)
                                let cell_y = (cache.cell_y + half_height as f32) as usize;
                                let cell_y_in_grid = cell_y.saturating_sub(padding_top);
                                let row = if cell_height > 0 {
                                    cell_y_in_grid / cell_height
                                } else {
                                    0
                                };
                                (cache.cell_col, cache.cell_col + (cache.cell_num_cols.max(1) as usize), row, row + 1)
                            } else {
                                // Multi-cell quad (e.g., selection rectangle) - hash into all covered cells
                                // Convert screen coords to pixel coords, then subtract padding to get
                                // position within the cell grid
                                let x1 = (quad_verts[V_TOP_LEFT].position[0] + half_width as f32)
                                    .max(0.0) as usize;
                                let y1 = (quad_verts[V_TOP_LEFT].position[1] + half_height as f32)
                                    .max(0.0) as usize;
                                let x2 = (quad_verts[V_BOT_RIGHT].position[0] + half_width as f32)
                                    .max(0.0) as usize;
                                let y2 = (quad_verts[V_BOT_RIGHT].position[1] + half_height as f32)
                                    .max(0.0) as usize;

                                // Subtract padding to get position relative to cell grid origin
                                let x1_in_grid = x1.saturating_sub(padding_left);
                                let y1_in_grid = y1.saturating_sub(padding_top);
                                let x2_in_grid = x2.saturating_sub(padding_left);
                                let y2_in_grid = y2.saturating_sub(padding_top);

                                let start_col = if cell_width > 0 {
                                    x1_in_grid / cell_width
                                } else {
                                    0
                                };
                                let end_col = if cell_width > 0 {
                                    (x2_in_grid + cell_width - 1) / cell_width
                                } else {
                                    1
                                };
                                let start_row = if cell_height > 0 {
                                    y1_in_grid / cell_height
                                } else {
                                    0
                                };
                                let end_row = if cell_height > 0 {
                                    (y2_in_grid + cell_height - 1) / cell_height
                                } else {
                                    1
                                };
                                (start_col, end_col, start_row, end_row)
                            };

                            // Hash for cell-level change detection - all cells in range
                            for row in start_row..end_row.min(num_rows) {
                                for col in start_col..end_col.min(num_cols) {
                                    let cell_idx = row * num_cols + col;
                                    for v in quad_verts {
                                        hash_vertex(v, &cache, &mut cell_hashers[cell_idx]);
                                    }
                                }
                            }

                            // Track cursor quad positions (cursor quads have high bit set in glyph_id)
                            if is_cursor_glyph(cache.glyph_id) {
                                for row in start_row..end_row.min(num_rows) {
                                    for col in start_col..end_col.min(num_cols) {
                                        current_cursor_cells.push((col, row));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let frame_hash = frame_hasher.finish();
        let current_cell_buckets: Vec<CellBucket> = cell_hashers
            .into_iter()
            .map(|h| CellBucket { hash: h.finish() })
            .collect();

        // Check if we can reuse the previous frame
        let can_reuse = {
            let state = self.cairo2d_state.borrow();
            state.surface.is_some()
                && state.width == width
                && state.height == height
                && state.last_frame_hash == frame_hash
        };

        if can_reuse {
            metrics::histogram!("cairo2d.frame.reused.rate").record(1.);
            {
                let mut state = self.cairo2d_state.borrow_mut();
                if let Some(ref mut surface) = state.surface {
                    surface.flush();
                    if let Ok(data) = surface.data() {
                        let pixels: Vec<u8> = data.to_vec();
                        if let Some(window) = self.window.as_ref() {
                            let _ = window.present_software_frame_region(
                                &pixels,
                                width as u32,
                                height as u32,
                                0,
                                0,
                            );
                        }
                    }
                }
            }
            return Ok(());
        }

        // Detect dirty regions (cell-level)
        let (dirty_rects, force_full_redraw) = self.detect_dirty_cells(
            &current_cell_buckets,
            &current_cursor_cells,
            width,
            height,
            num_cols,
            num_rows,
        );

        // Force full redraw if cursor is involved to avoid cursor ghost artifacts.
        // The partial update system has trouble tracking cursor movements reliably.
        let cursor_involved = !current_cursor_cells.is_empty()
            || !self.cairo2d_state.borrow().prev_cursor_cells.is_empty();
        let do_partial_update = !force_full_redraw && !dirty_rects.is_empty() && !cursor_involved;
        if do_partial_update {
            metrics::histogram!("cairo2d.frame.partial.rate").record(1.);
        } else {
            metrics::histogram!("cairo2d.frame.rendered.rate").record(1.);
        }

        // Get or create the persistent surface
        let surface = {
            let mut state = self.cairo2d_state.borrow_mut();
            if state.surface.is_none() || state.width != width || state.height != height {
                state.surface = ImageSurface::create(Format::ARgb32, width, height).ok();
                state.width = width;
                state.height = height;
                state.cell_buckets.clear();
                state.num_cols = 0;
                state.num_rows = 0;
            }
            // Update cell dimensions if they changed - clear cell buckets since cached data has wrong size
            if state.cell_width != cell_width || state.cell_height != cell_height {
                state.cell_width = cell_width;
                state.cell_height = cell_height;
                state.cell_buckets.clear();
                state.num_cols = 0;
                state.num_rows = 0;
            }
            state.surface.take()
        };
        let mut surface = surface.context("Failed to get Cairo surface")?;

        // Collect glyph jobs for batched processing
        let mut glyph_jobs: Vec<GlyphJob> = Vec::new();

        // PASS 1: Render Cairo operations and collect glyph jobs
        self.render_cairo_pass1(
            &mut surface,
            render_state,
            &atlas,
            atlas_width,
            atlas_height,
            half_width,
            half_height,
            default_bg_r,
            default_bg_g,
            default_bg_b,
            &mut glyph_jobs,
        )?;

        // PASS 2: Batch process glyphs with caching
        if !glyph_jobs.is_empty() {
            self.render_cairo_pass2(
                &mut surface,
                &atlas,
                width as usize,
                height as usize,
                &glyph_jobs,
                default_bg_r,
                default_bg_g,
                default_bg_b,
            )?;
        }

        // Present the frame
        surface.flush();
        let data = surface.data().context("Failed to get surface data")?;
        let pixels: Vec<u8> = data.to_vec();
        drop(data);

        if let Some(window) = self.window.as_ref() {
            let full_frame_bytes = width as usize * height as usize * 4;
            let bytes_sent = if do_partial_update && !dirty_rects.is_empty() {
                self.present_partial_frame(
                    window,
                    &pixels,
                    width,
                    height,
                    cell_width,
                    cell_height,
                    padding_left,
                    padding_top,
                    &dirty_rects,
                )?
            } else {
                metrics::counter!("cairo2d.full.bytes_sent").increment(full_frame_bytes as u64);
                window.present_software_frame_region(&pixels, width as u32, height as u32, 0, 0)?;
                full_frame_bytes
            };

            // Update frame area skip metrics
            {
                let mut state = self.cairo2d_state.borrow_mut();
                let skip_1s = state
                    .skip_ratio_1s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                let skip_10s = state
                    .skip_ratio_10s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                let skip_60s = state
                    .skip_ratio_60s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                metrics::gauge!("cairo2d.frame_area_update_skip_1s_pct").set(skip_1s);
                metrics::gauge!("cairo2d.frame_area_update_skip_10s_pct").set(skip_10s);
                metrics::gauge!("cairo2d.frame_area_update_skip_60s_pct").set(skip_60s);
            }
        }

        // Store surface and cell buckets for next frame
        {
            let mut state = self.cairo2d_state.borrow_mut();
            state.surface = Some(surface);
            state.last_frame_hash = frame_hash;
            state.cell_buckets = current_cell_buckets;
            state.num_cols = num_cols;
            state.num_rows = num_rows;
            // Save current cursor positions for next frame's dirty tracking
            state.prev_cursor_cells = current_cursor_cells;
        }

        Ok(())
    }

    /// Detect which screen cells have changed since the last frame
    /// Returns dirty rectangles (coalesced row-wise runs of dirty cells)
    fn detect_dirty_cells(
        &self,
        current_cell_buckets: &[CellBucket],
        current_cursor_cells: &[(usize, usize)],
        width: i32,
        height: i32,
        num_cols: usize,
        num_rows: usize,
    ) -> (Vec<DirtyCellRect>, bool) {
        let state = self.cairo2d_state.borrow();
        let prev_buckets = &state.cell_buckets;

        // Force full redraw if dimensions changed or no previous state
        if state.width != width
            || state.height != height
            || state.num_cols != num_cols
            || state.num_rows != num_rows
            || prev_buckets.is_empty()
        {
            return (Vec::new(), true);
        }

        // Get previous cursor positions for dirty tracking
        let prev_cursor_cells = &state.prev_cursor_cells;

        // Build dirty cell bitmap
        let num_cells = num_rows * num_cols;
        let mut dirty_count = 0usize;
        let mut dirty_bitmap: Vec<bool> = vec![false; num_cells];

        for (idx, bucket) in current_cell_buckets.iter().enumerate() {
            let prev_hash = prev_buckets.get(idx).map(|b| b.hash).unwrap_or(0);
            if bucket.hash != prev_hash {
                dirty_bitmap[idx] = true;
                dirty_count += 1;
            }
        }

        // Force all previous cursor cells dirty.
        // This ensures the old cursor position is repainted even if the
        // underlying text hasn't changed. We mark ALL previous cursor cells
        // dirty regardless of whether the cursor moved, because the cursor
        // might blink or change shape, and the hash-based detection might
        // miss some cursor changes due to timing.
        for &(col, row) in prev_cursor_cells.iter() {
            if col < num_cols && row < num_rows {
                let cell_idx = row * num_cols + col;
                if !dirty_bitmap[cell_idx] {
                    dirty_bitmap[cell_idx] = true;
                    dirty_count += 1;
                    log::trace!(
                        "cairo2d: forcing previous cursor cell ({}, {}) dirty",
                        col,
                        row
                    );
                }
            }
        }

        // Also force current cursor cells dirty to ensure cursor updates are visible
        for &(col, row) in current_cursor_cells.iter() {
            if col < num_cols && row < num_rows {
                let cell_idx = row * num_cols + col;
                if !dirty_bitmap[cell_idx] {
                    dirty_bitmap[cell_idx] = true;
                    dirty_count += 1;
                }
            }
        }

        // Fall back to full redraw if too many cells are dirty
        let dirty_ratio = dirty_count as f32 / num_cells.max(1) as f32;
        if dirty_ratio > 0.5 {
            metrics::counter!("cairo2d.partial.full_redraw_threshold").increment(1);
            return (Vec::new(), true);
        }

        // Coalesce dirty cells into rectangles (row-wise runs of dirty cells)
        let mut dirty_rects: Vec<DirtyCellRect> = Vec::new();

        for row in 0..num_rows {
            let mut run_start: Option<usize> = None;

            for col in 0..num_cols {
                let cell_idx = row * num_cols + col;
                let is_dirty = dirty_bitmap[cell_idx];

                match (run_start, is_dirty) {
                    (None, true) => {
                        // Start a new run of dirty cells
                        run_start = Some(col);
                    }
                    (Some(start), false) => {
                        // End the current run
                        dirty_rects.push(DirtyCellRect {
                            col: start,
                            row,
                            width: col - start,
                            height: 1,
                        });
                        run_start = None;
                    }
                    _ => {}
                }
            }

            // Handle run that extends to end of row
            if let Some(start) = run_start {
                dirty_rects.push(DirtyCellRect {
                    col: start,
                    row,
                    width: num_cols - start,
                    height: 1,
                });
            }
        }

        metrics::counter!("cairo2d.partial.dirty_cells_total").increment(dirty_count as u64);
        metrics::counter!("cairo2d.partial.dirty_rects_total").increment(dirty_rects.len() as u64);

        (dirty_rects, false)
    }

    /// Pass 1: Render solid colors and images using Cairo, collect glyph jobs.
    ///
    /// This pass handles:
    /// - Background fills (solid color rectangles)
    /// - Selection highlighting rectangles
    /// - Color emoji and images (via Cairo's image compositing)
    /// - Underline/strikethrough decorations
    ///
    /// Monochrome glyphs are deferred to Pass 2 for optimized CPU rendering
    /// with pre-computed blend tables.
    fn render_cairo_pass1(
        &self,
        surface: &mut ImageSurface,
        render_state: &RenderState,
        atlas: &CairoTexture,
        atlas_width: f64,
        atlas_height: f64,
        half_width: f64,
        half_height: f64,
        default_bg_r: u8,
        default_bg_g: u8,
        default_bg_b: u8,
        glyph_jobs: &mut Vec<GlyphJob>,
    ) -> anyhow::Result<()> {
        let ctx = cairo::Context::new(surface).context("Failed to create Cairo context")?;

        // Clear to default background
        ctx.set_source_rgba(
            default_bg_r as f64 / 255.0,
            default_bg_g as f64 / 255.0,
            default_bg_b as f64 / 255.0,
            1.0,
        );
        ctx.set_operator(Operator::Source);
        ctx.paint()?;
        ctx.set_operator(Operator::Over);

        for layer in render_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb_cell = &layer.vb.borrow()[idx];
                let (vertex_count, _index_count) = vb_cell.vertex_index_count();

                if vertex_count == 0 {
                    vb_cell.next_index();
                    continue;
                }

                let vertices_ref = vb_cell.current_vb_mut();
                let (vertices, cache_data) = match &*vertices_ref {
                    VertexBuffer::Cairo2D(cairo_vb) => {
                        (cairo_vb.vertices.borrow(), cairo_vb.cache_data.borrow())
                    }
                    _ => {
                        vb_cell.next_index();
                        continue;
                    }
                };

                let num_quads = vertex_count / VERTICES_PER_CELL;
                for quad_idx in 0..num_quads {
                    let base = quad_idx * VERTICES_PER_CELL;
                    if base + VERTICES_PER_CELL > vertices.len() {
                        break;
                    }

                    let tl = &vertices[base + V_TOP_LEFT];
                    let tr = &vertices[base + V_TOP_RIGHT];
                    let bl = &vertices[base + V_BOT_LEFT];
                    let br = &vertices[base + V_BOT_RIGHT];
                    let cache = cache_data.get(quad_idx).copied().unwrap_or_default();

                    let dest_x = tl.position[0] as f64 + half_width;
                    let dest_y = tl.position[1] as f64 + half_height;
                    let dest_width = (tr.position[0] - tl.position[0]) as f64;
                    let dest_height = (bl.position[1] - tl.position[1]) as f64;

                    if dest_width <= 0.0 || dest_height <= 0.0 {
                        continue;
                    }

                    let has_color = tl.has_color;

                    if has_color == 3.0 {
                        // Solid color (e.g., selection rectangle)
                        // Draw semi-transparently - Cairo will blend in sRGB space.
                        // For consistency with glyph backgrounds, we'll handle
                        // the color space mismatch in the glyph rendering path.
                        let [r, g, b, a] = tl.fg_color;
                        ctx.set_source_rgba(
                            linear_to_srgb(r),
                            linear_to_srgb(g),
                            linear_to_srgb(b),
                            a as f64,
                        );
                        ctx.rectangle(dest_x, dest_y, dest_width, dest_height);
                        ctx.fill()?;
                    } else {
                        let tex_x1 = tl.tex[0] as f64 * atlas_width;
                        let tex_y1 = tl.tex[1] as f64 * atlas_height;
                        let tex_x2 = br.tex[0] as f64 * atlas_width;
                        let tex_y2 = br.tex[1] as f64 * atlas_height;
                        let tex_width = tex_x2 - tex_x1;
                        let tex_height = tex_y2 - tex_y1;

                        if tex_width <= 0.0 || tex_height <= 0.0 {
                            continue;
                        }

                        if has_color == 0.0 || has_color == 4.0 {
                            // Glyph - collect for batched processing
                            // Use vertex fields for fg_color, cache_data for bg/cell info
                            let [fg_r, fg_g, fg_b, fg_a] = tl.fg_color;
                            let [bg_r, bg_g, bg_b, bg_a] = cache.bg_color;
                            let cell_y = (cache.cell_y as f64 + half_height) as usize;
                            let cell_height = cache.cell_height as usize;

                            glyph_jobs.push(GlyphJob {
                                dest_x: dest_x as usize,
                                dest_y: dest_y as usize,
                                dest_width: dest_width as usize,
                                cell_y,
                                cell_height,
                                tex_x: tex_x1 as usize,
                                tex_y: tex_y1 as usize,
                                width: tex_width as usize,
                                height: tex_height as usize,
                                fg_r: (linear_to_srgb(fg_r) * 255.0) as u8,
                                fg_g: (linear_to_srgb(fg_g) * 255.0) as u8,
                                fg_b: (linear_to_srgb(fg_b) * 255.0) as u8,
                                fg_a: (fg_a * 255.0) as u8,
                                bg_r: (linear_to_srgb(bg_r) * 255.0) as u8,
                                bg_g: (linear_to_srgb(bg_g) * 255.0) as u8,
                                bg_b: (linear_to_srgb(bg_b) * 255.0) as u8,
                                bg_a: (bg_a * 255.0) as u8,
                                glyph_id: cache.glyph_id,
                                is_selected: cache.is_selected,
                            });
                        } else {
                            // Color emoji or background image
                            let atlas_surface = atlas.surface();
                            ctx.save()?;
                            ctx.translate(dest_x, dest_y);
                            let scale_x = dest_width / tex_width;
                            let scale_y = dest_height / tex_height;
                            if (scale_x - 1.0).abs() > 0.001 || (scale_y - 1.0).abs() > 0.001 {
                                ctx.scale(scale_x, scale_y);
                            }
                            ctx.set_source_surface(&*atlas_surface, -tex_x1, -tex_y1)?;
                            ctx.rectangle(0.0, 0.0, tex_width, tex_height);
                            ctx.clip();
                            ctx.paint()?;
                            ctx.restore()?;
                        }
                    }
                }

                drop(vertices);
                drop(cache_data);
                drop(vertices_ref);
                vb_cell.next_index();
            }
        }

        Ok(())
    }

    /// Pass 2: Batch process glyphs with optimized CPU compositing.
    ///
    /// This pass renders monochrome glyphs using:
    /// - **Pre-computed blend tables**: 256-entry lookup tables for each fg/bg color pair
    ///   to avoid per-pixel arithmetic
    /// - **Gamma-correct blending**: Coverage values are converted to linear space for
    ///   proper alpha compositing, then results are converted back to sRGB
    ///
    /// Glyphs are processed in two batches:
    /// 1. Regular glyphs (text content)
    /// 2. Cursor glyphs (rendered last to ensure they overlay character overhangs)
    fn render_cairo_pass2(
        &self,
        surface: &mut ImageSurface,
        atlas: &CairoTexture,
        dest_width: usize,
        dest_height: usize,
        glyph_jobs: &[GlyphJob],
        default_bg_r: u8,
        default_bg_g: u8,
        default_bg_b: u8,
    ) -> anyhow::Result<()> {
        let mut atlas_surface_mut = atlas.surface_mut();
        let atlas_stride = atlas_surface_mut.stride() as usize;
        let atlas_width = atlas_surface_mut.width() as usize;
        let atlas_height = atlas_surface_mut.height() as usize;
        let dest_stride = surface.stride() as usize;

        let atlas_data = atlas_surface_mut
            .data()
            .context("Failed to get atlas data")?;
        let mut dest_data = surface.data().context("Failed to get destination data")?;

        let default_bg = (default_bg_r, default_bg_g, default_bg_b);

        // Render cursor jobs LAST to ensure they overlay everything including overhangs.
        let (cursor_jobs, regular_jobs): (Vec<_>, Vec<_>) = glyph_jobs
            .iter()
            .partition(|job| is_cursor_glyph(job.glyph_id));

        // Process regular jobs first, then cursor jobs
        for job in regular_jobs.into_iter().chain(cursor_jobs.into_iter()) {
            // Determine actual background color (fall back to default if bg_a is 0)
            let actual_bg = if job.bg_a > 0 {
                (job.bg_r, job.bg_g, job.bg_b)
            } else {
                log::trace!(
                    "cairo2d: bg_a is 0 for glyph at ({}, {}), using default background",
                    job.dest_x,
                    job.dest_y
                );
                default_bg
            };

            // Compute effective cell dimensions for background fills
            let (effective_cell_height, effective_cell_y) = if job.cell_height > 0 {
                (job.cell_height, job.cell_y)
            } else if job.bg_a > 0 {
                // Fallback for terminal content: use glyph dimensions
                log::trace!(
                    "cairo2d: cell_height is 0 for glyph at ({}, {}), using glyph height {} instead",
                    job.dest_x,
                    job.dest_y,
                    job.height
                );
                (job.height, job.dest_y)
            } else {
                // UI element - no cell-based background fill needed
                (0, 0)
            };

            // Pre-compute blend tables for this fg/bg combination
            let blend_tables =
                compute_blend_tables((job.fg_r, job.fg_g, job.fg_b, job.fg_a), actual_bg);

            if is_cursor_glyph(job.glyph_id) {
                // CURSOR PATH: Special handling for cursor glyphs
                render_cursor_glyph(
                    &atlas_data,
                    atlas_stride,
                    atlas_width,
                    atlas_height,
                    &mut dest_data,
                    dest_stride,
                    dest_width,
                    dest_height,
                    job,
                    &blend_tables,
                    default_bg,
                );
            } else {
                // DIRECT PATH: Render all non-cursor glyphs directly to screen
                // This avoids the double alpha blending issue that occurred with the
                // cached path (render_glyph_cached stored fg with coverage alpha,
                // then blitting alpha-blended again causing darkened glyphs).

                // Fill background first (skip for selected cells - selection rectangle handles it)
                if !job.is_selected && effective_cell_height > 0 {
                    fill_cell_background(
                        &mut dest_data,
                        dest_stride,
                        dest_width,
                        dest_height,
                        job.dest_x,
                        effective_cell_y,
                        job.dest_width,
                        effective_cell_height,
                        actual_bg,
                    );
                }

                // Render glyph directly to screen
                render_glyph_direct(
                    &atlas_data,
                    atlas_stride,
                    atlas_width,
                    atlas_height,
                    &mut dest_data,
                    dest_stride,
                    dest_width,
                    dest_height,
                    job,
                    &blend_tables,
                );
            }
        }

        drop(dest_data);
        drop(atlas_data);
        surface.mark_dirty();

        Ok(())
    }

    /// Present only the dirty rectangular regions of the frame
    fn present_partial_frame(
        &self,
        window: &::window::Window,
        pixels: &[u8],
        width: i32,
        height: i32,
        cell_width: usize,
        cell_height: usize,
        padding_left: usize,
        padding_top: usize,
        dirty_rects: &[DirtyCellRect],
    ) -> anyhow::Result<usize> {
        let src_stride = width as usize * 4;
        let mut bytes_sent = 0usize;
        let width_usize = width as usize;
        let height_usize = height as usize;
        let full_frame_bytes = width_usize * height_usize * 4;

        log::debug!(
            "cairo2d partial update: {} dirty cell rects",
            dirty_rects.len()
        );

        for rect in dirty_rects {
            // Convert cell coordinates to pixel coordinates (add padding offset)
            let pixel_x = padding_left + rect.col * cell_width;
            let pixel_y = padding_top + rect.row * cell_height;
            let pixel_width = rect.width * cell_width;
            let pixel_height = rect.height * cell_height;

            // Validate bounds
            if pixel_x >= width_usize || pixel_y >= height_usize {
                continue;
            }

            let rect_width = pixel_width.min(width_usize.saturating_sub(pixel_x));
            let rect_height = pixel_height.min(height_usize.saturating_sub(pixel_y));

            if rect_width == 0 || rect_height == 0 {
                continue;
            }

            // Extract rectangular region into contiguous buffer
            let rect_stride = rect_width * 4;
            let rect_bytes = rect_stride * rect_height;
            let mut rect_pixels = vec![0u8; rect_bytes];

            for row in 0..rect_height {
                let src_y = pixel_y + row;
                let src_offset = src_y * src_stride + pixel_x * 4;
                let dst_offset = row * rect_stride;

                if src_offset + rect_stride <= pixels.len() {
                    rect_pixels[dst_offset..dst_offset + rect_stride]
                        .copy_from_slice(&pixels[src_offset..src_offset + rect_stride]);
                }
            }

            window.present_software_frame_region(
                &rect_pixels,
                rect_width as u32,
                rect_height as u32,
                pixel_x as i16,
                pixel_y as i16,
            )?;
            bytes_sent += rect_bytes;
        }

        metrics::counter!("cairo2d.partial.bytes_sent").increment(bytes_sent as u64);
        metrics::counter!("cairo2d.partial.bytes_saved")
            .increment((full_frame_bytes - bytes_sent) as u64);

        log::trace!(
            "cairo2d partial: {} cell rects, sent {} / {} bytes",
            dirty_rects.len(),
            bytes_sent,
            full_frame_bytes
        );

        Ok(bytes_sent)
    }
}
