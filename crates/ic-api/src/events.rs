//! The `/v1/events` stream: newline-delimited JSON over a long-lived
//! HTTP/1.1 response, parsed incrementally and mapped per
//! `icinga2-apievents.cpp`.

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use futures::stream::BoxStream;
use ic_model::{AckKind, CheckableState, Event, ObjectChange, ObjectKey, StateType, Timestamp};
use serde::Deserialize;
use serde_json::Value;

use crate::error::ApiError;
use crate::lenient::{FromJson, L};
use crate::wire::{
    CommentAttrs, DowntimeAttrs, WireCheckResult, ack_kind, clamp_u32, host_state, object_key,
    service_state, state_type,
};

/// Lines longer than this are dropped (Icinga's events are a few KiB; a
/// huge line means something is wrong, and buffering it unbounded would
/// let a broken server exhaust memory).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Splits a byte stream into lines, across chunk boundaries.
#[derive(Debug, Default)]
pub(crate) struct LineBuffer {
    buffer: Vec<u8>,
    /// Inside an over-long line: drop bytes until its newline.
    discarding: bool,
}

impl LineBuffer {
    /// Appends a chunk and returns the lines it completed (without the
    /// newline; `\r` and surrounding whitespace trimmed; empty lines
    /// skipped).
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        let mut rest = chunk;
        while let Some(newline) = rest.iter().position(|&byte| byte == b'\n') {
            let (head, tail) = rest.split_at(newline);
            rest = &tail[1..];
            if self.discarding {
                self.discarding = false;
                self.buffer.clear();
                continue;
            }
            self.buffer.extend_from_slice(head);
            let line = std::mem::take(&mut self.buffer);
            if let Some(line) = trimmed(&line) {
                lines.push(line);
            }
        }
        if !self.discarding {
            self.buffer.extend_from_slice(rest);
            if self.buffer.len() > MAX_LINE_BYTES {
                tracing::warn!(
                    bytes = self.buffer.len(),
                    "dropping an over-long event stream line"
                );
                self.buffer = Vec::new();
                self.discarding = true;
            }
        }
        lines
    }

    /// The unterminated rest at the end of the stream, if any.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let rest = std::mem::take(&mut self.buffer);
        if std::mem::take(&mut self.discarding) {
            return None;
        }
        trimmed(&rest)
    }
}

fn trimmed(line: &[u8]) -> Option<Vec<u8>> {
    let start = line.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let end = line.iter().rposition(|byte| !byte.is_ascii_whitespace())?;
    Some(line[start..=end].to_vec())
}

/// Parses one line of the stream. Malformed lines and unknown or
/// unusable events are logged and skipped (`None`).
pub(crate) fn parse_line(line: &[u8]) -> Option<Event> {
    let wire: WireEvent = match serde_json::from_slice(line) {
        Ok(wire) => wire,
        Err(error) => {
            tracing::warn!(%error, "skipping a malformed event stream line");
            return None;
        }
    };
    let kind = wire.kind.0.clone();
    let event = wire.into_event();
    if event.is_none() {
        tracing::warn!(kind = %kind, "skipping an unknown or incomplete event");
    }
    event
}

/// Every field any event type carries. `comment` is a string in
/// `AcknowledgementSet` and a dictionary in `CommentAdded`/`Removed`, so it
/// stays raw.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: L<String>,
    timestamp: L<f64>,
    host: L<String>,
    service: L<String>,
    check_result: L<Option<WireCheckResult>>,
    state: L<Option<f64>>,
    state_type: L<Option<f64>>,
    downtime_depth: L<Option<f64>>,
    acknowledgement: L<Option<Value>>,
    acknowledgement_type: L<Option<f64>>,
    author: L<String>,
    comment: L<Option<Value>>,
    expiry: L<f64>,
    downtime: L<Option<Value>>,
    is_flapping: L<bool>,
    flapping_current: L<Option<f64>>,
    /// The docs call it `current_flapping`; the source sends
    /// `flapping_current`. Accept both.
    current_flapping: L<Option<f64>>,
    object_type: L<String>,
    object_name: L<String>,
}

