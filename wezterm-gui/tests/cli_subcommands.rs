//! End-to-end tests for the subcommands that run in a terminal.
//!
//! What they assert is the exit status and what reaches stderr, neither of
//! which a unit test can observe, so these spawn the built binary.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Runs `wezterm-gui` with the given arguments.
///
/// The `WEZTERM_CONFIG_*` variables are cleared from the child, since
/// wezterm exports them to the programs it runs and a test run from inside
/// a wezterm pane would otherwise inherit them.
fn run(args: &[&str]) -> Output {
    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_wezterm-gui")))
        .args(args)
        .env_remove("WEZTERM_CONFIG_FILE")
        .env_remove("WEZTERM_CONFIG_DIR")
        .output()
        .expect("run wezterm-gui")
}

fn exit_code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("exited via a signal rather than a status")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_cli_failure_is_not_announced_to_the_desktop() {
    // `; terminating` is emitted only by `terminate_with_error_message`,
    // which is also what raises the notification, so its absence is what
    // says this took the stderr path.
    let output = run(&["--skip-config", "ls-fonts", "--codepoints", "zz"]);

    assert_eq!(exit_code(&output), 1, "stderr: {}", stderr(&output));
    assert!(
        !stderr(&output).contains("terminating"),
        "a cli failure should not take the notification path, got: {}",
        stderr(&output)
    );
    assert!(
        !stderr(&output).trim().is_empty(),
        "the failure should still be reported on stderr"
    );
}
