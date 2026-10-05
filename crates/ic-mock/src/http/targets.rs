//! `FilterUtility::GetFilterTargets`: which objects a query or action
//! applies to (names, plural names, `type` + `filter` + `filter_vars`).

use serde_json::Value as Json;

use super::params::{Params, to_icinga_string};
use crate::auth::Principal;
use crate::filter::{self, FilterError};
use crate::model::attrs::{EMPTY_TYPES, ObjectScope};
use crate::model::{ObjKind, ObjRef, World};

/// Why targets could not be determined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetError {
    /// `MissingPermissionError` → 403 with this status.
    Forbidden(String),
    /// Any other exception → 404 "No objects found." (with diagnostics).
    NotFound(String),
    /// A valid filter the mock can't evaluate → 400.
    Unsupported(String),
}

/// Whether Icinga knows a type name (`Type::GetByName` + config object).
pub(crate) fn is_config_type(name: &str) -> bool {
    ObjKind::from_type_name(name).is_some() || EMPTY_TYPES.iter().any(|(_, t)| *t == name)
}

/// The target objects of a query or action. `types` must be sorted by type
/// name (Icinga iterates a `std::set`). Types the mock has no objects of
/// are passed as `extra_types` so `type` validation still accepts them.
#[expect(
    clippy::too_many_lines,
    reason = "mirrors Icinga's FilterUtility::GetFilterTargets step by step"
)]
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
    let objects = kind.map(|kind| (kind, world.object_names(kind)));
    if params.contains("filter") {
        if enforce_filter_permission && !user.has_permission("filter-expression") {
            return Err(TargetError::Forbidden(
                "Missing permission: filter-expression".into(),
            ));
        }
        let source = params.last_string("filter");
        let vars = match params.get("filter_vars") {
            None | Some(Json::Null) => None,
            Some(Json::Object(map)) => Some(map),
            Some(other) => {
                return Err(TargetError::NotFound(format!(
                    "Error: Cannot convert value of type '{}' to an object.",
                    super::params::icinga_type_name(other)
                )));
            }
        };
        let compiled = filter::compile(&source, vars).map_err(|error| match error {
            FilterError::Syntax(_) => TargetError::NotFound(format!("{error}")),
            FilterError::Unsupported(_) => TargetError::Unsupported(format!("{error}")),
        })?;
        if let Some((kind, names)) = objects {
            for name in names {
                let object = ObjRef { kind, name };
                let scope = ObjectScope {
                    world,
                    object: object.clone(),
                };
                match compiled.matches(&scope) {
                    Ok(true) => result.push(object),
                    Ok(false) => {}
                    Err(error) => return Err(TargetError::NotFound(format!("{error}"))),
                }
            }
        }
    } else if let Some((kind, names)) = objects {
        result.extend(names.into_iter().map(|name| ObjRef { kind, name }));
    }
    Ok(result)
}