impl WireEvent {
    fn into_event(mut self) -> Option<Event> {
        let at = Timestamp::from_unix_seconds(self.timestamp.0);
        let event = match self.kind.0.as_str() {
            "CheckResult" => Event::CheckResult {
                object: self.object()?,
                downtime_depth: self.downtime_depth.0.map(clamp_u32),
                acknowledgement: self.acknowledgement(),
                result: self.check_result.0.take()?.into_model(),
                at,
            },
            "StateChange" => {
                let object = self.object()?;
                let check_result = self.check_result.0.take()?;
                let state = match object {
                    ObjectKey::Host { .. } => CheckableState::Host(host_state(
                        self.state.0,
                        check_result.reachable_after().unwrap_or(true),
                    )),
                    ObjectKey::Service { .. } => {
                        CheckableState::Service(service_state(self.state.0))
                    }
                };
                Event::StateChange {
                    state_type: state_type(self.state_type.0)
                        .or_else(|| check_result.state_type_after())
                        .unwrap_or(StateType::Hard),
                    downtime_depth: self.downtime_depth.0.map(clamp_u32),
                    acknowledgement: self.acknowledgement(),
                    result: check_result.into_model(),
                    object,
                    state,
                    at,
                }
            }
            "AcknowledgementSet" => Event::AcknowledgementSet {
                object: self.object()?,
                author: std::mem::take(&mut self.author.0),
                comment: self
                    .comment
                    .0
                    .take()
                    .map(String::from_json)
                    .unwrap_or_default(),
                // It's set, so a missing or zero type still means normal.
                kind: match self.acknowledgement_type.0.map(ack_kind) {
                    Some(AckKind::Sticky) => AckKind::Sticky,
                    _ => AckKind::Normal,
                },
                expiry: Timestamp::from_unix_seconds(self.expiry.0).non_zero(),
                at,
            },
            "AcknowledgementCleared" => Event::AcknowledgementCleared {
                object: self.object()?,
                at,
            },
            "CommentAdded" => Event::CommentAdded {
                comment: self.comment_payload()?,
                at,
            },
            "CommentRemoved" => Event::CommentRemoved {
                comment: self.comment_payload()?,
                at,
            },
            "DowntimeAdded" => Event::DowntimeAdded {
                downtime: self.downtime_payload(at)?,
                at,
            },
            // `in_effect` here says whether the downtime was in effect until
            // it ended (cancelled inside its window, or ran out), not at the
            // event: Icinga removes an expired downtime just after its end.
            "DowntimeRemoved" => Event::DowntimeRemoved {
                downtime: self.removed_downtime_payload(at)?,
                at,
            },
            "DowntimeStarted" => Event::DowntimeStarted {
                downtime: self.downtime_payload(at)?,
                at,
            },
            "DowntimeTriggered" => Event::DowntimeTriggered {
                downtime: self.downtime_payload(at)?,
                at,
            },
            "Flapping" => Event::Flapping {
                object: self.object()?,
                flapping: self.is_flapping.0,
                current: self
                    .flapping_current
                    .0
                    .or(self.current_flapping.0)
                    .unwrap_or(0.0),
                at,
            },
            "ObjectCreated" => self.lifecycle(ObjectChange::Created, at)?,
            "ObjectModified" => self.lifecycle(ObjectChange::Modified, at)?,
            "ObjectDeleted" => self.lifecycle(ObjectChange::Deleted, at)?,
            // `Notification` (Icinga's own notifications; the client doesn't
            // subscribe to it) and anything newer.
            _ => return None,
        };
        Some(event)
    }

    fn object(&self) -> Option<ObjectKey> {
        object_key(&self.host.0, &self.service.0)
    }

    /// Events carry `acknowledgement` as a boolean (`IsAcknowledged`), so
    /// sticky vs normal isn't known; `true` maps to normal. Numbers (the
    /// objects' 0/1/2 form) are honoured.
    fn acknowledgement(&self) -> Option<AckKind> {
        match self.acknowledgement.0.as_ref()? {
            Value::Bool(true) => Some(AckKind::Normal),
            Value::Bool(false) => Some(AckKind::None),
            other => crate::lenient::number(other).map(ack_kind),
        }
    }

