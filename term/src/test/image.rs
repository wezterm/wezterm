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
        Arc::new(TestTermConfig {
            scrollback: 0,
            kitty_image_storage_budget_mb: None,
            image_dedup_cache_budget_bytes: None,
            kitty_image_disk_cache: None,
        }),
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
    for line in term.screen().visible_lines().iter() {
        for cell in line.visible_cells() {
            if let Some(im) = cell
                .attrs()
                .images()
                .and_then(|images| images.into_iter().next())
            {
                return Arc::clone(im.image_data());
            }
        }
    }
    panic!("no image was attached to the screen");
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

/// The (column, image id) of every cell on the visible screen that
/// currently carries a kitty image attachment.
fn attached_image_ids(term: &TestTerm) -> Vec<(usize, Option<u32>)> {
    let mut out = vec![];
    for line in term.screen().visible_lines().iter() {
        for cell in line.visible_cells() {
            if let Some(images) = cell.attrs().images() {
                for image in images {
                    out.push((cell.cell_index(), image.image_id()));
                }
            }
        }
    }
    out
}

/// Transmit the tiny PNG with a client-chosen image id, leaving the
/// cursor right after the 2x1 cells the 11x11 pixel image occupies.
fn place_tiny_png(term: &mut TestTerm, image_id: u32, extra_keys: &str) {
    let seq = format!(
        "\x1b_Ga=T,t=d,f=100,i={}{};{}\x1b\\",
        image_id, extra_keys, TINY_PNG_BASE64
    );
    term.print(seq.as_bytes());
}

/// d=c must remove exactly the placements that intersect the cursor cell,
/// leaving unrelated placements alone.
#[test]
fn kitty_delete_at_cursor_cell() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[2;1H"); // move to the start of the second row
    place_tiny_png(&mut term, 2, "");

    // Park the cursor on the first image (row 1, column 1) and delete
    // whatever is under it.
    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=d,d=c\x1b\\");

    let ids = attached_image_ids(&term);
    assert!(
        ids.iter().all(|(_, id)| *id == Some(2)),
        "only the second image should remain, got {:?}",
        ids
    );
}

/// d=x must remove placements intersecting a column, and d=y those
/// intersecting a (viewport) row.
#[test]
fn kitty_delete_by_column_and_row() {
    let mut term = TestTerm::new(3, 10, 0);

    // Image occupies columns 0-1 of row 1
    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b_Ga=d,d=x,x=0\x1b\\");
    assert!(attached_image_ids(&term).is_empty());

    // And row deletion picks up an image placed on row 2
    term.print(b"\x1b[2;1H");
    place_tiny_png(&mut term, 2, "");
    term.print(b"\x1b_Ga=d,d=y,y=1\x1b\\");
    assert!(attached_image_ids(&term).is_empty());
}

/// d=q removes placements at a cell only when the z-index matches too.
#[test]
fn kitty_delete_at_cell_with_z() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, ",z=5");
    // Wrong z-index: nothing is removed
    term.print(b"\x1b_Ga=d,d=q,x=0,y=0,z=6\x1b\\");
    assert!(attached_image_ids(&term).len() == 2);

    // Matching z-index at the same cell removes it
    term.print(b"\x1b_Ga=d,d=q,x=0,y=0,z=5\x1b\\");
    assert!(attached_image_ids(&term).is_empty());
}

/// d=z removes placements carrying a given z-index.
#[test]
fn kitty_delete_by_z_index() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, ",z=-1");
    term.print(b"\x1b[2;1H");
    place_tiny_png(&mut term, 2, ",z=2");

    term.print(b"\x1b_Ga=d,d=z,z=-1\x1b\\");

    let ids = attached_image_ids(&term);
    assert!(ids.iter().all(|(_, id)| *id == Some(2)));
}

/// d=r removes every image whose id falls in the x=..y= range.
#[test]
fn kitty_delete_by_id_range() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[2;1H");
    place_tiny_png(&mut term, 3, "");

    // Range 1..2 covers image 1 but not image 3
    term.print(b"\x1b_Ga=d,d=r,x=1,y=2\x1b\\");

    let ids = attached_image_ids(&term);
    assert!(ids.iter().all(|(_, id)| *id == Some(3)));
}

/// d=n with an unknown image number must answer ENOENT on the pty
/// rather than silently doing nothing.
#[test]
fn kitty_delete_unknown_number_replies_enoent() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    term.print(b"\x1b_Ga=d,d=n,I=32\x1b\\");

    wait_for_output(&sink, "\x1b_GI=32;ENOENT");
}

/// Specifying both i= and I= is an EINVAL, answered on the pty
/// (this used to be a TODO that only logged).
#[test]
fn kitty_transmit_with_both_id_and_number_replies_einval() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    let seq = format!("\x1b_Ga=T,t=d,f=100,i=1,I=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());

    wait_for_output(&sink, "\x1b_Gi=1,I=2;EINVAL");
}

