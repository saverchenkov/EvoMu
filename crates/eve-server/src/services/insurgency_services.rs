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

pub const PIRATE_STRONGHOLD_GURISTAS_TYPE: u32 = 79172;
pub const PIRATE_STRONGHOLD_ANGEL_TYPE: u32 = 78260;
pub const PIRATE_STRONGHOLD_WRECK_TYPE: u32 = 79386;

pub const PHASE_FORECASTING: &str = "Forecasting";
pub const PHASE_ACTIVE: &str = "Active";
pub const PHASE_WINDDOWN: &str = "WindDown";

/// DERIVED (synthetic): FOB vulnerability window and campaign phase durations based on Havoc expansion defaults.
pub const FORECASTING_DURATION_SECS: u64 = 48 * 3600; // 48h
pub const ACTIVE_DURATION_SECS: u64 = 7 * 86400;      // 7 days
pub const WINDDOWN_DURATION_SECS: u64 = 48 * 3600;    // 48h

/// DERIVED (synthetic): Insurgency complex capture rate balance constants.
pub const MIN_FW_CAPTURE_DURATION_SECONDS: u64 = 600;
pub const MIN_INSURGENCY_CAPTURE_POINTS_DELTA: i64 = 1;
pub const MAX_INSURGENCY_TICK_DELTA_SECS: u64 = 60;
pub const INSURGENCY_CAPTURE_POINTS_TOTAL: u64 = 1000;

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
    let now = ctx.now_unix();
    let snap_opt = read_insurgency_runtime(ctx, |rt| {
        rt.campaigns.get(&campaign_id).map(|c| {
            build_campaign_snapshot_for_system(
                c,
                sys_id,
                now,
                rt.empire_ambition_modifier.get(&c.pirate_faction_id).copied().unwrap_or(0),
            )
        })
    });
    if let Some(snapshot) = snap_opt {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnInsurgencyCampaignStartedForLocation",
            "solarsystemid",
            vec![snapshot],
        );
    }
}

