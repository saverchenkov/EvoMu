//! Integration tests covering open RPC parity rows across structure services (`structureSettings`,
//! `structureGuests`, `structureHangarViewMgr`, `structureDirectory`), dogmaIM, and POS manager (`posMgr`).

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
const STANDUP_MARKET_TYPE_ID: u32 = 35892;
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
async fn test_structure_settings_parity() {
    // parity: rpc:structureSettings.CharacterCheckService, rpc:structureSettings.CharacterGetService, rpc:structureSettings.CharacterGetServices, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterHasSetting, rpc:structureSettings.clear_memoized
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 101, "demo", "demo-secret", PILOT1).await;
    let structure_id = 10_000_000_001_i64;

    seed_test_structure(
        &ctx,
        structure_id,
        ASTRAHUS_TYPE_ID,
        CORP_CALDARI_NAVY,
        SYS_JITA,
        "Perimeter Trade Free-Market",
        "ShieldVulnerable",
    );

    // 1. Initial state: structure has no online service modules
    let s_empty = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetServices",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("CharacterGetServices empty structure");
    let PyRep::Dict(entries) = s_empty else {
        panic!("expected Dict");
    };
    assert!(entries.is_empty(), "Unfitted structure must report 0 services");

    let has_svc = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasService");
    assert_eq!(has_svc, PyRep::Bool(false));

    let check_svc = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterCheckService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterCheckService");
    assert_eq!(check_svc, PyRep::Bool(false));

    let get_svc = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterGetService");
    assert_eq!(get_svc, PyRep::Bool(false));

    let has_setting = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasSetting",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasSetting");
    assert_eq!(has_setting, PyRep::Bool(false));

    // 2. Install and online Standup Market Hub I (service_id = 1, type_id = 35892)
    let module_item_id = 20_000_000_001_i64;
    seed_test_item(
        &ctx,
        module_item_id,
        STANDUP_MARKET_TYPE_ID,
        PILOT1,
        structure_id,
        InventoryFlag::ServiceSlot0,
        1,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(module_item_id, true);
    });

    // 3. Verify services are now reported and active
    let s_active = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetServices",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("CharacterGetServices with market hub");
    let PyRep::Dict(active_entries) = s_active else {
        panic!("expected Dict");
    };
    assert_eq!(
        active_entries.len(),
        2,
        "Expected mapped service ID + type ID entries in Dict"
    );
    assert_eq!(
        active_entries
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(1))
            .map(|(_, v)| v),
        Some(&PyRep::Integer(1))
    );
    assert_eq!(
        active_entries
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(STANDUP_MARKET_TYPE_ID as i64))
            .map(|(_, v)| v),
        Some(&PyRep::Integer(1))
    );

    let has_market = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasService for market service ID");
    assert_eq!(has_market, PyRep::Bool(true));

    let has_market_tid = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[
            PyRep::Integer(structure_id),
            PyRep::Integer(STANDUP_MARKET_TYPE_ID as i64),
        ],
    )
    .await
    .expect("CharacterHasService for market type ID");
    assert_eq!(has_market_tid, PyRep::Bool(true));

    let has_clone_bay = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(structure_id), PyRep::Integer(2)],
    )
    .await
    .expect("CharacterHasService for uninstalled clone bay");
    assert_eq!(has_clone_bay, PyRep::Bool(false));

    let check_market = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterCheckService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterCheckService");
    assert_eq!(check_market, PyRep::Bool(true));

    let get_market = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterGetService");
    assert_eq!(get_market, PyRep::Bool(true));

    let has_setting_ok = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasSetting",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasSetting");
    assert_eq!(has_setting_ok, PyRep::Bool(true));

    // 4. Test docked convenience shorthand (session.structure_id provides structure ID)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.structure_id = Some(structure_id);
            ch.station_id = None;
        }
    });
    sess.structure_id = Some(structure_id);
    sess.station_id = None;
    let docked_has = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasService while docked");
    assert_eq!(docked_has, PyRep::Bool(true));

    let docked_has_missing = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(99)],
    )
    .await
    .expect("CharacterHasService missing while docked");
    assert_eq!(docked_has_missing, PyRep::Bool(false));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.structure_id = None;
        }
    });
    sess.structure_id = None;

    // 5. Test non-operational structure behavior
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "AnchorVulnerable".to_string();
        }
    });
    let non_op_services = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetServices",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("CharacterGetServices non-operational");
    let PyRep::Dict(non_op_entries) = non_op_services else {
        panic!("expected Dict");
    };
    assert!(non_op_entries.is_empty(), "Non-operational structure must report 0 active services");

    let non_op_has = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasService non-operational");
    assert_eq!(non_op_has, PyRep::Bool(false));

    // 6. Test clear_memoized cache invalidation notification
    let clear_res = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "clear_memoized",
        &[],
    )
    .await
    .expect("structureSettings.clear_memoized");
    assert_eq!(clear_res, PyRep::None);
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

    // 1. structureGuests.GetGuests: empty initially
    let g_init = rpc(
        &ctx,
        &mut sess1,
        "structureGuests",
        "GetGuests",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("structureGuests.GetGuests initially empty");
    let PyRep::List(g_list0) = g_init else {
        panic!("expected List");
    };
    assert!(g_list0.is_empty(), "No guests should be docked yet");

    // Dock pilot 1 into structure
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.structure_id = Some(structure_id);
        }
    });
    sess1.structure_id = Some(structure_id);

    let g_p1 = rpc(
        &ctx,
        &mut sess1,
        "structureGuests",
        "GetGuests",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("structureGuests.GetGuests after pilot1 docks");
    let PyRep::List(g_list1) = g_p1 else {
        panic!("expected List");
    };
    assert_eq!(g_list1.len(), 1);
    assert_eq!(
        g_list1[0],
        PyRep::Tuple(vec![
            PyRep::Integer(PILOT1),
            PyRep::Integer(CORP_CALDARI_NAVY),
            PyRep::None,
        ])
    );

    // Dock pilot 2 as well
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT2) {
            ch.structure_id = Some(structure_id);
        }
    });
    sess2.structure_id = Some(structure_id);

    let g_both = rpc(
        &ctx,
        &mut sess1,
        "structureGuests",
        "GetGuests",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("structureGuests.GetGuests after pilot2 docks");
    let PyRep::List(g_list2) = g_both else {
        panic!("expected List");
    };
    assert_eq!(g_list2.len(), 2);
    let PyRep::Tuple(ref t0) = g_list2[0] else { panic!("expected Tuple") };
    let PyRep::Tuple(ref t1) = g_list2[1] else { panic!("expected Tuple") };
    assert_eq!(t0.first(), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(t1.first(), Some(&PyRep::Integer(PILOT2)));

    // 2. structureHangarViewMgr.GetMyHangarViewState
    let ship_id = 30_000_000_001_i64;
    seed_test_item(
        &ctx,
        ship_id,
        587, // Rifter
        PILOT1,
        structure_id,
        InventoryFlag::Hangar,
        1,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = ship_id;
        }
    });
    sess1.ship_id = Some(ship_id);

    let h_view = rpc(
        &ctx,
        &mut sess1,
        "structureHangarViewMgr",
        "GetMyHangarViewState",
        &[],
    )
    .await
    .expect("GetMyHangarViewState while docked");
    assert_eq!(h_view.dict_get("characterID"), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(h_view.dict_get("structureID"), Some(&PyRep::Integer(structure_id)));
    assert_eq!(h_view.dict_get("structureTypeID"), Some(&PyRep::Integer(ASTRAHUS_TYPE_ID as i64)));
    assert_eq!(h_view.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(h_view.dict_get("shipTypeID"), Some(&PyRep::Integer(587)));

    // Undock and verify GetMyHangarViewState fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.structure_id = None;
        }
    });
    sess1.structure_id = None;
    let not_docked_err = rpc(
        &ctx,
        &mut sess1,
        "structureHangarViewMgr",
        "GetMyHangarViewState",
        &[],
    )
    .await
    .expect_err("GetMyHangarViewState when undocked must fail");
    assert_eq!(not_docked_err.code, "NotDockedInStructure");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.structure_id = Some(structure_id);
        }
    });
    sess1.structure_id = Some(structure_id);

    // 3. structureDirectory.SetStructureDescription & GetStructureDescription
    let desc_init = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "GetStructureDescription",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("GetStructureDescription initial");
    assert_eq!(desc_init, PyRep::None);

    let set_desc_res = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "SetStructureDescription",
        &[
            PyRep::Integer(structure_id),
            PyRep::String("Caldari Navy Operational Staging Hub".into()),
        ],
    )
    .await
    .expect("SetStructureDescription by corp member");
    assert_eq!(set_desc_res, PyRep::None);

    let desc_after = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "GetStructureDescription",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("GetStructureDescription after update");
    assert_eq!(
        desc_after,
        PyRep::String("Caldari Navy Operational Staging Hub".into())
    );

    // Non-corp member cannot change description
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT2) {
            ch.corporation_id = CORP_OTHER;
        }
    });
    sess2.corporation_id = Some(CORP_OTHER);
    let foreign_err = rpc(
        &ctx,
        &mut sess2,
        "structureDirectory",
        "SetStructureDescription",
        &[
            PyRep::Integer(structure_id),
            PyRep::String("Unauthorized graffiti".into()),
        ],
    )
    .await
    .expect_err("SetStructureDescription by non-corp member must fail");
    assert_eq!(foreign_err.code, "StructureOwnershipViolation");

    // 4. structureDirectory.GetJumpBridgesWithMyAccess & GetNearbyJumpBridges
    let jb1_id = 10_000_000_003_i64;
    let jb2_id = 10_000_000_004_i64;
    seed_test_structure(
        &ctx,
        jb1_id,
        ANSIBLEX_TYPE_ID,
        CORP_CALDARI_NAVY,
        SYS_JITA,
        "Ansiblex Gate >> Perimeter",
        "ShieldVulnerable",
    );
    seed_test_structure(
        &ctx,
        jb2_id,
        ANSIBLEX_TYPE_ID,
        CORP_CALDARI_NAVY,
        SYS_DEST,
        "Ansiblex Gate >> Jita",
        "ShieldVulnerable",
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb1_id, jb2_id);
        ext.jump_bridge_links.insert(jb2_id, jb1_id);
        save_ext_state(snap, &ext);
    });

    let jb_list = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        &[],
    )
    .await
    .expect("GetJumpBridgesWithMyAccess");
    let PyRep::List(jb_entries) = jb_list else {
        panic!("expected List");
    };
    assert!(!jb_entries.is_empty(), "Expected accessible jump bridges in list");
    let jb1_entry = jb_entries
        .iter()
        .find(|e| e.dict_get("structureID") == Some(&PyRep::Integer(jb1_id)))
        .expect("jb1_id in jump bridges list");
    assert_eq!(
        jb1_entry.dict_get("solarSystemID"),
        Some(&PyRep::Integer(SYS_JITA as i64))
    );
    assert_eq!(
        jb1_entry.dict_get("linkedStructureID"),
        Some(&PyRep::Integer(jb2_id))
    );
    assert_eq!(
        jb1_entry.dict_get("destinationSolarSystemID"),
        Some(&PyRep::Integer(SYS_DEST as i64))
    );

    let nearby_jb = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "GetNearbyJumpBridges",
        &[],
    )
    .await
    .expect("GetNearbyJumpBridges");
    let PyRep::List(nearby_entries) = nearby_jb else {
        panic!("expected List");
    };
    assert_eq!(nearby_entries.len(), jb_entries.len());

    // 5. structureDirectory.clear_memoized
    let dir_clear = rpc(
        &ctx,
        &mut sess1,
        "structureDirectory",
        "clear_memoized",
        &[],
    )
    .await
    .expect("structureDirectory.clear_memoized");
    assert_eq!(dir_clear, PyRep::None);
}

