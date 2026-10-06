//! The event bus behind `/v1/events`: one bounded queue per stream.
//!
//! Publishing never blocks: a stream whose queue is full is disconnected,
//! like a client on a broken connection. Events are encoded once and shared
//! by every stream that receives them.

use std::borrow::Cow;
use std::collections::HashSet;

use bytes::Bytes;
use serde_json::Value as Json;
use tokio::sync::mpsc;

use crate::config::NumberFormat;
use crate::filter::{ApiFilter, EvalError, Frame, Item, Value};
use crate::json;
use crate::model::ObjRef;

/// The event types of `/v1/events` (`eventshandler.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum EventType {
    CheckResult,
    StateChange,
    Notification,
    AcknowledgementSet,
    AcknowledgementCleared,
    CommentAdded,
    CommentRemoved,
    DowntimeAdded,
    DowntimeRemoved,
    DowntimeStarted,
    DowntimeTriggered,
    Flapping,
    ObjectCreated,
    ObjectModified,
    ObjectDeleted,
}

impl EventType {
    pub(crate) const ALL: [Self; 15] = [
        Self::CheckResult,
        Self::StateChange,
        Self::Notification,
        Self::AcknowledgementSet,
        Self::AcknowledgementCleared,
        Self::CommentAdded,
        Self::CommentRemoved,
        Self::DowntimeAdded,
        Self::DowntimeRemoved,
        Self::DowntimeStarted,
        Self::DowntimeTriggered,
        Self::Flapping,
        Self::ObjectCreated,
        Self::ObjectModified,
        Self::ObjectDeleted,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::CheckResult => "CheckResult",
            Self::StateChange => "StateChange",
            Self::Notification => "Notification",
            Self::AcknowledgementSet => "AcknowledgementSet",
            Self::AcknowledgementCleared => "AcknowledgementCleared",
            Self::CommentAdded => "CommentAdded",
            Self::CommentRemoved => "CommentRemoved",
            Self::DowntimeAdded => "DowntimeAdded",
            Self::DowntimeRemoved => "DowntimeRemoved",
            Self::DowntimeStarted => "DowntimeStarted",
            Self::DowntimeTriggered => "DowntimeTriggered",
            Self::Flapping => "Flapping",
            Self::ObjectCreated => "ObjectCreated",
            Self::ObjectModified => "ObjectModified",
            Self::ObjectDeleted => "ObjectDeleted",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }
}

/// What a stream's body receives.
pub(crate) type StreamItem = Bytes;

struct Subscriber {
    id: u64,
    types: HashSet<EventType>,
    filter: Option<ApiFilter>,
    user: String,
    queue: mpsc::Sender<StreamItem>,
    /// Receives nothing more but stays open (a stalled proxy or queue).
    stalled: bool,
    /// Lines and bytes queued for it so far.
    lines: u64,
    bytes: u64,
}

impl Subscriber {
    /// Whether the stream's filter lets the event through. Like Icinga, an
    /// event whose filter fails is skipped and logged (a filter that
    /// doesn't compile fails every event; it was logged when the stream
    /// opened).
    fn accepts(&self, frame: &EventFrame<'_>) -> bool {
        let Some(filter) = &self.filter else {
            return true;
        };
        match filter.matches(frame) {
            Ok(accepted) => accepted,
            Err(EvalError::Compile(_)) => false,
            Err(error @ EvalError::Script(_)) => {
                tracing::warn!(
                    stream = self.id,
                    user = %self.user,
                    %error,
                    "error evaluating event filter"
                );
                false
            }
        }
    }
}

/// Information about a connected stream, for the control API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamInfo {
    pub(crate) id: u64,
    pub(crate) user: String,
    pub(crate) types: Vec<&'static str>,
    pub(crate) filtered: bool,
    /// Lines and bytes queued for it so far.
    pub(crate) lines: u64,
    pub(crate) bytes: u64,
}

/// All connected event streams.
pub(crate) struct EventBus {
    subscribers: Vec<Subscriber>,
    next_id: u64,
    number_format: NumberFormat,
    buffer: usize,
    published: u64,
    /// Lines and bytes queued for every stream ever connected.
    delivered: (u64, u64),
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscribers", &self.subscribers.len())
            .field("published", &self.published)
            .finish_non_exhaustive()
    }
}

/// The frame of an event filter: the event as `event` (and `obj`).
struct EventFrame<'a> {
    event: &'a Json,
    now: f64,
}

impl Frame for EventFrame<'_> {
    fn variable(&self, name: &str) -> Option<Item<'_>> {
        matches!(name, "event" | "obj").then(|| Item::Json(Cow::Borrowed(self.event)))
    }

    fn field(&self, _object: &ObjRef, _name: &str) -> Result<Item<'_>, String> {
        Ok(Item::null())
    }

    fn object_value(&self, _object: &ObjRef) -> Value {
        Value::Null
    }

    fn now(&self) -> f64 {
        self.now
    }
}

