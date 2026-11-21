use super::*;
use std::io::Write;

#[test]
fn output_arriving_within_the_coalesce_delay_forms_one_batch() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser_with(|config| {
        config.mux_output_parser_coalesce_delay_ms = 200;
    });

    let started = Instant::now();
    tx.write_all(b"first half ").unwrap();
    thread::sleep(Duration::from_millis(20));
    tx.write_all(b"second half").unwrap();

    assert!(wait_for(
        || contains_print(&pane.flattened_actions(), "second half"),
        Duration::from_secs(5)
    ));
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(
        pane.recorded_batches().iter().any(|batch| {
            contains_print(batch, "first half") && contains_print(batch, "second half")
        }),
        "output within the coalesce delay should be applied together, got {:?}",
        pane.recorded_batches()
    );

    stop_parser(tx, parser);
}

#[test]
fn a_full_buffer_is_sent_without_waiting_out_the_coalesce_delay() {
    let _guard = TEST_LOCK.lock();
    let (mut tx, pane, parser) = start_parser_with(|config| {
        config.mux_output_parser_buffer_size = 64;
        config.mux_output_parser_coalesce_delay_ms = 60_000;
    });

    tx.write_all(&[b'x'; 256]).unwrap();

    assert!(
        wait_for(
            || contains_print(&pane.flattened_actions(), "x"),
            Duration::from_secs(5)
        ),
        "a full buffer must be sent even though the coalesce delay has not passed"
    );

    stop_parser(tx, parser);
}
