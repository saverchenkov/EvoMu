//! Unit tests for `notify_emitters_tournaments_anomalies`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_expeditions::EscalatingPathEntry;
use crate::services::pve_incursion_sites::{
    IncursionCompletionResult, IncursionSiteEntry, IncursionTaleRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::broadcast;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_online_ctx() -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn tournament_scanning_signal_tracker_and_dungeon_room_triggers() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;

    emit_proving_grounds_match_found(&ctx, char_id);
    emit_pvp_matchmaking_failure(&ctx, char_id, vec!["ShipRestricted".into()]);
    emit_enter_tournament_system(&ctx, char_id);
    emit_leave_tournament_system(&ctx, char_id);

    // Tournament match state: red_pilots get team 0, duplicates in blue/referee are skipped
    emit_tournament_match_state_change(&ctx, 10, 20, 2, &[char_id], &[char_id], &[char_id]);
    emit_tournament_match_state_change(&ctx, 15, 25, 2, &[], &[char_id], &[]);
    emit_tournament_match_state_change(&ctx, 30, 40, 3, &[], &[], &[char_id]);

    emit_cosmic_anomaly_added_for_system(&ctx, sys_id, "ANOM-1", 501, 10_001, "Guristas Hideaway");
    // scan_strength 0.0 is clamped to 0.01
    emit_dungeon_instance_added(&ctx, sys_id, 502, 10_002, "SIG-1", 0.0);
    emit_dungeon_instance_removed(&ctx, sys_id, 502, "SIG-1", 10_002);
    emit_probe_warp_end(&ctx, char_id, 900_001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "ANOM-1".into(),
                solar_system_id: sys_id,
                anomaly_type: "Combat".into(),
                name: "Hideaway".into(),
                signal_strength: 1.0,
                position: [1.0, 2.0, 3.0],
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1_775_400_000,
                lifetime_seconds: 3600,
                cleared: false,
            });
    });
    let sigs = vec![CosmicSignatureRecord {
        signature_id: "ABC-123".into(),
        solar_system_id: sys_id,
        sig_group: "Data Site".into(),
        type_name: "Central Guristas Sparking Transmitter".into(),
        signal_strength: 0.75,
        position: [10.0, 20.0, 30.0],
        wormhole_type_id: None,
        destination_system_id: None,
        max_jump_mass_kg: 0.0,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        created_unix: 1_775_400_000,
        lifetime_seconds: 3600,
        collapsed: false,
    }];
    emit_signal_tracker_full_state_for_system(&ctx, char_id, sys_id, &sigs);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 12);
    assert_eq!(drained[0].event, "OnProvingGroundsMatchFound");
    assert_eq!(drained[1].event, "OnMatchMakingFailure");
    assert_eq!(drained[2].event, "OnEnterTournamentSystem");
    assert_eq!(drained[3].event, "OnLeaveTournamentSystem");
    assert_eq!(
        drained[4].args,
        vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::Integer(0),
            PyRep::Integer(2)
        ]
    );
    assert_eq!(
        drained[5].args,
        vec![
            PyRep::Integer(15),
            PyRep::Integer(25),
            PyRep::Integer(1),
            PyRep::Integer(2)
        ]
    );
    assert_eq!(
        drained[6].args,
        vec![
            PyRep::Integer(30),
            PyRep::Integer(40),
            PyRep::Integer(-1),
            PyRep::Integer(3)
        ]
    );
    assert_eq!(drained[7].event, "OnCosmicAnomalyAdded");
    assert_eq!(drained[8].event, "OnDungeonInstanceAdded");
    assert_eq!(drained[9].event, "OnDungeonInstanceRemoved");
    assert_eq!(drained[10].event, "OnProbeWarpEnd");
    assert_eq!(drained[11].event, "OnSignalTrackerFullState");

    // Dungeon room trigger core helpers
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, _rx) = broadcast::channel(64);
    emit_dungeon_camera_interest_core(&ctx.db, Some(&pend), Some(&tx), 0, 1, 1.0, 2.0);
    assert!(pend.read().unwrap().is_empty());

    emit_dungeon_camera_interest_core(&ctx.db, Some(&pend), Some(&tx), char_id, 10, 1.5, 3.0);
    emit_dungeon_trigger_ambient_sound_core(&ctx.db, Some(&pend), Some(&tx), char_id, 20, "snd", 30);
    emit_dungeon_trigger_audio_core(&ctx.db, Some(&pend), Some(&tx), char_id, 20, "url");
    emit_dungeon_trigger_message_core(&ctx.db, Some(&pend), Some(&tx), char_id, "info", 40);
    emit_dungeon_trigger_music_core(&ctx.db, Some(&pend), Some(&tx), char_id, 20, 50);
    assert_eq!(pend.read().unwrap().get(&char_id).unwrap().len(), 5);
}

