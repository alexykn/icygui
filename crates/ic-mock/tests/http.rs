//! Protocol-level behaviour: info and status, authentication, permissions,
//! error bodies, method override, body limits and request recording.

mod common;

use common::{PASSWORD, USER, get, json, lab, prod, request, results, start};
use ic_mock::{MockConfig, MockUser, NumberFormat};
use reqwest::{Method, StatusCode};
use serde_json::json;

#[tokio::test]
async fn info_lists_user_permissions_and_version() {
    let (server, client) = lab().await;
    let (status, body) = get(&client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);
    let info = &results(&body)[0];
    assert_eq!(info["user"], "root");
    assert_eq!(info["permissions"], json!(["*"]));
    assert_eq!(info["version"], "r2.14.3-1");
    assert!(info["info"].as_str().unwrap().contains("documentation"));

    // Without `Accept: application/json` Icinga answers with HTML.
    let response = client
        .get(format!("{}/v1", server.url()))
        .basic_auth(USER, Some(PASSWORD))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Hello from Icinga 2")
    );
}

#[tokio::test]
async fn server_header_names_the_version() {
    let (server, client) = lab().await;
    let response = request(&client, &server, Method::GET, "/v1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["server"], "Icinga/r2.14.3-1");
}

#[tokio::test]
async fn status_lists_every_component() {
    let (server, client) = prod().await;
    let (status, body) = get(&client, &server, "/v1/status").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = results(&body)
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    // Every status function of Icinga 2.15, enabled or not.
    assert_eq!(
        names,
        [
            "ApiListener",
            "CIB",
            "CheckerComponent",
            "ElasticsearchWriter",
            "FileLogger",
            "GelfWriter",
            "GraphiteWriter",
            "IcingaApplication",
            "IdoMysqlConnection",
            "IdoPgsqlConnection",
            "Influxdb2Writer",
            "InfluxdbWriter",
            "JournaldLogger",
            "NotificationComponent",
            "OpenTsdbWriter",
            "PerfdataWriter",
            "SyslogLogger",
        ]
    );
    let graphite = results(&body)
        .iter()
        .find(|entry| entry["name"] == "GraphiteWriter")
        .unwrap();
    assert_eq!(
        graphite,
        &json!({"name": "GraphiteWriter", "perfdata": [], "status": {"graphitewriter": {}}})
    );

    let (status, body) = get(&client, &server, "/v1/status/IcingaApplication").await;
    assert_eq!(status, StatusCode::OK);
    let app = &results(&body)[0]["status"]["icingaapplication"]["app"];
    assert_eq!(app["node_name"], "master-01");
    assert_eq!(app["version"], "r2.14.3-1");
    assert!(app["program_start"].as_f64().unwrap() > 1.0e9);

    let (status, body) = get(&client, &server, "/v1/status/CIB").await;
    assert_eq!(status, StatusCode::OK);
    let cib = &results(&body)[0]["status"];
    assert_eq!(cib["num_hosts_down"], json!(3));
    assert_eq!(cib["num_hosts_unreachable"], json!(5));
    let summary = ic_mock::scenarios::prod_cluster().summary();
    assert_eq!(
        cib["num_services_critical"],
        json!(summary.services_critical)
    );

    let (status, body) = get(&client, &server, "/v1/status/Nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": 404, "status": "No objects found."}));
}

#[tokio::test]
async fn bad_credentials_get_icinga_401() {
    let (server, client) = lab().await;
    for auth in [Some(("root", "wrong")), Some(("nobody", "icinga")), None] {
        let mut builder = client
            .get(format!("{}/v1/objects/hosts", server.url()))
            .header("Accept", "application/json");
        if let Some((user, password)) = auth {
            builder = builder.basic_auth(user, Some(password));
        }
        let response = builder.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()["www-authenticate"],
            "Basic realm=\"Icinga 2\""
        );
        let (_, body) = json(response).await;
        assert_eq!(
            body,
            json!({"error": 401, "status": "Unauthorized. Please check your user credentials."})
        );
    }

    // Any other Accept gets HTML.
    let response = client
        .get(format!("{}/v1/objects/hosts", server.url()))
        .basic_auth("root", Some("wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.text().await.unwrap(),
        "<h1>Unauthorized. Please check your user credentials.</h1>"
    );
}

