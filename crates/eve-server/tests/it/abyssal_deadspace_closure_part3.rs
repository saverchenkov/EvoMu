//! Continuation of Abyssal Deadspace closure tests:
//! - Fleet-only Frigate/Destroyer activation and bound `fleet_id` enforcement (`BF-01`)
//! - Downtime evacuation purge of abandoned and orphaned drones in Abyssal pockets (`NBF-02`)
//! - Monotonic `WorldRepository::instance_id` isolation in the ballpark registry (`NBF-01`)

#![forbid(unsafe_code)]

use crate::abyssal_deadspace_closure::{
    login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T1_ELEC, SYS_08_LASHESIH,
};
use crate::common;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use eve_server::services::pve_special_sites::with_pve_special_reg;
use eve_server::system_actor::try_remove_ball_for_db;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_abyssal_frigate_destroyer_fleet_only_and_bound_fleet_id() {
    eve_testkit::attest(&[
        "feat:pve.abyssal-entry-ship-limits",
        "feat:pve.abyssal-coop-frigate-destroyer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;

    let (char1, char2) = (90_000_001_i64, 90_000_002_i64);
    let (ship1, ship2) = (640_000_001_i64, 640_000_002_i64);
    let base = DVec3::new(20_000_000.0, 2.0e9, 0.0);
    let mut sess1 = login_pilot_in_space(&ctx, 888_101, "demo", "demo-secret", char1, ship1, 587, sys_id, base).await;
    let mut sess2 = login_pilot_in_space(&ctx, 888_102, "other", "other-secret", char2, ship2, 587, sys_id, base + DVec3::new(300.0, 0.0, 0.0)).await;

    // 1. Solo fleetless Frigate (587) rejected on both AbyssalEntranceDeployment and direct AbyssalEntranceGateActivation
    seed_cargo_item(&ctx, 740_100_001, FIL_T1_ELEC, char1, ship1, 3);
    let err_dep_fg = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_001)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_dep_fg.code, "NotInFleet");
    let err_act_fg = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(740_100_001)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_act_fg.code, "NotInFleet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&740_100_001].quantity), 3, "Filaments preserved on NotInFleet");

    // 2. Solo fleetless Destroyer (16236) rejected on both deployment and direct inventory activation
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 16_236, sys_id, base).await;
    seed_cargo_item(&ctx, 740_100_002, FIL_T1_ELEC, char1, ship1, 2);
    let err_dep_dd = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_002)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_dep_dd.code, "NotInFleet");
    let err_act_dd = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(740_100_002)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_act_dd.code, "NotInFleet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&740_100_002].quantity), 2);

    // 3. Solo fleetless Cruiser (621) IS allowed without a fleet (maxShips == 1)
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 621, sys_id, base).await;
    seed_cargo_item(&ctx, 740_100_003, FIL_T1_ELEC, char1, ship1, 1);
    let dep_cr = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_003)], &kw, None)
        .await
        .expect("Solo Cruiser deployment allowed without fleet");
    assert_eq!(dep_cr.dict_get("maxShips"), Some(&PyRep::Integer(1)));

    // 4. Opener in fleet 99_301 deploys Frigate trace (bound fleet_id == Some(99_301)):
    //    - If opener leaves fleet before activating trace -> NotInFleet
    //    - If opener switches to a different fleet 99_302 -> AccessDenied
    //    - Fleetmate in bound fleet 99_301 can still enter even while opener is outside 99_301
    //    - Opener rejoining 99_301 enters the same pocket
    move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 587, sys_id, base + DVec3::new(2_000_000.0, 0.0, 0.0)).await;
    move_pilot_to_system(&ctx, &mut sess2, char2, ship2, 587, sys_id, base + DVec3::new(2_000_300.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            99_301,
            FleetRecord { fleet_id: 99_301, boss_id: char1, members: vec![char1, char2], ..Default::default() },
        );
    });
    let dep_fg = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_001)], &kw, None)
        .await
        .expect("Fleet member Frigate deploy");
    let tr_id = dep_fg.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        with_pve_special_reg(&ctx, |reg| reg.abyssal_traces.get(&tr_id).and_then(|t| t.fleet_id)),
        Some(99_301)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&99_301).unwrap().members.retain(|&m| m != char1);
    });
    let err_opener_left = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_opener_left.code, "NotInFleet", "Opener who left fleet cannot activate Frigate trace");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            99_302,
            FleetRecord { fleet_id: 99_302, boss_id: char1, members: vec![char1], ..Default::default() },
        );
    });
    let err_opener_other_fleet = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_opener_other_fleet.code, "AccessDenied", "Opener in a different fleet cannot activate bound trace");

    let ent2 = ctx
        .dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
        .await
        .expect("Member of bound fleet 99_301 can enter even after opener left");
    let pid = ent2.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.remove(&99_302);
        snap.fleets.get_mut(&99_301).unwrap().members.push(char1);
    });
    let ent1 = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr_id)], &kw, None)
        .await
        .expect("Opener rejoining bound fleet 99_301 enters same pocket");
    assert_eq!(ent1.dict_get("pocketID").and_then(|v| v.as_i64()), Some(pid));
}

