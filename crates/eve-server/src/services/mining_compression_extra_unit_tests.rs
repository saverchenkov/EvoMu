//! Additional unit and mutation-killing tests for `mining_compression`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_advanced::mining_compression::*;
use eve_db::{
    corp_roles, CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DogmaAttributeType, InvType, SdeRegistry, SolarSystem, TypeListInfo};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id, group_id, category_id, name: name.into(), mass: 1.0, volume, capacity: 0.0,
        radius: 10.0, base_price: 100.0, published: true, portion_size: 1, market_group_id: None,
        race_id: None, faction_id: None, variation_parent_type_id: None, description_id: None,
        icon_id: None, graphic_id: None, sound_id: None, meta_group_id: None, meta_level: None,
        tech_level: None, default_effect_id: None, description: String::new(),
        effect_ids: Vec::new(), default_attributes,
    }
}

fn make_extra_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.attributes.insert(3262, DogmaAttributeType {
        attribute_id: 3262, name: "gasDecompressionBaseEfficiency".into(),
        default_value: 0.80, high_is_good: true, stackable: true, published: true,
    });
    for (tid, gid, cid, name, vol, attrs) in [
        (62452, 255, 16, "Gas Decompression Efficiency", 0.01, HashMap::from([(3260, 1.0)])),
        (62453, 255, 16, "Shipboard Compression Technology", 0.01, HashMap::from([(3263, 10.0)])),
        (35835, 1406, 65, "Athanor", 10_000.0, HashMap::from([(3261, 0.04), (2722, 2.0), (1547, 2.0)])),
        (81826, 4744, 65, "Metenox Moon Drill", 5_000.0, HashMap::new()),
        (35899, 1322, 66, "Standup Reprocessing Facility I", 100.0, HashMap::from([(717, 0.50)])),
        (37178, 1941, 66, "Standup M-Set Asteroid Ore Grading Processor I", 10.0, HashMap::from([(1547, 2.0), (717, 0.51), (2355, 1.0), (2356, 1.06), (2357, 1.12)])),
        (1230, 462, 25, "Veldspar", 0.1, HashMap::new()),
        (62516, 462, 25, "Compressed Veldspar", 0.001, HashMap::new()),
        (30370, 711, 4, "Fullerite-C50", 1.0, HashMap::new()),
        (62396, 4168, 4, "Compressed Fullerite-C50", 0.01, HashMap::new()),
        (28352, 883, 6, "Rorqual", 1e6, HashMap::new()),
        (28583, 515, 7, "Industrial Core I", 5.0, HashMap::from([(1302, 28352.0)])),
        (62454, 4174, 7, "Capital Asteroid Ore Compressor I", 5.0, HashMap::from([(1298, 883.0), (3265, 1.0), (3255, 500.0), (54, 44_000.0)])),
    ] {
        sde.types.insert(tid, make_inv_type(tid, gid, cid, name, vol, attrs));
    }
    sde.compressible_types.insert(1230, 62516);
    sde.compressible_types.insert(30370, 62396);
    sde.type_lists.insert(500, TypeListInfo {
        type_list_id: 500, name: "Compressible Ore".into(), included_type_ids: vec![1230], ..Default::default()
    });
    for (sys_id, reg_id, const_id, name, sec) in [
        (30_002_537, 10_000_030, 20_000_372, "Amamake", 0.3),
        (30_004_759, 10_000_060, 20_000_700, "1DQ1-A", -0.5),
    ] {
        sde.solar_systems.insert(sys_id, SolarSystem {
            solar_system_id: sys_id, region_id: reg_id, constellation_id: const_id,
            name: name.into(), security_status: sec, radius: 1e12, galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(), stations: Vec::new(), wormhole_class_id: None,
        });
    }

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn compressor_hull_limits_reprocessing_service_rig_yields_and_metenox_helpers() {
    let ctx = make_extra_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_600_001_i64;
    let ship_id = 910_001_i64;
    let struct_hs = 1_000_000_001_001_i64;
    let struct_ls = 1_000_000_001_002_i64;
    let struct_ns = 1_000_000_001_003_i64;
    let metenox_hs = 1_000_000_001_004_i64;

    let (types, groups) = resolve_compressor_hull_limits(ctx.sde.types.get(&28583).unwrap());
    assert_eq!(types, vec![28352]);
    assert!(groups.is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = corp_id;
        ch.solar_system_id = 30_000_142;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;

        let mut corp = CorporationRecord {
            corporation_id: corp_id, ceo_id: 90_000_099, name: "RigCorp".into(), ticker: "RIGC".into(), ..Default::default()
        };
        corp.member_roles.insert(char_id, corp_roles::ROLE_HANGAR_CAN_TAKE[1]);
        snap.corporations.insert(corp_id, corp);

        for (iid, tid, loc, flag) in [
            (ship_id, 28352, 30_000_142, InventoryFlag::Hangar),
            (910_002, 28583, ship_id, InventoryFlag::HiSlot0),
            (910_003, 62454, ship_id, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: char_id, location_id: loc,
                flag, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.module_active_states.insert(910_003);

        for (sid, sys) in [(struct_hs, 30_000_142), (struct_ls, 30_002_537), (struct_ns, 30_004_759)] {
            snap.structures.insert(sid, UpwellStructureRecord {
                structure_id: sid, type_id: 35835, owner_corp_id: corp_id, solar_system_id: sys,
                name: "Rigged Athanor".into(), state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 100, ..Default::default()
            });
            snap.items.insert(sid + 100, InventoryItemRecord {
                item_id: sid + 100, type_id: 37178, owner_id: corp_id, location_id: sid,
                flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, ..Default::default()
            });
        }

        snap.structures.insert(metenox_hs, UpwellStructureRecord {
            structure_id: metenox_hs, type_id: 81826, owner_corp_id: corp_id, solar_system_id: 30_000_142,
            name: "HS Metenox".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, ..Default::default()
        });

        snap.items.insert(910_050, InventoryItemRecord {
            item_id: 910_050, type_id: 35899, owner_id: corp_id, location_id: struct_hs,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(910_050, false);
    });

    let empty_fittings = HashMap::new();
    ctx.db.with_snapshot(|snap| {
        assert!(has_active_compressor_burst(&ctx.sde, snap, &empty_fittings, char_id, 30_000_142, 1230, 10.0).is_none());
        assert!(!structure_has_online_reprocessing_service(&ctx.sde, snap, struct_hs, 35835));

        let y_hs = compute_structure_rigged_base_yield(&ctx.sde, snap, struct_hs, ctx.sde.types.get(&1230));
        let y_ls = compute_structure_rigged_base_yield(&ctx.sde, snap, struct_ls, ctx.sde.types.get(&1230));
        let y_ns = compute_structure_rigged_base_yield(&ctx.sde, snap, struct_ns, ctx.sde.types.get(&1230));
        assert!(y_hs > 0.51 && y_ls > y_hs && y_ns > y_ls);

        assert!(caller_can_take_corp_hangar_item(snap, char_id, corp_id, InventoryFlag::CorpSAG2, 0));
        assert!(!caller_can_take_corp_hangar_item(snap, char_id, corp_id, InventoryFlag::CorpSAG1, 0));
        assert!(!caller_has_structure_extraction_role(snap, char_id, corp_id, 0));
        assert!(caller_has_structure_extraction_role(snap, char_id, corp_id, corp_roles::ROLE_STATION_MANAGER));

        assert_eq!(inspect_metenox_drill(&ctx.sde, snap, 999_999, char_id, 0.40).err().unwrap().code, "StructureNotFound");
        assert_eq!(inspect_metenox_drill(&ctx.sde, snap, struct_hs, char_id, 0.40).err().unwrap().code, "InvalidStructureType");
        assert_eq!(inspect_metenox_drill(&ctx.sde, snap, metenox_hs, char_id, 0.40).err().unwrap().code, "AccessDenied");
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(910_002);
        snap.module_online_states.insert(910_050, true);
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, corp_roles::ROLE_STATION_MANAGER);
        snap.skills.entry(char_id).or_default().insert(62453, TrainedSkillRecord { type_id: 62453, level: 5, skill_points: 256_000.0 });
    });

    ctx.db.with_snapshot(|snap| {
        assert!(structure_has_online_reprocessing_service(&ctx.sde, snap, struct_hs, 35835));
        let (_, max_range) = has_active_compressor_burst(&ctx.sde, snap, &empty_fittings, char_id, 30_000_142, 1230, 10.0).expect("active burst");
        assert!((max_range - 66_000.0).abs() < 1.0);
        assert!(has_active_compressor_burst(&ctx.sde, snap, &empty_fittings, char_id, 30_000_142, 30370, 10.0).is_none());
        assert_eq!(inspect_metenox_drill(&ctx.sde, snap, metenox_hs, char_id, 0.40).err().unwrap().code, "InvalidSystemSecurity");
    });
}

#[tokio::test]
async fn in_space_and_structure_compression_error_branches_fleet_burst_and_corp_hangar() {
    let ctx = make_extra_ctx();
    let miner_id = 90_000_001_i64;
    let booster_id = 90_000_002_i64;
    let corp_id = 98_600_002_i64;
    let miner_ship = 920_001_i64;
    let booster_ship = 920_002_i64;
    let struct_id = 1_000_000_002_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&miner_id].clone();
        for (cid, sid) in [(miner_id, miner_ship), (booster_id, booster_ship)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.fleet_id = Some(501);
            snap.characters.insert(cid, ch);
        }

        snap.fleets.insert(501, FleetRecord {
            fleet_id: 501, boss_id: booster_id, members: vec![booster_id, miner_id], ..Default::default()
        });

        let mut corp = CorporationRecord {
            corporation_id: corp_id, ceo_id: booster_id, name: "FleetCompressCorp".into(), ticker: "FCC".into(), ..Default::default()
        };
        corp.member_roles.insert(miner_id, corp_roles::ROLE_HANGAR_CAN_TAKE[1]);
        snap.corporations.insert(corp_id, corp);

        for (sid, tid, oid) in [(miner_ship, 587, miner_id), (booster_ship, 28352, booster_id)] {
            snap.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: tid, owner_id: oid, location_id: sys_id as i64,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
            });
        }
        for (iid, tid, flag) in [(920_010, 28583, InventoryFlag::HiSlot0), (920_011, 62454, InventoryFlag::HiSlot1)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: booster_id, location_id: booster_ship,
                flag, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_active_states.insert(iid);
        }

        for (iid, tid, flag, qty) in [
            (920_020, 1230, InventoryFlag::Locked, 100),
            (920_021, 1230, InventoryFlag::DroneBay, 100),
            (920_022, 1230, InventoryFlag::SpecializedOreHold, 400),
            (920_023, 62516, InventoryFlag::SpecializedOreHold, 50),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: miner_id, location_id: miner_ship,
                flag, quantity: qty, singleton: false, ..Default::default()
            });
        }

        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: 35835, owner_corp_id: corp_id, solar_system_id: sys_id,
            name: "Corp Athanor".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, ..Default::default()
        });
        for (iid, tid, flag, qty) in [
            (920_030, 1230, InventoryFlag::CorpSAG1, 100),
            (920_031, 1230, InventoryFlag::CorpSAG2, 200),
            (920_032, 62516, InventoryFlag::CorpSAG2, 30),
            (920_033, 62396, InventoryFlag::CorpSAG2, 1),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: corp_id, location_id: struct_id,
                flag, quantity: qty, singleton: false, ..Default::default()
            });
        }
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(miner_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(miner_ship);
    sess.solar_system_id = Some(sys_id);

    assert_eq!(handle_in_space_compression_mgr(&ctx, &mut sess, &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_in_space_compression_mgr(&ctx, &mut sess, &[PyRep::Integer(920_020)]).await.unwrap_err().code, "ItemLocked");
    assert_eq!(handle_in_space_compression_mgr(&ctx, &mut sess, &[PyRep::Integer(920_021)]).await.unwrap_err().code, "NotInMiningHold");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, x) in [(miner_ship, 587, 0.0), (booster_ship, 28352, 10_000.0)] {
        sys_handle.spawn_ball(Ball {
            id: bid, type_id: tid, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 100.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }

    let res = handle_in_space_compression_mgr(&ctx, &mut sess, &[PyRep::Integer(920_022)]).await.expect("fleetmate burst compression");
    assert_eq!(res.dict_get("compressedTypeID").and_then(|v| v.as_i64()), Some(62516));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&920_022].type_id, 62516);
        assert_eq!(s.items[&920_022].quantity, 400);
    });

    sess.structure_id = Some(struct_id);
    assert_eq!(handle_structure_compression_mgr(&ctx, &mut sess, "CompressItemInStructure", &[PyRep::Integer(920_030)]).unwrap().unwrap_err().code, "CrpAccessDenied");

    let corp_comp = handle_structure_compression_mgr(&ctx, &mut sess, "CompressItemInStructure", &[PyRep::Integer(920_031)]).unwrap().expect("CorpSAG2 compression");
    assert_eq!(corp_comp.dict_get("compressedTypeID").and_then(|v| v.as_i64()), Some(62516));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&920_031].type_id, 62516);
        assert_eq!(s.items[&920_031].quantity, 200);
    });

    assert_eq!(handle_structure_compression_mgr(&ctx, &mut sess, "DecompressGasInStructure", &[PyRep::Integer(920_032)]).unwrap().unwrap_err().code, "NotCompressedGas");
    assert_eq!(handle_structure_compression_mgr(&ctx, &mut sess, "DecompressGasInStructure", &[PyRep::Integer(920_033)]).unwrap().unwrap_err().code, "InsufficientQuantityToDecompress");
    assert_eq!(handle_compression(&ctx, &mut sess, "corpMiningLedger", "GetObserversWithMiningEvents", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "CrpAccessDenied");
}
