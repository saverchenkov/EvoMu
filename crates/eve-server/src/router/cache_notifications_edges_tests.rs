//! Edge-case and full-branch unit tests for `router::cache_notifications`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::GatewaySubscriber;
use crate::router::ServerContext;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord,
    SystemDynamicState, WorldRepository,
};
use eve_machonet::{MachoAddress, MachoPacketType};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, VecDeque};
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
fn crc32_cache_object_key_and_cached_object_record_encodings() {
    assert_eq!(crc32_ieee(b""), 0);
    assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926_u32 as i32);
    assert_ne!(crc32_ieee(b"\x00"), crc32_ieee(b"\x01"));

    assert_eq!(cache_object_key(&PyRep::String("abc".into())), "abc");
    assert_eq!(cache_object_key(&PyRep::Token("tok".into())), "tok");
    assert_eq!(cache_object_key(&PyRep::Integer(-99)), "-99");
    assert_eq!(
        cache_object_key(&PyRep::Tuple(vec![
            PyRep::Token("Method".into()),
            PyRep::Integer(7),
            PyRep::Bool(true),
        ])),
        "(Method,7,{\"$py\":\"Bool\",\"val\":true})"
    );
    let list_key = cache_object_key(&PyRep::List(vec![PyRep::Integer(1), PyRep::None]));
    assert!(!list_key.is_empty());

    let ctx = test_ctx();
    let oid = ServerContext::method_call_object_id("cfg", "GetBulk");

    // Small payload (uncompressed, shared=false)
    let small = ctx.put_cached_object(oid.clone(), PyRep::Integer(42), false);
    assert!(!small.shared);
    assert!(!small.compressed);
    let PyRep::Object { ref type_name, ref args } = small.to_cached_object_pyrep() else {
        panic!("expected Object");
    };
    assert_eq!(type_name, eve_proto::marshal::V24_CACHED_OBJECT_CLASS);
    let PyRep::Tuple(ref fields) = **args else { panic!("expected Tuple") };
    assert_eq!(fields[3], PyRep::Integer(0)); // shared = 0
    assert_eq!(fields[5], PyRep::Integer(0)); // compressed = 0

    // Direct overwrite without invalidating advances timestamp strictly
    let updated = ctx.put_cached_object(oid.clone(), PyRep::Integer(43), true);
    assert!(updated.shared);
    assert!(updated.timestamp > small.timestamp);

    // Large repetitive payload triggers zlib compression in marshal::encode
    let big_payload = PyRep::List(vec![PyRep::String("AAAAAAAAAABBBBBBBBBB".into()); 128]);
    let big = ctx.put_cached_object(PyRep::String("big.Key".into()), big_payload, true);
    let PyRep::Object { args: big_args, .. } = big.to_cached_object_pyrep() else {
        panic!("expected Object");
    };
    let PyRep::Tuple(big_fields) = *big_args else { panic!("expected Tuple") };
    assert_eq!(big_fields[3], PyRep::Integer(1));
    assert_eq!(
        big_fields[5],
        PyRep::Integer(if big.compressed { 1 } else { 0 })
    );

    // Inline method call result with session_info=None vs Some(...)
    let r_none = updated.to_cached_method_call_result_pyrep("run", None);
    let PyRep::Object { args: r_none_args, .. } = r_none else { panic!("expected Object") };
    let PyRep::Tuple(r_none_fields) = *r_none_args else { panic!("expected Tuple") };
    let PyRep::Dict(d_none) = &r_none_fields[0] else { panic!("expected Dict") };
    assert_eq!(d_none.len(), 1);

    let r_some = updated.to_cached_method_call_result_pyrep("always", Some("corp_id"));
    let PyRep::Object { args: r_some_args, .. } = r_some else { panic!("expected Object") };
    let PyRep::Tuple(r_some_fields) = *r_some_args else { panic!("expected Tuple") };
    let PyRep::Dict(d_some) = &r_some_fields[0] else { panic!("expected Dict") };
    assert_eq!(d_some.len(), 2);

    let hint = updated.to_cache_hint_pyrep();
    assert!(matches!(
        hint,
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHE_HINT_CLASS
    ));
    let call_hint = updated.to_cached_method_call_hint_pyrep("run");
    assert!(matches!(
        call_hint,
        PyRep::Object { ref type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
    ));

    let wrapped = ctx.cache_method_call_result("svc", "meth", PyRep::Bool(false), "run", Some("sys"));
    assert!(matches!(wrapped, PyRep::Object { .. }));
    assert!(ctx.invalidate_cached_method("svc", "meth"));
    assert!(!ctx.invalidate_cached_method("svc", "meth"));
    assert!(!ctx.invalidate_cached_object(&PyRep::String("never.inserted".into())));
}