/// Placing an image that was never transmitted answers ENOENT, and a
/// successful placement is acknowledged with OK, echoing the placement
/// id the client chose.
#[test]
fn kitty_place_replies_enoent_then_ok_with_placement_id() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    // Not transmitted yet
    term.print(b"\x1b_Ga=p,i=99\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=99;ENOENT");

    // Transmit quietly, then place with an explicit placement id
    let seq = format!("\x1b_Ga=t,t=d,f=100,i=7,q=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());
    term.print(b"\x1b_Ga=p,i=7,p=5\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=7,p=5;OK");
}

/// An image that would need more than the per-image RAM cap is refused
/// with ENOSPC rather than a free-form error.
#[test]
fn kitty_transmit_oversized_image_replies_enospc() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    // 6000x5000 pixels of RGBA is 120MB, over the 100MB cap.
    // The size check fires before the payload length is validated,
    // so no actual pixel data needs to be sent.
    term.print(b"\x1b_Ga=t,t=d,f=32,s=6000,v=5000,i=3;AAAA\x1b\\");

    wait_for_output(&sink, "\x1b_Gi=3;ENOSPC");
}

/// A query (a=q) against a garbage direct payload answers EINVAL.
#[test]
fn kitty_query_bad_payload_replies_einval() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    term.print(b"\x1b_Ga=q,t=d,f=24,s=1,v=1,i=1;!!!not-base64!!!\x1b\\");

    wait_for_output(&sink, "\x1b_Gi=1;EINVAL");
}

/// A 2x4 pixel RGBA image (1:2 aspect ratio). Cells in the mock
/// terminal are 8x16 pixels.
const TINY_2X4_RGBA_BASE64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// With only c= specified, the number of rows is derived from the
/// aspect ratio instead of squashing the image vertically.
/// 2 cells wide = 16px, so a 1:2 image becomes 32px tall = 2 rows.
#[test]
fn kitty_only_columns_derives_rows_from_aspect() {
    let mut term = TestTerm::new(4, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,t=d,f=32,s=2,v=4,i=1,c=2;{}\x1b\\",
        TINY_2X4_RGBA_BASE64
    );
    term.print(seq.as_bytes());

    // The derived placement is 2x2 cells and the cursor ends up right
    // after the bottom right cell
    assert!(attached_image_ids(&term).len() == 4);
    term.assert_cursor_pos(2, 1, None, None);
}

/// With only r= specified, the number of columns is derived from the
/// aspect ratio: 2 rows = 32px tall, so a 1:2 image becomes 16px = 2
/// columns wide.
#[test]
fn kitty_only_rows_derives_columns_from_aspect() {
    let mut term = TestTerm::new(4, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,t=d,f=32,s=2,v=4,i=1,r=2;{}\x1b\\",
        TINY_2X4_RGBA_BASE64
    );
    term.print(seq.as_bytes());

    assert!(attached_image_ids(&term).len() == 4);
    term.assert_cursor_pos(2, 1, None, None);
}

/// With both c= and r= specified the image is letterboxed instead of
/// distorted: a 1:2 image inside a 4x2 cell rectangle (32x32px) is
/// scaled to 16x32px and centered, so only the middle two columns of
/// each row carry image data while the cursor still advances past the
/// whole rectangle.
#[test]
fn kitty_both_columns_and_rows_letterboxes() {
    let mut term = TestTerm::new(4, 10, 0);

    let seq = format!(
        "\x1b_Ga=T,t=d,f=32,s=2,v=4,i=1,c=4,r=2;{}\x1b\\",
        TINY_2X4_RGBA_BASE64
    );
    term.print(seq.as_bytes());

    // The letterbox bands (columns 0 and 3 of each row) have no image
    assert!(attached_image_ids(&term).len() == 4);
    let cols: Vec<usize> = attached_image_ids(&term)
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert!(
        cols.iter().all(|c| *c == 1 || *c == 2),
        "expected image only in the middle columns, got {:?}",
        cols
    );

    // But the cursor still moves past the full 4 column rectangle.
    // The image-bearing cells render as spaces in the text view.
    term.print(b"ok");
    assert_visible_contents(&term, file!(), line!(), &["   ", "    ok", "", ""]);
}

/// When two images share a z-index, the one with the lower image id is
/// stacked below, as the spec requires.
#[test]
fn kitty_same_z_images_stack_by_image_id() {
    let mut term = TestTerm::new(3, 10, 0);

    // Attach image 2 first, then image 1 over the same cells with the
    // same z-index; the attachment list must still order 1 below 2.
    place_tiny_png(&mut term, 2, ",z=3");
    term.print(b"\x1b[1;1H");
    place_tiny_png(&mut term, 1, ",z=3");

    let ids: Vec<Option<u32>> = term.screen().visible_lines()[0]
        .visible_cells()
        .next()
        .and_then(|cell| cell.attrs().images())
        .map(|images| images.into_iter().map(|img| img.image_id()).collect())
        .unwrap_or_default();
    assert!(
        ids == vec![Some(1), Some(2)],
        "expected lower image id stacked first, got {:?}",
        ids
    );
}

