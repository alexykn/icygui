//! One engine per environment (PLAN.md D2): every saved environment's
//! engine runs, in the background too, and keeps its event stream, rules,
//! event log and notifications; the active one drives the window.
//!
//! What the state keeps of each engine is an [`EngineSlot`]: the link to
//! it, the latest snapshot, connection, permissions and notifications it
//! reported. The active environment's slot is `AppState::engine`, every
//! other environment's is parked by id; switching swaps them, so the new
//! environment shows at once what its engine already has. Events arrive
//! tagged with their environment ([`AppState::apply_from`]); an inactive
//! engine is told so (`Command::SetActive(false)`) and publishes less
//! often.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use ic_config::Environment;
use ic_core::snapshot::Snapshot;
use ic_core::{ApiInfo, Command, ConnectionState, CoreEvent, NotificationRecord};

use super::hydration::Hydration;
use super::{AppState, ConnectionStatus, CoreLink, MAX_NOTIFICATIONS, endpoint_of, user_of};

/// What the app knows about one environment's engine.
#[derive(Debug)]
pub(crate) struct EngineSlot {
    /// The running engine (`None` before it started, while it is being
    /// replaced, or after it stopped).
    pub(super) core: Option<Box<dyn CoreLink>>,
    /// Its latest snapshot.
    pub(super) snapshot: Arc<Snapshot>,
    /// Its connection as the footer and the banners show it.
    pub(super) connection: ConnectionStatus,
    /// The API user and its permissions, once connected.
    pub(super) permissions: Option<ApiInfo>,
    /// Its notifications for the notification centre, newest first: the
    /// log's recent ones, then every new one.
    pub(super) notifications: VecDeque<NotificationRecord>,
    /// The environment changed in place while the engine waited to
    /// reconnect; the update goes out once it connects.
    pub(super) update_pending: bool,
    /// What the window asked to hydrate recently.
    pub(super) hydration: Hydration,
    /// When the user last asked for a reload.
    pub(super) last_refresh: Option<Instant>,
}

impl EngineSlot {
    /// No environment: nothing runs.
    pub(super) fn idle() -> Self {
        Self::with_connection(ConnectionStatus::idle())
    }

    /// `environment`'s engine is starting: nothing known yet.
    pub(super) fn starting(environment: &Environment) -> Self {
        Self::with_connection(ConnectionStatus::starting(
            &endpoint_of(environment),
            user_of(environment),
        ))
    }

    fn with_connection(connection: ConnectionStatus) -> Self {
        Self {
            core: None,
            snapshot: Arc::default(),
            connection,
            permissions: None,
            notifications: VecDeque::new(),
            update_pending: false,
            hydration: Hydration::default(),
            last_refresh: None,
        }
    }

    /// Sends `command` to the engine, if it runs.
    pub(super) fn send(&self, command: Command) {
        if let Some(core) = &self.core {
            core.send(command);
        } else {
            tracing::debug!(?command, "no core running; command dropped");
        }
    }

    /// The latest snapshot.
    pub(crate) fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    /// The connection.
    pub(crate) fn connection(&self) -> &ConnectionStatus {
        &self.connection
    }

    /// The API user and its permissions, once connected.
    pub(crate) fn permissions(&self) -> Option<&ApiInfo> {
        self.permissions.as_ref()
    }

    /// Its notifications, newest first.
    pub(crate) fn notifications(&self) -> impl Iterator<Item = &NotificationRecord> {
        self.notifications.iter()
    }

    /// Its notifications not seen yet, silent ones included.
    pub(crate) fn unread(&self) -> usize {
        self.notifications
            .iter()
            .filter(|record| !record.read)
            .count()
    }

    /// Adds a new notification (unless it is known).
    pub(super) fn push_notification(&mut self, record: NotificationRecord) {
        if !self
            .notifications
            .iter()
            .any(|known| known.intent.id == record.intent.id)
        {
            self.notifications.push_front(record);
            self.notifications.truncate(MAX_NOTIFICATIONS);
        }
    }

    /// Marks notification `id` read (here and in the engine's log).
    /// Returns whether it was unread.
    pub(super) fn mark_read(&mut self, id: &str) -> bool {
        let Some(record) = self
            .notifications
            .iter_mut()
            .find(|record| record.intent.id == id && !record.read)
        else {
            return false;
        };
        record.read = true;
        self.send(Command::MarkNotificationRead(id.to_owned()));
        true
    }

