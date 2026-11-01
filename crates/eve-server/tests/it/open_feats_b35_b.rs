//! Dedicated integration tests for open `feat:modules.*` and `feat:navigation.*` parity rows (Batch 35, Part 2).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::open_feats_b35::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_b35_06_jump_portal_bridge_rules() {
    // parity: feat:modules.jump-portal-bridge-rules
    eve_testkit::attest(&["feat:modules.jump-portal-bridge-rules"]);
    let ctx = build_ctx();
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2): unauthenticated bridge & jump-through rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(1)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(1), PyRep::Integer(1)], &kw, None).await.is_err());

    // Light Normal Cyno (21096), Covert Cyno (28646), and Industrial Cyno (52694) in DEST_NULL_SYS
    let cyno_char = 952_301;
    let cyno_ship = 962_301;
    put_in_fleet(&ctx, 880_001, cyno_char, &[(cyno_char, true), (952_311, true), (952_312, true)]);
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_301, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_302, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_norm = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_301), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Normal cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let cov_cyno_char = 952_311;
    let cov_cyno_ship = 962_311;
    let mut cov_cyno_sess = seed_pilot(&ctx, cov_cyno_char, cov_cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cov_cyno_char, cov_cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(20_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_311, 28646, cov_cyno_char, cov_cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_312, 16273, cov_cyno_char, cov_cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_cov = ctx.dispatch_rpc(&mut cov_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_311), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Covert cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let ind_cyno_char = 952_312;
    let ind_cyno_ship = 962_312;
    let mut ind_cyno_sess = seed_pilot(&ctx, ind_cyno_char, ind_cyno_ship, 32880, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, ind_cyno_char, ind_cyno_ship, 32880, DEST_NULL_SYS, DVec3::new(30_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_313, 52694, ind_cyno_char, ind_cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_314, 16273, ind_cyno_char, ind_cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_ind = ctx.dispatch_rpc(&mut ind_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_313), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Indy cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // 1. Titan (Avatar 11567): normal cyno only, 60s, 500 Strontium base + JPG V (24562) -> 250 Strontium, immobilized, bubbled host allowed
    let titan_char = 952_302;
    let titan_ship = 962_302;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    let titan_beyonce = ctx.bind_object(&mut titan_sess, "beyonce", NULL_SYS as i64).unwrap();
    give_item(&ctx, 972_303, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
    give_item(&ctx, 972_304, 16275, titan_char, titan_ship, InventoryFlag::Cargo, 1_000); // Strontium
    // Put partial topes in Fuel Bay (10) and rest in Cargo (50,000) to verify Fuel Bay -> Cargo fallback!
    give_item(&ctx, 972_305, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 10);
    give_item(&ctx, 972_315, 16274, titan_char, titan_ship, InventoryFlag::Cargo, 50_000);

    let j_ok_char = 952_303;
    let j_ok_ship = 962_303;
    let mut j_ok_sess = seed_pilot(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, false); // Maller subcap
    spawn_ship_in_space(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let j_cap_char = 952_313;
    let j_cap_ship = 962_313;
    let mut j_cap_sess = seed_pilot(&ctx, j_cap_char, j_cap_ship, 23757, NULL_SYS, false); // Archon capital (must be rejected!)
    spawn_ship_in_space(&ctx, j_cap_char, j_cap_ship, 23757, NULL_SYS, DVec3::new(1_200.0, 0.0, 0.0)).await;

    let j_far_char = 952_304;
    let j_far_ship = 962_304;
    let mut j_far_sess = seed_pilot(&ctx, j_far_char, j_far_ship, 624, NULL_SYS, false);
    spawn_ship_in_space(&ctx, j_far_char, j_far_ship, 624, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;

    let blops_char = 952_306;
    let blops_ship = 962_306;
    let cov_jumper = 952_307;
    let cov_ship = 962_307;
    let noncov_jumper = 952_308;
    let noncov_ship = 962_308;
    let rorq_char = 952_320;
    let rorq_ship = 962_320;
    let ven_j = 952_321;
    let porp_j = 952_322;
    let orca_j = 952_323;

    put_in_fleet(&ctx, 880_002, titan_char, &[
        (cyno_char, true), (cov_cyno_char, true), (ind_cyno_char, true), (titan_char, true),
        (j_ok_char, true), (j_cap_char, true), (j_far_char, true),
        (blops_char, true), (cov_jumper, true), (noncov_jumper, true),
        (rorq_char, true), (ven_j, true), (porp_j, true), (orca_j, true),
    ]);

    // Titan rejected targeting Covert Cyno or Industrial Cyno
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cov_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_cov)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ind_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_ind)], &kw, None).await.is_err());

    // Host in bubble can open bridge; JPG 0 -> 500 Strontium, 60s, Titan immobilized
    set_skill(&ctx, titan_char, 24562, 0);
    let _ = origin_sys.add_warp_bubble(999_001, DVec3::ZERO, 20_000.0).await;
    let bridge_res = ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_norm)], &kw, None).await.expect("Titan open bridge");
    assert_eq!(bridge_res.dict_get("durationSeconds"), Some(&PyRep::Real(60.0)));
    assert_eq!(bridge_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(500)));
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", Some(titan_beyonce), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.is_err(), "Titan must be immobilized while portal is open");

    // >2,500m jumper & bubbled jumper rejected
    assert!(ctx.dispatch_rpc(&mut j_far_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err());

    let _ = origin_sys.remove_warp_bubble(999_001).await;

    // Capital ship rejected from Titan bridge
    assert!(ctx.dispatch_rpc(&mut j_cap_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err(), "Capital ship cannot take Titan bridge");

    // Tethered jumper rejected (CannotJumpWhileTethered) & <0 WCS jumper rejected!
    let mut cs_teth = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
    cs_teth.is_tethered = true;
    origin_sys.set_combat_state(j_ok_ship, cs_teth).await.unwrap();
    let teth_err = ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.unwrap_err();
    assert_eq!(teth_err.code, "CannotJumpWhileTethered");

    let mut cs_wcs = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
    cs_wcs.is_tethered = false;
    cs_wcs.warp_core_strength = -1;
    origin_sys.set_combat_state(j_ok_ship, cs_wcs).await.unwrap();
    assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &kw, None).await.is_err(), "Ship below 0 WCS cannot take bridge");

    let mut cs_clean = origin_sys.get_combat_state(j_ok_ship).await.unwrap().unwrap();
    cs_clean.warp_core_strength = 0;
    origin_sys.set_combat_state(j_ok_ship, cs_clean).await.unwrap();

    // Subcap jumps (4-arg client call) -> consumes all 10 topes from Fuel Bay first, then remainder from Cargo!
    assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship), PyRep::Integer(beacon_norm + 999), PyRep::Integer(DEST_NULL_SYS as i64)], &kw, None).await.is_err(), "Mismatched beaconID must fail");
    let jump_ok = ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship), PyRep::Integer(beacon_norm), PyRep::Integer(DEST_NULL_SYS as i64)], &kw, None).await.expect("JumpThroughFleet");
    let fuel_consumed = jump_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
    assert!(fuel_consumed > 10);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&972_305)), "Fuel bay stack must be exhausted first");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_315).unwrap().quantity), 50_000 - (fuel_consumed as i32 - 10), "Remainder must come from bridger cargo");

    // Verify Jump Portal Generation V (24562) reduces Titan Strontium cost by 50% (500 -> 250) after 60s portal closes
    ctx.advance_time_seconds(65);
    set_skill(&ctx, titan_char, 24562, 5);
    ctx.invalidate_ship_fitting(titan_ship);
    let bridge_jpg5 = ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_norm)], &kw, None).await.expect("Titan bridge JPG V");
    assert_eq!(bridge_jpg5.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(250)), "JPG V must halve Strontium cost to 250");

    // 2. Black Ops (Redeemer 22428) bridge: 20s, 0 Strontium, covert-cloak hulls only, bridger JFC V halves passenger fuel
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, blops_char, 21610, 5); // JFC V on bridger
    give_item(&ctx, 972_306, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_307, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 100_000);

    let mut cov_sess = seed_pilot(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, false); // Helios
    spawn_ship_in_space(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let mut noncov_sess = seed_pilot(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, false); // Maller
    spawn_ship_in_space(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let beacon_cov = ctx.dispatch_rpc(&mut cov_cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_311), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Re-light covert cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    let blops_b_res = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cov_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_cov)], &kw, None).await.expect("Blops open bridge");
    assert_eq!(blops_b_res.dict_get("durationSeconds"), Some(&PyRep::Real(20.0)));
    assert_eq!(blops_b_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(0)));
    assert!(ctx.dispatch_rpc(&mut noncov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut cov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &kw, None).await.is_ok());

    // 3. Rorqual (28352 + Industrial Jump Portal Generator I 63140): 20s, 0 Strontium, mining hulls (Venture, Porpoise) accepted, Orca & Maller rejected
    let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
    spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_320, 63140, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_321, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 100_000);
    let rorq_b_res = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ind_cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_ind)], &kw, None).await.expect("Rorqual open bridge");
    assert_eq!(rorq_b_res.dict_get("durationSeconds"), Some(&PyRep::Real(20.0)));
    assert_eq!(rorq_b_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(0)));

    let mut ven_sess = seed_pilot(&ctx, ven_j, 962_321, 32880, NULL_SYS, false);
    spawn_ship_in_space(&ctx, ven_j, 962_321, 32880, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let mut porp_sess = seed_pilot(&ctx, porp_j, 962_322, 42244, NULL_SYS, false);
    spawn_ship_in_space(&ctx, porp_j, 962_322, 42244, NULL_SYS, DVec3::new(1_200.0, 0.0, 0.0)).await;
    let mut orca_sess = seed_pilot(&ctx, orca_j, 962_323, 28606, NULL_SYS, false);
    spawn_ship_in_space(&ctx, orca_j, 962_323, 28606, NULL_SYS, DVec3::new(1_400.0, 0.0, 0.0)).await;

    assert!(ctx.dispatch_rpc(&mut orca_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_err(), "Orca must be rejected by Rorqual portal");
    assert!(ctx.dispatch_rpc(&mut noncov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_err(), "Combat cruiser must be rejected by Rorqual portal");
    // BF-B35-01: Active session-change timer on passenger must fail before deducting host fuel
    let rorq_fuel_before_sc = ctx.db.with_snapshot(|s| s.items.get(&972_321).unwrap().quantity);
    eve_server::services::space_destiny::record_session_change_timer(&ctx, ven_j, ctx.now_unix_secs(), NULL_SYS, 1, "jump");
    assert_eq!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.unwrap_err().code, "SessionChangeTimerActive");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_321).unwrap().quantity), rorq_fuel_before_sc, "SessionChangeTimerActive must not deduct host portal fuel");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, ven_j);
    assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.is_ok(), "Venture must be accepted by Rorqual portal");
    // BF-B35-02: Destroying or removing the bridging ship ball invalidates the bridge for remaining passengers
    origin_sys.try_remove_ball(rorq_ship);
    assert_eq!(ctx.dispatch_rpc(&mut porp_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(rorq_char), PyRep::Integer(rorq_ship)], &kw, None).await.unwrap_err().code, "NoActiveBridge");
}

