//! `sov_services` — Sovereignty Management, Equinox Skyhooks & Dev Indices (`§3.8`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
pub use eve_db::PersistedSovState as SovRuntimeState;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// SDE Type ID for Orbital Skyhook.
pub const TYPE_ORBITAL_SKYHOOK: u32 = 81080;
/// SDE Type ID for Sovereignty Hub.
pub const TYPE_SOVEREIGNTY_HUB: u32 = 32458;
/// SDE Type ID for Pharolux Cyno Beacon.
pub const TYPE_PHAROLUX_CYNO_BEACON: u32 = 35840;
pub const TYPE_TENEBREX_CYNO_JAMMER: u32 = 37534;

/// Reagents produced by Skyhooks: Magmatic Gas (81143) and Superionic Ice (81144).
pub const TYPE_MAGMATIC_GAS: u32 = 81143;
pub const TYPE_SUPERIONIC_ICE: u32 = 81144;

pub fn with_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut SovRuntimeState) -> R) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.sov_runtime))
}

pub fn read_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&SovRuntimeState) -> R) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.sov_runtime))
}

/// Computes the Activity Defense Multiplier (ADM) from military, industrial, and strategic indices.
///
/// Formula: ADM = 1.0 + mil_bonus + ind_bonus + strat_bonus, clamped between 1.0 and 6.0.
pub fn calculate_adm(mil_idx: u8, ind_idx: u8, strat_idx: u8) -> f64 {
    const MIL_IND_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
    const STRAT_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];
    let mil_b = MIL_IND_BONUS[usize::from(mil_idx.min(5))];
    let ind_b = MIL_IND_BONUS[usize::from(ind_idx.min(5))];
    let strat_b = STRAT_BONUS[usize::from(strat_idx.min(5))];
    ((1.0 + mil_b + ind_b + strat_b).clamp(1.0, 6.0) * 10.0).round() / 10.0
}

/// Scales base entosis capture duration by ADM.
pub fn scale_entosis_capture_seconds(base_seconds: u64, adm: f64) -> u64 {
    ((base_seconds as f64) * adm.max(1.0)).round() as u64
}

/// Broadcasts `OnSovereigntyChanged` to all sessions and queues destiny event for the solar system.
pub fn emit_sovereignty_changed(ctx: &ServerContext, sys_id: u32, alliance_id: Option<i64>, corp_id: Option<i64>) {
    ctx.notify(
        NotificationTarget::All,
        "OnSovereigntyChanged",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            corp_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ],
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let aid = alliance_id.unwrap_or(0);
    tokio::spawn(async move {
        let _ = handle.emit_destiny_event(0, eve_destiny::DestinyEvent::OnSovereigntyChanged {
            solar_system_id: sys_id,
            alliance_id: aid,
        }).await;
    });
}

/// Broadcasts `OnSolarSystemSovStructuresUpdated` notification.
pub fn emit_sov_structures_updated(ctx: &ServerContext, sys_id: u32, structs: Vec<PyRep>) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarSystemSovStructuresUpdated",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::List(structs),
        ],
    );
}

/// Broadcasts `OnSolarSystemDevIndexChanged` notification when indices increase.
pub fn emit_dev_index_changed(ctx: &ServerContext, sys_id: u32, dev_indices: &HashMap<String, i64>) {
    let dict_items: Vec<(PyRep, PyRep)> = dev_indices
        .iter()
        .map(|(k, v)| (PyRep::String(k.clone()), PyRep::Integer(*v)))
        .collect();
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarSystemDevIndexChanged",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Dict(dict_items),
        ],
    );
}

/// Marks a Skyhook as hackable and emits `OnSkyhookCanBeHacked`.
pub fn set_skyhook_hackable(ctx: &ServerContext, sys_id: u32, skyhook_id: i64, can_hack: bool) {
    with_sov_runtime(ctx, |rt| {
        rt.skyhook_hackable.insert(skyhook_id, can_hack);
        if can_hack {
            rt.skyhook_reagents.entry(skyhook_id).or_insert((TYPE_MAGMATIC_GAS, 500));
        }
    });
    if can_hack {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnSkyhookCanBeHacked",
            "solarsystemid",
            vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Integer(skyhook_id),
            ],
        );
    }
}

/// Sets the reagent type and quantity for a Skyhook in the runtime state.
pub fn set_skyhook_reagents(ctx: &ServerContext, skyhook_id: i64, reagent_type_id: u32, quantity: u64) {
    with_sov_runtime(ctx, |rt| {
        rt.skyhook_reagents.insert(skyhook_id, (reagent_type_id, quantity));
    });
}

