//! Acceptance tests for Classic machoNet RPC methods closure.
//!
//! Validates:
//! - `billMgr` (personal & corporation bills, automatic pay settings)
//! - `notificationMgr` (notification inbox, processing, groups, batch deletion)
//! - `jumpCloneSvc` (station & structure jump clone installation, validation, naming, jumping, destroy)
//! - `survey` (CSAT player surveys & rewards)
//! - `eventLog` (client telemetry, autopilot events, disconnect reasons)
//! - `operationsManager` (AIR NPE career operations, tutorial status, task transitions)
//! - `onlineStatus` & `pingService` (presence tracking & cluster blueos time ping)
//! - `overviewPresetMgr` & `sharedSettingsMgr` (overview and filter setting links)
//! - `petitioner` (complete GM and player petition support lifecycle)


use eve_db::{CorporationRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{NotificationRecord, ServerContext};
use std::collections::HashMap;

const P1: i64 = 90_000_001; // Aura Capsuleer (user 1, demo)
const P2: i64 = 90_000_002; // Rival Pilot (user 2, other)
const JITA_44: i64 = 60_003_760;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(username.into()), PyRep::String(password.into())],
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

// ============================================================================
// 1. billMgr Tests
// ============================================================================
#[tokio::test]
async fn test_bill_manager_rpc_suite() {
    // parity: rpc:billMgr.CharPayBill, rpc:billMgr.GetAutomaticPaySettings, rpc:billMgr.GetCorporationBills, rpc:billMgr.GetCorporationBillsReceivable, rpc:billMgr.PayCorporationBill, rpc:billMgr.SendAutomaticPaySettings
    eve_testkit::attest(&[
        "rpc:billMgr.CharPayBill",
        "rpc:billMgr.GetAutomaticPaySettings",
        "rpc:billMgr.GetCorporationBills",
        "rpc:billMgr.GetCorporationBillsReceivable",
        "rpc:billMgr.PayCorporationBill",
        "rpc:billMgr.SendAutomaticPaySettings",
            ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 801, "demo", "demo-secret", P1).await;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    let landlord_corp = 1_000_035_i64; // Caldari Navy
    let now = ctx.now_unix();

    // Setup corp wallet divisions
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(50_000_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.master_wallet = IskCents::from_isk(50_000_000);
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(10_000_000);
        }
    });

    // 1. billMgr.get
    let get_res = rpc(&ctx, &mut s1, "billMgr", "get", &[]).await.expect("billMgr.get");
    assert!(matches!(get_res, PyRep::List(_)));

    // Create a personal bill and a corporation bill using serverops helper
    let p_bill_id = eve_server::services::wallet_serverops::create_bill(
        &ctx,
        1,
        P1,
        landlord_corp,
        2_500.0,
        now + 3600,
        0.0,
        0,
        0,
    );
    let c_bill_id = eve_server::services::wallet_serverops::create_bill(
        &ctx,
        2,
        corp_id,
        landlord_corp,
        10_000.0,
        now + 3600,
        0.0,
        JITA_44,
        0,
    );

    // 2. billMgr.GetCorporationBills
    let bills_rep = rpc(&ctx, &mut s1, "billMgr", "GetCorporationBills", &[])
        .await
        .expect("billMgr.GetCorporationBills");
    let PyRep::Rowset { rows, .. } = bills_rep else {
        panic!("expected Rowset from GetCorporationBills");
    };
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill_id))));
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill_id))));

    // 3. billMgr.GetCorporationBillsReceivable
    let recv_rep = rpc(&ctx, &mut s1, "billMgr", "GetCorporationBillsReceivable", &[])
        .await
        .expect("billMgr.GetCorporationBillsReceivable");
    assert!(matches!(recv_rep, PyRep::Rowset { .. }));

    // 4. billMgr.CharPayBill
    let pay_p = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[PyRep::Integer(p_bill_id)])
        .await
        .expect("billMgr.CharPayBill");
    assert_eq!(pay_p, PyRep::Bool(true));

    // 5. billMgr.PayCorporationBill
    let pay_c = rpc(
        &ctx,
        &mut s1,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(c_bill_id), PyRep::Integer(1002)],
    )
    .await
    .expect("billMgr.PayCorporationBill");
    assert_eq!(pay_c, PyRep::Bool(true));

    // 6. billMgr.SendAutomaticPaySettings & 7. billMgr.GetAutomaticPaySettings
    let auto_dict = PyRep::Dict(vec![
        (PyRep::Integer(2), PyRep::Bool(true)),
        (PyRep::String("divisionID".into()), PyRep::Integer(1004)),
    ]);
    let set_ap = rpc(
        &ctx,
        &mut s1,
        "billMgr",
        "SendAutomaticPaySettings",
        &[PyRep::Dict(vec![(PyRep::Integer(corp_id), auto_dict)])],
    )
    .await
    .expect("billMgr.SendAutomaticPaySettings");
    assert_eq!(set_ap, PyRep::Bool(true));

    let get_ap = rpc(&ctx, &mut s1, "billMgr", "GetAutomaticPaySettings", &[])
        .await
        .expect("billMgr.GetAutomaticPaySettings");
    let PyRep::Dict(ap_entries) = get_ap else {
        panic!("expected Dict from GetAutomaticPaySettings");
    };
    assert!(!ap_entries.is_empty());
}

