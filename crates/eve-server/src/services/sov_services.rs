//! `sov_services` — Sovereignty Management, Equinox Skyhooks & Dev Indices (`§3.8`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

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

#[derive(Debug, Default)]
pub struct SovRuntimeState {
    pub db_weak: Weak<eve_db::WorldRepository>,
    pub fuel_access_groups: HashMap<i64, i64>,
    pub hacked_hubs: HashMap<i64, u64>,
    pub skyhook_reagents: HashMap<i64, (u32, u64)>, // skyhook_id -> (reagent_type_id, units)
    pub skyhook_hackable: HashMap<i64, bool>,
}

static SOV_RUNTIME: OnceLock<RwLock<HashMap<usize, SovRuntimeState>>> = OnceLock::new();

fn runtime_map() -> &'static RwLock<HashMap<usize, SovRuntimeState>> {
    SOV_RUNTIME.get_or_init(|| RwLock::new(HashMap::new()))
}

fn ctx_key(ctx: &ServerContext) -> usize {
    Arc::as_ptr(&ctx.db) as usize
}

pub fn with_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut SovRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| SovRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
}

pub fn read_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&SovRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| SovRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
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

/// Broadcasts `OnSovereigntyChanged` to all sessions or target character.
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
    with_sov_runtime(ctx, |rt| {
        rt.hacked_hubs.insert(hub_id, now + disruption_seconds);
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
                        let reagents = read_sov_runtime(ctx, |rt| {
                            rt.skyhook_reagents.get(&s.structure_id).copied().unwrap_or((0, 0))
                        });
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
