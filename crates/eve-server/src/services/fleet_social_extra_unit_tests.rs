//! Additional branch-coverage and mutation-killing unit tests for `fleet_social.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::fleet_social_fleets::FleetAdvertState;
use crate::services::fleet_social::{
    get_effective_corp_roles, get_trashed_mails, handle, handle_get_init_state, handle_get_wings,
    is_in_fleet_boost_scope, parse_dest_id, sweep_expired_bookmarks_and_notify,
};
use eve_db::{
    corp_roles, BookmarkRecord, CorporationRecord, FleetMemberRecord, FleetRecord, FleetRole,
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        solar_system_id: Some(30_000_142),
        corp_role: 0,
        ..Default::default()
    }
}

async fn call_office(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "officeManager", method, args, &HashMap::new()).await
}

async fn call_fleet_obj(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, "fleetObjectHandler", method, args, &HashMap::new()).await
}

#[test]
fn boost_scope_none_wings_trashed_mails_and_shared_bookmark_sweep_kill_mutations() {
    let mut fleet = FleetRecord {
        fleet_id: 10,
        boss_id: 100,
        members: vec![100, 101, 102, 103],
        wings: vec![1],
        squads: vec![10],
        free_move: false,
        is_registered: false,
        wing_details: HashMap::new(),
        squad_details: HashMap::new(),
        member_details: HashMap::new(),
        invites: HashSet::new(),
        motd: String::new(),
    };
    let mk_member = |cid: i64, role: FleetRole, wing: Option<i64>, squad: Option<i64>| {
        FleetMemberRecord {
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
        }
    };
    fleet.member_details.insert(101, mk_member(101, FleetRole::WingCommander, None, None));
    fleet.member_details.insert(102, mk_member(102, FleetRole::SquadCommander, Some(1), None));
    fleet.member_details.insert(103, mk_member(103, FleetRole::SquadMember, None, None));
    assert!(!is_in_fleet_boost_scope(&fleet, 101, 103));
    assert!(!is_in_fleet_boost_scope(&fleet, 102, 103));

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    ctx.social_extras.lock().unwrap().trashed_mails.insert((90_000_001, 111));
    ctx.db.with_mut_snapshot(|snap| {
        snap.trashed_mails.insert((90_000_001, 222));
        snap.bookmarks.insert(
            601,
            BookmarkRecord {
                bookmark_id: 601,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                x: 10.0,
                y: 20.0,
                z: 30.0,
                item_id: None,
                label: "shared_no_folder".into(),
                memo: String::new(),
                folder_id: None,
                is_shared: true,
            },
        );
        snap.bookmark_expiries.insert(601, 1_700_000_000);
    });
    let trashed = get_trashed_mails(&ctx);
    assert!(trashed.contains(&(90_000_001, 111)));
    assert!(trashed.contains(&(90_000_001, 222)));

    let swept = sweep_expired_bookmarks_and_notify(&ctx, 1_700_000_000);
    assert_eq!(swept, vec![601]);
    assert_eq!(parse_dest_id(&[PyRep::String("bad".into())], &HashMap::new()), None);
}

