//! Additional unit and mutation-killing tests for `cluster_tick_downtime`.

use super::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity,
};
use crate::services::mission_fixture::{self, MissionFixture};
use crate::sp6_lifecycle::{
    with_mut_sp6_state, with_sp6_state, PersistedCombatHp, PersistedSpaceObjectMeta,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
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
        mission_fixture::clear_all(snap);
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
    let m1_agent = 3_010_001u32; // Uses explicit initial_wave_count = 7
    let m2_agent = 3_010_002u32; // Uses spawned_npc_ball_ids.len() = 6 (> npcs_remaining=2, > 4)
    let m3_agent = 3_010_003u32; // Uses npcs_remaining = 5 (> spawned_npc_ball_ids.len()=1, > 4)

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.sp6_lifecycle.as_inner_mut();
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
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 601, m1_agent as i64, 30_000_142)
                .current_room_id(2)
                .npcs_remaining(1)
                .spawned_npc_ball_ids(vec![910_100_001])
                .initial_wave_count(Some(7))
                .initial_wave_types(Some(vec![10_019, 10_020]))
                .awaiting_downtime_respawn(true),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 602, m2_agent as i64, 30_000_142)
                .current_room_id(3)
                .npcs_remaining(2)
                .spawned_npc_ball_ids(vec![1, 2, 3, 4, 5, 6]),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 603, m3_agent as i64, 30_000_142)
                .current_room_id(4)
                .npcs_remaining(5)
                .spawned_npc_ball_ids(vec![7]),
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

        let m1 = mission_fixture::get(snap, char_id, 601).unwrap();
        assert_eq!(m1.initial_wave_count, Some(7));
        assert!(m1.initial_wave_types.is_some());
        assert!(m1.awaiting_downtime_respawn);
        assert_eq!(m1.npcs_remaining, 1);
        assert_eq!(mission_fixture::get(snap, char_id, 602).unwrap().npcs_remaining, 2);
        assert_eq!(mission_fixture::get(snap, char_id, 603).unwrap().npcs_remaining, 5);
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
        .filter(|t| {
            t.category_id == 11
                && t.faction_id.is_some()
                && t.default_attributes
                    .get(&416)
                    .is_some_and(|&v| (2_500.0..=15_000.0).contains(&v))
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let fallback_npc_tid = 999_888_777u32;

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_all(snap);
        // Mission needs respawn via (spawned_npc_ball_ids.is_empty() && npcs_remaining > 0) even without awaiting flag!
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, 701, agent_id as i64, sys_id)
                .current_room_id(2)
                .npcs_remaining(2)
                .initial_wave_count(Some(4))
                .initial_wave_types(Some(vec![valid_npc_tid, fallback_npc_tid])),
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
    assert_eq!(spawned, 2);

    // Calling again immediately spawns 0 because spawned_npc_ball_ids is now non-empty and not awaiting.
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, origin),
        0
    );

    let bids = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, 701).unwrap();
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 2);
        m.spawned_npc_ball_ids
    });
    assert_eq!(bids.len(), 2);

    let expected_pirate_faction =
        crate::combat::npc_ai::regional_pirate_faction_id_for_sde(&ctx.sde, sys_id);
    with_combat_runtime(&ctx, |rt| {
        // idx 0 -> valid_npc_tid at angle 0: origin + (4500, 0, 0)
        let e0 = &rt.npc_entities[&bids[0]];
        assert_eq!(e0.type_id, valid_npc_tid);
        assert!((e0.spawn_position.x - (origin.x + 4_500.0)).abs() <= 1.0);
        assert!((e0.spawn_position.y - origin.y).abs() <= 1.0);
        assert!((e0.spawn_position.z - origin.z).abs() <= 1.0);
        assert!(e0.orbit_range >= 2_500.0 && e0.orbit_range <= 15_000.0);
        assert!(e0.bounty_isk >= 10_000.0);

        // idx 1 -> fallback_npc_tid at angle PI: origin + (-4500, 0, 0)
        let e1 = &rt.npc_entities[&bids[1]];
        assert_eq!(e1.type_id, fallback_npc_tid);
        assert_eq!(e1.faction_id, expected_pirate_faction);
        assert_eq!(e1.bounty_isk, 25_000.0);
        assert!(e1.optimal_range >= 5_000.0);
        assert!((e1.orbit_range - (e1.optimal_range * 0.6).clamp(2_500.0, 20_000.0)).abs() < 1e-6);
        assert!(
            (e1.damage_vector.em
                + e1.damage_vector.explosive
                + e1.damage_vector.kinetic
                + e1.damage_vector.thermal)
                > 0.0
        );
        assert!((e1.spawn_position.x - (origin.x - 4_500.0)).abs() <= 1.0);
        assert!((e1.spawn_position.z - origin.z).abs() <= 1.0);
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
    ctx.db.with_mut_snapshot(|snap| {
        let cw = &mut snap.combat_crimewatch_state;
        cw.pending_bounty_ticks.clear();
        cw.pending_bounty_ticks.insert((sys2, c2), 12_500.0);
        cw.pending_bounty_ticks.insert((sys1, c1), 25_000.0);
        cw.pending_bounty_ticks.insert((sys1, c2), 0.0);
        cw.pending_bounty_ticks.insert((sys2, c1), -5.0);
    });
    let flushed = super::flush_pending_bounty_ticks_on_downtime(&ctx);
    assert_eq!(flushed, 2);
    ctx.db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        assert!(!cw.pending_bounty_ticks.contains_key(&(sys1, c1)));
        assert!(!cw.pending_bounty_ticks.contains_key(&(sys2, c2)));
    });

    // 2. clear_safe_logoff and disconnect_safe_logoff_sessions
    let ship1 = 930_000_001i64;
    let ship2 = 930_000_002i64;
    with_mut_sp6_state(&ctx.db, |st| {
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
    //    - non-Accepted mission cleaned out of initial wave count & awaiting respawn
    //    - has_combat_wave == false clears npcs_remaining without awaiting respawn
    //    - initial_count == 0, spawned=1, npcs_remaining=1 -> target_count floor 4
    let m_offered = 3_020_001u32;
    let m_non_combat = 3_020_002u32;
    let m_floor4 = 3_020_003u32;
    let origin = DVec3::new(10_000.0, 0.0, 20_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_all(snap);
        mission_fixture::insert(
            snap,
            MissionFixture::offered(c1, 1, m_offered as i64, sys1)
                .current_room_id(1)
                .initial_wave_count(Some(6))
                .awaiting_downtime_respawn(true),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(c1, 2, m_non_combat as i64, sys1).current_room_id(1),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(c1, 3, m_floor4 as i64, sys1)
                .current_room_id(1)
                .npcs_remaining(1)
                .spawned_npc_ball_ids(vec![990_000_001]),
        );
    });

    let (purged_wrecks, reset_missions) =
        cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(purged_wrecks, 0);
    assert_eq!(reset_missions, 1);

    ctx.db.with_snapshot(|snap| {
        let m1 = mission_fixture::get(snap, c1, 1).unwrap();
        let m2 = mission_fixture::get(snap, c1, 2).unwrap();
        let m3 = mission_fixture::get(snap, c1, 3).unwrap();
        assert!(m1.initial_wave_count.is_none());
        assert!(!m1.awaiting_downtime_respawn);
        assert!(!m2.awaiting_downtime_respawn);
        assert!(m3.awaiting_downtime_respawn);
        assert_eq!(m2.npcs_remaining, 0);
        assert_eq!(m3.npcs_remaining, 1);
    });

    // Pocket entry clears awaiting respawn (even for 0-NPC missions) and respawns surviving 1 NPC (`PVE-019`)
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::edit(snap, c1, 2, |m| {
            m.awaiting_downtime_respawn = true;
        });
    });
    let spawned = spawn_mission_wave_on_pocket_entry_if_needed(&ctx, c1, sys1, origin);
    assert_eq!(spawned, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(!mission_fixture::get(snap, c1, 2)
            .unwrap()
            .awaiting_downtime_respawn);
        assert!(!mission_fixture::get(snap, c1, 3)
            .unwrap()
            .awaiting_downtime_respawn);
    });
}

