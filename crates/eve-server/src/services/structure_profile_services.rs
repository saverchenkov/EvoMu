//! `structure_profile_services` — Per-service ACL group bindings, per-ACL-group tax resolution,
//! and V24 `structures/services.pyj` & `structures/settings.pyj` enforcement (`feat:structures.profiles`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::ServerContext;
use crate::services::station_structure::{
    structure_lifecycle as life,
    structure_upwell_ops::{
        self as ops, err, load_ext_state, save_ext_state, validate_and_classify_entity,
        StructureAccessProfile, StructureExtendedState,
    },
};

// V24 `structures/settings.pyj` & `ownergroups/ownergroupConst.pyj` IDs:
pub const NO_GROUP_ID: i64 = 0;
pub const CATEGORY_HULL_DEFENSE: i64 = 1003;
pub const SETTING_REPROCESSING_TAX: i64 = 3;
pub const SETTING_MARKET_TAX: i64 = 4;
pub const SETTING_DEFENSE_CAN_CONTROL_STRUCTURE: i64 = 17;
pub const SETTING_HOUSING_CAN_DOCK: i64 = 19;
pub const SETTING_CORP_RENT_OFFICE: i64 = 20;
pub const SETTING_CLONINGBAY_TAX: i64 = 23;
pub const SETTING_INDUSTRY_TAX: i64 = 24;
pub const SETTING_REACTION_BIOCHEMICAL_TAX: i64 = 26;
pub const SETTING_REACTION_HYBRID_TAX: i64 = 27;
pub const SETTING_REACTION_COMPOSITE_TAX: i64 = 28;
pub const SETTING_MANUFACTURING_TAX: i64 = 29;
pub const SETTING_MANUFACTURING_CAPITAL_TAX: i64 = 30;
pub const SETTING_MANUFACTURING_SUPERCAPITAL_TAX: i64 = 31;
pub const SETTING_RESEARCH_TAX: i64 = 32;
pub const SETTING_INVENTION_TAX: i64 = 33;
pub const SETTING_JUMP_BRIDGE_ACTIVATION: i64 = 34;
pub const SETTING_CYNO_BEACON: i64 = 35;
pub const SETTING_AUTOMOONMINING: i64 = 36;

// V24 `structures/services.pyj` service IDs:
pub const SERVICE_DOCKING: i64 = 1;
pub const SERVICE_FITTING: i64 = 2;
pub const SERVICE_OFFICES: i64 = 3;
pub const SERVICE_REPROCESSING: i64 = 4;
pub const SERVICE_MARKET: i64 = 5;
pub const SERVICE_MEDICAL: i64 = 6;
pub const SERVICE_REPAIR: i64 = 8;
pub const SERVICE_JUMP_CLONE: i64 = 10;
pub const SERVICE_MOONMINING: i64 = 14;
pub const SERVICE_JUMP_BRIDGE: i64 = 15;
pub const SERVICE_CYNO_BEACON: i64 = 16;
pub const SERVICE_AUTOMOONMINING: i64 = 18;
pub const SERVICE_INDUSTRY: i64 = 20;
pub const SERVICE_MANUFACTURING: i64 = 21;
pub const SERVICE_MANUFACTURING_BASIC: i64 = 22;
pub const SERVICE_MANUFACTURING_CAPITAL: i64 = 23;
pub const SERVICE_MANUFACTURING_SUPERCAPITAL: i64 = 24;
pub const SERVICE_LABORATORY: i64 = 30;
pub const SERVICE_LABORATORY_RESEARCH_TIME: i64 = 31;
pub const SERVICE_LABORATORY_RESEARCH_MATERIAL: i64 = 32;
pub const SERVICE_LABORATORY_COPYING: i64 = 33;
pub const SERVICE_LABORATORY_INVENTION: i64 = 34;
pub const SERVICE_REACTIONS: i64 = 40;
pub const SERVICE_REACTIONS_COMPOSITE: i64 = 41;
pub const SERVICE_REACTIONS_BIOCHEMICAL: i64 = 42;
pub const SERVICE_REACTIONS_HYBRID: i64 = 43;

/// Resolves the active `StructureAccessProfile` for `structure_id`, honoring due delayed
/// `ChangeProfiles` transitions (`pending_profile_applications`) at `now_unix_opt`.
pub fn effective_profile_for_structure<'a>(
    ext: &'a StructureExtendedState,
    structure_id: i64,
    now_unix_opt: Option<u64>,
) -> Option<&'a StructureAccessProfile> {
    now_unix_opt
        .and_then(|now| {
            ext.pending_profile_applications
                .get(&structure_id)
                .filter(|&&(_, apply_at)| now >= apply_at)
                .and_then(|&(pid, _)| ext.profiles_by_id.get(&pid))
        })
        .or_else(|| ext.access_profiles.get(&structure_id))
}

