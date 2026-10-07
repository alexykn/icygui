//! macOS launch agents (`launchd.plist(5)`).
//!
//! The agent is loaded by launchd at the next login; writing the file
//! doesn't start anything now (a `launchctl bootstrap` would, and with
//! `RunAtLoad` it would start a second instance right away).
//!
//! launchd also keeps its own list of disabled jobs, outside the file:
//! `launchctl disable gui/<uid>/<label>` adds to it, and so can switching
//! the app off under System Settings › General › Login Items. A job on
//! that list isn't loaded however the file reads, so the `launchd` module
//! (macOS only) asks launchd too.

use super::BACKGROUND_ARG;

/// The property list that starts `exe --background` at login.
///
/// - `LimitLoadToSessionType = Aqua`: only in graphical login sessions, not
///   for SSH logins.
/// - `ProcessType = Interactive`: an app the user interacts with, so
///   launchd applies no background throttling.
/// - `AssociatedBundleIdentifiers`: lets System Settings (Login Items, macOS
///   13+) show the app's name and icon for the agent. The app id is the
///   bundle identifier.
pub(super) fn render(app_id: &str, exe: &str) -> String {
    let label = escape(app_id);
    let exe = escape(exe);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>{BACKGROUND_ARG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{label}</string>
	</array>
</dict>
</plist>
"#
    )
}

/// Whether launchd will load the agent: not marked `Disabled`.
pub(super) fn is_active(contents: &str) -> bool {
    let compact: String = contents.split_whitespace().collect();
    compact.contains("<plist") && !compact.contains("<key>Disabled</key><true/>")
}

/// Whether the output of `launchctl print-disabled <domain>` lists `label`
/// as disabled. The list reads `"label" => disabled` (or `enabled`) since
/// macOS 13 and `"label" => true` (or `false`) before; labels that aren't
/// listed aren't disabled. Only the `disabled services` block counts.
#[cfg(any(target_os = "macos", test))]
pub(super) fn disabled_in(print_disabled: &str, label: &str) -> bool {
    let mut in_services = false;
    for line in print_disabled.lines() {
        let line = line.trim();
        if let Some(block) = line.strip_suffix('{') {
            in_services = block.trim_end().trim_end_matches('=').trim() == "disabled services";
            continue;
        }
        if line.starts_with('}') {
            in_services = false;
            continue;
        }
        if !in_services {
            continue;
        }
        let Some((key, value)) = line.split_once("=>") else {
            continue;
        };
        let key = key.trim();
        let listed = key
            .strip_prefix('"')
            .and_then(|key| key.strip_suffix('"'))
            .unwrap_or(key);
        if listed == label {
            return matches!(value.trim(), "disabled" | "true");
        }
    }
    false
}

/// launchd's own record of disabled jobs in the user's GUI domain.
#[cfg(target_os = "macos")]
pub(super) mod launchd {
    use std::process::{Command, Output};

    use crate::PlatformError;

    /// Absolute, so a `launchctl` earlier in `PATH` can't stand in.
    const LAUNCHCTL: &str = "/bin/launchctl";

    /// The domain of agents in the user's graphical login session.
    fn domain() -> String {
        format!("gui/{}", rustix::process::getuid().as_raw())
    }

    fn launchctl(args: &[&str]) -> Option<Output> {
        match Command::new(LAUNCHCTL).args(args).output() {
            Ok(output) if output.status.success() => Some(output),
            Ok(output) => {
                tracing::warn!(
                    ?args,
                    status = %output.status,
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "launchctl failed"
                );
                None
            }
            Err(error) => {
                tracing::warn!(?args, %error, "cannot run launchctl");
                None
            }
        }
    }

    /// Whether launchd has the job `label` disabled; `None` if launchctl
    /// can't tell.
    pub(crate) fn is_disabled(label: &str) -> Option<bool> {
        let output = launchctl(&["print-disabled", &domain()])?;
        Some(super::disabled_in(
            &String::from_utf8_lossy(&output.stdout),
            label,
        ))
    }

    /// Clears launchd's disabled mark for `label`, which the user may do
    /// for jobs in their own GUI domain.
    ///
    /// # Errors
    ///
    /// [`PlatformError::DisabledBySystem`]: the job is still disabled
    /// afterwards (macOS keeps it off until the user allows it again in
    /// System Settings).
    pub(crate) fn enable(label: &str) -> Result<(), PlatformError> {
        if is_disabled(label) != Some(true) {
            return Ok(());
        }
        let target = format!("{}/{label}", domain());
        if launchctl(&["enable", &target]).is_some() {
            tracing::info!(%target, "cleared launchd's disabled mark");
        }
        if is_disabled(label) == Some(true) {
            return Err(PlatformError::DisabledBySystem);
        }
        Ok(())
    }
}

