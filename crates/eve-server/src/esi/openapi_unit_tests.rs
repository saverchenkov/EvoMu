//! Unit and mutation-killing tests for `esi::openapi`.

use super::*;
use serde_json::json;

#[test]
fn strip_version_prefix_and_normalize_canonical_path_cover_all_prefixes() {
    assert_eq!(normalize_canonical_path(""), "/");
    assert_eq!(normalize_canonical_path("   "), "/");
    assert_eq!(normalize_canonical_path("/"), "/");
    assert_eq!(normalize_canonical_path("status/"), "/status");
    assert_eq!(normalize_canonical_path("/v1/status/"), "/v1/status");

    for &vp in VERSION_PREFIXES {
        assert_eq!(
            OpenApiSpec::strip_version_prefix(vp),
            (Some(vp.to_string()), "/".to_string())
        );
        let with_sub = format!("{vp}/universe/types/34/?datasource=tranquility");
        assert_eq!(
            OpenApiSpec::strip_version_prefix(&with_sub),
            (
                Some(vp.to_string()),
                "/universe/types/34".to_string()
            )
        );
    }

    assert_eq!(
        OpenApiSpec::strip_version_prefix(""),
        (None, "/".to_string())
    );
    assert_eq!(
        OpenApiSpec::strip_version_prefix("/status?datasource=tranquility"),
        (None, "/status".to_string())
    );
}