/// Builds the campaign snapshot dictionary expected by client `insurgencyCampaignSvc`.
pub fn build_campaign_snapshot_for_system(c: &InsurgencyCampaign, sys_id: u32, now: u64, emp_mod: u8) -> PyRep {
    let (phase, end_time) = get_campaign_phase_and_timer(c, now);
    let c_pts = c.corruption_points.get(&sys_id).copied().unwrap_or(0);
    let s_pts = c.suppression_points.get(&sys_id).copied().unwrap_or(0);
    let c_stage = if c_pts == 0 { 0 } else { ((c_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
    let s_stage = if s_pts == 0 { 0 } else { ((s_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
    let is_fob = c.fob_solar_system_id == sys_id;
    let s5 = c.suppression_points.values().filter(|&&pts| insurgency_stage(pts) >= 5).count();
    let sys_reps: Vec<PyRep> = c.systems.iter().map(|&s| PyRep::Integer(i64::from(s))).collect();

    PyRep::keyval([
        ("campaignID", PyRep::Integer(c.campaign_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("pirateFactionID", PyRep::Integer(c.pirate_faction_id)),
        ("targetFactionID", PyRep::Integer(c.target_faction_id)),
        ("fobSolarSystemID", PyRep::Integer(i64::from(c.fob_solar_system_id))),
        ("fsmState", PyRep::String(phase.into())),
        ("phase", PyRep::String(phase.into())),
        ("phaseEndTime", PyRep::Integer(end_time as i64)),
        ("isFobSystem", PyRep::Bool(is_fob)),
        ("isFobVulnerable", PyRep::Bool(phase == PHASE_ACTIVE && s5 >= usize::from(5 + emp_mod.min(5)))),
        ("corruptionStage", PyRep::Integer(i64::from(c_stage))),
        ("suppressionStage", PyRep::Integer(i64::from(s_stage))),
        ("corruptionPoints", PyRep::Integer(c_pts)),
        ("suppressionPoints", PyRep::Integer(s_pts)),
        ("coveredSolarsystemIDs", PyRep::List(sys_reps.clone())),
        ("systems", PyRep::List(sys_reps)),
    ])
}

/// Broadcasts `OnInsurgencyCampaignUpdatedForLocation` notification.
pub fn emit_campaign_updated(ctx: &ServerContext, sys_id: u32, campaign_id: i64, _stage: u8) {
    let now = ctx.now_unix();
    let snap = read_insurgency_runtime(ctx, |rt| {
        rt.campaigns.get(&campaign_id).map(|c| build_campaign_snapshot_for_system(c, sys_id, now, rt.empire_ambition_modifier.get(&c.pirate_faction_id).copied().unwrap_or(0)))
    });
    if let Some(snap) = snap {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnInsurgencyCampaignUpdatedForLocation",
            "solarsystemid",
            vec![snap],
        );
    }
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

    let stronghold_id = 1_898_000_000 + campaign_id * 10 + 1;
    with_insurgency_runtime(ctx, |rt| {
        rt.campaigns.insert(campaign_id, campaign);
        let name = if pirate_faction == FACTION_ANGEL_CARTEL {
            "Angel Cartel Stronghold"
        } else {
            "Guristas Stronghold"
        };
        rt.insurgency_instances.push(InsurgencySiteRecord {
            instance_id: stronghold_id,
            solar_system_id: fob_system,
            site_type: "PirateStronghold".into(),
            name: name.into(),
            corruption_reward: 500.0,
            completed: false,
        });
    });

    let type_id = if pirate_faction == FACTION_ANGEL_CARTEL {
        PIRATE_STRONGHOLD_ANGEL_TYPE
    } else {
        PIRATE_STRONGHOLD_GURISTAS_TYPE
    };
    let pos = [100_000.0, 0.0, 50_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        for sys in &systems {
            snap.system_states.entry(*sys).or_default();
        }
        if !snap.items.contains_key(&stronghold_id) {
            snap.items.insert(stronghold_id, eve_db::InventoryItemRecord {
                item_id: stronghold_id,
                type_id,
                owner_id: pirate_faction,
                location_id: fob_system as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            });
            snap.item_positions.insert(stronghold_id, pos);
        }
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

/// Encodes `(field 1: solarsystem.Identifier, field 2: math.Fraction)` for pirate `SetNotice` / `ResetNotice`.
///
/// Schema (`eveProto/generated/eve_public/pirate/{corruption,suppression}/api/notices_pb2.pyj`):
/// - `field 1: system (.eve_public.solarsystem.Identifier { uint64 sequential = 1 })`
/// - `field 2: progress (.eve_public.math.Fraction { uint64 numerator = 1, uint64 denominator = 2 })`
/// DERIVED: denominator `1000` matches `quasar_space_services::INSURGENCY_STAGE_THRESHOLDS` (`[200, 400, 600, 800, 1000]`).
fn encode_pirate_progress_notice(sys_id: u32, numerator: u64, denominator: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, u64::from(sys_id), &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut payload);
    let mut frac = Vec::new();
    encode_varint_field(1, numerator, &mut frac);
    encode_varint_field(2, denominator, &mut frac);
    encode_bytes_field(2, &frac, &mut payload);
    payload
}

/// Encodes `IncreasedNotice` (`eveProto/generated/eve_public/pirate/{corruption,suppression}/api/notices_pb2.pyj`):
/// - `field 1: system (.eve_public.solarsystem.Identifier { uint64 sequential = 1 })`
/// - `field 2: old_corruption / old_suppression (.eve_public.math.Fraction)`
/// - `field 3: new_corruption / new_suppression (.eve_public.math.Fraction)`
/// - `field 4: capsuleers (bool)` — `true` (1) for capsuleer-driven complex/site completions
/// - `field 5: vanguard (bool)` — `false` (0) when not instigated by Vanguard
/// Consumed by `eve/client/script/ui/services/insurgency/messenger.pyj`.
/// DERIVED: denominator `1000` matches `quasar_space_services::INSURGENCY_STAGE_THRESHOLDS`.
fn encode_pirate_increased_notice(sys_id: u32, old_pts: u64, new_pts: u64) -> Vec<u8> {
    let mut q_payload = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, u64::from(sys_id), &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut q_payload);

    let mut old_f = Vec::new();
    encode_varint_field(1, old_pts, &mut old_f);
    encode_varint_field(2, 1000, &mut old_f);
    encode_bytes_field(2, &old_f, &mut q_payload);

    let mut new_f = Vec::new();
    encode_varint_field(1, new_pts, &mut new_f);
    encode_varint_field(2, 1000, &mut new_f);
    encode_bytes_field(3, &new_f, &mut q_payload);

    encode_varint_field(4, 1, &mut q_payload);
    encode_varint_field(5, 0, &mut q_payload);
    q_payload
}

/// Ends an active pirate insurgency campaign and emits `OnInsurgencyCampaignEndingForLocation`,
/// `OnInsurgencyStateChangedForFaction`, and Quasar `ResetNotice` for corruption and suppression.
pub fn end_insurgency_campaign(ctx: &ServerContext, campaign_id: i64) {
    let removed = with_insurgency_runtime(ctx, |rt| rt.campaigns.remove(&campaign_id));
    if let Some(c) = removed {
        let now = ctx.now_unix();
        for sys in &c.systems {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(st) = snap.system_states.get_mut(sys) {
                    st.insurgency_corruption_stage = 0;
                    st.insurgency_suppression_stage = 0;
                }
            });
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
            let reset_payload = encode_pirate_progress_notice(*sys, 0, 1000);
            for notice_type in [
                "eve_public.pirate.corruption.api.ResetNotice",
                "eve_public.pirate.suppression.api.ResetNotice",
            ] {
                let _ = ctx.quasar_gateway.publish_notice(
                    NoticeScope::SolarSystem(*sys),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{notice_type}"),
                        value: reset_payload.clone(),
                    },
                    now,
                );
            }
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
    let (pts, old_stage, stage, pirate_faction, campaign_active, in_active_phase) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 0, 1, FACTION_GURISTAS, false, false);
        };
        let active = now < c.winddown_end_unix;
        let in_act = now >= c.forecasting_end_unix && now < c.active_end_unix;
        let p = c.corruption_points.entry(sys_id).or_insert(0);
        let prev_stage = if *p == 0 { 0 } else { insurgency_stage(*p) };
        *p = (*p + points).max(0);
        let s = insurgency_stage(*p).max(1);
        (*p, prev_stage, s, c.pirate_faction_id, active, in_act)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_corruption_stage = stage;
        }
    });

    if in_active_phase && stage >= 3 && old_stage < 3 {
        let _ = crate::services::fw_gameplay::try_spread_insurgency_from_system(ctx, campaign_id, sys_id);
    }

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);

    // Broadcast Quasar IncreasedNotice when corruption increases, or SetNotice when adjusted downwards
    let old_pts = (pts - points).max(0);
    if campaign_active && pts != old_pts {
        let (notice_type, q_payload) = if points > 0 && pts > old_pts {
            (
                "eve_public.pirate.corruption.api.IncreasedNotice",
                encode_pirate_increased_notice(sys_id, old_pts as u64, pts as u64),
            )
        } else {
            (
                "eve_public.pirate.corruption.api.SetNotice",
                encode_pirate_progress_notice(sys_id, pts as u64, 1000),
            )
        };
        let _ = ctx.quasar_gateway.publish_notice(
            NoticeScope::SolarSystem(sys_id),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{notice_type}"),
                value: q_payload,
            },
            now,
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
        let should_resolve = with_insurgency_runtime(ctx, |rt| {
            let Some(camp) = rt.campaigns.get(&campaign_id) else { return false; };
            if rt.campaign_outcomes.contains_key(&campaign_id) { return false; };
            let p_mod = rt.pirate_ambition_modifier.get(&camp.pirate_faction_id).copied().unwrap_or(0).min(5);
            let c5 = camp.corruption_points.values().filter(|&&p| insurgency_stage(p) >= 5).count();
            c5 >= usize::from(5 + p_mod)
        });
        if should_resolve {
            let _ = crate::services::fw_gameplay::fw_gameplay_insurgency::evaluate_and_resolve_insurgency_campaign(ctx, campaign_id, None);
        }
    }
    (pts, stage)
}

