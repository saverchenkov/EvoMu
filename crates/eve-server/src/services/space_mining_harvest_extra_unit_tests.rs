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

#[tokio::test]
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