    fn comment_payload(&mut self) -> Option<ic_model::Comment> {
        let attrs: CommentAttrs = serde_json::from_value(self.comment.0.take()?).ok()?;
        attrs.into_model("")
    }

    fn downtime_payload(&mut self, at: Timestamp) -> Option<ic_model::Downtime> {
        let attrs: DowntimeAttrs = serde_json::from_value(self.downtime.0.take()?).ok()?;
        attrs.into_model("", at)
    }

    fn removed_downtime_payload(&mut self, at: Timestamp) -> Option<ic_model::Downtime> {
        let attrs: DowntimeAttrs = serde_json::from_value(self.downtime.0.take()?).ok()?;
        attrs.into_removed_model("", at)
    }

    fn lifecycle(&mut self, change: ObjectChange, at: Timestamp) -> Option<Event> {
        if self.object_type.0.is_empty() || self.object_name.0.is_empty() {
            return None;
        }
        Some(Event::ObjectLifecycle {
            change,
            object_type: std::mem::take(&mut self.object_type.0),
            name: std::mem::take(&mut self.object_name.0),
            at,
        })
    }
}

/// The live event stream from [`crate::Client::events`].
///
/// Yields events in order. Malformed lines and unknown event types are
/// skipped with a warning. A transport error is yielded once, then the
/// stream ends; it also ends when the server closes the connection. There
/// is no read timeout: Icinga may stay silent for a long time.
pub struct EventStream {
    chunks: BoxStream<'static, Result<Bytes, ApiError>>,
    lines: LineBuffer,
    pending: VecDeque<Event>,
    done: bool,
}

impl fmt::Debug for EventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream")
            .field("pending", &self.pending.len())
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl EventStream {
    pub(crate) fn new(chunks: BoxStream<'static, Result<Bytes, ApiError>>) -> Self {
        Self {
            chunks,
            lines: LineBuffer::default(),
            pending: VecDeque::new(),
            done: false,
        }
    }

    fn take_lines(&mut self, lines: &[Vec<u8>]) {
        self.pending
            .extend(lines.iter().filter_map(|line| parse_line(line)));
    }
}