/// Advances suppression points in a system and updates suppression stage (1..5).
pub fn add_system_suppression(ctx: &ServerContext, campaign_id: i64, sys_id: u32, points: i64) -> (i64, u8) {
    let now = ctx.now_unix();
    let (pts, old_stage, stage, campaign_active, in_active_phase) = with_insurgency_runtime(ctx, |rt| {
        let Some(c) = rt.campaigns.get_mut(&campaign_id) else {
            return (0, 0, 1, false, false);
        };
        let active = now < c.winddown_end_unix;
        let in_act = now >= c.forecasting_end_unix && now < c.active_end_unix;
        let p = c.suppression_points.entry(sys_id).or_insert(0);
        let prev_stage = if *p == 0 { 0 } else { insurgency_stage(*p) };
        *p = (*p + points).max(0);
        let s = insurgency_stage(*p).max(1);
        (*p, prev_stage, s, active, in_act)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_suppression_stage = stage;
        }
    });

    if in_active_phase && stage >= 3 && old_stage < 3 {
        let _ = crate::services::fw_gameplay::try_spread_insurgency_from_system(ctx, campaign_id, sys_id);
    }

    emit_campaign_updated(ctx, sys_id, campaign_id, stage);

    // Broadcast Quasar IncreasedNotice when suppression increases, or SetNotice when adjusted downwards
    let old_pts = (pts - points).max(0);
    if campaign_active && pts != old_pts {
        let (notice_type, q_payload) = if points > 0 && pts > old_pts {
            (
                "eve_public.pirate.suppression.api.IncreasedNotice",
                encode_pirate_increased_notice(sys_id, old_pts as u64, pts as u64),
            )
        } else {
            (
                "eve_public.pirate.suppression.api.SetNotice",
                encode_pirate_progress_notice(sys_id, pts as u64, 1000),
            )
        };
        let _ = ctx.quasar_gateway.publish_notice(
            NoticeScope::SolarSystem(sys_id),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{notice_type}"),
                value: q_payload,
            },
            now,
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

/// Spawns a Pirate Stronghold site instance for an insurgency campaign in its FOB system and seeds world items.
// parity: feat:pve.pirate-strongholds
pub fn spawn_pirate_stronghold(ctx: &ServerContext, campaign_id: i64) -> Option<i64> {
    let (fob, fid, id) = with_insurgency_runtime(ctx, |rt| {
        let camp = rt.campaigns.get(&campaign_id)?;
        let fob = camp.fob_solar_system_id;
        let fid = camp.pirate_faction_id;
        let name = if fid == FACTION_ANGEL_CARTEL {
            "Angel Cartel Stronghold"
        } else {
            "Guristas Stronghold"
        };
        let id = 1_898_000_000 + campaign_id * 10 + 1;
        if let Some(existing) = rt.insurgency_instances.iter().find(|s| s.solar_system_id == fob && s.site_type == "PirateStronghold") {
            return Some((fob, fid, existing.instance_id));
        }
        rt.insurgency_instances.push(InsurgencySiteRecord {
            instance_id: id,
            solar_system_id: fob,
            site_type: "PirateStronghold".into(),
            name: name.into(),
            corruption_reward: 500.0,
            completed: false,
        });
        Some((fob, fid, id))
    })?;

    let type_id = if fid == FACTION_ANGEL_CARTEL {
        PIRATE_STRONGHOLD_ANGEL_TYPE
    } else {
        PIRATE_STRONGHOLD_GURISTAS_TYPE
    };
    let pos = [100_000.0, 0.0, 50_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.items.contains_key(&id) {
            snap.items.insert(id, eve_db::InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: fid,
                location_id: fob as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            });
            snap.item_positions.insert(id, pos);
        }
    });

    Some(id)
}

