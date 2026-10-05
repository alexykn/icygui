//! `GET /v1/objects/<type>[/<name>]` (`ObjectQueryHandler`).
//!
//! Small answers are serialized at once. Big ones (all services of a large
//! installation) are streamed in batches, and the world is locked only
//! while a batch is serialized: the checker, the simulator, event streams
//! and other requests carry on meanwhile, as they do in Icinga.

use std::collections::BTreeSet;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderValue};
use http_body::Frame;
use http_body_util::BodyExt as _;
use serde_json::{Map, Value as Json};
use tokio::sync::mpsc;

use super::params::{Params, to_icinga_string};
use super::response::{Body, json_bytes, json_error, path_not_found};
use super::targets::{TargetError, filter_targets};
use crate::auth::Principal;
use crate::config::NumberFormat;
use crate::json::{RESULTS_END, RESULTS_START, push_result};
use crate::model::attrs::EMPTY_TYPES;
use crate::model::{ObjKind, ObjRef, World};
use crate::server::{Shared, blocking};

/// Objects serialized per lock of the world. Answers with more objects are
/// streamed batch by batch.
const BATCH: usize = 250;

/// Serialized batches buffered ahead of a slow reader.
const BATCHES_AHEAD: usize = 4;

/// The answer to a query: done, or a big one to stream with [`stream`].
pub(crate) enum Answer {
    Done(hyper::Response<Body>),
    Stream(Plan),
}

/// What a big answer serializes.
pub(crate) struct Plan {
    targets: Vec<ObjRef>,
    selection: Selection,
    user: Principal,
    /// `meta`: `used_by`, `location`.
    meta: (bool, bool),
}

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

/// Handles an object query (`segments` are the decoded path segments):
/// errors and small answers are done right away, big answers are left to
/// [`stream`].
pub(crate) fn handle(
    world: &World,
    user: &Principal,
    mut params: Params,
    segments: &[String],
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> Answer {
    match query(
        world,
        user,
        &mut params,
        segments,
        format,
        enforce_filter_permission,
    ) {
        Ok(plan) if plan.targets.len() > BATCH && !params.pretty() => Answer::Stream(plan),
        Ok(plan) => {
            let results = plan
                .targets
                .iter()
                .map(|target| serialize(world, &plan.user, target, &plan.selection, plan.meta));
            Answer::Done(json_bytes(
                200,
                crate::json::encode_results(results, format, params.pretty()),
            ))
        }
        Err(response) => Answer::Done(*response),
    }
}

/// Validates a query and finds its targets, step by step like Icinga's
/// `ObjectQueryHandler`.
///
/// # Errors
/// The error response Icinga sends.
fn query(
    world: &World,
    user: &Principal,
    params: &mut Params,
    segments: &[String],
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> Result<Plan, Box<hyper::Response<Body>>> {
    let fail = |code: u16, message: &str, params: &Params, diagnostic: Option<&str>| {
        Box::new(json_error(code, message, format, Some(params), diagnostic))
    };
    let plural = segments[2].to_lowercase();
    let query_type = if let Some(kind) = ObjKind::ALL.into_iter().find(|k| k.plural() == plural) {
        QueryType::Served(kind)
    } else if let Some((_, name)) = EMPTY_TYPES.iter().find(|(p, _)| *p == plural) {
        QueryType::Empty(name)
    } else {
        return Err(fail(400, "Invalid type specified.", params, None));
    };
    let (attrs, joins, metas) = match (
        array_param(params, "attrs"),
        array_param(params, "joins"),
        array_param(params, "meta"),
    ) {
        (Ok(attrs), Ok(joins), Ok(metas)) => (attrs, joins, metas),
        (Err(message), _, _) | (_, Err(message), _) | (_, _, Err(message)) => {
            return Err(fail(400, &message, params, None));
        }
    };
    let mut used_by = false;
    let mut location = false;
    for meta in metas.iter().flatten() {
        match meta.as_str() {
            "used_by" => used_by = true,
            "location" => location = true,
            other => {
                let message = format!("Invalid field specified for meta: {other}");
                return Err(fail(400, &message, params, None));
            }
        }
    }
    // Icinga reads it through a number: "0" is false, "yes" throws, and
    // the exception becomes the generic 404 of the HTTP handler.
    let Ok(all_joins) = params.flag("all_joins") else {
        return Err(Box::new(path_not_found(segments, format, Some(params))));
    };
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
            let message = format!("Missing permission: {}", permission.to_lowercase());
            return Err(fail(403, &message, params, None));
        }
        return Err(fail(
            404,
            "No objects found.",
            params,
            Some("Error: Object does not exist."),
        ));
    }
    let targets = match filter_targets(
        world,
        &kinds,
        &extra,
        &permission,
        params,
        user,
        enforce_filter_permission,
    ) {
        Ok(targets) => targets,
        Err(TargetError::Forbidden(message)) => return Err(fail(403, &message, params, None)),
        Err(TargetError::NotFound(diagnostic)) => {
            return Err(fail(404, "No objects found.", params, Some(&diagnostic)));
        }
    };
    let selection = Selection::new(attrs, joins, all_joins);
    if let Err(message) = check_selection(world, user, &targets, &selection) {
        return Err(fail(400, &message, params, None));
    }
    Ok(Plan {
        targets,
        selection,
        user: user.clone(),
        meta: (used_by, location),
    })
}

