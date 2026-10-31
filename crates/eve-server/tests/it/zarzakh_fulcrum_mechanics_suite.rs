//! Integration tests for Zarzakh gate tolls, Fulcrum alignment services,
//! Pochven restrictions & wormholes, and Deathless shipcaster mechanics.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{
    check_and_apply_shipcaster_target_cooldown, is_zarzakh_horizon_siege_active,
    set_zarzakh_horizon_siege_active,
};
use crate::common;

#[tokio::test]
async fn test_zarzakh_gate_toll_and_journal() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let turnur_sys = 30002086_u32;
    let turnur_gate_id = 50016562_i64;

    let mut sess = SessionState::new(883_401);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = turnur_sys;
            ch.active_ship_id = ship_id;
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.war_faction_id = None;
            ch.station_id = None;
            ch.structure_id = None;
        }
        s.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 28606, // Orca (heavy industrial)
                owner_id: char_id,
                location_id: turnur_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(turnur_sys);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let t_handle = ctx.systems.get_or_spawn(turnur_sys);
    let gate_pos = ctx.sde.jumps.get(&turnur_gate_id).map(|j| j.position).unwrap_or(glam::DVec3::ZERO);
    let ball = Ball {
        id: ship_id,
        type_id: 28606,
        position: gate_pos + glam::DVec3::new(100.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 60.0,
        inertia: 0.5,
        mass: 150_000_000.0,
        radius: 300.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = t_handle.spawn_ball(ball.clone()).await;
    let turnur_beyonce = ctx.bind_object(&mut sess, "beyonce", turnur_sys as i64).unwrap();

    // 0. Out of range jump: pilot at 50,000m fails with OutsideStargateJumpRange and NO toll debited (R-B7-05)
    let balance_before = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    let mut far_ball = ball.clone();
    far_ball.position = gate_pos + glam::DVec3::new(50_000.0, 0.0, 0.0);
    let _ = t_handle.spawn_ball(far_ball).await;
    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(turnur_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(turnur_gate_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Out of range gate jump fails");
    assert_eq!(range_err.code, "OutsideStargateJumpRange");
    let balance_after_fail = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(balance_after_fail, balance_before, "Wallet balance must be untouched when jump fails range check");
    let _ = t_handle.spawn_ball(ball.clone()).await;

    // 1. Decline toll -> jump aborted
    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirm_toll".to_string(), PyRep::Bool(false));
    let decline_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(turnur_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(turnur_gate_id)],
            &decline_kw,
            None,
        )
        .await
        .expect_err("Declined toll aborts jump");
    assert_eq!(decline_err.code, "ZarzakhGateTollDeclined");

    // 2. Confirm toll -> jump succeeds, debited and journaled
    let mut confirm_kw = HashMap::new();
    confirm_kw.insert("confirm_toll".to_string(), PyRep::Bool(true));
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(turnur_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(turnur_gate_id)],
            &confirm_kw,
            None,
        )
        .await
        .expect("Confirmed toll jump succeeds");
    assert_eq!(jump_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30100000));

    // Verify journal entry recorded and exact toll debited (150M kg Orca -> 1.5M ISK)
    let bal_after = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(bal_after.to_f64_isk(), 48_500_000.0, "Toll of 1,500,000 ISK debited for 150M kg Orca");
    let has_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll")
    });
    assert!(has_journal, "Wallet journal entry recorded for zarzakh_gate_toll");

    // 3. Pirate enlisted pilot pays 0 ISK
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = turnur_sys;
        ch.balance = eve_db::IskCents::from_isk(100);
        ch.war_faction_id = Some(500010); // Angel Cartel
        ch.station_id = None;
        ch.structure_id = None;
    });
    sess.solar_system_id = Some(turnur_sys);
    sess.station_id = None;
    sess.structure_id = None;
    let turnur_beyonce = ctx.bind_object(&sess, "beyonce", turnur_sys as i64).unwrap();
    let _ = t_handle.spawn_ball(ball.clone()).await;
    ctx.advance_secs(11);
    let pirate_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(turnur_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(turnur_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Pirate pilot jumps free of charge");
    assert_eq!(pirate_jump.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30100000));
}

