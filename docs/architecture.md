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

Implemented: the data types (`model.rs`) plus persistence, validation and sharing.

```rust
pub struct Paths { pub config_file: PathBuf, pub data_dir: PathBuf, pub log_dir: PathBuf }
impl Paths {
    pub fn from_system() -> Result<Paths, ConfigError>;   // directories::ProjectDirs("io.github", "alexykn", "icygui"); logs: ~/Library/Logs/io.github.alexykn.icygui (macOS), $XDG_STATE_HOME/icygui/logs (Linux)
    pub fn in_dir(root: &Path) -> Paths;                   // tests and portable mode: <root>/config.toml, <root>/data, <root>/logs
    pub fn config_store(&self) -> ConfigStore;
    pub fn create_dirs(&self) -> Result<(), ConfigError>;  // new directories are 0700 on Unix
}
pub struct ConfigStore { /* path */ }
impl ConfigStore {
    pub fn new(path: PathBuf) -> Self;
    pub fn path(&self) -> &Path;
    pub fn backup_path(&self) -> PathBuf;                 // "<file name>.bak" next to the file
    pub fn load(&self) -> Result<Config, ConfigError>;    // missing file → Config::default(); runs migrations; repairs ids (below)
    pub fn load_backup(&self) -> Result<Option<Config>, ConfigError>;   // the `.bak` copy, for "restore"; never writes
    pub fn save(&self, config: &Config) -> Result<(), ConfigError>;   // atomic: temp file in same dir, fsync, rename; keeps one `.bak`
}
pub fn migrate(raw: toml::Table) -> Result<Config, ConfigError>;     // version 0/absent → 1, future versions → error
impl Config {
    pub fn validate(&self) -> Vec<ValidationIssue>;       // ids unique, URLs are https with host, names non-empty, …
    pub fn environment(&self, id: &str) -> Option<&Environment>;
    pub fn environment_mut(&mut self, id: &str) -> Option<&mut Environment>;
    pub fn repair_ids(&mut self) -> usize;                // fresh ids for blank or duplicate ones; returns how many changed
}
impl Environment {
    pub fn new(name: &str, url: &str, auth: AuthConfig) -> Environment;   // fresh UUID, default dashboards, trusts the system roots
    pub fn author_name(&self) -> &str;                    // author (unless blank) or the basic-auth username; "" for a client certificate without author
    pub fn group(&self, group_id: &str) -> Option<&DashboardGroup>;      // + group_mut
    pub fn dashboard(&self, group_id: &str, dashboard_id: &str) -> Option<&Dashboard>;   // + dashboard_mut
    pub fn api_url(&self) -> Result<Url, ConfigError>;    // checked like validate(); the path always ends in "/", so join("v1/…") keeps a proxy prefix
    pub fn validate(&self) -> Vec<ValidationIssue>;       // paths relative to the environment, for the environment editor
}
impl TlsConfig { pub fn pinned_fingerprint(&self) -> Result<Option<[u8; 32]>, ConfigError>; }
impl DashboardGroup { pub fn new(name: &str) -> Self; /* + dashboard, dashboard_mut */ }   // fresh id, ScopeSetting::Inherit
impl Dashboard { pub fn new(name: &str, view: View) -> Self; }                            // fresh id, ScopeSetting::Inherit
pub struct ValidationIssue { pub path: String, pub message: String }   // "environments[0].tls.pinned_sha256" / "must not be empty"; Display "path: message"
pub const MIN_EVENT_LOG_RETENTION_HOURS: u32;             // 1
pub const MIN_RECONCILE_INTERVAL_SECS: u32;               // 10
pub fn default_groups() -> Vec<DashboardGroup>;           // "overview": "problems" (services, problems_only, hide_handled), "host problems", "all services"
pub fn new_id() -> String;                                // UUID v4
pub fn export_groups(groups: &[DashboardGroup]) -> Result<String, ConfigError>;     // TOML for sharing: format = "icygui-dashboards", version, [[groups]]
pub fn import_groups(text: &str) -> Result<Vec<DashboardGroup>, ConfigError>;       // fresh ids on import; nameless groups/dashboards → Invalid
pub fn parse_fingerprint(text: &str) -> Result<[u8; 32], ConfigError>;              // "AB:CD:…", "AB CD …", plain hex, openssl's "sha256 Fingerprint=…"
pub fn format_fingerprint(fingerprint: &[u8; 32]) -> String;                        // "AB:CD:…", the form to store in pinned_sha256
pub enum ConfigError {
    NoHomeDirectory, Io { action: &'static str, path: PathBuf, source: io::Error }, Parse { message: String },
    InvalidVersion(String), UnsupportedVersion { found: u64, supported: u64 }, Serialize(String),
    InvalidFingerprint(String), InvalidUrl { url: String, reason: String }, NotAnExport(String), Invalid(Vec<ValidationIssue>),
}
```

