use crate::quad::{Vertex, VERTICES_PER_CELL};
use crate::renderstate::VertexBuffer;
use crate::selection::SelectionRange;
use ::window::bitmaps::{BitmapImage, ImageTexture};
use ::window::WindowOps;
use std::time::Instant;
use termwiz::surface::SequenceNo;

#[derive(Clone, Debug)]
pub struct DirtyRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub struct PureCpuState {
    pub frame_buffer: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// When true, the entire framebuffer will be cleared and repainted
    pub force_full_repaint: bool,
    /// Dirty pixel regions for the current frame
    pub dirty_pixel_rects: Vec<DirtyRect>,
    /// Generation counters for detecting config/shape/quad changes
    pub last_config_generation: usize,
    pub last_shape_generation: usize,
    pub last_quad_generation: usize,
    /// Last resolved viewport position (tracks physical_top when following bottom)
    pub last_resolved_viewport: Option<wezterm_term::StableRowIndex>,
    /// Last cursor position for tracking cursor movement
    pub last_cursor_y: Option<wezterm_term::StableRowIndex>,
    pub last_cursor_x: Option<usize>,
    /// Last seqno for change detection
    pub last_seqno: SequenceNo,
    /// Last selection range — force full repaint when it changes
    pub last_selection_range: Option<SelectionRange>,
    /// Last quantized cursor blink phase (true = visible) for
    /// detecting blink transitions without running paint_impl every frame.
    pub last_blink_visible: bool,
}

impl PureCpuState {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            frame_buffer: vec![0u8; (width * height * 4) as usize],
            width,
            height,
            force_full_repaint: true,
            dirty_pixel_rects: vec![],
            last_config_generation: 0,
            last_shape_generation: 0,
            last_quad_generation: 0,
            last_resolved_viewport: None,
            last_cursor_y: None,
            last_cursor_x: None,
            last_seqno: 0,
            last_selection_range: None,
            last_blink_visible: true,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.frame_buffer.resize((width * height * 4) as usize, 0);
            self.force_full_repaint = true;
        }
    }
}

/// Collect per-dirty-rect clip regions for a quad.
/// Each entry is the intersection of the quad with one dirty rect.
/// This avoids the bounding-box problem where non-adjacent dirty rects
/// cause large quads to overwrite clean areas between them.
#[inline]
fn collect_clip_rects(
    dest_x: i32,
    dest_y: i32,
    dest_x2: i32,
    dest_y2: i32,
    rects: &[DirtyRect],
    out: &mut Vec<[i32; 4]>,
) {
    out.clear();
    for r in rects {
        let rx2 = r.x + r.width;
        let ry2 = r.y + r.height;
        if dest_x < rx2 && dest_x2 > r.x && dest_y < ry2 && dest_y2 > r.y {
            out.push([
                dest_x.max(r.x),
                dest_y.max(r.y),
                dest_x2.min(rx2),
                dest_y2.min(ry2),
            ]);
        }
    }
}

/// Clear a rectangular region in the framebuffer to black (zero)
fn clear_rect(fb: &mut [u8], fb_w: usize, fb_h: usize, rect: &DirtyRect) {
    let x0 = rect.x.max(0) as usize;
    let y0 = rect.y.max(0) as usize;
    let x1 = (rect.x + rect.width).min(fb_w as i32) as usize;
    let y1 = (rect.y + rect.height).min(fb_h as i32) as usize;

    for y in y0..y1 {
        let row_start = (y * fb_w + x0) * 4;
        let row_end = (y * fb_w + x1) * 4;
        if row_end <= fb.len() {
            fb[row_start..row_end].fill(0);
        }
    }
}

/// Coalesce dirty rects into full-width horizontal bands for XPutImage.
/// XPutImage needs contiguous pixel data, so we use full-width bands
/// where the row data IS contiguous in the framebuffer.
fn coalesce_to_bands(rects: &[DirtyRect], screen_width: u32) -> Vec<DirtyRect> {
    if rects.is_empty() {
        return vec![];
    }

    // Collect all y-ranges
    let mut y_ranges: Vec<(i32, i32)> = rects.iter().map(|r| (r.y, r.y + r.height)).collect();
    y_ranges.sort_by_key(|r| r.0);

    // Merge overlapping y-ranges
    let mut merged_bands: Vec<(i32, i32)> = vec![y_ranges[0]];
    for &(start, end) in &y_ranges[1..] {
        let last = merged_bands.last_mut().unwrap();
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged_bands.push((start, end));
        }
    }

    merged_bands
        .into_iter()
        .map(|(y, y2)| DirtyRect {
            x: 0,
            y,
            width: screen_width as i32,
            height: y2 - y,
        })
        .collect()
}

