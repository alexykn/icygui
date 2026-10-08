//! The simulator: a seeded, deterministic schedule of checks and incidents.
//!
//! Time is counted in ticks. Every decision uses the seeded generator and
//! the current state only (never the wall clock), so the same seed and tick
//! sequence produce the same events. A real-time driver in the server calls
//! [`World::sim_tick`] every `tick / speed`; tests call it directly through
//! `MockControl::step_simulation`.
//!
//! Per tick:
//! 1. Incidents are decided: new problems (churn), recoveries, flapping,
//!    outages (a parent host goes down, its dependents become unreachable),
//!    maintenance downtimes and storms.
//! 2. The checker runs every check that is due, like Icinga's checker:
//!    objects are re-checked at `check_interval`, at `retry_interval` while
//!    soft, so problems become hard after `max_check_attempts`.

use std::collections::BTreeMap;

use crate::config::SimulationConfig;
use crate::model::logic::DowntimeSpec;
use crate::model::{CheckInput, World};
use crate::outputs::{self, PluginResult};
use crate::rng::Rng;

/// Where the simulator steers an object.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    /// Raw state to report.
    state: u8,
    /// Fixed output, or `None` to generate one per check.
    result: Option<PluginResult>,
    /// Tick at which the object recovers.
    until: Option<u64>,
    /// Remaining state flips while flapping.
    flaps: u32,
    /// Re-check every tick (storms, flapping, outages).
    fast: bool,
}

/// An outage of a parent host.
#[derive(Clone, Debug, PartialEq)]
struct Outage {
    parent: String,
    until: u64,
}

/// Simulator state, kept inside the world (under its lock).
#[derive(Debug)]
pub(crate) struct SimState {
    config: SimulationConfig,
    rng: Rng,
    /// Whether the real-time driver ticks.
    pub(crate) running: bool,
    /// Ticks so far.
    pub(crate) tick: u64,
    /// Objects due for a check, by tick.
    due: BTreeMap<u64, Vec<String>>,
    targets: BTreeMap<String, Target>,
    outage: Option<Outage>,
    storm_until: Option<u64>,
    /// Hosts in a simulated maintenance window, until the given tick. The
    /// simulator tracks these itself: whether the downtime objects still
    /// exist depends on the wall clock, which must not steer decisions.
    maintenance: BTreeMap<String, u64>,
}

impl Default for SimState {
    fn default() -> Self {
        Self {
            config: SimulationConfig::default(),
            rng: Rng::new(0),
            running: false,
            tick: 0,
            due: BTreeMap::new(),
            targets: BTreeMap::new(),
            outage: None,
            storm_until: None,
            maintenance: BTreeMap::new(),
        }
    }
}

impl SimState {
    /// Simulated seconds per tick.
    pub(crate) fn tick_seconds(&self) -> f64 {
        self.config.tick.as_secs_f64().max(0.001)
    }

    /// Real seconds per tick.
    pub(crate) fn real_tick_seconds(&self) -> f64 {
        self.tick_seconds() / self.config.speed.max(0.001)
    }

    fn ticks(&self, seconds: f64) -> u64 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "positive and far below u64::MAX for any sane interval"
        )]
        let ticks = (seconds / self.tick_seconds()).ceil().max(1.0) as u64;
        ticks
    }

    /// Probability per tick of something that happens `per_hour` times an
    /// hour of simulated time.
    fn per_tick(&self, per_hour: f64) -> f64 {
        (per_hour * self.tick_seconds() / 3_600.0).max(0.0)
    }

    fn schedule(&mut self, tick: u64, object: String) {
        self.due.entry(tick).or_default().push(object);
    }
}

/// Upper bound of checks per tick; the rest waits for the next tick.
const MAX_CHECKS_PER_TICK: usize = 5_000;

