use crate::quad::{Vertex, VERTICES_PER_CELL};
use crate::renderstate::VertexBuffer;
use ::window::bitmaps::{BitmapImage, ImageTexture};
use ::window::WindowOps;

pub struct PureCpuState {
    pub frame_buffer: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl PureCpuState {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            frame_buffer: vec![0u8; (width * height * 4) as usize],
            width,
            height,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.frame_buffer.resize((width * height * 4) as usize, 0);
        }
    }
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

        // Clear frame buffer to black
        state.frame_buffer.fill(0);

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
                for q in 0..num_quads {
                    let base = q * VERTICES_PER_CELL;
                    let tl = &vertices[base];     // top-left
                    let tr = &vertices[base + 1]; // top-right
                    let bl = &vertices[base + 2]; // bot-left
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

                    // Atlas pixel rect from normalized tex coords
                    let tex_px_x = (tl.tex[0] * atlas_w as f32) as i32;
                    let tex_px_y = (tl.tex[1] * atlas_h as f32) as i32;
                    let tex_px_x2 = (br.tex[0] * atlas_w as f32) as i32;
                    let tex_px_y2 = (br.tex[1] * atlas_h as f32) as i32;
                    let tex_w = tex_px_x2 - tex_px_x;
                    let tex_h = tex_px_y2 - tex_px_y;

                    if has_color == 3.0 {
                        // IS_SOLID_COLOR: fill the dest rect with fg color, no texture
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

                        for py in 0..dest_h {
                            let dy = dest_y + py;
                            if dy < 0 || dy >= fb_h as i32 {
                                continue;
                            }
                            let row_off = dy as usize * fb_w;
                            for px in 0..dest_w {
                                let dx = dest_x + px;
                                if dx < 0 || dx >= fb_w as i32 {
                                    continue;
                                }
                                let fi = (row_off + dx as usize) * 4;
                                blend_over(&mut state.frame_buffer, fi, sr8, sg8, sb8, sa8);
                            }
                        }
                        continue;
                    }

                    // For textured quads: blit 1:1 from atlas to dest.
                    // Iterate over the texture pixel dimensions.
                    // The glyph is placed at dest_x, dest_y.
                    let blit_w = tex_w.min(dest_w);
                    let blit_h = tex_h.min(dest_h);

                    if blit_w <= 0 || blit_h <= 0 {
                        continue;
                    }

                    for row in 0..blit_h {
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

                        for col in 0..blit_w {
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

                vb.next_index();
            }
        }

        // Present the frame
        let window = self.window.as_ref().unwrap();
        window.present_software_frame_region(
            &state.frame_buffer,
            state.width,
            state.height,
            0,
            0,
        )?;

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
