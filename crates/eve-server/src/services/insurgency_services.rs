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
pub const MIN_INSURGENCY_CAPTURE_POINTS_DELTA: i64 = 50;
pub const MAX_INSURGENCY_TICK_DELTA_SECS: u64 = 30;
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

/// Builds the campaign snapshot dictionary expected by client `insurgencyCampaignSvc`.
pub fn build_campaign_snapshot_for_system(c: &InsurgencyCampaign, sys_id: u32, now: u64) -> PyRep {
    let (phase, end_time) = get_campaign_phase_and_timer(c, now);
    let c_pts = c.corruption_points.get(&sys_id).copied().unwrap_or(0);
    let s_pts = c.suppression_points.get(&sys_id).copied().unwrap_or(0);
    let c_stage = if c_pts == 0 { 0 } else { ((c_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
    let s_stage = if s_pts == 0 { 0 } else { ((s_pts as f64 / 200.0).ceil() as u8).clamp(1, 5) };
    let is_fob = c.fob_solar_system_id == sys_id;
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
        // DERIVED (synthetic): FOB is vulnerable exclusively during the Active campaign phase
        ("isFobVulnerable", PyRep::Bool(phase == PHASE_ACTIVE)),
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
        rt.campaigns.get(&campaign_id).map(|c| build_campaign_snapshot_for_system(c, sys_id, now))
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

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
            .map(|c| build_campaign_snapshot_for_system(c, info.solar_system_id, now))
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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

/// Destroys a pirate stronghold, marking the site completed and affecting campaign state.
///
/// Acceptance: "Pirate strongholds spawn during insurgency campaigns and their destruction affects campaign state."
// parity: feat:pve.pirate-strongholds
pub fn destroy_pirate_stronghold(
    ctx: &ServerContext,
    campaign_id: i64,
    stronghold_id: Option<i64>,
) -> bool {
    let now = ctx.now_unix();
    let fob_sys = with_insurgency_runtime(ctx, |rt| {
        let camp = rt.campaigns.get_mut(&campaign_id)?;
        let (phase, _) = get_campaign_phase_and_timer(camp, now);
        if phase != PHASE_ACTIVE {
            return None;
        }
        let fob = camp.fob_solar_system_id;
        if camp.active_end_unix > now {
            camp.forecasting_end_unix = camp.forecasting_end_unix.min(now);
            camp.active_end_unix = now;
            camp.winddown_end_unix = now + WINDDOWN_DURATION_SECS;
        }
        for site in rt.insurgency_instances.iter_mut() {
            if site.solar_system_id == fob
                && site.site_type == "PirateStronghold"
                && (stronghold_id.is_none() || Some(site.instance_id) == stronghold_id)
            {
                site.completed = true;
            }
        }
        Some(fob)
    });

    if let Some(sys_id) = fob_sys {
        let (_pts, stage) = add_system_suppression(ctx, campaign_id, sys_id, 500);
        emit_campaign_updated(ctx, sys_id, campaign_id, stage);
        true
    } else {
        false
    }
}

/// Advances insurgency capture progression for an active ship near a FW complex beacon.
/// Can be invoked by system tick / capture hooks.
pub async fn step_system_insurgency_capture_tick(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    fid: i64,
) -> bool {
    let Ok(Some((tick, ship_ball))) = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship_id).await else {
        return false;
    };
    let effective_tick = tick.max(1);
    let camp_pulse = ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.values().find(|c| c.systems.contains(&sys_id))?;
        let camp_id = camp.campaign_id;
        let pirate_fac = camp.pirate_faction_id;
        let st = snap.system_states.get_mut(&sys_id)?;
        let beacon = glam::DVec3::from_array(st.fw_complex_position?);
        if ship_ball.position.distance(beacon) > 30_000.0 || effective_tick <= st.last_fw_capture_tick {
            return None;
        }
        let delta = if st.last_fw_capture_tick == 0 {
            1
        } else {
            effective_tick
                .saturating_sub(st.last_fw_capture_tick)
                .clamp(1, MAX_INSURGENCY_TICK_DELTA_SECS)
        };
        let duration = st.fw_capture_duration_seconds.max(MIN_FW_CAPTURE_DURATION_SECONDS);
        st.last_fw_capture_tick = effective_tick;
        let pts_delta = ((u64::from(delta) * INSURGENCY_CAPTURE_POINTS_TOTAL) / u64::from(duration))
            .max(MIN_INSURGENCY_CAPTURE_POINTS_DELTA as u64) as i64;
        Some((camp_id, pirate_fac, pts_delta))
    });
    if let Some((camp_id, pirate_fac, pts_delta)) = camp_pulse {
        if fid == pirate_fac || fid == FACTION_GURISTAS || fid == FACTION_ANGEL_CARTEL {
            let _ = add_system_corruption(ctx, camp_id, sys_id, pts_delta);
        } else {
            let _ = add_system_suppression(ctx, camp_id, sys_id, pts_delta);
        }
        true
    } else {
        false
    }
}

/// Steps insurgency capture progression for all active player ships in `sys_id`.
/// Invoked by cluster / system simulation tick (`feat:pve.insurgency`).
// parity: feat:pve.insurgency
pub async fn step_system_insurgency_tick(ctx: &ServerContext, sys_id: u32) {
    let has_campaign = read_insurgency_runtime(ctx, |rt| {
        rt.campaigns.values().any(|c| c.systems.contains(&sys_id))
    });
    if !has_campaign {
        return;
    }
    let ships: Vec<(i64, i64)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| c.solar_system_id == sys_id && c.station_id.is_none() && c.structure_id.is_none())
            .filter_map(|c| {
                let fid = c.war_faction_id.unwrap_or(0);
                if fid != 0 {
                    Some((c.active_ship_id, fid))
                } else {
                    None
                }
            })
            .collect()
    });
    for (ship_id, fid) in ships {
        let _ = step_system_insurgency_capture_tick(ctx, sys_id, ship_id, fid).await;
    }
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
                    .map(|c| build_campaign_snapshot_for_system(c, sys_id, now))
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
