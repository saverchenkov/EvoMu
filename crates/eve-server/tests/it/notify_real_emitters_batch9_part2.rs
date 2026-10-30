//! Real server-side emission tests for Notify Batch 9 (Part 2: Milestones, Redeeming, Crates, Character & Subscription).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
};
use crate::notify_real_emitters_batch9::login_docked;
use eve_db::InventoryFlag;
use eve_proto::PyRep;

// parity: notify:OnLoginMilestoneAchieved, notify:OnMilestoneTimerStarted, notify:OnRedeemingQueueUpdated, notify:OnCrateLootSpawned, notify:OnPodOrCorvetteAssembly
#[tokio::test]
async fn notify_batch9_milestones_redeeming_crates_and_assembly() {
    eve_testkit::attest(&[
        "notify:OnLoginMilestoneAchieved",
        "notify:OnMilestoneTimerStarted",
        "notify:OnRedeemingQueueUpdated",
        "notify:OnCrateLootSpawned",
        "notify:OnPodOrCorvetteAssembly",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 1-2. OnMilestoneTimerStarted & OnLoginMilestoneAchieved
    // Negative 1a: Unauthenticated ProcessCharacterLogon fails
    let mut unauth = eve_machonet::SessionState::new(998);
    assert!(rpc(&ctx, &mut unauth, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .is_err());

    // Positive: ProcessCharacterLogon starts milestone timer and emits OnMilestoneTimerStarted([end_ft])
    let _ = rpc(&ctx, &mut sess_a, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    let ms_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timer_args = find_notif(&ms_notifs, "OnMilestoneTimerStarted");
    assert_eq!(timer_args.len(), 1);
    assert!(timer_args[0].as_i64().unwrap_or(0) > 0);

    // Negative 1b: ClaimRewards remains fail-closed (UnimplementedRPC) and does not emit OnLoginMilestoneAchieved
    assert!(rpc(&ctx, &mut sess_a, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLoginMilestoneAchieved"));

    // Positive: Advancing cluster tick past 7200s completes the login milestone and emits OnLoginMilestoneAchieved([1, 500000.0])
    let future_unix = ctx.now_unix() + 7_205;
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        future_unix,
    );
    let achieved_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&achieved_notifs, "OnLoginMilestoneAchieved"),
        &[PyRep::Integer(1), PyRep::Real(500_000.0)]
    );

    // Negative 2b: Re-running sweep does not duplicate OnLoginMilestoneAchieved
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        future_unix + 60,
    );
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLoginMilestoneAchieved"));

    // 3. OnRedeemingQueueUpdated
    // Negative 3a: TrashRedeemTokens with non-existent token does NOT emit OnRedeemingQueueUpdated
    let trash_empty = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(888_888)])
        .await
        .expect("TrashRedeemTokens empty");
    assert_eq!(trash_empty, PyRep::Bool(false));
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Positive: CreateRedeemTokenQA emits OnRedeemingQueueUpdated
    sess_a.role |= eve_db::service_roles::ROLE_GMH;
    let tok_res = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(34), PyRep::Integer(5), PyRep::String("RedeemTest".into())],
    )
    .await
    .expect("CreateRedeemTokenQA");
    sess_a.role &= !eve_db::service_roles::ROLE_GMH;
    let tok_id = match tok_res {
        PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    let redeem_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(redeem_notifs.iter().any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Negative 3b: ClaimRedeemTokens with invalid token fails and does not emit OnRedeemingQueueUpdated
    assert!(rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(777_777)])])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Positive: TrashRedeemTokens on valid token emits OnRedeemingQueueUpdated
    let trash_ok = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(tok_id)])
        .await
        .expect("TrashRedeemTokens valid");
    assert_eq!(trash_ok, PyRep::Bool(true));
    assert!(drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // 4. OnCrateLootSpawned
    // Negative 4a: ClaimLoot on non-existent crate fails and emits nothing
    assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(920_999_999)])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCrateLootSpawned"));

    // Negative 4b: ClaimLoot on a non-crate item (Tritanium = 34) fails with NotACrate
    let non_crate_id = 920_000_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_crate_id,
            make_item(non_crate_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(non_crate_id)])
        .await
        .is_err());

    // Positive: ClaimLoot on a valid crate (type 40520 or 3466/crate)
    let crate_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            (t.name.contains("Crate")
                || t.name.contains("Box")
                || t.name.contains("Cache")
                || t.name.contains("Bundle")
                || t.name.contains("Package"))
                && ctx.sde.type_materials.get(&t.type_id).is_some_and(|m| !m.is_empty())
        })
        .map(|t| t.type_id)
        .unwrap_or(40520);
    let crate_id = 920_000_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            crate_id,
            make_item(crate_id, crate_type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
        .await
        .expect("OpenCrate");
    let mut crate_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(crate_id)])
        .await
        .expect("ClaimLoot");
    crate_notifs.extend(drain_all(&ctx, &mut sess_a, PILOT_A));
    assert!(crate_notifs.iter().any(|(e, a)| e == "OnCrateLootSpawned" && a.is_empty()));

    // 5. OnPodOrCorvetteAssembly
    // Negative 5a: Assembling a regular frigate (Rifter = 587, group 25) does NOT emit OnPodOrCorvetteAssembly
    let rifter_id = 920_000_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rifter_id,
            make_item(rifter_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(rifter_id)])
        .await
        .expect("AssembleShip rifter");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPodOrCorvetteAssembly"));

    // Negative 5b: Assembling a non-ship item fails with NotAShip
    assert!(rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(non_crate_id)])
        .await
        .is_err());

    // Positive: Assembling a Corvette (Velator = 606, group 237) emits OnPodOrCorvetteAssembly([velator_id, 606])
    let corvette_id = 920_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corvette_id,
            make_item(corvette_id, 606, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(corvette_id)])
        .await
        .expect("AssembleShip corvette");
    let asm_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&asm_notifs, "OnPodOrCorvetteAssembly"),
        &[PyRep::Integer(corvette_id), PyRep::Integer(606)]
    );
}

