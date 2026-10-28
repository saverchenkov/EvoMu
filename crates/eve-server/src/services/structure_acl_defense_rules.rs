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

pub fn handle_set_member_role(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let group_id = arg_i64(args, 0).ok_or_else(|| err("MissingGroupId", "SetMemberRole requires groupID"))?;
    let entity_id = arg_i64(args, 1).ok_or_else(|| err("MissingEntityId", "SetMemberRole requires entityID"))?;
    let role = arg_i64(args, 2).unwrap_or(ACL_ROLE_MEMBER);
    if !matches!(role, ACL_ROLE_BLOCKED | ACL_ROLE_MEMBER | ACL_ROLE_ADMIN | ACL_ROLE_MANAGER) {
        return Err(err("InvalidAclRole", format!("Role {role} is not a valid ACL role (-1, 0, 1, 2)")));
    }

    let notify_corp = ctx.db.try_with_mut_snapshot(|snap| {
        let (is_ally, is_corp, _) = ops::validate_and_classify_entity(snap, entity_id)?;
        let mut ext = load_ext_state(snap);
        let grp = ext.owner_groups.get(&group_id).ok_or_else(|| err("AccessGroupNotFound", format!("Group {group_id} not found")))?;

        // Permission check: modifying admin/manager requires admin or corp director
        let is_admin_or_dir = is_acl_admin(char_id, grp)
            || (grp.owner_corp_id != 0 && ops::require_corp_profile_manager(snap, session, char_id, grp.owner_corp_id).is_ok())
            || session.has_gm_role();
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
        if !is_admin_or_dir && !is_acl_manager_or_admin(char_id, grp) {
            return Err(err("InsufficientCorpRoles", "Caller lacks manager permissions in this access group"));
        }

        let owner_corp = grp.owner_corp_id;
        if let Some(grp_mut) = ext.owner_groups.get_mut(&group_id) {
            grp_mut.members.insert(entity_id, role);
        }

        // Immediately sync to all linked profiles and structures
        let is_blocked = role == ACL_ROLE_BLOCKED;
        let linked_pids: Vec<i64> = ext
            .profiles_by_id
            .values()
            .filter(|p| p.access_group_ids.contains(&group_id))
            .map(|p| p.profile_id)
            .collect();

        for pid in linked_pids {
            if let Some(prof) = ext.profiles_by_id.get_mut(&pid) {
                ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, is_blocked, true);
                if !is_blocked {
                    ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, true, false);
                } else {
                    ops::apply_entity_to_profile(prof, entity_id, is_ally, is_corp, false, false);
                }
            }
            ops::sync_assigned_structures_for_profile(&mut ext, pid);
        }

        save_ext_state(snap, &ext);
        Ok(owner_corp)
    })?;

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
// 3. Board Structure & Eject (`feat:structures.board-structure`)
// ============================================================================

pub fn handle_board_structure(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "BoardStructure requires structureID"))?;

    ctx.db.try_with_mut_snapshot(|snap| {
        let st = ops::require_structure_corp_member(snap, char_id, structure_id)?;
        ops::require_structure_control_role(snap, char_id, &st)?;
        if session.structure_id != Some(structure_id)
            && session.solar_system_id != Some(st.solar_system_id)
        {
            return Err(err(
                "NotDockedInStructure",
                format!(
                    "Must be docked in structure {structure_id} (or in system {}) to board it",
                    st.solar_system_id
                ),
            ));
        }
        let mut ext = load_ext_state(snap);
        if let Some(&pilot) = ext.active_pilot_by_structure.get(&structure_id) {
            if pilot != char_id {
                return Err(err(
                    "StructureAlreadyControlled",
                    format!("Structure {structure_id} is already controlled by pilot {pilot}"),
                ));
            }
        }
        ext.active_pilot_by_structure.insert(structure_id, char_id);
        save_ext_state(snap, &ext);
        Ok(())
    })?;

    session.ship_id = Some(structure_id);
    session.push_notification(
        "OnSessionChanged",
        vec![PyRep::keyval([("shipID", PyRep::Integer(structure_id))])],
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![PyRep::keyval([("shipID", PyRep::Integer(structure_id))])],
    );
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("pilotID", PyRep::Integer(char_id)),
        ("boarded", PyRep::Bool(true)),
    ]))
}

pub fn handle_eject_from_structure(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "EjectFromStructure requires structureID"))?;

    let orig_ship_id = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err(
                "NotControllingStructure",
                format!("Character {char_id} is not boarding/controlling structure {structure_id}"),
            ));
        }
        ext.active_pilot_by_structure.remove(&structure_id);
        save_ext_state(snap, &ext);
        let fallback_ship = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == structure_id
                    && i.item_id != structure_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
            })
            .map(|i| i.item_id)
            .unwrap_or(0);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            if ch.active_ship_id == structure_id || ch.active_ship_id <= 0 {
                ch.active_ship_id = fallback_ship;
            }
            Ok(ch.active_ship_id)
        } else {
            Ok(0)
        }
    })?;

    if orig_ship_id > 0 {
        session.ship_id = Some(orig_ship_id);
        session.push_notification(
            "OnSessionChanged",
            vec![PyRep::keyval([("shipID", PyRep::Integer(orig_ship_id))])],
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnSessionChanged",
            "charid",
            vec![PyRep::keyval([("shipID", PyRep::Integer(orig_ship_id))])],
        );
    } else if session.ship_id == Some(structure_id) {
        session.ship_id = None;
    }
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("pilotID", PyRep::None),
        ("boarded", PyRep::Bool(false)),
    ]))
}

