//! Icinga domain types shared by every other crate.
//!
//! This crate is pure: no I/O, no async runtime, no UI. Wire formats live in
//! `ic-api` and are mapped into these types at the edge.

mod action;
mod event;
mod name;
mod notification;
mod object;
mod perfdata;
pub mod severity;
mod state;
mod status;
mod time;

pub use action::{Action, ActionTarget, ChildOptions, CommandType, DowntimeMode};
pub use event::{CheckableState, Event, EventKind, ObjectChange, StateAfter};
pub use name::{HostName, ObjectKey, ServiceKey};
pub use notification::{Notification, Notified};
pub use object::{
    AckKind, CheckInfo, CheckResult, Comment, CommentKind, Dependency, Downtime, Endpoint,
    Features, Host, HostGroup, Links, Service, ServiceGroup, Vars, Zone,
};
pub use perfdata::{
    Perfdata, PerfdataStatus, Threshold, format_number, parse_perfdata, parse_perfdata_entry,
};
pub use state::{HostState, ServiceState, StateType};
pub use status::InstanceStatus;
pub use time::{Timestamp, format_compact, format_two_units};
