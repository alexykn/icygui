//! Scenario tests: sequences of inputs and ticks with explicit timestamps
//! and local times, judged through the public API only.

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, StateType, Timestamp};
use ic_rules::{
    Change, DashboardRef, DashboardScope, EventFilter, GroupScope, LocalTime, NotificationIntent,
    NotificationSettings, ObjectMode, ObjectOverride, QuietHours, Rule, RuleEngine, RuleInput,
    RuleSet, ScopeSetting, StateFilter, StormControl, Tone,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Scenario times are offsets in seconds from this instant.
const T0: f64 = 1_700_000_000.0;
const ENV: &str = "prod-cluster";
const HOST: &str = "db-prod-03";
const SERVICE: &str = "postgres-replication";

const MONDAY: u8 = 0;
const FRIDAY: u8 = 4;
const SATURDAY: u8 = 5;

fn ts(offset: f64) -> Timestamp {
    Timestamp::from_unix_seconds(T0 + offset)
}

fn local(weekday: u8, hour: u16, minute: u16) -> LocalTime {
    LocalTime {
        weekday,
        minute_of_day: hour * 60 + minute,
    }
}

/// A rule engine with a clock that only moves when the test says so.
struct Scenario {
    engine: RuleEngine,
    now: f64,
    local: LocalTime,
}

impl Scenario {
    fn new(rules: RuleSet) -> Self {
        Self {
            engine: RuleEngine::new(rules),
            now: 0.0,
            local: local(MONDAY, 12, 0),
        }
    }

    /// Moves the clock to `offset` (the local time doesn't move by itself).
    fn at(&mut self, offset: f64) -> &mut Self {
        self.now = offset;
        self
    }

    /// Sets the local wall-clock time.
    fn local(&mut self, local: LocalTime) -> &mut Self {
        self.local = local;
        self
    }

    fn input(&mut self, input: RuleInput) -> Vec<NotificationIntent> {
        self.engine.on_input(input, ts(self.now), self.local)
    }

    fn tick(&mut self) -> Vec<NotificationIntent> {
        self.engine.tick(ts(self.now), self.local)
    }
}

/// The only intent in `intents`.
fn one(mut intents: Vec<NotificationIntent>) -> NotificationIntent {
    assert_eq!(intents.len(), 1, "expected one intent: {intents:#?}");
    intents.swap_remove(0)
}

fn titles(intents: &[NotificationIntent]) -> Vec<&str> {
    intents.iter().map(|intent| intent.title.as_str()).collect()
}

fn none(intents: &[NotificationIntent]) {
    assert!(intents.is_empty(), "expected no intents: {intents:#?}");
}

// Rules ----------------------------------------------------------------------

fn rules(groups: Vec<GroupScope>) -> RuleSet {
    RuleSet {
        environment_name: ENV.to_owned(),
        settings: NotificationSettings::default(),
        groups,
    }
}

fn with_settings(mut rules: RuleSet, change: impl FnOnce(&mut NotificationSettings)) -> RuleSet {
    change(&mut rules.settings);
    rules
}

fn group(id: &str, setting: ScopeSetting, dashboards: Vec<DashboardScope>) -> GroupScope {
    GroupScope {
        id: id.to_owned(),
        name: id.to_owned(),
        setting,
        dashboards,
    }
}

fn dashboard(id: &str, setting: ScopeSetting) -> DashboardScope {
    DashboardScope {
        id: id.to_owned(),
        name: id.to_owned(),
        setting,
    }
}

fn rule(change: impl FnOnce(&mut Rule)) -> Rule {
    let mut rule = Rule::default();
    change(&mut rule);
    rule
}

fn delayed(seconds: u32) -> Rule {
    rule(|rule| rule.min_duration_secs = seconds)
}

fn all_events() -> EventFilter {
    EventFilter {
        acknowledgements: true,
        downtimes: true,
        flapping: true,
    }
}

// Inputs ---------------------------------------------------------------------

fn service_state(
    service: &str,
    state: ServiceState,
    state_type: StateType,
    since: f64,
) -> RuleInput {
    let output = match state {
        ServiceState::Ok => "OK - replication lag 2s\nprimary db-01",
        ServiceState::Warning => "WARNING - replication lag 95s",
        ServiceState::Critical => "CRITICAL - replication lag 412s\nprimary db-01\nstandby db-03",
        ServiceState::Unknown => "UNKNOWN - connection refused",
        ServiceState::Pending => "",
    };
    RuleInput {
        object: ObjectKey::service(HOST, service),
        host_display: HOST.to_owned(),
        service_display: Some(service.to_owned()),
        change: Change::State {
            previous: None,
            current: CheckableState::Service(state),
            state_type,
            since: ts(since),
            output: output.to_owned(),
        },
        handled: false,
        memberships: Vec::new(),
        at: ts(since),
    }
}

fn critical(since: f64) -> RuleInput {
    service_state(SERVICE, ServiceState::Critical, StateType::Hard, since)
}

fn soft_critical(since: f64) -> RuleInput {
    service_state(SERVICE, ServiceState::Critical, StateType::Soft, since)
}

fn warning(since: f64) -> RuleInput {
    service_state(SERVICE, ServiceState::Warning, StateType::Hard, since)
}

fn unknown(since: f64) -> RuleInput {
    service_state(SERVICE, ServiceState::Unknown, StateType::Hard, since)
}

fn ok(since: f64) -> RuleInput {
    service_state(SERVICE, ServiceState::Ok, StateType::Hard, since)
}

fn host_state(name: &str, state: HostState, state_type: StateType, since: f64) -> RuleInput {
    RuleInput {
        object: ObjectKey::host(name),
        host_display: name.to_owned(),
        service_display: None,
        change: Change::State {
            previous: None,
            current: CheckableState::Host(state),
            state_type,
            since: ts(since),
            output: format!("PING {state:?}"),
        },
        handled: false,
        memberships: Vec::new(),
        at: ts(since),
    }
}

fn event(change: Change, at: f64) -> RuleInput {
    RuleInput {
        object: ObjectKey::service(HOST, SERVICE),
        host_display: HOST.to_owned(),
        service_display: Some(SERVICE.to_owned()),
        change,
        handled: false,
        memberships: Vec::new(),
        at: ts(at),
    }
}

fn ack(at: f64) -> RuleInput {
    event(
        Change::AcknowledgementSet {
            author: "m.keller".to_owned(),
            comment: "looking into it".to_owned(),
        },
        at,
    )
    .handled()
}

fn ack_cleared(at: f64) -> RuleInput {
    event(Change::AcknowledgementCleared, at)
}

fn downtime_started(at: f64) -> RuleInput {
    event(
        Change::DowntimeStarted {
            author: "ops".to_owned(),
            comment: "kernel update".to_owned(),
        },
        at,
    )
}

fn downtime_ended(at: f64) -> RuleInput {
    event(Change::DowntimeEnded, at)
}

trait InputExt {
    /// The dashboards the object appears on.
    fn on(self, dashboards: &[(&str, &str)]) -> Self;
    /// Handled after the change.
    fn handled(self) -> Self;
    /// Happened at `offset` (state changes keep their `since`).
    fn happened(self, offset: f64) -> Self;
    /// About another service of the same host.
    fn service(self, name: &str) -> Self;
}

impl InputExt for RuleInput {
    fn on(mut self, dashboards: &[(&str, &str)]) -> Self {
        self.memberships = dashboards
            .iter()
            .map(|(group, dashboard)| DashboardRef {
                group_id: (*group).to_owned(),
                dashboard_id: (*dashboard).to_owned(),
            })
            .collect();
        self
    }

    fn handled(mut self) -> Self {
        self.handled = true;
        self
    }

    fn happened(mut self, offset: f64) -> Self {
        self.at = ts(offset);
        self
    }

    fn service(mut self, name: &str) -> Self {
        self.object = ObjectKey::service(HOST, name);
        self.service_display = Some(name.to_owned());
        self
    }
}

fn the_service() -> ObjectKey {
    ObjectKey::service(HOST, SERVICE)
}

// ---------------------------------------------------------------------------
// Basics and text
// ---------------------------------------------------------------------------

mod basics {
    use super::*;

    #[test]
    fn a_hard_critical_notifies_with_title_body_and_subtitle() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        let intent = one(scenario.at(0.0).input(critical(0.0)));
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700000000.000"
        );
        assert_eq!(intent.object, Some(the_service()));
        assert_eq!(
            intent.title,
            "CRITICAL · postgres-replication on db-prod-03"
        );
        assert_eq!(intent.subtitle, ENV);
        assert_eq!(intent.body, "CRITICAL - replication lag 412s");
        assert_eq!(intent.tone, Tone::Critical);
        assert!(intent.sound);
        assert!(!intent.silent);
        assert_eq!(intent.at, ts(0.0));
    }

    #[test]
    fn the_default_rule_selects_critical_unknown_and_down() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.input(warning(0.0).service("a")));
        let unknown = one(scenario.input(unknown(0.0).service("b")));
        assert_eq!(unknown.title, "UNKNOWN · b on db-prod-03");
        assert_eq!(unknown.tone, Tone::Unknown);

        let down = one(scenario.input(host_state(
            "k8s-node-07",
            HostState::Down,
            StateType::Hard,
            0.0,
        )));
        assert_eq!(down.title, "DOWN · k8s-node-07");
        assert_eq!(down.id, "k8s-node-07:down:1700000000.000");
        assert_eq!(down.tone, Tone::Critical);
        none(&scenario.input(host_state(
            "k8s-node-08",
            HostState::Unreachable,
            StateType::Hard,
            0.0,
        )));
    }

    #[test]
    fn every_selected_state_has_its_label_and_tone() {
        let everything = rule(|rule| {
            rule.states = StateFilter {
                critical: true,
                warning: true,
                unknown: true,
                down: true,
                unreachable: true,
                recovery: true,
            };
        });
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule = everything;
            settings.storm.window_secs = 0;
        }));
        let warning = one(scenario.input(warning(0.0)));
        assert_eq!(
            warning.title,
            "WARNING · postgres-replication on db-prod-03"
        );
        assert_eq!(warning.tone, Tone::Warning);
        let unreachable = one(scenario.input(host_state(
            "leaf-01",
            HostState::Unreachable,
            StateType::Hard,
            0.0,
        )));
        assert_eq!(unreachable.title, "UNREACHABLE · leaf-01");
        assert_eq!(unreachable.tone, Tone::Unknown);
    }

    #[test]
    fn display_names_are_used_and_fall_back_to_names() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        let mut input = critical(0.0);
        input.host_display = "DB 3 (prod)".to_owned();
        input.service_display = Some("Replication".to_owned());
        assert_eq!(
            one(scenario.input(input)).title,
            "CRITICAL · Replication on DB 3 (prod)"
        );

        let mut input = critical(0.0).service("disk");
        input.service_display = None;
        assert_eq!(
            one(scenario.input(input)).title,
            "CRITICAL · disk on db-prod-03"
        );
    }

    #[test]
    fn the_body_is_the_first_line_of_the_output() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        let mut input = critical(0.0);
        if let Change::State { output, .. } = &mut input.change {
            *output = "\nDISK CRITICAL - /var 98%\n/var/log 97%".to_owned();
        }
        assert_eq!(one(scenario.input(input)).body, "DISK CRITICAL - /var 98%");
    }

    #[test]
    fn a_sound_follows_the_deciding_rule() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.sound = false;
        }));
        assert!(!one(scenario.input(critical(0.0))).sound);
    }

    #[test]
    fn a_change_to_pending_forgets_the_object() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.input(critical(0.0)));
        none(&scenario.at(10.0).input(service_state(
            SERVICE,
            ServiceState::Pending,
            StateType::Hard,
            10.0,
        )));
        none(&scenario.at(20.0).input(ok(20.0)));
    }

    #[test]
    fn rules_can_be_read_back_and_replaced() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        assert_eq!(scenario.engine.rules().environment_name, ENV);
        let mut replaced = rules(Vec::new());
        replaced.environment_name = "staging".to_owned();
        scenario.engine.set_rules(replaced);
        assert_eq!(one(scenario.input(critical(0.0))).subtitle, "staging");
    }
}

