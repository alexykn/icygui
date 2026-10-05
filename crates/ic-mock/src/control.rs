//! [`MockControl`]: drive a running mock from tests (state changes, events,
//! faults, the simulator, the clock) and inspect what it has seen.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ic_model::{
    Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostState, InstanceStatus, ObjectKey,
    Service, ServiceGroup, ServiceState, Timestamp,
};

use crate::error::MockError;
use crate::events::EventType;
use crate::model::logic::{DowntimeSpec, RemovalReason};
use crate::model::{CheckInput, ObjKind, ObjRef, ProcessOutcome, World};
use crate::server::Shared;
use crate::tls;

/// A request the server received (recorded before authentication, so
/// failed logins show up too).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedRequest {
    /// The HTTP method as sent.
    pub method: String,
    /// The `X-HTTP-Method-Override` header, if any.
    pub method_override: Option<String>,
    /// The raw (percent-encoded) path.
    pub path: String,
    /// Decoded query parameters in order.
    pub query: Vec<(String, String)>,
    /// The body, if it was JSON.
    pub body: Option<serde_json::Value>,
    /// The authenticated user.
    pub user: Option<String>,
    /// The response status.
    pub status: u16,
    /// When it arrived.
    pub at: Timestamp,
}

impl RecordedRequest {
    /// The method the server acted on (the override, if any).
    #[must_use]
    pub fn effective_method(&self) -> &str {
        self.method_override.as_deref().unwrap_or(&self.method)
    }
}

/// Controls a running [`crate::MockServer`]. Cheap to clone; every method
/// acts immediately and emits the events Icinga would.
#[derive(Clone)]
pub struct MockControl {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for MockControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockControl")
            .field("node", &self.shared.node_name)
            .finish_non_exhaustive()
    }
}

fn unknown(object: &str) -> MockError {
    MockError::UnknownObject(object.to_owned())
}

fn service_code(state: ServiceState) -> Result<u8, MockError> {
    match state {
        ServiceState::Ok => Ok(0),
        ServiceState::Warning => Ok(1),
        ServiceState::Critical => Ok(2),
        ServiceState::Unknown => Ok(3),
        ServiceState::Pending => Err(MockError::Rejected(
            "an object can't go back to pending".to_owned(),
        )),
    }
}

