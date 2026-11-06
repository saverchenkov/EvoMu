//! Deep branch-coverage and mutation-killing unit tests for `router::cache_notifications`:
//! IEEE CRC32 bit-level invariants, `cache_object_key` variants, `CachedObjectRecord` wire
//! encodings (`shared`/`compressed`/`sessionInfo` branches), monotonic filetime advancement across
//! updates and invalidations, `get_or_populate_cached_object` alias/sorting/field branches,
//! notification target scoping (including Upwell `structure_id` docking), queue cap eviction,
//! login deduplication, local chat & Quasar subscriber synchronization, and module/combat emitters.

#![forbid(unsafe_code)]

use super::cache_notifications::*;
use super::ServerContext;
use crate::quasar_gateway::GatewaySubscriber;
use eve_db::{
    AccountRecord, FleetRecord, InventoryFlag, InventoryItemRecord, ServerClock,
    SystemDynamicState, VirtualServerClock, WorldRepository,
};
use eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
use eve_proto::{marshal, PyRep};
use eve_sde::{MarketGroupInfo, SdeRegistry};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(Arc::clone(sde), Arc::new(WorldRepository::with_demo_seed()))
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn as_tuple(rep: &PyRep) -> &[PyRep] {
    let PyRep::Tuple(items) = rep else { panic!("expected Tuple, got {rep:?}") };
    items
}

fn as_list(rep: &PyRep) -> &[PyRep] {
    let PyRep::List(items) = rep else { panic!("expected List, got {rep:?}") };
    items
}

fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
    let PyRep::Dict(items) = rep else { panic!("expected Dict, got {rep:?}") };
    items
}

fn extract_rowset_rows(rep: &PyRep) -> Option<Vec<Vec<PyRep>>> {
    let PyRep::Rowset { rows, .. } = rep else { return None };
    Some(rows.clone())
}

