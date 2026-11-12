//! Strict Quasar outbound protobuf decode tap and PB-4 request-to-response pairing validator (`C-0`, `#911`).
//!
//! Validates every outbound [`GoogleAny`] (responses, notices, events) against the 1,455-file
//! V24.01 [`QuasarDescriptorRegistry`]:
//! - Rejects unknown `type_url`s (`UnknownTypeUrl`).
//! - Rejects malformed protobuf wire bytes (`WireDecodeError`).
//! - Rejects **unknown field numbers** not present in the V24.01 `DescriptorProto`
//!   (`UnknownFieldNumber` — catches `#893` where internal `eve.*` fields `4, 5, 6` were encoded
//!   into `eve_public.plex.vault.transaction.Attributes`).
//! - Rejects wire-type mismatches (`WireTypeMismatch`) and invalid UTF-8 strings (`InvalidUtf8`).
//! - Recursively validates nested submessages and nested `google.protobuf.Any` payloads.
//! - Enforces **PB-4 request-to-response type pairing** (`MismatchedResponseType` — catches `#936`
//!   where 9 skill-plan handlers returned `google.protobuf.Empty` instead of their paired `*Response`).

use eve_proto::quasar::{
    FieldLabel, FieldType, GoogleAny, QuasarDescriptorRegistry, QuasarMessage, WireMessage,
    WireValue,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A strict protobuf schema or PB-4 pairing violation detected by the Quasar tap (`#911`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum QuasarTapViolation {
    /// `type_url` does not exist in `QuasarDescriptorRegistry`.
    UnknownTypeUrl {
        context: String,
        type_url: String,
    },
    /// Wire bytes failed protobuf decoding.
    WireDecodeError {
        context: String,
        message_type: String,
        error: String,
    },
    /// A wire field number is not defined on `message_type` in the V24.01 descriptor catalog.
    UnknownFieldNumber {
        context: String,
        message_type: String,
        field_number: u32,
    },
    /// Wire encoding type does not match the descriptor's `FieldType`.
    WireTypeMismatch {
        context: String,
        message_type: String,
        field_name: String,
        field_number: u32,
        expected: String,
        actual: String,
    },
    /// A `string` field contains invalid UTF-8 bytes.
    InvalidUtf8 {
        context: String,
        message_type: String,
        field_name: String,
        field_number: u32,
    },
    /// PB-4 violation: request returned a response `type_url` other than its paired `*Response`
    /// descriptor (e.g. `google.protobuf.Empty` when `*Response` exists in the catalog, `#936`).
    MismatchedResponseType {
        request_type: String,
        expected_response_type: String,
        actual_response_type: String,
    },
}

impl QuasarTapViolation {
    pub fn ratchet_id(&self) -> String {
        match self {
            Self::UnknownTypeUrl { context, type_url } => {
                format!("UnknownTypeUrl:{context}:{type_url}")
            }
            Self::WireDecodeError {
                context,
                message_type,
                ..
            } => format!("WireDecodeError:{context}:{message_type}"),
            Self::UnknownFieldNumber {
                context,
                message_type,
                field_number,
            } => format!("UnknownFieldNumber:{context}:{message_type}:#{field_number}"),
            Self::WireTypeMismatch {
                context,
                message_type,
                field_number,
                ..
            } => format!("WireTypeMismatch:{context}:{message_type}:#{field_number}"),
            Self::InvalidUtf8 {
                context,
                message_type,
                field_number,
                ..
            } => format!("InvalidUtf8:{context}:{message_type}:#{field_number}"),
            Self::MismatchedResponseType {
                request_type,
                expected_response_type,
                actual_response_type,
            } => format!(
                "MismatchedResponseType:{request_type}:expected={expected_response_type}:actual={actual_response_type}"
            ),
        }
    }
}

/// Shrinking ratchet for pre-existing Quasar tap findings (`#911`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuasarTapRatchet {
    pub version: u32,
    pub allowed_violations: BTreeSet<String>,
}

impl QuasarTapRatchet {
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("quasar_tap_ratchet.json")
    }

    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }
}

