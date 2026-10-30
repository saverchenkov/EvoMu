//! Continuation of `sp6_logoff_and_server_tick` (`feat:lifecycle.abyssal-disconnect` & R2 Track 3 lifecycle).

use crate::common::*;
use crate::sp6_logoff_and_server_tick::{setup_pilot_in_space, sp6_item};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp7_abyssal_disconnect_20m_collapse_destroys_ship_and_pod() {
    // parity: feat:lifecycle.abyssal-disconnect
    eve_testkit::attest(&["feat:lifecycle.abyssal-disconnect"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let empty = HashMap::new();

    let (mut sess, origin_sys, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let _ = ctx
        .systems
        .get_or_spawn(origin_sys)
        .set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0))
        .await;

    // Give pilot two Abyssal filament stacks (`47765` = T1 Calm Electrical Filament, qty 3 for Frigate) and an active implant
    let filament_1 = 992_000_001_i64;
    let filament_2 = 992_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(90_000_001).or_default().push(9_899);
        let mut f1 = sp6_item(filament_1, 56_131, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new());
        f1.quantity = 3;
        f1.singleton = false;
        let mut f2 = sp6_item(filament_2, 56_131, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new());
        f2.quantity = 3;
        f2.singleton = false;
        snap.items.insert(filament_1, f1);
        snap.items.insert(filament_2, f2);
    });

    // 1. Enter Abyssal pocket via `abyssalMgr.AbyssalEntranceGateActivation`, disconnect for 600s (< 20m),
    //    reconnect in the pocket, and exit cleanly via `abyssalMgr.AbyssalEndGateActivation`
    ctx.dispatch_rpc(
        &mut sess,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(filament_1)],
        &empty,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation #1");
    assert_eq!(sess.solar_system_id, Some(32_000_001));

    handle_character_logoff_or_disconnect(&ctx, 90_000_001).await;
    ctx.advance_secs(600);
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)));

    let mut sess_reconnect = SessionState::default();
    login_pilot(&ctx, &mut sess_reconnect, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess_reconnect,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty,
        None,
    )
    .await
    .expect("SelectCharacterID inside Abyssal pocket before 20m expiry");
    assert_eq!(sess_reconnect.solar_system_id, Some(32_000_001));
    assert_eq!(sess_reconnect.ship_id, Some(ship_id));

    let (pid, goffset) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |r| {
        let p = r.abyssal_pockets.get_mut(&90_000_001).unwrap();
        p.current_room = 3;
        p.rooms[2].cleared = true;
        (p.pocket_id, p.grid_offset)
    });
    eve_server::services::pve_special_sites_abyssal::ensure_abyssal_room_balls_for_ctx(&ctx, 90_000_001).await;
    let _ = ctx
        .systems
        .get_or_spawn(32_000_001)
        .set_ball_position(ship_id, DVec3::from_array(goffset) + DVec3::new(0.0, 0.0, 100_000.0))
        .await;
    let exit_gate_id = 1_895_000_000 + pid * 10 + 3;
    ctx.dispatch_rpc(&mut sess_reconnect, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(exit_gate_id)], &empty, None)
        .await
        .expect("AbyssalEndGateActivation");
    assert_eq!(sess_reconnect.solar_system_id, Some(origin_sys));

    let _ = ctx
        .systems
        .get_or_spawn(origin_sys)
        .set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0))
        .await;

    // 2. Enter a second Abyssal pocket, disconnect inside the pocket, and advance to T+1200s (20m collapse)
    ctx.dispatch_rpc(
        &mut sess_reconnect,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(filament_2)],
        &empty,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation #2");
    assert_eq!(sess_reconnect.solar_system_id, Some(32_000_001));

    handle_character_logoff_or_disconnect(&ctx, 90_000_001).await;

    // At T+1199s (19m59s), ship is still alive in the pocket
    ctx.advance_secs(1_199);
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)));

    // At T+1200s (20m00s), the Abyssal pocket collapses, destroying ship + capsule and respawning pilot at home station in their race rookie corvette (`588` Reaper)
    ctx.advance_secs(1);

    // Log the pilot back in (`SelectCharacterID`) after clock advance with no Abyssal RPC calls
    let mut sess_relogin = SessionState::default();
    login_pilot(&ctx, &mut sess_relogin, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess_relogin, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &empty, None)
        .await
        .expect("SelectCharacterID after Abyssal collapse");
    assert_eq!(sess_relogin.station_id, Some(60_003_760));
    assert_eq!(sess_relogin.solar_system_id, Some(30_000_142));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_id));
        assert!(snap.active_implants.get(&90_000_001).is_none_or(|m| m.is_empty()));
        let ch = &snap.characters[&90_000_001];
        assert_eq!(ch.station_id, Some(ch.home_station_id));
        assert_eq!(snap.items[&ch.active_ship_id].type_id, 588);
        let pocket_kms: Vec<_> = snap.killmails.values()
            .filter(|km| km.victim_character_id == 90_000_001 && km.solar_system_id == 32_000_001)
            .collect();
        assert_eq!(pocket_kms.len(), 2, "Both ship and capsule killmails must be recorded in Abyssal pocket 32000001");
        assert!(pocket_kms.iter().any(|km| km.ship_type_id == 670), "Capsule (670) killmail must be recorded");
        assert!(pocket_kms.iter().any(|km| km.ship_type_id != 670), "Ship killmail must be recorded");
    });
}

