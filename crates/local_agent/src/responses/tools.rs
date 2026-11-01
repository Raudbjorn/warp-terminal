//! Converts neutral [`ToolDefinition`]s into flat Responses function tools.
//!
//! Tool schemas come from the host and from arbitrary MCP servers, so they
//! are normalized defensively before being sent: the API rejects schemas
//! whose nodes lack a `type`, objects without `properties`, and arrays
//! without `items`. Normalization only fills in what the schema already
//! implies (an untyped node with `properties` is an object) or states
//! equivalently (`nullable: true` becomes a `null` type member); it never
//! narrows what a schema accepts, except that an array with no `items` is
//! given the permissive [`any_value_schema`].

use serde_json::{Map, Value, json};

use crate::ToolDefinition;

/// Keywords whose presence tells us a schema node already constrains its
/// value, so it needs no [`any_value_schema`] stand-in.
const SELF_DESCRIBING_KEYWORDS: [&str; 8] = [
    "type", "$ref", "anyOf", "oneOf", "allOf", "not", "enum", "const",
];

/// Keywords allowed on a node of a strict-mode schema.
const STRICT_KEYWORDS: [&str; 8] = [
    "type",
    "description",
    "title",
    "enum",
    "properties",
    "required",
    "additionalProperties",
    "items",
];

/// Strict-mode size ceilings. The API has raised its published limits over
/// time; these are the older, smaller ones, so a big schema merely falls back
/// to non-strict instead of being rejected.
const MAX_STRICT_NESTING: usize = 5;
const MAX_STRICT_PROPERTIES: usize = 100;

/// Keywords from which a missing `type` can be inferred, per target type.
const OBJECT_HINTS: [&str; 5] = [
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "minProperties",
];
const ARRAY_HINTS: [&str; 4] = ["items", "prefixItems", "minItems", "maxItems"];
const STRING_HINTS: [&str; 4] = ["minLength", "maxLength", "pattern", "format"];
const NUMBER_HINTS: [&str; 5] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
];

/// Subschema-valued keywords that hold a single schema.
const SCHEMA_KEYWORDS: [&str; 3] = ["items", "additionalProperties", "not"];
/// Subschema-valued keywords that hold a list of schemas.
const SCHEMA_LIST_KEYWORDS: [&str; 4] = ["anyOf", "oneOf", "allOf", "prefixItems"];
/// Subschema-valued keywords that hold a name -> schema map.
const SCHEMA_MAP_KEYWORDS: [&str; 4] = ["properties", "patternProperties", "$defs", "definitions"];

/// Builds the flat `{"type": "function", ...}` tool the Responses API takes.
///
/// `strict` is on only for schemas that satisfy strict mode as written (every
/// property required, `additionalProperties: false`, nothing exotic); a tool
/// with optional parameters must stay non-strict or the API rejects it.
pub(super) fn function_tool(tool: &ToolDefinition) -> Value {
    let parameters = normalize_parameters(&tool.parameters);
    let strict = is_strict_compatible(&parameters);
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": parameters,
        "strict": strict,
    })
}

fn normalize_parameters(parameters: &Value) -> Value {
    let Some(schema) = parameters.as_object() else {
        log::warn!("tool parameters are not a JSON object; sending an empty object schema");
        return json!({ "type": "object", "properties": {} });
    };
    let mut schema = schema.clone();
    // A function's parameters are always an object, whatever the schema omits.
    schema
        .entry("type")
        .or_insert_with(|| Value::String("object".to_string()));
    normalize_node(&mut schema);
    Value::Object(schema)
}

fn normalize_node(node: &mut Map<String, Value>) {
    resolve_nullable(node);
    infer_type(node);
    if node.get("required").is_some_and(Value::is_null) {
        node.remove("required");
    }
    if has_type(node, "object") && !node.get("properties").is_some_and(Value::is_object) {
        node.insert("properties".to_string(), json!({}));
    }
    if has_type(node, "array") && !node.contains_key("items") {
        node.insert("items".to_string(), any_value_schema());
    }
    if !SELF_DESCRIBING_KEYWORDS
        .iter()
        .any(|keyword| node.contains_key(*keyword))
    {
        // Untyped and unconstrained: "any value", which the API cannot take.
        node.insert("anyOf".to_string(), any_value_union());
    }
    normalize_children(node);
}

fn normalize_children(node: &mut Map<String, Value>) {
    for keyword in SCHEMA_KEYWORDS {
        if let Some(Value::Object(child)) = node.get_mut(keyword) {
            normalize_node(child);
        }
    }
    for keyword in SCHEMA_LIST_KEYWORDS {
        if let Some(Value::Array(children)) = node.get_mut(keyword) {
            children
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .for_each(normalize_node);
        }
    }
    // `items` may also be the legacy tuple form: a list of schemas.
    if let Some(Value::Array(children)) = node.get_mut("items") {
        children
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .for_each(normalize_node);
    }
    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(Value::Object(children)) = node.get_mut(keyword) {
            children
                .values_mut()
                .filter_map(Value::as_object_mut)
                .for_each(normalize_node);
        }
    }
}

