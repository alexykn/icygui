# Architecture contracts

This is the binding contract between crates. `PLAN.md` explains the product and the reasons for these decisions; this file says exactly what each crate exposes and how it must behave. When code and this file disagree, fix one of them in the same change.

Already implemented and binding:
- `ic-model`: all domain types (names, timestamps, perfdata, objects, severity, events, actions, instance status).
- `ic-rules`: settings and intent types (`settings.rs`, `intent.rs`). The engine is still to be built.
- `ic-config`: data types (`model.rs`). Persistence is still to be built.
- `ic-core`: `ports.rs` (`SecretStore`, `Notifier`, `Clock`) and `snapshot.rs` (`Snapshot`, `DashboardResult`, `DashboardRow`, `Summary`). The runtime is still to be built.

Read the existing code before implementing against it. Don't change these types without a strong reason; if you must, explain the change in your report.

## Rules for every crate

- **Lints:** `cargo clippy -p <crate> --all-targets -- -D warnings` must pass with the workspace lints (`clippy::pedantic` plus the restriction set in the root `Cargo.toml`).
  - No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `println!` or `dbg!` outside tests.
  - Exceptions use `#[expect(lint, reason = "…")]`.
  - Every public item has a doc comment. Fallible public functions have `# Errors` sections.
- **Formatting:** `cargo fmt` (edition 2024 style).
- **Errors:** `thiserror` enums per crate; no `anyhow` in libraries.
- **Logging:** `tracing`. Never log secrets (passwords, keys, auth headers).
- **Dependencies:** don't edit the root `Cargo.toml`. Use `x.workspace = true` for dependencies already in `[workspace.dependencies]`. Declare anything new in your crate's `Cargo.toml` with an explicit version; it gets hoisted into the workspace when merged. Prefer well-maintained crates; mind licences (MIT/Apache/BSD/ISC/Zlib/MPL ok; no GPL).
- **Tests:** real unit tests in the crate, plus integration tests under `tests/` where useful. Test behaviour, edge cases and failure paths, not just happy paths.
- **No I/O** in `ic-model`, `ic-filter` or `ic-rules`. `ic-config` only touches the filesystem.
- **Only runtime operations** go to Icinga: never `objects/modify`, config packages, object creation or deletion, console, or process restart (PLAN.md D6).

Reference material lives outside the repo (Icinga's sources are GPL, so they aren't committed), in `/tmp/claude-0/-home-claude-repo/ae45fc2b-efd3-54e2-9623-951b98f290f3/scratchpad/ref/`:
- Icinga 2 docs: `12-icinga2-api.md`, `09-object-types.md`, `08-advanced-topics.md`, `17-language-reference.md`, `18-library-reference.md`, `19-technical-concepts.md`
- Icinga 2 sources (`icinga2-*.cpp/.ti/.yy/.ll`): API handlers, event JSON (`icinga2-apievents.cpp`), actions (`icinga2-apiactions.cpp`), filter-language grammar and operators (`icinga2-config-config_parser.yy`, `icinga2-config-config_lexer.ll`, `icinga2-config-expression.cpp`, `icinga2-base-value-operators.cpp`), built-in functions (`icinga2-base-scriptutils.cpp`, `*-script.cpp`), object attributes (`*.ti`)
- Icinga Web's perfdata and severity code (`iw-*.php`)

---

## ic-filter

Parses and evaluates the subset of the Icinga 2 DSL used in API filters and dashboard filters. It's used by the dashboards and notification rules (via `ic-core`) and by `ic-mock` (to evaluate API `filter` parameters).

```rust
pub struct Filter { /* parsed AST + source */ }
impl Filter {
    pub fn parse(source: &str) -> Result<Filter, ParseError>;   // empty/whitespace source = matches everything
    pub fn source(&self) -> &str;
    pub fn evaluate(&self, scope: &dyn Scope) -> Result<Value, EvalError>;
    pub fn matches(&self, scope: &dyn Scope) -> bool;            // evaluate → truthiness; errors → false
}
pub struct ParseError { pub message: String, pub offset: usize }  // byte offset for the editor's error marker
pub struct EvalError { pub message: String }

pub enum Value { Null, Bool(bool), Number(f64), String(Arc<str>), Array(Arc<Vec<Value>>), Dict(Arc<BTreeMap<String, Value>>) }
impl Value { pub fn is_truthy(&self) -> bool; pub fn from_json(&serde_json::Value) -> Value; }

/// Resolves variables. `path` is a root name followed by constant member
/// names: `host.vars.role` → ["host", "vars", "role"]. Return the value at the
/// deepest prefix you can resolve; the evaluator indexes the rest.
pub trait Scope { fn lookup(&self, path: &[&str]) -> Option<Value>; }

pub struct HostScope<'a> { pub host: &'a Host }
pub struct ServiceScope<'a> { pub service: &'a Service, pub host: Option<&'a Host> }
pub struct VarsScope<'a> { pub vars: &'a BTreeMap<String, Value> }       // API filter_vars
pub struct Chain<'a> { pub scopes: &'a [&'a dyn Scope] }                  // first match wins
```

