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
    assert_eq!(active_entries.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v), Some(&PyRep::Real(0.01)));

    let has_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterHasService for market service ID");
    assert_eq!(has_market, PyRep::Bool(true));

    let check_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterCheckService");
    assert_eq!(check_market, PyRep::None);

    let get_market = rpc(&ctx, &mut sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
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
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(profile_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(group_id), PyRep::Real(0.025)])])]).await.expect("SaveProfileSettings on profile_id"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(structure_id)]), PyRep::Integer(profile_id)]).await.expect("ChangeProfiles"), PyRep::Bool(true));
    assert_eq!(
        ctx.db.with_snapshot(|s| load_ext_state(s).access_profiles.get(&structure_id).and_then(|p| p.service_settings.get(&1)).copied()),
        Some(250),
        "2.5% tax rate assigned via standalone profile must be stored as 250 basis points (i64)"
    );

    let get_market_updated = rpc(&ctx, &mut sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterGetService updated");
    assert_eq!(get_market_updated, PyRep::Real(0.025));

    // Allowed vs blocked character/alliance test via structureSettings & ownerGroupManager RPCs
    let mut sess_blocked = setup_pilot(&ctx, 102, "other", "other-secret", PILOT2).await;
    let foreign_save = rpc(&ctx, &mut sess_blocked, "structureSettings", "SaveProfileSettings", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.05)])])]).await;
    assert_eq!(foreign_save.unwrap_err().code, "StructureOwnershipViolation");

    // Atomic batch rollback: valid PILOT2 + invalid 888_777_666 must fail and leave PILOT2 unblocked
    let bad_batch = rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(PILOT2), PyRep::Integer(888_777_666)]), PyRep::Bool(true)]).await;
    assert_eq!(bad_batch.unwrap_err().code, "InvalidAccessEntity");
    assert_eq!(rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await.expect("PILOT2 still has service after failed batch"), PyRep::Bool(true));

    // Blocking PILOT2's alliance (99_000_077) via standalone group_id blocks PILOT2 from structure services, docking, jump bridge, and tether
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(true)]).await.expect("block alliance in group");
    let ally_blocked_check = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await;
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
    let ally_jb_err = rpc(&ctx, &mut sess_blocked, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(ally_jb_err.unwrap_err().code, "AccessDenied", "Blocked alliance must be denied Ansiblex jump bridge usage");
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(true)]).await.expect("unblock alliance in group");
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(false)]).await.expect("allow alliance in group");
    ctx.advance_secs(12);
    let ally_jb_allowed = rpc(&ctx, &mut sess_blocked, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(structure_id)]).await;
    assert_eq!(ally_jb_allowed.unwrap_err().code, "NoLinkedStructure", "Allowed alliance must pass Ansiblex ACL check");
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(99_000_077)]), PyRep::Bool(false)]).await.expect("remove allow alliance in group");

    // Blocking PILOT2 directly via character ID
    rpc(&ctx, &mut sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Bool(true)]).await.expect("block character");

    let blocked_check = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await;
    assert_eq!(blocked_check.unwrap_err().code, "StructureServiceAccessDenied");

    let blocked_has = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterHasService blocked");
    assert_eq!(blocked_has, PyRep::Bool(false));

    let blocked_get = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterGetService blocked");
    assert_eq!(blocked_get, PyRep::None);

    let blocked_services = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterGetServices", &[PyRep::Integer(structure_id)])
        .await
        .expect("CharacterGetServices blocked");
    let PyRep::Dict(b_entries) = blocked_services else { panic!("expected Dict"); };
    assert!(b_entries.is_empty(), "Blocked character must receive empty services dict");

    let blocked_setting = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
        .await
        .expect("CharacterHasSetting blocked");
    assert_eq!(blocked_setting, PyRep::Bool(false));

    // Removing PILOT2 from blocked_characters restores access
    rpc(&ctx, &mut sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Bool(true)]).await.expect("unblock character");
    let unblocked_has = rpc(&ctx, &mut sess_blocked, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(1)])
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
async fn test_dogma_im_parity() {
    // parity: rpc:dogmaIM.CreateNewbieShip, rpc:dogmaIM.GetRequiredSkillLevels, rpc:dogmaIM.SetSessionCheck, rpc:stationSvc.GetStation
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 301, "demo", "demo-secret", PILOT1).await;

    let st_info = rpc(&ctx, &mut sess, "stationSvc", "GetStation", &[PyRep::Integer(STATION_JITA)]).await.expect("stationSvc.GetStation");
    assert_eq!(st_info.dict_get("stationID"), Some(&PyRep::Integer(STATION_JITA)));
    assert_eq!(st_info.dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(SYS_JITA))));
    assert_eq!(st_info.dict_get("stationTypeID"), Some(&PyRep::Integer(52678)));

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
