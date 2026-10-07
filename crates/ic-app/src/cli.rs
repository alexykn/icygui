//! Command-line options. `--version` and `--help` answer without opening a
//! window or touching any file (REL-06: package managers and the Homebrew
//! formula test run them on machines without a display).

use std::ffi::OsString;
use std::fmt;

/// The binary's name, as the usage text shows it.
const BINARY: &str = "icygui";

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Invocation {
    /// Start the app.
    Run(Options),
    /// Print the version and exit.
    Version,
    /// Print the usage and exit.
    Help,
}

/// How to start the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--demo`: run against the built-in simulated Icinga (ENV-10).
    pub(crate) demo: bool,
    /// `--background`: start in the tray without the window (launch at
    /// login, BG-03).
    pub(crate) background: bool,
}

/// A command line that can't be understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reads the arguments (without the program name). The first of
/// `--help` and `--version` wins over everything after it.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, UsageError> {
    let mut options = Options::default();
    for arg in args {
        let Some(arg) = arg.to_str() else {
            return Err(UsageError(format!(
                "unexpected argument {}",
                arg.to_string_lossy()
            )));
        };
        match arg {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--demo" => options.demo = true,
            "--background" => options.background = true,
            // macOS passes a process serial number when Finder starts an
            // app bundle on older systems.
            psn if psn.starts_with("-psn_") => {}
            other if other.starts_with('-') => {
                return Err(UsageError(format!("unknown option {other}")));
            }
            other => return Err(UsageError(format!("unexpected argument {other}"))),
        }
    }
    Ok(Invocation::Run(options))
}

/// `icygui 0.1.0`, for `--version`.
pub(crate) fn version_text() -> String {
    format!("{BINARY} {}\n", env!("CARGO_PKG_VERSION"))
}

/// The usage text, for `--help` and after a usage error.
pub(crate) fn help_text() -> String {
    format!(
        "{BINARY} {version}: a desktop client for the Icinga 2 monitoring API.\n\
         \n\
         Usage: {BINARY} [OPTIONS]\n\
         \n\
         Options:\n\
         \x20     --demo     Run against a built-in simulated Icinga (no server or\n\
         \x20                credentials needed; nothing is saved)\n\
         \x20     --background\n\
         \x20                Start in the tray without the window (as at login)\n\
         \x20 -V, --version  Print the version and exit\n\
         \x20 -h, --help     Print this help and exit\n\
         \n\
         Environment:\n\
         \x20 RUST_LOG       Log filter (default: info); logs also go to the log\n\
         \x20                directory (Linux: ~/.local/state/icygui/logs,\n\
         \x20                macOS: ~/Library/Logs/io.github.alexykn.icygui)\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Invocation, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_run_the_app() {
        assert_eq!(parse_strs(&[]), Ok(Invocation::Run(Options::default())));
    }

    #[test]
    fn demo_runs_the_demo() {
        assert_eq!(
            parse_strs(&["--demo"]),
            Ok(Invocation::Run(Options {
                demo: true,
                background: false
            }))
        );
    }

    #[test]
    fn background_starts_in_the_tray() {
        assert_eq!(
            parse_strs(&["--background"]),
            Ok(Invocation::Run(Options {
                demo: false,
                background: true
            }))
        );
        assert!(help_text().contains("--background"));
    }

    #[test]
    fn help_and_version_answer_without_running() {
        assert_eq!(parse_strs(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Invocation::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Invocation::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Invocation::Version));
        assert_eq!(
            parse_strs(&["--demo", "--version", "--bogus"]),
            Ok(Invocation::Version),
            "the first of them wins over what follows"
        );
    }

    #[test]
    fn unknown_arguments_are_usage_errors() {
        assert_eq!(
            parse_strs(&["--bogus"]),
            Err(UsageError("unknown option --bogus".to_owned()))
        );
        assert!(parse_strs(&["file.toml"]).is_err());
    }

    #[test]
    fn finder_process_serial_numbers_are_ignored() {
        assert_eq!(
            parse_strs(&["-psn_0_1234567"]),
            Ok(Invocation::Run(Options::default()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn arguments_that_arent_text_are_usage_errors() {
        use std::os::unix::ffi::OsStringExt as _;
        let invalid = OsString::from_vec(vec![0x2d, 0xff, 0xfe]);
        assert!(parse([invalid]).is_err());
    }

    #[test]
    fn texts_name_the_version() {
        assert_eq!(
            version_text(),
            format!("icygui {}\n", env!("CARGO_PKG_VERSION"))
        );
        let help = help_text();
        assert!(help.contains("--demo"));
        assert!(help.contains("--version"));
        assert!(help.lines().all(|line| line.len() <= 80), "{help}");
    }
}
