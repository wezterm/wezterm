use crate::termwindow::box_model::*;
use crate::termwindow::{DimensionContext, TermWindow, UIItemType};
use ::window::RectF;
use config::Dimension;
use mux::pane::PaneId;
use mux::tab::{PositionedPane, SplitDirection, SplitRequest, SplitSize};
use window::color::LinearRgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneDropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Debug)]
pub struct PaneDropTarget {
    pub src_pane_id: PaneId,
    pub target_pane_id: PaneId,
    pub edge: PaneDropEdge,
    pub highlight_rect: RectF,
}

pub fn compute_drop_edge(local_x: f32, local_y: f32, pane_rect: RectF) -> PaneDropEdge {
    let w = pane_rect.width().max(1.0);
    let h = pane_rect.height().max(1.0);
    let nx = ((local_x - pane_rect.min_x()) / w).clamp(0.0, 1.0);
    let ny = ((local_y - pane_rect.min_y()) / h).clamp(0.0, 1.0);

    let d_left = nx;
    let d_right = 1.0 - nx;
    let d_top = ny;
    let d_bottom = 1.0 - ny;

    let min_d = d_left.min(d_right).min(d_top).min(d_bottom);
    if (min_d - d_left).abs() < f32::EPSILON {
        PaneDropEdge::Left
    } else if (min_d - d_right).abs() < f32::EPSILON {
        PaneDropEdge::Right
    } else if (min_d - d_top).abs() < f32::EPSILON {
        PaneDropEdge::Top
    } else {
        PaneDropEdge::Bottom
    }
}

pub fn compute_drop_highlight_rect(pane_rect: RectF, edge: PaneDropEdge) -> RectF {
    let half_w = pane_rect.width() / 2.0;
    let half_h = pane_rect.height() / 2.0;
    match edge {
        PaneDropEdge::Left => euclid::rect(pane_rect.min_x(), pane_rect.min_y(), half_w, pane_rect.height()),
        PaneDropEdge::Right => euclid::rect(
            pane_rect.min_x() + half_w,
            pane_rect.min_y(),
            half_w,
            pane_rect.height(),
        ),
        PaneDropEdge::Top => euclid::rect(pane_rect.min_x(), pane_rect.min_y(), pane_rect.width(), half_h),
        PaneDropEdge::Bottom => euclid::rect(
            pane_rect.min_x(),
            pane_rect.min_y() + half_h,
            pane_rect.width(),
            half_h,
        ),
    }
}

pub fn drop_edge_to_split_request(edge: PaneDropEdge) -> SplitRequest {
    let (direction, target_is_second) = match edge {
        PaneDropEdge::Left => (SplitDirection::Horizontal, false),
        PaneDropEdge::Right => (SplitDirection::Horizontal, true),
        PaneDropEdge::Top => (SplitDirection::Vertical, false),
        PaneDropEdge::Bottom => (SplitDirection::Vertical, true),
    };
    SplitRequest {
        direction,
        target_is_second,
        top_level: false,
        size: SplitSize::Percent(50),
    }
}

fn distance_sq_to_rect(x: f32, y: f32, rect: RectF) -> f32 {
    let cx = x.clamp(rect.min_x(), rect.max_x());
    let cy = y.clamp(rect.min_y(), rect.max_y());
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy
}

impl TermWindow {
    pub fn pane_title_bar_rows(&self) -> usize {
        if self.config.show_pane_title_bar {
            1
        } else {
            0
        }
    }

    pub fn pane_title_bar_pixel_height(&self) -> f32 {
        if self.config.show_pane_title_bar {
            self.render_metrics.cell_size.height as f32
        } else {
            0.0
        }
    }

