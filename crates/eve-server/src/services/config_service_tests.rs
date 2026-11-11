//! Unit tests for `config_service` (`config` metadata lookups, map queries, and `notificationMgr` inbox ops).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{NotificationRecord, ServerContext};
use eve_db::{AllianceRecord, CorporationRecord, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
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

fn auth_session(char_id: i64, corp_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s
}

#[test]
fn config_owners_locations_shortnames_and_map_queries() {
    let ctx = test_ctx();
    let cid = 930_001_i64;
    let corp_id = 98_300_001_i64;
    let ally_id = 99_300_001_i64;
    let item_id = 88_300_001_i64;
    let sys_id = 30000142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.name = "ConfigTester".to_string();
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(ally_id);
        snap.characters.insert(cid, ch);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = cid;
        corp.alliance_id = Some(ally_id);
        corp.name = "ConfigCorp".to_string();
        corp.ticker = "CFGC".to_string();
        snap.corporations.insert(corp_id, corp);

        let mut members = HashSet::new();
        members.insert(corp_id);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "ConfigAlliance".to_string(),
                short_name: "CFGA".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: members,
                ..Default::default()
            },
        );

        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: cid,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions
            .insert(item_id, [1_000.0, 2_000.0, 3_000.0]);
    });

    let mut unauth = SessionState::default();
    let kw = HashMap::new();
    let err_unauth = handle(&ctx, &mut unauth, "config", "GetMultiOwnersEx", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert!(err_unauth.code.starts_with("Unauth"));

    let mut sess = auth_session(cid, corp_id, sys_id);

    // GetMultiOwnersEx: resolves char, corp, alliance, EVE system (1), solar systems (typeID=5), plus duplicate deduplication
    let owners = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiOwnersEx",
        &[PyRep::List(vec![
            PyRep::Integer(cid),
            PyRep::Integer(corp_id),
            PyRep::Integer(ally_id),
            PyRep::Integer(1),
            PyRep::Integer(cid),
        ])],
        &kw,
    )
    .unwrap()
    .expect("owners lookup");
    assert_eq!(owners.row_count(), Some(4));

    let sys_owners = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiOwnersEx",
        &[PyRep::List(vec![
            PyRep::Integer(30_045_324),
            PyRep::Integer(30_002_055),
            PyRep::Integer(30_002_974),
            PyRep::Integer(30_003_788),
            PyRep::Integer(30_000_142),
        ])],
        &kw,
    )
    .unwrap()
    .expect("solar system owners lookup");
    let PyRep::TupleSet { rows: sys_rows, .. } = sys_owners else {
        panic!("expected TupleSet for GetMultiOwnersEx, got {sys_owners:?}");
    };
    assert_eq!(sys_rows.len(), 5);
    for row in &sys_rows {
        assert_eq!(row.get(2), Some(&PyRep::Integer(5)));
        assert_eq!(row.get(3), Some(&PyRep::Bool(false)));
        assert_eq!(row.get(4), Some(&PyRep::None));
    }

    // Invalid element in list raises InvalidArgument
    let bad_list = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiOwnersEx",
        &[PyRep::List(vec![PyRep::String("bad".into())])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_list.code, "InvalidArgument");

    // GetMultiLocationsEx: resolves item, system, station
    let station_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| s.stations.first())
        .map(|st| st.station_id)
        .unwrap_or(60003760);
    let locs = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiLocationsEx",
        &[PyRep::List(vec![
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(station_id),
        ])],
        &kw,
    )
    .unwrap()
    .expect("locations lookup");
    assert!(locs.row_count().unwrap_or(0) >= 2);

    // GetMultiAllianceShortNamesEx & GetMultiCorpTickerNamesEx
    let short_names = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiAllianceShortNamesEx",
        &[PyRep::List(vec![PyRep::Integer(ally_id)])],
        &kw,
    )
    .unwrap()
    .expect("short names");
    assert_eq!(short_names.row_count(), Some(1));

    let tickers = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiCorpTickerNamesEx",
        &[PyRep::List(vec![PyRep::Integer(corp_id)])],
        &kw,
    )
    .unwrap()
    .expect("corp tickers");
    assert_eq!(tickers.row_count(), Some(1));

    // GetStationSolarSystemsByOwner
    let owner_stations = handle(
        &ctx,
        &mut sess,
        "config",
        "GetStationSolarSystemsByOwner",
        &[PyRep::Integer(1_000_035)],
        &kw,
    )
    .unwrap()
    .expect("owner stations");
    assert!(owner_stations.row_count().is_some());

    // GetMap + get_map_objects + get_map_connections + get_multi_inv_types_ex + get_multi_station_ex
    let map_rep = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMap",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .unwrap()
    .expect("GetMap");
    assert!(map_rep.row_count().is_some());

    let map_objs = get_map_objects(&ctx, &sess, &[PyRep::Integer(i64::from(sys_id))]).expect("map objects");
    assert!(map_objs.row_count().is_some());

    let map_conns = get_map_connections(&ctx, &sess, &[PyRep::Integer(i64::from(sys_id))]).expect("map conns");
    assert!(map_conns.row_count().is_some());

    let inv_types = get_multi_inv_types_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(587), PyRep::Integer(34)])],
        &kw,
    )
    .expect("inv types");
    assert_eq!(inv_types.row_count(), Some(2));

    let stations_ex = get_multi_station_ex(
        &ctx,
        &sess,
        &[PyRep::List(vec![PyRep::Integer(station_id)])],
        &kw,
    )
    .expect("stations ex");
    assert!(stations_ex.row_count().is_some());

    assert_eq!(
        get_multi_graphics_ex(&ctx, &sess).unwrap_err().code,
        "UnimplementedRPC"
    );
    assert_eq!(
        get_celestial_statistic(&ctx, &sess).unwrap_err().code,
        "UnimplementedRPC"
    );
}

