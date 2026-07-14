//! Provider-specific JSON Schema compatibility views.
//!
//! The registered schema remains the host contract. Codecs call this module only while building a
//! disposable wire body, and receive cloned [`ToolDef`]s so projection cannot affect validation or
//! authorization after the provider returns a tool call.

use serde_json::{Map, Value};

use flux_core::{Error, Result};
use flux_provider::ToolDef;

/// Return the tool definitions OpenRouter can forward to the selected model.
pub(crate) fn openrouter_tools(model: &str, tools: &[ToolDef]) -> Result<Vec<ToolDef>> {
    if !is_gemini(model) {
        return Ok(tools.to_vec());
    }

    tools
        .iter()
        .map(|tool| {
            let mut projected = tool.clone();
            project_gemini_schema(&tool.name, &mut projected.input_schema, "")?;
            Ok(projected)
        })
        .collect()
}

fn is_gemini(model: &str) -> bool {
    model
        .strip_prefix("google/")
        .is_some_and(|name| name.starts_with("gemini-"))
}

fn project_gemini_schema(operation: &str, schema: &mut Value, pointer: &str) -> Result<()> {
    match schema {
        // An empty object is the JSON Schema object spelling of the same unconstrained contract,
        // and OpenRouter's Gemini adapter accepts it where a boolean schema is not portable.
        Value::Bool(true) => *schema = Value::Object(Map::new()),
        Value::Bool(false) => {
            return Err(incompatible(
                operation,
                pointer,
                "the boolean `false` schema has no equivalent Gemini function-declaration shape",
            ));
        }
        Value::Object(object) => project_gemini_object(operation, object, pointer)?,
        _ => {
            return Err(incompatible(
                operation,
                pointer,
                "expected a JSON Schema object or boolean",
            ));
        }
    }
    Ok(())
}

fn project_gemini_object(
    operation: &str,
    object: &mut Map<String, Value>,
    pointer: &str,
) -> Result<()> {
    drop_safe_annotations(object);
    reject_unsupported_keywords(operation, object, pointer)?;
    normalize_simple_nullable_any_of(object);
    normalize_type(operation, object, pointer)?;
    validate_supported_keyword_shapes(operation, object, pointer)?;
    if pointer.is_empty() && object.get("type").and_then(Value::as_str) != Some("object") {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "type"),
            "Gemini function parameters require an object schema at the root",
        ));
    }
    if pointer.is_empty() && object.get("nullable") == Some(&Value::Bool(true)) {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "nullable"),
            "Gemini function parameters cannot make the root argument object nullable",
        ));
    }
    materialize_required_properties(operation, object, pointer)?;

    if object.get("type").and_then(Value::as_str) == Some("array") && !object.contains_key("items")
    {
        // Omitting `items` and spelling it as `{}` are equivalent in JSON Schema. Gemini requires
        // the field, while OpenRouter accepts the explicit unconstrained schema.
        object.insert("items".into(), Value::Object(Map::new()));
    }

    for keyword in ["properties", "$defs"] {
        project_schema_map(operation, object, pointer, keyword)?;
    }
    project_schema_array(operation, object, pointer, "anyOf")?;
    project_schema_value(operation, object, pointer, "items", false)?;

    // Boolean additional-property constraints are already meaningful provider fields; recurse only
    // when the value is itself a schema object. A `false` value is rejected above only when
    // materializing a required-but-undeclared property would otherwise widen it.
    project_schema_value(operation, object, pointer, "additionalProperties", true)?;

    Ok(())
}

