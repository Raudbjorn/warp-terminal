use super::*;

fn tool(parameters: Value) -> ToolDefinition {
    ToolDefinition {
        name: "t".to_string(),
        description: "d".to_string(),
        parameters,
    }
}

fn parameters_of(parameters: Value) -> Value {
    function_tool(&tool(parameters))["parameters"].clone()
}

fn strict_of(parameters: Value) -> bool {
    function_tool(&tool(parameters))["strict"] == true
}

#[test]
fn builds_a_flat_function_tool() {
    let parameters = json!({
        "type": "object",
        "properties": { "command": { "type": "string", "description": "what to run" } },
        "required": ["command"],
        "additionalProperties": false,
    });

    let payload = function_tool(&tool(parameters.clone()));

    assert_eq!(
        payload,
        json!({
            "type": "function",
            "name": "t",
            "description": "d",
            "parameters": parameters,
            "strict": true,
        })
    );
}

#[test]
fn a_well_formed_schema_is_left_untouched() {
    let parameters = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "file" },
            "mode": { "type": "string", "enum": ["r", "w"] },
            "lines": { "type": ["integer", "null"], "minimum": 1 },
            "tags": { "type": "array", "items": { "type": "string" } },
            "target": { "$ref": "#/$defs/target" },
            "either": { "anyOf": [{ "type": "string" }, { "type": "number" }] },
        },
        "required": ["path"],
        "$defs": { "target": { "type": "object", "properties": { "id": { "type": "string" } } } },
    });

    assert_eq!(parameters_of(parameters.clone()), parameters);
}

#[test]
fn normalization_is_idempotent() {
    let messy = json!({
        "properties": {
            "a": {},
            "b": { "type": "array" },
            "c": { "type": "string", "nullable": true },
            "d": { "type": "object" },
        },
        "required": null,
    });

    let once = parameters_of(messy);

    assert_eq!(parameters_of(once.clone()), once);
}

#[test]
fn fills_in_the_root_type_and_properties_an_mcp_tool_omits() {
    for (input, expected) in [
        (json!({}), json!({ "type": "object", "properties": {} })),
        (
            json!({ "type": "object" }),
            json!({ "type": "object", "properties": {} }),
        ),
        (
            json!({ "properties": { "q": { "type": "string" } } }),
            json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        ),
        (json!(null), json!({ "type": "object", "properties": {} })),
        (json!(true), json!({ "type": "object", "properties": {} })),
    ] {
        assert_eq!(parameters_of(input.clone()), expected, "input {input}");
    }
}

#[test]
fn infers_missing_types_from_the_keywords_that_imply_them() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": {
            "obj": { "properties": { "x": { "type": "string" } } },
            "list": { "items": { "type": "string" } },
            "text": { "format": "uri" },
            "count": { "minimum": 0 },
            "choice": { "enum": ["a", "b"] },
            "level": { "enum": [1, 2] },
            "fixed": { "const": true },
        },
    }));

    let types: Vec<_> = ["obj", "list", "text", "count", "choice", "level", "fixed"]
        .iter()
        .map(|name| parameters["properties"][name]["type"].clone())
        .collect();
    assert_eq!(
        types,
        vec![
            json!("object"),
            json!("array"),
            json!("string"),
            json!("number"),
            json!("string"),
            json!("integer"),
            json!("boolean"),
        ]
    );
}

#[test]
fn leaves_mixed_enums_and_refs_without_a_type() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": {
            "mixed": { "enum": ["a", 1] },
            "reference": { "$ref": "#/$defs/x" },
            "union": { "oneOf": [{ "type": "string" }, { "type": "null" }] },
        },
    }));

    assert!(parameters["properties"]["mixed"].get("type").is_none());
    assert!(parameters["properties"]["reference"].get("type").is_none());
    assert!(parameters["properties"]["union"].get("type").is_none());
    assert!(parameters["properties"]["mixed"].get("anyOf").is_none());
}

#[test]
fn gives_nested_objects_and_arrays_what_the_api_requires() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": {
            "opts": { "type": "object", "additionalProperties": { "type": "string" } },
            "list": { "type": "array" },
            "nested": { "type": "array", "items": { "type": "object" } },
        },
    }));

    assert_eq!(parameters["properties"]["opts"]["properties"], json!({}));
    assert_eq!(
        parameters["properties"]["opts"]["additionalProperties"],
        json!({ "type": "string" })
    );
    assert_eq!(
        parameters["properties"]["list"]["items"],
        any_value_schema()
    );
    assert_eq!(
        parameters["properties"]["nested"]["items"],
        json!({ "type": "object", "properties": {} })
    );
}

