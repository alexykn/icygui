//! Client-side notifications in the window (PLAN.md §2.7, D5): what the
//! notification centre lists, watching and muting objects, pausing, and
//! the local event log's history (PANE-04). The rules themselves run in
//! the core (`ic-rules`); the settings dialog that edits them is
//! `crate::settings`.
//!
//! - [`timing`]: pause and mute choices and how their ends read;
//! - [`OverrideChange`]: watch, mute or unmute objects (NOTE-02), from the
//!   panes and the palette;
//! - [`entry`]: a notification as the centre shows it (NOTE-05);
//! - [`history`]: a log entry as the history shows it (PANE-04).

pub(crate) mod entry;
pub(crate) mod history;
mod timing;

use ic_model::{ObjectKey, Timestamp};
use ic_rules::ObjectMode;

pub(crate) use self::timing::{MuteChoice, PauseChoice, when};
use crate::app_state::AppState;
use crate::operate::forms::describe_objects;

/// What to do about objects' notifications (NOTE-02): always notify,
/// never notify for a while, or follow the dashboards again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OverrideChange {
    /// Notify with the environment's rule even if no dashboard does.
    Watch,
    /// Never notify, for this long.
    Mute(MuteChoice),
    /// Remove the watch or mute.
    Clear,
}

impl OverrideChange {
    /// Applies the change to `objects` at `now`. Returns what to tell the
    /// user, or `None` if nothing changed.
    pub(crate) fn apply(
        self,
        state: &mut AppState,
        objects: &[ObjectKey],
        now: Timestamp,
    ) -> Option<String> {
        let what = describe_objects(objects);
        let changed = match self {
            Self::Watch => state.set_object_override(objects, ObjectMode::Watch, None, now),
            Self::Mute(choice) => {
                state.set_object_override(objects, ObjectMode::Mute, choice.until(now), now)
            }
            Self::Clear => state.remove_object_override(objects, now),
        };
        changed.then(|| match self {
            Self::Watch => format!("Watching {what}"),
            Self::Mute(choice) => format!("Muted {what} {}", choice.label(now)),
            Self::Clear => format!("{what}: notifications follow the dashboards again"),
        })
    }
}

/// How a watch or mute reads in a pane (`watched`, `muted until 18:30`,
/// `muted`).
pub(crate) fn override_text(entry: &ic_rules::ObjectOverride, now: Timestamp) -> String {
    match (entry.mode, entry.until) {
        (ObjectMode::Watch, _) => "watched · always notifies".to_owned(),
        (ObjectMode::Mute, Some(until)) => format!("muted until {}", when(until, now)),
        (ObjectMode::Mute, None) => "muted until unmuted".to_owned(),
    }
}

/// What the pause chips are labelled: `pause`, or `pause all` with
/// several environments (the pause holds for every one; an environment
/// is muted on its own from the switcher or the palette, A5).
pub(crate) fn pause_label(environments: usize) -> &'static str {
    if environments > 1 {
        "pause all"
    } else {
        "pause"
    }
}

/// How a running pause of every environment reads: `paused until 08:00`,
/// or `all paused until 08:00` with several environments.
pub(crate) fn paused_text(environments: usize, until: Timestamp, now: Timestamp) -> String {
    let all = if environments > 1 { "all " } else { "" };
    format!("{all}paused until {}", when(until, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_several_environments_the_pause_says_it_holds_for_all() {
        assert_eq!(pause_label(1), "pause");
        assert_eq!(pause_label(3), "pause all");
        let at = now();
        let until = Timestamp::from_unix_seconds(at.as_unix_seconds() + 1800.);
        assert!(paused_text(1, until, at).starts_with("paused until "));
        assert!(paused_text(2, until, at).starts_with("all paused until "));
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    #[test]
    fn changes_say_what_they_did() {
        let mut state = AppState::fixture(now());
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let objects = std::slice::from_ref(&replication);
        let muted = OverrideChange::Mute(MuteChoice::Hour).apply(&mut state, objects, now());
        assert_eq!(
            muted.as_deref(),
            Some("Muted postgres-replication on db-prod-03 for 1 hour")
        );
        let entry = state.object_override(&replication, now()).unwrap();
        assert!(override_text(entry, now()).starts_with("muted until "));
        assert_eq!(
            OverrideChange::Watch
                .apply(&mut state, objects, now())
                .as_deref(),
            Some("Watching postgres-replication on db-prod-03")
        );
        assert_eq!(
            override_text(state.object_override(&replication, now()).unwrap(), now()),
            "watched · always notifies"
        );
        assert!(
            OverrideChange::Clear
                .apply(&mut state, objects, now())
                .is_some()
        );
        assert!(
            OverrideChange::Clear
                .apply(&mut state, objects, now())
                .is_none(),
            "nothing left to clear"
        );
    }
}
