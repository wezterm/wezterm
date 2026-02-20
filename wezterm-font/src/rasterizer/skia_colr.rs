//! Rasterize COLR paint operations using tiny-skia, replacing Cairo.
//!
//! This module takes a list of PaintOps (collected during COLR table walking)
//! and rasterizes them into a premultiplied RGBA bitmap suitable for the
//! GPU texture atlas.

use crate::rasterizer::paint_ops::{draw_ops_to_path, ColorLine, ColorStop, ImageExtents, PaintOp};
use crate::rasterizer::RasterizedGlyph;
use crate::units::PixelLength;
use wezterm_color_types::SrgbaTuple;

/// Rasterize a list of paint operations into a RasterizedGlyph.
/// scale_x and scale_y are applied to the initial transform (typically
/// to convert from font units to pixels; scale_y is usually negative).
pub fn rasterize_paint_ops(
    ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
) -> anyhow::Result<RasterizedGlyph> {
    // Start with a generous initial size and we'll crop
    let initial_size = 512u32;
    let Some(mut pixmap) = tiny_skia::Pixmap::new(initial_size, initial_size) else {
        return Ok(empty_glyph());
    };

    let origin_x = (initial_size / 2) as f32;
    let origin_y = (initial_size / 2) as f32;

    let base_transform = tiny_skia::Transform::from_row(
        scale_x as f32,
        0.0,
        0.0,
        scale_y as f32,
        origin_x,
        origin_y,
    );

    let has_color = render_ops_to_pixmap(&mut pixmap, &ops, base_transform)?;

    // Find ink bounds
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let (min_x, min_y, max_x, max_y) = find_ink_bounds(pixmap.data(), width, height);

    if min_x > max_x || min_y > max_y {
        return Ok(empty_glyph());
    }

    let crop_x = min_x as u32;
    let crop_y = min_y as u32;
    let crop_w = (max_x - min_x + 1) as u32;
    let crop_h = (max_y - min_y + 1) as u32;

    // Extract cropped region as RGBA
    let mut data = Vec::with_capacity((crop_w * crop_h * 4) as usize);
    let src = pixmap.data();
    let stride = width * 4;
    for y in crop_y..crop_y + crop_h {
        let row_start = (y as usize * stride + crop_x as usize * 4) as usize;
        let row_end = row_start + (crop_w * 4) as usize;
        data.extend_from_slice(&src[row_start..row_end]);
    }

    let bearing_x = crop_x as f64 - origin_x as f64;
    let bearing_y = -(crop_y as f64 - origin_y as f64);

    Ok(RasterizedGlyph {
        data,
        height: crop_h as usize,
        width: crop_w as usize,
        bearing_x: PixelLength::new(bearing_x),
        bearing_y: PixelLength::new(bearing_y),
        has_color,
        is_scaled: true,
    })
}

fn empty_glyph() -> RasterizedGlyph {
    RasterizedGlyph {
        data: vec![],
        height: 0,
        width: 0,
        bearing_x: PixelLength::new(0.),
        bearing_y: PixelLength::new(0.),
        has_color: false,
        is_scaled: true,
    }
}

/// Find the bounding box of non-transparent pixels in RGBA data.
fn find_ink_bounds(data: &[u8], width: usize, height: usize) -> (usize, usize, usize, usize) {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0usize;
    let mut max_y = 0usize;

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            let a = data[idx + 3];
            if a > 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    (min_x, min_y, max_x, max_y)
}

