//! Comprehensive Phase 5M Mining, Harvesting, Processing & Moons test suite (`MN-01..MN-14, MN-16, C-5, C-6`).
//!

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_test_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    if char_id != 90_000_001 {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.user_id = 1;
                ch.clone_grade = 2;
            } else if let Some(mut base_char) = snap.characters.get(&90_000_001).cloned() {
                base_char.character_id = char_id;
                base_char.user_id = 1;
                base_char.name = format!("Pilot {char_id}");
                base_char.station_id = Some(60_003_760);
                base_char.structure_id = None;
                base_char.solar_system_id = 30_000_142;
                snap.next_entity_id += 1;
                let new_ship_id = snap.next_entity_id;
                base_char.active_ship_id = new_ship_id;
                snap.characters.insert(char_id, base_char);
                snap.items.insert(
                    new_ship_id,
                    InventoryItemRecord {
                        item_id: new_ship_id,
                        type_id: 606, // Velator
                        owner_id: char_id,
                        location_id: 60_003_760,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        });
    }
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Initialize character with high skills
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for (skill_id, level) in [
            (3386, 5),  // Mining V
            (3410, 5),  // Astrogeology V
            (17940, 5), // Mining Barge V
            (22551, 5), // Exhumers V
            (16281, 5), // Ice Harvesting V
            (25544, 5), // Gas Cloud Harvesting V
            (3438, 5),  // Mining Drone Operation V (SDE type_id 3438)
            (3439, 5),  // Legacy Mining Drone Operation alias
            (22541, 5), // Mining Drone Specialization V
            (3436, 5),  // Drone Interfacing V
            (3385, 5),  // Reprocessing V
            (3389, 5),  // Reprocessing Efficiency V
            (12196, 5), // Scrapmetal Processing V
            (18025, 5), // Veldspar Processing V
            (12195, 5), // Veldspar Processing V (SDE type_id 12195)
            (18024, 5), // Scordite Processing V
            (60377, 5), // Simple Ore Processing V
            (22578, 5), // Mining Upgrades V
            (3318, 5),  // Weapon Upgrades V
            (3412, 5),  // Astrometrics V
            (28585, 5), // Industrial Reconfiguration V
            (58956, 5), // Industrial Command Core Skill V
            (62450, 5), // Fleet Compression Logistics V
            (62451, 5), // Capital Fleet Compression Logistics V
            (62452, 5), // Shipboard Compression Technology V
            (62453, 5), // Fleet Compression V
            (62454, 5), // Gas Decompression Efficiency V
            (22536, 5), // Mining Foreman V
            (22552, 5), // Mining Director V
            (3348, 5),  // Leadership V
            (3426, 5),  // CPU Management V
            (3413, 5),  // Power Grid Management V
        ] {
            skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 500_000.0,
                },
            );
        }
    });

    sess
}

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| match k {
                PyRep::String(s) => s == key,
                _ => false,
            })
            .map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

