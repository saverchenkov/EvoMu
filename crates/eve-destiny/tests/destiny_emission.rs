//! Destiny actions emitted by the `Ballpark` simulation on real state changes (P-7 / SP-3).
//!
//! Every test drives the public `Ballpark` API, drains the per-observer queue the server ships in
//! `DoDestinyUpdate` (`system_actor.rs` `build_tick_broadcast` -> `encode_destiny_updates`), and
//! asserts the encoded `(stamp, (action, args))` tuple against the V24 client handler signature
//! (from the client's own `michelle.pyj` bytecode) and evemu_Crucible's encoder.

use eve_destiny::wire::{encode_destiny_updates, BALL_FLAG_IS_FREE, BALL_FLAG_IS_INTERACTIVE};
use eve_destiny::wire::{BALL_FLAG_IS_MASSIVE, DST_BALL_GOTO, DST_BALL_STOP};
use eve_destiny::{Ball, BallId, Ballpark, DamageVector, DestinyEvent, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use glam::DVec3;

const SYSTEM_ID: u32 = 30_000_142;
const SHIP_A: BallId = 1_000_000_001;
const SHIP_B: BallId = 1_000_000_002;

/// Test fixture ship (frigate-sized values, test-only).
fn ship(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode,
    }
}

fn pilot_meta(char_id: i64, corp_id: i64) -> SlimItemMeta {
    SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id: char_id,
        char_id: Some(char_id),
        corp_id,
        alliance_id: -1,
        security_status: 0.0,
        is_cloaked: false,
    }
}

/// Drains `obs`'s queue and encodes it exactly as the server puts it into `DoDestinyUpdate`.
fn wire_for(park: &mut Ballpark, obs: BallId) -> Vec<PyRep> {
    let events = park.drain_observer_events(obs);
    let stamp = park.current_tick as u32;
    match encode_destiny_updates(stamp, &events, Some(park)) {
        PyRep::List(v) => v,
        other => panic!("DoDestinyUpdate list expected, got {other:?}"),
    }
}

/// Splits `(stamp, (name, args))`.
fn split(entry: &PyRep) -> (i64, &str, &PyRep) {
    let PyRep::Tuple(outer) = entry else { panic!("entry tuple expected: {entry:?}") };
    assert_eq!(outer.len(), 2, "(stamp, action) pair");
    let PyRep::Tuple(action) = &outer[1] else { panic!("action tuple expected") };
    assert_eq!(action.len(), 2, "(name, args) pair");
    (outer[0].as_i64().expect("int stamp"), action[0].as_str().expect("name"), &action[1])
}

fn args_of<'a>(actions: &'a [PyRep], name: &str) -> Vec<&'a [PyRep]> {
    actions
        .iter()
        .filter_map(|e| {
            let (_, n, args) = split(e);
            match (n == name, args) {
                (true, PyRep::Tuple(a)) => Some(a.as_slice()),
                _ => None,
            }
        })
        .collect()
}

fn names(actions: &[PyRep]) -> Vec<String> {
    actions.iter().map(|e| split(e).1.to_string()).collect()
}

/// Little-endian cursor over the binary destiny state buffer.
struct Cur<'a>(&'a [u8], usize);
impl Cur<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let v: [u8; N] = self.0[self.1..self.1 + N].try_into().unwrap();
        self.1 += N;
        v
    }
    fn u8(&mut self) -> u8 { self.take::<1>()[0] }
    fn i32(&mut self) -> i32 { i32::from_le_bytes(self.take()) }
    fn u32(&mut self) -> u32 { u32::from_le_bytes(self.take()) }
    fn i64(&mut self) -> i64 { i64::from_le_bytes(self.take()) }
    fn f32(&mut self) -> f32 { f32::from_le_bytes(self.take()) }
    fn f64(&mut self) -> f64 { f64::from_le_bytes(self.take()) }
}

