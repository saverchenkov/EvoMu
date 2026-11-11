//! Unit tests for `space_destiny` (`essMgr`, session timers, warp speed, `beyonce` state/bubble/components).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::with_mut_combat_runtime;
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
    assert!(is_location_change_invulnerable(&ctx, char_id, t0 + 59));
    assert!(!is_location_change_invulnerable(&ctx, char_id, t0 + 60));

    break_location_change_invulnerability(&ctx, char_id);
    assert!(!is_location_change_invulnerable(&ctx, char_id, t0 + 5));

    grant_location_change_invulnerability(&ctx, char_id, t0 + 120);
    assert!(is_location_change_invulnerable(&ctx, char_id, t0 + 119));

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
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.ess_main_bank_thefts.insert(null_sys, vec![(9301, 25_000_000.0, now_u)]);
        rt.ess_reserve_bank_thefts.insert(null_sys, vec![(9301, 10_000_000.0, now_u)]);
    });

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

    // Spawn warping ball: when registered in ctx.active_warps, UpdateStateRequest returns Ok(None)
    // and leaves MovementMode::Warp intact; when absent (emergency logoff return-warp), forces Stop.
    let mut warp_ball = test_ship_ball(88603, 588, DVec3::new(5_000.0, 0.0, 0.0));
    warp_ball.mode = MovementMode::new_warp(
        warp_ball.position,
        DVec3::new(1_000_000.0, 0.0, 0.0),
        3.0,
    );
    let _ = sys_h.spawn_ball(warp_ball).await;
    ctx.active_warps.lock().unwrap().insert(88603, 1);

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
    ctx.active_warps.lock().unwrap().clear();
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

    // 4. record_session_change_timer_with_invuln notification suppression & structure dock formatting
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
    assert!(is_location_change_invulnerable(&ctx, 9415, 1_700_020_010) == false);

    record_session_change_timer_with_invuln(
        &ctx,
        9415,
        1_700_020_005,
        sys_id,
        5,
        "jump_drive",
        true,
    );
    assert!(is_location_change_invulnerable(&ctx, 9415, 1_700_020_050));
    let _ = ctx.drain_notifications_for_char(9415);

    record_session_change_timer(&ctx, 9415, 1_700_020_010, sys_id, 6, "board_or_eject");
    assert!(ctx.drain_notifications_for_char(9415).is_empty());

    // Empty entries suppression (set_account_key in system with no character row -> has_entries = false)
    record_client_session_check_timer(&ctx, 9499, 1_700_020_020, 0, 7, "set_account_key");
    assert!(ctx.drain_notifications_for_char(9499).is_empty());
}
