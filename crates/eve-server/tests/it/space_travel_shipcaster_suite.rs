//! Integration test suite for Shipcaster and Space Travel RPC Services.
//!
//! Validates:
//! - `shipCasterLandingPadMgr` (GetAllLandingPads, GetFactionLandingPads)
//! - `shipCasterLauncherMgr` (GetFactionsWithShipcaster)
//! - `starterShipcasterTravelMgr` (CanCharacterJump, CmdJumpThroughStarterShipcaster)
//! - `shipcasterTravelMgr` (CmdJumpThroughShipcaster)
//! - `populationCap` (GetCharacterLoadSlot, MoveCharacterToNewSystem)

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::with_misc_state;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    }
}

fn kv_bool(rep: &PyRep, key: &str) -> bool {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()).unwrap_or(false),
        _ => false,
    }
}

async fn spawn_ship_at(
    ctx: &ServerContext,
    handle: &eve_server::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    pos: DVec3,
) {
    let _ = handle.remove_ball(ship_id).await;
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;
}

async fn deploy_and_complete_beacon(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    constructor_type_id: u32,
    war_faction_id: i64,
    warzone_sys: u32,
) -> i64 {
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(warzone_sys);
    let cid = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = warzone_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = Some(war_faction_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = warzone_sys as i64;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: constructor_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        id
    });
    let h = ctx.systems.get_or_spawn(warzone_sys);
    spawn_ship_at(ctx, &h, char_id, ship_id, DVec3::new(101_000.0, 0.0, 0.0)).await;
    let launched = rpc(
        ctx,
        sess,
        "ship",
        "LaunchForSelf",
        &[
            PyRep::List(vec![PyRep::Integer(cid)]),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("LaunchForSelf beacon constructor");
    let PyRep::List(launched_list) = launched else {
        panic!("expected LaunchForSelf list");
    };
    let pad_id = kv_i64(&launched_list[0], "itemID");
    let comps = ctx.sde.shipcaster_sde_components().expect("SDE components");
    let spec = &comps.under_construction[&constructor_type_id];
    let mut qty_dict = Vec::new();
    for (&mat_tid, entry) in &spec.input_items {
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: mat_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: i32::try_from(entry.required_qty).unwrap(),
                    ..Default::default()
                },
            );
        });
        qty_dict.push((
            PyRep::Integer(i64::from(mat_tid)),
            PyRep::Integer(i64::from(entry.required_qty)),
        ));
    }
    rpc(
        ctx,
        sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(pad_id),
            PyRep::String("underConstruction".into()),
            PyRep::String("DepositItemsFromShip".into()),
            PyRep::Dict(qty_dict),
        ],
    )
    .await
    .expect("DepositItemsFromShip");
    pad_id
}

#[tokio::test]
async fn test_shipcaster_landing_pads_and_launchers() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(600);
    let mut sess = SessionState::new(20_201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. Fresh cluster starts with 0 Empire landing pads until constructed in FW space
    let init_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    assert_eq!(init_pads, PyRep::List(vec![]));

    // Deploy and construct 1 Caldari beacon (Onatoh 30002811) and 1 Minmatar beacon (Resbroko 30002056)
    let _cal_pad = deploy_and_complete_beacon(&ctx, &mut sess, char_id, ship_id, 75933, 500_001, 30002811).await;
    let _min_pad = deploy_and_complete_beacon(&ctx, &mut sess, char_id, ship_id, 75935, 500_002, 30002056).await;

    let all_pads_rep = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    let PyRep::List(all_pads) = all_pads_rep else {
        panic!("GetAllLandingPads must return a List");
    };
    assert_eq!(all_pads.len(), 2);
    for pad in &all_pads {
        assert!(kv_i64(pad, "itemID") > 0);
        assert!(kv_bool(pad, "isBuilt") && kv_bool(pad, "isLinked") && kv_bool(pad, "canBeLinked") && !kv_bool(pad, "isDisrupted"));
    }

    // 2. GetFactionLandingPads
    let caldari_pads_rep = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500001)],
    )
    .await
    .expect("GetFactionLandingPads for Caldari");
    let PyRep::List(caldari_pads) = caldari_pads_rep else {
        panic!("GetFactionLandingPads must return a List");
    };
    assert_eq!(caldari_pads.len(), 1);
    assert_eq!(kv_i64(&caldari_pads[0], "factionID"), 500001);
    assert_eq!(kv_i64(&caldari_pads[0], "solarSystemID"), 30002811);

    let minmatar_pads_rep = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500002)],
    )
    .await
    .expect("GetFactionLandingPads for Minmatar");
    let PyRep::List(minmatar_pads) = minmatar_pads_rep else {
        panic!("GetFactionLandingPads must return a List");
    };
    assert_eq!(minmatar_pads.len(), 1);
    assert_eq!(kv_i64(&minmatar_pads[0], "factionID"), 500002);
    assert_eq!(kv_i64(&minmatar_pads[0], "solarSystemID"), 30002056);

    let missing_faction_err = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[])
        .await
        .expect_err("Missing factionID must fail");
    assert_eq!(missing_faction_err.code, "InvalidArguments");

    // 3. GetFactionsWithShipcaster
    let factions_rep = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    assert_eq!(
        factions_rep,
        PyRep::List(vec![
            PyRep::Integer(500001),
            PyRep::Integer(500002),
            PyRep::Integer(500003),
            PyRep::Integer(500004),
        ])
    );
}

