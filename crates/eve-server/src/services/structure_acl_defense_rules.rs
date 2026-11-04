//! `structure_acl_defense_rules` — Structure Access Lists (ACLs), Docking Access Control,
//! Structure Boarding/Eject, Auto-Defense Targeting Settings, Anchor Restrictions,
//! EWAR Immunity, Fitting State Locks, and Mobile Cynosural Inhibitors (`feat:structures.*`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::station_structure::structure_lifecycle as life;
use super::station_structure::structure_upwell_ops::{
    self as ops, load_ext_state, save_ext_state,
};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

fn arg_i64(args: &[PyRep], idx: usize) -> Option<i64> {
    args.get(idx).and_then(|a| match a {
        PyRep::Integer(i) => Some(*i),
        _ => None,
    })
}

// ============================================================================
// 1. Structure Access Lists (`feat:structures.access-lists`)
// ============================================================================

pub const ACL_ROLE_BLOCKED: i64 = -1;
pub const ACL_ROLE_MEMBER: i64 = 0;
pub const ACL_ROLE_ADMIN: i64 = 1;
pub const ACL_ROLE_MANAGER: i64 = 2;

pub fn is_acl_admin(char_id: i64, grp: &ops::AccessGroupRecord) -> bool {
    grp.creator_id == char_id || grp.members.get(&char_id).copied() == Some(ACL_ROLE_ADMIN)
}

pub fn is_acl_manager_or_admin(char_id: i64, grp: &ops::AccessGroupRecord) -> bool {
    is_acl_admin(char_id, grp)
        || grp.members.get(&char_id).copied() == Some(ACL_ROLE_MANAGER)
}

/// Handles `ownerGroupManager.UpdateMembershipTypes(groupID, memberIDs, membershipType)`
/// (`eve/client/script/ui/structure/accessGroups/accessGroupsController.pyj`).
pub fn handle_set_member_role(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_id = arg_i64(args, 0).ok_or_else(|| err("MissingGroupId", "UpdateMembershipTypes requires groupID"))?;
    let default_role = arg_i64(args, 2).unwrap_or(ACL_ROLE_MEMBER);
    let updates: Vec<(i64, i64)> = match args.get(1) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            let ids: Vec<i64> = items.iter().filter_map(|v| v.as_i64()).collect();
            if ids.is_empty() {
                return Err(err("MissingEntityId", "UpdateMembershipTypes requires memberIDs"));
            }
            ids.into_iter().map(|id| (id, default_role)).collect()
        }
        Some(PyRep::Dict(entries)) => {
            let pairs: Vec<(i64, i64)> = entries
                .iter()
                .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64().unwrap_or(default_role))))
                .collect();
            if pairs.is_empty() {
                return Err(err("MissingEntityId", "UpdateMembershipTypes requires memberIDs"));
            }
            pairs
        }
        _ => {
            let eid = arg_i64(args, 1).ok_or_else(|| err("MissingEntityId", "UpdateMembershipTypes requires memberIDs"))?;
            vec![(eid, default_role)]
        }
    };
    for &(_, role) in &updates {
        if !matches!(role, ACL_ROLE_BLOCKED | ACL_ROLE_MEMBER | ACL_ROLE_ADMIN | ACL_ROLE_MANAGER) {
            return Err(err("InvalidAclRole", format!("Role {role} is not a valid ACL role (-1, 0, 1, 2)")));
        }
    }

    let (notify_corp, released) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut classified = Vec::with_capacity(updates.len());
        for &(entity_id, role) in &updates {
            let (is_ally, is_corp, _) = ops::validate_and_classify_entity(snap, entity_id)?;
            classified.push((entity_id, role, is_ally, is_corp));
        }
        let mut ext = load_ext_state(snap);
        let grp = ext.owner_groups.get(&group_id).ok_or_else(|| err("AccessGroupNotFound", format!("Group {group_id} not found")))?;

        let is_admin_or_dir = is_acl_admin(char_id, grp)
            || (grp.owner_corp_id != 0 && ops::require_corp_profile_manager(snap, session, char_id, grp.owner_corp_id).is_ok())
            || session.has_gm_role();
        if !is_admin_or_dir && !is_acl_manager_or_admin(char_id, grp) {
            return Err(err("InsufficientCorpRoles", "Caller lacks manager permissions in this access group"));
        }
        for &(entity_id, role, _, _) in &classified {
            let target_existing_role = grp.members.get(&entity_id).copied().unwrap_or(0);
            if entity_id == grp.creator_id && role != ACL_ROLE_ADMIN && !session.has_gm_role() {
                return Err(err("InsufficientCorpRoles", "Cannot demote or block access group creator"));
            }
            if (matches!(role, ACL_ROLE_ADMIN | ACL_ROLE_MANAGER)
                || entity_id == grp.creator_id
                || matches!(target_existing_role, ACL_ROLE_ADMIN | ACL_ROLE_MANAGER))
                && !is_admin_or_dir
            {
                return Err(err("InsufficientCorpRoles", "Modifying admin or manager role requires ACL Admin or Director"));
            }
        }

        let owner_corp = grp.owner_corp_id;
        if let Some(grp_mut) = ext.owner_groups.get_mut(&group_id) {
            for &(entity_id, role, _, _) in &classified {
                grp_mut.members.insert(entity_id, role);
            }
        }

        let linked_pids: Vec<i64> = ext
            .profiles_by_id
            .values()
            .filter(|p| p.access_group_ids.contains(&group_id) || p.defense_group_ids.contains(&group_id))
            .map(|p| p.profile_id)
            .collect();

        for pid in linked_pids {
            if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                for &(entity_id, role, is_ally, is_corp) in &classified {
                    let is_blocked = role == ACL_ROLE_BLOCKED;
                    ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, is_blocked, true);
                    if !is_blocked {
                        ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, true, false);
                    } else {
                        ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, false, false);
                    }
                }
            }
            ops::sync_assigned_structures_for_profile(&mut ext, pid);
        }

        save_ext_state(snap, &ext);
        let released = crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(snap);
        Ok((owner_corp, released))
    })?;

    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    if notify_corp > 0 {
        ctx.notify(
            crate::router::NotificationTarget::Corporation(notify_corp),
            "OnOwnerGroupsUpdated",
            "corpid",
            vec![PyRep::Dict(vec![(PyRep::Integer(group_id), PyRep::Integer(1))])],
        );
    }
    Ok(PyRep::Bool(true))
}

