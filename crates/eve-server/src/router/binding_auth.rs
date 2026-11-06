//! Per-service context authorization checks for bound object monikers.

use crate::router::context::{BoundObjectEntry, ServerContext};
use eve_machonet::SessionState;

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
        "beyonce" | "scanMgr" => session.solar_system_id.map(i64::from) == Some(object_id),
        "dogmaIM" | "ship" => {
            session.ship_id == Some(object_id)
                || session.station_id == Some(object_id)
                || session.structure_id == Some(object_id)
                || session.solar_system_id.map(i64::from) == Some(object_id)
                || crate::router::client_compat::is_dogma_station_or_structure_accessible(
                    ctx, session, char_id, object_id,
                )
        }
        "invbroker" => authorize_invbroker_binding(ctx, session, object_id),
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
        "crimewatch" => {
            Some(object_id) == session.character_id
                || session.solar_system_id.map(i64::from) == Some(object_id)
                || session.station_id == Some(object_id)
                || object_id == 0
        }
        "i2" => session.has_gm_role() && object_id > 0,
        "corpRegistry" | "corpmgr" | "officeManager" => {
            object_id > 0
                && (session.corporation_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id))
        }
        "allianceRegistry" => object_id > 0 && session.alliance_id == Some(object_id),
        "fleetObjectHandler" => object_id > 0 && session.fleet_id == Some(object_id),
        "planetMgr" => authorize_planet_binding(ctx, session, char_id, object_id),
        "structure" => authorize_structure_binding(ctx, session, object_id),
        "posMgr" => authorize_pos_binding(ctx, session, object_id),
        "trademgr" => authorize_trade_binding(ctx, session, object_id),
        "warRegistry" | "warStatisticMgr" => {
            object_id > 0
                && (session.corporation_id == Some(object_id)
                    || session.alliance_id == Some(object_id))
        }
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
        "agentMgr" | "keeper" => object_id > 0,
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
        "beyonce" | "scanMgr" => session.solar_system_id.map(i64::from) == Some(entry.object_id),
        "dogmaIM" | "ship" => {
            session.ship_id == Some(entry.object_id)
                || session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
                || session.solar_system_id.map(i64::from) == Some(entry.object_id)
        }
        "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
            Some(entry.object_id) == session.character_id
                || session.solar_system_id.map(i64::from) == Some(entry.object_id)
                || session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
                || entry.object_id == 0
        }
        "crimewatch" => {
            Some(entry.object_id) == session.character_id
                || session.solar_system_id.map(i64::from) == Some(entry.object_id)
                || session.station_id == Some(entry.object_id)
                || entry.object_id == 0
        }
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => {
            session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
        }
        "invbroker" => authorize_invbroker_binding(ctx, session, entry.object_id),
        "corpRegistry" | "corpmgr" | "officeManager" => {
            entry.object_id > 0
                && (session.corporation_id == Some(entry.object_id)
                    || session.station_id == Some(entry.object_id)
                    || session.structure_id == Some(entry.object_id))
        }
        "allianceRegistry" => entry.object_id > 0 && session.alliance_id == Some(entry.object_id),
        "fleetObjectHandler" => entry.object_id > 0 && session.fleet_id == Some(entry.object_id),
        "planetMgr" => authorize_planet_binding(ctx, session, entry.character_id, entry.object_id),
        "structure" => authorize_structure_binding(ctx, session, entry.object_id),
        "posMgr" => authorize_pos_binding(ctx, session, entry.object_id),
        "trademgr" => authorize_trade_binding(ctx, session, entry.object_id),
        "warRegistry" | "warStatisticMgr" => {
            entry.object_id > 0
                && (session.corporation_id == Some(entry.object_id)
                    || session.alliance_id == Some(entry.object_id))
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
        "agentMgr" | "keeper" => entry.object_id > 0,
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
    let (ship_id, corp_id, alliance_id, fleet_id, dock_is_structure, struct_in_ctx, inv_in_ctx) =
        ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id);
            let ship_id = ch.map(|c| c.active_ship_id).filter(|&id| id > 0);
            let corp_id = ch.map(|c| c.corporation_id).filter(|&id| id > 0);
            let alliance_id = ch.and_then(|c| c.alliance_id);
            let fleet_id = ch.and_then(|c| c.fleet_id);
            let dock_is_structure =
                active_dock_id.is_some_and(|did| snap.structures.contains_key(&did));
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
                || (active_dock_id.is_none() && oid == i64::from(system_id))
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
                struct_in_ctx,
                inv_in_ctx,
            )
        });
    let _ = dock_is_structure;

    if reason == "corp_change" {
        return match entry.service.as_str() {
            "corpRegistry" | "corpmgr" => corp_id.is_some_and(|cid| oid != cid),
            "allianceRegistry" => alliance_id != Some(oid),
            "warRegistry" | "warStatisticMgr" => {
                corp_id.is_some_and(|cid| oid != cid && alliance_id != Some(oid))
            }
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
            | "jump_bridge"
            | "shipcaster"
    );

    match entry.service.as_str() {
        "beyonce" | "scanMgr" => is_docked || is_space_jump || oid != i64::from(system_id),
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
        "warRegistry" | "warStatisticMgr" => {
            corp_id.is_some_and(|cid| oid != cid && alliance_id != Some(oid))
        }
        _ => false,
    }
}

fn authorize_invbroker_binding(ctx: &ServerContext, session: &SessionState, object_id: i64) -> bool {
    Some(object_id) == session.character_id
        || Some(object_id) == session.ship_id
        || Some(object_id) == session.station_id
        || Some(object_id) == session.structure_id
        || (session.station_id.is_none()
            && session.structure_id.is_none()
            && session.solar_system_id.map(i64::from) == Some(object_id))
        || ctx
            .sde
            .stations
            .get(&object_id)
            .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
        || ctx.db.with_snapshot(|snap| {
            snap.structures
                .get(&object_id)
                .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
                || snap.items.get(&object_id).is_some_and(|it| {
                    Some(it.location_id) == session.station_id
                        || Some(it.location_id) == session.structure_id
                        || Some(it.location_id) == session.ship_id
                        || (session.station_id.is_none()
                            && session.structure_id.is_none()
                            && Some(it.location_id) == session.solar_system_id.map(i64::from))
                })
        })
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