/// Normalize the common JSON Schema nullable spelling emitted by generators such as schemars.
/// Restrict the rewrite to two type-only branches: a `$ref` or constrained branch needs a more
/// involved exact transformation and therefore follows the ordinary recursive rejection path.
fn normalize_simple_nullable_any_of(object: &mut Map<String, Value>) {
    fn branch_type(branch: &Value) -> Option<&str> {
        let branch = branch.as_object()?;
        (branch.len() == 1)
            .then(|| branch.get("type").and_then(Value::as_str))
            .flatten()
    }

    if object.contains_key("type") {
        return;
    }
    let Some(branches) = object.get("anyOf").and_then(Value::as_array) else {
        return;
    };
    if branches.len() != 2 {
        return;
    }
    let left = branch_type(&branches[0]);
    let right = branch_type(&branches[1]);
    let concrete = match (left, right) {
        (Some("null"), Some(value)) | (Some(value), Some("null"))
            if matches!(
                value,
                "array" | "boolean" | "integer" | "number" | "object" | "string"
            ) =>
        {
            value
        }
        _ => return,
    };
    let concrete = concrete.to_string();
    object.remove("anyOf");
    object.insert(
        "type".into(),
        Value::Array(vec![Value::String(concrete), Value::String("null".into())]),
    );
}

/// Gemini function declarations support a documented OpenAPI subset. Three assertion keywords in
/// addition to that list are retained because the live OpenRouter Gemini path accepted them in the
/// A-78 catalog (`additionalProperties`, `maxItems`, and `minimum`). Everything else either has an
/// explicit equivalence rewrite or fails locally; silently forwarding it would defer a paid 400.
const SUPPORTED_KEYWORDS: &[&str] = &[
    "$defs",
    "$ref",
    "additionalProperties",
    "anyOf",
    "description",
    "enum",
    "format",
    "items",
    "maxItems",
    "minimum",
    "nullable",
    "properties",
    "required",
    "type",
];

/// Annotation-only JSON Schema/OpenAPI keywords do not affect the instance set. Removing them from
/// the wire view is therefore equivalence-preserving; the original registered schema keeps them.
const SAFE_ANNOTATIONS: &[&str] = &[
    "$comment",
    "default",
    "deprecated",
    "example",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];

fn drop_safe_annotations(object: &mut Map<String, Value>) {
    object.retain(|keyword, _| {
        !SAFE_ANNOTATIONS.contains(&keyword.as_str()) && !keyword.starts_with("x-")
    });
}

fn reject_unsupported_keywords(
    operation: &str,
    object: &Map<String, Value>,
    pointer: &str,
) -> Result<()> {
    for keyword in object.keys() {
        if !SUPPORTED_KEYWORDS.contains(&keyword.as_str()) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, keyword),
                &format!(
                    "keyword `{keyword}` is outside the supported OpenRouter Gemini function-schema subset"
                ),
            ));
        }
    }
    Ok(())
}

