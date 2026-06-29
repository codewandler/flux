//! `flux-datasource` — the shared datasource schema (L0): records, entity declarations, and the
//! retrieval request/response types.
//!
//! This is the contract both ends of the knowledge layer agree on, kept here as **pure data** (no IO,
//! no flux deps) so it can sit beneath both:
//! - `flux-capabilities` (L5) indexes [`Record`]s and answers [`SearchInput`] / [`GetInput`] / [`ListInput`]
//!   / [`RelationInput`] / [`BatchGetInput`] queries (story D-07);
//! - integration plugins (`flux-plugin`, L4) declare [`Declaration`]s and emit [`Record`]s over the
//!   process-plugin protocol (stories D-10 / D-08), so live integrations and local docs share one shape.
//!
//! Shapes are ported (not copied) from fluxplane's `fluxplane-datasource`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where a record came from: a plugin (or `"local"` for the host's own ingesters) and an optional
/// configured instance (e.g. two GitLab endpoints).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Source {
    /// The contributing plugin name, or `"local"` for host-ingested records.
    pub plugin: String,
    /// An optional instance discriminator when a plugin is configured more than once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl Source {
    /// A source from just a plugin name (no instance).
    pub fn new(plugin: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            instance: None,
        }
    }

    /// A source with a configured instance.
    pub fn with_instance(plugin: impl Into<String>, instance: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            instance: Some(instance.into()),
        }
    }

    /// A stable string key for the source: `"plugin"` or `"plugin/instance"`. Used as part of a
    /// record's primary key by the index.
    pub fn key(&self) -> String {
        match &self.instance {
            Some(i) => format!("{}/{}", self.plugin, i),
            None => self.plugin.clone(),
        }
    }
}

/// A typed relation from one record to another — powers [`RelationInput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// The relation name (e.g. `"author"`, `"project"`, `"parent"`).
    pub rel: String,
    /// The related record's entity type.
    pub target_entity: String,
    /// The related record's id.
    pub target_id: String,
}

/// One indexed knowledge record, addressable by `(source, entity, id)`.
///
/// `title`+`body` are the searchable text; `links` carry typed relations; `meta` is freeform
/// (url, path, line, `updated_at`, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The entity type, e.g. `"file.document"`, `"openapi.operation"`, `"gitlab.merge_request"`.
    pub entity: String,
    /// The id, stable within `(source, entity)`.
    pub id: String,
    /// The datasource origin.
    pub source: Source,
    /// A short human title.
    #[serde(default)]
    pub title: String,
    /// The indexed text (the chunk).
    #[serde(default)]
    pub body: String,
    /// Typed relations to other records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Freeform metadata (url/path/line/updated_at/…).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub meta: Value,
}

impl Record {
    /// A minimal record from its address + text (no links/meta).
    pub fn new(
        source: Source,
        entity: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            entity: entity.into(),
            id: id.into(),
            source,
            title: title.into(),
            body: body.into(),
            links: Vec::new(),
            meta: Value::Null,
        }
    }

    /// The record's primary key tuple `(source_key, entity, id)` — what the index dedups/upserts on.
    pub fn address(&self) -> (String, String, String) {
        (self.source.key(), self.entity.clone(), self.id.clone())
    }
}

/// A field in an [`EntitySchema`] — describes one column of a contributed entity for display/lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaField {
    /// The field name.
    pub name: String,
    /// An optional JSON-ish type hint (`"string"`, `"number"`, `"boolean"`, `"object"`, `"array"`).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    /// An optional human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The shape of an entity a datasource contributes — which field is the id, which is the title, and
/// the displayable fields. Declared explicitly (a `#[derive(EntitySchema)]` is an optional later
/// convenience; explicit values are the baseline).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EntitySchema {
    /// The entity type this schema describes.
    pub entity: String,
    /// The struct/record field that holds the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_field: Option<String>,
    /// The field that holds the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_field: Option<String>,
    /// The displayable fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SchemaField>,
}

