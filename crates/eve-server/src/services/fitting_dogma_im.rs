//! `fitting_dogma_im` — `dogmaIM` inspection, fitting, targeting, weapon linking,ammo & `shipStanceMgr`.

#![forbid(unsafe_code)]

use super::*;

/// Resolves a player-facing entity/target name from custom item names, structures,
/// characters, or SDE item type names, falling back to `"Target"`.
pub(crate) fn resolve_target_display_name(ctx: &ServerContext, target_id: i64) -> String {
    let custom_name = ctx
        .esi_state
        .custom_item_names
        .read()
        .ok()
        .and_then(|m| m.get(&target_id).filter(|s| !s.trim().is_empty()).cloned());
    custom_name
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.structures
                    .get(&target_id)
                    .filter(|st| !st.name.trim().is_empty())
                    .map(|st| st.name.clone())
                    .or_else(|| {
                        snap.characters
                            .get(&target_id)
                            .filter(|c| !c.name.trim().is_empty())
                            .map(|c| c.name.clone())
                    })
                    .or_else(|| {
                        snap.items
                            .get(&target_id)
                            .and_then(|i| ctx.sde.types.get(&i.type_id).map(|t| t.name.clone()))
                    })
            })
        })
        .unwrap_or_else(|| "Target".to_string())
}

/// Builds a `ShipJammed` RPC error carrying V24 `UserError("TargetingSystemsBeingJammed")` (`#1082`).
pub(crate) fn ship_jammed_rpc_error(ctx: &ServerContext, target_id: i64) -> BridgeRpcError {
    let target_name = resolve_target_display_name(ctx, target_id);
    BridgeRpcError::with_client_error(
        "ShipJammed",
        "Targeting systems are jammed by ECM",
        eve_proto::ClientError::user_with_args(
            eve_proto::UserErrorKey::TargetingSystemsBeingJammed,
            [(
                "targetname",
                eve_proto::UserErrorArg::Str(target_name),
            )],
        ),
    )
}

/// Builds a `TargetingFailed` lock-cap error carrying exact V24 `UserError` (`#1082`):
/// `TargetingAptitudeAlreadyFullyUtilized` when character skill cap bottlenecks, or
/// `TargetingSystemsAlreadyFullyUtilized` when the ship hull/rig cap bottlenecks.
pub(crate) fn target_lock_cap_rpc_error(
    max_locked: usize,
    skill_bottleneck: bool,
) -> BridgeRpcError {
    let key = if skill_bottleneck {
        eve_proto::UserErrorKey::TargetingAptitudeAlreadyFullyUtilized
    } else {
        eve_proto::UserErrorKey::TargetingSystemsAlreadyFullyUtilized
    };
    BridgeRpcError::with_client_error(
        "TargetingFailed",
        format!("Max locked targets exceeded ({max_locked})"),
        eve_proto::ClientError::user_with_args(
            key,
            [("limit", eve_proto::UserErrorArg::Int(max_locked as i64))],
        ),
    )
}

fn is_npc_corporation_id(ctx: &ServerContext, corp_id: i64) -> bool {
    corp_id <= 0
        || (1_000_000..2_000_000).contains(&corp_id)
        || ctx.sde.npc_corporations.contains_key(&corp_id)
}

fn is_on_grid_visible_ball(
    ctx: &ServerContext,
    session: &SessionState,
    target_ball_id: i64,
    expected_sys_id: u32,
) -> bool {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return false;
    }
    let Some(caller_sys) = session.solar_system_id else {
        return false;
    };
    if caller_sys != expected_sys_id {
        return false;
    }
    let Some(caller_ship) = session.ship_id else {
        return false;
    };
    let Some(sys) = ctx.systems.get_handle(caller_sys) else {
        return false;
    };
    let view = sys.view();
    if view.is_ball_cloaked(target_ball_id) || view.is_gate_cloaked(target_ball_id) {
        return false;
    }
    let (Some(caller_pos), Some(target_pos)) = (
        view.ball_position(caller_ship),
        view.ball_position(target_ball_id),
    ) else {
        return false;
    };
    match (
        view.bubble_id_for_ball(caller_ship),
        view.bubble_id_for_ball(target_ball_id),
    ) {
        (Some(caller_bubble), Some(target_bubble)) => caller_bubble == target_bubble,
        _ => (caller_pos - target_pos).length() <= eve_destiny::BUBBLE_RADIUS_METERS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemInfoAccess {
    Full,
    OnGridPublicOnly,
}

fn is_gm_session(session: &SessionState) -> bool {
    session.has_gm_role() || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0
}

#[allow(clippy::too_many_arguments)]
fn is_structure_combat_gunnery_flag(flag: InventoryFlag, flag_u16: u16) -> bool {
    matches!(flag_u16, 11..=34 | 158..=163)
        || matches!(flag, InventoryFlag::SpecializedAmmoHold)
}

fn char_granted_by_defense_acl(
    snap: &eve_db::WorldStateSnapshot,
    ext: &crate::services::station_structure::structure_upwell_ops::StructureExtendedState,
    prof_opt: Option<&crate::services::station_structure::structure_upwell_ops::StructureAccessProfile>,
    char_id: i64,
) -> bool {
    let Some(p) = prof_opt else {
        return false;
    };
    let caller_corp = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let caller_ally = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.alliance_id)
        .or_else(|| snap.corporations.get(&caller_corp).and_then(|c| c.alliance_id))
        .unwrap_or(0);
    if p.blocked_characters.contains(&char_id)
        || p.defense_blocked_characters.contains(&char_id)
        || (caller_corp > 0 && p.blocked_corporations.contains(&caller_corp))
        || (caller_ally > 0 && p.blocked_alliances.contains(&caller_ally))
    {
        return false;
    }
    use crate::services::structure_acl_defense_rules::{is_acl_role_allowed, is_acl_role_excluded};
    if p.defense_group_ids.iter().any(|gid| {
        ext.owner_groups.get(gid).is_some_and(|grp| {
            grp.members.get(&char_id).copied().is_some_and(is_acl_role_excluded)
                || (caller_corp > 0 && grp.members.get(&caller_corp).copied().is_some_and(is_acl_role_excluded))
                || (caller_ally > 0 && grp.members.get(&caller_ally).copied().is_some_and(is_acl_role_excluded))
        })
    }) {
        return false;
    }
    p.defense_allowed_characters.contains(&char_id)
        || p.defense_group_ids.iter().any(|gid| {
            ext.owner_groups.get(gid).is_some_and(|g| {
                g.members.get(&0).copied().is_some_and(is_acl_role_allowed)
                    || g.members.get(&char_id).copied().is_some_and(is_acl_role_allowed)
                    || (caller_corp > 0 && g.members.get(&caller_corp).copied().is_some_and(is_acl_role_allowed))
                    || (caller_ally > 0 && g.members.get(&caller_ally).copied().is_some_and(is_acl_role_allowed))
            })
        })
}

fn can_gunner_access_structure_combat(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    st: &eve_db::UpwellStructureRecord,
    char_id: i64,
) -> bool {
    if st.owner_corp_id <= 0
        || is_npc_corporation_id(ctx, st.owner_corp_id)
        || !snap.corporations.contains_key(&st.owner_corp_id)
        || !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st)
    {
        return false;
    }
    let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
    let now_unix = ctx.now_unix();
    let prof_opt = crate::services::structure_profile_services::effective_profile_for_structure(
        &ext,
        st.structure_id,
        Some(now_unix),
    );
    if crate::services::structure_control_ops::verify_control_acl_or_role_with_profile_at(
        snap, &ext, st, prof_opt, char_id, now_unix,
    )
    .is_err()
    {
        return false;
    }
    // Fail closed unless `char_id` itself is granted by the defense ACL or is an
    // active owner-corp member (not excluded from `corp.members`) with CEO/control role.
    if !char_granted_by_defense_acl(snap, &ext, prof_opt, char_id) {
        const CTRL_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
            | eve_db::corp_roles::ROLE_STATION_MANAGER
            | eve_db::corp_roles::ROLE_STARBASE_OPERATOR
            | eve_db::corp_roles::ROLE_STARBASE_CONFIG;
        return crate::services::station_structure::structure_upwell_ops::has_db_structure_corp_role(
            ctx, snap, char_id, st, CTRL_ROLES, now_unix,
        );
    }
    true
}

