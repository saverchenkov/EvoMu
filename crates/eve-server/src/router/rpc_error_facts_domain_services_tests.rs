//! Domain verification suite for `#1195` live-client `BridgeRpcError` + `RejectionFacts`
//! migrations across the disjoint Wave 2 router/service modules.

#![forbid(unsafe_code)]

use super::{map_call_req_error_with_telemetry, FlightRecorder};
use crate::router::rpc_registry::{RpcDispatchHandler, RpcEntry};
use crate::router::ServerContext;
use crate::services;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::station_structure::structure_lifecycle::{
    self as structure_life, structure_profile, CATEGORY_STRUCTURE,
};
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use crate::services::structure_standup_weapons::{
    try_activate_standup_weapon, GROUP_STANDUP_FLAK_AMMO, GROUP_STANDUP_POINT_DEFENSE_BATTERY,
    TYPE_KEEPSTAR,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    ContractRecord, CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NpeProgressRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, ClientError, PyRep, RejectionFacts, UserErrorKey};
use eve_sde::{
    CelestialInfo, InvType, SdeRegistry, SolarSystem, StationOperationInfo, TypeListInfo,
    TypeMaterialEntry,
};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
const CODE_TARGETING_FAILED: &str = "TargetingFailed";
const CODE_INVALID_TARGET: &str = "InvalidTarget";
const CODE_OWNERSHIP_OR_LOCATION: &str = "OwnershipOrLocationViolation";
const CODE_CONTRACT_OWNERSHIP: &str = "ContractOwnershipViolation";
const CODE_INVALID_CONTRACT_STATE: &str = "InvalidContractState";
const CODE_NOT_ENOUGH_CAPACITOR: &str = "NotEnoughCapacitor";
const CODE_WEAPONS_TIMER_ACTIVE: &str = "WeaponsTimerActive";
const CODE_SHIP_IN_WARP: &str = "ShipInWarp";
const CODE_MODULE_NOT_FOUND: &str = "ModuleNotFound";
const CODE_INSUFFICIENT_ROLE: &str = "InsufficientRole";
const CODE_GM_ROLE_REQUIRED: &str = "GMRoleRequired";
const CODE_INSUFFICIENT_ROLES: &str = "InsufficientRoles";

