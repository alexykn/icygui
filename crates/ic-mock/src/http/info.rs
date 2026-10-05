//! `GET /v1` (`InfoHandler`) and `GET /v1/status` (`StatusHandler`).

use std::borrow::Cow;

use serde_json::{Map, Value as Json};

use super::params::Params;
use super::response::{Body, html, json, json_error};
use crate::auth::Principal;
use crate::config::NumberFormat;
use crate::filter::{self, Frame, Item, Value};
use crate::model::stats::STATUS_FUNCTIONS;
use crate::model::{ObjRef, World};

/// `GET /v1`: JSON only when `Accept` is exactly `application/json`.
pub(crate) fn info(
    world: &World,
    user: &Principal,
    params: &Params,
    accept_json: bool,
    format: NumberFormat,
) -> hyper::Response<Body> {
    let version = &world.app.version;
    if accept_json {
        let mut result = Map::new();
        result.insert("user".into(), Json::String(user.name.clone()));
        result.insert(
            "permissions".into(),
            Json::Array(user.permissions.iter().cloned().map(Json::String).collect()),
        );
        result.insert("version".into(), Json::String(version.clone()));
        result.insert(
            "info".into(),
            Json::String(
                "More information about API requests is available in the documentation at https://icinga.com/docs/icinga2/latest/"
                    .into(),
            ),
        );
        let mut body = Map::new();
        body.insert("results".into(), Json::Array(vec![Json::Object(result)]));
        return json(200, Json::Object(body), format, params.pretty());
    }
    let mut body = format!(
        "<html><head><title>Icinga 2</title></head><h1>Hello from Icinga 2 (Version: {version})!</h1><p>You are authenticated as <b>{}</b>. ",
        user.name
    );
    if user.permissions.is_empty() {
        body.push_str("Your user does not have any permissions.</p>");
    } else {
        body.push_str("Your user has the following permissions:</p> <ul>");
        for permission in &user.permissions {
            body.push_str("<li>");
            body.push_str(permission);
            body.push_str("</li>");
        }
        body.push_str("</ul>");
    }
    body.push_str(r#"<p>More information about API requests is available in the <a href="https://icinga.com/docs/icinga2/latest/" target="_blank">documentation</a>.</p></html>"#);
    html(200, body)
}

/// The frame of a status filter. Status entries are dictionaries, so
/// Icinga names the variable after their type: `dictionary` (and `obj`).
struct StatusFrame<'a> {
    entry: &'a Json,
    now: f64,
}

impl Frame for StatusFrame<'_> {
    fn variable(&self, name: &str) -> Option<Item<'_>> {
        matches!(name, "dictionary" | "obj").then(|| Item::Json(Cow::Borrowed(self.entry)))
    }

    fn field(&self, _object: &ObjRef, _name: &str) -> Result<Item<'_>, String> {
        Ok(Item::null())
    }

    fn object_value(&self, _object: &ObjRef) -> Value {
        Value::Null
    }

    fn now(&self) -> f64 {
        self.now
    }
}

/// `GET /v1/status[/<name>]`.
pub(crate) fn status(
    world: &World,
    user: &Principal,
    mut params: Params,
    name: Option<&str>,
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> hyper::Response<Body> {
    if !user.has_permission("status/query") {
        return json_error(
            403,
            "Missing permission: status/query",
            format,
            Some(&params),
            None,
        );
    }
    params.set("type", Json::String("Status".into()));
    if let Some(name) = name {
        params.set("status", Json::String(name.to_owned()));
    }
    let not_found = |params: &Params, diagnostic: &str| {
        json_error(
            404,
            "No objects found.",
            format,
            Some(params),
            Some(diagnostic),
        )
    };
    let mut names: Vec<String> = Vec::new();
    if params.contains("status") {
        names.push(params.last_string("status"));
    }
    if let Some(Json::Array(list)) = params.get("statuses") {
        names.extend(list.iter().map(super::params::to_icinga_string));
    }
    let mut results = Vec::new();
    for name in &names {
        match world.status_entry(name) {
            Some(entry) => results.push(entry),
            None => return not_found(&params, "Error: Invalid status function name."),
        }
    }
    if params.contains("filter") || results.is_empty() {
        let compiled = if params.contains("filter") {
            if enforce_filter_permission && !user.has_permission("filter-expression") {
                return json_error(
                    403,
                    "Missing permission: filter-expression",
                    format,
                    Some(&params),
                    None,
                );
            }
            // Compiled first, then `filter_vars` are read; a filter that
            // doesn't compile fails at the first entry.
            let compiled = filter::compile(&params.last_string("filter"), world.filter_node());
            match super::targets::filter_vars(&params) {
                Ok(vars) => Some(compiled.with_vars(vars)),
                Err(diagnostic) => return not_found(&params, &diagnostic),
            }
        } else {
            None
        };
        for name in STATUS_FUNCTIONS {
            let Some(entry) = world.status_entry(name) else {
                continue;
            };
            if let Some(compiled) = &compiled {
                let frame = StatusFrame {
                    entry: &entry,
                    now: world.now(),
                };
                match compiled.matches(&frame) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => return not_found(&params, &error.to_string()),
                }
            }
            results.push(entry);
        }
    }
    let mut body = Map::new();
    body.insert("results".into(), Json::Array(results));
    json(200, Json::Object(body), format, params.pretty())
}