// ====================================================================
// Test 1: MN-01, MN-02, C-5: Static Data, Belt Ore Distribution, Ice Belts
// ====================================================================
#[tokio::test]
async fn test_mn01_mn02_c5_static_data_and_belt_ore_distribution() {
    // parity: feat:mining.static-data, feat:mining.belts-as-balls, feat:mining.belt-ore-distribution
    eve_testkit::attest(&["feat:mining.static-data", "feat:mining.belts-as-balls", "feat:mining.belt-ore-distribution"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // 1. Static data verification from SDE (`feat:mining.static-data`):
    let veldspar_mats = ctx.sde.type_materials.get(&1230).expect("Veldspar typeMaterials");
    assert_eq!(veldspar_mats.len(), 1, "Veldspar must yield Tritanium only");
    assert_eq!(veldspar_mats[0].material_type_id, 34);
    assert_eq!(veldspar_mats[0].quantity, 400);

    let scordite_mats = ctx.sde.type_materials.get(&1228).expect("Scordite typeMaterials");
    assert_eq!(scordite_mats.len(), 2, "Scordite must yield Tritanium and Pyerite");
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 34));
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 35));

    // +5%, +10%, +15% ore variants (`17470`, `17471`, `46689`) and moon ore (`45490` Zeolites) + gas (`30370` Fullerite-C50):
    assert_eq!(ctx.sde.type_materials[&17470][0].quantity, 420); // +5% Veldspar II-Grade
    assert_eq!(ctx.sde.type_materials[&17471][0].quantity, 440); // +10% Veldspar III-Grade
    assert_eq!(ctx.sde.type_materials[&46689][0].quantity, 460); // +15% Veldspar IV-Grade
    assert!(!ctx.sde.type_materials[&45490].is_empty(), "Zeolites moon ore must have typeMaterials");
    assert_eq!(ctx.sde.types[&30370].group_id, 711, "Fullerite-C50 must be Harvestable Cloud group 711");

    // Clear Icicle (16262): Ice yields Heavy Water, Liquid Ozone, Strontium Clathrates, and Helium Isotopes
    let ice_mats = ctx.sde.type_materials.get(&16262).expect("Clear Icicle mats");
    let ice_types: Vec<u32> = ice_mats.iter().map(|m| m.material_type_id).collect();
    for mat_tid in [16272u32, 16273, 16274, 16275] {
        assert!(ice_types.contains(&mat_tid), "Clear Icicle must yield {mat_tid}");
    }
    assert!(ctx.sde.compressible_types.contains_key(&1230), "Veldspar must be compressible in SDE");
    assert!(ctx.sde.compressible_types.contains_key(&16262), "Clear Icicle must be compressible in SDE");

    // 2. Belt ore distribution & Ballpark verification (`feat:mining.belts-as-balls`, `feat:mining.belt-ore-distribution`):
    let mut highsec_bp = eve_destiny::Ballpark::new(30000144);
    let highsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut highsec_bp, 30000144, &ctx.sde);
    assert!(!highsec_states.is_empty(), "Highsec belts must generate asteroids");
    let perim_sys = ctx.systems.get_or_spawn(30000144);
    let _ = perim_sys.populate_asteroid_belt_entities().await.unwrap();
    assert!(perim_sys.get_ball_snapshot(highsec_states[0].asteroid_id).await.unwrap().is_some(), "Belt asteroids must be spawned as Balls in Ballpark");
    let highsec_type_ids: Vec<u32> = highsec_states.iter().map(|s| s.type_id).collect();
    assert!(highsec_type_ids.contains(&1230) && highsec_type_ids.contains(&1228));

    let mut lowsec_bp = eve_destiny::Ballpark::new(30002537); // Amamake (lowsec 0.4)
    let lowsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut lowsec_bp, 30002537, &ctx.sde);
    assert!(!lowsec_states.is_empty(), "Lowsec belts must generate asteroids");
    let lowsec_type_ids: Vec<u32> = lowsec_states.iter().map(|s| s.type_id).collect();
    assert!(
        lowsec_type_ids.iter().any(|&tid| matches!(tid, 20 | 21 | 1226 | 1227 | 1231)),
        "Lowsec belt must contain lowsec ores (Kernite/Omber/Jaspet/Hemorphite/Hedbergite)"
    );

    let mut nullsec_bp = eve_destiny::Ballpark::new(30000208); // LZ-6SU (nullsec -0.17)
    let nullsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut nullsec_bp, 30000208, &ctx.sde);
    assert!(!nullsec_states.is_empty(), "Nullsec belts must generate asteroids");
    let nullsec_type_ids: Vec<u32> = nullsec_states.iter().map(|s| s.type_id).collect();
    assert!(
        nullsec_type_ids.contains(&21) || nullsec_type_ids.contains(&1229) || nullsec_type_ids.contains(&1232),
        "Nullsec belt must contain high-tier nullsec ores (Hedbergite/Bistot/Dark Ochre)"
    );
}