/// A 2x2 pixel RGBA image used as animation frames.
const TINY_2X2_RGBA_BASE64: &str = "AQIDBAECAwQBAgMEAQIDBA==";

/// Transmit image id 1 and append a second frame, making it animated.
fn make_two_frame_animation(term: &mut TestTerm) {
    term.print(
        format!(
            "\x1b_Ga=T,t=d,f=32,s=2,v=2,i=1;{}\x1b\\",
            TINY_2X2_RGBA_BASE64
        )
        .as_bytes(),
    );
    term.print(
        format!(
            "\x1b_Ga=f,t=d,f=32,s=2,v=2,i=1;{}\x1b\\",
            TINY_2X2_RGBA_BASE64
        )
        .as_bytes(),
    );
}

/// The a=a animation control command manipulates the playback state:
/// stopping, client-driven frame selection, loop counts and per-frame
/// gaps.
#[test]
fn kitty_animation_control_updates_playback_state() {
    use std::time::Duration;
    let mut term = TestTerm::new(3, 10, 0);
    make_two_frame_animation(&mut term);

    // Animations auto-play by default
    {
        let image = first_image(&term);
        let data = image.data();
        match &*data {
            ImageDataType::AnimRgba8 {
                frames, control, ..
            } => {
                assert!(frames.len() == 2);
                assert!(control.running);
            }
            _ => panic!("expected AnimRgba8"),
        }
    }

    // Stop the animation, select frame 1 and request 2 loops
    term.print(b"\x1b_Ga=a,i=1,s=1,c=1,v=2\x1b\\");
    {
        let image = first_image(&term);
        let data = image.data();
        match &*data {
            ImageDataType::AnimRgba8 { control, .. } => {
                assert!(!control.running);
                assert!(control.paused_frame == Some(1));
                assert!(control.loop_count == Some(2));
            }
            _ => panic!("expected AnimRgba8"),
        }
    }

    // Resume in loading mode and set the gap of frame 2 to 100ms
    term.print(b"\x1b_Ga=a,i=1,s=2,r=2,z=100\x1b\\");
    {
        let image = first_image(&term);
        let data = image.data();
        match &*data {
            ImageDataType::AnimRgba8 {
                durations, control, ..
            } => {
                assert!(control.running);
                assert!(control.loading);
                assert!(durations[1] == Duration::from_millis(100));
            }
            _ => panic!("expected AnimRgba8"),
        }
    }
}

/// d=f deletes the frame selected by c= from an animation.
#[test]
fn kitty_delete_frame_removes_animation_frame() {
    let mut term = TestTerm::new(3, 10, 0);
    make_two_frame_animation(&mut term);

    term.print(b"\x1b_Ga=d,d=f,i=1,c=2\x1b\\");

    let image = first_image(&term);
    let data = image.data();
    match &*data {
        ImageDataType::AnimRgba8 { frames, .. } => {
            assert!(frames.len() == 1, "expected one frame left");
        }
        _ => panic!("expected AnimRgba8"),
    }
}

/// Selecting a frame outside the animation answers EINVAL.
#[test]
fn kitty_animate_bad_frame_replies_einval() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);
    make_two_frame_animation(&mut term);

    term.print(b"\x1b_Ga=a,i=1,c=9\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=1;EINVAL");

    // And an unknown image answers ENOENT
    term.print(b"\x1b_Ga=a,i=42,s=1\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=42;ENOENT");
}

/// The (top_left, bottom_right, image_id) of every attached kitty
/// image cell on the visible screen, in scan order.
fn attached_slices(term: &TestTerm) -> Vec<((f32, f32), (f32, f32), Option<u32>)> {
    let mut out = vec![];
    for line in term.screen().visible_lines().iter() {
        for cell in line.visible_cells() {
            if let Some(images) = cell.attrs().images() {
                for image in images {
                    let tl = image.top_left();
                    let br = image.bottom_right();
                    out.push((
                        (tl.x.into_inner(), tl.y.into_inner()),
                        (br.x.into_inner(), br.y.into_inner()),
                        image.image_id(),
                    ));
                }
            }
        }
    }
    out
}

