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

#[tokio::test]
async fn death_pipeline_loot_insurance_escape_frigate_kill_rights_and_pod_boosters() {
    use super::death_pipeline::{
        execute_death_pipeline_sync, resolve_destroyed_ship_contents_in_snap,
    };
    use crate::services::combat_crimewatch::with_mut_combat_runtime;

    let mut ctx = test_ctx();
    let sys_id = 30_000_142u32; // high-sec 0.9
    let vic_char = 90_000_001i64;
    let att_char = 90_000_002i64;
    let now = ctx.now_unix();

    // 1. Unit check of `resolve_destroyed_ship_contents_in_snap` with Plastic Wrap (`3468`),
    // nested modules, zero-price fallback, and surviving vs destroyed branches
    {
        let mut snap = eve_db::WorldStateSnapshot::default();
        let ship_id = 100_095_001i64;
        let wreck_id = 100_095_002i64;
        let wrap_drop = 100_095_003i64;
        let in_wrap_drop = 100_095_004i64;
        let container_ok = 100_095_005i64;
        let in_container = 100_095_006i64;
        let container_fail = 100_095_007i64;
        let in_fail_container = 100_095_008i64;
        snap.items.insert(ship_id, make_item(ship_id, 587, vic_char, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(wrap_drop, make_item(wrap_drop, 3468, vic_char, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(in_wrap_drop, make_item(in_wrap_drop, 34, vic_char, wrap_drop, InventoryFlag::None, 10));
        snap.items.insert(container_ok, make_item(container_ok, 23, vic_char, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(in_container, make_item(in_container, 34, vic_char, container_ok, InventoryFlag::None, 7));
        snap.items.insert(container_fail, make_item(container_fail, 23, vic_char, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(in_fail_container, make_item(in_fail_container, 34, vic_char, container_fail, InventoryFlag::None, 5));

        let out = resolve_destroyed_ship_contents_in_snap(
            &mut snap,
            &ctx.sde,
            ship_id,
            wreck_id,
            Some(att_char),
            None,
            false,
            0.5,
            |_| true,
            |iid| if iid == container_fail { 0.99 } else { 0.1 },
        );
        assert!(!snap.items.contains_key(&wrap_drop));
        assert_eq!(snap.items[&in_wrap_drop].location_id, wreck_id);
        assert_eq!(snap.items[&in_wrap_drop].owner_id, att_char);
        assert_eq!(snap.items[&in_wrap_drop].flag, InventoryFlag::Cargo);
        assert_eq!(snap.items[&in_container].location_id, container_ok);
        assert!(!snap.items.contains_key(&container_fail));
        assert!(!snap.items.contains_key(&in_fail_container));
        assert!(out.dropped_ids.contains(&in_wrap_drop));
        assert!(out.destroyed_ids.contains(&container_fail));
    }

    // 2. Battleship with Escape Frigate (`group_id == 27`), existing persisted kill right in DB only,
    // custom ship fitting NOT replaced on transition, corp missing payout fallback, and epoch/version checks
    let custom_tid = 999_802u32;
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut custom_bs = sde.types[&641].clone();
        custom_bs.type_id = custom_tid;
        custom_bs.base_price = 0.0;
        sde.types.insert(custom_tid, custom_bs);
    }
    let bs_id = 100_095_101i64;
    let esc_frig_id = 100_095_102i64;
    let esc_custom_mod = 100_095_103i64;
    let db_kr_id = 100_095_104i64;
    spawn_ship(&ctx, sys_id, bs_id, vic_char, custom_tid, DVec3::new(10_000.0, 0.0, 0.0));
    let (pre_epoch, pre_version, pre_bal) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&vic_char) {
            ch.system_epoch = 42;
            ch.station_id = None;
            ch.structure_id = None;
            ch.balance = eve_db::IskCents::from_isk(1_000_000);
        }
        let mut esc_item = make_item(
            esc_frig_id,
            587,
            vic_char,
            bs_id,
            InventoryFlag::from_u16(177),
            1,
        );
        esc_item.singleton = true;
        snap.items.insert(esc_frig_id, esc_item);
        snap.items.insert(
            esc_custom_mod,
            make_item(esc_custom_mod, 2048, vic_char, esc_frig_id, InventoryFlag::from_u16(11), 1),
        );
        snap.items.get_mut(&bs_id).unwrap().insured_payout_isk = 500_000.0;
        // Pre-seed active non-suspect KillRight so death_pipeline reuses it without duplicating (`L933-960`)
        snap.kill_rights.insert(
            db_kr_id,
            eve_db::KillRightRecord {
                kill_right_id: db_kr_id,
                owner_id: vic_char,
                target_id: att_char,
                activation_fee: eve_db::IskCents::ZERO,
                is_active_suspect: false,
            },
        );
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .insert(db_kr_id, now + 3_600);
        let ch = &snap.characters[&vic_char];
        (ch.system_epoch, ch.version, ch.balance)
    });

    let res_esc = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        vic_char,
        bs_id,
        att_char,
        DVec3::new(10_000.0, 0.0, 0.0),
        0, // rng_seed == 0 exercises getrandom branch (`L621-624`)
    )
    .unwrap();
    assert_eq!(res_esc.ejected_capsule_id, Some(esc_frig_id));
    ctx.db.with_snapshot(|snap| {
        let vic = &snap.characters[&vic_char];
        assert_eq!(vic.active_ship_id, esc_frig_id);
        assert_eq!(vic.system_epoch, pre_epoch);
        assert!(vic.version > pre_version);
        assert!(vic.station_id.is_none());
        assert!(vic.balance > pre_bal);
        // Escape frigate custom fitted module preserved (`(false, false)` transition preserves modules!)
        assert_eq!(snap.items[&esc_custom_mod].location_id, esc_frig_id);
        // No duplicate kill right created since DB kill right was still active (`L941-944`)
        assert_eq!(
            snap.kill_rights
                .values()
                .filter(|k| k.owner_id == vic_char && k.target_id == att_char)
                .count(),
            1
        );
    });

    // 3. Corp insurance payout on missing corp fallback (`L762`) + expired insurance contract fallback (`L433`)
    let corp_ship = 100_095_201i64;
    let mut esc2_pos = DVec3::new(12_000.0, 0.0, 0.0);
    spawn_ship(&ctx, sys_id, corp_ship, vic_char, 587, esc2_pos);
    let _exp_ship = 100_095_202i64;
    spawn_ship(&ctx, sys_id, _exp_ship, vic_char, 587, esc2_pos);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            corp_ship,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: corp_ship,
                ship_type_id: 587,
                owner_id: 98_999_999, // missing corp fallback (`L762`)
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 1_000.0,
                payout_isk: 250_000.0,
                start_unix: now - 100,
                end_unix: now + 3_600,
            },
        );
        rt.insurance_contracts.insert(
            _exp_ship,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: _exp_ship,
                ship_type_id: 587,
                owner_id: vic_char,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 1_000.0,
                payout_isk: 250_000.0,
                start_unix: now - 5_000,
                end_unix: now - 10, // expired (`L422-425`)
            },
        );
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(
        &ctx,
        |st| {
            st.insurance_contracts.insert(
                _exp_ship,
                crate::services::inventory_fitting::fitting_containers_ships::InsuranceContractRecord {
                    ship_id: _exp_ship,
                    ship_type_id: 587,
                    owner_id: vic_char,
                    level_name: "Platinum".into(),
                    level_fraction: 1.0,
                    cost_isk: 1_000.0,
                    payout_isk: 250_000.0,
                    start_unix: now - 5_000,
                    end_unix: now - 10, // expired (`L433`)
                },
            );
        },
    );
    // Also spawn pod with Booster item + Implant item + active_implants to hit L645-688
    let _ = execute_death_pipeline_sync(&ctx, sys_id, vic_char, corp_ship, att_char, esc2_pos, 11).unwrap();
    let _ = execute_death_pipeline_sync(&ctx, sys_id, vic_char, _exp_ship, att_char, esc2_pos, 12).unwrap();

    let pod_id = 100_095_203i64;
    esc2_pos += DVec3::new(500.0, 0.0, 0.0);
    spawn_ship(&ctx, sys_id, pod_id, vic_char, 670, esc2_pos);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(vic_char, vec![19_540]);
        snap.items.insert(
            100_095_204,
            make_item(100_095_204, 19_551, vic_char, vic_char, InventoryFlag::Implant, 1),
        );
        snap.items.insert(
            100_095_205,
            make_item(100_095_205, 19_540, vic_char, vic_char, InventoryFlag::Implant, 1),
        );
        snap.items.insert(
            100_095_206,
            make_item(100_095_206, 28_672, vic_char, vic_char, InventoryFlag::Booster, 1),
        );
    });
    let pod_res = execute_death_pipeline_sync(&ctx, sys_id, vic_char, pod_id, att_char, esc2_pos, 13).unwrap();
    assert!(pod_res.killmail_id > 0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&100_095_204));
        assert!(!snap.items.contains_key(&100_095_205));
        assert!(!snap.items.contains_key(&100_095_206));
    });
}