pub fn handle_get_structure_pilot(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "GetStructurePilot requires structureID"))?;
    Ok(ctx.db.with_snapshot(|snap| {
        load_ext_state(snap)
            .active_pilot_by_structure
            .get(&structure_id)
            .copied()
            .map(PyRep::Integer)
            .unwrap_or(PyRep::None)
    }))
}

pub fn handle_release_control(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "ReleaseControl requires structureID"))?;
    let target_id = arg_i64(args, 1).unwrap_or(0);
    let orig_ship_id = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        if ext.active_pilot_by_structure.get(&structure_id) != Some(&char_id) {
            return Err(err(
                "NotControllingStructure",
                format!("Character {char_id} does not control structure {structure_id}"),
            ));
        }
        ext.active_pilot_by_structure.remove(&structure_id);
        save_ext_state(snap, &ext);
        let fallback_ship = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == structure_id
                    && i.item_id != structure_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
            })
            .map(|i| i.item_id)
            .unwrap_or(0);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            if ch.active_ship_id == structure_id || ch.active_ship_id <= 0 {
                ch.active_ship_id = fallback_ship;
            }
            Ok(ch.active_ship_id)
        } else {
            Ok(0)
        }
    })?;
    if orig_ship_id > 0 {
        session.ship_id = Some(orig_ship_id);
        session.push_notification(
            "OnSessionChanged",
            vec![PyRep::keyval([("shipID", PyRep::Integer(orig_ship_id))])],
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnSessionChanged",
            "charid",
            vec![PyRep::keyval([("shipID", PyRep::Integer(orig_ship_id))])],
        );
    } else if session.ship_id == Some(structure_id) {
        session.ship_id = None;
    }
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnStructureTargetRemoved",
        "charid",
        vec![PyRep::Integer(structure_id), PyRep::Integer(target_id)],
    );
    Ok(PyRep::None)
}

// ============================================================================
// 4. Structure Auto-Defense Settings (`feat:structures.defense-settings`)
// ============================================================================

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
}

impl Default for StructureDefenseSettings {
    fn default() -> Self {
        Self {
            structure_id: 0,
            auto_defense_enabled: true,
            auto_target_attackers: true,
            auto_target_war_targets: true,
            standing_threshold: 0.0,
            target_blocked_acl: true,
            known_attackers: Vec::new(),
        }
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
    let key = defense_blob_key(settings.structure_id);
    if let Ok(json) = serde_json::to_string(settings) {
        snap.service_state_blobs.insert(key, json);
    }
}

pub fn evaluate_auto_defense_target(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    pilot_id: i64,
) -> bool {
    let settings = load_defense_settings(snap, structure_id);
    if !settings.auto_defense_enabled {
        return false;
    }

    let Some(st) = snap.structures.get(&structure_id) else {
        return false;
    };
    let (pilot_corp, pilot_ally) = snap
        .characters
        .get(&pilot_id)
        .map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0)))
        .unwrap_or((0, 0));

    // Friendly exemption: pilots belonging to owning corp or alliance are never auto-targeted
    if pilot_corp == st.owner_corp_id {
        return false;
    }
    if let Some(owner_corp) = snap.corporations.get(&st.owner_corp_id) {
        if let Some(owner_ally) = owner_corp.alliance_id {
            if owner_ally > 0 && pilot_ally == owner_ally {
                return false;
            }
        }
    }

    // 1. Attackers targeting: check known attackers
    if settings.auto_target_attackers && settings.known_attackers.contains(&pilot_id) {
        return true;
    }

    // 2. War targets: check active war between pilot's corp/alliance and structure owner
    if settings.auto_target_war_targets {
        let now = ctx.now_unix();
        let at_war = snap.wars.values().any(|w| {
            w.active
                && now >= w.started_unix.saturating_add(86_400)
                && ((w.aggressor_corp_id == pilot_corp && w.defender_corp_id == st.owner_corp_id)
                    || (w.aggressor_corp_id == st.owner_corp_id && w.defender_corp_id == pilot_corp))
        });
        if at_war {
            return true;
        }
    }

    // 3. Standing threshold check: standing between owner corp and pilot
    let standing = snap
        .characters
        .get(&pilot_id)
        .and_then(|c| c.standings.get(&st.owner_corp_id).copied())
        .unwrap_or(0.0);
    if standing < settings.standing_threshold {
        return true;
    }

    // 4. Blocked ACL check: pilots denied by structure access profile are targeted
    if settings.target_blocked_acl && !ops::evaluate_character_structure_access(ctx, snap, structure_id, pilot_id) {
        return true;
    }

    false
}