/// The full unicode placeholder round trip: create a virtual
/// placement, print U+10EEEE cells whose foreground color carries the
/// image id and whose diacritics select the row/column, and observe
/// one image slice per placeholder cell with the expected texture
/// coordinates. The last cell omits its column diacritic and inherits
/// col = left + 1 from the run.
#[test]
fn kitty_unicode_placeholder_renders_slices() {
    let mut term = TestTerm::new(4, 10, 0);

    // Transmit a 2x2 image and reserve a 2x2 cell virtual placement
    term.print(
        format!(
            "\x1b_Ga=t,t=d,f=32,s=2,v=2,i=1,q=2;{}\x1b\\",
            TINY_2X2_RGBA_BASE64
        )
        .as_bytes(),
    );
    term.print(b"\x1b_Ga=p,U=1,i=1,c=2,r=2,q=2\x1b\\");

    // The image id (1) travels in the truecolor foreground
    term.print(b"\x1b[38;2;0;0;1m");
    // Row 1: cells (1,1) and (1,2). U+030D has value 1, U+030E has 2.
    term.print("\u{10EEEE}\u{030D}\u{030D}".as_bytes());
    term.print("\u{10EEEE}\u{030D}\u{030E}".as_bytes());
    term.print(b"\r\n");
    // Row 2: cells (2,1) and (2,2 inherited from the left)
    term.print("\u{10EEEE}\u{030E}\u{030D}".as_bytes());
    term.print("\u{10EEEE}\u{030E}".as_bytes());

    let slices = attached_slices(&term);
    assert!(slices.len() == 4, "expected 4 slices, got {:?}", slices);
    assert!(slices.iter().all(|(_, _, id)| *id == Some(1)));
    // First cell covers the top left quadrant of the image
    assert!(slices[0].0 == (0.0, 0.0) && slices[0].1 == (0.5, 0.5));
    // The inherited-column cell covers the bottom right quadrant
    let last = &slices[3];
    assert!(last.0 == (0.5, 0.5) && last.1 == (1.0, 1.0));
}

/// Virtual placements survive d=a (the spec only allows deleting them
/// via d=i/I/r/R/n/N) but disappear with d=i.
#[test]
fn kitty_virtual_placement_delete_semantics() {
    let mut term = TestTerm::new(4, 10, 0);

    let setup = format!(
        "\x1b_Ga=t,t=d,f=32,s=2,v=2,i=1,q=2;{}",
        TINY_2X2_RGBA_BASE64
    );
    term.print(setup.as_bytes());
    term.print(b"\x1b_Ga=p,U=1,i=1,c=2,r=2,q=2\x1b\\");

    let print_placeholder_row = |term: &mut TestTerm| {
        term.print(b"\x1b[38;2;0;0;1m");
        term.print("\u{10EEEE}\u{030D}\u{030D}".as_bytes());
        term.print("\u{10EEEE}\u{030D}\u{030E}".as_bytes());
    };

    // d=a keeps the virtual placement around
    term.print(b"\x1b[1;1H\x1b[2K");
    term.print(b"\x1b_Ga=d,d=a\x1b\\");
    print_placeholder_row(&mut term);
    assert!(attached_slices(&term).len() == 2);

    // d=i removes it, and the same placeholder text attaches nothing
    term.print(b"\x1b[1;1H\x1b[2K");
    term.print(b"\x1b_Ga=d,d=i,i=1\x1b\\");
    print_placeholder_row(&mut term);
    assert!(attached_slices(&term).is_empty());
}

/// A virtual placement combined with relative placement keys answers
/// EINVAL, as the spec requires.
#[test]
fn kitty_virtual_placement_cannot_be_relative() {
    let (mut term, sink) = TestTerm::new_with_output_sink(3, 10, 0);

    let setup = format!(
        "\x1b_Ga=t,t=d,f=32,s=2,v=2,i=1,q=2;{}",
        TINY_2X2_RGBA_BASE64
    );
    term.print(setup.as_bytes());
    term.print(b"\x1b_Ga=p,U=1,i=1,c=1,r=1,P=2\x1b\\");

    wait_for_output(&sink, "\x1b_Gi=1;EINVAL");
}

/// The (column, row, image id) of every attached kitty image cell on
/// the visible screen, in scan order.
fn attached_positions(term: &TestTerm) -> Vec<(usize, usize, Option<u32>)> {
    let mut out = vec![];
    for (row, line) in term.screen().visible_lines().iter().enumerate() {
        for cell in line.visible_cells() {
            if let Some(images) = cell.attrs().images() {
                for image in images {
                    out.push((cell.cell_index(), row, image.image_id()));
                }
            }
        }
    }
    out
}

