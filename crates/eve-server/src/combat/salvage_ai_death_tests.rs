//! Edge-case unit tests covering salvage, tractor beam, NPC AI/belt spawns, death pipeline, and CrimeWatch.

use super::drones_salvage_tractor::{
    attempt_salvage_wreck, execute_wreck_salvage_cycle, step_tractor_beam,
    validate_wreck_salvage_target, SalvageOutcome,
};
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn make_item(
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
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

pub(super) fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, type_id, owner_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    cs.capacitor = 250.0;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[tokio::test]
async fn salvage_covers_too_much_loot_range_cargo_rig_bonus_and_jetcan_transfer() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_090_001;
    let wreck1 = 100_090_010;
    let wreck2 = 100_090_011;
    let wreck3 = 100_090_012;
    let wreck4 = 100_090_013;

    spawn_ship(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Wreck 1: oversized loot volume (> 27,500 m3)
    // Wreck 2: far away (> 6,000 m / > 60,000 m)
    // Wreck 3: unspawned in space (not spawned in sys_handle)
    // Wreck 4: valid wreck within 2km containing loot item to transfer into jetcan
    for (wid, pos) in [
        (wreck1, DVec3::new(1_000.0, 0.0, 0.0)),
        (wreck2, DVec3::new(80_000.0, 0.0, 0.0)),
        (wreck4, DVec3::new(1_500.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: wid,
                type_id: 26557,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let loot_in_w4 = 100_090_020;
    let rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 773 || (t.effect_ids.contains(&2663) && t.default_attributes.contains_key(&901)))
        .map(|t| t.type_id)
        .unwrap_or(31_790);

    ctx.db.with_mut_snapshot(|snap| {
        for wid in [wreck1, wreck2, wreck3, wreck4] {
            snap.items
                .insert(wid, make_item(wid, 26557, 90_000_002, sys_id as i64, InventoryFlag::None, 1));
        }
        // Place huge loot stack inside wreck1 (10,000 ships * volume >> 27,500 m3)
        snap.items.insert(
            100_090_019,
            make_item(100_090_019, 587, 90_000_002, wreck1, InventoryFlag::None, 10_000),
        );
        // Place modest loot stack inside wreck4
        snap.items.insert(
            loot_in_w4,
            make_item(loot_in_w4, 204, 90_000_002, wreck4, InventoryFlag::None, 2),
        );
        // Overfill ship cargo for attempt_salvage_wreck NotEnoughCargoSpace test
        snap.items.insert(
            100_090_021,
            make_item(100_090_021, 1230, char_id, ship_id, InventoryFlag::Cargo, 50_000),
        );
        // Equip salvage tackle rig & Salvaging skill V
        snap.items.insert(
            100_090_022,
            make_item(100_090_022, rig_tid, char_id, ship_id, InventoryFlag::RigSlot0, 1),
        );
        snap.module_online_states.insert(100_090_022, true);
        snap.skills.entry(char_id).or_default().insert(
            3438,
            TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. Over-capacity wreck rejected by both sync & async validators
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck1, 0.3, 0.0)
            .await
            .unwrap_err(),
        "SalvageTooMuchLootBody"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck1, Some(25_861))
            .await
            .unwrap_err()
            .code,
        "SalvageTooMuchLootBody"
    );

    // 2. Full ship cargo returns NotEnoughCargoSpace on attempt_salvage_wreck
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck4, 0.5, 0.0)
            .await
            .unwrap_err(),
        "NotEnoughCargoSpace"
    );

    // 3. Out of range & unspawned checks for module and salvage drone
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck2, Some(25_861))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck2, None)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, wreck3, None)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );

    // Free ship cargo space and verify attempt_salvage_wreck + execute_wreck_salvage_cycle
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_090_021);
    });
    let outcome = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck4, 0.5, 0.0)
        .await
        .unwrap();
    match outcome {
        SalvageOutcome::Success {
            yielded_salvage_type_ids,
            yielded_quantities,
        } => {
            assert_eq!(yielded_salvage_type_ids.len(), yielded_quantities.len());
            assert!(!yielded_salvage_type_ids.is_empty());
        }
        SalvageOutcome::Failed { reason } => panic!("expected salvage success, got {reason}"),
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wreck4));
        let can_id = snap.items[&loot_in_w4].location_id;
        assert_eq!(snap.items[&can_id].type_id, 23);
    });

    // Negative wreck difficulty hits InsufficientAccessChance branch
    let neg_wreck_tid = 999_801u32;
    let mut custom_ctx = test_ctx();
    let mut neg_t = custom_ctx.sde.types[&26557].clone();
    neg_t.type_id = neg_wreck_tid;
    neg_t.default_attributes.insert(901, -90.0);
    Arc::make_mut(&mut custom_ctx.sde)
        .types
        .insert(neg_wreck_tid, neg_t);
    spawn_ship(&custom_ctx, sys_id, 100_091_001, char_id, 587, DVec3::ZERO);
    let _ = custom_ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: 100_091_002,
            type_id: neg_wreck_tid,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    custom_ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_091_002,
            make_item(100_091_002, neg_wreck_tid, 90_000_002, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let rep = execute_wreck_salvage_cycle(
        &custom_ctx,
        sys_id,
        char_id,
        100_091_001,
        100_091_002,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        rep.dict_get("reason").and_then(|v| v.as_str()),
        Some("InsufficientAccessChance")
    );

    // Tractor beam advances toward ship and clamps at 1,200 m stop distance
    let (pulled, arrived_far) =
        step_tractor_beam(DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO, 2_500.0, 1.0);
    assert!(!arrived_far);
    assert!((pulled.x - 17_500.0).abs() < 1e-6);
    let (clamped, arrived_near) =
        step_tractor_beam(DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO, 2_500.0, 1.0);
    assert!(arrived_near);
    assert!((clamped.x - 1_200.0).abs() < 1e-6);
}