#[tokio::test]
async fn unknown_paths_are_404_with_icinga_message() {
    let (server, client) = lab().await;
    let (status, body) = get(&client, &server, "/v1/nonsense").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body["status"],
        "The requested path 'v1/nonsense' could not be found or the request method is not valid for this path."
    );
    // Actions only answer POST.
    let (status, _) = get(&client, &server, "/v1/actions/reschedule-check").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_without_json_accept_is_rejected_before_auth() {
    let (server, client) = lab().await;
    let response = client
        .post(format!("{}/v1/actions/reschedule-check", server.url()))
        .basic_auth("root", Some("wrong"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Accept header is missing or not set to 'application/json'.")
    );
}

#[tokio::test]
async fn method_override_turns_post_into_get() {
    let (server, client) = lab().await;
    let response = request(&client, &server, Method::POST, "/v1/objects/hosts")
        .header("X-HTTP-Method-Override", "GET")
        .json(&json!({"filter": "host.name == \"lab-01\"", "attrs": ["name", "address"]}))
        .send()
        .await
        .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&body)[0]["attrs"],
        json!({"name": "lab-01", "address": "192.168.56.11"})
    );
    let recorded = server.control().requests();
    let last = recorded.last().unwrap();
    assert_eq!(last.method, "POST");
    assert_eq!(last.effective_method(), "GET");
    assert_eq!(last.user.as_deref(), Some("root"));
    assert_eq!(last.status, 200);
    assert_eq!(
        last.body.as_ref().unwrap()["attrs"],
        json!(["name", "address"])
    );
}

#[tokio::test]
async fn invalid_bodies_are_400() {
    let (server, client) = lab().await;
    for body in ["{nope", "[1, 2]", "\"text\""] {
        let response = request(
            &client,
            &server,
            Method::POST,
            "/v1/actions/reschedule-check",
        )
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
        let (status, json) = json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            json["status"]
                .as_str()
                .unwrap()
                .starts_with("Invalid request body: "),
            "{json}"
        );
    }
}