#[test]
fn get_or_populate_cached_object_covers_all_aliases_and_empty_vs_populated_sov() {
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
        snap.system_states.insert(
            30_000_144,
            SystemDynamicState {
                solar_system_id: 30_000_144,
                sov_alliance_id: None,
                ..Default::default()
            },
        );
    });

    for key in [
        "(Method Call,server,(config,GetUnits))",
        "config.Units",
        "config.BulkData.units",
        "(Method Call,server,(marketProxy,GetMarketGroups))",
        "config.MarketGroups",
        "config.Groups",
        "config.Categories",
        "config.StaticOwners",
        "(Method Call,server,(stationSvc,GetAllianceSystems))",
        "config.Races",
        "charCreationInfo.races",
        "config.Bloodlines",
        "charCreationInfo.bloodlines",
        "config.Ancestries",
        "charCreationInfo.ancestries",
        "config.Schools",
        "charCreationInfo.schools",
    ] {
        let oid = PyRep::String(key.into());
        let first = ctx
            .get_or_populate_cached_object(&oid, false)
            .unwrap_or_else(|| panic!("expected synthesized entry for {key}"));
        assert!(!first.marshaled_bytes.is_empty());
        let second = ctx.get_or_populate_cached_object(&oid, true).unwrap();
        assert_eq!(second.timestamp, first.timestamp);
    }

    assert!(ctx
        .get_or_populate_cached_object(&PyRep::String("unknown.bulk.key".into()), true)
        .is_none());
}

