//! `structure_profiles_rpc` — V24 `structureProfiles` & `ownerGroupManager` RPC handlers
//! (`eve/client/script/ui/structure/structureSettings/controllers/{allProfilesController,slimProfileController,structureProfileController}.pyj`),
//! plus backwards-compatible `structureSettings` profile aliases.
#![forbid(unsafe_code)]

use super::structure_upwell_ops::{
    apply_entity_to_profile, arg_i64, err, load_ext_state, require_corp_profile_manager,
    require_structure_corp_manager, save_ext_state, sync_assigned_structures_for_profile,
    sync_assigned_structures_for_profile_with_snap, validate_and_classify_entity,
    StructureAccessProfile, StructureExtendedState, PROFILE_CHANGE_DELAY_SECS,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::structure_profile_services as svc_acl;
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("structureProfiles", "GetProfiles") => Some(handle_get_profiles(ctx, session)),
        ("structureProfiles", "CreateProfile") | ("structureSettings", "CreateProfile") => {
            Some(handle_create_profile(ctx, session, args))
        }
        ("structureProfiles", "SaveProfileSettings")
        | ("structureSettings", "SaveProfileSettings") => {
            Some(handle_save_profile_settings(ctx, session, args))
        }
        ("structureProfiles", "UpdateProfile") | ("structureSettings", "UpdateProfile") => {
            Some(handle_update_profile(ctx, session, args))
        }
        ("structureProfiles", "ChangeProfiles") | ("structureSettings", "ChangeProfiles") => {
            Some(handle_change_profiles(ctx, session, args))
        }
        ("structureProfiles", "DeleteProfile") => Some(handle_delete_profile(ctx, session, args)),
        ("structureProfiles", "DuplicateProfile") => {
            Some(handle_duplicate_profile(ctx, session, args))
        }
        ("structureProfiles", "SetDefaultProfile") => {
            Some(handle_set_default_profile(ctx, session, args))
        }
        ("structureProfiles", "GetProfileSettings") => {
            Some(handle_get_profile_settings(ctx, session, args))
        }
        ("ownerGroupManager", "Create") => {
            Some(svc_acl::handle_create_owner_group(ctx, session, args))
        }
        ("ownerGroupManager", "AddMembers") => {
            Some(svc_acl::apply_group_member_batch(ctx, session, args, true))
        }
        ("ownerGroupManager", "DeleteMembers") => {
            Some(svc_acl::apply_group_member_batch(ctx, session, args, false))
        }
        _ => None,
    }
}

pub(crate) fn handle_get_profiles(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rows = ctx.db.with_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        let ext = load_ext_state(snap);
        let mut profiles: Vec<&StructureAccessProfile> = ext
            .profiles_by_id
            .values()
            .filter(|p| p.owner_corp_id == corp_id)
            .collect();
        profiles.sort_by_key(|p| p.profile_id);
        profiles
            .into_iter()
            .map(|p| {
                PyRep::keyval([
                    ("profileID", PyRep::Integer(p.profile_id)),
                    ("name", PyRep::String(p.name.clone())),
                    ("description", PyRep::String(p.description.clone())),
                    ("isDefault", PyRep::Bool(p.is_default)),
                ])
            })
            .collect::<Vec<_>>()
    });
    Ok(PyRep::List(rows))
}

pub(crate) fn handle_create_profile(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let name = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("New Profile")
        .to_string();
    let description = args
        .get(1)
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        require_corp_profile_manager(snap, session, char_id, corp_id)?;
        let mut ext = load_ext_state(snap);
        let profile_id = ext.profiles_by_id.keys().copied().max().unwrap_or(70_000_000) + 1;
        ext.profiles_by_id.insert(
            profile_id,
            StructureAccessProfile {
                profile_id,
                owner_corp_id: corp_id,
                name,
                description,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
        Ok(PyRep::Integer(profile_id))
    })
}

pub(crate) fn handle_update_profile(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id =
        arg_i64(args, 0).ok_or_else(|| err("MissingProfileId", "UpdateProfile requires profileID"))?;
    let name = args.get(1).and_then(|a| a.as_str()).map(str::to_string);
    let desc = args.get(2).and_then(|a| a.as_str()).map(str::to_string);
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let owner_corp = ext
            .profiles_by_id
            .get(&profile_id)
            .map(|p| p.owner_corp_id)
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, owner_corp)?;
        if let Some(prof) = ext.profiles_by_id.get_mut(&profile_id) {
            if let Some(n) = name {
                prof.name = n;
            }
            if let Some(d) = desc {
                prof.description = d;
            }
        }
        sync_assigned_structures_for_profile(&mut ext, profile_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::Bool(true))
    })
}

