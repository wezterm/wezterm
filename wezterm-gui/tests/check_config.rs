//! End-to-end tests for `wezterm-gui check-config`.
//!
//! The command's contract is its exit code, and the wiring that carries its
//! options into the lua state runs inside `run()`, out of a unit test's
//! reach.  So these spawn the built binary; it exits before any window
//! code, so they run headless.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Runs `check-config` over a configuration file holding `config_body`.
///
/// The `WEZTERM_CONFIG_*` variables are cleared from the child, since
/// wezterm exports them to the programs it runs and `WEZTERM_CONFIG_FILE`
/// would take precedence over the file named here.
fn check(config_body: &str, extra_args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = dir.path().join("wezterm.lua");
    std::fs::write(&config, config_body).expect("write config file");

    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_wezterm-gui")))
        .arg("check-config")
        .args(extra_args)
        .arg(&config)
        .env_remove("WEZTERM_CONFIG_FILE")
        .env_remove("WEZTERM_CONFIG_DIR")
        .output()
        .expect("run wezterm-gui check-config")
}

/// Runs `check-config` against a path that was never created.
fn check_missing_file() -> Output {
    let dir = tempfile::tempdir().expect("create temp dir");

    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_wezterm-gui")))
        .arg("check-config")
        .arg(dir.path().join("not-here.lua"))
        .env_remove("WEZTERM_CONFIG_FILE")
        .env_remove("WEZTERM_CONFIG_DIR")
        .output()
        .expect("run wezterm-gui check-config")
}

/// Runs `check-config` with no configuration file reachable: none named,
/// and the search paths pointed at an empty directory.
fn check_with_nothing_to_find(global_args: &[&str], sub_args: &[&str]) -> Output {
    let home = tempfile::tempdir().expect("create temp home");

    Command::new(PathBuf::from(env!("CARGO_BIN_EXE_wezterm-gui")))
        .args(global_args)
        .arg("check-config")
        .args(sub_args)
        .env_remove("WEZTERM_CONFIG_FILE")
        .env_remove("WEZTERM_CONFIG_DIR")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .output()
        .expect("run wezterm-gui check-config")
}

fn exit_code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("check-config exited via a signal rather than a status")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_good_configuration_exits_zero_and_names_the_file() {
    let output = check("return {}\n", &[]);

    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("config ok:"),
        "stdout: {}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("wezterm.lua"),
        "the success line should name the file it checked, got: {}",
        stdout(&output)
    );
}

#[test]
fn a_configuration_that_raises_an_error_exits_one() {
    // The failed-assertion path a configuration-as-test-suite relies on.
    let output = check("error('a test failed')\nreturn {}\n", &[]);

    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("a test failed"),
        "stderr should carry the lua error, got: {}",
        stderr(&output)
    );
}

#[test]
fn a_failing_configuration_is_not_reported_as_a_missing_one() {
    // A failed load leaves `WEZTERM_CONFIG_FILE` unset, which is also how
    // "no file was found" is detected.  The file was found, so saying
    // otherwise would contradict the error printed beside it.
    let output = check("error('a test failed')\nreturn {}\n", &[]);

    assert!(
        !stderr(&output).contains("no configuration file was found"),
        "a file was named and found, got: {}",
        stderr(&output)
    );
}

#[test]
fn a_configuration_that_returns_nothing_exits_one() {
    // A file that merely runs cleanly is not a pass.
    let output = check("local config = {}\n", &[]);

    assert_eq!(exit_code(&output), 1, "stdout: {}", stdout(&output));
}

#[test]
fn the_debug_module_is_withheld_unless_asked_for() {
    // Without the negative case, always enabling `debug` would pass.
    let body = "assert(debug ~= nil, 'debug module missing')\nreturn {}\n";

    let withheld = check(body, &[]);
    assert_eq!(
        exit_code(&withheld),
        1,
        "debug must be absent by default, stdout: {}",
        stdout(&withheld)
    );

    let available = check(body, &["--unsafe-enable-debug-module"]);
    assert_eq!(
        exit_code(&available),
        0,
        "--unsafe-enable-debug-module should provide it, stderr: {}",
        stderr(&available)
    );
}

#[test]
fn the_simulated_appearance_follows_the_flag() {
    let body = "local w = require 'wezterm'\n\
                if w.gui.get_appearance():find('Dark') then\n\
                  error('the dark branch ran')\n\
                end\n\
                return {}\n";

    assert_eq!(exit_code(&check(body, &[])), 0);
    assert_eq!(exit_code(&check(body, &["--appearance", "light"])), 0);

    let dark = check(body, &["--appearance", "dark"]);
    assert_eq!(exit_code(&dark), 1);
    assert!(
        stderr(&dark).contains("the dark branch ran"),
        "stderr: {}",
        stderr(&dark)
    );
}

#[test]
fn screens_are_available_rather_than_an_error() {
    // Without the simulated display this raises "cannot get window
    // Connection", failing a configuration that a real wezterm runs.
    let output = check(
        "local w = require 'wezterm'\n\
         local s = w.gui.screens()\n\
         assert(s.active.width > 0, 'screen should have a width')\n\
         return {}\n",
        &[],
    );

    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
}

#[test]
fn naming_a_configuration_file_that_is_not_there_exits_one() {
    // A failure, not a quiet fallback to the defaults.
    let output = check_missing_file();

    assert_eq!(exit_code(&output), 1, "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("not-here.lua"),
        "the error should name the file, got: {}",
        stderr(&output)
    );
}

#[test]
fn finding_no_configuration_file_passes_but_says_so() {
    // The built-in defaults are a valid configuration, so this passes --
    // audibly, for the sake of a job whose configuration never landed.
    let output = check_with_nothing_to_find(&[], &[]);

    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("built-in defaults"),
        "stdout: {}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("no configuration file was found"),
        "it should warn that nothing of yours was checked, got: {}",
        stderr(&output)
    );
}

#[test]
fn finding_no_configuration_file_fails_when_warnings_are_errors() {
    // Falling back to the defaults is announced as a warning, so asking
    // for warnings to be fatal has to catch this one too.
    let output = check_with_nothing_to_find(&[], &["--warnings-as-errors"]);

    assert_eq!(exit_code(&output), 1, "stdout: {}", stdout(&output));
    assert!(
        stderr(&output).contains("no configuration file was found"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn skipping_the_configuration_does_not_warn_about_finding_none() {
    // `--skip-config` asked for the defaults, so their use is not news --
    // not even under `--warnings-as-errors`.
    let output = check_with_nothing_to_find(&["--skip-config"], &["--warnings-as-errors"]);

    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        !stderr(&output).contains("no configuration file was found"),
        "asked-for defaults should not warn, got: {}",
        stderr(&output)
    );
}

#[test]
fn warnings_pass_unless_treated_as_errors() {
    let body = "return { not_a_real_option = true }\n";

    assert_eq!(exit_code(&check(body, &[])), 0);
    assert_eq!(exit_code(&check(body, &["--warnings-as-errors"])), 1);
}
