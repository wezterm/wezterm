use crate::colorease::ColorEase;
use crate::customglyph::{BlockKey, *};
use crate::glyphcache::{CachedGlyph, GlyphCache};
use crate::quad::{
    HeapQuadAllocator, QuadAllocator, QuadImpl, QuadTrait, TripleLayerQuadAllocator,
    TripleLayerQuadAllocatorTrait,
};
use crate::shapecache::*;
use crate::termwindow::render::paint::AllowImage;
use crate::termwindow::{BorrowedShapeCacheKey, RenderState, ShapedInfo, TermWindowNotif};
use crate::utilsprites::RenderMetrics;
use ::window::bitmaps::{TextureCoord, TextureRect, TextureSize};
use ::window::{DeadKeyStatus, PointF, RectF, SizeF, WindowOps};
use anyhow::{anyhow, Context};
use config::{
    BoldBrightening, Config, ConfigHandle, DimensionContext, HorizontalWindowContentAlignment,
    TextStyle, VerticalWindowContentAlignment, VisualBellTarget,
};
use euclid::num::Zero;
use mux::pane::{Pane, PaneId};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use ordered_float::NotNan;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use termwiz::cellcluster::CellCluster;
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::{CursorShape, CursorVisibility, SequenceNo};
use wezterm_font::shaper::PresentationWidth;
use wezterm_font::units::{IntPixelLength, PixelLength};
use wezterm_font::{ClearShapeCache, GlyphInfo, LoadedFont};
use wezterm_term::color::{ColorAttribute, ColorPalette};
use wezterm_term::{CellAttributes, Line, StableRowIndex};
use window::color::LinearRgba;

pub mod borders;
pub mod corners;
pub mod draw;
pub mod fancy_tab_bar;
pub mod paint;
pub mod pane;
pub mod screen_line;
pub mod split;
pub mod tab_bar;
pub mod window_buttons;

/// The data that we associate with a line; we use this to cache it shape hash
#[derive(Debug)]
pub struct CachedLineState {
    pub id: u64,
    pub seqno: SequenceNo,
    pub shape_hash: [u8; 16],
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub struct LineQuadCacheKey {
    pub config_generation: usize,
    pub shape_generation: usize,
    pub quad_generation: usize,
    /// Only set if cursor.y == stable_row
    pub composing: Option<String>,
    pub selection: Range<usize>,
    pub shape_hash: [u8; 16],
    pub top_pixel_y: NotNan<f32>,
    pub left_pixel_x: NotNan<f32>,
    pub phys_line_idx: usize,
    pub pane_id: PaneId,
    pub pane_is_active: bool,
    /// A cursor position with the y value fixed at 0.
    /// Only is_some() if the y value matches this row.
    pub cursor: Option<CursorProperties>,
    pub reverse_video: bool,
    pub password_input: bool,
}

pub struct LineQuadCacheValue {
    pub expires: Option<Instant>,
    pub layers: HeapQuadAllocator,
    // Only set if the line contains any hyperlinks, so
    // that we can invalidate when it changes
    pub current_highlight: Option<Arc<Hyperlink>>,
    pub invalidate_on_hover_change: bool,
}

pub struct LineToElementParams<'a> {
    pub line: &'a Line,
    pub config: &'a ConfigHandle,
    pub palette: &'a ColorPalette,
    pub window_is_transparent: bool,
    pub reverse_video: bool,
    pub shape_key: &'a Option<LineToEleShapeCacheKey>,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct LineToEleShapeCacheKey {
    pub shape_hash: [u8; 16],
    pub composing: Option<(usize, String)>,
    pub shape_generation: usize,
}

pub struct LineToElementShapeItem {
    pub expires: Option<Instant>,
    pub shaped: Rc<Vec<LineToElementShape>>,
    // Only set if the line contains any hyperlinks, so
    // that we can invalidate when it changes
    pub current_highlight: Option<Arc<Hyperlink>>,
    pub invalidate_on_hover_change: bool,
}

pub struct LineToElementShape {
    pub underline_tex_rect: TextureRect,
    pub fg_color: LinearRgba,
    pub bg_color: LinearRgba,
    pub underline_color: LinearRgba,
    pub x_pos: f32,
    pub pixel_width: f32,
    pub glyph_info: Rc<Vec<ShapedInfo>>,
    pub cluster: CellCluster,
}

pub struct RenderScreenLineResult {
    pub invalidate_on_hover_change: bool,
}

pub struct RenderScreenLineParams<'a> {
    /// zero-based offset from top of the window viewport to the line that
    /// needs to be rendered, measured in pixels
    pub top_pixel_y: f32,
    /// zero-based offset from left of the window viewport to the line that
    /// needs to be rendered, measured in pixels
    pub left_pixel_x: f32,
    pub pixel_width: f32,
    pub stable_line_idx: Option<StableRowIndex>,
    pub line: &'a Line,
    pub selection: Range<usize>,
    pub cursor: &'a StableCursorPosition,
    pub palette: &'a ColorPalette,
    pub dims: &'a RenderableDimensions,
    pub config: &'a ConfigHandle,
    pub pane: Option<&'a Arc<dyn Pane>>,

    pub white_space: TextureRect,
    pub filled_box: TextureRect,

    pub cursor_border_color: LinearRgba,
    pub foreground: LinearRgba,
    pub is_active: bool,

    pub selection_fg: LinearRgba,
    pub selection_bg: LinearRgba,
    pub cursor_fg: LinearRgba,
    pub cursor_bg: LinearRgba,
    pub cursor_is_default_color: bool,

    pub window_is_transparent: bool,
    pub default_bg: LinearRgba,

    /// Override font resolution; useful together with
    /// the resolved title font
    pub font: Option<Rc<LoadedFont>>,
    pub style: Option<&'a TextStyle>,

    /// If true, use the shaper-determined pixel positions,
    /// rather than using monospace cell based positions.
    pub use_pixel_positioning: bool,

    pub render_metrics: RenderMetrics,
    pub shape_key: Option<LineToEleShapeCacheKey>,
    pub password_input: bool,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct CursorProperties {
    pub position: StableCursorPosition,
    pub dead_key_or_leader: bool,
    pub cursor_is_default_color: bool,
    pub cursor_fg: LinearRgba,
    pub cursor_bg: LinearRgba,
    pub cursor_border_color: LinearRgba,
}

