//! `cargo xtask demo-config`: writes the demo cluster's environment
//! (`demo/icygui/environment.toml`) and dashboards
//! (`demo/icygui/dashboards.toml`) into icygui's settings, until icygui can
//! import environments itself (topic 08). Everything goes through
//! `ic-config`, the code icygui reads its settings with: the environment
//! file loads like a settings file, the dashboards import like a shared
//! export, and the result is checked and saved like the app saves.
//!
//! ```text
//! cargo xtask demo-config [--config-dir DIR] [--data-dir DIR] [--ca FILE]
//!                         [--via-proxy] [--select GROUP/DASHBOARD]
//!                         [--theme dark|light|system]
//! ```
//!
//! - `--config-dir`: where `config.toml` is (default: icygui's own, from
//!   `HOME` and the XDG variables, as the app finds it). An existing
//!   settings file keeps its other environments; the demo environment is
//!   replaced and made the active one.
//! - `--data-dir`: icygui's data directory, where `--select` writes the UI
//!   state (default: icygui's own).
//! - `--ca`: the cluster's CA certificate; by default it is read from the
//!   running cluster (`docker compose exec master-01`). It is copied next
//!   to `config.toml` as `demo-ca.crt`, which the environment trusts.
//! - No password: icygui asks for it like for any environment and keeps it
//!   in the OS secret store. The screenshot harness stores the demo value
//!   in its own throwaway keyring (`demo/screenshot.sh`,
//!   `ic-platform`'s `store_secret` example).
//! - `--via-proxy`: master-01 through the cluster's proxy
//!   (`https://127.0.0.1:5667`), for the dead-network scenario.
//! - The ports: `ICYGUI_DEMO_PORT_1`, `ICYGUI_DEMO_PORT_2` and
//!   `ICYGUI_DEMO_PORT_PROXY` move master-01, master-02 and the proxy, as
//!   they do in `demo/docker-compose.yml` and `demo/up.sh`, so the
//!   environment points where the cluster listens.
//! - `--select`: the dashboard shown at start, by names (`overview/databases`;
//!   written to the UI state file next to the data).
//! - `--theme`: the appearance's theme.
//!
//! Groups, dashboards and views get ids derived from their names
//! (`demo-overview-databases-view-0`), so a run selects the same ones.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ic_config::{Config, ConfigStore, Paths, ThemeChoice};
use ic_rules::DashboardRef;

use crate::Result;

/// What `demo-config` was asked to do.
#[derive(Debug, Default)]
struct Options {
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    ca: Option<PathBuf>,
    via_proxy: bool,
    select: Option<String>,
    theme: Option<ThemeChoice>,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--config-dir" => options.config_dir = Some(PathBuf::from(value()?)),
            "--data-dir" => options.data_dir = Some(PathBuf::from(value()?)),
            "--ca" => options.ca = Some(PathBuf::from(value()?)),
            "--via-proxy" => options.via_proxy = true,
            "--select" => options.select = Some(value()?),
            "--theme" => {
                options.theme = Some(match value()?.as_str() {
                    "dark" => ThemeChoice::Dark,
                    "light" => ThemeChoice::Light,
                    "system" => ThemeChoice::System,
                    other => return Err(format!("--theme {other}: dark, light or system")),
                });
            }
            other => return Err(format!("demo-config: unknown argument {other}")),
        }
    }
    Ok(options)
}

