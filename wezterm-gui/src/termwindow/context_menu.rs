use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::{DimensionContext, TermWindow};
use crate::utilsprites::RenderMetrics;
use ::window::{CursorIcon, MouseEvent, MouseEventKind as WMEK, MousePress, WindowOps};
use config::keyassignment::KeyAssignment;
use config::Dimension;
use mux::pane::PaneId;
use mux::Mux;
use std::cell::{Ref, RefCell};
use wezterm_term::{KeyCode, KeyModifiers};
use window::color::LinearRgba;

#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenuItem {
    pub label: String,
    pub checked: Option<bool>,
    pub action: KeyAssignment,
}

impl ContextMenuItem {
    fn display_label(&self) -> String {
        match self.checked {
            Some(true) => format!("[x] {}", self.label),
            Some(false) => format!("[ ] {}", self.label),
            None => format!("    {}", self.label),
        }
    }
}

pub struct ContextMenu {
    pane_id: PaneId,
    anchor: (f32, f32),
    items: Vec<ContextMenuItem>,
    selected_row: RefCell<Option<usize>>,
    element: RefCell<Option<Vec<ComputedElement>>>,
}

pub(crate) fn clamp_menu_position(
    anchor: (f32, f32),
    menu_size: (f32, f32),
    window_size: (f32, f32),
) -> (f32, f32) {
    let max_x = (window_size.0 - menu_size.0).max(0.0);
    let max_y = (window_size.1 - menu_size.1).max(0.0);
    (anchor.0.clamp(0.0, max_x), anchor.1.clamp(0.0, max_y))
}

pub(crate) fn next_selected_row(
    current: Option<usize>,
    len: usize,
    move_down: bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, move_down) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(idx), true) => (idx + 1).min(len - 1),
        (Some(idx), false) => idx.saturating_sub(1),
    })
}

impl ContextMenu {
    pub fn new(term_window: &mut TermWindow, pane_id: PaneId) -> Self {
        let anchor = term_window
            .current_mouse_event
            .as_ref()
            .map(|ev| (ev.coords.x as f32, ev.coords.y as f32))
            .unwrap_or_else(|| {
                (
                    term_window.dimensions.pixel_width as f32 / 2.0,
                    term_window.dimensions.pixel_height as f32 / 2.0,
                )
            });

        let (watch_for_silence, watch_for_activity) = {
            let state = term_window.pane_state(pane_id);
            (
                state.watch.watch_for_silence,
                state.watch.watch_for_activity,
            )
        };

        let items = vec![
            ContextMenuItem {
                label: "Watch for Silence".to_string(),
                checked: Some(watch_for_silence),
                action: KeyAssignment::ToggleWatchForSilence,
            },
            ContextMenuItem {
                label: "Watch for Activity".to_string(),
                checked: Some(watch_for_activity),
                action: KeyAssignment::ToggleWatchForActivity,
            },
        ];

        Self {
            pane_id,
            anchor,
            items,
            selected_row: RefCell::new(None),
            element: RefCell::new(None),
        }
    }

    fn build_menu_element(
        term_window: &mut TermWindow,
        items: &[ContextMenuItem],
        selected_row: Option<usize>,
        row_min_width: Option<f32>,
    ) -> anyhow::Result<ComputedElement> {
        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics())
            .scale_line_height(term_window.config.command_palette_line_height);

        let solid_bg_color: InheritableColor = term_window
            .config
            .command_palette_bg_color
            .to_linear()
            .into();
        let solid_fg_color: InheritableColor = term_window
            .config
            .command_palette_fg_color
            .to_linear()
            .into();

        let mut rows = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            let is_selected = selected_row == Some(idx);
            let (bg, text) = if is_selected {
                (solid_fg_color.clone(), solid_bg_color.clone())
            } else {
                (LinearRgba::TRANSPARENT.into(), solid_fg_color.clone())
            };

