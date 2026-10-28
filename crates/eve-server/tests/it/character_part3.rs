//! Object cache, config service, and notifications integration tests (`character` part 3).

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_p3_p4_p5_object_cache_config_service_and_notifications() {
    // parity: plan:P-3, plan:P-4, plan:P-5, feat:serverops.object-cache, feat:serverops.config-svc, feat:serverops.notification-transport, feat:social.notifications-list, feat:social.notifications-mark, feat:social.notifications-delete, feat:social.notifications-generate, rpc:notificationMgr.DeleteNotifications, rpc:notificationMgr.GetAllNotifications, rpc:notificationMgr.GetByGroupID, rpc:notificationMgr.GetUnprocessed, rpc:notificationMgr.MarkAsProcessed
    use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
    use eve_proto::{marshal, DBRowDescriptor};
    use eve_server::router::NotificationTarget;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(982);
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // ========================================================================
    // P-3: objectCaching.GetCachableObject, CachedMethodCallResult, Invalidation
    // ========================================================================
    let units_oid = PyRep::String("config.Units".into());
    let cached_units_obj = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "GetCachableObject",
            &[
                PyRep::Bool(false),
                units_oid.clone(),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Integer(888_444),
            ],
            &kw,
            None,
        )
        .await
        .expect("objectCaching.GetCachableObject(config.Units)");

    // Verify wire round-trip of objectCaching.CachedObject and inner marshaled payload
    let co_wire = marshal::encode(&cached_units_obj);
    let co_decoded = marshal::decode(&co_wire).expect("decode objectCaching.CachedObject");
    assert_eq!(co_decoded, cached_units_obj);
    let (ts1, ver1) = match &co_decoded {
        PyRep::Object { type_name, args } => {
            assert_eq!(type_name, "objectCaching.CachedObject");
            let PyRep::Tuple(items) = args.as_ref() else {
                panic!("Expected 7-tuple in objectCaching.CachedObject");
            };
            assert_eq!(items.len(), 7);
            let PyRep::Tuple(ver_pair) = &items[0] else {
                panic!("Expected (timestamp, version) tuple");
            };
            let ts = ver_pair[0].as_i64().unwrap();
            let ver = ver_pair[1].as_i64().unwrap();
            let PyRep::Buffer(inner_bytes) = &items[4] else {
                panic!("Expected Buffer in CachedObject lret");
            };
            let inner_rep = marshal::decode(inner_bytes).expect("decode inner cached payload");
            assert!(matches!(inner_rep, PyRep::IndexRowset { .. }));
            assert_eq!(items[6], units_oid);
            (ts, ver)
        }
        other => panic!("Expected PyRep::Object, got {other:?}"),
    };

    // Verify CachedMethodCallResult inline and hint forms round-trip through marshal
    let wrapped_cmcr = ctx.cache_method_call_result(
        "config",
        "GetStationSolarSystemsByOwner",
        ctx.build_config_units_payload(),
        "run",
        None,
    );
    let cmcr_wire = marshal::encode(&wrapped_cmcr);
    assert_eq!(marshal::decode(&cmcr_wire).unwrap(), wrapped_cmcr);

    // Unknown config.* cache key must fail with CachedObjectNotFound (no empty-rowset fallback)
    let unknown_cache_err = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "GetCachableObject",
            &[
                PyRep::Integer(0),
                PyRep::Tuple(vec![
                    PyRep::String("Method Call".into()),
                    PyRep::String("server".into()),
                    PyRep::Tuple(vec![
                        PyRep::String("config".into()),
                        PyRep::String("GetMapData".into()),
                    ]),
                ]),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0)]),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unknown cache key must return CachedObjectNotFound");
    assert_eq!(unknown_cache_err.code, "CachedObjectNotFound");

    // Non-GM caller must be rejected on objectCaching.InvalidateCachedObjects
    let non_gm_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "InvalidateCachedObjects",
            &[PyRep::List(vec![units_oid.clone()])],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM InvalidateCachedObjects must fail");
    assert_eq!(non_gm_inv.code, "InsufficientRole");

    // Grant GM role, invalidate, and re-populate with updated payload -> timestamp strictly advances and version changes
    sess.role |= eve_db::service_roles::ROLE_GML;
    let inv_cnt = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "InvalidateCachedObjects",
            &[PyRep::List(vec![units_oid.clone()])],
            &kw,
            None,
        )
        .await
        .expect("InvalidateCachedObjects as GM");
    assert_eq!(inv_cnt, PyRep::Integer(1));
    let rec2 = ctx.put_cached_object(
        units_oid.clone(),
        PyRep::rowset(vec!["unitID"], vec![vec![PyRep::Integer(999)]]),
        true,
    );
    assert!(rec2.timestamp > ts1);
    assert_ne!(rec2.version as i64, ver1);
    let hint_rep = rec2.to_cached_method_call_hint_pyrep("run");
    assert_eq!(marshal::decode(&marshal::encode(&hint_rep)).unwrap(), hint_rep);
    sess.role &= !eve_db::service_roles::ROLE_GML;

    // ========================================================================
    // P-4: config.* RPCs (GetMultiOwnersEx, GetMultiLocationsEx, GetMultiStationEx,
    //      GetMultiInvTypesEx, GetMultiCorpTickerNamesEx, GetStationSolarSystemsByOwner, etc.)
    // ========================================================================
    let owners_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_000_035),
                PyRep::Integer(500_001),
            ])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiOwnersEx");
    if let PyRep::TupleSet { columns, rows } = &owners_rep {
        assert_eq!(
            columns,
            &vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"]
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], PyRep::Integer(90_000_001));
    } else {
        panic!("Expected TupleSet from GetMultiOwnersEx");
    }

    // Invalid non-integer element in GetMultiOwnersEx must fail with InvalidArgument
    let bad_owners = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![PyRep::String("not-an-int".into())])],
            &kw,
            None,
        )
        .await
        .expect_err("non-integer in ownerIDs must fail");
    assert_eq!(bad_owners.code, "InvalidArgument");

    let locs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiLocationsEx",
            &[PyRep::List(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(60_003_760),
                PyRep::Integer(100_000_001),
            ])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiLocationsEx");
    if let PyRep::TupleSet { columns, rows } = &locs_rep {
        assert_eq!(
            columns,
            &vec![
                "locationID",
                "locationName",
                "x",
                "y",
                "z",
                "locationNameID"
            ]
        );
        assert_eq!(rows.len(), 3);
    } else {
        panic!("Expected TupleSet from GetMultiLocationsEx");
    }

    let stations_rep = eve_server::services::config_service::get_multi_station_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(60_003_760)])],
        &kw,
    )
    .expect("config_service::get_multi_station_ex");
    if let PyRep::TupleSet { rows, .. } = &stations_rep {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], PyRep::Integer(60_003_760));
        assert_eq!(rows[0][3], PyRep::Integer(30_000_142));
    } else {
        panic!("Expected TupleSet from get_multi_station_ex");
    }

    let inv_types_rep = eve_server::services::config_service::get_multi_inv_types_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(587)])],
        &kw,
    )
    .expect("config_service::get_multi_inv_types_ex");
    let inv_types_wire = marshal::encode(&inv_types_rep);
    let inv_types_decoded = marshal::decode(&inv_types_wire).expect("decode get_multi_inv_types_ex");
    assert_eq!(inv_types_decoded, inv_types_rep);
    if let PyRep::List(rows) = &inv_types_decoded {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dbrow_get("typeID"), Some(&PyRep::Integer(34)));
        assert_eq!(rows[1].dbrow_get("typeID"), Some(&PyRep::Integer(587)));
    } else {
        panic!("Expected List of DBRow from get_multi_inv_types_ex");
    }

    let tickers_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiCorpTickerNamesEx",
            &[PyRep::List(vec![PyRep::Integer(1_000_035)])],
            &kw,
            None,
        )
        .await
        .expect("config.GetMultiCorpTickerNamesEx");
    if let PyRep::List(rows) = &tickers_rep {
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].dbrow_get("corporationID"),
            Some(&PyRep::Integer(1_000_035))
        );
    } else {
        panic!("Expected List of DBRow from GetMultiCorpTickerNamesEx");
    }

    let owner_stations_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetStationSolarSystemsByOwner",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("config.GetStationSolarSystemsByOwner");
    if let PyRep::Rowset { columns, rows } = &owner_stations_rep {
        assert_eq!(columns, &vec!["stationID", "solarSystemID"]);
        assert!(rows
            .iter()
            .any(|r| r[0] == PyRep::Integer(60_003_760) && r[1] == PyRep::Integer(30_000_142)));
    } else {
        panic!("Expected Rowset from GetStationSolarSystemsByOwner");
    }

    let map_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMap",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("config.GetMap");
    assert!(map_rep.row_count().unwrap_or(0) > 0);

    let map_objs_rep = eve_server::services::config_service::get_map_objects(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .expect("config_service::get_map_objects");
    assert!(map_objs_rep.is_crowset());
    assert!(!map_objs_rep.crowset_rows().unwrap().is_empty());

    let map_conns_rep = eve_server::services::config_service::get_map_connections(
        &ctx,
        &sess,
        &[PyRep::Integer(30_000_142)],
    )
    .expect("config_service::get_map_connections");
    assert!(map_conns_rep.row_count().unwrap_or(0) > 0);

    // Unhandled config methods and unimported SDE tables must return UnimplementedRPC (no fabricated rows)
    let graphics_err = eve_server::services::config_service::get_multi_graphics_ex(&ctx, &sess)
        .expect_err("get_multi_graphics_ex must return UnimplementedRPC");
    assert_eq!(graphics_err.code, "UnimplementedRPC");

    let cel_stat_err = eve_server::services::config_service::get_celestial_statistic(&ctx, &sess)
        .expect_err("get_celestial_statistic must return UnimplementedRPC");
    assert_eq!(cel_stat_err.code, "UnimplementedRPC");

    let unhandled_cfg_err = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMapData",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect_err("unhandled config.GetMapData must return UnimplementedRPC");
    assert_eq!(unhandled_cfg_err.code, "UnimplementedRPC");

    // ========================================================================
    // P-5: Binary MachoPacket notifications & notificationMgr service
    // ========================================================================
    // Offline character 90_000_002 must not accumulate queued notifications
    let offline_emitted = ctx.notify_account_change(90_000_002, "cash", 90_000_002, 100.0);
    assert!(offline_emitted.is_empty());

    let _ = ctx.drain_notifications_for_char(90_000_001);
    let emitted_acct = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 2_500_000.0);
    assert_eq!(emitted_acct.len(), 1);
    let corp_id = ctx.db.get_character(90_000_001).unwrap().corporation_id;
    let _ = ctx.notify_remote_message(
        NotificationTarget::Corporation(corp_id),
        "CorpNews",
        vec![("text", PyRep::String("Welcome".into()))],
    );
    let sample_item_row = DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(606),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(-1),
        PyRep::Integer(25),
        PyRep::Integer(6),
        PyRep::String(String::new()),
    ]);
    let _ = ctx.notify_item_change(90_000_001, sample_item_row, vec![(3, PyRep::Integer(30_000_142))]);
    let _ = ctx.notify_skill_trained(90_000_001, 200_001, 3_300, 4);
    let created_notif = ctx.create_character_notification(
        1_000_035,
        90_000_001,
        14,
        9,
        "bounty: 125000",
    );

    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert_eq!(drained.len(), 5);
    for notif in &drained {
        // Every queued notification must be a real binary-marshaled MachoPacket(Notification = 12)
        let decoded_pyrep =
            marshal::decode(&notif.marshaled_packet).expect("decode binary MachoPacket");
        let pkt = MachoPacket::from_pyrep(&decoded_pyrep).expect("parse MachoPacket");
        assert_eq!(pkt.packet_type, MachoPacketType::Notification);
        assert_eq!(
            pkt.destination,
            MachoAddress::Broadcast {
                broadcast_id: notif.event.clone(),
                id_type: "charid".into(),
                narrowcast: vec![90_000_001],
            }
        );
    }

    // Verify notificationMgr RPCs return dbutil.CRowset with DBRowDescriptor::notification_header()
    let unproc = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("notificationMgr.GetUnprocessed");
    assert!(unproc.is_crowset());
    let unproc_wire = marshal::encode(&unproc);
    assert_eq!(marshal::decode(&unproc_wire).unwrap(), unproc);
    let unproc_rows = unproc.crowset_rows().unwrap();
    assert_eq!(unproc_rows.len(), 1);
    assert_eq!(
        unproc_rows[0].dbrow_get("notificationID"),
        Some(&PyRep::Integer(created_notif.notification_id))
    );
    assert_eq!(
        unproc_rows[0].dbrow_get("processed"),
        Some(&PyRep::Bool(false))
    );
    assert_eq!(
        unproc_rows[0].dbrow_get("data"),
        Some(&PyRep::String("bounty: 125000".into()))
    );

    let by_grp = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "GetByGroupID",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.GetByGroupID");
    assert_eq!(by_grp.crowset_rows().unwrap().len(), 1);

    let marked = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "MarkAsProcessed",
            &[PyRep::List(vec![PyRep::Integer(
                created_notif.notification_id,
            )])],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.MarkAsProcessed");
    assert_eq!(marked, PyRep::Integer(1));

    let unproc_after = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("notificationMgr.GetUnprocessed after MarkAsProcessed");
    assert_eq!(unproc_after.crowset_rows().unwrap().len(), 0);

    let deleted = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "DeleteNotifications",
            &[PyRep::List(vec![PyRep::Integer(
                created_notif.notification_id,
            )])],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.DeleteNotifications");
    assert_eq!(deleted, PyRep::Integer(1));

    let all_after_del = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "GetAllNotifications",
            &[],
            &kw,
            None,
        )
        .await
        .expect("notificationMgr.GetAllNotifications after delete");
    assert_eq!(all_after_del.crowset_rows().unwrap().len(), 0);
}
