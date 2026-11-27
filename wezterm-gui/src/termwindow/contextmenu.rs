//! The right-click menu.
//!
//! The menu is drawn by wezterm itself, with the same box model as the command
//! palette, rather than asked of the platform. Linux has no system context menu
//! a toolkit-less application can open, so drawing our own is the only way the
//! menu looks and behaves the same on every OS and with every front end.
//!
//! An entry can be a submenu, which opens beside it. The configuration adds
//! entries to the tab bar's menu with the `augment-tab-bar-menu` event.
use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::{DimensionContext, GuiWin, TermWindow, UIItemType};
use crate::utilsprites::RenderMetrics;
use anyhow::{anyhow, bail};
use config::keyassignment::{
    ClipboardCopyDestination, ClipboardPasteSource, KeyAssignment, SpawnCommand, SpawnTabDomain,
};
use config::Dimension;
use luahelper::from_lua_value_dynamic;
use mux_lua::MuxPane;
use std::cell::{Cell, Ref, RefCell};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;
use window::{Modifiers, WindowOps, WindowState};

/// The event a configuration handles to add entries to the tab bar's menu.
const TAB_BAR_MENU_EVENT: &str = "augment-tab-bar-menu";

/// What choosing an entry does. Most entries are ordinary key assignments, so
/// the menu shows the shortcut the input map has for them; the rest act on a
/// particular tab or on the window, which no key assignment can name.
#[derive(Debug, Clone, PartialEq)]
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
    /// A label that opens further entries beside the menu.
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
    Separator,
}

impl Entry {
    /// Whether the entry can be highlighted: an item that does something, or
    /// a submenu, which opens.
    fn is_choosable(&self) -> bool {
        matches!(
            self,
            Entry::Item { enabled: true, .. } | Entry::Submenu { .. }
        )
    }
}

/// An entry a configuration adds to the tab bar's menu, as its
/// `augment-tab-bar-menu` handler returned it.
#[derive(Debug, Clone, PartialEq)]
pub enum UserEntry {
    Item {
        label: String,
        action: KeyAssignment,
    },
    Submenu {
        label: String,
        entries: Vec<UserEntry>,
    },
    Separator,
}

/// A key the menu acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
}

/// What a key or a click did to the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuOutcome {
    Unchanged,
    /// What is highlighted or open changed, so the menu must be drawn again.
    Redraw,
    /// The entry at this path (an index in the menu, then in each submenu
    /// down to the entry) was chosen.
    Choose(Vec<usize>),
    /// The whole menu closes.
    Close,
}

pub struct ContextMenu {
    entries: Vec<Entry>,
    /// Where the click was, in window pixels: the menu's top left corner
    /// unless that would put part of it outside the window.
    anchor: (isize, isize),
    /// The levels that are open, and the entry highlighted in each: the first
    /// is the menu itself, and each further level is the submenu of the entry
    /// highlighted in the level before it. There is always at least one.
    cursor: RefCell<Vec<Option<usize>>>,
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
    /// that was clicked, if any, then the window, then what the configuration
    /// added.
    pub fn for_tab_bar(
        term_window: &TermWindow,
        tab_idx: Option<usize>,
        anchor: (isize, isize),
        added: Vec<UserEntry>,
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
        menu.append(added, &|action| shortcut_label(term_window, action));
        menu
    }

    fn new(anchor: (isize, isize)) -> Self {
        Self {
            entries: vec![],
            anchor,
            cursor: RefCell::new(vec![None]),
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

    /// Adds the configuration's entries after a separator, each item with the
    /// shortcut `shortcut` gives its action.
    fn append(
        &mut self,
        added: Vec<UserEntry>,
        shortcut: &dyn Fn(&KeyAssignment) -> Option<String>,
    ) {
        if added.is_empty() {
            return;
        }
        self.entries.push(Entry::Separator);
        self.entries
            .extend(added.into_iter().map(|entry| menu_entry(entry, shortcut)));
    }

    /// The entries of the level at `depth`, which `cursor` has open.
    fn level<'a>(&'a self, cursor: &[Option<usize>], depth: usize) -> &'a [Entry] {
        let mut entries = self.entries.as_slice();
        for selected in &cursor[..depth] {
            match selected.and_then(|idx| entries.get(idx)) {
                Some(Entry::Submenu { entries: inner, .. }) => entries = inner,
                _ => return &[],
            }
        }
        entries
    }

    /// The entry at `path`: an index in the menu, then in each submenu.
    fn entry_at(&self, path: &[usize]) -> Option<&Entry> {
        let (&last, submenus) = path.split_last()?;
        let mut entries = self.entries.as_slice();
        for &idx in submenus {
            match entries.get(idx) {
                Some(Entry::Submenu { entries: inner, .. }) => entries = inner,
                _ => return None,
            }
        }
        entries.get(last)
    }

