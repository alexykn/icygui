//! Snapshots of the runtime state as `ic_model` types, for assertions.

use ic_model::{
    AckKind, CheckInfo, CheckResult, Comment, CommentKind, Dependency, Downtime, Endpoint,
    Features, Host, HostGroup, HostName, HostState, InstanceStatus, Links, Notification, ObjectKey,
    Service, ServiceGroup, ServiceKey, ServiceState, StateType, Timestamp, parse_perfdata_entry,
};

use super::World;
use super::types::{Checkable, CommentData, DowntimeData};

fn ts(seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

fn non_zero(seconds: f64) -> Option<Timestamp> {
    (seconds > 0.0).then(|| ts(seconds))
}

impl World {
    fn check_info(&self, c: &Checkable) -> CheckInfo {
        let result = c.cr.as_ref().map(|cr| {
            let (output, long_output) = CheckResult::split_output(&cr.output);
            CheckResult {
                output,
                long_output,
                perfdata: cr
                    .performance_data
                    .iter()
                    .flatten()
                    .filter_map(|entry| parse_perfdata_entry(entry))
                    .collect(),
                exit_status: i32::try_from(cr.exit_status).unwrap_or(3),
                schedule_start: ts(cr.schedule_start),
                execution_start: ts(cr.execution_start),
                execution_end: ts(cr.execution_end),
                check_source: cr.check_source.clone(),
                active: cr.active,
            }
        });
        CheckInfo {
            state_type: if c.state_type == 1 {
                StateType::Hard
            } else {
                StateType::Soft
            },
            last_state_change: ts(c.last_state_change),
            last_hard_state_change: ts(c.last_hard_state_change),
            last_check: c.cr.as_ref().map(|cr| ts(cr.schedule_end)),
            next_check: non_zero(c.next_check),
            attempt: c.check_attempt,
            max_attempts: c.max_check_attempts,
            result,
            acknowledgement: if self.is_acknowledged(c) {
                AckKind::from_code(c.acknowledgement)
            } else {
                AckKind::None
            },
            acknowledgement_expiry: non_zero(c.acknowledgement_expiry),
            downtime_depth: self.downtime_depth(&c.full_name()),
            flapping: self.is_flapping(c),
            flapping_current: c.flapping_current,
            reachable: c.last_reachable,
            check_command: c.check_command.clone(),
            check_interval: c.check_interval,
            retry_interval: c.retry_interval,
            command_endpoint: (!c.command_endpoint.is_empty()).then(|| c.command_endpoint.clone()),
            zone: (!c.meta.zone.is_empty()).then(|| c.meta.zone.clone()),
            features: Features {
                active_checks: c.enable_active_checks,
                passive_checks: c.enable_passive_checks,
                notifications: c.enable_notifications,
                event_handler: c.enable_event_handler,
                flap_detection: c.enable_flapping,
                perfdata: c.enable_perfdata,
            },
        }
    }

    fn links(c: &Checkable) -> Links {
        Links {
            notes: c.notes.clone(),
            notes_url: c.notes_url.clone(),
            action_url: c.action_url.clone(),
            icon_image: c.icon_image.clone(),
        }
    }

    /// A host as `ic_model` sees it (pending without a check result,
    /// unreachable when down with `last_reachable = false`).
    pub(crate) fn host_snapshot(&self, c: &Checkable) -> Host {
        let state = if !c.has_been_checked() {
            HostState::Pending
        } else if c.state() == 0 {
            HostState::Up
        } else if c.last_reachable {
            HostState::Down
        } else {
            HostState::Unreachable
        };
        Host {
            name: HostName::new(&c.host_name),
            display_name: if c.display_name.is_empty() {
                c.host_name.clone()
            } else {
                c.display_name.clone()
            },
            address: c.address.clone(),
            address6: c.address6.clone(),
            state,
            check: self.check_info(c),
            groups: c.groups.clone(),
            vars: c.vars.clone().unwrap_or_default(),
            links: Self::links(c),
        }
    }

    /// A service as `ic_model` sees it.
    pub(crate) fn service_snapshot(&self, c: &Checkable) -> Service {
        let state = if c.has_been_checked() {
            ServiceState::from_code(c.state_raw).unwrap_or(ServiceState::Unknown)
        } else {
            ServiceState::Pending
        };
        let name = c.service_name.clone().unwrap_or_default();
        Service {
            key: ServiceKey::new(&c.host_name, &name),
            display_name: if c.display_name.is_empty() {
                name
            } else {
                c.display_name.clone()
            },
            state,
            check: self.check_info(c),
            groups: c.groups.clone(),
            vars: c.vars.clone().unwrap_or_default(),
            links: Self::links(c),
        }
    }

    pub(crate) fn comment_snapshot(comment: &CommentData) -> Comment {
        Comment {
            name: comment.name.clone(),
            object: comment.object(),
            author: comment.author.clone(),
            text: comment.text.clone(),
            kind: CommentKind::from_code(comment.entry_type),
            entry_time: ts(comment.entry_time),
            expire_time: non_zero(comment.expire_time),
            persistent: comment.persistent,
        }
    }

    pub(crate) fn downtime_snapshot(&self, downtime: &DowntimeData) -> Downtime {
        Downtime {
            name: downtime.name.clone(),
            object: downtime.object(),
            author: downtime.author.clone(),
            comment: downtime.comment.clone(),
            start_time: ts(downtime.start_time),
            end_time: ts(downtime.end_time),
            fixed: downtime.fixed,
            duration: downtime.duration,
            entry_time: ts(downtime.entry_time),
            trigger_time: non_zero(downtime.trigger_time),
            triggered_by: (!downtime.triggered_by.is_empty())
                .then(|| downtime.triggered_by.clone()),
            parent: (!downtime.parent.is_empty()).then(|| downtime.parent.clone()),
            in_effect: downtime.is_in_effect(self.now()),
            config_owned: !downtime.config_owner.is_empty() || !downtime.scheduled_by.is_empty(),
        }
    }

    pub(crate) fn hosts_snapshot(&self) -> Vec<Host> {
        self.hosts.values().map(|c| self.host_snapshot(c)).collect()
    }

    pub(crate) fn services_snapshot(&self) -> Vec<Service> {
        self.all_services()
            .map(|c| self.service_snapshot(c))
            .collect()
    }

    pub(crate) fn comments_snapshot(&self) -> Vec<Comment> {
        self.comments.values().map(Self::comment_snapshot).collect()
    }

    pub(crate) fn downtimes_snapshot(&self) -> Vec<Downtime> {
        self.downtimes
            .values()
            .map(|d| self.downtime_snapshot(d))
            .collect()
    }

    pub(crate) fn notifications_snapshot(&self) -> Vec<Notification> {
        self.notifications
            .values()
            .map(|n| Notification {
                name: n.name.clone(),
                object: if n.service_name.is_empty() {
                    ObjectKey::host(&n.host_name)
                } else {
                    ObjectKey::service(&n.host_name, &n.service_name)
                },
                last_notification: non_zero(n.last_notification),
                notified_problem_users: n.notified_problem_users.clone(),
            })
            .collect()
    }

    pub(crate) fn host_groups_snapshot(&self) -> Vec<HostGroup> {
        self.host_groups
            .values()
            .map(|g| HostGroup {
                name: g.name.clone(),
                display_name: g.display_name.clone(),
            })
            .collect()
    }

    pub(crate) fn service_groups_snapshot(&self) -> Vec<ServiceGroup> {
        self.service_groups
            .values()
            .map(|g| ServiceGroup {
                name: g.name.clone(),
                display_name: g.display_name.clone(),
            })
            .collect()
    }

    pub(crate) fn dependencies_snapshot(&self) -> Vec<Dependency> {
        self.dependencies
            .values()
            .map(|d| Dependency {
                name: d.name.clone(),
                child: d.child.clone(),
                parent: d.parent.clone(),
            })
            .collect()
    }

    pub(crate) fn endpoints_snapshot(&self) -> Vec<Endpoint> {
        self.endpoints
            .values()
            .map(|e| Endpoint {
                name: e.name.clone(),
                zone: e.member_of.clone(),
                connected: e.connected,
            })
            .collect()
    }

    pub(crate) fn status_snapshot(&self) -> InstanceStatus {
        let now = self.now();
        let mut latency = 0.0;
        let mut execution = 0.0;
        let mut count = 0.0;
        for cr in self.all_checkables().filter_map(|c| c.cr.as_ref()) {
            let exec = cr.execution_end - cr.execution_start;
            execution += exec;
            latency += ((cr.schedule_end - cr.schedule_start) - exec).max(0.0);
            count += 1.0;
        }
        let average = |sum: f64| if count > 0.0 { sum / count } else { 0.0 };
        InstanceStatus {
            node_name: self.app.node_name.clone(),
            version: self.app.version.clone(),
            program_start: ts(self.app.program_start),
            notifications_enabled: self.app.enable_notifications,
            host_checks_enabled: self.app.enable_host_checks,
            service_checks_enabled: self.app.enable_service_checks,
            event_handlers_enabled: self.app.enable_event_handlers,
            flap_detection_enabled: self.app.enable_flapping,
            perfdata_enabled: self.app.enable_perfdata,
            checks_per_minute: f64::from(self.stats.checks_last_minute(now)),
            avg_latency: average(latency),
            avg_execution_time: average(execution),
            counts: self.object_counts(),
        }
    }
}