fn authorize_corp_flag_access(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    owner_corp_id: i64,
    root_loc: i64,
    root_flag: InventoryFlag,
    root_flag_u16: u16,
) -> bool {
    let caller_corp = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .or(session.corporation_id)
        .unwrap_or(0);
    if caller_corp <= 0
        || owner_corp_id != caller_corp
        || is_npc_corporation_id(ctx, caller_corp)
    {
        return false;
    }
    let is_valid_member = snap.corporations.get(&caller_corp).is_some_and(|corp| {
        corp.ceo_id == char_id
            || corp.members.contains(&char_id)
            || (corp.members.is_empty()
                && (corp.member_roles.contains_key(&char_id)
                    || snap.corp_role_assignments.contains_key(&(caller_corp, char_id))))
    });
    if !is_valid_member {
        return false;
    }
    let (is_ceo, role_mask) = snap.effective_corp_roles_at(
        caller_corp,
        char_id,
        Some(root_loc),
        0,
        ctx.now_unix(),
    );
    if is_ceo
        || eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_DIRECTOR)
    {
        return true;
    }
    let flag_num = i64::from(root_flag_u16);
    if (116..=122).contains(&flag_num) {
        let div_idx = (flag_num - 116) as usize;
        let div_roles = eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[div_idx]
            | eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]
            | eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx];
        return (role_mask & div_roles) != 0;
    }
    if matches!(
        root_flag,
        InventoryFlag::Deliveries
            | InventoryFlag::CorpDeliveries
            | InventoryFlag::CapsuleerDeliveries
    ) {
        let deliveries_roles = eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_QUERY
            | eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE
            | eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE
            | eve_db::corp_roles::ROLE_FACTORY_MANAGER
            | eve_db::corp_roles::ROLE_TRADER;
        return (role_mask & deliveries_roles) != 0;
    }
    if let Some(st) = snap
        .structures
        .get(&root_loc)
        .filter(|st| st.owner_corp_id == caller_corp)
    {
        let is_structure_equipment_flag =
            matches!(flag_num, 11..=34 | 92..=94 | 158..=171 | 180)
                || matches!(
                    root_flag,
                    InventoryFlag::SpecializedAmmoHold | InventoryFlag::QuantumCoreRoom
                );
        if is_structure_equipment_flag {
            let equip_roles = eve_db::corp_roles::ROLE_STATION_MANAGER
                | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
            if (role_mask & equip_roles) != 0 {
                return true;
            }
        }
        if is_structure_combat_gunnery_flag(root_flag, root_flag_u16)
            && can_gunner_access_structure_combat(ctx, snap, st, char_id)
        {
            return true;
        }
        if matches!(
            root_flag,
            InventoryFlag::StructureFuel | InventoryFlag::SpecializedFuelBay
        ) {
            let fuel_roles = eve_db::corp_roles::ROLE_STATION_MANAGER
                | eve_db::corp_roles::ROLE_STARBASE_CARETAKER
                | eve_db::corp_roles::ROLE_STARBASE_OPERATOR
                | eve_db::corp_roles::ROLE_STARBASE_CONFIG;
            if (role_mask & fuel_roles) != 0 {
                return true;
            }
        }
    }
    false
}

fn authorize_item_info_access(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    item: &eve_db::InventoryItemRecord,
    allow_on_grid: bool,
) -> Result<ItemInfoAccess, BridgeRpcError> {
    if is_gm_session(session) {
        return Ok(ItemInfoAccess::Full);
    }
    if item.owner_id == char_id {
        return Ok(ItemInfoAccess::Full);
    }
    let authorized_in_db = ctx.db.with_snapshot(|snap| {
        // 1. Item nested inside caller's own ship, container, or hangar asset.
        let mut cur_loc = item.location_id;
        for _ in 0..8 {
            if (60_000_000..=69_999_999).contains(&cur_loc)
                || cur_loc >= 1_000_000_000_000
                || cur_loc <= 0
            {
                break;
            }
            let Some(parent) = snap.items.get(&cur_loc) else {
                break;
            };
            if parent.owner_id == char_id {
                return true;
            }
            if parent.location_id == cur_loc {
                break;
            }
            cur_loc = parent.location_id;
        }

        // 2. Player-corporation item accessible via DB-authoritative corp/hangar roles (never NPC corp).
        let caller_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if caller_corp > 0
            && item.owner_id == caller_corp
            && !is_npc_corporation_id(ctx, caller_corp)
        {
            let (mut root_loc, mut root_flag) = (item.location_id, item.flag);
            for _ in 0..8 {
                if (60_000_000..=69_999_999).contains(&root_loc)
                    || root_loc >= 1_000_000_000_000
                    || snap.structures.contains_key(&root_loc)
                {
                    break;
                }
                let Some(parent) = snap.items.get(&root_loc) else {
                    break;
                };
                if parent.owner_id != caller_corp {
                    break;
                }
                if parent.type_id == 27 {
                    root_loc = parent.location_id;
                    break;
                }
                if parent.location_id == root_loc {
                    break;
                }
                root_loc = parent.location_id;
                root_flag = parent.flag;
            }
            if let Some(gov) = snap.corp_governance.get(&caller_corp) {
                if let Some((&st_id, _)) = gov.offices.iter().find(|(_, &oid)| oid == root_loc) {
                    root_loc = st_id;
                }
            }
            if authorize_corp_flag_access(
                ctx,
                snap,
                session,
                char_id,
                item.owner_id,
                root_loc,
                root_flag,
                root_flag as u16,
            ) {
                return true;
            }
        }

        // 2b. Active structure controllers and Defense-ACL / structure-operator gunners may inspect
        // fitted structure combat equipment, tubes, and specialized ammo holds (never services/rigs/corp hangars).
        if item.owner_id > 0 && !is_npc_corporation_id(ctx, item.owner_id) {
            let (mut root_loc, mut root_flag) = (item.location_id, item.flag);
            for _ in 0..8 {
                if (60_000_000..=69_999_999).contains(&root_loc)
                    || root_loc >= 1_000_000_000_000
                    || snap.structures.contains_key(&root_loc)
                {
                    break;
                }
                let Some(parent) = snap.items.get(&root_loc) else {
                    break;
                };
                if parent.owner_id != item.owner_id || parent.location_id == root_loc {
                    break;
                }
                root_loc = parent.location_id;
                root_flag = parent.flag;
            }
            if let Some(st) = snap.structures.get(&root_loc) {
                if st.owner_corp_id == item.owner_id
                    && is_structure_combat_gunnery_flag(root_flag, root_flag as u16)
                    && can_gunner_access_structure_combat(ctx, snap, st, char_id)
                {
                    return true;
                }
            }
        }
        false
    });
    if authorized_in_db {
        return Ok(ItemInfoAccess::Full);
    }

    // 3. Undocked caller in the same solar system inspecting a visible, uncloaked on-grid ball.
    if allow_on_grid {
        if let Ok(item_sys_id) = u32::try_from(item.location_id) {
            if is_on_grid_visible_ball(ctx, session, item.item_id, item_sys_id) {
                return Ok(ItemInfoAccess::OnGridPublicOnly);
            }
        }
    }

    Err(BridgeRpcError {
        code: "AccessDenied".into(),
        message: format!("Not authorized to inspect item {}", item.item_id),
        client_error: None,
    })
}

fn validate_linkable_weapon_pair(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    first_id: i64,
    second_id: i64,
) -> Result<(), BridgeRpcError> {
    if first_id == second_id {
        return Err(BridgeRpcError {
            code: "InvalidWeaponGroup".into(),
            message: "Two distinct identical high-slot weapons are required to form a group".into(),
            client_error: None,
        });
    }
    let Some(first_it) = snap.items.get(&first_id) else {
        return Err(BridgeRpcError {
            code: "InvalidWeaponGroup".into(),
            message: format!("Module {first_id} not found"),
            client_error: None,
        });
    };
    let Some(second_it) = snap.items.get(&second_id) else {
        return Err(BridgeRpcError {
            code: "InvalidWeaponGroup".into(),
            message: format!("Module {second_id} not found"),
            client_error: None,
        });
    };
    let first_linkable = ctx
        .sde
        .types
        .get(&first_it.type_id)
        .is_some_and(crate::services::combat_crimewatch::combat_weapons_spool::is_linkable_weapon_type);
    let second_linkable = ctx
        .sde
        .types
        .get(&second_it.type_id)
        .is_some_and(crate::services::combat_crimewatch::combat_weapons_spool::is_linkable_weapon_type);
    let first_online = snap
        .module_online_states
        .get(&first_id)
        .copied()
        .unwrap_or(true);
    let second_online = snap
        .module_online_states
        .get(&second_id)
        .copied()
        .unwrap_or(true);
    if first_it.location_id != ship_id
        || second_it.location_id != ship_id
        || !matches!(first_it.flag as u16, 27..=34)
        || !matches!(second_it.flag as u16, 27..=34)
        || first_it.type_id != second_it.type_id
        || !first_linkable
        || !second_linkable
        || !first_online
        || !second_online
    {
        return Err(BridgeRpcError {
            code: "InvalidWeaponGroup".into(),
            message: "Only identical online high-slot weapons fitted to the ship can be grouped".into(),
            client_error: None,
        });
    }
    Ok(())
}

fn find_weapon_bank_master(
    banks: &std::collections::HashMap<i64, Vec<i64>>,
    module_id: i64,
) -> Option<i64> {
    if banks.contains_key(&module_id) {
        return Some(module_id);
    }
    banks
        .iter()
        .find_map(|(&master, members)| members.contains(&module_id).then_some(master))
}

fn peel_module_from_weapon_banks(
    banks: &mut std::collections::HashMap<i64, Vec<i64>>,
    module_id: i64,
    destroyed: &mut Vec<i64>,
) {
    let Some(owner_master) = find_weapon_bank_master(banks, module_id) else {
        return;
    };
    let Some(mut members) = banks.remove(&owner_master) else {
        return;
    };
    members.retain(|&id| id != module_id);
    if members.len() <= 1 {
        if !destroyed.contains(&owner_master) {
            destroyed.push(owner_master);
        }
        return;
    }
    if owner_master == module_id {
        let new_master = members[0];
        if !destroyed.contains(&owner_master) {
            destroyed.push(owner_master);
        }
        banks.insert(new_master, members);
    } else {
        banks.insert(owner_master, members);
    }
}

