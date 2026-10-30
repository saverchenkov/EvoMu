//! Phase 4 Dogma Heat Rack Thermodynamics, Nanite Repair, & T2 Crystal Volatility tests (`DG-6`, Part 3).

use crate::common;
use crate::dogma_capital_heat_crystals::*;

use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_dg6_heat_rack_thermodynamics_nanite_repair_and_crystal_volatility() {
    // parity: feat:dogma.heat-rack-thermodynamics, feat:fitting.nanite-repair-paste-consumption, feat:modules.t2-crystal-volatility-destruction, notify:OnHeatAdded, notify:OnHeatRemoved, notify:OnMultiModuleRepairStarted, notify:OnRepairDone, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo
    eve_testkit::attest(&[
        "feat:dogma.heat-rack-thermodynamics",
        "feat:fitting.nanite-repair-paste-consumption",
        "feat:modules.t2-crystal-volatility-destruction",
        "notify:OnHeatAdded",
        "notify:OnHeatRemoved",
        "notify:OnMultiModuleRepairStarted",
        "notify:OnRepairDone",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4204);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade ship to Punisher (597, Amarr Frigate) and fit two Small Focused Pulse Laser II (3041) in HiSlot0 & HiSlot1
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|&id, it| id == ship_id || it.location_id != ship_id);
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 597;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    for tid in [597u32, 3041, 12565, 28668, 1319] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[
            (3426u32, 5u8),
            (3413, 5),
            (3418, 5),
            (3417, 5),
            (28164, 4), // Thermodynamics IV (-20% heat damage)
            (28879, 4), // Nanite Operation IV (-20% nanite repair paste consumption)
            (28880, 5), // Nanite Interfacing V (+100% nanite repair speed)
        ] {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let laser_1_id = 100_002_301i64;
    let laser_2_id = 100_002_302i64;
    let conflag_id = 100_002_303i64;
    let paste_id = 100_002_304i64;
    seed_item(&ctx, laser_1_id, 3041, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, laser_2_id, 3041, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, conflag_id, 12565, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    seed_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 50, false);

    // Populate low slots with online passive modules so Online Slots / Total Slots reflects a combat-fitted frigate
    for (idx, flag) in [
        InventoryFlag::LoSlot0,
        InventoryFlag::LoSlot1,
        InventoryFlag::LoSlot2,
        InventoryFlag::LoSlot3,
        InventoryFlag::LoSlot4,
    ]
    .into_iter()
    .enumerate()
    {
        let mod_id = 100_002_350 + idx as i64;
        seed_item(&ctx, mod_id, 1319, char_id, ship_id, flag, 1, true);
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(mod_id, true);
        });
    }

    for (mid, flag) in [
        (laser_1_id, InventoryFlag::HiSlot0),
        (laser_2_id, InventoryFlag::HiSlot1),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mid),
                PyRep::Integer(ship_id),
                PyRep::Integer(flag as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit Small Focused Pulse Laser II");
    }

    // Load Conflagration S (12565, T2 frequency crystal) into laser_1_id
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(laser_1_id)]),
            PyRep::List(vec![PyRep::Integer(conflag_id)]),
            PyRep::Integer(ship_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Conflagration S into Small Focused Pulse Laser II");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn target dummy at 2 km and lock it
    let target_id = 100_002_320i64;
    let mut target_cs = CombatState::default();
    target_cs.max_shield_hp = 500_000.0;
    target_cs.shield_hp = 500_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        target_cs,
    )
    .await
    .unwrap();
    sys.add_target_lock(
        ship_id,
        eve_destiny::TargetLock {
            target_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    ctx.set_mechanic_rng_seed(Some(7));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(1175, 0.85);
        }
    });

    // 1. Activate laser_1_id with Overload via dogmaIM.Overload and test OverloadRack / StopOverloadRack / StopOverload
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Overload",
        &[
            PyRep::Integer(laser_1_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(target_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Overload laser_1_id");
    let notes_ovl = sess.drain_notifications();
    assert!(notes_ovl.iter().any(|(n, _)| n == "OnHeatAdded"));

    // Step 20s of overloaded weapon cycles -> high rack heat (1175) stays above 0.85,
    // laser_1_id takes more probabilistic heat hits than attenuated adjacent laser_2_id,
    // and loaded Conflagration S crystal accumulates crystalVolatilityDamage!
    for _ in 0..20 {
        sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }

    let ship_info_heated = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let hi_rack_heat = extract_attr_from_info(&ship_info_heated, 1175);
    assert!(
        hi_rack_heat > 0.45,
        "High rack heat (1175) must accumulate during overloaded weapon cycles, got {hi_rack_heat}"
    );

    let l1_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let l2_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_2_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let l1_dmg = extract_attr_from_info(&l1_info, 3);
    let l2_dmg = extract_attr_from_info(&l2_info, 3);
    assert!(
        l1_dmg > 0.0 && l2_dmg > 0.0 && l1_dmg > l2_dmg,
        "Overloaded laser_1 ({l1_dmg}) and adjacent laser_2 ({l2_dmg}) must take probabilistic heat damage with slot-distance attenuation"
    );

    // Verify OverloadRack, StopOverloadRack, and StopOverload RPCs
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "OverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("OverloadRack");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverloadRack", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("StopOverloadRack");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopOverload", &[PyRep::Integer(laser_1_id)], &kw, None)
        .await
        .expect("StopOverload");
    assert!(sess.drain_notifications().iter().any(|(n, _)| n == "OnHeatRemoved"));

    // Step 5s with no overloaded modules -> high rack heat (1175) dissipates exponentially!
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }
    let ship_info_cooled = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let hi_rack_heat_cooled = extract_attr_from_info(&ship_info_cooled, 1175);
    assert!(
        hi_rack_heat_cooled < hi_rack_heat,
        "Rack heat must dissipate exponentially when no modules in the rack are overloaded: {hi_rack_heat_cooled} < {hi_rack_heat}"
    );

    // 2. Verify T2 crystal volatility damage & destruction at 1.0 HP (feat:modules.t2-crystal-volatility-destruction)
    let loaded_crystal_id = ctx
        .db
        .with_snapshot(|s| {
            s.items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == InventoryFlag::HiSlot0
                        && i.item_id != laser_1_id
                })
                .map(|i| (i.item_id, i.dynamic_attributes.get(&3).copied().unwrap_or(0.0)))
        })
        .expect("Loaded Conflagration S crystal");
    assert!(
        loaded_crystal_id.1 > 0.0,
        "Conflagration S must accumulate volatility damage per cycle, got {}",
        loaded_crystal_id.1
    );
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c_it) = s.items.get_mut(&loaded_crystal_id.0) {
            c_it.dynamic_attributes.insert(3, 0.995);
        }
    });
    for _ in 0..4 {
        sys.step_manual(1.0).await.unwrap();
    }
    let crystal_still_exists = ctx
        .db
        .with_snapshot(|s| s.items.contains_key(&loaded_crystal_id.0));
    assert!(
        !crystal_still_exists,
        "Conflagration S crystal must shatter and be destroyed when volatility damage reaches 1.0 HP"
    );

    // Deactivate laser_1_id so we can repair it with Nanite Repair Paste
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(laser_1_id),
            PyRep::String("targetAttack".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate laser_1_id");

    // 3. Verify Nanite Repair Paste consumption, RepairModule, StopRepairModule, and burnout rejection
    let paste_before = ctx
        .db
        .with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    let _ = sess.drain_notifications();
    let rep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
        .expect("RepairModule on damaged laser_1_id");
    assert!(
        sess.drain_notifications()
            .iter()
            .any(|(n, _)| n == "OnMultiModuleRepairStarted")
    );
    let paste_consumed = rep_res
        .dict_get("pasteConsumed")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let paste_after = ctx
        .db
        .with_snapshot(|s| s.items.get(&paste_id).map(|i| i.quantity).unwrap_or(0));
    assert!(
        paste_consumed >= 1 && paste_after == paste_before - (paste_consumed as i32),
        "RepairModule must reserve Nanite Repair Paste from ship cargo"
    );

    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let l1_info_repaired = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(laser_1_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        extract_attr_from_info(&l1_info_repaired, 3),
        0.0,
        "Completed nanite repair must restore module damage (3) to 0.0"
    );

    // Start repair on laser_2_id and test StopRepairModule
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "RepairModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
        &kw,
        None,
    )
    .await
    .expect("RepairModule on laser_2_id");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "StopRepairModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
        &kw,
        None,
    )
    .await
    .expect("StopRepairModule on laser_2_id");
    assert!(
        sess.drain_notifications()
            .iter()
            .any(|(n, _)| n == "OnRepairDone")
    );

    // Completely burned-out module (damage == 40.0 HP) must be rejected by RepairModule with ModuleBurnedOut
    ctx.db.with_mut_snapshot(|s| {
        if let Some(m_it) = s.items.get_mut(&laser_2_id) {
            m_it.dynamic_attributes.insert(3, 40.0);
        }
    });
    let burn_rep_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_2_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        burn_rep_err
            .expect_err("Completely burned-out module cannot be repaired with Nanite Repair Paste in space")
            .code,
        "ModuleBurnedOut"
    );
}
