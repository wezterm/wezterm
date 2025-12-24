use crate::colorease::ColorEaseUniform;
use crate::termwindow::webgpu::ShaderUniform;
use crate::termwindow::RenderFrame;
use crate::uniforms::UniformBuilder;
use ::window::glium;
use ::window::glium::uniforms::{
    MagnifySamplerFilter, MinifySamplerFilter, Sampler, SamplerWrapFunction,
};
use ::window::glium::{BlendingFunction, LinearBlendingFactor, Surface};
use config::FreeTypeLoadTarget;

impl crate::TermWindow {
    pub fn call_draw(&mut self, frame: &mut RenderFrame) -> anyhow::Result<()> {
        match frame {
            RenderFrame::Glium(ref mut frame) => self.call_draw_glium(frame),
            RenderFrame::WebGpu => self.call_draw_webgpu(),
            RenderFrame::Cairo2D => self.call_draw_cairo2d(),
        }
    }

    fn call_draw_cairo2d(&mut self) -> anyhow::Result<()> {
        use crate::quad::{VERTICES_PER_CELL, V_BOT_LEFT, V_BOT_RIGHT, V_TOP_LEFT, V_TOP_RIGHT};
        use crate::renderstate::VertexBuffer;
        use crate::termwindow::cairo2d::CairoTexture;
        use ::window::bitmaps::Texture2d;
        use ::window::WindowOps;
        use anyhow::Context;
        use cairo::{Format, ImageSurface, Operator};
        use std::cell::RefCell;
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use wezterm_color_types::srgb8_to_linear_u8;

        // Represents a dirty row region for partial updates
        #[derive(Clone, Debug)]
        struct DirtyRow {
            pixel_y: usize,      // Top pixel of the row
            pixel_height: usize, // Height in pixels
        }

        // Tracks hash and actual pixel bounds for a line bucket
        #[derive(Clone, Debug, Default)]
        struct LineBucket {
            hash: u64,
            min_y: usize, // Actual minimum pixel Y of content in this bucket
            max_y: usize, // Actual maximum pixel Y of content in this bucket
        }

        // Tracks bandwidth efficiency over a time window
        struct EfficiencyWindow {
            bytes_sent: u64,
            bytes_total: u64, // sent + saved (i.e., full frame equivalent)
            window_start: std::time::Instant,
            window_duration: std::time::Duration,
        }

        impl EfficiencyWindow {
            fn new(duration_secs: u64) -> Self {
                Self {
                    bytes_sent: 0,
                    bytes_total: 0,
                    window_start: std::time::Instant::now(),
                    window_duration: std::time::Duration::from_secs(duration_secs),
                }
            }

            // Add bytes and return current running efficiency for this window
            fn add(&mut self, sent: u64, total: u64) -> f64 {
                // Reset window if expired
                if self.window_start.elapsed() >= self.window_duration {
                    self.bytes_sent = 0;
                    self.bytes_total = 0;
                    self.window_start = std::time::Instant::now();
                }

                self.bytes_sent += sent;
                self.bytes_total += total;

                // Return current efficiency
                if self.bytes_total > 0 {
                    ((self.bytes_total - self.bytes_sent) as f64 / self.bytes_total as f64) * 100.0
                } else {
                    0.0
                }
            }
        }

        // Persistent surface and frame state for incremental rendering
        struct Cairo2DState {
            surface: Option<ImageSurface>,
            width: i32,
            height: i32,
            last_frame_hash: u64,
            // Per-line bucket data for detecting which lines changed
            line_buckets: Vec<LineBucket>,
            // Efficiency tracking over time windows
            efficiency_1s: EfficiencyWindow,
            efficiency_10s: EfficiencyWindow,
            efficiency_60s: EfficiencyWindow,
        }

        thread_local! {
            static CAIRO2D_STATE: RefCell<Cairo2DState> = RefCell::new(Cairo2DState {
                surface: None,
                width: 0,
                height: 0,
                last_frame_hash: 0,
                line_buckets: Vec::new(),
                efficiency_1s: EfficiencyWindow::new(1),
                efficiency_10s: EfficiencyWindow::new(10),
                efficiency_60s: EfficiencyWindow::new(60),
            });
        }

        // Convert linear RGB to sRGB (gamma correction)
        #[inline]
        fn linear_to_srgb(linear: f32) -> f64 {
            let srgb = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            srgb as f64
        }

        // Hash vertex fields for frame-level change detection
        #[inline(always)]
        fn hash_vertex_for_frame(v: &crate::quad::Vertex, hasher: &mut impl Hasher) {
            v.position[0].to_bits().hash(hasher);
            v.position[1].to_bits().hash(hasher);
            v.tex[0].to_bits().hash(hasher);
            v.tex[1].to_bits().hash(hasher);
            v.fg_color[0].to_bits().hash(hasher);
            v.fg_color[1].to_bits().hash(hasher);
            v.fg_color[2].to_bits().hash(hasher);
            v.fg_color[3].to_bits().hash(hasher);
            v.bg_color[0].to_bits().hash(hasher);
            v.bg_color[1].to_bits().hash(hasher);
            v.bg_color[2].to_bits().hash(hasher);
            v.bg_color[3].to_bits().hash(hasher);
            v.has_color.to_bits().hash(hasher);
        }

        // Hash vertex fields for line-level change detection (same as frame, for consistency)
        #[inline(always)]
        fn hash_vertex_for_line(v: &crate::quad::Vertex, hasher: &mut impl Hasher) {
            hash_vertex_for_frame(v, hasher);
        }

        let width = self.dimensions.pixel_width as i32;
        let height = self.dimensions.pixel_height as i32;
        let width_f = width as f64;
        let height_f = height as f64;
        let half_width = width_f / 2.0;
        let half_height = height_f / 2.0;

        // Get the default background color from palette (for cells with transparent bg)
        // Must be done before borrowing render_state
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
        // We process quads (4 vertices each) together to ensure all vertices of a quad
        // hash to the same line (using the quad's top Y position)
        // We also track the actual pixel Y bounds of content in each bucket
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

        for layer in render_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb_cell = &layer.vb.borrow()[idx];
                let (vertex_count, _) = vb_cell.vertex_index_count();
                if vertex_count > 0 {
                    let vertices_ref = vb_cell.current_vb_mut();
                    if let VertexBuffer::Cairo2D(cairo_vb) = &*vertices_ref {
                        let vertices = cairo_vb.vertices.borrow();
                        // Process quads (4 vertices each) to ensure consistent line bucketing
                        let num_quads = vertex_count / 4;
                        for quad_idx in 0..num_quads {
                            let base = quad_idx * 4;
                            if base + 4 > vertices.len() {
                                break;
                            }

                            // Find the actual Y bounds of this quad (all 4 vertices)
                            let quad_verts = &vertices[base..base + 4];
                            let mut quad_min_y = f32::MAX;
                            let mut quad_max_y = f32::MIN;
                            for v in quad_verts {
                                let vy = v.position[1] + half_height as f32;
                                quad_min_y = quad_min_y.min(vy);
                                quad_max_y = quad_max_y.max(vy);
                            }

                            // Get the top-left vertex to determine the quad's line bucket
                            let v_top = &vertices[base]; // V_TOP_LEFT

                            // Determine line index using the TOP of the quad
                            // For glyphs, use cell_y; for others, use the minimum y
                            let bucket_y = if v_top.cell_height > 0.0 {
                                v_top.cell_y + half_height as f32
                            } else {
                                quad_min_y
                            };

                            // Skip quads outside visible area
                            if bucket_y < 0.0 || bucket_y >= height_f as f32 {
                                // Still hash for frame-level detection
                                for v in quad_verts {
                                    hash_vertex_for_frame(v, &mut frame_hasher);
                                }
                                continue;
                            }

                            let line_idx = if cell_height > 0 {
                                (bucket_y as usize) / cell_height
                            } else {
                                0
                            };

                            // Update actual Y bounds for this line bucket
                            if line_idx < num_lines {
                                let qmin = quad_min_y.max(0.0) as usize;
                                let qmax =
                                    (quad_max_y.min(height_f as f32) as usize).min(height as usize);
                                line_min_y[line_idx] = line_min_y[line_idx].min(qmin);
                                line_max_y[line_idx] = line_max_y[line_idx].max(qmax);
                            }

                            // Hash all 4 vertices of the quad
                            for v in quad_verts {
                                // Frame-level hash (includes bg_color for selection changes)
                                hash_vertex_for_frame(v, &mut frame_hasher);

                                // Line-level hash (all vertices of this quad go to same line)
                                if line_idx < num_lines {
                                    hash_vertex_for_line(v, &mut line_hashers[line_idx]);
                                }
                            }
                        }
                    }
                }
            }
        }
        let frame_hash = frame_hasher.finish();
        // Build line buckets with hash and actual Y bounds
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
        let can_reuse = CAIRO2D_STATE.with(|state| {
            let state = state.borrow();
            state.surface.is_some()
                && state.width == width
                && state.height == height
                && state.last_frame_hash == frame_hash
        });

        if can_reuse {
            // Frame unchanged - just present the existing surface
            metrics::histogram!("cairo2d.frame.reused.rate").record(1.);
            CAIRO2D_STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some(ref mut surface) = state.surface {
                    surface.flush();
                    if let Ok(data) = surface.data() {
                        let pixels: Vec<u8> = data.to_vec();
                        if let Some(window) = self.window.as_ref() {
                            let _ =
                                window.present_software_frame(&pixels, width as u32, height as u32);
                        }
                    }
                }
            });
            return Ok(());
        }

        // Detect which lines have changed by comparing line bucket hashes
        // and use actual pixel bounds from buckets for precise dirty regions
        let (dirty_rows, force_full_redraw) = CAIRO2D_STATE.with(|state| {
            let state = state.borrow();
            let prev_buckets = &state.line_buckets;

            // Force full redraw if dimensions changed or no previous buckets
            if state.width != width || state.height != height || prev_buckets.is_empty() {
                return (Vec::new(), true);
            }

            // Find dirty lines by comparing hashes, collecting their actual pixel bounds
            struct DirtyLine {
                idx: usize,
                min_y: usize,
                max_y: usize,
            }
            let mut dirty_lines: Vec<DirtyLine> = Vec::new();
            for (idx, bucket) in current_line_buckets.iter().enumerate() {
                let prev_hash = prev_buckets.get(idx).map(|b| b.hash).unwrap_or(0);
                if bucket.hash != prev_hash {
                    // Use actual content bounds, but fall back to bucket bounds if no content
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

            // If more than 50% of lines are dirty, full redraw is more efficient
            let dirty_ratio = dirty_lines.len() as f32 / num_lines.max(1) as f32;
            if dirty_ratio > 0.5 {
                metrics::counter!("cairo2d.partial.full_redraw_threshold").increment(1);
                return (Vec::new(), true);
            }

            // Coalesce adjacent dirty lines into regions using actual pixel bounds
            // Allow small gaps (up to 2 lines) to reduce number of PutImage calls
            let mut dirty_rows: Vec<DirtyRow> = Vec::new();
            let mut region_start_idx: Option<usize> = None;
            let mut region_end_idx: usize = 0;
            let mut region_min_y: usize = 0;
            let mut region_max_y: usize = 0;

            for dirty in &dirty_lines {
                match region_start_idx {
                    None => {
                        // Start a new region
                        region_start_idx = Some(dirty.idx);
                        region_end_idx = dirty.idx;
                        region_min_y = dirty.min_y;
                        region_max_y = dirty.max_y;
                    }
                    Some(_) => {
                        // Check if this line is adjacent or within gap tolerance
                        if dirty.idx <= region_end_idx + 3 {
                            // Extend current region
                            region_end_idx = dirty.idx;
                            region_min_y = region_min_y.min(dirty.min_y);
                            region_max_y = region_max_y.max(dirty.max_y);
                        } else {
                            // Flush current region and start new one
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
            // Flush final region
            if region_start_idx.is_some() {
                let pixel_height = region_max_y.saturating_sub(region_min_y);
                if pixel_height > 0 && region_min_y < height as usize {
                    dirty_rows.push(DirtyRow {
                        pixel_y: region_min_y,
                        pixel_height: pixel_height
                            .min((height as usize).saturating_sub(region_min_y)),
                    });
                }
            }

            // Record metrics - use counters for counts, not histograms
            metrics::counter!("cairo2d.partial.dirty_lines_total")
                .increment(dirty_lines.len() as u64);
            metrics::counter!("cairo2d.partial.dirty_regions_total")
                .increment(dirty_rows.len() as u64);

            (dirty_rows, false)
        });

        // Track whether we're doing a partial or full update
        let do_partial_update = !force_full_redraw && !dirty_rows.is_empty();
        if do_partial_update {
            metrics::histogram!("cairo2d.frame.partial.rate").record(1.);
        } else {
            metrics::histogram!("cairo2d.frame.rendered.rate").record(1.);
        }

        // Get or create the persistent surface
        let surface = CAIRO2D_STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.surface.is_none() || state.width != width || state.height != height {
                state.surface = ImageSurface::create(Format::ARgb32, width, height).ok();
                state.width = width;
                state.height = height;
                state.line_buckets.clear();
            }
            state.surface.take()
        });
        let mut surface = surface.context("Failed to get Cairo surface")?;

        // Collect glyph rendering jobs for batched processing
        struct GlyphJob {
            dest_x: usize,
            dest_y: usize,
            dest_width: usize,  // Glyph quad width
            cell_y: usize,      // Cell top position (for full cell background fill)
            cell_height: usize, // Cell height (for full cell background fill)
            tex_x: usize,
            tex_y: usize,
            width: usize,  // Glyph texture width
            height: usize, // Glyph texture height
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
        let mut glyph_jobs: Vec<GlyphJob> = Vec::new();

        // PASS 1: Render all Cairo operations (solid colors, images) and collect glyph jobs
        {
            let ctx = cairo::Context::new(&surface).context("Failed to create Cairo context")?;

            // Clear to the default background color (not transparent black)
            // This ensures cells with "default" background have the correct color
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
                    let vertices = match &*vertices_ref {
                        VertexBuffer::Cairo2D(cairo_vb) => cairo_vb.vertices.borrow(),
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

                        let dest_x = tl.position[0] as f64 + half_width;
                        let dest_y = tl.position[1] as f64 + half_height;
                        let dest_width = (tr.position[0] - tl.position[0]) as f64;
                        let dest_height = (bl.position[1] - tl.position[1]) as f64;

                        if dest_width <= 0.0 || dest_height <= 0.0 {
                            continue;
                        }

                        let has_color = tl.has_color;

                        if has_color == 3.0 {
                            // Solid color - render with Cairo
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
                                let [fg_r, fg_g, fg_b, fg_a] = tl.fg_color;
                                let [bg_r, bg_g, bg_b, bg_a] = tl.bg_color;
                                // cell_y and cell_height are stored in vertex for full-cell background fill
                                let cell_y = (tl.cell_y as f64 + half_height) as usize;
                                let cell_height = tl.cell_height as usize;
                                glyph_jobs.push(GlyphJob {
                                    dest_x: dest_x as usize,
                                    dest_y: dest_y as usize,
                                    dest_width: dest_width as usize,
                                    cell_y,
                                    cell_height,
                                    tex_x: tex_x1 as usize,
                                    tex_y: tex_y1 as usize,
                                    // Use actual glyph dimensions from atlas for caching
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
                                    glyph_id: tl.glyph_id,
                                });
                            } else {
                                // Color emoji or background image - render with Cairo
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
                    drop(vertices_ref);
                    vb_cell.next_index();
                }
            }
        } // ctx dropped here

        // PASS 2: Batch process all glyphs with caching
        if !glyph_jobs.is_empty() {
            use std::cell::RefCell;
            use std::collections::HashMap;

            // Cache key: (glyph_id, fg_color, bg_color, cell_dimensions)
            // We cache the full cell including background, so cell size is part of the key.
            #[derive(Hash, Eq, PartialEq, Clone, Copy)]
            struct GlyphCacheKey {
                glyph_id: u32,
                fg_rgba: u32,
                bg_rgba: u32,
                cell_width: u16,
                cell_height: u16,
            }

            struct CachedGlyph {
                pixels: Vec<u8>, // Pre-rendered BGRA pixels (full cell)
            }

            thread_local! {
                static GLYPH_CACHE: RefCell<HashMap<GlyphCacheKey, CachedGlyph>> = RefCell::new(HashMap::new());
            }

            #[inline]
            fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
                (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
            }

            let mut atlas_surface_mut = atlas.surface_mut();
            let atlas_stride = atlas_surface_mut.stride() as usize;
            let atlas_width_px = atlas_surface_mut.width() as usize;
            let atlas_height_px = atlas_surface_mut.height() as usize;
            let dest_stride = surface.stride() as usize;
            let dest_width = width as usize;
            let dest_height = height as usize;

            let atlas_data = atlas_surface_mut.data().expect("Failed to get atlas data");
            let mut dest_data = surface.data().expect("Failed to get destination data");

            for job in &glyph_jobs {
                // Use the background color from the vertex data (job.bg_*).
                // This is the correct composited background color, already blended
                // with selection highlighting if applicable (done in compute_cell_fg_bg
                // via composite_over).
                let actual_bg_r = job.bg_r;
                let actual_bg_g = job.bg_g;
                let actual_bg_b = job.bg_b;
                // Since bg_color already includes the composited selection color,
                // we can always cache based on (glyph_id, fg, bg).
                let can_cache = true;

                // Fill the FULL CELL area with the correct background color.
                // Use cell_y and cell_height (not dest_y/dest_height) because glyphs
                // have y-offsets for baseline alignment - they don't cover the entire cell.
                // This ensures no gaps between lines.
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
                        // Cairo BGRA format
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
                    cell_height: job.cell_height as u16, // Use full cell height
                };

                // Try cache lookup
                let cache_hit = if can_cache {
                    GLYPH_CACHE.with(|cache| {
                        let cache_ref = cache.borrow();
                        if let Some(cached) = cache_ref.get(&cache_key) {
                            // Copy full cached cell to destination
                            let cell_width = job.dest_width;
                            let cell_height = job.cell_height;
                            for row in 0..cell_height {
                                let dest_row = job.cell_y + row; // Use cell_y, not dest_y
                                if dest_row >= dest_height {
                                    break;
                                }
                                let copy_width =
                                    cell_width.min(dest_width.saturating_sub(job.dest_x));
                                if copy_width == 0 {
                                    continue;
                                }
                                let src_start = row * cell_width * 4;
                                let dest_start = dest_row * dest_stride + job.dest_x * 4;
                                let copy_bytes = copy_width * 4;
                                dest_data[dest_start..dest_start + copy_bytes].copy_from_slice(
                                    &cached.pixels[src_start..src_start + copy_bytes],
                                );
                            }
                            metrics::histogram!("cairo2d.cache.hit.rate").record(1.);
                            return true;
                        }
                        false
                    })
                } else {
                    false
                };

                if cache_hit {
                    continue;
                }
                if !can_cache {
                    metrics::histogram!("cairo2d.cache.skip.rate").record(1.);
                } else {
                    metrics::histogram!("cairo2d.cache.miss.rate").record(1.);
                }

                // Render glyph using actual background color for blending
                let fg_a = job.fg_a as u16;
                let bg_r = actual_bg_r as u16;
                let bg_g = actual_bg_g as u16;
                let bg_b = actual_bg_b as u16;

                // Pre-compute blend tables for this fg/bg combination
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

                // Render glyph pixels on top of the background (already filled above)
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

                        // Read coverage from atlas (BGRA format, premultiplied)
                        let cov_b = atlas_data[atlas_offset + 0] as u16;
                        let cov_g = atlas_data[atlas_offset + 1] as u16;
                        let cov_a = atlas_data[atlas_offset + 3] as u16;
                        let cov_r = atlas_data[atlas_offset + 2] as u16;

                        // Skip pixels with no coverage - background already filled
                        if cov_a == 0 {
                            continue;
                        }

                        let dest_offset = dest_row * dest_stride + dest_col * 4;

                        // Un-premultiply and convert to linear
                        let cov_r_lin = srgb8_to_linear_u8((cov_r * 255 / cov_a).min(255) as u8);
                        let cov_g_lin = srgb8_to_linear_u8((cov_g * 255 / cov_a).min(255) as u8);
                        let cov_b_lin = srgb8_to_linear_u8((cov_b * 255 / cov_a).min(255) as u8);

                        // Use blend tables to compute output pixel
                        let out_r = blend_table_r[cov_r_lin as usize];
                        let out_g = blend_table_g[cov_g_lin as usize];
                        let out_b = blend_table_b[cov_b_lin as usize];

                        // Write to destination
                        dest_data[dest_offset + 0] = out_b;
                        dest_data[dest_offset + 1] = out_g;
                        dest_data[dest_offset + 2] = out_r;
                        dest_data[dest_offset + 3] = 255;
                    }
                }

                // Cache the full cell (background + glyph)
                if can_cache {
                    let cell_width = job.dest_width;
                    let cell_height = job.cell_height;
                    let mut cell_buffer = vec![0u8; cell_width * cell_height * 4];

                    // Copy the rendered cell from destination to cache buffer
                    for row in 0..cell_height {
                        let dest_row = job.cell_y + row; // Use cell_y, not dest_y
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

                    GLYPH_CACHE.with(|cache| {
                        let mut cache_mut = cache.borrow_mut();
                        // Simple eviction: clear if too many entries
                        if cache_mut.len() > 10000 {
                            metrics::histogram!("cairo2d.cache.evict.rate").record(1.);
                            cache_mut.clear();
                        }
                        cache_mut.insert(
                            cache_key,
                            CachedGlyph {
                                pixels: cell_buffer,
                            },
                        );
                    });
                }
            }

            drop(dest_data);
            drop(atlas_data);
            surface.mark_dirty();
        }

        // Get the pixel data and present it
        surface.flush();
        let data = surface.data().context("Failed to get surface data")?;

        // Cairo's ARGB32 is actually BGRA in memory on little-endian systems
        // which matches what X11 expects for depth 32 with a TrueColor visual
        let pixels: Vec<u8> = data.to_vec();
        drop(data);

        // Present the frame - use partial updates if we detected dirty regions
        if let Some(window) = self.window.as_ref() {
            let full_frame_bytes = width as usize * height as usize * 4;
            let bytes_sent = if do_partial_update && !dirty_rows.is_empty() {
                // Partial update: send only dirty regions
                let stride = width as usize * 4; // BGRA = 4 bytes per pixel
                let mut bytes_sent = 0usize;
                let height_usize = height as usize;

                log::debug!("cairo2d partial update: {} dirty regions", dirty_rows.len());

                for region in &dirty_rows {
                    // Skip regions that start beyond the image
                    if region.pixel_y >= height_usize {
                        continue;
                    }

                    // Bounds check with saturating subtraction to prevent underflow
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

                // Record partial update metrics
                metrics::counter!("cairo2d.partial.bytes_sent").increment(bytes_sent as u64);
                metrics::counter!("cairo2d.partial.bytes_saved")
                    .increment((full_frame_bytes - bytes_sent) as u64);
                log::trace!(
                    "cairo2d partial: {} regions, sent {} / {} bytes",
                    dirty_rows.len(),
                    bytes_sent,
                    full_frame_bytes
                );

                bytes_sent
            } else {
                // Full frame update
                metrics::counter!("cairo2d.full.bytes_sent").increment(full_frame_bytes as u64);
                window.present_software_frame(&pixels, width as u32, height as u32)?;
                full_frame_bytes
            };

            // Update time-windowed efficiency gauges (unified for both paths)
            CAIRO2D_STATE.with(|state| {
                let mut state = state.borrow_mut();
                let eff_1s = state
                    .efficiency_1s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                let eff_10s = state
                    .efficiency_10s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                let eff_60s = state
                    .efficiency_60s
                    .add(bytes_sent as u64, full_frame_bytes as u64);
                metrics::gauge!("cairo2d.efficiency_1s_pct").set(eff_1s);
                metrics::gauge!("cairo2d.efficiency_10s_pct").set(eff_10s);
                metrics::gauge!("cairo2d.efficiency_60s_pct").set(eff_60s);
            });
        }

        // Store the surface and line buckets back for reuse next frame
        CAIRO2D_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.surface = Some(surface);
            state.last_frame_hash = frame_hash;
            state.line_buckets = current_line_buckets;
        });

        Ok(())
    }

    fn call_draw_webgpu(&mut self) -> anyhow::Result<()> {
        use crate::termwindow::webgpu::WebGpuTexture;

        let webgpu = self.webgpu.as_mut().unwrap();
        let render_state = self.render_state.as_ref().unwrap();

        let output = webgpu.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = webgpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });
        let tex = render_state.glyph_cache.borrow().atlas.texture();
        let tex = tex.downcast_ref::<WebGpuTexture>().unwrap();
        let texture_view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_linear_bind_group =
            webgpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                layout: &webgpu.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&webgpu.texture_linear_sampler),
                    },
                ],
                label: Some("linear bind group"),
            });

        let texture_nearest_bind_group =
            webgpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                layout: &webgpu.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&webgpu.texture_nearest_sampler),
                    },
                ],
                label: Some("nearest bind group"),
            });

        let mut cleared = false;
        let foreground_text_hsb = self.config.foreground_text_hsb;
        let foreground_text_hsb = [
            foreground_text_hsb.hue,
            foreground_text_hsb.saturation,
            foreground_text_hsb.brightness,
        ];

        let milliseconds = self.created.elapsed().as_millis() as u32;
        let projection = euclid::Transform3D::<f32, f32, f32>::ortho(
            -(self.dimensions.pixel_width as f32) / 2.0,
            self.dimensions.pixel_width as f32 / 2.0,
            self.dimensions.pixel_height as f32 / 2.0,
            -(self.dimensions.pixel_height as f32) / 2.0,
            -1.0,
            1.0,
        )
        .to_arrays_transposed();

        for layer in render_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb = &layer.vb.borrow()[idx];
                let (vertex_count, index_count) = vb.vertex_index_count();
                let vertex_buffer;
                let uniforms;
                if vertex_count > 0 {
                    let mut vertices = vb.current_vb_mut();
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Render Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: if cleared {
                                    wgpu::LoadOp::Load
                                } else {
                                    wgpu::LoadOp::Clear(wgpu::Color {
                                        r: 0.,
                                        g: 0.,
                                        b: 0.,
                                        a: 0.,
                                    })
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });
                    cleared = true;

                    uniforms = webgpu.create_uniform(ShaderUniform {
                        foreground_text_hsb,
                        milliseconds,
                        projection,
                    });

                    render_pass.set_pipeline(&webgpu.render_pipeline);
                    render_pass.set_bind_group(0, &uniforms, &[]);
                    render_pass.set_bind_group(1, &texture_linear_bind_group, &[]);
                    render_pass.set_bind_group(2, &texture_nearest_bind_group, &[]);
                    vertex_buffer = vertices.webgpu_mut().recreate();
                    vertex_buffer.unmap();
                    render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                    render_pass
                        .set_index_buffer(vb.indices.webgpu().slice(..), wgpu::IndexFormat::Uint32);
                    render_pass.draw_indexed(0..index_count as _, 0, 0..1);
                }

                vb.next_index();
            }
        }

        // submit will accept anything that implements IntoIter
        webgpu.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }

    fn call_draw_glium(&mut self, frame: &mut glium::Frame) -> anyhow::Result<()> {
        use window::glium::texture::SrgbTexture2d;

        let gl_state = self.render_state.as_ref().unwrap();
        let tex = gl_state.glyph_cache.borrow().atlas.texture();
        let tex = tex.downcast_ref::<SrgbTexture2d>().unwrap();

        frame.clear_color(0., 0., 0., 0.);

        let projection = euclid::Transform3D::<f32, f32, f32>::ortho(
            -(self.dimensions.pixel_width as f32) / 2.0,
            self.dimensions.pixel_width as f32 / 2.0,
            self.dimensions.pixel_height as f32 / 2.0,
            -(self.dimensions.pixel_height as f32) / 2.0,
            -1.0,
            1.0,
        )
        .to_arrays_transposed();

        let use_subpixel = match self
            .config
            .freetype_render_target
            .unwrap_or(self.config.freetype_load_target)
        {
            FreeTypeLoadTarget::HorizontalLcd | FreeTypeLoadTarget::VerticalLcd => true,
            _ => false,
        };

        let dual_source_blending = glium::DrawParameters {
            blend: glium::Blend {
                color: BlendingFunction::Addition {
                    source: LinearBlendingFactor::SourceOneColor,
                    destination: LinearBlendingFactor::OneMinusSourceOneColor,
                },
                alpha: BlendingFunction::Addition {
                    source: LinearBlendingFactor::SourceOneColor,
                    destination: LinearBlendingFactor::OneMinusSourceOneColor,
                },
                constant_value: (0.0, 0.0, 0.0, 0.0),
            },

            ..Default::default()
        };

        let alpha_blending = glium::DrawParameters {
            blend: glium::Blend {
                color: BlendingFunction::Addition {
                    source: LinearBlendingFactor::SourceAlpha,
                    destination: LinearBlendingFactor::OneMinusSourceAlpha,
                },
                alpha: BlendingFunction::Addition {
                    source: LinearBlendingFactor::One,
                    destination: LinearBlendingFactor::OneMinusSourceAlpha,
                },
                constant_value: (0.0, 0.0, 0.0, 0.0),
            },
            ..Default::default()
        };

        // Clamp and use the nearest texel rather than interpolate.
        // This prevents things like the box cursor outlines from
        // being randomly doubled in width or height
        let atlas_nearest_sampler = Sampler::new(&*tex)
            .wrap_function(SamplerWrapFunction::Clamp)
            .magnify_filter(MagnifySamplerFilter::Nearest)
            .minify_filter(MinifySamplerFilter::Nearest);

        let atlas_linear_sampler = Sampler::new(&*tex)
            .wrap_function(SamplerWrapFunction::Clamp)
            .magnify_filter(MagnifySamplerFilter::Linear)
            .minify_filter(MinifySamplerFilter::Linear);

        let foreground_text_hsb = self.config.foreground_text_hsb;
        let foreground_text_hsb = (
            foreground_text_hsb.hue,
            foreground_text_hsb.saturation,
            foreground_text_hsb.brightness,
        );

        let milliseconds = self.created.elapsed().as_millis() as u32;

        let cursor_blink: ColorEaseUniform = (*self.cursor_blink_state.borrow()).into();
        let blink: ColorEaseUniform = (*self.blink_state.borrow()).into();
        let rapid_blink: ColorEaseUniform = (*self.rapid_blink_state.borrow()).into();

        for layer in gl_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb = &layer.vb.borrow()[idx];
                let (vertex_count, index_count) = vb.vertex_index_count();
                if vertex_count > 0 {
                    let vertices = vb.current_vb_mut();
                    let subpixel_aa = use_subpixel && idx == 1;

                    let mut uniforms = UniformBuilder::default();

                    uniforms.add("projection", &projection);
                    uniforms.add("atlas_nearest_sampler", &atlas_nearest_sampler);
                    uniforms.add("atlas_linear_sampler", &atlas_linear_sampler);
                    uniforms.add("foreground_text_hsb", &foreground_text_hsb);
                    uniforms.add("subpixel_aa", &subpixel_aa);
                    uniforms.add("milliseconds", &milliseconds);
                    uniforms.add_struct("cursor_blink", &cursor_blink);
                    uniforms.add_struct("blink", &blink);
                    uniforms.add_struct("rapid_blink", &rapid_blink);

                    frame.draw(
                        vertices.glium().slice(0..vertex_count).unwrap(),
                        vb.indices.glium().slice(0..index_count).unwrap(),
                        gl_state.glyph_prog.as_ref().unwrap(),
                        &uniforms,
                        if subpixel_aa {
                            &dual_source_blending
                        } else {
                            &alpha_blending
                        },
                    )?;
                }

                vb.next_index();
            }
        }

        Ok(())
    }
}