pub(crate) fn handle_delete_profile(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id =
        arg_i64(args, 0).ok_or_else(|| err("MissingProfileId", "DeleteProfile requires profileID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let owner_corp = ext
            .profiles_by_id
            .get(&profile_id)
            .map(|p| p.owner_corp_id)
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, owner_corp)?;
        let in_use = ext.structure_profile_id.iter().any(|(&sid, &pid)| {
            pid == profile_id && snap.structures.get(&sid).is_some_and(|s| s.owner_corp_id == owner_corp)
        }) || ext.pending_profile_applications.iter().any(|(&sid, &(pid, _))| {
            pid == profile_id && snap.structures.get(&sid).is_some_and(|s| s.owner_corp_id == owner_corp)
        });
        if in_use {
            return Err(err(
                "ProfileInUse",
                format!("Profile {profile_id} is currently assigned to one or more structures"),
            ));
        }
        ext.structure_profile_id.retain(|_, pid| *pid != profile_id);
        ext.pending_profile_applications.retain(|_, (pid, _)| *pid != profile_id);
        ext.profiles_by_id.remove(&profile_id);
        save_ext_state(snap, &ext);
        Ok(PyRep::Bool(true))
    })
}

pub(crate) fn handle_duplicate_profile(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id = arg_i64(args, 0)
        .ok_or_else(|| err("MissingProfileId", "DuplicateProfile requires profileID"))?;
    let custom_name = args.get(1).and_then(|a| a.as_str()).map(str::to_string);
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let src = ext
            .profiles_by_id
            .get(&profile_id)
            .cloned()
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, src.owner_corp_id)?;
        let new_id = ext.profiles_by_id.keys().copied().max().unwrap_or(70_000_000) + 1;
        let mut dup = src;
        dup.profile_id = new_id;
        dup.is_default = false;
        dup.name = custom_name.unwrap_or_else(|| format!("{} (Copy)", dup.name));
        ext.profiles_by_id.insert(new_id, dup);
        save_ext_state(snap, &ext);
        Ok(PyRep::Integer(new_id))
    })
}

pub(crate) fn handle_set_default_profile(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id = arg_i64(args, 0)
        .ok_or_else(|| err("MissingProfileId", "SetDefaultProfile requires profileID"))?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let owner_corp = ext
            .profiles_by_id
            .get(&profile_id)
            .map(|p| p.owner_corp_id)
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, owner_corp)?;
        for prof in ext.profiles_by_id.values_mut() {
            if prof.owner_corp_id == owner_corp {
                prof.is_default = prof.profile_id == profile_id;
            }
        }
        save_ext_state(snap, &ext);
        Ok(PyRep::Bool(true))
    })
}

pub(crate) fn handle_get_profile_settings(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let profile_id = arg_i64(args, 0)
        .ok_or_else(|| err("MissingProfileId", "GetProfileSettings requires profileID"))?;
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let prof = ext
            .profiles_by_id
            .get(&profile_id)
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        let caller_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if prof.owner_corp_id != 0 && caller_corp != prof.owner_corp_id && !session.has_gm_role() {
            return Err(err(
                "StructureOwnershipViolation",
                "Caller does not belong to the owning corporation",
            ));
        }
        // V24 `structureProfileController.pyj:53` (`LoadGroups`) expects a mapping
        // `settingID -> list[KeyVal(groupID, value)]`.
        let mut by_setting: HashMap<i64, Vec<PyRep>> = HashMap::new();
        let mut docking_entries: Vec<PyRep> = prof
            .access_group_ids
            .iter()
            .copied()
            .map(|gid| {
                PyRep::keyval([
                    ("groupID", PyRep::Integer(gid)),
                    ("value", PyRep::Integer(1)),
                ])
            })
            .collect();
        if docking_entries.is_empty() {
            docking_entries.push(PyRep::keyval([
                ("groupID", PyRep::Integer(0)),
                ("value", PyRep::Integer(if prof.corp_only { 0 } else { 1 })),
            ]));
        }
        by_setting.insert(
            crate::services::structure_profile_services::SETTING_HOUSING_CAN_DOCK,
            docking_entries,
        );
        for (&gid, &flag) in &prof.general_settings {
            if matches!(gid, 3 | 4 | 17 | 20 | 23 | 24 | 26..=36) {
                by_setting.entry(gid).or_default().push(PyRep::keyval([
                    ("groupID", PyRep::Integer(0)),
                    ("value", PyRep::Integer(if flag { 1 } else { 0 })),
                ]));
            }
        }
        for (&sid, rows) in &prof.service_acl_rules {
            let entry = by_setting.entry(sid).or_default();
            for &(gid, bps) in rows {
                entry.push(PyRep::keyval([
                    ("groupID", PyRep::Integer(gid)),
                    ("value", PyRep::Real(bps as f64 / 10_000.0)),
                ]));
            }
        }
        for (&sid, &bps) in &prof.service_settings {
            if !prof.service_acl_rules.contains_key(&sid) {
                by_setting.entry(sid).or_default().push(PyRep::keyval([
                    ("groupID", PyRep::Integer(0)),
                    ("value", PyRep::Real(bps as f64 / 10_000.0)),
                ]));
            }
        }
        if !prof.defense_group_ids.is_empty() || !prof.defense_allowed_characters.is_empty() {
            let mut def_entries: Vec<PyRep> = prof
                .defense_group_ids
                .iter()
                .copied()
                .map(|gid| {
                    PyRep::keyval([
                        ("groupID", PyRep::Integer(gid)),
                        ("value", PyRep::Integer(1)),
                    ])
                })
                .collect();
            for &cid in &prof.defense_allowed_characters {
                def_entries.push(PyRep::keyval([
                    ("groupID", PyRep::Integer(cid)),
                    ("value", PyRep::Integer(1)),
                ]));
            }
            by_setting.insert(17, def_entries);
        }
        let mut sorted_keys: Vec<i64> = by_setting.keys().copied().collect();
        sorted_keys.sort_unstable();
        let dict_entries = sorted_keys
            .into_iter()
            .map(|sid| {
                (
                    PyRep::Integer(sid),
                    PyRep::List(by_setting.remove(&sid).unwrap_or_default()),
                )
            })
            .collect();
        Ok(PyRep::Dict(dict_entries))
    })
}

