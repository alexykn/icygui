//! Randomized invariant tests: long, seeded random sequences of inputs,
//! ticks, pauses and rule changes, checked against properties that must
//! hold whatever the rules say.

use std::collections::{HashMap, HashSet};

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, StateType, Timestamp};
use ic_rules::{
    Change, DashboardRef, DashboardScope, EventFilter, GroupScope, LocalTime, NotificationIntent,
    NotificationSettings, ObjectMode, ObjectOverride, QuietHours, Rule, RuleEngine, RuleInput,
    RuleSet, ScopeSetting, StateFilter, StormControl, Tone,
};

const T0: f64 = 1_700_000_000.0;
const STORM_WINDOW: u32 = 20;
const STORM_THRESHOLD: u32 = 3;
const MUTED_UNTIL: u32 = 3_000;

/// xorshift64*: small, deterministic, good enough to shuffle scenarios.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n.max(1))).unwrap_or(0)
    }

    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T], fallback: T) -> T {
        let len = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let index = usize::try_from(self.below(len)).unwrap_or(0);
        items.get(index).copied().unwrap_or(fallback)
    }
}

fn ts(offset: u32) -> Timestamp {
    Timestamp::from_unix_seconds(T0 + f64::from(offset))
}

fn local_time(start_minute: u32, offset: u32) -> LocalTime {
    let minutes = start_minute + offset / 60;
    LocalTime {
        weekday: u8::try_from(minutes / 1_440 % 7).unwrap_or(0),
        minute_of_day: u16::try_from(minutes % 1_440).unwrap_or(0),
    }
}

fn random_rule(rng: &mut Rng) -> Rule {
    Rule {
        states: StateFilter {
            critical: rng.chance(90),
            warning: rng.chance(50),
            unknown: rng.chance(70),
            down: rng.chance(90),
            unreachable: rng.chance(30),
            recovery: rng.chance(80),
        },
        hard_only: rng.chance(60),
        skip_handled: rng.chance(70),
        events: EventFilter {
            acknowledgements: rng.chance(50),
            downtimes: rng.chance(50),
            flapping: rng.chance(50),
        },
        min_duration_secs: rng.pick(&[0, 0, 30, 120], 0),
        sound: rng.chance(50),
    }
}

fn random_setting(rng: &mut Rng) -> ScopeSetting {
    match rng.below(4) {
        0 => ScopeSetting::Inherit,
        1 => ScopeSetting::Off,
        2 => ScopeSetting::On,
        _ => ScopeSetting::Custom(random_rule(rng)),
    }
}

fn dashboard_ref(group: &str, dashboard: &str) -> DashboardRef {
    DashboardRef {
        group_id: group.to_owned(),
        dashboard_id: dashboard.to_owned(),
    }
}

fn objects() -> Vec<ObjectKey> {
    vec![
        ObjectKey::service("h0", "muted"),
        ObjectKey::service("h0", "watched"),
        ObjectKey::service("h0", "s2"),
        ObjectKey::service("h0", "s3"),
        ObjectKey::service("h1", "s4"),
        ObjectKey::host("h0"),
        ObjectKey::host("h1"),
    ]
}

