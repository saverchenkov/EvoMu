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

#[tokio::test]
async fn fleet_proxy_update_advert_v2401_and_persistence() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // UpdateAdvertAllowedEntities before creating advert yields AdvertNotFound
    let err = handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "UpdateAdvertAllowedEntities",
        &[PyRep::Dict(vec![(
            PyRep::String("membergroupsAllowedEntities".into()),
            PyRep::List(vec![PyRep::Integer(98_000_001)]),
        )])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "AdvertNotFound");

    let boss_id = session.character_id.unwrap();
    let boss_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.accounts.values().next().unwrap().user_id;
        snap.characters.get_mut(&boss_id).unwrap().user_id = uid;
        uid
    });
    session.user_id = Some(boss_uid);
    ctx.db
        .acquire_online_lock(boss_uid, session.session_id.max(1), Some(boss_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(boss_id);

    // Seed an advert via AddFleetFinderAdvert
    let add_rep = handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[PyRep::keyval([
            ("fleetName", PyRep::String("Test Fleet".into())),
            ("description", PyRep::String("Public roam".into())),
            ("inviteScope", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(add_rep, PyRep::Dict(_)), "AddFleetFinderAdvert must return PyRep::Dict (#732)");
    assert_eq!(add_rep.dict_get("fleetName").and_then(PyRep::as_str), Some("Test Fleet"));
    assert_eq!(add_rep.dict_get("leaderID").and_then(PyRep::as_i64), Some(boss_id));
    let leader_kv = add_rep
        .dict_get("leader")
        .expect("AddFleetFinderAdvert must include leader KeyVal (#775)");
    assert_eq!(leader_kv.dict_get("charID").and_then(PyRep::as_i64), Some(boss_id));
    assert_eq!(
        leader_kv.dict_get("warFactionID"),
        Some(&PyRep::None),
        "leader KeyVal must include warFactionID for PanelFleetFinder.GetFleetEntry (#805, #807)"
    );
    let raw_add_notifs = ctx.drain_notifications_for_char(boss_id);
    let add_notifs: Vec<_> = raw_add_notifs
        .iter()
        .filter(|n| n.event == "OnFleetFinderAdvertChanged")
        .collect();
    assert!(!add_notifs.is_empty(), "expected OnFleetFinderAdvertChanged after AddFleetFinderAdvert");
    assert!(add_notifs.iter().all(|n| n.args.is_empty()), "OnFleetFinderAdvertChanged must take 0 args (#720)");
    let opt_add_notif = raw_add_notifs
        .iter()
        .find(|n| n.event == "OnFleetOptionsChanged")
        .expect("expected OnFleetOptionsChanged after AddFleetFinderAdvert (#774)");
    assert_eq!(opt_add_notif.args.len(), 2);
    assert_eq!(opt_add_notif.args[0].dict_get("isRegistered"), Some(&PyRep::Bool(false)));
    assert_eq!(opt_add_notif.args[1].dict_get("isRegistered"), Some(&PyRep::Bool(true)));

    // Re-calling AddFleetFinderAdvert when already registered must still emit OnFleetOptionsChanged (#809)
    let readd_rep = handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[PyRep::keyval([
            ("fleetName", PyRep::String("Test Fleet".into())),
            ("description", PyRep::String("Public roam".into())),
            ("inviteScope", PyRep::Integer(1)),
            ("allowedEntities", PyRep::List(vec![PyRep::Integer(98_000_701)])),
            ("public_allowedEntities", PyRep::List(vec![PyRep::Integer(98_000_702)])),
            ("membergroups_allowedEntities", PyRep::List(vec![PyRep::Integer(98_000_703)])),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        readd_rep.dict_get("allowedEntities"),
        Some(&PyRep::List(vec![
            PyRep::Integer(98_000_701),
            PyRep::Integer(98_000_702),
            PyRep::Integer(98_000_703),
        ]))
    );
    let readd_notifs = ctx.drain_notifications_for_char(boss_id);
    let readd_opt = readd_notifs
        .iter()
        .find(|n| n.event == "OnFleetOptionsChanged")
        .expect("re-calling AddFleetFinderAdvert must still emit OnFleetOptionsChanged (#809)");
    assert_eq!(readd_opt.args[1].dict_get("isRegistered"), Some(&PyRep::Bool(true)));

    // V24.01 UpdateAdvertInfo(numMembers, allowedDiffKeyVal)
    let diff = PyRep::keyval([
        (
            "membergroupsToAddToAllowed",
            PyRep::List(vec![PyRep::Integer(98_000_111), PyRep::Integer(98_000_222)]),
        ),
        ("publicToAddToAllowed", PyRep::List(vec![PyRep::Integer(1)])),
    ]);
    handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "UpdateAdvertInfo",
        &[PyRep::Integer(7), diff],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let upd_notifs: Vec<_> = ctx
        .drain_notifications_for_char(boss_id)
        .into_iter()
        .filter(|n| n.event == "OnFleetFinderAdvertChanged")
        .collect();
    assert!(!upd_notifs.is_empty(), "expected OnFleetFinderAdvertChanged after UpdateAdvertInfo");
    assert!(upd_notifs.iter().all(|n| n.args.is_empty()), "OnFleetFinderAdvertChanged must take 0 args (#720)");

    // Remove one entity via V24.01 allowedDiffKeyVal
    let diff_remove = PyRep::keyval([(
        "membergroupsToRemoveFromAllowed",
        PyRep::List(vec![PyRep::Integer(98_000_111)]),
    )]);
    handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "UpdateAdvertInfo",
        &[PyRep::Integer(8), diff_remove],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Check DB snapshot persistence and allowed entities
    let persisted = ctx
        .db
        .with_snapshot(|snap| snap.fleet_and_social_ops.fleet_adverts.contains_key(&fleet_id));
    assert!(persisted);
    {
        let ex = ctx.fleet_extras.lock().unwrap();
        let adv = ex.adverts.get(&fleet_id).unwrap();
        assert!(adv.allowed_entities.contains(&98_000_222));
        assert!(adv.allowed_entities.contains(&1));
        assert!(!adv.allowed_entities.contains(&98_000_111));
        assert_eq!(adv.invite_scope, 1);
    }

    // V24.01 UpdateAdvertAllowedEntities(Dict/KeyVal)
    handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "UpdateAdvertAllowedEntities",
        &[PyRep::keyval([
            (
                "membergroupsAllowedEntities",
                PyRep::List(vec![PyRep::Integer(98_000_333)]),
            ),
            ("publicAllowedEntities", PyRep::List(vec![])),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let adv_entities = ctx
        .fleet_extras
        .lock()
        .unwrap()
        .adverts
        .get(&fleet_id)
        .unwrap()
        .allowed_entities
        .clone();
    assert_eq!(adv_entities, HashSet::from([98_000_333]));

    let _ = ctx.drain_notifications_for_char(boss_id);
    handle_finder_and_tactical(
        &ctx,
        &mut session,
        "fleetProxy",
        "RemoveFleetFinderAdvert",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let raw_rem_notifs = ctx.drain_notifications_for_char(boss_id);
    let opt_rem_notif = raw_rem_notifs
        .iter()
        .find(|n| n.event == "OnFleetOptionsChanged")
        .expect("expected OnFleetOptionsChanged after RemoveFleetFinderAdvert (#774)");
    assert_eq!(opt_rem_notif.args.len(), 2);
    assert_eq!(opt_rem_notif.args[0].dict_get("isRegistered"), Some(&PyRep::Bool(true)));
    assert_eq!(opt_rem_notif.args[1].dict_get("isRegistered"), Some(&PyRep::Bool(false)));
}

#[tokio::test]
async fn watchlist_unsubscribed_remove_all_and_setup_wing_expansion_edges() {
    use crate::services::fleet_social::handle;

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_500);
    let (fleet_id, mut boss) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Unknown method returns None
    let unknown_op = format!("{}{}", "NoSuch", "WatchlistMethod");
    assert!(handle(&ctx, &mut boss, "fleetObjectHandler", &unknown_op, &[], &kw)
        .await
        .is_none());

    // RegisterForDamageUpdates when NOT in fleet -> watched:false; GetWatchlist -> NotInFleet; RemoveFromWatchlist -> watched:false
    let mut outsider = SessionState::new(99_000_777);
    outsider.user_id = Some(99_000_777);
    outsider.character_id = Some(90_000_099);
    let reg_outside = handle_fleet_watchlist_ops(&ctx, &mut outsider, "RegisterForDamageUpdates", &[PyRep::Integer(90_000_001)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reg_outside.dict_get("watched"), Some(&PyRep::Bool(false)));
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut outsider, "GetWatchlist", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut outsider, "RemoveFromWatchlist", &[PyRep::Integer(90_000_001)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // Single integer arg on RegisterForDamageUpdates + RemoveFromWatchlist with fav_ids second arg removing last item -> watched:false
    let reg_one = handle_fleet_watchlist_ops(&ctx, &mut boss, "RegisterForDamageUpdates", &[PyRep::Integer(90_000_002)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reg_one.dict_get("watched"), Some(&PyRep::Bool(true)));

    let rem_all = handle_fleet_watchlist_ops(
        &ctx,
        &mut boss,
        "RemoveFromWatchlist",
        &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::List(vec![])],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rem_all.dict_get("watched"), Some(&PyRep::Bool(false)));

    // StoreFleetSetup via kwargs + LoadFleetSetup expanding wings up to cap of 5 and emitting notifications
    let mut setup_kw = HashMap::new();
    setup_kw.insert("setupName".into(), PyRep::String("WideWingSetup".into()));
    setup_kw.insert("motd".into(), PyRep::String("KwargsMOTD".into()));
    setup_kw.insert("isFreeMove".into(), PyRep::Bool(true));
    setup_kw.insert("maxSize".into(), PyRep::Integer(120));
    setup_kw.insert(
        "wings".into(),
        PyRep::List(
            (1..=6)
                .map(|w| {
                    PyRep::keyval([
                        ("name", PyRep::String(format!("Wing{w}"))),
                        ("squads", PyRep::List(vec![PyRep::String(format!("Squad{w}"))])),
                    ])
                })
                .collect(),
        ),
    );
    let stored = handle(&ctx, &mut boss, "fleetMgr", "StoreFleetSetup", &[], &setup_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.dict_get("wingCount"), Some(&PyRep::Integer(6)));

    // FC role (even non-boss if FleetCommander) and NotInFleet/FleetNotFound checks on LoadFleetSetup
    let loaded = handle(
        &ctx,
        &mut boss,
        "fleetMgr",
        "LoadFleetSetup",
        &[PyRep::String("WideWingSetup".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.dict_get("motd"), Some(&PyRep::String("KwargsMOTD".into())));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets[&fleet_id].wings.len(), 5);
    });

    let mut no_fleet = boss.clone();
    no_fleet.fleet_id = None;
    assert_eq!(
        handle(&ctx, &mut no_fleet, "fleetMgr", "LoadFleetSetup", &[PyRep::String("WideWingSetup".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    let mut bad_fleet = boss.clone();
    bad_fleet.fleet_id = Some(999_999_999);
    assert_eq!(
        handle(&ctx, &mut bad_fleet, "fleetMgr", "LoadFleetSetup", &[PyRep::String("WideWingSetup".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetNotFound"
    );
}

#[tokio::test]
async fn finder_advert_auto_join_and_formation_kwargs_reconnect_edges() {
    use crate::services::fleet_social::handle;

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_600);
    let (fleet_id, mut boss) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Reconnect / GetFleetID clears stale session.fleet_id when fleet no longer exists or caller removed
    let mut stale = SessionState::new(777_111);
    stale.user_id = Some(777_111);
    stale.character_id = Some(90_000_088);
    stale.fleet_id = Some(888_888_888);
    let recon = handle(&ctx, &mut stale, "fleetObjectHandler", "Reconnect", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recon.dict_get("fleetID"), Some(&PyRep::None));
    assert_eq!(stale.fleet_id, None);

    // parse_formation_input kwargs + spacing/size clamping
    let mut fkw = HashMap::new();
    fkw.insert("formation".into(), PyRep::String("diamond".into()));
    assert_eq!(parse_formation_input(&[], &fkw), (4, "Diamond"));
    let clamped = handle(
        &ctx,
        &mut boss,
        "fleetMgr",
        "SetFleetFormationSettings",
        &[PyRep::Integer(2), PyRep::Real(-10.0), PyRep::Real(999_999.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(clamped.dict_get("formationSpacing"), Some(&PyRep::Real(0.0)));
    assert_eq!(clamped.dict_get("formationSize"), Some(&PyRep::Real(100_000.0)));

    // ApplyToJoinFleet guards: AlreadyInFleet, InvalidArgument, FleetNotFound, FleetAdvertNotFound
    assert_eq!(
        handle(&ctx, &mut boss, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyInFleet"
    );
    let mut joiner = SessionState::new(90_000_071);
    joiner.user_id = Some(90_000_071);
    joiner.character_id = Some(90_000_071);
    joiner.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = 90_000_071;
        ch.user_id = 90_000_071;
        ch.fleet_id = None;
        snap.characters.insert(90_000_071, ch);
    });
    assert_eq!(
        handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(0)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(999_111_222)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetAdvertNotFound"
    );

    // Public auto_join advert + allowed_entities gating + auto_join immediate entry
    let _ = handle(
        &ctx,
        &mut boss,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("Open Auto Fleet".into()),
            PyRep::String("Auto".into()),
            PyRep::Integer(8),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut boss,
        "fleetProxy",
        "UpdateAdvertAllowedEntities",
        &[PyRep::List(vec![PyRep::Integer(99_999_999)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetJoinDenied"
    );
    let _ = handle(
        &ctx,
        &mut boss,
        "fleetProxy",
        "UpdateAdvertAllowedEntities",
        &[PyRep::List(vec![])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    let joined_rep = handle(&ctx, &mut joiner, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(joined_rep.dict_get("joined"), Some(&PyRep::Bool(true)));
    assert_eq!(joiner.fleet_id, Some(fleet_id));

    // FleetFull check when max_size equals current member count
    let cur_len = ctx.db.with_snapshot(|snap| snap.fleets[&fleet_id].members.len());
    ctx.fleet_extras
        .lock()
        .unwrap()
        .max_sizes
        .insert(fleet_id, cur_len as u16);
    let mut joiner2 = joiner.clone();
    joiner2.character_id = Some(90_000_072);
    joiner2.fleet_id = None;
    assert_eq!(
        handle(&ctx, &mut joiner2, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetFull"
    );

    // Clear advert from stale/not-in-fleet vs valid FleetNotFound clearing session.fleet_id
    let mut gone = boss.clone();
    gone.fleet_id = Some(444_555_666);
    assert_eq!(
        handle(&ctx, &mut gone, "fleetProxy", "RemoveFleetFinderAdvert", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetNotFound"
    );
    assert_eq!(gone.fleet_id, None);
}
