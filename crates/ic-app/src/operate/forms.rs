//! What the action dialogs edit, and how it becomes an `ic_model::Action`
//! (ACT-02..06): plain data checked field by field, so a dialog can show
//! each problem next to its field and the tests need no window.
//!
//! Also which of the chosen objects an action applies to: Icinga refuses
//! to acknowledge an OK object or one acknowledged already (409 per
//! object), so the dialog says which are skipped instead of sending
//! requests bound to fail.

use std::collections::BTreeMap;
use std::time::Duration;

use ic_core::snapshot::Snapshot;
use ic_model::{Action, ChildOptions, CommandType, DowntimeMode, ObjectKey, Timestamp, Vars};
use serde_json::Value;

use super::when::{parse_duration, parse_time};
use crate::actions::ObjectAction;

/// A field of an action dialog, for its problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum FormField {
    /// The comment (acknowledgement, downtime, comment).
    Comment,
    /// When the acknowledgement or comment expires.
    Expiry,
    /// A downtime's start.
    Start,
    /// A downtime's end.
    End,
    /// A flexible downtime's duration.
    Duration,
    /// The downtime that triggers this one.
    Trigger,
    /// A passive result's plugin output.
    Output,
    /// A passive result's performance data.
    Perfdata,
    /// The command to run.
    Command,
    /// Where to run it.
    Endpoint,
    /// Macro overrides.
    Macros,
    /// How long Icinga keeps the execution's result.
    Ttl,
}

/// The problems of a form, by field.
pub(crate) type Issues = BTreeMap<FormField, String>;

/// Converts a time parser's result into a field problem.
fn time_field(
    issues: &mut Issues,
    field: FormField,
    parsed: Result<Timestamp, String>,
) -> Option<Timestamp> {
    match parsed {
        Ok(at) => Some(at),
        Err(error) => {
            issues.insert(field, error);
            None
        }
    }
}

/// A non-blank comment, else a problem.
fn comment(issues: &mut Issues, text: &str, what: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        issues.insert(
            FormField::Comment,
            format!("Say why ({what} needs a comment)."),
        );
    }
    trimmed.to_owned()
}

/// An expiry in the future, if one is wanted.
fn expiry(issues: &mut Issues, wanted: bool, text: &str, now: Timestamp) -> Option<Timestamp> {
    if !wanted {
        return None;
    }
    let at = time_field(issues, FormField::Expiry, parse_time(text, now, now))?;
    if at <= now {
        issues.insert(FormField::Expiry, "This is in the past.".to_owned());
        return None;
    }
    Some(at)
}

fn finish(action: Action, issues: Issues) -> Result<Action, Issues> {
    if issues.is_empty() {
        Ok(action)
    } else {
        Err(issues)
    }
}

/// The acknowledge dialog (ACT-02). Icinga is never asked to notify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AckForm {
    /// What is being done about it.
    pub(crate) comment: String,
    /// Stay acknowledged through changes between problem states, until
    /// the object is OK.
    pub(crate) sticky: bool,
    /// Keep the comment after the acknowledgement ends.
    pub(crate) persistent: bool,
    /// Whether the acknowledgement expires.
    pub(crate) expires: bool,
    /// When (see `when`).
    pub(crate) expiry: String,
}

impl Default for AckForm {
    fn default() -> Self {
        Self {
            comment: String::new(),
            sticky: false,
            persistent: false,
            expires: false,
            expiry: "+4h".to_owned(),
        }
    }
}

impl AckForm {
    /// The action, or the problems.
    ///
    /// # Errors
    ///
    /// The problems by field.
    pub(crate) fn action(&self, now: Timestamp) -> Result<Action, Issues> {
        let mut issues = Issues::new();
        let comment = comment(&mut issues, &self.comment, "an acknowledgement");
        let expiry = expiry(&mut issues, self.expires, &self.expiry, now);
        finish(
            Action::Acknowledge {
                comment,
                sticky: self.sticky,
                persistent: self.persistent,
                expiry,
            },
            issues,
        )
    }
}

/// The downtime dialog (ACT-03).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DowntimeForm {
    /// Why.
    pub(crate) comment: String,
    /// The window's start (see `when`; relative to now).
    pub(crate) start: String,
    /// The window's end (relative to the start).
    pub(crate) end: String,
    /// Flexible: starts with the first problem inside the window.
    pub(crate) flexible: bool,
    /// How long a flexible downtime lasts once it started.
    pub(crate) duration: String,
    /// For hosts: their services get downtimes too.
    pub(crate) all_services: bool,
    /// For hosts: downtimes for child hosts.
    pub(crate) child_options: ChildOptions,
    /// The full name of a downtime that triggers this one.
    pub(crate) trigger: String,
}