/// Validates and encodes a profile service setting rate/fee into `1/10_000` basis units.
/// Percentage tax settings require a finite rate in `0.0..=1.0`; flat-ISK settings (`20`, `23`, `34`, `35894`)
/// accept finite non-negative ISK values up to `1_000_000_000_000.0`.
pub fn encode_service_setting_bps(setting_id: i64, raw: f64) -> Result<i64, BridgeRpcError> {
    let allows_flat_isk = matches!(
        setting_id,
        SETTING_CORP_RENT_OFFICE | SETTING_CLONINGBAY_TAX | SETTING_JUMP_BRIDGE_ACTIVATION | 35_894 | 15
    );
    let max_allowed = if allows_flat_isk { 1_000_000_000_000.0 } else { 1.0 };
    if !raw.is_finite() || raw < 0.0 || raw > max_allowed {
        return Err(err(
            "InvalidStructureTaxRate",
            "Structure service tax rate must be a finite number in valid range",
        ));
    }
    Ok((raw * 10_000.0).round() as i64)
}

/// Validates all ACL group IDs referenced by `service_acl_updates` against `ext.owner_groups`
/// and corporation ownership (`owner_corp`).
pub fn validate_service_acl_groups(
    ext: &StructureExtendedState,
    session: &SessionState,
    char_id: i64,
    owner_corp: i64,
    service_acl_updates: &HashMap<i64, Vec<(i64, f64)>>,
) -> Result<HashMap<i64, Vec<(i64, i64)>>, BridgeRpcError> {
    let mut encoded: HashMap<i64, Vec<(i64, i64)>> = HashMap::with_capacity(service_acl_updates.len());
    for (&sid, rows) in service_acl_updates {
        let mut out_rows = Vec::with_capacity(rows.len());
        for &(gid, raw_val) in rows {
            if gid < 0 {
                return Err(err("AccessGroupNotFound", format!("Invalid negative access group {gid}")));
            }
            if gid > 0 {
                let grp = ext
                    .owner_groups
                    .get(&gid)
                    .ok_or_else(|| err("AccessGroupNotFound", format!("Access group {gid} not found")))?;
                if grp.owner_corp_id != 0 && owner_corp != 0 && grp.owner_corp_id != owner_corp {
                    return Err(err(
                        "StructureOwnershipViolation",
                        "Cannot attach an access group owned by another corporation",
                    ));
                }
                if grp.owner_corp_id == 0 && grp.creator_id != char_id && !session.has_gm_role() {
                    return Err(err(
                        "StructureOwnershipViolation",
                        "Cannot attach an access group owned by another character",
                    ));
                }
            }
            let bps = encode_service_setting_bps(sid, raw_val)?;
            out_rows.push((gid, bps));
        }
        encoded.insert(sid, out_rows);
    }
    Ok(encoded)
}

/// Applies validated `encoded_service_acls` to `prof`, keeping legacy `service_settings` synced with
/// the minimum configured rate for flat-fallback callers while never mutating Docking ACLs.
pub fn apply_service_acls_to_profile(
    prof: &mut StructureAccessProfile,
    encoded_service_acls: HashMap<i64, Vec<(i64, i64)>>,
) {
    for (sid, rows) in encoded_service_acls {
        if let Some(min_bps) = rows.iter().map(|&(_, bps)| bps).min() {
            prof.service_settings.insert(sid, min_bps);
        }
        prof.service_acl_rules.insert(sid, rows);
    }
}

/// Evaluates `(char_id, char_corp, char_ally)` against a single `rules: &[(group_id, value_bps)]` slice.
/// Returns `None` if blocked or no ACL group matches; returns `Some(min_value_bps)` if allowed.
fn evaluate_rule_slice(
    ext: &StructureExtendedState,
    rules: &[(i64, i64)],
    char_id: i64,
    char_corp: i64,
    char_ally: i64,
) -> Option<i64> {
    if rules.is_empty() {
        return None;
    }
    for &(gid, _) in rules {
        if gid > 0 {
            if let Some(grp) = ext.owner_groups.get(&gid) {
                if grp.members.get(&char_id) == Some(&-1)
                    || (char_corp > 0 && grp.members.get(&char_corp) == Some(&-1))
                    || (char_ally > 0 && grp.members.get(&char_ally) == Some(&-1))
                {
                    return None;
                }
            }
        }
    }
    let mut best_bps: Option<i64> = None;
    for &(gid, bps) in rules {
        let matched = if gid == 0 {
            true
        } else if let Some(grp) = ext.owner_groups.get(&gid) {
            grp.members.get(&0).is_some_and(|&r| r >= 0)
                || grp.members.get(&char_id).is_some_and(|&r| r >= 0)
                || (char_corp > 0 && grp.members.get(&char_corp).is_some_and(|&r| r >= 0))
                || (char_ally > 0 && grp.members.get(&char_ally).is_some_and(|&r| r >= 0))
        } else {
            false
        };
        if matched {
            best_bps = Some(best_bps.map_or(bps, |cur| cur.min(bps)));
        }
    }
    best_bps
}

