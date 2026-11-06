//! `structure_profiles_directory` — V24 `structureDirectory` queries
//! (`GetMyDockableStructures`, `GetMyCharacterStructures`, `GetMyCorporationStructures`,
//! `CheckMyDockingAccessToStructures`, `GetStructureMapData`,
//! `GetMyAccessibleOnlineCynoBeaconStructures`, `GetSolarSystemsWithBeacons`, `GetValidWarHQs`).
#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::{
    structure_lifecycle as life,
    structure_upwell_ops::{
        evaluate_character_structure_access_in_snap, sync_pending_profiles_in_ctx,
    },
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeSet, HashMap};

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: feat:structures.structure-directory, feat:structures.docking-acl
        ("structureDirectory", "GetMyDockableStructures") => {
            Some(handle_get_my_dockable_structures(ctx, session, args))
        }
        ("structureDirectory", "GetMyCharacterStructures") => {
            Some(super::structure_lifecycle_rpc::handle_get_structures(ctx, session, false))
        }
        ("structureDirectory", "GetMyCorporationStructures") => {
            Some(super::structure_lifecycle_rpc::handle_get_structures(ctx, session, true))
        }
        ("structureDirectory", "CheckMyDockingAccessToStructures") => {
            Some(handle_check_my_docking_access(ctx, session, args))
        }
        ("structureDirectory", "GetStructureMapData") => {
            Some(handle_get_structure_map_data(ctx, session, args))
        }
        ("structureDirectory", "GetMyAccessibleOnlineCynoBeaconStructures") => {
            Some(handle_get_accessible_cyno_beacons(ctx, session))
        }
        ("structureDirectory", "GetSolarSystemsWithBeacons") => {
            Some(handle_get_solar_systems_with_beacons(ctx, session))
        }
        ("structureDirectory", "GetValidWarHQs") => Some(handle_get_valid_war_hqs(ctx, session)),
        _ => None,
    }
}

pub fn handle_get_my_dockable_structures(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    let sys_id = args
        .first()
        .and_then(|a| a.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .or(session.solar_system_id)
        .unwrap_or(0);
    let ids = ctx.db.with_snapshot(|snap| {
        let mut v: Vec<i64> = snap
            .structures
            .values()
            .filter(|s| {
                s.solar_system_id == sys_id
                    && s.state != life::STATE_DESTROYED
                    && !matches!(s.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring")
                    && !matches!(s.type_id, 35840 | 35841 | 37534 | 81826)
                    && evaluate_character_structure_access_in_snap(snap, s.structure_id, char_id)
            })
            .map(|s| s.structure_id)
            .collect();
        v.sort_unstable();
        v.into_iter().map(PyRep::Integer).collect()
    });
    Ok(PyRep::List(ids))
}

fn handle_check_my_docking_access(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    let requested: Vec<i64> = match args.first() {
        Some(PyRep::List(xs) | PyRep::Tuple(xs) | PyRep::Set(xs)) => {
            xs.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(PyRep::Integer(sid)) => vec![*sid],
        _ => Vec::new(),
    };
    let accessible = ctx.db.with_snapshot(|snap| {
        let mut out: Vec<i64> = requested
            .into_iter()
            .filter(|&sid| {
                snap.structures.get(&sid).is_some_and(|s| {
                    s.state != life::STATE_DESTROYED
                        && !matches!(s.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring")
                        && !matches!(s.type_id, 35840 | 35841 | 37534 | 81826)
                        && evaluate_character_structure_access_in_snap(snap, sid, char_id)
                })
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out.into_iter().map(PyRep::Integer).collect()
    });
    Ok(PyRep::List(accessible))
}

fn handle_get_structure_map_data(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    let sys_id = args
        .first()
        .and_then(|a| a.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .or(session.solar_system_id)
        .unwrap_or(0);
    ctx.db.with_snapshot(|snap| {
        let mut list: Vec<&eve_db::UpwellStructureRecord> = snap
            .structures
            .values()
            .filter(|s| {
                s.solar_system_id == sys_id
                    && s.state != life::STATE_DESTROYED
                    && evaluate_character_structure_access_in_snap(snap, s.structure_id, char_id)
            })
            .collect();
        list.sort_by_key(|s| s.structure_id);
        let rows = list
            .into_iter()
            .map(|st| {
                let has_dock = !matches!(st.type_id, 35840 | 35841 | 37534 | 81826)
                    && evaluate_character_structure_access_in_snap(snap, st.structure_id, char_id);
                PyRep::keyval([
                    ("structureID", PyRep::Integer(st.structure_id)),
                    ("typeID", PyRep::Integer(i64::from(st.type_id))),
                    ("ownerID", PyRep::Integer(st.owner_corp_id)),
                    ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                    ("itemName", PyRep::String(st.name.clone())),
                    ("x", PyRep::Real(st.position[0])),
                    ("y", PyRep::Real(st.position[1])),
                    ("z", PyRep::Real(st.position[2])),
                    ("hasDockingAccess", PyRep::Bool(has_dock)),
                ])
            })
            .collect();
        Ok(PyRep::List(rows))
    })
}

fn handle_get_accessible_cyno_beacons(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        let mut entries = Vec::new();
        for st in snap.structures.values() {
            if st.type_id == 35840
                && life::is_operational(&st.state)
                && st.quantum_core_installed
                && st.fuel_blocks > 0
                && evaluate_character_structure_access_in_snap(snap, st.structure_id, char_id)
            {
                entries.push((
                    PyRep::Integer(st.structure_id),
                    PyRep::keyval([
                        ("structureID", PyRep::Integer(st.structure_id)),
                        ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
                        ("itemName", PyRep::String(st.name.clone())),
                        ("ownerID", PyRep::Integer(st.owner_corp_id)),
                        ("typeID", PyRep::Integer(i64::from(st.type_id))),
                    ]),
                ));
            }
        }
        Ok(PyRep::Dict(entries))
    })
}

fn handle_get_solar_systems_with_beacons(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        let systems: BTreeSet<i64> = snap
            .structures
            .values()
            .filter(|st| {
                st.type_id == 35840
                    && life::is_operational(&st.state)
                    && st.quantum_core_installed
                    && st.fuel_blocks > 0
                    && evaluate_character_structure_access_in_snap(snap, st.structure_id, char_id)
            })
            .map(|st| i64::from(st.solar_system_id))
            .collect();
        Ok(PyRep::List(systems.into_iter().map(PyRep::Integer).collect()))
    })
}

fn handle_get_valid_war_hqs(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    sync_pending_profiles_in_ctx(ctx);
    ctx.db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .structures
            .values()
            .filter(|st| {
                let sec = ctx
                    .sde
                    .solar_systems
                    .get(&st.solar_system_id)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0);
                sec >= 0.45
                    && life::is_operational(&st.state)
                    && !matches!(st.type_id, 35840 | 35841 | 37534 | 81826)
                    && evaluate_character_structure_access_in_snap(snap, st.structure_id, char_id)
            })
            .map(|st| st.structure_id)
            .collect();
        ids.sort_unstable();
        Ok(PyRep::List(ids.into_iter().map(PyRep::Integer).collect()))
    })
}

#[cfg(test)] #[path = "structure_profiles_directory_tests.rs"]
mod structure_profiles_directory_tests;

