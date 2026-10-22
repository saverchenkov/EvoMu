//! `insurgency_services` — Pirate Insurgencies, Corruption/Suppression Campaigns & Sites (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

pub const FACTION_GURISTAS: i64 = 500010;
pub const FACTION_ANGEL_CARTEL: i64 = 500011;

pub const PHASE_FORECASTING: &str = "Forecasting";
pub const PHASE_ACTIVE: &str = "Active";
pub const PHASE_WINDDOWN: &str = "WindDown";

pub const FORECASTING_DURATION_SECS: u64 = 48 * 3600; // 48h
pub const ACTIVE_DURATION_SECS: u64 = 7 * 86400;      // 7 days
pub const WINDDOWN_DURATION_SECS: u64 = 48 * 3600;    // 48h

#[derive(Debug, Clone)]
pub struct InsurgencyCampaign {
    pub campaign_id: i64,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub fob_solar_system_id: u32,
    pub systems: Vec<u32>,
    pub created_at_unix: u64,
    pub forecasting_end_unix: u64,
    pub active_end_unix: u64,
    pub winddown_end_unix: u64,
    pub corruption_points: HashMap<u32, i64>,
    pub suppression_points: HashMap<u32, i64>,
}

#[derive(Debug, Default)]
pub struct InsurgencyRuntimeState {
    pub db_weak: Weak<eve_db::WorldRepository>,
    pub campaigns: HashMap<i64, InsurgencyCampaign>,
    pub insurgency_instances: Vec<InsurgencySiteRecord>,
    pub ice_heist_instances: Vec<IceHeistRecord>,
}

#[derive(Debug, Clone)]
pub struct InsurgencySiteRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub site_type: String,
    pub name: String,
    pub corruption_reward: f64,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub struct IceHeistRecord {
    pub instance_id: i64,
    pub solar_system_id: u32,
    pub pirate_tankers_saved: u32,
    pub empire_tankers_saved: u32,
    pub completed: bool,
}

static INSURGENCY_RUNTIME: OnceLock<RwLock<HashMap<usize, InsurgencyRuntimeState>>> = OnceLock::new();

fn runtime_map() -> &'static RwLock<HashMap<usize, InsurgencyRuntimeState>> {
    INSURGENCY_RUNTIME.get_or_init(|| RwLock::new(HashMap::new()))
}

fn ctx_key(ctx: &ServerContext) -> usize {
    Arc::as_ptr(&ctx.db) as usize
}

pub fn with_insurgency_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut InsurgencyRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| InsurgencyRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
}

pub fn read_insurgency_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&InsurgencyRuntimeState) -> R) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, v| v.db_weak.strong_count() > 0);
    let st = guard.entry(key).or_insert_with(|| InsurgencyRuntimeState {
        db_weak: Arc::downgrade(&ctx.db),
        ..Default::default()
    });
    f(st)
}

/// Broadcasts `OnInsurgencyCampaignStartedForLocation` notification.
pub fn emit_campaign_started(ctx: &ServerContext, sys_id: u32, campaign_id: i64) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnInsurgencyCampaignStartedForLocation",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(campaign_id),
        ],
    );
}

/// Broadcasts `OnInsurgencyCampaignUpdatedForLocation` notification.
pub fn emit_campaign_updated(ctx: &ServerContext, sys_id: u32, campaign_id: i64, stage: u8) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnInsurgencyCampaignUpdatedForLocation",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(campaign_id),
            PyRep::Integer(i64::from(stage)),
        ],
    );
}

/// Initializes a pirate insurgency campaign with starting systems.
pub fn init_insurgency_campaign(
    ctx: &ServerContext,
    campaign_id: i64,
    pirate_faction: i64,
    target_faction: i64,
    fob_system: u32,
    mut systems: Vec<u32>,
) {
    if !systems.contains(&fob_system) {
        systems.push(fob_system);
    }
    let now = ctx.now_unix();
    let fc_end = now + FORECASTING_DURATION_SECS;
    let act_end = fc_end + ACTIVE_DURATION_SECS;
    let wd_end = act_end + WINDDOWN_DURATION_SECS;

    let campaign = InsurgencyCampaign {
        campaign_id,
        pirate_faction_id: pirate_faction,
        target_faction_id: target_faction,
        fob_solar_system_id: fob_system,
        systems: systems.clone(),
        created_at_unix: now,
        forecasting_end_unix: fc_end,
        active_end_unix: act_end,
        winddown_end_unix: wd_end,
        corruption_points: HashMap::new(),
        suppression_points: HashMap::new(),
    };

    with_insurgency_runtime(ctx, |rt| {
        rt.campaigns.insert(campaign_id, campaign);
    });

    for sys in &systems {
        emit_campaign_started(ctx, *sys, campaign_id);
        ctx.notify(
            NotificationTarget::SolarSystem(*sys),
            "OnInsurgencyStateChangedForFaction",
            "solarsystemid",
            vec![
                PyRep::Integer(1),
                PyRep::Integer(pirate_faction),
                PyRep::Integer(i64::from(fob_system)),
            ],
        );
    }
}