/// Streams a big answer: a task serializes it batch by batch, locking the
/// world only for each batch, and stops when the client goes away.
pub(crate) fn stream(
    shared: Arc<Shared>,
    plan: Plan,
    format: NumberFormat,
) -> hyper::Response<Body> {
    let (tx, rx) = mpsc::channel(BATCHES_AHEAD);
    tokio::spawn(produce(shared, plan, format, tx));
    let mut response = hyper::Response::new(
        StreamedBody {
            rx,
            finished: false,
        }
        .boxed(),
    );
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// A piece of a streamed answer.
enum Chunk {
    Data(Bytes),
    /// The end of the document.
    Last(Bytes),
}

async fn produce(shared: Arc<Shared>, plan: Plan, format: NumberFormat, tx: mpsc::Sender<Chunk>) {
    let mut out = RESULTS_START.to_vec();
    let mut first = true;
    for batch in plan.targets.chunks(BATCH) {
        blocking(|| {
            let world = shared.world();
            for target in batch {
                // Comments and downtimes may be gone by now.
                if !world.exists(target.kind, &target.name) {
                    continue;
                }
                let entry = serialize(&world, &plan.user, target, &plan.selection, plan.meta);
                push_result(&mut out, entry, format, first);
                first = false;
            }
        });
        if tx
            .send(Chunk::Data(Bytes::from(std::mem::take(&mut out))))
            .await
            .is_err()
        {
            return;
        }
        // Let the checker, the simulator and other requests have the world.
        tokio::task::yield_now().await;
    }
    let _ = tx.send(Chunk::Last(Bytes::from_static(RESULTS_END))).await;
}

/// The body of a streamed answer. If the producer stops before the end
/// (the server shuts down), the body fails, so the client sees a broken
/// connection instead of a truncated document.
struct StreamedBody {
    rx: mpsc::Receiver<Chunk>,
    finished: bool,
}

impl http_body::Body for StreamedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Chunk::Data(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Chunk::Last(bytes))) => {
                self.finished = true;
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(None) => Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the query was aborted",
            )))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
}

/// The `attrs` and `joins` a query asks for.
struct Selection {
    attrs: Option<Vec<String>>,
    joins: Option<Vec<String>>,
    /// The join names (`host` of `host.name`).
    join_prefixes: BTreeSet<String>,
    all_joins: bool,
}

impl Selection {
    fn new(attrs: Option<Vec<String>>, joins: Option<Vec<String>>, all_joins: bool) -> Self {
        let join_prefixes = joins
            .iter()
            .flatten()
            .map(|join| join.split('.').next().unwrap_or_default().to_owned())
            .collect();
        Self {
            attrs,
            joins,
            join_prefixes,
            all_joins,
        }
    }

    /// Whether the joined object of `prefix` is serialized whole.
    fn whole_join(&self, prefix: &str) -> bool {
        self.all_joins
            || self
                .joins
                .as_ref()
                .is_some_and(|joins| joins.iter().any(|j| j == prefix))
    }

    /// The attributes selected from the joined object of `prefix`.
    fn join_attrs<'s>(&'s self, prefix: &'s str) -> impl Iterator<Item = &'s str> + 's {
        self.joins
            .iter()
            .flatten()
            .filter_map(|join| join.split_once('.'))
            .filter(move |(join_prefix, _)| *join_prefix == prefix)
            .map(|(_, attr)| attr)
    }
}

/// Icinga 2.15 serializes every object before it answers, so an attribute
/// a type doesn't have fails the whole request with 400 `Invalid field
/// specified: <name>`. Like there, the check only applies to what actually
/// gets serialized: no targets, no error; a join selection only matters
/// for objects whose joined object exists and may be read.
fn check_selection(
    world: &World,
    user: &Principal,
    targets: &[ObjRef],
    selection: &Selection,
) -> Result<(), String> {
    let Some(first) = targets.first() else {
        return Ok(());
    };
    if let Some(names) = &selection.attrs {
        first.kind.check_fields(names.iter().map(String::as_str))?;
    }
    // Join selections that name unknown fields, in field order.
    let invalid: Vec<(&str, ObjKind, String)> = first
        .kind
        .navigation()
        .iter()
        .filter(|(prefix, _)| selection.all_joins || selection.join_prefixes.contains(*prefix))
        .filter(|(prefix, _)| !selection.whole_join(prefix))
        .filter_map(|(prefix, kind)| {
            kind.check_fields(selection.join_attrs(prefix))
                .err()
                .map(|message| (*prefix, *kind, message))
        })
        .collect();
    if invalid.is_empty() {
        return Ok(());
    }
    for target in targets {
        for (prefix, kind, message) in &invalid {
            if world.navigate(target.kind, &target.name, prefix).is_some()
                && user.has_permission(&format!("objects/query/{}", kind.type_name()))
            {
                return Err(message.clone());
            }
        }
    }
    Ok(())
}

/// One result entry: `name`, `type`, `attrs`, `joins` and `meta`.
fn serialize(
    world: &World,
    user: &Principal,
    target: &ObjRef,
    selection: &Selection,
    (used_by, location): (bool, bool),
) -> Json {
    let type_name = target.kind.type_name();
    // `check_selection` ran first, so this only happens if an object
    // vanished between the two (it can't while the world is locked).
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
    match world.object_attrs(target, selection.attrs.as_deref()) {
        Ok(map) => {
            entry.insert("attrs".into(), Json::Object(map));
        }
        Err(message) => return error_entry(message),
    }
    let mut joined = Map::new();
    for (prefix, target_kind) in target.kind.navigation() {
        if !selection.all_joins && !selection.join_prefixes.contains(*prefix) {
            continue;
        }
        let Some(other) = world.navigate(target.kind, &target.name, prefix) else {
            continue;
        };
        if !user.has_permission(&format!("objects/query/{}", target_kind.type_name())) {
            continue;
        }
        let attrs: Option<Vec<String>> = (!selection.whole_join(prefix))
            .then(|| selection.join_attrs(prefix).map(str::to_owned).collect());
        match world.object_attrs(&other, attrs.as_deref()) {
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
