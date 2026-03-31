//! Font rasterizer using skrifa + zeno directly.
//! Replaces swash-based rasterizer for glyph rendering, with full
//! hinting mode support via skrifa::outline::Target.

use crate::parser::ParsedFont;
use crate::rasterizer::paint_ops::{
    ColorLine, ColorStop, CompositeMode, DrawOp, ExtendMode, PaintOp, Transform as PaintTransform,
};
use crate::rasterizer::skia_colr::rasterize_paint_ops;
use crate::rasterizer::{FontRasterizer, FAKE_ITALIC_SKEW};
use crate::units::*;
use crate::RasterizedGlyph;
use config::{DisplayPixelGeometry, FreeTypeLoadFlags, FreeTypeLoadTarget};
use skrifa::color::{Brush, ColorPainter, CompositeMode as SkrifaCompositeMode, Extend};
use skrifa::instance::Size;
use skrifa::outline::{DrawSettings, HintingInstance, OutlinePen, SmoothMode, Target};
use skrifa::MetadataProvider;
use std::sync::Arc;
use wezterm_color_types::linear_u8_to_srgb8;

/// Adapter: collects skrifa outline pen commands into zeno commands.
struct ZenoPen(Vec<zeno::Command>);

impl OutlinePen for ZenoPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0
            .push(zeno::Command::MoveTo(zeno::Vector { x, y }));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0
            .push(zeno::Command::LineTo(zeno::Vector { x, y }));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.push(zeno::Command::QuadTo(
            zeno::Vector { x: cx0, y: cy0 },
            zeno::Vector { x, y },
        ));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(zeno::Command::CurveTo(
            zeno::Vector { x: cx0, y: cy0 },
            zeno::Vector { x: cx1, y: cy1 },
            zeno::Vector { x, y },
        ));
    }
    fn close(&mut self) {
        self.0.push(zeno::Command::Close);
    }
}

enum HintingConfig {
    None,
    Hinted(Target),
}

pub struct SkrifaRasterizer {
    font_data: Arc<Vec<u8>>,
    font_index: u32,
    synthesize_bold: bool,
    synthesize_italic: bool,
    display_pixel_geometry: DisplayPixelGeometry,
    scale: f64,
    hinting_config: HintingConfig,
    use_lcd_subpixel: bool,
}

impl SkrifaRasterizer {
    pub fn from_locator(
        parsed: &ParsedFont,
        display_pixel_geometry: DisplayPixelGeometry,
    ) -> anyhow::Result<Self> {
        log::trace!("SkrifaRasterizer wants {:?}", parsed);

        let data = parsed.handle.source.load_data()?;
        let font_data = Arc::new(data.into_owned());
        let font_index = parsed.handle.index;

        let (hinting_config, use_lcd_subpixel) =
            map_freetype_config(parsed.freetype_load_target, parsed.freetype_load_flags);

        Ok(Self {
            font_data,
            font_index,
            synthesize_bold: parsed.synthesize_bold,
            synthesize_italic: parsed.synthesize_italic,
            display_pixel_geometry,
            scale: parsed.scale.unwrap_or(1.0),
            hinting_config,
            use_lcd_subpixel,
        })
    }

    fn font_ref(&self) -> anyhow::Result<skrifa::FontRef<'_>> {
        skrifa::FontRef::from_index(&self.font_data, self.font_index)
            .map_err(|e| anyhow::anyhow!("failed to get font reference: {}", e))
    }
}

impl FontRasterizer for SkrifaRasterizer {
    fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        let font_ref = self.font_ref()?;
        let pixel_size = (size * self.scale * dpi as f64 / 72.0) as f32;
        let skrifa_size = Size::new(pixel_size);
        let glyph_id = skrifa::GlyphId::new(glyph_pos);
        let location = skrifa::instance::LocationRef::default();

        // Try COLR color rendering first (COLRv0/v1)
        if let Some(result) = self.render_colr(&font_ref, glyph_id, pixel_size) {
            return Ok(result);
        }

        // Try bitmap strikes (CBDT/sbix)
        if let Some(result) = self.render_bitmap(&font_ref, glyph_id, skrifa_size) {
            return Ok(result);
        }