// ---------------------------------------------------------------------------
// Scope resolution
// ---------------------------------------------------------------------------

mod scopes {
    use super::*;

    #[test]
    fn an_object_on_no_dashboard_follows_the_environment() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.enabled = false;
        }));
        none(&scenario.input(critical(0.0)));
    }

    #[test]
    fn an_object_in_an_off_and_an_on_dashboard_notifies_once_from_the_on_one() {
        let mut scenario = Scenario::new(rules(vec![group(
            "databases",
            ScopeSetting::Inherit,
            vec![
                dashboard("all", ScopeSetting::Off),
                dashboard("production", ScopeSetting::On),
            ],
        )]));
        let intent =
            one(scenario
                .input(critical(0.0).on(&[("databases", "all"), ("databases", "production")])));
        assert_eq!(intent.subtitle, "databases / production");

        // Only on the Off dashboard: silent even though the environment is on.
        none(&scenario.input(critical(0.0).service("disk").on(&[("databases", "all")])));
    }

    #[test]
    fn on_wins_over_a_disabled_environment_and_off_group() {
        let mut scenario = Scenario::new(with_settings(
            rules(vec![group(
                "sandbox",
                ScopeSetting::Off,
                vec![
                    dashboard("inherit", ScopeSetting::Inherit),
                    dashboard("on", ScopeSetting::On),
                ],
            )]),
            |settings| settings.enabled = false,
        ));
        none(&scenario.input(critical(0.0).on(&[("sandbox", "inherit")])));
        let intent = one(scenario.input(critical(0.0).service("b").on(&[("sandbox", "on")])));
        assert_eq!(intent.subtitle, "sandbox / on");
    }

    #[test]
    fn a_custom_rule_on_a_dashboard_under_an_off_group_applies() {
        let warnings_quietly = rule(|rule| {
            rule.states.warning = true;
            rule.sound = false;
        });
        let mut scenario = Scenario::new(rules(vec![group(
            "sandbox",
            ScopeSetting::Off,
            vec![
                dashboard("inherit", ScopeSetting::Inherit),
                dashboard("custom", ScopeSetting::Custom(warnings_quietly)),
            ],
        )]));
        none(&scenario.input(critical(0.0).on(&[("sandbox", "inherit")])));

        let intent = one(scenario.input(warning(0.0).service("b").on(&[("sandbox", "custom")])));
        assert_eq!(intent.title, "WARNING · b on db-prod-03");
        assert_eq!(intent.subtitle, "sandbox / custom");
        assert!(!intent.sound, "the custom rule's sound setting");

        let intent = one(scenario.input(
            critical(0.0)
                .service("c")
                .on(&[("sandbox", "inherit"), ("sandbox", "custom")]),
        ));
        assert_eq!(intent.subtitle, "sandbox / custom");
    }

    #[test]
    fn a_custom_group_rule_is_inherited_by_its_dashboards() {
        let warnings = rule(|rule| rule.states.warning = true);
        let mut scenario = Scenario::new(rules(vec![group(
            "web",
            ScopeSetting::Custom(warnings),
            vec![dashboard("frontends", ScopeSetting::Inherit)],
        )]));
        let intent = one(scenario.input(warning(0.0).on(&[("web", "frontends")])));
        assert_eq!(intent.subtitle, "web / frontends");
        // Off the group, the environment's default (no warnings) applies.
        none(&scenario.input(warning(0.0).service("b")));
    }

    #[test]
    fn the_subtitle_names_the_first_matching_dashboard_in_sidebar_order() {
        let warnings = rule(|rule| rule.states.warning = true);
        let mut scenario = Scenario::new(rules(vec![
            group(
                "a",
                ScopeSetting::Inherit,
                vec![dashboard("critical-only", ScopeSetting::Inherit)],
            ),
            group(
                "b",
                ScopeSetting::Inherit,
                vec![
                    dashboard("first", ScopeSetting::Custom(warnings.clone())),
                    dashboard("second", ScopeSetting::Custom(warnings)),
                ],
            ),
        ]));
        // Input order doesn't matter, sidebar order does.
        let intent = one(scenario.input(critical(0.0).on(&[
            ("b", "second"),
            ("b", "first"),
            ("a", "critical-only"),
        ])));
        assert_eq!(intent.subtitle, "a / critical-only");

        // A warning doesn't match a/critical-only; b/first is the first match.
        let intent = one(scenario.input(warning(0.0).service("b").on(&[
            ("b", "second"),
            ("a", "critical-only"),
            ("b", "first"),
        ])));
        assert_eq!(intent.subtitle, "b / first");
    }

    #[test]
    fn deleted_dashboards_count_as_no_membership() {
        let mut scenario = Scenario::new(rules(vec![group(
            "databases",
            ScopeSetting::Off,
            vec![dashboard("all", ScopeSetting::Inherit)],
        )]));
        let intent = one(scenario.input(critical(0.0).on(&[("gone", "dashboard")])));
        assert_eq!(intent.subtitle, ENV, "the environment pair applies");
        none(
            &scenario.input(
                critical(0.0)
                    .service("b")
                    .on(&[("gone", "dashboard"), ("databases", "all")]),
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Object overrides
// ---------------------------------------------------------------------------

mod overrides {
    use super::*;

    fn override_rules(mode: ObjectMode, until: Option<f64>, groups: Vec<GroupScope>) -> RuleSet {
        with_settings(rules(groups), |settings| {
            settings.objects = vec![ObjectOverride {
                object: the_service(),
                mode,
                until: until.map(ts),
            }];
        })
    }

    fn sandbox_off() -> Vec<GroupScope> {
        vec![group(
            "sandbox",
            ScopeSetting::Off,
            vec![dashboard("all", ScopeSetting::Inherit)],
        )]
    }

    #[test]
    fn a_mute_suppresses_everything_until_it_expires() {
        let mut rules = override_rules(ObjectMode::Mute, Some(1_000.0), Vec::new());
        rules.settings.default_rule.events = all_events();
        let mut scenario = Scenario::new(rules);
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(10.0).input(ack(10.0)));
        none(&scenario.at(20.0).input(ok(20.0)));
        none(&scenario.at(30.0).input(downtime_started(30.0)));
        none(
            &scenario
                .at(40.0)
                .input(event(Change::FlappingStarted, 40.0)),
        );
        none(&scenario.at(999.0).tick());
        // Nothing is open when the mute ends, so nothing is held back.
        none(&scenario.at(1_000.0).tick());

        // After expiry everything notifies again.
        let intent = one(scenario.at(1_001.0).input(critical(1_001.0)));
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700001001.000"
        );
        one(scenario.at(1_002.0).input(ack(1_002.0)));
        // Other objects were never muted.
        one(scenario.at(0.0).input(critical(0.0).service("other")));
    }

    #[test]
    fn a_muted_recovery_ends_the_problem() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        scenario
            .engine
            .set_rules(override_rules(ObjectMode::Mute, None, Vec::new()));
        none(&scenario.at(10.0).input(ok(10.0)));

        // Unmuted: a later problem and its recovery notify as usual.
        scenario.engine.set_rules(rules(Vec::new()));
        none(&scenario.at(20.0).input(ok(20.0)));
        one(scenario.at(30.0).input(critical(30.0)));
        assert_eq!(
            one(scenario.at(40.0).input(ok(40.0))).title,
            "RECOVERED · postgres-replication on db-prod-03"
        );
    }

    #[test]
    fn a_problem_still_open_when_the_mute_expires_notifies_then() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, Some(100.0), Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(99.0).tick());
        let intent = one(scenario.at(100.0).tick());
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700000000.000"
        );
        assert_eq!(intent.at, ts(0.0), "`at` is when the change happened");
        none(&scenario.at(101.0).tick());
        // It was notified, so its recovery is too.
        one(scenario.at(200.0).input(ok(200.0)));
    }

    #[test]
    fn a_problem_over_before_the_mute_expires_never_notifies() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, Some(100.0), Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(50.0).input(ok(50.0)));
        none(&scenario.at(100.0).tick());
        none(&scenario.at(200.0).tick());
    }

    #[test]
    fn a_problem_handled_when_the_mute_expires_stays_quiet() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, Some(100.0), Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(50.0).input(ack(50.0)));
        none(&scenario.at(100.0).tick());
        // Until the acknowledgement goes away.
        none(&scenario.at(200.0).input(ack_cleared(200.0)));
        one(scenario.at(210.0).tick());
    }

    #[test]
    fn removing_a_mute_notifies_a_problem_that_is_still_open() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, None, Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(10_000.0).tick());
        scenario.engine.set_rules(rules(Vec::new()));
        one(scenario.at(10_001.0).tick());
    }

    #[test]
    fn a_mute_added_while_a_notification_waits_holds_it_back() {
        let mut rules = rules(Vec::new());
        rules.settings.default_rule = delayed(300);
        let mut scenario = Scenario::new(rules.clone());
        none(&scenario.at(0.0).input(critical(0.0)));
        let mut muted = rules;
        muted.settings.objects = vec![ObjectOverride {
            object: the_service(),
            mode: ObjectMode::Mute,
            until: Some(ts(400.0)),
        }];
        scenario.engine.set_rules(muted);
        none(&scenario.at(100.0).tick());
        none(&scenario.at(300.0).tick());
        none(&scenario.at(399.0).tick());
        // The delay ran out during the mute: it notifies when the mute ends.
        assert_eq!(one(scenario.at(400.0).tick()).at, ts(0.0));
    }

    #[test]
    fn a_mute_expiring_during_a_wait_lets_the_notification_through() {
        let mut rules = override_rules(ObjectMode::Mute, Some(200.0), Vec::new());
        rules.settings.default_rule = delayed(300);
        let mut scenario = Scenario::new(rules);
        // Muted when the problem starts, but the delay runs past the mute.
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(299.0).tick());
        let intent = one(scenario.at(300.0).tick());
        assert_eq!(intent.at, ts(0.0));
    }

    #[test]
    fn a_watch_overrides_off_scopes() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Watch, None, sandbox_off()));
        let intent = one(scenario.input(critical(0.0).on(&[("sandbox", "all")])));
        assert_eq!(intent.subtitle, ENV);
        // The same change for an unwatched object stays silent.
        none(&scenario.input(critical(0.0).service("other").on(&[("sandbox", "all")])));
    }

    #[test]
    fn a_watch_overrides_a_disabled_environment() {
        let mut rules = override_rules(ObjectMode::Watch, None, Vec::new());
        rules.settings.enabled = false;
        let mut scenario = Scenario::new(rules);
        one(scenario.input(critical(0.0)));
        none(&scenario.input(critical(0.0).service("other")));
    }

    #[test]
    fn a_watch_uses_the_default_rule_and_adds_to_dashboard_rules() {
        let warnings = rule(|rule| rule.states.warning = true);
        let mut scenario = Scenario::new(override_rules(
            ObjectMode::Watch,
            None,
            vec![group(
                "db",
                ScopeSetting::Custom(warnings),
                vec![dashboard("all", ScopeSetting::Inherit)],
            )],
        ));
        // The dashboard's custom rule still applies to the watched object.
        assert_eq!(
            one(scenario.input(warning(0.0).on(&[("db", "all")]))).subtitle,
            "db / all"
        );
        // The watch's own rule is the default rule: no warnings.
        none(&scenario.input(warning(10.0).on(&[])));
    }

    #[test]
    fn an_expired_watch_no_longer_applies() {
        let mut scenario = Scenario::new(override_rules(
            ObjectMode::Watch,
            Some(100.0),
            sandbox_off(),
        ));
        one(scenario
            .at(50.0)
            .input(critical(50.0).on(&[("sandbox", "all")])));
        none(
            &scenario
                .at(150.0)
                .input(unknown(150.0).on(&[("sandbox", "all")])),
        );
    }

    #[test]
    fn a_watched_problem_held_back_by_a_mute_notifies_when_the_mute_ends() {
        // Only the watch notifies (the group is off); the mute holds the
        // problem back, and the watch still applies once the mute ends.
        let mut rules = override_rules(ObjectMode::Watch, None, sandbox_off());
        rules.settings.objects.push(ObjectOverride {
            object: the_service(),
            mode: ObjectMode::Mute,
            until: Some(ts(100.0)),
        });
        let mut scenario = Scenario::new(rules);
        none(
            &scenario
                .at(0.0)
                .input(critical(0.0).on(&[("sandbox", "all")])),
        );
        none(&scenario.at(99.0).tick());
        assert_eq!(one(scenario.at(100.0).tick()).subtitle, ENV);
    }

    #[test]
    fn a_mute_that_ended_before_a_late_recovery_notifies_the_problem_first() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, Some(100.0), Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        // No tick ran between the end of the mute and the recovery.
        assert_eq!(
            titles(&scenario.at(150.0).input(ok(150.0))),
            [
                "CRITICAL · postgres-replication on db-prod-03",
                "RECOVERED · postgres-replication on db-prod-03"
            ]
        );
    }

    #[test]
    fn a_problem_that_recovered_before_the_mute_ended_stays_silent_on_a_late_recovery() {
        let mut scenario = Scenario::new(override_rules(ObjectMode::Mute, Some(100.0), Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0)));
        // The recovery happened during the mute but arrives after it.
        none(&scenario.at(150.0).input(ok(80.0)));
    }

    #[test]
    fn a_mute_wins_over_a_watch() {
        let mut rules = override_rules(ObjectMode::Watch, None, Vec::new());
        rules.settings.objects.push(ObjectOverride {
            object: the_service(),
            mode: ObjectMode::Mute,
            until: None,
        });
        let mut scenario = Scenario::new(rules);
        none(&scenario.input(critical(0.0)));
    }
}