- File permissions: `0600` for the config file and its `.bak` on Unix.
- Unknown keys in the file are ignored without failing the load (and logged).
- A corrupt file is reported, not silently replaced: `load` returns an error (`Parse`, with line and column) and the app offers to restore the `.bak` copy (`load_backup`, then `save`) or start fresh (`save(&Config::default())`); either way the corrupt file becomes the `.bak`. A file from a newer version gives `UnsupportedVersion`; don't save over it without asking.
- `load` gives fresh ids to entries without a unique id (hand-written files) and saves at once, so the ids stay stable: environment ids are keychain accounts.
- `save` writes nothing when the contents are unchanged, replaces the target of a symlinked config file (keeping the link), and stamps `version = CONFIG_VERSION`.
- Validation is advisory and pure: `load` never fails because of it.
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
    pub async fn hosts(&self) -> Result<Vec<Host>, ApiError>;
    pub async fn services(&self) -> Result<Vec<Service>, ApiError>;
    pub async fn comments(&self) -> Result<Vec<Comment>, ApiError>;
    pub async fn downtimes(&self) -> Result<Vec<Downtime>, ApiError>;
    pub async fn host_groups(&self) -> Result<Vec<HostGroup>, ApiError>;
    pub async fn service_groups(&self) -> Result<Vec<ServiceGroup>, ApiError>;
    pub async fn dependencies(&self) -> Result<Vec<Dependency>, ApiError>;
    pub async fn endpoints(&self) -> Result<Vec<Endpoint>, ApiError>;
    pub async fn objects(&self, keys: &[ObjectKey]) -> Result<(Vec<Host>, Vec<Service>), ApiError>;   // targeted re-query, chunked
    pub async fn run_action(&self, action: &Action, target: &ActionTarget, author: &str) -> Result<Vec<ActionResult>, ApiError>;
    pub async fn events(&self, queue: &str, kinds: &[EventKind]) -> Result<EventStream, ApiError>;
}
pub async fn fetch_server_certificate(base_url: &Url, server_name: Option<&str>) -> Result<CertificateInfo, ApiError>;  // trust on first use: sha256, subject, issuer, not_after; accepts any cert, only reads it

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

**Wire rules:** verify against the docs and the handler sources.
- *Queries:* `POST /v1/objects/<type>` with `X-HTTP-Method-Override: GET`, `Accept: application/json` and a JSON body `{ "attrs": [...], "filter": ..., "filter_vars": ... }`. Request only the attributes the model needs. Responses are `{ "results": [ { "name", "type", "attrs": {…}, "joins": {}, "meta": {} } ] }`.
- *Numbers* arrive as JSON floats (`2.0`); map them defensively, never panic on odd input.
  - Pending is `last_check_result == null`.
  - Host `state` 1 with `last_reachable == false` is `HostState::Unreachable`.
  - Output: split `last_check_result.output` into first line and long output.
  - `performance_data` entries are strings (parse with `ic_model::parse_perfdata_entry`) or `PerfdataValue` dictionaries (`label`, `value`, `unit`, `warn`, `crit`, `min`, `max`).
  - Timestamps of 0 mean never.
