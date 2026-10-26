//! `insurgency_services` — Pirate Insurgencies, Corruption/Suppression Campaigns & Sites (`§3.11`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::NoticeScope;
use crate::router::NotificationTarget;
use crate::router::ServerContext;
pub use eve_db::{
    PersistedIceHeistRecord as IceHeistRecord,
    PersistedInsurgencyCampaign as InsurgencyCampaign,
    PersistedInsurgencySiteRecord as InsurgencySiteRecord,
    PersistedInsurgencyState as InsurgencyRuntimeState,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub const FACTION_GURISTAS: i64 = 500010;
pub const FACTION_ANGEL_CARTEL: i64 = 500011;

pub const PHASE_FORECASTING: &str = "Forecasting";
pub const PHASE_ACTIVE: &str = "Active";
pub const PHASE_WINDDOWN: &str = "WindDown";

pub const FORECASTING_DURATION_SECS: u64 = 48 * 3600; // 48h
pub const ACTIVE_DURATION_SECS: u64 = 7 * 86400;      // 7 days
pub const WINDDOWN_DURATION_SECS: u64 = 48 * 3600;    // 48h

pub fn with_insurgency_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut InsurgencyRuntimeState) -> R,
) -> R {
    ctx.db
        .with_mut_snapshot(|snap| f(&mut snap.insurgency_runtime))
}

pub fn read_insurgency_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&InsurgencyRuntimeState) -> R,
) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.insurgency_runtime))
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

/// Insurgency stage (0..=5) for corruption / suppression points.
///
/// DERIVED (synthetic — the client stage table is not in the EvoMu SDE import): one stage per
/// 200 points, mirrored by `quasar_space_services::INSURGENCY_STAGE_THRESHOLDS`.
pub fn insurgency_stage(points: i64) -> u8 {
    ((points.max(0) as f64 / 200.0).ceil() as u8).min(5)
}

/// Advances corruption points in a system and updates corruption stage (1..5).
pub fn add_system_corruption(ctx: &ServerContext, campaign_id: i64, sys_id: u32, points: i64) -> (i64, u8) {
    let now = ctx.now_unix();
    let (pts, stage, pirate_faction, campaign_active) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 1, FACTION_GURISTAS, false);
        };
        let active = now < c.winddown_end_unix;
        let p = c.corruption_points.entry(sys_id).or_insert(0);
        *p = (*p + points).max(0);
        let s = insurgency_stage(*p).max(1);
        (*p, s, c.pirate_faction_id, active)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_corruption_stage = stage;
        }
    });

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);

    // Broadcast Quasar IncreasedNotice only when corruption actually increased on an active campaign
    let old_pts = (pts - points).max(0);
    if points > 0 && pts > old_pts && campaign_active {
    let mut q_payload = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, sys_id as u64, &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut q_payload);

    let mut old_f = Vec::new();
    encode_varint_field(1, old_pts as u64, &mut old_f);
    encode_varint_field(2, 1000, &mut old_f);
    encode_bytes_field(2, &old_f, &mut q_payload);

    let mut new_f = Vec::new();
    encode_varint_field(1, pts as u64, &mut new_f);
    encode_varint_field(2, 1000, &mut new_f);
    encode_bytes_field(3, &new_f, &mut q_payload);

    encode_varint_field(4, 1, &mut q_payload);
    encode_varint_field(5, 0, &mut q_payload);

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.corruption.api.IncreasedNotice"),
            value: q_payload,
        },
        ctx.now_unix(),
    );
    }
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
    let now = ctx.now_unix();
    let (pts, stage, campaign_active) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 1, false);
        };
        let active = now < c.winddown_end_unix;
        let p = c.suppression_points.entry(sys_id).or_insert(0);
        *p = (*p + points).max(0);
        let s = insurgency_stage(*p).max(1);
        (*p, s, active)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_suppression_stage = stage;
        }
    });

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);

    // Broadcast Quasar IncreasedNotice only when suppression actually increased on an active campaign
    let old_pts = (pts - points).max(0);
    if points > 0 && pts > old_pts && campaign_active {
    let mut q_payload = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, sys_id as u64, &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut q_payload);

    let mut old_f = Vec::new();
    encode_varint_field(1, old_pts as u64, &mut old_f);
    encode_varint_field(2, 1000, &mut old_f);
    encode_bytes_field(2, &old_f, &mut q_payload);

    let mut new_f = Vec::new();
    encode_varint_field(1, pts as u64, &mut new_f);
    encode_varint_field(2, 1000, &mut new_f);
    encode_bytes_field(3, &new_f, &mut q_payload);

    encode_varint_field(4, 1, &mut q_payload);
    encode_varint_field(5, 0, &mut q_payload);

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.IncreasedNotice"),
            value: q_payload,
        },
        ctx.now_unix(),
    );
    }
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
                        let c_stage = if c_pts == 0 { 0 } else { ((c_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
                        let s_stage = if s_pts == 0 { 0 } else { ((s_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
                        let is_fob = c.fob_solar_system_id == sys_id;

                        PyRep::keyval([
                            ("campaignID", PyRep::Integer(c.campaign_id)),
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("pirateFactionID", PyRep::Integer(c.pirate_faction_id)),
                            ("targetFactionID", PyRep::Integer(c.target_faction_id)),
                            ("fsmState", PyRep::String(phase.into())),
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

            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(sys_id)),
                snapshot.unwrap_or(PyRep::None),
            ])))
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
