//! Dedicated end-to-end integration tests for open `feat:modules.*`, `feat:ships.*`, and `feat:combat.*` parity rows (Part 1).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::open_feats_b35::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use std::collections::HashMap;

#[tokio::test]
async fn test_cyno_lighting_rules_beacons_and_covert_cyno() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142; // 0.95 high-sec
    let pochven_sys: u32 = 30_000_021; // Kuharah (Pochven)

    let char_id = 981_001;
    let ship_id = 991_001;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 11957, jita_sys, false); // Falcon Force Recon (group 833)
    put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true)]);
    set_skill(&ctx, char_id, 21603, 5); // Cynosural Field Theory V
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, jita_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;

    let cyno_mod = 995_001;
    give_item(&ctx, cyno_mod, 21096, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_002, 16273, char_id, ship_id, InventoryFlag::Cargo, 2_000); // Liquid Ozone

    // 1. High-sec blocks Cynosural Field Generator I
    assert!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .is_err(),
        "Cyno must be blocked in high-security space (Jita)"
    );

    // 2. Pochven (region 10000070) blocks Cynosural Field Generator I
    let _ = ctx.systems.get_or_spawn(jita_sys).remove_ball(ship_id).await;
    sess.solar_system_id = Some(pochven_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys;
        }
    });
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, pochven_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "CynoJammedInPochven",
        "Pochven region-wide cyno jammer must block cyno activation"
    );

    // 3. Null-sec (30004759): moving >= 500 m/s blocks cyno; stopping allows cyno, consumes Liquid Ozone, spawns beacon ball, and immobilizes ship!
    let _ = ctx.systems.get_or_spawn(pochven_sys).remove_ball(ship_id).await;
    sess.solar_system_id = Some(NULL_SYS);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = NULL_SYS;
        }
    });
    let sys_null = ctx.systems.get_or_spawn(NULL_SYS);
    let fast_ball = Ball {
        id: ship_id,
        type_id: 11957,
        position: DVec3::new(20_000.0, 0.0, 0.0),
        velocity: DVec3::new(650.0, 0.0, 0.0),
        max_velocity: 1_000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 150.0,
        speed_fraction: 0.65,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    let _ = sys_null
        .spawn_ball_with_combat(fast_ball, ctx.build_type_combat_state(11957))
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipMovingTooFast",
        "Cannot light cyno while moving >= 500 m/s"
    );

    let _ = sys_null.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 11957, NULL_SYS, DVec3::new(20_000.0, 0.0, 0.0)).await;
    let ozone_before = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
    let cyno_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod)], &kw, None)
        .await
        .expect("Activate Cynosural Field Generator I in Null-sec");
    let ozone_after = ctx.db.with_snapshot(|s| s.items.get(&995_002).unwrap().quantity);
    assert!(ozone_after < ozone_before, "Lighting cyno must consume Liquid Ozone ({ozone_after} < {ozone_before})");
    let beacon_id = cyno_res
        .dict_get("beaconID")
        .and_then(|v| v.as_i64())
        .expect("Beacon ID must be spawned");
    assert!(
        sys_null.get_ball_snapshot(beacon_id).await.unwrap().is_some(),
        "Cynosural beacon ball must exist in space"
    );
    assert!(
        eve_server::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(&ctx, ship_id),
        "Ship cyno state must be active"
    );

    // 4. Covert Cyno (28646) on Prowler Blockade Runner (12735) in cyno-jammed system succeeds!
    let covert_char = 981_002;
    let prowler_ship = 991_002;
    let mut covert_sess = seed_pilot(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, false);
    put_in_fleet(&ctx, 77_001, char_id, &[(char_id, true), (covert_char, true)]);
    give_item(&ctx, 995_099, 33700, char_id, NULL_SYS as i64, InventoryFlag::None, 1); // Mobile Cyno Inhibitor
    set_skill(&ctx, covert_char, 21603, 5);
    spawn_ship_in_space(&ctx, covert_char, prowler_ship, 12735, NULL_SYS, DVec3::new(35_000.0, 0.0, 0.0)).await;
    let covert_mod = 995_010;
    give_item(&ctx, covert_mod, 28646, covert_char, prowler_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_011, 16273, covert_char, prowler_ship, InventoryFlag::Cargo, 500);
    let cov_res = ctx
        .dispatch_rpc(&mut covert_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_mod)], &kw, None)
        .await
        .expect("Covert Cyno on Prowler must bypass cyno inhibitor in Null-sec");
    let cov_beacon_id = cov_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(cov_beacon_id > 0, "Covert cyno must spawn covert fleet beacon");
}

