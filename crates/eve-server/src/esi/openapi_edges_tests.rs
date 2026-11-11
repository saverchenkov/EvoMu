//! Edge-case and full-branch unit tests for `esi::openapi`.

#![forbid(unsafe_code)]

use super::*;
use serde_json::json;

fn mandatory_headers() -> HashMap<String, String> {
    HashMap::from([
        ("x-esi-error-limit-remain".into(), "100".into()),
        ("x-esi-error-limit-reset".into(), "60".into()),
        ("x-esi-request-id".into(), "req-edges".into()),
    ])
}

#[test]
fn pinned_spec_and_custom_spec_parsing_matching_and_literal_precedence() {
    let pinned = OpenApiSpec::pinned();
    assert!(!pinned.compatibility_dates.is_empty());
    assert!(pinned.operations.len() >= 180);
    assert!(!pinned.tags.is_empty());

    // Legacy operation matches cleanly
    assert!(pinned
        .match_operation("GET", "/v1/characters/90000001/bookmarks")
        .is_some());
    assert!(pinned.path_exists("/opportunities/groups"));

    let spec_json = json!({
        "paths": {
            "/not-an-object": "ignored",
            "/items/{item_id}": {
                "get": {
                    "operationId": "GetItemById",
                    "parameters": [
                        {"name": "", "in": "query", "schema": {"type": "string"}},
                        {"name": "item_id", "in": "path", "required": true, "schema": {"type": "integer"}}
                    ],
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}}
                                }
                            }
                        },
                        "204": {},
                        "default": {
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object", "required": ["error"], "properties": {"error": {"type": "string"}}}
                                }
                            }
                        }
                    }
                }
            },
            "/items/special": {
                "get": {
                    "operationId": "GetSpecialItem",
                    "responses": {
                        "200": {}
                    }
                }
            }
        }
    });
    let spec = OpenApiSpec::from_json_str(&spec_json.to_string()).unwrap();

    // Literal score beats parameterized route regardless of map iteration order
    let special = spec.match_operation("GET", "/items/special").unwrap();
    assert_eq!(special.operation.operation_id, "GetSpecialItem");
    let param_match = spec.match_operation("GET", "/items/42").unwrap();
    assert_eq!(param_match.operation.operation_id, "GetItemById");
    // Method mismatch, segment len mismatch, literal mismatch
    assert!(spec.match_operation("POST", "/items/42").is_none());
    assert!(spec.match_operation("GET", "/items/42/extra").is_none());
    assert!(spec.match_operation("GET", "/other/42").is_none());

    let hdrs = mandatory_headers();
    // 204 empty passes, non-empty fails
    assert!(spec
        .validate_response("GET", "/items/42", 204, &hdrs, b"")
        .is_ok());
    assert!(spec
        .validate_response("GET", "/items/42", 204, &hdrs, b"{}")
        .unwrap_err()
        .contains("204 No Content must have an empty body"));

    // Undefined 2xx status code fails; >= 400 falls back to "default"
    assert!(spec
        .validate_response("GET", "/items/42", 201, &hdrs, b"{}")
        .unwrap_err()
        .contains("status code 201 is not defined"));
    assert!(spec
        .validate_response("GET", "/items/42", 404, &hdrs, br#"{"error":"nope"}"#)
        .is_ok());
    assert!(spec
        .validate_response("GET", "/items/42", 500, &hdrs, br#"{"bad":1}"#)
        .is_err());

    // Invalid JSON body fails
    assert!(spec
        .validate_response("GET", "/items/42", 200, &hdrs, b"not-json")
        .unwrap_err()
        .contains("not valid JSON"));
    assert!(spec
        .validate_response("GET", "/items/42", 200, &hdrs, br#"{"id":1}"#)
        .is_ok());
}

#[test]
fn validate_value_against_schema_covers_all_scalar_type_errors_and_edge_branches() {
    // null
    assert!(validate_value_against_schema(&Value::Null, &json!({"type": "null"}), "r").is_ok());
    assert!(validate_value_against_schema(&json!(1), &json!({"type": "null"}), "r")
        .unwrap_err()
        .contains("expected null"));

    // object non-object, null field skipped, additionalProperties: true
    assert!(validate_value_against_schema(&json!(1), &json!({"type": "object"}), "r")
        .unwrap_err()
        .contains("expected object"));
    let obj_with_null_and_add = json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "additionalProperties": true
    });
    assert!(validate_value_against_schema(
        &json!({"name": null, "extra": 123}),
        &obj_with_null_and_add,
        "r"
    )
    .is_ok());

    // array non-array
    assert!(validate_value_against_schema(&json!("not-arr"), &json!({"type": "array"}), "r")
        .unwrap_err()
        .contains("expected array"));

    // integer non-int, min, max
    let int_schema = json!({"type": "integer", "minimum": 10, "maximum": 20});
    assert!(validate_value_against_schema(&json!("s"), &int_schema, "r")
        .unwrap_err()
        .contains("expected integer"));
    assert!(validate_value_against_schema(&json!(5), &int_schema, "r")
        .unwrap_err()
        .contains("below minimum"));
    assert!(validate_value_against_schema(&json!(25), &int_schema, "r")
        .unwrap_err()
        .contains("exceeds maximum"));
    assert!(validate_value_against_schema(&json!(15), &int_schema, "r").is_ok());

    // number non-number
    assert!(validate_value_against_schema(&json!("s"), &json!({"type": "number"}), "r")
        .unwrap_err()
        .contains("expected number"));

    // string non-string, date-time with + offset, date wrong length
    assert!(validate_value_against_schema(&json!(1), &json!({"type": "string"}), "r")
        .unwrap_err()
        .contains("expected string"));
    assert!(validate_value_against_schema(
        &json!("2026-01-01T12:00:00+00:00"),
        &json!({"type": "string", "format": "date-time"}),
        "r"
    )
    .is_ok());
    assert!(validate_value_against_schema(
        &json!("2026-01"),
        &json!({"type": "string", "format": "date"}),
        "r"
    )
    .is_err());

    // boolean
    assert!(validate_value_against_schema(&json!(true), &json!({"type": "boolean"}), "r").is_ok());
    assert!(validate_value_against_schema(&json!(1), &json!({"type": "boolean"}), "r")
        .unwrap_err()
        .contains("expected boolean"));
}
