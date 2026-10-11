//! ACT-08 (PLAN.md D6): the client only ever calls runtime operations. This
//! crate is the only one that talks HTTP, so its sources are checked: GET
//! and POST only, no configuration, console, object creation, change or
//! deletion, no process restart and no notification actions; object
//! queries go to `/v1/objects/<type>` and nothing deeper; every action
//! endpoint comes from `Action::api_name()` (checked against the allowed
//! list in `ic-model`) or is `remove-comment`. `ic-app`'s UI tests check
//! the requests themselves against the demo's mock.

#![expect(clippy::unwrap_used, reason = "tests fail loudly")]

use std::fs;
use std::path::Path;

/// The crate's sources without their test modules.
fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_none_or(|extension| extension != "rs") || name.ends_with("_tests.rs")
        {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let code = match text.find("#[cfg(test)]\nmod tests") {
            Some(end) => text[..end].to_owned(),
            None => text,
        };
        sources.push((name, code));
    }
    assert!(sources.len() >= 5, "found the sources: {sources:?}");
    sources
}

#[test]
fn only_get_and_post_are_used() {
    for (name, code) in sources() {
        for (index, _) in code.match_indices("Method::") {
            let method: String = code[index + "Method::".len()..]
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect();
            assert!(
                method == "GET" || method == "POST",
                "{name} uses HTTP {method}"
            );
        }
    }
}

#[test]
fn no_configuration_console_restart_or_notification_endpoints() {
    let forbidden = [
        "v1/config",
        "v1/console",
        "objects/modify",
        "restart-process",
        "shutdown-process",
        "send-custom-notification",
        "delay-notification",
        "generate-ticket",
        "X-HTTP-Method-Override\", HeaderValue::from_static(\"DELETE",
        "X-HTTP-Method-Override\", HeaderValue::from_static(\"PUT",
    ];
    for (name, code) in sources() {
        let code = code
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for marker in forbidden {
            assert!(!code.contains(marker), "{name} mentions {marker}");
        }
    }
}

#[test]
fn requests_go_to_the_known_paths_only() {
    let mut paths = Vec::new();
    for (name, code) in sources() {
        for (index, _) in code.match_indices(".request(reqwest::Method::") {
            let rest = &code[index..];
            let start = rest.find(',').unwrap() + 1;
            let end = rest[start..].find(")?").unwrap() + start;
            paths.push((
                name.clone(),
                rest[start..end].trim().replace(char::is_whitespace, ""),
            ));
        }
    }
    let mut found: Vec<&str> = paths.iter().map(|(_, path)| path.as_str()).collect();
    found.sort_unstable();
    assert_eq!(
        found,
        [
            "\"v1\"",
            "\"v1/events\"",
            "&format!(\"v1/actions/{endpoint}\")",
            "&format!(\"v1/objects/{plural}\")",
            "&format!(\"v1/status/{name}\")",
        ],
        "{paths:?}"
    );
    // Action endpoints are Icinga's action names: `api_name()` or the
    // comment removal, nothing assembled from user input.
    let actions = sources()
        .into_iter()
        .find(|(name, _)| name == "actions.rs")
        .map(|(_, code)| code)
        .unwrap();
    for (index, _) in actions.match_indices("endpoint: ") {
        let value: String = actions[index + "endpoint: ".len()..]
            .chars()
            .take_while(|c| *c != ',' && *c != '\n' && *c != '}')
            .collect();
        let value = value.trim();
        assert!(
            [
                "&'static str",
                "self.endpoint",
                "action.api_name()",
                "endpoint"
            ]
            .contains(&value),
            "an action endpoint from {value}"
        );
    }
    assert!(actions.contains("named(TargetKind::Comment, \"remove-comment\", names)"));
}
