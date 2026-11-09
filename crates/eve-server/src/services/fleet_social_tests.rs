use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, FleetMemberRecord, FleetRecord, FleetRole,
    InventoryFlag, InventoryItemRecord, IskCents, MailingListRecord,
};
use std::collections::{HashMap, HashSet};

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn fleet_boost_scope_and_parse_dest_id_boundaries() {
    let mut fleet = FleetRecord {
        fleet_id: 1,
        boss_id: 100,
        members: vec![100, 101, 102, 103, 104],
        wings: vec![1, 2],
        squads: vec![10, 20],
        free_move: false,
        is_registered: false,
        wing_details: HashMap::new(),
        squad_details: HashMap::new(),
        member_details: HashMap::new(),
        invites: HashSet::new(),
        motd: String::new(),
    };
    let mk_member = |cid: i64, role: FleetRole, wing: Option<i64>, squad: Option<i64>| FleetMemberRecord {
        character_id: cid,
        role,
        wing_id: wing,
        squad_id: squad,
        takes_fleet_warp: true,
        accepts_regroup: true,
        accepts_conduit_jumps: true,
        job: 0,
        role_name: String::new(),
        join_time_unix: 1000,
    };
    fleet.member_details.insert(100, mk_member(100, FleetRole::FleetCommander, None, None));
    fleet.member_details.insert(101, mk_member(101, FleetRole::WingCommander, Some(1), None));
    fleet.member_details.insert(102, mk_member(102, FleetRole::SquadCommander, Some(1), Some(10)));
    fleet.member_details.insert(103, mk_member(103, FleetRole::SquadMember, Some(1), Some(10)));
    fleet.member_details.insert(104, mk_member(104, FleetRole::SquadMember, Some(2), Some(20)));

    assert!(is_in_fleet_boost_scope(&fleet, 999, 999));
    assert!(!is_in_fleet_boost_scope(&fleet, 999, 100));
    assert!(!is_in_fleet_boost_scope(&fleet, 100, 999));
    assert!(is_in_fleet_boost_scope(&fleet, 100, 104));
    assert!(is_in_fleet_boost_scope(&fleet, 101, 103));
    assert!(!is_in_fleet_boost_scope(&fleet, 101, 104));
    assert!(is_in_fleet_boost_scope(&fleet, 102, 103));
    assert!(!is_in_fleet_boost_scope(&fleet, 102, 104));
    assert!(!is_in_fleet_boost_scope(&fleet, 103, 102));

    let empty_kw = HashMap::new();
    assert_eq!(parse_dest_id(&[PyRep::Integer(42)], &empty_kw), Some(42));
    let mut kw = HashMap::new();
    kw.insert("itemID".to_string(), PyRep::Integer(77));
    assert_eq!(parse_dest_id(&[], &kw), Some(77));
    let mut kw2 = HashMap::new();
    kw2.insert("destinationID".to_string(), PyRep::Integer(88));
    assert_eq!(parse_dest_id(&[], &kw2), Some(88));
    assert_eq!(parse_dest_id(&[], &empty_kw), None);
}

#[test]
fn bookmark_expiry_and_sweep_helpers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            501,
            BookmarkRecord {
                bookmark_id: 501,
                owner_id: 100_000_001,
                solar_system_id: 30_000_142,
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                label: "perm".into(),
                memo: String::new(),
                folder_id: Some(10),
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(501, 0);
        snap.bookmarks.insert(
            502,
            BookmarkRecord {
                bookmark_id: 502,
                owner_id: 100_000_001,
                solar_system_id: 30_000_142,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: None,
                label: "expiring".into(),
                memo: String::new(),
                folder_id: Some(20),
                is_shared: true,
            },
        );
        snap.bookmark_expiries.insert(502, 500);
        snap.bookmark_subfolders.insert(502, 21);
        snap.bookmark_folders.insert(
            20,
            BookmarkFolderRecord {
                folder_id: 20,
                owner_id: 100_000_001,
                folder_name: "shared".into(),
                description: String::new(),
                is_shared: true,
                corporation_id: Some(1_000_009),
                admin_group_id: None,
                manage_group_id: None,
                use_group_id: None,
                view_group_id: None,
            },
        );
        snap.mailing_lists.insert(
            9001,
            MailingListRecord {
                list_id: 9001,
                name: "test_list".into(),
                owner_id: 100_000_001,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: HashMap::from([(100_000_001, 3), (100_000_002, 1)]),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: HashSet::new(),
            },
        );
        snap.trashed_mails.insert((100_000_001, 801));
    });

    assert_eq!(mailing_list_member_ids(&ctx, 9001).len(), 2);
    assert!(mailing_list_member_ids(&ctx, 9999).is_empty());
    assert!(get_trashed_mails(&ctx).contains(&(100_000_001, 801)));

    assert!(!is_bookmark_expired(&ctx, 501, 1000));
    assert!(!is_bookmark_expired(&ctx, 502, 499));
    assert!(is_bookmark_expired(&ctx, 502, 500));

    let swept = sweep_expired_bookmarks_and_notify(&ctx, 500);
    assert_eq!(swept, vec![502]);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.bookmarks.contains_key(&501));
        assert!(!snap.bookmarks.contains_key(&502));
        assert!(!snap.bookmark_subfolders.contains_key(&502));
    });
}