#[tokio::test]
async fn test_fulcrum_docking_and_alignment_services() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let zarzakh_sys = 30100000_u32;
    let fulcrum_station_id = 60_015_187_i64;

    let mut sess = SessionState::new(883_402);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = zarzakh_sys;
            ch.active_ship_id = ship_id;
            ch.war_faction_id = None; // Neutral
            ch.station_id = None;
            ch.structure_id = None;
        }
        s.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: zarzakh_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(zarzakh_sys);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let z_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    let fulcrum_pos = ctx.sde.stations.get(&fulcrum_station_id).map(|s| s.position).unwrap_or(glam::DVec3::ZERO);
    let ship_b = Ball {
        id: ship_id,
        type_id: 621,
        position: fulcrum_pos + glam::DVec3::new(100.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = z_handle.spawn_ball(ship_b).await;
    let fulcrum_beyonce = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();

    // 1. Weapons / Retribution timer refuses docking
    let now = ctx.now_unix();
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(char_id, now + 300); // 5 min retribution timer
    });
    let timer_dock_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(fulcrum_beyonce), "CmdDock", &[PyRep::Integer(fulcrum_station_id), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("Fulcrum refuses docking with active retribution timer");
    assert_eq!(timer_dock_err.code, "FulcrumRefusesTimerActive");

    // Clear retribution timer -> neutral character docks successfully
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.remove(&char_id);
    });
    let dock_rep = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(fulcrum_beyonce), "CmdDock", &[PyRep::Integer(fulcrum_station_id), PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("Neutral pilot docks at Fulcrum");
    assert!(matches!(dock_rep, PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Integer(_)));

    // 2. Neutral pilot has access to standard services: repair, fitting, market, insurance, industry
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_station_id, 17)); // fitting
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_station_id, 21)); // repair
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_station_id, 13)); // market

    // 3. Jump cloning: refused for neutral character
    sess.station_id = Some(fulcrum_station_id);
    let clone_install_err = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await
        .expect_err("Clone install refused for non-pirates at Fulcrum");
    assert_eq!(clone_install_err.code, "FulcrumCloneRequiresPirateEnlistment");

    // Enlist in pirate militia (Angel 500010) and train Infomorph Psychology -> jump clone install allowed
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500010);
        s.skills.entry(char_id).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let clone_install_rep = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await
        .expect("Clone install succeeds for pirate pilot at Fulcrum");
    assert!(matches!(clone_install_rep, PyRep::Integer(_) | PyRep::Dict(_) | PyRep::KeyVal(_)));

    // 4. Corporation office rental: refused for non-pirate corp, allowed for pirate-enlisted corp
    let corp_id = 98_000_101_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Pirate Fleet Corp".into(),
                ticker: "PIRAT".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: eve_db::IskCents::from_isk(100_000_000),
                members: [char_id].into_iter().collect(),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: "Pirates".into(),
                url: "".into(),
                hq_station_id: Some(fulcrum_station_id),
            },
        );
        s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&char_id).unwrap().war_faction_id = None; // Neutral first
    });
    sess.corporation_id = Some(corp_id);
    let office_denied = ctx
        .dispatch_rpc(&mut sess, "officeManager", None, "RentOffice", &[PyRep::Integer(fulcrum_station_id)], &kw, None)
        .await
        .expect_err("Non-pirate corp office rental refused at Fulcrum");
    assert_eq!(office_denied.code, "FulcrumOfficeRequiresPirateEnlistment");

    // Enlist corporation itself in Guristas (500010) -> office rental allowed
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500010);
        s.fw_runtime.corp_war_factions.insert(corp_id, 500010);
    });
    let office_rep = ctx
        .dispatch_rpc(&mut sess, "officeManager", None, "RentOffice", &[PyRep::Integer(fulcrum_station_id)], &kw, None)
        .await
        .expect("Office manager rents office at Fulcrum for pirate corp");
    assert!(matches!(office_rep, PyRep::KeyVal(_) | PyRep::Dict(_)));

    // 5. Authoritative unseeded Angel Cartel (1000138) and Guristas (1000127) LP stores available
    let angel_rep = ctx.dispatch_rpc(
        &mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1000138)],
        &kw, None,
    ).await.expect("Angel Cartel LP store query");
    let PyRep::List(ref a_offers) = angel_rep else { panic!("expected list of offers"); };
    assert!(!a_offers.is_empty(), "Angel Cartel LP store has available offers");
    let a_off = a_offers.iter().find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3838)).expect("Offer 3838");
    assert_eq!(a_off.dict_get("corpID").and_then(|v| v.as_i64()), Some(1000138));
    assert_eq!(a_off.dict_get("lpCost").and_then(|v| v.as_i64()), Some(23_625));
    assert_eq!(a_off.dict_get("typeID").and_then(|v| v.as_i64()), Some(20_498));

    let guristas_rep = ctx.dispatch_rpc(
        &mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1000127)],
        &kw, None,
    ).await.expect("Guristas LP store query");
    let PyRep::List(ref g_offers) = guristas_rep else { panic!("expected list of offers"); };
    assert!(!g_offers.is_empty(), "Guristas LP store has available offers");
    let g_off = g_offers.iter().find(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(3938)).expect("Offer 3938");
    assert_eq!(g_off.dict_get("corpID").and_then(|v| v.as_i64()), Some(1000127));
    assert_eq!(g_off.dict_get("lpCost").and_then(|v| v.as_i64()), Some(23_625));
    assert_eq!(g_off.dict_get("typeID").and_then(|v| v.as_i64()), Some(20_121));
}

