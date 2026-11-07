//! Extra unit and mutation-killing tests for `services::space_destiny`
//! covering session-change timer notifications, invulnerability cancellation on movement,
//! `essMgr` error & edge branches, `UpdateStateRequest` ball spawn/warp recovery,
//! `AdminGetBubbleID` visibility checks, and `CallComponentFromClient` (`itemTrader.ProcessTrade`).

#![forbid(unsafe_code)]

use super::space_destiny::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(720);
    s.user_id = Some(1);
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
        session_id: 720,
        user_id: 1,
        character_id: 90_000_001,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

fn put_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
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
}

#[test]
fn session_change_timer_reasons_wormhole_id_and_warp_speed_fallback() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let sys_id = 30_000_142;
    let now = ctx.now_unix();

    // Unknown ship type falls back to 3.0 AU/s
    assert!((compute_ship_warp_speed_au_s(&ctx, 999_999_999) - 3.0).abs() < 1e-6);

    // Deterministic wormhole item ID formula
    let wid1 = wormhole_item_id_for_signature(31_000_001, "K162");
    let wid2 = wormhole_item_id_for_signature(31_000_001, "H296");
    let wid3 = wormhole_item_id_for_signature(31_000_002, "K162");
    assert!((760_000_000..770_000_000).contains(&wid1));
    assert_ne!(wid1, wid2);
    assert_ne!(wid1, wid3);

    // board_or_eject does not push OnSessionChanged
    ctx.pending_notifications.write().unwrap().clear();
    record_session_change_timer(&ctx, char_id, now, sys_id, 5, "board_or_eject");
    assert!(ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char_id)
        .map_or(true, Vec::is_empty));

    // dock / undock / structure_id reasons record session change timer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_000_111);
        }
    });
    record_session_change_timer(&ctx, char_id, now, sys_id, 6, "dock");
    assert_eq!(get_session_change_timer(&ctx, char_id).unwrap().reason, "dock");

    // break_location_change_invulnerability while active & character has active_ship_id > 0
    grant_location_change_invulnerability(&ctx, char_id, now + 60);
    assert!(is_location_change_invulnerable(&ctx, char_id, now + 10));
    break_location_change_invulnerability(&ctx, char_id);
    assert!(!is_location_change_invulnerable(&ctx, char_id, now + 10));
}

#[tokio::test]
async fn cmd_stop_preserves_invuln_while_other_movement_breaks_it() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);

    grant_location_change_invulnerability(&ctx, char_id, now + 60);
    let _ = handle(&ctx, &mut sess, None, "beyonce", "CmdStop", &[], &kw).await;
    assert!(
        is_location_change_invulnerable(&ctx, char_id, now + 5),
        "CmdStop must not break location-change invulnerability"
    );

    let _ = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
        &kw,
    )
    .await;
    assert!(
        !is_location_change_invulnerable(&ctx, char_id, now + 5),
        "Non-stop movement command must break location-change invulnerability"
    );
}

