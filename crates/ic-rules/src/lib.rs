//! Client-side notification rule engine.
//!
//! Resolves rule scopes (object > dashboard > group > environment), matches
//! changes against rules, delays, deduplicates and coalesces storms. Pure:
//! it turns [`RuleInput`]s into [`NotificationIntent`]s and never talks to
//! the OS or reads a clock.
//!
//! - [`RuleSet`] describes one environment's rules: its
//!   [`NotificationSettings`] and the group → dashboard tree with each
//!   level's [`ScopeSetting`].
//! - [`RuleEngine`] judges changes against it. The full semantics are
//!   documented there.
//!
//! ```
//! use ic_model::{CheckableState, ObjectKey, ServiceState, StateType, Timestamp};
//! use ic_rules::{Change, LocalTime, RuleEngine, RuleInput, RuleSet};
//!
//! let rules = RuleSet {
//!     environment_name: "prod-cluster".to_owned(),
//!     ..RuleSet::default()
//! };
//! let mut engine = RuleEngine::new(rules);
//! let now = Timestamp::from_unix_seconds(1_700_000_000.0);
//! let noon = LocalTime { weekday: 0, minute_of_day: 12 * 60 };
//!
//! let intents = engine.on_input(
//!     RuleInput {
//!         object: ObjectKey::service("db-prod-03", "postgres-replication"),
//!         host_display: "db-prod-03".to_owned(),
//!         service_display: Some("postgres-replication".to_owned()),
//!         change: Change::State {
//!             previous: Some(CheckableState::Service(ServiceState::Ok)),
//!             current: CheckableState::Service(ServiceState::Critical),
//!             state_type: StateType::Hard,
//!             since: now,
//!             output: "CRITICAL - replication lag 412s".to_owned(),
//!         },
//!         handled: false,
//!         memberships: Vec::new(),
//!         at: now,
//!     },
//!     now,
//!     noon,
//! );
//! assert_eq!(intents.len(), 1);
//! assert_eq!(intents[0].title, "CRITICAL · postgres-replication on db-prod-03");
//! assert_eq!(intents[0].subtitle, "prod-cluster");
//! assert_eq!(intents[0].body, "CRITICAL - replication lag 412s");
//! assert!(!intents[0].silent);
//!
//! // The engine also needs a tick about once a second, for delayed
//! // notifications and storm summaries.
//! assert!(engine.tick(now, noon).is_empty());
//! ```

mod dedupe;
mod engine;
mod intent;
mod quiet;
mod recent;
mod scope;
mod settings;
mod storm;
mod text;

pub use engine::RuleEngine;
pub use intent::{
    Change, DashboardRef, LocalTime, NotificationIntent, RuleInput, Silence, Tone, state_intent_id,
};
pub use scope::{DashboardScope, EffectiveRule, GroupScope, RuleSet};
pub use settings::{
    EventFilter, NotificationSettings, ObjectMode, ObjectOverride, QuietHours, Rule, ScopeSetting,
    StateFilter, StormControl,
};