impl Stream for EventStream {
    type Item = Result<Event, ApiError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match this.chunks.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    let lines = this.lines.push(&chunk);
                    this.take_lines(&lines);
                }
                Poll::Ready(Some(Err(error))) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => {
                    this.done = true;
                    if let Some(last) = this.lines.finish() {
                        this.take_lines(&[last]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use ic_model::{CommentKind, HostState, ServiceState};

    use super::*;

    #[test]
    fn lines_span_chunks() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(b"{\"a\":").is_empty());
        assert!(buffer.push(b"1}").is_empty());
        assert_eq!(
            buffer.push(b"\n{\"b\":2}\r\n\n  \n{\"c\""),
            vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]
        );
        assert_eq!(buffer.finish(), Some(b"{\"c\"".to_vec()));
        assert_eq!(buffer.finish(), None);
    }

    #[test]
    fn over_long_lines_are_dropped_until_their_newline() {
        let mut buffer = LineBuffer::default();
        let big = vec![b'x'; MAX_LINE_BYTES + 1];
        assert!(buffer.push(&big).is_empty());
        assert!(buffer.push(b"more of the same").is_empty());
        assert_eq!(
            buffer.push(b"tail\n{\"ok\":1}\n"),
            vec![b"{\"ok\":1}".to_vec()]
        );
    }

    fn one(line: &str) -> Event {
        parse_line(line.as_bytes()).unwrap_or_else(|| panic!("not parsed: {line}"))
    }

    #[test]
    fn check_result_event() {
        let event = one(
            r#"{"acknowledgement": false, "check_result": {"active": true, "check_source": "icinga-master", "command": "dummy", "execution_end": 1791203173.754809, "execution_start": 1791203173.754809, "exit_status": 1, "output": "WARNING - load average 14.2, 12.8, 11.1", "performance_data": ["load1=14.2;10;20;0"], "previous_hard_state": 99, "schedule_end": 1791203173.754815, "schedule_start": 1791203173.754265, "scheduling_source": "icinga-master", "state": 1, "ttl": 0, "type": "CheckResult", "vars_after": {"attempt": 1, "reachable": true, "state": 1, "state_type": 1}, "vars_before": null}, "downtime_depth": 0, "host": "db-prod-03", "service": "load", "timestamp": 1791203173.754955, "type": "CheckResult"}"#,
        );
        let Event::CheckResult {
            object,
            result,
            downtime_depth,
            acknowledgement,
            at,
        } = event
        else {
            panic!("wrong event: {event:?}");
        };
        assert_eq!(object, ObjectKey::service("db-prod-03", "load"));
        assert_eq!(result.output, "WARNING - load average 14.2, 12.8, 11.1");
        assert_eq!(result.exit_status, 1);
        assert_eq!(result.perfdata.len(), 1);
        assert_eq!(result.check_source, "icinga-master");
        assert_eq!(downtime_depth, Some(0));
        assert_eq!(acknowledgement, Some(AckKind::None));
        assert_eq!(at, Timestamp::from_unix_seconds(1_791_203_173.754_955));
    }

    #[test]
    fn host_check_result_without_service() {
        let event = one(
            r#"{"type":"CheckResult","timestamp":5,"host":"k8s-node-11","check_result":{"output":"PING CRITICAL","state":2,"exit_status":2},"acknowledgement":true}"#,
        );
        assert_eq!(event.object(), Some(&ObjectKey::host("k8s-node-11")));
        let Event::CheckResult {
            downtime_depth,
            acknowledgement,
            ..
        } = event
        else {
            panic!();
        };
        assert_eq!(downtime_depth, None, "older Icinga versions don't send it");
        assert_eq!(acknowledgement, Some(AckKind::Normal));
    }

    #[test]
    fn passive_state_change_takes_the_state_not_the_exit_status() {
        let event = one(
            r#"{"acknowledgement": false, "check_result": {"active": false, "check_source": "icinga-master", "command": null, "execution_end": 1791203174.695258, "execution_start": 1791203174.695258, "exit_status": 0, "output": "DISK CRITICAL - /var 97% used (1.2 GiB free)", "performance_data": ["/var=97%;80;90;0;100"], "previous_hard_state": 99, "schedule_end": 1791203174.695258, "schedule_start": 1791203174.695258, "scheduling_source": "icinga-master", "state": 2, "ttl": 0, "type": "CheckResult", "vars_after": {"attempt": 2, "reachable": true, "state": 2, "state_type": 0}, "vars_before": null}, "downtime_depth": 0, "host": "k8s-node-07", "service": "disk /var", "state": 2, "state_type": 0, "timestamp": 1791203174.695517, "type": "StateChange"}"#,
        );
        let Event::StateChange {
            object,
            state,
            state_type,
            result,
            ..
        } = event
        else {
            panic!();
        };
        assert_eq!(object, ObjectKey::service("k8s-node-07", "disk /var"));
        assert_eq!(state, CheckableState::Service(ServiceState::Critical));
        assert_eq!(state_type, StateType::Soft);
        assert_eq!(result.exit_status, 2);
        assert!(!result.active);
    }

    #[test]
    fn host_state_change_knows_unreachable_from_vars_after() {
        let line = |reachable: bool| {
            format!(
                r#"{{"type":"StateChange","timestamp":1,"host":"behind-node-11","state":1,"state_type":1,"check_result":{{"output":"down","state":2,"vars_after":{{"reachable":{reachable}}}}}}}"#
            )
        };
        let Event::StateChange { state, .. } = one(&line(false)) else {
            panic!();
        };
        assert_eq!(state, CheckableState::Host(HostState::Unreachable));
        let Event::StateChange { state, .. } = one(&line(true)) else {
            panic!();
        };
        assert_eq!(state, CheckableState::Host(HostState::Down));
    }

    #[test]
    fn acknowledgement_events() {
        let set = one(
            r#"{"acknowledgement_type": 2, "author": "icygui", "comment": "cleaning up logs", "expiry": 1791206774, "host": "k8s-node-07", "notify": false, "persistent": false, "service": "disk /var", "state": 2, "state_type": 0, "timestamp": 1791203175.848169, "type": "AcknowledgementSet"}"#,
        );
        assert_eq!(
            set,
            Event::AcknowledgementSet {
                object: ObjectKey::service("k8s-node-07", "disk /var"),
                author: "icygui".to_owned(),
                comment: "cleaning up logs".to_owned(),
                kind: AckKind::Sticky,
                expiry: Some(Timestamp::from_unix_seconds(1_791_206_774.0)),
                at: Timestamp::from_unix_seconds(1_791_203_175.848_169),
            }
        );
        let normal_no_expiry = one(
            r#"{"type":"AcknowledgementSet","timestamp":1,"host":"h","author":"a","comment":"c","acknowledgement_type":1,"expiry":0}"#,
        );
        let Event::AcknowledgementSet { kind, expiry, .. } = normal_no_expiry else {
            panic!();
        };
        assert_eq!(kind, AckKind::Normal);
        assert_eq!(expiry, None);

        let cleared = one(
            r#"{"acknowledgement_type": 0, "host": "k8s-node-07", "service": "disk /var", "state": 2, "state_type": 0, "timestamp": 1791203179.706936, "type": "AcknowledgementCleared"}"#,
        );
        assert_eq!(
            cleared,
            Event::AcknowledgementCleared {
                object: ObjectKey::service("k8s-node-07", "disk /var"),
                at: Timestamp::from_unix_seconds(1_791_203_179.706_936),
            }
        );
    }

    const COMMENT: &str = r#"{"__name": "k8s-node-07!disk /var!efa9ecb5-3329-40d7-8e31-8f7de5f69bf9", "author": "icygui", "entry_time": 1791203175.845182, "entry_type": 4, "expire_time": 1791206774, "host_name": "k8s-node-07", "legacy_id": 1, "name": "efa9ecb5-3329-40d7-8e31-8f7de5f69bf9", "package": "_api", "persistent": false, "service_name": "disk /var", "sticky": true, "templates": ["efa9ecb5-3329-40d7-8e31-8f7de5f69bf9"], "text": "cleaning up logs", "type": "Comment", "version": 1791203175.84523, "zone": ""}"#;

    #[test]
    fn comment_events() {
        for (kind, removed) in [("CommentAdded", false), ("CommentRemoved", true)] {
            let event = one(&format!(
                r#"{{"comment": {COMMENT}, "timestamp": 1791203175.847835, "type": "{kind}"}}"#
            ));
            let ((Event::CommentAdded { comment, .. }, false)
            | (Event::CommentRemoved { comment, .. }, true)) = (&event, removed)
            else {
                panic!("wrong event {event:?}");
            };
            assert_eq!(
                comment.name,
                "k8s-node-07!disk /var!efa9ecb5-3329-40d7-8e31-8f7de5f69bf9"
            );
            assert_eq!(
                comment.object,
                ObjectKey::service("k8s-node-07", "disk /var")
            );
            assert_eq!(comment.kind, CommentKind::Acknowledgement);
            assert_eq!(comment.text, "cleaning up logs");
            assert_eq!(
                comment.expire_time,
                Some(Timestamp::from_unix_seconds(1_791_206_774.0))
            );
        }
    }

    #[test]
    fn comment_name_is_composed_without_dunder_name() {
        let event = one(
            r#"{"type":"CommentAdded","timestamp":1,"comment":{"name":"abc","host_name":"h","service_name":"","author":"a","text":"t","entry_type":1}}"#,
        );
        let Event::CommentAdded { comment, .. } = event else {
            panic!();
        };
        assert_eq!(comment.name, "h!abc");
        assert_eq!(comment.object, ObjectKey::host("h"));
        assert_eq!(comment.kind, CommentKind::User);
    }

    fn downtime_line(kind: &str, trigger_time: f64, timestamp: f64) -> String {
        format!(
            r#"{{"downtime": {{"__name": "k8s-node-11!e96da238-4aa1-4be6-9fb4-b2d9c7f706ca", "author": "a.ivanova", "authoritative_zone": "", "comment": "rack maintenance", "config_owner": "", "config_owner_hash": "", "duration": 0, "end_time": 1791210374, "entry_time": 1791203175.966761, "fixed": true, "host_name": "k8s-node-11", "legacy_id": 1, "name": "e96da238-4aa1-4be6-9fb4-b2d9c7f706ca", "package": "_api", "parent": "", "remove_time": 0, "scheduled_by": "", "service_name": "", "start_time": 1791203174, "templates": ["e96da238-4aa1-4be6-9fb4-b2d9c7f706ca"], "trigger_time": {trigger_time}, "triggered_by": "", "triggers": [], "type": "Downtime", "version": 1791203175.966782, "zone": ""}}, "timestamp": {timestamp}, "type": "{kind}"}}"#
        )
    }

    #[test]
    fn downtime_events() {
        for kind in [
            "DowntimeAdded",
            "DowntimeStarted",
            "DowntimeTriggered",
            "DowntimeRemoved",
        ] {
            let event = one(&downtime_line(
                kind,
                1_791_203_175.966_761,
                1_791_203_175.969,
            ));
            let downtime = match &event {
                Event::DowntimeAdded { downtime, .. }
                | Event::DowntimeStarted { downtime, .. }
                | Event::DowntimeTriggered { downtime, .. }
                | Event::DowntimeRemoved { downtime, .. } => downtime,
                other => panic!("wrong event {other:?}"),
            };
            let expected_kind = match &event {
                Event::DowntimeAdded { .. } => "DowntimeAdded",
                Event::DowntimeStarted { .. } => "DowntimeStarted",
                Event::DowntimeTriggered { .. } => "DowntimeTriggered",
                _ => "DowntimeRemoved",
            };
            assert_eq!(expected_kind, kind);
            assert_eq!(
                downtime.name,
                "k8s-node-11!e96da238-4aa1-4be6-9fb4-b2d9c7f706ca"
            );
            assert_eq!(downtime.object, ObjectKey::host("k8s-node-11"));
            assert!(downtime.fixed);
            assert!(
                downtime.in_effect,
                "fixed and inside its window at the event"
            );
            assert!(!downtime.config_owned);
        }
        // Removed by Icinga's cleanup timer after the window ended (the
        // usual end of a maintenance window): it was in effect until then,
        // so its removal ends a downtime.
        let expired = one(&downtime_line(
            "DowntimeRemoved",
            1_791_203_175.966_761,
            1_791_210_374.1,
        ));
        let Event::DowntimeRemoved { downtime, .. } = expired else {
            panic!();
        };
        assert!(downtime.in_effect, "ran out while in effect");
        // Cancelled before its window started: it never was in effect.
        let early = one(&downtime_line("DowntimeRemoved", 0.0, 1_791_203_000.0));
        let Event::DowntimeRemoved { downtime, .. } = early else {
            panic!();
        };
        assert!(!downtime.in_effect);
        assert_eq!(downtime.trigger_time, None);
        // Other downtime events still say whether it's in effect at the
        // event.
        let late_added = one(&downtime_line("DowntimeAdded", 0.0, 1_791_210_400.0));
        let Event::DowntimeAdded { downtime, .. } = late_added else {
            panic!();
        };
        assert!(!downtime.in_effect);
    }

    #[test]
    fn recorded_downtime_removals_end_downtimes_in_effect() {
        let recorded = include_str!("../../../contract/samples/events.ndjson");
        let removed: Vec<ic_model::Downtime> = recorded
            .lines()
            .filter_map(|line| parse_line(line.as_bytes()))
            .filter_map(|event| match event {
                Event::DowntimeRemoved { downtime, .. } => Some(downtime),
                _ => None,
            })
            .collect();
        // Both were cancelled inside their window.
        assert_eq!(removed.len(), 2);
        assert!(removed.iter().all(|downtime| downtime.in_effect));
    }

    #[test]
    fn flapping_event_accepts_both_field_names() {
        let source = one(
            r#"{"type":"Flapping","timestamp":3,"host":"h","service":"s","state":2,"state_type":1,"is_flapping":true,"flapping_current":42.5,"threshold_low":25,"threshold_high":30}"#,
        );
        assert_eq!(
            source,
            Event::Flapping {
                object: ObjectKey::service("h", "s"),
                flapping: true,
                current: 42.5,
                at: Timestamp::from_unix_seconds(3.0),
            }
        );
        let docs = one(
            r#"{"type":"Flapping","timestamp":3,"host":"h","is_flapping":false,"current_flapping":12}"#,
        );
        let Event::Flapping {
            flapping, current, ..
        } = docs
        else {
            panic!();
        };
        assert!(!flapping);
        assert!((current - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn object_lifecycle_events() {
        for (kind, change) in [
            ("ObjectCreated", ObjectChange::Created),
            ("ObjectModified", ObjectChange::Modified),
            ("ObjectDeleted", ObjectChange::Deleted),
        ] {
            let event = one(&format!(
                r#"{{"object_name": "k8s-node-07!disk /var", "object_type": "Service", "timestamp": 7, "type": "{kind}"}}"#
            ));
            assert_eq!(
                event,
                Event::ObjectLifecycle {
                    change,
                    object_type: "Service".to_owned(),
                    name: "k8s-node-07!disk /var".to_owned(),
                    at: Timestamp::from_unix_seconds(7.0),
                }
            );
        }
    }

    #[test]
    fn skips_unknown_malformed_and_incomplete_events() {
        assert_eq!(parse_line(b"{not json"), None);
        assert_eq!(parse_line(b"[1,2,3]"), None);
        assert_eq!(
            parse_line(br#"{"type":"Notification","host":"h","users":["a"]}"#),
            None
        );
        assert_eq!(parse_line(br#"{"type":"SomethingNew","host":"h"}"#), None);
        assert_eq!(
            parse_line(br#"{"type":"StateChange","host":"","state":2}"#),
            None,
            "no host"
        );
        assert_eq!(
            parse_line(br#"{"type":"CheckResult","host":"h"}"#),
            None,
            "no check result"
        );
        assert_eq!(parse_line(br#"{"type":"ObjectCreated"}"#), None);
    }

    #[test]
    fn the_recorded_stream_parses_completely() {
        let recorded = include_str!("../../../contract/samples/events.ndjson");
        let lines: Vec<&str> = recorded
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let events: Vec<Event> = lines
            .iter()
            .filter_map(|line| parse_line(line.as_bytes()))
            .collect();
        assert_eq!(events.len(), lines.len(), "every recorded event maps");
    }

    #[tokio::test]
    async fn stream_reassembles_chunks_and_ends() {
        let chunks: Vec<Result<Bytes, ApiError>> = vec![
            Ok(Bytes::from_static(b"{\"type\":\"ObjectCreated\",\"timestamp\":1,")),
            Ok(Bytes::from_static(
                b"\"object_type\":\"Host\",\"object_name\":\"a\"}\ngarbage\n{\"type\":\"Nope\"}\n",
            )),
            Ok(Bytes::from_static(
                b"{\"type\":\"ObjectDeleted\",\"timestamp\":2,\"object_type\":\"Host\",\"object_name\":\"a\"}",
            )),
        ];
        let stream = EventStream::new(futures::stream::iter(chunks).boxed());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            Ok(Event::ObjectLifecycle {
                change: ObjectChange::Created,
                ..
            })
        ));
        assert!(matches!(
            events[1],
            Ok(Event::ObjectLifecycle {
                change: ObjectChange::Deleted,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_yields_a_transport_error_once_then_ends() {
        let chunks: Vec<Result<Bytes, ApiError>> = vec![
            Ok(Bytes::from_static(
                b"{\"type\":\"ObjectCreated\",\"timestamp\":1,\"object_type\":\"Host\",\"object_name\":\"a\"}\n",
            )),
            Err(ApiError::Connect("reset".to_owned())),
            Ok(Bytes::from_static(b"never read\n")),
        ];
        let stream = EventStream::new(futures::stream::iter(chunks).boxed());
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 2);
        assert!(events[0].is_ok());
        assert_eq!(events[1], Err(ApiError::Connect("reset".to_owned())));
    }
}