#[test]
fn crc32_cache_keys_and_cached_record_wire_encodings() {
    assert_eq!(crc32_ieee(b""), 0);
    assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926_u32 as i32);
    assert_eq!(crc32_ieee(b"\x00"), 0xD202_EF8D_u32 as i32);
    assert_eq!(crc32_ieee(b"\x01"), 0xA505_DF1B_u32 as i32);
    assert_eq!(crc32_ieee(b"\xff"), 0xFF00_0000_u32 as i32);

    assert_eq!(cache_object_key(&PyRep::String("s.key".into())), "s.key");
    assert_eq!(cache_object_key(&PyRep::Token("tok.key".into())), "tok.key");
    assert_eq!(cache_object_key(&PyRep::Tuple(Vec::new())), "()");
    assert_eq!(cache_object_key(&PyRep::Integer(0)), "0");
    assert_eq!(cache_object_key(&PyRep::Integer(-42)), "-42");
    assert_eq!(
        cache_object_key(&PyRep::Tuple(vec![
            PyRep::Token("Method Call".into()),
            PyRep::Integer(-7),
            PyRep::Tuple(vec![PyRep::Bool(true)]),
        ])),
        "(Method Call,-7,({\"$py\":\"Bool\",\"val\":true}))"
    );
    assert_eq!(cache_object_key(&PyRep::None), "{\"$py\":\"None\"}");

    let oid = PyRep::String("config.Units".into());
    let rec_unshared = CachedObjectRecord {
        object_id: oid.clone(),
        timestamp: 13_300_000_000_000_0000,
        version: 123_456,
        node_id: 888_444,
        shared: false,
        compressed: false,
        marshaled_bytes: vec![1, 2, 3],
        payload: PyRep::Integer(77),
    };
    let PyRep::Object { type_name: co_type, args: co_args } = rec_unshared.to_cached_object_pyrep() else {
        panic!("expected objectCaching.CachedObject");
    };
    assert_eq!(co_type, "objectCaching.CachedObject");
    let co_t = as_tuple(&co_args);
    assert_eq!(co_t.len(), 7);
    assert_eq!(co_t[0], PyRep::Tuple(vec![PyRep::Integer(13_300_000_000_000_0000), PyRep::Integer(123_456)]));
    assert_eq!(co_t[1], PyRep::None);
    assert_eq!(co_t[2], PyRep::Integer(888_444));
    assert_eq!(co_t[3], PyRep::Integer(0));
    assert_eq!(co_t[4], PyRep::Buffer(vec![1, 2, 3]));
    assert_eq!(co_t[5], PyRep::Integer(0));
    assert_eq!(co_t[6], oid);

    let rec_sc = CachedObjectRecord { shared: true, compressed: true, ..rec_unshared.clone() };
    let PyRep::Object { args: sc_args, .. } = rec_sc.to_cached_object_pyrep() else { panic!() };
    let sc_t = as_tuple(&sc_args);
    assert_eq!((&sc_t[3], &sc_t[5]), (&PyRep::Integer(1), &PyRep::Integer(1)));

    let hint = rec_unshared.to_cache_hint_pyrep();
    let PyRep::Object { type_name: hint_type, args: hint_args } = &hint else { panic!() };
    assert_eq!(hint_type, "util.CachedObject");
    let ht = as_tuple(hint_args);
    assert_eq!(ht[0], oid);
    assert_eq!(ht[1], PyRep::Integer(888_444));
    assert_eq!(ht[2], PyRep::Tuple(vec![PyRep::Integer(13_300_000_000_000_0000), PyRep::Integer(123_456)]));

    let PyRep::Object { type_name: res_type, args: res_no_si } =
        rec_unshared.to_cached_method_call_result_pyrep("run", None)
    else { panic!() };
    assert_eq!(res_type, "objectCaching.CachedMethodCallResult");
    let rt0 = as_tuple(&res_no_si);
    assert_eq!(as_dict(&rt0[0]).len(), 1);
    assert_eq!(
        rt0[0].dict_get("versionCheck"),
        Some(&PyRep::Tuple(vec![PyRep::String("run".into()), PyRep::None, PyRep::None]))
    );
    assert_eq!(rt0[1], PyRep::SubStream(Box::new(PyRep::Integer(77))));
    assert_eq!(rt0[2], PyRep::List(vec![PyRep::Integer(13_300_000_000_000_0000), PyRep::Integer(123_456)]));

    let PyRep::Object { args: res_with_si, .. } =
        rec_unshared.to_cached_method_call_result_pyrep("now", Some("userid"))
    else { panic!() };
    let rt1 = as_tuple(&res_with_si);
    assert_eq!(as_dict(&rt1[0]).len(), 2);
    assert_eq!(rt1[0].dict_get("sessionInfo"), Some(&PyRep::String("userid".into())));

    let PyRep::Object { type_name: hrt, args: hra } = rec_unshared.to_cached_method_call_hint_pyrep("run") else {
        panic!();
    };
    assert_eq!(hrt, "objectCaching.CachedMethodCallResult");
    let hrt_t = as_tuple(&hra);
    assert_eq!(hrt_t[0].dict_get("versionCheck"), Some(&PyRep::String("run".into())));
    assert_eq!(hrt_t[1], hint);
    assert_eq!(hrt_t[2], PyRep::None);
}

#[test]
fn put_cached_object_monotonic_timestamps_compression_and_invalidation() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);
    let t0 = ctx.now_blueos_filetime_ticks();

    let oid = ServerContext::method_call_object_id("stationSvc", "GetAllianceSystems");
    let r1 = ctx.put_cached_object(oid.clone(), PyRep::Integer(10), false);
    assert_eq!(r1.timestamp, t0);
    assert_eq!(r1.node_id, 888_444);
    assert!(!r1.shared);
    assert!(!r1.compressed);
    assert_eq!(r1.version, crc32_ieee(&r1.marshaled_bytes));

    let r2 = ctx.put_cached_object(oid.clone(), PyRep::Integer(20), true);
    assert_eq!(r2.timestamp, t0 + 1);
    assert!(r2.shared);
    assert_ne!(r2.version, r1.version);

    assert!(ctx.invalidate_cached_object(&oid));
    assert!(!ctx.invalidate_cached_object(&oid));
    let r3 = ctx.put_cached_object(oid.clone(), PyRep::Integer(30), true);
    assert_eq!(r3.timestamp, t0 + 2);

    clock.advance_secs(10);
    let t1 = ctx.now_blueos_filetime_ticks();
    assert!(t1 > t0 + 2);
    let r4 = ctx.put_cached_object(oid, PyRep::Integer(40), true);
    assert_eq!(r4.timestamp, t1);

    let comp_rec = ctx.put_cached_object(PyRep::String("big.cache".into()), PyRep::String("A".repeat(4096)), true);
    assert_ne!(comp_rec.marshaled_bytes.first().copied(), Some(eve_proto::ZLIB_HEADER_BYTE));
    assert!(!comp_rec.compressed);
    let PyRep::Object { args, .. } = comp_rec.to_cached_object_pyrep() else { panic!() };
    assert_eq!(as_tuple(&args)[5], PyRep::Integer(0));

    let wrapped = ctx.cache_method_call_result(
        "stationSvc",
        "GetAllianceSystems",
        PyRep::List(vec![]),
        "run",
        Some("solarsystemid2"),
    );
    assert!(matches!(wrapped, PyRep::Object { .. }));
    assert!(ctx.invalidate_cached_method("stationSvc", "GetAllianceSystems"));
    assert!(!ctx.invalidate_cached_method("stationSvc", "GetAllianceSystems"));
}

