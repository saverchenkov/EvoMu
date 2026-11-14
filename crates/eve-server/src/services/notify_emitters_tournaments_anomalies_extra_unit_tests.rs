//! Additional branch-coverage and mutation-killing unit tests for `notify_emitters_tournaments_anomalies.rs`.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::ServerContext;
use crate::services::notify_emitters_tournaments_anomalies::*;
use crate::services::pve_expeditions::EscalatingPathEntry;
use crate::services::pve_exploration::{
    with_pve_exploration_reg_db, PveSiteInstance,
};
use crate::services::pve_incursion_sites::{
    IncursionCompletionResult, IncursionSiteEntry, IncursionTaleRecord,
};
use crate::services::pve_special_sites_abyssal::{
    AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::broadcast;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn tournament_dedup_signal_tracker_filtering_and_dungeon_trigger_queue_cap_kill_mutations() {
    let (ctx, char_id) = make_ctx();
    let sys_id = 30_000_142_u32;

    // 1. emit_tournament_match_state_change skips <= 0 IDs and deduplicates across red/blue/referee
    emit_tournament_match_state_change(&ctx, 1, 2, 1, &[0, -5], &[0, -1], &[0, -9]);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. emit_dungeon_instance_added preserves scan_strength > 0.01 and clamps < 0.01 to 0.01
    emit_dungeon_instance_added(&ctx, sys_id, 701, 10_001, "SIG-POS", 0.85);
    emit_dungeon_instance_added(&ctx, sys_id, 702, 10_001, "SIG-NEG", -3.0);
    let inst_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(inst_notifs.len(), 2);
    assert_eq!(
        inst_notifs[0].args[1].dict_get("scanStrengthValue"),
        Some(&PyRep::Real(0.85))
    );
    assert_eq!(
        inst_notifs[1].args[1].dict_get("scanStrengthValue"),
        Some(&PyRep::Real(0.01))
    );

    // 3. emit_signal_tracker_full_state_for_system filters cleared and expired anomalies, collapsed sigs, and other-system structures
    let now = ctx.now_unix();
    let struct_in_sys = 1_080_000_001_i64;
    let struct_other_sys = 1_080_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let anoms = snap.cosmic_anomalies.entry(sys_id).or_default();
        anoms.clear();
        // Cleared -> excluded
        anoms.push(CosmicAnomalyRecord {
            anomaly_id: "CLEARED-1".into(),
            solar_system_id: sys_id,
            anomaly_type: "Combat".into(),
            name: "Cleared".into(),
            signal_strength: 1.0,
            position: [0.0; 3],
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: now,
            lifetime_seconds: 3600,
            cleared: true,
        });
        // Expired (now >= created + lifetime) -> excluded
        anoms.push(CosmicAnomalyRecord {
            anomaly_id: "EXPIRED-1".into(),
            solar_system_id: sys_id,
            anomaly_type: "Combat".into(),
            name: "Expired".into(),
            signal_strength: 1.0,
            position: [0.0; 3],
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: now - 100,
            lifetime_seconds: 100,
            cleared: false,
        });
        // lifetime_seconds == 0 (never expires even when created_unix < now) -> included
        anoms.push(CosmicAnomalyRecord {
            anomaly_id: "ETERNAL-1".into(),
            solar_system_id: sys_id,
            anomaly_type: "Combat".into(),
            name: "Eternal".into(),
            signal_strength: 1.0,
            position: [1.0, 2.0, 3.0],
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: now - 50_000,
            lifetime_seconds: 0,
            cleared: false,
        });

        snap.structures.insert(
            struct_in_sys,
            UpwellStructureRecord {
                structure_id: struct_in_sys,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "InSysAstrahus".into(),
                position: [10.0, 20.0, 30.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_other_sys,
            UpwellStructureRecord {
                structure_id: struct_other_sys,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_002_187,
                name: "OtherSysAstrahus".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let sigs = vec![
        CosmicSignatureRecord {
            signature_id: "COLLAPSED-SIG".into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            signal_strength: 0.5,
            position: [0.0; 3],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 3600,
            collapsed: true,
        },
        CosmicSignatureRecord {
            signature_id: "ACTIVE-SIG".into(),
            solar_system_id: sys_id,
            sig_group: "Relic Site".into(),
            type_name: "Ruined Guristas Monument".into(),
            signal_strength: 0.5,
            position: [4.0, 5.0, 6.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 3600,
            collapsed: false,
        },
    ];
    emit_signal_tracker_full_state_for_system(&ctx, char_id, sys_id, &sigs);
    let st_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(st_notifs.len(), 1);
    if let PyRep::Tuple(parts) = &st_notifs[0].args[1] {
        // parts[0] = anom_dict (only ETERNAL-1), parts[1] = sig_dict (only ACTIVE-SIG), parts[3] = struct_dict (only struct_in_sys)
        if let PyRep::Dict(anoms) = &parts[0] {
            assert_eq!(anoms.len(), 1);
            assert_eq!(anoms[0].0, PyRep::String("ETERNAL-1".into()));
            assert!(
                anoms[0].1.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
                "OnSignalTrackerFullState anomaly must include positive integer dungeonID (#747)"
            );
            assert!(
                anoms[0].1.dict_get("archetypeID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
                "OnSignalTrackerFullState anomaly must include positive integer archetypeID (#747)"
            );
            assert!(
                anoms[0].1.dict_get("factionID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
                "OnSignalTrackerFullState anomaly must include positive integer factionID (#747)"
            );
            assert!(
                anoms[0].1.dict_get("scanStrengthAttribute").and_then(|v| v.as_i64()).is_some_and(|id| id > 0),
                "OnSignalTrackerFullState anomaly must include positive integer scanStrengthAttribute (#747)"
            );
        } else {
            panic!("expected Dict for anoms");
        }
        if let PyRep::Dict(sig_d) = &parts[1] {
            assert_eq!(sig_d.len(), 1);
            assert_eq!(sig_d[0].0, PyRep::String("ACTIVE-SIG".into()));
        } else {
            panic!("expected Dict for sigs");
        }
        if let PyRep::Dict(st_d) = &parts[3] {
            assert!(st_d.iter().any(|(k, _)| *k == PyRep::Integer(struct_in_sys)));
            assert!(!st_d
                .iter()
                .any(|(k, _)| *k == PyRep::Integer(struct_other_sys)));
        } else {
            panic!("expected Dict for structs");
        }
    } else {
        panic!("expected Tuple for full_state");
    }

    // 4. push_dungeon_room_trigger_notify: None channel/map early return & 256 queue cap
    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    let (tx, _rx) = broadcast::channel(512);
    push_dungeon_room_trigger_notify(&ctx.db, None, Some(&tx), char_id, "Evt", vec![]);
    push_dungeon_room_trigger_notify(&ctx.db, Some(&pend), None, char_id, "Evt", vec![]);
    assert!(pend.read().unwrap().is_empty());

    for i in 0..260 {
        push_dungeon_room_trigger_notify(
            &ctx.db,
            Some(&pend),
            Some(&tx),
            char_id,
            "OnDungeonTriggerMusic",
            vec![PyRep::Integer(i)],
        );
    }
    let q = pend.read().unwrap();
    let list = &q[&char_id];
    assert_eq!(list.len(), 256);
    assert_eq!(list.first().unwrap().args, vec![PyRep::Integer(4)]);
    assert_eq!(list.last().unwrap().args, vec![PyRep::Integer(259)]);
}

#[test]
fn ore_mined_sim_clock_rebase_incursion_and_expedition_edge_cases_kill_mutations() {
    let (ctx, char_id) = make_ctx();
    let sys_id = 30_000_142_u32;
    let const_id = ctx.sde.solar_systems[&sys_id].constellation_id;

    // Put char_id in space in sys_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
        }
    });

    // 1. emit_ore_mined_by_other: quantity <= 0 or miner == observer emits nothing
    emit_ore_mined_by_other(&ctx, sys_id, 90_000_999, 700_001, 0);
    emit_ore_mined_by_other(&ctx, sys_id, 90_000_999, 700_001, -10);
    emit_ore_mined_by_other(&ctx, sys_id, char_id, 700_001, 500);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 2. emit_sim_clock_rebase_if_advanced: secs == 0 or prev_unix >= now_unix emits nothing
    let now_u = ctx.now_unix();
    emit_sim_clock_rebase_if_advanced(&ctx, now_u - 100, 0);
    emit_sim_clock_rebase_if_advanced(&ctx, now_u, 100);
    emit_sim_clock_rebase_if_advanced(&ctx, now_u + 50, 100);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Valid rebase verifies exact old_ft and new_ft arithmetic
    let prev_u = now_u - 60;
    emit_sim_clock_rebase_if_advanced(&ctx, prev_u, 60);
    let reb = ctx.drain_notifications_for_char(char_id);
    assert_eq!(reb.len(), 1);
    let expected_old_ft = (prev_u as i64) * 10_000_000 + 116_444_736_000_000_000;
    let expected_new_ft = ctx.now_blueos_filetime_ticks();
    assert_eq!(
        reb[0].args,
        vec![PyRep::Tuple(vec![
            PyRep::Integer(expected_old_ft),
            PyRep::Integer(expected_new_ft),
        ])]
    );

    // 3. emit_incursion_completion_combat_notifications: unknown site_id is no-op; incursion_ended=false skips OnTaleRemove
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            const_id,
            IncursionTaleRecord {
                tale_id: 910,
                constellation_id: const_id,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_id,
                influence: 0.8,
                state: "Established".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-910".into(),
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
            site_id: "UNKNOWN-SITE".into(),
            tier: "Vanguard".into(),
            fleet_size: 1,
            fleet_scale_factor: 1.0,
            isk_per_pilot: 1,
            concord_lp_per_pilot: 1,
            influence: 0.75,
            state: "Established".into(),
            mothership_spawned: false,
            incursion_ended: false,
        },
    );
    assert!(pend.read().unwrap().is_empty());

    emit_incursion_completion_combat_notifications(
        &ctx.db,
        &ctx.sde,
        Some(&pend),
        Some(&tx),
        &IncursionCompletionResult {
            site_id: "INC-910".into(),
            tier: "Vanguard".into(),
            fleet_size: 1,
            fleet_scale_factor: 1.0,
            isk_per_pilot: 1,
            concord_lp_per_pilot: 1,
            influence: 0.75,
            state: "Established".into(),
            mothership_spawned: false,
            incursion_ended: false,
        },
    );
    assert_eq!(pend.read().unwrap()[&char_id].len(), 1);
    assert_eq!(pend.read().unwrap()[&char_id][0].event, "OnInfluenceUpdate");

    // 4. emit_expedition_combat_notifications: cleared_site_id collapses old sig & emits OnDungeonInstanceRemoved;
    //    completed escalation (dungeons_entered.len() == total_steps) is skipped
    let exp_inst_id = 77_i64;
    let old_sig_id = crate::services::pve_expeditions::expedition_sig_id(exp_inst_id, 1);
    with_pve_exploration_reg_db(&ctx.db, now_u, |reg| {
        reg.sites_by_system
            .entry(sys_id)
            .or_default()
            .push(PveSiteInstance {
                site_id: "EXP-77-1".into(),
                dungeon_id: 444,
                solar_system_id: sys_id,
                ..Default::default()
            });
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: old_sig_id.clone(),
                solar_system_id: sys_id,
                sig_group: "Combat Site".into(),
                type_name: "Step 1".into(),
                signal_strength: 0.5,
                position: [0.0; 3],
                wormhole_type_id: None,
                destination_system_id: None,
                max_jump_mass_kg: 0.0,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                created_unix: now_u,
                lifetime_seconds: 3600,
                collapsed: false,
            });
    });

    let finished_esc = EscalatingPathEntry {
        instance_id: exp_inst_id,
        character_id: char_id,
        source_dungeon_id: 444,
        expedition_dungeon_id: 445,
        expedition_name: "Finished Expedition".into(),
        destination_solar_system_id: sys_id,
        step_index: 2,
        total_steps: 2,
        journal_text: String::new(),
        created_at_sec: now_u,
        expires_at_sec: now_u + 3600,
        dungeons_entered: vec![1, 2], // len == total_steps -> filtered out!
        journal_entry_template_id: 1,
    };
    pend.write().unwrap().clear();
    emit_expedition_combat_notifications(
        &ctx.db,
        &pend,
        &tx,
        Some("EXP-77-1"),
        None,
        Some(&finished_esc),
    );
    let q = pend.read().unwrap();
    assert_eq!(q[&char_id].len(), 1);
    assert_eq!(q[&char_id][0].event, "OnDungeonInstanceRemoved");
    ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.signature_id == old_sig_id)
            .unwrap();
        assert!(sig.collapsed);
    });
}