// ---------------------------------------------------------------------------
// States: hard/soft, handled, recoveries
// ---------------------------------------------------------------------------

mod states {
    use super::*;

    #[test]
    fn soft_states_are_ignored_until_they_turn_hard() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(soft_critical(0.0)));
        none(&scenario.at(60.0).input(soft_critical(0.0).happened(60.0)));
        let intent = one(scenario.at(120.0).input(critical(0.0).happened(120.0)));
        assert_eq!(
            intent.id, "db-prod-03!postgres-replication:critical:1700000000.000",
            "the id keeps the soft state's `since`"
        );
        assert_eq!(intent.at, ts(120.0), "it happened when it turned hard");
        none(&scenario.at(180.0).input(critical(0.0).happened(180.0)));
    }

    #[test]
    fn with_soft_states_on_the_hard_transition_does_not_notify_again() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.hard_only = false;
        }));
        let intent = one(scenario.at(0.0).input(soft_critical(0.0)));
        assert_eq!(intent.at, ts(0.0));
        none(&scenario.at(120.0).input(critical(0.0).happened(120.0)));
    }

    #[test]
    fn a_soft_problem_that_recovers_never_notifies() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(soft_critical(0.0)));
        none(&scenario.at(60.0).input(ok(60.0)));
    }

    #[test]
    fn handled_problems_are_skipped_unless_the_rule_says_otherwise() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.input(critical(0.0).handled()));

        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.skip_handled = false;
        }));
        one(scenario.input(critical(0.0).handled()));
    }

    #[test]
    fn a_recovery_notifies_only_after_a_notified_problem() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        // A warning doesn't notify, so neither does its recovery.
        none(&scenario.at(0.0).input(warning(0.0)));
        none(&scenario.at(10.0).input(ok(10.0)));

        one(scenario.at(20.0).input(critical(20.0)));
        let recovery = one(scenario.at(30.0).input(ok(30.0)));
        assert_eq!(
            recovery.id,
            "db-prod-03!postgres-replication:ok:1700000030.000"
        );
        assert_eq!(
            recovery.title,
            "RECOVERED · postgres-replication on db-prod-03"
        );
        assert_eq!(recovery.body, "OK - replication lag 2s");
        assert_eq!(recovery.tone, Tone::Recovery);
        assert_eq!(recovery.subtitle, ENV);
        assert!(!recovery.silent);

        // The problem is over: another OK doesn't notify again.
        none(&scenario.at(40.0).input(ok(40.0)));
    }

    #[test]
    fn a_recovery_follows_a_problem_that_changed_state_in_between() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(10.0).input(warning(10.0)));
        assert_eq!(
            titles(&scenario.at(20.0).input(ok(20.0))),
            ["RECOVERED · postgres-replication on db-prod-03"]
        );
    }

    #[test]
    fn a_handled_problem_gets_no_recovery() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0).handled()));
        none(&scenario.at(10.0).input(ok(10.0)));
    }

    #[test]
    fn recoveries_can_be_switched_off() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.states.recovery = false;
        }));
        one(scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(10.0).input(ok(10.0)));
    }

    #[test]
    fn hosts_recover_too() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(host_state(
            "k8s-node-07",
            HostState::Down,
            StateType::Hard,
            0.0,
        )));
        let recovery = one(scenario.at(60.0).input(host_state(
            "k8s-node-07",
            HostState::Up,
            StateType::Hard,
            60.0,
        )));
        assert_eq!(recovery.title, "RECOVERED · k8s-node-07");
        assert_eq!(recovery.id, "k8s-node-07:up:1700000060.000");
    }

    #[test]
    fn a_recovery_after_a_silent_quiet_hours_notification_still_notifies() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.quiet_hours = QuietHours {
                enabled: true,
                start_minute: 22 * 60,
                end_minute: 7 * 60,
                days: [true; 7],
                allow_critical: false,
            };
        }));
        let problem = one(scenario
            .local(local(MONDAY, 23, 0))
            .at(0.0)
            .input(critical(0.0)));
        assert!(problem.silent);
        let recovery = one(scenario
            .local(local(1, 7, 30))
            .at(30_600.0)
            .input(ok(30_600.0)));
        assert_eq!(recovery.tone, Tone::Recovery);
        assert!(!recovery.silent);
    }

    #[test]
    fn a_recovery_is_judged_by_the_scopes_that_notified_the_problem() {
        // The dashboard's filter only matches while the service is
        // critical, and the environment is off: without remembering the
        // dashboard, the recovery would go unnoticed.
        let mut scenario = Scenario::new(with_settings(
            rules(vec![group(
                "databases",
                ScopeSetting::Inherit,
                vec![dashboard("critical", ScopeSetting::On)],
            )]),
            |settings| settings.enabled = false,
        ));
        one(scenario
            .at(0.0)
            .input(critical(0.0).on(&[("databases", "critical")])));
        let recovery = one(scenario.at(10.0).input(ok(10.0).on(&[])));
        assert_eq!(recovery.subtitle, "databases / critical");
    }

    #[test]
    fn a_recovery_is_not_notified_by_a_scope_turned_off_since() {
        let on = rules(vec![group(
            "databases",
            ScopeSetting::Inherit,
            vec![dashboard("critical", ScopeSetting::On)],
        )]);
        let mut scenario = Scenario::new(with_settings(on.clone(), |settings| {
            settings.enabled = false;
        }));
        one(scenario
            .at(0.0)
            .input(critical(0.0).on(&[("databases", "critical")])));
        let mut off = with_settings(on, |settings| settings.enabled = false);
        off.groups[0].dashboards[0].setting = ScopeSetting::Off;
        scenario.engine.set_rules(off);
        none(
            &scenario
                .at(10.0)
                .input(ok(10.0).on(&[("databases", "critical")])),
        );
    }

    #[test]
    fn down_and_unreachable_share_a_since_without_repeating() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        let node = "k8s-node-07";
        one(scenario
            .at(0.0)
            .input(host_state(node, HostState::Down, StateType::Hard, 0.0)));
        // A parent went down: same Icinga state and `since`, now unreachable.
        none(
            &scenario.at(30.0).input(
                host_state(node, HostState::Unreachable, StateType::Hard, 0.0).happened(30.0),
            ),
        );
        // The parent is back: down again with the same `since`, already notified.
        none(
            &scenario
                .at(60.0)
                .input(host_state(node, HostState::Down, StateType::Hard, 0.0).happened(60.0)),
        );
        one(scenario
            .at(90.0)
            .input(host_state(node, HostState::Up, StateType::Hard, 90.0)));
    }

    #[test]
    fn an_older_state_change_is_ignored() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        scenario
            .engine
            .set_rules(with_settings(rules(Vec::new()), |settings| {
                settings.default_rule = delayed(300);
            }));
        none(&scenario.at(100.0).input(critical(100.0)));
        // A late duplicate of the recovery before it must not cancel the delay.
        none(&scenario.at(150.0).input(ok(50.0)));
        assert_eq!(
            titles(&scenario.at(400.0).tick()),
            ["CRITICAL · postgres-replication on db-prod-03"]
        );
    }

    #[test]
    fn a_known_state_from_the_future_does_not_block_newer_input() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        // Icinga's clock was ahead when this state began.
        one(scenario.at(0.0).input(critical(1_000.0)));
        // After the clock was fixed, the recovery's `since` is "older".
        let recovery = one(scenario.at(10.0).input(ok(10.0)));
        assert_eq!(recovery.tone, Tone::Recovery);
    }

    #[test]
    fn a_missing_since_falls_back_to_the_time_of_the_change() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        let mut input = critical(0.0).happened(42.0);
        if let Change::State { since, .. } = &mut input.change {
            *since = Timestamp::EPOCH;
        }
        let intent = one(scenario.at(42.0).input(input));
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700000042.000"
        );
    }
}