fn pilot_corp_and_ally(snap: &WorldStateSnapshot, char_id: i64) -> (i64, i64) {
    snap.characters
        .get(&char_id)
        .map(|c| {
            let corp = c.corporation_id;
            let ally = c
                .alliance_id
                .or_else(|| snap.corporations.get(&corp).and_then(|cr| cr.alliance_id))
                .unwrap_or(0);
            (corp, ally)
        })
        .unwrap_or((0, 0))
}

/// Evaluates pilot access + minimum ACL-group tax/fee across `candidate_setting_ids` on `prof`.
/// - Returns `Some(Ok(Some(bps)))` when at least one candidate setting has `service_acl_rules` configured and grants access.
/// - Returns `Some(Err(()))` when `service_acl_rules` is configured for this service domain and denies `char_id`.
/// - Returns `None` when NO per-service ACL rules were configured for this service domain (caller falls back to legacy flat `service_settings`).
pub fn evaluate_domain_acl_on_profile(
    snap: &WorldStateSnapshot,
    ext: &StructureExtendedState,
    prof: &StructureAccessProfile,
    char_id: i64,
    candidate_setting_ids: &[i64],
    any_of_candidates: bool,
) -> Option<Result<i64, ()>> {
    let (char_corp, char_ally) = pilot_corp_and_ally(snap, char_id);
    if any_of_candidates {
        let mut saw_configured = false;
        let mut best: Option<i64> = None;
        for &cid in candidate_setting_ids {
            if let Some(rules) = prof.service_acl_rules.get(&cid) {
                saw_configured = true;
                if let Some(bps) = evaluate_rule_slice(ext, rules, char_id, char_corp, char_ally) {
                    best = Some(best.map_or(bps, |cur| cur.min(bps)));
                }
            }
        }
        if !saw_configured {
            return None;
        }
        return Some(best.ok_or(()));
    }
    for &cid in candidate_setting_ids {
        if let Some(rules) = prof.service_acl_rules.get(&cid) {
            return Some(evaluate_rule_slice(ext, rules, char_id, char_corp, char_ally).ok_or(()));
        }
    }
    None
}

/// Checks whether `service_id` (supporting both V24 `structures/services.pyj` constants and EvoMu
/// module/legacy IDs) is online at `structure_id`.
pub fn is_v24_or_legacy_service_online(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    service_id: i64,
) -> bool {
    let Some(st) = snap.structures.get(&structure_id) else {
        return false;
    };
    if !life::is_operational(&st.state) {
        return false;
    }
    if ops::is_structure_service_online(ctx, snap, structure_id, service_id) {
        return true;
    }
    let online = life::online_service_type_ids(ctx, snap, structure_id);
    match service_id {
        SERVICE_MARKET => online.contains(&35_892),
        SERVICE_REPROCESSING => online.contains(&35_899),
        SERVICE_MEDICAL | SERVICE_JUMP_CLONE => online.contains(&35_894),
        SERVICE_MOONMINING => online.contains(&45_009),
        SERVICE_JUMP_BRIDGE => {
            (st.type_id == 35_841 && st.quantum_core_installed && st.fuel_blocks > 0)
                || online.contains(&35_913)
        }
        SERVICE_CYNO_BEACON => st.type_id == 35_840 && st.quantum_core_installed && st.fuel_blocks > 0,
        SERVICE_AUTOMOONMINING => st.type_id == 81_826 && st.quantum_core_installed && st.fuel_blocks > 0,
        SERVICE_REPAIR | 13 => st.quantum_core_installed && !matches!(st.type_id, 35_840 | 35_841 | 37_534 | 81_826),
        SERVICE_INDUSTRY => online.iter().any(|&t| matches!(t, 35_878 | 35_891 | 35_886 | 45_537..=45_539)),
        SERVICE_MANUFACTURING | SERVICE_MANUFACTURING_BASIC | SERVICE_MANUFACTURING_CAPITAL | SERVICE_MANUFACTURING_SUPERCAPITAL => {
            online.contains(&35_878)
        }
        SERVICE_LABORATORY => online.iter().any(|&t| matches!(t, 35_891 | 35_886)),
        SERVICE_LABORATORY_RESEARCH_TIME | SERVICE_LABORATORY_RESEARCH_MATERIAL | SERVICE_LABORATORY_COPYING => {
            online.contains(&35_891)
        }
        SERVICE_LABORATORY_INVENTION => online.contains(&35_886),
        SERVICE_REACTIONS | SERVICE_REACTIONS_COMPOSITE | SERVICE_REACTIONS_BIOCHEMICAL | SERVICE_REACTIONS_HYBRID => {
            online.iter().any(|&t| (45_537..=45_539).contains(&t))
        }
        _ => false,
    }
}