/// Render paint ops into a pixmap. Returns whether color was used.
fn render_ops_to_pixmap(
    pixmap: &mut tiny_skia::Pixmap,
    ops: &[PaintOp],
    base_transform: tiny_skia::Transform,
) -> anyhow::Result<bool> {
    let mut has_color = false;
    let mut transform_stack: Vec<tiny_skia::Transform> = vec![base_transform];
    let mut group_stack: Vec<tiny_skia::Pixmap> = vec![];

    // tiny-skia 0.11 removed ClipMask; we use a mask Pixmap for clipping instead
    let mut clip_mask_stack: Vec<Option<tiny_skia::Mask>> = vec![];

    for op in ops {
        match op {
            PaintOp::PushTransform(t) => {
                transform_stack.push(t.to_tiny_skia());
            }
            PaintOp::PopTransform => {
                if transform_stack.len() > 1 {
                    transform_stack.pop();
                }
            }
            PaintOp::PushClip(draw_ops) => {
                let path = draw_ops_to_path(draw_ops);
                let ts = compute_transform(&transform_stack);
                let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())
                    .ok_or_else(|| anyhow::anyhow!("failed to create clip mask"))?;
                mask.fill_path(&path, tiny_skia::FillRule::Winding, true, ts);
                clip_mask_stack.push(Some(mask));
            }
            PaintOp::PushRectClip {
                xmin,
                ymin,
                xmax,
                ymax,
            } => {
                let mut pb = tiny_skia::PathBuilder::new();
                if let Some(rect) =
                    tiny_skia::Rect::from_ltrb(*xmin, *ymin, *xmax, *ymax)
                {
                    pb.push_rect(rect);
                }
                if let Some(path) = pb.finish() {
                    let ts = compute_transform(&transform_stack);
                    let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())
                        .ok_or_else(|| anyhow::anyhow!("failed to create clip mask"))?;
                    mask.fill_path(&path, tiny_skia::FillRule::Winding, true, ts);
                    clip_mask_stack.push(Some(mask));
                } else {
                    clip_mask_stack.push(None);
                }
            }
            PaintOp::PopClip => {
                clip_mask_stack.pop();
            }
            PaintOp::PushGroup => {
                let saved = pixmap.clone();
                pixmap.fill(tiny_skia::Color::TRANSPARENT);
                group_stack.push(saved);
            }
            PaintOp::PopGroup(mode) => {
                if let Some(backdrop) = group_stack.pop() {
                    let group_content = pixmap.clone();
                    pixmap.data_mut().copy_from_slice(backdrop.data());
                    let paint = tiny_skia::PixmapPaint {
                        blend_mode: mode.to_tiny_skia(),
                        opacity: 1.0,
                        quality: tiny_skia::FilterQuality::Bilinear,
                    };
                    pixmap.draw_pixmap(
                        0,
                        0,
                        group_content.as_ref(),
                        &paint,
                        tiny_skia::Transform::identity(),
                        None,
                    );
                }
            }
            PaintOp::PaintSolid(color) => {
                if color.as_srgba32() != 0xffffffff {
                    has_color = true;
                }
                let (r, g, b, a) = color.as_srgba_tuple();
                let ts = compute_transform(&transform_stack);
                let mask = get_mask(&clip_mask_stack);

                if let Some(sk_color) =
                    tiny_skia::Color::from_rgba(r as f32, g as f32, b as f32, a as f32)
                {
                    let paint = tiny_skia::Paint {
                        shader: tiny_skia::Shader::SolidColor(sk_color),
                        blend_mode: tiny_skia::BlendMode::SourceOver,
                        anti_alias: true,
                        force_hq_pipeline: false,
                    };

                    let w = pixmap.width() as f32;
                    let h = pixmap.height() as f32;
                    if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
                        pixmap.fill_rect(rect, &paint, ts, mask);
                    }
                }
            }
            PaintOp::PaintLinearGradient {
                x0,
                y0,
                x1,
                y1,
                x2,
                y2,
                color_line,
            } => {
                has_color = true;
                paint_linear_gradient(
                    pixmap,
                    &transform_stack,
                    &clip_mask_stack,
                    color_line,
                    *x0,
                    *y0,
                    *x1,
                    *y1,
                    *x2,
                    *y2,
                )?;
            }
            PaintOp::PaintRadialGradient {
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                color_line,
            } => {
                has_color = true;
                paint_radial_gradient(
                    pixmap,
                    &transform_stack,
                    &clip_mask_stack,
                    color_line,
                    *x0,
                    *y0,
                    *r0,
                    *x1,
                    *y1,
                    *r1,
                )?;
            }
            PaintOp::PaintSweepGradient {
                x0,
                y0,
                start_angle,
                end_angle,
                color_line,
            } => {
                has_color = true;
                paint_sweep_gradient(
                    pixmap,
                    &transform_stack,
                    &clip_mask_stack,
                    color_line,
                    *x0,
                    *y0,
                    *start_angle,
                    *end_angle,
                )?;
            }
            PaintOp::PaintImage {
                data: img_data,
                width: _,
                height: _,
                is_png,
                slant,
                extents,
            } => {
                if *is_png {
                    has_color = true;
                    paint_image(
                        pixmap,
                        &transform_stack,
                        &clip_mask_stack,
                        img_data,
                        *slant,
                        extents.as_ref(),
                    )?;
                } else {
                    log::warn!("skia_colr: unsupported image format (non-PNG)");
                }
            }
        }
    }

    Ok(has_color)
}