#[test]
fn get_or_populate_cached_object_aliases_sorting_null_fields_and_hit_shortcircuit() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.market_groups.insert(
        900_002,
        MarketGroupInfo {
            market_group_id: 900_002,
            parent_group_id: Some(900_001),
            name: "Child Group".into(),
            has_types: true,
            icon_id: Some(42),
        },
    );
    sde.market_groups.insert(
        900_001,
        MarketGroupInfo {
            market_group_id: 900_001,
            parent_group_id: None,
            name: "Root Group".into(),
            has_types: false,
            icon_id: None,
        },
    );

    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            30_000_144,
            SystemDynamicState { solar_system_id: 30_000_144, sov_alliance_id: Some(99_000_002), ..Default::default() },
        );
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState { solar_system_id: 30_000_142, sov_alliance_id: Some(99_000_001), ..Default::default() },
        );
        snap.system_states.insert(
            30_000_145,
            SystemDynamicState { solar_system_id: 30_000_145, sov_alliance_id: None, ..Default::default() },
        );
    });

    let u1 = ctx.get_or_populate_cached_object(&PyRep::String("config.Units".into()), true).unwrap();
    let u2 = ctx.get_or_populate_cached_object(&PyRep::String("config.BulkData.units".into()), false).unwrap();
    assert!(!u2.shared);
    assert_eq!(u1.version, u2.version);

    let mg_rec = ctx
        .get_or_populate_cached_object(&ServerContext::method_call_object_id("marketProxy", "GetMarketGroups"), true)
        .unwrap();
    let mg_rows = extract_rowset_rows(&mg_rec.payload).unwrap();
    let pos_root = mg_rows.iter().position(|r| r[0] == PyRep::Integer(900_001)).unwrap();
    let pos_child = mg_rows.iter().position(|r| r[0] == PyRep::Integer(900_002)).unwrap();
    assert!(pos_root < pos_child);
    assert_eq!((&mg_rows[pos_root][1], &mg_rows[pos_root][4], &mg_rows[pos_root][5]), (&PyRep::None, &PyRep::None, &PyRep::Integer(0)));
    assert_eq!((&mg_rows[pos_child][1], &mg_rows[pos_child][4], &mg_rows[pos_child][5]), (&PyRep::Integer(900_001), &PyRep::Integer(42), &PyRep::Integer(1)));

    let ally_rec = ctx
        .get_or_populate_cached_object(&ServerContext::method_call_object_id("stationSvc", "GetAllianceSystems"), true)
        .unwrap();
    assert_eq!(
        extract_rowset_rows(&ally_rec.payload).unwrap(),
        vec![
            vec![PyRep::Integer(30_000_142), PyRep::Integer(99_000_001)],
            vec![PyRep::Integer(30_000_144), PyRep::Integer(99_000_002)],
        ]
    );

    let owner_rows = extract_rowset_rows(
        &ctx.get_or_populate_cached_object(&PyRep::String("config.StaticOwners".into()), true).unwrap().payload,
    )
    .unwrap();
    assert!(owner_rows.iter().any(|r| r.get(2) == Some(&PyRep::Integer(30))));
    assert!(owner_rows.iter().any(|r| r.get(2) == Some(&PyRep::Integer(2))));

    for (k_cfg, k_cc) in [
        ("config.Races", "charCreationInfo.races"),
        ("config.Bloodlines", "charCreationInfo.bloodlines"),
        ("config.Ancestries", "charCreationInfo.ancestries"),
        ("config.Schools", "charCreationInfo.schools"),
    ] {
        let a = ctx.get_or_populate_cached_object(&PyRep::String(k_cfg.into()), true).unwrap();
        let b = ctx.get_or_populate_cached_object(&PyRep::String(k_cc.into()), true).unwrap();
        assert_eq!(a.version, b.version);
    }

    let custom = ctx.put_cached_object(PyRep::String("config.Units".into()), PyRep::Integer(555), false);
    let hit = ctx.get_or_populate_cached_object(&PyRep::String("config.Units".into()), true).unwrap();
    assert_eq!((hit.version, hit.payload, hit.shared), (custom.version, PyRep::Integer(555), false));
    assert!(ctx.get_or_populate_cached_object(&PyRep::String("config.unknown_cache_key".into()), true).is_none());
}