impl EventBus {
    pub(crate) fn new(number_format: NumberFormat, buffer: usize) -> Self {
        Self {
            subscribers: Vec::new(),
            next_id: 1,
            number_format,
            buffer: buffer.max(1),
            published: 0,
            delivered: (0, 0),
        }
    }

    /// Registers a stream; the receiver feeds the HTTP response body.
    pub(crate) fn subscribe(
        &mut self,
        types: HashSet<EventType>,
        filter: Option<ApiFilter>,
        user: &str,
    ) -> (u64, mpsc::Receiver<StreamItem>) {
        let (queue, rx) = mpsc::channel(self.buffer);
        let id = self.next_id;
        self.next_id += 1;
        self.subscribers.push(Subscriber {
            id,
            types,
            filter,
            user: user.to_owned(),
            queue,
            stalled: false,
            lines: 0,
            bytes: 0,
        });
        (id, rx)
    }

    /// Whether anyone listens for `ty` (lets callers skip building events).
    pub(crate) fn wants(&self, ty: EventType) -> bool {
        self.subscribers.iter().any(|s| s.types.contains(&ty))
    }

    /// Publishes an event of type `ty` to every stream that subscribed to it
    /// and whose filter matches; `now` is the time filters see.
    pub(crate) fn publish(&mut self, ty: EventType, event: Json, now: f64) {
        if !self.wants(ty) {
            return;
        }
        self.published += 1;
        // Filters see the event before it's encoded (encoding consumes it).
        let frame = EventFrame { event: &event, now };
        let wanted: Vec<bool> = self
            .subscribers
            .iter()
            .map(|subscriber| subscriber.types.contains(&ty) && subscriber.accepts(&frame))
            .collect();
        let mut line = json::encode(event, self.number_format, false);
        line.push(b'\n');
        let line = Bytes::from(line);
        let mut wanted = wanted.into_iter();
        let delivered = &mut self.delivered;
        self.subscribers.retain_mut(|subscriber| {
            if wanted.next().unwrap_or(false) {
                deliver(subscriber, line.clone(), delivered)
            } else {
                !subscriber.queue.is_closed()
            }
        });
    }

    /// Sends a raw JSON value: to streams subscribed to its `type`, or to
    /// every stream when the type is missing or unknown.
    pub(crate) fn publish_raw(&mut self, event: Json, now: f64) {
        let ty = event
            .get("type")
            .and_then(Json::as_str)
            .and_then(EventType::from_name);
        if let Some(ty) = ty {
            self.publish(ty, event, now);
        } else {
            let mut line = json::encode(event, self.number_format, false);
            line.push(b'\n');
            self.publish_line_bytes(&Bytes::from(line));
        }
    }

    /// Sends raw bytes (e.g. a malformed line) to every stream.
    pub(crate) fn publish_line_bytes(&mut self, line: &Bytes) {
        let delivered = &mut self.delivered;
        self.subscribers
            .retain_mut(|subscriber| deliver(subscriber, line.clone(), delivered));
    }

    /// Lines and bytes queued for every stream ever connected.
    pub(crate) fn delivered(&self) -> (u64, u64) {
        self.delivered
    }

    /// Stalls every connected stream: it stays open but receives nothing
    /// more. Streams opened later aren't stalled. Returns how many.
    pub(crate) fn stall_all(&mut self) -> usize {
        self.subscribers.retain(|s| !s.queue.is_closed());
        for subscriber in &mut self.subscribers {
            subscriber.stalled = true;
        }
        self.subscribers.len()
    }

    /// Disconnects every stream (the connections are aborted).
    pub(crate) fn drop_all(&mut self) -> usize {
        let count = self.subscribers.len();
        self.subscribers.clear();
        count
    }

    /// Connected streams (closed ones are pruned first).
    pub(crate) fn streams(&mut self) -> Vec<StreamInfo> {
        self.subscribers.retain(|s| !s.queue.is_closed());
        self.subscribers
            .iter()
            .map(|s| {
                let mut types: Vec<_> = s.types.iter().map(|t| t.name()).collect();
                types.sort_unstable();
                StreamInfo {
                    id: s.id,
                    user: s.user.clone(),
                    types,
                    filtered: s.filter.is_some(),
                    lines: s.lines,
                    bytes: s.bytes,
                }
            })
            .collect()
    }
}

