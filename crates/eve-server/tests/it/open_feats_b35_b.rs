//! Dedicated integration tests for open `feat:modules.*` and `feat:navigation.*` parity rows (Batch 35, Part 2).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::open_feats_b35::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::InventoryFlag;
use eve_destiny::DVec3;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_b35_06_jump_portal_bridge_rules() {
    // parity: feat:modules.jump-portal-bridge-rules
    eve_testkit::attest(&["feat:modules.jump-portal-bridge-rules"]);
    let ctx = build_ctx();
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);

    // Light normal cyno in DEST_NULL_SYS on Force Recon (Falcon 11957)
    let cyno_char = 952_301;
    let cyno_ship = 962_301;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_301, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_302, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_301), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Normal cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // 1. Titan (Avatar 11567): 60s, 500 Strontium cost, bubbled host allowed
    let titan_char = 952_302;
    let titan_ship = 962_302;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_303, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
    give_item(&ctx, 972_304, 16275, titan_char, titan_ship, InventoryFlag::Cargo, 500); // Strontium
    give_item(&ctx, 972_305, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 100_000);

    let j_ok_char = 952_303;
    let j_ok_ship = 962_303;
    let mut j_ok_sess = seed_pilot(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, false); // Moa
    spawn_ship_in_space(&ctx, j_ok_char, j_ok_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

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

    put_in_fleet(&ctx, 880_002, titan_char, &[
        (cyno_char, true), (titan_char, true), (j_ok_char, true), (j_far_char, true),
        (blops_char, true), (cov_jumper, true), (noncov_jumper, true),
    ]);

    // Host in bubble can bridge
    let _ = origin_sys.add_warp_bubble(999_001, DVec3::ZERO, 20_000.0).await;
    let bridge_res = ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Titan open bridge while bubbled");
    assert_eq!(bridge_res.dict_get("durationSeconds"), Some(&PyRep::Real(60.0)));
    assert_eq!(bridge_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(500)));

    // >2,500m jumper rejected
    assert!(ctx.dispatch_rpc(&mut j_far_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &HashMap::new(), None).await.is_err());
    // Bubbled jumper rejected
    assert!(ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &HashMap::new(), None).await.is_err());

    let _ = origin_sys.remove_warp_bubble(999_001).await;
    let jump_ok = ctx.dispatch_rpc(&mut j_ok_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)], &HashMap::new(), None).await.expect("JumpThroughFleet");
    let fuel_consumed = jump_ok.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(fuel_consumed > 0, "Bridge must consume fuel from bridger's fuel bay");

    // 2. Black Ops (Redeemer 22428) bridge: 20s open duration, 0 Strontium cost, covert-cloak only
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_306, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1); // Covert Jump Portal Generator
    give_item(&ctx, 972_307, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 100_000);

    let mut cov_sess = seed_pilot(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, false); // Helios (group 830)
    spawn_ship_in_space(&ctx, cov_jumper, cov_ship, 11172, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let mut noncov_sess = seed_pilot(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, false); // Moa
    spawn_ship_in_space(&ctx, noncov_jumper, noncov_ship, 624, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let blops_b_res = ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Blops open bridge");
    assert_eq!(blops_b_res.dict_get("durationSeconds"), Some(&PyRep::Real(20.0)), "Black Ops bridge must stay open 20s");
    assert_eq!(blops_b_res.dict_get("activationConsumptionQuantity"), Some(&PyRep::Integer(0)), "Black Ops bridge costs 0 Strontium to open");

    // Non-covert hull rejected by Black Ops portal
    let noncov_err = ctx.dispatch_rpc(&mut noncov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &HashMap::new(), None).await;
    assert!(noncov_err.is_err(), "Non-covert hull must be rejected by Black Ops jump portal");

    // Covert hull accepted
    let cov_ok = ctx.dispatch_rpc(&mut cov_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(blops_char), PyRep::Integer(blops_ship)], &HashMap::new(), None).await;
    assert!(cov_ok.is_ok(), "Covert hull must be accepted by Black Ops jump portal");
}