#[tokio::test]
async fn ess_mgr_edge_cases_expired_pulses_and_unlock_validation_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let kw = HashMap::new();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let high_sys = 30_000_142;

    // 1. Session without solar_system_id returns None / empty list / false / true
    let mut no_sys_sess = make_session(char_id, 98_000_001, None, Some(ship_id), 0);
    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "GetDataForClientSolarSystem", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "GetMainBankTheftsForClientSolarSystem", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "GetReserveBankTheftsForClientSolarSystem", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "IsClientLinkedToReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "RequestMainBankUnlink", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // 2. Expired unlocked_until_unix zeroes reserveBankPulsesRemaining
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = IskCents::from_isk(10_000_000);
        st.ess_linked_character_id = Some(char_id);
        set_ess_reserve_pulses(
            snap,
            null_sys,
            &EssReservePulseState {
                pulses_total: 15,
                pulses_remaining: 9,
                unlocked_until_unix: now_u.saturating_sub(10),
                reserve_linked_character_id: None,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.insert(null_sys, 0);
    });
    let mut null_sess = make_session(char_id, 98_000_001, Some(null_sys), Some(ship_id), 0);
    let data = handle(&ctx, &mut null_sess, None, "essMgr", "GetDataForClientSolarSystem", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Dict(entries) = &data {
        let rem = entries
            .iter()
            .find(|(k, _)| k.as_str() == Some("reserveBankPulsesRemaining"))
            .and_then(|(_, v)| v.as_i64());
        assert_eq!(rem, Some(0));
    } else {
        panic!("expected dict");
    }

    // RequestMainBankUnlink unlinks main bank when not reserve-linked
    assert_eq!(
        handle(&ctx, &mut null_sess, None, "essMgr", "RequestMainBankUnlink", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut null_sess, None, "essMgr", "RequestMainBankUnlink", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // 3. RequestUnlockReserveBank error guards
    let mut docked_sess = make_session(char_id, 98_000_001, Some(null_sys), Some(ship_id), 0);
    docked_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle(&ctx, &mut docked_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    let mut no_ship_sess = make_session(char_id, 98_000_001, Some(null_sys), None, 0);
    assert_eq!(
        handle(&ctx, &mut no_ship_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    assert_eq!(
        handle(&ctx, &mut no_sys_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    let mut hs_sess = make_session(char_id, 98_000_001, Some(high_sys), Some(ship_id), 0);
    assert_eq!(
        handle(&ctx, &mut hs_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "EssOnlyInNullSec"
    );

    // Ship not spawned in null_sys -> OutsideEssLinkRange
    assert_eq!(
        handle(&ctx, &mut null_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideEssLinkRange"
    );

    // Spawn ship 10km from ESS beacon -> OutsideEssLinkRange (> 2500m)
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let ess_pos = sys_h
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|b| b.id == 820_000_000 + null_sys as i64)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: ess_pos + DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle(&ctx, &mut null_sess, None, "essMgr", "RequestUnlockReserveBank", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideEssLinkRange"
    );
}

#[tokio::test]
async fn update_state_request_and_admin_get_bubble_id_edge_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let sys_id = 30_000_142;
    let ship_id = 100_000_001;
    let other_ball_id = 100_000_999;
    let kw = HashMap::new();

    // 1. Bound system mismatch -> BoundObjectOwnershipViolation
    let mut sess = make_session(char_id, 98_000_001, Some(30_000_144), Some(ship_id), 0);
    let bound = bound_sys(sys_id);
    assert_eq!(
        handle_update_state_request(&ctx, &sess, Some(&bound))
            .await
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );

    // 2. Unbound without solar_system_id -> NoSolarSystem
    sess.solar_system_id = None;
    assert_eq!(
        handle_update_state_request(&ctx, &sess, None)
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // 3. Docked returns Ok(PyRep::None)
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_update_state_request(&ctx, &sess, None).await.unwrap(),
        PyRep::None
    );
    sess.station_id = None;

    // 4. Ship not yet in ballpark, but in snap.items -> spawns ball and finishes active warp if stopped
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        snap.item_positions.insert(ship_id, [15_000.0, 0.0, 0.0]);
    });
    ctx.active_warps.lock().unwrap().insert(ship_id, 100);
    assert_eq!(
        handle_update_state_request(&ctx, &sess, Some(&bound))
            .await
            .unwrap(),
        PyRep::None
    );
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // 5. Emergency warp without active_warps entry -> resets ship mode to Stop
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert_eq!(
        handle_update_state_request(&ctx, &sess, Some(&bound))
            .await
            .unwrap(),
        PyRep::None
    );
    let (_, b_after) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(b_after.mode, MovementMode::Stop));

    // 6. AdminGetBubbleID: NoActiveShip, BallNotVisible (distant ball), and visible ball success
    let mut gm_no_ship = make_session(char_id, 98_000_001, Some(sys_id), None, service_roles::ROLE_GMH);
    assert_eq!(
        handle(&ctx, &mut gm_no_ship, Some(&bound), "beyonce", "AdminGetBubbleID", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut gm_sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), service_roles::ROLE_GMH);
    assert_eq!(
        handle(&ctx, &mut gm_sess, Some(&bound), "beyonce", "AdminGetBubbleID", &[PyRep::Integer(other_ball_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BallNotVisible"
    );

    // Spawn other_ball_id at same position as ship_id -> visible in same bubble
    let _ = sys_h
        .spawn_ball(Ball {
            id: other_ball_id, type_id: 587, position: DVec3::new(15_200.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0,
            radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        })
        .await;
    let bid_res = handle(&ctx, &mut gm_sess, Some(&bound), "beyonce", "AdminGetBubbleID", &[PyRep::Integer(other_ball_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(bid_res.as_i64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn call_component_from_client_item_trader_validation_and_trade_execution() {
    let ctx = test_ctx();
    let (char_id, sys_id, ship_id, trader_id) = (90_000_001, 30_000_142, 100_000_001, 880_999_001);
    let kw = HashMap::new();
    let bound = bound_sys(sys_id);

    // Docked, NoActiveShip, InvalidArgument, InvalidMultiplier
    let mut docked = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle(&ctx, &mut docked, Some(&bound), "beyonce", "CallComponentFromClient", &[], &kw).await.unwrap().unwrap_err().code,
        "NotInSpace"
    );

    let mut no_ship = make_session(char_id, 98_000_001, Some(sys_id), None, 0);
    assert_eq!(
        handle(&ctx, &mut no_ship, Some(&bound), "beyonce", "CallComponentFromClient", &[], &kw).await.unwrap().unwrap_err().code,
        "NoActiveShip"
    );

    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), 0);
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &[], &kw).await.unwrap().unwrap_err().code,
        "InvalidArgument"
    );

    let trade_args = |rid: i64, mult: i64| {
        vec![
            PyRep::Integer(trader_id),
            PyRep::String("itemTrader".into()),
            PyRep::String("ProcessTrade".into()),
            PyRep::Integer(rid),
            PyRep::Integer(mult),
        ]
    };
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &trade_args(1, 0), &kw).await.unwrap().unwrap_err().code,
        "InvalidMultiplier"
    );

    // Non-itemTrader type in space -> NotAnItemTrader
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, trader_id, 587, 1, sys_id as i64, InventoryFlag::None, 1, true);
        snap.item_positions.insert(trader_id, [0.0, 0.0, 0.0]);
    });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &trade_args(1, 1), &kw).await.unwrap().unwrap_err().code,
        "NotAnItemTrader"
    );

    // Find a real non-insurgency itemTrader type and recipe in SDE deterministically
    let recipes_table = ctx.sde.load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary").expect("itemtraderrecipes");
    let mut candidate_tids: Vec<u32> = ctx
        .sde
        .types
        .keys()
        .copied()
        .filter(|&tid| !matches!(tid, 78_868..=78_871 | 79_115 | 79_116))
        .collect();
    candidate_tids.sort_unstable();
    let (trader_tid, recipe_id, range_m) = candidate_tids
        .into_iter()
        .find_map(|tid| {
            let (recipes, range_m) = ctx.sde.item_trader_component_for_type(tid).ok()??;
            let rid = *recipes.iter().find(|r| recipes_table.contains_key(&r.to_string()))?;
            Some((tid, rid, range_m))
        })
        .expect("SDE itemTrader type with recipe");

    let trader_radius = ctx.sde.types.get(&trader_tid).map(|t| t.radius).unwrap_or(500.0);
    let out_of_range_x = trader_radius + 50.0 + range_m + 10_000.0;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id, type_id: 20_185, position: DVec3::new(out_of_range_x, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1_000_000.0,
            radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, ship_id, 20_185, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        put_item(snap, trader_id, trader_tid, 1, sys_id as i64, InventoryFlag::None, 1, true);
        snap.item_positions.insert(trader_id, [0.0, 0.0, 0.0]);
    });

    // Out of range -> ItemTraderOutOfRange
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &trade_args(recipe_id as i64, 1), &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemTraderOutOfRange"
    );

    // Move ship to origin (within range), test RecipeNotAvailableAtTrader
    let _ = sys_h.set_ball_position(ship_id, DVec3::ZERO).await;
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &trade_args(999_999_999, 1), &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "RecipeNotAvailableAtTrader"
    );

    // Seed required ISK and input items into ship cargo, then execute trade
    let r_val = recipes_table.get(&recipe_id.to_string()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000_000_000);
        }
        if let Some(in_obj) = r_val.get("inputItems").and_then(|v| v.as_object()) {
            for (idx, (k, v)) in in_obj.iter().enumerate() {
                if let (Ok(tid), Some(qty)) = (k.parse::<u32>(), v.as_i64()) {
                    put_item(snap, 881_000 + idx as i64, tid, char_id, ship_id, InventoryFlag::Cargo, (qty as i32) * 2, false);
                }
            }
        }
    });

    let trade_ok = handle(&ctx, &mut sess, Some(&bound), "beyonce", "CallComponentFromClient", &trade_args(recipe_id as i64, 1), &kw)
        .await
        .unwrap()
        .expect("ProcessTrade succeeds");
    assert!(matches!(trade_ok, PyRep::Tuple(_)));
}
