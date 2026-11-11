//! Additional edge-case unit tests for `combat::drones_salvage_tractor`.

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn spawn_ship_and_wreck(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    wreck_pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            wreck_id,
            make_item(wreck_id, 26557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions
            .insert(wreck_id, [wreck_pos.x, wreck_pos.y, wreck_pos.z]);
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
    sys.try_spawn_ball_with_combat(
        Ball {
            id: wreck_id,
            type_id: 26557,
            position: wreck_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attempt_salvage_wreck_loot_overflow_cargo_overflow_and_jetcan_spawn() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_900_001;
    let wreck_over = 100_900_010;
    let wreck_loot = 100_900_011;

    spawn_ship_and_wreck(&ctx, sys_id, char_id, ship_id, wreck_over, DVec3::new(1_000.0, 0.0, 0.0));
    spawn_ship_and_wreck(&ctx, sys_id, char_id, ship_id, wreck_loot, DVec3::new(1_000.0, 0.0, 0.0));

    // 1. Loot volume > 27,500 m3 -> SalvageTooMuchLootBody
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_900_020,
            make_item(100_900_020, 1230, char_id, wreck_over, InventoryFlag::Cargo, 500_000),
        );
        // Overfill ship cargo
        snap.items.insert(
            100_900_021,
            make_item(100_900_021, 1230, char_id, ship_id, InventoryFlag::Cargo, 50_000),
        );
    });
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_over, 0.5, 0.0)
            .await
            .unwrap_err(),
        "SalvageTooMuchLootBody"
    );

    // 2. Ship cargo full -> NotEnoughCargoSpace
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_loot, 0.5, 0.0)
            .await
            .unwrap_err(),
        "NotEnoughCargoSpace"
    );

    // 3. Clear ship cargo, put small loot inside wreck_loot -> spawns Cargo Container (type 23) with child item
    let child_loot = 100_900_022;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_900_021);
        snap.items.insert(
            child_loot,
            make_item(child_loot, 34, 90_000_002, wreck_loot, InventoryFlag::Cargo, 10),
        );
    });
    let res = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_loot, 0.5, 0.0)
        .await
        .unwrap();
    assert!(matches!(res, SalvageOutcome::Success { .. }));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wreck_loot));
        let can = snap
            .items
            .values()
            .find(|i| i.type_id == 23 && i.location_id == sys_id as i64)
            .expect("jetcan spawned");
        assert_eq!(snap.items[&child_loot].location_id, can.item_id);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_and_execute_salvage_cycle_scheduled_auto_repeat_and_rigs() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_910_001;
    let wreck_id = 100_910_010;
    let wreck_far = 100_910_011;
    let wreck_zero = 100_910_012;
    let wreck_unspawned = 100_910_013;

    spawn_ship_and_wreck(&ctx, sys_id, char_id, ship_id, wreck_id, DVec3::new(1_500.0, 0.0, 0.0));
    spawn_ship_and_wreck(&ctx, sys_id, char_id, ship_id, wreck_far, DVec3::new(100_000.0, 0.0, 0.0));
    spawn_ship_and_wreck(&ctx, sys_id, char_id, ship_id, wreck_zero, DVec3::new(1_500.0, 0.0, 0.0));

    // Drone mode: missing wreck ball -> TargetNotInSpace; > drone range -> TargetOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_unspawned,
            make_item(wreck_unspawned, 26557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck_unspawned, None)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck_far, None)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // Loot volume > 27,500 m3 in validate_wreck_salvage_target
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_910_020,
            make_item(100_910_020, 1230, char_id, wreck_far, InventoryFlag::Cargo, 500_000),
        );
    });
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck_far, None)
            .await
            .unwrap_err()
            .code,
        "SalvageTooMuchLootBody"
    );

    // Create custom impossible wreck (difficulty -50) -> InsufficientAccessChance
    // and custom salvager module with cap_need = 100.0, cycle = 5000ms
    let hard_wreck_tid = 999_801u32;
    let mod_tid = 999_802u32;
    let rig_tid = 999_803u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut wt = sde_mut.types[&26557].clone();
        wt.type_id = hard_wreck_tid;
        wt.group_id = 186;
        wt.default_attributes.insert(901, -50.0);
        sde_mut.types.insert(hard_wreck_tid, wt);

        let mut mt = sde_mut.types[&25861].clone();
        mt.type_id = mod_tid;
        mt.default_attributes.insert(eve_sde::attr_ids::CAPACITOR_NEED, 100.0);
        mt.default_attributes.insert(73, 5_000.0);
        mt.default_attributes.insert(902, 55.0);
        sde_mut.types.insert(mod_tid, mt);

        let mut rt = sde_mut.types[&25861].clone();
        rt.type_id = rig_tid;
        rt.default_attributes.insert(901, 10.0);
        sde_mut.types.insert(rig_tid, rt);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&wreck_zero).unwrap().type_id = hard_wreck_tid;
        // Zero skill
        snap.skills.entry(char_id).or_default().remove(&3438);
    });
    // Drone mode (3.0) - 50.0 = -47.0 <= 0 -> InsufficientAccessChance
    let zero_rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_zero, None)
        .await
        .unwrap();
    assert_eq!(zero_rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        zero_rep.dict_get("reason").and_then(|v| v.as_str()),
        Some("InsufficientAccessChance")
    );

    // Add Salvage Tackle rig (+10) -> accessChance = 55 + 10 - 50 = 15% (low chance -> CycleRollFailed with seeded rng or high enough negative diff)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_910_030,
            make_item(100_910_030, rig_tid, char_id, ship_id, InventoryFlag::RigSlot0, 1),
        );
        // Set hard_wreck_tid diff to -64.9 so accessChance = 55 + 10 - 64.9 = 0.1% -> guarantees CycleRollFailed
        // Wait: mutate sde instead or check below
    });
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut
            .types
            .get_mut(&hard_wreck_tid)
            .unwrap()
            .default_attributes
            .insert(901, -64.99);
    }

    // Scheduled cycle with auto_repeat on CycleRollFailed schedules ActiveModuleCycle
    let sys = ctx.systems.get_or_spawn(sys_id);
    let fail_rep = execute_wreck_salvage_auto_repeat(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_zero,
        Some(mod_tid),
    )
    .await
    .unwrap();
    assert_eq!(
        fail_rep.dict_get("reason").and_then(|v| v.as_str()),
        Some("CycleRollFailed")
    );
    assert!(sys.has_active_module_cycle_sync(ship_id, wreck_zero));

    // NotEnoughCapacitor error when cap < 100
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().capacitor = 10.0;
    }
    assert_eq!(
        execute_wreck_salvage_cycle_scheduled(
            &ctx,
            sys_id,
            char_id,
            ship_id,
            wreck_zero,
            Some(990_001),
            Some(mod_tid),
            false,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughCapacitor"
    );

    // Put child item inside normal wreck_id and execute_wreck_salvage_cycle -> spawns jetcan
    let child = 100_910_040;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            child,
            make_item(child, 34, char_id, wreck_id, InventoryFlag::Cargo, 5),
        );
    });
    // Ensure 100% success via high salvager bonus
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut
            .types
            .get_mut(&mod_tid)
            .unwrap()
            .default_attributes
            .insert(902, 200.0);
    }
    let ok_rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_id, Some(mod_tid))
        .await
        .unwrap();
    assert_eq!(ok_rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wreck_id));
        let can = snap
            .items
            .values()
            .find(|i| i.type_id == 23 && i.location_id == sys_id as i64)
            .expect("jetcan spawned");
        assert_eq!(snap.items[&child].location_id, can.item_id);
    });
}

