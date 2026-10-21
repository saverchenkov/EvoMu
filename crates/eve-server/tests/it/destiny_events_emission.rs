//! Destiny actions emitted by the live simulation and encoded into `DoDestinyUpdate` exactly in the
//! argument layout the client unpacks (`(stamp, (actionName, argsTuple))`), verified by marshal round-trip.


use eve_db::types::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    SystemDynamicState, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::SystemHandle;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const PILOT: i64 = 90_000_001;
const JITA: u32 = 30_000_142;

async fn login_and_undock(ctx: &ServerContext, sess_id: u64) -> (SessionState, u64, i64) {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(kv) = &undock else { panic!("Undock must return KeyVal") };
    let beyonce = kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = sess.ship_id.expect("active ship");
    (sess, beyonce, ship_id)
}

/// Drains `observer`'s queued destiny updates, checks the marshal round-trip, and returns
/// every `(actionName, args)` pair in order.
async fn drain_actions(handle: &SystemHandle, observer: i64) -> Vec<(String, Vec<PyRep>)> {
    let (_tick, _evs, updates) = handle.drain_observer_updates(observer).await.expect("drain");
    parse_actions(&updates)
}

/// Parses a `DoDestinyUpdate` list into `(actionName, args)` pairs after a marshal round-trip.
fn parse_actions(updates: &PyRep) -> Vec<(String, Vec<PyRep>)> {
    let bytes = eve_proto::marshal::encode(updates);
    let decoded = eve_proto::marshal::decode(&bytes).expect("DoDestinyUpdate marshal round-trip");
    assert_eq!(&decoded, updates, "DoDestinyUpdate must survive marshal encode/decode unchanged");
    let PyRep::List(entries) = decoded else { panic!("DoDestinyUpdate payload must be a list") };
    entries
        .into_iter()
        .map(|e| {
            let PyRep::Tuple(outer) = e else { panic!("entry must be (stamp, action)") };
            assert!(matches!(outer[0], PyRep::Integer(_)), "stamp must be an int");
            let PyRep::Tuple(inner) = &outer[1] else { panic!("action must be (name, args)") };
            let name = inner[0].as_str().expect("action name").to_string();
            let args = match &inner[1] {
                PyRep::Tuple(t) => t.clone(),
                other => vec![other.clone()],
            };
            (name, args)
        })
        .collect()
}

fn find<'a>(actions: &'a [(String, Vec<PyRep>)], name: &str) -> Vec<&'a Vec<PyRep>> {
    actions.iter().filter(|(n, _)| n == name).map(|(_, a)| a).collect()
}

fn real(p: &PyRep) -> f64 {
    match p {
        PyRep::Real(v) => *v,
        other => panic!("expected real, got {other:?}"),
    }
}

async fn clear_invulnerability(handle: &SystemHandle, ship_id: i64) {
    let mut cs = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    cs.warp_exit_invulnerable_remaining_ticks = 0;
    cs.sync_invuln_with_undock_timer = false;
    cs.is_cloaked = false;
    handle.set_combat_state(ship_id, cs).await.unwrap();
}

fn fit(ctx: &ServerContext, item_id: i64, type_id: u32, location_id: i64, flag: InventoryFlag) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: PILOT,
                location_id,
                flag,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[tokio::test]
