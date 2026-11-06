//! Additional branch-coverage and mutation-killing unit tests for `character_lifecycle.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::character_lifecycle::{
    handle, is_character_delete_pending, load_store, save_store, LifecycleStore,
    BIOMASS_DELAY_SECS, CHARACTER_LIFECYCLE_STORE_KEY,
};
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::snapshot_ext::{CharacterScopedRolesRecord, CorpMemberRoleAssignment};
use eve_db::{
    corp_roles, ContractRecord, CorporationRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MarketOrderMeta, MarketOrderRecord, PlanetaryColonyLayoutRecord,
    PlanetaryColonyRecord, UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn call_unbound(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "charUnboundMgr", method, args, kwargs)
}

fn call_char_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "charMgr", method, args, kwargs)
}

fn call_other_svc(ctx: &ServerContext, session: &mut SessionState) -> Option<Result<PyRep, BridgeRpcError>> {
    let svc = "other";
    let m = "noop";
    handle(ctx, session, svc, m, &[], &HashMap::new())
}

#[test]
fn unbound_mgr_slots_locks_starter_systems_and_resculpt_branches_kill_mutations() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let mut sess = auth_session(uid, cid, corp_id, 30_000_142);

    assert!(call_other_svc(&ctx, &mut sess).is_none());
    let mut unauth = SessionState::default();
    assert!(call_unbound(&ctx, &mut unauth, "GetNumCharacters", &[], &kw).unwrap().is_err());
    assert!(call_unbound(&ctx, &mut sess, "UnknownMethod", &[], &kw).is_none());

    // Store helpers with invalid JSON fallback and roundtrip
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(CHARACTER_LIFECYCLE_STORE_KEY, "{bad-json".into());
        assert!(load_store(snap).delete_prepared_unix.is_empty());
        let mut st = LifecycleStore::default();
        st.delete_prepared_unix.insert(cid, 12345);
        save_store(snap, &st);
        assert!(is_character_delete_pending(snap, cid));
        st.delete_prepared_unix.remove(&cid);
        save_store(snap, &st);
        assert!(!is_character_delete_pending(snap, cid));
    });

    // CreateCharacterWithDoll: valid pass-through -> None, banned word -> UserError, >= 3 chars -> MaxCharacterSlotsReached
    assert!(call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &[PyRep::String("ValidPilot".into())], &kw).is_none());
    let banned_err = call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &[PyRep::String("badword_pilot".into())], &kw).unwrap().unwrap_err();
    assert_eq!(banned_err.code, "UserError");

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&cid).unwrap().clone();
        for extra_id in [90_700_001_i64, 90_700_002_i64, 90_700_003_i64] {
            let mut ch = base.clone();
            ch.character_id = extra_id;
            ch.user_id = uid;
            snap.characters.insert(extra_id, ch);
        }
    });
    let max_err = call_unbound(&ctx, &mut sess, "CreateCharacterWithDoll", &[PyRep::String("ValidPilot".into())], &kw).unwrap().unwrap_err();
    assert_eq!(max_err.code, "MaxCharacterSlotsReached");
    ctx.db.with_mut_snapshot(|snap| {
        for extra_id in [90_700_001_i64, 90_700_002_i64, 90_700_003_i64] {
            snap.characters.remove(&extra_id);
        }
    });

    // SelectCharacterID: missing arg -> None, unlocked -> None, character_lock blob -> CharacterLocked
    assert!(call_unbound(&ctx, &mut sess, "SelectCharacterID", &[], &kw).is_none());
    assert!(call_unbound(&ctx, &mut sess, "SelectCharacterID", &[PyRep::Integer(cid)], &kw).is_none());
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("character_lock:{cid}"), "banned_test".into());
    });
    let lock_err = call_unbound(&ctx, &mut sess, "SelectCharacterID", &[PyRep::Integer(cid)], &kw).unwrap().unwrap_err();
    assert_eq!(lock_err.code, "CharacterLocked");
    assert!(lock_err.message.contains("banned_test"));
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove(&format!("character_lock:{cid}"));
    });

    // GetQAStarterSystemIDs across all 4 fallback branches
    sess.solar_system_id = Some(30_002_510);
    let l1 = call_unbound(&ctx, &mut sess, "GetQAStarterSystemIDs", &[], &kw).unwrap().unwrap();
    assert_eq!(l1, PyRep::List(vec![PyRep::Integer(30_002_510), PyRep::Integer(30_000_144), PyRep::Integer(30_002_187), PyRep::Integer(30_002_510)]));
    sess.solar_system_id = None;
    let l2 = call_unbound(&ctx, &mut sess, "GetQAStarterSystemIDs", &[], &kw).unwrap().unwrap();
    assert_eq!(l2.as_list().unwrap()[0], PyRep::Integer(30_000_142));
    sess.character_id = None;
    let l3 = call_unbound(&ctx, &mut sess, "GetQAStarterSystemIDs", &[], &kw).unwrap().unwrap();
    assert!(l3.as_list().unwrap()[0].as_i64().unwrap() > 0);

    let mut empty_user_sess = auth_session(999_999, cid, corp_id, 0);
    empty_user_sess.solar_system_id = None;
    empty_user_sess.character_id = None;
    let l4 = call_unbound(&ctx, &mut empty_user_sess, "GetQAStarterSystemIDs", &[], &kw).unwrap().unwrap();
    assert_eq!(l4.as_list().unwrap()[0], PyRep::Integer(0));
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);

    // owned_char_arg error branches + UpdateCharacterBloodline resculpt branches
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterGender", &[], &kw).unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterGender", &[PyRep::Integer(999_999_999)], &kw).unwrap().unwrap_err().code, "CharacterOwnershipMismatch");

    let (&bl_a, binfo_a) = ctx.sde.bloodlines.iter().next().unwrap();
    let (&bl_diff_race, _) = ctx.sde.bloodlines.iter().find(|(_, b)| b.race_id != binfo_a.race_id).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.race_id = binfo_a.race_id;
            ch.bloodline_id = bl_a;
        }
        snap.service_state_blobs.remove(&format!("resculpt_pending:{cid}"));
        snap.service_state_blobs.remove(&format!("account_resculpt_tokens:{uid}"));
    });
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterBloodline", &[PyRep::Integer(cid), PyRep::Integer(bl_diff_race as i64)], &kw).unwrap().unwrap_err().code, "ResculptRequired");
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterBloodline", &[PyRep::Integer(cid), PyRep::Integer(bl_a as i64)], &kw).unwrap().unwrap(), PyRep::Bool(true));

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("account_resculpt_tokens:{uid}"), "1".into());
    });
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterBloodline", &[PyRep::Integer(cid), PyRep::Integer(bl_diff_race as i64)], &kw).unwrap().unwrap(), PyRep::Bool(true));

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("account_resculpt_tokens:{uid}"), "0".into());
    });
    sess.role = service_roles::ROLE_GML;
    assert_eq!(call_unbound(&ctx, &mut sess, "UpdateCharacterBloodline", &[PyRep::Integer(cid), PyRep::Integer(bl_a as i64)], &kw).unwrap().unwrap(), PyRep::Bool(true));
    sess.role = 0;
}

