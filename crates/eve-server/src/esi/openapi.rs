//! `esi::openapi` — Pinned ESI OpenAPI 3.0 specification loader, `$ref` resolver, route matcher,
//! and response schema validator (`ESI-2`).
//!
//! Loads `.agents/skills/evomu-adversarial-review/references/parity/sources/esi_openapi.json`
//! (197 operations across 182 paths) and validates HTTP status codes, required response headers,
//! and JSON payloads against the resolved OpenAPI schemas.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Raw JSON text of the pinned V24.01 ESI OpenAPI 3.0 specification.
pub const EMBEDDED_ESI_OPENAPI_JSON: &str =
    include_str!("../../../../.agents/skills/evomu-adversarial-review/references/parity/sources/esi_openapi.json");

/// Supported ESI route version prefixes that map to the canonical OpenAPI path space.
pub const VERSION_PREFIXES: &[&str] = &[
    "/latest", "/legacy", "/dev", "/v1", "/v2", "/v3", "/v4", "/v5", "/v6",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterSpec {
    pub name: String,
    pub location: String,
    pub required: bool,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseSpec {
    pub status_code: String,
    pub required_headers: Vec<String>,
    pub json_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationSpec {
    pub method: String,
    pub path_pattern: String,
    pub segments: Vec<String>,
    pub operation_id: String,
    pub tags: Vec<String>,
    pub required_scopes: Vec<String>,
    #[serde(default)]
    pub required_roles: Vec<String>,
    pub parameters: Vec<ParameterSpec>,
    pub responses: HashMap<String, ResponseSpec>,
}

#[derive(Debug, Clone)]
pub struct MatchedOperation<'a> {
    pub operation: &'a OperationSpec,
    pub path_params: HashMap<String, String>,
    pub version_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpenApiSpec {
    pub raw_root: Value,
    pub compatibility_dates: Vec<String>,
    pub operations: Vec<OperationSpec>,
    pub tags: Vec<String>,
}

static PINNED_SPEC: OnceLock<OpenApiSpec> = OnceLock::new();

impl OpenApiSpec {
    /// Returns a shared reference to the parsed and `$ref`-resolved pinned ESI OpenAPI specification.
    pub fn pinned() -> &'static Self {
        PINNED_SPEC.get_or_init(|| {
            Self::from_json_str(EMBEDDED_ESI_OPENAPI_JSON)
                .expect("pinned esi_openapi.json must parse cleanly")
        })
    }

    /// Parses an OpenAPI 3.0 JSON specification and resolves all internal `$ref` pointers.
    pub fn from_json_str(json_str: &str) -> Result<Self, String> {
        let root: Value =
            serde_json::from_str(json_str).map_err(|e| format!("Invalid OpenAPI JSON: {e}"))?;

        let mut compatibility_dates = Vec::new();
        if let Some(enum_vals) = root
            .pointer("/components/parameters/CompatibilityDate/schema/enum")
            .and_then(|v| v.as_array())
        {
            for v in enum_vals {
                if let Some(s) = v.as_str() {
                    compatibility_dates.push(s.to_string());
                }
            }
        }
        if compatibility_dates.is_empty() {
            compatibility_dates.push("2020-01-01".to_string());
        }

        let paths_obj = root
            .get("paths")
            .and_then(|v| v.as_object())
            .ok_or_else(|| "OpenAPI spec missing 'paths' object".to_string())?;

        let mut operations = Vec::new();
        let mut tag_set = HashSet::new();

        for (path_pattern, path_item) in paths_obj {
            let Some(methods_obj) = path_item.as_object() else {
                continue;
            };
            let normalized_pattern = normalize_canonical_path(path_pattern);
            let segments: Vec<String> = normalized_pattern
                .trim_start_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();

            for method_lc in ["get", "post", "put", "delete"] {
                let Some(op_val) = methods_obj.get(method_lc) else {
                    continue;
                };
                let resolved_op = resolve_refs(&root, op_val, 0)?;
                let operation_id = resolved_op
                    .get("operationId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut tags = Vec::new();
                if let Some(arr) = resolved_op.get("tags").and_then(|v| v.as_array()) {
                    for t in arr {
                        if let Some(ts) = t.as_str() {
                            tags.push(ts.to_string());
                            tag_set.insert(ts.to_string());
                        }
                    }
                }

                let mut required_scopes = Vec::new();
                if let Some(sec_arr) = resolved_op.get("security").and_then(|v| v.as_array()) {
                    for sec_item in sec_arr {
                        if let Some(oauth_arr) = sec_item.get("OAuth2").and_then(|v| v.as_array()) {
                            for sc in oauth_arr {
                                if let Some(s) = sc.as_str() {
                                    required_scopes.push(s.to_string());
                                }
                            }
                        }
                    }
                }

                let mut required_roles = Vec::new();
                if let Some(roles_arr) = resolved_op.get("x-required-roles").and_then(|v| v.as_array()) {
                    for r in roles_arr {
                        if let Some(rs) = r.as_str() {
                            required_roles.push(rs.to_string());
                        }
                    }
                }

                let mut parameters = Vec::new();
                if let Some(param_arr) = resolved_op.get("parameters").and_then(|v| v.as_array()) {
                    for p in param_arr {
                        let name = p
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let location = p
                            .get("in")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let required = p
                            .get("required")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let schema = p.get("schema").cloned().unwrap_or(Value::Null);
                        if !name.is_empty() {
                            parameters.push(ParameterSpec {
                                name,
                                location,
                                required,
                                schema,
                            });
                        }
                    }
                }

                let mut responses = HashMap::new();
                if let Some(resp_obj) = resolved_op.get("responses").and_then(|v| v.as_object()) {
                    for (code_key, resp_val) in resp_obj {
                        let mut required_headers = Vec::new();
                        if let Some(hdrs_obj) = resp_val.get("headers").and_then(|v| v.as_object())
                        {
                            for hdr_name in hdrs_obj.keys() {
                                required_headers.push(hdr_name.clone());
                            }
                            required_headers.sort();
                        }
                        let json_schema = resp_val
                            .pointer("/content/application~1json/schema")
                            .cloned();
                        responses.insert(
                            code_key.clone(),
                            ResponseSpec {
                                status_code: code_key.clone(),
                                required_headers,
                                json_schema,
                            },
                        );
                    }
                }

                operations.push(OperationSpec {
                    method: method_lc.to_ascii_uppercase(),
                    path_pattern: normalized_pattern.clone(),
                    segments: segments.clone(),
                    operation_id,
                    tags,
                    required_scopes,
                    required_roles,
                    parameters,
                    responses,
                });
            }
        }

        operations.extend(legacy_operations());

        let mut tags: Vec<String> = tag_set.into_iter().collect();
        tags.sort();

        Ok(Self {
            raw_root: root,
            compatibility_dates,
            operations,
            tags,
        })
    }

    /// Strips any optional version prefix (`/latest`, `/legacy`, `/dev`, `/v1`..`/v6`) and trailing
    /// slash, returning `(version_prefix, canonical_path)`.
    pub fn strip_version_prefix(raw_path: &str) -> (Option<String>, String) {
        let without_query = raw_path.split('?').next().unwrap_or(raw_path);
        let cleaned = if without_query.is_empty() {
            "/"
        } else {
            without_query
        };

        for &prefix in VERSION_PREFIXES {
            if cleaned == prefix {
                return (Some(prefix.to_string()), "/".to_string());
            }
            if let Some(rest) = cleaned.strip_prefix(prefix) {
                if rest.starts_with('/') {
                    return (
                        Some(prefix.to_string()),
                        normalize_canonical_path(rest),
                    );
                }
            }
        }

        (None, normalize_canonical_path(cleaned))
    }

    /// Matches an HTTP method and request path (with or without version prefix / trailing slash)
    /// against the OpenAPI operations table, extracting path parameters.
    pub fn match_operation(&self, method: &str, raw_path: &str) -> Option<MatchedOperation<'_>> {
        let (version_prefix, canonical) = Self::strip_version_prefix(raw_path);
        let req_segments: Vec<&str> = canonical
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        // Prefer exact literal matches before parameterized matches.
        let mut best_match: Option<(usize, MatchedOperation<'_>)> = None;

        for op in &self.operations {
            if !op.method.eq_ignore_ascii_case(method) {
                continue;
            }
            if op.segments.len() != req_segments.len() {
                continue;
            }
            let mut params = HashMap::new();
            let mut literal_score = 0usize;
            let mut matched = true;
            for (pat_seg, req_seg) in op.segments.iter().zip(req_segments.iter()) {
                if let Some(param_name) = pat_seg
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                {
                    params.insert(param_name.to_string(), (*req_seg).to_string());
                } else if pat_seg == req_seg {
                    literal_score += 1;
                } else {
                    matched = false;
                    break;
                }
            }
            if matched {
                let candidate = MatchedOperation {
                    operation: op,
                    path_params: params,
                    version_prefix: version_prefix.clone(),
                };
                if best_match
                    .as_ref()
                    .map(|(best_score, _)| literal_score > *best_score)
                    .unwrap_or(true)
                {
                    best_match = Some((literal_score, candidate));
                }
            }
        }

        best_match.map(|(_, m)| m)
    }

    /// Checks whether any operation exists at `raw_path` for *any* HTTP method.
    pub fn path_exists(&self, raw_path: &str) -> bool {
        let (_, canonical) = Self::strip_version_prefix(raw_path);
        let req_segments: Vec<&str> = canonical
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        self.operations.iter().any(|op| {
            op.segments.len() == req_segments.len()
                && op
                    .segments
                    .iter()
                    .zip(req_segments.iter())
                    .all(|(pat_seg, req_seg)| {
                        (pat_seg.starts_with('{') && pat_seg.ends_with('}')) || pat_seg == req_seg
                    })
        })
    }

    /// Validates an HTTP response (`status_code`, `headers`, `body`) against the pinned OpenAPI
    /// operation specification for `(method, raw_path)`.
    pub fn validate_response(
        &self,
        method: &str,
        raw_path: &str,
        status_code: u16,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<(), String> {
        let matched = self.match_operation(method, raw_path).ok_or_else(|| {
            format!("No OpenAPI operation matches {method} {raw_path}")
        })?;
        let op = matched.operation;

        // Every ESI response must carry error-limit and request-id headers.
        for mandatory_esi_hdr in [
            "x-esi-error-limit-remain",
            "x-esi-error-limit-reset",
            "x-esi-request-id",
        ] {
            if !has_header_ci(headers, mandatory_esi_hdr) {
                return Err(format!(
                    "{} {}: missing mandatory ESI header '{}'",
                    op.method, op.path_pattern, mandatory_esi_hdr
                ));
            }
        }

        if status_code == 304 {
            if !body.is_empty() {
                return Err(format!(
                    "{} {}: 304 Not Modified response must have an empty body, got {} bytes",
                    op.method,
                    op.path_pattern,
                    body.len()
                ));
            }
            if !has_header_ci(headers, "etag") {
                return Err(format!(
                    "{} {}: 304 Not Modified response missing ETag header",
                    op.method, op.path_pattern
                ));
            }
            return Ok(());
        }

        let code_str = status_code.to_string();
        let resp_spec = op
            .responses
            .get(&code_str)
            .or_else(|| {
                if status_code >= 400 {
                    op.responses.get("default")
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                format!(
                    "{} {}: status code {} is not defined in OpenAPI responses",
                    op.method, op.path_pattern, status_code
                )
            })?;

        for req_hdr in &resp_spec.required_headers {
            let Some(val) = get_header_ci(headers, req_hdr) else {
                return Err(format!(
                    "{} {} ({}): missing OpenAPI required response header '{}'",
                    op.method, op.path_pattern, status_code, req_hdr
                ));
            };
            if val.trim().is_empty() {
                return Err(format!(
                    "{} {} ({}): OpenAPI required response header '{}' is empty",
                    op.method, op.path_pattern, status_code, req_hdr
                ));
            }
            if req_hdr.eq_ignore_ascii_case("x-pages") {
                let parsed: i64 = val.parse().map_err(|_| {
                    format!(
                        "{} {}: X-Pages header '{}' is not a valid integer",
                        op.method, op.path_pattern, val
                    )
                })?;
                if parsed < 1 {
                    return Err(format!(
                        "{} {}: X-Pages header must be >= 1, got {}",
                        op.method, op.path_pattern, parsed
                    ));
                }
            }
        }

        if status_code == 204 {
            if !body.is_empty() {
                return Err(format!(
                    "{} {}: 204 No Content must have an empty body",
                    op.method, op.path_pattern
                ));
            }
            return Ok(());
        }

        if let Some(ref schema) = resp_spec.json_schema {
            let json_val: Value = serde_json::from_slice(body).map_err(|e| {
                format!(
                    "{} {} ({}): response body is not valid JSON: {e}",
                    op.method, op.path_pattern, status_code
                )
            })?;
            validate_value_against_schema(
                &json_val,
                schema,
                &format!("{} {} ({})", op.method, op.path_pattern, status_code),
            )?;
        }

        Ok(())
    }
}

pub fn normalize_canonical_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    let without_trailing = trimmed.trim_end_matches('/');
    if without_trailing.starts_with('/') {
        without_trailing.to_string()
    } else {
        format!("/{without_trailing}")
    }
}

fn has_header_ci(headers: &HashMap<String, String>, target: &str) -> bool {
    get_header_ci(headers, target).is_some()
}

fn get_header_ci<'a>(headers: &'a HashMap<String, String>, target: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(target))
        .map(|(_, v)| v.as_str())
}

fn resolve_refs(root: &Value, current: &Value, depth: usize) -> Result<Value, String> {
    if depth > 24 {
        return Err("Exceeded maximum OpenAPI $ref resolution depth".into());
    }
    match current {
        Value::Object(map) => {
            if let Some(ref_str) = map.get("$ref").and_then(|v| v.as_str()) {
                let ptr = ref_str.strip_prefix('#').ok_or_else(|| {
                    format!("Unsupported external $ref '{ref_str}'")
                })?;
                let target = root.pointer(ptr).ok_or_else(|| {
                    format!("Unresolved OpenAPI $ref pointer '{ref_str}'")
                })?;
                return resolve_refs(root, target, depth + 1);
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), resolve_refs(root, v, depth + 1)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(resolve_refs(root, item, depth + 1)?);
            }
            Ok(Value::Array(out))
        }
        other => Ok(other.clone()),
    }
}

/// Recursively validates a `serde_json::Value` against a resolved OpenAPI 3.0 / 3.1 schema object.
pub fn validate_value_against_schema(
    value: &Value,
    schema: &Value,
    path_ctx: &str,
) -> Result<(), String> {
    let Some(schema_obj) = schema.as_object() else {
        return Ok(());
    };

    if let Some(enum_arr) = schema_obj.get("enum").and_then(|v| v.as_array()) {
        if !enum_arr.is_empty() && !enum_arr.contains(value) {
            return Err(format!(
                "{path_ctx}: value {value} is not in allowed enum {enum_arr:?}"
            ));
        }
    }

    if let Some(all_of) = schema_obj.get("allOf").and_then(|v| v.as_array()) {
        for (idx, sub) in all_of.iter().enumerate() {
            validate_value_against_schema(value, sub, &format!("{path_ctx}(allOf[{idx}])"))?;
        }
    }

    if let Some(any_of) = schema_obj.get("anyOf").and_then(|v| v.as_array()) {
        if !any_of.is_empty()
            && !any_of
                .iter()
                .any(|sub| validate_value_against_schema(value, sub, path_ctx).is_ok())
        {
            return Err(format!(
                "{path_ctx}: value did not match any subschema in anyOf"
            ));
        }
    }

    if let Some(one_of) = schema_obj.get("oneOf").and_then(|v| v.as_array()) {
        if !one_of.is_empty() {
            let matched = one_of
                .iter()
                .filter(|sub| validate_value_against_schema(value, sub, path_ctx).is_ok())
                .count();
            if matched != 1 {
                return Err(format!(
                    "{path_ctx}: expected value to match exactly 1 subschema in oneOf, matched {matched}"
                ));
            }
        }
    }

    if let Some(type_arr) = schema_obj.get("type").and_then(|v| v.as_array()) {
        let allowed_types: Vec<&str> = type_arr.iter().filter_map(|t| t.as_str()).collect();
        if !allowed_types.is_empty() {
            let mut matched_any = false;
            let mut last_err = String::new();
            for &t in &allowed_types {
                match validate_value_for_scalar_type(value, schema_obj, t, path_ctx) {
                    Ok(()) => {
                        matched_any = true;
                        break;
                    }
                    Err(e) => last_err = e,
                }
            }
            if !matched_any {
                return Err(format!(
                    "{path_ctx}: value did not match allowed types {allowed_types:?} ({last_err})"
                ));
            }
        }
    } else if let Some(expected_type) = schema_obj.get("type").and_then(|v| v.as_str()) {
        validate_value_for_scalar_type(value, schema_obj, expected_type, path_ctx)?;
    }

    Ok(())
}

fn validate_value_for_scalar_type(
    value: &Value,
    schema_obj: &serde_json::Map<String, Value>,
    expected_type: &str,
    path_ctx: &str,
) -> Result<(), String> {
    match expected_type {
        "null" => {
            if !value.is_null() {
                return Err(format!(
                    "{path_ctx}: expected null, got {}",
                    json_type_name(value)
                ));
            }
        }
        "object" => {
            let Some(obj) = value.as_object() else {
                return Err(format!(
                    "{path_ctx}: expected object, got {}",
                    json_type_name(value)
                ));
            };
            if let Some(req_arr) = schema_obj.get("required").and_then(|v| v.as_array()) {
                for req_key in req_arr {
                    if let Some(key_str) = req_key.as_str() {
                        match obj.get(key_str) {
                            Some(v) if !v.is_null() => {}
                            _ => {
                                return Err(format!(
                                    "{path_ctx}: missing required property '{key_str}'"
                                ));
                            }
                        }
                    }
                }
            }
            let props = schema_obj.get("properties").and_then(|v| v.as_object());
            let additional = schema_obj.get("additionalProperties");
            for (k, v) in obj {
                if v.is_null() {
                    continue;
                }
                if let Some(props_map) = props {
                    if let Some(prop_schema) = props_map.get(k) {
                        validate_value_against_schema(
                            v,
                            prop_schema,
                            &format!("{path_ctx}.{k}"),
                        )?;
                        continue;
                    }
                }
                if let Some(add_schema) = additional {
                    if add_schema.is_object() {
                        validate_value_against_schema(
                            v,
                            add_schema,
                            &format!("{path_ctx}.{k}"),
                        )?;
                        continue;
                    } else if add_schema.as_bool() == Some(true) {
                        continue;
                    }
                }
                if props.is_some() && additional.is_none() {
                    return Err(format!(
                        "{path_ctx}: unexpected property '{k}' not in OpenAPI schema"
                    ));
                }
            }
        }
        "array" => {
            let Some(arr) = value.as_array() else {
                return Err(format!(
                    "{path_ctx}: expected array, got {}",
                    json_type_name(value)
                ));
            };
            if schema_obj
                .get("uniqueItems")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                for i in 0..arr.len() {
                    for j in (i + 1)..arr.len() {
                        if arr[i] == arr[j] {
                            return Err(format!(
                                "{path_ctx}: array violates uniqueItems at indices {i} and {j}"
                            ));
                        }
                    }
                }
            }
            if let Some(items_schema) = schema_obj.get("items") {
                for (idx, elem) in arr.iter().enumerate() {
                    validate_value_against_schema(
                        elem,
                        items_schema,
                        &format!("{path_ctx}[{idx}]"),
                    )?;
                }
            }
        }
        "integer" => {
            if !(value.is_i64() || value.is_u64()) {
                return Err(format!(
                    "{path_ctx}: expected integer, got {}",
                    json_type_name(value)
                ));
            }
            if let (Some(n), Some(min)) = (
                value.as_f64(),
                schema_obj.get("minimum").and_then(|v| v.as_f64()),
            ) {
                if n < min {
                    return Err(format!("{path_ctx}: value {n} is below minimum {min}"));
                }
            }
            if let (Some(n), Some(max)) = (
                value.as_f64(),
                schema_obj.get("maximum").and_then(|v| v.as_f64()),
            ) {
                if n > max {
                    return Err(format!("{path_ctx}: value {n} exceeds maximum {max}"));
                }
            }
        }
        "number" => {
            if !value.is_number() {
                return Err(format!(
                    "{path_ctx}: expected number, got {}",
                    json_type_name(value)
                ));
            }
            if let (Some(n), Some(min)) = (
                value.as_f64(),
                schema_obj.get("minimum").and_then(|v| v.as_f64()),
            ) {
                if n < min {
                    return Err(format!("{path_ctx}: value {n} is below minimum {min}"));
                }
            }
            if let (Some(n), Some(max)) = (
                value.as_f64(),
                schema_obj.get("maximum").and_then(|v| v.as_f64()),
            ) {
                if n > max {
                    return Err(format!("{path_ctx}: value {n} exceeds maximum {max}"));
                }
            }
        }
        "string" => {
            let Some(s) = value.as_str() else {
                return Err(format!(
                    "{path_ctx}: expected string, got {}",
                    json_type_name(value)
                ));
            };
            if let Some(fmt) = schema_obj.get("format").and_then(|v| v.as_str()) {
                if fmt == "date-time"
                    && (!s.contains('T') || !(s.ends_with('Z') || s.contains('+')))
                {
                    return Err(format!(
                        "{path_ctx}: string '{s}' is not a valid RFC3339 date-time"
                    ));
                }
                if fmt == "date"
                    && (s.len() != 10 || s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-')
                {
                    return Err(format!(
                        "{path_ctx}: string '{s}' is not a valid ISO-8601 date (YYYY-MM-DD)"
                    ));
                }
            }
        }
        "boolean" => {
            if !value.is_boolean() {
                return Err(format!(
                    "{path_ctx}: expected boolean, got {}",
                    json_type_name(value)
                ));
            }
        }
        other => {
            return Err(format!("{path_ctx}: unsupported schema type '{other}'"));
        }
    }
    Ok(())
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn make_legacy_op(method: &str, path_pattern: &str, op_id: &str, scope: Option<&str>) -> OperationSpec {
    let segments = path_pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    let mut responses = HashMap::new();
    responses.insert(
        "200".to_string(),
        ResponseSpec {
            status_code: "200".to_string(),
            required_headers: vec!["Cache-Control".to_string(), "ETag".to_string()],
            json_schema: None,
        },
    );
    OperationSpec {
        method: method.to_string(),
        path_pattern: path_pattern.to_string(),
        segments,
        operation_id: op_id.to_string(),
        tags: vec!["Character".to_string()],
        required_scopes: scope.map(|s| vec![s.to_string()]).unwrap_or_default(),
        required_roles: Vec::new(),
        parameters: Vec::new(),
        responses,
    }
}

pub fn legacy_operations() -> Vec<OperationSpec> {
    vec![
        make_legacy_op(
            "GET",
            "/characters/{character_id}/bookmarks",
            "GetCharactersCharacterIdBookmarks",
            Some("esi-bookmarks.read_character_bookmarks.v1"),
        ),
        make_legacy_op(
            "GET",
            "/characters/{character_id}/bookmarks/folders",
            "GetCharactersCharacterIdBookmarkFolders",
            Some("esi-bookmarks.read_character_bookmarks.v1"),
        ),
        make_legacy_op(
            "GET",
            "/corporations/{corporation_id}/bookmarks",
            "GetCorporationsCorporationIdBookmarks",
            Some("esi-bookmarks.read_corporation_bookmarks.v1"),
        ),
        make_legacy_op(
            "GET",
            "/corporations/{corporation_id}/bookmarks/folders",
            "GetCorporationsCorporationIdBookmarkFolders",
            Some("esi-bookmarks.read_corporation_bookmarks.v1"),
        ),
        make_legacy_op(
            "GET",
            "/corporations/{corporation_id}/projects",
            "GetCorporationsCorporationIdProjects",
            Some("esi-corporations.read_projects.v1"),
        ),
        make_legacy_op(
            "GET",
            "/opportunities/groups",
            "GetOpportunitiesGroups",
            None,
        ),
        make_legacy_op(
            "GET",
            "/opportunities/tasks",
            "GetOpportunitiesTasks",
            None,
        ),
        make_legacy_op(
            "GET",
            "/characters/{character_id}/opportunities",
            "GetCharactersCharacterIdOpportunities",
            Some("esi-characters.read_opportunities.v1"),
        ),
    ]
}