// ====================================================================
// Test 3: MN-05: Specialized Holds and Mining to Jetcan
// ====================================================================
#[tokio::test]
async fn test_mn05_special_holds_and_mining_to_jetcan() {
    // parity: feat:ships.special-holds, feat:mining.ore-hold, feat:mining.mining-to-jetcan, rpc:ship.Jettison
    eve_testkit::attest(&["feat:ships.special-holds", "feat:mining.ore-hold", "feat:mining.mining-to-jetcan"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, mut ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.mode = MovementMode::Stop;
    jita.spawn_ball(ship_ball.clone()).await.unwrap();

    // Ship is Venture (type 32880) with SpecializedOreHold (flag 134, 5000 m3) and Miner II (482)
    let laser_id = 800_000_105i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(laser_id, make_item(laser_id, 482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ast_id = 750_000_105i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 50_000,
        initial_quantity: 50_000,
        position: (ship_ball.position.x + 600.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 50_000,
            remaining_units: 50_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(600.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 60.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // 1. Normal mining cycle via dogmaIM.Activate deposits into Venture's SpecializedOreHold (134)
    let mine_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, laser_id, ast_id)
        .await
        .expect("Miner II cycle on Venture");
    assert_eq!(mine_res.destination_flag, InventoryFlag::SpecializedOreHold);

    // 2. Specialized holds item-group filtering & capacity limits (`feat:ships.special-holds`, `feat:mining.ore-hold`):
    let mineral_stack_id = 800_000_201i64;
    let oversized_ore_id = 800_000_202i64;
    let ammo_stack_id = 800_000_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mineral_stack_id, make_item(mineral_stack_id, 34, char_id, ship_id, InventoryFlag::Cargo, 1_000, false));
        snap.items.insert(oversized_ore_id, make_item(oversized_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 60_000, false));
        snap.items.insert(ammo_stack_id, make_item(ammo_stack_id, 18066, char_id, ship_id, InventoryFlag::Cargo, 5, false));
    });

    // Moving minerals into Venture's Ore Hold (134) must fail with InvalidItemTypeForHold
    let err_min_to_ore = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mineral_stack_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Minerals must be rejected by SpecializedOreHold");
    assert_eq!(err_min_to_ore.code, "InvalidItemTypeForHold");

    // Moving 6,000 m3 of Veldspar into Venture's 5,000 m3 Ore Hold (134) must fail with CapacityExceeded
    let err_overflow = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(oversized_ore_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("6,000 m3 ore must exceed Venture 5,000 m3 Ore Hold");
    assert_eq!(err_overflow.code, "CapacityExceeded");

    // Switch ship to Kryos (654, SpecializedMineralHold flag 136):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 654; // Kryos
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Moving raw ore into Kryos's Mineral Hold (136) must fail with InvalidItemTypeForHold
    let mined_ore_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold)
            .map(|i| i.item_id)
            .unwrap()
    });
    let err_ore_to_min = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mined_ore_item_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Raw ore must be rejected by SpecializedMineralHold");
    assert_eq!(err_ore_to_min.code, "InvalidItemTypeForHold");

    // Moving Tritanium (34) into Kryos's Mineral Hold (136) must succeed
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(mineral_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Tritanium must be accepted into Kryos SpecializedMineralHold");

    // Kryos also has specialIceHoldCapacity (3136 = 30,000 m3, SpecializedIceHold flag 181): accepts ice (16262) and rejects raw ore
    let ice_stack_id = 800_000_204i64;
    let gas_stack_id = 800_000_205i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ice_stack_id, make_item(ice_stack_id, 16262, char_id, ship_id, InventoryFlag::Cargo, 5, false));
        snap.items.insert(gas_stack_id, make_item(gas_stack_id, 30370, char_id, ship_id, InventoryFlag::Cargo, 100, false));
    });
    let err_ore_to_ice = ctx
        .dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(mined_ore_item_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedIceHold as i64)], &kw, None)
        .await
        .expect_err("Raw ore must be rejected by SpecializedIceHold");
    assert_eq!(err_ore_to_ice.code, "InvalidItemTypeForHold");
    ctx.dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(ice_stack_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedIceHold as i64)], &kw, None)
        .await
        .expect("Clear Icicle (16262) must be accepted into SpecializedIceHold (181)");

    // Switch ship to Hoarder (651, SpecializedAmmoHold flag 143 + SpecializedGasHold flag 135 via attr 1557 = 30,000 m3):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 651; // Hoarder
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Moving Charge/Crystal (cat 8) into Hoarder's SpecializedAmmoHold (143) must succeed
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(ammo_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::SpecializedAmmoHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Charges must be accepted into Hoarder SpecializedAmmoHold");

    // SpecializedGasHold (135): rejects raw ore, accepts Fullerite-C50 (30370, group 711)
    let err_ore_to_gas = ctx
        .dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(mined_ore_item_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedGasHold as i64)], &kw, None)
        .await
        .expect_err("Raw ore must be rejected by SpecializedGasHold");
    assert_eq!(err_ore_to_gas.code, "InvalidItemTypeForHold");
    ctx.dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(gas_stack_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedGasHold as i64)], &kw, None)
        .await
        .expect("Harvestable gas (30370) must be accepted into SpecializedGasHold (135)");

    // 3. Jettison mined ore into a jetcan, loot from a second character (Suspect flag), and verify 1h (3,600s) expiry (`feat:mining.mining-to-jetcan`):
    let jettison_res = ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "Jettison",
            &[PyRep::List(vec![PyRep::Integer(mined_ore_item_id)])],
            &kw,
            None,
        )
        .await
        .expect("ship.Jettison mined ore");
    let jetcan_id = get_dict_val(&jettison_res, "jetcanID").unwrap().as_i64().unwrap();
    assert!(jetcan_id > 0, "ship.Jettison must spawn a valid Cargo Container jetcan");

    let can_exists = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&jetcan_id)
            .is_some_and(|i| i.type_id == 23 && i.location_id == 30000142)
    });
    assert!(can_exists, "Jetcan cargo container (type 23) must exist in system space");
    assert!(
        jita.get_ball_snapshot(jetcan_id).await.unwrap().is_some(),
        "Jetcan ball must exist in solar system ballpark"
    );

    // Second character loots the foreign jetcan -> receives CrimeWatch Suspect flag
    let thief_char_id = 90_000_002i64;
    let mut thief_session = setup_test_session(&ctx, thief_char_id).await;
    let thief_undock = ctx
        .dispatch_rpc(&mut thief_session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock thief");
    let thief_ship_id = get_dict_val(&thief_undock, "shipID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&thief_ship_id) {
            ship.type_id = 32880; // Venture (5,000 m³ SpecializedOreHold so critical mining yields fit deterministically)
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&thief_ship_id);

    let loot_res = ctx
        .dispatch_rpc(
            &mut thief_session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mined_ore_item_id),
                PyRep::Integer(thief_ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Thief loots ore from foreign jetcan");
    let suspect_flagged = get_dict_val(&loot_res, "suspectFlagged").unwrap().as_bool().unwrap();
    assert!(suspect_flagged, "Looting from another pilot's jetcan must trigger a CrimeWatch Suspect flag");
    let is_suspect_in_cw = ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&thief_char_id)
        .is_some_and(|cw| cw.suspect_flag_until_unix > ctx.now_unix());
    assert!(is_suspect_in_cw, "Thief character must have active Suspect timer in CrimeWatch state");

    // Advance virtual clock to 3,590s (< 1h, still alive) and then +15s past 3,600s (1 hour jetcan lifetime) -> automatic jetcan despawn
    ctx.advance_time_seconds(3_590);
    assert!(ctx.db.with_snapshot(|snap| snap.items.contains_key(&jetcan_id)), "Jetcan must still exist before 3,600s (1h) expiry");
    ctx.advance_time_seconds(15);
    let can_after_expiry = ctx.db.with_snapshot(|snap| snap.items.contains_key(&jetcan_id));
    assert!(!can_after_expiry, "Expired jetcan must be removed from DB at 3,600s (1 hour) lifetime");
    assert!(
        jita.get_ball_snapshot(jetcan_id).await.unwrap().is_none(),
        "Expired jetcan ball must be despawned from system ballpark"
    );
}


