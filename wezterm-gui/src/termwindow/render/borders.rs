use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::utilsprites::RenderMetrics;
use ::window::{PointF, SizeF, ULength};
use config::{ConfigHandle, DimensionContext};

impl crate::TermWindow {
    pub fn paint_window_borders(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let border_dimensions = self.get_os_border();

        if border_dimensions.top.get() > 0
            || border_dimensions.bottom.get() > 0
            || border_dimensions.left.get() > 0
            || border_dimensions.right.get() > 0
        {
            let height = self.dimensions.pixel_height as f32;
            let width = self.dimensions.pixel_width as f32;

            let border_top = border_dimensions.top.get() as f32;
            if border_top > 0.0 {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(0.0, 0.0, width, border_top),
                    self.config
                        .window_frame
                        .border_top_color
                        .map(|c| c.to_linear())
                        .unwrap_or(border_dimensions.color),
                )?;
            }

            let border_left = border_dimensions.left.get() as f32;
            if border_left > 0.0 {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(0.0, 0.0, border_left, height),
                    self.config
                        .window_frame
                        .border_left_color
                        .map(|c| c.to_linear())
                        .unwrap_or(border_dimensions.color),
                )?;
            }

            let border_bottom = border_dimensions.bottom.get() as f32;
            if border_bottom > 0.0 {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(0.0, height - border_bottom, width, height),
                    self.config
                        .window_frame
                        .border_bottom_color
                        .map(|c| c.to_linear())
                        .unwrap_or(border_dimensions.color),
                )?;
            }

            let border_right = border_dimensions.right.get() as f32;
            if border_right > 0.0 {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(width - border_right, 0.0, border_right, height),
                    self.config
                        .window_frame
                        .border_right_color
                        .map(|c| c.to_linear())
                        .unwrap_or(border_dimensions.color),
                )?;
            }

            // On macOS, the WindowServer rounds the outer silhouette of the
            // window, but the border rectangles above paint square corners:
            // the corner overlap is only `border_width` pixels across, and
            // since the OS corner radius is bigger than that, the whole
            // colored overlap sits inside the region the OS clips away and
            // the corner comes out bare.
            //
            // Rebuild each corner as: a solid square of border color, with
            // a background-colored quarter-disc painted on top to carve the
            // window interior back out. The disc is inset by the border
            // widths and its arc is centered on the same point as the OS's
            // corner arc, so the visible ring between the OS clip and the
            // disc reads as a constant-thickness border that follows the
            // rounded corner. The OS radius isn't queryable from here; we
            // use an estimate, and because the disc meets the straight
            // border edges exactly at the configured width, an estimation
            // error only slightly fattens/thins the ring at the diagonal.
            //
            // In native fullscreen (and when the user forces square corners),
            // the OS doesn't round the window at all - the straight
            // rectangles above already produce a correct, solid square
            // corner - so skip the carve entirely; doing it anyway would cut
            // a rounded notch into a corner that's supposed to be square.
            #[cfg(target_os = "macos")]
            if !self.window_state.contains(window::WindowState::FULL_SCREEN)
                && !self
                    .config
                    .window_decorations
                    .contains(window::WindowDecorations::MACOS_FORCE_SQUARE_CORNERS)
            {
                let top_color = self
                    .config
                    .window_frame
                    .border_top_color
                    .map(|c| c.to_linear())
                    .unwrap_or(border_dimensions.color);
                let bottom_color = self
                    .config
                    .window_frame
                    .border_bottom_color
                    .map(|c| c.to_linear())
                    .unwrap_or(border_dimensions.color);
                let left_color = self
                    .config
                    .window_frame
                    .border_left_color
                    .map(|c| c.to_linear())
                    .unwrap_or(border_dimensions.color);
                let right_color = self
                    .config
                    .window_frame
                    .border_right_color
                    .map(|c| c.to_linear())
                    .unwrap_or(border_dimensions.color);

                let bg_color = self
                    .palette()
                    .background
                    .to_linear()
                    .mul_alpha(self.config.window_background_opacity);

                // Estimated OS corner radius: ~10pt on modern macOS.
                // dpi is 72 * scale on macOS, so pt -> px is dpi / 72.
                let radius = (10.0 * self.dimensions.dpi as f32 / 72.0)
                    .max(border_top.max(border_bottom))
                    .max(border_left.max(border_right))
                    + 1.0;

                let mut corner = |x: f32,
                                  y: f32,
                                  b_x: f32,
                                  b_y: f32,
                                  poly: &'static [crate::customglyph::Poly],
                                  color|
                 -> anyhow::Result<()> {
                    if b_x <= 0.0 && b_y <= 0.0 {
                        return Ok(());
                    }
                    self.filled_rectangle(
                        layers,
                        1,
                        euclid::rect(x, y, radius, radius),
                        color,
                    )?;
                    let inset_w = radius - b_x;
                    let inset_h = radius - b_y;
                    if inset_w > 0.0 && inset_h > 0.0 {
                        // The disc hugs the inner corner of its cell, so
                        // inset the cell origin only on the outer sides.
                        let inset_x = if x == 0.0 { x + b_x } else { x };
                        let inset_y = if y == 0.0 { y + b_y } else { y };
                        self.poly_quad(
                            layers,
                            1,
                            PointF::new(inset_x, inset_y),
                            poly,
                            0,
                            SizeF::new(inset_w, inset_h),
                            bg_color,
                        )?;
                    }
                    Ok(())
                };

                corner(
                    0.0,
                    0.0,
                    border_left,
                    border_top,
                    TOP_LEFT_ROUNDED_CORNER,
                    if border_top >= border_left {
                        top_color
                    } else {
                        left_color
                    },
                )?;
                corner(
                    width - radius,
                    0.0,
                    border_right,
                    border_top,
                    TOP_RIGHT_ROUNDED_CORNER,
                    if border_top >= border_right {
                        top_color
                    } else {
                        right_color
                    },
                )?;
                corner(
                    0.0,
                    height - radius,
                    border_left,
                    border_bottom,
                    BOTTOM_LEFT_ROUNDED_CORNER,
                    if border_bottom >= border_left {
                        bottom_color
                    } else {
                        left_color
                    },
                )?;
                corner(
                    width - radius,
                    height - radius,
                    border_right,
                    border_bottom,
                    BOTTOM_RIGHT_ROUNDED_CORNER,
                    if border_bottom >= border_right {
                        bottom_color
                    } else {
                        right_color
                    },
                )?;
            }
        }

        Ok(())
    }

    pub fn get_os_border_impl(
        os_parameters: &Option<window::parameters::Parameters>,
        config: &ConfigHandle,
        dimensions: &crate::Dimensions,
        render_metrics: &RenderMetrics,
    ) -> window::parameters::Border {
        let mut border = os_parameters
            .as_ref()
            .and_then(|p| p.border_dimensions.clone())
            .unwrap_or_default();

        border.left += ULength::new(
            config
                .window_frame
                .border_left_width
                .evaluate_as_pixels(DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: render_metrics.cell_size.width as f32,
                })
                .ceil() as usize,
        );
        border.right += ULength::new(
            config
                .window_frame
                .border_right_width
                .evaluate_as_pixels(DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: render_metrics.cell_size.width as f32,
                })
                .ceil() as usize,
        );
        border.top += ULength::new(
            config
                .window_frame
                .border_top_height
                .evaluate_as_pixels(DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: render_metrics.cell_size.height as f32,
                })
                .ceil() as usize,
        );
        border.bottom += ULength::new(
            config
                .window_frame
                .border_bottom_height
                .evaluate_as_pixels(DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: render_metrics.cell_size.height as f32,
                })
                .ceil() as usize,
        );

        border
    }

    pub fn get_os_border(&self) -> window::parameters::Border {
        Self::get_os_border_impl(
            &self.os_parameters,
            &self.config,
            &self.dimensions,
            &self.render_metrics,
        )
    }
}
