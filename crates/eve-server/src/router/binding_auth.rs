//! Per-service context authorization checks for bound object monikers.

use crate::router::context::{BoundObjectEntry, ServerContext};
use eve_machonet::SessionState;

fn is_direct_location_or_ship(session: &SessionState, object_id: i64) -> bool {
    session.ship_id == Some(object_id)
        || session.station_id == Some(object_id)
        || session.structure_id == Some(object_id)
        || session.solar_system_id.map(i64::from) == Some(object_id)
}

/// All `36` service names handled by [`authorize_service_binding`].
pub const BINDABLE_SERVICES: &[&str] = &[
    "populationCap",
    "beyonce",
    "scanMgr",
    "entity",
    "ship",
    "dogmaIM",
    "invbroker",
    "station",
    "stationSvc",
    "corpStationMgr",
    "repairSvc",
    "reprocessingSvc",
    "insuranceSvc",
    "skillMgr2",
    "charMgr",
    "jumpCloneSvc",
    "crimewatch",
    "i2", // rpc-lint: allow internal bound object alias for invbroker
    "corpRegistry",
    "corpmgr",
    "officeManager",
    "allianceRegistry",
    "fleetObjectHandler",
    "planetMgr",
    "planetOrbitalRegistryBroker",
    "structure",
    "posMgr",
    "trademgr",
    "warRegistry",
    "warStatisticMgr",
    "devIndexManager",
    "voucher",
    "contractProxy",
    "keeper",
    "agentMgr",
    "tourneyMgr",
];

/// Verifies whether `session` (with active character `char_id`) is authorized
/// to bind `service` to `object_id`.
pub(super) fn authorize_service_binding(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    service: &str,
    object_id: i64,
) -> bool {
    if object_id < 0 {
        return false;
    }
    match service {
        "populationCap" => object_id == char_id,
        "beyonce" | "scanMgr" | "entity" => {
            session.solar_system_id.map(i64::from) == Some(object_id)
        }
        "ship" => is_direct_location_or_ship(session, object_id),
        "dogmaIM" => {
            is_direct_location_or_ship(session, object_id)
                || crate::router::client_compat::is_dogma_station_or_structure_accessible(
                    ctx, session, char_id, object_id,
                )
        }
        "invbroker" => authorize_invbroker_binding(ctx, session, char_id, object_id, false),
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => {
            session.station_id == Some(object_id) || session.structure_id == Some(object_id)
        }
        "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
            Some(object_id) == session.character_id
                || session.solar_system_id.map(i64::from) == Some(object_id)
                || session.station_id == Some(object_id)
                || session.structure_id == Some(object_id)
                || object_id == 0
        }
        "crimewatch" => is_authorized_crimewatch_target(ctx, session, object_id),
        "i2" => session.has_gm_role() && object_id > 0,
        "corpRegistry" | "corpmgr" => {
            object_id > 0
                && (session.corporation_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id))
        }
        "officeManager" => authorize_office_manager_binding(ctx, session, char_id, object_id),
        "allianceRegistry" => object_id > 0 && session.alliance_id == Some(object_id),
        "fleetObjectHandler" => object_id > 0 && session.fleet_id == Some(object_id),
        "planetMgr" => authorize_planet_binding(ctx, session, char_id, object_id),
        "planetOrbitalRegistryBroker" => {
            is_solar_system_id(ctx, object_id)
                && session.solar_system_id.map(i64::from) == Some(object_id)
        }
        "structure" => authorize_structure_binding(ctx, session, object_id),
        "posMgr" => authorize_pos_binding(ctx, session, object_id),
        "trademgr" => authorize_trade_binding(ctx, session, object_id),
        "warRegistry" => {
            object_id > 0
                && (session.corporation_id == Some(object_id)
                    || session.alliance_id == Some(object_id))
        }
        "warStatisticMgr" => authorize_war_stat_binding(ctx, session, char_id, object_id),
        "devIndexManager" => {
            object_id > 0
                && (session.solar_system_id.map(i64::from) == Some(object_id)
                    || session.constellation_id.map(i64::from) == Some(object_id)
                    || session.region_id.map(i64::from) == Some(object_id))
        }
        "voucher" => object_id > 0 && (object_id == char_id || session.corporation_id == Some(object_id)),
        "contractProxy" => {
            object_id > 0
                && (object_id == char_id
                    || session.corporation_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.solar_system_id.map(i64::from) == Some(object_id))
        }
        "keeper" => authorize_keeper_binding(ctx, session, object_id),
        "agentMgr" => authorize_agent_binding(ctx, object_id),
        "tourneyMgr" => authorize_tourney_binding(ctx, session, char_id, object_id),
        _ => false,
    }
}