**Object attributes:** these must be resolvable under `host.` and `service.`, with Icinga's names and value types (numbers are numbers):
- identity: `name`, `display_name`, `__name` (`host` / `host!service`), `host_name` (services), `address`, `address6` (hosts)
- state: `state` (host 0/1, service 0–3), `state_type`, `last_state_change`, `last_hard_state_change`, `last_check`, `next_check`, `check_attempt`, `max_check_attempts`, `acknowledgement`, `acknowledgement_expiry`, `downtime_depth`, `flapping`, `flapping_current`, `last_reachable`, `problem`, `handled`, `severity`
- check config: `check_command`, `check_interval`, `retry_interval`, `command_endpoint`, `zone`, `enable_*`
- `groups`, `vars`, `notes`, `notes_url`, `action_url`, `icon_image`
- `last_check_result` (dict with `output`, `exit_status`, `state`, `execution_start`, `execution_end`, `schedule_start`, `check_source`, `active`)

Pending objects have `last_check_result = null` and `state = 0`; unreachable hosts have `state = 1` and `last_reachable = false`, like Icinga. `ServiceScope` resolves `host.*` from its host.

**Language subset:** follow the Icinga grammar and semantics in the reference sources.
- literals: numbers, durations (`5m`, `1h`, `30s`, `2d`, `500ms`), strings with escapes, `{{{ }}}` multi-line strings, `true`, `false`, `null`, arrays `[…]`, dictionaries `{ key = value }`
- operators: `!`, unary `-`, `~`, `*`, `/`, `%`, `+`, `-`, `<<`, `>>`, `<`, `>`, `<=`, `>=`, `==`, `!=`, `in`, `!in`, `&`, `^`, `|`, `&&`, `||`, with Icinga's precedence and short-circuit; member access `.`, indexers `[…]`, parentheses
- functions: `match(pattern, value[, MatchAll|MatchAny])` (glob `*` `?`, arrays), `regex(pattern, value[, mode])`, `cidr_match(pattern, ip[, mode])`, `len`, `typeof`, `string`, `number`, `bool`, `intersection`, `union`, `range`, `get_time`
- methods: string `contains`, `find`, `len`, `lower`, `upper`, `trim`, `split`, `substr`, `starts_with`, `ends_with`; array `contains`, `len`, `join`; dict `contains`, `get`, `keys`, `len`

**Semantics:**
- equality, ordering, `+` on strings, `in` on arrays, and truthiness follow `value-operators.cpp`;
- unknown variables and missing keys are `null`, not errors (dashboard filters must not blow up on hosts lacking a var);
- calling an unknown function is an `EvalError`;
- assignments, loops, function definitions and other statements are parse errors with a clear message ("only expressions are supported in filters").

**Performance:** compile glob and regex patterns that are string literals once, at parse time. Evaluating a typical dashboard filter over 20 000 services must take well under 50 ms in release builds; add a benchmark-style test with a generous bound, ignored by default.

---

## ic-config

The data types are done (`model.rs`). To be built:

```rust
pub struct Paths { pub config_file: PathBuf, pub data_dir: PathBuf, pub log_dir: PathBuf }
impl Paths {
    pub fn from_system() -> Result<Paths, ConfigError>;   // directories::ProjectDirs("io.github", "alexykn", "icygui")
    pub fn in_dir(root: &Path) -> Paths;                   // tests and portable mode
}
pub struct ConfigStore { /* path */ }
impl ConfigStore {
    pub fn new(path: PathBuf) -> Self;
    pub fn load(&self) -> Result<Config, ConfigError>;    // missing file → Config::default(); runs migrations
    pub fn save(&self, config: &Config) -> Result<(), ConfigError>;   // atomic: temp file in same dir, fsync, rename; keeps one `.bak`
}
pub fn migrate(raw: toml::Table) -> Result<Config, ConfigError>;     // version 0/absent → 1, future versions → error
impl Config {
    pub fn validate(&self) -> Vec<ValidationIssue>;       // ids unique, URLs are https with host, names non-empty, …
    pub fn environment(&self, id: &str) -> Option<&Environment>;
    pub fn environment_mut(&mut self, id: &str) -> Option<&mut Environment>;
}
impl Environment {
    pub fn new(name: &str, url: &str, auth: AuthConfig) -> Environment;   // fresh UUID, default dashboards
    pub fn author_name(&self) -> &str;                    // author or the basic-auth username
    pub fn dashboard(&self, group_id: &str, dashboard_id: &str) -> Option<&Dashboard>;
}
pub fn default_groups() -> Vec<DashboardGroup>;           // "overview": "problems" (services, problems_only, hide_handled), "host problems", "all services"
pub fn new_id() -> String;                                // UUID v4
pub fn export_groups(groups: &[DashboardGroup]) -> Result<String, ConfigError>;     // TOML for sharing
pub fn import_groups(text: &str) -> Result<Vec<DashboardGroup>, ConfigError>;       // fresh ids on import
pub fn parse_fingerprint(text: &str) -> Result<[u8; 32], ConfigError>;              // "AB:CD:…" or hex
```