    /// The action of the entry at `path`, if choosing it does anything.
    pub fn action(&self, path: &[usize]) -> Option<MenuAction> {
        match self.entry_at(path) {
            Some(Entry::Item {
                action,
                enabled: true,
                ..
            }) => Some(action.clone()),
            _ => None,
        }
    }

    /// Replaces the open levels and their highlights; returns true if that
    /// changed what has to be drawn.
    fn set_cursor(&self, cursor: Vec<Option<usize>>) -> bool {
        if *self.cursor.borrow() == cursor {
            return false;
        }
        self.cursor.replace(cursor);
        self.element.borrow_mut().take();
        true
    }

    /// The open levels once entry `idx` of the level at `depth` is
    /// highlighted: the levels after it close, and a submenu opens.
    fn highlighting(&self, depth: usize, idx: usize) -> Vec<Option<usize>> {
        let mut cursor = self.cursor.borrow().clone();
        cursor.truncate(depth + 1);
        let entry = self.level(&cursor, depth).get(idx);
        if entry.map(Entry::is_choosable).unwrap_or(false) {
            cursor[depth] = Some(idx);
            if let Some(Entry::Submenu { .. }) = entry {
                cursor.push(None);
            }
        } else {
            cursor[depth] = None;
        }
        cursor
    }

    /// Highlights the entry under the pointer, `(depth, idx)`: entry `idx` of
    /// the level open at `depth`. Over a submenu, that opens it. Away from
    /// every entry, the deepest level has nothing highlighted, and the
    /// submenus stay open. Returns true if that changed what has to be drawn.
    pub fn hover(&self, at: Option<(usize, usize)>) -> bool {
        let depth_open = self.cursor.borrow().len();
        match at {
            Some((depth, idx)) if depth < depth_open => {
                self.set_cursor(self.highlighting(depth, idx))
            }
            Some(_) => false,
            None => {
                let mut cursor = self.cursor.borrow().clone();
                *cursor.last_mut().expect("the menu is open") = None;
                self.set_cursor(cursor)
            }
        }
    }

    /// A release over entry `idx` of the level at `depth`: an item is chosen,
    /// a submenu opens, but only once the menu is armed.
    pub fn release(&self, depth: usize, idx: usize) -> MenuOutcome {
        if !self.is_armed() || depth >= self.cursor.borrow().len() {
            return MenuOutcome::Unchanged;
        }
        let cursor = self.cursor.borrow().clone();
        match self.level(&cursor, depth).get(idx) {
            Some(Entry::Item { enabled: true, .. }) => {
                let mut path: Vec<usize> = cursor[..depth].iter().flatten().copied().collect();
                path.push(idx);
                MenuOutcome::Choose(path)
            }
            Some(Entry::Submenu { .. }) => {
                if self.set_cursor(self.highlighting(depth, idx)) {
                    MenuOutcome::Redraw
                } else {
                    MenuOutcome::Unchanged
                }
            }
            _ => MenuOutcome::Unchanged,
        }
    }

    /// Acts on a key: Up and Down move through the level that has the
    /// highlight, Right or Enter open the highlighted submenu at its first
    /// entry, Enter chooses the highlighted item, Left closes the deepest
    /// submenu, and Escape closes it too, or the menu when none is open.
    pub fn key(&self, key: MenuKey) -> MenuOutcome {
        let mut cursor = self.cursor.borrow().clone();
        // The level the keys act on: the deepest one with a highlight. A
        // submenu opened by hovering has none until it is entered.
        let focus = cursor.iter().rposition(Option::is_some).unwrap_or(0);
        let changed = |cursor| {
            if self.set_cursor(cursor) {
                MenuOutcome::Redraw
            } else {
                MenuOutcome::Unchanged
            }
        };
        match key {
            MenuKey::Up | MenuKey::Down => {
                let entries = self.level(&cursor, focus);
                match step(entries, cursor[focus], key == MenuKey::Down) {
                    Some(idx) => {
                        cursor.truncate(focus + 1);
                        cursor[focus] = Some(idx);
                        changed(cursor)
                    }
                    None => MenuOutcome::Unchanged,
                }
            }
            MenuKey::Right | MenuKey::Enter => {
                let selected = match cursor[focus] {
                    Some(idx) => idx,
                    None => return MenuOutcome::Unchanged,
                };
                match self.level(&cursor, focus).get(selected) {
                    Some(Entry::Submenu { entries, .. }) => {
                        let first = step(entries, None, true);
                        cursor.truncate(focus + 1);
                        cursor.push(first);
                        changed(cursor)
                    }
                    Some(Entry::Item { enabled: true, .. }) if key == MenuKey::Enter => {
                        MenuOutcome::Choose(cursor[..=focus].iter().flatten().copied().collect())
                    }
                    _ => MenuOutcome::Unchanged,
                }
            }
            MenuKey::Left | MenuKey::Escape => {
                if cursor.len() > 1 {
                    cursor.pop();
                    changed(cursor)
                } else if key == MenuKey::Escape {
                    MenuOutcome::Close
                } else {
                    MenuOutcome::Unchanged
                }
            }
        }
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

    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics())
            .scale_line_height(term_window.config.command_palette_line_height);
        let colors = MenuColors::new(term_window);

