//! Additional unit and mutation-killing tests for `cluster_tick_downtime`.

use super::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity,
};
use crate::sp6_lifecycle::{
    with_mut_sp6_state, with_sp6_state, PersistedCombatHp, PersistedSpaceObjectMeta,
};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::DamageVector;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[tokio::test]
async fn downtime_wreck_filter_system_bounds_group186_and_mission_target_counts() {
    let ctx = test_ctx();

    // Empty run returns (0, 0) when no wrecks or Accepted missions exist.
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.clear();
    });
    assert_eq!(cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx), (0, 0));

    // Find an SDE wreck type with group_id == 186 and type_id != 26557.
    let sde_wreck_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186 && t.type_id != 26557)
        .map(|t| t.type_id)
        .unwrap_or(26557);

    let w_low_bound = 910_000_001; // location_id == 30_000_000 (inclusive lower bound -> removed)
    let w_high_bound = 910_000_002; // location_id == 34_000_000 (inclusive upper bound -> removed)
    let w_below_bound = 910_000_003; // location_id == 29_999_999 (excluded -> kept)
    let w_above_bound = 910_000_004; // location_id == 34_000_001 (excluded -> kept)
    let w_non_wreck_type = 910_000_005; // group != 186, type != 26557, is_npc_wreck = true (kept)
    let w_other_child = 910_000_006; // child inside w_below_bound (kept)

    let char_id = 90_000_001;
    let m1_agent = 3_010_001u32; // Uses explicit mission_initial_npc_counts = 7
    let m2_agent = 3_010_002u32; // Uses spawned_npc_ball_ids.len() = 6 (> npcs_remaining=2, > 4)
    let m3_agent = 3_010_003u32; // Uses npcs_remaining = 5 (> spawned_npc_ball_ids.len()=1, > 4)

    with_mut_sp6_state(&ctx.db, |st, snap| {
        for (wid, tid, loc, is_npc) in [
            (w_low_bound, sde_wreck_tid, 30_000_000i64, true),
            (w_high_bound, 26557u32, 34_000_000i64, true),
            (w_below_bound, 26557u32, 29_999_999i64, true),
            (w_above_bound, 26557u32, 34_000_001i64, true),
            (w_non_wreck_type, 587u32, 30_000_142i64, true),
        ] {
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: tid,
                    owner_id: 1_000_125,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(wid, [10.0, 20.0, 30.0]);
            st.space_object_meta.insert(
                wid,
                PersistedSpaceObjectMeta {
                    is_npc_wreck: is_npc,
                    ..Default::default()
                },
            );
            st.ship_combat_hp.insert(
                wid,
                PersistedCombatHp {
                    shield_hp: 0.0,
                    armor_hp: 0.0,
                    hull_hp: 100.0,
                },
            );
        }
        snap.items.insert(
            w_other_child,
            InventoryItemRecord {
                item_id: w_other_child,
                type_id: 34,
                owner_id: 1_000_125,
                location_id: w_below_bound,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Missions with distinct target_count branches and pre-existing awaiting key.
        snap.active_missions.insert(
            (char_id, m1_agent),
            ActiveMissionRecord {
                mission_id: 601,
                character_id: char_id,
                agent_id: m1_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 2,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![910_100_001],
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:601"), 7);
        st.mission_initial_npc_types
            .insert(format!("{char_id}:601"), vec![10_019, 10_020]);
        st.mission_initial_npc_types
            .insert("stale:999".into(), vec![10_019]);
        // Pre-populate awaiting key to verify no duplicate is pushed.
        st.missions_awaiting_downtime_respawn
            .push((char_id, m1_agent));

        snap.active_missions.insert(
            (char_id, m2_agent),
            ActiveMissionRecord {
                mission_id: 602,
                character_id: char_id,
                agent_id: m2_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 3,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![1, 2, 3, 4, 5, 6],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, m3_agent),
            ActiveMissionRecord {
                mission_id: 603,
                character_id: char_id,
                agent_id: m3_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 4,
                npcs_remaining: 5,
                spawned_npc_ball_ids: vec![7],
                bookmark_id: None,
            },
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            w_low_bound,
            ExtendedNpcEntity {
                ball_id: w_low_bound,
                type_id: sde_wreck_tid,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: 30_000_000,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 0.0,
                optimal_range: 1_000.0,
                orbit_range: 500.0,
                damage_vector: DamageVector::default(),
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
                custom_cargo_loot: None,
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
            },
        );
    });

    let (wrecks_removed, missions_reset) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(wrecks_removed, 2);
    assert_eq!(missions_reset, 3);

    with_sp6_state(&ctx.db, |st| {
        assert!(!st.ship_combat_hp.contains_key(&w_low_bound));
        assert!(!st.ship_combat_hp.contains_key(&w_high_bound));
        assert!(st.ship_combat_hp.contains_key(&w_below_bound));
        assert!(st.ship_combat_hp.contains_key(&w_above_bound));
        assert!(st.ship_combat_hp.contains_key(&w_non_wreck_type));

        assert_eq!(
            st.mission_initial_npc_counts.get(&format!("{char_id}:601")),
            Some(&7)
        );
        assert!(st
            .mission_initial_npc_types
            .contains_key(&format!("{char_id}:601")));
        assert!(!st.mission_initial_npc_types.contains_key("stale:999"));

        let m1_await_count = st
            .missions_awaiting_downtime_respawn
            .iter()
            .filter(|&&k| k == (char_id, m1_agent))
            .count();
        assert_eq!(m1_await_count, 1);
    });

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&w_low_bound));
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&w_low_bound));
        assert!(!snap.items.contains_key(&w_high_bound));
        assert!(snap.items.contains_key(&w_below_bound));
        assert!(snap.items.contains_key(&w_above_bound));
        assert!(snap.items.contains_key(&w_non_wreck_type));
        assert!(snap.items.contains_key(&w_other_child));

        assert_eq!(snap.active_missions[&(char_id, m1_agent)].npcs_remaining, 7);
        assert_eq!(snap.active_missions[&(char_id, m2_agent)].npcs_remaining, 6);
        assert_eq!(snap.active_missions[&(char_id, m3_agent)].npcs_remaining, 5);
    });
}

