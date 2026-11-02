//! Integration tests for SDE-backed Faction Warfare Interstellar Shipcasters, Dynamic FW Beacon
//! Construction/Linking/Cycling/Disruption, and Rookie Starter Shipcasters.

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

async fn spawn_test_ship(
    ctx: &ServerContext,
    handle: &eve_server::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    let _ = handle.remove_ball(ship_id).await;
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, type_id),
        )
        .await;
}

fn place_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_id as i64;
        }
    });
}

async fn seed_and_build_caldari_beacon(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    lowsec_sys: u32,
    pos: DVec3,
) -> PyRep {
    place_pilot_in_space(ctx, sess, char_id, ship_id, lowsec_sys);
    let handle = ctx.systems.get_or_spawn(lowsec_sys);
    spawn_test_ship(ctx, &handle, char_id, ship_id, 601, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    let cid = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
        }
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: 75933, // State Shipcaster Beacon Constructor
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        cid
    });

    let launched = rpc(
        ctx,
        sess,
        "ship",
        "LaunchForSelf",
        &[
            PyRep::List(vec![PyRep::Integer(cid)]),
            PyRep::Real(pos.x),
            PyRep::Real(pos.y),
            PyRep::Real(pos.z),
        ],
    )
    .await
    .expect("LaunchForSelf beacon constructor");
    let PyRep::List(launched_list) = launched else {
        panic!("expected LaunchForSelf list");
    };
    let deployed = launched_list[0].clone();
    let pad_id = kv_i64(&deployed, "itemID");
    assert!(!kv_bool(&deployed, "isBuilt") && !kv_bool(&deployed, "isLinked"));
    assert_eq!(kv_i64(&deployed, "linkTimestamp"), 0);

    let comps = ctx.sde.shipcaster_sde_components().expect("SDE components");
    let spec = comps.under_construction.get(&75933).expect("75933 spec");
    let mut qty_dict = Vec::new();
    for (&mat_tid, entry) in &spec.input_items {
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let sid = snap.next_entity_id;
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
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
    let dep_res = rpc(
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
    assert_eq!(
        dep_res,
        PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Dict(vec![])])
    );

    let pads = rpc(
        ctx,
        sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("GetFactionLandingPads");
    let PyRep::List(list) = pads else {
        panic!("expected list");
    };
    list.into_iter()
        .find(|p| kv_i64(p, "itemID") == pad_id)
        .expect("constructed pad in GetFactionLandingPads")
}

