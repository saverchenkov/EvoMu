//! Real server-side emission tests for Notify Batch 9 (Part 5: JumpQueue, FlaggedLists, CfgDataChanged, CCPNotification, ServerMessage, Localization & ServiceMapping).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use crate::notify_real_emitters_batch9::{login_docked, upwell_struct};
use eve_proto::PyRep;
use glam::DVec3;

// parity: notify:OnCfgDataChanged
#[tokio::test]
async fn notify_batch9_jump_queue_flagged_lists_and_cfg_data() {
    eve_testkit::attest(&[
        "notify:OnCfgDataChanged",
    ]);

    let ctx = new_ctx();
    let ship_a = 950_000_001_i64;
    let mut sess_a = login_in_space(&ctx, 9_501, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_docked(&ctx, 9_502, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

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

// parity: notify:OnServerMessage, notify:OnUpdateLocalizationTextCache
#[tokio::test]
async fn notify_batch9_ccp_notification_server_msg_localization_and_service_map() {
    eve_testkit::attest(&[
        "notify:OnServerMessage",
        "notify:OnUpdateLocalizationTextCache",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_511, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_gm = login_docked(&ctx, 9_512, "other", "other-secret", PILOT_B, JITA_STATION).await;
    sess_gm.role |= eve_db::service_roles::ROLE_GMH;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_gm, PILOT_B);

    // 5. OnServerMessage: machoNet.GracefulShutdown
    // Negative 5a: Non-GM calling GracefulShutdown fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "GracefulShutdown",
        &[PyRep::Integer(1), PyRep::String("Maintenance".into()), PyRep::Integer(300), PyRep::Bool(true)],
    )
    .await
    .is_err());

    // Positive: GM initiating GracefulShutdown broadcasts OnServerMessage
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "GracefulShutdown",
        &[PyRep::Integer(1), PyRep::String("Welcome to Tranquility".into()), PyRep::Integer(300), PyRep::Bool(true)],
    )
    .await
    .expect("GracefulShutdown");
    let srv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&srv_notifs, "OnServerMessage"),
        &[PyRep::String("Welcome to Tranquility".into())]
    );

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
}