#[tokio::test]
async fn spawn_mission_wave_handles_custom_types_fallback_stats_and_geometry() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let agent_id = 3_020_001u32;

    // Pick one valid SDE NPC type and one synthetic unmapped type ID (999_888_777) to hit the fallback tuple.
    let valid_npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11 && t.faction_id.is_some())
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let fallback_npc_tid = 999_888_777u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.clear();
    });

    with_mut_sp6_state(&ctx.db, |st, snap| {
        st.missions_awaiting_downtime_respawn.clear();
        // Mission needs respawn via (spawned_npc_ball_ids.is_empty() && npcs_remaining > 0) even without awaiting flag!
        snap.active_missions.insert(
            (char_id, agent_id),
            ActiveMissionRecord {
                mission_id: 701,
                character_id: char_id,
                agent_id: agent_id as i64,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 2,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:701"), 4);
        st.mission_initial_npc_types.insert(
            format!("{char_id}:701"),
            vec![valid_npc_tid, fallback_npc_tid],
        );
    });

    // Wrong character or wrong solar system spawns 0 NPCs.
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id + 1, sys_id, DVec3::ZERO),
        0
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id + 1, DVec3::ZERO),
        0
    );

    let origin = DVec3::new(10_000.0, 200.0, -5_000.0);
    let spawned = spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, origin);
    assert_eq!(spawned, 4);

    // Calling again immediately spawns 0 because spawned_npc_ball_ids is now non-empty and not awaiting.
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, origin),
        0
    );

    let bids = ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, agent_id)];
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 4);
        m.spawned_npc_ball_ids.clone()
    });
    assert_eq!(bids.len(), 4);

    with_combat_runtime(&ctx, |rt| {
        // idx 0 -> valid_npc_tid at angle 0: origin + (4500, 0, 0)
        let e0 = &rt.npc_entities[&bids[0]];
        assert_eq!(e0.type_id, valid_npc_tid);
        assert!((e0.spawn_position.x - (origin.x + 4_500.0)).abs() < 1e-6);
        assert!((e0.spawn_position.y - origin.y).abs() < 1e-6);
        assert!((e0.spawn_position.z - origin.z).abs() < 1e-6);
        assert!(e0.orbit_range >= 2_500.0 && e0.orbit_range <= 15_000.0);
        assert!(e0.bounty_isk >= 10_000.0);

        // idx 1 -> fallback_npc_tid at angle PI/2: origin + (0, 0, 4500)
        let e1 = &rt.npc_entities[&bids[1]];
        assert_eq!(e1.type_id, fallback_npc_tid);
        assert_eq!(e1.faction_id, 500_010);
        assert_eq!(e1.bounty_isk, 25_000.0);
        assert_eq!(e1.optimal_range, 10_000.0);
        assert_eq!(e1.orbit_range, 6_000.0);
        assert_eq!(e1.damage_vector, DamageVector::new(12.0, 15.0, 15.0, 12.0));
        assert!((e1.spawn_position.x - origin.x).abs() < 1e-6);
        assert!((e1.spawn_position.z - (origin.z + 4_500.0)).abs() < 1e-6);

        // idx 2 -> valid_npc_tid (cycles modulo 2) at angle PI: origin + (-4500, 0, 0)
        let e2 = &rt.npc_entities[&bids[2]];
        assert_eq!(e2.type_id, valid_npc_tid);
        assert!((e2.spawn_position.x - (origin.x - 4_500.0)).abs() < 1e-6);
    });
}

