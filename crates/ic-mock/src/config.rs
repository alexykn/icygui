//! Configuration of a mock server: scenario, users, TLS, simulation, wire
//! details.

use std::fmt;
use std::time::Duration;

use crate::scenario::Scenario;
use crate::tls::TlsMaterial;

/// Everything [`crate::MockServer::start`] needs.
#[derive(Clone, Debug)]
pub struct MockConfig {
    /// The objects to serve.
    pub scenario: Scenario,
    /// TCP port on 127.0.0.1; `0` picks a free one.
    pub port: u16,
    /// API users (`ApiUser` objects). An empty list means nobody can log in.
    pub users: Vec<MockUser>,
    /// The simulator.
    pub simulation: SimulationConfig,
    /// Certificates.
    pub tls: MockTls,
    /// `ApiListener.enforce_filter_expression_permission`: whether `filter`
    /// parameters need the `filter-expression` permission. Icinga's default
    /// is `true`; `*` grants it.
    pub enforce_filter_expression_permission: bool,
    /// How numbers are written (see [`NumberFormat`]).
    pub number_format: NumberFormat,
    /// Events buffered per event stream before a slow consumer is
    /// disconnected (the server never blocks on a stream).
    pub event_buffer: usize,
    /// Delay between a `reschedule-check` and the check result it causes.
    pub reschedule_delay: Duration,
    /// How many requests [`crate::MockControl::requests`] keeps (oldest are
    /// dropped first).
    pub max_recorded_requests: usize,
    /// How often timers run: fixed downtimes starting, downtimes, comments
    /// and acknowledgements expiring, rescheduled checks executing.
    pub housekeeping_interval: Duration,
}

impl Default for MockConfig {
    /// The `lab` scenario on a random port, user `root`/`icinga` with `*`,
    /// self-signed TLS, simulation off (step it manually or turn it on).
    fn default() -> Self {
        Self {
            scenario: crate::scenarios::lab(),
            port: 0,
            users: vec![MockUser::root()],
            simulation: SimulationConfig::default(),
            tls: MockTls::SelfSigned,
            enforce_filter_expression_permission: true,
            number_format: NumberFormat::Float,
            event_buffer: 10_000,
            reschedule_delay: Duration::from_millis(300),
            max_recorded_requests: 10_000,
            housekeeping_interval: Duration::from_millis(250),
        }
    }
}

impl MockConfig {
    /// The defaults with another scenario.
    #[must_use]
    pub fn with_scenario(scenario: Scenario) -> Self {
        Self {
            scenario,
            ..Self::default()
        }
    }
}

/// An `ApiUser` with Basic-auth credentials and permissions.
#[derive(Clone, PartialEq, Eq)]
pub struct MockUser {
    /// Login name.
    pub username: String,
    /// Password (never logged).
    pub password: String,
    /// Permission patterns as in Icinga (`*`, `objects/query/*`,
    /// `actions/acknowledge-problem`, `events/CheckResult`, ...), matched
    /// case-insensitively with `*` and `?` wildcards.
    pub permissions: Vec<String>,
    /// `client_cn`: authenticates TLS clients presenting a certificate with
    /// this common name, signed by the mock's CA.
    pub client_cn: Option<String>,
}

impl MockUser {
    /// A user with the given permissions.
    #[must_use]
    pub fn new(username: &str, password: &str, permissions: &[&str]) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            client_cn: None,
        }
    }

    /// `root` / `icinga` with every permission (`*`), Icinga's documentation
    /// default.
    #[must_use]
    pub fn root() -> Self {
        Self::new("root", "icinga", &["*"])
    }

    /// A read-only user: object queries, status and all event types, plus
    /// `filter-expression` (the client sends filters).
    #[must_use]
    pub fn read_only(username: &str, password: &str) -> Self {
        Self::new(
            username,
            password,
            &[
                "objects/query/*",
                "status/query",
                "events/*",
                "filter-expression",
            ],
        )
    }

    /// Also authenticate clients whose certificate has this common name.
    #[must_use]
    pub fn with_client_cn(mut self, cn: &str) -> Self {
        self.client_cn = Some(cn.to_owned());
        self
    }
}

impl fmt::Debug for MockUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockUser")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("permissions", &self.permissions)
            .field("client_cn", &self.client_cn)
            .finish()
    }
}

/// Which certificates the server presents.
#[derive(Clone, Debug, Default)]
pub enum MockTls {
    /// A self-signed certificate for the node name, `localhost`,
    /// `127.0.0.1` and `::1`.
    #[default]
    SelfSigned,
    /// A CA ("Icinga CA", like Icinga's own) and a leaf for the node name,
    /// `localhost`, `127.0.0.1` and `::1` signed by it. Clients verify with
    /// [`crate::MockServer::ca_pem`]. Client certificates signed by this CA
    /// are accepted (see [`MockUser::client_cn`]).
    CaSigned,
    /// Fixed material, e.g. from an earlier [`crate::MockServer::tls_material`]
    /// to restart with the same fingerprint.
    Provided(TlsMaterial),
}

/// How JSON numbers are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NumberFormat {
    /// Every number with a fractional part (`200.0`), like Icinga up to
    /// 2.12 and the API documentation's examples.
    #[default]
    Float,
    /// Integral values without a fractional part (`200`), like Icinga 2.13+.
    Integral,
}

/// The simulator: churn, flapping, outages, downtimes and storms on a
/// seeded, deterministic schedule.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent feature switches of the simulator"
)]
pub struct SimulationConfig {
    /// Start the real-time driver right away. Off by default for the
    /// library (tests step the simulator); the binary turns it on.
    pub enabled: bool,
    /// Seed of every random decision. Same seed and tick sequence give the
    /// same events.
    pub seed: u64,
    /// Real-time speed factor: `2.0` runs ticks twice as often. Check
    /// intervals are counted in ticks, so everything happens faster.
    pub speed: f64,
    /// Simulated time per tick (at speed 1 also the real time per tick).
    pub tick: Duration,
    /// Run the active checker: objects are re-checked at their
    /// `check_interval` (`retry_interval` while soft), producing
    /// `CheckResult` events like a real Icinga.
    pub checks: bool,
    /// Expected number of new problems per hour of simulated time.
    pub problems_per_hour: f64,
    /// Average time a simulated problem lasts before it recovers.
    pub problem_duration: Duration,
    /// Occasionally make objects flap.
    pub flapping: bool,
    /// Occasionally take down a parent host so its dependents become
    /// unreachable, then recover it.
    pub outages: bool,
    /// Occasionally schedule short maintenance downtimes.
    pub downtimes: bool,
    /// Problem storms.
    pub storm: Option<StormConfig>,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            seed: 1,
            speed: 1.0,
            tick: Duration::from_secs(1),
            checks: true,
            problems_per_hour: 30.0,
            problem_duration: Duration::from_mins(20),
            flapping: true,
            outages: true,
            downtimes: true,
            storm: None,
        }
    }
}

impl SimulationConfig {
    /// Running in real time from the start, with `seed`.
    #[must_use]
    pub fn running(seed: u64) -> Self {
        Self {
            enabled: true,
            seed,
            ..Self::default()
        }
    }
}

/// A burst of problems: `size` services go critical within a few ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StormConfig {
    /// A storm starts every this many ticks.
    pub every_ticks: u64,
    /// Number of services that fail.
    pub size: usize,
    /// Ticks until they recover.
    pub duration_ticks: u64,
}

impl Default for StormConfig {
    fn default() -> Self {
        Self {
            every_ticks: 900,
            size: 30,
            duration_ticks: 120,
        }
    }
}