// ============================================================================
// 2. notificationMgr Tests
// ============================================================================
#[tokio::test]
async fn test_notification_manager_rpc_suite() {
    // parity: rpc:notificationMgr.DeleteAllNotifications, rpc:notificationMgr.DeleteGroupNotifications, rpc:notificationMgr.DeleteNotifications, rpc:notificationMgr.GetAllNotifications, rpc:notificationMgr.GetByGroupID, rpc:notificationMgr.GetUnprocessed, rpc:notificationMgr.LogNotificationInteraction, rpc:notificationMgr.MarkAllAsProcessed, rpc:notificationMgr.MarkAsProcessed, rpc:notificationMgr.MarkGroupAsProcessed
    eve_testkit::attest(&[
        "rpc:notificationMgr.DeleteAllNotifications",
        "rpc:notificationMgr.DeleteGroupNotifications",
        "rpc:notificationMgr.DeleteNotifications",
        "rpc:notificationMgr.GetAllNotifications",
        "rpc:notificationMgr.GetByGroupID",
        "rpc:notificationMgr.GetUnprocessed",
        "rpc:notificationMgr.LogNotificationInteraction",
        "rpc:notificationMgr.MarkAllAsProcessed",
        "rpc:notificationMgr.MarkAsProcessed",
        "rpc:notificationMgr.MarkGroupAsProcessed",
            ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 802, "demo", "demo-secret", P1).await;
    let now = ctx.now_unix();

    // Populate notification records in ctx
    {
        let mut recs = ctx.notification_records.write().unwrap();
        recs.push(NotificationRecord {
            notification_id: 101,
            type_id: 1,
            sender_id: 1000170,
            receiver_id: P1,
            processed: false,
            created: now as i64,
            data: "{\"text\":\"Welcome to EVE\"}".into(),
            deleted: false,
            group_id: 10,
        });
        recs.push(NotificationRecord {
            notification_id: 102,
            type_id: 2,
            sender_id: 1000170,
            receiver_id: P1,
            processed: false,
            created: now as i64,
            data: "{\"text\":\"Corp Bulletin\"}".into(),
            deleted: false,
            group_id: 10,
        });
        recs.push(NotificationRecord {
            notification_id: 103,
            type_id: 3,
            sender_id: 1000170,
            receiver_id: P1,
            processed: false,
            created: now as i64,
            data: "{\"text\":\"Market Order Filled\"}".into(),
            deleted: false,
            group_id: 20,
        });
    }

    // 1. GetAllNotifications
    let all = rpc(&ctx, &mut s1, "notificationMgr", "GetAllNotifications", &[])
        .await
        .expect("GetAllNotifications");
    assert!(matches!(all, PyRep::ObjectEx { .. })); // dbutil.CRowset

    // 2. GetUnprocessed
    let unproc = rpc(&ctx, &mut s1, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    assert!(matches!(unproc, PyRep::ObjectEx { .. }));

    // 3. GetByGroupID
    let grp = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "GetByGroupID",
        &[PyRep::Integer(10)],
    )
    .await
    .expect("GetByGroupID");
    assert!(matches!(grp, PyRep::ObjectEx { .. }));

    // 4. MarkAsProcessed
    let marked = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "MarkAsProcessed",
        &[PyRep::List(vec![PyRep::Integer(101)])],
    )
    .await
    .expect("MarkAsProcessed");
    assert_eq!(marked, PyRep::Integer(1));

    // 5. MarkGroupAsProcessed
    let marked_grp = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "MarkGroupAsProcessed",
        &[PyRep::Integer(10)],
    )
    .await
    .expect("MarkGroupAsProcessed");
    assert_eq!(marked_grp, PyRep::Integer(1));

    // 6. MarkAllAsProcessed
    let marked_all = rpc(&ctx, &mut s1, "notificationMgr", "MarkAllAsProcessed", &[])
        .await
        .expect("MarkAllAsProcessed");
    assert_eq!(marked_all, PyRep::Integer(1));

    // 7. LogNotificationInteraction
    let log_res = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "LogNotificationInteraction",
        &[PyRep::Integer(101), PyRep::String("clicked".into())],
    )
    .await
    .expect("LogNotificationInteraction");
    assert_eq!(log_res, PyRep::Bool(true));

    // 8. iterkeys
    let keys = rpc(&ctx, &mut s1, "notificationMgr", "iterkeys", &[])
        .await
        .expect("iterkeys");
    let PyRep::List(key_list) = keys else {
        panic!("expected List from iterkeys");
    };
    assert_eq!(key_list.len(), 3);

    // 9. DeleteNotifications
    let del_res = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "DeleteNotifications",
        &[PyRep::List(vec![PyRep::Integer(101)])],
    )
    .await
    .expect("DeleteNotifications");
    assert_eq!(del_res, PyRep::Integer(1));

    // 10. DeleteGroupNotifications
    let del_grp = rpc(
        &ctx,
        &mut s1,
        "notificationMgr",
        "DeleteGroupNotifications",
        &[PyRep::Integer(10)],
    )
    .await
    .expect("DeleteGroupNotifications");
    assert_eq!(del_grp, PyRep::Integer(1));

    // 11. DeleteAllNotifications
    let del_all = rpc(&ctx, &mut s1, "notificationMgr", "DeleteAllNotifications", &[])
        .await
        .expect("DeleteAllNotifications");
    assert_eq!(del_all, PyRep::Integer(1));
}