    /// Takes the environment's settings: at once, or (while the engine
    /// waits to reconnect, which an update would cut short) once it
    /// connects by itself.
    pub(super) fn update_environment(&mut self, environment: &Environment) {
        if self.connection.is_waiting() {
            self.update_pending = true;
        } else {
            self.update_pending = false;
            self.send(Command::UpdateEnvironment(environment.clone()));
        }
    }
}

impl AppState {
    /// The slot of environment `id`: the active one's, or a parked one
    /// (`None` for an unknown environment, or one whose engine never
    /// reported anything).
    pub(crate) fn slot(&self, id: &str) -> Option<&EngineSlot> {
        if self.active_environment_id() == Some(id) {
            Some(&self.engine)
        } else {
            self.parked.get(id)
        }
    }

    /// The slot of environment `id`, created for a known environment.
    pub(super) fn slot_mut(&mut self, id: &str) -> Option<&mut EngineSlot> {
        let environment = self.config.environment(id)?;
        if self.config.active_environment.as_deref() == Some(id) {
            return Some(&mut self.engine);
        }
        Some(
            self.parked
                .entry(id.to_owned())
                .or_insert_with(|| EngineSlot::starting(environment)),
        )
    }

    /// Whether `id` names the active environment.
    pub(crate) fn is_active(&self, id: &str) -> bool {
        self.active_environment_id() == Some(id)
    }

    /// Connects environment `id`'s slot to its running engine: an inactive
    /// one is told it isn't on screen, and every engine gets the pause in
    /// force for it. A link for an environment removed meanwhile is
    /// dropped (which stops that engine).
    pub(crate) fn set_core_for(&mut self, id: &str, core: Box<dyn CoreLink>) {
        let active = self.is_active(id);
        let pause = self.effective_pause(id, ic_model::Timestamp::now());
        let Some(slot) = self.slot_mut(id) else {
            tracing::info!(environment = %id, "an engine for a removed environment was stopped");
            return;
        };
        slot.core = Some(core);
        if !active {
            slot.send(Command::SetActive(false));
        }
        if let Some(until) = pause {
            slot.send(Command::PauseNotifications(Some(until)));
        }
    }

    /// Disconnects environment `id`'s slot (to stop its engine).
    pub(crate) fn take_core_of(&mut self, id: &str) -> Option<Box<dyn CoreLink>> {
        if self.is_active(id) {
            self.engine.core.take()
        } else {
            self.parked.get_mut(id).and_then(|slot| slot.core.take())
        }
    }

    /// Disconnects every engine (at quit).
    pub(crate) fn take_all_cores(&mut self) -> Vec<Box<dyn CoreLink>> {
        std::iter::once(&mut self.engine)
            .chain(self.parked.values_mut())
            .filter_map(|slot| slot.core.take())
            .collect()
    }

    /// Environment `id`'s engine couldn't start.
    pub(crate) fn engine_failed_for(&mut self, id: &str, error: String) {
        if let Some(slot) = self.slot_mut(id) {
            slot.connection.on_engine_error(error);
        }
    }

    /// Environment `id`'s engine stopped on its own (its events ended):
    /// its link goes.
    pub(crate) fn engine_stopped_for(&mut self, id: &str, error: String) {
        if let Some(slot) = self.slot_mut(id) {
            drop(slot.core.take());
            slot.connection.on_engine_stopped(error);
        }
    }

    /// Starts environment `id`'s slot over (its engine restarts: another
    /// URL, login or certificate): nothing known yet; the link to the
    /// engine stays (the session replaces it). For the active environment
    /// also what the window had asked for it
    /// ([`AppState::reset_connection`]).
    pub(crate) fn reset_environment(&mut self, id: &str) {
        if self.is_active(id) {
            self.reset_connection();
        } else if let Some(environment) = self.config.environment(id) {
            let mut slot = EngineSlot::starting(environment);
            slot.core = self.parked.remove(id).and_then(|old| old.core);
            self.parked.insert(id.to_owned(), slot);
        }
    }

