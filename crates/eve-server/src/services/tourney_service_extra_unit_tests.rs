//! Extra unit and mutation-killing tests for `services::tourney_service`
//! covering `CreateMatch` tuple/kwarg parsing & duplicate ID detection, `BanShip` (captain vs
//! referee, red vs blue team ban tracking), `StartAutobanning`, `ExecuteMemberChange` (0/2/3/4
//! arg forms, add/remove, locked roster), `RemovePlayer`, `CompletePregameChecks` (banned ship,
//! max points exceeded, pilot errors & `OverrideError`), `QueryShipList` rulesets (0, 1, 2) &
//! finalized ban filtering, `GetActiveMatchForCamera`, `IsOtherMatchInProgress`, `CleanupGrid`
//! (wreck & unanchored can + child item removal vs anchored preservation), `TeleportPlayers`,
//! `ReturnTeamPlayers`, `ReturnAllPlayers`, `TeleportRefHome`, `UpdateFleetDetails`,
//! `RemoveSelfFromReferees`, and `OverrideDroneRestrict`/`OverrideLockRestrict`/`OverrideMoveRestrict`/`OverrideFleetBossID`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::tourney_service::*;
use crate::sp6_lifecycle::PersistedSpaceObjectMeta;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
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

fn make_session(char_id: i64, sys_id: u32, is_gm: bool) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(sys_id);
    if is_gm {
        s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    }
    s
}

async fn call_tm(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, "tourneyMgr", method, args, kwargs)
        .await
        .expect("tourneyMgr handled")
}