/// Relative placements position their image at parent + (H, V), follow
/// the parent when it is re-placed, and are deleted together with it.
#[test]
fn kitty_relative_placement_lifecycle() {
    let mut term = TestTerm::new(4, 10, 0);

    // Parent (image 1) at row 0, columns 0-1
    place_tiny_png(&mut term, 1, "");

    // Child (image 2) placed relative to the parent, two cells right
    let seq = format!("\x1b_Ga=t,t=d,f=100,i=2,q=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());
    term.print(b"\x1b_Ga=p,i=2,P=1,H=2,V=0\x1b\\");

    let positions = attached_positions(&term);
    assert!(
        positions
            .iter()
            .any(|(c, r, id)| *id == Some(2) && *r == 0 && *c == 2),
        "child should sit at row 0 column 2, got {:?}",
        positions
    );

    // Move the parent down a row; the child must follow
    term.print(b"\x1b[2;1H");
    term.print(b"\x1b_Ga=p,i=1\x1b\\");
    let positions = attached_positions(&term);
    assert!(
        positions.iter().all(|(_, r, _)| *r == 1),
        "everything should have moved to row 1, got {:?}",
        positions
    );
    assert!(positions.iter().any(|(c, _, id)| *id == Some(2) && *c == 2));

    // Deleting the parent takes the child with it
    term.print(b"\x1b_Ga=d,d=i,i=1\x1b\\");
    assert!(attached_positions(&term).is_empty());
}

/// Relative placement parents answer the protocol error codes: a
/// missing parent is ENOPARENT and self-parenting is EINVAL.
#[test]
fn kitty_relative_placement_error_codes() {
    let (mut term, sink) = TestTerm::new_with_output_sink(4, 10, 0);

    let seq = format!("\x1b_Ga=t,t=d,f=100,i=2,q=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());

    term.print(b"\x1b_Ga=p,i=2,P=99\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=2;ENOPARENT");

    term.print(b"\x1b_Ga=p,i=2,P=2\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=2;EINVAL");
}

/// A parent chain that circles back on itself answers ECYCLE.
#[test]
fn kitty_relative_placement_cycle_replies_ecycle() {
    let mut term = TestTerm::new(4, 10, 0);

    // Image 1 at the cursor, image 2 relative to it
    place_tiny_png(&mut term, 1, "");
    let seq = format!("\x1b_Ga=t,t=d,f=100,i=2,q=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());
    term.print(b"\x1b_Ga=p,i=2,P=1,H=2\x1b\\");

    // Re-placing image 1 relative to image 2 closes the loop; the
    // command must be refused, leaving image 1 where it was
    term.print(b"\x1b[3;1H");
    term.print(b"\x1b_Ga=p,i=1,P=2,V=2\x1b\\");
    let positions = attached_positions(&term);
    assert!(
        positions.iter().any(|(_, r, id)| *id == Some(1) && *r == 0),
        "image 1 must not have moved to row 2, got {:?}",
        positions
    );
}

/// ECYCLE is answered on the pty as well.
#[test]
fn kitty_relative_placement_cycle_replies_ecycle_response() {
    let (mut term, sink) = TestTerm::new_with_output_sink(4, 10, 0);

    place_tiny_png(&mut term, 1, "");
    let seq = format!("\x1b_Ga=t,t=d,f=100,i=2,q=2;{}\x1b\\", TINY_PNG_BASE64);
    term.print(seq.as_bytes());
    term.print(b"\x1b_Ga=p,i=2,P=1,H=2\x1b\\");

    term.print(b"\x1b[3;1H");
    term.print(b"\x1b_Ga=p,i=1,P=2,V=2\x1b\\");
    wait_for_output(&sink, "\x1b_Gi=1;ECYCLE");
}

/// Transmit an 11x11 RGBA image with `fill` in its RGB channels, so
/// that each call yields distinct content (and a distinct dedup hash)
/// while keeping every payload the same 484-byte size.
fn transmit_rgba_11x11(term: &mut TestTerm, image_id: u32, fill: u8) {
    let mut pixels = vec![fill; 11 * 11 * 4];
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&pixels);
    let seq = format!("\x1b_Ga=t,t=d,f=32,s=11,v=11,i={image_id};{b64}\x1b\\");
    term.print(seq.as_bytes());
}

/// Once the kitty graphics storage budget is exceeded, the oldest image
/// that no placement references must be released.  A zero budget makes
/// every transmission exceed it, without needing large test images.
#[test]
fn kitty_storage_budget_releases_oldest_unreferenced() {
    let mut term = TestTerm::new_with_config(3, 10, 0, Some(0), None);

    // a=t only stores the images; nothing is displayed, so both are
    // prune candidates.
    transmit_rgba_11x11(&mut term, 1, 0x10);
    transmit_rgba_11x11(&mut term, 2, 0x20);

    // Image 1 is the older unreferenced image: the budget prune that
    // follows storing image 2 releases it, while image 2 itself is
    // spared because it is the image being stored.
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![2]);
}

/// Images whose placements are still on screen must survive the budget
/// prune: the spec allows freeing over-quota images, but a client must
/// never lose something it can currently see.
#[test]
fn kitty_displayed_images_survive_budget_prune() {
    let mut term = TestTerm::new_with_config(4, 10, 0, Some(0), None);

    // a=T transmits and displays; the on-screen placement protects the
    // image from the zero budget prune triggered by each transmission.
    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[2;1H");
    place_tiny_png(&mut term, 2, "");

    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2]);
    assert!(!attached_image_ids(&term).is_empty());
}

/// d=a removes every placement; the delete path also enforces the
/// storage budget so that a client exiting with a "delete all"
/// (eg: pdfterm) releases its image data right away.
#[test]
fn kitty_delete_all_releases_orphaned_storage() {
    let mut term = TestTerm::new_with_config(4, 10, 0, Some(0), None);

    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[2;1H");
    place_tiny_png(&mut term, 2, "");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2]);

    term.print(b"\x1b_Ga=d,d=a,q=2\x1b\\");

    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    assert!(attached_image_ids(&term).is_empty());
}

