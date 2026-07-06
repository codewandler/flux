//! A runtime object-schema builder: `object_schema([req(...), opt(...)])`. Complements
//! [`crate::ToolSpec::read_only`] (a hand-rolled `serde_json::json!` schema) and
//! [`crate::ToolSpec::read_only_typed`] (a compile-time Rust type) with a third path — building an
//! object schema from a field list *at runtime* — for consumers that define families of tools
//! programmatically (e.g. one op per record type in a catalog) rather than one bespoke input struct
//! per tool.
//!
//! Output is a plain, draft-07-compatible JSON-Schema object: `{"type": "object", "properties": {...},
//! "required": [...]}`, with each field carrying its own `type` + `description` — the same shape the
//! hand-rolled schemas elsewhere in this workspace already use.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// A JSON-Schema field type [`object_schema`] knows how to render. Covers the shapes flux's own
/// tool schemas actually use: bare scalars, a nested object, and a list of strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    /// `{"type": "array", "items": {"type": "string"}}` — the common case of a list of strings.
    ArrayOfString,
}

impl FieldType {
    fn schema(self) -> Value {
        match self {
            FieldType::String => json!({"type": "string"}),
            FieldType::Integer => json!({"type": "integer"}),
            FieldType::Number => json!({"type": "number"}),
            FieldType::Boolean => json!({"type": "boolean"}),
            FieldType::Object => json!({"type": "object"}),
            FieldType::ArrayOfString => json!({"type": "array", "items": {"type": "string"}}),
        }
    }
}

/// One field of a runtime-built object schema. Build with [`req`]/[`opt`] and pass a list to
/// [`object_schema`].
#[derive(Debug, Clone)]
pub struct Field {
    name: String,
    ty: FieldType,
    description: String,
    required: bool,
}

/// A **required** input field.
pub fn req(name: impl Into<String>, ty: FieldType, description: impl Into<String>) -> Field {
    Field {
        name: name.into(),
        ty,
        description: description.into(),
        required: true,
    }
}

/// An **optional** input field.
pub fn opt(name: impl Into<String>, ty: FieldType, description: impl Into<String>) -> Field {
    Field {
        name: name.into(),
        ty,
        description: description.into(),
        required: false,
    }
}

/// Build a draft-07-compatible JSON-Schema `object` from a field list: `type: "object"`, one
/// `properties` entry per field (its `type` plus `description`), and a `required` array listing only
/// the [`req`] fields, in declaration order. The result composes with
/// [`ToolSpec::read_only`](crate::ToolSpec::read_only) (or any builder taking a raw `input_schema`).
pub fn object_schema(fields: impl IntoIterator<Item = Field>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for f in fields {
        let mut field_schema = f.ty.schema();
        if let Some(obj) = field_schema.as_object_mut() {
            obj.insert("description".to_string(), Value::String(f.description));
        }
        properties.insert(f.name.clone(), field_schema);
        if f.required {
            required.push(Value::String(f.name));
        }
    }
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
    })
}

/// A no-argument object schema (`{"type": "object", "properties": {}}`) — for the many tools that
/// take no parameters.
pub fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_schema_declares_required_only_for_req_fields() {
        let schema = object_schema([
            req("path", FieldType::String, "file path"),
            opt("limit", FieldType::Integer, "max rows"),
        ]);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["path"]["description"], "file path");
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        assert_eq!(schema["properties"]["limit"]["description"], "max rows");
        assert!(!schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("limit")));
    }

    #[test]
    fn object_schema_covers_every_supported_field_type() {
        let schema = object_schema([
            req("s", FieldType::String, ""),
            req("i", FieldType::Integer, ""),
            req("n", FieldType::Number, ""),
            req("b", FieldType::Boolean, ""),
            req("o", FieldType::Object, ""),
            req("a", FieldType::ArrayOfString, ""),
        ]);
        assert_eq!(schema["properties"]["s"]["type"], "string");
        assert_eq!(schema["properties"]["i"]["type"], "integer");
        assert_eq!(schema["properties"]["n"]["type"], "number");
        assert_eq!(schema["properties"]["b"]["type"], "boolean");
        assert_eq!(schema["properties"]["o"]["type"], "object");
        assert_eq!(schema["properties"]["a"]["type"], "array");
        assert_eq!(schema["properties"]["a"]["items"]["type"], "string");
    }

    #[test]
    fn empty_schema_has_no_properties_or_required() {
        let schema = empty_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"], json!({}));
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn object_schema_with_no_fields_is_a_bare_object() {
        let schema = object_schema(Vec::new());
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["required"], json!([]));
    }

    /// Composes with the ordinary `ToolSpec` builder path, the same as a hand-rolled `json!` schema.
    #[test]
    fn composes_with_tool_spec_read_only() {
        let spec = crate::ToolSpec::read_only(
            "lookup",
            "look something up",
            object_schema([req("id", FieldType::String, "the id")]),
        );
        assert_eq!(spec.input_schema["required"], json!(["id"]));
        assert_eq!(spec.input_schema["properties"]["id"]["type"], "string");
    }
}