        let dimensions = term_window.dimensions;
        let window = Size {
            width: dimensions.pixel_width as f32,
            height: dimensions.pixel_height as f32,
        };
        let cursor = self.cursor.borrow().clone();
        let mut levels: Vec<ComputedElement> = vec![];
        for (depth, &selected) in cursor.iter().enumerate() {
            let entries = self.level(&cursor, depth);
            let (element, width) =
                level_element(&font, &metrics, &colors, entries, depth, selected);
            let mut computed = term_window.compute_element(
                &LayoutContext {
                    height: DimensionContext {
                        dpi: dimensions.dpi as f32,
                        pixel_max: window.height,
                        pixel_cell: metrics.cell_size.height as f32,
                    },
                    width: DimensionContext {
                        dpi: dimensions.dpi as f32,
                        pixel_max: window.width,
                        pixel_cell: metrics.cell_size.width as f32,
                    },
                    bounds: euclid::rect(0., 0., width + 2., window.height),
                    metrics: &metrics,
                    gl_state: term_window.render_state.as_ref().unwrap(),
                    // Each submenu over the level it opened from.
                    zindex: i8::try_from(100 + depth).unwrap_or(i8::MAX),
                },
                &element,
            )?;

            let bounds = computed.bounds;
            let size = Size {
                width: bounds.width(),
                height: bounds.height(),
            };
            let (x, y) = match levels.last() {
                None => place_menu((self.anchor.0 as f32, self.anchor.1 as f32), size, window),
                Some(parent) => {
                    let opened_from = UIItemType::ContextMenuItem(
                        depth - 1,
                        cursor[depth - 1].expect("an open submenu's entry is highlighted"),
                    );
                    let row_top = parent
                        .ui_items()
                        .into_iter()
                        .find(|item| item.item_type == opened_from)
                        .map(|item| item.y as f32)
                        .unwrap_or(parent.bounds.min_y());
                    // The submenu's first entry level with the entry it opened from.
                    let first_row = computed.content_rect.min_y() - bounds.min_y();
                    place_submenu(
                        (parent.bounds.min_x(), parent.bounds.max_x()),
                        row_top - first_row,
                        size,
                        window,
                    )
                }
            };
            computed.translate(euclid::vec2(x - bounds.min_x(), y - bounds.min_y()));
            levels.push(computed);
        }
        Ok(levels)
    }
}

/// The entry a configuration's entry becomes.
fn menu_entry(entry: UserEntry, shortcut: &dyn Fn(&KeyAssignment) -> Option<String>) -> Entry {
    match entry {
        UserEntry::Item { label, action } => Entry::Item {
            label,
            shortcut: shortcut(&action),
            action: MenuAction::Assignment(action),
            enabled: true,
        },
        UserEntry::Submenu { label, entries } => Entry::Submenu {
            label,
            entries: entries
                .into_iter()
                .map(|entry| menu_entry(entry, shortcut))
                .collect(),
        },
        UserEntry::Separator => Entry::Separator,
    }
}

