//! macOS launch agents (`launchd.plist(5)`).
//!
//! The agent is loaded by launchd at the next login; writing the file
//! doesn't start anything now (a `launchctl bootstrap` would, and with
//! `RunAtLoad` it would start a second instance right away).

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
