//! Phase 5M Mining System & Reprocessing integration tests (`MN-04..MN-12`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(901);
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

    // Ensure character has necessary mining and harvesting skills for fitting tests
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for (skill_id, level) in [
            (3386, 5),  // Mining V
            (3410, 5),  // Astrogeology V
            (16281, 5), // Ice Harvesting V
            (25544, 5), // Gas Cloud Harvesting V
        ] {
            skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    sess
}

fn spawn_test_asteroid(
    ctx: &ServerContext,
    asteroid_id: i64,
    solar_system_id: u32,
    type_id: u32,
    quantity: i64,
    position: (f64, f64, f64),
) -> i64 {
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id,
        solar_system_id,
        belt_id: 1001,
        type_id,
        quantity,
        initial_quantity: quantity,
        position,
        radius: 100.0,
    });
    asteroid_id
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

#[tokio::test]
async fn test_phase_5m_mn04_mining_laser_cycle_engine() {
    // parity: plan:MN-04
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Undock character to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // Spawn an asteroid in the system near the ship (1000m away)
    let ast_pos = (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2);
    let ast_id = spawn_test_asteroid(&ctx, 1_000_000_001, sys_id, 1230, 10_000, ast_pos);

    // Fit Miner I (type 483, group 54) to ship in HiSlot0 (flag 27)
    let module_id = 100_000_801;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_000_005);
        snap.items.insert(
            module_id,
            InventoryItemRecord {
                item_id: module_id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 1. Invalid target check
    let invalid_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_res.unwrap_err().code, "InvalidTarget");

    // Clear notifications before activation
    let _ = ctx.drain_notifications_for_char(char_id);

    // 2. Valid activation cycle
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner I");

    let yield_units = get_dict_val(&res, "yieldUnits")
        .and_then(|v| v.as_i64())
        .expect("yieldUnits");
    let cycle_time = get_dict_val(&res, "cycleTimeMs")
        .and_then(|v| v.as_i64())
        .expect("cycleTimeMs");
    let ore_type = get_dict_val(&res, "oreTypeID")
        .and_then(|v| v.as_i64())
        .expect("oreTypeID");
    let is_depleted = get_dict_val(&res, "depleted")
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .expect("depleted");
    let is_hold_full = get_dict_val(&res, "holdFull")
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .expect("holdFull");

    assert!(yield_units > 0);
    assert!(cycle_time > 0);
    assert_eq!(ore_type, 1230);
    assert!(!is_depleted);
    assert!(!is_hold_full);

    // Verify asteroid quantity decreased
    let ast_after = ctx.db.get_asteroid(ast_id).expect("asteroid");
    assert_eq!(ast_after.quantity, 10_000 - yield_units);

    // Verify ore deposited in ship
    let ship_items = ctx.db.get_items_for_character(char_id);
    let ore_in_ship = ship_items
        .iter()
        .find(|i| i.location_id == ship_id && i.type_id == 1230);
    assert!(ore_in_ship.is_some());
    assert_eq!(ore_in_ship.unwrap().quantity as i64, yield_units);

    // 3. Verify push notifications OnOreMined and OnMiningLaserFiredLocal
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnOreMined"),
        "Expected OnOreMined notification"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnMiningLaserFiredLocal"),
        "Expected OnMiningLaserFiredLocal notification"
    );

    // 4. Partial cycle on rock depletion
    let small_ast_id = spawn_test_asteroid(&ctx, 1_000_000_002, sys_id, 1230, 3, ast_pos);

    let dep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(small_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner on small rock");

    let small_yield = get_dict_val(&dep_res, "yieldUnits")
        .and_then(|v| v.as_i64())
        .expect("yieldUnits");
    let small_depleted = get_dict_val(&dep_res, "depleted")
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .expect("depleted");
    let small_cycle = get_dict_val(&dep_res, "cycleTimeMs")
        .and_then(|v| v.as_i64())
        .expect("cycleTimeMs");

    assert_eq!(small_yield, 3);
    assert!(small_depleted);
    assert!(small_cycle < cycle_time, "Partial cycle time must be shorter");

    // Subsequent activation on depleted asteroid returns AsteroidDepleted
    let dep_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(small_ast_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(dep_err.unwrap_err().code, "AsteroidDepleted");

    // 5. Module deactivation
    let deact_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(module_id)],
            &kw,
            None,
        )
        .await
        .expect("Deactivate Miner I");
    let is_active = get_dict_val(&deact_res, "active").and_then(|v| match v {
        PyRep::Bool(b) => Some(*b),
        _ => None,
    });
    assert_eq!(is_active, Some(false));
}