            let mut row = Element::new(&font, ElementContent::Text(item.display_label()))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg,
                    text,
                })
                .padding(BoxDimension {
                    left: Dimension::Cells(0.5),
                    right: Dimension::Cells(0.5),
                    top: Dimension::Cells(0.15),
                    bottom: Dimension::Cells(0.15),
                })
                .display(DisplayType::Block);

            if let Some(width) = row_min_width {
                row = row.min_width(Some(Dimension::Pixels(width)));
            }

            rows.push(row);
        }

        let container = Element::new(&font, ElementContent::Children(rows))
            .colors(ElementColors {
                border: BorderColor::new(
                    term_window
                        .config
                        .command_palette_fg_color
                        .to_linear()
                        .into(),
                ),
                bg: solid_bg_color,
                text: solid_fg_color,
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.25),
                bottom: Dimension::Cells(0.25),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .border_corners(Some(Corners {
                top_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_LEFT_ROUNDED_CORNER,
                },
                top_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_RIGHT_ROUNDED_CORNER,
                },
                bottom_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_LEFT_ROUNDED_CORNER,
                },
                bottom_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                },
            }));

        let dimensions = term_window.dimensions;
        let gl_state = term_window
            .render_state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("render state not initialized"))?;

        term_window.compute_element(
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
                bounds: euclid::rect(
                    0.,
                    0.,
                    dimensions.pixel_width as f32,
                    dimensions.pixel_height as f32,
                ),
                metrics: &metrics,
                gl_state,
                zindex: 100,
            },
            &container,
        )
    }

    fn compute(
        term_window: &mut TermWindow,
        anchor: (f32, f32),
        items: &[ContextMenuItem],
        selected_row: Option<usize>,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        if term_window.render_state.is_none() {
            return Ok(vec![]);
        }

        let measured = Self::build_menu_element(term_window, items, selected_row, None)?;
        let max_row_content_width = match &measured.content {
            ComputedElementContent::Children(kids) => kids
                .iter()
                .map(|kid| kid.content_rect.width())
                .fold(0.0f32, f32::max),
            _ => 0.0,
        };

        let mut computed =
            Self::build_menu_element(term_window, items, selected_row, Some(max_row_content_width))?;

        let dimensions = term_window.dimensions;
        let (x, y) = clamp_menu_position(
            anchor,
            (computed.bounds.width(), computed.bounds.height()),
            (
                dimensions.pixel_width as f32,
                dimensions.pixel_height as f32,
            ),
        );
        computed.translate(euclid::vec2(x, y));

        Ok(vec![computed])
    }

    fn hit_test(&self, x: f32, y: f32) -> (bool, Option<usize>) {
        let element = self.element.borrow();
        let Some(elements) = element.as_ref() else {
            return (false, None);
        };
        let Some(menu) = elements.first() else {
            return (false, None);
        };
        let pt = euclid::point2(x, y);
        if !menu.bounds.contains(pt) {
            return (false, None);
        }
        if let ComputedElementContent::Children(kids) = &menu.content {
            for (idx, kid) in kids.iter().enumerate() {
                if kid.bounds.contains(pt) {
                    return (true, Some(idx));
                }
            }
        }
        (true, None)
    }

    fn activate_item(&self, idx: usize, term_window: &mut TermWindow) {
        let Some(item) = self.items.get(idx) else {
            term_window.cancel_modal();
            return;
        };
        let action = item.action.clone();
        term_window.cancel_modal();
        if let Some(window) = term_window.window.as_ref() {
            window.set_cursor(Some(CursorIcon::Default));
            window.invalidate();
        }

        let pane = Mux::get()
            .get_pane(self.pane_id)
            .or_else(|| term_window.get_active_pane_or_overlay());
        if let Some(pane) = pane {
            if let Err(err) = term_window.perform_key_assignment(&pane, &action) {
                log::error!("Error performing context menu action {action:?}: {err:#}");
            }
        }
    }

    fn close(&self, term_window: &mut TermWindow) {
        term_window.cancel_modal();
        if let Some(window) = term_window.window.as_ref() {
            window.set_cursor(Some(CursorIcon::Default));
            window.invalidate();
        }
    }
}

impl Modal for ContextMenu {
    fn perform_assignment(
        &self,
        _assignment: &KeyAssignment,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }

    fn mouse_event(&self, event: MouseEvent, term_window: &mut TermWindow) -> anyhow::Result<bool> {
        // Ensure layout is computed before hit-testing if an event arrives prior to paint.
        let _ = self.computed_element(term_window)?;

        let x = event.coords.x as f32;
        let y = event.coords.y as f32;
        let (inside_menu, hit_item) = self.hit_test(x, y);

        match event.kind {
            WMEK::Move => {
                let changed = {
                    let mut selected = self.selected_row.borrow_mut();
                    if *selected != hit_item {
                        *selected = hit_item;
                        true
                    } else {
                        false
                    }
                };
                if let Some(window) = term_window.window.as_ref() {
                    window.set_cursor(Some(if hit_item.is_some() {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    }));
                }
                if changed {
                    term_window.invalidate_modal();
                }
            }
            WMEK::Press(MousePress::Left) => {
                if let Some(idx) = hit_item {
                    self.activate_item(idx, term_window);
                } else if !inside_menu {
                    self.close(term_window);
                }
            }
            WMEK::Press(MousePress::Right) => {
                self.close(term_window);
            }
            WMEK::Release(_) => {
                // Ignore button release (including the right-click release right after opening).
            }
            _ => {}
        }

        Ok(true)
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE) | (KeyCode::Char('g'), KeyModifiers::CTRL) => {
                self.close(term_window);
                return Ok(true);
            }
            (KeyCode::UpArrow, KeyModifiers::NONE) | (KeyCode::Char('p'), KeyModifiers::CTRL) => {
                let mut selected = self.selected_row.borrow_mut();
                *selected = next_selected_row(*selected, self.items.len(), false);
            }
            (KeyCode::DownArrow, KeyModifiers::NONE) | (KeyCode::Char('n'), KeyModifiers::CTRL) => {
                let mut selected = self.selected_row.borrow_mut();
                *selected = next_selected_row(*selected, self.items.len(), true);
            }
            (KeyCode::Enter, KeyModifiers::NONE) | (KeyCode::Char(' '), KeyModifiers::NONE) => {
                let selected = *self.selected_row.borrow();
                if let Some(idx) = selected {
                    self.activate_item(idx, term_window);
                }
                return Ok(true);
            }
            _ => return Ok(false),
        }

        term_window.invalidate_modal();
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let element = Self::compute(
                term_window,
                self.anchor,
                &self.items,
                *self.selected_row.borrow(),
            )?;
            self.element.borrow_mut().replace(element);
        }

        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_menu_position_keeps_menu_within_window() {
        // Normal placement with ample room
        assert_eq!(
            clamp_menu_position((100.0, 150.0), (200.0, 60.0), (800.0, 600.0)),
            (100.0, 150.0)
        );
        // Near right and bottom edges
        assert_eq!(
            clamp_menu_position((750.0, 580.0), (200.0, 60.0), (800.0, 600.0)),
            (600.0, 540.0)
        );
        // Negative anchor clamped to 0
        assert_eq!(
            clamp_menu_position((-10.0, -5.0), (200.0, 60.0), (800.0, 600.0)),
            (0.0, 0.0)
        );
        // Menu larger than window clamps to origin
        assert_eq!(
            clamp_menu_position((50.0, 50.0), (300.0, 200.0), (200.0, 100.0)),
            (0.0, 0.0)
        );
    }

    #[test]
    fn next_selected_row_navigation() {
        assert_eq!(next_selected_row(None, 0, true), None);
        assert_eq!(next_selected_row(None, 0, false), None);

        assert_eq!(next_selected_row(None, 2, true), Some(0));
        assert_eq!(next_selected_row(Some(0), 2, true), Some(1));
        assert_eq!(next_selected_row(Some(1), 2, true), Some(1));

        assert_eq!(next_selected_row(None, 2, false), Some(1));
        assert_eq!(next_selected_row(Some(1), 2, false), Some(0));
        assert_eq!(next_selected_row(Some(0), 2, false), Some(0));
    }

    #[test]
    fn context_menu_item_display_label_formatting() {
        let silence_off = ContextMenuItem {
            label: "Watch for Silence".to_string(),
            checked: Some(false),
            action: KeyAssignment::ToggleWatchForSilence,
        };
        let silence_on = ContextMenuItem {
            label: "Watch for Silence".to_string(),
            checked: Some(true),
            action: KeyAssignment::ToggleWatchForSilence,
        };
        let plain = ContextMenuItem {
            label: "Plain".to_string(),
            checked: None,
            action: KeyAssignment::ShowContextMenu,
        };

        assert_eq!(silence_off.display_label(), "[ ] Watch for Silence");
        assert_eq!(silence_on.display_label(), "[x] Watch for Silence");
        assert_eq!(plain.display_label(), "    Plain");
    }
}
