//! Live end-to-end integration tests for Pirate Insurgency complexes:
//! - `feat:pve.insurgency-laundering`
//! - `feat:pve.insurgency-mining-ambush`
//! - `feat:pve.insurgency-outpost-raid`

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_ephemeral;
use eve_server::services::fw_gameplay::{militia_lp_corp_id, tick_insurgency_campaigns};
use eve_server::services::fw_services::{
    compute_all_frontline_statuses, HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR,
    HQ_SYSTEM_ONNAMON, STATUS_FRONTLINE,
};
use eve_server::services::insurgency_complex_ops::{
    is_named_insurgency_roamer, is_named_pirate_commander_roamer, load_ops_state,
    tick_insurgency_complex_ops, OutpostRaidSize, LARGE_ADV_OUTPOST_RESPAWN_SECS,
    MODERATE_OUTPOST_BC_SKILL_DBUFF_ID, TYPE_ANGEL_INSURGENCY_DATA_SEQ,
    TYPE_ANGEL_LAUNDERING_FACILITY, TYPE_FEDERATION_CI_OUTPOST_HUB,
    TYPE_GURISTAS_INSURGENCY_DATA_SEQ, TYPE_GURISTAS_LAUNDERING_FACILITY,
    TYPE_IMPERIAL_CI_OUTPOST_HUB, TYPE_ORE_RORQUAL_WRECK,
    TYPE_REPUBLIC_CI_OUTPOST_HUB, TYPE_STATE_CI_OUTPOST_HUB, TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
};
use eve_server::services::insurgency_services::{
    insurgency_system_corruption_stage, read_insurgency_runtime, step_system_insurgency_tick_delta,
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, FORECASTING_DURATION_SECS,
};
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn rpc_bound(ctx: &ServerContext, sess: &mut SessionState, svc: &str, bound: u64, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, Some(bound), method, args, &HashMap::new(), None).await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    common::slash_cmd(ctx, sess, cmd).await
}

fn sde_type_id(ctx: &ServerContext, name: &str) -> u32 {
    ctx.sde.types.values().find(|t| t.name == name).map(|t| t.type_id).unwrap_or_else(|| panic!("SDE type '{name}' not found"))
}

fn gear_type_ids(ctx: &ServerContext) -> (u32, u32, u32, u32, u32, u32) {
    (sde_type_id(ctx, "Hurricane"), sde_type_id(ctx, "425mm AutoCannon II"), sde_type_id(ctx, "Gyrostabilizer II"), sde_type_id(ctx, "50MN Y-T8 Compact Microwarpdrive"), sde_type_id(ctx, "Republic Fleet EMP M"), sde_type_id(ctx, "Rifter"))
}

fn collect_skill_topo(sde: &eve_sde::SdeRegistry, sid: u32, seen: &mut Vec<u32>, out: &mut Vec<u32>) {
    if seen.contains(&sid) { return; }
    seen.push(sid);
    if let Some(t) = sde.types.get(&sid) {
        for attr in [182_u16, 183, 184, 1285, 1289, 1290] { if let Some(&req) = t.default_attributes.get(&attr) { if req > 0.0 { collect_skill_topo(sde, req as u32, seen, out); } } }
    }
    out.push(sid);
}

async fn train_skills_to_five(ctx: &ServerContext, sess: &mut SessionState, skill_ids: &[u32]) {
    let char_id = sess.character_id.unwrap();
    let (mut seen, mut ordered) = (Vec::new(), Vec::new());
    for &sid in skill_ids { collect_skill_topo(&ctx.sde, sid, &mut seen, &mut ordered); }
    let to_buy: Vec<PyRep> = ordered.iter().copied().filter(|&sid| ctx.db.with_snapshot(|s| !s.skills.get(&char_id).is_some_and(|m| m.contains_key(&sid)))).map(|sid| PyRep::Integer(i64::from(sid))).collect();
    if !to_buy.is_empty() { let _ = rpc(ctx, sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(to_buy)]).await; }
    let mut q = Vec::new();
    for sid in ordered {
        let curr = ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
        for lvl in (curr + 1)..=5 { q.push(PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(lvl))])); }
    }
    if !q.is_empty() {
        rpc(ctx, sess, "skillMgr2", "SaveNewQueue", &[PyRep::List(q)]).await.expect("SaveNewQueue");
        ctx.advance_virtual_clock(3_650 * 86_400);
        let _ = rpc(ctx, sess, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await;
    }
}

async fn spawn_live_pilot(
    ctx: &ServerContext, sid: u64, cid: i64, sys_id: u32, ship_tid: u32, gun_tid: u32, gyro_tid: u32, mwd_tid: u32, ammo_tid: u32,
) -> (LivePilot, Vec<i64>) {
    let (user, pass) = (format!("insurg_p_{cid}"), format!("pw_{cid}"));
    ctx.db.provision_gm_account(&user, &pass, cid, &format!("Insurg Pilot {cid}"));
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("SelectCharacterID");
    let mut reqs = vec![3426_u32, 3413, 3318, 11207, 3429, 3453, 3450, 3455, 3417, 3418];
    for tid in [ship_tid, gun_tid, gyro_tid, mwd_tid, ammo_tid] {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for attr in [182_u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&r) = t.default_attributes.get(&attr) { if r > 0.0 { reqs.push(r as u32); } }
            }
        }
    }
    train_skills_to_five(ctx, &mut sess, &reqs).await;
    let ship_id = slash(ctx, &mut sess, &format!("/createitem {ship_tid} 1")).await.dict_get("itemID").and_then(PyRep::as_i64).expect("ship itemID");
    rpc(ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Board");
    rpc(ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    slash(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let PyRep::Tuple(parts) = rpc(ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None]).await.expect("beyonce bind") else { panic!() };
    let mut pilot = LivePilot { sess, char_id: cid, ship_id, beyonce: parts[0].as_i64().unwrap() as u64 };
    let mut guns = Vec::new();
    if gun_tid > 0 {
        for _ in 0..6 { slash(ctx, &mut pilot.sess, &format!("/fit me {gun_tid}")).await; }
        for _ in 0..3 { slash(ctx, &mut pilot.sess, &format!("/fit me {gyro_tid}")).await; }
        slash(ctx, &mut pilot.sess, &format!("/fit me {mwd_tid}")).await;
        let mods: Vec<i64> = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == ship_id && (i.type_id == gun_tid || i.type_id == gyro_tid || i.type_id == mwd_tid)).map(|i| i.item_id).collect());
        for mid in mods { let _ = rpc(ctx, &mut pilot.sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(mid)]).await; }
        guns = ctx.db.with_snapshot(|s| { let mut g: Vec<i64> = s.items.values().filter(|i| i.location_id == ship_id && i.type_id == gun_tid).map(|i| i.item_id).collect(); g.sort_unstable(); g });
        let ammo_id = common::stock_ship_bay(ctx, &mut pilot, ammo_tid, 10_000, InventoryFlag::Cargo as i64).await;
        rpc(ctx, &mut pilot.sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(guns.iter().map(|&g| PyRep::Integer(g)).collect()), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await.expect("LoadAmmo");
        for &gid in &guns { slash(ctx, &mut pilot.sess, &format!("/setattr {gid} 64 5000.0")).await; }
        ctx.advance_secs(11);
    }
    let _ = rpc_bound(ctx, &mut pilot.sess, "beyonce", pilot.beyonce, "CmdStop", &[]).await;
    slash(ctx, &mut pilot.sess, "/heal").await;
    (pilot, guns)
}