#[tokio::test]
async fn office_manager_fulcrum_upwell_capacity_roles_and_wallet_branches_kill_mutations() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_100_001_i64;
    let station_id = *ctx.sde.stations.keys().find(|&&id| id != 60_015_187).unwrap_or(&60_003_760);
    let fulcrum_id = 60_015_187_i64;
    let struct_id = 1_045_000_000_100_i64;

    let mut unauth = SessionState::default();
    assert!(call_office(&ctx, &mut unauth, "Bind", &[]).await.unwrap().is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "OfficeTestCorp".into(),
                ticker: "OFFC".into(),
                ceo_id: other_char,
                master_wallet: IskCents::from_isk(500),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "OfficeCitadel".into(),
                state: "Armor_Vulnerable".into(),
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id));
    assert_eq!(call_office(&ctx, &mut sess, "Bind", &[]).await.unwrap().unwrap(), PyRep::None);
    assert_eq!(call_office(&ctx, &mut sess, "SetSessionCheck", &[]).await.unwrap().unwrap(), PyRep::None);

    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(station_id + 1)]).await.unwrap().unwrap_err().code, "NotDockedAtStation");
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(station_id)]).await.unwrap().unwrap_err().code, "CorpRoleDenied");
    assert_eq!(call_office(&ctx, &mut sess, "UnrentOffice", &[PyRep::Integer(station_id)]).await.unwrap().unwrap_err().code, "CorpRoleDenied");

    sess.corp_role = corp_roles::ROLE_RENT_OFFICE;
    let (is_ceo, _) = get_effective_corp_roles(&ctx, corp_id, char_id);
    assert!(!is_ceo);

    sess.station_id = Some(999_999_999);
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(999_999_999)]).await.unwrap().unwrap_err().code, "StationNotFound");
    sess.station_id = None;
    sess.structure_id = Some(1_999_999_999_999);
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(1_999_999_999_999)]).await.unwrap().unwrap_err().code, "StationNotFound");

    sess.structure_id = Some(struct_id);
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(struct_id)]).await.unwrap().unwrap_err().code, "NotEnoughISK");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().master_wallet = IskCents::from_isk(100_000);
    });
    let rent_struct = call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(struct_id)]).await.unwrap().unwrap();
    assert_eq!(rent_struct.dict_get("stationID"), Some(&PyRep::Integer(struct_id)));
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(struct_id)]).await.unwrap().unwrap_err().code, "OfficeAlreadyRented");

    if ctx.sde.stations.contains_key(&fulcrum_id) {
        sess.structure_id = None;
        sess.station_id = Some(fulcrum_id);
        assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(fulcrum_id)]).await.unwrap().unwrap_err().code, "FulcrumOfficeRequiresPirateEnlistment");
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.corp_war_factions.insert(corp_id, 500_010);
        });
        assert!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(fulcrum_id)]).await.unwrap().is_ok());
    }

    sess.structure_id = None;
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..16 {
            let iid = 930_000 + i;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 27,
                    owner_id: 98_900_000 + i,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(call_office(&ctx, &mut sess, "GetPriceQuote", &[PyRep::Integer(station_id)]).await.unwrap().unwrap(), PyRep::Real(90_000.0));
    assert_eq!(call_office(&ctx, &mut sess, "GetEmptyOfficeCount", &[PyRep::Integer(station_id)]).await.unwrap().unwrap(), PyRep::Integer(0));
    assert_eq!(call_office(&ctx, &mut sess, "RentOffice", &[PyRep::Integer(station_id)]).await.unwrap().unwrap_err().code, "StationOfficesFull");
    assert_eq!(call_office(&ctx, &mut sess, "GetMyCorporationsOffice", &[PyRep::Integer(station_id)]).await.unwrap().unwrap_err().code, "NoOfficeAtStation");
    assert_eq!(call_office(&ctx, &mut sess, "UnrentOffice", &[PyRep::Integer(station_id)]).await.unwrap().unwrap_err().code, "NoOfficeAtStation");
}