pub fn handle_edit_group(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_id = arg_i64(args, 0).ok_or_else(|| err("MissingGroupId", "EditGroup requires groupID"))?;
    let name = args.get(1).and_then(|a| a.as_str()).map(str::to_string);
    let desc = args.get(2).and_then(|a| a.as_str()).map(str::to_string);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let grp = ext.owner_groups.get(&group_id).ok_or_else(|| err("AccessGroupNotFound", format!("Group {group_id} not found")))?;
        let is_admin_or_dir = is_acl_admin(char_id, grp)
            || (grp.owner_corp_id != 0 && ops::require_corp_profile_manager(snap, session, char_id, grp.owner_corp_id).is_ok())
            || session.has_gm_role();

        if !is_admin_or_dir {
            return Err(err("InsufficientCorpRoles", "Editing access group requires ACL Admin or Director"));
        }
        if let Some(grp_mut) = ext.owner_groups.get_mut(&group_id) {
            if let Some(n) = name {
                grp_mut.name = n;
            }
            if let Some(d) = desc {
                grp_mut.description = d;
            }
        }
        save_ext_state(snap, &ext);
        Ok(())
    })?;
    Ok(PyRep::Bool(true))
}

pub fn handle_get_group(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_id = arg_i64(args, 0).ok_or_else(|| err("MissingGroupId", "GetGroup requires groupID"))?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let grp = ext.owner_groups.get(&group_id).ok_or_else(|| err("AccessGroupNotFound", format!("Group {group_id} not found")))?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let can_view = grp.creator_id == char_id
            || grp.members.get(&char_id).is_some_and(|&r| r >= 0)
            || (grp.owner_corp_id != 0 && grp.owner_corp_id == caller_corp)
            || session.has_gm_role();
        if !can_view {
            return Err(err("AccessGroupAccessDenied", "Caller does not have access to view this access group"));
        }
        Ok(PyRep::keyval([
            ("groupID", PyRep::Integer(grp.group_id)),
            ("name", PyRep::String(grp.name.clone())),
            ("description", PyRep::String(grp.description.clone())),
            ("creatorID", PyRep::Integer(grp.creator_id)),
            ("ownerCorpID", PyRep::Integer(grp.owner_corp_id)),
            ("memberCount", PyRep::Integer(grp.members.len() as i64)),
        ]))
    })
}

pub fn handle_get_members(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_id = arg_i64(args, 0).ok_or_else(|| err("MissingGroupId", "GetMembers requires groupID"))?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let grp = ext.owner_groups.get(&group_id).ok_or_else(|| err("AccessGroupNotFound", format!("Group {group_id} not found")))?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let can_view = grp.creator_id == char_id
            || grp.members.get(&char_id).is_some_and(|&r| r >= 0)
            || (grp.owner_corp_id != 0 && grp.owner_corp_id == caller_corp)
            || session.has_gm_role();
        if !can_view {
            return Err(err("AccessGroupAccessDenied", "Caller does not have access to view this access group's members"));
        }
        let mut entries = Vec::new();
        for (&eid, &role) in &grp.members {
            entries.push((PyRep::Integer(eid), PyRep::Integer(role)));
        }
        Ok(PyRep::Dict(entries))
    })
}

// ============================================================================
// 2. Docking ACL Verification (`feat:structures.docking-acl`)
// ============================================================================

pub fn apply_pending_profile_applications(ctx: &ServerContext) {
    ops::sync_pending_profiles_in_ctx(ctx);
}

pub fn verify_structure_docking_acl(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    if !ops::evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
        return Err(err(
            "StructureDockingAccessDenied",
            format!("Character {char_id} is denied docking access by structure {structure_id} ACL"),
        ));
    }
    Ok(())
}

// ============================================================================
// 4. Structure Auto-Defense Settings (`feat:structures.defense-settings`)
// ============================================================================

