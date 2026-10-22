//! `fw_services` — Faction Warfare Warzones, Victory Points, Advantage & HQ Systems (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// SDE Solar System IDs for the 4 Empire Militia War HQs:
/// - Mehatoor (30002974) - 24th Imperial Crusade (Amarr)
/// - Onnamon (30045324) - State Protectorate (Caldari)
/// - Intaki (30003788) - Federal Defence Union (Gallente)
/// - Amo (30002055) - Tribal Liberation Force (Minmatar)
pub const HQ_SYSTEM_MEHATOOR: u32 = 30002974;
pub const HQ_SYSTEM_ONNAMON: u32 = 30045324;
pub const HQ_SYSTEM_INTAKI: u32 = 30003788;
pub const HQ_SYSTEM_AMO: u32 = 30002055;

pub const FACTION_CALDARI: i64 = 500001;
pub const FACTION_MINMATAR: i64 = 500002;
pub const FACTION_AMARR: i64 = 500003;
pub const FACTION_GALLENTE: i64 = 500004;

pub const STATUS_FRONTLINE: i64 = 1;
pub const STATUS_COMMAND: i64 = 2;
pub const STATUS_REARGUARD: i64 = 3;

#[derive(Debug, Default)]
pub struct FwRuntimeState {
    pub db_weak: Weak<eve_db::WorldRepository>,
    pub system_victory_points: HashMap<u32, i64>,
    pub system_advantage: HashMap<u32, (f64, f64)>, // sys_id -> (friendly_adv, enemy_adv)
    pub battlefield_instances: Vec<BattlefieldRecord>,
}

#[derive(Debug, Clone)]
pub struct BattlefieldRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub faction_id: i64,
    pub name: String,
    pub capture_progress: f64,
    pub completed: bool,
}

static FW_RUNTIME: OnceLock<RwLock<HashMap<usize, FwRuntimeState>>> = OnceLock::new();

fn runtime_map() -> &'static RwLock<HashMap<usize, FwRuntimeState>> {
    FW_RUNTIME.get_or_init(|| RwLock::new(HashMap::new()))
}

fn ctx_key(ctx: &ServerContext) -> usize {
    Arc::as_ptr(&ctx.db) as usize
}

pub fn with_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut FwRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| FwRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
}

pub fn read_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&FwRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| FwRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
}

/// Broadcasts `OnSolarsystemAdvantageStateUpdated_Local` notification.
pub fn emit_advantage_updated(ctx: &ServerContext, sys_id: u32, friendly: f64, enemy: f64) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarsystemAdvantageStateUpdated_Local",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Real(friendly),
            PyRep::Real(enemy),
        ],
    );
}

/// Updates advantage in a warzone system and notifies clients.
pub fn update_system_advantage(ctx: &ServerContext, sys_id: u32, friendly_delta: f64, enemy_delta: f64) {
    let (friendly, enemy) = with_fw_runtime(ctx, |rt| {
        let entry = rt.system_advantage.entry(sys_id).or_insert((0.0, 0.0));
        entry.0 = (entry.0 + friendly_delta).clamp(0.0, 100.0);
        entry.1 = (entry.1 + enemy_delta).clamp(0.0, 100.0);
        *entry
    });
    emit_advantage_updated(ctx, sys_id, friendly, enemy);
}

/// Adds victory points to a system from complex captures or enemy kills.
pub fn add_victory_points(ctx: &ServerContext, sys_id: u32, vp: i64) -> (i64, f64) {
    let new_vp = with_fw_runtime(ctx, |rt| {
        let entry = rt.system_victory_points.entry(sys_id).or_insert(0);
        *entry = (*entry + vp).max(0);
        *entry
    });
    let contested = ((new_vp as f64 / 3000.0) * 100.0).clamp(0.0, 100.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.fw_contested_pct = contested;
        }
    });
    (new_vp, contested)
}

/// Scales capture speed by advantage.
pub fn calculate_capture_speed_multiplier(friendly_adv: f64, enemy_adv: f64) -> f64 {
    (1.0 + (friendly_adv - enemy_adv) * 0.005).clamp(0.5, 2.0)
}