- File permissions: `0600` for the config file on Unix.
- Unknown keys in the file are ignored without failing the load.
- A corrupt file is reported, not silently replaced: `load` returns an error and the app offers to restore the `.bak` copy or start fresh.
- Tests: round trips, migrations, atomicity (an interrupted write leaves the old file intact), validation, import/export, path layout.

---

## ic-rules (engine)

The types are done (`settings.rs`, `intent.rs`). To be built:

```rust
pub struct RuleSet { pub environment_name: String, pub settings: NotificationSettings, pub groups: Vec<GroupScope> }
pub struct GroupScope { pub id: String, pub name: String, pub setting: ScopeSetting, pub dashboards: Vec<DashboardScope> }
pub struct DashboardScope { pub id: String, pub name: String, pub setting: ScopeSetting }

pub struct RuleEngine { /* rules, pending delays, dedupe, storm window, notified problems, pause */ }
impl RuleEngine {
    pub fn new(rules: RuleSet) -> Self;
    pub fn set_rules(&mut self, rules: RuleSet);
    pub fn pause_until(&mut self, until: Option<Timestamp>);   // paused → intents are silent
    pub fn paused_until(&self) -> Option<Timestamp>;
    pub fn on_input(&mut self, input: RuleInput, now: Timestamp, local: LocalTime) -> Vec<NotificationIntent>;
    pub fn tick(&mut self, now: Timestamp, local: LocalTime) -> Vec<NotificationIntent>;   // due delayed intents, storm summaries
}
```

**Scope resolution:**
1. Start at the environment: `(settings.enabled, settings.default_rule)`.
2. A group's setting applies to that pair: `Inherit` keeps it, `Off` disables it, `On` enables it with the parent rule, `Custom(r)` enables it with `r`.
3. A dashboard's setting applies the same way to its group's result.
4. With memberships, the change notifies if *any* membership's effective rule is enabled and matches. The subtitle names the first matching `group / dashboard`.
5. Without memberships, the environment pair applies and the subtitle is the environment name.
6. Object overrides come first: `Mute` (until expiry) suppresses everything for the object; `Watch` notifies with the environment's default rule even if every scope is off.

**Matching:**
- *States:*
  - `StateFilter` picks which states notify. `recovery` covers the change to OK/UP, and only notifies for objects that had a *notified* problem.
  - `hard_only` ignores soft states.
  - `skip_handled` ignores changes where `input.handled` is true.
- *Other events:* `EventFilter` enables acknowledgement, downtime and flapping changes.
- *Delays:* `min_duration_secs` holds a problem notification until `since + min_duration`. `tick` releases it if the object is still in that state and unhandled. A recovery, a handling change or another state change cancels it.

**Output:**
- Intent ids are stable: `"{object}:{state}:{since}"` for state changes, similar for other kinds. The same id is never emitted twice (dedupe across memberships, repeated inputs and reconnect replays). Bound the dedupe memory, e.g. 10 000 recent ids.
- *Storm:* when more than `storm.threshold` intents fall inside `storm.window_secs`, further ones in that window are emitted with `silent = true`. When the window closes, `tick` emits one summary intent ("14 new problems in prod-cluster"; `object = None`, `tone = Info`, not silent).
- *Quiet hours:* intents are `silent` inside the window (local time; windows may cross midnight; `days` is keyed by the start day). With `allow_critical`, critical and down states stay audible.
- *Pause:* intents are `silent` while paused.
- *Text:*
  - title: `"{LABEL} · {service} on {host}"` for services or `"{LABEL} · {host}"` for hosts. LABEL is CRITICAL, WARNING, UNKNOWN, DOWN, UNREACHABLE, RECOVERED, ACKNOWLEDGED, DOWNTIME, FLAPPING …, using display names.
  - body: the output's first line, or the comment.
  - tone: by state.
  - sound: from the rule.

Test everything above with scenario tests (sequences of inputs and ticks with explicit timestamps).

---

## ic-api

Async Icinga 2 REST client (tokio + reqwest with rustls). Maps wire JSON into `ic-model` types; wire structs stay private.

