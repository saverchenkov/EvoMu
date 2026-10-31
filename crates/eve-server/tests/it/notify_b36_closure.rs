//! Live integration tests for Batch 36 state-backed V24 notification emitters:
//! - `notify:OnNotificationUndeleted`
//! - `notify:OnPlexDepositFailed`
//! - `notify:OnPlexWithdrawFailed`
//! - `notify:OnProgressionAdvanced`
//! - `notify:OnProgressionTaskCompleted`
//! - `notify:OnProgressionTaskWidgetUpdate`
//! - `notify:OnStartConversation`
//! - `notify:OnServerBrainUpdated`
//! - `notify:OnCombatMessage`
//! - `notify:OnOfferReceived`
//! - `notify:OnFlaggedListsChange`
//! - `notify:OnUpdateServiceMapping`

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYSTEM: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &args,
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_b36_notification_undeleted_and_flagged_lists() {
    // parity: notify:OnNotificationUndeleted
    // parity: notify:OnFlaggedListsChange
    eve_testkit::attest(&[
        "notify:OnNotificationUndeleted",
        "notify:OnFlaggedListsChange",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 911, "demo", "demo-secret", P1).await;

    // Seed a deleted notification for P1
    let notif_id = 777_001i64;
    {
        let mut records = ctx
            .notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner());
        records.push(eve_server::router::NotificationRecord {
            notification_id: notif_id,
            type_id: 1,
            group_id: 1,
            sender_id: P2,
            receiver_id: P1,
            created: ctx.now_blueos_filetime_ticks(),
            data: "{}".into(),
            processed: false,
            deleted: true,
            interacted: false,
        });
    }

    // Negative case: undeleting a non-existent/non-deleted ID emits nothing
    let res_none = ctx
        .dispatch_rpc(
            &mut s1,
            "notificationMgr",
            None,
            "LogNotificationInteraction",
            &[PyRep::Integer(999_999), PyRep::String("undelete".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("LogNotificationInteraction non-existent");
    assert_eq!(res_none, PyRep::Bool(true));
    assert!(events(&ctx, P1, "OnNotificationUndeleted").is_empty());

    // Positive case: undeleting the deleted notification mutates state and emits OnNotificationUndeleted
    let res_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "notificationMgr",
            None,
            "LogNotificationInteraction",
            &[PyRep::Integer(notif_id), PyRep::String("undelete".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("LogNotificationInteraction undelete");
    assert_eq!(res_ok, PyRep::Bool(true));
    let undeleted_ev = events(&ctx, P1, "OnNotificationUndeleted");
    assert_eq!(undeleted_ev.len(), 1);
    assert_eq!(undeleted_ev[0].len(), 1);
    match &undeleted_ev[0][0] {
        PyRep::List(ids) => {
            assert_eq!(ids.len(), 1);
            assert_eq!(ids[0].as_i64(), Some(notif_id));
        }
        other => panic!("expected List of notificationIDs, got {other:?}"),
    }

    // Negative case: unblocking a contact that is not blocked emits no OnFlaggedListsChange
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "charMgr",
            None,
            "UnblockOwners",
            &[PyRep::List(vec![PyRep::Integer(P2)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("UnblockOwners");
    assert!(events(&ctx, P1, "OnFlaggedListsChange").is_empty());

    // Positive case: blocking P2 mutates blocklist and emits OnFlaggedListsChange()
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "charMgr",
            None,
            "BlockOwners",
            &[PyRep::List(vec![PyRep::Integer(P2)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("BlockOwners");
    let flagged_ev = events(&ctx, P1, "OnFlaggedListsChange");
    assert_eq!(flagged_ev.len(), 1);
    assert!(flagged_ev[0].is_empty());
}

#[tokio::test]
async fn test_b36_plex_deposit_and_withdraw_failed() {
    // parity: notify:OnPlexDepositFailed
    // parity: notify:OnPlexWithdrawFailed
    eve_testkit::attest(&[
        "notify:OnPlexDepositFailed",
        "notify:OnPlexWithdrawFailed",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 912, "demo", "demo-secret", P1).await;
    s1.station_id = Some(JITA_STATION);
    s1.solar_system_id = Some(JITA_SYSTEM);

    // Seed 50 PLEX in P1's hangar at Jita 4-4
    let plex_item_id = 888_100i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_item_id,
            eve_db::InventoryItemRecord {
                item_id: plex_item_id,
                type_id: 44992, // PLEX
                owner_id: P1,
                location_id: JITA_STATION,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Positive deposit of 20 PLEX should NOT emit OnPlexDepositFailed
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(plex_item_id),
                PyRep::Integer(20),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("DepositPlexToVault valid");
    assert!(events(&ctx, P1, "OnPlexDepositFailed").is_empty());

    // Negative deposit (requesting 100 PLEX when only 30 remain) emits OnPlexDepositFailed(error_key, reference)
    let err_dep = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(plex_item_id),
                PyRep::Integer(100),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(err_dep.is_err());
    let dep_ev = events(&ctx, P1, "OnPlexDepositFailed");
    assert_eq!(dep_ev.len(), 1);
    assert_eq!(dep_ev[0].len(), 2);
    assert_eq!(dep_ev[0][0].as_str(), Some("NotEnoughQuantity"));
    assert_eq!(dep_ev[0][1].as_i64(), Some(plex_item_id));

    // Negative withdraw (requesting 999_999_999 PLEX when vault has far less) emits OnPlexWithdrawFailed(error_key, reference)
    let err_wd = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "WithdrawPlexFromVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(999_999_999)],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(err_wd.is_err());
    let wd_ev = events(&ctx, P1, "OnPlexWithdrawFailed");
    assert_eq!(wd_ev.len(), 1);
    assert_eq!(wd_ev[0].len(), 2);
    assert_eq!(wd_ev[0][0].as_str(), Some("NotEnoughPlex"));
    assert_eq!(wd_ev[0][1].as_i64(), Some(999_999_999));
}

#[tokio::test]
async fn test_b36_progression_and_conversation_notifications() {
    // parity: notify:OnProgressionAdvanced
    // parity: notify:OnProgressionTaskCompleted
    // parity: notify:OnProgressionTaskWidgetUpdate
    // parity: notify:OnStartConversation
    eve_testkit::attest(&[
        "notify:OnProgressionAdvanced",
        "notify:OnProgressionTaskCompleted",
        "notify:OnProgressionTaskWidgetUpdate",
        "notify:OnStartConversation",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 913, "demo", "demo-secret", P1).await;

    // 1. OnStartConversation
    let res_conv = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[
                PyRep::String("start_conversation".into()),
                PyRep::Integer(42),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("start_conversation");
    assert_eq!(res_conv, PyRep::Bool(true));
    let conv_ev = events(&ctx, P1, "OnStartConversation");
    assert_eq!(conv_ev.len(), 1);
    assert_eq!(conv_ev[0].len(), 2);
    assert_eq!(conv_ev[0][0].as_i64(), Some(42));
    assert_eq!(conv_ev[0][1], PyRep::Bool(true));

    // 2. OnProgressionTaskWidgetUpdate
    let mut widget_kwargs = HashMap::new();
    widget_kwargs.insert(
        "extra_data".into(),
        PyRep::Dict(vec![
            (PyRep::String("completed".into()), PyRep::Bool(true)),
            (PyRep::String("progress".into()), PyRep::Integer(1)),
        ]),
    );
    let res_widget = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[
                PyRep::String("update_task_widget".into()),
                PyRep::Integer(101),
                PyRep::Integer(7),
            ],
            &widget_kwargs,
            None,
        )
        .await
        .expect("update_task_widget");
    assert_eq!(res_widget, PyRep::Bool(true));
    let widget_ev = events(&ctx, P1, "OnProgressionTaskWidgetUpdate");
    assert_eq!(widget_ev.len(), 1);
    assert_eq!(widget_ev[0].len(), 3);
    assert_eq!(widget_ev[0][0].as_i64(), Some(101));
    assert_eq!(widget_ev[0][1].as_i64(), Some(7));
    assert!(matches!(widget_ev[0][2], PyRep::Dict(_)));

    // 3. OnProgressionTaskCompleted & OnProgressionAdvanced
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "activate_operation_in_solar_system",
            &[PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("activate_operation");
    let res_task = ctx
        .dispatch_rpc(
            &mut s1,
            "operationsManager",
            None,
            "process_client_event",
            &[PyRep::String("complete_task".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("complete_task");
    assert_eq!(res_task, PyRep::Bool(true));
    let task_ev = events(&ctx, P1, "OnProgressionTaskCompleted");
    assert_eq!(task_ev.len(), 1);
    assert_eq!(task_ev[0].len(), 1);
    assert!(task_ev[0][0].as_i64().unwrap_or(0) > 0);

    let adv_ev = events(&ctx, P1, "OnProgressionAdvanced");
    assert_eq!(adv_ev.len(), 1);
    assert_eq!(adv_ev[0].len(), 2);
    assert!(adv_ev[0][0].as_i64().unwrap_or(0) > 0);
    assert_eq!(adv_ev[0][1], task_ev[0][0]);
}

#[tokio::test]
async fn test_b36_brain_combat_offer_and_service_mapping() {
    // parity: notify:OnServerBrainUpdated
    // parity: notify:OnCombatMessage
    // parity: notify:OnOfferReceived
    // parity: notify:OnUpdateServiceMapping
    eve_testkit::attest(&[
        "notify:OnServerBrainUpdated",
        "notify:OnCombatMessage",
        "notify:OnOfferReceived",
        "notify:OnUpdateServiceMapping",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 914, "demo", "demo-secret", P1).await;

    // 1. OnOfferReceived via contextualOfferMgr.TrackOfferDelivered
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "contextualOfferMgr",
            None,
            "TrackOfferDelivered",
            &[PyRep::Integer(555)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("TrackOfferDelivered");
    let offer_ev = events(&ctx, P1, "OnOfferReceived");
    assert_eq!(offer_ev.len(), 1);
    assert_eq!(offer_ev[0].len(), 2);
    assert_eq!(offer_ev[0][0].as_i64(), Some(555));

    // 2. OnUpdateServiceMapping: unauthorized pilot fails and emits nothing
    let unauth = ctx
        .dispatch_rpc(
            &mut s1,
            "machoNet",
            None,
            "SetGlobalConfigValue",
            &[
                PyRep::String("serviceMapping:marketProxy".into()),
                PyRep::Integer(1002),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(unauth.is_err());
    assert!(events(&ctx, P1, "OnUpdateServiceMapping").is_empty());

    // Authorized GM updates service mapping -> emits OnUpdateServiceMapping(svc, nodeID)
    s1.role |= eve_db::service_roles::ROLE_GMH;
    let ok_map = ctx
        .dispatch_rpc(
            &mut s1,
            "machoNet",
            None,
            "SetGlobalConfigValue",
            &[
                PyRep::String("serviceMapping:marketProxy".into()),
                PyRep::Integer(1002),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SetGlobalConfigValue GM");
    assert_eq!(ok_map, PyRep::Bool(true));
    let map_ev = events(&ctx, P1, "OnUpdateServiceMapping");
    assert_eq!(map_ev.len(), 1);
    assert_eq!(map_ev[0].len(), 2);
    assert_eq!(map_ev[0][0].as_str(), Some("marketProxy"));
    assert_eq!(map_ev[0][1].as_i64(), Some(1002));

    // 3. OnServerBrainUpdated via implant plugging
    let implant_id = 888_200i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.station_id = Some(JITA_STATION);
        }
        snap.skills.entry(P1).or_default().insert(
            3411,
            eve_db::TrainedSkillRecord {
                type_id: 3411,
                level: 1,
                skill_points: 250.0,
            },
        ); // Cybernetics I
        snap.items.insert(
            implant_id,
            eve_db::InventoryItemRecord {
                item_id: implant_id,
                type_id: 9899, // Ocular Filter - Basic
                owner_id: P1,
                location_id: JITA_STATION,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = eve_server::services::implant_plugging::inject_implant(
        &ctx,
        &s1,
        &[PyRep::Integer(implant_id)],
    )
    .expect("inject_implant");
    let brain_ev = events(&ctx, P1, "OnServerBrainUpdated");
    assert_eq!(brain_ev.len(), 1);
    assert_eq!(brain_ev[0].len(), 1);
    match &brain_ev[0][0] {
        PyRep::Tuple(parts) => {
            assert_eq!(parts.len(), 4);
            assert!(parts[0].as_i64().unwrap_or(0) >= 1);
        }
        other => panic!("expected brainData Tuple, got {other:?}"),
    }

    // 4. OnCombatMessage via state-backed emitter
    eve_server::services::notify_emitters_b36::emit_combat_message(
        &ctx,
        P1,
        "AttackHit",
        900_000_099,
        142.5,
    );
    let combat_ev = events(&ctx, P1, "OnCombatMessage");
    assert_eq!(combat_ev.len(), 1);
    assert_eq!(combat_ev[0].len(), 2);
    assert_eq!(combat_ev[0][0].as_str(), Some("AttackHit"));
    match &combat_ev[0][1] {
        PyRep::Dict(entries) => {
            let has_special = entries
                .iter()
                .any(|(k, v)| k.as_str() == Some("specialObject") && v.as_i64() == Some(900_000_099));
            assert!(has_special);
        }
        other => panic!("expected messageArgs Dict, got {other:?}"),
    }
}