fn run_weapon_pair_mutation<F>(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    first_name: &str,
    second_name: &str,
    return_info: bool,
    mutate: F,
) -> Option<Result<PyRep, BridgeRpcError>>
where
    F: FnOnce(&mut std::collections::HashMap<i64, Vec<i64>>, i64, i64) -> Vec<i64>,
{
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let rpc = crate::router::RpcArgs::positional(args);
    let ship_id = match rpc.opt_positive_i64(0, "shipID") {
        Ok(v) => v.or(session.ship_id).unwrap_or(0),
        Err(e) => return Some(Err(e)),
    };
    if !ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).is_some_and(|it| it.owner_id == char_id))
    {
        return Some(Err(BridgeRpcError {
            code: "ShipOwnershipViolation".into(),
            message: format!("Ship {ship_id} is not owned by {char_id}"),
            client_error: None,
        }));
    }
    let first_id = match rpc.req_positive_i64(1, first_name) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let second_id = match rpc.req_positive_i64(2, second_name) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let mut mutate_opt = Some(mutate);
    let res = crate::services::combat_crimewatch::combat_weapons_spool::run_with_weapon_link_context(
        ctx,
        Some(session),
        char_id,
        ship_id,
        |snap| {
            validate_linkable_weapon_pair(ctx, snap, ship_id, first_id, second_id)?;
            let banks = snap.weapon_banks.entry(ship_id).or_default();
            let destroyed = if let Some(mutate_fn) = mutate_opt.take() {
                mutate_fn(banks, first_id, second_id)
            } else {
                Vec::new()
            };
            let info =
                crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(
                    snap, ship_id,
                );
            Ok((destroyed, info))
        },
    );
    Some(res.map(|(destroyed, info)| {
        crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(
            ctx, char_id, ship_id, &destroyed, &info,
        );
        if return_info {
            info
        } else {
            PyRep::None
        }
    }))
}