impl Default for DowntimeForm {
    fn default() -> Self {
        Self {
            comment: String::new(),
            start: "now".to_owned(),
            end: "+1h".to_owned(),
            flexible: false,
            duration: "1h".to_owned(),
            all_services: true,
            child_options: ChildOptions::None,
            trigger: String::new(),
        }
    }
}

impl DowntimeForm {
    /// The window this form describes, as far as it parses.
    pub(crate) fn window(
        &self,
        now: Timestamp,
    ) -> (Result<Timestamp, String>, Result<Timestamp, String>) {
        let start = parse_time(&self.start, now, now);
        let base = start.as_ref().map_or(now, |start| *start);
        let end = parse_time(&self.end, base, now);
        (start, end)
    }

    /// The action, or the problems.
    ///
    /// # Errors
    ///
    /// The problems by field.
    pub(crate) fn action(&self, now: Timestamp) -> Result<Action, Issues> {
        let mut issues = Issues::new();
        let comment = comment(&mut issues, &self.comment, "a downtime");
        let (start, end) = self.window(now);
        let start = time_field(&mut issues, FormField::Start, start);
        let end = time_field(&mut issues, FormField::End, end);
        if let (Some(start), Some(end)) = (start, end) {
            if end <= start {
                issues.insert(
                    FormField::End,
                    "The end must be after the start.".to_owned(),
                );
            } else if end <= now {
                issues.insert(FormField::End, "This window is over already.".to_owned());
            }
        }
        let mode = if self.flexible {
            match parse_duration(&self.duration) {
                Ok(duration) => Some(DowntimeMode::Flexible {
                    duration: duration.as_secs_f64(),
                }),
                Err(error) => {
                    issues.insert(FormField::Duration, error);
                    None
                }
            }
        } else {
            Some(DowntimeMode::Fixed)
        };
        let trigger = self.trigger.trim();
        if trigger.contains(char::is_whitespace) {
            issues.insert(
                FormField::Trigger,
                "A downtime's name has no spaces (host!service!id).".to_owned(),
            );
        }
        let (Some(start), Some(end), Some(mode)) = (start, end, mode) else {
            return Err(issues);
        };
        finish(
            Action::ScheduleDowntime {
                comment,
                start,
                end,
                mode,
                all_services: self.all_services,
                child_options: self.child_options,
                trigger_name: (!trigger.is_empty()).then(|| trigger.to_owned()),
            },
            issues,
        )
    }
}

/// The comment dialog (ACT-04).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommentForm {
    /// The comment.
    pub(crate) text: String,
    /// Whether it expires.
    pub(crate) expires: bool,
    /// When.
    pub(crate) expiry: String,
}

impl Default for CommentForm {
    fn default() -> Self {
        Self {
            text: String::new(),
            expires: false,
            expiry: "+1d".to_owned(),
        }
    }
}

impl CommentForm {
    /// The action, or the problems.
    ///
    /// # Errors
    ///
    /// The problems by field.
    pub(crate) fn action(&self, now: Timestamp) -> Result<Action, Issues> {
        let mut issues = Issues::new();
        let text = self.text.trim();
        if text.is_empty() {
            issues.insert(FormField::Comment, "Write the comment.".to_owned());
        }
        let expiry = expiry(&mut issues, self.expires, &self.expiry, now);
        finish(
            Action::AddComment {
                text: text.to_owned(),
                expiry,
            },
            issues,
        )
    }
}

/// The passive check result dialog (ACT-05).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResultForm {
    /// The plugin exit status, 0–3 (OK, WARNING, CRITICAL, UNKNOWN; for
    /// hosts 0 is UP and 2 is DOWN).
    pub(crate) exit_status: u8,
    /// The plugin output (first line, then the long output).
    pub(crate) output: String,
    /// Performance data, space separated (`load1=0.5;1;2 'disk /'=80%`).
    pub(crate) perfdata: String,
}

impl ResultForm {
    /// The action, or the problems.
    ///
    /// # Errors
    ///
    /// The problems by field.
    pub(crate) fn action(&self) -> Result<Action, Issues> {
        let mut issues = Issues::new();
        let output = self.output.trim();
        if output.is_empty() {
            issues.insert(
                FormField::Output,
                "Enter the plugin output (what the check would say).".to_owned(),
            );
        }
        let perfdata = match parse_perfdata_entries(&self.perfdata) {
            Ok(entries) => entries,
            Err(error) => {
                issues.insert(FormField::Perfdata, error);
                Vec::new()
            }
        };
        finish(
            Action::ProcessCheckResult {
                exit_status: self.exit_status.min(3),
                output: output.to_owned(),
                perfdata,
                ttl: None,
            },
            issues,
        )
    }
}