// ---------------------------------------------------------------------------
// Delays (min_duration_secs) and handling
// ---------------------------------------------------------------------------

mod delays {
    use super::*;

    fn delayed_rules(seconds: u32) -> RuleSet {
        with_settings(rules(Vec::new()), |settings| {
            settings.default_rule = delayed(seconds);
        })
    }

    #[test]
    fn a_delayed_notification_is_released_by_tick_when_due() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(299.0).tick());
        let intent = one(scenario.at(300.0).tick());
        assert_eq!(
            intent.title,
            "CRITICAL · postgres-replication on db-prod-03"
        );
        assert_eq!(intent.at, ts(0.0), "`at` is when the change happened");
        none(&scenario.at(301.0).tick());
        none(&scenario.at(400.0).input(critical(0.0).happened(400.0)));
    }

    #[test]
    fn the_delay_counts_from_since_including_the_soft_phase() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(soft_critical(0.0)));
        none(&scenario.at(120.0).input(critical(0.0).happened(120.0)));
        none(&scenario.at(299.0).tick());
        one(scenario.at(300.0).tick());
    }

    #[test]
    fn a_server_clock_running_ahead_does_not_stretch_the_delay() {
        let mut scenario = Scenario::new(delayed_rules(300));
        // Icinga's clock is 1000 s ahead: the state "begins" in our future.
        none(&scenario.at(0.0).input(critical(1_000.0)));
        none(&scenario.at(299.0).tick());
        one(scenario.at(300.0).tick());
    }

    #[test]
    fn a_problem_that_already_lasted_long_enough_notifies_at_once() {
        let mut scenario = Scenario::new(delayed_rules(300));
        // A change found late (e.g. by a reconcile).
        one(scenario.at(500.0).input(critical(0.0).happened(500.0)));
    }

    #[test]
    fn a_recovery_cancels_a_delayed_notification() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(100.0).input(ok(100.0)));
        none(&scenario.at(300.0).tick());
        none(&scenario.at(1_000.0).tick());
    }

    #[test]
    fn an_acknowledgement_cancels_a_delayed_notification() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(100.0).input(ack(100.0)));
        none(&scenario.at(300.0).tick());
        none(&scenario.at(1_000.0).tick());
    }

    #[test]
    fn an_acknowledgement_notifies_itself_but_cancels_the_problem() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule = rule(|rule| {
                rule.min_duration_secs = 300;
                rule.events = all_events();
            });
        }));
        none(&scenario.at(0.0).input(critical(0.0)));
        let acknowledged = one(scenario.at(100.0).input(ack(100.0)));
        assert_eq!(
            acknowledged.title,
            "ACKNOWLEDGED · postgres-replication on db-prod-03"
        );
        none(&scenario.at(300.0).tick());
    }

    #[test]
    fn a_downtime_cancels_a_delayed_notification() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(100.0).input(downtime_started(100.0).handled()));
        none(&scenario.at(300.0).tick());
    }

    #[test]
    fn another_state_change_cancels_a_delayed_notification() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        // Warnings don't notify under the default rule.
        none(&scenario.at(100.0).input(warning(100.0)));
        none(&scenario.at(300.0).tick());
        none(&scenario.at(400.0).tick());
    }

    #[test]
    fn a_new_problem_state_starts_its_own_delay() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(100.0).input(unknown(100.0)));
        none(&scenario.at(300.0).tick());
        none(&scenario.at(399.0).tick());
        let intent = one(scenario.at(400.0).tick());
        assert_eq!(intent.title, "UNKNOWN · postgres-replication on db-prod-03");
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:unknown:1700000100.000"
        );
    }

    #[test]
    fn a_delay_that_ran_out_before_a_late_recovery_notifies_first() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        // No tick ran between 300 and the recovery at 400.
        assert_eq!(
            titles(&scenario.at(400.0).input(ok(400.0))),
            [
                "CRITICAL · postgres-replication on db-prod-03",
                "RECOVERED · postgres-replication on db-prod-03"
            ]
        );
    }

    #[test]
    fn a_recovery_before_the_delay_ran_out_cancels_even_if_delivered_late() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        // The recovery happened at 250 but reaches the engine at 400.
        none(&scenario.at(400.0).input(ok(250.0)));
        none(&scenario.at(500.0).tick());
    }

    #[test]
    fn the_earliest_matching_rule_decides() {
        let mut scenario = Scenario::new(rules(vec![group(
            "g",
            ScopeSetting::Inherit,
            vec![
                dashboard("slow", ScopeSetting::Custom(delayed(600))),
                dashboard("fast", ScopeSetting::Custom(delayed(60))),
            ],
        )]));
        none(
            &scenario
                .at(0.0)
                .input(critical(0.0).on(&[("g", "slow"), ("g", "fast")])),
        );
        none(&scenario.at(59.0).tick());
        assert_eq!(one(scenario.at(60.0).tick()).subtitle, "g / fast");
        none(&scenario.at(600.0).tick());
    }

    #[test]
    fn when_several_delays_ran_out_the_first_dashboard_names_it() {
        let mut scenario = Scenario::new(rules(vec![group(
            "g",
            ScopeSetting::Inherit,
            vec![
                dashboard("slow", ScopeSetting::Custom(delayed(600))),
                dashboard("fast", ScopeSetting::Custom(delayed(60))),
            ],
        )]));
        none(
            &scenario
                .at(0.0)
                .input(critical(0.0).on(&[("g", "slow"), ("g", "fast")])),
        );
        // The engine was not ticked for a long time.
        assert_eq!(one(scenario.at(700.0).tick()).subtitle, "g / slow");
    }

    #[test]
    fn an_immediate_rule_beats_a_delayed_one() {
        let mut scenario = Scenario::new(rules(vec![group(
            "g",
            ScopeSetting::Inherit,
            vec![
                dashboard("slow", ScopeSetting::Custom(delayed(600))),
                dashboard("now", ScopeSetting::On),
            ],
        )]));
        let intent = one(scenario
            .at(0.0)
            .input(critical(0.0).on(&[("g", "slow"), ("g", "now")])));
        assert_eq!(intent.subtitle, "g / now");
        none(&scenario.at(600.0).tick());
    }

    #[test]
    fn new_rules_apply_to_waiting_notifications_on_the_next_tick() {
        let mut scenario = Scenario::new(delayed_rules(600));
        none(&scenario.at(0.0).input(critical(0.0)));
        scenario.engine.set_rules(delayed_rules(60));
        let intent = one(scenario.at(100.0).tick());
        assert_eq!(intent.at, ts(0.0));
    }

    #[test]
    fn new_rules_that_shorten_a_delay_count_before_a_late_recovery() {
        let mut scenario = Scenario::new(delayed_rules(600));
        none(&scenario.at(0.0).input(critical(0.0)));
        scenario.engine.set_rules(delayed_rules(60));
        // No tick ran since the rules changed.
        assert_eq!(
            titles(&scenario.at(200.0).input(ok(200.0))),
            [
                "CRITICAL · postgres-replication on db-prod-03",
                "RECOVERED · postgres-replication on db-prod-03"
            ]
        );
    }

    #[test]
    fn turning_a_scope_off_drops_its_waiting_notification() {
        let on = rules(vec![group(
            "g",
            ScopeSetting::Inherit,
            vec![dashboard("d", ScopeSetting::Custom(delayed(300)))],
        )]);
        let mut scenario = Scenario::new(on.clone());
        none(&scenario.at(0.0).input(critical(0.0).on(&[("g", "d")])));
        let mut off = on;
        off.groups[0].dashboards[0].setting = ScopeSetting::Off;
        scenario.engine.set_rules(off);
        none(&scenario.at(10.0).tick());
        none(&scenario.at(300.0).tick());
    }

    #[test]
    fn a_repeat_of_the_same_state_keeps_the_delay() {
        let mut scenario = Scenario::new(delayed_rules(300));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(200.0).input(critical(0.0).happened(200.0)));
        one(scenario.at(300.0).tick());
    }
}