        // Try outline rendering
        let outlines = font_ref.outline_glyphs();
        if let Some(outline_glyph) = outlines.get(glyph_id) {
            return self.render_outline(
                &font_ref,
                &outlines,
                &outline_glyph,
                skrifa_size,
                location,
                pixel_size,
            );
        }

        // No outline or bitmap found
        Ok(RasterizedGlyph {
            data: vec![],
            height: 0,
            width: 0,
            bearing_x: PixelLength::new(0.0),
            bearing_y: PixelLength::new(0.0),
            has_color: false,
            is_scaled: true,
        })
    }
}

impl SkrifaRasterizer {
    fn render_outline(
        &self,
        _font_ref: &skrifa::FontRef<'_>,
        outlines: &skrifa::outline::OutlineGlyphCollection<'_>,
        outline_glyph: &skrifa::outline::OutlineGlyph<'_>,
        skrifa_size: Size,
        location: skrifa::instance::LocationRef<'_>,
        _pixel_size: f32,
    ) -> anyhow::Result<RasterizedGlyph> {
        let mut pen = ZenoPen(Vec::new());

        // Draw with or without hinting
        let _adjusted = match &self.hinting_config {
            HintingConfig::Hinted(target) => {
                let hinting = HintingInstance::new(
                    outlines,
                    skrifa_size,
                    location,
                    *target,
                )
                .map_err(|e| anyhow::anyhow!("hinting instance error: {}", e))?;
                let settings = DrawSettings::hinted(&hinting, false);
                outline_glyph
                    .draw(settings, &mut pen)
                    .map_err(|e| anyhow::anyhow!("outline draw error: {}", e))?
            }
            HintingConfig::None => {
                let settings = DrawSettings::unhinted(skrifa_size, location);
                outline_glyph
                    .draw(settings, &mut pen)
                    .map_err(|e| anyhow::anyhow!("outline draw error: {}", e))?
            }
        };

        if pen.0.is_empty() {
            return Ok(RasterizedGlyph {
                data: vec![],
                height: 0,
                width: 0,
                bearing_x: PixelLength::new(0.0),
                bearing_y: PixelLength::new(0.0),
                has_color: false,
                is_scaled: true,
            });
        }

        // Apply synthetic italic via transform on commands
        let transform = if self.synthesize_italic {
            Some(zeno::Transform {
                xx: 1.0,
                yx: 0.0,
                xy: FAKE_ITALIC_SKEW as f32,
                yy: 1.0,
                x: 0.0,
                y: 0.0,
            })
        } else {
            None
        };

        let format = if self.use_lcd_subpixel {
            zeno::Format::Subpixel
        } else {
            zeno::Format::Alpha
        };

        let mut mask = zeno::Mask::new(&pen.0[..]);
        mask.format(format).origin(zeno::Origin::BottomLeft);
        if let Some(t) = transform {
            mask.transform(Some(t));
        }
        let (mut data, placement) = mask.render();

        let width = placement.width as usize;
        let height = placement.height as usize;
        let bearing_x = placement.left as f64;
        let bearing_y = (placement.top + placement.height as i32) as f64;

        if width == 0 || height == 0 {
            return Ok(RasterizedGlyph {
                data: vec![],
                height: 0,
                width: 0,
                bearing_x: PixelLength::new(0.0),
                bearing_y: PixelLength::new(0.0),
                has_color: false,
                is_scaled: true,
            });
        }

        // Apply synthetic bold via post-rasterization dilation
        if self.synthesize_bold {
            match format {
                zeno::Format::Alpha => embolden_alpha_mask(&mut data, width, height),
                zeno::Format::Subpixel | zeno::Format::CustomSubpixel(_) => {
                    embolden_subpixel_mask(&mut data, width, height)
                }
            }
        }

        // Convert mask to RGBA
        let (rgba, has_color) = if self.use_lcd_subpixel {
            let rgba =
                subpixel_mask_to_rgba(&data, width, height, &self.display_pixel_geometry);
            (rgba, false)
        } else {
            let rgba = alpha_mask_to_rgba(&data, width, height);
            (rgba, false)
        };

        Ok(RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(bearing_x),
            bearing_y: PixelLength::new(bearing_y),
            has_color,
            is_scaled: true,
        })
    }

    fn render_bitmap(
        &self,
        font_ref: &skrifa::FontRef<'_>,
        glyph_id: skrifa::GlyphId,
        size: Size,
    ) -> Option<RasterizedGlyph> {
        use skrifa::bitmap::BitmapData;

        let bitmap = font_ref.bitmap_strikes().glyph_for_size(size, glyph_id)?;
        let bmp_data = &bitmap.data;
        let width = bitmap.width as usize;
        let height = bitmap.height as usize;

        // skrifa splits bitmap bearings into outer (font units) and inner (pixels
        // at the strike's ppem).  For CBDT the outer values are always 0; the
        // actual placement offsets live in inner_bearing_*.
        let bearing_x = bitmap.inner_bearing_x as f64;
        let bearing_y = match bitmap.placement_origin {
            // TopLeft (CBDT/EBDT): inner_bearing_y is the distance from the
            // baseline to the top of the bitmap (positive = above baseline).
            skrifa::bitmap::Origin::TopLeft => bitmap.inner_bearing_y as f64,
            // BottomLeft (sbix): inner_bearing_y is a Y-up offset from the
            // glyph origin to the bottom of the image, so the top of the
            // bitmap is offset + height.
            skrifa::bitmap::Origin::BottomLeft => {
                (bitmap.inner_bearing_y + bitmap.height as f32) as f64
            }
        };

        if width == 0 || height == 0 {
            return None;
        }

        let (rgba, has_color) = match bmp_data {
            BitmapData::Png(png_data) => {
                let img = image::load_from_memory(png_data).ok()?;
                let rgba_img = img.into_rgba8();
                let actual_width = rgba_img.width() as usize;
                let actual_height = rgba_img.height() as usize;
                let data = rgba_img.into_raw();
                return Some(RasterizedGlyph {
                    data,
                    height: actual_height,
                    width: actual_width,
                    bearing_x: PixelLength::new(bearing_x),
                    bearing_y: PixelLength::new(bearing_y),
                    has_color: true,
                    is_scaled: false,
                });
            }
            BitmapData::Bgra(bgra_data) => {
                let mut rgba = vec![0u8; width * height * 4];
                for i in 0..(width * height) {
                    rgba[i * 4] = bgra_data[i * 4 + 2]; // R
                    rgba[i * 4 + 1] = bgra_data[i * 4 + 1]; // G
                    rgba[i * 4 + 2] = bgra_data[i * 4]; // B
                    rgba[i * 4 + 3] = bgra_data[i * 4 + 3]; // A
                }
                (rgba, true)
            }
            BitmapData::Mask(mask) => {
                // For 8-bit masks, use data directly. For other bit depths,
                // we'd need to unpack, but 8bpp is the common case.
                if mask.bpp == 8 {
                    let rgba = alpha_mask_to_rgba(mask.data, width, height);
                    (rgba, false)
                } else {
                    // Unsupported bit depth for now
                    return None;
                }
            }
        };

        Some(RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(bearing_x),
            bearing_y: PixelLength::new(bearing_y),
            has_color,
            is_scaled: false,
        })
    }

    fn render_colr(
        &self,
        font_ref: &skrifa::FontRef<'_>,
        glyph_id: skrifa::GlyphId,
        pixel_size: f32,
    ) -> Option<RasterizedGlyph> {
        let color_glyphs = font_ref.color_glyphs();
        let color_glyph = color_glyphs.get(glyph_id)?;

        let location = skrifa::instance::LocationRef::default();

        // Resolve the default palette colors from CPAL table
        use read_fonts::TableProvider;
        let cpal = font_ref.cpal().ok()?;
        let num_entries = cpal.num_palette_entries();
        let color_records = cpal.color_records_array()?.ok()?;
        let first_color_index = cpal.color_record_indices()[0].get() as usize;
        let colors: Vec<wezterm_color_types::SrgbaPixel> = (0..num_entries as usize)
            .map(|i| {
                let idx = first_color_index + i;
                if idx < color_records.len() {
                    let c = &color_records[idx];
                    wezterm_color_types::SrgbaPixel::rgba(c.red(), c.green(), c.blue(), c.alpha())
                } else {
                    wezterm_color_types::SrgbaPixel::rgba(0, 0, 0, 255)
                }
            })
            .collect();

        let upem = font_ref.head().ok()?.units_per_em() as f32;
        let scale = pixel_size / upem;

        let mut collector = PaintOpCollector {
            ops: Vec::new(),
            font_ref,
            palette_colors: &colors,
        };

        if color_glyph.paint(location, &mut collector).is_err() {
            return None;
        }

        if collector.ops.is_empty() {
            return None;
        }

        match rasterize_paint_ops(collector.ops, scale as f64, -(scale as f64)) {
            Ok(glyph) if glyph.width > 0 && glyph.height > 0 => Some(glyph),
            _ => None,
        }
    }
}