/// Records a successful hack on a Sovereignty Hub, temporarily disabling upgrades and emitting `OnSovHubHacked`.
pub fn record_hub_hack(ctx: &ServerContext, sys_id: u32, hub_id: i64, disruption_seconds: u64) {
    let now = ctx.now_unix();
    let until = now + disruption_seconds;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.insert(hub_id, until);
        if let Some(st) = snap.structures.get_mut(&hub_id) {
            if st.state == "ShieldVulnerable" {
                st.state = "ArmorReinforced".into();
            }
            if st.reinforced_until_unix == 0 {
                st.reinforced_until_unix = until;
            }
        }
        snap.sov_campaign_scores.entry(hub_id).or_insert(
            eve_db::snapshot_ext::SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: 0.35,
                defender_score: 0.65,
            },
        );
    });
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSovHubHacked",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(hub_id),
        ],
    );
}

/// Checks whether a Sovereignty Hub is currently disrupted by a hack.
pub fn is_hub_hacked(ctx: &ServerContext, hub_id: i64) -> bool {
    let now = ctx.now_unix();
    read_sov_runtime(ctx, |rt| {
        rt.hacked_hubs.get(&hub_id).copied().is_some_and(|until| now < until)
    })
}

/// Claims sovereignty in `sys_id` for `alliance_id` with `hub_id`.
pub fn claim_system_sovereignty(
    ctx: &ServerContext,
    sys_id: u32,
    alliance_id: i64,
    corp_id: i64,
    _hub_id: i64,
) -> Result<(), BridgeRpcError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(st) = snap.system_states.get_mut(&sys_id) else {
            return Err(BridgeRpcError {
                code: "SystemNotFound".into(),
                message: format!("System {sys_id} not initialized"),
            });
        };
        if let Some(existing) = st.sov_alliance_id {
            if existing != alliance_id {
                return Err(BridgeRpcError {
                    code: "SovereigntyHeldByAnotherAlliance".into(),
                    message: format!("System {sys_id} sovereignty is held by alliance {existing}"),
                });
            }
        }
        st.sov_alliance_id = Some(alliance_id);
        st.military_index = st.military_index.max(1);
        st.industrial_index = st.industrial_index.max(1);
        st.strategic_index = st.strategic_index.max(1);
        Ok(())
    })?;

    emit_sovereignty_changed(ctx, sys_id, Some(alliance_id), Some(corp_id));
    let structs = collect_sov_structures(ctx, sys_id);
    emit_sov_structures_updated(ctx, sys_id, structs);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnCapitalSystemChanged",
        "solarsystemid",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(alliance_id),
            PyRep::None,
            PyRep::Integer(i64::from(sys_id)),
        ])],
    );
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSovereigntyAudioEvent",
        "solarsystemid",
        vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(sys_id))),
                (PyRep::String("allianceID".into()), PyRep::Integer(alliance_id)),
            ]),
        ],
    );
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "ProcessSovStatusChanged",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("allianceID", PyRep::Integer(alliance_id)),
                ("corporationID", PyRep::Integer(corp_id)),
            ]),
        ],
    );
    Ok(())
}

/// Claims sovereignty in `sys_id` and yields to allow destiny event emission.
pub async fn claim_system_sovereignty_async(
    ctx: &ServerContext,
    sys_id: u32,
    alliance_id: i64,
    corp_id: i64,
    hub_id: i64,
) -> Result<(), BridgeRpcError> {
    claim_system_sovereignty(ctx, sys_id, alliance_id, corp_id, hub_id)?;
    tokio::task::yield_now().await;
    Ok(())
}