// ---------------------------------------------------------------------------
// Handling ending: Icinga-style notifications for problems that were handled
// ---------------------------------------------------------------------------

mod handling {
    use super::*;

    #[test]
    fn a_problem_that_began_in_a_downtime_notifies_when_the_downtime_ends() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(downtime_started(0.0)));
        none(&scenario.at(100.0).input(critical(100.0).handled()));
        none(&scenario.at(1_000.0).input(downtime_ended(1_000.0)));
        none(&scenario.at(1_009.0).tick());
        let intent = one(scenario.at(1_010.0).tick());
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700000100.000"
        );
        assert_eq!(intent.at, ts(100.0));
        // And its recovery notifies, too.
        one(scenario.at(2_000.0).input(ok(2_000.0)));
    }

    #[test]
    fn a_problem_that_recovered_during_the_downtime_stays_silent() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(100.0).input(critical(100.0).handled()));
        none(&scenario.at(500.0).input(ok(500.0)));
        none(&scenario.at(1_000.0).input(downtime_ended(1_000.0)));
        none(&scenario.at(1_100.0).tick());
    }

    #[test]
    fn a_downtime_ending_while_still_acknowledged_keeps_it_quiet() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(100.0).input(critical(100.0).handled()));
        none(
            &scenario
                .at(1_000.0)
                .input(downtime_ended(1_000.0).handled()),
        );
        none(&scenario.at(2_000.0).tick());
        // Until the acknowledgement goes away too.
        none(&scenario.at(3_000.0).input(ack_cleared(3_000.0)));
        one(scenario.at(3_010.0).tick());
    }

    #[test]
    fn an_expired_acknowledgement_brings_back_a_delayed_notification() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule = delayed(300);
        }));
        none(&scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(60.0).input(ack(60.0)));
        none(&scenario.at(120.0).input(ack_cleared(120.0)));
        none(&scenario.at(299.0).tick());
        let intent = one(scenario.at(300.0).tick());
        assert_eq!(
            intent.title,
            "CRITICAL · postgres-replication on db-prod-03"
        );
    }

    #[test]
    fn an_acknowledgement_cleared_by_a_state_change_does_not_notify_the_old_state() {
        // Icinga clears a normal acknowledgement on a state change and
        // reports the clearing first.
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(critical(0.0).handled()));
        none(&scenario.at(500.0).input(ack_cleared(500.0)));
        none(&scenario.at(500.0).input(warning(500.0)));
        none(&scenario.at(600.0).tick());
    }

    #[test]
    fn a_problem_notified_before_the_acknowledgement_does_not_repeat() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(60.0).input(ack(60.0)));
        none(&scenario.at(120.0).input(ack_cleared(120.0)));
        none(&scenario.at(1_000.0).tick());
    }

    #[test]
    fn a_soft_problem_handled_and_released_stays_quiet_under_hard_only() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.at(0.0).input(soft_critical(0.0).handled()));
        none(&scenario.at(30.0).input(ack_cleared(30.0)));
        none(&scenario.at(100.0).tick());
    }

    #[test]
    fn a_repeated_state_with_handling_gone_notifies() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        // Handled because its host is down.
        none(&scenario.at(0.0).input(critical(0.0).handled()));
        // The host came back: the caller repeats the service's state (same
        // `since`) with `handled` cleared, as `on_input` documents.
        let intent = one(scenario.at(120.0).input(critical(0.0).happened(120.0)));
        assert_eq!(
            intent.id,
            "db-prod-03!postgres-replication:critical:1700000000.000"
        );
        assert_eq!(intent.at, ts(120.0));
        // Repeating it again changes nothing.
        none(&scenario.at(180.0).input(critical(0.0).happened(180.0)));
    }
}

