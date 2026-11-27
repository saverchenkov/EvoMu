//! Declarative per-`(service, method)` RPC fault policy table and telemetry (`#1195`).
//!
//! Complements [`crate::router::binding_transition_policy`] (which handles lifecycle/transition
//! teardown calls) by declaring deterministic domain-error wire mappings post-handler so known
//! client races and benign errors never leak raw unmapped `CustomInfo` popups onto `:26000`.

#![forbid(unsafe_code)]

use crate::flight_recorder::FlightRecorder;
use eve_proto::{BridgeRpcError, ClientError, PyRep, UserErrorKey};

/// Deterministic empty wire reply materialized by [`RpcFaultAction::SoftEmpty`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftEmptyReply {
    None,
    EmptyList,
    EmptyDict,
    ZeroInt,
}

impl SoftEmptyReply {
    /// Converts this soft-empty reply specification into its wire [`PyRep`].
    pub fn to_pyrep(self) -> PyRep {
        match self {
            Self::None => PyRep::None,
            Self::EmptyList => PyRep::List(Vec::new()),
            Self::EmptyDict => PyRep::Dict(Vec::new()),
            Self::ZeroInt => PyRep::Integer(0),
        }
    }
}

/// Post-handler fault policy applied when a dispatched RPC matches an [`RpcFaultPolicyRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcFaultAction {
    /// Converts matching domain errors into a deterministic `Ok(...)` empty reply.
    SoftEmpty(SoftEmptyReply),
    /// Preserves `err.code` and attaches a typed [`UserErrorKey`] wire error when
    /// `err.client_error.is_none()`.
    QuietUserError(UserErrorKey),
    /// Preserves `err.code` and attaches `ClientError::Internal(err.code.clone())` when
    /// `err.client_error.is_none()`, explicitly declaring V24 client-expected internal errors.
    TypedInternal,
}

/// Declarative fault-policy entry keyed by `(service, method, error_codes)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcFaultPolicyRule {
    pub service: &'static str,
    pub method: &'static str,
    pub error_codes: &'static [&'static str],
    pub action: RpcFaultAction,
}

const ITEM_NOT_FOUND_CODES: &[&str] = &["ItemNotFound"];
const WARP_IN_WARP_CODES: &[&str] = &["ShipAlreadyInWarp", "ShipInWarp"];
const WARP_DOCKING_APPROACH_CODES: &[&str] = &["OutsideDockingPerimeter", "DockingApproach"];
const WARP_TOO_SHORT_CODES: &[&str] = &["WarpDistanceTooShort", "WarpTooShort"];
const WARP_DESTINATION_GONE_CODES: &[&str] = &["InvalidWarpDestination", "WarpDestinationGone"];
const CHAR_NOT_AT_STATION_CODES: &[&str] = &["CharacterNotAtStation"];
const FAKE_ITEM_NOT_FOUND_CODES: &[&str] = &["FakeItemNotFound"];

const fn quiet(
    service: &'static str,
    method: &'static str,
    error_codes: &'static [&'static str],
    key: UserErrorKey,
) -> RpcFaultPolicyRule {
    RpcFaultPolicyRule {
        service,
        method,
        error_codes,
        action: RpcFaultAction::QuietUserError(key),
    }
}

const fn rule(
    service: &'static str,
    method: &'static str,
    error_codes: &'static [&'static str],
    action: RpcFaultAction,
) -> RpcFaultPolicyRule {
    RpcFaultPolicyRule {
        service,
        method,
        error_codes,
        action,
    }
}