/// Splits performance data into entries (labels may be quoted with `'`
/// and contain spaces) and checks each.
///
/// # Errors
///
/// Which entry isn't performance data.
pub(crate) fn parse_perfdata_entries(text: &str) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let end = entry_end(rest);
        let (entry, after) = rest.split_at(end);
        if ic_model::parse_perfdata_entry(entry).is_none() {
            return Err(format!(
                "“{entry}” isn't performance data: label=value[unit];warn;crit;min;max"
            ));
        }
        entries.push(entry.to_owned());
        rest = after.trim_start();
    }
    Ok(entries)
}

/// Where the perfdata entry at the start of `text` ends: at whitespace
/// outside a quoted label.
fn entry_end(text: &str) -> usize {
    let mut quoted = false;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match c {
            '\'' if quoted && chars.peek().is_some_and(|&(_, next)| next == '\'') => {
                chars.next();
            }
            '\'' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => return index,
            _ => {}
        }
    }
    text.len()
}

/// The run command dialog (ACT-06).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandForm {
    /// The object's check or event command.
    pub(crate) command_type: CommandType,
    /// Another command by name (blank: the object's own).
    pub(crate) command: String,
    /// Where to run it (blank: the object's command endpoint, else the
    /// endpoint the client talks to).
    pub(crate) endpoint: String,
    /// Macro overrides, one `name = value` per line.
    pub(crate) macros: String,
    /// How long Icinga keeps the result.
    pub(crate) ttl: String,
}

impl Default for CommandForm {
    fn default() -> Self {
        Self {
            command_type: CommandType::EventCommand,
            command: String::new(),
            endpoint: String::new(),
            macros: String::new(),
            ttl: "5m".to_owned(),
        }
    }
}

impl CommandForm {
    /// The action, or the problems. `endpoints` are the endpoints Icinga
    /// reported (empty when unknown: then any name is taken).
    ///
    /// # Errors
    ///
    /// The problems by field.
    pub(crate) fn action(&self, endpoints: &[String]) -> Result<Action, Issues> {
        let mut issues = Issues::new();
        let command = self.command.trim();
        if command.contains(char::is_whitespace) {
            issues.insert(
                FormField::Command,
                "A command's name has no spaces (the name of a CheckCommand or EventCommand)."
                    .to_owned(),
            );
        }
        let endpoint = self.endpoint.trim();
        if !endpoint.is_empty()
            && !endpoints.is_empty()
            && !endpoints.iter().any(|known| known == endpoint)
        {
            issues.insert(
                FormField::Endpoint,
                format!(
                    "Icinga has no endpoint “{endpoint}” (it knows {}).",
                    known_list(endpoints)
                ),
            );
        }
        let macros = match parse_macros(&self.macros) {
            Ok(macros) => macros,
            Err(error) => {
                issues.insert(FormField::Macros, error);
                Vars::new()
            }
        };
        let ttl = match parse_duration(&self.ttl) {
            Ok(ttl) => ttl,
            Err(error) => {
                issues.insert(FormField::Ttl, error);
                Duration::ZERO
            }
        };
        finish(
            Action::ExecuteCommand {
                command_type: self.command_type,
                command: (!command.is_empty()).then(|| command.to_owned()),
                endpoint: (!endpoint.is_empty()).then(|| endpoint.to_owned()),
                macros,
                ttl: ttl.as_secs_f64(),
            },
            issues,
        )
    }
}

/// At most five endpoint names, for messages.
fn known_list(endpoints: &[String]) -> String {
    let mut names: Vec<&str> = endpoints.iter().map(String::as_str).take(5).collect();
    if endpoints.len() > names.len() {
        names.push("…");
    }
    names.join(", ")
}

/// Macro overrides, one `name = value` (or `name: value`) per line; blank
/// lines and `#` comments are skipped. Values that read as JSON (numbers,
/// `true`, `"quoted"`, arrays) keep their type, the rest are strings.
///
/// # Errors
///
/// Which line is wrong.
pub(crate) fn parse_macros(text: &str) -> Result<Vars, String> {
    let mut macros = Vars::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = number + 1;
        let Some((name, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            return Err(format!("Line {number}: write name = value."));
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(format!(
                "Line {number}: “{name}” isn't a macro name (letters, digits, _ . -)."
            ));
        }
        if macros.contains_key(name) {
            return Err(format!("Line {number}: {name} is set twice."));
        }
        let value = value.trim();
        let parsed = serde_json::from_str::<Value>(value)
            .ok()
            .filter(|value| !value.is_object())
            .unwrap_or_else(|| Value::String(value.to_owned()));
        macros.insert(name.to_owned(), parsed);
    }
    Ok(macros)
}

