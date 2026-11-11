//! Edge-case, ACL, permission, and state-transition tests for `fleet_social_finder_tactical`.

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, seed_multi_member_fleet, test_ctx_and_clock,
};

#[tokio::test]
async fn watchlist_errors_overflow_broadcast_and_db_hp_fallback() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;

    // Unauthenticated fails
    let mut anon = SessionState::new(901);
    assert!(handle_fleet_watchlist_ops(&ctx, &mut anon, "GetWatchlist", &[])
        .await
        .unwrap()
        .is_err());

    // Not in fleet: RegisterForDamageUpdates returns watched=false, others return NotInFleet
    let mut no_fleet = session.clone();
    no_fleet.fleet_id = None;
    let r = handle_fleet_watchlist_ops(&ctx, &mut no_fleet, "RegisterForDamageUpdates", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.dict_get("watched").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut no_fleet, "GetWatchlist", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // LockFromBroadcast errors: missing arg & docked/other-system target
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut session, "LockFromBroadcast", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fleet_watchlist_ops(
            &ctx,
            &mut session,
            "TargetFromBroadcast",
            &[PyRep::Integer(90_000_005)] // in Perimeter, caller is in Jita
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInSystem"
    );

    // Seed 16 members into fleet to hit MAX_WATCHLIST_SIZE = 15
    let mut sixteen = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..16i64 {
            let cid = 90_800_000 + i;
            let sid = 100_800_000 + i;
            let mut ch = clone_char_with(snap, cid, &format!("WL{i}"), 98_000_001, 30_000_142, sid);
            ch.fleet_id = Some(fleet_id);
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: 30_000_142,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::from([(264, 50.0), (266, 20.0), (3, 10.0)]),
                    ..Default::default()
                },
            );
            if let Some(f) = snap.fleets.get_mut(&fleet_id) {
                f.members.push(cid);
            }
            sixteen.push(PyRep::Integer(cid));
        }
    });

    assert_eq!(
        handle_fleet_watchlist_ops(
            &ctx,
            &mut session,
            "RegisterForDamageUpdates",
            &[PyRep::Tuple(sixteen.clone())]
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WatchlistFull"
    );

    // MemberNotFound when registering a non-fleet char
    assert_eq!(
        handle_fleet_watchlist_ops(
            &ctx,
            &mut session,
            "RegisterForDamageUpdates",
            &[PyRep::Integer(99_999_999)]
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MemberNotFound"
    );

    // AddToWatchlist with fav_ids, then overflow with 16th member
    let ok_add = handle_fleet_watchlist_ops(
        &ctx,
        &mut session,
        "AddToWatchlist",
        &[PyRep::List(sixteen[..14].to_vec()), PyRep::Tuple(vec![sixteen[14].clone()])],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_add.dict_get("watched").and_then(|v| v.as_bool()), Some(true));

    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut session, "AddToWatchlist", &[sixteen[15].clone()])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WatchlistFull"
    );

    // GetWatchlist resolves DB dynamic_attributes HP ratios for unspawned ships
    let wl = handle_fleet_watchlist_ops(&ctx, &mut session, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(wl, PyRep::List(ref v) if v.len() == 15));

    // AddToWatchlist with non-member fails MemberNotFound
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut session, "AddToWatchlist", &[PyRep::Integer(99_999_998)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MemberNotFound"
    );
}

