//! An environment's trouble alerts settings (topic 16; the settings'
//! environment page): how they notify, and which heartbeats prove that
//! Icinga runs its checks. Changes save at once and reach the
//! environment's engine, which finds the heartbeats again.

use ic_config::Trouble;
use ic_model::ServiceKey;

use super::AppState;

impl AppState {
    /// Changes environment `id`'s trouble alerts settings. Returns whether
    /// they changed (an unknown environment changes nothing).
    pub(crate) fn set_trouble(&mut self, id: &str, change: impl FnOnce(&mut Trouble)) -> bool {
        let Some(environment) = self.config.environment(id) else {
            return false;
        };
        let mut trouble = environment.trouble.clone();
        change(&mut trouble);
        if trouble == environment.trouble {
            return false;
        }
        tracing::info!(
            environment = %environment.name,
            policy = trouble.policy.label(),
            heartbeats = ?trouble.heartbeats.mode,
            "trouble alerts changed"
        );
        self.change_environment_of(id, |environment| {
            environment.trouble = trouble;
            Some(())
        })
        .is_some()
    }

    /// *confirm removal* of a heartbeat that disappeared: environment
    /// `id`'s engine forgets it and clears its finding.
    pub(crate) fn confirm_heartbeat_removal(&self, id: &str, key: ServiceKey) {
        if let Some(slot) = self.slot(id) {
            tracing::info!(heartbeat = %format!("{}!{}", key.host, key.name), "heartbeat removal confirmed");
            slot.send(ic_core::Command::ConfirmHeartbeatRemoval(key));
        }
    }
}

#[cfg(test)]
mod tests {
    use ic_config::{HeartbeatMode, TroublePolicy};
    use ic_model::Timestamp;

    use super::*;
    use crate::app_state::testing::Recorder;

    #[test]
    fn trouble_settings_save_and_reach_the_engine() {
        let mut state = AppState::fixture(Timestamp::from_unix_seconds(1_790_000_000.));
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
        let id = state.active_environment_id().unwrap().to_owned();
        assert!(!state.set_trouble(&id, |_| {}), "unchanged");
        assert!(!state.set_trouble("nowhere", |trouble| {
            trouble.policy = TroublePolicy::Persistent;
        }));
        assert!(state.set_trouble(&id, |trouble| {
            trouble.policy = TroublePolicy::Persistent;
            trouble.heartbeats.mode = HeartbeatMode::List;
            trouble.heartbeats.list = vec!["icygui-hb-ams!beat".to_owned()];
        }));
        let trouble = &state.environment().unwrap().trouble;
        assert_eq!(trouble.policy, TroublePolicy::Persistent);
        assert_eq!(trouble.heartbeats.list, ["icygui-hb-ams!beat"]);
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
        state.confirm_heartbeat_removal(&id, ServiceKey::new("icygui-hb-fra", "beat"));
        assert_eq!(recorder.sent().len(), 2);
        assert!(recorder.sent()[1].starts_with("ConfirmHeartbeatRemoval"), "{:?}", recorder.sent());
    }
}