/// Adds a Battlefield site instance to a warzone system.
pub fn spawn_battlefield_site(ctx: &ServerContext, sys_id: u32, faction_id: i64, name: &str) -> i64 {
    with_fw_runtime(ctx, |rt| {
        let id = (rt.battlefield_instances.len() as i64) + 700_001;
        rt.battlefield_instances.push(BattlefieldRecord {
            instance_id: id,
            solar_system_id: sys_id,
            faction_id,
            name: name.to_string(),
            capture_progress: 0.0,
            completed: false,
        });
        id
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
        ("fwWarzoneSolarsystem", "GetHQSystemIDs") => {
            let hqs = vec![
                PyRep::Integer(i64::from(HQ_SYSTEM_MEHATOOR)),
                PyRep::Integer(i64::from(HQ_SYSTEM_ONNAMON)),
                PyRep::Integer(i64::from(HQ_SYSTEM_INTAKI)),
                PyRep::Integer(i64::from(HQ_SYSTEM_AMO)),
            ];
            Some(Ok(PyRep::List(hqs)))
        }

        ("fwWarzoneSolarsystem", "GetAllWarzones") => {
            let wz1 = PyRep::keyval([
                ("warzoneID", PyRep::Integer(1)),
                ("name", PyRep::String("Caldari - Gallente Warzone".into())),
                ("factionOneID", PyRep::Integer(FACTION_CALDARI)),
                ("factionTwoID", PyRep::Integer(FACTION_GALLENTE)),
            ]);
            let wz2 = PyRep::keyval([
                ("warzoneID", PyRep::Integer(2)),
                ("name", PyRep::String("Amarr - Minmatar Warzone".into())),
                ("factionOneID", PyRep::Integer(FACTION_AMARR)),
                ("factionTwoID", PyRep::Integer(FACTION_MINMATAR)),
            ]);
            Some(Ok(PyRep::List(vec![wz1, wz2])))
        }

        ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates")
        | ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached") => {
            let states = ctx.db.with_snapshot(|snap| {
                let mut list = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
                    if sec > 0.0 && sec < 0.45 {
                        let fac = ctx
                            .sde
                            .solar_systems
                            .get(&sys_id)
                            .and_then(|s| s.stations.first())
                            .and_then(|st| st.owner_faction_id)
                            .unwrap_or(FACTION_CALDARI);
                        let front = if st.fw_contested_pct > 75.0 {
                            STATUS_FRONTLINE
                        } else if st.fw_contested_pct > 25.0 {
                            STATUS_COMMAND
                        } else {
                            STATUS_REARGUARD
                        };
                        list.push(PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("ownerFactionID", PyRep::Integer(fac)),
                            ("occupierFactionID", PyRep::Integer(fac)),
                            ("frontlineStatus", PyRep::Integer(front)),
                            ("contestedPercentage", PyRep::Real(st.fw_contested_pct)),
                        ]));
                    }
                }
                list
            });
            Some(Ok(PyRep::List(states)))
        }

        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            let is_hq = matches!(
                sys_id,
                HQ_SYSTEM_MEHATOOR | HQ_SYSTEM_ONNAMON | HQ_SYSTEM_INTAKI | HQ_SYSTEM_AMO
            );
            let (is_warzone, contested, owner_fac, occupier_fac) = ctx.db.with_snapshot(|snap| {
                let c = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| s.fw_contested_pct)
                    .unwrap_or(0.0);
                if let Some(occ) = snap.fw_system_occupations.get(&sys_id) {
                    return (true, c, occ.owner_faction_id, occ.occupier_faction_id);
                }
                let in_wz = (sec > 0.0 && sec < 0.45) || is_hq;
                let f = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.first())
                    .and_then(|st| st.owner_faction_id)
                    .unwrap_or(FACTION_CALDARI);
                (in_wz, c, f, f)
            });
            // `FwWarzoneSvc.GetLocalOccupationState` (`eve/client/script/ui/services/fwWarzoneSvc.pyj:74-95`)
            // unpacks `solarsystemID, occupationState = sm.RemoteSvc('fwWarzoneSolarsystem').GetLocalOccupationState()`,
            // caches `(solarsystemID, occupationState)`, and returns `occupationState` (`_localOccupationSystemAndState[1]`).
            // For non-warzone systems (e.g. high-sec Jita 30000142 / Maurasi 30000141), `occupationState` is `None`
            // so `FwWarzoneSvc.GetLocalOccupationState()` returns `None` and `IsWarzoneSolarSystem` returns `False`.
            let occ_state = if is_warzone {
                let front = if contested > 75.0 {
                    STATUS_FRONTLINE
                } else if contested > 25.0 {
                    STATUS_COMMAND
                } else {
                    STATUS_REARGUARD
                };
                PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("ownerFactionID", PyRep::Integer(owner_fac)),
                    ("occupierFactionID", PyRep::Integer(occupier_fac)),
                    ("frontlineStatus", PyRep::Integer(front)),
                    ("contestedPercentage", PyRep::Real(contested)),
                ])
            } else {
                PyRep::None
            };
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(sys_id)),
                occ_state,
            ])))
        }

        ("fwVictoryPointMgr", "GetLocalVictoryPointState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let (vp, contested) = read_fw_runtime(ctx, |rt| {
                let points = rt.system_victory_points.get(&sys_id).copied().unwrap_or(0);
                let c = ctx.db.with_snapshot(|snap| {
                    snap.system_states.get(&sys_id).map(|s| s.fw_contested_pct).unwrap_or(0.0)
                });
                (points, c)
            });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("victoryPoints", PyRep::Integer(vp)),
                ("contestedPercentage", PyRep::Real(contested)),
                ("threshold", PyRep::Integer(3000)),
            ])))
        }

        ("fwVictoryPointMgr", "GetAllVictoryPointStates") => {
            let res = read_fw_runtime(ctx, |rt| {
                let mut map = Vec::new();
                for (&sys_id, &vp) in &rt.system_victory_points {
                    let c = ctx.db.with_snapshot(|snap| {
                        snap.system_states.get(&sys_id).map(|s| s.fw_contested_pct).unwrap_or(0.0)
                    });
                    map.push((
                        PyRep::Integer(i64::from(sys_id)),
                        PyRep::keyval([
                            ("victoryPoints", PyRep::Integer(vp)),
                            ("contestedPercentage", PyRep::Real(c)),
                        ]),
                    ));
                }
                PyRep::Dict(map)
            });
            Some(Ok(res))
        }

        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id);
            let instances = read_fw_runtime(ctx, |rt| {
                rt.battlefield_instances
                    .iter()
                    .filter(|b| sys_id.map(|s| b.solar_system_id == s).unwrap_or(true))
                    .map(|b| {
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(b.instance_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(b.solar_system_id))),
                            ("factionID", PyRep::Integer(b.faction_id)),
                            ("name", PyRep::String(b.name.clone())),
                            ("captureProgress", PyRep::Real(b.capture_progress)),
                            ("completed", PyRep::Bool(b.completed)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(instances)))
        }

        _ => None,
    }
}
