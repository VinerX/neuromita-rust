use std::process::{Command, Output};

fn run(filter: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_neuromita"));
    command.arg("--invalid-option").env_remove("RUST_LOG");
    if let Some(filter) = filter {
        command.env("RUST_LOG", filter);
    }
    command.output().unwrap()
}

#[test]
fn errors_are_formatted_on_stderr_without_ansi_when_redirected() {
    let output = run(None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(log.contains("ERROR") && log.contains("application failed"));
    assert!(log.contains("unknown argument"));
    assert!(!log.contains("\x1b[") && !log.contains("DEBUG"));
}

#[test]
fn rust_log_controls_levels_and_invalid_filters_are_visible_errors() {
    let output = run(Some("neuromita=debug"));
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(log.contains("DEBUG") && log.contains("starting application"));
    let output = run(Some("neuromita=not_a_level"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Logging initialization failed"));
}