#[tokio::test]
async fn test_shipcaster_dynamic_fw_beacon_construction_linking_limit_and_disruption() {
    // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
    // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
    // parity: notify:OnShipcasterTargetStateChanged
    eve_testkit::attest(&[
        "rpc:shipCasterLandingPadMgr.GetAllLandingPads",
        "rpc:shipCasterLandingPadMgr.GetFactionLandingPads",
        "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster",
        "notify:OnShipcasterTargetStateChanged",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(4_200);
    let mut sess = SessionState::new(30_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. Verify SDE War HQ launchers exist in Mehatoor, Onnamon, Intaki, and Amo with 0 initial empire pads
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
    with_misc_state(&ctx, |st| {
        let mut hq: Vec<(i32, u32, u32)> = st
            .launchers
            .iter()
            .filter(|l| matches!(l.faction_id, 500001..=500004))
            .map(|l| (l.faction_id, l.type_id, l.solar_system_id))
            .collect();
        hq.sort_by_key(|x| x.0);
        assert_eq!(
            hq,
            vec![
                (500001, 76466, 30045324), // Caldari -> Onnamon
                (500002, 76468, 30002055), // Minmatar -> Amo
                (500003, 76465, 30002974), // Amarr -> Mehatoor
                (500004, 76467, 30003788), // Gallente -> Intaki
            ]
        );
    });

    let init_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    assert_eq!(init_pads, PyRep::List(vec![]), "No empire landing pads may be statically pre-seeded");

    let no_arg_err = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[])
        .await
        .expect_err("Missing factionID");
    assert_eq!(no_arg_err.code, "InvalidArguments");

    // 2. Highsec deployment rejected; lowsec FW deployment + SDE material contributions build & link up to 3 pads
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, 30_000_142); // Jita (highsec)
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    spawn_test_ship(&ctx, &jita_handle, char_id, ship_id, 601, DVec3::new(100_000.0, 0.0, 0.0)).await;
    let hs_cid = ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 75933,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        id
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", "LaunchForSelf", &[PyRep::Integer(hs_cid), PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .expect_err("NaN coordinates must fail")
            .code,
        "InvalidCoordinates"
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", "LaunchForSelf", &[PyRep::Integer(hs_cid), PyRep::Real(900_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .expect_err("Out-of-range coordinates must fail")
            .code,
        "OutOfRange"
    );
    let hs_err = rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchForSelf",
        &[PyRep::List(vec![PyRep::Integer(hs_cid)])],
    )
    .await
    .expect_err("Highsec beacon deployment must fail");
    assert_eq!(hs_err.code, "InvalidBeaconDeploymentSystem");

    let _ = ctx.drain_notifications_for_char(char_id);
    let lp_before = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&1_000_180).copied().unwrap_or(0));

    // Build 4 Caldari beacons across lowsec systems (Onatoh 30002811, Tama 30002813, Kedama 30002812, Nennamaila 30045344)
    let lowsec_systems = [30002811_u32, 30002813, 30002812, 30045344];
    let mut built_pads = Vec::new();
    for (idx, &ls_sys) in lowsec_systems.iter().enumerate() {
        ctx.advance_secs(30);
        let pad_rep = seed_and_build_caldari_beacon(
            &ctx,
            &mut sess,
            char_id,
            ship_id,
            ls_sys,
            DVec3::new(100_000.0 + (idx as f64) * 5_000.0, 0.0, 0.0),
        )
        .await;
        built_pads.push(pad_rep);
    }

    // Verify militia LP rewarded from SDE underConstruction inputItems (28,000 LP per completed beacon)
    let lp_after = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&1_000_180).copied().unwrap_or(0));
    assert!(lp_after >= lp_before + 4 * 28_000);

    // First 3 built pads auto-linked with real dynamic timestamps; 4th built pad stays unlinked due to 3-pad cap!
    for pad in built_pads.iter().take(3) {
        assert_eq!(kv_i64(pad, "typeID"), 75929);
        assert!(kv_bool(pad, "isBuilt") && kv_bool(pad, "isLinked"));
        assert!(kv_i64(pad, "linkTimestamp") > 1_700_004_000);
    }
    let fourth_pad_id = kv_i64(&built_pads[3], "itemID");
    assert!(kv_bool(&built_pads[3], "isBuilt") && kv_bool(&built_pads[3], "canBeLinked"));
    assert!(!kv_bool(&built_pads[3], "isLinked"));
    assert_eq!(kv_i64(&built_pads[3], "linkTimestamp"), 0);

    let cap_err = eve_server::services::shipcaster_ops::link_fw_landing_pad(&ctx, fourth_pad_id)
        .expect_err("Linking 4th pad when 3 are active must fail");
    assert_eq!(cap_err.code, "MaxLinkedLandingPadsReached");

    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(
        !notes.iter().any(|n| n.event == "OnUnderConstructionUpdated"),
        "OnUnderConstructionUpdated is client-local and must not be emitted by the server"
    );
    assert!(notes.iter().any(|n| {
        n.event == "OnSlimItemChange"
            && n.args.len() == 2
            && n.args[1].dict_get("component_underConstruction_completed") == Some(&PyRep::Bool(true))
            && matches!(n.args[1].dict_get("component_underConstruction_progress"), Some(PyRep::Dict(d)) if !d.is_empty())
    }));
    assert!(notes
        .iter()
        .any(|n| n.event == "OnShipcasterTargetStateChanged" && n.args.len() == 1));

    // 3. Destroy pad #1 via live combat ball destruction -> unlinks/destroys pad #1 and auto-links pad #4!
    let first_pad_id = kv_i64(&built_pads[0], "itemID");
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, lowsec_systems[0]);
    let _ = ctx.drain_notifications_for_char(char_id);
    let ls0_handle = ctx.systems.get_or_spawn(lowsec_systems[0]);
    let _ = ls0_handle
        .apply_damage(
            first_pad_id,
            eve_destiny::DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 },
        )
        .await
        .expect("apply lethal damage to pad #1");
    let _ = ls0_handle.step_manual(1.0).await.expect("step_manual destruction");
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnShipcasterTargetStateChanged"
            && n.args == vec![PyRep::Integer(first_pad_id)]));

    let caldari_pads = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500001)],
    )
    .await
    .expect("GetFactionLandingPads");
    let PyRep::List(caldari_list) = caldari_pads else {
        panic!("expected list");
    };
    assert_eq!(caldari_list.len(), 3);
    let fourth_after = caldari_list
        .iter()
        .find(|p| kv_i64(p, "itemID") == fourth_pad_id)
        .expect("4th built pad present");
    assert!(kv_bool(fourth_after, "isLinked"), "4th built pad must auto-link when pad #1 is destroyed");

    // 4. Multi-stack partial deposit + batch failedToMove reporting (`RF-04`)
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, lowsec_systems[0]);
    let pos0 = DVec3::new(120_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx, &ls0_handle, char_id, ship_id, 601, pos0).await;
    let mat_tid = *ctx.sde.shipcaster_sde_components().unwrap().under_construction[&75933].input_items.keys().next().unwrap();
    let (c5, s_a, s_b) = ctx.db.with_mut_snapshot(|snap| {
        let mut alloc = || { snap.next_entity_id += 1; snap.next_entity_id };
        let (c5, s_a, s_b) = (alloc(), alloc(), alloc());
        for (id, tid, qty) in [(c5, 75933_u32, 1_i32), (s_a, mat_tid, 30), (s_b, mat_tid, 30)] {
            snap.items.insert(id, InventoryItemRecord { item_id: id, type_id: tid, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: qty, ..Default::default() });
        }
        (c5, s_a, s_b)
    });
    let PyRep::List(l5) = rpc(&ctx, &mut sess, "ship", "LaunchForSelf", &[PyRep::Integer(c5)]).await.expect("launch #5") else { panic!() };
    let pad5_id = kv_i64(&l5[0], "itemID");
    let dep = rpc(&ctx, &mut sess, "beyonce", "CallComponentFromClient", &[PyRep::Integer(pad5_id), PyRep::String("underConstruction".into()), PyRep::String("DepositItemsFromShip".into()), PyRep::Dict(vec![(PyRep::Integer(mat_tid as i64), PyRep::Integer(45)), (PyRep::Integer(75924), PyRep::Integer(10))])]).await.expect("DepositItemsFromShip");
    assert_eq!(dep, PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Dict(vec![(PyRep::Integer(75924), PyRep::Integer(10))])]));
    let rem_total: i32 = ctx.db.with_snapshot(|s| [s_a, s_b].iter().filter_map(|id| s.items.get(id)).map(|i| i.quantity).sum());
    assert_eq!(rem_total, 15, "Multi-stack partial deposit of 45 from 30+30 must leave 15");
}

