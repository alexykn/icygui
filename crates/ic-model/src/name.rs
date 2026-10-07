//! Object identities: host names, service keys and the `host!service`
//! full names Icinga 2 uses in the API.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The name of a host (`Host.name`). Cheap to clone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostName(Arc<str>);

impl HostName {
    /// Wraps a host name.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self(Arc::from(name))
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for HostName {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// A service is identified by its host and its short name. Icinga's full
/// name for it is `host!service`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServiceKey {
    /// The host the service belongs to.
    pub host: HostName,
    /// The service's short name (`Service.name`).
    pub name: Arc<str>,
}

impl ServiceKey {
    /// Builds a key from host and service names.
    #[must_use]
    pub fn new(host: &str, name: &str) -> Self {
        Self {
            host: HostName::new(host),
            name: Arc::from(name),
        }
    }

    /// Parses Icinga's full name `host!service`. The service part may itself
    /// contain `!`, so only the first one separates host and service.
    #[must_use]
    pub fn parse(full_name: &str) -> Option<Self> {
        let (host, name) = full_name.split_once('!')?;
        (!host.is_empty() && !name.is_empty()).then(|| Self::new(host, name))
    }

    /// The full name `host!service`.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}!{}", self.host, self.name)
    }
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}!{}", self.host, self.name)
    }
}

/// A host or a service: anything that has a check and a state
/// (Icinga's "checkable").
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ObjectKey {
    /// A host.
    Host {
        /// Host name.
        name: HostName,
    },
    /// A service.
    Service {
        /// Host and service name.
        key: ServiceKey,
    },
}

impl ObjectKey {
    /// A host key.
    #[must_use]
    pub fn host(name: &str) -> Self {
        Self::Host {
            name: HostName::new(name),
        }
    }

    /// A service key.
    #[must_use]
    pub fn service(host: &str, service: &str) -> Self {
        Self::Service {
            key: ServiceKey::new(host, service),
        }
    }

    /// The host this object is or belongs to.
    #[must_use]
    pub fn host_name(&self) -> &HostName {
        match self {
            Self::Host { name } => name,
            Self::Service { key } => &key.host,
        }
    }

    /// The service key, if this is a service.
    #[must_use]
    pub fn as_service(&self) -> Option<&ServiceKey> {
        match self {
            Self::Host { .. } => None,
            Self::Service { key } => Some(key),
        }
    }

    /// Icinga's full name: `host` or `host!service`.
    #[must_use]
    pub fn full_name(&self) -> String {
        match self {
            Self::Host { name } => name.to_string(),
            Self::Service { key } => key.full_name(),
        }
    }

    /// The API object type: `Host` or `Service`.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Host { .. } => "Host",
            Self::Service { .. } => "Service",
        }
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host { name } => write!(f, "{name}"),
            Self::Service { key } => write!(f, "{key}"),
        }
    }
}

impl From<ServiceKey> for ObjectKey {
    fn from(key: ServiceKey) -> Self {
        Self::Service { key }
    }
}

impl From<HostName> for ObjectKey {
    fn from(name: HostName) -> Self {
        Self::Host { name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_service_names() {
        let key = ServiceKey::parse("db-prod-03!postgres-replication").unwrap();
        assert_eq!(key.host.as_str(), "db-prod-03");
        assert_eq!(&*key.name, "postgres-replication");
        assert_eq!(key.full_name(), "db-prod-03!postgres-replication");
    }

    #[test]
    fn only_the_first_bang_separates_host_and_service() {
        let key = ServiceKey::parse("web!http!8080").unwrap();
        assert_eq!(key.host.as_str(), "web");
        assert_eq!(&*key.name, "http!8080");
    }

    #[test]
    fn rejects_names_without_both_parts() {
        assert!(ServiceKey::parse("web").is_none());
        assert!(ServiceKey::parse("!http").is_none());
        assert!(ServiceKey::parse("web!").is_none());
    }

    #[test]
    fn object_keys_know_their_host() {
        assert_eq!(ObjectKey::host("a").host_name().as_str(), "a");
        assert_eq!(ObjectKey::service("a", "b").host_name().as_str(), "a");
        assert_eq!(ObjectKey::service("a", "b").full_name(), "a!b");
        assert_eq!(ObjectKey::service("a", "b").type_name(), "Service");
    }

    #[test]
    fn serializes_with_a_type_tag() {
        let json = serde_json::to_string(&ObjectKey::service("a", "b")).unwrap();
        assert_eq!(json, r#"{"type":"service","key":{"host":"a","name":"b"}}"#);
        let back: ObjectKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ObjectKey::service("a", "b"));
    }
}