/// Bridges skrifa's ColorPainter trait to our PaintOp list for skia_colr rendering.
struct PaintOpCollector<'a> {
    ops: Vec<PaintOp>,
    font_ref: &'a skrifa::FontRef<'a>,
    palette_colors: &'a [wezterm_color_types::SrgbaPixel],
}

impl<'a> PaintOpCollector<'a> {
    fn resolve_color(&self, palette_index: u16, alpha: f32) -> wezterm_color_types::SrgbaPixel {
        let base = if (palette_index as usize) < self.palette_colors.len() {
            self.palette_colors[palette_index as usize]
        } else {
            wezterm_color_types::SrgbaPixel::rgba(0, 0, 0, 255)
        };
        let (r, g, b, a) = base.as_rgba();
        let a = (a as f32 * alpha) as u8;
        wezterm_color_types::SrgbaPixel::rgba(r, g, b, a)
    }

    fn convert_extend(extend: Extend) -> ExtendMode {
        match extend {
            Extend::Pad => ExtendMode::Pad,
            Extend::Repeat => ExtendMode::Repeat,
            Extend::Reflect => ExtendMode::Reflect,
            _ => ExtendMode::Pad,
        }
    }

    fn convert_color_stops(&self, stops: &[skrifa::color::ColorStop]) -> ColorLine {
        let color_stops = stops
            .iter()
            .map(|s| {
                let color = self.resolve_color(s.palette_index, s.alpha);
                let (r, g, b, a) = color.as_rgba();
                ColorStop {
                    offset: s.offset as f64,
                    color: wezterm_color_types::SrgbaPixel::rgba(r, g, b, a),
                }
            })
            .collect();
        ColorLine {
            color_stops,
            extend: ExtendMode::Pad,
        }
    }