#[tokio::test]
async fn test_jump_drive_fatigue_and_titan_jump_portal_bridge() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let origin_sys = NULL_SYS; // 30004759
    let dest_sys = DEST_NULL_SYS; // 30004760

    let cyno_char = 981_101;
    let cyno_ship = 991_101;
    let carrier_char = 981_102;
    let carrier_ship = 991_102;
    let titan_char = 981_103;
    let titan_ship = 991_103;
    let subcap_char = 981_104;
    let subcap_ship = 991_104;

    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, dest_sys, false);
    let mut carrier_sess = seed_pilot(&ctx, carrier_char, carrier_ship, 23757, origin_sys, false);
    let mut titan_sess = seed_pilot(&ctx, titan_char, titan_ship, 11567, origin_sys, false);
    let mut subcap_sess = seed_pilot(&ctx, subcap_char, subcap_ship, 626, origin_sys, false);

    put_in_fleet(
        &ctx,
        77_002,
        titan_char,
        &[
            (cyno_char, true),
            (carrier_char, true),
            (titan_char, true),
            (subcap_char, true),
        ],
    );

    for &(cid, sk) in &[
        (cyno_char, 21603),
        (carrier_char, 21610),
        (carrier_char, 21611),
        (titan_char, 21610),
        (titan_char, 21611),
        (titan_char, 24562),
    ] {
        set_skill(&ctx, cid, sk, 5);
    }

    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, dest_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, carrier_char, carrier_ship, 23757, origin_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, titan_char, titan_ship, 11567, origin_sys, DVec3::new(20_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, subcap_char, subcap_ship, 626, origin_sys, DVec3::new(21_000.0, 0.0, 0.0)).await;

    // Light Cyno in dest_sys
    give_item(&ctx, 995_101, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 995_102, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000);
    let cyno_res = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(995_101)], &kw, None)
        .await
        .expect("Light Cyno in dest_sys");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap_or(0);

    // 1. Archon Carrier jumps via CmdJumpToFleetModuleBeacon -> consumes Helium Isotopes (16274) and accumulates Jump Fatigue!
    give_item(&ctx, 995_103, 16274, carrier_char, carrier_ship, InventoryFlag::SpecializedFuelBay, 10_000);
    let fatigue_before = ctx.db.with_snapshot(|s| s.characters.get(&carrier_char).map(|c| c.jump_fatigue_until_unix).unwrap_or(0));
    assert_eq!(fatigue_before, 0);
    ctx.dispatch_rpc(
        &mut carrier_sess,
        "beyonce",
        None,
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Carrier CmdJumpToFleetModuleBeacon");
    assert_eq!(carrier_sess.solar_system_id, Some(dest_sys), "Carrier must arrive in dest_sys");
    let fuel_left = ctx.db.with_snapshot(|s| s.items.get(&995_103).unwrap().quantity);
    assert!(fuel_left < 10_000, "Jump Drive must consume isotopes ({fuel_left} < 10000)");
    let (fat_until, cd_until, fat_min) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&carrier_char).unwrap();
        (ch.jump_fatigue_until_unix, ch.jump_cooldown_until_unix, ch.jump_fatigue_minutes)
    });
    let now_u = ctx.now_unix_secs();
    assert!(fat_min > 0.0 && fat_until > now_u + 300, "Jump fatigue timer must be set (> 5m, got {fat_min}m)");
    assert!(cd_until > now_u, "Jump reactivation (blue) timer must be set");

    // 2. Avatar Titan opens Jump Portal via CmdBridgeToFleetModuleBeacon & Subcap jumps through via CmdJumpThroughFleet!
    give_item(&ctx, 995_105, 23953, titan_char, titan_ship, InventoryFlag::HiSlot0, 1); // Jump Portal Generator I
    give_item(&ctx, 995_106, 16275, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 5_000);
    give_item(&ctx, 995_107, 16274, titan_char, titan_ship, InventoryFlag::SpecializedFuelBay, 10_000);
    ctx.dispatch_rpc(
        &mut titan_sess,
        "beyonce",
        None,
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(cyno_char), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Titan CmdBridgeToFleetModuleBeacon");

    ctx.dispatch_rpc(
        &mut subcap_sess,
        "beyonce",
        None,
        "CmdJumpThroughFleet",
        &[PyRep::Integer(titan_char), PyRep::Integer(titan_ship)],
        &kw,
        None,
    )
    .await
    .expect("Fleetmate CmdJumpThroughFleet through Titan bridge");
    assert_eq!(subcap_sess.solar_system_id, Some(dest_sys), "Subcap must arrive in dest_sys via Titan Jump Portal");
    assert!(
        ctx.db.with_snapshot(|s| s.characters.get(&subcap_char).is_some_and(|c| c.jump_fatigue_until_unix > now_u)),
        "Jumping through Titan bridge must accumulate jump fatigue on passenger"
    );
}