#[tokio::test]
async fn finalize_safe_logoff_flushes_staged_dispatch_notifications_before_charsel_signal() {
    let ctx = seeded_downtime_ctx();
    let now_unix = 1_800_000_000u64;
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        now_unix,
    )));
    let char_id = 90_000_001i64;
    let sid = 88_138_601u64;
    ctx.db
        .acquire_online_lock(1, sid, Some(char_id), true, now_unix)
        .expect("acquire online lock");
    let (live_tx, mut live_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(sid, live_tx);
    let mut notify_rx = ctx.notify_tx.subscribe();
    let _ = ctx.drain_notifications_for_char(char_id);

    // Run inside `with_staged_dispatch_notifications` (matching `timeControl.AdvanceClock`
    // inside `dispatch_rpc`) and assert that `OnSafeLogoffActivated` is ALREADY visible in
    // `pending_notifications` and `notify_tx` BEFORE the staged dispatch scope exits.
    crate::router::cache_notifications::with_staged_dispatch_notifications(
        &*ctx.pending_notifications,
        &ctx.notify_tx,
        async {
            super::finalize_safe_logoff_sessions_with_hook(&ctx, char_id, |emit_activation| {
                assert!(emit_activation, "expected online character activation hook");
                crate::router::notify_registry::emit_typed_char_with_db(
                    &ctx.db,
                    Some(&*ctx.pending_notifications),
                    Some(&ctx.notify_tx),
                    char_id,
                    crate::router::notify_registry::OnSafeLogoffActivated,
                );
            });

            // Still inside the enclosing staged dispatch scope (before outer flush runs):
            let (code, reason) = live_rx
                .try_recv()
                .expect("SafeLogoffCharsel must be delivered to live closer");
            assert_eq!(code, "SafeLogoffCharsel");
            assert_eq!(reason, char_id.to_string());

            let queued = ctx.drain_notifications_for_char(char_id);
            assert!(
                queued.iter().any(|n| n.event == "OnSafeLogoffActivated"),
                "OnSafeLogoffActivated must be flushed into pending_notifications before SafeLogoffCharsel: {queued:?}"
            );

            let mut saw_tx_activated = false;
            while let Ok((target_cid, notif)) = notify_rx.try_recv() {
                if target_cid == char_id && notif.event == "OnSafeLogoffActivated" {
                    saw_tx_activated = true;
                }
            }
            assert!(
                saw_tx_activated,
                "OnSafeLogoffActivated must be broadcast on notify_tx before SafeLogoffCharsel"
            );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_system_ai_and_combat_bounded_enforces_deadline_when_sync_npc_ai_stalls() {
    use std::time::Duration;

    let ctx = seeded_downtime_ctx();
    let sys_id = 30_000_142_u32;
    let handle = ctx.systems.get_or_spawn(sys_id);

    // Hold `npc_spawns` write lock on a background thread so `step_system_ai_and_combat_bounded`'s
    // synchronous `spawn_blocking` AI/spawns check stalls until released (`step_combat_system`
    // does not touch `npc_spawns`), verifying the bounded timeout returns promptly at `deadline`.
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let npc_spawns = Arc::clone(&ctx.npc_spawns);
    let lock_thread = std::thread::spawn(move || {
        let _guard = npc_spawns.write().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    held_rx.recv().unwrap();

    let deadline = Duration::from_millis(80);
    let started = std::time::Instant::now();
    super::step_system_ai_and_combat_bounded(&ctx, &handle, sys_id, deadline).await;
    let elapsed = started.elapsed();

    let _ = release_tx.send(());
    lock_thread.join().unwrap();

    assert!(
        elapsed >= deadline.saturating_sub(Duration::from_millis(25))
            && elapsed < Duration::from_millis(750),
        "step_system_ai_and_combat_bounded must return promptly past deadline={deadline:?}, got {elapsed:?}"
    );
}


