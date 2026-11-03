//! Live integration tests covering 14 missing `notify:*` parity rows.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{
    cache_notifications::NotificationRecord, NotificationTarget, ServerContext,
};
use eve_server::services::notify_emitters_batch3::{
    emit_ccp_notification, emit_contextual_offer_purchased, emit_login_milestone_achieved,
};
use eve_server::services::notify_emitters_batch3_space::{
    emit_ess_reserve_bank_inventory_full, emit_jump_queue_update,
};
use eve_server::services::notify_emitters_missing::{
    emit_bean_prime, emit_clear_fetched_data, emit_flagged_lists_change,
    restore_character_notifications,
};
use eve_server::services::pve_dungeon_runtime::{
    check_dungeon_triggers, with_dungeon_runtime_store_db, DungeonRuntimeInstance,
    DungeonRuntimeObject, DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction,
    DungeonTriggerKind,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_PERIMETER: u32 = 30_000_144;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
}

async fn setup_pilot_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    gm: bool,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(make_ball(ship_id, ship_type_id, DVec3::new(1000.0, 0.0, 0.0)))
        .await;
    let mut sess = SessionState::new(char_id as u64);
    let (user, pass) = if char_id == PILOT_A {
        ("demo", "demo-secret")
    } else {
        ("other", "other-secret")
    };
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    if gm {
        sess.role |= ROLE_GM_DEFAULT;
    }
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: notify:OnDungeonCameraInterest, notify:OnDungeonTriggerAmbientSound
// parity: notify:OnDungeonTriggerAudio, notify:OnDungeonTriggerMessage
// parity: notify:OnDungeonTriggerMusic, notify:OnEssReserveBankInventoryFull
// parity: notify:OnJumpQueueUpdate
#[tokio::test]
async fn test_parity_notify_missing1_dungeon_and_space() {
    let ctx = ServerContext::new();
    let ship_a = 700_801_i64;
    let ship_b = 700_802_i64;
    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, ship_a, 587, SYS_JITA, false).await;
    let mut sess_b = setup_pilot_in_space(&ctx, PILOT_B, ship_b, 587, SYS_PERIMETER, false).await;

    let handle = ctx.systems.get_or_spawn(SYS_JITA);
    // Ship begins out of trigger range (50 km away, radius 5 km)
    let _ = handle
        .spawn_ball(make_ball(ship_a, 587, DVec3::new(50_000.0, 0.0, 0.0)))
        .await;

    let inst_id = 99_101_i64;
    let dungeon_id = 27_u32;
    let approach_kind = DungeonTriggerKind::OnApproach {
        target_ball_id: None,
        target_object_index: None,
        anchor_pos: [0.0, 0.0, 0.0],
        radius_m: 5_000.0,
    };
    with_dungeon_runtime_store_db(&ctx.db, |store| {
        store.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id,
                dungeon_name: "Parity Dungeon".to_string(),
                category: "combat".to_string(),
                archetype: "caldari".to_string(),
                solar_system_id: SYS_JITA,
                owner_char_id: PILOT_A,
                base_position: [0.0, 0.0, 0.0],
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 0,
                created_unix: 1_700_000_000,
                completed: false,
                despawned: false,
                active_objective_id: 0,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
                rooms: vec![DungeonRuntimeRoom {
                    room_index: 0,
                    room_id: 1,
                    room_name: "Room 1".to_string(),
                    position: [0.0, 0.0, 0.0],
                    gate_destination_pos: [0.0, 0.0, 0.0],
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: false,
                    spawned: true,
                    cleared: false,
                    objects: vec![DungeonRuntimeObject {
                        ball_id: 900_777,
                        type_id: 29624,
                        name: "Trigger Beacon".to_string(),
                        position: [0.0, 0.0, 0.0],
                        radius: 100.0,
                        is_container: false,
                        loot_items: vec![],
                        spawned: true,
                    }],
                    npc_groups: vec![],
                    triggers: vec![
                        DungeonTrigger {
                            trigger_id: 1,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::CameraInterest {
                                item_id: 900_777,
                                speed_bits: 2.0_f64.to_bits(),
                                duration_bits: 5.0_f64.to_bits(),
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 2,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerAmbientSound {
                                sound_id: 301,
                                entity_item_id: 900_777,
                                emitter_id: 12,
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 3,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerAudio {
                                audio_url: "wise:/dungeon_alert_02".to_string(),
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 4,
                            kind: approach_kind.clone(),
                            action: DungeonTriggerAction::TriggerMessage {
                                message_type: 1,
                                message_id: 4001,
                            },
                            fired: false,
                        },
                        DungeonTrigger {
                            trigger_id: 5,
                            kind: approach_kind,
                            action: DungeonTriggerAction::TriggerMusic {
                                music_trigger_id: 888,
                            },
                            fired: false,
                        },
                    ],
                }],
            },
        );
    });

    // 1-5. Negative: ship out of trigger radius (50 km > 5 km) -> zero triggers fire
    let fired_none = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_a).await;
    assert_eq!(fired_none, 0);
    let neg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!has_notif(&neg_notifs, "OnDungeonCameraInterest"));
    assert!(!has_notif(&neg_notifs, "OnDungeonTriggerAmbientSound"));
    assert!(!has_notif(&neg_notifs, "OnDungeonTriggerAudio"));
    assert!(!has_notif(&neg_notifs, "OnDungeonTriggerMessage"));
    assert!(!has_notif(&neg_notifs, "OnDungeonTriggerMusic"));

    // Positive: move ship inside trigger radius (1 km <= 5 km) -> all 5 triggers fire
    let _ = handle
        .spawn_ball(make_ball(ship_a, 587, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    let fired_all = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_a).await;
    assert_eq!(fired_all, 5);

    let notifs = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 1. OnDungeonCameraInterest
    let cam = find_notif(&notifs, "OnDungeonCameraInterest");
    assert_eq!(cam.len(), 3);
    assert_eq!(cam[0].as_i64(), Some(900_777));
    assert_eq!(cam[1].as_f64(), Some(2.0));
    assert_eq!(cam[2].as_f64(), Some(5.0));

    // 2. OnDungeonTriggerAmbientSound
    let amb = find_notif(&notifs, "OnDungeonTriggerAmbientSound");
    assert_eq!(amb.len(), 3);
    assert_eq!(amb[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(amb[1].as_i64(), Some(301));
    assert_eq!(amb[2].as_i64(), Some(900_777));

    // 3. OnDungeonTriggerAudio
    let aud = find_notif(&notifs, "OnDungeonTriggerAudio");
    assert_eq!(aud.len(), 2);
    assert_eq!(aud[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(aud[1].as_str(), Some("wise:/dungeon_alert_02"));

    // 4. OnDungeonTriggerMessage
    let msg = find_notif(&notifs, "OnDungeonTriggerMessage");
    assert_eq!(msg.len(), 2);
    assert_eq!(msg[0].as_i64(), Some(1));
    assert_eq!(msg[1].as_i64(), Some(4001));

    // 5. OnDungeonTriggerMusic
    let mus = find_notif(&notifs, "OnDungeonTriggerMusic");
    assert_eq!(mus.len(), 2);
    assert_eq!(mus[0].as_i64(), Some(i64::from(dungeon_id)));
    assert_eq!(mus[1].as_i64(), Some(888));

    // Idempotency negative: triggers already marked fired do not re-emit
    let fired_again = check_dungeon_triggers(&ctx, PILOT_A, SYS_JITA, ship_a).await;
    assert_eq!(fired_again, 0);
    assert!(drain_all(&ctx, &mut sess_a, PILOT_A).is_empty());

    // 6. OnEssReserveBankInventoryFull
    // Negative: Pilot A initially has no ESS reserve full notification
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_a, PILOT_A),
        "OnEssReserveBankInventoryFull"
    ));
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_b, PILOT_B),
        "OnEssReserveBankInventoryFull"
    ));

    // Positive: system reserve bank capacity reached -> emits to solar system
    emit_ess_reserve_bank_inventory_full(&ctx, SYS_JITA);
    let notifs_ess_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ess = find_notif(&notifs_ess_a, "OnEssReserveBankInventoryFull");
    assert_eq!(ess.len(), 0, "OnEssReserveBankInventoryFull arity is 0");

    // Negative: Pilot B in SYS_PERIMETER does NOT receive SYS_JITA's ESS full notification
    let notifs_ess_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(!has_notif(&notifs_ess_b, "OnEssReserveBankInventoryFull"));

    // 7. OnJumpQueueUpdate
    // Negative: Pilot A initially has no jump queue notification
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_a, PILOT_A),
        "OnJumpQueueUpdate"
    ));

    // Positive: pilot queued at stargate receives OnJumpQueueUpdate with 5 V24 args
    emit_jump_queue_update(&ctx, PILOT_A, SYS_JITA, 40, 3, vec![101, 102], 180);
    let notifs_jq = drain_all(&ctx, &mut sess_a, PILOT_A);
    let jq = find_notif(&notifs_jq, "OnJumpQueueUpdate");
    assert_eq!(jq.len(), 5);
    assert_eq!(jq[0].as_i64(), Some(i64::from(SYS_JITA)));
    assert_eq!(jq[1].as_i64(), Some(40));
    assert_eq!(jq[2].as_i64(), Some(3));
    assert!(matches!(&jq[3], PyRep::List(keys) if keys.len() == 2));
    assert_eq!(jq[4].as_i64(), Some(180));

    // Negative: Pilot B was not in the jump queue -> 0 OnJumpQueueUpdate
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_b, PILOT_B),
        "OnJumpQueueUpdate"
    ));
}

