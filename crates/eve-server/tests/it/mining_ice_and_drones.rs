//! MN-08..MN-09 ice harvester cycle duration modifiers, mining drone yield bonuses, and Rorqual
//! Excavator mining & ice harvesting drones through SDE dogma attributes.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::mining_cycle_engine::{activate, advance, attr, lock, pilot, rock, rpc, Pilot, CHAR_ID, JITA, SKILL_MINING};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;

const RETRIEVER: u32 = 17_478;

const BARGE_SKILLS: [(u32, u8); 10] =
    [(3_386, 5), (3_410, 5), (17_940, 5), (22_578, 4), (32_918, 5), (3_380, 5), (3_327, 5), (60_377, 3), (3_429, 5), (3_436, 5)];

fn insert_item(p: &Pilot, item_id: i64, type_id: u32, location: i64, flag: InventoryFlag, qty: i32) {
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: CHAR_ID,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn module_attr(p: &Pilot, module: i64, attr_id: u16) -> f64 {
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    fit.get_module_effective_attribute(module, attr_id, &p.ctx.sde)
}

async fn deactivate(p: &mut Pilot, module: i64) {
    rpc(p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(module), PyRep::String("miningLaser".into())]).await.unwrap();
}

#[tokio::test]
async fn test_mn08_ice_harvester_cycle_duration_modifiers() {
    // parity: feat:mining.ice-harvester-cycle-modifiers
    eve_testkit::attest(&["feat:mining.ice-harvester-cycle-modifiers"]);
    const ICE_HARV_I: u32 = 16_278;
    const ICE_HARV_II: u32 = 22_229;
    const IHU_II: u32 = 28_578;
    const ICE_RIG_I: u32 = 32_819;
    const HULK: u32 = 22_544;
    const MACKINAW: u32 = 22_548;
    const CLEAR_ICICLE: u32 = 16_262;

    let harv = 800_500_401;
    let skills = [
        (SKILL_MINING, 5),
        (16_281, 5), // Ice Harvesting V (-5%/lvl = 0.75x)
        (17_940, 5), // Mining Barge V (-2%/lvl on Retriever = 0.90x, -3%/lvl on Hulk = 0.85x, -4%/lvl on Mackinaw = 0.80x)
        (22_551, 5), // Exhumers V (-4%/lvl on Hulk = 0.80x)
        (22_578, 4), // Mining Upgrades IV (-20% CPU penalty -> 10% per IHU II)
        (3_402, 5),
        (3_380, 5),
        (3_327, 5),
        (3_429, 5),
        (26_252, 5), // Jury Rigging
        (26_253, 5), // Astronautics Rigging
    ];
    let mut p = pilot(MACKINAW, &skills, &[(harv, ICE_HARV_I, InventoryFlag::HiSlot0)]).await;
    let ice_rock = 750_500_401;
    rock(&p, ice_rock, CLEAR_ICICLE, 500, 3_000.0).await;
    lock(&mut p, ice_rock).await;

    // Base Ice Harvester I duration 240,000 ms, Ice Harvester II 200,000 ms; both yield 1,000 m3 (1 ice block).
    assert_eq!((attr(&p.ctx, ICE_HARV_I, 73), attr(&p.ctx, ICE_HARV_I, 77)), (240_000.0, 1_000.0));
    assert_eq!((attr(&p.ctx, ICE_HARV_II, 73), attr(&p.ctx, ICE_HARV_II, 77)), (200_000.0, 1_000.0));

    let swap_ship = |p: &Pilot, ship_type_id: u32| {
        p.ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&p.ship_id) {
                ship.type_id = ship_type_id;
            }
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };
    let set_skill = |p: &Pilot, skill_id: u32, level: u8| {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(CHAR_ID).or_default().insert(
                skill_id,
                TrainedSkillRecord { type_id: skill_id, level, skill_points: 256_000.0 },
            );
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };

    // 1. Mining Barge (Retriever 17478): Ice Harvesting V (0.75) x Mining Barge V (-2%/lvl = 0.90) x role (-12.5% = 0.875) = 141,750 ms.
    swap_ship(&p, RETRIEVER);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.75 * 0.90 * 0.875).abs() < 1e-6);

    // 2. Exhumer (Hulk 22544): Ice Harvesting V (0.75) x Mining Barge V (-3%/lvl = 0.85) x Exhumers V (-4%/lvl = 0.80) x role (-30% = 0.70) = 85,680 ms.
    swap_ship(&p, HULK);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.75 * 0.85 * 0.80 * 0.70).abs() < 1e-6);

    // 3. Exhumer (Mackinaw 22548): Ice Harvesting V (0.75) x Mining Barge V (-4%/lvl = 0.80) x role (-12.5% = 0.875) = 126,000 ms.
    swap_ship(&p, MACKINAW);
    let base_mack_dur = 240_000.0 * 0.75 * 0.80 * 0.875;
    assert!((module_attr(&p, harv, 73) - base_mack_dur).abs() < 1e-6);

    // 4. Ice Harvesting skill per-level check (level 3 = 0.85x vs level 5 = 0.75x):
    set_skill(&p, 16_281, 3);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.85 * 0.80 * 0.875).abs() < 1e-6);
    set_skill(&p, 16_281, 5);

    // 5. Fit two Ice Harvester Upgrade IIs (-9% duration each, +10% CPU each with Mining Upgrades IV) and
    // Medium Ice Harvester Accelerator I rig (-12% duration):
    insert_item(&p, 800_500_410, IHU_II, p.ship_id, InventoryFlag::LoSlot0, 1);
    insert_item(&p, 800_500_411, IHU_II, p.ship_id, InventoryFlag::LoSlot1, 1);
    insert_item(&p, 800_500_412, ICE_RIG_I, p.ship_id, InventoryFlag::RigSlot0, 1);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let expected_dur = base_mack_dur * 0.91 * 0.91 * 0.88;
    assert!((module_attr(&p, harv, 73) - expected_dur).abs() < 1e-6);
    assert!((module_attr(&p, harv, 50) - 60.0 * 1.10 * 1.10).abs() < 1e-6);

    // 6. Run a cycle and verify 1 whole ice unit (1,000 m3) is delivered to the mining hold.
    p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let act = activate(&mut p, harv, ice_rock).await.expect("Activate Ice Harvester I");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(expected_dur as i64));
    let out = advance(&p, (expected_dur / 1000.0).ceil() as u64).await;
    assert_eq!((out[0].mining_amount_m3, out[0].units_removed, out[0].units_delivered), (1_000.0, 1, 1));
    assert_eq!(out[0].destination_flag, InventoryFlag::SpecializedOreHold);
    deactivate(&mut p, harv).await;
}