fn candidate_settings_for_service_query(
    _ctx: &ServerContext,
    _snap: &WorldStateSnapshot,
    _prof: &StructureAccessProfile,
    _structure_id: i64,
    service_id: i64,
) -> (Vec<i64>, bool, &'static [i64]) {
    match service_id {
        SERVICE_DOCKING | SERVICE_FITTING => (vec![SETTING_HOUSING_CAN_DOCK], false, &[SETTING_HOUSING_CAN_DOCK]),
        SERVICE_OFFICES => (vec![SETTING_CORP_RENT_OFFICE], false, &[SETTING_CORP_RENT_OFFICE]),
        SERVICE_REPROCESSING => (vec![SETTING_REPROCESSING_TAX], false, &[SETTING_REPROCESSING_TAX]),
        SERVICE_MARKET => (vec![SETTING_MARKET_TAX], false, &[SETTING_MARKET_TAX]),
        SERVICE_MEDICAL | SERVICE_JUMP_CLONE => (vec![SETTING_CLONINGBAY_TAX], false, &[SETTING_CLONINGBAY_TAX]),
        SERVICE_REPAIR => (vec![SERVICE_REPAIR], false, &[SERVICE_REPAIR]),
        SERVICE_MOONMINING => (vec![SERVICE_MOONMINING], false, &[SERVICE_MOONMINING]),
        SERVICE_JUMP_BRIDGE => (vec![SETTING_JUMP_BRIDGE_ACTIVATION], false, &[SETTING_JUMP_BRIDGE_ACTIVATION]),
        SERVICE_CYNO_BEACON => (vec![SETTING_CYNO_BEACON], false, &[SETTING_CYNO_BEACON]),
        SERVICE_AUTOMOONMINING => (vec![SETTING_AUTOMOONMINING], false, &[SETTING_AUTOMOONMINING]),
        SERVICE_INDUSTRY => (
            vec![
                SETTING_MANUFACTURING_TAX,
                SETTING_MANUFACTURING_CAPITAL_TAX,
                SETTING_MANUFACTURING_SUPERCAPITAL_TAX,
                SETTING_RESEARCH_TAX,
                SETTING_INVENTION_TAX,
                SETTING_REACTION_COMPOSITE_TAX,
                SETTING_REACTION_BIOCHEMICAL_TAX,
                SETTING_REACTION_HYBRID_TAX,
                SETTING_INDUSTRY_TAX,
            ],
            true,
            &[SETTING_INDUSTRY_TAX, SETTING_MANUFACTURING_TAX],
        ),
        SERVICE_MANUFACTURING => (
            vec![
                SETTING_MANUFACTURING_TAX,
                SETTING_MANUFACTURING_CAPITAL_TAX,
                SETTING_MANUFACTURING_SUPERCAPITAL_TAX,
                SETTING_INDUSTRY_TAX,
            ],
            true,
            &[SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_MANUFACTURING_BASIC => (
            vec![SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_MANUFACTURING_CAPITAL => (
            vec![SETTING_MANUFACTURING_CAPITAL_TAX, SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_MANUFACTURING_CAPITAL_TAX, SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_MANUFACTURING_SUPERCAPITAL => (
            vec![SETTING_MANUFACTURING_SUPERCAPITAL_TAX, SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_MANUFACTURING_SUPERCAPITAL_TAX, SETTING_MANUFACTURING_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_LABORATORY => (
            vec![SETTING_RESEARCH_TAX, SETTING_INVENTION_TAX, SETTING_INDUSTRY_TAX],
            true,
            &[SETTING_RESEARCH_TAX, SETTING_INVENTION_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_LABORATORY_RESEARCH_TIME | SERVICE_LABORATORY_RESEARCH_MATERIAL | SERVICE_LABORATORY_COPYING => {
            (vec![SETTING_RESEARCH_TAX, SETTING_INDUSTRY_TAX], false, &[SETTING_RESEARCH_TAX, SETTING_INDUSTRY_TAX])
        }
        SERVICE_LABORATORY_INVENTION => (
            vec![SETTING_INVENTION_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_INVENTION_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_REACTIONS => (
            vec![
                SETTING_REACTION_COMPOSITE_TAX,
                SETTING_REACTION_BIOCHEMICAL_TAX,
                SETTING_REACTION_HYBRID_TAX,
                SETTING_INDUSTRY_TAX,
            ],
            true,
            &[
                SETTING_REACTION_COMPOSITE_TAX,
                SETTING_REACTION_BIOCHEMICAL_TAX,
                SETTING_REACTION_HYBRID_TAX,
                SETTING_INDUSTRY_TAX,
            ],
        ),
        SERVICE_REACTIONS_COMPOSITE => (
            vec![SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_REACTIONS_BIOCHEMICAL => (
            vec![SETTING_REACTION_BIOCHEMICAL_TAX, SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_REACTION_BIOCHEMICAL_TAX, SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
        ),
        SERVICE_REACTIONS_HYBRID => (
            vec![SETTING_REACTION_HYBRID_TAX, SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
            false,
            &[SETTING_REACTION_HYBRID_TAX, SETTING_REACTION_COMPOSITE_TAX, SETTING_INDUSTRY_TAX],
        ),
        other => (vec![other], false, &[]),
    }
}

/// Evaluates whether `char_id` can use `service_id` on `structure_id` at `now_unix` and returns
/// `Some(bps)` (basis points / encoded fee) if allowed, or `None` if offline or denied by Docking or Service ACL.
pub fn evaluate_character_service_bps(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    service_id: i64,
    default_bps: i64,
) -> Option<i64> {
    let now = ctx.now_unix();
    let st = snap.structures.get(&structure_id)?;
    if !life::is_operational(&st.state) {
        return None;
    }
    let is_external_flex = matches!(service_id, SERVICE_JUMP_BRIDGE | SERVICE_CYNO_BEACON | 9 | 35_913)
        && matches!(st.type_id, 35_840 | 35_841);
    if !is_external_flex
        && !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now))
    {
        return None;
    }
    if !is_v24_or_legacy_service_online(ctx, snap, structure_id, service_id) {
        return None;
    }
    let ext = load_ext_state(snap);
    if let Some(prof) = effective_profile_for_structure(&ext, structure_id, Some(now)) {
        if prof.general_settings.get(&service_id) == Some(&false) {
            return None;
        }
        let (cands, any_of, flat_keys) =
            candidate_settings_for_service_query(ctx, snap, prof, structure_id, service_id);
        match evaluate_domain_acl_on_profile(snap, &ext, prof, char_id, &cands, any_of) {
            Some(Ok(bps)) => return Some(bps),
            Some(Err(())) => return None,
            None => {}
        }
        if let Some(&bps) = prof.service_settings.get(&service_id) {
            return Some(bps);
        }
        for &fk in flat_keys {
            if let Some(&bps) = prof.service_settings.get(&fk) {
                return Some(bps);
            }
        }
    } else if is_external_flex
        && !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now))
    {
        return None;
    }
    Some(default_bps)
}

/// Evaluates `CharacterHasSetting(structure_id, setting_id)` against both `general_settings` and
/// `service_acl_rules` (including delayed `ChangeProfiles` transitions).
pub fn evaluate_character_setting(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    setting_id: i64,
) -> bool {
    let now = ctx.now_unix();
    if !snap.structures.contains_key(&structure_id) {
        return false;
    }
    let is_external = matches!(setting_id, SETTING_JUMP_BRIDGE_ACTIVATION | SETTING_CYNO_BEACON);
    if !is_external
        && !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now))
    {
        return false;
    }
    let ext = load_ext_state(snap);
    if let Some(prof) = effective_profile_for_structure(&ext, structure_id, Some(now)) {
        if setting_id == SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
            return snap
                .structures
                .get(&structure_id)
                .is_some_and(|st| crate::services::structure_control_ops::can_character_control_structure_with_profile(snap, &ext, st, Some(prof), char_id));
        }
        if let Some(res) = evaluate_domain_acl_on_profile(snap, &ext, prof, char_id, &[setting_id], false) {
            return res.is_ok();
        }
        if let Some(&granted) = prof.general_settings.get(&setting_id) {
            return granted;
        }
    } else if setting_id == SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
        return snap
            .structures
            .get(&structure_id)
            .is_some_and(|st| crate::services::structure_control_ops::can_character_control_structure_with_profile(snap, &ext, st, None, char_id));
    } else if is_external
        && !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now))
    {
        return false;
    }
    matches!(setting_id, 1 | 2 | SETTING_HOUSING_CAN_DOCK)
}

/// Domain helper for Reprocessing, Clone Bay, Repair, Industry, and Jump Bridge handlers:
/// checks per-service ACL (plus delayed profile apply) and returns `Ok(Some(bps))` when a service
/// ACL rule matched, `Ok(None)` when no per-service ACL was configured, or `Err(BridgeRpcError)` when denied.
pub fn check_service_domain_acl_and_tax(
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    candidate_setting_ids: &[i64],
    any_of_candidates: bool,
    now_unix_opt: Option<u64>,
    err_code: &str,
    service_label: &str,
) -> Result<Option<i64>, BridgeRpcError> {
    let ext = load_ext_state(snap);
    if let Some(prof) = effective_profile_for_structure(&ext, structure_id, now_unix_opt) {
        match evaluate_domain_acl_on_profile(
            snap,
            &ext,
            prof,
            char_id,
            candidate_setting_ids,
            any_of_candidates,
        ) {
            Some(Ok(bps)) => return Ok(Some(bps)),
            Some(Err(())) => {
                return Err(err(
                    err_code,
                    format!(
                        "Character {char_id} is denied {service_label} service on structure {structure_id} by service access profile"
                    ),
                ));
            }
            None => {}
        }
    }
    Ok(None)
}

/// Evaluates Industry activity service ACL (`1` Manufacturing, `3|4|5` Research/Copying, `8` Invention, `9|11` Reactions)
/// and returns `Ok(Some(tax_rate_fraction))` when a per-ACL-group rate applies.
pub fn check_industry_activity_acl_and_tax(
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    activity_id: u8,
    now_unix: u64,
) -> Result<Option<f64>, BridgeRpcError> {
    if !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now_unix)) {
        return Err(err(
            "StructureServiceAccessDenied",
            format!("Character {char_id} is denied access to structure {structure_id}"),
        ));
    }
    let (cands, any_of): (&[i64], bool) = match activity_id {
        1 => (&[29, 30, 31, 24, 35_878], false),
        3..=5 => (&[32, 24, 35_891], false),
        8 => (&[33, 24, 35_886], false),
        9 | 11 => (&[28, 26, 27, 24, 45_537, 45_538, 45_539, 7], true),
        _ => (&[24, 29], true),
    };
    let bps_opt = check_service_domain_acl_and_tax(
        snap,
        structure_id,
        char_id,
        cands,
        any_of,
        Some(now_unix),
        "StructureServiceAccessDenied",
        "industry",
    )?;
    Ok(bps_opt.map(|bps| (bps.max(0) as f64 / 10_000.0).clamp(0.0, 1.0)))
}

/// Evaluates Jump Bridge (`35841` Ansiblex) access for `char_id` at `now_unix`, honoring
/// `SETTING_JUMP_BRIDGE_ACTIVATION` (`34`) per-service ACLs when configured, or general structure
/// access otherwise.
pub fn can_character_use_jump_bridge(
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    now_unix: u64,
) -> bool {
    let ext = load_ext_state(snap);
    if let Some(prof) = effective_profile_for_structure(&ext, structure_id, Some(now_unix)) {
        if prof.blocked_characters.contains(&char_id) {
            return false;
        }
        let (char_corp, char_ally) = pilot_corp_and_ally(snap, char_id);
        if (char_corp > 0 && prof.blocked_corporations.contains(&char_corp))
            || (char_ally > 0 && prof.blocked_alliances.contains(&char_ally))
        {
            return false;
        }
        if let Some(res) = evaluate_domain_acl_on_profile(
            snap,
            &ext,
            prof,
            char_id,
            &[SETTING_JUMP_BRIDGE_ACTIVATION, 35_913, 35_841, SERVICE_JUMP_BRIDGE, 9],
            false,
        ) {
            return res.is_ok();
        }
    }
    ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now_unix))
}

/// Checks whether `structure_id` (if it is an Upwell structure in `snap.structures`) has an online
/// market service, allows `char_id` structure & `SETTING_MARKET_TAX` (`4`) service ACL access at
/// `now_unix`, and returns `Ok(Some((owner_corp_id, broker_rate, pilot_bps)))` for Upwell structures
/// or `Ok(None)` for NPC stations / unknown IDs.
pub fn check_upwell_market_access_and_rate(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    now_unix: u64,
    offline_err_code: &str,
) -> Result<Option<(i64, f64, Option<i64>)>, BridgeRpcError> {
    let Some(st) = snap.structures.get(&structure_id) else {
        return Ok(None);
    };
    if st.state == "Destroyed"
        || !crate::services::market_trading::is_structure_market_online(snap, structure_id)
        || !ops::structure_allows_market_orders(ctx, snap, structure_id)
    {
        return Err(err(
            offline_err_code,
            format!("Standup Market Hub I service module at structure {structure_id} is offline"),
        ));
    }
    if !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now_unix)) {
        return Err(err(
            "StructureServiceAccessDenied",
            format!("Character {char_id} is excluded by structure {structure_id} access list"),
        ));
    }
    let ext = load_ext_state(snap);
    if let Some(prof) = effective_profile_for_structure(&ext, structure_id, Some(now_unix)) {
        if prof.general_settings.get(&SETTING_MARKET_TAX) == Some(&false) {
            return Err(err(
                "StructureServiceAccessDenied",
                format!("Character {char_id} is denied market service on structure {structure_id} by service access profile"),
            ));
        }
    }
    let pilot_bps = check_service_domain_acl_and_tax(
        snap,
        structure_id,
        char_id,
        &[SETTING_MARKET_TAX],
        false,
        Some(now_unix),
        "StructureServiceAccessDenied",
        "market",
    )?;
    let broker_rate = pilot_bps
        .map(|bps| 0.005 + (bps.max(0) as f64 / 10_000.0).clamp(0.0, 1.0))
        .unwrap_or_else(|| crate::services::market_trading::resolve_structure_broker_rate(snap, structure_id));
    Ok(Some((st.owner_corp_id, broker_rate, pilot_bps)))
}