    fn glyph_outline_draw_ops(&self, glyph_id: skrifa::GlyphId) -> Vec<DrawOp> {
        let mut ops = Vec::new();
        let outlines = self.font_ref.outline_glyphs();
        if let Some(outline_glyph) = outlines.get(glyph_id) {
            let mut pen = DrawOpPen(Vec::new());
            let settings =
                DrawSettings::unhinted(Size::unscaled(), skrifa::instance::LocationRef::default());
            let _ = outline_glyph.draw(settings, &mut pen);
            ops = pen.0;
        }
        ops
    }
}

/// Pen that collects outline commands as DrawOps (for COLR clip paths).
struct DrawOpPen(Vec<DrawOp>);

impl OutlinePen for DrawOpPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(DrawOp::MoveTo { to_x: x, to_y: y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(DrawOp::LineTo { to_x: x, to_y: y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(DrawOp::QuadTo {
            control_x: cx,
            control_y: cy,
            to_x: x,
            to_y: y,
        });
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(DrawOp::CubicTo {
            control1_x: cx0,
            control1_y: cy0,
            control2_x: cx1,
            control2_y: cy1,
            to_x: x,
            to_y: y,
        });
    }
    fn close(&mut self) {
        self.0.push(DrawOp::ClosePath);
    }
}

fn convert_composite_mode(mode: SkrifaCompositeMode) -> CompositeMode {
    match mode {
        SkrifaCompositeMode::Clear => CompositeMode::Clear,
        SkrifaCompositeMode::Src => CompositeMode::Source,
        SkrifaCompositeMode::Dest => CompositeMode::Dest,
        SkrifaCompositeMode::SrcOver => CompositeMode::Over,
        SkrifaCompositeMode::DestOver => CompositeMode::DestOver,
        SkrifaCompositeMode::SrcIn => CompositeMode::In,
        SkrifaCompositeMode::DestIn => CompositeMode::DestIn,
        SkrifaCompositeMode::SrcOut => CompositeMode::Out,
        SkrifaCompositeMode::DestOut => CompositeMode::DestOut,
        SkrifaCompositeMode::SrcAtop => CompositeMode::Atop,
        SkrifaCompositeMode::DestAtop => CompositeMode::DestAtop,
        SkrifaCompositeMode::Xor => CompositeMode::Xor,
        SkrifaCompositeMode::Plus => CompositeMode::Add,
        SkrifaCompositeMode::Screen => CompositeMode::Screen,
        SkrifaCompositeMode::Overlay => CompositeMode::Overlay,
        SkrifaCompositeMode::Darken => CompositeMode::Darken,
        SkrifaCompositeMode::Lighten => CompositeMode::Lighten,
        SkrifaCompositeMode::ColorDodge => CompositeMode::ColorDodge,
        SkrifaCompositeMode::ColorBurn => CompositeMode::ColorBurn,
        SkrifaCompositeMode::HardLight => CompositeMode::HardLight,
        SkrifaCompositeMode::SoftLight => CompositeMode::SoftLight,
        SkrifaCompositeMode::Difference => CompositeMode::Difference,
        SkrifaCompositeMode::Exclusion => CompositeMode::Exclusion,
        SkrifaCompositeMode::Multiply => CompositeMode::Multiply,
        SkrifaCompositeMode::HslHue => CompositeMode::HslHue,
        SkrifaCompositeMode::HslSaturation => CompositeMode::HslSaturation,
        SkrifaCompositeMode::HslColor => CompositeMode::HslColor,
        SkrifaCompositeMode::HslLuminosity => CompositeMode::HslLuminosity,
        _ => CompositeMode::Over,
    }
}

impl<'a> ColorPainter for PaintOpCollector<'a> {
    fn push_transform(&mut self, transform: skrifa::color::Transform) {
        self.ops.push(PaintOp::PushTransform(PaintTransform {
            xx: transform.xx as f64,
            yx: transform.yx as f64,
            xy: transform.xy as f64,
            yy: transform.yy as f64,
            x0: transform.dx as f64,
            y0: transform.dy as f64,
        }));
    }

    fn pop_transform(&mut self) {
        self.ops.push(PaintOp::PopTransform);
    }

    fn push_clip_glyph(&mut self, glyph_id: skrifa::GlyphId) {
        let draw_ops = self.glyph_outline_draw_ops(glyph_id);
        self.ops.push(PaintOp::PushClip(draw_ops));
    }

    fn push_clip_box(&mut self, clip_box: read_fonts::types::BoundingBox<f32>) {
        self.ops.push(PaintOp::PushRectClip {
            xmin: clip_box.x_min,
            ymin: clip_box.y_min,
            xmax: clip_box.x_max,
            ymax: clip_box.y_max,
        });
    }

    fn pop_clip(&mut self) {
        self.ops.push(PaintOp::PopClip);
    }

    fn fill(&mut self, brush: Brush<'_>) {
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => {
                let color = self.resolve_color(palette_index, alpha);
                self.ops.push(PaintOp::PaintSolid(color));
            }
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                let mut cl = self.convert_color_stops(color_stops);
                cl.extend = Self::convert_extend(extend);
                self.ops.push(PaintOp::PaintLinearGradient {
                    x0: p0.x,
                    y0: p0.y,
                    x1: p1.x,
                    y1: p1.y,
                    x2: p0.x,
                    y2: p0.y,
                    color_line: cl,
                });
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                let mut cl = self.convert_color_stops(color_stops);
                cl.extend = Self::convert_extend(extend);
                self.ops.push(PaintOp::PaintRadialGradient {
                    x0: c0.x,
                    y0: c0.y,
                    r0,
                    x1: c1.x,
                    y1: c1.y,
                    r1,
                    color_line: cl,
                });
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                let mut cl = self.convert_color_stops(color_stops);
                cl.extend = Self::convert_extend(extend);
                self.ops.push(PaintOp::PaintSweepGradient {
                    x0: c0.x,
                    y0: c0.y,
                    start_angle,
                    end_angle,
                    color_line: cl,
                });
            }
        }
    }

    fn push_layer(&mut self, composite_mode: SkrifaCompositeMode) {
        self.ops.push(PaintOp::PushGroup);
        // Store mode for pop_layer_with_mode
        self.ops
            .push(PaintOp::PopGroup(convert_composite_mode(composite_mode)));
        // Actually, we need PushGroup now and PopGroup later.
        // Remove the PopGroup we just added; it will be added by pop_layer_with_mode.
        self.ops.pop();
    }

    fn pop_layer(&mut self) {
        self.ops.push(PaintOp::PopGroup(CompositeMode::Over));
    }

    fn pop_layer_with_mode(&mut self, composite_mode: SkrifaCompositeMode) {
        self.ops
            .push(PaintOp::PopGroup(convert_composite_mode(composite_mode)));
    }
}