fn collect_sov_structures(ctx: &ServerContext, sys_id: u32) -> Vec<PyRep> {
    ctx.db.with_snapshot(|snap| {
        let aid = snap.system_states.get(&sys_id).and_then(|s| s.sov_alliance_id);
        snap.structures
            .values()
            .filter(|s| s.solar_system_id == sys_id)
            .map(|s| {
                let vuln = snap
                    .structure_vulnerability_windows
                    .get(&s.structure_id)
                    .copied()
                    .unwrap_or((0, 0));
                PyRep::keyval([
                    ("itemID", PyRep::Integer(s.structure_id)),
                    ("structureID", PyRep::Integer(s.structure_id)),
                    ("typeID", PyRep::Integer(s.type_id as i64)),
                    ("corporationID", PyRep::Integer(s.owner_corp_id)),
                    ("allianceID", aid.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("vulnerableStartTime", PyRep::Integer(vuln.0 as i64)),
                    ("vulnerableEndTime", PyRep::Integer(vuln.1 as i64)),
                ])
            })
            .collect()
    })
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
        ("sovMgr", "SetSovHubFuelAccessGroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(hub_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "SetSovHubFuelAccessGroup requires hubID".into(),
                }));
            };
            let access_group_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            let is_corp_director = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    let has_role = char_id == corp.ceo_id
                        || eve_db::corp_roles::has_role(
                            role,
                            eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER,
                        );
                    if !has_role {
                        return false;
                    }
                } else {
                    return false;
                }
                snap.structures
                    .get(&hub_id)
                    .map(|st| st.owner_corp_id == corp_id)
                    .unwrap_or(false)
            });
            if !is_corp_director {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only owner corporation directors or station managers may set hub fuel access group".into(),
                }));
            }

            with_sov_runtime(ctx, |rt| {
                rt.fuel_access_groups.insert(hub_id, access_group_id);
            });
            Some(Ok(PyRep::Integer(access_group_id)))
        }

        ("sovMgr", "GetSovHubFuelAccessGroup") => {
            let Some(hub_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "GetSovHubFuelAccessGroup requires hubID".into(),
                }));
            };
            let group_id = read_sov_runtime(ctx, |rt| rt.fuel_access_groups.get(&hub_id).copied().unwrap_or(0));
            Some(Ok(PyRep::Integer(group_id)))
        }

        ("sovMgr", "IsOnLocalSovHubFuelAccessGroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(hub_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "IsOnLocalSovHubFuelAccessGroup requires hubID".into(),
                }));
            };
            let allowed = ctx.db.with_snapshot(|snap| {
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let char_alliance = snap.characters.get(&char_id).and_then(|c| c.alliance_id);
                if let Some(st) = snap.structures.get(&hub_id) {
                    if st.owner_corp_id == char_corp {
                        return true;
                    }
                    if let Some(hub_alliance) = snap.corporations.get(&st.owner_corp_id).and_then(|c| c.alliance_id) {
                        if Some(hub_alliance) == char_alliance {
                            return true;
                        }
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(allowed)))
        }

        ("sovMgr", "DestroySkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
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
                    message: "DestroySkyhooks requires a valid solarSystemID".into(),
                }));
            };

            let is_corp_director = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    char_id == corp.ceo_id
                        || eve_db::corp_roles::has_role(
                            role,
                            eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER,
                        )
                } else {
                    false
                }
            });
            if !is_corp_director {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only owner corporation directors or station managers may destroy skyhooks".into(),
                }));
            }

            let destroyed_count = ctx.db.with_mut_snapshot(|snap| {
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let skyhook_ids: Vec<i64> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == sys_id && s.type_id == TYPE_ORBITAL_SKYHOOK && s.owner_corp_id == char_corp)
                    .map(|s| s.structure_id)
                    .collect();
                for sid in &skyhook_ids {
                    snap.structures.remove(sid);
                }
                skyhook_ids.len()
            });

            let structs = collect_sov_structures(ctx, sys_id);
            emit_sov_structures_updated(ctx, sys_id, structs);
            Some(Ok(PyRep::Integer(destroyed_count as i64)))
        }

        ("colonyResourcesMgr", "GetMyCorporationSkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let skyhooks = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                snap.structures
                    .values()
                    .filter(|s| s.type_id == TYPE_ORBITAL_SKYHOOK && s.owner_corp_id == corp_id)
                    .map(|s| {
                        let reagents = snap
                            .sov_runtime
                            .skyhook_reagents
                            .get(&s.structure_id)
                            .copied()
                            .unwrap_or((0, 0));
                        PyRep::keyval([
                            ("skyhookID", PyRep::Integer(s.structure_id)),
                            ("structureID", PyRep::Integer(s.structure_id)),
                            ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                            ("corporationID", PyRep::Integer(s.owner_corp_id)),
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("state", PyRep::String(s.state.clone())),
                            ("reagentTypeID", PyRep::Integer(i64::from(reagents.0))),
                            ("reagentQuantity", PyRep::Integer(reagents.1 as i64)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(skyhooks)))
        }

        ("devIndexManager", "GetAllDevelopmentIndices") => {
            let res = ctx.db.with_snapshot(|snap| {
                let mut map = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    if st.sov_alliance_id.is_some() || st.military_index > 0 || st.industrial_index > 0 {
                        let indices = PyRep::keyval([
                            ("military", PyRep::Integer(i64::from(st.military_index))),
                            ("industrial", PyRep::Integer(i64::from(st.industrial_index))),
                            ("strategic", PyRep::Integer(i64::from(st.strategic_index))),
                            ("adm", PyRep::Real(calculate_adm(st.military_index, st.industrial_index, st.strategic_index))),
                        ]);
                        map.push((PyRep::Integer(i64::from(sys_id)), indices));
                    }
                }
                PyRep::Dict(map)
            });
            Some(Ok(res))
        }

        ("devIndexManager", "GetDevelopmentIndicesForSystem") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_sys = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| session.solar_system_id.map(i64::from))
                .unwrap_or(0);
            let Some(sys_id) = u32::try_from(raw_sys)
                .ok()
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Solar system {raw_sys} does not exist"),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let st = snap.system_states.get(&sys_id);
                let mil_lvl = st.map(|s| i64::from(s.military_index)).unwrap_or(0);
                let ind_lvl = st.map(|s| i64::from(s.industrial_index)).unwrap_or(0);
                let strat_lvl = st.map(|s| i64::from(s.strategic_index)).unwrap_or(0);
                let mil_pts = mil_lvl * 20_000;
                let ind_pts = ind_lvl * 20_000;
                let strat_pts = strat_lvl * 20_000;
                let inc = i64::from(st.and_then(|s| s.sov_alliance_id).is_some());
                let mk = |attr_id: i64, pts: i64, lvl: i64| {
                    (
                        PyRep::Integer(attr_id),
                        PyRep::keyval([
                            ("attributeID", PyRep::Integer(attr_id)),
                            ("points", PyRep::Integer(pts)),
                            ("level", PyRep::Integer(lvl)),
                            ("increasing", PyRep::Integer(inc)),
                        ]),
                    )
                };
                PyRep::Dict(vec![
                    mk(1583, mil_pts, mil_lvl),
                    mk(1584, ind_pts, ind_lvl),
                    mk(1615, strat_pts, strat_lvl),
                ])
            });
            Some(Ok(res))
        }

        ("warpVectorMgr", "UseVector") => {
            Some(handle_use_warp_vector(ctx, session, args).await)
        }

        ("map", "GetCurrentSovData") => {
            let sov_data = ctx.db.with_snapshot(|snap| {
                let list = snap
                    .system_states
                    .iter()
                    .filter_map(|(&sys_id, st)| {
                        st.sov_alliance_id.map(|aid| {
                            let hub_id = snap
                                .structures
                                .values()
                                .find(|s| {
                                    s.solar_system_id == sys_id
                                        && (s.type_id == TYPE_SOVEREIGNTY_HUB || s.type_id == 32226)
                                })
                                .map(|s| s.structure_id);
                            let corp_id = snap
                                .alliances
                                .get(&aid)
                                .map(|al| al.executor_corp_id)
                                .unwrap_or(0);
                            PyRep::keyval([
                                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                                ("allianceID", PyRep::Integer(aid)),
                                ("corporationID", PyRep::Integer(corp_id)),
                                ("infrastructureHubID", hub_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                                ("stationCount", PyRep::Integer(0)),
                            ])
                        })
                    })
                    .collect::<Vec<PyRep>>();
                list
            });
            Some(Ok(PyRep::List(sov_data)))
        }

        ("beyonce", "GetCynoJammerState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (is_jammed, cyno_suppressed, inhibitor_active) = ctx.db.with_snapshot(|snap| {
                let structure_jammed = snap.structures.values().any(|s| {
                    s.solar_system_id == sys_id
                        && (s.type_id == TYPE_TENEBREX_CYNO_JAMMER
                            || s.type_id == 81926
                            || ctx.sde.types.get(&s.type_id).is_some_and(|t| {
                                t.default_attributes.get(&3038).copied().unwrap_or(0.0) > 0.0
                            }))
                        && s.type_id != TYPE_PHAROLUX_CYNO_BEACON
                        && s.type_id != 35841
                        && s.state != "Destroyed"
                        && s.state != "Anchoring"
                        && s.fuel_blocks > 0
                });
                let item_inhibitor = snap.items.values().any(|i| {
                    i.location_id == sys_id as i64
                        && (i.type_id == 33700
                            || ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                                t.default_attributes.get(&3038).copied().unwrap_or(0.0) > 0.0
                            }))
                });
                (structure_jammed || item_inhibitor, structure_jammed, item_inhibitor)
            });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("isJammed", PyRep::Bool(is_jammed)),
                ("active", PyRep::Bool(is_jammed)),
                ("cynoSuppression", PyRep::Bool(cyno_suppressed)),
                ("structureInhibitor", PyRep::Bool(inhibitor_active)),
            ])))
        }

        _ => None,
    }
}