/// Verifies whether a previously bound object entry remains valid in `session`.
pub(super) fn verify_service_binding_context(
    ctx: &ServerContext,
    session: &SessionState,
    entry: &BoundObjectEntry,
) -> bool {
    if entry.object_id < 0 {
        return false;
    }
    match entry.service.as_str() {
        "i2" => session.has_gm_role() && entry.object_id > 0,
        "populationCap" => entry.object_id == entry.character_id,
        "beyonce" | "scanMgr" | "entity" => {
            session.solar_system_id.map(i64::from) == Some(entry.object_id)
        }
        "ship" => is_direct_location_or_ship(session, entry.object_id),
        "dogmaIM" => {
            is_direct_location_or_ship(session, entry.object_id)
                || (Some(entry.character_id) == session.character_id
                    && crate::router::client_compat::is_dogma_station_or_structure_accessible(
                        ctx,
                        session,
                        entry.character_id,
                        entry.object_id,
                    ))
        }
        "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
            Some(entry.object_id) == session.character_id
                || session.solar_system_id.map(i64::from) == Some(entry.object_id)
                || session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
                || entry.object_id == 0
        }
        "crimewatch" => is_authorized_crimewatch_target(ctx, session, entry.object_id),
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => {
            session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
        }
        "invbroker" => {
            entry.object_id > 0
                && Some(entry.character_id) == session.character_id
                && authorize_invbroker_binding(ctx, session, entry.character_id, entry.object_id, true)
        }
        "corpRegistry" | "corpmgr" => {
            entry.object_id > 0
                && (session.corporation_id == Some(entry.object_id)
                    || session.station_id == Some(entry.object_id)
                    || session.structure_id == Some(entry.object_id))
        }
        "officeManager" => {
            authorize_office_manager_binding(ctx, session, entry.character_id, entry.object_id)
        }
        "allianceRegistry" => entry.object_id > 0 && session.alliance_id == Some(entry.object_id),
        "fleetObjectHandler" => entry.object_id > 0 && session.fleet_id == Some(entry.object_id),
        "planetMgr" => authorize_planet_binding(ctx, session, entry.character_id, entry.object_id),
        "planetOrbitalRegistryBroker" => {
            is_solar_system_id(ctx, entry.object_id)
                && session.solar_system_id.map(i64::from) == Some(entry.object_id)
        }
        "structure" => authorize_structure_binding(ctx, session, entry.object_id),
        "posMgr" => authorize_pos_binding(ctx, session, entry.object_id),
        "trademgr" => authorize_trade_binding(ctx, session, entry.object_id),
        "warRegistry" => {
            entry.object_id > 0
                && (session.corporation_id == Some(entry.object_id)
                    || session.alliance_id == Some(entry.object_id))
        }
        "warStatisticMgr" => {
            authorize_war_stat_binding(ctx, session, entry.character_id, entry.object_id)
        }
        "devIndexManager" => {
            entry.object_id > 0
                && (session.solar_system_id.map(i64::from) == Some(entry.object_id)
                    || session.constellation_id.map(i64::from) == Some(entry.object_id)
                    || session.region_id.map(i64::from) == Some(entry.object_id))
        }
        "voucher" => {
            entry.object_id > 0
                && (entry.object_id == entry.character_id
                    || session.corporation_id == Some(entry.object_id))
        }
        "contractProxy" => {
            entry.object_id > 0
                && (entry.object_id == entry.character_id
                    || session.corporation_id == Some(entry.object_id)
                    || session.station_id == Some(entry.object_id)
                    || session.structure_id == Some(entry.object_id)
                    || session.solar_system_id.map(i64::from) == Some(entry.object_id))
        }
        "keeper" => authorize_keeper_binding(ctx, session, entry.object_id),
        "agentMgr" => authorize_agent_binding(ctx, entry.object_id),
        "tourneyMgr" => {
            authorize_tourney_binding(ctx, session, entry.character_id, entry.object_id)
        }
        _ => false,
    }
}