#[test]
fn biomass_delete_contracts_orders_jobs_colonies_and_corp_asset_rules_kill_mutations() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    ctx.set_clock(clock.clone());
    let kw = HashMap::new();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let (sac1, sac2, player_corp) = (90_888_101_i64, 90_888_102_i64, 98_888_101_i64);
    let (office_st_1, office_st_2, remote_st) = (60_003_760_i64, 60_004_588_i64, 60_008_494_i64);

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&cid).unwrap().clone();
        if let Some(other) = snap.characters.get_mut(&other_cid) {
            other.balance = IskCents(1_000_00);
        }
        let mut c1 = base.clone();
        c1.character_id = sac1;
        c1.user_id = uid;
        c1.name = "SacPilotOne".into();
        c1.corporation_id = player_corp;
        c1.station_id = None;
        c1.home_station_id = office_st_1;
        c1.balance = IskCents(20_000_00);
        snap.characters.insert(sac1, c1);

        let mut c2 = base;
        c2.character_id = sac2;
        c2.user_id = uid;
        c2.name = "SacPilotTwo".into();
        c2.corporation_id = player_corp;
        c2.station_id = Some(office_st_1);
        c2.home_station_id = office_st_1;
        c2.balance = IskCents(5_000_00);
        snap.characters.insert(sac2, c2);

        snap.corporations.insert(
            player_corp,
            CorporationRecord {
                corporation_id: player_corp,
                name: "InheritCorp".into(),
                ceo_id: cid,
                hq_station_id: Some(office_st_1),
                members: HashSet::from([cid, sac1, sac2]),
                ..Default::default()
            },
        );
        for (iid, st) in [(920_001, office_st_1), (920_002, office_st_2)] {
            snap.items.insert(iid, InventoryItemRecord { item_id: iid, type_id: 27, owner_id: player_corp, location_id: st, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default() });
        }
        snap.items.insert(920_010, InventoryItemRecord { item_id: 920_010, type_id: 3293, owner_id: sac1, location_id: office_st_2, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(920_011, InventoryItemRecord { item_id: 920_011, type_id: 34, owner_id: sac1, location_id: 920_010, flag: InventoryFlag::Cargo, quantity: 50, ..Default::default() });
        snap.items.insert(920_012, InventoryItemRecord { item_id: 920_012, type_id: 35, owner_id: sac1, location_id: remote_st, flag: InventoryFlag::Hangar, quantity: 70, ..Default::default() });

        snap.market_orders.insert(920_020, MarketOrderRecord { order_id: 920_020, owner_id: sac1, station_id: office_st_2, region_id: 10_000_002, type_id: 36, is_bid: false, price: IskCents(100), vol_entered: 25, vol_remaining: 25, min_volume: 1, is_npc: false, escrow: IskCents(0) });
        snap.market_order_meta.insert(920_020, MarketOrderMeta { issued_unix: 1_700_000_000, duration_days: 90, range: "region".into(), issued_by: None, wallet_division: None, is_corporation: false, state: None });
        snap.market_orders.insert(920_021, MarketOrderRecord { order_id: 920_021, owner_id: sac1, station_id: office_st_1, region_id: 10_000_002, type_id: 34, is_bid: false, price: IskCents(100), vol_entered: 10, vol_remaining: 10, min_volume: 1, is_npc: true, escrow: IskCents(0) });

        snap.items.insert(920_030, InventoryItemRecord { item_id: 920_030, type_id: 37, owner_id: 0, location_id: office_st_1, flag: InventoryFlag::Locked, quantity: 10, ..Default::default() });
        snap.contracts.insert(920_031, ContractRecord { contract_id: 920_031, issuer_id: sac1, status: "Outstanding".into(), current_bidder_id: Some(other_cid), current_bid: IskCents(2_000_00), reward: IskCents(3_000_00), start_station_id: office_st_1, item_ids: vec![920_030], ..Default::default() });

        snap.items.insert(920_032, InventoryItemRecord { item_id: 920_032, type_id: 38, owner_id: 0, location_id: office_st_1, flag: InventoryFlag::Locked, quantity: 5, ..Default::default() });
        snap.contracts.insert(920_033, ContractRecord { contract_id: 920_033, issuer_id: other_cid, assignee_id: sac1, status: "Outstanding".into(), current_bidder_id: Some(sac1), current_bid: IskCents(4_000_00), reward: IskCents(1_500_00), start_station_id: office_st_1, item_ids: vec![920_032], ..Default::default() });
        snap.contracts.insert(920_034, ContractRecord { contract_id: 920_034, issuer_id: other_cid, assignee_id: 0, status: "Outstanding".into(), current_bidder_id: Some(sac1), current_bid: IskCents(6_000_00), start_station_id: office_st_1, ..Default::default() });
        snap.contracts.insert(920_035, ContractRecord { contract_id: 920_035, issuer_id: other_cid, acceptor_id: Some(sac1), status: "InProgress".into(), start_station_id: office_st_1, ..Default::default() });

        snap.items.insert(920_040, InventoryItemRecord { item_id: 920_040, type_id: 683, owner_id: sac1, location_id: office_st_1, flag: InventoryFlag::Locked, quantity: 1, singleton: true, ..Default::default() });
        snap.industry_jobs.insert(920_041, IndustryJobRecord { job_id: 920_041, installer_id: sac1, facility_id: office_st_1, blueprint_item_id: 920_040, blueprint_type_id: 683, product_type_id: 587, activity_id: 1, runs: 1, start_unix: 1_700_000_000, duration_seconds: 3_600, status: "active".into(), ..Default::default() });
        snap.planetary_colonies.insert((sac1, 40_000_001), PlanetaryColonyRecord { owner_id: sac1, planet_id: 40_000_001, solar_system_id: 30_000_142, command_center_level: 1, pins_count: 1, extracted_commodities: HashMap::new(), last_extracted_unix: 1_700_000_000 });
        snap.planetary_colony_layouts.insert((sac1, 40_000_001), PlanetaryColonyLayoutRecord::default());
    });

    let mut sess = auth_session(uid, sac1, player_corp, 30_000_142);
    sess.fleet_id = Some(555);
    assert_eq!(call_unbound(&ctx, &mut sess, "CancelCharacterDeletePrepare", &[PyRep::Integer(sac1)], &kw).unwrap().unwrap(), PyRep::Bool(false));

    let expected_ready = 1_700_000_000 + BIOMASS_DELAY_SECS;
    assert_eq!(call_unbound(&ctx, &mut sess, "PrepareCharacterForDelete", &[PyRep::Integer(sac1)], &kw).unwrap().unwrap(), PyRep::Integer(expected_ready as i64));
    clock.advance_secs(BIOMASS_DELAY_SECS - 1);
    assert_eq!(call_unbound(&ctx, &mut sess, "DeleteCharacter", &[PyRep::Integer(sac1)], &kw).unwrap().unwrap_err().code, "DeleteTimerNotExpired");
    clock.advance_secs(1);
    let del1 = call_unbound(&ctx, &mut sess, "DeleteCharacter", &[PyRep::Integer(sac1)], &kw).unwrap().unwrap();
    assert_eq!(del1.dict_get("deleted"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.character_id, None);
    assert_eq!(sess.fleet_id, None);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&920_010].location_id, office_st_2);
        assert_eq!(snap.items[&920_011].location_id, 920_010);
        assert_eq!(snap.items[&920_012].location_id, office_st_1);
        assert_eq!(snap.items[&920_012].flag, InventoryFlag::CorpSAG1);
        assert!(!snap.market_orders.contains_key(&920_020));
        assert!(snap.market_orders.contains_key(&920_021));
        assert!(!snap.contracts.contains_key(&920_031));
        assert_eq!(snap.contracts[&920_033].status, "Rejected");
        assert_eq!(snap.contracts[&920_034].current_bidder_id, None);
        assert!(!snap.contracts.contains_key(&920_035));
        assert_eq!(snap.characters[&other_cid].balance, IskCents(4_500_00));
        assert!(!snap.industry_jobs.contains_key(&920_041));
        assert!(!snap.planetary_colonies.contains_key(&(sac1, 40_000_001)));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&920_001);
        snap.items.remove(&920_002);
    });
    let mut gm_sess = auth_session(uid, cid, player_corp, 30_000_142);
    gm_sess.role = service_roles::ROLE_GML;
    assert_eq!(call_unbound(&ctx, &mut gm_sess, "DeleteCharacter", &[PyRep::Integer(sac2)], &kw).unwrap().unwrap().dict_get("deleted"), Some(&PyRep::Bool(true)));
}

