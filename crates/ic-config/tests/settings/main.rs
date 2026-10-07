//! Integration tests for `ic-config`'s public API: round trips, loading and
//! migrations, validation, secrets, sharing dashboards and the file layout.

// `cfg(test)` makes clippy treat the helpers in these modules as test code.
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod layout;
#[cfg(test)]
mod loading;
#[cfg(test)]
mod logs;
#[cfg(test)]
mod round_trip;
#[cfg(test)]
mod secrets;
#[cfg(test)]
mod sharing;
#[cfg(test)]
mod validation;