/// Re-transmitting image id 0 replaces the stored payload; the
/// accounting for the replaced payload must be dropped or used_memory
/// would grow without bound for clients that stream via id 0.
#[test]
fn kitty_id_zero_retransmit_does_not_double_count() {
    let mut term = TestTerm::new_with_config(3, 10, 0, None, None);

    transmit_rgba_11x11(&mut term, 0, 0x10);
    k9::assert_equal!(term.kitty_used_memory(), 11 * 11 * 4);

    transmit_rgba_11x11(&mut term, 0, 0x20);
    k9::assert_equal!(term.kitty_used_memory(), 11 * 11 * 4);
}

/// RIS (full reset) resets the device to its initial state, which
/// includes any images retained on behalf of the kitty graphics
/// protocol; previously only the DECSTR soft reset cleared them.
#[test]
fn kitty_ris_clears_image_storage() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, "");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1]);

    term.print(b"\x1bc");

    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    assert!(attached_image_ids(&term).is_empty());
}

/// Identical payloads transmitted under different ids must share one
/// decoded payload via the content-addressed dedup cache.
#[test]
fn image_dedup_shares_identical_retransmissions() {
    let mut term = TestTerm::new(3, 10, 0);

    place_tiny_png(&mut term, 1, "");
    let first = first_image(&term);

    term.print(b"\x1b[2J\x1b[H");
    place_tiny_png(&mut term, 2, "");
    let second = first_image(&term);

    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "identical payload must reuse the same decoded image"
    );
}

/// The dedup cache must respect its byte budget, but never shrink
/// below a small floor of recent entries so that clients working with
/// a handful of images (eg: pdfterm's 4 rotating ids) keep their
/// dedup hits.
#[test]
fn image_dedup_budget_evicts_oldest_but_keeps_floor() {
    // A 1-byte budget: every 484-byte image exceeds it
    let mut term = TestTerm::new_with_config(3, 10, 0, None, Some(1));

    for id in 1..=5u32 {
        transmit_rgba_11x11(&mut term, id, 0x10 + id as u8);
    }
    // 5 distinct images were cached; eviction stopped at the floor
    k9::assert_equal!(term.image_cache_len(), 4);

    // Continued growth keeps hitting the budget and stays at the floor
    for id in 6..=10u32 {
        transmit_rgba_11x11(&mut term, id, 0x10 + id as u8);
    }
    k9::assert_equal!(term.image_cache_len(), 4);
}

/// The on-disk image cache is a process-global singleton; hold this
/// lock for the duration of tests that enable it, clearing the cache
/// on both entry and release so each test starts and ends empty.
static DISK_CACHE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Holds the test lock alive for the duration of a test; releasing it
/// also clears the cache.
struct DiskCacheSerial(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for DiskCacheSerial {
    fn drop(&mut self) {
        wezterm_blob_leases::image_disk_cache::clear();
    }
}

fn lock_disk_cache() -> DiskCacheSerial {
    let guard = DISK_CACHE_TEST_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    wezterm_blob_leases::image_disk_cache::clear();
    DiskCacheSerial(guard)
}

/// With the disk cache enabled, images released by the storage budget
/// are spilled to it, and placing the released id again restores the
/// pixels instead of observing ENOENT.
#[test]
fn kitty_budget_release_spills_and_place_restores() {
    let _serial = lock_disk_cache();
    let mut term = TestTerm::new_with_disk_cache(3, 10, 0, Some(0), None, Some(Default::default()));

    // a=t only stores; the zero-budget prune that follows storing
    // image 2 releases image 1, spilling it to the disk cache.
    transmit_rgba_11x11(&mut term, 1, 0x10);
    transmit_rgba_11x11(&mut term, 2, 0x20);
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![2]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1]);

    // Placing the released id restores it from disk and re-displays it
    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=p,i=1\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), Vec::<u32>::new());
    assert!(
        attached_image_ids(&term)
            .iter()
            .any(|(_, id)| *id == Some(1)),
        "the restored image must be displayed"
    );
}

/// A restored image is protected by its placement: the prune triggered
/// by a later transmission must release other, unreferenced images —
/// never the just-restored one.  This pins the no-restore/release-cycle
/// property of the restore path (which itself never prunes).
#[test]
fn kitty_restored_image_survives_next_prune() {
    let _serial = lock_disk_cache();
    let mut term = TestTerm::new_with_disk_cache(3, 10, 0, Some(0), None, Some(Default::default()));

    transmit_rgba_11x11(&mut term, 1, 0x10);
    transmit_rgba_11x11(&mut term, 2, 0x20);

    // Restore image 1 by placing it; its placement now references it
    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=p,i=1\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2]);

    // Storing image 3 triggers a prune over the zero budget: the
    // unreferenced image 2 is released (and spilled), image 1 survives
    transmit_rgba_11x11(&mut term, 3, 0x30);
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 3]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![2]);
}