fn wire_value_kind(v: &WireValue) -> &'static str {
    match v {
        WireValue::Varint(_) => "Varint(0)",
        WireValue::Fixed64(_) => "Fixed64(1)",
        WireValue::LengthDelimited(_) => "LengthDelimited(2)",
        WireValue::Fixed32(_) => "Fixed32(5)",
    }
}

fn is_packable_scalar(ft: FieldType) -> bool {
    matches!(
        ft,
        FieldType::Double
            | FieldType::Float
            | FieldType::Int64
            | FieldType::Uint64
            | FieldType::Int32
            | FieldType::Fixed64
            | FieldType::Fixed32
            | FieldType::Bool
            | FieldType::Uint32
            | FieldType::Enum
            | FieldType::Sfixed32
            | FieldType::Sfixed64
            | FieldType::Sint32
            | FieldType::Sint64
    )
}

/// Resolves the expected response message full name for `request_type_name` using:
/// 1. `ServiceDescriptorProto` method bindings in `registry`, or
/// 2. V24.01 naming convention (`<prefix>Request` -> `<prefix>Response`) when `<prefix>Response`
///    exists in `registry.messages_by_full_name`.
pub fn expected_paired_response_type(
    registry: &QuasarDescriptorRegistry,
    request_type_name: &str,
) -> Option<String> {
    let req_norm = request_type_name
        .strip_prefix("type.googleapis.com/")
        .unwrap_or(request_type_name)
        .trim_start_matches('.');

    for svc in registry.services_by_full_name.values() {
        for method in &svc.methods {
            if method.input_type.trim_start_matches('.') == req_norm {
                let out = method.output_type.trim_start_matches('.').to_string();
                if !out.is_empty() {
                    return Some(out);
                }
            }
        }
    }

    if let Some(stem) = req_norm.strip_suffix("Request") {
        let candidate = format!("{stem}Response");
        if registry.get_message(&candidate).is_some() {
            return Some(candidate);
        }
    }
    None
}

/// Enforces PB-4 request-to-response type pairing (`#911`, `#936`).
pub fn validate_request_response_pairing(
    registry: &QuasarDescriptorRegistry,
    request_type_name: &str,
    response_any: &GoogleAny,
    violations: &mut Vec<QuasarTapViolation>,
) {
    let req_norm = request_type_name
        .strip_prefix("type.googleapis.com/")
        .unwrap_or(request_type_name)
        .trim_start_matches('.');
    let actual_rsp = response_any.normalized_type_name();

    if let Some(expected_rsp) = expected_paired_response_type(registry, req_norm) {
        if actual_rsp != expected_rsp {
            violations.push(QuasarTapViolation::MismatchedResponseType {
                request_type: req_norm.to_string(),
                expected_response_type: expected_rsp,
                actual_response_type: actual_rsp.to_string(),
            });
        }
    }
}

/// Strictly validates a [`GoogleAny`] payload and all nested submessages against `registry`,
/// flagging unknown `type_url`s, wire decode errors, unknown field numbers, and wire-type mismatches.
pub fn validate_any_strict(
    registry: &QuasarDescriptorRegistry,
    context: &str,
    any: &GoogleAny,
    violations: &mut Vec<QuasarTapViolation>,
) {
    let norm = any.normalized_type_name();
    if norm.is_empty() {
        violations.push(QuasarTapViolation::UnknownTypeUrl {
            context: context.to_string(),
            type_url: any.type_url.clone(),
        });
        return;
    }
    validate_message_bytes_strict(registry, context, norm, &any.value, violations);
}

