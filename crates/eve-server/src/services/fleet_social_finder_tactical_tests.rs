use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};

#[test]
fn formation_names_and_persistence_roundtrip() {
    assert_eq!(formation_name(0), "Point");
    assert_eq!(formation_name(1), "Plane");
    assert_eq!(formation_name(2), "Sphere");
    assert_eq!(formation_name(3), "Arrow");
    assert_eq!(formation_name(4), "Diamond");
    assert_eq!(formation_name(5), "Relative");
    assert_eq!(formation_name(99), "Point");

    let kw = HashMap::new();
    assert_eq!(parse_formation_input(&[PyRep::String("sphere".into())], &kw), (2, "Sphere"));
    assert_eq!(parse_formation_input(&[PyRep::Integer(4)], &kw), (4, "Diamond"));

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    ctx.fleet_extras.lock().unwrap().stored_formations.insert(100_000_001, (3, 5000.0, 15000.0));
    sync_fleet_formations_to_db(&ctx);
    ctx.fleet_extras.lock().unwrap().stored_formations.clear();
    ensure_fleet_formations_loaded(&ctx);
    assert_eq!(
        ctx.fleet_extras.lock().unwrap().stored_formations.get(&100_000_001).copied(),
        Some((3, 5000.0, 15000.0))
    );
}

#[tokio::test]
async fn watchlist_setups_formations_and_join_requests_ops() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    let svc = "fleetObjectHandler";

    let reg = handle_fleet_watchlist_ops(
        &ctx,
        &mut session,
        "RegisterForDamageUpdates",
        &[PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(90_000_003)])],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reg.dict_get("watched").and_then(|v| v.as_bool()), Some(true));

    let wl = handle_fleet_watchlist_ops(&ctx, &mut session, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(wl, PyRep::List(ref items) if items.len() == 2));

    let rem = handle_fleet_watchlist_ops(
        &ctx,
        &mut session,
        "RemoveFromWatchlist",
        &[PyRep::Integer(90_000_003)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rem.dict_get("watched").and_then(|v| v.as_bool()), Some(false));

    let locked = handle_fleet_watchlist_ops(
        &ctx,
        &mut session,
        "LockFromBroadcast",
        &[PyRep::Integer(90_000_002)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(locked.dict_get("locked").and_then(|v| v.as_bool()), Some(true));

    let stored = handle_finder_and_tactical(
        &ctx,
        &mut session,
        svc,
        "StoreFleetSetup",
        &[PyRep::String("alpha setup".into()), PyRep::String("motd".into()), PyRep::Bool(true), PyRep::Integer(128)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.dict_get("maxSize").and_then(|v| v.as_i64()), Some(128));

    let setups = handle_finder_and_tactical(&ctx, &mut session, svc, "GetFleetSetups", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(setups, PyRep::Dict(_)));

    let form_set = handle_finder_and_tactical(
        &ctx,
        &mut session,
        svc,
        "SetFleetFormationSettings",
        &[PyRep::Integer(2), PyRep::Real(8000.0), PyRep::Real(20000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(form_set.dict_get("formationID").and_then(|v| v.as_i64()), Some(2));

    let form_get = handle_finder_and_tactical(&ctx, &mut session, svc, "GetFormationSettings", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(form_get.dict_get("formationID").and_then(|v| v.as_i64()), Some(2));

    let fid_rep = handle_finder_and_tactical(&ctx, &mut session, svc, "GetFleetID", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fid_rep, PyRep::Integer(fleet_id));

    let reconn = handle_finder_and_tactical(&ctx, &mut session, svc, "Reconnect", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reconn.dict_get("fleetID").and_then(|v| v.as_i64()), Some(fleet_id));

    ctx.fleet_extras
        .lock()
        .unwrap()
        .target_tags
        .entry(fleet_id)
        .or_default()
        .insert(999_888, "A".into());
    clear_destroyed_target_tag(&ctx, 999_888);
    assert!(
        !ctx.fleet_extras
            .lock()
            .unwrap()
            .target_tags
            .get(&fleet_id)
            .is_some_and(|m| m.contains_key(&999_888))
    );
}
