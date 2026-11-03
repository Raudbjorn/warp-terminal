//! Converts neutral [`ToolDefinition`]s into flat Responses function tools.
//!
//! Tool schemas come from the host and from arbitrary MCP servers, so they
//! are normalized defensively before being sent: the API rejects schemas
//! whose nodes lack a `type`, objects without `properties`, and arrays
//! without `items`. Normalization only fills in what the schema already
//! implies (an untyped node with `properties` is an object) or states
//! equivalently (`nullable: true` becomes a `null` type member); it never
//! narrows what a schema accepts. A node that constrains nothing and an
//! array with no `items` are both given the permissive
//! [`any_value_schema`], which accepts every JSON value.

use serde_json::{Map, Value, json};

use crate::ToolDefinition;

/// Keywords whose presence tells us a schema node already constrains its
/// value, so it needs no [`any_value_schema`] stand-in.
const SELF_DESCRIBING_KEYWORDS: [&str; 8] = [
    "type", "$ref", "anyOf", "oneOf", "allOf", "not", "enum", "const",
];

/// Keywords that already spell out the values a node accepts. A sibling
/// type-specific keyword (`maxLength`, `minimum`, ...) only constrains one
/// type, so inferring a type from it would narrow the node below what the
/// schema itself accepts.
const UNION_KEYWORDS: [&str; 4] = ["anyOf", "oneOf", "allOf", "not"];

/// The JSON instance types a node accepts when the schema puts no
/// constraint on its value. `number` covers the integers.
const ANY_VALUE_TYPES: [&str; 6] = ["string", "number", "boolean", "object", "array", "null"];

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
    // The type is inferred first: `resolve_nullable` can only add a `null`
    // member to a type that is already there, so a node whose type still has
    // to be inferred would lose its nullability.
    infer_type(node);
    resolve_nullable(node);
    if node.get("required").is_some_and(Value::is_null) {
        node.remove("required");
    }
    let any_value = is_any_value_schema(node);
    if has_type(node, "object")
        && !any_value
        && !node.get("properties").is_some_and(Value::is_object)
    {
        node.insert("properties".to_string(), json!({}));
    }
    if has_type(node, "array") && !any_value && !node.contains_key("items") {
        node.insert("items".to_string(), any_value_schema());
    }
    if !SELF_DESCRIBING_KEYWORDS
        .iter()
        .any(|keyword| node.contains_key(*keyword))
    {
        // Untyped and unconstrained: "any value", which the API cannot take
        // without an explicit type.
        node.insert("type".to_string(), json!(ANY_VALUE_TYPES));
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
    // A node that already states which values it accepts must not be
    // narrowed: a sibling type-specific keyword (`maxLength`, `minimum`, ...)
    // only constrains one kind, so inferring a type from it would drop the
    // alternatives the node itself allows.
    if UNION_KEYWORDS
        .iter()
        .any(|keyword| node.contains_key(*keyword))
    {
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
///
/// Every JSON instance type is listed, so the stand-in accepts exactly what
/// the original schema accepted: `null`, numbers, booleans, objects with any
/// properties, and arrays with any contents.
fn any_value_schema() -> Value {
    json!({ "type": ANY_VALUE_TYPES })
}

/// Whether the node is the permissive any-value stand-in, which is the only
/// node whose type names several kinds at once.
fn is_any_value_schema(node: &Map<String, Value>) -> bool {
    let Some(Value::Array(types)) = node.get("type") else {
        return false;
    };
    types.len() == ANY_VALUE_TYPES.len()
        && ANY_VALUE_TYPES.iter().all(|expected| {
            types
                .iter()
                .any(|actual| actual.as_str() == Some(*expected))
        })
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
