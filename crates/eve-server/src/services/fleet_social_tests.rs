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
    assert_eq!(empty_before.as_i64(), Some(24));

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

#[tokio::test]
async fn office_manager_uses_station_rental_cost_map_and_emits_account_and_item_notifications() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 90_000_001_i64;
    let char2_id = 90_000_002_i64;
    let corp_id = 98_000_055_i64;
    let station_id = 60_003_760_i64;
    let uid = char_id as u64;
    let uid2 = char2_id as u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Quote Corp", "QUOT", char_id)
            .with_master_wallet(IskCents::from_isk(1_000_000));
        corp.hq_station_id = Some(station_id);
        corp.tax_rate = 0.05;
        corp.members.insert(char_id);
        corp.members.insert(char2_id);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut ch2 = snap
            .characters
            .get(&char_id)
            .cloned()
            .expect("primary character seeded");
        ch2.character_id = char2_id;
        ch2.user_id = uid2;
        ch2.name = "Corp Member 2".into();
        ch2.corporation_id = corp_id;
        ch2.station_id = Some(station_id);
        snap.characters.insert(char2_id, ch2);
        for acct_uid in [uid, uid2] {
            if !snap.accounts.values().any(|a| a.user_id == acct_uid) {
                let uname = format!("u{acct_uid}");
                snap.accounts.insert(
                    uname.clone(),
                    eve_db::AccountRecord {
                        user_id: acct_uid,
                        username: uname,
                        password_token: String::new(),
                        role: 0,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
            }
        }
        snap.station_office_rental_costs.insert(station_id, 50_000.0);
        snap.items.retain(|_, i| !(i.location_id == station_id && i.type_id == 27));
        snap.items.insert(
            920_000_050,
            InventoryItemRecord {
                item_id: 920_000_050,
                type_id: 27,
                owner_id: 98_999_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = make_session(char_id, corp_id, Some(station_id));
    session.user_id = Some(uid);
    session.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db
        .acquire_online_lock(uid, session.session_id, Some(char_id), true, 1_700_000_000)
        .unwrap();
    let sess2 = make_session(char2_id, corp_id, Some(station_id));
    ctx.db
        .acquire_online_lock(uid2, sess2.session_id + 1, Some(char2_id), true, 1_700_000_000)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(char2_id);
    let kw = HashMap::new();

    // Base 50,000 ISK with 1 existing office -> 50,000 * 1.5 = 75,000 ISK.
    let quote = handle(&ctx, &mut session, "officeManager", "GetPriceQuote", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(quote.as_f64(), Some(75_000.0));

    let rent = handle(&ctx, &mut session, "officeManager", "RentOffice", &[PyRep::Integer(station_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    let office_id = rent.dict_get("officeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(rent.dict_get("cost").and_then(|v| v.as_f64()), Some(75_000.0));

    let expected_creditor = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| s.owner_corporation_id)
        .unwrap_or(1_000_035);
    ctx.db.with_snapshot(|snap| {
        let journals = snap
            .corp_wallet_journals
            .get(&(corp_id, 1))
            .expect("corp division 1 journal recorded");
        let rent_entry = journals
            .iter()
            .find(|e| e.ref_type == "office_rental_fee")
            .expect("office_rental_fee journal entry present");
        assert_eq!(rent_entry.amount, -75_000.0);
        assert_eq!(rent_entry.balance, 925_000.0);
        assert_eq!(rent_entry.first_party_id, Some(char_id));
        assert_eq!(rent_entry.second_party_id, Some(expected_creditor));
    });

    for member_id in [char_id, char2_id] {
        let notifs = ctx.drain_notifications_for_char(member_id);
        assert_eq!(
            notifs
                .iter()
                .filter(|n| n.event == "OnAccountChange")
                .map(|n| n.args.clone())
                .collect::<Vec<_>>(),
            vec![vec![
                PyRep::String("cash".into()),
                PyRep::Integer(corp_id),
                PyRep::Real(925_000.0),
            ]],
            "member {member_id} OnAccountChange"
        );
        assert_eq!(
            notifs.iter().filter(|n| n.event == "OnItemChange").count(),
            1,
            "member {member_id} corp-targeted OnItemChange"
        );
        assert_eq!(
            notifs
                .iter()
                .filter(|n| n.event == "OnOfficeRentalChange")
                .map(|n| n.args.clone())
                .collect::<Vec<_>>(),
            vec![vec![PyRep::Integer(corp_id), PyRep::Integer(office_id)]],
            "member {member_id} OnOfficeRentalChange"
        );
        let bill_creditor = notifs
            .iter()
            .find(|n| n.event == "OnBillReceived")
            .and_then(|n| n.args.first())
            .and_then(|b| b.dict_get("creditorID"))
            .and_then(|v| v.as_i64());
        assert_eq!(
            bill_creditor,
            Some(expected_creditor),
            "member {member_id} OnBillReceived creditorID"
        );
    }

    // Renting in an Upwell structure debits active session.corp_account_key division (div 2 / 1001)
    // and credits the structure owner corporation's master wallet with journal + OnAccountChange.
    let struct_id = 1_030_000_000_055_i64;
    let owner_corp_id = 98_000_056_i64;
    let owner_char_id = 90_000_056_i64;
    let owner_uid = owner_char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .set_division_balance(2, IskCents::from_isk(200_000));
        let mut owner_corp =
            eve_db::CorporationRecord::new(owner_corp_id, "Structure Owner", "SOWN", owner_char_id)
                .with_master_wallet(IskCents::from_isk(500_000));
        owner_corp.members.insert(owner_char_id);
        snap.corporations.insert(owner_corp_id, owner_corp);
        let mut owner_ch = snap.characters.get(&char_id).cloned().unwrap();
        owner_ch.character_id = owner_char_id;
        owner_ch.user_id = owner_uid;
        owner_ch.name = "Owner Pilot".into();
        owner_ch.corporation_id = owner_corp_id;
        snap.characters.insert(owner_char_id, owner_ch);
        let uname = format!("u{owner_uid}");
        snap.accounts.insert(
            uname.clone(),
            eve_db::AccountRecord {
                user_id: owner_uid,
                username: uname,
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id,
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let mut prof = crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
            profile_id: 501,
            name: "Open Offices Profile".into(),
            owner_corp_id,
            ..Default::default()
        };
        prof.general_settings.insert(
            crate::services::structure_profile_services::SETTING_CORP_RENT_OFFICE,
            true,
        );
        ext.profiles_by_id.insert(501, prof.clone());
        ext.access_profiles.insert(struct_id, prof);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        snap.station_office_rental_costs.insert(struct_id, 40_000.0);
    });
    ctx.db
        .acquire_online_lock(owner_uid, 9_056, Some(owner_char_id), true, 1_700_000_000)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(owner_char_id);

    session.station_id = None;
    session.structure_id = Some(struct_id);
    session.corp_account_key = 1001;
    let rent_upwell = handle(
        &ctx,
        &mut session,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rent_upwell.dict_get("cost").and_then(|v| v.as_f64()), Some(40_000.0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp_id].division_balance(2),
            IskCents::from_isk(160_000)
        );
        assert_eq!(
            snap.corporations[&owner_corp_id].division_balance(1),
            IskCents::from_isk(540_000)
        );
        let cred_journal = snap
            .corp_wallet_journals
            .get(&(owner_corp_id, 1))
            .expect("structure owner corp journal recorded");
        assert!(cred_journal
            .iter()
            .any(|e| e.ref_type == "office_rental_fee" && (e.amount - 40_000.0).abs() < 1e-6));
    });
    let owner_notifs = ctx.drain_notifications_for_char(owner_char_id);
    assert_eq!(
        owner_notifs
            .iter()
            .filter(|n| n.event == "OnAccountChange")
            .map(|n| n.args.clone())
            .collect::<Vec<_>>(),
        vec![vec![
            PyRep::String("cash".into()),
            PyRep::Integer(owner_corp_id),
            PyRep::Real(540_000.0),
        ]]
    );

    // A second corporation renting at the same Upwell structure pays the static owner fee
    // (`40_000.0`), not an occupancy-scaled 1.5x NPC quote.
    let corp2_id = 98_000_057_i64;
    let char2_id = 90_000_057_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp2 =
            eve_db::CorporationRecord::new(corp2_id, "Second Tenant", "TEN2", char2_id)
                .with_master_wallet(IskCents::from_isk(100_000));
        corp2.members.insert(char2_id);
        corp2.member_roles.insert(char2_id, u64::MAX);
        snap.corporations.insert(corp2_id, corp2);
        let mut ch2 = snap.characters.get(&char_id).cloned().unwrap();
        ch2.character_id = char2_id;
        ch2.user_id = char2_id as u64;
        ch2.name = "Tenant Two CEO".into();
        ch2.corporation_id = corp2_id;
        ch2.station_id = None;
        ch2.structure_id = Some(struct_id);
        snap.characters.insert(char2_id, ch2);
    });
    let mut sess2 = session.clone();
    sess2.session_id = 9_057;
    sess2.character_id = Some(char2_id);
    sess2.user_id = Some(char2_id as u64);
    sess2.corporation_id = Some(corp2_id);
    sess2.corp_role = u64::MAX;
    sess2.corp_account_key = 1000;
    sess2.station_id = None;
    sess2.structure_id = Some(struct_id);

    let quote_upwell_2 = handle(
        &ctx,
        &mut sess2,
        "officeManager",
        "GetPriceQuote",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(quote_upwell_2, PyRep::Real(40_000.0));
    let rent_upwell_2 = handle(
        &ctx,
        &mut sess2,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rent_upwell_2.dict_get("cost").and_then(|v| v.as_f64()), Some(40_000.0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp2_id].division_balance(1),
            IskCents::from_isk(60_000)
        );
        assert_eq!(
            snap.corporations[&owner_corp_id].division_balance(1),
            IskCents::from_isk(580_000)
        );
    });
}

#[tokio::test]
async fn upwell_office_acl_domain20_core_zero_fee_and_unlimited_slots_vs_npc_24_cap() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::{
        encode_service_setting_bps, SERVICE_OFFICES, SETTING_CORP_RENT_OFFICE,
        SETTING_HOUSING_CAN_DOCK,
    };

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let kw = HashMap::new();
    let struct_id = 1_030_000_000_220_i64;
    let flex_id = 1_030_000_000_221_i64;
    let npc_station_id = 60_003_760_i64;

    let owner_corp_id = 98_000_220_i64;
    let owner_char_id = 90_000_220_i64;
    let paid_corp_id = 98_000_221_i64;
    let paid_char_id = 90_000_221_i64;
    let free_corp_id = 98_000_222_i64;
    let free_char_id = 90_000_222_i64;
    let dock_only_corp_id = 98_000_223_i64;
    let dock_only_char_id = 90_000_223_i64;

    let seed_corp_and_ceo = |snap: &mut eve_db::WorldStateSnapshot,
                             corp_id: i64,
                             char_id: i64,
                             name: &str,
                             ticker: &str,
                             balance_isk: i64| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, name, ticker, char_id)
            .with_master_wallet(IskCents::from_isk(balance_isk));
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(corp_id, corp);
        snap.characters.insert(
            char_id,
            eve_db::CharacterRecord {
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("{ticker} CEO"),
                corporation_id: corp_id,
                solar_system_id: 30_000_142,
                structure_id: Some(struct_id),
                balance: IskCents::from_isk(0),
                ..Default::default()
            },
        );
        let uname = format!("u{}", char_id);
        snap.accounts.insert(
            uname.clone(),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: uname,
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    };

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp_and_ceo(snap, owner_corp_id, owner_char_id, "Citadel Host", "HST", 0);
        seed_corp_and_ceo(snap, paid_corp_id, paid_char_id, "Paid Tenant", "PAID", 100_000);
        seed_corp_and_ceo(snap, free_corp_id, free_char_id, "Free Ally", "FREE", 0);
        seed_corp_and_ceo(
            snap,
            dock_only_corp_id,
            dock_only_char_id,
            "Dock Only Corp",
            "DOCK",
            500_000,
        );

        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id,
                solar_system_id: 30_000_142,
                name: "ACL Astrahus".into(),
                state: "NoCore".into(),
                quantum_core_installed: false,
                ..Default::default()
            },
        );
        snap.structures.insert(
            flex_id,
            eve_db::UpwellStructureRecord {
                structure_id: flex_id,
                type_id: 35_841,
                owner_corp_id,
                solar_system_id: 30_000_142,
                name: "Flex Gate".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                ..Default::default()
            },
        );
    });

    let mut owner_sess = make_session(owner_char_id, owner_corp_id, None);
    owner_sess.structure_id = Some(struct_id);
    let mut paid_sess = make_session(paid_char_id, paid_corp_id, None);
    paid_sess.structure_id = Some(struct_id);
    let mut free_sess = make_session(free_char_id, free_corp_id, None);
    free_sess.structure_id = Some(struct_id);
    let mut dock_only_sess = make_session(dock_only_char_id, dock_only_corp_id, None);
    dock_only_sess.structure_id = Some(struct_id);

    // 1. Un-cored / NoCore / Anchoring / FLEX structures reject RentOffice & GetPriceQuote with
    // StructureNotOperational and report 0 empty office slots.
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "RentOffice", &[PyRep::Integer(flex_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDockedAtStation"
    );
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(flex_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.quantum_core_installed = false;
    });
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    // 2. Operational + cored structure with NO access profile rejects outsiders, and a profile
    // that only grants docking (SETTING_HOUSING_CAN_DOCK=19 / legacy 1) without domain 20 STILL
    // rejects RentOffice & GetPriceQuote and returns 0 empty offices.
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.quantum_core_installed = true;
    });
    assert_eq!(
        handle(&ctx, &mut paid_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut paid_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            701,
            AccessGroupRecord {
                group_id: 701,
                creator_id: owner_char_id,
                owner_corp_id,
                name: "Dock Only Group".into(),
                description: String::new(),
                members: HashMap::from([
                    (dock_only_corp_id, 1),
                    (paid_corp_id, 1),
                    (free_corp_id, 1),
                ]),
            },
        );
        let mut dock_prof = StructureAccessProfile {
            profile_id: 710,
            owner_corp_id,
            ..Default::default()
        };
        dock_prof.allowed_corporations =
            HashSet::from([dock_only_corp_id, paid_corp_id, free_corp_id]);
        dock_prof.general_settings.insert(1, true);
        dock_prof.general_settings.insert(SETTING_HOUSING_CAN_DOCK, true);
        dock_prof.service_acl_rules.insert(1, vec![(701, 0)]);
        dock_prof
            .service_acl_rules
            .insert(SETTING_HOUSING_CAN_DOCK, vec![(701, 0)]);
        ext.profiles_by_id.insert(710, dock_prof.clone());
        ext.access_profiles.insert(struct_id, dock_prof);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle(&ctx, &mut dock_only_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut dock_only_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut dock_only_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    // 3. Configure domain 20 (SETTING_CORP_RENT_OFFICE) ACL tiers:
    // - paid_corp_id -> 25,000 ISK
    // - free_corp_id & owner_corp_id -> 0 ISK
    // - dock_only_corp_id -> NOT in domain 20 -> rejected!
    let paid_bps = encode_service_setting_bps(SETTING_CORP_RENT_OFFICE, 25_000.0).unwrap();
    let free_bps = encode_service_setting_bps(SETTING_CORP_RENT_OFFICE, 0.0).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            702,
            AccessGroupRecord {
                group_id: 702,
                creator_id: owner_char_id,
                owner_corp_id,
                name: "Paid Office Group".into(),
                description: String::new(),
                members: HashMap::from([(paid_corp_id, 1)]),
            },
        );
        ext.owner_groups.insert(
            703,
            AccessGroupRecord {
                group_id: 703,
                creator_id: owner_char_id,
                owner_corp_id,
                name: "Free Office Group".into(),
                description: String::new(),
                members: HashMap::from([(free_corp_id, 1), (owner_corp_id, 1)]),
            },
        );
        let prof = ext.access_profiles.get_mut(&struct_id).unwrap();
        prof.service_acl_rules.insert(
            SETTING_CORP_RENT_OFFICE,
            vec![(702, paid_bps), (703, free_bps)],
        );
        save_ext_state(snap, &ext);
    });

    assert_eq!(
        handle(&ctx, &mut dock_only_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );

    assert_eq!(
        handle(&ctx, &mut paid_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Real(25_000.0)
    );
    assert!(
        handle(&ctx, &mut paid_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 24
    );
    let paid_rent = handle(&ctx, &mut paid_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paid_rent.dict_get("cost").and_then(|v| v.as_f64()), Some(25_000.0));

    // Free (0 ISK) tier succeeds even with a 0 ISK wallet, emits OnItemChange & OnOfficeRentalChange,
    // and skips bogus zero-value OnAccountChange / OnBillReceived / 0 ISK journals.
    assert_eq!(
        handle(&ctx, &mut free_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Real(0.0)
    );
    let _ = ctx.db.acquire_online_lock(free_char_id as u64, 9_222, Some(free_char_id), true, 1_700_000_000);
    let _ = ctx.drain_notifications_for_char(free_char_id);
    let free_rent = handle(&ctx, &mut free_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(free_rent.dict_get("cost").and_then(|v| v.as_f64()), Some(0.0));
    let free_notifs = ctx.drain_notifications_for_char(free_char_id);
    assert!(!free_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(!free_notifs.iter().any(|n| n.event == "OnBillReceived"));
    assert!(free_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(free_notifs.iter().any(|n| n.event == "OnOfficeRentalChange"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&free_corp_id].division_balance(1), IskCents::from_isk(0));
        assert!(!snap.corp_wallet_journals.contains_key(&(free_corp_id, 1)));
        assert!(snap.corp_governance[&free_corp_id].offices.contains_key(&struct_id));
    });

    // 4. Setting SERVICE_OFFICES = false or SETTING_CORP_RENT_OFFICE = false disables office rental.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.access_profiles
            .get_mut(&struct_id)
            .unwrap()
            .general_settings
            .insert(SERVICE_OFFICES, false);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    // 5. Re-enable with station_office_rental_costs = 0.0 (no ACL rule fees) AND verify >24
    // offices succeed on Upwell Citadels while NPC stations cap at 24.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.get_mut(&struct_id).unwrap();
        prof.general_settings.remove(&SERVICE_OFFICES);
        prof.service_acl_rules.remove(&SETTING_CORP_RENT_OFFICE);
        prof.general_settings.insert(SETTING_CORP_RENT_OFFICE, true);
        save_ext_state(snap, &ext);
        snap.station_office_rental_costs.insert(struct_id, 0.0);

        for i in 0..25_i64 {
            let iid = 950_000 + i;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 27,
                    owner_id: 98_950_000 + i,
                    location_id: struct_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetPriceQuote", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(struct_id)], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 24
    );
    let owner_rent = handle(&ctx, &mut owner_sess, "officeManager", "RentOffice", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner_rent.dict_get("cost").and_then(|v| v.as_f64()), Some(0.0));

    // Confirm NPC station allows 17th..24th offices and caps at 24.
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..20_i64 {
            let iid = 951_000 + i;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 27,
                    owner_id: 98_960_000 + i,
                    location_id: npc_station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(npc_station_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(4)
    );
    ctx.db.with_mut_snapshot(|snap| {
        for i in 20..24_i64 {
            let iid = 951_000 + i;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 27,
                    owner_id: 98_960_000 + i,
                    location_id: npc_station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        handle(&ctx, &mut owner_sess, "officeManager", "GetEmptyOfficeCount", &[PyRep::Integer(npc_station_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
}