/// Ensures the pirate stronghold is spawned into space with real `CombatState` in its FOB system.
// parity: feat:pve.pirate-strongholds
pub async fn ensure_stronghold_in_space(ctx: &ServerContext, campaign_id: i64) -> Option<i64> {
    let stronghold_id = spawn_pirate_stronghold(ctx, campaign_id)?;
    let (fob_system, pirate_faction) = read_insurgency_runtime(ctx, |rt| {
        let camp = rt.campaigns.get(&campaign_id)?;
        Some((camp.fob_solar_system_id, camp.pirate_faction_id))
    })?;
    let type_id = if pirate_faction == FACTION_ANGEL_CARTEL {
        PIRATE_STRONGHOLD_ANGEL_TYPE
    } else {
        PIRATE_STRONGHOLD_GURISTAS_TYPE
    };
    let sys_handle = ctx.systems.get_or_spawn(fob_system);
    let ball_exists = sys_handle.get_ball_snapshot(stronghold_id).await.map(|b| b.is_some()).unwrap_or(false);
    if !ball_exists {
        let pos = glam::DVec3::new(100_000.0, 0.0, 50_000.0);
        let ball = eve_destiny::Ball {
            id: stronghold_id,
            type_id,
            position: pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let mut cs = ctx.build_type_combat_state(type_id);
        if cs.max_structure_hp <= 0.0 {
            cs.max_shield_hp = 50_000.0;
            cs.shield_hp = 50_000.0;
            cs.max_armor_hp = 50_000.0;
            cs.armor_hp = 50_000.0;
            cs.max_structure_hp = 100_000.0;
            cs.structure_hp = 100_000.0;
        }
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

        let entity = crate::services::combat_crimewatch::ExtendedNpcEntity {
            ball_id: stronghold_id,
            type_id,
            faction_id: pirate_faction as u32,
            archetype: "PirateStronghold".into(),
            solar_system_id: fob_system,
            spawn_group_id: campaign_id as u32,
            target_id: None,
            aggression_threat: HashMap::new(),
            threat_table: HashMap::new(),
            bounty_isk: 30_000_000.0,
            optimal_range: 50_000.0,
            orbit_range: 30_000.0,
            damage_vector: eve_destiny::DamageVector { em: 50.0, explosive: 50.0, kinetic: 50.0, thermal: 50.0 },
            entropic_cycles: 0,
            drifter_overshield_hp: 0.0,
            drifter_doomsday_fired: false,
            web_speed_factor: 0.0,
            web_range: 0.0,
            scram_strength: 0.0,
            scram_range: 0.0,
            neut_amount: 0.0,
            ecm_jammed: false,
            remote_shield_rep: 0.0,
            remote_armor_rep: 0.0,
            remote_rep_range: 0.0,
            tag_drop_type_id: None,
            spawn_position: pos,
            sleeper_wh_class: 0,
            custom_cargo_loot: None,
        };
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(stronghold_id, entity);
        });
    }
    Some(stronghold_id)
}

#[derive(Debug, Clone)]
pub struct StrongholdDestructionInfo {
    pub campaign_id: i64,
    pub solar_system_id: u32,
    pub killer_char_id: Option<i64>,
    pub old_suppression: i64,
    pub new_suppression: i64,
    pub new_suppression_stage: u8,
}

/// Returns true if `ball_id` in `solar_system_id` is an insurgency stronghold that is currently invulnerable
/// (i.e. its campaign is NOT in `Active` phase).
/// DERIVED (synthetic): Strongholds / FOBs are invulnerable during Forecasting and WindDown phases.
// parity: feat:pve.pirate-strongholds
pub fn is_stronghold_invulnerable_in_db(
    db: &eve_db::WorldRepository,
    solar_system_id: u32,
    ball_id: i64,
) -> bool {
    let now = db.clock().now_unix_secs();
    db.with_snapshot(|snap| {
        for camp in snap.insurgency_runtime.campaigns.values() {
            if camp.fob_solar_system_id == solar_system_id {
                let is_sh_id = ball_id == 1_898_000_000 + camp.campaign_id * 10 + 1;
                let is_sh_type = snap.items.get(&ball_id).map(|it| {
                    it.type_id == PIRATE_STRONGHOLD_GURISTAS_TYPE || it.type_id == PIRATE_STRONGHOLD_ANGEL_TYPE
                }).unwrap_or(false);
                let has_site_match = snap.insurgency_runtime.insurgency_instances.iter().any(|s| {
                    s.solar_system_id == solar_system_id
                        && s.site_type == "PirateStronghold"
                        && (s.instance_id == ball_id || is_sh_id || is_sh_type)
                        && !s.completed
                });
                if has_site_match || is_sh_id || is_sh_type {
                    let (phase, _) = get_campaign_phase_and_timer(camp, now);
                    return phase != PHASE_ACTIVE;
                }
            }
        }
        false
    })
}

/// Returns true if `target_id` is an insurgency stronghold in `sys_id` that is invulnerable.
// parity: feat:pve.pirate-strongholds
pub fn is_insurgency_stronghold_invulnerable(
    ctx: &ServerContext,
    sys_id: u32,
    target_id: i64,
) -> bool {
    is_stronghold_invulnerable_in_db(&ctx.db, sys_id, target_id)
}