#[tokio::test]
async fn mass_move_members_make_leader_get_wings_and_init_state_error_paths_kill_mutations() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;

    let mut no_fleet_sess = make_session(90_000_001, 98_000_001, None);
    assert_eq!(call_fleet_obj(&ctx, &mut no_fleet_sess, "MassMoveMembers", &[]).await.unwrap().unwrap_err().code, "NotInFleet");
    assert_eq!(call_fleet_obj(&ctx, &mut no_fleet_sess, "MakeLeader", &[PyRep::Integer(90_000_002)]).await.unwrap().unwrap_err().code, "NotInFleet");
    assert_eq!(handle_get_wings(&ctx, &mut no_fleet_sess).unwrap().unwrap_err().code, "NotInFleet");
    assert_eq!(handle_get_init_state(&ctx, &mut no_fleet_sess).unwrap().unwrap_err().code, "NotInFleet");

    let mut member_sess = make_session(90_000_003, 98_000_001, None);
    member_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        call_fleet_obj(&ctx, &mut member_sess, "MassMoveMembers", &[PyRep::Integer(90_000_003), PyRep::Integer(710_001), PyRep::Integer(720_001)]).await.unwrap().unwrap_err().code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        call_fleet_obj(&ctx, &mut boss_sess, "MassMoveMembers", &[PyRep::Integer(90_000_003), PyRep::Integer(0), PyRep::Integer(720_001)]).await.unwrap().unwrap_err().code,
        "WingNotFound"
    );
    assert_eq!(
        call_fleet_obj(&ctx, &mut boss_sess, "MassMoveMembers", &[PyRep::Integer(90_000_003), PyRep::Integer(710_001), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "SquadNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fl) = snap.fleets.get_mut(&fleet_id) {
            if let Some(m) = fl.member_details.get_mut(&90_000_002) {
                m.role = FleetRole::WingCommander;
                m.wing_id = Some(710_001);
            }
            if let Some(w) = fl.wing_details.get_mut(&710_001) {
                w.commander_id = Some(90_000_002);
            }
        }
    });
    let mv = call_fleet_obj(&ctx, &mut boss_sess, "MassMoveMembers", &[PyRep::Integer(90_000_002), PyRep::Integer(710_001), PyRep::Integer(720_001)]).await.unwrap().unwrap();
    assert_eq!(mv.dict_get("movedCount"), Some(&PyRep::Integer(1)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets[&fleet_id].wing_details[&710_001].commander_id, None);
    });

    assert_eq!(call_fleet_obj(&ctx, &mut boss_sess, "MakeLeader", &[]).await.unwrap().unwrap_err().code, "InvalidArgument");
    ctx.fleet_extras.lock().unwrap().disconnected_members.insert(90_000_002, 1_700_000_000);
    assert_eq!(call_fleet_obj(&ctx, &mut boss_sess, "MakeLeader", &[PyRep::Integer(90_000_002)]).await.unwrap().unwrap_err().code, "TargetMemberOffline");
    ctx.fleet_extras.lock().unwrap().disconnected_members.remove(&90_000_002);

    assert_eq!(call_fleet_obj(&ctx, &mut member_sess, "MakeLeader", &[PyRep::Integer(90_000_002)]).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(call_fleet_obj(&ctx, &mut boss_sess, "MakeLeader", &[PyRep::Integer(999_999_999)]).await.unwrap().unwrap_err().code, "MemberNotFound");

    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.adverts.insert(
            fleet_id,
            FleetAdvertState {
                fleet_id,
                leader_id: 90_000_001,
                solar_system_id: 30_000_142,
                fleet_name: "StrikeFleet".into(),
                description: "desc".into(),
                invite_scope: 1,
                min_standing: 0.0,
                min_security: -10.0,
                auto_join: true,
                allowed_entities: HashSet::new(),
                _created_unix: 1_700_000_000,
            },
        );
        ex.fleet_names.insert(fleet_id, "CustomTaskforce".into());
        ex.max_sizes.insert(fleet_id, 128);
        ex.auto_join_squads.insert(fleet_id, Some(720_001));
        ex.target_tags.entry(fleet_id).or_default().insert(5001, "A".into());
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(3348, TrainedSkillRecord { type_id: 3348, level: 5, skill_points: 256_000.0 });
    });

    let init = handle_get_init_state(&ctx, &mut boss_sess).unwrap().unwrap();
    assert_eq!(init.dict_get("fleetName"), Some(&PyRep::String("CustomTaskforce".into())));
    assert_eq!(init.dict_get("maxSize"), Some(&PyRep::Integer(128)));

    let _ = call_fleet_obj(&ctx, &mut boss_sess, "MakeLeader", &[PyRep::Integer(90_000_002)]).await.unwrap().unwrap();
    assert_eq!(ctx.fleet_extras.lock().unwrap().adverts[&fleet_id].leader_id, 90_000_002);

    let mut stale_sess = make_session(90_000_001, 98_000_001, None);
    stale_sess.fleet_id = Some(999_999);
    assert_eq!(handle_get_wings(&ctx, &mut stale_sess).unwrap().unwrap_err().code, "FleetNotFound");
    assert_eq!(stale_sess.fleet_id, None);

    stale_sess.fleet_id = Some(fleet_id);
    stale_sess.character_id = Some(90_000_099);
    assert_eq!(handle_get_init_state(&ctx, &mut stale_sess).unwrap().unwrap_err().code, "NotInFleet");
    assert_eq!(stale_sess.fleet_id, None);
}