pub struct ComputeCellFgBgParams<'a> {
    pub selected: bool,
    pub cursor: Option<&'a StableCursorPosition>,
    pub fg_color: LinearRgba,
    pub bg_color: LinearRgba,
    pub is_active_pane: bool,
    pub config: &'a ConfigHandle,
    pub selection_fg: LinearRgba,
    pub selection_bg: LinearRgba,
    pub cursor_fg: LinearRgba,
    pub cursor_bg: LinearRgba,
    pub cursor_is_default_color: bool,
    pub cursor_border_color: LinearRgba,
    pub pane: Option<&'a Arc<dyn Pane>>,
}

#[derive(Debug)]
pub struct ComputeCellFgBgResult {
    pub fg_color: LinearRgba,
    pub fg_color_alt: LinearRgba,
    pub bg_color: LinearRgba,
    pub bg_color_alt: LinearRgba,
    pub fg_color_mix: f32,
    pub bg_color_mix: f32,
    pub cursor_border_color: LinearRgba,
    pub cursor_border_color_alt: LinearRgba,
    pub cursor_border_mix: f32,
    pub cursor_shape: Option<CursorShape>,
}

/// Basic cache of computed data from prior cluster to avoid doing the same
/// work for space separated clusters with the same style
#[derive(Clone, Debug)]
pub struct ClusterStyleCache<'a> {
    attrs: &'a CellAttributes,
    style: &'a TextStyle,
    underline_tex_rect: TextureRect,
    fg_color: LinearRgba,
    bg_color: LinearRgba,
    underline_color: LinearRgba,
}

impl crate::TermWindow {
    pub fn update_next_frame_time(&self, next_due: Option<Instant>) {
        if next_due.is_some() {
            update_next_frame_time(&mut *self.has_animation.borrow_mut(), next_due);
        }
    }

    fn get_intensity_if_bell_target_ringing(
        &self,
        pane: &Arc<dyn Pane>,
        config: &ConfigHandle,
        target: VisualBellTarget,
    ) -> Option<f32> {
        let mut per_pane = self.pane_state(pane.pane_id());
        if let Some(ringing) = per_pane.bell_start {
            if config.visual_bell.target == target {
                let mut color_ease = ColorEase::new(
                    config.visual_bell.fade_in_duration_ms,
                    config.visual_bell.fade_in_function,
                    config.visual_bell.fade_out_duration_ms,
                    config.visual_bell.fade_out_function,
                    Some(ringing),
                );

                let intensity = color_ease.intensity_one_shot();

                match intensity {
                    None => {
                        per_pane.bell_start.take();
                    }
                    Some((intensity, next)) => {
                        self.update_next_frame_time(Some(next));
                        return Some(intensity);
                    }
                }
            }
        }
        None
    }

