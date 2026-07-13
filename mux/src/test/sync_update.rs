use super::*;
use crate::{configuration, hold_timeout_from};
use std::io::Write;
use termwiz::escape::csi::{Cursor, Device, Keyboard, Mode, CSI};

#[test]
fn a_synchronized_update_expires_after_the_configured_timeout() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(500);

    tx.write_all(BSU).unwrap();
    tx.write_all(b"hello").unwrap();

    assert!(
        wait_for(
            || contains_print(&pane.flattened_actions(), "hello"),
            Duration::from_secs(5)
        ),
        "expiring the hold should flush the buffered output"
    );

    // A hold released by the timeout must not hold up later output
    // either: the closing sequence for the expired update is a no-op
    // and new output flows through immediately.
    tx.write_all(ESU).unwrap();
    tx.write_all(b"world").unwrap();
    assert!(wait_for(
        || contains_print(&pane.flattened_actions(), "world"),
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}

#[test]
fn repeating_the_set_sequence_does_not_leak_the_held_frame() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(60_000);

    tx.write_all(BSU).unwrap();
    tx.write_all(b"part one").unwrap();
    tx.write_all(BSU).unwrap();
    tx.write_all(b"part two").unwrap();
    // A passed-through query acts as a barrier proving the parser has
    // consumed everything written above
    tx.write_all(DA1_QUERY).unwrap();

    let da1 = Action::CSI(CSI::Device(Box::new(
        Device::RequestPrimaryDeviceAttributes,
    )));
    assert!(wait_for(
        || pane.flattened_actions().contains(&da1),
        Duration::from_secs(5)
    ));
    assert!(
        !contains_print(&pane.flattened_actions(), "part one"),
        "a repeated set of mode 2026 must not flush the frame held so far"
    );

    tx.write_all(ESU).unwrap();
    assert!(wait_for(
        || {
            let actions = pane.flattened_actions();
            contains_print(&actions, "part one") && contains_print(&actions, "part two")
        },
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}

#[test]
fn a_new_update_gets_a_fresh_deadline() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(2_000);

    let da1 = Action::CSI(CSI::Device(Box::new(
        Device::RequestPrimaryDeviceAttributes,
    )));
    let da1_count = |pane: &RecordingPane, want: usize| {
        let da1 = da1.clone();
        let actions = pane.flattened_actions();
        actions.iter().filter(|action| **action == da1).count() >= want
    };

    tx.write_all(BSU).unwrap();
    tx.write_all(b"first frame").unwrap();
    // Barrier: once this query is answered the parser has consumed the
    // opening sequence above
    tx.write_all(DA1_QUERY).unwrap();
    assert!(wait_for(|| da1_count(&pane, 1), Duration::from_secs(5)));
    // A second barrier that can only arrive through a subsequent read
    // proves the parser got back to the top of its read loop with the
    // update's deadline running
    tx.write_all(DA1_QUERY).unwrap();
    assert!(wait_for(|| da1_count(&pane, 2), Duration::from_secs(5)));
    thread::sleep(Duration::from_millis(1_500));

    // Close the first update and open the next one within a single
    // write, so both transitions happen inside one parse call. The
    // new update must not inherit the 500ms left on the old deadline.
    let mut esu_bsu = ESU.to_vec();
    esu_bsu.extend_from_slice(BSU);
    esu_bsu.extend_from_slice(b"second frame");
    esu_bsu.extend_from_slice(DA1_QUERY);
    tx.write_all(&esu_bsu).unwrap();
    assert!(wait_for(
        || contains_print(&pane.flattened_actions(), "first frame") && da1_count(&pane, 3),
        Duration::from_secs(5)
    ));

    thread::sleep(Duration::from_millis(1_000));
    assert!(
        !contains_print(&pane.flattened_actions(), "second frame"),
        "the second update inherited the first update's deadline"
    );

    tx.write_all(ESU).unwrap();
    assert!(wait_for(
        || contains_print(&pane.flattened_actions(), "second frame"),
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}

#[test]
fn queries_pass_through_a_synchronized_update() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(60_000);

    tx.write_all(BSU).unwrap();
    tx.write_all(b"hello").unwrap();
    tx.write_all(DA1_QUERY).unwrap();
    tx.write_all(KITTY_QUERY).unwrap();

    let da1 = Action::CSI(CSI::Device(Box::new(
        Device::RequestPrimaryDeviceAttributes,
    )));
    let kitty = Action::CSI(CSI::Keyboard(Keyboard::QueryKittySupport));
    assert!(
        wait_for(
            || {
                let actions = pane.flattened_actions();
                actions.contains(&da1) && actions.contains(&kitty)
            },
            Duration::from_secs(5)
        ),
        "queries should be answered while the update is held, got {:?}",
        pane.flattened_actions()
    );
    assert!(
        pane.recorded_batches()
            .iter()
            .any(|batch| batch.as_slice() == [da1.clone()]),
        "a passed-through query must form its own batch, got {:?}",
        pane.recorded_batches()
    );
    assert!(
        !contains_print(&pane.flattened_actions(), "hello"),
        "held output must not leak out with the queries"
    );

    tx.write_all(ESU).unwrap();
    assert!(
        wait_for(
            || contains_print(&pane.flattened_actions(), "hello"),
            Duration::from_secs(5)
        ),
        "closing the update should flush the held output"
    );

    stop_parser(tx, parser);
}

#[test]
fn the_hold_timeout_is_clamped_against_extreme_values() {
    let _guard = TEST_LOCK.lock();
    let mut config = config::Config::default_config();
    config.mux_synchronized_output_timeout_ms = u64::MAX;
    config::use_this_configuration(config);
    assert_eq!(
        hold_timeout_from(&configuration()),
        Duration::from_millis(i32::MAX as u64)
    );
}

#[test]
fn kitty_state_mutations_stay_held() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(60_000);

    tx.write_all(BSU).unwrap();
    // A held switch to the alternate screen; the keyboard stacks are
    // per-screen, so applying the push before the switch would land
    // it on the wrong screen's stack
    tx.write_all(b"\x1b[?1049h").unwrap();
    tx.write_all(b"\x1b[>1u").unwrap();
    tx.write_all(KITTY_QUERY).unwrap();

    let query = Action::CSI(CSI::Keyboard(Keyboard::QueryKittySupport));
    let is_push = |action: &Action| {
        matches!(
            action,
            Action::CSI(CSI::Keyboard(Keyboard::PushKittyState { .. }))
        )
    };
    assert!(wait_for(
        || pane.flattened_actions().contains(&query),
        Duration::from_secs(5)
    ));
    assert!(
        !pane.flattened_actions().iter().any(is_push),
        "kitty state mutations must wait for the update to close"
    );

    tx.write_all(ESU).unwrap();
    assert!(wait_for(
        || pane.flattened_actions().iter().any(is_push),
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}

#[test]
fn decrqm_stays_held() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(60_000);

    let is_decrqm = |action: &Action| {
        matches!(
            action,
            Action::CSI(CSI::Mode(Mode::QueryDecPrivateMode(_) | Mode::QueryMode(_)))
        )
    };

    tx.write_all(BSU).unwrap();
    // Hide the cursor, then ask whether it is visible; the answer
    // depends on the held action, so it must wait for the update
    tx.write_all(b"\x1b[?25l").unwrap();
    tx.write_all(b"\x1b[?25$p").unwrap();
    tx.write_all(DA1_QUERY).unwrap();

    let da1 = Action::CSI(CSI::Device(Box::new(
        Device::RequestPrimaryDeviceAttributes,
    )));
    assert!(wait_for(
        || pane.flattened_actions().contains(&da1),
        Duration::from_secs(5)
    ));
    assert!(
        !pane.flattened_actions().iter().any(is_decrqm),
        "DECRQM depends on held mode changes, so it must wait for the update to close"
    );

    tx.write_all(ESU).unwrap();
    assert!(wait_for(
        || pane.flattened_actions().iter().any(is_decrqm),
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}

#[test]
fn cursor_position_reports_stay_held() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser(60_000);

    tx.write_all(BSU).unwrap();
    tx.write_all(b"\x1b[6n").unwrap();
    tx.write_all(DA1_QUERY).unwrap();

    let da1 = Action::CSI(CSI::Device(Box::new(
        Device::RequestPrimaryDeviceAttributes,
    )));
    assert!(wait_for(
        || pane.flattened_actions().contains(&da1),
        Duration::from_secs(5)
    ));
    let cpr = Action::CSI(CSI::Cursor(Cursor::RequestActivePositionReport));
    assert!(
        !pane.flattened_actions().contains(&cpr),
        "CPR depends on the held actions, so it must wait for the update to close"
    );

    tx.write_all(ESU).unwrap();
    assert!(wait_for(
        || pane.flattened_actions().contains(&cpr),
        Duration::from_secs(5)
    ));

    stop_parser(tx, parser);
}