// ============================================================================
// 3. jumpCloneSvc Tests
// ============================================================================
#[tokio::test]
async fn test_jump_clone_svc_rpc_suite() {
    // parity: rpc:jumpCloneSvc.CancelShipCloneInstallation, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.DestroyInstalledClone, rpc:jumpCloneSvc.GetCloneState, rpc:jumpCloneSvc.GetNumClonesInPilotsStructure, rpc:jumpCloneSvc.GetPriceForClone, rpc:jumpCloneSvc.GetStationCloneState, rpc:jumpCloneSvc.InstallCloneInStation, rpc:jumpCloneSvc.InstallCloneInStructure, rpc:jumpCloneSvc.SetJumpCloneName, rpc:jumpCloneSvc.ValidateInstallJumpClone
    eve_testkit::attest(&[
        "rpc:jumpCloneSvc.CancelShipCloneInstallation",
        "rpc:jumpCloneSvc.CloneJump",
        "rpc:jumpCloneSvc.DestroyInstalledClone",
        "rpc:jumpCloneSvc.GetCloneState",
        "rpc:jumpCloneSvc.GetNumClonesInPilotsStructure",
        "rpc:jumpCloneSvc.GetPriceForClone",
        "rpc:jumpCloneSvc.GetStationCloneState",
        "rpc:jumpCloneSvc.InstallCloneInStation",
        "rpc:jumpCloneSvc.InstallCloneInStructure",
        "rpc:jumpCloneSvc.SetJumpCloneName",
        "rpc:jumpCloneSvc.ValidateInstallJumpClone",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 803, "demo", "demo-secret", P1).await;

    // Train Infomorph Psychology V and Advanced Infomorph Psychology V
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(P1).or_default();
        skills.insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            33407,
            eve_db::TrainedSkillRecord {
                type_id: 33407,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. ValidateInstallJumpClone
    let val_res = rpc(&ctx, &mut s1, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect("ValidateInstallJumpClone");
    assert_eq!(val_res, PyRep::Bool(true));

    // 2. GetPriceForClone
    let price_res = rpc(&ctx, &mut s1, "jumpCloneSvc", "GetPriceForClone", &[])
        .await
        .expect("GetPriceForClone");
    assert!(matches!(price_res, PyRep::Real(_) | PyRep::Integer(_)));

    // 3. InstallCloneInStation
    let install_res = rpc(&ctx, &mut s1, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect("InstallCloneInStation");
    let clone_id = match install_res {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv
            .get("jumpCloneID")
            .and_then(|v| v.as_i64())
            .expect("jumpCloneID"),
        PyRep::Rowset { rows, .. } => rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_i64())
            .expect("first row clone id"),
        other => panic!("unexpected return from InstallCloneInStation: {other:?}"),
    };
    assert!(clone_id > 0);

    // 4. SetJumpCloneName
    let set_name = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(clone_id), PyRep::String("Home Jita".into())],
    )
    .await
    .expect("SetJumpCloneName");
    assert_eq!(set_name, PyRep::None);

    // 5. GetCloneState
    let state_res = rpc(&ctx, &mut s1, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState");
    let clone_rows = match &state_res {
        PyRep::Rowset { rows, .. } => rows.clone(),
        PyRep::KeyVal(map) => match map.get("clones") {
            Some(PyRep::Rowset { rows, .. }) => rows.clone(),
            _ => panic!("expected rowset in clones"),
        },
        _ => panic!("expected Rowset or KeyVal from GetCloneState"),
    };
    assert!(clone_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(clone_id))));

    // 6. GetStationCloneState
    let station_state = rpc(&ctx, &mut s1, "jumpCloneSvc", "GetStationCloneState", &[])
        .await
        .expect("GetStationCloneState");
    assert!(matches!(station_state, PyRep::Rowset { .. }));

    // 7. CancelShipCloneInstallation
    let cancel_res = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "CancelShipCloneInstallation",
        &[],
    )
    .await
    .expect("CancelShipCloneInstallation");
    assert_eq!(cancel_res, PyRep::Bool(true));

    // 8. GetNumClonesInPilotsStructure
    let struct_clones = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "GetNumClonesInPilotsStructure",
        &[PyRep::Integer(JITA_44)],
    )
    .await
    .expect("GetNumClonesInPilotsStructure");
    assert!(matches!(struct_clones, PyRep::Integer(_)));

    // Setup an Upwell structure with active cloning center
    let struct_id = 1_000_000_100_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                owner_corp_id: 1000170,
                solar_system_id: 30000142,
                name: "Freeport Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.items.insert(
            9901,
            eve_db::InventoryItemRecord {
                item_id: 9901,
                type_id: 35894, // Standup Cloning Center I
                owner_id: 1000170,
                location_id: struct_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
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
        snap.module_online_states.insert(9901, true);
    });

    // Dock pilot in structure
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.structure_id = Some(struct_id);
        ch.station_id = None;
    });

    // 9. InstallCloneInStructure
    let struct_install = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "InstallCloneInStructure",
        &[PyRep::Integer(struct_id)],
    )
    .await
    .expect("InstallCloneInStructure");
    let struct_clone_id = match struct_install {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv
            .get("jumpCloneID")
            .and_then(|v| v.as_i64())
            .expect("jumpCloneID"),
        PyRep::Rowset { rows, .. } => rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_i64())
            .expect("first row clone id"),
        other => panic!("unexpected return from InstallCloneInStructure: {other:?}"),
    };
    assert!(struct_clone_id > 0);

    // Dock pilot back in station for CloneJump
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&P1).unwrap();
        ch.station_id = Some(JITA_44);
        ch.structure_id = None;
    });

    // 10. CloneJump
    let jump_res = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "CloneJump",
        &[PyRep::Integer(JITA_44), PyRep::Integer(clone_id)],
    )
    .await
    .expect("CloneJump");
    assert!(matches!(jump_res, PyRep::KeyVal(_) | PyRep::Bool(true) | PyRep::None | PyRep::Rowset { .. }));

    // 11. DestroyInstalledClone
    let destroy_res = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(struct_clone_id)],
    )
    .await
    .expect("DestroyInstalledClone");
    assert!(matches!(destroy_res, PyRep::Bool(true) | PyRep::None));
}