#[tokio::test]
async fn test_starter_and_interstellar_shipcaster_jumps() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(600);
    let mut sess = SessionState::new(20_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    let starter_sys: u32 = 30100070; // Tatrys (Caldari Exordium embassy system)
    let war_hq_sys: u32 = 30045324;  // Onnamon (Caldari War HQ)
    let warzone_sys: u32 = 30002811; // Onatoh (Caldari FW lowsec)

    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    let cal_pad_id = deploy_and_complete_beacon(&ctx, &mut sess, char_id, ship_id, 75933, 500_001, warzone_sys).await;
    let handle_warzone_pre = ctx.systems.get_or_spawn(warzone_sys);
    let _ = handle_warzone_pre.remove_ball(ship_id).await;

    let (starter_launcher_id, starter_pos, hq_launcher_id, hq_pos) = with_misc_state(&ctx, |st| {
        let sl = st.starter_launchers.iter().find(|l| l.solar_system_id == starter_sys).unwrap();
        let hl = st.launchers.iter().find(|l| l.faction_id == 500_001).unwrap();
        (sl.item_id, sl.position, hl.item_id, hl.position)
    });

    let now = ctx.now_unix();
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.create_date_unix = now - 3 * 86_400; // 3-day-old rookie
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = starter_sys as i64;
        }
    });

    let handle_starter = ctx.systems.get_or_spawn(starter_sys);

    // 1. starterShipcasterTravelMgr.CanCharacterJump
    let can_jump_rep = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump_rep, PyRep::Bool(true));

    // 2. Negative test: jump fails closed when ship ball is not in space
    let missing_ship_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .expect_err("Must fail when ship ball is not in space");
    assert_eq!(missing_ship_err.code, "ShipNotInSpace");

    spawn_ship_at(&ctx, &handle_starter, char_id, ship_id, starter_pos + DVec3::new(50_000.0, 0.0, 0.0)).await;

    let zero_target_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("Must fail when target_id is 0");
    assert_eq!(zero_target_err.code, "TargetNotFound");

    // 3. Negative test: jump fails when outside 5,000m range
    let out_of_range_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .expect_err("Must fail when outside operational range");
    assert_eq!(out_of_range_err.code, "OutsideOperationalRange");

    spawn_ship_at(&ctx, &handle_starter, char_id, ship_id, starter_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;

    // 4. Negative test: jump blocked by weapons timer
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = now + 60;
    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    let timer_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .expect_err("Must fail under weapons timer");
    assert_eq!(timer_err.code, "WeaponsTimerActive");
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = 0;

    // 5. Positive test: Starter jump succeeds and moves ball to Onnamon War HQ
    ctx.advance_secs(12);
    let starter_jump_res = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(starter_jump_res, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(war_hq_sys));

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    assert!(handle_starter.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_hq = ctx.systems.get_or_spawn(war_hq_sys);
    assert!(handle_hq.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // 6. Interstellar Shipcaster Jump: Negative test - non-existent landing pad rejected
    let unlinked_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(999_999)],
    )
    .await
    .expect_err("Unknown landing pad must be rejected");
    assert_eq!(unlinked_err.code, "LandingPadNotFound");

    spawn_ship_at(&ctx, &handle_hq, char_id, ship_id, hq_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;

    // 7. Positive test: Interstellar jump succeeds from Onnamon War HQ to built warzone beacon
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    let shipcaster_jump = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(cal_pad_id)],
    )
    .await
    .expect("CmdJumpThroughShipcaster");
    assert_eq!(shipcaster_jump, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(warzone_sys));

    assert!(handle_hq.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let handle_warzone = ctx.systems.get_or_spawn(warzone_sys);
    assert!(handle_warzone.get_ball_snapshot(ship_id).await.unwrap().is_some());

    let _ = handle_warzone.remove_ball(ship_id).await;
    spawn_ship_at(&ctx, &handle_hq, char_id, ship_id, hq_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    sess.solar_system_id = Some(war_hq_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = war_hq_sys;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = war_hq_sys as i64;
        }
    });

    let re_jump_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(cal_pad_id)],
    )
    .await
    .expect_err("Immediate re-jump must fail with session change timer active");
    assert_eq!(re_jump_err.code, "SessionChangeInProgress");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    sess.station_id = Some(60_015_184);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_015_184);
        }
    });
    let docked_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(cal_pad_id)],
    )
    .await
    .expect_err("Docked shipcaster jump must be rejected");
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 19720;
        }
    });
    let cap_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(cal_pad_id)],
    )
    .await
    .expect_err("Capital ship jump must be rejected");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 601;
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_002);
        }
    });
    let fw_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(hq_launcher_id), PyRep::Integer(warzone_sys as i64), PyRep::Integer(cal_pad_id)],
    )
    .await
    .expect_err("Hostile militia character must be rejected");
    assert_eq!(fw_err.code, "FactionShipcasterAccessDenied");
}

