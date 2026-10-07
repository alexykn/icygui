//! Saving the settings and the UI state off the UI thread.
//!
//! A save syncs files to disk, which can take a while on a slow or busy
//! disk; the UI thread only hands the latest version to a writer thread.
//! Saves that queue up while one runs collapse into the newest, so moving
//! the window or clicking through sort keys costs one write, not one per
//! step. Every result goes back to the app ([`SaveReport`]), which shows a
//! banner when the settings can't be saved (a read-only file managed by
//! Ansible, a full disk).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ic_config::{Config, ConfigStore, StateStore, UiState};

/// What a save did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveReport {
    /// The settings file: saved, or why not.
    Config(Result<(), String>),
    /// The UI state file: saved, or why not.
    Ui(Result<(), String>),
}

/// Receives every [`SaveReport`], on the writer thread.
pub(crate) type Reporter = Box<dyn Fn(SaveReport) + Send + 'static>;

enum Job {
    Config(Box<Config>),
    Ui(Box<UiState>),
    Flush(Sender<()>),
}

/// The handle on the writer thread. Dropping it lets the thread finish
/// the saves already queued and end.
#[derive(Debug)]
pub(crate) struct Persistence {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    /// The settings as the file holds them: as last read, or as the
    /// writer last saved them. A file that reads differently was changed
    /// by someone else (*edit in settings file*).
    on_disk: Arc<Mutex<Option<Config>>>,
}

impl Persistence {
    /// Starts the writer thread for `config` (the settings file) and `ui`
    /// (the UI state file); `report` hears about every save.
    ///
    /// # Errors
    ///
    /// The thread couldn't be started.
    pub(crate) fn start(
        config: ConfigStore,
        ui: StateStore,
        report: Reporter,
    ) -> std::io::Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        let on_disk = Arc::new(Mutex::new(None));
        let written = on_disk.clone();
        let thread = std::thread::Builder::new()
            .name("icygui-persist".to_owned())
            .spawn(move || run(&config, &ui, &receiver, &report, &written))?;
        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
            on_disk,
        })
    }

    /// The settings as the file holds them, as far as icygui knows: as
    /// last read ([`Persistence::read_from_disk`]) or saved.
    pub(crate) fn on_disk(&self) -> Option<Config> {
        self.on_disk
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The settings file was read and holds `config`.
    pub(crate) fn read_from_disk(&self, config: Config) {
        *self.on_disk.lock().unwrap_or_else(PoisonError::into_inner) = Some(config);
    }

    /// Saves the settings (soon; a newer save replaces a waiting one).
    pub(crate) fn save_config(&self, config: Config) {
        self.send(Job::Config(Box::new(config)));
    }

    /// Saves the UI state (soon; a newer save replaces a waiting one).
    pub(crate) fn save_ui(&self, ui: UiState) {
        self.send(Job::Ui(Box::new(ui)));
    }

    /// Waits until everything queued so far is written, at most `timeout`.
    /// Returns whether it was.
    pub(crate) fn flush(&self, timeout: Duration) -> bool {
        let (done, wait) = mpsc::channel();
        self.send(Job::Flush(done));
        wait.recv_timeout(timeout).is_ok()
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_err()
        {
            tracing::error!("the settings writer has stopped; a save was lost");
        }
    }
}

impl Drop for Persistence {
    fn drop(&mut self) {
        // Closing the channel ends the thread once the queue is written.
        self.jobs = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the settings writer panicked");
        }
    }
}