#[tokio::test]
async fn test_phase_5m_mn05_specialized_holds_volume_and_stack_merging() {
    // parity: plan:MN-05
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Venture (type 32880): specialOreHoldCapacity = 5,000.0 m3
    let venture_id = 100_000_810;
    let station_id = 60_003_760;

    let item_a = 100_000_811;
    let item_b = 100_000_812;
    let item_c = 100_000_813;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = venture_id;
        }
        snap.items.insert(
            venture_id,
            InventoryItemRecord {
                item_id: venture_id,
                type_id: 32880, // Venture
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Item A: 10,000 Veldspar (0.1 m3 each = 1,000 m3)
        snap.items.insert(
            item_a,
            InventoryItemRecord {
                item_id: item_a,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Item B: 5,000 Veldspar (0.1 m3 each = 500 m3)
        snap.items.insert(
            item_b,
            InventoryItemRecord {
                item_id: item_b,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Item C: 50,000 Veldspar (0.1 m3 each = 5,000 m3)
        snap.items.insert(
            item_c,
            InventoryItemRecord {
                item_id: item_c,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    sess.ship_id = Some(venture_id);

    // Move Item A to SpecializedOreHold (flag 134)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(item_a),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Move Item A to OreHold");

    let moved_a = ctx.db.get_item(item_a).expect("item_a");
    assert_eq!(moved_a.location_id, venture_id);
    assert_eq!(moved_a.flag, InventoryFlag::SpecializedOreHold);
    assert_eq!(moved_a.quantity, 10_000);

    // Move Item B to SpecializedOreHold: stacks must merge into item_a
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(item_b),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Move Item B to OreHold");

    // Item B must be removed, Item A quantity must be 15,000
    assert!(ctx.db.get_item(item_b).is_none());
    let merged_a = ctx.db.get_item(item_a).expect("item_a after merge");
    assert_eq!(merged_a.quantity, 15_000);

    // Move Item C (5,000 m3) into OreHold (current hold has 1,500 m3 used out of 5,000 m3, remaining 3,500 m3)
    // Must be rejected with HoldCapacityExceeded
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(item_c),
                PyRep::Integer(venture_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(cap_err.unwrap_err().code, "CapacityExceeded");
    let untouched_c = ctx.db.get_item(item_c).expect("item_c untouched");
    assert_eq!(untouched_c.location_id, station_id);
    assert_eq!(untouched_c.quantity, 50_000);
}

#[tokio::test]
async fn test_phase_5m_mn06_mining_crystals_and_residue() {
    // parity: plan:MN-06
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Undock to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // Spawn Veldspar asteroid (type 1230) 1000m from ship
    let ast_pos = (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2);
    let ast_id = spawn_test_asteroid(&ctx, 1_000_000_003, sys_id, 1230, 50_000, ast_pos);

    // Fit Miner II (type 482) in HiSlot0 (flag 27) with residue attributes (prob = 34.0, mult = 1.0)
    let module_id = 100_000_820;
    // Load Veldspar Mining Crystal I (type 18066) into the same slot (flag 27)
    let crystal_id = 100_000_821;

    let mut crystal_attrs = HashMap::new();
    crystal_attrs.insert(eve_sde::attr_ids::HP_STRUCT, 1.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_000_005);
        snap.items.insert(
            module_id,
            InventoryItemRecord {
                item_id: module_id,
                type_id: 482, // Miner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            crystal_id,
            InventoryItemRecord {
                item_id: crystal_id,
                type_id: 18066, // Veldspar Mining Crystal I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: crystal_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 1. Activate cycle: crystal damage and residue
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with Crystal");

    let yield_units = get_dict_val(&res, "yieldUnits")
        .and_then(|v| v.as_i64())
        .expect("yieldUnits");
    let crystal_dmg = get_dict_val(&res, "crystalDamage")
        .and_then(|v| match v {
            PyRep::Real(r) => Some(*r),
            _ => None,
        })
        .expect("crystalDamage");
    let crystal_destroyed = get_dict_val(&res, "crystalDestroyed")
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .expect("crystalDestroyed");
    let residue_units = get_dict_val(&res, "residueUnits")
        .and_then(|v| v.as_i64())
        .expect("residueUnits");

    assert_eq!(crystal_dmg, 0.025); // Veldspar Mining Crystal I attribute 784
    assert!(!crystal_destroyed);
    assert!(residue_units > 0);

    // Check crystal HP reduced in db
    let crystal_item = ctx.db.get_item(crystal_id).expect("crystal_item");
    let crystal_hp = crystal_item
        .dynamic_attributes
        .get(&eve_sde::attr_ids::HP_STRUCT)
        .copied()
        .unwrap_or(0.0);
    assert!((crystal_hp - 0.975).abs() < 1e-4);

    // Check asteroid quantity reduced by yield + residue
    let ast_after = ctx.db.get_asteroid(ast_id).expect("asteroid");
    assert_eq!(ast_after.quantity, 50_000 - (yield_units + residue_units));

    // 2. Crystal destruction when HP drops to 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&crystal_id) {
            c.dynamic_attributes
                .insert(eve_sde::attr_ids::HP_STRUCT, 0.020);
        }
    });

    let res_destr = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II to destroy crystal");

    let is_destroyed = get_dict_val(&res_destr, "crystalDestroyed")
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .expect("crystalDestroyed");
    assert!(is_destroyed);

    // Crystal must be removed from inventory
    assert!(ctx.db.get_item(crystal_id).is_none());
}

#[tokio::test]
async fn test_phase_5m_mn07_ice_and_gas_harvesting() {
    // parity: plan:MN-07
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Undock to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // 1. Ice Harvesting: Ice Harvester I (type 16278), Clear Icicle (type 16262, 1000 m3/unit)
    let ice_ast_pos = (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2);
    let ice_ast_id = spawn_test_asteroid(&ctx, 1_000_000_004, sys_id, 16262, 100, ice_ast_pos);

    let ice_mod_id = 100_000_830;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 17480; // Procurer (16,000 m3 ice hold)
        }
        snap.items.remove(&100_000_005);
        snap.items.insert(
            ice_mod_id,
            InventoryItemRecord {
                item_id: ice_mod_id,
                type_id: 16278, // Ice Harvester I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Set Ice Harvesting skill (16281) to level 5 (reduces cycle time by 25%)
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            16281,
            TrainedSkillRecord {
                type_id: 16281,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ice_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ice_mod_id), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Ice Harvester I");

    let ice_yield = get_dict_val(&ice_res, "yieldUnits")
        .and_then(|v| v.as_i64())
        .expect("yieldUnits");
    let ice_type = get_dict_val(&ice_res, "oreTypeID")
        .and_then(|v| v.as_i64())
        .expect("oreTypeID");
    let ice_cycle = get_dict_val(&ice_res, "cycleTimeMs")
        .and_then(|v| v.as_i64())
        .expect("cycleTimeMs");

    assert_eq!(ice_yield, 1, "Ice Harvester I yields exactly 1 unit of 1000m3 ice");
    assert_eq!(ice_type, 16262);
    // Base duration 240,000 ms * (1.0 - 0.25) = 180,000 ms
    assert_eq!(ice_cycle, 180_000);

    // 2. Gas Harvesting: Gas Cloud Scoop I (type 25266), Amber Cytoserocin (type 25268, volume 10 m3)
    let gas_ast_id = spawn_test_asteroid(&ctx, 1_000_000_005, sys_id, 25268, 500, ice_ast_pos);

    let gas_mod_id = 100_000_831;
    // Set ship type to Venture (32880, 100% gas bonus -> double yield)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ice_mod_id);
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.type_id = 32880;
        }
        snap.items.insert(
            gas_mod_id,
            InventoryItemRecord {
                item_id: gas_mod_id,
                type_id: 25266, // Gas Cloud Scoop I (base amount 10 m3)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let gas_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gas_mod_id), PyRep::Integer(gas_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Gas Cloud Harvester I");

    let gas_yield = get_dict_val(&gas_res, "yieldUnits")
        .and_then(|v| v.as_i64())
        .expect("yieldUnits");
    let gas_type = get_dict_val(&gas_res, "oreTypeID")
        .and_then(|v| v.as_i64())
        .expect("oreTypeID");

    // Base volume 10 m3 * 2.0 (Venture 100% bonus) = 20 m3 / 10 m3 per unit = 2 units
    assert_eq!(gas_yield, 2);
    assert_eq!(gas_type, 25268);
}

#[tokio::test]
async fn test_phase_5m_mn08_survey_scanner() {
    // parity: plan:MN-08
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Empty args and kwargs check must return UnimplementedRPC for backwards compatibility
    let empty_err = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[], &kw, None)
        .await;
    assert_eq!(empty_err.unwrap_err().code, "UnimplementedRPC");

    // Undock to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // Fit Mining Survey Chipset I (type 444, group 49) in MedSlot0 (flag 19)
    let scanner_id = 100_000_840;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            scanner_id,
            InventoryItemRecord {
                item_id: scanner_id,
                type_id: 444, // Mining Survey Chipset I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Spawn 2 asteroids in system within survey scanner range (range >= 15km)
    let ast1_id = spawn_test_asteroid(&ctx, 1_000_000_006, sys_id, 1230, 24_000, (ship_pos.0 + 2_000.0, ship_pos.1, ship_pos.2));
    let ast2_id = spawn_test_asteroid(&ctx, 1_000_000_007, sys_id, 1228, 15_000, (ship_pos.0 + 4_000.0, ship_pos.1, ship_pos.2));

    // Clear notifications before scan
    let _ = ctx.drain_notifications_for_char(char_id);

    // Call survey scan with scanner moduleID
    let scan_res = ctx
        .dispatch_rpc(
            &mut sess,
            "miningScanMgr",
            None,
            "perform_scan",
            &[PyRep::Integer(scanner_id)],
            &kw,
            None,
        )
        .await
        .expect("perform_scan");

    let count = get_dict_val(&scan_res, "count")
        .and_then(|v| v.as_i64())
        .expect("count");
    assert!(count >= 2);

    let asteroids_val = get_dict_val(&scan_res, "asteroids").expect("asteroids list");
    let ast_list = match asteroids_val {
        PyRep::List(l) => l,
        _ => panic!("Expected list of asteroids"),
    };

    let found_ast1 = ast_list.iter().find(|entry| {
        get_dict_val(entry, "asteroidID").and_then(|v| v.as_i64()) == Some(ast1_id)
    });
    assert!(found_ast1.is_some());
    let ast1_entry = found_ast1.unwrap();
    assert_eq!(
        get_dict_val(ast1_entry, "quantity").and_then(|v| v.as_i64()),
        Some(24_000)
    );
    assert_eq!(
        get_dict_val(ast1_entry, "typeID").and_then(|v| v.as_i64()),
        Some(1230)
    );

    let found_ast2 = ast_list.iter().find(|entry| {
        get_dict_val(entry, "asteroidID").and_then(|v| v.as_i64()) == Some(ast2_id)
    });
    assert!(found_ast2.is_some());
    let ast2_entry = found_ast2.unwrap();
    assert_eq!(
        get_dict_val(ast2_entry, "quantity").and_then(|v| v.as_i64()),
        Some(15_000)
    );
    assert_eq!(
        get_dict_val(ast2_entry, "typeID").and_then(|v| v.as_i64()),
        Some(1228)
    );

    // Verify OnAsteroidRevealed push notifications were emitted
    let notifs = ctx.drain_notifications_for_char(char_id);
    let revealed: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnAsteroidRevealed")
        .collect();
    assert!(
        revealed.len() >= 2,
        "Expected at least 2 OnAsteroidRevealed notifications"
    );
}

#[tokio::test]
async fn test_phase_5m_mn11_space_and_structure_compression() {
    // parity: plan:MN-11
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    let station_id = 60_003_760;

    // 1. Structure compression while docked
    // Insert 350 units of Veldspar (portion size 100 -> compresses to type 62516)
    let ore_item_id = 100_000_850;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_item_id,
            InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230, // Veldspar (portion_size = 100)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 350,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let comp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(ore_item_id)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInStructure");

    let comp_type = get_dict_val(&comp_res, "compressedTypeID")
        .and_then(|v| v.as_i64())
        .expect("compressedTypeID");
    let comp_qty = get_dict_val(&comp_res, "quantity")
        .and_then(|v| v.as_i64())
        .expect("quantity");
    let comp_rem = get_dict_val(&comp_res, "remainderUnits")
        .and_then(|v| v.as_i64())
        .expect("remainderUnits");

    assert_eq!(comp_type, 62516); // Compressed Veldspar
    assert_eq!(comp_qty, 3);
    assert_eq!(comp_rem, 50);

    // Verify original item was converted and remainder item was created
    let item_after = ctx.db.get_item(ore_item_id).expect("item_after");
    assert_eq!(item_after.type_id, 62516);
    assert_eq!(item_after.quantity, 3);

    let hangar_items = ctx.db.get_items_for_character(char_id);
    let rem_item = hangar_items
        .iter()
        .find(|i| i.location_id == station_id && i.type_id == 1230 && i.item_id != 100_000_012 && i.item_id != ore_item_id);
    assert!(rem_item.is_some());
    assert_eq!(rem_item.unwrap().quantity, 50);

    // Ice compression: Clear Icicle (16262, portion size 1 -> compresses to 28434)
    let ice_item_id = 100_000_851;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ice_item_id,
            InventoryItemRecord {
                item_id: ice_item_id,
                type_id: 16262, // Clear Icicle
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 7,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let ice_comp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(ice_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Compress ice in structure");

    let ice_comp_type = get_dict_val(&ice_comp_res, "compressedTypeID")
        .and_then(|v| v.as_i64())
        .expect("compressedTypeID");
    let ice_comp_qty = get_dict_val(&ice_comp_res, "quantity")
        .and_then(|v| v.as_i64())
        .expect("quantity");
    let ice_comp_rem = get_dict_val(&ice_comp_res, "remainderUnits")
        .and_then(|v| v.as_i64())
        .expect("remainderUnits");

    assert_eq!(ice_comp_type, 28434); // Compressed Clear Icicle
    assert_eq!(ice_comp_qty, 7);
    assert_eq!(ice_comp_rem, 0);

    // Calling CompressItemInSpace while docked must fail with MustBeInSpace
    let space_comp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(ore_item_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(space_comp_err.unwrap_err().code, "MustBeInSpace");

    // 2. In-space compression while undocked
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");

    // Calling structure compression while undocked must fail with NotDocked
    let struct_comp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(ore_item_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(struct_comp_err.unwrap_err().code, "NotDocked");

    // In-space compression of Scordite (1228, portion size 100 -> compresses to 62520) in ship cargo
    let space_ore_id = 100_000_852;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            space_ore_id,
            InventoryItemRecord {
                item_id: space_ore_id,
                type_id: 1228, // Scordite
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let space_comp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(space_ore_id)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInSpace");

    let space_comp_type = get_dict_val(&space_comp_res, "compressedTypeID")
        .and_then(|v| v.as_i64())
        .expect("compressedTypeID");
    let space_comp_qty = get_dict_val(&space_comp_res, "quantity")
        .and_then(|v| v.as_i64())
        .expect("quantity");

    assert_eq!(space_comp_type, 62520); // Compressed Scordite
    assert_eq!(space_comp_qty, 5);
}

#[tokio::test]
async fn test_phase_5m_mn12_reprocessing_service() {
    // parity: plan:MN-12
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    let station_id = 60_003_760;

    // Train skills for character:
    // Reprocessing (3385) = 5 (+15%)
    // Reprocessing Efficiency (3389) = 4 (+8%)
    // Veldspar Processing (12180) = 4 (+8%)
    // Scrapmetal Processing (12196) = 5 (+10%)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3385,
            TrainedSkillRecord {
                type_id: 3385,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3389,
            TrainedSkillRecord {
                type_id: 3389,
                level: 4,
                skill_points: 90_510.0,
            },
        );
        sk.insert(
            60377,
            TrainedSkillRecord {
                type_id: 60377,
                level: 4,
                skill_points: 90_510.0,
            },
        );
        sk.insert(
            12196,
            TrainedSkillRecord {
                type_id: 12196,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. GetReprocessingInfo
    let info_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetReprocessingInfo",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetReprocessingInfo");

    let base_yield = get_dict_val(&info_res, "stationBaseYield")
        .and_then(|v| match v {
            PyRep::Real(r) => Some(*r),
            _ => None,
        })
        .expect("stationBaseYield");
    let reproc_lvl = get_dict_val(&info_res, "reprocessingSkillLevel")
        .and_then(|v| v.as_i64())
        .expect("reprocessingSkillLevel");
    let reproc_eff_lvl = get_dict_val(&info_res, "reprocessingEfficiencyLevel")
        .and_then(|v| v.as_i64())
        .expect("reprocessingEfficiencyLevel");

    assert_eq!(base_yield, 0.50);
    assert_eq!(reproc_lvl, 5);
    assert_eq!(reproc_eff_lvl, 4);

    // 2. Reprocess Veldspar: 1,050 units (portion_size 100 -> 10 portions of 400 Tritanium each = 4000 base Tritanium)
    // Yield rate = 0.50 * (1 + 0.15) * (1 + 0.08) * (1 + 0.08) = 0.50 * 1.15 * 1.08 * 1.08 = 0.67068
    // Station tax (0 standing) = 5% (0.05)
    // Net yield = 0.67068 * 0.95 = 0.637146
    // Output Tritanium = floor(4000 * 0.637146) = 2548
    let veldspar_id = 100_000_860;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            veldspar_id,
            InventoryItemRecord {
                item_id: veldspar_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1_050,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // GetQuote
    let quote_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuote",
            &[PyRep::Integer(veldspar_id)],
            &kw,
            None,
        )
        .await
        .expect("GetQuote");

    let quote_entry = match &quote_res {
        PyRep::List(l) => l.first().expect("quote entry"),
        _ => &quote_res,
    };
    let quote_yield = get_dict_val(quote_entry, "yieldRate")
        .and_then(|v| match v {
            PyRep::Real(r) => Some(*r),
            _ => None,
        })
        .expect("yieldRate");
    let quote_tax = get_dict_val(quote_entry, "stationTaxRate")
        .and_then(|v| match v {
            PyRep::Real(r) => Some(*r),
            _ => None,
        })
        .expect("stationTaxRate");

    assert!((quote_yield - 0.67068).abs() < 1e-4);
    assert!((quote_tax - 0.05).abs() < 1e-4);

    // Clear notifications
    let _ = ctx.drain_notifications_for_char(char_id);

    // Execute Reprocess
    let reproc_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(veldspar_id)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess Veldspar");

    let outputs_rep = get_dict_val(&reproc_res, "outputs").expect("outputs list");
    let mat_outputs = match outputs_rep {
        PyRep::List(l) => l,
        _ => panic!("Expected list of output items"),
    };
    assert!(!mat_outputs.is_empty());

    let trit_out = mat_outputs.iter().find(|entry| {
        get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(34)
    });
    assert!(trit_out.is_some());
    let trit_qty = get_dict_val(trit_out.unwrap(), "quantity")
        .and_then(|v| v.as_i64())
        .expect("tritium quantity");
    assert_eq!(trit_qty, 2548);

    // Remainder of 50 Veldspar remains in hangar
    let rem_veldspar = ctx.db.get_item(veldspar_id).expect("remainder veldspar");
    assert_eq!(rem_veldspar.quantity, 50);

    // Verify OnReprocessingMaterialsChanged notification was emitted
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs
            .iter()
            .any(|n| n.event == "OnReprocessingMaterialsChanged"),
        "Expected OnReprocessingMaterialsChanged notification"
    );

    // 3. Scrapmetal Processing: unassembled Rifter (type 587, singleton: false)
    let rifter_item_id = 100_000_861;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rifter_item_id,
            InventoryItemRecord {
                item_id: rifter_item_id,
                type_id: 587, // Rifter (has SDE typeMaterials)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false, // packaged
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let scrap_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(rifter_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess packaged Rifter via Scrapmetal Processing");

    let scrap_outputs_rep = get_dict_val(&scrap_res, "outputs").expect("scrap outputs list");
    let scrap_outputs = match scrap_outputs_rep {
        PyRep::List(l) => l,
        _ => panic!("Expected list of output items from scrapmetal"),
    };
    assert!(!scrap_outputs.is_empty());
    // Must produce Tritanium (34), Pyerite (35), Mexallon (36)
    assert!(scrap_outputs.iter().any(|entry| {
        get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(34)
    }));
    assert!(scrap_outputs.iter().any(|entry| {
        get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(35)
    }));
    assert!(scrap_outputs.iter().any(|entry| {
        get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(36)
    }));
}
