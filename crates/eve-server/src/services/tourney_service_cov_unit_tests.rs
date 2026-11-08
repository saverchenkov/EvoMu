//! Focused coverage and mutation-killing unit tests for `tourney_service`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::sp6_lifecycle::PersistedSpaceObjectMeta;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn mk_sess(char_id: Option<i64>, sys_id: Option<u32>, is_gm: bool) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = char_id.map(|c| c as u64);
    s.character_id = char_id;
    s.solar_system_id = sys_id;
    if is_gm {
        s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    }
    s
}

fn py_ints(xs: &[i64]) -> Vec<PyRep> {
    xs.iter().copied().map(PyRep::Integer).collect()
}

async fn tm(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, "tourneyMgr", method, args, kwargs)
        .await
        .expect("tourneyMgr handled")
}

async fn tm_ok(ctx: &ServerContext, sess: &mut SessionState, method: &str, args: &[PyRep]) -> PyRep {
    tm(ctx, sess, method, args, &HashMap::new()).await.unwrap()
}

async fn tm_err(ctx: &ServerContext, sess: &mut SessionState, method: &str, args: &[PyRep]) -> String {
    tm(ctx, sess, method, args, &HashMap::new())
        .await
        .unwrap_err()
        .code
}

fn seed_pilot_and_ship(
    ctx: &ServerContext,
    cid: i64,
    name: &str,
    sys_id: u32,
    home_st_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.user_id = cid as u64;
        ch.name = name.into();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.home_station_id = home_st_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(cid, ch);
        if ship_id > 0 {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
}

#[tokio::test]
async fn create_match_variants_ban_limits_member_locks_and_query_fallbacks() {
    let ctx = make_ctx();
    let sys_jita = 30_000_142_u32;
    let (gm_cid, red_capt, blue_capt, blue_mem, outsider) =
        (980_001_i64, 980_002, 980_003, 980_004, 980_005);

    for (cid, name, sid, stid) in [
        (gm_cid, "GM", 810_001, 587_u32),       // Rifter = 2 pts
        (red_capt, "RedC", 810_002, 587_u32),   // Rifter = 2 pts
        (blue_capt, "BlueC", 810_003, 587_u32), // Rifter = 2 pts
        (blue_mem, "BlueM", 810_004, 620_u32),  // Osprey = 9 pts
        (outsider, "Out", 810_005, 587_u32),
    ] {
        seed_pilot_and_ship(&ctx, cid, name, sys_jita, 60_003_760, sid, stid);
    }

    // 1. Unauth session errors; GetMatchSystem / QueryShipList / MatchNotFound fallbacks
    let mut unauth = mk_sess(None, None, false);
    assert!(!tm_err(&ctx, &mut unauth, "AbortCountdown", &[]).await.is_empty());
    assert_eq!(tm_ok(&ctx, &mut unauth, "GetMatchSystem", &py_ints(&[999])).await, PyRep::Integer(0));
    let q_empty = tm_ok(&ctx, &mut unauth, "QueryShipList", &[]).await;
    assert!(!q_empty.dict_get("allowedShipList").and_then(|v| v.as_list()).unwrap().is_empty());
    assert_eq!(tm_err(&ctx, &mut unauth, "GetFancyDetails", &py_ints(&[404])).await, "MatchNotFound");
    assert_eq!(tm_err(&ctx, &mut unauth, "GetTeamMembers", &py_ints(&[404])).await, "MatchNotFound");

    // 2. CreateMatch via individual team/captain/system kwargs with `session.solar_system_id == None`
    let mut gm = mk_sess(Some(gm_cid), None, true);
    let mut red_s = mk_sess(Some(red_capt), Some(sys_jita), false);
    let mut blue_s = mk_sess(Some(blue_capt), Some(sys_jita), false);
    let mut out_s = mk_sess(Some(outsider), Some(sys_jita), false);

    let (match_id, tourney_id) = (7_101_i64, 55_i64);
    let mut cm_kw = HashMap::new();
    cm_kw.insert("redTeamID".into(), PyRep::Integer(1_000_009));
    cm_kw.insert("blueTeamID".into(), PyRep::Integer(1_000_125));
    cm_kw.insert("redCaptainID".into(), PyRep::Integer(red_capt));
    cm_kw.insert("blueCaptainID".into(), PyRep::Integer(blue_capt));
    cm_kw.insert("systemID".into(), PyRep::Integer(i64::from(sys_jita)));
    tm(&ctx, &mut gm, "CreateMatch", &py_ints(&[tourney_id, match_id, 10, 600, 30, 0]), &cm_kw)
        .await
        .unwrap();

    // AbortCountdown while in Setup (not Countdown) clears countdown_active without state notification
    assert_eq!(tm_ok(&ctx, &mut gm, "AbortCountdown", &py_ints(&[match_id])).await, PyRep::Bool(true));

    // 3. StartAutobanning positional args (len >= 5) with explicit pool and clamped banDurationSeconds
    let mut dur_kw = HashMap::new();
    dur_kw.insert("banDurationSeconds".into(), PyRep::Integer(1));
    tm(
        &ctx,
        &mut gm,
        "StartAutobanning",
        &[
            PyRep::Integer(match_id),
            PyRep::Integer(0),
            PyRep::Integer(red_capt),
            PyRep::Integer(blue_capt),
            PyRep::Integer(2),
            PyRep::List(py_ints(&[638])),
        ],
        &dur_kw,
    )
    .await
    .unwrap();

    // Outsider rejected on BanShip & BanShipType
    let ban_one = |id: i64, ships: &[i64]| vec![PyRep::Integer(id), PyRep::List(py_ints(ships))];
    assert_eq!(tm_err(&ctx, &mut out_s, "BanShip", &ban_one(1, &[588])).await, "AccessDenied");
    assert_eq!(tm_err(&ctx, &mut out_s, "BanShipType", &py_ints(&[0, 588])).await, "AccessDenied");

    // Red already has 1 autoban (638) with autoban_qty = 2:
    // - 2 new types [588, 589] exceeds cap 2 -> TourneyBanLimitExceeded
    // - 1 new type [588] reaches exact cap 2 -> succeeds; re-submitting [588] (0 new unique) succeeds
    assert_eq!(
        tm_err(&ctx, &mut red_s, "BanShip", &ban_one(1, &[588, 589])).await,
        "TourneyBanLimitExceeded"
    );
    tm_ok(&ctx, &mut red_s, "BanShip", &ban_one(1, &[588])).await;
    tm_ok(&ctx, &mut red_s, "BanShip", &ban_one(2, &[588])).await;

    // Blue captain using 2-arg `BanShipType` is forced to blue team even if passing requested_team=0
    tm_ok(&ctx, &mut blue_s, "BanShipType", &py_ints(&[0, 590])).await;
    tm_ok(&ctx, &mut blue_s, "BanShipType", &py_ints(&[0, 590])).await;

    // QueryShipList by `tourney_id` after FinalizeBans excludes banned ships; BanShip rejected afterwards
    tm_ok(&ctx, &mut gm, "FinalizeBans", &[]).await;
    let q_by_tid = tm_ok(&ctx, &mut gm, "QueryShipList", &py_ints(&[tourney_id])).await;
    let allowed = q_by_tid.dict_get("allowedShipList").and_then(|v| v.as_list()).unwrap();
    assert!(!allowed.contains(&PyRep::Integer(588)));
    assert_eq!(tm_err(&ctx, &mut red_s, "BanShip", &ban_one(3, &[591])).await, "TourneyBansFinalized");

    // 4. AddPlayer & ExecuteMemberChange: blue lock, max_points exceeded, existing player update, 1-arg no-op
    tm_ok(&ctx, &mut gm, "LockMemberList", &py_ints(&[1])).await;
    assert_eq!(tm_err(&ctx, &mut gm, "AddPlayer", &py_ints(&[1, blue_mem])).await, "MemberListLocked");
    assert_eq!(tm_err(&ctx, &mut blue_s, "ExecuteMemberChange", &py_ints(&[1, blue_mem])).await, "MemberListLocked");
    tm_ok(&ctx, &mut gm, "UnlockMemberList", &py_ints(&[1])).await;

    // Adding blue_mem (9 pts) + blue_capt (2 pts) = 11 > max_points (10) -> TourneyMaxPointsExceeded
    assert_eq!(tm_err(&ctx, &mut gm, "AddPlayer", &py_ints(&[1, blue_mem])).await, "TourneyMaxPointsExceeded");
    assert_eq!(
        tm_err(&ctx, &mut blue_s, "ExecuteMemberChange", &py_ints(&[1, blue_mem])).await,
        "TourneyMaxPointsExceeded"
    );
    assert!(!tm_err(&ctx, &mut out_s, "ExecuteMemberChange", &py_ints(&[1, outsider])).await.is_empty());
    assert_eq!(tm_ok(&ctx, &mut red_s, "ExecuteMemberChange", &py_ints(&[match_id])).await, PyRep::None);

    // Re-adding existing red_capt updates existing slot without duplicating
    tm_ok(&ctx, &mut gm, "AddPlayer", &py_ints(&[0, red_capt])).await;
    tm_ok(&ctx, &mut red_s, "ExecuteMemberChange", &py_ints(&[0, red_capt])).await;
    assert_eq!(ctx.db.with_snapshot(|s| load_store(s).matches[&match_id].red_players.len()), 1);
}

#[tokio::test]
async fn pregame_points_cleanup_radius_boundary_teleport_return_and_fleet_sync() {
    let ctx = make_ctx();
    let (sys_arena, sys_home) = (30_000_142_u32, 30_002_187_u32);
    let st_amarr = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_home)
        .map(|s| s.station_id)
        .unwrap_or(60_008_494);

    let (gm_cid, red_p1, red_p2, blue_p1, blue_extra, ghost_pilot) =
        (981_001_i64, 981_002, 981_003, 981_004, 981_005, 981_006);

    for (cid, name, sys, sid, stid) in [
        (gm_cid, "RefGM", sys_arena, 820_001, 587_u32),
        (red_p1, "Red1", sys_home, 820_002, 620_u32),   // 9 pts, starts in Amarr
        (red_p2, "Red2", sys_home, 820_003, 587_u32),   // 2 pts
        (blue_p1, "Blue1", sys_home, 820_004, 587_u32), // 2 pts
        (blue_extra, "BlueEx", sys_home, 820_005, 587_u32),
    ] {
        seed_pilot_and_ship(&ctx, cid, name, sys, st_amarr, sid, stid);
    }

    let mut gm = mk_sess(Some(gm_cid), Some(sys_arena), true);
    let mut outsider = mk_sess(Some(blue_extra), Some(sys_arena), false);

    // TeleportRefHome when NO match exists yet succeeds for GM
    assert_eq!(tm_ok(&ctx, &mut gm, "TeleportRefHome", &py_ints(&[999])).await, PyRep::Bool(true));
    gm.solar_system_id = Some(sys_arena);
    gm.station_id = None;

    // Register arena system with combat_beacons & warp_bubbles for CleanupGrid coverage
    register_tourney_setup(
        &ctx,
        88,
        "Cov Cup",
        i64::from(sys_arena),
        TourneyArenaSystem {
            combat_beacons: vec![830_001],
            warp_bubbles: vec![830_002],
        },
    )
    .unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_arena);
    let _ = sys_h
        .spawn_ball(Ball {
            id: 830_001,
            type_id: 2977,
            position: DVec3::new(0.0, 500_000.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(830_002, [0.0, -500_000.0, 0.0]);
    });

    // CreateMatch via positional args[6], args[7], args[8] with max_points = 10
    let mid = 7_201_i64;
    tm_ok(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(88),
            PyRep::Integer(mid),
            PyRep::Integer(10),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Tuple(py_ints(&[1_000_009, 1_000_125])),
            PyRep::Tuple(py_ints(&[red_p1, blue_p1])),
            PyRep::Integer(i64::from(sys_arena)),
        ],
    )
    .await;

    // CleanupGrid: outsider denied; exact 250_000m boundary removed, 250_001m / anchoring / positionless preserved
    assert!(!tm_err(&ctx, &mut outsider, "CleanupGrid", &py_ints(&[mid])).await.is_empty());
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, pos_opt) in [
            (840_001, 26_557_u32, Some([0.0, 750_000.0, 0.0])),
            (840_002, 26_557_u32, Some([0.0, 750_001.0, 0.0])),
            (840_003, 26_557_u32, None),
            (840_004, 587_u32, Some([-50_000.0, 0.0, 0.0])),
            (840_005, 23_u32, Some([0.0, -500_000.0, 0.0])),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: red_p1,
                    location_id: i64::from(sys_arena),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if let Some(p) = pos_opt {
                snap.item_positions.insert(iid, p);
            }
        }
        snap.sp6_lifecycle.space_object_meta.insert(
            840_005,
            PersistedSpaceObjectMeta {
                anchor_start_unix: Some(1_000),
                ..Default::default()
            },
        );
    });
    tm_ok(&ctx, &mut gm, "CleanupGrid", &py_ints(&[mid])).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&840_001));
        for kept in [840_002, 840_003, 840_004, 840_005] {
            assert!(snap.items.contains_key(&kept), "kept={kept}");
        }
    });

    // Fleet sync: red unlocked -> adds red_p2 (9 + 2 = 11 > 10) -> UpdateFleetDetails fails; blue locked -> skips blue_extra
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        store.matches.get_mut(&mid).unwrap().member_list_locked_blue = true;
        save_store(snap, &store, 1_000);
        snap.fleets.insert(
            901,
            FleetRecord {
                fleet_id: 901,
                boss_id: red_p1,
                members: vec![red_p1, red_p2],
                ..Default::default()
            },
        );
        snap.fleets.insert(
            902,
            FleetRecord {
                fleet_id: 902,
                boss_id: blue_p1,
                members: vec![blue_p1, blue_extra],
                ..Default::default()
            },
        );
    });
    assert_eq!(tm_err(&ctx, &mut gm, "UpdateFleetDetails", &py_ints(&[mid])).await, "TourneyMaxPointsExceeded");

    // Seed red_p2 directly in roster (11 > 10) to hit TourneyMaxPointsExceeded in CompletePregameChecks & TeleportPlayers
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        store.matches.get_mut(&mid).unwrap().red_players.push(TourneyPilot {
            character_id: red_p2,
            ship_type_id: 587,
            ship_id: 820_003,
            points: 2,
        });
        save_store(snap, &store, 1_001);
    });
    assert_eq!(tm_err(&ctx, &mut gm, "CompletePregameChecks", &py_ints(&[mid])).await, "TourneyMaxPointsExceeded");
    assert_eq!(tm_err(&ctx, &mut gm, "TeleportPlayers", &py_ints(&[0, 0])).await, "TourneyMaxPointsExceeded");

    // Raise max_points to exact 11, seed custom pilot error -> OverrideError removes specific error codes
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let m = store.matches.get_mut(&mid).unwrap();
        m.max_points = 11;
        m.pilot_errors.insert(red_p1, vec!["TourneyMaxPointsExceeded".into(), "CustomErr".into()]);
        save_store(snap, &store, 1_002);
    });
    for pid in [red_p1, red_p2] {
        tm_ok(&ctx, &mut gm, "OverrideError", &[PyRep::Integer(pid), PyRep::String("TourneyMaxPointsExceeded".into())]).await;
    }
    assert_eq!(tm_err(&ctx, &mut gm, "CompletePregameChecks", &py_ints(&[mid])).await, "CustomErr");
    tm_ok(&ctx, &mut gm, "OverrideError", &[PyRep::Integer(red_p1), PyRep::String("CustomErr".into())]).await;
    tm_ok(&ctx, &mut gm, "CompletePregameChecks", &py_ints(&[mid])).await;
    tm_ok(&ctx, &mut gm, "UpdateFleetDetails", &py_ints(&[mid])).await;
    tm_ok(&ctx, &mut gm, "OverrideFleetBossID", &py_ints(&[1, blue_p1])).await;

    // Register beyonce bound object & make blue_p1 a referee so session-update & bound-object branches run
    ctx.bound_objects.write().unwrap().insert(
        501,
        BoundObjectEntry {
            bound_id: 501,
            session_id: 1,
            user_id: blue_p1 as u64,
            character_id: blue_p1,
            service: "beyonce".into(),
            object_id: i64::from(sys_home),
            oid_str: "oid=(1, 501)".into(),
            ref_id: 1,
        },
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let m = store.matches.get_mut(&mid).unwrap();
        m.referees.push(blue_p1);
        m.blue_players.push(TourneyPilot {
            character_id: ghost_pilot,
            ship_type_id: 0,
            ship_id: 0,
            points: 0,
        });
        save_store(snap, &store, 1_003);
        if !snap.accounts.values().any(|a| a.user_id == blue_p1 as u64) {
            if let Some(mut acc) = snap.accounts.values().next().cloned() {
                acc.user_id = blue_p1 as u64;
                acc.username = format!("blue_ref_{blue_p1}");
                snap.accounts.insert(acc.username.clone(), acc);
            }
        }
    });

    let buid = ctx.db.with_snapshot(|s| s.characters.get(&blue_p1).map(|c| c.user_id).unwrap_or(blue_p1 as u64));
    ctx.db.acquire_online_lock(buid, 1, Some(blue_p1), true, 1_800_000_000).unwrap();
    let mut blue_ref_sess = mk_sess(Some(blue_p1), Some(sys_home), false);
    tm_ok(&ctx, &mut blue_ref_sess, "TeleportPlayers", &py_ints(&[1, 1])).await;
    assert_eq!(blue_ref_sess.solar_system_id, Some(sys_arena));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&501));
    assert!(ctx
        .drain_notifications_for_char(blue_p1)
        .iter()
        .any(|n| n.event == "OnMachoObjectDisconnect"));
    let arena_bid = ctx
        .bind_object(&blue_ref_sess, "beyonce", i64::from(sys_arena))
        .expect("rebind beyonce in arena");
    tm_ok(&ctx, &mut blue_ref_sess, "TeleportPlayers", &py_ints(&[1, 1])).await;

    // 1-arg ReturnTeamPlayers returns blue_p1 from sys_arena back to st_amarr in sys_home (`home_sys != old_sys`)
    tm_ok(&ctx, &mut blue_ref_sess, "ReturnTeamPlayers", &py_ints(&[1])).await;
    assert_eq!(blue_ref_sess.station_id, Some(st_amarr));
    assert_eq!(blue_ref_sess.solar_system_id, Some(sys_home));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&arena_bid));
    assert!(ctx
        .drain_notifications_for_char(blue_p1)
        .iter()
        .any(|n| n.event == "OnMachoObjectDisconnect"));

    // 3-arg RemovePlayer on blue team
    tm_ok(&ctx, &mut gm, "RemovePlayer", &py_ints(&[mid, 1, ghost_pilot])).await;
}