fn compute_transform(stack: &[tiny_skia::Transform]) -> tiny_skia::Transform {
    let mut result = tiny_skia::Transform::identity();
    for t in stack {
        result = result.post_concat(*t);
    }
    result
}

fn get_mask<'a>(stack: &'a [Option<tiny_skia::Mask>]) -> Option<&'a tiny_skia::Mask> {
    stack.last().and_then(|c| c.as_ref())
}

fn normalize_color_line(color_line: &ColorLine) -> (f64, f64, Vec<(f32, tiny_skia::Color)>) {
    let mut stops = color_line.color_stops.clone();
    stops.sort_by(|a, b| a.offset.partial_cmp(&b.offset).unwrap());

    let smallest = stops.first().map(|s| s.offset).unwrap_or(0.0);
    let largest = stops.last().map(|s| s.offset).unwrap_or(1.0);

    let range = largest - smallest;
    let colors: Vec<(f32, tiny_skia::Color)> = stops
        .iter()
        .map(|s| {
            let offset = if range > 0.0001 {
                ((s.offset - smallest) / range) as f32
            } else {
                0.0
            };
            let (r, g, b, a) = s.color.as_srgba_tuple();
            let color = tiny_skia::Color::from_rgba(r as f32, g as f32, b as f32, a as f32)
                .unwrap_or(tiny_skia::Color::TRANSPARENT);
            (offset, color)
        })
        .collect();

    (smallest, largest, colors)
}

fn paint_linear_gradient(
    pixmap: &mut tiny_skia::Pixmap,
    transform_stack: &[tiny_skia::Transform],
    clip_stack: &[Option<tiny_skia::Mask>],
    color_line: &ColorLine,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) -> anyhow::Result<()> {
    let (min_stop, max_stop, colors) = normalize_color_line(color_line);

    let (ax0, ay0, ax1, ay1) = reduce_anchors(
        x0 as f64, y0 as f64, x1 as f64, y1 as f64, x2 as f64, y2 as f64,
    );

    let start_x = (ax0 + min_stop * (ax1 - ax0)) as f32;
    let start_y = (ay0 + min_stop * (ay1 - ay0)) as f32;
    let end_x = (ax0 + max_stop * (ax1 - ax0)) as f32;
    let end_y = (ay0 + max_stop * (ay1 - ay0)) as f32;

    let stops: Vec<tiny_skia::GradientStop> = colors
        .iter()
        .map(|(offset, color)| tiny_skia::GradientStop::new(*offset, *color))
        .collect();

    if let Some(shader) = tiny_skia::LinearGradient::new(
        tiny_skia::Point::from_xy(start_x, start_y),
        tiny_skia::Point::from_xy(end_x, end_y),
        stops,
        color_line.extend.to_tiny_skia(),
        tiny_skia::Transform::identity(),
    ) {
        let paint = tiny_skia::Paint {
            shader,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            anti_alias: true,
            force_hq_pipeline: false,
        };

        let ts = compute_transform(transform_stack);
        let mask = get_mask(clip_stack);
        let w = pixmap.width() as f32;
        let h = pixmap.height() as f32;
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
            pixmap.fill_rect(rect, &paint, ts, mask);
        }
    }

    Ok(())
}