#[tokio::test]
async fn test_abyssal_downtime_evacuation_purges_abandoned_and_orphaned_drones() {
    eve_testkit::attest(&["feat:pve.abyssal-in-pocket-restrictions"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let sec_init = ctx.now_unix() % 86_400;
    let target_pre_dt = 11 * 3_600 - 300;
    ctx.advance_virtual_clock(if sec_init <= target_pre_dt { target_pre_dt - sec_init } else { 86_400 - sec_init + target_pre_dt });

    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;
    let (char1, ship1) = (90_000_001_i64, 641_000_001_i64);
    let mut sess1 = login_pilot_in_space(&ctx, 888_201, "demo", "demo-secret", char1, ship1, 621, sys_id, DVec3::new(0.0, 2.0e9, 0.0)).await;

    let bay_drone_id = 741_500_001_i64;
    let orphan_drone_id = 741_500_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bay_drone_id,
            InventoryItemRecord {
                item_id: bay_drone_id, type_id: 2454, owner_id: char1, location_id: ship1,
                flag: InventoryFlag::DroneBay, quantity: 1, singleton: true, version: 1, ..Default::default()
            },
        );
    });
    seed_cargo_item(&ctx, 741_500_010, FIL_T1_ELEC, char1, ship1, 1);
    let ent = ctx
        .dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(741_500_010)], &kw, None)
        .await
        .expect("Enter Abyssal pocket");
    let p_sys = ent.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let p_h = ctx.systems.get_or_spawn(p_sys);

    // Launch drone and abandon it (`owner_char_id = 0, parent_ship_id = 0, original_owner_char_id = char1`)
    ctx.dispatch_rpc(&mut sess1, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(bay_drone_id)])], &kw, None)
        .await
        .expect("LaunchDrones");
    let abandoned_ball_id = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.keys().copied().next().expect("launched drone ball")
    });
    ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdAbandonDrone", &[PyRep::List(vec![PyRep::Integer(abandoned_ball_id)])], &kw, None)
        .await
        .expect("CmdAbandonDrone");
    with_combat_runtime(&ctx, |rt| {
        let d = &rt.active_drones_in_space[&abandoned_ball_id];
        assert!(d.is_abandoned);
        assert_eq!(d.owner_char_id, 0);
        assert_eq!(d.parent_ship_id, 0);
        assert_eq!(d.original_owner_char_id, char1);
    });

    // Also place an orphaned drone item & ball (`owner_id = 0`) in `p_sys`
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            orphan_drone_id,
            InventoryItemRecord {
                item_id: orphan_drone_id, type_id: 2454, owner_id: 0, location_id: p_sys as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            },
        );
        snap.item_positions.insert(orphan_drone_id, [100.0, 0.0, 0.0]);
    });
    let _ = p_h
        .spawn_ball(Ball {
            id: orphan_drone_id, type_id: 2454, position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 400.0, inertia: 1.0, mass: 1.0, radius: 10.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        })
        .await;

    ctx.advance_virtual_clock(360);
    let dt_res = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
    assert_eq!(dt_res.get("is1100UtcWindow"), Some(&1));
    assert!(dt_res.get("evacuatedAbyssalPilots").copied().unwrap_or(0) >= 1);

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&abandoned_ball_id), "Abandoned drone purged from active_drones_in_space");
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char1].solar_system_id, sys_id);
        assert!(!snap.items.contains_key(&abandoned_ball_id), "Abandoned drone item purged");
        assert!(!snap.items.contains_key(&bay_drone_id), "Launched drone item purged");
        assert!(!snap.items.contains_key(&orphan_drone_id), "Orphaned drone item in abyss_sys purged");
    });
    assert!(p_h.get_ball_snapshot(abandoned_ball_id).await.unwrap().is_none(), "Abandoned drone ball removed");
    assert!(p_h.get_ball_snapshot(orphan_drone_id).await.unwrap().is_none(), "Orphaned drone ball removed");
    with_mut_combat_runtime(&ctx, |rt| rt.active_drones_in_space.clear());
}

#[tokio::test]
async fn test_world_repository_monotonic_instance_id_and_ballpark_isolation() {
    let r1 = Arc::new(WorldRepository::with_demo_seed());
    let r2 = Arc::new(WorldRepository::new_empty_with_snapshot_path(None).unwrap());
    let tmp = std::env::temp_dir().join(format!("evomu_repo_id_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let r3 = Arc::new(WorldRepository::open_sqlite(&tmp).unwrap());
    let _ = std::fs::remove_file(&tmp);

    assert!(r1.instance_id() > 0);
    assert!(r2.instance_id() > r1.instance_id());
    assert!(r3.instance_id() > r2.instance_id());

    let ctx_a = ServerContext::with_repository(r1.clone());
    let ctx_b = ServerContext::with_repository(r2.clone());
    let sys_id = SYS_08_LASHESIH;
    let ball_id = 742_900_001_i64;
    let h_a = ctx_a.systems.get_or_spawn(sys_id);
    let h_b = ctx_b.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ball_id, type_id: 47_458, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = h_a.spawn_ball(ball.clone()).await;
    let _ = h_b.spawn_ball(ball).await;

    try_remove_ball_for_db(&r1, sys_id, ball_id);
    assert!(h_a.get_ball_snapshot(ball_id).await.unwrap().is_none(), "Ball removed in r1 ballpark");
    assert!(h_b.get_ball_snapshot(ball_id).await.unwrap().is_some(), "Ball in r2 ballpark unaffected");
}