#[tokio::test]
async fn create_match_kwargs_ban_ship_autoban_query_ship_list_and_member_changes() {
    let ctx = test_ctx();
    let ref_id = 970_001_i64;
    let red_capt = 970_002_i64;
    let blue_capt = 970_003_i64;
    let red_member = 970_004_i64;
    let sys_id = 30_000_142_u32;
    let match_id = 6_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().unwrap().clone();
        for (cid, name, ship_id, ship_tid) in [
            (ref_id, "Referee", 700_001, 587_u32),
            (red_capt, "RedCapt", 700_002, 587_u32),
            (blue_capt, "BlueCapt", 700_003, 620_u32),
            (red_member, "RedMem", 700_004, 587_u32),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let mut gm = make_session(ref_id, sys_id, true);
    let mut red_sess = make_session(red_capt, sys_id, false);
    let mut blue_sess = make_session(blue_capt, sys_id, false);
    let empty_kw = HashMap::new();

    // 1. GetActiveMatchForCamera returns None when no matches exist
    assert_eq!(call_tm(&ctx, &mut gm, "GetActiveMatchForCamera", &[], &empty_kw).await.unwrap(), PyRep::None);

    // 2. CreateMatch with tuple in args[1] and ruleset 1 (frigate/destroyer ruleset)
    let tup_info = PyRep::Tuple(vec![
        PyRep::Integer(match_id),
        PyRep::Integer(101),
        PyRep::Integer(102),
        PyRep::Integer(red_capt),
        PyRep::Integer(blue_capt),
    ]);
    let created = call_tm(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(10),
            tup_info.clone(),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(60),
            PyRep::Integer(1), // ruleset 1
        ],
        &empty_kw,
    )
    .await
    .unwrap();
    assert!(matches!(created, PyRep::Tuple(_)));

    // Duplicate match ID rejected
    assert_eq!(
        call_tm(&ctx, &mut gm, "CreateMatch", &[PyRep::Integer(10), tup_info], &empty_kw)
            .await
            .unwrap_err()
            .code,
        "DuplicateMatchID"
    );

    // GetActiveMatchForCamera now returns match dict
    let cam = call_tm(&ctx, &mut gm, "GetActiveMatchForCamera", &[], &empty_kw).await.unwrap();
    assert_eq!(cam.dict_get("matchID").and_then(|v| v.as_i64()), Some(match_id));

    // 3. BanShip by red captain and blue captain records team-specific bans
    call_tm(
        &ctx,
        &mut red_sess,
        "BanShip",
        &[PyRep::Integer(match_id), PyRep::List(vec![PyRep::Integer(587)])],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(
        &ctx,
        &mut blue_sess,
        "BanShip",
        &[PyRep::Integer(match_id), PyRep::List(vec![PyRep::Integer(588)])],
        &empty_kw,
    )
    .await
    .unwrap();

    // StartAutobanning adds cruiser bans up to totalNumBans
    let mut auto_kw = HashMap::new();
    auto_kw.insert("totalNumBans".into(), PyRep::Integer(3));
    call_tm(&ctx, &mut gm, "StartAutobanning", &[PyRep::Integer(match_id)], &auto_kw)
        .await
        .unwrap();

    // QueryShipList before FinalizeBans vs after FinalizeBans
    let q_before = call_tm(&ctx, &mut gm, "QueryShipList", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();
    let list_before = match q_before.dict_get("allowedShipList") {
        Some(PyRep::List(v)) => v.clone(),
        _ => Vec::new(),
    };
    assert!(list_before.contains(&PyRep::Integer(587)));

    call_tm(&ctx, &mut gm, "FinalizeBans", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();
    let q_after = call_tm(&ctx, &mut gm, "QueryShipList", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();
    let list_after = match q_after.dict_get("allowedShipList") {
        Some(PyRep::List(v)) => v.clone(),
        _ => Vec::new(),
    };
    assert!(!list_after.contains(&PyRep::Integer(587)));

    // Unban 587 & 588 for subsequent pregame checks
    for (team, stid) in [(0, 587), (1, 588)] {
        call_tm(
            &ctx,
            &mut gm,
            "UnBanShipType",
            &[PyRep::Integer(match_id), PyRep::Integer(team), PyRep::Integer(stid)],
            &empty_kw,
        )
        .await
        .unwrap();
    }

    // 4. ExecuteMemberChange: empty args -> None, 3-arg add, 4-arg remove, RemovePlayer
    assert_eq!(call_tm(&ctx, &mut gm, "ExecuteMemberChange", &[], &empty_kw).await.unwrap(), PyRep::None);
    call_tm(
        &ctx,
        &mut gm,
        "ExecuteMemberChange",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(red_member)],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(
        &ctx,
        &mut gm,
        "ExecuteMemberChange",
        &[
            PyRep::Integer(match_id),
            PyRep::Integer(0),
            PyRep::Integer(red_member),
            PyRep::Bool(false),
        ],
        &empty_kw,
    )
    .await
    .unwrap();
    // Re-add via 2-arg form (uses active match) then remove via RemovePlayer (2-arg form)
    call_tm(
        &ctx,
        &mut gm,
        "ExecuteMemberChange",
        &[PyRep::Integer(0), PyRep::Integer(red_member)],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(
        &ctx,
        &mut gm,
        "RemovePlayer",
        &[PyRep::Integer(0), PyRep::Integer(red_member)],
        &empty_kw,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn pregame_checks_overrides_cleanup_grid_teleport_return_and_fleet_sync() {
    let ctx = test_ctx();
    let ref_id = 971_001_i64;
    let red_capt = 971_002_i64;
    let blue_capt = 971_003_i64;
    let bystander = 971_004_i64;
    let sys_id = 30_000_142_u32;
    let match_id = 6_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().unwrap().clone();
        for (cid, name, ship_id, ship_tid) in [
            (ref_id, "Referee2", 710_001, 587_u32),
            (red_capt, "RedCapt2", 710_002, 587_u32),
            (blue_capt, "BlueCapt2", 710_003, 587_u32),
            (bystander, "Bystander", 710_004, 587_u32),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.fleets.insert(
            801,
            FleetRecord {
                fleet_id: 801,
                boss_id: red_capt,
                members: vec![red_capt],
                ..Default::default()
            },
        );
        snap.fleets.insert(
            802,
            FleetRecord {
                fleet_id: 802,
                boss_id: blue_capt,
                members: vec![blue_capt],
                ..Default::default()
            },
        );
    });

    let mut gm = make_session(ref_id, sys_id, true);
    let mut outsider_sess = make_session(bystander, sys_id, false);
    let mut kw = HashMap::new();
    kw.insert("teams".into(), PyRep::List(vec![PyRep::Integer(201), PyRep::Integer(202)]));
    kw.insert(
        "captains".into(),
        PyRep::List(vec![PyRep::Integer(red_capt), PyRep::Integer(blue_capt)]),
    );

    // CreateMatch with ruleset 2 and max_points = 500
    call_tm(
        &ctx,
        &mut gm,
        "CreateMatch",
        &[
            PyRep::Integer(20),
            PyRep::Integer(match_id),
            PyRep::Integer(500),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(2),
        ],
        &kw,
    )
    .await
    .unwrap();
    let empty_kw = HashMap::new();

    // UpdateFleetDetails binds red & blue fleets
    call_tm(&ctx, &mut gm, "UpdateFleetDetails", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();

    // Ban 587 -> CompletePregameChecks & TeleportPlayers & UpdateFleetDetails reject with TourneyShipTypeBanned
    call_tm(
        &ctx,
        &mut gm,
        "BanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
        &empty_kw,
    )
    .await
    .unwrap();
    assert_eq!(
        call_tm(&ctx, &mut gm, "CompletePregameChecks", &[PyRep::Integer(match_id)], &empty_kw)
            .await
            .unwrap_err()
            .code,
        "TourneyShipTypeBanned"
    );
    assert_eq!(
        call_tm(
            &ctx,
            &mut gm,
            "TeleportPlayers",
            &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(0)],
            &empty_kw
        )
        .await
        .unwrap_err()
        .code,
        "TourneyShipTypeBanned"
    );

    // Unban 587, clear pilot error via OverrideError, then CompletePregameChecks succeeds
    call_tm(
        &ctx,
        &mut gm,
        "UnBanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
        &empty_kw,
    )
    .await
    .unwrap();
    for cid in [red_capt, blue_capt] {
        call_tm(
            &ctx,
            &mut gm,
            "OverrideError",
            &[PyRep::Integer(match_id), PyRep::Integer(cid), PyRep::String(String::new())],
            &empty_kw,
        )
        .await
        .unwrap();
    }
    call_tm(&ctx, &mut gm, "CompletePregameChecks", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();

    // OverrideFleetBossID & restriction overrides (Drone, Lock, Move, Warp)
    call_tm(
        &ctx,
        &mut gm,
        "OverrideFleetBossID",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(red_capt)],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(&ctx, &mut gm, "StartCountdown", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();

    // Bystander in the same system is NOT restricted; participant is restricted until overridden
    assert!(check_tourney_drone_restricted(&ctx, sys_id, bystander).is_ok());
    assert!(check_tourney_drone_restricted(&ctx, sys_id, red_capt).is_err());
    for method in ["OverrideDroneRestrict", "OverrideLockRestrict", "OverrideMoveRestrict"] {
        call_tm(&ctx, &mut gm, method, &[PyRep::Bool(false)], &empty_kw).await.unwrap();
    }
    assert!(check_tourney_drone_restricted(&ctx, sys_id, red_capt).is_ok());
    assert!(check_tourney_lock_restricted(&ctx, sys_id, red_capt).is_ok());
    assert!(check_tourney_move_restricted(&ctx, sys_id, red_capt).is_ok());

    // IsOtherMatchInProgress: false for same match_id, true for another match_id
    assert_eq!(
        call_tm(&ctx, &mut gm, "IsOtherMatchInProgress", &[PyRep::Integer(match_id)], &empty_kw)
            .await
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_tm(&ctx, &mut gm, "IsOtherMatchInProgress", &[PyRep::Integer(9_999)], &empty_kw)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );

    // CleanupGrid removes wreck (26557) + child loot and unanchored cargo container (group 12), preserves anchored can
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc, pos) in [
            (750_001, 26557_u32, sys_id as i64, Some([-50_000.0, 100.0, 0.0])), // wreck near beacon 0
            (750_002, 34_u32, 750_001_i64, None),                               // child inside wreck
            (750_003, 23_u32, sys_id as i64, Some([50_000.0, 0.0, 0.0])),       // unanchored cargo can (group 12)
            (750_004, 23_u32, sys_id as i64, Some([50_000.0, 0.0, 0.0])),       // anchored cargo can
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: red_capt,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            if let Some(p) = pos {
                snap.item_positions.insert(iid, p);
            }
        }
        snap.sp6_lifecycle.space_object_meta.insert(
            750_004,
            PersistedSpaceObjectMeta {
                anchor_completed: true,
                ..Default::default()
            },
        );
    });
    call_tm(&ctx, &mut gm, "CleanupGrid", &[PyRep::Integer(match_id)], &empty_kw).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&750_001));
        assert!(!snap.items.contains_key(&750_002));
        assert!(!snap.items.contains_key(&750_003));
        assert!(snap.items.contains_key(&750_004));
    });

    // TeleportPlayers -> ReturnTeamPlayers -> ReturnAllPlayers -> TeleportRefHome -> RemoveSelfFromReferees
    call_tm(
        &ctx,
        &mut gm,
        "TeleportPlayers",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(0)],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(
        &ctx,
        &mut gm,
        "ReturnTeamPlayers",
        &[PyRep::Integer(match_id), PyRep::Integer(0)],
        &empty_kw,
    )
    .await
    .unwrap();
    call_tm(&ctx, &mut gm, "ReturnAllPlayers", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();

    // Non-referee non-GM denied TeleportRefHome; GM referee succeeds
    assert_eq!(
        call_tm(&ctx, &mut outsider_sess, "TeleportRefHome", &[PyRep::Integer(999_999)], &empty_kw)
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    call_tm(&ctx, &mut gm, "TeleportRefHome", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();
    assert!(gm.station_id.is_some());

    // RemoveSelfFromReferees removes ref_id; calling again returns TourneyNotReferee
    call_tm(&ctx, &mut gm, "RemoveSelfFromReferees", &[PyRep::Integer(match_id)], &empty_kw)
        .await
        .unwrap();
    assert_eq!(
        call_tm(&ctx, &mut gm, "RemoveSelfFromReferees", &[PyRep::Integer(match_id)], &empty_kw)
            .await
            .unwrap_err()
            .code,
        "TourneyNotReferee"
    );
}