async fn test_set_state_emitted_on_update_state_request() {
    // parity: destiny:SetState
    eve_testkit::attest(&["destiny:SetState"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8101).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    let _ = drain_actions(&handle, ship_id).await;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "UpdateStateRequest", &[], &HashMap::new(), None)
        .await
        .expect("UpdateStateRequest");
    let actions = drain_actions(&handle, ship_id).await;
    let set_state = find(&actions, "SetState");
    assert_eq!(set_state.len(), 1, "UpdateStateRequest must queue exactly one SetState");
    let PyRep::KeyVal(kv) = &set_state[0][0] else { panic!("SetState arg must be a KeyVal") };
    assert_eq!(kv["ego"].as_i64(), Some(ship_id));
    let PyRep::Bytes(state) = &kv["state"] else { panic!("state must be the binary ball stream") };
    let (_, _, balls) = eve_destiny::wire::decode_add_balls(state).expect("binary destiny state decodes");
    assert!(balls.iter().any(|b| b.id == ship_id), "ego ball must be in the state stream");
    assert!(balls.iter().any(|b| b.id == 60_003_760), "Jita 4-4 station must be in the state stream");
    let PyRep::List(slims) = &kv["slims"] else { panic!("slims must be a list") };
    assert_eq!(slims.len(), balls.len(), "one slim item per ball");
    let PyRep::Dict(damage) = &kv["damageState"] else { panic!("damageState must be a dict") };
    let ship_dmg = damage
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(ship_id))
        .map(|(_, v)| v)
        .expect("ship damage state");
    let PyRep::Tuple(d) = ship_dmg else { panic!() };
    let PyRep::Tuple(shield) = &d[0] else { panic!("shield entry must be (ratio, tau, stamp)") };
    assert_eq!(shield.len(), 3);
    assert_eq!(kv["industryLevel"].as_i64(), Some(0));
    assert_eq!(kv["researchLevel"].as_i64(), Some(0));
    assert_eq!(kv["dbuffState"], PyRep::List(Vec::new()));
}

