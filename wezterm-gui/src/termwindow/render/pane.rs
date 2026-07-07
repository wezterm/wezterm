use crate::quad::{HeapQuadAllocator, QuadTrait, TripleLayerQuadAllocator};
use crate::selection::SelectionRange;
use crate::termwindow::box_model::*;
use crate::termwindow::cursortrail::{CursorTrailState, HighlightKind};
use crate::termwindow::render::{
    same_hyperlink, CursorProperties, LineQuadCacheKey, LineQuadCacheValue, LineToEleShapeCacheKey,
    RenderScreenLineParams,
};
use crate::termwindow::{ScrollHit, UIItem, UIItemType};
use ::window::bitmaps::TextureRect;
use ::window::DeadKeyStatus;
use anyhow::Context;
use config::VisualBellTarget;
use mux::pane::{PaneId, WithPaneLines};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::tab::PositionedPane;
use ordered_float::NotNan;
use std::time::Instant;
use wezterm_dynamic::Value;
use wezterm_term::color::{ColorAttribute, ColorPalette};
use termwiz::surface::CursorVisibility;
use wezterm_term::{Line, StableRowIndex};
use window::color::LinearRgba;

impl crate::TermWindow {
    fn paint_pane_box_model(&mut self, pos: &PositionedPane) -> anyhow::Result<()> {
        let computed = self.build_pane(pos)?;
        let mut ui_items = computed.ui_items();
        self.ui_items.append(&mut ui_items);
        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(&computed, gl_state, None)
    }

    pub fn paint_pane(
        &mut self,
        pos: &PositionedPane,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        if self.config.use_box_model_render {
            return self.paint_pane_box_model(pos);
        }

        self.check_for_dirty_lines_and_invalidate_selection(&pos.pane);
        /*
        let zone = {
            let dims = pos.pane.get_dimensions();
            let position = self
                .get_viewport(pos.pane.pane_id())
                .unwrap_or(dims.physical_top);

            let zones = self.get_semantic_zones(&pos.pane);
            let idx = match zones.binary_search_by(|zone| zone.start_y.cmp(&position)) {
                Ok(idx) | Err(idx) => idx,
            };
            let idx = ((idx as isize) - 1).max(0) as usize;
            zones.get(idx).cloned()
        };
        */

        let global_cursor_fg = self.palette().cursor_fg;
        let global_cursor_bg = self.palette().cursor_bg;
        let config = self.config.clone();
        let palette = pos.pane.palette();

        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        let cursor = pos.pane.get_cursor_position();
        let pane_id = pos.pane.pane_id();
        if pos.is_active {
            self.prev_cursor.update(&cursor);
        }
        let current_viewport = self.get_viewport(pane_id);
        let dims = pos.pane.get_dimensions();

        let gl_state = self.render_state.as_ref().unwrap();

        let cursor_border_color = palette.cursor_border.to_linear();
        let foreground = palette.foreground.to_linear();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

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

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let background_rect = {
            // We want to fill out to the edges of the splits
            let (x, width_delta) = if pos.left == 0 {
                (
                    0.,
                    padding_left + border.left.get() as f32 + (cell_width / 2.0),
                )
            } else {
                (
                    padding_left + border.left.get() as f32 - (cell_width / 2.0)
                        + (pos.left as f32 * cell_width),
                    cell_width,
                )
            };

            let (y, height_delta) = if pos.top == 0 {
                (
                    (top_pixel_y - padding_top),
                    padding_top + (cell_height / 2.0),
                )
            } else {
                (
                    top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                    cell_height,
                )
            };
            euclid::rect(
                x,
                y,
                // Go all the way to the right edge if we're right-most
                if pos.left + pos.width >= self.terminal_size.cols as usize {
                    self.dimensions.pixel_width as f32 - x
                } else {
                    (pos.width as f32 * cell_width) + width_delta
                },
                // Go all the way to the bottom if we're bottom-most
                if pos.top + pos.height >= self.terminal_size.rows as usize {
                    self.dimensions.pixel_height as f32 - y
                } else {
                    (pos.height as f32 * cell_height) + height_delta as f32
                },
            )
        };

        if self.window_background.is_empty() {
            // Per-pane, palette-specified background

            let mut quad = self
                .filled_rectangle(
                    layers,
                    0,
                    background_rect,
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity),
                )
                .context("filled_rectangle")?;
            quad.set_hsv(if pos.is_active {
                None
            } else {
                Some(config.inactive_pane_hsb)
            });
        }