// ============================================================================
// 4. survey & eventLog Tests
// ============================================================================
#[tokio::test]
async fn test_survey_and_telemetry_rpc_suite() {
    // parity: rpc:survey.ClaimSurveyRewards, rpc:survey.GetActiveSurveyID, rpc:survey.PerformSurveyChecks, rpc:eventLog.LogClientEvent, rpc:eventLog.LogClientStats, rpc:eventLog.LogPlayerRequestedDisconnect
    eve_testkit::attest(&[
        "rpc:survey.ClaimSurveyRewards",
        "rpc:survey.GetActiveSurveyID",
        "rpc:survey.PerformSurveyChecks",
        "rpc:eventLog.LogClientEvent",
        "rpc:eventLog.LogClientStats",
        "rpc:eventLog.LogPlayerRequestedDisconnect",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 804, "demo", "demo-secret", P1).await;

    // survey.GetActiveSurveyID
    let active_survey = rpc(&ctx, &mut s1, "survey", "GetActiveSurveyID", &[])
        .await
        .expect("GetActiveSurveyID");
    assert_eq!(active_survey, PyRep::None);

    // survey.PerformSurveyChecks
    let checks = rpc(&ctx, &mut s1, "survey", "PerformSurveyChecks", &[])
        .await
        .expect("PerformSurveyChecks");
    assert_eq!(checks, PyRep::List(Vec::new()));

    // survey.ClaimSurveyRewards
    let sp_before = ctx.db.with_snapshot(|s| s.characters[&P1].free_skill_points);
    let claim = rpc(
        &ctx,
        &mut s1,
        "survey",
        "ClaimSurveyRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("ClaimSurveyRewards");
    assert_eq!(claim, PyRep::Bool(true));
    let sp_after = ctx.db.with_snapshot(|s| s.characters[&P1].free_skill_points);
    assert_eq!(sp_after - sp_before, 10_000.0);

    // eventLog.LogClientEvent
    let log_event = rpc(
        &ctx,
        &mut s1,
        "eventLog",
        "LogClientEvent",
        &[PyRep::String("AutopilotEngaged".into()), PyRep::Dict(Vec::new())],
    )
    .await
    .expect("LogClientEvent");
    assert_eq!(log_event, PyRep::None);

    // eventLog.LogClientStats
    let log_stats = rpc(
        &ctx,
        &mut s1,
        "eventLog",
        "LogClientStats",
        &[PyRep::Dict(vec![(
            PyRep::String("fps".into()),
            PyRep::Integer(60),
        )])],
    )
    .await
    .expect("LogClientStats");
    assert_eq!(log_stats, PyRep::None);

    // eventLog.LogPlayerRequestedDisconnect
    let log_disc = rpc(
        &ctx,
        &mut s1,
        "eventLog",
        "LogPlayerRequestedDisconnect",
        &[PyRep::String("UserQuit".into())],
    )
    .await
    .expect("LogPlayerRequestedDisconnect");
    assert_eq!(log_disc, PyRep::None);
}

// ============================================================================
// 5. operationsManager Tests
// ============================================================================
#[tokio::test]
async fn test_operations_manager_rpc_suite() {
    // parity: rpc:operationsManager.activate_operation_in_solar_system, rpc:operationsManager.block_task, rpc:operationsManager.complete_category_for_character, rpc:operationsManager.get_active_category_id, rpc:operationsManager.get_category_state, rpc:operationsManager.get_character_progress, rpc:operationsManager.get_mission_avoidance_systems, rpc:operationsManager.get_operations_completed_at_least_once, rpc:operationsManager.get_tutorial_state, rpc:operationsManager.has_skipped_tutorial, rpc:operationsManager.is_category_complete, rpc:operationsManager.is_main_tutorial_finished, rpc:operationsManager.process_client_event, rpc:operationsManager.skip_task, rpc:operationsManager.start_site, rpc:operationsManager.unblock_task
    eve_testkit::attest(&[
        "rpc:operationsManager.activate_operation_in_solar_system",
        "rpc:operationsManager.block_task",
        "rpc:operationsManager.complete_category_for_character",
        "rpc:operationsManager.get_active_category_id",
        "rpc:operationsManager.get_category_state",
        "rpc:operationsManager.get_character_progress",
        "rpc:operationsManager.get_mission_avoidance_systems",
        "rpc:operationsManager.get_operations_completed_at_least_once",
        "rpc:operationsManager.get_tutorial_state",
        "rpc:operationsManager.has_skipped_tutorial",
        "rpc:operationsManager.is_category_complete",
        "rpc:operationsManager.is_main_tutorial_finished",
        "rpc:operationsManager.process_client_event",
        "rpc:operationsManager.skip_task",
        "rpc:operationsManager.start_site",
        "rpc:operationsManager.unblock_task",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 805, "demo", "demo-secret", P1).await;

    // 1. get_tutorial_state
    let tut_state = rpc(&ctx, &mut s1, "operationsManager", "get_tutorial_state", &[])
        .await
        .expect("get_tutorial_state");
    assert!(matches!(tut_state, PyRep::Dict(_)));

    // 2. has_skipped_tutorial
    let skipped = rpc(&ctx, &mut s1, "operationsManager", "has_skipped_tutorial", &[])
        .await
        .expect("has_skipped_tutorial");
    assert_eq!(skipped, PyRep::Bool(false));

    // 3. is_main_tutorial_finished
    let finished = rpc(&ctx, &mut s1, "operationsManager", "is_main_tutorial_finished", &[])
        .await
        .expect("is_main_tutorial_finished");
    assert_eq!(finished, PyRep::Bool(true));

    // 4. get_active_category_id
    let active_cat = rpc(&ctx, &mut s1, "operationsManager", "get_active_category_id", &[])
        .await
        .expect("get_active_category_id");
    assert_eq!(active_cat, PyRep::None);

    // 5. activate_operation_in_solar_system
    let activate = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(101), PyRep::Integer(30000142), PyRep::Bool(false)],
    )
    .await
    .expect("activate_operation_in_solar_system");
    assert_eq!(activate, PyRep::Bool(true));

    // 6. get_category_state
    let cat_state = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "get_category_state",
        &[PyRep::Integer(201)],
    )
    .await
    .expect("get_category_state");
    assert_eq!(cat_state, PyRep::Integer(1)); // AVAILABLE

    // 7. is_category_complete (before)
    let comp_before = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(201)],
    )
    .await
    .expect("is_category_complete before");
    assert_eq!(comp_before, PyRep::Bool(false));

    // 8. complete_category_for_character
    let comp_res = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(201)],
    )
    .await
    .expect("complete_category_for_character");
    assert_eq!(comp_res, PyRep::Bool(true));

    // 9. is_category_complete (after)
    let comp_after = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "is_category_complete",
        &[PyRep::Integer(201)],
    )
    .await
    .expect("is_category_complete after");
    assert_eq!(comp_after, PyRep::Bool(true));

    // 10. get_character_progress
    let prog = rpc(&ctx, &mut s1, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress");
    let PyRep::Dict(entries) = prog else {
        panic!("expected Dict from get_character_progress");
    };
    assert!(entries.iter().any(|(k, _)| k == &PyRep::Integer(201)));

    // 11. get_mission_avoidance_systems
    let avoid = rpc(&ctx, &mut s1, "operationsManager", "get_mission_avoidance_systems", &[])
        .await
        .expect("get_mission_avoidance_systems");
    assert_eq!(avoid, PyRep::List(Vec::new()));

    // 12. get_operations_completed_at_least_once
    let ops_once = rpc(&ctx, &mut s1, "operationsManager", "get_operations_completed_at_least_once", &[])
        .await
        .expect("get_operations_completed_at_least_once");
    assert!(matches!(ops_once, PyRep::List(_)));

    // 13. block_task & 14. unblock_task
    let block = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "block_task",
        &[PyRep::Integer(501)],
    )
    .await
    .expect("block_task");
    assert_eq!(block, PyRep::Bool(true));

    let unblock = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "unblock_task",
        &[PyRep::Integer(501)],
    )
    .await
    .expect("unblock_task");
    assert_eq!(unblock, PyRep::Bool(true));

    // 15. skip_task
    let skip = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "skip_task",
        &[PyRep::Integer(502)],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip, PyRep::Bool(true));

    // 16. start_site & process_client_event
    let site = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(9001)],
    )
    .await
    .expect("start_site");
    assert_eq!(site, PyRep::Bool(true));

    let evt = rpc(
        &ctx,
        &mut s1,
        "operationsManager",
        "process_client_event",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("process_client_event");
    assert_eq!(evt, PyRep::None);
}