const MIGRATED_LIVE_ERROR_FILES: &[&str] = &[
    "router/session.rs",
    "services/abyssal_void_gates.rs",
    "services/character_expert_systems.rs",
    "services/cosmetics_events.rs",
    "services/cosmetics_ui_ops.rs",
    "services/encounter_surveillance_system.rs",
    "services/fitting_cycles_overheat.rs",
    "services/fitting_dogma_im.rs",
    "services/fitting_saved_multifit.rs",
    "services/fitting_scanners_superweapons.rs",
    "services/market_contracts.rs",
    "services/mining_compression.rs",
    "services/notify_emitters_seasons_tactical.rs",
    "services/serverops_cluster.rs",
    "services/social_corp_fleet.rs",
    "services/sov_gameplay.rs",
    "services/sov_gameplay_skyhook.rs",
    "services/sov_services.rs",
    "services/space_destiny.rs",
    "services/space_entity_mining.rs",
    "services/structure_lifecycle_cargo_fuel.rs",
    "services/structure_lifecycle_rpc.rs",
    "services/structure_standup_weapons.rs",
    "sp6_lifecycle.rs",
];

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
        ("BridgeRpcError::from_user_key", b'(', b')', true),
        ("BridgeRpcError::user_error", b'(', b')', false),
        ("err", b'(', b')', false),
        ("rpc_err", b'(', b')', false),
        ("alliance_rpc_error", b'(', b')', false),
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
            let allow_qualified_struct =
                pat == "BridgeRpcError" && start >= 2 && &bytes[start - 2..start] == b"::";
            if !prev_ok || (prev_is_member && !pat.contains("::") && !allow_qualified_struct) {
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
            if check_raw_user_keys
                && (snippet.contains("ClientError::user(")
                    || snippet.contains("ClientError::user_with_args(")
                    || pat == "BridgeRpcError::from_user_key")
            {
                for key in AMENDED_RAW_USER_KEYS {
                    // Generic inventory item lookups (e.g. charge stacks) legitimately use
                    // `BridgeRpcError::from_user_key(UserErrorKey::ItemNotFound)`; `ModuleNotFound`
                    // and `with_client_error` remain guarded above.
                    if pat == "BridgeRpcError::from_user_key" && *key == "ItemNotFound" {
                        continue;
                    }
                    if snippet.contains(&format!("UserErrorKey::{key}")) {
                        hits.push(format!(
                            "{rel_file}:{line_no} raw {pat} uses UserErrorKey::{key} without RejectionFacts"
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

fn assert_ship_not_in_space(
    service: &str,
    method: &str,
    char_id: Option<i64>,
    err: &BridgeRpcError,
    expected_ship_id: i64,
    expected_char_id: Option<i64>,
    expected_sys_id: Option<u32>,
    expected_action: &str,
) {
    let (facts, _) = assert_rejected_wire_facts(
        service,
        method,
        char_id,
        err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id: expected_ship_id,
            char_id: expected_char_id,
            solar_system_id: expected_sys_id,
            action: expected_action.into(),
        }
    );
}

#[allow(clippy::too_many_arguments)]
fn assert_ownership_violation(
    service: &str,
    method: &str,
    char_id: Option<i64>,
    err: &BridgeRpcError,
    expected_item_id: i64,
    expected_owner_id: Option<i64>,
    expected_location_id: Option<i64>,
    expected_required_owner_id: Option<i64>,
    expected_required_location_id: Option<i64>,
) {
    let (facts, _) = assert_rejected_wire_facts(
        service,
        method,
        char_id,
        err,
        CODE_OWNERSHIP_OR_LOCATION,
        UserErrorKey::CantDoThatWithSomeoneElsesStuff,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: expected_item_id,
            owner_id: expected_owner_id,
            location_id: expected_location_id,
            required_owner_id: expected_required_owner_id,
            required_location_id: expected_required_location_id,
        }
    );
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

fn seed_char_in_ctx(
    ctx: &ServerContext,
    uid: u64,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    ship_id: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
    });
}

fn upsert_test_type(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .or_else(|| reg.types.values().next())
        .cloned()
        .expect("base SDE type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn ensure_nullsec_system(sde: &mut Arc<SdeRegistry>) -> u32 {
    if let Some(sys_id) = sde
        .solar_systems
        .values()
        .find(|s| services::sov_gameplay::is_claimable_nullsec_system(sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
    {
        return sys_id;
    }
    let sys_id = 30_092_101_u32;
    Arc::make_mut(sde).solar_systems.insert(
        sys_id,
        SolarSystem {
            solar_system_id: sys_id,
            constellation_id: 20_992_101,
            region_id: 10_992_101,
            name: "NullsecTest".into(),
            security_status: -0.5,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sys_id
}

#[test]
fn live_client_error_services_have_zero_raw_amended_sites() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_eq!(MIGRATED_LIVE_ERROR_FILES.len(), 24);

    let mut violations = Vec::new();
    for rel_file in MIGRATED_LIVE_ERROR_FILES {
        let path = src_dir.join(rel_file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        violations.extend(collect_raw_amended_sites_in_production_src(rel_file, &src));
    }
    assert!(
        violations.is_empty(),
        "found remaining raw #1195 live-client error sites in migrated files:\n{}",
        violations.join("\n")
    );

    let session_src = std::fs::read_to_string(src_dir.join("router/session.rs"))
        .expect("read router/session.rs");
    let fn_start = session_src
        .find("pub fn require_undocked_space_system")
        .expect("require_undocked_space_system present");
    let fn_end = session_src[fn_start..]
        .find("pub fn require_undocked_space_ship")
        .map(|off| fn_start + off)
        .expect("require_undocked_space_ship present");
    let fn_body = &session_src[fn_start..fn_end];
    assert!(
        fn_body.contains("docked_code == \"ShipNotInSpace\"")
            && fn_body.contains("BridgeRpcError::ship_not_in_space("),
        "require_undocked_space_system must map ShipNotInSpace via BridgeRpcError::ship_not_in_space instead of CustomInfo"
    );
}

#[tokio::test]
async fn session_and_space_ship_not_in_space_handlers_emit_typed_facts() {
    let mut ctx = test_ctx();
    let uid = 92_101_u64;
    let char_id = 92_101_001_i64;
    let corp_id = 98_201_011_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_921_000_100_i64;
    let null_sys_id = ensure_nullsec_system(&mut ctx.sde);
    seed_char_in_ctx(&ctx, uid, char_id, corp_id, null_sys_id, Some(station_id), ship_id);

    let mut docked_sess = auth_session(uid, char_id, corp_id, null_sys_id);
    docked_sess.station_id = Some(station_id);
    docked_sess.ship_id = Some(ship_id);

    let sess_err = ctx
        .require_undocked_space_system(
            &docked_sess,
            CODE_SHIP_NOT_IN_SPACE,
            "Cannot launch drones while docked",
            true,
            false,
        )
        .expect_err("docked session with ShipNotInSpace must fail");
    assert!(
        !matches!(sess_err.client_error.as_deref(), Some(ClientError::CustomInfo(_))),
        "require_undocked_space_system must never wrap ShipNotInSpace in CustomInfo"
    );
    let (facts, _) = assert_rejected_wire_facts(
        "ship",
        "LaunchDrones",
        Some(char_id),
        &sess_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(null_sys_id),
            action: "require_undocked_space_system".into(),
        }
    );

    let non_amended = ctx
        .require_undocked_space_system(&docked_sess, "ShipDocked", "docked", true, false)
        .expect_err("non-ShipNotInSpace code still returns ShipDocked");
    assert_eq!(non_amended.code, "ShipDocked");

    // ESS link handler when active ship ball is absent from Ballpark
    let mut space_sess = auth_session(uid, char_id, corp_id, null_sys_id);
    space_sess.ship_id = Some(ship_id);
    let ess_err = services::encounter_surveillance_system::handle_ess_link(
        &ctx,
        &space_sess,
        "AttemptLinkToMainBank",
        &[],
    )
    .await
    .expect_err("ESS link without ship ball in space must fail with ShipNotInSpace");
    let (facts, _) = assert_rejected_wire_facts(
        "essMgr",
        "AttemptLinkToMainBank",
        Some(char_id),
        &ess_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: Some(char_id),
            solar_system_id: Some(null_sys_id),
            action: "ess_link".into(),
        }
    );

    // Mercenary Den shield repair when ship ball is absent from Ballpark
    let repair_err = services::sov_gameplay::apply_mercenary_den_shield_repair(
        &ctx,
        null_sys_id,
        ship_id,
        1_921_000_300,
        250.0,
    )
    .await
    .expect_err("shield repair without ship ball must fail with ShipNotInSpace");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &repair_err,
        CODE_SHIP_NOT_IN_SPACE,
        UserErrorKey::CantShipNotPresent,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ShipNotInSpace {
            ship_id,
            char_id: None,
            solar_system_id: Some(null_sys_id),
            action: "mercenary_den_shield_repair".into(),
        }
    );

    // Remaining ShipNotInSpace handlers across sov_gameplay, sov_gameplay_skyhook,
    // structure_lifecycle_rpc, space_entity_mining, structure_lifecycle_cargo_fuel, and mining_compression
    let merc_dock_err = services::sov_gameplay::handle_anchor_mercenary_den_rpc(&ctx, &mut docked_sess, &[])
        .await
        .expect_err("docked Mercenary Den anchor must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &merc_dock_err, ship_id, Some(char_id), Some(null_sys_id), "anchor_mercenary_den");

    let merc_space_err = services::sov_gameplay::handle_anchor_mercenary_den_rpc(&ctx, &mut space_sess, &[])
        .await
        .expect_err("unspawned Mercenary Den anchor must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &merc_space_err, ship_id, Some(char_id), Some(null_sys_id), "anchor_mercenary_den");

    let merc_comp_err = services::sov_gameplay::handle_mercden_component_call(
        &ctx, &space_sess, null_sys_id, char_id, ship_id, 1_921_000_301, "mercenaryDen", "extract_infomorphs", &[],
    ).await.expect_err("unspawned mercden component call must fail");
    assert_ship_not_in_space("beyonce", "CallComponentFromClient", Some(char_id), &merc_comp_err, ship_id, Some(char_id), Some(null_sys_id), "mercden_component_call");

    let hub_err = services::sov_gameplay::handle_anchor_sov_hub_rpc(&ctx, &mut space_sess, &[])
        .await
        .expect_err("unspawned Sov Hub anchor must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &hub_err, ship_id, Some(char_id), Some(null_sys_id), "anchor_sov_hub");

    let sky_dock_err = services::sov_gameplay::handle_anchor_skyhook_rpc(&ctx, &mut docked_sess, &[])
        .await
        .expect_err("docked Skyhook anchor must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &sky_dock_err, ship_id, Some(char_id), Some(null_sys_id), "anchor_skyhook");

    let mut no_ship_sess = auth_session(uid, char_id, corp_id, null_sys_id);
    no_ship_sess.ship_id = None;
    let sky_noship_err = services::sov_gameplay::handle_anchor_skyhook_rpc(&ctx, &mut no_ship_sess, &[])
        .await
        .expect_err("Skyhook anchor without active ship must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &sky_noship_err, 0, Some(char_id), Some(null_sys_id), "anchor_skyhook");

    let planet_id = 40_992_101_i64;
    if !ctx.sde.celestials_in_system(null_sys_id).iter().any(|c| c.group_id == 7) {
        let reg = Arc::make_mut(&mut ctx.sde);
        Arc::make_mut(&mut reg.celestials_by_system)
            .entry(null_sys_id)
            .or_default()
            .push(CelestialInfo {
                celestial_id: planet_id,
                solar_system_id: null_sys_id,
                type_id: 11,
                group_id: 7,
                name: "SkyhookPlanet".into(),
                radius: 1_000_000.0,
                position: DVec3::ZERO,
                orbit_id: None,
                orbit_index: Some(1),
                celestial_index: Some(1),
            });
        Arc::make_mut(&mut reg.celestial_system_by_id).insert(planet_id, null_sys_id);
    }
    let test_planet_id = ctx.sde.celestials_in_system(null_sys_id).into_iter().find(|c| c.group_id == 7).expect("planet").celestial_id;
    let st_id = 1_092_101_001_i64;
    let mine_err = services::space_entity_mining::handle_mine(&ctx, &space_sess, None, &[PyRep::Integer(80_000_001)])
        .await
        .expect_err("CmdMineRepeatedly without spawned ship must fail");
    assert_ship_not_in_space("entity", "CmdMineRepeatedly", Some(char_id), &mine_err, ship_id, Some(char_id), Some(null_sys_id), "CmdMineRepeatedly");

    if structure_profile(&ctx, 35_832).is_none() {
        upsert_test_type(&mut ctx.sde, 35_834, 35_832, 1_657, CATEGORY_STRUCTURE, &[(structure_life::ATTR_RIG_SIZE, 2.0), (structure_life::ATTR_SHIELD_CAPACITY, 500_000.0), (structure_life::ATTR_ARMOR_HP, 500_000.0), (structure_life::ATTR_HP, 500_000.0)]);
    }
    let st_anchor_err = services::station_structure::structure_lifecycle_rpc::handle_anchor(&ctx, &mut space_sess, &[PyRep::Integer(35_832), PyRep::String("Astra".into())])
        .await
        .expect_err("structure anchor without spawned ship must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &st_anchor_err, ship_id, Some(char_id), Some(null_sys_id), "structure_deployment");

    let comp_char_id = 92_101_002_i64;
    let comp_ship_id = 1_921_000_102_i64;
    let comp_mod_id = 1_921_000_103_i64;
    let comp_ore_item_id = 1_921_000_104_i64;
    let comp_ore_tid = 951_901_u32;
    let comp_out_tid = 951_902_u32;
    let comp_mod_tid = 951_903_u32;
    let comp_tl_id = 951_904_u32;
    let cargo_item_id = 1_921_000_105_i64;
    upsert_test_type(&mut ctx.sde, 1_230, comp_ore_tid, 462, 25, &[]);
    upsert_test_type(&mut ctx.sde, 28_432, comp_out_tid, 462, 25, &[]);
    upsert_test_type(&mut ctx.sde, 3_651, comp_mod_tid, 4_174, 7, &[(3255, f64::from(comp_tl_id)), (3265, 0.0), (54, 50_000.0)]);
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.compressible_types.insert(comp_ore_tid, comp_out_tid);
        reg.type_lists.insert(comp_tl_id, TypeListInfo {
            type_list_id: comp_tl_id,
            name: "TestCompList".into(),
            included_category_ids: vec![],
            included_group_ids: vec![],
            included_type_ids: vec![comp_ore_tid],
            excluded_category_ids: vec![],
            excluded_group_ids: vec![],
            excluded_type_ids: vec![],
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.name = "SovCorp".into();
            c.ticker = "SOV".into();
            c.ceo_id = char_id;
            c
        });
        let mut comp_ch = snap.characters.get(&char_id).cloned().expect("char");
        comp_ch.character_id = comp_char_id;
        comp_ch.user_id = uid + 1;
        comp_ch.solar_system_id = null_sys_id;
        comp_ch.station_id = None;
        comp_ch.structure_id = None;
        comp_ch.active_ship_id = comp_ship_id;
        snap.characters.insert(comp_char_id, comp_ch);
        snap.items.insert(comp_ship_id, InventoryItemRecord { item_id: comp_ship_id, type_id: 587, owner_id: comp_char_id, location_id: null_sys_id as i64, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(comp_mod_id, InventoryItemRecord { item_id: comp_mod_id, type_id: comp_mod_tid, owner_id: comp_char_id, location_id: comp_ship_id, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
        snap.module_active_states.insert(comp_mod_id);
        snap.fleets.insert(92_101_050, FleetRecord { fleet_id: 92_101_050, boss_id: char_id, members: vec![char_id, comp_char_id], ..Default::default() });
        snap.items.insert(comp_ore_item_id, InventoryItemRecord { item_id: comp_ore_item_id, type_id: comp_ore_tid, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::SpecializedOreHold, quantity: 100, singleton: false, ..Default::default() });
        snap.items.insert(cargo_item_id, InventoryItemRecord { item_id: cargo_item_id, type_id: 34, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default() });
        snap.structures.insert(st_id, UpwellStructureRecord { structure_id: st_id, type_id: 35_832, owner_corp_id: corp_id, solar_system_id: null_sys_id, name: "Dropoff Astra".into(), position: [0.0, 0.0, 0.0], state: structure_life::STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 10, ..Default::default() });
    });

    let sky_ball_err = services::sov_gameplay::handle_anchor_skyhook_rpc(&ctx, &mut space_sess, &[PyRep::Integer(services::sov_services::TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(test_planet_id)])
        .await
        .expect_err("Skyhook anchor without spawned ship must fail");
    assert_ship_not_in_space("structureDeployment", "Anchor", Some(char_id), &sky_ball_err, ship_id, Some(char_id), Some(null_sys_id), "anchor_skyhook");

    let sky_comp_err = services::sov_gameplay::handle_skyhook_component_call(&ctx, &space_sess, null_sys_id, char_id, ship_id, 1_921_000_302, "orbitalSkyhook", "status", &[])
        .await
        .expect_err("unspawned skyhook component call must fail");
    assert_ship_not_in_space("beyonce", "CallComponentFromClient", Some(char_id), &sky_comp_err, ship_id, Some(char_id), Some(null_sys_id), "skyhook_component_call");

    let kw = HashMap::new();
    let drop_unspawned_err = invoke_rpc_entry(
        services::station_structure::structure_upwell_ops::RPCS,
        &ctx,
        &mut space_sess,
        "structureCargoDelivery",
        "DropOffItems",
        &[PyRep::List(vec![PyRep::Integer(cargo_item_id)]), PyRep::Integer(ship_id), PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .expect_err("cargo dropoff without spawned ship must fail");
    assert_ship_not_in_space("structureCargoDelivery", "DropOffItems", Some(char_id), &drop_unspawned_err, ship_id, Some(char_id), Some(null_sys_id), "structure_cargo_dropoff");

    let sys_handle = ctx.systems.get_or_spawn(null_sys_id);
    for (bid, tid) in [(ship_id, 587u32), (comp_ship_id, 587u32)] {
        let _ = sys_handle.spawn_ball(Ball { id: bid, type_id: tid, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1.0e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = ship_id + 99;
        }
    });
    let drop_changed_err = invoke_rpc_entry(
        services::station_structure::structure_upwell_ops::RPCS,
        &ctx,
        &mut space_sess,
        "structureCargoDelivery",
        "DropOffItems",
        &[PyRep::List(vec![PyRep::Integer(cargo_item_id)]), PyRep::Integer(ship_id), PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .expect_err("cargo dropoff with changed DB active ship must fail");
    assert_ship_not_in_space("structureCargoDelivery", "DropOffItems", Some(char_id), &drop_changed_err, ship_id, Some(char_id), Some(null_sys_id), "structure_cargo_dropoff");

    let comp_err = services::mining_advanced::mining_compression::handle_in_space_compression_mgr(&ctx, &mut space_sess, &[PyRep::Integer(comp_ore_item_id)])
        .await
        .expect_err("in-space compression with changed DB ship must fail");
    assert_ship_not_in_space("inSpaceCompressionMgr", "CompressItemInSpace", Some(char_id), &comp_err, ship_id, Some(char_id), Some(null_sys_id), "mining_compression");
}

#[test]
fn dogma_target_lock_cap_emits_targeting_failed_facts() {
    let skill_err =
        services::inventory_fitting::fitting_dogma_im::target_lock_cap_rpc_error(3, true);
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "AddTarget",
        None,
        &skill_err,
        CODE_TARGETING_FAILED,
        UserErrorKey::TargetingAptitudeAlreadyFullyUtilized,
    );
    assert!(args.iter().any(|(k, v)| k == "limit" && *v == PyRep::Integer(3)));
    assert_eq!(
        facts,
        &RejectionFacts::TargetingFailed {
            ship_id: None,
            target_id: None,
            reason: "max_locked_targets".into(),
            distance_m: None,
            max_range_m: None,
        }
    );

    let hull_err =
        services::inventory_fitting::fitting_dogma_im::target_lock_cap_rpc_error(5, false);
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "AddTarget",
        None,
        &hull_err,
        CODE_TARGETING_FAILED,
        UserErrorKey::TargetingSystemsAlreadyFullyUtilized,
    );
    assert!(args.iter().any(|(k, v)| k == "limit" && *v == PyRep::Integer(5)));
    assert_eq!(
        facts,
        &RejectionFacts::TargetingFailed {
            ship_id: None,
            target_id: None,
            reason: "max_locked_targets".into(),
            distance_m: None,
            max_range_m: None,
        }
    );

    let entity_err = services::inventory_fitting::fitting_dogma_im::target_lock_cap_rpc_error_with_ids(
        Some(893_101),
        Some(893_202),
        3,
        true,
    );
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "AddTarget",
        None,
        &entity_err,
        CODE_TARGETING_FAILED,
        UserErrorKey::TargetingAptitudeAlreadyFullyUtilized,
    );
    assert_eq!(
        facts,
        &RejectionFacts::TargetingFailed {
            ship_id: Some(893_101),
            target_id: Some(893_202),
            reason: "max_locked_targets".into(),
            distance_m: None,
            max_range_m: None,
        }
    );
}

#[tokio::test]
async fn abyssal_void_gates_invalid_target_emits_typed_facts() {
    let ctx = test_ctx();
    let uid = 92_201_u64;
    let char_id = 92_201_001_i64;
    let corp_id = 98_201_021_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_922_000_100_i64;
    let bad_gate_id = 1_922_000_200_i64;
    let missing_gate_id = 1_922_000_999_i64;
    let kw = HashMap::new();

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, None, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
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
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let origin_pos = DVec3::splat(500_000_000.0);
    for (id, tid, pos) in [
        (ship_id, 587_u32, origin_pos),
        (bad_gate_id, 587_u32, origin_pos + DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let mut sess = auth_session(uid, char_id, corp_id, sys_id);
    sess.ship_id = Some(ship_id);
    ctx.sync_session(&mut sess);

    let self_err = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("self-target VoidSpacePlayerJump must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        Some(char_id),
        &self_err,
        CODE_INVALID_TARGET,
        UserErrorKey::DeniedTargetSelf,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(ship_id),
            module_id: None,
            reason: "void_space_gate_self".into(),
        }
    );

    let missing_err = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(missing_gate_id)],
            &kw,
            None,
        )
        .await
        .expect_err("missing target VoidSpacePlayerJump must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        Some(char_id),
        &missing_err,
        CODE_INVALID_TARGET,
        UserErrorKey::InvalidTargetForModule,
    );
    assert!(args
        .iter()
        .any(|(k, v)| k == "moduleName" && *v == PyRep::String("Void Space Gate".into())));
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(missing_gate_id),
            module_id: None,
            reason: "not_filament_or_gate".into(),
        }
    );

    let bad_err = ctx
        .dispatch_rpc(
            &mut sess,
            "voidSpaceMgr",
            None,
            "VoidSpacePlayerJump",
            &[PyRep::Integer(bad_gate_id)],
            &kw,
            None,
        )
        .await
        .expect_err("non-gate ball VoidSpacePlayerJump must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        Some(char_id),
        &bad_err,
        CODE_INVALID_TARGET,
        UserErrorKey::InvalidTargetForModule,
    );
    assert!(args
        .iter()
        .any(|(k, v)| k == "moduleName" && *v == PyRep::String("Void Space Gate".into())));
    assert_eq!(
        facts,
        &RejectionFacts::InvalidTarget {
            target_id: Some(bad_gate_id),
            module_id: None,
            reason: "invalid_void_entrance_gate".into(),
        }
    );
}

#[tokio::test]
async fn ownership_and_contract_handlers_emit_typed_facts() {
    let mut ctx = test_ctx();
    let uid = 92_301_u64;
    let char_id = 92_301_001_i64;
    let foreign_cid = 92_301_002_i64;
    let corp_id = 98_201_031_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let other_station_id = 60_003_761_i64;
    let active_ship_id = 1_923_000_100_i64;
    let foreign_item_id = 1_923_000_201_i64;
    let remote_item_id = 1_923_000_202_i64;
    let foreign_ore_id = 1_923_000_203_i64;
    let open_auc_id = 923_001_i64;
    let finished_auc_id = 923_002_i64;
    let remote_corp_ship_id = 1_923_000_204_i64;
    let own_ore_id = 1_923_000_205_i64;
    let foreign_cont_id = 1_923_000_206_i64;
    let foreign_parent_cont_id = 1_923_000_207_i64;
    let child_cont_id = 1_923_000_208_i64;
    let cont_tid = 953_001_u32;
    let own_ore_tid = 953_002_u32;
    let kw = HashMap::new();

    upsert_test_type(&mut ctx.sde, 17_366, cont_tid, 448, 2, &[(eve_sde::attr_ids::CAPACITY, 1_000.0)]);
    upsert_test_type(&mut ctx.sde, 1_230, own_ore_tid, 462, 25, &[]);
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        if let Some(t) = reg.types.get_mut(&own_ore_tid) {
            t.portion_size = 100;
        }
        reg.type_materials.insert(own_ore_tid, vec![TypeMaterialEntry { material_type_id: 34, quantity: 100 }]);
    }

    if !services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 5) {
        let op_id = ctx
            .sde
            .stations
            .get(&station_id)
            .map(|s| s.operation_id)
            .unwrap_or(1);
        let mut op = ctx
            .sde
            .station_operations
            .get(&op_id)
            .cloned()
            .unwrap_or(StationOperationInfo {
                operation_id: op_id,
                name: "TestRefineryOp".into(),
                activity_id: None,
                services: Vec::new(),
            });
        if !op.services.contains(&5) {
            op.services.push(5);
        }
        Arc::make_mut(&mut ctx.sde).station_operations.insert(op_id, op);
    }

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, Some(station_id), active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            active_ship_id,
            InventoryItemRecord {
                item_id: active_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_item_id,
            InventoryItemRecord {
                item_id: foreign_item_id,
                type_id: 587,
                owner_id: foreign_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            remote_item_id,
            InventoryItemRecord {
                item_id: remote_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: other_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_ore_id,
            InventoryItemRecord {
                item_id: foreign_ore_id,
                type_id: 1_230,
                owner_id: foreign_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        for (iid, tid, own, loc, single, qty) in [
            (remote_corp_ship_id, 587, corp_id, other_station_id, true, 1),
            (own_ore_id, own_ore_tid, char_id, station_id, false, 500),
            (foreign_cont_id, cont_tid, foreign_cid, station_id, true, 1),
            (foreign_parent_cont_id, cont_tid, foreign_cid, station_id, true, 1),
            (child_cont_id, cont_tid, char_id, foreign_parent_cont_id, true, 1),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: own, location_id: loc,
                flag: InventoryFlag::Hangar, quantity: qty, singleton: single, ..Default::default()
            });
        }
        for (cid, status) in [(open_auc_id, "Outstanding"), (finished_auc_id, "Finished")] {
            snap.contracts.insert(
                cid,
                ContractRecord {
                    contract_id: cid,
                    issuer_id: foreign_cid,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    status: status.into(),
                    start_station_id: station_id,
                    end_station_id: station_id,
                    price: IskCents::from_f64_isk(1_000_000.0),
                    reward: IskCents(0),
                    collateral: IskCents(0),
                    item_ids: Vec::new(),
                    expiration_unix: 10,
                    issuer_corp_id: 0,
                    assignee_id: 0,
                    availability: "Public".into(),
                    buyout: IskCents(0),
                    current_bid: IskCents::from_f64_isk(500_000.0),
                    current_bidder_id: None,
                    requested_items: Vec::new(),
                    bid_history: Vec::new(),
                    date_issued_unix: 1,
                    title: "Test Auction".into(),
                    for_corp: false,
                    num_days: 1,
                },
            );
        }
    });

    let mut sess = auth_session(uid, char_id, corp_id, sys_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(active_ship_id);

    // 1. fitting_saved_multifit: repackagingSvc.RepackageItems ownership + location violations
    let rep_owner_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_saved_multifit::RPCS,
        &ctx,
        &mut sess,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(foreign_item_id)])],
        &kw,
    )
    .await
    .expect_err("foreign RepackageItems must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "repackagingSvc",
        "RepackageItems",
        Some(char_id),
        &rep_owner_err,
        CODE_OWNERSHIP_OR_LOCATION,
        UserErrorKey::CantDoThatWithSomeoneElsesStuff,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: foreign_item_id,
            owner_id: Some(foreign_cid),
            location_id: Some(station_id),
            required_owner_id: Some(char_id),
            required_location_id: Some(station_id),
        }
    );

    let rep_loc_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_saved_multifit::RPCS,
        &ctx,
        &mut sess,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(remote_item_id)])],
        &kw,
    )
    .await
    .expect_err("remote RepackageItems must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "repackagingSvc",
        "RepackageItems",
        Some(char_id),
        &rep_loc_err,
        CODE_OWNERSHIP_OR_LOCATION,
        UserErrorKey::CantDoThatWithSomeoneElsesStuff,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: remote_item_id,
            owner_id: Some(char_id),
            location_id: Some(other_station_id),
            required_owner_id: Some(char_id),
            required_location_id: Some(station_id),
        }
    );

    // 2. fitting_saved_multifit: ship.AssembleShip ownership + location violations
    let asm_owner_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_saved_multifit::RPCS,
        &ctx,
        &mut sess,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(foreign_item_id)],
        &kw,
    )
    .await
    .expect_err("foreign AssembleShip must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "ship",
        "AssembleShip",
        Some(char_id),
        &asm_owner_err,
        CODE_OWNERSHIP_OR_LOCATION,
        UserErrorKey::CantDoThatWithSomeoneElsesStuff,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: foreign_item_id,
            owner_id: Some(foreign_cid),
            location_id: Some(station_id),
            required_owner_id: Some(char_id),
            required_location_id: Some(station_id),
        }
    );

    let asm_loc_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_saved_multifit::RPCS,
        &ctx,
        &mut sess,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(remote_item_id)],
        &kw,
    )
    .await
    .expect_err("remote AssembleShip must fail");
    assert_ownership_violation("ship", "AssembleShip", Some(char_id), &asm_loc_err, remote_item_id, Some(char_id), Some(other_station_id), Some(char_id), Some(station_id));

    let asm_corp_loc_err = invoke_rpc_entry(
        services::inventory_fitting::fitting_saved_multifit::RPCS,
        &ctx,
        &mut sess,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(remote_corp_ship_id)],
        &kw,
    )
    .await
    .expect_err("remote corp AssembleShip must fail");
    assert_ownership_violation("ship", "AssembleShip", Some(char_id), &asm_corp_loc_err, remote_corp_ship_id, Some(corp_id), Some(other_station_id), Some(corp_id), Some(station_id));

    // 3. space_entity_mining: reprocessingSvc.Reprocess ownership & container hierarchy violations
    let ore_err = services::space_entity_mining::handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::Integer(foreign_ore_id)],
        None,
    )
    .await
    .expect("handled")
    .expect_err("foreign Reprocess must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "reprocessingSvc",
        "Reprocess",
        Some(char_id),
        &ore_err,
        CODE_OWNERSHIP_OR_LOCATION,
        UserErrorKey::CantDoThatWithSomeoneElsesStuff,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: foreign_ore_id,
            owner_id: Some(foreign_cid),
            location_id: Some(station_id),
            required_owner_id: Some(char_id),
            required_location_id: Some(station_id),
        }
    );

    let out_owner_err = services::space_entity_mining::handle(
        &ctx, &mut sess, "reprocessingSvc", "Reprocess",
        &[PyRep::Integer(own_ore_id), PyRep::Integer(station_id), PyRep::Integer(foreign_cid), PyRep::None, PyRep::None], None,
    ).await.expect("handled").expect_err("foreign output owner must fail");
    assert_ownership_violation("reprocessingSvc", "Reprocess", Some(char_id), &out_owner_err, own_ore_id, Some(foreign_cid), Some(station_id), Some(char_id), Some(station_id));

    let foreign_cont_err = services::space_entity_mining::handle(
        &ctx, &mut sess, "reprocessingSvc", "Reprocess",
        &[PyRep::Integer(own_ore_id), PyRep::Integer(station_id), PyRep::None, PyRep::Integer(foreign_cont_id), PyRep::Integer(InventoryFlag::Hangar as i64)], None,
    ).await.expect("handled").expect_err("foreign output container must fail");
    assert_ownership_violation("reprocessingSvc", "Reprocess", Some(char_id), &foreign_cont_err, foreign_cont_id, Some(foreign_cid), Some(station_id), Some(char_id), Some(station_id));

    let parent_cont_err = services::space_entity_mining::handle(
        &ctx, &mut sess, "reprocessingSvc", "Reprocess",
        &[PyRep::Integer(own_ore_id), PyRep::Integer(station_id), PyRep::None, PyRep::Integer(child_cont_id), PyRep::Integer(InventoryFlag::Hangar as i64)], None,
    ).await.expect("handled").expect_err("container hierarchy ownership mismatch must fail");
    assert_ownership_violation("reprocessingSvc", "Reprocess", Some(char_id), &parent_cont_err, foreign_parent_cont_id, Some(foreign_cid), Some(station_id), Some(char_id), Some(station_id));

    // 4. market_contracts: contractProxy.FinishAuction ownership + state violations
    let auc_owner_err = services::market_contracts::handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(open_auc_id)],
        &kw,
    )
    .expect("handled")
    .expect_err("non-owner FinishAuction must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "contractProxy",
        "FinishAuction",
        Some(char_id),
        &auc_owner_err,
        CODE_CONTRACT_OWNERSHIP,
        UserErrorKey::AccessDenied,
    );
    assert_eq!(
        facts,
        &RejectionFacts::OwnershipOrLocationViolation {
            item_id: open_auc_id,
            owner_id: Some(char_id),
            location_id: None,
            required_owner_id: Some(foreign_cid),
            required_location_id: None,
        }
    );

    let auc_state_err = services::market_contracts::handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(finished_auc_id)],
        &kw,
    )
    .expect("handled")
    .expect_err("finished FinishAuction must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "contractProxy",
        "FinishAuction",
        Some(char_id),
        &auc_state_err,
        CODE_INVALID_CONTRACT_STATE,
        UserErrorKey::ConContractNotOutstanding,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InvalidContractState {
            contract_id: finished_auc_id,
            expected_state: "Outstanding".into(),
            actual_state: "Finished".into(),
        }
    );
}