/// `warpVectorMgr.UseVector(itemID)` (`menuFunctions.pyj:586`): validates that the character is
/// undocked in space and applies the directional warp/jump vector to the active ship ball.
pub async fn handle_use_warp_vector(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate a warp vector while docked".into(),
        });
    }
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship in space".into(),
    })?;
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solar system".into(),
    })?;
    let target_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "warpVectorMgr.UseVector requires a positive vector itemID".into(),
        })?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.is_ball_scrambled_sync(ship_id) || sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect".into(),
        });
    }
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
        })?;
    let (_, target_ball) = sys_handle
        .get_ball_snapshot(target_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "VectorItemNotFound".into(),
            message: format!("Warp vector entity {target_id} is not in space in solar system {sys_id}"),
        })?;
    let dist = (target_ball.position - ship_ball.position).length();
    const MAX_VECTOR_PROXIMITY_M: f64 = 100_000.0;
    if dist > MAX_VECTOR_PROXIMITY_M {
        return Err(BridgeRpcError {
            code: "TargetNotInRange".into(),
            message: format!("Warp vector entity {target_id} is out of activation range ({dist:.1} m > {MAX_VECTOR_PROXIMITY_M:.1} m)"),
        });
    }
    let explicit_vec = match args.get(1) {
        Some(PyRep::Tuple(seq) | PyRep::List(seq)) if seq.len() >= 3 => {
            let v = glam::DVec3::new(
                seq[0].as_f64().unwrap_or(0.0),
                seq[1].as_f64().unwrap_or(0.0),
                seq[2].as_f64().unwrap_or(0.0),
            );
            if !v.is_finite() {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Vector coordinates must be finite".into(),
                });
            }
            Some(v)
        }
        _ => None,
    };
    let dest_pos = if let Some(v) = explicit_vec.filter(|v| v.length() > 1e-6) {
        if v.length() >= 1_000.0 {
            ship_ball.position + v
        } else {
            ship_ball.position + v.normalize() * 100_000.0
        }
    } else {
        let delta = target_ball.position - ship_ball.position;
        if delta.length() > 1.0 {
            target_ball.position + delta.normalize() * 100_000.0
        } else {
            target_ball.position + glam::DVec3::new(100_000.0, 0.0, 0.0)
        }
    };
    let _ = sys_handle.set_ball_position(ship_id, dest_pos).await;
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("vectorItemID", PyRep::Integer(target_id)),
        (
            "destination",
            PyRep::Tuple(vec![
                PyRep::Real(dest_pos.x),
                PyRep::Real(dest_pos.y),
                PyRep::Real(dest_pos.z),
            ]),
        ),
    ]))
}