/// Production fault policy table validated at [`crate::router::rpc_registry::RpcRegistry::default_compiled`].
///
/// Unlisted `(service, method, code)` tuples pass through untouched (fail-closed).
pub const RPC_FAULT_POLICY_TABLE: &[RpcFaultPolicyRule] = &[
    // Auto-reload / consumed charge (#1124).
    quiet("dogmaIM", "LoadAmmo", ITEM_NOT_FOUND_CODES, UserErrorKey::ItemNotFound),
    // Repeatable / idempotent warp commands (#1159, #1165).
    quiet("beyonce", "CmdWarpToStuff", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("beyonce", "CmdWarpToStuff", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("beyonce", "CmdWarpToStuff", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("beyonce", "CmdWarpToStuff", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    quiet("beyonce", "CmdWarpToStuffAutopilot", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("beyonce", "CmdWarpToStuffAutopilot", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("beyonce", "CmdWarpToStuffAutopilot", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("beyonce", "CmdWarpToStuffAutopilot", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    quiet("agentMgr", "WarpToAgentInSpace", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("agentMgr", "WarpToAgentInSpace", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("agentMgr", "WarpToAgentInSpace", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("agentMgr", "WarpToAgentInSpace", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    quiet("agentMgr", "WarpToLocation", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("agentMgr", "WarpToLocation", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("agentMgr", "WarpToLocation", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("agentMgr", "WarpToLocation", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    quiet("fleetMgr", "WarpToMember", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("fleetMgr", "WarpToMember", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("fleetMgr", "WarpToMember", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("fleetMgr", "WarpToMember", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    quiet("fleetObjectHandler", "WarpToMember", WARP_IN_WARP_CODES, UserErrorKey::ShipInWarp),
    quiet("fleetObjectHandler", "WarpToMember", WARP_DOCKING_APPROACH_CODES, UserErrorKey::DockingApproach),
    quiet("fleetObjectHandler", "WarpToMember", WARP_TOO_SHORT_CODES, UserErrorKey::WarpTooShort),
    quiet("fleetObjectHandler", "WarpToMember", WARP_DESTINATION_GONE_CODES, UserErrorKey::WarpDestinationGone),
    // Late / cross-transition station & ship queries (#1163, #1150).
    quiet("ship", "GetDirtTimestamp", ITEM_NOT_FOUND_CODES, UserErrorKey::ItemNotFound),
    rule("invbroker", "GetInventory", CHAR_NOT_AT_STATION_CODES, RpcFaultAction::TypedInternal),
    rule("invbroker", "GetInventoryFromId", FAKE_ITEM_NOT_FOUND_CODES, RpcFaultAction::TypedInternal),
    rule("invbroker", "GetSelfInvItem", FAKE_ITEM_NOT_FOUND_CODES, RpcFaultAction::TypedInternal),
];

/// Applies [`RPC_FAULT_POLICY_TABLE`] to `result` for `(service, method)`.
pub fn apply_rpc_fault_policy(
    service: &str,
    method: &str,
    result: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    apply_fault_policy_with_rules(RPC_FAULT_POLICY_TABLE, service, method, result)
}

/// Applies an explicit `rules` slice to `result` for `(service, method)`.
///
/// Exposed at `pub(crate)` so unit tests can exercise all [`RpcFaultAction`] variants
/// deterministically without adding test-only entries to [`RPC_FAULT_POLICY_TABLE`].
pub(crate) fn apply_fault_policy_with_rules(
    rules: &[RpcFaultPolicyRule],
    service: &str,
    method: &str,
    result: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    let Err(mut err) = result else {
        return result;
    };
    let Some(policy) = rules.iter().find(|entry| {
        entry.service == service
            && entry.method == method
            && entry.error_codes.contains(&err.code.as_str())
    }) else {
        return Err(err);
    };

    match policy.action {
        RpcFaultAction::SoftEmpty(reply) => Ok(reply.to_pyrep()),
        RpcFaultAction::QuietUserError(key) => {
            if err.client_error.is_none() {
                let args = key.default_args_with_message(&err.message);
                err.client_error = Some(Box::new(ClientError::User { key, args }));
            }
            Err(err)
        }
        RpcFaultAction::TypedInternal => {
            if err.client_error.is_none() {
                err.client_error = Some(Box::new(ClientError::Internal(err.code.clone())));
            }
            Err(err)
        }
    }
}

/// Maps a failed `CallReq` [`BridgeRpcError`] onto `:26000` [`ClientError`], applies context-bound
/// argument enrichment, and records telemetry whenever the unmapped `_ => CustomInfo` fallback
/// fires (`#1195`).
pub(crate) fn map_call_req_error_with_telemetry(
    flight_recorder: &FlightRecorder,
    character_id: Option<i64>,
    service: &str,
    method: &str,
    err: &BridgeRpcError,
) -> (ClientError, bool) {
    let mapping = err.to_client_error_mapped(service, method);
    let mut client_err = mapping.client_error;
    if let ClientError::User {
        key: UserErrorKey::InsureShipFailedSingleContract,
        ref mut args,
    } = client_err
    {
        if !args.iter().any(|(k, _)| k == "ownerName") {
            if let Some(cid) = character_id {
                args.push(("ownerName".into(), PyRep::Integer(cid)));
            }
        }
    }
    if mapping.used_unmapped_custom_info_fallback {
        let count = flight_recorder.record_unmapped_custom_info_fallback(service, method, &err.code);
        if count == 1 || count % 100 == 0 {
            tracing::warn!(
                service,
                method,
                code = %err.code,
                count,
                "Unmapped BridgeRpcError fell back to wire CustomInfo (#1195)"
            );
        }
    }
    (client_err, mapping.used_unmapped_custom_info_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight_recorder::{
        FlightRecorder, MAX_UNMAPPED_CUSTOM_INFO_KEYS, UNMAPPED_CUSTOM_INFO_OVERFLOW_KEY,
    };
    use crate::router::rpc_registry::RpcRegistry;
    use eve_proto::{RejectionFacts, WarpPhaseKind};

    #[test]
    fn default_compiled_validates_fault_policy_table_and_rejects_invalid_rules() {
        let reg = RpcRegistry::default_compiled();
        assert!(reg.validate_fault_policy_table(RPC_FAULT_POLICY_TABLE).is_ok());
        for entry in RPC_FAULT_POLICY_TABLE {
            assert!(
                reg.lookup(entry.service, entry.method).is_some(),
                "fault policy target {}.{} missing from registry",
                entry.service,
                entry.method
            );
        }

        let unknown_rpc = [rule(
            // rpc-lint: allow synthetic unregistered service/method for table validation negative test
            "nonExistentFaultPolicySvc",
            // rpc-lint: allow synthetic unregistered method for table validation negative test
            "MissingMethod",
            &["ItemNotFound"],
            RpcFaultAction::QuietUserError(UserErrorKey::ItemNotFound),
        )];
        let err = reg
            .validate_fault_policy_table(&unknown_rpc)
            .expect_err("unknown (service, method) must fail validation");
        assert!(err.contains("unregistered RPC"));

        let empty_codes = [rule(
            "dogmaIM",
            "LoadAmmo",
            &[],
            RpcFaultAction::QuietUserError(UserErrorKey::ItemNotFound),
        )];
        let err = reg
            .validate_fault_policy_table(&empty_codes)
            .expect_err("empty error_codes must fail validation");
        assert!(err.contains("at least one error code"));

        let empty_code_str = [rule(
            "dogmaIM",
            "LoadAmmo",
            &[""],
            RpcFaultAction::QuietUserError(UserErrorKey::ItemNotFound),
        )];
        let err = reg
            .validate_fault_policy_table(&empty_code_str)
            .expect_err("empty error code string must fail validation");
        assert!(err.contains("empty error code"));

        let duplicate_code = [
            rule(
                "dogmaIM",
                "LoadAmmo",
                &["ItemNotFound"],
                RpcFaultAction::QuietUserError(UserErrorKey::ItemNotFound),
            ),
            rule(
                "dogmaIM",
                "LoadAmmo",
                &["ItemNotFound"],
                RpcFaultAction::TypedInternal,
            ),
        ];
        let err = reg
            .validate_fault_policy_table(&duplicate_code)
            .expect_err("duplicate (service, method, error_code) must fail validation");
        assert!(err.contains("duplicate fault policy rule"));

        let write_soft_empty = [rule(
            "dogmaIM",
            "LoadAmmo",
            &["ItemNotFound"],
            RpcFaultAction::SoftEmpty(SoftEmptyReply::None),
        )];
        let err = reg
            .validate_fault_policy_table(&write_soft_empty)
            .expect_err("SoftEmpty on Write RPC must fail validation");
        assert!(err.contains("SoftEmpty to Write RPC"));
    }

    #[test]
    fn apply_fault_policy_enforces_actions_passthrough_and_preserves_rejection_facts() {
        let custom_rules = [
            rule(
                "dogmaIM",
                "LoadAmmo",
                &["E_NONE"],
                RpcFaultAction::SoftEmpty(SoftEmptyReply::None),
            ),
            rule(
                "dogmaIM",
                "UnloadAmmo",
                &["E_LIST"],
                RpcFaultAction::SoftEmpty(SoftEmptyReply::EmptyList),
            ),
            rule(
                "ship",
                "GetDirtTimestamp",
                &["E_DICT"],
                RpcFaultAction::SoftEmpty(SoftEmptyReply::EmptyDict),
            ),
            rule(
                "invbroker",
                "GetInventory",
                &["E_ZERO"],
                RpcFaultAction::SoftEmpty(SoftEmptyReply::ZeroInt),
            ),
        ];
        assert_eq!(
            apply_fault_policy_with_rules(
                &custom_rules,
                "dogmaIM",
                "LoadAmmo",
                Err(BridgeRpcError {
                    code: "E_NONE".into(),
                    message: "E_NONE".into(),
                    client_error: None,
                })
            ),
            Ok(PyRep::None)
        );
        assert_eq!(
            apply_fault_policy_with_rules(
                &custom_rules,
                "dogmaIM",
                "UnloadAmmo",
                Err(BridgeRpcError {
                    code: "E_LIST".into(),
                    message: "E_LIST".into(),
                    client_error: None,
                })
            ),
            Ok(PyRep::List(Vec::new()))
        );
        assert_eq!(
            apply_fault_policy_with_rules(
                &custom_rules,
                "ship",
                "GetDirtTimestamp",
                Err(BridgeRpcError {
                    code: "E_DICT".into(),
                    message: "E_DICT".into(),
                    client_error: None,
                })
            ),
            Ok(PyRep::Dict(Vec::new()))
        );
        assert_eq!(
            apply_fault_policy_with_rules(
                &custom_rules,
                "invbroker",
                "GetInventory",
                Err(BridgeRpcError {
                    code: "E_ZERO".into(),
                    message: "E_ZERO".into(),
                    client_error: None,
                })
            ),
            Ok(PyRep::Integer(0))
        );

        let load_err = apply_rpc_fault_policy(
            "dogmaIM",
            "LoadAmmo",
            Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: "ItemNotFound".into(),
                client_error: None,
            }),
        )
        .expect_err("QuietUserError preserves domain Err");
        assert_eq!(load_err.code, "ItemNotFound");
        assert_eq!(
            load_err.client_error.as_deref(),
            Some(&ClientError::user(UserErrorKey::ItemNotFound))
        );

        let inv_err = apply_rpc_fault_policy(
            "invbroker",
            "GetInventory",
            Err(BridgeRpcError {
                code: "CharacterNotAtStation".into(),
                message: "CharacterNotAtStation".into(),
                client_error: None,
            }),
        )
        .expect_err("TypedInternal preserves domain Err");
        assert_eq!(inv_err.code, "CharacterNotAtStation");
        assert_eq!(
            inv_err.client_error.as_deref(),
            Some(&ClientError::Internal("CharacterNotAtStation".into()))
        );

        let fake_err = apply_rpc_fault_policy(
            "invbroker",
            "GetInventoryFromId",
            Err(BridgeRpcError {
                code: "FakeItemNotFound".into(),
                message: "FakeItemNotFound".into(),
                client_error: None,
            }),
        )
        .expect_err("TypedInternal preserves domain Err for FakeItemNotFound");
        assert_eq!(
            fake_err.client_error.as_deref(),
            Some(&ClientError::Internal("FakeItemNotFound".into()))
        );

        let passthrough = apply_rpc_fault_policy(
            "dogmaIM",
            "LoadAmmo",
            Err(BridgeRpcError {
                code: "SomeOtherDomainError".into(),
                message: "SomeOtherDomainError".into(),
                client_error: None,
            }),
        )
        .expect_err("unlisted code must pass through");
        assert!(passthrough.client_error.is_none());

        let pre_typed = BridgeRpcError::ship_already_in_warp_for_phase(
            Some(9001),
            "CmdWarpToStuff",
            WarpPhaseKind::Cruising,
            "Ship already in warp",
        );
        let preserved = apply_rpc_fault_policy("beyonce", "CmdWarpToStuff", Err(pre_typed))
            .expect_err("QuietUserError must preserve pre-attached RejectionFacts");
        assert_eq!(
            preserved.rejection_facts(),
            Some(&RejectionFacts::ShipInWarp {
                ship_id: Some(9001),
                action: "CmdWarpToStuff".into(),
                warp_phase: Some("Cruising".into()),
            })
        );
    }

    #[test]
    fn flight_recorder_tracks_and_caps_unmapped_custom_info_fallbacks() {
        let fr = FlightRecorder::new();
        assert_eq!(fr.unmapped_custom_info_fallback_total(), 0);
        assert!(fr.unmapped_custom_info_fallback_counts().is_empty());

        assert_eq!(
            fr.record_unmapped_custom_info_fallback("dogmaIM", "Activate", "UnknownErr"),
            1
        );
        assert_eq!(
            fr.record_unmapped_custom_info_fallback("dogmaIM", "Activate", "UnknownErr"),
            2
        );
        assert_eq!(fr.unmapped_custom_info_fallback_total(), 2);
        assert_eq!(
            fr.unmapped_custom_info_fallback_counts()
                .get("dogmaIM.Activate:UnknownErr")
                .copied(),
            Some(2)
        );

        let extra_overflow = 25usize;
        for idx in 1..=(MAX_UNMAPPED_CUSTOM_INFO_KEYS + extra_overflow) {
            let code = format!("SyntheticUnmappedCode{idx}");
            let _ = fr.record_unmapped_custom_info_fallback("ship", "GetDirtTimestamp", &code);
        }
        let counts = fr.unmapped_custom_info_fallback_counts();
        assert_eq!(counts.len(), MAX_UNMAPPED_CUSTOM_INFO_KEYS);
        assert_eq!(
            fr.unmapped_custom_info_fallback_total(),
            (2 + MAX_UNMAPPED_CUSTOM_INFO_KEYS + extra_overflow) as u64
        );
        // Existing `"dogmaIM.Activate:UnknownErr"` occupied 1 of the 1,023 individual slots,
        // so 1,022 of the synthetic codes entered individual slots and the remaining 27 spilled
        // into `"<overflow>"` while `"<overflow>"` itself occupied slot #1,024.
        assert_eq!(
            counts.get(UNMAPPED_CUSTOM_INFO_OVERFLOW_KEY).copied(),
            Some(27)
        );
        assert_eq!(
            fr.record_unmapped_custom_info_fallback("dogmaIM", "Activate", "UnknownErr"),
            3
        );
    }

    #[test]
    fn map_call_req_error_with_telemetry_enriches_insurance_and_counts_fallbacks() {
        let fr = FlightRecorder::new();
        let insurance_err =
            BridgeRpcError::from_user_key(UserErrorKey::InsureShipFailedSingleContract);
        let (mapped_ins, used_ins_fb) = map_call_req_error_with_telemetry(
            &fr,
            Some(4242),
            "insuranceSvc",
            "InsureShip",
            &insurance_err,
        );
        assert!(!used_ins_fb);
        assert_eq!(fr.unmapped_custom_info_fallback_total(), 0);
        match mapped_ins {
            ClientError::User { key, args } => {
                assert_eq!(key, UserErrorKey::InsureShipFailedSingleContract);
                assert!(args
                    .iter()
                    .any(|(k, v)| k == "ownerName" && *v == PyRep::Integer(4242)));
            }
            other => panic!("expected User error, got {other:?}"),
        }

        let unmapped = BridgeRpcError {
            code: "CompletelyUnknownTelemetryCode".into(),
            message: "CompletelyUnknownTelemetryCode".into(),
            client_error: None,
        };
        let (mapped_unmapped, used_fb) = map_call_req_error_with_telemetry(
            &fr,
            Some(4242),
            "charUnboundMgr",
            "SelectCharacterID",
            &unmapped,
        );
        assert!(used_fb);
        assert_eq!(
            mapped_unmapped,
            ClientError::CustomInfo(
                "CompletelyUnknownTelemetryCode: CompletelyUnknownTelemetryCode".into()
            )
        );
        assert_eq!(fr.unmapped_custom_info_fallback_total(), 1);
        assert_eq!(
            fr.unmapped_custom_info_fallback_counts()
                .get("charUnboundMgr.SelectCharacterID:CompletelyUnknownTelemetryCode")
                .copied(),
            Some(1)
        );
    }
}

#[cfg(test)]
#[path = "rpc_error_facts_t3a_tests.rs"]
mod rpc_error_facts_t3a_tests;

#[cfg(test)]
#[path = "rpc_error_facts_domain_services_tests.rs"]
mod rpc_error_facts_domain_services_tests;
