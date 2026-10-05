//! XDG autostart entries (freedesktop Desktop Entry Specification 1.5 and
//! the Desktop Application Autostart Specification).
//!
//! The `Exec` value is encoded in the spec's three layers, innermost first:
//! literal `%` becomes the field code `%%`; an argument with a reserved
//! character is put in double quotes with `"`, `` ` ``, `$` and `\`
//! backslash-escaped; and the whole value gets the general string escapes
//! (`\\`, `\n`, `\t`, `\r`). Readers undo them in the opposite order.

use super::BACKGROUND_ARG;

/// The `.desktop` file that starts `exe --background` at login.
pub(super) fn render(app_id: &str, app_name: &str, exe: &str) -> String {
    let exec = format!("{} {BACKGROUND_ARG}", exec_argument(exe));
    let lines = [
        "[Desktop Entry]".to_owned(),
        "Type=Application".to_owned(),
        "Version=1.5".to_owned(),
        format!("Name={}", escape_value(app_name)),
        format!(
            "Comment={}",
            escape_value(&format!("Start {app_name} in the background at login"))
        ),
        format!("Exec={}", escape_value(&exec)),
        format!("Icon={}", escape_value(app_id)),
        "Terminal=false".to_owned(),
        // No window appears at login, so don't show a busy cursor.
        "StartupNotify=false".to_owned(),
        "X-GNOME-Autostart-enabled=true".to_owned(),
    ];
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Whether the entry still starts the app: it has a `[Desktop Entry]` group
/// with an `Exec` key, and the desktop hasn't disabled it with
/// `Hidden=true` (the autostart spec) or `X-GNOME-Autostart-enabled=false`
/// (GNOME's settings).
pub(super) fn is_active(contents: &str) -> bool {
    let mut in_main_group = false;
    let mut has_exec = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_group || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => return false,
            ("Exec", exec) => has_exec = !exec.is_empty(),
            _ => {}
        }
    }
    has_exec
}

/// Characters that force an `Exec` argument into quotes.
fn is_reserved(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '"'
            | '\''
            | '\\'
            | '>'
            | '<'
            | '~'
            | '|'
            | '&'
            | ';'
            | '$'
            | '*'
            | '?'
            | '#'
            | '('
            | ')'
            | '`'
    ) || c.is_control()
}