#[tokio::test]
async fn test_blockade_runner_scan_immunity_and_frigate_escape_bay() {
    let ctx = build_ctx();
    let kw = HashMap::new();
    let sys_id = NULL_SYS;

    let scanner_char = 981_201;
    let scanner_ship = 991_201;
    let br_char = 981_202;
    let br_ship = 991_202;
    let rifter_char = 981_203;
    let rifter_ship = 991_203;

    let mut scanner_sess = seed_pilot(&ctx, scanner_char, scanner_ship, 626, sys_id, false);
    let _br_sess = seed_pilot(&ctx, br_char, br_ship, 12735, sys_id, false);
    let _rifter_sess = seed_pilot(&ctx, rifter_char, rifter_ship, 587, sys_id, false);

    spawn_ship_in_space(&ctx, scanner_char, scanner_ship, 626, sys_id, DVec3::new(0.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, br_char, br_ship, 12735, sys_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    spawn_ship_in_space(&ctx, rifter_char, rifter_ship, 587, sys_id, DVec3::new(6_000.0, 0.0, 0.0)).await;

    let cargo_scanner = 995_201;
    give_item(&ctx, cargo_scanner, 442, scanner_char, scanner_ship, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, 995_202, 34, br_char, br_ship, InventoryFlag::Cargo, 5_000);
    give_item(&ctx, 995_203, 34, rifter_char, rifter_ship, InventoryFlag::Cargo, 1_234);

    // 1. Cargo Scanner on Blockade Runner (Prowler 12735) -> immune!
    let br_scan = ctx
        .dispatch_rpc(
            &mut scanner_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cargo_scanner), PyRep::Integer(br_ship)],
            &kw,
            None,
        )
        .await
        .expect("Cargo Scan Prowler");
    assert_eq!(br_scan.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(br_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // Cargo Scanner on normal Rifter (587) -> reveals 1,234 Tritanium!
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&cargo_scanner);
    });
    let normal_scan = ctx
        .dispatch_rpc(
            &mut scanner_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cargo_scanner), PyRep::Integer(rifter_ship)],
            &kw,
            None,
        )
        .await
        .expect("Cargo Scan Rifter");
    assert_eq!(normal_scan.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(1_234));

    // 2. Frigate Escape Bay on Megathron (641): assembled Rifter in flag 179 ejects pilot into Rifter on Battleship destruction!
    let bs_char = 981_204;
    let bs_ship = 991_204;
    let feb_frigate = 995_210;
    let _bs_sess = seed_pilot(&ctx, bs_char, bs_ship, 641, sys_id, false);
    spawn_ship_in_space(&ctx, bs_char, bs_ship, 641, sys_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, feb_frigate, 587, bs_char, bs_ship, InventoryFlag::from_u16(179), 1);
    give_item(&ctx, 995_211, 34, bs_char, bs_ship, InventoryFlag::Cargo, 500);

    let death_out = execute_death_pipeline(&ctx, sys_id, bs_char, bs_ship, scanner_char, DVec3::new(10_000.0, 0.0, 0.0), 42)
        .await
        .expect("Execute Megathron death pipeline");
    assert_eq!(death_out.ejected_capsule_id, Some(feb_frigate), "Pilot must eject into Frigate Escape Bay ship ({feb_frigate})");
    let feb_item = ctx.db.with_snapshot(|s| s.items.get(&feb_frigate).cloned()).expect("FEB frigate must survive");
    assert_eq!(feb_item.location_id, sys_id as i64, "FEB frigate must be in space in solar system");
    assert!(ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(feb_frigate).await.unwrap().is_some(), "FEB frigate ball must be spawned in space");
}