#[test]
fn notification_recipient_resolution_and_packet_builders_cover_all_targets_and_offline_cases() {
    let ctx = test_ctx();
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    let char3 = 90_000_099_i64;
    let corp_id = 98_000_055_i64;
    let ally_id = 99_000_055_i64;
    let sys_id = 30_000_142_u32;
    let structure_id = 1_055_000_777_i64;
    let fleet_id = 777_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let uid3 = 777_099_u64;
        if let Some(mut acc) = snap.accounts.values().next().cloned() {
            acc.user_id = uid3;
            acc.username = "offline_user".into();
            snap.accounts.insert("offline_user".into(), acc);
        }
        let mut c3 = snap.characters[&char2].clone();
        c3.character_id = char3;
        c3.user_id = uid3;
        c3.name = "Offline Pilot".into();
        c3.corporation_id = corp_id;
        c3.alliance_id = Some(ally_id);
        c3.solar_system_id = sys_id;
        c3.station_id = None;
        c3.structure_id = Some(structure_id);
        snap.characters.insert(char3, c3);
        if let Some(c1) = snap.characters.get_mut(&char1) {
            c1.corporation_id = corp_id;
            c1.alliance_id = Some(ally_id);
            c1.solar_system_id = sys_id;
            c1.station_id = None;
            c1.structure_id = Some(structure_id);
        }
        if let Some(c2) = snap.characters.get_mut(&char2) {
            c2.corporation_id = 1_000_009;
            c2.alliance_id = None;
            c2.solar_system_id = 30_000_144;
            c2.station_id = Some(60_003_760);
            c2.structure_id = None;
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char1,
                members: vec![char1, char2, char3, 888_888_888],
                ..Default::default()
            },
        );
    });

    // All offline initially
    for target in [
        NotificationTarget::Character(char1),
        NotificationTarget::Character(888_888_888),
        NotificationTarget::Corporation(corp_id),
        NotificationTarget::Alliance(ally_id),
        NotificationTarget::Fleet(fleet_id),
        NotificationTarget::Fleet(999_999),
        NotificationTarget::SolarSystem(sys_id),
        NotificationTarget::Station(structure_id),
        NotificationTarget::All,
    ] {
        assert!(ctx.resolve_notification_recipients(&target).is_empty());
    }

    // Bring char1 and char2 online, leave char3 offline
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&char1].user_id);
    let uid2 = ctx.db.with_snapshot(|s| s.characters[&char2].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid1, 1, Some(char1), false, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(uid2, 2, Some(char2), false, ctx.now_unix());

    assert!(ctx.is_character_online(char1, uid1));
    assert!(!ctx.is_character_online(char3, 777_099));

    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Character(char1))
            .len(),
        1
    );
    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Corporation(corp_id)),
        vec![(char1, Some(uid1))]
    );
    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Alliance(ally_id)),
        vec![(char1, Some(uid1))]
    );
    // Fleet has char1 (online), char2 (online), char3 (offline), 888_888_888 (missing)
    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Fleet(fleet_id)),
        vec![(char1, Some(uid1)), (char2, Some(uid2))]
    );
    assert!(ctx
        .resolve_notification_recipients(&NotificationTarget::Fleet(999_999))
        .is_empty());
    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::SolarSystem(sys_id)),
        vec![(char1, Some(uid1))]
    );
    // Station target matches structure_id on char1
    assert_eq!(
        ctx.resolve_notification_recipients(&NotificationTarget::Station(structure_id)),
        vec![(char1, Some(uid1))]
    );
    // All includes char1 and char2 sorted by char_id
    let all = ctx.resolve_notification_recipients(&NotificationTarget::All);
    assert_eq!(all, vec![(char1, Some(uid1)), (char2, Some(uid2))]);

    let (npkt, nbytes) = ServerContext::build_macho_notification_packet(
        -5,
        None,
        "OnAccountChange",
        "charid",
        vec![PyRep::None],
    );
    assert_eq!(npkt.packet_type, MachoPacketType::Notification);
    assert!(matches!(
        npkt.destination,
        MachoAddress::Broadcast { ref narrowcast, .. } if narrowcast == &vec![0]
    ));
    assert!(!nbytes.is_empty());

    let (cpkt, cbytes) = ServerContext::build_macho_call_response_packet(
        Some("svc"),
        11,
        22,
        Some(uid1),
        PyRep::Integer(1),
    );
    assert_eq!(cpkt.packet_type, MachoPacketType::CallRsp);
    assert!(!cbytes.is_empty());

    let (epkt, ebytes) = ServerContext::build_macho_error_response_packet(
        None,
        11,
        22,
        None,
        "ErrCode",
        "ErrMsg",
    );
    assert_eq!(epkt.packet_type, MachoPacketType::ErrorResponse);
    assert!(!ebytes.is_empty());
}