async fn enlist(ctx: &ServerContext, p: &mut LivePilot, war_fac: i64) { rpc(ctx, &mut p.sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(war_fac)]).await.expect("enlist"); }

async fn travel_to(ctx: &ServerContext, p: &mut LivePilot, target_sys: u32) {
    let cur = p.sess.solar_system_id.unwrap_or(0);
    if cur != target_sys {
        if let Some(gate_id) = ctx.sde.jumps.values().find_map(|j| (j.from_solar_system_id == cur && j.to_solar_system_id == target_sys).then_some(j.stargate_id)) {
            warp_to_item(ctx, p, gate_id, 0.0).await.expect("warp gate"); rpc(ctx, &mut p.sess, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id)]).await.expect("CmdStargateJump");
        } else { slash(ctx, &mut p.sess, &format!("/tr {target_sys}")).await; }
        slash(ctx, &mut p.sess, "/heal").await;
    }
    if let PyRep::Tuple(parts) = rpc(ctx, &mut p.sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(target_sys)), PyRep::None]).await.expect("rebind beyonce") { p.beyonce = parts[0].as_i64().unwrap() as u64; }
}

async fn warp_to_target(ctx: &ServerContext, p: &mut LivePilot, stuff: &str, item_id: i64, min_range: f64) -> Result<(), BridgeRpcError> {
    let sys = ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space"));
    for hop in 0..8 {
        slash(ctx, &mut p.sess, "/heal").await;
        match rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdWarpToStuff", &[PyRep::String(stuff.into()), PyRep::Integer(item_id), PyRep::Real(min_range)]).await {
            Err(e) if e.code == "WarpDistanceTooShort" && hop > 0 => {
                let sdist = sys.get_ball_snapshot(p.ship_id).await.ok().flatten().zip(sys.get_ball_snapshot(item_id).await.ok().flatten()).map_or(0.0, |((_, a), (_, b))| a.position.distance(b.position));
                if stuff == "item" && (item_id >= 1_893_100_000 || ctx.db.with_snapshot(|s| s.items.get(&item_id).is_some_and(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 366)))) && sdist > min_range + 2_400.0 {
                    return Err(BridgeRpcError { code: "WarpDistanceTooShort".into(), message: format!("short hop {sdist:.1} m") });
                }
                break;
            }
            Err(e) if e.code == "NotEnoughCapacitor" && hop < 7 => { sys.step_manual(20.0).await.unwrap(); continue; }
            res => res?,
        };
        for _ in 0..20 {
            let aligning = sys.get_ball_snapshot(p.ship_id).await.ok().flatten().is_some_and(|(_, b)| matches!(b.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }));
            sys.step_manual(if aligning { 4.0 } else { 20.0 }).await.unwrap();
            if sys.get_ball_snapshot(p.ship_id).await.ok().flatten().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) { break; }
        }
        let _ = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdStop", &[]).await;
        if sys.get_combat_state(p.ship_id).await.ok().flatten().is_some_and(|c| c.capacitor > 1.0) { break; }
    }
    slash(ctx, &mut p.sess, "/heal").await;
    Ok(())
}

async fn warp_to_item(ctx: &ServerContext, p: &mut LivePilot, item_id: i64, min_range: f64) -> Result<(), BridgeRpcError> {
    match warp_to_target(ctx, p, "item", item_id, min_range).await {
        Err(e) if e.code == "WarpDistanceTooShort" => {
            let (sys_id, sys) = (p.sess.solar_system_id.expect("in space"), ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space")));
            let Some((_, b)) = sys.get_ball_snapshot(item_id).await.ok().flatten() else { return Err(e); };
            let sp = sys.get_ball_snapshot(p.ship_id).await.ok().flatten().map(|(_, s)| s.position).unwrap_or(b.position);
            if sp.distance(b.position) > 2_400.0 {
                if let Some(cid) = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| sp.distance(c.position) >= 165_000.0 && b.position.distance(c.position) >= 165_000.0).map(|c| c.celestial_id) {
                    let _ = warp_to_target(ctx, p, "item", cid, 0.0).await;
                    return warp_to_target(ctx, p, "item", item_id, min_range).await;
                }
            }
            fly_near_point(ctx, p, b.position, min_range.max(1_500.0), 12.0).await;
            Ok(())
        }
        res => res,
    }
}

