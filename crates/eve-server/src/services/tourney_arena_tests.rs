use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::tourney_service::{
    register_tourney_setup, save_store, TourneyArenaSystem, TourneyMatch,
};
use std::collections::HashMap;

#[tokio::test]
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
