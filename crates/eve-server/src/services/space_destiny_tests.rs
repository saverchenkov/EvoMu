//! Unit tests for `space_destiny` (`essMgr`, session timers, warp speed, `beyonce` state/bubble/components).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn put_item(
    snap: &mut WorldStateSnapshot,
    item_id_opt: Option<i64>,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
    let item_id = item_id_opt.unwrap_or_else(|| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

fn rep_list_len(rep: &PyRep) -> Option<usize> {
    match rep {
        PyRep::List(v) | PyRep::Tuple(v) => Some(v.len()),
        _ => None,
    }
}

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

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(710);
    s.user_id = Some(22);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = role;
    s
}

fn bound_sys(sys_id: u32) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 710,
        user_id: 20,
        character_id: 9001,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

fn test_ship_ball(ship_id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    let PyRep::Dict(entries) = rep else {
        return None;
    };
    entries
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

#[test]
fn ess_reserve_pulse_blob_and_snapshot_roundtrip() {
    let ctx = test_ctx();
    let state = EssReservePulseState {
        last_pulse_sec: Some(1_700_000_123),
        pulses_remaining: 31,
        pulses_total: 45,
        unlocked_until_unix: 1_700_002_823,
        reserve_linked_character_id: Some(9001),
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        let def = get_ess_reserve_pulses(snap, 30000142);
        assert_eq!(def.pulses_total, 0);
        assert_eq!(def.pulses_remaining, 0);
        assert!(def.reserve_linked_character_id.is_none());

        set_ess_reserve_pulses(snap, 30001234, &state);
        let snap_read = get_ess_reserve_pulses(snap, 30001234);
        assert_eq!(snap_read.last_pulse_sec, Some(1_700_000_123));
        assert_eq!(snap_read.pulses_remaining, 31);
        assert_eq!(snap_read.pulses_total, 45);
        assert_eq!(snap_read.unlocked_until_unix, 1_700_002_823);
        assert_eq!(snap_read.reserve_linked_character_id, Some(9001));
    });
}

#[test]
fn session_change_timers_and_invulnerability_lifecycle() {
    let ctx = test_ctx();
    let char_id = 9101;
    let t0 = 1_700_010_000;

    record_session_change_timer(&ctx, char_id, t0, 30000142, 1, "jump_drive");
    assert!(check_session_change_timer_active(&ctx, char_id, t0 + 9, None).is_some());
    assert!(check_session_change_timer_active(&ctx, char_id, t0 + 10, None).is_none());
    let stored = get_session_change_timer(&ctx, char_id).expect("timer active");
    assert_eq!(stored.reason, "jump_drive");

    clear_session_change_timer(&ctx, char_id);
    assert!(check_session_change_timer_active(&ctx, char_id, t0 + 1, None).is_none());

    record_client_session_check_timer(&ctx, char_id, t0 + 200, 30000142, 10, "jump");
    assert!(check_session_change_timer_active(&ctx, char_id, t0 + 205, Some((30000142, 15))).is_some());
    assert!(check_session_change_timer_active(&ctx, char_id, t0 + 205, Some((30000142, 25))).is_none());
    let _ = wormhole_item_id_for_signature(31000001, "ABC-123");
}

#[test]
fn warp_speed_calculations_apply_subwarpspeed_and_navigation() {
    let mut ctx = test_ctx();
    let ship_tid = 99510;
    clone_type_with(
        &mut ctx.sde,
        588,
        ship_tid,
        25,
        6,
        2500.0,
        150.0,
        &[(600, 3.0), (1281, 1.5)], // baseWarpSpeed = 3.0, warpSpeedMultiplier = 1.5 -> 4.5 AU/s
    );
    let base = compute_ship_warp_speed_au_s(&ctx, ship_tid);
    assert!((base - 4.5).abs() < 1e-6);
    assert!((compute_pilot_warp_speed_au_s(&ctx, 9001, 30000142, ship_tid) - 4.5).abs() < 1e-6);

    // Zero warpSpeedMultiplier falls back to baseWarpSpeed = 3.0 AU/s
    let ship_zero_mult = 99511;
    clone_type_with(
        &mut ctx.sde,
        588,
        ship_zero_mult,
        25,
        6,
        2500.0,
        150.0,
        &[(600, 3.0), (1281, 0.0)],
    );
    assert!((compute_ship_warp_speed_au_s(&ctx, ship_zero_mult) - 3.0).abs() < 1e-6);

    // Role bonus attr 2789 boosts ship warp speed
    let ship_role_bonus = 99512;
    clone_type_with(
        &mut ctx.sde,
        588,
        ship_role_bonus,
        25,
        6,
        2500.0,
        150.0,
        &[(600, 3.0), (1281, 1.5), (2789, 25.0)],
    );
    let boosted = compute_pilot_warp_speed_au_s(&ctx, 9001, 30000142, ship_role_bonus);
    assert!((boosted - 4.5 * 1.25).abs() < 1e-6);
}

