//! Deep branch-coverage and mutation-killing unit tests for `space_mining_harvest`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_mining_harvest::*;
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{InvType, RandomizedTypeMaterialEntry, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
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

fn ensure_type(sde: &mut Arc<SdeRegistry>, tid: u32, group_id: u32, volume: f64, radius: f64) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base type");
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = 25;
    rec.name = format!("HarvestExtraType_{tid}");
    rec.volume = volume;
    rec.radius = radius;
    rec.portion_size = 100;
    reg.types.insert(tid, rec);
}

#[test]
fn mining_contribution_method_exact_group_boundaries() {
    assert_eq!(mining_contribution_method(464), "MineAsteroid");
    assert_eq!(mining_contribution_method(465), "MineIce");
    assert_eq!(mining_contribution_method(466), "MineAsteroid");

    assert_eq!(mining_contribution_method(710), "MineAsteroid");
    assert_eq!(mining_contribution_method(711), "MineGas");
    assert_eq!(mining_contribution_method(712), "MineAsteroid");

    assert_eq!(mining_contribution_method(1883), "MineAsteroid");
    assert_eq!(mining_contribution_method(1884), "MineMoon");
    assert_eq!(mining_contribution_method(1885), "MineAsteroid");

    assert_eq!(mining_contribution_method(1919), "MineAsteroid");
    for g in 1920..=1923 {
        assert_eq!(mining_contribution_method(g), "MineMoon");
    }
    assert_eq!(mining_contribution_method(1924), "MineAsteroid");
}

#[test]
fn deposit_mined_ore_structure_belt_filtering_and_wormhole_class_tables() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 1_095_000_001_i64;
    let ore_tid = 996_501_u32;
    let struct_belt_id = 1_000_000_999_001_i64;

    ensure_type(&mut ctx.sde, ore_tid, 1921, 0.005, 50.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Moon Refinery without quantum core / fuel is offline and must not record observer ledger,
        // while a non-moon belt asteroid or call without moon asteroid identity also must not record.
        snap.structures.insert(
            struct_belt_id,
            UpwellStructureRecord {
                structure_id: struct_belt_id,
                type_id: 35835,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Athanor".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_095_000_060,
            InventoryItemRecord {
                item_id: 1_095_000_060,
                type_id: 45009,
                owner_id: 1_000_009,
                location_id: struct_belt_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(1_095_000_060, true);
        snap.asteroids.insert(
            1_095_000_050,
            AsteroidRecord {
                asteroid_id: 1_095_000_050,
                type_id: ore_tid,
                solar_system_id: sys_id,
                belt_id: struct_belt_id,
                quantity: 1_000,
                initial_quantity: 1_000,
                position: (0.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });

    // Generic deposit without asteroid context must NOT mirror to system structures
    deposit_mined_ore_and_record_ledger(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        25,
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.observer_mining_ledger.contains_key(&struct_belt_id));
    });

    // Moon asteroid deposit scoped to eligible online Refinery DOES record in its observer ledger
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        25,
        0,
        Some(1_095_000_050),
        Some([0.0, 0.0, 0.0]),
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.observer_mining_ledger[&struct_belt_id].len(), 1);
    });

    // Dispatch with unknown ore_type_id falls back to default volumes without panicking
    dispatch_mining_gameplay_events(&ctx, char_id, 10, sys_id, 999_888_777);

    // Exhaustive wormhole class mapping assertions
    for c in 1..=4 {
        let sites = wormhole_ore_sites_for_class(c);
        assert_eq!(sites.len(), 4);
        assert!(sites.iter().all(|s| s.wh_class_min == 1 && s.wh_class_max == 6));
    }
    for c in 5..=6 {
        let sites = wormhole_ore_sites_for_class(c);
        assert_eq!(sites.len(), 10);
        assert!(sites.iter().any(|s| s.dungeon_name == "Exceptional Core Deposit"));
        assert!(!sites.iter().any(|s| s.dungeon_name == "Shattered Debris Field"));
    }
    for c in [12, 14, 15, 16, 17, 18] {
        let sites = wormhole_ore_sites_for_class(c);
        assert_eq!(sites.len(), 5);
        assert!(sites.iter().any(|s| s.dungeon_name == "Shattered Debris Field"));
        assert!(!sites.iter().any(|s| s.wh_class_min == 5));
    }
    let c13 = wormhole_ore_sites_for_class(13);
    assert_eq!(c13.len(), 5);
    assert!(c13.iter().any(|s| s.dungeon_name == "Shattered Debris Field"));
    for c in [0, 7, 11, 19, 255] {
        assert_eq!(wormhole_ore_sites_for_class(c).len(), 4);
    }
}