#[tokio::test]
async fn test_warp_to_and_warp_special_fx_emitted_on_warp() {
    // parity: destiny:WarpTo, destiny:OnSpecialFX
    eve_testkit::attest(&["destiny:WarpTo", "destiny:OnSpecialFX"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8102).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    let _ = drain_actions(&handle, ship_id).await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(50_001_249)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdWarpToStuff");
    let (_, ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination, warp_speed_au_s, .. } = ball.mode else { panic!("must be warping") };
    let actions = drain_actions(&handle, ship_id).await;
    let warp = find(&actions, "WarpTo");
    assert_eq!(warp.len(), 1);
    let w = warp[0];
    assert_eq!(w.len(), 6, "WarpTo(entityID, x, y, z, distance, warpSpeed)");
    assert_eq!(w[0], PyRep::Integer(ship_id));
    assert_eq!((real(&w[1]), real(&w[2]), real(&w[3])), (destination.x, destination.y, destination.z));
    assert!(matches!(w[4], PyRep::Integer(_)), "distance is an int");
    assert_eq!(w[5], PyRep::Integer((warp_speed_au_s * 10.0).round() as i64), "warpSpeed is AU/s x 10");

    let fx = find(&actions, "OnSpecialFX");
    assert_eq!(fx.len(), 1, "warp start emits one OnSpecialFX");
    let f = fx[0];
    assert_eq!(f.len(), 14, "OnSpecialFX carries the V24 14-argument layout");
    assert_eq!(f[0], PyRep::Integer(ship_id));
    assert_eq!(f[5], PyRep::String("effects.Warping".into()));
    assert_eq!(f[6], PyRep::Bool(false), "warp is not offensive");
    assert_eq!((f[7].clone(), f[8].clone()), (PyRep::Integer(1), PyRep::Integer(1)), "start=1, active=1");
    assert_eq!(f[9], PyRep::Integer(-1), "duration -1");
    assert_eq!(f[12], PyRep::Integer(0), "timeFromStart 0");

    // Landing ends the effect: OnSpecialFX(start=0, active=0).
    let mut ended = None;
    for _ in 0..600 {
        // Each tick flushes observer queues into the tick broadcast that is sent to clients.
        let tb = handle.step_manual(1.0).await.unwrap();
        let Some(py) = tb.observer_updates_pyrep.get(&ship_id) else { continue };
        let acts = parse_actions(py);
        if let Some(a) = find(&acts, "OnSpecialFX").into_iter().next() {
            ended = Some(a.clone());
            break;
        }
    }
    let end = ended.expect("warp exit must emit OnSpecialFX");
    assert_eq!(end[5], PyRep::String("effects.Warping".into()));
    assert_eq!((end[7].clone(), end[8].clone()), (PyRep::Integer(0), PyRep::Integer(0)));
}

#[tokio::test]
async fn test_goto_point_emitted_by_cmd_goto_point() {
    // parity: destiny:GotoPoint
    eve_testkit::attest(&["destiny:GotoPoint"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8103).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    let (_, start) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let target = start.position + DVec3::new(0.0, 40_000.0, 0.0);
    let _ = drain_actions(&handle, ship_id).await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce),
        "CmdGotoPoint",
        &[PyRep::Real(target.x), PyRep::Real(target.y), PyRep::Real(target.z)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdGotoPoint");
    let actions = drain_actions(&handle, ship_id).await;
    let gp = find(&actions, "GotoPoint");
    assert_eq!(gp.len(), 1);
    assert_eq!(
        gp[0].as_slice(),
        &[PyRep::Integer(ship_id), PyRep::Real(target.x), PyRep::Real(target.y), PyRep::Real(target.z)]
    );
    let (_, ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::GotoDirection { direction } = ball.mode else { panic!("ship must steer toward the point") };
    assert!((direction - DVec3::Y).length() < 1e-9, "heading must point at the target");
    let err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdGotoPoint", &[PyRep::Real(1.0)], &HashMap::new(), None)
        .await
        .expect_err("CmdGotoPoint requires x, y, z");
    assert_eq!(err.code, "InvalidArguments");
}

#[tokio::test]
async fn test_damage_state_change_emitted_on_damage() {
    // parity: destiny:OnDamageStateChange, destiny:OnFleetDamageStateChange, destiny:TerminalPlayDestructionEffect
    eve_testkit::attest(&[
        "destiny:OnDamageStateChange",
        "destiny:OnFleetDamageStateChange",
        "destiny:TerminalPlayDestructionEffect",
    ]);
    let ctx = ServerContext::new();
    let (_sess, _beyonce, ship_id) = login_and_undock(&ctx, 8104).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    clear_invulnerability(&handle, ship_id).await;
    let (_, ship_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let target_id = 990_000_001i64;
    handle
        .spawn_ball(Ball {
            id: target_id,
            position: ship_ball.position + DVec3::new(500.0, 0.0, 0.0),
            ..ship_ball
        })
        .await
        .unwrap();
    clear_invulnerability(&handle, target_id).await;
    let _ = drain_actions(&handle, ship_id).await;
    let hit = DamageVector { em: 60.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (applied, _, _) = handle.apply_damage(ship_id, hit).await.unwrap().unwrap();
    assert!(applied > 0.0);
    let cs = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let actions = drain_actions(&handle, ship_id).await;
    for action_name in ["OnDamageStateChange", "OnFleetDamageStateChange"] {
        let ds = find(&actions, action_name);
        assert_eq!(ds.len(), 1, "{action_name} must be emitted once per damage application");
        assert_eq!(ds[0].len(), 2, "{action_name}(entityID, damageState)");
        assert_eq!(ds[0][0], PyRep::Integer(ship_id));
        let PyRep::Tuple(state) = &ds[0][1] else { panic!("damage state tuple") };
        assert_eq!(state.len(), 3);
        let PyRep::Tuple(shield) = &state[0] else { panic!("(shield, tau, stamp)") };
        assert!((real(&shield[0]) - cs.shield_hp / cs.max_shield_hp).abs() < 1e-9);
        assert_eq!(real(&shield[1]), cs.shield_recharge_time_ms, "tau is the ship's shield recharge time");
        assert!(matches!(shield[2], PyRep::Integer(_)));
        assert!((real(&state[1]) - cs.armor_hp / cs.max_armor_hp).abs() < 1e-9);
        assert!((real(&state[2]) - cs.structure_hp / cs.max_structure_hp).abs() < 1e-9);
    }

    let lethal = DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (_, destroyed, _) = handle.apply_damage(target_id, lethal).await.unwrap().unwrap();
    assert!(destroyed, "lethal damage must destroy target ball");
    let actions = drain_actions(&handle, ship_id).await;
    let term = find(&actions, "TerminalPlayDestructionEffect");
    assert_eq!(term.len(), 1, "destroying a grid ball must emit TerminalPlayDestructionEffect");
    assert_eq!(term[0][0], PyRep::Integer(target_id));
    assert!(matches!(term[0][1], PyRep::Integer(_) | PyRep::None));
}

#[tokio::test]
async fn test_cloak_ball_emitted_on_cloak_toggle() {
    // parity: destiny:CloakBall, destiny:OnShipStateUpdate
    eve_testkit::attest(&["destiny:CloakBall", "destiny:OnShipStateUpdate"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8105).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    clear_invulnerability(&handle, ship_id).await;
    let cloak_id = 78_000_001i64;
    fit(&ctx, cloak_id, 11_370, ship_id, InventoryFlag::HiSlot0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cloak_id);
    });
    let _ = drain_actions(&handle, ship_id).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdStop", &[], &kw, None).await.expect("CmdStop");
    assert!(handle.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);
    let actions = drain_actions(&handle, ship_id).await;
    let on = find(&actions, "CloakBall");
    assert_eq!(on.len(), 1, "activating the cloak emits one CloakBall");
    assert_eq!(on[0].as_slice(), &[PyRep::Integer(ship_id), PyRep::Bool(true)]);
    let ship_state = find(&actions, "OnShipStateUpdate");
    assert_eq!(ship_state.len(), 1, "activating cloak emits OnShipStateUpdate");
    let PyRep::Dict(mod_dict) = &ship_state[0][0] else { panic!("shipState must be a dict") };
    let (_, PyRep::Tuple(mod_tuple)) = mod_dict.iter().find(|(k, _)| *k == PyRep::Integer(cloak_id)).expect("cloak entry") else {
        panic!("moduleState must be a 6-tuple");
    };
    assert_eq!(mod_tuple.len(), 6);
    assert_eq!((&mod_tuple[0], &mod_tuple[1], &mod_tuple[2]), (&PyRep::Integer(PILOT), &PyRep::Integer(cloak_id), &PyRep::Integer(2)));
    assert_eq!((real(&mod_tuple[4]), real(&mod_tuple[5])), (1.0, 0.0));

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_id);
    });
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdStop", &[], &kw, None).await.expect("CmdStop");
    assert!(!handle.get_combat_state(ship_id).await.unwrap().unwrap().is_cloaked);
    let actions = drain_actions(&handle, ship_id).await;
    let off = find(&actions, "CloakBall");
    assert_eq!(off.len(), 1, "deactivating the cloak emits one CloakBall");
    assert_eq!(off[0].as_slice(), &[PyRep::Integer(ship_id), PyRep::Bool(false)]);
    // No state change, no CloakBall.
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdStop", &[], &kw, None).await.expect("CmdStop");
    assert!(find(&drain_actions(&handle, ship_id).await, "CloakBall").is_empty());
}

#[tokio::test]
async fn test_drone_state_change_emitted_on_launch() {
    // parity: destiny:OnDroneStateChange
    eve_testkit::attest(&["destiny:OnDroneStateChange"]);
    let ctx = ServerContext::new();
    let (mut sess, _beyonce, ship_id) = login_and_undock(&ctx, 8106).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    // Vexor hull (SDE drone bandwidth) with a Hobgoblin I in the drone bay.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 626;
    });
    ctx.invalidate_ship_fitting(ship_id);
    let drone_item = 79_000_001i64;
    fit(&ctx, drone_item, 2_454, ship_id, InventoryFlag::DroneBay);
    let _ = drain_actions(&handle, ship_id).await;
    let launched = ctx
        .dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_item)])], &HashMap::new(), None)
        .await
        .expect("LaunchDrones");
    let PyRep::List(ids) = launched else { panic!("LaunchDrones returns launched ball IDs") };
    assert_eq!(ids.len(), 1);
    let drone_ball = ids[0].as_i64().unwrap();
    let actions = drain_actions(&handle, ship_id).await;
    let st = find(&actions, "OnDroneStateChange");
    assert_eq!(st.len(), 1, "one OnDroneStateChange per launched drone");
    assert_eq!(
        st[0].as_slice(),
        &[
            PyRep::Integer(drone_ball),
            PyRep::Integer(PILOT),
            PyRep::Integer(ship_id),
            PyRep::Integer(0),
            PyRep::Integer(2_454),
            PyRep::Integer(PILOT),
            PyRep::None,
        ],
        "(droneID, ownerID, controllerID, activityState, droneTypeID, controllerOwnerID, targetID)"
    );
}

