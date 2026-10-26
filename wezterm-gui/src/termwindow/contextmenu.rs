//! The right-click menu.
//!
//! The menu is drawn by wezterm itself, with the same box model as the command
//! palette, rather than asked of the platform. Linux has no system context menu
//! a toolkit-less application can open, so drawing our own is the only way the
//! menu looks and behaves the same on every OS and with every front end.
use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::{DimensionContext, TermWindow, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::keyassignment::{
    ClipboardCopyDestination, ClipboardPasteSource, KeyAssignment, SpawnCommand, SpawnTabDomain,
};
use config::Dimension;
use std::cell::{Cell, Ref, RefCell};
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;
use window::{Modifiers, WindowOps, WindowState};

/// What choosing an entry does. Most entries are ordinary key assignments, so
/// the menu shows the shortcut the input map has for them; the rest act on a
/// particular tab or on the window, which no key assignment can name.
#[derive(Debug, Clone)]
pub enum MenuAction {
    Assignment(KeyAssignment),
    CloseTab(usize),
    Minimize,
    ToggleMaximize,
    CloseWindow,
}

#[derive(Debug, Clone)]
enum Entry {
    Item {
        label: String,
        shortcut: Option<String>,
        action: MenuAction,
        enabled: bool,
    },
    Separator,
}

pub struct ContextMenu {
    entries: Vec<Entry>,
    /// Where the click was, in window pixels: the menu's top left corner
    /// unless that would put part of it outside the window.
    anchor: (isize, isize),
    selected: Cell<Option<usize>>,
    /// Whether a release over an entry chooses it. The release that ends the
    /// opening press must not, or a menu opened near an edge, and so drawn
    /// under the pointer, would choose whatever it opened on.
    armed: Cell<bool>,
    element: RefCell<Option<Vec<ComputedElement>>>,
}

fn split_command() -> SpawnCommand {
    SpawnCommand {
        domain: SpawnTabDomain::CurrentPaneDomain,
        ..Default::default()
    }
}

impl ContextMenu {
    /// The menu for the terminal area: the clipboard first, then the pane,
    /// the tab and the window, in the order they get further from the text.
    pub fn for_pane(term_window: &TermWindow, has_selection: bool, anchor: (isize, isize)) -> Self {
        let mut menu = Self::new(anchor);
        menu.assignment(
            term_window,
            "Copy",
            KeyAssignment::CopyTo(ClipboardCopyDestination::Clipboard),
            has_selection,
        );
        menu.assignment(
            term_window,
            "Paste",
            KeyAssignment::PasteFrom(ClipboardPasteSource::Clipboard),
            true,
        );
        menu.entries.push(Entry::Separator);
        menu.assignment(
            term_window,
            "Split Right",
            KeyAssignment::SplitHorizontal(split_command()),
            true,
        );
        menu.assignment(
            term_window,
            "Split Down",
            KeyAssignment::SplitVertical(split_command()),
            true,
        );
        menu.assignment(
            term_window,
            "Close Pane",
            KeyAssignment::CloseCurrentPane { confirm: true },
            true,
        );
        menu.entries.push(Entry::Separator);
        menu.assignment(
            term_window,
            "New Tab",
            KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain),
            true,
        );
        menu.assignment(
            term_window,
            "Command Palette",
            KeyAssignment::ActivateCommandPalette,
            true,
        );
        menu
    }

    /// The menu for the tab bar, which stands in for the title bar: the tab
    /// that was clicked, if any, then the window.
    pub fn for_tab_bar(
        term_window: &TermWindow,
        tab_idx: Option<usize>,
        anchor: (isize, isize),
    ) -> Self {
        let mut menu = Self::new(anchor);
        menu.assignment(
            term_window,
            "New Tab",
            KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain),
            true,
        );
        if let Some(tab_idx) = tab_idx {
            menu.item("Close Tab", None, MenuAction::CloseTab(tab_idx), true);
        }
        menu.entries.push(Entry::Separator);
        let maximized = term_window
            .window_state
            .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
        menu.item("Minimize", None, MenuAction::Minimize, true);
        menu.item(
            if maximized { "Restore" } else { "Maximize" },
            None,
            MenuAction::ToggleMaximize,
            true,
        );
        menu.item("Close Window", None, MenuAction::CloseWindow, true);
        menu
    }

    fn new(anchor: (isize, isize)) -> Self {
        Self {
            entries: vec![],
            anchor,
            selected: Cell::new(None),
            armed: Cell::new(false),
            element: RefCell::new(None),
        }
    }

    fn item(&mut self, label: &str, shortcut: Option<String>, action: MenuAction, enabled: bool) {
        self.entries.push(Entry::Item {
            label: label.to_string(),
            shortcut,
            action,
            enabled,
        });
    }

    fn assignment(
        &mut self,
        term_window: &TermWindow,
        label: &str,
        action: KeyAssignment,
        enabled: bool,
    ) {
        let shortcut = shortcut_label(term_window, &action);
        self.item(label, shortcut, MenuAction::Assignment(action), enabled);
    }

    fn is_choosable(&self, idx: usize) -> bool {
        matches!(
            self.entries.get(idx),
            Some(Entry::Item { enabled: true, .. })
        )
    }

    /// The action of entry `idx`, if choosing it does anything.
    pub fn action(&self, idx: usize) -> Option<MenuAction> {
        match self.entries.get(idx) {
            Some(Entry::Item {
                action,
                enabled: true,
                ..
            }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Highlights the entry under the pointer. Returns true if that changed
    /// what has to be drawn.
    pub fn hover(&self, idx: Option<usize>) -> bool {
        let idx = idx.filter(|&idx| self.is_choosable(idx));
        if self.selected.get() == idx {
            return false;
        }
        self.selected.set(idx);
        self.element.borrow_mut().take();
        true
    }

    pub fn arm(&self) {
        self.armed.set(true);
    }

    /// The pointer has moved to `coords`; a release now chooses, as long as
    /// the pointer really left the spot where the menu was opened.
    pub fn pointer_moved(&self, coords: (isize, isize)) {
        let (x, y) = self.anchor;
        if (coords.0 - x).abs() > 2 || (coords.1 - y).abs() > 2 {
            self.armed.set(true);
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed.get()
    }

    fn step(&self, forward: bool) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let mut idx = match self.selected.get() {
            Some(idx) => idx,
            None if forward => len - 1,
            None => 0,
        };
        for _ in 0..len {
            idx = if forward {
                (idx + 1) % len
            } else {
                (idx + len - 1) % len
            };
            if self.is_choosable(idx) {
                self.selected.set(Some(idx));
                self.element.borrow_mut().take();
                return;
            }
        }
    }

    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics())
            .scale_line_height(term_window.config.command_palette_line_height);

        let fg = term_window.config.command_palette_fg_color.to_linear();
        let bg = term_window.config.command_palette_bg_color.to_linear();
        let muted = mix(fg, bg, 0.5);

        // Wide enough for the longest label and the longest shortcut, with
        // room between them, so a shortcut never sits against its label.
        let label_cells = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { label, .. } => unicode_column_width(label),
                Entry::Separator => 0,
            })
            .max()
            .unwrap_or(0);
        let shortcut_cells = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Item {
                    shortcut: Some(shortcut),
                    ..
                } => unicode_column_width(shortcut) + 3,
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        let cell_width = metrics.cell_size.width as f32;
        let row_pixel_width = (label_cells + shortcut_cells + 2) as f32 * cell_width;

        let mut rows = vec![];
        for (idx, entry) in self.entries.iter().enumerate() {
            match entry {
                Entry::Separator => {
                    rows.push(
                        Element::new(&font, ElementContent::Children(vec![]))
                            .display(DisplayType::Block)
                            .min_width(Some(Dimension::Percent(1.)))
                            .margin(BoxDimension {
                                left: Dimension::Cells(0.),
                                right: Dimension::Cells(0.),
                                top: Dimension::Cells(0.2),
                                bottom: Dimension::Cells(0.2),
                            })
                            .border(BoxDimension {
                                left: Dimension::Pixels(0.),
                                right: Dimension::Pixels(0.),
                                top: Dimension::Pixels(0.),
                                bottom: Dimension::Pixels(1.),
                            })
                            .colors(ElementColors {
                                border: BorderColor::new(muted),
                                bg: LinearRgba::TRANSPARENT.into(),
                                text: muted.into(),
                            }),
                    );
                }
                Entry::Item {
                    label,
                    shortcut,
                    enabled,
                    ..
                } => {
                    let selected = self.selected.get() == Some(idx);
                    let (row_bg, row_text, shortcut_text) = if selected {
                        (fg, bg, bg)
                    } else if *enabled {
                        (LinearRgba::TRANSPARENT, fg, muted)
                    } else {
                        (LinearRgba::TRANSPARENT, muted, muted)
                    };

                    let mut children =
                        vec![Element::new(&font, ElementContent::Text(label.clone()))];
                    if let Some(shortcut) = shortcut {
                        children.push(
                            Element::new(&font, ElementContent::Text(shortcut.clone()))
                                .float(Float::Right)
                                .colors(ElementColors {
                                    border: BorderColor::default(),
                                    bg: row_bg.into(),
                                    text: shortcut_text.into(),
                                }),
                        );
                    }

                    rows.push(
                        Element::new(&font, ElementContent::Children(children))
                            .item_type(UIItemType::ContextMenuItem(idx))
                            .display(DisplayType::Block)
                            .min_width(Some(Dimension::Percent(1.)))
                            .padding(BoxDimension {
                                left: Dimension::Cells(1.),
                                right: Dimension::Cells(1.),
                                top: Dimension::Cells(0.1),
                                bottom: Dimension::Cells(0.1),
                            })
                            .colors(ElementColors {
                                border: BorderColor::default(),
                                bg: row_bg.into(),
                                text: row_text.into(),
                            }),
                    );
                }
            }
        }

        let element = Element::new(&font, ElementContent::Children(rows))
            .item_type(UIItemType::ContextMenu)
            .colors(ElementColors {
                border: BorderColor::new(muted),
                bg: bg.into(),
                text: fg.into(),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.),
                right: Dimension::Cells(0.),
                top: Dimension::Cells(0.2),
                bottom: Dimension::Cells(0.2),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .min_width(Some(Dimension::Pixels(row_pixel_width)));

        let dimensions = term_window.dimensions;
        let window_width = dimensions.pixel_width as f32;
        let window_height = dimensions.pixel_height as f32;
        let (anchor_x, anchor_y) = (self.anchor.0 as f32, self.anchor.1 as f32);

        let mut computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: window_height,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: window_width,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(anchor_x, anchor_y, row_pixel_width + 2., window_height),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        // Keep the whole menu in the window. Past the right edge it moves left;
        // past the bottom it opens upwards from the pointer, as menus do, and
        // only if there is no room for that either is it pushed against an edge.
        let bounds = computed.bounds;
        let mut dx = 0.;
        if bounds.max_x() > window_width {
            dx = (window_width - bounds.max_x()).max(-bounds.min_x());
        }
        let mut dy = 0.;
        if bounds.max_y() > window_height {
            dy = if bounds.height() <= anchor_y {
                -bounds.height()
            } else {
                (window_height - bounds.max_y()).max(-bounds.min_y())
            };
        }
        if dx != 0. || dy != 0. {
            computed.translate(euclid::vec2(dx, dy));
        }

        Ok(vec![computed])
    }
}