/// Returns `true` when `entry` belongs to `char_id` and is invalidated by a session transition.
pub(super) fn should_invalidate_bound_object(
    ctx: &ServerContext,
    entry: &BoundObjectEntry,
    system_id: u32,
    active_dock_id: Option<i64>,
    reason: &str,
) -> bool {
    if matches!(reason, "set_account_key" | "login") {
        return false;
    }
    let char_id = entry.character_id;
    let oid = entry.object_id;
    let (
        ship_id,
        corp_id,
        alliance_id,
        fleet_id,
        dock_is_structure,
        oid_is_structure,
        struct_in_ctx,
        inv_in_ctx,
    ) = ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id);
            let ship_id = ch.map(|c| c.active_ship_id).filter(|&id| id > 0);
            let corp_id = ch.map(|c| c.corporation_id).filter(|&id| id > 0);
            let alliance_id = ch.and_then(|c| c.alliance_id);
            let fleet_id = ch.and_then(|c| c.fleet_id);
            let dock_is_structure =
                active_dock_id.is_some_and(|did| snap.structures.contains_key(&did));
            let oid_is_structure = snap.structures.contains_key(&oid);
            let struct_in_ctx = active_dock_id == Some(oid)
                || snap
                    .structures
                    .get(&oid)
                    .is_some_and(|st| st.solar_system_id == system_id || Some(st.owner_corp_id) == corp_id);
            let station_id = active_dock_id.filter(|_| !dock_is_structure);
            let structure_id = active_dock_id.filter(|_| dock_is_structure);
            let inv_in_ctx = oid == char_id
                || ship_id == Some(oid)
                || active_dock_id == Some(oid)
                || ((active_dock_id.is_none() || dock_is_structure) && oid == i64::from(system_id))
                || ctx
                    .sde
                    .stations
                    .get(&oid)
                    .is_some_and(|st| st.solar_system_id == system_id)
                || snap
                    .structures
                    .get(&oid)
                    .is_some_and(|st| st.solar_system_id == system_id)
                || snap.items.get(&oid).is_some_and(|it| {
                    station_id == Some(it.location_id)
                        || structure_id == Some(it.location_id)
                        || ship_id == Some(it.location_id)
                        || (active_dock_id.is_none() && it.location_id == i64::from(system_id))
                });
            (
                ship_id,
                corp_id,
                alliance_id,
                fleet_id,
                dock_is_structure,
                oid_is_structure,
                struct_in_ctx,
                inv_in_ctx,
            )
        });
    let _ = dock_is_structure;
    let inv_in_ctx = inv_in_ctx
        || (entry.service == "invbroker"
            && crate::services::inventory_bound::resolve_missing_ship_stub_type_id(
                ctx,
                char_id,
                oid,
                true,
                active_dock_id.is_some(),
                ship_id,
            )
            .is_some());

    if reason == "corp_change" {
        return match entry.service.as_str() {
            "corpRegistry" | "corpmgr" => corp_id.is_some_and(|cid| oid != cid),
            "allianceRegistry" => alliance_id != Some(oid),
            "warRegistry" => {
                corp_id.is_some_and(|cid| oid != cid && alliance_id != Some(oid))
            }
            "officeManager" => corp_id.is_some_and(|cid| oid != cid && active_dock_id != Some(oid)),
            "voucher" => corp_id.is_some_and(|cid| oid != cid && oid != char_id),
            "contractProxy" => {
                corp_id.is_some_and(|cid| {
                    oid != cid
                        && oid != char_id
                        && active_dock_id != Some(oid)
                        && oid != i64::from(system_id)
                })
            }
            "structure" | "posMgr" => oid_is_structure && !struct_in_ctx,
            _ => false,
        };
    }
    if reason == "fleet_change" {
        return entry.service == "fleetObjectHandler" && fleet_id != Some(oid);
    }
    if matches!(reason, "board_or_eject" | "ship_loss" | "clone_swap") {
        return match entry.service.as_str() {
            "ship" | "dogmaIM" => {
                ship_id.is_some_and(|sid| oid != sid)
                    && active_dock_id != Some(oid)
                    && oid != i64::from(system_id)
            }
            "invbroker" => !inv_in_ctx,
            "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
            | "insuranceSvc" => active_dock_id != Some(oid),
            "officeManager" => active_dock_id != Some(oid) && corp_id != Some(oid),
            "structure" => !struct_in_ctx,
            "beyonce" | "scanMgr" | "entity" => oid != i64::from(system_id),
            _ => false,
        };
    }

    let is_docked = active_dock_id.is_some()
        || matches!(
            reason,
            "dock" | "podding" | "clone_jump" | "turbo_dock" | "self_destruct_clone"
        );
    let is_space_jump = matches!(
        reason,
        "stargate_jump"
            | "wormhole_jump"
            | "jump_drive"
            | "cyno_jump"
            | "filamentJump"
            | "filament_jump"
            | "abyssal_entry"
            | "abyssal_exit"
            | "void_entry"
            | "void_exit"
            | "pvp_arena_entry"
            | "pvp_arena_exit"
            | "jump_bridge"
            | "shipcaster"
            | "tourney_teleport"
    );

    match entry.service.as_str() {
        "beyonce" | "scanMgr" | "entity" => is_docked || is_space_jump || oid != i64::from(system_id),
        "planetOrbitalRegistryBroker" => is_space_jump || oid != i64::from(system_id),
        "keeper" | "posMgr" | "devIndexManager" if (30_000_000..=39_999_999).contains(&oid) => {
            is_docked || is_space_jump || oid != i64::from(system_id)
        }
        "crimewatch" if oid != char_id && oid != 0 => {
            if is_docked && active_dock_id == Some(oid) {
                false
            } else {
                is_space_jump || oid != i64::from(system_id)
            }
        }
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => active_dock_id != Some(oid),
        "officeManager" => active_dock_id != Some(oid) && corp_id != Some(oid),
        "structure" => !struct_in_ctx,
        "ship" | "dogmaIM" => {
            ship_id.is_some_and(|sid| oid != sid)
                && active_dock_id != Some(oid)
                && oid != i64::from(system_id)
        }
        "invbroker" => !inv_in_ctx,
        "corpRegistry" | "corpmgr" => corp_id.is_some_and(|cid| oid != cid),
        "allianceRegistry" => corp_id.is_some() && alliance_id != Some(oid),
        "fleetObjectHandler" => corp_id.is_some() && fleet_id != Some(oid),
        "warRegistry" => {
            corp_id.is_some_and(|cid| oid != cid && alliance_id != Some(oid))
        }
        _ => false,
    }
}

