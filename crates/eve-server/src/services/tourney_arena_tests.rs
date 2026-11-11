use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::tourney_service::{
    register_tourney_setup, save_store, TourneyArenaSystem, TourneyMatch,
};
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tourney_arena_setup_matches_and_teleportation() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut gm = auth_session(2, 90_000_002, 98_000_001, 30_000_142);
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    let kw = HashMap::new();

    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut sess, "other", "GetPotentialMatches", &[], &kw).await.is_none());
    assert!(handle(&ctx, &mut sess, "tourneyMgr", unknown_method, &[], &kw).await.is_none());

    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "tourneyMgr", "GetPotentialMatches", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // Register tournament setup & seed a match with `90_000_001` as referee
    let sys_name = register_tourney_setup(
        &ctx,
        10,
        "Alliance Tournament XXII",
        30_000_142,
        TourneyArenaSystem {
            combat_beacons: vec![9001, 9002],
            warp_bubbles: vec![9003],
        },
    )
    .unwrap();
    assert!(!sys_name.is_empty());

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(&*snap);
        let mut m = TourneyMatch::new(501, 10, 30_000_144, 100, 600, 300, 1);
        m.red_team_name = "Team Red".into();
        m.blue_team_name = "Team Blue".into();
        m.referees.push(90_000_001);
        store.matches.insert(501, m);
        store.active_match_id = Some(501);
        save_store(snap, &store, now);

        // Give referee an active ship in Jita (30000142)
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            eve_db::InventoryItemRecord {
                item_id: sid,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = sid;
        }
    });

    // GetPotentialMatches & GetTourneySetup
    let matches = handle(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "GetPotentialMatches",
        &[PyRep::Integer(10)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(matches, PyRep::List(ref l) if l.len() == 1));

    let setup = handle(&ctx, &mut sess, "tourneyMgr", "GetTourneySetup", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(setup, PyRep::Tuple(ref t) if t.len() == 3));

    // TeleportToArena & TeleportToStation
    let mut non_ref = auth_session(3, 90_000_003, 98_000_001, 30_000_142);
    assert!(handle(
        &ctx,
        &mut non_ref,
        "tourneyMgr",
        "TeleportToArena",
        &[PyRep::Integer(501)],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    let tp_arena = handle(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "TeleportToArena",
        &[PyRep::Integer(501)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_arena, PyRep::Bool(true));
    assert_eq!(sess.solar_system_id, Some(30_000_144));
    assert_eq!(sess.station_id, None);

    let tp_station = handle(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "TeleportToStation",
        &[PyRep::Integer(501)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_station, PyRep::Bool(true));
    assert!(sess.station_id.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tourney_arena_edges_auth_gm_fallback_and_notifications() {
    use crate::services::tourney_service::TourneyMatchState;

    let ctx = test_ctx();
    let mut ref_sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut gm = auth_session(2, 90_000_002, 98_000_001, 30_000_142);
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;
    let mut pleb = auth_session(3, 90_000_003, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Unauthenticated rejected for GetTourneySetup, TeleportToArena, TeleportToStation
    let mut unauth = SessionState::default();
    let expected_unauth = ["Authentication", "Required"].concat();
    for m in ["GetTourneySetup", "TeleportToArena", "TeleportToStation"] {
        assert_eq!(
            handle(&ctx, &mut unauth, "tourneyMgr", m, &[], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_unauth
        );
    }

    // Seed two tournaments, one active match and one Completed match, plus an invalid SDE system in arena_systems
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(&*snap);
        store.tournaments.insert(11, "AT A".into());
        store.tournaments.insert(12, "AT B".into());
        store.arena_systems.insert(
            999_999_999, // not in SDE -> filtered out by GetTourneySetup
            TourneyArenaSystem {
                combat_beacons: vec![1],
                warp_bubbles: vec![2],
            },
        );
        let mut m1 = TourneyMatch::new(601, 11, 30_000_144, 100, 600, 300, 1);
        m1.beacon_ids = vec![7001];
        m1.referees.push(90_000_001);
        let mut m2 = TourneyMatch::new(602, 12, 30_000_142, 100, 600, 300, 1);
        m2.state = TourneyMatchState::Completed;
        m2.beacon_ids = vec![7002];
        store.matches.insert(601, m1);
        store.matches.insert(602, m2);
        save_store(snap, &store, now);
    });

    // GetPotentialMatches with tourney_id = 0 returns all matches (both 601 and 602)
    let all_matches = handle(
        &ctx,
        &mut ref_sess,
        "tourneyMgr",
        "GetPotentialMatches",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(all_matches, PyRep::List(ref l) if l.len() == 2));

    // GetTourneySetup filters invalid SDE system and only puts non-Completed match system in activeSystems
    let setup = handle(&ctx, &mut ref_sess, "tourneyMgr", "GetTourneySetup", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(parts) = setup else { panic!("expected 3-tuple") };
    assert!(matches!(&parts[0], PyRep::Dict(d) if d.len() == 2));
    assert!(matches!(&parts[1], PyRep::Dict(d) if d.iter().all(|(k, _)| *k != PyRep::Integer(999_999_999))));
    assert_eq!(parts[2], PyRep::List(vec![PyRep::Integer(30_000_144)]));

    // Non-referee, non-GM fails TeleportToArena and TeleportToStation when no match resolved
    assert_eq!(
        handle(&ctx, &mut pleb, "tourneyMgr", "TeleportToArena", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut pleb, "tourneyMgr", "TeleportToStation", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // GM with no active_match_id and unknown location -> InvalidLocation
    let mut gm_lost = auth_session(4, 90_000_002, 98_000_001, 30_000_142);
    gm_lost.role |= eve_machonet::service_roles::ROLE_GML;
    gm_lost.solar_system_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.remove(&90_000_002);
    });
    assert_eq!(
        handle(&ctx, &mut gm_lost, "tourneyMgr", "TeleportToArena", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLocation"
    );

    // GM with valid solar_system_id but missing character in DB -> CharacterNotFound
    gm.solar_system_id = Some(30_000_144);
    assert_eq!(
        handle(&ctx, &mut gm, "tourneyMgr", "TeleportToArena", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // GM TeleportToStation with no active_match_id succeeds cleanly
    assert_eq!(
        handle(&ctx, &mut gm, "tourneyMgr", "TeleportToStation", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Referee already in arena system in space does not emit duplicate OnSessionChanged
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = None;
        }
    });
    ref_sess.solar_system_id = Some(30_000_144);
    ref_sess.station_id = None;
    ctx.pending_notifications.write().unwrap().clear();
    assert_eq!(
        handle(&ctx, &mut ref_sess, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(601)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(ctx.pending_notifications.read().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutant_kill_tourney_arena_teleport_db_ship_and_notifications() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Service/method miss must return None
    assert!(handle(&ctx, &mut sess, "nonmatching_service", "TeleportToArena", &[], &kw).await.is_none());
    assert!(handle(&ctx, &mut sess, "tourneyMgr", "unmatched_method", &[], &kw).await.is_none());

    let _ = register_tourney_setup(
        &ctx,
        22,
        "AT Mutant Verify",
        30_000_144,
        TourneyArenaSystem {
            combat_beacons: vec![9101],
            warp_bubbles: vec![9102],
        },
    )
    .unwrap();

    let now = ctx.now_unix();
    let ship_id = 880_700_001_i64;
    ctx.db.acquire_online_lock(1, 710, Some(char_id), true, now).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(&*snap);
        let mut m = TourneyMatch::new(701, 22, 30_000_144, 100, 600, 300, 1);
        m.referees.push(char_id);
        store.matches.insert(701, m);
        store.active_match_id = Some(701);
        save_store(snap, &store, now);

        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.home_station_id = 60_003_760;
        }
    });

    // TeleportToArena updates DB char & ship, spawns ball, and emits OnSessionChanged + OnEnterTournamentSystem
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        handle(&ctx, &mut sess, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(701)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.solar_system_id, 30_000_144);
        assert_eq!(ch.station_id, None);
        let sh = &snap.items[&ship_id];
        assert_eq!(sh.location_id, 30_000_144);
        assert_eq!(sh.flag, eve_db::InventoryFlag::None);
    });
    let enter_notes = ctx.drain_notifications_for_char(char_id);
    assert!(enter_notes.iter().any(|n| n.event == "OnSessionChanged"));
    assert!(enter_notes.iter().any(|n| n.event == "OnEnterTournamentSystem"));

    // TeleportToStation restores char station & ship Hangar flag and emits OnSessionChanged + OnLeaveTournamentSystem
    assert_eq!(
        handle(&ctx, &mut sess, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(701)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.station_id, Some(60_003_760));
        let sh = &snap.items[&ship_id];
        assert_eq!(sh.location_id, 60_003_760);
        assert_eq!(sh.flag, eve_db::InventoryFlag::Hangar);
    });
    let leave_notes = ctx.drain_notifications_for_char(char_id);
    assert!(leave_notes.iter().any(|n| n.event == "OnSessionChanged"));
    assert!(leave_notes.iter().any(|n| n.event == "OnLeaveTournamentSystem"));
}