impl crate::TermWindow {
    pub fn call_draw_purecpu(&mut self) -> anyhow::Result<()> {
        let render_state = self.render_state.as_ref().unwrap();
        let tex = render_state.glyph_cache.borrow().atlas.texture();
        let tex = tex.downcast_ref::<ImageTexture>().unwrap();
        let atlas_image = tex.image.borrow();
        let (atlas_w, atlas_h) = atlas_image.image_dimensions();
        let atlas_data = atlas_image.pixel_data_slice();
        let atlas_stride = atlas_w * 4;

        let state = self.purecpu_state.as_mut().unwrap();
        let fb_w = state.width as usize;
        let fb_h = state.height as usize;
        let screen_width = state.width;
        let screen_height = state.height;

        let full_repaint = state.force_full_repaint;

        // Determine effective dirty rects and clear framebuffer regions.
        // Neighboring glyphs that overhang into dirty regions are caught by
        // the overlap test at blit time and re-blitted automatically.
        let effective_dirty: Vec<DirtyRect>;
        if full_repaint {
            state.frame_buffer.fill(0);
            state.force_full_repaint = false;
            state.dirty_pixel_rects.clear();
            effective_dirty = vec![];
            metrics::histogram!("purecpu.full_repaint.rate").record(1.);
        } else {
            metrics::histogram!("purecpu.dirty_repaint.rate").record(1.);
            metrics::histogram!("purecpu.dirty_rects").record(
                state.dirty_pixel_rects.len() as f64,
            );

            let dirty_pixels: i64 = state
                .dirty_pixel_rects
                .iter()
                .map(|r| (r.width as i64) * (r.height as i64))
                .sum();
            let total_pixels = (fb_w * fb_h) as i64;
            metrics::histogram!("purecpu.dirty_pixel_pct").record(
                if total_pixels > 0 {
                    100.0 * dirty_pixels as f64 / total_pixels as f64
                } else {
                    0.0
                },
            );

            let effective = state.dirty_pixel_rects.clone();

            // Clear dirty regions in framebuffer before re-blitting
            for rect in &effective {
                clear_rect(&mut state.frame_buffer, fb_w, fb_h, rect);
            }

            state.dirty_pixel_rects.clear();
            effective_dirty = effective;
        }

        let blit_start = Instant::now();
        let mut quads_total: u64 = 0;
        let mut quads_blitted: u64 = 0;

        let foreground_text_hsb = self.config.foreground_text_hsb;
        let apply_hsv = foreground_text_hsb.hue != 1.0
            || foreground_text_hsb.saturation != 1.0
            || foreground_text_hsb.brightness != 1.0;

        let half_w = fb_w as f32 / 2.0;
        let half_h = fb_h as f32 / 2.0;

        for layer in render_state.layers.borrow().iter() {
            for idx in 0..3 {
                let vb = &layer.vb.borrow()[idx];
                let (vertex_count, _index_count) = vb.vertex_index_count();
                if vertex_count == 0 {
                    continue;
                }
                let bufs = vb.current_vb_mut();
                let vertices: &[Vertex] = match &*bufs {
                    VertexBuffer::PureCpu(v) => &v[..vertex_count],
                    _ => continue,
                };

                let num_quads = vertex_count / VERTICES_PER_CELL;
                quads_total += num_quads as u64;
                let mut clip_rects: Vec<[i32; 4]> = Vec::new();
                for q in 0..num_quads {
                    let base = q * VERTICES_PER_CELL;
                    let tl = &vertices[base];     // top-left
                    let _tr = &vertices[base + 1]; // top-right
                    let _bl = &vertices[base + 2]; // bot-left
                    let br = &vertices[base + 3]; // bot-right

                    let has_color = tl.has_color;
                    let fg = tl.fg_color;
                    let alt = tl.alt_color;
                    let mix_value = tl.mix_value;
                    let hsv = tl.hsv;

                    // Mix fg and alt color
                    let fg_r = fg[0] * (1.0 - mix_value) + alt[0] * mix_value;
                    let fg_g = fg[1] * (1.0 - mix_value) + alt[1] * mix_value;
                    let fg_b = fg[2] * (1.0 - mix_value) + alt[2] * mix_value;
                    let fg_a = fg[3] * (1.0 - mix_value) + alt[3] * mix_value;

                    // Screen destination rect (clip-space to pixels)
                    let dest_x = (tl.position[0] + half_w) as i32;
                    let dest_y = (tl.position[1] + half_h) as i32;
                    let dest_x2 = (br.position[0] + half_w) as i32;
                    let dest_y2 = (br.position[1] + half_h) as i32;

                    let dest_w = dest_x2 - dest_x;
                    let dest_h = dest_y2 - dest_y;
                    if dest_w <= 0 || dest_h <= 0 {
                        continue;
                    }

                    // Build the list of clip rects for this quad.
                    // For full repaint: one clip rect = the entire quad.
                    // For incremental: one clip rect per overlapping dirty rect
                    // (the intersection). This avoids the bounding-box problem
                    // where non-adjacent dirty rects cause large quads to
                    // overwrite clean framebuffer areas between them.
                    if full_repaint {
                        clip_rects.clear();
                        clip_rects.push([dest_x, dest_y, dest_x2, dest_y2]);
                    } else {
                        collect_clip_rects(
                            dest_x, dest_y, dest_x2, dest_y2,
                            &effective_dirty, &mut clip_rects,
                        );
                        if clip_rects.is_empty() {
                            continue;
                        }
                    }

                    quads_blitted += 1;

                    // Atlas pixel rect from normalized tex coords
                    let tex_px_x = (tl.tex[0] * atlas_w as f32) as i32;
                    let tex_px_y = (tl.tex[1] * atlas_h as f32) as i32;
                    let tex_px_x2 = (br.tex[0] * atlas_w as f32) as i32;
                    let tex_px_y2 = (br.tex[1] * atlas_h as f32) as i32;
                    let tex_w = tex_px_x2 - tex_px_x;
                    let tex_h = tex_px_y2 - tex_px_y;

                    let state = self.purecpu_state.as_mut().unwrap();

                    if has_color == 3.0 {
                        // IS_SOLID_COLOR: fill each clip rect with fg color
                        let mut sr = fg_r;
                        let mut sg = fg_g;
                        let mut sb = fg_b;
                        let sa = fg_a;

                        if hsv[0] != 1.0 || hsv[1] != 1.0 || hsv[2] != 1.0 {
                            let (h, s, v) = rgb_to_hsv(sr, sg, sb);
                            let (nr, ng, nb) =
                                hsv_to_rgb(h * hsv[0], s * hsv[1], v * hsv[2]);
                            sr = nr;
                            sg = ng;
                            sb = nb;
                        }

                        sr = linear_to_srgb(sr);
                        sg = linear_to_srgb(sg);
                        sb = linear_to_srgb(sb);

                        let sb8 = (sb.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                        let sg8 = (sg.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                        let sr8 = (sr.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                        let sa8 = (sa.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;

                        for clip in &clip_rects {
                            let [cx1, cy1, cx2, cy2] = *clip;
                            for dy in cy1..cy2 {
                                if dy < 0 || dy >= fb_h as i32 {
                                    continue;
                                }
                                let row_off = dy as usize * fb_w;
                                for dx in cx1..cx2 {
                                    if dx < 0 || dx >= fb_w as i32 {
                                        continue;
                                    }
                                    let fi = (row_off + dx as usize) * 4;
                                    blend_over(&mut state.frame_buffer, fi, sr8, sg8, sb8, sa8);
                                }
                            }
                        }
                        continue;
                    }

                    // For textured quads: blit 1:1 from atlas to dest,
                    // clipped to each dirty region individually.
                    let blit_w = tex_w.min(dest_w);
                    let blit_h = tex_h.min(dest_h);

                    if blit_w <= 0 || blit_h <= 0 {
                        continue;
                    }

                    for clip in &clip_rects {
                        let [cx1, cy1, cx2, cy2] = *clip;

                        // Clipped row/col ranges relative to dest origin
                        let row_start = (cy1 - dest_y).max(0);
                        let row_end = (cy2 - dest_y).min(blit_h);
                        let col_start = (cx1 - dest_x).max(0);
                        let col_end = (cx2 - dest_x).min(blit_w);

                        for row in row_start..row_end {
                            let dy = dest_y + row;
                            if dy < 0 || dy >= fb_h as i32 {
                                continue;
                            }
                            let atlas_row = tex_px_y + row;
                            if atlas_row < 0 || atlas_row >= atlas_h as i32 {
                                continue;
                            }
                            let fb_row_off = dy as usize * fb_w;
                            let atlas_row_off = atlas_row as usize * atlas_stride;

                            for col in col_start..col_end {
                                let dx = dest_x + col;
                                if dx < 0 || dx >= fb_w as i32 {
                                    continue;
                                }
                                let atlas_col = tex_px_x + col;
                                if atlas_col < 0 || atlas_col >= atlas_w as i32 {
                                    continue;
                                }

                                let ai = atlas_row_off + atlas_col as usize * 4;
                                // Atlas is RGBA (ImageTexture stores RGBA despite
                                // BitmapImage docs claiming BGRA)
                                let tex_r = atlas_data[ai] as f32 / 255.0;
                                let tex_g = atlas_data[ai + 1] as f32 / 255.0;
                                let tex_b = atlas_data[ai + 2] as f32 / 255.0;
                                let tex_a = atlas_data[ai + 3] as f32 / 255.0;

                                let (mut out_r, mut out_g, mut out_b, out_a);

                                if has_color == 2.0 {
                                    // IS_BG_IMAGE
                                    out_r = tex_r;
                                    out_g = tex_g;
                                    out_b = tex_b;
                                    out_a = tex_a * fg_a;
                                } else if has_color == 1.0 {
                                    // IS_COLOR_EMOJI
                                    out_r = tex_r;
                                    out_g = tex_g;
                                    out_b = tex_b;
                                    out_a = tex_a;
                                } else if has_color == 4.0 {
                                    // IS_GRAY_SCALE
                                    out_r = fg_r;
                                    out_g = fg_g;
                                    out_b = fg_b;
                                    out_a = fg_a * tex_a;
                                } else {
                                    // IS_GLYPH (0.0) — coverage mask
                                    out_r = fg_r;
                                    out_g = fg_g;
                                    out_b = fg_b;
                                    out_a = tex_a;

                                    if apply_hsv {
                                        let (h, s, v) = rgb_to_hsv(out_r, out_g, out_b);
                                        let (nr, ng, nb) = hsv_to_rgb(
                                            h * foreground_text_hsb.hue,
                                            s * foreground_text_hsb.saturation,
                                            v * foreground_text_hsb.brightness,
                                        );
                                        out_r = nr;
                                        out_g = ng;
                                        out_b = nb;
                                    }
                                }

                                // Color space handling:
                                // Texture-sourced colors (color emoji, bg image) are
                                // already sRGB in the ImageTexture atlas.
                                // Vertex-sourced colors (glyph, grayscale) are linear RGB.
                                let tex_is_srgb = has_color == 1.0 || has_color == 2.0;

                                // Per-vertex HSV (must operate in linear space)
                                if hsv[0] != 1.0 || hsv[1] != 1.0 || hsv[2] != 1.0 {
                                    if tex_is_srgb {
                                        out_r = srgb_to_linear(out_r);
                                        out_g = srgb_to_linear(out_g);
                                        out_b = srgb_to_linear(out_b);
                                    }
                                    let (h, s, v) = rgb_to_hsv(out_r, out_g, out_b);
                                    let (nr, ng, nb) =
                                        hsv_to_rgb(h * hsv[0], s * hsv[1], v * hsv[2]);
                                    out_r = nr;
                                    out_g = ng;
                                    out_b = nb;
                                    // After HSV in linear space, convert to sRGB
                                    out_r = linear_to_srgb(out_r);
                                    out_g = linear_to_srgb(out_g);
                                    out_b = linear_to_srgb(out_b);
                                } else if !tex_is_srgb {
                                    // Vertex colors are linear, convert to sRGB
                                    out_r = linear_to_srgb(out_r);
                                    out_g = linear_to_srgb(out_g);
                                    out_b = linear_to_srgb(out_b);
                                }
                                // tex_is_srgb with no HSV → already sRGB, no conversion

                                if out_a <= 0.0 {
                                    continue;
                                }

                                let fi = (fb_row_off + dx as usize) * 4;
                                let sb = (out_b.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                                let sg = (out_g.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                                let sr = (out_r.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                                let sa = (out_a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                                blend_over(&mut state.frame_buffer, fi, sr, sg, sb, sa);
                            }
                        }
                    }
                }

                vb.next_index();
            }
        }

        let blit_elapsed = blit_start.elapsed();
        metrics::histogram!("purecpu.blit").record(blit_elapsed);
        metrics::histogram!("purecpu.quads_total").record(quads_total as f64);
        metrics::histogram!("purecpu.quads_blitted").record(quads_blitted as f64);

        // Present the frame
        let state = self.purecpu_state.as_mut().unwrap();
        let window = self.window.as_ref().unwrap();
        if full_repaint {
            window.present_software_frame_region(
                &state.frame_buffer,
                state.width,
                state.height,
                0,
                0,
            )?;
        } else {
            // Coalesce dirty rects into horizontal bands for XPutImage.
            // Since bands span full width, the pixel data is contiguous in the framebuffer.
            let bands = coalesce_to_bands(&effective_dirty, screen_width);
            for band in &bands {
                let y = band.y.max(0) as usize;
                let h = band.height as usize;
                if y + h > screen_height as usize {
                    continue;
                }
                let offset = y * fb_w * 4;
                let size = h * fb_w * 4;
                if offset + size <= state.frame_buffer.len() {
                    window.present_software_frame_region(
                        &state.frame_buffer[offset..offset + size],
                        screen_width,
                        h as u32,
                        0,
                        band.y as i16,
                    )?;
                }
            }
        }

        Ok(())
    }
}

/// Alpha-blend a source pixel (sRGB, non-premultiplied) over the framebuffer.
/// fb is BGRA layout.
#[inline]
fn blend_over(fb: &mut [u8], fi: usize, sr: u8, sg: u8, sb: u8, sa: u8) {
    if sa == 255 {
        fb[fi] = sb;
        fb[fi + 1] = sg;
        fb[fi + 2] = sr;
        fb[fi + 3] = 255;
    } else if sa > 0 {
        let sa_f = sa as f32 / 255.0;
        let inv = 1.0 - sa_f;
        fb[fi] = (sb as f32 * sa_f + fb[fi] as f32 * inv + 0.5) as u8;
        fb[fi + 1] = (sg as f32 * sa_f + fb[fi + 1] as f32 * inv + 0.5) as u8;
        fb[fi + 2] = (sr as f32 * sa_f + fb[fi + 2] as f32 * inv + 0.5) as u8;
        fb[fi + 3] = ((sa as f32 + fb[fi + 3] as f32 * inv).min(255.0) + 0.5) as u8;
    }
}

#[inline]
fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

#[inline]
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max == 0.0 { 0.0 } else { d / max };
    let v = max;
    let h = if d < 1.0e-10 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, v)
}

#[inline]
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h.fract() * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 1.0 {
        (c, x, 0.0)
    } else if h < 2.0 {
        (x, c, 0.0)
    } else if h < 3.0 {
        (0.0, c, x)
    } else if h < 4.0 {
        (0.0, x, c)
    } else if h < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    ((r + m).clamp(0.0, 1.0), (g + m).clamp(0.0, 1.0), (b + m).clamp(0.0, 1.0))
}

#[cfg(test)]
mod test {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn srgb_linear_roundtrip() {
        for &v in &[0.0_f32, 0.01, 0.04045, 0.5, 0.75, 1.0] {
            let rt = linear_to_srgb(srgb_to_linear(v));
            assert!(approx_eq(rt, v), "roundtrip failed for {v}: got {rt}");
        }
    }

    #[test]
    fn srgb_linear_boundary() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(linear_to_srgb(0.0), 0.0);
        assert!(approx_eq(srgb_to_linear(1.0), 1.0));
        assert!(approx_eq(linear_to_srgb(1.0), 1.0));
    }

    #[test]
    fn hsv_roundtrip() {
        let colors: &[(f32, f32, f32)] = &[
            (1.0, 0.0, 0.0), // red
            (0.0, 1.0, 0.0), // green
            (0.0, 0.0, 1.0), // blue
            (1.0, 1.0, 1.0), // white
            (0.5, 0.5, 0.5), // gray
            (0.0, 0.0, 0.0), // black
            (0.8, 0.2, 0.5), // arbitrary
        ];
        for &(r, g, b) in colors {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!(
                approx_eq(r, r2) && approx_eq(g, g2) && approx_eq(b, b2),
                "HSV roundtrip failed for ({r},{g},{b}): got ({r2},{g2},{b2})"
            );
        }
    }

    #[test]
    fn blend_over_opaque() {
        let mut fb = vec![0u8; 8]; // two pixels
        // Fully opaque red over black
        blend_over(&mut fb, 0, 255, 0, 0, 255);
        assert_eq!(fb[0], 0); // B
        assert_eq!(fb[1], 0); // G
        assert_eq!(fb[2], 255); // R
        assert_eq!(fb[3], 255); // A
    }

    #[test]
    fn blend_over_transparent() {
        let mut fb = vec![100, 150, 200, 255]; // existing pixel
        // Fully transparent: no change
        blend_over(&mut fb, 0, 0, 0, 0, 0);
        assert_eq!(fb, [100, 150, 200, 255]);
    }

    #[test]
    fn blend_over_semi() {
        let mut fb = vec![0, 0, 0, 255]; // black background
        // 50% white over black → ~128
        blend_over(&mut fb, 0, 128, 128, 128, 128);
        // sa_f = 128/255 ≈ 0.502
        // each channel: 128 * 0.502 + 0 * 0.498 ≈ 64
        assert!(fb[0] > 60 && fb[0] < 68);
        assert!(fb[1] > 60 && fb[1] < 68);
        assert!(fb[2] > 60 && fb[2] < 68);
    }

    #[test]
    fn coalesce_to_bands_merges_overlapping() {
        let rects = vec![
            DirtyRect { x: 0, y: 10, width: 100, height: 20 },
            DirtyRect { x: 0, y: 25, width: 100, height: 20 },
        ];
        let bands = coalesce_to_bands(&rects, 100);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].y, 10);
        assert_eq!(bands[0].height, 35); // 10..45
    }

    #[test]
    fn coalesce_to_bands_keeps_disjoint() {
        let rects = vec![
            DirtyRect { x: 0, y: 0, width: 100, height: 10 },
            DirtyRect { x: 0, y: 50, width: 100, height: 10 },
        ];
        let bands = coalesce_to_bands(&rects, 100);
        assert_eq!(bands.len(), 2);
    }

    #[test]
    fn coalesce_to_bands_empty() {
        let bands = coalesce_to_bands(&[], 100);
        assert!(bands.is_empty());
    }

    #[test]
    fn collect_clip_rects_intersection() {
        let rects = vec![DirtyRect { x: 10, y: 10, width: 20, height: 20 }];
        let mut out = Vec::new();
        // Quad fully inside dirty rect
        collect_clip_rects(12, 12, 25, 25, &rects, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], [12, 12, 25, 25]);

        // Quad partially overlapping
        out.clear();
        collect_clip_rects(0, 0, 15, 15, &rects, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], [10, 10, 15, 15]);

        // Quad not overlapping
        out.clear();
        collect_clip_rects(0, 0, 5, 5, &rects, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn clear_rect_zeroes_region() {
        let mut fb = vec![255u8; 4 * 4 * 4]; // 4x4 pixel framebuffer, all white
        let rect = DirtyRect { x: 1, y: 1, width: 2, height: 2 };
        clear_rect(&mut fb, 4, 4, &rect);
        // Check that pixel (1,1) is zero
        let idx = (1 * 4 + 1) * 4;
        assert_eq!(fb[idx..idx + 4], [0, 0, 0, 0]);
        // Check that pixel (0,0) is still white
        assert_eq!(fb[0..4], [255, 255, 255, 255]);
    }
}