fn fold_role_bits(bits: &[u64; 7]) -> u64 {
    bits.iter().copied().fold(0u64, |acc, bit| acc | bit)
}

fn has_corp_hangar_or_director_role(role_mask: u64) -> bool {
    let hangar_mask = fold_role_bits(&eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE)
        | fold_role_bits(&eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY)
        | fold_role_bits(&eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE);
    let deliveries_and_office_mask = eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE
        | eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_QUERY
        | eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE
        | eve_db::corp_roles::ROLE_STATION_MANAGER
        | eve_db::corp_roles::ROLE_FACTORY_MANAGER
        | eve_db::corp_roles::ROLE_TRADER;
    eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_DIRECTOR)
        || (role_mask & (hangar_mask | deliveries_and_office_mask)) != 0
}

fn is_foreign_ship_bay_reachable(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    it: &eve_db::InventoryItemRecord,
    docked: Option<i64>,
    foreign_ship_pos_pair: Option<(glam::DVec3, glam::DVec3)>,
    verify_only: bool,
) -> bool {
    let Some(t) = ctx.sde.types.get(&it.type_id) else {
        return false;
    };
    let (host_ship, host_type, check_sma, check_fh) = if t.category_id == 6 && it.owner_id != char_id {
        let has_sma = t.default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0;
        let has_fh = t.default_attributes.get(&912).copied().unwrap_or(0.0) > 0.0;
        if !has_sma && !has_fh {
            return false;
        }
        (it, t, has_sma, has_fh)
    } else if crate::services::fitting_containers_ships::is_container_group(t.group_id, it.type_id) {
        let Some((host, entry_flag)) = crate::services::inventory_bound::find_ship_bay_ancestor_in_snap(
            ctx,
            snap,
            it.location_id,
            it.flag as u16,
        ) else {
            return false;
        };
        if host.owner_id == char_id {
            return false;
        }
        let Some(ht) = ctx.sde.types.get(&host.type_id) else {
            return false;
        };
        let has_sma = matches!(entry_flag, 90 | 138)
            && ht.default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0;
        let has_fh =
            entry_flag == 155 && ht.default_attributes.get(&912).copied().unwrap_or(0.0) > 0.0;
        if !has_sma && !has_fh {
            return false;
        }
        (host, ht, has_sma, has_fh)
    } else {
        return false;
    };

    if !verify_only {
        let acl_ok = session.has_gm_role()
            || (check_sma
                && crate::services::fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                    snap, char_id, host_ship.item_id, host_ship.owner_id, true, None,
                ))
            || (check_fh
                && crate::services::fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                    snap, char_id, host_ship.item_id, host_ship.owner_id, false, None,
                ))
            || (it.item_id == host_ship.item_id && {
                let cfg = crate::services::fitting_containers_ships::ship_access_config_snap(snap, host_ship.item_id);
                cfg.password.as_deref().is_some_and(|p| !p.is_empty()) && {
                    let in_same_fleet = snap
                        .fleets
                        .values()
                        .any(|fl| fl.members.contains(&char_id) && fl.members.contains(&host_ship.owner_id));
                    let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
                    let c2 = snap.characters.get(&host_ship.owner_id).map(|c| c.corporation_id).or_else(|| {
                        snap.corporations
                            .contains_key(&host_ship.owner_id)
                            .then_some(host_ship.owner_id)
                    });
                    let same_corp =
                        c1.is_some_and(|cid| cid > 0 && (cid == host_ship.owner_id || c2 == Some(cid)));
                    (check_sma
                        && ((in_same_fleet && cfg.allow_fleet_smb)
                            || (same_corp && cfg.allow_corp_smb)))
                        || (check_fh
                            && ((in_same_fleet && cfg.allow_fleet_hangar)
                                || (same_corp && cfg.allow_corp_hangar)))
                }
            });
        if !acl_ok {
            return false;
        }
    }

    if let Some(dock_id) = docked {
        host_ship.location_id == dock_id
    } else if let Some(sys_id) = session.solar_system_id {
        if host_ship.location_id != i64::from(sys_id) {
            return false;
        }
        let caller_ship = session.ship_id.unwrap_or(0);
        let ship_r = host_type.radius.max(0.0);
        let caller_r = snap
            .items
            .get(&caller_ship)
            .and_then(|s| ctx.sde.types.get(&s.type_id))
            .map(|ct| ct.radius.max(0.0))
            .unwrap_or(0.0);
        let pos_pair = foreign_ship_pos_pair.or_else(|| {
            snap.item_positions
                .get(&caller_ship)
                .zip(snap.item_positions.get(&host_ship.item_id))
                .map(|(&a, &b)| (glam::DVec3::from(a), glam::DVec3::from(b)))
        });
        pos_pair
            .map(|(a, b)| (a.distance(b) - ship_r - caller_r).max(0.0) <= 2_500.0)
            .unwrap_or(false)
    } else {
        false
    }
}