impl MockControl {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.shared.world()
    }

    // --- state changes -----------------------------------------------------

    /// Processes check results for a service until it reports `state`:
    /// one result, or (with `hard`) as many as `max_check_attempts` needs
    /// for a hard state. Each result emits `CheckResult` and `StateChange`
    /// events like a real check.
    ///
    /// # Errors
    /// Unknown service, or `Pending`.
    pub fn set_service_state(
        &self,
        host: &str,
        service: &str,
        state: ServiceState,
        output: &str,
        hard: bool,
    ) -> Result<(), MockError> {
        let code = service_code(state)?;
        self.set_state(&format!("{host}!{service}"), code, output, hard)
    }

    /// Processes check results for a host until it reports `state`
    /// (`Down` and `Unreachable` both send a DOWN result; whether the host
    /// counts as unreachable follows from its dependencies).
    ///
    /// # Errors
    /// Unknown host, or `Pending`.
    pub fn set_host_state(
        &self,
        host: &str,
        state: HostState,
        output: &str,
        hard: bool,
    ) -> Result<(), MockError> {
        let code = match state {
            HostState::Up => 0,
            HostState::Down | HostState::Unreachable => 2,
            HostState::Pending => {
                return Err(MockError::Rejected(
                    "an object can't go back to pending".to_owned(),
                ));
            }
        };
        self.set_state(host, code, output, hard)
    }

    fn set_state(&self, object: &str, code: u8, output: &str, hard: bool) -> Result<(), MockError> {
        let mut world = self.world();
        let max = world
            .checkable(object)
            .ok_or_else(|| unknown(object))?
            .max_check_attempts;
        for _ in 0..=max {
            let checkable = world.checkable(object).ok_or_else(|| unknown(object))?;
            let input = world.recheck_input(checkable, Some((code, output.to_owned(), None)));
            world.process_check_result(object, input);
            let checkable = world.checkable(object).ok_or_else(|| unknown(object))?;
            if !hard || checkable.state_type == 1 {
                break;
            }
        }
        Ok(())
    }

    /// Processes one check result (as from the checker) with a plugin exit
    /// status (services 0–3; hosts 0 up, 1 down), output and perfdata.
    ///
    /// # Errors
    /// Unknown object, or a host exit status other than 0 and 1.
    pub fn process_check_result(
        &self,
        object: &ObjectKey,
        exit_status: u8,
        output: &str,
        perfdata: &[&str],
    ) -> Result<(), MockError> {
        let name = object.full_name();
        let mut world = self.world();
        let checkable = world.checkable(&name).ok_or_else(|| unknown(&name))?;
        let state = if checkable.is_service() {
            exit_status.min(3)
        } else {
            match exit_status {
                0 => 0,
                1 => 2,
                _ => {
                    return Err(MockError::Rejected(format!(
                        "Invalid 'exit_status' for Host {name}."
                    )));
                }
            }
        };
        let base = world.recheck_input(checkable, None);
        let input = CheckInput {
            state,
            exit_status: i64::from(exit_status),
            output: output.to_owned(),
            performance_data: Some(perfdata.iter().map(|p| (*p).to_owned()).collect()),
            ..base
        };
        match world.process_check_result(&name, input) {
            Some(ProcessOutcome::Processed) => Ok(()),
            Some(ProcessOutcome::NewerCheckResultPresent) => Err(MockError::Rejected(
                "a newer check result is already present".to_owned(),
            )),
            None => Err(unknown(&name)),
        }
    }

    /// Acknowledges a problem like `acknowledge-problem` (with the
    /// acknowledgement comment and events).
    ///
    /// # Errors
    /// Unknown object; OK/UP objects and existing acknowledgements are
    /// rejected as Icinga does.
    pub fn acknowledge(
        &self,
        object: &ObjectKey,
        author: &str,
        comment: &str,
        sticky: bool,
    ) -> Result<(), MockError> {
        let name = object.full_name();
        let mut world = self.world();
        let checkable = world.checkable(&name).ok_or_else(|| unknown(&name))?;
        let ok = if checkable.is_service() {
            checkable.state_raw == 0
        } else {
            checkable.state() == 0
        };
        if ok {
            return Err(MockError::Rejected(format!("{name} has no problem")));
        }
        world.expire_acknowledgement(&name);
        if world
            .checkable(&name)
            .is_some_and(|c| c.acknowledgement != 0)
        {
            return Err(MockError::Rejected(format!(
                "{name} is already acknowledged"
            )));
        }
        world.acknowledge(&name, author, comment, sticky, false, false, 0.0);
        Ok(())
    }

    /// Removes an acknowledgement and its comments.
    ///
    /// # Errors
    /// Unknown object.
    pub fn remove_acknowledgement(&self, object: &ObjectKey) -> Result<(), MockError> {
        let name = object.full_name();
        let mut world = self.world();
        world.checkable(&name).ok_or_else(|| unknown(&name))?;
        world.clear_acknowledgement(&name, "");
        world.remove_ack_comments(&name, "", f64::INFINITY);
        Ok(())
    }

    /// Adds a user comment; returns its name.
    ///
    /// # Errors
    /// Unknown object.
    pub fn add_comment(
        &self,
        object: &ObjectKey,
        author: &str,
        text: &str,
    ) -> Result<String, MockError> {
        let name = object.full_name();
        self.world()
            .add_comment(&name, 1, author, text, false, 0.0, false)
            .map(|(comment, _)| comment)
            .ok_or_else(|| unknown(&name))
    }

    /// Schedules a downtime (fixed, or flexible with `duration`); returns
    /// its name.
    ///
    /// # Errors
    /// Unknown object.
    pub fn schedule_downtime(
        &self,
        object: &ObjectKey,
        author: &str,
        comment: &str,
        start: Timestamp,
        end: Timestamp,
        flexible_duration: Option<Duration>,
    ) -> Result<String, MockError> {
        let name = object.full_name();
        self.world()
            .add_downtime(
                &name,
                DowntimeSpec {
                    author: author.to_owned(),
                    comment: comment.to_owned(),
                    start_time: start.as_unix_seconds(),
                    end_time: end.as_unix_seconds(),
                    fixed: flexible_duration.is_none(),
                    duration: flexible_duration.map_or(0.0, |d| d.as_secs_f64()),
                    ..DowntimeSpec::default()
                },
            )
            .map(|(downtime, _)| downtime)
            .ok_or_else(|| unknown(&name))
    }

    /// Removes a downtime (and its child downtimes).
    ///
    /// # Errors
    /// Unknown downtime, or one owned by a `ScheduledDowntime`.
    pub fn remove_downtime(&self, name: &str) -> Result<(), MockError> {
        match self
            .world()
            .remove_downtime(name, true, RemovalReason::User, "")
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(unknown(name)),
            Err(message) => Err(MockError::Rejected(message)),
        }
    }

    /// Marks a cluster endpoint connected or disconnected.
    ///
    /// # Errors
    /// Unknown endpoint.
    pub fn set_endpoint_connected(&self, name: &str, connected: bool) -> Result<(), MockError> {
        let mut world = self.world();
        let endpoint = world.endpoints.get_mut(name).ok_or_else(|| unknown(name))?;
        endpoint.connected = connected;
        Ok(())
    }

    /// Emits `ObjectModified` for an object, as a configuration deployment
    /// would (the mock never changes configuration itself).
    ///
    /// # Errors
    /// Unknown type or object.
    pub fn touch_object(&self, type_name: &str, name: &str) -> Result<(), MockError> {
        let kind = ObjKind::from_type_name(type_name).ok_or_else(|| unknown(type_name))?;
        let mut world = self.world();
        if !world.exists(kind, name) {
            return Err(unknown(name));
        }
        world.emit_object_change(EventType::ObjectModified, type_name, name);
        Ok(())
    }

    // --- event streams ----------------------------------------------------

    /// Sends a raw JSON value to the event streams: to those subscribed to
    /// its `type`, or to every stream when the type is missing or unknown.
    pub fn emit_raw(&self, event: serde_json::Value) {
        let mut world = self.world();
        let now = world.now();
        world.bus.publish_raw(event, now);
    }

    /// Sends a raw line (a newline is appended) to every event stream, e.g.
    /// malformed JSON.
    pub fn emit_raw_line(&self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.world().bus.publish_line_bytes(&Bytes::from(bytes));
    }

    /// Disconnects every event stream (the connections are aborted, like a
    /// network failure). Returns how many there were.
    pub fn drop_event_streams(&self) -> usize {
        self.world().bus.drop_all()
    }

    /// Number of connected event streams.
    pub fn event_streams(&self) -> usize {
        self.world().bus.streams().len()
    }

    /// Waits until at least `count` event streams are connected.
    pub async fn wait_for_event_streams(&self, count: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.event_streams() >= count {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // --- faults -------------------------------------------------------------

    /// Fails the next `count` requests with `status` (e.g. 500, 503) and an
    /// Icinga-style error body, before authentication. Codes outside
    /// `100..=999` become 500. `count = 0` cancels pending failures.
    pub fn fail_next(&self, count: u32, status: u16) {
        self.shared.set_failures(count, status);
    }

    /// Delays every response by `latency` (zero turns it off).
    pub fn set_latency(&self, latency: Duration) {
        self.shared.set_latency(latency);
    }

    /// Aborts every open connection, event streams included.
    pub fn drop_connections(&self) {
        self.world().bus.drop_all();
        self.shared.kill_connections();
    }

    /// Serves a new certificate (new fingerprint; same CA when CA-signed)
    /// and drops open connections. Returns the new fingerprint.
    ///
    /// # Errors
    /// Certificate generation failed.
    pub fn rotate_certificate(&self) -> Result<[u8; 32], MockError> {
        let material = self.shared.tls().material.rotated(&self.shared.node_name)?;
        let state = tls::server_state(material)?;
        let fingerprint = state.fingerprint;
        self.shared.set_tls(state);
        self.drop_connections();
        Ok(fingerprint)
    }

    // --- simulator ------------------------------------------------------------

    /// Stops the real-time simulator.
    pub fn pause_simulation(&self) {
        self.world().sim.running = false;
    }

    /// Starts (or resumes) the real-time simulator.
    pub fn resume_simulation(&self) {
        self.world().sim.running = true;
    }

    /// Whether the real-time simulator runs.
    pub fn simulation_running(&self) -> bool {
        self.world().sim.running
    }

    /// Runs `ticks` simulator ticks now (whether paused or not).
    pub fn step_simulation(&self, ticks: u64) {
        let mut world = self.world();
        for _ in 0..ticks {
            world.sim_tick();
        }
    }

    /// Ticks simulated so far.
    pub fn simulation_tick(&self) -> u64 {
        self.world().sim.tick
    }

    // --- checks and bursts ----------------------------------------------------------

    /// A burst: re-checks every host and service at once, like a forced
    /// `reschedule-check` of everything or an Icinga restart. Each object
    /// gets a new check result (its current state again, with fresh
    /// timestamps), so `CheckResult` events (and `StateChange` for soft
    /// states) follow. The checker works through them at
    /// [`crate::MockConfig::check_rate`] per second (Icinga: about 5 000),
    /// so the 32 000 objects of the `large` scenario take about 6.5 s.
    /// Returns how many checks were queued; objects already waiting are
    /// checked once.
    pub fn burst(&self) -> usize {
        let queued = self.world().queue_all_checks();
        tracing::debug!(queued, "burst: re-checking every object");
        queued
    }

    /// Checks waiting for the checker (from bursts and forced re-checks).
    pub fn queued_checks(&self) -> usize {
        self.world().checks.len()
    }

    /// Changes how many queued checks the checker runs per second.
    ///
    /// # Errors
    /// [`MockError::InvalidConfig`] unless `per_second` is a positive
    /// number.
    pub fn set_check_rate(&self, per_second: f64) -> Result<(), MockError> {
        let rate = crate::server::check_rate(per_second)?;
        self.world().checks.set_rate(rate);
        Ok(())
    }

    /// Runs up to `max` queued checks now, ignoring the rate (for tests
    /// that don't want to wait). Returns how many ran.
    pub fn run_queued_checks(&self, max: usize) -> usize {
        self.world().run_queued_checks(max)
    }

    /// Waits until no check is queued any more; `false` on timeout.
    pub async fn wait_for_queued_checks(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.queued_checks() == 0 {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // --- time -------------------------------------------------------------------

    /// Moves the mock's clock forward and runs the timers (see
    /// [`Self::run_timers`]).
    pub fn advance_clock(&self, by: Duration) {
        let mut world = self.world();
        world.clock_offset += by.as_secs_f64();
        world.housekeeping();
        world.run_queued_checks(usize::MAX);
    }

    /// Runs the timers now: downtimes starting and expiring,
    /// acknowledgements and comments expiring, command executions finishing,
    /// and every check that is due, including everything still queued from
    /// a burst (without waiting for the checker's rate).
    pub fn run_timers(&self) {
        let mut world = self.world();
        world.housekeeping();
        world.run_queued_checks(usize::MAX);
    }

    /// Sets `program_start`, as if the Icinga process had restarted at
    /// `at`, without dropping any connection: clients polling `/v1/status`
    /// see the new start time. (A real restart also drops every connection;
    /// add [`Self::drop_connections`] for that.)
    pub fn set_program_start(&self, at: Timestamp) {
        self.world().app.program_start = at.as_unix_seconds();
    }

    /// The mock's current time.
    pub fn now(&self) -> Timestamp {
        Timestamp::from_unix_seconds(self.world().now())
    }

    // --- requests -----------------------------------------------------------------

    /// The requests received so far (oldest first).
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.requests()
    }

    /// Forgets the recorded requests.
    pub fn clear_requests(&self) {
        self.shared.clear_requests();
    }

    // --- snapshots ------------------------------------------------------------------

    /// All hosts.
    pub fn hosts(&self) -> Vec<Host> {
        self.world().hosts_snapshot()
    }

    /// One host.
    pub fn host(&self, name: &str) -> Option<Host> {
        let world = self.world();
        world.hosts.get(name).map(|h| world.host_snapshot(h))
    }

    /// All services.
    pub fn services(&self) -> Vec<Service> {
        self.world().services_snapshot()
    }

    /// One service.
    pub fn service(&self, host: &str, name: &str) -> Option<Service> {
        let world = self.world();
        world
            .services
            .get(host)
            .and_then(|services| services.get(name))
            .map(|s| world.service_snapshot(s))
    }

    /// All comments.
    pub fn comments(&self) -> Vec<Comment> {
        self.world().comments_snapshot()
    }

    /// All downtimes.
    pub fn downtimes(&self) -> Vec<Downtime> {
        self.world().downtimes_snapshot()
    }

    /// Icinga's own notifications (`Notification` objects), by name.
    pub fn notifications(&self) -> Vec<ic_model::Notification> {
        self.world().notifications_snapshot()
    }

    /// All host groups.
    pub fn host_groups(&self) -> Vec<HostGroup> {
        self.world().host_groups_snapshot()
    }

    /// All service groups.
    pub fn service_groups(&self) -> Vec<ServiceGroup> {
        self.world().service_groups_snapshot()
    }

    /// All dependencies.
    pub fn dependencies(&self) -> Vec<Dependency> {
        self.world().dependencies_snapshot()
    }

    /// All endpoints.
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.world().endpoints_snapshot()
    }

    /// The instance status (`/v1/status` in model terms).
    pub fn status(&self) -> InstanceStatus {
        self.world().status_snapshot()
    }

    /// An object's attributes exactly as `/v1/objects` returns them (in
    /// the server's [`crate::NumberFormat`]), by Icinga type name (`Host`,
    /// `Service`, `Comment`, ...) and full name.
    pub fn object_attrs(&self, type_name: &str, name: &str) -> Option<serde_json::Value> {
        let kind = ObjKind::from_type_name(type_name)?;
        let world = self.world();
        let object = ObjRef {
            kind,
            name: name.to_owned(),
        };
        if !world.exists(kind, name) {
            return None;
        }
        let mut attrs = serde_json::Value::Object(world.object_attrs(&object, None).ok()?);
        crate::json::normalize(&mut attrs, self.shared.number_format);
        Some(attrs)
    }
}