/// Active aggressor retention window (`15 minutes = 900 s`) for Upwell structure auto-defense.
pub const STRUCTURE_ATTACKER_TTL_SECS: u64 = 900;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructureDefenseSettings {
    pub structure_id: i64,
    pub auto_defense_enabled: bool,
    pub auto_target_attackers: bool,
    pub auto_target_war_targets: bool,
    pub standing_threshold: f64,
    pub target_blocked_acl: bool,
    #[serde(default)]
    pub known_attackers: Vec<i64>,
    #[serde(default)]
    pub known_attacker_timestamps: HashMap<i64, u64>,
}

impl Default for StructureDefenseSettings {
    fn default() -> Self {
        Self {
            structure_id: 0,
            auto_defense_enabled: false,
            auto_target_attackers: true,
            auto_target_war_targets: true,
            standing_threshold: 0.0,
            target_blocked_acl: false,
            known_attackers: Vec::new(),
            known_attacker_timestamps: HashMap::new(),
        }
    }
}

impl StructureDefenseSettings {
    /// Records `attacker_char_id` as an active aggressor at `now_unix`, pruning expired entries.
    pub fn record_attacker(&mut self, attacker_char_id: i64, now_unix: u64) {
        self.prune_expired_attackers(now_unix);
        if attacker_char_id > 0 {
            if !self.known_attackers.contains(&attacker_char_id) {
                self.known_attackers.push(attacker_char_id);
            }
            self.known_attacker_timestamps.insert(attacker_char_id, now_unix);
        }
    }

    /// Removes attacker entries whose recorded timestamp has exceeded `STRUCTURE_ATTACKER_TTL_SECS`.
    pub fn prune_expired_attackers(&mut self, now_unix: u64) {
        let cutoff = now_unix.saturating_sub(STRUCTURE_ATTACKER_TTL_SECS);
        self.known_attacker_timestamps.retain(|_, &mut ts| ts > cutoff);
        let timestamps = &self.known_attacker_timestamps;
        self.known_attackers
            .retain(|cid| timestamps.contains_key(cid));
    }

    /// Returns `true` if `pilot_id` has an active, non-expired attacker record at `now_unix`.
    pub fn is_active_attacker(&self, pilot_id: i64, now_unix: u64) -> bool {
        let cutoff = now_unix.saturating_sub(STRUCTURE_ATTACKER_TTL_SECS);
        self.known_attacker_timestamps
            .get(&pilot_id)
            .copied()
            .is_some_and(|ts| ts > cutoff)
    }
}

pub fn defense_blob_key(structure_id: i64) -> String {
    format!("structure_defense:{structure_id}")
}

pub fn load_defense_settings(snap: &WorldStateSnapshot, structure_id: i64) -> StructureDefenseSettings {
    let key = defense_blob_key(structure_id);
    snap.service_state_blobs
        .get(&key)
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_else(|| StructureDefenseSettings {
            structure_id,
            ..Default::default()
        })
}

pub fn save_defense_settings(snap: &mut WorldStateSnapshot, settings: &StructureDefenseSettings) {
    let mut sanitized = settings.clone();
    if !sanitized.standing_threshold.is_finite() {
        sanitized.standing_threshold = 0.0;
    }
    let key = defense_blob_key(sanitized.structure_id);
    if let Ok(json) = serde_json::to_string(&sanitized) {
        snap.service_state_blobs.insert(key, json);
    }
}

fn resolve_owner_standing_toward_pilot(
    snap: &WorldStateSnapshot,
    owner_corp_id: i64,
    owner_ally_id: i64,
    pilot_id: i64,
    pilot_corp: i64,
    pilot_ally: i64,
) -> f64 {
    if let Some(list) = snap.corp_contacts.get(&owner_corp_id) {
        for target in [pilot_id, pilot_corp, pilot_ally] {
            if target > 0 {
                if let Some(c) = list.iter().find(|c| c.contact_id == target) {
                    return c.standing;
                }
            }
        }
    }
    if owner_ally_id > 0 {
        if let Some(list) = snap.alliance_contacts.get(&owner_ally_id) {
            for target in [pilot_id, pilot_corp, pilot_ally] {
                if target > 0 {
                    if let Some(c) = list.iter().find(|c| c.contact_id == target) {
                        return c.standing;
                    }
                }
            }
        }
    }
    0.0
}