/// Convert 8-bit alpha mask to premultiplied RGBA 32bpp.
fn alpha_mask_to_rgba(alpha_data: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let linear_alpha = alpha_data[y * width + x];
            let srgb_gray = linear_u8_to_srgb8(linear_alpha);
            let dest = (y * width + x) * 4;
            rgba[dest] = srgb_gray;
            rgba[dest + 1] = srgb_gray;
            rgba[dest + 2] = srgb_gray;
            rgba[dest + 3] = linear_alpha;
        }
    }
    rgba
}

/// Convert 32-bit RGBA subpixel mask to the format expected by GPU shaders.
fn subpixel_mask_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    pixel_geometry: &DisplayPixelGeometry,
) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for i in 0..(width * height) {
        let src = i * 4;
        let dst = i * 4;
        let r = data[src];
        let g = data[src + 1];
        let b = data[src + 2];

        let linear_alpha = r.max(g).max(b);

        let srgb_r = linear_u8_to_srgb8(r);
        let srgb_g = linear_u8_to_srgb8(g);
        let srgb_b = linear_u8_to_srgb8(b);

        let (red, blue) = match pixel_geometry {
            DisplayPixelGeometry::RGB => (srgb_r, srgb_b),
            DisplayPixelGeometry::BGR => (srgb_b, srgb_r),
        };

        rgba[dst] = red;
        rgba[dst + 1] = srgb_g;
        rgba[dst + 2] = blue;
        rgba[dst + 3] = linear_alpha;
    }
    rgba
}