// parity: notify:OnPasswordChanged, notify:OnPlayerStateUpdated, notify:OnServerBrainUpdated, notify:OnSubscriptionChangedServer, notify:OnSubscriptionTimeUpdateServer
#[tokio::test]
async fn notify_batch9_password_player_state_brain_and_subscription() {
    eve_testkit::attest(&[
        "notify:OnPasswordChanged",
        "notify:OnPlayerStateUpdated",
        "notify:OnServerBrainUpdated",
        "notify:OnSubscriptionChangedServer",
        "notify:OnSubscriptionTimeUpdateServer",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_211, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 6. OnPasswordChanged
    // Negative 6a: Setting password on a standard Cargo Container (type 23, group 12) fails
    let bad_item_id = 920_000_030_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bad_item_id,
            make_item(bad_item_id, 23, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPasswordChanged"));

    // Negative 6b: Unauthenticated SetPassword fails
    let mut unauth = eve_machonet::SessionState::new(997);
    assert!(rpc(
        &ctx,
        &mut unauth,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
    )
    .await
    .is_err());

    // Positive: Setting password on an Audit Log Secure Container (type 17366) emits OnPasswordChanged
    let alsc_id = 920_000_031_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            alsc_id,
            make_item(alsc_id, 17366, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::String("hunter2".into())],
    )
    .await
    .expect("SetPassword ALSC");
    let pw_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&pw_notifs, "OnPasswordChanged"),
        &[
            PyRep::Integer(1),
            PyRep::keyval([
                ("itemID", PyRep::Integer(alsc_id)),
                ("typeID", PyRep::Integer(17366)),
            ]),
        ]
    );

    // 7. OnPlayerStateUpdated: Project Discovery classification
    // Negative 7a: Unauthenticated post_classification fails
    assert!(rpc(&ctx, &mut unauth, "projectDiscovery", "post_classification", &[])
        .await
        .is_err());

    // Negative 7b: Invalid classification payload fails without emitting OnPlayerStateUpdated
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "projectDiscovery",
        "post_classification",
        &[PyRep::String("invalid_not_dict".into())],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPlayerStateUpdated"));

    // Positive: Valid Project Discovery classification emits OnPlayerStateUpdated([playerState])
    let payload = PyRep::Dict(vec![
        (PyRep::String("task_id".into()), PyRep::Integer(1)),
        (
            PyRep::String("classification".into()),
            PyRep::List(vec![PyRep::Integer(1)]),
        ),
    ]);
    let task_res = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = match task_res {
        PyRep::KeyVal(kv) => kv
            .get("taskNumber")
            .or_else(|| kv.get("id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1),
        _ => 1,
    };
    let _ = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "post_classification", &[PyRep::Integer(task_id), payload])
        .await
        .expect("post_classification");
    let pd_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ps_args = find_notif(&pd_notifs, "OnPlayerStateUpdated");
    assert_eq!(ps_args.len(), 1);
    assert!(matches!(&ps_args[0], PyRep::KeyVal(_)));

    // 8. OnServerBrainUpdated: Applying free SP or plugging an implant emits OnServerBrainUpdated
    // Negative 8a: ApplyFreeSkillPoints with 0 SP fails and does not emit OnServerBrainUpdated
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3300), PyRep::Real(0.0)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnServerBrainUpdated"));

    // Negative 8b: ApplyFreeSkillPoints with unknown skill type 0 fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(0), PyRep::Real(1_000.0)],
    )
    .await
    .is_err());

    // Positive: Grant free SP and apply to Gunnery (3300) -> emits OnServerBrainUpdated([version, grayMatter])
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.free_skill_points = 500.0;
        }
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3300), PyRep::Real(250.0)],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    let brain_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let brain_args = find_notif(&brain_notifs, "OnServerBrainUpdated");
    assert_eq!(brain_args.len(), 1);
    assert!(matches!(&brain_args[0], PyRep::Tuple(t) if t.len() == 2));

    // 9-10. OnSubscriptionChangedServer & OnSubscriptionTimeUpdateServer
    let uid_a = sess_a.user_id.unwrap_or(9_211);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 1;
        }
    });
    let future_exp = ctx.now_unix() + 30 * 86_400;
    let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, future_exp);
    let sub_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&sub_notifs, "OnSubscriptionChangedServer"),
        &[PyRep::Integer(2)]
    );
    let time_args = find_notif(&sub_notifs, "OnSubscriptionTimeUpdateServer");
    assert_eq!(time_args.len(), 1);
    assert!(time_args[0].as_i64().unwrap_or(0) > 0);

    // Negative 9a/10a: Calling sync_subscription again when clone_grade is already 2 does NOT re-emit OnSubscriptionChangedServer
    let _ = eve_server::services::account_services::sync_subscription(&ctx, uid_a);
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSubscriptionChangedServer"));

    // Negative 9b/10b: Expiring subscription (until = 1) emits OnSubscriptionTimeUpdateServer
    let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, 1);
    let lapse_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(lapse_notifs
        .iter()
        .any(|(e, a)| e == "OnSubscriptionTimeUpdateServer" && a.len() == 1));
}