// ============================================================================
// 6. onlineStatus, pingService, overviewPresetMgr, sharedSettingsMgr Tests
// ============================================================================
#[tokio::test]
async fn test_online_status_and_ping_rpc_suite() {
    // parity: rpc:onlineStatus.GetInitialState, rpc:onlineStatus.GetOnlineStatus, rpc:overviewPresetMgr.GetStoredPreset, rpc:overviewPresetMgr.StoreLinkAndGetID, rpc:pingService.Ping, rpc:sharedSettingsMgr.GetStoredSharedSetting, rpc:sharedSettingsMgr.StoreSettingLinkAndGetID
    eve_testkit::attest(&[
        "rpc:onlineStatus.GetInitialState",
        "rpc:onlineStatus.GetOnlineStatus",
        "rpc:overviewPresetMgr.GetStoredPreset",
        "rpc:overviewPresetMgr.StoreLinkAndGetID",
        "rpc:pingService.Ping",
        "rpc:sharedSettingsMgr.GetStoredSharedSetting",
        "rpc:sharedSettingsMgr.StoreSettingLinkAndGetID",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 806, "demo", "demo-secret", P1).await;
    let _s2 = setup_pilot(&ctx, 807, "other", "other-secret", P2).await;

    // 1. pingService.Ping
    let ping = rpc(&ctx, &mut s1, "pingService", "Ping", &[])
        .await
        .expect("pingService.Ping");
    let PyRep::Integer(ticks) = ping else {
        panic!("expected Integer filetime from Ping");
    };
    assert!(ticks > 0);

    // 2. onlineStatus.GetOnlineStatus
    let status_res = rpc(
        &ctx,
        &mut s1,
        "onlineStatus",
        "GetOnlineStatus",
        &[PyRep::Integer(P2)],
    )
    .await
    .expect("onlineStatus.GetOnlineStatus");
    assert!(matches!(status_res, PyRep::Bool(_)));

    // 3. onlineStatus.GetInitialState
    let init_state = rpc(&ctx, &mut s1, "onlineStatus", "GetInitialState", &[])
        .await
        .expect("onlineStatus.GetInitialState");
    assert!(matches!(init_state, PyRep::Rowset { .. }));

    // 4. overviewPresetMgr.StoreLinkAndGetID & 5. overviewPresetMgr.GetStoredPreset
    let store_preset = rpc(
        &ctx,
        &mut s1,
        "overviewPresetMgr",
        "StoreLinkAndGetID",
        &[PyRep::String("DefaultPvP".into()), PyRep::String("ships,drones".into())],
    )
    .await
    .expect("overviewPresetMgr.StoreLinkAndGetID");
    let PyRep::Integer(preset_id) = store_preset else {
        panic!("expected Integer link ID from StoreLinkAndGetID");
    };
    assert!(preset_id > 0);

    let get_preset = rpc(
        &ctx,
        &mut s1,
        "overviewPresetMgr",
        "GetStoredPreset",
        &[PyRep::Integer(preset_id)],
    )
    .await
    .expect("overviewPresetMgr.GetStoredPreset");
    assert_eq!(
        get_preset,
        PyRep::Tuple(vec![PyRep::String("DefaultPvP".into()), PyRep::String("ships,drones".into())])
    );

    // 6. sharedSettingsMgr.StoreSettingLinkAndGetID & 7. sharedSettingsMgr.GetStoredSharedSetting
    let store_setting = rpc(
        &ctx,
        &mut s1,
        "sharedSettingsMgr",
        "StoreSettingLinkAndGetID",
        &[PyRep::String("inventoryFilter".into()), PyRep::String("modulesOnly".into())],
    )
    .await
    .expect("sharedSettingsMgr.StoreSettingLinkAndGetID");
    let PyRep::Integer(setting_id) = store_setting else {
        panic!("expected Integer link ID from StoreSettingLinkAndGetID");
    };
    assert!(setting_id > 0);

    let get_setting = rpc(
        &ctx,
        &mut s1,
        "sharedSettingsMgr",
        "GetStoredSharedSetting",
        &[PyRep::Integer(setting_id)],
    )
    .await
    .expect("sharedSettingsMgr.GetStoredSharedSetting");
    assert_eq!(
        get_setting,
        PyRep::Tuple(vec![PyRep::String("inventoryFilter".into()), PyRep::String("modulesOnly".into())])
    );
}

// ============================================================================
// 7. petitioner Tests (Complete 31-Method Suite)
// ============================================================================
#[tokio::test]
async fn test_petitioner_rpc_suite() {
    // parity: rpc:petitioner.AddPetitionRating, rpc:petitioner.CancelPetition, rpc:petitioner.ClaimPetition, rpc:petitioner.ClosePetition, rpc:petitioner.CreatePetition, rpc:petitioner.DeletePetition, rpc:petitioner.EscalatePetition, rpc:petitioner.GetCategories, rpc:petitioner.GetCategoryHierarchicalInfo, rpc:petitioner.GetCategoryProperties, rpc:petitioner.GetClaimedPetitions, rpc:petitioner.GetClientPickerInfo, rpc:petitioner.GetLog, rpc:petitioner.GetMyPetitionsEx, rpc:petitioner.GetPetitionMessages, rpc:petitioner.GetPetitionQueue, rpc:petitioner.GetQueues, rpc:petitioner.GetUnreadMessages, rpc:petitioner.GetUserCatalogCountry, rpc:petitioner.GetZendeskJwtLink, rpc:petitioner.HasOpenTickets, rpc:petitioner.IsSerenity, rpc:petitioner.IsZendeskEnabled, rpc:petitioner.IsZendeskSwapEnabled, rpc:petitioner.MarkAsRead, rpc:petitioner.MayPetition, rpc:petitioner.PetitioneeChat, rpc:petitioner.PetitionerChat, rpc:petitioner.PropertyPopulationInfo, rpc:petitioner.UnClaimPetition, rpc:petitioner.UpdatePetitionRating
    eve_testkit::attest(&[
        "rpc:petitioner.AddPetitionRating",
        "rpc:petitioner.CancelPetition",
        "rpc:petitioner.ClaimPetition",
        "rpc:petitioner.ClosePetition",
        "rpc:petitioner.CreatePetition",
        "rpc:petitioner.DeletePetition",
        "rpc:petitioner.EscalatePetition",
        "rpc:petitioner.GetCategories",
        "rpc:petitioner.GetCategoryHierarchicalInfo",
        "rpc:petitioner.GetCategoryProperties",
        "rpc:petitioner.GetClaimedPetitions",
        "rpc:petitioner.GetClientPickerInfo",
        "rpc:petitioner.GetLog",
        "rpc:petitioner.GetMyPetitionsEx",
        "rpc:petitioner.GetPetitionMessages",
        "rpc:petitioner.GetPetitionQueue",
        "rpc:petitioner.GetQueues",
        "rpc:petitioner.GetUnreadMessages",
        "rpc:petitioner.GetUserCatalogCountry",
        "rpc:petitioner.GetZendeskJwtLink",
        "rpc:petitioner.HasOpenTickets",
        "rpc:petitioner.IsSerenity",
        "rpc:petitioner.IsZendeskEnabled",
        "rpc:petitioner.IsZendeskSwapEnabled",
        "rpc:petitioner.MarkAsRead",
        "rpc:petitioner.MayPetition",
        "rpc:petitioner.PetitioneeChat",
        "rpc:petitioner.PetitionerChat",
        "rpc:petitioner.PropertyPopulationInfo",
        "rpc:petitioner.UnClaimPetition",
        "rpc:petitioner.UpdatePetitionRating",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 808, "demo", "demo-secret", P1).await;

    // 1. Static & Capability Checks
    let cats = rpc(&ctx, &mut s1, "petitioner", "GetCategories", &[]).await.expect("GetCategories");
    assert!(matches!(cats, PyRep::List(_)));

    let hier = rpc(&ctx, &mut s1, "petitioner", "GetCategoryHierarchicalInfo", &[]).await.expect("GetCategoryHierarchicalInfo");
    assert!(matches!(hier, PyRep::List(_)));

    let props = rpc(&ctx, &mut s1, "petitioner", "GetCategoryProperties", &[]).await.expect("GetCategoryProperties");
    assert!(matches!(props, PyRep::Dict(_)));

    let picker = rpc(&ctx, &mut s1, "petitioner", "GetClientPickerInfo", &[]).await.expect("GetClientPickerInfo");
    assert!(matches!(picker, PyRep::List(_)));

    let queues = rpc(&ctx, &mut s1, "petitioner", "GetQueues", &[]).await.expect("GetQueues");
    assert!(matches!(queues, PyRep::List(_)));

    let unread = rpc(&ctx, &mut s1, "petitioner", "GetUnreadMessages", &[]).await.expect("GetUnreadMessages");
    assert_eq!(unread, PyRep::Integer(0));

    let country = rpc(&ctx, &mut s1, "petitioner", "GetUserCatalogCountry", &[]).await.expect("GetUserCatalogCountry");
    assert_eq!(country, PyRep::String("US".into()));

    let zendesk_jwt = rpc(&ctx, &mut s1, "petitioner", "GetZendeskJwtLink", &[]).await.expect("GetZendeskJwtLink");
    assert_eq!(zendesk_jwt, PyRep::None);

    assert_eq!(rpc(&ctx, &mut s1, "petitioner", "IsSerenity", &[]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut s1, "petitioner", "IsZendeskEnabled", &[]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut s1, "petitioner", "IsZendeskSwapEnabled", &[]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut s1, "petitioner", "MayPetition", &[]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut s1, "petitioner", "PropertyPopulationInfo", &[]).await.unwrap(), PyRep::Dict(Vec::new()));

    // 2. CreatePetition
    let create_rep = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "CreatePetition",
        &[
            PyRep::String("Stuck in gate".into()),
            PyRep::String("My ship is stuck in the stargate".into()),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("CreatePetition");
    let PyRep::KeyVal(create_kv) = create_rep else {
        panic!("expected KeyVal from CreatePetition");
    };
    let pet_id = create_kv.get("petitionID").and_then(|v| v.as_i64()).expect("petitionID");
    assert!(pet_id > 0);

    // 3. HasOpenTickets & GetMyPetitionsEx
    let has_open = rpc(&ctx, &mut s1, "petitioner", "HasOpenTickets", &[]).await.expect("HasOpenTickets");
    assert_eq!(has_open, PyRep::Bool(true));

    let my_pets = rpc(&ctx, &mut s1, "petitioner", "GetMyPetitionsEx", &[]).await.expect("GetMyPetitionsEx");
    let PyRep::List(pet_list) = my_pets else {
        panic!("expected List from GetMyPetitionsEx");
    };
    assert!(!pet_list.is_empty());

    // 4. PetitionerChat
    let chat_rep = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "PetitionerChat",
        &[PyRep::Integer(pet_id), PyRep::String("Still stuck, please help".into())],
    )
    .await
    .expect("PetitionerChat");
    assert!(matches!(chat_rep, PyRep::KeyVal(_)));

    // 5. GM Administrative Lifecycle
    // Grant GM role to s1 to test GM methods
    s1.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    let queue = rpc(&ctx, &mut s1, "petitioner", "GetPetitionQueue", &[]).await.expect("GetPetitionQueue");
    assert!(matches!(queue, PyRep::List(_)));

    let claim = rpc(&ctx, &mut s1, "petitioner", "ClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("ClaimPetition");
    assert!(matches!(claim, PyRep::KeyVal(_)));

    let claimed = rpc(&ctx, &mut s1, "petitioner", "GetClaimedPetitions", &[]).await.expect("GetClaimedPetitions");
    let PyRep::List(claimed_list) = claimed else {
        panic!("expected List from GetClaimedPetitions");
    };
    assert!(!claimed_list.is_empty());

    let gm_chat = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "PetitioneeChat",
        &[PyRep::Integer(pet_id), PyRep::String("Looking into this now".into())],
    )
    .await
    .expect("PetitioneeChat");
    assert!(matches!(gm_chat, PyRep::KeyVal(_)));

    let msgs = rpc(&ctx, &mut s1, "petitioner", "GetPetitionMessages", &[PyRep::Integer(pet_id)]).await.expect("GetPetitionMessages");
    let PyRep::List(msg_list) = msgs else {
        panic!("expected List from GetPetitionMessages");
    };
    assert!(msg_list.len() >= 2);

    let logs = rpc(&ctx, &mut s1, "petitioner", "GetLog", &[PyRep::Integer(pet_id)]).await.expect("GetLog");
    assert!(matches!(logs, PyRep::List(_)));

    let esc = rpc(&ctx, &mut s1, "petitioner", "EscalatePetition", &[PyRep::Integer(pet_id), PyRep::String("Need dev check".into())]).await.expect("EscalatePetition");
    assert!(matches!(esc, PyRep::KeyVal(_)));

    let unclaim = rpc(&ctx, &mut s1, "petitioner", "UnClaimPetition", &[PyRep::Integer(pet_id)]).await.expect("UnClaimPetition");
    assert!(matches!(unclaim, PyRep::KeyVal(_)));

    let close = rpc(&ctx, &mut s1, "petitioner", "ClosePetition", &[PyRep::Integer(pet_id), PyRep::String("Ship unstuck, safe travels!".into())]).await.expect("ClosePetition");
    assert!(matches!(close, PyRep::KeyVal(_)));

    // Revert role to player
    s1.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 6. Ratings & Closing
    let rate = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "AddPetitionRating",
        &[PyRep::Integer(pet_id), PyRep::Integer(9), PyRep::String("Awesome response".into())],
    )
    .await
    .expect("AddPetitionRating");
    assert!(matches!(rate, PyRep::KeyVal(_)));

    let update_rate = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "UpdatePetitionRating",
        &[PyRep::Integer(pet_id), PyRep::Integer(10), PyRep::String("Perfect 10/10".into())],
    )
    .await
    .expect("UpdatePetitionRating");
    assert!(matches!(update_rate, PyRep::KeyVal(_)));

    let mark_read = rpc(&ctx, &mut s1, "petitioner", "MarkAsRead", &[PyRep::Integer(pet_id)]).await.expect("MarkAsRead");
    assert_eq!(mark_read, PyRep::Bool(true));

    let cancel = rpc(&ctx, &mut s1, "petitioner", "CancelPetition", &[PyRep::Integer(pet_id)]).await.expect("CancelPetition");
    assert_eq!(cancel, PyRep::Bool(true));

    let del = rpc(&ctx, &mut s1, "petitioner", "DeletePetition", &[PyRep::Integer(pet_id)]).await.expect("DeletePetition");
    assert_eq!(del, PyRep::Bool(true));
}