/// One `Exec` argument: field-code and quoting layers (the string layer is
/// applied to the whole value by [`escape_value`]).
pub(super) fn exec_argument(argument: &str) -> String {
    let argument = argument.replace('%', "%%");
    if !argument.is_empty() && !argument.chars().any(is_reserved) {
        return argument;
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    for c in argument.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// The general escapes for `string` and `localestring` values. A leading
/// space would be dropped by readers, so it becomes `\s`.
pub(super) fn escape_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for (index, c) in value.chars().enumerate() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            ' ' if index == 0 => escaped.push_str("\\s"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader written from the spec: string escapes, then quoting and
    /// argument splitting, then field codes.
    fn decode_exec(value: &str) -> Vec<String> {
        let mut unescaped = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next().unwrap() {
                    's' => unescaped.push(' '),
                    'n' => unescaped.push('\n'),
                    't' => unescaped.push('\t'),
                    'r' => unescaped.push('\r'),
                    '\\' => unescaped.push('\\'),
                    other => panic!("invalid string escape \\{other} in {value:?}"),
                }
            } else {
                unescaped.push(c);
            }
        }

        let mut arguments = Vec::new();
        let mut current: Option<String> = None;
        let mut chars = unescaped.chars();
        while let Some(c) = chars.next() {
            match c {
                ' ' => arguments.extend(current.take()),
                '"' => {
                    let argument = current.get_or_insert_with(String::new);
                    loop {
                        match chars.next().expect("unterminated quote") {
                            '"' => break,
                            '\\' => {
                                let escaped = chars.next().unwrap();
                                assert!(
                                    matches!(escaped, '"' | '`' | '$' | '\\'),
                                    "invalid quote escape \\{escaped} in {value:?}"
                                );
                                argument.push(escaped);
                            }
                            c => argument.push(c),
                        }
                    }
                }
                c => {
                    assert!(!is_reserved(c), "unquoted reserved {c:?} in {value:?}");
                    current.get_or_insert_with(String::new).push(c);
                }
            }
        }
        arguments.extend(current);
        arguments
            .into_iter()
            .map(|argument| {
                let mut expanded = String::new();
                let mut chars = argument.chars();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        assert_eq!(chars.next(), Some('%'), "stray field code in {value:?}");
                    }
                    expanded.push(c);
                }
                expanded
            })
            .collect()
    }

    fn exec_line(contents: &str) -> &str {
        contents
            .lines()
            .find_map(|line| line.strip_prefix("Exec="))
            .unwrap()
    }

    #[test]
    fn entry_contents() {
        assert_eq!(
            render("io.github.alexykn.icygui", "icygui", "/usr/bin/icygui"),
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.5\n\
             Name=icygui\n\
             Comment=Start icygui in the background at login\n\
             Exec=/usr/bin/icygui --background\n\
             Icon=io.github.alexykn.icygui\n\
             Terminal=false\n\
             StartupNotify=false\n\
             X-GNOME-Autostart-enabled=true\n"
        );
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let contents = render("a.b", "x", "/home/me/My Apps/icygui");
        assert_eq!(
            exec_line(&contents),
            r#""/home/me/My Apps/icygui" --background"#
        );
        assert_eq!(
            decode_exec(exec_line(&contents)),
            ["/home/me/My Apps/icygui", "--background"]
        );
    }

    #[test]
    fn quoting_follows_the_spec_examples() {
        // "a literal dollar sign in a quoted argument in a desktop entry
        // file is unambiguously represented with (\\$)"
        assert_eq!(
            escape_value(&exec_argument("/opt/a b/$x")),
            r#""/opt/a b/\\$x""#
        );
        // "to unambiguously represent a literal backslash character in a
        // quoted argument ... requires the use of four successive
        // backslash characters"
        assert_eq!(
            escape_value(&exec_argument(r"/opt/a\b")),
            r#""/opt/a\\\\b""#
        );
        assert_eq!(escape_value(&exec_argument("/opt/100%/x")), "/opt/100%%/x");
        assert_eq!(exec_argument("/usr/bin/icygui"), "/usr/bin/icygui");
        assert_eq!(exec_argument(""), r#""""#);
    }

    #[test]
    fn every_reserved_character_is_quoted() {
        for c in [
            ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#',
            '(', ')', '`',
        ] {
            let argument = exec_argument(&format!("/opt/a{c}b"));
            assert!(
                argument.starts_with('"') && argument.ends_with('"'),
                "{c:?}: {argument}"
            );
        }
        for c in ['=', '-', '_', '.', ',', '+', '@', 'ü', '日'] {
            assert_eq!(
                exec_argument(&format!("/opt/a{c}b")),
                format!("/opt/a{c}b"),
                "{c:?}"
            );
        }
    }

    #[test]
    fn tricky_paths_survive_a_round_trip() {
        for path in [
            "/usr/bin/icygui",
            "/home/me/My Apps/icygui",
            "/opt/it's here/icygui",
            r#"/opt/"quoted"/icygui"#,
            "/opt/back`tick`/icygui",
            "/opt/$HOME/icygui",
            r"/opt/back\slash/icygui",
            r"/opt/trailing\",
            "/opt/100% sure/icygui",
            "/opt/%f/icygui",
            "/opt/~user/icygui",
            "/opt/a;b|c&d>e<f*g?h#i(j)k/icygui",
            "/opt/tab\there/icygui",
            "/opt/new\nline/icygui",
            "/opt/carriage\rreturn/icygui",
            "/opt/ünïcødé 日本/icygui",
            "/opt/  double  spaces  /icygui",
            r#"/opt/\"mixed\" $(x) `y` \\ %%/icygui"#,
        ] {
            let contents = render("a.b", "x", path);
            let exec = exec_line(&contents);
            assert!(!exec.contains('\n') && !exec.contains('\t'), "{exec:?}");
            assert_eq!(
                decode_exec(exec),
                [path, "--background"],
                "{path:?} → {exec:?}"
            );
        }
    }

    #[test]
    fn names_are_escaped() {
        let contents = render("a.b", " R&D\\ops\nteam", "/x");
        assert!(
            contents.contains("\nName=\\sR&D\\\\ops\\nteam\n"),
            "{contents}"
        );
        assert_eq!(contents.lines().count(), 10, "no raw newline in a value");
    }

    #[test]
    fn disabled_entries_are_detected() {
        let entry = render("a.b", "x", "/x");
        assert!(is_active(&entry));
        assert!(!is_active(&format!("{entry}Hidden=true\n")));
        assert!(!is_active(&format!("{entry}Hidden = true\n")));
        assert!(is_active(&format!("{entry}Hidden=false\n")));
        assert!(!is_active(
            &entry.replace("Autostart-enabled=true", "Autostart-enabled=false")
        ));
        // Keys in other groups and comments don't count.
        assert!(is_active(&format!(
            "{entry}# Hidden=true\n[Desktop Action x]\nHidden=true\n"
        )));
        // Not a usable entry at all.
        assert!(!is_active(""));
        assert!(!is_active("[Desktop Entry]\nName=x\n"));
        assert!(!is_active("[Other]\nExec=/x\n"));
    }
}