#[tokio::test]
async fn standup_weapons_and_filament_timers_warp_emit_typed_facts() {
    let mut ctx = test_ctx();
    let fixed_now = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(fixed_now)));

    if structure_profile(&ctx, TYPE_KEEPSTAR).is_none() {
        upsert_test_type(
            &mut ctx.sde,
            35_834,
            TYPE_KEEPSTAR,
            1_657,
            CATEGORY_STRUCTURE,
            &[(1547, 4.0)],
        );
    }
    let pdb_tid = 951_001_u32;
    let flak_tid = 951_002_u32;
    upsert_test_type(
        &mut ctx.sde,
        35_926,
        pdb_tid,
        GROUP_STANDUP_POINT_DEFENSE_BATTERY,
        66,
        &[
            (6, 5_000.0),
            (73, 12_000.0),
            (54, 15_000.0),
            (158, 10_000.0),
            (64, 10.0),
            (56, 1.0),
            (713, 1.0),
        ],
    );
    upsert_test_type(
        &mut ctx.sde,
        35_926,
        flak_tid,
        GROUP_STANDUP_FLAK_AMMO,
        8,
        &[(114, 100.0), (118, 100.0), (117, 100.0), (116, 100.0)],
    );

    let sys_id = ensure_nullsec_system(&mut ctx.sde);
    let char_id = 92_401_001_i64;
    let corp_id = 98_201_041_i64;
    let sid = 1_092_401_001_i64;
    let mod_id = 1_924_000_201_i64;
    let ammo_id = 1_924_000_202_i64;
    let ship_id = 1_924_000_301_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: pdb_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: flak_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, char_id);
        ext.lifecycle.entry(sid).or_default().online_modules.push(mod_id);
        ext.structure_cap_state.insert(sid, (100.0, fixed_now));
        save_ext_state(snap, &ext);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let st_ball = Ball {
        id: sid,
        type_id: TYPE_KEEPSTAR,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e12,
        radius: 10_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut st_cs = CombatState::new();
    st_cs.capacitor = 100.0;
    st_cs.max_capacitor = 100_000.0;
    let _ = sys_handle.spawn_ball_with_combat(st_ball.clone(), st_cs).await;

    let cap_err = try_activate_standup_weapon(
        &ctx,
        &sys_handle,
        char_id,
        sid,
        sys_id,
        mod_id,
        None,
        &st_ball,
    )
    .await
    .expect("handled")
    .expect_err("insufficient structure capacitor must fail");
    let (facts, args) = assert_rejected_wire_facts(
        "dogmaIM",
        "Activate",
        Some(char_id),
        &cap_err,
        CODE_NOT_ENOUGH_CAPACITOR,
        UserErrorKey::NotEnoughEnergy,
    );
    assert!(args
        .iter()
        .any(|(k, v)| k == "effectname" && *v == PyRep::String("TestType_951001".into())));
    assert!(args.iter().any(|(k, v)| k == "need" && *v == PyRep::Real(5_000.0)));
    assert!(args.iter().any(|(k, v)| k == "got" && *v == PyRep::Real(100.0)));
    assert_eq!(
        facts,
        &RejectionFacts::NotEnoughCapacitor {
            entity_id: Some(sid),
            effect_name: "TestType_951001".into(),
            required_gj: 5_000.0,
            available_gj: 100.0,
        }
    );

    // sp6_lifecycle: WeaponsTimerActive
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.trigger_weapon_timer(char_id, fixed_now);
    }
    let weap_err =
        crate::sp6_lifecycle::require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .expect_err("active weapons timer must block filament jump");
    let (facts, _) = assert_rejected_wire_facts(
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        Some(char_id),
        &weap_err,
        CODE_WEAPONS_TIMER_ACTIVE,
        UserErrorKey::CantJumpAfterAggression,
    );
    assert_eq!(
        facts,
        &RejectionFacts::WeaponsTimerActive {
            char_id,
            action: "filament_activate".into(),
            timer_kind: "weapons".into(),
        }
    );

    // sp6_lifecycle: ShipInWarp preserving exact WarpPhase label
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).weapon_timer_until_unix = 0;
    }
    for (phase, expected_label) in [
        (WarpPhase::Aligning, "Aligning"),
        (WarpPhase::Cruising, "Cruising"),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
                    warp_speed_au_s: 3.0,
                    total_distance: 1_000_000_000.0,
                    phase,
                    elapsed_warp_time: 1.0,
                },
            })
            .await;
        let warp_err = crate::sp6_lifecycle::require_ship_ready_for_filament_jump(
            &ctx, char_id, ship_id, sys_id,
        )
        .expect_err("warp phase must block filament jump");
        let (facts, _) = assert_rejected_wire_facts(
            "voidSpaceMgr",
            "VoidSpacePlayerJump",
            Some(char_id),
            &warp_err,
            CODE_SHIP_IN_WARP,
            UserErrorKey::ShipInWarp,
        );
        assert_eq!(
            facts,
            &RejectionFacts::ShipInWarp {
                ship_id: Some(ship_id),
                action: "filament_activate".into(),
                warp_phase: Some(expected_label.into()),
            }
        );
        let _ = sys_handle.remove_ball(ship_id).await;
    }
}

