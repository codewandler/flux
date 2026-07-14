//! Async live-system datasource contract.
//!
//! This trait sits beside [`super::DatasourceBackend`]: the existing backend owns a local indexed
//! snapshot, while a live backend reads a remote system of record on demand. Implementations receive
//! the guarded [`ToolContext`] and declare their exact external resource families up front.

use std::collections::HashSet;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_datasource::live::{Filters, LiveSchema, Page, PageRequest, Row};
use flux_runtime::ToolContext;

/// External guarded resource used by a live backend in addition to its datasource identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiveAccess {
    /// HTTP or other URL-addressed network egress.
    Network {
        /// Exact policy subject, normally a guarded origin or URL.
        subject: String,
    },
    /// Raw or driver-owned connection target.
    Connection {
        /// Exact policy subject, such as `tcp:db.example:5432`.
        subject: String,
    },
}

impl LiveAccess {
    /// Concrete policy subject carried by this declaration.
    pub fn subject(&self) -> &str {
        match self {
            Self::Network { subject } | Self::Connection { subject } => subject,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Connection { .. } => "connection",
        }
    }
}

/// A live, async system-of-record backend.
///
/// Implementations must perform real IO through flux's guarded host surfaces. Receiving
/// [`ToolContext`] keeps filesystem/process access on the canonical runtime context; URL and
/// connection clients must still use the corresponding DNS/private-network or host-capability
/// guards described by their [`LiveAccess`] declaration.
#[async_trait]
pub trait LiveDatasource: Send + Sync {
    /// Model-facing entities, filter contracts, and page bounds.
    fn schema(&self) -> LiveSchema;

    /// Concrete external resources needed by `list` and `get`. Empty means an in-process backend.
    fn access(&self) -> Vec<LiveAccess> {
        Vec::new()
    }

    /// Fetch one page of an entity using already-validated filters and paging.
    async fn list(
        &self,
        ctx: &ToolContext,
        entity: &str,
        page: PageRequest,
        filters: &Filters,
    ) -> Result<Page<Row>>;

    /// Resolve one row by stable entity/id, re-entering host-owned authentication and connection
    /// state rather than consuming a model-held capability.
    async fn get(&self, ctx: &ToolContext, entity: &str, id: &str) -> Result<Option<Row>>;
}

/// Validate one domain's static contract before any operation is advertised.
pub fn validate_live_contract(
    domain: &str,
    schema: &LiveSchema,
    access: &[LiveAccess],
) -> Result<()> {
    if !valid_domain(domain) {
        return Err(Error::Other(format!(
            "live datasource domain `{domain}` must match [a-z][a-z0-9_]*"
        )));
    }
    if schema.entities.is_empty() {
        return Err(Error::Other(format!(
            "live datasource `{domain}` declares no entities"
        )));
    }

    let mut entities = HashSet::new();
    for entity in &schema.entities {
        let name = entity.entity.trim();
        if name.is_empty() {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares a blank entity"
            )));
        }
        if name != entity.entity {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{}` has surrounding whitespace",
                entity.entity
            )));
        }
        if !entities.insert(name) {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares duplicate entity `{name}`"
            )));
        }
        if entity.default_page == 0 || entity.max_page == 0 {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{name}` page limits must be greater than zero"
            )));
        }
        if entity.default_page > entity.max_page {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{name}` default_page {} exceeds max_page {}",
                entity.default_page, entity.max_page
            )));
        }

        let mut filters = HashSet::new();
        for filter in &entity.filters {
            let filter_name = filter.name.trim();
            if filter_name.is_empty() || filter_name != filter.name {
                return Err(Error::Other(format!(
                    "live datasource `{domain}` entity `{name}` has an invalid blank/whitespace filter name"
                )));
            }
            if !filters.insert(filter_name) {
                return Err(Error::Other(format!(
                    "live datasource `{domain}` entity `{name}` declares duplicate filter `{filter_name}`"
                )));
            }
            if let flux_datasource::live::FilterType::Enum(values) = &filter.ty {
                if values.is_empty() {
                    return Err(Error::Other(format!(
                        "live datasource `{domain}` entity `{name}` filter `{filter_name}` has an empty enum"
                    )));
                }
                let mut seen = HashSet::new();
                for value in values {
                    let trimmed = value.trim();
                    if trimmed.is_empty() || trimmed != value || !seen.insert(trimmed) {
                        return Err(Error::Other(format!(
                            "live datasource `{domain}` entity `{name}` filter `{filter_name}` has a blank, whitespace-padded, or duplicate enum value"
                        )));
                    }
                }
            }
        }
    }

    let mut resources = HashSet::new();
    for declared in access {
        let subject = declared.subject();
        if subject.trim().is_empty() {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares a blank {} authority subject",
                declared.kind()
            )));
        }
        if subject.trim() != subject {
            return Err(Error::Other(format!(
                "live datasource `{domain}` {} authority subject has surrounding whitespace",
                declared.kind()
            )));
        }
        if !resources.insert(declared) {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares duplicate {} authority `{subject}`",
                declared.kind()
            )));
        }
    }

    Ok(())
}

fn valid_domain(domain: &str) -> bool {
    let mut chars = domain.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}
