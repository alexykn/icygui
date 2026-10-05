//! `GET /v1/objects/<type>[/<name>]` (`ObjectQueryHandler`).

use std::collections::BTreeSet;

use serde_json::{Map, Value as Json};

use super::params::{Params, to_icinga_string};
use super::response::{Body, json_bytes, json_error};
use super::targets::{TargetError, filter_targets};
use crate::auth::Principal;
use crate::config::NumberFormat;
use crate::model::attrs::EMPTY_TYPES;
use crate::model::{ObjKind, ObjRef, World};

/// Resolved `<type>` of the URL.
enum QueryType {
    Served(ObjKind),
    /// A type Icinga knows that the mock has no objects of.
    Empty(&'static str),
}

/// Reads an optional array parameter.
fn array_param(params: &Params, key: &str) -> Result<Option<Vec<String>>, String> {
    match params.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Array(items)) => Ok(Some(items.iter().map(to_icinga_string).collect())),
        Some(_) => Err(format!(
            "Invalid type for '{key}' attribute specified. Array type is required."
        )),
    }
}

/// Handles an object query. `segments` are the decoded path segments.
#[expect(
    clippy::too_many_lines,
    reason = "mirrors Icinga's ObjectQueryHandler step by step"
)]
pub(crate) fn handle(
    world: &World,
    user: &Principal,
    mut params: Params,
    segments: &[String],
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> hyper::Response<Body> {
    let plural = segments[2].to_lowercase();
    let query_type = if let Some(kind) = ObjKind::ALL.into_iter().find(|k| k.plural() == plural) {
        QueryType::Served(kind)
    } else if let Some((_, name)) = EMPTY_TYPES.iter().find(|(p, _)| *p == plural) {
        QueryType::Empty(name)
    } else {
        return json_error(400, "Invalid type specified.", format, Some(&params), None);
    };
    let (attrs, joins, metas) = match (
        array_param(&params, "attrs"),
        array_param(&params, "joins"),
        array_param(&params, "meta"),
    ) {
        (Ok(attrs), Ok(joins), Ok(metas)) => (attrs, joins, metas),
        (Err(message), _, _) | (_, Err(message), _) | (_, _, Err(message)) => {
            return json_error(400, &message, format, Some(&params), None);
        }
    };
    let mut used_by = false;
    let mut location = false;
    for meta in metas.iter().flatten() {
        match meta.as_str() {
            "used_by" => used_by = true,
            "location" => location = true,
            other => {
                return json_error(
                    400,
                    &format!("Invalid field specified for meta: {other}"),
                    format,
                    Some(&params),
                    None,
                );
            }
        }
    }
    let all_joins = params.last_bool("all_joins");
    let type_name = match &query_type {
        QueryType::Served(kind) => kind.type_name(),
        QueryType::Empty(name) => name,
    };
    params.set("type", Json::String(type_name.to_owned()));
    if let Some(name) = segments.get(3) {
        params.set(&type_name.to_lowercase(), Json::String(name.clone()));
    }

    let (kinds, extra): (Vec<ObjKind>, Vec<&str>) = match &query_type {
        QueryType::Served(kind) => (vec![*kind], Vec::new()),
        QueryType::Empty(name) => (Vec::new(), vec![*name]),
    };
    let permission = format!("objects/query/{type_name}");
    if let QueryType::Empty(_) = query_type
        && params.contains(&type_name.to_lowercase())
    {
        // A name was given, but there are no such objects.
        if !user.has_permission(&permission) {
            return json_error(
                403,
                &format!("Missing permission: {}", permission.to_lowercase()),
                format,
                Some(&params),
                None,
            );
        }
        return json_error(
            404,
            "No objects found.",
            format,
            Some(&params),
            Some("Error: Object does not exist."),
        );
    }
    let targets = match filter_targets(
        world,
        &kinds,
        &extra,
        &permission,
        &params,
        user,
        enforce_filter_permission,
    ) {
        Ok(targets) => targets,
        Err(TargetError::Forbidden(message)) => {
            return json_error(403, &message, format, Some(&params), None);
        }
        Err(TargetError::NotFound(diagnostic)) => {
            return json_error(
                404,
                "No objects found.",
                format,
                Some(&params),
                Some(&diagnostic),
            );
        }
        Err(TargetError::Unsupported(message)) => {
            return json_error(400, &message, format, Some(&params), None);
        }
    };

    let join_prefixes: BTreeSet<String> = joins
        .iter()
        .flatten()
        .map(|join| join.split('.').next().unwrap_or_default().to_owned())
        .collect();
    let results = targets.iter().map(|target| {
        serialize(
            world,
            user,
            target,
            attrs.as_deref(),
            joins.as_deref(),
            &join_prefixes,
            all_joins,
            (used_by, location),
        )
    });
    json_bytes(
        200,
        crate::json::encode_results(results, format, params.pretty()),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors ObjectQueryHandler's per-object generator"
)]
fn serialize(
    world: &World,
    user: &Principal,
    target: &ObjRef,
    attrs: Option<&[String]>,
    joins: Option<&[String]>,
    join_prefixes: &BTreeSet<String>,
    all_joins: bool,
    (used_by, location): (bool, bool),
) -> Json {
    let type_name = target.kind.type_name();
    let error_entry = |message: String| {
        let mut entry = Map::new();
        entry.insert("code".into(), crate::json::int(400));
        entry.insert("name".into(), Json::String(target.name.clone()));
        entry.insert("status".into(), Json::String(message));
        entry.insert("type".into(), Json::String(type_name.to_owned()));
        Json::Object(entry)
    };
    let mut entry = Map::new();
    entry.insert("name".into(), Json::String(target.name.clone()));
    entry.insert("type".into(), Json::String(type_name.to_owned()));
    let mut meta = Map::new();
    if used_by {
        meta.insert("used_by".into(), Json::Array(used_by_of(world, target)));
    }
    if location && let Ok(Some(source)) = world.attr(target, "source_location") {
        meta.insert("location".into(), source);
    }
    entry.insert("meta".into(), Json::Object(meta));
    match world.object_attrs(target, attrs) {
        Ok(map) => {
            entry.insert("attrs".into(), Json::Object(map));
        }
        Err(message) => return error_entry(message),
    }
    let mut joined = Map::new();
    for (prefix, target_kind) in target.kind.navigation() {
        if !all_joins && !join_prefixes.contains(*prefix) {
            continue;
        }
        let Some(other) = world.navigate(target.kind, &target.name, prefix) else {
            continue;
        };
        if !user.has_permission(&format!("objects/query/{}", target_kind.type_name())) {
            continue;
        }
        let whole = all_joins || joins.is_some_and(|joins| joins.iter().any(|j| j == prefix));
        let selection: Option<Vec<String>> = if whole {
            None
        } else {
            Some(
                joins
                    .iter()
                    .flat_map(|joins| joins.iter())
                    .filter_map(|join| join.split_once('.'))
                    .filter(|(join_prefix, _)| join_prefix == prefix)
                    .map(|(_, attr)| attr.to_owned())
                    .collect(),
            )
        };
        match world.object_attrs(&other, selection.as_deref()) {
            Ok(map) => {
                joined.insert((*prefix).to_owned(), Json::Object(map));
            }
            Err(message) => return error_entry(message),
        }
    }
    entry.insert("joins".into(), Json::Object(joined));
    Json::Object(entry)
}

/// `meta=used_by`: the objects that reference `target`.
fn used_by_of(world: &World, target: &ObjRef) -> Vec<Json> {
    let mut refs: Vec<(ObjKind, String)> = Vec::new();
    match target.kind {
        ObjKind::Host => {
            refs.extend(
                world
                    .services_of(&target.name)
                    .map(|s| (ObjKind::Service, s.full_name())),
            );
            refs.extend(
                world
                    .comments_of(&target.name)
                    .into_iter()
                    .map(|c| (ObjKind::Comment, c)),
            );
            refs.extend(
                world
                    .downtimes_of(&target.name)
                    .into_iter()
                    .map(|d| (ObjKind::Downtime, d)),
            );
            refs.extend(
                world
                    .dependencies
                    .values()
                    .filter(|d| {
                        d.child.host_name().as_str() == target.name
                            || d.parent.host_name().as_str() == target.name
                    })
                    .map(|d| (ObjKind::Dependency, d.name.clone())),
            );
        }
        ObjKind::Service => {
            refs.extend(
                world
                    .comments_of(&target.name)
                    .into_iter()
                    .map(|c| (ObjKind::Comment, c)),
            );
            refs.extend(
                world
                    .downtimes_of(&target.name)
                    .into_iter()
                    .map(|d| (ObjKind::Downtime, d)),
            );
            refs.extend(
                world
                    .dependencies
                    .values()
                    .filter(|d| {
                        d.child.full_name() == target.name || d.parent.full_name() == target.name
                    })
                    .map(|d| (ObjKind::Dependency, d.name.clone())),
            );
        }
        ObjKind::HostGroup => refs.extend(
            world
                .hosts
                .values()
                .filter(|h| h.groups.contains(&target.name))
                .map(|h| (ObjKind::Host, h.full_name())),
        ),
        ObjKind::ServiceGroup => refs.extend(
            world
                .all_services()
                .filter(|s| s.groups.contains(&target.name))
                .map(|s| (ObjKind::Service, s.full_name())),
        ),
        ObjKind::CheckCommand => refs.extend(
            world
                .all_checkables()
                .filter(|c| c.check_command == target.name)
                .map(|c| (c_kind(c), c.full_name())),
        ),
        ObjKind::EventCommand => refs.extend(
            world
                .all_checkables()
                .filter(|c| c.event_command == target.name)
                .map(|c| (c_kind(c), c.full_name())),
        ),
        ObjKind::Endpoint => refs.extend(
            world
                .zones
                .values()
                .filter(|z| z.endpoints.contains(&target.name))
                .map(|z| (ObjKind::Zone, z.name.clone())),
        ),
        _ => {}
    }
    refs.into_iter()
        .map(|(kind, name)| {
            let mut map = Map::new();
            map.insert("name".into(), Json::String(name));
            map.insert("type".into(), Json::String(kind.type_name().to_owned()));
            Json::Object(map)
        })
        .collect()
}

fn c_kind(checkable: &crate::model::Checkable) -> ObjKind {
    if checkable.is_service() {
        ObjKind::Service
    } else {
        ObjKind::Host
    }
}
