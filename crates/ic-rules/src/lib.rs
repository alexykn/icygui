//! Client-side notification rule engine.
//!
//! Resolves rule scopes (environment > group > dashboard > object), matches
//! events, deduplicates and coalesces storms. Pure: it turns events into
//! notification intents and never talks to the OS.