#[tokio::test]
async fn bodies_over_one_mebibyte_are_refused() {
    // Users with `config/modify` (like `*`) may send 512 MiB; others 1 MiB.
    let config = MockConfig {
        users: vec![MockUser::new("operator", "secret", &["actions/*"])],
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let big = format!("{{\"comment\": \"{}\"}}", "x".repeat(1024 * 1024 + 10));
    let response = client
        .post(format!("{}/v1/actions/add-comment", server.url()))
        .basic_auth("operator", Some("secret"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(big)
        .send()
        .await;
    // Icinga answers 400 and closes the connection; the client may see
    // either the answer or the reset.
    if let Ok(response) = response {
        let status = response.status();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("Bad Request: body limit exceeded")
        );
    }
}

#[tokio::test]
async fn permissions_are_checked_per_type_and_action() {
    let config = MockConfig {
        users: vec![
            MockUser::root(),
            MockUser::new("viewer", "secret", &["objects/query/Host", "status/query"]),
            MockUser::new(
                "operator",
                "secret",
                &[
                    "objects/query/*",
                    "actions/acknowledge-problem",
                    "filter-expression",
                ],
            ),
        ],
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let as_user = |user: &'static str, method: Method, path: &str| {
        client
            .request(method, format!("{}{path}", server.url()))
            .basic_auth(user, Some("secret"))
            .header("Accept", "application/json")
    };

    let (status, _) = json(
        as_user("viewer", Method::GET, "/v1/objects/hosts")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = json(
        as_user("viewer", Method::GET, "/v1/objects/services")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"error": 403, "status": "Missing permission: objects/query/service"})
    );
    // Filters need `filter-expression` (Icinga's default enforcement).
    let (status, body) = json(
        as_user(
            "viewer",
            Method::GET,
            "/v1/objects/hosts?filter=host.name==%22lab-01%22",
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["status"], "Missing permission: filter-expression");

    let (status, body) = json(
        as_user("operator", Method::POST, "/v1/actions/reschedule-check")
            .json(&json!({"type": "Host", "host": "lab-01"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["status"],
        "Missing permission: actions/reschedule-check"
    );

    // Event streams the user may not read look like unknown paths.
    let (status, body) = json(
        as_user("viewer", Method::POST, "/v1/events")
            .json(&json!({"types": ["CheckResult"], "queue": "q"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["status"]
            .as_str()
            .unwrap()
            .starts_with("The requested path")
    );
}

#[tokio::test]
async fn filter_expression_enforcement_can_be_disabled() {
    let config = MockConfig {
        enforce_filter_expression_permission: false,
        users: vec![MockUser::read_only("viewer", "secret")],
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let response = client
        .get(format!(
            "{}/v1/objects/hosts?filter=host.name==%22lab-01%22",
            server.url()
        ))
        .basic_auth("viewer", Some("secret"))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(results(&body).len(), 1);
}

#[tokio::test]
async fn number_formats() {
    // The default, like Icinga 2.15: whole numbers are integers.
    let (server, client) = lab().await;
    let path = "/v1/objects/hosts/lab-01?attrs=state&attrs=max_check_attempts&attrs=check_interval&attrs=last_check";
    let (_, body) = get(&client, &server, path).await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state"], json!(0));
    assert_eq!(attrs["max_check_attempts"], json!(3));
    assert!(attrs["state"].is_u64());
    assert!(
        attrs["last_check"].is_f64(),
        "fractional seconds stay floats"
    );
    let (_, body) = get(&client, &server, "/v1/nonsense").await;
    assert_eq!(body["error"], json!(404));

    // The documentation's style: every number with a fraction.
    let config = MockConfig {
        number_format: NumberFormat::Float,
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let (_, body) = get(&client, &server, path).await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state"], json!(0.0));
    assert_eq!(attrs["max_check_attempts"], json!(3.0));
    assert_eq!(attrs["check_interval"], json!(60.0));
    let (_, body) = get(&client, &server, "/v1/nonsense").await;
    assert_eq!(body["error"], json!(404.0));
}

#[tokio::test]
async fn pretty_output_is_indented() {
    let (server, client) = lab().await;
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/objects/hosts/lab-01?attrs=name&pretty=1",
    )
    .send()
    .await
    .unwrap();
    let text = response.text().await.unwrap();
    assert!(text.contains("\n    \"results\": ["), "{text}");
}

/// Icinga reads `pretty` and `verbose` through numbers ("0" is false);
/// values it can't convert make the answer a bare 500 (recorded for
/// queries in `fixtures/.../queries.json`). Following its sources, an
/// action still runs when only its answer fails, and `verbose` is read
/// before any action runs.
#[tokio::test]
async fn unreadable_flags_fail_the_answer() {
    let (server, client) = lab().await;
    let unhandled = json!({"error": 500, "status": "Unhandled exception"});
    let (status, body) = get(&client, &server, "/v1/status/CIB?pretty=true").await;
    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, unhandled.clone())
    );
    let (status, _) = get(&client, &server, "/v1/status/CIB?pretty=0&verbose=yes").await;
    assert_eq!(status, StatusCode::OK, "verbose only matters for errors");
    let (status, body) = get(&client, &server, "/v1/status/nope?verbose=yes").await;
    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, unhandled.clone())
    );

    let comments = server.control().comments().len();
    let add = |flags: &str| {
        request(
            &client,
            &server,
            Method::POST,
            &format!("/v1/actions/add-comment?{flags}"),
        )
        .json(&json!({"type": "Host", "hosts": ["lab-01"], "author": "a", "comment": "c"}))
    };
    let (status, body) = json(add("verbose=yes").send().await.unwrap()).await;
    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, unhandled.clone())
    );
    assert_eq!(server.control().comments().len(), comments, "nothing ran");
    let (status, body) = json(add("pretty=yes").send().await.unwrap()).await;
    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, unhandled)
    );
    assert_eq!(server.control().comments().len(), comments + 1, "it ran");
    let (status, _) = json(add("pretty=0&verbose=1").send().await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn injected_failures_and_latency() {
    let (server, client) = lab().await;
    let control = server.control();
    control.fail_next(2, 503);
    for _ in 0..2 {
        let (status, body) = get(&client, &server, "/v1/status").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], json!(503));
        assert!(
            body["status"]
                .as_str()
                .unwrap()
                .contains("failure injected by ic-mock")
        );
    }
    let (status, _) = get(&client, &server, "/v1/status").await;
    assert_eq!(status, StatusCode::OK);

    control.set_latency(std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    let (status, _) = get(&client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(started.elapsed() >= std::time::Duration::from_millis(200));
    control.set_latency(std::time::Duration::ZERO);

    control.clear_requests();
    assert!(control.requests().is_empty());
}

#[tokio::test]
async fn shutdown_stops_listening() {
    let (server, client) = lab().await;
    let url = server.url();
    server.shutdown().await;
    assert!(client.get(format!("{url}/v1")).send().await.is_err());
}