```rust
pub struct ConnectionSettings {
    pub base_url: Url,                         // https://host:5665 (the client appends /v1/…)
    pub credentials: Credentials,
    pub tls: TlsSettings,
    pub request_timeout: Duration,             // default 30 s; connect timeout 10 s
}
pub enum Credentials {
    Basic { username: String, password: SecretString },
    ClientCertificate { cert_pem: Vec<u8>, key_pem: SecretBox<Vec<u8>> },
}
pub struct TlsSettings {
    pub ca_pem: Option<Vec<u8>>,               // extra trusted CAs (Icinga's CA)
    pub pinned_sha256: Option<[u8; 32]>,       // accept exactly this leaf certificate (skips chain and name checks)
    pub server_name: Option<String>,           // verify against this name instead of the URL host
    pub use_system_roots: bool,
}

#[derive(Clone)] pub struct Client { … }
impl Client {
    pub fn new(settings: ConnectionSettings) -> Result<Client, ApiError>;
    pub async fn info(&self) -> Result<ApiInfo, ApiError>;               // GET /v1 with `Accept: application/json` exactly: user, permissions, version
    pub async fn status(&self) -> Result<InstanceStatus, ApiError>;     // /v1/status/IcingaApplication + /v1/status/CIB
    pub async fn hosts(&self) -> Result<Vec<Host>, ApiError>;                    // full attributes (hosts are few)
    pub async fn services(&self, detail: Detail) -> Result<Vec<Service>, ApiError>;  // Detail::Lean for the initial load (docs/performance.md)
    pub async fn comments(&self) -> Result<Vec<Comment>, ApiError>;
    pub async fn downtimes(&self) -> Result<Vec<Downtime>, ApiError>;
    pub async fn host_groups(&self) -> Result<Vec<HostGroup>, ApiError>;
    pub async fn service_groups(&self) -> Result<Vec<ServiceGroup>, ApiError>;
    pub async fn dependencies(&self) -> Result<Vec<Dependency>, ApiError>;
    pub async fn endpoints(&self) -> Result<Vec<Endpoint>, ApiError>;
    pub async fn objects(&self, keys: &[ObjectKey], detail: Detail) -> Result<Fetched, ApiError>;   // by name, ≤ 200 per request; a 404 batch is bisected to find deleted names
    pub async fn run_action(&self, action: &Action, target: &ActionTarget, author: &str) -> Result<Vec<ActionResult>, ApiError>;
    pub async fn events(&self, queue: &str, kinds: &[EventKind]) -> Result<EventStream, ApiError>;
}
pub async fn fetch_server_certificate(base_url: &Url, server_name: Option<&str>) -> Result<CertificateInfo, ApiError>;  // trust on first use: sha256, subject, issuer, not_after; accepts any cert, only reads it

pub enum Detail {
    Lean,   // state, state_type, last_state_change, last_hard_state_change, last_check, next_check, next_update, check_attempt, max_check_attempts,
            // acknowledgement(+expiry), downtime_depth, flapping, last_reachable, check_interval, retry_interval, groups, vars, display_name, host_name
    Full,   // Lean + last_check_result, check_command, command_endpoint, zone, enable_*, flapping_current, notes, notes_url, action_url, icon_image
}
pub struct Fetched { pub hosts: Vec<Host>, pub services: Vec<Service>, pub missing: Vec<ObjectKey> }   // missing = deleted in Icinga
pub struct ApiInfo { pub user: String, pub permissions: Vec<String>, pub version: String }
impl ApiInfo { pub fn allows(&self, permission: &str) -> bool; }       // Icinga wildcard semantics ("*", "actions/*", "objects/query/*"); "(filtered)" entries count as allowed
pub struct ActionResult { pub code: u16, pub status: String, pub name: Option<String> }
pub struct EventStream { … }                                            // impl Stream<Item = Result<Event, ApiError>>; ends on disconnect
pub enum ApiError {
    Connect(String), Tls(String), CertificateMismatch { expected: String, actual: String },
    Unauthorized, Forbidden(String), NotFound(String), Http { status: u16, message: String },
    Decode(String), Timeout, InvalidSettings(String),
}
impl ApiError { pub fn is_transient(&self) -> bool; }                 // connect/timeout/5xx → retry with backoff
```