#[tokio::test]
async fn test_population_cap_load_slots_and_migration() {
    // parity: rpc:populationCap.MoveCharacterToNewSystem
    eve_testkit::attest(&["rpc:populationCap.MoveCharacterToNewSystem"]);

    let ctx = ServerContext::new();
    let mut sess = SessionState::new(20_203);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    let uncapped_sys = 30000143; // Non-capped system
    let capped_sys = 30002813;   // Tama (capped system)

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = uncapped_sys;
        }
    });

    with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(capped_sys, 0);
    });

    // 1. Uncapped system returns single slot with no alternative
    let normal_slots_rep = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "GetCharacterLoadSlot",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("GetCharacterLoadSlot for uncapped system");
    let PyRep::Dict(normal_slots) = normal_slots_rep else {
        panic!("GetCharacterLoadSlot must return a Dict");
    };
    assert_eq!(normal_slots.len(), 1, "Uncapped system has exactly 1 load slot");
    assert_eq!(normal_slots[0].0, PyRep::Integer(uncapped_sys as i64));
    assert_eq!(normal_slots[0].1, PyRep::None);

    // Negative test: Arbitrary move attempt on uncapped system without offered slot must fail (R-SC-03)
    let uncapped_move_err = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(30002811), PyRep::String("arbitrary-key".into())],
    )
    .await
    .expect_err("Arbitrary move without offer must fail");
    assert_eq!(uncapped_move_err.code, "SystemNotOffered");

    // 2. Set system to capped system (Tama)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = capped_sys;
        }
    });

    let capped_slots_rep = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "GetCharacterLoadSlot",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("GetCharacterLoadSlot for capped system");
    let PyRep::Dict(capped_slots) = capped_slots_rep else {
        panic!("GetCharacterLoadSlot must return a Dict");
    };
    assert_eq!(capped_slots.len(), 2, "Capped system offers alternative load slot");
    assert_eq!(capped_slots[0].0, PyRep::Integer(capped_sys as i64));
    assert_eq!(capped_slots[0].1, PyRep::None);
    let PyRep::Integer(sys_id) = &capped_slots[1].0 else {
        panic!("Expected Integer for offered_sys");
    };
    let offered_sys = *sys_id as u32;
    let PyRep::String(slot_key) = &capped_slots[1].1 else {
        panic!("Offered alternative slot must include slotKey string");
    };
    assert!(!slot_key.is_empty(), "slotKey must not be empty");

    // 3. Negative test: Unowned character rejected (R-SC-03)
    let unowned_err = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(99_999_999), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
    )
    .await
    .expect_err("Unowned character must be rejected");
    assert_eq!(unowned_err.code, "CharacterOwnershipError");

    // 4. Negative test: Unoffered system rejected even if in SDE (R-SC-03)
    let unoffered_err = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(30000142), PyRep::String(slot_key.clone())],
    )
    .await
    .expect_err("Unoffered system must be rejected");
    assert_eq!(unoffered_err.code, "SystemNotOffered");

    // 5. Negative test: Invalid slot key rejected (R-SC-03)
    let invalid_key_err = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String("bogus-slot-key".into())],
    )
    .await
    .expect_err("Invalid slot key must be rejected");
    assert_eq!(invalid_key_err.code, "SystemNotOffered");

    // 6. Positive test: MoveCharacterToNewSystem to offered alternative system succeeds (R-SC-03)
    let move_res = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
    )
    .await
    .expect("MoveCharacterToNewSystem");
    assert_eq!(move_res, PyRep::None);

    // Verify character relocated and docking state cleared
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("character in snapshot");
        assert_eq!(ch.solar_system_id, offered_sys, "Character solar system updated");
        assert_eq!(ch.station_id, None, "Station cleared");
        assert_eq!(ch.structure_id, None, "Structure cleared");
    });

    // 7. Consumed offer cannot be reused
    let reuse_err = rpc(
        &ctx,
        &mut sess,
        "populationCap",
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(offered_sys as i64), PyRep::String(slot_key.clone())],
    )
    .await
    .expect_err("Reusing consumed slot key must be rejected");
    assert_eq!(reuse_err.code, "SystemNotOffered");
}