#[tokio::test]
async fn office_manager_lifecycle_and_fleet_rpc_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let corp_id = 98_000_001;
    let station_id = *ctx.sde.stations.keys().find(|&&id| id != 60_015_187).unwrap_or(&60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "test corp".into();
            __corp.ticker = "TSTC".into();
            __corp.ceo_id = char_id;
            __corp.hq_station_id = Some(station_id);
            __corp.tax_rate = 0.05;
            __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = make_session(char_id, corp_id, Some(station_id));
    let kw = HashMap::new();

    let quote = handle(&ctx, &mut session, "officeManager", "GetPriceQuote", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(quote.as_f64(), Some(10_000.0));

    let empty_before = handle(&ctx, &mut session, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty_before.as_i64(), Some(16));

    let impounded_before = handle(&ctx, &mut session, "officeManager", "HasCorpImpoundedItems", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(impounded_before, PyRep::Bool(false));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_002,
            InventoryItemRecord {
                item_id: 777_002,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let impounded_after = handle(&ctx, &mut session, "officeManager", "HasCorpImpoundedItems", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(impounded_after, PyRep::Bool(true));

    let rent = handle(&ctx, &mut session, "officeManager", "RentOffice", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rent.dict_get("stationID").and_then(|v| v.as_i64()), Some(station_id));

    let my_office = handle(&ctx, &mut session, "officeManager", "GetMyCorporationsOffice", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(my_office.dict_get("corporationID").and_then(|v| v.as_i64()), Some(corp_id));

    let my_offices = handle(&ctx, &mut session, "officeManager", "GetMyCorporationsOffices", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(my_offices, PyRep::None));

    let corps_with = handle(&ctx, &mut session, "officeManager", "GetCorporationsWithOffices", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(corps_with, PyRep::None));

    let unrent = handle(&ctx, &mut session, "officeManager", "UnrentOffice", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unrent.dict_get("unrented").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&777_001).map(|i| i.flag), Some(InventoryFlag::Deliveries));
    });

    let formations = handle(&ctx, &mut session, "beyonce", "GetFormations", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(formations, PyRep::Tuple(_)));

    let (fleet_id, mut fleet_session) = seed_multi_member_fleet(&ctx).await;
    let moved = handle(
        &ctx,
        &mut fleet_session,
        "fleetObjectHandler",
        "MassMoveMembers",
        &[PyRep::List(vec![PyRep::Integer(90_000_003)]), PyRep::Integer(710_001), PyRep::Integer(720_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(moved.dict_get("movedCount").and_then(|v| v.as_i64()), Some(1));

    let wings = handle_get_wings(&ctx, &mut fleet_session).unwrap().unwrap();
    assert!(matches!(wings, PyRep::Dict(_)));
    let init = handle_get_init_state(&ctx, &mut fleet_session).unwrap().unwrap();
    assert_eq!(init.dict_get("fleetID").and_then(|v| v.as_i64()), Some(fleet_id));

    let leader = handle(
        &ctx,
        &mut fleet_session,
        "fleetMgr",
        "MakeLeader",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(leader.dict_get("newBossID").and_then(|v| v.as_i64()), Some(90_000_002));
}