fn random_rules(rng: &mut Rng, objects: &[ObjectKey]) -> RuleSet {
    let mut groups = Vec::new();
    for group in ["g1", "g2"] {
        groups.push(GroupScope {
            id: group.to_owned(),
            name: group.to_owned(),
            setting: random_setting(rng),
            dashboards: ["d1", "d2"]
                .into_iter()
                .map(|dashboard| DashboardScope {
                    id: dashboard.to_owned(),
                    name: dashboard.to_owned(),
                    setting: random_setting(rng),
                })
                .collect(),
        });
    }
    let start = rng.below(1_440);
    RuleSet {
        environment_name: "prod".to_owned(),
        settings: NotificationSettings {
            enabled: rng.chance(70),
            default_rule: random_rule(rng),
            quiet_hours: QuietHours {
                enabled: rng.chance(50),
                start_minute: u16::try_from(start).unwrap_or(0),
                end_minute: u16::try_from((start + 60 + rng.below(600)) % 1_440).unwrap_or(0),
                days: [true, true, false, true, true, false, true],
                allow_critical: rng.chance(50),
            },
            storm: StormControl {
                window_secs: STORM_WINDOW,
                threshold: STORM_THRESHOLD,
            },
            objects: vec![
                ObjectOverride {
                    object: objects
                        .first()
                        .cloned()
                        .unwrap_or_else(|| ObjectKey::host("x")),
                    mode: ObjectMode::Mute,
                    until: Some(ts(MUTED_UNTIL)),
                },
                ObjectOverride {
                    object: objects
                        .get(1)
                        .cloned()
                        .unwrap_or_else(|| ObjectKey::host("x")),
                    mode: ObjectMode::Watch,
                    until: None,
                },
            ],
        },
        groups,
    }
}

fn random_state(rng: &mut Rng, object: &ObjectKey) -> CheckableState {
    if object.as_service().is_some() {
        CheckableState::Service(rng.pick(
            &[
                ServiceState::Ok,
                ServiceState::Ok,
                ServiceState::Warning,
                ServiceState::Critical,
                ServiceState::Critical,
                ServiceState::Unknown,
                ServiceState::Pending,
            ],
            ServiceState::Ok,
        ))
    } else {
        CheckableState::Host(rng.pick(
            &[
                HostState::Up,
                HostState::Up,
                HostState::Down,
                HostState::Down,
                HostState::Unreachable,
            ],
            HostState::Up,
        ))
    }
}

fn is_problem_tone(tone: Tone) -> bool {
    matches!(tone, Tone::Critical | Tone::Warning | Tone::Unknown)
}

fn state_slug(state: CheckableState) -> &'static str {
    match state {
        CheckableState::Service(ServiceState::Ok) => "ok",
        CheckableState::Service(ServiceState::Warning) => "warning",
        CheckableState::Service(ServiceState::Critical) => "critical",
        CheckableState::Service(ServiceState::Unknown) => "unknown",
        CheckableState::Host(HostState::Up) => "up",
        CheckableState::Host(HostState::Down) => "down",
        CheckableState::Host(HostState::Unreachable) => "unreachable",
        CheckableState::Service(ServiceState::Pending)
        | CheckableState::Host(HostState::Pending) => "pending",
    }
}

fn is_up(state: CheckableState) -> bool {
    matches!(
        state,
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up)
    )
}

/// The state slug in a state intent's id (`host!service:critical:…`).
fn id_state(id: &str) -> &str {
    id.rsplit(':').nth(1).unwrap_or("")
}

/// The engine's rule for a flapping flag nobody cleared: an hour without a
/// state change ends it.
const FLAPPING_EXPIRY: u32 = 3_600;

/// The state the test knows an object is in.
#[derive(Clone, Copy)]
struct Known {
    state: CheckableState,
    since: u32,
}

/// What the test has seen of one object's notifications.
#[derive(Clone, Copy, Default)]
struct History {
    /// A problem intent was emitted since the last recovery intent.
    problem_since_recovery: bool,
    /// The state of the last problem intent, while the object has stayed
    /// in problem states since.
    last_problem: Option<&'static str>,
    /// Flapping, with the time of the last evidence (start or state change).
    flapping_since: Option<u32>,
}

impl History {
    fn is_flapping(&self, now: u32) -> bool {
        self.flapping_since
            .is_some_and(|evidence| now < evidence + FLAPPING_EXPIRY)
    }
}

/// What a batch of intents came out of.
#[derive(Clone, Copy)]
enum Cause<'a> {
    /// A tick.
    Tick,
    /// An input for `object`.
    Input {
        object: &'a ObjectKey,
        change: InputKind,
    },
}