/// Simple emboldening for alpha masks: shift right by 1px and max-composite.
fn embolden_alpha_mask(data: &mut Vec<u8>, width: usize, height: usize) {
    if width < 2 {
        return;
    }
    let original = data.clone();
    for y in 0..height {
        for x in 1..width {
            let idx = y * width + x;
            let left = y * width + x - 1;
            data[idx] = data[idx].max(original[left]);
        }
    }
}

/// Simple emboldening for subpixel (RGBA) masks.
fn embolden_subpixel_mask(data: &mut Vec<u8>, width: usize, height: usize) {
    if width < 2 {
        return;
    }
    let original = data.clone();
    for y in 0..height {
        for x in 1..width {
            let idx = (y * width + x) * 4;
            let left = (y * width + x - 1) * 4;
            for c in 0..4 {
                data[idx + c] = data[idx + c].max(original[left + c]);
            }
        }
    }
}

/// Map FreeType config options to skrifa hinting target + LCD subpixel flag.
fn map_freetype_config(
    load_target: Option<FreeTypeLoadTarget>,
    load_flags: Option<FreeTypeLoadFlags>,
) -> (HintingConfig, bool) {
    let no_hinting = load_flags
        .map(|f| f.contains(FreeTypeLoadFlags::NO_HINTING))
        .unwrap_or(false);

    if no_hinting {
        return (HintingConfig::None, false);
    }

    match load_target {
        Some(FreeTypeLoadTarget::Normal) | None => (
            HintingConfig::Hinted(Target::Smooth {
                mode: SmoothMode::Normal,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            }),
            false,
        ),
        Some(FreeTypeLoadTarget::Light) => (
            HintingConfig::Hinted(Target::Smooth {
                mode: SmoothMode::Light,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            }),
            false,
        ),
        Some(FreeTypeLoadTarget::Mono) => (HintingConfig::Hinted(Target::Mono), false),
        Some(FreeTypeLoadTarget::HorizontalLcd) => (
            HintingConfig::Hinted(Target::Smooth {
                mode: SmoothMode::Lcd,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            }),
            true,
        ),
        Some(FreeTypeLoadTarget::VerticalLcd) => (
            HintingConfig::Hinted(Target::Smooth {
                mode: SmoothMode::VerticalLcd,
                symmetric_rendering: true,
                preserve_linear_metrics: false,
            }),
            true,
        ),
    }
}