/// Ends an active pirate insurgency campaign and emits `OnInsurgencyCampaignEndingForLocation` and `OnInsurgencyStateChangedForFaction`.
pub fn end_insurgency_campaign(ctx: &ServerContext, campaign_id: i64) {
    let removed = with_insurgency_runtime(ctx, |rt| rt.campaigns.remove(&campaign_id));
    if let Some(c) = removed {
        for sys in &c.systems {
            ctx.notify(
                NotificationTarget::SolarSystem(*sys),
                "OnInsurgencyCampaignEndingForLocation",
                "solarsystemid",
                vec![PyRep::Integer(i64::from(*sys))],
            );
            ctx.notify(
                NotificationTarget::SolarSystem(*sys),
                "OnInsurgencyStateChangedForFaction",
                "solarsystemid",
                vec![
                    PyRep::Integer(0),
                    PyRep::Integer(c.pirate_faction_id),
                    PyRep::Integer(i64::from(c.fob_solar_system_id)),
                ],
            );
        }
    }
}

/// Computes the current campaign phase and end time from server clock.
pub fn get_campaign_phase_and_timer(campaign: &InsurgencyCampaign, now: u64) -> (&'static str, u64) {
    if now < campaign.forecasting_end_unix {
        (PHASE_FORECASTING, campaign.forecasting_end_unix)
    } else if now < campaign.active_end_unix {
        (PHASE_ACTIVE, campaign.active_end_unix)
    } else {
        (PHASE_WINDDOWN, campaign.winddown_end_unix)
    }
}

/// Advances corruption points in a system and updates corruption stage (1..5).
pub fn add_system_corruption(ctx: &ServerContext, campaign_id: i64, sys_id: u32, points: i64) -> (i64, u8) {
    let (pts, stage, pirate_faction) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 1, FACTION_GURISTAS);
        };
        let p = c.corruption_points.entry(sys_id).or_insert(0);
        *p = (*p + points).max(0);
        let s = ((*p as f64 / 200.0).ceil() as u8).clamp(1, 5);
        (*p, s, c.pirate_faction_id)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_corruption_stage = stage;
        }
    });

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);
    if stage >= 5 {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnSystemFullyCorruptedForFaction",
            "solarsystemid",
            vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Integer(pirate_faction),
                PyRep::Integer(pts),
                PyRep::Integer(1000),
            ],
        );
    }
    (pts, stage)
}

/// Advances suppression points in a system and updates suppression stage (1..5).
pub fn add_system_suppression(ctx: &ServerContext, campaign_id: i64, sys_id: u32, points: i64) -> (i64, u8) {
    let (pts, stage) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 1);
        };
        let p = c.suppression_points.entry(sys_id).or_insert(0);
        *p = (*p + points).max(0);
        let s = ((*p as f64 / 200.0).ceil() as u8).clamp(1, 5);
        (*p, s)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_suppression_stage = stage;
        }
    });

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);
    if stage >= 5 {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnSystemFullySuppressedForFaction",
            "solarsystemid",
            vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Integer(pts),
                PyRep::Integer(1000),
            ],
        );
    }
    (pts, stage)
}

