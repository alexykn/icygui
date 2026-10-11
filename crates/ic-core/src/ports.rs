//! Ports: what the core needs from the operating system. Implemented in
//! `ic-platform` (secrets) and `ic-app` (notifications, via GPUI); tests use
//! in-memory fakes.

use ic_model::Timestamp;
use ic_rules::{LocalTime, NotificationIntent};
use secrecy::SecretString;

/// Error from the platform's secret store.
#[derive(Debug, thiserror::Error)]
#[error("secret store: {message}")]
pub struct SecretError {
    /// What went wrong, for logs and the UI.
    pub message: String,
}

impl SecretError {
    /// An error with a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Stores credentials outside the config file (Keychain, Secret Service).
/// `account` is the environment id.
pub trait SecretStore: Send + Sync + 'static {
    /// Reads a secret; `Ok(None)` if there is none.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError>;

    /// Creates or replaces a secret.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError>;

    /// Deletes a secret; succeeds if there was none.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// Shows OS notifications. Called from the core's runtime thread; the
/// implementation hands the intent to the UI thread.
pub trait Notifier: Send + Sync + 'static {
    /// Shows `intent` (callers never pass silent intents).
    fn notify(&self, intent: &NotificationIntent);
}

/// Wall-clock time, injectable for tests.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now(&self) -> Timestamp;
    /// Local weekday and time of day, for quiet hours.
    fn local(&self) -> LocalTime;
}

/// The system clock, with local time in the system's time zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn local(&self) -> LocalTime {
        local_time(&jiff::Zoned::now())
    }
}

/// `at` as a local time of day (`02:14`), in the system's time zone: how
/// trouble alerts say when something began.
pub(crate) fn clock_time(at: Timestamp) -> String {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "milliseconds since 1970 fit an i64"
    )]
    let millis = (at.as_unix_seconds() * 1_000.0).round() as i64;
    jiff::Timestamp::from_millisecond(millis).map_or_else(
        |_| String::new(),
        |time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        },
    )
}

/// Weekday (Monday = 0) and minute of the day of a zoned time.
fn local_time(time: &jiff::Zoned) -> LocalTime {
    let weekday = u8::try_from(time.weekday().to_monday_zero_offset()).unwrap_or(0);
    let minutes = i32::from(time.hour()) * 60 + i32::from(time.minute());
    LocalTime {
        weekday,
        minute_of_day: u16::try_from(minutes).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use super::*;

    #[test]
    fn local_time_counts_from_monday_and_midnight() {
        let zone = TimeZone::fixed(jiff::tz::offset(2));
        // 2026-10-05 is a Monday.
        let monday = date(2026, 10, 5)
            .at(0, 0, 0, 0)
            .to_zoned(zone.clone())
            .unwrap();
        assert_eq!(
            local_time(&monday),
            LocalTime {
                weekday: 0,
                minute_of_day: 0
            }
        );
        let sunday = date(2026, 10, 11).at(23, 59, 30, 0).to_zoned(zone).unwrap();
        assert_eq!(
            local_time(&sunday),
            LocalTime {
                weekday: 6,
                minute_of_day: 23 * 60 + 59
            }
        );
    }

    #[test]
    fn system_clock_is_now() {
        let before = Timestamp::now();
        let now = SystemClock.now();
        assert!(now >= before);
        let local = SystemClock.local();
        assert!(local.weekday < 7);
        assert!(local.minute_of_day < 24 * 60);
    }
}