/// OpenAPI-style `nullable: true` is not JSON Schema; the equivalent is a
/// `null` member of the `type` list.
fn resolve_nullable(node: &mut Map<String, Value>) {
    let Some(nullable) = node.remove("nullable") else {
        return;
    };
    if nullable != Value::Bool(true) {
        return;
    }
    match node.get_mut("type") {
        Some(Value::String(kind)) if kind != "null" => {
            let kind = std::mem::take(kind);
            node.insert("type".to_string(), json!([kind, "null"]));
        }
        Some(Value::Array(kinds)) if !kinds.iter().any(|kind| kind == "null") => {
            kinds.push(Value::String("null".to_string()));
        }
        _ => {}
    }
}

fn infer_type(node: &mut Map<String, Value>) {
    if node.contains_key("type") || node.contains_key("$ref") {
        return;
    }
    let has_any = |keywords: &[&str]| keywords.iter().any(|keyword| node.contains_key(*keyword));
    let inferred = if has_any(&OBJECT_HINTS) {
        Some("object")
    } else if has_any(&ARRAY_HINTS) {
        Some("array")
    } else if has_any(&STRING_HINTS) {
        Some("string")
    } else if has_any(&NUMBER_HINTS) {
        Some("number")
    } else {
        literal_type(node.get("const").map(std::slice::from_ref).or_else(|| {
            node.get("enum")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
        }))
    };
    if let Some(kind) = inferred {
        node.insert("type".to_string(), Value::String(kind.to_string()));
    }
}

/// The single JSON type shared by every literal of an `enum`/`const`.
fn literal_type(literals: Option<&[Value]>) -> Option<&'static str> {
    let kind_of = |value: &Value| match value {
        Value::String(_) => Some("string"),
        Value::Bool(_) => Some("boolean"),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some("integer"),
        Value::Number(_) => Some("number"),
        _ => None,
    };
    let mut kinds = literals?.iter().map(kind_of);
    let first = kinds.next()??;
    kinds.all(|kind| kind == Some(first)).then_some(first)
}

fn has_type(node: &Map<String, Value>, kind: &str) -> bool {
    match node.get("type") {
        Some(Value::String(actual)) => actual == kind,
        Some(Value::Array(actual)) => actual.iter().any(|actual| actual == kind),
        _ => false,
    }
}

/// Stand-in for a schema that accepts any value, expressed with the explicit
/// types the API insists on.
fn any_value_schema() -> Value {
    json!({ "anyOf": any_value_union() })
}

fn any_value_union() -> Value {
    json!([
        { "type": "string" },
        { "type": "number" },
        { "type": "boolean" },
        { "type": "object", "properties": {} },
        { "type": "array", "items": { "type": "string" } },
    ])
}

/// Whether a (normalized) schema already meets every strict-mode rule, so
/// that `strict: true` cannot make the API reject the tool.
fn is_strict_compatible(parameters: &Value) -> bool {
    let mut properties_seen = 0;
    parameters.as_object().is_some_and(|root| {
        has_type(root, "object") && is_strict_node(root, 1, &mut properties_seen)
    })
}

fn is_strict_node(node: &Map<String, Value>, depth: usize, properties_seen: &mut usize) -> bool {
    if depth > MAX_STRICT_NESTING
        || node
            .keys()
            .any(|key| !STRICT_KEYWORDS.contains(&key.as_str()))
    {
        return false;
    }
    match node.get("type").and_then(Value::as_str) {
        Some("string" | "number" | "integer" | "boolean") => true,
        Some("array") => node
            .get("items")
            .and_then(Value::as_object)
            .is_some_and(|items| is_strict_node(items, depth + 1, properties_seen)),
        Some("object") => is_strict_object(node, depth, properties_seen),
        _ => false,
    }
}

fn is_strict_object(node: &Map<String, Value>, depth: usize, properties_seen: &mut usize) -> bool {
    let Some(properties) = node.get("properties").and_then(Value::as_object) else {
        return false;
    };
    if node.get("additionalProperties") != Some(&Value::Bool(false)) {
        return false;
    }
    *properties_seen += properties.len();
    if *properties_seen > MAX_STRICT_PROPERTIES {
        return false;
    }
    // Strict mode wants an explicit `required` list even when it is empty;
    // a schema without one is left non-strict rather than risk a rejection.
    let Some(required) = node.get("required").and_then(Value::as_array) else {
        return false;
    };
    required.len() == properties.len()
        && properties
            .keys()
            .all(|name| required.iter().any(|required| required == name))
        && properties.values().all(|property| {
            property
                .as_object()
                .is_some_and(|property| is_strict_node(property, depth + 1, properties_seen))
        })
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