pub fn evaluate_auto_defense_target(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    pilot_id: i64,
) -> bool {
    let Some(st) = snap.structures.get(&structure_id) else {
        return false;
    };
    if matches!(
        st.state.as_str(),
        "Destroyed" | "Unanchored" | "AnchorVulnerable" | "Anchoring" | "NoCore"
    ) {
        return false;
    }
    let ext = load_ext_state(snap);
    let legacy = load_defense_settings(snap, structure_id);
    let (enabled, tgt_att, tgt_war, thresh, tgt_blk) =
        if let Some(ds) = ext.access_profiles.get(&structure_id).and_then(|p| p.defense_settings.as_ref()) {
            (
                ds.auto_defense_enabled,
                ds.auto_target_attackers,
                ds.auto_target_war_targets,
                ds.standing_threshold,
                ds.target_blocked_acl,
            )
        } else if let Some(prof) = ext.access_profiles.get(&structure_id).filter(|p| p.has_defense_config) {
            (
                prof.auto_defense_enabled,
                prof.auto_target_attackers,
                prof.auto_target_war_targets,
                prof.defense_standing_threshold,
                prof.target_blocked_acl,
            )
        } else {
            (
                legacy.auto_defense_enabled,
                legacy.auto_target_attackers,
                legacy.auto_target_war_targets,
                legacy.standing_threshold,
                legacy.target_blocked_acl,
            )
        };
    if !enabled {
        return false;
    }

    let (pilot_corp, pilot_ally) = snap
        .characters
        .get(&pilot_id)
        .map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0)))
        .unwrap_or((0, 0));
    let owner_ally = snap
        .corporations
        .get(&st.owner_corp_id)
        .and_then(|c| c.alliance_id)
        .unwrap_or(0);

    // Friendly exemption: owning corp and alliance members are never auto-targeted.
    if pilot_corp == st.owner_corp_id || (owner_ally > 0 && pilot_ally == owner_ally) {
        return false;
    }

    let now_unix = ctx.now_unix();
    if tgt_att && legacy.is_active_attacker(pilot_id, now_unix) {
        return true;
    }
    if tgt_war && are_corps_at_war_in_snap(snap, pilot_corp, st.owner_corp_id, now_unix) {
        return true;
    }

    // In high-security space (>= 0.45), automated targeting of non-attackers/non-wartargets
    // via standing or ACL is disabled by CONCORD safety rules.
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&st.solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec >= 0.45 && st.solar_system_id < 31_000_000 {
        return false;
    }

    let standing = resolve_owner_standing_toward_pilot(
        snap,
        st.owner_corp_id,
        owner_ally,
        pilot_id,
        pilot_corp,
        pilot_ally,
    );
    if standing < thresh {
        return true;
    }
    if tgt_blk {
        if let Some(p) = ext.access_profiles.get(&structure_id) {
            let grp_blk = p.access_group_ids.iter().chain(&p.defense_group_ids).any(|gid| {
                ext.owner_groups.get(gid).is_some_and(|g| {
                    g.members.get(&pilot_id) == Some(&-1)
                        || (pilot_corp > 0 && g.members.get(&pilot_corp) == Some(&-1))
                        || (pilot_ally > 0 && g.members.get(&pilot_ally) == Some(&-1))
                })
            });
            if grp_blk
                || p.blocked_characters.contains(&pilot_id)
                || p.defense_blocked_characters.contains(&pilot_id)
                || p.blocked_corporations.contains(&pilot_corp)
                || (pilot_ally > 0 && p.blocked_alliances.contains(&pilot_ally))
            {
                return true;
            }
        }
    }
    false
}

pub async fn trigger_structure_auto_defense_on_grid(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    structure_id: i64,
    attacker_char_id: Option<i64>,
    now_unix: u64,
) {
    crate::services::structure_weapons_ops::trigger_structure_auto_defense_on_grid(
        ctx,
        sys_handle,
        sys_id,
        structure_id,
        attacker_char_id,
        now_unix,
    )
    .await;
}

pub async fn trigger_system_structures_auto_defense(ctx: &ServerContext, sys_id: u32) {
    crate::services::structure_weapons_ops::trigger_system_structures_auto_defense(ctx, sys_id).await;
}
// ============================================================================
// 5. Anchoring Restrictions (`feat:structures.anchor-restrictions`)
// ============================================================================

pub fn validate_anchoring_restrictions(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    profile: &life::StructureProfile,
    sys_id: u32,
    pos: DVec3,
) -> Result<(), BridgeRpcError> {
    life::check_anchor_position(ctx, snap, profile, sys_id, pos)
}

// ============================================================================
// 6. EWAR Immunity (`feat:structures.ewar-immunity`)
// ============================================================================

pub fn is_structure_immune_to_ewar(snap: &WorldStateSnapshot, target_id: i64) -> bool {
    snap.structures.contains_key(&target_id)
}

// ============================================================================
// 7. Fitting State Locks (`feat:structures.fitting-state-locks`)
// ============================================================================

pub fn validate_structure_fitting_state(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    ext: &ops::StructureExtendedState,
    structure_id: i64,
    now: u64,
) -> Result<(eve_db::UpwellStructureRecord, life::StructureProfile), BridgeRpcError> {
    life::validate_structure_refit(ctx, snap, ext, structure_id, now)
}

// ============================================================================
// 8. Mobile Cynosural Inhibitor (`feat:structures.mobile-cyno-inhibitor`)
// ============================================================================

pub const TYPE_MOBILE_CYNO_INHIBITOR: u32 = 33476;
pub const GROUP_MOBILE_CYNO_INHIBITOR: u32 = 1249;
pub const MOBILE_CYNO_INHIBITOR_RANGE_M: f64 = 100_000.0; // 100 km

pub fn is_cyno_inhibited(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    sys_id: u32,
    pos: Option<DVec3>,
) -> bool {
    let now_unix = ctx.now_unix();
    let deployables = crate::services::mobile_deployables::read_deployable_state_snap(snap);
    snap.items.values().any(|i| {
        if i.location_id != sys_id as i64 {
            return false;
        }
        let is_inhibitor = i.type_id == TYPE_MOBILE_CYNO_INHIBITOR
            || ctx
                .sde
                .types
                .get(&i.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0)
                == GROUP_MOBILE_CYNO_INHIBITOR;
        if !is_inhibitor {
            return false;
        }
        if let Some(rec) = deployables.deployables.get(&i.item_id) {
            if rec.is_unanchored {
                return false;
            }
            let ready_unix = rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs);
            if now_unix < ready_unix {
                return false;
            }
            if rec.expires_at_unix > 0 && now_unix >= rec.expires_at_unix {
                return false;
            }
        }
        if let Some(test_pos) = pos {
            let live_or_snap_pos = ctx
                .systems
                .get_or_spawn(sys_id)
                .get_ball_position_sync(i.item_id)
                .or_else(|| snap.item_positions.get(&i.item_id).map(|p| DVec3::from_array(*p)));
            if let Some(inhibitor_pos) = live_or_snap_pos {
                return test_pos.distance(inhibitor_pos) <= MOBILE_CYNO_INHIBITOR_RANGE_M;
            }
        }
        true
    })
}