        {
            // If the bell is ringing, we draw another background layer over the
            // top of this in the configured bell color
            if let Some(intensity) = self.get_intensity_if_bell_target_ringing(
                &pos.pane,
                &config,
                VisualBellTarget::BackgroundColor,
            ) {
                // target background color
                let LinearRgba(r, g, b, _) = config
                    .resolved_palette
                    .visual_bell
                    .as_deref()
                    .unwrap_or(&palette.foreground)
                    .to_linear();

                let background = if window_is_transparent {
                    // for transparent windows, we fade in the target color
                    // by adjusting its alpha
                    LinearRgba::with_components(r, g, b, intensity)
                } else {
                    // otherwise We'll interpolate between the background color
                    // and the target color
                    let (r1, g1, b1, a) = palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity)
                        .tuple();
                    LinearRgba::with_components(
                        r1 + (r - r1) * intensity,
                        g1 + (g - g1) * intensity,
                        b1 + (b - b1) * intensity,
                        a,
                    )
                };
                log::trace!("bell color is {:?}", background);

                let mut quad = self
                    .filled_rectangle(layers, 0, background_rect, background)
                    .context("filled_rectangle")?;

                quad.set_hsv(if pos.is_active {
                    None
                } else {
                    Some(config.inactive_pane_hsb)
                });
            }
        }

        // TODO: we only have a single scrollbar in a single position.
        // We only update it for the active pane, but we should probably
        // do a per-pane scrollbar.  That will require more extensive
        // changes to ScrollHit, mouse positioning, PositionedPane
        // and tab size calculation.
        if pos.is_active && self.show_scroll_bar {
            let thumb_y_offset = top_bar_height as usize + border.top.get();

            let min_height = self.min_scroll_bar_height();

            let info = ScrollHit::thumb(
                &*pos.pane,
                current_viewport,
                self.dimensions.pixel_height.saturating_sub(
                    thumb_y_offset + border.bottom.get() + bottom_bar_height as usize,
                ),
                min_height as usize,
            );
            let abs_thumb_top = thumb_y_offset + info.top;
            let thumb_size = info.height;
            let color = palette.scrollbar_thumb.to_linear();

            // Adjust the scrollbar thumb position
            let config = &self.config;
            let padding = self.effective_right_padding(&config) as f32;

            let thumb_x = self.dimensions.pixel_width - padding as usize - border.right.get();

            // Register the scroll bar location
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: thumb_y_offset,
                height: info.top,
                item_type: UIItemType::AboveScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top,
                height: thumb_size,
                item_type: UIItemType::ScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top + thumb_size,
                height: self
                    .dimensions
                    .pixel_height
                    .saturating_sub(abs_thumb_top + thumb_size),
                item_type: UIItemType::BelowScrollThumb,
            });

            self.filled_rectangle(
                layers,
                2,
                euclid::rect(
                    thumb_x as f32,
                    abs_thumb_top as f32,
                    padding,
                    thumb_size as f32,
                ),
                color,
            )
            .context("filled_rectangle")?;
        }

        let (selrange, rectangular) = {
            let sel = self.selection(pos.pane.pane_id());
            (sel.range.clone(), sel.rectangular)
        };

        let start = Instant::now();
        let selection_fg = palette.selection_fg.to_linear();
        let selection_bg = palette.selection_bg.to_linear();
        let cursor_fg = palette.cursor_fg.to_linear();
        let cursor_bg = palette.cursor_bg.to_linear();
        let cursor_is_default_color =
            palette.cursor_fg == global_cursor_fg && palette.cursor_bg == global_cursor_bg;

        // Tick the cursor trail / smear state BEFORE line rendering so the
        // line-render path knows whether to suppress the static cursor.
        //
        // We only suppress while a multi-cell jump is being *deferred* for
        // snap-back detection — during that window the cursor has actually
        // moved but we want the user to still see it parked at the pre-jump
        // cell (drawn by the frozen smear quad), so showing the static
        // cursor at its new position would betray the deferred destination.
        //
        // During an ordinary smear animation we deliberately do NOT
        // suppress: the static cursor draws at its target with full
        // cursor_fg / cursor_bg / inversion styling, while the smear quad
        // streaks behind it on layer 0. Both the smear and the cursor cell
        // bg use cursor_bg, so the layered draw is visually seamless and
        // cursor_fg becomes visible the moment the cursor reaches its new
        // cell instead of after the smear has finished.
        let cursor_anim_enabled = pos.is_active
            && (config.cursor_smear
                || config.cursor_trail_style.is_some()
                || config.cursor_animation_duration > 0.0);
        let now = Instant::now();
        let cursor_visible = cursor.visibility == CursorVisibility::Visible;
        let mut suppress_static_cursor = false;
        if cursor_anim_enabled {
            let mut trails = self.cursor_trail_states.borrow_mut();
            let trail = trails.entry(pane_id).or_insert_with(CursorTrailState::new);
            if cursor_visible {
                trail.update(
                    &cursor,
                    cell_width,
                    cell_height,
                    config.cursor_trail_min_distance,
                    config.cursor_trail_style,
                    config.cursor_vfx_particle_density,
                    config.cursor_vfx_particle_lifetime,
                    config.cursor_vfx_particle_speed,
                    config.cursor_smear,
                    config.cursor_animation_duration,
                    config.cursor_trail_size,
                    config.default_cursor_style.effective_shape(cursor.shape),
                    now,
                );
                suppress_static_cursor = trail.is_smear_deferred();
            } else {
                trail.advance_hidden(&cursor);
            }
        }

        {
            let stable_range = match current_viewport {
                Some(top) => top..top + dims.viewport_rows as StableRowIndex,
                None => dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex,
            };

            pos.pane
                .apply_hyperlinks(stable_range.clone(), &self.config.hyperlink_rules);

            struct LineRender<'a, 'b> {
                term_window: &'a mut crate::TermWindow,
                selrange: Option<SelectionRange>,
                rectangular: bool,
                dims: RenderableDimensions,
                top_pixel_y: f32,
                left_pixel_x: f32,
                pos: &'a PositionedPane,
                pane_id: PaneId,
                cursor: &'a StableCursorPosition,
                palette: &'a ColorPalette,
                default_bg: LinearRgba,
                cursor_border_color: LinearRgba,
                selection_fg: LinearRgba,
                selection_bg: LinearRgba,
                cursor_fg: LinearRgba,
                cursor_bg: LinearRgba,
                foreground: LinearRgba,
                cursor_is_default_color: bool,
                white_space: TextureRect,
                filled_box: TextureRect,
                window_is_transparent: bool,
                layers: &'a mut TripleLayerQuadAllocator<'b>,
                error: Option<anyhow::Error>,
                /// When true the cursor is omitted from line rendering and drawn
                /// separately as an animated overlay in `paint_animated_cursor`.
                suppress_cursor: bool,
            }

            let left_pixel_x = padding_left
                + border.left.get() as f32
                + (pos.left as f32 * self.render_metrics.cell_size.width as f32);

            let mut render = LineRender {
                term_window: self,
                selrange,
                rectangular,
                dims,
                top_pixel_y,
                left_pixel_x,
                pos,
                pane_id,
                cursor: &cursor,
                palette: &palette,
                cursor_border_color,
                selection_fg,
                selection_bg,
                cursor_fg,
                default_bg,
                cursor_bg,
                foreground,
                cursor_is_default_color,
                white_space,
                filled_box,
                window_is_transparent,
                layers,
                error: None,
                // Suppress the static cursor only while the smear is actively
                // animating or deferred — at rest, the line render path is the
                // only place that applies cursor_fg / cursor_bg / inversion to
                // the glyph beneath the cursor, so unconditional suppression
                // would silently break those palette colours.
                suppress_cursor: suppress_static_cursor,
            };

            impl<'a, 'b> LineRender<'a, 'b> {
                fn render_line(
                    &mut self,
                    stable_top: StableRowIndex,
                    line_idx: usize,
                    line: &&mut Line,
                ) -> anyhow::Result<()> {
                    let stable_row = stable_top + line_idx as StableRowIndex;
                    let selrange = self
                        .selrange
                        .map_or(0..0, |sel| sel.cols_for_row(stable_row, self.rectangular));
                    // Constrain to the pane width!
                    let selrange = selrange.start..selrange.end.min(self.dims.cols);

                    let (cursor, composing, password_input) = if self.cursor.y == stable_row
                        && !self.suppress_cursor
                    {
                        (
                            Some(CursorProperties {
                                position: StableCursorPosition {
                                    y: 0,
                                    ..*self.cursor
                                },
                                dead_key_or_leader: self.term_window.dead_key_status
                                    != DeadKeyStatus::None
                                    || self.term_window.leader_is_active(),
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_border_color: self.cursor_border_color,
                                cursor_is_default_color: self.cursor_is_default_color,
                            }),
                            match (self.pos.is_active, &self.term_window.dead_key_status) {
                                (true, DeadKeyStatus::Composing(composing)) => {
                                    Some(composing.to_string())
                                }
                                _ => None,
                            },
                            if self.term_window.config.detect_password_input {
                                match self.pos.pane.get_metadata() {
                                    Value::Object(obj) => {
                                        match obj.get(&Value::String("password_input".to_string()))
                                        {
                                            Some(Value::Bool(b)) => *b,
                                            _ => false,
                                        }
                                    }
                                    _ => false,
                                }
                            } else {
                                false
                            },
                        )
                    } else {
                        (None, None, false)
                    };

                    let shape_hash = self.term_window.shape_hash_for_line(line);

                    // When cursor animation is active, keep the cursor in the
                    // cache key so the line caches and reuses its quads across
                    // frames while the cursor sits still.  The animated overlay
                    // drawn by paint_animated_cursor covers the static cursor rect.
                    let cache_cursor = cursor;

                    let quad_key = LineQuadCacheKey {
                        pane_id: self.pane_id,
                        password_input,
                        pane_is_active: self.pos.is_active,
                        config_generation: self.term_window.config.generation(),
                        shape_generation: self.term_window.shape_generation,
                        quad_generation: self.term_window.quad_generation,
                        composing: composing.clone(),
                        selection: selrange.clone(),
                        cursor: cache_cursor,
                        shape_hash,
                        top_pixel_y: NotNan::new(self.top_pixel_y).unwrap()
                            + (line_idx + self.pos.top) as f32
                                * self.term_window.render_metrics.cell_size.height as f32,
                        left_pixel_x: NotNan::new(self.left_pixel_x).unwrap(),
                        phys_line_idx: line_idx,
                        reverse_video: self.dims.reverse_video,
                    };

                    if let Some(cached_quad) =
                        self.term_window.line_quad_cache.borrow_mut().get(&quad_key)
                    {
                        let expired = cached_quad
                            .expires
                            .map(|i| Instant::now() >= i)
                            .unwrap_or(false);
                        let hover_changed = if cached_quad.invalidate_on_hover_change {
                            !same_hyperlink(
                                cached_quad.current_highlight.as_ref(),
                                self.term_window.current_highlight.as_ref(),
                            )
                        } else {
                            false
                        };
                        if !expired && !hover_changed {
                            cached_quad
                                .layers
                                .apply_to(self.layers)
                                .context("cached_quad.layers.apply_to")?;
                            self.term_window.update_next_frame_time(cached_quad.expires);
                            return Ok(());
                        }
                    }

                    let mut buf = HeapQuadAllocator::default();
                    let next_due = self.term_window.has_animation.borrow_mut().take();

                    let shape_key = LineToEleShapeCacheKey {
                        shape_hash,
                        shape_generation: quad_key.shape_generation,
                        composing: if self.cursor.y == stable_row && self.pos.is_active {
                            if let DeadKeyStatus::Composing(composing) =
                                &self.term_window.dead_key_status
                            {
                                Some((self.cursor.x, composing.to_string()))
                            } else {
                                None
                            }
                        } else {
                            None
                        },
                    };

                    // Sentinel cursor that won't match any visible row,
                    // used to suppress static cursor drawing when animating.
                    let suppress_sentinel;
                    let render_cursor: &StableCursorPosition = if self.suppress_cursor {
                        suppress_sentinel = StableCursorPosition {
                            y: StableRowIndex::MIN,
                            ..*self.cursor
                        };
                        &suppress_sentinel
                    } else {
                        self.cursor
                    };

                    let render_result = self
                        .term_window
                        .render_screen_line(
                            RenderScreenLineParams {
                                top_pixel_y: *quad_key.top_pixel_y,
                                left_pixel_x: self.left_pixel_x,
                                pixel_width: self.dims.cols as f32
                                    * self.term_window.render_metrics.cell_size.width as f32,
                                stable_line_idx: Some(stable_row),
                                line: &line,
                                selection: selrange.clone(),
                                cursor: render_cursor,
                                palette: &self.palette,
                                dims: &self.dims,
                                config: &self.term_window.config,
                                cursor_border_color: self.cursor_border_color,
                                foreground: self.foreground,
                                is_active: self.pos.is_active,
                                pane: Some(&self.pos.pane),
                                selection_fg: self.selection_fg,
                                selection_bg: self.selection_bg,
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_is_default_color: self.cursor_is_default_color,
                                white_space: self.white_space,
                                filled_box: self.filled_box,
                                window_is_transparent: self.window_is_transparent,
                                default_bg: self.default_bg,
                                font: None,
                                style: None,
                                use_pixel_positioning: self
                                    .term_window
                                    .config
                                    .experimental_pixel_positioning,
                                render_metrics: self.term_window.render_metrics,
                                shape_key: Some(shape_key),
                                password_input,
                            },
                            &mut TripleLayerQuadAllocator::Heap(&mut buf),
                        )
                        .context("render_screen_line")?;

                    let expires = self.term_window.has_animation.borrow().as_ref().cloned();
                    self.term_window.update_next_frame_time(next_due);

                    buf.apply_to(self.layers)
                        .context("HeapQuadAllocator::apply_to")?;

                    let quad_value = LineQuadCacheValue {
                        layers: buf,
                        expires,
                        invalidate_on_hover_change: render_result.invalidate_on_hover_change,
                        current_highlight: if render_result.invalidate_on_hover_change {
                            self.term_window.current_highlight.clone()
                        } else {
                            None
                        },
                    };

                    self.term_window
                        .line_quad_cache
                        .borrow_mut()
                        .put(quad_key, quad_value);

                    Ok(())
                }
            }

            impl<'a, 'b> WithPaneLines for LineRender<'a, 'b> {
                fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                    for (line_idx, line) in lines.iter().enumerate() {
                        if let Err(err) = self.render_line(stable_top, line_idx, line) {
                            self.error.replace(err);
                            return;
                        }
                    }
                }
            }

            pos.pane.with_lines_mut(stable_range.clone(), &mut render);
            if let Some(error) = render.error.take() {
                return Err(error).context("error while calling with_lines_mut");
            }
        }

        if cursor_anim_enabled {
            // Trail state was already ticked above (before line rendering)
            // so we could decide on suppress_static_cursor for the cache key.
            // Here we only paint based on that already-up-to-date state.
            if cursor_visible {
                let trail_states = self.cursor_trail_states.borrow();
                let trail_state = match trail_states.get(&pane_id) {
                    Some(s) => s,
                    None => return Ok(()),
                };

                if config.cursor_trail_style.is_some() {
                    self.paint_cursor_trail(
                        layers,
                        pos,
                        &cursor,
                        &dims,
                        current_viewport,
                        top_pixel_y,
                        padding_left,
                        border.clone(),
                        &palette,
                        trail_state,
                        now,
                    )?;
                }

                self.paint_animated_cursor(
                    layers,
                    pos,
                    &cursor,
                    &dims,
                    current_viewport,
                    top_pixel_y,
                    padding_left,
                    border,
                    &palette,
                    trail_state,
                    now,
                )?;
            }
        }

        /*
        if let Some(zone) = zone {
            // TODO: render a thingy to jump to prior prompt
        }
        */
        metrics::histogram!("paint_pane.lines").record(start.elapsed());
        log::trace!("lines elapsed {:?}", start.elapsed());

        Ok(())
    }

    pub fn build_pane(&mut self, pos: &PositionedPane) -> anyhow::Result<ComputedElement> {
        // First compute the bounds for the pane background

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()?
        } else {
            0.
        };
        let (top_bar_height, _bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        // We want to fill out to the edges of the splits
        let (x, width_delta) = if pos.left == 0 {
            (
                0.,
                padding_left + border.left.get() as f32 + (cell_width / 2.0),
            )
        } else {
            (
                padding_left + border.left.get() as f32 - (cell_width / 2.0)
                    + (pos.left as f32 * cell_width),
                cell_width,
            )
        };

        let (y, height_delta) = if pos.top == 0 {
            (
                (top_pixel_y - padding_top),
                padding_top + (cell_height / 2.0),
            )
        } else {
            (
                top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                cell_height,
            )
        };

        let background_rect = euclid::rect(
            x,
            y,
            // Go all the way to the right edge if we're right-most
            if pos.left + pos.width >= self.terminal_size.cols as usize {
                self.dimensions.pixel_width as f32 - x
            } else {
                (pos.width as f32 * cell_width) + width_delta
            },
            // Go all the way to the bottom if we're bottom-most
            if pos.top + pos.height >= self.terminal_size.rows as usize {
                self.dimensions.pixel_height as f32 - y
            } else {
                (pos.height as f32 * cell_height) + height_delta as f32
            },
        );

        // Bounds for the terminal cells
        let content_rect = euclid::rect(
            padding_left + border.left.get() as f32 - (cell_width / 2.0)
                + (pos.left as f32 * cell_width),
            top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
            pos.width as f32 * cell_width,
            pos.height as f32 * cell_height,
        );

        let palette = pos.pane.palette();

        // TODO: visual bell background layer
        // TODO: scrollbar

        Ok(ComputedElement {
            item_type: None,
            zindex: 0,
            bounds: background_rect,
            border: PixelDimension::default(),
            border_rect: background_rect,
            border_corners: None,
            colors: ElementColors {
                border: BorderColor::default(),
                bg: if self.window_background.is_empty() {
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(self.config.window_background_opacity)
                        .into()
                } else {
                    InheritableColor::Inherited
                },
                text: InheritableColor::Inherited,
            },
            hover_colors: None,
            padding: background_rect,
            content_rect,
            baseline: 1.0,
            content: ComputedElementContent::Children(vec![]),
        })
    }

    fn paint_cursor_trail(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        pos: &PositionedPane,
        cursor: &StableCursorPosition,
        dims: &RenderableDimensions,
        current_viewport: Option<StableRowIndex>,
        top_pixel_y: f32,
        padding_left: f32,
        border: window::parameters::Border,
        palette: &ColorPalette,
        trail_state: &CursorTrailState,
        now: Instant,
    ) -> anyhow::Result<()> {
        let config = &self.config;

        if !trail_state.has_active_animation() {
            return Ok(());
        }

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        let viewport_top: StableRowIndex = current_viewport.unwrap_or(dims.physical_top);
        let viewport_top_px = viewport_top as f32 * cell_height;
        let viewport_h_px = dims.viewport_rows as f32 * cell_height;

        let pane_left = pos.left as f32 * cell_width;
        let pane_top = pos.top as f32 * cell_height;
        let left_offset = padding_left + border.left.get() as f32;

        let cursor_shape = config.default_cursor_style.effective_shape(cursor.shape);
        let trail_color = if config.force_reverse_video_cursor {
            palette.foreground.to_linear()
        } else {
            palette.cursor_bg.to_linear()
        };
        let (r, g, b, _) = trail_color.tuple();
        let start_opacity = config.cursor_vfx_opacity;

        let layer_num = match cursor_shape {
            termwiz::surface::CursorShape::BlinkingBar
            | termwiz::surface::CursorShape::SteadyBar => 2,
            _ => 0,
        };

        // ── Particles (Railgun / Torpedo / PixieDust) ─────────────────────────
        // Batch all visible particle quads into a single vertex buffer submission
        // to avoid per-particle allocate() overhead (up to 256 draw calls → 1).
        {
            use crate::quad::{TripleLayerQuadAllocatorTrait, Vertex, VERTICES_PER_CELL};

            let half_win_w = self.dimensions.pixel_width as f32 / 2.0;
            let half_win_h = self.dimensions.pixel_height as f32 / 2.0;
            let particle_size = config.cursor_vfx_particle_size;

            let (has_color, mix_val, is_ring) = match config.cursor_trail_style {
                Some(config::CursorTrailStyle::Railgun)
                | Some(config::CursorTrailStyle::Torpedo) => (5.0f32, 0.5f32, true),
                _ => (3.0f32, 0.0f32, false),
            };

            let gl_state = self.render_state.as_ref();
            let (tx1, tx2, ty1, ty2) = if is_ring {
                (0.0f32, 1.0f32, 0.0f32, 1.0f32)
            } else if let Some(gs) = gl_state {
                let tex = gs.util_sprites.filled_box.texture_coords();
                (tex.min_x(), tex.max_x(), tex.min_y(), tex.max_y())
            } else {
                (0.0, 1.0, 0.0, 1.0)
            };

            // ── Hoist frame-constant values out of the per-particle loop ──────
            // screen_base_{x,y}: common screen-space offset, computed once.
            let screen_base_x = left_offset + pane_left;
            // viewport_top_px is already folded in so the loop only adds p.y.
            let screen_base_y = top_pixel_y + pane_top - viewport_top_px;
            // Viewport culling bounds (particle centre coordinates, not clip).
            let y_cull_min = top_pixel_y + pane_top - cell_height;
            let y_cull_max = top_pixel_y + pane_top + viewport_h_px;
            // For PixieDust size is independent of life_frac; precompute both paths.
            let is_pixie_dust =
                matches!(config.cursor_trail_style, Some(config::CursorTrailStyle::PixieDust));
            // particle_half_base = cell_width * particle_size * 0.5;
            // For non-PixieDust: half = particle_half_base * life_frac
            // For PixieDust:     half = pixie_half (constant per frame, min 2px so
            //                    particles remain visible at small cell sizes)
            let particle_half_base = cell_width * particle_size * 0.5;
            let pixie_half = particle_half_base.max(2.0);

            // ── Thread-local scratch buffer: zero heap allocation per frame ───
            // The Vec grows to the high-water mark and is reused across frames,
            // eliminating the malloc/free pair that previously occurred every
            // animation frame (60 Hz × ~27 KB for a full 256-particle batch).
            thread_local! {
                static PARTICLE_VERTS: std::cell::RefCell<Vec<Vertex>> =
                    std::cell::RefCell::new(Vec::new());
            }

            PARTICLE_VERTS.with(|scratch| {
                let mut batch = scratch.borrow_mut();
                batch.clear();
                batch.reserve(trail_state.particles.len() * VERTICES_PER_CELL);

                // hsv is identical for every particle; hoist it out of the loop.
                const HSV: [f32; 3] = [1.0, 1.0, 1.0];

                for p in &trail_state.particles {
                    let life_frac = p.lifetime / p.max_lifetime;
                    let alpha = start_opacity * life_frac;
                    if alpha <= 0.005 {
                        continue;
                    }

                    let screen_x = screen_base_x + p.x;
                    let screen_y = screen_base_y + p.y;

                    if screen_y < y_cull_min || screen_y > y_cull_max {
                        continue;
                    }

                    let col = [r, g, b, alpha];

                    let half = if is_pixie_dust {
                        pixie_half
                    } else {
                        particle_half_base * life_frac
                    };

                    let mk = |px: f32, py: f32, u: f32, v: f32| Vertex {
                        position: [px, py],
                        tex: [u, v],
                        fg_color: col,
                        alt_color: col,
                        hsv: HSV,
                        has_color,
                        mix_value: mix_val,
                    };

                    batch.push(mk(
                        screen_x - half - half_win_w,
                        screen_y - half - half_win_h,
                        tx1,
                        ty1,
                    ));
                    batch.push(mk(
                        screen_x + half - half_win_w,
                        screen_y - half - half_win_h,
                        tx2,
                        ty1,
                    ));
                    batch.push(mk(
                        screen_x - half - half_win_w,
                        screen_y + half - half_win_h,
                        tx1,
                        ty2,
                    ));
                    batch.push(mk(
                        screen_x + half - half_win_w,
                        screen_y + half - half_win_h,
                        tx2,
                        ty2,
                    ));
                }

                if !batch.is_empty() {
                    layers.extend_with(layer_num, &batch);
                }
            });
        }

        // ── Highlight (SonicBoom / Ripple / Wireframe) ───────────────────────
        if let Some(ref h) = trail_state.highlight {
            match &h.kind {
                // ── SonicBoom: expanding filled square ────────────────────────
                HighlightKind::SonicBoom { cx, cy } => {
                    let t = h.t;
                    let opacity = start_opacity * (1.0 - t).powi(2);
                    if opacity > 0.005 {
                        let radius = t * cell_height * 1.5;
                        let d = radius * 2.0;
                        let screen_cx = left_offset + pane_left + cx;
                        let screen_cy = top_pixel_y + pane_top + (cy - viewport_top_px);
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(screen_cx - radius, screen_cy - radius, d, d),
                            LinearRgba::with_components(r, g, b, opacity),
                        )?;
                    }
                }

                // ── Ripple: expanding hollow ring (4 thin rectangles) ─────────
                HighlightKind::Ripple { cx, cy } => {
                    let t = h.t;
                    let opacity = start_opacity * (1.0 - t);
                    if opacity > 0.005 {
                        let radius = t * cell_height * 2.5;
                        let bw = 2.5_f32;
                        let d = radius * 2.0;
                        let screen_cx = left_offset + pane_left + cx;
                        let screen_cy = top_pixel_y + pane_top + (cy - viewport_top_px);
                        let x0 = screen_cx - radius;
                        let y0 = screen_cy - radius;
                        let color = LinearRgba::with_components(r, g, b, opacity);
                        // top bar
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0, d, bw),
                            color,
                        )?;
                        // bottom bar
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0 + d - bw, d, bw),
                            color,
                        )?;
                        // left side
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0 + bw, bw, d - 2.0 * bw),
                            color,
                        )?;
                        // right side
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0 + d - bw, y0 + bw, bw, d - 2.0 * bw),
                            color,
                        )?;
                    }
                }

                // ── Wireframe: expanding hollow rectangle (cell aspect ratio) ─
                HighlightKind::Wireframe {
                    cx,
                    cy,
                    cell_w,
                    cell_h,
                } => {
                    let t = h.t;
                    let opacity = start_opacity * (1.0 - t);
                    if opacity > 0.005 {
                        let scale = t * 2.5;
                        let rx = cell_w * scale;
                        let ry = cell_h * scale;
                        let bw = 2.5_f32;
                        let w = rx * 2.0;
                        let h = ry * 2.0;
                        let screen_cx = left_offset + pane_left + cx;
                        let screen_cy = top_pixel_y + pane_top + (cy - viewport_top_px);
                        let x0 = screen_cx - rx;
                        let y0 = screen_cy - ry;
                        let color = LinearRgba::with_components(r, g, b, opacity);
                        // top bar
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0, w, bw),
                            color,
                        )?;
                        // bottom bar
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0 + h - bw, w, bw),
                            color,
                        )?;
                        // left side
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0, y0 + bw, bw, h - 2.0 * bw),
                            color,
                        )?;
                        // right side
                        self.filled_rectangle(
                            layers,
                            layer_num,
                            euclid::rect(x0 + w - bw, y0 + bw, bw, h - 2.0 * bw),
                            color,
                        )?;
                    }
                }
            }
        }

        // Schedule the next repaint immediately; the presentation engine
        // (PresentMode::Fifo) will vsync to the actual display refresh rate.
        // has_active_animation() was confirmed true at the top of this function
        // (early-return path). Nothing mutates trail_state during paint, so it
        // remains true here — call unconditionally to avoid a redundant check.
        self.update_next_frame_time(Some(now));

        Ok(())
    }

    /// Draws the cursor at its target position, rendering the Neovide-style
    /// deforming smear body when `cursor_smear` is active.
    fn paint_animated_cursor(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        pos: &PositionedPane,
        cursor: &StableCursorPosition,
        dims: &RenderableDimensions,
        current_viewport: Option<StableRowIndex>,
        top_pixel_y: f32,
        padding_left: f32,
        border: window::parameters::Border,
        palette: &ColorPalette,
        trail_state: &CursorTrailState,
        now: Instant,
    ) -> anyhow::Result<()> {
        let config = &self.config;

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        let viewport_top_px = current_viewport.unwrap_or(dims.physical_top) as f32 * cell_height;
        let pane_left = pos.left as f32 * cell_width;
        let pane_top = pos.top as f32 * cell_height;
        let left_offset = padding_left + border.left.get() as f32;

        let cursor_color = if config.force_reverse_video_cursor {
            palette.foreground.to_linear()
        } else {
            palette.cursor_bg.to_linear()
        };

        let shape = config.default_cursor_style.effective_shape(cursor.shape);

        // Apply cursor blink if the shape is a blinking variant.
        let is_blinking = matches!(
            shape,
            termwiz::surface::CursorShape::BlinkingBlock
                | termwiz::surface::CursorShape::BlinkingBar
                | termwiz::surface::CursorShape::BlinkingUnderline
        ) && config.cursor_blink_rate != 0
            && self.focused.is_some()
            && pos.is_active;

        let cursor_color = if is_blinking {
            let mut color_ease = self.cursor_blink_state.borrow_mut();
            color_ease.update_start(self.prev_cursor.last_cursor_movement());
            let (intensity, next) = color_ease.intensity_continuous();
            self.update_next_frame_time(Some(next));
            // intensity=0 → fully visible, intensity=1 → fully invisible.
            // Skip drawing entirely when invisible so the cursor truly disappears
            // (drawing a zero-alpha rect on a solid background shows nothing, but
            // smear / trail draws underneath it would still be visible otherwise).
            if intensity >= 1.0 {
                return Ok(());
            }
            let (r, g, b, _a) = cursor_color.tuple();
            LinearRgba::with_components(r, g, b, 1.0 - intensity)
        } else {
            cursor_color
        };

        // Logical (target) cursor position in stable pixel space.
        let target_x = cursor.x as f32 * cell_width;
        let target_y = cursor.y as f32 * cell_height;

        // Screen offset shared by both visual and target positions.
        let screen_off_x = left_offset + pane_left;
        let screen_off_y = top_pixel_y + pane_top - viewport_top_px;

        // ── Neovide-style deforming smear (all shapes) ───────────────────────
        // Four corners animate at different speeds — leading edge races ahead,
        // trailing edge lags — producing the characteristic stretched-body look.
        // Corner targets are shape-adjusted (bar compressed, underline at bottom)
        // in the trail state so the animated positions are already correct.
        let smear_active = config.cursor_smear
            && trail_state.has_smear_animation(target_x, target_y, cell_width, cell_height);

        if smear_active {
            let c = &trail_state.smear_corners;
            let tl = (screen_off_x + c[0].visual_x, screen_off_y + c[0].visual_y);
            let tr = (screen_off_x + c[1].visual_x, screen_off_y + c[1].visual_y);
            let br = (screen_off_x + c[2].visual_x, screen_off_y + c[2].visual_y);
            let bl = (screen_off_x + c[3].visual_x, screen_off_y + c[3].visual_y);

            let corner_alphas = if config.cursor_smear_gradient {
                let (_, _, _, base_a) = cursor_color.tuple();
                let offsets = trail_state.corner_offsets();
                let max_dist = cell_width.hypot(cell_height);
                Some(std::array::from_fn::<f32, 4, _>(|i| {
                    let (ox, oy) = offsets[i];
                    let tx = target_x + ox * cell_width;
                    let ty = target_y + oy * cell_height;
                    let dist = (c[i].visual_x - tx).hypot(c[i].visual_y - ty);
                    let lag = (dist / max_dist).min(1.0);
                    base_a * (1.0 - lag)
                }))
            } else {
                None
            };

            self.draw_cursor_deformed_quad(layers, 0, tl, tr, br, bl, cursor_color, corner_alphas);
            self.update_next_frame_time(Some(now));
        }

        // No cap rect: the static cursor drawn by the line render path is
        // the canonical cursor draw at the target cell and already applies
        // cursor_fg / cursor_bg / inversion to the underlying glyph. The
        // smear quad streaks behind it on layer 0 using the same cursor_bg
        // colour, so the head reads as a clean cursor cell with a trailing
        // smear and cursor_fg is visible immediately on arrival rather than
        // after the smear finishes.
        //
        // While a multi-cell jump is deferred, the static cursor is
        // suppressed in line rendering (see suppress_static_cursor in
        // paint_pane) so the frozen smear quad alone forms the visible
        // cursor block at the pre-jump cell.

        Ok(())
    }
}
