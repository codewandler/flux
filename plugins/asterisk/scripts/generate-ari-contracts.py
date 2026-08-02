#!/usr/bin/env python3
"""Compile Asterisk's legacy ARI Swagger documents into runtime-free Rust facts."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SOURCE = ROOT / "specs" / "ari-22.10.1" / "api-docs"
DEFAULT_OUTPUT = ROOT / "src" / "ari_generated.rs"
PRIMITIVES = {
    "string": {"type": "string"},
    "Date": {"type": "string", "format": "date-time"},
    "int": {"type": "integer", "format": "int32"},
    "long": {"type": "integer", "format": "int64"},
    "double": {"type": "number", "format": "double"},
    "boolean": {"type": "boolean"},
    "object": {"type": "object"},
    "containers": {"type": "object"},
}

# Reviewed against the 22.10.1 resource documents. Mutations in these families change PBX control,
# live calls/media, subscriptions, or external messages. Device-state/mailbox mutations remain the
# ordinary medium write class; DELETE is handled separately as destructive for every resource.
HIGH_RISK_MUTATION_RESOURCES = {
    "applications",
    "asterisk",
    "bridges",
    "channels",
    "endpoints",
    "events",
    "playbacks",
    "recordings",
}


class SpecError(ValueError):
    pass


def require(value: Any, kind: type, where: str) -> Any:
    if not isinstance(value, kind):
        raise SpecError(f"{where}: expected {kind.__name__}")
    return value


def required_string(value: dict[str, Any], key: str, where: str) -> str:
    result = value.get(key)
    if not isinstance(result, str) or not result:
        raise SpecError(f"{where}.{key}: expected non-empty string")
    return result


def schema_for_type(type_name: str, models: set[str], where: str) -> dict[str, Any]:
    if type_name.startswith("List[") and type_name.endswith("]"):
        return {
            "type": "array",
            "items": schema_for_type(type_name[5:-1], models, where),
        }
    if type_name in PRIMITIVES:
        return dict(PRIMITIVES[type_name])
    if type_name in models:
        return {"$ref": f"#/$defs/{type_name}"}
    raise SpecError(f"{where}: unknown type `{type_name}`")


def enum_values(value: dict[str, Any], where: str) -> list[Any]:
    allowable = value.get("allowableValues")
    if allowable is None:
        return []
    allowable = require(allowable, dict, f"{where}.allowableValues")
    if allowable.get("valueType") == "RANGE":
        for key in ("min", "max"):
            if key in allowable and not isinstance(allowable[key], (int, float)):
                raise SpecError(f"{where}.allowableValues.{key}: expected number")
        return []
    if allowable.get("valueType") != "LIST":
        raise SpecError(f"{where}.allowableValues.valueType: unsupported value")
    return require(allowable.get("values"), list, f"{where}.allowableValues.values")


def apply_allowable(schema: dict[str, Any], value: dict[str, Any], where: str) -> list[Any]:
    values = enum_values(value, where)
    allowable = value.get("allowableValues")
    if isinstance(allowable, dict) and allowable.get("valueType") == "RANGE":
        if "min" in allowable:
            schema["minimum"] = allowable["min"]
        if "max" in allowable:
            schema["maximum"] = allowable["max"]
    return values


def resource_id(stem: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", stem).lower()


def safety(method: str, resource: str, nickname: str) -> dict[str, Any]:
    """The reviewed safety table, expressed exhaustively for every method class.

    All ARI mutations affect a live PBX, so POST/PUT are deliberately high risk rather than the
    ordinary medium write default. DELETE is separately destructive. A small set also carries the
    externally-visible/cost consequence that its route actually performs.
    """
    if method == "GET":
        return {
            "effects": ["read", "network"],
            "risk": "low",
            "idempotency": "idempotent",
            "semantic_effects": ["read", "network"],
        }
    if method == "DELETE":
        return {
            "effects": ["write", "network"],
            "risk": "destructive",
            "idempotency": "idempotent",
            "semantic_effects": ["delete", "network"],
        }
    if method not in {"POST", "PUT"}:
        raise SpecError(f"{resource}.{nickname}: unsupported HTTP method `{method}`")

    semantic = ["write_db", "network"]
    externally_visible = {
        ("channels", "originate"),
        ("channels", "originateWithId"),
        ("channels", "dial"),
        ("channels", "create"),
        ("channels", "externalMedia"),
        ("channels", "snoopChannel"),
        ("channels", "snoopChannelWithId"),
        ("endpoints", "sendMessage"),
        ("endpoints", "sendMessageToEndpoint"),
        ("endpoints", "refer"),
        ("events", "userEvent"),
    }
    human_visible_resources = {"playbacks", "recordings", "sounds"}
    if (resource, nickname) in externally_visible:
        semantic.append("send_external")
    if resource in human_visible_resources:
        semantic.append("human_visible")
    if resource == "channels" and nickname in {"originate", "originateWithId", "dial"}:
        semantic.append("money")
    return {
        "effects": ["write", "network"],
        "risk": "high" if resource in HIGH_RISK_MUTATION_RESOURCES else "medium",
        "idempotency": "idempotent" if method == "PUT" else "non_idempotent",
        "semantic_effects": semantic,
    }


def load_documents(source: Path) -> list[tuple[str, dict[str, Any]]]:
    if not source.is_dir():
        raise SpecError(f"source directory does not exist: {source}")
    paths = sorted(source.glob("*.json"))
    if len(paths) != 11:
        raise SpecError(f"{source}: expected 11 API documents, found {len(paths)}")
    documents = []
    for path in paths:
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise SpecError(f"{path}: {error}") from error
        document = require(raw, dict, str(path))
        if document.get("swaggerVersion") not in {"1.1", "1.2"}:
            raise SpecError(f"{path}: unsupported swaggerVersion")
        if document.get("basePath") != "http://localhost:8088/ari":
            raise SpecError(f"{path}: unexpected basePath")
        require(document.get("apis"), list, f"{path}.apis")
        require(document.get("models"), dict, f"{path}.models")
        documents.append((path.stem, document))
    return documents


def compile_contracts(source: Path) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    documents = load_documents(source)
    model_names: set[str] = set()
    raw_models: dict[str, tuple[dict[str, Any], str]] = {}
    for stem, document in documents:
        for name, model in document["models"].items():
            where = f"{stem}.models.{name}"
            required_string(require(model, dict, where), "id", where)
            if name in raw_models:
                raise SpecError(f"duplicate model `{name}`")
            model_names.add(name)
            raw_models[name] = (model, where)
    if len(model_names) != 85:
        raise SpecError(f"expected 85 unique models, found {len(model_names)}")

    model_schemas: dict[str, Any] = {}
    for name in sorted(raw_models):
        model, where = raw_models[name]
        properties: dict[str, Any] = {}
        required_properties: list[str] = []
        raw_properties = require(model.get("properties"), dict, f"{where}.properties")
        for property_name in sorted(raw_properties):
            prop = require(raw_properties[property_name], dict, f"{where}.properties.{property_name}")
            prop_where = f"{where}.properties.{property_name}"
            prop_schema = schema_for_type(required_string(prop, "type", prop_where), model_names, prop_where)
            if isinstance(prop.get("description"), str):
                prop_schema["description"] = prop["description"]
            values = apply_allowable(prop_schema, prop, prop_where)
            if values:
                prop_schema["enum"] = values
            required = prop.get("required", False)
            if not isinstance(required, bool):
                raise SpecError(f"{prop_where}.required: expected boolean")
            if required:
                required_properties.append(property_name)
            properties[property_name] = prop_schema
        schema: dict[str, Any] = {
            "type": "object",
            "description": required_string(model, "description", where),
            "properties": properties,
        }
        if required_properties:
            schema["required"] = required_properties
        subtypes = model.get("subTypes", [])
        if not isinstance(subtypes, list) or not all(isinstance(value, str) for value in subtypes):
            raise SpecError(f"{where}.subTypes: expected string array")
        missing_subtypes = sorted(set(subtypes) - model_names)
        if missing_subtypes:
            raise SpecError(f"{where}.subTypes: unknown models {missing_subtypes}")
        if subtypes:
            # Swagger inheritance is inclusive polymorphism. `anyOf` is intentional: the legacy
            # subtype objects are open and overlap, so `oneOf` would reject valid events that match
            # more than one subtype schema.
            schema["anyOf"] = [{"$ref": f"#/$defs/{subtype}"} for subtype in subtypes]
        if "discriminator" in model:
            schema["x-ari-discriminator"] = required_string(model, "discriminator", where)
        model_schemas[name] = schema

    operations: list[dict[str, Any]] = []
    identities: set[str] = set()
    for stem, document in documents:
        resource = resource_id(stem)
        for api_index, api in enumerate(document["apis"]):
            api = require(api, dict, f"{stem}.apis[{api_index}]")
            path = required_string(api, "path", f"{stem}.apis[{api_index}]")
            api_description = api.get("description", "")
            if not isinstance(api_description, str):
                raise SpecError(f"{stem}.apis[{api_index}].description: expected string")
            raw_operations = require(api.get("operations"), list, f"{stem}.apis[{api_index}].operations")
            for operation_index, raw_operation in enumerate(raw_operations):
                where = f"{stem}.apis[{api_index}].operations[{operation_index}]"
                operation = require(raw_operation, dict, where)
                method = required_string(operation, "httpMethod", where)
                nickname = required_string(operation, "nickname", where)
                response_class = required_string(operation, "responseClass", where)
                websocket = operation.get("upgrade") == "websocket"
                identity = f"asterisk.ari.{resource}.{nickname}"
                if identity in identities:
                    raise SpecError(f"duplicate operation identity `{identity}`")
                identities.add(identity)
                parameters: list[dict[str, Any]] = []
                properties: dict[str, Any] = {}
                required_parameters: list[str] = []
                body_count = 0
                for parameter_index, raw_parameter in enumerate(operation.get("parameters", [])):
                    parameter_where = f"{where}.parameters[{parameter_index}]"
                    parameter = require(raw_parameter, dict, parameter_where)
                    name = required_string(parameter, "name", parameter_where)
                    placement = required_string(parameter, "paramType", parameter_where)
                    if placement not in {"path", "query", "body"}:
                        raise SpecError(f"{parameter_where}.paramType: unsupported `{placement}`")
                    if placement == "body":
                        body_count += 1
                    data_type = required_string(parameter, "dataType", parameter_where)
                    required = parameter.get("required", False)
                    multiple = parameter.get("allowMultiple", False)
                    if not isinstance(required, bool) or not isinstance(multiple, bool):
                        raise SpecError(f"{parameter_where}: required/allowMultiple must be booleans")
                    property_schema = schema_for_type(data_type, model_names, parameter_where)
                    if multiple:
                        property_schema = {"type": "array", "items": property_schema}
                    values = apply_allowable(
                        property_schema["items"] if multiple else property_schema,
                        parameter,
                        parameter_where,
                    )
                    description = parameter.get("description", "")
                    if not isinstance(description, str):
                        raise SpecError(f"{parameter_where}.description: expected string")
                    if description:
                        property_schema["description"] = description
                    if values:
                        if multiple:
                            property_schema["items"]["enum"] = values
                        else:
                            property_schema["enum"] = values
                    if "defaultValue" in parameter:
                        property_schema["default"] = parameter["defaultValue"]
                    property_schema["x-ari-placement"] = placement
                    property_schema["x-ari-allow-multiple"] = multiple
                    properties[name] = property_schema
                    if required:
                        required_parameters.append(name)
                    parameters.append(
                        {
                            "name": name,
                            "description": description,
                            "placement": placement,
                            "required": parameter.get("required"),
                            "allow_multiple": parameter.get("allowMultiple"),
                            "data_type": data_type,
                            "enum_values": values,
                            "default_value": parameter.get("defaultValue"),
                            "allowable_values": parameter.get("allowableValues"),
                        }
                    )
                if body_count > 1:
                    raise SpecError(f"{where}: more than one body parameter")
                input_schema: dict[str, Any] = {
                    "type": "object",
                    "additionalProperties": False,
                    "properties": properties,
                }
                if required_parameters:
                    input_schema["required"] = required_parameters

                if response_class == "void":
                    output_schema = {
                        "type": "object",
                        "additionalProperties": False,
                        "properties": {"status": {"type": "integer"}},
                        "required": ["status"],
                    }
                    response_kind = "void"
                elif response_class == "binary":
                    output_schema = {
                        "type": "object",
                        "additionalProperties": False,
                        "properties": {
                            "blob_ref": {"type": "string"},
                            "size": {"type": "integer", "minimum": 0},
                            "sha256": {"type": "string"},
                        },
                        "required": ["blob_ref", "size", "sha256"],
                    }
                    response_kind = "binary"
                else:
                    # Validate the response reference now; runtime assembles the schema from the
                    # one generated model table so 85 `$defs` are not duplicated 51 times.
                    schema_for_type(response_class, model_names, where)
                    output_schema = None
                    response_kind = "json"

                source_fact = {
                    "resource": stem,
                    "nickname": nickname,
                    "method": method,
                    "path": path,
                    "websocket": websocket,
                    "response_class": response_class,
                    "parameters": parameters,
                }
                operations.append(
                    {
                        "name": identity,
                        "resource": resource,
                        "nickname": nickname,
                        "method": method,
                        "path": path,
                        "websocket": websocket,
                        "description": required_string(operation, "summary", where),
                        "resource_description": api_description,
                        "response_class": response_class,
                        "response_kind": response_kind,
                        "parameters": parameters,
                        "input_schema": input_schema,
                        **({"output_schema": output_schema} if output_schema is not None else {}),
                        "source": source_fact,
                        **safety(method, resource, nickname),
                    }
                )
    operations.sort(key=lambda operation: operation["name"])
    if len(operations) != 109:
        raise SpecError(f"expected 109 operations, found {len(operations)}")
    if sum(not operation["websocket"] for operation in operations) != 108:
        raise SpecError("expected exactly 108 REST operations and one WebSocket operation")
    return operations, model_schemas


def raw_string(value: Any) -> str:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    delimiter = "####"
    if f'"{delimiter}' in encoded:
        raise SpecError("generated JSON collides with Rust raw-string delimiter")
    return f'r{delimiter}"{encoded}"{delimiter}'


def render(source: Path) -> str:
    operations, model_schemas = compile_contracts(source)
    source_operations = [operation["source"] for operation in operations]
    return """// @generated by scripts/generate-ari-contracts.py; do not edit by hand.\n\
// The vendored Swagger is a development input only. Production embeds these compiled facts.\n\n\
pub(crate) const ARI_OPERATIONS_JSON: &str = %s;\n\n\
#[cfg(test)]\n\
pub(crate) const ARI_SOURCE_OPERATIONS_JSON: &str = %s;\n\n\
pub(crate) const ARI_MODEL_SCHEMAS_JSON: &str = %s;\n""" % (
        raw_string(operations),
        raw_string(source_operations),
        raw_string(model_schemas),
    )


def write_atomic(output: Path, content: str) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{output.name}.", dir=output.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary_name, output)
    except BaseException:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-dir", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        content = render(args.source_dir)
        if args.check:
            try:
                current = args.output.read_text(encoding="utf-8")
            except OSError as error:
                raise SpecError(f"{args.output}: {error}") from error
            if current != content:
                raise SpecError(f"{args.output}: generated output is stale")
        else:
            write_atomic(args.output, content)
    except SpecError as error:
        print(f"generate-ari-contracts: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