#[test]
fn char_mgr_home_station_scoped_roles_public_info_and_cohorts_kill_mutations() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));
    let kw = HashMap::new();
    let (uid, cid, other_cid, corp_id) = (1_u64, 90_000_001_i64, 90_000_002_i64, 98_000_001_i64);

    let mut unauth = SessionState::default();
    for m in [
        "GetCharacterCreationDate", "GetHomeStationRow", "GetOrganizationInfoForCharacters",
        "GetPrivateInfoOnCorpChange", "GetPublicInfo", "GetRecentShipKillsAndLosses",
        "SetActivityStatus", "GetCohortsForCharacter",
    ] {
        assert!(call_char_mgr(&ctx, &mut unauth, m, &[], &kw).unwrap().is_err());
    }
    let mut sess = auth_session(uid, cid, corp_id, 30_000_142);
    assert!(call_char_mgr(&ctx, &mut sess, "UnknownMethod", &[], &kw).is_none());
    assert!(call_char_mgr(&ctx, &mut sess, "GetCharacterCreationDate", &[PyRep::Integer(999_999_999)], &kw).unwrap().unwrap().as_i64().unwrap() > 0);

    let struct_id = 1_045_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id, type_id: 35832, owner_corp_id: corp_id,
                solar_system_id: 30_000_142, name: "TestCitadel".into(), state: "Armor_Vulnerable".into(),
                position: [0.0, 0.0, 0.0], ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) { ch.home_station_id = struct_id; }
    });
    let hs_struct = call_char_mgr(&ctx, &mut sess, "GetHomeStationRow", &[], &kw).unwrap().unwrap();
    assert_eq!(hs_struct.dict_get("stationID"), Some(&PyRep::Integer(struct_id)));
    assert_eq!(hs_struct.dict_get("typeID"), Some(&PyRep::Integer(35832)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) { ch.home_station_id = 0; ch.school_id = 0; }
    });
    assert!(call_char_mgr(&ctx, &mut sess, "GetHomeStationRow", &[], &kw).unwrap().unwrap().dict_get("stationID").unwrap().as_i64().unwrap() > 0);
    assert_eq!(call_char_mgr(&ctx, &mut sess, "GetOrganizationInfoForCharacters", &[PyRep::Integer(999_999_999)], &kw).unwrap().unwrap().as_dict().map(|d| d.len()), Some(1));

    let all_role_names: Vec<String> = [
        "Director", "Personnel_Manager", "Accountant", "Security_Officer", "Factory_Manager",
        "Station_Manager", "Auditor", "Diplomat", "Config_Equipment", "Equipment_Config",
        "Starbase_Config", "Starbase_Operator", "Junior_Accountant", "Trader",
        "Contract_Manager", "Project_Manager", "Unrecognized_Role",
    ].into_iter().map(String::from).collect();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&cid) { c1.corporation_id = corp_id; }
        if let Some(c2) = snap.characters.get_mut(&other_cid) {
            c2.corporation_id = corp_id;
            c2.war_faction_id = Some(500_001);
            c2.standings.insert(500_001, 4.5);
        }
        snap.corporations.insert(corp_id, CorporationRecord { corporation_id: corp_id, ceo_id: 0, ..Default::default() });
        snap.corp_role_assignments.insert((corp_id, cid), CorpMemberRoleAssignment { character_id: cid, roles: corp_roles::ROLE_PERSONNEL_MANAGER, ..Default::default() });
        snap.char_scoped_roles.insert(other_cid, CharacterScopedRolesRecord { roles: all_role_names, ..Default::default() });
        snap.employment_history.insert(other_cid, vec![(corp_id, 1_650_000_000, None)]);
    });
    let priv_info = call_char_mgr(&ctx, &mut sess, "GetPrivateInfoOnCorpChange", &[PyRep::Integer(other_cid)], &kw).unwrap().unwrap();
    let roles_bits = priv_info.dict_get("roles").unwrap().as_i64().unwrap() as u64;
    assert_ne!(roles_bits & corp_roles::ROLE_DIRECTOR, 0);
    assert_ne!(roles_bits & corp_roles::ROLE_PROJECT_MANAGER, 0);

    let pub_wf = call_char_mgr(&ctx, &mut sess, "GetPublicInfo", &[PyRep::Integer(other_cid)], &kw).unwrap().unwrap();
    assert_eq!(pub_wf.dict_get("warFactionID"), Some(&PyRep::Integer(500_001)));
    let pub_unk = call_char_mgr(&ctx, &mut sess, "GetPublicInfo", &[PyRep::Integer(999_999_999)], &kw).unwrap().unwrap();
    assert_eq!(pub_unk.dict_get("characterName"), Some(&PyRep::String("Unknown".into())));

    let _ = call_char_mgr(&ctx, &mut sess, "SetActivityStatus", &[PyRep::Integer(2)], &kw);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].activity_status.clone()), Some("2".into()));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) { ch.create_date_unix = 1_700_000_000 - 86_400; }
    });
    let new_cohorts = call_char_mgr(&ctx, &mut sess, "GetCohortsForCharacter", &[], &kw).unwrap().unwrap();
    assert!(new_cohorts.as_list().unwrap().contains(&PyRep::String("cohort_new_player".into())));

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("char_cohorts:{cid}"), r#"["alpha","beta"]"#.into());
    });
    let blob_cohorts = call_char_mgr(&ctx, &mut sess, "GetCohortsForCharacter", &[], &kw).unwrap().unwrap();
    assert_eq!(blob_cohorts, PyRep::List(vec![PyRep::String("alpha".into()), PyRep::String("beta".into())]));
}