/// `jumpCloneSvc.ResetLastCloneJumpTime()` (`insider.pyj:2142`): resets the character's
/// `last_clone_jump_unix` timestamp to `0` in `WorldStateSnapshot` and emits `OnCloneJumpUpdate`.
pub fn handle_reset_last_clone_jump_time(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "jumpCloneSvc.ResetLastCloneJumpTime requires GM role".into(),
        });
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        ch.last_clone_jump_unix = 0;
        ch.version += 1;
        Ok(())
    })?;
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnCloneJumpUpdate",
        "charid",
        vec![],
    );
    Ok(PyRep::Integer(0))
}

const DISMISSED_CONTRACT_NOTIFS_BLOB: &str = "contract_dismissed_notifs";

/// `contractProxy.DeleteContractNotification(contractID, forCorp)` and
/// `contractProxy.DeleteNotification(contractID, forCorp)` (`contracts.pyj:881,1032`).
pub fn handle_contract_delete_notif(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: format!("contractProxy.{method} requires a positive contractID"),
        })?;
    let for_corp = args
        .get(1)
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_i64().unwrap_or(0) != 0);
    let caller_corp = session.corporation_id.unwrap_or(0);
    let owner_id = if for_corp { caller_corp } else { char_id };
    let now_unix = ctx.now_unix();
    let changed = ctx.db.try_with_mut_snapshot(|snap| {
        if for_corp {
            if caller_corp < 98_000_000 {
                return Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Corporation contract notifications require a player corporation".into(),
                });
            }
            let allowed = snap.corporations.get(&caller_corp).is_some_and(|corp| {
                let r = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                char_id == corp.ceo_id
                    || (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER)) != 0
            });
            if !allowed {
                return Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Deleting corporation contract notifications requires Contract Manager or Director role".into(),
                });
            }
        }
        let Some(c) = snap.contracts.get(&cid).cloned() else {
            return Err(BridgeRpcError {
                code: "ContractNotFound".into(),
                message: format!("Contract {cid} does not exist"),
            });
        };
        let is_related = c.issuer_id == owner_id
            || c.issuer_corp_id == owner_id
            || c.assignee_id == owner_id
            || c.acceptor_id == Some(owner_id)
            || c.current_bidder_id == Some(owner_id)
            || c.bid_history.iter().any(|&(b, _, _)| b == owner_id || b == char_id);
        if !is_related {
            return Err(BridgeRpcError {
                code: "ContractAccessDenied".into(),
                message: format!("Owner {owner_id} is not associated with contract {cid}"),
            });
        }
        if c.status == "Outstanding" && c.expiration_unix > 0 && c.expiration_unix <= now_unix {
            let _ = snap.resolve_expired_contract(cid, now_unix);
        }
        let mut dismissed: Vec<String> = snap
            .quasar_state_blobs
            .get(DISMISSED_CONTRACT_NOTIFS_BLOB)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let key = format!("{owner_id}:{cid}");
        let mut mutated = false;
        if !dismissed.contains(&key) {
            dismissed.push(key);
            if let Ok(json) = serde_json::to_string(&dismissed) {
                snap.quasar_state_blobs
                    .insert(DISMISSED_CONTRACT_NOTIFS_BLOB.to_string(), json);
            }
            mutated = true;
        }
        Ok(mutated)
    })?;
    Ok(PyRep::Bool(changed))
}