/// The kind of input, as far as the invariants care.
#[derive(Clone, Copy)]
enum InputKind {
    /// A state change (or repeat) to `state` since `since`.
    State(CheckableState, u32),
    FlappingStarted,
    FlappingStopped,
    OtherEvent,
}

/// What the random runs produced, to make sure they exercise the engine.
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    intents: usize,
    silent: usize,
    recoveries: usize,
    summaries: usize,
    events: usize,
    released_by_tick: usize,
    held_while_flapping: usize,
}

impl Stats {
    fn add(&mut self, other: Self) {
        self.intents += other.intents;
        self.silent += other.silent;
        self.recoveries += other.recoveries;
        self.summaries += other.summaries;
        self.events += other.events;
        self.released_by_tick += other.released_by_tick;
        self.held_while_flapping += other.held_while_flapping;
    }
}

/// Checks batches of intents against the invariants.
struct Checker {
    seen_ids: HashSet<String>,
    audible_times: Vec<u32>,
    known: HashMap<ObjectKey, Known>,
    history: HashMap<ObjectKey, History>,
    stats: Stats,
}

impl Checker {
    fn check(
        &mut self,
        intents: &[NotificationIntent],
        now: u32,
        paused: bool,
        cause: Cause<'_>,
        context: &str,
    ) {
        // Flapping that stops ends before the intents it releases.
        if let Cause::Input {
            object,
            change: InputKind::FlappingStopped,
        } = cause
        {
            self.history
                .entry(object.clone())
                .or_default()
                .flapping_since = None;
        }
        // A state change happens before the intents it causes; the state
        // before it is what a delay that ran out first is about.
        if let Cause::Input {
            object,
            change: InputKind::State(state, since),
        } = cause
        {
            let previous = self.known.get(object).copied();
            let repeat = previous.is_some_and(|known| known.state == state && known.since == since);
            if !repeat {
                let history = self.history.entry(object.clone()).or_default();
                history.flapping_since = history.is_flapping(now).then_some(now);
            }
            self.known.insert(object.clone(), Known { state, since });
        }

        for intent in intents {
            self.check_one(intent, now, paused, cause, context);
        }

        if let Cause::Input { object, change } = cause {
            let history = self.history.entry(object.clone()).or_default();
            match change {
                InputKind::FlappingStarted => history.flapping_since = Some(now),
                InputKind::State(state, _) if !state.is_problem() => history.last_problem = None,
                InputKind::State(..) | InputKind::FlappingStopped | InputKind::OtherEvent => {}
            }
        }
    }

