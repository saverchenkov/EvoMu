//! Comprehensive branch-coverage and mutation-killing tests for `fleet_social_finder_tactical.rs`.

use crate::services::fleet_social::fleet_social_finder_tactical::{
    advert_to_dict, clear_destroyed_target_tag, ensure_fleet_formations_loaded,
    ensure_fleet_setups_loaded, formation_name, handle_fleet_watchlist_ops,
    normalize_advert_date_created_filetime, sync_fleet_formations_to_db,
    sync_fleet_setups_to_db,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::fleet_social_fleets::{FleetAdvertState, FleetSetupRecord};
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

    let add_ad = call_fleet_proxy(&ctx, &mut boss_sess, "AddFleetFinderAdvert", &[
        PyRep::String("Corp Standing Fleet".into()), PyRep::String("Corp only".into()),
        PyRep::Integer(1), PyRep::Bool(false), PyRep::Real(1.0), PyRep::Real(0.0),
    ], &kw).await.unwrap();
    assert!(matches!(add_ad, PyRep::Dict(_)), "AddFleetFinderAdvert must return Dict (#732)");
    assert_eq!(add_ad.dict_get("fleetName").and_then(PyRep::as_str), Some("Corp Standing Fleet"));

    let my_ad = call_fleet_proxy(&ctx, &mut boss_sess, "GetMyFleetFinderAdvert", &[], &kw).await.unwrap();
    assert!(matches!(my_ad, PyRep::Dict(_)), "GetMyFleetFinderAdvert must return Dict (#732)");
    assert!(format!("{my_ad:?}").contains("Corp Standing Fleet"));

    let mut sess_low_sec = make_session(applicant_low_sec, None, Some(30_000_142), None);
    let ads_low = call_fleet_proxy(&ctx, &mut sess_low_sec, "GetAvailableFleetAds", &[], &kw).await.unwrap();
    assert!(matches!(ads_low, PyRep::Dict(_)));
    assert!(!format!("{ads_low:?}").contains("Corp Standing Fleet"));
    let ads_low_char = call_fleet_proxy(&ctx, &mut sess_low_sec, "GetFleetAdsForChar", &[], &kw).await.unwrap();
    assert!(matches!(ads_low_char, PyRep::Dict(_)));
    assert!(!format!("{ads_low_char:?}").contains("Corp Standing Fleet"));
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
    let ads_ok_char = call_fleet_proxy(&ctx, &mut sess_ok, "GetFleetAdsForChar", &[], &kw).await.unwrap();
    assert!(matches!(ads_ok_char, PyRep::Dict(ref d) if !d.is_empty()), "GetFleetAdsForChar must return Dict of Dicts (#732)");
    if let PyRep::Dict(entries) = &ads_ok_char {
        assert!(entries.iter().any(|(k, v)| *k == PyRep::Integer(fleet_id) && matches!(v, PyRep::Dict(_))));
    }

    let upd_ad = call_fleet_proxy(&ctx, &mut boss_sess, "UpdateAdvertInfo", &[PyRep::Integer(0)], &kw).await.unwrap();
    assert!(matches!(upd_ad, PyRep::Dict(_)), "UpdateAdvertInfo must return Dict (#732)");

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
    assert!(matches!(rem_ad, PyRep::Dict(_)), "RemoveFleetFinderAdvert must return Dict (#732)");
    assert_eq!(rem_ad.dict_get("removed"), Some(&PyRep::Bool(true)));
    let my_ad_after = call_fleet_proxy(&ctx, &mut boss_sess, "GetMyFleetFinderAdvert", &[], &kw).await.unwrap();
    assert_eq!(my_ad_after, PyRep::None);

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

const CLIENT_YEAR1800_TICKS: i64 = 199 * 365 * 86_400 * 10_000_000;

fn assert_advert_date_created_valid(ad_rep: &PyRep, now_client_wall_ft: i64, label: &str) -> i64 {
    let date_created = ad_rep
        .dict_get("dateCreated")
        .and_then(PyRep::as_i64)
        .unwrap_or_else(|| panic!("{label}: missing integer dateCreated in {ad_rep:?}"));
    assert!(
        date_created > 0,
        "{label}: dateCreated must be positive FILETIME ticks, got {date_created}"
    );
    let elapsed_ticks = now_client_wall_ft - date_created;
    assert!(
        elapsed_ticks >= 0,
        "{label}: gametime.GetWallclockTime() - dateCreated must be >= 0 for FmtDate (elapsed_ticks={elapsed_ticks}, now_client_wall_ft={now_client_wall_ft}, date_created={date_created})"
    );
    assert!(
        elapsed_ticks < CLIENT_YEAR1800_TICKS,
        "{label}: elapsed_ticks ({elapsed_ticks}) must remain < const.YEAR365 * 199 for FmtDate('ns')"
    );
    date_created
}

#[test]
fn normalize_advert_date_created_filetime_handles_advancetime_zero_negative_filetime_and_skew() {
    let unix_to_ft = crate::sp6_lifecycle::unix_to_filetime;
    let host_now: u64 = 1_775_000_000;

    // 1. Normal historical Unix timestamp (unrebased = 0)
    let normal_now_ft = unix_to_ft(host_now);
    assert_eq!(
        normalize_advert_date_created_filetime(1_700_000_000, host_now, normal_now_ft, 0),
        unix_to_ft(1_700_000_000)
    );

    // 2. AdvanceClock (+3600s) before advert creation (#1295-#1298):
    //    server now_unix & ad._created_unix both carry +3600, while client wallclock stays at host_now.
    let adv_secs: u64 = 3_600;
    let server_now = host_now + adv_secs;
    let dt_at_create = normalize_advert_date_created_filetime(server_now, server_now, normal_now_ft, adv_secs);
    assert_eq!(dt_at_create, normal_now_ft);
    assert_eq!(normal_now_ft - dt_at_create, 0);

    // 25s later in real time: client sees +25s age
    let dt_later = normalize_advert_date_created_filetime(
        server_now,
        server_now + 25,
        unix_to_ft(host_now + 25),
        adv_secs,
    );
    assert_eq!(unix_to_ft(host_now + 25) - dt_later, 25 * 10_000_000);

    // 3. AdvanceClock (+3600s) after advert creation: age reflects +3600s server time and >= 0
    let dt_pre_adv = normalize_advert_date_created_filetime(host_now, server_now, normal_now_ft, adv_secs);
    assert_eq!(normal_now_ft - dt_pre_adv, (adv_secs as i64) * 10_000_000);

    // 4. Zero / negative-bitcast / invalid intermediate tick fallback safely to client wallclock now
    assert_eq!(
        normalize_advert_date_created_filetime(0, server_now, normal_now_ft, adv_secs),
        normal_now_ft
    );
    assert_eq!(
        normalize_advert_date_created_filetime((-120_i64) as u64, server_now, normal_now_ft, adv_secs),
        normal_now_ft
    );
    assert_eq!(
        normalize_advert_date_created_filetime(50_000_000_000_000, server_now, normal_now_ft, adv_secs),
        normal_now_ft
    );

    // 5. Future-seeded / forward clock skew clamps cleanly to client wallclock now
    assert_eq!(
        normalize_advert_date_created_filetime(server_now + 86_400, server_now, normal_now_ft, adv_secs),
        normal_now_ft
    );

    // 6. Unix-vs-FILETIME mismatch: FILETIME ticks stored directly into _created_unix
    let past_ft = unix_to_ft(server_now - 90) as u64;
    let norm_past_ft = normalize_advert_date_created_filetime(past_ft, server_now, normal_now_ft, adv_secs);
    assert_eq!(normal_now_ft - norm_past_ft, 90 * 10_000_000);

    let future_ft = unix_to_ft(server_now + 1_200) as u64;
    assert_eq!(
        normalize_advert_date_created_filetime(future_ft, server_now, normal_now_ft, adv_secs),
        normal_now_ft
    );

    // 7. Unix milliseconds stored into _created_unix
    let ms_created = (server_now - 45) * 1_000;
    let norm_ms = normalize_advert_date_created_filetime(ms_created, server_now, normal_now_ft, adv_secs);
    assert_eq!(normal_now_ft - norm_ms, 45 * 10_000_000);
}

#[tokio::test]
async fn fleet_finder_adverts_prevent_negative_fmtdate_across_advancetime_seeds_and_reload() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_fleet_ad_fmtdate_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db_path = dir.join("fleet_ad_fmtdate.sqlite");
    let kw = HashMap::new();

    let sde_ctx = crate::router::ServerContext::new();
    let sde = std::sync::Arc::clone(&sde_ctx.sde);
    let demo_char = sde_ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).cloned())
        .expect("demo character seeded");

    // Boot 1: AdvanceClock (+7200s) before advert creation, exercise Add/Update/Get + corrupt seeds, persist SQLite checkpoints.
    let fleet_id = {
        let db1 = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 1"),
        );
        db1.with_mut_snapshot(|s| {
            s.characters.insert(demo_char.character_id, demo_char.clone());
        });
        let ctx1 = crate::router::ServerContext::with_sde_and_repository(
            std::sync::Arc::clone(&sde),
            db1,
        );
        let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx1).await;
        let outsider_id = 90_000_088_i64;
        ctx1.db.with_mut_snapshot(|snap| {
            let mut outsider = demo_char.clone();
            outsider.character_id = outsider_id;
            outsider.user_id = outsider_id as u64;
            outsider.fleet_id = None;
            outsider.security_status = 0.0;
            snap.characters.insert(outsider_id, outsider);
        });
        let mut outsider_sess = make_session(outsider_id, None, Some(30_000_142), None);

        // Reproduce #1295-#1298: timeControl.AdvanceClock before creating advert
        ctx1.advance_secs_without_sim_clock_rebase(7_200);

        let add_ad = call_fleet_proxy(
            &ctx1,
            &mut boss_sess,
            "AddFleetFinderAdvert",
            &[
                PyRep::String("AdvanceClock Public Fleet".into()),
                PyRep::String("Testing FmtDate baseline".into()),
                PyRep::Integer(3),
                PyRep::Bool(true),
                PyRep::Real(-10.0),
                PyRep::Real(-10.0),
            ],
            &kw,
        )
        .await
        .unwrap();
        let wall_ft = ctx1.now_machonet_wallclock_filetime_ticks();
        assert_advert_date_created_valid(&add_ad, wall_ft, "AddFleetFinderAdvert");

        // AdvanceClock again after advert creation + query GetAvailableFleetAds / GetFleetAdsForChar / GetMyFleetFinderAdvert
        ctx1.advance_secs_without_sim_clock_rebase(1_800);
        let wall_ft_after = ctx1.now_machonet_wallclock_filetime_ticks();

        let upd_ad = call_fleet_proxy(
            &ctx1,
            &mut boss_sess,
            "UpdateAdvertInfo",
            &[
                PyRep::String("AdvanceClock Updated Fleet".into()),
                PyRep::String("Post-advance desc".into()),
                PyRep::Integer(3),
            ],
            &kw,
        )
        .await
        .unwrap();
        assert_advert_date_created_valid(&upd_ad, wall_ft_after, "UpdateAdvertInfo");

        let my_ad = call_fleet_proxy(&ctx1, &mut boss_sess, "GetMyFleetFinderAdvert", &[], &kw)
            .await
            .unwrap();
        assert_advert_date_created_valid(&my_ad, wall_ft_after, "GetMyFleetFinderAdvert");

        for method in ["GetAvailableFleetAds", "GetFleetAdsForChar"] {
            let ads = call_fleet_proxy(&ctx1, &mut outsider_sess, method, &[], &kw)
                .await
                .unwrap();
            let PyRep::Dict(entries) = ads else {
                panic!("{method} must return Dict");
            };
            assert!(
                !entries.is_empty(),
                "{method} must contain active public advert"
            );
            for (k, v) in &entries {
                assert_advert_date_created_valid(v, wall_ft_after, &format!("{method}[{k:?}]"));
            }
        }

        // Verify direct advert_to_dict and RPC normalization on zero, negative-wrapped, future-skewed, and FILETIME seeds
        let now_u = ctx1.now_unix();
        for (bad_fid, bad_ts, tag) in [
            (fleet_id + 101, 0_u64, "zero_created_unix"),
            (fleet_id + 102, (-300_i64) as u64, "negative_bitcast_created_unix"),
            (fleet_id + 103, now_u + 86_400, "future_skew_created_unix"),
            (
                fleet_id + 104,
                crate::sp6_lifecycle::unix_to_filetime(now_u + 3_600) as u64,
                "future_filetime_created_unix",
            ),
        ] {
            let bad_ad = FleetAdvertState {
                fleet_id: bad_fid,
                leader_id: 90_000_001,
                fleet_name: format!("Seeded {tag}"),
                description: tag.into(),
                solar_system_id: 30_000_142,
                invite_scope: 3,
                auto_join: true,
                min_standing: -10.0,
                min_security: -10.0,
                allowed_entities: HashSet::new(),
                _created_unix: bad_ts,
            };
            let direct = advert_to_dict(&ctx1, &bad_ad, 1, None);
            assert_advert_date_created_valid(&direct, ctx1.now_machonet_wallclock_filetime_ticks(), tag);
            ctx1.fleet_extras.lock().unwrap().adverts.insert(bad_fid, bad_ad);
        }

        let ads_all = call_fleet_proxy(&ctx1, &mut outsider_sess, "GetAvailableFleetAds", &[], &kw)
            .await
            .unwrap();
        let wall_ft_seeded = ctx1.now_machonet_wallclock_filetime_ticks();
        if let PyRep::Dict(entries) = ads_all {
            assert_eq!(entries.len(), 5);
            for (k, v) in &entries {
                assert_advert_date_created_valid(v, wall_ft_seeded, &format!("seeded[{k:?}]"));
            }
        } else {
            panic!("GetAvailableFleetAds must return Dict");
        }

        // Remove synthetic corrupt test adverts so only the live advert persists across restart
        {
            let mut ex = ctx1.fleet_extras.lock().unwrap();
            for offset in [101_i64, 102, 103, 104] {
                ex.adverts.remove(&(fleet_id + offset));
            }
        }
        ctx1.persist_server_runtime_to_db();
        fleet_id
    };

    // Boot 2: reload persisted adverts + unrebased_clock_offset_secs from SQLite and confirm dateCreated remains non-negative
    {
        let db2 = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 2"),
        );
        assert_eq!(db2.persisted_unrebased_clock_offset_secs(), Ok(9_000));
        db2.with_mut_snapshot(|snap| {
            snap.characters.insert(demo_char.character_id, demo_char.clone());
            let mut outsider = demo_char.clone();
            outsider.character_id = 90_000_088;
            outsider.user_id = 90_000_088;
            outsider.fleet_id = None;
            outsider.security_status = 0.0;
            snap.characters.insert(90_000_088, outsider);
        });
        let ctx2 = crate::router::ServerContext::with_sde_and_repository(sde, db2);
        ctx2.rehydrate_runtime_from_db();
        assert!(
            ctx2.fleet_extras.lock().unwrap().adverts.contains_key(&fleet_id),
            "persisted advert must reload across SQLite restart"
        );
        let mut outsider_sess = make_session(90_000_088, None, Some(30_000_142), None);
        let mut boss_sess = make_session(90_000_001, Some(fleet_id), Some(30_000_142), None);
        let wall_ft_reloaded = ctx2.now_machonet_wallclock_filetime_ticks();

        let reloaded_my = call_fleet_proxy(&ctx2, &mut boss_sess, "GetMyFleetFinderAdvert", &[], &kw)
            .await
            .unwrap();
        assert_advert_date_created_valid(&reloaded_my, wall_ft_reloaded, "reloaded GetMyFleetFinderAdvert");

        for method in ["GetAvailableFleetAds", "GetFleetAdsForChar"] {
            let reloaded_ads = call_fleet_proxy(&ctx2, &mut outsider_sess, method, &[], &kw)
                .await
                .unwrap();
            let PyRep::Dict(entries) = reloaded_ads else {
                panic!("{method} after reload must return Dict");
            };
            assert_eq!(entries.len(), 1);
            assert_advert_date_created_valid(
                &entries[0].1,
                wall_ft_reloaded,
                &format!("reloaded {method}"),
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