// ---------------------------------------------------------------------------
// Other events
// ---------------------------------------------------------------------------

mod events {
    use super::*;

    fn events_rules() -> RuleSet {
        with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.events = all_events();
            settings.storm.window_secs = 0;
        })
    }

    #[test]
    fn events_are_off_by_default() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        none(&scenario.input(ack(0.0)));
        none(&scenario.input(ack_cleared(1.0)));
        none(&scenario.input(downtime_started(2.0)));
        none(&scenario.input(downtime_ended(3.0)));
        none(&scenario.input(event(Change::FlappingStarted, 4.0)));
        none(&scenario.input(event(Change::FlappingStopped, 5.0)));
    }

    #[test]
    fn every_event_kind_has_a_title_body_and_id() {
        let mut scenario = Scenario::new(events_rules());
        let acknowledged = one(scenario.input(ack(1.0)));
        assert_eq!(
            acknowledged.title,
            "ACKNOWLEDGED · postgres-replication on db-prod-03"
        );
        assert_eq!(acknowledged.body, "m.keller: looking into it");
        assert_eq!(acknowledged.tone, Tone::Info);
        assert_eq!(
            acknowledged.id,
            "db-prod-03!postgres-replication:ack:1700000001.000"
        );
        assert_eq!(acknowledged.at, ts(1.0));

        let cleared = one(scenario.input(ack_cleared(2.0)));
        assert_eq!(
            cleared.title,
            "ACK CLEARED · postgres-replication on db-prod-03"
        );
        assert_eq!(cleared.body, "");

        let downtime = one(scenario.input(downtime_started(3.0)));
        assert_eq!(
            downtime.title,
            "DOWNTIME · postgres-replication on db-prod-03"
        );
        assert_eq!(downtime.body, "ops: kernel update");
        assert_eq!(
            downtime.id,
            "db-prod-03!postgres-replication:downtime-start:1700000003.000"
        );

        let ended = one(scenario.input(downtime_ended(4.0)));
        assert_eq!(
            ended.title,
            "DOWNTIME ENDED · postgres-replication on db-prod-03"
        );

        let flapping = one(scenario.input(event(Change::FlappingStarted, 5.0)));
        assert_eq!(
            flapping.title,
            "FLAPPING · postgres-replication on db-prod-03"
        );
        assert_eq!(flapping.tone, Tone::Info);

        let stopped = one(scenario.input(event(Change::FlappingStopped, 6.0)));
        assert_eq!(
            stopped.title,
            "FLAPPING STOPPED · postgres-replication on db-prod-03"
        );
        assert_eq!(
            stopped.id,
            "db-prod-03!postgres-replication:flapping-end:1700000006.000"
        );
    }

    #[test]
    fn event_kinds_are_enabled_separately() {
        let mut scenario = Scenario::new(with_settings(rules(Vec::new()), |settings| {
            settings.default_rule.events.downtimes = true;
        }));
        none(&scenario.input(ack(1.0)));
        one(scenario.input(downtime_started(2.0)));
        none(&scenario.input(event(Change::FlappingStarted, 3.0)));
    }

    #[test]
    fn events_follow_scopes_and_ignore_skip_handled() {
        let acks = rule(|rule| rule.events.acknowledgements = true);
        let mut scenario = Scenario::new(rules(vec![group(
            "db",
            ScopeSetting::Inherit,
            vec![
                dashboard("quiet", ScopeSetting::Off),
                dashboard("acks", ScopeSetting::Custom(acks)),
            ],
        )]));
        none(&scenario.input(ack(1.0).on(&[("db", "quiet")])));
        let intent = one(scenario.input(ack(2.0).on(&[("db", "quiet"), ("db", "acks")])));
        assert_eq!(intent.subtitle, "db / acks");
        assert!(intent.title.starts_with("ACKNOWLEDGED"));
    }

    #[test]
    fn a_replayed_event_notifies_once() {
        let mut scenario = Scenario::new(events_rules());
        one(scenario.at(0.0).input(ack(0.0)));
        none(&scenario.at(5.0).input(ack(0.0)));
        one(scenario.at(6.0).input(ack(6.0)));
    }

    #[test]
    fn a_host_event_names_the_host() {
        let mut scenario = Scenario::new(events_rules());
        let mut input = downtime_started(0.0);
        input.object = ObjectKey::host("k8s-node-07");
        input.host_display = "k8s-node-07".to_owned();
        input.service_display = None;
        assert_eq!(one(scenario.input(input)).title, "DOWNTIME · k8s-node-07");
    }
}