#[test]
fn persistent_notifications_login_delivery_local_chat_and_emitters() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 10, Some(char_id), false, ctx.now_unix());

    // Early return when no unprocessed notifications exist
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let r1 = ctx.create_character_notification(1_000_009, char_id, 10, 1, "payload1");
    let r2 = ctx.create_character_notification(1_000_009, char_id, 11, 1, "payload2");
    let r_del = ctx.create_character_notification(1_000_009, char_id, 12, 1, "deleted");
    let r_proc = ctx.create_character_notification(1_000_009, char_id, 13, 1, "processed");
    {
        let mut recs = ctx.notification_records.write().unwrap();
        for rec in recs.iter_mut() {
            if rec.notification_id == r_del.notification_id {
                rec.deleted = true;
            }
            if rec.notification_id == r_proc.notification_id {
                rec.processed = true;
            }
        }
    }
    ctx.sync_character_notifications_to_db(char_id);

    // Drain r2, r_del, r_proc so only r1 remains queued, or drain all then test dedup
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), 4);

    // Re-queue r1 by calling deliver_unprocessed_notifications_on_login -> delivers r1 & r2 (not deleted/processed)
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    // Calling a second time while still in queue hits the dedup `continue` branch
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    let login_q = ctx.drain_notifications_for_char(char_id);
    assert_eq!(login_q.len(), 2);
    assert_eq!(login_q[0].args[0], PyRep::Integer(r1.notification_id));
    assert_eq!(login_q[1].args[0], PyRep::Integer(r2.notification_id));

    // Fill queue to MAX_PENDING_NOTIFICATIONS_PER_CHAR to exercise eviction inside deliver_unprocessed_notifications_on_login
    for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        let _ = ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![PyRep::Integer(i as i64)],
        );
    }
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    assert_eq!(
        ctx.drain_notifications_for_char(char_id).len(),
        MAX_PENDING_NOTIFICATIONS_PER_CHAR
    );

    // Quasar subscriber sync: unbound matching user_id, bound matching char_id, and other char
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    {
        let mut subs = ctx.quasar_gateway.subscribers.write().unwrap();
        subs.insert(
            1,
            GatewaySubscriber {
                session_id: 1,
                subscribed_at_unix: 1,
                application_instance_uuid: vec![1],
                user_id: Some(uid as i32),
                character_id: None,
                corporation_id: None,
                alliance_id: None,
                solar_system_id: None,
                fleet_id: None,
                bubble_instance_uuid: None,
                subscribed_notice_types: Vec::new(),
                sender: tx.clone(),
                inbox: VecDeque::new(),
            },
        );
        subs.insert(
            2,
            GatewaySubscriber {
                session_id: 2,
                subscribed_at_unix: 1,
                application_instance_uuid: vec![2],
                user_id: Some(99_999),
                character_id: Some(other_char as u32),
                corporation_id: Some(1_000_009),
                alliance_id: None,
                solar_system_id: Some(30_000_144),
                fleet_id: None,
                bubble_instance_uuid: None,
                subscribed_notice_types: Vec::new(),
                sender: tx,
                inbox: VecDeque::new(),
            },
        );
    }

    // First entry (old_sys = None -> delivers unprocessed notifications & binds sub 1 & emits OnChannelJoined)
    ctx.sync_character_local_chat(char_id, None, 30_000_142);
    let evs1 = ctx.drain_notifications_for_char(char_id);
    assert!(evs1.iter().any(|n| n.event == "OnNotificationReceived"));
    assert!(evs1.iter().any(|n| n.event == "OnChannelJoined"));
    assert_eq!(
        ctx.quasar_gateway.subscribers.read().unwrap()[&1].character_id,
        Some(char_id as u32)
    );
    assert_eq!(
        ctx.quasar_gateway.subscribers.read().unwrap()[&1].solar_system_id,
        Some(30_000_142)
    );

    // Same system (old_sys == Some(new_sys)) -> updates sub 1 without re-delivering login notifications
    ctx.sync_character_local_chat(char_id, Some(30_000_142), 30_000_142);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Jump into wormhole (delayed local) -> skips OnChannelJoined, removes from 30000142
    ctx.sync_character_local_chat(char_id, Some(30_000_142), 31_000_005);
    let wh_evs = ctx.drain_notifications_for_char(char_id);
    assert!(!wh_evs.iter().any(|n| n.event == "OnChannelJoined"));

    // Remaining emitters and flag branches
    let mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.default_attributes.contains_key(&73))
        .map(|t| t.type_id)
        .unwrap_or(587);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 31_000_005;
        }
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: 100_000_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
    });

    assert_eq!(
        ctx.notify_godma_ship_effect(NotificationTarget::Character(char_id), 700_001, 10, false, 1000, 0)
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_module_effect_started(char_id, 100_000_001, 700_001, mod_tid, Some("online"), Some(100_000_002))
            .len(),
        1
    );
    assert_eq!(ctx.notify_module_online_state(char_id, 700_001, false).len(), 1);
    assert_eq!(ctx.notify_module_online_state(char_id, 700_001, true).len(), 1);
    assert_eq!(
        ctx.notify_target_events(char_id, &[("add", 10, "ok"), ("lost", 11, "range")])
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_system_criminal_flag_updates(31_000_005, &[char_id], &[other_char], &[char_id])
            .len(),
        1
    );
    assert_eq!(
        ctx.notify_fitted_item_trashed(char_id, 700_001, mod_tid as i64, 1, 100_000_001, 27)
            .len(),
        1
    );
}