// ============================================================================
// 9. Fuel Bay, Startup Fuel, Quantum Core Ship-Fitting Gate & Alliance War Rules
// ============================================================================

pub fn are_corps_at_war_in_snap(
    snap: &WorldStateSnapshot,
    corp_a: i64,
    corp_b: i64,
    now_unix: u64,
) -> bool {
    if corp_a <= 0 || corp_b <= 0 || corp_a == corp_b {
        return false;
    }
    let ally_a = snap.corporations.get(&corp_a).and_then(|c| c.alliance_id).unwrap_or(0);
    let ally_b = snap.corporations.get(&corp_b).and_then(|c| c.alliance_id).unwrap_or(0);
    if ally_a > 0 && ally_a == ally_b {
        return false;
    }
    let ent_a = if ally_a > 0 { ally_a } else { corp_a };
    let ent_b = if ally_b > 0 { ally_b } else { corp_b };
    snap.wars.values().any(|w| {
        if !w.active {
            return false;
        }
        if snap
            .war_state
            .wars
            .get(&w.war_id)
            .and_then(|m| m.finish_at_unix)
            .is_some_and(|f| now_unix >= f)
        {
            return false;
        }
        if !w.mutual && now_unix < w.started_unix.saturating_add(86_400) {
            return false;
        }
        let is_agg = |ent: i64, corp: i64| w.aggressor_corp_id == ent || w.aggressor_corp_id == corp;
        let is_def_or_ally = |ent: i64, corp: i64| {
            w.defender_corp_id == ent
                || w.defender_corp_id == corp
                || snap.war_state.negotiations.iter().any(|n| {
                    n.war_id == w.war_id
                        && n.kind == eve_db::PersistedNegKind::Ally
                        && n.status == "Accepted"
                        && now_unix >= n.effective_unix.unwrap_or(0)
                        && (n.from_corp == ent || n.from_corp == corp)
                })
        };
        (is_agg(ent_a, corp_a) && is_def_or_ally(ent_b, corp_b))
            || (is_agg(ent_b, corp_b) && is_def_or_ally(ent_a, corp_a))
    })
}

pub fn has_structure_fuel_bay_role(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    corp_id: i64,
) -> bool {
    use eve_db::corp_roles::{
        ROLE_DIRECTOR, ROLE_STARBASE_CONFIG, ROLE_STARBASE_OPERATOR, ROLE_STATION_MANAGER,
    };
    const FUEL_ROLES: u64 =
        ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_STARBASE_OPERATOR | ROLE_STARBASE_CONFIG;
    session.has_gm_role()
        || (session.corp_role & FUEL_ROLES) != 0
        || snap.corporations.get(&corp_id).is_some_and(|c| {
            c.ceo_id == char_id
                || c.member_roles.get(&char_id).is_some_and(|&r| (r & FUEL_ROLES) != 0)
        })
        || snap
            .corp_role_assignments
            .get(&(corp_id, char_id))
            .is_some_and(|a| (a.roles & FUEL_ROLES) != 0)
}

pub fn is_fuel_block_type(ctx: &ServerContext, type_id: u32) -> bool {
    ctx.sde
        .types
        .get(&type_id)
        .is_some_and(|t| life::service_fuel_groups(ctx).contains(&t.group_id))
}

#[inline]
pub fn is_structure_fuel_flag(flag: eve_db::InventoryFlag) -> bool {
    flag == eve_db::InventoryFlag::SpecializedFuelBay
        || flag == eve_db::InventoryFlag::StructureFuel
        || flag as u16 == 172
}

pub fn is_valid_structure_fuel_bay_item(ctx: &ServerContext, type_id: u32) -> bool {
    is_fuel_block_type(ctx, type_id)
        || type_id == 16273
        || ctx.sde.types.get(&type_id).is_some_and(|t| t.group_id == 4729)
}

pub fn service_module_fuel_multiplier(ctx: &ServerContext, st_type_id: u32, mod_type_id: u32) -> f64 {
    let Some(profile) = life::structure_profile(ctx, st_type_id) else {
        return 1.0;
    };
    if life::service_family_for_module(ctx, mod_type_id) != life::structure_family(profile.group_id) {
        return 1.0;
    }
    if profile.fuel_bonus_pct < 0.0 {
        (1.0 + profile.fuel_bonus_pct / 100.0).clamp(0.1, 1.0)
    } else if profile.fuel_bonus_pct > 0.0 {
        (1.0 - profile.fuel_bonus_pct / 100.0).clamp(0.1, 1.0)
    } else {
        1.0
    }
}

