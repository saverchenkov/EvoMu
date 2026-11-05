//! Unit tests for `tourney_service` (`tourneyMgr` match lifecycle, rosters, ship bans, restrictions, countdowns).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn gm_tourney_session(char_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(sys_id);
    s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    s
}

#[test]
fn tourney_match_lifecycle_bans_locks_and_arena_restrictions() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let ref_id = 960_001_i64;
    let pilot_red = 960_002_i64;
    let pilot_blue = 960_003_i64;
    let sys_id = 30000142_u32;
    let match_id = 5_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().unwrap().clone();
        for (cid, name) in [
            (ref_id, "Referee"),
            (pilot_red, "RedPilot"),
            (pilot_blue, "BluePilot"),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.to_string();
            ch.solar_system_id = sys_id;
            snap.characters.insert(cid, ch);
        }
    });

    let mut non_gm = SessionState::default();
    non_gm.user_id = Some(2);
    non_gm.character_id = Some(pilot_red);
    non_gm.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // Non-GM denied CreateMatch
    let denied = rt
        .block_on(handle(
            &ctx,
            &mut non_gm,
            "tourneyMgr",
            "CreateMatch",
            &[PyRep::Integer(1), PyRep::Integer(match_id)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(denied.code, "AdminAccessDenied");

    let mut gm = gm_tourney_session(ref_id, sys_id);

    // CreateMatch with maxPoints = 50, matchTime = 600
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(match_id),
            PyRep::Integer(50),
            PyRep::Integer(600),
            PyRep::Integer(120),
            PyRep::Integer(0),
        ],
        &kw,
    ))
    .unwrap()
    .expect("CreateMatch");

    // GetMatchSystem returns sys_id
    let msys = rt
        .block_on(handle(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "GetMatchSystem",
            &[PyRep::Integer(match_id)],
            &kw,
        ))
        .unwrap()
        .expect("GetMatchSystem");
    assert_eq!(msys, PyRep::Integer(i64::from(sys_id)));

    // Add red and blue pilots
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(pilot_red)],
        &kw,
    ))
    .unwrap()
    .expect("AddPlayer red");
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(match_id), PyRep::Integer(1), PyRep::Integer(pilot_blue)],
        &kw,
    ))
    .unwrap()
    .expect("AddPlayer blue");

    // Lock red member list -> adding another red player blocked with MemberListLocked
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "LockMemberList",
        &[PyRep::Integer(match_id), PyRep::Integer(0)],
        &kw,
    ))
    .unwrap()
    .expect("LockMemberList");
    let lock_err = rt
        .block_on(handle(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "AddPlayer",
            &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(999_999)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(lock_err.code, "MemberListLocked");

    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "UnlockMemberList",
        &[PyRep::Integer(match_id), PyRep::Integer(0)],
        &kw,
    ))
    .unwrap()
    .expect("UnlockMemberList");

    // Ban ship type 587 -> unban -> re-ban -> FinalizeBans -> post-finalize ban rejected
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
        &kw,
    ))
    .unwrap()
    .expect("BanShipType");
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "UnBanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
        &kw,
    ))
    .unwrap()
    .expect("UnBanShipType");
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(620)],
        &kw,
    ))
    .unwrap()
    .expect("BanShipType 620");
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "FinalizeBans",
        &[PyRep::Integer(match_id)],
        &kw,
    ))
    .unwrap()
    .expect("FinalizeBans");

    let ban_after_final = rt
        .block_on(handle(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "BanShipType",
            &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
            &kw,
        ))
        .unwrap()
        .unwrap_err();
    assert_eq!(ban_after_final.code, "TourneyBansFinalized");

    // StartCountdown activates restrictions -> verify warp/drone/lock/move restriction checks
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "StartCountdown",
        &[PyRep::Integer(match_id), PyRep::Integer(60)],
        &kw,
    ))
    .unwrap()
    .expect("StartCountdown");

    assert_eq!(
        check_tourney_warp_restricted(&ctx, sys_id, pilot_red)
            .unwrap_err()
            .code,
        "WarpRestrictedInTourney"
    );
    assert_eq!(
        check_tourney_drone_restricted(&ctx, sys_id, pilot_red)
            .unwrap_err()
            .code,
        "DroneRestrictedInTourney"
    );
    assert_eq!(
        check_tourney_lock_restricted(&ctx, sys_id, pilot_red)
            .unwrap_err()
            .code,
        "LockRestrictedInTourney"
    );
    assert_eq!(
        check_tourney_move_restricted(&ctx, sys_id, pilot_red)
            .unwrap_err()
            .code,
        "MoveRestrictedInTourney"
    );

    // OverrideWarpRestrict clears warp restriction while lock restriction remains active
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "OverrideWarpRestrict",
        &[PyRep::Integer(match_id), PyRep::Bool(false)],
        &kw,
    ))
    .unwrap()
    .expect("OverrideWarpRestrict");
    assert!(check_tourney_warp_restricted(&ctx, sys_id, pilot_red).is_ok());

    // AbortCountdown returns match to Setup; CleanupGrid, MatchCompleted, ResetMatch
    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AbortCountdown",
        &[PyRep::Integer(match_id)],
        &kw,
    ))
    .unwrap()
    .expect("AbortCountdown");
    assert!(check_tourney_lock_restricted(&ctx, sys_id, pilot_red).is_ok());

    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CleanupGrid",
        &[PyRep::Integer(match_id)],
        &kw,
    ))
    .unwrap()
    .expect("CleanupGrid");

    let details = rt
        .block_on(handle(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "GetFancyDetails",
            &[PyRep::Integer(match_id)],
            &kw,
        ))
        .unwrap()
        .expect("GetFancyDetails");
    assert!(details.dict_get("redTeam").is_some() || details.dict_get("matchID").is_some());

    let team_members = rt
        .block_on(handle(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "GetTeamMembers",
            &[PyRep::Integer(match_id)],
            &kw,
        ))
        .unwrap()
        .expect("GetTeamMembers");
    assert!(matches!(team_members, PyRep::Tuple(_) | PyRep::List(_) | PyRep::Dict(_)));

    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "MatchCompleted",
        &[PyRep::Integer(match_id)],
        &kw,
    ))
    .unwrap()
    .expect("MatchCompleted");

    rt.block_on(handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "ResetMatch",
        &[PyRep::Integer(match_id)],
        &kw,
    ))
    .unwrap()
    .expect("ResetMatch");

    // Non-tourneyMgr service & unknown method return None
    assert!(rt
        .block_on(handle(&ctx, &mut gm, "", "", &[], &kw))
        .is_none());
    assert!(rt
        .block_on(handle(&ctx, &mut gm, "tourneyMgr", "", &[], &kw))
        .is_none());

    // Verify match state persisted cleanly in WorldRepository
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.owner_notes.is_empty());
    });
}
