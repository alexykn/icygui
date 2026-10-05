//! `icinga-mock`: serves one or more mock Icinga 2 environments for
//! development (PLAN.md §3.6).
//!
//! ```text
//! icinga-mock --env prod-cluster:5665 --env staging:5666 --env lab:5667 --cert-dir ./mock-certs
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use ic_mock::{
    MockConfig, MockError, MockServer, MockTls, MockUser, NumberFormat, SimulationConfig,
    StormConfig, TlsMaterial, scenarios,
};
use tracing_subscriber::EnvFilter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum TlsMode {
    /// A self-signed certificate per environment.
    SelfSigned,
    /// One "Icinga CA" signing a certificate per environment.
    Ca,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Numbers {
    /// `200.0`, like the API documentation and Icinga up to 2.12.
    Float,
    /// `200`, like Icinga 2.13+.
    Integral,
}

/// Mock Icinga 2 API environments for developing and testing icygui.
#[derive(Debug, Parser)]
#[command(name = "icinga-mock", version)]
struct Args {
    /// Environment to serve, as NAME[:PORT]. Names: prod-cluster, staging,
    /// lab, large. Repeatable. Default: prod-cluster:5665.
    #[arg(long = "env", value_name = "NAME[:PORT]")]
    envs: Vec<String>,
    /// Seed of the simulator and of the large scenario.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Simulation speed factor (2.0 = twice as fast).
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// API user as USER:PASS[:PERM,PERM,...] (no ':' in passwords).
    /// Repeatable. Default: root:icinga with "*".
    #[arg(long = "user", value_name = "USER:PASS[:PERMS]")]
    users: Vec<String>,
    /// Serve the scenarios without the simulator.
    #[arg(long)]
    no_sim: bool,
    /// Certificates.
    #[arg(long, value_enum, default_value_t = TlsMode::SelfSigned)]
    tls: TlsMode,
    /// Write certificates (and the CA) here, and reuse them on the next
    /// start so fingerprints stay the same.
    #[arg(long, value_name = "DIR")]
    cert_dir: Option<PathBuf>,
    /// How JSON numbers are written.
    #[arg(long, value_enum, default_value_t = Numbers::Float)]
    numbers: Numbers,
    /// Start a problem storm (30 services failing) every N ticks.
    #[arg(long, value_name = "TICKS")]
    storm_every: Option<u64>,
}

/// One environment to serve.
#[derive(Debug)]
struct EnvSpec {
    name: String,
    port: u16,
}

fn parse_envs(envs: &[String]) -> Result<Vec<EnvSpec>, String> {
    let requested: Vec<String> = if envs.is_empty() {
        vec!["prod-cluster:5665".to_owned()]
    } else {
        envs.to_vec()
    };
    let mut specs = Vec::new();
    let mut ports = BTreeSet::new();
    for entry in &requested {
        let (name, port) = match entry.split_once(':') {
            Some((name, port)) => (
                name,
                Some(
                    port.parse::<u16>()
                        .map_err(|_| format!("invalid port in --env {entry}"))?,
                ),
            ),
            None => (entry.as_str(), None),
        };
        if !scenarios::NAMES.contains(&name) {
            return Err(format!(
                "unknown environment '{name}' (choose from {})",
                scenarios::NAMES.join(", ")
            ));
        }
        if let Some(port) = port
            && port != 0
            && !ports.insert(port)
        {
            return Err(format!("port {port} is used twice"));
        }
        specs.push((name.to_owned(), port));
    }
    let mut next = 5665;
    Ok(specs
        .into_iter()
        .map(|(name, port)| {
            let port = port.unwrap_or_else(|| {
                while ports.contains(&next) {
                    next += 1;
                }
                ports.insert(next);
                next
            });
            EnvSpec { name, port }
        })
        .collect())
}

fn parse_users(users: &[String]) -> Result<Vec<MockUser>, String> {
    if users.is_empty() {
        return Ok(vec![MockUser::root()]);
    }
    users
        .iter()
        .map(|entry| {
            let mut parts = entry.splitn(3, ':');
            let (Some(user), Some(password)) = (parts.next(), parts.next()) else {
                return Err(format!("--user {entry}: expected USER:PASS[:PERMS]"));
            };
            if user.is_empty() || password.is_empty() {
                return Err(format!(
                    "--user {entry}: user and password must not be empty"
                ));
            }
            let permissions: Vec<&str> = match parts.next() {
                Some(perms) => perms.split(',').filter(|p| !p.is_empty()).collect(),
                None => vec!["*"],
            };
            Ok(MockUser::new(user, password, &permissions))
        })
        .collect()
}

fn write_file(path: &Path, contents: &str, secret: bool) -> Result<(), MockError> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = secret;
    Ok(())
}