#[tokio::test]
async fn role_gate_handlers_preserve_legacy_codes_and_masks() {
    let ctx = test_ctx();
    let uid = 92_501_u64;
    let char_id = 92_501_001_i64;
    let corp_id = 98_201_051_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, None, 1_925_000_100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            char_id,
            NpeProgressRecord {
                character_id: char_id,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
    });

    let mut session = auth_session(uid, char_id, corp_id, sys_id);
    session.role = service_roles::ROLE_PLAYER_DEFAULT;
    ctx.sync_session(&mut session);

    // 1. fitting_scanners_superweapons: GMRoleRequired
    for method in [
        "ResetSystemWideEffects",
        "RefreshSystemWideEnvironment",
        "ApplyBuffsToItems",
    ] {
        let gm_err = invoke_rpc_entry(
            services::inventory_fitting::fitting_scanners_superweapons::RPCS,
            &ctx,
            &mut session,
            "dogmaIM",
            method,
            &[],
            &kw,
        )
        .await
        .expect_err("non-GM dogmaIM GM method must fail");
        let (facts, _) = assert_rejected_wire_facts(
            "dogmaIM",
            method,
            Some(char_id),
            &gm_err,
            CODE_GM_ROLE_REQUIRED,
            UserErrorKey::InsufficientRoles,
        );
        assert_eq!(
            facts,
            &RejectionFacts::InsufficientRole {
                required_role_mask: service_roles::ROLE_GM_MASK,
                actual_role_mask: service_roles::ROLE_PLAYER_DEFAULT,
                action: method.into(),
            }
        );
    }

    // 2. social_corp_fleet: GMRoleRequired
    let ally_err = services::social_corp_fleet::handle(
        &ctx,
        &mut session,
        "allianceRegistry",
        "GMForceAcceptApplication",
        &[PyRep::Integer(corp_id)],
        &kw,
        None,
    )
    .await
    .expect("handled")
    .expect_err("non-GM GMForceAcceptApplication must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "allianceRegistry",
        "GMForceAcceptApplication",
        Some(char_id),
        &ally_err,
        CODE_GM_ROLE_REQUIRED,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "GMForceAcceptApplication".into(),
        }
    );

    // 3. sov_services: GMRoleRequired
    let clone_err = services::sov_services::handle_reset_last_clone_jump_time(&ctx, &session)
        .expect_err("non-GM ResetLastCloneJumpTime must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "jumpCloneSvc",
        "ResetLastCloneJumpTime",
        Some(char_id),
        &clone_err,
        CODE_GM_ROLE_REQUIRED,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "jumpCloneSvc.ResetLastCloneJumpTime".into(),
        }
    );

    // 4. space_destiny: InsufficientRoles
    let bubble_err = services::space_destiny::handle(
        &ctx,
        &mut session,
        None,
        "beyonce",
        "AdminGetBubbleID",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-GM AdminGetBubbleID must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "beyonce",
        "AdminGetBubbleID",
        Some(char_id),
        &bubble_err,
        CODE_INSUFFICIENT_ROLES,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "AdminGetBubbleID".into(),
        }
    );

    // 5. serverops_cluster + character_expert_systems + cosmetics_ui_ops + cosmetics_events: InsufficientRole
    let cluster_err = services::serverops_cluster::require_gm_or_admin(&session)
        .expect_err("non-GM require_gm_or_admin must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "cluster",
        "AddClusterNode",
        Some(char_id),
        &cluster_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: ServerContext::ROLE_GM_CANONICAL_MASK,
            actual_role_mask: session.role,
            action: "cluster_admin".into(),
        }
    );

    let npe_err = services::character_expert_systems::handle(
        &ctx,
        &mut session,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(473),
            PyRep::Integer(0),
            PyRep::String("complete_air_npe".into()),
        ],
        &kw,
    )
    .expect("handled")
    .expect_err("non-GM AIR NPE control message must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "node_graph",
        "process_client_graph_message",
        Some(char_id),
        &npe_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "air_npe_control_graph".into(),
        }
    );

    for (method, expected_action) in [
        ("RemoveSkin", "shipCosmeticsMgr.RemoveSkin"),
        ("GMExpireSkinLicense", "shipCosmeticsMgr.GMExpireSkinLicense"),
    ] {
        let skin_err = services::cosmetics_ui_ops::handle(
            &ctx,
            &mut session,
            "shipCosmeticsMgr",
            method,
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("non-GM skin admin method must fail");
        let (facts, _) = assert_rejected_wire_facts(
            "shipCosmeticsMgr",
            method,
            Some(char_id),
            &skin_err,
            CODE_INSUFFICIENT_ROLE,
            UserErrorKey::InsufficientRoles,
        );
        assert_eq!(
            facts,
            &RejectionFacts::InsufficientRole {
                required_role_mask: service_roles::ROLE_GM_MASK,
                actual_role_mask: session.role,
                action: expected_action.into(),
            }
        );
    }

    let give_err = services::cosmetics_events::handle(
        &ctx,
        &mut session,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-GM GiveSkin must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "shipCosmeticsMgr",
        "GiveSkin",
        Some(char_id),
        &give_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "shipCosmeticsMgr.GiveSkin".into(),
        }
    );

    let lod2_err = services::cosmetics_events::materialize_system_lod2(&ctx, &session, &[])
        .await
        .expect_err("non-GM materialize_system_lod2 must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "shipCosmeticsMgr",
        "GiveSkin",
        Some(char_id),
        &lod2_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "materialize_system_lod2".into(),
        }
    );

    let convoy_role_err = services::cosmetics_events::interdict_convoy(&ctx, &session, &[])
        .await
        .expect_err("non-GM interdict_convoy must fail");
    let (facts, _) = assert_rejected_wire_facts(
        "shipCosmeticsMgr",
        "GiveSkin",
        Some(char_id),
        &convoy_role_err,
        CODE_INSUFFICIENT_ROLE,
        UserErrorKey::InsufficientRoles,
    );
    assert_eq!(
        facts,
        &RejectionFacts::InsufficientRole {
            required_role_mask: service_roles::ROLE_GM_MASK,
            actual_role_mask: session.role,
            action: "interdict_convoy".into(),
        }
    );

    ctx.living_world.seed_default_convoys();
    let ship_id = 1_925_000_100_i64;
    let mut gm_sess = auth_session(uid, char_id, corp_id, sys_id);
    gm_sess.role = service_roles::ROLE_GM_MASK;
    gm_sess.ship_id = Some(ship_id);
    ctx.sync_session(&mut gm_sess);
    let convoy_ship_err = services::cosmetics_events::interdict_convoy(
        &ctx,
        &gm_sess,
        &[PyRep::Integer(800_000_001)],
    )
    .await
    .expect_err("GM interdict_convoy without spawned ship must fail");
    assert_ship_not_in_space(
        "ship",
        "LaunchDrones",
        Some(char_id),
        &convoy_ship_err,
        ship_id,
        Some(char_id),
        Some(sys_id),
        "interdict_convoy",
    );
}