/// An object an action skips, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skipped {
    /// The object.
    pub(crate) object: ObjectKey,
    /// Why (`is OK`, `is acknowledged already`).
    pub(crate) reason: &'static str,
}

/// The objects an action applies to, and those it skips.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Eligible {
    /// The objects to send it for, in the order given.
    pub(crate) targets: Vec<ObjectKey>,
    /// The others.
    pub(crate) skipped: Vec<Skipped>,
}

/// Which of `objects` `action` applies to, by the snapshot: problems that
/// aren't acknowledged for acknowledgements, acknowledged objects for
/// removing acknowledgements, objects with downtimes for removing them.
/// Other actions apply to every object the snapshot knows.
pub(crate) fn eligible(
    action: &ObjectAction,
    snapshot: &Snapshot,
    objects: &[ObjectKey],
) -> Eligible {
    let mut result = Eligible::default();
    let mut seen = std::collections::HashSet::new();
    for object in objects.iter().filter(|object| seen.insert(*object)) {
        match skip_reason(action, snapshot, object) {
            Some(reason) => result.skipped.push(Skipped {
                object: object.clone(),
                reason,
            }),
            None => result.targets.push(object.clone()),
        }
    }
    result
}

fn skip_reason(
    action: &ObjectAction,
    snapshot: &Snapshot,
    object: &ObjectKey,
) -> Option<&'static str> {
    let judged = matches!(
        action,
        ObjectAction::Acknowledge
            | ObjectAction::RemoveAcknowledgement
            | ObjectAction::RemoveDowntimes
    );
    if !judged {
        // The others apply whatever the state; an object gone meanwhile
        // gets Icinga's own answer.
        return None;
    }
    let facts = match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| {
            (
                host.is_problem(),
                host.check.acknowledgement.is_acknowledged(),
                if host.state == ic_model::HostState::Pending {
                    "is pending"
                } else {
                    "is UP"
                },
            )
        }),
        ObjectKey::Service { key } => snapshot.services.get(key).map(|service| {
            (
                service.is_problem(),
                service.check.acknowledgement.is_acknowledged(),
                if service.state == ic_model::ServiceState::Pending {
                    "is pending"
                } else {
                    "is OK"
                },
            )
        }),
    };
    let Some((problem, acknowledged, ok_word)) = facts else {
        return Some("is gone from Icinga");
    };
    match action {
        ObjectAction::Acknowledge if !problem => Some(ok_word),
        ObjectAction::Acknowledge if acknowledged => Some("is acknowledged already"),
        ObjectAction::RemoveAcknowledgement if !acknowledged => Some("isn't acknowledged"),
        ObjectAction::RemoveDowntimes
            if snapshot.downtimes.get(object).is_none_or(Vec::is_empty) =>
        {
            Some("has no downtime")
        }
        _ => None,
    }
}

/// Skips objects the snapshot doesn't know (removed meanwhile).
pub(crate) fn known(snapshot: &Snapshot, objects: &[ObjectKey]) -> Eligible {
    let mut result = Eligible::default();
    let mut seen = std::collections::HashSet::new();
    for object in objects.iter().filter(|object| seen.insert(*object)) {
        let present = match object {
            ObjectKey::Host { name } => snapshot.hosts.contains_key(name),
            ObjectKey::Service { key } => snapshot.services.contains_key(key),
        };
        if present {
            result.targets.push(object.clone());
        } else {
            result.skipped.push(Skipped {
                object: object.clone(),
                reason: "is gone from Icinga",
            });
        }
    }
    result
}