**Wire rules:** verify against the docs, the handler sources and the **real recorded samples in `contract/samples/`** (Icinga 2.15.6). The samples win when they disagree with the docs.
- *Queries:* `POST /v1/objects/<type>` with `X-HTTP-Method-Override: GET`, `Accept: application/json` and a JSON body `{ "attrs": [...] }`, plus `"hosts"`/`"services"` name arrays for targeted queries (never `filter`). Request only the attributes the model needs. Responses are `{ "results": [ { "name", "type", "attrs": {…}, "joins": {}, "meta": {} } ] }`.
- *Numbers:* Icinga writes doubles with integral values as JSON *integers* (`"state": 2`) and others as floats; accept both everywhere and never panic on odd input. In *events*, `acknowledgement` is a boolean; on *objects* it is 0/1/2.
  - `last_check_result.exit_status` stays 0 for passive results: take the state from `state`, never from `exit_status`.
  - Pending is `last_check_result == null`.
  - Host `state` 1 with `last_reachable == false` is `HostState::Unreachable`.
  - Output: split `last_check_result.output` into first line and long output.
  - `performance_data` entries are strings (parse with `ic_model::parse_perfdata_entry`) or `PerfdataValue` dictionaries (`label`, `value`, `unit`, `warn`, `crit`, `min`, `max`).
  - Timestamps of 0 mean never.
- *Lean objects* have no `last_check_result`: `check.result` is `None`. If `last_check < 0` the object is pending (`ServiceState::Pending`/`HostState::Pending`); otherwise its state is known and only the output isn't loaded yet. Services: `state` 0–3; hosts: `state` 0/1 with `last_reachable`.
- *`CheckResult` events* carry `check_result.vars_after` (`state`, `state_type`, `attempt`, `reachable`) plus `downtime_depth` and `acknowledgement` (a boolean in events). Map them into the event so `ic-core` can update the object without a re-query. For hosts, `vars_after.state` is a *service-style* state (0/1 = up, 2/3 = down; see `Host::CalculateState`).
- *Comments and downtimes:* the object comes from `host_name` / `service_name` (empty string = host).
  - `Downtime.in_effect`: use `is_in_effect` when present, otherwise compute it from fixed/flexible, start/end and `trigger_time`.
  - `config_owned`: `config_owner` is non-empty, or `scheduled_by` is non-empty.
- *Targeting (verified against a real Icinga 2.15 with `enforce_filter_expression_permission = true`, see `contract/`):* never send `filter` expressions. From Icinga 2.17 they need the `filter-expression` permission (`403 Missing permission: filter-expression`).
  - Target objects by name: `"hosts": [...]` (type `Host`) or `"services": ["host!service", ...]` (type `Service`). This works for both queries and actions without extra permissions.
  - A name list that contains one unknown name fails the whole request with `404 No objects found.` On a 404 for a batch, retry each name individually and treat the 404s as deleted objects.
- *Actions:* `POST /v1/actions/<name>` with `{ "type": "Host"|"Service", "hosts"|"services": [...], … }`.
  - Split mixed host/service targets into two requests.
  - `ActionTarget::Downtime(name)` uses `{ "downtime": name }`; `ActionTarget::Comment(name)` uses `{ "comment": name }`.
  - Parameter names and rules follow `12-icinga2-api.md`: `reschedule-check` sets `next_check` = now with `force`; `acknowledge-problem` always sends `notify: false`; `schedule-downtime` sends `fixed`, `duration`, `all_services`, `child_options`, `trigger_name`.
  - `execute-command` needs an `endpoint` unless the object has `command_endpoint` (otherwise the per-object result is `404 Can't find a valid endpoint`). Pass the endpoint explicitly, defaulting to the object's `command_endpoint` or the instance's node name.
  - A response with per-object `code >= 400` still returns `Ok`, and the caller inspects the results. HTTP-level errors map to `ApiError`.
- *Events:* `POST /v1/events` with `{ "queue": queue, "types": [...] }`. The response is newline-delimited JSON over a long-lived HTTP/1.1 response.
  - Parse incrementally (lines can span chunks).
  - Map each type per `icinga2-apievents.cpp`.
  - Skip unknown types and malformed lines with a warning; don't end the stream.
  - The stream ends when the connection closes.
  - No read timeout on the stream; enable TCP keepalive.
- *TLS:* rustls (ring provider).
  - Pinning: a custom verifier that compares the SHA-256 of the leaf DER and reports `CertificateMismatch` with both fingerprints, colon-hex uppercase.
  - Name override: verify against `server_name`.
  - Icinga's CA certificates often lack modern extensions; use webpki verification with the provided CA and don't add stricter policies.