pub fn service_module_online_fuel_cost(ctx: &ServerContext, st_type_id: u32, mod_type_id: u32) -> u32 {
    let raw = ctx
        .sde
        .types
        .get(&mod_type_id)
        .and_then(|t| t.default_attributes.get(&life::ATTR_SERVICE_FUEL_ONLINE))
        .copied()
        .unwrap_or(0.0);
    if raw <= 0.0 {
        return 0;
    }
    (raw * service_module_fuel_multiplier(ctx, st_type_id, mod_type_id))
        .round()
        .max(1.0) as u32
}

pub fn sync_seeded_structure_fuel_bay(
    _ctx: &ServerContext,
    _snap: &mut WorldStateSnapshot,
    _structure_id: i64,
) {
    // Automatic refill removed per R-P0-METENOX-FUEL-DUP: fuel bay items and st.fuel_blocks
    // are kept in sync on deposits, withdrawals, and consumption.
}

pub fn consume_structure_fuel_blocks(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
    amount: u32,
) -> bool {
    let bay_blocks: u32 = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == structure_id
                && is_structure_fuel_flag(i.flag)
                && i.quantity > 0
                && is_fuel_block_type(ctx, i.type_id)
        })
        .map(|i| i.quantity as u32)
        .fold(0u32, u32::saturating_add);
    let st_blocks = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
    let avail = if bay_blocks > 0 { bay_blocks } else { st_blocks };
    if avail < amount {
        return false;
    }
    if amount == 0 {
        return true;
    }
    let mut item_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == structure_id
                && is_structure_fuel_flag(i.flag)
                && i.quantity > 0
                && is_fuel_block_type(ctx, i.type_id)
        })
        .map(|i| i.item_id)
        .collect();
    item_ids.sort_unstable();
    let mut rem = amount;
    for iid in item_ids {
        if rem == 0 {
            break;
        }
        let remove_it = if let Some(it) = snap.items.get_mut(&iid) {
            let take = rem.min(it.quantity.max(0) as u32);
            it.quantity -= take as i32;
            it.version += 1;
            rem -= take;
            it.quantity <= 0
        } else {
            false
        };
        if remove_it {
            snap.items.remove(&iid);
        }
    }
    if let Some(st) = snap.structures.get_mut(&structure_id) {
        if bay_blocks > 0 {
            let rem_bay: u32 = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == structure_id
                        && is_structure_fuel_flag(i.flag)
                        && i.quantity > 0
                        && is_fuel_block_type(ctx, i.type_id)
                })
                .map(|i| i.quantity as u32)
                .fold(0u32, u32::saturating_add);
            st.fuel_blocks = rem_bay;
        } else {
            st.fuel_blocks = st.fuel_blocks.saturating_sub(amount);
        }
    }
    true
}

pub fn materialize_structure_fuel_for_disposal(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    structure_id: i64,
) {
    let backed_blocks: u32 = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == structure_id
                && is_structure_fuel_flag(i.flag)
                && i.quantity > 0
                && is_fuel_block_type(ctx, i.type_id)
        })
        .map(|i| i.quantity as u32)
        .fold(0u32, u32::saturating_add);
    let Some((total_blocks, owner_corp)) = snap.structures.get_mut(&structure_id).map(|st| {
        let fb = st.fuel_blocks;
        st.fuel_blocks = 0;
        (fb, st.owner_corp_id)
    }) else {
        return;
    };
    if total_blocks > backed_blocks {
        let unbacked = (total_blocks - backed_blocks).min(i32::MAX as u32) as i32;
        snap.next_entity_id += 1;
        let new_id = snap.next_entity_id;
        snap.items.insert(
            new_id,
            eve_db::InventoryItemRecord {
                item_id: new_id,
                type_id: 4247,
                owner_id: owner_corp,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::SpecializedFuelBay,
                quantity: unbacked,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    }
}

pub fn snapshot_system_balls_for_inventory(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> HashMap<i64, (DVec3, f64)> {
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.characters
                    .get(&char_id)
                    .map(|c| c.solar_system_id)
                    .filter(|&id| id > 0)
            })
        })
        .unwrap_or(0);
    if sys_id == 0 {
        return HashMap::new();
    }
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let res = sys_h
        .ballpark
        .read()
        .ok()
        .map(|bp| {
            bp.balls
                .iter()
                .map(|(&id, b)| (id, (b.position, b.radius)))
                .collect()
        })
        .unwrap_or_default();
    res
}

