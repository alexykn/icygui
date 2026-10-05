//! Captures what the crate logs, so tests can check its warnings.

use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// One logged event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Logged {
    /// Its level.
    pub(crate) level: Level,
    /// Its message, then its other fields as `name=value`.
    pub(crate) text: String,
}

/// Runs `f` and returns its result with the events it logged on this
/// thread (other tests run on other threads and aren't captured).
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<Logged>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let result = tracing::subscriber::with_default(Capture(Arc::clone(&events)), f);
    let events = events.lock().unwrap().clone();
    (result, events)
}

/// The text of each warning in `events`.
pub(crate) fn warnings(events: &[Logged]) -> Vec<&str> {
    events
        .iter()
        .filter(|event| event.level == Level::WARN)
        .map(|event| event.text.as_str())
        .collect()
}

struct Capture(Arc<Mutex<Vec<Logged>>>);

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut text = Text::default();
        event.record(&mut text);
        self.0.lock().unwrap().push(Logged {
            level: *event.metadata().level(),
            text: format!("{}{}", text.message, text.fields),
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// An event's message, and its other fields as ` name=value`.
#[derive(Default)]
struct Text {
    message: String,
    fields: String,
}

impl Visit for Text {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            write!(self.message, "{value:?}").unwrap();
        } else {
            write!(self.fields, " {}={value:?}", field.name()).unwrap();
        }
    }
}