- *Auth:* Basic auth, or a client certificate via the rustls client auth config.
- *Errors:* 401 → `Unauthorized`, 403 → `Forbidden` (include the response's `status` text), 404 → `NotFound`, other non-2xx → `Http`.
- *Tests:*
  - deserialisation tests from realistic JSON (doc examples plus pending, unreachable, perfdata dict and string forms, comments, downtimes);
  - an in-process HTTPS test server (hyper or axum with a self-signed certificate from `rcgen`) for pinning, CA trust, name override, auth headers, action bodies, event streaming split across chunks, and error mapping.
  - Integration against `ic-mock` comes in wave 2.

---

## ic-mock (wave 2)

See PLAN.md §3.6. `MockServer::start(MockConfig) -> MockServer` serves HTTPS on `127.0.0.1:<port>` with a self-signed certificate and Basic auth. It exposes `url()`, `cert_pem()`, `cert_sha256()`, `control() -> MockControl` and `shutdown()`. It has built-in scenarios (`prod_cluster`, `staging`, `lab`, `large(seed)`) and a seedable simulator.
- `large` matches production scale (docs/performance.md): 2 000 hosts × 15 services, realistic payload sizes, 5-minute intervals.
- Mass re-check bursts are available through `MockControl::burst` (every object, ~5 000 events/s).
- It honours `Detail`-style `attrs` selection and name lists exactly like Icinga, including the all-or-nothing 404. The binary is `icinga-mock`. It writes its own wire JSON straight from the docs and sources; it never uses `ic-api`'s types.

---

## ic-core (wave 3)

The UI-agnostic engine for the *active* environment (PLAN.md D2: one at a time). It owns a tokio runtime on a dedicated thread and talks to the UI only through `CoreHandle`.

```rust
pub struct EnvironmentSpec { pub environment: ic_config::Environment, pub general: ic_config::General, pub data_dir: PathBuf }
pub struct Ports { pub secrets: Arc<dyn SecretStore>, pub notifier: Arc<dyn Notifier>, pub clock: Arc<dyn Clock> }
pub struct SystemClock;                                        // impl Clock with the local time zone (jiff)

pub fn start(spec: EnvironmentSpec, ports: Ports) -> Result<CoreHandle, CoreError>;   // spawns the runtime thread
pub struct CoreHandle { … }
impl CoreHandle {
    pub fn send(&self, command: Command);                      // never blocks
    pub fn take_events(&mut self) -> Option<futures::channel::mpsc::UnboundedReceiver<CoreEvent>>;   // taken once by the UI bridge
    pub fn shutdown(self);                                     // stops streams, flushes the log, joins the thread (bounded wait)
}

/// Settings dialog helpers, runnable without a started environment.
/// They run on a shared background runtime and return a oneshot receiver
/// the UI can await.
pub fn test_connection(environment: ic_config::Environment, password: Option<SecretString>) -> oneshot::Receiver<Result<ConnectionReport, ConnectionFailure>>;
pub fn fetch_certificate(url: String, server_name: Option<String>) -> oneshot::Receiver<Result<CertificateInfo, String>>;
pub struct ConnectionReport { pub info: ApiInfo, pub status: InstanceStatus, pub missing_permissions: Vec<String> }
pub enum ConnectionFailure { Unauthorized, Tls { message: String, certificate: Option<CertificateInfo> }, CertificateMismatch { expected: String, actual: String }, Unreachable(String), Other(String) }

pub enum Command {
    Action { id: u64, target: ActionTarget, action: Action },
    Refresh,                                                   // full re-sync now (also retries a failed connection)
    UpdateEnvironment(ic_config::Environment),                 // dashboards/rules changed → re-evaluate; connection settings changed → reconnect
    UpdateGeneral(ic_config::General),
    PauseNotifications(Option<Timestamp>),
    LoadHistory { object: Option<ObjectKey>, limit: usize, reply: oneshot::Sender<Vec<LogEntry>> },
    LoadNotifications { limit: usize, reply: oneshot::Sender<Vec<NotificationRecord>> },
    MarkNotificationsRead,
    PreviewDashboard { view: ic_config::View, reply: oneshot::Sender<Result<DashboardResult, String>> },   // dashboard editor: live match count and rows
    Hydrate(Vec<ObjectKey>),                                   // fetch Full details for lean objects (visible rows, opened pane)
}
pub enum CoreEvent {
    Connection(ConnectionState),
    Snapshot(Arc<Snapshot>),
    Permissions(ApiInfo),
    ActionFinished { id: u64, outcome: ActionOutcome },
    Notification(NotificationRecord),                          // every intent, silent or not, after it is logged
    NotificationsPaused(Option<Timestamp>),
}
pub enum ConnectionState {
    Connecting { attempt: u32 },
    Loading { phase: LoadPhase, done: usize, total: Option<usize> },   // tiered initial load progress
    Connected { endpoint: String, version: String, since: Timestamp },
    Reconnecting { error: String, attempt: u32, retry_at: Timestamp },
    AuthFailed { message: String },                            // no automatic retry; Refresh or UpdateEnvironment retries
    TlsFailed { message: String, certificate: Option<CertificateInfo> },   // no automatic retry; offers trust-on-first-use
    MissingSecret,                                             // no password in the keychain
}
pub struct ActionOutcome { pub ok: usize, pub failed: Vec<(String, String)>, pub error: Option<String> }   // per-object failures, or a request error
pub struct LogEntry { pub at: Timestamp, pub object: ObjectKey, pub kind: LogKind, pub text: String, pub author: Option<String> }
pub enum LogKind { State { state: CheckableState, state_type: StateType }, AcknowledgementSet, AcknowledgementCleared, CommentAdded, CommentRemoved, DowntimeStarted, DowntimeEnded, FlappingStarted, FlappingStopped }
pub struct NotificationRecord { pub intent: NotificationIntent, pub read: bool }
```

The `Snapshot` contract type gains two fields; both are allowed additive changes:
- `last_event_at: Option<Timestamp>`, which the footer shows as "master-01 · 2s";
- `overall: Summary`, over all hosts and services, which drives the tray icon and its tooltip.

**Sync engine** (designed for 2 000 hosts / 30 000 services; numbers and reasoning in docs/performance.md):
1. Connect: build an `ic_api::Client` from the environment.
   - The password comes from `SecretStore` with account = environment id; `MissingSecret` if absent.
   - Client certificates are read from their files.
   - The CA file is read; the pin is parsed with `ic_config::parse_fingerprint`.
2. Tiered initial load, with no rule inputs. Publish a snapshot after each tier so the UI fills in progressively.
   1. `info`, `status`, groups, dependencies, endpoints, comments, downtimes and hosts (`Full`).
   2. Services (`Detail::Lean`).
   3. Every service in a problem state, `Full`, by name in batches.
   - Expose progress (`ConnectionState::Loading { phase, done, total }`).
3. Open the event stream (queue `icygui-<uuid>`, all `EventKind::ALL`). Split it into two tasks:
   - a *reader* that only reads lines into an unbounded channel, so Icinga's send buffer never backs up;
   - an *applier* that parses and applies in batches.
   - `CheckResult` updates the object completely (state, state type, attempt and reachability from `vars_after`; downtime depth; acknowledgement; output and perfdata). `next_check` is estimated from `execution_end` plus the check or retry interval.
   - Several `CheckResult`s for one object within a batch collapse to the last one. Other event types are never collapsed.
   - Objects are re-queried only for `ObjectCreated`/`ObjectModified`/`ObjectDeleted` and for unknown objects.
   - Required throughput: 50 000 recorded events applied in under 3 s; steady state about 110 events/s.
4. *Freshness watchdog:*
   - Each object's deadline is Icinga's `next_update`: `next_check` + interval + 2 × latency for active checks, last result + 2 × interval for passive ones.
   - It comes from the lean query and is recomputed from every `CheckResult` event, so manual checks by anyone move it.
   - Overdue objects are re-queried by name: batches ≤ 200, at most once per object per interval.
   - Objects Icinga still reports overdue get `late = true` in the snapshot (the UI shows "late").
5. *Hydration on demand:*
   - `Command::Hydrate(Vec<ObjectKey>)` asks for `Full` details of lean objects. The UI sends it, debounced, for visible rows without output and for an opened pane.
   - Batches hold at most 200 names and requests are deduplicated.
6. *Reconcile:* a lean reload (tiers 1–3) on connect, after every reconnect (with jitter) and on `Refresh`.
   - Periodically, adaptively: every 5 minutes below 5 000 objects, every 15 minutes above. `General.reconcile_interval_secs = 0` means adaptive; any other value overrides it.
   - Diff the reload against the store; state changes found only by the diff produce rule inputs, as do removals.
   - **Never** periodic full-attribute reloads: they cost the master around 1 GB of memory at this scale.
7. Poll status every 30 s. A changed `program_start` means Icinga restarted and triggers a reload.
8. Stream end or error → `Reconnecting` with exponential backoff and jitter (1 s → 60 s, reset after 5 minutes of health). Auth (401) → `AuthFailed`. TLS errors and pin mismatch → `TlsFailed` with the presented certificate (via `fetch_server_certificate`).

**Store and snapshots:**
- Objects are `Arc`-shared in `BTreeMap`s and copied on write.
- A snapshot is published at most every 250 ms while there are changes, and immediately after the initial load and after actions.
- `Snapshot.revision` increases monotonically.

**Dashboards:** evaluate per dashboard per snapshot, incrementally, on a blocking thread (`spawn_blocking`):
- Keep a compiled `ic_filter::Filter` per dashboard (recompiled when its source changes) and a per-dashboard match set.
- Re-evaluate only dirty objects; do a full re-evaluation when dashboards or the object set change wholesale (reload).
- A `Services` view evaluates `ServiceScope { service, host }`; a `Hosts` view evaluates `HostScope`.
- *Membership* (used for notifications) = filter matches, ignoring `problems_only` and `hide_handled`, so recoveries still match.
- *Rows:*
  - apply `problems_only`, then `hide_handled` (Icinga's handled: acknowledged, in downtime, or host problem for services);
  - sort per `Sort`; ties go to severity desc, then `last_state_change` desc, then host name, then service name;
  - `GroupBy` inserts `DashboardRow::Group` headers. Groups are ordered by their worst severity (desc), then label; with host groups and service groups an object appears under each of its groups, and objects without groups go under "ungrouped" last.
- *Summary* is computed over the matches *before* `hide_handled`.
- A filter error sets `DashboardResult.error`.
- Performance target: 20 000 services × 10 dashboards stays responsive. A full evaluation takes well under a second in release builds; incremental updates take milliseconds. Test this with the `large` mock scenario (ignored test, run in release).

**Notifications:**
- Build an `ic_rules::RuleSet` from the environment (environment name, settings, groups/dashboards with their `ScopeSetting`s) and rebuild it on `UpdateEnvironment`.
- For every applied change, produce a `RuleInput`:
  - `StateChange`: previous state from the store before applying, `since` = `last_state_change`;
  - `AcknowledgementSet`/`AcknowledgementCleared`;
  - `DowntimeStarted`/`DowntimeTriggered` → `DowntimeStarted`;
  - `DowntimeRemoved` of an in-effect downtime → `DowntimeEnded`;
  - `Flapping`.
  - `handled` is computed from the store after applying; `memberships` from the dashboard filters.
- Tick the engine every second.
- Every intent goes into the SQLite log, then `CoreEvent::Notification`; non-silent ones also go to `Notifier::notify`.

**Event log:** SQLite (rusqlite, bundled) at `<data_dir>/events-<environment id>.sqlite3`.
- WAL mode, schema version table.
- Tables `events` (at, object, kind, state, state_type, text, author) and `notifications` (id, at, object, title, subtitle, body, tone, silent, read).
- What gets logged: state changes (hard and soft), acknowledgements, user comments, downtime start/end, flapping. Not plain check results.
- Prune on start and hourly to `event_log_retention_hours`.
- All database work happens on blocking threads.

**Actions:**
- `Command::Action` runs `Client::run_action` with `author` = `Environment::author_name()`.
- It reports `ActionFinished` with per-object failures. For example, `execute-command`'s "Can't find a valid endpoint": by default pass the object's `command_endpoint`, else the instance's node name.
- After success it marks the targets dirty, so their new state shows within a second.

**Tests** (integration, against `ic_mock::MockServer` in-process, no sleeps beyond small bounded waits on channels):
- initial load → snapshot contents;
- event application (each event type) → snapshot;
- dirty re-query;
- reconcile diff (drop the stream, change state behind its back, reconnect) → rule input;
- backoff states;
- 401 → `AuthFailed`; pin mismatch → `TlsFailed` with the fingerprint; missing secret;
- every action end to end;
- dashboard evaluation (filters, sort, group-by, summary, hide_handled, problems_only, filter error);
- notifications end to end with a fake `Notifier` (including no notifications from the initial load);
- event log write, prune and query;
- shutdown joins cleanly;
- `test_connection` success, missing permissions and TLS failure.

## ic-platform

```rust
pub struct KeyringSecrets;                   // impl ic_core::ports::SecretStore, service "io.github.alexykn.icygui", account = environment id
pub mod tray {
    pub struct Tray { … }                    // must be created on the main thread (macOS); wraps tray-icon (ksni backend on Linux)
    pub enum TrayCommand { Open, PauseFor(Duration), Resume, SwitchEnvironment(String), Quit }
    impl Tray {
        pub fn new(app_name: &str) -> Result<Tray, PlatformError>;
        pub fn commands(&self) -> futures::channel::mpsc::UnboundedReceiver<TrayCommand>;   // take once
        pub fn set_state(&self, worst: Option<TrayTone>, tooltip: &str);   // coloured state dot icon, generated in code
        pub fn set_environments(&self, environments: &[(String, String)], active: Option<&str>);   // (id, name) submenu
        pub fn set_paused(&self, paused_until: Option<String>);   // menu shows "Paused until …" / Resume
    }
}
pub mod autostart {
    pub fn set_enabled(enabled: bool, app_id: &str, app_name: &str, exe: &Path) -> Result<(), PlatformError>;   // LaunchAgent plist / XDG autostart .desktop
    pub fn is_enabled(app_id: &str) -> bool;
}
```

## ic-ui-kit and ic-app

The UI renders `ic_core::snapshot::Snapshot` and `ic_config` types; it never talks HTTP or evaluates filters. All colours and sizes come from `ic_ui_kit::Theme`. The design is in `design/project/Icinga Client v2.dc.html` (calm v2 look; primary) and `design/project/Icinga Client.dc.html` (turn 1: palette, editor, sub-tabs). Pixel details are in PLAN.md §1.
