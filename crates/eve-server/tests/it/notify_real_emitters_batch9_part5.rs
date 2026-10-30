//! Real server-side emission tests for Notify Batch 9 (Part 5: JumpQueue, FlaggedLists, CfgDataChanged, CCPNotification, ServerMessage, Localization & ServiceMapping).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use crate::notify_real_emitters_batch9::{login_docked, upwell_struct};
use eve_proto::PyRep;
use glam::DVec3;

// parity: notify:OnJumpQueueUpdate, notify:OnFlaggedListsChange, notify:OnCfgDataChanged
#[tokio::test]
async fn notify_batch9_jump_queue_flagged_lists_and_cfg_data() {
    eve_testkit::attest(&[
        "notify:OnJumpQueueUpdate",
        "notify:OnFlaggedListsChange",
        "notify:OnCfgDataChanged",
    ]);

    let ctx = new_ctx();
    let ship_a = 950_000_001_i64;
    let mut sess_a = login_in_space(&ctx, 9_501, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_docked(&ctx, 9_502, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnJumpQueueUpdate
    // Negative 1a: Non-GM calling SetJumpQueueThreshold fails with RoleRequired
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "jumpQueue",
        "SetJumpQueueThreshold",
        &[PyRep::Integer(SYS_JITA as i64), PyRep::Integer(1)],
    )
    .await
    .is_err());

    // Negative 1b: Threshold < 1 fails with InvalidThreshold
    sess_b.role |= eve_db::service_roles::ROLE_GMH;
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "jumpQueue",
        "SetSystemJumpQueueThreshold",
        &[PyRep::Integer(0), PyRep::Integer(1)],
    )
    .await
    .is_err());

    // Positive: Set threshold to 1 for SYS_JITA, trigger stargate jump queue check for PILOT_A, and query GetJumpQueue -> emits OnJumpQueueUpdate (5 args)
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "jumpQueue",
        "SetSystemJumpQueueThreshold",
        &[PyRep::Integer(SYS_JITA as i64), PyRep::Integer(1)],
    )
    .await
    .expect("SetSystemJumpQueueThreshold");
    eve_server::services::notify_emitters_batch9_part2::check_and_notify_stargate_jump_queue(
        &ctx, PILOT_A, SYS_JITA,
    );
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "jumpQueue",
        "GetJumpQueue",
        &[PyRep::Integer(SYS_JITA as i64)],
    )
    .await
    .expect("GetJumpQueue");
    let jq_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let jq_args = find_notif(&jq_notifs, "OnJumpQueueUpdate");
    assert_eq!(jq_args.len(), 5);
    assert_eq!(jq_args[0], PyRep::Integer(SYS_JITA as i64));

    // 2. OnFlaggedListsChange
    // Negative 2a: Non-GM calling SetGlobalConfigValue("browser_flagged_sites") fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("browser_flagged_sites".into()), PyRep::String("evil.example.com".into())],
    )
    .await
    .is_err());

    // Negative 2b: Empty URL fails with InvalidArgument
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("browser_flagged_sites".into()), PyRep::String("   ".into())],
    )
    .await
    .is_err());

    // Positive: GM updating flagged sites broadcasts OnFlaggedListsChange
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("browser_flagged_sites".into()), PyRep::String("phishing.example.com".into())],
    )
    .await
    .expect("SetGlobalConfigValue browser_flagged_sites");
    let flag_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(flag_notifs.iter().any(|(e, _)| e == "OnFlaggedListsChange"));

    // 3. OnCfgDataChanged: RenameStructure
    let struct_id = 1_050_000_000_001_i64;
    let corp_id = 98_900_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Cfg Corp".into(),
                ticker: "CFGC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            struct_id,
            upwell_struct(struct_id, 35832, corp_id, SYS_JITA, "Old Astrahus", [5_000.0, 1_000.0, -2_000.0]),
        );
    });
    sess_a.corporation_id = Some(corp_id);

    // Negative 3a: Empty structure name fails with InvalidStructureName
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("   ".into())],
    )
    .await
    .is_err());

    // Negative 3b: Non-corp member (PILOT_B) calling RenameStructure fails
    sess_b.role = 0;
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("Hacked Name".into())],
    )
    .await
    .is_err());

    // Positive: CEO renaming structure emits OnCfgDataChanged(["evelocations", 6-elem list])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("New Astrahus".into())],
    )
    .await
    .expect("RenameStructure");
    let cfg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cfg_args = find_notif(&cfg_notifs, "OnCfgDataChanged");
    assert_eq!(cfg_args.len(), 2);
    assert_eq!(cfg_args[0], PyRep::String("evelocations".into()));
    assert!(matches!(&cfg_args[1], PyRep::List(elems) if elems.len() == 6));
}