    /// Takes one event from environment `id`'s engine: the active one's
    /// drive the window ([`AppState::apply`]); another's update its parked
    /// slot (the tray, the switcher and the notification counts read
    /// them). Events of an environment removed meanwhile are dropped.
    pub(crate) fn apply_from(&mut self, id: &str, event: CoreEvent) {
        if self.is_active(id) {
            self.apply(event);
            return;
        }
        let Some(name) = self
            .config
            .environment(id)
            .map(|environment| environment.name.clone())
        else {
            tracing::debug!(environment = %id, "an event of a removed environment was dropped");
            return;
        };
        match event {
            CoreEvent::ActionFinished {
                id: action,
                outcome,
            } => {
                // An action sent from another environment's notification.
                self.action_finished(action, &outcome);
            }
            CoreEvent::NotificationsPaused(_) => self.expire_pauses(ic_model::Timestamp::now()),
            event => {
                let Some(slot) = self.slot_mut(id) else {
                    return;
                };
                let mut update = false;
                match event {
                    CoreEvent::Snapshot(snapshot) => {
                        slot.connection.on_snapshot(&snapshot);
                        slot.snapshot = snapshot;
                    }
                    CoreEvent::Connection(state) => {
                        if let ConnectionState::Connected { node, .. } = &state {
                            tracing::info!(environment = %name, node = %node.name, view = %node.view.label(), "connected in the background");
                        }
                        slot.connection.on_state(state);
                        update = slot.update_pending && !slot.connection.is_waiting();
                    }
                    CoreEvent::Permissions(info) => slot.permissions = Some(info),
                    CoreEvent::Notification(record) => slot.push_notification(record),
                    CoreEvent::ActionFinished { .. } | CoreEvent::NotificationsPaused(_) => {}
                }
                if update {
                    self.send_environment_to(id);
                }
            }
        }
    }

    /// Sends environment `id`'s settings to its engine (unless it waits to
    /// reconnect: then once it connects).
    pub(super) fn send_environment_to(&mut self, id: &str) {
        let Some(environment) = self.config.environment(id).cloned() else {
            return;
        };
        if let Some(slot) = self.slot_mut(id) {
            slot.update_environment(&environment);
        }
    }

    /// Makes `id` the active environment: the current one's slot is
    /// parked (unless it was removed), `id`'s comes back, or starts empty
    /// when its engine hasn't reported yet.
    pub(super) fn activate(&mut self, id: Option<String>) {
        let incoming = match id.as_deref() {
            Some(new) => self.parked.remove(new).unwrap_or_else(|| {
                self.config
                    .environment(new)
                    .map_or_else(EngineSlot::idle, EngineSlot::starting)
            }),
            None => EngineSlot::idle(),
        };
        let outgoing = std::mem::replace(&mut self.engine, incoming);
        if let Some(previous) = self.config.active_environment.take()
            && previous.as_str() != id.as_deref().unwrap_or_default()
            && self.config.environment(&previous).is_some()
        {
            self.parked.insert(previous, outgoing);
        } else if outgoing.core.is_some() {
            tracing::warn!("an engine link was dropped with its environment");
        }
        self.config.active_environment = id;
    }

    /// Sends a command to every environment's engine (`command` makes each
    /// one's copy): app-wide settings.
    pub(super) fn send_to_every_engine(&self, command: impl Fn() -> Command) {
        self.engine.send(command());
        for slot in self.parked.values() {
            slot.send(command());
        }
    }

    /// Tells every engine whether its environment is the one on screen.
    pub(super) fn announce_active(&self) {
        self.engine.send(Command::SetActive(true));
        for slot in self.parked.values() {
            slot.send(Command::SetActive(false));
        }
    }

    /// Environment `id`'s latest snapshot.
    pub(crate) fn snapshot_of(&self, id: &str) -> Option<&Arc<Snapshot>> {
        self.slot(id).map(EngineSlot::snapshot)
    }

    /// Why the API user of environment `id` may not run `action`, if it
    /// may not.
    pub(crate) fn action_denial_in(
        &self,
        id: &str,
        action: &crate::actions::ObjectAction,
    ) -> Option<String> {
        super::permissions::action_denial(self.slot(id).and_then(EngineSlot::permissions), action)
    }
}