    fn check_one(
        &mut self,
        intent: &NotificationIntent,
        now: u32,
        paused: bool,
        cause: Cause<'_>,
        context: &str,
    ) {
        self.stats.intents += 1;
        self.stats.silent += usize::from(intent.silent);
        self.stats.recoveries += usize::from(intent.tone == Tone::Recovery);
        self.stats.summaries += usize::from(intent.object.is_none());
        self.stats.events += usize::from(intent.tone == Tone::Info && intent.object.is_some());
        assert!(
            self.seen_ids.insert(intent.id.clone()),
            "{context}: id emitted twice: {intent:#?}"
        );
        if paused {
            assert!(
                intent.silent,
                "{context}: audible while paused: {intent:#?}"
            );
        }
        let Some(object) = &intent.object else {
            assert!(intent.id.starts_with("storm:"), "{context}: {intent:#?}");
            assert_eq!(intent.tone, Tone::Info, "{context}");
            return;
        };
        if !intent.silent {
            self.audible_times.push(now);
        }
        if object == &ObjectKey::service("h0", "muted") {
            assert!(
                now >= MUTED_UNTIL,
                "{context}: notified a muted object: {intent:#?}"
            );
        }
        if let Cause::Input {
            object: input_object,
            ..
        } = cause
        {
            assert_eq!(object, input_object, "{context}: intent for another object");
        }
        let state_input = matches!(
            cause,
            Cause::Input {
                change: InputKind::State(..),
                ..
            }
        );
        let known = self.known.get(object).copied();
        let history = self.history.entry(object.clone()).or_default();

        if is_problem_tone(intent.tone) {
            assert!(
                !history.is_flapping(now),
                "{context}: a problem notified while flapping: {intent:#?}"
            );
            // Outside a state change, a problem intent is about the state
            // the object is in now: the engine never notifies a gone state.
            if !state_input {
                let slug = known.map_or("never seen", |known| state_slug(known.state));
                assert!(
                    known.is_some_and(|known| known.state.is_problem())
                        && intent.id.contains(&format!(":{slug}:")),
                    "{context}: notified a state the object isn't in ({slug}): {intent:#?}"
                );
            }
            if matches!(cause, Cause::Tick) {
                self.stats.released_by_tick += 1;
            }
            // A state notified during a problem isn't notified again until
            // another state was.
            let state = id_state(&intent.id);
            assert_ne!(
                history.last_problem.map(str::to_owned).as_deref(),
                Some(state),
                "{context}: the same state notified twice in one problem: {intent:#?}"
            );
            history.last_problem = slug_of(state);
            history.problem_since_recovery = true;
        } else if intent.tone == Tone::Recovery {
            assert!(
                !history.is_flapping(now),
                "{context}: a recovery notified while flapping: {intent:#?}"
            );
            assert!(
                known.is_some_and(|known| is_up(known.state)),
                "{context}: a recovery for an object that isn't OK / UP: {intent:#?}"
            );
            assert!(
                history.problem_since_recovery,
                "{context}: a recovery without a notified problem: {intent:#?}"
            );
            history.problem_since_recovery = false;
        }
    }

    /// Storm control: shown notifications within any window are bounded by
    /// the threshold.
    fn check_storm_rate(&self, seed: u64) {
        let limit = usize::try_from(STORM_THRESHOLD).unwrap_or(usize::MAX);
        for (index, start) in self.audible_times.iter().enumerate() {
            let in_window = self
                .audible_times
                .iter()
                .skip(index)
                .take_while(|time| **time < start + STORM_WINDOW)
                .count();
            assert!(
                in_window <= limit,
                "seed {seed}: {in_window} audible notifications within {STORM_WINDOW}s of {start}"
            );
        }
    }
}

/// The `'static` slug for a slug read from an id.
fn slug_of(slug: &str) -> Option<&'static str> {
    [
        "ok",
        "warning",
        "critical",
        "unknown",
        "up",
        "down",
        "unreachable",
        "pending",
    ]
    .into_iter()
    .find(|known| *known == slug)
}

fn random_change(
    rng: &mut Rng,
    object: &ObjectKey,
    previous: Option<Known>,
    now: u32,
) -> (Change, Option<(CheckableState, u32)>) {
    if rng.chance(60) {
        let state = random_state(rng, object);
        let since = match previous {
            Some(known) if known.state == state && !rng.chance(10) => known.since,
            _ => now,
        };
        let state_type = if state.is_problem() && rng.chance(40) {
            StateType::Soft
        } else {
            StateType::Hard
        };
        let change = Change::State {
            previous: previous.map(|known| known.state),
            current: state,
            state_type,
            since: ts(since),
            output: format!("{state:?} at {now}"),
        };
        return (change, Some((state, since)));
    }
    let change = match rng.below(6) {
        0 => Change::AcknowledgementSet {
            author: "a".to_owned(),
            comment: "c".to_owned(),
        },
        1 => Change::AcknowledgementCleared,
        2 => Change::DowntimeStarted {
            author: "a".to_owned(),
            comment: "c".to_owned(),
        },
        3 => Change::DowntimeEnded,
        4 => Change::FlappingStarted,
        _ => Change::FlappingStopped,
    };
    (change, None)
}