// ---------------------------------------------------------------------------
// Dedupe
// ---------------------------------------------------------------------------

mod dedupe {
    use super::*;

    #[test]
    fn repeated_identical_inputs_notify_once() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        none(&scenario.at(10.0).input(critical(0.0)));
        none(&scenario.at(20.0).input(critical(0.0)));
        none(&scenario.at(30.0).tick());
    }

    #[test]
    fn a_replay_after_the_recovery_notifies_nothing() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        one(scenario.at(10.0).input(ok(10.0)));
        // A reconnect replays both changes.
        none(&scenario.at(20.0).input(critical(0.0).happened(20.0)));
        none(&scenario.at(21.0).input(ok(10.0).happened(21.0)));
    }

    #[test]
    fn the_same_change_through_several_dashboards_notifies_once() {
        let mut scenario = Scenario::new(rules(vec![group(
            "g",
            ScopeSetting::Inherit,
            vec![
                dashboard("a", ScopeSetting::On),
                dashboard("b", ScopeSetting::On),
            ],
        )]));
        one(scenario.input(critical(0.0).on(&[("g", "a"), ("g", "b")])));
        // Even if the caller sent it once per dashboard.
        none(&scenario.input(critical(0.0).on(&[("g", "b")])));
    }

    #[test]
    fn the_same_state_again_later_is_a_new_problem() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        one(scenario.at(0.0).input(critical(0.0)));
        one(scenario.at(10.0).input(ok(10.0)));
        let again = one(scenario.at(20.0).input(critical(20.0)));
        assert_eq!(
            again.id,
            "db-prod-03!postgres-replication:critical:1700000020.000"
        );
    }
}

// ---------------------------------------------------------------------------
// Storm control
// ---------------------------------------------------------------------------

mod storm {
    use super::*;

    fn storm_rules(window_secs: u32, threshold: u32) -> RuleSet {
        with_settings(rules(Vec::new()), |settings| {
            settings.storm = StormControl {
                window_secs,
                threshold,
            };
        })
    }

    fn problem(index: u32, at: f64) -> RuleInput {
        critical(at).service(&format!("svc-{index:02}"))
    }

    #[test]
    fn a_storm_is_silenced_after_the_threshold_and_summarized() {
        let mut scenario = Scenario::new(storm_rules(10, 3));
        let mut silent = Vec::new();
        for index in 0..14 {
            let at = f64::from(index) * 0.5;
            let intent = one(scenario.at(at).input(problem(index, at)));
            silent.push(intent.silent);
        }
        assert_eq!(silent.iter().filter(|silent| !**silent).count(), 3);
        assert!(
            silent[..3].iter().all(|silent| !silent),
            "the first three are shown"
        );

        none(&scenario.at(9.9).tick());
        let summary = one(scenario.at(10.0).tick());
        assert_eq!(summary.title, "14 new problems in prod-cluster");
        assert_eq!(summary.body, "14 critical");
        assert_eq!(summary.subtitle, ENV);
        assert_eq!(summary.object, None);
        assert_eq!(summary.tone, Tone::Info);
        assert!(!summary.silent);
        assert!(summary.sound);
        assert_eq!(summary.id, "storm:1700000000.000");
        assert_eq!(summary.at, ts(10.0));

        // The next window starts fresh.
        assert!(!one(scenario.at(11.0).input(problem(99, 11.0))).silent);
        none(&scenario.at(21.0).tick());
    }

    #[test]
    fn exactly_the_threshold_is_no_storm() {
        let mut scenario = Scenario::new(storm_rules(10, 3));
        for index in 0..3 {
            assert!(!one(scenario.at(0.0).input(problem(index, 0.0))).silent);
        }
        none(&scenario.at(10.0).tick());
    }

    #[test]
    fn the_summary_breaks_down_mixed_windows() {
        let mut scenario = Scenario::new(storm_rules(10, 1));
        one(scenario.at(0.0).input(critical(0.0)));
        one(scenario.at(1.0).input(host_state(
            "k8s-node-07",
            HostState::Down,
            StateType::Hard,
            1.0,
        )));
        one(scenario.at(2.0).input(ok(2.0)));
        let summary = one(scenario.at(10.0).tick());
        assert_eq!(summary.title, "3 notifications in prod-cluster");
        assert_eq!(summary.body, "1 critical · 1 down · 1 recovered");
    }

    #[test]
    fn a_summary_comes_out_of_the_next_input_if_no_tick_closed_the_window() {
        let mut scenario = Scenario::new(storm_rules(10, 1));
        one(scenario.at(0.0).input(problem(0, 0.0)));
        assert!(one(scenario.at(1.0).input(problem(1, 1.0))).silent);
        let intents = scenario.at(12.0).input(problem(2, 12.0));
        assert_eq!(
            titles(&intents),
            [
                "2 new problems in prod-cluster",
                "CRITICAL · svc-02 on db-prod-03"
            ]
        );
        assert!(!intents[1].silent, "it opened a new window");
    }

    #[test]
    fn released_delays_count_towards_storms() {
        let mut scenario = Scenario::new(with_settings(storm_rules(10, 1), |settings| {
            settings.default_rule = delayed(60);
        }));
        for index in 0..3 {
            none(&scenario.at(0.0).input(problem(index, 0.0)));
        }
        let released = scenario.at(60.0).tick();
        assert_eq!(released.iter().filter(|intent| !intent.silent).count(), 1);
        assert_eq!(released.len(), 3);
        assert_eq!(
            one(scenario.at(70.0).tick()).title,
            "3 new problems in prod-cluster"
        );
    }

    #[test]
    fn a_zero_window_disables_storm_control() {
        let mut scenario = Scenario::new(storm_rules(0, 0));
        for index in 0..20 {
            assert!(!one(scenario.at(0.0).input(problem(index, 0.0))).silent);
        }
        none(&scenario.at(100.0).tick());
    }

    #[test]
    fn notifications_silenced_by_quiet_hours_do_not_count() {
        let mut scenario = Scenario::new(with_settings(storm_rules(10, 2), |settings| {
            settings.default_rule.states.warning = true;
            settings.quiet_hours = QuietHours {
                enabled: true,
                start_minute: 22 * 60,
                end_minute: 7 * 60,
                days: [true; 7],
                allow_critical: true,
            };
        }));
        scenario.local(local(MONDAY, 23, 0));
        for index in 0..5 {
            let intent = one(scenario
                .at(0.0)
                .input(warning(0.0).service(&format!("w{index}"))));
            assert!(intent.silent);
        }
        // Criticals stay audible at night, and the warnings didn't use up
        // the storm budget.
        assert!(!one(scenario.at(1.0).input(problem(0, 1.0))).silent);
        assert!(!one(scenario.at(1.0).input(problem(1, 1.0))).silent);
        assert!(one(scenario.at(1.0).input(problem(2, 1.0))).silent);
        let summary = one(scenario.at(11.0).tick());
        assert_eq!(summary.title, "3 new problems in prod-cluster");
        assert!(
            !summary.silent,
            "it absorbed a critical, which quiet hours allow"
        );
    }

