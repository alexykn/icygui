# Architecture contracts

This is the binding contract between crates. `PLAN.md` explains the product and the reasons for these decisions; this file says exactly what each crate exposes and how it must behave. When code and this file disagree, fix one of them in the same change.

Already implemented and binding:
- `ic-model`: all domain types (names, timestamps, perfdata, objects, severity, events, actions, instance status).
- `ic-filter`, `ic-config`, `ic-rules` (engine included), `ic-platform`: implemented and reviewed, as specified below.
- `ic-api`: implemented and reviewed, including the tiered loading (`Detail`, `Fetched`), with integration tests against `ic-mock` and contract tests against a real Icinga 2.15.6.
- `ic-mock`: implemented (wave 2); its filter is still a shim, to be replaced by `ic-filter`.
- `ic-ui-kit` and `ic-app`: the static UI (chrome, dashboard list, service and host panes, tabs) on demo data.
- `ic-core`: `ports.rs` (`SecretStore`, `Notifier`, `Clock`) and `snapshot.rs` (`Snapshot`, `DashboardResult`, `DashboardRow`, `Summary`). The runtime is still to be built (wave 3).

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
/// names: `host.vars.role` → ["host", "vars", "role"]. Return `Some` only with
/// the value of the *whole* path (a missing key below a variable you provide
/// is `Some(Null)`), `None` when you don't provide `path[0]` or can't follow
/// the path; the evaluator then retries shorter prefixes and indexes the rest.
pub trait Scope { fn lookup(&self, path: &[&str]) -> Option<Value>; }