/// Applies any due delayed structure profile transitions in `snap` and runs `check_upwell_market_access_and_rate`.
pub fn apply_due_profiles_and_check_upwell_market(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    now_unix: u64,
    offline_err_code: &str,
) -> Result<Option<(i64, f64, Option<i64>)>, BridgeRpcError> {
    if snap.structures.contains_key(&structure_id) {
        let mut ext = load_ext_state(snap);
        if ext.pending_profile_applications.values().any(|&(_, apply_at)| now_unix >= apply_at) {
            let released = ops::apply_pending_profile_applications_with_snap(snap, &mut ext, now_unix);
            if !released.is_empty() {
                crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
            }
        }
    }
    check_upwell_market_access_and_rate(ctx, snap, structure_id, char_id, now_unix, offline_err_code)
}

/// Validates and classifies an entity ID for `ownerGroupManager`, also permitting `0` (`PUBLIC_MEMBER_ID`).
pub fn validate_group_member_entity(
    snap: &WorldStateSnapshot,
    entity_id: i64,
    is_blocked: bool,
) -> Result<(bool, bool, bool), BridgeRpcError> {
    if entity_id == 0 && !is_blocked {
        return Ok((false, false, false));
    }
    validate_and_classify_entity(snap, entity_id)
}

pub fn handle_create_owner_group(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let name = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("Access Group")
        .to_string();
    let description = args
        .get(1)
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let corp_owned = args
        .get(2)
        .map(|a| matches!(a, PyRep::Bool(true) | PyRep::Integer(1)))
        .unwrap_or(true);
    ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if corp_owned {
            ops::require_corp_profile_manager(snap, session, char_id, corp_id)?;
        }
        let mut ext = load_ext_state(snap);
        let group_id = ext.owner_groups.keys().copied().max().unwrap_or(80_000_000) + 1;
        ext.owner_groups.insert(
            group_id,
            ops::AccessGroupRecord {
                group_id,
                creator_id: char_id,
                owner_corp_id: if corp_owned { corp_id } else { 0 },
                name: name.clone(),
                description: description.clone(),
                members: HashMap::new(),
            },
        );
        save_ext_state(snap, &ext);
        Ok(PyRep::keyval([
            ("groupID", PyRep::Integer(group_id)),
            ("name", PyRep::String(name)),
            ("description", PyRep::String(description)),
            ("creatorID", PyRep::Integer(char_id)),
            ("corpOwned", PyRep::Bool(corp_owned)),
        ]))
    })
}