/// Asserts BallHeader + MassSector + DataSector (evemu_Crucible `DestinyStructs.h:95-122`) for a
/// free, non-rigid ball and returns after the DataSector (cursor at the mode-specific struct).
fn assert_free_ball_prefix(c: &mut Cur, b: &Ball, mode: u8, corp: i32, cloak: u8) {
    assert_eq!(c.i64(), b.id, "BallHeader.entityID");
    assert_eq!(c.u8(), mode, "BallHeader.mode");
    assert_eq!(c.f32(), b.radius as f32, "BallHeader.radius");
    assert_eq!([c.f64(), c.f64(), c.f64()], b.position.to_array(), "BallHeader.pos");
    let flags = BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE;
    assert_eq!(c.u8(), flags, "BallHeader.flags (Free|Massive|Interactive = 0x0D)");
    assert_eq!(c.f64(), b.mass, "MassSector.mass");
    assert_eq!(c.u8(), cloak, "MassSector.cloak");
    assert_eq!(c.i64(), -1, "MassSector.allianceID (no alliance -> -1, Ship.cpp:2612)");
    assert_eq!(c.i32(), corp, "MassSector.corporationID");
    assert_eq!(c.i32(), -1, "MassSector.harmonic");
    assert_eq!(c.f32(), b.max_velocity as f32, "DataSector.maxSpeed");
    assert_eq!([c.f64(), c.f64(), c.f64()], b.velocity.to_array(), "DataSector.vel");
    assert_eq!(c.f32(), b.inertia as f32, "DataSector.inertia");
    assert_eq!(c.f32(), b.speed_fraction as f32, "DataSector.speedfraction");
}

fn damage_tuple(shield: f64, tau_ms: f64, stamp: i64, armor: f64, hull: f64) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Tuple(vec![PyRep::Real(shield), PyRep::Real(tau_ms), PyRep::Integer(stamp)]),
        PyRep::Real(armor),
        PyRep::Real(hull),
    ])
}

/// Two pilots on one grid: A stopped, B flying `GotoDirection(+X)` at half speed.
fn two_ship_park() -> Ballpark {
    let mut park = Ballpark::new(SYSTEM_ID);
    park.add_ball(ship(SHIP_A, DVec3::new(1_000.0, 2_000.0, 3_000.0), MovementMode::Stop));
    park.set_ball_slim_meta(SHIP_A, pilot_meta(90_000_001, 1_000_170));
    let mut b = ship(SHIP_B, DVec3::new(6_000.0, 2_000.0, 3_000.0), MovementMode::GotoDirection {
        direction: DVec3::X,
    });
    b.velocity = DVec3::new(120.0, 0.0, 0.0);
    b.speed_fraction = 0.5;
    park.add_ball(b);
    park.set_ball_slim_meta(SHIP_B, pilot_meta(90_000_002, 1_000_171));
    park.drain_all_observer_events();
    park
}

