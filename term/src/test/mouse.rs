//! Tests for mouse reporting.
//!
//! These exercise the escape sequences that the terminal writes back to the
//! application when mouse tracking modes are enabled. Everything here operates
//! on the terminal model alone, so it needs no window system.

use super::*;
use crate::input::{MouseButton, MouseEvent, MouseEventKind};
use k9::assert_equal;
use std::io::Write;
use std::time::{Duration, Instant};

/// How long to wait for the writer thread to go quiet before comparing output.
const SETTLE: Duration = Duration::from_millis(100);

/// Upper bound on how long to wait for expected output to show up. Only ever
/// reached when the assertion is going to fail anyway.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Captures everything the terminal writes towards the application.
#[derive(Clone, Debug, Default)]
struct Recorder {
    data: Arc<Mutex<Vec<u8>>>,
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.data.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MouseTerm {
    term: Terminal,
    rec: Recorder,
}

impl MouseTerm {
    fn new() -> Self {
        let _ = env_logger::Builder::new()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let rec = Recorder::default();
        let term = Terminal::new(
            TerminalSize {
                rows: 24,
                cols: 80,
                // 8x16 cells, which keeps the SGR-Pixels arithmetic easy
                pixel_width: 80 * 8,
                pixel_height: 24 * 16,
                dpi: 0,
            },
            Arc::new(TestTermConfig { scrollback: 0 }),
            "WezTerm",
            "O_o",
            Box::new(rec.clone()),
        );
        Self { term, rec }
    }

    /// Enable the same set of mouse modes that tmux enables:
    /// 1000 (VT200 click reporting), 1002 (button-event/drag reporting)
    /// and 1006 (SGR encoding).
    fn enable_tmux_style_mouse(&mut self) {
        self.enable_mouse(b"\x1b[?1000;1002;1006h");
    }

    fn enable_mouse(&mut self, decset: &[u8]) {
        self.term.advance_bytes(decset);
        assert!(
            self.term.is_mouse_grabbed(),
            "mouse reporting should be enabled; otherwise these tests would \
             pass without exercising anything"
        );
        self.discard_output();
    }

    fn mouse(&mut self, kind: MouseEventKind, button: MouseButton, x: usize, y: i64) {
        self.mouse_at_pixel(kind, button, x, y, 0, 0)
    }

    fn mouse_at_pixel(
        &mut self,
        kind: MouseEventKind,
        button: MouseButton,
        x: usize,
        y: i64,
        x_pixel_offset: isize,
        y_pixel_offset: isize,
    ) {
        self.term
            .mouse_event(MouseEvent {
                kind,
                x,
                y,
                x_pixel_offset,
                y_pixel_offset,
                button,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();
    }

    fn recorded(&self) -> Vec<u8> {
        self.rec.data.lock().unwrap().clone()
    }

    fn discard_output(&mut self) {
        // Setting the mouse modes doesn't write anything back, but wait for the
        // writer thread anyway so that we can't accidentally attribute startup
        // output to the events under test.
        std::thread::sleep(SETTLE);
        self.rec.data.lock().unwrap().clear();
    }

    /// Assert that the events fed in so far produced exactly `expected`, then
    /// reset the recording.
    ///
    /// The terminal writes through a background thread, so output arrives
    /// asynchronously. Poll until it matches instead of sleeping for a fixed
    /// amount of time, but then keep waiting a little longer and compare again:
    /// otherwise an unwanted report that is still in flight could slip past the
    /// comparison, which is precisely what these tests need to catch.
    fn assert_output(&mut self, expected: &str) {
        let deadline = Instant::now() + TIMEOUT;
        while self.recorded() != expected.as_bytes() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(SETTLE);
        assert_equal!(
            String::from_utf8_lossy(&self.recorded()).to_string(),
            expected.to_string()
        );
        self.rec.data.lock().unwrap().clear();
    }
}

/// Regression test for <https://github.com/wezterm/wezterm/issues/2414>.
///
/// Windows 10 and later synthesize a zero-distance `WM_MOUSEMOVE` in between
/// the button-down and button-up of the click that activates a previously
/// unfocused window. xterm specifies that "Motion events are reported only if
/// the mouse pointer has moved to a different character cell", so that event
/// must not produce a button-motion (drag) report; if it does, programs such as
/// tmux treat the click as a drag, enter copy mode and clobber the clipboard.
#[test]
fn motion_in_the_cell_that_was_just_pressed_is_not_reported() {
    let mut term = MouseTerm::new();
    term.enable_tmux_style_mouse();

    term.mouse(MouseEventKind::Press, MouseButton::Left, 24, 7);
    term.mouse(MouseEventKind::Move, MouseButton::Left, 24, 7);
    term.mouse(MouseEventKind::Release, MouseButton::Left, 24, 7);

    // Press and release only; no `\x1b[<32;25;8M` in between.
    term.assert_output("\x1b[<0;25;8M\x1b[<0;25;8m");
}

/// Motion that does reach a different cell must still be reported, otherwise
/// dragging would stop working.
#[test]
fn motion_to_a_different_cell_is_reported() {
    let mut term = MouseTerm::new();
    term.enable_tmux_style_mouse();

    term.mouse(MouseEventKind::Press, MouseButton::Left, 24, 7);
    term.mouse(MouseEventKind::Move, MouseButton::Left, 28, 7);
    term.mouse(MouseEventKind::Release, MouseButton::Left, 28, 7);

    term.assert_output("\x1b[<0;25;8M\x1b[<32;29;8M\x1b[<0;29;8m");
}

/// Because a press now re-establishes the reported position, performing the
/// identical drag gesture twice in a row reports the motion both times.
/// Previously the second gesture's motion was silently dropped, since the
/// remembered position was only ever updated by reported motion events.
#[test]
fn repeating_the_same_drag_reports_motion_every_time() {
    let mut term = MouseTerm::new();
    term.enable_tmux_style_mouse();

    for _ in 0..2 {
        term.mouse(MouseEventKind::Press, MouseButton::Left, 24, 7);
        term.mouse(MouseEventKind::Move, MouseButton::Left, 28, 7);
        term.mouse(MouseEventKind::Release, MouseButton::Left, 28, 7);
        term.assert_output("\x1b[<0;25;8M\x1b[<32;29;8M\x1b[<0;29;8m");
    }
}

/// With SGR-Pixels (1016) reporting, sub-cell motion is meaningful, so the
/// dedup has to compare pixel offsets too: a move to the exact same pixel is
/// suppressed, while a move within the same cell but to a different pixel is
/// still reported.
#[test]
fn sgr_pixels_motion_dedup_is_pixel_accurate() {
    let mut term = MouseTerm::new();
    term.enable_mouse(b"\x1b[?1000;1002;1016h");

    // cells are 8x16, so cell (24,7) + offset (3,4) reports as pixel
    // (24*8+3+1, 7*16+4+1) == (196, 117)
    term.mouse_at_pixel(MouseEventKind::Press, MouseButton::Left, 24, 7, 3, 4);
    // exact same pixel: suppressed
    term.mouse_at_pixel(MouseEventKind::Move, MouseButton::Left, 24, 7, 3, 4);
    // same cell, different pixel: reported
    term.mouse_at_pixel(MouseEventKind::Move, MouseButton::Left, 24, 7, 5, 4);
    term.mouse_at_pixel(MouseEventKind::Release, MouseButton::Left, 24, 7, 5, 4);

    term.assert_output("\x1b[<0;196;117M\x1b[<32;198;117M\x1b[<0;198;117m");
}