pub fn apply_group_member_batch(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_add: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_or_struct_id = ops::arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingGroupId", "ownerGroupManager requires groupID"))?;
    let explicit_blocked = args.get(2).and_then(|a| match a {
        PyRep::Bool(b) => Some(*b),
        PyRep::Integer(i) => Some(*i != 0),
        _ => None,
    });
    let default_blocked = explicit_blocked.unwrap_or(false);
    let members: Vec<(i64, bool)> = match args.get(1) {
        Some(PyRep::Dict(d)) => d
            .iter()
            .filter_map(|(k, v)| {
                k.as_i64().map(|eid| {
                    (
                        eid,
                        explicit_blocked.unwrap_or_else(|| {
                            v.as_i64().is_some_and(|t| t < 0) || matches!(v, PyRep::Bool(false))
                        }),
                    )
                })
            })
            .collect(),
        Some(PyRep::List(xs) | PyRep::Tuple(xs)) => xs
            .iter()
            .filter_map(|x| x.as_i64().map(|eid| (eid, default_blocked)))
            .collect(),
        Some(PyRep::Integer(eid)) => vec![(*eid, default_blocked)],
        _ => return Err(err("MissingEntityId", "ownerGroupManager requires memberIDs")),
    };
    let (notify_corp, released) = ctx.db.try_with_mut_snapshot(|snap| {
        let classified: Vec<(i64, bool, (bool, bool, bool))> = members
            .iter()
            .map(|&(eid, blk)| Ok((eid, blk, validate_group_member_entity(snap, eid, blk)?)))
            .collect::<Result<_, BridgeRpcError>>()?;
        let mut ext = load_ext_state(snap);
        if let Some(grp) = ext.owner_groups.get(&group_or_struct_id) {
            let is_admin_or_dir = grp.creator_id == char_id
                || grp.members.get(&char_id) == Some(&1)
                || (grp.owner_corp_id != 0
                    && ops::require_corp_profile_manager(snap, session, char_id, grp.owner_corp_id)
                        .is_ok())
                || session.has_gm_role();
            let is_mgr = is_admin_or_dir || grp.members.get(&char_id) == Some(&2);
            if !is_mgr {
                return Err(err(
                    "InsufficientCorpRoles",
                    "Caller is not a manager or admin of this access group",
                ));
            }
            for &(eid, blk, _) in &classified {
                if eid == grp.creator_id && (!is_add || blk) && !session.has_gm_role() {
                    return Err(err(
                        "InsufficientCorpRoles",
                        "Cannot remove or block access group creator",
                    ));
                }
                if !is_admin_or_dir
                    && (eid == grp.creator_id
                        || matches!(grp.members.get(&eid).copied(), Some(1 | 2)))
                {
                    return Err(err(
                        "InsufficientCorpRoles",
                        "ACL Managers cannot overwrite or remove Admins or Managers",
                    ));
                }
            }
            let owner_corp = grp.owner_corp_id;
            if let Some(grp_mut) = ext.owner_groups.get_mut(&group_or_struct_id) {
                for &(eid, blk, _) in &classified {
                    if is_add {
                        grp_mut.members.insert(eid, if blk { -1 } else { 0 });
                    } else {
                        grp_mut.members.remove(&eid);
                    }
                }
            }
            let linked_pids: Vec<i64> = ext
                .profiles_by_id
                .values()
                .filter(|p| {
                    p.access_group_ids.contains(&group_or_struct_id)
                        || p.defense_group_ids.contains(&group_or_struct_id)
                        || p.service_acl_rules
                            .values()
                            .any(|rows| rows.iter().any(|&(gid, _)| gid == group_or_struct_id))
                })
                .map(|p| p.profile_id)
                .collect();
            for pid in linked_pids {
                let is_docking_linked = ext
                    .profiles_by_id
                    .get(&pid)
                    .is_some_and(|p| p.access_group_ids.contains(&group_or_struct_id));
                if is_docking_linked {
                    let attached_gids: Vec<i64> = ext
                        .profiles_by_id
                        .get(&pid)
                        .map(|p| p.access_group_ids.iter().copied().collect())
                        .unwrap_or_default();
                    for &(eid, blk, (is_ally, is_corp, _)) in &classified {
                        if eid == 0 {
                            continue;
                        }
                        let still_in_other = |want_blk: bool| {
                            attached_gids.iter().any(|&gid| {
                                gid != group_or_struct_id
                                    && ext
                                        .owner_groups
                                        .get(&gid)
                                        .and_then(|g| g.members.get(&eid))
                                        .is_some_and(|&mt| (mt < 0) == want_blk)
                            })
                        };
                        if is_add || !still_in_other(blk) {
                            if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                                ops::apply_entity_to_profile(prof, eid, is_ally, is_corp, blk, is_add);
                            }
                        }
                        if !is_add && explicit_blocked.is_none() && !still_in_other(!blk) {
                            if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                                ops::apply_entity_to_profile(prof, eid, is_ally, is_corp, !blk, false);
                            }
                        }
                    }
                }
                ops::sync_assigned_structures_for_profile(&mut ext, pid);
            }
            save_ext_state(snap, &ext);
            let released =
                crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(
                    snap,
                );
            Ok((owner_corp, released))
        } else {
            let st = ops::require_structure_corp_manager(snap, session, char_id, group_or_struct_id)?;
            let prof = ext.access_profiles.entry(group_or_struct_id).or_default();
            for (eid, blk, (is_ally, is_corp, _)) in classified {
                if eid == 0 {
                    continue;
                }
                ops::apply_entity_to_profile(prof, eid, is_ally, is_corp, blk, is_add);
                if !is_add && explicit_blocked.is_none() {
                    ops::apply_entity_to_profile(prof, eid, is_ally, is_corp, !blk, false);
                }
            }
            save_ext_state(snap, &ext);
            let released =
                crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(
                    snap,
                );
            Ok((st.owner_corp_id, released))
        }
    })?;
    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    if notify_corp > 0 {
        ctx.notify(
            crate::router::NotificationTarget::Corporation(notify_corp),
            "OnOwnerGroupsUpdated",
            "corpid",
            vec![PyRep::Dict(vec![(
                PyRep::Integer(group_or_struct_id),
                PyRep::Integer(if is_add { 1 } else { 2 }),
            )])],
        );
    }
    Ok(PyRep::List(Vec::new()))
}

#[cfg(test)] #[path = "structure_profile_services_tests.rs"]
mod structure_profile_services_tests;

