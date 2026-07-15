//! Integration tests against the built `umami` binary.

use std::process::Command;

fn umami() -> Command {
    Command::new(env!("CARGO_BIN_EXE_umami"))
}

#[test]
fn status_runs_and_reports_memory() {
    let out = umami().arg("status").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("memory:"));
    assert!(stdout.contains("memory pressure (PSI):"));
    assert!(stdout.contains("vm.swappiness"));
}

#[test]
fn help_exits_zero() {
    let out = umami().arg("help").output().unwrap();
    assert!(out.status.success());
}

#[test]
fn unknown_command_exits_two() {
    let out = umami().arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn no_args_exits_two() {
    let out = umami().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