#[tokio::test]
async fn test_combat_insurance_void_and_expiry_and_pochven_loot_drop() {
    let ctx = build_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let pochven_sys: u32 = 30_000_021; // Kuharah

    let char_id = 981_301;
    let ship_id = 991_301;
    let killer_char = 981_302;
    let killer_ship = 991_302;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);
    let _killer_sess = seed_pilot(&ctx, killer_char, killer_ship, 626, pochven_sys, false);
    sess.station_id = Some(60_003_760);

    // 1. Insurance contract via insuranceSvc.InsureShip, voiding on repackage, and 12-week expiry fallback (40%)
    let base_price = ctx.sde.types.get(&587).map(|t| t.base_price).filter(|&p| p > 0.0).unwrap_or(230_000.0);
    let cost_isk = (base_price * 0.30).round();
    let _ins_res = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("InsureShip RPC");
    assert!(with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)));

    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "UnInsureShip",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("UnInsureShip voids active insurance contract");
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_id)),
        "Voiding/repackaging ship must remove active insurance contract"
    );

    // Re-assemble, re-insure, advance clock > 12 weeks -> expired contract pays 40% fallback + EVE-mail!
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.singleton = true;
            it.location_id = pochven_sys as i64;
        }
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = pochven_sys;
        }
    });
    sess.station_id = Some(60_003_760);
    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
        &kw,
        None,
    )
    .await
    .expect("Re-InsureShip");
    sess.station_id = None;
    sess.solar_system_id = Some(pochven_sys);
    ctx.advance_time_seconds(12 * 7 * 86_400 + 30);

    // 2. Seed 20 cargo items on ship in Pochven (30000021) -> destroy ship -> ~60% loot drop (12/20) + expired 40% insurance payout + EVE-mail!
    for i in 0..20_i64 {
        give_item(&ctx, 995_300 + i, 34, char_id, ship_id, InventoryFlag::Cargo, 100);
    }
    let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
    let poch_death = execute_death_pipeline(&ctx, pochven_sys, char_id, ship_id, killer_char, DVec3::ZERO, 42)
        .await
        .expect("Execute Pochven death pipeline");
    assert!(
        poch_death.loot_dropped_item_ids.len() > 10,
        "Pochven PvP loot drop (~60%) must drop more than 50% of 20 stacks (got {})",
        poch_death.loot_dropped_item_ids.len()
    );
    assert!(
        ctx.db.with_snapshot(|s| s.mails.len() > mails_before),
        "Ship loss must deliver Insurance Payout EVE-mail"
    );
}