/// Broadcasts Machonet notifications and Quasar IncreasedNotice when pirate stronghold destruction awards suppression.
pub fn notify_stronghold_destruction(
    db: &eve_db::WorldRepository,
    gw: Option<&crate::quasar_gateway::PublicGatewayRouter>,
    pending_notifications: Option<&std::sync::RwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>>,
    notify_tx: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>>,
    info: &StrongholdDestructionInfo,
) {
    let now = db.clock().now_unix_secs();

    if let Some(gw) = gw {
        let q_payload = encode_pirate_increased_notice(
            info.solar_system_id,
            info.old_suppression as u64,
            info.new_suppression as u64,
        );
        let _ = gw.publish_notice(
            NoticeScope::SolarSystem(info.solar_system_id),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.IncreasedNotice"),
                value: q_payload,
            },
            now,
        );
    }

    let mut system_chars = db.with_snapshot(|snap| {
        let camp_systems = snap.insurgency_runtime.campaigns.get(&info.campaign_id).map(|c| c.systems.clone()).unwrap_or_default();
        snap.characters
            .values()
            .filter(|c| c.solar_system_id == info.solar_system_id || camp_systems.contains(&c.solar_system_id))
            .map(|c| c.character_id)
            .collect::<Vec<_>>()
    });
    if let Some(kid) = info.killer_char_id {
        if !system_chars.contains(&kid) {
            system_chars.push(kid);
        }
    }

    let now_ft = db.clock().now_blueos_filetime_ticks();
    let snap_rep = db.with_snapshot(|snap| {
        snap.insurgency_runtime
            .campaigns
            .get(&info.campaign_id)
            .map(|c| build_campaign_snapshot_for_system(c, info.solar_system_id, now, snap.insurgency_runtime.empire_ambition_modifier.get(&c.pirate_faction_id).copied().unwrap_or(0)))
    }).unwrap_or_else(|| {
        let sys_reps = vec![PyRep::Integer(i64::from(info.solar_system_id))];
        PyRep::keyval([
            ("campaignID", PyRep::Integer(info.campaign_id)),
            ("solarSystemID", PyRep::Integer(i64::from(info.solar_system_id))),
            ("suppressionStage", PyRep::Integer(i64::from(info.new_suppression_stage))),
            ("suppressionPoints", PyRep::Integer(info.new_suppression)),
            ("coveredSolarsystemIDs", PyRep::List(sys_reps.clone())),
            ("systems", PyRep::List(sys_reps)),
        ])
    });
    let update_args = vec![snap_rep];

    let full_supp_args = if info.new_suppression_stage >= 5 {
        Some(vec![
            PyRep::Integer(i64::from(info.solar_system_id)),
            PyRep::Integer(info.new_suppression),
            PyRep::Integer(1000),
        ])
    } else {
        None
    };

    for cid in system_chars {
        let (packet, marshaled_packet) = crate::router::ServerContext::build_macho_notification_packet(
            cid,
            None,
            "OnInsurgencyCampaignUpdatedForLocation",
            "solarsystemid",
            update_args.clone(),
        );
        let notif = crate::router::cache_notifications::PendingMachoNotification {
            recipient_char_id: cid,
            event: "OnInsurgencyCampaignUpdatedForLocation".into(),
            id_type: "solarsystemid".into(),
            args: update_args.clone(),
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        if let Some(pend) = pending_notifications {
            if let Ok(mut map) = pend.write() {
                map.entry(cid).or_default().push(notif.clone());
            }
        }
        if let Some(tx) = notify_tx {
            let _ = tx.send((cid, notif));
        }

        if let Some(ref fs_args) = full_supp_args {
            let (packet_fs, marshaled_packet_fs) = crate::router::ServerContext::build_macho_notification_packet(
                cid,
                None,
                "OnSystemFullySuppressedForFaction",
                "solarsystemid",
                fs_args.clone(),
            );
            let notif_fs = crate::router::cache_notifications::PendingMachoNotification {
                recipient_char_id: cid,
                event: "OnSystemFullySuppressedForFaction".into(),
                id_type: "solarsystemid".into(),
                args: fs_args.clone(),
                packet: packet_fs,
                marshaled_packet: marshaled_packet_fs,
                created_filetime: now_ft,
            };
            if let Some(pend) = pending_notifications {
                if let Ok(mut map) = pend.write() {
                    map.entry(cid).or_default().push(notif_fs.clone());
                }
            }
            if let Some(tx) = notify_tx {
                let _ = tx.send((cid, notif_fs));
            }
        }
    }
}

