//! Offline projection of Flux's foundational catalogue.
//!
//! The runtime registry and Flux-Lang schema remain the authorities. This module selects and
//! serializes them; it neither invokes an operation nor performs IO beyond writing stdout.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use flux_runtime::ToolRegistry;
use flux_spec::ToolSpec;

use super::{CatalogAction, CatalogFormat};

const ENTRY_SCHEMA: &str = "https://flux.codewandler.org/v1/schema/core-entry.schema.json";
const CATALOG_SCHEMA: &str = "https://flux.codewandler.org/v1/schema/core-catalog.schema.json";
const AST_SCHEMA: &str = "https://flux.codewandler.org/v1/schema/flux-ast.schema.json";
const CATALOG_ID: &str = "https://flux.codewandler.org/v1/core/index.json";
const CORE_PREFIX: &str = "https://flux.codewandler.org/v1/core/";

const OPERATION_NAMES: &[&str] = &[
    "all",
    "any",
    "coalesce",
    "compare",
    "count_by",
    "dedupe",
    "filter",
    "first",
    "flatten",
    "group_by",
    "has",
    "http.request",
    "join",
    "keys",
    "last",
    "len",
    "map",
    "merge",
    "merge_obj",
    "omit",
    "pick",
    "regex_extract",
    "regex_match",
    "skip",
    "sort",
    "split",
    "sum",
    "top",
    "values",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Available,
    Planned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    Operation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum NodeKind {
    Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CapabilityKind {
    Capability,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CoreOperation {
    #[serde(rename = "$schema")]
    schema: String,
    #[serde(rename = "$id")]
    id: String,
    schema_version: u32,
    kind: OperationKind,
    name: String,
    title: String,
    description: String,
    category: Vec<String>,
    availability: Availability,
    tool_spec: ToolSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CoreNode {
    #[serde(rename = "$schema")]
    schema: String,
    #[serde(rename = "$id")]
    id: String,
    schema_version: u32,
    kind: NodeKind,
    name: String,
    title: String,
    description: String,
    category: Vec<String>,
    availability: Availability,
    schema_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CoreCapability {
    #[serde(rename = "$schema")]
    schema: String,
    #[serde(rename = "$id")]
    id: String,
    schema_version: u32,
    kind: CapabilityKind,
    name: String,
    title: String,
    description: String,
    category: Vec<String>,
    availability: Availability,
    callable: bool,
    operation_ids: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum CoreEntry {
    Operation(CoreOperation),
    Node(CoreNode),
    Capability(CoreCapability),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CoreSchemas {
    catalog: Value,
    entry: Value,
    flux_ast: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CoreCatalog {
    #[serde(rename = "$schema")]
    schema: String,
    #[serde(rename = "$id")]
    id: String,
    schema_version: u32,
    generator: String,
    operations: Vec<CoreOperation>,
    nodes: Vec<CoreNode>,
    capabilities: Vec<CoreCapability>,
    schemas: CoreSchemas,
}

pub(super) fn run_catalog(action: CatalogAction) -> Result<()> {
    match action {
        CatalogAction::Core {
            format: CatalogFormat::Json,
        } => {
            let catalog = build_core_catalog()?;
            let mut stdout = io::BufWriter::new(io::stdout().lock());
            serde_json::to_writer_pretty(&mut stdout, &catalog)
                .context("serialize core catalog")?;
            stdout.write_all(b"\n").context("write core catalog")?;
            stdout.flush().context("flush core catalog")
        }
    }
}

fn build_core_catalog() -> Result<CoreCatalog> {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)
        .context("register built-in operations for core catalogue")?;
    flux_web::try_register_web(&mut registry, &flux_web::WebOptions::default())
        .context("register web operations for core catalogue")?;
    let specs: BTreeMap<String, ToolSpec> = registry
        .specs()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect();

    let mut operations = Vec::with_capacity(OPERATION_NAMES.len());
    for name in OPERATION_NAMES {
        let spec = specs
            .get(*name)
            .with_context(|| format!("required core operation `{name}` is not registered"))?
            .clone();
        let (id, category) = operation_identity(name);
        operations.push(CoreOperation {
            schema: ENTRY_SCHEMA.into(),
            id,
            schema_version: 1,
            kind: OperationKind::Operation,
            name: (*name).into(),
            title: human_title(name),
            description: spec.description.clone(),
            category,
            availability: Availability::Available,
            tool_spec: spec,
        });
    }

    let mut ast_schema = flux_lang::schema::ast_schema();
    install_ast_identity_and_anchors(&mut ast_schema)?;
    let nodes = flux_lang::schema::node_kind_rows()
        .into_iter()
        .map(|(name, description)| CoreNode {
            schema: ENTRY_SCHEMA.into(),
            id: format!("{CORE_PREFIX}language/node/{name}.json"),
            schema_version: 1,
            kind: NodeKind::Node,
            title: human_title(&name),
            schema_ref: format!("{AST_SCHEMA}#node-{name}"),
            name,
            description,
            category: vec!["language".into(), "node".into()],
            availability: Availability::Available,
        })
        .collect();

    let http_operation_id = operations
        .iter()
        .find(|op| op.name == "http.request")
        .map(|op| op.id.clone())
        .context("HTTP operation selected")?;
    let mut capabilities = vec![
        capability(
            "http",
            "HTTP",
            "Typed HTTP requests through Flux's guarded web egress boundary.",
            &["network", "application"],
            Availability::Available,
            true,
            vec![http_operation_id],
        ),
        capability(
            "dns",
            "DNS",
            "Explicit DNS resolution; planned pending a guarded resolver contract.",
            &["network", "application"],
            Availability::Planned,
            false,
            vec![],
        ),
        capability(
            "tcp",
            "TCP",
            "Guarded TCP stream access; planned pending connection-lifetime and authority rules.",
            &["network", "transport"],
            Availability::Planned,
            false,
            vec![],
        ),
        capability(
            "udp",
            "UDP",
            "Guarded datagram access; planned pending destination and reply-source rules.",
            &["network", "transport"],
            Availability::Planned,
            false,
            vec![],
        ),
        capability(
            "icmp",
            "ICMP",
            "Portable guarded reachability checks; planned where host privilege permits.",
            &["network", "internet"],
            Availability::Planned,
            false,
            vec![],
        ),
    ];
    capabilities.sort_by(|a, b| a.id.cmp(&b.id));

    let mut catalog_schema = serde_json::to_value(schemars::schema_for!(CoreCatalog))
        .context("serialize core catalogue schema")?;
    identify_schema(&mut catalog_schema, CATALOG_SCHEMA);
    let mut entry_schema = serde_json::to_value(schemars::schema_for!(CoreEntry))
        .context("serialize core entry schema")?;
    identify_schema(&mut entry_schema, ENTRY_SCHEMA);

    let catalog = CoreCatalog {
        schema: CATALOG_SCHEMA.into(),
        id: CATALOG_ID.into(),
        schema_version: 1,
        generator: format!("flux {}", env!("CARGO_PKG_VERSION")),
        operations,
        nodes,
        capabilities,
        schemas: CoreSchemas {
            catalog: catalog_schema,
            entry: entry_schema,
            flux_ast: ast_schema,
        },
    };
    validate_catalog_invariants(&catalog)?;
    Ok(catalog)
}

fn operation_identity(name: &str) -> (String, Vec<String>) {
    if name == "http.request" {
        return (
            format!("{CORE_PREFIX}network/application/http/request.json"),
            vec!["network".into(), "application".into(), "http".into()],
        );
    }
    (
        format!("{CORE_PREFIX}data/transform/{name}.json"),
        vec!["data".into(), "transform".into()],
    )
}

fn capability(
    name: &str,
    title: &str,
    description: &str,
    category: &[&str],
    availability: Availability,
    callable: bool,
    operation_ids: Vec<String>,
) -> CoreCapability {
    CoreCapability {
        schema: ENTRY_SCHEMA.into(),
        id: format!("{CORE_PREFIX}{}/{name}.json", category.join("/")),
        schema_version: 1,
        kind: CapabilityKind::Capability,
        name: name.into(),
        title: title.into(),
        description: description.into(),
        category: category.iter().map(|part| (*part).into()).collect(),
        availability,
        callable,
        operation_ids,
    }
}

fn identify_schema(schema: &mut Value, id: &str) {
    if let Some(object) = schema.as_object_mut() {
        object.insert("$id".into(), Value::String(id.into()));
        object.entry("$schema").or_insert_with(|| {
            Value::String("https://json-schema.org/draft/2020-12/schema".into())
        });
    }
}

fn install_ast_identity_and_anchors(schema: &mut Value) -> Result<()> {
    identify_schema(schema, AST_SCHEMA);
    let defs_key = if schema.get("$defs").is_some() {
        "$defs"
    } else if schema.get("definitions").is_some() {
        "definitions"
    } else {
        bail!("Flux AST schema has no definitions map");
    };
    let variants = schema
        .get_mut(defs_key)
        .and_then(|defs| defs.get_mut("Node"))
        .and_then(|node| node.get_mut("oneOf"))
        .and_then(Value::as_array_mut)
        .context("Flux AST schema has no Node.oneOf")?;
    for variant in variants {
        let name = variant
            .get("properties")
            .and_then(|props| props.get("kind"))
            .and_then(|kind| {
                kind.get("const").and_then(Value::as_str).or_else(|| {
                    kind.get("enum")
                        .and_then(Value::as_array)
                        .and_then(|items| items.first())
                        .and_then(Value::as_str)
                })
            })
            .context("Node variant has no kind tag")?
            .to_string();
        variant
            .as_object_mut()
            .context("Node variant is not an object schema")?
            .insert("$anchor".into(), Value::String(format!("node-{name}")));
    }
    Ok(())
}

fn validate_catalog_invariants(catalog: &CoreCatalog) -> Result<()> {
    let mut ids = BTreeSet::new();
    for (id, schema, version) in catalog
        .operations
        .iter()
        .map(|entry| (&entry.id, &entry.schema, entry.schema_version))
        .chain(
            catalog
                .nodes
                .iter()
                .map(|entry| (&entry.id, &entry.schema, entry.schema_version)),
        )
        .chain(
            catalog
                .capabilities
                .iter()
                .map(|entry| (&entry.id, &entry.schema, entry.schema_version)),
        )
    {
        if !id.starts_with(CORE_PREFIX) || !id.ends_with(".json") {
            bail!("core entry has non-canonical id `{id}`");
        }
        if schema != ENTRY_SCHEMA || version != 1 {
            bail!("core entry `{id}` has the wrong schema contract");
        }
        if !ids.insert(id) {
            bail!("duplicate core entry id `{id}`");
        }
    }
    for capability in &catalog.capabilities {
        match capability.availability {
            Availability::Available
                if !capability.callable || capability.operation_ids.is_empty() =>
            {
                bail!(
                    "available capability `{}` has no operation",
                    capability.name
                )
            }
            Availability::Planned
                if capability.callable || !capability.operation_ids.is_empty() =>
            {
                bail!(
                    "planned capability `{}` masquerades as callable",
                    capability.name
                )
            }
            _ => {}
        }
        for operation_id in &capability.operation_ids {
            if !catalog.operations.iter().any(|op| &op.id == operation_id) {
                bail!(
                    "capability `{}` references missing operation `{operation_id}`",
                    capability.name
                );
            }
        }
    }
    Ok(())
}

fn human_title(name: &str) -> String {
    name.split(['.', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_operations_are_exact_and_resolve_from_the_registry() {
        let catalog = build_core_catalog().unwrap();
        assert_eq!(catalog.operations.len(), OPERATION_NAMES.len());
        assert_eq!(
            catalog
                .operations
                .iter()
                .map(|op| op.name.as_str())
                .collect::<Vec<_>>(),
            OPERATION_NAMES
        );
    }

    #[test]
    fn ast_projection_has_one_stable_anchor_per_node() {
        let catalog = build_core_catalog().unwrap();
        let schema = catalog.schemas.flux_ast.to_string();
        for node in &catalog.nodes {
            assert!(schema.contains(&format!("node-{}", node.name)));
        }
    }

    #[test]
    fn schemas_and_documents_round_trip() {
        let catalog = build_core_catalog().unwrap();
        let value = serde_json::to_value(&catalog).unwrap();
        let decoded: CoreCatalog = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.schema_version, 1);
        jsonschema::validator_for(&decoded.schemas.catalog)
            .unwrap()
            .validate(&serde_json::to_value(&decoded).unwrap())
            .unwrap();
        let entry_validator = jsonschema::validator_for(&decoded.schemas.entry).unwrap();
        let entries = decoded
            .operations
            .iter()
            .cloned()
            .map(CoreEntry::Operation)
            .chain(decoded.nodes.iter().cloned().map(CoreEntry::Node))
            .chain(
                decoded
                    .capabilities
                    .iter()
                    .cloned()
                    .map(CoreEntry::Capability),
            );
        for entry in entries {
            let value = serde_json::to_value(entry).unwrap();
            let _: CoreEntry = serde_json::from_value(value.clone()).unwrap();
            entry_validator.validate(&value).unwrap();
        }
    }
}