// parity: notify:OnCCPNotification, notify:OnServerMessage, notify:OnUpdateLocalizationTextCache, notify:OnUpdateServiceMapping
#[tokio::test]
async fn notify_batch9_ccp_notification_server_msg_localization_and_service_map() {
    eve_testkit::attest(&[
        "notify:OnCCPNotification",
        "notify:OnServerMessage",
        "notify:OnUpdateLocalizationTextCache",
        "notify:OnUpdateServiceMapping",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_511, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_gm = login_docked(&ctx, 9_512, "other", "other-secret", PILOT_B, JITA_STATION).await;
    sess_gm.role |= eve_db::service_roles::ROLE_GMH;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_gm, PILOT_B);

    // 4. OnCCPNotification
    // Negative 4a: Non-GM calling SetGlobalConfigValue("ccp_notification") fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "SetGlobalConfigValue",
        &[
            PyRep::String("ccp_notification".into()),
            PyRep::String("Test".into()),
            PyRep::String("Sub".into()),
            PyRep::String("Body".into()),
        ],
    )
    .await
    .is_err());

    // Negative 4b: Empty title fails with InvalidArgument
    assert!(rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[
            PyRep::String("ccp_notification".into()),
            PyRep::String("".into()),
            PyRep::String("Sub".into()),
            PyRep::String("Body".into()),
        ],
    )
    .await
    .is_err());

    // Positive: GM broadcasting CCP notification emits OnCCPNotification
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[
            PyRep::String("ccp_notification".into()),
            PyRep::String("Server Alert".into()),
            PyRep::String("Maintenance".into()),
            PyRep::String("Scheduled downtime in 15m".into()),
        ],
    )
    .await
    .expect("SetGlobalConfigValue ccp_notification");
    let ccp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(ccp_notifs.iter().any(|(e, _)| e == "OnCCPNotification"));

    // 5. OnServerMessage
    // Negative 5a: Non-GM calling SetGlobalConfigValue("server_message") fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("server_message".into()), PyRep::String("Hello".into())],
    )
    .await
    .is_err());

    // Negative 5b: Empty message fails with InvalidArgument
    assert!(rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("server_message".into()), PyRep::String("   ".into())],
    )
    .await
    .is_err());

    // Positive: GM broadcasting server message emits OnServerMessage
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("server_message".into()), PyRep::String("Welcome to Tranquility".into())],
    )
    .await
    .expect("SetGlobalConfigValue server_message");
    let srv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(srv_notifs.iter().any(|(e, _)| e == "OnServerMessage"));

    // 6. OnUpdateLocalizationTextCache
    // Negative 6a: Non-GM calling UpdateLocalizationQAWrap fails with RoleRequired
    let loc_dict = PyRep::Dict(vec![(PyRep::Integer(1001), PyRep::String("Override Text".into()))]);
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[loc_dict.clone()],
    )
    .await
    .is_err());

    // Negative 6b: Boolean QA-wrap toggle fails with UnimplementedRPC
    assert!(rpc(
        &ctx,
        &mut sess_gm,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .is_err());

    // Positive: GM updating localization text cache emits OnUpdateLocalizationTextCache
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[loc_dict],
    )
    .await
    .expect("UpdateLocalizationQAWrap");
    let loc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(loc_notifs.iter().any(|(e, _)| e == "OnUpdateLocalizationTextCache"));

    // 7. OnUpdateServiceMapping
    // Negative 7a: Non-GM calling SetGlobalConfigValue("service_mapping") fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("service_mapping".into()), PyRep::String("marketProxy".into()), PyRep::Integer(2)],
    )
    .await
    .is_err());

    // Negative 7b: Empty service name fails with InvalidArgument
    assert!(rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("service_mapping".into()), PyRep::String("".into()), PyRep::Integer(2)],
    )
    .await
    .is_err());

    // Positive: GM updating service mapping emits OnUpdateServiceMapping
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("service_mapping".into()), PyRep::String("market".into()), PyRep::Integer(2)],
    )
    .await
    .expect("SetGlobalConfigValue service_mapping");
    let map_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(map_notifs.iter().any(|(e, _)| e == "OnUpdateServiceMapping"));
}