fn mix(a: LinearRgba, b: LinearRgba, amount: f32) -> LinearRgba {
    LinearRgba(
        a.0 + (b.0 - a.0) * amount,
        a.1 + (b.1 - a.1) * amount,
        a.2 + (b.2 - a.2) * amount,
        a.3 + (b.3 - a.3) * amount,
    )
}

fn unicode_column_width(s: &str) -> usize {
    termwiz::cell::unicode_column_width(s, None)
}

/// The shortcut the input map has for `action`, as the command palette shows
/// it. Where there are several, the one a reader is most likely to have and
/// to press: on macOS a CMD shortcut, elsewhere one without SUPER, which the
/// desktop environment tends to keep for itself.
fn shortcut_label(term_window: &TermWindow, action: &KeyAssignment) -> Option<String> {
    let mut keys = term_window.input_map.locate_app_wide_key_assignment(action);
    keys.sort_by_key(|(_, mods)| {
        let preferred = if cfg!(target_os = "macos") {
            mods.contains(Modifiers::SUPER)
        } else {
            !mods.contains(Modifiers::SUPER)
        };
        // A shortcut with no modifiers is a dedicated key such as Copy or
        // Paste, which most keyboards lack, so it comes last.
        (!preferred, mods.is_empty(), mods.bits())
    });
    let (key, mods) = keys.into_iter().next()?;

    let rendering = term_window.config.ui_key_cap_rendering;
    let separator = if rendering == ::window::UIKeyCapRendering::AppleSymbols {
        " "
    } else {
        "-"
    };
    let mut label = mods.to_string_with_separator(::window::ModifierToStringArgs {
        separator,
        want_none: false,
        ui_key_cap_rendering: Some(rendering),
    });
    if !label.is_empty() {
        label.push_str(separator);
    }
    label.push_str(&crate::inputmap::ui_key(&key, rendering));
    Some(label)
}