fn normalize_type(operation: &str, object: &mut Map<String, Value>, pointer: &str) -> Result<()> {
    let schema_type = match object.get("type").cloned() {
        Some(schema_type) => schema_type,
        None if object.get("enum").is_some_and(|values| {
            values
                .as_array()
                .is_some_and(|values| !values.is_empty() && values.iter().all(Value::is_string))
        }) =>
        {
            // A non-empty all-string enum already accepts only strings in JSON Schema. Making the
            // implied type explicit is exact and satisfies Gemini's concrete-type requirement.
            let schema_type = Value::String("string".into());
            object.insert("type".into(), schema_type.clone());
            schema_type
        }
        None => {
            let has_constraint_requiring_type = object.keys().any(|keyword| {
                !matches!(keyword.as_str(), "$defs" | "$ref" | "anyOf" | "description")
            });
            if has_constraint_requiring_type {
                return Err(incompatible(
                    operation,
                    &child_pointer(pointer, "type"),
                    "a constrained Gemini function schema requires one concrete `type`",
                ));
            }
            return Ok(());
        }
    };

    let (concrete, nullable) = match schema_type {
        Value::String(value) => (vec![value], false),
        Value::Array(values) => {
            let values = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        incompatible(
                            operation,
                            &child_pointer(&child_pointer(pointer, "type"), &index.to_string()),
                            "type-union members must be strings",
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let concrete = values
                .iter()
                .filter(|value| value.as_str() != "null")
                .cloned()
                .collect::<Vec<_>>();
            let nullable = values.iter().any(|value| value == "null");
            if concrete.is_empty()
                || values.len() != concrete.len() + usize::from(nullable)
                || concrete
                    .iter()
                    .enumerate()
                    .any(|(index, value)| concrete[..index].contains(value))
            {
                return Err(incompatible(
                    operation,
                    &child_pointer(pointer, "type"),
                    "`type` unions must contain unique concrete types and at most one `null`",
                ));
            }
            (concrete, nullable)
        }
        _ => {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "type"),
                "`type` must be a string or an array of strings",
            ));
        }
    };

    for concrete_type in &concrete {
        if !matches!(
            concrete_type.as_str(),
            "array" | "boolean" | "integer" | "number" | "object" | "string"
        ) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "type"),
                &format!("unsupported Gemini function-schema type `{concrete_type}`"),
            ));
        }
    }

    if concrete.len() > 1 {
        if nullable {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "type"),
                "a multi-type union containing `null` has no proven exact Gemini projection",
            ));
        }
        if let Some(keyword) = object
            .keys()
            .find(|keyword| !matches!(keyword.as_str(), "type" | "description" | "$defs"))
        {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "type"),
                &format!(
                    "a multi-type union combined with sibling `{keyword}` has no proven exact Gemini projection"
                ),
            ));
        }
    }

    if !nullable && object.get("nullable") == Some(&Value::Bool(true)) {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "nullable"),
            "standalone `nullable: true` would widen the original JSON Schema; use a `type` union with `null`",
        ));
    }

    if nullable {
        if object.get("nullable").is_some_and(|value| value != true) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "nullable"),
                "nullable type union conflicts with the existing `nullable` value",
            ));
        }
        object.insert("nullable".into(), Value::Bool(true));
    }
    if concrete.len() == 1 {
        object.insert("type".into(), Value::String(concrete[0].clone()));
    } else {
        // Gemini's OpenAPI-shaped declaration accepts `anyOf`, not JSON Schema's array spelling
        // for `type`. The sibling guard above restricts this rewrite to annotation/declaration
        // siblings, so one type-only branch per member is exactly equivalent without relying on
        // provider-specific handling of type assertions beside a parent `anyOf`.
        object.remove("type");
        object.insert(
            "anyOf".into(),
            Value::Array(
                concrete
                    .into_iter()
                    .map(|schema_type| {
                        Value::Object(Map::from_iter([(
                            "type".into(),
                            Value::String(schema_type),
                        )]))
                    })
                    .collect(),
            ),
        );
    }
    Ok(())
}

fn validate_supported_keyword_shapes(
    operation: &str,
    object: &Map<String, Value>,
    pointer: &str,
) -> Result<()> {
    for keyword in ["description", "format", "$ref"] {
        if object.get(keyword).is_some_and(|value| !value.is_string()) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, keyword),
                &format!("`{keyword}` must be a string"),
            ));
        }
    }
    if object
        .get("nullable")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "nullable"),
            "`nullable` must be a boolean",
        ));
    }
    if object
        .get("maxItems")
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "maxItems"),
            "`maxItems` must be a non-negative integer",
        ));
    }
    if object
        .get("minimum")
        .is_some_and(|value| !value.is_number())
    {
        return Err(incompatible(
            operation,
            &child_pointer(pointer, "minimum"),
            "`minimum` must be a number",
        ));
    }
    if let Some(values) = object.get("enum") {
        let values = values
            .as_array()
            .filter(|values| !values.is_empty())
            .ok_or_else(|| {
                incompatible(
                    operation,
                    &child_pointer(pointer, "enum"),
                    "`enum` must be a non-empty array of strings",
                )
            })?;
        if let Some((index, _)) = values
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_string())
        {
            return Err(incompatible(
                operation,
                &child_pointer(&child_pointer(pointer, "enum"), &index.to_string()),
                "Gemini function-schema enum members must be strings",
            ));
        }
    }
    if let Some(values) = object.get("anyOf") {
        if !values.as_array().is_some_and(|values| !values.is_empty()) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "anyOf"),
                "`anyOf` must be a non-empty array of schemas",
            ));
        }
    }
    Ok(())
}

