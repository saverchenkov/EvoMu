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

#[test]
fn seq_id_overflow_preserves_recent_notifications_and_dedup() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 401, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(ctx.latest_notification_seq_for_char(char_id), 0);

    // Fill queue past MAX_PENDING_NOTIFICATIONS_PER_CHAR and record midpoint seq_id.
    for i in 0..100 {
        ctx.notify_skill_trained(char_id, 500 + i, 3300, 1);
    }
    let pre_seq = ctx.latest_notification_seq_for_char(char_id);
    assert!(pre_seq > 0);

    for i in 100..160 {
        ctx.notify_skill_trained(char_id, 500 + i, 3300, 2);
    }
    let all = ctx.drain_notifications_for_char(char_id);
    assert_eq!(all.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    let post_seq_notifs: Vec<_> = all.iter().filter(|n| n.seq_id > pre_seq).collect();
    assert_eq!(post_seq_notifs.len(), 60);
    assert!(all.windows(2).all(|w| w[0].seq_id < w[1].seq_id));
}

#[test]
fn compute_item_change_dict_and_mutation_router_v24_parity() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let other_char = 90_000_002;
    let corp_id = 1_000_299_i64;
    let (u1, u2) = ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Router Test Corp".into();
        corp.ceo_id = char_id;
        corp.members.insert(char_id);
        corp.members.insert(other_char);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&other_char) {
            ch.corporation_id = corp_id;
        }
        (snap.characters[&char_id].user_id, snap.characters[&other_char].user_id)
    });
    let _ = ctx.db.acquire_online_lock(u1, 402, Some(char_id), false, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock(u2, 403, Some(other_char), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_char);

    let stack_old = InventoryItemRecord {
        item_id: 880_001,
        type_id: 34,
        owner_id: char_id,
        location_id: 60_003_760,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        singleton: false,
        ..Default::default()
    };
    let stack_new = InventoryItemRecord {
        owner_id: corp_id,
        location_id: 60_003_761,
        flag: InventoryFlag::CorpSAG1,
        quantity: 60,
        ..stack_old.clone()
    };
    let changes = ServerContext::compute_item_change_dict(&stack_old, &stack_new);
    assert!(changes.iter().all(|(k, _)| *k != 5), "legacy ixQuantity (5) must never be emitted");
    assert!(changes.contains(&(2, PyRep::Integer(char_id))));
    assert!(changes.contains(&(3, PyRep::Integer(60_003_760))));
    assert!(changes.contains(&(4, PyRep::Integer(InventoryFlag::Hangar as i64))));
    assert!(changes.contains(&(9, PyRep::Integer(100))));

    // Moving personal -> corp hangar notifies both source character and destination corp members.
    ctx.notify_item_mutation(char_id, Some(&stack_old), Some(&stack_new));
    let n_char = ctx.drain_notifications_for_char(char_id);
    let n_other = ctx.drain_notifications_for_char(other_char);
    assert!(n_other.iter().any(|n| n.event == "OnItemChange" && n.id_type == "corpid"));
    assert!(n_char.iter().filter(|n| n.event == "OnItemChange").count() >= 1);

    // Deletion from corp hangar sends tombstone (locationID=6, flag=0, qty=0) to corp scope.
    ctx.notify_item_mutation(char_id, Some(&stack_new), None);
    let del_other = ctx.drain_notifications_for_char(other_char);
    let del_notif = del_other
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("corp deletion OnItemChange");
    assert_eq!(del_notif.id_type, "corpid");
    assert_eq!(del_notif.args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));
    assert_eq!(del_notif.args[0].dbrow_get("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(del_notif.args[0].dbrow_get("quantity"), Some(&PyRep::Integer(0)));

    // Singleton assembly/repackaging transitions record previous ixSingleton (10) state.
    let single_new = InventoryItemRecord {
        singleton: true,
        quantity: 1,
        ..InventoryItemRecord {
            quantity: 1,
            singleton: false,
            ..stack_old.clone()
        }
    };
    let single_old = InventoryItemRecord { quantity: 1, singleton: false, ..stack_old };
    let singleton_changes = ServerContext::compute_item_change_dict(&single_old, &single_new);
    assert!(singleton_changes.contains(&(10, PyRep::Integer(0))));
    assert!(singleton_changes.iter().all(|(k, _)| *k != 5));
    let repackage_changes = ServerContext::compute_item_change_dict(&single_new, &single_old);
    assert!(repackage_changes.contains(&(10, PyRep::Integer(1))));

    // Wallet helper covers personal cash and corp divisions cash / cashN.
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_char);
    ctx.notify_wallet_balance(false, char_id, 1, eve_db::IskCents(123_456));
    ctx.notify_wallet_balance(true, corp_id, 3, eve_db::IskCents(999_000));
    let wn = ctx.drain_notifications_for_char(other_char);
    assert!(wn.iter().any(|n| {
        n.event == "OnAccountChange"
            && n.id_type == "corpid"
            && n.args[0] == PyRep::String("cash3".into())
    }));

    // Master corp wallet (`div_id == 0` or `1`) uses `"cash"`, while divisions `2..=7` use `"cash2".."cash7"`.
    ctx.notify_wallet_balance(true, corp_id, 1, eve_db::IskCents(500_000));
    ctx.notify_wallet_balance(true, corp_id, 0, eve_db::IskCents(600_000));
    let wn_master = ctx.drain_notifications_for_char(other_char);
    assert_eq!(wn_master.len(), 2);
    assert!(wn_master.iter().all(|n| n.args[0] == PyRep::String("cash".into())));

    // `resolve_item_notification_target` routes items owned by another character (`is_other_char`)
    // to `NotificationTarget::Character(other_char)` when not in a corp hangar flag.
    let other_personal_item = InventoryItemRecord {
        item_id: 880_099,
        type_id: 34,
        owner_id: other_char,
        location_id: 60_003_760,
        flag: InventoryFlag::Hangar,
        quantity: 10,
        singleton: false,
        ..Default::default()
    };
    assert_eq!(
        ctx.resolve_item_notification_target(char_id, &other_personal_item),
        (NotificationTarget::Character(other_char), "charid")
    );

    // `notify_item_mutation`: `(None, Some)` creation, `(Some, Some)` identical no-op, and `(None, None)` no-op.
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_char);
    ctx.notify_item_mutation(char_id, None, None);
    ctx.notify_item_mutation(char_id, Some(&other_personal_item), Some(&other_personal_item));
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    assert!(ctx.drain_notifications_for_char(other_char).is_empty());

    ctx.notify_item_mutation(char_id, None, Some(&other_personal_item));
    let created_other = ctx.drain_notifications_for_char(other_char);
    assert_eq!(created_other.len(), 1);
    assert_eq!(created_other[0].event, "OnItemChange");
    assert_eq!(created_other[0].args[1], PyRep::Dict(Vec::new()));
}