#[tokio::test]
async fn test_pochven_mechanics_and_restrictions() {
    // parity: feat:structures.pochven-anchoring-restrictions
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let pochven_sys = 30_000_021_u32; // Krai system in Pochven (region 10000070, Kuharah)

    let mut sess = SessionState::new(883_403);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        s.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: pochven_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(pochven_sys);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    // 1. Region-wide cyno jammer stops cyno in Pochven
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, pochven_sys).is_some());

    // 2. Capital manufacturing in Pochven is refused
    let facility_id = 60002002_i64;
    let bp_item_id = 100_000_299_i64;
    sess.station_id = Some(facility_id);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().station_id = Some(facility_id);
        s.items.insert(
            bp_item_id,
            eve_db::InventoryItemRecord {
                item_id: bp_item_id,
                type_id: 23758, // Archon Blueprint (Capital Carrier)
                owner_id: char_id,
                location_id: facility_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let job_dict = PyRep::keyval([
        ("facilityID", PyRep::Integer(facility_id)),
        ("blueprintID", PyRep::Integer(bp_item_id)),
        ("blueprintTypeID", PyRep::Integer(23758)),
        ("activityID", PyRep::Integer(1)),
        ("runs", PyRep::Integer(1)),
    ]);
    let cap_mfg_err = ctx.dispatch_rpc(
        &mut sess, "industryManager", None, "InstallJob",
        &[job_dict],
        &kw, None,
    ).await.expect_err("Capital manufacturing forbidden in Pochven");
    assert_eq!(cap_mfg_err.code, "CapitalConstructionForbiddenInPochven");
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().station_id = None;
    });

    // 3. Wormhole spawning with guard NPCs
    let static_whs = eve_server::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(&ctx.sde, pochven_sys, 0, ctx.now_unix());
    assert!(!static_whs.is_empty(), "Pochven system has static wormholes");
    let wh_sig = &static_whs[0];
    let wh_item_id = 100_000_209_i64;
    let (_wh_pos, guards) = eve_server::services::space_destiny::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards(
        &ctx, pochven_sys, wh_sig, wh_item_id,
    ).await;
    assert!(!guards.is_empty(), "Pochven wormhole spawns guard NPCs");
    let p_handle = ctx.systems.get_or_spawn(pochven_sys);
    for gid in guards {
        assert!(p_handle.get_combat_state(gid).await.unwrap().is_some(), "Guard NPC has full CombatState");
        let ai_registered = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&gid));
        assert!(ai_registered, "Guard NPC registered with NPC AI runtime");
    }

    // 4. Upwell structure anchoring forbidden in Pochven
    let poch_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832)], &kw, None)
        .await.expect_err("Anchoring Upwell structures disallowed in Pochven");
    assert_eq!(poch_anchor_err.code, "AnchoringDisallowedInPochven");
}