#[tokio::test]
async fn test_b35_07_conduit_jump() {
    // parity: feat:navigation.conduit-jump
    eve_testkit::attest(&["feat:navigation.conduit-jump"]);
    let ctx = build_ctx();

    // Covert cyno in DEST_NULL_SYS on Force Recon (Falcon 11957)
    let cyno_char = 952_401;
    let cyno_ship = 962_401;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_401, 28646, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_402, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000);
    let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_401), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Covert cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // 1. Titan refused
    let titan_char = 952_402;
    let titan_ship = 962_402;
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, NULL_SYS, false);
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, NULL_SYS, DVec3::ZERO).await;
    assert!(ctx.dispatch_rpc(&mut titan_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.is_err());

    // 2. Black Ops (Redeemer 22428): Force Recon (833) jumps, Deacon/Megathron fails even with cloak in cargo, 30 cap
    let blops_char = 952_403;
    let blops_ship = 962_403;
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false);
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_403, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    // Force Recon (Pilgrim 11957, group 833)
    let recon_char = 952_404;
    let recon_ship = 962_404;
    seed_pilot(&ctx, recon_char, recon_ship, 11957, NULL_SYS, false);
    spawn_ship_in_space(&ctx, recon_char, recon_ship, 11957, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;

    // Ineligible Logistics Frigate (Deacon 37453, group 1527) holding cloak in cargo
    let inelig_char = 952_405;
    let inelig_ship = 962_405;
    seed_pilot(&ctx, inelig_char, inelig_ship, 37453, NULL_SYS, false);
    spawn_ship_in_space(&ctx, inelig_char, inelig_ship, 37453, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_409, 11578, inelig_char, inelig_ship, InventoryFlag::Cargo, 1);

    put_in_fleet(&ctx, 880_004, blops_char, &[(cyno_char, true), (blops_char, true), (recon_char, true), (inelig_char, true)]);
    ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Blops conduit jump");

    let recon_sys = ctx.db.with_snapshot(|s| s.characters.get(&recon_char).unwrap().solar_system_id);
    let inelig_sys = ctx.db.with_snapshot(|s| s.characters.get(&inelig_char).unwrap().solar_system_id);
    assert_eq!(recon_sys, DEST_NULL_SYS, "Force Recon (group 833) must conduit jump with Black Ops");
    assert_eq!(inelig_sys, NULL_SYS, "Ineligible hull must be skipped even with cloak in cargo");

    // 3. Rorqual (28352) refused while Industrial Core active and fuel is NOT consumed
    let rorq_char = 952_406;
    let rorq_ship = 962_406;
    let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false);
    spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_406, 20280, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_407, 16274, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(972_406); });
    let rorq_err = ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(rorq_err.is_err(), "Rorqual cannot conduit jump while Industrial Core is active");
    let fuel_preserved = ctx.db.with_snapshot(|s| s.items.get(&972_407).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(fuel_preserved, 50_000, "Refused Rorqual conduit jump must NOT consume fuel");
}