#[tokio::test]
async fn test_starter_shipcaster_sde_systems_rookie_age_one_time_use_and_timers() {
    // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
    // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    // parity: feat:navigation.starter-shipcaster
    eve_testkit::attest(&[
        "rpc:starterShipcasterTravelMgr.CanCharacterJump",
        "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster",
        "feat:navigation.starter-shipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(30_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. Verify SDE starter shipcasters cover all 60 school starter/career systems + 4 Exordium embassy systems = 64 systems
    let (career_launcher_id, career_launcher_pos) = with_misc_state(&ctx, |st| {
        assert!(st.starter_launchers.len() >= 64, "Expected >= 64 SDE starter/career/embassy launchers");
        for req_sys in [30030141_u32, 30013489, 30014971, 30012547, 30100070, 30100063, 30100077, 30100084] {
            assert!(
                st.starter_launchers.iter().any(|l| l.solar_system_id == req_sys),
                "Missing SDE starter shipcaster in system {req_sys}"
            );
        }
        let caldari_career = st
            .starter_launchers
            .iter()
            .find(|l| l.solar_system_id == 30030141) // State War Academy career system (Jouvulen)
            .expect("Jouvulen starter shipcaster");
        assert_eq!(caldari_career.staging_system_id, 30045324); // Onnamon War HQ
        assert_eq!(caldari_career.staging_station_id, 60015184);
        (caldari_career.item_id, caldari_career.position)
    });

    let now = ctx.now_unix();
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, 30030141);
    let handle_starter = ctx.systems.get_or_spawn(30030141);
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, career_launcher_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    // 2. Veteran character (> 30 days old) rejected by BOTH CanCharacterJump and CmdJumpThroughStarterShipcaster
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().create_date_unix = now - 31 * 86_400;
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    let vet_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(career_launcher_id)],
    )
    .await
    .expect_err("Veteran character rejected");
    assert_eq!(vet_err.code, "CharacterTooOldForStarterShipcaster");

    // Set rookie character age (5 days old <= 30 days)
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().create_date_unix = now - 5 * 86_400;
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(true)
    );

    // 3. Weapons timer & capital ship block BOTH CanCharacterJump and CmdJumpThroughStarterShipcaster
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = now + 60;
    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = 0;

    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 19720);
    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    let cap_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(career_launcher_id)],
    )
    .await
    .expect_err("Capital rejected");
    assert_eq!(cap_err.code, "CapitalShipCannotJumpShipcaster");
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 601);

    // 4. Positive rookie jump -> arrives in Onnamon (30045324), then one-time-use blocks any second jump!
    ctx.advance_secs(15);
    let jump_ok = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(career_launcher_id)],
    )
    .await
    .expect("Rookie jump to Onnamon War HQ");
    assert_eq!(jump_ok, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(30045324));

    assert_eq!(
        rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[]).await.unwrap(),
        PyRep::Bool(false),
        "One-time-use must immediately flip CanCharacterJump to false"
    );
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, 30030141);
    spawn_test_ship(&ctx, &handle_starter, char_id, ship_id, 601, career_launcher_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let reuse_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(career_launcher_id)],
    )
    .await
    .expect_err("Second starter shipcaster jump must fail");
    assert_eq!(reuse_err.code, "StarterShipcasterAlreadyUsed");
}

