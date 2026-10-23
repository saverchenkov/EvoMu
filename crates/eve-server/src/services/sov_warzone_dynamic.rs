//! `sov_warzone_dynamic` — Dynamic Resource / ESS Cache, Sovereignty Hub Ops, and FW Warzone Query Handlers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::services::fw_services::{
    compute_all_frontline_statuses, STATUS_REARGUARD,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn build_warzone_occupation_state(
    _ctx: &ServerContext,
    sys_id: u32,
    contested: f64,
    owner_fac: i64,
    occupier_fac: i64,
    frontline: i64,
) -> PyRep {
    PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("ownerFactionID", PyRep::Integer(owner_fac)),
        ("occupierFactionID", PyRep::Integer(occupier_fac)),
        ("frontlineStatus", PyRep::Integer(frontline)),
        ("contestedPercentage", PyRep::Real(contested)),
    ])
}

fn collect_all_warzone_systems(ctx: &ServerContext) -> Vec<(u32, f64, i64, i64, i64)> {
    ctx.db.with_snapshot(|snap| {
        let frontlines = compute_all_frontline_statuses(ctx, snap);
        let mut list = Vec::new();
        for (&sys_id, occ) in &snap.fw_system_occupations {
            let contested = snap
                .system_states
                .get(&sys_id)
                .map(|st| st.fw_contested_pct)
                .unwrap_or_else(|| {
                    let threshold = (occ.victory_points_threshold as f64).max(1.0);
                    let vp = snap
                        .fw_runtime
                        .system_victory_points
                        .get(&sys_id)
                        .copied()
                        .unwrap_or(occ.victory_points);
                    ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0)
                });
            let frontline = frontlines.get(&sys_id).copied().unwrap_or(STATUS_REARGUARD);
            list.push((sys_id, contested, occ.owner_faction_id, occ.occupier_faction_id, frontline));
        }
        list
    })
}

