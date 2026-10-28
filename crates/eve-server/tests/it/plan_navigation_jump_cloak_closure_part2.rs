//! Integration tests for Space Navigation: Cloak targeting delay, live fitting transitions,
//! unfitting decloak safety, and hull/subsystem delay modifiers (`feat:navigation.cloak-targeting-delay`).

#![forbid(unsafe_code)]

use crate::plan_navigation_jump_cloak_closure::setup_nav_pilot;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYSTEM: u32 = 30_000_142;
const PILOT_1: i64 = 93_200_001;
const PILOT_2: i64 = 93_200_002;
const SHIP_1: i64 = 83_200_001;
const SHIP_2: i64 = 83_200_002;

// parity: feat:navigation.cloak-targeting-delay
#[tokio::test]
async fn test_space_navigation_cloak_targeting_delay_and_overrides() {
    eve_testkit::attest(&["feat:navigation.cloak-targeting-delay"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let origin_pos = DVec3::new(15_000_000.0, 0.0, 0.0);
    let mut s1 = setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 28661, JITA_SYSTEM, origin_pos).await; // Kronos
    let _s2 = setup_nav_pilot(
        &ctx,
        PILOT_2,
        SHIP_2,
        11999,
        JITA_SYSTEM,
        origin_pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let kw = HashMap::new();

    // =========================================================================
    // 1. Kronos (28661) + Prototype Cloaking Device I (11370, 30s) + Cloaking II (-20% -> 24.0s)
    //    Verified via direct dogmaIM.Activate -> dogmaIM.Deactivate WITHOUT UpdateStateRequest,
    //    followed by ticking past 24s so AddTarget succeeds!
    // =========================================================================
    let cloak_item_id = 92_600_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_1).or_default().insert(
            11579,
            TrainedSkillRecord {
                type_id: 11579,
                level: 2,
                skill_points: 8_000.0,
            },
        );
        snap.items.insert(
            cloak_item_id,
            InventoryItemRecord {
                item_id: cloak_item_id,
                type_id: 11370, // Prototype Cloaking Device I (SDE attr 560 = 30000ms)
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);

    // Activate cloak directly via dogmaIM.Activate (no beyonce.UpdateStateRequest!)
    let act_res = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert!(act_res.is_ok(), "Cloak activate: {act_res:?}");
    let cs_cloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        cs_cloaked.is_cloaked,
        "CombatState.is_cloaked must sync directly on dogmaIM.Activate"
    );
    assert!(
        (cs_cloaked.cloak_configured_targeting_delay_s - 24.0).abs() < 0.1,
        "Configured targeting delay must be 24.0s on Kronos with Cloaking II"
    );

    // AddTargetOBO must also fail while cloaked WITHOUT stripping undock/location-change invulnerability (`R-CLOAK-OBO-01`)
    {
        let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        cs_inv.undock_invulnerable_remaining_ticks = 30;
        jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
    }
    let obo_cloaked = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTargetOBO",
            &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(obo_cloaked.unwrap_err().code, "TargetingFailed");
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        30,
        "Rejected AddTargetOBO while cloaked must not clear undock_invulnerable_remaining_ticks"
    );
    assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());

    // Deactivate cloak directly via dogmaIM.Deactivate
    let deact_res = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert!(deact_res.is_ok(), "Cloak deactivate: {deact_res:?}");
    let cs_decloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(!cs_decloaked.is_cloaked, "Ship is decloaked");
    assert!(
        (cs_decloaked.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "Decloak targeting delay must be 24.0s: got {:.2}s",
        cs_decloaked.decloak_targeting_delay_remaining_s
    );

    // Immediate re-cloak blocked by module reactivation delay (`P2-03`)
    let recloak_err = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(recloak_err.unwrap_err().code, "ModuleReactivationDelay");

    // Target lock blocked while sensor recalibration delay is active (`AddTarget` and `AddTargetOBO`)
    let lock_blocked = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(lock_blocked.unwrap_err().code, "TargetingFailed");
    let obo_recal = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTargetOBO",
            &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(obo_recal.unwrap_err().code, "TargetingFailed");
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        30,
        "Rejected AddTarget/AddTargetOBO during sensor recalibration must not clear undock_invulnerable_remaining_ticks"
    );
    assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());
    {
        let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        cs_inv.undock_invulnerable_remaining_ticks = 0;
        jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
    }

    // Tick 23s: 1.0s remaining -> AddTarget still blocked
    for _ in 0..23 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    assert!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .decloak_targeting_delay_remaining_s
            > 0.5
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "TargetingFailed"
    );

    // Tick 2 more seconds (25s total > 24s): recalibration timer expires -> AddTarget succeeds!
    jita_sys.step_manual(1.0).await.unwrap();
    jita_sys.step_manual(1.0).await.unwrap();
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .decloak_targeting_delay_remaining_s,
        0.0
    );
    let lock_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert!(
        lock_ok.is_ok(),
        "AddTarget must succeed once Kronos 24s decloak recalibration timer expires: {lock_ok:?}"
    );
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "RemoveTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;

    // =========================================================================
    // 2. Unfitting an active cloak (`invbroker.Add` to Cargo) and `TakeModuleOffline`
    //    must immediately clear `is_cloaked` (no permacloak) and start recalibration delay!
    // =========================================================================
    ctx.advance_secs(35);
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cloak_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Re-activate cloak after reactivation delay");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);

    // Unfit active cloak into ship Cargo via invbroker.Add while within 2,500m of a Mobile Depot
    let depot_id = 92_600_099i64;
    let (_, ship1_ball) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33474, // Mobile Depot (group 1246)
                owner_id: PILOT_1,
                location_id: JITA_SYSTEM as i64,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: depot_id,
            type_id: 33474,
            position: ship1_ball.position + DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 75.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let ship_inv_bid = ctx
        .bind_object(&mut s1, "invbroker", SHIP_1)
        .expect("Bind ship invbroker");
    let unfit_res = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_inv_bid),
            "Add",
            &[
                PyRep::Integer(cloak_item_id),
                PyRep::Integer(SHIP_1),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(unfit_res.is_ok(), "Unfit active cloak to cargo: {unfit_res:?}");
    let cs_after_unfit = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        !cs_after_unfit.is_cloaked,
        "Unfitting an active cloak must immediately clear is_cloaked"
    );
    assert!(
        (cs_after_unfit.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "Unfitting an active cloak must start the 24.0s recalibration delay"
    );
    for _ in 0..25 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // Also verify `invbroker.MultiAdd` unfitting an active cloak in space at 2,500m from Mobile Depot (`R-CLOAK-UNFIT-01`)
    ctx.dispatch_rpc(
        &mut s1,
        "invbroker",
        Some(ship_inv_bid),
        "Add",
        &[
            PyRep::Integer(cloak_item_id),
            PyRep::Integer(SHIP_1),
            PyRep::Integer(InventoryFlag::HiSlot1 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Re-fit cloak into HiSlot1 near Mobile Depot");
    ctx.advance_secs(35);
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cloak_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate re-fitted cloak");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    let multi_unfit = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_inv_bid),
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(cloak_item_id)]),
                PyRep::Integer(SHIP_1),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(multi_unfit.is_ok(), "MultiAdd unfit active cloak to cargo: {multi_unfit:?}");
    let cs_after_multi = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        !cs_after_multi.is_cloaked,
        "MultiAdd unfitting an active cloak must immediately clear is_cloaked"
    );
    assert!(
        (cs_after_multi.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "MultiAdd unfitting an active cloak must start the 24.0s recalibration delay, got {:.2}s",
        cs_after_multi.decloak_targeting_delay_remaining_s
    );
    let _ = jita_sys.remove_ball(depot_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&depot_id);
    });
    for _ in 0..25 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // =========================================================================
    // 3. Stealth Bomber (Nemesis 11377 -> 0s) & Covert Ops Frigate (Anathema 11188 -> 0s)
    // =========================================================================
    let bomber_cloak_id = 92_600_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&cloak_item_id);
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 11377; // Nemesis (group 834, 1035 = 0.0)
        snap.items.insert(
            bomber_cloak_id,
            InventoryItemRecord {
                item_id: bomber_cloak_id,
                type_id: 11370,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 11377,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11377),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
        .await
        .expect("Bomber cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
        .await
        .expect("Bomber cloak deactivate");
    assert_eq!(
        jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
        0.0
    );
    assert!(ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await.is_ok());
    let _ = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await;

    // Covert Ops Frigate (Anathema 11188 + Covert Ops Cloaking Device II 11578 -> 0s)
    let covert_cloak_id = 92_600_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&bomber_cloak_id);
        snap.skills.entry(PILOT_1).or_default().insert(
            11579,
            TrainedSkillRecord {
                type_id: 11579,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 11188; // Anathema
        snap.items.insert(
            covert_cloak_id,
            InventoryItemRecord {
                item_id: covert_cloak_id,
                type_id: 11578,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 11188,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11188),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
        .await
        .expect("Covert Ops cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
        .await
        .expect("Covert Ops cloak deactivate");
    assert_eq!(
        jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
        0.0
    );

    // =========================================================================
    // 4. Non-zero `covertOpsStealthBomberTargettingDelay` (`1035 = 5000.0` ms -> 5.0s)
    //    on Black Ops (Redeemer 22428 override) / Endurance (37135) AND T3 Strategic Cruiser
    //    (Legion 29986 + Covert Reconfiguration 45586 + Covert Ops Cloak II 11578 -> 5.0s)!
    // =========================================================================
    let blops_cloak_id = 92_600_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&covert_cloak_id);
        let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
        ship_it.type_id = 22428; // Redeemer Black Ops
        ship_it.dynamic_attributes.insert(1035, 5000.0); // Non-zero 5000ms override
        snap.items.insert(
            blops_cloak_id,
            InventoryItemRecord {
                item_id: blops_cloak_id,
                type_id: 11370, // 30s base Prototype Cloak overridden to 5.0s by attr 1035!
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 22428,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 2.0,
                mass: 100_000_000.0,
                radius: 300.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 22428),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
        .await
        .expect("Black Ops cloak activate");
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
        .await
        .expect("Black Ops cloak deactivate");
    let cs_blops = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        (cs_blops.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
        "Non-zero Black Ops 1035 override must yield 5.0s delay, got {:.2}s",
        cs_blops.decloak_targeting_delay_remaining_s
    );
    for _ in 0..6 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // T3 Strategic Cruiser: Legion (29986) + Legion Defensive - Covert Reconfiguration (45586) + Covert Cloak II (11578)
    let t3c_sub_id = 92_600_030i64;
    let t3c_cloak_id = 92_600_031i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&blops_cloak_id);
        let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
        ship_it.type_id = 29986; // Legion
        ship_it.dynamic_attributes.clear();
        snap.items.insert(
            t3c_sub_id,
            InventoryItemRecord {
                item_id: t3c_sub_id,
                type_id: 45586, // Legion Defensive - Covert Reconfiguration
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::SubSystemSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            t3c_cloak_id,
            InventoryItemRecord {
                item_id: t3c_cloak_id,
                type_id: 11578, // Covert Ops Cloaking Device II (10s base * 0.5 Cloaking V = 5.0s; 0 CPU via 45586)
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 29986,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 2.0,
                mass: 15_000_000.0,
                radius: 170.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
        .await
        .expect("T3C Covert Reconfiguration cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
        .await
        .expect("T3C Covert Reconfiguration cloak deactivate");
    let cs_t3c = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        (cs_t3c.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
        "T3C Covert Reconfiguration + Covert Cloak II + Cloaking V must evaluate to 5.0s via ShipFittingEngine, got {:.2}s",
        cs_t3c.decloak_targeting_delay_remaining_s
    );

    // Gate-cloak break via AddTarget & AddTargetOBO: breaks 60s jump cloak with 0s delay & starts lock
    for _ in 0..6 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    for use_obo in [false, true] {
        let mut gc = ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986);
        gc.is_cloaked = true;
        gc.gate_cloak_remaining_ticks = 60;
        let (_, b1) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
        let _ = jita_sys.spawn_ball_with_combat(b1, gc).await;
        let _ = jita_sys.clear_target_locks(SHIP_1).await;
        let res = if use_obo {
            ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTargetOBO", &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)], &kw, None).await
        } else {
            ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await
        };
        assert!(res.is_ok(), "Gate-cloak target lock (obo={use_obo}) must succeed: {res:?}");
        let cs_after = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(!cs_after.is_cloaked && cs_after.gate_cloak_remaining_ticks == 0 && cs_after.decloak_targeting_delay_remaining_s == 0.0);
        assert_eq!(jita_sys.get_target_locks(SHIP_1).await.unwrap().len(), 1);
    }
}