- *Comments and downtimes:* the object comes from `host_name` / `service_name` (empty string = host).
  - `Downtime.in_effect`: use `is_in_effect` when present, otherwise compute it from fixed/flexible, start/end and `trigger_time`.
  - `config_owned`: `config_owner` is non-empty, or `scheduled_by` is non-empty.
- *Actions:* `POST /v1/actions/<name>` with `{ "type": "Host"|"Service", "filter": "host.name in names" | "service.__name in names", "filter_vars": { "names": [...] }, … }`.
  - Split mixed host/service targets into two requests.
  - `ActionTarget::Downtime(name)` uses `{ "downtime": name }`; `ActionTarget::Comment(name)` uses `{ "comment": name }`.
  - Parameter names and rules follow `12-icinga2-api.md`: `reschedule-check` sets `next_check` = now with `force`; `acknowledge-problem` always sends `notify: false`; `schedule-downtime` sends `fixed`, `duration`, `all_services`, `child_options`, `trigger_name`.
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

See PLAN.md §3.6. `MockServer::start(MockConfig) -> MockServer` serves HTTPS on `127.0.0.1:<port>` with a self-signed certificate and Basic auth. It exposes `url()`, `cert_pem()`, `cert_sha256()`, `control() -> MockControl` and `shutdown()`. It has built-in scenarios (`prod_cluster`, `staging`, `lab`, `large(seed)`) and a seedable simulator. The binary is `icinga-mock`. It writes its own wire JSON straight from the docs and sources; it never uses `ic-api`'s types.

---

## ic-core (wave 3)

```rust
pub struct EnvironmentSpec { pub environment: ic_config::Environment, pub general: ic_config::General, pub data_dir: PathBuf }
pub struct Ports { pub secrets: Arc<dyn SecretStore>, pub notifier: Arc<dyn Notifier>, pub clock: Arc<dyn Clock> }
pub fn start(spec: EnvironmentSpec, ports: Ports) -> Result<CoreHandle, CoreError>;   // spawns the runtime thread (tokio)
pub struct CoreHandle { /* command sender, event receiver, join handle */ }
impl CoreHandle {
    pub fn send(&self, command: Command);
    pub fn events(&self) -> impl Stream<Item = CoreEvent>;    // runtime-agnostic (futures channel), consumed on the GPUI thread
    pub fn shutdown(self);                                    // stops streams, flushes the event log, joins the thread
}
pub enum Command {
    Action { id: u64, target: ActionTarget, action: Action },
    Refresh,                                                   // full re-sync now
    UpdateEnvironment(ic_config::Environment),                 // dashboards or rules changed; re-evaluate
    PauseNotifications(Option<Timestamp>),
    LoadHistory { object: Option<ObjectKey>, limit: usize, reply: oneshot::Sender<Vec<LogEntry>> },
    LoadNotifications { limit: usize, reply: oneshot::Sender<Vec<NotificationRecord>> },
    MarkNotificationsRead,
}
pub enum CoreEvent {
    Connection(ConnectionState),
    Snapshot(Arc<Snapshot>),
    ActionFinished { id: u64, outcome: Result<ActionSummary, String> },
    Notification(NotificationRecord),
    Permissions(ApiInfo),
}
```

- *Sync:* initial parallel load → event stream → apply events to the store → mark the touched objects dirty → re-query dirty objects in batches (debounced about 500 ms) to fill in what events don't carry → reconcile everything every `reconcile_interval_secs`, and after every reconnect.
- *Snapshots:* batch publishing (at most about 4 per second).
- *Dashboards:* evaluate every dashboard (filter, `problems_only`, `hide_handled`, sort, group-by, summary) off the async threads, using `spawn_blocking`.
- *Rules:* feed `RuleInput`s for state, acknowledgement, downtime and flapping changes, never for the initial load. Changes found during a reconcile do count.
- *Event log:* SQLite in `data_dir`, pruned to `event_log_retention_hours`.
- *Connection:* reconnect with exponential backoff and jitter (1 s → 60 s). `ConnectionState` covers connecting, connected, reconnecting (with error and next retry), auth failed, and TLS failure (with the server fingerprint, for the trust-on-first-use dialog).

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
