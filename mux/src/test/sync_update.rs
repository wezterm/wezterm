use super::*;
use std::io::Write;
use termwiz::escape::csi::{Cursor, Device, Keyboard, Mode, CSI};

#[test]
fn queries_pass_through_a_synchronized_update() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser();

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
fn kitty_state_mutations_stay_held() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser();

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
    let (mut tx, pane, parser) = start_parser();

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
    let (mut tx, pane, parser) = start_parser();

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