fn is_item_chain_in_reach(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    it: &eve_db::InventoryItemRecord,
    hangar_loc_id: i64,
    corp_role_ok: bool,
) -> bool {
    let mut cur_loc = it.location_id;
    let mut visited = [0i64; 8];
    for idx in 0..visited.len() {
        if cur_loc == hangar_loc_id || Some(cur_loc) == session.ship_id {
            return true;
        }
        if visited[..idx].contains(&cur_loc) {
            return false;
        }
        visited[idx] = cur_loc;
        let Some(parent) = snap.items.get(&cur_loc) else {
            return false;
        };
        let parent_ok = session.has_gm_role()
            || parent.owner_id == char_id
            || (session.corporation_id.filter(|&c| c > 0) == Some(parent.owner_id)
                && (corp_role_ok || parent.type_id == 27));
        if !parent_ok {
            return false;
        }
        cur_loc = parent.location_id;
    }
    cur_loc == hangar_loc_id || Some(cur_loc) == session.ship_id
}

#[allow(clippy::too_many_arguments)]
fn authorize_invbroker_item_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    it: &eve_db::InventoryItemRecord,
    now_unix: u64,
    docked: Option<i64>,
    hangar_loc_id: i64,
    foreign_ship_pos_pair: Option<(glam::DVec3, glam::DVec3)>,
    verify_only: bool,
) -> bool {
    if docked.is_none()
        && it.type_id == 23
        && it.owner_id == 0
        && Some(it.location_id) == session.solar_system_id.map(i64::from)
    {
        return true;
    }
    if is_foreign_ship_bay_reachable(
        ctx,
        snap,
        session,
        char_id,
        it,
        docked,
        foreign_ship_pos_pair,
        verify_only,
    ) {
        return true;
    }
    let corp_role_ok = session.corporation_id.filter(|&cid| cid > 0).is_some_and(|cid| {
        let role_loc = docked.or(Some(it.location_id));
        let (is_ceo, role_mask) =
            snap.effective_corp_roles_at_db(cid, char_id, role_loc, now_unix);
        is_ceo || has_corp_hangar_or_director_role(role_mask)
    });
    if !is_item_chain_in_reach(snap, session, char_id, it, hangar_loc_id, corp_role_ok) {
        return false;
    }
    if session.has_gm_role() || it.owner_id == char_id {
        return true;
    }
    if let Some(corp_id) = session.corporation_id.filter(|&cid| cid > 0) {
        if it.owner_id == corp_id && corp_role_ok {
            return true;
        }
    }
    if verify_only
        && docked.is_some()
        && Some(it.location_id) == docked
        && !snap.corporations.contains_key(&it.owner_id)
        && ctx
            .sde
            .types
            .get(&it.type_id)
            .is_some_and(|t| crate::services::fitting_containers_ships::is_container_group(t.group_id, it.type_id))
    {
        return crate::services::fitting_containers_ships::container_password_snap(snap, it.item_id, 1).is_some()
            || crate::services::fitting_containers_ships::container_password_snap(snap, it.item_id, 2).is_some();
    }
    false
}