/// Explicit deletes must not be undone by the disk cache: freeing an
/// id's data also forgets its spilled copy, and a later placement of
/// the id observes ENOENT rather than restoring stale pixels.
#[test]
fn kitty_explicit_delete_forgets_spilled_copy() {
    let _serial = lock_disk_cache();
    let mut term = TestTerm::new_with_disk_cache(3, 10, 0, Some(0), None, Some(Default::default()));

    transmit_rgba_11x11(&mut term, 1, 0x10);
    transmit_rgba_11x11(&mut term, 2, 0x20);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1]);

    // d=I with delete: image 1's data and spilled record both go away.
    // The prune that follows the delete then releases (and spills)
    // image 2, which is unreferenced over the zero budget.
    term.print(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![2]);

    // Placing the explicitly deleted id cannot restore anything
    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=p,i=1,q=2\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    assert!(
        !attached_image_ids(&term)
            .iter()
            .any(|(_, id)| *id == Some(1)),
        "the explicitly deleted image must not be displayed"
    );
}

/// When the disk cache's own budget evicts a spilled payload, placing
/// its id observes ENOENT again, and the stale id→hash record is
/// dropped so that later placements fail fast; ids whose payloads
/// are still on disk keep restoring.
#[test]
fn kitty_disk_quota_eviction_makes_restore_fail() {
    let _serial = lock_disk_cache();
    let mut term = TestTerm::new_with_disk_cache(
        3,
        10,
        0,
        Some(0), // zero RAM storage budget: every transmit prunes
        Some(1), // tiny dedup budget: eviction stops at the floor of 4
        Some(TestDiskCacheConfig {
            budget_bytes: Some(1), // tiny disk budget: one payload at a time
        }),
    );

    // Six distinct images: the RAM budget releases (and spills) each
    // predecessor, the disk budget keeps only the newest payload, and
    // the dedup cache keeps only the last four payloads.
    for id in 1..=6u32 {
        transmit_rgba_11x11(&mut term, id, 0x10 + id as u8);
    }
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![6]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1, 2, 3, 4, 5]);

    // Image 2 is recoverable from nowhere: the dedup cache evicted it
    // (only the last four payloads remain) and the disk budget evicted
    // its file, so placing it observes ENOENT and drops the stale record
    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=p,i=2,q=2\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![6]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1, 3, 4, 5]);
    assert!(
        !attached_image_ids(&term)
            .iter()
            .any(|(_, id)| *id == Some(2)),
        "the unrecoverable image must not be displayed"
    );

    // Image 3 is still held by the dedup cache and restores from
    // memory without touching the disk
    term.print(b"\x1b[2;1H");
    term.print(b"\x1b_Ga=p,i=3\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![3, 6]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1, 4, 5]);
}

/// Transmit (a=t, store only) a w×h RGBA image under a client-chosen
/// id, sized so that a few images exceed the 1 MiB granularity of the
/// kitty storage budget.
fn transmit_rgba_wh(term: &mut TestTerm, image_id: u32, w: u32, h: u32, fill: u8) {
    transmit_rgba_wh_action(term, "a=t", image_id, w, h, fill)
}

/// Transmit-and-display (a=T) the same, so the image is referenced by
/// a placement and cannot be dropped by the budget prune.
fn place_rgba_wh(term: &mut TestTerm, image_id: u32, fill: u8) {
    transmit_rgba_wh_action(term, "a=T", image_id, 400, 250, fill)
}

fn transmit_rgba_wh_action(
    term: &mut TestTerm,
    action: &str,
    image_id: u32,
    w: u32,
    h: u32,
    fill: u8,
) {
    let mut pixels = vec![fill; (w * h * 4) as usize];
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&pixels);
    let seq = format!("\x1b_G{action},t=d,f=32,s={w},v={h},i={image_id};{b64}\x1b\\");
    term.print(seq.as_bytes());
}

/// When the storage budget is exceeded but every stored image is
/// referenced by a placement, their payloads are swapped out to the
/// disk cache instead of the images being dropped: the ids stay
/// stored, and reading the payload brings the pixels back.
#[test]
fn kitty_over_budget_swaps_referenced_images() {
    let _serial = lock_disk_cache();
    // A 1 MiB budget (the option's finest granularity) vs 400 KiB images
    let mut term =
        TestTerm::new_with_disk_cache(60, 80, 0, Some(1), None, Some(Default::default()));

    // a=T transmits and displays: both images are referenced, and two
    // of them fit the budget, so nothing happens yet
    place_rgba_wh(&mut term, 1, 0x11);
    term.print(b"\x1b[17;1H");
    place_rgba_wh(&mut term, 2, 0x22);
    k9::assert_equal!(term.kitty_disk_backed_ids(), Vec::<u32>::new());

    // Storing a third image exceeds the budget; nothing can be
    // released (all referenced), so the oldest payload is swapped out
    transmit_rgba_wh(&mut term, 3, 400, 250, 0x33);
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2, 3]);
    k9::assert_equal!(term.kitty_disk_backed_ids(), vec![1]);
    k9::assert_equal!(term.kitty_spilled_image_ids(), Vec::<u32>::new());
    k9::assert_equal!(term.kitty_used_memory(), 2 * 400 * 250 * 4);

    // The swapped image stays usable: accessing the payload reads the
    // pixels back
    let img = term.kitty_image_data(1).unwrap();
    assert!(img.is_disk_backed());
    drop(img.data());
    assert!(!img.is_disk_backed());
    k9::assert_equal!(img.len(), 400 * 250 * 4);
}