#[tokio::test]
async fn test_set_state_full_state_buffer_for_observer() {
    // parity: destiny:SetState
    eve_testkit::attest(&["destiny:SetState"]);
    let mut park = two_ship_park();
    // Real damage so the damageState dict carries non-trivial values: 45 EM on 450 shield.
    park.apply_damage_to_ball(SHIP_B, DamageVector::new(45.0, 0.0, 0.0, 0.0));
    park.drain_all_observer_events();

    assert!(park.queue_set_state(SHIP_A));
    let tick = park.current_tick as i64;
    let actions = wire_for(&mut park, SHIP_A);
    assert_eq!(names(&actions), vec!["SetState"]);
    let (stamp, _, args) = split(&actions[0]);
    assert_eq!(stamp, tick);
    // Client: `Park.SetState(self, bag)` (michelle.pyj:1224) reads bag.droneState, solItem, state,
    // ego, slims, damageState, effectStates, industryLevel, researchLevel, dbuffState
    // (michelle.pyj:1228-1280); Crucible SetState element: Destiny.xmlp:139-189.
    let PyRep::Tuple(a) = args else { panic!("args tuple") };
    assert_eq!(a.len(), 1);
    let PyRep::KeyVal(bag) = &a[0] else { panic!("util.KeyVal bag expected") };
    assert_eq!(bag["stamp"], PyRep::Integer(tick));
    assert_eq!(bag["ego"], PyRep::Integer(SHIP_A));
    assert_eq!(bag["effectStates"], PyRep::List(vec![]));
    assert_eq!(bag["dbuffState"], PyRep::List(vec![]));
    assert_eq!(bag["industryLevel"], PyRep::Integer(0));
    assert_eq!(bag["researchLevel"], PyRep::Integer(0));
    let PyRep::Rowset { columns, rows } = &bag["droneState"] else { panic!("droneState rowset") };
    assert_eq!(columns.join(","), "droneID,ownerID,controllerID,activityState,typeID,controllerOwnerID,targetID");
    assert!(rows.is_empty());
    // util.Row state is `(header, line)` (carbon/common/script/sys/row.pyj:98-99).
    let PyRep::Object { type_name, args: row } = &bag["solItem"] else { panic!("solItem") };
    assert_eq!(type_name, "util.Row");
    let PyRep::Tuple(hl) = row.as_ref() else { panic!("(header, line)") };
    let (PyRep::List(header), PyRep::List(line)) = (&hl[0], &hl[1]) else { panic!("lists") };
    assert_eq!((header.len(), line.len()), (11, 11));
    assert_eq!(header[0].as_str(), Some("itemID"));
    assert_eq!(line[0], PyRep::Integer(SYSTEM_ID as i64));
    assert_eq!(line[3], PyRep::Integer(park.constellation_id as i64));

    // slims: foo.SlimItem objects whose state is a plain dict, one per ball in the buffer.
    let PyRep::List(slims) = &bag["slims"] else { panic!("slims") };
    assert_eq!(slims.len(), 2);
    for (slim, (id, owner, corp)) in slims.iter().zip([(SHIP_A, 90_000_001, 1_000_170), (SHIP_B, 90_000_002, 1_000_171)]) {
        let PyRep::Object { type_name, args } = slim else { panic!("slim object") };
        assert_eq!(type_name, "foo.SlimItem");
        assert!(matches!(args.as_ref(), PyRep::Dict(_)), "SlimItem state must be a dict");
        assert_eq!(slim.dict_get("itemID"), Some(&PyRep::Integer(id)));
        assert_eq!(slim.dict_get("ownerID"), Some(&PyRep::Integer(owner)));
        assert_eq!(slim.dict_get("corpID"), Some(&PyRep::Integer(corp)));
        assert_eq!(slim.dict_get("typeID"), Some(&PyRep::Integer(587)));
    }
    // damageState: {ballID: ((shield, rechargeMs, stamp), armor, structure)} (Destiny.xmlp:303-317).
    let PyRep::Dict(dmg) = &bag["damageState"] else { panic!("damageState dict") };
    assert_eq!(dmg.len(), 2);
    assert_eq!(dmg[0], (PyRep::Integer(SHIP_A), damage_tuple(1.0, 120_000.0, tick, 1.0, 1.0)));
    let PyRep::Tuple(b_dmg) = &dmg[1].1 else { panic!() };
    let PyRep::Tuple(b_shield) = &b_dmg[0] else { panic!() };
    let PyRep::Real(b_ratio) = b_shield[0] else { panic!() };
    // 405 HP left of 450 after 45 EM damage (shield resonance 1.0); no tick has run since.
    assert!((b_ratio - 405.0 / 450.0).abs() < 1e-12, "shield ratio {b_ratio}");

    // Binary state buffer: AddBall_header {u8 packet_type = 0 "full state", u32 stamp}
    // (DestinyStructs.h:85-88), then one ball per slim in id order.
    let PyRep::Bytes(state) = &bag["state"] else { panic!("state bytes") };
    let mut c = Cur(state, 0);
    assert_eq!(c.u8(), 0, "packet_type 0 = full state");
    assert_eq!(c.u32() as i64, tick);
    let a_ball = park.balls[&SHIP_A].clone();
    assert_free_ball_prefix(&mut c, &a_ball, DST_BALL_STOP, 1_000_170, 0);
    assert_eq!(c.u8(), 0xFF, "STOP_Struct.formationID (DestinyStructs.h:149-151)");
    let b_ball = park.balls[&SHIP_B].clone();
    assert_free_ball_prefix(&mut c, &b_ball, DST_BALL_GOTO, 1_000_171, 0);
    assert_eq!(c.u8(), 0xFF, "GOTO_Struct.formationID");
    assert_eq!([c.f64(), c.f64(), c.f64()], [1.0, 0.0, 0.0], "GOTO_Struct direction");
    assert_eq!(c.1, state.len(), "no trailing bytes: 5 + 100 + 124");
    assert_eq!(state.len(), 229);
}

