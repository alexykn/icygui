//! The event pipeline: a reader task that only moves lines from the
//! connection into an unbounded channel (so Icinga's send buffer never
//! backs up, whatever the applier does), and the applier's batch
//! preparation: parsing and collapsing check results.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use ic_api::{ApiError, EventLines, parse_event};
use ic_model::{Event, ObjectKey};
use tokio::sync::mpsc::UnboundedSender;

/// What the reader hands over.
#[derive(Debug)]
pub(super) enum ReaderMsg {
    /// A raw line and its sequence number (1, 2, … per engine).
    Line(u64, Vec<u8>),
    /// The stream ended: with the transport error, or closed by Icinga.
    End(Option<ApiError>),
}

/// Reads `lines` until the stream ends, numbering each line with `seq`
/// when it is read (queries record the count when they are sent; see the
/// store's ordering notes).
pub(super) async fn read(
    mut lines: EventLines,
    seq: Arc<AtomicU64>,
    tx: UnboundedSender<ReaderMsg>,
) {
    while let Some(item) = lines.next().await {
        match item {
            Ok(line) => {
                let number = seq.fetch_add(1, Ordering::SeqCst) + 1;
                if tx.send(ReaderMsg::Line(number, line)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(ReaderMsg::End(Some(error)));
                return;
            }
        }
    }
    let _ = tx.send(ReaderMsg::End(None));
}

/// Parses a batch of lines (malformed lines and unknown types are logged
/// and skipped by the parser) and collapses it.
pub(super) fn prepare(lines: Vec<(u64, Vec<u8>)>) -> Vec<(u64, Event)> {
    collapse(
        lines
            .into_iter()
            .filter_map(|(seq, line)| parse_event(&line).map(|event| (seq, event)))
            .collect(),
    )
}

/// Keeps only the last `CheckResult` per object: each one carries the
/// object's complete check state, so the last says it all. Every other
/// event stays, in order (rules and the event log need each transition).
pub(super) fn collapse(events: Vec<(u64, Event)>) -> Vec<(u64, Event)> {
    let mut last: HashMap<&ObjectKey, usize> = HashMap::new();
    let mut checks = 0;
    for (index, (_, event)) in events.iter().enumerate() {
        if let Event::CheckResult { object, .. } = event {
            last.insert(object, index);
            checks += 1;
        }
    }
    if last.len() == checks {
        // One result per object at most: nothing to collapse.
        return events;
    }
    let keep: Vec<bool> = events
        .iter()
        .enumerate()
        .map(|(index, (_, event))| match event {
            Event::CheckResult { object, .. } => last.get(object) == Some(&index),
            _ => true,
        })
        .collect();
    events
        .into_iter()
        .zip(keep)
        .filter_map(|(event, keep)| keep.then_some(event))
        .collect()
}

#[cfg(test)]
mod tests {
    use ic_model::{CheckResult, Timestamp};

    use super::*;

    fn check(object: &ObjectKey, output: &str) -> Event {
        Event::CheckResult {
            object: object.clone(),
            result: CheckResult {
                output: output.to_owned(),
                ..CheckResult::default()
            },
            downtime_depth: None,
            acknowledgement: None,
            after: None,
            at: Timestamp::EPOCH,
        }
    }

    fn output(event: &Event) -> &str {
        match event {
            Event::CheckResult { result, .. } => &result.output,
            _ => "other",
        }
    }

    #[test]
    fn collapses_check_results_per_object_only() {
        let a = ObjectKey::service("h", "a");
        let b = ObjectKey::host("h");
        let flapping = Event::Flapping {
            object: a.clone(),
            flapping: true,
            current: 30.0,
            at: Timestamp::EPOCH,
        };
        let events = vec![
            (1, check(&a, "a1")),
            (2, check(&b, "b1")),
            (3, flapping.clone()),
            (4, check(&a, "a2")),
            (5, flapping.clone()),
            (6, check(&a, "a3")),
        ];
        let collapsed = collapse(events);
        let seqs: Vec<u64> = collapsed.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, [2, 3, 5, 6], "other events are never collapsed");
        let outputs: Vec<&str> = collapsed.iter().map(|(_, e)| output(e)).collect();
        assert_eq!(outputs, ["b1", "other", "other", "a3"]);
    }

    #[test]
    fn parses_and_skips_garbage() {
        let lines = vec![
            (
                1,
                br#"{"type":"Flapping","host":"h","is_flapping":true,"timestamp":1}"#.to_vec(),
            ),
            (2, b"not json".to_vec()),
            (3, br#"{"type":"Nope"}"#.to_vec()),
        ];
        let events = prepare(lines);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 1);
    }
}