fn dynamic_resource_settings() -> PyRep {
    PyRep::Dict(vec![
        (
            PyRep::String("bountyRiskModifierMax".into()),
            PyRep::Real(2.0),
        ),
        (
            PyRep::String("bountyRiskModifierMin".into()),
            PyRep::Real(0.5),
        ),
        (
            PyRep::String("bountyRiskModifierEquilibrium".into()),
            PyRep::Real(1.15),
        ),
        (PyRep::String("mainBankShare".into()), PyRep::Real(0.05)),
        (PyRep::String("reserveBankShare".into()), PyRep::Real(0.05)),
        (
            PyRep::String("linkDurationSeconds".into()),
            PyRep::Integer(300),
        ),
        (
            PyRep::String("linkDistanceMeters".into()),
            PyRep::Real(2500.0),
        ),
        (
            PyRep::String("autoPayoutTimeSeconds".into()),
            PyRep::Integer(10800),
        ),
        (
            PyRep::String("reserveBankUnlockDurationSeconds".into()),
            PyRep::Integer(2700),
        ),
        (
            PyRep::String("reserveBankKeyGroupIDs".into()),
            PyRep::List(vec![PyRep::Integer(4102)]),
        ),
    ])
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dynamicResourceCacheMgr", "GetDBSMapData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let map = ctx.db.with_snapshot(|snap| {
                let mut entries = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    let sec = ctx
                        .sde
                        .solar_systems
                        .get(&sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if sec <= 0.0 && sys_id < 31_000_000 {
                        entries.push((
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Real(st.dbs_multiplier),
                        ));
                    }
                }
                if entries.is_empty() {
                    if let Some(sys_id) = session.solar_system_id {
                        let sec = ctx
                            .sde
                            .solar_systems
                            .get(&sys_id)
                            .map(|s| s.security_status)
                            .unwrap_or(1.0);
                        if sec <= 0.0 && sys_id < 31_000_000 {
                            entries.push((PyRep::Integer(i64::from(sys_id)), PyRep::Real(1.35)));
                        }
                    }
                }
                entries
            });
            Some(Ok(PyRep::Dict(map)))
        }

        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(dynamic_resource_settings()))
        }

        ("dynamicResourceCacheMgr", "GetESSAgencyData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_u = ctx.now_unix();
            let unlocked_map =
                with_combat_runtime(ctx, |rt| rt.ess_reserve_unlocked_until.clone());
            let entries = ctx.db.with_snapshot(|snap| {
                let mut list = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    let sec = ctx
                        .sde
                        .solar_systems
                        .get(&sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if sec <= 0.0 && sys_id < 31_000_000 {
                        let unlocked_until = unlocked_map.get(&sys_id).copied().unwrap_or(0);
                        list.push(PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("mainBankValue", PyRep::Real(st.ess_main_bank.to_wire_f64())),
                            ("reserveBankValue", PyRep::Real(st.ess_reserve_bank.to_wire_f64())),
                            ("dbsMultiplier", PyRep::Real(st.dbs_multiplier)),
                            (
                                "linkedCharacterID",
                                st.ess_linked_character_id
                                    .map(PyRep::Integer)
                                    .unwrap_or(PyRep::None),
                            ),
                            ("isLinked", PyRep::Bool(st.ess_linked_character_id.is_some())),
                            ("unlockedUntil", PyRep::Integer(unlocked_until as i64)),
                            ("isUnlocked", PyRep::Bool(unlocked_until > now_u)),
                        ]));
                    }
                }
                list
            });
            Some(Ok(PyRep::List(entries)))
        }

        ("dynamicResourceCacheMgr", "GetESSReserveBankKeys") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut keys = Vec::new();
            for tid in 57714..=57721 {
                let name = ctx
                    .sde
                    .types
                    .get(&tid)
                    .map(|t| t.name.as_str())
                    .unwrap_or("ESS Reserve Security Key");
                keys.push(PyRep::keyval([
                    ("typeID", PyRep::Integer(i64::from(tid))),
                    ("typeName", PyRep::String(name.into())),
                    ("groupID", PyRep::Integer(4102)),
                    ("unlockDurationSeconds", PyRep::Integer(2700)),
                    ("pulseCount", PyRep::Integer(45)),
                ]));
            }
            Some(Ok(PyRep::List(keys)))
        }

        ("dynamicResourceCacheMgr", "GetESSSystemDetails") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "GetESSSystemDetails requires solarSystemID".into(),
                }));
            };
            let now_u = ctx.now_unix();
            let unlocked_until = with_combat_runtime(ctx, |rt| {
                rt.ess_reserve_unlocked_until
                    .get(&sys_id)
                    .copied()
                    .unwrap_or(0)
            });
            let rep = ctx.db.with_snapshot(|snap| {
                let st = snap.system_states.get(&sys_id);
                PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    (
                        "mainBankValue",
                        PyRep::Real(st.map(|s| s.ess_main_bank.to_wire_f64()).unwrap_or(0.0)),
                    ),
                    (
                        "reserveBankValue",
                        PyRep::Real(st.map(|s| s.ess_reserve_bank.to_wire_f64()).unwrap_or(0.0)),
                    ),
                    (
                        "dbsMultiplier",
                        PyRep::Real(st.map(|s| s.dbs_multiplier).unwrap_or(1.0)),
                    ),
                    (
                        "linkedCharacterID",
                        st.and_then(|s| s.ess_linked_character_id)
                            .map(PyRep::Integer)
                            .unwrap_or(PyRep::None),
                    ),
                    (
                        "linkStartedTick",
                        st.and_then(|s| s.ess_link_started_tick)
                            .map(|t| PyRep::Integer(t as i64))
                            .unwrap_or(PyRep::None),
                    ),
                    ("unlockedUntil", PyRep::Integer(unlocked_until as i64)),
                    ("isUnlocked", PyRep::Bool(unlocked_until > now_u)),
                ])
            });
            Some(Ok(rep))
        }

        ("dynamicResourceCacheMgr", "get") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_val = args.get(1).cloned().unwrap_or(PyRep::None);
            let Some(first_arg) = args.first() else {
                return Some(Ok(default_val));
            };
            if let Some(sys_id) = first_arg.as_i64().and_then(|v| u32::try_from(v).ok()) {
                let mult = ctx.db.with_snapshot(|snap| {
                    snap.system_states
                        .get(&sys_id)
                        .map(|s| PyRep::Real(s.dbs_multiplier))
                });
                Some(Ok(mult.unwrap_or(default_val)))
            } else if let Some(key_str) = first_arg.as_str() {
                if key_str == "settings" {
                    Some(Ok(dynamic_resource_settings()))
                } else {
                    Some(Ok(default_val))
                }
            } else {
                Some(Ok(default_val))
            }
        }

        ("dynamicResourceCacheMgr", "iteritems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let items = ctx.db.with_snapshot(|snap| {
                snap.system_states
                    .iter()
                    .map(|(&sys_id, st)| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(i64::from(sys_id)),
                            PyRep::Real(st.dbs_multiplier),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(items)))
        }

        ("sovMgr", "remove_from_cache") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("fwWarzoneSolarsystem", "iteritems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let items: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, contested, owner_fac, occupier_fac, frontline)| {
                    let st = build_warzone_occupation_state(
                        ctx,
                        sys_id,
                        contested,
                        owner_fac,
                        occupier_fac,
                        frontline,
                    );
                    PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), st])
                })
                .collect();
            Some(Ok(PyRep::List(items)))
        }

        ("fwWarzoneSolarsystem", "itervalues") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let values: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, contested, owner_fac, occupier_fac, frontline)| {
                    build_warzone_occupation_state(
                        ctx,
                        sys_id,
                        contested,
                        owner_fac,
                        occupier_fac,
                        frontline,
                    )
                })
                .collect();
            Some(Ok(PyRep::List(values)))
        }

        ("fwWarzoneSolarsystem", "keys") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let systems = collect_all_warzone_systems(ctx);
            let keys: Vec<PyRep> = systems
                .into_iter()
                .map(|(sys_id, _, _, _, _)| PyRep::Integer(i64::from(sys_id)))
                .collect();
            Some(Ok(PyRep::List(keys)))
        }

        _ => None,
    }
}
