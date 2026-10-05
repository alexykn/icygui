//! `GET /v1`: who we are and what we may do.

/// The authenticated API user, its permissions and the Icinga version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiInfo {
    /// The `ApiUser` name.
    pub user: String,
    /// Permission patterns as Icinga lists them (`objects/query/*`,
    /// `actions/acknowledge-problem (filtered)`).
    pub permissions: Vec<String>,
    /// Icinga version (`v2.15.6`).
    pub version: String,
}

/// The suffix `GET /v1` appends to permissions restricted by a filter.
const FILTERED_SUFFIX: &str = " (filtered)";

impl ApiInfo {
    /// Whether the user holds `permission` (for example
    /// `actions/acknowledge-problem` or `objects/query/Host`).
    ///
    /// Follows Icinga's `FilterUtility::HasPermission`: both sides are
    /// compared case-insensitively and each granted entry is a glob
    /// pattern (`*` matches any run of characters including `/`, `?` one
    /// character), so `*`, `actions/*` and `objects/query/*` work. Entries
    /// restricted by a filter (`… (filtered)`) count as allowed: the
    /// filter limits which objects, not whether the endpoint can be used.
    #[must_use]
    pub fn allows(&self, permission: &str) -> bool {
        if permission.is_empty() {
            return true;
        }
        let required = permission.to_lowercase();
        self.permissions.iter().any(|granted| {
            let pattern = granted
                .strip_suffix(FILTERED_SUFFIX)
                .unwrap_or(granted)
                .trim()
                .to_lowercase();
            glob_match(&pattern, &required)
        })
    }
}

/// Glob matching as Icinga's `Utility::Match`: `*` matches any sequence
/// (including empty and `/`), `?` matches exactly one character, `\`
/// escapes the next character.
fn glob_match(pattern: &str, text: &str) -> bool {
    #[derive(Clone, Copy)]
    enum Token {
        Any,
        One,
        Char(char),
    }
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '*' => Token::Any,
            '?' => Token::One,
            '\\' => Token::Char(chars.next().unwrap_or('\\')),
            other => Token::Char(other),
        });
    }
    let text: Vec<char> = text.chars().collect();

    // Iterative wildcard matching with single-star backtracking.
    let (mut t, mut p) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match tokens.get(p) {
            Some(Token::Any) => {
                backtrack = Some((p, t));
                p += 1;
            }
            Some(Token::One) => {
                p += 1;
                t += 1;
            }
            Some(Token::Char(c)) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    p = star + 1;
                    t = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    tokens[p..].iter().all(|token| matches!(token, Token::Any))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(permissions: &[&str]) -> ApiInfo {
        ApiInfo {
            user: "icygui".to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            version: "v2.15.6".to_owned(),
        }
    }

    #[test]
    fn star_allows_everything() {
        let all = info(&["*"]);
        assert!(all.allows("actions/acknowledge-problem"));
        assert!(all.allows("objects/query/Host"));
        assert!(all.allows("events/StateChange"));
    }

    #[test]
    fn hierarchical_wildcards() {
        let user = info(&["objects/query/*", "status/query", "events/*", "actions/*"]);
        assert!(user.allows("objects/query/Host"));
        assert!(user.allows("objects/query/Service"));
        assert!(user.allows("status/query"));
        assert!(user.allows("events/CheckResult"));
        assert!(user.allows("actions/schedule-downtime"));
        assert!(!user.allows("objects/modify/Host"));
        assert!(!user.allows("config/modify"));
        assert!(!user.allows("console"));
    }

    #[test]
    fn exact_permissions_are_case_insensitive() {
        let viewer = info(&["objects/query/Host", "events/StateChange"]);
        assert!(viewer.allows("objects/query/host"));
        assert!(viewer.allows("OBJECTS/QUERY/HOST"));
        assert!(viewer.allows("events/statechange"));
        assert!(!viewer.allows("objects/query/Service"));
        assert!(
            !viewer.allows("objects/query/Hostgroup"),
            "no prefix matching"
        );
        assert!(!viewer.allows("actions/acknowledge-problem"));
    }

    #[test]
    fn filtered_permissions_count_as_allowed() {
        let user = info(&["objects/query/Host (filtered)", "actions/* (filtered)"]);
        assert!(user.allows("objects/query/Host"));
        assert!(user.allows("actions/remove-comment"));
        assert!(!user.allows("objects/query/Service"));
    }

    #[test]
    fn no_permissions_allow_nothing() {
        assert!(!info(&[]).allows("status/query"));
        assert!(info(&[]).allows(""), "an empty requirement is always met");
    }

    #[test]
    fn glob_details() {
        assert!(glob_match("actions/*", "actions/"));
        assert!(glob_match("*/query/*", "objects/query/host"));
        assert!(glob_match("events/??", "events/ab"));
        assert!(!glob_match("events/??", "events/abc"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("**", ""));
        assert!(glob_match(r"literal\*", "literal*"));
        assert!(!glob_match(r"literal\*", "literalx"));
        assert!(!glob_match("", "x"));
    }
}