fn read_pair(cert: &Path, key: &Path) -> Result<Option<(String, String)>, MockError> {
    match (cert.exists(), key.exists()) {
        (true, true) => Ok(Some((
            std::fs::read_to_string(cert)?,
            std::fs::read_to_string(key)?,
        ))),
        (false, false) => Ok(None),
        _ => Err(MockError::Certificate(format!(
            "found only one of {} and {}; remove it or add the other",
            cert.display(),
            key.display()
        ))),
    }
}

/// Certificates for one environment, reused from `dir` when present.
fn material_for(
    dir: Option<&Path>,
    mode: TlsMode,
    env: &str,
    node_name: &str,
    ca: Option<&(String, String)>,
) -> Result<MockTls, MockError> {
    let Some(dir) = dir else {
        return Ok(match mode {
            TlsMode::SelfSigned => MockTls::SelfSigned,
            TlsMode::Ca => match ca {
                Some((ca_pem, ca_key)) => {
                    MockTls::Provided(TlsMaterial::signed_by_ca(node_name, ca_pem, ca_key)?)
                }
                None => MockTls::CaSigned,
            },
        });
    };
    let cert_path = dir.join(format!("{env}.crt"));
    let key_path = dir.join(format!("{env}.key"));
    if let Some((cert_pem, key_pem)) = read_pair(&cert_path, &key_path)? {
        let stored = TlsMaterial {
            cert_pem,
            key_pem,
            ca_pem: ca.map(|(pem, _)| pem.clone()),
            ca_key_pem: ca.map(|(_, key)| key.clone()),
        };
        // Reuse what still fits (same fingerprint as last time); replace
        // certificates that expired or belong to another mode or CA.
        match stored.validate() {
            Ok(()) => return Ok(MockTls::Provided(stored)),
            Err(error) => tracing::warn!(
                path = %cert_path.display(),
                %error,
                "not reusing the stored certificate; writing a new one"
            ),
        }
    }
    let material = match (mode, ca) {
        (TlsMode::Ca, Some((ca_pem, ca_key))) => {
            TlsMaterial::signed_by_ca(node_name, ca_pem, ca_key)?
        }
        _ => TlsMaterial::self_signed(node_name)?,
    };
    write_file(&cert_path, &material.cert_pem, false)?;
    write_file(&key_path, &material.key_pem, true)?;
    Ok(MockTls::Provided(material))
}

/// The CA for `--tls ca`, reused from `dir` when present.
fn shared_ca(dir: Option<&Path>) -> Result<(String, String), MockError> {
    if let Some(dir) = dir
        && let Some(pair) = read_pair(&dir.join("ca.crt"), &dir.join("ca.key"))?
    {
        return Ok(pair);
    }
    let material = TlsMaterial::ca_signed("icinga-mock")?;
    let (Some(ca_pem), Some(ca_key)) = (material.ca_pem, material.ca_key_pem) else {
        return Err(MockError::Certificate("CA generation failed".to_owned()));
    };
    if let Some(dir) = dir {
        write_file(&dir.join("ca.crt"), &ca_pem, false)?;
        write_file(&dir.join("ca.key"), &ca_key, true)?;
    }
    Ok((ca_pem, ca_key))
}

/// The startup banner of one environment.
fn banner(
    env: &str,
    server: &MockServer,
    users: &[MockUser],
    ca_path: Option<&Path>,
    cert_path: Option<&Path>,
    simulation: &SimulationConfig,
) -> String {
    use std::fmt::Write as _;
    let status = server.control().status();
    let mut text = String::new();
    let _ = writeln!(text, "icinga-mock · {env}");
    let _ = writeln!(text, "  URL          {}", server.url());
    for user in users {
        let _ = writeln!(
            text,
            "  User         {}:{} ({})",
            user.username,
            user.password,
            user.permissions.join(", ")
        );
    }
    let _ = writeln!(text, "  SHA-256      {}", server.cert_fingerprint());
    let _ = match (ca_path, cert_path) {
        (Some(ca), _) => writeln!(text, "  CA file      {}", ca.display()),
        (None, Some(cert)) => writeln!(text, "  Certificate  {} (self-signed)", cert.display()),
        (None, None) => writeln!(
            text,
            "  CA file      none (self-signed; use --cert-dir to write it)"
        ),
    };
    let _ = writeln!(
        text,
        "  Node         {} (Icinga {})",
        status.node_name, status.version
    );
    let _ = if simulation.enabled {
        writeln!(
            text,
            "  Simulator    running (seed {}, speed {})",
            simulation.seed, simulation.speed
        )
    } else {
        writeln!(text, "  Simulator    off")
    };
    text.push('\n');
    text
}