#[tokio::test]
async fn deathzone_room_offset_and_system_bounds_kill_mutations() {
    let (ctx, char_id) = make_ctx();
    let ship_id = 100_000_001_i64;

    // Non-abyssal systems (31_999_999 and 33_000_000) are ignored
    for non_abyss in [30_000_142_u32, 31_999_999_u32, 33_000_000_u32] {
        sync_deathzone_state_notifications(&ctx, non_abyss);
    }
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Abyssal system 32_000_001 with pocket in room 2 (z offset = (2 - 1) * 50_000 = 50_000) and grid_offset [10_000, 0, 0]
    let abyss_sys = 32_000_001_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = abyss_sys;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ship_id;
        }
    });
    let dummy_room = AbyssalRoomRecord {
        room_index: 1,
        dungeon_id: 1,
        dungeon_name: "R".into(),
        npc_ball_ids: vec![],
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    crate::services::pve_special_sites::with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            1,
            AbyssalPocketSession {
                pocket_id: 1,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0; 3],
                grid_offset: [10_000.0, 0.0, 0.0],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2,
                rooms: [dummy_room.clone(), dummy_room.clone(), dummy_room],
                started_at_sec: 1_775_400_000,
                collapse_at_sec: 1_775_401_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });

    // Room 2 center is at (10_000, 0, 50_000). Place ship right at that center -> inside boundary, no notification!
    let handle = ctx.systems.get_or_spawn(abyss_sys);
    handle.ballpark.write().unwrap().balls.insert(
        ship_id,
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 50_000.0),
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
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[tokio::test]
async fn mutant_kill_deathzone_transitions_boundary_and_incursion_tale_remove() {
    let (ctx, char_id) = make_ctx();
    let ship_id = 100_000_002_i64;
    let abyss_sys = 32_000_002_u32;
    let const_id = ctx.sde.solar_systems[&30_000_142].constellation_id;
    let r = crate::services::pve_abyssal_hazards::ABYSSAL_ROOM_BOUNDARY_RADIUS_M;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = abyss_sys;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ship_id;
        }
    });
    let dummy_room = AbyssalRoomRecord {
        room_index: 1,
        dungeon_id: 1,
        dungeon_name: "R".into(),
        npc_ball_ids: vec![],
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    crate::services::pve_special_sites::with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            2,
            AbyssalPocketSession {
                pocket_id: 2,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [0.0; 3],
                grid_offset: [10_000.0, 0.0, 0.0],
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2,
                rooms: [dummy_room.clone(), dummy_room.clone(), dummy_room],
                started_at_sec: 1_775_400_000,
                collapse_at_sec: 1_775_401_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });

    let handle = ctx.systems.get_or_spawn(abyss_sys);
    let center = DVec3::new(10_000.0, 0.0, 50_000.0);
    let set_pos = |pos: DVec3| {
        handle.ballpark.write().unwrap().balls.insert(
            ship_id,
            Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        );
    };

    // 1. Exact boundary radius (distance == r): not outside (> r is false)
    set_pos(center + DVec3::new(r, 0.0, 0.0));
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.abyssal_state.deathzone_active_ships.contains(&ship_id));
    });

    // 2. Step outside boundary (distance == r + 1.0): enters danger, emits keyval, inserts active ship
    set_pos(center + DVec3::new(r + 1.0, 0.0, 0.0));
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    let enter_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(enter_notifs.len(), 1);
    assert_eq!(enter_notifs[0].event, "OnDeathzoneStateUpdate");
    assert_eq!(enter_notifs[0].args[1], PyRep::Integer(ship_id));
    assert_eq!(enter_notifs[0].args[2].dict_get("dangerLevel"), Some(&PyRep::Real(1.0)));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.abyssal_state.deathzone_active_ships.contains(&ship_id));
    });

    // 3. Re-sync while still outside: no duplicate notification and ship remains tracked
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.abyssal_state.deathzone_active_ships.contains(&ship_id));
    });

    // 4. Return inside boundary: emits None payload and removes ship from active deathzone set
    set_pos(center);
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    let exit_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(exit_notifs.len(), 1);
    assert_eq!(exit_notifs[0].event, "OnDeathzoneStateUpdate");
    assert_eq!(exit_notifs[0].args[2], PyRep::None);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.abyssal_state.deathzone_active_ships.contains(&ship_id));
    });

    // 5. Re-sync while still inside: no spurious exit/entry notification
    sync_deathzone_state_notifications(&ctx, abyss_sys);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 6. Verify emit_incursion_completion_combat_notifications emits OnTaleRemove when incursion_ended == true
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_000_142;
    });
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            const_id,
            IncursionTaleRecord {
                tale_id: 911,
                constellation_id: const_id,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: 30_000_142,
                vanguard_solar_system_ids: vec![30_000_142],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: 30_000_142,
                influence: 0.0,
                state: "Withdrawn".into(),
                mothership_spawned: true,
                mothership_defeated: true,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-911".into(),
                    dungeon_id: 2,
                    dungeon_name: "HQ".into(),
                    solar_system_id: 30_000_142,
                    tier: "Headquarters".into(),
                    min_pilots: 10,
                    max_optimal_pilots: 20,
                    base_isk: 30_000_000,
                    base_concord_lp: 3_000,
                    influence_delta: 0.1,
                    locked_until_low_influence: false,
                    completed_count: 1,
                    boss_npc_ball_id: 999_002,
                    boss_type_id: 2933,
                    position: [0.0, 0.0, 0.0],
                    escort_spawns: vec![],
                    boss_destroyed: true,
                    killer_char_id: Some(char_id),
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: true,
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
            site_id: "INC-911".into(),
            tier: "Headquarters".into(),
            fleet_size: 1,
            fleet_scale_factor: 1.0,
            isk_per_pilot: 1,
            concord_lp_per_pilot: 1,
            influence: 0.0,
            state: "Withdrawn".into(),
            mothership_spawned: true,
            incursion_ended: true,
        },
    );
    let inc_notifs = &pend.read().unwrap()[&char_id];
    assert!(inc_notifs.iter().any(|n| n.event == "OnInfluenceUpdate"));
    assert!(inc_notifs.iter().any(|n| n.event == "OnTaleRemove"));
}