    #[test]
    fn a_summary_is_silent_while_paused() {
        let mut scenario = Scenario::new(storm_rules(10, 1));
        one(scenario.at(0.0).input(problem(0, 0.0)));
        one(scenario.at(1.0).input(problem(1, 1.0)));
        scenario.engine.pause_until(Some(ts(3_600.0)));
        assert!(one(scenario.at(10.0).tick()).silent);
    }

    #[test]
    fn a_summary_is_silent_in_quiet_hours_unless_it_absorbed_a_critical() {
        let mut scenario = Scenario::new(with_settings(storm_rules(10, 1), |settings| {
            settings.default_rule.states.warning = true;
            settings.quiet_hours = QuietHours {
                enabled: true,
                start_minute: 22 * 60,
                end_minute: 7 * 60,
                days: [true; 7],
                allow_critical: true,
            };
        }));
        // Daytime: warnings are absorbed by the storm.
        one(scenario.at(0.0).input(warning(0.0).service("a")));
        one(scenario.at(1.0).input(warning(1.0).service("b")));
        // The window closes at night: the summary of warnings is silent.
        assert!(one(scenario.local(local(MONDAY, 22, 0)).at(10.0).tick()).silent);
    }
}

// ---------------------------------------------------------------------------
// Quiet hours and pause
// ---------------------------------------------------------------------------

mod quiet_hours {
    use super::*;

    fn friday_nights(allow_critical: bool) -> RuleSet {
        let mut days = [false; 7];
        days[usize::from(FRIDAY)] = true;
        with_settings(rules(Vec::new()), |settings| {
            settings.quiet_hours = QuietHours {
                enabled: true,
                start_minute: 22 * 60,
                end_minute: 7 * 60,
                days,
                allow_critical,
            };
            settings.default_rule.states.warning = true;
            settings.storm.window_secs = 0;
        })
    }

    fn silent_at(scenario: &mut Scenario, time: LocalTime, service: &str) -> bool {
        one(scenario.local(time).input(warning(0.0).service(service))).silent
    }

    #[test]
    fn quiet_hours_cross_midnight_on_the_right_days() {
        let mut scenario = Scenario::new(friday_nights(false));
        assert!(!silent_at(&mut scenario, local(FRIDAY, 21, 59), "a"));
        assert!(silent_at(&mut scenario, local(FRIDAY, 22, 0), "b"));
        assert!(silent_at(&mut scenario, local(FRIDAY, 23, 59), "c"));
        assert!(
            silent_at(&mut scenario, local(SATURDAY, 0, 0), "d"),
            "Friday's night goes on"
        );
        assert!(silent_at(&mut scenario, local(SATURDAY, 6, 59), "e"));
        assert!(!silent_at(&mut scenario, local(SATURDAY, 7, 0), "f"));
        assert!(
            !silent_at(&mut scenario, local(SATURDAY, 23, 0), "g"),
            "Saturday isn't selected"
        );
        assert!(
            !silent_at(&mut scenario, local(FRIDAY, 3, 0), "h"),
            "Friday morning belongs to Thursday's night"
        );
    }

    #[test]
    fn allow_critical_keeps_critical_and_down_audible() {
        let mut scenario = Scenario::new(with_settings(friday_nights(true), |settings| {
            settings.default_rule.states.unreachable = true;
            settings.default_rule.events = all_events();
        }));
        scenario.local(local(FRIDAY, 23, 0));
        assert!(!one(scenario.input(critical(0.0))).silent);
        assert!(
            !one(scenario.input(host_state("n1", HostState::Down, StateType::Hard, 0.0))).silent
        );
        assert!(one(scenario.input(warning(0.0).service("w"))).silent);
        assert!(one(scenario.input(unknown(0.0).service("u"))).silent);
        assert!(
            one(scenario.input(host_state(
                "n2",
                HostState::Unreachable,
                StateType::Hard,
                0.0
            )))
            .silent
        );
        assert!(
            one(scenario.at(10.0).input(ok(10.0))).silent,
            "recoveries are quiet"
        );
        assert!(one(scenario.input(ack(11.0))).silent, "events are quiet");
    }

    #[test]
    fn a_delayed_notification_released_in_quiet_hours_is_silent() {
        let mut scenario = Scenario::new(with_settings(friday_nights(false), |settings| {
            settings.default_rule.min_duration_secs = 600;
        }));
        none(
            &scenario
                .local(local(FRIDAY, 21, 55))
                .at(0.0)
                .input(critical(0.0)),
        );
        assert!(one(scenario.local(local(FRIDAY, 22, 5)).at(600.0).tick()).silent);
    }

    #[test]
    fn a_pause_silences_everything_until_it_ends() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        scenario.engine.pause_until(Some(ts(100.0)));
        assert_eq!(scenario.engine.paused_until(), Some(ts(100.0)));
        assert!(one(scenario.at(0.0).input(critical(0.0))).silent);
        assert!(one(scenario.at(99.0).input(critical(99.0).service("b"))).silent);

        none(&scenario.at(100.0).tick());
        assert_eq!(
            scenario.engine.paused_until(),
            None,
            "an expired pause reads as none"
        );
        assert!(!one(scenario.at(101.0).input(critical(101.0).service("c"))).silent);
    }

    #[test]
    fn resuming_ends_the_pause_at_once() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        scenario.engine.pause_until(Some(ts(1_000.0)));
        assert!(one(scenario.at(0.0).input(critical(0.0))).silent);
        scenario.engine.pause_until(None);
        assert_eq!(scenario.engine.paused_until(), None);
        assert!(!one(scenario.at(1.0).input(critical(1.0).service("b"))).silent);
    }

    #[test]
    fn a_recovery_after_a_paused_problem_still_counts_as_notified() {
        let mut scenario = Scenario::new(rules(Vec::new()));
        scenario.engine.pause_until(Some(ts(100.0)));
        assert!(one(scenario.at(0.0).input(critical(0.0))).silent);
        assert!(!one(scenario.at(200.0).input(ok(200.0))).silent);
    }
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

mod load {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    #[ignore = "benchmark; run with `cargo test --release -p ic-rules -- --ignored`"]
    fn a_full_outage_of_twenty_thousand_services_stays_fast() {
        let groups = (0..5)
            .map(|group_index| {
                let dashboards = vec![
                    dashboard("delayed", ScopeSetting::Custom(delayed(60))),
                    dashboard("now", ScopeSetting::Custom(Rule::default())),
                ];
                group(
                    &format!("g{group_index}"),
                    ScopeSetting::Inherit,
                    dashboards,
                )
            })
            .collect();
        let mut scenario = Scenario::new(rules(groups));
        let names: Vec<String> = (0..5)
            .map(|group_index| format!("g{group_index}"))
            .collect();
        let memberships: Vec<(&str, &str)> = names
            .iter()
            .flat_map(|group| [(group.as_str(), "delayed"), (group.as_str(), "now")])
            .collect();

        let started = Instant::now();
        let mut intents = 0;
        for index in 0..20_000_u32 {
            let at = f64::from(index / 200);
            let input = critical(at)
                .service(&format!("svc-{index:05}"))
                .on(&memberships);
            intents += scenario.at(at).input(input).len();
        }
        for second in 100..300 {
            intents += scenario.at(f64::from(second)).tick().len();
        }
        for index in 0..20_000_u32 {
            let input = ok(300.0)
                .service(&format!("svc-{index:05}"))
                .on(&memberships);
            intents += scenario.at(300.0).input(input).len();
        }
        let elapsed = started.elapsed();

        // Every problem and every recovery, plus storm summaries.
        assert!(intents >= 40_000, "{intents}");
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }
}
