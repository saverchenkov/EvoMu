//! Track 3A verification suite for `#1195` live-client `BridgeRpcError` + `RejectionFacts`
//! migrations across the 26 disjoint service modules.

#![forbid(unsafe_code)]

use super::{map_call_req_error_with_telemetry, FlightRecorder};
use crate::router::rpc_registry::{RpcDispatchHandler, RpcEntry};
use crate::router::ServerContext;
use crate::services;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{
    FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, ClientError, PyRep, RejectionFacts, UserErrorKey};
use std::collections::HashMap;
use std::path::Path;

const CODE_INSUFFICIENT_ROLE: &str = "InsufficientRole";
const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
const CODE_MODULE_NOT_FOUND: &str = "ModuleNotFound";
const CODE_FITTING_VALIDATION_FAILED: &str = "FittingValidationFailed";
const CODE_INVALID_TARGET: &str = "InvalidTarget";

const T3A_SERVICE_FILES: &[&str] = &[
    "character_clone_jump_rules.rs",
    "character_lifecycle.rs",
    "combat_wrecks_loot_ops.rs",
    "config_service.rs",
    "corp_registry_burst_fallback.rs",
    "fitting_ship_bays_store_ops.rs",
    "fleet_social_access_group_bookmarks_ops.rs",
    "fleet_social_bookmarks.rs",
    "fleet_social_finder_tactical.rs",
    "fw_gameplay.rs",
    "insurgency_complex_ops.rs",
    "inventory_fitting_rpc.rs",
    "macho_cluster_mgmt.rs",
    "notify_emitters_fleet_combat.rs",
    "notify_emitters_operations_nodegraph.rs",
    "notify_emitters_skills_trade_godma.rs",
    "operations_manager.rs",
    "petitioner_ops.rs",
    "pve_fob_observatory.rs",
    "pve_hacking_sites.rs",
    "shipcaster_ops_starter.rs",
    "social_wars.rs",
    "sov_gameplay_entosis.rs",
    "structure_upwell_ops.rs",
    "time_control.rs",
    "wallet_serverops.rs",
];

// `"MustBeDocked"` is intentionally omitted from this guard list:
// `services/character_clone_jump_rules.rs:152,167` remains on raw `"MustBeDocked"`
// and is deferred to Track 3B because `crates/eve-proto/` is out of scope in
// Track 3A, `client_error_facts.rs` only exposes `must_be_docked_at_start_station`,
// and `client_error.rs:2202` already maps `"MustBeDocked"` to
// `(ClientError::user(UserErrorKey::MustBeDocked), false)` without `CustomInfo`
// fallback.
const AMENDED_LIVE_CLIENT_ERROR_CODES: &[&str] = &[
    "QuantumCoreRequired",
    "ShipAlreadyInWarp",
    "ShipInWarp",
    "CantActivateInWarp",
    "DeniedActivateInWarp",
    "NoAmmoLoaded",
    "NoChargesLoaded",
    "NotEnoughCapacitor",
    "ShipNotInSpace",
    "FittingValidationFailed",
    "CannotFitStackedModule",
    "InvalidTarget",
    "TargetingFailed",
    "InventoryNotAccessible",
    "MustBeDockedAtStartStation",
    "StructureNotDockable",
    "StructureServiceAccessDenied",
    "WeaponsTimerActive",
    "CannotJumpWithWeaponsTimer",
    "InvalidContractState",
    "ContractOwnershipViolation",
    "ModuleNotFound",
    "OwnershipOrLocationViolation",
    "InsufficientRole",
    "GMRoleRequired",
    "GmRoleRequired",
    "InsufficientRoles",
    "GM_InsufficientRole",
    "UserError: GM_InsufficientRole",
];

const AMENDED_RAW_USER_KEYS: &[&str] = &[
    "CantShipNotPresent",
    "ItemNotFound",
    "ModuleNoLongerPresentForCharges",
    "ModuleFitFailed",
    "InvalidTargetGroup",
    "InvalidTargetForModule",
    "DeniedTargetSelf",
    "WarpDestinationGone",
    "InsufficientRoles",
];

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn skip_char_literal(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'\'') {
        return None;
    }
    let mut i = start + 1;
    if bytes.get(i) == Some(&b'\\') {
        i += 2;
        while i < bytes.len() && bytes[i] != b'\'' {
            if bytes[i] == b'\n' || i - start > 10 {
                return None;
            }
            i += 1;
        }
        return (bytes.get(i) == Some(&b'\'')).then_some(i + 1);
    }
    // Single scalar char literal `'x'` (UTF-8 <= 4 bytes) vs lifetime `'a`
    let mut j = i;
    while j < bytes.len() && j - i < 4 && bytes[j] != b'\'' && bytes[j] != b'\n' {
        j += 1;
    }
    if j > i && bytes.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

fn skip_quoted_literal(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    Some(bytes.len())
}

