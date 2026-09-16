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

/// Runs a subcommand against a configuration file that raises an error.
fn run_with_a_broken_config(args: &[&str]) -> (tempfile::TempDir, Output) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = dir.path().join("wezterm.lua");
    std::fs::write(&config, "error('the config is broken')\nreturn {}\n")
        .expect("write config file");

    let mut full = vec!["--config-file", config.to_str().expect("utf-8 temp path")];
    full.extend_from_slice(args);
    let output = run(&full);
    (dir, output)
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn ls_fonts_fails_when_the_configuration_does_not_load() {
    let (_dir, output) = run_with_a_broken_config(&["ls-fonts"]);

    assert_eq!(exit_code(&output), 1, "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("the config is broken"),
        "stderr should carry the config error, got: {}",
        stderr(&output)
    );
}

#[test]
fn show_keys_fails_when_the_configuration_does_not_load() {
    // The interesting half is stdout: this used to print the default key
    // table, which reads as though it were the user's.
    let (_dir, output) = run_with_a_broken_config(&["show-keys"]);

    assert_eq!(exit_code(&output), 1, "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("the config is broken"),
        "stderr should carry the config error, got: {}",
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("CTRL"),
        "no key table should be printed, got: {}",
        stdout(&output)
    );
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
