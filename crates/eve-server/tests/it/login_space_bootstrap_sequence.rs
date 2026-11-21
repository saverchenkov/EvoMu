use std::collections::HashMap;

use eve_destiny::wire::{decode_add_balls, DecodedBall, DST_BALL_STOP};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::serverops_cluster::set_system_tidi_factor;
use eve_server::sp6_lifecycle::populate_persisted_space_objects;
use eve_testkit::client_harness::{
    extract_bound_id_from_wire_rep, ClientCallOutput, ClientHarness, ClientWireFrameKind,
};
use glam::DVec3;

use crate::common::{
    borrow_wire_harness, gm_pilot_in_space, new_wire_harness, pin_clock, teardown_guard,
    ServerWireTransport,
};

const TEST_SYS_A: u32 = 30_000_142; // Jita
const TEST_SYS_B: u32 = 30_000_144; // Perimeter
const RIFTER_TYPE: u32 = 587;
const TRISTAN_TYPE: u32 = 593;
const CAPSULE_TYPE: u32 = 670;
const AUTOCANNON_TYPE: u32 = 2889;
const AFTERBURNER_TYPE: u32 = 439;
const AMMO_EMP_S_TYPE: u32 = 182;

fn extract_set_state_keyvals(out: &ClientCallOutput) -> Vec<&PyRep> {
    let mut found = Vec::new();
    for frame in &out.frames {
        let ClientWireFrameKind::DoDestinyUpdate { updates, .. } = &frame.kind else {
            continue;
        };
        if let PyRep::List(items) = updates {
            for item in items {
                if let PyRep::Tuple(pair) = item {
                    if let Some(PyRep::Tuple(fa)) = pair.get(1) {
                        if fa.first().and_then(|v| v.as_str()) == Some("SetState") {
                            if let Some(PyRep::Tuple(args)) = fa.get(1) {
                                if let Some(kv) = args.first() {
                                    found.push(kv);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    found
}

fn collect_destiny_action_names(out: &ClientCallOutput) -> Vec<String> {
    let mut names = Vec::new();
    for frame in &out.frames {
        let ClientWireFrameKind::DoDestinyUpdate { updates, .. } = &frame.kind else {
            continue;
        };
        if let PyRep::List(items) = updates {
            for item in items {
                if let PyRep::Tuple(pair) = item {
                    if let Some(PyRep::Tuple(fa)) = pair.get(1) {
                        if let Some(name) = fa.first().and_then(|v| v.as_str()) {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    names
}

fn collect_remove_ball_ids(out: &ClientCallOutput) -> Vec<i64> {
    let mut ids = Vec::new();
    for frame in &out.frames {
        let ClientWireFrameKind::DoDestinyUpdate { updates, .. } = &frame.kind else {
            continue;
        };
        if let PyRep::List(items) = updates {
            for item in items {
                if let PyRep::Tuple(pair) = item {
                    if let Some(PyRep::Tuple(fa)) = pair.get(1) {
                        let action = fa.first().and_then(|v| v.as_str());
                        if let Some(PyRep::Tuple(args) | PyRep::List(args)) = fa.get(1) {
                            if action == Some("RemoveBalls") {
                                if let Some(PyRep::List(ball_ids) | PyRep::Tuple(ball_ids)) =
                                    args.first()
                                {
                                    ids.extend(ball_ids.iter().filter_map(|v| v.as_i64()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    ids
}

fn decode_single_set_state_balls(out: &ClientCallOutput) -> (i64, Vec<DecodedBall>) {
    let kvs = extract_set_state_keyvals(out);
    assert_eq!(kvs.len(), 1, "expected exactly 1 SetState action, got {}", kvs.len());
    let kv = kvs[0];
    let ego = kv.dict_get("ego").and_then(|v| v.as_i64()).expect("SetState ego");
    let PyRep::Bytes(state_bytes) = kv.dict_get("state").expect("SetState state bytes") else {
        panic!("SetState state must be Bytes: {kv:?}");
    };
    let (_pkt_type, _stamp, balls) =
        decode_add_balls(state_bytes).expect("decode SetState binary state");
    (ego, balls)
}

async fn setup_space_pilot_logged_out(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
    sys_id: u32,
) -> (String, String, i64) {
    let kw = HashMap::new();
    let mut pilot = gm_pilot_in_space(ctx, sess_id, char_id, ship_type_id, sys_id).await;
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    {
        let mut h = borrow_wire_harness(ctx, &mut pilot.sess);
        let safe = h.call_exchange("ship", None, "SafeLogoff", &[], &kw).await;
        assert_eq!(safe.result, Ok(PyRep::List(vec![])), "SafeLogoff failed: {:?}", safe.result);
    }
    ctx.advance_secs(31);
    (user, pass, pilot.ship_id)
}

async fn login_into_space_wire<'c>(
    ctx: &'c ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> (ClientHarness<ServerWireTransport<'c>>, ClientCallOutput) {
    let kw = HashMap::new();
    let mut h = new_wire_harness(ctx, sess_id);
    h.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.to_string()), PyRep::String(pass.to_string())],
        &kw,
    )
    .await;
    let sel_out = h
        .call_exchange(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await;
    assert!(sel_out.result.is_ok(), "SelectCharacterID failed: {:?}", sel_out.result);
    (h, sel_out)
}

fn find_notification_frames<'a>(
    out: &'a ClientCallOutput,
    event_name: &str,
) -> Vec<&'a [PyRep]> {
    let mut found = Vec::new();
    for frame in &out.frames {
        if let ClientWireFrameKind::Notification { event, args } = &frame.kind {
            if event == event_name {
                found.push(args.as_slice());
            }
        }
    }
    for (event, args) in &out.notifications {
        if event == event_name && !found.iter().any(|existing| *existing == args.as_slice()) {
            found.push(args.as_slice());
        }
    }
    found
}

#[tokio::test]
async fn beyonce_bind_emits_setstate_before_call_rsp() {
    // parity: rpc:beyonce.Bind, destiny:SetState, journey:launch_login
    eve_testkit::attest(&["journey:launch_login", "destiny:SetState"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let (user, pass, ship_id) =
        setup_space_pilot_logged_out(&ctx, 51_001, 91_001_001, RIFTER_TYPE, TEST_SYS_A).await;
    let (mut h, _sel) = login_into_space_wire(&ctx, 51_002, &user, &pass, 91_001_001).await;

    let bind_out = h
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None],
            &kw,
        )
        .await;
    let bind_rep = bind_out.result.as_ref().expect("beyonce.MachoBindObject in space");
    let beyonce_oid = extract_bound_id_from_wire_rep(bind_rep).expect("beyonce bound id");
    assert!(beyonce_oid > 0);

    let dest_idx = bind_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { has_set_state: true, .. }))
        .expect("DoDestinyUpdate(SetState) frame missing");
    let rsp_idx = bind_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp frame missing");
    assert!(
        dest_idx < rsp_idx,
        "SetState DoDestinyUpdate ({dest_idx}) must precede CallRsp ({rsp_idx})"
    );

    let (ego, balls) = decode_single_set_state_balls(&bind_out);
    assert_eq!(ego, ship_id);
    let ship_ball = balls
        .iter()
        .find(|b| b.id == ship_id)
        .expect("pilot ship ball in SetState");
    assert_eq!(ship_ball.mode, DST_BALL_STOP, "initial SetState pilot ship must be in STOP mode");

    // Negative: docked beyonce.MachoBindObject returns bound id without emitting SetState.
    ctx.db.provision_gm_account("dock51", "dock51-secret", 91_001_002, "Dock Pilot");
    let mut dh = new_wire_harness(&ctx, 51_003);
    dh.call_ok("authentication", None, "Login", &[PyRep::String("dock51".into()), PyRep::String("dock51-secret".into())], &kw).await;
    dh.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(91_001_002)], &kw).await;
    let docked_bind = dh
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None],
            &kw,
        )
        .await;
    assert!(extract_bound_id_from_wire_rep(docked_bind.result.as_ref().unwrap()).is_some());
    assert_eq!(extract_set_state_keyvals(&docked_bind).len(), 0);
}

#[tokio::test]
async fn space_login_getallinfo_counts_and_no_item_change_churn() {
    // parity: feat:dogma.location-prime, notify:OnServerBrainUpdated, notify:OnGodmaShipEffect, notify:OnItemChange
    eve_testkit::attest(&["feat:dogma.location-prime"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let (user, pass, ship_id) =
        setup_space_pilot_logged_out(&ctx, 52_001, 91_002_001, RIFTER_TYPE, TEST_SYS_A).await;
    let char_id = 91_002_001_i64;
    let (mod1_id, mod2_id, real_ammo_id) = (910_200_001_i64, 910_200_002_i64, 910_200_003_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, flag, qty, singleton) in [
            (mod1_id, AUTOCANNON_TYPE, eve_db::InventoryFlag::HiSlot0, 1, true),
            (mod2_id, AFTERBURNER_TYPE, eve_db::InventoryFlag::MedSlot0, 1, true),
            (real_ammo_id, AMMO_EMP_S_TYPE, eve_db::InventoryFlag::HiSlot0, 40, false),
        ] {
            let mut rec = eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: tid,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                ..Default::default()
            };
            if singleton {
                rec.dynamic_attributes.insert(2, 1.0);
                snap.module_online_states.insert(iid, true);
            }
            snap.items.insert(iid, rec);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let (mut h, _sel) = login_into_space_wire(&ctx, 52_002, &user, &pass, char_id).await;
    let all_out = h
        .call_exchange("dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw)
        .await;
    let all_info = all_out.result.as_ref().expect("dogmaIM.GetAllInfo");

    let PyRep::Dict(ship_info) = all_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("shipInfo must be Dict: {all_info:?}");
    };
    assert_eq!(ship_info.len(), 1 + 2 + 1, "shipInfo must have 1 ship + 2 fitted + 1 loaded charge");
    assert!(
        ship_info.iter().any(|(k, _)| matches!(k, PyRep::Tuple(t) if t.len() == 3 && t[0].as_i64() == Some(ship_id) && t[1].as_i64() == Some(27) && t[2].as_i64() == Some(i64::from(AMMO_EMP_S_TYPE)))),
        "loaded charge must be keyed by (shipID, flagID, typeID)"
    );
    assert!(
        !ship_info.iter().any(|(k, _)| k.as_i64() == Some(real_ammo_id)),
        "real charge itemID {real_ammo_id} must be absent from shipInfo"
    );

    let PyRep::Tuple(ship_state) = all_info.dict_get("shipState").expect("shipState") else {
        panic!("shipState must be Tuple: {all_info:?}");
    };
    let PyRep::Dict(instance_cache) = &ship_state[0] else {
        panic!("shipState[0] must be Dict");
    };
    assert_eq!(instance_cache.len(), 2 + 2, "shipState[0] must have ship + char + 2 fitted modules");
    let PyRep::Dict(subloc_cache) = &ship_state[1] else {
        panic!("shipState[1] must be Dict");
    };
    let PyRep::Dict(flag_map) = &subloc_cache
        .iter()
        .find(|(k, _)| k.as_i64() == Some(ship_id))
        .expect("shipState[1] entry for ship")
        .1
    else {
        panic!("shipState[1][shipID] must be Dict");
    };
    assert!(flag_map.iter().any(|(k, _)| k.as_i64() == Some(27)));

    let brain_notifs = find_notification_frames(&all_out, "OnServerBrainUpdated");
    assert_eq!(brain_notifs.len(), 1, "first GetAllInfo must emit single OnServerBrainUpdated");
    let PyRep::Tuple(brain_tuple) = &brain_notifs[0][0] else {
        panic!("OnServerBrainUpdated arg must be Tuple: {:?}", brain_notifs[0]);
    };
    let PyRep::Bytes(brain_blob) = &brain_tuple[1] else {
        panic!("OnServerBrainUpdated arg[1] must be marshaled Bytes");
    };
    let PyRep::Tuple(decoded_brain) = eve_proto::marshal::decode(brain_blob).expect("decode brain blob") else {
        panic!("OnServerBrainUpdated decoded blob must be 3-tuple");
    };
    assert_eq!(decoded_brain.len(), 3);
    assert_eq!(find_notification_frames(&all_out, "OnItemChange").len(), 0);
    assert_eq!(find_notification_frames(&all_out, "OnGodmaShipEffect").len(), 0);

    // Bind invbroker and List() active ship inventory -> zero OnItemChange / OnGodmaShipEffect churn.
    let inv_bind = h
        .call_exchange("invbroker", None, "MachoBindObject", &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None], &kw)
        .await;
    let inv_mgr_oid = extract_bound_id_from_wire_rep(inv_bind.result.as_ref().unwrap()).unwrap();
    let ship_inv_out = h
        .call_exchange("invbroker", Some(inv_mgr_oid), "GetInventoryFromId", &[PyRep::Integer(ship_id), PyRep::Integer(0)], &kw)
        .await;
    let ship_inv_oid = extract_bound_id_from_wire_rep(ship_inv_out.result.as_ref().unwrap())
        .or_else(|| ship_inv_out.result.as_ref().unwrap().as_i64().map(|v| v as u64))
        .expect("ship inventory bound id");
    let list_out = h.call_exchange("invbroker", Some(ship_inv_oid), "List", &[], &kw).await;
    assert!(list_out.result.is_ok());
    for step_out in [&inv_bind, &ship_inv_out, &list_out] {
        assert_eq!(find_notification_frames(step_out, "OnGodmaShipEffect").len(), 0);
        for args in find_notification_frames(step_out, "OnItemChange") {
            let dbg = format!("{args:?}");
            assert!(!dbg.contains(&mod1_id.to_string()) && !dbg.contains(&mod2_id.to_string()));
        }
    }

    // Negative / idempotence: second GetAllInfo emits zero OnItemChange and zero extra OnServerBrainUpdated.
    let second_all = h
        .call_exchange("dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw)
        .await;
    assert!(second_all.result.is_ok());
    assert_eq!(find_notification_frames(&second_all, "OnItemChange").len(), 0);
    assert_eq!(find_notification_frames(&second_all, "OnServerBrainUpdated").len(), 0);
}

#[tokio::test]
async fn getformations_bootstrap_is_not_replayed_by_bind() {
    // parity: rpc:beyonce.GetFormations, rpc:beyonce.Bind, destiny:SetState
    eve_testkit::attest(&["rpc:beyonce.GetFormations", "destiny:SetState"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let (user, pass, ship_id) =
        setup_space_pilot_logged_out(&ctx, 53_001, 91_003_001, RIFTER_TYPE, TEST_SYS_A).await;
    let (mut h, _sel) = login_into_space_wire(&ctx, 53_002, &user, &pass, 91_003_001).await;

    let form_out = h.call_exchange("beyonce", None, "GetFormations", &[], &kw).await;
    assert!(form_out.result.is_ok(), "beyonce.GetFormations failed: {:?}", form_out.result);
    let (ego, balls) = decode_single_set_state_balls(&form_out);
    assert_eq!(ego, ship_id);
    assert!(balls.iter().any(|b| b.id == ship_id && b.mode == DST_BALL_STOP));

    let dest_idx = form_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { has_set_state: true, .. }))
        .expect("SetState frame");
    let rsp_idx = form_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp frame");
    assert!(dest_idx < rsp_idx, "GetFormations SetState must precede CallRsp");

    // Subsequent bind must NOT replay SetState or emit duplicate DoDestinyUpdates.
    let bind_out = h
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None],
            &kw,
        )
        .await;
    assert!(extract_bound_id_from_wire_rep(bind_out.result.as_ref().unwrap()).is_some());
    let duplicate_destiny_frames = bind_out
        .frames
        .iter()
        .filter(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { .. }))
        .count();
    assert_eq!(duplicate_destiny_frames, 0, "subsequent beyonce bind must emit zero DoDestinyUpdates");

    // Negative: docked GetFormations emits zero DoDestinyUpdates.
    ctx.db.provision_gm_account("dock53", "dock53-secret", 91_003_002, "Dock53");
    let mut dh = new_wire_harness(&ctx, 53_003);
    dh.call_ok("authentication", None, "Login", &[PyRep::String("dock53".into()), PyRep::String("dock53-secret".into())], &kw).await;
    dh.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(91_003_002)], &kw).await;
    let docked_form = dh.call_exchange("beyonce", None, "GetFormations", &[], &kw).await;
    assert!(docked_form.result.is_ok());
    assert_eq!(extract_set_state_keyvals(&docked_form).len(), 0);
}

#[tokio::test]
async fn update_state_request_sends_setstate_without_addballs2() {
    // parity: rpc:beyonce.UpdateStateRequest, destiny:SetState, destiny:AddBalls2
    eve_testkit::attest(&["rpc:beyonce.UpdateStateRequest", "destiny:SetState"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let (user, pass, ship_id) =
        setup_space_pilot_logged_out(&ctx, 54_001, 91_004_001, RIFTER_TYPE, TEST_SYS_A).await;
    let (mut h, _sel) = login_into_space_wire(&ctx, 54_002, &user, &pass, 91_004_001).await;

    let bind_rep = h
        .call_ok("beyonce", None, "MachoBindObject", &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None], &kw)
        .await;
    let beyonce_oid = extract_bound_id_from_wire_rep(&bind_rep).expect("beyonce bound id");

    let upd_out = h
        .call_exchange("beyonce", Some(beyonce_oid), "UpdateStateRequest", &[], &kw)
        .await;
    assert_eq!(upd_out.result.as_ref().unwrap(), &PyRep::None, "UpdateStateRequest must return None");
    let (ego, balls) = decode_single_set_state_balls(&upd_out);
    assert_eq!(ego, ship_id);
    assert!(balls.iter().any(|b| b.id == ship_id && b.mode == DST_BALL_STOP));
    let actions = collect_destiny_action_names(&upd_out);
    assert!(actions.iter().any(|a| a == "SetState"));
    assert!(!actions.iter().any(|a| a == "AddBalls2"), "UpdateStateRequest must not emit AddBalls2: {actions:?}");

    // Negative: docked UpdateStateRequest is a benign no-op returning None without emitting SetState or AddBalls2.
    ctx.db.provision_gm_account("dock54", "dock54-secret", 91_004_002, "Dock54");
    let mut dh = new_wire_harness(&ctx, 54_003);
    dh.call_ok("authentication", None, "Login", &[PyRep::String("dock54".into()), PyRep::String("dock54-secret".into())], &kw).await;
    dh.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(91_004_002)], &kw).await;
    let docked_upd = dh.call_exchange("beyonce", None, "UpdateStateRequest", &[], &kw).await;
    assert_eq!(docked_upd.result, Ok(PyRep::None));
    assert_eq!(extract_set_state_keyvals(&docked_upd).len(), 0);
    assert!(!collect_destiny_action_names(&docked_upd).iter().any(|a| a == "AddBalls2"));
}

#[tokio::test]
async fn login_into_lagged_system_rebases_once() {
    // parity: notify:DoSimClockRebase, rpc:beyonce.Bind, journey:launch_login
    eve_testkit::attest(&["notify:DoSimClockRebase", "journey:launch_login"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let (user, pass, _ship_id) =
        setup_space_pilot_logged_out(&ctx, 55_001, 91_005_001, RIFTER_TYPE, TEST_SYS_A).await;
    set_system_tidi_factor(&ctx, TEST_SYS_A, 0.5);
    ctx.advance_secs(60);

    let (mut h, sel_out) = login_into_space_wire(&ctx, 55_002, &user, &pass, 91_005_001).await;
    let rebases = find_notification_frames(&sel_out, "DoSimClockRebase");
    assert_eq!(rebases.len(), 1, "logging into lagged system must emit DoSimClockRebase once");
    let PyRep::Tuple(times) = &rebases[0][0] else {
        panic!("DoSimClockRebase payload must be (old_ft, new_ft) tuple");
    };
    let old_ft = times[0].as_i64().expect("old_sim_time");
    let new_ft = times[1].as_i64().expect("new_sim_time");
    assert!(new_ft < old_ft, "lagged sim clock must rebase backward: new_ft={new_ft} < old_ft={old_ft}");

    // Subsequent beyonce bind must never emit DoSimClockRebase.
    let bind_out = h
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None],
            &kw,
        )
        .await;
    assert!(bind_out.result.is_ok());
    assert_eq!(find_notification_frames(&bind_out, "DoSimClockRebase").len(), 0);

    // Negative: logging into a current (undilated) system emits zero DoSimClockRebase.
    let (user_ok, pass_ok, _ship_ok) =
        setup_space_pilot_logged_out(&ctx, 55_003, 91_005_002, RIFTER_TYPE, TEST_SYS_B).await;
    let (_h2, sel_ok) = login_into_space_wire(&ctx, 55_004, &user_ok, &pass_ok, 91_005_002).await;
    assert_eq!(find_notification_frames(&sel_ok, "DoSimClockRebase").len(), 0);
}

#[tokio::test]
async fn offline_player_ships_hidden_until_login() {
    // parity: feat:serverops.persistence, destiny:SetState, journey:launch_login
    eve_testkit::attest(&["feat:serverops.persistence", "destiny:SetState"]);
    let ctx1 = ServerContext::new();
    pin_clock(&ctx1);
    let _guard1 = teardown_guard(&ctx1);

    let (user_a, pass_a, ship_a) =
        setup_space_pilot_logged_out(&ctx1, 56_001, 91_006_001, RIFTER_TYPE, TEST_SYS_A).await;

    // Cold-restart context sharing persisted DB; Pilot A is offline in space.
    let ctx2 = ServerContext::with_sde_and_repository(ctx1.sde.clone(), ctx1.db.clone());
    pin_clock(&ctx2);
    let _guard2 = teardown_guard(&ctx2);

    // Negative: persisted ballpark hydration & unrelated observer bind must NOT reveal offline ship_a.
    let mut fresh_bp = eve_destiny::Ballpark::new(TEST_SYS_A);
    let _ = populate_persisted_space_objects(&mut fresh_bp, TEST_SYS_A, &ctx2.sde, &ctx2.db);
    assert!(fresh_bp.balls.get(&ship_a).is_none(), "offline player ship must be omitted from persisted hydration");

    let mut obs = gm_pilot_in_space(&ctx2, 56_002, 91_006_002, TRISTAN_TYPE, TEST_SYS_A).await;
    let obs_upd_before = borrow_wire_harness(&ctx2, &mut obs.sess)
        .call_exchange("beyonce", Some(obs.beyonce), "UpdateStateRequest", &[], &HashMap::new())
        .await;
    let (_ego_b, balls_before) = decode_single_set_state_balls(&obs_upd_before);
    assert!(!balls_before.iter().any(|b| b.id == ship_a), "offline pilot ship must stay hidden from observer SetState");

    // Pilot A logs in -> ship_a restores into space ballpark and appears in SetState.
    let (mut ha, _sel_a) = login_into_space_wire(&ctx2, 56_003, &user_a, &pass_a, 91_006_001).await;
    let bind_a = ha
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(TEST_SYS_A)), PyRep::None],
            &HashMap::new(),
        )
        .await;
    let (ego_a, balls_a) = decode_single_set_state_balls(&bind_a);
    assert_eq!(ego_a, ship_a);
    assert!(balls_a.iter().any(|b| b.id == ship_a));
    assert!(ctx2.systems.get_or_spawn(TEST_SYS_A).get_ball_snapshot(ship_a).await.unwrap().is_some());
}

#[tokio::test]
async fn space_boarding_consumes_pod_and_keeps_ballpark() {
    // parity: feat:ships.board-in-space, destiny:SetState
    eve_testkit::attest(&["feat:ships.board-in-space"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    let mut p = gm_pilot_in_space(&ctx, 57_001, 91_007_001, RIFTER_TYPE, TEST_SYS_A).await;
    let sys_handle = ctx.systems.get_or_spawn(TEST_SYS_A);
    let ship_pos = sys_handle.get_ball_snapshot(p.ship_id).await.unwrap().expect("pilot ball").1.position;

    let (tristan_id, far_ship_id) = (910_700_001_i64, 910_700_003_i64);
    for (iid, tid, pos) in [
        (tristan_id, TRISTAN_TYPE, ship_pos + DVec3::new(500.0, 0.0, 0.0)),
        (far_ship_id, RIFTER_TYPE, ship_pos + DVec3::new(50_000.0, 0.0, 0.0)),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: p.char_id,
                    location_id: i64::from(TEST_SYS_A),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(iid, [pos.x, pos.y, pos.z]);
        });
        let cs = ctx.build_type_combat_state(tid);
        let group_id = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(25);
        let radius = ctx.sde.types.get(&tid).map(|t| t.radius).unwrap_or(50.0);
        let mass = ctx.sde.types.get(&tid).map(|t| t.mass).unwrap_or(1_000_000.0);
        let _ = sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: iid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 2.0,
                    mass,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                SlimItemMeta {
                    group_id,
                    category_id: 6,
                    owner_id: p.char_id,
                    char_id: None,
                    corp_id: 1_000_009,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            )
            .await;
    }

    // Grant Tristan prerequisites via /giveskill so Board validates real SDE skill checks.
    let tristan_skills: Vec<u32> = ctx
        .sde
        .types
        .get(&TRISTAN_TYPE)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for skill_id in tristan_skills {
        borrow_wire_harness(&ctx, &mut p.sess)
            .call_ok(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(format!("/giveskill {skill_id} 5"))],
                &kw,
            )
            .await;
    }

    // 1. Ship-to-ship boarding in space swaps ship without owner SetState ballpark reset.
    let board_tristan = borrow_wire_harness(&ctx, &mut p.sess)
        .call_exchange("ship", None, "Board", &[PyRep::Integer(tristan_id)], &kw)
        .await;
    assert!(board_tristan.result.is_ok(), "space Board Tristan failed: {:?}", board_tristan.result);
    assert_eq!(p.sess.ship_id, Some(tristan_id));
    assert_eq!(extract_set_state_keyvals(&board_tristan).len(), 0, "in-space ship swap must not reset ballpark with SetState");

    // 2. Eject into capsule (670), then board Tristan from capsule -> consumes pod item and emits RemoveBall(pod_id) without SetState.
    ctx.advance_secs(11);
    let eject_out = borrow_wire_harness(&ctx, &mut p.sess)
        .call_exchange("ship", None, "Eject", &[], &kw)
        .await;
    assert!(eject_out.result.is_ok(), "space Eject failed: {:?}", eject_out.result);
    let pod_id = p.sess.ship_id.expect("capsule ship_id after Eject");
    assert_eq!(ctx.db.get_item(pod_id).map(|i| i.type_id), Some(CAPSULE_TYPE));
    assert_eq!(extract_set_state_keyvals(&eject_out).len(), 0);

    ctx.advance_secs(11);
    let board_from_pod = borrow_wire_harness(&ctx, &mut p.sess)
        .call_exchange("ship", None, "Board", &[PyRep::Integer(tristan_id)], &kw)
        .await;
    assert!(board_from_pod.result.is_ok(), "boarding from pod failed: {:?}", board_from_pod.result);
    assert_eq!(p.sess.ship_id, Some(tristan_id));
    assert_eq!(extract_set_state_keyvals(&board_from_pod).len(), 0);
    assert!(ctx.db.get_item(pod_id).is_none(), "boarding from capsule must consume pod item");
    assert!(sys_handle.get_ball_snapshot(pod_id).await.unwrap().is_none(), "pod ball must be removed from ballpark");
    assert!(
        collect_remove_ball_ids(&board_from_pod).contains(&pod_id),
        "boarding from pod must emit RemoveBalls([{pod_id}])"
    );

    // Negative: boarding an out-of-range ship fails cleanly without changing ship or emitting SetState.
    let board_far = borrow_wire_harness(&ctx, &mut p.sess)
        .call_exchange("ship", None, "Board", &[PyRep::Integer(far_ship_id)], &kw)
        .await;
    assert!(board_far.result.is_err());
    assert_eq!(p.sess.ship_id, Some(tristan_id));
    assert_eq!(extract_set_state_keyvals(&board_far).len(), 0);
}

#[tokio::test]
async fn stargate_jump_concurrent_displacement_prevents_dual_ball_and_preserves_notifications() {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{mpsc, oneshot};

    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
        CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
        VersionExchange, MACHO_PROXY_NODE_ID,
    };
    use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
    use eve_server::router::NotificationTarget;
    use eve_server::services::space_warp_jump::{
        set_pre_take_ball_hook_for_test, PreTakeBallHook,
    };
    use eve_server::sp6_lifecycle::{
        has_active_disconnected_ship, tick_background_cluster_with_step,
    };

    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);

    let char_id = 91_008_001_i64;
    let (user, pass, ship_id) =
        setup_space_pilot_logged_out(&ctx, 58_001, char_id, RIFTER_TYPE, TEST_SYS_A).await;
    ctx.advance_secs(12);

    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == TEST_SYS_A && j.to_solar_system_id == TEST_SYS_B)
        .cloned()
        .expect("Jita -> Perimeter stargate in SDE");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test macho listener");
    let addr = listener.local_addr().unwrap();
    let (task_tx, mut task_rx) = mpsc::unbounded_channel();
    let accept_ctx = Arc::clone(&ctx);
    let accept_handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&accept_ctx);
            let jh = tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
            if task_tx.send(jh).is_err() {
                break;
            }
        }
    });

    async fn tcp_login(
        addr: std::net::SocketAddr,
        user: &str,
        pass: &str,
    ) -> (TcpStream, u64, u64) {
        let mut s = TcpStream::connect(addr).await.expect("connect macho");
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(cc.as_str(), Some("OK CC"));

        let chal = CryptoChallengePacket::new_password_login(user, pass);
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap(), PyRep::Integer(2));
        let srv_hs = CryptoServerHandshake::from_pyrep(
            &read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        )
        .expect("CryptoServerHandshake");
        write_framed_pyrep(
            &mut s,
            &(CryptoHandshakeResult {
                challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
                func_output: Vec::new(),
                func_result: PyRep::Integer(0),
            })
            .to_pyrep(),
        )
        .await
        .unwrap();
        let ack = CryptoHandshakeAck::from_pyrep(
            &read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        )
        .expect("CryptoHandshakeAck");
        let init_pkt =
            MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
        let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
            panic!("expected Client destination on SessionInitialStateNotification");
        };
        let _sess_chg = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        (s, client_id, ack.user_id)
    }

    let make_call = |client_id: u64, user_id: u64, call_id: u64, service: &str, method: &str, args: Vec<PyRep>| {
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(service.into()),
                call_id: 0,
            },
            user_id: Some(user_id),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes(method),
                    PyRep::Tuple(args),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        }
    };

    async fn read_until_call_rsp(stream: &mut TcpStream) {
        loop {
            let rep = tokio::time::timeout(Duration::from_secs(10), read_framed_pyrep(stream))
                .await
                .expect("timeout waiting for CallRsp")
                .expect("frame read")
                .expect("non-EOF");
            let pkt = MachoPacket::from_pyrep(&rep).expect("MachoPacket");
            assert_ne!(
                pkt.packet_type,
                MachoPacketType::ErrorResponse,
                "unexpected ErrorResponse: {rep:?}"
            );
            if pkt.packet_type == MachoPacketType::CallRsp {
                return;
            }
        }
    }

    // 1. Connect S1, select char_id into space at TEST_SYS_A, and move near the Jita->Perimeter gate.
    let (mut s1, client_id_1, user_id) = tcp_login(addr, &user, &pass).await;
    let s1_task = tokio::time::timeout(Duration::from_secs(5), task_rx.recv())
        .await
        .unwrap()
        .expect("S1 listener task");
    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    read_until_call_rsp(&mut s1).await;

    ctx.advance_secs(12);
    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            2,
            "beyonce",
            "MachoBindObject",
            vec![
                PyRep::Integer(i64::from(TEST_SYS_A)),
                PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetFormations"),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    read_until_call_rsp(&mut s1).await;

    let sys_a_handle = ctx.systems.get_or_spawn(TEST_SYS_A);
    assert!(
        sys_a_handle
            .set_ball_position(ship_id, jita_gate.position + DVec3::new(1_000.0, 0.0, 0.0))
            .await,
        "ship {ship_id} ball must exist in TEST_SYS_A before stargate jump"
    );
    ctx.advance_secs(12);

    // 2. Install deterministic pause barrier immediately before old_handle.take_ball in CmdStargateJump.
    let (paused_tx, paused_rx) = oneshot::channel::<()>();
    let (resume_tx, resume_rx) = oneshot::channel::<()>();
    let hook: PreTakeBallHook = Box::new(move || {
        Box::pin(async move {
            let _ = paused_tx.send(());
            let _ = resume_rx.await;
        })
    });
    set_pre_take_ball_hook_for_test(&ctx, char_id, Some(hook));

    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            3,
            "beyonce",
            "CmdStargateJump",
            vec![PyRep::Integer(jita_gate.stargate_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), paused_rx)
        .await
        .expect("S1 must hit pre-take_ball pause barrier")
        .expect("paused_tx dropped cleanly");

    // 3. Displace account from S2 while S1 is mid-RPC before take_ball.
    let (mut s2, client_id_2, user_id_2) = tcp_login(addr, &user, &pass).await;
    assert_eq!(user_id_2, user_id);
    let s2_task = tokio::time::timeout(Duration::from_secs(5), task_rx.recv())
        .await
        .unwrap()
        .expect("S2 listener task");
    assert!(
        !has_active_disconnected_ship(&ctx.db, char_id),
        "displacing live S1 mid-jump must not record a premature disconnected_ships entry"
    );

    write_framed_pyrep(
        &mut s2,
        &make_call(
            client_id_2,
            user_id,
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    read_until_call_rsp(&mut s2).await;

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Real(7_777_000.0),
        ],
    );

    // 4. Resume S1 and await S1's full RPC exit + disconnect cleanup before ticking cluster.
    let _ = resume_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), s1_task)
        .await
        .expect("S1 handler must exit after displacement")
        .expect("S1 task join ok");

    let mut s1_stole_account_change = false;
    let mut s1_got_jump_error_rsp = false;
    while let Ok(Ok(Some(rep))) =
        tokio::time::timeout(Duration::from_secs(1), read_framed_pyrep(&mut s1)).await
    {
        if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
            if pkt.packet_type == MachoPacketType::ErrorResponse
                && matches!(pkt.destination, MachoAddress::Client { call_id: 3, .. })
            {
                s1_got_jump_error_rsp = true;
            }
            if pkt.packet_type == MachoPacketType::Notification {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "OnAccountChange" {
                        s1_stole_account_change = true;
                    }
                }
            }
        }
    }
    assert!(
        s1_got_jump_error_rsp,
        "displaced S1 must receive ErrorResponse for in-flight CmdStargateJump (call_id=3)"
    );
    assert!(
        !s1_stole_account_change,
        "displaced S1 must not steal S2's OnAccountChange notification"
    );

    // 5. Run two cluster ticks and verify no dual-ball, no TEST_SYS_A disconnect hold, and S2 got its notification.
    for _ in 0..2 {
        ctx.advance_secs(1);
        tick_background_cluster_with_step(&ctx, 1).await;
    }

    let sys_b_handle = ctx.systems.get_or_spawn(TEST_SYS_B);
    let in_a = sys_a_handle.get_ball_snapshot(ship_id).await.unwrap().is_some();
    let in_b = sys_b_handle.get_ball_snapshot(ship_id).await.unwrap().is_some();
    assert!(
        in_a && !in_b,
        "ship {ship_id} must stay strictly in TEST_SYS_A under S2 after displaced jump abort (in_a={in_a}, in_b={in_b})"
    );
    assert!(
        ctx.db
            .with_snapshot(|s| !s.sp6_lifecycle.disconnected_ships.contains_key(&char_id)),
        "disconnected_ships must not retain any TEST_SYS_A entry while S2 owns {char_id}"
    );

    loop {
        let rep = tokio::time::timeout(Duration::from_secs(10), read_framed_pyrep(&mut s2))
            .await
            .expect("S2 must receive preserved OnAccountChange notification")
            .expect("frame read")
            .expect("non-EOF on S2");
        let pkt = MachoPacket::from_pyrep(&rep).expect("MachoPacket");
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnAccountChange" {
                    break;
                }
            }
        }
    }

    set_pre_take_ball_hook_for_test(&ctx, char_id, None);
    accept_handle.abort();
    drop(s2);
    let _ = tokio::time::timeout(Duration::from_secs(5), s2_task).await;
}