#[test]
fn notification_mgr_lifecycle_and_unknown_dispatch() {
    let ctx = test_ctx();
    let cid = 930_002_i64;
    let corp_id = 98_300_002_i64;
    let sys_id = 30000142_u32;

    {
        let mut recs = ctx.notification_records.write().unwrap();
        recs.push(NotificationRecord {
            notification_id: 9001,
            type_id: 1,
            group_id: 10,
            sender_id: 1_000_125,
            receiver_id: cid,
            created: 100,
            data: String::new(),
            processed: false,
            deleted: false,
            interacted: false,
        });
        recs.push(NotificationRecord {
            notification_id: 9002,
            type_id: 2,
            group_id: 20,
            sender_id: 1_000_125,
            receiver_id: cid,
            created: 200,
            data: String::new(),
            processed: false,
            deleted: false,
            interacted: false,
        });
        recs.push(NotificationRecord {
            notification_id: 9003,
            type_id: 2,
            group_id: 20,
            sender_id: 1_000_125,
            receiver_id: cid,
            created: 300,
            data: String::new(),
            processed: false,
            deleted: false,
            interacted: false,
        });
    }

    let mut sess = auth_session(cid, corp_id, sys_id);
    let kw = HashMap::new();

    // GetAllNotifications returns all 3
    let all = handle(&ctx, &mut sess, "notificationMgr", "GetAllNotifications", &[], &kw)
        .unwrap()
        .expect("get all notifications");
    assert_eq!(all.row_count(), Some(3));

    // GetByGroupID filters by group 20 -> 2 rows
    let by_grp = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "GetByGroupID",
        &[PyRep::Integer(20)],
        &kw,
    )
    .unwrap()
    .expect("get by group");
    assert_eq!(by_grp.row_count(), Some(2));

    // GetUnprocessed -> 3 rows initially
    let unproc = handle(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[], &kw)
        .unwrap()
        .expect("get unprocessed");
    assert_eq!(unproc.row_count(), Some(3));

    // MarkAsProcessed([9001]) -> updates 1
    let marked = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "MarkAsProcessed",
        &[PyRep::List(vec![PyRep::Integer(9001)])],
        &kw,
    )
    .unwrap()
    .expect("mark one processed");
    assert_eq!(marked, PyRep::Integer(1));

    // MarkGroupAsProcessed(20) -> updates remaining 2
    let marked_grp = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "MarkGroupAsProcessed",
        &[PyRep::Integer(20)],
        &kw,
    )
    .unwrap()
    .expect("mark group processed");
    assert_eq!(marked_grp, PyRep::Integer(2));

    // LogNotificationInteraction(9001)
    let logged = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "LogNotificationInteraction",
        &[PyRep::Integer(9001)],
        &kw,
    )
    .unwrap()
    .expect("log interaction");
    assert_eq!(logged, PyRep::Bool(true));

    // DeleteGroupNotifications(10) -> deletes 1 (id 9001)
    let del_grp = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "DeleteGroupNotifications",
        &[PyRep::Integer(10)],
        &kw,
    )
    .unwrap()
    .expect("delete group");
    assert_eq!(del_grp, PyRep::Integer(1));

    // DeleteNotifications([9002]) -> deletes 1
    let del_one = handle(
        &ctx,
        &mut sess,
        "notificationMgr",
        "DeleteNotifications",
        &[PyRep::List(vec![PyRep::Integer(9002)])],
        &kw,
    )
    .unwrap()
    .expect("delete one");
    assert_eq!(del_one, PyRep::Integer(1));

    // DeleteAllNotifications -> deletes remaining 1 (id 9003)
    let del_all = handle(&ctx, &mut sess, "notificationMgr", "DeleteAllNotifications", &[], &kw)
        .unwrap()
        .expect("delete all");
    assert_eq!(del_all, PyRep::Integer(1));

    assert!(handle(&ctx, &mut sess, "config", "", &[], &kw).is_none());
    assert!(handle(&ctx, &mut sess, "notificationMgr", "", &[], &kw).is_none());
}