impl World {
    /// Configures the simulator. Each object's first check comes at its
    /// `next_check`, as Icinga's checker would run it (a soft problem's
    /// retry a minute after the scenario starts): clients see that time,
    /// and a later first check would show as late. Objects without a
    /// `next_check` ahead are spread over their interval.
    pub(crate) fn sim_configure(&mut self, config: &SimulationConfig) {
        self.sim.config = config.clone();
        self.sim.rng = Rng::derive(config.seed, 0x0073_696d);
        self.sim.running = config.enabled;
        self.sim.tick = 0;
        self.sim.due.clear();
        self.sim.targets.clear();
        self.sim.outage = None;
        self.sim.storm_until = None;
        self.sim.maintenance.clear();
        // Measured from the scenario's load, not the wall clock, so the
        // same seed tells the same story however long starting took.
        let start = self.loaded_at;
        let speed = config.speed.max(0.001);
        let objects: Vec<(String, f64, f64)> = self
            .all_checkables()
            .filter(|c| c.enable_active_checks)
            .map(|c| (c.full_name(), c.check_interval, c.next_check))
            .collect();
        for (object, interval, next_check) in objects {
            let ticks = self.sim.ticks(interval);
            // Drawn for every object, so the seed's story doesn't depend
            // on which objects have a next check ahead.
            let spread = 1 + self.sim.rng.below(ticks);
            let due_in = next_check - start;
            let first = if due_in.is_finite() && due_in > 0.0 {
                // Wall-clock seconds to ticks: a tick takes
                // `tick / speed` real seconds.
                self.sim.ticks(due_in * speed)
            } else {
                spread
            };
            self.sim.schedule(first, object);
        }
    }

    /// Advances the simulation by one tick.
    pub(crate) fn sim_tick(&mut self) {
        self.sim.tick += 1;
        let tick = self.sim.tick;
        self.sim_recoveries(tick);
        self.sim_churn(tick);
        if self.sim.config.flapping {
            self.sim_flapping(tick);
        }
        if self.sim.config.outages {
            self.sim_outage(tick);
        }
        if self.sim.config.downtimes {
            self.sim_downtime(tick);
        }
        self.sim_storm(tick);
        if self.sim.config.checks {
            self.sim_checker(tick);
        } else {
            // Still run what incidents need (targets are checked each tick).
            let objects: Vec<String> = self
                .sim
                .targets
                .iter()
                .filter(|(_, t)| t.fast)
                .map(|(name, _)| name.clone())
                .collect();
            for object in objects {
                self.sim_check(&object);
            }
        }
    }

    fn sim_recoveries(&mut self, tick: u64) {
        let expired: Vec<String> = self
            .sim
            .targets
            .iter()
            .filter(|(_, t)| t.until.is_some_and(|until| until <= tick))
            .map(|(name, _)| name.clone())
            .collect();
        for object in expired {
            if let Some(target) = self.sim.targets.get_mut(&object) {
                target.state = 0;
                target.result = None;
                target.until = None;
                target.flaps = 0;
            }
            self.sim.schedule(tick, object);
        }
        if let Some(outage) = &self.sim.outage
            && outage.until <= tick
        {
            let parent = outage.parent.clone();
            self.sim.outage = None;
            for child in self.all_children_of(&parent) {
                let delay = 2 + self.sim.rng.below(20);
                self.sim.schedule(tick + delay, child);
            }
        }
        if self.sim.storm_until.is_some_and(|until| until <= tick) {
            self.sim.storm_until = None;
        }
        self.sim.maintenance.retain(|_, until| *until > tick);
    }

    /// Non-pinned, untargeted services in `state`, whose host is up.
    fn sim_candidates(&self, problem: bool) -> Vec<String> {
        self.all_services()
            .filter(|s| s.enable_active_checks && s.has_been_checked())
            .filter(|s| (s.state_raw != 0) == problem)
            .filter(|s| self.hosts.get(&s.host_name).is_some_and(|h| h.state() == 0))
            .map(super::model::Checkable::full_name)
            .filter(|name| !self.pinned.contains(name) && !self.sim.targets.contains_key(name))
            .collect()
    }