#[test]
fn mutants_cov_notification_seq_flag_module_fx_tombstone_and_target_edges() {
    use crate::router::cache_notifications::{
        is_corp_hangar_flag, next_notification_seq_id, push_pending_notification_capped,
        PendingMachoNotification,
    };

    // 1. Monotonic seq IDs > 1
    let s1 = next_notification_seq_id();
    let s2 = next_notification_seq_id();
    assert_eq!(s2, s1 + 1);
    assert!(s2 > 1);

    // 2. is_corp_hangar_flag & ServerContext::is_corp_hangar_flag for CorpDeliveries/CorpSAG1..7 vs Hangar/Cargo
    for f in [
        InventoryFlag::CorpDeliveries,
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG7,
    ] {
        assert!(is_corp_hangar_flag(f));
        assert!(ServerContext::is_corp_hangar_flag(f));
    }
    for f in [InventoryFlag::None, InventoryFlag::Hangar, InventoryFlag::Cargo] {
        assert!(!is_corp_hangar_flag(f));
        assert!(!ServerContext::is_corp_hangar_flag(f));
    }

    // 3. push_pending_notification_capped evicts only at >= MAX_PENDING_NOTIFICATIONS_PER_CHAR
    let mut queue = Vec::new();
    let (dummy_pkt, dummy_raw) =
        ServerContext::build_macho_notification_packet(1, Some(1), "Dummy", "charid", vec![]);
    for i in 0..MAX_PENDING_NOTIFICATIONS_PER_CHAR {
        push_pending_notification_capped(
            &mut queue,
            PendingMachoNotification::new(
                1,
                format!("Ev{i}"),
                "charid",
                vec![],
                dummy_pkt.clone(),
                dummy_raw.clone(),
                i as i64,
            ),
        );
    }
    assert_eq!(queue.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!(queue[0].event, "Ev0");
    push_pending_notification_capped(
        &mut queue,
        PendingMachoNotification::new(
            1,
            "EvOverflow",
            "charid",
            vec![],
            dummy_pkt,
            dummy_raw,
            999,
        ),
    );
    assert_eq!(queue.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!(queue[0].event, "Ev1");
    assert_eq!(queue.last().unwrap().event, "EvOverflow");

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 1_000_399_i64;
    let zero_uid = 888_000_u64;
    let (u1, u2) = ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Target Corp".into();
        corp.ceo_id = char_id;
        corp.members.insert(char_id);
        corp.members.insert(other_char);
        snap.corporations.insert(corp_id, corp);

        // Insert a character at id 0 and backing account to exercise `owner_id > 0` boundary (>= mutant) cleanly.
        if let Some(mut zero_acct) = snap.accounts.values().next().cloned() {
            zero_acct.user_id = zero_uid;
            zero_acct.username = "zero_char_uid".into();
            snap.accounts.insert("zero_char_uid".into(), zero_acct);
        }
        let mut zero_ch = snap.characters[&char_id].clone();
        zero_ch.character_id = 0;
        zero_ch.user_id = zero_uid;
        snap.characters.insert(0, zero_ch);

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&other_char) {
            ch.corporation_id = corp_id;
        }

        (snap.characters[&char_id].user_id, snap.characters[&other_char].user_id)
    });
    let _ = ctx.db.acquire_online_lock(u1, 411, Some(char_id), false, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock(u2, 412, Some(other_char), false, ctx.now_unix());
    ctx.db
        .acquire_online_lock(zero_uid, 413, Some(0), false, ctx.now_unix())
        .expect("zero_uid online lock");
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_char);
    let _ = ctx.drain_notifications_for_char(0);

    // 4. `notify` OnModuleAttributeChanges cid == 0 preserves None in Tuple[1] while cid > 0 populates cid
    let attr_change_arg = || {
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()),
            PyRep::None,
            PyRep::Integer(700_001),
            PyRep::Integer(9),
            PyRep::Integer(0),
            PyRep::Real(100.0),
            PyRep::Real(90.0),
        ])])]
    };
    let res_zero = ctx.notify(
        NotificationTarget::Character(0),
        "OnModuleAttributeChanges",
        "charid",
        attr_change_arg(),
    );
    assert_eq!(res_zero.len(), 1);
    if let PyRep::List(items) = &res_zero[0].args[0] {
        if let PyRep::Tuple(t) = &items[0] {
            assert_eq!(t[1], PyRep::None);
        } else {
            panic!("expected tuple");
        }
    }
    let res_pos = ctx.notify(
        NotificationTarget::Character(char_id),
        "OnModuleAttributeChanges",
        "charid",
        attr_change_arg(),
    );
    assert_eq!(res_pos.len(), 1);
    if let PyRep::List(items) = &res_pos[0].args[0] {
        if let PyRep::Tuple(t) = &items[0] {
            assert_eq!(t[1], PyRep::Integer(char_id));
            assert_ne!(t[4], PyRep::Integer(0));
        } else {
            panic!("expected tuple");
        }
    }

    // 5. `notify_module_effect_started` d73 == 0 fallback to attr 51 & d73 > 0 precedence
    let ship_id = 100_880_001_i64;
    let mod_d73_zero = 770_001_i64;
    let mod_d73_pos = 770_002_i64;
    let mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.effect_ids.contains(&16))
        .map(|t| t.type_id)
        .unwrap_or(2048);
    {
        let mut fit = eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
        let mut dyn_zero = std::collections::HashMap::new();
        dyn_zero.insert(73_u16, 0.0);
        dyn_zero.insert(51_u16, 4_321.0);
        fit.modules.insert(
            mod_d73_zero,
            eve_dogma::FittedModule {
                item_id: mod_d73_zero,
                type_id: mod_tid,
                flag_id: 27,
                state: eve_dogma::ModuleState::Active,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: dyn_zero,
            },
        );
        let mut dyn_pos = std::collections::HashMap::new();
        dyn_pos.insert(73_u16, 7_654.0);
        dyn_pos.insert(51_u16, 4_321.0);
        fit.modules.insert(
            mod_d73_pos,
            eve_dogma::FittedModule {
                item_id: mod_d73_pos,
                type_id: mod_tid,
                flag_id: 28,
                state: eve_dogma::ModuleState::Active,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: dyn_pos,
            },
        );
        fit.recalculate_stats(&ctx.sde);
        ctx.ship_fittings.write().unwrap().insert(ship_id, fit);
    }
    let _ = ctx.drain_notifications_for_char(char_id);
    let n_zero = ctx.notify_module_effect_started(char_id, ship_id, mod_d73_zero, mod_tid, None, None);
    assert_eq!(n_zero[0].args[7], PyRep::Real(4_321.0));
    let n_pos = ctx.notify_module_effect_started(char_id, ship_id, mod_d73_pos, mod_tid, None, None);
    assert_eq!(n_pos[0].args[7], PyRep::Real(7_654.0));
    let n_online = ctx.notify_module_effect_started(char_id, ship_id, mod_d73_zero, mod_tid, Some("online"), None);
    assert_eq!(n_online[0].args[8], PyRep::Integer(0));

    // 6. Direct `notify_item_record_change` + `build_item_tombstone` + `resolve_item_notification_target` matrix
    let personal_item = InventoryItemRecord {
        item_id: 881_001,
        type_id: 34,
        owner_id: char_id,
        location_id: 60_003_760,
        flag: InventoryFlag::Hangar,
        quantity: 42,
        singleton: false,
        ..Default::default()
    };
    let direct_change = ctx.notify_item_record_change(char_id, &personal_item, vec![(9, PyRep::Integer(50))]);
    assert_eq!(direct_change.len(), 1);
    assert_eq!(direct_change[0].event, "OnItemChange");

    let (tomb, tomb_changes) = ServerContext::build_item_tombstone(&personal_item);
    assert_eq!(tomb.item_id, 881_001);
    assert_eq!(tomb.type_id, 34);
    assert_eq!(tomb.owner_id, char_id);
    assert_eq!(tomb.location_id, 6);
    assert_eq!(tomb.flag, InventoryFlag::None);
    assert_eq!(tomb.quantity, 0);
    assert!(!tomb.singleton);
    assert_eq!(
        tomb_changes,
        vec![
            (3, PyRep::Integer(60_003_760)),
            (4, PyRep::Integer(InventoryFlag::Hangar as i64)),
            (9, PyRep::Integer(42)),
        ]
    );

    // Corp hangar flag owned by a non-corp character must NOT route to Corporation
    let char_sag = InventoryItemRecord {
        flag: InventoryFlag::CorpSAG1,
        owner_id: char_id,
        ..personal_item.clone()
    };
    assert_eq!(
        ctx.resolve_item_notification_target(char_id, &char_sag),
        (NotificationTarget::Character(char_id), "charid")
    );
    // Non-corp-hangar flag owned by a corp must NOT route to Corporation
    let corp_hangar_flag = InventoryItemRecord {
        flag: InventoryFlag::Hangar,
        owner_id: corp_id,
        ..personal_item.clone()
    };
    assert_eq!(
        ctx.resolve_item_notification_target(char_id, &corp_hangar_flag),
        (NotificationTarget::Character(char_id), "charid")
    );
    // Unknown owner_id (>0, not in characters) routes to fallback_char_id
    let unknown_owner = InventoryItemRecord {
        owner_id: 999_888_777,
        ..personal_item.clone()
    };
    assert_eq!(
        ctx.resolve_item_notification_target(char_id, &unknown_owner),
        (NotificationTarget::Character(char_id), "charid")
    );
    // Zero owner_id (even if char 0 is in characters map) must NOT trigger > 0 `other_char` branch
    let zero_owner = InventoryItemRecord {
        owner_id: 0,
        ..personal_item.clone()
    };
    assert_eq!(
        ctx.resolve_item_notification_target(char_id, &zero_owner),
        (NotificationTarget::Character(char_id), "charid")
    );

    // 7. Return value assertions for `notify_wallet_balance` personal + corp div 1 vs div 2
    let w_pers = ctx.notify_wallet_balance(false, char_id, 1, eve_db::IskCents(77_000));
    assert_eq!(w_pers.len(), 1);
    assert_eq!(w_pers[0].event, "OnAccountChange");
    assert_eq!(w_pers[0].args[0], PyRep::String("cash".into()));

    let w_corp2 = ctx.notify_wallet_balance(true, corp_id, 2, eve_db::IskCents(88_000));
    assert_eq!(w_corp2.len(), 2);
    assert!(w_corp2.iter().all(|n| n.args[0] == PyRep::String("cash2".into())));
}
