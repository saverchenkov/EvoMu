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
async fn test_phase_5m_mn05_specialized_holds_volume_and_stack_merging() {

    eve_testkit::attest(&[]);
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
        "Add",
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
        "Add",
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
            "Add",
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
async fn test_phase_5m_mn07_ice_and_gas_harvesting() {

    eve_testkit::attest(&[]);
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
        // Set Ice Harvesting skill (16281) to level 5 (reduces cycle time by 25%) and Mining Barge V
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(17940, TrainedSkillRecord { type_id: 17940, level: 5, skill_points: 256_000.0 });
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

    let ice_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, ice_mod_id, ice_ast_id)
        .await
        .expect("Ice Harvester I cycle");
    assert_eq!(ice_res.units_removed, 1, "Ice Harvester I yields exactly 1 unit of 1000m3 ice");
    assert_eq!(ice_res.ore_type_id, 16262);
    // Ice Harvesting V (-5%/level, wiki:Ice_harvesting) and the Procurer's Mining Barge ice
    // harvester duration bonus (SDE attribute 3182 = -2%/level, trained to V here):
    // 240,000 ms x 0.75 x 0.90 = 162,000 ms.
    assert_eq!(ctx.sde.types[&17480].default_attributes[&3182], -2.0);
    assert_eq!(ice_res.cycle_duration_ms, 162_000);

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

    let gas_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, gas_mod_id, gas_ast_id)
        .await
        .expect("Gas Cloud Scoop I cycle");
    let gas_yield = gas_res.units_removed;
    let gas_type = i64::from(gas_res.ore_type_id);

    // Base volume 10 m3 * 2.0 (Venture 100% bonus) = 20 m3 / 10 m3 per unit = 2 units
    assert_eq!(gas_yield, 2);
    assert_eq!(gas_type, 25268);
}

#[tokio::test]
async fn test_phase_5m_mn11_space_and_structure_compression() {
    // parity: rpc:inSpaceCompressionMgr.CompressItemInSpace, rpc:structureCompressionMgr.CompressItemInStructure
    eve_testkit::attest(&["rpc:inSpaceCompressionMgr.CompressItemInSpace", "rpc:structureCompressionMgr.CompressItemInStructure"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    let station_id = 60_003_760;

    // 1. Structure compression while docked in NPC station must fail with MustBeInStructure,
    // and succeed 1:1 losslessly when docked in an Upwell structure (`docs/eve-wiki/Compression.md:27,48`)
    let npc_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(100_000_850)],
            &kw,
            None,
        )
        .await
        .expect_err("NPC station must reject CompressItemInStructure");
    assert_eq!(npc_err.code, "MustBeInStructure");

    let structure_id = 1_000_000_009_001i64;
    let ore_item_id = 100_000_850;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                owner_corp_id: 1000001,
                type_id: 35835, // Athanor
                name: "Perimeter Reprocessing Hub".into(),
                solar_system_id: 30000144,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        snap.items.insert(
            ore_item_id,
            InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230, // Veldspar (1:1 lossless compression to 62516)
                owner_id: char_id,
                location_id: structure_id,
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
    sess.station_id = None;
    sess.structure_id = Some(structure_id);

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
    assert_eq!(comp_qty, 350);
    assert_eq!(comp_rem, 0);

    // Verify original item was converted 1:1 in-place
    let item_after = ctx.db.get_item(ore_item_id).expect("item_after");
    assert_eq!(item_after.type_id, 62516);
    assert_eq!(item_after.quantity, 350);

    // Ice compression: Clear Icicle (16262 -> compresses 1:1 to 28434)
    let ice_item_id = 100_000_851;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ice_item_id,
            InventoryItemRecord {
                item_id: ice_item_id,
                type_id: 16262, // Clear Icicle
                owner_id: char_id,
                location_id: structure_id,
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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
    });
    sess.station_id = Some(station_id);
    sess.structure_id = None;
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

    // In-space compression of Scordite (1228 -> compresses 1:1 to 62520) in SpecializedOreHold
    // on an Orca (28606) with an active Large Industrial Core I (58945) and Large Asteroid Ore Compressor I (62625)
    let space_ore_id = 100_000_852;
    let comp_mod_id = 100_000_853;
    let core_mod_id = 100_000_854;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = 28606; // Orca
        }
        // Unfit the starter ship's modules so the compressor and core get free high slots
        snap.items.retain(|_, item| item.location_id != ship_id);
        // Train the SDE skill prerequisites (requiredSkill1..3 / level) of the compressor and the core
        for tid in [62625_u32, 58945] {
            let t = &ctx.sde.types[&tid];
            for (sk_attr, lvl_attr) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
                if let Some(&sk) = t.default_attributes.get(&sk_attr) {
                    let lvl = t.default_attributes.get(&lvl_attr).copied().unwrap_or(1.0) as u8;
                    snap.skills.entry(char_id).or_default().insert(
                        sk as u32,
                        eve_db::TrainedSkillRecord { type_id: sk as u32, level: lvl.max(1), skill_points: 256_000.0 },
                    );
                }
            }
        }
        snap.items.insert(
            comp_mod_id,
            InventoryItemRecord {
                item_id: comp_mod_id,
                type_id: 62625, // Large Asteroid Ore Compressor I
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
            core_mod_id,
            InventoryItemRecord {
                item_id: core_mod_id,
                type_id: 58945, // Large Industrial Core I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
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
            space_ore_id,
            InventoryItemRecord {
                item_id: space_ore_id,
                type_id: 1228, // Scordite
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
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

    // Activate the core, then the compressor (attribute 3265 requires an active Industrial Core)
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    for (module_id, effect) in [(core_mod_id, "industrialCore"), (comp_mod_id, "compressor")] {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::String(effect.into())],
            &kw,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("Activate {module_id}: {e:?}"));
    }

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
    assert_eq!(space_comp_qty, 500);
}

#[tokio::test]
async fn test_phase_5m_mn12_reprocessing_service() {

    eve_testkit::attest(&[]);
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

    // 1. GetQuotes
    let info_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetQuotes");

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
    // Station tax (0 standing) = 5% (0.05) charged in ISK
    // Output Tritanium = floor(4000 * 0.67068) = 2682
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

    // GetQuotes
    let quote_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[PyRep::Integer(veldspar_id)],
            &kw,
            None,
        )
        .await
        .expect("GetQuotes");

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
    assert_eq!(trit_qty, 2682);

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
