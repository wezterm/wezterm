use super::*;
use mux::domain::DomainId;
use mux::pane::{CachePolicy, ForEachPaneLogicalLine, LogicalLine, PaneId, WithPaneLines};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use parking_lot::{MappedMutexGuard, Mutex};
use rangeset::RangeSet;
use termwiz::surface::{Line, SEQ_ZERO};
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent, TerminalSize};

/// Just enough of a Pane to serve lines to the selection code
struct FakePane {
    lines: Mutex<Vec<Line>>,
}

impl FakePane {
    fn from_text(text: &str) -> Self {
        Self {
            lines: Mutex::new(vec![Line::from_text(
                text,
                &Default::default(),
                SEQ_ZERO,
                None,
            )]),
        }
    }
}

impl Pane for FakePane {
    fn pane_id(&self) -> PaneId {
        unimplemented!()
    }
    fn get_cursor_position(&self) -> StableCursorPosition {
        unimplemented!()
    }
    fn get_current_seqno(&self) -> SequenceNo {
        unimplemented!()
    }
    fn get_changed_since(
        &self,
        _: Range<StableRowIndex>,
        _: SequenceNo,
    ) -> RangeSet<StableRowIndex> {
        unimplemented!()
    }
    fn with_lines_mut(
        &self,
        stable_range: Range<StableRowIndex>,
        with_lines: &mut dyn WithPaneLines,
    ) {
        let mut line_refs = vec![];
        let mut lines = self.lines.lock();
        for line in lines
            .iter_mut()
            .skip(stable_range.start as usize)
            .take((stable_range.end - stable_range.start) as usize)
        {
            line_refs.push(line);
        }
        with_lines.with_lines_mut(stable_range.start, &mut line_refs);
    }
    fn for_each_logical_line_in_stable_range_mut(
        &self,
        lines: Range<StableRowIndex>,
        for_line: &mut dyn ForEachPaneLogicalLine,
    ) {
        mux::pane::impl_for_each_logical_line_via_get_logical_lines(self, lines, for_line)
    }
    fn get_logical_lines(&self, lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        mux::pane::impl_get_logical_lines_via_get_lines(self, lines)
    }
    fn get_lines(&self, lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        let first = lines.start;
        (
            first,
            self.lines
                .lock()
                .iter()
                .skip(lines.start as usize)
                .take((lines.end - lines.start) as usize)
                .cloned()
                .collect(),
        )
    }
    fn get_dimensions(&self) -> RenderableDimensions {
        unimplemented!()
    }
    fn get_title(&self) -> String {
        unimplemented!()
    }
    fn send_paste(&self, _: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
        Ok(None)
    }
    fn writer(&self) -> MappedMutexGuard<'_, dyn std::io::Write> {
        unimplemented!()
    }
    fn resize(&self, _: TerminalSize) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn mouse_event(&self, _: MouseEvent) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn is_dead(&self) -> bool {
        unimplemented!()
    }
    fn palette(&self) -> ColorPalette {
        unimplemented!()
    }
    fn domain_id(&self) -> DomainId {
        unimplemented!()
    }
    fn is_mouse_grabbed(&self) -> bool {
        false
    }
    fn is_alt_screen_active(&self) -> bool {
        false
    }
    fn get_current_working_dir(&self, _: CachePolicy) -> Option<Url> {
        None
    }
    fn key_down(&self, _: KeyCode, _: KeyModifiers) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn key_up(&self, _: KeyCode, _: KeyModifiers) -> anyhow::Result<()> {
        unimplemented!()
    }
}

fn word_at(text: &str, x: usize) -> SelectionRange {
    SelectionRange::word_around(SelectionCoordinate::x_y(x, 0), &FakePane::from_text(text))
}

/// Clicking inside a word selects the whole word
#[test]
fn word_around_selects_word() {
    assert_eq!(
        word_at("foo bar", 5),
        SelectionRange {
            start: SelectionCoordinate::x_y(4, 0),
            end: SelectionCoordinate::x_y(6, 0),
        }
    );
}

/// Column 0 of an empty line yields an empty double click range.
/// This used to evaluate `0 - 1` and underflow.
#[test]
fn word_around_empty_line_column_zero() {
    let start = SelectionCoordinate::x_y(0, 0);
    assert_eq!(word_at("", 0), SelectionRange { start, end: start });
}

/// The same underflow when column 0 holds a word boundary character
#[test]
fn word_around_boundary_char_column_zero() {
    let start = SelectionCoordinate::x_y(0, 0);
    assert_eq!(word_at(" foo", 0), SelectionRange { start, end: start });
}
