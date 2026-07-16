//! Cell/run drawing for the GDI text front_end.
//!
//! Walks the visible lines of each pane and produces GDI draw commands: an
//! `ExtTextOutW` with `ETO_OPAQUE` per cell run (which paints the background and
//! the glyph in a single call, so RDP remotes it as text), plus manually drawn
//! underline/strikethrough and the cursor.
//!
//! Phase 1 is a full-redraw baseline: every visible cell is emitted each paint.
//! Phase 2 will restrict this to dirty lines.

use super::font::FontStyleKey;
use super::{colorref, GdiState};
use crate::termwindow::TermWindow;
use config::DimensionContext;
use termwiz::cell::{CellAttributes, Intensity, Underline};
use termwiz::surface::{CursorShape, CursorVisibility};
use wezterm_term::color::{ColorAttribute, ColorPalette};
use wezterm_term::StableRowIndex;
use winapi::shared::windef::{HDC, RECT};
use winapi::um::wingdi::{
    CreateSolidBrush, DeleteObject, ExtTextOutW, SelectObject, SetBkColor, SetTextColor,
    ETO_CLIPPED, ETO_OPAQUE,
};
use winapi::um::winuser::FillRect;

/// Resolve a cell's foreground/background to GDI `COLORREF`s, applying the
/// default-color fast path and reverse-video swap. Selection override (if any)
/// is layered on by the caller.
fn resolve_fg_bg(
    palette: &ColorPalette,
    attrs: &CellAttributes,
    default_fg: u32,
    default_bg: u32,
) -> (u32, u32) {
    let fg_attr = attrs.foreground();
    let bg_attr = attrs.background();
    let mut fg = if fg_attr == ColorAttribute::Default {
        default_fg
    } else {
        colorref(palette.resolve_fg(fg_attr))
    };
    let mut bg = if bg_attr == ColorAttribute::Default {
        default_bg
    } else {
        colorref(palette.resolve_bg(bg_attr))
    };
    if attrs.reverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

/// A single drawable cell run. For Phase 1 each cell is emitted individually so
/// that combining characters and wide glyphs stay correct without a per-glyph
/// `dx` array; run batching is a Phase 2 optimization.
pub(crate) struct GdiCell {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    text: Vec<u16>,
    fg: u32,
    bg: u32,
    style: FontStyleKey,
    underline: bool,
    strike: bool,
}

/// A cursor to overlay after the cells are drawn.
pub(crate) struct GdiCursor {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    shape: CursorShape,
    bg: u32,
    fg: u32,
    focused: bool,
    text: Vec<u16>,
    text_style: FontStyleKey,
}

#[derive(Default)]
pub(crate) struct GdiFrameData {
    pub cells: Vec<GdiCell>,
    pub cursors: Vec<GdiCursor>,
    /// When true, the caller should clear the whole client area before drawing
    /// (full redraw). When false, only changed cells are present and the
    /// previous frame's pixels are left in place (incremental).
    pub full_clear: bool,
}

impl TermWindow {
    /// Collect the GDI draw commands for the current frame. Uses GDI cell
    /// metrics (`cw`/`ch`) for pixel positioning.
    pub(crate) fn gdi_collect_frame(&mut self, cw: i32, ch: i32) -> GdiFrameData {
        let mut data = GdiFrameData::default();

        // Rebuild mouse hit-test regions from scratch each frame, mirroring the
        // GPU paint_pass which clears `ui_items` at the start of every paint.
        self.ui_items.clear();

        let panes = self.get_panes_to_render();
        if panes.is_empty() {
            // Nothing to render (e.g. window tearing down); clear so we don't
            // leave the previous frame's pixels on screen.
            data.full_clear = true;
            if let Some(g) = self.gdi.as_mut() {
                g.pane_states.clear();
                g.last_layout.clear();
                g.needs_full_paint = false;
            }
            return data;
        }

        // Phase 2 damage tracking: pull the per-pane paint state out of GdiState
        // so we can borrow `self` for pane/selection access, then write it back.
        let needs_full = self
            .gdi
            .as_ref()
            .map(|g| g.needs_full_paint)
            .unwrap_or(true);
        let mut pane_states = self
            .gdi
            .as_mut()
            .map(|g| std::mem::take(&mut g.pane_states))
            .unwrap_or_default();

        // Layout signature: a change (split/close/move/zoom) can relocate
        // unchanged lines, so force a full redraw when it differs.
        let layout: Vec<(mux::pane::PaneId, usize, usize, usize, usize, bool)> = panes
            .iter()
            .map(|p| {
                (
                    p.pane.pane_id(),
                    p.left,
                    p.top,
                    p.width,
                    p.height,
                    p.is_active,
                )
            })
            .collect();
        let layout_changed = self
            .gdi
            .as_ref()
            .map(|g| g.last_layout != layout)
            .unwrap_or(true);

        // Pre-pass: a viewport-top change means the pane scrolled, which seqno
        // tracking can't express (moved lines keep their old seqno). Any scroll,
        // newly-seen pane, or layout change forces a full-frame clear+redraw so
        // we never leave stale pixels or mix a clear with per-pane incremental
        // draws.
        let mut full = needs_full || layout_changed;
        for pos in &panes {
            let pane_id = pos.pane.pane_id();
            let dims = pos.pane.get_dimensions();
            let stable_top = self.get_viewport(pane_id).unwrap_or(dims.physical_top);
            match pane_states.get(&pane_id) {
                Some(s) if s.top == stable_top => {}
                _ => full = true,
            }
        }
        data.full_clear = full;

        // Ensure the palette is initialized, then borrow it (avoid cloning the
        // whole 256-entry palette every frame).
        self.palette();
        let palette = self.palette.as_ref().unwrap();
        let focused = self.focused.is_some();

        // Window padding (in the GDI metric space) and tab bar reservation.
        let dpi = self.dimensions.dpi as f32;
        let ctx_h = DimensionContext {
            dpi,
            pixel_max: self.dimensions.pixel_width as f32,
            pixel_cell: cw as f32,
        };
        let ctx_v = DimensionContext {
            dpi,
            pixel_max: self.dimensions.pixel_height as f32,
            pixel_cell: ch as f32,
        };
        let pad_left = self.config.window_padding.left.evaluate_as_pixels(ctx_h) as i32;
        let pad_top = self.config.window_padding.top.evaluate_as_pixels(ctx_v) as i32;
        let tab_at_bottom = self.config.tab_bar_at_bottom;
        let tab_bar_h = if self.show_tab_bar && !tab_at_bottom {
            ch
        } else {
            0
        };

        let default_fg = colorref(palette.foreground);
        let default_bg = colorref(palette.background);
        let sel_fg = colorref(palette.selection_fg);
        let sel_bg = colorref(palette.selection_bg);
        let cursor_bg = colorref(palette.cursor_bg);
        let cursor_fg = colorref(palette.cursor_fg);

        for pos in &panes {
            let pane = &pos.pane;
            let pane_id = pane.pane_id();

            let dims = pane.get_dimensions();
            let viewport = self.get_viewport(pane_id);
            let stable_top = viewport.unwrap_or(dims.physical_top);
            let stable_range = stable_top..stable_top + pos.height as isize;

            let (first_row, lines) = pane.get_lines(stable_range);

            // Selection for this pane.
            let (sel_range, sel_rect) = {
                let sel = self.selection(pane_id);
                (sel.range, sel.rectangular)
            };

            // Cursor (only for the active pane, when visible).
            let cursor = pane.get_cursor_position();
            let cursor_visible = pos.is_active && cursor.visibility == CursorVisibility::Visible;

            // Damage tracking: which stable rows must be redrawn this frame.
            let cur_seqno = pane.get_current_seqno();
            let prev_state = pane_states.get(&pane_id).cloned().unwrap_or_default();
            let changed = if full {
                None
            } else {
                Some(pane.get_changed_since(
                    first_row..first_row + lines.len() as isize,
                    prev_state.last_seqno,
                ))
            };

            // Current selection (range + rectangular) and whether it changed.
            let cur_sel = sel_range.map(|r| (r, sel_rect));
            let sel_changed = cur_sel != prev_state.selection;
            let cur_sel_rows = sel_range.map(|r| r.rows());
            let prev_sel_rows = prev_state.selection.as_ref().map(|(r, _)| r.rows());

            // A row is dirty if: content changed since last paint, the cursor is
            // on it now or was last frame, or its selection state changed
            // (including horizontal changes on an already-selected row).
            let row_is_dirty = |stable_row: StableRowIndex| -> bool {
                if full {
                    return true;
                }
                if let Some(changed) = &changed {
                    if changed.contains(stable_row) {
                        return true;
                    }
                }
                if cursor_visible && cursor.y == stable_row {
                    return true;
                }
                if let Some((_, prev_row)) = prev_state.cursor {
                    if prev_row == stable_row {
                        return true;
                    }
                }
                if sel_changed {
                    let in_now = cur_sel_rows
                        .as_ref()
                        .map(|r| r.contains(&stable_row))
                        .unwrap_or(false);
                    let in_prev = prev_sel_rows
                        .as_ref()
                        .map(|r| r.contains(&stable_row))
                        .unwrap_or(false);
                    if in_now || in_prev {
                        return true;
                    }
                }
                false
            };

            let origin_x = pad_left + pos.left as i32 * cw;
            let origin_y = pad_top + tab_bar_h + pos.top as i32 * ch;

            for (line_idx, line) in lines.iter().enumerate() {
                let stable_row = first_row + line_idx as isize;
                if !row_is_dirty(stable_row) {
                    continue;
                }
                let y = origin_y + line_idx as i32 * ch;

                let sel_cols = sel_range.and_then(|r| {
                    if r.rows().contains(&stable_row) {
                        Some(r.cols_for_row(stable_row, sel_rect))
                    } else {
                        None
                    }
                });

                let mut next_col = 0usize;
                for cell in line.visible_cells() {
                    let idx = cell.cell_index();
                    let width = cell.width().max(1);
                    next_col = idx + width;
                    let x = origin_x + idx as i32 * cw;
                    let w = width as i32 * cw;

                    let attrs = cell.attrs();

                    let (mut fg_ref, mut bg_ref) =
                        resolve_fg_bg(&palette, attrs, default_fg, default_bg);

                    // Selection overrides colors.
                    let selected = sel_cols
                        .as_ref()
                        .map(|cols| cols.contains(&idx))
                        .unwrap_or(false);
                    if selected {
                        fg_ref = sel_fg;
                        bg_ref = sel_bg;
                    }

                    let bold = attrs.intensity() == Intensity::Bold;
                    let italic = attrs.italic();
                    let style = super::font::GdiFonts::style_for(bold, italic);

                    let text: Vec<u16> = if attrs.invisible() {
                        Vec::new()
                    } else {
                        cell.str().encode_utf16().collect()
                    };

                    data.cells.push(GdiCell {
                        x,
                        y,
                        w,
                        h: ch,
                        text,
                        fg: fg_ref,
                        bg: bg_ref,
                        style,
                        underline: attrs.underline() != Underline::None,
                        strike: attrs.strikethrough(),
                    });
                }

                // Cover trailing columns past the end of the line content. A
                // `Line` may omit trailing default blanks, so on an incremental
                // frame stale glyphs/backgrounds could remain; emit one opaque
                // blank span (default background) across the remaining width.
                if next_col < pos.width {
                    let x = origin_x + next_col as i32 * cw;
                    let w = (pos.width - next_col) as i32 * cw;
                    data.cells.push(GdiCell {
                        x,
                        y,
                        w,
                        h: ch,
                        text: Vec::new(),
                        fg: default_fg,
                        bg: default_bg,
                        style: super::font::FontStyleKey::Regular,
                        underline: false,
                        strike: false,
                    });
                }
            }

            if cursor_visible {
                // Locate the cursor cell relative to the rendered viewport.
                // Use `first_row` (the range actually returned by get_lines,
                // which may be clamped at the scrollback top) so the cursor row
                // and glyph lookup match the cell loop's `first_row + line_idx`.
                let screen_row = cursor.y - first_row;
                if screen_row >= 0 && (screen_row as usize) < lines.len() {
                    let x = origin_x + cursor.x as i32 * cw;
                    let y = origin_y + screen_row as i32 * ch;

                    // Recover the glyph under the cursor for block redraw.
                    let (glyph, glyph_style) = lines
                        .get(screen_row as usize)
                        .and_then(|line| {
                            line.visible_cells()
                                .find(|c| c.cell_index() == cursor.x)
                                .map(|c| {
                                    let a = c.attrs();
                                    (
                                        c.str().encode_utf16().collect::<Vec<u16>>(),
                                        super::font::GdiFonts::style_for(
                                            a.intensity() == Intensity::Bold,
                                            a.italic(),
                                        ),
                                    )
                                })
                        })
                        .unwrap_or_else(|| (Vec::new(), FontStyleKey::Regular));

                    data.cursors.push(GdiCursor {
                        x,
                        y,
                        w: cw,
                        h: ch,
                        shape: self
                            .config
                            .default_cursor_style
                            .effective_shape(cursor.shape),
                        bg: cursor_bg,
                        fg: cursor_fg,
                        focused,
                        text: glyph,
                        text_style: glyph_style,
                    });
                }
            }

            // Record this pane's paint state for the next frame's damage calc.
            pane_states.insert(
                pane_id,
                super::PanePaintState {
                    last_seqno: cur_seqno,
                    top: stable_top,
                    cursor: if cursor_visible {
                        Some((cursor.x, cursor.y))
                    } else {
                        None
                    },
                    selection: cur_sel,
                },
            );
        }

        // Tab strip: render the formatted tab-bar Line (which already carries
        // the format-tab-title colors) as a single row at the top or bottom.
        if self.show_tab_bar {
            let y = if tab_at_bottom {
                (self.dimensions.pixel_height as i32 - ch).max(0)
            } else {
                0
            };

            // Register tab-bar mouse hit regions every frame (ui_items were
            // cleared above and are consumed by mouse hit-testing), regardless
            // of whether the strip pixels need redrawing this frame.
            let mut items = self.tab_bar.compute_ui_items(
                y.max(0) as usize,
                ch.max(1) as usize,
                cw.max(1) as usize,
            );
            self.ui_items.append(&mut items);

            // Compare by reference (no clone) to decide whether to redraw pixels.
            let tab_changed = full
                || self
                    .gdi
                    .as_ref()
                    .map(|g| g.last_tab_bar.as_ref() != Some(self.tab_bar.line()))
                    .unwrap_or(true);
            if tab_changed {
                let mut next_col = 0usize;
                for cell in self.tab_bar.line().visible_cells() {
                    let idx = cell.cell_index();
                    let width = cell.width().max(1);
                    next_col = idx + width;
                    let x = idx as i32 * cw;
                    let w = width as i32 * cw;
                    let attrs = cell.attrs();

                    let (fg_ref, bg_ref) = resolve_fg_bg(&palette, attrs, default_fg, default_bg);

                    data.cells.push(GdiCell {
                        x,
                        y,
                        w,
                        h: ch,
                        text: cell.str().encode_utf16().collect(),
                        fg: fg_ref,
                        bg: bg_ref,
                        style: super::font::GdiFonts::style_for(
                            attrs.intensity() == Intensity::Bold,
                            attrs.italic(),
                        ),
                        underline: attrs.underline() != Underline::None,
                        strike: attrs.strikethrough(),
                    });
                }

                // Cover trailing columns past the end of the tab line so a
                // shorter tab bar on an incremental frame doesn't leave stale
                // pixels to the right.
                let total_cols = (self.dimensions.pixel_width / cw.max(1) as usize).max(0);
                if next_col < total_cols {
                    let x = next_col as i32 * cw;
                    let w = (total_cols - next_col) as i32 * cw;
                    data.cells.push(GdiCell {
                        x,
                        y,
                        w,
                        h: ch,
                        text: Vec::new(),
                        fg: default_fg,
                        bg: default_bg,
                        style: super::font::FontStyleKey::Regular,
                        underline: false,
                        strike: false,
                    });
                }

                if let Some(g) = self.gdi.as_mut() {
                    g.last_tab_bar = Some(self.tab_bar.line().clone());
                }
            }
        }

        // Write back the damage-tracking state and clear the full-paint flag.
        // Drop paint state for panes that are no longer present.
        let live_ids: std::collections::HashSet<mux::pane::PaneId> =
            layout.iter().map(|l| l.0).collect();
        pane_states.retain(|id, _| live_ids.contains(id));
        if let Some(g) = self.gdi.as_mut() {
            g.pane_states = pane_states;
            g.last_layout = layout;
            g.needs_full_paint = false;
        }

        data
    }
}

impl GdiState {
    /// Render the collected frame data to `hdc`. Assumes the background has
    /// already been cleared and text alignment/bk mode are set.
    pub(crate) unsafe fn render_frame(&self, hdc: HDC, data: &GdiFrameData) {
        for cell in &data.cells {
            self.draw_cell(hdc, cell);
        }
        for cursor in &data.cursors {
            self.draw_cursor(hdc, cursor);
        }
    }

    unsafe fn draw_cell(&self, hdc: HDC, cell: &GdiCell) {
        let fonts = match &self.fonts {
            Some(f) => f,
            None => return,
        };

        let rect = RECT {
            left: cell.x,
            top: cell.y,
            right: cell.x + cell.w,
            bottom: cell.y + cell.h,
        };

        SetTextColor(hdc, cell.fg);
        SetBkColor(hdc, cell.bg);
        let prev = fonts.select(hdc, cell.style);

        ExtTextOutW(
            hdc,
            cell.x,
            cell.y,
            ETO_OPAQUE | ETO_CLIPPED,
            &rect,
            cell.text.as_ptr(),
            cell.text.len() as u32,
            std::ptr::null(),
        );

        SelectObject(hdc, prev);

        if cell.underline {
            self.fill_line(hdc, cell.x, cell.y + cell.h - 2, cell.w, 1, cell.fg);
        }
        if cell.strike {
            self.fill_line(hdc, cell.x, cell.y + cell.h / 2, cell.w, 1, cell.fg);
        }
    }

    unsafe fn draw_cursor(&self, hdc: HDC, cursor: &GdiCursor) {
        // When the window is not focused, every cursor shape renders as a hollow
        // block outline (matching the GPU renderer and the docs).
        if !cursor.focused {
            self.fill_line(hdc, cursor.x, cursor.y, cursor.w, 1, cursor.bg);
            self.fill_line(
                hdc,
                cursor.x,
                cursor.y + cursor.h - 1,
                cursor.w,
                1,
                cursor.bg,
            );
            self.fill_line(hdc, cursor.x, cursor.y, 1, cursor.h, cursor.bg);
            self.fill_line(
                hdc,
                cursor.x + cursor.w - 1,
                cursor.y,
                1,
                cursor.h,
                cursor.bg,
            );
            return;
        }
        match cursor.shape {
            CursorShape::BlinkingBar | CursorShape::SteadyBar => {
                self.fill_line(hdc, cursor.x, cursor.y, 2, cursor.h, cursor.bg);
            }
            CursorShape::BlinkingUnderline | CursorShape::SteadyUnderline => {
                self.fill_line(
                    hdc,
                    cursor.x,
                    cursor.y + cursor.h - 2,
                    cursor.w,
                    2,
                    cursor.bg,
                );
            }
            // Default and block shapes (focused): filled block with inverted
            // glyph.
            _ => {
                let rect = RECT {
                    left: cursor.x,
                    top: cursor.y,
                    right: cursor.x + cursor.w,
                    bottom: cursor.y + cursor.h,
                };
                SetTextColor(hdc, cursor.fg);
                SetBkColor(hdc, cursor.bg);
                if let Some(fonts) = &self.fonts {
                    let prev = fonts.select(hdc, cursor.text_style);
                    ExtTextOutW(
                        hdc,
                        cursor.x,
                        cursor.y,
                        ETO_OPAQUE | ETO_CLIPPED,
                        &rect,
                        cursor.text.as_ptr(),
                        cursor.text.len() as u32,
                        std::ptr::null(),
                    );
                    SelectObject(hdc, prev);
                }
            }
        }
    }

    unsafe fn fill_line(&self, hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let rect = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let brush = CreateSolidBrush(color);
        if !brush.is_null() {
            FillRect(hdc, &rect, brush);
            DeleteObject(brush as _);
        }
    }
}
