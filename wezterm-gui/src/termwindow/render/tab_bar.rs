use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::RenderScreenLineParams;
use crate::utilsprites::RenderMetrics;
use config::{ConfigHandle, TabBarPosition};
use mux::renderable::RenderableDimensions;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

/// Pixel insets carved out of the window for the tab bar.
///
/// Exactly one of `top` / `bottom` / `left` is non-zero in practice
/// (or all zero if the tab bar is hidden). Consumers should treat
/// the struct uniformly so that switching position is a pure data
/// change.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TabBarInsets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl TabBarInsets {
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

impl crate::TermWindow {
    pub fn paint_tab_bar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        if self.config.use_fancy_tab_bar {
            if self.fancy_tab_bar.is_none() {
                let palette = self.palette().clone();
                let tab_bar = self.build_fancy_tab_bar(&palette)?;
                self.fancy_tab_bar.replace(tab_bar);
            }

            self.ui_items.append(&mut self.paint_fancy_tab_bar()?);
            return Ok(());
        }

        let border = self.get_os_border();

        let palette = self.palette().clone();
        let tab_bar_height = self.tab_bar_pixel_height()?;
        let tab_bar_y = if self.effective_tab_bar_position() == TabBarPosition::Bottom {
            ((self.dimensions.pixel_height as f32) - (tab_bar_height + border.bottom.get() as f32))
                .max(0.)
        } else {
            border.top.get() as f32
        };

        // Register the tab bar location
        self.ui_items.append(&mut self.tab_bar.compute_ui_items(
            tab_bar_y as usize,
            self.render_metrics.cell_size.height as usize,
            self.render_metrics.cell_size.width as usize,
        ));

        let window_is_transparent =
            !self.window_background.is_empty() || self.config.window_background_opacity != 1.0;
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                self.config.text_background_opacity
            });

        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y: tab_bar_y,
                left_pixel_x: 0.,
                pixel_width: self.dimensions.pixel_width as f32,
                stable_line_idx: None,
                line: self.tab_bar.line(),
                selection: 0..0,
                cursor: &Default::default(),
                palette: &palette,
                dims: &RenderableDimensions {
                    cols: self.dimensions.pixel_width
                        / self.render_metrics.cell_size.width as usize,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: self.terminal_size.dpi,
                    pixel_height: self.render_metrics.cell_size.height as usize,
                    pixel_width: self.terminal_size.pixel_width,
                    reverse_video: false,
                },
                config: &self.config,
                cursor_border_color: LinearRgba::default(),
                foreground: palette.foreground.to_linear(),
                pane: None,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                white_space,
                filled_box,
                window_is_transparent,
                default_bg,
                style: None,
                font: None,
                use_pixel_positioning: self.config.experimental_pixel_positioning,
                render_metrics: self.render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;

        Ok(())
    }

    pub fn tab_bar_pixel_height_impl(
        config: &ConfigHandle,
        fontconfig: &wezterm_font::FontConfiguration,
        render_metrics: &RenderMetrics,
    ) -> anyhow::Result<f32> {
        if config.use_fancy_tab_bar {
            let font = fontconfig.title_font()?;
            Ok((font.metrics().cell_height.get() as f32 * 1.75).ceil())
        } else {
            Ok(render_metrics.cell_size.height as f32)
        }
    }

    pub fn tab_bar_pixel_height(&self) -> anyhow::Result<f32> {
        Self::tab_bar_pixel_height_impl(&self.config, &self.fonts, &self.render_metrics)
    }

    /// Width of the tab bar when it is rendered as a vertical strip
    /// down the side of the window.
    ///
    /// Currently a fixed value. A future iteration can promote this
    /// to a config knob, or derive it from the longest tab title.
    pub fn tab_bar_pixel_width(&self) -> f32 {
        220.0
    }

    /// Resolve the effective tab-bar position from the two config
    /// fields. `tab_bar_position` wins when non-default; otherwise
    /// the legacy `tab_bar_at_bottom` boolean controls.
    pub fn effective_tab_bar_position(&self) -> TabBarPosition {
        Self::effective_tab_bar_position_impl(&self.config)
    }

    pub fn effective_tab_bar_position_impl(config: &ConfigHandle) -> TabBarPosition {
        match config.tab_bar_position {
            TabBarPosition::Top if config.tab_bar_at_bottom => TabBarPosition::Bottom,
            other => other,
        }
    }

    /// Pixel insets reserved for the tab bar. Returns all-zero when
    /// the tab bar is hidden. Vertical tab bars are only supported
    /// in fancy mode; in non-fancy mode they fall back to Top.
    pub fn tab_bar_insets(&self) -> TabBarInsets {
        if !self.show_tab_bar {
            return TabBarInsets::default();
        }
        Self::tab_bar_insets_impl(
            &self.config,
            self.tab_bar_pixel_height().unwrap_or(0.0),
            self.tab_bar_pixel_width(),
        )
    }

    pub fn tab_bar_insets_impl(
        config: &ConfigHandle,
        tab_bar_pixel_height: f32,
        tab_bar_pixel_width: f32,
    ) -> TabBarInsets {
        let pos = Self::effective_tab_bar_position_impl(config);
        let pos = if pos == TabBarPosition::Left && !config.use_fancy_tab_bar {
            // Non-fancy bar cannot render vertically. Quietly fall back
            // to Top. A user-visible warning is logged at config load.
            TabBarPosition::Top
        } else {
            pos
        };
        match pos {
            TabBarPosition::Top => TabBarInsets {
                top: tab_bar_pixel_height,
                ..Default::default()
            },
            TabBarPosition::Bottom => TabBarInsets {
                bottom: tab_bar_pixel_height,
                ..Default::default()
            },
            TabBarPosition::Left => TabBarInsets {
                left: tab_bar_pixel_width,
                ..Default::default()
            },
        }
    }
}