#[tokio::test]
async fn test_dogma_im_parity() {
    // parity: rpc:dogmaIM.CreateNewbieShip, rpc:dogmaIM.GetRequiredSkillLevels, rpc:dogmaIM.SetSessionCheck
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 301, "demo", "demo-secret", PILOT1).await;

    // 1. dogmaIM.SetSessionCheck on authenticated session
    let sc_ok = rpc(&ctx, &mut sess, "dogmaIM", "SetSessionCheck", &[])
        .await
        .expect("dogmaIM.SetSessionCheck authenticated");
    assert_eq!(sc_ok, PyRep::Bool(true));

    let mut unauth_sess = SessionState::new(302);
    let sc_err = rpc(&ctx, &mut unauth_sess, "dogmaIM", "SetSessionCheck", &[])
        .await
        .expect_err("dogmaIM.SetSessionCheck unauthenticated must fail");
    assert_eq!(sc_err.code, "Unauthorized");

    // 2. dogmaIM.GetRequiredSkillLevels
    // Query by ship typeID 587 (Rifter)
    let rifter_skills = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "GetRequiredSkillLevels",
        &[PyRep::Integer(587)],
    )
    .await
    .expect("GetRequiredSkillLevels for Rifter");
    assert_eq!(rifter_skills.dict_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(
        rifter_skills.dict_get("typeName"),
        Some(&PyRep::String("Rifter".into()))
    );
    assert_eq!(rifter_skills.dict_get("groupID"), Some(&PyRep::Integer(25))); // Frigate
    let PyRep::Dict(req_skills) = rifter_skills
        .dict_get("requiredSkills")
        .expect("requiredSkills dict")
    else {
        panic!("expected Dict");
    };
    // Minmatar Frigate skill is type 3329, required level 1
    assert_eq!(
        req_skills
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(3329))
            .map(|(_, v)| v),
        Some(&PyRep::Integer(1))
    );

    // Query by existing itemID in inventory
    let test_item_id = 40_000_000_001_i64;
    seed_test_item(
        &ctx,
        test_item_id,
        587,
        PILOT1,
        STATION_JITA,
        InventoryFlag::Hangar,
        1,
    );
    let item_skills = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "GetRequiredSkillLevels",
        &[PyRep::Integer(test_item_id)],
    )
    .await
    .expect("GetRequiredSkillLevels by itemID");
    assert_eq!(item_skills.dict_get("typeID"), Some(&PyRep::Integer(587)));

    // Query non-existent type ID
    let missing_type_err = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "GetRequiredSkillLevels",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect_err("GetRequiredSkillLevels missing type must fail");
    assert_eq!(missing_type_err.code, "TypeNotFound");

    // 3. dogmaIM.CreateNewbieShip
    // Initial conditions: docked at station, active ship is capsule (type 670), no other ships in station
    sess.station_id = Some(STATION_JITA);
    let capsule_item_id = 40_000_000_002_i64;
    seed_test_item(
        &ctx,
        capsule_item_id,
        670, // Capsule (group 29)
        PILOT1,
        STATION_JITA,
        InventoryFlag::Hangar,
        1,
    );
    ctx.db.with_mut_snapshot(|snap| {
        // Clear non-capsule ships owned by PILOT1 at this station
        snap.items.retain(|_, it| {
            !(it.owner_id == PILOT1
                && it.location_id == STATION_JITA
                && it.type_id != 670
                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 6))
        });
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = capsule_item_id;
            ch.race_id = 1; // Caldari -> Ibis (type 601)
        }
    });
    sess.ship_id = Some(capsule_item_id);

    let new_ship_res = rpc(&ctx, &mut sess, "dogmaIM", "CreateNewbieShip", &[])
        .await
        .expect("CreateNewbieShip when docked in capsule with no ships");
    let new_ship_id = new_ship_res
        .dict_get("shipID")
        .and_then(|v| v.as_i64())
        .expect("new shipID");
    assert_eq!(new_ship_res.dict_get("typeID"), Some(&PyRep::Integer(601))); // Ibis
    assert_eq!(new_ship_res.dict_get("raceID"), Some(&PyRep::Integer(1)));
    assert_eq!(sess.ship_id, Some(new_ship_id));

    let created_item = ctx.db.get_item(new_ship_id).expect("new corvette in DB");
    assert_eq!(created_item.type_id, 601);
    assert_eq!(created_item.owner_id, PILOT1);
    assert_eq!(created_item.location_id, STATION_JITA);

    // Call again immediately: cooldown active
    let cooldown_err = rpc(&ctx, &mut sess, "dogmaIM", "CreateNewbieShip", &[])
        .await
        .expect_err("Immediate second CreateNewbieShip must hit cooldown");
    assert_eq!(cooldown_err.code, "NewbieShipCooldown");

    // Call while not in capsule
    let claim_key = -8_800_000 - PILOT1;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&claim_key);
    });
    let not_in_capsule_err = rpc(&ctx, &mut sess, "dogmaIM", "CreateNewbieShip", &[])
        .await
        .expect_err("CreateNewbieShip while in corvette must fail");
    assert_eq!(not_in_capsule_err.code, "NotInCapsule");

    // Call while not docked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    let not_docked_err = rpc(&ctx, &mut sess, "dogmaIM", "CreateNewbieShip", &[])
        .await
        .expect_err("CreateNewbieShip while undocked must fail");
    assert_eq!(not_docked_err.code, "NotDocked");
}