pub struct HostScope<'a> { pub host: &'a Host }
pub struct ServiceScope<'a> { pub service: &'a Service, pub host: Option<&'a Host> }
pub struct VarsScope<'a> { pub vars: &'a BTreeMap<String, Value> }       // API filter_vars
pub struct Chain<'a> { pub scopes: &'a [&'a dyn Scope] }                  // the first scope providing path[0] answers
```

**Object attributes:** these must be resolvable under `host.` and `service.`, with Icinga's names and value types (numbers are numbers):
- identity: `name`, `display_name`, `__name` (`host` / `host!service`), `host_name` (services), `address`, `address6` (hosts)
- state: `state` (host 0/1, service 0–3), `state_type`, `last_state_change`, `last_hard_state_change`, `last_check`, `next_check`, `check_attempt`, `max_check_attempts`, `acknowledgement`, `acknowledgement_expiry`, `downtime_depth`, `flapping`, `flapping_current`, `last_reachable`, `problem`, `handled`, `severity`
- check config: `check_command`, `check_interval`, `retry_interval`, `command_endpoint`, `zone`, `enable_*`
- `groups`, `vars`, `notes`, `notes_url`, `action_url`, `icon_image`
- `last_check_result` (dict with `output`, `exit_status`, `state`, `execution_start`, `execution_end`, `schedule_start`, `check_source`, `active`)

Pending objects have `last_check_result = null`, `problem = false` and, like Icinga (whose raw state starts as UNKNOWN; recorded in `contract/samples/services.json`), `state = 3` for services and `state = 1` for hosts; unreachable hosts have `state = 1` and `last_reachable = false`. Objects without custom variables have `vars = null`, as in Icinga. `ServiceScope` resolves `host.*` from its host.

**Language subset:** follow the Icinga grammar and semantics in the reference sources.
- literals: numbers, durations (`5m`, `1h`, `30s`, `2d`, `500ms`), strings with escapes, `{{{ }}}` multi-line strings, `true`, `false`, `null`, arrays `[…]`, dictionaries `{ key = value }`
- operators: `!`, unary `-`, `~`, `*`, `/`, `%`, `+`, `-`, `<<`, `>>`, `<`, `>`, `<=`, `>=`, `==`, `!=`, `in`, `!in`, `&`, `^`, `|`, `&&`, `||`, with Icinga's precedence and short-circuit; member access `.`, indexers `[…]`, parentheses
- functions: `match(pattern, value[, MatchAll|MatchAny])` (glob `*` `?`, arrays), `regex(pattern, value[, mode])`, `cidr_match(pattern, ip[, mode])`, `len`, `typeof`, `string`, `number`, `bool`, `intersection`, `union`, `range`, `get_time`
- methods: string `contains`, `find`, `len`, `lower`, `upper`, `trim`, `split`, `substr`, `starts_with`, `ends_with`; array `contains`, `len`, `join`; dict `contains`, `get`, `keys`, `len`

**Semantics:**
- equality, ordering, `+` on strings, `in` on arrays, and truthiness follow `value-operators.cpp`;
- unknown variables and missing keys are `null`, not errors (dashboard filters must not blow up on hosts lacking a var); for the same reason, methods called on `null` treat it as an empty value (`contains` false, `len` 0, …) instead of failing as in Icinga;
- calling an unknown function is an `EvalError`;
- one evaluation may create at most 16 MiB of text and 100 000 array items/dictionary entries (`range()` ≤ 10 000 numbers); beyond that it's an `EvalError`, so no filter can exhaust memory;
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
    pub fn load(&self) -> Result<Config, ConfigError>;    // missing file → Config::default() (a dangling symlink → Io error); runs migrations; repairs ids (below)
    pub fn load_backup(&self) -> Result<Option<Config>, ConfigError>;   // the `.bak` copy, for "restore"; never writes
    pub fn save(&self, config: &Config) -> Result<(), ConfigError>;   // atomic: temp file in same dir, fsync, rename; keeps one `.bak`; refuses secrets (Invalid)
}
pub fn migrate(raw: toml::Table) -> Result<Config, ConfigError>;     // version 0/absent → 1, future versions → error
impl Config {
    pub fn validate(&self) -> Vec<ValidationIssue>;       // ids unique, URLs are https with host, names non-empty, …
    pub fn environment(&self, id: &str) -> Option<&Environment>;
    pub fn environment_mut(&mut self, id: &str) -> Option<&mut Environment>;
    pub fn repair_ids(&mut self) -> usize;                // ids derived from content (UUID v5) for blank or duplicate ones; returns how many changed
}
impl Environment {
    pub fn new(name: &str, url: &str, auth: AuthConfig) -> Environment;   // fresh UUID, default dashboards, trusts the system roots
    pub fn author_name(&self) -> &str;                    // author (unless blank) or the basic-auth username, trimmed; "" for a client certificate without author
    pub fn group(&self, group_id: &str) -> Option<&DashboardGroup>;      // + group_mut
    pub fn dashboard(&self, group_id: &str, dashboard_id: &str) -> Option<&Dashboard>;   // + dashboard_mut
    pub fn api_url(&self) -> Result<Url, ConfigError>;    // checked like validate(); the path always ends in "/", so join("v1/…") keeps a proxy prefix; InvalidUrl masks credentials, query and fragment
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
- Unknown keys in the file are ignored without failing the load (and logged; keys that look like passwords get a warning of their own, also inside the tagged `auth` and override `object` tables). Logs never contain values.
- A corrupt file is reported, not silently replaced: `load` returns an error (`Parse`, with line and column and at most a short excerpt of the line) and the app offers to restore the `.bak` copy (`load_backup`, then `save`) or start fresh (`save(&Config::default())`); either way the corrupt file becomes the `.bak`, and `save` also keeps any file this version can't read (corrupt or newer) as `<name>.unreadable-<unix seconds>`, which later saves never replace. A file from a newer version gives `UnsupportedVersion`; don't save over it without asking.
- A config file that is a symlink (or below one) whose target is missing is an `Io` error, not a missing file: the volume may not be mounted yet, and defaults would silently drop every environment. `save` never replaces such a link: it writes through it when the target's directory exists and fails otherwise.
- `load` gives entries without a unique id (hand-written files) ids derived from their content: environment from name and URL, group from environment id and name, dashboard from environment id, group id and name (UUID v5, fixed namespace; never change the derivation). The same file therefore gets the same ids at every start even when it can't be written (read-only, managed by Nix or Ansible), so keychain accounts (environment ids) stay stable. It also saves at once to record them; a failed save is only logged.
- `save` writes nothing when the contents are unchanged, replaces the target of a symlinked config file (keeping the link), and stamps `version = CONFIG_VERSION`. It refuses (`Invalid`, nothing written) settings that would put a secret into the file: a user name or password in an environment URL, or a basic-auth username containing `:` (curl's `user:password`, which Icinga can never match).
- Validation is advisory and pure: `load` never fails because of it, and `save` refuses only the secret issues above. Error messages and validation issues quote user values only in short excerpts and never quote URL credentials.
- Tests: round trips, migrations, atomicity (an interrupted write leaves the old file intact, concurrent saves and loads never see a partial file), stable derived ids, secrets, symlinks, validation, import/export (with log capture), path layout.

---

## ic-rules (engine)

Implemented: the types (`settings.rs`, `intent.rs`) and the engine.

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
  - `skip_handled` ignores problems where `input.handled` is true. Recoveries don't depend on it.
  - A *problem* lasts from leaving OK/UP until back. A state already notified during it doesn't notify again unless another state notified in between (like Icinga 2.14+).
  - A problem skipped only because it was handled notifies once the handling ends *and* a fresh check confirms it. Without a check it waits 1 minute after an ack or downtime ended, or 5 minutes after `handled` cleared without an event. A state change in between is judged instead, so a problem that recovers on its next check never notifies (like Icinga's suppressed notifications).
  - While the object flaps, problems and recoveries don't notify; when it stops, its current state is judged. A lost `FlappingStopped` expires an hour after the last state change.
  - Recoveries and acknowledgements are judged by the scopes that notified the problem (as configured now) plus a current watch, never by scopes that didn't notify it.
- *Other events:* `EventFilter` enables acknowledgement, downtime and flapping changes. Downtime starts for one object within 5 s count as one.
- *Delays:* `min_duration_secs` holds a problem notification until the *problem* has lasted that long, counted from when it began; a change to another problem state doesn't restart it. `tick` then releases the state the object is in. A recovery cancels it, and so does handling for a rule with `skip_handled`.

**Output:**
- Intent ids are stable: `"{object}:{state}:{since}"` for state changes, similar for other kinds. The same id is never emitted twice (dedupe across memberships, repeated inputs and reconnect replays). Bound the dedupe memory, e.g. 10 000 recent ids.
- *Storm:* an intent that would be the `storm.threshold + 1`-th audible one within the trailing `storm.window_secs` is emitted with `silent = true`, for as long as the flood lasts. `tick` emits a summary intent ("14 new problems in prod-cluster"; `object = None`, `tone = Info`, not silent) once a whole window passes without a silenced intent, and at most once a minute while the storm lasts.
- *Quiet hours:* intents are `silent` inside the window (local time; windows may cross midnight; `days` is keyed by the start day). With `allow_critical`, critical and down states stay audible.
- *Pause:* intents are `silent` while paused.
- *Text:*
  - title: `"{LABEL} · {service} on {host}"` for services or `"{LABEL} · {host}"` for hosts. LABEL is CRITICAL, WARNING, UNKNOWN, DOWN, UNREACHABLE, RECOVERED, ACKNOWLEDGED, DOWNTIME, FLAPPING …, using display names.
  - body: the output's first non-blank line, or the comment.
  - All text is stripped of control and bidirectional formatting characters and cut to 400 characters per part.
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
    pub request_timeout: Duration,             // default 30 s: until the response headers, and per pause between body reads (a body may take ≤ 20× in total); connect timeout 10 s
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
    pub async fn run_action(&self, action: &Action, target: &ActionTarget, author: &str) -> Result<Vec<ActionResult>, ApiError>;   // Err only if the first request fails as a whole; later failures become per-target results (code 0 for timeout/connect)
    pub async fn events(&self, queue: &str, kinds: &[EventKind]) -> Result<EventStream, ApiError>;
    pub fn unknown_attributes(&self) -> Vec<(&'static str, &'static str)>;   // (type plural, attribute) this Icinga answered "Invalid field specified" for, sorted; empty on a current Icinga
}
pub async fn fetch_server_certificate(base_url: &Url, server_name: Option<&str>) -> Result<CertificateInfo, ApiError>;  // trust on first use: sha256, subject, issuer, not_after; accepts any cert, only reads it
pub struct CertificateInfo { pub sha256: [u8; 32], pub subject: String, pub issuer: String, pub names: Vec<String>, pub not_before: Timestamp, pub not_after: Timestamp }   // fingerprint() = colon hex
pub fn format_fingerprint(sha256: &[u8; 32]) -> String;              // "AB:CD:…", as in CertificateMismatch

pub enum Detail {
    Lean,   // display_name, state, state_type, last_state_change, last_hard_state_change, last_check, next_check, check_attempt, max_check_attempts,
            // acknowledgement(+expiry), downtime_depth, flapping, flapping_current, last_reachable, check_command, check_interval, retry_interval,
            // command_endpoint, zone, enable_* (all six), groups, vars; hosts also address, address6; services also host_name, name
    Full,   // Lean + last_check_result, notes, notes_url, action_url, icon_image
}
impl Detail { pub fn host_attrs(self) -> &'static [&'static str]; pub fn service_attrs(self) -> &'static [&'static str]; }   // the exact lists
pub struct Fetched { pub hosts: Vec<Host>, pub services: Vec<Service>, pub missing: Vec<ObjectKey> }   // missing: unknown to Icinga (deleted), request order, each once
pub struct ApiInfo { pub user: String, pub permissions: Vec<String>, pub version: String }
impl ApiInfo { pub fn allows(&self, permission: &str) -> bool; }       // Icinga wildcard semantics ("*", "actions/*", "objects/query/*"); "(filtered)" entries count as allowed
pub struct ActionResult { pub code: u16, pub status: String, pub name: Option<String>, pub target: Option<String> }   // name: created comment/downtime; target: the object/downtime/comment the result is for
impl ActionResult { pub fn is_success(&self) -> bool; }              // 2xx
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
  - Verified on 2.15.6: a never-checked service has `last_check: -1` and `state: 3` (the default raw state UNKNOWN; hosts map it to `state: 1`, `Host::CalculateState`), so `last_check` alone decides pending; mapping `state` would show every pending service as UNKNOWN and every pending host as DOWN.
  - Lean objects carry every attribute dashboard and rule filters can use (ic-filter, "Object attributes") except `last_check_result` and the links (`notes`, `notes_url`, `action_url`, `icon_image`), which keep their defaults (`None`, `""`). The check configuration, the `enable_*` switches and `flapping_current` are lean because no event carries them: a lean object would never learn them, and filters such as `service.check_command == "disk"` or `service.zone == "dmz"` would silently miss it. The check result comes with the next `CheckResult` event (or a full fetch); the links only with a full fetch. So a filter on `last_check_result` or the links sees `null`/`""` for a lean object until then; `ic-core` must not overwrite a hydrated object's result and links with a lean reload's.
  - `next_update` is **not** loaded: `ic_model::CheckInfo` has no field for it. The freshness watchdog computes the deadline with Icinga's formula (`Checkable::GetNextUpdate`): with active checks `next_check + interval + 2 × latency`, without `(end of the last result, or program start) + 2 × interval + 2 × latency`; `interval` is `retry_interval` for a soft problem with active checks and `check_interval` otherwise; `latency` (the last result's `execution_end − schedule_start`) is unknown for a lean object and counts as 0 until a `CheckResult` event or a full fetch brings a result. Request `next_update` once the model can carry it: it exists from 2.12 (`lib/icinga/checkable.ti`), not in 2.11, where the unknown-attribute rule below leaves it out at the cost of one extra request per type and client.
  - Sizes (`Detail::Lean` / `Detail::Full` / all attributes, services): `ic-mock`'s `large` scenario (30 000 services) 26.5 / 46.4 / 75.7 MB (884 / 1 548 / 2 524 bytes per service; its full and all-attribute sizes match the real measurements in docs/performance.md); the contract instance's 6 services 4.6 / 15.9 / 21.5 KB. The check configuration, switches and `flapping_current` add about 210 bytes per service to the lean list (3.4 → 4.6 KB on the contract instance).
- *`CheckResult` events* carry `check_result.vars_after` (`state`, `state_type`, `attempt`, `reachable`) plus `downtime_depth` and `acknowledgement` (a boolean in events). Map them into the event so `ic-core` can update the object without a re-query. For hosts, `vars_after.state` is a *service-style* state (0/1 = up, 2/3 = down; see `Host::CalculateState`).
- *Comments and downtimes:* the object comes from `host_name` / `service_name` (empty string = host).
  - `Downtime.in_effect`: use `is_in_effect` when present, otherwise compute it from fixed/flexible, start/end and `trigger_time`.
  - `config_owned`: `config_owner` is non-empty, or `scheduled_by` is non-empty.
- *Targeting (verified against a real Icinga 2.15 with `enforce_filter_expression_permission = true`, see `contract/`):* never send `filter` expressions. From Icinga 2.17 they need the `filter-expression` permission (`403 Missing permission: filter-expression`).
  - Target objects by name: `"hosts": [...]` (type `Host`) or `"services": ["host!service", ...]` (type `Service`). This works for both queries and actions without extra permissions.
  - A name list that contains one unknown name fails the whole request with `404 No objects found.` On a 404 for a batch, retry each name individually and treat the 404s as deleted objects. (`ic-api` splits the batch in halves until the unknown names are isolated: same result, fewer requests.)
  - **Never send an empty name list:** Icinga then targets *every* object of the type (`FilterUtility::GetFilterTargets` falls back to the type when no target was found).
  - Name lists go out in batches of `ic_api::NAMES_PER_REQUEST` (200).
  - `Fetched.missing` holds the isolated unknown names. Icinga answers a name hidden by a filtered `objects/query/*` permission ("Access denied to object") with the same 404, so those count as missing too.
- *Unknown attributes:* every attribute the client asks for exists in 2.15 (a unit test checks every request list against the recorded all-attribute samples; the contract tests query both `Detail` lists verbatim and require `unknown_attributes()` to stay empty). An attribute Icinga doesn't know fails the query: 2.15 and older reject the whole request (`400 {"error": 400, "status": "Invalid field specified: <attr>"}`, verified on 2.15.6); newer versions (current sources, which stream the response) answer `200` with every object as `{ "code": 400, "status": "Invalid field specified: <attr>" }`. Icinga only notices it while serialising an object: a type with no objects answers `200 {"results": []}` whatever `attrs` says. Either way `ic-api` leaves out exactly the named attribute (only one it asked for, never the last one), remembers it per client and type (`Client::unknown_attributes`), logs a warning and repeats the query; the mapping keeps that field's default. That lets an older Icinga work without attributes it lacks (`Downtime.parent` and `next_update` don't exist in 2.11). It never repeats without `attrs`: that returns every attribute, 75.7 MB instead of 26.5 MB for 30 000 services, and costs the master the memory docs/performance.md warns about. (`"attrs": []` returns objects with no attributes at all.)
- *Actions:* `POST /v1/actions/<name>` with `{ "type": "Host"|"Service", "hosts"|"services": [...], … }`.
  - Split mixed host/service targets into two requests.
  - `ActionTarget::Downtime(name)` uses `{ "downtime": name }` (only with `Action::RemoveAllDowntimes`, else `InvalidSettings`); `ActionTarget::Comment(name)` uses `{ "comment": name }` and means `remove-comment` (`ic_model::Action` has no remove-comment variant): it too needs `Action::RemoveAllDowntimes`, else `InvalidSettings` before anything is sent.
  - Parameter names and rules follow `12-icinga2-api.md`: `reschedule-check` sends `force` and leaves out `next_check`, so Icinga uses its own "now" (no client clock skew); `acknowledge-problem` always sends `notify: false`; `schedule-downtime` sends `fixed`, `duration` (0 for fixed), `all_services`, `child_options`, `trigger_name`; `process-check-result` for hosts sends 0 (UP) for plugin statuses 0–1 and 1 (DOWN) for 2 and above, since the API accepts only 0 and 1 for hosts (a UI offering UP/DOWN passes 0 or 2).
  - A 404 `No objects found.` for an action is handled like for queries (the targets were resolved before anything ran, so retrying is safe); the vanished names get a per-object 404 result. Results are matched to their target names by order.
  - `execute-command` needs an `endpoint` unless the object has `command_endpoint` (otherwise the per-object result is `404 Can't find a valid endpoint`). Pass the endpoint explicitly, defaulting to the object's `command_endpoint` or the instance's node name.
  - A response with per-object `code >= 400` still returns `Ok`, and the caller inspects the results. Icinga sets the HTTP status from the per-object codes, so any `{"results": [...]}` body is read as results whatever the status; only Icinga's error document (`{error, status}`) or an unreadable body maps to `ApiError`.
- *Events:* `POST /v1/events` with `{ "queue": queue, "types": [...] }`. The response is newline-delimited JSON over a long-lived HTTP/1.1 response.
  - Parse incrementally (lines can span chunks).
  - Map each type per `icinga2-apievents.cpp`.
  - Skip unknown types and malformed lines with a warning; don't end the stream.
  - The stream ends when the connection closes.
  - No read timeout on the stream; TCP keepalive (idle 30 s, interval 10 s, 3 retries; on Linux also `TCP_USER_TIMEOUT` 30 s) notices a dead connection within about a minute.
  - The queue name must not be blank (Icinga 2.15 rejects it with 400): `InvalidSettings`, nothing sent.
  - A missing `events/<type>` permission comes back from Icinga as a generic 404 (`path 'v1/events' … could not be found`). `events()` maps it to `Forbidden`, naming the missing permissions from `GET /v1`. Subscribe only to kinds `ApiInfo::allows`.
- *TLS:* rustls (ring provider).
  - Pinning: a custom verifier that compares the SHA-256 of the leaf DER and reports `CertificateMismatch` with both fingerprints, colon-hex uppercase.
  - Name override: verify against `server_name`.
  - Icinga's CA certificates often lack modern extensions; use webpki verification with the provided CA and don't add stricter policies.
  - Old Icinga node certificates may have no subjectAltName at all: then (and only then) the CN is compared with the expected name, as OpenSSL-based clients do.
  - No pin and no trusted root (no CA, no system roots) rejects every certificate as `UnknownIssuer`, so the caller can offer trust on first use.
  - Proxy environment variables are ignored and redirects are not followed.
- *Auth:* Basic auth, or a client certificate via the rustls client auth config.
- *Errors:* 401 → `Unauthorized`, 403 → `Forbidden` (include the response's `status` text), 404 → `NotFound`, other non-2xx → `Http`.
- *Tests:*
  - deserialisation tests from realistic JSON (doc examples plus pending, unreachable, perfdata dict and string forms, comments, downtimes);
  - an in-process HTTPS test server (hyper or axum with a self-signed certificate from `rcgen`) for pinning, CA trust, name override, auth headers, action bodies, event streaming split across chunks, and error mapping.
  - `crates/ic-api/tests/mock.rs` runs against `ic-mock` in-process: pinning to its self-signed certificate (and its CA), `prod_cluster` lean and full against the mock's own state, pending lean objects, `objects()` with unknown names across batches, actions with per-object results (200/409/404, created comment and downtime names), the event stream with a `MockControl::burst`, error mapping (401, 403 with Icinga's lowercased permission, the hidden events 404, 404 and 503), and the tiered load of a tenth of the `large` scenario; the full-size `large` load is `#[ignore]`d (about 20 s in a debug build).
  - `crates/ic-api/tests/contract.rs` runs read-only checks against a real Icinga when the `ICYGUI_CONTRACT_*` variables from `contract/run-icinga.sh` are set (and passes trivially otherwise): queries, both `Detail` lists verbatim and no unknown attribute in any query, lean against full services, `objects()` with unknown names, the unknown-attribute answer, a refused action and the event stream. Tests that need checked objects first wait (≤ 150 s) until Icinga has checked every object with active checks: a fresh instance runs its first checks within a minute of starting. With `ICYGUI_CONTRACT_REQUIRED` set, missing variables fail instead; `.github/workflows/contract.yml` sets it and runs them nightly (and on demand, with an image tag) against Icinga in Docker.

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
   - It is computed locally with Icinga's formula from the lean attributes (`next_update` isn't loaded; see ic-api, *Lean objects*) and recomputed from every `CheckResult` event, so manual checks by anyone move it.
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
- *Summary* is computed over all filter matches, *before* `problems_only` and `hide_handled` (it counts OK objects and handled problems too).
- A filter error sets `DashboardResult.error`.
- Performance target: 20 000 services × 10 dashboards stays responsive. A full evaluation takes well under a second in release builds; incremental updates take milliseconds. Test this with the `large` mock scenario (ignored test, run in release).

**Notifications:**
- Build an `ic_rules::RuleSet` from the environment (environment name, settings, groups/dashboards with their `ScopeSetting`s) and rebuild it on `UpdateEnvironment`.
- For every applied change, produce a `RuleInput`:
  - `StateChange`: previous state from the store before applying, `since` = `last_state_change`. A host that isn't reachable (`vars_after.reachable` / `last_reachable` false) is reported as `Unreachable`, not `Down`: a down notification can't be taken back;
  - `AcknowledgementSet`/`AcknowledgementCleared`;
  - `DowntimeStarted`/`DowntimeTriggered` → `DowntimeStarted` (Icinga reports a fixed downtime as both; the engine counts starts within 5 s as one);
  - `DowntimeRemoved` of an in-effect downtime → `DowntimeEnded`;
  - `Flapping` → `FlappingStarted`/`FlappingStopped`, also when a reconcile finds the flag changed.
  - `handled` is computed from the store after applying (acknowledged, in downtime, host problem for services, or unreachable through a dependency, which Icinga suppresses too); `memberships` from the dashboard filters.
  - When an object's `handled` changes without an event of its own (the services of a host that went down or came back, an object whose parent recovered), a repeat of its state (same `current` and `since`) with the new `handled`.
  - After an object's `handled` turned false, a repeat of its state on its next `CheckResult`, with `at` = that check's time: it tells the engine the state is current, not left over from the outage or maintenance.
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
    pub fn host_available() -> bool;         // a tray host shows the icon (Linux: StatusNotifierWatcher with a host on the session bus; macOS: always). Check before keeping the app running without a window (BG-01)
}
pub mod autostart {
    pub fn set_enabled(enabled: bool, app_id: &str, app_name: &str, exe: &Path) -> Result<(), PlatformError>;   // LaunchAgent plist / XDG autostart .desktop
    pub fn is_enabled(app_id: &str) -> bool;
}
```

## ic-ui-kit and ic-app

The UI renders `ic_core::snapshot::Snapshot` and `ic_config` types; it never talks HTTP or evaluates filters. All colours and sizes come from `ic_ui_kit::Theme`. The design is in `design/project/Icinga Client v2.dc.html` (calm v2 look; primary) and `design/project/Icinga Client.dc.html` (turn 1: palette, editor, sub-tabs). Pixel details are in PLAN.md §1.