fn handle_item_get_info(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let subloc_tuple = match args.first() {
        Some(PyRep::Tuple(parts)) if parts.len() == 3 => {
            match (parts[0].as_i64(), parts[1].as_i64(), parts[2].as_i64()) {
                (Some(loc_id), Some(flag_id), Some(tid)) if loc_id > 0 => {
                    match (u16::try_from(flag_id), u32::try_from(tid)) {
                        (Ok(flag_u16), Ok(type_id)) if flag_u16 > 0 && type_id > 0 => {
                            Some((loc_id, flag_u16, type_id))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };

    let (subloc_key_opt, item_opt, raw_id) = if let Some((loc_id, flag_u16, sub_tid)) = subloc_tuple {
        let charge_opt = ctx.db.with_snapshot(|snap| {
            let db_corp = snap
                .characters
                .get(&char_id)
                .map(|ch| ch.corporation_id)
                .filter(|&cid| cid > 0);
            let caller_corp = db_corp.or_else(|| session.corporation_id.filter(|&cid| cid > 0));
            let struct_owner = snap
                .structures
                .get(&loc_id)
                .map(|st| st.owner_corp_id)
                .filter(|&oid| oid > 0);
            let loc_owner = snap
                .items
                .get(&loc_id)
                .map(|loc| loc.owner_id)
                .filter(|&oid| oid > 0);
            let flag = InventoryFlag::from_u16(flag_u16);
            let is_personal_hangar = matches!(flag, InventoryFlag::Hangar);
            let is_struct_combat_gunnery =
                struct_owner.is_some() && is_structure_combat_gunnery_flag(flag, flag_u16);
            snap.items
                .values()
                .filter(|i| {
                    i.location_id == loc_id
                        && (i.flag as u16) == flag_u16
                        && i.type_id == sub_tid
                        && i.quantity > 0
                })
                .min_by_key(|i| {
                    // Shared hangars and containers can hold stacks from multiple owners at the
                    // same `(location_id, flag, type_id)`; prefer structure-owned stacks first on
                    // structure combat gunnery slots so an allied gunner's corp stack cannot shadow
                    // loaded structure ammo, otherwise prefer caller/corp/location stacks cleanly.
                    let owner_rank = if is_struct_combat_gunnery {
                        if struct_owner == Some(i.owner_id) {
                            0_u8
                        } else if i.owner_id == char_id {
                            1_u8
                        } else if caller_corp == Some(i.owner_id)
                            || loc_owner == Some(i.owner_id)
                        {
                            2_u8
                        } else {
                            4_u8
                        }
                    } else if i.owner_id == char_id {
                        0_u8
                    } else if !is_personal_hangar && caller_corp == Some(i.owner_id) {
                        1_u8
                    } else if !is_personal_hangar
                        && (struct_owner == Some(i.owner_id) || loc_owner == Some(i.owner_id))
                    {
                        2_u8
                    } else if caller_corp == Some(i.owner_id)
                        || struct_owner == Some(i.owner_id)
                        || loc_owner == Some(i.owner_id)
                    {
                        3_u8
                    } else {
                        4_u8
                    };
                    (owner_rank, i.item_id)
                })
                .cloned()
        });
        let raw_id = charge_opt.as_ref().map_or(0, |it| it.item_id);
        let key_rep = PyRep::Tuple(vec![
            PyRep::Integer(loc_id),
            PyRep::Integer(i64::from(flag_u16)),
            PyRep::Integer(i64::from(sub_tid)),
        ]);
        (Some((key_rep, loc_id, flag_u16, sub_tid)), charge_opt, raw_id)
    } else {
        let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
            return Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ItemGetInfo requires itemID or typeID".into(), client_error: None });
        };
        let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());
        (None, item_opt, raw_id)
    };

    // 1. Check if raw_id is an item in the world DB or a launched space drone ball
    let space_drone_opt = if subloc_key_opt.is_none() {
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space
                .values()
                .find(|d| d.ball_id == raw_id || d.drone_item_id == raw_id)
                .cloned()
        })
    } else {
        None
    };

    // Enforce item ownership / player-corp hangar role / on-grid visibility / GM role before disclosure.
    let mut info_access = ItemInfoAccess::Full;
    if let Some((_, loc_id, flag_u16, _)) = subloc_key_opt.as_ref() {
        let is_gm = is_gm_session(session);
        let flag = InventoryFlag::from_u16(*flag_u16);
        let struct_authorized = ctx.db.with_snapshot(|snap| {
            snap.structures.get(loc_id).map(|st| {
                is_gm
                    || authorize_corp_flag_access(
                        ctx,
                        snap,
                        session,
                        char_id,
                        st.owner_corp_id,
                        *loc_id,
                        flag,
                        *flag_u16,
                    )
                    || (is_structure_combat_gunnery_flag(flag, *flag_u16)
                        && can_gunner_access_structure_combat(ctx, snap, st, char_id))
            })
        });
        if let Some(allowed) = struct_authorized {
            if !allowed {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Not authorized to inspect structure sublocation {loc_id}"),
                    client_error: None,
                });
            }
        } else {
            let loc_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(loc_id).cloned());
            if let Some(ref loc_item) = loc_item_opt {
                authorize_item_info_access(ctx, session, char_id, loc_item, false)?;
            } else if !is_gm {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Not authorized to inspect sublocation {loc_id}"),
                    client_error: None,
                });
            }
        }
        if let Some(ref charge_item) = item_opt {
            authorize_item_info_access(ctx, session, char_id, charge_item, false)?;
        }
    } else if let Some(ref item) = item_opt {
        info_access = authorize_item_info_access(ctx, session, char_id, item, true)?;
    } else if let Some(ref sd) = space_drone_opt {
        let is_gm = is_gm_session(session);
        if !is_gm && sd.owner_char_id != char_id {
            if is_on_grid_visible_ball(ctx, session, sd.ball_id, sd.solar_system_id) {
                info_access = ItemInfoAccess::OnGridPublicOnly;
            } else {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Not authorized to inspect drone {}", sd.ball_id),
                    client_error: None,
                });
            }
        }
    }

    let is_public_only = info_access == ItemInfoAccess::OnGridPublicOnly;

    let (type_id, owner_id, mut dyn_attrs, location_id, qty, singleton) = if let Some(ref item) = item_opt {
        (
            item.type_id,
            PyRep::Integer(item.owner_id),
            item.dynamic_attributes.clone(),
            Some(item.location_id),
            item.quantity,
            if subloc_key_opt.is_some() { false } else { item.singleton },
        )
    } else if let Some((_, loc_id, _, sub_tid)) = subloc_key_opt.as_ref() {
        let loc_id = *loc_id;
        let sub_tid = *sub_tid;
        let ship_owner = ctx.db.with_snapshot(|s| {
            s.items
                .get(&loc_id)
                .map(|i| i.owner_id)
                .or_else(|| s.structures.get(&loc_id).map(|st| st.owner_corp_id))
        });
        (
            sub_tid,
            ship_owner.map(PyRep::Integer).unwrap_or(PyRep::None),
            HashMap::new(),
            Some(loc_id),
            0,
            false,
        )
    } else if let Some(ref sd) = space_drone_opt {
        (
            sd.type_id,
            PyRep::Integer(sd.owner_char_id),
            HashMap::new(),
            Some(sd.parent_ship_id),
            1,
            true,
        )
    } else {
        // Not found in `snap.items` or `active_drones_in_space`. If `raw_id` was recently
        // destroyed in combat/space cleanup or exceeds `u32::MAX` (64-bit dynamic entity ID,
        // never an SDE `typeID`), return `None` as EVE's `godma.py` expects (`UpdateItem` /
        // `RefreshItemEffects` / `RefreshItemAttributes`: `if infoz is None: return`) instead
        // of misinterpreting the stale itemID as a `typeID` and raising `TypeNotFound` (#1032).
        let is_destroyed_entity = raw_id > 0
            && (TypeId::try_from(raw_id).is_err()
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.is_recently_destroyed_item_id(raw_id)
                        || rt.destroyed_npc_cache.contains_key(&raw_id)
                }));
        if is_destroyed_entity {
            return Ok(PyRep::None);
        }
        // Otherwise treat `raw_id` as an SDE `TypeId`.
        let Ok(tid) = TypeId::try_from(raw_id) else {
            return Err(BridgeRpcError {
                code: "TypeNotFound".into(),
                message: format!("Type {raw_id} not found in SDE"),
                client_error: None,
            });
        };
        (tid, PyRep::None, HashMap::new(), None, 1, false)
    };

    if is_public_only {
        dyn_attrs.clear();
    }

    let Some(show_info) = ctx.sde.show_info_for_type(type_id) else {
        return Err(BridgeRpcError { code: "TypeNotFound".into(), message: format!("Type {type_id} not found in SDE"), client_error: None });
    };

    // Merge default attributes with dynamic attributes
    let mut merged_attrs = show_info.default_attributes.clone();
    for (&attr_id, &val) in &dyn_attrs {
        merged_attrs.insert(attr_id, val);
    }
    let mut fitted_online_ship: Option<(i64, i64)> = None;
    let item_cat = ctx.sde.types.get(&type_id).map(|t| t.category_id).unwrap_or(0);
    if !is_public_only {
        if item_cat == 18 {
            let parent_opt = space_drone_opt
                .as_ref()
                .map(|sd| (sd.owner_char_id, sd.parent_ship_id))
                .or_else(|| item_opt.as_ref().map(|it| (it.owner_id, it.location_id)));
            if let Some((p_owner, p_ship)) = parent_opt {
                if let Ok(fit) = ctx.fit(p_owner, p_ship) {
                    let drone_state_opt = fit.drone_state(raw_id, &ctx.sde).or_else(|| {
                        fit.evaluated_drones
                            .values()
                            .find(|d| d.type_id == type_id)
                            .cloned()
                    });
                    if let Some(d_state) = drone_state_opt {
                        let all_aids: std::collections::BTreeSet<u16> = d_state
                            .base_attributes
                            .keys()
                            .copied()
                            .chain(d_state.modifiers.iter().map(|m| m.target_attribute_id))
                            .collect();
                        for aid in all_aids {
                            merged_attrs.insert(aid, d_state.get_attribute(aid, &ctx.sde));
                        }
                    }
                    let has_active_triage = fit.modules.values().any(|fm| {
                        matches!(fm.type_id, 27951 | 4294)
                            && matches!(
                                fm.state,
                                eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                            )
                    });
                    if has_active_triage {
                        merged_attrs.insert(64, 0.0);
                    }
                }
            }
        } else if let Some(ref item) = item_opt {
            if item_cat == 6 {
                if let Ok(fit) = ctx.fit(item.owner_id, item.item_id) {
                    let all_aids: std::collections::BTreeSet<u16> = fit
                        .ship
                        .base_attributes
                        .keys()
                        .copied()
                        .chain(fit.ship.modifiers.iter().map(|m| m.target_attribute_id))
                        .collect();
                    for aid in all_aids {
                        merged_attrs.insert(aid, fit.ship.get_attribute(aid, &ctx.sde));
                    }
                    for (&attr_id, &val) in &dyn_attrs {
                        if matches!(attr_id, 3 | 18 | 264 | 265 | 266)
                            || !fit
                                .ship
                                .modifiers
                                .iter()
                                .any(|m| m.target_attribute_id == attr_id)
                        {
                            merged_attrs.insert(attr_id, val);
                        }
                    }
                    let already_applied_dbuffs: Vec<u32> =
                        fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
                    crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
                        ctx,
                        item.item_id,
                        &already_applied_dbuffs,
                        &mut merged_attrs,
                    );
                }
            } else if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
                if let Ok(fit) = ctx.fit(item.owner_id, item.location_id) {
                    if item_cat == 8 {
                        let slot_flag = item.flag as u16;
                        let launcher_id_opt = fit
                            .modules
                            .values()
                            .find(|m| m.flag_id == slot_flag)
                            .map(|m| m.item_id);
                        let charge_state_opt = launcher_id_opt
                            .and_then(|lid| fit.charge_state(lid, &ctx.sde))
                            .or_else(|| {
                                fit.evaluated_charges
                                    .values()
                                    .find(|c| c.type_id == item.type_id)
                                    .cloned()
                            });
                        if let Some(c_state) = charge_state_opt {
                            let all_aids: std::collections::BTreeSet<u16> = c_state
                                .base_attributes
                                .keys()
                                .copied()
                                .chain(c_state.modifiers.iter().map(|m| m.target_attribute_id))
                                .collect();
                            for aid in all_aids {
                                merged_attrs.insert(aid, c_state.get_attribute(aid, &ctx.sde));
                            }
                        }
                    } else if let Some(m_state) = fit.module_state(item.item_id, &ctx.sde) {
                        let all_aids: std::collections::BTreeSet<u16> = m_state
                            .base_attributes
                            .keys()
                            .copied()
                            .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
                            .collect();
                        for aid in all_aids {
                            merged_attrs.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                        }
                        crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                            ctx,
                            item.location_id,
                            &mut merged_attrs,
                        );
                    }
                    if item_cat != 8 {
                        let is_mod_online = fit
                            .modules
                            .get(&item.item_id)
                            .map(|m| m.state != eve_dogma::ModuleState::Offline)
                            .unwrap_or_else(|| {
                                ctx.db.with_snapshot(|s| {
                                    s.module_online_states.get(&item.item_id).copied().unwrap_or(true)
                                })
                            });
                        merged_attrs.insert(2, if is_mod_online { 1.0 } else { 0.0 });
                        if is_mod_online {
                            fitted_online_ship = Some((item.owner_id, item.location_id));
                        }
                    }
                    for &aid in &[3u16, 9, 84, 267, 268, 269, 270] {
                        if let Some(&v) = dyn_attrs.get(&aid) {
                            merged_attrs.insert(aid, v);
                        }
                    }
                }
            }
        }
    }

    if raw_id > 0 && !is_public_only {
        crate::services::tactical_modules::apply_transient_item_attrs(ctx, raw_id, &mut merged_attrs);
    }
    if subloc_key_opt.is_some() {
        merged_attrs.insert(attr_ids::QUANTITY, qty as f64);
    }
    let dur_ms = merged_attrs
        .get(&73)
        .copied()
        .filter(|&v| v > 0.0)
        .or_else(|| merged_attrs.get(&51).copied().filter(|&v| v > 0.0))
        .unwrap_or(1_000.0)
        .max(0.0);
    let mut attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
        .into_iter()
        .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
        .collect();
    attr_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

    let mut dyn_entries: Vec<(PyRep, PyRep)> = dyn_attrs
        .into_iter()
        .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
        .collect();
    dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

    let var_reps: Vec<PyRep> = show_info.variations.into_iter().map(|v| PyRep::Integer(v as i64)).collect();
    let custom_name = if raw_id > 0 {
        ctx.db.with_snapshot(|snap| snap.item_custom_names.get(&raw_id).cloned()).unwrap_or_default()
    } else {
        String::new()
    };
    let now_ft = ctx.now_client_sim_filetime_ticks();
    let inv_item = if subloc_key_opt.is_some() || is_public_only {
        PyRep::None
    } else {
        ctx.db
            .get_item(raw_id)
            .map(|it| eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(item_to_packed_cells(ctx, &it)))
            .unwrap_or(PyRep::None)
    };

    let active_effects = match fitted_online_ship {
        Some((mod_owner_id, mod_ship_id)) => {
            let mut effs = crate::services::inventory_fitting::fitting_module_cycles::online_module_active_effects(
                raw_id,
                mod_owner_id,
                mod_ship_id,
                now_ft,
                true,
            );
            let is_mod_active = ctx.db.with_snapshot(|s| s.module_active_states.contains(&raw_id))
                || ctx
                    .ship_fittings
                    .read()
                    .ok()
                    .and_then(|sf| {
                        sf.get(&mod_ship_id).and_then(|f| {
                            f.modules.get(&raw_id).map(|m| {
                                matches!(
                                    m.state,
                                    eve_dogma::ModuleState::Active
                                        | eve_dogma::ModuleState::Overloaded
                                )
                            })
                        })
                    })
                    .unwrap_or(false);
            if is_mod_active {
                if let Some(eff_id) = crate::router::binding::effect_id_for_module_sde(
                    &ctx.sde,
                    show_info.type_id,
                    Some("defaultEffect"),
                )
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&show_info.type_id)
                        .and_then(|t| t.effect_ids.iter().copied().find(|&eid| eid != 16))
                        .map(|eid| eid as i64)
                }) {
                    if eff_id != 16 {
                        if let PyRep::Dict(ref mut entries) = effs {
                            entries.push((
                                PyRep::Integer(eff_id),
                                PyRep::List(vec![
                                    PyRep::Integer(raw_id),
                                    PyRep::Integer(mod_owner_id),
                                    PyRep::Integer(mod_ship_id),
                                    PyRep::None,
                                    PyRep::None,
                                    PyRep::List(Vec::new()),
                                    PyRep::Integer(eff_id),
                                    PyRep::Integer(now_ft),
                                    PyRep::Real(dur_ms),
                                    PyRep::Integer(1000),
                                ]),
                            ));
                        }
                    }
                }
            }
            effs
        }
        None => PyRep::Dict(Vec::new()),
    };

    let item_id_rep = subloc_key_opt
        .map(|(key_rep, _, _, _)| key_rep)
        .unwrap_or(PyRep::Integer(raw_id));

    Ok(PyRep::keyval([
        ("itemID", item_id_rep),
        ("invItem", inv_item),
        ("typeID", PyRep::Integer(show_info.type_id as i64)),
        ("typeName", PyRep::String(show_info.name)),
        ("customName", PyRep::String(custom_name)),
        ("groupID", PyRep::Integer(show_info.group_id as i64)),
        ("categoryID", PyRep::Integer(show_info.category_id as i64)),
        ("ownerID", owner_id),
        ("locationID", location_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("quantity", PyRep::Integer(qty as i64)),
        ("singleton", PyRep::Bool(singleton)),
        ("mass", PyRep::Real(show_info.mass)),
        ("volume", PyRep::Real(show_info.volume)),
        ("capacity", PyRep::Real(show_info.capacity)),
        ("portionSize", PyRep::Integer(show_info.portion_size as i64)),
        ("basePrice", PyRep::Real(show_info.base_price)),
        ("published", PyRep::Bool(show_info.published)),
        ("marketGroupID", show_info.market_group_id.map(|mg| PyRep::Integer(mg as i64)).unwrap_or(PyRep::None)),
        ("variationParentTypeID", show_info.variation_parent_type_id.map(|vp| PyRep::Integer(vp as i64)).unwrap_or(PyRep::None)),
        ("variations", PyRep::List(var_reps)),
        ("dynamicAttributes", PyRep::Dict(dyn_entries)),
        ("attributes", PyRep::Dict(attr_entries)),
        ("activeEffects", active_effects),
        ("time", PyRep::Integer(now_ft)),
        ("wallclockTime", PyRep::Integer(now_ft)),
    ]))
}