impl TermWindow {
    /// Opens the menu for the terminal area at the pointer.
    pub fn show_context_menu(&mut self) {
        let anchor = self
            .current_mouse_event
            .as_ref()
            .map(|event| (event.coords.x, event.coords.y))
            .unwrap_or((0, 0));
        let has_selection = self
            .get_active_pane_or_overlay()
            .map(|pane| !self.selection_text(&pane).is_empty())
            .unwrap_or(false);
        let menu = ContextMenu::for_pane(self, has_selection, anchor);
        self.set_modal(std::rc::Rc::new(menu));
    }

    /// Opens the menu for the tab bar at `anchor`, for the tab at `tab_idx`
    /// if the click was on one.
    pub fn show_tab_bar_menu(&mut self, tab_idx: Option<usize>, anchor: (isize, isize)) {
        let menu = ContextMenu::for_tab_bar(self, tab_idx, anchor);
        self.set_modal(std::rc::Rc::new(menu));
    }

    /// Closes the menu and does what entry `idx` says.
    pub fn choose_context_menu_entry(&mut self, menu: &ContextMenu, idx: usize) {
        let action = match menu.action(idx) {
            Some(action) => action,
            None => return,
        };
        self.cancel_modal();

        match action {
            MenuAction::Assignment(assignment) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    if let Err(err) = self.perform_key_assignment(&pane, &assignment) {
                        log::error!("context menu: {assignment:?}: {err:#}");
                    }
                }
            }
            MenuAction::CloseTab(tab_idx) => self.close_specific_tab(tab_idx, true),
            MenuAction::Minimize => {
                if let Some(window) = self.window.as_ref() {
                    window.hide();
                }
            }
            MenuAction::ToggleMaximize => {
                if let Some(window) = self.window.as_ref() {
                    if self
                        .window_state
                        .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN)
                    {
                        window.restore();
                    } else {
                        window.maximize();
                    }
                }
            }
            MenuAction::CloseWindow => {
                if let Some(window) = self.window.clone() {
                    self.close_requested(&window);
                }
            }
        }
    }
}

impl Modal for ContextMenu {
    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        // Window mouse events reach the menu before the terminal sees them;
        // see TermWindow::mouse_event_context_menu.
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        match (key, mods) {
            (KeyCode::Escape, _) => {
                term_window.cancel_modal();
                return Ok(true);
            }
            (KeyCode::UpArrow, KeyModifiers::NONE) => self.step(false),
            (KeyCode::DownArrow, KeyModifiers::NONE) => self.step(true),
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if let Some(idx) = self.selected.get() {
                    term_window.choose_context_menu_entry(self, idx);
                }
                return Ok(true);
            }
            // While the menu is open, typing goes nowhere: a keystroke meant
            // for the menu must not reach the program in the pane.
            _ => return Ok(true),
        }
        term_window.invalidate_modal();
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let element = self.compute(term_window)?;
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