fn materialize_required_properties(
    operation: &str,
    object: &mut Map<String, Value>,
    pointer: &str,
) -> Result<()> {
    let Some(required) = object.get("required") else {
        return Ok(());
    };
    let required = required.as_array().ok_or_else(|| {
        incompatible(
            operation,
            &child_pointer(pointer, "required"),
            "`required` must be an array",
        )
    })?;
    let required = required
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(|name| (index, name.to_string()))
                .ok_or_else(|| {
                    incompatible(
                        operation,
                        &child_pointer(&child_pointer(pointer, "required"), &index.to_string()),
                        "required property names must be strings",
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;

    let existing = match object.get("properties") {
        Some(Value::Object(properties)) => properties.keys().cloned().collect::<Vec<_>>(),
        Some(_) => {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "properties"),
                "`properties` must be an object",
            ));
        }
        None => Vec::new(),
    };
    let missing = required
        .into_iter()
        .filter(|(_, name)| !existing.iter().any(|existing| existing == name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    let additional = object
        .get("additionalProperties")
        .cloned()
        .unwrap_or(Value::Bool(true));
    for (index, name) in &missing {
        if additional == Value::Bool(false) {
            return Err(incompatible(
                operation,
                &child_pointer(&child_pointer(pointer, "required"), &index.to_string()),
                &format!(
                    "required property `{name}` is absent from `properties` and forbidden by `additionalProperties: false`"
                ),
            ));
        }
        if !matches!(additional, Value::Bool(true) | Value::Object(_)) {
            return Err(incompatible(
                operation,
                &child_pointer(pointer, "additionalProperties"),
                "`additionalProperties` must be a schema object or boolean",
            ));
        }
    }

    let properties = object
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("properties was validated as an object above");
    for (_, name) in missing {
        let mut property_schema = match &additional {
            Value::Bool(true) => Value::Object(Map::new()),
            Value::Object(_) => additional.clone(),
            _ => unreachable!("additionalProperties was validated above"),
        };
        project_gemini_schema(
            operation,
            &mut property_schema,
            &child_pointer(pointer, "additionalProperties"),
        )?;
        properties.insert(name, property_schema);
    }
    Ok(())
}

fn project_schema_map(
    operation: &str,
    object: &mut Map<String, Value>,
    pointer: &str,
    keyword: &str,
) -> Result<()> {
    let Some(value) = object.get_mut(keyword) else {
        return Ok(());
    };
    let map = value.as_object_mut().ok_or_else(|| {
        incompatible(
            operation,
            &child_pointer(pointer, keyword),
            &format!("`{keyword}` must be an object of schemas"),
        )
    })?;
    for (name, schema) in map {
        project_gemini_schema(
            operation,
            schema,
            &child_pointer(&child_pointer(pointer, keyword), name),
        )?;
    }
    Ok(())
}

fn project_schema_array(
    operation: &str,
    object: &mut Map<String, Value>,
    pointer: &str,
    keyword: &str,
) -> Result<()> {
    let Some(value) = object.get_mut(keyword) else {
        return Ok(());
    };
    let schemas = value.as_array_mut().ok_or_else(|| {
        incompatible(
            operation,
            &child_pointer(pointer, keyword),
            &format!("`{keyword}` must be an array of schemas"),
        )
    })?;
    for (index, schema) in schemas.iter_mut().enumerate() {
        project_gemini_schema(
            operation,
            schema,
            &child_pointer(&child_pointer(pointer, keyword), &index.to_string()),
        )?;
    }
    Ok(())
}

fn project_schema_value(
    operation: &str,
    object: &mut Map<String, Value>,
    pointer: &str,
    keyword: &str,
    allow_boolean_constraint: bool,
) -> Result<()> {
    let Some(schema) = object.get_mut(keyword) else {
        return Ok(());
    };
    if allow_boolean_constraint && schema.is_boolean() {
        return Ok(());
    }
    project_gemini_schema(operation, schema, &child_pointer(pointer, keyword))
}

fn child_pointer(parent: &str, token: &str) -> String {
    let token = token.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{token}")
}

fn incompatible(operation: &str, pointer: &str, detail: &str) -> Error {
    let pointer = if pointer.is_empty() {
        "<root>"
    } else {
        pointer
    };
    Error::Config(format!(
        "OpenRouter Gemini operation `{operation}` input schema at `{pointer}` is not portable: {detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_gemini_openrouter_schema_is_byte_for_byte_unchanged() {
        let tools = vec![ToolDef {
            name: "records.merge".into(),
            description: "merge".into(),
            input_schema: json!({"type": "array"}),
        }];

        assert_eq!(
            openrouter_tools("deepseek/deepseek-v4", &tools).unwrap(),
            tools
        );
    }

    #[test]
    fn required_property_inherits_additional_properties_schema() {
        let tools = vec![ToolDef {
            name: "labels.put".into(),
            description: "put label".into(),
            input_schema: json!({
                "type": "object",
                "required": ["label"],
                "additionalProperties": {"type": "string"}
            }),
        }];

        let projected = openrouter_tools("google/gemini-3.5-flash", &tools).unwrap();

        assert_eq!(
            projected[0].input_schema["properties"]["label"],
            json!({"type": "string"})
        );
        assert_eq!(tools[0].input_schema.get("properties"), None);
    }

    #[test]
    fn nullable_union_projects_to_openapi_nullable_without_mutating_original() {
        let tools = vec![ToolDef {
            name: "labels.put".into(),
            description: "put label".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": {"type": ["string", "null"], "default": null}
                }
            }),
        }];

        let projected = openrouter_tools("google/gemini-3.5-flash", &tools).unwrap();

        assert_eq!(
            projected[0].input_schema["properties"]["label"],
            json!({"type": "string", "nullable": true})
        );
        assert_eq!(
            tools[0].input_schema["properties"]["label"]["type"],
            json!(["string", "null"])
        );
        assert!(tools[0].input_schema["properties"]["label"]
            .get("default")
            .is_some());
    }

    #[test]
    fn simple_any_of_null_projects_to_nullable_concrete_type() {
        let tools = vec![ToolDef {
            name: "labels.put".into(),
            description: "put label".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": {
                        "anyOf": [
                            {"type": "string"},
                            {"type": "null"}
                        ]
                    }
                }
            }),
        }];

        let projected = openrouter_tools("google/gemini-3.5-flash", &tools).unwrap();

        assert_eq!(
            projected[0].input_schema["properties"]["label"],
            json!({"type": "string", "nullable": true})
        );
        assert!(tools[0].input_schema["properties"]["label"]
            .get("anyOf")
            .is_some());
    }

    #[test]
    fn standalone_nullable_true_is_rejected_instead_of_widening_json_schema() {
        let tools = vec![ToolDef {
            name: "labels.put".into(),
            description: "put label".into(),
            input_schema: json!({"type": "string", "nullable": true}),
        }];

        let error = openrouter_tools("google/gemini-3.5-flash", &tools)
            .unwrap_err()
            .to_string();

        assert!(error.contains("/nullable"), "error was: {error}");
        assert!(error.contains("would widen"), "error was: {error}");
    }

    #[test]
    fn string_enum_without_type_gets_its_exact_implied_type() {
        let tools = vec![ToolDef {
            name: "colors.put".into(),
            description: "put color".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"color": {"enum": ["red", "blue"]}}
            }),
        }];

        let projected = openrouter_tools("google/gemini-3.5-flash", &tools).unwrap();

        assert_eq!(
            projected[0].input_schema["properties"]["color"],
            json!({"type": "string", "enum": ["red", "blue"]})
        );
        assert_eq!(
            tools[0].input_schema["properties"]["color"],
            json!({"enum": ["red", "blue"]})
        );
    }

    #[test]
    fn concrete_type_union_projects_to_equivalent_any_of_without_mutating_original() {
        let tools = vec![ToolDef {
            name: "values.put".into(),
            description: "put value".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "value": {"type": ["string", "number"]}
                }
            }),
        }];

        let projected = openrouter_tools("google/gemini-3.5-flash", &tools).unwrap();

        assert_eq!(
            projected[0].input_schema["properties"]["value"],
            json!({
                "anyOf": [
                    {"type": "string"},
                    {"type": "number"}
                ]
            })
        );
        assert_eq!(
            tools[0].input_schema["properties"]["value"]["type"],
            json!(["string", "number"])
        );
    }

    #[test]
    fn multi_type_union_rejects_unproven_nullable_and_assertion_combinations() {
        for (schema, detail) in [
            (
                json!({"type": ["string", "number", "null"]}),
                "containing `null`",
            ),
            (
                json!({"type": ["string", "number"], "minimum": 0}),
                "sibling `minimum`",
            ),
        ] {
            let tools = vec![ToolDef {
                name: "values.put".into(),
                description: "put value".into(),
                input_schema: schema,
            }];

            let error = openrouter_tools("google/gemini-3.5-flash", &tools)
                .unwrap_err()
                .to_string();

            assert!(error.contains("/type"), "error was: {error}");
            assert!(error.contains(detail), "error was: {error}");
        }
    }

    #[test]
    fn materialized_property_error_keeps_additional_properties_source_path() {
        let tools = vec![ToolDef {
            name: "labels.put".into(),
            description: "put label".into(),
            input_schema: json!({
                "type": "object",
                "required": ["label"],
                "additionalProperties": {
                    "type": "string",
                    "pattern": "^[a-z]+$"
                }
            }),
        }];

        let error = openrouter_tools("google/gemini-3.5-flash", &tools)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("/additionalProperties/pattern"),
            "error was: {error}"
        );
        assert!(!error.contains("/properties/label"), "error was: {error}");
    }

    #[test]
    fn diagnostics_use_escaped_rfc6901_property_paths() {
        let tools = vec![ToolDef {
            name: "closed.put".into(),
            description: "put".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"a/b~c": false}
            }),
        }];

        let error = openrouter_tools("google/gemini-3.5-flash", &tools)
            .unwrap_err()
            .to_string();

        assert!(error.contains("/properties/a~1b~0c"), "error was: {error}");
    }

    #[test]
    fn gemini_function_parameters_reject_non_object_root_but_allow_nested_arrays() {
        let root_array = vec![ToolDef {
            name: "rows.replace".into(),
            description: "replace rows".into(),
            input_schema: json!({"type": "array", "items": {"type": "string"}}),
        }];
        let error = openrouter_tools("google/gemini-3.5-flash", &root_array)
            .unwrap_err()
            .to_string();
        assert!(error.contains("/type"), "error was: {error}");
        assert!(
            error.contains("object schema at the root"),
            "error was: {error}"
        );

        let nullable_root_object = vec![ToolDef {
            name: "rows.replace".into(),
            description: "replace rows".into(),
            input_schema: json!({"type": ["object", "null"], "properties": {}}),
        }];
        let error = openrouter_tools("google/gemini-3.5-flash", &nullable_root_object)
            .unwrap_err()
            .to_string();
        assert!(error.contains("/nullable"), "error was: {error}");
        assert!(
            error.contains("cannot make the root argument object nullable"),
            "error was: {error}"
        );

        let nested_array = vec![ToolDef {
            name: "rows.replace".into(),
            description: "replace rows".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"rows": {"type": "array"}}
            }),
        }];
        let projected = openrouter_tools("google/gemini-3.5-flash", &nested_array).unwrap();
        assert_eq!(
            projected[0].input_schema["properties"]["rows"]["items"],
            json!({})
        );
    }

    #[test]
    fn nested_closed_object_rejection_names_the_required_entry_path() {
        let tools = vec![ToolDef {
            name: "envelopes.put".into(),
            description: "put".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "envelope": {
                        "type": "object",
                        "properties": {},
                        "required": ["payload"],
                        "additionalProperties": false
                    }
                }
            }),
        }];

        let error = openrouter_tools("google/gemini-3.5-flash", &tools)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("/properties/envelope/required/0"),
            "error was: {error}"
        );
    }
}