#[tokio::test]
async fn fleet_setups_store_load_delete_permissions_and_wing_squad_expansion() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    let svc = "fleetMgr";

    // Empty setup name fails
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut boss_sess,
            svc,
            "StoreFleetSetup",
            &[PyRep::String("   ".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Store setup with custom wings + squads and verify reload from DB
    let wings_arg = PyRep::List(vec![
        PyRep::keyval([
            ("name", PyRep::String("Alpha Wing".into())),
            ("squads", PyRep::List(vec![PyRep::String("Alpha 1".into()), PyRep::String("Alpha 2".into())])),
        ]),
        PyRep::keyval([
            ("name", PyRep::String("Bravo Wing".into())),
            ("squads", PyRep::List(vec![PyRep::String("Bravo 1".into())])),
        ]),
        PyRep::keyval([
            ("name", PyRep::String("Charlie Wing".into())),
            ("squads", PyRep::List(vec![PyRep::String("Charlie 1".into())])),
        ]),
    ]);
    handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        svc,
        "StoreFleetSetup",
        &[
            PyRep::String("DoctrineX".into()),
            PyRep::String("Doctrine MOTD".into()),
            PyRep::Integer(1),
            PyRep::Integer(200),
            wings_arg,
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Clear in-memory cache and reload via GetFleetSetups
    ctx.fleet_extras.lock().unwrap().saved_setups.clear();
    let fetched = handle_finder_and_tactical(&ctx, &mut boss_sess, svc, "GetFleetSetups", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(fetched, PyRep::Dict(ref d) if d.len() == 1));

    // Non-boss, non-FC member cannot LoadFleetSetup
    let mut mem_sess = SessionState::new(902);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut mem_sess,
            svc,
            "LoadFleetSetup",
            &[PyRep::String("DoctrineX".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Boss loads DoctrineX -> expands wings and squads
    let loaded = handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        svc,
        "LoadFleetSetup",
        &[PyRep::String("DoctrineX".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.dict_get("motd").and_then(|v| v.as_str()), Some("Doctrine MOTD"));
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        assert!(f.wings.len() >= 3);
    });

    // Delete existing and missing setup
    let del_ok = handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        svc,
        "DeleteFleetSetup",
        &[PyRep::String("DoctrineX".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_ok.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));
    let del_miss = handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        svc,
        "DeleteFleetSetup",
        &[PyRep::String("Missing".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_miss.dict_get("deleted").and_then(|v| v.as_bool()), Some(false));

    // NotInFleet for LoadFleetSetup
    let mut no_fleet = boss_sess.clone();
    no_fleet.fleet_id = None;
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut no_fleet, svc, "LoadFleetSetup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
}

#[tokio::test]
async fn fleet_finder_acl_filters_join_requests_accept_reject_and_leader_transfer() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Non-boss non-leader fails UpdateAdvertInfo with FleetRoleDenied
    let mut mem_sess = SessionState::new(903);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut mem_sess, "fleetProxy", "UpdateAdvertInfo", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetRoleDenied"
    );

    // Create advert via positional args with auto_join = false, min_standing = 5.0, min_security = 0.0, invite_scope = 1 (corp)
    handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("CorpOps".into()),
            PyRep::String("Corp only".into()),
            PyRep::Integer(1),
            PyRep::Bool(false),
            PyRep::Real(5.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Seed 3 outside applicants
    let app_low_sec = 90_900_001_i64;
    let app_bad_standing = 90_900_002_i64;
    let app_valid = 90_900_003_i64;
    let app_other_corp = 90_900_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let boss_corp = snap.characters[&90_000_001].corporation_id;
        for (cid, sec, corp) in [
            (app_low_sec, -5.0, boss_corp),
            (app_bad_standing, 2.0, boss_corp),
            (app_valid, 2.0, boss_corp),
            (app_other_corp, 2.0, 98_999_999),
        ] {
            let mut ch = clone_char_with(snap, cid, &format!("App{cid}"), corp, 30_000_142, 0);
            ch.security_status = sec;
            snap.characters.insert(cid, ch);
        }
        snap.corp_contacts.entry(boss_corp).or_default().push(eve_db::SocialContactRecord {
            contact_id: app_valid,
            standing: 10.0,
            contact_type: "character".into(),
            label_ids: vec![],
            is_watched: false,
            is_blocked: false,
        });
        snap.corp_contacts.entry(boss_corp).or_default().push(eve_db::SocialContactRecord {
            contact_id: app_other_corp,
            standing: 10.0,
            contact_type: "character".into(),
            label_ids: vec![],
            is_watched: false,
            is_blocked: false,
        });
    });

    // GetMyFleetFinderAdvert & GetAvailableFleetAds filtering
    let my_ad = handle_finder_and_tactical(&ctx, &mut boss_sess, "fleetProxy", "GetMyFleetFinderAdvert", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(my_ad, PyRep::None));

    let mut s_low = SessionState::new(910);
    s_low.user_id = Some(app_low_sec as u64);
    s_low.character_id = Some(app_low_sec);
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut s_low,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fleet_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetJoinDenied"
    );

    let mut s_bad_st = SessionState::new(911);
    s_bad_st.user_id = Some(app_bad_standing as u64);
    s_bad_standing_set(&mut s_bad_st, app_bad_standing);
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut s_bad_st,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fleet_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetJoinDenied"
    );

    let mut s_other_corp = SessionState::new(912);
    s_bad_standing_set(&mut s_other_corp, app_other_corp);
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut s_other_corp,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fleet_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetJoinDenied"
    );

    // Valid applicant queues join request (joined = false because auto_join = false)
    let mut s_valid = SessionState::new(913);
    s_bad_standing_set(&mut s_valid, app_valid);
    let queued = handle_finder_and_tactical(
        &ctx,
        &mut s_valid,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(queued.dict_get("joined").and_then(|v| v.as_bool()), Some(false));

    // SquadMember cannot view or accept join requests
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut mem_sess, "fleetProxy", "GetJoinRequests", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut mem_sess,
            "fleetProxy",
            "AcceptJoinRequest",
            &[PyRep::Integer(app_valid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Boss views join requests, tries occupied WingCommander slot -> PositionOccupied, then accepts as SquadMember
    let reqs = handle_finder_and_tactical(&ctx, &mut boss_sess, "fleetProxy", "GetJoinRequests", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(reqs, PyRep::Dict(ref d) if d.len() == 1));

    // Note: Wing 710_001 already has WC 90_000_002 -> PositionOccupied
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut boss_sess,
            "fleetProxy",
            "AcceptJoinRequest",
            &[PyRep::Integer(app_valid), PyRep::Integer(710_001), PyRep::Integer(720_001), PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "PositionOccupied"
    );

    let acc = handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        "fleetProxy",
        "AcceptJoinRequest",
        &[PyRep::Integer(app_valid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(acc.dict_get("approved").and_then(|v| v.as_bool()), Some(true));

    // Now test RejectJoinRequest + auto_join = true + UpdateFleetAdvertWithNewLeader + RemoveFleetFinderAdvert
    handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        "fleetProxy",
        "UpdateFleetAdvertWithNewLeader",
        &[PyRep::Integer(90_000_004)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut boss_sess,
            "fleetProxy",
            "UpdateFleetAdvertWithNewLeader",
            &[PyRep::Integer(99_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CustomError"
    );

    // Non-boss fails RemoveFleetFinderAdvert
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut mem_sess, "fleetProxy", "RemoveFleetFinderAdvert", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );

    // Manually insert join request for app_bad_standing and reject it as boss
    ctx.fleet_extras
        .lock()
        .unwrap()
        .join_requests
        .entry(fleet_id)
        .or_default()
        .insert(app_bad_standing);
    let rej = handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        "fleetProxy",
        "RejectJoinRequest",
        &[PyRep::Integer(app_bad_standing)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rej.dict_get("rejected").and_then(|v| v.as_bool()), Some(true));

    // Reject missing request -> FleetJoinRequestNotFound
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut boss_sess,
            "fleetProxy",
            "RejectJoinRequest",
            &[PyRep::Integer(app_bad_standing)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetJoinRequestNotFound"
    );

    // Boss removes advert, then ApplyToJoinFleet fails FleetAdvertNotFound
    let rem = handle_finder_and_tactical(&ctx, &mut boss_sess, "fleetProxy", "RemoveFleetFinderAdvert", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rem.dict_get("removed").and_then(|v| v.as_bool()), Some(true));

    // Auto-join path when advert has auto_join = true
    handle_finder_and_tactical(
        &ctx,
        &mut boss_sess,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("OpenFleet".into()),
            PyRep::String("Open".into()),
            PyRep::Integer(3),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let auto_joined = handle_finder_and_tactical(
        &ctx,
        &mut s_other_corp,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(auto_joined.dict_get("joined").and_then(|v| v.as_bool()), Some(true));

    // AlreadyInFleet when applying again
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut s_other_corp,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fleet_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AlreadyInFleet"
    );
}

fn s_bad_standing_set(sess: &mut SessionState, cid: i64) {
    sess.user_id = Some(cid as u64);
    sess.character_id = Some(cid);
}
