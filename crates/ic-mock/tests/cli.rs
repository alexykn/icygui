//! The `icinga-mock` binary: banner, serving, certificate reuse.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly"
)]

mod common;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

struct Banner {
    url: String,
    fingerprint: [u8; 32],
    fingerprint_text: String,
    user_line: String,
}

async fn launch(args: &[&str]) -> (Child, Banner) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_icinga-mock"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut url = None;
    let mut fingerprint_text = None;
    let mut user_line = None;
    while url.is_none() || fingerprint_text.is_none() || user_line.is_none() {
        let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .expect("banner within 20s")
            .unwrap()
            .expect("banner before exit");
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("URL") {
            url = Some(rest.trim().to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("SHA-256") {
            fingerprint_text = Some(rest.trim().to_owned());
        } else if trimmed.starts_with("User") {
            user_line = Some(trimmed.to_owned());
        }
    }
    let fingerprint_text = fingerprint_text.unwrap();
    let bytes: Vec<u8> = fingerprint_text
        .split(':')
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect();
    let banner = Banner {
        url: url.unwrap(),
        fingerprint: bytes.try_into().unwrap(),
        fingerprint_text,
        user_line: user_line.unwrap(),
    };
    (child, banner)
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("ic-mock-{tag}-{}-{nanos}", std::process::id()))
}

#[tokio::test]
async fn serves_with_banner_and_reuses_certificates() {
    let dir = temp_dir("certs");
    let dir_arg = dir.to_str().unwrap().to_owned();
    let args = [
        "--env",
        "lab:0",
        "--no-sim",
        "--user",
        "alice:wonder:*",
        "--cert-dir",
        &dir_arg,
    ];
    let (mut child, banner) = launch(&args).await;
    assert!(
        banner.url.starts_with("https://127.0.0.1:"),
        "{}",
        banner.url
    );
    assert!(
        banner.user_line.contains("alice:wonder"),
        "{}",
        banner.user_line
    );
    assert_eq!(
        banner.fingerprint_text,
        banner.fingerprint_text.to_uppercase()
    );

    let client = common::client_from(common::pinned_config(banner.fingerprint));
    let response = client
        .get(format!("{}/v1/objects/hosts", banner.url))
        .basic_auth("alice", Some("wonder"))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    child.kill().await.unwrap();

    // The second start reads the certificate back: same fingerprint.
    let (mut child, second) = launch(&args).await;
    assert_eq!(second.fingerprint, banner.fingerprint);
    child.kill().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Switching `--tls` modes over one `--cert-dir` replaces the certificate
/// that no longer fits, then keeps reusing the new one.
#[tokio::test]
async fn cert_dir_follows_the_tls_mode() {
    let dir = temp_dir("modes");
    let dir_arg = dir.to_str().unwrap().to_owned();
    let self_signed = ["--env", "lab:0", "--no-sim", "--cert-dir", &dir_arg];
    let with_ca = [
        "--env",
        "lab:0",
        "--no-sim",
        "--tls",
        "ca",
        "--cert-dir",
        &dir_arg,
    ];
    let (mut child, first) = launch(&self_signed).await;
    child.kill().await.unwrap();
    let (mut child, ca_run) = launch(&with_ca).await;
    child.kill().await.unwrap();
    assert_ne!(ca_run.fingerprint, first.fingerprint, "re-issued by the CA");
    assert!(dir.join("ca.crt").exists());
    let (mut child, again) = launch(&with_ca).await;
    child.kill().await.unwrap();
    assert_eq!(again.fingerprint, ca_run.fingerprint, "reused under the CA");

    // The reused certificate verifies against the CA file.
    let ca = std::fs::read_to_string(dir.join("ca.crt")).unwrap();
    let (mut child, served) = launch(&with_ca).await;
    let client = common::client_from(common::ca_config(&ca, None));
    let response = client
        .get(format!("{}/v1", served.url))
        .basic_auth("root", Some("icinga"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    child.kill().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `icinga-mock | head`: a closed stdout must not stop the servers.
#[tokio::test]
async fn survives_a_closed_stdout() {
    // The second environment's banner is written after the reader is gone.
    let (mut child, banner) = launch(&["--env", "lab:0", "--env", "staging:0", "--no-sim"]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "icinga-mock exited after its stdout was closed"
    );
    let client = common::client_from(common::pinned_config(banner.fingerprint));
    let response = client
        .get(format!("{}/v1", banner.url))
        .basic_auth("root", Some("icinga"))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    child.kill().await.unwrap();
}

/// Opens an event stream on a served environment.
async fn events(banner: &Banner, types: &[&str]) -> common::EventStream {
    let client = common::client_from(common::pinned_config(banner.fingerprint));
    let response = client
        .post(format!("{}/v1/events", banner.url))
        .basic_auth("root", Some("icinga"))
        .header("Accept", "application/json")
        .json(&serde_json::json!({"types": types, "queue": "cli"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    common::EventStream::from_response(response)
}

/// Reads `count` events and returns the objects they are about.
async fn objects_of(stream: &mut common::EventStream, count: usize) -> Vec<String> {
    let mut objects = Vec::new();
    for _ in 0..count {
        let event = stream.next().await;
        assert_eq!(event["type"], "CheckResult");
        let host = event["host"].as_str().unwrap();
        objects.push(match event["service"].as_str() {
            Some(service) => format!("{host}!{service}"),
            None => host.to_owned(),
        });
    }
    objects.sort();
    objects
}

/// `kill -USR1` re-checks every object of every environment.
#[cfg(unix)]
#[tokio::test]
async fn sigusr1_starts_a_burst() {
    let (mut child, banner) = launch(&["--env", "lab:0", "--no-sim"]).await;
    let mut stream = events(&banner, &["CheckResult"]).await;
    let pid = child.id().unwrap().to_string();
    let status = Command::new("kill")
        .args(["-USR1", &pid])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    // The lab: two hosts and five services.
    let objects = objects_of(&mut stream, 7).await;
    assert!(objects.contains(&"lab-02!ping4".to_owned()), "{objects:?}");
    assert!(child.try_wait().unwrap().is_none(), "still serving");
    child.kill().await.unwrap();
}

/// `--burst-every` re-checks every object periodically.
#[tokio::test]
async fn burst_every_rechecks_periodically() {
    let (mut child, banner) = launch(&["--env", "lab:0", "--no-sim", "--burst-every", "1"]).await;
    let mut stream = events(&banner, &["CheckResult"]).await;
    let first = objects_of(&mut stream, 7).await;
    let second = objects_of(&mut stream, 7).await;
    assert_eq!(first, second);
    child.kill().await.unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_icinga-mock"))
        .args(["--env", "lab:0", "--burst-every", "0"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success(), "a zero period is refused");
}

#[tokio::test]
async fn rejects_unknown_environments() {
    let output = Command::new(env!("CARGO_BIN_EXE_icinga-mock"))
        .args(["--env", "moon-base"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown environment 'moon-base'"),
        "{stderr}"
    );
}
