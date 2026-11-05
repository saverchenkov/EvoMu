//! Unit tests for `notify_emitters_missing`.

use super::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn make_online_ctx() -> (ServerContext, i64) {
    let sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn emit_effect_plex_bean_clear_flagged_and_market_notifications() {
    let (ctx, char_id) = make_online_ctx();

    emit_effect_message(
        &ctx,
        char_id,
        "ModuleJam",
        &[("moduleID", PyRep::Integer(1001)), ("jammer", PyRep::String("ECM".into()))],
    );
    emit_plex_deposit_failed(&ctx, char_id, "InsufficientItems", PyRep::Integer(7001));
    emit_plex_withdraw_failed(&ctx, char_id, "VaultLocked", PyRep::String("ref-2".into()));
    emit_bean_prime(
        &ctx,
        NotificationTarget::Character(char_id),
        vec![(12345, 99, "stacktrace".into())],
    );
    emit_clear_fetched_data(&ctx);
    emit_flagged_lists_change(&ctx);
    emit_client_event_market_item_received(&ctx, char_id, 900_001, 34, 4, 60_003_760);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 7);

    assert_eq!(drained[0].event, "OnEffectMessage");
    assert_eq!(drained[0].id_type, "charid");
    assert_eq!(
        drained[0].args,
        vec![
            PyRep::String("ModuleJam".into()),
            PyRep::Dict(vec![
                (PyRep::String("moduleID".into()), PyRep::Integer(1001)),
                (PyRep::String("jammer".into()), PyRep::String("ECM".into())),
            ])
        ]
    );

    assert_eq!(drained[1].event, "OnPlexDepositFailed");
    assert_eq!(
        drained[1].args,
        vec![PyRep::String("InsufficientItems".into()), PyRep::Integer(7001)]
    );

    assert_eq!(drained[2].event, "OnPlexWithdrawFailed");
    assert_eq!(
        drained[2].args,
        vec![PyRep::String("VaultLocked".into()), PyRep::String("ref-2".into())]
    );

    assert_eq!(drained[3].event, "OnBeanPrime");
    assert_eq!(
        drained[3].args,
        vec![PyRep::Dict(vec![(
            PyRep::Integer(12345),
            PyRep::Tuple(vec![PyRep::Integer(99), PyRep::String("stacktrace".into())]),
        )])]
    );

    assert_eq!(drained[4].event, "OnClearFetchedData");
    assert_eq!(drained[4].id_type, "clientID");
    assert!(drained[4].args.is_empty());

    assert_eq!(drained[5].event, "OnFlaggedListsChange");
    assert_eq!(drained[5].id_type, "clientID");
    assert!(drained[5].args.is_empty());

    assert_eq!(drained[6].event, "OnClientEvent_MarketItemReceived");
    assert_eq!(
        drained[6].args,
        vec![
            PyRep::Integer(900_001),
            PyRep::Integer(34),
            PyRep::Integer(4),
            PyRep::Integer(60_003_760),
        ]
    );
}

#[test]
fn restore_character_notifications_only_restores_matching_deleted_records() {
    let (ctx, char_id) = make_online_ctx();

    let n1 = ctx.create_character_notification(1_000_125, char_id, 10, 1, "first");
    let n2 = ctx.create_character_notification(1_000_125, char_id, 10, 1, "second");
    let n_other = ctx.create_character_notification(1_000_125, 90_000_002, 10, 1, "other");
    let _ = ctx.drain_notifications_for_char(char_id);

    // Mark n1 and n_other as deleted; leave n2 undeleted
    {
        let mut records = ctx.notification_records.write().unwrap();
        for r in records.iter_mut() {
            if r.notification_id == n1.notification_id || r.notification_id == n_other.notification_id {
                r.deleted = true;
            }
        }
    }

    // Attempting to restore n2 (not deleted) or n_other (different receiver) returns 0 and emits nothing
    let count0 = restore_character_notifications(&ctx, char_id, &[n2.notification_id, n_other.notification_id]);
    assert_eq!(count0, 0);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Restoring n1 succeeds, clears deleted flag, and emits OnNotificationUndeleted
    let count1 = restore_character_notifications(&ctx, char_id, &[n1.notification_id, n2.notification_id]);
    assert_eq!(count1, 1);
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].event, "OnNotificationUndeleted");
    assert_eq!(
        drained[0].args,
        vec![PyRep::List(vec![PyRep::Integer(n1.notification_id)])]
    );

    // Second restore of n1 is a no-op because deleted is now false
    assert_eq!(restore_character_notifications(&ctx, char_id, &[n1.notification_id]), 0);
}

#[test]
fn emit_dungeon_camera_and_trigger_notifications() {
    let (ctx, char_id) = make_online_ctx();

    emit_dungeon_camera_interest(&ctx, char_id, 70_001, 2.5, 8.0);
    emit_dungeon_trigger_ambient_sound(&ctx, char_id, 400, 12, 70_002);
    emit_dungeon_trigger_audio(&ctx, char_id, 400, "res:/Audio/dungeon_intro.ogg");
    emit_dungeon_trigger_message(&ctx, char_id, 3, 5500);
    emit_dungeon_trigger_music(&ctx, char_id, 400, 901);

    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 5);

    assert_eq!(drained[0].event, "OnDungeonCameraInterest");
    assert_eq!(
        drained[0].args,
        vec![PyRep::Integer(70_001), PyRep::Real(2.5), PyRep::Real(8.0)]
    );

    assert_eq!(drained[1].event, "OnDungeonTriggerAmbientSound");
    assert_eq!(
        drained[1].args,
        vec![
            PyRep::Integer(400),
            PyRep::Integer(12),
            PyRep::Integer(70_002)
        ]
    );

    assert_eq!(drained[2].event, "OnDungeonTriggerAudio");
    assert_eq!(
        drained[2].args,
        vec![
            PyRep::Integer(400),
            PyRep::String("res:/Audio/dungeon_intro.ogg".into())
        ]
    );

    assert_eq!(drained[3].event, "OnDungeonTriggerMessage");
    assert_eq!(
        drained[3].args,
        vec![PyRep::Integer(3), PyRep::Integer(5500)]
    );

    assert_eq!(drained[4].event, "OnDungeonTriggerMusic");
    assert_eq!(
        drained[4].args,
        vec![PyRep::Integer(400), PyRep::Integer(901)]
    );
}