/// A datasource a plugin contributes: a named, typed set of records the host can search/get/list.
/// Part of a plugin's manifest (consumed by D-10/D-08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// The datasource name, e.g. `"slack.channels"`.
    pub name: String,
    /// The entity type its records carry, e.g. `"slack.channel"`.
    pub entity: String,
    /// A human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The retrieval capabilities it supports: any of `"search"`, `"get"`, `"list"`, `"relation"`,
    /// `"index"` (contributes records to the host index).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The entity's schema, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_schema: Option<EntitySchema>,
}

/// `search` input: a free-text query, optionally scoped to one source/entity, with a result cap.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchInput {
    /// The query string.
    pub query: String,
    /// Restrict to one source key (`"plugin"` or `"plugin/instance"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Restrict to one entity type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Max results (the backend picks a default when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// One scored search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    /// The matching record.
    pub record: Record,
    /// The relevance score (higher is better; backend-defined scale).
    pub score: f64,
    /// Which fields matched (e.g. `["title"]`), when the backend can attribute them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_fields: Vec<String>,
}

/// `get` input: fetch one record by its `(source, entity, id)` address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetInput {
    /// The source key.
    pub source: String,
    /// The entity type.
    pub entity: String,
    /// The id.
    pub id: String,
}

/// `list` input: enumerate a datasource (optionally one entity type), paged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ListInput {
    /// The source key to enumerate.
    pub source: String,
    /// Restrict to one entity type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Skip this many records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    /// Return at most this many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// `relation` input: the records linked from one record, optionally filtered by relation name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationInput {
    /// The source key.
    pub source: String,
    /// The entity type of the originating record.
    pub entity: String,
    /// The id of the originating record.
    pub id: String,
    /// Only links with this relation name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
}

/// `batch_get` input: fetch several records of one entity from one source in a single round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchGetInput {
    /// The source key.
    pub source: String,
    /// The entity type.
    pub entity: String,
    /// The ids to fetch.
    pub ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_key_with_and_without_instance() {
        assert_eq!(Source::new("local").key(), "local");
        assert_eq!(Source::with_instance("gitlab", "prod").key(), "gitlab/prod");
    }

    #[test]
    fn record_address_is_source_entity_id() {
        let r = Record::new(
            Source::with_instance("gitlab", "prod"),
            "gitlab.merge_request",
            "42",
            "Fix the thing",
            "body",
        );
        assert_eq!(
            r.address(),
            (
                "gitlab/prod".to_string(),
                "gitlab.merge_request".to_string(),
                "42".to_string()
            )
        );
    }

    #[test]
    fn record_round_trips_through_json() {
        let mut r = Record::new(
            Source::new("local"),
            "file.document",
            "docs/x.md",
            "X",
            "warm transfer details",
        );
        r.links.push(Link {
            rel: "parent".into(),
            target_entity: "file.document".into(),
            target_id: "docs/index.md".into(),
        });
        r.meta = json!({ "path": "docs/x.md", "updated_at": 1 });
        let s = serde_json::to_string(&r).unwrap();
        let back: Record = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
        // a record with no links/meta omits them (compact wire shape)
        let bare = Record::new(Source::new("local"), "e", "1", "t", "b");
        let bare_json: Value =
            serde_json::from_str(&serde_json::to_string(&bare).unwrap()).unwrap();
        assert!(bare_json.get("links").is_none());
        assert!(bare_json.get("meta").is_none());
    }

    #[test]
    fn declaration_round_trips_with_type_rename() {
        let d = Declaration {
            name: "slack.channels".into(),
            entity: "slack.channel".into(),
            description: Some("Slack channels".into()),
            capabilities: vec!["search".into(), "get".into()],
            entity_schema: Some(EntitySchema {
                entity: "slack.channel".into(),
                id_field: Some("id".into()),
                title_field: Some("name".into()),
                fields: vec![SchemaField {
                    name: "topic".into(),
                    ty: Some("string".into()),
                    description: None,
                }],
            }),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(
            s.contains("\"type\":\"string\""),
            "field type renames to `type`: {s}"
        );
        let back: Declaration = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }
}
