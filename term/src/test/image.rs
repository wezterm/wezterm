//! Tests for inline image protocol handling

use super::*;
use k9::assert_equal as assert_eq;

/// A tiny but valid 11x11 PNG, base64 encoded.
/// Taken from the reproduction in <https://github.com/wezterm/wezterm/issues/6344>.
const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAsAAAALCAYAAACprHcmAAAACXBIWXMAAAGKAAABigEzlzBYAAAAOUlEQVQYlZXOwQ0AMAzCQEdi7yaT0xWAN7JuDCac2PQKYxflycOoICOKtPIuqFCg4/LzKxiz6xjyAYh9DR1sLUN1AAAAAElFTkSuQmCC";

/// Feeding a Kitty graphics escape that requests a zero-sized placement (here `r=0,h=0`)
/// must not panic the terminal.
/// Prior to the fix for <https://github.com/wezterm/wezterm/issues/6344> this divided by zero
/// while computing the per-cell pixel deltas and took down the whole pane.
#[test]
fn kitty_zero_dimension_image_does_not_panic() {
    let mut term = TestTerm::new(3, 10, 0);

    // a=T: transmit and display, t=d: data is directly embedded,
    // f=100: PNG, r=0/h=0: zero rows / zero source height.
    let seq = format!("\x1b_Gr=0,h=0,a=T,t=d,f=100;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());

    // The image is refused, so the cursor never moved;
    // Printing normal text and observing it confirms we recovered rather than crashing.
    term.print(b"ok");
    assert_visible_contents(&term, file!(), line!(), &["ok", "", ""]);
}

/// A well-formed Kitty graphic with non-zero dimensions should continue to be accepted.
/// The test passes as long as processing the image does not panic and the terminal remains usable.
#[test]
fn kitty_valid_image_is_accepted() {
    let mut term = TestTerm::new(3, 10, 0);

    let seq = format!("\x1b_Ga=T,t=d,f=100;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());

    // Printing normal text and observing it shifted confirms the terminal is usable.
    term.print(b"ok");
    assert_visible_contents(&term, file!(), line!(), &["  ok", "", ""]);
}

/// When the pty has no pixel size, `cell_pixel_width`/`cell_pixel_height` are zero.
/// Displaying an image sized in cells (ie: without explicit `c=`/`r=`) must not divide by zero.
/// This is a distinct crash from the zero-dimension image above and is not caught by that guard.
/// See <https://github.com/wezterm/wezterm/issues/6344>.
#[test]
fn kitty_image_with_zero_pixel_dimensions_does_not_panic() {
    let mut term = Terminal::new(
        TerminalSize {
            rows: 3,
            cols: 80,
            // No pixel size!
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        Arc::new(TestTermConfig { scrollback: 0 }),
        "WezTerm",
        "O_o",
        Box::new(Vec::new()),
    );

    // No `c=`/`r=`, so the placement is computed from the (zero) cell pixel
    // size, exercising the divide that previously panicked.
    let seq = format!("\x1b_Ga=T,t=d,f=100;{}\x1b\\", TINY_PNG_BASE64);
    term.advance_bytes(seq.as_bytes());

    // The image is refused, so the cursor never moved;
    // Printing normal text and observing it confirms we recovered rather than crashing.
    term.advance_bytes(b"ok");
    assert_visible_contents(&term, file!(), line!(), &["ok", "", ""]);
}

/// A kitty virtual placement (`U=1`) must not display anything at the
/// cursor position nor move the cursor; the image only shows where the
/// application prints U+10EEEE placeholder cells.
/// <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>
#[test]
fn kitty_virtual_placement_is_not_displayed_at_cursor() {
    let mut term = TestTerm::new(3, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,U=1,i=42,c=2,r=2,t=d,f=100;{}\x1b\\",
        TINY_PNG_BASE64
    );
    term.print(seq.as_bytes());

    term.print("ok");
    assert_visible_contents(&term, file!(), line!(), &["ok", "", ""]);

    let no_images = term.screen().visible_lines()[0]
        .visible_cells()
        .all(|c| c.attrs().images().is_none());
    assert_eq!(no_images, true);
}

/// Placeholder cells reference a tile of a virtual placement through
/// their diacritics (row, column) and foreground color (image id), and
/// must resolve to that slice of the image rather than render as text.
#[test]
fn kitty_unicode_placeholder_shows_image_tiles() {
    let mut term = TestTerm::new(3, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,U=1,i=42,c=2,r=2,t=d,f=100;{}\x1b\\",
        TINY_PNG_BASE64
    );
    term.print(seq.as_bytes());

    // Image id 42 encoded in the foreground color; row 0 in the first
    // diacritic; columns 0 and 1 in the second.
    term.print("\x1b[38;2;0;0;42m");
    term.print("\u{10eeee}\u{305}\u{305}\u{10eeee}\u{305}\u{30d}");

    let lines = term.screen().visible_lines();
    let cells: Vec<_> = lines[0].visible_cells().collect();

    for (x, expected_left) in [(0usize, 0.0f32), (1usize, 0.5f32)] {
        // The placeholder text must not remain in the cell; it would
        // otherwise be subjected to font rendering
        assert_eq!(cells[x].str(), " ");
        let images = cells[x].attrs().images().expect("image tile attached");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].image_id(), Some(42));
        assert_eq!(images[0].top_left().x.into_inner(), expected_left);
        assert_eq!(images[0].top_left().y.into_inner(), 0.0);
        assert_eq!(images[0].bottom_right().x.into_inner(), expected_left + 0.5);
        assert_eq!(images[0].bottom_right().y.into_inner(), 0.5);
    }
}

/// Placeholders that omit the diacritics continue from the placeholder
/// cell to their left.
#[test]
fn kitty_unicode_placeholder_infers_omitted_diacritics() {
    let mut term = TestTerm::new(3, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,U=1,i=42,c=2,r=2,t=d,f=100;{}\x1b\\",
        TINY_PNG_BASE64
    );
    term.print(seq.as_bytes());

    // Second cell has no diacritics at all: row continues, column advances
    term.print("\x1b[38;2;0;0;42m");
    term.print("\u{10eeee}\u{30d}\u{305}\u{10eeee}");

    let lines = term.screen().visible_lines();
    let cells: Vec<_> = lines[0].visible_cells().collect();

    let first = &cells[0].attrs().images().expect("image tile attached")[0];
    assert_eq!(first.top_left().x.into_inner(), 0.0);
    assert_eq!(first.top_left().y.into_inner(), 0.5);

    let second = &cells[1].attrs().images().expect("image tile attached")[0];
    assert_eq!(second.top_left().x.into_inner(), 0.5);
    assert_eq!(second.top_left().y.into_inner(), 0.5);
}

/// A placeholder that doesn't reference any known virtual placement is
/// not ours to interpret; it must pass through as regular text.
#[test]
fn kitty_unicode_placeholder_without_placement_is_text() {
    let mut term = TestTerm::new(3, 10, 0);

    term.print("\x1b[38;2;0;0;42m");
    term.print("\u{10eeee}\u{305}\u{305}");

    let lines = term.screen().visible_lines();
    let cell = lines[0].visible_cells().next().unwrap();
    assert_eq!(cell.str(), "\u{10eeee}\u{305}\u{305}");
    assert_eq!(cell.attrs().images().is_none(), true);
}