/// Spawns an Ice Heist site instance in a system.
pub fn spawn_ice_heist(ctx: &ServerContext, sys_id: u32) -> i64 {
    with_insurgency_runtime(ctx, |rt| {
        let id = (rt.ice_heist_instances.len() as i64) + 800_001;
        rt.ice_heist_instances.push(IceHeistRecord {
            instance_id: id,
            solar_system_id: sys_id,
            pirate_tankers_saved: 0,
            empire_tankers_saved: 0,
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
    let now = ctx.now_unix();
    match (service, method) {
        ("insurgencySolarsystem", "GetAllVisibleCampaigns") => {
            let campaigns = read_insurgency_runtime(ctx, |rt| {
                rt.campaigns
                    .values()
                    .map(|c| {
                        let (phase, end_time) = get_campaign_phase_and_timer(c, now);
                        let sys_reps: Vec<PyRep> = c.systems.iter().map(|&s| PyRep::Integer(i64::from(s))).collect();
                        PyRep::keyval([
                            ("campaignID", PyRep::Integer(c.campaign_id)),
                            ("pirateFactionID", PyRep::Integer(c.pirate_faction_id)),
                            ("targetFactionID", PyRep::Integer(c.target_faction_id)),
                            ("fobSolarSystemID", PyRep::Integer(i64::from(c.fob_solar_system_id))),
                            ("phase", PyRep::String(phase.into())),
                            ("phaseEndTime", PyRep::Integer(end_time as i64)),
                            ("systems", PyRep::List(sys_reps)),
                            ("isFobVulnerable", PyRep::Bool(phase == PHASE_ACTIVE)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(campaigns)))
        }

        ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };

            let snapshot = read_insurgency_runtime(ctx, |rt| {
                rt.campaigns
                    .values()
                    .find(|c| c.systems.contains(&sys_id))
                    .map(|c| {
                        let (phase, end_time) = get_campaign_phase_and_timer(c, now);
                        let c_pts = c.corruption_points.get(&sys_id).copied().unwrap_or(0);
                        let s_pts = c.suppression_points.get(&sys_id).copied().unwrap_or(0);
                        let c_stage = ((c_pts as f64 / 200.0).ceil() as u8).clamp(1, 5);
                        let s_stage = ((s_pts as f64 / 200.0).ceil() as u8).clamp(1, 5);
                        let is_fob = c.fob_solar_system_id == sys_id;

                        PyRep::keyval([
                            ("campaignID", PyRep::Integer(c.campaign_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("phase", PyRep::String(phase.into())),
                            ("phaseEndTime", PyRep::Integer(end_time as i64)),
                            ("isFobSystem", PyRep::Bool(is_fob)),
                            ("corruptionStage", PyRep::Integer(i64::from(c_stage))),
                            ("suppressionStage", PyRep::Integer(i64::from(s_stage))),
                            ("corruptionPoints", PyRep::Integer(c_pts)),
                            ("suppressionPoints", PyRep::Integer(s_pts)),
                        ])
                    })
            });

            match snapshot {
                Some(rep) => Some(Ok(rep)),
                None => {
                    // Default snapshot for system outside active campaign
                    Some(Ok(PyRep::keyval([
                        ("campaignID", PyRep::Integer(0)),
                        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("phase", PyRep::String("None".into())),
                        ("phaseEndTime", PyRep::Integer(0)),
                        ("isFobSystem", PyRep::Bool(false)),
                        ("corruptionStage", PyRep::Integer(1)),
                        ("suppressionStage", PyRep::Integer(1)),
                        ("corruptionPoints", PyRep::Integer(0)),
                        ("suppressionPoints", PyRep::Integer(0)),
                    ])))
                }
            }
        }

        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id);
            let instances = read_insurgency_runtime(ctx, |rt| {
                rt.insurgency_instances
                    .iter()
                    .filter(|s| sys_id.map(|sys| s.solar_system_id == sys).unwrap_or(true))
                    .map(|s| {
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(s.instance_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(s.solar_system_id))),
                            ("siteType", PyRep::String(s.site_type.clone())),
                            ("name", PyRep::String(s.name.clone())),
                            ("corruptionReward", PyRep::Real(s.corruption_reward)),
                            ("completed", PyRep::Bool(s.completed)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(instances)))
        }

        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id);
            let instances = read_insurgency_runtime(ctx, |rt| {
                rt.ice_heist_instances
                    .iter()
                    .filter(|h| sys_id.map(|sys| h.solar_system_id == sys).unwrap_or(true))
                    .map(|h| {
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(h.instance_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(h.solar_system_id))),
                            ("pirateTankersSaved", PyRep::Integer(i64::from(h.pirate_tankers_saved))),
                            ("empireTankersSaved", PyRep::Integer(i64::from(h.empire_tankers_saved))),
                            ("completed", PyRep::Bool(h.completed)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(instances)))
        }

        _ => None,
    }
}