pub(crate) fn handle_change_profiles(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(xs) | PyRep::Tuple(xs)) => xs.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if structure_ids.is_empty() {
        return Err(err(
            "MissingStructureId",
            "ChangeProfiles requires structureIDs",
        ));
    }
    let profile_id =
        arg_i64(args, 1).ok_or_else(|| err("MissingProfileId", "ChangeProfiles requires profileID"))?;
    let (owner_corp, sys_ids, released) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext
            .profiles_by_id
            .get(&profile_id)
            .cloned()
            .ok_or_else(|| err("ProfileNotFound", format!("Profile {profile_id} not found")))?;
        require_corp_profile_manager(snap, session, char_id, prof.owner_corp_id)?;
        let mut sys_ids = Vec::new();
        for &sid in &structure_ids {
            let st = require_structure_corp_manager(snap, session, char_id, sid)?;
            if prof.owner_corp_id != 0 && st.owner_corp_id != prof.owner_corp_id {
                return Err(err(
                    "StructureOwnershipViolation",
                    "Profile and structure must belong to the same corporation",
                ));
            }
            sys_ids.push(st.solar_system_id);
        }
        let now_unix = ctx.now_unix();
        let mut immediate_defense_updates = Vec::new();
        for &sid in &structure_ids {
            if ext.structure_profile_id.contains_key(&sid) {
                ext.pending_profile_applications
                    .insert(sid, (profile_id, now_unix + PROFILE_CHANGE_DELAY_SECS));
            } else {
                ext.structure_profile_id.insert(sid, profile_id);
                ext.access_profiles.insert(sid, prof.clone());
                if let Some(ds) = prof.defense_settings.clone() {
                    immediate_defense_updates.push((sid, ds));
                }
            }
        }
        save_ext_state(snap, &ext);
        for (sid, mut ds) in immediate_defense_updates {
            let existing =
                crate::services::structure_acl_defense_rules::load_defense_settings(snap, sid);
            ds.structure_id = sid;
            if ds.known_attackers.is_empty() {
                ds.known_attackers = existing.known_attackers;
                ds.known_attacker_timestamps = existing.known_attacker_timestamps;
            }
            crate::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        }
        let released =
            crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(
                snap,
            );
        Ok((prof.owner_corp_id, sys_ids, released))
    })?;
    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    ctx.notify(
        NotificationTarget::Corporation(owner_corp),
        "OnProfileSettingsChange",
        "corpid",
        vec![PyRep::List(
            structure_ids.into_iter().map(PyRep::Integer).collect(),
        )],
    );
    for sys_id in sys_ids {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnDockingAccessChangedForCurrentSolarSystem",
            "solarsystemid",
            vec![],
        );
    }
    Ok(PyRep::Bool(true))
}