fn run(
    config_store: &ConfigStore,
    ui_store: &StateStore,
    jobs: &Receiver<Job>,
    report: &Reporter,
    on_disk: &Mutex<Option<Config>>,
) {
    while let Ok(first) = jobs.recv() {
        let mut config = None;
        let mut ui = None;
        let mut flushes = Vec::new();
        let mut take = |job: Job| match job {
            Job::Config(next) => config = Some(next),
            Job::Ui(next) => ui = Some(next),
            Job::Flush(done) => flushes.push(done),
        };
        take(first);
        while let Ok(job) = jobs.try_recv() {
            take(job);
        }
        if let Some(config) = config {
            let result = config_store
                .save(&config)
                .map_err(|error| error.to_string());
            match &result {
                Ok(()) => {
                    *on_disk.lock().unwrap_or_else(PoisonError::into_inner) = Some(*config);
                }
                Err(error) => {
                    tracing::warn!(%error, path = %config_store.path().display(), "the settings could not be saved");
                }
            }
            report(SaveReport::Config(result));
        }
        if let Some(ui) = ui {
            let result = ui_store.save(&ui).map_err(|error| error.to_string());
            if let Err(error) = &result {
                tracing::warn!(%error, path = %ui_store.path().display(), "the window and tab state could not be saved");
            }
            report(SaveReport::Ui(result));
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ic_config::{AuthConfig, Environment, WindowState};

    use super::*;

    fn stores(dir: &std::path::Path) -> (ConfigStore, StateStore) {
        (
            ConfigStore::new(dir.join("config.toml")),
            StateStore::new(dir.join("state.toml")),
        )
    }

    fn recorder() -> (Arc<Mutex<Vec<SaveReport>>>, Reporter) {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink = reports.clone();
        (
            reports,
            Box::new(move |report| sink.lock().unwrap().push(report)),
        )
    }

    fn config(name: &str) -> Config {
        Config {
            environments: vec![Environment::new(
                name,
                "https://master-01:5665",
                AuthConfig::Basic {
                    username: "icygui".to_owned(),
                },
            )],
            ..Config::default()
        }
    }

    #[test]
    fn saves_the_latest_config_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let (config_store, ui_store) = stores(dir.path());
        let (reports, report) = recorder();
        let persistence =
            Persistence::start(config_store.clone(), ui_store.clone(), report).unwrap();
        persistence.save_config(config("first"));
        persistence.save_config(config("second"));
        let ui = UiState {
            window: Some(WindowState {
                x: 10.,
                y: 20.,
                width: 1200.,
                height: 800.,
                maximized: false,
            }),
            ..UiState::default()
        };
        persistence.save_ui(ui.clone());
        assert!(persistence.flush(Duration::from_secs(10)));
        assert_eq!(config_store.load().unwrap().environments[0].name, "second");
        assert_eq!(
            persistence.on_disk().unwrap().environments[0].name,
            "second",
            "what was written is what the file holds"
        );
        assert_eq!(ui_store.load().unwrap(), ui);
        let reports = reports.lock().unwrap().clone();
        assert!(reports.contains(&SaveReport::Config(Ok(()))));
        assert!(reports.contains(&SaveReport::Ui(Ok(()))));
        assert!(
            reports.len() <= 3,
            "saves queued together collapse: {reports:?}"
        );
    }

    #[test]
    fn failures_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the settings directory should be.
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "").unwrap();
        let config_store = ConfigStore::new(blocker.join("config.toml"));
        let ui_store = StateStore::new(blocker.join("state.toml"));
        let (reports, report) = recorder();
        let persistence = Persistence::start(config_store, ui_store, report).unwrap();
        persistence.save_config(config("x"));
        persistence.save_ui(UiState::default());
        assert!(persistence.flush(Duration::from_secs(10)));
        let reports = reports.lock().unwrap().clone();
        assert!(
            matches!(
                &reports[..],
                [SaveReport::Config(Err(_)), SaveReport::Ui(Err(_))]
            ),
            "{reports:?}"
        );
    }

    #[test]
    fn dropping_writes_what_is_queued() {
        let dir = tempfile::tempdir().unwrap();
        let (config_store, ui_store) = stores(dir.path());
        let (_reports, report) = recorder();
        let persistence = Persistence::start(config_store.clone(), ui_store, report).unwrap();
        persistence.save_config(config("kept"));
        drop(persistence);
        assert_eq!(config_store.load().unwrap().environments[0].name, "kept");
    }
}
