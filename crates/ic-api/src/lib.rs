//! Icinga 2 REST API client: object queries, `/v1/actions`, `/v1/status` and the
//! `/v1/events` stream.
//!
//! Read-only towards configuration: this crate never calls `objects/modify`,
//! config packages or anything else that changes Icinga's settings.
