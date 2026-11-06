//! Comprehensive branch-coverage and mutation-killing tests for `fleet_social_finder_tactical.rs`.

use crate::services::fleet_social::fleet_social_finder_tactical::{
    clear_destroyed_target_tag, ensure_fleet_formations_loaded, ensure_fleet_setups_loaded,
    formation_name, handle_fleet_watchlist_ops, sync_fleet_formations_to_db,
    sync_fleet_setups_to_db,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::fleet_social_fleets::FleetSetupRecord;
use crate::services::fleet_social::handle;
use eve_db::{FleetRole, InventoryFlag, InventoryItemRecord, SocialContactRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

fn make_session(char_id: i64, fleet_id: Option<i64>, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.fleet_id = fleet_id;
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn setups_and_formations_persistence_and_guard_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_000);
    for (id, expected) in [(-1, "Point"), (0, "Point"), (1, "Plane"), (2, "Sphere"), (3, "Arrow"), (4, "Diamond"), (5, "Relative"), (6, "Point")] {
        assert_eq!(formation_name(id), expected);
    }

    let rec = FleetSetupRecord {
        setup_name: "PersistedSetup".into(),
        motd: "SavedMOTD".into(),
        is_free_move: true,
        max_size: 50,
        default_squad_id: Some(1),
        wings: vec![("Alpha".into(), vec!["Sq1".into(), "Sq2".into()])],
    };
    ctx.fleet_extras.lock().unwrap().saved_setups.entry(90_000_001).or_default().insert("PersistedSetup".into(), rec);
    sync_fleet_setups_to_db(&ctx);

    ctx.fleet_extras.lock().unwrap().saved_setups.get_mut(&90_000_001).unwrap().get_mut("PersistedSetup").unwrap().max_size = 99;
    ensure_fleet_setups_loaded(&ctx);
    assert_eq!(ctx.fleet_extras.lock().unwrap().saved_setups[&90_000_001]["PersistedSetup"].max_size, 99);

    ctx.fleet_extras.lock().unwrap().saved_setups.clear();
    ensure_fleet_setups_loaded(&ctx);
    let loaded = ctx.fleet_extras.lock().unwrap().saved_setups[&90_000_001]["PersistedSetup"].clone();
    assert_eq!(loaded.motd, "SavedMOTD");
    assert_eq!(loaded.max_size, 50);
    assert!(loaded.is_free_move);

    ctx.fleet_extras.lock().unwrap().stored_formations.insert(90_000_001, (4, 6_000.0, 18_000.0));
    sync_fleet_formations_to_db(&ctx);
    ctx.fleet_extras.lock().unwrap().stored_formations.insert(90_000_001, (2, 1_000.0, 2_000.0));
    ensure_fleet_formations_loaded(&ctx);
    assert_eq!(ctx.fleet_extras.lock().unwrap().stored_formations[&90_000_001].0, 2);
    ctx.fleet_extras.lock().unwrap().stored_formations.clear();
    ensure_fleet_formations_loaded(&ctx);
    assert_eq!(ctx.fleet_extras.lock().unwrap().stored_formations[&90_000_001], (4, 6_000.0, 18_000.0));
}

#[tokio::test]
async fn watchlist_hp_ratios_broadcast_lock_and_capacity_limits() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_100);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;

    let mut anon = SessionState::new(999);
    assert!(handle_fleet_watchlist_ops(&ctx, &mut anon, "GetWatchlist", &[]).await.unwrap().is_err());
    let mut no_fleet = make_session(90_000_001, None, Some(30_000_142), Some(100_000_001));
    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut no_fleet, "GetWatchlist", &[]).await.unwrap().unwrap_err().code, "NotInFleet");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.characters.get_mut(&90_000_002) {
            c2.active_ship_id = 100_000_002;
            c2.solar_system_id = 30_000_142;
            c2.station_id = None;
            c2.structure_id = None;
        }
        if let Some(c3) = snap.characters.get_mut(&90_000_003) {
            c3.active_ship_id = 100_000_003;
            c3.solar_system_id = 30_000_143;
            c3.station_id = None;
            c3.structure_id = None;
        }
        snap.items.insert(
            100_000_003,
            InventoryItemRecord {
                item_id: 100_000_003, type_id: 587, owner_id: 90_000_003, location_id: 30_000_143,
                flag: InventoryFlag::None, quantity: 1, singleton: true,
                dynamic_attributes: HashMap::from([(264, 100.0), (266, 50.0), (3, 25.0)]),
                version: 1, ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys.spawn_ball_with_combat(
        Ball {
            id: 100_000_002, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 250.0, max_shield_hp: 500.0, armor_hp: 300.0, max_armor_hp: 400.0,
            structure_hp: 150.0, max_structure_hp: 300.0, ..Default::default()
        },
    ).await;

    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "LockFromBroadcast", &[]).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "TargetFromBroadcast", &[PyRep::Integer(90_000_003)]).await.unwrap().unwrap_err().code, "TargetNotInSystem");

    let lock_kv = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "TargetFromBroadcast", &[PyRep::Integer(90_000_002)]).await.unwrap().unwrap();
    assert_eq!(lock_kv.dict_get("shipID"), Some(&PyRep::Integer(100_000_002)));
    assert_eq!(lock_kv.dict_get("shieldRatio"), Some(&PyRep::Real(0.5)));
    assert_eq!(lock_kv.dict_get("armorRatio"), Some(&PyRep::Real(0.75)));
    assert_eq!(lock_kv.dict_get("hullRatio"), Some(&PyRep::Real(0.5)));

    let npc_lock = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "LockFromBroadcast", &[PyRep::Integer(777_888_999)]).await.unwrap().unwrap();
    assert_eq!(npc_lock.dict_get("shieldRatio"), Some(&PyRep::Real(1.0)));

    let too_many: Vec<PyRep> = (1..=16).map(PyRep::Integer).collect();
    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "RegisterForDamageUpdates", &[PyRep::List(too_many)]).await.unwrap().unwrap_err().code, "WatchlistFull");
    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "RegisterForDamageUpdates", &[PyRep::Integer(99_999_999)]).await.unwrap().unwrap_err().code, "MemberNotFound");

    let empty_reg = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "RegisterForDamageUpdates", &[PyRep::List(vec![])]).await.unwrap().unwrap();
    assert_eq!(empty_reg.dict_get("watched"), Some(&PyRep::Bool(false)));

    let tuple_reg = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "RegisterForDamageUpdates", &[PyRep::Tuple(vec![PyRep::Integer(90_000_002), PyRep::Integer(90_000_003)])]).await.unwrap().unwrap();
    assert_eq!(tuple_reg.dict_get("watched"), Some(&PyRep::Bool(true)));

    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "AddToWatchlist", &[PyRep::Integer(99_999_999)]).await.unwrap().unwrap_err().code, "MemberNotFound");
    let self_add = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "AddToWatchlist", &[]).await.unwrap().unwrap();
    assert_eq!(self_add.dict_get("characterID"), Some(&PyRep::Integer(90_000_001)));
    assert_eq!(self_add.dict_get("watched"), Some(&PyRep::Bool(true)));

    let fav_add = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "AddToWatchlist", &[PyRep::Tuple(vec![PyRep::Integer(90_000_002)]), PyRep::Tuple(vec![PyRep::Integer(90_000_004)])]).await.unwrap().unwrap();
    assert_eq!(fav_add.dict_get("shieldRatio"), Some(&PyRep::Real(0.5)));

    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        let set = ex.watchlist.entry(90_000_001).or_default();
        for i in 1..=15 {
            set.insert(80_000_000 + i);
        }
    }
    let _ = fleet_id;
    assert_eq!(handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "AddToWatchlist", &[PyRep::Integer(90_000_005)]).await.unwrap().unwrap_err().code, "WatchlistFull");
}