#[test]
fn an_unconstrained_property_accepts_any_value_but_keeps_its_description() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": { "payload": { "description": "anything" } },
    }));

    assert_eq!(
        parameters["properties"]["payload"],
        json!({ "description": "anything", "anyOf": any_value_union() })
    );
}

#[test]
fn rewrites_nullable_into_a_null_type_member() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": {
            "a": { "type": "string", "nullable": true },
            "b": { "type": ["string", "integer"], "nullable": true },
            "c": { "type": "string", "nullable": false },
            "d": { "type": "string", "nullable": true, "enum": ["x"] },
        },
    }));

    let properties = &parameters["properties"];
    assert_eq!(properties["a"], json!({ "type": ["string", "null"] }));
    assert_eq!(
        properties["b"],
        json!({ "type": ["string", "integer", "null"] })
    );
    assert_eq!(properties["c"], json!({ "type": "string" }));
    assert_eq!(properties["d"]["type"], json!(["string", "null"]));
}

#[test]
fn drops_a_null_required_list() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": null,
    }));

    assert!(parameters.get("required").is_none());
}

#[test]
fn normalizes_subschemas_everywhere_they_can_appear() {
    let parameters = parameters_of(json!({
        "type": "object",
        "properties": {
            "u": { "anyOf": [{ "type": "object" }, { "type": "array" }] },
        },
        "$defs": { "d": { "type": "object" } },
        "definitions": { "e": { "type": "array" } },
    }));

    assert_eq!(
        parameters["properties"]["u"]["anyOf"],
        json!([
            { "type": "object", "properties": {} },
            { "type": "array", "items": any_value_schema() },
        ])
    );
    assert_eq!(parameters["$defs"]["d"]["properties"], json!({}));
    assert_eq!(parameters["definitions"]["e"]["items"], any_value_schema());
}

#[test]
fn strict_requires_every_property_to_be_required() {
    assert!(strict_of(json!({
        "type": "object",
        "properties": { "a": { "type": "string" }, "b": { "type": "integer" } },
        "required": ["a", "b"],
        "additionalProperties": false,
    })));
    assert!(!strict_of(json!({
        "type": "object",
        "properties": { "a": { "type": "string" }, "b": { "type": "integer" } },
        "required": ["a"],
        "additionalProperties": false,
    })));
}

#[test]
fn strict_requires_additional_properties_to_be_forbidden() {
    assert!(!strict_of(json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": ["a"],
    })));
    assert!(!strict_of(json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": ["a"],
        "additionalProperties": true,
    })));
}

#[test]
fn strict_checks_nested_objects_and_arrays() {
    let nested_ok = json!({
        "type": "object",
        "properties": {
            "inner": {
                "type": "object",
                "properties": { "x": { "type": "string", "enum": ["a"] } },
                "required": ["x"],
                "additionalProperties": false,
            },
            "list": { "type": "array", "items": { "type": "number" } },
        },
        "required": ["inner", "list"],
        "additionalProperties": false,
    });
    assert!(strict_of(nested_ok));

    let nested_open = json!({
        "type": "object",
        "properties": {
            "inner": {
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x"],
            },
        },
        "required": ["inner"],
        "additionalProperties": false,
    });
    assert!(!strict_of(nested_open));
}

#[test]
fn strict_is_off_for_keywords_strict_mode_may_reject() {
    for property in [
        json!({ "type": "string", "default": "x" }),
        json!({ "type": ["string", "null"] }),
        json!({ "$ref": "#/$defs/x" }),
        json!({ "anyOf": [{ "type": "string" }] }),
        json!({ "type": "integer", "minimum": 1 }),
    ] {
        assert!(
            !strict_of(json!({
                "type": "object",
                "properties": { "a": property.clone() },
                "required": ["a"],
                "additionalProperties": false,
            })),
            "property {property}"
        );
    }
}

#[test]
fn a_tool_without_parameters_is_not_strict() {
    // `additionalProperties` is not ours to add: it would narrow the schema.
    assert!(!strict_of(json!({ "type": "object", "properties": {} })));
    assert!(strict_of(json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })));
}