#[tokio::test]
async fn test_mn09_mining_drones_and_excavators() {
    // parity: feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:entity.CmdMineRepeatedly, rpc:entity.CmdReturnBay, rpc:ship.LaunchDrones, rpc:ship.Undock
    eve_testkit::attest(&[
        "feat:drones.mining-drones",
        "feat:mining.mining-drones-yield",
        "feat:mining.excavator-drones",
    ]);
    let p = pilot(RETRIEVER, &BARGE_SKILLS, &[]).await;
    let kw = HashMap::new();
    let mut session = p.sess;
    let ship_id = p.ship_id;
    let jita = p.ctx.systems.get_or_spawn(JITA);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_000_112i64;
    let ice_ast_id = 750_000_113i64;
    p.ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: JITA,
        belt_id: 40_000_001,
        type_id: 1230, // Veldspar (0.1 m3)
        quantity: 120_000,
        initial_quantity: 120_000,
        position: (ship_ball.position.x + 400.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 120_000,
            remaining_units: 120_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(400.0, 0.0, 0.0),
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
    p.ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: JITA,
        belt_id: 40_000_001,
        type_id: 16262, // Clear Icicle (1000 m3, group 465)
        quantity: 200,
        initial_quantity: 200,
        position: (ship_ball.position.x + 450.0, ship_ball.position.y, ship_ball.position.z),
        radius: 100.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ice_ast_id,
            belt_id: 40_000_001,
            type_id: 16262,
            initial_units: 200,
            remaining_units: 200,
            unit_volume: 1_000.0,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ice_ast_id,
            type_id: 16262,
            position: ship_ball.position + DVec3::new(450.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // 1. Mining Drone II (type 10250, group 101, base yield 33 m3 = 330 Veldspar units)
    let drone_id = 800_000_120i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever (has SpecializedOreHold attr 1556)
        }
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 10250,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err_in_bay = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Mining drone in DroneBay cannot mine until launched");
    assert_eq!(err_in_bay.code, "DroneNotInSpace");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("ship.LaunchDrones");

    // 1a. Low skills: Mining Drone Operation I (3438 = 1 -> 1.05x), no Drone Interfacing (3442/3436), no Specialization (22541), no rig
    // Expected yield: floor(33.0 * 1.05 / 0.1) = floor(346.5) = 346 units delivered to SpecializedOreHold (134)
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&CHAR_ID) {
            skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 1, skill_points: 1000.0 });
            skills.remove(&3439);
            skills.remove(&3442);
            skills.remove(&3436);
            skills.remove(&22541);
        }
    });
    let _low_drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly low skills");
    let next_tick = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick).await;
    let has_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 346)
    });
    assert!(has_ore, "Mining Drone II with Mining Drone Operation I must yield floor(33*1.05/0.1) = 346 Veldspar units");
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.clear();
    });

    // 1b. Switch ship hull to Vexor (626, cruiser with no SpecializedOreHold) -> ore delivered to Cargo (5)
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 626; // Vexor
        }
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    p.ctx.invalidate_ship_fitting(ship_id);
    let _cargo_drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly on Vexor");
    let next_tick_cargo = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_cargo).await;
    let has_cargo_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.location_id == ship_id && it.flag == InventoryFlag::Cargo && it.type_id == 1230 && it.quantity == 346)
    });
    assert!(has_cargo_ore, "Vexor Cargo hold must contain the 346 Veldspar units mined by drone");
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.clear();
    });

    // 1c. High skills + Medium Drone Mining Augmentor I rig (25918, +10% attr 434):
    // 33.0 * 1.25 (3438 V) * 1.50 (3442 V) * 1.10 (22541 V) * 1.10 (25918 rig) = 74.86875 m3 -> floor(748.6875) = 748 units
    let rig_id = 800_000_122i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        let skills = snap.skills.entry(CHAR_ID).or_default();
        skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 500_000.0 });
        skills.insert(3442, TrainedSkillRecord { type_id: 3442, level: 5, skill_points: 500_000.0 });
        skills.insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 500_000.0 });
        skills.insert(22541, TrainedSkillRecord { type_id: 22541, level: 5, skill_points: 500_000.0 });
        snap.items.insert(
            rig_id,
            InventoryItemRecord {
                item_id: rig_id,
                type_id: 25918,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    p.ctx.invalidate_ship_fitting(ship_id);

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    let _drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly high skills + rig");
    let next_tick_high = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_high).await;
    let has_ore_748 = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 748)
    });
    assert!(has_ore_748, "Mining Drone II with V/V/V skills + 10% rig must yield floor(74.86875 / 0.1) = 748 units");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdReturnBay");

    // 2. Rorqual Excavator Drones (`feat:mining.excavator-drones`):
    // Both 'Excavator' Mining Drone (41030, ore) and 'Excavator' Ice Harvesting Drone (43681, ice)
    let exc_ore_drone_id = 800_000_121i64;
    let exc_ice_drone_id = 800_000_124i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        for (did, dtid) in [(exc_ore_drone_id, 41030u32), (exc_ice_drone_id, 43681u32)] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: dtid,
                    owner_id: CHAR_ID,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Attempting to launch Excavator drones from a Retriever (non-Rorqual) is rejected:
    let err_non_rorq = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id), PyRep::Integer(exc_ice_drone_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator launch must require a Capital Industrial Ship hull");
    assert_eq!(err_non_rorq.code, "InvalidShipForExcavator");

    let ind_core_id = 800_000_123i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28352; // Rorqual
        }
        snap.skills.entry(CHAR_ID).or_default().insert(
            28374, // Capital Industrial Ships V (+10%/lvl = +50% ore drone yield)
            TrainedSkillRecord { type_id: 28374, level: 5, skill_points: 500_000.0 },
        );
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::DroneBay);
        snap.items.insert(
            ind_core_id,
            InventoryItemRecord {
                item_id: ind_core_id,
                type_id: 28583, // Capital Industrial Core I (+170% ore drone yield attr 2585, -75% ice drone cycle attr 2586)
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    p.ctx.invalidate_ship_fitting(ship_id);

    p.ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id), PyRep::Integer(exc_ice_drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("Launch Excavator ore and ice drones from Rorqual");

    let err_no_core = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator must require active Industrial Core on Rorqual");
    assert_eq!(err_no_core.code, "IndustrialCoreRequired");
    let err_ice_no_core = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Ice Excavator must also require active Industrial Core on Rorqual");
    assert_eq!(err_ice_no_core.code, "IndustrialCoreRequired");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ind_core_id), PyRep::String("industrialCore".into())],
            &kw,
            None,
        )
        .await
        .expect("Activate Capital Industrial Core I");

    // 'Excavator' Mining Drone (41030): base 80 m3 * 1.25 (3438 V) * 1.50 (3442 V) * 1.10 (22541 V) * 1.50 (Rorqual V) * 2.70 (Industrial Core I +170%) = 668.25 m3 = 6,682 Veldspar units
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    let _exc_ore_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly with Excavator Mining Drone on active-core Rorqual");
    let next_tick_exc_ore = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_exc_ore).await;
    let has_exc_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 6_682)
    });
    assert!(has_exc_ore, "'Excavator' Mining Drone on active-core Rorqual V must yield floor(668.25 / 0.1) = 6,682 Veldspar units in SpecializedOreHold");

    // 'Excavator' Ice Harvesting Drone (43681): base 1000 m3 per shortened cycle = 1 Clear Icicle unit
    let _exc_ice_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly with Excavator Ice Harvesting Drone on active-core Rorqual");
    let next_tick_exc_ice = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_exc_ice).await;
    let has_exc_ice = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 16262 && it.quantity == 1)
    });
    assert!(has_exc_ice, "'Excavator' Ice Harvesting Drone on active-core Rorqual must harvest 1 Clear Icicle unit (1,000 m3) per shortened cycle");
}
