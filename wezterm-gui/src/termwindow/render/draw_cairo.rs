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
use config::ConfigHandle;
use lfucache::LfuCache;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;
use wezterm_color_types::srgb8_to_linear_u8;

/// Represents a dirty row region for partial screen updates
#[derive(Clone, Debug)]
struct DirtyRow {
    /// Top pixel of the dirty row region
    pixel_y: usize,
    /// Height in pixels of the dirty region
    pixel_height: usize,
}

/// Tracks hash and actual pixel bounds for a line bucket
#[derive(Clone, Debug, Default)]
struct LineBucket {
    /// Hash of the line's vertex data
    hash: u64,
    /// Actual minimum pixel Y of content in this bucket
    min_y: usize,
    /// Actual maximum pixel Y of content in this bucket
    max_y: usize,
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

/// Cache key for pre-rendered glyphs
#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct GlyphCacheKey {
    glyph_id: u32,
    fg_rgba: u32,
    bg_rgba: u32,
    cell_width: u16,
    cell_height: u16,
}

/// Cached pre-rendered glyph pixels
struct CachedGlyph {
    /// Pre-rendered BGRA pixels (full cell including background)
    pixels: Vec<u8>,
}

/// Persistent surface and frame state for incremental Cairo2D rendering.
/// This is stored in TermWindow rather than thread_local to properly scope
/// the state to each window instance.
pub struct Cairo2DRenderState {
    surface: Option<ImageSurface>,
    width: i32,
    height: i32,
    last_frame_hash: u64,
    /// Per-line bucket data for detecting which lines changed
    line_buckets: Vec<LineBucket>,
    /// Tracks percentage of frame area skipped over time windows
    skip_ratio_1s: SkipRatioWindow,
    skip_ratio_10s: SkipRatioWindow,
    skip_ratio_60s: SkipRatioWindow,
    /// Glyph cache for pre-rendered glyphs using LfuCache for proper eviction
    glyph_cache: LfuCache<GlyphCacheKey, CachedGlyph>,
}

impl Cairo2DRenderState {
    pub fn new(config: &ConfigHandle) -> Self {
        Self {
            surface: None,
            width: 0,
            height: 0,
            last_frame_hash: 0,
            line_buckets: Vec::new(),
            skip_ratio_1s: SkipRatioWindow::new(1),
            skip_ratio_10s: SkipRatioWindow::new(10),
            skip_ratio_60s: SkipRatioWindow::new(60),
            glyph_cache: LfuCache::new(
                "cairo2d.glyph_cache.hit.rate",
                "cairo2d.glyph_cache.miss.rate",
                |config| config.cairo2d_glyph_cache_size,
                config,
            ),
        }
    }