async fn fly_near_point(ctx: &ServerContext, p: &mut LivePilot, dest: DVec3, stop_dist: f64, _max_step: f64) {
    let sys = ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space"));
    let mut best = sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position.distance(dest);
    if best <= stop_dist || best > 500_000.0 { return; }
    for step in 0..160 {
        if step % 6 == 0 { rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdGotoPoint", &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)]).await.expect("CmdGotoPoint"); }
        sys.step_manual(((best - stop_dist * 0.5).max(300.0) / 300.0).clamp(1.0, 12.0)).await.unwrap();
        let d = sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position.distance(dest);
        if d <= stop_dist || d >= best { break; }
        best = d;
    }
    let _ = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdStop", &[]).await;
}

async fn approach_npc(ctx: &ServerContext, p: &mut LivePilot, npc_id: i64) {
    let (sys_id, sys) = (p.sess.solar_system_id.expect("in space"), ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space")));
    for _ in 0..8 {
        for _ in 0..30 { let Some((_, nb)) = sys.get_ball_snapshot(npc_id).await.unwrap() else { return; }; if !matches!(nb.mode, MovementMode::Warp { .. }) { break; } sys.step_manual(5.0).await.unwrap(); }
        let Some((_, nb)) = sys.get_ball_snapshot(npc_id).await.unwrap() else { return; };
        let (pos, ship_pos) = (nb.position, sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball").1.position);
        let (dist, spd) = (ship_pos.distance(pos), nb.velocity.length());
        if dist <= 25_000.0 { return; }
        let dir = if spd > 20.0 { nb.velocity / spd } else { DVec3::ZERO };
        let ahead = (ship_pos - pos).dot(dir);
        if spd > 50.0 && ahead < 260_000.0 {
            if let Some(cid) = ctx.sde.celestials_in_system(sys_id).into_iter().filter(|c| ship_pos.distance(c.position) >= 165_000.0 && (c.position - pos).dot(dir) > 260_000.0).min_by_key(|c| { let d = c.position.distance(pos); let cos = (c.position - pos).dot(dir) / d.max(1.0); if cos >= 0.75 { d as i64 } else { (1e17 - cos * 1e12) as i64 } }).map(|c| c.celestial_id) {
                let _ = warp_to_item(ctx, p, cid, 0.0).await; continue;
            }
        }
        if dist >= 165_000.0 {
            let _ = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdStop", &[]).await;
            let PyRep::Tuple(bm) = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "BookmarkLocation", &[PyRep::Integer(npc_id), PyRep::Integer(0), PyRep::String("target".into()), PyRep::String(String::new())]).await.expect("BookmarkLocation") else { panic!() };
            let bm_id = bm[0].as_i64().unwrap();
            let lead = if spd > 50.0 && ahead >= 200_000.0 { (spd * 26.0).min(dist - 160_000.0).clamp(5_000.0, 100_000.0) } else { 5_000.0 };
            if warp_to_target(ctx, p, "bookmark", bm_id, lead).await.is_err() { warp_to_target(ctx, p, "bookmark", bm_id, 50_000.0).await.unwrap(); }
        } else if spd > 50.0 && ahead > 12_000.0 { sys.step_manual(((ahead - 12_000.0) / spd).clamp(1.0, 10.0)).await.unwrap(); }
        else { fly_near_point(ctx, p, pos, 12_000.0, 12.0).await; }
    }
    let (_, nb) = sys.get_ball_snapshot(npc_id).await.unwrap().expect("npc ball");
    let (_, sb) = sys.get_ball_snapshot(p.ship_id).await.unwrap().expect("ship ball");
    panic!("approach_npc {npc_id} stuck at {:.1} m: ship={:?} npc={:?} nb_mode={:?} vel={:?}", sb.position.distance(nb.position), sb.position, nb.position, nb.mode, nb.velocity);
}

async fn kill_npc_live(ctx: &ServerContext, p: &mut LivePilot, guns: &[i64], target_id: i64) -> Option<i64> {
    assert!(!guns.is_empty(), "ship {} has no fitted guns", p.ship_id);
    let (sys_id, sys) = (p.sess.solar_system_id.expect("in space"), ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space")));
    for attempt in 0..10 {
        if sys.get_ball_snapshot(p.ship_id).await.ok().flatten()?.1.position.distance(sys.get_ball_snapshot(target_id).await.ok().flatten()?.1.position) > 25_000.0 { approach_npc(ctx, p, target_id).await; let _ = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdFollowBall", &[PyRep::Integer(target_id), PyRep::Real(5_000.0)]).await; }
        match rpc(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await { Ok(_) => break, Err(e) if e.code == "TargetingFailed" && attempt < 9 => { sys.step_manual(2.0).await.unwrap(); } Err(e) => panic!("AddTarget: {e:?}"), }
    }
    for _ in 0..8 {
        sys.step_manual(2.0).await.unwrap();
        if sys.get_target_locks(p.ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) { break; }
    }
    let (mut need_activate, mut cur_last) = (true, sys.get_ball_snapshot(target_id).await.ok().flatten()?.1.position);
    for step in 0..360 {
        let Some((_, tb)) = sys.get_ball_snapshot(target_id).await.unwrap() else { eve_server::services::combat_npc_encounters::finalize_cycle_destroyed_npcs(ctx, sys_id).await; break; };
        cur_last = tb.position;
        if step % 12 == 0 {
            slash(ctx, &mut p.sess, "/heal").await;
            if sys.get_ball_snapshot(p.ship_id).await.unwrap().is_some_and(|(_, sb)| sb.position.distance(tb.position) > 25_000.0) { approach_npc(ctx, p, target_id).await; need_activate = true; let _ = rpc(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await; }
            let _ = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CmdFollowBall", &[PyRep::Integer(target_id), PyRep::Real(5_000.0)]).await;
        }
        if need_activate {
            need_activate = false;
            for &gid in guns {
                if sys.get_ball_snapshot(target_id).await.unwrap().is_none() { break; }
                let eff = if ctx.db.with_snapshot(|s| s.items.get(&gid).and_then(|i| ctx.sde.types.get(&i.type_id)).is_some_and(|t| matches!(t.group_id, 506..=511 | 1674 | 4807))) { "useMissiles" } else { "projectileFired" };
                if let Err(e) = rpc(ctx, &mut p.sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String(eff.into()), PyRep::Integer(target_id)]).await {
                    if e.code == "TargetNotLocked" { let _ = rpc(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await; need_activate = true; }
                    else if e.code != "ModuleAlreadyActive" { panic!("Activate gun {gid} failed: {e:?}"); }
                }
            }
        }
        sys.step_manual(2.0).await.unwrap();
        if step == 0 { sys.step_manual(2.0).await.unwrap(); }
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() { eve_server::services::combat_npc_encounters::finalize_cycle_destroyed_npcs(ctx, sys_id).await; break; }
    }
    ctx.advance_secs(2);
    assert!(sys.get_ball_snapshot(target_id).await.unwrap().is_none(), "NPC {target_id} must die from live fire");
    ctx.db.with_snapshot(|s| {
        s.items.iter().filter(|(_, it)| it.location_id == i64::from(sys_id) && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 186))
            .min_by(|(a, _), (b, _)| s.item_positions.get(a).map_or(f64::MAX, |p| DVec3::from_array(*p).distance(cur_last)).total_cmp(&s.item_positions.get(b).map_or(f64::MAX, |p| DVec3::from_array(*p).distance(cur_last))))
            .map(|(id, _)| *id)
    })
}

async fn loot_wreck_via_invbroker(ctx: &ServerContext, p: &mut LivePilot, wreck_id: i64) -> Vec<u32> {
    let (sys_id, sys) = (p.sess.solar_system_id.expect("in space"), ctx.systems.get_or_spawn(p.sess.solar_system_id.expect("in space")));
    if let Some(wpos) = ctx.db.with_snapshot(|s| s.item_positions.get(&wreck_id).copied().map(DVec3::from_array)) {
        let spos = sys.get_ball_snapshot(p.ship_id).await.ok().flatten().map(|(_, s)| s.position).unwrap_or(wpos);
        if spos.distance(wpos) > 25_000.0 {
            if spos.distance(wpos) < 165_000.0 { if let Some(cid) = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| spos.distance(c.position) >= 165_000.0 && wpos.distance(c.position) >= 165_000.0).map(|c| c.celestial_id) { let _ = warp_to_item(ctx, p, cid, 0.0).await; } }
            let PyRep::Tuple(bm) = rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "BookmarkLocation", &[PyRep::Integer(wreck_id), PyRep::Integer(0), PyRep::String("wreck".into()), PyRep::String(String::new())]).await.expect("BookmarkLocation wreck") else { panic!() };
            let _ = warp_to_target(ctx, p, "bookmark", bm[0].as_i64().unwrap(), 0.0).await;
        }
        fly_near_point(ctx, p, wpos, 1_500.0, 12.0).await;
    }
    let PyRep::Tuple(bparts) = rpc(ctx, &mut p.sess, "invbroker", "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::Integer(0)]).await.expect("bind invbroker") else { panic!() };
    let sys_inv = bparts[0].as_i64().unwrap() as u64;
    let wreck_inv = rpc_bound(ctx, &mut p.sess, "invbroker", sys_inv, "GetInventoryFromId", &[PyRep::Integer(wreck_id), PyRep::Integer(0)]).await.expect("GetInventoryFromId wreck").as_i64().unwrap() as u64;
    let list_rep = rpc_bound(ctx, &mut p.sess, "invbroker", wreck_inv, "List", &[PyRep::Integer(0)]).await.expect("List wreck");
    let ship_inv = rpc_bound(ctx, &mut p.sess, "invbroker", sys_inv, "GetInventoryFromId", &[PyRep::Integer(p.ship_id), PyRep::Integer(0)]).await.expect("GetInventoryFromId ship").as_i64().unwrap() as u64;
    let mut looted = Vec::new();
    for entry in list_rep.crowset_rows().unwrap_or(&[]) {
        let (iid, tid) = (entry.dbrow_get("itemID").or_else(|| entry.dict_get("itemID")).and_then(PyRep::as_i64).unwrap_or(0), entry.dbrow_get("typeID").or_else(|| entry.dict_get("typeID")).and_then(PyRep::as_i64).unwrap_or(0) as u32);
        if iid > 0 && tid > 0 { rpc_bound(ctx, &mut p.sess, "invbroker", ship_inv, "Add", &[PyRep::Integer(iid), PyRep::Integer(wreck_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("invbroker Add"); looted.push(tid); }
    }
    looted
}

async fn hold_fw_complex_ticks(ctx: &ServerContext, p: &mut LivePilot, sys_id: u32, ticks: u32) {
    travel_to(ctx, p, sys_id).await;
    let PyRep::List(insts) = rpc(ctx, &mut p.sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[PyRep::Integer(i64::from(sys_id))]).await.expect("GetPirateInsurgencyInstances") else { panic!() };
    let inst_id = insts[0].dict_get("instanceID").and_then(PyRep::as_i64).expect("instanceID");
    if let Err(e) = warp_to_target(ctx, p, "dungeon", inst_id, 0.0).await {
        let bpos = ctx.db.with_snapshot(|s| DVec3::from_array(s.system_states[&sys_id].fw_complex_position.expect("fw_complex_position")));
        if e.code == concat!("Warp", "DistanceTooShort") { fly_near_point(ctx, p, bpos, 10_000.0, 12.0).await; } else { panic!("warp insurgency site failed: {e:?}"); }
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.step_manual(1.0).await.unwrap();
    for _ in 0..ticks { ctx.advance_virtual_clock(60); step_system_insurgency_tick_delta(ctx, sys_id, Some(60)).await; }
    tick_insurgency_complex_ops(ctx, ctx.now_unix()).await;
    let star_id = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 6).map(|c| c.celestial_id).unwrap();
    warp_to_item(ctx, p, star_id, 100_000.0).await.expect("leave grid");
}

fn find_trader_recipe(ctx: &ServerContext, trader_tid: u32, required_tid: u32) -> (i64, Vec<(u32, i32)>, Vec<u32>) {
    let (recipes, _) = ctx.sde.item_trader_component_for_type(trader_tid).unwrap().expect("itemTrader component");
    let table = ctx.sde.load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary").unwrap();
    for rid in recipes {
        let Some(rec) = table.get(&rid.to_string()) else { continue };
        let ins: Vec<(u32, i32)> = rec.get("inputItems").and_then(|v| v.as_object()).map(|o| o.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.as_i64()? as i32))).collect()).unwrap_or_default();
        if ins.iter().any(|&(t, _)| t == required_tid) {
            let outs = rec.get("outputItems").and_then(|v| v.as_object()).map(|o| o.keys().filter_map(|k| k.parse::<u32>().ok()).collect()).unwrap_or_default();
            return (i64::from(rid), ins, outs);
        }
    }
    panic!("No recipe for trader {trader_tid} with input {required_tid}");
}

async fn ensure_trade_inputs_in_cargo(ctx: &ServerContext, p: &mut LivePilot, ins: &[(u32, i32)]) {
    for &(tid, req_q) in ins {
        let have: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|it| it.owner_id == p.char_id && it.location_id == p.ship_id && it.flag == InventoryFlag::Cargo && it.type_id == tid).map(|it| it.quantity).sum());
        if have < req_q { common::stock_ship_bay(ctx, p, tid, req_q - have, InventoryFlag::Cargo as i64).await; }
    }
}

async fn call_process_trade(ctx: &ServerContext, p: &mut LivePilot, trader_id: i64, recipe_id: i64) -> Result<PyRep, BridgeRpcError> {
    rpc_bound(ctx, &mut p.sess, "beyonce", p.beyonce, "CallComponentFromClient", &[PyRep::Integer(trader_id), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(recipe_id), PyRep::Integer(1)]).await
}

async fn step_ops(ctx: &ServerContext, dt: u64) { ctx.advance_virtual_clock(dt); tick_insurgency_complex_ops(ctx, ctx.now_unix()).await; }

fn char_militia_lp(ctx: &ServerContext, char_id: i64, faction_id: i64) -> i64 {
    let corp = militia_lp_corp_id(&ctx.sde, faction_id);
    ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.loyalty_points.get(&corp)).copied().unwrap_or(0))
}
async fn verify_war_hq_handin(ctx: &ServerContext, pilot: &mut LivePilot, wrong_fac_pilot: &mut LivePilot, camp_id: i64, fob_sys: u32, hq_sys: u32, hub_id: i64, hub_tid: u32, war_fac: i64, seq_tid: u32) {
    let (recipe_id, req_ins, expected_crates) = find_trader_recipe(ctx, hub_tid, seq_tid);
    assert!(!expected_crates.is_empty());
    travel_to(ctx, pilot, hq_sys).await;
    assert_eq!(call_process_trade(ctx, pilot, hub_id, recipe_id).await.unwrap_err().code, "ItemTraderOutOfRange");
    warp_to_item(ctx, pilot, hub_id, 0.0).await.unwrap();
    travel_to(ctx, wrong_fac_pilot, hq_sys).await;
    warp_to_item(ctx, wrong_fac_pilot, hub_id, 0.0).await.unwrap();
    assert_eq!(call_process_trade(ctx, wrong_fac_pilot, hub_id, recipe_id).await.unwrap_err().code, "FactionNotAligned");
    let has_inputs = req_ins.iter().all(|&(tid, req_q)| ctx.db.with_snapshot(|s| s.items.values().filter(|it| it.owner_id == pilot.char_id && it.location_id == pilot.ship_id && it.flag == InventoryFlag::Cargo && it.type_id == tid).map(|it| it.quantity).sum::<i32>()) >= req_q);
    if !has_inputs { assert_eq!(call_process_trade(ctx, pilot, hub_id, recipe_id).await.unwrap_err().code, "InsufficientInputItems"); }
    ensure_trade_inputs_in_cargo(ctx, pilot, &req_ins).await;
    let lp_before = char_militia_lp(ctx, pilot.char_id, war_fac);
    let supp_before = read_insurgency_runtime(ctx, |rt| rt.campaigns[&camp_id].suppression_points.get(&fob_sys).copied().unwrap_or(0));
    let PyRep::Tuple(tparts) = call_process_trade(ctx, pilot, hub_id, recipe_id).await.expect("War HQ ProcessTrade") else { panic!() };
    let PyRep::List(delivered) = &tparts[1] else { panic!() };
    assert_eq!(tparts[0], PyRep::Bool(true));
    assert!(expected_crates.contains(&ctx.db.with_snapshot(|s| s.items[&delivered[0].as_i64().unwrap()].type_id)));
    assert_eq!(char_militia_lp(ctx, pilot.char_id, war_fac) - lp_before, 2_500);
    assert_eq!(read_insurgency_runtime(ctx, |rt| rt.campaigns[&camp_id].suppression_points[&fob_sys]) - supp_before, 15);
}

