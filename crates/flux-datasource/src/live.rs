//! Pure data contracts for async, paged live datasources.
//!
//! These shapes describe rows returned directly from an external system of record. They carry
//! names and weak references only; authentication, connections, and runtime handles remain owned by
//! the host-side backend.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One model-visible row returned by a live datasource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// Stable identifier within the row's entity.
    pub id: String,
    /// Short human-facing title.
    #[serde(default)]
    pub title: String,
    /// Compact one-line description used by list results.
    #[serde(default)]
    pub summary: String,
    /// Optional weak locator for a later lookup or human navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Reference>,
}

/// A weak row locator. It is plain data, never a credential, connection, session, or live handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reference {
    /// Re-enter the backend using an entity name and stable id.
    Entity {
        /// Entity understood by the backend.
        entity: String,
        /// Stable id within that entity.
        id: String,
    },
    /// Human-navigation URL. Backends must not place credentials or presigned secrets in it.
    Url {
        /// Public or otherwise non-secret navigation URL.
        url: String,
    },
}

/// One page of rows plus an opaque continuation cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Rows returned for this page.
    pub rows: Vec<T>,
    /// Backend-owned continuation cursor. Flux passes it through without interpretation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Normalized paging request handed to a live backend.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PageRequest {
    /// Opaque cursor returned by a previous [`Page`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Validated and clamped maximum row count.
    pub limit: usize,
}

/// One already-validated scalar filter value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterValue {
    /// Text or enum value.
    String(String),
    /// Signed integer value.
    Int(i64),
    /// Boolean value.
    Bool(bool),
}

/// Deterministically ordered, already-validated filters passed to a backend.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Filters(BTreeMap<String, FilterValue>);

impl Filters {
    /// Create an empty filter set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace one normalized filter.
    pub fn insert(&mut self, name: impl Into<String>, value: FilterValue) -> Option<FilterValue> {
        self.0.insert(name.into(), value)
    }

    /// Read one normalized filter by name.
    pub fn get(&self, name: &str) -> Option<&FilterValue> {
        self.0.get(name)
    }

    /// Iterate in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &FilterValue)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// Whether no filters were supplied.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of normalized filters.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<K> FromIterator<(K, FilterValue)> for Filters
where
    K: Into<String>,
{
    fn from_iter<T: IntoIterator<Item = (K, FilterValue)>>(iter: T) -> Self {
        Self(
            iter.into_iter()
                .map(|(name, value)| (name.into(), value))
                .collect(),
        )
    }
}

/// Declared scalar type of a supported filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    /// Arbitrary text.
    String,
    /// Signed integer.
    Int,
    /// Boolean.
    Bool,
    /// One of the declared string values.
    Enum(Vec<String>),
}

/// One filter accepted by an entity's list operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterKey {
    /// Model-facing filter name.
    pub name: String,
    /// Scalar contract enforced before backend invocation.
    #[serde(rename = "type")]
    pub ty: FilterType,
    /// Whether callers must supply this filter.
    #[serde(default)]
    pub required: bool,
    /// Optional model-facing explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One entity exposed by a live datasource domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEntity {
    /// Entity discriminator accepted by list/get.
    pub entity: String,
    /// Filters accepted by list for this entity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterKey>,
    /// Page size used when the caller omits a limit.
    pub default_page: usize,
    /// Hard ceiling applied to caller-supplied limits.
    pub max_page: usize,
    /// Optional model-facing explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Complete model-facing schema for one registered live domain.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LiveSchema {
    /// Entities exposed by the domain.
    pub entities: Vec<LiveEntity>,
}
