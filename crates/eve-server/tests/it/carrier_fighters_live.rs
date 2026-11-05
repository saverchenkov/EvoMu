//! Live carrier fighter suite driven only through client RPCs (`fighterMgr`, `dogmaIM`, `beyonce`,
//! `invbroker`) plus GM slash commands for seeding, with real system ticks / clock advances:
//! - Heavy long-range bombs (Ametat I), Space Superiority (Equite I) and tube refuel / rearm / repair.
//! - Support fighters (Scarab I, Siren I, Dromi I, Cenobite I) EWAR scaling with surviving squadron size.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::types::InventoryFlag;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::SystemHandle;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const HIGHSEC: u32 = 30_000_144; // Perimeter (0.9)
const NULLSEC: u32 = 30_000_021; // Kuharah (-1.0)
const FIGHTER_BAY: i64 = InventoryFlag::FighterBay as i64;

const AEON: u32 = 23919;
const ARCHON: u32 = 23757;
const THANATOS: u32 = 23911;
const VEXOR: u32 = 626;
const AMETAT: u32 = 40362;
const EQUITE: u32 = 40358;
const SCARAB: u32 = 40345;
const SIREN: u32 = 40346;
const DROMI: u32 = 40347;
const CENOBITE: u32 = 37599;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn pt(p: DVec3) -> PyRep {
    PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])
}

fn ilist(vals: &[i64]) -> PyRep {
    PyRep::List(vals.iter().copied().map(PyRep::Integer).collect())
}