/// Strictly validates raw protobuf wire bytes against the descriptor for `message_type`.
pub fn validate_message_bytes_strict(
    registry: &QuasarDescriptorRegistry,
    context: &str,
    message_type: &str,
    wire_bytes: &[u8],
    violations: &mut Vec<QuasarTapViolation>,
) {
    let norm = message_type
        .strip_prefix("type.googleapis.com/")
        .unwrap_or(message_type)
        .trim_start_matches('.');

    let Some(desc) = registry.get_message(norm) else {
        violations.push(QuasarTapViolation::UnknownTypeUrl {
            context: context.to_string(),
            type_url: message_type.to_string(),
        });
        return;
    };

    let wm = match WireMessage::decode(wire_bytes) {
        Ok(m) => m,
        Err(e) => {
            violations.push(QuasarTapViolation::WireDecodeError {
                context: context.to_string(),
                message_type: norm.to_string(),
                error: e.to_string(),
            });
            return;
        }
    };

    for wf in &wm.fields {
        let Some(fdesc) = desc.fields.iter().find(|f| f.number == wf.field_number) else {
            violations.push(QuasarTapViolation::UnknownFieldNumber {
                context: context.to_string(),
                message_type: norm.to_string(),
                field_number: wf.field_number,
            });
            continue;
        };

        // Allow packed repeated scalar fields encoded as LengthDelimited
        if fdesc.label == FieldLabel::Repeated
            && is_packable_scalar(fdesc.field_type)
            && matches!(wf.value, WireValue::LengthDelimited(_))
        {
            if let WireValue::LengthDelimited(packed_bytes) = &wf.value {
                if matches!(
                    fdesc.field_type,
                    FieldType::Fixed64 | FieldType::Sfixed64 | FieldType::Double
                ) && packed_bytes.len() % 8 != 0
                {
                    violations.push(QuasarTapViolation::WireTypeMismatch {
                        context: context.to_string(),
                        message_type: norm.to_string(),
                        field_name: fdesc.name.clone(),
                        field_number: wf.field_number,
                        expected: "packed 64-bit multiple of 8 bytes".to_string(),
                        actual: format!("{} bytes", packed_bytes.len()),
                    });
                } else if matches!(
                    fdesc.field_type,
                    FieldType::Fixed32 | FieldType::Sfixed32 | FieldType::Float
                ) && packed_bytes.len() % 4 != 0
                {
                    violations.push(QuasarTapViolation::WireTypeMismatch {
                        context: context.to_string(),
                        message_type: norm.to_string(),
                        field_name: fdesc.name.clone(),
                        field_number: wf.field_number,
                        expected: "packed 32-bit multiple of 4 bytes".to_string(),
                        actual: format!("{} bytes", packed_bytes.len()),
                    });
                }
            }
            continue;
        }

        match (&fdesc.field_type, &wf.value) {
            (
                FieldType::Int32
                | FieldType::Int64
                | FieldType::Uint32
                | FieldType::Uint64
                | FieldType::Sint32
                | FieldType::Sint64
                | FieldType::Bool
                | FieldType::Enum,
                WireValue::Varint(_),
            ) => {}
            (
                FieldType::Double | FieldType::Fixed64 | FieldType::Sfixed64,
                WireValue::Fixed64(_),
            ) => {}
            (
                FieldType::Float | FieldType::Fixed32 | FieldType::Sfixed32,
                WireValue::Fixed32(_),
            ) => {}
            (FieldType::Bytes, WireValue::LengthDelimited(_)) => {}
            (FieldType::String, WireValue::LengthDelimited(bytes)) => {
                if std::str::from_utf8(bytes).is_err() {
                    violations.push(QuasarTapViolation::InvalidUtf8 {
                        context: context.to_string(),
                        message_type: norm.to_string(),
                        field_name: fdesc.name.clone(),
                        field_number: wf.field_number,
                    });
                }
            }
            (FieldType::Message | FieldType::Group, WireValue::LengthDelimited(sub_bytes)) => {
                let child_type = fdesc.type_name.trim_start_matches('.');
                let child_ctx = format!("{context}.{}(#{})", fdesc.name, wf.field_number);
                if child_type == "google.protobuf.Any" {
                    match GoogleAny::decode_proto(sub_bytes) {
                        Ok(inner_any) => {
                            if !inner_any.type_url.is_empty() {
                                validate_any_strict(registry, &child_ctx, &inner_any, violations);
                            }
                        }
                        Err(e) => {
                            violations.push(QuasarTapViolation::WireDecodeError {
                                context: child_ctx,
                                message_type: "google.protobuf.Any".to_string(),
                                error: e.to_string(),
                            });
                        }
                    }
                } else if !child_type.is_empty() {
                    validate_message_bytes_strict(
                        registry,
                        &child_ctx,
                        child_type,
                        sub_bytes,
                        violations,
                    );
                }
            }
            (_, actual_val) => {
                violations.push(QuasarTapViolation::WireTypeMismatch {
                    context: context.to_string(),
                    message_type: norm.to_string(),
                    field_name: fdesc.name.clone(),
                    field_number: wf.field_number,
                    expected: format!("{:?}", fdesc.field_type),
                    actual: wire_value_kind(actual_val).to_string(),
                });
            }
        }
    }
}

