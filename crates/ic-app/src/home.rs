//! The home directory in paths people type and read: `~/certs/ca.pem` in
//! a path field, `~/.config/icygui` in the settings. One pair of helpers,
//! so every field reads a path the same way (surrounding spaces ignored, a
//! lone `~` is the home directory too) and shows it the same way back.

use std::path::{Path, PathBuf};

/// A path as typed, with a leading `~/` (or a lone `~`) meaning the home
/// directory. Surrounding spaces are ignored.
pub(crate) fn expand_home(text: &str) -> PathBuf {
    expand_in(text, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

/// `path` with the home directory written as `~` (`~/.config/icygui`):
/// the inverse of [`expand_home`].
pub(crate) fn tilde(path: &Path) -> String {
    tilde_in(path, std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

fn expand_in(text: &str, home: Option<&Path>) -> PathBuf {
    let text = text.trim();
    match (text.strip_prefix("~/"), text == "~", home) {
        (Some(rest), _, Some(home)) => home.join(rest),
        (None, true, Some(home)) => home.to_path_buf(),
        _ => PathBuf::from(text),
    }
}

fn tilde_in(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_paths_expand_the_home_directory() {
        let home = Path::new("/home/ops");
        let expand = |text: &str| expand_in(text, Some(home));
        assert_eq!(expand("~/certs/ca.pem"), home.join("certs/ca.pem"));
        assert_eq!(expand(" ~ "), home, "a lone ~ and the spaces around it");
        assert_eq!(expand("  ~/x.toml "), home.join("x.toml"));
        assert_eq!(expand("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand("~other/x"), PathBuf::from("~other/x"));
        assert_eq!(
            expand_in("~/x", None),
            PathBuf::from("~/x"),
            "no home: left as typed"
        );
    }

    #[test]
    fn the_home_directory_reads_as_a_tilde() {
        let home = Path::new("/home/ops");
        assert_eq!(
            tilde_in(&home.join(".config/icygui"), Some(home)),
            "~/.config/icygui"
        );
        assert_eq!(tilde_in(home, Some(home)), "~");
        assert_eq!(
            tilde_in(Path::new("/etc/icygui"), Some(home)),
            "/etc/icygui"
        );
        assert_eq!(
            tilde_in(Path::new("/home/opsy/x"), Some(home)),
            "/home/opsy/x"
        );
    }

    #[test]
    fn tilde_undoes_expand_home() {
        let home = Path::new("/home/ops");
        for text in ["~", "~/a/b.pem"] {
            assert_eq!(tilde_in(&expand_in(text, Some(home)), Some(home)), text);
        }
    }
}
