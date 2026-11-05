//! Unit and mutation-killing tests for `router::cache_notifications`.

use super::*;
use crate::router::ServerContext;
use eve_db::{FleetRecord, SystemDynamicState, WorldRepository};
use eve_machonet::MachoPacketType;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn crc32_cache_keys_and_object_caching_lifecycle_preserve_monotonic_timestamps() {
    // IEEE 802.3 CRC32 standard check value for "123456789" is 0xCBF43926
    assert_eq!(crc32_ieee(b""), 0);
    assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926_u32 as i32);
    assert_ne!(crc32_ieee(b"123456789"), crc32_ieee(b"123456780"));

    assert_eq!(cache_object_key(&PyRep::String("config.Units".into())), "config.Units");
    assert_eq!(cache_object_key(&PyRep::Integer(42)), "42");
    assert_eq!(
        cache_object_key(&ServerContext::method_call_object_id("stationSvc", "GetAllianceSystems")),
        "(Method Call,server,(stationSvc,GetAllianceSystems))"
    );

    let ctx = test_ctx();
    let oid = ServerContext::method_call_object_id("stationSvc", "GetStation");
    let r1 = ctx.put_cached_object(oid.clone(), PyRep::Integer(100), true);
    assert!(matches!(r1.to_cached_object_pyrep(), PyRep::Object { ref type_name, .. } if type_name == "objectCaching.CachedObject"));
    assert!(matches!(r1.to_cache_hint_pyrep(), PyRep::Object { ref type_name, .. } if type_name == "util.CachedObject"));
    assert!(matches!(r1.to_cached_method_call_result_pyrep("run", Some("solarsystemid2")), PyRep::Object { ref type_name, .. } if type_name == "objectCaching.CachedMethodCallResult"));
    assert!(matches!(r1.to_cached_method_call_hint_pyrep("run"), PyRep::Object { ref type_name, .. } if type_name == "objectCaching.CachedMethodCallResult"));

    // Invalidate and re-insert: timestamp must strictly advance (> r1.timestamp)
    assert!(ctx.invalidate_cached_method("stationSvc", "GetStation"));
    assert!(!ctx.invalidate_cached_method("stationSvc", "GetStation"));
    let r2 = ctx.put_cached_object(oid, PyRep::Integer(200), false);
    assert!(r2.timestamp > r1.timestamp);

    let wrapped = ctx.cache_method_call_result("stationSvc", "GetStation", PyRep::Bool(true), "run", None);
    assert!(matches!(wrapped, PyRep::Object { .. }));
}

#[test]
fn get_or_populate_cached_object_synthesizes_all_bulk_sde_and_db_keys() {
    let ctx = test_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
    });

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
        let rec = ctx
            .get_or_populate_cached_object(&oid, true)
            .unwrap_or_else(|| panic!("expected synthesized cache entry for {key}"));
        assert!(!rec.marshaled_bytes.is_empty());
        // Second lookup hits in-memory object_cache directly
        let cached = ctx.get_or_populate_cached_object(&oid, true).unwrap();
        assert_eq!(cached.version, rec.version);
    }

    assert!(
        ctx.get_or_populate_cached_object(&PyRep::String("nonexistent.Key".into()), true)
            .is_none()
    );
}