#[tokio::test]
async fn test_on_dbuff_updated_and_on_dot_victim_updated() {
    // parity: destiny:OnDbuffUpdated, destiny:OnDotVictimUpdated
    eve_testkit::attest(&["destiny:OnDbuffUpdated", "destiny:OnDotVictimUpdated"]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8107).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    clear_invulnerability(&handle, ship_id).await;
    let (_, ship_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let st_id = 1_049_000_001i64;
    let corp_id = sess.corporation_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832, // Astrahus
                name: "Jita Tether Hub".into(),
                solar_system_id: JITA,
                owner_corp_id: corp_id,
                position: [ship_ball.position.x, ship_ball.position.y, ship_ball.position.z],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    let _ = drain_actions(&handle, ship_id).await;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "UpdateStateRequest", &[], &HashMap::new(), None)
        .await
        .expect("UpdateStateRequest");
    let actions = drain_actions(&handle, ship_id).await;
    let db = find(&actions, "OnDbuffUpdated");
    assert_eq!(db.len(), 1, "entering tether emits OnDbuffUpdated");
    assert_eq!(db[0][0], PyRep::Integer(ship_id));
    let PyRep::List(entries) = &db[0][1] else { panic!("dbuffState must be a list") };
    assert_eq!(entries.len(), 1);
    let PyRep::Tuple(pair) = &entries[0] else { panic!("(dbuffCollectionID, (value, expiryTime))") };
    assert_eq!(pair[0], PyRep::Integer(60));
    let PyRep::Tuple(val_exp) = &pair[1] else { panic!("(value, expiryTime)") };
    assert_eq!(real(&val_exp[0]), 1.0);
    assert!(val_exp[1].as_i64().unwrap() > 116_444_736_000_000_000);

    // Move out of tether range and verify OnDbuffUpdated clears dbuffs, then tick Breacher Pod DoT.
    handle.set_ball_position(ship_id, ship_ball.position + DVec3::new(500_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "UpdateStateRequest", &[], &HashMap::new(), None)
        .await
        .expect("UpdateStateRequest");
    let actions = drain_actions(&handle, ship_id).await;
    let db_off = find(&actions, "OnDbuffUpdated");
    assert_eq!(db_off.len(), 1);
    assert_eq!(db_off[0].as_slice(), &[PyRep::Integer(ship_id), PyRep::List(Vec::new())]);

    let mut cs: CombatState = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.is_tethered = false;
    cs.active_breacher_dot = Some((200.0, 0.01, 5));
    handle.set_combat_state(ship_id, cs).await.unwrap();
    let tb = handle.step_manual(1.0).await.unwrap();
    let acts = parse_actions(tb.observer_updates_pyrep.get(&ship_id).expect("observer updates"));
    let dot = find(&acts, "OnDotVictimUpdated");
    assert_eq!(dot.len(), 1, "active Breacher Pod DoT tick must emit OnDotVictimUpdated");
    assert_eq!(dot[0][0], PyRep::Integer(ship_id));
    let PyRep::List(apps) = &dot[0][1] else { panic!("dmgAppsInfo must be a list") };
    assert_eq!(apps.len(), 1);
    let PyRep::KeyVal(app) = &apps[0] else { panic!("dmgAppsInfo entry must be a KeyVal") };
    assert_eq!(app["targetID"], PyRep::Integer(ship_id));
    assert_eq!(app["weaponTypeID"], PyRep::Integer(84_115));
    assert_eq!((real(&app["maxDamage"]), real(&app["maxHPNormalizedRatio"])), (200.0, 0.01));
    assert_eq!(app["activityState"], PyRep::Integer(1));
}

#[tokio::test]
async fn test_slim_item_interactive_and_packaged_action_on_eject_and_board() {
    // parity: destiny:OnSlimItemChange, destiny:SetBallInteractive, destiny:PackagedAction
    eve_testkit::attest(&[
        "destiny:OnSlimItemChange",
        "destiny:SetBallInteractive",
        "destiny:PackagedAction",
    ]);
    let ctx = ServerContext::new();
    let (mut sess, _beyonce, ship_id) = login_and_undock(&ctx, 8108).await;
    let handle = ctx.systems.get_or_spawn(JITA);
    clear_invulnerability(&handle, ship_id).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "ship", None, "Eject", &[], &kw, None).await.expect("Eject");
    let pod_id = sess.ship_id.expect("capsule active");
    assert_ne!(pod_id, ship_id);
    let actions = drain_actions(&handle, pod_id).await;
    let slim_ch: Vec<_> = find(&actions, "OnSlimItemChange")
        .into_iter()
        .filter(|a| a.first() == Some(&PyRep::Integer(ship_id)))
        .collect();
    assert_eq!(slim_ch.len(), 1);
    assert_eq!(slim_ch[0][0], PyRep::Integer(ship_id));
    let PyRep::Object { type_name, args } = &slim_ch[0][1] else { panic!("slim item object") };
    assert_eq!(type_name, "foo.SlimItem");
    assert_eq!(args.dict_get("itemID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(args.dict_get("charID"), Some(&PyRep::None), "ejected ship has no pilot");
    let inter = find(&actions, "SetBallInteractive");
    assert_eq!(inter.len(), 1);
    assert_eq!(inter[0].as_slice(), &[PyRep::Integer(ship_id), PyRep::Integer(0)]);
    let pkg = find(&actions, "PackagedAction");
    assert_eq!(pkg.len(), 1);
    let PyRep::Bytes(pkg_bytes) = &pkg[0][0] else { panic!("PackagedAction arg is marshaled bytes") };
    let inner_acts = parse_actions(&eve_proto::marshal::decode(pkg_bytes).expect("decode PackagedAction"));
    assert_eq!(find(&inner_acts, "OnSlimItemChange").len(), 1);
    assert_eq!(find(&inner_acts, "SetBallInteractive").len(), 1);

    let _ = drain_actions(&handle, ship_id).await;
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("Board");
    assert_eq!(sess.ship_id, Some(ship_id));
    let actions = drain_actions(&handle, ship_id).await;
    let inter_on = find(&actions, "SetBallInteractive");
    assert_eq!(inter_on.len(), 1);
    assert_eq!(inter_on[0].as_slice(), &[PyRep::Integer(ship_id), PyRep::Integer(1)]);
}

#[tokio::test]
async fn test_set_ball_free_radius_harmonic_sovereignty_and_controller_event() {
    // parity: destiny:SetBallFree, destiny:SetBallRadius, destiny:SetBallHarmonic
    // parity: destiny:OnSovereigntyChanged, destiny:OnClientControllerEvent
    eve_testkit::attest(&[
        "destiny:SetBallFree",
        "destiny:SetBallRadius",
        "destiny:SetBallHarmonic",
        "destiny:OnSovereigntyChanged",
        "destiny:OnClientControllerEvent",
    ]);
    let ctx = ServerContext::new();
    let (mut sess, beyonce, ship_id) = login_and_undock(&ctx, 8109).await;
    let kw = HashMap::new();
    // Stargate jump emits OnClientControllerEvent on the origin stargate ball.
    let gate_id = 50_001_248i64;
    let gate_pos = ctx.sde.jumps[&gate_id].position;
    let dest_sys = ctx.sde.jumps[&gate_id].to_solar_system_id;
    let jita_handle = ctx.systems.get_or_spawn(JITA);
    jita_handle.set_ball_position(ship_id, gate_pos).await;
    let (_, ship_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let gate_obs = 990_000_009i64;
    jita_handle.spawn_ball(Ball { id: gate_obs, ..ship_ball }).await.unwrap();
    let _ = drain_actions(&jita_handle, gate_obs).await;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(gate_id)], &kw, None)
        .await
        .expect("CmdStargateJump");
    let jump_acts = drain_actions(&jita_handle, gate_obs).await;
    let ctrl = find(&jump_acts, "OnClientControllerEvent");
    assert_eq!(ctrl.len(), 1);
    assert_eq!(
        ctrl[0].as_slice(),
        &[
            PyRep::Integer(gate_id),
            PyRep::Integer(ship_id),
            PyRep::String("GateActivate".into()),
            PyRep::Integer(1),
            PyRep::None,
        ]
    );

    // Move to null-sec system 1DQ1-A (30004759) to anchor a Mobile Small Warp Disruptor I (12198) and update sovereignty.
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let null_sys = 30_004_759u32;
    let mut ball = dest_handle.remove_ball(ship_id).await.unwrap();
    ball.position = DVec3::new(1.0e6, 0.0, 0.0);
    let null_handle = ctx.systems.get_or_spawn(null_sys);
    null_handle.spawn_ball(ball.clone()).await.unwrap();
    ctx.db.set_character_solar_system(PILOT, null_sys, &ctx.sde).unwrap();
    sess.solar_system_id = Some(null_sys);
    sess.alliance_id = Some(99_000_123);
    let mwd_item = 89_000_001i64;
    fit(&ctx, mwd_item, 12_198, ship_id, InventoryFlag::Cargo);
    let (corp, alliance) = (98_100_001i64, 99_000_123i64);
    sess.corporation_id = Some(corp);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.corporation_id = corp;
            ch.alliance_id = Some(alliance);
        }
        snap.corporations.insert(
            corp,
            CorporationRecord {
                corporation_id: corp,
                name: "Sov Corp".into(),
                ticker: "SOVC".into(),
                ceo_id: PILOT,
                alliance_id: Some(alliance),
                members: HashSet::from([PILOT]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance,
            AllianceRecord {
                alliance_id: alliance,
                name: "Sov Alliance".into(),
                short_name: "SOVA".into(),
                executor_corp_id: corp,
                member_corp_ids: HashSet::from([corp]),
            },
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
        snap.structures.insert(
            1_049_500_001,
            UpwellStructureRecord {
                structure_id: 1_049_500_001,
                type_id: 32_458,
                name: "Sov Hub".into(),
                solar_system_id: null_sys,
                owner_corp_id: corp,
                position: [ball.position.x, ball.position.y, ball.position.z],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        let sk = snap.skills.entry(PILOT).or_default();
        sk.insert(3_394, TrainedSkillRecord { type_id: 3_394, level: 5, skill_points: 256_000.0 });
        sk.insert(3_435, TrainedSkillRecord { type_id: 3_435, level: 5, skill_points: 256_000.0 });
        sk.insert(11_584, TrainedSkillRecord { type_id: 11_584, level: 5, skill_points: 256_000.0 });
    });
    let _ = drain_actions(&null_handle, ship_id).await;
    ctx.dispatch_rpc(
        &mut sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mwd_item), PyRep::Tuple(vec![PyRep::Real(ball.position.x + 200.0), PyRep::Real(0.0), PyRep::Real(0.0)])],
        &kw,
        None,
    )
    .await
    .expect("AnchorStructure");
    ctx.dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[PyRep::Integer(null_sys as i64)], &kw, None)
        .await
        .expect("AcquireSkyhooks");
    let acts = drain_actions(&null_handle, ship_id).await;
    assert_eq!(find(&acts, "SetBallFree")[0].as_slice(), &[PyRep::Integer(mwd_item), PyRep::Integer(0)]);
    assert_eq!(find(&acts, "SetBallHarmonic")[0].as_slice(), &[PyRep::Integer(mwd_item), PyRep::Integer(1)]);
    let rad = find(&acts, "SetBallRadius");
    assert_eq!(rad.len(), 1);
    assert_eq!(rad[0][0], PyRep::Integer(mwd_item));
    assert!(real(&rad[0][1]) > 0.0);
    let sov = find(&acts, "OnSovereigntyChanged");
    assert_eq!(sov.len(), 1);
    assert_eq!(sov[0].as_slice(), &[PyRep::Integer(null_sys as i64), PyRep::Integer(99_000_123)]);
}