#[test]
fn recipient_resolution_structure_docking_and_packet_wire_builders() {
    let ctx = test_ctx();
    let (c1, c2, c3, u3) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64, 3_u64);
    let (structure_id, ally_id, fleet_id) = (1_000_000_888_001_i64, 99_000_123_i64, 800_055_i64);
    let (u1, u2) = ctx.db.with_snapshot(|s| (s.characters[&c1].user_id, s.characters[&c2].user_id));

    assert!(!ctx.is_character_online(c1, u1));
    let _ = ctx.db.acquire_online_lock(u1, 10, None, false, ctx.now_unix());
    assert!(!ctx.is_character_online(c1, u1));
    let _ = ctx.db.acquire_online_lock(u1, 10, Some(c2), false, ctx.now_unix());
    assert!(!ctx.is_character_online(c1, u1));
    let _ = ctx.db.acquire_online_lock(u1, 10, Some(c1), false, ctx.now_unix());
    assert!(ctx.is_character_online(c1, u1));

    let _ = ctx.db.acquire_online_lock(u2, 20, Some(c2), false, ctx.now_unix());
    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.characters[&c1].corporation_id;
        if let Some(ch1) = snap.characters.get_mut(&c1) {
            ch1.alliance_id = Some(ally_id);
            ch1.station_id = Some(60_003_760);
            ch1.structure_id = None;
        }
        if let Some(ch2) = snap.characters.get_mut(&c2) {
            ch2.corporation_id = corp;
            ch2.alliance_id = Some(ally_id);
            ch2.station_id = None;
            ch2.structure_id = Some(structure_id);
        }
        snap.accounts.insert(
            "offline_c3".into(),
            AccountRecord {
                user_id: u3,
                username: "offline_c3".into(),
                password_token: "hash".into(),
                role: ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut c3_rec = snap.characters[&c1].clone();
        c3_rec.character_id = c3;
        c3_rec.user_id = u3;
        c3_rec.name = "Offline Pilot".into();
        c3_rec.corporation_id = corp;
        c3_rec.alliance_id = Some(ally_id);
        c3_rec.solar_system_id = 30_000_142;
        c3_rec.station_id = Some(60_003_760);
        snap.characters.insert(c3, c3_rec);
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: c1, members: vec![c2, c1, c2, 999_999_999, c3], ..Default::default() });
        corp
    });
    assert!(!ctx.is_character_online(c3, u3));

    assert_eq!(ctx.resolve_notification_recipients(&NotificationTarget::Station(structure_id)), vec![(c2, Some(u2))]);
    assert_eq!(ctx.resolve_notification_recipients(&NotificationTarget::Station(60_003_760)), vec![(c1, Some(u1))]);
    assert_eq!(ctx.resolve_notification_recipients(&NotificationTarget::Fleet(fleet_id)), vec![(c1, Some(u1)), (c2, Some(u2))]);
    assert!(ctx.resolve_notification_recipients(&NotificationTarget::Fleet(999_999)).is_empty());
    assert_eq!(ctx.resolve_notification_recipients(&NotificationTarget::Alliance(ally_id)), vec![(c1, Some(u1)), (c2, Some(u2))]);
    assert!(ctx.resolve_notification_recipients(&NotificationTarget::Alliance(99_999_999)).is_empty());
    assert_eq!(ctx.resolve_notification_recipients(&NotificationTarget::Corporation(corp_id)), vec![(c1, Some(u1)), (c2, Some(u2))]);
    assert!(ctx.resolve_notification_recipients(&NotificationTarget::Character(c3)).is_empty());

    let (neg_pkt, neg_bytes) = ServerContext::build_macho_notification_packet(-15, None, "OnAccountChange", "charid", vec![PyRep::Integer(1)]);
    assert_eq!(neg_pkt.destination, MachoAddress::Broadcast { broadcast_id: "OnAccountChange".into(), id_type: "charid".into(), narrowcast: vec![0] });
    assert_eq!(MachoPacket::from_pyrep(&marshal::decode(&neg_bytes).unwrap()).unwrap().packet_type, MachoPacketType::Notification);

    let (rsp_some, rsp_bytes) = ServerContext::build_macho_call_response_packet(Some("charMgr"), 77, 88, Some(u1), PyRep::String("ok".into()));
    assert_eq!(rsp_some.source, MachoAddress::Node { node_id: 888_444, service: Some("charMgr".into()), call_id: 88 });
    assert_eq!(MachoPacket::from_pyrep(&marshal::decode(&rsp_bytes).unwrap()).unwrap().packet_type, MachoPacketType::CallRsp);

    let (err_pkt, err_bytes) = ServerContext::build_macho_error_response_packet(Some("stationSvc"), 77, 89, Some(u1), "CustomInfo", "Denied");
    assert_eq!(err_pkt.packet_type, MachoPacketType::ErrorResponse);
    assert_eq!(MachoPacket::from_pyrep(&marshal::decode(&err_bytes).unwrap()).unwrap().packet_type, MachoPacketType::ErrorResponse);
}

