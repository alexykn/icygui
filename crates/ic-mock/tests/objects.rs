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
    assert_eq!(attrs["state"], json!(2));
    assert_eq!(attrs["state_type"], json!(1));
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

    // Unknown attributes fail the whole request (Icinga 2.15 serializes
    // every object before it answers); hidden ones are left out.
    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/hosts?attrs=state&attrs=bogus&attrs=other",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": 400, "status": "Invalid field specified: bogus"})
    );
    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/lab-01?attrs=state_raw&attrs=state",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results(&body)[0]["attrs"], json!({"state": 0}));
    // The same for joined attributes, but only where a joined object is
    // serialized; `attrs: []` selects nothing.
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/services",
        &json!({"service": "lab-01!ssh", "attrs": ["state"], "joins": ["host.state", "host.bogus"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "Invalid field specified: bogus");
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/services",
        &json!({"service": "lab-01!ssh", "attrs": [], "joins": ["nothing.bogus"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(results(&body)[0]["attrs"], json!({}));
    assert_eq!(results(&body)[0]["joins"], json!({}));
    let (status, body) = get(&client, &server, "/v1/objects/comments?attrs=bogus").await;
    assert_eq!(status, StatusCode::OK, "no comments, nothing serialized");
    assert_eq!(body, json!({"results": []}));
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

    // The whole language of ic-filter: regular expressions, joined objects,
    // methods, filter_vars dictionaries.
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/services",
        &json!({
            "filter": "regex(\"^db-prod-0[13]$\", host.name) && service.name.contains(\"replication\") && service.host.name == host.name && service.state >= limits.critical",
            "filter_vars": {"limits": {"critical": 2}},
            "attrs": ["name"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["db-prod-03!postgres-replication"]);

    // An error for any object fails the whole request, like in Icinga.
    for filter in [
        "host.name == \"db-prod-03\" || nothing",
        "host.bogus",
        "host.name < 1",
        "x = 1",
    ] {
        let (status, body) = post_get(
            &client,
            &server,
            "/v1/objects/hosts",
            &json!({"filter": filter, "verbose": true}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{filter}: {body}");
        assert_eq!(body["status"], "No objects found.");
        assert!(
            body["diagnostic_information"]
                .as_str()
                .unwrap()
                .starts_with("Error: "),
            "{body}"
        );
    }

    // Empty filters match nothing.
    let (status, body) = post_get(
        &client,
        &server,
        "/v1/objects/hosts",
        &json!({"filter": " "}),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({"results": []})));
}

/// Actions take filters the same way.
#[tokio::test]
async fn actions_resolve_filters_like_queries() {
    let (server, client) = lab().await;
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/reschedule-check",
        &json!({"type": "Service", "filter": "service.name == check", "filter_vars": {"check": "ssh"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!results(&body).is_empty());
    for (filter, code) in [("host.name ==", 404), ("nothing", 404), ("false", 404)] {
        let (status, body) = post(
            &client,
            &server,
            "/v1/actions/reschedule-check",
            &json!({"type": "Service", "filter": filter}),
        )
        .await;
        assert_eq!(status.as_u16(), code, "{filter}: {body}");
        assert_eq!(body["status"], "No objects found.", "{filter}");
    }
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
    assert_eq!(entry["joins"]["host"]["state"], json!(0));
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
    assert_eq!(attrs["acknowledgement"], json!(1));
    assert_eq!(attrs["handled"], json!(true));
    // Problem in downtime.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/cache-02!redis-memory?attrs=downtime_depth&attrs=handled",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["downtime_depth"], json!(1));
    assert_eq!(attrs["handled"], json!(true));
    // Soft state.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/k8s-node-04!kubelet?attrs=state_type&attrs=check_attempt&attrs=max_check_attempts",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state_type"], json!(0));
    assert_eq!(attrs["check_attempt"], json!(2));
    assert_eq!(attrs["max_check_attempts"], json!(3));
    // Unreachable hosts behind the AMS uplink.
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/edge-ams-09?attrs=state&attrs=last_reachable",
    )
    .await;
    let attrs = &results(&body)[0]["attrs"];
    assert_eq!(attrs["state"], json!(1));
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

/// Name lists target objects for queries and actions without the
/// `filter-expression` permission; one unknown name fails the whole request
/// (recorded from Icinga 2.15.6).
#[tokio::test]
async fn name_lists_need_no_filter_permission() {
    let config = ic_mock::MockConfig {
        users: vec![ic_mock::MockUser::new(
            "icygui",
            "icygui-test",
            &["objects/query/*", "status/query", "events/*", "actions/*"],
        )],
        ..ic_mock::MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())
    };
    let (server, client) = common::start(config).await;
    // `GET` is sent like the client does: `POST` with the override header.
    let send = async |method: Method, path: &str, body: Value| {
        let mut request = client
            .post(format!("{}{path}", server.url()))
            .basic_auth("icygui", Some("icygui-test"))
            .header("Accept", "application/json")
            .json(&body);
        if method == Method::GET {
            request = request.header("X-HTTP-Method-Override", "GET");
        }
        json(request.send().await.unwrap()).await
    };

    let (status, body) = send(
        Method::GET,
        "/v1/objects/hosts",
        json!({"hosts": ["db-prod-03", "k8s-node-11"], "attrs": ["name"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["db-prod-03", "k8s-node-11"]);
    let (_, body) = send(
        Method::GET,
        "/v1/objects/services",
        json!({"services": ["db-prod-03!postgres-replication"], "attrs": ["state"]}),
    )
    .await;
    assert_eq!(names(&body), ["db-prod-03!postgres-replication"]);
    let (_, body) = send(
        Method::GET,
        "/v1/objects/services",
        json!({"service": "db-prod-03!postgres-replication", "attrs": ["state"]}),
    )
    .await;
    assert_eq!(results(&body).len(), 1);

    let comments: Vec<String> = server
        .control()
        .comments()
        .into_iter()
        .map(|c| c.name)
        .take(2)
        .collect();
    assert_eq!(comments.len(), 2);
    let (_, body) = send(
        Method::GET,
        "/v1/objects/comments",
        json!({"comments": comments, "attrs": ["author"]}),
    )
    .await;
    assert_eq!(results(&body).len(), 2);
    let downtime = server.control().downtimes()[0].name.clone();
    let (_, body) = send(
        Method::GET,
        "/v1/objects/downtimes",
        json!({"downtimes": [downtime], "attrs": ["author"]}),
    )
    .await;
    assert_eq!(results(&body).len(), 1);

    // One unknown name fails the whole request.
    let not_found = json!({"error": 404, "status": "No objects found."});
    for (path, body) in [
        (
            "/v1/objects/hosts",
            json!({"hosts": ["db-prod-03", "decommissioned-01"]}),
        ),
        (
            "/v1/objects/services",
            json!({"services": ["db-prod-03!postgres-replication", "db-prod-03!gone"]}),
        ),
        (
            "/v1/objects/comments",
            json!({"comments": [comments[0], "db-prod-03!no-such-comment"]}),
        ),
    ] {
        let (status, response) = send(Method::GET, path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(response, not_found, "{path}");
    }

    // Actions take the same lists.
    let (status, body) = send(
        Method::POST,
        "/v1/actions/add-comment",
        json!({"type": "Host", "hosts": ["db-prod-03", "k8s-node-11"], "author": "icygui", "comment": "drill"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let added: Vec<Value> = results(&body).iter().map(|r| r["name"].clone()).collect();
    assert_eq!(added.len(), 2);
    let (status, body) = send(
        Method::POST,
        "/v1/actions/remove-comment",
        json!({"comments": added}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(results(&body).len(), 2);
    let (status, body) = send(
        Method::POST,
        "/v1/actions/remove-comment",
        json!({"comments": added}),
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::NOT_FOUND, not_found.clone()),
        "gone now"
    );
    let now = server.control().now().as_unix_seconds();
    let (_, body) = send(
        Method::POST,
        "/v1/actions/schedule-downtime",
        json!({"type": "Service", "services": ["db-prod-03!load"], "author": "icygui", "comment": "x", "start_time": now, "end_time": now + 600.0}),
    )
    .await;
    let scheduled = results(&body)[0]["name"].clone();
    let (status, body) = send(
        Method::POST,
        "/v1/actions/remove-downtime",
        json!({"downtimes": [scheduled]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Filters need the permission this user lacks.
    let (status, body) = send(
        Method::GET,
        "/v1/objects/hosts",
        json!({"filter": "host.name == \"db-prod-03\""}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"error": 403, "status": "Missing permission: filter-expression"})
    );
}

/// `next_update` (`Checkable::GetNextUpdate`): active checks at
/// `next_check` + interval + 2 × latency, passive ones at the last result
/// (or the program start) + 2 × interval.
#[tokio::test]
async fn next_update_follows_icinga() {
    let (server, client) = lab().await;
    let attrs = async |object: &str| -> Value {
        let (_, body) = get(
            &client,
            &server,
            &format!(
                "/v1/objects/services/{object}?attrs=next_update&attrs=next_check&attrs=check_interval&attrs=last_check_result"
            ),
        )
        .await;
        results(&body)[0]["attrs"].clone()
    };
    let close = |a: f64, b: f64| (a - b).abs() < 1e-3;

    let active = attrs("lab-01!ssh").await;
    let cr = &active["last_check_result"];
    let latency = cr["execution_end"].as_f64().unwrap() - cr["schedule_start"].as_f64().unwrap();
    let expected = active["next_check"].as_f64().unwrap()
        + active["check_interval"].as_f64().unwrap()
        + 2.0 * latency;
    assert!(
        close(active["next_update"].as_f64().unwrap(), expected),
        "{active}"
    );

    // Passive and pending: from the program start.
    let (_, status) = get(&client, &server, "/v1/status/IcingaApplication").await;
    let program_start = results(&status)[0]["status"]["icingaapplication"]["app"]["program_start"]
        .as_f64()
        .unwrap();
    let pending = attrs("lab-02!ping4").await;
    assert!(pending["last_check_result"].is_null());
    assert!(
        close(
            pending["next_update"].as_f64().unwrap(),
            program_start + 2.0 * 60.0
        ),
        "{pending}"
    );

    // Passive with a result: from that result.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/process-check-result",
        &json!({"type": "Service", "service": "lab-02!ping4", "exit_status": 0, "plugin_output": "PING OK"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let passive = attrs("lab-02!ping4").await;
    let end = passive["last_check_result"]["execution_end"]
        .as_f64()
        .unwrap();
    assert!(
        close(passive["next_update"].as_f64().unwrap(), end + 2.0 * 60.0),
        "{passive}"
    );
}

/// Big answers are streamed in batches (chunked, the world unlocked in
/// between); they read exactly like answers built at once.
#[tokio::test]
async fn big_answers_are_streamed_in_batches() {
    let (server, client) = prod().await;
    let response = request(&client, &server, Method::GET, "/v1/objects/services")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("content-length").is_none(),
        "streamed"
    );
    let streamed: Value = response.json().await.unwrap();
    // `pretty` answers are built at once.
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/objects/services?pretty=1",
    )
    .send()
    .await
    .unwrap();
    assert!(response.headers().get("content-length").is_some());
    let whole: Value = response.json().await.unwrap();
    assert!(results(&streamed).len() > 1_000);
    assert_eq!(names(&streamed), names(&whole));
    for (a, b) in results(&streamed).iter().zip(results(&whole)) {
        // Only the clock-dependent attributes may differ between the two.
        let strip = |entry: &Value| {
            let mut entry = entry.clone();
            for attr in ["next_update", "severity", "downtime_depth", "handled"] {
                entry["attrs"].as_object_mut().unwrap().remove(attr);
            }
            entry
        };
        assert_eq!(strip(a), strip(b));
    }
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/objects/hosts/db-prod-03",
    )
    .send()
    .await
    .unwrap();
    assert!(response.headers().get("content-length").is_some(), "small");
}