#[tokio::test]
async fn test_live_insurgency_laundering_roamers_and_war_hq_crates() {
    // parity: feat:pve.insurgency-laundering
    eve_testkit::attest(&["feat:pve.insurgency-laundering"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.mechanic_rng.set_seed(Some(42));
    let (hurr, gun, gyro, mwd, ammo, rifter) = gear_type_ids(&ctx);
    let (caracal, rlml, bcs, light_msl) = (sde_type_id(&ctx, "Caracal"), sde_type_id(&ctx, "Rapid Light Missile Launcher II"), sde_type_id(&ctx, "Ballistic Control System II"), sde_type_id(&ctx, "Scourge Light Missile"));
    let gur_sys = 30_002_538_u32;
    let (mut p_gur, guns_gur) = spawn_live_pilot(&ctx, 71_001, 95_100_001, gur_sys, caracal, rlml, bcs, mwd, light_msl).await;
    let (mut p_cal, guns_cal) = spawn_live_pilot(&ctx, 71_002, 95_100_002, gur_sys, caracal, rlml, bcs, mwd, light_msl).await;
    let (mut p_gal, _) = spawn_live_pilot(&ctx, 71_003, 95_100_003, HQ_SYSTEM_INTAKI, rifter, 0, 0, 0, 0).await;
    enlist(&ctx, &mut p_gur, FACTION_GURISTAS).await;
    enlist(&ctx, &mut p_cal, 500_001).await;
    enlist(&ctx, &mut p_gal, 500_004).await;
    let _ = rpc(&ctx, &mut p_gur.sess, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await;
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;

    let (camp_id, fob_sys, sys0) = read_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.values().find(|c| c.pirate_faction_id == FACTION_GURISTAS).unwrap();
        (c.campaign_id, c.fob_solar_system_id, c.systems[0])
    });

    // Stage 2: regular roamer drops NO special sequence.
    hold_fw_complex_ticks(&ctx, &mut p_gur, sys0, 2).await;
    assert_eq!(insurgency_system_corruption_stage(&ctx, sys0), 2);
    let reg_pir = with_combat_ephemeral(&ctx, |rt| rt.npc_entities.values().find(|n| n.solar_system_id == sys0 && n.archetype == "InsurgencyPirateRoamer" && !is_named_insurgency_roamer(n.type_id)).map(|n| n.ball_id)).expect("Stage 2 regular roamer");
    travel_to(&ctx, &mut p_cal, sys0).await;
    let reg_wreck = kill_npc_live(&ctx, &mut p_cal, &guns_cal, reg_pir).await.unwrap();
    assert!(!loot_wreck_via_invbroker(&ctx, &mut p_cal, reg_wreck).await.contains(&TYPE_GURISTAS_INSURGENCY_DATA_SEQ));

    let s: u64 = 0u64.wrapping_sub(0x9E37_79B9_7F4A_7C15);
    ctx.mechanic_rng.set_seed(Some(s));
    hold_fw_complex_ticks(&ctx, &mut p_gur, sys0, 1).await;
    let (mut looted_gur_seq, mut looted_upwell) = (false, false);
    for idx in 0..12_usize {
        let Some(sys_i) = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].systems.get(idx).copied()) else { break };
        if insurgency_system_corruption_stage(&ctx, sys_i) < 3 {
            ctx.mechanic_rng.set_seed(Some(s));
            hold_fw_complex_ticks(&ctx, &mut p_gur, sys_i, 3).await;
        }
        if ctx.db.with_snapshot(|snap| snap.system_states.get(&sys_i).map_or(0, |st| st.insurgency_suppression_stage)) < 3 {
            ctx.mechanic_rng.set_seed(Some(s));
            hold_fw_complex_ticks(&ctx, &mut p_cal, sys_i, 3).await;
        }
        let npcs: Vec<(i64, u32)> = with_combat_ephemeral(&ctx, |rt| rt.npc_entities.values().filter(|n| n.solar_system_id == sys_i).map(|n| (n.ball_id, n.type_id)).collect());
        if !looted_gur_seq {
            if let Some(&(bid, _)) = npcs.iter().find(|(_, t)| is_named_pirate_commander_roamer(*t)) {
                travel_to(&ctx, &mut p_cal, sys_i).await;
                let wid = kill_npc_live(&ctx, &mut p_cal, &guns_cal, bid).await.unwrap();
                looted_gur_seq = loot_wreck_via_invbroker(&ctx, &mut p_cal, wid).await.contains(&TYPE_GURISTAS_INSURGENCY_DATA_SEQ);
            }
        }
        if !looted_upwell {
            if let Some(&(bid, _)) = npcs.iter().find(|(_, t)| is_named_insurgency_roamer(*t) && !is_named_pirate_commander_roamer(*t)) {
                travel_to(&ctx, &mut p_gur, sys_i).await;
                let wid = kill_npc_live(&ctx, &mut p_gur, &guns_gur, bid).await.unwrap();
                looted_upwell = loot_wreck_via_invbroker(&ctx, &mut p_gur, wid).await.contains(&TYPE_UPWELL_CONSORTIUM_DATA_CACHE);
            }
        }
        if looted_gur_seq && looted_upwell { break; }
    }
    assert!(looted_gur_seq && looted_upwell);

    let lf_id = 1_891_100_000 + camp_id;
    let (lf_recipe, lf_ins, lf_crates) = find_trader_recipe(&ctx, TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_UPWELL_CONSORTIUM_DATA_CACHE);
    travel_to(&ctx, &mut p_gur, fob_sys).await;
    warp_to_item(&ctx, &mut p_gur, lf_id, 0.0).await.unwrap();
    ensure_trade_inputs_in_cargo(&ctx, &mut p_gur, &lf_ins).await;
    let gur_lp0 = char_militia_lp(&ctx, p_gur.char_id, FACTION_GURISTAS);
    let PyRep::Tuple(lres) = call_process_trade(&ctx, &mut p_gur, lf_id, lf_recipe).await.unwrap() else { panic!() };
    let PyRep::List(ldeliv) = &lres[1] else { panic!() };
    assert!(lf_crates.contains(&ctx.db.with_snapshot(|s| s.items[&ldeliv[0].as_i64().unwrap()].type_id)));
    assert_eq!(char_militia_lp(&ctx, p_gur.char_id, FACTION_GURISTAS) - gur_lp0, 2_500);
    verify_war_hq_handin(&ctx, &mut p_cal, &mut p_gal, camp_id, fob_sys, HQ_SYSTEM_ONNAMON, 1_891_200_003, TYPE_STATE_CI_OUTPOST_HUB, 500_001, TYPE_GURISTAS_INSURGENCY_DATA_SEQ).await;
    verify_war_hq_handin(&ctx, &mut p_gal, &mut p_cal, camp_id, fob_sys, HQ_SYSTEM_INTAKI, 1_891_200_001, TYPE_FEDERATION_CI_OUTPOST_HUB, 500_004, TYPE_GURISTAS_INSURGENCY_DATA_SEQ).await;

    // Angel Cartel campaign (`Mehatoor` Amarr & `Amo` Minmatar + Angel FOB Laundering Facility).
    let ctx2 = ServerContext::new();
    common::pin_clock(&ctx2);
    ctx2.mechanic_rng.set_seed(Some(42));
    let amamin_fl = ctx2.db.with_snapshot(|s| *compute_all_frontline_statuses(&ctx2, s).iter().find(|(&sys, &st)| st == STATUS_FRONTLINE && s.fw_system_occupations.get(&sys).is_some_and(|o| matches!(o.occupier_faction_id, 500_002 | 500_003))).unwrap().0);
    let (mut p_ang, _) = spawn_live_pilot(&ctx2, 71_101, 95_101_001, amamin_fl, hurr, gun, gyro, mwd, ammo).await;
    let (mut p_ama, _) = spawn_live_pilot(&ctx2, 71_102, 95_101_002, HQ_SYSTEM_MEHATOOR, rifter, 0, 0, 0, 0).await;
    let (mut p_min, _) = spawn_live_pilot(&ctx2, 71_103, 95_101_003, HQ_SYSTEM_AMO, rifter, 0, 0, 0, 0).await;
    enlist(&ctx2, &mut p_ang, FACTION_ANGEL_CARTEL).await;
    enlist(&ctx2, &mut p_ama, 500_003).await;
    enlist(&ctx2, &mut p_min, 500_002).await;
    let _ = rpc(&ctx2, &mut p_ang.sess, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await;
    tick_insurgency_campaigns(&ctx2, ctx2.now_unix()).await;
    ctx2.advance_secs(FORECASTING_DURATION_SECS + 10);
    tick_insurgency_campaigns(&ctx2, ctx2.now_unix()).await;
    tick_insurgency_complex_ops(&ctx2, ctx2.now_unix()).await;
    let (ang_cid, ang_fob, ang_s0) = read_insurgency_runtime(&ctx2, |rt| { let c = rt.campaigns.values().find(|c| c.pirate_faction_id == FACTION_ANGEL_CARTEL).unwrap(); (c.campaign_id, c.fob_solar_system_id, c.systems[0]) });
    hold_fw_complex_ticks(&ctx2, &mut p_ang, ang_s0, 3).await;
    let ang_lf = 1_891_100_000 + ang_cid;
    let (alf_rec, alf_ins, alf_crates) = find_trader_recipe(&ctx2, TYPE_ANGEL_LAUNDERING_FACILITY, TYPE_UPWELL_CONSORTIUM_DATA_CACHE);
    travel_to(&ctx2, &mut p_ang, ang_fob).await;
    warp_to_item(&ctx2, &mut p_ang, ang_lf, 0.0).await.unwrap();
    ensure_trade_inputs_in_cargo(&ctx2, &mut p_ang, &alf_ins).await;
    let PyRep::Tuple(ares) = call_process_trade(&ctx2, &mut p_ang, ang_lf, alf_rec).await.unwrap() else { panic!() };
    let PyRep::List(adeliv) = &ares[1] else { panic!() };
    assert!(alf_crates.contains(&ctx2.db.with_snapshot(|s| s.items[&adeliv[0].as_i64().unwrap()].type_id)));
    verify_war_hq_handin(&ctx2, &mut p_ama, &mut p_min, ang_cid, ang_fob, HQ_SYSTEM_MEHATOOR, 1_891_200_002, TYPE_IMPERIAL_CI_OUTPOST_HUB, 500_003, TYPE_ANGEL_INSURGENCY_DATA_SEQ).await;
    verify_war_hq_handin(&ctx2, &mut p_min, &mut p_ama, ang_cid, ang_fob, HQ_SYSTEM_AMO, 1_891_200_004, TYPE_REPUBLIC_CI_OUTPOST_HUB, 500_002, TYPE_ANGEL_INSURGENCY_DATA_SEQ).await;
}