    pub fn filled_rectangle<'a>(
        &self,
        layers: &'a mut TripleLayerQuadAllocator,
        layer_num: usize,
        rect: RectF,
        color: LinearRgba,
    ) -> anyhow::Result<QuadImpl<'a>> {
        let mut quad = layers.allocate(layer_num)?;
        let left_offset = self.dimensions.pixel_width as f32 / 2.;
        let top_offset = self.dimensions.pixel_height as f32 / 2.;
        let gl_state = self.render_state.as_ref().unwrap();
        quad.set_position(
            rect.min_x() as f32 - left_offset,
            rect.min_y() as f32 - top_offset,
            rect.max_x() as f32 - left_offset,
            rect.max_y() as f32 - top_offset,
        );
        quad.set_texture(gl_state.util_sprites.filled_box.texture_coords());
        quad.set_is_background();
        quad.set_fg_color(color);
        quad.set_hsv(None);
        Ok(quad)
    }

    pub fn poly_quad<'a>(
        &self,
        layers: &'a mut TripleLayerQuadAllocator,
        layer_num: usize,
        point: PointF,
        polys: &'static [Poly],
        underline_height: IntPixelLength,
        cell_size: SizeF,
        color: LinearRgba,
    ) -> anyhow::Result<QuadImpl<'a>> {
        let left_offset = self.dimensions.pixel_width as f32 / 2.;
        let top_offset = self.dimensions.pixel_height as f32 / 2.;
        let gl_state = self.render_state.as_ref().unwrap();
        let sprite = gl_state
            .glyph_cache
            .borrow_mut()
            .cached_block(
                BlockKey::PolyWithCustomMetrics {
                    polys,
                    underline_height,
                    cell_size: euclid::size2(cell_size.width as isize, cell_size.height as isize),
                },
                &self.render_metrics,
            )?
            .texture_coords();

        let mut quad = layers.allocate(layer_num)?;

        quad.set_position(
            point.x - left_offset,
            point.y - top_offset,
            (point.x + cell_size.width as f32) - left_offset,
            (point.y + cell_size.height as f32) - top_offset,
        );
        quad.set_texture(sprite);
        quad.set_fg_color(color);
        quad.set_alt_color_and_mix_value(color, 0.);
        quad.set_hsv(None);
        quad.set_has_color(false);
        Ok(quad)
    }

    pub fn min_scroll_bar_height(&self) -> f32 {
        self.config
            .min_scroll_bar_height
            .evaluate_as_pixels(DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: self.terminal_size.pixel_height as f32,
                pixel_cell: self.render_metrics.cell_size.height as f32,
            })
    }

    pub fn padding_left_top(&self) -> (f32, f32) {
        let h_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.terminal_size.pixel_width as f32,
            pixel_cell: self.render_metrics.cell_size.width as f32,
        };
        let v_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.terminal_size.pixel_height as f32,
            pixel_cell: self.render_metrics.cell_size.height as f32,
        };

        let padding_left = self
            .config
            .window_padding
            .left
            .evaluate_as_pixels(h_context);
        let padding_right = self.config.window_padding.right;
        let padding_top = self.config.window_padding.top.evaluate_as_pixels(v_context);
        let padding_bottom = self
            .config
            .window_padding
            .bottom
            .evaluate_as_pixels(v_context);

        let horizontal_gap = self.dimensions.pixel_width as f32
            - self.terminal_size.pixel_width as f32
            - padding_left
            - if self.show_scroll_bar && padding_right.is_zero() {
                h_context.pixel_cell
            } else {
                padding_right.evaluate_as_pixels(h_context)
            };
        let vertical_gap = self.dimensions.pixel_height as f32
            - self.terminal_size.pixel_height as f32
            - padding_top
            - padding_bottom
            - if self.show_tab_bar {
                self.tab_bar_pixel_height().unwrap_or(0.)
            } else {
                0.
            };
        let left_gap = match self.config.window_content_alignment.horizontal {
            HorizontalWindowContentAlignment::Left => 0.,
            HorizontalWindowContentAlignment::Center => (horizontal_gap / 2.).round(),
            HorizontalWindowContentAlignment::Right => horizontal_gap,
        };
        let top_gap = match self.config.window_content_alignment.vertical {
            VerticalWindowContentAlignment::Top => 0.,
            VerticalWindowContentAlignment::Center => (vertical_gap / 2.).round(),
            VerticalWindowContentAlignment::Bottom => vertical_gap,
        };

        (padding_left + left_gap, padding_top + top_gap)
    }

    fn resolve_lock_glyph(
        &self,
        style: &TextStyle,
        attrs: &CellAttributes,
        font: Option<&Rc<LoadedFont>>,
        gl_state: &RenderState,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<Rc<CachedGlyph>> {
        let fa_lock = "\u{f023}";
        let line = Line::from_text(fa_lock, attrs, 0, None);
        let cluster = line.cluster(None);
        let shape_info = self.cached_cluster_shape(style, &cluster[0], gl_state, font, metrics)?;
        Ok(Rc::clone(&shape_info[0].glyph))
    }

    pub fn populate_block_quad(
        &self,
        block: BlockKey,
        gl_state: &RenderState,
        quads: &mut dyn QuadAllocator,
        pos_x: f32,
        params: &RenderScreenLineParams,
        hsv: Option<config::HsbTransform>,
        glyph_color: LinearRgba,
    ) -> anyhow::Result<()> {
        let sprite = gl_state
            .glyph_cache
            .borrow_mut()
            .cached_block(block, &params.render_metrics)?
            .texture_coords();

        let mut quad = quads.allocate()?;
        let cell_width = params.render_metrics.cell_size.width as f32;
        let cell_height = params.render_metrics.cell_size.height as f32;
        let pos_y = (self.dimensions.pixel_height as f32 / -2.) + params.top_pixel_y;
        quad.set_position(pos_x, pos_y, pos_x + cell_width, pos_y + cell_height);
        quad.set_hsv(hsv);
        quad.set_fg_color(glyph_color);
        quad.set_texture(sprite);
        quad.set_has_color(false);
        Ok(())
    }

    /// Render iTerm2 style image attributes
    pub fn populate_image_quad(
        &self,
        image: &termwiz::image::ImageCell,
        gl_state: &RenderState,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        cell_idx: usize,
        params: &RenderScreenLineParams,
        hsv: Option<config::HsbTransform>,
        glyph_color: LinearRgba,
    ) -> anyhow::Result<()> {
        if self.allow_images == AllowImage::No {
            return Ok(());
        }

        let padding = self
            .render_metrics
            .cell_size
            .height
            .max(params.render_metrics.cell_size.width) as usize;
        let padding = if padding.is_power_of_two() {
            padding
        } else {
            padding.next_power_of_two()
        };

        let (sprite, next_due, _load_state) = gl_state
            .glyph_cache
            .borrow_mut()
            .cached_image(image.image_data(), Some(padding), self.allow_images)
            .context("cached_image")?;
        self.update_next_frame_time(next_due);
        let width = sprite.coords.size.width;
        let height = sprite.coords.size.height;

        let top_left = image.top_left();
        let bottom_right = image.bottom_right();

        // We *could* call sprite.texture.to_texture_coords() here,
        // but since that takes integer pixel coordinates, we'd
        // lose precision and end up with visual artifacts.
        // Instead, we compute the texture coords here in floating point.

        let texture_width = sprite.texture.width() as f32;
        let texture_height = sprite.texture.height() as f32;
        let origin = TextureCoord::new(
            (sprite.coords.origin.x as f32 + (*top_left.x * width as f32)) / texture_width,
            (sprite.coords.origin.y as f32 + (*top_left.y * height as f32)) / texture_height,
        );

        let size = TextureSize::new(
            (*bottom_right.x - *top_left.x) * width as f32 / texture_width,
            (*bottom_right.y - *top_left.y) * height as f32 / texture_height,
        );

        let texture_rect = TextureRect::new(origin, size);

        let mut quad = layers.allocate(layer_num)?;
        let cell_width = params.render_metrics.cell_size.width as f32;
        let cell_height = params.render_metrics.cell_size.height as f32;
        let pos_y = (self.dimensions.pixel_height as f32 / -2.) + params.top_pixel_y;

        let pos_x = (self.dimensions.pixel_width as f32 / -2.)
            + params.left_pixel_x
            + (cell_idx as f32 * cell_width);

        let (padding_left, padding_top, padding_right, padding_bottom) = image.padding();

        quad.set_position(
            pos_x + padding_left as f32,
            pos_y + padding_top as f32,
            pos_x + cell_width + padding_left as f32 - padding_right as f32,
            pos_y + cell_height + padding_top as f32 - padding_bottom as f32,
        );
        quad.set_hsv(hsv);
        quad.set_fg_color(glyph_color);
        quad.set_texture(texture_rect);
        quad.set_has_color(true);

        Ok(())
    }

    fn ensure_min_contrast(&self, fg_color: LinearRgba, bg_color: LinearRgba) -> LinearRgba {
        match self.config.text_min_contrast_ratio {
            Some(ratio) => fg_color
                .ensure_contrast_ratio(&bg_color, ratio)
                .unwrap_or(fg_color),
            None => fg_color,
        }
    }

    pub fn compute_cell_fg_bg(&self, params: ComputeCellFgBgParams) -> ComputeCellFgBgResult {
        if params.cursor.is_some() {
            if let Some(bg_color_mix) = self.get_intensity_if_bell_target_ringing(
                params.pane.expect("cursor only set if pane present"),
                params.config,
                VisualBellTarget::CursorColor,
            ) {
                let (fg_color, bg_color) = if self.use_reverse_video_cursor(&params) {
                    (force_opaque(params.bg_color), force_opaque(params.fg_color))
                } else {
                    (
                        force_opaque(params.cursor_fg),
                        force_opaque(params.cursor_bg),
                    )
                };

                let fg_color = self.ensure_min_contrast(fg_color, bg_color);

                // interpolate between the background color and the target color
                let bg_color_alt = force_opaque(
                    params
                        .config
                        .resolved_palette
                        .visual_bell
                        .map(|c| c.to_linear())
                        .unwrap_or(fg_color),
                );

                // Cursor is mid-visual-bell. It's appearance reset to solid block,
                // so cell's bg equals cursor's color.
                return ComputeCellFgBgResult {
                    fg_color,
                    fg_color_alt: fg_color,
                    fg_color_mix: 0.,
                    bg_color,
                    bg_color_alt: bg_color_alt,
                    bg_color_mix,
                    cursor_shape: Some(CursorShape::Default),
                    cursor_border_color: bg_color,
                    cursor_border_color_alt: bg_color_alt,
                    cursor_border_mix: bg_color_mix,
                };
            }

            let dead_key_or_leader =
                self.dead_key_status != DeadKeyStatus::None || self.leader_is_active();

            if dead_key_or_leader && params.is_active_pane {
                let (fg_color, bg_color) = if self.use_reverse_video_cursor(&params) {
                    (force_opaque(params.bg_color), force_opaque(params.fg_color))
                } else {
                    (
                        force_opaque(params.cursor_fg),
                        force_opaque(params.cursor_bg),
                    )
                };

                let fg_color = self.ensure_min_contrast(fg_color, bg_color);

                let color = force_opaque(
                    params
                        .config
                        .resolved_palette
                        .compose_cursor
                        .map(|c| c.to_linear())
                        .unwrap_or(bg_color),
                );

                // Cursor is mid-leader-indication. It's appearance reset to solid block,
                // so cell's bg equals cursor's color.
                return ComputeCellFgBgResult {
                    fg_color,
                    fg_color_alt: fg_color,
                    fg_color_mix: 0.,
                    bg_color: color,
                    bg_color_alt: color,
                    bg_color_mix: 0.,
                    cursor_shape: Some(CursorShape::Default),
                    cursor_border_color: color,
                    cursor_border_color_alt: color,
                    cursor_border_mix: 0.,
                };
            }
        }

        let (cursor_shape, visibility) = match params.cursor {
            Some(cursor) => (
                params
                    .config
                    .default_cursor_style
                    .effective_shape(cursor.shape),
                cursor.visibility,
            ),
            _ => (CursorShape::default(), CursorVisibility::Hidden),
        };

        let focused_and_active = self.focused.is_some() && params.is_active_pane;

        let (fg_color, bg_color, cursor_bg) = match (
            params.selected,
            focused_and_active,
            cursor_shape,
            visibility,
        ) {
            // Selected text overrides colors
            (true, _, _, CursorVisibility::Hidden) => (
                // A fully transparent `selection_fg` means
                // the selection supplied no colour, so the cell keeps its own
                // foreground.
                if params.selection_fg.is_fully_transparent() {
                    params.fg_color
                } else {
                    force_opaque(params.selection_fg)
                },
                params.selection_bg,
                force_opaque(params.cursor_bg),
            ),
            // block Cursor cell overrides colors
            (
                _,
                true,
                CursorShape::BlinkingBlock | CursorShape::SteadyBlock,
                CursorVisibility::Visible,
            ) => {
                if self.use_reverse_video_cursor(&params) {
                    let (fg_color, bg_color) =
                        (force_opaque(params.bg_color), force_opaque(params.fg_color));
                    (fg_color, bg_color, bg_color)
                } else {
                    (
                        // A fully transparent `cursor_fg` means the cursor
                        // supplied no colour, so the cell keeps its own
                        // foreground.
                        if params.cursor_fg.is_fully_transparent() {
                            params.fg_color
                        } else {
                            force_opaque(params.cursor_fg)
                        },
                        force_opaque(params.cursor_bg),
                        force_opaque(params.cursor_bg),
                    )
                }
            }
            (
                _,
                true,
                CursorShape::BlinkingUnderline
                | CursorShape::SteadyUnderline
                | CursorShape::BlinkingBar
                | CursorShape::SteadyBar,
                CursorVisibility::Visible,
            ) => {
                // A bar or underline cursor substitutes only the cursor
                // border, so the glyph keeps the cell's own foreground.
                if self.use_reverse_video_cursor(&params) {
                    (
                        params.fg_color,
                        params.bg_color,
                        force_opaque(params.fg_color),
                    )
                } else {
                    (
                        params.fg_color,
                        params.bg_color,
                        force_opaque(params.cursor_bg),
                    )
                }
            }
            // Normally, render the cell as configured (or if the window is unfocused)
            _ => (
                params.fg_color,
                params.bg_color,
                force_opaque(params.cursor_border_color),
            ),
        };

        let fg_color = self.ensure_min_contrast(fg_color, bg_color);

        let blinking = params.cursor.is_some()
            && params.is_active_pane
            && cursor_shape.is_blinking()
            && params.config.cursor_blink_rate != 0
            && self.focused.is_some();

        let mut fg_color_alt = fg_color;
        let bg_color_alt = bg_color;
        let mut fg_color_mix = 0.;
        let bg_color_mix = 0.;
        let mut cursor_border_color_alt = cursor_bg;
        let mut cursor_border_mix = 0.;

        if blinking {
            let mut color_ease = self.cursor_blink_state.borrow_mut();
            color_ease.update_start(self.prev_cursor.last_cursor_movement());
            let (intensity, next) = color_ease.intensity_continuous();

            cursor_border_mix = intensity;
            // Note: don't use LinearRgba::TRANSPARENT here or it will affect
            // interpolation of RGB components between `cursor_border_color`
            // and `cursor_border_color_alt`.
            cursor_border_color_alt = cursor_bg.mul_alpha(0.0);

            if matches!(
                cursor_shape,
                CursorShape::BlinkingBlock | CursorShape::SteadyBlock,
            ) {
                // The blink's other endpoint is the cell as it looks with no
                // cursor on it, fade and all.
                fg_color_alt = params.fg_color;
                fg_color_mix = intensity;
            }

            self.update_next_frame_time(Some(next));
        }

        ComputeCellFgBgResult {
            fg_color,
            fg_color_alt,
            bg_color,
            bg_color_alt,
            fg_color_mix,
            bg_color_mix,
            cursor_border_color: cursor_bg,
            cursor_border_color_alt,
            cursor_border_mix,
            cursor_shape: if visibility == CursorVisibility::Visible {
                match cursor_shape {
                    CursorShape::BlinkingBlock | CursorShape::SteadyBlock if focused_and_active => {
                        Some(CursorShape::Default)
                    }
                    // When not focused, convert bar to block to make it more visually
                    // distinct from the focused bar in another pane
                    _shape if !focused_and_active => Some(CursorShape::SteadyBlock),
                    shape => Some(shape),
                }
            } else {
                None
            },
        }
    }

    fn use_reverse_video_cursor(&self, params: &ComputeCellFgBgParams) -> bool {
        self.config.force_reverse_video_cursor
            && params.cursor_is_default_color
            && params.fg_color.contrast_ratio(&params.bg_color)
                >= self.config.reverse_video_cursor_min_contrast
    }

    fn glyph_infos_to_glyphs(
        &self,
        style: &TextStyle,
        glyph_cache: &mut GlyphCache,
        infos: &[GlyphInfo],
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<Vec<Rc<CachedGlyph>>> {
        let mut glyphs = Vec::with_capacity(infos.len());
        let mut iter = infos.iter().peekable();
        while let Some(info) = iter.next() {
            if self.config.custom_block_glyphs {
                if info.only_char.and_then(BlockKey::from_char).is_some() {
                    // Don't bother rendering the glyph from the font, as it can
                    // have incorrect advance metrics.
                    // Instead, just use our pixel-perfect cell metrics
                    glyphs.push(Rc::new(CachedGlyph {
                        brightness_adjust: 1.0,
                        has_color: false,
                        texture: None,
                        x_advance: PixelLength::new(metrics.cell_size.width as f64),
                        x_offset: PixelLength::zero(),
                        y_offset: PixelLength::zero(),
                        bearing_x: PixelLength::zero(),
                        bearing_y: PixelLength::zero(),
                        scale: 1.0,
                    }));
                    continue;
                }
            }

            let followed_by_space = match iter.peek() {
                Some(next_info) => next_info.is_space,
                None => false,
            };

            glyphs.push(glyph_cache.cached_glyph(
                info,
                &style,
                followed_by_space,
                font,
                metrics,
                info.num_cells,
            )?);
        }
        Ok(glyphs)
    }

    /// Shape the printable text from a cluster
    fn cached_cluster_shape(
        &self,
        style: &TextStyle,
        cluster: &CellCluster,
        gl_state: &RenderState,
        font: Option<&Rc<LoadedFont>>,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<Rc<Vec<ShapedInfo>>> {
        let shape_resolve_start = Instant::now();
        let key = BorrowedShapeCacheKey {
            style,
            text: &cluster.text,
        };
        let glyph_info = match self.lookup_cached_shape(&key) {
            Some(Ok(info)) => info,
            Some(Err(err)) => return Err(err),
            None => {
                let font = match font {
                    Some(f) => Rc::clone(f),
                    None => self.fonts.resolve_font(style)?,
                };
                let window = self.window.as_ref().unwrap().clone();

                let presentation_width = PresentationWidth::with_cluster(&cluster);

                match font.shape(
                    &cluster.text,
                    move || window.notify(TermWindowNotif::InvalidateShapeCache),
                    BlockKey::filter_out_synthetic,
                    Some(cluster.presentation),
                    cluster.direction,
                    None, // FIXME: need more paragraph context
                    Some(&presentation_width),
                ) {
                    Ok(info) => {
                        let glyphs = self.glyph_infos_to_glyphs(
                            &style,
                            &mut gl_state.glyph_cache.borrow_mut(),
                            &info,
                            &font,
                            metrics,
                        )?;
                        let shaped = Rc::new(ShapedInfo::process(&info, &glyphs));

                        self.shape_cache
                            .borrow_mut()
                            .put(key.to_owned(), Ok(Rc::clone(&shaped)));
                        shaped
                    }
                    Err(err) => {
                        if err.root_cause().downcast_ref::<ClearShapeCache>().is_some() {
                            return Err(err);
                        }

                        let res = anyhow!("shaper error: {}", err);
                        self.shape_cache.borrow_mut().put(key.to_owned(), Err(err));
                        return Err(res);
                    }
                }
            }
        };
        metrics::histogram!("cached_cluster_shape").record(shape_resolve_start.elapsed());
        log::trace!(
            "shape_resolve for cluster len {} -> elapsed {:?}",
            cluster.text.len(),
            shape_resolve_start.elapsed()
        );
        Ok(glyph_info)
    }

    fn lookup_cached_shape(
        &self,
        key: &dyn ShapeCacheKeyTrait,
    ) -> Option<anyhow::Result<Rc<Vec<ShapedInfo>>>> {
        match self.shape_cache.borrow_mut().get(key) {
            Some(Ok(info)) => Some(Ok(Rc::clone(info))),
            Some(Err(err)) => Some(Err(anyhow!("cached shaper error: {}", err))),
            None => None,
        }
    }

    pub fn recreate_texture_atlas(&mut self, size: Option<usize>) -> anyhow::Result<()> {
        self.shape_generation += 1;
        self.shape_cache.borrow_mut().clear();
        self.line_to_ele_shape_cache.borrow_mut().clear();
        if let Some(render_state) = self.render_state.as_mut() {
            render_state.recreate_texture_atlas(&self.fonts, &self.render_metrics, size)?;
        }
        Ok(())
    }

    fn shape_hash_for_line(&mut self, line: &Line) -> [u8; 16] {
        let seqno = line.current_seqno();
        let mut id = None;
        if let Some(cached_arc) = line.get_appdata() {
            if let Some(line_state) = cached_arc.downcast_ref::<CachedLineState>() {
                if line_state.seqno == seqno {
                    // Touch the LRU
                    self.line_state_cache.borrow_mut().get(&line_state.id);
                    return line_state.shape_hash;
                }
                id.replace(line_state.id);
            }
        }

        let id = id.unwrap_or_else(|| {
            let id = self.next_line_state_id;
            self.next_line_state_id += 1;
            id
        });

        let shape_hash = line.compute_shape_hash();

        let state = Arc::new(CachedLineState {
            id,
            seqno,
            shape_hash,
        });

        line.set_appdata(Arc::clone(&state));

        self.line_state_cache.borrow_mut().put(id, state);
        shape_hash
    }
}

/// Force a colour to full opacity by setting its alpha to 1. Used throughout
/// this code for elements where dim style and translucent colors should not
/// affect rendering, e.g. when rendering cursor, search/select overlay, etc.
fn force_opaque(color: LinearRgba) -> LinearRgba {
    let (r, g, b, _) = color.tuple();
    LinearRgba::with_components(r, g, b, 1.0)
}

/// How much `dim_opacity` fades this cell.
pub fn dim_fade_factor(attrs: &CellAttributes, config: &Config) -> f32 {
    if config.dim_is_faded() && config.is_dim(attrs) && !attrs.suppress_dim_fade() {
        config.dim_opacity()
    } else {
        1.0
    }
}

/// Apply `dim_opacity` to a colour about to draw glyphs or decorations for
/// this cell.
pub fn apply_dim_fade(color: LinearRgba, attrs: &CellAttributes, config: &Config) -> LinearRgba {
    color.mul_alpha(dim_fade_factor(attrs, config))
}

/// Would drawing this glyph change nothing? Such a glyph must not be drawn:
/// antialiasing gives it a ghostly outline.
///
/// Compositing the glyph over the background leaves the result untouched only
/// when `fg.a * (1 - bg.a) == 0`, so there are two cases: a glyph that is
/// fully transparent, and an opaque background the glyph matches in colour.
///
/// `bg_color` describes one quad, not the stack under it, so its alpha is what
/// says whether that stack matters. `render_screen_line` zeroes it for a cell
/// with the default background in a transparent window, where the pixels
/// behind are the desktop or a background image and nothing can be claimed
/// about them; an alpha of 1.0 means something opaque is known to be there,
/// either the cell's own quad or the pane's, drawn in the colour named here.
pub fn glyph_is_invisible(glyph_color: LinearRgba, bg_color: LinearRgba) -> bool {
    let (fg_r, fg_g, fg_b, fg_a) = glyph_color.tuple();
    let (bg_r, bg_g, bg_b, bg_a) = bg_color.tuple();
    fg_a == 0.0 || (bg_a == 1.0 && (fg_r, fg_g, fg_b) == (bg_r, bg_g, bg_b))
}

/// Resolve the colour a cell's foreground attribute names, including the
/// standard-palette lift that `bold_brightens_ansi_colors` performs.
///
/// Takes `&Config` and not `&ConfigHandle` because a handle has no
/// constructor that accepts a custom value, so the lift could not otherwise be
/// tested. Call sites pass their `&ConfigHandle` and coerce.
fn resolve_fg_color_attr(
    attrs: &CellAttributes,
    fg: ColorAttribute,
    palette: &ColorPalette,
    config: &Config,
    style: &config::TextStyle,
) -> LinearRgba {
    match fg {
        wezterm_term::color::ColorAttribute::Default => {
            if let Some(fg) = style.foreground {
                fg.into()
            } else {
                palette.resolve_fg(attrs.foreground())
            }
        }
        wezterm_term::color::ColorAttribute::PaletteIndex(idx)
            if idx < 8 && config.bold_brightens_ansi_colors != BoldBrightening::No =>
        {
            // For compatibility purposes, switch to a brighter version
            // of one of the standard ANSI colors when Bold is enabled.
            // This lifts black to dark grey.
            let idx = if config.is_bold(attrs) { idx + 8 } else { idx };

            palette.resolve_fg(wezterm_term::color::ColorAttribute::PaletteIndex(idx))
        }
        _ => palette.resolve_fg(fg),
    }
    .to_linear()
}

fn update_next_frame_time(storage: &mut Option<Instant>, next_due: Option<Instant>) {
    if let Some(next_due) = next_due {
        match storage.take() {
            None => {
                storage.replace(next_due);
            }
            Some(t) if next_due < t => {
                storage.replace(next_due);
            }
            Some(t) => {
                storage.replace(t);
            }
        }
    }
}

fn same_hyperlink(a: Option<&Arc<Hyperlink>>, b: Option<&Arc<Hyperlink>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

#[cfg(test)]
mod fade_test {
    use super::{apply_dim_fade, dim_fade_factor, force_opaque, glyph_is_invisible};
    use ::window::color::LinearRgba;
    use config::Config;
    use wezterm_term::{CellAttributes, Intensity};

    fn opaque_white() -> LinearRgba {
        LinearRgba::with_components(1.0, 1.0, 1.0, 1.0)
    }

    fn config_with_opacity(opacity: Option<f32>) -> Config {
        let mut config = Config::default_config();
        config.dim_opacity = opacity;
        config
    }

    fn dim() -> CellAttributes {
        let mut attrs = CellAttributes::default();
        attrs.set_intensity(Intensity::Half);
        attrs
    }

    #[test]
    fn forcing_a_colour_opaque_leaves_its_rgb_alone() {
        let transparent = LinearRgba::with_components(0.2, 0.3, 0.4, 0.0);
        assert_eq!(force_opaque(transparent).tuple(), (0.2, 0.3, 0.4, 1.0));
    }

    #[test]
    fn a_reverse_video_cursor_can_still_hide_an_invisible_faded_glyph() {
        let config = config_with_opacity(Some(0.5));
        let cell = LinearRgba::with_components(0.23, 0.23, 0.23, 1.0);
        let faded_fg = apply_dim_fade(cell, &dim(), &config);
        assert_eq!(
            faded_fg.tuple().3,
            0.5,
            "the fixture has to be faded or this test proves nothing"
        );

        let (glyph, behind) = (force_opaque(cell), force_opaque(faded_fg));
        assert!(
            glyph_is_invisible(glyph, behind),
            "the glyph and the rectangle behind it are the same opaque \
             colour: {:?} against {:?}",
            glyph,
            behind
        );

        let red = LinearRgba::with_components(0.9, 0.1, 0.1, 1.0);
        let (glyph, behind) = (
            force_opaque(cell),
            force_opaque(apply_dim_fade(red, &dim(), &config)),
        );
        assert!(
            !glyph_is_invisible(glyph, behind),
            "a different colour still draws: {:?} against {:?}",
            glyph,
            behind
        );
    }

    #[test]
    fn the_fade_does_nothing_at_the_default() {
        let config = Config::default_config();
        assert_eq!(
            apply_dim_fade(opaque_white(), &dim(), &config).tuple().3,
            1.0
        );
    }

    #[test]
    fn the_fade_does_nothing_at_the_default_whatever_the_intensity() {
        let config = Config::default_config();
        for intensity in [Intensity::Normal, Intensity::Bold, Intensity::Half] {
            let mut attrs = CellAttributes::default();
            attrs.set_intensity(intensity);
            assert_eq!(
                apply_dim_fade(opaque_white(), &attrs, &config).tuple().3,
                1.0
            );
        }
    }

    #[test]
    fn a_configured_opacity_fades_dim_text_and_nothing_else() {
        let config = config_with_opacity(Some(0.25));

        assert_eq!(
            apply_dim_fade(opaque_white(), &dim(), &config).tuple().3,
            0.25,
            "dim text fades"
        );

        for intensity in [Intensity::Normal, Intensity::Bold] {
            let mut attrs = CellAttributes::default();
            attrs.set_intensity(intensity);
            assert_eq!(
                apply_dim_fade(opaque_white(), &attrs, &config).tuple().3,
                1.0,
                "{intensity:?} text does not fade"
            );
        }
    }

    #[test]
    fn an_explicit_opacity_of_one_is_inert() {
        let config = config_with_opacity(Some(1.0));
        let faded = LinearRgba::with_components(1.0, 1.0, 1.0, 0.6);
        assert_eq!(apply_dim_fade(faded, &dim(), &config), faded);
    }

    #[test]
    fn the_fade_follows_the_tracking_mode() {
        let mut attrs = CellAttributes::default();
        attrs.apply_sgr_intensity(Intensity::Half);
        attrs.apply_sgr_intensity(Intensity::Bold);
        assert!(attrs.dim(), "the dim record is set");
        assert_eq!(
            attrs.intensity(),
            Intensity::Bold,
            "but bold arrived most recently"
        );

        let mut unified = config_with_opacity(Some(0.25));
        unified.track_bold_and_dim_separately = false;
        assert_eq!(
            apply_dim_fade(opaque_white(), &attrs, &unified).tuple().3,
            1.0,
            "under unified tracking this cell is bold, so it does not fade"
        );

        let mut separate = config_with_opacity(Some(0.25));
        separate.track_bold_and_dim_separately = true;
        assert_eq!(
            apply_dim_fade(opaque_white(), &attrs, &separate).tuple().3,
            0.25,
            "under separate tracking it is dim as well, so it fades"
        );
    }

    #[test]
    fn a_glyph_matching_an_opaque_background_draws_nothing() {
        let c = LinearRgba::with_components(0.23, 0.23, 0.23, 1.0);
        assert!(glyph_is_invisible(c, c));
        assert!(!glyph_is_invisible(
            c,
            LinearRgba::with_components(0.23, 0.23, 0.24, 1.0)
        ));
    }

    #[test]
    fn a_faded_glyph_matching_an_opaque_background_draws_nothing() {
        let bg = LinearRgba::with_components(0.23, 0.23, 0.23, 1.0);
        let faded_glyph = bg.mul_alpha(0.5);
        assert_ne!(faded_glyph, bg, "the fixture has to differ in alpha");
        assert!(glyph_is_invisible(faded_glyph, bg));
    }

    #[test]
    fn a_glyph_over_a_translucent_background_is_always_drawn() {
        let rgb = (0.23, 0.23, 0.23);
        let glyph = LinearRgba::with_components(rgb.0, rgb.1, rgb.2, 1.0);

        for bg_alpha in [0.0, 0.5, 0.999] {
            let bg = LinearRgba::with_components(rgb.0, rgb.1, rgb.2, bg_alpha);
            assert!(!glyph_is_invisible(glyph, bg), "bg alpha {}", bg_alpha);
            assert!(
                !glyph_is_invisible(glyph.mul_alpha(0.5), bg),
                "bg alpha {}, faded glyph",
                bg_alpha
            );
        }
    }

    #[test]
    fn a_fully_transparent_glyph_draws_nothing() {
        let clear = LinearRgba::with_components(0.9, 0.1, 0.1, 0.0);
        for bg_alpha in [0.0, 0.5, 1.0] {
            let bg = LinearRgba::with_components(0.23, 0.23, 0.23, bg_alpha);
            assert!(glyph_is_invisible(clear, bg), "bg alpha {}", bg_alpha);
        }
    }

    #[test]
    fn the_fade_factor_is_exactly_one_when_inert() {
        let config = Config::default_config();
        assert_eq!(dim_fade_factor(&dim(), &config), 1.0);
    }

    #[test]
    fn a_marked_cell_never_fades() {
        for track_separately in [false, true] {
            for opacity in [0.0f32, 0.25, 0.5, 1.0] {
                let mut config = config_with_opacity(Some(opacity));
                config.track_bold_and_dim_separately = track_separately;
                let case = format!("separate={track_separately} opacity={opacity}");

                let mut marked = dim();
                marked.set_suppress_dim_fade(true);
                assert_eq!(
                    dim_fade_factor(&marked, &config),
                    1.0,
                    "a marked cell does not fade ({case})"
                );

                assert_eq!(
                    dim_fade_factor(&dim(), &config),
                    opacity,
                    "the same cell unmarked reports the opacity ({case})"
                );

                assert_eq!(
                    apply_dim_fade(opaque_white(), &marked, &config).tuple().3,
                    1.0,
                    "the wrapper follows the factor ({case})"
                );

                for intensity in [Intensity::Normal, Intensity::Bold] {
                    let mut attrs = CellAttributes::default();
                    attrs.set_intensity(intensity);
                    attrs.set_suppress_dim_fade(true);
                    assert_eq!(
                        dim_fade_factor(&attrs, &config),
                        1.0,
                        "marking {intensity:?} text changes nothing ({case})"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod colour_lift_test {
    use super::resolve_fg_color_attr;
    use ::window::color::LinearRgba;
    use config::{BoldBrightening, Config, TextStyle};
    use wezterm_term::color::{AnsiColor, ColorAttribute, ColorPalette};
    use wezterm_term::{CellAttributes, Intensity};

    /// `AnsiColor::Red` is index 9, the bright one.
    const STANDARD_PALETTE_RED: u8 = AnsiColor::Maroon as u8;

    fn config(track_separately: bool, brightening: BoldBrightening) -> Config {
        let mut config = Config::default_config();
        config.track_bold_and_dim_separately = track_separately;
        config.bold_brightens_ansi_colors = brightening;
        config
    }

    fn both_attributes(dim_last: bool) -> CellAttributes {
        let mut attrs = CellAttributes::default();
        if dim_last {
            attrs.apply_sgr_intensity(Intensity::Bold);
            attrs.apply_sgr_intensity(Intensity::Half);
        } else {
            attrs.apply_sgr_intensity(Intensity::Half);
            attrs.apply_sgr_intensity(Intensity::Bold);
        }
        attrs
    }

    fn cell(intensity: Intensity) -> CellAttributes {
        let mut attrs = CellAttributes::default();
        attrs.set_intensity(intensity);
        attrs
    }

    fn resolved(config: &Config, attrs: &CellAttributes, idx: u8) -> LinearRgba {
        resolve_fg_color_attr(
            attrs,
            ColorAttribute::PaletteIndex(idx),
            &ColorPalette::default(),
            config,
            &TextStyle::default(),
        )
    }

    fn palette_colour(idx: u8) -> LinearRgba {
        ColorPalette::default()
            .resolve_fg(ColorAttribute::PaletteIndex(idx))
            .to_linear()
    }

    #[test]
    fn a_lifted_colour_differs_from_its_unlifted_one() {
        assert_ne!(
            palette_colour(STANDARD_PALETTE_RED),
            palette_colour(STANDARD_PALETTE_RED + 8)
        );
        assert_ne!(palette_colour(7), palette_colour(15));
    }

    #[test]
    fn the_lift_follows_the_bold_record_under_separate_tracking() {
        for brightening in [BoldBrightening::BrightAndBold, BoldBrightening::BrightOnly] {
            let config = config(true, brightening);
            for dim_last in [true, false] {
                assert_eq!(
                    resolved(&config, &both_attributes(dim_last), STANDARD_PALETTE_RED),
                    palette_colour(STANDARD_PALETTE_RED + 8),
                    "separate tracking lifts in either order \
                     (dim_last = {dim_last}, {brightening:?})"
                );
            }
        }

        let unified = config(false, BoldBrightening::BrightAndBold);
        assert_eq!(
            resolved(&unified, &both_attributes(true), STANDARD_PALETTE_RED),
            palette_colour(STANDARD_PALETTE_RED),
            "dim arrived last, so unified tracking does not call this bold"
        );
        assert_eq!(
            resolved(&unified, &both_attributes(false), STANDARD_PALETTE_RED),
            palette_colour(STANDARD_PALETTE_RED + 8),
            "and bold arriving last is what lifts it there"
        );
    }

    #[test]
    fn dim_alone_is_never_lifted() {
        for track_separately in [false, true] {
            let config = config(track_separately, BoldBrightening::BrightAndBold);
            assert_eq!(
                resolved(&config, &cell(Intensity::Half), STANDARD_PALETTE_RED),
                palette_colour(STANDARD_PALETTE_RED),
                "separate = {track_separately}"
            );
            assert_eq!(
                resolved(&config, &cell(Intensity::Normal), STANDARD_PALETTE_RED),
                palette_colour(STANDARD_PALETTE_RED),
                "and plain text likewise (separate = {track_separately})"
            );
        }
    }

    #[test]
    fn the_lift_is_retired_only_by_no() {
        for track_separately in [false, true] {
            let off = config(track_separately, BoldBrightening::No);
            for dim_last in [true, false] {
                assert_eq!(
                    resolved(&off, &both_attributes(dim_last), STANDARD_PALETTE_RED),
                    palette_colour(STANDARD_PALETTE_RED),
                    "separate = {track_separately}, dim_last = {dim_last}"
                );
            }
            assert_eq!(
                resolved(&off, &cell(Intensity::Bold), STANDARD_PALETTE_RED),
                palette_colour(STANDARD_PALETTE_RED),
                "not even plain bold text (separate = {track_separately})"
            );
        }
    }

    #[test]
    fn the_lift_stops_at_palette_index_eight() {
        assert_eq!(AnsiColor::Silver as u8, 7, "last standard-palette colour");
        assert_eq!(AnsiColor::Grey as u8, 8, "first bright-palette colour");

        for track_separately in [false, true] {
            let config = config(track_separately, BoldBrightening::BrightAndBold);
            let bold = cell(Intensity::Bold);
            assert_eq!(
                resolved(&config, &bold, 7),
                palette_colour(15),
                "7 is inside the lift and becomes 7 + 8 \
                 (separate = {track_separately})"
            );
            assert_eq!(
                resolved(&config, &bold, 8),
                palette_colour(8),
                "8 is outside it and is drawn as itself \
                 (separate = {track_separately})"
            );
        }
    }
}