#[tokio::test]
async fn tales_incursions_system_effects_deathzone_and_expedition_notifications() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;
    let const_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)
        .unwrap_or(20_000_020);

    emit_progression_left(&ctx, char_id);
    emit_tale_data_for_system(&ctx, sys_id, 901, PyRep::Integer(1));
    emit_tale_influence_update(&ctx, const_id, 901, 0.65);
    emit_tale_remove_for_constellation(&ctx, const_id, 901, 1, 7);
    emit_system_effect_started(&ctx, sys_id, vec![30829]);
    emit_system_wide_effect_stop(&ctx, sys_id, 1, 2, 3);
    emit_component_removed_system_environment(&ctx, sys_id);
    emit_analysis_kredits_change(&ctx, char_id);
    emit_system_activity_updated(&ctx, sys_id);
    emit_space_white_out_start(&ctx, char_id, sys_id);

    // Put character in space in Jita and test DoSimClockRebase & OnOreMinedByOther
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
        }
    });
    emit_ore_mined_by_other(&ctx, sys_id, 90_000_999, 700_001, 500);
    emit_sim_clock_rebase_if_advanced(&ctx, 1_775_399_900, 100);

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnProgressionLeft"));
    assert!(events.contains("OnTaleData"));
    assert!(events.contains("OnInfluenceUpdate"));
    assert!(events.contains("OnTaleRemove"));
    assert!(events.contains("OnSystemEffectStarted"));
    assert!(events.contains("OnSystemWideEffectStop"));
    assert!(events.contains("OnComponentRemovedSystemEnvironment"));
    assert!(events.contains("OnAnalysisKreditsChange"));
    assert!(events.contains("OnSystemActivityUpdated"));
    assert!(events.contains("OnSpaceWhiteOutStart"));
    assert!(events.contains("OnOreMinedByOther"));
    assert!(events.contains("DoSimClockRebase"));

    // Deathzone state transition in abyssal system 32_000_001
    let abyss_sys = 32_000_001_u32;
    let ship_id = 100_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = abyss_sys;
            c.active_ship_id = ship_id;
        }
    });
    let handle = ctx.systems.get_or_spawn(abyss_sys);
    handle.ballpark.write().unwrap().balls.insert(
        ship_id,
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(200_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    handle.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = DVec3::ZERO;
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    let dz_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(dz_notifs.len(), 2);
    assert_eq!(dz_notifs[0].event, "OnDeathzoneStateUpdate");
    assert_ne!(dz_notifs[0].args[2], PyRep::None);
    assert_eq!(dz_notifs[1].args[2], PyRep::None);

    // Incursion & Expedition combat notifications
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
        }
    });
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            const_id,
            IncursionTaleRecord {
                tale_id: 888,
                constellation_id: const_id,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_id,
                influence: 0.5,
                state: "Established".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![IncursionSiteEntry {
                    site_id: "777".into(),
                    dungeon_id: 1,
                    dungeon_name: "Vanguard".into(),
                    solar_system_id: sys_id,
                    tier: "Vanguard".into(),
                    min_pilots: 5,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.05,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: 999_001,
                    boss_type_id: 2933,
                    position: [0.0, 0.0, 0.0],
                    escort_spawns: vec![],
                    boss_destroyed: true,
                    killer_char_id: Some(char_id),
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
    });
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, _rx) = broadcast::channel(64);
    emit_incursion_completion_combat_notifications(
        &ctx.db,
        &ctx.sde,
        Some(&pend),
        Some(&tx),
        &IncursionCompletionResult {
            site_id: "777".into(),
            tier: "Vanguard".into(),
            fleet_size: 1,
            fleet_scale_factor: 1.0,
            isk_per_pilot: 10_000_000,
            concord_lp_per_pilot: 1_000,
            influence: 0.0,
            state: "Ended".into(),
            mothership_spawned: true,
            incursion_ended: true,
        },
    );
    let esc = EscalatingPathEntry {
        instance_id: 42,
        character_id: char_id,
        source_dungeon_id: 100,
        expedition_dungeon_id: 200,
        expedition_name: "Mul-Zatah Monastery".into(),
        destination_solar_system_id: sys_id,
        step_index: 1,
        total_steps: 4,
        journal_text: "Go to Jita".into(),
        created_at_sec: 1_775_400_000,
        expires_at_sec: 1_775_500_000,
        dungeons_entered: vec![],
        journal_entry_template_id: 1,
    };
    emit_expedition_combat_notifications(&ctx.db, &pend, &tx, None, Some(&esc), None);
    let q = pend.read().unwrap();
    let char_q = q.get(&char_id).expect("queued notifications");
    assert!(char_q.iter().any(|n| n.event == "OnInfluenceUpdate"));
    assert!(char_q.iter().any(|n| n.event == "OnTaleRemove"));
    assert!(char_q.iter().any(|n| n.event == "OnDungeonInstanceAdded"));
}