/// A swapped payload that was read back counts as freshly displayed:
/// the next budget enforcement swaps other, less recently touched
/// images first, so viewing an image does not ping-pong it straight
/// back out to disk.
#[test]
fn kitty_recently_read_back_image_is_swapped_last() {
    let _serial = lock_disk_cache();
    let mut term =
        TestTerm::new_with_disk_cache(60, 80, 0, Some(1), None, Some(Default::default()));

    place_rgba_wh(&mut term, 1, 0x11);
    term.print(b"\x1b[17;1H");
    place_rgba_wh(&mut term, 2, 0x22);
    term.print(b"\x1b[33;1H");
    place_rgba_wh(&mut term, 3, 0x33);
    // Over the 1 MiB budget with everything referenced: the oldest
    // image (1) was swapped, not dropped
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2, 3]);
    k9::assert_equal!(term.kitty_disk_backed_ids(), vec![1]);

    // Reading image 1's payload back is the "displayed again" signal
    let img = term.kitty_image_data(1).unwrap();
    drop(img.data());
    drop(img);

    // Over the budget again with a fourth image: the middle images are
    // swapped, while the just-viewed image 1 stays in memory despite
    // having the oldest generation
    term.print(b"\x1b[49;1H");
    place_rgba_wh(&mut term, 4, 0x44);
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1, 2, 3, 4]);
    k9::assert_equal!(term.kitty_disk_backed_ids(), vec![2, 3]);
    assert!(
        !term.kitty_image_data(1).unwrap().is_disk_backed(),
        "the recently read-back image must not be swapped again"
    );
}

/// Leaving the alternate screen ends the client application's session:
/// image data with no placement left (the client deleted its placements
/// on the way out) is released, together with the dedup cache, instead
/// of being retained until the storage budget is exceeded.
#[test]
fn kitty_alt_screen_exit_releases_orphaned_images() {
    let mut term = TestTerm::new(3, 10, 0);

    term.print(b"\x1b[?1049h");
    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b_Ga=d,d=a,q=2\x1b\\");
    // The lowercase delete-all keeps the data around, as before
    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1]);
    assert!(term.image_cache_len() >= 1);

    term.print(b"\x1b[?1049l");
    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    k9::assert_equal!(term.image_cache_len(), 0);
}

/// A client killed without a delete-all leaves its placements on the
/// abandoned alternate screen; leaving the screen still releases the
/// data, because those rows are no longer part of the active screen.
#[test]
fn kitty_alt_screen_exit_releases_images_left_by_killed_client() {
    let mut term = TestTerm::new(3, 10, 0);

    term.print(b"\x1b[?1049h");
    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[?1049l");

    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    k9::assert_equal!(term.image_cache_len(), 0);
}

/// Images drawn by clients that never use the alternate screen (ie:
/// living in the primary screen or scrollback) survive an unrelated
/// alt-screen session coming and going.
#[test]
fn kitty_alt_screen_exit_keeps_scrollback_images() {
    let mut term = TestTerm::new(3, 10, 10);

    place_tiny_png(&mut term, 1, "");
    term.print(b"\x1b[?1049h");
    term.print(b"\x1b[?1049l");

    k9::assert_equal!(term.kitty_stored_image_ids(), vec![1]);
    assert!(
        attached_image_ids(&term)
            .iter()
            .any(|(_, id)| *id == Some(1)),
        "the image must still be displayed"
    );
}

/// With the disk cache enabled, the alt-screen sweep also removes the
/// on-disk copies (both those of stored images and those a budget
/// prune spilled earlier): re-placing a released id observes ENOENT
/// rather than restoring the payload.
#[test]
fn kitty_alt_screen_exit_removes_disk_cache_copies() {
    let _serial = lock_disk_cache();
    let mut term = TestTerm::new_with_disk_cache(3, 10, 0, Some(0), None, Some(Default::default()));

    // Zero storage budget: storing image 2 releases and spills image 1
    transmit_rgba_11x11(&mut term, 1, 0x10);
    transmit_rgba_11x11(&mut term, 2, 0x20);
    k9::assert_equal!(term.kitty_spilled_image_ids(), vec![1]);

    term.print(b"\x1b[?1049h");
    term.print(b"\x1b_Ga=d,d=a,q=2\x1b\\");
    term.print(b"\x1b[?1049l");

    // Everything was released and nothing is recoverable from disk
    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    k9::assert_equal!(term.kitty_spilled_image_ids(), Vec::<u32>::new());

    term.print(b"\x1b[1;1H");
    term.print(b"\x1b_Ga=p,i=1\x1b\\");
    k9::assert_equal!(term.kitty_stored_image_ids(), Vec::<u32>::new());
    assert!(attached_image_ids(&term).is_empty());
}