#[test]
fn config_corp_ticker_names_includes_governance_logo_fields() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(102);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let corp_id = 98_440_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Logo Corp".into();
                __corp.ticker = "LOGO".into();
                __corp.ceo_id = 90_000_001;
                __corp.alliance_id = None;
                __corp.tax_rate = 0.05;
                __corp.description = String::new();
                __corp.url = String::new();
                __corp.hq_station_id = None;
                __corp.members = HashSet::new();
                __corp.member_roles = HashMap::new();
                __corp.applications = HashMap::new();
                __corp.wallet_divisions = vec![];
                let __mw = eve_db::IskCents(0);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.logo = (415, 416, 417, 671);
        gov.logo_extra_colors = (672, 673);
    });
    let kw = HashMap::new();
    let rep = handle(
        &ctx,
        &mut sess,
        "config",
        "GetMultiCorpTickerNamesEx",
        &[PyRep::List(vec![PyRep::Integer(corp_id)])],
        &kw,
    )
    .unwrap()
    .expect("corp ticker lookup");
    let PyRep::List(rows) = rep else {
        panic!("Expected PackedRow list, got {rep:?}");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dbrow_get("corporationID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(rows[0].dbrow_get("tickerName"), Some(&PyRep::String("LOGO".into())));
    assert_eq!(rows[0].dbrow_get("shape1"), Some(&PyRep::Integer(415)));
    assert_eq!(rows[0].dbrow_get("shape2"), Some(&PyRep::Integer(416)));
    assert_eq!(rows[0].dbrow_get("shape3"), Some(&PyRep::Integer(417)));
    assert_eq!(rows[0].dbrow_get("color1"), Some(&PyRep::Real(671.0)));
    assert_eq!(rows[0].dbrow_get("color2"), Some(&PyRep::Real(672.0)));
    assert_eq!(rows[0].dbrow_get("color3"), Some(&PyRep::Real(673.0)));
}

#[cfg(test)]
#[path = "config_service_extra_unit_tests.rs"]
mod config_service_extra_unit_tests;
