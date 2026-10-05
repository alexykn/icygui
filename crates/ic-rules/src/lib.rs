//! Client-side notification rule engine.
//!
//! Resolves rule scopes (object > dashboard > group > environment), matches
//! changes against rules, deduplicates and coalesces storms. Pure: it turns
//! [`RuleInput`]s into [`NotificationIntent`]s and never talks to the OS.

mod intent;
mod settings;

pub use intent::{Change, DashboardRef, LocalTime, NotificationIntent, RuleInput, Tone};
pub use settings::{
    EventFilter, NotificationSettings, ObjectMode, ObjectOverride, QuietHours, Rule, ScopeSetting,
    StateFilter, StormControl,
};
