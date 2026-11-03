//! End-to-end parity closure tests (Part 2) for FW Boosters, Abyssal Bioadaptive
//! Cache rewards, Cynosural Field lighting rules, and Blockade Runner cargo-scan immunity.

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay_closure::{
    fit_subcap_blasters, lock_target_via_rpc, open_sqlite_ctx, place_ship_ball,
    register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks, temp_sqlite_path,
    ALLIANCE, CORP, PILOT, PILOT_SHIP,
};

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_proto::PyRep;
use eve_server::services::lp_store_catalog::{register_lp_store_offer, LPOffer};
use eve_server::services::pve_special_sites_abyssal::{
    abyssal_mutaplasmid_for_tier, BIOADAPTIVE_CACHE_TYPE_ID, EXTRACTION_SUBNODE_TYPE_ID,
    TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, ZERO_POINT_CONDENSATE_TYPE_ID,
};
use eve_server::services::structure_acl_defense_rules::TYPE_MOBILE_CYNO_INHIBITOR;
use glam::DVec3;

#[tokio::test]
async fn test_fw_boosters_lp_store_and_consumption_closure() {
    // parity: feat:fw.fw-boosters
    eve_testkit::attest(&["feat:fw.fw-boosters"]);

    let db_path = temp_sqlite_path("parity_fw_boosters");
    let ctx = open_sqlite_ctx(&db_path, true);
    let militia_corp = 1_000_179i64; // 24th Imperial Crusade
    let booster_tid = 91_916u32;   // Imperial Electronics Booster I

    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.owner_corporation_id == militia_corp)
        .map(|st| st.station_id)
        .expect("24th Imperial Crusade station in SDE");
    let sys_id = ctx.sde.stations.get(&station_id).unwrap().solar_system_id;

    let mut sess = register_online_pilot(
        &ctx,
        88_003,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        None,
        sys_id,
        PILOT_SHIP,
        639,
        DVec3::ZERO,
        0,
    )
    .await;
    sess.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(
            snap,
            LPOffer {
                offer_id: 990_179,
                corp_id: militia_corp,
                type_id: booster_tid,
                quantity: 1,
                lp_cost: 2_500,
                isk_cost: IskCents::from_isk(1_000_000),
                req_items: Vec::new(),
                ak_cost: 0,
                min_standing: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(5_000_000);
        }
        snap.skills.entry(PILOT).or_default().insert(
            3405, // Biology
            TrainedSkillRecord { type_id: 3405, level: 2, skill_points: 10_000.0 },
        );
    });

    // 1. GetAvailableOffersFromCorp lists the FW booster offer.
    let offers = rpc_call(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(militia_corp)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else { panic!("expected offer list"); };
    assert!(offer_list.iter().any(|o| o.dict_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(booster_tid))));

    // 2. Negative: insufficient LP fails closed.
    assert!(
        rpc_call(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(militia_corp), PyRep::Integer(990_179), PyRep::Integer(1)],
        )
        .await
        .is_err()
    );

    // 3. Grant LP, purchase FW booster, and consume via dogmaIM.UseBooster.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.loyalty_points.insert(militia_corp, 5_000);
        }
    });
    rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(militia_corp), PyRep::Integer(990_179), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter");

    let booster_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == PILOT && i.location_id == station_id && i.type_id == booster_tid)
            .map(|i| i.item_id)
            .expect("purchased FW booster in station hangar")
    });

    rpc_call(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_item_id)])
        .await
        .expect("UseBooster");
    let active = rpc_call(&ctx, &mut sess, "skillMgr2", "GetBoosters", &[])
        .await
        .expect("GetBoosters");
    let PyRep::Dict(active_dict) = active else { panic!("expected active boosters dict"); };
    assert!(active_dict.iter().any(|(k, _)| k.as_i64() == Some(i64::from(booster_tid))));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_abyssal_bioadaptive_cache_rewards_closure() {
    // parity: feat:abyssal.bioadaptive-cache-rewards
    eve_testkit::attest(&["feat:abyssal.bioadaptive-cache-rewards"]);

    let db_path = temp_sqlite_path("parity_abyssal_cache");
    let ctx = open_sqlite_ctx(&db_path, true);
    let origin_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| (0.45..0.85).contains(&s.security_status) && s.region_id < 11_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_659);

    let mut sess = register_online_pilot(
        &ctx,
        88_004,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        None,
        origin_sys,
        PILOT_SHIP,
        621, // Caracal (Cruiser)
        DVec3::new(10_000.0, 0.0, 0.0),
        0,
    )
    .await;

    // Enter T1 Exotic pocket (47761) via abyssalMgr RPCs and verify Bioadaptive Cache + Extraction Sub-node + T1 HP.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_401,
            InventoryItemRecord {
                item_id: 988_401,
                type_id: 47_761,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let dep1 = rpc_call(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(988_401)])
        .await
        .expect("AbyssalEntranceDeployment T1");
    let tr1 = dep1.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent1 = rpc_call(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)])
        .await
        .expect("AbyssalEntranceGateActivation T1");
    let abyss_sys_t1 = ent1.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let t1_balls = ctx.systems.get_or_spawn(abyss_sys_t1).list_balls().await.unwrap();
    let r1_cache_t1 = t1_balls
        .iter()
        .find(|b| b.type_id == BIOADAPTIVE_CACHE_TYPE_ID)
        .map(|b| b.id)
        .expect("Bioadaptive Cache ball in T1 Abyssal room");
    assert!(t1_balls.iter().any(|b| b.type_id == EXTRACTION_SUBNODE_TYPE_ID));
    let t1_hp = ctx
        .systems
        .get_or_spawn(abyss_sys_t1)
        .get_combat_state(r1_cache_t1)
        .await
        .unwrap()
        .unwrap()
        .max_structure_hp;

    // Enter T5 Exotic pocket (47891) for a second pilot and verify 25x higher hull HP.
    let pilot2 = 90_000_202i64;
    let ship2 = 100_000_202i64;
    let mut s2 = register_online_pilot(
        &ctx,
        88_005,
        "demo2",
        "demo2-secret",
        pilot2,
        CORP,
        None,
        origin_sys,
        ship2,
        621,
        DVec3::new(2_000_000.0, 0.0, 0.0),
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_402,
            InventoryItemRecord {
                item_id: 988_402,
                type_id: 47_891,
                owner_id: pilot2,
                location_id: ship2,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let dep5 = rpc_call(&ctx, &mut s2, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(988_402)])
        .await
        .expect("AbyssalEntranceDeployment T5");
    let tr5 = dep5.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent5 = rpc_call(&ctx, &mut s2, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(tr5)])
        .await
        .expect("AbyssalEntranceGateActivation T5");
    let abyss_sys_t5 = ent5.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let t5_balls = ctx.systems.get_or_spawn(abyss_sys_t5).list_balls().await.unwrap();
    let r1_cache_t5 = t5_balls
        .iter()
        .find(|b| b.type_id == BIOADAPTIVE_CACHE_TYPE_ID && b.id != r1_cache_t1)
        .map(|b| b.id)
        .expect("Bioadaptive Cache ball in T5 Abyssal room");
    let t5_hp = ctx
        .systems
        .get_or_spawn(abyss_sys_t5)
        .get_combat_state(r1_cache_t5)
        .await
        .unwrap()
        .unwrap()
        .max_structure_hp;
    assert!(t5_hp >= t1_hp * 20.0, "T5 cache hull HP ({t5_hp}) must be much higher than T1 ({t1_hp})");

    // Destroy T1 Bioadaptive Cache with blasters and verify wreck container drops tier rewards.
    set_pilot_location(&ctx, &mut sess, abyss_sys_t1, None);
    place_ship_ball(&ctx, abyss_sys_t1, PILOT, PILOT_SHIP, 621, DVec3::new(0.0, 0.0, 1_000.0)).await;
    let blaster_base = 988_910i64;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, blaster_base);
    lock_target_via_rpc(&ctx, &mut sess, abyss_sys_t1, PILOT_SHIP, r1_cache_t1).await;
    let _ = rpc_call(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(blaster_base), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(r1_cache_t1)],
    )
    .await;
    step_weapon_ticks(&ctx, abyss_sys_t1).await;

    let expected_muta = abyssal_mutaplasmid_for_tier(1);
    let dropped_types: Vec<u32> = ctx.db.with_snapshot(|snap| {
        let wreck_id = snap
            .items
            .values()
            .find(|i| i.location_id == abyss_sys_t1 as i64 && i.item_id != r1_cache_t1 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186))
            .map(|i| i.item_id)
            .expect("wreck container produced on Bioadaptive Cache destruction");
        snap.items.values().filter(|i| i.location_id == wreck_id).map(|i| i.type_id).collect()
    });
    assert!(dropped_types.contains(&expected_muta), "cache drop must include tier mutaplasmid");
    assert!(dropped_types.contains(&TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID), "cache drop must include Triglavian survey database");
    assert!(dropped_types.contains(&47_761), "cache drop must include filament");
    assert!(dropped_types.contains(&ZERO_POINT_CONDENSATE_TYPE_ID), "cache drop must include zero-point condensate");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_cyno_lighting_rules_and_blockade_runner_scan_immunity_closure() {
    // parity: feat:modules.cyno-lighting-rules
    // parity: feat:ships.blockade-runner-cargo-scan-immunity
    eve_testkit::attest(&[
        "feat:modules.cyno-lighting-rules",
        "feat:ships.blockade-runner-cargo-scan-immunity",
    ]);

    let db_path = temp_sqlite_path("parity_cyno_and_br");
    let ctx = open_sqlite_ctx(&db_path, true);
    let high_sys = 30_000_142u32; // Jita (0.95)
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.44 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("low-sec system in SDE");
    let wh_sys = 31_000_001u32;   // Wormhole

    let mut s_recon = register_online_pilot(
        &ctx,
        88_006,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        high_sys,
        PILOT_SHIP,
        11_957, // Falcon (Force Recon)
        DVec3::new(10_000.0, 0.0, 0.0),
        0,
    )
    .await;

    // 1. Hull restriction: Rifter (587) cannot fit Cynosural Field Generator I (21096).
    let rifter_id = 100_000_801i64;
    let cyno_mod_id = 988_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rifter_id,
            InventoryItemRecord {
                item_id: rifter_id,
                type_id: 587,
                owner_id: PILOT,
                location_id: high_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            988_202,
            InventoryItemRecord {
                item_id: 988_202,
                type_id: 16_273, // Liquid Ozone
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
        snap.skills.entry(PILOT).or_default().insert(
            21_603, // Cynosural Field Theory V
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
    });
    assert!(
        eve_dogma::ShipFittingEngine::from_sde(1, 587, &ctx.sde)
            .fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde)
            .is_err(),
        "normal cyno must not fit on non-Recon/non-BlackOps hull"
    );

    // 2. High-sec, non-fleet, and Mobile Cyno Inhibitor checks.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CannotLightCynoInSystem"
    );

    let _ = ctx.systems.get_or_spawn(high_sys).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_recon, low_sys, None);
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "MustBeInFleet"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_001,
            FleetRecord {
                fleet_id: 77_001,
                boss_id: PILOT,
                members: vec![PILOT],
                ..Default::default()
            },
        );
        snap.items.insert(
            988_205,
            InventoryItemRecord {
                item_id: 988_205,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(988_205, [10_000.0, 0.0, 0.0]);
    });
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoInhibited"
    );

    // Move outside inhibitor range, light cyno: consumes 50 Liquid Ozone on Force Recon, immobilizes, blocks eject.
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&988_205); });
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light normal cyno on Force Recon");
    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&988_202).unwrap().quantity);
    assert_eq!(ozone_left, 450, "Force Recon with CFT V must consume 50 Liquid Ozone");
    assert_eq!(
        err_code(rpc_call(&ctx, &mut s_recon, "ship", "Eject", &[]).await),
        "CynosuralFieldActive"
    );

    // 3. Wormhole space: cyno can be lit, but jump drive cannot lock/jump to it.
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(PILOT_SHIP).await;
    eve_server::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(&ctx, PILOT, PILOT_SHIP);
    ctx.advance_secs(601);
    set_pilot_location(&ctx, &mut s_recon, wh_sys, None);
    place_ship_ball(&ctx, wh_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light cyno in wormhole space");

    let jumper_cid = 90_000_301i64;
    let jumper_ship = 100_000_301i64;
    let mut s_jumper = register_online_pilot(
        &ctx,
        88_007,
        "jumper",
        "sec",
        jumper_cid,
        CORP,
        Some(ALLIANCE),
        low_sys,
        jumper_ship,
        22_428, // Redeemer
        DVec3::ZERO,
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&77_001).unwrap().members.push(jumper_cid);
    });
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_jumper,
                "beyonce",
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(PILOT), PyRep::Integer(wh_sys as i64), PyRep::Integer(0)],
            )
            .await
        ),
        "JumpDriveForbiddenInWormholeSpace"
    );

    // 4. Blockade Runner (Prorator 12733, group 1202) cargo scan immunity vs high-sec NPC customs scan.
    let br_ship = 100_000_401i64;
    let scanner_mod = 988_301i64;
    let contraband_item = 988_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            br_ship,
            InventoryItemRecord {
                item_id: br_ship,
                type_id: 12_733, // Prorator (Blockade Runner)
                owner_id: PILOT,
                location_id: high_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            contraband_item,
            InventoryItemRecord {
                item_id: contraband_item,
                type_id: 3_721, // Slaves (contraband in high-sec)
                owner_id: PILOT,
                location_id: br_ship,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
        snap.items.insert(
            scanner_mod,
            InventoryItemRecord {
                item_id: scanner_mod,
                type_id: 442, // Cargo Scanner I
                owner_id: jumper_cid,
                location_id: jumper_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(jumper_ship).await;
    set_pilot_location(&ctx, &mut s_jumper, high_sys, None);
    place_ship_ball(&ctx, high_sys, jumper_cid, jumper_ship, 22_428, DVec3::new(1_000.0, 0.0, 0.0)).await;
    place_ship_ball(&ctx, high_sys, PILOT, br_ship, 12_733, DVec3::new(5_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_jumper, high_sys, jumper_ship, br_ship).await;

    let scan_res = rpc_call(
        &ctx,
        &mut s_jumper,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(scanner_mod), PyRep::String("effects.CargoScan".into()), PyRep::Integer(br_ship)],
    )
    .await
    .expect("cargo scan Blockade Runner");
    assert_eq!(scan_res.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(scan_res.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // High-sec NPC customs contraband scan still detects & confiscates contraband on Blockade Runner.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.solar_system_id = high_sys;
            ch.active_ship_id = br_ship;
            ch.station_id = None;
        }
    });
    let confiscated = eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, high_sys, br_ship);
    assert!(!confiscated.is_empty(), "high-sec NPC customs must still confiscate contraband from Blockade Runner");
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&contraband_item)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