#[tokio::test]
async fn test_zarzakh_bookmark_dscan_and_shipcaster_rules() {
    // parity: feat:navigation.zarzakh-bookmark-dscan-disabled, feat:navigation.zarzakh-capital-gate-ban
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let zarzakh_sys = 30100000_u32;

    let mut sess = SessionState::new(883_404);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await.expect("SelectCharacterID");
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = zarzakh_sys;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        s.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 621,
                owner_id: char_id,
                location_id: zarzakh_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(zarzakh_sys);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    // 1. CmdWarpToStuff with stuff_type == "bookmark" refused in Zarzakh
    let z_handle = ctx.systems.get_or_spawn(zarzakh_sys);
    let s_ball = Ball {
        id: ship_id,
        type_id: 621,
        position: DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = z_handle.spawn_ball(s_ball).await;
    let z_beyonce = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();

    let create_bm_err = ctx.dispatch_rpc(
        &mut sess, "bookmark", None, "BookmarkLocation",
        &[PyRep::Integer(zarzakh_sys as i64), PyRep::None, PyRep::None, PyRep::String("bm".into())],
        &kw, None,
    ).await.expect_err("Bookmark creation refused in Zarzakh");
    assert_eq!(create_bm_err.code, "CantbookmarkEveSystem");

    // RF-06: Scan-result bookmarking and ACL shared bookmarking also refused in Zarzakh
    let scan_bm_err = ctx.dispatch_rpc(
        &mut sess, "scanMgr", None, "BookmarkScanResult",
        &[PyRep::String("ABC-123".into()), PyRep::String("sig".into())], &kw, None,
    ).await.expect_err("BookmarkScanResult refused in Zarzakh");
    assert_eq!(scan_bm_err.code, "CantbookmarkEveSystem");

    let acl_bm_err = ctx.dispatch_rpc(
        &mut sess, "accessGroupBookmarkMgr", None, "BookmarkStaticLocation",
        &[PyRep::String("Spot".into()), PyRep::String("memo".into())], &kw, None,
    ).await.expect_err("ACL BookmarkStaticLocation refused in Zarzakh");
    assert_eq!(acl_bm_err.code, "CantbookmarkEveSystem");

    let warp_bm_err = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(z_beyonce), "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(9999), PyRep::Integer(0)],
        &kw, None,
    ).await.expect_err("Bookmark warp refused in Zarzakh");
    assert_eq!(warp_bm_err.code, "DunBlacklistCannotWarp");

    // RF-06: FleetWarp and CmdAlignTo bookmark also refused in Zarzakh
    ctx.dispatch_rpc(&mut sess, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
    ctx.db.with_mut_snapshot(|s| {
        s.bookmarks.insert(9999, eve_db::BookmarkRecord {
            bookmark_id: 9999, owner_id: char_id, label: "z".into(), solar_system_id: zarzakh_sys,
            x: 500_000.0, y: 0.0, z: 0.0, item_id: Some(ship_id), memo: "z".into(), folder_id: None, is_shared: false,
        });
    });
    let fw_bm_err = ctx.dispatch_rpc(
        &mut sess, "fleetMgr", None, "WarpFleet", &[PyRep::Integer(9999), PyRep::Real(0.0)], &kw, None,
    ).await.expect_err("FleetWarp to bookmark refused in Zarzakh");
    assert_eq!(fw_bm_err.code, "DunBlacklistCannotWarp");

    let mut align_kw = HashMap::new();
    align_kw.insert("bookmarkID".into(), PyRep::Integer(9999));
    let align_bm_err = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(z_beyonce), "CmdAlignTo", &[], &align_kw, None,
    ).await.expect_err("CmdAlignTo bookmark refused in Zarzakh");
    assert_eq!(align_bm_err.code, "DunBlacklistCannotWarp");

    let goto_bm_err = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(z_beyonce), "CmdGotoBookmark", &[PyRep::Integer(9999)], &kw, None,
    ).await.expect_err("CmdGotoBookmark refused in Zarzakh");
    assert_eq!(goto_bm_err.code, "DunBlacklistCannotWarp");

    // 2. D-scan disabled in Zarzakh, restored on leaving
    let dscan_err = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "DirectionalScan", &[PyRep::Real(360.0)], &kw, None)
        .await.expect_err("DirectionalScan disabled in Zarzakh");
    assert_eq!(dscan_err.code, "CannotDscanInThisSystem");

    // Move pilot to Jita (30000142) -> DScan and bookmark creation restored
    sess.solar_system_id = Some(30000142);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().solar_system_id = 30000142);
    let dscan_ok = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "DirectionalScan", &[PyRep::Real(360.0)], &kw, None)
        .await.expect("DirectionalScan restored outside Zarzakh");
    assert!(matches!(dscan_ok, PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Dict(_)));

    let create_bm_ok = ctx.dispatch_rpc(
        &mut sess, "bookmark", None, "BookmarkLocation",
        &[PyRep::Integer(30000142), PyRep::None, PyRep::None, PyRep::String("jita_bm".into())],
        &kw, None,
    ).await.expect("Bookmark creation restored outside Zarzakh");
    assert!(matches!(create_bm_ok, PyRep::Integer(_) | PyRep::KeyVal(_)));

    // 3. Horizon Siege state gating & per-target cooldown
    assert!(!is_zarzakh_horizon_siege_active(&ctx));
    set_zarzakh_horizon_siege_active(&ctx, true);
    assert!(is_zarzakh_horizon_siege_active(&ctx));
    set_zarzakh_horizon_siege_active(&ctx, false);

    // Test per-target cooldown check
    let target_id = 104_000_020_i64;
    let applied = check_and_apply_shipcaster_target_cooldown(&ctx, target_id, 120);
    assert!(applied.is_ok(), "Cooldown successfully applied to shipcaster target");
    let applied_again = check_and_apply_shipcaster_target_cooldown(&ctx, target_id, 120);
    assert!(applied_again.is_err(), "Target currently on cooldown refused");

    // 4. RF-07: Jumping out of Zarzakh via Deathless Shipcaster clears Zarzakh gate lock and emits DeactivatedNotice
    ctx.advance_secs(130);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = 30002086; ch.war_faction_id = Some(500_010); ch.station_id = None;
    });
    sess.solar_system_id = Some(30002086);
    let t_handle = ctx.systems.get_or_spawn(30002086);
    let t_gpos = ctx.sde.jumps.get(&50016562).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let _ = t_handle.spawn_ball(Ball { id: ship_id, type_id: 621, position: t_gpos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());
    let tb = ctx.bind_object(&mut sess, "beyonce", 30002086).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(tb), "CmdStargateJump", &[PyRep::Integer(50016562)], &kw, None).await.expect("Enter Zarzakh");
    let _ = ctx.quasar_gateway.poll_notices(sess.session_id);
    ctx.advance_secs(15);
    let _ = z_handle.spawn_ball(Ball { id: ship_id, type_id: 621, position: DVec3::new(250_100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 250.0, inertia: 0.5, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    ctx.dispatch_rpc(&mut sess, "shipcasterTravelMgr", None, "CmdJumpThroughShipcaster", &[PyRep::Integer(105_000_010), PyRep::Integer(30002056), PyRep::Integer(104_000_020)], &kw, None).await.expect("Shipcaster jump out of Zarzakh");
    let sc_notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(sc_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("stargate.lock.api.DeactivatedNotice"))), "Shipcaster jump out of Zarzakh emits DeactivatedNotice");
    assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)), "Shipcaster jump clears Zarzakh gate lock");
}