#[allow(clippy::too_many_arguments)]
pub fn validate_structure_fuel_bay_transfer(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    structure_id: i64,
    item_type_id: u32,
    is_deposit: bool,
    ch_dock: i64,
    ch_sys: i64,
    caller_ship_id: i64,
    pre_balls: &HashMap<i64, (DVec3, f64)>,
) -> Result<(), BridgeRpcError> {
    let st = snap.structures.get(&structure_id).ok_or_else(|| {
        err("StructureNotFound", format!("Structure {structure_id} not found"))
    })?;
    if st.state == life::STATE_DESTROYED {
        return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
    }
    if st.state == "Unanchored" {
        return Err(err(
            "StructureNotOperational",
            format!("Structure {structure_id} fuel bay is unavailable in state {}", st.state),
        ));
    }
    let reachable = if ch_dock == structure_id {
        true
    } else if i64::from(st.solar_system_id) == ch_sys && caller_ship_id > 0 {
        let ship_pr = pre_balls.get(&caller_ship_id).copied().or_else(|| {
            snap.item_positions
                .get(&caller_ship_id)
                .map(|&p| (DVec3::from_array(p), 50.0))
        });
        let st_rad = ctx.sde.types.get(&st.type_id).map(|t| t.radius).unwrap_or(5_000.0);
        let st_pr = pre_balls
            .get(&structure_id)
            .copied()
            .unwrap_or((DVec3::from_array(st.position), st_rad));
        ship_pr.is_some_and(|(sp, sr)| (sp.distance(st_pr.0) - sr - st_pr.1).max(0.0) <= 5_000.0)
    } else {
        false
    };
    if !reachable {
        let code = if is_deposit { "DestinationAccessDenied" } else { "ItemOwnershipViolation" };
        return Err(err(code, format!("Structure {structure_id} fuel bay is out of 5,000 m range")));
    }
    if is_deposit && !is_valid_structure_fuel_bay_item(ctx, item_type_id) {
        return Err(err(
            "InvalidInventoryFlag",
            format!("Item {item_type_id} is not valid structure fuel or reagent for SpecializedFuelBay"),
        ));
    }
    let caller_corp = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .or(session.corporation_id)
        .unwrap_or(0);
    if st.owner_corp_id >= 98_000_000 || snap.corporations.contains_key(&st.owner_corp_id) || !is_deposit {
        if caller_corp != st.owner_corp_id {
            return Err(err(
                "StructureOwnershipViolation",
                format!("Character {char_id} does not belong to structure owning corp {}", st.owner_corp_id),
            ));
        }
        if !has_structure_fuel_bay_role(snap, session, char_id, caller_corp) {
            return Err(err(
                "CorpRoleDenied",
                "Accessing the structure fuel bay requires Director, Station Manager, or Fuel Technician role",
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn finalize_fuel_bay_move_in_snap(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    moved_item_id: i64,
    old_loc: i64,
    old_flag: eve_db::InventoryFlag,
    new_loc: i64,
    new_flag: eve_db::InventoryFlag,
    char_id: i64,
    caller_corp: i64,
) -> (Option<eve_db::InventoryItemRecord>, Option<u32>) {
    let from_fuel = is_structure_fuel_flag(old_flag)
        && snap.structures.contains_key(&old_loc);
    let to_fuel = is_structure_fuel_flag(new_flag)
        && snap.structures.contains_key(&new_loc);
    if !from_fuel && !to_fuel {
        return (snap.items.get(&moved_item_id).cloned(), None);
    }
    let Some(moving) = snap.items.get(&moved_item_id).cloned() else {
        return (None, None);
    };
    let same_bay = from_fuel && to_fuel && old_loc == new_loc;
    let qty_u32 = moving.quantity.max(0) as u32;
    let is_block = is_fuel_block_type(ctx, moving.type_id);
    if from_fuel && !same_bay {
        if is_block {
            if let Some(st) = snap.structures.get_mut(&old_loc) {
                st.fuel_blocks = st.fuel_blocks.saturating_sub(qty_u32);
            }
        }
        if !to_fuel {
            let new_owner = if (116..=122).contains(&(new_flag as u16)) && caller_corp > 0 {
                caller_corp
            } else {
                char_id
            };
            if let Some(it) = snap.items.get_mut(&moved_item_id) {
                it.owner_id = new_owner;
            }
        }
    }
    if to_fuel {
        let owner_corp = snap.structures.get(&new_loc).map(|s| s.owner_corp_id).unwrap_or(caller_corp);
        if let Some(it) = snap.items.get_mut(&moved_item_id) {
            it.owner_id = owner_corp;
        }
        if !same_bay && is_block {
            if let Some(st) = snap.structures.get_mut(&new_loc) {
                st.fuel_blocks = st.fuel_blocks.saturating_add(qty_u32);
            }
        }
        if !moving.singleton && !moving.is_blueprint_copy && moving.dynamic_attributes.is_empty() {
            let merge_target = snap
                .items
                .values()
                .find(|i| {
                    i.item_id != moved_item_id
                        && i.location_id == new_loc
                        && is_structure_fuel_flag(i.flag)
                        && i.type_id == moving.type_id
                        && !i.singleton
                        && !i.is_blueprint_copy
                        && i.dynamic_attributes.is_empty()
                })
                .map(|i| i.item_id);
            if let Some(tid) = merge_target {
                if let Some(new_q) = snap.items.get(&tid).and_then(|t| t.quantity.checked_add(moving.quantity)) {
                    snap.items.remove(&moved_item_id);
                    if let Some(t) = snap.items.get_mut(&tid) {
                        t.owner_id = owner_corp;
                        t.quantity = new_q;
                        t.version += 1;
                    }
                    let fb = snap.structures.get(&new_loc).map(|s| s.fuel_blocks);
                    return (snap.items.get(&tid).cloned(), fb);
                }
            }
        }
        let fb = snap.structures.get(&new_loc).map(|s| s.fuel_blocks);
        return (snap.items.get(&moved_item_id).cloned(), fb);
    }
    let fb = snap.structures.get(&old_loc).map(|s| s.fuel_blocks);
    (snap.items.get(&moved_item_id).cloned(), fb)
}

pub fn validate_docked_ship_fitting_facility_in_snap(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    dock_id: i64,
) -> Result<(), BridgeRpcError> {
    if dock_id <= 0 {
        return Ok(());
    }
    if dock_id < 1_000_000_000_000 {
        if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, dock_id, 17) {
            return Err(err(
                "StationServiceUnavailable",
                format!("Station {dock_id} does not offer fitting facilities"),
            ));
        }
        return Ok(());
    }
    if let Some(st) = snap.structures.get(&dock_id) {
        if st.state == life::STATE_DESTROYED {
            return Err(err("StructureDestroyed", format!("Structure {dock_id} is destroyed")));
        }
        if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
            return Err(err(
                "StructureNotOperational",
                format!("Structure {dock_id} is in state '{}' and cannot service ship fitting", st.state),
            ));
        }
        if (!st.quantum_core_installed || matches!(st.state.as_str(), "NoCore" | "Onlining"))
            && life::structure_profile(ctx, st.type_id).is_some_and(|p| p.core_type_id.is_some())
        {
            return Err(err(
                "QuantumCoreRequired",
                format!("Ship fitting service is disabled at structure {dock_id} until its Quantum Core is online"),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// RPC Dispatcher
// ============================================================================

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("ownerGroupManager", "UpdateMembershipTypes") => Some(handle_set_member_role(ctx, session, args)),
        ("ownerGroupManager", "EditGroup") | ("ownerGroupManager", "UpdateGroup") => {
            Some(handle_edit_group(ctx, session, args))
        }
        ("ownerGroupManager", "GetGroup") => Some(handle_get_group(ctx, session, args)),
        ("ownerGroupManager", "GetMembers") | ("ownerGroupManager", "GetGroupMembers") => {
            Some(handle_get_members(ctx, session, args))
        }
        _ => None,
    }
}

const DEFAULT_STRUCTURE_SERVICE_TAX_BPS: i64 = 100;

fn parse_struct_svc_args(session: &SessionState, args: &[PyRep], m: &str) -> Result<(i64, i64), BridgeRpcError> {
    if args.len() >= 2 {
        Ok((arg_i64(args, 0).ok_or_else(|| err("MissingStructureId", m))?, arg_i64(args, 1).ok_or_else(|| err("MissingServiceId", m))?))
    } else if args.len() == 1 {
        Ok((session.structure_id.ok_or_else(|| err("MissingStructureId", m))?, arg_i64(args, 0).ok_or_else(|| err("MissingServiceId", m))?))
    } else {
        Err(err("MissingArguments", m))
    }
}

pub(crate) fn handle_character_get_services(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "CharacterGetServices requires structureID"))?;
    ops::sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !life::is_operational(&st.state) || !ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(ctx.now_unix())) {
            return Ok(PyRep::Dict(Vec::new()));
        }
        let mut entries = Vec::new();
        for tid in life::online_service_type_ids(ctx, snap, structure_id) {
            if let Some(svc_id) = ops::service_id_for_module_type(tid) {
                if !entries.iter().any(|(k, _)| k == &PyRep::Integer(svc_id)) {
                    if let Some(bps) = crate::services::structure_profile_services::evaluate_character_service_bps(
                        ctx, snap, structure_id, char_id, svc_id, DEFAULT_STRUCTURE_SERVICE_TAX_BPS,
                    ) {
                        entries.push((PyRep::Integer(svc_id), PyRep::Real(bps as f64 / 10_000.0)));
                    }
                }
            }
        }
        Ok(PyRep::Dict(entries))
    })
}

pub(crate) fn handle_character_has_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterHasService")?;
    ops::sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        Ok(PyRep::Bool(
            crate::services::structure_profile_services::evaluate_character_service_bps(
                ctx, snap, structure_id, char_id, service_id, DEFAULT_STRUCTURE_SERVICE_TAX_BPS,
            )
            .is_some(),
        ))
    })
}