#[test]
fn from_json_str_and_resolve_refs_handle_errors_and_depth_limit() {
    assert!(OpenApiSpec::from_json_str("not-json").is_err());
    assert!(OpenApiSpec::from_json_str(r#"{"openapi":"3.0.0"}"#)
        .unwrap_err()
        .contains("missing 'paths'"));

    // External $ref error
    let ext_ref = json!({
        "paths": {
            "/test": {
                "get": { "$ref": "http://example.com/spec.json#/Op" }
            }
        }
    });
    assert!(OpenApiSpec::from_json_str(&ext_ref.to_string())
        .unwrap_err()
        .contains("Unsupported external $ref"));

    // Unresolved internal $ref error
    let missing_ref = json!({
        "paths": {
            "/test": {
                "get": { "$ref": "#/components/Missing" }
            }
        }
    });
    assert!(OpenApiSpec::from_json_str(&missing_ref.to_string())
        .unwrap_err()
        .contains("Unresolved OpenAPI $ref pointer"));

    // Depth > 24 cycle error
    let cyclic_root = json!({
        "a": { "$ref": "#/a" }
    });
    assert!(resolve_refs(&cyclic_root, &cyclic_root["a"], 0)
        .unwrap_err()
        .contains("Exceeded maximum OpenAPI $ref resolution depth"));

    // Valid minimal spec with default compatibility_dates fallback ("2020-01-01")
    let valid_spec_json = json!({
        "paths": {
            "/items/{item_id}": {
                "get": {
                    "operationId": "GetItem",
                    "tags": ["Items"],
                    "security": [{"OAuth2": ["esi-items.read.v1"]}],
                    "x-required-roles": ["Director"],
                    "parameters": [
                        {"name": "item_id", "in": "path", "required": true, "schema": {"type": "integer"}}
                    ],
                    "responses": {
                        "200": {
                            "headers": {"X-Pages": {}},
                            "content": {
                                "application/json": {
                                    "schema": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}}
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    let parsed = OpenApiSpec::from_json_str(&valid_spec_json.to_string()).expect("valid spec");
    assert_eq!(parsed.compatibility_dates, vec!["2020-01-01".to_string()]);
    assert!(parsed.path_exists("/v1/items/42"));
    assert!(!parsed.path_exists("/v1/unknown/42"));
    let matched = parsed
        .match_operation("GET", "/v2/items/99/")
        .expect("matches GetItem");
    assert_eq!(matched.operation.operation_id, "GetItem");
    assert_eq!(matched.operation.required_roles, vec!["Director"]);
    assert_eq!(
        matched.path_params.get("item_id").map(String::as_str),
        Some("99")
    );
}

#[test]
fn validate_response_checks_mandatory_headers_304_204_xpages_and_body() {
    let spec = OpenApiSpec::pinned();
    assert_eq!(legacy_operations().len(), 8);

    // Unknown operation fails
    let mut hdrs = HashMap::new();
    assert!(spec
        .validate_response("GET", "/does/not/exist", 200, &hdrs, b"{}")
        .is_err());

    // Missing mandatory ESI headers fails
    assert!(spec
        .validate_response("GET", "/status", 200, &hdrs, b"{}")
        .unwrap_err()
        .contains("missing mandatory ESI header"));

    hdrs.insert("X-Esi-Error-Limit-Remain".to_string(), "100".to_string());
    hdrs.insert("X-Esi-Error-Limit-Reset".to_string(), "60".to_string());
    hdrs.insert("X-Esi-Request-Id".to_string(), "req-1".to_string());

    // 304 Not Modified: non-empty body fails, missing ETag fails, empty body + ETag succeeds
    assert!(spec
        .validate_response("GET", "/status", 304, &hdrs, b"not-empty")
        .unwrap_err()
        .contains("304 Not Modified response must have an empty body"));
    assert!(spec
        .validate_response("GET", "/status", 304, &hdrs, b"")
        .unwrap_err()
        .contains("missing ETag header"));
    hdrs.insert("ETag".to_string(), "\"abc\"".to_string());
    assert!(spec.validate_response("GET", "/status", 304, &hdrs, b"").is_ok());

    // Missing required Cache-Control on 200 /status fails
    assert!(spec
        .validate_response("GET", "/status", 200, &hdrs, b"{}")
        .unwrap_err()
        .contains("missing OpenAPI required response header"));
    hdrs.insert("Cache-Control".to_string(), "   ".to_string());
    assert!(spec
        .validate_response("GET", "/status", 200, &hdrs, b"{}")
        .unwrap_err()
        .contains("is empty"));
    hdrs.insert("Cache-Control".to_string(), "public, max-age=30".to_string());
    hdrs.insert(
        "Last-Modified".to_string(),
        "Wed, 15 Nov 2023 00:00:00 GMT".to_string(),
    );
    hdrs.insert(
        "X-Compatibility-Date".to_string(),
        "2025-12-16".to_string(),
    );

    // X-Pages validation on paginated endpoint (/contracts/public/10000002)
    hdrs.insert("X-Pages".to_string(), "not-int".to_string());
    assert!(spec
        .validate_response("GET", "/contracts/public/10000002", 200, &hdrs, b"[]")
        .unwrap_err()
        .contains("not a valid integer"));
    hdrs.insert("X-Pages".to_string(), "0".to_string());
    assert!(spec
        .validate_response("GET", "/contracts/public/10000002", 200, &hdrs, b"[]")
        .unwrap_err()
        .contains("must be >= 1"));
    hdrs.insert("X-Pages".to_string(), "1".to_string());
    assert!(spec
        .validate_response("GET", "/contracts/public/10000002", 200, &hdrs, b"[]")
        .is_ok());
}

#[test]
fn validate_value_against_schema_covers_all_json_schema_combinators_and_types() {
    assert_eq!(json_type_name(&Value::Null), "null");
    assert_eq!(json_type_name(&json!(true)), "boolean");
    assert_eq!(json_type_name(&json!(42)), "integer");
    assert_eq!(json_type_name(&json!(3.5)), "number");
    assert_eq!(json_type_name(&json!("s")), "string");
    assert_eq!(json_type_name(&json!([])), "array");
    assert_eq!(json_type_name(&json!({})), "object");

    // Non-object schema passes trivially
    assert!(validate_value_against_schema(&json!(1), &json!(true), "root").is_ok());

    // enum, allOf, anyOf, oneOf
    let enum_schema = json!({"enum": ["a", "b"]});
    assert!(validate_value_against_schema(&json!("a"), &enum_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!("c"), &enum_schema, "root").is_err());

    let all_of_schema = json!({"allOf": [{"type": "integer", "minimum": 5}, {"type": "integer", "maximum": 10}]});
    assert!(validate_value_against_schema(&json!(7), &all_of_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!(12), &all_of_schema, "root").is_err());

    let any_of_schema = json!({"anyOf": [{"type": "integer"}, {"type": "string"}]});
    assert!(validate_value_against_schema(&json!("ok"), &any_of_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!(false), &any_of_schema, "root").is_err());

    let one_of_schema = json!({"oneOf": [{"type": "integer", "minimum": 0}, {"type": "integer", "maximum": 10}]});
    // 5 matches both -> fails oneOf; -5 matches only second -> passes oneOf
    assert!(validate_value_against_schema(&json!(5), &one_of_schema, "root").is_err());
    assert!(validate_value_against_schema(&json!(-5), &one_of_schema, "root").is_ok());

    // Union type array ["string", "null"]
    let union_schema = json!({"type": ["string", "null"]});
    assert!(validate_value_against_schema(&Value::Null, &union_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!("hi"), &union_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!(123), &union_schema, "root").is_err());

    // Object required, unexpected property, additionalProperties object/bool
    let obj_schema = json!({
        "type": "object",
        "required": ["id"],
        "properties": {"id": {"type": "integer"}}
    });
    assert!(validate_value_against_schema(&json!({}), &obj_schema, "root").is_err());
    assert!(validate_value_against_schema(&json!({"id": 1, "extra": 2}), &obj_schema, "root").is_err());

    let add_props_schema = json!({
        "type": "object",
        "additionalProperties": {"type": "boolean"}
    });
    assert!(validate_value_against_schema(&json!({"flag": true}), &add_props_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!({"flag": 1}), &add_props_schema, "root").is_err());

    // Array uniqueItems & items
    let arr_schema = json!({"type": "array", "uniqueItems": true, "items": {"type": "integer"}});
    assert!(validate_value_against_schema(&json!([1, 2, 3]), &arr_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!([1, 2, 1]), &arr_schema, "root").is_err());

    // Number min/max, string formats (date-time, date), boolean, unsupported type
    let num_schema = json!({"type": "number", "minimum": 1.5, "maximum": 2.5});
    assert!(validate_value_against_schema(&json!(2.0), &num_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!(1.0), &num_schema, "root").is_err());
    assert!(validate_value_against_schema(&json!(3.0), &num_schema, "root").is_err());

    let dt_schema = json!({"type": "string", "format": "date-time"});
    assert!(validate_value_against_schema(&json!("2023-11-14T22:13:20Z"), &dt_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!("2023-11-14"), &dt_schema, "root").is_err());

    let d_schema = json!({"type": "string", "format": "date"});
    assert!(validate_value_against_schema(&json!("2023-11-14"), &d_schema, "root").is_ok());
    assert!(validate_value_against_schema(&json!("2023/11/14"), &d_schema, "root").is_err());

    let unsupported = json!({"type": "custom_alien_type"});
    assert!(validate_value_against_schema(&json!(1), &unsupported, "root")
        .unwrap_err()
        .contains("unsupported schema type"));
}