/// XML text escaping. Carriage returns become a character reference,
/// because XML parsers turn a literal one into a line feed.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\r' => escaped.push_str("&#13;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `<string>` values in document order, unescaped.
    fn strings(plist: &str) -> Vec<String> {
        plist
            .split("<string>")
            .skip(1)
            .map(|rest| {
                let (value, _) = rest.split_once("</string>").unwrap();
                value
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&#13;", "\r")
                    .replace("&amp;", "&")
            })
            .collect()
    }

    #[test]
    fn agent_contents() {
        let plist = render(
            "io.github.alexykn.icygui",
            "/Applications/icygui.app/Contents/MacOS/icygui",
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(
            plist.contains("\t<key>Label</key>\n\t<string>io.github.alexykn.icygui</string>\n")
        );
        assert!(plist.contains(
            "\t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/Applications/icygui.app/Contents/MacOS/icygui</string>\n\
             \t\t<string>--background</string>\n\t</array>\n"
        ));
        assert!(plist.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
        assert!(!plist.contains("KeepAlive"), "quitting must stay quit");
        assert_eq!(
            strings(&plist),
            [
                "io.github.alexykn.icygui",
                "/Applications/icygui.app/Contents/MacOS/icygui",
                "--background",
                "Aqua",
                "Interactive",
                "io.github.alexykn.icygui",
            ]
        );
        assert!(is_active(&plist));
    }

    #[test]
    fn special_characters_are_escaped() {
        let exe = "/Users/me/Apps & <Tools>/\"it's\"/a\rb/icygui";
        let plist = render("a.b", exe);
        assert!(
            plist.contains(
                "<string>/Users/me/Apps &amp; &lt;Tools&gt;/&quot;it&apos;s&quot;/a&#13;b/icygui</string>"
            ),
            "{plist}"
        );
        assert_eq!(strings(&plist)[1], exe);
    }

    const LABEL: &str = "io.github.alexykn.icygui";

    #[test]
    fn launchd_overrides_since_macos_13() {
        let output = "disabled services = {\n\
                      \t\"com.apple.ScreenReaderUIServer\" => disabled\n\
                      \t\"io.github.alexykn.icygui\" => disabled\n\
                      \t\"com.apple.Siri.agent\" => enabled\n\
                      }\n\
                      \n\
                      login item associations = {\n\
                      \t\"com.example.helper\" => \"com.example.app\"\n\
                      }\n";
        assert!(disabled_in(output, LABEL));
        assert!(disabled_in(output, "com.apple.ScreenReaderUIServer"));
        assert!(!disabled_in(output, "com.apple.Siri.agent"));
        assert!(!disabled_in(output, "com.example.helper"), "other blocks");
        assert!(!disabled_in(
            &output.replace(
                "\"io.github.alexykn.icygui\" => disabled",
                "\"io.github.alexykn.icygui\" => enabled"
            ),
            LABEL
        ));
    }

    #[test]
    fn launchd_overrides_before_macos_13() {
        let output = "disabled services = {\n\
                      \t\"com.apple.ftp-proxy\" => true\n\
                      \t\"io.github.alexykn.icygui\" => false\n\
                      }\n";
        assert!(disabled_in(output, "com.apple.ftp-proxy"));
        assert!(!disabled_in(output, LABEL));
        assert!(disabled_in(&output.replace("=> false", "=> true"), LABEL));
    }

    #[test]
    fn unlisted_and_similar_labels_are_not_disabled() {
        let output = "disabled services = {\n\
                      \t\"io.github.alexykn.icygui.helper\" => disabled\n\
                      \t\"x.io.github.alexykn.icygui\" => disabled\n\
                      }\n";
        assert!(!disabled_in(output, LABEL));
        assert!(!disabled_in("", LABEL));
        assert!(!disabled_in(
            "garbage\n\"io.github.alexykn.icygui\" => disabled\n",
            LABEL
        ));
    }

    #[test]
    fn disabled_agents_are_detected() {
        let plist = render("a.b", "/x");
        let disabled = plist.replace("<dict>\n", "<dict>\n\t<key>Disabled</key>\n\t<true/>\n");
        assert!(!is_active(&disabled));
        let enabled = plist.replace("<dict>\n", "<dict>\n\t<key>Disabled</key>\n\t<false/>\n");
        assert!(is_active(&enabled));
        assert!(!is_active(""));
    }
}