#[tokio::test]
async fn test_live_insurgency_mining_ambush_ransom_rescue_destroy_lp_split() {
    // parity: feat:pve.insurgency-mining-ambush
    eve_testkit::attest(&["feat:pve.insurgency-mining-ambush"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (hurr, gun, gyro, mwd, ammo, rifter) = gear_type_ids(&ctx);

    let sys_id = 30_002_538_u32;
    let (mut pirates, mut pir_guns) = (Vec::new(), Vec::new());
    for i in 0..6_usize {
        let (ship, g) = if i == 0 { (hurr, gun) } else { (rifter, 0) };
        let (p, gs) = spawn_live_pilot(&ctx, 72_001 + i as u64, 95_200_001 + i as i64, sys_id, ship, g, gyro, mwd, ammo).await;
        pirates.push(p);
        if i == 0 { pir_guns = gs; }
    }
    let (mut p_cal, guns_cal) = spawn_live_pilot(&ctx, 72_010, 95_200_010, sys_id, hurr, gun, gyro, mwd, ammo).await;
    for p in &mut pirates { enlist(&ctx, p, FACTION_GURISTAS).await; }
    enlist(&ctx, &mut p_cal, 500_001).await;
    let _ = rpc(&ctx, &mut pirates[0].sess, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await;
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;

    let (camp_id, systems) = read_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.values().find(|c| c.pirate_faction_id == FACTION_GURISTAS).unwrap();
        (c.campaign_id, c.systems.clone())
    });

    let (s0, s1, s2) = (systems[0], systems[1], systems[2]);
    hold_fw_complex_ticks(&ctx, &mut pirates[0], s0, 1).await;
    let amb0 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&s0].clone());
    assert!(ctx.systems.get_or_spawn(s0).get_combat_state(amb0.rorqual_ball_id).await.unwrap().unwrap().warp_scramble_strength >= 3);
    for p in &mut pirates { travel_to(&ctx, p, s0).await; warp_to_item(&ctx, p, amb0.beacon_id, 1_500.0).await.unwrap(); }
    for def_id in amb0.ore_defender_ids { kill_npc_live(&ctx, &mut pirates[0], &pir_guns, def_id).await; }
    let pir_lp0: Vec<i64> = pirates.iter().map(|p| char_militia_lp(&ctx, p.char_id, FACTION_GURISTAS)).collect();
    let corr0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points.get(&s0).copied().unwrap_or(0));

    step_ops(&ctx, 295).await;
    assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&s0].resolved));
    step_ops(&ctx, 5).await;
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&s0].resolved));
    assert!(matches!(ctx.systems.get_or_spawn(s0).get_ball_snapshot(amb0.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points[&s0]) - corr0, 50);
    for (idx, p) in pirates.iter().enumerate() { assert_eq!(char_militia_lp(&ctx, p.char_id, FACTION_GURISTAS) - pir_lp0[idx], 25_000); }

    hold_fw_complex_ticks(&ctx, &mut pirates[0], s1, 1).await;
    let amb1 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&s1].clone());
    travel_to(&ctx, &mut p_cal, s1).await;
    warp_to_item(&ctx, &mut p_cal, amb1.beacon_id, 1_500.0).await.unwrap();
    let cal_lp0 = char_militia_lp(&ctx, p_cal.char_id, 500_001);
    let supp0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points.get(&s1).copied().unwrap_or(0));
    for tackler_id in amb1.pirate_tackler_ids { kill_npc_live(&ctx, &mut p_cal, &guns_cal, tackler_id).await; }
    assert!(matches!(ctx.systems.get_or_spawn(s1).get_ball_snapshot(amb1.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points[&s1]) - supp0, 50);
    assert!(char_militia_lp(&ctx, p_cal.char_id, 500_001) - cal_lp0 >= 30_000);

    hold_fw_complex_ticks(&ctx, &mut pirates[0], s2, 1).await;
    let amb2 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&s2].clone());
    travel_to(&ctx, &mut pirates[0], s2).await;
    warp_to_item(&ctx, &mut pirates[0], amb2.beacon_id, 1_500.0).await.unwrap();
    let lp_before_kill = char_militia_lp(&ctx, pirates[0].char_id, FACTION_GURISTAS);
    let corr2_0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points.get(&s2).copied().unwrap_or(0));
    let rorq_wreck = kill_npc_live(&ctx, &mut pirates[0], &pir_guns, amb2.rorqual_ball_id).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&rorq_wreck].type_id), TYPE_ORE_RORQUAL_WRECK);
    assert_eq!(char_militia_lp(&ctx, pirates[0].char_id, FACTION_GURISTAS), lp_before_kill);
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points[&s2]) - corr2_0, 50);
    assert!(loot_wreck_via_invbroker(&ctx, &mut pirates[0], rorq_wreck).await.contains(&TYPE_UPWELL_CONSORTIUM_DATA_CACHE));
}