pub(crate) fn handle_character_check_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterCheckService")?;
    ops::sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        if !snap.structures.contains_key(&structure_id) {
            return Err(err("StructureNotFound", format!("Structure {structure_id} not found")));
        }
        if crate::services::structure_profile_services::evaluate_character_service_bps(
            ctx, snap, structure_id, char_id, service_id, DEFAULT_STRUCTURE_SERVICE_TAX_BPS,
        )
        .is_none()
        {
            return Err(err("StructureServiceAccessDenied", "Character is denied structure service access"));
        }
        Ok(PyRep::None)
    })
}

pub(crate) fn handle_character_get_service(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, service_id) = parse_struct_svc_args(session, args, "CharacterGetService")?;
    ops::sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        Ok(crate::services::structure_profile_services::evaluate_character_service_bps(
            ctx, snap, structure_id, char_id, service_id, DEFAULT_STRUCTURE_SERVICE_TAX_BPS,
        )
        .map_or(PyRep::None, |bps| PyRep::Real(bps as f64 / 10_000.0)))
    })
}

pub(crate) fn handle_character_has_setting(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (structure_id, setting_id) = parse_struct_svc_args(session, args, "CharacterHasSetting")?;
    ops::sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        Ok(PyRep::Bool(crate::services::structure_profile_services::evaluate_character_setting(
            ctx, snap, structure_id, char_id, setting_id,
        )))
    })
}