async fn call(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn ok(ctx: &ServerContext, p: &mut LivePilot, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    call(ctx, p, svc, method, args).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

/// `fighterMgr.CmdActivateAbilitySlots(fighterID, slotID[, target])`.
async fn ability(ctx: &ServerContext, p: &mut LivePilot, ball: i64, slot: i64, target: Option<PyRep>) -> Result<PyRep, BridgeRpcError> {
    let mut args = vec![PyRep::Integer(ball), PyRep::Integer(slot)];
    args.extend(target);
    call(ctx, p, "fighterMgr", "CmdActivateAbilitySlots", &args).await
}

fn drain(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let mut out = p.sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(p.char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

async fn load_and_launch(ctx: &ServerContext, p: &mut LivePilot, tube: i64, item: i64) -> i64 {
    let load = ok(ctx, p, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(tube), PyRep::Integer(item)]).await;
    assert_eq!(load.dict_get("success"), Some(&PyRep::Bool(true)));
    let PyRep::List(ids) = ok(ctx, p, "fighterMgr", "LaunchFightersFromTubes", &[ilist(&[tube])]).await else { panic!("launch list") };
    ids[0].as_i64().expect("fighter ball id")
}

async fn recall_and_unload(ctx: &ServerContext, p: &mut LivePilot, tube: i64) {
    ok(ctx, p, "fighterMgr", "RecallFightersToTubes", &[ilist(&[tube])]).await;
    ctx.advance_secs(25);
    ok(ctx, p, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(tube)]).await;
}

async fn lock(ctx: &ServerContext, sys: &SystemHandle, p: &mut LivePilot, target: i64) {
    let ship = p.ship_id;
    ok(ctx, p, "dogmaIM", "AddTargetOBO", &[PyRep::Integer(ship), PyRep::Integer(target)]).await;
    for _ in 0..60 {
        sys.step_manual(1.0).await.unwrap();
    }
}

async fn shield(sys: &SystemHandle, id: i64) -> f64 {
    sys.get_combat_state(id).await.unwrap().expect("combat state").shield_hp
}

async fn attr(ctx: &ServerContext, p: &mut LivePilot, attr_id: i64) -> f64 {
    let ship = p.ship_id;
    ok(ctx, p, "dogmaIM", "QueryAttributeValue", &[PyRep::Integer(ship), PyRep::Integer(attr_id)]).await.as_f64().expect("attr value")
}

/// `fighterMgr.ExecuteMovementCommandOnFighters([fighter], "FOLLOW", target, 3000)` and tick until within 5 km.
async fn fighters_follow(ctx: &ServerContext, sys: &SystemHandle, p: &mut LivePilot, fighter: i64, target: i64) {
    let args = [ilist(&[fighter]), PyRep::String("FOLLOW".into()), PyRep::Integer(target), PyRep::Real(3_000.0)];
    ok(ctx, p, "fighterMgr", "ExecuteMovementCommandOnFighters", &args).await;
    for _ in 0..120 {
        sys.step_manual(1.0).await.unwrap();
        let f = sys.get_ball_snapshot(fighter).await.unwrap().expect("fighter ball").1.position;
        if f.distance(sys.get_ball_snapshot(target).await.unwrap().expect("target ball").1.position) < 5_000.0 {
            break;
        }
    }
}

fn ewar(res: &PyRep) -> f64 {
    res.dict_get("ewarStrength").and_then(|v| v.as_f64()).expect("ewarStrength")
}

#[tokio::test]
async fn test_live_heavy_bombs_space_superiority_and_tube_rearm() {
    let ctx = paused_ctx();

    // 1. High-sec: the Ametat point bomb is refused (SDE ability 7 disallowInHighSec).
    let mut hs = common::gm_pilot_in_space(&ctx, 1, 91_100_001, AEON, HIGHSEC).await;
    let hs_stack = common::stock_ship_bay(&ctx, &mut hs, AMETAT, 6, FIGHTER_BAY).await;
    let hs_ball = load_and_launch(&ctx, &mut hs, 0, hs_stack).await;
    let hs_pos = ctx.systems.get_or_spawn(HIGHSEC).get_ball_snapshot(hs_ball).await.unwrap().unwrap().1.position;
    let hs_err = ability(&ctx, &mut hs, hs_ball, 2, Some(pt(hs_pos + DVec3::X * 5_000.0))).await.expect_err("high-sec bomb");
    assert_eq!(hs_err.code, "BombForbiddenInHighSec");

    // 2. Null-sec grid: Aeon + Archon at the landing point, Thanatos victim 5 km out, Vexor 40 km out.
    let mut aeon = common::gm_pilot_in_space(&ctx, 2, 91_100_002, AEON, NULLSEC).await;
    let mut archon = common::gm_pilot_in_space(&ctx, 3, 91_100_003, ARCHON, NULLSEC).await;
    let mut victim = common::gm_pilot_in_space(&ctx, 4, 91_100_004, THANATOS, NULLSEC).await;
    let mut far = common::gm_pilot_in_space(&ctx, 5, 91_100_005, VEXOR, NULLSEC).await;
    let sys = ctx.systems.get_or_spawn(NULLSEC);
    let origin = sys.get_ball_snapshot(aeon.ship_id).await.unwrap().unwrap().1.position;
    let v_pos = common::fly_to(&ctx, &mut victim, origin + DVec3::X * 5_000.0).await;
    let f_pos = common::fly_to(&ctx, &mut far, v_pos + DVec3::X * 40_000.0).await;
    let bomb_point = v_pos + DVec3::X * 12_000.0;
    assert!(bomb_point.distance(origin) > 16_000.0 && bomb_point.distance(origin) < 90_000.0, "carriers outside blast: {v_pos:?}");
    assert!(f_pos.distance(bomb_point) > 16_000.0, "far ship outside blast: {f_pos:?}");

    let ametat_stack = common::stock_ship_bay(&ctx, &mut aeon, AMETAT, 12, FIGHTER_BAY).await;
    let ametat = load_and_launch(&ctx, &mut aeon, 0, ametat_stack).await;
    let range_err = ability(&ctx, &mut aeon, ametat, 2, Some(pt(origin + DVec3::X * 150_000.0))).await.expect_err("> 100 km bomb");
    assert_eq!(range_err.code, "TargetOutOfRange");

    let (v0, f0) = (shield(&sys, victim.ship_id).await, shield(&sys, far.ship_id).await);
    let bomb1 = ability(&ctx, &mut aeon, ametat, 2, Some(pt(bomb_point))).await.expect("bomb 1");
    assert_eq!(bomb1.dict_get("remainingCharges"), Some(&PyRep::Integer(2)));
    let b1_dmg = v0 - shield(&sys, victim.ship_id).await;
    assert!((b1_dmg - 3_840.0).abs() < 1.0, "6 Ametat x 640 EM (Micro Electron Bomb SDE) expected, got {b1_dmg}");
    assert!((shield(&sys, far.ship_id).await - f0).abs() < 1e-6, "ship outside the 15 km blast must be untouched");
    let cd_err = ability(&ctx, &mut aeon, ametat, 2, Some(pt(bomb_point))).await.expect_err("bomb cooldown");
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    // 3. Equite I: Evasive Maneuvers (speed/sig from SDE 2224/2225) + cooldown, then anti-fighter missiles.
    let eq_stack = common::stock_ship_bay(&ctx, &mut archon, EQUITE, 12, FIGHTER_BAY).await;
    let equite = load_and_launch(&ctx, &mut archon, 0, eq_stack).await;
    let vel0 = sys.get_ball_snapshot(equite).await.unwrap().unwrap().1.max_velocity;
    let sig0 = sys.get_combat_state(equite).await.unwrap().unwrap().signature_radius;
    ability(&ctx, &mut archon, equite, 1, None).await.expect("Evasive Maneuvers");
    assert!(sys.get_ball_snapshot(equite).await.unwrap().unwrap().1.max_velocity > vel0 * 2.5);
    assert!(sys.get_combat_state(equite).await.unwrap().unwrap().signature_radius < sig0 * 0.5);
    assert_eq!(ability(&ctx, &mut archon, equite, 1, None).await.expect_err("evasive cooldown").code, "AbilityOnCooldown");

    lock(&ctx, &sys, &mut archon, victim.ship_id).await;
    lock(&ctx, &sys, &mut archon, ametat).await;
    fighters_follow(&ctx, &sys, &mut archon, equite, victim.ship_id).await;
    let pre_ship = shield(&sys, victim.ship_id).await;
    ability(&ctx, &mut archon, equite, 2, Some(PyRep::Integer(victim.ship_id))).await.expect("Equite vs ship");
    let ship_dmg = pre_ship - shield(&sys, victim.ship_id).await;
    assert!((ship_dmg - 456.0).abs() < 1.0, "Equite volley vs ship expected 456 EM, got {ship_dmg}");
    ctx.advance_secs(12);
    fighters_follow(&ctx, &sys, &mut archon, equite, ametat).await;

    let _ = drain(&ctx, &mut aeon);
    for i in 0..5 {
        let pre = shield(&sys, ametat).await;
        ability(&ctx, &mut archon, equite, 2, Some(PyRep::Integer(ametat))).await.expect("Equite vs fighters");
        if i == 0 {
            let dmg = pre - shield(&sys, ametat).await;
            assert!((dmg - 912.0).abs() < 1.0, "Space superiority missiles deal 2x (912 EM) vs fighters, got {dmg}");
        }
        ctx.advance_secs(12);
    }
    assert!(
        drain(&ctx, &mut aeon).iter().any(|(ev, a)| ev == "OnInSpaceSquadronSizeChanged" && a.get(1) == Some(&PyRep::Integer(5))),
        "anti-fighter volleys must kill one Ametat and emit OnInSpaceSquadronSizeChanged(5)"
    );

    // 4. Surviving 5-unit squadron: bomb damage scales (5 x 640), last charge, then 0-charge rejection.
    let pre_b2 = shield(&sys, victim.ship_id).await;
    let bomb2 = ability(&ctx, &mut aeon, ametat, 2, Some(pt(bomb_point))).await.expect("bomb 2");
    assert_eq!(bomb2.dict_get("remainingCharges"), Some(&PyRep::Integer(1)));
    let b2_dmg = pre_b2 - shield(&sys, victim.ship_id).await;
    assert!((b2_dmg - 3_200.0).abs() < 1.0, "5-unit Ametat bomb must deal 3200 EM, got {b2_dmg}");
    ctx.advance_secs(61);
    let bomb3 = ability(&ctx, &mut aeon, ametat, 2, Some(pt(bomb_point))).await.expect("bomb 3");
    assert_eq!(bomb3.dict_get("remainingCharges"), Some(&PyRep::Integer(0)));
    ctx.advance_secs(61);
    let empty = ability(&ctx, &mut aeon, ametat, 2, Some(pt(bomb_point))).await.expect_err("no bombs left");
    assert_eq!(empty.code, "OutOfCharges");

    // 5. Recall: refuel + per-charge bomb rearm + unit replacement timer blocks relaunch until it elapses.
    let _ = drain(&ctx, &mut aeon);
    ok(&ctx, &mut aeon, "fighterMgr", "RecallFightersToTubes", &[ilist(&[0])]).await;
    let task_ticks = drain(&ctx, &mut aeon)
        .iter()
        .find(|(ev, _)| ev == "OnFighterTubeTaskStatus")
        .map(|(_, a)| a.get(3).and_then(|v| v.as_i64()).unwrap_or(0) - a.get(2).and_then(|v| v.as_i64()).unwrap_or(0))
        .expect("OnFighterTubeTaskStatus on recall");
    assert!(task_ticks >= 120_000_000, "refuel + 3 bomb reloads + 1 replacement must exceed 12 s, got {task_ticks} ticks");
    ctx.advance_secs(10);
    let rearming = call(&ctx, &mut aeon, "fighterMgr", "LaunchFightersFromTubes", &[ilist(&[0])]).await.expect_err("still rearming");
    assert_eq!(rearming.code, "TubeRearming");

    ctx.advance_secs(15);
    let PyRep::List(re_ids) = ok(&ctx, &mut aeon, "fighterMgr", "LaunchFightersFromTubes", &[ilist(&[0])]).await else { panic!("relaunch") };
    let relaunched = re_ids[0].as_i64().unwrap();
    let aeon_ship = aeon.ship_id;
    let reserve: i32 = ctx.db.with_snapshot(|s| {
        s.items.values()
            .filter(|i| i.location_id == aeon_ship && i.flag == InventoryFlag::FighterBay && i.type_id == AMETAT && i.item_id != ametat_stack)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(reserve, 5, "one replacement Ametat must be drawn from the FighterBay reserve");
    let re_bomb = ability(&ctx, &mut aeon, relaunched, 2, Some(pt(bomb_point))).await.expect("rearmed bomb");
    assert_eq!(re_bomb.dict_get("remainingCharges"), Some(&PyRep::Integer(2)), "rearmed squadron carries 3 fresh bombs");
}

#[tokio::test]
async fn test_live_support_fighter_ewar_scales_with_surviving_squadron() {
    let ctx = paused_ctx();
    let mut aeon = common::gm_pilot_in_space(&ctx, 11, 91_100_011, AEON, NULLSEC).await;
    let mut archon = common::gm_pilot_in_space(&ctx, 12, 91_100_012, ARCHON, NULLSEC).await;
    let mut vexor = common::gm_pilot_in_space(&ctx, 13, 91_100_013, VEXOR, NULLSEC).await;
    let sys = ctx.systems.get_or_spawn(NULLSEC);
    let (archon_ship, vexor_ship) = (archon.ship_id, vexor.ship_id);

    let mut bay = HashMap::new();
    for ty in [SCARAB, SIREN, DROMI, CENOBITE] {
        bay.insert(ty, common::stock_ship_bay(&ctx, &mut archon, ty, 3, FIGHTER_BAY).await);
    }
    let eq_stack = common::stock_ship_bay(&ctx, &mut aeon, EQUITE, 12, FIGHTER_BAY).await;
    lock(&ctx, &sys, &mut vexor, archon_ship).await;
    lock(&ctx, &sys, &mut archon, vexor_ship).await;

    // 1. Full 3-unit Scarab I: 3 x 2.4 = 7.2 jam strength >= Vexor threshold (16 magnetometric x 0.4).
    let scarab = load_and_launch(&ctx, &mut archon, 1, bay[&SCARAB]).await;
    let dromi = load_and_launch(&ctx, &mut archon, 2, bay[&DROMI]).await;
    let jam_full = ability(&ctx, &mut archon, scarab, 0, Some(PyRep::Integer(vexor_ship))).await.expect("Scarab ECM");
    assert!((ewar(&jam_full) - 7.2).abs() < 1e-6);
    assert!(sys.get_target_locks(vexor_ship).await.unwrap().is_empty(), "Scarab jam must break the victim's locks");
    let jammed = call(&ctx, &mut vexor, "dogmaIM", "AddTargetOBO", &[PyRep::Integer(vexor_ship), PyRep::Integer(archon_ship)]).await;
    assert_eq!(jammed.unwrap_err().code, "ShipJammed");

    // 2. Real anti-fighter combat kills one Scarab (8 x 912 > 6800 HP): 2 survivors -> 4.8 < 6.4, no jam.
    lock(&ctx, &sys, &mut aeon, scarab).await;
    let equite = load_and_launch(&ctx, &mut aeon, 0, eq_stack).await;
    let _ = drain(&ctx, &mut archon);
    for _ in 0..8 {
        ability(&ctx, &mut aeon, equite, 2, Some(PyRep::Integer(scarab))).await.expect("Equite vs Scarab");
        ctx.advance_secs(12);
    }
    assert!(drain(&ctx, &mut archon).iter().any(|(ev, a)| ev == "OnInSpaceSquadronSizeChanged" && a.get(1) == Some(&PyRep::Integer(2))));
    lock(&ctx, &sys, &mut vexor, archon_ship).await;
    let jam_reduced = ability(&ctx, &mut archon, scarab, 0, Some(PyRep::Integer(vexor_ship))).await.expect("reduced Scarab ECM");
    assert!((ewar(&jam_reduced) - 4.8).abs() < 1e-6);
    assert_eq!(sys.get_target_locks(vexor_ship).await.unwrap().len(), 1, "2-unit Scarab (4.8 < 6.4) must not jam the Vexor");

    // 3. Dromi I web: 3 x 10% (SDE 2184) applied to Destiny speed and dogma maxVelocity.
    let base_vel = attr(&ctx, &mut vexor, 37).await;
    ctx.dispatch_rpc(&mut vexor.sess, "beyonce", Some(vexor.beyonce), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None)
        .await
        .expect("CmdSetSpeedFraction");
    let web = ability(&ctx, &mut archon, dromi, 0, Some(PyRep::Integer(vexor_ship))).await.expect("Dromi web");
    assert!((ewar(&web) - 0.30).abs() < 1e-6);
    let webbed_vel = attr(&ctx, &mut vexor, 37).await;
    let frac = sys.get_ball_snapshot(vexor_ship).await.unwrap().unwrap().1.speed_fraction;
    assert!((frac - 0.70).abs() < 0.02 && webbed_vel < base_vel * 0.75, "web: fraction {frac}, velocity {base_vel} -> {webbed_vel}");

    // 4. Swap tubes through recall/unload: Siren I scram (3 x 1 point) and Cenobite I neut (3 x 70 GJ, SDE 2211).
    recall_and_unload(&ctx, &mut archon, 1).await;
    let siren = load_and_launch(&ctx, &mut archon, 1, bay[&SIREN]).await;
    let scram = ability(&ctx, &mut archon, siren, 0, Some(PyRep::Integer(vexor_ship))).await.expect("Siren scram");
    assert!((ewar(&scram) - 3.0).abs() < 1e-6);
    assert_eq!(sys.get_combat_state(vexor_ship).await.unwrap().unwrap().warp_scramble_strength, 3);

    recall_and_unload(&ctx, &mut archon, 2).await;
    let ceno = load_and_launch(&ctx, &mut archon, 2, bay[&CENOBITE]).await;
    let cap0 = sys.get_combat_state(vexor_ship).await.unwrap().unwrap().capacitor;
    let neut = ability(&ctx, &mut archon, ceno, 0, Some(PyRep::Integer(vexor_ship))).await.expect("Cenobite neut");
    let cap1 = sys.get_combat_state(vexor_ship).await.unwrap().unwrap().capacitor;
    assert!((ewar(&neut) - 210.0).abs() < 1e-6);
    assert!((cap0 - cap1 - 210.0).abs() < 1.0, "3-unit Cenobite must neut 210 GJ, got {}", cap0 - cap1);
}