// ====================================================================
// Test 5: MN-08: Ice Belts, Ice Harvesting & Gas Cloud Harvesting
// ====================================================================
#[tokio::test]
async fn test_mn08_ice_and_gas_harvesting() {
    // parity: feat:mining.ice-belts, feat:mining.ice-harvester, feat:mining.gas-harvesting
    eve_testkit::attest(&["feat:mining.ice-belts", "feat:mining.ice-harvester", "feat:mining.gas-harvesting"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    // 0. Regional Ice Belt dungeons & 4-hour (14,400s) depletion respawn (`feat:mining.ice-belts`):
    for (sys_id, exp_name, exp_ice_tid, exp_count) in [
        (30_000_142u32, "Blue Ice Belt", 16264u32, 21usize),
        (30_002_187u32, "Clear Icicle Belt", 16262u32, 19usize),
        (30_002_510u32, "Glacial Mass Belt", 16263u32, 20usize),
        (30_002_659u32, "White Glaze Belt", 16265u32, 19usize),
        (30_002_537u32, "Thick Blue Ice Belt", 16266u32, 37usize),
    ] {
        let resolved = eve_server::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, sys_id)
            .expect("Regional ice belt must match")
            .expect("Regional ice belt must resolve against SDE dungeon layout");
        assert_eq!(resolved.name, exp_name);
        assert_eq!(resolved.asteroids.len(), exp_count);
        assert!(resolved.asteroids.iter().any(|a| a.type_id == exp_ice_tid));
    }
    assert!(!eve_server::services::mining_sites::is_ice_belt_ready_to_respawn(14_399));
    assert!(eve_server::services::mining_sites::is_ice_belt_ready_to_respawn(14_400));

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, mut ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.mode = MovementMode::Stop;
    jita.spawn_ball(ship_ball.clone()).await.unwrap();

    // 1. Ice Harvesting: Clear Icicle (16262, unit volume 1000 m3) (`feat:mining.ice-harvester`)
    let ice_ast_id = 750_000_108i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 16262,
        quantity: 50,
        initial_quantity: 50,
        position: (ship_ball.position.x + 1000.0, ship_ball.position.y, ship_ball.position.z),
        radius: 100.0,
    });
    let ice_harvester_id = 800_000_110i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever (Mining Barge)
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(ice_harvester_id, make_item(ice_harvester_id, 16278, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let ice_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, ice_harvester_id, ice_ast_id)
        .await
        .expect("Ice Harvester cycle");
    assert_eq!(ice_res.mining_amount_m3, 1000.0, "Ice Harvester I miningAmount is one 1000 m3 ice unit");
    assert_eq!((ice_res.units_removed, ice_res.units_delivered), (1, 1), "Ice harvester cycle must harvest 1 ice block (1000 m3)");
    assert_eq!(ctx.db.get_asteroid(ice_ast_id).unwrap().quantity, 49);

    // 2. Gas Harvesting in both K-space Nebula (`GAS_SITES[0]` "Saintly Nebula") and W-space Reservoir (`WORMHOLE_GAS_SITES[0]` "Barren Perimeter Reservoir") (`feat:mining.gas-harvesting`):
    let kspace_gas_site = eve_server::services::mining_sites::resolve_gas_site(&ctx.sde, &eve_server::services::mining_sites::GAS_SITES[0])
        .expect("Resolve K-space Saintly Nebula gas site");
    assert_eq!(kspace_gas_site.name, "Saintly Nebula");
    let (_, wh_clouds) = eve_server::services::mining_sites::resolve_wormhole_gas_site(&ctx.sde, &eve_server::services::mining_sites::WORMHOLE_GAS_SITES[0])
        .expect("Resolve W-space Barren Perimeter Reservoir gas site");
    let (wh_gas_tid, wh_gas_units) = wh_clouds[0];

    let kspace_cloud_id = 750_000_109i64;
    let wh_cloud_id = 750_000_110i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: kspace_cloud_id,
        solar_system_id: 30000142,
        belt_id: kspace_gas_site.dungeon_id as i64,
        type_id: kspace_gas_site.gas_type_id,
        quantity: kspace_gas_site.cloud_units[0],
        initial_quantity: kspace_gas_site.cloud_units[0],
        position: (ship_ball.position.x + 1100.0, ship_ball.position.y, ship_ball.position.z),
        radius: 120.0,
    });
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: wh_cloud_id,
        solar_system_id: 30000142,
        belt_id: 900_000_001,
        type_id: wh_gas_tid,
        quantity: wh_gas_units,
        initial_quantity: wh_gas_units,
        position: (ship_ball.position.x + 1200.0, ship_ball.position.y, ship_ball.position.z),
        radius: 120.0,
    });
    let gas_scoop_id = 800_000_111i64;
    let gas_harvester_id = 800_000_112i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture (has turret hardpoints and gas bonus)
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(gas_scoop_id, make_item(gas_scoop_id, 25266, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));     // Gas Cloud Scoop I
        snap.items.insert(gas_harvester_id, make_item(gas_harvester_id, 25542, char_id, ship_id, InventoryFlag::HiSlot1, 1, true)); // Gas Cloud Harvester I
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let kspace_gas_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, gas_scoop_id, kspace_cloud_id)
        .await
        .expect("Gas Cloud Scoop I cycle on K-space nebula cloud");
    assert_eq!(kspace_gas_res.ore_type_id, kspace_gas_site.gas_type_id);
    assert!(kspace_gas_res.units_removed > 0, "Gas Cloud Scoop I must harvest Cytoserocin from K-space nebula cloud");
    assert_eq!(ctx.db.get_asteroid(kspace_cloud_id).unwrap().quantity, kspace_gas_site.cloud_units[0] - kspace_gas_res.units_removed - kspace_gas_res.residue_units);

    let wh_gas_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, gas_harvester_id, wh_cloud_id)
        .await
        .expect("Gas Cloud Harvester I cycle on W-space Fullerite cloud");
    assert_eq!(wh_gas_res.ore_type_id, wh_gas_tid);
    assert!(wh_gas_res.units_removed >= 10, "Gas Cloud Harvester I must harvest Fullerite from W-space reservoir cloud");
    assert_eq!(ctx.db.get_asteroid(wh_cloud_id).unwrap().quantity, wh_gas_units - wh_gas_res.units_removed - wh_gas_res.residue_units);
}