#[test]
fn module_not_found_handlers_emit_typed_facts() {
    let mut ctx = test_ctx();
    let uid = 92_601_u64;
    let char_id = 92_601_001_i64;
    let corp_id = 98_201_061_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = 1_926_000_100_i64;
    let charge_id = 1_926_000_200_i64;
    let missing_mod_id = 1_926_000_999_i64;
    let struct_id = 1_092_600_300_i64;

    if structure_profile(&ctx, 35_832).is_none() {
        upsert_test_type(
            &mut ctx.sde,
            35_832,
            35_832,
            1_657,
            CATEGORY_STRUCTURE,
            &[
                (structure_life::ATTR_RIG_SIZE, 2.0),
                (structure_life::ATTR_SHIELD_CAPACITY, 500_000.0),
                (structure_life::ATTR_ARMOR_HP, 500_000.0),
                (structure_life::ATTR_HP, 500_000.0),
            ],
        );
    }
    let st_prof = structure_profile(&ctx, 35_832).expect("Astrahus profile");

    seed_char_in_ctx(&ctx, uid, char_id, corp_id, sys_id, Some(station_id), ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.name = "Module Guard Corp".into();
            corp.ticker = "MGC".into();
            corp.ceo_id = char_id;
            corp
        });
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 192,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                state: structure_life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: st_prof.shield_hp,
                armor_hp: st_prof.armor_hp,
                hull_hp: st_prof.hull_hp,
                ..Default::default()
            },
        );
    });

    // 1. fitting_cycles_overheat::check_can_overload_module
    let overload_err =
        services::fitting_cycles_overheat::check_can_overload_module(&ctx, char_id, 999_999_888)
            .expect_err("unknown module type must fail with ModuleNotFound");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "Overload",
        Some(char_id),
        &overload_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: 0,
            owner_entity_id: Some(char_id),
            type_id: Some(999_999_888),
        }
    );

    // 2. notify_emitters_seasons_tactical::handle_load_ammo
    let mut sess = auth_session(uid, char_id, corp_id, sys_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    let ammo_err = services::notify_emitters_seasons_tactical::handle_load_ammo(
        &ctx,
        &sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(missing_mod_id)]),
            PyRep::List(vec![PyRep::Integer(charge_id)]),
        ],
    )
    .expect_err("LoadAmmo on missing module must fail with ModuleNotFound");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "LoadAmmo",
        Some(char_id),
        &ammo_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ModuleNoLongerPresentForCharges,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: missing_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: None,
        }
    );

    let ammo_recheck_err = ctx
        .db
        .with_snapshot(|snap| {
            services::notify_emitters_seasons_tactical::validate_load_ammo_module_in_snap(
                snap,
                missing_mod_id,
                char_id,
                ship_id,
            )
        })
        .expect_err("snapshot re-check on missing module must fail with ModuleNotFound");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "LoadAmmo",
        Some(char_id),
        &ammo_recheck_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ModuleNoLongerPresentForCharges,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: missing_mod_id,
            owner_entity_id: Some(ship_id),
            type_id: None,
        }
    );

    // 3. structure_lifecycle_rpc::handle_structure_module_online
    let mut struct_sess = auth_session(uid, char_id, corp_id, sys_id);
    struct_sess.structure_id = Some(struct_id);
    let st_mod_err = services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
        &ctx,
        &mut struct_sess,
        struct_id,
        missing_mod_id,
        true,
    )
    .expect_err("structure module online on missing module must fail with ModuleNotFound");
    let (facts, _) = assert_rejected_wire_facts(
        "dogmaIM",
        "SetModuleOnline",
        Some(char_id),
        &st_mod_err,
        CODE_MODULE_NOT_FOUND,
        UserErrorKey::ItemNotFound,
    );
    assert_eq!(
        facts,
        &RejectionFacts::ModuleNotFound {
            module_id: missing_mod_id,
            owner_entity_id: Some(struct_id),
            type_id: None,
        }
    );
}