#[test]
fn notification_queue_eviction_login_dedup_local_chat_and_emitters() {
    let ctx = test_ctx();
    let (char_id, other_char) = (90_000_001_i64, 90_000_002_i64);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 301, Some(char_id), false, ctx.now_unix());
    let mut rx = ctx.notify_tx.subscribe();

    for i in 0..(MAX_PENDING_NOTIFICATIONS_PER_CHAR + 3) {
        let _ = ctx.notify(NotificationTarget::Character(char_id), "OnAccountChange", "charid", vec![PyRep::Integer(i as i64)]);
    }
    let drained = ctx.drain_notifications_for_char(char_id);
    assert_eq!(drained.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!(drained.first().unwrap().args, vec![PyRep::Integer(3)]);
    assert_eq!(drained.last().unwrap().args, vec![PyRep::Integer((MAX_PENDING_NOTIFICATIONS_PER_CHAR + 2) as i64)]);
    let (tx_cid, tx_notif) = rx.try_recv().unwrap();
    assert_eq!((tx_cid, tx_notif.event.as_str()), (char_id, "OnAccountChange"));

    let r1 = ctx.create_character_notification(1_000_009, char_id, 10, 1, "first");
    let r2 = ctx.create_character_notification(1_000_009, char_id, 11, 1, "second");
    assert!(r2.notification_id > r1.notification_id);
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 2);

    if let Some(first) = ctx.notification_records.write().unwrap().iter_mut().find(|r| r.notification_id == r1.notification_id) {
        first.processed = true;
    }
    ctx.sync_character_notifications_to_db(char_id);
    let expected_note_id = -(7_900_000_000_000 + r1.notification_id);
    ctx.db.with_snapshot(|snap| {
        let note = snap.owner_notes.get(&expected_note_id).unwrap();
        assert_eq!((note.character_id, note.label.as_str()), (-79, format!("notif:{}", r1.notification_id).as_str()));
        assert!(serde_json::from_str::<NotificationRecord>(&note.note).unwrap().processed);
    });

    let _ = ctx.notify(NotificationTarget::Character(char_id), "OnAccountChange", "charid", vec![PyRep::Integer(r2.notification_id)]);
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    let login_delivered = ctx.drain_notifications_for_char(char_id);
    assert_eq!(login_delivered.len(), 2);
    assert_eq!(login_delivered[1].event, "OnNotificationReceived");

    for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        let _ = ctx.notify(NotificationTarget::Character(char_id), "OnAccountChange", "charid", vec![PyRep::Integer(i as i64)]);
    }
    ctx.deliver_unprocessed_notifications_on_login(char_id);
    let capped = ctx.drain_notifications_for_char(char_id);
    assert_eq!(capped.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!(capped.last().unwrap().event, "OnNotificationReceived");

    let (q_tx, _q_rx) = tokio::sync::mpsc::channel(16);
    ctx.quasar_gateway.subscribers.write().unwrap().insert(
        1,
        GatewaySubscriber {
            session_id: 1, subscribed_at_unix: 100, application_instance_uuid: vec![1],
            user_id: Some(uid as i32), character_id: None, corporation_id: None, alliance_id: None,
            solar_system_id: None, fleet_id: None, bubble_instance_uuid: None,
            subscribed_notice_types: Vec::new(), sender: q_tx.clone(), inbox: VecDeque::new(),
        },
    );
    ctx.quasar_gateway.subscribers.write().unwrap().insert(
        2,
        GatewaySubscriber {
            session_id: 2, subscribed_at_unix: 100, application_instance_uuid: vec![2],
            user_id: Some(9999), character_id: Some(other_char as u32), corporation_id: Some(1_000_009),
            alliance_id: None, solar_system_id: Some(30_000_144), fleet_id: None, bubble_instance_uuid: None,
            subscribed_notice_types: Vec::new(), sender: q_tx, inbox: VecDeque::new(),
        },
    );

    ctx.sync_character_local_chat(char_id, None, 30_000_142);
    let init_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(init_notifs.iter().any(|n| n.event == "OnNotificationReceived"));
    assert!(init_notifs.iter().any(|n| n.event == "OnChannelJoined"));
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap()[&1].solar_system_id, Some(30_000_142));
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap()[&2].solar_system_id, Some(30_000_144));

    ctx.sync_character_local_chat(char_id, Some(30_000_142), 30_000_142);
    assert!(ctx.db.with_snapshot(|s| s.chat_channels["solarsystemid2:30000142"].members.contains(&char_id)));
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnNotificationReceived"));

    ctx.sync_character_local_chat(char_id, Some(30_000_142), 31_000_005);
    ctx.db.with_snapshot(|s| {
        assert!(!s.chat_channels["solarsystemid2:30000142"].members.contains(&char_id));
        let wh = &s.chat_channels["solarsystemid2:31000005"];
        assert!(wh.members.contains(&char_id) && wh.is_delayed_local);
    });
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnChannelJoined"));

    let module_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && ctx.effect_id_for_module(t.type_id, None).is_some())
        .map(|t| t.type_id)
        .unwrap_or(587);
    assert!(ctx.effect_id_for_module(999_999_999, None).is_none());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001, type_id: module_tid, owner_id: char_id, location_id: 100_000_001,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
            },
        );
    });

    let eff_off = ctx.notify_godma_ship_effect(NotificationTarget::Character(char_id), 700_001, 16, false, 2500, 0);
    assert_eq!((&eff_off[0].args[3], &eff_off[0].args[4]), (&PyRep::Integer(0), &PyRep::Integer(0)));

    let started = ctx.notify_module_effect_started(char_id, 100_000_001, 700_001, module_tid, None, Some(100_000_002));
    assert_eq!(as_tuple(&started[0].args[5])[3], PyRep::Integer(100_000_002));

    let offline_mod = ctx.notify_module_online_state(char_id, 700_001, false);
    let entry = as_tuple(&as_list(&offline_mod[0].args[0])[0]);
    assert_eq!((&entry[5], &entry[6]), (&PyRep::Real(0.0), &PyRep::Real(1.0)));

    let dmg = ctx.notify_damage_state_change(NotificationTarget::Character(char_id), 700_001, -0.25, 1.5, 0.5);
    let dmg_tuple = as_tuple(&dmg[0].args[1]);
    assert_eq!((&as_tuple(&dmg_tuple[0])[0], &dmg_tuple[1], &dmg_tuple[2]), (&PyRep::Real(0.0), &PyRep::Real(1.0), &PyRep::Real(0.5)));
}