#[tokio::test]
async fn test_b35_08_jump_arrival_placement() {
    // parity: feat:navigation.jump-arrival-placement
    eve_testkit::attest(&["feat:navigation.jump-arrival-placement"]);
    let ctx = build_ctx();
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);

    // Cyno ship in DEST_NULL_SYS
    let cyno_char = 952_501;
    let cyno_ship = 962_501;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    let cyno_pos = DVec3::new(100_000.0, 0.0, 0.0);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, cyno_pos).await;
    give_item(&ctx, 972_501, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_502, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_501), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // 1. Normal jump drive arrival: materialise at 5 km offset from cyno beacon
    let jump_char = 952_502;
    let jump_ship = 962_502;
    let mut jump_sess = seed_pilot(&ctx, jump_char, jump_ship, 23757, NULL_SYS, false); // Archon
    spawn_ship_in_space(&ctx, jump_char, jump_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_503, 16274, jump_char, jump_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    put_in_fleet(&ctx, 880_005, cyno_char, &[(cyno_char, true), (jump_char, true)]);

    ctx.dispatch_rpc(&mut jump_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Jump drive");
    let arrival_ball = dest_sys.get_ball_snapshot(jump_ship).await.unwrap().unwrap().1;
    let beacon_ball = dest_sys.get_ball_snapshot(beacon_id).await.unwrap().unwrap().1;
    let dist_to_beacon = arrival_ball.position.distance(beacon_ball.position);
    assert!((dist_to_beacon - 5_000.0).abs() < 50.0, "Jump arrival must materialise ~5km from beacon (got {dist_to_beacon}m)");

    // 2. Fallback to destination star when cyno ship is destroyed via real death pipeline
    let cyno2_char = 952_503;
    let cyno2_ship = 962_503;
    let mut cyno2_sess = seed_pilot(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno2_char, cyno2_ship, 11957, DEST_NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_504, 21096, cyno2_char, cyno2_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_505, 16273, cyno2_char, cyno2_ship, InventoryFlag::Cargo, 2_000);
    let cyno2_res = ctx.dispatch_rpc(&mut cyno2_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_504), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Cyno 2");
    let beacon2_id = cyno2_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let jump2_char = 952_504;
    let jump2_ship = 962_504;
    let mut jump2_sess = seed_pilot(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, jump2_char, jump2_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    give_item(&ctx, 972_506, 16274, jump2_char, jump2_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    put_in_fleet(&ctx, 880_006, cyno2_char, &[(cyno2_char, true), (jump2_char, true)]);

    // Destroy cyno ship through real combat death pipeline
    let killer = 999_999;
    let _ = eve_server::combat::death_pipeline::execute_death_pipeline(&ctx, DEST_NULL_SYS, cyno2_char, cyno2_ship, killer, DVec3::ZERO, ctx.now_unix_secs()).await.expect("Destroy cyno ship");

    ctx.dispatch_rpc(&mut jump2_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno2_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon2_id)], &HashMap::new(), None).await.expect("Jump to destroyed cyno");
    let fallback_ball = dest_sys.get_ball_snapshot(jump2_ship).await.unwrap().unwrap().1;
    assert_eq!(fallback_ball.position, DVec3::ZERO, "Ship jumping to destroyed cyno must land at system star (DVec3::ZERO)");
}

#[tokio::test]
async fn test_b35_09_jump_drive_range_fuel() {
    // parity: feat:navigation.jump-drive-range-fuel
    eve_testkit::attest(&["feat:navigation.jump-drive-range-fuel"]);
    let ctx = build_ctx();
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);

    let cyno_char = 953_001;
    let cyno_ship = 963_001;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 973_001, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 973_002, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(973_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // Archon (23757): base range 3.5 ly, Helium Isotopes (16274)
    let car_char = 953_002;
    let car_ship = 963_002;
    let jf_char = 953_003;
    let jf5_char = 953_004;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, car_char, 21611, 5); // JDC V -> +100% range (7.0 ly)
    set_skill(&ctx, car_char, 21610, 5); // JFC V -> -50% fuel
    set_skill(&ctx, car_char, 3456, 5);  // JDO V -> 71.25% cap need
    put_in_fleet(&ctx, 880_010, car_char, &[(cyno_char, true), (car_char, true), (jf_char, true), (jf5_char, true)]);

    // 1. Transactionality: failed jump (no fuel) must NOT deduct capacitor
    let cap_before_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    let fail_res = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(fail_res.is_err(), "Jump without fuel must fail");
    let cap_after_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    assert_eq!(cap_after_fail, cap_before_fail, "Failed jump must NOT deduct capacitor");

    // 2. Refused inside warp disruption bubble
    give_item(&ctx, 973_003, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 100_000);
    let _ = origin_sys.add_warp_bubble(999_002, DVec3::ZERO, 20_000.0).await;
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.is_err());

    // 3. Succeeds without alignment when unbubbled; consumes 95% * (1 - 0.25) = 71.25% cap, leaving 28.75%
    let _ = origin_sys.remove_warp_bubble(999_002).await;
    let mut cs_ok = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
    cs_ok.capacitor = cs_ok.max_capacitor;
    let max_cap = cs_ok.max_capacitor;
    origin_sys.set_combat_state(car_ship, cs_ok).await.unwrap();

    let jump_ok = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Jump succeeds");
    assert_eq!(jump_ok.dict_get("fuelTypeID"), Some(&PyRep::Integer(16274)));
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let cap_after = dest_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    let expected_cap = max_cap * (1.0 - 0.95 * 0.75);
    assert!((cap_after - expected_cap).abs() < 1.0, "JDO V leaves 28.75% capacitor, got {cap_after}/{max_cap}");

    // 4. Jump Freighter skill (29029) reduces fuel consumption by 10%/level (50% at V)
    let jf_ship = 963_003;
    let mut jf_sess = seed_pilot(&ctx, jf_char, jf_ship, 28844, NULL_SYS, false); // Rhea (group 902)
    spawn_ship_in_space(&ctx, jf_char, jf_ship, 28844, NULL_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
    give_item(&ctx, 973_004, 17888, jf_char, jf_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    set_skill(&ctx, jf_char, 21611, 5); // JDC V
    set_skill(&ctx, jf_char, 21610, 0); // JFC 0
    set_skill(&ctx, jf_char, 29029, 0); // JF 0
    ctx.invalidate_ship_fitting(jf_ship);

    let jf_res0 = ctx.dispatch_rpc(&mut jf_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("JF jump 0");
    let fuel_jf0 = jf_res0.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();

    let jf5_ship = 963_004;
    let mut jf5_sess = seed_pilot(&ctx, jf5_char, jf5_ship, 28844, NULL_SYS, false);
    spawn_ship_in_space(&ctx, jf5_char, jf5_ship, 28844, NULL_SYS, DVec3::new(600.0, 0.0, 0.0)).await;
    give_item(&ctx, 973_005, 17888, jf5_char, jf5_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    set_skill(&ctx, jf5_char, 21611, 5);
    set_skill(&ctx, jf5_char, 21610, 0);
    set_skill(&ctx, jf5_char, 29029, 5); // JF 5 -> 50% reduction
    ctx.invalidate_ship_fitting(jf5_ship);

    let jf_res5 = ctx.dispatch_rpc(&mut jf5_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("JF jump 5");
    let fuel_jf5 = jf_res5.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(fuel_jf5, (fuel_jf0 + 1) / 2, "Jump Freighter V skill must halve isotope consumption ({fuel_jf5} vs {fuel_jf0})");
}