pub fn handle_get_defense_settings(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "GetDefenseSettings requires structureID"))?;

    ctx.db.with_snapshot(|snap| {
        let st = ops::require_structure_corp_member(snap, char_id, structure_id)?;
        ops::require_structure_control_role(snap, char_id, &st)?;
        let s = load_defense_settings(snap, structure_id);
        let attackers: Vec<PyRep> = s.known_attackers.iter().map(|&a| PyRep::Integer(a)).collect();
        Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(s.structure_id)),
            ("autoDefenseEnabled", PyRep::Bool(s.auto_defense_enabled)),
            ("autoTargetAttackers", PyRep::Bool(s.auto_target_attackers)),
            ("autoTargetWarTargets", PyRep::Bool(s.auto_target_war_targets)),
            ("standingThreshold", PyRep::Real(s.standing_threshold)),
            ("targetBlockedAcl", PyRep::Bool(s.target_blocked_acl)),
            ("knownAttackers", PyRep::List(attackers)),
        ]))
    })
}

pub fn handle_set_defense_settings(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "SetDefenseSettings requires structureID"))?;

    let mut auto_defense_enabled = None;
    let mut auto_target_attackers = None;
    let mut auto_target_war_targets = None;
    let mut standing_threshold = None;
    let mut target_blocked_acl = None;
    let mut add_attacker = None;

    if let Some(PyRep::Dict(entries)) = args.get(1) {
        for (k, v) in entries {
            if let Some(key) = k.as_str() {
                match key {
                    "autoDefenseEnabled" => auto_defense_enabled = v.as_bool(),
                    "autoTargetAttackers" => auto_target_attackers = v.as_bool(),
                    "autoTargetWarTargets" => auto_target_war_targets = v.as_bool(),
                    "standingThreshold" => standing_threshold = v.as_f64(),
                    "targetBlockedAcl" => target_blocked_acl = v.as_bool(),
                    "addAttacker" | "attackerID" => add_attacker = v.as_i64(),
                    _ => {}
                }
            }
        }
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let st = ops::require_structure_corp_member(snap, char_id, structure_id)?;
        ops::require_structure_control_role(snap, char_id, &st)?;
        let mut s = load_defense_settings(snap, structure_id);
        if let Some(val) = auto_defense_enabled {
            s.auto_defense_enabled = val;
        }
        if let Some(val) = auto_target_attackers {
            s.auto_target_attackers = val;
        }
        if let Some(val) = auto_target_war_targets {
            s.auto_target_war_targets = val;
        }
        if let Some(val) = standing_threshold {
            s.standing_threshold = val;
        }
        if let Some(val) = target_blocked_acl {
            s.target_blocked_acl = val;
        }
        if let Some(att) = add_attacker {
            if !s.known_attackers.contains(&att) {
                s.known_attackers.push(att);
            }
        }
        save_defense_settings(snap, &s);
        Ok(PyRep::Bool(true))
    })
}

pub fn handle_evaluate_defense_target(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let structure_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingStructureId", "EvaluateDefenseTarget requires structureID"))?;
    let target_pilot_id = arg_i64(args, 1)
        .ok_or_else(|| err("MissingTargetId", "EvaluateDefenseTarget requires targetPilotID"))?;

    let is_target = ctx.db.with_snapshot(|snap| {
        evaluate_auto_defense_target(ctx, snap, structure_id, target_pilot_id)
    });
    Ok(PyRep::Bool(is_target))
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
// RPC Dispatcher
// ============================================================================

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // rpc-lint: allow canonical ledger feat:structures.access-lists requires SetMemberRole
        ("ownerGroupManager", "SetMemberRole") => Some(handle_set_member_role(ctx, session, args)),
        ("ownerGroupManager", "EditGroup") | ("ownerGroupManager", "UpdateGroup") => {
            Some(handle_edit_group(ctx, session, args))
        }
        ("ownerGroupManager", "GetGroup") => Some(handle_get_group(ctx, session, args)),
        ("ownerGroupManager", "GetMembers") | ("ownerGroupManager", "GetGroupMembers") => {
            Some(handle_get_members(ctx, session, args))
        }
        ("structureControl", "BoardStructure") => Some(handle_board_structure(ctx, session, args)),
        ("structureControl", "EjectFromStructure") => Some(handle_eject_from_structure(ctx, session, args)),
        ("structureControl", "GetStructurePilot") => Some(handle_get_structure_pilot(ctx, session, args)),
        ("structureControl", "ReleaseControl") => Some(handle_release_control(ctx, session, args)),
        // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
        ("structureControl", "GetDefenseSettings") => Some(handle_get_defense_settings(ctx, session, args)),
        // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
        ("structureControl", "SetDefenseSettings") => Some(handle_set_defense_settings(ctx, session, args)),
        // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
        ("structureControl", "EvaluateDefenseTarget") => Some(handle_evaluate_defense_target(ctx, session, args)),
        _ => None,
    }
}