    fn sim_churn(&mut self, tick: u64) {
        let expected = self.sim.per_tick(self.sim.config.problems_per_hour);
        let mut count = expected.floor();
        if self.sim.rng.chance(expected - count) {
            count += 1.0;
        }
        let duration = self
            .sim
            .ticks(self.sim.config.problem_duration.as_secs_f64());
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "count is a small non-negative whole number"
        )]
        let count = count.clamp(0.0, 10_000.0) as u32;
        for _ in 0..count {
            let candidates = self.sim_candidates(false);
            let Some(object) = self.sim.rng.pick(&candidates).cloned() else {
                break;
            };
            let roll = self.sim.rng.unit();
            let state = if roll < 0.5 {
                1
            } else if roll < 0.8 {
                2
            } else {
                3
            };
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "tick counts are small positive numbers"
            )]
            let length = ((duration as f64) * self.sim.rng.range(0.5, 1.5)).max(1.0) as u64;
            self.sim.targets.insert(
                object.clone(),
                Target {
                    state,
                    result: None,
                    until: Some(tick + length),
                    flaps: 0,
                    fast: false,
                },
            );
            self.sim.schedule(tick, object);
        }
        // Problems that were there before recover at a similar rate.
        if self.sim.rng.chance(expected / 2.0) {
            let candidates = self.sim_candidates(true);
            if let Some(object) = self.sim.rng.pick(&candidates).cloned() {
                self.sim.targets.insert(
                    object.clone(),
                    Target {
                        state: 0,
                        result: None,
                        until: None,
                        flaps: 0,
                        fast: false,
                    },
                );
                self.sim.schedule(tick, object);
            }
        }
    }

    fn sim_flapping(&mut self, tick: u64) {
        let chance = self.sim.per_tick(2.0);
        if !self.sim.rng.chance(chance) {
            return;
        }
        let candidates: Vec<String> = self
            .sim_candidates(false)
            .into_iter()
            .filter(|name| self.checkable(name).is_some_and(|c| c.enable_flapping))
            .collect();
        if let Some(object) = self.sim.rng.pick(&candidates).cloned() {
            self.sim.targets.insert(
                object.clone(),
                Target {
                    state: 1,
                    result: None,
                    until: None,
                    flaps: 24,
                    fast: true,
                },
            );
            self.sim.schedule(tick, object);
        }
    }

    fn sim_outage(&mut self, tick: u64) {
        if self.sim.outage.is_some() || !self.sim.rng.chance(self.sim.per_tick(0.5)) {
            return;
        }
        let parents: Vec<String> = self
            .hosts
            .values()
            .filter(|h| h.state() == 0 && h.enable_active_checks)
            .map(super::model::Checkable::full_name)
            .filter(|name| !self.pinned.contains(name) && !self.sim.targets.contains_key(name))
            .filter(|name| !self.children_of(name).is_empty())
            .collect();
        let Some(parent) = self.sim.rng.pick(&parents).cloned() else {
            return;
        };
        let length = self.sim.ticks(600.0);
        self.sim.targets.insert(
            parent.clone(),
            Target {
                state: 2,
                result: None,
                until: Some(tick + length),
                flaps: 0,
                fast: true,
            },
        );
        self.sim.schedule(tick, parent.clone());
        for child in self.all_children_of(&parent) {
            let delay = 3 + self.sim.rng.below(15);
            self.sim.schedule(tick + delay, child);
        }
        self.sim.outage = Some(Outage {
            parent,
            until: tick + length,
        });
    }

    fn sim_downtime(&mut self, tick: u64) {
        if !self.sim.rng.chance(self.sim.per_tick(1.0)) {
            return;
        }
        let hosts: Vec<String> = self
            .hosts
            .keys()
            .filter(|name| {
                !self.pinned.contains(*name) && !self.sim.maintenance.contains_key(*name)
            })
            .cloned()
            .collect();
        let Some(host) = self.sim.rng.pick(&hosts).cloned() else {
            return;
        };
        let minutes = 5.0 + self.sim.rng.range(0.0, 25.0);
        let ticks = self.sim.ticks(minutes * 60.0);
        self.sim.maintenance.insert(host.clone(), tick + ticks);
        let now = self.now();
        #[expect(clippy::cast_precision_loss, reason = "tick counts are small")]
        let length = ticks as f64 * self.sim.real_tick_seconds();
        self.add_downtime(
            &host,
            DowntimeSpec {
                author: "ic-mock".to_owned(),
                comment: "Simulated maintenance window".to_owned(),
                start_time: now,
                end_time: now + length,
                fixed: true,
                ..DowntimeSpec::default()
            },
        );
    }

    fn sim_storm(&mut self, tick: u64) {
        let Some(storm) = self.sim.config.storm.clone() else {
            return;
        };
        if storm.every_ticks == 0
            || !tick.is_multiple_of(storm.every_ticks)
            || self.sim.storm_until.is_some()
        {
            return;
        }
        let mut candidates = self.sim_candidates(false);
        for _ in 0..storm.size {
            if candidates.is_empty() {
                break;
            }
            let index = self.sim.rng.index(candidates.len());
            let object = candidates.swap_remove(index);
            self.sim.targets.insert(
                object.clone(),
                Target {
                    state: 2,
                    result: None,
                    until: Some(tick + storm.duration_ticks.max(1)),
                    flaps: 0,
                    fast: true,
                },
            );
            self.sim.schedule(tick, object);
        }
        self.sim.storm_until = Some(tick + storm.duration_ticks.max(1));
    }

    fn sim_checker(&mut self, tick: u64) {
        let later = self.sim.due.split_off(&(tick + 1));
        let due = std::mem::replace(&mut self.sim.due, later);
        let mut objects: Vec<String> = due.into_values().flatten().collect();
        if objects.len() > MAX_CHECKS_PER_TICK {
            let rest = objects.split_off(MAX_CHECKS_PER_TICK);
            for object in rest {
                self.sim.schedule(tick + 1, object);
            }
        }
        // An object may be due twice (incident + regular schedule).
        let mut seen = std::collections::HashSet::new();
        objects.retain(|object| seen.insert(object.clone()));
        for object in objects {
            if let Some(next) = self.sim_check(&object) {
                let at = tick + next;
                self.sim.due.entry(at).or_default().retain(|o| *o != object);
                self.sim.schedule(at, object);
            }
        }
    }

    /// Whether `zone` is another zone than the node's whose endpoints are
    /// all disconnected (the results of its satellites can't arrive).
    fn zone_cut_off(&self, zone: &str) -> bool {
        if zone.is_empty() || zone == self.app.zone_name {
            return false;
        }
        self.zones.get(zone).is_some_and(|data| {
            !data.endpoints.is_empty()
                && data.endpoints.iter().all(|name| {
                    self.endpoints
                        .get(name)
                        .is_some_and(|endpoint| !endpoint.connected)
                })
        })
    }

    /// Checks one object. Returns the number of ticks until its next check,
    /// or `None` if it isn't actively checked.
    #[expect(
        clippy::too_many_lines,
        reason = "the checker walks every simulated state transition in order"
    )]
    fn sim_check(&mut self, object: &str) -> Option<u64> {
        let checkable = self.checkable(object)?;
        if !checkable.enable_active_checks {
            return None;
        }
        let globally_enabled = if checkable.is_service() {
            self.app.enable_service_checks
        } else {
            self.app.enable_host_checks
        };
        let command = checkable.check_command.clone();
        let interval = checkable.check_interval;
        let retry = checkable.retry_interval;
        // A zone whose endpoints are all gone sends no results: its
        // checks run (or not) out of the node's sight and become late.
        if !globally_enabled || self.zone_cut_off(&checkable.meta.zone) {
            return Some(self.sim.ticks(interval));
        }
        let is_service = checkable.is_service();
        let address = self
            .hosts
            .get(&checkable.host_name)
            .map(|h| h.address.clone())
            .unwrap_or_default();
        let host_name = checkable.host_name.clone();
        let service_name = checkable.service_name.clone().unwrap_or_default();
        let current_state = checkable.cr.as_ref().map_or(0, |cr| cr.state);
        // Problems the simulator caused through a failed host or parent.
        let caused_by_outage = checkable.cr.as_ref().is_some_and(|cr| {
            cr.state != 0
                && (cr.output.starts_with("CRITICAL - Host Unreachable")
                    || cr.output.starts_with("Remote Icinga instance")
                    || cr.output.starts_with("connect to address"))
        });
        let pinned = self.pinned.contains(object);
        let mut target = self.sim.targets.get(object).cloned();
        let endpoint = if checkable.command_endpoint.is_empty() {
            self.app.node_name.clone()
        } else {
            checkable.command_endpoint.clone()
        };

        let (state, result) = if pinned {
            (current_state, None)
        } else if let Some(target) = &mut target {
            let state = if target.flaps > 0 {
                target.flaps -= 1;
                if target.flaps == 0 {
                    target.state = 0;
                    target.fast = false;
                    0
                } else {
                    u8::from(target.flaps % 2 == 0)
                }
            } else {
                target.state
            };
            (state, target.result.clone())
        } else if !is_service && !self.is_reachable(object, crate::model::DepType::State) {
            // The network path is down: the host check fails too.
            (
                2,
                Some(outputs::host_output(&address, false, &mut self.sim.rng)),
            )
        } else if is_service
            && self
                .hosts
                .get(&host_name)
                .is_some_and(|h| h.state() != 0 && h.state_type == 1)
        {
            let (state, result) =
                outputs::host_down_output(&command, &host_name, &address, &endpoint);
            (state, Some(result))
        } else if caused_by_outage {
            // The parent or host is back, and so is this object.
            (0, None)
        } else {
            (current_state, None)
        };
        if let Some(t) = target {
            self.sim.targets.insert(object.to_owned(), t);
        }
        let result = match result {
            Some(result) => Some(result),
            None if pinned => None,
            None if state == current_state && state != 0 => None,
            None if is_service => Some(outputs::service_output(
                &service_name,
                &command,
                state,
                &mut self.sim.rng,
            )),
            None => Some(outputs::host_output(
                &address,
                state == 0,
                &mut self.sim.rng,
            )),
        };
        let checkable = self.checkable(object)?;
        let input: CheckInput = self.recheck_input(
            checkable,
            result.map(|r| (state, r.output, Some(r.perfdata))),
        );
        let input = CheckInput {
            state,
            exit_status: i64::from(state),
            ..input
        };
        self.process_check_result(object, input);

        let checkable = self.checkable(object)?;
        let soft_problem = checkable.state_type == 0 && checkable.problem();
        let recovered = checkable.state_raw == 0 && checkable.state_type == 1;
        let fast = self.sim.targets.get(object).is_some_and(|t| t.fast);
        if recovered
            && self
                .sim
                .targets
                .get(object)
                .is_some_and(|t| t.state == 0 && t.flaps == 0)
        {
            self.sim.targets.remove(object);
        }
        let next = if fast {
            1
        } else if soft_problem {
            self.sim.ticks(retry)
        } else {
            self.sim.ticks(interval)
        };
        #[expect(clippy::cast_precision_loss, reason = "tick counts are small")]
        let next_real = next as f64 * self.sim.real_tick_seconds();
        let now = self.now();
        if let Some(checkable) = self.checkable_mut(object) {
            checkable.next_check = now + next_real;
        }
        Some(next)
    }
}
