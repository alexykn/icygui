//! `FilterUtility::GetFilterTargets`: which objects a query or action
//! applies to (names, plural names, `type` + `filter` + `filter_vars`).

use serde_json::Value as Json;

use super::params::{Params, to_icinga_string};
use crate::auth::Principal;
use crate::filter;
use crate::model::attrs::{EMPTY_TYPES, ObjectScope, ObjectValues};
use crate::model::{ObjKind, ObjRef, World};

/// Why targets could not be determined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetError {
    /// `MissingPermissionError` → 403 with this status.
    Forbidden(String),
    /// Any other exception → 404 "No objects found." (with diagnostics),
    /// filters that fail (or don't compile) for an object included.
    NotFound(String),
}

/// `filter_vars`: a dictionary, or nothing.
///
/// # Errors
/// Icinga's conversion error for anything else (a 404 for the request).
pub(crate) fn filter_vars(
    params: &Params,
) -> Result<Option<&serde_json::Map<String, Json>>, String> {
    match params.get("filter_vars") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(map)) => Ok(Some(map)),
        Some(other) => Err(format!(
            "Error: Cannot convert value of type '{}' to an object.",
            super::params::icinga_type_name(other)
        )),
    }
}

/// Whether Icinga knows a type name (`Type::GetByName` + config object).
pub(crate) fn is_config_type(name: &str) -> bool {
    ObjKind::from_type_name(name).is_some() || EMPTY_TYPES.iter().any(|(_, t)| *t == name)
}

/// The target objects of a query or action. `types` must be sorted by type
/// name (Icinga iterates a `std::set`). Types the mock has no objects of
/// are passed as `extra_types` so `type` validation still accepts them.
pub(crate) fn filter_targets(
    world: &World,
    types: &[ObjKind],
    extra_types: &[&str],
    permission: &str,
    params: &Params,
    user: &Principal,
    enforce_filter_permission: bool,
) -> Result<Vec<ObjRef>, TargetError> {
    if !user.has_permission(permission) {
        return Err(TargetError::Forbidden(format!(
            "Missing permission: {}",
            permission.to_lowercase()
        )));
    }
    let mut result = Vec::new();
    for kind in types {
        let attr = kind.type_name().to_lowercase();
        if params.contains(&attr) {
            let name = params.last_string(&attr);
            if !world.exists(*kind, &name) {
                return Err(TargetError::NotFound(
                    "Error: Object does not exist.".into(),
                ));
            }
            result.push(ObjRef { kind: *kind, name });
        }
        let plural = kind.plural();
        if let Some(value) = params.get(plural) {
            let names = match value {
                Json::Array(names) => names.clone(),
                Json::Null => Vec::new(),
                other => {
                    return Err(TargetError::NotFound(format!(
                        "Error: Cannot convert value of type '{}' to an object.",
                        super::params::icinga_type_name(other)
                    )));
                }
            };
            for name in names {
                let name = to_icinga_string(&name);
                if !world.exists(*kind, &name) {
                    return Err(TargetError::NotFound(
                        "Error: Object does not exist.".into(),
                    ));
                }
                result.push(ObjRef { kind: *kind, name });
            }
        }
    }
    if !(params.contains("filter") || result.is_empty()) {
        return Ok(result);
    }
    if !params.contains("type") {
        return Err(TargetError::NotFound(
            "Error: Type must be specified when using a filter.".into(),
        ));
    }
    let type_name = params.last_string("type");
    if !is_config_type(&type_name) {
        return Err(TargetError::NotFound(
            "Error: Invalid type specified.".into(),
        ));
    }
    let kind = types.iter().copied().find(|k| k.type_name() == type_name);
    if kind.is_none() && !extra_types.contains(&type_name.as_str()) {
        return Err(TargetError::NotFound(
            "Error: Invalid type specified for this query.".into(),
        ));
    }
    if params.contains("filter") {
        if enforce_filter_permission && !user.has_permission("filter-expression") {
            return Err(TargetError::Forbidden(
                "Missing permission: filter-expression".into(),
            ));
        }
        result.extend(filtered(world, kind, params)?);
    } else if let Some(kind) = kind {
        result.extend(
            world
                .object_names(kind)
                .into_iter()
                .map(|name| ObjRef { kind, name }),
        );
    }
    Ok(result)
}

/// The objects of type `kind` (`None`: a type the mock has no objects of)
/// that `filter` selects.
fn filtered(
    world: &World,
    kind: Option<ObjKind>,
    params: &Params,
) -> Result<Vec<ObjRef>, TargetError> {
    // Compiled first, then `filter_vars` are read, as in Icinga. A filter
    // that doesn't compile fails at the first object it is evaluated for,
    // like Icinga's `ThrowExpression`.
    let compiled = filter::compile(&params.last_string("filter"), world.filter_node())
        .with_vars(filter_vars(params).map_err(TargetError::NotFound)?);
    let Some(kind) = kind else {
        return Ok(Vec::new());
    };
    if let Some(names) = compiled.targets(kind) {
        // Icinga's targeted lookup: no evaluation, the filter's order,
        // duplicates kept, names without an object left out.
        return Ok(names
            .into_iter()
            .filter(|name| world.exists(kind, name))
            .map(|name| ObjRef { kind, name })
            .collect());
    }
    let values = ObjectValues::default();
    let mut result = Vec::new();
    for name in world.object_names(kind) {
        let object = ObjRef { kind, name };
        let scope = ObjectScope {
            world,
            object: object.clone(),
            values: &values,
        };
        match compiled.matches(&scope) {
            Ok(true) => result.push(object),
            Ok(false) => {}
            Err(error) => return Err(TargetError::NotFound(error.to_string())),
        }
    }
    Ok(result)
}