fn paint_radial_gradient(
    pixmap: &mut tiny_skia::Pixmap,
    transform_stack: &[tiny_skia::Transform],
    clip_stack: &[Option<tiny_skia::Mask>],
    color_line: &ColorLine,
    x0: f32,
    y0: f32,
    _r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
) -> anyhow::Result<()> {
    let (min_stop, max_stop, colors) = normalize_color_line(color_line);

    let xx0 = x0 as f64 + min_stop * (x1 as f64 - x0 as f64);
    let yy0 = y0 as f64 + min_stop * (y1 as f64 - y0 as f64);
    let xx1 = x0 as f64 + max_stop * (x1 as f64 - x0 as f64);
    let yy1 = y0 as f64 + max_stop * (y1 as f64 - y0 as f64);
    let rr1 = _r0 as f64 + max_stop * (r1 as f64 - _r0 as f64);

    let stops: Vec<tiny_skia::GradientStop> = colors
        .iter()
        .map(|(offset, color)| tiny_skia::GradientStop::new(*offset, *color))
        .collect();

    if let Some(shader) = tiny_skia::RadialGradient::new(
        tiny_skia::Point::from_xy(xx0 as f32, yy0 as f32),
        tiny_skia::Point::from_xy(xx1 as f32, yy1 as f32),
        rr1 as f32,
        stops,
        color_line.extend.to_tiny_skia(),
        tiny_skia::Transform::identity(),
    ) {
        let paint = tiny_skia::Paint {
            shader,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            anti_alias: true,
            force_hq_pipeline: false,
        };

        let ts = compute_transform(transform_stack);
        let mask = get_mask(clip_stack);
        let w = pixmap.width() as f32;
        let h = pixmap.height() as f32;
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
            pixmap.fill_rect(rect, &paint, ts, mask);
        }
    }

    Ok(())
}

/// Sweep gradient approximation: decompose into N arc segments
/// with interpolated solid fills.
fn paint_sweep_gradient(
    pixmap: &mut tiny_skia::Pixmap,
    transform_stack: &[tiny_skia::Transform],
    clip_stack: &[Option<tiny_skia::Mask>],
    color_line: &ColorLine,
    cx: f32,
    cy: f32,
    start_angle: f32,
    end_angle: f32,
) -> anyhow::Result<()> {
    let ts = compute_transform(transform_stack);
    let mask = get_mask(clip_stack);

    let w = pixmap.width() as f64;
    let h = pixmap.height() as f64;
    let radius = ((w * w + h * h) as f64).sqrt() as f32;

    let mut stops = color_line.color_stops.clone();
    stops.sort_by(|a, b| a.offset.partial_cmp(&b.offset).unwrap());

    if stops.is_empty() {
        return Ok(());
    }

    let angle_span = end_angle - start_angle;
    if angle_span.abs() < 0.0001 {
        return Ok(());
    }

    let n_segments = 64usize;
    for seg in 0..n_segments {
        let t0 = seg as f32 / n_segments as f32;
        let t1 = (seg + 1) as f32 / n_segments as f32;
        let angle0 = start_angle + t0 * angle_span;
        let angle1 = start_angle + t1 * angle_span;
        let t_mid = (t0 + t1) / 2.0;

        let color = interpolate_color_line(&stops, t_mid as f64);
        let (r, g, b, a) = color.as_srgba_tuple();

        if let Some(sk_color) =
            tiny_skia::Color::from_rgba(r as f32, g as f32, b as f32, a as f32)
        {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(cx, cy);

            let a0_rad = angle0 * std::f32::consts::PI * 2.0;
            let a1_rad = angle1 * std::f32::consts::PI * 2.0;
            let px0 = cx + radius * a0_rad.cos();
            let py0 = cy + radius * a0_rad.sin();
            let px1 = cx + radius * a1_rad.cos();
            let py1 = cy + radius * a1_rad.sin();

            pb.line_to(px0, py0);

            let da = a1_rad - a0_rad;
            let alpha = (4.0 / 3.0) * (da / 4.0).tan();
            let cos0 = a0_rad.cos();
            let sin0 = a0_rad.sin();
            let cos1 = a1_rad.cos();
            let sin1 = a1_rad.sin();
            let cp1x = cx + radius * (cos0 - alpha * sin0);
            let cp1y = cy + radius * (sin0 + alpha * cos0);
            let cp2x = cx + radius * (cos1 + alpha * sin1);
            let cp2y = cy + radius * (sin1 - alpha * cos1);
            pb.cubic_to(cp1x, cp1y, cp2x, cp2y, px1, py1);
            pb.close();

            if let Some(path) = pb.finish() {
                let paint = tiny_skia::Paint {
                    shader: tiny_skia::Shader::SolidColor(sk_color),
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    anti_alias: true,
                    force_hq_pipeline: false,
                };
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, ts, mask);
            }
        }
    }

    Ok(())
}

