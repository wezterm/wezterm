use crate::tabbar::compute_pane_title;
use crate::termwindow::render::{RenderScreenLineParams, TripleLayerQuadAllocator};
use crate::termwindow::{PaneInformation, TabInformation};
use config::PaneBorderStatus;
use mux::renderable::RenderableDimensions;
use mux::tab::PositionedPane;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

impl crate::TermWindow {
    /// Paint a one-row title bar for a single split pane.
    ///
    /// The bar is drawn at the top or bottom of the pane's allocated cell
    /// space (controlled by `config.pane_border_status`).  Terminal content
    /// is rendered on top of it in `paint_pane`; this function draws the
    /// background and text before the pane content so that on the default
    /// `Top` position the title sits above the scrollback.
    ///
    /// The text content comes from the `format-pane-title` Lua event (see
    /// `compute_pane_title`).  When no callback is registered the pane's
    /// window title is used as a fallback.
    pub fn paint_pane_title_bar(
        &mut self,
        pos: &PositionedPane,
        pane_info: &[PaneInformation],
        tab_info: &[TabInformation],
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let config = self.config.clone();

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()?
        } else {
            0.
        };
        let (top_bar_height, _bottom_bar_height) = if config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        // Pixel y-coordinate of the title bar row.
        let title_y = match config.pane_border_status {
            PaneBorderStatus::Top => top_pixel_y + pos.top as f32 * cell_height,
            PaneBorderStatus::Bottom => {
                top_pixel_y + (pos.top + pos.height.saturating_sub(1)) as f32 * cell_height
            }
            PaneBorderStatus::Off => return Ok(()),
        };

        // Left edge of this pane in pixels.
        let title_x = padding_left + border.left.get() as f32 + pos.left as f32 * cell_width;
        let pixel_width = pos.width as f32 * cell_width;

        // Look up the PaneInformation so we can call the Lua callback.
        let pane_info_item = match pane_info.iter().find(|p| p.pane_id == pos.pane.pane_id()) {
            Some(p) => p,
            None => return Ok(()),
        };

        let title_line = compute_pane_title(pane_info_item, pane_info, tab_info, &config);

        let palette = pos.pane.palette();
        let foreground = palette.foreground.to_linear();
        let window_is_transparent =
            !self.window_background.is_empty() || config.window_background_opacity != 1.0;
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                config.text_background_opacity
            });

        // Draw a solid background strip behind the title text so it is
        // legible even when a window background image is configured.
        self.filled_rectangle(
            layers,
            0,
            euclid::rect(title_x, title_y, pixel_width, cell_height),
            default_bg,
        )?;

        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        let dims = pos.pane.get_dimensions();

        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y: title_y,
                left_pixel_x: title_x,
                pixel_width,
                stable_line_idx: None,
                line: &title_line,
                selection: 0..0,
                cursor: &Default::default(),
                palette: &palette,
                dims: &RenderableDimensions {
                    cols: pos.width,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: dims.dpi,
                    pixel_height: self.render_metrics.cell_size.height as usize,
                    pixel_width: pos.pixel_width,
                    reverse_video: false,
                },
                config: &config,
                cursor_border_color: LinearRgba::default(),
                foreground,
                pane: None,
                is_active: pos.is_active,
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
                use_pixel_positioning: config.experimental_pixel_positioning,
                render_metrics: self.render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;

        Ok(())
    }
}