#[tokio::test]
async fn test_add_balls_on_grid_entry_and_add_balls2_for_resident() {
    // parity: destiny:AddBalls, destiny:AddBalls2
    eve_testkit::attest(&["destiny:AddBalls", "destiny:AddBalls2"]);
    // A sits in grid 1 at the origin; B sits alone 20,000 km away (its own grid 2). A then warps to
    // B, so A crosses into B's grid during `Ballpark::step`.
    let mut park = Ballpark::new(SYSTEM_ID);
    park.add_ball(ship(SHIP_A, DVec3::ZERO, MovementMode::Stop));
    park.set_ball_slim_meta(SHIP_A, pilot_meta(90_000_001, 1_000_170));
    let dest = DVec3::new(20_000_000.0, 0.0, 0.0);
    park.add_ball(ship(SHIP_B, dest + DVec3::new(0.0, 5_000.0, 0.0), MovementMode::Stop));
    park.set_ball_slim_meta(SHIP_B, pilot_meta(90_000_002, 1_000_171));
    assert_ne!(park.bubble_id_for_ball(SHIP_A), park.bubble_id_for_ball(SHIP_B));
    park.set_mode(SHIP_A, MovementMode::new_warp(DVec3::ZERO, dest, 3.0)).expect("warp");
    park.drain_all_observer_events();

    let (mut a_add, mut b_add2) = (None, None);
    for _ in 0..40 {
        park.step(1.0);
        let tick = park.current_tick as i64;
        for e in wire_for(&mut park, SHIP_A) {
            if split(&e).1 == "AddBalls" {
                a_add = Some((tick, e));
            }
        }
        // Keep the simulation's own event so the byte check compares against the ball state the
        // simulation queued (the ball can leave warp later in the same tick).
        let b_events = park.drain_observer_events(SHIP_B);
        for ev in &b_events {
            if let DestinyEvent::AddBalls2 { balls } = ev {
                let wire = encode_destiny_updates(tick as u32, std::slice::from_ref(ev), Some(&park));
                let PyRep::List(mut v) = wire else { panic!() };
                b_add2 = Some((tick, balls[0].clone(), v.remove(0)));
            }
        }
        if a_add.is_some() && b_add2.is_some() {
            break;
        }
    }
    assert_eq!(park.bubble_id_for_ball(SHIP_A), park.bubble_id_for_ball(SHIP_B));

    // --- AddBalls to the ball that entered the grid ---------------------------------------
    // Client `Park.AddBalls(self, chunk)`: `state, slims, damageDict = chunk` (michelle.pyj:1645-1671);
    // Crucible SystemBubble::SendAddBalls builds the same 3-tuple (SystemBubble.cpp:697-745).
    let (tick, entry) = a_add.expect("A must receive AddBalls when it enters B's grid");
    let (stamp, _, args) = split(&entry);
    assert_eq!(stamp, tick);
    let PyRep::Tuple(a) = args else { panic!() };
    assert_eq!(a.len(), 1);
    let PyRep::Tuple(chunk) = &a[0] else { panic!("chunk tuple") };
    assert_eq!(chunk.len(), 3, "(state, slims, damageDict)");
    let PyRep::List(slims) = &chunk[1] else { panic!("slims list") };
    assert_eq!(slims.len(), 1);
    let PyRep::Object { type_name, args: slim_state } = &slims[0] else { panic!() };
    assert_eq!(type_name, "foo.SlimItem");
    assert!(matches!(slim_state.as_ref(), PyRep::Dict(_)));
    assert_eq!(slims[0].dict_get("itemID"), Some(&PyRep::Integer(SHIP_B)));
    assert_eq!(slims[0].dict_get("charID"), Some(&PyRep::Integer(90_000_002)));
    assert_eq!(chunk[2], PyRep::Dict(vec![(PyRep::Integer(SHIP_B), damage_tuple(1.0, 120_000.0, tick, 1.0, 1.0))]));
    let PyRep::Bytes(state) = &chunk[0] else { panic!("state bytes") };
    let mut c = Cur(state, 0);
    assert_eq!(c.u8(), 1, "AddBall_header.packet_type 1 = balls (SystemBubble.cpp:713)");
    assert_eq!(c.u32() as i64, tick);
    let b_ball = park.balls[&SHIP_B].clone();
    assert_free_ball_prefix(&mut c, &b_ball, DST_BALL_STOP, 1_000_171, 0);
    assert_eq!(c.u8(), 0xFF);
    assert_eq!(c.1, state.len());

    // --- AddBalls2 to the ball already resident in that grid ----------------------------------
    // Client `Park.AddBalls2(self, chunk)`: `state, extraBallData = chunk`, each entry a
    // `(slimItemDict, damageState)` tuple whose dict becomes `SlimItem().__dict__`
    // (michelle.pyj:1674-1700); Crucible SendAddBalls2 (SystemBubble.cpp:747-805).
    let (tick, a_ball, entry) = b_add2.expect("B must receive AddBalls2 for A");
    let (stamp, _, args) = split(&entry);
    assert_eq!(stamp, tick);
    let PyRep::Tuple(a) = args else { panic!() };
    let PyRep::Tuple(chunk) = &a[0] else { panic!() };
    assert_eq!(chunk.len(), 2, "(state, extraBallData)");
    let PyRep::List(extra) = &chunk[1] else { panic!() };
    let PyRep::Tuple(pair) = &extra[0] else { panic!("(slimDict, damageState)") };
    let PyRep::Dict(slim) = &pair[0] else { panic!("AddBalls2 slim must be a plain dict") };
    assert!(slim.contains(&(PyRep::String("itemID".into()), PyRep::Integer(SHIP_A))));
    assert!(slim.contains(&(PyRep::String("corpID".into()), PyRep::Integer(1_000_170))));
    assert_eq!(pair[1], damage_tuple(1.0, 120_000.0, tick, 1.0, 1.0));
    let PyRep::Bytes(state) = &chunk[0] else { panic!() };
    let mut c = Cur(state, 0);
    assert_eq!(c.u8(), 1);
    assert_eq!(c.u32() as i64, tick);
    let MovementMode::Warp { destination, warp_speed_au_s, .. } = a_ball.mode else {
        panic!("A crosses the grid wall while still in warp")
    };
    assert_free_ball_prefix(&mut c, &a_ball, eve_destiny::wire::DST_BALL_WARP, 1_000_170, 0);
    // WARP_Struct (DestinyStructs.h:124-133); the two int64 constants are the values Crucible logs
    // for a warp in progress (:130-131): bit patterns of -1.0 and 15000.0.
    assert_eq!(c.u8(), 0xFF);
    assert_eq!([c.f64(), c.f64(), c.f64()], destination.to_array());
    assert_eq!(c.i32() as i64, tick, "WARP_Struct.effectStamp");
    assert_eq!(c.i64(), -4_616_189_618_054_758_400);
    assert_eq!(c.i64(), 4_669_471_951_536_783_360);
    assert_eq!(c.i32(), (warp_speed_au_s * 10.0) as i32, "speed = AU/s * 10");
    assert_eq!(c.1, state.len());
}