#[test]
fn macho_packets_notification_targets_and_helper_emitters_work_end_to_end() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let corp_id = 1_000_009_i64;
    let ally_id = 99_000_001_i64;
    let fleet_id = 700_001_i64;

    let (n_pkt, n_bytes) = ServerContext::build_macho_notification_packet(
        char_id,
        Some(1),
        "OnAccountChange",
        "charid",
        vec![PyRep::Integer(1)],
    );
    assert_eq!(n_pkt.packet_type, MachoPacketType::Notification);
    assert!(!n_bytes.is_empty());

    let (rsp_pkt, rsp_bytes) = ServerContext::build_macho_call_response_packet(
        Some("charMgr"),
        10,
        20,
        Some(1),
        PyRep::Bool(true),
    );
    assert_eq!(rsp_pkt.packet_type, MachoPacketType::CallRsp);
    assert!(!rsp_bytes.is_empty());

    let err_code = ["Invalid", "Argument"].concat();
    let (err_pkt, err_bytes) = ServerContext::build_macho_error_response_packet(
        None,
        10,
        20,
        Some(1),
        &err_code,
        "Invalid argument",
    );
    assert_eq!(err_pkt.packet_type, MachoPacketType::ErrorResponse);
    assert!(!err_bytes.is_empty());

    // Mark char_id online and configure corp/alliance/fleet/station/solar_system
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 101, Some(char_id), false, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
            ch.solar_system_id = sys_id;
            ch.station_id = Some(st_id);
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
    });

    assert!(ctx.is_character_online(char_id, uid));
    for target in [
        NotificationTarget::Character(char_id),
        NotificationTarget::Corporation(corp_id),
        NotificationTarget::Alliance(ally_id),
        NotificationTarget::Fleet(fleet_id),
        NotificationTarget::SolarSystem(sys_id),
        NotificationTarget::Station(st_id),
        NotificationTarget::All,
    ] {
        let rcpts = ctx.resolve_notification_recipients(&target);
        assert!(rcpts.iter().any(|&(cid, _)| cid == char_id));
    }

    // Queue cap eviction at MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)
    for i in 0..(MAX_PENDING_NOTIFICATIONS_PER_CHAR + 5) {
        let _ = ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![PyRep::Integer(i as i64)],
        );
    }
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Exercise all notification helper methods & persistent character notifications
    assert_eq!(
        ctx.notify_remote_message(
            NotificationTarget::Character(char_id),
            "CustomInfo",
            vec![("msg", PyRep::String("hi".into()))]
        )
        .len(),
        1
    );
    assert_eq!(ctx.notify_account_change(char_id, "cash", char_id, 1000.0).len(), 1);
    assert_eq!(ctx.notify_item_change(char_id, PyRep::None, vec![(2, PyRep::Integer(1))]).len(), 1);
    assert_eq!(ctx.notify_skill_trained(char_id, 10, 3300, 3).len(), 1);
    assert_eq!(ctx.notify_skill_queue_paused(char_id).len(), 1);
    assert_eq!(ctx.notify_skill_queue_changed(char_id).len(), 1);
    assert_eq!(
        ctx.notify_damage_state_change(NotificationTarget::Character(char_id), 100, 0.9, 0.8, 1.0)
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_godma_ship_effect(NotificationTarget::Character(char_id), 100, 10, true, 5000, 1)
            .len(),
        1
    );
    assert_eq!(ctx.notify_fleet_broadcast(fleet_id, "Target", char_id, 999).len(), 1);
    assert_eq!(
        ctx.notify_lsc_message(NotificationTarget::Character(char_id), "local", char_id, "o7")
            .len(),
        1
    );
    assert_eq!(ctx.notify_warp_to_asteroid_belt(char_id, 40_009_077).len(), 1);
    assert_eq!(ctx.notify_asteroid_terminated(sys_id, 700_111).len(), 1);
    assert_eq!(
        ctx.notify_mining_ledger_graph_setting_changed(char_id, "filter", PyRep::Bool(true))
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_module_effect_started(char_id, 100_000_001, 500, 2048, None, None)
            .len(),
        1
    );
    assert_eq!(ctx.notify_module_online_state(char_id, 500, true).len(), 1);
    assert_eq!(ctx.notify_target_events(char_id, &[("add", 999, "")]).len(), 1);
    assert_eq!(
        ctx.notify_system_criminal_flag_updates(sys_id, &[char_id], &[], &[])
            .len(),
        1
    );
    assert_eq!(ctx.notify_fitted_item_trashed(char_id, 500, 2048, 1, 100_000_001, 11).len(), 1);

    let _ = ctx.drain_notifications_for_char(char_id);
    let rec = ctx.create_character_notification(1_000_009, char_id, 5, 1, "text: hello");
    assert_eq!(rec.receiver_id, char_id);
    ctx.sync_character_notifications_to_db(char_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);

    ctx.sync_character_local_chat(char_id, Some(sys_id), 30_000_144);
    assert!(ctx.db.with_snapshot(|s| {
        s.chat_channels
            .get("solarsystemid2:30000144")
            .is_some_and(|c| c.members.contains(&char_id))
    }));
}