fn authorize_office_manager_binding(
    _ctx: &ServerContext,
    session: &SessionState,
    _char_id: i64,
    object_id: i64,
) -> bool {
    object_id > 0
        && session.station_id.or(session.structure_id).is_some()
        && (session.station_id == Some(object_id)
            || session.structure_id == Some(object_id)
            || session.corporation_id == Some(object_id))
}

fn authorize_war_stat_binding(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> bool {
    if object_id <= 0 {
        return false;
    }
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        if !snap.wars.contains_key(&object_id) {
            return false;
        }
        if session.has_gm_role() {
            return true;
        }
        let session_alliance = session.alliance_id.filter(|&a| a > 0);
        let corp_id = session.corporation_id.filter(|&id| id > 0).or_else(|| {
            snap.characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .filter(|&id| id > 0)
        });
        session_alliance.is_some_and(|aid| {
            crate::domain::wars::is_war_participant(snap, object_id, 0, Some(aid), now_unix)
        }) || corp_id.is_some_and(|my_corp| {
            crate::domain::wars::is_war_participant(snap, object_id, my_corp, None, now_unix)
        })
    })
}

fn authorize_keeper_binding(
    ctx: &ServerContext,
    session: &SessionState,
    object_id: i64,
) -> bool {
    if object_id <= 0 {
        return false;
    }
    if session.solar_system_id.map(i64::from) == Some(object_id) {
        return true;
    }
    session.has_gm_role()
        && u32::try_from(object_id)
            .ok()
            .is_some_and(|dungeon_id| ctx.sde.dungeons.contains_key(&dungeon_id))
}

fn authorize_agent_binding(ctx: &ServerContext, object_id: i64) -> bool {
    object_id > 0 && ctx.sde.agents.contains_key(&object_id)
}

fn match_includes_caller(m: &eve_db::TourneyMatch, session: &SessionState, char_id: i64) -> bool {
    session.has_gm_role()
        || m.referees.contains(&char_id)
        || m.red_players.iter().any(|p| p.character_id == char_id)
        || m.blue_players.iter().any(|p| p.character_id == char_id)
}

fn authorize_tourney_binding(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> bool {
    if object_id <= 0 {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        let store = &snap.tourney_state;
        if let Some(m) = store.matches.get(&object_id) {
            return match_includes_caller(m, session, char_id);
        }
        if store.tournaments.contains_key(&object_id) {
            return session.has_gm_role()
                || store
                    .matches
                    .values()
                    .any(|m| m.tourney_id == object_id && match_includes_caller(m, session, char_id));
        }
        if store.arena_systems.contains_key(&object_id) {
            return session.has_gm_role()
                || store.matches.values().any(|m| {
                    m.solar_system_id == object_id && match_includes_caller(m, session, char_id)
                });
        }
        false
    })
}