#[tokio::test]
async fn fleet_setups_formations_and_adverts_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_200);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    assert_eq!(call_fleet_mgr(&ctx, &mut boss_sess, "StoreFleetSetup", &[PyRep::String("   ".into())], &kw).await.unwrap_err().code, "InvalidArgument");

    let wings_arg = PyRep::List(vec![
        PyRep::keyval([
            ("name", PyRep::String("CustomWingA".into())),
            ("squads", PyRep::List(vec![PyRep::String("CustomSq1".into()), PyRep::keyval([("name", PyRep::String("CustomSq2".into()))])])),
        ]),
        PyRep::String("CustomWingB".into()),
    ]);
    let stored = call_fleet_mgr(&ctx, &mut boss_sess, "StoreFleetSetup", &[PyRep::String("TacticalSetup".into()), PyRep::String("TacMOTD".into()), PyRep::Integer(1), PyRep::Integer(999), wings_arg], &kw).await.unwrap();
    assert_eq!(stored.dict_get("maxSize"), Some(&PyRep::Integer(256)));
    assert_eq!(stored.dict_get("isFreeMove"), Some(&PyRep::Bool(true)));
    assert_eq!(stored.dict_get("wingCount"), Some(&PyRep::Integer(2)));

    let mut sq_member = make_session(90_000_004, Some(fleet_id), Some(30_000_142), Some(100_000_004));
    assert_eq!(call_fleet_mgr(&ctx, &mut sq_member, "LoadFleetSetup", &[PyRep::String("TacticalSetup".into())], &kw).await.unwrap_err().code, "FleetPermissionDenied");

    let loaded = call_fleet_mgr(&ctx, &mut boss_sess, "LoadFleetSetup", &[PyRep::String("TacticalSetup".into())], &kw).await.unwrap();
    assert_eq!(loaded.dict_get("motd"), Some(&PyRep::String("TacMOTD".into())));
    assert_eq!(loaded.dict_get("isFreeMove"), Some(&PyRep::Bool(true)));

    let del1 = call_fleet_mgr(&ctx, &mut boss_sess, "DeleteFleetSetup", &[PyRep::String("TacticalSetup".into())], &kw).await.unwrap();
    assert_eq!(del1.dict_get("deleted"), Some(&PyRep::Bool(true)));
    let del2 = call_fleet_mgr(&ctx, &mut boss_sess, "DeleteFleetSetup", &[PyRep::String("TacticalSetup".into())], &kw).await.unwrap();
    assert_eq!(del2.dict_get("deleted"), Some(&PyRep::Bool(false)));

    for (name_in, expected_id, expected_name, expected_default_spacing) in [
        ("point_formation", 0, "Point", 0.0),
        ("wall_formation", 1, "Wall", 5_000.0),
        ("plane_formation", 1, "Plane", 5_000.0),
        ("sphere_formation", 2, "Sphere", 10_000.0),
        ("arrow_formation", 3, "Arrow", 5_000.0),
        ("diamond_formation", 4, "Diamond", 5_000.0),
        ("relative_formation", 5, "Relative", 0.0),
        ("unknown_shape", 0, "Point", 0.0),
    ] {
        let f_res = call_fleet_mgr(&ctx, &mut boss_sess, "SetFleetFormationSettings", &[PyRep::String(name_in.into())], &kw).await.unwrap();
        assert_eq!(f_res.dict_get("formationID"), Some(&PyRep::Integer(expected_id)));
        assert_eq!(f_res.dict_get("formationName"), Some(&PyRep::String(expected_name.into())));
        assert_eq!(f_res.dict_get("formationSpacing"), Some(&PyRep::Real(expected_default_spacing)));
    }

    assert_eq!(call_fleet_proxy(&ctx, &mut sq_member, "AddFleetFinderAdvert", &[], &kw).await.unwrap_err().code, "FleetRoleDenied");
    assert_eq!(call_fleet_proxy(&ctx, &mut boss_sess, "UpdateAdvertInfo", &[], &kw).await.unwrap_err().code, "AdvertNotFound");

    let ad_kv = PyRep::keyval([
        ("fleetName", PyRep::String("Standing Ops".into())),
        ("description", PyRep::String("Gatecamp".into())),
        ("inviteScope", PyRep::Integer(2)),
        ("joinNeedsApproval", PyRep::Bool(true)),
        ("minStanding", PyRep::Real(2.5)),
        ("minSecurity", PyRep::Real(-1.0)),
    ]);
    let added_ad = call_fleet_proxy(&ctx, &mut boss_sess, "AddFleetFinderAdvert", &[ad_kv], &kw).await.unwrap();
    assert_eq!(added_ad.dict_get("autoJoin"), Some(&PyRep::Bool(false)));
    assert_eq!(added_ad.dict_get("inviteScope"), Some(&PyRep::Integer(2)));

    let upd_ad = call_fleet_proxy(&ctx, &mut boss_sess, "UpdateAdvertInfo", &[PyRep::String("Updated Ops".into()), PyRep::String("New Desc".into()), PyRep::Integer(1)], &kw).await.unwrap();
    assert_eq!(upd_ad.dict_get("fleetName"), Some(&PyRep::String("Updated Ops".into())));

    let ent_res = call_fleet_proxy(&ctx, &mut boss_sess, "UpdateAdvertAllowedEntities", &[PyRep::List(vec![PyRep::Integer(90_000_099), PyRep::Integer(1_000_009)])], &kw).await.unwrap();
    assert_eq!(ent_res.dict_get("allowedCount"), Some(&PyRep::Integer(2)));

    assert_eq!(call_fleet_proxy(&ctx, &mut boss_sess, "UpdateFleetAdvertWithNewLeader", &[PyRep::Integer(90_000_099)], &kw).await.unwrap_err().code, "CustomError");
    let new_ldr = call_fleet_proxy(&ctx, &mut boss_sess, "UpdateFleetAdvertWithNewLeader", &[PyRep::Integer(90_000_004)], &kw).await.unwrap();
    assert_eq!(new_ldr.dict_get("leaderID"), Some(&PyRep::Integer(90_000_004)));
}

