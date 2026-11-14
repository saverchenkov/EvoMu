//! Integration tests covering open RPC parity rows across structure services (`structureSettings`,
//! `structureGuests`, `structureHangarViewMgr`, `structureDirectory`), dogmaIM, and POS manager (`posMgr`).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const CORP_CALDARI_NAVY: i64 = 1_000_035;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;

const ASTRAHUS_TYPE_ID: u32 = 35832;
const STANDUP_MARKET_TYPE_ID: u32 = 35892;

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
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
}

#[tokio::test]
async fn test_structure_settings_parity() {
    // parity: rpc:structureSettings.CharacterCheckService, rpc:structureSettings.CharacterGetService, rpc:structureSettings.CharacterGetServices, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterHasSetting, rpc:structureSettings.clear_memoized, rpc:structureDeployment.Unanchor, rpc:structureDeployment.CancelUnanchor, rpc:structureDeployment.RenameStructure, rpc:structureVulnerability.UpdateReinforceTimesFromClient, rpc:structureDirectory.SetStructureDescription
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
    .await;
    assert_eq!(check_svc.unwrap_err().code, "StructureServiceAccessDenied");

    let get_svc = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterGetService",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterGetService");
    assert_eq!(get_svc, PyRep::None);

    let has_setting = rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "CharacterHasSetting",
        &[PyRep::Integer(structure_id), PyRep::Integer(1)],
    )
    .await
    .expect("CharacterHasSetting");
    assert_eq!(has_setting, PyRep::Bool(true));

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

    // 3. Verify services are now reported and active (no module typeIDs as keys)
    let s_active = rpc(&ctx, &mut sess, "structureSettings", "CharacterGetServices", &[PyRep::Integer(structure_id)])
        .await
        .expect("CharacterGetServices with market hub");
    let PyRep::Dict(active_entries) = s_active else { panic!("expected Dict"); };
    assert_eq!(active_entries.len(), 1, "Expected only mapped service ID in Dict (no module typeIDs)");
    assert_eq!(active_entries.iter().find(|(k, _)| k == &PyRep::Integer(5)).map(|(_, v)| v), Some(&PyRep::Real(0.01)));

    let has_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterHasService for market service ID");
    assert_eq!(has_market, PyRep::Bool(true));

    let check_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterCheckService");
    assert_eq!(check_market, PyRep::None);

    let get_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterGetService");
    assert_eq!(get_market, PyRep::Real(0.01));

    // Update profile setting via structureProfiles & ownerGroupManager RPCs and verify corp role requirement + basis-point tax validation
    use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) { c1.corporation_id = CORP_CALDARI_NAVY; }
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.corporation_id = 1_000_172;
            c2.alliance_id = Some(99_000_077);
            c2.station_id = None;
        }
        snap.alliances.insert(
            99_000_077,
            eve_db::AllianceRecord {
                alliance_id: 99_000_077,
                name: "Test Rival Alliance".into(),
                short_name: "RIVL".into(),
                executor_corp_id: 1_000_172,
                member_corp_ids: std::collections::HashSet::from([1_000_172]),
            },
        );
        let mut ext = load_ext_state(snap);
        ext.descriptions.insert(structure_id, "Preserved description".into());
        save_ext_state(snap, &ext);
    });

    // Line member without Director/Station Manager role is denied on profile, ACL, and structure management RPCs
    sess.corp_role = 0;
    let profile_settings_2_5 = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.025)])]);
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(structure_id), profile_settings_2_5.clone()]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Bool(true)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(structure_id)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(structure_id)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(structure_id), PyRep::String("Unauthorized Rename".into())]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(structure_id), PyRep::Integer(12), PyRep::Integer(3)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Unauthorized Bio".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

    // NaN, infinity, and negative tax rates must be rejected via RPC and never wipe extended state
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(structure_id), PyRep::String("Caldari Navy Hub".into())]).await.expect("RenameStructure with Director");
    rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(structure_id), PyRep::Integer(18), PyRep::Integer(5)]).await.expect("UpdateReinforceTimesFromClient with Director");
    rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Preserved description".into())]).await.expect("SetStructureDescription with Director");
    rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(structure_id)]).await.expect("Unanchor with Director");
    sess.corp_role = 0;
    assert_eq!(rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(structure_id)]).await.unwrap_err().code, "InsufficientCorpRoles", "Zero-role member cannot cancel active unanchor");
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(structure_id)]).await.expect("CancelUnanchor with Director");
    for bad_tax in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5, 1.5] {
        let bad_settings = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(bad_tax)])]);
        let tax_err = rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(structure_id), bad_settings]).await;
        assert_eq!(tax_err.unwrap_err().code, "InvalidStructureTaxRate");
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| load_ext_state(s).descriptions.get(&structure_id).cloned()),
        Some("Preserved description".to_string()),
        "Rejected NaN/inf tax rate must never corrupt or wipe StructureExtendedState"
    );

    // Standalone ownerGroupManager.Create + structureSettings.CreateProfile / UpdateProfile / SaveProfileSettings / ChangeProfiles
    let grp_kv = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Citadel ACL".into()), PyRep::String("Main ACL".into()), PyRep::Bool(true)]).await.expect("ownerGroupManager.Create");
    let group_id = grp_kv.dict_get("groupID").and_then(|v| v.as_i64()).expect("groupID");
    assert!(group_id >= 80_000_001);
    let profile_id = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Trade Profile".into()), PyRep::String("Desc".into())]).await.expect("CreateProfile").as_i64().expect("profileID");
    assert!(profile_id >= 70_000_001);
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "UpdateProfile", &[PyRep::Integer(profile_id), PyRep::String("Updated Trade Profile".into()), PyRep::String("Updated Desc".into())]).await.expect("UpdateProfile"), PyRep::Bool(true));
    let mut sess_blocked = setup_pilot(&ctx, 102, "other", "other-secret", PILOT2).await;
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(PILOT1)]), PyRep::Integer(2)]).await.expect("allow pilot1 in group");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(group_id), PyRep::Real(0.025)]), PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(group_id), PyRep::Real(0.0)])])]).await.expect("SaveProfileSettings on profile_id"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(structure_id)]), PyRep::Integer(profile_id)]).await.expect("ChangeProfiles"), PyRep::Bool(true));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_ext_state(s).access_profiles.get(&structure_id).and_then(|p| p.service_settings.get(&4)).copied()),
        Some(250),
        "2.5% tax rate assigned via standalone profile must be stored as 250 basis points (i64)"
    );

    let get_market_updated = rpc(&ctx, &mut sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterGetService updated");
    assert_eq!(get_market_updated, PyRep::Real(0.025));

    // Allowed vs blocked character/alliance test via structureSettings & ownerGroupManager RPCs
    let foreign_save = rpc(&ctx, &mut sess_blocked, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Real(0.05)])])]).await;
    assert_eq!(foreign_save.unwrap_err().code, "StructureOwnershipViolation");

    // Atomic batch rollback: valid PILOT2 + invalid 888_777_666 must fail and leave PILOT2 unblocked
    let bad_batch = rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(PILOT2), PyRep::Integer(888_777_666)]), PyRep::Bool(true)]).await;
    assert_eq!(bad_batch.unwrap_err().code, "InvalidAccessEntity");
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(5)]).await.expect("PILOT1 still has service after failed batch"), PyRep::Bool(true));

    // Blocking PILOT2's alliance (99_000_077) via standalone group_id blocks PILOT2 from structure services, docking, jump bridge, and tether
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(true)]).await.expect("block alliance in group");
    let ally_blocked_check = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(5)]).await;
    assert_eq!(ally_blocked_check.unwrap_err().code, "StructureServiceAccessDenied");
    let jita_sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_sys.spawn_ball(eve_destiny::Ball {
        id: 100_000_002, type_id: 587, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO,
        max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await;
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, PILOT2, SYS_JITA, 100_000_002).await;
    assert!(jita_sys.get_combat_state(100_000_002).await.ok().flatten().map(|c| !c.is_tethered).unwrap_or(true), "ACL-blocked pilot must not be tethered");
    sess_blocked.station_id = None;
    sess_blocked.solar_system_id = Some(SYS_JITA);
    sess_blocked.ship_id = Some(100_000_002);
    ctx.advance_secs(12);
    let ally_dock_err = rpc(&ctx, &mut sess_blocked, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(ally_dock_err.unwrap_err().code, "StructureDockingAccessDenied");
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&structure_id).unwrap().type_id = 35_841);
    let ally_jb_err = rpc(&ctx, &mut sess_blocked, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(ally_jb_err.unwrap_err().code, "AccessDenied", "Blocked alliance must be denied Ansiblex jump bridge usage");
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(true)]).await.expect("unblock alliance in group");
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077), PyRep::Integer(PILOT2)]), PyRep::Bool(false)]).await.expect("allow alliance and pilot2 in group");
    ctx.advance_secs(12);
    let ally_jb_allowed = rpc(&ctx, &mut sess_blocked, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(ally_jb_allowed.unwrap_err().code, "NoLinkedStructure", "Allowed alliance must pass Ansiblex ACL check");
    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&structure_id).unwrap().type_id = ASTRAHUS_TYPE_ID);
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(false)]).await.expect("remove allow alliance in group");

    // Blocking PILOT2 directly via character ID
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Bool(true)]).await.expect("block character");

    let blocked_check = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(5)]).await;
    assert_eq!(blocked_check.unwrap_err().code, "StructureServiceAccessDenied");

    let blocked_has = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterHasService blocked");
    assert_eq!(blocked_has, PyRep::Bool(false));

    let blocked_get = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterGetService blocked");
    assert_eq!(blocked_get, PyRep::None);

    let blocked_services = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterGetServices", &[PyRep::Integer(structure_id)])
        .await
        .expect("CharacterGetServices blocked");
    let PyRep::Dict(b_entries) = blocked_services else { panic!("expected Dict"); };
    assert!(b_entries.is_empty(), "Blocked character must receive empty services dict");

    let blocked_setting = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(structure_id), PyRep::Integer(4)])
        .await
        .expect("CharacterHasSetting blocked");
    assert_eq!(blocked_setting, PyRep::Bool(false));

    // Removing PILOT2 from blocked_characters restores access
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Bool(true)]).await.expect("unblock character");
    let unblocked_has = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(5)])
        .await
        .expect("CharacterHasService after unblock");
    assert_eq!(unblocked_has, PyRep::Bool(true));

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
        &[PyRep::Integer(5)],
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

    // 6. Test clear_memoized (client-side memoized cache invalidation -> None)
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
async fn test_dogma_im_parity() {
    // parity: rpc:dogmaIM.CreateNewbieShip, rpc:dogmaIM.GetRequiredSkillLevels, rpc:dogmaIM.SetSessionCheck, rpc:stationSvc.GetStation
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 301, "demo", "demo-secret", PILOT1).await;

    let st_info = rpc(&ctx, &mut sess, "stationSvc", "GetStation", &[PyRep::Integer(STATION_JITA)]).await.expect("stationSvc.GetStation");
    assert_eq!(st_info.dict_get("stationID"), Some(&PyRep::Integer(STATION_JITA)));
    assert_eq!(st_info.dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(SYS_JITA))));
    assert_eq!(st_info.dict_get("stationTypeID"), Some(&PyRep::Integer(52678)));

    // 1. dogmaIM.SetSessionCheck returns verified session-check metadata
    let sc_rep = rpc(&ctx, &mut sess, "dogmaIM", "SetSessionCheck", &[])
        .await
        .expect("dogmaIM.SetSessionCheck authenticated");
    assert_eq!(sc_rep.dict_get("valid"), Some(&PyRep::Bool(true)));
    assert_eq!(sc_rep.dict_get("characterID"), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(sc_rep.dict_get("checkCount"), Some(&PyRep::Integer(1)));

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

    // Call while already in corvette (even after cooldown expires) -> AlreadyInNewbieShip
    let claim_key = -8_800_000 - PILOT1;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&claim_key);
    });
    let already_corv_err = rpc(&ctx, &mut sess, "dogmaIM", "CreateNewbieShip", &[])
        .await
        .expect_err("CreateNewbieShip while in corvette must fail");
    assert_eq!(already_corv_err.code, "AlreadyInNewbieShip");

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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_structures_pos_dogma_ext_b {
    use super::*;

// Part 2 of integration tests covering structure directory, guests, hangar view, and POS manager (`posMgr`).

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
                last_damage_unix: 0, self_destruct_at_unix: None,
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
    let PyRep::Dict(g_map0) = g_init else { panic!("expected Dict, got {g_init:?}"); };
    assert!(g_map0.is_empty(), "No guests should be docked yet");

    let g_none = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::None]).await.expect("structureGuests.GetGuests(None) when undocked");
    assert_eq!(g_none, PyRep::Dict(Vec::new()));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.structure_id = Some(structure_id);
        }
    });
    sess1.structure_id = Some(structure_id);

    let g_p1 = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await.expect("structureGuests.GetGuests after pilot1 docks");
    let PyRep::Dict(g_map1) = g_p1 else { panic!("expected Dict, got {g_p1:?}"); };
    assert_eq!(g_map1.len(), 1);
    assert_eq!(
        g_map1[0],
        (
            PyRep::Integer(PILOT1),
            PyRep::Tuple(vec![PyRep::Integer(CORP_CALDARI_NAVY), PyRep::None, PyRep::None]),
        )
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT2) {
            ch.structure_id = Some(structure_id);
            ch.war_faction_id = Some(500_001);
        }
    });
    sess2.structure_id = Some(structure_id);

    let g_both = rpc(&ctx, &mut sess1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await.expect("structureGuests.GetGuests after pilot2 docks");
    let PyRep::Dict(g_map2) = g_both else { panic!("expected Dict, got {g_both:?}"); };
    assert_eq!(g_map2.len(), 2);
    assert_eq!(g_map2[0].0, PyRep::Integer(PILOT1));
    assert_eq!(
        g_map2[1],
        (
            PyRep::Integer(PILOT2),
            PyRep::Tuple(vec![PyRep::Integer(CORP_CALDARI_NAVY), PyRep::None, PyRep::Integer(500_001)]),
        )
    );

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

    sess1.corp_role = 0;
    let role_err = rpc(&ctx, &mut sess1, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Unauthorized line-member bio".into())]).await.expect_err("SetStructureDescription by zero-role corp member must fail");
    assert_eq!(role_err.code, "InsufficientCorpRoles");

    sess1.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
    let set_desc_res = rpc(&ctx, &mut sess1, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(structure_id), PyRep::String("Caldari Navy Operational Staging Hub".into())]).await.expect("SetStructureDescription by Station Manager");
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

    let jb_res = rpc(&ctx, &mut sess1, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await.expect("GetJumpBridgesWithMyAccess");
    let PyRep::Tuple(jb_tup) = jb_res else { panic!("expected 3-tuple"); };
    assert_eq!(jb_tup.len(), 3);
    let PyRep::List(jb_entries) = &jb_tup[0] else { panic!("expected pairs List at tuple[0]"); };
    let PyRep::List(jb_accessible) = &jb_tup[1] else { panic!("expected accessible IDs List at tuple[1]"); };
    assert!(!jb_entries.is_empty(), "Expected accessible jump bridges in list");
    assert!(jb_accessible.contains(&PyRep::Integer(jb1_id)));
    let jb1_pair = jb_entries.iter().find_map(|e| match e {
        PyRep::Tuple(pair) if pair.len() == 2 && pair[0].dict_get("structureID") == Some(&PyRep::Integer(jb1_id)) => Some(pair),
        _ => None,
    }).expect("jb1_id in jump bridges pairs");
    assert_eq!(jb1_pair[0].dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(jb1_pair[1].dict_get("structureID"), Some(&PyRep::Integer(jb2_id)));
    assert_eq!(jb1_pair[1].dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_DEST as i64)));

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
    let orbital_corp_id = 98_000_777_i64;
    seed_test_item(&ctx, gantry_ls_id, GANTRY_TYPE_ID, PILOT1, ship_item_id, InventoryFlag::Cargo, 1);
    sess.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
    sess.corporation_id = Some(orbital_corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
            ch.corporation_id = orbital_corp_id;
        }
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = orbital_corp_id;
        corp.name = "Orbital Test Corp".into();
        corp.ticker = "ORBT".into();
        corp.ceo_id = 90_000_999;
        corp.members.insert(PILOT1);
        snap.corporations.insert(orbital_corp_id, corp);
    });
    let ls_planet = ctx
        .sde
        .celestials_in_system(LOWSEC_SOLAR_SYSTEM_ID)
        .into_iter()
        .find(|c| c.celestial_id == LOWSEC_PLANET_ID)
        .expect("lowsec planet");
    let ls_sys = ctx.systems.get_or_spawn(LOWSEC_SOLAR_SYSTEM_ID);
    ls_sys
        .spawn_ball(eve_destiny::Ball {
            id: ship_item_id,
            type_id: 587,
            position: ls_planet.position + glam::DVec3::new(ls_planet.radius + 5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn lowsec ship ball");

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

    sess.corp_role = 0;
    let no_role_err = rpc(&ctx, &mut sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)]).await.expect_err("OnlineOrbital without Director/StationManager role must fail");
    assert_eq!(no_role_err.code, "CrpAccessDenied");
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

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

#[tokio::test]
async fn test_wire_upwell_dock_session_guests_dict_and_undock_left_notify_gh1048_1049_1050() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.corporation_id = CORP_CALDARI_NAVY;
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.corporation_id = CORP_CALDARI_NAVY;
        }
    });

    let kw = HashMap::new();
    let mut harness1 = common::new_wire_harness(&ctx, 104_801);
    harness1
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    harness1
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT1)],
            &kw,
        )
        .await;

    let mut harness2 = common::new_wire_harness(&ctx, 104_802);
    harness2
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("other".into()), PyRep::String("other-secret".into())],
            &kw,
        )
        .await;
    harness2
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT2)],
            &kw,
        )
        .await;

    ctx.advance_secs(12);
    harness1.call_ok("stationSvc", None, "Undock", &[], &kw).await;

    let ship1_id = ctx.db.with_snapshot(|s| s.characters[&PILOT1].active_ship_id);
    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let (_, ship1_ball) = sys_handle
        .get_ball_snapshot(ship1_id)
        .await
        .expect("get_ball_snapshot")
        .expect("undocked ship1 ball");

    let structure_id = 1_040_000_010_480_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: ASTRAHUS_TYPE_ID,
                owner_corp_id: CORP_CALDARI_NAVY,
                solar_system_id: SYS_JITA,
                name: "Wire Parity Astrahus".into(),
                position: ship1_ball.position.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.station_id = None;
            c2.structure_id = Some(structure_id);
        }
    });

    let extract_sc_dict = |pkt_rep: &PyRep| -> HashMap<String, PyRep> {
        let pkt = eve_machonet::MachoPacket::from_pyrep(pkt_rep)
            .expect("SessionChangeNotification MachoPacket");
        let PyRep::Tuple(outer) = pkt.payload else {
            panic!("expected Tuple payload, got {:?}", pkt.payload);
        };
        let PyRep::Tuple(inner) = &outer[1] else {
            panic!("expected inner tuple, got {:?}", outer[1]);
        };
        let PyRep::Dict(entries) = &inner[1] else {
            panic!("expected Dict changes, got {:?}", inner[1]);
        };
        entries
            .iter()
            .filter_map(|(k, v)| k.as_str().map(|ks| (ks.to_string(), v.clone())))
            .collect()
    };

    // #1048: Dock in Upwell structure keeps solarsystemid and locationid intact in wire SessionChangeNotification.
    ctx.advance_secs(12);
    let dock_out = harness1
        .call_exchange(
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(structure_id)],
            &kw,
        )
        .await;
    assert!(dock_out.result.is_ok(), "structureDocking.Dock must succeed: {:?}", dock_out.result);
    assert_eq!(dock_out.session_change_packets.len(), 1);
    let dock_changes = extract_sc_dict(&dock_out.session_change_packets[0]);
    assert_eq!(
        dock_changes.get("structureid"),
        Some(&PyRep::Tuple(vec![PyRep::None, PyRep::Integer(structure_id)]))
    );
    assert!(
        !dock_changes.contains_key("solarsystemid"),
        "Upwell structure dock must NOT clear solarsystemid (#1048): {dock_changes:?}"
    );
    assert!(
        !dock_changes.contains_key("locationid"),
        "Upwell structure dock must NOT change locationid away from solarsystemid (#1048): {dock_changes:?}"
    );

    // #1049: structureGuests.GetGuests returns dict {charID: (corpID, allianceID, warFactionID)} with zero contract violations.
    let guests_out = harness1
        .call_exchange(
            "structureGuests",
            None,
            "GetGuests",
            &[PyRep::Integer(structure_id)],
            &kw,
        )
        .await;
    assert!(guests_out.violations.is_empty(), "GetGuests contract violations: {:?}", guests_out.violations);
    let guests_rep = guests_out.result.expect("structureGuests.GetGuests must succeed");
    let PyRep::Dict(guests_map) = guests_rep else {
        panic!("expected PyRep::Dict from GetGuests (#1049), got {guests_rep:?}");
    };
    assert_eq!(guests_map.len(), 2);
    assert!(guests_map.iter().any(|(k, v)| {
        *k == PyRep::Integer(PILOT1)
            && *v == PyRep::Tuple(vec![PyRep::Integer(CORP_CALDARI_NAVY), PyRep::None, PyRep::None])
    }));
    assert!(guests_map.iter().any(|(k, _)| *k == PyRep::Integer(PILOT2)));

    // #1050: Undock clears structureid while preserving solarsystemid, emits OnCharacterLeftStructure to peer ONLY, and GetGuests(None) returns empty dict.
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let _ = ctx.drain_notifications_for_char(PILOT2);
    ctx.advance_secs(12);
    let undock_out = harness1
        .call_exchange("structureDocking", None, "Undock", &[], &kw)
        .await;
    assert!(undock_out.result.is_ok(), "structureDocking.Undock must succeed: {:?}", undock_out.result);
    assert_eq!(undock_out.session_change_packets.len(), 1);
    let undock_changes = extract_sc_dict(&undock_out.session_change_packets[0]);
    assert_eq!(
        undock_changes.get("structureid"),
        Some(&PyRep::Tuple(vec![PyRep::Integer(structure_id), PyRep::None]))
    );
    assert!(
        !undock_changes.contains_key("solarsystemid"),
        "Upwell structure undock must NOT emit stale solarsystemid diff (#1048): {undock_changes:?}"
    );
    assert!(
        !undock_out
            .notifications
            .iter()
            .any(|(ev, _)| ev == "OnCharacterLeftStructure"),
        "Undocking pilot must NOT receive OnCharacterLeftStructure (#1050): {:?}",
        undock_out.notifications
    );
    let peer_notifs = ctx.drain_notifications_for_char(PILOT2);
    assert!(
        peer_notifs
            .iter()
            .any(|n| n.event == "OnCharacterLeftStructure" && n.args == vec![PyRep::Integer(PILOT1)]),
        "Remaining structure peer must receive OnCharacterLeftStructure(PILOT1) (#1050): {peer_notifs:?}"
    );

    let guests_none = harness1
        .call_ok("structureGuests", None, "GetGuests", &[PyRep::None], &kw)
        .await;
    assert_eq!(
        guests_none,
        PyRep::Dict(Vec::new()),
        "GetGuests(None) while undocked must return empty Dict (#1050)"
    );
}

}