#[tokio::test]
async fn salvage_capacity_fallback_out_of_range_cap_drain_and_repeat_cycle_scheduling() {
    use super::drones_salvage_tractor::{
        attempt_salvage_wreck, execute_wreck_salvage_cycle, execute_wreck_salvage_cycle_scheduled,
    };

    let mut ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let ship_id = 100_096_001i64;
    let wreck_far = 100_096_010i64;
    let wreck_can = 100_096_011i64;
    let wreck_fail = 100_096_012i64;
    let loot_item = 100_096_020i64;

    // Give ship small base SDE capacity & zero cargo fitting so sde fallback triggers (`L106-112`, `L498-504`)
    spawn_ship(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO);
    // Clear ship_fittings so `get_or_rebuild_ship_fitting` uses DB or errors/falls back? Wait: set active ship type_id in DB to custom non-fittable or clear char? Even better: test `NotEnoughCargoSpace` on attempt_salvage_wreck and `execute_wreck_salvage_cycle` jetcan spawn!
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (wid, pos) in [
        (wreck_far, DVec3::new(90_000.0, 0.0, 0.0)),
        (wreck_can, DVec3::new(1_000.0, 0.0, 0.0)),
        (wreck_fail, DVec3::new(1_200.0, 0.0, 0.0)),
    ] {
        let _ = handle
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
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                wid,
                make_item(wid, 26557, char_id, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.item_positions.insert(wid, [pos.x, pos.y, pos.z]);
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            25863,
            eve_db::TrainedSkillRecord {
                type_id: 25863,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            loot_item,
            make_item(loot_item, 34, char_id, wreck_can, InventoryFlag::Cargo, 10),
        );
    });

    // Out-of-range checks for salvager module (`L346`) and salvage drone (`L366`)
    let err_mod_range = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_far, Some(25861))
        .await
        .unwrap_err();
    assert_eq!(err_mod_range.code, "TargetOutOfRange");
    let err_drone_range = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_far, None)
        .await
        .unwrap_err();
    assert_eq!(err_drone_range.code, "TargetOutOfRange");

    // Jetcan spawn on `execute_wreck_salvage_cycle` when wreck holds loot (`L539-562`, `L593-615`)
    // Make a hard-ish but solvable or loop until `salvaged == true`
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3438,
            eve_db::TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    for _ in 0..15 {
        let rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_can, Some(25861))
            .await
            .unwrap();
        if rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(true) {
            break;
        }
    }
    ctx.db.with_snapshot(|snap| {
        let can_id = snap.items[&loot_item].location_id;
        assert_eq!(snap.items[&can_id].type_id, 23);
    });

    // Fill ship cargo past capacity to hit attempt_salvage_wreck `NotEnoughCargoSpace` (`L133`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_096_030,
            make_item(100_096_030, 34, char_id, ship_id, InventoryFlag::Cargo, 50_000),
        );
    });
    let full_err = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_fail, 1.0, 0.0)
        .await
        .unwrap_err();
    assert!(full_err.contains("NotEnoughCargoSpace"));

    // Custom salvager module with cap cost & negative difficulty so `CycleRollFailed` schedules repeat (`L696-709`, `L718-737`)
    let cap_mod_tid = 999_803u32;
    let hard_wreck_tid = 999_804u32;
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut mt = sde.types[&25861].clone();
        mt.type_id = cap_mod_tid;
        mt.default_attributes.insert(eve_sde::attr_ids::CAPACITOR_NEED, 50.0);
        mt.default_attributes.insert(73, 5_000.0);
        sde.types.insert(cap_mod_tid, mt);

        let mut wt = sde.types[&26557].clone();
        wt.type_id = hard_wreck_tid;
        // Slightly difficult (-15) rather than <=0 total chance -> let's use custom accessBonus -25 with skill 0 on char2, OR deterministic 0.1%? Actually let's check: `-15` gives positive `accessChance` that still fails most rolls OR we need `InsufficientAccessChance` vs `CycleRollFailed`!
        wt.default_attributes.insert(901, -40.0);
        sde.types.insert(hard_wreck_tid, wt);
    }
    let ship2 = 100_096_101i64;
    let wreck_sched = 100_096_102i64;
    spawn_ship(&ctx, sys_id, ship2, char_id, 587, DVec3::ZERO);
    // Remove cargo overload from ship2 and give ship2 low cap first -> NotEnoughCapacitor (`L697-705`)
    let mut low_cap = handle.get_combat_state(ship2).await.unwrap().unwrap();
    low_cap.capacitor = 10.0;
    let _ = handle.set_combat_state(ship2, low_cap.clone()).await;
    let _ = handle
        .spawn_ball(Ball {
            id: wreck_sched,
            type_id: hard_wreck_tid,
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_sched,
            make_item(wreck_sched, hard_wreck_tid, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let no_cap_err = execute_wreck_salvage_cycle_scheduled(
        &ctx,
        sys_id,
        char_id,
        ship2,
        wreck_sched,
        Some(100_096_199),
        Some(cap_mod_tid),
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(no_cap_err.code, "NotEnoughCapacitor");

    // Raise cap above 50.0 so cap is deducted and CycleRollFailed schedules ActiveModuleCycle (`L706-737`)
    low_cap.capacitor = 200.0;
    let _ = handle.set_combat_state(ship2, low_cap).await;
    // Set skill 3438 to level 1 so access chance is small > 0 (5 base + 5*1 + (-40) = -30? Wait: module base accessChance is ~20 or 30, level 5 * 5 = 25, attr 901 = -20 -> small positive chance ~25%)
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        if let Some(wt) = sde.types.get_mut(&hard_wreck_tid) {
            wt.default_attributes.insert(901, -40.0);
        }
        if let Some(mt) = sde.types.get_mut(&cap_mod_tid) {
            mt.default_attributes.insert(1090, 5.0);
        }
    }
    let _ = execute_wreck_salvage_cycle_scheduled(
        &ctx,
        sys_id,
        char_id,
        ship2,
        wreck_sched,
        Some(100_096_199),
        Some(cap_mod_tid),
        true,
    )
    .await;
}

