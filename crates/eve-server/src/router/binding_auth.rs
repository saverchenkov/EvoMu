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
        "corpRegistry" | "corpmgr" => object_id > 0 && session.corporation_id == Some(object_id),
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
    match entry.service.as_str() {
        "i2" => session.has_gm_role() && entry.object_id > 0,
        "beyonce" | "scanMgr" => session.solar_system_id.map(i64::from) == Some(entry.object_id),
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => {
            session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
        }
        "invbroker" => authorize_invbroker_binding(ctx, session, entry.object_id),
        "corpRegistry" | "corpmgr" => session.corporation_id == Some(entry.object_id),
        "allianceRegistry" => session.alliance_id == Some(entry.object_id),
        "fleetObjectHandler" => session.fleet_id == Some(entry.object_id),
        "planetMgr" => authorize_planet_binding(ctx, session, entry.character_id, entry.object_id),
        "structure" => authorize_structure_binding(ctx, session, entry.object_id),
        "posMgr" => authorize_pos_binding(ctx, session, entry.object_id),
        "trademgr" => authorize_trade_binding(ctx, session, entry.object_id),
        "warRegistry" | "warStatisticMgr" => {
            session.corporation_id == Some(entry.object_id)
                || session.alliance_id == Some(entry.object_id)
        }
        "devIndexManager" => {
            session.solar_system_id.map(i64::from) == Some(entry.object_id)
                || session.constellation_id.map(i64::from) == Some(entry.object_id)
                || session.region_id.map(i64::from) == Some(entry.object_id)
        }
        "voucher" => {
            entry.object_id == entry.character_id
                || session.corporation_id == Some(entry.object_id)
        }
        "contractProxy" => {
            entry.object_id == entry.character_id
                || session.corporation_id == Some(entry.object_id)
                || session.station_id == Some(entry.object_id)
                || session.structure_id == Some(entry.object_id)
                || session.solar_system_id.map(i64::from) == Some(entry.object_id)
        }
        _ => true,
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