#[tokio::test]
async fn test_pos_mgr_parity() {
    // parity: rpc:posMgr.AnchorOrbital, rpc:posMgr.AnchorStructure, rpc:posMgr.GetControlTowers, rpc:posMgr.GetMoonForTower, rpc:posMgr.GetMoonProcessInfoForTower, rpc:posMgr.OnlineOrbital, rpc:posMgr.SetSessionCheck
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 401, "demo", "demo-secret", PILOT1).await;

    // 1. posMgr.SetSessionCheck
    let sc_res = rpc(&ctx, &mut sess, "posMgr", "SetSessionCheck", &[])
        .await
        .expect("posMgr.SetSessionCheck");
    assert_eq!(sc_res, PyRep::Bool(true));

    // 2. posMgr.GetControlTowers (superseded, returns empty list)
    let towers_res = rpc(&ctx, &mut sess, "posMgr", "GetControlTowers", &[])
        .await
        .expect("posMgr.GetControlTowers");
    assert_eq!(towers_res, PyRep::List(Vec::new()));

    // 3. posMgr.GetMoonForTower and GetMoonProcessInfoForTower (superseded, return None)
    let moon_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetMoonForTower",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("posMgr.GetMoonForTower");
    assert_eq!(moon_res, PyRep::None);

    let proc_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "GetMoonProcessInfoForTower",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("posMgr.GetMoonProcessInfoForTower");
    assert_eq!(proc_res, PyRep::None);

    // 4. posMgr.AnchorStructure: legacy POS starbase refusal vs deployables
    let tower_item_id = 50_000_000_001_i64;
    seed_test_item(
        &ctx,
        tower_item_id,
        CONTROL_TOWER_TYPE_ID, // Caldari Control Tower (group 365, category 23)
        PILOT1,
        STATION_JITA,
        InventoryFlag::Hangar,
        1,
    );
    let starbase_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(tower_item_id)],
    )
    .await
    .expect_err("Legacy POS starbase anchoring must be refused");
    assert_eq!(starbase_err.code, "StarbaseRemoved");
    assert!(
        starbase_err
            .message
            .contains("Legacy POS starbases and control towers are superseded"),
        "Error message must cite Upwell supersession"
    );

    // Non-existent item ID also triggers StarbaseRemoved
    let missing_item_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect_err("Nonexistent item for AnchorStructure must return StarbaseRemoved");
    assert_eq!(missing_item_err.code, "StarbaseRemoved");

    // Deployable: Mobile Small Warp Disruptor I (type 26888, group 361) in high-sec empire
    let bubble_item_id = 50_000_000_002_i64;
    let ship_item_id = 50_000_000_003_i64;
    seed_test_item(
        &ctx,
        ship_item_id,
        587,
        PILOT1,
        SYS_JITA as i64,
        InventoryFlag::None,
        1,
    );
    seed_test_item(
        &ctx,
        bubble_item_id,
        26888,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT1).or_default();
        sk.insert(
            11584,
            TrainedSkillRecord {
                type_id: 11584,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3435,
            TrainedSkillRecord {
                type_id: 3435,
                level: 5,
                skill_points: 256_000.0,
            },
        );
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

    let empire_bubble_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(bubble_item_id)],
    )
    .await
    .expect_err("Mobile Warp Disruptor anchoring in empire must fail");
    assert_eq!(empire_bubble_err.code, "CantAnchorInEmpire");

    // 5. posMgr.AnchorOrbital: Customs Office Gantry (type 3962)
    // High-sec check
    let gantry_hs_id = 50_000_000_004_i64;
    seed_test_item(
        &ctx,
        gantry_hs_id,
        GANTRY_TYPE_ID,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );
    let hs_gantry_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(gantry_hs_id), PyRep::Integer(JITA_PLANET_ID)],
    )
    .await
    .expect_err("Anchoring Gantry in high-sec must fail");
    assert_eq!(hs_gantry_err.code, "CannotAnchorOrbitalInHighSec");

    // Low-sec anchoring
    let gantry_ls_id = 50_000_000_005_i64;
    seed_test_item(
        &ctx,
        gantry_ls_id,
        GANTRY_TYPE_ID,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );
    sess.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
        }
    });

    let gantry_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)],
    )
    .await
    .expect("Anchor Customs Office Gantry at low-sec planet");
    assert_eq!(gantry_res.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_ls_id)));
    assert_eq!(gantry_res.dict_get("planetID"), Some(&PyRep::Integer(LOWSEC_PLANET_ID)));
    assert_eq!(gantry_res.dict_get("typeID"), Some(&PyRep::Integer(GANTRY_TYPE_ID as i64)));
    assert_eq!(gantry_res.dict_get("state"), Some(&PyRep::String("Anchored".into())));

    // Anchoring second orbital on same planet fails
    let gantry_dup_id = 50_000_000_006_i64;
    seed_test_item(
        &ctx,
        gantry_dup_id,
        GANTRY_TYPE_ID,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );
    let dup_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(gantry_dup_id), PyRep::Integer(LOWSEC_PLANET_ID)],
    )
    .await
    .expect_err("Duplicate orbital on same planet must fail");
    assert_eq!(dup_err.code, "PlanetAlreadyHasOrbital");

    // Non-orbital item falls through to legacy StarbaseRemoved
    let non_orbital_id = 50_000_000_007_i64;
    seed_test_item(
        &ctx,
        non_orbital_id,
        587,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );
    let non_orb_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(non_orbital_id), PyRep::Integer(LOWSEC_PLANET_ID)],
    )
    .await
    .expect_err("Non-orbital item for AnchorOrbital must fail");
    assert_eq!(non_orb_err.code, "StarbaseRemoved");

    // 6. posMgr.OnlineOrbital
    // Missing upgrade item (Customs Office type 2233)
    let missing_upgrade_err = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)],
    )
    .await
    .expect_err("OnlineOrbital without upgrade material must fail");
    assert_eq!(missing_upgrade_err.code, "CustomsOfficeItemRequired");

    // Seed upgrade material in inventory
    let poco_upgrade_id = 50_000_000_008_i64;
    seed_test_item(
        &ctx,
        poco_upgrade_id,
        POCO_TYPE_ID,
        PILOT1,
        ship_item_id,
        InventoryFlag::Cargo,
        1,
    );

    let online_res = rpc(
        &ctx,
        &mut sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)],
    )
    .await
    .expect("Upgrade Gantry to POCO via OnlineOrbital");
    assert_eq!(online_res.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_ls_id)));
    assert_eq!(online_res.dict_get("planetID"), Some(&PyRep::Integer(LOWSEC_PLANET_ID)));
    assert_eq!(online_res.dict_get("typeID"), Some(&PyRep::Integer(POCO_TYPE_ID as i64)));
    assert_eq!(online_res.dict_get("state"), Some(&PyRep::String("ShieldVulnerable".into())));

    // Upgrade material was consumed
    assert!(
        ctx.db.get_item(poco_upgrade_id).is_none(),
        "Upgrade material item must be consumed"
    );

    // Structure in DB transitioned to POCO
    let poco_struct = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&gantry_ls_id).cloned())
        .expect("POCO in DB");
    assert_eq!(poco_struct.type_id, POCO_TYPE_ID);
    assert_eq!(poco_struct.state, "ShieldVulnerable");
    assert_eq!(poco_struct.shield_hp, 10_000_000.0);
}