#[tokio::test]
async fn test_interstellar_shipcaster_war_hq_jump_and_six_minute_cycle_rotation() {
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    // parity: feat:navigation.shipcaster
    eve_testkit::attest(&[
        "rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster",
        "feat:navigation.shipcaster",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(3_600);
    let mut sess = SessionState::new(30_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // Construct 2 linked Caldari beacons in Onatoh (30002811) and Tama (30002813)
    let pad_a = seed_and_build_caldari_beacon(&ctx, &mut sess, char_id, ship_id, 30002811, DVec3::new(100_000.0, 0.0, 0.0)).await;
    let pad_b = seed_and_build_caldari_beacon(&ctx, &mut sess, char_id, ship_id, 30002813, DVec3::new(120_000.0, 0.0, 0.0)).await;
    let pad_a_id = kv_i64(&pad_a, "itemID");
    let pad_b_id = kv_i64(&pad_b, "itemID");

    let (onnamon_launcher_id, onnamon_launcher_pos) = with_misc_state(&ctx, |st| {
        let l = st.launchers.iter().find(|l| l.faction_id == 500_001).unwrap();
        (l.item_id, l.position)
    });

    // Align clock to start of an even 360s cycle (`(now / 360) % 2 == 0` -> `pad_a` active, `pad_b` inactive)
    let rem = ctx.now_unix() % 720;
    if rem != 0 {
        ctx.advance_secs(720 - rem);
    }

    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, 30045324); // Onnamon War HQ
    let handle_hq = ctx.systems.get_or_spawn(30045324);
    spawn_test_ship(&ctx, &handle_hq, char_id, ship_id, 601, onnamon_launcher_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // 1. Inactive linked beacon in current 6-minute cycle is rejected
    let cycle_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(onnamon_launcher_id), PyRep::Integer(30002813), PyRep::Integer(pad_b_id)],
    )
    .await
    .expect_err("Jumping to off-cycle beacon must fail");
    assert_eq!(cycle_err.code, "ShipcasterTargetNotActiveInCycle");

    // 2. Hostile militia rejected
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_002));
    let hostile_err = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(onnamon_launcher_id), PyRep::Integer(30002811), PyRep::Integer(pad_a_id)],
    )
    .await
    .expect_err("Hostile militia rejected");
    assert_eq!(hostile_err.code, "FactionShipcasterAccessDenied");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001));

    // 3. Jump to active cycle beacon (`pad_a` in Onatoh 30002811) succeeds!
    let jump_a = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(onnamon_launcher_id), PyRep::Integer(30002811), PyRep::Integer(pad_a_id)],
    )
    .await
    .expect("Jump to active pad_a");
    assert_eq!(jump_a, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(30002811));

    // 4. Advance 360s (6 minutes) -> active target rotates to `pad_b` in Tama (30002813)!
    ctx.advance_secs(360);
    place_pilot_in_space(&ctx, &mut sess, char_id, ship_id, 30045324);
    spawn_test_ship(&ctx, &handle_hq, char_id, ship_id, 601, onnamon_launcher_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    let jump_b = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(onnamon_launcher_id), PyRep::Integer(30002813), PyRep::Integer(pad_b_id)],
    )
    .await
    .expect("Jump to rotated pad_b after 360s");
    assert_eq!(jump_b, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(30002813));
}
