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
//!                         [--secrets-dir DIR] [--via-proxy]
//!                         [--select GROUP/DASHBOARD] [--theme dark|light|system]
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
//! - `--secrets-dir`: also stores the demo password (a documented demo
//!   value) as `<DIR>/<environment id>`, the format `ICYGUI_DEV_SECRETS_DIR`
//!   reads (development and headless runs only; `ic_platform::DirSecrets`).
//!   Without it icygui asks for the password like for any environment.
//! - `--via-proxy`: master-01 through the cluster's proxy
//!   (`https://127.0.0.1:5667`), for the dead-network scenario.
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

/// The demo API user's password: a documented demo value (docs/demo.md),
/// not a secret.
const DEMO_PASSWORD: &str = "icygui-demo-password";

/// What `demo-config` was asked to do.
#[derive(Debug, Default)]
struct Options {
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    ca: Option<PathBuf>,
    secrets_dir: Option<PathBuf>,
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
            "--secrets-dir" => options.secrets_dir = Some(PathBuf::from(value()?)),
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
    if options.via_proxy
        && let Some(first) = environment.urls.first_mut()
    {
        "https://127.0.0.1:5667".clone_into(&mut first.url);
    }
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

    if let Some(dir) = &options.secrets_dir {
        write_password(dir, &id)?;
        println!(
            "stored the demo password in {} (ICYGUI_DEV_SECRETS_DIR)",
            dir.display()
        );
    }
    Ok(())
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

/// The demo password as `<dir>/<id>`, user-only.
fn write_password(dir: &Path, id: &str) -> Result<()> {
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let file = dir.join(id);
    fs::write(&file, DEMO_PASSWORD)
        .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for (path, mode) in [(dir, 0o700), (file.as_path(), 0o600)] {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .map_err(|error| format!("cannot protect {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ic_config::{DowntimesMode, GridCells, ObjectKind, SidebarMark, ViewDisplay};

    use super::*;

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
