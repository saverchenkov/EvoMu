//! Combat, fighters, and weapon systems integration tests.

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s2_fighter_mgr_unimplemented_rpc() {
    // parity: plan:S-2, rpc:charUnboundMgr.SelectCharacterID, rpc:fighterMgr.CmdActivateAbilitySlots
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(902);
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
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ball_before) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001");
    assert_eq!(ball_before.id, 100_000_001);

    let s2_err = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(100_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        s2_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "FighterNotFound"
    ));

    let (_, ball_after) = sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball 100_000_001 must remain intact in ballpark");
    assert_eq!(ball_after.id, 100_000_001);
}

#[tokio::test]
async fn test_ship_operations_logoff_scoop_drop_and_dirt() {
    // parity: rpc:ship.ResetDirtTimestamp, rpc:ship.SafeLogoff, rpc:ship.AbortSafeLogoff, rpc:ship.GetDirtTimestamp
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(903);
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

    // 1. ResetDirtTimestamp and GetDirtTimestamp
    let dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ResetDirtTimestamp",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("ResetDirtTimestamp");
    let dirt_ft = dirt.as_i64().expect("filetime");
    assert!(dirt_ft > 116_444_736_000_000_000);

    // Verify dynamic attribute 1969 was mutated in snapshot
    let dirt_dyn = ctx.db.with_snapshot(|snap| {
        snap.items.get(&100_000_001).and_then(|it| it.dynamic_attributes.get(&1969).copied())
    });
    assert_eq!(dirt_dyn, Some(dirt_ft as f64));

    // Verify GetDirtTimestamp reads the persisted timestamp
    let get_dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "GetDirtTimestamp",
            &[PyRep::Integer(100_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetDirtTimestamp");
    assert_eq!(get_dirt.as_i64(), Some(dirt_ft));

    // 2. SetSessionCheck & clear_memoized
    let sc = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect("SetSessionCheck");
    assert_eq!(sc, PyRep::Bool(true));

    let cm = ctx
        .dispatch_rpc(&mut sess, "ship", None, "clear_memoized", &[], &kw, None)
        .await
        .expect("clear_memoized");
    assert_eq!(cm, PyRep::Bool(true));

    // 3. SafeLogoff while docked returns failed conditions list (`SAFE_LOGOFF_DOCKED`)
    let sl_docked = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None)
        .await
        .expect("SafeLogoff docked");
    let PyRep::List(sl_failed) = sl_docked else {
        panic!("expected PyRep::List of failed conditions when docked")
    };
    assert!(!sl_failed.is_empty(), "docked SafeLogoff must return failed condition labels");

    // Undock to space for SafeLogoff, Drop, and Scoop tests
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sl = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &kw, None)
        .await
        .expect("SafeLogoff in space");
    assert_eq!(sl, PyRep::List(vec![]));

    let asl = ctx
        .dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &kw, None)
        .await
        .expect("AbortSafeLogoff");
    assert_eq!(asl, PyRep::Bool(true));

    // Seed item into ship cargo for Drop
    let cargo_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    // 4. Drop: Non-deployable Tritanium in cargo is rejected with UserError
    let drop_err = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::Integer(cargo_item_id)], &kw, None)
        .await
        .expect_err("Non-deployable item in Drop must fail with UserError");
    assert_eq!(drop_err.code, "UserError");

    let sys_id = sess.solar_system_id.expect("solar_system_id");
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys
        .get_ball_snapshot(100_000_001)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(glam::DVec3::ZERO);

    // Seed Mobile Depot into ship cargo for Drop
    let depot_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 33474,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    // Drop Mobile Depot: launches deployable directly as a space ball (no jetcan)
    let drop_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::Integer(depot_item_id)], &kw, None)
        .await
        .expect("Drop Mobile Depot");
    let PyRep::Dict(drop_dict) = drop_res else { panic!("expected Dict") };
    let launched = drop_dict.iter().find(|(k, _)| k.as_i64() == Some(depot_item_id)).map(|(_, v)| v).expect("found key");
    let PyRep::List(ball_ids) = launched else { panic!("expected List") };
    assert_eq!(ball_ids.len(), 1);
    assert_eq!(ball_ids[0].as_i64(), Some(depot_item_id));

    // 5. ScoopToMobileDepotHold: corvette has no mobile depot hold -> NoMobileDepotHold
    let depot_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToMobileDepotHold",
            &[PyRep::Integer(depot_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Corvette has no Mobile Depot Hold");
    assert_eq!(depot_err.code, "NoMobileDepotHold");

    // 6. ScoopToInfrastructureHold: deployable structure item; corvette has no infrastructure hold -> NoInfrastructureHold
    let infra_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 12198,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });
    let _ = sys
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: infra_item_id,
                type_id: 12198,
                position: ship_pos + glam::DVec3::new(200.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                group_id: 361,
                category_id: 22,
                owner_id: 90_000_001,
                char_id: None,
                corp_id: sess.corporation_id.unwrap_or(-1),
                alliance_id: sess.alliance_id.unwrap_or(-1),
                security_status: 0.0,
                is_cloaked: false,
            },
        )
        .await;

    let infra_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToInfrastructureHold",
            &[PyRep::Integer(infra_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Corvette has no Infrastructure Hold");
    assert_eq!(infra_err.code, "NoInfrastructureHold");

    // 7. Scoop: foreign non-abandoned container in space fails with OwnershipViolation
    let foreign_can_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 3467, // Small Secure Container (100 m3, fits in 140 m3 corvette cargo)
                owner_id: 90_000_002, // foreign owner
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });
    let _ = sys
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: foreign_can_id,
                type_id: 3467,
                position: ship_pos + glam::DVec3::new(300.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 500.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                group_id: 340,
                category_id: 2,
                owner_id: 90_000_002,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        )
        .await;

    let scoop_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Scoop",
            &[PyRep::Integer(foreign_can_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Scoop foreign non-abandoned container must fail");
    assert_eq!(scoop_err.code, "OwnershipViolation");

    // Abandon container: owner_id = 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&foreign_can_id) {
            it.owner_id = 0;
        }
    });

    let scoop_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Scoop",
            &[PyRep::Integer(foreign_can_id)],
            &kw,
            None,
        )
        .await
        .expect("Scoop abandoned container");
    let PyRep::KeyVal(sc_kv) = scoop_res else { panic!("expected KeyVal") };
    assert_eq!(sc_kv.get("flagID"), Some(&PyRep::Integer(5)));
    let can_owner_after = ctx.db.with_snapshot(|s| s.items.get(&foreign_can_id).map(|i| i.owner_id));
    assert_eq!(can_owner_after, Some(90_000_001));
}