/// `cargo xtask demo-config`.
pub(crate) fn demo_config(root: &Path, args: &[String]) -> Result<()> {
    let options = parse(args)?;
    let demo = root.join("demo");
    let system = Paths::from_system().map_err(|error| error.to_string())?;
    let config_file = options
        .config_dir
        .as_ref()
        .map_or_else(|| system.config_file.clone(), |dir| dir.join("config.toml"));
    let config_dir = config_file
        .parent()
        .ok_or("the settings file has no directory")?
        .to_owned();
    fs::create_dir_all(&config_dir)
        .map_err(|error| format!("cannot create {}: {error}", config_dir.display()))?;

    // The CA certificate, next to the settings.
    let ca_file = config_dir.join("demo-ca.crt");
    let pem = match &options.ca {
        Some(path) => fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?,
        None => cluster_ca(&demo)?,
    };
    if !String::from_utf8_lossy(&pem).contains("BEGIN CERTIFICATE") {
        return Err("the CA certificate is not PEM".to_owned());
    }
    fs::write(&ca_file, &pem)
        .map_err(|error| format!("cannot write {}: {error}", ca_file.display()))?;

    let mut environment = demo_environment(&demo)?;
    environment.tls.ca_file = Some(ca_file);
    let ports = Ports::from_env(|name| std::env::var(name).ok())?;
    ports.apply(&mut environment, options.via_proxy)?;
    environment.groups = demo_groups(&demo)?;

    let store = ConfigStore::new(config_file.clone());
    let mut config = store
        .read()
        .map_err(|error| format!("{}: {error}", config_file.display()))?
        .unwrap_or_default();
    config
        .environments
        .retain(|other| other.id != environment.id);
    config.active_environment = Some(environment.id.clone());
    let id = environment.id.clone();
    let selected = options
        .select
        .as_deref()
        .map(|names| select(&environment, names))
        .transpose()?;
    config.environments.insert(0, environment);
    if let Some(theme) = options.theme {
        config.appearance.theme = theme;
    }
    let issues = config.validate();
    if !issues.is_empty() {
        let issues: Vec<String> = issues.iter().map(ToString::to_string).collect();
        return Err(format!(
            "the demo settings don't validate: {}",
            issues.join("; ")
        ));
    }
    store
        .save(&config)
        .map_err(|error| format!("{}: {error}", config_file.display()))?;
    println!("wrote the demo environment into {}", config_file.display());

    if let Some(selected) = selected {
        let paths = Paths {
            config_file: config_file.clone(),
            data_dir: options.data_dir.clone().unwrap_or(system.data_dir.clone()),
            log_dir: system.log_dir.clone(),
        };
        let state_store = paths.state_store();
        let mut state = state_store.load().unwrap_or_default();
        let mut environment_state = state.environment(&id);
        environment_state.selected = Some(selected);
        state.set_environment(&id, environment_state);
        state_store
            .save(&state)
            .map_err(|error| format!("cannot save the UI state: {error}"))?;
    }
    Ok(())
}

/// The ports the demo cluster publishes on this machine: master-01,
/// master-02 and the proxy in front of master-01. Like
/// `demo/docker-compose.yml`, from `ICYGUI_DEMO_PORT_1`, `_2` and `_PROXY`
/// when they are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ports {
    first: u16,
    second: u16,
    proxy: u16,
}

impl Ports {
    /// What `docker-compose.yml` uses without the variables.
    const DEFAULT: Self = Self {
        first: 5665,
        second: 5666,
        proxy: 5667,
    };

    /// The ports, from the variables `var` reads (unset or empty: the
    /// default).
    fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let port = |name: &str, default: u16| match var(name).filter(|value| !value.is_empty()) {
            None => Ok(default),
            Some(value) => value
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| format!("{name}={value} is not a port")),
        };
        Ok(Self {
            first: port("ICYGUI_DEMO_PORT_1", Self::DEFAULT.first)?,
            second: port("ICYGUI_DEMO_PORT_2", Self::DEFAULT.second)?,
            proxy: port("ICYGUI_DEMO_PORT_PROXY", Self::DEFAULT.proxy)?,
        })
    }

    /// Points the environment's URLs (master-01, then master-02) at these
    /// ports; master-01 through the proxy with `via_proxy`.
    fn apply(self, environment: &mut ic_config::Environment, via_proxy: bool) -> Result<()> {
        let first = if via_proxy { self.proxy } else { self.first };
        for (url, port) in environment.urls.iter_mut().zip([first, self.second]) {
            url.url = with_port(&url.url, port)?;
        }
        Ok(())
    }
}

/// `url` with its port replaced (`https://127.0.0.1:5665` → `…:5675`).
fn with_port(url: &str, port: u16) -> Result<String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url}: not a URL"))?;
    let (authority, path) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
    let host = match authority.rsplit_once(':') {
        Some((host, digits)) if digits.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    Ok(format!("{scheme}://{host}:{port}{path}"))
}

