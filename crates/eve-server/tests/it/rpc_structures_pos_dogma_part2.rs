//! Part 2 of integration tests covering structure directory, guests, hangar view, and POS manager (`posMgr`).

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, ANSIBLEX_TYPE_ID,
};
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const CORP_CALDARI_NAVY: i64 = 1_000_035;
const CORP_OTHER: i64 = 1_000_172;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;
const SYS_DEST: u32 = 30_000_144;
const JITA_PLANET_ID: i64 = 40_009_082;
const LOWSEC_SOLAR_SYSTEM_ID: u32 = 30_000_012;
const LOWSEC_PLANET_ID: i64 = 40_000_663;

const ASTRAHUS_TYPE_ID: u32 = 35832;
const GANTRY_TYPE_ID: u32 = 3962;
const POCO_TYPE_ID: u32 = 2233;
const CONTROL_TOWER_TYPE_ID: u32 = 16213;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn seed_test_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
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
}

fn seed_test_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
    state: &str,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id,
                solar_system_id,
                name: name.to_string(),
                position: [0.0, 0.0, 0.0],
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
}

#[tokio::test]
async fn test_structure_guests_hangar_view_and_directory_parity() {
    // parity: rpc:structureDirectory.GetJumpBridgesWithMyAccess, rpc:structureDirectory.GetNearbyJumpBridges, rpc:structureDirectory.GetStructureDescription, rpc:structureDirectory.SetStructureDescription, rpc:structureDirectory.clear_memoized, rpc:structureGuests.GetGuests, rpc:structureHangarViewMgr.GetMyHangarViewState
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 201, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 202, "other", "other-secret", PILOT2).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.corporation_id = CORP_CALDARI_NAVY;
            c1.station_id = None;
            c1.structure_id = None;
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.corporation_id = CORP_CALDARI_NAVY;
            c2.station_id = None;
            c2.structure_id = None;
        }
    });
    sess1.corporation_id = Some(CORP_CALDARI_NAVY);
    sess2.corporation_id = Some(CORP_CALDARI_NAVY);
    sess1.station_id = None;
    sess1.structure_id = None;
    sess2.station_id = None;
    sess2.structure_id = None;

    let structure_id = 10_000_000_002_i64;
    seed_test_structure(
        &ctx,
        structure_id,
        ASTRAHUS_TYPE_ID,
        CORP_CALDARI_NAVY,
        SYS_JITA,
        "Naval Defense Citadel",
        "ShieldVulnerable",
    );

    let g_init = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await.expect("structureGuests.GetGuests initially empty");
    let PyRep::List(g_list0) = g_init else { panic!("expected List"); };
    assert!(g_list0.is_empty(), "No guests should be docked yet");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.structure_id = Some(structure_id);
        }
    });
    sess1.structure_id = Some(structure_id);

    let g_p1 = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await.expect("structureGuests.GetGuests after pilot1 docks");
    let PyRep::List(g_list1) = g_p1 else { panic!("expected List"); };
    assert_eq!(g_list1.len(), 1);
    assert_eq!(g_list1[0], PyRep::Tuple(vec![PyRep::Integer(PILOT1), PyRep::Integer(CORP_CALDARI_NAVY), PyRep::None]));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT2) {
            ch.structure_id = Some(structure_id);
        }
    });
    sess2.structure_id = Some(structure_id);

    let g_both = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await.expect("structureGuests.GetGuests after pilot2 docks");
    let PyRep::List(g_list2) = g_both else { panic!("expected List"); };
    assert_eq!(g_list2.len(), 2);
    let PyRep::Tuple(ref t0) = g_list2[0] else { panic!("expected Tuple") };
    let PyRep::Tuple(ref t1) = g_list2[1] else { panic!("expected Tuple") };
    assert_eq!(t0.first(), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(t1.first(), Some(&PyRep::Integer(PILOT2)));

    let ship_id = 30_000_000_001_i64;
    seed_test_item(&ctx, ship_id, 587, PILOT1, structure_id, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = ship_id;
        }
    });
    sess1.ship_id = Some(ship_id);

    let h_view = rpc(&ctx, &mut sess1, "structureHangarViewMgr", "GetMyHangarViewState", &[]).await.expect("GetMyHangarViewState while docked");
    assert_eq!(h_view.dict_get("characterID"), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(h_view.dict_get("structureID"), Some(&PyRep::Integer(structure_id)));
    assert_eq!(h_view.dict_get("structureTypeID"), Some(&PyRep::Integer(ASTRAHUS_TYPE_ID as i64)));
    assert_eq!(h_view.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(h_view.dict_get("shipTypeID"), Some(&PyRep::Integer(587)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.structure_id = None;
        }
    });
    sess1.structure_id = None;
    let not_docked_err = rpc(&ctx, &mut sess1, "structureHangarViewMgr", "GetMyHangarViewState", &[]).await.expect_err("GetMyHangarViewState when undocked must fail");
    assert_eq!(not_docked_err.code, "NotDockedInStructure");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.structure_id = Some(structure_id);
        }
    });
    sess1.structure_id = Some(structure_id);

    let desc_init = rpc(&ctx, &mut sess1, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(structure_id)]).await.expect("GetStructureDescription initial");
    assert_eq!(desc_init, PyRep::None);

    let set_desc_res = rpc(&ctx, &mut sess1, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Caldari Navy Operational Staging Hub".into())]).await.expect("SetStructureDescription by corp member");
    assert_eq!(set_desc_res, PyRep::None);

    let desc_after = rpc(&ctx, &mut sess1, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(structure_id)]).await.expect("GetStructureDescription after update");
    assert_eq!(desc_after, PyRep::String("Caldari Navy Operational Staging Hub".into()));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT2) {
            ch.corporation_id = CORP_OTHER;
        }
    });
    sess2.corporation_id = Some(CORP_OTHER);
    let foreign_err = rpc(&ctx, &mut sess2, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Unauthorized graffiti".into())]).await.expect_err("SetStructureDescription by non-corp member must fail");
    assert_eq!(foreign_err.code, "StructureOwnershipViolation");

    let jb1_id = 10_000_000_003_i64;
    let jb2_id = 10_000_000_004_i64;
    seed_test_structure(&ctx, jb1_id, ANSIBLEX_TYPE_ID, CORP_CALDARI_NAVY, SYS_JITA, "Ansiblex Gate >> Perimeter", "ShieldVulnerable");
    seed_test_structure(&ctx, jb2_id, ANSIBLEX_TYPE_ID, CORP_CALDARI_NAVY, SYS_DEST, "Ansiblex Gate >> Jita", "ShieldVulnerable");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb1_id, jb2_id);
        ext.jump_bridge_links.insert(jb2_id, jb1_id);
        save_ext_state(snap, &ext);
    });

    let jb_list = rpc(&ctx, &mut sess1, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.expect("GetJumpBridgesWithMyAccess");
    let PyRep::List(jb_entries) = jb_list else { panic!("expected List"); };
    assert!(!jb_entries.is_empty(), "Expected accessible jump bridges in list");
    let jb1_entry = jb_entries.iter().find(|e| e.dict_get("structureID") == Some(&PyRep::Integer(jb1_id))).expect("jb1_id in jump bridges list");
    assert_eq!(jb1_entry.dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(jb1_entry.dict_get("linkedStructureID"), Some(&PyRep::Integer(jb2_id)));
    assert_eq!(jb1_entry.dict_get("destinationSolarSystemID"), Some(&PyRep::Integer(SYS_DEST as i64)));

    let nearby_jb = rpc(&ctx, &mut sess1, "structureDirectory", "GetNearbyJumpBridges", &[]).await.expect("GetNearbyJumpBridges");
    let PyRep::List(nearby_entries) = nearby_jb else { panic!("expected List"); };
    assert_eq!(nearby_entries.len(), jb_entries.len());

    let dir_clear = rpc(&ctx, &mut sess1, "structureDirectory", "clear_memoized", &[]).await.expect("structureDirectory.clear_memoized");
    assert_eq!(dir_clear, PyRep::None);
}

#[tokio::test]
async fn test_pos_mgr_parity() {
    // parity: rpc:posMgr.AnchorOrbital, rpc:posMgr.AnchorStructure, rpc:posMgr.GetControlTowers, rpc:posMgr.GetMoonForTower, rpc:posMgr.GetMoonProcessInfoForTower, rpc:posMgr.OnlineOrbital, rpc:posMgr.SetSessionCheck
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 401, "demo", "demo-secret", PILOT1).await;

    let sc_res = rpc(&ctx, &mut sess, "posMgr", "SetSessionCheck", &[]).await.expect("posMgr.SetSessionCheck");
    assert_eq!(sc_res, PyRep::Bool(true));

    let towers_res = rpc(&ctx, &mut sess, "posMgr", "GetControlTowers", &[]).await.expect("posMgr.GetControlTowers");
    assert_eq!(towers_res, PyRep::List(Vec::new()));

    let moon_res = rpc(&ctx, &mut sess, "posMgr", "GetMoonForTower", &[PyRep::Integer(1001)]).await.expect("posMgr.GetMoonForTower");
    assert_eq!(moon_res, PyRep::None);

    let proc_res = rpc(&ctx, &mut sess, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(1001)]).await.expect("posMgr.GetMoonProcessInfoForTower");
    assert_eq!(proc_res, PyRep::None);

    let tower_item_id = 50_000_000_001_i64;
    seed_test_item(&ctx, tower_item_id, CONTROL_TOWER_TYPE_ID, PILOT1, STATION_JITA, InventoryFlag::Hangar, 1);
    let starbase_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(tower_item_id)]).await.expect_err("Legacy POS starbase anchoring must be refused");
    assert_eq!(starbase_err.code, "StarbaseRemoved");
    assert!(starbase_err.message.contains("Legacy POS starbases and control towers are superseded"));

    let missing_item_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(999_999_999)]).await.expect_err("Nonexistent item for AnchorStructure must return StarbaseRemoved");
    assert_eq!(missing_item_err.code, "StarbaseRemoved");

    let bubble_item_id = 50_000_000_002_i64;
    let ship_item_id = 50_000_000_003_i64;
    seed_test_item(&ctx, ship_item_id, 587, PILOT1, SYS_JITA as i64, InventoryFlag::None, 1);
    seed_test_item(&ctx, bubble_item_id, 26888, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT1).or_default();
        sk.insert(11584, TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        sk.insert(3435, TrainedSkillRecord { type_id: 3435, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_item_id;
            ch.solar_system_id = SYS_JITA;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(SYS_JITA);
    sess.ship_id = Some(ship_item_id);

    let empire_bubble_err = rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_item_id)]).await.expect_err("Mobile Warp Disruptor anchoring in empire must fail");
    assert_eq!(empire_bubble_err.code, "CantAnchorInEmpire");

    let gantry_hs_id = 50_000_000_004_i64;
    seed_test_item(&ctx, gantry_hs_id, GANTRY_TYPE_ID, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);
    let hs_gantry_err = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_hs_id), PyRep::Integer(JITA_PLANET_ID)]).await.expect_err("Anchoring Gantry in high-sec must fail");
    assert_eq!(hs_gantry_err.code, "CannotAnchorOrbitalInHighSec");

    let gantry_ls_id = 50_000_000_005_i64;
    seed_test_item(&ctx, gantry_ls_id, GANTRY_TYPE_ID, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);
    sess.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
        }
    });

    let gantry_res = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect("Anchor Customs Office Gantry at low-sec planet");
    assert_eq!(gantry_res.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_ls_id)));
    assert_eq!(gantry_res.dict_get("planetID"), Some(&PyRep::Integer(LOWSEC_PLANET_ID)));
    assert_eq!(gantry_res.dict_get("typeID"), Some(&PyRep::Integer(GANTRY_TYPE_ID as i64)));
    assert_eq!(gantry_res.dict_get("state"), Some(&PyRep::String("Anchored".into())));

    let gantry_dup_id = 50_000_000_006_i64;
    seed_test_item(&ctx, gantry_dup_id, GANTRY_TYPE_ID, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);
    let dup_err = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_dup_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect_err("Duplicate orbital on same planet must fail");
    assert_eq!(dup_err.code, "PlanetAlreadyHasOrbital");

    let non_orbital_id = 50_000_000_007_i64;
    seed_test_item(&ctx, non_orbital_id, 587, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);
    let non_orb_err = rpc(&ctx, &mut sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(non_orbital_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect_err("Non-orbital item for AnchorOrbital must fail");
    assert_eq!(non_orb_err.code, "StarbaseRemoved");

    let missing_upgrade_err = rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect_err("OnlineOrbital without upgrade material must fail");
    assert_eq!(missing_upgrade_err.code, "CustomsOfficeItemRequired");

    let poco_upgrade_id = 50_000_000_008_i64;
    seed_test_item(&ctx, poco_upgrade_id, POCO_TYPE_ID, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);

    let online_res = rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect("Upgrade Gantry to POCO via OnlineOrbital");
    assert_eq!(online_res.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_ls_id)));
    assert_eq!(online_res.dict_get("planetID"), Some(&PyRep::Integer(LOWSEC_PLANET_ID)));
    assert_eq!(online_res.dict_get("typeID"), Some(&PyRep::Integer(POCO_TYPE_ID as i64)));
    assert_eq!(online_res.dict_get("state"), Some(&PyRep::String("ShieldVulnerable".into())));

    assert!(ctx.db.get_item(poco_upgrade_id).is_none(), "Upgrade material item must be consumed");

    let poco_struct = ctx.db.with_snapshot(|snap| snap.structures.get(&gantry_ls_id).cloned()).expect("POCO in DB");
    assert_eq!(poco_struct.type_id, POCO_TYPE_ID);
    assert_eq!(poco_struct.state, "ShieldVulnerable");
    assert_eq!(poco_struct.shield_hp, 10_000_000.0);
}