fn parse_setting_scalar(
    sid: i64,
    val: &PyRep,
    service_taxes: &mut Vec<(i64, f64)>,
    general_settings: &mut Vec<(i64, bool)>,
) {
    match val {
        PyRep::Real(rate) => service_taxes.push((sid, *rate)),
        PyRep::Bool(flag) => general_settings.push((sid, *flag)),
        PyRep::Integer(v) => {
            if sid <= 2 || sid == 17 || sid == 19 {
                general_settings.push((sid, *v != 0));
            } else {
                service_taxes.push((sid, *v as f64));
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct DefensePolicyPatch {
    auto_enabled: Option<bool>,
    auto_attackers: Option<bool>,
    auto_war: Option<bool>,
    standing_thresh: Option<f64>,
    target_blocked: Option<bool>,
}

impl DefensePolicyPatch {
    fn is_any(&self) -> bool {
        self.auto_enabled.is_some()
            || self.auto_attackers.is_some()
            || self.auto_war.is_some()
            || self.standing_thresh.is_some()
            || self.target_blocked.is_some()
    }
}

fn validate_defense_standing_threshold(v: f64) -> Result<f64, BridgeRpcError> {
    if !v.is_finite() || !(-10.0..=10.0).contains(&v) {
        return Err(err(
            "InvalidDefenseStandingThreshold",
            "Defense standing threshold must be a finite number in [-10.0, 10.0]",
        ));
    }
    Ok(v)
}

// DERIVED: V24 `structureSettingsWnd.pyj` (`SaveProfile`) sends `(settingID, value, groupID)` tuples
// where `NO_GROUP_ID = 0` (`ownergroups/ownergroupConst.pyj`), `SETTING_DEFENSE_CAN_CONTROL_STRUCTURE = 17`,
// and `CATEGORY_HULL_DEFENSE = 1003` (`structures/settings.pyj`). Under `groupID == 0` for `17` or `1003`,
// accepts `(auto_defense_enabled, auto_target_attackers, auto_target_war_targets, standing_threshold, target_blocked_acl)`
// (or scalar `bool` / `f64`) so `feat:structures.defense-settings` is configurable via `SaveProfileSettings`
// without inventing non-client string dict keys.
fn try_parse_defense_policy_value(
    val: &PyRep,
    patch: &mut DefensePolicyPatch,
) -> Result<bool, BridgeRpcError> {
    match val {
        PyRep::Bool(b) => {
            patch.auto_enabled = Some(*b);
            Ok(true)
        }
        PyRep::Real(r) => {
            patch.standing_thresh = Some(validate_defense_standing_threshold(*r)?);
            Ok(true)
        }
        PyRep::Tuple(xs) | PyRep::List(xs) if matches!(xs.first(), Some(PyRep::Bool(_))) => {
            if let Some(b0) = xs.first().and_then(PyRep::as_bool) {
                patch.auto_enabled = Some(b0);
            }
            if let Some(x1) = xs.get(1).filter(|x| !matches!(x, PyRep::None)) {
                let b1 = x1.as_bool().ok_or_else(|| {
                    err("InvalidDefenseSettings", "auto_target_attackers must be a boolean")
                })?;
                patch.auto_attackers = Some(b1);
            }
            if let Some(x2) = xs.get(2).filter(|x| !matches!(x, PyRep::None)) {
                let b2 = x2.as_bool().ok_or_else(|| {
                    err("InvalidDefenseSettings", "auto_target_war_targets must be a boolean")
                })?;
                patch.auto_war = Some(b2);
            }
            if let Some(x3) = xs.get(3).filter(|x| !matches!(x, PyRep::None)) {
                let r3 = x3.as_f64().ok_or_else(|| {
                    err("InvalidDefenseStandingThreshold", "standing_threshold must be numeric")
                })?;
                patch.standing_thresh = Some(validate_defense_standing_threshold(r3)?);
            }
            if let Some(x4) = xs.get(4).filter(|x| !matches!(x, PyRep::None)) {
                let b4 = x4.as_bool().ok_or_else(|| {
                    err("InvalidDefenseSettings", "target_blocked_acl must be a boolean")
                })?;
                patch.target_blocked = Some(b4);
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn resolve_3tuple_gid_and_val<'a>(
    snap: &WorldStateSnapshot,
    ext: &StructureExtendedState,
    a: &'a PyRep,
    b: &'a PyRep,
) -> (i64, &'a PyRep) {
    match (a, b) {
        (PyRep::Integer(ia), PyRep::Integer(ib)) => {
            let is_known = |id: i64| {
                ext.owner_groups.contains_key(&id)
                    || snap.characters.contains_key(&id)
                    || snap.corporations.contains_key(&id)
                    || snap.alliances.contains_key(&id)
            };
            if *ia == 0 {
                (0, b)
            } else if is_known(*ia) {
                (*ia, b)
            } else if is_known(*ib) {
                (*ib, a)
            } else if *ib == 0 && (-1..=1).contains(ia) {
                (0, a)
            } else if (-1..=1).contains(ib) && !(-1..=1).contains(ia) {
                (*ia, b)
            } else if (-1..=1).contains(ia) && !(-1..=1).contains(ib) {
                (*ib, a)
            } else {
                (*ia, b)
            }
        }
        (_, PyRep::Integer(ib)) => (*ib, a),
        (PyRep::Integer(ia), _) => (*ia, b),
        _ => (0, b),
    }
}

fn push_service_acl_row(
    sid: i64,
    gid: i64,
    val: &PyRep,
    service_acl_updates: &mut HashMap<i64, Vec<(i64, f64)>>,
    service_taxes: &mut Vec<(i64, f64)>,
    general_settings: &mut Vec<(i64, bool)>,
) {
    if gid > 0 {
        let rows = service_acl_updates.entry(sid).or_default();
        match val {
            PyRep::Real(r) => rows.push((gid, *r)),
            PyRep::Integer(i) => rows.push((gid, *i as f64)),
            PyRep::Bool(true) => rows.push((gid, 0.0)),
            PyRep::Bool(false) => {}
            _ => rows.push((gid, f64::NAN)),
        }
    } else {
        parse_setting_scalar(sid, val, service_taxes, general_settings);
    }
}

pub(crate) fn handle_save_profile_settings(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let target_id = arg_i64(args, 0)
        .or(session.structure_id)
        .ok_or_else(|| err("MissingProfileId", "SaveProfileSettings requires profileID"))?;
    let (_, char_id) = ServerContext::require_character(session)?;
    let payload = args.get(1).or_else(|| args.get(2));
    let released = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let is_standalone_profile = ext.profiles_by_id.contains_key(&target_id);
        let owner_corp = if is_standalone_profile {
            let corp = ext
                .profiles_by_id
                .get(&target_id)
                .map(|p| p.owner_corp_id)
                .unwrap_or(0);
            require_corp_profile_manager(snap, session, char_id, corp)?;
            corp
        } else {
            require_structure_corp_manager(snap, session, char_id, target_id)?.owner_corp_id
        };
        let mut corp_only = None;
        let mut service_taxes = Vec::new();
        let mut service_acl_updates: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
        let mut general_settings = Vec::new();
        let mut blocked = Vec::new();
        let mut allowed = Vec::new();
        let mut group_ids = Vec::new();
        let mut raw_docking_1_ids: Vec<(i64, bool)> = Vec::new();
        let mut raw_defense_17_ids: Vec<(i64, bool)> = Vec::new();
        let mut defense_group_ids = Vec::new();
        let mut defense_allowed = Vec::new();
        let mut defense_blocked = Vec::new();
        let mut has_docking_groups = false;
        let mut has_blocked = false;
        let mut has_allowed = false;
        let mut has_setting_17 = false;
        let mut has_defense_groups = false;
        let mut has_defense_allowed = false;
        let mut has_defense_blocked = false;
        let mut def_patch = DefensePolicyPatch::default();
        let is_def_sid = |sid: i64| {
            sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE
                || sid == svc_acl::CATEGORY_HULL_DEFENSE
        };
        if let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload {
            for r in rows {
                if let PyRep::Tuple(items) | PyRep::List(items) = r {
                    if let Some(sid) = items.first().and_then(|x| x.as_i64()) {
                        if items.len() >= 3 {
                            let (gid, v2) =
                                resolve_3tuple_gid_and_val(snap, &ext, &items[1], &items[2]);
                            if is_def_sid(sid) {
                                if gid > 0 && sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                    let is_blk = matches!(v2, PyRep::Bool(false))
                                        || v2.as_i64().is_some_and(|x| x < 0);
                                    raw_defense_17_ids.push((gid, is_blk));
                                } else if gid == svc_acl::NO_GROUP_ID {
                                    if try_parse_defense_policy_value(v2, &mut def_patch)? {
                                        if let PyRep::Bool(flag) = v2 {
                                            if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                                general_settings.push((17, *flag));
                                            }
                                        }
                                    } else if let Some(iv) = v2.as_i64() {
                                        def_patch.auto_enabled = Some(iv != 0);
                                        if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                            general_settings.push((17, iv != 0));
                                        }
                                    }
                                }
                            } else if sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK {
                                if gid > 0 {
                                    let is_blk = matches!(v2, PyRep::Bool(false))
                                        || v2.as_i64().is_some_and(|x| x < 0);
                                    raw_docking_1_ids.push((gid, is_blk));
                                } else if let Some(b) = v2.as_bool() {
                                    corp_only = Some(!b);
                                    general_settings.push((1, b));
                                } else if let Some(iv) = v2.as_i64() {
                                    corp_only = Some(iv == 0);
                                    general_settings.push((1, iv != 0));
                                }
                                if sid == 1 {
                                    if let PyRep::Real(rate) = v2 {
                                        service_taxes.push((1, *rate));
                                    }
                                }
                            } else {
                                push_service_acl_row(
                                    sid,
                                    gid,
                                    v2,
                                    &mut service_acl_updates,
                                    &mut service_taxes,
                                    &mut general_settings,
                                );
                            }
                        } else if let Some(val) = items.get(1) {
                            if is_def_sid(sid) {
                                if try_parse_defense_policy_value(val, &mut def_patch)? {
                                    if let PyRep::Bool(flag) = val {
                                        if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                            general_settings.push((17, *flag));
                                        }
                                    }
                                } else if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                    match val {
                                        PyRep::List(xs) | PyRep::Tuple(xs) => {
                                            has_setting_17 = true;
                                            for x in xs {
                                                if let Some(v) = x.as_i64().filter(|&v| v > 0) {
                                                    raw_defense_17_ids.push((v, false));
                                                }
                                            }
                                        }
                                        PyRep::Integer(v) if *v > 0 => {
                                            has_setting_17 = true;
                                            raw_defense_17_ids.push((*v, false));
                                        }
                                        PyRep::Integer(0) => {
                                            def_patch.auto_enabled = Some(false);
                                            general_settings.push((17, false));
                                        }
                                        _ => {}
                                    }
                                }
                            } else if sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK {
                                match val {
                                    PyRep::List(xs) | PyRep::Tuple(xs) => {
                                        has_docking_groups = true;
                                        for x in xs {
                                            if let Some(v) = x.as_i64().filter(|&v| v > 0) {
                                                raw_docking_1_ids.push((v, false));
                                            }
                                        }
                                    }
                                    PyRep::Bool(flag) => {
                                        corp_only = Some(!*flag);
                                        general_settings.push((1, *flag));
                                    }
                                    PyRep::Integer(v) if *v == 0 || *v == 1 => {
                                        corp_only = Some(*v == 0);
                                        general_settings.push((1, *v != 0));
                                    }
                                    PyRep::Integer(v) if *v > 1 => {
                                        raw_docking_1_ids.push((*v, false));
                                    }
                                    PyRep::Real(rate) if sid == 1 => service_taxes.push((1, *rate)),
                                    _ => {}
                                }
                            } else {
                                match val {
                                    PyRep::Real(rate) => service_taxes.push((sid, *rate)),
                                    PyRep::Bool(flag) => general_settings.push((sid, *flag)),
                                    PyRep::Integer(v) => service_taxes.push((sid, *v as f64)),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(PyRep::Dict(d)) = payload {
            for (k, v) in d {
                if let Some(sid) = k.as_i64() {
                    if is_def_sid(sid) && try_parse_defense_policy_value(v, &mut def_patch)? {
                        if let PyRep::Bool(flag) = v {
                            if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                general_settings.push((17, *flag));
                            }
                        }
                        continue;
                    }
                    match v {
                        PyRep::List(entries) | PyRep::Tuple(entries) => {
                            if (sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK) && entries.is_empty() {
                                has_docking_groups = true;
                            } else if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE && entries.is_empty() {
                                has_setting_17 = true;
                            } else if sid != 1
                                && sid != svc_acl::SETTING_HOUSING_CAN_DOCK
                                && !is_def_sid(sid)
                                && entries.is_empty()
                            {
                                service_acl_updates.entry(sid).or_default();
                            }
                            for entry in entries {
                                if let Some(id_val) = entry.as_i64() {
                                    if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE && id_val > 0 {
                                        has_setting_17 = true;
                                        raw_defense_17_ids.push((id_val, false));
                                    } else if (sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK) && id_val > 0 {
                                        raw_docking_1_ids.push((id_val, false));
                                    } else if !is_def_sid(sid) && id_val >= 0 {
                                        service_acl_updates.entry(sid).or_default().push((id_val, 0.0));
                                    }
                                    continue;
                                }
                                if let PyRep::Tuple(titems) | PyRep::List(titems) = entry {
                                    if titems.len() >= 2 {
                                        let (gid, val) =
                                            resolve_3tuple_gid_and_val(snap, &ext, &titems[0], &titems[1]);
                                        if is_def_sid(sid) {
                                            if gid > 0 && sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                                let is_blk = matches!(val, PyRep::Bool(false))
                                                    || val.as_i64().is_some_and(|x| x < 0);
                                                raw_defense_17_ids.push((gid, is_blk));
                                            } else if gid == svc_acl::NO_GROUP_ID {
                                                let _ = try_parse_defense_policy_value(val, &mut def_patch)?;
                                            }
                                        } else {
                                            push_service_acl_row(
                                                sid,
                                                gid,
                                                val,
                                                &mut service_acl_updates,
                                                &mut service_taxes,
                                                &mut general_settings,
                                            );
                                        }
                                        continue;
                                    }
                                }
                                let kv_pairs: Option<Vec<(String, PyRep)>> = match entry {
                                    PyRep::KeyVal(kv) => {
                                        Some(kv.iter().map(|(ek, ev)| (ek.clone(), ev.clone())).collect())
                                    }
                                    PyRep::Dict(ed) => Some(
                                        ed.iter()
                                            .filter_map(|(ek, ev)| {
                                                ek.as_str().map(|s| (s.to_string(), ev.clone()))
                                            })
                                            .collect(),
                                    ),
                                    _ => None,
                                };
                                if let Some(kv) = kv_pairs {
                                    let gid = kv
                                        .iter()
                                        .find(|(ek, _)| ek == "groupID")
                                        .and_then(|(_, ev)| ev.as_i64())
                                        .unwrap_or(0);
                                    if let Some((_, val)) = kv.iter().find(|(ek, _)| ek == "value") {
                                        if is_def_sid(sid) {
                                            if gid > 0 && sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE {
                                                let is_blk = matches!(val, PyRep::Bool(false))
                                                    || val.as_i64().is_some_and(|x| x < 0);
                                                raw_defense_17_ids.push((gid, is_blk));
                                            } else if gid == svc_acl::NO_GROUP_ID {
                                                let _ = try_parse_defense_policy_value(val, &mut def_patch)?;
                                            }
                                        } else if sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK {
                                            if gid > 0 {
                                                has_docking_groups = true;
                                                group_ids.push(gid);
                                            } else if let Some(b) = val.as_bool() {
                                                corp_only = Some(!b);
                                                general_settings.push((1, b));
                                            } else if let Some(iv) = val.as_i64() {
                                                corp_only = Some(iv == 0);
                                                general_settings.push((1, iv != 0));
                                            }
                                            if sid == 1 {
                                                if let PyRep::Real(rate) = val {
                                                    service_taxes.push((1, *rate));
                                                }
                                            }
                                        } else {
                                            push_service_acl_row(
                                                sid,
                                                gid,
                                                val,
                                                &mut service_acl_updates,
                                                &mut service_taxes,
                                                &mut general_settings,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        PyRep::Integer(iv)
                            if sid == svc_acl::SETTING_DEFENSE_CAN_CONTROL_STRUCTURE && *iv > 0 =>
                        {
                            has_setting_17 = true;
                            raw_defense_17_ids.push((*iv, false));
                        }
                        PyRep::Integer(iv)
                            if (sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK) && *iv > 1 =>
                        {
                            raw_docking_1_ids.push((*iv, false));
                        }
                        PyRep::Integer(iv)
                            if (sid == 1 || sid == svc_acl::SETTING_HOUSING_CAN_DOCK)
                                && (*iv == 0 || *iv == 1) =>
                        {
                            corp_only = Some(*iv == 0);
                            general_settings.push((1, *iv != 0));
                        }
                        other => {
                            parse_setting_scalar(sid, other, &mut service_taxes, &mut general_settings);
                        }
                    }
                }
            }
        }
        // Classify raw IDs under settingID == 17 against actual DB/state tables (`ext.owner_groups`
        // vs `validate_and_classify_entity`) instead of comparing against a numeric ID threshold.
        for (v, is_blk) in raw_defense_17_ids {
            if ext.owner_groups.contains_key(&v) {
                has_defense_groups = true;
                if !defense_group_ids.contains(&v) {
                    defense_group_ids.push(v);
                }
            } else {
                let (_, _, is_char) = validate_and_classify_entity(snap, v)?;
                if !is_char {
                    return Err(err(
                        "InvalidAccessEntity",
                        format!("Defense setting 17 entity {v} must be a known access group or character"),
                    ));
                }
                if is_blk {
                    has_defense_blocked = true;
                    if !defense_blocked.contains(&v) {
                        defense_blocked.push(v);
                    }
                } else {
                    has_defense_allowed = true;
                    if !defense_allowed.contains(&v) {
                        defense_allowed.push(v);
                    }
                }
            }
        }
        for (v, is_blk) in raw_docking_1_ids {
            if ext.owner_groups.contains_key(&v) {
                has_docking_groups = true;
                if !group_ids.contains(&v) {
                    group_ids.push(v);
                }
            } else {
                let _ = validate_and_classify_entity(snap, v)?;
                if is_blk {
                    has_blocked = true;
                    if !blocked.contains(&v) {
                        blocked.push(v);
                    }
                } else {
                    has_allowed = true;
                    if !allowed.contains(&v) {
                        allowed.push(v);
                    }
                }
            }
        }
        let mut parsed_taxes = Vec::with_capacity(service_taxes.len());
        for &(sid, tax) in &service_taxes {
            parsed_taxes.push((sid, svc_acl::encode_service_setting_bps(sid, tax)?));
        }
        let encoded_service_acls = svc_acl::validate_service_acl_groups(
            &ext,
            session,
            char_id,
            owner_corp,
            &service_acl_updates,
        )?;
        let classified_blocked: Vec<_> = blocked
            .iter()
            .map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?)))
            .collect::<Result<_, BridgeRpcError>>()?;
        let classified_allowed: Vec<_> = allowed
            .iter()
            .map(|&eid| Ok((eid, validate_and_classify_entity(snap, eid)?)))
            .collect::<Result<_, BridgeRpcError>>()?;
        for &cid in defense_allowed.iter().chain(&defense_blocked) {
            let (_, _, is_char) = validate_and_classify_entity(snap, cid)?;
            if !is_char {
                return Err(err(
                    "InvalidAccessEntity",
                    format!("Defense character ID {cid} must be a known character"),
                ));
            }
        }
        let mut group_members_to_sync = Vec::new();
        for &gid in group_ids.iter().chain(&defense_group_ids) {
            let grp = ext
                .owner_groups
                .get(&gid)
                .ok_or_else(|| err("AccessGroupNotFound", format!("Access group {gid} not found")))?;
            if grp.owner_corp_id != 0 && grp.owner_corp_id != owner_corp {
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
        let effective_docking_gids: Vec<i64> = if has_docking_groups {
            group_ids.clone()
        } else if is_standalone_profile {
            ext.profiles_by_id
                .get(&target_id)
                .map(|p| p.access_group_ids.iter().copied().collect())
                .unwrap_or_default()
        } else {
            ext.access_profiles
                .get(&target_id)
                .map(|p| p.access_group_ids.iter().copied().collect())
                .unwrap_or_default()
        };
        if has_docking_groups || has_allowed || has_blocked {
            for &gid in &effective_docking_gids {
                if let Some(grp) = ext.owner_groups.get(&gid) {
                    for (&eid, &mtype) in &grp.members {
                        if eid == 0 {
                            continue;
                        }
                        group_members_to_sync.push((
                            eid,
                            validate_and_classify_entity(snap, eid)?,
                            mtype < 0,
                        ));
                    }
                }
            }
        }
        let has_defense_flag_updates = def_patch.is_any();
        let existing_structure_ds = if !is_standalone_profile && has_defense_flag_updates {
            Some(crate::services::structure_acl_defense_rules::load_defense_settings(snap, target_id))
        } else {
            None
        };
        let prof_opt = if is_standalone_profile {
            ext.profiles_by_id.get_mut(&target_id)
        } else {
            Some(ext.access_profiles.entry(target_id).or_default())
        };
        if let Some(prof) = prof_opt {
            if let Some(co) = corp_only {
                prof.corp_only = co;
            }
            for (sid, bps) in parsed_taxes {
                prof.service_settings.insert(sid, bps);
            }
            svc_acl::apply_service_acls_to_profile(prof, encoded_service_acls);
            for &(gid, val) in &general_settings {
                prof.general_settings.insert(gid, val);
            }
            if has_docking_groups {
                prof.access_group_ids.clear();
                for &gid in &group_ids {
                    prof.access_group_ids.insert(gid);
                }
            }
            if has_blocked || has_docking_groups {
                prof.blocked_characters.clear();
                prof.blocked_corporations.clear();
                prof.blocked_alliances.clear();
            }
            if has_allowed || has_docking_groups {
                prof.allowed_characters.clear();
                prof.allowed_corporations.clear();
                prof.allowed_alliances.clear();
            }
            if has_docking_groups || has_allowed || has_blocked {
                for (eid, (is_ally, is_corp, _), is_blk) in group_members_to_sync {
                    if (is_blk && (has_blocked || has_docking_groups))
                        || (!is_blk && (has_allowed || has_docking_groups))
                    {
                        apply_entity_to_profile(prof, eid, is_ally, is_corp, is_blk, true);
                    }
                }
            }
            for (eid, (is_ally, is_corp, _)) in classified_blocked {
                apply_entity_to_profile(prof, eid, is_ally, is_corp, true, true);
            }
            for (eid, (is_ally, is_corp, _)) in classified_allowed {
                apply_entity_to_profile(prof, eid, is_ally, is_corp, false, true);
            }
            if has_setting_17 || has_defense_groups {
                prof.defense_group_ids.clear();
                for &gid in &defense_group_ids {
                    prof.defense_group_ids.insert(gid);
                }
            }
            if has_setting_17 || has_defense_allowed {
                prof.defense_allowed_characters.clear();
                for &cid in &defense_allowed {
                    prof.defense_allowed_characters.insert(cid);
                }
            }
            if has_defense_blocked {
                prof.defense_blocked_characters.clear();
                for &cid in &defense_blocked {
                    prof.defense_blocked_characters.insert(cid);
                }
            }
            if has_defense_flag_updates {
                let mut ds = prof
                    .defense_settings
                    .clone()
                    .or(existing_structure_ds)
                    .unwrap_or_default();
                if let Some(v) = def_patch.auto_enabled {
                    ds.auto_defense_enabled = v;
                }
                if let Some(v) = def_patch.auto_attackers {
                    ds.auto_target_attackers = v;
                }
                if let Some(v) = def_patch.auto_war {
                    ds.auto_target_war_targets = v;
                }
                if let Some(v) = def_patch.standing_thresh {
                    ds.standing_threshold = v;
                }
                if let Some(v) = def_patch.target_blocked {
                    ds.target_blocked_acl = v;
                }
                ds.known_attackers.clear();
                ds.known_attacker_timestamps.clear();
                prof.defense_settings = Some(ds);
            }
        }
        if is_standalone_profile {
            sync_assigned_structures_for_profile_with_snap(snap, &mut ext, target_id);
        } else if let Some(ds_tmpl) = ext
            .access_profiles
            .get(&target_id)
            .and_then(|p| p.defense_settings.clone())
        {
            let existing =
                crate::services::structure_acl_defense_rules::load_defense_settings(snap, target_id);
            let mut ds = ds_tmpl;
            ds.structure_id = target_id;
            ds.known_attackers = existing.known_attackers;
            ds.known_attacker_timestamps = existing.known_attacker_timestamps;
            crate::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        }
        save_ext_state(snap, &ext);
        Ok(
            crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(
                snap,
            ),
        )
    })?;
    crate::services::structure_control_ops::notify_released_structure_controllers(
        ctx, &released,
    );
    Ok(PyRep::Bool(true))
}
#[cfg(test)]
#[path = "structure_profiles_rpc_tests.rs"]
mod structure_profiles_rpc_tests;