#[tokio::test]
async fn join_requests_advert_filtering_and_application_lifecycle() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_000_300);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    let applicant_ok = 90_000_050i64;
    let applicant_low_sec = 90_000_051i64;
    let applicant_other_corp = 90_000_052i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&90_000_001].clone();
        for (cid, corp_id, sec) in [
            (applicant_ok, base_ch.corporation_id, 2.0),
            (applicant_low_sec, base_ch.corporation_id, -5.0),
            (applicant_other_corp, 1_000_125, 2.0),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp_id;
            ch.security_status = sec;
            ch.fleet_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.corp_contacts.entry(base_ch.corporation_id).or_default().push(SocialContactRecord {
            contact_id: applicant_ok, contact_type: "character".into(), standing: 5.0,
            is_watched: false, is_blocked: false, label_ids: vec![],
        });
    });

    let _ = call_fleet_proxy(&ctx, &mut boss_sess, "AddFleetFinderAdvert", &[
        PyRep::String("Corp Standing Fleet".into()), PyRep::String("Corp only".into()),
        PyRep::Integer(1), PyRep::Bool(false), PyRep::Real(1.0), PyRep::Real(0.0),
    ], &kw).await.unwrap();

    let my_ad = call_fleet_proxy(&ctx, &mut boss_sess, "GetMyFleetFinderAdvert", &[], &kw).await.unwrap();
    assert!(format!("{my_ad:?}").contains("Corp Standing Fleet"));

    let mut sess_low_sec = make_session(applicant_low_sec, None, Some(30_000_142), None);
    let ads_low = call_fleet_proxy(&ctx, &mut sess_low_sec, "GetAvailableFleetAds", &[], &kw).await.unwrap();
    assert!(!format!("{ads_low:?}").contains("Corp Standing Fleet"));
    assert_eq!(call_fleet_proxy(&ctx, &mut sess_low_sec, "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw).await.unwrap_err().code, "FleetJoinDenied");

    let mut sess_other_corp = make_session(applicant_other_corp, None, Some(30_000_142), None);
    ctx.db.with_mut_snapshot(|snap| {
        let boss_corp = snap.characters[&90_000_001].corporation_id;
        snap.corp_contacts.entry(boss_corp).or_default().push(SocialContactRecord {
            contact_id: applicant_other_corp, contact_type: "character".into(), standing: 5.0,
            is_watched: false, is_blocked: false, label_ids: vec![],
        });
    });
    assert_eq!(call_fleet_proxy(&ctx, &mut sess_other_corp, "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw).await.unwrap_err().code, "FleetJoinDenied");

    let mut sess_ok = make_session(applicant_ok, None, Some(30_000_142), None);
    let ads_ok = call_fleet_proxy(&ctx, &mut sess_ok, "GetAvailableFleetAds", &[], &kw).await.unwrap();
    assert!(format!("{ads_ok:?}").contains("Corp Standing Fleet"));

    let app_res = call_fleet_proxy(&ctx, &mut sess_ok, "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw).await.unwrap();
    assert_eq!(app_res.dict_get("joined"), Some(&PyRep::Bool(false)));

    let mut sq_member = make_session(90_000_004, Some(fleet_id), Some(30_000_142), Some(100_000_004));
    assert_eq!(call_fleet_mgr(&ctx, &mut sq_member, "GetJoinRequests", &[], &kw).await.unwrap_err().code, "FleetPermissionDenied");
    let reqs = call_fleet_mgr(&ctx, &mut boss_sess, "GetJoinRequests", &[], &kw).await.unwrap();
    assert!(matches!(reqs, PyRep::Dict(ref d) if d.len() == 1));

    assert_eq!(call_fleet_mgr(&ctx, &mut sq_member, "RejectJoinRequest", &[PyRep::Integer(applicant_ok)], &kw).await.unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(call_fleet_mgr(&ctx, &mut boss_sess, "RejectJoinRequest", &[PyRep::Integer(999_999)], &kw).await.unwrap_err().code, "FleetJoinRequestNotFound");
    let rej = call_fleet_mgr(&ctx, &mut boss_sess, "RejectJoinRequest", &[PyRep::Integer(applicant_ok)], &kw).await.unwrap();
    assert_eq!(rej.dict_get("rejected"), Some(&PyRep::Bool(true)));

    let _ = call_fleet_proxy(&ctx, &mut sess_ok, "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw).await.unwrap();
    assert_eq!(call_fleet_mgr(&ctx, &mut boss_sess, "AcceptJoinRequest", &[PyRep::Integer(applicant_ok), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1)], &kw).await.unwrap_err().code, "PositionOccupied");
    let acc = call_fleet_mgr(&ctx, &mut boss_sess, "AcceptJoinRequest", &[PyRep::Integer(applicant_ok), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)], &kw).await.unwrap();
    assert_eq!(acc.dict_get("approved"), Some(&PyRep::Bool(true)));

    assert_eq!(call_fleet_proxy(&ctx, &mut sq_member, "RemoveFleetFinderAdvert", &[], &kw).await.unwrap_err().code, "FleetPermissionDenied");
    let rem_ad = call_fleet_proxy(&ctx, &mut boss_sess, "RemoveFleetFinderAdvert", &[], &kw).await.unwrap();
    assert_eq!(rem_ad.dict_get("removed"), Some(&PyRep::Bool(true)));

    let umi = call_fleet_obj(&ctx, &mut boss_sess, "UpdateMemberInfo", &[], &kw).await.unwrap();
    assert_eq!(umi.dict_get("fleetID"), Some(&PyRep::Integer(fleet_id)));
    assert_eq!(umi.dict_get("role"), Some(&PyRep::Integer(1)));
    clear_destroyed_target_tag(&ctx, 123_456);
    let _ = (FleetRole::SquadMember, HashSet::<i64>::new);
}

async fn call_fleet_obj(
    ctx: &crate::router::ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["fleetObjectHandler"][0];
    handle(ctx, session, svc, method, args, kwargs).await.unwrap()
}

async fn call_fleet_mgr(
    ctx: &crate::router::ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["fleetMgr"][0];
    handle(ctx, session, svc, method, args, kwargs).await.unwrap()
}

async fn call_fleet_proxy(
    ctx: &crate::router::ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let svc = ["fleetProxy"][0];
    handle(ctx, session, svc, method, args, kwargs).await.unwrap()
}