fn is_authorized_shipping_storage_in_space(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> bool {
    let Some(sys_id) = session.solar_system_id else {
        return false;
    };
    let authorized = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.contains(&(object_id, char_id))
    });
    if !authorized {
        return false;
    }
    let (has_cap, owner_ok) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&object_id);
        let has_cap = it
            .and_then(|item| ctx.sde.types.get(&item.type_id))
            .is_some_and(|t| t.capacity > 0.0);
        let owner = it.map(|i| i.owner_id).unwrap_or(0);
        let auth = session.has_gm_role()
            || owner == char_id
            || owner == 0
            || snap
                .fleets
                .values()
                .any(|f| f.members.contains(&char_id) && f.members.contains(&owner));
        (has_cap, auth)
    });
    if !(has_cap && owner_ok) {
        return false;
    }
    let ship_id = session.ship_id.unwrap_or(0);
    ctx.systems
        .get_or_spawn_if_runtime(sys_id)
        .and_then(|sys_handle| {
            let ship_pos = sys_handle.get_ball_position_sync(ship_id)?;
            let unit_pos = sys_handle.get_ball_position_sync(object_id)?;
            Some(ship_pos.distance(unit_pos) <= 5_000.0)
        })
        .unwrap_or(false)
}

fn resolve_foreign_ship_pos_pair(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> Option<(glam::DVec3, glam::DVec3)> {
    let sys_id = session.solar_system_id?;
    let caller_ship = session.ship_id.unwrap_or(0);
    let ball_target = ctx
        .db
        .with_snapshot(|snap| {
            let it = snap.items.get(&object_id)?;
            let t = ctx.sde.types.get(&it.type_id)?;
            if t.category_id == 6 && it.owner_id != char_id {
                return Some(it.item_id);
            }
            if crate::services::fitting_containers_ships::is_container_group(t.group_id, it.type_id)
            {
                let (host, _) = crate::services::inventory_bound::find_ship_bay_ancestor_in_snap(
                    ctx,
                    snap,
                    it.location_id,
                    it.flag as u16,
                )?;
                if host.owner_id != char_id {
                    return Some(host.item_id);
                }
            }
            None
        })
        .unwrap_or(object_id);
    ctx.systems
        .get_or_spawn_if_runtime(sys_id)
        .and_then(|sys_handle| {
            sys_handle
                .get_ball_position_sync(caller_ship)
                .zip(sys_handle.get_ball_position_sync(ball_target))
        })
}

fn authorize_invbroker_binding(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
    verify_only: bool,
) -> bool {
    if object_id <= 0 {
        return false;
    }
    let now_unix = ctx.now_unix();
    if Some(object_id) == session.character_id
        || Some(object_id) == session.ship_id
        || Some(object_id) == session.station_id
        || Some(object_id) == session.structure_id
        || (session.station_id.is_none()
            && session.solar_system_id.map(i64::from) == Some(object_id))
        || ctx
            .sde
            .stations
            .get(&object_id)
            .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
    {
        return true;
    }
    let docked = session.station_id.or(session.structure_id);
    let hangar_loc_id = docked
        .or_else(|| session.solar_system_id.map(i64::from))
        .unwrap_or(0);
    if docked.is_none() && is_authorized_shipping_storage_in_space(ctx, session, char_id, object_id)
    {
        return true;
    }
    let foreign_ship_pos_pair = if docked.is_none() {
        resolve_foreign_ship_pos_pair(ctx, session, char_id, object_id)
    } else {
        None
    };
    let in_snap = ctx.db.with_snapshot(|snap| {
        if let Some(st) = snap.structures.get(&object_id) {
            return Some(st.solar_system_id) == session.solar_system_id;
        }
        if let Some(rec) = snap.crab_beacons.beacons.get(&object_id) {
            let eff_now = now_unix.max(snap.pve_special_sites.sim_clock_sec);
            let in_loot_window = match rec.phase {
                crate::services::pve_crab_beacons::CrabBeaconPhase::LootRestricted => {
                    eff_now < rec.self_destruct_at_sec
                }
                crate::services::pve_crab_beacons::CrabBeaconPhase::Scanning => {
                    rec.scan_completes_at_sec > 0
                        && eff_now >= rec.scan_completes_at_sec
                        && eff_now < rec.self_destruct_at_sec
                }
                _ => false,
            };
            return in_loot_window
                && docked.is_none()
                && Some(rec.solar_system_id) == session.solar_system_id
                && (session.has_gm_role() || char_id == rec.deployer_char_id);
        }
        snap.items.get(&object_id).is_some_and(|it| {
            authorize_invbroker_item_in_snap(
                ctx,
                snap,
                session,
                char_id,
                it,
                now_unix,
                docked,
                hangar_loc_id,
                foreign_ship_pos_pair,
                verify_only,
            )
        })
    });
    if in_snap {
        return true;
    }
    session.user_id.is_some()
        && Some(char_id) == session.character_id
        && crate::services::inventory_bound::resolve_missing_ship_stub_type_id_for_session(
            ctx, session, char_id, object_id,
        )
        .is_some()
}

fn authorize_planet_binding(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> bool {
    if object_id <= 0 {
        return false;
    }
    session.solar_system_id.is_some_and(|sys_id| {
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|sys| sys.planet_ids.contains(&object_id))
    }) || ctx
        .db
        .with_snapshot(|snap| snap.planetary_colonies.contains_key(&(char_id, object_id)))
}

fn authorize_structure_binding(ctx: &ServerContext, session: &SessionState, object_id: i64) -> bool {
    if object_id <= 0 {
        return false;
    }
    session.structure_id == Some(object_id)
        || ctx.db.with_snapshot(|snap| {
            snap.structures.get(&object_id).is_some_and(|st| {
                Some(st.solar_system_id) == session.solar_system_id
                    || Some(st.owner_corp_id) == session.corporation_id
            })
        })
}

fn authorize_pos_binding(ctx: &ServerContext, session: &SessionState, object_id: i64) -> bool {
    if object_id <= 0 {
        return false;
    }
    session.solar_system_id.map(i64::from) == Some(object_id)
        || session.station_id == Some(object_id)
        || session.structure_id == Some(object_id)
        || session.corporation_id == Some(object_id)
        || ctx.db.with_snapshot(|snap| {
            snap.structures.get(&object_id).is_some_and(|st| {
                Some(st.solar_system_id) == session.solar_system_id
                    || Some(st.owner_corp_id) == session.corporation_id
            })
        })
}

fn authorize_trade_binding(ctx: &ServerContext, session: &SessionState, object_id: i64) -> bool {
    if object_id <= 0 {
        return false;
    }
    if crate::services::wallet_services_ext::is_active_trade_participant(ctx, session, object_id) {
        return true;
    }
    session.station_id == Some(object_id)
        || session.structure_id == Some(object_id)
        || session.solar_system_id.map(i64::from) == Some(object_id)
        || u32::try_from(object_id)
            .ok()
            .is_some_and(|rid| ctx.sde.regions.contains_key(&rid))
        || ctx
            .sde
            .stations
            .get(&object_id)
            .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
        || ctx.db.with_snapshot(|snap| {
            snap.structures.get(&object_id).is_some_and(|st| {
                Some(st.solar_system_id) == session.solar_system_id
                    || Some(st.owner_corp_id) == session.corporation_id
            })
        })
}

pub(super) fn is_solar_system_id(ctx: &ServerContext, object_id: i64) -> bool {
    ctx.resolve_solar_system_location_id(object_id).is_some()
}

fn is_authorized_crimewatch_target(
    ctx: &ServerContext,
    session: &SessionState,
    object_id: i64,
) -> bool {
    Some(object_id) == session.character_id
        || session.solar_system_id.map(i64::from) == Some(object_id)
        || session.station_id == Some(object_id)
        || session.structure_id == Some(object_id)
        || object_id == 0
        || session.station_id.and_then(|st| {
            ctx.sde
                .stations
                .get(&st)
                .map(|s| i64::from(s.solar_system_id))
        }) == Some(object_id)
        || session.structure_id.is_some_and(|sid| {
            ctx.db.with_snapshot(|snap| {
                snap.structures
                    .get(&sid)
                    .is_some_and(|st| i64::from(st.solar_system_id) == object_id)
            })
        })
}