#[derive(Clone, Debug, Default)]
struct DogmaSessionCheckState {
    character_id: i64,
    ship_id: Option<i64>,
    solar_system_id: Option<u32>,
    station_id: Option<i64>,
    corporation_id: Option<i64>,
    check_count: u64,
    updated_at: u64,
}

fn dogma_session_check_store(
) -> &'static std::sync::RwLock<HashMap<(u64, u64), DogmaSessionCheckState>> {
    static STORE: std::sync::OnceLock<
        std::sync::RwLock<HashMap<(u64, u64), DogmaSessionCheckState>>,
    > = std::sync::OnceLock::new();
    STORE.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

fn lookup_session_check_i64(
    spec: Option<&PyRep>,
    kwargs: &HashMap<String, PyRep>,
    keys: &[&str],
) -> Option<i64> {
    for &k in keys {
        if let Some(val) = kwargs.get(k).and_then(|v| v.as_i64()) {
            return Some(val);
        }
        if let Some(rep) = spec {
            if let Some(val) = rep.dict_get(k).and_then(|v| v.as_i64()) {
                return Some(val);
            }
        }
    }
    None
}

fn handle_dogma_set_session_check(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let clear_requested =
        matches!(args.first(), Some(PyRep::None)) && kwargs.is_empty();
    let spec = args.first().filter(|a| !matches!(a, PyRep::None));

    if !clear_requested {
        if let Some(exp_char) = lookup_session_check_i64(spec, kwargs, &["charid", "charID"]) {
            if exp_char != char_id {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check charid {exp_char} does not match {char_id}"
                    ),
                    client_error: None,
                });
            }
        }
        if let Some(exp_sys) = lookup_session_check_i64(
            spec,
            kwargs,
            &["solarsystemid2", "solarsystemid", "solarSystemID"],
        ) {
            if session.solar_system_id.map(i64::from) != Some(exp_sys) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check solarsystemid2 {exp_sys} does not match {:?}",
                        session.solar_system_id
                    ),
                    client_error: None,
                });
            }
        }
        if let Some(exp_ship) = lookup_session_check_i64(spec, kwargs, &["shipid", "shipID"]) {
            if session.ship_id != Some(exp_ship) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check shipid {exp_ship} does not match {:?}",
                        session.ship_id
                    ),
                    client_error: None,
                });
            }
        }
        if let Some(exp_station) =
            lookup_session_check_i64(spec, kwargs, &["stationid", "stationID"])
        {
            if session.station_id != Some(exp_station) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check stationid {exp_station} does not match {:?}",
                        session.station_id
                    ),
                    client_error: None,
                });
            }
        }
        if let Some(exp_corp) = lookup_session_check_i64(spec, kwargs, &["corpid", "corpID"]) {
            if session.corporation_id != Some(exp_corp) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check corpid {exp_corp} does not match {:?}",
                        session.corporation_id
                    ),
                    client_error: None,
                });
            }
        }
    }

    let ship_in_db = session.ship_id.is_some_and(|sid| {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&sid)
                .is_some_and(|it| it.owner_id == char_id)
        })
    });
    let fitting_valid = match session.ship_id {
        Some(sid) if ship_in_db => ctx.fit(char_id, sid).is_ok(),
        Some(_) => false,
        None => true,
    };

    let key = (ctx.instance_id, session.session_id);
    let (check_count, session_check_rep) = if clear_requested {
        let mut guard = dogma_session_check_store()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.remove(&key);
        (0u64, PyRep::None)
    } else {
        let mut guard = dogma_session_check_store()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key).or_default();
        entry.character_id = char_id;
        entry.ship_id = session.ship_id;
        entry.solar_system_id = session.solar_system_id;
        entry.station_id = session.station_id;
        entry.corporation_id = session.corporation_id;
        entry.check_count += 1;
        entry.updated_at = now_unix;
        let mut check_fields = vec![("charid", PyRep::Integer(entry.character_id))];
        if let Some(corp) = entry.corporation_id {
            check_fields.push(("corpid", PyRep::Integer(corp)));
        }
        if let Some(ship) = entry.ship_id {
            check_fields.push(("shipid", PyRep::Integer(ship)));
        }
        if let Some(sys) = entry.solar_system_id {
            check_fields.push(("solarsystemid2", PyRep::Integer(i64::from(sys))));
        }
        if let Some(station) = entry.station_id {
            check_fields.push(("stationid", PyRep::Integer(station)));
        }
        (entry.check_count, PyRep::keyval(check_fields))
    };

    Ok(PyRep::keyval([
        ("valid", PyRep::Bool(fitting_valid)),
        ("characterID", PyRep::Integer(char_id)),
        (
            "shipID",
            session.ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "solarSystemID",
            session
                .solar_system_id
                .map(|s| PyRep::Integer(i64::from(s)))
                .unwrap_or(PyRep::None),
        ),
        (
            "stationID",
            session
                .station_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "corporationID",
            session
                .corporation_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "boundObjectID",
            bound_entry
                .map(|b| PyRep::Integer(b.object_id))
                .unwrap_or(PyRep::None),
        ),
        ("sessionCheck", session_check_rep),
        ("fittingValid", PyRep::Bool(fitting_valid)),
        ("checkCount", PyRep::Integer(check_count as i64)),
        ("updatedAt", PyRep::Integer(now_unix as i64)),
    ]))
}

