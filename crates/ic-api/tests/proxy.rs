//! Proxy environment variables are ignored: the client always connects
//! straight to Icinga. A test binary of its own, because it changes the
//! process environment, which no other test may see.

#![expect(
    clippy::unwrap_used,
    reason = "test harness: a failure should panic the test"
)]

#[expect(
    dead_code,
    reason = "shared test support; this binary uses only part of it"
)]
mod support;

use ic_api::Client;
use serde_json::json;
use support::{Pki, SERVER_NAME, ServerOptions, TestServer, basic_settings, ca_trust, ok_json};

/// Points every proxy variable at `proxy` and clears the exceptions.
#[expect(
    unsafe_code,
    reason = "changing the environment; this binary runs a single test"
)]
fn use_proxy_everywhere(proxy: &str) {
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        // SAFETY: this test binary has a single test and sets the variables
        // before it starts anything that could read the environment
        // concurrently (no client, no resolver thread exists yet).
        unsafe { std::env::set_var(name, proxy) };
    }
    for name in ["NO_PROXY", "no_proxy"] {
        // SAFETY: as above.
        unsafe { std::env::remove_var(name) };
    }
}

#[tokio::test]
async fn proxy_environment_variables_are_ignored() {
    // A "proxy" that refuses every connection: requests only work if the
    // client doesn't use it.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    use_proxy_everywhere(&format!("http://{closed}"));

    let pki = Pki::new();
    let server = TestServer::start(ServerOptions::new(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |_| {
            ok_json(&json!({ "results": [{
                "permissions": ["*"], "user": "icygui", "version": "v2.15.6"
            }]}))
        },
    ))
    .await;
    let client = Client::new(basic_settings(server.url(), ca_trust(&pki))).unwrap();
    assert_eq!(client.info().await.unwrap().user, "icygui");
    assert_eq!(server.requests().len(), 1, "straight to the server");
}