/// The choosable entry after (or before) `from` in `entries`, going round;
/// the first (or last) one when nothing is highlighted.
fn step(entries: &[Entry], from: Option<usize>, forward: bool) -> Option<usize> {
    let len = entries.len();
    if len == 0 {
        return None;
    }
    let mut idx = match from {
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
        if entries[idx].is_choosable() {
            return Some(idx);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Size {
    width: f32,
    height: f32,
}

/// Where the menu's top left corner goes: at the pointer, keeping the whole
/// menu in the window. Past the right edge it moves left; past the bottom it
/// opens upwards from the pointer, as menus do, and only if there is no room
/// for that either is it pushed against an edge.
fn place_menu(anchor: (f32, f32), size: Size, window: Size) -> (f32, f32) {
    let (mut x, mut y) = anchor;
    if x + size.width > window.width {
        x = (window.width - size.width).max(0.);
    }
    if y + size.height > window.height {
        y = if size.height <= anchor.1 {
            anchor.1 - size.height
        } else {
            (window.height - size.height).max(0.)
        };
    }
    (x, y)
}

/// Where a submenu's top left corner goes: right of the level it opened
/// from, `parent_x` being that level's left and right edges, at `top`, which
/// puts its first entry level with the entry it opened from; left of that
/// level when there is no room on the right, and moved up to stay in the
/// window.
fn place_submenu(parent_x: (f32, f32), top: f32, size: Size, window: Size) -> (f32, f32) {
    let (parent_left, parent_right) = parent_x;
    let mut x = parent_right;
    if x + size.width > window.width {
        x = parent_left - size.width;
        if x < 0. {
            x = (window.width - size.width).max(0.);
        }
    }
    let mut y = top;
    if y + size.height > window.height {
        y = window.height - size.height;
    }
    (x, y.max(0.))
}

struct MenuColors {
    fg: LinearRgba,
    bg: LinearRgba,
    muted: LinearRgba,
}

impl MenuColors {
    fn new(term_window: &TermWindow) -> Self {
        let fg = term_window.config.command_palette_fg_color.to_linear();
        let bg = term_window.config.command_palette_bg_color.to_linear();
        Self {
            fg,
            bg,
            muted: mix(fg, bg, 0.5),
        }
    }
}

/// What a submenu's entry shows on its right.
const SUBMENU_MARK: &str = "▸";

/// The box of one open level, at `depth`, and its width in pixels.
fn level_element(
    font: &std::rc::Rc<wezterm_font::LoadedFont>,
    metrics: &RenderMetrics,
    colors: &MenuColors,
    entries: &[Entry],
    depth: usize,
    selected: Option<usize>,
) -> (Element, f32) {
    let (fg, bg, muted) = (colors.fg, colors.bg, colors.muted);

    // Wide enough for the longest label and the longest shortcut or submenu
    // mark, with room between them, so neither sits against its label.
    let label_cells = entries
        .iter()
        .map(|entry| match entry {
            Entry::Item { label, .. } | Entry::Submenu { label, .. } => unicode_column_width(label),
            Entry::Separator => 0,
        })
        .max()
        .unwrap_or(0);
    let trailing_cells = entries
        .iter()
        .map(|entry| match entry {
            Entry::Item {
                shortcut: Some(shortcut),
                ..
            } => unicode_column_width(shortcut) + 3,
            Entry::Submenu { .. } => unicode_column_width(SUBMENU_MARK) + 3,
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let cell_width = metrics.cell_size.width as f32;
    let row_pixel_width = (label_cells + trailing_cells + 2) as f32 * cell_width;

    let mut rows = vec![];
    for (idx, entry) in entries.iter().enumerate() {
        let (label, trailing, enabled) = match entry {
            Entry::Separator => {
                rows.push(
                    Element::new(font, ElementContent::Children(vec![]))
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
                continue;
            }
            Entry::Item {
                label,
                shortcut,
                enabled,
                ..
            } => (label, shortcut.as_deref(), *enabled),
            Entry::Submenu { label, .. } => (label, Some(SUBMENU_MARK), true),
        };

        let (row_bg, row_text, trailing_text) = if selected == Some(idx) {
            (fg, bg, bg)
        } else if enabled {
            (LinearRgba::TRANSPARENT, fg, muted)
        } else {
            (LinearRgba::TRANSPARENT, muted, muted)
        };

        let mut children = vec![Element::new(font, ElementContent::Text(label.clone()))];
        if let Some(trailing) = trailing {
            children.push(
                Element::new(font, ElementContent::Text(trailing.to_string()))
                    .float(Float::Right)
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: row_bg.into(),
                        text: trailing_text.into(),
                    }),
            );
        }

        rows.push(
            Element::new(font, ElementContent::Children(children))
                .item_type(UIItemType::ContextMenuItem(depth, idx))
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

    let element = Element::new(font, ElementContent::Children(rows))
        .item_type(UIItemType::ContextMenu(depth))
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
    (element, row_pixel_width)
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

const ENTRY_KEYS: &str = "label, action, submenu and separator";

/// The entries an `augment-tab-bar-menu` handler returned: nil for none, or a
/// list of entries, each `{ label = ..., action = <key assignment> }`,
/// `{ label = ..., submenu = { <entries> } }` or `{ separator = true }`.
/// Anything else is refused, naming the entry and what is wrong with it.
pub fn user_entries(value: mlua::Value) -> anyhow::Result<Vec<UserEntry>> {
    match value {
        mlua::Value::Nil => Ok(vec![]),
        value => entry_list(value, &[]),
    }
}

/// How an error names the list at `within`, the labels of the submenus that
/// lead to it.
fn list_name(within: &[String]) -> String {
    if within.is_empty() {
        "the list the handler returned".to_string()
    } else {
        format!("the submenu {}", within.join(" ▸ "))
    }
}

fn key_name(key: &mlua::Value) -> String {
    match key {
        mlua::Value::String(s) => format!("{:?}", s.to_string_lossy()),
        mlua::Value::Integer(n) => n.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        other => format!("of type {}", other.type_name()),
    }
}

fn entry_list(value: mlua::Value, within: &[String]) -> anyhow::Result<Vec<UserEntry>> {
    let name = list_name(within);
    let table = match value {
        mlua::Value::Table(table) => table,
        other => bail!(
            "{name} is of type {}, not a list of entries",
            other.type_name()
        ),
    };
    let mut positions = BTreeMap::new();
    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (key, value) = pair?;
        match key {
            mlua::Value::Integer(n) if n >= 1 => {
                positions.insert(n, value);
            }
            key => bail!(
                "{name} has the key {}, where a list has only the positions 1, 2, 3 and so on",
                key_name(&key)
            ),
        }
    }
    let mut entries = vec![];
    for (expected, (position, value)) in (1..).zip(positions) {
        if position != expected {
            bail!("{name} has no entry {expected}, but has one at {position}");
        }
        let place = format!("entry {position} of {name}");
        entries.push(user_entry(value, within, &place)?);
    }
    Ok(entries)
}

fn user_entry(value: mlua::Value, within: &[String], place: &str) -> anyhow::Result<UserEntry> {
    let table = match value {
        mlua::Value::Table(table) => table,
        other => bail!("{place} is of type {}, not a table", other.type_name()),
    };
    let (mut label, mut action, mut submenu, mut separator) = (None, None, None, None);
    for pair in table.pairs::<mlua::Value, mlua::Value>() {
        let (key, value) = pair?;
        let field = match &key {
            mlua::Value::String(s) => s.to_str()?.to_string(),
            key => bail!(
                "{place} has the key {}; an entry's keys are {ENTRY_KEYS}",
                key_name(key)
            ),
        };
        match field.as_str() {
            "label" => label = Some(value),
            "action" => action = Some(value),
            "submenu" => submenu = Some(value),
            "separator" => separator = Some(value),
            other => bail!("{place} has the key {other:?}; an entry's keys are {ENTRY_KEYS}"),
        }
    }

    if let Some(separator) = separator {
        if !matches!(separator, mlua::Value::Boolean(true)) {
            bail!("{place} has a separator that is not true");
        }
        if label.is_some() || action.is_some() || submenu.is_some() {
            bail!("{place} is a separator, which has no label, action or submenu");
        }
        return Ok(UserEntry::Separator);
    }

    let label = match label {
        Some(mlua::Value::String(s)) => s.to_str()?.to_string(),
        Some(other) => bail!(
            "{place} has a label of type {}, not a string",
            other.type_name()
        ),
        None => bail!("{place} has no label"),
    };
    if label.is_empty() {
        bail!("{place} has an empty label");
    }
    let place = format!("{place} ({label:?})");
    match (action, submenu) {
        (Some(_), Some(_)) => {
            bail!("{place} has both an action and a submenu, and can have only one")
        }
        (None, None) => bail!("{place} has neither an action nor a submenu"),
        (Some(action), None) => {
            let action = from_lua_value_dynamic::<KeyAssignment>(action).map_err(|err| {
                anyhow!("{place} has an action that is not a key assignment: {err}")
            })?;
            Ok(UserEntry::Item { label, action })
        }
        (None, Some(submenu)) => {
            let mut inner = within.to_vec();
            inner.push(label.clone());
            let entries = entry_list(submenu, &inner)?;
            if !entries
                .iter()
                .any(|entry| !matches!(entry, UserEntry::Separator))
            {
                bail!("{} has nothing to choose", list_name(&inner));
            }
            Ok(UserEntry::Submenu { label, entries })
        }
    }
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
        let added = self.tab_bar_menu_additions(tab_idx);
        let menu = ContextMenu::for_tab_bar(self, tab_idx, anchor, added);
        self.set_modal(std::rc::Rc::new(menu));
    }

    /// The entries the configuration's `augment-tab-bar-menu` handler adds,
    /// called with the window, its active pane, and the index of the tab
    /// clicked (nil when the click was not on a tab). A handler that fails,
    /// or returns entries that are not as documented, is reported as other
    /// configuration errors are, and the menu opens without its entries.
    fn tab_bar_menu_additions(&self, tab_idx: Option<usize>) -> Vec<UserEntry> {
        let window = GuiWin::new(self);
        let pane = self
            .get_active_pane_or_overlay()
            .map(|pane| MuxPane(pane.pane_id()));
        let result = config::run_immediate_with_lua_config(|lua| {
            let lua = match lua {
                Some(lua) => lua,
                None => return Ok(vec![]),
            };
            let value = config::lua::emit_sync_callback(
                &*lua,
                (TAB_BAR_MENU_EVENT.to_string(), (window, pane, tab_idx)),
            )?;
            user_entries(value)
        });
        match result {
            Ok(entries) => entries,
            Err(err) => {
                let message = format!("while processing {TAB_BAR_MENU_EVENT} event: {err:#}");
                log::error!("{message}");
                config::show_error(&message);
                vec![]
            }
        }
    }

    /// Closes the menu and does what the entry at `path` says.
    pub fn choose_context_menu_entry(&mut self, menu: &ContextMenu, path: &[usize]) {
        let action = match menu.action(path) {
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
        let key = match (key, mods) {
            (KeyCode::Escape, _) => MenuKey::Escape,
            (KeyCode::UpArrow, KeyModifiers::NONE) => MenuKey::Up,
            (KeyCode::DownArrow, KeyModifiers::NONE) => MenuKey::Down,
            (KeyCode::LeftArrow, KeyModifiers::NONE) => MenuKey::Left,
            (KeyCode::RightArrow, KeyModifiers::NONE) => MenuKey::Right,
            (KeyCode::Enter, KeyModifiers::NONE) => MenuKey::Enter,
            // While the menu is open, typing goes nowhere: a keystroke meant
            // for the menu must not reach the program in the pane.
            _ => return Ok(true),
        };
        match self.key(key) {
            MenuOutcome::Unchanged => {}
            MenuOutcome::Redraw => term_window.invalidate_modal(),
            MenuOutcome::Choose(path) => term_window.choose_context_menu_entry(self, &path),
            MenuOutcome::Close => term_window.cancel_modal(),
        }
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

#[cfg(test)]
mod test {
    use super::*;

    fn lua_entries(source: &str) -> anyhow::Result<Vec<UserEntry>> {
        let lua = mlua::Lua::new();
        let value: mlua::Value = lua.load(source).eval()?;
        user_entries(value)
    }

    fn error(source: &str) -> String {
        format!("{:#}", lua_entries(source).unwrap_err())
    }

    fn send(text: &str) -> KeyAssignment {
        KeyAssignment::SendString(text.to_string())
    }

    #[test]
    fn a_handler_returns_items_submenus_and_separators() {
        let entries = lua_entries(
            r#"return {
                { label = 'Palette', action = 'ActivateCommandPalette' },
                { separator = true },
                { label = 'Prompts', submenu = {
                    { label = 'Daily', submenu = {
                        { label = 'Stand-up', action = { SendString = 'standup' } },
                    } },
                    { separator = true },
                    { label = 'Hello', action = { EmitEvent = 'user-defined-3' } },
                } },
            }"#,
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![
                UserEntry::Item {
                    label: "Palette".into(),
                    action: KeyAssignment::ActivateCommandPalette,
                },
                UserEntry::Separator,
                UserEntry::Submenu {
                    label: "Prompts".into(),
                    entries: vec![
                        UserEntry::Submenu {
                            label: "Daily".into(),
                            entries: vec![UserEntry::Item {
                                label: "Stand-up".into(),
                                action: send("standup"),
                            }],
                        },
                        UserEntry::Separator,
                        UserEntry::Item {
                            label: "Hello".into(),
                            action: KeyAssignment::EmitEvent("user-defined-3".into()),
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn a_handler_that_returns_nothing_adds_nothing() {
        assert_eq!(lua_entries("return nil").unwrap(), vec![]);
        assert_eq!(lua_entries("return {}").unwrap(), vec![]);
    }

    #[test]
    fn entries_not_as_documented_are_refused_naming_the_entry() {
        assert_eq!(
            error("return 'Prompts'"),
            "the list the handler returned is of type string, not a list of entries"
        );
        // One entry returned where a list of them belongs.
        let single = error("return { label = 'A', action = 'Nop' }");
        assert!(
            single.starts_with("the list the handler returned has the key \"")
                && single.ends_with(", where a list has only the positions 1, 2, 3 and so on"),
            "{}",
            single
        );
        assert_eq!(
            error("return { [1] = { separator = true }, [3] = { separator = true } }"),
            "the list the handler returned has no entry 2, but has one at 3"
        );
        assert_eq!(
            error("return { { action = 'Nop' } }"),
            "entry 1 of the list the handler returned has no label"
        );
        assert_eq!(
            error("return { { label = '', action = 'Nop' } }"),
            "entry 1 of the list the handler returned has an empty label"
        );
        assert_eq!(
            error("return { { label = 7, action = 'Nop' } }"),
            "entry 1 of the list the handler returned has a label of type integer, not a string"
        );
        assert_eq!(
            error("return { { lable = 'A' } }"),
            "entry 1 of the list the handler returned has the key \"lable\"; an entry's keys are \
             label, action, submenu and separator"
        );
        assert_eq!(
            error("return { { label = 'A' } }"),
            "entry 1 of the list the handler returned (\"A\") has neither an action nor a submenu"
        );
        assert_eq!(
            error("return { { label = 'A', action = 'Nop', submenu = {} } }"),
            "entry 1 of the list the handler returned (\"A\") has both an action and a submenu, \
             and can have only one"
        );
        assert_eq!(
            error("return { { separator = false } }"),
            "entry 1 of the list the handler returned has a separator that is not true"
        );
        assert_eq!(
            error("return { { separator = true, label = 'A' } }"),
            "entry 1 of the list the handler returned is a separator, which has no label, \
             action or submenu"
        );
        assert_eq!(
            error("return { 'A' }"),
            "entry 1 of the list the handler returned is of type string, not a table"
        );
        let action = error("return { { label = 'A', action = { NoSuchAction = 1 } } }");
        assert!(
            action.starts_with(
                "entry 1 of the list the handler returned (\"A\") has an action that is not a \
                 key assignment: "
            ),
            "{}",
            action
        );
    }

    #[test]
    fn submenus_name_the_way_to_a_bad_entry_and_must_offer_something() {
        assert_eq!(
            error(
                "return { { label = 'Prompts', submenu = { { label = 'Daily', submenu = { \
                 { separator = true }, { label = 'B' } } } } } }"
            ),
            "entry 2 of the submenu Prompts ▸ Daily (\"B\") has neither an action nor a submenu"
        );
        assert_eq!(
            error("return { { label = 'Prompts', submenu = {} } }"),
            "the submenu Prompts has nothing to choose"
        );
        assert_eq!(
            error("return { { label = 'Prompts', submenu = { { separator = true } } } }"),
            "the submenu Prompts has nothing to choose"
        );
        assert_eq!(
            error("return { { label = 'Prompts', submenu = 'none' } }"),
            "the submenu Prompts is of type string, not a list of entries"
        );
    }

    /// A menu as the tab bar's is built: its own entries, then the
    /// configuration's after a separator.
    fn menu() -> ContextMenu {
        let mut menu = ContextMenu::new((0, 0));
        menu.item("New Tab", None, MenuAction::Assignment(send("new")), true);
        menu.item("Disabled", None, MenuAction::Minimize, false);
        menu.append(
            vec![UserEntry::Submenu {
                label: "Prompts".into(),
                entries: vec![
                    UserEntry::Submenu {
                        label: "Daily".into(),
                        entries: vec![
                            UserEntry::Item {
                                label: "Stand-up".into(),
                                action: send("standup"),
                            },
                            UserEntry::Item {
                                label: "Review".into(),
                                action: send("review"),
                            },
                        ],
                    },
                    UserEntry::Separator,
                    UserEntry::Item {
                        label: "Hello".into(),
                        action: send("hello"),
                    },
                ],
            }],
            &|action| (*action == send("hello")).then(|| "CTRL-H".to_string()),
        );
        menu
    }

    fn cursor(menu: &ContextMenu) -> Vec<Option<usize>> {
        menu.cursor.borrow().clone()
    }

    #[test]
    fn the_configurations_entries_follow_a_separator_with_their_shortcuts() {
        let menu = menu();
        assert!(matches!(menu.entries[2], Entry::Separator));
        match &menu.entries[3] {
            Entry::Submenu { label, entries } => {
                assert_eq!(label, "Prompts");
                match &entries[2] {
                    Entry::Item {
                        label, shortcut, ..
                    } => assert_eq!(
                        (label.as_str(), shortcut.as_deref()),
                        ("Hello", Some("CTRL-H"))
                    ),
                    other => panic!("{:?}", other),
                }
            }
            other => panic!("{:?}", other),
        }
        assert_eq!(
            menu.action(&[3, 0, 1]),
            Some(MenuAction::Assignment(send("review")))
        );
        // A submenu, a separator and a disabled item do nothing when chosen.
        assert_eq!(menu.action(&[3]), None);
        assert_eq!(menu.action(&[3, 1]), None);
        assert_eq!(menu.action(&[1]), None);
        assert_eq!(menu.action(&[3, 0, 5]), None);

        let mut empty = ContextMenu::new((0, 0));
        empty.append(vec![], &|_| None);
        assert!(empty.entries.is_empty());
    }

    #[test]
    fn hovering_a_submenu_opens_it_and_hovering_elsewhere_closes_it() {
        let menu = menu();
        assert!(menu.hover(Some((0, 3))));
        assert_eq!(cursor(&menu), vec![Some(3), None]);
        assert!(menu.hover(Some((1, 0))));
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), None]);
        assert!(menu.hover(Some((2, 1))));
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), Some(1)]);
        // Leaving the menus keeps the submenus open.
        assert!(menu.hover(None));
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), None]);
        assert!(!menu.hover(None));
        // A separator highlights nothing, and closes what was open past it.
        assert!(menu.hover(Some((1, 1))));
        assert_eq!(cursor(&menu), vec![Some(3), None]);
        // A level that is not open is not under the pointer.
        assert!(!menu.hover(Some((2, 0))));
        // Back on the menu itself, the submenu closes.
        assert!(menu.hover(Some((0, 0))));
        assert_eq!(cursor(&menu), vec![Some(0)]);
        // A disabled item highlights nothing.
        assert!(menu.hover(Some((0, 1))));
        assert_eq!(cursor(&menu), vec![None]);
    }

    #[test]
    fn a_release_chooses_an_item_in_a_submenu_and_opens_a_submenu() {
        let menu = menu();
        menu.hover(Some((0, 3)));
        // The release ending the press that opened the menu chooses nothing.
        assert_eq!(menu.release(1, 2), MenuOutcome::Unchanged);
        menu.arm();
        assert_eq!(menu.release(1, 2), MenuOutcome::Choose(vec![3, 2]));
        assert_eq!(menu.release(1, 0), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), None]);
        assert_eq!(menu.release(1, 0), MenuOutcome::Unchanged);
        assert_eq!(menu.release(2, 0), MenuOutcome::Choose(vec![3, 0, 0]));
        assert_eq!(menu.release(1, 1), MenuOutcome::Unchanged);
        assert_eq!(menu.release(0, 1), MenuOutcome::Unchanged);
        assert_eq!(menu.release(3, 0), MenuOutcome::Unchanged);
    }

    #[test]
    fn the_keyboard_walks_into_and_out_of_submenus() {
        let menu = menu();
        // Down from nothing highlights the first entry, skipping what cannot be chosen.
        assert_eq!(menu.key(MenuKey::Down), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(0)]);
        assert_eq!(menu.key(MenuKey::Down), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3)]);
        assert_eq!(menu.key(MenuKey::Left), MenuOutcome::Unchanged);
        // Right opens the submenu at its first entry.
        assert_eq!(menu.key(MenuKey::Right), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0)]);
        assert_eq!(menu.key(MenuKey::Down), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(2)]);
        assert_eq!(menu.key(MenuKey::Down), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0)]);
        // Enter opens a submenu too.
        assert_eq!(menu.key(MenuKey::Enter), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), Some(0)]);
        assert_eq!(menu.key(MenuKey::Up), MenuOutcome::Redraw);
        assert_eq!(menu.key(MenuKey::Right), MenuOutcome::Unchanged);
        assert_eq!(menu.key(MenuKey::Enter), MenuOutcome::Choose(vec![3, 0, 1]));
        // Left and Escape close one level at a time; Escape then closes the menu.
        assert_eq!(menu.key(MenuKey::Left), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0)]);
        assert_eq!(menu.key(MenuKey::Escape), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3)]);
        assert_eq!(menu.key(MenuKey::Escape), MenuOutcome::Close);
    }

    #[test]
    fn keys_after_hovering_act_on_the_level_with_the_highlight() {
        let menu = menu();
        // Hovering opens the submenu without entering it: Down moves on in
        // the menu itself, closing the submenu.
        menu.hover(Some((0, 3)));
        assert_eq!(menu.key(MenuKey::Down), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(0)]);
        // Right enters a submenu that hovering opened.
        menu.hover(Some((0, 3)));
        assert_eq!(menu.key(MenuKey::Right), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0)]);
        // Escape closes a submenu that hovering opened.
        menu.hover(Some((1, 0)));
        assert_eq!(cursor(&menu), vec![Some(3), Some(0), None]);
        assert_eq!(menu.key(MenuKey::Escape), MenuOutcome::Redraw);
        assert_eq!(cursor(&menu), vec![Some(3), Some(0)]);
        // With nothing highlighted, Enter does nothing.
        menu.key(MenuKey::Left);
        menu.hover(None);
        assert_eq!(cursor(&menu), vec![None]);
        assert_eq!(menu.key(MenuKey::Enter), MenuOutcome::Unchanged);
        assert_eq!(menu.key(MenuKey::Right), MenuOutcome::Unchanged);
    }

    const WINDOW: Size = Size {
        width: 800.,
        height: 600.,
    };

    #[test]
    fn the_menu_opens_at_the_pointer_inside_the_window() {
        let size = Size {
            width: 200.,
            height: 300.,
        };
        assert_eq!(place_menu((10., 20.), size, WINDOW), (10., 20.));
        assert_eq!(place_menu((700., 20.), size, WINDOW), (600., 20.));
        // Near the bottom it opens upwards from the pointer.
        assert_eq!(place_menu((10., 500.), size, WINDOW), (10., 200.));
        // With room neither below nor above, it is pushed against the bottom.
        let tall = Size {
            width: 200.,
            height: 550.,
        };
        assert_eq!(place_menu((10., 100.), tall, WINDOW), (10., 50.));
    }

    #[test]
    fn a_submenu_opens_beside_its_entry_inside_the_window() {
        let size = Size {
            width: 200.,
            height: 100.,
        };
        assert_eq!(place_submenu((10., 210.), 50., size, WINDOW), (210., 50.));
        // No room on the right: it opens on the left of the menu.
        assert_eq!(place_submenu((500., 700.), 50., size, WINDOW), (300., 50.));
        // No room on either side: against the right edge.
        let wide = Size {
            width: 450.,
            height: 100.,
        };
        assert_eq!(place_submenu((300., 500.), 50., wide, WINDOW), (350., 50.));
        // Past the bottom it moves up.
        assert_eq!(place_submenu((10., 210.), 550., size, WINDOW), (210., 500.));
        assert_eq!(place_submenu((10., 210.), -4., size, WINDOW), (210., 0.));
    }
}