fn skip_raw_string_literal(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    if bytes.get(i) == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let mut hashes = 0usize;
    while bytes.get(i) == Some(&b'#') {
        hashes += 1;
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut matched = 0usize;
            while matched < hashes && bytes.get(i + 1 + matched) == Some(&b'#') {
                matched += 1;
            }
            if matched == hashes {
                return Some(i + 1 + hashes);
            }
        }
        i += 1;
    }
    Some(bytes.len())
}

fn strip_rust_comments_keep_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        if let Some(end) = skip_raw_string_literal(bytes, i) {
            out.extend_from_slice(&bytes[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = skip_char_literal(bytes, i) {
            out.extend_from_slice(&bytes[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == b'"' {
            let end = skip_quoted_literal(bytes, i).unwrap_or(n);
            out.extend_from_slice(&bytes[i..end]);
            i = end;
            continue;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out.extend_from_slice(b"  ");
            i += 2;
            let mut depth = 1usize;
            while i < n && depth > 0 {
                if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    depth += 1;
                } else if i + 1 < n && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    depth -= 1;
                } else {
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).expect("UTF-8 preserved")
}

fn find_cfg_test_attr_end(bytes: &[u8], hash_idx: usize) -> Option<usize> {
    let mut j = hash_idx + 2;
    let mut depth = 1usize;
    while j < bytes.len() && depth > 0 {
        match bytes[j] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if depth != 0 {
        return None;
    }
    let inner = std::str::from_utf8(&bytes[hash_idx + 2..j]).ok()?;
    let compact: String = inner.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    (compact == "cfg(test)").then_some(j)
}

fn find_attributed_item_end(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    let mut pos = start;
    loop {
        while pos < n && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos + 1 < n && bytes[pos] == b'#' && bytes[pos + 1] == b'[' {
            pos += 2;
            let mut bd = 1usize;
            while pos < n && bd > 0 {
                match bytes[pos] {
                    b'[' => bd += 1,
                    b']' => bd -= 1,
                    _ => {}
                }
                pos += 1;
            }
            continue;
        }
        break;
    }
    while pos < n {
        if let Some(end) = skip_raw_string_literal(bytes, pos) {
            pos = end;
            continue;
        }
        if let Some(end) = skip_char_literal(bytes, pos) {
            pos = end;
            continue;
        }
        if bytes[pos] == b'"' {
            pos = skip_quoted_literal(bytes, pos).unwrap_or(n);
            continue;
        }
        match bytes[pos] {
            b';' => return pos + 1,
            b'{' => {
                let mut depth = 1usize;
                pos += 1;
                while pos < n && depth > 0 {
                    if let Some(end) = skip_raw_string_literal(bytes, pos) {
                        pos = end;
                        continue;
                    }
                    if let Some(end) = skip_char_literal(bytes, pos) {
                        pos = end;
                        continue;
                    }
                    if bytes[pos] == b'"' {
                        pos = skip_quoted_literal(bytes, pos).unwrap_or(n);
                        continue;
                    }
                    match bytes[pos] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    pos += 1;
                }
                return pos;
            }
            _ => pos += 1,
        }
    }
    n
}

fn strip_cfg_test_items_preserving_strings(no_comments: &str) -> String {
    let mut bytes = no_comments.as_bytes().to_vec();
    let n = bytes.len();
    let mut i = 0usize;
    while i < n {
        if let Some(end) = skip_raw_string_literal(&bytes, i) {
            i = end;
            continue;
        }
        if let Some(end) = skip_char_literal(&bytes, i) {
            i = end;
            continue;
        }
        if bytes[i] == b'"' {
            i = skip_quoted_literal(&bytes, i).unwrap_or(n);
            continue;
        }
        if i + 1 < n && bytes[i] == b'#' && bytes[i + 1] == b'[' {
            if let Some(attr_end) = find_cfg_test_attr_end(&bytes, i) {
                let item_end = find_attributed_item_end(&bytes, attr_end + 1);
                for b in &mut bytes[i..item_end] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
                i = item_end;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8(bytes).expect("UTF-8 preserved")
}

fn extract_delimited_span(bytes: &[u8], open_idx: usize, open_ch: u8, close_ch: u8) -> usize {
    let mut depth = 1usize;
    let mut i = open_idx + 1;
    while i < bytes.len() && depth > 0 {
        if let Some(end) = skip_raw_string_literal(bytes, i) {
            i = end;
            continue;
        }
        if let Some(end) = skip_char_literal(bytes, i) {
            i = end;
            continue;
        }
        if bytes[i] == b'"' {
            i = skip_quoted_literal(bytes, i).unwrap_or(bytes.len());
            continue;
        }
        if bytes[i] == open_ch {
            depth += 1;
        } else if bytes[i] == close_ch {
            depth -= 1;
        }
        i += 1;
    }
    i
}

fn collect_raw_amended_sites_in_production_src(rel_file: &str, src: &str) -> Vec<String> {
    let no_comments = strip_rust_comments_keep_strings(src);
    let prod = strip_cfg_test_items_preserving_strings(&no_comments);
    let bytes = prod.as_bytes();
    let n = bytes.len();
    let mut hits = Vec::new();

    let patterns: &[(&str, u8, u8, bool)] = &[
        ("BridgeRpcError", b'{', b'}', false),
        ("BridgeRpcError::with_client_error", b'(', b')', true),
        ("BridgeRpcError::user_error", b'(', b')', false),
        ("err", b'(', b')', false),
        ("rpc_err", b'(', b')', false),
        ("hacking_client_err", b'(', b')', false),
        ("session_attr_err", b'(', b')', false),
    ];

    for &(pat, open_ch, close_ch, check_raw_user_keys) in patterns {
        let mut cursor = 0usize;
        while let Some(rel) = prod[cursor..].find(pat) {
            let start = cursor + rel;
            let after = start + pat.len();
            cursor = after;
            let prev_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
            let prev_is_member =
                start > 0 && (bytes[start - 1] == b'.' || (start >= 2 && &bytes[start - 2..start] == b"::"));
            if !prev_ok || (prev_is_member && !pat.contains("::")) {
                continue;
            }
            let mut open_idx = after;
            while open_idx < n && bytes[open_idx].is_ascii_whitespace() {
                open_idx += 1;
            }
            if open_idx >= n || bytes[open_idx] != open_ch {
                continue;
            }
            if pat == "BridgeRpcError" && open_ch == b'{' {
                let preceding = prod[..start].trim_end();
                if preceding.ends_with("->") {
                    continue;
                }
            }
            let end_idx = extract_delimited_span(bytes, open_idx, open_ch, close_ch);
            let snippet = &prod[start..end_idx];
            if pat == "BridgeRpcError" && open_ch == b'{' && !snippet.contains("code:") {
                continue;
            }
            let line_no = prod[..start].bytes().filter(|&b| b == b'\n').count() + 1;
            for code in AMENDED_LIVE_CLIENT_ERROR_CODES {
                let quoted = format!("\"{code}\"");
                if snippet.contains(&quoted) {
                    hits.push(format!("{rel_file}:{line_no} raw {pat} uses {quoted}"));
                }
            }
            if check_raw_user_keys && snippet.contains("ClientError::user(") {
                for key in AMENDED_RAW_USER_KEYS {
                    if snippet.contains(&format!("UserErrorKey::{key}")) {
                        hits.push(format!(
                            "{rel_file}:{line_no} raw {pat} uses ClientError::user(UserErrorKey::{key}) without RejectionFacts"
                        ));
                    }
                }
            }
        }
    }

    hits
}

fn assert_wire_mapping_has_no_custom_info_fallback(
    service: &str,
    method: &str,
    char_id: Option<i64>,
    err: &BridgeRpcError,
) -> ClientError {
    let mapping = err.to_client_error_mapped(service, method);
    assert!(
        !mapping.used_unmapped_custom_info_fallback,
        "{service}.{method} ({}) must not fall back to unmapped CustomInfo",
        err.code
    );
    let fr = FlightRecorder::new();
    let (wire_err, used_fb) =
        map_call_req_error_with_telemetry(&fr, char_id, service, method, err);
    assert!(
        !used_fb,
        "{service}.{method} ({}) telemetry must not record unmapped CustomInfo fallback",
        err.code
    );
    assert_eq!(fr.unmapped_custom_info_fallback_total(), 0);
    assert_eq!(wire_err, mapping.client_error);
    mapping.client_error
}

fn assert_rejected_wire_facts<'a>(
    service: &str,
    method: &str,
    char_id: Option<i64>,
    err: &'a BridgeRpcError,
    expected_code: &str,
    expected_key: UserErrorKey,
) -> (&'a RejectionFacts, Vec<(String, PyRep)>) {
    assert_eq!(err.code, expected_code, "{service}.{method} code mismatch");
    let client_err = assert_wire_mapping_has_no_custom_info_fallback(service, method, char_id, err);
    match client_err {
        ClientError::Rejected { key, args, .. } => {
            assert_eq!(key, expected_key, "{service}.{method} UserErrorKey mismatch");
            let facts = err
                .rejection_facts()
                .expect("migrated error must carry RejectionFacts");
            (facts, args)
        }
        other => panic!(
            "{service}.{method} expected ClientError::Rejected({expected_key:?}), got {other:?}"
        ),
    }
}

async fn invoke_rpc_entry(
    entries: &[RpcEntry],
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let entry = entries
        .iter()
        .find(|e| e.service == service && e.method == method)
        .unwrap_or_else(|| panic!("missing RpcEntry for {service}.{method}"));
    match entry.handler {
        RpcDispatchHandler::Sync(f) => f(ctx, session, service, method, args, kwargs)
            .expect("handler must return Some"),
        RpcDispatchHandler::Async(f) => f(ctx, session, service, method, args, kwargs)
            .await
            .expect("handler must return Some"),
        RpcDispatchHandler::BoundAsync(f) => f(ctx, session, None, service, method, args, kwargs)
            .await
            .expect("handler must return Some"),
        other => panic!("unexpected handler kind for {service}.{method}: {other:?}"),
    }
}

#[test]
fn t3a_migrated_files_have_zero_raw_amended_error_sites() {
    let services_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services");
    assert_eq!(T3A_SERVICE_FILES.len(), 26);

    let mut violations = Vec::new();
    for rel_file in T3A_SERVICE_FILES {
        let path = services_dir.join(rel_file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        violations.extend(collect_raw_amended_sites_in_production_src(rel_file, &src));
    }
    assert!(
        violations.is_empty(),
        "found remaining raw #1195 live-client error sites in Track 3A files:\n{}",
        violations.join("\n")
    );
}

#[tokio::test]
async fn t3a_insufficient_role_handlers_emit_typed_facts() {
    let ctx = test_ctx();
    let uid = 91_101_u64;
    let char_id = 91_101_001_i64;
    let other_cid = 91_101_002_i64;
    let corp_id = 98_101_001_i64;
    let sys_id = 30_000_142_u32;
    let pet_uid = 91_102_u64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let seed = snap.characters.values().next().cloned().expect("seed char");
        let mut ch = seed.clone();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);

        let mut other = seed;
        other.character_id = other_cid;
        other.user_id = pet_uid;
        other.corporation_id = corp_id;
        other.solar_system_id = sys_id;
        snap.characters.insert(other_cid, other);
    });

    let mut other_session = auth_session(pet_uid, other_cid, corp_id, sys_id);
    ctx.sync_session(&mut other_session);
    let pet_reply = ctx
        .dispatch_rpc(
            &mut other_session,
            "petitioner",
            None,
            "CreatePetition",
            &[
                PyRep::String("Help".into()),
                PyRep::String("Stuck".into()),
                PyRep::Integer(401),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePetition for other_cid must succeed");
    let pet_id = pet_reply
        .dict_get("petitionID")
        .and_then(|v| v.as_i64())
        .expect("CreatePetition returns petitionID");

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    session.role = service_roles::ROLE_PLAYER_DEFAULT;
    ctx.sync_session(&mut session);

    let time_err = invoke_rpc_entry(
        services::time_control::RPCS,
        &ctx,
        &mut session,
        "timeControl",
        "AdvanceClock",
        &[PyRep::Integer(5)],
        &kw,
    )
    .await
    .expect_err("non-GM timeControl.AdvanceClock must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "timeControl",
        "AdvanceClock",
        Some(char_id),
        &time_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
            action: "timeControl.AdvanceClock".into(),
        }
    );

    let clear_err = invoke_rpc_entry(
        services::time_control::RPCS,
        &ctx,
        &mut session,
        "timeControl",
        "ClearSessionTimer",
        &[],
        &kw,
    )
    .await
    .expect_err("non-GM timeControl.ClearSessionTimer must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "timeControl",
        "ClearSessionTimer",
        Some(char_id),
        &clear_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let cfg_err = services::config_service::handle(
        &ctx,
        &mut session,
        "objectCaching",
        "InvalidateCachedObjects",
        &[],
        &kw,
    )
    .expect("handled")
    .expect_err("non-GM objectCaching.InvalidateCachedObjects must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "objectCaching",
        "InvalidateCachedObjects",
        Some(char_id),
        &cfg_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let mut qa_session = session.clone();
    qa_session.role = service_roles::ROLE_QA;
    let qa_err = services::character_lifecycle::handle(
        &ctx,
        &mut qa_session,
        "charUnboundMgr",
        "GetQAStarterSystemIDs",
        &[],
        &kw,
    )
    .expect("handled")
    .expect_err("QA role must not pass privileged charUnboundMgr GM gate");
    let (facts, _) = assert_rejected_wire_facts(
        "charUnboundMgr",
        "GetQAStarterSystemIDs",
        Some(char_id),
        &qa_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    let expected_priv_mask = service_roles::ROLE_GMH
        | service_roles::ROLE_GML
        | service_roles::ROLE_GMS
        | service_roles::ROLE_ADMIN;
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: expected_priv_mask,
            actual_role_mask: service_roles::ROLE_QA,
            action: "charUnboundMgr.GetQAStarterSystemIDs".into(),
        }
    );

    let macho_err = services::macho_cluster_mgmt::handle(
        &ctx,
        &mut session,
        "machoNet",
        "GetMetrics",
        &[],
        &kw,
    )
    .expect("handled")
    .expect_err("non-GM machoNet.GetMetrics must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "machoNet",
        "GetMetrics",
        Some(char_id),
        &macho_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let sess_mgr_err = services::macho_cluster_mgmt::handle(
        &ctx,
        &mut session,
        "sessionMgr",
        "EndAllGameSessions",
        &[],
        &kw,
    )
    .expect("handled")
    .expect_err("non-GM sessionMgr.EndAllGameSessions must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "sessionMgr",
        "EndAllGameSessions",
        Some(char_id),
        &sess_mgr_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK | eve_db::service_roles::ROLE_ADMIN,
            actual_role_mask: session.role,
            action: "sessionMgr.EndAllGameSessions".into(),
        }
    );

    let war_err = invoke_rpc_entry(
        services::social_corp_fleet::social_wars::RPCS,
        &ctx,
        &mut session,
        "warRegistry",
        "GMJoinDefender",
        &[],
        &kw,
    )
    .await
    .expect_err("non-GM warRegistry.GMJoinDefender must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "warRegistry",
        "GMJoinDefender",
        Some(char_id),
        &war_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: expected_priv_mask,
            actual_role_mask: session.role,
            action: "warRegistry.GMJoinDefender".into(),
        }
    );

    let upwell_err = ctx
        .dispatch_rpc(
            &mut session,
            "structureAssetSafety",
            None,
            "MoveEjectTimeGM",
            &[PyRep::Integer(1), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM structureAssetSafety.MoveEjectTimeGM must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "structureAssetSafety",
        "MoveEjectTimeGM",
        Some(char_id),
        &upwell_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let op_cat_err = ctx
        .dispatch_rpc(
            &mut session,
            "operationsManager",
            None,
            "complete_category_for_character",
            &[PyRep::Integer(other_cid), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM completing another char category must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "operationsManager",
        "complete_category_for_character",
        Some(char_id),
        &op_cat_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let pet_err = ctx
        .dispatch_rpc(
            &mut session,
            "petitioner",
            None,
            "CancelPetition",
            &[PyRep::Integer(pet_id)],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM cancelling another player's petition must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "petitioner",
        "CancelPetition",
        Some(char_id),
        &pet_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert!(matches!(
        facts,
        RejectionFacts::InsufficientRole {
            required_role_mask,
            actual_role_mask,
            ..
        } if *required_role_mask == service_roles::ROLE_GM_MASK && *actual_role_mask == session.role
    ));

    let mut no_role_session = session.clone();
    no_role_session.role = 0;
    let fsd_err = services::notify_emitters_operations_nodegraph::reload_operations_fsd_data(
        &ctx,
        &no_role_session,
        char_id,
    )
    .expect_err("role=0 reload_operations_fsd_data must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "operationsManager",
        "process_client_event",
        Some(char_id),
        &fsd_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_DEFAULT
                | service_roles::ROLE_CONTENT
                | service_roles::ROLE_PROGRAMMER,
            actual_role_mask: no_role_session.role,
            action: "reload_fsd_data".into(),
        }
    );
}

#[tokio::test]
async fn t3a_ship_not_in_space_handlers_emit_typed_facts() {
    let ctx = test_ctx();
    let uid = 91_201_u64;
    let char_id = 91_201_001_i64;
    let burst_uid = 91_202_u64;
    let burst_char_id = 91_201_003_i64;
    let target_cid = 91_201_002_i64;
    let corp_id = 98_201_001_i64;
    let enemy_corp_id = 98_201_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_912_000_100_i64;
    let other_ship_id = 1_912_000_101_i64;
    let host_ship_id = 1_912_000_102_i64;
    let analyzer_id = 1_912_000_200_i64;
    let burst_mod_id = 1_912_000_201_i64;
    let burst_only_ship_id = 1_912_000_103_i64;
    let enemy_struct_id = 1_912_000_300_i64;
    let fleet_id = 512_001_i64;
    let kw = HashMap::new();

    let analyzer_tid = ctx
        .sde
        .lowest_matching_type(|t| t.group_id == 538)
        .map(|t| t.type_id)
        .unwrap_or(22_586);
    let burst_tid = ctx
        .sde
        .lowest_matching_type(|t| t.group_id == 1770)
        .map(|t| t.type_id)
        .unwrap_or(42_990);

    ctx.db.with_mut_snapshot(|snap| {
        let seed = snap.characters.values().next().cloned().expect("seed char");
        let mut ch = seed.clone();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let mut bch = seed;
        bch.character_id = burst_char_id;
        bch.user_id = burst_uid;
        bch.corporation_id = corp_id;
        bch.solar_system_id = sys_id;
        bch.station_id = None;
        bch.structure_id = None;
        bch.active_ship_id = burst_only_ship_id;
        snap.characters.insert(burst_char_id, bch);

        for (sid, tid) in [
            (ship_id, 587_u32),
            (other_ship_id, 587_u32),
            (host_ship_id, 23_757_u32),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            analyzer_id,
            InventoryItemRecord {
                item_id: analyzer_id,
                type_id: analyzer_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_mod_id,
            InventoryItemRecord {
                item_id: burst_mod_id,
                type_id: burst_tid,
                owner_id: burst_char_id,
                location_id: burst_only_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            enemy_struct_id,
            UpwellStructureRecord {
                structure_id: enemy_struct_id,
                type_id: 35_832,
                owner_corp_id: enemy_corp_id,
                solar_system_id: sys_id,
                name: "Target Citadel".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id, burst_char_id],
                member_details: HashMap::from([
                    (
                        char_id,
                        FleetMemberRecord {
                            character_id: char_id,
                            role: FleetRole::FleetCommander,
                            job: 2,
                            ..Default::default()
                        },
                    ),
                    (
                        burst_char_id,
                        FleetMemberRecord {
                            character_id: burst_char_id,
                            role: FleetRole::SquadMember,
                            job: 0,
                            ..Default::default()
                        },
                    ),
                ]),
                ..Default::default()
            },
        );
    });

    // 1. fleet_social_finder_tactical: LockFromBroadcast while docked
    let mut docked_fleet_sess = auth_session(uid, char_id, corp_id, sys_id);
    docked_fleet_sess.station_id = Some(60_003_760);
    docked_fleet_sess.ship_id = Some(ship_id);
    docked_fleet_sess.fleet_id = Some(fleet_id);
    let lock_err = services::fleet_social::fleet_social_finder_tactical::handle_fleet_watchlist_ops(
        &ctx,
        &mut docked_fleet_sess,
        "LockFromBroadcast",
        &[PyRep::Integer(target_cid)],
    )
    .await
    .expect("handled")
    .expect_err("docked LockFromBroadcast must fail with ShipNotInSpace");
    let (facts, _) = assert_rejected_wire_facts(
        "fleetObjectHandler",
        // rpc-lint: allow existing fleet_social_finder_tactical::handle_fleet_watchlist_ops test method
        "LockFromBroadcast",
        Some(char_id),
        &lock_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(sys_id),
            action: "lock_broadcast_target".into(),
        }
    );

    // 2. pve_hacking_sites: StartNewGameInstance with ship ball absent (must NOT emit CustomInfo!)
    let mut space_sess = auth_session(uid, char_id, corp_id, sys_id);
    space_sess.ship_id = Some(ship_id);
    ctx.sync_session(&mut space_sess);
    let hack_err = ctx
        .dispatch_rpc(
            &mut space_sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(enemy_struct_id)],
            &kw,
            None,
        )
        .await
        .expect_err("hacking without ship ball in space must fail");
    assert!(
        !matches!(hack_err.client_error.as_deref(), Some(ClientError::CustomInfo(_))),
        "pve_hacking_sites ShipNotInSpace must no longer wrap CustomInfo"
    );
    let (facts, _) = assert_rejected_wire_facts(
        "hackingMgr",
        "StartNewGameInstance",
        Some(char_id),
        &hack_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(sys_id),
            action: "hacking_site".into(),
        }
    );

    // 3. fleet_social_bookmarks: BookmarkLocation when ship ball is absent
    let bm_err = ctx
        .dispatch_rpc(
            &mut space_sess,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Safe Spot".into())],
            &kw,
            None,
        )
        .await
        .expect_err("in-space BookmarkLocation without ship ball must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "bookmark",
        "BookmarkLocation",
        Some(char_id),
        &bm_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(sys_id),
            action: "bookmark_in_space".into(),
        }
    );

    // 4. fitting_ship_bays_store_ops: StoreVessel when active/host balls are absent
    let store_err = ctx
        .dispatch_rpc(
            &mut space_sess,
            "ship",
            None,
            "StoreVessel",
            &[PyRep::Integer(other_ship_id), PyRep::Integer(host_ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("in-space StoreVessel without ship balls must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "ship",
        "StoreVessel",
        Some(char_id),
        &store_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(sys_id),
            action: "ship_bay_access".into(),
        }
    );

    // 5. corp_registry_burst_fallback: dogmaIM.Activate burst fallback when ball absent
    let mut burst_sess = auth_session(burst_uid, burst_char_id, corp_id, sys_id);
    burst_sess.ship_id = Some(burst_only_ship_id);
    burst_sess.fleet_id = Some(fleet_id);
    ctx.sync_session(&mut burst_sess);
    let burst_err = ctx
        .dispatch_rpc(
            &mut burst_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(burst_mod_id)],
            &kw,
            None,
        )
        .await
        .expect_err("burst fallback without ship ball must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(burst_char_id),
        &burst_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id: burst_only_ship_id,
            char_id: Some(burst_char_id),
            solar_system_id: Some(sys_id),
            action: "command_burst".into(),
        }
    );
}

#[tokio::test]
async fn t3a_module_not_found_handlers_emit_typed_facts() {
    let ctx = test_ctx();
    let uid = 91_301_u64;
    let char_id = 91_301_001_i64;
    let corp_id = 98_301_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_913_000_100_i64;
    let titan_ship_id = 1_913_000_101_i64;
    let weapon_id = 1_913_000_200_i64;
    let missing_mod_id = 999_888_777_i64;
    let kw = HashMap::new();

    let weapon_tid = ctx
        .sde
        .lowest_matching_type(|t| t.group_id == 53)
        .map(|t| t.type_id)
        .unwrap_or(484);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(60_003_760);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            titan_ship_id,
            InventoryItemRecord {
                item_id: titan_ship_id,
                type_id: 671, // Erebus Titan
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            weapon_id,
            InventoryItemRecord {
                item_id: weapon_id,
                type_id: weapon_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(weapon_id, true);
    });

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    session.station_id = Some(60_003_760);
    session.ship_id = Some(ship_id);
    ctx.sync_session(&mut session);

    // 1. inventory_fitting_rpc: dogmaIM.LaunchProbes unknown module on armed ship
    let probe_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "LaunchProbes",
            &[PyRep::Integer(missing_mod_id)],
            &kw,
            None,
        )
        .await
        .expect_err("LaunchProbes with unknown moduleID must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "LaunchProbes",
        Some(char_id),
        &probe_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: missing_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: None,
        }
    );

    // 2. character_clone_jump_rules: activate_clone_vat_bay unknown module
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let vat_err = services::character_clone_jump_rules::activate_clone_vat_bay(
        &ctx,
        &sys_handle,
        char_id,
        titan_ship_id,
        missing_mod_id,
    )
    .await
    .expect_err("activate_clone_vat_bay with missing module must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &vat_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert!(matches!(
        facts,
        RejectionFacts::ModuleNotFound {
            module_id,
            owner_entity_id: Some(owner),
            ..
        } if *module_id == missing_mod_id && *owner == titan_ship_id
    ));
}

#[tokio::test]
async fn t3a_fitting_validation_failed_and_burned_out_godma_handlers() {
    let ctx = test_ctx();
    let uid = 91_401_u64;
    let char_id = 91_401_001_i64;
    let corp_id = 98_401_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_914_000_100_i64;
    let cargo_mod_id = 1_914_000_200_i64;
    let huge_mod_id = 1_914_000_201_i64;
    let burned_mod_id = 1_914_000_202_i64;
    let kw = HashMap::new();

    let low_mod = ctx
        .sde
        .lowest_matching_type(|t| {
            t.effect_ids.contains(&11) && t.default_attributes.get(&30).copied().unwrap_or(0.0) <= 5.0
        })
        .map(|t| {
            let hp = t
                .default_attributes
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(40.0);
            (t.type_id, t.name.clone(), hp)
        })
        .unwrap_or((1248, "Module".into(), 40.0));
    let huge_mod = ctx
        .sde
        .lowest_matching_type(|t| {
            t.effect_ids.contains(&11) && t.default_attributes.get(&30).copied().unwrap_or(0.0) > 500.0
        })
        .map(|t| (t.type_id, t.name.clone()));

    ctx.db.with_mut_snapshot(|snap| {
        let seed = snap.characters.values().next().cloned().expect("seed char");
        let mut ch = seed.clone();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_mod_id,
            InventoryItemRecord {
                item_id: cargo_mod_id,
                type_id: low_mod.0,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some((htid, _)) = &huge_mod {
            snap.items.insert(
                huge_mod_id,
                InventoryItemRecord {
                    item_id: huge_mod_id,
                    type_id: *htid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::LoSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(huge_mod_id, false);
        }
        let mut burned_attrs = HashMap::new();
        burned_attrs.insert(eve_sde::attr_ids::DAMAGE, low_mod.2.max(40.0));
        snap.items.insert(
            burned_mod_id,
            InventoryItemRecord {
                item_id: burned_mod_id,
                type_id: low_mod.0,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                dynamic_attributes: burned_attrs,
                ..Default::default()
            },
        );
    });

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    session.ship_id = Some(ship_id);
    ctx.sync_session(&mut session);

    // 1. Cargo module SetModuleOnline -> FittingValidationFailed + ModuleFitFailed args
    let cargo_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "SetModuleOnline",
            &[PyRep::Integer(ship_id), PyRep::Integer(cargo_mod_id)],
            &kw,
            None,
        )
        .await
        .expect_err("cargo module SetModuleOnline must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "SetModuleOnline",
        Some(char_id),
        &cargo_err,
        CODE_FITTING_VALIDATION_FAILED,
        UserErrorKey::ModuleFitFailed,
    );
    assert!(args.iter().any(|(k, v)| k == "moduleName" && v == &PyRep::String(low_mod.1.clone())));
    assert!(args.iter().any(|(k, v)| k == "reason" && matches!(v, PyRep::String(s) if !s.is_empty())));
    assert!(matches!(
        facts,
        RejectionFacts::FittingValidationFailed {
            ship_id: Some(sid),
            item_id: Some(iid),
            reason,
            ..
        } if *sid == ship_id && *iid == cargo_mod_id && !reason.is_empty()
    ));

    // 2. Fitted overloaded/missing-skill module SetModuleOnline -> carries module_type_id
    if let Some((htid, ref hname)) = huge_mod {
        let huge_err = ctx
            .dispatch_rpc(
                &mut session,
                "dogmaIM",
                None,
                "SetModuleOnline",
                &[PyRep::Integer(ship_id), PyRep::Integer(huge_mod_id)],
                &kw,
                None,
            )
            .await
            .expect_err("oversized fitted module SetModuleOnline must fail");
        let (facts, args) = assert_rejected_wire_facts(
            "dogmaIM",
            "SetModuleOnline",
            Some(char_id),
            &huge_err,
            CODE_FITTING_VALIDATION_FAILED,
            UserErrorKey::ModuleFitFailed,
        );
        assert!(args.iter().any(|(k, v)| k == "moduleName" && v == &PyRep::String(hname.clone())));
        assert_eq!(
            facts,
            &RejectionFacts::FittingValidationFailed {
                ship_id: Some(ship_id),
                item_id: Some(huge_mod_id),
                module_type_id: Some(i64::from(htid)),
                reason: huge_err.message.clone(),
            }
        );
    }

    // 3. TakeModuleOffline on non-fitted item -> FittingValidationFailed
    let offline_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "TakeModuleOffline",
            &[PyRep::Integer(ship_id), PyRep::Integer(cargo_mod_id)],
            &kw,
            None,
        )
        .await
        .expect_err("cargo module TakeModuleOffline must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "TakeModuleOffline",
        Some(char_id),
        &offline_err,
        CODE_FITTING_VALIDATION_FAILED,
        UserErrorKey::ModuleFitFailed,
    );
    assert!(args.iter().any(|(k, _)| k == "moduleName"));
    assert!(args.iter().any(|(k, _)| k == "reason"));
    assert!(matches!(
        facts,
        RejectionFacts::FittingValidationFailed {
            ship_id: Some(sid),
            item_id: Some(iid),
            ..
        } if *sid == ship_id && *iid == cargo_mod_id
    ));

    // 4. ModuleBurnedOut branch must remain untouched
    let burned_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "SetModuleOnline",
            &[PyRep::Integer(ship_id), PyRep::Integer(burned_mod_id)],
            &kw,
            None,
        )
        .await
        .expect_err("burned module in space must return ModuleBurnedOut");
    assert_eq!(burned_err.code, "ModuleBurnedOut");
    assert!(burned_err.rejection_facts().is_none());
}

#[tokio::test]
async fn t3a_invalid_target_pos_change_provision_type_emits_typed_facts() {
    let ctx = test_ctx();
    let uid = 91_501_u64;
    let char_id = 91_501_001_i64;
    let corp_id = 98_501_001_i64;
    let sys_id = 30_000_142_u32;
    let tower_id = 1_915_000_100_i64;
    let non_silo_id = 1_915_000_101_i64;
    let missing_id = 1_915_000_999_i64;
    let kw = HashMap::new();

    let moon_mat_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 427)
        .map(|t| t.type_id)
        .expect("canonical SDE includes a group 427 moon material type");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            non_silo_id,
            InventoryItemRecord {
                item_id: non_silo_id,
                type_id: 34, // Tritanium, not anchored silo 14343
                owner_id: corp_id,
                location_id: tower_id,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    ctx.sync_session(&mut session);

    let non_silo_err = ctx
        .dispatch_rpc(
            &mut session,
            "posMgr",
            None,
            "ChangeStructureProvisionType",
            &[
                PyRep::Integer(tower_id),
                PyRep::Integer(non_silo_id),
                PyRep::Integer(i64::from(moon_mat_tid)),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-silo item must fail with InvalidTarget");
    let (facts, args) = assert_rejected_wire_facts(
        "posMgr",
        "ChangeStructureProvisionType",
        Some(char_id),
        &non_silo_err,
        CODE_INVALID_TARGET,
        UserErrorKey::InvalidTargetGroup,
    );
    assert!(args.iter().any(|(k, v)| k == "groupName" && v == &PyRep::String("Silo".into())));
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(non_silo_id),
            module_id: None,
            reason: "not_anchored_silo".into(),
        }
    );

    let missing_err = ctx
        .dispatch_rpc(
            &mut session,
            "posMgr",
            None,
            "ChangeStructureProvisionType",
            &[
                PyRep::Integer(tower_id),
                PyRep::Integer(missing_id),
                PyRep::Integer(i64::from(moon_mat_tid)),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("missing target item must fail with InvalidTarget");
    let (facts, args) = assert_rejected_wire_facts(
        "posMgr",
        "ChangeStructureProvisionType",
        Some(char_id),
        &missing_err,
        CODE_INVALID_TARGET,
        UserErrorKey::InvalidTargetGroup,
    );
    assert!(args.iter().any(|(k, v)| k == "groupName" && v == &PyRep::String("Silo".into())));
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(missing_id),
            module_id: None,
            reason: "target_not_found".into(),
        }
    );
}