#[tokio::test]
async fn test_b35_07_conduit_jump() {
    // parity: feat:navigation.conduit-jump
    eve_testkit::attest(&["feat:navigation.conduit-jump"]);
    let ctx = build_ctx();
    let kw = HashMap::new();

    // Negative/abuse case 1: unauthenticated CmdGroupJumpToFleetModuleBeacon rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(1)], &kw, None).await.is_err());

    // Normal cyno (for Carrier / Command Carrier / Rorqual / Blops) in DEST_NULL_SYS
    let cyno_char = 952_401;
    let cyno_ship = 962_401;
    put_in_fleet(&ctx, 880_003, cyno_char, &[(cyno_char, true)]);
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_401, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_402, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_401), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Normal cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // Negative/abuse case 2: Titan refused
    let titan_char = 952_402;
    let titan_ship = 962_402;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    put_in_fleet(&ctx, 880_003, titan_char, &[(cyno_char, true), (titan_char, true)]);
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());

    // 1. Black Ops (Redeemer 22428): flat 1000 * dist_ly fuel (attr 3131, NOT 700!), cap checked & deducted,
    // covert hulls <= 10km jumped, non-covert skipped, >10km skipped, opted-out (accepts_conduit_jumps=false) skipped!
    let blops_char = 952_403;
    let blops_ship = 962_403;
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, blops_char, 3456, 0); // JDO 0 -> exact 95% base jumpDriveCapacitorNeed
    give_item(&ctx, 972_403, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let recon_ok = 952_404; // Falcon <= 10km, opted in -> JUMPS
    let inelig = 952_405;   // Deacon with cloak in cargo -> SKIPPED
    let recon_far = 952_410; // Falcon at 15km (> 10km) -> SKIPPED
    let recon_optout = 952_411; // Falcon at 2km, accepts_conduit_jumps = false -> SKIPPED
    let recon_wpn = 952_412; // Falcon at 2km with active Weapons Timer -> SKIPPED
    let recon_cd = 952_413;  // Falcon at 2km with active red jump cooldown -> SKIPPED
    for &(cid, sid, tid, pos) in &[
        (recon_ok, 962_404, 11957u32, DVec3::new(2_000.0, 0.0, 0.0)),
        (inelig, 962_405, 37453, DVec3::new(2_000.0, 0.0, 0.0)),
        (recon_far, 962_410, 11957, DVec3::new(15_000.0, 0.0, 0.0)),
        (recon_optout, 962_411, 11957, DVec3::new(2_500.0, 0.0, 0.0)),
        (recon_wpn, 962_412, 11957, DVec3::new(2_000.0, 0.0, 0.0)),
        (recon_cd, 962_413, 11957, DVec3::new(2_000.0, 0.0, 0.0)),
    ] {
        seed_pilot(&ctx, cid, sid, tid, NULL_SYS, false);
        spawn_ship_in_space(&ctx, cid, sid, tid, NULL_SYS, pos).await;
    }
    give_item(&ctx, 972_409, 11578, inelig, 962_405, InventoryFlag::Cargo, 1);
    let now_u = ctx.now_unix_secs();
    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(recon_wpn).weapon_timer_until_unix = now_u + 60;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&recon_cd).unwrap().jump_cooldown_until_unix = now_u + 600; });

    put_in_fleet(&ctx, 880_004, blops_char, &[
        (cyno_char, true), (blops_char, true), (recon_ok, true),
        (inelig, true), (recon_far, true), (recon_optout, false),
        (recon_wpn, true), (recon_cd, true),
    ]);
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
    let blops_max_cap = origin_sys.get_combat_state(blops_ship).await.unwrap().unwrap().max_capacitor;
    let blops_res = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Blops conduit jump");
    let dist_ly = blops_res.dict_get("distanceLY").and_then(|v| v.as_f64()).unwrap();
    let blops_fuel = blops_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(blops_fuel, (1_000.0 * dist_ly).ceil() as i64, "Black Ops conduit jump must charge flat 1000 * LY isotopes (attr 3131)");
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let blops_cap_after = dest_sys.get_combat_state(blops_ship).await.unwrap().unwrap().capacitor;
    assert!(blops_cap_after < blops_max_cap * 0.10, "Conduit jump must deduct 95% leader capacitor");

    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_ok).unwrap().solar_system_id), DEST_NULL_SYS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&inelig).unwrap().solar_system_id), NULL_SYS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_far).unwrap().solar_system_id), NULL_SYS, ">10km member must not be conduit jumped");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_optout).unwrap().solar_system_id), NULL_SYS, "Opted-out member must be skipped");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_wpn).unwrap().solar_system_id), NULL_SYS, "Passenger with active Weapons Timer must be excluded from conduit jump");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&recon_cd).unwrap().solar_system_id), NULL_SYS, "Passenger with active jump cooldown must be excluded from conduit jump");
    let recon_fat = ctx.db.with_snapshot(|s| s.characters.get(&recon_ok).unwrap().jump_fatigue_minutes);
    assert!((recon_fat - 10.0 * (1.0 + dist_ly * 1.0)).abs() < 0.05, "Falcon passenger (1.0x fatigue multiplier) must receive 1.0x distance fatigue ({recon_fat})");

    // 2. Rorqual (28352): blocked while Industrial Core active (preserving fuel & cap); when inactive, flat 5000 * LY,
    // jumps Venture (32880), Procurer (17480), Porpoise (42244), and EXCLUDES Orca (28606)!
    let rorq_char = 952_406;
    let rorq_ship = 962_406;
    let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
    spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_406, 20280, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_407, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(972_406); });

    let m_ven = 952_420;
    let m_proc = 952_421;
    let m_porp = 952_422;
    let m_orca = 952_423;
    for &(cid, sid, tid) in &[(m_ven, 962_420, 32880u32), (m_proc, 962_421, 17480), (m_porp, 962_422, 42244), (m_orca, 962_423, 28606)] {
        seed_pilot(&ctx, cid, sid, tid, NULL_SYS, false);
        spawn_ship_in_space(&ctx, cid, sid, tid, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
    }
    put_in_fleet(&ctx, 880_040, rorq_char, &[(cyno_char, true), (rorq_char, true), (m_ven, true), (m_proc, true), (m_porp, true), (m_orca, true)]);

    assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&972_407).unwrap().quantity), 50_000, "Refused Rorqual conduit jump must NOT consume fuel");

    ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&972_406); });
    let rorq_ok = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Rorqual conduit jump");
    assert_eq!(rorq_ok.dict_get("fuelConsumed"), Some(&PyRep::Integer((5_000.0 * dist_ly).ceil() as i64)), "Rorqual conduit jump must charge flat 5000 * LY isotopes");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_ven).unwrap().solar_system_id), DEST_NULL_SYS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_proc).unwrap().solar_system_id), DEST_NULL_SYS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_porp).unwrap().solar_system_id), DEST_NULL_SYS, "Porpoise must conduit jump with Rorqual");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&m_orca).unwrap().solar_system_id), NULL_SYS, "Orca must NOT conduit jump with Rorqual");
    let porp_fat = ctx.db.with_snapshot(|s| s.characters.get(&m_porp).unwrap().jump_fatigue_minutes);
    assert!((porp_fat - 10.0 * (1.0 + dist_ly * 0.10)).abs() < 0.05, "Porpoise passenger (90% jump fatigue reduction, attr 1971=0.1) must receive 0.10x distance fatigue ({porp_fat})");

    // 3. Carrier (Archon 23757, CJPG 0 -> cap 5) vs Command Carrier (Salvation 92822, CJPG 0 -> double cap = 10)
    let car_char = 952_430;
    let car_ship = 962_430;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, car_char, 21611, 5); // JDC V
    set_skill(&ctx, car_char, 83094, 0); // CJPG 0 -> max 5 passengers
    give_item(&ctx, 972_430, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let mut car_fleet = vec![(cyno_char, true), (car_char, true)];
    for i in 0..7i64 {
        let pc = 952_440 + i;
        let ps = 962_440 + i;
        // Mix eligible subcap hulls: shuttle (672), capsule (670), corvette (596), frigate (587), destroyer (16240), cruiser (624), battleship (641)
        let ptid = [672u32, 670, 596, 587, 16240, 624, 641][i as usize];
        seed_pilot(&ctx, pc, ps, ptid, NULL_SYS, false);
        spawn_ship_in_space(&ctx, pc, ps, ptid, NULL_SYS, DVec3::new(1_500.0, 0.0, 0.0)).await;
        car_fleet.push((pc, true));
    }
    put_in_fleet(&ctx, 880_041, car_char, &car_fleet);
    let car_res = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Carrier conduit jump");
    assert_eq!(car_res.dict_get("fuelConsumed"), Some(&PyRep::Integer((3_000.0 * dist_ly).ceil() as i64)), "Carrier conduit must charge flat 3000 * LY isotopes");
    let jumped_car_cnt = (0..7i64).filter(|&i| ctx.db.with_snapshot(|s| s.characters.get(&(952_440 + i)).unwrap().solar_system_id == DEST_NULL_SYS)).count();
    assert_eq!(jumped_car_cnt, 5, "T1 Carrier at CJPG 0 must cap conduit passengers at 5 (got {jumped_car_cnt})");

    let cmd_char = 952_460;
    let cmd_ship = 962_460;
    let mut cmd_sess = seed_pilot(&ctx, cmd_char, cmd_ship, 92822, NULL_SYS, false); // Salvation (group 5120)
    spawn_ship_in_space(&ctx, cmd_char, cmd_ship, 92822, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, cmd_char, 21611, 5);
    set_skill(&ctx, cmd_char, 83094, 0); // CJPG 0 -> double = 10 passengers!
    give_item(&ctx, 972_460, 16274, cmd_char, cmd_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    let mut cmd_fleet = vec![(cyno_char, true), (cmd_char, true)];
    for i in 0..12i64 {
        let pc = 952_470 + i;
        let ps = 962_470 + i;
        seed_pilot(&ctx, pc, ps, 587, NULL_SYS, false);
        spawn_ship_in_space(&ctx, pc, ps, 587, NULL_SYS, DVec3::new(1_500.0, 0.0, 0.0)).await;
        cmd_fleet.push((pc, true));
    }
    put_in_fleet(&ctx, 880_042, cmd_char, &cmd_fleet);
    ctx.dispatch_rpc(&mut cmd_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Command Carrier conduit jump");
    let jumped_cmd_cnt = (0..12i64).filter(|&i| ctx.db.with_snapshot(|s| s.characters.get(&(952_470 + i)).unwrap().solar_system_id == DEST_NULL_SYS)).count();
    assert_eq!(jumped_cmd_cnt, 10, "Command Carrier at CJPG 0 must double passenger cap to 10 (got {jumped_cmd_cnt})");
}

#[tokio::test]
async fn test_b35_08_jump_arrival_placement() {
    // parity: feat:navigation.jump-arrival-placement
    eve_testkit::attest(&["feat:navigation.jump-arrival-placement"]);
    let ctx = build_ctx();
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let kw = HashMap::new();

    // Cyno ship in DEST_NULL_SYS
    let cyno_char = 952_501;
    let cyno_ship = 962_501;
    put_in_fleet(&ctx, 880_005, cyno_char, &[(cyno_char, true)]);
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    let cyno_pos = DVec3::new(100_000.0, 0.0, 0.0);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, cyno_pos).await;
    give_item(&ctx, 972_501, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_502, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_501), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    let beacon_pos = dest_sys.get_ball_snapshot(beacon_id).await.unwrap().unwrap().1.position;

    // 1. 3D 5 km sphere placement across multiple jumps (verifies non-zero 3D vertical Y dispersion & 5km radius)
    let mut fleet_members = vec![(cyno_char, true)];
    for i in 0..4i64 {
        fleet_members.push((952_510 + i, true));
    }
    put_in_fleet(&ctx, 880_005, cyno_char, &fleet_members);
    let mut max_abs_y = 0.0_f64;
    for i in 0..4i64 {
        let jc = 952_510 + i;
        let js = 962_510 + i;
        let mut jsess = seed_pilot(&ctx, jc, js, 23757, NULL_SYS, false);
        spawn_ship_in_space(&ctx, jc, js, 23757, NULL_SYS, DVec3::new(i as f64 * 500.0, 0.0, 0.0)).await;
        give_item(&ctx, 972_510 + i, 16274, jc, js, InventoryFlag::SpecializedFuelBay, 50_000);
        ctx.dispatch_rpc(&mut jsess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump drive");
        let b = dest_sys.get_ball_snapshot(js).await.unwrap().unwrap().1;
        let dist = b.position.distance(beacon_pos);
        assert!((dist - 5_000.0).abs() < 50.0, "Jump arrival must materialise at 5km sphere around beacon (got {dist}m)");
        max_abs_y = max_abs_y.max((b.position.y - beacon_pos.y).abs());
    }
    assert!(max_abs_y > 50.0, "Jump arrivals must sample a true 3D sphere with vertical Y dispersion (max |dy| = {max_abs_y})");

    // 2. Structure-bounding-sphere overlap ejection with outward velocity:
    // Place an online Keepstar (35834, large bounding sphere) centered at beacon_pos so any 5km arrival overlaps it!
    let keepstar_id = 982_501;
    let s_radius = ctx.sde.types.get(&35834).map(|t| t.radius).unwrap_or(15_000.0).max(10_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            keepstar_id,
            UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: 35834,
                owner_corp_id: 1000009,
                solar_system_id: DEST_NULL_SYS,
                name: "Overlap Keepstar".into(),
                position: beacon_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let ej_char = 952_520;
    let ej_ship = 962_520;
    let mut ej_sess = seed_pilot(&ctx, ej_char, ej_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, ej_char, ej_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_520, 16274, ej_char, ej_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    put_in_fleet(&ctx, 880_055, cyno_char, &[(cyno_char, true), (ej_char, true)]);
    ctx.dispatch_rpc(&mut ej_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump into overlapping structure");
    let ej_ball = dest_sys.get_ball_snapshot(ej_ship).await.unwrap().unwrap().1;
    assert!(ej_ball.position.distance(beacon_pos) > s_radius, "Ship overlapping structure must be ejected outside structure bounding sphere");
    assert!(ej_ball.velocity.length() >= 200.0 && ej_ball.velocity.dot(ej_ball.position - beacon_pos) > 0.0, "Ejected ship must receive outward velocity");
    ctx.db.with_mut_snapshot(|s| { s.structures.remove(&keepstar_id); });

    // 3. Destroyed-beacon fallback lands outside destination star's photosphere on BOTH Jump Drive AND Bridge!
    let star_radius = ctx.sde.celestials_in_system(DEST_NULL_SYS).into_iter().find(|c| c.group_id == 6).map(|c| c.radius).unwrap_or(65_000_000.0);
    let cyno2_char = 952_503;
    let cyno2_ship = 962_503;
    let mut cyno2_sess = seed_pilot(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, false);
    ctx.db.with_mut_snapshot(|s| { if let Some(fl) = s.fleets.values_mut().next() { fl.members.push(cyno2_char); } });
    spawn_ship_in_space(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_504, 21096, cyno2_char, cyno2_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_505, 16273, cyno2_char, cyno2_ship, InventoryFlag::Cargo, 2_000);
    let beacon2_id = ctx.dispatch_rpc(&mut cyno2_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_504), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno 2").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let titan_c = 952_530;
    let titan_s = 962_530;
    let mut titan_sess = seed_pilot(&ctx, titan_c, titan_s, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_c, titan_s, 11567, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_530, 23953, titan_c, titan_s, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_531, 16275, titan_c, titan_s, InventoryFlag::Cargo, 500);
    give_item(&ctx, 972_532, 16274, titan_c, titan_s, InventoryFlag::SpecializedFuelBay, 50_000);

    let jump2_char = 952_504;
    let jump2_ship = 962_504;
    let mut jump2_sess = seed_pilot(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_506, 16274, jump2_char, jump2_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let bridge_p_char = 952_535;
    let bridge_p_ship = 962_535;
    let mut bridge_p_sess = seed_pilot(&ctx, bridge_p_char, bridge_p_ship, 624, NULL_SYS, false);
    spawn_ship_in_space(&ctx, bridge_p_char, bridge_p_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

    put_in_fleet(&ctx, 880_006, cyno2_char, &[(cyno2_char, true), (titan_c, true), (jump2_char, true), (bridge_p_char, true)]);
    ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno2_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon2_id)], &kw, None).await.expect("Open bridge before cyno dies");

    // Destroy cyno ship via real combat death pipeline
    let _ = eve_server::combat::death_pipeline::execute_death_pipeline(&ctx, DEST_NULL_SYS, cyno2_char, cyno2_ship, 999_999, DVec3::ZERO, ctx.now_unix_secs()).await.expect("Destroy cyno ship");

    ctx.dispatch_rpc(&mut jump2_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno2_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon2_id)], &kw, None).await.expect("Jump to destroyed cyno");
    ctx.dispatch_rpc(&mut bridge_p_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_c), PyRep::Integer(titan_s)], &kw, None).await.expect("Bridge to destroyed cyno");
    let fb_jump = dest_sys.get_ball_snapshot(jump2_ship).await.unwrap().unwrap().1;
    let fb_bridge = dest_sys.get_ball_snapshot(bridge_p_ship).await.unwrap().unwrap().1;
    assert!(fb_jump.position.length() > star_radius && fb_jump.position.length() < star_radius + 50_000.0, "Jump to destroyed cyno must land outside star photosphere (got r={}, star={star_radius})", fb_jump.position.length());
    assert!(fb_bridge.position.length() > star_radius && fb_bridge.position.length() < star_radius + 50_000.0, "Bridge to destroyed cyno must also land outside star photosphere");
}