    pub fn pane_pixel_rect(&self, pos: &PositionedPane) -> RectF {
        let border = self.get_os_border();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.0)
        } else {
            0.0
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.0
        } else {
            tab_bar_height
        };
        let (padding_left, padding_top) = self.padding_left_top();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        euclid::rect(
            padding_left + border.left.get() as f32 + pos.left as f32 * cell_width,
            top_pixel_y + pos.top as f32 * cell_height,
            pos.width as f32 * cell_width,
            pos.height as f32 * cell_height,
        )
    }

    pub fn resolve_pane_drop_target(
        &self,
        src_pane_id: PaneId,
        local_x: f32,
        local_y: f32,
    ) -> Option<PaneDropTarget> {
        let panes = self.get_panes_to_render();
        if panes.is_empty() {
            return None;
        }

        let pt = euclid::point2(local_x, local_y);
        let best = panes
            .iter()
            .map(|pos| (pos, self.pane_pixel_rect(pos)))
            .find(|(_, rect)| rect.contains(pt))
            .or_else(|| {
                panes
                    .iter()
                    .map(|pos| (pos, self.pane_pixel_rect(pos)))
                    .min_by(|(_, a), (_, b)| {
                        distance_sq_to_rect(local_x, local_y, *a)
                            .partial_cmp(&distance_sq_to_rect(local_x, local_y, *b))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            })?;

        let target_pane_id = best.0.pane.pane_id();
        if target_pane_id == src_pane_id {
            return None;
        }

        let edge = compute_drop_edge(local_x, local_y, best.1);
        let highlight_rect = compute_drop_highlight_rect(best.1, edge);
        Some(PaneDropTarget {
            src_pane_id,
            target_pane_id,
            edge,
            highlight_rect,
        })
    }

    pub fn paint_pane_title_bars(&mut self, panes: &[PositionedPane]) -> anyhow::Result<()> {
        if !self.config.show_pane_title_bar {
            return Ok(());
        }

        let bar_height = self.pane_title_bar_pixel_height();
        if bar_height <= 0.0 {
            return Ok(());
        }

        let font = self.fonts.default_font()?;
        let metrics = self.render_metrics;
        let dimensions = self.dimensions;
        let gl_state = self.render_state.as_ref().unwrap();

        for pos in panes {
            let rect = self.pane_pixel_rect(pos);
            let pane_id = pos.pane.pane_id();
            let title = pos.pane.get_title();
            let label = format!(" :: {title}");

            let (bg, text) = if pos.is_active {
                (
                    LinearRgba::with_components(0.18, 0.27, 0.38, 1.0),
                    LinearRgba::with_components(0.94, 0.96, 1.0, 1.0),
                )
            } else {
                (
                    LinearRgba::with_components(0.12, 0.13, 0.16, 1.0),
                    LinearRgba::with_components(0.65, 0.68, 0.72, 1.0),
                )
            };

            let hover_colors = ElementColors {
                border: BorderColor::new(LinearRgba::with_components(0.38, 0.56, 0.80, 1.0).into()),
                bg: LinearRgba::with_components(0.24, 0.36, 0.52, 1.0).into(),
                text: LinearRgba::with_components(1.0, 1.0, 1.0, 1.0).into(),
            };

            let element = Element::new(&font, ElementContent::Text(label))
                .item_type(UIItemType::PaneTitleBar(pane_id))
                .colors(ElementColors {
                    border: BorderColor::new(
                        LinearRgba::with_components(0.26, 0.30, 0.36, 1.0).into(),
                    ),
                    bg: bg.into(),
                    text: text.into(),
                })
                .hover_colors(Some(hover_colors))
                .border(BoxDimension {
                    left: Dimension::Pixels(0.0),
                    right: Dimension::Pixels(0.0),
                    top: Dimension::Pixels(0.0),
                    bottom: Dimension::Pixels(1.0),
                })
                .min_width(Some(Dimension::Pixels(rect.width())))
                .max_width(Some(Dimension::Pixels(rect.width())));

            let mut computed = self.compute_element(
                &LayoutContext {
                    height: DimensionContext {
                        dpi: dimensions.dpi as f32,
                        pixel_max: bar_height,
                        pixel_cell: metrics.cell_size.height as f32,
                    },
                    width: DimensionContext {
                        dpi: dimensions.dpi as f32,
                        pixel_max: dimensions.pixel_width as f32,
                        pixel_cell: metrics.cell_size.width as f32,
                    },
                    bounds: euclid::rect(0.0, 0.0, rect.width(), bar_height),
                    metrics: &metrics,
                    gl_state,
                    zindex: 50,
                },
                &element,
            )?;

            computed.translate(euclid::vec2(rect.min_x(), rect.min_y()));
            let mut ui_items = computed.ui_items();
            self.render_element(&computed, gl_state, None)?;
            self.ui_items.append(&mut ui_items);
        }

        Ok(())
    }

    pub fn paint_pane_drag_overlay(&mut self) -> anyhow::Result<()> {
        let Some(target) = self.pane_drag_target.clone() else {
            return Ok(());
        };

        let rect = target.highlight_rect;
        let border_px = 2.0;
        let inner_w = (rect.width() - border_px * 2.0).max(0.0);
        let inner_h = (rect.height() - border_px * 2.0).max(0.0);

        let font = self.fonts.default_font()?;
        let metrics = self.render_metrics;
        let dimensions = self.dimensions;
        let gl_state = self.render_state.as_ref().unwrap();

        let element = Element::new(&font, ElementContent::Children(vec![]))
            .colors(ElementColors {
                border: BorderColor::new(
                    LinearRgba::with_components(0.40, 0.75, 1.0, 0.95).into(),
                ),
                bg: LinearRgba::with_components(0.25, 0.55, 0.95, 0.28).into(),
                text: LinearRgba::TRANSPARENT.into(),
            })
            .border(BoxDimension::new(Dimension::Pixels(border_px)))
            .min_width(Some(Dimension::Pixels(inner_w)))
            .min_height(Some(Dimension::Pixels(inner_h)));

        let mut computed = self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(0.0, 0.0, rect.width(), rect.height()),
                metrics: &metrics,
                gl_state,
                zindex: 90,
            },
            &element,
        )?;
        computed.translate(euclid::vec2(rect.min_x(), rect.min_y()));
        self.render_element(&computed, gl_state, None)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_drop_edge_selects_nearest_quadrant() {
        let rect: RectF = euclid::rect(100.0, 200.0, 400.0, 300.0);
        assert_eq!(compute_drop_edge(120.0, 350.0, rect), PaneDropEdge::Left);
        assert_eq!(compute_drop_edge(480.0, 350.0, rect), PaneDropEdge::Right);
        assert_eq!(compute_drop_edge(300.0, 210.0, rect), PaneDropEdge::Top);
        assert_eq!(compute_drop_edge(300.0, 490.0, rect), PaneDropEdge::Bottom);
    }

    #[test]
    fn compute_drop_highlight_rect_splits_pane_in_half() {
        let rect: RectF = euclid::rect(100.0, 200.0, 400.0, 300.0);
        assert_eq!(
            compute_drop_highlight_rect(rect, PaneDropEdge::Left),
            euclid::rect(100.0, 200.0, 200.0, 300.0)
        );
        assert_eq!(
            compute_drop_highlight_rect(rect, PaneDropEdge::Right),
            euclid::rect(300.0, 200.0, 200.0, 300.0)
        );
        assert_eq!(
            compute_drop_highlight_rect(rect, PaneDropEdge::Top),
            euclid::rect(100.0, 200.0, 400.0, 150.0)
        );
        assert_eq!(
            compute_drop_highlight_rect(rect, PaneDropEdge::Bottom),
            euclid::rect(100.0, 350.0, 400.0, 150.0)
        );
    }

    #[test]
    fn drop_edge_maps_to_expected_split_request() {
        assert_eq!(
            drop_edge_to_split_request(PaneDropEdge::Left),
            SplitRequest {
                direction: SplitDirection::Horizontal,
                target_is_second: false,
                top_level: false,
                size: SplitSize::Percent(50),
            }
        );
        assert_eq!(
            drop_edge_to_split_request(PaneDropEdge::Right),
            SplitRequest {
                direction: SplitDirection::Horizontal,
                target_is_second: true,
                top_level: false,
                size: SplitSize::Percent(50),
            }
        );
        assert_eq!(
            drop_edge_to_split_request(PaneDropEdge::Top),
            SplitRequest {
                direction: SplitDirection::Vertical,
                target_is_second: false,
                top_level: false,
                size: SplitSize::Percent(50),
            }
        );
        assert_eq!(
            drop_edge_to_split_request(PaneDropEdge::Bottom),
            SplitRequest {
                direction: SplitDirection::Vertical,
                target_is_second: true,
                top_level: false,
                size: SplitSize::Percent(50),
            }
        );
    }
}
