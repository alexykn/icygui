//! REL-06: `icygui --version` and `--help` answer without opening a window
//! (the Homebrew formula test and package managers run them on machines
//! without a display), and a wrong option is a usage error.

#![expect(clippy::unwrap_used, reason = "tests fail loudly")]

use std::process::{Command, Stdio};

fn icygui(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_icygui"))
        .args(args)
        // No display: opening a window would fail (or hang), so passing
        // proves none is opened.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn version_prints_the_version() {
    let output = icygui(&["--version"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("icygui {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty(), "nothing is logged");
    let short = icygui(&["-V"]);
    assert!(short.status.success());
}

#[test]
fn help_prints_the_usage() {
    let output = icygui(&["--help"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Usage: icygui [OPTIONS]"), "{text}");
    assert!(text.contains("--demo"), "{text}");
}

#[test]
fn unknown_options_are_usage_errors() {
    let output = icygui(&["--frobnicate"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(text.contains("unknown option --frobnicate"), "{text}");
    assert!(text.contains("Usage:"), "{text}");
    assert!(output.stdout.is_empty());
}
