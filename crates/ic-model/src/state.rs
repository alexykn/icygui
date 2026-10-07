//! Host and service states as Icinga 2 reports them.

use serde::{Deserialize, Serialize};

/// Whether a state is soft (still retrying) or hard (confirmed).
///
/// Icinga 2 reports this as `state_type`: `0` = soft, `1` = hard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateType {
    /// The check failed but has not reached `max_check_attempts` yet.
    Soft,
    /// The state is confirmed; notifications are based on hard states.
    Hard,
}

impl StateType {
    /// Maps Icinga's numeric `state_type`.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Soft),
            1 => Some(Self::Hard),
            _ => None,
        }
    }
}

/// The current state of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceState {
    /// `0`.
    Ok,
    /// `1`.
    Warning,
    /// `2`.
    Critical,
    /// `3`.
    Unknown,
    /// Not checked yet (`last_check_result` is null).
    Pending,
}

impl ServiceState {
    /// Maps Icinga's numeric service `state`. Pending is not a code; callers
    /// use [`ServiceState::Pending`] when the object has no check result.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Ok),
            1 => Some(Self::Warning),
            2 => Some(Self::Critical),
            3 => Some(Self::Unknown),
            _ => None,
        }
    }

    /// The short label shown under state circles (`CRIT`, `WARN`, …).
    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARN",
            Self::Critical => "CRIT",
            Self::Unknown => "UNKN",
            Self::Pending => "PEND",
        }
    }

    /// Whether this state is a problem (anything but OK and pending).
    #[must_use]
    pub fn is_problem(self) -> bool {
        matches!(self, Self::Warning | Self::Critical | Self::Unknown)
    }
}

/// The current state of a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostState {
    /// `0`.
    Up,
    /// `1` while the host is reachable.
    Down,
    /// `1` while a parent is down. Icinga 2 reports these as down with
    /// `last_reachable = false`; Icinga Web shows them as unreachable.
    Unreachable,
    /// Not checked yet (`last_check_result` is null).
    Pending,
}

impl HostState {
    /// Maps Icinga's numeric host `state` together with its reachability.
    #[must_use]
    pub fn from_code(code: u8, reachable: bool) -> Option<Self> {
        match (code, reachable) {
            (0, _) => Some(Self::Up),
            (1, true) => Some(Self::Down),
            (1, false) => Some(Self::Unreachable),
            _ => None,
        }
    }

    /// The short label shown under state circles (`UP`, `DOWN`, …).
    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Unreachable => "UNRE",
            Self::Pending => "PEND",
        }
    }

    /// Whether this state is a problem (down or unreachable).
    #[must_use]
    pub fn is_problem(self) -> bool {
        matches!(self, Self::Down | Self::Unreachable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_codes_round_trip() {
        assert_eq!(ServiceState::from_code(0), Some(ServiceState::Ok));
        assert_eq!(ServiceState::from_code(1), Some(ServiceState::Warning));
        assert_eq!(ServiceState::from_code(2), Some(ServiceState::Critical));
        assert_eq!(ServiceState::from_code(3), Some(ServiceState::Unknown));
        assert_eq!(ServiceState::from_code(4), None);
    }

    #[test]
    fn unreachable_hosts_are_down_hosts_behind_a_down_parent() {
        assert_eq!(HostState::from_code(0, false), Some(HostState::Up));
        assert_eq!(HostState::from_code(1, true), Some(HostState::Down));
        assert_eq!(HostState::from_code(1, false), Some(HostState::Unreachable));
        assert_eq!(HostState::from_code(2, true), None);
    }

    #[test]
    fn only_non_ok_checked_states_are_problems() {
        assert!(!ServiceState::Ok.is_problem());
        assert!(!ServiceState::Pending.is_problem());
        assert!(ServiceState::Unknown.is_problem());
        assert!(!HostState::Up.is_problem());
        assert!(HostState::Unreachable.is_problem());
    }

    #[test]
    fn state_types() {
        assert_eq!(StateType::from_code(0), Some(StateType::Soft));
        assert_eq!(StateType::from_code(1), Some(StateType::Hard));
        assert_eq!(StateType::from_code(2), None);
    }
}