/// Queues a line, counting it (per stream and in `delivered`); `false`
/// drops the subscriber (gone, or too slow). A stalled subscriber gets
/// nothing and stays while its connection does.
fn deliver(subscriber: &mut Subscriber, line: Bytes, delivered: &mut (u64, u64)) -> bool {
    if subscriber.stalled {
        return !subscriber.queue.is_closed();
    }
    let bytes = line.len() as u64;
    match subscriber.queue.try_send(line) {
        Ok(()) => {
            subscriber.lines += 1;
            subscriber.bytes += bytes;
            delivered.0 += 1;
            delivered.1 += bytes;
            true
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                stream = subscriber.id,
                user = %subscriber.user,
                "event stream consumer too slow; disconnecting it"
            );
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Node, compile};
    use serde_json::json;

    fn types(list: &[EventType]) -> HashSet<EventType> {
        list.iter().copied().collect()
    }

    #[test]
    fn names_round_trip() {
        for ty in EventType::ALL {
            assert_eq!(EventType::from_name(ty.name()), Some(ty));
        }
        assert_eq!(EventType::from_name("checkresult"), None);
    }

    #[test]
    fn delivers_only_subscribed_types() {
        let mut bus = EventBus::new(NumberFormat::Float, 8);
        let (_, mut rx) = bus.subscribe(types(&[EventType::StateChange]), None, "root");
        bus.publish(
            EventType::CheckResult,
            json!({ "type": "CheckResult" }),
            0.0,
        );
        bus.publish(
            EventType::StateChange,
            json!({ "type": "StateChange", "state": 2 }),
            0.0,
        );
        let line = rx.try_recv().unwrap();
        assert_eq!(&line[..], b"{\"state\":2.0,\"type\":\"StateChange\"}\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn filters_events() {
        let mut bus = EventBus::new(NumberFormat::Integral, 8);
        let filter = compile(
            r#"event.host == "a" && obj.check_result.state > 0 && get_time() == 5"#,
            Node::default(),
        );
        let (_, mut rx) = bus.subscribe(types(&[EventType::CheckResult]), Some(filter), "root");
        let (_, mut nothing) = bus.subscribe(
            types(&[EventType::CheckResult]),
            Some(compile("event.host ==", Node::default())),
            "root",
        );
        let failing = compile("event.host.x", Node::default());
        let (_, mut failed) =
            bus.subscribe(types(&[EventType::CheckResult]), Some(failing), "root");
        for host in ["b", "a"] {
            bus.publish(
                EventType::CheckResult,
                json!({ "type": "CheckResult", "host": host, "check_result": { "state": 2 } }),
                5.0,
            );
        }
        let line = rx.try_recv().unwrap();
        assert!(String::from_utf8_lossy(&line).contains("\"host\":\"a\""));
        assert!(rx.try_recv().is_err());
        assert!(nothing.try_recv().is_err(), "a filter that doesn't compile");
        assert!(failed.try_recv().is_err(), "failing filters skip the event");
        assert_eq!(bus.streams().len(), 3, "and keep the stream");
    }

    #[test]
    fn slow_consumers_are_dropped_without_blocking() {
        let mut bus = EventBus::new(NumberFormat::Float, 2);
        let (_, mut rx) = bus.subscribe(types(&[EventType::CheckResult]), None, "root");
        for _ in 0..5 {
            bus.publish(
                EventType::CheckResult,
                json!({ "type": "CheckResult" }),
                0.0,
            );
        }
        assert!(
            bus.streams().is_empty(),
            "overflowing stream is disconnected"
        );
        // The two queued lines are still readable, then the channel closes.
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn stalled_streams_stay_open_but_receive_nothing() {
        let mut bus = EventBus::new(NumberFormat::Float, 4);
        let (_, mut stalled) = bus.subscribe(types(&[EventType::CheckResult]), None, "root");
        assert_eq!(bus.stall_all(), 1);
        let (_, mut fresh) = bus.subscribe(types(&[EventType::CheckResult]), None, "root");
        // More than the buffer holds: a stalled stream isn't "too slow".
        for _ in 0..10 {
            bus.publish(
                EventType::CheckResult,
                json!({ "type": "CheckResult" }),
                0.0,
            );
            assert!(fresh.try_recv().is_ok(), "streams opened later work");
        }
        bus.publish_line_bytes(&Bytes::from_static(b"raw\n"));
        assert!(matches!(
            stalled.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(bus.streams().len(), 2, "both stay connected");
        drop(stalled);
        bus.publish(
            EventType::CheckResult,
            json!({ "type": "CheckResult" }),
            0.0,
        );
        assert_eq!(bus.streams().len(), 1, "a closed stalled stream is pruned");
    }

    #[test]
    fn closed_streams_are_pruned() {
        let mut bus = EventBus::new(NumberFormat::Float, 2);
        let (_, rx) = bus.subscribe(types(&[EventType::CheckResult]), None, "root");
        assert_eq!(bus.streams().len(), 1);
        drop(rx);
        assert!(bus.streams().is_empty());
    }

    #[test]
    fn raw_events_with_unknown_types_reach_everyone() {
        let mut bus = EventBus::new(NumberFormat::Float, 4);
        let (_, mut rx) = bus.subscribe(types(&[EventType::Flapping]), None, "root");
        bus.publish_raw(json!({ "type": "SomethingNew" }), 0.0);
        bus.publish_line_bytes(&Bytes::from_static(b"not json\n"));
        assert!(rx.try_recv().is_ok());
        assert_eq!(&rx.try_recv().unwrap()[..], b"not json\n");
        assert_eq!(bus.drop_all(), 1);
    }
}