fn interpolate_color_line(stops: &[ColorStop], t: f64) -> wezterm_color_types::SrgbaPixel {
    if stops.len() == 1 {
        return stops[0].color;
    }

    let t = t.clamp(0.0, 1.0);

    let mut i = 0;
    while i < stops.len() - 1 && stops[i + 1].offset < t {
        i += 1;
    }
    if i >= stops.len() - 1 {
        return stops.last().unwrap().color;
    }

    let s0 = &stops[i];
    let s1 = &stops[i + 1];
    let range = s1.offset - s0.offset;
    if range < 0.0001 {
        return s0.color;
    }

    let k = (t - s0.offset) / range;
    let c0: SrgbaTuple = s0.color.into();
    let c1: SrgbaTuple = s1.color.into();
    let result = c0.interpolate(c1, k);
    wezterm_color_types::SrgbaPixel::rgba(
        (result.0 * 255.0) as u8,
        (result.1 * 255.0) as u8,
        (result.2 * 255.0) as u8,
        (result.3 * 255.0) as u8,
    )
}

fn paint_image(
    pixmap: &mut tiny_skia::Pixmap,
    transform_stack: &[tiny_skia::Transform],
    clip_stack: &[Option<tiny_skia::Mask>],
    data: &[u8],
    slant: f32,
    extents: Option<&ImageExtents>,
) -> anyhow::Result<()> {
    let decoded = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()?
        .decode()?;

    let rgba = decoded.into_rgba8();
    let (img_w, img_h) = (rgba.width(), rgba.height());

    let mut premul_data = rgba.into_vec();
    for pixel in premul_data.chunks_exact_mut(4) {
        let a = pixel[3];
        if a != 0xff && a != 0 {
            pixel[0] = multiply_alpha(a, pixel[0]);
            pixel[1] = multiply_alpha(a, pixel[1]);
            pixel[2] = multiply_alpha(a, pixel[2]);
        }
    }

    let img_pixmap = tiny_skia::PixmapRef::from_bytes(&premul_data, img_w, img_h)
        .ok_or_else(|| anyhow::anyhow!("failed to create image pixmap"))?;

    let ts = compute_transform(transform_stack);
    let mask = get_mask(clip_stack);

    if let Some(ext) = extents {
        let slant_ts = tiny_skia::Transform::from_row(1.0, 0.0, slant, 1.0, 0.0, 0.0);
        let slanted_width = ext.width - ext.height * slant;
        let slanted_x_bearing = ext.x_bearing - ext.y_bearing * slant;
        let translate = tiny_skia::Transform::from_translate(slanted_x_bearing, ext.y_bearing);
        let scale_ts = tiny_skia::Transform::from_row(
            slanted_width / img_w as f32,
            0.0,
            0.0,
            ext.height / img_h as f32,
            0.0,
            0.0,
        );

        let combined = ts
            .post_concat(slant_ts)
            .post_concat(translate)
            .post_concat(scale_ts);

        let paint = tiny_skia::PixmapPaint {
            blend_mode: tiny_skia::BlendMode::SourceOver,
            opacity: 1.0,
            quality: tiny_skia::FilterQuality::Bilinear,
        };

        pixmap.draw_pixmap(0, 0, img_pixmap, &paint, combined, mask);
    } else {
        let paint = tiny_skia::PixmapPaint::default();
        pixmap.draw_pixmap(0, 0, img_pixmap, &paint, ts, mask);
    }

    Ok(())
}

fn multiply_alpha(alpha: u8, color: u8) -> u8 {
    let temp: u32 = alpha as u32 * (color as u32 + 0x80);
    ((temp + (temp >> 8)) >> 8) as u8
}

fn reduce_anchors(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
) -> (f64, f64, f64, f64) {
    let q2x = x2 - x0;
    let q2y = y2 - y0;
    let q1x = x1 - x0;
    let q1y = y1 - y0;

    let s = q2x * q2x + q2y * q2y;
    if s < 0.000001 {
        return (x0, y0, x1, y1);
    }

    let k = (q2x * q1x + q2y * q1y) / s;
    (x0, y0, x1 - k * q2x, y1 - k * q2y)
}