/// Handles combat destruction of a pirate stronghold in space:
/// verifies the campaign is in `Active` phase (strongholds are invulnerable during `Forecasting` or `WindDown`),
/// marks the site completed, transitions campaign phase to `WindDown`,
/// awards +500 suppression to the FOB solar system, and spawns the FOB wreck (type 79386).
// parity: feat:pve.pirate-strongholds
pub async fn on_insurgency_stronghold_destroyed_core(
    systems: &crate::system_actor::SystemRegistry,
    db: &eve_db::WorldRepository,
    solar_system_id: u32,
    ball_id: i64,
    killer_char_id: Option<i64>,
) -> Option<StrongholdDestructionInfo> {
    let now = db.clock().now_unix_secs();
    let destruction_info = db.with_mut_snapshot(|snap| {
        let mut matched = None;
        for camp in snap.insurgency_runtime.campaigns.values_mut() {
            if camp.fob_solar_system_id == solar_system_id {
                let is_sh_id = ball_id == 1_898_000_000 + camp.campaign_id * 10 + 1;
                let is_sh_type = snap.items.get(&ball_id).map(|it| it.type_id == PIRATE_STRONGHOLD_GURISTAS_TYPE || it.type_id == PIRATE_STRONGHOLD_ANGEL_TYPE).unwrap_or(false);
                let has_site_match = snap.insurgency_runtime.insurgency_instances.iter().any(|s| {
                    s.solar_system_id == solar_system_id
                        && s.site_type == "PirateStronghold"
                        && (s.instance_id == ball_id || is_sh_id || is_sh_type)
                        && !s.completed
                });
                if has_site_match || is_sh_id || is_sh_type {
                    let (phase, _) = get_campaign_phase_and_timer(camp, now);
                    if phase != PHASE_ACTIVE {
                        return None;
                    }
                    camp.forecasting_end_unix = camp.forecasting_end_unix.min(now);
                    camp.active_end_unix = now;
                    camp.winddown_end_unix = now + WINDDOWN_DURATION_SECS;

                    let p = camp.suppression_points.entry(solar_system_id).or_insert(0);
                    let old_supp = *p;
                    *p = (*p + 500).max(0);
                    let new_supp = *p;
                    let s_stage = insurgency_stage(*p).max(1);
                    if let Some(st) = snap.system_states.get_mut(&solar_system_id) {
                        st.insurgency_suppression_stage = s_stage;
                    }
                    matched = Some((camp.campaign_id, old_supp, new_supp, s_stage));
                    break;
                }
            }
        }
        let Some((camp_id, old_supp, new_supp, s_stage)) = matched else {
            return None;
        };

        for site in snap.insurgency_runtime.insurgency_instances.iter_mut() {
            if site.solar_system_id == solar_system_id && site.site_type == "PirateStronghold" {
                site.completed = true;
            }
        }

        let pos = snap.item_positions.remove(&ball_id).unwrap_or([100_000.0, 0.0, 50_000.0]);
        snap.items.remove(&ball_id);

        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;
        snap.items.insert(wreck_id, eve_db::InventoryItemRecord {
            item_id: wreck_id,
            type_id: PIRATE_STRONGHOLD_WRECK_TYPE,
            owner_id: killer_char_id.unwrap_or(500001),
            location_id: solar_system_id as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        snap.item_positions.insert(wreck_id, pos);

        Some((StrongholdDestructionInfo {
            campaign_id: camp_id,
            solar_system_id,
            killer_char_id,
            old_suppression: old_supp,
            new_suppression: new_supp,
            new_suppression_stage: s_stage,
        }, wreck_id, pos))
    });

    let Some((info, wreck_id, pos)) = destruction_info else {
        return None;
    };

    let sys_handle = systems.get_or_spawn(solar_system_id);
    let wreck_ball = eve_destiny::Ball {
        id: wreck_id,
        type_id: PIRATE_STRONGHOLD_WRECK_TYPE,
        position: glam::DVec3::new(pos[0], pos[1], pos[2]),
        velocity: glam::DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(wreck_ball).await;
    Some(info)
}

fn are_fw_factions_allied(a: i64, b: i64) -> bool {
    a == b
        || matches!(
            (a, b),
            (500_001, 500_003) | (500_003, 500_001) | (500_002, 500_004) | (500_004, 500_002)
        )
}

fn fw_militia_side_key(fid: i64, camp_info: Option<(i64, i64)>) -> u8 {
    if let Some((_, pirate_fac)) = camp_info {
        if fid == pirate_fac || fid == FACTION_GURISTAS || fid == FACTION_ANGEL_CARTEL {
            10
        } else {
            20
        }
    } else {
        match fid {
            500_001 | 500_003 => 1,
            500_002 | 500_004 => 2,
            500_010 => 3,
            500_011 => 4,
            _ => 5,
        }
    }
}

/// Evaluates all eligible (online, undocked, non-capsule, uncloaked, `<= 30 km`) militia ships
/// on the FW/insurgency complex grid in `sys_id` once per simulation tick (`feat:pve.insurgency`).
async fn evaluate_system_fw_complex_tick(
    ctx: &ServerContext,
    sys_id: u32,
    explicit_caller: Option<(i64, i64)>,
    forced_delta: Option<u64>,
) -> bool {
    if explicit_caller.is_some_and(|(_, fid)| fid == 0) {
        return false;
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let is_lowsec_warzone = sys_sec > 0.0 && sys_sec < 0.45;

    let Some((beacon_arr, candidates)) = ctx.db.with_snapshot(|snap| {
        let beacon_arr = snap.system_states.get(&sys_id)?.fw_complex_position?;
        let mut cands: Vec<(Option<i64>, i64, i64)> = Vec::new();
        if let Some((req_ship_id, req_fid)) = explicit_caller {
            let pilot = snap
                .characters
                .values()
                .find(|c| c.active_ship_id == req_ship_id)
                .map(|c| (c.character_id, c.station_id.is_some() || c.structure_id.is_some()));
            if pilot.is_some_and(|(_, docked)| docked) {
                return None;
            }
            if snap.items.get(&req_ship_id).is_some_and(|it| it.type_id == 670) {
                return None;
            }
            cands.push((pilot.map(|(cid, _)| cid), req_ship_id, req_fid));
        }
        for c in snap.characters.values() {
            if c.solar_system_id != sys_id || c.station_id.is_some() || c.structure_id.is_some() {
                continue;
            }
            let fid = c.war_faction_id.unwrap_or(0);
            if fid == 0 || !ctx.is_character_online(c.character_id, c.user_id) {
                continue;
            }
            if cands.iter().any(|&(_, sid, _)| sid == c.active_ship_id) {
                continue;
            }
            if snap
                .items
                .get(&c.active_ship_id)
                .is_some_and(|it| it.type_id == 670)
            {
                continue;
            }
            cands.push((Some(c.character_id), c.active_ship_id, fid));
        }
        Some((beacon_arr, cands))
    }) else {
        return false;
    };
    if candidates.is_empty() {
        return false;
    }

    let beacon = glam::DVec3::from_array(beacon_arr);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut on_grid: Vec<(Option<i64>, i64, i64, u64)> = Vec::new();
    for (pilot_cid, cand_ship_id, cand_fid) in candidates {
        let Ok(Some((tick, ball))) = sys_handle.get_ball_snapshot(cand_ship_id).await else {
            continue;
        };
        let in_beacon_range = ball.position.distance(beacon) <= 30_000.0
            || ball.position.distance(glam::DVec3::ZERO) <= 30_000.0;
        if ball.type_id == 670 || !in_beacon_range {
            continue;
        }
        if sys_handle
            .get_combat_state(cand_ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.is_cloaked || cs.structure_hp <= 0.0)
        {
            continue;
        }
        on_grid.push((pilot_cid, cand_ship_id, cand_fid, tick.max(1)));
    }
    if let Some((req_ship_id, _)) = explicit_caller {
        if !on_grid.iter().any(|&(_, sid, _, _)| sid == req_ship_id) {
            return false;
        }
    }
    if on_grid.is_empty() {
        return false;
    }
    let sim_secs = sys_handle.sim_elapsed_secs().round() as u64;
    let effective_tick = sim_secs.max(on_grid.iter().map(|x| x.3).max().unwrap_or(1));

    let pulse = ctx.db.with_mut_snapshot(|snap| {
        let camp_info = snap
            .insurgency_runtime
            .campaigns
            .values()
            .find(|c| c.systems.contains(&sys_id))
            .map(|c| (c.campaign_id, c.pirate_faction_id));
        if camp_info.is_none() && !is_lowsec_warzone {
            return None;
        }
        let occupier_opt = snap
            .fw_system_occupations
            .get(&sys_id)
            .map(|o| o.occupier_faction_id);
        let status = crate::services::fw_services::compute_system_frontline_status(ctx, snap, sys_id);
        let has_occ = occupier_opt.is_some();
        let (occ_adv, attacker_adv) = snap
            .fw_runtime
            .system_advantage
            .get(&sys_id)
            .copied()
            .unwrap_or((0.0, 0.0));

        let st = snap.system_states.get_mut(&sys_id)?;
        if forced_delta.is_none() && effective_tick <= st.last_fw_capture_tick {
            return None;
        }
        let first_side = fw_militia_side_key(on_grid[0].2, camp_info);
        if on_grid
            .iter()
            .any(|&(_, _, f, _)| fw_militia_side_key(f, camp_info) != first_side)
        {
            st.last_fw_capture_tick = effective_tick;
            return None;
        }
        let lead_fid = on_grid[0].2;
        let is_defender = camp_info.is_none()
            && occupier_opt.is_some_and(|occ_fid| are_fw_factions_allied(lead_fid, occ_fid));
        if camp_info.is_none() && is_defender && st.fw_capture_seconds_elapsed == 0 {
            return None;
        }

        let delta = forced_delta.unwrap_or_else(|| {
            if st.last_fw_capture_tick == 0 {
                effective_tick.clamp(1, MAX_INSURGENCY_TICK_DELTA_SECS)
            } else {
                effective_tick
                    .saturating_sub(st.last_fw_capture_tick)
                    .clamp(1, MAX_INSURGENCY_TICK_DELTA_SECS)
            }
        });
        let min_dur = if camp_info.is_some() { 300 } else { MIN_FW_CAPTURE_DURATION_SECONDS };
        let duration = if st.fw_capture_duration_seconds > 0 {
            st.fw_capture_duration_seconds.max(min_dur)
        } else {
            min_dur
        };
        st.last_fw_capture_tick = effective_tick;
        let pts_delta = ((u64::from(delta) * INSURGENCY_CAPTURE_POINTS_TOTAL) / u64::from(duration))
            .max(MIN_INSURGENCY_CAPTURE_POINTS_DELTA as u64) as i64;

        let mut total_lp_awarded = 0i64;
        if is_lowsec_warzone && camp_info.is_none() {
            let (grid_friendly_adv, grid_enemy_adv) = if !has_occ || is_defender {
                (occ_adv, attacker_adv)
            } else {
                (attacker_adv, occ_adv)
            };
            let adv_mult = crate::services::fw_services::calculate_capture_speed_multiplier(
                grid_friendly_adv,
                grid_enemy_adv,
            );
            let capture_advance = ((delta as f64) * adv_mult).round().max(1.0) as u64;
            if is_defender {
                st.fw_capture_seconds_elapsed =
                    st.fw_capture_seconds_elapsed.saturating_sub(capture_advance);
            } else {
                let prev_contested_pct = st.fw_contested_pct;
                st.fw_capture_seconds_elapsed = st
                    .fw_capture_seconds_elapsed
                    .saturating_add(capture_advance)
                    .min(duration);
                if st.fw_capture_seconds_elapsed >= duration {
                    let status_mult = if has_occ {
                        match status {
                            crate::services::fw_services::STATUS_FRONTLINE => 1.5,
                            crate::services::fw_services::STATUS_COMMAND => 1.0,
                            _ => 0.75,
                        }
                    } else if prev_contested_pct > 75.0 {
                        1.5
                    } else if prev_contested_pct > 25.0 {
                        1.0
                    } else {
                        0.75
                    };
                    total_lp_awarded = (15_000.0 * status_mult) as i64;
                    st.fw_complex_position = None;
                    st.fw_capture_seconds_elapsed = 0;
                }
            }
        }
        let pilot_cids: Vec<(i64, i64)> = on_grid
            .iter()
            .filter_map(|&(cid_opt, _, pfid, _)| cid_opt.map(|cid| (cid, pfid)))
            .collect();
        if total_lp_awarded > 0 {
            let n_pilots = pilot_cids.len().max(1) as i64;
            let lp_each = total_lp_awarded / n_pilots;
            for &(cid, pfid) in &pilot_cids {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    let militia_corp = ctx
                        .sde
                        .factions
                        .get(&(pfid as u32))
                        .and_then(|f| f.militia_corporation_id)
                        .map(|c| c as i64)
                        .unwrap_or(pfid);
                    *ch.loyalty_points.entry(militia_corp).or_insert(0) += lp_each;
                }
            }
            let mut new_sys_contested = None;
            if let Some(occ) = snap.fw_system_occupations.get_mut(&sys_id) {
                let vp_entry = snap
                    .fw_runtime
                    .system_victory_points
                    .entry(sys_id)
                    .or_insert(occ.victory_points);
                *vp_entry = vp_entry.saturating_add(20);
                occ.victory_points = *vp_entry;
                let thresh = (occ.victory_points_threshold as f64).max(1.0);
                new_sys_contested = Some(((*vp_entry as f64 / thresh) * 100.0).clamp(0.0, 100.0));
            } else {
                let vp_entry = snap.fw_runtime.system_victory_points.entry(sys_id).or_insert(0);
                *vp_entry = vp_entry.saturating_add(20);
            }
            if let (Some(pct), Some(st2)) = (new_sys_contested, snap.system_states.get_mut(&sys_id)) {
                st2.fw_contested_pct = pct;
            }
        }
        Some((camp_info, pts_delta, lead_fid, pilot_cids, total_lp_awarded))
    });

    if let Some((camp_info, pts_delta, lead_fid, pilot_cids, total_lp_awarded)) = pulse {
        if let Some((camp_id, pirate_fac)) = camp_info {
            if lead_fid == pirate_fac || lead_fid == FACTION_GURISTAS || lead_fid == FACTION_ANGEL_CARTEL {
                let _ = add_system_corruption(ctx, camp_id, sys_id, pts_delta);
            } else {
                let _ = add_system_suppression(ctx, camp_id, sys_id, pts_delta);
            }
            for &(cid, pfid) in &pilot_cids {
                crate::services::fw_gameplay::record_insurgency_contribution(
                    ctx, camp_id, cid, pfid, pts_delta,
                );
            }
        }
        for (cid, pfid) in pilot_cids {
            ctx.quasar_gateway.progression.extra.on_campaign_gameplay_event(
                cid as u32,
                crate::services::quasar_campaign_state::CampaignEvent {
                    method: "CaptureFWComplex",
                    type_id: None,
                    solar_system_id: Some(sys_id),
                    faction_id: Some(pfid),
                },
                1,
                ctx.now_unix(),
                &ctx.quasar_gateway,
            );
            if total_lp_awarded > 0 {
                ctx.quasar_gateway
                    .progression
                    .career_goals
                    .on_gameplay_event(cid as u32, "faction_warfare", 1, &ctx.quasar_gateway);
                crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
                    ctx,
                    cid,
                    &["CaptureFWComplexes", "DefendFWComplexes"],
                    1,
                    Some(sys_id as i64),
                );
            }
        }
        true
    } else {
        false
    }
}

/// Advances FW warzone and insurgency capture progression for an active ship near a FW complex beacon.
/// Invoked strictly by the server simulation tick (never client RPC).
pub async fn step_system_insurgency_capture_tick(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    fid: i64,
) -> bool {
    evaluate_system_fw_complex_tick(ctx, sys_id, Some((ship_id, fid)), None).await
}

/// Steps insurgency capture progression for all active player ships in `sys_id`.
/// Invoked by cluster / system simulation tick (`feat:pve.insurgency`).
// parity: feat:pve.insurgency
pub async fn step_system_insurgency_tick(ctx: &ServerContext, sys_id: u32) {
    step_system_insurgency_tick_delta(ctx, sys_id, None).await;
}

pub async fn step_system_insurgency_tick_delta(ctx: &ServerContext, sys_id: u32, forced_delta: Option<u64>) {
    let has_active_complex = ctx.db.with_snapshot(|snap| {
        snap.insurgency_runtime
            .campaigns
            .values()
            .any(|c| c.systems.contains(&sys_id))
            || snap
                .system_states
                .get(&sys_id)
                .is_some_and(|st| st.fw_complex_position.is_some())
    });
    if !has_active_complex {
        return;
    }
    let _ = evaluate_system_fw_complex_tick(ctx, sys_id, None, forced_delta).await;
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
                        let emp_mod = rt.empire_ambition_modifier.get(&c.pirate_faction_id).copied().unwrap_or(0).min(5);
                        let s5 = c.suppression_points.values().filter(|&&pts| insurgency_stage(pts) >= 5).count();
                        let sys_reps: Vec<PyRep> = c.systems.iter().map(|&s| PyRep::Integer(i64::from(s))).collect();
                        PyRep::keyval([
                            ("campaignID", PyRep::Integer(c.campaign_id)),
                            ("pirateFactionID", PyRep::Integer(c.pirate_faction_id)),
                            ("targetFactionID", PyRep::Integer(c.target_faction_id)),
                            ("fobSolarSystemID", PyRep::Integer(i64::from(c.fob_solar_system_id))),
                            ("phase", PyRep::String(phase.into())),
                            ("phaseEndTime", PyRep::Integer(end_time as i64)),
                            ("systems", PyRep::List(sys_reps)),
                            ("isFobVulnerable", PyRep::Bool(phase == PHASE_ACTIVE && s5 >= usize::from(5 + emp_mod))),
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
                    .map(|c| build_campaign_snapshot_for_system(c, sys_id, now, rt.empire_ambition_modifier.get(&c.pirate_faction_id).copied().unwrap_or(0)))
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
