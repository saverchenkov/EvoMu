//! Additional edge-case and branch-coverage unit tests for `tourney_service.rs`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
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

fn seed_pilot(ctx: &ServerContext, cid: i64, sys_id: u32, ship_id: i64, ship_type: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.user_id = cid as u64;
        ch.name = format!("Pilot_{cid}");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.home_station_id = 60_003_760;
        ch.active_ship_id = ship_id;
        snap.characters.insert(cid, ch);
        if !snap.accounts.values().any(|a| a.user_id == cid as u64) {
            if let Some(mut acc) = snap.accounts.values().next().cloned() {
                acc.user_id = cid as u64;
                acc.username = format!("tourney_pilot_{cid}");
                snap.accounts.insert(acc.username.clone(), acc);
            }
        }
        if ship_id > 0 {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type,
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
    let _ = ctx
        .db
        .acquire_online_lock(cid as u64, cid as u64, Some(cid), true, 1_775_400_000);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_member_change_guards_ban_slots_and_autoban_defaults() {
    let ctx = make_ctx();
    let sys_jita = 30_000_142_u32;
    let (gm_cid, red_c, blue_c, pilot_a, outsider) =
        (981_001_i64, 981_002, 981_003, 981_004, 981_005);
    for (cid, sid, stype) in [
        (gm_cid, 811_001, 587_u32),
        (red_c, 811_002, 587_u32),
        (blue_c, 811_003, 587_u32),
        (pilot_a, 811_004, 587_u32),
        (outsider, 811_005, 587_u32),
    ] {
        seed_pilot(&ctx, cid, sys_jita, sid, stype);
    }

    let mut gm = mk_sess(Some(gm_cid), Some(sys_jita), true);
    let mut red_s = mk_sess(Some(red_c), Some(sys_jita), false);
    let mut blue_s = mk_sess(Some(blue_c), Some(sys_jita), false);
    let mut out_s = mk_sess(Some(outsider), Some(sys_jita), false);
    let kw = HashMap::new();

    // Non-tourneyMgr returns None
    assert!(
        handle(&ctx, &mut gm, "charMgr", "GetCharacterInfo", &[], &kw)
            .await
            .is_none()
    );

    // Create match (match_id = 7201)
    let mut cm_kw = HashMap::new();
    cm_kw.insert("redTeamID".into(), PyRep::Integer(1_000_009));
    cm_kw.insert("blueTeamID".into(), PyRep::Integer(1_000_125));
    cm_kw.insert("redCaptainID".into(), PyRep::Integer(red_c));
    cm_kw.insert("blueCaptainID".into(), PyRep::Integer(blue_c));
    cm_kw.insert("systemID".into(), PyRep::Integer(i64::from(sys_jita)));
    tm(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(77),
            PyRep::Integer(7201),
            PyRep::Integer(20),
            PyRep::Integer(600),
            PyRep::Integer(15),
            PyRep::Integer(0),
        ],
        &cm_kw,
    )
    .await
    .unwrap();

    // ExecuteMemberChange: InvalidTeamIndex
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "ExecuteMemberChange",
            &[PyRep::Integer(2), PyRep::Integer(pilot_a)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidTeamIndex"
    );

    // ExecuteMemberChange: red captain trying to modify blue team (1) -> AccessDenied
    assert_eq!(
        tm(
            &ctx,
            &mut red_s,
            "ExecuteMemberChange",
            &[PyRep::Integer(1), PyRep::Integer(pilot_a)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // ExecuteMemberChange: 1-arg form allowed for captain (no-op) vs denied for outsider
    assert_eq!(
        tm(
            &ctx,
            &mut red_s,
            "ExecuteMemberChange",
            &[PyRep::Integer(0)],
            &kw
        )
        .await
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        tm(
            &ctx,
            &mut out_s,
            "ExecuteMemberChange",
            &[PyRep::Integer(0)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // ExecuteMemberChange: char_id <= 0 no-op succeeds
    assert_eq!(
        tm(
            &ctx,
            &mut red_s,
            "ExecuteMemberChange",
            &[PyRep::Integer(0), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap(),
        PyRep::None
    );

    // Blue captain using BanShip works on blue team slot before autobanning fills all 3 slots
    assert_eq!(
        tm(
            &ctx,
            &mut blue_s,
            "BanShip",
            &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(588)])],
            &kw,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // StartAutobanning with default totalNumBans=3 + huge banDurationSeconds clamped to 3600
    let mut ab_kw = HashMap::new();
    ab_kw.insert("banDurationSeconds".into(), PyRep::Integer(99_999));
    let before_ban = ctx.now_unix();
    let ab = tm(
        &ctx,
        &mut gm,
        "StartAutobanning",
        &[PyRep::Integer(7201)],
        &ab_kw,
    )
    .await
    .unwrap();
    assert_eq!(ab, PyRep::Bool(true));
    let ban_notes = ctx.drain_notifications_for_char(gm_cid);
    let perform_ban = ban_notes
        .iter()
        .find(|n| n.event == "OnTournamentPerformBan")
        .expect("OnTournamentPerformBan notification emitted");
    assert!(matches!(
        perform_ban.args.get(3),
        Some(PyRep::Integer(deadline))
            if *deadline >= (before_ban + 3_600) as i64
                && *deadline <= (ctx.now_unix() + 3_600) as i64
    ));

    // Additional BanShip once autobanning filled all 3 team slots -> TourneyBanLimitExceeded
    assert_eq!(
        tm(
            &ctx,
            &mut blue_s,
            "BanShip",
            &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(599_999)])],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "TourneyBanLimitExceeded"
    );

    // GetFancyDetails & GetTeamMembers denied to non-referee during Setup/Banning
    assert_eq!(
        tm(
            &ctx,
            &mut out_s,
            "GetFancyDetails",
            &[PyRep::Integer(7201)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        tm(
            &ctx,
            &mut out_s,
            "GetTeamMembers",
            &[PyRep::Integer(7201)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pregame_checks_countdown_abort_complete_reset_and_restriction_helpers() {
    let ctx = make_ctx();
    let sys_jita = 30_000_142_u32;
    let (gm_cid, red_c, blue_c, banned_pilot, heavy_pilot, ref2) =
        (982_001_i64, 982_002, 982_003, 982_004, 982_005, 982_006);
    for (cid, sid, stype) in [
        (gm_cid, 812_001, 587_u32),
        (red_c, 812_002, 587_u32),        // 2 pts
        (blue_c, 812_003, 587_u32),       // 2 pts
        (banned_pilot, 812_004, 585_u32), // Slasher (2 pts, banned below)
        (heavy_pilot, 812_005, 620_u32),  // Osprey = 9 pts -> exceeds max_points 5
        (ref2, 812_006, 587_u32),
    ] {
        seed_pilot(&ctx, cid, sys_jita, sid, stype);
    }

    let mut gm = mk_sess(Some(gm_cid), Some(sys_jita), true);
    let mut red_s = mk_sess(Some(red_c), Some(sys_jita), false);
    let mut ref2_s = mk_sess(Some(ref2), Some(sys_jita), false);
    let kw = HashMap::new();

    let mut cm_kw = HashMap::new();
    cm_kw.insert("redTeamID".into(), PyRep::Integer(1_000_009));
    cm_kw.insert("blueTeamID".into(), PyRep::Integer(1_000_125));
    cm_kw.insert("redCaptainID".into(), PyRep::Integer(red_c));
    cm_kw.insert("blueCaptainID".into(), PyRep::Integer(blue_c));
    cm_kw.insert("systemID".into(), PyRep::Integer(i64::from(sys_jita)));
    tm(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(88),
            PyRep::Integer(7301),
            PyRep::Integer(5), // max_points = 5
            PyRep::Integer(600),
            PyRep::Integer(15),
            PyRep::Integer(0),
        ],
        &cm_kw,
    )
    .await
    .unwrap();

    // Add banned pilot to red and heavy pilot to blue
    tm(
        &ctx,
        &mut gm,
        "AddPlayer",
        &[PyRep::Integer(0), PyRep::Integer(banned_pilot)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "AddPlayer",
            &[PyRep::Integer(1), PyRep::Integer(heavy_pilot)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "TourneyMaxPointsExceeded"
    );
    // Add heavy_pilot in a lightweight ship first, then swap back to 620 so CompletePregameChecks catches both
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&812_005) {
            it.type_id = 587;
        }
    });
    tm(
        &ctx,
        &mut gm,
        "AddPlayer",
        &[PyRep::Integer(1), PyRep::Integer(heavy_pilot)],
        &kw,
    )
    .await
    .unwrap();
    // Swap heavy_pilot back to 620 so CompletePregameChecks catches banned 585 and overweight 620
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&812_005) {
            it.type_id = 620;
        }
    });

    // Ban 585 (Slasher, 2 pts) and CompletePregameChecks -> TourneyShipTypeBanned
    tm(
        &ctx,
        &mut gm,
        "BanShipType",
        &[PyRep::Integer(0), PyRep::Integer(585)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "CompletePregameChecks",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "TourneyShipTypeBanned"
    );

    // Unban 585 + override banned_pilot error -> now TourneyMaxPointsExceeded on blue
    tm(
        &ctx,
        &mut gm,
        "UnBanShipType",
        &[PyRep::Integer(0), PyRep::Integer(585)],
        &kw,
    )
    .await
    .unwrap();
    tm(
        &ctx,
        &mut gm,
        "OverrideError",
        &[
            PyRep::Integer(7301),
            PyRep::Integer(banned_pilot),
            PyRep::String("TourneyShipTypeBanned".into()),
        ],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "CompletePregameChecks",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "TourneyMaxPointsExceeded"
    );

    // Remove heavy pilot + clear blue captain's stale max-points error -> CompletePregameChecks succeeds
    tm(
        &ctx,
        &mut gm,
        "RemovePlayer",
        &[PyRep::Integer(1), PyRep::Integer(heavy_pilot)],
        &kw,
    )
    .await
    .unwrap();
    tm(
        &ctx,
        &mut gm,
        "OverrideError",
        &[
            PyRep::Integer(7301),
            PyRep::Integer(blue_c),
            PyRep::String("TourneyMaxPointsExceeded".into()),
        ],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "CompletePregameChecks",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // OverrideWarpRestrict + Lock/UnlockMemberList on both red (0) and blue (1)
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "OverrideWarpRestrict",
            &[PyRep::Integer(7301), PyRep::Bool(true)],
            &kw
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    tm(&ctx, &mut gm, "LockMemberList", &[PyRep::Integer(0)], &kw)
        .await
        .unwrap();
    tm(&ctx, &mut gm, "UnlockMemberList", &[PyRep::Integer(0)], &kw)
        .await
        .unwrap();
    tm(&ctx, &mut gm, "LockMemberList", &[PyRep::Integer(1)], &kw)
        .await
        .unwrap();
    tm(&ctx, &mut gm, "UnlockMemberList", &[PyRep::Integer(1)], &kw)
        .await
        .unwrap();

    // StartCountdown -> check restrictions active, AbortCountdown -> resets to Setup!
    tm(
        &ctx,
        &mut gm,
        "StartCountdown",
        &[PyRep::Integer(7301)],
        &kw,
    )
    .await
    .unwrap();
    assert!(check_tourney_warp_restricted(&ctx, sys_jita, banned_pilot).is_err());
    assert!(check_tourney_move_restricted(&ctx, sys_jita, banned_pilot).is_err()); // active by default during Countdown
    tm(
        &ctx,
        &mut gm,
        "OverrideMoveRestrict",
        &[PyRep::Integer(7301), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap();
    assert!(check_tourney_move_restricted(&ctx, sys_jita, banned_pilot).is_ok()); // cleared via OverrideMoveRestrict(false)
    assert!(check_tourney_warp_restricted(&ctx, sys_jita, heavy_pilot).is_ok()); // non-participant

    // Outsider can view GetFancyDetails & GetTeamMembers once Countdown starts
    assert!(tm(
        &ctx,
        &mut ref2_s,
        "GetFancyDetails",
        &[PyRep::Integer(7301)],
        &kw
    )
    .await
    .is_ok());
    assert!(tm(
        &ctx,
        &mut ref2_s,
        "GetTeamMembers",
        &[PyRep::Integer(7301)],
        &kw
    )
    .await
    .is_ok());

    // Captain cannot ExecuteMemberChange during Countdown -> MatchAlreadyStarted
    assert_eq!(
        tm(
            &ctx,
            &mut red_s,
            "ExecuteMemberChange",
            &[PyRep::Integer(0), PyRep::Integer(heavy_pilot)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MatchAlreadyStarted"
    );

    // BanShipType during Countdown -> TourneyBansFinalized
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "BanShipType",
            &[PyRep::Integer(0), PyRep::Integer(589)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "TourneyBansFinalized"
    );

    // AbortCountdown transitions Countdown -> Setup
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "AbortCountdown",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // Re-enable StartCountdown -> OverrideLock/Move/Drone -> MatchCompleted -> ResetMatch
    tm(
        &ctx,
        &mut gm,
        "StartCountdown",
        &[PyRep::Integer(7301)],
        &kw,
    )
    .await
    .unwrap();
    tm(
        &ctx,
        &mut gm,
        "OverrideLockRestrict",
        &[PyRep::Integer(7301), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap();
    tm(
        &ctx,
        &mut gm,
        "OverrideMoveRestrict",
        &[PyRep::Integer(7301), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap();
    tm(
        &ctx,
        &mut gm,
        "OverrideDroneRestrict",
        &[PyRep::Integer(7301), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap();
    assert!(check_tourney_lock_restricted(&ctx, sys_jita, banned_pilot).is_err());
    assert!(check_tourney_move_restricted(&ctx, sys_jita, banned_pilot).is_err());
    assert!(check_tourney_drone_restricted(&ctx, sys_jita, banned_pilot).is_err());

    tm(
        &ctx,
        &mut gm,
        "MatchCompleted",
        &[PyRep::Integer(7301)],
        &kw,
    )
    .await
    .unwrap();
    // Completed clears active restrictions
    assert!(check_tourney_warp_restricted(&ctx, sys_jita, banned_pilot).is_ok());

    tm(&ctx, &mut gm, "ResetMatch", &[PyRep::Integer(7301)], &kw)
        .await
        .unwrap();

    // RemoveSelfFromReferees: non-referee -> TourneyNotReferee; referee -> succeeds
    assert_eq!(
        tm(
            &ctx,
            &mut ref2_s,
            "RemoveSelfFromReferees",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap_err()
        .code,
        "TourneyNotReferee"
    );
    assert_eq!(
        tm(
            &ctx,
            &mut gm,
            "RemoveSelfFromReferees",
            &[PyRep::Integer(7301)],
            &kw
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );

    // ruleset_allowed_ship_types across ruleset IDs 1, 2, 0 with banned exclusions
    let r1 = ruleset_allowed_ship_types(&ctx.sde, 1, &[587]);
    let r2 = ruleset_allowed_ship_types(&ctx.sde, 2, &[]);
    let r0 = ruleset_allowed_ship_types(&ctx.sde, 0, &[587]);
    assert!(!r1.contains(&587));
    assert!(!r2.is_empty());
    assert!(!r0.contains(&587));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn teleport_players_active_ship_fallback_caller_session_transition_and_restrictions() {
    let ctx = make_ctx();
    let start_sys = 30_000_142_u32;
    let arena_sys = 30_002_187_u32;
    let gm_cid = 983_001_i64;
    let zero_active_cid = 983_002_i64;
    let blue_cap_cid = 983_003_i64;
    let gm_ship = 813_001_i64;
    let fallback_ship = 813_002_i64;
    let blue_ship = 813_003_i64;

    seed_pilot(&ctx, gm_cid, start_sys, gm_ship, 587);
    seed_pilot(&ctx, zero_active_cid, start_sys, fallback_ship, 587);
    seed_pilot(&ctx, blue_cap_cid, start_sys, blue_ship, 587);

    let mut gm = mk_sess(Some(gm_cid), Some(start_sys), true);
    let kw = HashMap::new();

    // 1. Non-tourneyMgr returns None (kills `handle -> Some(Ok(Default::default()))`)
    let other_svc = ["char", "Mgr"].concat();
    assert!(handle(&ctx, &mut gm, &other_svc, "GetCharacterInfo", &[], &kw).await.is_none());

    let mut cm_kw = HashMap::new();
    cm_kw.insert("redTeamID".into(), PyRep::Integer(1_000_009));
    cm_kw.insert("blueTeamID".into(), PyRep::Integer(1_000_125));
    cm_kw.insert("redCaptainID".into(), PyRep::Integer(gm_cid));
    cm_kw.insert("blueCaptainID".into(), PyRep::Integer(blue_cap_cid));
    cm_kw.insert("systemID".into(), PyRep::Integer(i64::from(arena_sys)));
    tm(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(88),
            PyRep::Integer(7401),
            PyRep::Integer(50),
            PyRep::Integer(600),
            PyRep::Integer(15),
            PyRep::Integer(15),
            PyRep::Integer(0),
        ],
        &cm_kw,
    )
    .await
    .unwrap();

    // Put `gm_cid` on red (team 0), `zero_active_cid` on blue (team 1) with `active_ship_id = 0` so fallback to `p.ship_id` is tested!
    tm(&ctx, &mut gm, "AddPlayer", &[PyRep::Integer(0), PyRep::Integer(gm_cid)], &kw).await.unwrap();
    tm(&ctx, &mut gm, "AddPlayer", &[PyRep::Integer(1), PyRep::Integer(zero_active_cid)], &kw).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&zero_active_cid).unwrap().active_ship_id = 0;
    });

    // 2. Teleport team 0 (`gm_cid`, caller == Some(cid), active_ship_id > 0) -> moves gm_ship & transitions `gm.solar_system_id` to `arena_sys`
    let tp0 = tm(
        &ctx,
        &mut gm,
        "TeleportPlayers",
        &[PyRep::Integer(7401), PyRep::Integer(0), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(tp0, PyRep::Bool(true));
    assert_eq!(gm.solar_system_id, Some(arena_sys));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&gm_ship].location_id), i64::from(arena_sys));

    // 3. Teleport team 1 (`zero_active_cid`, active_ship_id == 0 -> uses resolved `p.ship_id = fallback_ship`, caller != Some(cid))
    gm.solar_system_id = Some(start_sys);
    let tp1 = tm(
        &ctx,
        &mut gm,
        "TeleportPlayers",
        &[PyRep::Integer(7401), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(tp1, PyRep::Bool(true));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&fallback_ship].location_id),
        i64::from(arena_sys),
        "active_ship_id == 0 must fall back to p.ship_id (> 0, not >= 0)"
    );
    assert!(
        ctx.systems
            .get_or_spawn(arena_sys)
            .get_ball_snapshot(fallback_ship)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        gm.solar_system_id,
        Some(start_sys),
        "caller session must not transition when teleported pilot cid != session.character_id"
    );

    // 4. `check_tourney_restriction` enforces restriction during Countdown (`!= Ok(())`)
    tm(&ctx, &mut gm, "StartCountdown", &[PyRep::Integer(7401)], &kw).await.unwrap();
    let restr_err = check_tourney_warp_restricted(&ctx, arena_sys, gm_cid).unwrap_err();
    assert_eq!(restr_err.code, "WarpRestrictedInTourney");
}