/// `contractProxy.DeleteMultipleContracts(contractIDs)` (`contracts.pyj:561`): deletes all
/// eligible `Outstanding` contracts in `contractIDs`, returning `(deletedIDs, failedIDs)`.
pub fn handle_contract_delete_multiple(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if cids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "contractProxy.DeleteMultipleContracts requires a non-empty list of contractIDs".into(),
        });
    }
    let caller_corp = session.corporation_id.unwrap_or(0);
    let (deleted, failed) = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_mgr = snap.corporations.get(&caller_corp).is_some_and(|corp| {
            let r = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
            char_id == corp.ceo_id
                || (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER)) != 0
        });
        let mut del = Vec::new();
        let mut fail = Vec::new();
        for cid in cids {
            let Some(c) = snap.contracts.get(&cid).cloned() else {
                fail.push(PyRep::Integer(cid));
                continue;
            };
            let owns = c.issuer_id == char_id
                || (c.for_corp && c.issuer_corp_id == caller_corp && corp_mgr);
            let has_bids = c.contract_type == "Auction" && c.current_bidder_id.is_some();
            if c.status == "Outstanding" && owns && !has_bids {
                snap.credit_contract_issuer(&c, c.reward);
                for iid in &c.item_ids {
                    if let Some(it) = snap.items.get_mut(iid) {
                        it.owner_id = c.issuer_id;
                        it.location_id = c.start_station_id;
                        it.flag = eve_db::InventoryFlag::Hangar;
                        it.version += 1;
                    }
                }
                if let Some(cm) = snap.contracts.get_mut(&cid) {
                    cm.status = "Deleted".into();
                }
                del.push(PyRep::Integer(cid));
            } else {
                fail.push(PyRep::Integer(cid));
            }
        }
        Ok::<_, BridgeRpcError>((del, fail))
    })?;
    Ok(PyRep::Tuple(vec![PyRep::List(deleted), PyRep::List(failed)]))
}

