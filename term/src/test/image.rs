//! Tests for inline image protocol handling

use super::*;
use wezterm_cell::image::ImageDataType;
use wezterm_surface::change::ImageData;

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

/// A 2x2 RGBA image, base64 encoded.
const TINY_RGBA_BASE64: &str = "AQID/wQFBv8HCAn/CgsM/w==";

/// The image data attached to the first cell that carries one.
fn first_image(term: &TestTerm) -> Arc<ImageData> {
    opt_first_image(term).expect("no image was attached to the screen")
}

/// The image data attached to the first cell that carries one, or None.
fn opt_first_image(term: &TestTerm) -> Option<Arc<ImageData>> {
    for line in term.screen().visible_lines().iter() {
        for cell in line.visible_cells() {
            if let Some(im) = cell
                .attrs()
                .images()
                .and_then(|images| images.into_iter().next())
            {
                return Some(Arc::clone(im.image_data()));
            }
        }
    }
    None
}

/// An Rgba8 stores the hash of its pixels and `compute_hash` reports it without
/// recomputing, so a kitty frame transmission that edits those pixels in place
/// must leave the stored hash describing what is now there.
#[test]
fn kitty_frame_edit_keeps_the_stored_hash_current() {
    let mut term = TestTerm::new(3, 10, 0);

    // a=T: transmit and display, f=32: RGBA, s/v: 2x2 pixels.
    let seq = format!("\x1b_Ga=T,t=d,f=32,s=2,v=2,i=1;{}\x1b\\", TINY_RGBA_BASE64);
    term.advance_bytes(seq.as_bytes());
    let transmitted_hash = first_image(&term).data().compute_hash();

    // a=f: transmit a frame, r=1: edit frame 1 in place, painting the top left
    // pixel opaque red over the 0x01,0x02,0x03 it arrived with.
    term.advance_bytes(b"\x1b_Ga=f,t=d,f=32,s=1,v=1,i=1,r=1,x=0,y=0;/wAA/w==\x1b\\");

    let image = first_image(&term);
    let image = image.data();
    let edited_hash = image.compute_hash();

    match &*image {
        ImageDataType::Rgba8 { data, .. } => {
            // Asserted on the pixels rather than on the hash, so a blit that
            // did nothing cannot be mistaken for a hash that went stale.
            k9::assert_equal!(&data[0..4], &[0xff, 0x00, 0x00, 0xff][..]);
            k9::assert_equal!(edited_hash, ImageDataType::hash_bytes(data));
        }
        other => panic!("expected Rgba8, got {:?}", other),
    }

    assert_ne!(edited_hash, transmitted_hash);
}

/// Indexed image uploads produce the expected images.
#[test]
fn kitty_indexed_image() {
    let mut term = TestTerm::new(3, 10, 0);
    // a=T: transmit and display, f=8: Indexed, s/v: 2x1 pixels, .
    let seq = format!("\x1b_Ga=T,t=d,f=8,s=2,v=1,i=1;AgE=\x1b\\");
    term.advance_bytes(seq.as_bytes());
    let image = first_image(&term);
    let image = image.data();
    match &*image {
        ImageDataType::Indexed {
            data,
            width,
            height,
            has_alpha,
            hash: _,
        } => {
            k9::assert_equal!(data, &[2, 1]);
            k9::assert_equal!(*width, 2);
            k9::assert_equal!(*height, 1);
            k9::assert_equal!(*has_alpha, false);
        }
        other => panic!("expected Indexed, got {:?}", other),
    }
}

/// IndexedAlpha image uploads produce the expected images.
#[test]
fn kitty_indexed_alpha_image() {
    let mut term = TestTerm::new(3, 10, 0);
    // a=T: transmit and display, f=16: IndexedAlpha, s/v: 2x1 pixels, .
    let seq = format!("\x1b_Ga=T,t=d,f=16,s=2,v=1,i=1;Av8BQA==\x1b\\");
    term.advance_bytes(seq.as_bytes());
    let image = first_image(&term);
    let image = image.data();
    match &*image {
        ImageDataType::Indexed {
            data,
            width,
            height,
            has_alpha,
            hash: _,
        } => {
            k9::assert_equal!(data, &[2, 255, 1, 64]);
            k9::assert_equal!(*width, 2);
            k9::assert_equal!(*height, 1);
            k9::assert_equal!(*has_alpha, true);
        }
        other => panic!("expected Indexed, got {:?}", other),
    }
}

/// Indexed image uploads with the wrong size produce no image (but don't crash).
#[test]
fn kitty_indexed_image_wrong_size() {
    let mut term = TestTerm::new(3, 10, 0);
    // f=16 2x1 with only 2 bytes
    let seq = format!("\x1b_Ga=T,t=d,f=16,s=2,v=1,i=1;AgE=\x1b\\");
    term.advance_bytes(seq.as_bytes());
    k9::assert_equal!(opt_first_image(&term).is_none(), true);
    // f=8 2x1 with 4 bytes
    let seq = format!("\x1b_Ga=T,t=d,f=8,s=2,v=1,i=1;Av8BQA==\x1b\\");
    term.advance_bytes(seq.as_bytes());
    k9::assert_equal!(opt_first_image(&term).is_none(), true);
}