#[test]
fn drone_bandwidth_max_five_and_missing_ship_errors() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_920_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    // > 5 drones
    let err_count = validate_drone_bandwidth(&ctx, ship_id, &[2454, 2454, 2454], &[2454, 2454, 2454])
        .unwrap_err();
    assert!(err_count.contains("more than 5 active drones"));

    // Missing ship
    let err_ship = validate_drone_bandwidth(&ctx, 999_444_333, &[2454], &[]).unwrap_err();
    assert!(err_ship.contains("not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn salvager_range_chance_math_slim_meta_and_scheduled_cap_boundaries() {
    use eve_dogma::{AttributeModifier, ModifierOperator};

    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_700_001_i64;
    let wreck_owner = 91_700_002_i64;
    let ship_id = 101_700_001_i64;
    let mod_item_id = 101_700_010_i64;
    let decoy_item_id = 101_700_011_i64;
    let rig_item_id = 101_700_012_i64;
    let salvager_tid = 999_810_u32;
    let decoy_tid = 999_811_u32;
    let rig_902_tid = 999_812_u32;
    let custom_wreck_tid = 999_813_u32;
    let drone_salv_tid = 999_814_u32;

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut st = sde_mut.types[&25861].clone();
        st.type_id = salvager_tid;
        st.group_id = 1122;
        st.default_attributes.remove(&182);
        st.default_attributes.remove(&183);
        st.default_attributes.remove(&184);
        st.default_attributes.insert(54, 7_500.0);
        st.default_attributes.insert(902, 5.0);
        st.default_attributes.insert(eve_sde::attr_ids::CAPACITOR_NEED, 40.0);
        st.default_attributes.insert(73, 2_000.0);
        sde_mut.types.insert(salvager_tid, st);

        let mut dt = sde_mut.types[&25861].clone();
        dt.type_id = decoy_tid;
        dt.group_id = 53; // non-1122 group
        dt.default_attributes.remove(&182);
        dt.default_attributes.remove(&183);
        dt.default_attributes.remove(&184);
        dt.default_attributes.insert(54, 50_000.0);
        sde_mut.types.insert(decoy_tid, dt);

        let mut rt = sde_mut.types[&25861].clone();
        rt.type_id = rig_902_tid;
        rt.default_attributes.remove(&182);
        rt.default_attributes.remove(&183);
        rt.default_attributes.remove(&184);
        rt.default_attributes.remove(&901);
        rt.default_attributes.insert(902, 10.0);
        sde_mut.types.insert(rig_902_tid, rt);

        let mut wt = sde_mut.types[&26557].clone();
        wt.type_id = custom_wreck_tid;
        wt.group_id = 186;
        wt.default_attributes.insert(901, -9.0);
        sde_mut.types.insert(custom_wreck_tid, wt);

        let mut sd = sde_mut.types[&2454].clone();
        sd.type_id = drone_salv_tid;
        sd.default_attributes.insert(902, 4.0);
        sd.default_attributes.insert(eve_sde::attr_ids::CAPACITOR_NEED, 25.0);
        sd.default_attributes.insert(73, 500.0); // clamped >= 1.0s
        sde_mut.types.insert(drone_salv_tid, sd);
    }

    let wreck_mid = 101_700_020_i64; // at 6,500m (inside 7,500 salvager, outside 5,000 clamp)
    let wreck_far = 101_700_021_i64; // at 8,500m (outside 7,500 salvager, inside 50,000 decoy)
    let wreck_exact = 101_700_022_i64; // at 1,500m for chance formula & roll checks
    let loot_in_exact = 101_700_030_i64;
    let loot_in_far = 101_700_031_i64;
    let drone_item_id = 101_700_040_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mod_item_id,
            make_item(mod_item_id, salvager_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            decoy_item_id,
            make_item(decoy_item_id, decoy_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            rig_item_id,
            make_item(rig_item_id, rig_902_tid, char_id, ship_id, InventoryFlag::RigSlot0, 1),
        );
        snap.items.insert(
            drone_item_id,
            make_item(drone_item_id, drone_salv_tid, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );

        for (wid, wtid, wpos) in [
            (wreck_mid, 26557, DVec3::new(6_500.0, 0.0, 0.0)),
            (wreck_far, 26557, DVec3::new(8_500.0, 0.0, 0.0)),
            (wreck_exact, custom_wreck_tid, DVec3::new(1_500.0, 0.0, 0.0)),
        ] {
            snap.items.insert(
                wid,
                make_item(wid, wtid, wreck_owner, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.item_positions.insert(wid, [wpos.x, wpos.y, wpos.z]);
        }

        // Exact 27,500.0 m3 loot in wreck_mid (type 1230 default_vol 0.1 * 275_000 = 27_500.0 if SDE Veldspar is 0.1)
        // Use synthetic 1.0 m3? Better: unit_vol of Tritanium (34) = 0.01 -> 50 units = 0.5 m3.
        snap.items.insert(
            loot_in_exact,
            make_item(loot_in_exact, 34, wreck_owner, wreck_exact, InventoryFlag::Cargo, 50),
        );
        snap.items.insert(
            loot_in_far,
            make_item(loot_in_far, 34, wreck_owner, wreck_mid, InventoryFlag::Cargo, 50),
        );

        // Salvaging skill (25863) lv 2 -> 5.0 * 2.0 = 10.0 (and test legacy 3438 separately)
        snap.skills.entry(char_id).or_default().insert(
            25863,
            eve_db::TrainedSkillRecord {
                type_id: 25863,
                level: 2,
                skill_points: 10_000.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, btid, pos, cap) in [
        (ship_id, 587, DVec3::ZERO, 40.0),
        (wreck_mid, 26557, DVec3::new(6_500.0, 0.0, 0.0), 0.0),
        (wreck_far, 26557, DVec3::new(8_500.0, 0.0, 0.0), 0.0),
        (wreck_exact, custom_wreck_tid, DVec3::new(1_500.0, 0.0, 0.0), 0.0),
    ] {
        let mut cs = ctx.build_ship_combat_state(char_id, bid, btid);
        cs.capacitor = cap;
        cs.max_capacitor = 500.0;
        sys.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: btid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 10_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        );
    }

    // Populate ShipFittingEngine in ctx.ship_fittings with custom evaluated modules & drones
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        let fit = fittings.get_mut(&ship_id).unwrap();
        fit.add_drone(drone_item_id, drone_salv_tid, &ctx.sde);
        if let Some(ds) = fit.evaluated_drones.get_mut(&drone_item_id) {
            ds.base_attributes.insert(902, 4.0);
            ds.base_attributes.insert(eve_sde::attr_ids::CAPACITOR_NEED, 25.0);
            ds.base_attributes.insert(73, 500.0);
        }
    }

    // 1. `validate_wreck_salvage_target` return value & matching-module range vs decoy
    let (v_tid, v_ball_opt) = validate_wreck_salvage_target(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_mid,
        Some(salvager_tid),
    )
    .await
    .expect("6,500m wreck within 7,500m custom salvager range");
    assert_eq!(v_tid, 26557);
    assert_eq!(v_ball_opt.as_ref().map(|b| b.id), Some(wreck_mid));

    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck_far, Some(salvager_tid))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // 2. `attempt_salvage_wreck` uses group 1122 (not decoy group 53), tests exact roll boundary `0.20 == chance`,
    //    and writes full `SlimItemMeta` on spawned jetcan!
    assert!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_far, 0.0, 0.0)
            .await
            .is_err(),
        "decoy non-1122 module with 50km range must NOT unlock 8.5km salvage"
    );
    // roll_u == 0.20 == chance -> Failed (`<` vs `<=`)
    let roll_eq = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_mid, 0.0, 0.20)
        .await
        .unwrap();
    assert!(matches!(roll_eq, SalvageOutcome::Failed { .. }));

    // roll_u == 0.19 < chance -> Success, moves loot to jetcan with verified SlimItemMeta
    let roll_ok = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_mid, 0.0, 0.19)
        .await
        .unwrap();
    assert!(matches!(roll_ok, SalvageOutcome::Success { .. }));
    let can1_id = ctx.db.with_snapshot(|snap| snap.items[&loot_in_far].location_id);
    {
        let bp = sys.ballpark.read().unwrap();
        let meta = bp.ball_slim_meta(can1_id).expect("jetcan slim meta");
        assert_eq!(meta.group_id, 12);
        assert_eq!(meta.category_id, 2);
        assert_eq!(meta.owner_id, wreck_owner);
        assert_eq!(meta.char_id, None);
        assert_eq!(meta.corp_id, -1);
        assert_eq!(meta.alliance_id, -1);
        assert_eq!(meta.security_status, 0.0);
        assert!(!meta.is_cloaked);
        assert_eq!(meta.name, ctx.sde.types.get(&23).map(|t| t.name.clone()));
    }

    // 3. `execute_wreck_salvage_cycle` exact accessChancePct formula & dogma modifier dedupe
    //    mod(5.0) + skill(25863 lv2 -> 10.0) + rig(902 -> 10.0) + wreck(-9.0) = 16.0%
    //    Scheduled cycle with exact cap == 40.0 (`cs.capacitor == cap_cost`) succeeds (`+ 1e-6 <` vs `- 1e-6`),
    //    drains capacitor to 0.0, and schedules duration_s == 2.0 (attr 73 = 2000ms).
    let exact_rep = execute_wreck_salvage_cycle_scheduled(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_exact,
        Some(mod_item_id),
        Some(salvager_tid),
        true,
    )
    .await
    .expect("scheduled salvage with exact cap == cap_cost must succeed");
    let pct1 = exact_rep
        .dict_get("accessChancePct")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((pct1 - 16.0).abs() < 1e-6, "expected 16.0, got {pct1}");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor, 0.0);

    // Check drone scheduled cycle uses EvaluatedDrone cap_need=25.0 & duration_s clamp max(1.0)
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().capacitor = 25.0;
    }
    let drone_rep = execute_wreck_salvage_cycle_scheduled(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_exact,
        None,
        Some(drone_salv_tid),
        false,
    )
    .await
    .unwrap();
    // mod(4.0) + skill(10.0) + rig(10.0) + wreck(-9.0) = 15.0%
    let pct_drone = drone_rep
        .dict_get("accessChancePct")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((pct_drone - 15.0).abs() < 1e-6, "expected 15.0, got {pct_drone}");
    assert_eq!(sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor, 0.0);

    // Add dogma modifiers `source_item_id == 25863` (skill already baked) & `source_item_id != 25863` (rig already baked)
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        let fit = fittings.get_mut(&ship_id).unwrap();
        if let Some(ms) = fit.evaluated_modules.get_mut(&mod_item_id) {
            ms.modifiers.push(AttributeModifier {
                source_item_id: 25863,
                effect_id: None,
                target_attribute_id: 902,
                operator: ModifierOperator::PostPercent,
                value: 10.0,
                stacking_penalized: false,
            });
            ms.modifiers.push(AttributeModifier {
                source_item_id: rig_item_id,
                effect_id: None,
                target_attribute_id: 902,
                operator: ModifierOperator::PostPercent,
                value: 20.0,
                stacking_penalized: false,
            });
        }
    }
    // Check formula with skill+rig skipped -> extra_skill = 0, rig = 0; only mod_access_bonus + (-9.0)
    // Remove skill 25863 & test legacy 3438 separately below; first verify `has_dogma_skill_25863 && has_dogma_rig_902` dedupe!
    let dedupe_rep =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_exact, Some(salvager_tid))
            .await
            .unwrap();
    let pct_dedupe = dedupe_rep
        .dict_get("accessChancePct")
        .and_then(|v| v.as_f64())
        .unwrap();
    // Wait: mod_access_bonus reads `ms.get_attribute(902)` (with PostPercent +10%, +20%) + 0 + 0 - 9.0.
    // Crucially neither `extra_skill_bonus` (10.0) nor `rig_access_bonus` (10.0) is added on top!
    assert!(
        pct_dedupe < 5.0,
        "dogma skill+rig dedupe must NOT double-count 10+10, got {pct_dedupe}"
    );

    // Legacy skill 3438 path (remove 25863 & modifiers, set 3438 = level 3 -> 15.0)
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        let fit = fittings.get_mut(&ship_id).unwrap();
        if let Some(ms) = fit.evaluated_modules.get_mut(&mod_item_id) {
            ms.modifiers.clear();
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let sks = snap.skills.entry(char_id).or_default();
        sks.remove(&25863);
        sks.insert(
            3438,
            eve_db::TrainedSkillRecord {
                type_id: 3438,
                level: 3,
                skill_points: 10_000.0,
            },
        );
    });
    // mod(5.0) + legacy(5.0 * 3 = 15.0) + rig(10.0) + wreck(-9.0) = 21.0%
    let legacy_rep =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_exact, Some(salvager_tid))
            .await
            .unwrap();
    let pct_legacy = legacy_rep
        .dict_get("accessChancePct")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((pct_legacy - 21.0).abs() < 1e-6, "expected 21.0, got {pct_legacy}");

    // Ensure guaranteed win on wreck_exact & assert jetcan SlimItemMeta inside `execute_wreck_salvage_cycle`
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        fittings
            .get_mut(&ship_id)
            .unwrap()
            .evaluated_modules
            .get_mut(&mod_item_id)
            .unwrap()
            .base_attributes
            .insert(902, 200.0);
    }
    let win_rep =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_exact, Some(salvager_tid))
            .await
            .unwrap();
    assert_eq!(win_rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(true));
    let can2_id = ctx.db.with_snapshot(|snap| snap.items[&loot_in_exact].location_id);
    {
        let bp = sys.ballpark.read().unwrap();
        let meta = bp.ball_slim_meta(can2_id).expect("cycle jetcan slim meta");
        assert_eq!(meta.group_id, 12);
        assert_eq!(meta.category_id, 2);
        assert_eq!(meta.owner_id, wreck_owner);
        assert_eq!(meta.char_id, None);
        assert_eq!(meta.corp_id, -1);
        assert_eq!(meta.alliance_id, -1);
        assert_eq!(meta.security_status, 0.0);
        assert!(!meta.is_cloaked);
        assert_eq!(meta.name, ctx.sde.types.get(&23).map(|t| t.name.clone()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_drone_bandwidth_and_launch_drones_pin_dps_vel_fit_and_slim_meta() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_710_001_i64;
    let corp_id = 98_710_001_i64;
    let ally_id = 99_710_001_i64;
    let ship_id = 101_710_001_i64;
    let drone_item_1 = 101_710_010_i64;
    let drone_item_2 = 101_710_011_i64;
    let drone_a_tid = 999_820_u32;
    let drone_b_tid = 999_821_u32;

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut da = sde_mut.types[&2454].clone();
        da.type_id = drone_a_tid;
        da.name = "Alpha Drone".into();
        da.group_id = 100;
        da.category_id = 18;
        da.default_attributes.insert(eve_sde::attr_ids::DRONE_BANDWIDTH_USED, 10.0);
        da.default_attributes.insert(37, 1_500.0);
        da.default_attributes.insert(114, 10.0);
        da.default_attributes.insert(116, 20.0);
        da.default_attributes.insert(117, 30.0);
        da.default_attributes.insert(118, 40.0); // raw = 100
        da.default_attributes.insert(64, 1.5); // mult = 1.5
        da.default_attributes.insert(51, 2_000.0); // rof_s = 2.0 => dps = 75.0
        sde_mut.types.insert(drone_a_tid, da);

        let mut db_t = sde_mut.types[&2454].clone();
        db_t.type_id = drone_b_tid;
        db_t.name = "Beta Drone".into();
        db_t.default_attributes.insert(eve_sde::attr_ids::DRONE_BANDWIDTH_USED, 5.0);
        db_t.default_attributes.insert(37, 0.0); // fallback 800.0
        db_t.default_attributes.insert(114, 12.0);
        db_t.default_attributes.insert(116, 0.0);
        db_t.default_attributes.insert(117, 0.0);
        db_t.default_attributes.insert(118, 0.0);
        db_t.default_attributes.insert(64, 0.5); // clamped max(1.0)
        db_t.default_attributes.insert(51, 500.0); // rof_s clamped max(1.0) => dps = 12.0
        sde_mut.types.insert(drone_b_tid, db_t);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(ally_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            drone_item_1,
            make_item(drone_item_1, drone_a_tid, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            drone_item_2,
            make_item(drone_item_2, drone_b_tid, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 10_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
    );

    // Override ship DRONE_BANDWIDTH to 25.0 in ShipFittingEngine so stale-bw check & exact fit bw work
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        let fit = fittings.get_mut(&ship_id).unwrap();
        let sde_bw = ctx
            .sde
            .types
            .get(&587)
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_BANDWIDTH).copied())
            .unwrap_or(0.0);
        fit.ship
            .base_attributes
            .insert(eve_sde::attr_ids::DRONE_BANDWIDTH, sde_bw);
        fit.ship.modifiers.push(eve_dogma::AttributeModifier {
            source_item_id: 777,
            effect_id: None,
            target_attribute_id: eve_sde::attr_ids::DRONE_BANDWIDTH,
            operator: eve_dogma::ModifierOperator::ModAdd,
            value: 25.0 - sde_bw,
            stacking_penalized: false,
        });
    }

    // 1. Exact bandwidth validation return value (remaining bandwidth) & 5-drone / 25-Mbps boundaries
    let rem_partial = validate_drone_bandwidth(
        &ctx,
        ship_id,
        &[drone_a_tid],
        &[],
    )
    .expect("10 <= 25 must leave 15.0 remaining");
    assert!((rem_partial - 15.0).abs() < 1e-6);

    let rem_exact = validate_drone_bandwidth(
        &ctx,
        ship_id,
        &[drone_a_tid, drone_b_tid, drone_b_tid],
        &[drone_b_tid],
    )
    .expect("10 + 5 + 5 + 5 = 25 <= 25 and 4 <= 5 drones");
    assert!(rem_exact.abs() < 1e-6);

    let rem_5_drones = validate_drone_bandwidth(
        &ctx,
        ship_id,
        &[drone_b_tid, drone_b_tid, drone_b_tid],
        &[drone_b_tid, drone_b_tid],
    )
    .expect("exact 5 drones (25 <= 25) must succeed");
    assert!(rem_5_drones.abs() < 1e-6);

    assert!(
        validate_drone_bandwidth(&ctx, ship_id, &[drone_a_tid, drone_a_tid], &[drone_a_tid])
            .is_err(),
        "30 Mbps > 25 Mbps must error"
    );

    // 2. `DroneController::launch_drones`
    let mut ctrl = DroneController::new();
    let launched = ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[drone_item_1, drone_item_2])
        .await
        .expect("launch_drones succeeds");
    assert_eq!(launched.len(), 2);

    let b1 = launched[0];
    let b2 = launched[1];
    assert!(b1 > 0 && b2 > b1);
    assert!((ctrl.active_drones[&b1].dps - 75.0).abs() < 1e-6);
    assert!((ctrl.active_drones[&b2].dps - 12.0).abs() < 1e-6);

    let (ball1, ball2, meta1) = {
        let bp = sys.ballpark.read().unwrap();
        (
            bp.balls[&b1].clone(),
            bp.balls[&b2].clone(),
            bp.ball_slim_meta(b1).unwrap().clone(),
        )
    };
    assert!((ball1.max_velocity - 1_500.0).abs() < 1e-6);
    assert!((ball2.max_velocity - 800.0).abs() < 1e-6);
    // idx=1 -> offset x = cos(100)*50 != 0, z = sin(100)*50 != 0
    assert!(ball2.position.distance(DVec3::new(1_000.0, 2_000.0, 3_000.0)) > 10.0);

    assert_eq!(meta1.group_id, 100);
    assert_eq!(meta1.category_id, 18);
    assert_eq!(meta1.owner_id, char_id);
    assert_eq!(meta1.char_id, None);
    assert_eq!(meta1.corp_id, corp_id);
    assert_eq!(meta1.alliance_id, ally_id);
    assert_eq!(meta1.security_status, 0.0);
    assert!(!meta1.is_cloaked);
    assert_eq!(meta1.name.as_deref(), Some("Alpha Drone"));
}
