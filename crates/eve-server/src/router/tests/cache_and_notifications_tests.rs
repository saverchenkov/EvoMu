//! Unit tests for object caching, CRC32 encoding, and notification routing in `router::cache_notifications`.

use crate::router::{cache_object_key, crc32_ieee, NotificationTarget, ServerContext};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn object_cache_encoders_population_and_invalidation() {
    let ctx = test_ctx();
    assert_eq!(crc32_ieee(b"123456789") as u32, 0xCBF4_3926);

    assert_eq!(cache_object_key(&PyRep::String("abc".into())), "abc");
    assert_eq!(cache_object_key(&PyRep::Integer(42)), "42");
    assert_eq!(
        cache_object_key(&PyRep::Tuple(vec![
            PyRep::String("a".into()),
            PyRep::Integer(1)
        ])),
        "(a,1)"
    );

    let rec = ctx.put_cached_object(
        PyRep::String("test.cache".into()),
        PyRep::Integer(123),
        true,
    );
    assert!(matches!(rec.to_cached_object_pyrep(), PyRep::Object { .. }));
    assert!(matches!(rec.to_cache_hint_pyrep(), PyRep::Object { .. }));
    assert!(matches!(
        rec.to_cached_method_call_result_pyrep("now", Some("charid")),
        PyRep::Object { .. }
    ));
    assert!(matches!(
        rec.to_cached_method_call_hint_pyrep("server"),
        PyRep::Object { .. }
    ));

    let wrapped = ctx.cache_method_call_result("config", "Ping", PyRep::Bool(true), "now", None);
    assert!(matches!(wrapped, PyRep::Object { .. }));
    assert!(ctx.invalidate_cached_method("config", "Ping"));
    assert!(!ctx.invalidate_cached_method("config", "Ping"));

    // Populate all built-in SDE cache keys
    for key in [
        "config.Units",
        "config.MarketGroups",
        "config.Groups",
        "config.Categories",
        "config.StaticOwners",
        "(Method Call,server,(stationSvc,GetAllianceSystems))",
        "config.Races",
        "config.Bloodlines",
        "config.Ancestries",
        "config.Schools",
    ] {
        let oid = PyRep::String(key.into());
        let first = ctx
            .get_or_populate_cached_object(&oid, true)
            .unwrap_or_else(|| panic!("expected cached population for {key}"));
        let second = ctx.get_or_populate_cached_object(&oid, true).unwrap();
        assert_eq!(first.version, second.version);
    }
    assert!(ctx
        .get_or_populate_cached_object(&PyRep::String("config.NonExistentKey".into()), true)
        .is_none());
}

#[test]
fn notifications_recipients_packets_and_local_chat_sync() {
    let ctx = test_ctx();
    let c1 = 90_000_001;
    let c2 = 90_000_002;

    let (u1, u2, corp_id) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&c1].user_id,
            s.characters[&c2].user_id,
            s.characters[&c1].corporation_id,
        )
    });
    let _ = ctx
        .db
        .acquire_online_lock(u1, 50_001, Some(c1), true, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(u2, 50_002, Some(c2), true, ctx.now_unix());

    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Character(c1)),
        vec![(c1, Some(u1))]
    );
    assert!(ctx
        .resolve_notification_recipients(&NotificationTarget::Corporation(corp_id))
        .iter()
        .any(|&(cid, _)| cid == c1));
    assert!(ctx
        .resolve_notification_recipients(&NotificationTarget::Station(60_003_760))
        .iter()
        .any(|&(cid, _)| cid == c1));
    assert!(ctx
        .resolve_notification_recipients(&NotificationTarget::SolarSystem(30_000_142))
        .iter()
        .any(|&(cid, _)| cid == c1));
    assert!(
        ctx.resolve_notification_recipients(&NotificationTarget::All)
            .len()
            >= 2
    );

    ctx.notify(
        NotificationTarget::Character(c1),
        "OnSkillQueueChanged",
        "charid",
        vec![PyRep::Integer(7)],
    );
    let drained = ctx.drain_notifications_for_char(c1);
    assert_eq!(drained.len(), 1);

    let (_pkt, raw_notif) = ServerContext::build_macho_notification_packet(
        c1,
        Some(1),
        "OnSkillQueueChanged",
        "charid",
        vec![PyRep::Integer(7)],
    );
    assert!(!raw_notif.is_empty());
    let (_ok_pkt, raw_ok) =
        ServerContext::build_macho_call_response_packet(None, 10, 99, Some(1), PyRep::Bool(true));
    assert!(!raw_ok.is_empty());
    let (_err_pkt, raw_err) = ServerContext::build_macho_error_response_packet(
        None,
        10,
        99,
        Some(1),
        "Err",
        "bad",
    );
    assert!(!raw_err.is_empty());

    ctx.notify_remote_message(
        NotificationTarget::Character(c1),
        "remote",
        vec![("msg", PyRep::None)],
    );
    ctx.notify_lsc_message(
        NotificationTarget::Character(c2),
        &format!("corpid_{corp_id}"),
        c1,
        "Hello local",
    );
    ctx.notify_warp_to_asteroid_belt(c1, 40_000_001);
    ctx.notify_asteroid_terminated(30_000_142, 70_000_001);
    ctx.notify_mining_ledger_graph_setting_changed(c1, "viewMode", PyRep::Integer(1));
    ctx.notify_module_effect_started(c1, 100_000_001, 500_001, 587, None, None);
    ctx.notify_module_online_state(c1, 500_001, false);
    ctx.notify_target_events(c1, &[("add", 100_000_002, "target")]);
    ctx.notify_system_criminal_flag_updates(30_000_142, &[], &[c1], &[c2]);
    ctx.notify_fitted_item_trashed(c1, 500_001, 587, 1, 100_000_001, 27);

    ctx.sync_character_local_chat(c1, Some(30_000_142), 30_000_144);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&c1].solar_system_id),
        30_000_142
    );
    assert!(ctx
        .db
        .with_snapshot(|s| s.chat_channels["solarsystemid2:30000144"].members.contains(&c1)));

    // Skill queue paused/changed + persistent character notification storage & login delivery
    assert!(!ctx.notify_skill_queue_paused(c1).is_empty());
    assert!(!ctx.notify_skill_queue_changed(c1).is_empty());

    let nrec = ctx.create_character_notification(1, c1, 10, 1, "subject: hi\n");
    assert!(!nrec.processed);
    ctx.sync_character_notifications_to_db(c1);
    ctx.pending_notifications.write().unwrap().remove(&c1);
    ctx.deliver_unprocessed_notifications_on_login(c1);
    assert!(ctx.pending_notifications.read().unwrap().get(&c1).is_some_and(|v| !v.is_empty()));
}