#[tokio::test]
async fn test_live_insurgency_outpost_raid_sizes_gates_bc_bonus_and_respawn() {
    // parity: feat:pve.insurgency-outpost-raid
    eve_testkit::attest(&["feat:pve.insurgency-outpost-raid"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let (rifter, svipul, cormorant, caracal, hurr, tengu, raven, wcs, gun, sgun, gyro, mwd, smwd, ammo, sammo) = (
        sde_type_id(&ctx, "Rifter"), sde_type_id(&ctx, "Svipul"), sde_type_id(&ctx, "Cormorant"), sde_type_id(&ctx, "Caracal"), sde_type_id(&ctx, "Hurricane"),
        sde_type_id(&ctx, "Tengu"), sde_type_id(&ctx, "Raven"), sde_type_id(&ctx, "Warp Core Stabilizer I"),
        sde_type_id(&ctx, "425mm AutoCannon II"), sde_type_id(&ctx, "200mm AutoCannon II"), sde_type_id(&ctx, "Gyrostabilizer II"),
        sde_type_id(&ctx, "50MN Y-T8 Compact Microwarpdrive"), sde_type_id(&ctx, "5MN Y-T8 Compact Microwarpdrive"), sde_type_id(&ctx, "Republic Fleet EMP M"), sde_type_id(&ctx, "Republic Fleet EMP S"),
    );
    let sys_id = 30_002_538_u32;
    let (mut p_pir_bc, guns_bc) = spawn_live_pilot(&ctx, 73_001, 95_300_001, sys_id, hurr, gun, gyro, mwd, ammo).await;
    let (mut p_pir_t3d, guns_t3d) = spawn_live_pilot(&ctx, 73_002, 95_300_002, sys_id, svipul, sgun, gyro, smwd, sammo).await;
    let ((mut p_pir_t3c, _), (mut p_pir_bs, _)) = (spawn_live_pilot(&ctx, 73_003, 95_300_003, sys_id, tengu, 0, 0, 0, 0).await, spawn_live_pilot(&ctx, 73_004, 95_300_004, sys_id, raven, 0, 0, 0, 0).await);
    let (mut p_cal_big, guns_cal) = spawn_live_pilot(&ctx, 73_005, 95_300_005, sys_id, hurr, gun, gyro, mwd, ammo).await;
    let (mut p_cal_sml, guns_sml) = spawn_live_pilot(&ctx, 73_006, 95_300_006, sys_id, cormorant, sgun, gyro, smwd, sammo).await;
    let ((mut p_cal_frig, _), (mut p_amarr, _), (mut p_neutral, _), (mut p_cal_cru, _)) = (spawn_live_pilot(&ctx, 73_007, 95_300_007, sys_id, rifter, 0, 0, 0, 0).await, spawn_live_pilot(&ctx, 73_008, 95_300_008, sys_id, rifter, 0, 0, 0, 0).await, spawn_live_pilot(&ctx, 73_009, 95_300_009, sys_id, rifter, 0, 0, 0, 0).await, spawn_live_pilot(&ctx, 73_010, 95_300_010, sys_id, caracal, 0, 0, 0, 0).await);
    for p in [&mut p_pir_bc, &mut p_pir_t3d, &mut p_pir_t3c, &mut p_pir_bs] { enlist(&ctx, p, FACTION_GURISTAS).await; }
    for p in [&mut p_cal_big, &mut p_cal_sml, &mut p_cal_frig, &mut p_cal_cru] { enlist(&ctx, p, 500_001).await; }
    enlist(&ctx, &mut p_amarr, 500_003).await;
    let _ = rpc(&ctx, &mut p_pir_bc.sess, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await;
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await; ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await; tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;
    let (camp_id, s0) = read_insurgency_runtime(&ctx, |rt| { let c = rt.campaigns.values().find(|c| c.pirate_faction_id == FACTION_GURISTAS).unwrap(); (c.campaign_id, c.systems[0]) });
    for p in [&mut p_pir_bc, &mut p_pir_t3d, &mut p_pir_t3c, &mut p_pir_bs, &mut p_cal_big, &mut p_cal_sml, &mut p_cal_frig, &mut p_amarr, &mut p_neutral, &mut p_cal_cru] { travel_to(&ctx, p, s0).await; }
    let gate = |sz: OutpostRaidSize| 1_893_100_000 + i64::from(s0) * 10 + sz.slot_index();
    let (g_small, g_med, g_mod, g_ladv) = (gate(OutpostRaidSize::Small), gate(OutpostRaidSize::Medium), gate(OutpostRaidSize::Moderate), gate(OutpostRaidSize::LargeAdv));
    let star_id = ctx.sde.celestials_in_system(s0)[0].celestial_id;
    assert_eq!((OutpostRaidSize::Small.base_capture_duration_secs(), OutpostRaidSize::Medium.base_capture_duration_secs(), OutpostRaidSize::Moderate.base_capture_duration_secs(), OutpostRaidSize::LargeAdv.base_capture_duration_secs()), (600, 900, 900, 900));
    assert_eq!(ctx.db.with_snapshot(|s| { let st = load_ops_state(s); (!st.outpost_raids[&g_mod].is_static, st.outpost_raids[&g_ladv].is_static, st.outpost_raids[&g_ladv].size.respawn_delay_secs()) }), (true, true, LARGE_ADV_OUTPOST_RESPAWN_SECS));

    // 2. Gate rejections: distance > 2500m, non-militia & cross-warzone, cruiser/T3D in Small, deadspace direct warp block, WCS & Capsule.
    assert_eq!(rpc(&ctx, &mut p_cal_frig.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "TooFarFromGate");
    for p in [&mut p_neutral, &mut p_amarr] {
        warp_to_item(&ctx, p, g_small, 0.0).await.unwrap();
        assert_eq!(rpc(&ctx, &mut p.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "NotEnlistedInMilitia");
    }
    warp_to_item(&ctx, &mut p_cal_cru, g_small, 0.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p_cal_cru.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "ShipRestrictedForComplex");
    warp_to_item(&ctx, &mut p_pir_t3d, g_small, 0.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p_pir_t3d.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "ShipRestrictedForComplex");

    warp_to_item(&ctx, &mut p_cal_frig, g_small, 0.0).await.unwrap();
    rpc(&ctx, &mut p_cal_frig.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.expect("Small admits Frigate");
    for (m, a) in [("CreateFleet", vec![]), ("Invite", vec![PyRep::Integer(p_cal_frig.char_id), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)])] { rpc(&ctx, &mut p_cal_big.sess, "fleetMgr", m, &a).await.unwrap(); }
    rpc(&ctx, &mut p_cal_frig.sess, "fleetMgr", "AcceptInvite", &[PyRep::Bool(false)]).await.unwrap();
    assert_eq!(rpc_bound(&ctx, &mut p_cal_big.sess, "beyonce", p_cal_big.beyonce, "CmdWarpToStuff", &[PyRep::String("char".into()), PyRep::Integer(p_cal_frig.char_id), PyRep::Real(0.0)]).await.unwrap_err().code, "NaturalPhenomenaDisruptingWarp");
    let _ = rpc(&ctx, &mut p_cal_frig.sess, "fleetMgr", "LeaveFleet", &[]).await;
    ctx.advance_secs(11);

    let station_id = ctx.sde.solar_systems[&s0].stations[0].station_id;
    for dock_wcs in [true, false] {
        warp_to_item(&ctx, &mut p_cal_frig, station_id, 0.0).await.unwrap();
        rpc(&ctx, &mut p_cal_frig.sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await.unwrap();
        ctx.advance_secs(11);
        if dock_wcs { slash(&ctx, &mut p_cal_frig.sess, &format!("/fit me {wcs}")).await; } else { rpc(&ctx, &mut p_cal_frig.sess, "ship", "LeaveShip", &[]).await.unwrap(); ctx.advance_secs(11); }
        rpc(&ctx, &mut p_cal_frig.sess, "stationSvc", "Undock", &[]).await.unwrap();
        p_cal_frig.ship_id = p_cal_frig.sess.ship_id.unwrap();
        slash(&ctx, &mut p_cal_frig.sess, "/heal").await;
        for _ in 0..32 { ctx.systems.get_or_spawn(p_cal_frig.sess.solar_system_id.unwrap()).step_manual(1.0).await.unwrap(); }
        travel_to(&ctx, &mut p_cal_frig, s0).await;
        warp_to_item(&ctx, &mut p_cal_frig, g_small, 0.0).await.unwrap();
        assert_eq!(rpc(&ctx, &mut p_cal_frig.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, if dock_wcs { "WarpCoreStabilizerRestricted" } else { "ShipRestrictedForComplex" });
    }

    let sml_def = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].pirate_defender_ball_id.unwrap());
    warp_to_item(&ctx, &mut p_cal_sml, g_small, 0.0).await.unwrap();
    rpc(&ctx, &mut p_cal_sml.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap();
    let supp_s0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points.get(&s0).copied().unwrap_or(0));
    kill_npc_live(&ctx, &mut p_cal_sml, &guns_sml, sml_def).await;
    step_ops(&ctx, 600).await;
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points[&s0]) - supp_s0, 2);
    warp_to_item(&ctx, &mut p_cal_sml, star_id, 100_000.0).await.unwrap();

    warp_to_item(&ctx, &mut p_pir_t3c, g_mod, 0.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p_pir_t3c.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.unwrap_err().code, "ShipRestrictedForComplex");

    let fit_before = ctx.get_or_rebuild_ship_fitting(p_pir_bc.char_id, p_pir_bc.ship_id).unwrap();
    let eval_before: Vec<(u16, f64)> = { let ev = fit_before.evaluated_context(&ctx.sde); ev.ship.base_attributes.keys().copied().map(|aid| (aid, ev.ship.get_attribute(aid, &ctx.sde))).collect() };
    warp_to_item(&ctx, &mut p_pir_bc, g_mod, 0.0).await.unwrap();
    rpc(&ctx, &mut p_pir_bc.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.expect("Moderate admits BC");
    let fit_in = ctx.get_or_rebuild_ship_fitting(p_pir_bc.char_id, p_pir_bc.ship_id).unwrap();
    assert!(fit_in.gang_bursts.iter().any(|b| b.dbuff_id == Some(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID)));
    let ev_in = fit_in.evaluated_context(&ctx.sde);
    assert!(eval_before.iter().any(|&(aid, b)| b > 0.0 && (ev_in.ship.get_attribute(aid, &ctx.sde) / b - 1.20).abs() < 0.01), "Moderate grants +20% BC bonus");

    let mordus_id = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].mordus_defender_ball_id.unwrap());
    assert_eq!(with_combat_ephemeral(&ctx, |rt| rt.npc_entities[&mordus_id].faction_id), 500_018);
    let corr_mod0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points.get(&s0).copied().unwrap_or(0));
    kill_npc_live(&ctx, &mut p_pir_bc, &guns_bc, mordus_id).await;
    step_ops(&ctx, 900).await;
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points[&s0]) - corr_mod0, 15);
    warp_to_item(&ctx, &mut p_pir_bc, star_id, 100_000.0).await.unwrap();

    // 4. Medium (T3C rejected, T3D admitted & captures 0.5% = +5 pts) & LargeAdv (BS & T3C admitted, 1.0% = +10 pts, 40m = 2,400s respawn).
    warp_to_item(&ctx, &mut p_pir_t3c, g_med, 0.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p_pir_t3c.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)]).await.unwrap_err().code, "ShipRestrictedForComplex");

    warp_to_item(&ctx, &mut p_pir_t3d, g_med, 0.0).await.unwrap();
    rpc(&ctx, &mut p_pir_t3d.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)]).await.expect("Medium admits T3D");
    let med_def = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_med].mordus_defender_ball_id.unwrap());
    let corr_med0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points[&s0]);
    kill_npc_live(&ctx, &mut p_pir_t3d, &guns_t3d, med_def).await;
    step_ops(&ctx, 900).await;
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].corruption_points[&s0]) - corr_med0, 5);
    warp_to_item(&ctx, &mut p_pir_t3d, star_id, 100_000.0).await.unwrap();

    for p in [&mut p_pir_t3c, &mut p_pir_bs] {
        warp_to_item(&ctx, p, g_ladv, 0.0).await.unwrap();
        rpc(&ctx, &mut p.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)]).await.expect("LargeAdv admits T3C/BS");
        warp_to_item(&ctx, p, star_id, 100_000.0).await.unwrap();
    }

    warp_to_item(&ctx, &mut p_cal_big, g_ladv, 0.0).await.unwrap();
    rpc(&ctx, &mut p_cal_big.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)]).await.unwrap();
    let pir_def = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].pirate_defender_ball_id.unwrap());
    assert_eq!(with_combat_ephemeral(&ctx, |rt| rt.npc_entities[&pir_def].faction_id), FACTION_GURISTAS as u32);
    let supp_ladv0 = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points[&s0]);
    kill_npc_live(&ctx, &mut p_cal_big, &guns_cal, pir_def).await;
    step_ops(&ctx, 600).await;
    assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].completed_on_cooldown));
    step_ops(&ctx, 300).await;
    assert_eq!(read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp_id].suppression_points[&s0]) - supp_ladv0, 10);
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].completed_on_cooldown));
    step_ops(&ctx, LARGE_ADV_OUTPOST_RESPAWN_SECS - 1).await;
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].completed_on_cooldown));
    step_ops(&ctx, 2).await;
    assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].completed_on_cooldown));
    assert!(ctx.systems.get_or_spawn(s0).get_ball_snapshot(g_ladv).await.unwrap().is_some());
}