    /// Update cache sizes when configuration changes
    pub fn update_config(&mut self, config: &ConfigHandle) {
        self.glyph_cache.update_config(config);
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

/// Hash vertex fields for frame-level change detection
#[inline(always)]
fn hash_vertex_for_frame(v: &Vertex, cache: &Cairo2DCacheData, hasher: &mut impl Hasher) {
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

/// Hash vertex fields for line-level change detection
#[inline(always)]
fn hash_vertex_for_line(v: &Vertex, cache: &Cairo2DCacheData, hasher: &mut impl Hasher) {
    hash_vertex_for_frame(v, cache, hasher);
}

/// Pack RGBA components into a single u32
#[inline]
fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
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
}

impl crate::TermWindow {
    /// Main Cairo2D rendering entry point
    pub(super) fn call_draw_cairo2d(&mut self) -> anyhow::Result<()> {
        let width = self.dimensions.pixel_width as i32;
        let height = self.dimensions.pixel_height as i32;
        let width_f = width as f64;
        let height_f = height as f64;
        let half_width = width_f / 2.0;
        let half_height = height_f / 2.0;

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

        // Compute per-line hashes for detecting which specific lines changed
        let cell_height = self.render_metrics.cell_size.height as usize;
        let num_lines = if cell_height > 0 {
            (height as usize + cell_height - 1) / cell_height
        } else {
            1
        };
        let mut line_hashers: Vec<DefaultHasher> =
            (0..num_lines).map(|_| DefaultHasher::new()).collect();
        let mut line_min_y: Vec<usize> = vec![usize::MAX; num_lines];
        let mut line_max_y: Vec<usize> = vec![0; num_lines];
        let mut frame_hasher = DefaultHasher::new();

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
                            let mut quad_min_y = f32::MAX;
                            let mut quad_max_y = f32::MIN;
                            for v in quad_verts {
                                let vy = v.position[1] + half_height as f32;
                                quad_min_y = quad_min_y.min(vy);
                                quad_max_y = quad_max_y.max(vy);
                            }

                            // Use cache data for cell_y and cell_height
                            let bucket_y = if cache.cell_height > 0.0 {
                                cache.cell_y + half_height as f32
                            } else {
                                quad_min_y
                            };

                            if bucket_y < 0.0 || bucket_y >= height_f as f32 {
                                for v in quad_verts {
                                    hash_vertex_for_frame(v, &cache, &mut frame_hasher);
                                }
                                continue;
                            }

                            let line_idx = if cell_height > 0 {
                                (bucket_y as usize) / cell_height
                            } else {
                                0
                            };

                            if line_idx < num_lines {
                                let qmin = quad_min_y.max(0.0) as usize;
                                let qmax =
                                    (quad_max_y.min(height_f as f32) as usize).min(height as usize);
                                line_min_y[line_idx] = line_min_y[line_idx].min(qmin);
                                line_max_y[line_idx] = line_max_y[line_idx].max(qmax);
                            }

                            for v in quad_verts {
                                hash_vertex_for_frame(v, &cache, &mut frame_hasher);
                                if line_idx < num_lines {
                                    hash_vertex_for_line(v, &cache, &mut line_hashers[line_idx]);
                                }
                            }
                        }
                    }
                }
            }
        }

        let frame_hash = frame_hasher.finish();
        let current_line_buckets: Vec<LineBucket> = line_hashers
            .into_iter()
            .enumerate()
            .map(|(idx, h)| LineBucket {
                hash: h.finish(),
                min_y: line_min_y[idx],
                max_y: line_max_y[idx],
            })
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

        // Detect dirty regions
        let (dirty_rows, force_full_redraw) =
            self.detect_dirty_regions(&current_line_buckets, width, height, cell_height, num_lines);

        let do_partial_update = !force_full_redraw && !dirty_rows.is_empty();
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
                state.line_buckets.clear();
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
            )?;
        }

        // Present the frame
        surface.flush();
        let data = surface.data().context("Failed to get surface data")?;
        let pixels: Vec<u8> = data.to_vec();
        drop(data);

        if let Some(window) = self.window.as_ref() {
            let full_frame_bytes = width as usize * height as usize * 4;
            let bytes_sent = if do_partial_update && !dirty_rows.is_empty() {
                self.present_partial_frame(window, &pixels, width, height, &dirty_rows)?
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

        // Store surface and line buckets for next frame
        {
            let mut state = self.cairo2d_state.borrow_mut();
            state.surface = Some(surface);
            state.last_frame_hash = frame_hash;
            state.line_buckets = current_line_buckets;
        }

        Ok(())
    }

    /// Detect which screen regions have changed since the last frame
    fn detect_dirty_regions(
        &self,
        current_line_buckets: &[LineBucket],
        width: i32,
        height: i32,
        cell_height: usize,
        num_lines: usize,
    ) -> (Vec<DirtyRow>, bool) {
        let state = self.cairo2d_state.borrow();
        let prev_buckets = &state.line_buckets;

        if state.width != width || state.height != height || prev_buckets.is_empty() {
            return (Vec::new(), true);
        }

        struct DirtyLine {
            idx: usize,
            min_y: usize,
            max_y: usize,
        }

        let mut dirty_lines: Vec<DirtyLine> = Vec::new();
        for (idx, bucket) in current_line_buckets.iter().enumerate() {
            let prev_hash = prev_buckets.get(idx).map(|b| b.hash).unwrap_or(0);
            if bucket.hash != prev_hash {
                let min_y = if bucket.min_y == usize::MAX {
                    idx * cell_height
                } else {
                    bucket.min_y
                };
                let max_y = if bucket.max_y == 0 {
                    ((idx + 1) * cell_height).min(height as usize)
                } else {
                    bucket.max_y
                };
                dirty_lines.push(DirtyLine { idx, min_y, max_y });
            }
        }

        let dirty_ratio = dirty_lines.len() as f32 / num_lines.max(1) as f32;
        if dirty_ratio > 0.5 {
            metrics::counter!("cairo2d.partial.full_redraw_threshold").increment(1);
            return (Vec::new(), true);
        }

        // Coalesce adjacent dirty lines
        let mut dirty_rows: Vec<DirtyRow> = Vec::new();
        let mut region_start_idx: Option<usize> = None;
        let mut region_end_idx: usize = 0;
        let mut region_min_y: usize = 0;
        let mut region_max_y: usize = 0;

        for dirty in &dirty_lines {
            match region_start_idx {
                None => {
                    region_start_idx = Some(dirty.idx);
                    region_end_idx = dirty.idx;
                    region_min_y = dirty.min_y;
                    region_max_y = dirty.max_y;
                }
                Some(_) => {
                    if dirty.idx <= region_end_idx + 3 {
                        region_end_idx = dirty.idx;
                        region_min_y = region_min_y.min(dirty.min_y);
                        region_max_y = region_max_y.max(dirty.max_y);
                    } else {
                        let pixel_height = region_max_y.saturating_sub(region_min_y);
                        if pixel_height > 0 && region_min_y < height as usize {
                            dirty_rows.push(DirtyRow {
                                pixel_y: region_min_y,
                                pixel_height: pixel_height
                                    .min((height as usize).saturating_sub(region_min_y)),
                            });
                        }
                        region_start_idx = Some(dirty.idx);
                        region_end_idx = dirty.idx;
                        region_min_y = dirty.min_y;
                        region_max_y = dirty.max_y;
                    }
                }
            }
        }

        if region_start_idx.is_some() {
            let pixel_height = region_max_y.saturating_sub(region_min_y);
            if pixel_height > 0 && region_min_y < height as usize {
                dirty_rows.push(DirtyRow {
                    pixel_y: region_min_y,
                    pixel_height: pixel_height.min((height as usize).saturating_sub(region_min_y)),
                });
            }
        }

        metrics::counter!("cairo2d.partial.dirty_lines_total").increment(dirty_lines.len() as u64);
        metrics::counter!("cairo2d.partial.dirty_regions_total").increment(dirty_rows.len() as u64);

        (dirty_rows, false)
    }

    /// PASS 1: Render solid colors and images, collect glyph jobs
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
                        // Solid color
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

    /// PASS 2: Batch process glyphs with caching
    fn render_cairo_pass2(
        &self,
        surface: &mut ImageSurface,
        atlas: &CairoTexture,
        dest_width: usize,
        dest_height: usize,
        glyph_jobs: &[GlyphJob],
    ) -> anyhow::Result<()> {
        let mut atlas_surface_mut = atlas.surface_mut();
        let atlas_stride = atlas_surface_mut.stride() as usize;
        let atlas_width_px = atlas_surface_mut.width() as usize;
        let atlas_height_px = atlas_surface_mut.height() as usize;
        let dest_stride = surface.stride() as usize;

        let atlas_data = atlas_surface_mut.data().expect("Failed to get atlas data");
        let mut dest_data = surface.data().expect("Failed to get destination data");

        for job in glyph_jobs {
            let actual_bg_r = job.bg_r;
            let actual_bg_g = job.bg_g;
            let actual_bg_b = job.bg_b;

            // Fill full cell with background
            for row in 0..job.cell_height {
                let dest_row = job.cell_y + row;
                if dest_row >= dest_height {
                    break;
                }
                for col in 0..job.dest_width {
                    let dest_col = job.dest_x + col;
                    if dest_col >= dest_width {
                        break;
                    }
                    let dest_offset = dest_row * dest_stride + dest_col * 4;
                    dest_data[dest_offset + 0] = actual_bg_b;
                    dest_data[dest_offset + 1] = actual_bg_g;
                    dest_data[dest_offset + 2] = actual_bg_r;
                    dest_data[dest_offset + 3] = 255;
                }
            }

            let cache_key = GlyphCacheKey {
                glyph_id: job.glyph_id,
                fg_rgba: pack_rgba(job.fg_r, job.fg_g, job.fg_b, job.fg_a),
                bg_rgba: pack_rgba(actual_bg_r, actual_bg_g, actual_bg_b, job.bg_a),
                cell_width: job.dest_width as u16,
                cell_height: job.cell_height as u16,
            };

            // Try cache lookup (get() requires mut because LfuCache updates frequency)
            let cache_hit = {
                let mut state = self.cairo2d_state.borrow_mut();
                if let Some(cached) = state.glyph_cache.get(&cache_key) {
                    let cell_width = job.dest_width;
                    let cell_height = job.cell_height;
                    for row in 0..cell_height {
                        let dest_row = job.cell_y + row;
                        if dest_row >= dest_height {
                            break;
                        }
                        let copy_width = cell_width.min(dest_width.saturating_sub(job.dest_x));
                        if copy_width == 0 {
                            continue;
                        }
                        let src_start = row * cell_width * 4;
                        let dest_start = dest_row * dest_stride + job.dest_x * 4;
                        let copy_bytes = copy_width * 4;
                        dest_data[dest_start..dest_start + copy_bytes]
                            .copy_from_slice(&cached.pixels[src_start..src_start + copy_bytes]);
                    }
                    metrics::histogram!("cairo2d.cache.hit.rate").record(1.);
                    true
                } else {
                    false
                }
            };

            if cache_hit {
                continue;
            }

            metrics::histogram!("cairo2d.cache.miss.rate").record(1.);

            // Render glyph
            let fg_a = job.fg_a as u16;
            let bg_r = actual_bg_r as u16;
            let bg_g = actual_bg_g as u16;
            let bg_b = actual_bg_b as u16;

            // Pre-compute blend tables
            let mut blend_table_r = [0u8; 256];
            let mut blend_table_g = [0u8; 256];
            let mut blend_table_b = [0u8; 256];
            for cov in 0..256u16 {
                let cov_scaled = (cov * fg_a) / 255;
                blend_table_r[cov as usize] =
                    ((bg_r * (255 - cov_scaled) + job.fg_r as u16 * cov_scaled) / 255).min(255)
                        as u8;
                blend_table_g[cov as usize] =
                    ((bg_g * (255 - cov_scaled) + job.fg_g as u16 * cov_scaled) / 255).min(255)
                        as u8;
                blend_table_b[cov as usize] =
                    ((bg_b * (255 - cov_scaled) + job.fg_b as u16 * cov_scaled) / 255).min(255)
                        as u8;
            }

            // Render glyph pixels
            for row in 0..job.height {
                let tex_row = job.tex_y + row;
                let dest_row = job.dest_y + row;

                if tex_row >= atlas_height_px || dest_row >= dest_height {
                    continue;
                }

                for col in 0..job.width {
                    let tex_col = job.tex_x + col;
                    let dest_col = job.dest_x + col;

                    if tex_col >= atlas_width_px || dest_col >= dest_width {
                        continue;
                    }

                    let atlas_offset = tex_row * atlas_stride + tex_col * 4;
                    let cov_b = atlas_data[atlas_offset + 0] as u16;
                    let cov_g = atlas_data[atlas_offset + 1] as u16;
                    let cov_a = atlas_data[atlas_offset + 3] as u16;
                    let cov_r = atlas_data[atlas_offset + 2] as u16;

                    if cov_a == 0 {
                        continue;
                    }

                    let dest_offset = dest_row * dest_stride + dest_col * 4;

                    let cov_r_lin = srgb8_to_linear_u8((cov_r * 255 / cov_a).min(255) as u8);
                    let cov_g_lin = srgb8_to_linear_u8((cov_g * 255 / cov_a).min(255) as u8);
                    let cov_b_lin = srgb8_to_linear_u8((cov_b * 255 / cov_a).min(255) as u8);

                    let out_r = blend_table_r[cov_r_lin as usize];
                    let out_g = blend_table_g[cov_g_lin as usize];
                    let out_b = blend_table_b[cov_b_lin as usize];

                    dest_data[dest_offset + 0] = out_b;
                    dest_data[dest_offset + 1] = out_g;
                    dest_data[dest_offset + 2] = out_r;
                    dest_data[dest_offset + 3] = 255;
                }
            }

            // Cache the rendered cell
            let cell_width = job.dest_width;
            let cell_height = job.cell_height;
            let mut cell_buffer = vec![0u8; cell_width * cell_height * 4];

            for row in 0..cell_height {
                let dest_row = job.cell_y + row;
                if dest_row >= dest_height {
                    break;
                }
                let src_start = dest_row * dest_stride + job.dest_x * 4;
                let dst_start = row * cell_width * 4;
                let copy_width = cell_width.min(dest_width.saturating_sub(job.dest_x));
                let copy_bytes = copy_width * 4;
                cell_buffer[dst_start..dst_start + copy_bytes]
                    .copy_from_slice(&dest_data[src_start..src_start + copy_bytes]);
            }

            {
                let mut state = self.cairo2d_state.borrow_mut();
                // LfuCache handles eviction automatically based on cairo2d_glyph_cache_size
                state.glyph_cache.put(
                    cache_key,
                    CachedGlyph {
                        pixels: cell_buffer,
                    },
                );
            }
        }

        drop(dest_data);
        drop(atlas_data);
        surface.mark_dirty();

        Ok(())
    }

    /// Present only the dirty regions of the frame
    fn present_partial_frame(
        &self,
        window: &::window::Window,
        pixels: &[u8],
        width: i32,
        height: i32,
        dirty_rows: &[DirtyRow],
    ) -> anyhow::Result<usize> {
        let stride = width as usize * 4;
        let mut bytes_sent = 0usize;
        let height_usize = height as usize;
        let full_frame_bytes = width as usize * height as usize * 4;

        log::debug!("cairo2d partial update: {} dirty regions", dirty_rows.len());

        for region in dirty_rows {
            if region.pixel_y >= height_usize {
                continue;
            }

            let pixel_offset = region.pixel_y * stride;
            let max_height = height_usize.saturating_sub(region.pixel_y);
            let region_height = region.pixel_height.min(max_height);

            if region_height == 0 {
                continue;
            }

            let region_bytes = region_height * stride;

            if pixel_offset + region_bytes <= pixels.len() {
                let region_pixels = &pixels[pixel_offset..pixel_offset + region_bytes];
                window.present_software_frame_region(
                    region_pixels,
                    width as u32,
                    region_height as u32,
                    0,
                    region.pixel_y as i16,
                )?;
                bytes_sent += region_bytes;
            }
        }

        metrics::counter!("cairo2d.partial.bytes_sent").increment(bytes_sent as u64);
        metrics::counter!("cairo2d.partial.bytes_saved")
            .increment((full_frame_bytes - bytes_sent) as u64);

        log::trace!(
            "cairo2d partial: {} regions, sent {} / {} bytes",
            dirty_rows.len(),
            bytes_sent,
            full_frame_bytes
        );

        Ok(bytes_sent)
    }
}