impl Eligible {
    /// `2 OK, 1 acknowledged already`: the skipped objects counted by
    /// reason, for the dialog and messages.
    pub(crate) fn skipped_summary(&self) -> String {
        let mut counts: Vec<(&'static str, usize)> = Vec::new();
        for skipped in &self.skipped {
            match counts
                .iter_mut()
                .find(|(reason, _)| *reason == skipped.reason)
            {
                Some((_, count)) => *count += 1,
                None => counts.push((skipped.reason, 1)),
            }
        }
        counts
            .into_iter()
            .map(|(reason, count)| {
                let reason = reason
                    .trim_start_matches("is ")
                    .trim_start_matches("isn't ")
                    .trim_start_matches("has ");
                match count {
                    1 => format!("1 {reason}"),
                    _ => format!("{count} {reason}"),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How many downtimes the schedule-downtime dialog offers as triggers.
pub(crate) const TRIGGER_CHOICES: usize = 6;
/// How much of a downtime's comment a trigger choice shows.
const TRIGGER_COMMENT_CHARS: usize = 32;

/// A downtime that could trigger the one being scheduled (ACT-03).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TriggerChoice {
    /// Its full name (`host!service!id`), sent as the trigger.
    pub(crate) name: String,
    /// What the dialog shows: `core-sw-01 · firmware · alice · 14:00 →
    /// 16:00`.
    pub(crate) label: String,
}

/// The downtimes that could trigger a downtime of `targets`: those of the
/// targets, of their hosts and of those hosts' parents (dependencies),
/// that haven't ended; in effect first, then by start; at most
/// [`TRIGGER_CHOICES`]. Their names appear nowhere else in the UI.
pub(crate) fn trigger_choices(
    snapshot: &Snapshot,
    targets: &[ObjectKey],
    now: Timestamp,
) -> Vec<TriggerChoice> {
    let mut objects: Vec<ObjectKey> = Vec::new();
    let mut add = |object: ObjectKey| {
        if !objects.contains(&object) {
            objects.push(object);
        }
    };
    for target in targets {
        add(target.clone());
        let host = ObjectKey::Host {
            name: target.host_name().clone(),
        };
        for dependency in snapshot.dependencies.iter() {
            if dependency.child == host && matches!(dependency.parent, ObjectKey::Host { .. }) {
                add(dependency.parent.clone());
            }
        }
        add(host);
    }
    let mut downtimes: Vec<&ic_model::Downtime> = objects
        .iter()
        .filter_map(|object| snapshot.downtimes.get(object))
        .flatten()
        .filter(|downtime| downtime.end_time > now)
        .collect();
    downtimes.sort_by(|a, b| {
        b.in_effect.cmp(&a.in_effect).then(
            a.start_time
                .as_unix_seconds()
                .total_cmp(&b.start_time.as_unix_seconds()),
        )
    });
    downtimes
        .into_iter()
        .take(TRIGGER_CHOICES)
        .map(|downtime| {
            let mut parts = vec![describe_objects(std::slice::from_ref(&downtime.object))];
            let comment = downtime.comment.lines().next().unwrap_or_default().trim();
            if !comment.is_empty() {
                parts.push(if comment.chars().count() > TRIGGER_COMMENT_CHARS {
                    let cut: String = comment.chars().take(TRIGGER_COMMENT_CHARS - 1).collect();
                    format!("{}…", cut.trim_end())
                } else {
                    comment.to_owned()
                });
            }
            if !downtime.author.trim().is_empty() {
                parts.push(downtime.author.trim().to_owned());
            }
            parts.push(format!(
                "{} → {}",
                crate::format::clock(downtime.start_time, now),
                crate::format::clock(downtime.end_time, now)
            ));
            TriggerChoice {
                name: downtime.name.clone(),
                label: parts.join(" · "),
            }
        })
        .collect()
}

/// `postgres-replication on db-prod-03`, `db-prod-03`, `3 services`,
/// `2 hosts`, `5 objects`: what an action applies to, in messages.
pub(crate) fn describe_objects(objects: &[ObjectKey]) -> String {
    match objects {
        [] => "nothing".to_owned(),
        [ObjectKey::Host { name }] => name.to_string(),
        [ObjectKey::Service { key }] => format!("{} on {}", key.name, key.host),
        many => {
            let hosts = many
                .iter()
                .filter(|key| matches!(key, ObjectKey::Host { .. }))
                .count();
            let noun = match (hosts, many.len() - hosts) {
                (_, 0) => "hosts",
                (0, _) => "services",
                _ => "objects",
            };
            format!("{} {noun}", many.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_model::{AckKind, Downtime, Host, HostState, Service, ServiceState};

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_296_400.)
    }

    fn later(hours: f64) -> Timestamp {
        Timestamp::from_unix_seconds(now().as_unix_seconds() + hours * 3_600.)
    }

    #[test]
    fn acknowledgements_need_a_comment_and_never_notify() {
        let form = AckForm::default();
        let issues = form.action(now()).unwrap_err();
        assert!(issues[&FormField::Comment].contains("needs a comment"));
        let form = AckForm {
            comment: "  looking into it ".to_owned(),
            sticky: true,
            ..AckForm::default()
        };
        assert_eq!(
            form.action(now()),
            Ok(Action::Acknowledge {
                comment: "looking into it".to_owned(),
                sticky: true,
                persistent: false,
                expiry: None,
            })
        );
    }

    #[test]
    fn expiries_are_parsed_and_must_be_ahead() {
        let mut form = AckForm {
            comment: "c".to_owned(),
            expires: true,
            expiry: "+2h".to_owned(),
            ..AckForm::default()
        };
        let Ok(Action::Acknowledge { expiry, .. }) = form.action(now()) else {
            panic!("valid");
        };
        assert_eq!(expiry, Some(later(2.)));
        form.expiry = "now".to_owned();
        assert_eq!(
            form.action(now()).unwrap_err()[&FormField::Expiry],
            "This is in the past."
        );
        form.expiry = "whenever".to_owned();
        assert!(form.action(now()).unwrap_err()[&FormField::Expiry].contains("isn't a time"));
        form.expires = false;
        assert!(
            form.action(now()).is_ok(),
            "an expiry not wanted isn't checked"
        );
    }

    #[test]
    fn downtimes_check_their_window() {
        let mut form = DowntimeForm {
            comment: "maintenance".to_owned(),
            ..DowntimeForm::default()
        };
        assert_eq!(
            form.action(now()),
            Ok(Action::ScheduleDowntime {
                comment: "maintenance".to_owned(),
                start: now(),
                end: later(1.),
                mode: DowntimeMode::Fixed,
                all_services: true,
                child_options: ChildOptions::None,
                trigger_name: None,
            })
        );
        // The end counts from the start.
        form.start = "+1h".to_owned();
        form.end = "+2h".to_owned();
        let Ok(Action::ScheduleDowntime { start, end, .. }) = form.action(now()) else {
            panic!("valid");
        };
        assert_eq!((start, end), (later(1.), later(3.)));
        form.end = "now".to_owned();
        assert_eq!(
            form.action(now()).unwrap_err()[&FormField::End],
            "The end must be after the start."
        );
        form.start = "-".to_owned();
        let issues = form.action(now()).unwrap_err();
        assert!(issues.contains_key(&FormField::Start));
        // A window entirely in the past.
        form.start = "2020-01-01 10:00".to_owned();
        form.end = "2020-01-01 12:00".to_owned();
        assert_eq!(
            form.action(now()).unwrap_err()[&FormField::End],
            "This window is over already."
        );
    }

    #[test]
    fn flexible_downtimes_need_a_duration() {
        let mut form = DowntimeForm {
            comment: "c".to_owned(),
            flexible: true,
            duration: "90m".to_owned(),
            child_options: ChildOptions::Triggered,
            trigger: " db-prod-03!dt-1 ".to_owned(),
            ..DowntimeForm::default()
        };
        let Ok(Action::ScheduleDowntime {
            mode,
            child_options,
            trigger_name,
            ..
        }) = form.action(now())
        else {
            panic!("valid");
        };
        assert_eq!(mode, DowntimeMode::Flexible { duration: 5_400. });
        assert_eq!(child_options, ChildOptions::Triggered);
        assert_eq!(trigger_name.as_deref(), Some("db-prod-03!dt-1"));
        form.duration = "90".to_owned();
        assert!(form.action(now()).unwrap_err()[&FormField::Duration].contains("unit"));
        form.duration = "1h".to_owned();
        form.trigger = "two words".to_owned();
        assert!(
            form.action(now())
                .unwrap_err()
                .contains_key(&FormField::Trigger)
        );
    }

    #[test]
    fn comments_and_their_expiry() {
        let mut form = CommentForm::default();
        assert!(
            form.action(now())
                .unwrap_err()
                .contains_key(&FormField::Comment)
        );
        form.text = "replaced the disk".to_owned();
        form.expires = true;
        assert_eq!(
            form.action(now()),
            Ok(Action::AddComment {
                text: "replaced the disk".to_owned(),
                expiry: Some(later(24.)),
            })
        );
    }

    #[test]
    fn passive_results_need_output_and_valid_perfdata() {
        let mut form = ResultForm::default();
        assert!(form.action().unwrap_err().contains_key(&FormField::Output));
        form.output = "OK - fixed by hand\nsecond line".to_owned();
        form.exit_status = 2;
        form.perfdata = "load1=0.5;1;2 'disk /'=80%;90;95  rta=1ms".to_owned();
        assert_eq!(
            form.action(),
            Ok(Action::ProcessCheckResult {
                exit_status: 2,
                output: "OK - fixed by hand\nsecond line".to_owned(),
                perfdata: vec![
                    "load1=0.5;1;2".to_owned(),
                    "'disk /'=80%;90;95".to_owned(),
                    "rta=1ms".to_owned(),
                ],
                ttl: None,
            })
        );
        form.perfdata = "load1=0.5 oops".to_owned();
        let error = form.action().unwrap_err()[&FormField::Perfdata].clone();
        assert!(error.contains("“oops”"), "{error}");
        form.perfdata = "'it''s'=1".to_owned();
        assert!(form.action().is_ok(), "doubled quotes inside a label");
        form.exit_status = 9;
        let Ok(Action::ProcessCheckResult { exit_status, .. }) = form.action() else {
            panic!("valid");
        };
        assert_eq!(exit_status, 3, "clamped to UNKNOWN");
    }

    #[test]
    fn commands_parse_macros_and_check_the_endpoint() {
        let endpoints = vec!["master-01".to_owned(), "sat-eu-01".to_owned()];
        let form = CommandForm {
            command_type: CommandType::CheckCommand,
            command: " ".to_owned(),
            endpoint: "sat-eu-01".to_owned(),
            macros: "# overrides\nping_wrta = 50\nby_ssh_address: 10.0.0.5\nquiet = true\n\nlist = [1, 2]"
                .to_owned(),
            ttl: "10m".to_owned(),
        };
        let Ok(Action::ExecuteCommand {
            command_type,
            command,
            endpoint,
            macros,
            ttl,
        }) = form.action(&endpoints)
        else {
            panic!("valid");
        };
        assert_eq!(command_type, CommandType::CheckCommand);
        assert_eq!(command, None, "blank: the object's own");
        assert_eq!(endpoint.as_deref(), Some("sat-eu-01"));
        assert_eq!(macros["ping_wrta"], serde_json::json!(50));
        assert_eq!(macros["by_ssh_address"], serde_json::json!("10.0.0.5"));
        assert_eq!(macros["quiet"], serde_json::json!(true));
        assert_eq!(macros["list"], serde_json::json!([1, 2]));
        assert!((ttl - 600.).abs() < f64::EPSILON);

        let wrong = CommandForm {
            endpoint: "nowhere".to_owned(),
            macros: "just text".to_owned(),
            ttl: "0m".to_owned(),
            command: "two words".to_owned(),
            ..CommandForm::default()
        };
        let issues = wrong.action(&endpoints).unwrap_err();
        assert!(issues[&FormField::Endpoint].contains("master-01, sat-eu-01"));
        assert!(issues[&FormField::Macros].contains("Line 1"));
        assert!(issues[&FormField::Ttl].contains("longer than zero"));
        assert!(issues.contains_key(&FormField::Command));
        // Unknown endpoints are taken when Icinga listed none.
        let anywhere = CommandForm {
            endpoint: "nowhere".to_owned(),
            ..CommandForm::default()
        };
        assert!(anywhere.action(&[]).is_ok());
        assert!(parse_macros("a = 1\na = 2").unwrap_err().contains("twice"));
        assert!(parse_macros("bad name = 1").is_err());
        assert_eq!(
            parse_macros("x = {\"a\": 1}").unwrap()["x"],
            serde_json::json!("{\"a\": 1}"),
            "dictionaries stay text"
        );
    }

    fn snapshot() -> Snapshot {
        let mut ok = Service::new("db-01", "disk");
        ok.state = ServiceState::Ok;
        let mut critical = Service::new("db-01", "load");
        critical.state = ServiceState::Critical;
        let mut acked = Service::new("db-01", "swap");
        acked.state = ServiceState::Warning;
        acked.check.acknowledgement = AckKind::Normal;
        let mut down = Host::new("db-01");
        down.state = HostState::Down;
        let services = [ok, critical, acked]
            .into_iter()
            .map(|service| (service.key.clone(), Arc::new(service)))
            .collect();
        let mut downtimes = BTreeMap::new();
        downtimes.insert(
            ObjectKey::host("db-01"),
            vec![Downtime {
                name: "db-01!dt".to_owned(),
                object: ObjectKey::host("db-01"),
                author: String::new(),
                comment: String::new(),
                start_time: now(),
                end_time: later(1.),
                fixed: true,
                duration: 0.,
                entry_time: now(),
                trigger_time: None,
                triggered_by: None,
                parent: None,
                in_effect: true,
                config_owned: false,
            }],
        );
        Snapshot {
            hosts: Arc::new([(down.name.clone(), Arc::new(down))].into_iter().collect()),
            services: Arc::new(services),
            downtimes: Arc::new(downtimes),
            ..Snapshot::default()
        }
    }

    #[test]
    fn only_unacknowledged_problems_are_acknowledged() {
        let snapshot = snapshot();
        let objects = [
            ObjectKey::service("db-01", "disk"),
            ObjectKey::service("db-01", "load"),
            ObjectKey::service("db-01", "swap"),
            ObjectKey::host("db-01"),
            ObjectKey::host("gone"),
            ObjectKey::service("db-01", "load"),
        ];
        let ack = eligible(&ObjectAction::Acknowledge, &snapshot, &objects);
        assert_eq!(
            ack.targets,
            [
                ObjectKey::service("db-01", "load"),
                ObjectKey::host("db-01")
            ],
            "once each, in order"
        );
        assert_eq!(ack.skipped.len(), 3);
        assert_eq!(
            ack.skipped_summary(),
            "1 OK, 1 acknowledged already, 1 gone from Icinga"
        );

        let remove = eligible(&ObjectAction::RemoveAcknowledgement, &snapshot, &objects);
        assert_eq!(remove.targets, [ObjectKey::service("db-01", "swap")]);
        let downtimes = eligible(&ObjectAction::RemoveDowntimes, &snapshot, &objects);
        assert_eq!(downtimes.targets, [ObjectKey::host("db-01")]);
        assert_eq!(
            downtimes.skipped_summary(),
            "3 no downtime, 1 gone from Icinga"
        );
        let check = eligible(&ObjectAction::CheckNow, &snapshot, &objects);
        assert_eq!(check.targets.len(), 5, "unknown objects are left to Icinga");

        let present = known(&snapshot, &objects);
        assert_eq!(present.targets.len(), 4);
        assert_eq!(present.skipped_summary(), "1 gone from Icinga");
    }

    fn downtime(object: ObjectKey, id: &str, start: f64, end: f64) -> Downtime {
        Downtime {
            name: format!("{object}!{id}"),
            object,
            author: "alice".to_owned(),
            comment: "firmware update on the core switches, then a reboot".to_owned(),
            start_time: later(start),
            end_time: later(end),
            fixed: true,
            duration: 0.,
            entry_time: now(),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: start <= 0. && end > 0.,
            config_owned: false,
        }
    }

    #[test]
    fn triggers_are_offered_from_the_objects_their_hosts_and_parents() {
        let mut snapshot = snapshot();
        let switch = ObjectKey::host("core-sw-01");
        let mut downtimes = (*snapshot.downtimes).clone();
        downtimes.insert(
            switch.clone(),
            vec![
                downtime(switch.clone(), "later", 2., 3.),
                downtime(switch.clone(), "now", -1., 1.),
                downtime(switch.clone(), "over", -3., -1.),
            ],
        );
        downtimes.insert(
            ObjectKey::host("elsewhere"),
            vec![downtime(ObjectKey::host("elsewhere"), "x", -1., 1.)],
        );
        snapshot.downtimes = Arc::new(downtimes);
        snapshot.dependencies = Arc::new(vec![ic_model::Dependency {
            name: "db-01!uplink".to_owned(),
            child: ObjectKey::host("db-01"),
            parent: switch.clone(),
        }]);
        let choices = trigger_choices(&snapshot, &[ObjectKey::service("db-01", "disk")], now());
        let names: Vec<&str> = choices.iter().map(|choice| choice.name.as_str()).collect();
        assert_eq!(
            names,
            ["core-sw-01!now", "db-01!dt", "core-sw-01!later"],
            "in effect first, the parent's too; ended and unrelated ones not"
        );
        assert!(
            choices[0]
                .label
                .starts_with("core-sw-01 · firmware update on the core swi… · alice · "),
            "{}",
            choices[0].label
        );
        assert!(choices[0].label.contains(" → "));
        assert_eq!(
            choices[1].label.matches(" · ").count(),
            1,
            "no comment, no author"
        );
        assert!(trigger_choices(&snapshot, &[ObjectKey::host("lonely")], now()).is_empty());
    }

    #[test]
    fn objects_are_described_by_kind() {
        assert_eq!(describe_objects(&[]), "nothing");
        assert_eq!(describe_objects(&[ObjectKey::host("a")]), "a");
        assert_eq!(
            describe_objects(&[ObjectKey::service("a", "disk")]),
            "disk on a"
        );
        assert_eq!(
            describe_objects(&[ObjectKey::host("a"), ObjectKey::host("b")]),
            "2 hosts"
        );
        assert_eq!(
            describe_objects(&[ObjectKey::service("a", "x"), ObjectKey::service("b", "y")]),
            "2 services"
        );
        assert_eq!(
            describe_objects(&[ObjectKey::host("a"), ObjectKey::service("b", "y")]),
            "2 objects"
        );
    }
}