#[tokio::test]
async fn test_b35_09_jump_drive_range_fuel() {
    // parity: feat:navigation.jump-drive-range-fuel
    eve_testkit::attest(&["feat:navigation.jump-drive-range-fuel"]);
    let ctx = build_ctx();
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // 1. V24 Hull-Class Base Range Matrix + JDC (+20%/level) + all 4 Faction Isotopes!
    for &(hull_tid, expected_base_ly, expected_iso) in &[
        (19720u32, 3.5_f64, 16274u32), // Dreadnought (Revelation) -> 3.5 ly, Helium
        (23757, 3.5, 16274),           // Carrier (Archon) -> 3.5 ly, Helium
        (37604, 3.5, 16274),           // FAX (Apostle) -> 3.5 ly, Helium
        (23919, 3.0, 16274),           // Supercarrier (Aeon) -> 3.0 ly, Helium
        (11567, 3.0, 16274),           // Titan (Avatar) -> 3.0 ly, Helium
        (28844, 5.0, 17888),           // Jump Freighter (Rhea) -> 5.0 ly, Nitrogen
        (28352, 5.0, 17887),           // Rorqual -> 5.0 ly, Oxygen
        (22440, 4.0, 17889),           // Black Ops (Panther) -> 4.0 ly, Hydrogen
    ] {
        let plan0 = eve_server::services::space_destiny::space_nav_travel::plan_jump_drive(&ctx, 1, hull_tid, NULL_SYS, DEST_NULL_SYS).expect("plan_jump_drive");
        let t = ctx.sde.types.get(&hull_tid).unwrap();
        assert!((t.default_attributes[&867] - expected_base_ly).abs() < 1e-6, "Hull {hull_tid} base range mismatch");
        assert_eq!(plan0.fuel_type_id, expected_iso, "Hull {hull_tid} faction isotope mismatch");
    }

    let cyno_char = 953_001;
    let cyno_ship = 963_001;
    put_in_fleet(&ctx, 880_010, cyno_char, &[(cyno_char, true)]);
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 973_001, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 973_002, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let beacon_id = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(973_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Cyno").dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // Far low-sec K-space system beyond 7.0 ly to verify JumpDistanceOutOfRange
    let far_sys = *ctx.sde.solar_systems.iter().find(|&(sid, s)| (30_000_000..31_000_000).contains(sid) && *sid != 30_100_000 && s.region_id != 10_000_070 && s.security_status < 0.4 && eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, NULL_SYS, s.solar_system_id) > 15.0).unwrap().0;

    let car_char = 953_002;
    let car_ship = 963_002;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, car_char, 21611, 5); // JDC V -> 7.0 ly
    set_skill(&ctx, car_char, 21610, 5); // JFC V -> -50% fuel
    set_skill(&ctx, car_char, 3456, 5);  // JDO V -> 71.25% cap need
    put_in_fleet(&ctx, 880_010, car_char, &[(cyno_char, true), (car_char, true), (953_003, true), (953_004, true), (953_005, true), (953_006, true)]);

    // Out-of-range check & transactionality (failed jump without fuel preserves capacitor)
    let range_err = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(far_sys as i64), PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err();
    assert_eq!(range_err.code, "JumpDistanceOutOfRange");
    let cap_before_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
    assert_eq!(origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor, cap_before_fail);

    // Refused inside bubble AND when warp core strength < 0; Fuel Bay -> Cargo fallback + JDO V cap deduction
    give_item(&ctx, 973_003, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 100);
    give_item(&ctx, 973_013, 16274, car_char, car_ship, InventoryFlag::Cargo, 100_000);
    let _ = origin_sys.add_warp_bubble(999_002, DVec3::ZERO, 20_000.0).await;
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
    let _ = origin_sys.remove_warp_bubble(999_002).await;

    let mut cs_neg_wcs = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
    cs_neg_wcs.warp_core_strength = -1;
    origin_sys.set_combat_state(car_ship, cs_neg_wcs).await.unwrap();
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err(), "Jump must fail when WCS < 0");

    let mut cs_ok = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
    cs_ok.warp_core_strength = 0;
    cs_ok.capacitor = cs_ok.max_capacitor;
    let max_cap = cs_ok.max_capacitor;
    origin_sys.set_combat_state(car_ship, cs_ok).await.unwrap();

    // Unaligned stationary ship jumps cleanly, exhausting Fuel Bay (100) then Cargo!
    let jump_ok = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("Jump succeeds");
    let dist_ly = jump_ok.dict_get("distanceLY").and_then(|v| v.as_f64()).unwrap();
    let car_fuel = jump_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(jump_ok.dict_get("fuelTypeID"), Some(&PyRep::Integer(16274)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&973_003)), "Fuel Bay must be drained before Cargo");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&973_013).unwrap().quantity), 100_000 - (car_fuel as i32 - 100));
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let cap_after = dest_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    assert!((cap_after - max_cap * (1.0 - 0.95 * 0.75)).abs() < 1.0, "JDO V must leave 28.75% capacitor");

    // 2. Jump Freighter (Rhea 28844, raw 10,000/ly): verify JFC + JF skill + stacking-penalized Prototype Jump Drive Economizers (34126)
    // INCLUDING at jf_lvl == 0 (BF-B35-06 regression check)!
    let s1 = eve_dogma::stacking_penalty_factor(1);
    for (idx, cid, jfc_l, jf_l, n_econ) in [
        (0usize, 953_003i64, 0u8, 0u8, 0usize), // raw 10,000 * D
        (1, 953_004, 4, 0, 2),                  // jf_lvl == 0 WITH JFC IV (-40%) + 2x Economizer (-10% stacking-penalized)!
        (2, 953_005, 0, 5, 0),                  // JF V (-50%)
        (3, 953_006, 5, 5, 2),                  // JFC V (-50%) * JF V (-50%) * 2x Economizer!
    ] {
        let sid = 963_003 + idx as i64;
        let mut s = seed_pilot(&ctx, cid, sid, 28844, NULL_SYS, false);
        spawn_ship_in_space(&ctx, cid, sid, 28844, NULL_SYS, DVec3::new((idx as f64 + 1.0) * 500.0, 0.0, 0.0)).await;
        give_item(&ctx, 973_020 + idx as i64 * 4, 17888, cid, sid, InventoryFlag::SpecializedFuelBay, 50_000);
        set_skill(&ctx, cid, 21611, 5);
        set_skill(&ctx, cid, 21610, jfc_l);
        set_skill(&ctx, cid, 29029, jf_l);
        if n_econ >= 1 {
            give_item(&ctx, 973_021 + idx as i64 * 4, 34126, cid, sid, InventoryFlag::LoSlot0, 1);
        }
        if n_econ >= 2 {
            give_item(&ctx, 973_022 + idx as i64 * 4, 34126, cid, sid, InventoryFlag::LoSlot1, 1);
        }
        ctx.invalidate_ship_fitting(sid);
        let res = ctx.dispatch_rpc(&mut s, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.expect("JF jump");
        let got_fuel = res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
        let mut expected_rate = 10_000.0 * (1.0 - 0.10 * jfc_l as f64) * (1.0 - 0.10 * jf_l as f64);
        if n_econ >= 1 { expected_rate *= 0.90; }
        if n_econ >= 2 { expected_rate *= 1.0 - 0.10 * s1; }
        let expected_fuel = (expected_rate * dist_ly).ceil() as i64;
        assert_eq!(got_fuel, expected_fuel, "JF fuel mismatch for JFC={jfc_l}, JF={jf_l}, econ={n_econ}");
    }
}