pub(crate) async fn handle_dogma_im(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: rpc:dogmaIM.SetSessionCheck
        ("dogmaIM", "SetSessionCheck") => {
            Some(handle_dogma_set_session_check(ctx, session, bound_entry, args, kwargs))
        }

        // rpc-lint: allow tools/client-harness/run_journeys bridge probe calls GetShipAttributes
        ("dogmaIM", "GetAllInfo") | ("dogmaIM", "GetShipAttributes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = bound_entry
                .map(|b| b.object_id)
                .filter(|&id| ctx.db.get_item(id).is_some())
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into(), client_error: None }));
            };

            let (ship_type_id, active_stance, entries, module_entries) =
                match evaluate_ship_dogma_attributes(ctx, char_id, ship_id) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

            if method == "GetAllInfo" {
                Some(Ok(build_all_info(ctx, session, char_id, ship_id, ship_type_id, active_stance, entries, module_entries, args)))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("shipStanceMgr", "SetShipStance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (ship_id_opt, stance_type_id_opt) = if args.len() >= 2 {
                let explicit_ship_id = args.first().and_then(|a| a.as_i64());
                let valid_none = matches!(args.first(), Some(PyRep::None));
                if !valid_none && explicit_ship_id.is_none_or(|id| id <= 0) {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot change stance on invalid ship target".into(),
                        client_error: None,
                    }));
                }
                (explicit_ship_id, args.get(1).and_then(|a| a.as_i64()))
            } else {
                (None, args.first().and_then(|a| a.as_i64()))
            };
            let Some(active_ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into(), client_error: None }));
            };
            let ship_id = ship_id_opt
                .filter(|&id| id > 0)
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(active_ship_id);
            if ship_id != active_ship_id {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Cannot change stance on non-active ship {ship_id}"),
                    client_error: None,
                }));
            }
            let Some(stance_arg) = stance_type_id_opt.filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError { code: "InvalidShipStance".into(), message: "SetShipStance requires a valid stance ID or typeID".into(), client_error: None }));
            };
            let now_ms = ctx.now_unix_secs().saturating_mul(1000);
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            // V24.01 `shipstance.pyj` passes 1-based `shipmode/data.pyj` stance keys
            // (`1 = shipStanceDefense`, `2 = shipStanceSpeed`, `3 = shipStanceSniper`),
            // while internal/compat tests may pass group-1306 stance `type_id`s directly.
            let Some(stance_type_id) = eve_dogma::ShipFittingEngine::resolve_t3d_stance_type_id(
                fit.ship.type_id,
                stance_arg,
            ) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipStance".into(),
                    message: format!(
                        "Ship type {} cannot enter stance {stance_arg}",
                        fit.ship.type_id
                    ),
                    client_error: None,
                }));
            };
            if fit.active_stance_type_id.is_none() {
                fit.active_stance_type_id =
                    eve_dogma::ShipFittingEngine::default_t3d_stance_type_id(fit.ship.type_id);
            }
            if let Err(e) = fit.switch_ship_stance(stance_type_id, now_ms, &ctx.sde) {
                let code = if e.contains("Cooldown") {
                    "StanceSwitchCooldown"
                } else {
                    "InvalidShipStance"
                };
                return Some(Err(BridgeRpcError { code: code.into(), message: e, client_error: None }));
            }
            let max_vel = fit.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
            // `ShipStanceButton.OnStanceActive`, `FittingController.OnStanceActive`, and
            // `spaceobjanimation.SetShipAnimationStance` all match against the V24 1-based
            // `shipmode/data.pyj` stance key (`1/2/3`), not the group-1306 item `type_id`.
            let active_stance_key = eve_dogma::ShipFittingEngine::t3d_stance_key(
                fit.ship.type_id,
                stance_type_id,
            )
            .map(i64::from)
            .unwrap_or_else(|| i64::from(stance_type_id));
            ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
            ctx.invalidate_and_push_fit(ship_id);
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnStanceActive {
                    ship_id,
                    stance_id: active_stance_key,
                },
            );
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnModuleAttributeChanges {
                    changes: PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::String("OnModuleAttributeChange".into()),
                        PyRep::Integer(char_id),
                        PyRep::Integer(ship_id),
                        PyRep::Integer(eve_sde::attr_ids::MAX_VELOCITY as i64),
                        PyRep::Integer(ctx.now_client_sim_filetime_ticks()),
                        PyRep::Real(max_vel),
                        PyRep::Real(max_vel),
                        PyRep::None,
                    ])]),
                },
            );
            Some(Ok(PyRep::Integer(i64::from(stance_type_id))))
        }

        ("dogmaIM", "ItemGetInfo") => {
            Some(handle_item_get_info(ctx, session, args))
        }
        ("invbroker", "GetItemInfo") => {
            Some(handle_item_get_info(ctx, session, args))
        }
        ("invbroker", "GetDynamicItemInfo") => {
            Some(handle_item_get_info(ctx, session, args))
        }
        ("dogmaIM", "GetDynamicItemInfo") => {
            Some(handle_item_get_info(ctx, session, args))
        }

        ("dogmaIM", "DestroyFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DestroyFitting requires itemID".into(), client_error: None }));
            };
            let undocked = session.station_id.is_none() && session.structure_id.is_none();
            if undocked {
                let now_unix = ctx.now_unix_secs();
                let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                if has_weapons_timer {
                    return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot destroy fitted modules or rigs in space while weapons timer is active".into(), client_error: None }));
                }
                let near_fitting_service = match (session.solar_system_id, session.ship_id) {
                    (Some(sys_id), Some(ship_id)) => is_near_in_space_fitting_service(ctx, sys_id, ship_id).await,
                    _ => false,
                };
                if !near_fitting_service {
                    return Some(Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Destroying fittings in space requires being within range of an anchored, non-reinforced Mobile Depot or capital SMA".into(), client_error: None }));
                }
            }
            if let Some(sys_id) = session.solar_system_id {
                let loc_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).map(|i| i.location_id));
                if let Some(loc) = loc_opt {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    if sys.has_active_module_cycle_sync(loc, item_id) {
                        return Some(Err(BridgeRpcError { code: "ModuleIsActive".into(), message: format!("Module {item_id} cannot be destroyed while active"), client_error: None }));
                    }
                    if sys.has_active_nanite_repair_sync(loc, item_id) {
                        return Some(Err(BridgeRpcError { code: "ModuleBeingRepaired".into(), message: format!("Module {item_id} cannot be destroyed while undergoing nanite repair"), client_error: None }));
                    }
                }
            }
            let old_fit_loads = ctx
                .db
                .with_snapshot(|s| s.items.get(&item_id).map(|i| i.location_id))
                .and_then(|loc| {
                    ctx.fit(char_id, loc).ok().map(|f| {
                        (
                            f.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde),
                            f.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde),
                        )
                    })
                });
            let destroy_res = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get(&item_id) {
                    let loc = item.location_id;
                    let flag_u16 = item.flag as u16;
                    let is_struct_slot = matches!(flag_u16, 11..=34 | 92..=94 | 164..=171);
                    let struct_opt = snap.structures.get(&loc);
                    if let Some(st) = struct_opt.filter(|st| is_struct_slot && item.owner_id == st.owner_corp_id) {
                        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                        if st.owner_corp_id != caller_corp {
                            return Err(BridgeRpcError { code: "StructureOwnershipViolation".into(), message: "Only the owning corporation can destroy structure fittings".into(), client_error: None });
                        }
                        if !crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(snap, session, char_id, caller_corp) {
                            return Err(BridgeRpcError { code: "InsufficientCorpRoles".into(), message: "Destroying structure fittings requires Director or Station Manager role".into(), client_error: None });
                        }
                    } else if item.owner_id != char_id {
                        return Err(BridgeRpcError { code: "ItemNotOwnedByCharacter".into(), message: "Cannot destroy an item not owned by character".into(), client_error: None });
                    } else if Some(item_id) == session.ship_id
                        || snap.characters.get(&char_id).is_some_and(|c| c.active_ship_id == item_id)
                        || (undocked && (Some(loc) != session.ship_id || !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)))
                        || (!undocked && flag_u16 != 4 && !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132 | 164..=171))
                    {
                        return Err(BridgeRpcError { code: "ItemNotFittedToActiveShip".into(), message: "DestroyFitting requires a fitted item".into(), client_error: None });
                    }
                    crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, item)?;
                    if snap.module_active_states.contains(&item_id) {
                        return Err(BridgeRpcError { code: "ModuleIsActive".into(), message: format!("Module {item_id} cannot be destroyed while active"), client_error: None });
                    }
                    if (125..=132).contains(&flag_u16) {
                        let has_fitted_modules = snap.items.values().any(|other| {
                            other.location_id == loc && other.item_id != item_id && (11..=34).contains(&(other.flag as u16)) && ctx.sde.types.get(&other.type_id).is_none_or(|t| t.category_id != 8)
                        });
                        if has_fitted_modules {
                            return Err(BridgeRpcError { code: "SubsystemHasFittedModules".into(), message: "Cannot destroy a T3 Strategic Cruiser subsystem while modules are fitted".into(), client_error: None });
                        }
                    }
                    let old_flag = item.flag;
                    let found = (loc, old_flag as i64, item.type_id as i64, item.quantity as i64);
                    let mut removed_charges = Vec::new();
                    if (11..=34).contains(&flag_u16)
                        && ctx.sde.types.get(&item.type_id).is_none_or(|t| t.category_id != 8)
                    {
                        let charge_ids: Vec<i64> = snap
                            .items
                            .values()
                            .filter(|other| {
                                other.location_id == loc
                                    && other.flag == old_flag
                                    && other.item_id != item_id
                                    && ctx.sde.types.get(&other.type_id).is_some_and(|t| t.category_id == 8)
                            })
                            .map(|other| other.item_id)
                            .collect();
                        for cid in charge_ids {
                            if let Some(c_rec) = snap.items.get(&cid).cloned() {
                                removed_charges.push(c_rec);
                            }
                            snap.remove_system_cleanup_item(cid);
                        }
                    }
                    if let Some(banks) = snap.weapon_banks.get_mut(&loc) {
                        for list in banks.values_mut() {
                            list.retain(|&id| id != item_id);
                        }
                        banks.retain(|k, v| *k != item_id && v.len() > 1);
                    }
                    ServerContext::clear_persisted_module_overload(snap, loc, item_id);
                    snap.remove_system_cleanup_item(item_id);
                    Ok(Some((found, removed_charges)))
                } else {
                    Ok(None)
                }
            });
            let loc_opt = match destroy_res { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if let Some(((loc, flag, type_id, qty), removed_charges)) = loc_opt {
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let _ = sys.remove_module_cycle(loc, item_id).await;
                }
                let affected_ewar = crate::services::notify_emitters_space_dungeons::flush_module_profile_and_ewar_end(
                    ctx,
                    char_id,
                    session.solar_system_id,
                    loc,
                    item_id,
                );
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    for tid in affected_ewar {
                        let tp_mult = ctx.effective_target_painter_multiplier(tid);
                        sys.try_recompute_combat_attributes(tid, |t_cs| {
                            t_cs.tp_sig_multiplier = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
                        });
                    }
                }
                crate::services::tactical_modules::remove_projected_remote_assistance(ctx, loc, item_id);
                for c_rec in &removed_charges {
                    ctx.notify_item_mutation(char_id, Some(c_rec), None);
                }
                ctx.invalidate_and_push_fit(loc);
                if let Ok(new_fit) = ctx.fit(char_id, loc) {
                    if let Some((old_cpu, old_pg)) = old_fit_loads {
                        let new_cpu = new_fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                        let new_pg = new_fit.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde);
                        let _ = ctx.notify_ship_fitting_load_changes(
                            char_id, loc, old_cpu, new_cpu, old_pg, new_pg,
                        );
                    }
                }
                // V24 client derives `OnModuleUnfitted` from this `OnItemChange` (`ixFlag` change).
                let _ = ctx.notify_fitted_item_trashed(char_id, item_id, type_id, qty, loc, flag);
            }
            Some(Ok(PyRep::keyval([("itemID", PyRep::Integer(item_id)), ("destroyed", PyRep::Bool(true))])))
        }

        ("dogmaIM", "SetModuleOnline") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            Some(
                crate::services::notify_emitters_skills_trade_godma::handle_dogma_set_module_online(
                    ctx, session, args,
                )
                .await,
            )
        }

        ("dogmaIM", "TakeModuleOffline") => {
            Some(
                crate::services::notify_emitters_skills_trade_godma::handle_dogma_take_module_offline(
                    ctx, session, args,
                )
                .await,
            )
        }

        ("dogmaIM", "AddTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into(), client_error: None }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "AddTarget requires integer targetID".into(), client_error: None }));
            };
            if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
                return Some(crate::services::structure_weapons_ops::handle_structure_add_target(ctx, session, char_id, ship_id, target_id).await);
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session is not in space".into(), client_error: None }));
            };
            if let Err(e) = crate::services::tourney_service::check_tourney_lock_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let sys = ctx.systems.get_or_spawn(sys_id);
            if crate::services::tactical_module_mechanics::is_untargetable_interdiction_probe(
                ctx, &sys, target_id,
            )
            .await
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "Interdiction probes cannot be targeted directly; use area-of-effect weapons".into(),
                    client_error: None,
                }));
            }
            let is_upwell_target = target_id >= 1_000_000_000_000
                || ctx.db.with_snapshot(|s| s.structures.contains_key(&target_id));
            if is_upwell_target {
                crate::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(
                    ctx, sys_id, target_id,
                )
                .await;
            }
            let Some((_, ship_ball)) = sys.get_ball_snapshot(ship_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Ship ball {ship_id} not found in system"), client_error: None }));
            };
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Target ball {target_id} not found in system"), client_error: None }));
            };

            let target_char_id = if is_upwell_target {
                None
            } else {
                ctx.db.with_snapshot(|snap| {
                    if snap.characters.contains_key(&target_id) {
                        Some(target_id)
                    } else {
                        snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)
                    }
                })
            };
            if let Some(t_char) = target_char_id {
                let now_unix = ctx.now_unix();
                if crate::services::space_destiny::is_location_change_invulnerable(ctx, t_char, now_unix) {
                    return Some(Err(BridgeRpcError { code: "TargetInvulnerable".into(), message: format!("Target character {t_char} is invulnerable due to recent location change"), client_error: None }));
                }
            }
            if let Some(tcs) = sys.get_combat_state(target_id).await.ok().flatten() {
                if tcs.max_structure_hp > 0.0 && tcs.structure_hp <= 0.0 {
                    return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Target {target_id} is destroyed"), client_error: None }));
                }
                if tcs.is_tethered {
                    return Some(Err(BridgeRpcError { code: "TargetInvulnerable".into(), message: format!("Target {target_id} is tethered and invulnerable to target locks"), client_error: None }));
                }
            }
            crate::services::space_destiny::break_location_change_invulnerability(ctx, char_id);
            let fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 && !cs.ecm_jammed_by.contains(&target_id) {
                    return Some(Err(ship_jammed_rpc_error(ctx, target_id)));
                }
                if (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0) || cs.decloak_targeting_delay_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: "Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay".into(), client_error: None }));
                }
            }

            let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let surface_offset = ship_ball.radius + target_ball.radius;
            let dist = ((ship_ball.position - target_ball.position).length() - surface_offset).max(0.0);
            if dist > max_range {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)"), client_error: None }));
            }

            let unapplied_triage_ship_bonus: usize = fit
                .modules
                .values()
                .filter(|fm| {
                    matches!(
                        fm.state,
                        eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                    ) && !fit.ship.modifiers.iter().any(|m| {
                        m.source_item_id == fm.item_id
                            && m.target_attribute_id == attr_ids::MAX_LOCKED_TARGETS
                    })
                })
                .map(|fm| match fm.type_id {
                    27951 => 4usize,
                    4294 => 5usize,
                    _ => 0usize,
                })
                .sum();
            let ship_max = (fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize)
                + unapplied_triage_ship_bonus;
            let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
            let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
            let char_eval = fit
                .get_character_effective_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde)
                .round()
                .max(0.0) as usize;
            let skill_char_max = 2 + (tm_lvl.min(5) as usize) + (atm_lvl.min(5) as usize);
            let skill_bottleneck = char_eval.max(skill_char_max) < ship_max;
            let max_locked = ship_max
                .min(char_eval.max(skill_char_max))
                .max(eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl));
            // Prune stale target locks whose target ball left the ballpark, warped, cloaked, or
            // moved outside `max_range_m` before `step()` ran, so despawned/broken targets never
            // permanently occupy server target slots (`#749`).
            let raw_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
            let mut current_locks = Vec::with_capacity(raw_locks.len());
            for existing_lock in raw_locks {
                let Some((_, tb)) = sys
                    .get_ball_snapshot(existing_lock.target_id)
                    .await
                    .unwrap_or(None)
                else {
                    let _ = sys.remove_target_lock(ship_id, existing_lock.target_id).await;
                    continue;
                };
                let tb_untarg = sys
                    .get_combat_state(existing_lock.target_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|c| c.is_cloaked || c.is_warp_exit_invulnerable());
                let tb_warping = matches!(tb.mode, eve_destiny::MovementMode::Warp { .. });
                let tb_out_of_range = existing_lock
                    .max_range_m
                    .is_some_and(|max_r| (ship_ball.position - tb.position).length() > max_r);
                if tb_untarg || tb_warping || tb_out_of_range {
                    let _ = sys.remove_target_lock(ship_id, existing_lock.target_id).await;
                    continue;
                }
                current_locks.push(existing_lock);
            }

            // Passive Targeter (`group_id == 82`, `feat:modules.passive-targeter`) suppresses `"targetedBy"` yellow-box notification!
            let has_active_passive_targeter = fit.modules.values().any(|fm| {
                matches!(
                    fm.state,
                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                ) && ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 82)
            });

            // If `target_id` is already locked on the server (e.g. client UI dropped it via local
            // `FailLockTarget` and re-issued `AddTarget`), return `locked = true` (`flag = 0`) with
            // the active target list instead of consuming an extra slot or failing `max_locked` (`#749`).
            if let Some(existing) = current_locks.iter().find(|l| l.target_id == target_id) {
                if existing.locked {
                    let locked_ids: Vec<PyRep> = current_locks
                        .iter()
                        .filter(|l| l.locked)
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::keyval([
                        ("targetID", PyRep::Integer(target_id)),
                        ("targetIDs", PyRep::List(locked_ids)),
                        ("locked", PyRep::Bool(true)),
                        ("lockTimeMs", PyRep::Real(0.0)),
                        ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
                    ])));
                }
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("locked", PyRep::Bool(false)),
                    ("lockTimeMs", PyRep::Real(existing.remaining_lock_ms.max(0.0))),
                    ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
                ])));
            } else if current_locks.len() >= max_locked {
                return Some(Err(target_lock_cap_rpc_error(max_locked, skill_bottleneck)));
            }

            let scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);
            let raw_sig = sys
                .get_combat_state(target_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.base_signature_radius.unwrap_or(cs.signature_radius))
                .filter(|&s| s > 0.0)
                .unwrap_or(0.0)
                .max(target_ball.radius)
                .max(35.0);
            let target_sig = ctx.effective_target_signature_radius(target_id, raw_sig);
            let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
            let lock_time_ms = lock_sec * 1000.0;

            let lock = eve_destiny::TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range + surface_offset),
            };
            if let Err(e) = sys.add_target_lock(ship_id, lock).await {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: e, client_error: None }));
            }
            crate::services::space_destiny::break_location_change_invulnerability(ctx, char_id);

            if !has_active_passive_targeter {
                if let Some(target_owner_id) = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
                    .filter(|&oid| oid != char_id && oid > 0)
                {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(target_owner_id),
                        crate::router::notify_registry::OnTarget {
                            arg0: PyRep::Integer(ship_id),
                            arg1: PyRep::String("targetedBy".into()),
                            reason: None,
                        },
                    );
                }
            }

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("locked", PyRep::Bool(false)),
                ("lockTimeMs", PyRep::Real(lock_time_ms)),
                ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
            ])))
        }

        ("dogmaIM", "RemoveTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into(), client_error: None }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "RemoveTarget requires targetID".into(), client_error: None }));
            };
            let (removed, sys_id_opt) = match (session.station_id, session.solar_system_id) {
                (None, Some(sys_id)) => {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    (sys.remove_target_lock_quiet(ship_id, target_id).await.unwrap_or(false), Some(sys_id))
                }
                _ => (false, None),
            };
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(target_id));
            crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(ctx, ship_id, char_id, Some(target_id));
            if removed {
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnTarget {
                        arg0: PyRep::Integer(target_id),
                        arg1: PyRep::String("lost".into()),
                        reason: None,
                    },
                );
                let _ = ctx.notify_target_events(char_id, &[("lost", target_id, "")]);
                if let (Some(sys_id), Some(target_char)) = (
                    sys_id_opt,
                    ctx.db.with_snapshot(|s| {
                        let oid = s.items.get(&target_id)?.owner_id;
                        if oid != char_id && s.characters.get(&oid).is_some_and(|c| c.station_id.is_none() && c.solar_system_id == sys_id_opt.unwrap_or(0)) {
                            Some(oid)
                        } else {
                            None
                        }
                    }),
                ) {
                    let _ = sys_id;
                    let _ = ctx.notify_target_events(target_char, &[("otherlost", ship_id, "")]);
                }
            }
            Some(Ok(PyRep::Integer(target_id)))
        }

        ("dogmaIM", "ClearTargets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into(), client_error: None }));
            };
            let (cleared_targets, sys_id_opt) = match (session.station_id, session.solar_system_id) {
                (None, Some(sys_id)) => {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let prior = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let _ = sys.clear_target_locks_quiet(ship_id).await;
                    let locked_ids: Vec<i64> = prior.into_iter().filter(|l| l.locked).map(|l| l.target_id).collect();
                    (locked_ids, Some(sys_id))
                }
                _ => (Vec::new(), None),
            };
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, None);
            crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(ctx, ship_id, char_id, None);
            if let Some(sys_id) = sys_id_opt {
                if !cleared_targets.is_empty() {
                    let target_chars: Vec<i64> = ctx.db.with_snapshot(|s| {
                        cleared_targets
                            .iter()
                            .filter_map(|tid| {
                                let oid = s.items.get(tid)?.owner_id;
                                if oid != char_id && s.characters.get(&oid).is_some_and(|c| c.station_id.is_none() && c.solar_system_id == sys_id) {
                                    Some(oid)
                                } else {
                                    None
                                }
                            })
                            .collect()
                    });
                    for target_char in target_chars {
                        let _ = ctx.notify_target_events(target_char, &[("otherlost", ship_id, "")]);
                    }
                    let _ = ctx.notify_target_events(char_id, &[("clear", 0, "")]);
                }
            }
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnTargetClear {
                    targets_to_skip: None,
                },
            );
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.GetTargets
        ("dogmaIM", "GetTargets") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let ids = locks
                        .into_iter()
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::List(ids)));
                }
            }
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "GetTargeters") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let view = sys.view();
                    let targeter_ids: Vec<PyRep> = view
                        .targeters_of(ship_id)
                        .map(PyRep::Integer)
                        .collect();
                    return Some(Ok(PyRep::List(targeter_ids)));
                }
            }
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "LinkWeapons") | ("charFittingMgr", "LinkWeapons") => {
            run_weapon_pair_mutation(
                ctx,
                session,
                args,
                "masterID",
                "slaveID",
                true,
                |banks, master_id, slave_id| {
                    let mut destroyed = Vec::new();
                    let target_master = find_weapon_bank_master(banks, master_id).unwrap_or(master_id);
                    if find_weapon_bank_master(banks, slave_id) != Some(target_master) {
                        peel_module_from_weapon_banks(banks, slave_id, &mut destroyed);
                        let target_master =
                            find_weapon_bank_master(banks, master_id).unwrap_or(master_id);
                        let bank = banks
                            .entry(target_master)
                            .or_insert_with(|| vec![target_master]);
                        if !bank.contains(&target_master) {
                            bank.insert(0, target_master);
                        }
                        if !bank.contains(&slave_id) {
                            bank.push(slave_id);
                        }
                    }
                    destroyed
                },
            )
        }

        ("dogmaIM", "PeelAndLink") | ("charFittingMgr", "PeelAndLink") => {
            run_weapon_pair_mutation(
                ctx,
                session,
                args,
                "newModID",
                "targetBankModID",
                false,
                |banks, new_mod_id, target_bank_mod_id| {
                    let mut destroyed = Vec::new();
                    let target_before = find_weapon_bank_master(banks, target_bank_mod_id);
                    let src_before = find_weapon_bank_master(banks, new_mod_id);
                    if target_before.is_some() && src_before == target_before {
                        return destroyed;
                    }
                    peel_module_from_weapon_banks(banks, new_mod_id, &mut destroyed);
                    let target_master =
                        find_weapon_bank_master(banks, target_bank_mod_id).unwrap_or(target_bank_mod_id);
                    let list = banks
                        .entry(target_master)
                        .or_insert_with(|| vec![target_master]);
                    if !list.contains(&target_master) {
                        list.insert(0, target_master);
                    }
                    if !list.contains(&new_mod_id) {
                        list.push(new_mod_id);
                    }
                    destroyed
                },
            )
        }

        ("dogmaIM", "MergeModuleGroups") | ("charFittingMgr", "MergeModuleGroups") => {
            run_weapon_pair_mutation(
                ctx,
                session,
                args,
                "masterID",
                "otherMasterID",
                false,
                |banks, master_id, other_master_id| {
                    let target_master = find_weapon_bank_master(banks, master_id).unwrap_or(master_id);
                    let other_before = find_weapon_bank_master(banks, other_master_id);
                    if other_before == Some(target_master) {
                        return Vec::new();
                    }
                    let mut destroyed = Vec::new();
                    let other_modules = match other_before {
                        Some(other_master) => {
                            destroyed.push(other_master);
                            banks
                                .remove(&other_master)
                                .unwrap_or_else(|| vec![other_master_id])
                        }
                        None => vec![other_master_id],
                    };
                    let master_list = banks
                        .entry(target_master)
                        .or_insert_with(|| vec![target_master]);
                    if !master_list.contains(&target_master) {
                        master_list.insert(0, target_master);
                    }
                    for id in other_modules {
                        if !master_list.contains(&id) {
                            master_list.push(id);
                        }
                    }
                    destroyed
                },
            )
        }

        ("dogmaIM", "UnlinkModule") | ("charFittingMgr", "UnlinkModule") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let mod_id = match rpc.req_positive_i64(1, "moduleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}"), client_error: None });
                }
                let mut destroyed = Vec::new();
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    for list in banks.values_mut() {
                        list.retain(|&id| id != mod_id);
                    }
                    destroyed = banks.iter().filter(|(k, v)| **k == mod_id || v.len() <= 1).map(|(k, _)| *k).collect();
                    destroyed.sort_unstable();
                    banks.retain(|k, v| *k != mod_id && v.len() > 1);
                }
                Ok((destroyed, crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id)))
            });
            Some(res.map(|(destroyed, info)| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "DestroyWeaponBank") | ("charFittingMgr", "DestroyWeaponBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let master_id = match rpc.req_positive_i64(1, "masterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}"), client_error: None });
                }
                let removed = snap.weapon_banks.get_mut(&ship_id).and_then(|b| b.remove(&master_id)).is_some();
                let destroyed = if removed { vec![master_id] } else { Vec::new() };
                Ok((destroyed, crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id)))
            });
            Some(res.map(|(destroyed, info)| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "LinkAllWeapons") | ("charFittingMgr", "LinkAllWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = match crate::router::RpcArgs::positional(args).opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let res = crate::services::combat_crimewatch::combat_weapons_spool::handle_link_all_weapons_atomic(
                ctx,
                Some(session),
                char_id,
                ship_id,
                true,
            );
            Some(res)
        }

        ("dogmaIM", "UnlinkAllModules") | ("charFittingMgr", "UnlinkAllModules") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = match crate::router::RpcArgs::positional(args).opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}"), client_error: None });
                }
                let mut destroyed: Vec<i64> = snap.weapon_banks.remove(&ship_id).map(|b| b.into_keys().collect()).unwrap_or_default();
                destroyed.sort_unstable();
                Ok(destroyed)
            });
            Some(res.map(|destroyed| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &PyRep::Dict(Vec::new()));
                PyRep::None
            }))
        }

        ("dogmaIM", "LoadAmmo") => Some(crate::services::notify_emitters_seasons_tactical::handle_load_ammo(ctx, session, args)),

        ("dogmaIM", "UnloadAmmo") => Some(handle_unload_ammo(ctx, session, args)),

        ("dogmaIM", "GetLayerDamageValuesByItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
                arg => crate::services::combat_crimewatch::extract_id_collection(arg)
                    .into_iter()
                    .filter(|id| *id > 0)
                    .collect(),
            };
            if item_ids.is_empty() {
                if let Some(sid) = session.ship_id {
                    item_ids.push(sid);
                } else {
                    item_ids.push(1);
                }
            }
            // Issue #990: V24.01 `eveDrones/droneDamageTracker.pyj` & `eveInflight/damageStateValue.pyj`
            // unpack `shieldInfo` as `(shieldRatio, tauMs = rechargeMs / 5.0, simTimeFt)` and evaluate
            // `exp((time - blue.os.GetSimTime()) / 10000.0 / tau)`. Anchor `sim_ft` to real OS wallclock
            // FILETIME ticks (`host_wall_unix_secs_and_nanos`) so GM virtual-clock advances without
            // `DoSimClockRebase` never push `time` into the client's future and overflow `math.exp`.
            let (secs, nanos) = eve_db::clock::host_wall_unix_secs_and_nanos();
            let sim_ft = (secs as i64 + 11_644_473_600) * 10_000_000 + (nanos as i64 / 100);
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                item_ids
                    .into_iter()
                    .map(|iid| {
                        let (armor_dmg, hull_dmg, shield_cur, shield_cap, recharge_ms) = snap
                            .items
                            .get(&iid)
                            .filter(|it| it.owner_id == char_id || session.has_gm_role())
                            .map(|it| {
                                let state = crate::sp6_lifecycle::read_unified_ship_damage_state(
                                    ctx,
                                    snap,
                                    iid,
                                    it.type_id,
                                    None,
                                );
                                let recharge = it
                                    .dynamic_attributes
                                    .get(&479)
                                    .copied()
                                    .or_else(|| {
                                        ctx.sde
                                            .types
                                            .get(&it.type_id)
                                            .and_then(|t| t.default_attributes.get(&479).copied())
                                    })
                                    .filter(|&v| v > 0.0)
                                    .unwrap_or(100_000.0);
                                (
                                    state.armor_damage,
                                    state.hull_damage,
                                    state.shield_hp,
                                    state.max_shield,
                                    recharge,
                                )
                            })
                            .unwrap_or((0.0, 0.0, 400.0, 400.0, 100_000.0));
                        let shield_ratio = (shield_cur / shield_cap.max(1.0)).clamp(0.0, 1.0);
                        let tau_ms = (recharge_ms / 5.0).max(1.0);
                        let kv = PyRep::keyval([
                            (
                                "shieldInfo",
                                PyRep::Tuple(vec![
                                    PyRep::Real(shield_ratio),
                                    PyRep::Real(tau_ms),
                                    PyRep::Integer(sim_ft),
                                ]),
                            ),
                            ("armorDamage", PyRep::Real(armor_dmg)),
                            ("hullDamage", PyRep::Real(hull_dmg)),
                        ]);
                        (PyRep::Integer(iid), kv)
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "fitting_dogma_im_tests.rs"]
mod fitting_dogma_im_tests;