// parity: notify:OnBeanPrime, notify:OnCCPNotification, notify:OnClearFetchedData
// parity: notify:OnContextualOfferPurchased, notify:OnFlaggedListsChange
// parity: notify:OnLoginMilestoneAchieved, notify:OnNotificationUndeleted
#[tokio::test]
async fn test_parity_notify_missing1_cluster_account_notifications() {
    let ctx = ServerContext::new();
    let mut sess_a = setup_pilot_in_space(&ctx, PILOT_A, 700_901, 587, SYS_JITA, false).await;
    let mut sess_b = setup_pilot_in_space(&ctx, PILOT_B, 700_902, 587, SYS_PERIMETER, false).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 8. OnBeanPrime
    // Negative: session initially has no OnBeanPrime
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnBeanPrime"));

    // Positive 8a: alert.SendClientStackTraceAlert with new hash primes bean and emits OnBeanPrime
    let stack_hash = 0x4B3A_1122_i64;
    let stack_id = PyRep::Tuple(vec![PyRep::Integer(stack_hash), PyRep::ascii_bytes("err_k")]);
    let trace_resp = rpc(
        &ctx,
        &mut sess_a,
        "alert",
        "SendClientStackTraceAlert",
        &[
            stack_id,
            PyRep::ascii_bytes("EXCEPTION in test\nline 2"),
            PyRep::ascii_bytes("Error"),
            PyRep::None,
        ],
    )
    .await
    .expect("SendClientStackTraceAlert");
    assert!(matches!(trace_resp, PyRep::Tuple(_)));
    let notifs_bean = drain_all(&ctx, &mut sess_a, PILOT_A);
    let bean_args = find_notif(&notifs_bean, "OnBeanPrime");
    assert_eq!(bean_args.len(), 1);
    assert!(matches!(&bean_args[0], PyRep::Dict(entries) if entries.iter().any(|(k, _)| k.as_i64() == Some(stack_hash))));

    // Cluster broadcast also reached Pilot B:
    let notifs_bean_b_initial = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(has_notif(&notifs_bean_b_initial, "OnBeanPrime"));

    // Positive 8b: explicit emit_bean_prime targeted to character
    emit_bean_prime(
        &ctx,
        NotificationTarget::Character(PILOT_A),
        vec![(0x9988_7766, 44001, "B".into())],
    );
    let notifs_bean_b = drain_all(&ctx, &mut sess_a, PILOT_A);
    let bean_b = find_notif(&notifs_bean_b, "OnBeanPrime");
    assert_eq!(bean_b.len(), 1);
    assert!(matches!(&bean_b[0], PyRep::Dict(entries) if entries.iter().any(|(k, _)| k.as_i64() == Some(0x9988_7766))));

    // Negative: Pilot B did not receive the targeted notification
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnBeanPrime"));

    // 9. OnCCPNotification
    // Negative: Pilot B initially has no CCP notifications
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnCCPNotification"));

    // Positive: GM broadcast / downtime notice
    emit_ccp_notification(
        &ctx,
        NotificationTarget::Character(PILOT_A),
        "Downtime Notice",
        "Scheduled Maintenance",
        "Cluster restart in 15 minutes",
        1,
        88_991,
    );
    let notifs_ccp = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ccp = find_notif(&notifs_ccp, "OnCCPNotification");
    assert_eq!(ccp.len(), 6);
    assert_eq!(ccp[0].as_str(), Some("Downtime Notice"));
    assert_eq!(ccp[1].as_str(), Some("Scheduled Maintenance"));
    assert_eq!(ccp[2].as_str(), Some("Cluster restart in 15 minutes"));
    assert_eq!(ccp[3].as_i64(), Some(1));
    assert_eq!(ccp[4], PyRep::None);
    assert_eq!(ccp[5].as_i64(), Some(88_991));

    // Negative: Pilot B was not targeted -> 0 OnCCPNotification
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnCCPNotification"));

    // 10. OnClearFetchedData
    // Negative: prior to clear, zero OnClearFetchedData
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnClearFetchedData"));

    // Positive: cluster service cache / fetched data cleared
    emit_clear_fetched_data(&ctx);
    let notifs_clear = drain_all(&ctx, &mut sess_a, PILOT_A);
    let clr = find_notif(&notifs_clear, "OnClearFetchedData");
    assert_eq!(clr.len(), 0, "OnClearFetchedData arity is 0");

    // 11. OnContextualOfferPurchased
    // Negative 11a: telemetry TrackOfferPurchased does NOT emit OnContextualOfferPurchased
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "contextualOfferMgr",
        "TrackOfferPurchased",
        &[PyRep::Integer(6601)],
    )
    .await
    .expect("TrackOfferPurchased");
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_a, PILOT_A),
        "OnContextualOfferPurchased"
    ));

    // Negative 11b: invalid offer ID (<= 0) does not emit
    emit_contextual_offer_purchased(&ctx, PILOT_A, 0);
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_a, PILOT_A),
        "OnContextualOfferPurchased"
    ));

    // Positive: valid purchase emits OnContextualOfferPurchased(offerID, purchaseInfo)
    emit_contextual_offer_purchased(&ctx, PILOT_A, 6601);
    let notifs_offer = drain_all(&ctx, &mut sess_a, PILOT_A);
    let off = find_notif(&notifs_offer, "OnContextualOfferPurchased");
    assert_eq!(off.len(), 2);
    assert_eq!(off[0].as_i64(), Some(6601));
    assert!(matches!(&off[1], PyRep::Dict(entries) if entries.iter().any(|(k, v)| k.as_str() == Some("confirmed") && *v == PyRep::Bool(true))));

    // Negative: Pilot B did not buy offer 6601 -> 0 OnContextualOfferPurchased
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_b, PILOT_B),
        "OnContextualOfferPurchased"
    ));

    // 12. OnFlaggedListsChange
    // Negative: reading browser lockdown status does NOT emit OnFlaggedListsChange
    let _ = rpc(&ctx, &mut sess_a, "browserLockdownSvc", "IsBrowserInLockdown", &[])
        .await
        .expect("IsBrowserInLockdown");
    assert!(!has_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnFlaggedListsChange"));

    // Positive: server updates flagged browser lists -> broadcasts OnFlaggedListsChange
    emit_flagged_lists_change(&ctx);
    let notifs_flag = drain_all(&ctx, &mut sess_a, PILOT_A);
    let flg = find_notif(&notifs_flag, "OnFlaggedListsChange");
    assert_eq!(flg.len(), 0, "OnFlaggedListsChange arity is 0");

    // 13. OnLoginMilestoneAchieved
    // Negative: Pilot B initially has 0 OnLoginMilestoneAchieved
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnLoginMilestoneAchieved"));

    // Positive: milestone achieved emits OnLoginMilestoneAchieved(milestoneID, iskAmount)
    emit_login_milestone_achieved(&ctx, PILOT_A, 401, 1_250_000.0);
    let notifs_mile = drain_all(&ctx, &mut sess_a, PILOT_A);
    let mile = find_notif(&notifs_mile, "OnLoginMilestoneAchieved");
    assert_eq!(mile.len(), 2);
    assert_eq!(mile[0].as_i64(), Some(401));
    assert_eq!(mile[1].as_f64(), Some(1_250_000.0));

    // Negative: Pilot B receives 0 OnLoginMilestoneAchieved
    assert!(!has_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnLoginMilestoneAchieved"));

    // 14. OnNotificationUndeleted
    // Setup persistent notification records for PILOT_A
    let n1_id = 9_801_i64;
    let n2_id = 9_802_i64;
    let now_u = ctx.now_unix();
    {
        let mut recs = ctx.notification_records.write().unwrap();
        recs.push(NotificationRecord {
            notification_id: n1_id,
            type_id: 1,
            sender_id: 1_000_001,
            receiver_id: PILOT_A,
            processed: false,
            group_id: 1,
            created: now_u as i64,
            data: "{}".into(),
            deleted: false,
            interacted: false,
        });
        recs.push(NotificationRecord {
            notification_id: n2_id,
            type_id: 1,
            sender_id: 1_000_001,
            receiver_id: PILOT_A,
            processed: false,
            group_id: 1,
            created: now_u as i64,
            data: "{}".into(),
            deleted: false,
            interacted: false,
        });
    }

    // Delete notifications via notificationMgr.DeleteNotifications
    let del_res = rpc(
        &ctx,
        &mut sess_a,
        "notificationMgr",
        "DeleteNotifications",
        &[PyRep::List(vec![PyRep::Integer(n1_id), PyRep::Integer(n2_id)])],
    )
    .await
    .expect("DeleteNotifications");
    assert_eq!(del_res, PyRep::Integer(2));
    let notifs_del = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(has_notif(&notifs_del, "OnNotificationDeleted"));
    assert!(!has_notif(&notifs_del, "OnNotificationUndeleted"));

    // Positive: restore_character_notifications un-deletes records and emits OnNotificationUndeleted
    let restored = restore_character_notifications(&ctx, PILOT_A, &[n1_id, n2_id]);
    assert_eq!(restored, 2);
    let notifs_undel = drain_all(&ctx, &mut sess_a, PILOT_A);
    let undel = find_notif(&notifs_undel, "OnNotificationUndeleted");
    assert_eq!(undel.len(), 1);
    assert!(matches!(&undel[0], PyRep::List(ids) if ids.len() == 2 && ids[0].as_i64() == Some(n1_id) && ids[1].as_i64() == Some(n2_id)));

    // Negative: Pilot B did not have notifications restored -> 0 OnNotificationUndeleted
    assert!(!has_notif(
        &drain_all(&ctx, &mut sess_b, PILOT_B),
        "OnNotificationUndeleted"
    ));
}