/// `contractProxy.GetItemsInDockableLocation(stationID, forCorp)` (`contracts.pyj:872`):
/// returns inventory items owned by the character (or corporation) at `stationID` / `structureID`.
pub fn handle_contract_get_items_in_dockable(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let loc_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| session.station_id.or(session.structure_id))
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "contractProxy.GetItemsInDockableLocation requires a valid dockable locationID".into(),
        })?;
    let for_corp = args
        .get(1)
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_i64().unwrap_or(0) != 0);
    let owner_id = if for_corp {
        session.corporation_id.unwrap_or(0)
    } else {
        char_id
    };
    let rows = ctx.db.with_snapshot(|snap| {
        let mut items: Vec<_> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == owner_id
                    && i.location_id == loc_id
                    && Some(i.item_id) != session.ship_id
                    && i.flag != eve_db::InventoryFlag::Locked
                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
            })
            .collect();
        items.sort_by_key(|i| i.item_id);
        items
            .into_iter()
            .map(|i| {
                let (gid, cid) = ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                    .unwrap_or((0, 0));
                let singleton_val = if i.is_blueprint_copy {
                    2
                } else if i.singleton {
                    1
                } else {
                    0
                };
                let qty_val = if i.is_blueprint_copy {
                    -2
                } else if i.singleton {
                    -1
                } else {
                    i64::from(i.quantity)
                };
                PyRep::keyval([
                    ("itemID", PyRep::Integer(i.item_id)),
                    ("typeID", PyRep::Integer(i64::from(i.type_id))),
                    ("groupID", PyRep::Integer(gid)),
                    ("categoryID", PyRep::Integer(cid)),
                    ("stacksize", PyRep::Integer(i64::from(i.quantity))),
                    ("quantity", PyRep::Integer(qty_val)),
                    ("flagID", PyRep::Integer(i.flag as i64)),
                    ("singleton", PyRep::Integer(singleton_val)),
                    ("ownerID", PyRep::Integer(i.owner_id)),
                    ("locationID", PyRep::Integer(i.location_id)),
                ])
            })
            .collect::<Vec<_>>()
    });
    Ok(PyRep::List(rows))
}

fn item_repair_metrics(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    item: &eve_db::InventoryItemRecord,
) -> (f64, f64, f64) {
    let mut dmg = item
        .dynamic_attributes
        .get(&eve_sde::attr_ids::DAMAGE)
        .copied()
        .unwrap_or(0.0)
        + item
            .dynamic_attributes
            .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
            .copied()
            .unwrap_or(0.0);
    if let Ok(fits) = ctx.ship_fittings.read() {
        if let Some(fit) = fits.get(&item.location_id) {
            if let Some(m) = fit.modules.values().find(|m| m.item_id == item.item_id) {
                dmg = dmg.max(m.damage_hp);
            }
        }
    }
    let t_opt = ctx.sde.types.get(&item.type_id);
    let max_hp = t_opt
        .map(|t| {
            let hp = t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(0.0);
            let armor = t.default_attributes.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(0.0);
            (hp + armor).max(40.0)
        })
        .unwrap_or(100.0);
    let base_price = t_opt.map(|t| t.base_price).unwrap_or(10_000.0).max(1_000.0);
    let _ = snap;
    let cost_per_unit = ((base_price * 0.0125) / max_hp).max(1.0);
    (dmg, max_hp, cost_per_unit)
}

/// `repairSvc.GetRepairQuotes(itemIDs)` (`base_repairshop.pyj:65`): returns a `Dict` mapping each
/// requested `itemID` to a `List` of `KeyVal` damage quote rows (`itemID`, `typeID`, `groupID`,
/// `damage`, `maxHealth`, `costToRepairOneUnitOfDamage`, `totalRepairCost`).
pub fn handle_repair_get_quotes(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let mut item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        if let Some(sid) = session.ship_id {
            item_ids.push(sid);
        }
    }
    let corp_id = session.corporation_id;
    let dict_entries = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::with_capacity(item_ids.len());
        for req_id in item_ids {
            let mut sub_items: Vec<&eve_db::InventoryItemRecord> = snap
                .items
                .values()
                .filter(|i| {
                    (i.owner_id == char_id || Some(i.owner_id) == corp_id)
                        && (i.item_id == req_id || i.location_id == req_id)
                })
                .collect();
            sub_items.sort_by_key(|i| i.item_id);
            let mut rows = Vec::new();
            for it in sub_items {
                let (dmg, max_hp, cpu) = item_repair_metrics(ctx, snap, it);
                let gid = ctx.sde.types.get(&it.type_id).map(|t| i64::from(t.group_id)).unwrap_or(0);
                let total = (dmg.ceil() * cpu).round();
                rows.push(PyRep::keyval([
                    ("itemID", PyRep::Integer(it.item_id)),
                    ("typeID", PyRep::Integer(i64::from(it.type_id))),
                    ("groupID", PyRep::Integer(gid)),
                    ("damage", PyRep::Real(dmg)),
                    ("maxHealth", PyRep::Integer(max_hp.round() as i64)),
                    ("costToRepairOneUnitOfDamage", PyRep::Real(cpu)),
                    ("totalRepairCost", PyRep::Real(total)),
                    ("discount", PyRep::Real(0.0)),
                    ("serviceCharge", PyRep::Real(0.0)),
                ]));
            }
            out.push((PyRep::Integer(req_id), PyRep::List(rows)));
        }
        out
    });
    Ok(PyRep::Dict(dict_entries))
}