#[tokio::test]
async fn test_r2_track3_lifecycle_filaments_salvage_and_drones() {
    let ctx = ServerContext::new();
    let empty = HashMap::new();
    let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Issue #75: SafeLogoff blocked while in warp or with persisted module_active_states
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_750,
            InventoryItemRecord {
                item_id: 990_750,
                type_id: 34593, // Entosis Link I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(990_750);
    });
    let sl_err = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff with persisted active module");
    assert_eq!(
        sl_err,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_ACTIVE_MODULES".into())])
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&990_750);
    });

    // 2. Issue #76: Filament jump blocked while warp scrambled
    let fil_id = 990_760;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 626; // Vexor (Cruiser)
        }
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 47_765,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 2;
        }
    }
    let scram_fil = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(fil_id)],
            &empty,
            None,
        )
        .await
        .expect_err("Warp-scrambled ship must not activate Abyssal filament");
    assert_eq!(scram_fil.code, "WarpScrambled");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 0;
        }
    }

    // 3. Issue #81: LaunchDrones deduplicates drone_ids, requires DroneBay + category 18, and splits stacks
    let stack_drone_id = 990_810;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_drone_id,
            InventoryItemRecord {
                item_id: stack_drone_id,
                type_id: 2454, // Hobgoblin I (category 18)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let launched = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![
                PyRep::Integer(stack_drone_id),
                PyRep::Integer(stack_drone_id),
            ])],
            &empty,
            None,
        )
        .await
        .expect("LaunchDrones from stack with duplicate ID");
    let PyRep::List(balls) = launched else {
        panic!("Expected list of launched drone ball IDs");
    };
    assert_eq!(balls.len(), 1, "Duplicate drone IDs must be deduplicated");
    let rem_qty = ctx
        .db
        .with_snapshot(|s| s.items.get(&stack_drone_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_qty, 4, "Launching 1 drone from stack of 5 must leave 4 in DroneBay");

    // 4. Issue #80: ScoopToDroneBay rejects non-drone items (ships/structures)
    let foreign_ship_id = 990_800;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            foreign_ship_id,
            InventoryItemRecord {
                item_id: foreign_ship_id,
                type_id: 626, // Vexor (Ship, category 6)
                owner_id: 90_000_002,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ship_pos = sys_handle
        .get_ball_position_sync(ship_id)
        .unwrap_or(glam::DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: foreign_ship_id,
            type_id: 626,
            position: ship_pos + glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let scoop_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToDroneBay",
            &[PyRep::Integer(foreign_ship_id)],
            &empty,
            None,
        )
        .await
        .expect_err("Must not scoop a ship into DroneBay");
    assert_eq!(scoop_err.code, "NotADrone");

    // 5. Issue #79: CmdSalvage rejects non-wreck targets (must never delete ships/structures)
    let salv_drone_id = 990_790;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            salv_drone_id,
            InventoryItemRecord {
                item_id: salv_drone_id,
                type_id: 32787, // Salvage Drone I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(salv_drone_id)])],
        &empty,
        None,
    )
    .await
    .expect("Launch Salvage Drone I");
    let salv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdSalvage",
            &[PyRep::Integer(foreign_ship_id)],
            &empty,
            None,
        )
        .await
        .expect_err("CmdSalvage must reject non-wreck ship target");
    assert_eq!(salv_err.code, "InvalidSalvageTarget");
    assert!(
        ctx.db.with_snapshot(|s| s.items.contains_key(&foreign_ship_id)),
        "Non-wreck ship must never be deleted by CmdSalvage"
    );
}