fn run(seed: u64, steps: u32) -> Stats {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let objects = objects();
    let memberships = [
        dashboard_ref("g1", "d1"),
        dashboard_ref("g1", "d2"),
        dashboard_ref("g2", "d1"),
        dashboard_ref("g2", "d2"),
        dashboard_ref("gone", "x"),
    ];
    let mut engine = RuleEngine::new(random_rules(&mut rng, &objects));
    let start_minute = rng.below(7 * 1_440);
    let mut checker = Checker {
        seen_ids: HashSet::new(),
        audible_times: Vec::new(),
        known: HashMap::new(),
        history: HashMap::new(),
        stats: Stats::default(),
    };
    let mut now = 0_u32;
    let mut paused_until: Option<u32> = None;

    for step in 0..steps {
        now += rng.below(15);
        let local = local_time(start_minute, now);
        let paused = paused_until.is_some_and(|until| now < until);
        let context = format!("seed {seed} step {step} at {now}");

        match rng.below(100) {
            0..25 => {
                let intents = engine.tick(ts(now), local);
                checker.check(&intents, now, paused, Cause::Tick, &context);
            }
            25..27 => {
                paused_until = if rng.chance(50) {
                    Some(now + rng.below(300))
                } else {
                    None
                };
                engine.pause_until(paused_until.map(ts));
            }
            27..29 => {
                let mut rules = random_rules(&mut rng, &objects);
                // Keep the overrides the checker knows about.
                rules
                    .settings
                    .objects
                    .clone_from(&engine.rules().settings.objects);
                engine.set_rules(rules);
            }
            _ => {
                let index = rng.pick(&[0, 1, 2, 3, 4, 5, 6], 0);
                let object = objects
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| ObjectKey::host("x"));
                let input_memberships: Vec<DashboardRef> = memberships
                    .iter()
                    .filter(|_| rng.chance(35))
                    .cloned()
                    .collect();
                let previous = checker.known.get(&object).copied();
                let (change, state) = random_change(&mut rng, &object, previous, now);
                let input = RuleInput {
                    object: object.clone(),
                    host_display: object.host_name().to_string(),
                    service_display: object.as_service().map(|key| key.name.to_string()),
                    change,
                    handled: rng.chance(30),
                    memberships: input_memberships,
                    at: ts(now.saturating_sub(rng.below(3))),
                };
                let kind = match (&input.change, state) {
                    (_, Some((state, since))) => InputKind::State(state, since),
                    (Change::FlappingStarted, None) => InputKind::FlappingStarted,
                    (Change::FlappingStopped, None) => InputKind::FlappingStopped,
                    _ => InputKind::OtherEvent,
                };
                let flapping_before = checker
                    .history
                    .get(&object)
                    .is_some_and(|history| history.is_flapping(now));
                let intents = engine.on_input(input, ts(now), local);
                checker.stats.held_while_flapping +=
                    usize::from(flapping_before && matches!(kind, InputKind::State(..)));
                let cause = Cause::Input {
                    object: &object,
                    change: kind,
                };
                checker.check(&intents, now, paused, cause, &context);
            }
        }
    }
    checker.check_storm_rate(seed);
    checker.stats
}

#[test]
fn invariants_hold_for_random_sequences() {
    let mut total = Stats::default();
    for seed in 1..=40 {
        total.add(run(seed, 2_500));
    }
    println!("{total:?}");
    // The runs must exercise every path the invariants talk about.
    assert!(total.intents > 5_000, "{total:?}");
    assert!(total.silent > 500, "{total:?}");
    assert!(total.recoveries > 300, "{total:?}");
    assert!(total.summaries > 20, "{total:?}");
    assert!(total.events > 500, "{total:?}");
    assert!(total.released_by_tick > 100, "{total:?}");
    assert!(total.held_while_flapping > 1_000, "{total:?}");
}