/// Writes the banner to stdout. A closed stdout (`icinga-mock | head`) must
/// not take the servers down, so write errors are only logged.
fn print_banner(text: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        tracing::debug!(%error, "can't write the banner to stdout");
    }
}

async fn run(args: Args) -> Result<(), String> {
    let envs = parse_envs(&args.envs)?;
    let users = parse_users(&args.users)?;
    if !(args.speed.is_finite() && args.speed > 0.0) {
        return Err("--speed must be a positive number".to_owned());
    }
    let dir = args.cert_dir.as_deref();
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let ca = match args.tls {
        TlsMode::Ca => Some(shared_ca(dir).map_err(|e| e.to_string())?),
        TlsMode::SelfSigned => None,
    };
    let simulation = SimulationConfig {
        enabled: !args.no_sim,
        seed: args.seed,
        speed: args.speed,
        storm: args.storm_every.map(|every| StormConfig {
            every_ticks: every,
            ..StormConfig::default()
        }),
        ..SimulationConfig::default()
    };
    let mut servers = Vec::new();
    for env in &envs {
        let scenario = scenarios::by_name(&env.name, args.seed)
            .ok_or_else(|| format!("unknown environment '{}'", env.name))?;
        let tls = material_for(
            dir,
            args.tls,
            &env.name,
            &scenario.status.node_name,
            ca.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        let config = MockConfig {
            scenario,
            port: env.port,
            users: users.clone(),
            simulation: simulation.clone(),
            tls,
            number_format: match args.numbers {
                Numbers::Float => NumberFormat::Float,
                Numbers::Integral => NumberFormat::Integral,
            },
            ..MockConfig::default()
        };
        let server = MockServer::start(config)
            .await
            .map_err(|e| format!("{}: {e}", env.name))?;
        let ca_path = (args.tls == TlsMode::Ca)
            .then(|| dir.map(|d| d.join("ca.crt")))
            .flatten();
        let cert_path = dir.map(|d| d.join(format!("{}.crt", env.name)));
        print_banner(&banner(
            &env.name,
            &server,
            &users,
            ca_path.as_deref(),
            cert_path.as_deref(),
            &simulation,
        ));
        servers.push(server);
    }
    wait_for_signal().await;
    tracing::info!("shutting down");
    for server in servers {
        server.shutdown().await;
    }
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(error) => tracing::warn!(%error, "can't listen for SIGTERM"),
        }
    }
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(%error, "can't listen for Ctrl-C; stopping in a day");
        tokio::time::sleep(Duration::from_hours(24)).await;
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("ic_mock=info,icinga_mock=info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let args = Args::parse();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envs_get_ports() {
        let envs =
            parse_envs(&["prod-cluster:5665".into(), "staging".into(), "lab".into()]).unwrap();
        let ports: Vec<u16> = envs.iter().map(|e| e.port).collect();
        assert_eq!(ports, vec![5665, 5666, 5667]);
        assert_eq!(parse_envs(&[]).unwrap()[0].name, "prod-cluster");
        assert!(parse_envs(&["nope".into()]).is_err());
        assert!(parse_envs(&["lab:1".into(), "staging:1".into()]).is_err());
        assert!(parse_envs(&["lab:x".into()]).is_err());
    }

    #[test]
    fn users_parse_permissions() {
        let users = parse_users(&["ro:secret:objects/query/*,status/query".into()]).unwrap();
        assert_eq!(users[0].username, "ro");
        assert_eq!(
            users[0].permissions,
            vec!["objects/query/*", "status/query"]
        );
        assert_eq!(
            parse_users(&["a:b".into()]).unwrap()[0].permissions,
            vec!["*"]
        );
        assert_eq!(parse_users(&[]).unwrap()[0].username, "root");
        assert!(parse_users(&["nopass".into()]).is_err());
        assert!(parse_users(&[":x".into()]).is_err());
    }
}