fn seeded_downtime_ctx() -> ServerContext {
    let base = test_ctx();
    base.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[tokio::test]
async fn recover_scanner_probes_on_downtime_covers_expiry_launcher_cargo_and_missing_ship() {
    let ctx = seeded_downtime_ctx();
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let now_unix = 1_700_005_000u64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(now_unix),
    ));

    let ship_room = 920_000_001i64;
    let launcher_ok = 920_000_010i64;
    let charge_ok = 920_000_011i64;
    let launcher_full = 920_000_012i64;
    let charge_full = 920_000_013i64;
    let launcher_mismatch = 920_000_014i64;
    let charge_mismatch = 920_000_015i64;
    let launcher_empty = 920_000_016i64;

    let ship_full_cargo = 920_000_020i64;
    let cargo_blocker = 920_000_021i64;
    let ship_exact_cargo = 920_000_030i64;
    let cargo_nearly_full = 920_000_031i64;
    let cargo_probe_stack = 920_000_032i64;
    let missing_ship = 920_999_999i64;

    let probe_vol = ctx
        .sde
        .types
        .get(&30_013)
        .map(|t| t.volume.max(0.01))
        .unwrap_or(0.1);
    let tritanium_tid = 34u32;
    let trit_vol = ctx
        .sde
        .types
        .get(&tritanium_tid)
        .map(|t| t.volume)
        .unwrap_or(0.01);
    let ship_cap = ctx
        .sde
        .types
        .get(&587)
        .map(|t| t.capacity)
        .unwrap_or(140.0);

    ctx.db.with_mut_snapshot(|snap| {
        for sid in [ship_room, ship_full_cargo, ship_exact_cargo] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Launchers on ship_room:
        // HiSlot0: 7 probes in launcher -> recovers to 8
        snap.items.insert(
            launcher_ok,
            InventoryItemRecord {
                item_id: launcher_ok,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_ok,
            InventoryItemRecord {
                item_id: charge_ok,
                type_id: 30_013,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot0,
                quantity: 7,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // HiSlot1: already 8 probes -> overflows to cargo
        snap.items.insert(
            launcher_full,
            InventoryItemRecord {
                item_id: launcher_full,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_full,
            InventoryItemRecord {
                item_id: charge_full,
                type_id: 30_013,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot1,
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // HiSlot2: combat probes 30028 loaded -> mismatch with 30013 -> overflows to cargo
        snap.items.insert(
            launcher_mismatch,
            InventoryItemRecord {
                item_id: launcher_mismatch,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_mismatch,
            InventoryItemRecord {
                item_id: charge_mismatch,
                type_id: 30_028,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot2,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // HiSlot3: empty launcher -> loads directly into HiSlot3
        snap.items.insert(
            launcher_empty,
            InventoryItemRecord {
                item_id: launcher_empty,
                type_id: 17_938,
                owner_id: char_id,
                location_id: ship_room,
                flag: InventoryFlag::HiSlot3,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        // Full cargo on ship_full_cargo (remaining < probe_vol)
        let full_qty = ((ship_cap / trit_vol).floor() as i32).max(1) + 5;
        snap.items.insert(
            cargo_blocker,
            InventoryItemRecord {
                item_id: cargo_blocker,
                type_id: tritanium_tid,
                owner_id: char_id,
                location_id: ship_full_cargo,
                flag: InventoryFlag::Cargo,
                quantity: full_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Exact remaining capacity for 1 probe + existing probe stack on ship_exact_cargo
        // Leave enough room for 1 probe_vol (and slightly under 2 * probe_vol)
        let target_used = (ship_cap - probe_vol * 1.25).max(0.0);
        let near_qty = ((target_used - probe_vol).max(0.0) / trit_vol).floor() as i32;
        if near_qty > 0 {
            snap.items.insert(
                cargo_nearly_full,
                InventoryItemRecord {
                    item_id: cargo_nearly_full,
                    type_id: tritanium_tid,
                    owner_id: char_id,
                    location_id: ship_exact_cargo,
                    flag: InventoryFlag::Cargo,
                    quantity: near_qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            cargo_probe_stack,
            InventoryItemRecord {
                item_id: cargo_probe_stack,
                type_id: 30_013,
                owner_id: char_id,
                location_id: ship_exact_cargo,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let base_probe = |probe_id: i64, ship_id: i64, launcher_module_id: i64| {
        crate::router::ActiveScannerProbe {
            probe_id,
            probe_type_id: 30_013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 1,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: now_unix - 100,
            state: 2,
            warp_end_ft: Some(123_456),
        }
    };

    {
        let mut probes = ctx.active_scanner_probes.write().unwrap();
        // Expired by state < 0
        let mut p_neg_state = base_probe(921_001, ship_room, launcher_ok);
        p_neg_state.state = -1;
        probes.insert(p_neg_state.probe_id, p_neg_state);

        // Expired by exact boundary now_unix == launched_at_unix + max_flight_time_s
        let mut p_expired_boundary = base_probe(921_002, ship_room, launcher_ok);
        p_expired_boundary.launched_at_unix = now_unix - 3_600;
        probes.insert(p_expired_boundary.probe_id, p_expired_boundary);

        // Alive (1 sec before expiry): merges into charge_ok (7 -> 8)
        let mut p_recover_launcher = base_probe(921_003, ship_room, launcher_ok);
        p_recover_launcher.launched_at_unix = now_unix - 3_599;
        probes.insert(p_recover_launcher.probe_id, p_recover_launcher);

        // Launcher full (8/8) -> spills to Cargo (new stack)
        probes.insert(921_004, base_probe(921_004, ship_room, launcher_full));

        // Mismatched launcher charge -> spills to Cargo (merges into new stack)
        probes.insert(921_005, base_probe(921_005, ship_room, launcher_mismatch));

        // Empty launcher -> inserts into HiSlot3
        probes.insert(921_006, base_probe(921_006, ship_room, launcher_empty));

        // Full cargo -> stays in active_scanner_probes with state = 0, warp_end_ft = None
        probes.insert(921_007, base_probe(921_007, ship_full_cargo, 0));

        // Existing cargo stack fits +1 on exact_cargo
        probes.insert(921_008, base_probe(921_008, ship_exact_cargo, 0));

        // Second probe on exact_cargo overflows cargo -> stays active with state = 0
        probes.insert(921_009, base_probe(921_009, ship_exact_cargo, 0));

        // Missing ship_id -> stays active with state = 0
        probes.insert(921_010, base_probe(921_010, missing_ship, 0));
    }

    let (recovered, expired_or_lost) = super::recover_scanner_probes_on_downtime(&ctx);
    assert_eq!(recovered, 5);
    assert_eq!(expired_or_lost, 2);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&charge_ok].quantity, 8);
        assert_eq!(snap.items[&cargo_probe_stack].quantity, 2);
        // Empty launcher HiSlot3 got 1 probe of 30_013
        assert!(snap.items.values().any(|it| {
            it.location_id == ship_room
                && it.flag == InventoryFlag::HiSlot3
                && it.type_id == 30_013
                && it.quantity == 1
                && !it.singleton
        }));
        // Two spilled probes in ship_room Cargo (launcher_full + launcher_mismatch)
        let room_cargo_probes: i32 = snap
            .items
            .values()
            .filter(|it| {
                it.location_id == ship_room
                    && it.flag == InventoryFlag::Cargo
                    && it.type_id == 30_013
            })
            .map(|it| it.quantity)
            .sum();
        assert_eq!(room_cargo_probes, 2);
    });

    // Unrecovered probes stay in active_scanner_probes with state = 0 and warp_end_ft = None
    {
        let probes = ctx.active_scanner_probes.read().unwrap();
        assert!(!probes.contains_key(&921_001));
        assert!(!probes.contains_key(&921_002));
        for kept in [921_007, 921_009, 921_010] {
            let p = &probes[&kept];
            assert_eq!(p.state, 0);
            assert_eq!(p.warp_end_ft, None);
        }
    }
}

#[tokio::test]
async fn flush_bounties_safe_logoff_and_pocket_entry_mission_edge_paths() {
    let ctx = seeded_downtime_ctx();
    let c1 = 90_000_001i64;
    let c2 = 90_000_002i64;
    let sys1 = 30_000_142u32;
    let sys2 = 30_000_144u32;

    // 1. flush_pending_bounty_ticks_on_downtime skips <= 0.0 and flushes positive across systems/chars
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.clear();
        rt.pending_bounty_ticks.insert((sys2, c2), 12_500.0);
        rt.pending_bounty_ticks.insert((sys1, c1), 25_000.0);
        rt.pending_bounty_ticks.insert((sys1, c2), 0.0);
        rt.pending_bounty_ticks.insert((sys2, c1), -5.0);
    });
    let flushed = super::flush_pending_bounty_ticks_on_downtime(&ctx);
    assert_eq!(flushed, 2);
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.pending_bounty_ticks.contains_key(&(sys1, c1)));
        assert!(!rt.pending_bounty_ticks.contains_key(&(sys2, c2)));
    });

    // 2. clear_safe_logoff and disconnect_safe_logoff_sessions
    let ship1 = 930_000_001i64;
    let ship2 = 930_000_002i64;
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.safe_logoffs.insert(
            c1,
            crate::sp6_lifecycle::SafeLogoffEntry {
                char_id: c1,
                ship_id: ship1,
                solar_system_id: sys1,
                started_unix: 100,
                complete_at_unix: 130,
                completed: false,
            },
        );
        st.safe_logoffs.insert(
            c2,
            crate::sp6_lifecycle::SafeLogoffEntry {
                char_id: c2,
                ship_id: ship2,
                solar_system_id: sys1,
                started_unix: 100,
                complete_at_unix: 130,
                completed: false,
            },
        );
    });
    super::clear_safe_logoff(&ctx.db, c2);
    with_sp6_state(&ctx.db, |st| {
        assert!(st.safe_logoffs.contains_key(&c1));
        assert!(!st.safe_logoffs.contains_key(&c2));
    });

    ctx.db
        .acquire_online_lock_with_computer_hash(1, 77_001, Some(c1), None, true, 100)
        .expect("lock c1");
    ctx.db
        .acquire_online_lock_with_computer_hash(2, 77_002, Some(c2), None, true, 100)
        .expect("lock c2");
    ctx.bound_objects.write().unwrap().insert(
        55_001,
        crate::router::BoundObjectEntry {
            bound_id: 55_001,
            session_id: 77_003,
            user_id: 1,
            character_id: c1,
            service: "ship".into(),
            object_id: ship1,
            oid_str: "55001".into(),
            ref_id: 0,
        },
    );
    ctx.bound_objects.write().unwrap().insert(
        55_002,
        crate::router::BoundObjectEntry {
            bound_id: 55_002,
            session_id: 77_002,
            user_id: 2,
            character_id: c2,
            service: "ship".into(),
            object_id: ship2,
            oid_str: "55002".into(),
            ref_id: 0,
        },
    );
    super::disconnect_safe_logoff_sessions(&ctx, c1);
    super::clear_safe_logoff(&ctx.db, c1);
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.safe_logoffs.contains_key(&c1));
    });
    assert!(ctx.db.get_online_session(1).is_none());
    assert!(ctx.db.get_online_session(2).is_some());
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&55_001));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&55_002));

    // 3. cleanup_npc_wrecks_and_reset_missions_on_downtime:
    //    - non-Accepted mission cleaned out of mission_initial_npc_counts & awaiting
    //    - has_combat_wave == false clears npcs_remaining without awaiting respawn
    //    - initial_count == 0, spawned=1, npcs_remaining=1 -> target_count floor 4
    let m_offered = 3_020_001u32;
    let m_non_combat = 3_020_002u32;
    let m_floor4 = 3_020_003u32;
    let origin = DVec3::new(10_000.0, 0.0, 20_000.0);

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.active_missions.clear();
        st.mission_initial_npc_counts.clear();
        st.missions_awaiting_downtime_respawn.clear();

        snap.active_missions.insert(
            (c1, m_offered),
            ActiveMissionRecord {
                character_id: c1,
                agent_id: m_offered as i64,
                mission_id: 1,
                solar_system_id: sys1,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts.insert(format!("{c1}:1"), 6);
        st.missions_awaiting_downtime_respawn.push((c1, m_offered));

        snap.active_missions.insert(
            (c1, m_non_combat),
            ActiveMissionRecord {
                character_id: c1,
                agent_id: m_non_combat as i64,
                mission_id: 2,
                solar_system_id: sys1,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );

        snap.active_missions.insert(
            (c1, m_floor4),
            ActiveMissionRecord {
                character_id: c1,
                agent_id: m_floor4 as i64,
                mission_id: 3,
                solar_system_id: sys1,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![990_000_001],
                bookmark_id: None,
            },
        );
    });

    let (purged_wrecks, reset_missions) =
        cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(purged_wrecks, 0);
    assert_eq!(reset_missions, 1);

    with_sp6_state(&ctx.db, |st| {
        assert!(!st.mission_initial_npc_counts.contains_key(&format!("{c1}:1")));
        assert!(!st.missions_awaiting_downtime_respawn.contains(&(c1, m_offered)));
        assert!(!st.missions_awaiting_downtime_respawn.contains(&(c1, m_non_combat)));
        assert!(st.missions_awaiting_downtime_respawn.contains(&(c1, m_floor4)));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(c1, m_non_combat)].npcs_remaining, 0);
        assert_eq!(snap.active_missions[&(c1, m_floor4)].npcs_remaining, 4);
    });

    // Pocket entry clears awaiting respawn and respawns 4 NPCs (even without prior initial_count)
    let spawned = spawn_mission_wave_on_pocket_entry_if_needed(&ctx, c1, sys1, origin);
    assert_eq!(spawned, 4);
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.missions_awaiting_downtime_respawn.contains(&(c1, m_floor4)));
    });
}