/// `repairSvc.RepairItemsInStation(itemIDs, iskRepairValue)` and
/// `repairSvc.RepairItemsInStructure(itemIDs)` (`base_repairshop.pyj:69,71`).
pub fn handle_repair_items_in_location(
    ctx: &ServerContext,
    session: &SessionState,
    is_structure: bool,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let loc_id = if is_structure {
        session.structure_id.ok_or_else(|| BridgeRpcError {
            code: "NotInStructure".into(),
            message: "RepairItemsInStructure requires being docked in an Upwell structure".into(),
        })?
    } else {
        session.station_id.ok_or_else(|| BridgeRpcError {
            code: "NotInStation".into(),
            message: "RepairItemsInStation requires being docked in a station".into(),
        })?
    };
    let item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "RepairItems requires a non-empty list of itemIDs".into(),
        });
    }
    let isk_budget = args.get(1).and_then(|a| a.as_f64());
    if isk_budget.is_some_and(|v| v < 0.0) {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "iskRepairValue cannot be negative".into(),
        });
    }
    let corp_id = session.corporation_id;
    let active_ship = session.ship_id;
    let (repaired_ids, total_cost, new_bal) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut target_ids = Vec::new();
        let mut total_isk = 0.0;
        for &req_id in &item_ids {
            let Some(root) = snap.items.get(&req_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {req_id} not found"),
                });
            };
            if root.owner_id != char_id && Some(root.owner_id) != corp_id {
                return Err(BridgeRpcError {
                    code: "NotItemOwner".into(),
                    message: format!("Character {char_id} does not own item {req_id}"),
                });
            }
            if root.location_id != loc_id && Some(root.location_id) != active_ship {
                return Err(BridgeRpcError {
                    code: "ItemNotAtLocation".into(),
                    message: format!("Item {req_id} is not located at dockable location {loc_id}"),
                });
            }
            for it in snap.items.values() {
                if (it.owner_id == char_id || Some(it.owner_id) == corp_id)
                    && (it.item_id == req_id || it.location_id == req_id)
                    && !target_ids.contains(&it.item_id)
                {
                    let (dmg, _, cpu) = item_repair_metrics(ctx, snap, it);
                    if dmg > 0.0 {
                        total_isk += (dmg.ceil() * cpu).round();
                    }
                    target_ids.push(it.item_id);
                }
            }
        }
        if total_isk > 0.0 {
            if let Some(budget) = isk_budget {
                if budget + 1e-6 < total_isk {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!("Provided iskRepairValue {budget:.2} is less than repair cost {total_isk:.2}"),
                    });
                }
            }
        }
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        let fee = eve_db::IskCents::from_f64_isk(total_isk);
        if fee.0 > 0 {
            ch.balance = ch.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                code: "InsufficientFunds".into(),
                message: "Insufficient ISK to pay repair bill".into(),
            })?;
            let bal = ch.balance;
            snap.append_journal(
                char_id,
                "RepairBill",
                eve_db::IskCents(-fee.0),
                bal,
                format!("Repaired {} item(s) at {loc_id}", target_ids.len()),
            );
        }
        let bal = snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
        for tid in &target_ids {
            if let Some(it) = snap.items.get_mut(tid) {
                it.dynamic_attributes.remove(&eve_sde::attr_ids::DAMAGE);
                it.dynamic_attributes.remove(&eve_sde::attr_ids::ARMOR_DAMAGE);
                it.version += 1;
            }
        }
        Ok((target_ids, total_isk, bal))
    })?;
    if let Ok(mut fits) = ctx.ship_fittings.write() {
        for fit in fits.values_mut() {
            for m in fit.modules.values_mut() {
                if repaired_ids.contains(&m.item_id) {
                    m.damage_hp = 0.0;
                }
            }
        }
    }
    if total_cost > 0.0 {
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![
                PyRep::String("cash".into()),
                PyRep::Integer(char_id),
                PyRep::Real(new_bal),
            ],
        );
    }
    for iid in &repaired_ids {
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnModuleRepaired",
            "charid",
            vec![PyRep::Integer(*iid)],
        );
    }
    Ok(PyRep::keyval([
        ("locationID", PyRep::Integer(loc_id)),
        ("repairedItemIDs", PyRep::List(repaired_ids.into_iter().map(PyRep::Integer).collect())),
        ("totalCostISK", PyRep::Real(total_cost)),
    ]))
}