/// Taps a complete Quasar request/response exchange plus any emitted notices (`#911`).
pub fn tap_exchange(
    registry: &QuasarDescriptorRegistry,
    request_type: &str,
    status_code: u32,
    response_payload: Option<&GoogleAny>,
    emitted_notices: &[GoogleAny],
) -> Vec<QuasarTapViolation> {
    let mut violations = Vec::new();
    if status_code == 200 {
        if let Some(rsp_any) = response_payload {
            validate_request_response_pairing(registry, request_type, rsp_any, &mut violations);
            let ctx = format!("response({request_type})");
            validate_any_strict(registry, &ctx, rsp_any, &mut violations);
        }
    }
    for (idx, notice) in emitted_notices.iter().enumerate() {
        let ctx = format!("notice[{idx}]({request_type})");
        validate_any_strict(registry, &ctx, notice, &mut violations);
    }
    violations.sort();
    violations.dedup();
    violations
}

/// Asserts that `violations` contain no unratcheted entries and do not exceed `ratchet.allowed_violations`.
pub fn assert_quasar_tap_ratchet(
    violations: &[QuasarTapViolation],
    ratchet: &QuasarTapRatchet,
) -> Result<(), String> {
    let unexpected: Vec<String> = violations
        .iter()
        .map(QuasarTapViolation::ratchet_id)
        .filter(|id| !ratchet.allowed_violations.contains(id))
        .collect();
    if !unexpected.is_empty() {
        return Err(format!(
            "Found {} unratcheted Quasar protobuf tap violation(s): {unexpected:#?}",
            unexpected.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64, GoogleEmpty,
        TYPE_GOOGLEAPIS_PREFIX,
    };

    #[test]
    fn tap_rejects_bug_893_wrong_plex_transaction_attributes_fields_and_accepts_fixed() {
        let reg = QuasarDescriptorRegistry::open_default_or_empty();
        if reg
            .get_message("eve_public.plex.vault.api.BalanceChangedNotice")
            .is_none()
        {
            return;
        }

        // Construct the pre-#893 broken BalanceChangedNotice payload (encoding internal `eve.*`
        // fields 2=user_owner, 4=amount_transferred, 5=resulting_balance, 6=description):
        let mut bad_notice_bytes = Vec::new();
        let mut ident = Vec::new();
        encode_varint_field(1, 901, &mut ident);
        encode_bytes_field(1, &ident, &mut bad_notice_bytes);

        let mut bad_attr = Vec::new();
        let mut ts_bytes = Vec::new();
        encode_varint_field(1, 1_760_000_000, &mut ts_bytes);
        encode_bytes_field(1, &ts_bytes, &mut bad_attr);
        let mut user_ident = Vec::new();
        encode_varint_field(1, 1001, &mut user_ident);
        encode_bytes_field(2, &user_ident, &mut bad_attr);
        let mut amt_curr = Vec::new();
        encode_varint_field(1, encode_zigzag64(10_000), &mut amt_curr);
        encode_bytes_field(4, &amt_curr, &mut bad_attr); // unknown field #4!
        let mut bal_curr = Vec::new();
        encode_varint_field(1, encode_zigzag64(50_000), &mut bal_curr);
        encode_bytes_field(5, &bal_curr, &mut bad_attr); // unknown field #5!
        encode_string_field(6, "GM Deposit", &mut bad_attr); // unknown field #6!
        encode_bytes_field(2, &bad_attr, &mut bad_notice_bytes);

        let bad_any = GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"
            ),
            value: bad_notice_bytes,
        };
        let mut bad_violations = Vec::new();
        validate_any_strict(&reg, "test_893_bad", &bad_any, &mut bad_violations);
        assert!(
            bad_violations.iter().any(|v| matches!(
                v,
                QuasarTapViolation::UnknownFieldNumber {
                    message_type,
                    field_number: 4 | 5 | 6,
                    ..
                } if message_type == "eve_public.plex.vault.transaction.Attributes"
            )),
            "#893 broken BalanceChangedNotice must be rejected with UnknownFieldNumber on Attributes fields 4/5/6, got {bad_violations:#?}"
        );

        // Construct the fixed #893 BalanceChangedNotice (field 1=timestamp, 2=amount_transferred, 3=resulting_balance):
        let mut good_notice_bytes = Vec::new();
        encode_bytes_field(1, &ident, &mut good_notice_bytes);
        let mut good_attr = Vec::new();
        encode_bytes_field(1, &ts_bytes, &mut good_attr);
        encode_bytes_field(2, &amt_curr, &mut good_attr);
        encode_bytes_field(3, &bal_curr, &mut good_attr);
        encode_bytes_field(2, &good_attr, &mut good_notice_bytes);

        let good_any = GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"
            ),
            value: good_notice_bytes,
        };
        let mut good_violations = Vec::new();
        validate_any_strict(&reg, "test_893_good", &good_any, &mut good_violations);
        assert!(
            good_violations.is_empty(),
            "#893 fixed BalanceChangedNotice must pass strict Quasar tap with 0 violations, got {good_violations:#?}"
        );
    }

    #[test]
    fn tap_rejects_bug_936_skill_plan_empty_responses_and_accepts_paired_responses() {
        let reg = QuasarDescriptorRegistry::open_default_or_empty();
        if reg
            .get_message("eve_public.skill.plan.DeleteResponse")
            .is_none()
        {
            return;
        }

        let nine_endpoints = [
            (
                "eve_public.skill.plan.DeleteRequest",
                "eve_public.skill.plan.DeleteResponse",
            ),
            (
                "eve_public.skill.plan.SetActiveRequest",
                "eve_public.skill.plan.SetActiveResponse",
            ),
            (
                "eve_public.character.skill.plan.SetActiveRequest",
                "eve_public.character.skill.plan.SetActiveResponse",
            ),
            (
                "eve_public.character.skill.plan.SetNameRequest",
                "eve_public.character.skill.plan.SetNameResponse",
            ),
            (
                "eve_public.character.skill.plan.SetDescriptionRequest",
                "eve_public.character.skill.plan.SetDescriptionResponse",
            ),
            (
                "eve_public.character.skill.plan.SetSkillRequirementsRequest",
                "eve_public.character.skill.plan.SetSkillRequirementsResponse",
            ),
            (
                "eve_public.character.skill.plan.DeleteRequest",
                "eve_public.character.skill.plan.DeleteResponse",
            ),
            (
                "eve_public.character.skill.plan.milestone.DeleteRequest",
                "eve_public.character.skill.plan.milestone.DeleteResponse",
            ),
            (
                "eve_public.character.skill.plan.milestone.SetDescriptionRequest",
                "eve_public.character.skill.plan.milestone.SetDescriptionResponse",
            ),
        ];

        for (req_type, expected_rsp_type) in nine_endpoints {
            let bad_empty = GoogleEmpty.to_any();
            let bad_v = tap_exchange(&reg, req_type, 200, Some(&bad_empty), &[]);
            assert!(
                bad_v.contains(&QuasarTapViolation::MismatchedResponseType {
                    request_type: req_type.to_string(),
                    expected_response_type: expected_rsp_type.to_string(),
                    actual_response_type: "google.protobuf.Empty".to_string(),
                }),
                "#936: returning GoogleEmpty for `{req_type}` must fail PB-4 pairing check, got {bad_v:#?}"
            );

            let good_rsp = GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{expected_rsp_type}"),
                value: Vec::new(),
            };
            let good_v = tap_exchange(&reg, req_type, 200, Some(&good_rsp), &[]);
            assert!(
                good_v.is_empty(),
                "#936: returning `{expected_rsp_type}` for `{req_type}` must pass PB-4 and wire validation, got {good_v:#?}"
            );
        }
    }
}