#[test]
fn signal_tracker_full_state_excludes_ore_anomaly_and_colliding_site_ids_gh1034() {
    let (ctx, char_id) = make_ctx();
    let sys_id = 30_000_119_u32;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
        }
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "DNG-1347-1897095014".into(),
                    solar_system_id: sys_id,
                    anomaly_type: "OreAnomaly".into(),
                    name: "Ordinary Moon Ore Deposit".into(),
                    signal_strength: 1.0,
                    position: [1_000.0, 2_000.0, 3_000.0],
                    ore_types: vec![45_490],
                    remaining_volume: 100_000.0,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-COLLIDE-1".into(),
                    solar_system_id: sys_id,
                    anomaly_type: "Combat".into(),
                    name: "Hideaway".into(),
                    signal_strength: 1.0,
                    position: [4_000.0, 5_000.0, 6_000.0],
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: now,
                    lifetime_seconds: 3_600,
                    cleared: false,
                },
            ],
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    let sigs = vec![
        CosmicSignatureRecord {
            signature_id: "DNG-1347-1897095014".into(),
            solar_system_id: sys_id,
            sig_group: "OreAnomaly".into(),
            type_name: "Ordinary Moon Ore Deposit".into(),
            signal_strength: 1.0,
            position: [1_000.0, 2_000.0, 3_000.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 86_400,
            collapsed: false,
        },
        CosmicSignatureRecord {
            signature_id: "ICE-MIRROR-1".into(),
            solar_system_id: sys_id,
            sig_group: "IceBelt".into(),
            type_name: "Ice Belt".into(),
            signal_strength: 1.0,
            position: [2_000.0, 3_000.0, 4_000.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 86_400,
            collapsed: false,
        },
        CosmicSignatureRecord {
            signature_id: "SOV-ORE-1".into(),
            solar_system_id: sys_id,
            sig_group: "SovOre".into(),
            type_name: "Large Jaspet Deposit".into(),
            signal_strength: 1.0,
            position: [3_000.0, 4_000.0, 5_000.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 86_400,
            collapsed: false,
        },
        CosmicSignatureRecord {
            signature_id: "ANOM-COLLIDE-1".into(),
            solar_system_id: sys_id,
            sig_group: "Combat".into(),
            type_name: "Hideaway".into(),
            signal_strength: 1.0,
            position: [4_000.0, 5_000.0, 6_000.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 3_600,
            collapsed: false,
        },
        CosmicSignatureRecord {
            signature_id: "REAL-SIG-1".into(),
            solar_system_id: sys_id,
            sig_group: "Relic Site".into(),
            type_name: "Ruined Guristas Monument".into(),
            signal_strength: 0.0,
            position: [7_000.0, 8_000.0, 9_000.0],
            wormhole_type_id: None,
            destination_system_id: None,
            max_jump_mass_kg: 0.0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            created_unix: now,
            lifetime_seconds: 3_600,
            collapsed: false,
        },
    ];

    emit_signal_tracker_full_state_for_system(&ctx, char_id, sys_id, &sigs);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnSignalTrackerFullState");
    let PyRep::Tuple(ref parts) = notifs[0].args[1] else {
        panic!("expected 4-tuple fullState, got {:?}", notifs[0].args[1]);
    };
    let PyRep::Dict(ref anoms) = parts[0] else {
        panic!("expected anom Dict, got {:?}", parts[0]);
    };
    let PyRep::Dict(ref out_sigs) = parts[1] else {
        panic!("expected sig Dict, got {:?}", parts[1]);
    };
    let anom_keys: Vec<&str> = anoms.iter().filter_map(|(k, _)| k.as_str()).collect();
    let sig_keys: Vec<&str> = out_sigs.iter().filter_map(|(k, _)| k.as_str()).collect();
    assert!(anom_keys.contains(&"DNG-1347-1897095014"));
    assert!(anom_keys.contains(&"ANOM-COLLIDE-1"));
    assert_eq!(
        sig_keys,
        vec!["REAL-SIG-1"],
        "anomaly-mirrored/colliding IDs must never appear in OnSignalTrackerFullState signatures (#1034)"
    );
}