#[tokio::test]
async fn ess_mgr_dispatch_covers_data_thefts_unlink_and_unlock() {
    let mut ctx = test_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("nullsec system in sde");
    let high_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status >= 0.7 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("highsec system in sde");

    let mut hs_sess = make_session(9301, 98000001, Some(high_sys), Some(88301), 0);
    let hs_data = handle(&ctx, &mut hs_sess, None, "essMgr", "GetDataForClientSolarSystem", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(hs_data, PyRep::None);

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = IskCents::from_isk(125_000_000);
        st.ess_reserve_bank = IskCents::from_isk(400_000_000);
        set_ess_reserve_pulses(
            snap,
            null_sys,
            &EssReservePulseState {
                last_pulse_sec: Some(now_u.saturating_sub(30)),
                pulses_remaining: 42,
                pulses_total: 45,
                unlocked_until_unix: now_u + 2500,
                reserve_linked_character_id: Some(9301),
                ..Default::default()
            },
        );
        snap.combat_crimewatch_state
            .ess_main_bank_thefts
            .insert(null_sys, vec![(9301, 25_000_000.0, now_u)]);
        snap.combat_crimewatch_state
            .ess_reserve_bank_thefts
            .insert(null_sys, vec![(9301, 10_000_000.0, now_u)]);
    });

    ctx.systems
        .wake_for_session_arrival(
            null_sys,
            crate::system_actor::SessionArrivalWakeReason::LoginInSpace,
        )
        .expect("wake null_sys");
    let mut ns_sess = make_session(9301, 98000001, Some(null_sys), Some(88301), 0);
    let data = handle(&ctx, &mut ns_sess, None, "essMgr", "GetDataForClientSolarSystem", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(dict_get(&data, "essID").and_then(PyRep::as_i64), Some(820_000_000 + null_sys as i64));
    assert_eq!(dict_get(&data, "reserveBankPulsesRemaining").and_then(PyRep::as_i64), Some(42));
    assert_eq!(dict_get(&data, "reserveBankActiveLinks").and_then(PyRep::as_i64), Some(1));

    let main_thefts = handle(&ctx, &mut ns_sess, None, "essMgr", "GetMainBankTheftsForClientSolarSystem", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(rep_list_len(&main_thefts), Some(1));

    let res_thefts = handle(&ctx, &mut ns_sess, None, "essMgr", "GetReserveBankTheftsForClientSolarSystem", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(rep_list_len(&res_thefts), Some(1));

    let linked = handle(&ctx, &mut ns_sess, None, "essMgr", "IsClientLinkedToReserveBank", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(linked.as_bool(), Some(true));

    let _ = handle(&ctx, &mut ns_sess, None, "essMgr", "RequestMainBankUnlink", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    let _ = handle(&ctx, &mut ns_sess, None, "essMgr", "RequestReserveBankUnlink", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    let linked_after = handle(&ctx, &mut ns_sess, None, "essMgr", "IsClientLinkedToReserveBank", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(linked_after.as_bool(), Some(false));

    for key_tid in [57714, 57716, 57718, 57720] {
        clone_type_with(&mut ctx.sde, 28667, key_tid, 4102, 30, 0.1, 0.0, &[]);
    }
    ctx.db.with_mut_snapshot(|snap| {
        set_ess_reserve_pulses(snap, null_sys, &EssReservePulseState::default());
        put_item(snap, Some(88301), 588, 9301, null_sys as i64, InventoryFlag::None, 1, true);
        for key_tid in [57714, 57716, 57718, 57720] {
            put_item(
                snap,
                None,
                key_tid,
                9301,
                88301,
                InventoryFlag::Cargo,
                1,
                false,
            );
        }
    });
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let ess_pos = sys_h
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|b| b.type_id == 32882 || b.type_id == 56002 || b.id == 820_000_000 + null_sys as i64)
        .map(|b| b.position)
        .unwrap_or(DVec3::new(0.0, 0.0, 75_000_000_000.0));
    let _ = sys_h
        .spawn_ball(test_ship_ball(88301, 588, ess_pos + DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    let unlocked = handle(
        &ctx,
        &mut ns_sess,
        None,
        "essMgr",
        "RequestUnlockReserveBank",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("unlock ok");
    assert!(matches!(unlocked, PyRep::KeyVal(_)));

    let clear_ok = handle(&ctx, &mut ns_sess, None, "essMgr", "clear_cache", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("clear_cache");
    assert_eq!(clear_ok, PyRep::None);
}

#[tokio::test]
async fn beyonce_update_state_and_admin_bubble_id_paths() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let b1 = test_ship_ball(88401, 588, DVec3::new(1000.0, 0.0, 0.0));
    let _ = sys_h.spawn_ball(b1).await;
    let _ = sys_h
        .spawn_ball(test_ship_ball(88402, 588, DVec3::new(2500.0, 0.0, 0.0)))
        .await;

    let mut player_sess = make_session(9401, 98000001, Some(sys_id), Some(88402), 0);
    let bound = bound_sys(sys_id);
    let res = handle(
        &ctx,
        &mut player_sess,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("UpdateStateRequest ok");
    assert_eq!(res, PyRep::None);

    let denied = handle(
        &ctx,
        &mut player_sess,
        Some(&bound),
        "beyonce",
        "AdminGetBubbleID",
        &[PyRep::Integer(88402)],
        &HashMap::new(),
    )
    .await
    .expect("handled");
    assert!(denied.is_err());

    let mut gm_sess = make_session(9402, 98000001, Some(sys_id), Some(88402), service_roles::ROLE_GMH);
    let bid_ship = handle(
        &ctx,
        &mut gm_sess,
        Some(&bound),
        "beyonce",
        "AdminGetBubbleID",
        &[PyRep::Integer(88402)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("AdminGetBubbleID ok");
    assert!(bid_ship.as_i64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn call_component_from_client_validates_and_executes_item_trader() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let char_id = 9501;
    let ship_id = 88501;
    let trader_id = 880_123_456;
    let mut sess = make_session(char_id, 98000001, Some(sys_id), Some(ship_id), 0);
    let bound = bound_sys(sys_id);

    // Unsupported component returns error
    let unsupported = handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(trader_id),
            PyRep::String("unknownComponent".into()),
            PyRep::String("DoThing".into()),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("unsupported component");
    assert_eq!(unsupported.code, "UnsupportedComponentCall");

    // Missing recipe ID returns InvalidRecipeID
    let no_recipe = handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(trader_id),
            PyRep::String("itemTrader".into()),
            PyRep::String("ProcessTrade".into()),
            PyRep::Integer(0),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("invalid recipe id");
    assert_eq!(no_recipe.code, "InvalidRecipeID");

    // Missing item trader ball/item in system returns ItemTraderNotFoundInSystem
    let missing_trader = handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(trader_id),
            PyRep::String("itemTrader".into()),
            PyRep::String("ProcessTrade".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("missing trader");
    assert_eq!(missing_trader.code, "ItemTraderNotFoundInSystem");
}

#[tokio::test]
async fn ess_mgr_clear_cache_evicts_ess_entries_only() {
    let ctx = test_ctx();
    let mut sess = make_session(9001, 98000001, Some(30000142), None, 0);
    let ess_oid = ServerContext::method_call_object_id("essMgr", "GetDataForClientSolarSystem");
    let other_oid = ServerContext::method_call_object_id("sovMgr", "GetSovStructuresInfoForSolarSystem");
    ctx.put_cached_object(ess_oid.clone(), PyRep::None, true);
    ctx.put_cached_object(other_oid.clone(), PyRep::None, true);

    let res = handle(&ctx, &mut sess, None, "essMgr", "clear_cache", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("ok");
    assert_eq!(res, PyRep::None);

    let cache = ctx.object_cache.read().unwrap();
    assert!(!cache.contains_key(&crate::router::cache_object_key(&ess_oid)));
    assert!(cache.contains_key(&crate::router::cache_object_key(&other_oid)));
}

#[tokio::test]
async fn space_destiny_edge_guards_bubble_errors_warp_recovery_and_timers() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(test_ship_ball(88601, 588, DVec3::new(1000.0, 0.0, 0.0)))
        .await;
    let bound = bound_sys(sys_id);

    // 1. AdminGetBubbleID edge errors: unauth, NoActiveShip, BallNotVisible, BallNotFound, InvalidBound
    let mut unauth = SessionState::new(711);
    assert!(
        handle(&ctx, &mut unauth, Some(&bound), "beyonce", "AdminGetBubbleID", &[], &HashMap::new())
            .await
            .unwrap()
            .is_err()
    );

    let mut gm_no_ship = make_session(9410, 98000001, Some(sys_id), None, service_roles::ROLE_GMH);
    let no_ship_err = handle(
        &ctx,
        &mut gm_no_ship,
        Some(&bound),
        "beyonce",
        "AdminGetBubbleID",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect_err("NoActiveShip");
    assert_eq!(no_ship_err.code, "NoActiveShip");

    // Ship 88601 is in space; target 99999999 is not in bubble -> BallNotVisible
    let mut gm_ship = make_session(
        9410,
        98000001,
        Some(sys_id),
        Some(88601),
        service_roles::ROLE_GMH,
    );
    let not_vis_err = handle(
        &ctx,
        &mut gm_ship,
        Some(&bound),
        "beyonce",
        "AdminGetBubbleID",
        &[PyRep::Integer(99_999_999)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect_err("BallNotVisible");
    assert_eq!(not_vis_err.code, "BallNotVisible");

    // Own ship 88602 is unspawned and no arg -> target_ball_id == ship_id -> BallNotFound
    let mut gm_unspawned = make_session(
        9411,
        98000001,
        Some(sys_id),
        Some(88602),
        service_roles::ROLE_GMH,
    );
    let not_found_err = handle(
        &ctx,
        &mut gm_unspawned,
        Some(&bound),
        "beyonce",
        "AdminGetBubbleID",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect_err("BallNotFound");
    assert_eq!(not_found_err.code, "BallNotFound");

    // 2. handle_update_state_request: BoundObjectOwnershipViolation, NoSolarSystem, docked, no ship,
    // missing snapshot item, active warp early-return, and emergency-logoff warp stop
    let mut wrong_sys = make_session(9412, 98000001, Some(30002187), Some(88601), 0);
    let mis_err = handle(
        &ctx,
        &mut wrong_sys,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect_err("BoundObjectOwnershipViolation");
    assert_eq!(mis_err.code, "BoundObjectOwnershipViolation");

    let mut no_sys = make_session(9412, 98000001, None, Some(88601), 0);
    let no_sys_err = handle(&ctx, &mut no_sys, None, "beyonce", "UpdateStateRequest", &[], &HashMap::new())
        .await
        .unwrap()
        .expect_err("NoSolarSystem");
    assert_eq!(no_sys_err.code, "NoSolarSystem");

    let mut docked = make_session(9412, 98000001, Some(sys_id), Some(88601), 0);
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle(&ctx, &mut docked, None, "beyonce", "UpdateStateRequest", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    let mut no_ship_sess = make_session(9412, 98000001, Some(sys_id), None, 0);
    assert_eq!(
        handle(&ctx, &mut no_ship_sess, None, "beyonce", "UpdateStateRequest", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Ship ID absent from both ballpark and snap.items returns Ok(None) cleanly
    let mut missing_item = make_session(9412, 98000001, Some(sys_id), Some(88699), 0);
    assert_eq!(
        handle(&ctx, &mut missing_item, None, "beyonce", "UpdateStateRequest", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Spawn warping ball: when registered in system active_warps, UpdateStateRequest returns Ok(None)
    // and leaves MovementMode::Warp intact; when absent (emergency logoff return-warp), forces Stop.
    let mut warp_ball = test_ship_ball(88603, 588, DVec3::new(5_000.0, 0.0, 0.0));
    warp_ball.mode = MovementMode::new_warp(
        warp_ball.position,
        DVec3::new(1_000_000.0, 0.0, 0.0),
        3.0,
    );
    let _ = sys_h.spawn_ball(warp_ball).await;
    sys_h.insert_active_warp_for_test(88603, 1);

    let mut warp_sess = make_session(9413, 98000001, Some(sys_id), Some(88603), 0);
    assert_eq!(
        handle(&ctx, &mut warp_sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let (_, snap_after_active) = sys_h.get_ball_snapshot(88603).await.unwrap().unwrap();
    assert!(matches!(snap_after_active.mode, MovementMode::Warp { .. }));

    // Clear active_warps to simulate stale emergency-logoff warp -> force_stop_warp triggers
    sys_h.clear_all_warp_tracking_for_test();
    assert_eq!(
        handle(&ctx, &mut warp_sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let (_, snap_after_stop) = sys_h.get_ball_snapshot(88603).await.unwrap().unwrap();
    assert!(matches!(snap_after_stop.mode, MovementMode::Stop));

    // 3. essMgr clear_cache unauth, thefts with solar_system_id == None, and prime_cache_result RPC
    assert!(
        handle(&ctx, &mut unauth, None, "essMgr", "clear_cache", &[], &HashMap::new())
            .await
            .unwrap()
            .is_err()
    );
    let mut no_sys_ess = make_session(9414, 98000001, None, None, 0);
    let empty_thefts = handle(
        &ctx,
        &mut no_sys_ess,
        None,
        "essMgr",
        "GetMainBankTheftsForClientSolarSystem",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rep_list_len(&empty_thefts), Some(0));
    assert!(
        handle(
            &ctx,
            &mut warp_sess,
            None,
            "essMgr",
            "prime_cache_result",
            &[PyRep::Integer(i64::from(sys_id))],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 4. record_session_change_timer notification suppression & structure dock formatting
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = 9415;
        ch.station_id = None;
        ch.structure_id = Some(1_000_000_999);
        ch.active_ship_id = 88604;
        snap.characters.insert(9415, ch);
    });
    ctx.db
        .acquire_online_lock(1, 9415, Some(9415), true, 1_700_020_000)
        .expect("online lock for 9415");
    record_session_change_timer(&ctx, 9415, 1_700_020_000, sys_id, 5, "dock");
    let notes_dock = ctx.drain_notifications_for_char(9415);
    assert!(notes_dock.iter().any(|n| n.event == "OnSessionChanged"));
    record_session_change_timer(
        &ctx,
        9415,
        1_700_020_005,
        sys_id,
        5,
        "jump_drive",
    );
    assert_eq!(
        get_session_change_timer(&ctx, 9415).unwrap().expires_unix,
        1_700_020_005 + 10
    );
    let _ = ctx.drain_notifications_for_char(9415);

    record_session_change_timer(&ctx, 9415, 1_700_020_010, sys_id, 6, "board_or_eject");
    assert!(ctx.drain_notifications_for_char(9415).is_empty());

    // Empty entries suppression (set_account_key in system with no character row -> has_entries = false)
    record_client_session_check_timer(&ctx, 9499, 1_700_020_020, 0, 7, "set_account_key");
    assert!(ctx.drain_notifications_for_char(9499).is_empty());
}

#[tokio::test]
async fn update_state_request_respects_warp_provenance_and_clears_landed_or_cancelled_warps() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let bound = bound_sys(sys_id);
    let kw = HashMap::new();
    let seed_online_char = |char_id: i64, ship_id: i64| {
        let uid = char_id as u64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = char_id;
            ch.user_id = uid;
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(char_id, ch);
            snap.accounts.insert(
                format!("test_u_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("test_u_{uid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        });
        ctx.db
            .acquire_online_lock(uid, uid, Some(char_id), true, 1_700_020_000)
            .expect("online lock");
    };

    // 1. DisconnectedEmergencyWarp initialization clears stale active_warps and UpdateStateRequest force-stops
    //    both explicit emergency warps (even if return_warps holds a stale entry) and untracked warps,
    //    while preserving ReconnectReturnWarp.
    let _ = sys_h
        .spawn_ball(test_ship_ball(88701, 588, DVec3::ZERO))
        .await;
    sys_h.insert_active_warp_for_test(88701, 9421);
    sys_h.prepare_disconnected_warp_sync(
        88701,
        588,
        40.0,
        1_000_000.0,
        1.0,
        300.0,
        3.0,
        DVec3::ZERO,
        DVec3::new(1_000_000.0, 0.0, 0.0),
    );
    assert!(
        !sys_h.is_active_warp(88701),
        "Initializing DisconnectedEmergencyWarp must clear stale active_warps"
    );
    assert_eq!(
        sys_h.warp_provenance(88701),
        Some(crate::system_actor::WarpProvenance::DisconnectedEmergencyWarp)
    );
    sys_h.insert_return_warp_for_test(88701);
    let mut sess_emerg = make_session(9421, 98000001, Some(sys_id), Some(88701), 0);
    handle(
        &ctx,
        &mut sess_emerg,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (_, b_emerg) = sys_h.get_ball_snapshot(88701).await.unwrap().unwrap();
    assert!(matches!(b_emerg.mode, MovementMode::Stop));
    assert!(!sys_h.is_active_warp(88701));
    assert!(!sys_h.is_return_warp(88701));
    assert_eq!(sys_h.warp_provenance(88701), None);

    // 1b. ReconnectReturnWarp preserves MovementMode::Warp across UpdateStateRequest.
    sys_h
        .set_warp_movement_mode(
            88701,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
            crate::system_actor::WarpProvenance::ReconnectReturnWarp,
        )
        .await
        .unwrap();
    handle(
        &ctx,
        &mut sess_emerg,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (_, b_ret) = sys_h.get_ball_snapshot(88701).await.unwrap().unwrap();
    assert!(matches!(b_ret.mode, MovementMode::Warp { .. }));
    let _ = sys_h.force_stop_warp(88701);
    clear_warp_tracking(&sys_h, 88701);

    // 2. PlayerOrFleetWarp preserves Warp across UpdateStateRequest even before active_warps sync,
    //    clears active_warps/provenance immediately on landing, and delivers OnWarpFinished once.
    let _ = sys_h
        .spawn_ball(test_ship_ball(88702, 588, DVec3::ZERO))
        .await;
    sys_h
        .set_warp_movement_mode(
            88702,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
            crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
        )
        .await
        .unwrap();
    assert!(!sys_h.is_active_warp(88702));
    seed_online_char(9422, 88702);
    let mut sess_player = make_session(9422, 98000001, Some(sys_id), Some(88702), 0);
    sess_player.session_id = 9422;
    sess_player.user_id = Some(9422);
    handle(
        &ctx,
        &mut sess_player,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (_, b_player_mid) = sys_h.get_ball_snapshot(88702).await.unwrap().unwrap();
    assert!(matches!(b_player_mid.mode, MovementMode::Warp { .. }));

    track_player_or_fleet_warp(&sys_h, 88702, 9422);
    let mut landed = false;
    for _ in 0..240 {
        let _ = sys_h.step_manual(1.0).await;
        let (_, b) = sys_h.get_ball_snapshot(88702).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            landed = true;
            break;
        }
    }
    assert!(landed, "Player warp must reach MovementMode::Stop");
    assert!(!sys_h.is_active_warp(88702));
    assert_eq!(sys_h.warp_provenance(88702), None);
    assert!(sys_h.is_pending_warp_finished(88702));

    let _ = ctx.drain_notifications_for_char(9422);
    handle(
        &ctx,
        &mut sess_player,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let fin_notes = ctx.drain_notifications_for_char(9422);
    assert!(fin_notes.iter().any(|n| n.event == "OnWarpFinished"));
    assert!(!sys_h.is_pending_warp_finished(88702));

    handle(
        &ctx,
        &mut sess_player,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !ctx.drain_notifications_for_char(9422)
            .iter()
            .any(|n| n.event == "OnWarpFinished")
    );

    // 3. Cancelling an aligning warp via CmdStop clears all tracking without pending OnWarpFinished.
    let _ = sys_h
        .spawn_ball(test_ship_ball(88703, 588, DVec3::ZERO))
        .await;
    sys_h
        .set_warp_movement_mode(
            88703,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
            crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
        )
        .await
        .unwrap();
    track_player_or_fleet_warp(&sys_h, 88703, 9423);
    seed_online_char(9423, 88703);
    let mut sess_cancel = make_session(9423, 98000001, Some(sys_id), Some(88703), 0);
    sess_cancel.session_id = 9423;
    sess_cancel.user_id = Some(9423);
    handle(
        &ctx,
        &mut sess_cancel,
        Some(&bound),
        "beyonce",
        "CmdStop",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (_, b_cancel) = sys_h.get_ball_snapshot(88703).await.unwrap().unwrap();
    assert!(matches!(b_cancel.mode, MovementMode::Stop));
    assert!(!sys_h.is_active_warp(88703));
    assert_eq!(sys_h.warp_provenance(88703), None);
    assert!(!sys_h.is_pending_warp_finished(88703));
    let _ = ctx.drain_notifications_for_char(9423);
    handle(
        &ctx,
        &mut sess_cancel,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !ctx.drain_notifications_for_char(9423)
            .iter()
            .any(|n| n.event == "OnWarpFinished")
    );
}

#[tokio::test]
async fn live_update_state_request_spam_does_not_bump_tick_or_jolt_peers_1169() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_a = 95_001_i64;
    let ship_a = 89_501_i64;
    let char_b = 95_002_i64;
    let ship_b = 89_502_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let seed_char = snap.characters.values().next().cloned().expect("seed char");
        for (cid, sid) in [(char_a, ship_a), (char_b, ship_b)] {
            let mut ch = seed_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("Pilot-{cid}");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            put_item(
                snap,
                Some(sid),
                588,
                cid,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            );
            snap.item_positions.insert(sid, [0.0, 0.0, 0.0]);
        }
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(test_ship_ball(ship_a, 588, DVec3::ZERO))
        .await;
    let _ = sys_h
        .spawn_ball(test_ship_ball(ship_b, 588, DVec3::new(2_000.0, 0.0, 0.0)))
        .await;
    let mut sys_rx = sys_h.subscribe();

    let mut sess_a = make_session(char_a, 98_000_001, Some(sys_id), Some(ship_a), 0);
    sess_a.session_id = 95_001;
    sess_a.user_id = Some(char_a as u64);
    let bound = bound_sys(sys_id);
    let kw = HashMap::new();

    // Initial bootstrap + SetState drain moves stream to Live.
    handle(
        &ctx,
        &mut sess_a,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (init_tick, init_evs, _) = sys_h.drain_observer_updates(ship_a).await.unwrap();
    assert!(
        init_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "initial UpdateStateRequest must emit SetState"
    );
    let _ = sys_h.drain_observer_updates(ship_b).await.unwrap();
    while sys_rx.try_recv().is_ok() {}

    let tick_before = sys_h.ballpark.read().unwrap().current_tick;
    assert_eq!(init_tick, tick_before);

    // 20 consecutive UpdateStateRequest calls without a physics step MUST leave ballpark.current_tick
    // unchanged, coalesce redundant SetState pushes, and emit zero future-lead broadcasts/jolts to peers.
    for _ in 0..20 {
        let rsp = handle(
            &ctx,
            &mut sess_a,
            Some(&bound),
            "beyonce",
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("UpdateStateRequest ok");
        assert_eq!(rsp, PyRep::None);

        let (drain_tick, drain_evs, _) = sys_h.drain_observer_updates(ship_a).await.unwrap();
        assert_eq!(drain_tick, tick_before, "drain must not advance ballpark.current_tick");
        assert!(
            drain_evs.is_empty(),
            "consecutive Live UpdateStateRequest calls must be rate-limited/coalesced"
        );
    }

    assert_eq!(sys_h.ballpark.read().unwrap().current_tick, tick_before);
    assert!(
        sys_rx.try_recv().is_err(),
        "UpdateStateRequest bursts must emit zero peer TickBroadcast jolts"
    );
    let (peer_tick, peer_evs, _) = sys_h.drain_observer_updates(ship_b).await.unwrap();
    assert_eq!(peer_tick, tick_before);
    assert!(
        peer_evs.is_empty(),
        "peer observer queue must stay clean across UpdateStateRequest bursts"
    );

    // Lag recovery (`reset_update_state_rate_limit`) re-allows SetState cleanly at `tick_before`.
    reset_update_state_rate_limit(&ctx, sess_a.session_id, char_a);
    handle(
        &ctx,
        &mut sess_a,
        Some(&bound),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let (lag_tick, lag_evs, _) = sys_h.drain_observer_updates(ship_a).await.unwrap();
    assert_eq!(lag_tick, tick_before);
    assert!(
        lag_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "lag reset must permit immediate resync SetState at current_tick"
    );

    // Space exit via remove_ball and session cleanup purge context-scoped UpdateStateRequest guards (`BF-1169-04`).
    assert!(!ctx.systems.explicit_update_state_guards.lock().unwrap().is_empty());
    let _ = sys_h.remove_ball(ship_a).await;
    assert!(
        !ctx.systems
            .explicit_update_state_guards
            .lock()
            .unwrap()
            .contains_key(&(sess_a.session_id, char_a)),
        "remove_ball must clear explicit_update_state_guards for exited ship"
    );
}

#[tokio::test]
async fn update_state_rate_limit_is_isolated_per_server_context_and_cleared_on_session_exit() {
    let ctx_a = test_ctx();
    let ctx_b = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_001_001_i64;
    let ship_id = 91_002_001_i64;
    let shared_session_id = 77_001_u64;

    for ctx in [&ctx_a, &ctx_b] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters.values().next().cloned().expect("seed char");
            ch.character_id = char_id;
            ch.user_id = char_id as u64;
            ch.name = format!("Pilot-{char_id}");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
            put_item(
                snap,
                Some(ship_id),
                588,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            );
            snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.spawn_ball(test_ship_ball(ship_id, 588, DVec3::ZERO)).await;
    }

    let mut sess_a = make_session(char_id, 98_100_001, Some(sys_id), Some(ship_id), 0);
    sess_a.session_id = shared_session_id;
    let mut sess_b = make_session(char_id, 98_100_002, Some(sys_id), Some(ship_id), 0);
    sess_b.session_id = shared_session_id;
    let bound = bound_sys(sys_id);
    let kw = HashMap::new();

    for (ctx, sess) in [(&ctx_a, &mut sess_a), (&ctx_b, &mut sess_b)] {
        handle(
            ctx,
            sess,
            Some(&bound),
            "beyonce",
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let (_, evs, _) = ctx
            .systems
            .get_or_spawn(sys_id)
            .drain_observer_updates(ship_id)
            .await
            .unwrap();
        assert!(evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })));
    }
    // Same (session_id, char_id) across distinct ServerContexts must not collide, and clearing ctx_a leaves ctx_b intact.
    ctx_a.systems.clear_explicit_update_state_guards_for_session(shared_session_id);
    assert!(ctx_a.systems.explicit_update_state_guards.lock().unwrap().is_empty());
    assert!(!ctx_b.systems.explicit_update_state_guards.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pending_structure_view_outside_set_state_is_isolated_per_server_context_and_cleared_on_session_exit() {
    let ctx_a = test_ctx();
    let ctx_b = test_ctx();
    let sys_id = 30_000_144_u32;
    let corp_id = 98_100_910_i64;
    let char_id = 91_001_002_i64;
    let ship_id = 91_002_002_i64;
    let struct_id = 1_040_001_461_202_i64;
    let shared_session_id = 77_002_u64;

    for ctx in [&ctx_a, &ctx_b] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                struct_id,
                eve_db::UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: 35832,
                    solar_system_id: sys_id,
                    owner_corp_id: corp_id,
                    name: "GH1461 Context Isolation Astrahus".into(),
                    position: [250_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 2_500_000.0,
                    armor_hp: 2_500_000.0,
                    hull_hp: 2_500_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            let mut ch = snap.characters.values().next().cloned().expect("seed char");
            ch.character_id = char_id;
            ch.user_id = char_id as u64;
            ch.name = format!("Pilot-{char_id}");
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
            put_item(
                snap,
                Some(ship_id),
                588,
                char_id,
                struct_id,
                InventoryFlag::Hangar,
                1,
                true,
            );
        });
    }

    let mut sess_a = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), 0);
    sess_a.session_id = shared_session_id;
    sess_a.station_id = None;
    sess_a.structure_id = Some(struct_id);
    let mut sess_b = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), 0);
    sess_b.session_id = shared_session_id;
    sess_b.station_id = None;
    sess_b.structure_id = Some(struct_id);
    let bound = bound_sys(sys_id);
    let kw = HashMap::new();

    for (ctx, sess) in [(&ctx_a, &mut sess_a), (&ctx_b, &mut sess_b)] {
        handle(
            ctx,
            sess,
            Some(&bound),
            "beyonce",
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
    }

    assert!(ctx_a
        .pending_structure_view_outside_set_state
        .read()
        .unwrap()
        .contains_key(&(shared_session_id, char_id)));
    assert!(ctx_b
        .pending_structure_view_outside_set_state
        .read()
        .unwrap()
        .contains_key(&(shared_session_id, char_id)));

    // Session exit on ctx_a clears only ctx_a and leaves ctx_b untouched.
    ctx_a.clear_pending_structure_view_outside_set_state_for_session(shared_session_id);
    assert!(ctx_a
        .pending_structure_view_outside_set_state
        .read()
        .unwrap()
        .is_empty());
    assert_eq!(
        take_pending_structure_set_state(&ctx_b, shared_session_id, char_id),
        Some(struct_id)
    );

    // Transition/bootstrap clearing (`clear_space_ballpark_bootstrapped`) also evicts ctx_a without touching ctx_b.
    ctx_a.set_pending_structure_view_outside_set_state(shared_session_id, char_id, struct_id);
    ctx_b.set_pending_structure_view_outside_set_state(shared_session_id, char_id, struct_id);
    ctx_a.clear_space_ballpark_bootstrapped(char_id);
    assert!(ctx_a
        .pending_structure_view_outside_set_state
        .read()
        .unwrap()
        .is_empty());
    assert_eq!(
        take_pending_structure_set_state(&ctx_b, shared_session_id, char_id),
        Some(struct_id)
    );

    // Overflow (>4096 active entries) must NOT drop live recent sessions on ctx_a.
    for idx in 0..4_100_u64 {
        ctx_a.set_pending_structure_view_outside_set_state(900_000 + idx, char_id, struct_id);
    }
    assert_eq!(
        take_pending_structure_set_state(&ctx_a, 900_000, char_id),
        Some(struct_id),
        "live entries under 600s age must NOT be wiped when map grows past 4096"
    );
}

#[tokio::test]
async fn update_state_request_fallback_respawn_requires_space_residency_and_system_match() {
    let ctx = test_ctx();
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let corp_id = 98_100_900_i64;
    let kw = HashMap::new();

    // 1. Stale session in sys_a while DB character is docked at station -> must NOT spawn ghost ball.
    let docked_cid = 9_910_001_i64;
    let docked_ship = 9_910_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = docked_cid;
        ch.user_id = docked_cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_a;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.active_ship_id = docked_ship;
        snap.characters.insert(docked_cid, ch);
        snap.items.insert(
            docked_ship,
            eve_db::InventoryItemRecord {
                item_id: docked_ship,
                type_id: 587,
                owner_id: docked_cid,
                location_id: sys_a as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess_docked = make_session(docked_cid, corp_id, Some(sys_a), Some(docked_ship), 0);
    let bound_a = bound_sys(sys_a);
    handle(&ctx, &mut sess_docked, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let handle_a = ctx.systems.get_or_spawn(sys_a);
    assert!(
        handle_a.get_ball_snapshot(docked_ship).await.unwrap().is_none(),
        "docked DB character must not respawn ghost ship ball in space"
    );

    // 2. DB character docked in Upwell structure -> must NOT spawn ghost ball.
    let struct_cid = 9_910_011_i64;
    let struct_ship = 9_910_012_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = struct_cid;
        ch.user_id = struct_cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_a;
        ch.station_id = None;
        ch.structure_id = Some(1_000_000_990_001);
        ch.active_ship_id = struct_ship;
        snap.characters.insert(struct_cid, ch);
        snap.items.insert(
            struct_ship,
            eve_db::InventoryItemRecord {
                item_id: struct_ship,
                type_id: 587,
                owner_id: struct_cid,
                location_id: sys_a as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess_struct = make_session(struct_cid, corp_id, Some(sys_a), Some(struct_ship), 0);
    handle(&ctx, &mut sess_struct, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        handle_a.get_ball_snapshot(struct_ship).await.unwrap().is_none(),
        "structure-docked DB character must not respawn ghost ship ball in space"
    );

    // 3. Cross-system mismatch (DB character moved to sys_b while stale request hits sys_a) -> must NOT spawn.
    let cross_cid = 9_910_021_i64;
    let cross_ship = 9_910_022_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = cross_cid;
        ch.user_id = cross_cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_b;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = cross_ship;
        snap.characters.insert(cross_cid, ch);
        snap.items.insert(
            cross_ship,
            eve_db::InventoryItemRecord {
                item_id: cross_ship,
                type_id: 587,
                owner_id: cross_cid,
                location_id: sys_b as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess_cross = make_session(cross_cid, corp_id, Some(sys_a), Some(cross_ship), 0);
    handle(&ctx, &mut sess_cross, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        handle_a.get_ball_snapshot(cross_ship).await.unwrap().is_none(),
        "cross-system stale UpdateStateRequest must not spawn ghost ball in old system"
    );

    // 4. Ship active_ship_id mismatch or docked ship item location mismatch -> must NOT spawn.
    let stale_ship_cid = 9_910_031_i64;
    let old_ship = 9_910_032_i64;
    let current_ship = 9_910_033_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = stale_ship_cid;
        ch.user_id = stale_ship_cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_a;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = current_ship;
        snap.characters.insert(stale_ship_cid, ch);
        snap.items.insert(
            old_ship,
            eve_db::InventoryItemRecord {
                item_id: old_ship,
                type_id: 587,
                owner_id: stale_ship_cid,
                location_id: sys_a as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            current_ship,
            eve_db::InventoryItemRecord {
                item_id: current_ship,
                type_id: 587,
                owner_id: stale_ship_cid,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess_old_ship = make_session(stale_ship_cid, corp_id, Some(sys_a), Some(old_ship), 0);
    handle(&ctx, &mut sess_old_ship, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        handle_a.get_ball_snapshot(old_ship).await.unwrap().is_none(),
        "stale session.ship_id != active_ship_id must not spawn old ship ball"
    );
    let mut sess_docked_item = make_session(stale_ship_cid, corp_id, Some(sys_a), Some(current_ship), 0);
    handle(&ctx, &mut sess_docked_item, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        handle_a.get_ball_snapshot(current_ship).await.unwrap().is_none(),
        "ship item not located in solar system must not spawn ghost ball"
    );

    // 5. Valid in-space resident character and ship -> fallback ball respawn succeeds.
    let valid_cid = 9_910_041_i64;
    let valid_ship = 9_910_042_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = valid_cid;
        ch.user_id = valid_cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_a;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = valid_ship;
        snap.characters.insert(valid_cid, ch);
        snap.items.insert(
            valid_ship,
            eve_db::InventoryItemRecord {
                item_id: valid_ship,
                type_id: 587,
                owner_id: valid_cid,
                location_id: sys_a as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(valid_ship, [12_000.0, 0.0, 0.0]);
    });
    let mut sess_valid = make_session(valid_cid, corp_id, Some(sys_a), Some(valid_ship), 0);
    handle(&ctx, &mut sess_valid, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        handle_a.get_ball_snapshot(valid_ship).await.unwrap().is_some(),
        "valid in-space character + ship must still support fallback ball respawn"
    );
}

#[tokio::test]
async fn update_state_request_structure_view_outside_observes_structure_and_never_spawns_docked_ship() {
    let ctx = test_ctx();
    let sys_a = 30_000_142_u32;
    let corp_id = 98_100_900_i64;
    let struct_id = 1_040_001_448_001_i64;
    let destroyed_struct_id = 1_040_001_448_002_i64;
    let cid = 9_914_481_i64;
    let docked_ship = 9_914_482_i64;
    let invalid_cid = 9_914_483_i64;
    let invalid_ship = 9_914_484_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                solar_system_id: sys_a,
                owner_corp_id: corp_id,
                name: "GH1448 View Outside Astrahus".into(),
                position: [250_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 2_500_000.0,
                armor_hp: 2_500_000.0,
                hull_hp: 2_500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            destroyed_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: destroyed_struct_id,
                type_id: 35832,
                solar_system_id: sys_a,
                owner_corp_id: corp_id,
                name: "GH1448 Destroyed Astrahus".into(),
                position: [350_000.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );

        let template = snap.characters.values().next().expect("demo char").clone();
        let mut ch = template.clone();
        ch.character_id = cid;
        ch.user_id = cid as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_a;
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        ch.active_ship_id = docked_ship;
        snap.characters.insert(cid, ch);
        put_item(
            snap,
            Some(docked_ship),
            587,
            cid,
            struct_id,
            InventoryFlag::Hangar,
            1,
            true,
        );

        let mut ch_inv = template;
        ch_inv.character_id = invalid_cid;
        ch_inv.user_id = invalid_cid as u64;
        ch_inv.corporation_id = corp_id;
        ch_inv.solar_system_id = sys_a;
        ch_inv.station_id = None;
        ch_inv.structure_id = Some(destroyed_struct_id);
        ch_inv.active_ship_id = invalid_ship;
        snap.characters.insert(invalid_cid, ch_inv);
        put_item(
            snap,
            Some(invalid_ship),
            587,
            invalid_cid,
            destroyed_struct_id,
            InventoryFlag::Hangar,
            1,
            true,
        );
    });

    let bound_a = bound_sys(sys_a);
    let handle_a = ctx.systems.get_or_spawn(sys_a);

    let mut sess = make_session(cid, corp_id, Some(sys_a), Some(docked_ship), 0);
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    handle(&ctx, &mut sess, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();

    assert!(
        handle_a.get_ball_snapshot(struct_id).await.unwrap().is_some(),
        "View Outside UpdateStateRequest must ensure the structure ball exists in space"
    );
    assert!(
        handle_a.get_ball_snapshot(docked_ship).await.unwrap().is_none(),
        "View Outside must NEVER spawn the pilot's docked ship ball in space"
    );
    assert!(
        ctx.is_space_ballpark_bootstrapped(sess.session_id, cid, sys_a),
        "View Outside UpdateStateRequest must mark ballpark bootstrapped for deduplication"
    );
    assert_eq!(
        take_pending_structure_set_state(&ctx, sess.session_id, cid),
        Some(struct_id),
        "View Outside UpdateStateRequest must record a one-shot session SetState refresh"
    );
    let (_, evs, _) = handle_a.drain_observer_updates(struct_id).await.unwrap();
    assert!(
        !evs.iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "View Outside must NOT queue SetState into shared observer_events[{struct_id}], got {evs:?}"
    );
    let (_, ss_kv, ss_updates) = handle_a.get_set_state(struct_id).await.unwrap();
    assert_eq!(ss_kv.dict_get("ego").and_then(|v| v.as_i64()), Some(struct_id));
    let decoded_balls = match ss_kv.dict_get("state") {
        Some(PyRep::Bytes(b)) => eve_destiny::wire::decode_add_balls(b).unwrap().2,
        other => panic!("unexpected SetState state payload: {other:?}"),
    };
    assert!(decoded_balls.iter().any(|b| b.id == struct_id));
    assert!(
        !decoded_balls.iter().any(|b| b.id == docked_ship),
        "non-destructive get_set_state must NEVER contain docked_ship={docked_ship}"
    );
    assert!(matches!(ss_updates, PyRep::List(items) if items.len() == 1));
    assert!(
        handle_a
            .drain_observer_updates(docked_ship)
            .await
            .unwrap()
            .1
            .is_empty(),
        "docked ship must have zero observer updates during View Outside"
    );

    // Rapid follow-up explicit UpdateStateRequest for same (sys_a, structure_id) hits cooldown.
    handle(&ctx, &mut sess, Some(&bound_a), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        take_pending_structure_set_state(&ctx, sess.session_id, cid),
        None,
        "rapid repeat View Outside UpdateStateRequest must respect cooldown"
    );
    let (_, evs_dup, _) = handle_a.drain_observer_updates(struct_id).await.unwrap();
    assert!(
        !evs_dup
            .iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "rapid repeat View Outside UpdateStateRequest must respect cooldown"
    );

    // Destroyed or non-resident structure must not spawn ball or push SetState.
    let mut sess_destroyed = make_session(invalid_cid, corp_id, Some(sys_a), Some(invalid_ship), 0);
    sess_destroyed.session_id = 711;
    sess_destroyed.structure_id = Some(destroyed_struct_id);
    handle(
        &ctx,
        &mut sess_destroyed,
        Some(&bound_a),
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        handle_a
            .get_ball_snapshot(destroyed_struct_id)
            .await
            .unwrap()
            .is_none(),
        "destroyed structure must not spawn exterior ball on View Outside"
    );
    assert!(
        !ctx.is_space_ballpark_bootstrapped(sess_destroyed.session_id, invalid_cid, sys_a),
        "destroyed structure View Outside must not mark ballpark bootstrapped"
    );
}

#[tokio::test]
async fn break_ship_protection_emits_single_invuln_cancelled_and_mirrors_actor() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 9486_i64;
    let ship_id = 88686_i64;
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = char_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);
    });
    ctx.db
        .acquire_online_lock(1, char_id as u64, Some(char_id), true, now_unix)
        .expect("online lock for 9486");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut combat = eve_destiny::CombatState::default();
    combat.apply_jump_cloak_and_invuln_timers(50);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        combat,
        eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mirrored = sys.get_protection_snapshot(ship_id).await.unwrap().unwrap();
    assert!(mirrored.is_protected());
    assert!(mirrored.snapshot.has_gate_cloak());
    let _ = ctx.drain_notifications_for_char(char_id);

    let events = break_ship_protection(&ctx, &sys, char_id, ship_id, crate::system_actor::ProtectionBreakReason::ModuleActivation).await;
    assert!(events.gate_cloak_broken && events.undock_invuln_cancelled && events.invuln_cancelled);
    assert!(!sys.get_protection_snapshot(ship_id).await.unwrap().unwrap().is_protected());
    let notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(), 1);
    assert_eq!(notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // Second invocation emits no duplicate notifications.
    let events_again = break_ship_protection(&ctx, &sys, char_id, ship_id, crate::system_actor::ProtectionBreakReason::ModuleActivation).await;
    assert!(!events_again.invuln_cancelled && !events_again.any_cloak_broken() && ctx.drain_notifications_for_char(char_id).is_empty());

    // Actor `break_protection` + `emit_protection_break_notifications` clears ballpark invuln cleanly without duplicating on follow-up breaks.
    let _ = sys.modify_combat_state(ship_id, |cs| cs.undock_invulnerable_remaining_ticks = 40).await;
    assert!(sys.get_protection_snapshot(ship_id).await.unwrap().unwrap().is_protected());
    let ev_actor = sys.break_protection(ship_id, Some(char_id), crate::system_actor::ProtectionBreakReason::ModuleActivation, now_unix).await.unwrap();
    emit_protection_break_notifications(&ctx, char_id, ship_id, &ev_actor);
    assert!(ev_actor.invuln_cancelled && ev_actor.undock_invuln_cancelled);
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    let ev_noop = break_ship_protection(&ctx, &sys, char_id, ship_id, crate::system_actor::ProtectionBreakReason::ModuleActivation).await;
    assert!(!ev_noop.invuln_cancelled && ctx.drain_notifications_for_char(char_id).is_empty());
}

#[tokio::test]
async fn break_ship_protection_ess_gate_entry_preserves_location_invuln() {
    let ctx = test_ctx();
    let char_id = 91_384_889_i64;
    let ship_id = 1_091_384_889_i64;
    let sys_id = 30_000_142_u32;
    let now_unix = ctx.now_unix();
    let sys = ctx.systems.get_or_spawn(sys_id);

    let mut combat = eve_destiny::CombatState {
        is_cloaked: true,
        is_covert_cloak: true,
        undock_invulnerable_remaining_ticks: 40,
        warp_exit_invulnerable_remaining_ticks: 15,
        ..Default::default()
    };
    combat.gate_cloak_remaining_ticks = 0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        combat,
        eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // EssGateEntry with ship_id <= 0 and live ball MUST both preserve arrival/undock and warp-exit invuln.
    let _ = ctx.drain_notifications_for_char(char_id);

    let ev_zero = break_ship_protection(
        &ctx,
        &sys,
        char_id,
        0,
        crate::system_actor::ProtectionBreakReason::EssGateEntry,
    )
    .await;
    assert!(!ev_zero.invuln_cancelled);
    assert!(sys.get_protection_snapshot(ship_id).await.unwrap().unwrap().is_protected());
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let ev_ess = break_ship_protection(
        &ctx,
        &sys,
        char_id,
        ship_id,
        crate::system_actor::ProtectionBreakReason::EssGateEntry,
    )
    .await;
    assert!(ev_ess.covert_cloak_broken);
    assert!(!ev_ess.invuln_cancelled);
    assert!(!ev_ess.warp_exit_invuln_cancelled);
    let mirrored = sys.get_protection_snapshot(ship_id).await.unwrap().unwrap();
    assert!(!mirrored.has_any_cloak());
    assert!(mirrored.is_protected());
    assert_eq!(mirrored.snapshot.undock_invulnerable_remaining_ticks, 40);
    assert_eq!(mirrored.snapshot.warp_exit_invulnerable_remaining_ticks, 15);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Actor break consumes both undock_invuln and warp_exit_invuln atomically.
    let ev_undock = sys.break_protection(ship_id, Some(char_id), crate::system_actor::ProtectionBreakReason::ModuleActivation, now_unix).await.unwrap();
    assert!(ev_undock.undock_invuln_cancelled && ev_undock.invuln_cancelled && ev_undock.warp_exit_invuln_cancelled);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn break_ship_protection_concurrent_and_stale_system_invariants() {
    let ctx = Arc::new(test_ctx());
    let (char_id, ship_id, sys_id) = (91_486_001_i64, 1_091_486_001_i64, 30_000_142_u32);
    let now = ctx.now_unix();
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        test_ship_ball(ship_id, 587, DVec3::ZERO),
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            owner_id: char_id,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // 1. Concurrent callers observing one arrival/undock invuln window flip `invuln_cancelled` once across tasks.
    let _ = sys.modify_combat_state(ship_id, |cs| cs.undock_invulnerable_remaining_ticks = 60).await;
    let _ = sys.get_protection_snapshot(ship_id).await.unwrap();
    let (c1, s1, c2, s2) = (Arc::clone(&ctx), sys.clone(), Arc::clone(&ctx), sys.clone());
    let (ra, rb) = tokio::join!(
        tokio::spawn(async move {
            break_ship_protection(
                &c1, &s1, char_id, ship_id, crate::system_actor::ProtectionBreakReason::ModuleActivation,
            )
            .await
        }),
        tokio::spawn(async move {
            break_ship_protection(
                &c2, &s2, char_id, ship_id, crate::system_actor::ProtectionBreakReason::ModuleActivation,
            )
            .await
        })
    );
    let (ev_a, ev_b) = (ra.unwrap(), rb.unwrap());
    assert!(ev_a.invuln_cancelled ^ ev_b.invuln_cancelled, "{ev_a:?} / {ev_b:?}");
    assert!(ev_a.undock_invuln_cancelled ^ ev_b.undock_invuln_cancelled);
    assert!(!sys.get_protection_snapshot(ship_id).await.unwrap().unwrap().is_protected());

    // 2. Stale System A BreakProtection after TakeBall and closed System A Err(_) fallback both
    //    preserve System B's active arrival protection and emit no false OnInvulnCancelled notification.
    let sys_b_id = 30_000_144_u32;
    let sys_b = ctx.systems.get_or_spawn(sys_b_id);
    let taken = sys.take_ball(ship_id).await.unwrap();
    sys_b
        .spawn_ball_with_meta(
            taken.ball,
            eve_destiny::CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 30,
                undock_invulnerable_remaining_ticks: 60,
                ..Default::default()
            },
            eve_destiny::SlimItemMeta {
                char_id: Some(char_id),
                owner_id: char_id,
                category_id: 6,
                group_id: 25,
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let ev_stale_a = break_ship_protection(
        &ctx,
        &sys,
        char_id,
        ship_id,
        crate::system_actor::ProtectionBreakReason::SubwarpMovement,
    )
    .await;
    assert!(!ev_stale_a.invuln_cancelled);
    assert!(!ev_stale_a.gate_cloak_broken);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let _ = ctx.systems.stop_system(sys_id).await;
    let ev_closed_a = break_ship_protection(
        &ctx,
        &sys,
        char_id,
        ship_id,
        crate::system_actor::ProtectionBreakReason::SubwarpMovement,
    )
    .await;
    assert!(!ev_closed_a.invuln_cancelled);
    assert!(!ev_closed_a.gate_cloak_broken);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    let snap_b = sys_b.get_protection_snapshot(ship_id).await.unwrap().unwrap();
    assert!(snap_b.is_protected());
    assert!(snap_b.snapshot.has_gate_cloak());
    assert_eq!(snap_b.snapshot.undock_invulnerable_remaining_ticks, 60);
    assert_eq!(snap_b.snapshot.gate_cloak_remaining_ticks, 30);
    let _ = now;
}

#[tokio::test]
async fn resolve_loaded_system_prefers_db_and_active_ship_over_deployables() {
    let ctx = test_ctx();
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;

    let char_db = 91_486_201_i64;
    let ship_db_b = 1_091_486_201_i64;
    let beacon_a = 1_091_486_202_i64;
    let alt_ship_a = 1_091_486_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = char_db;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_db_b;
        ch.solar_system_id = sys_b;
        snap.characters.insert(char_db, ch);
    });

    let h_a = ctx.systems.get_or_spawn(sys_a);
    let h_b = ctx.systems.get_or_spawn(sys_b);

    // Lower-ID sys_a holds a deployable beacon (category 22) and secondary ship owned by char_db.
    h_a.spawn_ball_with_meta(
        Ball {
            id: beacon_a,
            type_id: 21096,
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_db),
            owner_id: char_db,
            category_id: 22,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h_a.spawn_ball_with_meta(
        Ball {
            id: alt_ship_a,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_db),
            owner_id: char_db,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h_b.spawn_ball_with_meta(
        Ball {
            id: ship_db_b,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_db),
            owner_id: char_db,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // `resolve_loaded_char_system` fails closed (`None`) when category-6 ship balls exist in
    // both `sys_a` and `sys_b`, while `resolve_loaded_active_ship_system` pinpoints `sys_b`.
    assert_eq!(ctx.systems.resolve_loaded_char_system(char_db), None);
    assert_eq!(
        ctx.systems.resolve_loaded_active_ship_system(char_db, ship_db_b),
        Some(sys_b)
    );

    // Loaded fallback when DB character record is absent: deployable in sys_a ignored vs single active ship in sys_b.
    let char_nodb = 91_486_202_i64;
    let beacon_nodb_a = 1_091_486_210_i64;
    let ship_nodb_b = 1_091_486_211_i64;
    h_a.spawn_ball_with_meta(
        Ball {
            id: beacon_nodb_a,
            type_id: 21096,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_nodb),
            owner_id: char_nodb,
            category_id: 22,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h_b.spawn_ball_with_meta(
        Ball {
            id: ship_nodb_b,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(char_nodb),
            owner_id: char_nodb,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(ctx.systems.resolve_loaded_char_system(char_nodb), Some(sys_b));
    assert_eq!(
        ctx.systems.resolve_loaded_active_ship_system(char_nodb, ship_nodb_b),
        Some(sys_b)
    );
}

#[tokio::test]
async fn rejected_subwarp_and_add_target_preserve_protection_while_committed_breaks() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let attacker_char = 91_486_301_i64;
    let attacker_ship = 1_091_486_301_i64;
    let target_char = 91_486_302_i64;
    let target_ship = 1_091_486_302_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch_a = snap.characters[&90_000_001].clone();
        ch_a.character_id = attacker_char;
        ch_a.station_id = None;
        ch_a.structure_id = None;
        ch_a.active_ship_id = attacker_ship;
        ch_a.solar_system_id = sys_id;
        snap.characters.insert(attacker_char, ch_a);

        let mut ch_t = snap.characters[&90_000_001].clone();
        ch_t.character_id = target_char;
        ch_t.station_id = None;
        ch_t.structure_id = None;
        ch_t.active_ship_id = target_ship;
        ch_t.solar_system_id = sys_id;
        snap.characters.insert(target_char, ch_t);

        put_item(
            snap,
            Some(attacker_ship),
            587,
            attacker_char,
            sys_id as i64,
            InventoryFlag::Hangar,
            1,
            true,
        );
        put_item(
            snap,
            Some(target_ship),
            587,
            target_char,
            sys_id as i64,
            InventoryFlag::Hangar,
            1,
            true,
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid, pos) in [
        (attacker_char, attacker_ship, DVec3::ZERO),
        (target_char, target_ship, DVec3::new(1_500.0, 0.0, 0.0)),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                char_id: Some(cid),
                owner_id: cid,
                category_id: 6,
                group_id: 25,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let mut sess = make_session(attacker_char, 98_000_001, Some(sys_id), Some(attacker_ship), 0);
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(22),
        character_id: attacker_char,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();

    sys.modify_combat_state(attacker_ship, |cs| cs.apply_jump_cloak_and_invuln_timers(60))
        .await
        .unwrap();

    // 1. Pre-validation / rejected `beyonce` subwarp RPCs preserve attacker protection, while valid `CmdGotoPoint` clears it.
    assert!(handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdGotoPoint",
        &[PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());
    assert!(
        sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    sess.station_id = Some(60_003_760);
    assert!(handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "beyonce",
        "CmdAlignTo",
        &[PyRep::Integer(target_ship)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());
    sess.station_id = None;
    assert!(
        sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    assert!(handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "beyonce",
        "CmdGotoPoint",
        &[PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(
        !sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    sys.modify_combat_state(attacker_ship, |cs| cs.apply_jump_cloak_and_invuln_timers(60))
        .await
        .unwrap();

    // 2. Rejected `dogmaIM.AddTarget` calls (ShipJammed, TargetInvulnerable, TargetingFailed out-of-range)
    //    must preserve attacker arrival protection.
    let _ = sys.try_update_combat_state(attacker_ship, |cs| cs.ecm_jammed_remaining_s = 5.0);
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_ship)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipJammed"
    );
    let _ = sys.try_update_combat_state(attacker_ship, |cs| cs.ecm_jammed_remaining_s = 0.0);
    assert!(
        sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    let _ = sys.try_update_combat_state(target_ship, |cs| cs.is_tethered = true);
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_ship)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetInvulnerable"
    );
    let _ = sys.try_update_combat_state(target_ship, |cs| cs.is_tethered = false);
    assert!(
        sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    let _ = sys.set_ball_position(target_ship, DVec3::new(9_999_999.0, 0.0, 0.0)).await;
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_ship)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetingFailed"
    );
    let _ = sys.set_ball_position(target_ship, DVec3::new(1_500.0, 0.0, 0.0)).await;
    assert!(
        sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    // 3. Committed `dogmaIM.AddTarget` (both initial lock and idempotent re-lock) clears attacker protection.
    assert!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_ship)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        !sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );

    sys.modify_combat_state(attacker_ship, |cs| cs.apply_jump_cloak_and_invuln_timers(60))
        .await
        .unwrap();
    assert!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_ship)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        !sys.get_combat_state(attacker_ship)
            .await
            .unwrap()
            .unwrap()
            .is_protected()
    );
}

#[tokio::test]
async fn pilot_commands_end_protection_while_non_pilot_movement_preserves_protection() {
    let mut ctx = test_ctx();
    let sb_tid = 998_177_u32;
    clone_type_with(
        &mut ctx.sde,
        587,
        sb_tid,
        77,
        7,
        5.0,
        0.0,
        &[(6, 10.0), (73, 10_000.0), (50, 1.0), (30, 1.0)],
    );
    Arc::make_mut(&mut ctx.sde)
        .types
        .get_mut(&sb_tid)
        .expect("custom shield booster type")
        .effect_ids = vec![13, 16];

    let sys_id = 30_000_142_u32;
    let char_id = 91_486_401_i64;
    let ship_id = 1_091_486_401_i64;
    let target_char = 91_486_402_i64;
    let target_ship = 1_091_486_402_i64;
    let target_char_obo = 91_486_403_i64;
    let target_ship_obo = 1_091_486_403_i64;
    let bumper_char = 91_486_404_i64;
    let bumper_ship = 1_091_486_404_i64;
    let sb_mod_id = 1_091_486_410_i64;
    let bm_id = 91_486_499_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [
            (char_id, ship_id),
            (target_char, target_ship),
            (target_char_obo, target_ship_obo),
            (bumper_char, bumper_ship),
        ] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            snap.characters.insert(cid, ch);
            put_item(
                snap,
                Some(sid),
                587,
                cid,
                sys_id as i64,
                InventoryFlag::Hangar,
                1,
                true,
            );
        }
        put_item(
            snap,
            Some(sb_mod_id),
            sb_tid,
            char_id,
            ship_id,
            InventoryFlag::MedSlot0,
            1,
            true,
        );
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "TestGrid".into(),
                solar_system_id: sys_id,
                x: 8_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid, pos) in [
        (target_char, target_ship, DVec3::new(1_500.0, 0.0, 0.0)),
        (target_char_obo, target_ship_obo, DVec3::new(1_600.0, 0.0, 0.0)),
    ] {
        sys.spawn_ball_with_meta(
            Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                char_id: Some(cid),
                owner_id: cid,
                category_id: 6,
                group_id: 25,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let spawn_player_protected = |vel: DVec3, speed_fraction: f64, mode: MovementMode| {
        let mut cs = eve_destiny::CombatState::default();
        cs.capacitor = 1_000.0;
        cs.max_capacitor = 1_000.0;
        cs.apply_jump_cloak_and_invuln_timers(60);
        let ball = Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: vel,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction,
            mode,
        };
        let mut meta = eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            owner_id: char_id,
            category_id: 6,
            group_id: 25,
            is_cloaked: true,
            ..Default::default()
        };
        meta.category_id = 6;
        (ball, cs, meta)
    };

    // --- Part A: Non-pilot movement & stationary no-ops preserve cloak + arrival protection ---

    // A1. Collision / bumping at high relative velocity keeps gate cloak & arrival protection.
    let (b0, c0, m0) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    sys.spawn_ball_with_meta(b0, c0, m0).await.unwrap();
    sys.spawn_ball_with_meta(
        Ball {
            id: bumper_ship,
            type_id: 587,
            position: DVec3::new(-140.0, 0.0, 0.0),
            velocity: DVec3::new(300.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(bumper_char),
            owner_id: bumper_char,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.tick().await.unwrap();
    let cs_bumped = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_bumped.is_cloaked && cs_bumped.gate_cloak_remaining_ticks == 59);
    assert_eq!(cs_bumped.undock_invulnerable_remaining_ticks, 59);
    assert!(cs_bumped.is_protected());

    // Warp-exit invulnerability (not collision-exempt) experiences real collision velocity
    // (`> 10 m/s`) yet also preserves arrival protection across physics ticks.
    let (mut b_warp, _, _) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    b_warp.position = DVec3::ZERO;
    let mut cs_warp_in = eve_destiny::CombatState::default();
    cs_warp_in.warp_exit_invulnerable_remaining_ticks = 10;
    sys.spawn_ball_with_meta(
        b_warp,
        cs_warp_in,
        eve_destiny::SlimItemMeta {
            char_id: Some(char_id),
            owner_id: char_id,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.spawn_ball_with_meta(
        Ball {
            id: bumper_ship,
            type_id: 587,
            position: DVec3::new(-140.0, 0.0, 0.0),
            velocity: DVec3::new(300.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta {
            char_id: Some(bumper_char),
            owner_id: bumper_char,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sys.tick().await.unwrap();
    let (_, bumped_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_bumped_warp = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(bumped_ball.velocity.length() > 10.0, "ship must experience collision velocity");
    assert_eq!(cs_bumped_warp.warp_exit_invulnerable_remaining_ticks, 9);
    assert!(cs_bumped_warp.is_protected());

    // Move bumper well clear of the grid for subsequent steps.
    sys.spawn_ball_with_meta(
        Ball {
            id: bumper_ship,
            type_id: 587,
            position: DVec3::new(0.0, 500_000.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta::default(),
    )
    .await
    .unwrap();

    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(22),
        character_id: char_id,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();

    // A2. Overlap ejection (`250 m/s` in `Stop` mode with `speed_fraction = 0.0`) preserves
    //     cloak & protection across physics coasting to rest AND across stationary `CmdStop` /
    //     `CmdSetSpeedFraction(0.0)` both while coasting and after settling at rest.
    let (b1, c1, m1) = spawn_player_protected(DVec3::new(250.0, 0.0, 0.0), 0.0, MovementMode::Stop);
    sys.spawn_ball_with_meta(b1, c1, m1).await.unwrap();
    sys.tick().await.unwrap();
    let cs_ejected = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_ejected) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ball_ejected.velocity.length() > 10.0, "ejected ship still coasting");
    assert_eq!(ball_ejected.speed_fraction, 0.0);
    assert!(cs_ejected.is_cloaked && cs_ejected.gate_cloak_remaining_ticks == 59);
    assert_eq!(cs_ejected.undock_invulnerable_remaining_ticks, 59);
    assert!(cs_ejected.is_protected());
    assert!(handle(&ctx, &mut sess, Some(&bound), "beyonce", "CmdStop", &[], &kw).await.unwrap().is_ok());
    assert!(
        handle(
            &ctx,
            &mut sess,
            Some(&bound),
            "beyonce",
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    for _ in 0..34 {
        sys.tick().await.unwrap();
    }
    let (_, ball_rest) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        ball_rest.velocity.length() < 0.05 && ball_rest.speed_fraction == 0.0,
        "ejected ship must coast to rest in Stop mode (got {:?})",
        ball_rest.velocity
    );
    assert!(handle(&ctx, &mut sess, Some(&bound), "beyonce", "CmdStop", &[], &kw).await.unwrap().is_ok());
    assert!(
        handle(
            &ctx,
            &mut sess,
            Some(&bound),
            "beyonce",
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let cs_rest = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_rest.is_cloaked && cs_rest.gate_cloak_remaining_ticks == 25);
    assert_eq!(cs_rest.undock_invulnerable_remaining_ticks, 25);
    assert!(cs_rest.is_protected());

    // A3. Same-system in-space GM relocation via `slash.SlashCmd` (`/tr` / `/moveme`) moves
    //     the player ship while preserving gate cloak & arrival protection; stationary no-op
    //     `CmdStop` / `CmdSetSpeedFraction(0.0)` likewise leave cloak & protection intact.
    let (b2, c2, m2) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    sys.spawn_ball_with_meta(b2, c2, m2).await.unwrap();
    let mut gm_sess = sess.clone();
    gm_sess.role = service_roles::ROLE_ADMIN | service_roles::ROLE_GMH;
    for slash_cmd in [format!("/tr {target_ship}"), format!("/moveme {target_ship_obo}")] {
        assert!(
            crate::services::auth_cache::handle(
                &ctx,
                &mut gm_sess,
                "slash",
                "SlashCmd",
                &[PyRep::String(slash_cmd.clone())],
                &kw,
            )
            .await
            .unwrap()
            .is_ok(),
            "same-system {slash_cmd} must succeed"
        );
        let (_, moved_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            moved_ball.position.length() > 1_000.0,
            "same-system {slash_cmd} must relocate ship in Ballpark"
        );
        let cs_gm = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs_gm.is_cloaked && cs_gm.gate_cloak_remaining_ticks == 60);
        assert_eq!(cs_gm.undock_invulnerable_remaining_ticks, 60);
        assert!(cs_gm.is_protected());
    }
    // Stationary pilot no-ops (`CmdStop` and `CmdSetSpeedFraction(0.0)`) run atomically in
    // SystemActor (clearing `is_emergency_warping` while leaving cloak/protection intact and
    // suppressing redundant offline `DoDestinyUpdate` notifications).
    let (b_ew, mut c_ew, m_ew) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    c_ew.is_emergency_warping = true;
    sys.spawn_ball_with_meta(b_ew, c_ew, m_ew).await.unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(handle(&ctx, &mut sess, Some(&bound), "beyonce", "CmdStop", &[], &kw).await.unwrap().is_ok());
    assert!(
        handle(
            &ctx,
            &mut sess,
            Some(&bound),
            "beyonce",
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let cs_nonpilot = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_nonpilot.is_emergency_warping, "stationary CmdStop must still clear is_emergency_warping in actor");
    assert!(cs_nonpilot.is_cloaked && cs_nonpilot.gate_cloak_remaining_ticks == 60);
    assert_eq!(cs_nonpilot.undock_invulnerable_remaining_ticks, 60);
    assert!(cs_nonpilot.is_protected());
    assert!(
        !ctx.drain_notifications_for_char(char_id).iter().any(|n| {
            matches!(
                n.event.as_str(),
                "DoDestinyUpdate" | "OnJumpCloakCancelled" | "OnInvulnCancelled"
            )
        }),
        "stationary CmdStop / CmdSetSpeedFraction(0.0) must not emit destiny or break notices"
    );

    // --- Part B: Every pilot action immediately breaks gate cloak & arrival protection ---
    let nav_cases: [(&str, Vec<PyRep>); 7] = [
        ("CmdGotoPoint", vec![PyRep::Real(5_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ("CmdGotoDirection", vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
        ("CmdAlignTo", vec![PyRep::Integer(target_ship)]),
        ("CmdOrbit", vec![PyRep::Integer(target_ship), PyRep::Real(2_500.0)]),
        ("CmdFollowBall", vec![PyRep::Integer(target_ship), PyRep::Real(1_000.0)]),
        ("CmdSetSpeedFraction", vec![PyRep::Real(0.5)]),
        ("CmdGotoBookmark", vec![PyRep::Integer(bm_id)]),
    ];
    for (cmd_name, cmd_args) in nav_cases {
        let (b, c, m) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
        sys.spawn_ball_with_meta(b, c, m).await.unwrap();
        let call_res = if cmd_name == "CmdGotoBookmark" {
            crate::services::space_entity_ops::handle(
                &ctx, &mut sess, Some(&bound), "beyonce", cmd_name, &cmd_args, &kw,
            )
            .await
        } else {
            handle(&ctx, &mut sess, Some(&bound), "beyonce", cmd_name, &cmd_args, &kw).await
        };
        assert!(
            call_res.unwrap().is_ok(),
            "{cmd_name} must succeed"
        );
        let cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(
            !cs.is_cloaked
                && cs.gate_cloak_remaining_ticks == 0
                && cs.undock_invulnerable_remaining_ticks == 0
                && !cs.is_protected(),
            "{cmd_name} must break gate cloak and arrival protection"
        );
    }

    // Active `CmdStop` while moving breaks cloak & protection.
    let (bm, cm, mm) = spawn_player_protected(
        DVec3::new(100.0, 0.0, 0.0),
        1.0,
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    sys.spawn_ball_with_meta(bm, cm, mm).await.unwrap();
    assert!(handle(&ctx, &mut sess, Some(&bound), "beyonce", "CmdStop", &[], &kw).await.unwrap().is_ok());
    let cs_stop = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_stop.is_cloaked && cs_stop.gate_cloak_remaining_ticks == 0 && !cs_stop.is_protected());

    // Active `CmdSetSpeedFraction(0.0)` while moving (`speed_fraction > 0.0`) breaks cloak & protection.
    let (bm_sf, cm_sf, mm_sf) = spawn_player_protected(
        DVec3::new(100.0, 0.0, 0.0),
        1.0,
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    sys.spawn_ball_with_meta(bm_sf, cm_sf, mm_sf).await.unwrap();
    assert!(
        handle(
            &ctx,
            &mut sess,
            Some(&bound),
            "beyonce",
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let cs_sf0 = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_sf0.is_cloaked && cs_sf0.gate_cloak_remaining_ticks == 0 && !cs_sf0.is_protected());

    // `CmdWarpToStuff` (> 150 km celestial) breaks gate cloak & protection.
    let far_celestial_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.position.length() >= 200_000.0)
        .expect("far celestial in Jita")
        .celestial_id;
    let (bw, cw, mw) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    sys.spawn_ball_with_meta(bw, cw, mw).await.unwrap();
    assert!(
        handle(
            &ctx,
            &mut sess,
            Some(&bound),
            "beyonce",
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(far_celestial_id)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    let cs_warp = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_warp.is_cloaked && cs_warp.gate_cloak_remaining_ticks == 0 && !cs_warp.is_protected());

    // Target lock (`dogmaIM.AddTarget` and `dogmaIM.AddTargetOBO`) + module activation (`dogmaIM.Activate`).
    let action_cases = [
        ("AddTarget", vec![PyRep::Integer(target_ship)]),
        ("AddTargetOBO", vec![PyRep::Integer(ship_id), PyRep::Integer(target_ship_obo)]),
        ("Activate", vec![PyRep::Integer(sb_mod_id), PyRep::String("defaultEffect".into())]),
    ];
    for (method, call_args) in action_cases {
        let (ba, ca, ma) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
        sys.spawn_ball_with_meta(ba, ca, ma).await.unwrap();
        assert!(
            crate::services::inventory_fitting::handle(
                &ctx,
                &mut sess,
                None,
                "dogmaIM",
                method,
                &call_args,
                &kw,
            )
            .await
            .unwrap()
            .is_ok(),
            "dogmaIM.{method} must succeed"
        );
        let cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(
            !cs.is_cloaked
                && cs.gate_cloak_remaining_ticks == 0
                && cs.undock_invulnerable_remaining_ticks == 0
                && !cs.is_protected(),
            "dogmaIM.{method} must break gate cloak and arrival protection"
        );
    }

    // --- Part C: Rejected navigation commands never strip cloak or invulnerability, and
    //     cross-system GM `/tr` resets protection (`auth_cache.rs:1985-2042`) ---
    let mut warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000_000.0, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = warp_mode {
        *phase = eve_destiny::WarpPhase::Cruising;
    }
    let (bw_rej, mut cw_rej, mw_rej) = spawn_player_protected(DVec3::ZERO, 1.0, warp_mode);
    cw_rej.warp_exit_invulnerable_remaining_ticks = 25;
    sys.spawn_ball_with_meta(bw_rej, cw_rej, mw_rej).await.unwrap();
    let expected_warp_err = "ShipAlreadyInWarp";
    for (svc_kind, cmd_name, cmd_args) in [
        ("ops", "CmdGotoBookmark", vec![PyRep::Integer(bm_id)]),
        ("nav", "CmdStop", vec![]),
        ("nav", "CmdSetSpeedFraction", vec![PyRep::Real(0.5)]),
        ("nav", "CmdGotoDirection", vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ] {
        let rej = if svc_kind == "ops" {
            crate::services::space_entity_ops::handle(
                &ctx, &mut sess, Some(&bound), "beyonce", cmd_name, &cmd_args, &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
        } else {
            handle(&ctx, &mut sess, Some(&bound), "beyonce", cmd_name, &cmd_args, &kw)
                .await
                .unwrap()
                .unwrap_err()
        };
        assert_eq!(rej.code, expected_warp_err, "{cmd_name} must reject active warp");
        let cs_kept = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(
            cs_kept.is_cloaked
                && cs_kept.gate_cloak_remaining_ticks == 60
                && cs_kept.undock_invulnerable_remaining_ticks == 60
                && cs_kept.warp_exit_invulnerable_remaining_ticks == 25,
            "failed {cmd_name} must leave cloak and invulnerability intact"
        );
    }

    // Cross-system GM `/tr <other_sys_id>` takes the ball and spawns a fresh uncloaked CombatState.
    let (bx, cx, mx) = spawn_player_protected(DVec3::ZERO, 0.0, MovementMode::Stop);
    sys.spawn_ball_with_meta(bx, cx, mx).await.unwrap();
    let other_sys_id = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| sid != sys_id)
        .expect("secondary SDE solar system");
    assert!(
        crate::services::auth_cache::handle(
            &ctx,
            &mut gm_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(format!("/tr {other_sys_id}"))],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(sys.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let other_sys = ctx.systems.get_or_spawn(other_sys_id);
    let cs_cross = other_sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_cross.is_cloaked && cs_cross.gate_cloak_remaining_ticks == 0 && !cs_cross.is_protected());
}

#[tokio::test]
async fn update_state_cooldown_uses_sim_clock_domain_not_wall_time_rt91() {
    let ctx = test_ctx();
    ctx.set_clock_paused(true);
    let sys_id = 30_000_142_u32;
    let char_id = 91_196_091_i64;
    let ship_id = 91_196_191_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot-{char_id}");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(
            snap,
            Some(ship_id),
            588,
            char_id,
            i64::from(sys_id),
            InventoryFlag::None,
            1,
            true,
        );
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(test_ship_ball(ship_id, 588, DVec3::ZERO))
        .await;
    // Initialize `sim_unix_secs` via a first physics step so ticks and `sim_unix_secs` co-advance.
    sys_h.step_manual(1.0).await.expect("initial step_manual");

    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    sess.session_id = 91_196_901;
    sess.user_id = Some(char_id as u64);
    let bound = bound_sys(sys_id);
    let kw = HashMap::new();

    // 1. Initial explicit UpdateStateRequest bootstraps and records `last_push_tick`.
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let (_, init_evs, _) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    assert!(init_evs
        .iter()
        .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })));

    // 2. Wall time advancing while sim ticks & sim clock are frozen holds cooldown.
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sys_h
            .drain_observer_updates(ship_id)
            .await
            .unwrap()
            .1
            .is_empty(),
        "wall time sleep without sim advance must hold RT-91 UpdateStateRequest cooldown"
    );

    // 3. Advancing 4 sim steps (< 5 ticks & < 5 sim_unix_secs) still holds cooldown.
    for _ in 0..4 {
        sys_h.step_manual(1.0).await.expect("step_manual");
    }
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sys_h
            .drain_observer_updates(ship_id)
            .await
            .unwrap()
            .1
            .is_empty(),
        "4 sim steps (< UPDATE_STATE_COOLDOWN_TICKS=5) must keep UpdateStateRequest in cooldown"
    );

    // 4. Advancing the 5th sim step (>= 5 ticks & >= 5 sim_unix_secs) deterministically clears cooldown.
    sys_h.step_manual(1.0).await.expect("step_manual 5th");
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let (_, cleared_evs, _) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    assert!(
        cleared_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "5 sim steps must clear RT-91 UpdateStateRequest cooldown deterministically"
    );

    // 5. While freshly in cooldown again, both `reset_update_state_rate_limit` and `force_bypass` bypass immediately.
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(sys_h.drain_observer_updates(ship_id).await.unwrap().1.is_empty());

    push_set_state_with_guard_and_bootstrap(&ctx, &sess, char_id, sys_id, ship_id, &sys_h, true, true, true)
        .await
        .unwrap();
    assert!(
        sys_h
            .drain_observer_updates(ship_id)
            .await
            .unwrap()
            .1
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "force_bypass_cooldown=true must bypass active cooldown"
    );

    reset_update_state_rate_limit(&ctx, sess.session_id, char_id);
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sys_h
            .drain_observer_updates(ship_id)
            .await
            .unwrap()
            .1
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "reset_update_state_rate_limit must clear RT-91 cooldown immediately"
    );

    // 6. Unpaused wall clock under TiDi = 0.25: advancing wall time (+60s) must NOT expire cooldown
    //    before 5 true simulation ticks (19 steps = 4.75 sim-s = 4 ticks -> still suppressed;
    //    20th step = 5.00 sim-s = 5 ticks -> deterministically clears cooldown).
    ctx.set_clock_paused(false);
    crate::services::serverops_cluster::set_system_tidi_factor(&ctx, sys_id, 0.25);
    let tick_start = sys_h.view().current_tick;
    ctx.clock().advance_secs(60);
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    for _ in 0..19 {
        sys_h.step_manual(1.0).await.expect("tidi 0.25 step");
    }
    assert_eq!(sys_h.view().current_tick, tick_start + 4);
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sys_h.drain_observer_updates(ship_id).await.unwrap().1.is_empty(),
        "19 steps at 0.25 TiDi (4 sim ticks) must hold RT-91 cooldown despite unpaused +60s wall jump"
    );

    ctx.clock().advance_secs(15);
    sys_h.step_manual(1.0).await.expect("tidi 0.25 20th step");
    assert_eq!(sys_h.view().current_tick, tick_start + 5);
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sys_h
            .drain_observer_updates(ship_id)
            .await
            .unwrap()
            .1
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "20th step at 0.25 TiDi (5 sim ticks) must clear RT-91 cooldown"
    );

    // 7. Cross-system > 4,096 guard GC prunes strictly per each guard's own solar system tick domain:
    //    advance sys_id >= 650 ticks, keep sys_b at tick 2 -> sys_id stale guards & dead-sys guards
    //    prune cleanly while sys_b's fresh guard survives despite caller sys_id being >= 650 ticks ahead.
    crate::services::serverops_cluster::set_system_tidi_factor(&ctx, sys_id, 1.0);
    for _ in 0..650 {
        sys_h.step_manual_tick_only(1.0).await.expect("advance sys_id tick");
    }
    let sys_b = 30_000_144_u32;
    let sys_b_h = ctx.systems.get_or_spawn(sys_b);
    sys_b_h.step_manual_tick_only(1.0).await.expect("sys_b tick 1");
    sys_b_h.step_manual_tick_only(1.0).await.expect("sys_b tick 2");
    let sys_b_tick = sys_b_h.view().current_tick;
    let fresh_key = (888_001_u64, char_id);
    let dead_key = (888_002_u64, char_id);
    {
        let mut guards = ctx
            .systems
            .explicit_update_state_guards
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for i in 0..4_105_u64 {
            guards.insert(
                (900_000 + i, char_id),
                crate::system_actor::ExplicitUpdateStateGuard {
                    sys_id,
                    ship_id,
                    last_push_tick: 1,
                },
            );
        }
        guards.insert(
            fresh_key,
            crate::system_actor::ExplicitUpdateStateGuard {
                sys_id: sys_b,
                ship_id: ship_id + 1,
                last_push_tick: sys_b_tick,
            },
        );
        guards.insert(
            dead_key,
            crate::system_actor::ExplicitUpdateStateGuard {
                sys_id: 30_999_999,
                ship_id: ship_id + 2,
                last_push_tick: 1,
            },
        );
    }
    handle(&ctx, &mut sess, Some(&bound), "beyonce", "UpdateStateRequest", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let guards = ctx
        .systems
        .explicit_update_state_guards
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(guards.contains_key(&fresh_key), "fresh guard in slower sys_b must survive cross-system GC");
    assert!(!guards.contains_key(&dead_key), "inactive-system guard must be pruned");
    assert!(!guards.contains_key(&(900_000, char_id)), "stale guard in sys_id must be pruned");
    assert!(guards.len() <= 4);
}

#[tokio::test]
async fn cold_system_admin_get_bubble_id_rejects_without_spawning() {
    let ctx = test_ctx();
    let sys_id: u32 = 30_000_144;
    let ship_id: i64 = 889_101;
    let other_ball_id: i64 = 889_102;
    assert!(ctx.systems.get_handle(sys_id).is_none());

    let gm_sess = make_session(
        9402,
        98_000_001,
        Some(sys_id),
        Some(ship_id),
        service_roles::ROLE_GMH,
    );
    let bound = bound_sys(sys_id);

    let err_self = super::handle_admin_get_bubble_id(&ctx, &gm_sess, Some(&bound), &[])
        .await
        .expect_err("cold system must reject self AdminGetBubbleID");
    assert_eq!(err_self.code, "BallNotFound");
    assert!(ctx.systems.get_handle(sys_id).is_none());

    let err_other = super::handle_admin_get_bubble_id(
        &ctx,
        &gm_sess,
        Some(&bound),
        &[PyRep::Integer(other_ball_id)],
    )
    .await
    .expect_err("cold system must reject other-ball AdminGetBubbleID");
    assert_eq!(err_other.code, "BallNotVisible");
    assert!(ctx.systems.get_handle(sys_id).is_none());
}

#[tokio::test]
async fn cold_system_in_space_rpcs_reject_without_spawning() {
    use crate::services::station_structure::{
        handle as handle_station_structure,
        structure_ansiblex_navigation::{
            handle_cmd_jump_through_structure_stargate, is_sovereign_nullsec_for_ansiblex,
            ANSIBLEX_TYPE_ID,
        },
        structure_upwell_ops::{load_ext_state, save_ext_state},
    };
    use eve_db::{ObservatorySiteState, UpwellStructureRecord};

    let mut ctx = test_ctx();
    let kw = HashMap::new();
    let char_id: i64 = 955_001;
    let corp_id: i64 = 98_000_001;
    let ship_id: i64 = 955_101;
    let cont_id: i64 = 955_102;
    let cont_tid: u32 = 955_901;

    clone_type_with(&mut ctx.sde, 587, cont_tid, 340, 22, 100.0, 1_000.0, &[]);
    ctx.db
        .provision_gm_account("cold_rpc_pilot", "pw", char_id, "ColdRpcPilot");

    let dock_sys: u32 = 30_000_142;
    let anchor_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id >= 30_000_000
                && s.solar_system_id < 31_000_000
                && s.security_status <= 0.0
                && s.solar_system_id != 30_100_000
                && s.solar_system_id != dock_sys
        })
        .map(|s| s.solar_system_id)
        .expect("nullsec system for anchor test");

    let sov_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            sid != dock_sys
                && sid != anchor_sys
                && is_sovereign_nullsec_for_ansiblex(&ctx.sde, sid)
        })
        .take(80)
        .collect();
    let (jb_sys_a, jb_sys_b) = sov_systems
        .iter()
        .find_map(|&a| {
            sov_systems.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(&ctx, a, b);
                (a != b && d > 0.0 && d <= 4.5).then_some((a, b))
            })
        })
        .expect("pair of sov null systems within 4.5 LY");

    let obs_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != dock_sys
                && sid != anchor_sys
                && sid != jb_sys_a
                && sid != jb_sys_b
        })
        .expect("distinct observatory system");
    let fighter_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != dock_sys
                && sid != anchor_sys
                && sid != jb_sys_a
                && sid != jb_sys_b
                && sid != obs_sys
        })
        .expect("distinct fighter system");
    let ess_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id >= 30_000_000
                && s.solar_system_id < 31_000_000
                && s.security_status <= 0.0
                && s.solar_system_id != 30_100_000
                && s.solar_system_id != dock_sys
                && s.solar_system_id != anchor_sys
                && s.solar_system_id != jb_sys_a
                && s.solar_system_id != jb_sys_b
                && s.solar_system_id != obs_sys
                && s.solar_system_id != fighter_sys
        })
        .map(|s| s.solar_system_id)
        .expect("distinct nullsec system for ESS reserve test");
    let exp_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != dock_sys
                && sid != anchor_sys
                && sid != jb_sys_a
                && sid != jb_sys_b
                && sid != obs_sys
                && sid != fighter_sys
                && sid != ess_sys
        })
        .expect("distinct expedition system");

    let jb_a: i64 = 1_000_000_955_001;
    let jb_b: i64 = 1_000_000_955_002;
    let obs_site_id = 1_899_000_000 + i64::from(obs_sys);
    let obs_gate_id = obs_site_id + 2;

    let user_id = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&char_id].user_id;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = dock_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        put_item(
            snap,
            Some(ship_id),
            587,
            char_id,
            i64::from(dock_sys),
            InventoryFlag::None,
            1,
            true,
        );
        put_item(
            snap,
            Some(cont_id),
            cont_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            1,
            false,
        );
        for (sid, sys) in [(jb_a, jb_sys_a), (jb_b, jb_sys_b)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: ANSIBLEX_TYPE_ID,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("ColdAnsiblex-{sid}"),
                    position: [10_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    ..Default::default()
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        ext.jump_bridge_links.insert(jb_b, jb_a);
        save_ext_state(snap, &ext);
        snap.fob_observatory_state.observatory_sites.insert(
            obs_site_id,
            ObservatorySiteState {
                site_id: obs_site_id,
                solar_system_id: obs_sys,
                gate_id: obs_gate_id,
                ..Default::default()
            },
        );
        uid
    });

    let mut sess = make_session(char_id, corp_id, Some(dock_sys), Some(ship_id), 0);
    sess.user_id = Some(user_id);

    // 1. station_structure Dock
    assert!(ctx.systems.get_handle(dock_sys).is_none());
    let dock_err = handle_station_structure(
        &ctx,
        &mut sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(60_003_760)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dock_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(dock_sys).is_none());

    // 2. station_structure AnchorStructure (secure container)
    sess.solar_system_id = Some(anchor_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = anchor_sys;
        }
    });
    assert!(ctx.systems.get_handle(anchor_sys).is_none());
    let anchor_err = handle_station_structure(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(cont_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(anchor_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(anchor_sys).is_none());

    // 3. structure_ansiblex_navigation::handle_cmd_jump_through_structure_stargate
    sess.solar_system_id = Some(jb_sys_a);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jb_sys_a;
        }
    });
    assert!(ctx.systems.get_handle(jb_sys_a).is_none());
    assert!(ctx.systems.get_handle(jb_sys_b).is_none());
    let jb_err = handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
        .await
        .unwrap_err();
    assert_eq!(jb_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(jb_sys_a).is_none());
    assert!(ctx.systems.get_handle(jb_sys_b).is_none());

    // 4. pve_fob_observatory::try_activate_observatory_gate
    sess.solar_system_id = Some(obs_sys);
    assert!(ctx.systems.get_handle(obs_sys).is_none());
    let gate_err = crate::services::pve_fob_observatory::try_activate_observatory_gate(
        &ctx,
        &sess,
        obs_sys,
        obs_gate_id,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(gate_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(obs_sys).is_none());

    // 5. combat_fighters in-space fighter RPC
    sess.solar_system_id = Some(fighter_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = fighter_sys;
        }
    });
    assert!(ctx.systems.get_handle(fighter_sys).is_none());
    let fighter_err = crate::services::combat_crimewatch::combat_fighters::handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(fighter_err.code, "ShipNotInSpace");
    assert!(ctx.systems.get_handle(fighter_sys).is_none());

    // 6. essMgr.RequestUnlockReserveBank (space_destiny.rs:443)
    sess.solar_system_id = Some(ess_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ess_sys;
        }
    });
    assert!(ctx.systems.get_handle(ess_sys).is_none());
    let ess_err = handle(
        &ctx,
        &mut sess,
        None,
        "essMgr",
        "RequestUnlockReserveBank",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(ess_err.code, "OutsideEssLinkRange");
    assert!(ctx.systems.get_handle(ess_sys).is_none());

    // 7. pve_expeditions::validate_and_advance_pve_site_room (pve_expeditions.rs:1316)
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            exp_sys,
            vec![crate::services::pve_exploration::PveSiteInstance {
                site_id: "COLD-EXP-DED".into(),
                solar_system_id: exp_sys,
                ded_rating: Some(4),
                position: [50_000.0, 0.0, 0.0],
                created_unix: ctx.now_unix(),
                lifetime_seconds: 3_600,
                ..Default::default()
            }],
        );
    });
    sess.solar_system_id = Some(exp_sys);
    assert!(ctx.systems.get_handle(exp_sys).is_none());
    let exp_err = crate::services::pve_expeditions::validate_and_advance_pve_site_room(
        &ctx,
        &mut sess,
        exp_sys,
        "COLD-EXP-DED",
    )
    .await
    .unwrap_err();
    assert_eq!(exp_err.code, "NotInSpace");
    assert!(ctx.systems.get_handle(exp_sys).is_none());
}

#[tokio::test]
async fn cold_system_ticks_cleanups_and_tourney_return_do_not_spawn() {
    use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, StructureAccessProfile,
    };
    use crate::services::tourney_service::tourney_common::{
        load_store, save_store, TourneyMatch, TourneyMatchState, TourneyPilot,
    };
    use eve_db::UpwellStructureRecord;

    let ctx = test_ctx();
    let kw = HashMap::new();

    let (home_st_id, home_sys) = ctx
        .sde
        .stations
        .values()
        .min_by_key(|s| s.station_id)
        .map(|s| (s.station_id, s.solar_system_id))
        .expect("SDE station");
    let def_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| (30_000_000..31_000_000).contains(&sid) && sid != 30_100_000 && sid != home_sys)
        .expect("defense test system");
    let hack_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
        })
        .expect("hack test system");
    let arena_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
                && sid != hack_sys
        })
        .expect("arena test system");
    let wh_step_sys: u32 = 31_000_005;
    let wh_dt_sys: u32 = 31_000_006;

    // 1. trigger_system_structures_auto_defense
    let def_sid: i64 = 1_000_000_956_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            def_sid,
            UpwellStructureRecord {
                structure_id: def_sid,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: def_sys,
                name: "ColdDefCitadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            def_sid,
            StructureAccessProfile {
                has_defense_config: true,
                auto_defense_enabled: true,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });
    assert!(ctx.systems.get_handle(def_sys).is_none());
    crate::services::structure_weapons_ops::trigger_system_structures_auto_defense(&ctx, def_sys).await;
    assert!(ctx.systems.get_handle(def_sys).is_none());

    // 2. step_wormhole_pve_system
    assert!(ctx.systems.get_handle(wh_step_sys).is_none());
    crate::services::wormhole_pve_escalations::step_wormhole_pve_system(&ctx, wh_step_sys, 1.0).await;
    assert!(ctx.systems.get_handle(wh_step_sys).is_none());

    // 3. process_wormhole_sites_on_downtime
    assert!(ctx.systems.get_handle(wh_dt_sys).is_none());
    with_pve_exploration_reg(&ctx, |reg| {
        reg.downtime_counter = 3;
        reg.sites_by_system.insert(
            wh_dt_sys,
            vec![PveSiteInstance {
                site_id: "WH-COLD-DT".into(),
                solar_system_id: wh_dt_sys,
                wormhole_class: Some(3),
                cleared: true,
                activated_at_downtime: Some(1),
                active_npc_ball_ids: vec![777_001],
                ..Default::default()
            }],
        );
    });
    let despawned = crate::services::wormhole_pve_escalations::process_wormhole_sites_on_downtime(&ctx);
    assert_eq!(despawned, 1);
    assert!(ctx.systems.get_handle(wh_dt_sys).is_none());

    // 4. handle_second_hack_failure_explosion
    let cont_id: i64 = 956_201;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(
            snap,
            Some(cont_id),
            23,
            90_000_001,
            i64::from(hack_sys),
            InventoryFlag::None,
            1,
            true,
        );
    });
    assert!(ctx.systems.get_handle(hack_sys).is_none());
    let (exploded, _, _, _) =
        crate::services::pve_hacking_board::handle_second_hack_failure_explosion(
            &ctx,
            hack_sys,
            "DATA_COLD_1",
            cont_id,
            false,
        )
        .await;
    assert!(exploded);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&cont_id)));
    assert!(ctx.systems.get_handle(hack_sys).is_none());

    // 5. tourney_return_ops (ReturnAllPlayers + TeleportRefHome)
    let p_red: i64 = 956_301;
    let p_ship: i64 = 956_302;
    let gm_cid: i64 = 956_303;
    let gm_ship: i64 = 956_304;
    let match_id: i64 = 8_801;
    let other_st_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&id| id != home_st_id)
        .unwrap_or(home_st_id);
    ctx.db
        .provision_gm_account("cold_tr_p1", "pw", p_red, "ColdTourneyPilot");
    ctx.db
        .provision_gm_account("cold_tr_gm", "pw", gm_cid, "ColdTourneyRef");
    let gm_uid = ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, st_opt) in [(p_red, p_ship, None), (gm_cid, gm_ship, Some(other_st_id))] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = arena_sys;
                ch.station_id = st_opt;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.home_station_id = home_st_id;
            }
            put_item(
                snap,
                Some(sid),
                587,
                cid,
                i64::from(arena_sys),
                InventoryFlag::None,
                1,
                true,
            );
        }
        let mut store = load_store(snap);
        store.matches.insert(
            match_id,
            TourneyMatch {
                match_id,
                tourney_id: 1,
                solar_system_id: i64::from(arena_sys),
                state: TourneyMatchState::Completed,
                red_players: vec![TourneyPilot {
                    character_id: p_red,
                    ship_type_id: 587,
                    ship_id: p_ship,
                    points: 2,
                }],
                referees: vec![gm_cid],
                ..Default::default()
            },
        );
        save_store(snap, &store, ctx.now_unix());
        snap.characters[&gm_cid].user_id
    });

    let mut gm_sess = make_session(
        gm_cid,
        98_000_001,
        Some(arena_sys),
        Some(gm_ship),
        service_roles::ROLE_GMH,
    );
    gm_sess.user_id = Some(gm_uid);
    // Keep referee docked (`departing.in_space == false`) so TeleportRefHome directly
    // exercises `tourney_return_ops::handle_teleport_ref_home` (`sp6_lifecycle/drone_return`
    // already gates teardown via `get_handle`).
    gm_sess.station_id = Some(other_st_id);
    assert!(ctx.systems.get_handle(arena_sys).is_none());
    assert!(ctx.systems.get_handle(home_sys).is_none());

    crate::services::tourney_service::handle(
        &ctx,
        &mut gm_sess,
        "tourneyMgr",
        "ReturnAllPlayers",
        &[PyRep::Integer(match_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.systems.get_handle(arena_sys).is_none());
    assert!(ctx.systems.get_handle(home_sys).is_none());

    crate::services::tourney_service::handle(
        &ctx,
        &mut gm_sess,
        "tourneyMgr",
        "TeleportRefHome",
        &[PyRep::Integer(match_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    assert!(ctx.systems.get_handle(arena_sys).is_none());
    assert!(ctx.systems.get_handle(home_sys).is_none());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&p_ship].location_id, home_st_id);
        assert_eq!(snap.items[&gm_ship].location_id, home_st_id);
    });

    // 6. pve_fob_observatory (trigger_fob_defense_response, destroy_pirate_fob_and_payout, on_world_ark_boss_killed)
    let fob_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id >= 30_000_000
                && s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.solar_system_id != 30_100_000
                && s.solar_system_id != home_sys
                && s.solar_system_id != def_sys
                && s.solar_system_id != hack_sys
                && s.solar_system_id != arena_sys
        })
        .map(|s| s.solar_system_id)
        .expect("FOB cold test system");
    let ark_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
                && sid != hack_sys
                && sid != arena_sys
                && sid != fob_sys
        })
        .expect("World Ark cold test system");
    let inc_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
                && sid != hack_sys
                && sid != arena_sys
                && sid != fob_sys
                && sid != ark_sys
        })
        .expect("Incursion cold test system");
    let enc_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
                && sid != hack_sys
                && sid != arena_sys
                && sid != fob_sys
                && sid != ark_sys
                && sid != inc_sys
        })
        .expect("Encounter cold test system");
    let arena_cold_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            (30_000_000..31_000_000).contains(&sid)
                && sid != 30_100_000
                && sid != home_sys
                && sid != def_sys
                && sid != hack_sys
                && sid != arena_sys
                && sid != fob_sys
                && sid != ark_sys
                && sid != inc_sys
                && sid != enc_sys
        })
        .expect("Tourney TeleportToStation cold test system");

    let fob_id: i64 = 1_897_956_101;
    let ark_site_id: i64 = 1_899_000_000 + i64::from(ark_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                type_id: 47_512,
                owner_corp_id: 500_010,
                solar_system_id: fob_sys,
                name: "ColdPirateFOB".into(),
                position: [200_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.fob_observatory_state.observatory_sites.insert(
            ark_site_id,
            eve_db::ObservatorySiteState {
                site_id: ark_site_id,
                solar_system_id: ark_sys,
                gate_id: ark_site_id + 2,
                world_ark_id: ark_site_id + 1,
                site_pos: [400_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });
    assert!(ctx.systems.get_handle(fob_sys).is_none());
    let def_rep = crate::services::pve_fob_observatory::trigger_fob_defense_response(
        &ctx,
        fob_id,
        &[p_ship],
    )
    .await
    .expect("cold FOB defense response");
    assert!(ctx.systems.get_handle(fob_sys).is_none());
    if let PyRep::KeyVal(entries) = def_rep {
        let count = entries
            .iter()
            .find(|(k, _)| k.as_str() == "defenderCount")
            .map(|(_, v)| v.clone());
        assert_eq!(count, Some(PyRep::Integer(0)));
    } else {
        panic!("expected PyRep::KeyVal from trigger_fob_defense_response");
    }

    let fob_destroy_rep =
        crate::services::pve_fob_observatory::destroy_pirate_fob_and_payout(&ctx, fob_id, &[])
            .await
            .expect("cold FOB destroy payout");
    assert!(ctx.systems.get_handle(fob_sys).is_none());
    if let PyRep::KeyVal(entries) = fob_destroy_rep {
        let destroyed_fob = entries
            .iter()
            .find(|(k, _)| k.as_str() == "fobID")
            .map(|(_, v)| v.clone());
        assert_eq!(destroyed_fob, Some(PyRep::Integer(fob_id)));
    } else {
        panic!("expected PyRep::KeyVal from destroy_pirate_fob_and_payout");
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&fob_id].state, "Destroyed");
        assert!(snap.items.contains_key(&(fob_id + 90)));
    });

    assert!(ctx.systems.get_handle(ark_sys).is_none());
    crate::services::pve_fob_observatory::on_world_ark_boss_killed(&ctx, ark_sys, p_red).await;
    assert!(ctx.systems.get_handle(ark_sys).is_none());
    assert!(ctx
        .db
        .with_snapshot(|snap| snap.fob_observatory_state.observatory_sites.contains_key(&ark_site_id)));

    // 7. wormhole_pve_escalations::cleanup_unclaimed_wh_balls
    let wh_orphan_bid: i64 = 956_250;
    let wh_ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        wh_orphan_bid,
        30_191,
        wh_step_sys,
        "Sleeper",
    );
    crate::combat::npc_ai::npc_test_helpers::seed_npc_entity(&ctx, wh_ent);
    assert!(ctx.systems.get_handle(wh_step_sys).is_none());
    crate::services::wormhole_pve_escalations::cleanup_unclaimed_wh_balls(
        &ctx,
        wh_step_sys,
        &[wh_orphan_bid],
    )
    .await;
    assert!(ctx.systems.get_handle(wh_step_sys).is_none());
    assert!(crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        !rt.npc_entities.contains_key(&wh_orphan_bid)
    }));

    // 8. pve_incursion_sites::process_incursion_site_completion_and_payout_core fails closed without DB mutation
    let inc_boss_bid: i64 = 956_400;
    crate::services::pve_special_sites::with_pve_special_reg_db(&ctx.db, ctx.now_unix(), |reg| {
        reg.incursions.insert(
            20_956_400,
            crate::services::pve_incursion_sites::IncursionTaleRecord {
                tale_id: 20_956_400,
                constellation_id: 20_956_400,
                constellation_name: "ColdIncConst".into(),
                staging_solar_system_id: inc_sys,
                vanguard_solar_system_ids: vec![inc_sys],
                assault_solar_system_ids: vec![inc_sys],
                hq_solar_system_id: inc_sys,
                influence: 0.5,
                state: "Established".into(),
                lockout_until_filetime: None,
                mothership_spawned: false,
                mothership_defeated: false,
                tale_stop_published: false,
                sites: vec![crate::services::pve_incursion_sites::IncursionSiteEntry {
                    site_id: "COLD-INC-1".into(),
                    dungeon_id: 172,
                    dungeon_name: "Sansha Vanguard".into(),
                    tier: "Vanguard".into(),
                    solar_system_id: inc_sys,
                    position: [100_000.0, 0.0, 0.0],
                    min_pilots: 1,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.1,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: inc_boss_bid,
                    boss_type_id: 19_720,
                    boss_destroyed: true,
                    killer_char_id: None,
                    killer_fleet_id: None,
                    escort_spawns: vec![],
                }],
            },
        );
    });
    assert!(ctx.systems.get_handle(inc_sys).is_none());
    let inc_err = crate::services::pve_incursion_sites::process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        None,
        None,
        inc_sys,
        "COLD-INC-1",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(inc_err.code, "IncursionSystemNotLoaded");
    assert!(ctx.systems.get_handle(inc_sys).is_none());
    crate::services::pve_special_sites::read_pve_special_reg(&ctx, |reg| {
        let site = &reg.incursions[&20_956_400].sites[0];
        assert_eq!(site.completed_count, 0);
        assert_eq!(site.boss_npc_ball_id, inc_boss_bid);
        assert!(site.boss_destroyed);
    });

    // 9. pve_hacking_board::detonate_ghost_site_with_can
    let ghost_cont_id: i64 = 956_202;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(
            snap,
            Some(ghost_cont_id),
            23,
            p_red,
            i64::from(hack_sys),
            InventoryFlag::None,
            1,
            true,
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            hack_sys,
            vec![PveSiteInstance {
                site_id: "GHOST_COLD_1".into(),
                dungeon_id: 5534,
                solar_system_id: hack_sys,
                position: [52_000_000.0, 42_000_000.0, -18_000_000.0],
                hackable_can_ids: vec![ghost_cont_id],
                ..Default::default()
            }],
        );
    });
    assert!(ctx.systems.get_handle(hack_sys).is_none());
    let (ghost_dmg, spawned_ambush) =
        crate::services::pve_hacking_board::detonate_ghost_site_with_can(
            &ctx,
            &mut gm_sess,
            hack_sys,
            "GHOST_COLD_1",
            Some(ghost_cont_id),
        )
        .await;
    assert!(ghost_dmg > 0.0);
    assert!(spawned_ambush.is_empty());
    assert!(ctx.systems.get_handle(hack_sys).is_none());
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ghost_cont_id)));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = &reg.sites_by_system[&hack_sys][0];
        assert!(s.ghost_detonated);
    });

    // 10. combat_npc_encounters::handle_npc_destroyed_in_space_with_ctx -> finalize_cycle_destroyed_npcs & step_encounter_wave_on_kill
    let enc_id: u32 = 956_500;
    let enc_trigger_bid: i64 = 956_501;
    let drifter_bid: i64 = 956_502;
    let (enc_items_before, enc_next_id_before) = ctx.db.with_snapshot(|snap| {
        (
            snap.items
                .values()
                .filter(|it| it.location_id == enc_sys as i64)
                .count(),
            snap.next_entity_id,
        )
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.encounter_waves.insert(
            enc_id,
            crate::services::combat_crimewatch::EncounterWaveDirector {
                encounter_id: enc_id,
                solar_system_id: enc_sys,
                current_wave: 1,
                max_waves: 3,
                trigger_ball_id: enc_trigger_bid,
                npc_type_id: 10_001,
                active_wave_ball_ids: vec![enc_trigger_bid],
                completed: false,
                wave_npc_types: vec![vec![10_001], vec![10_002], vec![10_003]],
                origin_position: glam::DVec3::ZERO,
                next_wave_pending: false,
            },
        );
        let mut trig_npc = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
            enc_trigger_bid,
            10_001,
            enc_sys,
            "Pirate",
        );
        trig_npc.target_id = Some(p_red);
        rt.npc_entities.insert(enc_trigger_bid, trig_npc);

        let mut drifter_npc = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
            drifter_bid,
            10_001,
            enc_sys,
            "Drifter",
        );
        drifter_npc.target_id = Some(p_red);
        drifter_npc.sleeper_wh_class = 5;
        drifter_npc.custom_cargo_loot = Some((30_001, 2));
        rt.npc_entities.insert(drifter_bid, drifter_npc);
    });
    assert!(ctx.systems.get_handle(enc_sys).is_none());
    crate::services::combat_crimewatch::combat_npc_encounters::handle_npc_destroyed_in_space_with_ctx(
        &ctx,
        enc_sys,
        enc_trigger_bid,
        Some(p_red),
    )
    .await;
    crate::services::combat_crimewatch::combat_npc_encounters::handle_npc_destroyed_in_space_with_ctx(
        &ctx,
        enc_sys,
        drifter_bid,
        Some(p_red),
    )
    .await;
    assert!(ctx.systems.get_handle(enc_sys).is_none());
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&enc_trigger_bid));
        assert!(!rt.npc_entities.contains_key(&drifter_bid));
        assert!(rt.destroyed_npc_cache.contains_key(&enc_trigger_bid));
        assert!(rt.destroyed_npc_cache.contains_key(&drifter_bid));
    });

    crate::services::combat_crimewatch::combat_npc_encounters::finalize_cycle_destroyed_npcs(
        &ctx, enc_sys,
    )
    .await;
    crate::services::combat_crimewatch::combat_npc_encounters::step_encounter_wave_on_kill(
        &ctx,
        enc_sys,
        enc_trigger_bid,
        DVec3::ZERO,
    )
    .await;
    assert!(ctx.systems.get_handle(enc_sys).is_none());
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let enc = &rt.encounter_waves[&enc_id];
        assert_eq!(enc.current_wave, 1, "cold system must not advance current_wave");
        assert!(!enc.completed);
        assert_eq!(enc.active_wave_ball_ids, vec![enc_trigger_bid]);
        assert!(!rt.destroyed_npc_cache.contains_key(&enc_trigger_bid));
        assert!(!rt.destroyed_npc_cache.contains_key(&drifter_bid));
    });
    ctx.db.with_snapshot(|snap| {
        let enc_items: Vec<_> = snap
            .items
            .values()
            .filter(|it| it.location_id == enc_sys as i64)
            .collect();
        assert_eq!(
            enc_items.len(),
            enc_items_before,
            "cold system must not insert orphaned wreck or sleeper loot items"
        );
        assert!(
            !enc_items.iter().any(|it| it.item_id > enc_next_id_before),
            "cold system must not mint new DB items on NPC death finalization"
        );
    });

    // 11. tourney_arena::TeleportToStation on cold old_sys
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&gm_cid) {
            ch.solar_system_id = arena_cold_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = gm_ship;
        }
        if let Some(ship) = snap.items.get_mut(&gm_ship) {
            ship.location_id = i64::from(arena_cold_sys);
            ship.flag = InventoryFlag::None;
        }
    });
    // Keep referee docked (`departing.in_space == false`) while DB char + ship point to
    // cold `arena_cold_sys`, so `TeleportToStation` exercises `tourney_arena.rs:347`
    // (`sp6_lifecycle/drone_return` already gates teardown via `get_handle`).
    gm_sess.solar_system_id = Some(arena_cold_sys);
    gm_sess.station_id = Some(other_st_id);
    gm_sess.ship_id = Some(gm_ship);
    assert!(ctx.systems.get_handle(arena_cold_sys).is_none());
    crate::services::tourney_service::handle(
        &ctx,
        &mut gm_sess,
        "tourneyMgr",
        "TeleportToStation",
        &[PyRep::Integer(match_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.systems.get_handle(arena_cold_sys).is_none());
    assert!(ctx.systems.get_handle(home_sys).is_none());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&gm_ship].location_id, home_st_id);
    });
}
