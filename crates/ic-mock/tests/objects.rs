//! `/v1/objects`: types, names, attributes, filters, joins and meta.

#![allow(clippy::unwrap_used, reason = "test helpers fail the test loudly")]

mod common;

use common::{get, json, lab, post, prod, request, results};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn names(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = results(body)
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn single_objects_by_name_and_plural() {
    let (server, client) = prod().await;
    let (status, body) = get(&client, &server, "/v1/objects/hosts/db-prod-03").await;
    assert_eq!(status, StatusCode::OK);
    let host = &results(&body)[0];
    assert_eq!(host["name"], "db-prod-03");
    assert_eq!(host["type"], "Host");
    assert_eq!(host["meta"], json!({}));
    assert_eq!(host["joins"], json!({}));
    assert_eq!(host["attrs"]["__name"], "db-prod-03");
    assert_eq!(host["attrs"]["type"], "Host");

    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/services/db-prod-03!postgres-replication?attrs=state&attrs=state_type&attrs=last_check_result",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state"], json!(2.0));
    assert_eq!(attrs["state_type"], json!(1.0));
    let cr = &attrs["last_check_result"];
    assert_eq!(cr["type"], "CheckResult");
    assert!(cr["output"].as_str().unwrap().starts_with("CRITICAL"));
    assert!(!cr["performance_data"].as_array().unwrap().is_empty());

    let (status, body) = get(&client, &server, "/v1/objects/hosts/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["status"], "No objects found.");
}

#[tokio::test]
async fn unknown_types_and_bad_parameters() {
    let (server, client) = lab().await;
    let (status, body) = get(&client, &server, "/v1/objects/widgets").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "Invalid type specified.");

    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({"attrs": "name"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["status"],
        "Invalid type for 'attrs' attribute specified. Array type is required."
    );

    // Unknown attributes produce a per-object error entry.
    let (status, body) = get(&client, &server, "/v1/objects/hosts/lab-01?attrs=bogus").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&body)[0],
        json!({"code": 400.0, "name": "lab-01", "status": "Invalid field specified: bogus", "type": "Host"})
    );
}

async fn post_get(
    client: &reqwest::Client,
    server: &ic_mock::MockServer,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    json(
        request(client, server, Method::POST, path)
            .header("X-HTTP-Method-Override", "GET")
            .json(body)
            .send()
            .await
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn filters_select_objects() {
    let (server, client) = prod().await;
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/services",
        &json!({"filter": "service.state == 2 && service.state_type == 1 && host.name == \"db-prod-03\"", "attrs": ["name"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(names(&body).contains(&"db-prod-03!postgres-replication".to_owned()));

    // filter_vars keep values out of the expression.
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({
            "filter": "host.name in names",
            "filter_vars": {"names": ["db-prod-01", "db-prod-03", "missing"]},
            "attrs": ["name"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&body), ["db-prod-01", "db-prod-03"]);

    // Globs via match().
    let (_, body) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({"filter": "match(\"edge-ams-*\", host.name) && host.state == 1", "attrs": ["name"]}),
    )
    .await;
    assert!(results(&body).len() >= 5, "{body}");

    // Syntax errors are 404 like Icinga's GetFilterTargets.
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({"filter": "host.name ==", "verbose": true}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["status"], "No objects found.");
    assert!(body["diagnostic_information"].is_string());

    // Features the stand-in evaluator doesn't support are 400, never a
    // silently wrong answer.
    let (status, _) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({"filter": "regex(\"^db\", host.name)"}),
    )
    .await;
    assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn joins_add_the_host() {
    let (server, client) = prod().await;
    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/services/db-prod-03!postgres-replication?attrs=name&joins=host.name&joins=host.state&joins=host.address",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = &results(&body)[0];
    assert_eq!(entry["joins"]["host"]["name"], "db-prod-03");
    assert_eq!(entry["joins"]["host"]["state"], json!(0.0));
    assert!(entry["joins"]["host"]["address"].is_string());

    // `joins=host` (whole object).
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/db-prod-03!postgres-replication?attrs=name&joins=host",
    )
    .await;
    assert_eq!(results(&body)[0]["joins"]["host"]["__name"], "db-prod-03");
}

#[tokio::test]
async fn meta_used_by_lists_referencing_objects() {
    let (server, client) = lab().await;
    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/lab-01?attrs=name&meta=used_by",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let used_by = results(&body)[0]["meta"]["used_by"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        used_by
            .iter()
            .any(|r| r["type"] == "Service" && r["name"] == "lab-01!ssh"),
        "{used_by:?}"
    );
}

#[tokio::test]
async fn every_supported_type_answers() {
    let (server, client) = prod().await;
    for (plural, minimum) in [
        ("hosts", 140),
        ("services", 500),
        ("hostgroups", 3),
        ("servicegroups", 3),
        ("comments", 2),
        ("downtimes", 3),
        ("dependencies", 1),
        ("endpoints", 2),
        ("zones", 2),
        ("users", 1),
        ("checkcommands", 3),
        ("apiusers", 0),
    ] {
        let (status, body) = get(
            &client,
            &server,
            &format!("/v1/objects/{plural}?attrs=name"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{plural}: {body}");
        assert!(
            results(&body).len() >= minimum,
            "{plural}: {}",
            results(&body).len()
        );
    }
}

#[tokio::test]
async fn state_matches_the_design_scenario() {
    let (server, client) = prod().await;
    // Acknowledged TLS problem.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/web-edge-02!http-tls?attrs=acknowledgement&attrs=handled&attrs=state",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["acknowledgement"], json!(1.0));
    assert_eq!(attrs["handled"], json!(true));
    // Problem in downtime.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/cache-02!redis-memory?attrs=downtime_depth&attrs=handled",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["downtime_depth"], json!(1.0));
    assert_eq!(attrs["handled"], json!(true));
    // Soft state.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/k8s-node-04!kubelet?attrs=state_type&attrs=check_attempt&attrs=max_check_attempts",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state_type"], json!(0.0));
    assert_eq!(attrs["check_attempt"], json!(2.0));
    assert_eq!(attrs["max_check_attempts"], json!(3.0));
    // Unreachable hosts behind the AMS uplink.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/edge-ams-09?attrs=state&attrs=last_reachable",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state"], json!(1.0));
    assert_eq!(attrs["last_reachable"], json!(false));
}

#[tokio::test]
async fn comments_and_downtimes_reference_their_objects() {
    let (server, client) = prod().await;
    let (_, body) = post_get(
        &client,
        &server,
        "/v1/objects/comments",
        &json!({"filter": "comment.entry_type == 4", "attrs": ["host_name", "service_name", "author", "entry_type"]}),
    )
    .await;
    let acks = results(&body);
    assert!(
        acks.iter().any(|c| c["attrs"]["host_name"] == "web-edge-02"
            && c["attrs"]["service_name"] == "http-tls"),
        "{body}"
    );
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/downtimes?attrs=host_name&attrs=fixed&attrs=trigger_time&attrs=config_owner",
    )
    .await;
    let downtimes = results(&body);
    assert!(
        downtimes
            .iter()
            .any(|d| d["attrs"]["host_name"] == "edge-fra-04")
    );
    assert!(
        downtimes
            .iter()
            .any(|d| !d["attrs"]["config_owner"].as_str().unwrap().is_empty())
    );
}

#[tokio::test]
async fn objects_query_by_post_body() {
    let (server, client) = lab().await;
    // A POST without override is not an object query.
    let (status, _) = post(&client, &server, "/v1/objects/hosts", &json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