/// The environment of `demo/icygui/environment.toml`, read as a settings
/// file.
fn demo_environment(demo: &Path) -> Result<ic_config::Environment> {
    let path = demo.join("icygui/environment.toml");
    let config: Config = ConfigStore::new(path.clone())
        .load()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    match <[ic_config::Environment; 1]>::try_from(config.environments) {
        Ok([environment]) => Ok(environment),
        Err(_) => Err(format!("{} must hold one environment", path.display())),
    }
}

/// The dashboards of `demo/icygui/dashboards.toml`, imported like a shared
/// export, with ids from their names.
fn demo_groups(demo: &Path) -> Result<Vec<ic_config::DashboardGroup>> {
    let path = demo.join("icygui/dashboards.toml");
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut groups =
        ic_config::import_groups(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    for group in &mut groups {
        group.id = format!("demo-{}", slug(&group.name));
        for dashboard in &mut group.dashboards {
            dashboard.id = format!("{}-{}", group.id, slug(&dashboard.name));
            for (index, view) in dashboard.views.iter_mut().enumerate() {
                view.id = format!("{}-view-{index}", dashboard.id);
            }
        }
    }
    Ok(groups)
}

/// `overview/databases` → the dashboard's reference.
fn select(environment: &ic_config::Environment, names: &str) -> Result<DashboardRef> {
    let (group_name, dashboard_name) = names
        .split_once('/')
        .ok_or_else(|| format!("--select {names}: GROUP/DASHBOARD"))?;
    let group = environment
        .groups
        .iter()
        .find(|group| group.name == group_name)
        .ok_or_else(|| format!("--select: no group {group_name}"))?;
    let dashboard = group
        .dashboards
        .iter()
        .find(|dashboard| dashboard.name == dashboard_name)
        .ok_or_else(|| format!("--select: no dashboard {dashboard_name} in {group_name}"))?;
    Ok(DashboardRef {
        group_id: group.id.clone(),
        dashboard_id: dashboard.id.clone(),
    })
}

fn slug(name: &str) -> String {
    name.replace(' ', "-")
}

/// The CA certificate of the running demo cluster.
fn cluster_ca(demo: &Path) -> Result<Vec<u8>> {
    let output = Command::new("docker")
        .args(["compose", "-f"])
        .arg(demo.join("docker-compose.yml"))
        .args([
            "exec",
            "-T",
            "master-01",
            "cat",
            "/var/lib/icinga2/certs/ca.crt",
        ])
        .output()
        .map_err(|error| format!("docker: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot read the CA from the demo cluster (is it running? \
             docker compose -f demo/docker-compose.yml up -d --wait): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ic_config::{
        DowntimesMode, GridCells, HandledMode, HideHandled, ObjectKind, SidebarMark, ViewDisplay,
    };
    use ic_rules::ScopeSetting;

    use super::*;

    /// The ports the compose file and up.sh read move the environment too
    /// (and `--via-proxy` takes the proxy's).
    #[test]
    fn the_demo_ports_move_the_environment_with_the_cluster() {
        let demo = crate::root().join("demo");
        let urls = |ports: Ports, via_proxy: bool| {
            let mut environment = demo_environment(&demo).unwrap();
            ports.apply(&mut environment, via_proxy).unwrap();
            environment
                .urls
                .iter()
                .map(|url| url.url.clone())
                .collect::<Vec<_>>()
        };
        let none = Ports::from_env(|_| None).unwrap();
        assert_eq!(none, Ports::DEFAULT);
        assert_eq!(
            urls(none, false),
            ["https://127.0.0.1:5665", "https://127.0.0.1:5666"]
        );
        assert_eq!(
            urls(none, true),
            ["https://127.0.0.1:5667", "https://127.0.0.1:5666"]
        );
        let moved = Ports::from_env(|name| {
            match name {
                "ICYGUI_DEMO_PORT_1" => Some("15665"),
                "ICYGUI_DEMO_PORT_2" => Some("15666"),
                "ICYGUI_DEMO_PORT_PROXY" => Some("15667"),
                _ => None,
            }
            .map(str::to_owned)
        })
        .unwrap();
        assert_eq!(
            urls(moved, false),
            ["https://127.0.0.1:15665", "https://127.0.0.1:15666"]
        );
        assert_eq!(urls(moved, true)[0], "https://127.0.0.1:15667");
        assert!(Ports::from_env(|_| Some("http".to_owned())).is_err());
        assert!(Ports::from_env(|_| Some("0".to_owned())).is_err());
        assert_eq!(
            with_port("https://[::1]:5665/v1", 7000).unwrap(),
            "https://[::1]:7000/v1"
        );
        assert_eq!(
            with_port("https://master-01", 5665).unwrap(),
            "https://master-01:5665"
        );
    }

    /// The demo's files load through ic-config, and the dashboards show
    /// every view kind and layout the showcase list asks for (PLAN.md 4.2,
    /// "The demo is the showcase").
    #[test]
    fn the_demo_files_load_and_show_every_view_kind() {
        let demo = crate::root().join("demo");
        let environment = demo_environment(&demo).unwrap();
        assert_eq!(environment.urls.len(), 2, "both masters");
        assert!(
            environment
                .urls
                .iter()
                .all(|url| url.server_name().is_some())
        );
        let groups = demo_groups(&demo).unwrap();
        let views: Vec<_> = groups
            .iter()
            .flat_map(|group| &group.dashboards)
            .flat_map(|dashboard| &dashboard.views)
            .collect();
        let kinds: BTreeSet<String> = views
            .iter()
            .map(|view| format!("{:?}", view.display))
            .collect();
        for kind in ViewDisplay::ALL {
            assert!(kinds.contains(&format!("{kind:?}")), "{kind:?} missing");
        }
        assert!(
            views
                .iter()
                .any(|view| view.display == ViewDisplay::HostGroupGrid
                    && view.grid.cells == GridCells::Squares)
        );
        assert!(
            views
                .iter()
                .any(|view| view.display == ViewDisplay::HostGroupGrid
                    && view.grid.cells == GridCells::LabelledCells)
        );
        for mode in [DowntimesMode::Timeline, DowntimesMode::List] {
            assert!(
                views.iter().any(|view| view.display == ViewDisplay::Downtimes
                    && view.threads.mode == mode),
                "downtimes as {mode:?}"
            );
        }
        assert!(
            views
                .iter()
                .any(|view| view.object_kind == ObjectKind::Hosts)
        );
        let dashboards: Vec<_> = groups.iter().flat_map(|group| &group.dashboards).collect();
        assert!(
            dashboards
                .iter()
                .any(|dashboard| dashboard.views.len() == 1)
        );
        assert!(dashboards.iter().any(|dashboard| dashboard.views.len() > 2));
        assert!(
            dashboards
                .iter()
                .any(|dashboard| matches!(dashboard.mark, SidebarMark::Icon(_)))
        );
        // Handled problems hidden per kind (PLAN.md 4.2: "handled per
        // kind"): a view that hides some kinds and shows the others.
        assert!(
            views
                .iter()
                .any(|view| view.handled.mode == HandledMode::Hide
                    && view.handled.hide != HideHandled::ALL
                    && view.handled.hide.any()),
            "a view hiding some kinds of handled problems"
        );
        // Notification settings (groups on/off): a group off, a group and
        // a dashboard with their own setting.
        let group = |name: &str| groups.iter().find(|group| group.name == name).unwrap();
        assert_eq!(group("lab").notifications, ScopeSetting::Off);
        assert!(matches!(
            group("dba").notifications,
            ScopeSetting::Custom(ref rule) if rule.min_duration_secs > 0 && rule.states.warning
        ));
        assert!(
            dashboards
                .iter()
                .any(|dashboard| dashboard.notifications == ScopeSetting::On)
        );
        for names in ["overview/databases", "platform/fleet", "dba/dba"] {
            select(
                &ic_config::Environment {
                    groups: groups.clone(),
                    ..environment.clone()
                },
                names,
            )
            .unwrap();
        }
    }
}