#[test]
fn reprocessable_materials_single_entry_swapped_bounds_and_zero_sum_branches() {
    let mut ctx = test_ctx();
    let single_rand_tid = 997_501_u32;
    let zero_rand_tid = 997_502_u32;
    let fixed_tid = 997_503_u32;
    ensure_type(&mut ctx.sde, single_rand_tid, 462, 10.0, 50.0);
    ensure_type(&mut ctx.sde, zero_rand_tid, 462, 10.0, 50.0);
    ensure_type(&mut ctx.sde, fixed_tid, 462, 10.0, 50.0);

    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.type_materials.insert(
            fixed_tid,
            vec![TypeMaterialEntry {
                material_type_id: 34,
                quantity: 100,
            }],
        );
        // Single randomized entry with quantity_min > quantity_max (tests min/max swap)
        reg.randomized_type_materials.insert(
            single_rand_tid,
            vec![RandomizedTypeMaterialEntry {
                material_type_id: 36,
                quantity_min: 300,
                quantity_max: 100,
            }],
        );
        // Randomized entry with 0..=0 quantity -> skipped by `if sum == 0 { continue; }`
        reg.randomized_type_materials.insert(
            zero_rand_tid,
            vec![RandomizedTypeMaterialEntry {
                material_type_id: 37,
                quantity_min: 0,
                quantity_max: 0,
            }],
        );
    }

    let fixed_inv = ctx.sde.types.get(&fixed_tid).unwrap();
    let single_inv = ctx.sde.types.get(&single_rand_tid).unwrap();
    let zero_inv = ctx.sde.types.get(&zero_rand_tid).unwrap();

    // Negative portions on fixed materials clamps to 0
    let neg_fixed =
        resolve_reprocessable_materials_for_portions(&ctx.sde, fixed_inv, 1, 1, 1, -5, true)
            .unwrap();
    assert_eq!(neg_fixed[0].quantity, 0);

    // Deterministic quote on single entry with min=300, max=100 -> midpoint 200 per batch
    let det_single =
        resolve_reprocessable_materials_for_portions(&ctx.sde, single_inv, 10, 20, 30, 3, true)
            .unwrap();
    assert_eq!(det_single.len(), 1);
    assert_eq!(det_single[0].material_type_id, 36);
    assert_eq!(det_single[0].quantity, 600);

    // Non-deterministic roll on single entry with min=300, max=100 -> between 100 and 300 per batch
    let roll_single =
        resolve_reprocessable_materials_for_portions(&ctx.sde, single_inv, 10, 20, 30, 5, false)
            .unwrap();
    assert_eq!(roll_single.len(), 1);
    assert!((500..=1_500).contains(&roll_single[0].quantity));

    // Zero-sum randomized entry produces empty output vector
    let zero_out =
        resolve_reprocessable_materials_for_portions(&ctx.sde, zero_inv, 1, 1, 1, 4, false)
            .unwrap();
    assert!(zero_out.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hydrate_wormhole_guards_adopts_nearby_balls_and_spawns_unpersisted_anomalies() {
    let mut ctx = test_ctx();
    let sys_id = 31_000_009_u32;
    for &tid in WORMHOLE_GUARD_TYPE_IDS {
        ensure_type(&mut ctx.sde, tid, 551, 1_000_000.0, 40.0);
    }

    // Empty guard_waves is a no-op
    spawn_wormhole_ore_guards(&ctx, sys_id, Vec::new()).await;

    let def = WORMHOLE_ORE_SITES[1]; // Common Perimeter Deposit (5 guards of 30209)
    ctx.db.with_mut_snapshot(|snap| {
        let anoms = snap.cosmic_anomalies.entry(sys_id).or_default();
        // Anomaly 1 has a pre-existing guard ball within 15km -> should adopt nearby ball without spawning 5 new ones
        anoms.push(CosmicAnomalyRecord {
            anomaly_id: "WH-NEARBY-1".into(),
            solar_system_id: sys_id,
            name: def.dungeon_name.into(),
            anomaly_type: "OreAnomaly".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![46678],
            remaining_volume: 1000.0,
            created_unix: 100,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        // Anomaly 2 has no state and no nearby balls -> should build and spawn 5 new guard balls
        anoms.push(CosmicAnomalyRecord {
            anomaly_id: "WH-FRESH-2".into(),
            solar_system_id: sys_id,
            name: def.dungeon_name.into(),
            anomaly_type: "OreAnomaly".into(),
            position: [100_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![46678],
            remaining_volume: 1000.0,
            created_unix: 100,
            lifetime_seconds: 86_400,
            cleared: false,
        });
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    let existing_guard_id = 77_000_001_i64;
    let _ = handle
        .spawn_ball(Ball {
            id: existing_guard_id,
            type_id: WORMHOLE_GUARD_TYPE_IDS[0],
            position: DVec3::new(5_000.0, 0.0, 0.0), // within 15_000m of WH-NEARBY-1 at [0,0,0]
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let baseline_balls = handle.list_balls().await.unwrap().len();
    hydrate_wormhole_ore_guards_if_needed(&ctx, sys_id).await;
    let balls = handle.list_balls().await.unwrap();
    // WH-NEARBY-1 adopts existing_guard_id (+0 new balls) and WH-FRESH-2 spawns 5 new guards (+5 new balls)
    assert_eq!(balls.len(), baseline_balls + 5);

    // Calling hydrate a second time is a no-op because both anomalies are already claimed
    hydrate_wormhole_ore_guards_if_needed(&ctx, sys_id).await;
    assert_eq!(handle.list_balls().await.unwrap().len(), baseline_balls + 5);
}

#[test]
fn phased_asteroid_field_exact_boundary_thresholds() {
    let mut f = PhasedAsteroidFieldState::new("SIG-1".into(), 30_000_142);
    assert!((f.progress - 0.1).abs() < 1e-9);
    assert!(!f.beacon_spawned);
    assert!(!f.collapsed);
    assert!(!f.phased_in);
    assert!((f.mining_efficiency() - 0.10).abs() < 1e-9);

    // Advance to 0.4999 -> beacon still false
    f.advance_fracture(0.3999);
    assert!(!f.beacon_spawned);
    // Advance to exact 0.5 -> beacon_spawned becomes true
    f.advance_fracture(0.0001);
    assert!(f.beacon_spawned);
    assert!(!f.collapsed);

    // Connect ships: 99 energy -> not phased_in; +1 energy (= 100 exact) -> phased_in = true
    assert!(!f.connect_industrial_command_ship(101, 99));
    assert_eq!(f.mobile_phase_anchor_energy, 99);
    assert!(f.connect_industrial_command_ship(102, 1));
    assert_eq!(f.mobile_phase_anchor_energy, 100);
    assert!(f.phased_in);
    assert!((f.mining_efficiency() - 1.0).abs() < 1e-9);

    // Advance to 0.9999 -> still not collapsed
    f.advance_fracture(0.4999);
    assert!(!f.collapsed);
    // Advance to 1.0 -> collapsed = true, phased_in = false
    f.advance_fracture(0.0001);
    assert!(f.collapsed);
    assert!(!f.phased_in);
    assert!((f.mining_efficiency() - 0.0).abs() < 1e-9);

    // Connecting another ship after collapse does NOT re-enable phased_in
    assert!(!f.connect_industrial_command_ship(103, 200));
    assert!(!f.phased_in);
}

#[test]
fn deposit_mined_ore_zero_first_and_stack_notifications_and_sample_shattered_system() {
    let mut ctx = test_ctx();
    let char_id = 90_000_111_i64;
    let ship_id = 1_095_100_001_i64;
    let sys_id = 30_000_142_u32;
    let ore_tid = 996_511_u32;
    ensure_type(&mut ctx.sde, ore_tid, 1921, 0.1, 50.0);
    ctx.db
        .provision_gm_account(&format!("miner_{char_id}"), "pw", char_id, "Miner");
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("miner char");
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        let uid = ch.user_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        uid
    });
    let _ = ctx
        .db
        .acquire_online_lock(uid, 9911, Some(char_id), false, ctx.now_unix());

    // 1. mined_units == 0 must insert nothing, notify nothing, and record no ledger entry
    let _ = ctx.drain_notifications_for_char(char_id);
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        0,
        0,
        None,
        None,
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.mining_ledger.iter().all(|e| e.character_id != char_id));
        assert!(!snap
            .items
            .values()
            .any(|i| i.location_id == ship_id && i.type_id == ore_tid));
    });

    // 2. First deposit (prev_qty == 0) emits OnItemChange with ixFlag=3 -> 0
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        10,
        0,
        None,
        None,
    );
    let first_notifs = ctx.drain_notifications_for_char(char_id);
    let first_change = first_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("first deposit emits OnItemChange");
    assert_eq!(
        first_change.args.get(1),
        Some(&eve_proto::PyRep::Dict(vec![(
            eve_proto::PyRep::Integer(3),
            eve_proto::PyRep::Integer(0),
        )]))
    );

    // 3. Stacked deposit (prev_qty == 10 > 0) emits OnItemChange with ixQuantity=9 -> old qty 10
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        5,
        0,
        None,
        None,
    );
    let second_notifs = ctx.drain_notifications_for_char(char_id);
    let second_change = second_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("second deposit emits OnItemChange");
    assert_eq!(
        second_change.args.get(1),
        Some(&eve_proto::PyRep::Dict(vec![(
            eve_proto::PyRep::Integer(9),
            eve_proto::PyRep::Integer(10),
        )]))
    );

    // 4. sample_wormhole_ore_sites_for_system: shattered celestial (type_id 30889) vs normal C3
    let normal_sys = 31_000_701_u32;
    let shattered_sys = 31_000_702_u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut by_sys = (*sde.celestials_by_system).clone();
        by_sys.insert(
            shattered_sys,
            vec![eve_sde::CelestialInfo {
                celestial_id: 40_999_001,
                solar_system_id: shattered_sys,
                type_id: 30889,
                group_id: 7,
                name: "Shattered Planet".into(),
                radius: 1_000_000.0,
                position: DVec3::ZERO,
                orbit_id: None,
                orbit_index: None,
                celestial_index: None,
            }],
        );
        sde.celestials_by_system = Arc::new(by_sys);
    }
    let normal_sites = sample_wormhole_ore_sites_for_system(&ctx.sde, &ctx.mechanic_rng, normal_sys, 3);
    assert!(!normal_sites.is_empty());
    assert!(!normal_sites
        .iter()
        .any(|s| s.dungeon_name == "Shattered Debris Field"));

    let shattered_sites = sample_wormhole_ore_sites_for_system(&ctx.sde, &ctx.mechanic_rng, shattered_sys, 3);
    assert!(!shattered_sites.is_empty());
    assert_eq!(shattered_sites[0].dungeon_name, "Shattered Debris Field");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_all_ice_belt_respawns_matrix_and_wormhole_ore_guard_slim_meta() {
    let mut ctx = test_ctx();
    let now = ctx.now_unix();
    let ready_sys = 30_000_180_u32;
    let ore_only_sys = 30_000_156_u32;
    let partial_sys = 30_002_510_u32;
    let recent_sys = 30_002_659_u32;
    let cleared_ore_sys = 30_003_495_u32;

    for sys in [ready_sys, ore_only_sys, partial_sys, recent_sys, cleared_ore_sys] {
        if let Some(Ok(site)) = crate::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, sys) {
            for a in &site.asteroids {
                ensure_type(&mut ctx.sde, a.type_id, 465, 1_000.0, 120.0);
            }
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Only non-IceBelt cleared & old -> must NOT trigger IceBelt respawn
        snap.cosmic_anomalies.insert(
            ore_only_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ORE-OLD".into(),
                solar_system_id: ore_only_sys,
                name: "Ore Site".into(),
                anomaly_type: "OreAnomaly".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1,
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
        // 2. One cleared old IceBelt PLUS one uncleared IceBelt (no non-Ice anomalies) -> must NOT respawn
        snap.cosmic_anomalies.insert(
            partial_sys,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "ICE-CLEARED".into(),
                    solar_system_id: partial_sys,
                    name: "Ice Belt".into(),
                    anomaly_type: "IceBelt".into(),
                    position: [0.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![16264],
                    remaining_volume: 0.0,
                    created_unix: 1,
                    lifetime_seconds: 14_400,
                    cleared: true,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ICE-ACTIVE".into(),
                    solar_system_id: partial_sys,
                    name: "Ice Belt Active".into(),
                    anomaly_type: "IceBelt".into(),
                    position: [200_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![16264],
                    remaining_volume: 10_000.0,
                    created_unix: now,
                    lifetime_seconds: 14_400,
                    cleared: false,
                },
            ],
        );
        // 3. One cleared IceBelt, NOT yet expired -> must NOT respawn
        snap.cosmic_anomalies.insert(
            recent_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ICE-RECENT".into(),
                solar_system_id: recent_sys,
                name: "Ice Belt".into(),
                anomaly_type: "IceBelt".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![16264],
                remaining_volume: 0.0,
                created_unix: now.saturating_sub(10),
                lifetime_seconds: 14_400,
                cleared: true,
            }],
        );
        // 4. Cleared old non-IceBelt anomaly -> tests `a.anomaly_type == "IceBelt" && a.cleared` (L475)
        snap.cosmic_anomalies.insert(
            cleared_ore_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ORE-CLEARED".into(),
                solar_system_id: cleared_ore_sys,
                name: "Cleared Ore".into(),
                anomaly_type: "OreAnomaly".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1,
                lifetime_seconds: 86_400,
                cleared: true,
            }],
        );
        // 5. Cleared & expired IceBelt -> MUST respawn
        snap.cosmic_anomalies.insert(
            ready_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ICE-READY".into(),
                solar_system_id: ready_sys,
                name: "Ice Belt".into(),
                anomaly_type: "IceBelt".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![16264],
                remaining_volume: 0.0,
                created_unix: now.saturating_sub(crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS + 60),
                lifetime_seconds: 14_400,
                cleared: true,
            }],
        );
    });

    tick_all_ice_belt_respawns_sync(&ctx);

    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&ore_only_sys]
            .iter()
            .all(|a| a.anomaly_type != "IceBelt"));
        assert!(snap.cosmic_anomalies[&cleared_ore_sys]
            .iter()
            .all(|a| a.anomaly_type != "IceBelt"));
        assert!(snap.cosmic_anomalies[&partial_sys]
            .iter()
            .any(|a| a.anomaly_id == "ICE-CLEARED"));
        assert!(snap.cosmic_anomalies[&recent_sys]
            .iter()
            .any(|a| a.anomaly_id == "ICE-RECENT"));
        let ready_anoms = &snap.cosmic_anomalies[&ready_sys];
        assert!(
            ready_anoms
                .iter()
                .any(|a| a.anomaly_type == "IceBelt" && !a.cleared && a.anomaly_id != "ICE-READY"),
            "expired cleared ice belt must respawn"
        );
    });

    // spawn_wormhole_ore_guards: SlimItemMeta coverage + faction_id == 0 fallback -> 500_021
    let g_sys = 31_000_703_u32;
    let f0_tid = 998_601_u32;
    let f510_tid = 998_602_u32;
    ensure_type(&mut ctx.sde, f0_tid, 551, 1_000_000.0, 40.0);
    ensure_type(&mut ctx.sde, f510_tid, 552, 1_000_000.0, 40.0);
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let t0 = reg.types.get_mut(&f0_tid).unwrap();
        t0.name = "Zero Faction Guard".into();
        t0.category_id = 11;
        t0.faction_id = Some(0);
        let t510 = reg.types.get_mut(&f510_tid).unwrap();
        t510.name = "Guristas Ore Guard".into();
        t510.category_id = 11;
        t510.faction_id = Some(500_010);
    }
    let g0_id = 77_900_001_i64;
    let g510_id = 77_900_002_i64;
    spawn_wormhole_ore_guards(
        &ctx,
        g_sys,
        vec![
            (g0_id, f0_tid, DVec3::new(1_000.0, 0.0, 0.0)),
            (g510_id, f510_tid, DVec3::new(2_000.0, 0.0, 0.0)),
        ],
    )
    .await;

    let g_handle = ctx.systems.get_or_spawn(g_sys);
    let bp = g_handle.ballpark.read().unwrap();
    let m0 = bp.ball_slim_meta.get(&g0_id).expect("f0 guard meta");
    assert_eq!(m0.group_id, 551);
    assert_eq!(m0.category_id, 11);
    assert_eq!(m0.owner_id, 500_021);
    assert_eq!(m0.char_id, None);
    assert_eq!(m0.corp_id, 1_000_431);
    assert_eq!(m0.alliance_id, -1);
    assert!((m0.security_status - (-1.0)).abs() < 1e-6);
    assert!(!m0.is_cloaked);
    assert_eq!(m0.name.as_deref(), Some("Zero Faction Guard"));

    let m510 = bp.ball_slim_meta.get(&g510_id).expect("f510 guard meta");
    assert_eq!(m510.group_id, 552);
    assert_eq!(m510.owner_id, 500_010);
    assert_eq!(m510.corp_id, 1_000_127);
    assert_eq!(m510.name.as_deref(), Some("Guristas Ore Guard"));
}