#[tokio::test]
async fn test_warp_to_and_special_fx_on_warp_start_and_exit() {
    // parity: destiny:WarpTo, destiny:OnSpecialFX
    eve_testkit::attest(&["destiny:WarpTo", "destiny:OnSpecialFX"]);
    let mut park = two_ship_park();
    let dest = DVec3::new(3.0e9, -2.0e8, 5.0e7);
    let origin = park.balls[&SHIP_A].position;
    park.set_mode(SHIP_A, MovementMode::new_warp(origin, dest, 5.0)).expect("warp");
    assert!(matches!(park.balls[&SHIP_A].mode, MovementMode::Warp { .. }));

    // Crucible DestinyManager::WarpTo sends CmdWarpTo + OnSpecialFX('effects.Warping', start, active)
    // (DestinyManager.cpp:2045-2062). CmdWarpTo = (entityID, x, y, z, int distance, int warpSpeed*10)
    // (Destiny.xmlp:469-489). Both the warper and the grid neighbour get the same two actions.
    for obs in [SHIP_A, SHIP_B] {
        let actions = wire_for(&mut park, obs);
        assert_eq!(names(&actions), vec!["WarpTo", "OnSpecialFX"], "observer {obs}");
        let warp = args_of(&actions, "WarpTo")[0];
        assert_eq!(warp, &[
            PyRep::Integer(SHIP_A),
            PyRep::Real(dest.x),
            PyRep::Real(dest.y),
            PyRep::Real(dest.z),
            PyRep::Integer(0),
            PyRep::Integer(50),
        ]);
        // Client `Park.OnSpecialFX(shipID, moduleID, moduleTypeID, targetID, otherTypeID, guid,
        // isOffensive, start, active, duration=-1, repeat=None, startTime=None, timeFromStart=0,
        // graphicInfo=None)` (michelle.pyj:2087; defaults from the class body bytecode).
        let fx = args_of(&actions, "OnSpecialFX")[0];
        assert_eq!(fx.len(), 14);
        assert_eq!(fx, &[
            PyRep::Integer(SHIP_A),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::String("effects.Warping".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(0),
            PyRep::None,
        ]);
    }

    // Warp exit: the simulation drops the ball to Stop and turns the tunnel effect off
    // (start = active = 0). OnDeactivatingWarp is triaged client-local and must not be on the wire.
    let mut exit = None;
    for _ in 0..400 {
        park.step(1.0);
        let actions = wire_for(&mut park, SHIP_A);
        if names(&actions).iter().any(|n| n == "Stop") {
            exit = Some(actions);
            break;
        }
    }
    let actions = exit.expect("warp must finish within 400 ticks");
    assert!(matches!(park.balls[&SHIP_A].mode, MovementMode::Stop));
    assert!((park.balls[&SHIP_A].position - dest).length() < 1.0e6, "landed near destination");
    assert!(!names(&actions).iter().any(|n| n == "OnDeactivatingWarp" || n == "OnActivatingWarp"));
    let fx = args_of(&actions, "OnSpecialFX");
    assert_eq!(fx.len(), 1);
    assert_eq!(fx[0][5], PyRep::String("effects.Warping".into()));
    assert_eq!((&fx[0][7], &fx[0][8]), (&PyRep::Integer(0), &PyRep::Integer(0)));
}

#[tokio::test]
async fn test_cloak_ball_on_cloak_and_decloak() {
    // parity: destiny:CloakBall
    eve_testkit::attest(&["destiny:CloakBall"]);
    let mut park = two_ship_park();
    park.activate_cloak(SHIP_A, false, 30.0, 900.0).expect("cloak: 5 km clear of B");
    assert!(park.combat_states[&SHIP_A].is_cloaked);
    // Crucible SetBallCloak = ("CloakBall", (entityID, bool cloaked)) (Destiny.xmlp:670-681). The
    // client forwards it to its C++ ballpark and drops the ball itself for non-ego observers
    // (michelle.pyj:1584-1587); the server also removes it from the neighbour's view.
    let a_actions = wire_for(&mut park, SHIP_A);
    assert_eq!(names(&a_actions), vec!["CloakBall"]);
    assert_eq!(args_of(&a_actions, "CloakBall")[0], &[PyRep::Integer(SHIP_A), PyRep::Bool(true)]);
    let b_actions = wire_for(&mut park, SHIP_B);
    assert_eq!(names(&b_actions), vec!["CloakBall", "RemoveBalls"]);
    assert_eq!(args_of(&b_actions, "CloakBall")[0], &[PyRep::Integer(SHIP_A), PyRep::Bool(true)]);

    assert!(park.deactivate_cloak(SHIP_A));
    assert!(!park.combat_states[&SHIP_A].is_cloaked);
    let a_actions = wire_for(&mut park, SHIP_A);
    assert_eq!(args_of(&a_actions, "CloakBall")[0], &[PyRep::Integer(SHIP_A), PyRep::Bool(false)]);
    let b_actions = wire_for(&mut park, SHIP_B);
    assert_eq!(names(&b_actions), vec!["CloakBall", "AddBalls2"]);
    assert_eq!(args_of(&b_actions, "CloakBall")[0], &[PyRep::Integer(SHIP_A), PyRep::Bool(false)]);
    // A second decloak is not a state change and emits nothing.
    assert!(!park.deactivate_cloak(SHIP_A));
    assert!(wire_for(&mut park, SHIP_B).is_empty());
}

#[tokio::test]
async fn test_set_ball_radius_and_goto_point_commands() {
    // parity: destiny:SetBallRadius, destiny:GotoPoint
    eve_testkit::attest(&["destiny:SetBallRadius", "destiny:GotoPoint"]);
    let mut park = two_ship_park();

    // Crucible DestinyManager::SetRadius(update) -> SetBallRadius(entityID, real radius)
    // (DestinyManager.cpp:2455-2468, Destiny.xmlp:603-613).
    assert!(park.set_ball_radius(SHIP_A, 250.0));
    assert_eq!(park.balls[&SHIP_A].radius, 250.0);
    for obs in [SHIP_A, SHIP_B] {
        let actions = wire_for(&mut park, obs);
        assert_eq!(names(&actions), vec!["SetBallRadius"]);
        assert_eq!(args_of(&actions, "SetBallRadius")[0], &[PyRep::Integer(SHIP_A), PyRep::Real(250.0)]);
    }

    // Crucible DestinyManager::GotoPoint -> CmdGotoPoint(entityID, x, y, z) (DestinyManager.cpp:1994-2004,
    // Destiny.xmlp:516-528), and the ball starts moving toward the point.
    let a_pos = park.balls[&SHIP_A].position;
    let point = a_pos + DVec3::new(0.0, 30_000.0, 40_000.0);
    park.goto_point(SHIP_A, point).expect("goto point");
    let MovementMode::GotoDirection { direction } = park.balls[&SHIP_A].mode else { panic!() };
    assert!((direction - DVec3::new(0.0, 0.6, 0.8)).length() < 1e-12);
    for obs in [SHIP_A, SHIP_B] {
        let actions = wire_for(&mut park, obs);
        assert_eq!(names(&actions), vec!["GotoPoint"]);
        assert_eq!(args_of(&actions, "GotoPoint")[0], &[
            PyRep::Integer(SHIP_A),
            PyRep::Real(point.x),
            PyRep::Real(point.y),
            PyRep::Real(point.z),
        ]);
    }
    park.step(1.0);
    let moved = park.balls[&SHIP_A].position - a_pos;
    assert!(moved.y > 0.0 && moved.z > 0.0 && moved.x.abs() < 1e-9, "heading toward point: {moved:?}");
}

#[tokio::test]
async fn test_damage_state_change_and_destruction_effect() {
    // parity: destiny:OnDamageStateChange, destiny:TerminalPlayDestructionEffect
    eve_testkit::attest(&["destiny:OnDamageStateChange", "destiny:TerminalPlayDestructionEffect"]);
    let mut park = two_ship_park();
    let tick = park.current_tick as i64;

    // 90 EM on 450 shield -> ratio 0.8. Client `Park.OnDamageStateChange(shipID, damageState)`
    // (michelle.pyj:2070); damageState = ((shield, rechargeMs, stamp), armor, structure)
    // (Destiny.xmlp:289-317; recharge is the shield recharge time, SystemEntity.cpp:615).
    park.apply_damage_to_ball(SHIP_B, DamageVector::new(90.0, 0.0, 0.0, 0.0));
    let actions = wire_for(&mut park, SHIP_A);
    let dmg = args_of(&actions, "OnDamageStateChange");
    assert_eq!(dmg.len(), 1);
    assert_eq!(dmg[0], &[PyRep::Integer(SHIP_B), damage_tuple(0.8, 120_000.0, tick, 1.0, 1.0)]);

    // Lethal damage: neighbours get the final damage state, the destruction effect, then the
    // removal. Client `Park.TerminalPlayDestructionEffect(shipID, destructionEffectId)`
    // (michelle.pyj:1596); `None` selects destructionEffectType.EXPLOSION (michelle.pyj:1609), and
    // `RemoveBall(ballID, terminal=False)` follows (michelle.pyj:1762).
    let (_, destroyed, _) = park
        .apply_damage_to_ball(SHIP_B, DamageVector::new(5_000.0, 0.0, 0.0, 0.0))
        .expect("ball exists");
    assert!(destroyed);
    assert!(!park.balls.contains_key(&SHIP_B));
    let actions = wire_for(&mut park, SHIP_A);
    let order: Vec<String> = names(&actions).into_iter().filter(|n| n != "OnFleetDamageStateChange").collect();
    assert_eq!(order, vec!["OnDamageStateChange", "TerminalPlayDestructionEffect", "RemoveBall"]);
    assert_eq!(args_of(&actions, "OnDamageStateChange")[0], &[
        PyRep::Integer(SHIP_B),
        damage_tuple(0.0, 120_000.0, tick, 0.0, 0.0),
    ]);
    assert_eq!(args_of(&actions, "TerminalPlayDestructionEffect")[0], &[PyRep::Integer(SHIP_B), PyRep::None]);
    assert_eq!(args_of(&actions, "RemoveBall")[0], &[PyRep::Integer(SHIP_B)]);
}

#[tokio::test]
async fn test_slim_item_change_broadcast() {
    // parity: destiny:OnSlimItemChange
    eve_testkit::attest(&["destiny:OnSlimItemChange"]);
    let mut park = two_ship_park();
    // Pilot A changes corporation: Crucible UpdateNewShip/UpdateOldShip send
    // ("OnSlimItemChange", (itemID, foo.SlimItem)) (DestinyManager.cpp:2725-2770); client
    // `Park.OnSlimItemChange(itemID, newSlim)` stores newSlim in slimItems (michelle.pyj:2006-2009).
    let mut meta = pilot_meta(90_000_001, 98_000_001);
    meta.security_status = 2.5;
    assert!(park.change_slim_item(SHIP_A, meta.clone()));
    assert_eq!(park.ball_slim_meta(SHIP_A), Some(&meta));
    for obs in [SHIP_A, SHIP_B] {
        let actions = wire_for(&mut park, obs);
        assert_eq!(names(&actions), vec!["OnSlimItemChange"]);
        let args = args_of(&actions, "OnSlimItemChange")[0];
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], PyRep::Integer(SHIP_A));
        let PyRep::Object { type_name, args: state } = &args[1] else { panic!("SlimItem object") };
        assert_eq!(type_name, "foo.SlimItem");
        assert!(matches!(state.as_ref(), PyRep::Dict(_)));
        assert_eq!(args[1].dict_get("itemID"), Some(&PyRep::Integer(SHIP_A)));
        assert_eq!(args[1].dict_get("corpID"), Some(&PyRep::Integer(98_000_001)));
        assert_eq!(args[1].dict_get("securityStatus"), Some(&PyRep::Real(2.5)));
    }
    // Same slim again: no state change, nothing on the wire.
    assert!(!park.change_slim_item(SHIP_A, meta));
    assert!(wire_for(&mut park, SHIP_B).is_empty());
}
