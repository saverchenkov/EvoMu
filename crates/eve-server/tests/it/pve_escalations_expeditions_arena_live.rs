//! Live end-to-end integration tests for PvE escalations, multi-step expeditions,
//! and Proving Grounds queue validation, arena hazards, and rewards.

use crate::common::{gm_pilot_in_space, login_pilot, pin_clock, slash_cmd, stock_ship_bay, LivePilot};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::rng::MechanicRng;
use eve_server::router::ServerContext;
use eve_server::services::pvp_filament_mgr::get_pvp_filament_match_spec;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn fresh_ctx() -> ServerContext {
    static BASE: std::sync::OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = std::sync::OnceLock::new();
    let (sde, base_snap) = BASE.get_or_init(|| {
        let base_ctx = ServerContext::new();
        (base_ctx.sde.clone(), base_ctx.db.with_snapshot(Clone::clone))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    pin_clock(&ctx);
    ctx
}

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const TYPE_CAPSULE: u32 = 670;
const TYPE_ATRON: u32 = 608;
const TYPE_LARGE_INJECTOR: u32 = 40_520;

async fn rpc(ctx: &ServerContext, p: &mut LivePilot, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let oid = (svc == "beyonce" && m != "MachoBindObject" && p.beyonce != 0).then_some(p.beyonce);
    ctx.dispatch_rpc(&mut p.sess, svc, oid, m, args, &HashMap::new(), None).await
}
async fn ok(ctx: &ServerContext, p: &mut LivePilot, svc: &str, m: &str, args: &[PyRep]) -> PyRep {
    rpc(ctx, p, svc, m, args).await.unwrap_or_else(|e| panic!("{svc}.{m} failed: {e:?}"))
}
async fn bind_oid(ctx: &ServerContext, p: &mut LivePilot, svc: &str, arg: PyRep) -> u64 {
    let PyRep::Tuple(parts) = ok(ctx, p, svc, "MachoBindObject", &[arg, PyRep::None]).await else { panic!("bind tuple") };
    parts[0].as_i64().expect("bound oid") as u64
}
fn drain(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let mut out = p.sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(p.char_id) { if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) { out.push((n.event, n.args)); } }
    out
}
fn extract_rowset_rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep { PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => (columns.clone(), rows.clone()), PyRep::IndexRowset { columns, items, .. } => (columns.clone(), items.iter().map(|(_, r)| r.clone()).collect()), _ => (Vec::new(), Vec::new()) }
}
fn row_col<'a>(cols: &[String], row: &'a [PyRep], name: &str) -> Option<&'a PyRep> { cols.iter().position(|c| c == name).and_then(|i| row.get(i)) }
async fn create_item_id(ctx: &ServerContext, gm: &mut LivePilot, type_id: u32, qty: i32) -> i64 {
    slash_cmd(ctx, &mut gm.sess, &format!("/createitem {type_id} {qty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).expect("/createitem itemID")
}

async fn dock_gm_at(ctx: &ServerContext, gm: &mut LivePilot, sys_id: u32, stn_id: i64) {
    if gm.sess.station_id == Some(stn_id) { return; }
    if gm.sess.station_id.is_some() { ok(ctx, gm, "stationSvc", "Undock", &[]).await; }
    ctx.advance_secs(12);
    if gm.sess.solar_system_id != Some(sys_id) { slash_cmd(ctx, &mut gm.sess, &format!("/tr {sys_id}")).await; }
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.set_ball_position(gm.sess.ship_id.unwrap_or(gm.ship_id), ctx.sde.stations[&stn_id].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys_h.step_manual(0.1).await.unwrap();
    ok(ctx, gm, "stationSvc", "Dock", &[PyRep::Integer(stn_id)]).await;
}

async fn fly_stargates_to(ctx: &ServerContext, p: &mut LivePilot, target_sys: u32) {
    let start_sys = p.sess.solar_system_id.expect("pilot in space");
    if start_sys == target_sys { return; }
    let (mut visited, mut queue, mut route) = (HashSet::from([start_sys]), std::collections::VecDeque::from([(start_sys, Vec::<(i64, u32)>::new())]), None);
    while let Some((sys, path)) = queue.pop_front() {
        if sys == target_sys { route = Some(path); break; }
        for j in ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == sys && visited.insert(j.to_solar_system_id)) {
            let mut next = path.clone(); next.push((j.stargate_id, j.to_solar_system_id));
            queue.push_back((j.to_solar_system_id, next));
        }
    }
    let route = route.unwrap_or_else(|| panic!("no stargate route {start_sys} -> {target_sys}"));
    for (gate_id, next_sys) in route {
        let cur_sys = p.sess.solar_system_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(cur_sys);
        p.beyonce = bind_oid(ctx, p, "beyonce", PyRep::Integer(i64::from(cur_sys))).await;
        for hop in 0..8 {
            let _ = ok(ctx, p, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)]).await;
            for _ in 0..80 {
                sys_h.step_manual(12.0).await.unwrap(); ctx.advance_secs(12);
                if sys_h.get_ball_snapshot(p.ship_id).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) { break; }
            }
            let dist = match (sys_h.get_ball_snapshot(p.ship_id).await.unwrap(), sys_h.get_ball_snapshot(gate_id).await.unwrap()) {
                (Some((_, sb)), Some((_, gb))) => sb.position.distance(gb.position),
                _ => 0.0,
            };
            if dist <= 150_000.0 || hop == 7 { break; }
            for _ in 0..6 { sys_h.step_manual(20.0).await.unwrap(); ctx.advance_secs(20); }
        }
        if let (Some((_, sb)), Some((_, gb))) = (sys_h.get_ball_snapshot(p.ship_id).await.unwrap(), sys_h.get_ball_snapshot(gate_id).await.unwrap()) {
            let mut d_surf = (sb.position.distance(gb.position) - gb.radius).max(0.0);
            if d_surf > 2_000.0 {
                for step in 0..400 {
                    if step % 5 == 0 {
                        ok(ctx, p, "beyonce", "CmdGotoPoint", &[PyRep::Real(gb.position.x), PyRep::Real(gb.position.y), PyRep::Real(gb.position.z)]).await;
                    }
                    sys_h.step_manual(4.0).await.unwrap();
                    ctx.advance_secs(4);
                    if let Some((_, b)) = sys_h.get_ball_snapshot(p.ship_id).await.unwrap() {
                        d_surf = (b.position.distance(gb.position) - gb.radius).max(0.0);
                        if d_surf <= 2_000.0 { break; }
                    }
                }
            }
        }
        ctx.advance_secs(12);
        ok(ctx, p, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id)]).await;
        p.beyonce = bind_oid(ctx, p, "beyonce", PyRep::Integer(i64::from(next_sys))).await;
    }
    ctx.advance_secs(35);
    ctx.systems.get_or_spawn(target_sys).step_manual(32.0).await.unwrap();
}

fn find_t1_small_turret_and_ammo(sde: &eve_sde::SdeRegistry) -> (u32, u32) {
    let turret = sde.types.values().filter(|t| {
        t.published && t.category_id == 7 && t.group_id == 55
            && t.default_attributes.get(&633).copied().unwrap_or(0.0) == 0.0
            && t.default_attributes.get(&182).map(|v| *v as u32) == Some(3303)
            && t.default_attributes.get(&54).copied().unwrap_or(0.0) >= 10_000.0
    }).map(|t| t.type_id).min().unwrap_or(488);
    let t_attrs = &sde.types[&turret].default_attributes;
    let (charge_grp, charge_sz) = (t_attrs.get(&604).map(|v| *v as u32).unwrap_or(83), t_attrs.get(&128).copied());
    let ammo = sde.types.values().filter(|t| {
        t.published && t.category_id == 8 && t.group_id == charge_grp
            && t.default_attributes.get(&128).copied() == charge_sz
            && t.default_attributes.get(&633).copied().unwrap_or(0.0) == 0.0
    }).map(|t| t.type_id).min().unwrap_or(185);
    (turret, ammo)
}

fn walk_skill_deps(sde: &eve_sde::SdeRegistry, tid: u32, lvls: &mut HashMap<u32, u8>, seen: &mut HashSet<u32>, order: &mut Vec<u32>) {
    let Some(t) = sde.types.get(&tid) else { return };
    for (sa, la) in [(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
        if let Some(&sk) = t.default_attributes.get(&sa).filter(|&&v| v > 0.0) {
            let (sk_id, req) = (sk as u32, t.default_attributes.get(&la).map(|v| (*v as u8).clamp(1, 5)).unwrap_or(1));
            lvls.entry(sk_id).and_modify(|l| *l = (*l).max(req)).or_insert(req);
            if !seen.contains(&sk_id) { walk_skill_deps(sde, sk_id, lvls, seen, order); }
            if seen.insert(sk_id) { order.push(sk_id); }
        }
    }
}
fn skill_plan_for_type(sde: &eve_sde::SdeRegistry, tid: u32) -> Vec<(u32, u8, i64)> {
    let (mut lvls, mut seen, mut order) = (HashMap::new(), HashSet::new(), Vec::new());
    walk_skill_deps(sde, tid, &mut lvls, &mut seen, &mut order);
    order.into_iter().map(|sk| {
        let rank = sde.types.get(&sk).and_then(|t| t.default_attributes.get(&275)).copied().unwrap_or(1.0).max(1.0);
        let lvl = lvls[&sk];
        let sp = (250.0 * rank * 2.0_f64.powf(2.5 * f64::from(lvl - 1))).ceil() as i64 + 100;
        (sk, lvl, sp)
    }).collect()
}

async fn provision_contract_player_in_space(
    ctx: &ServerContext, gm: &mut LivePilot, sess_id: u64, user: &str, name: &str, hull: u32, fit_guns: bool, extra_types: &[u32], target_sys: u32,
) -> (LivePilot, Vec<i64>, Vec<i64>) {
    let pass = format!("{user}-secret");
    ctx.db.provision_player_account(user, &pass);
    let mut sess = SessionState::new(sess_id);
    login_pilot(ctx, &mut sess, user, &pass).await;
    let doll = PyRep::keyval([("gender", PyRep::Integer(1))]);
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 2).map(|a| i64::from(a.ancestry_id)).unwrap_or(7);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let cid = ctx.dispatch_rpc(
        &mut sess, "charUnboundMgr", None, "CreateCharacterWithDoll",
        &[PyRep::String(name.into()), PyRep::Integer(2), PyRep::Integer(1), PyRep::Integer(anc), doll.clone(), doll, PyRep::Integer(sch)],
        &HashMap::new(), None,
    ).await.expect("CreateCharacterWithDoll").as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &HashMap::new(), None).await.unwrap();
    let mut p = LivePilot { sess, char_id: cid, ship_id: 0, beyonce: 0 };
    let (p_stn, p_sys) = (p.sess.station_id.unwrap_or(JITA_STATION), p.sess.solar_system_id.unwrap_or(JITA_SYS));
    dock_gm_at(ctx, gm, p_sys, p_stn).await;

    let mut transfer_ids = vec![create_item_id(ctx, gm, hull, 1).await];
    let ship_item = transfer_ids[0];
    let (turret_tid, ammo_tid) = find_t1_small_turret_and_ammo(&ctx.sde);
    let mut plan = skill_plan_for_type(&ctx.sde, hull);
    if fit_guns {
        for item in skill_plan_for_type(&ctx.sde, turret_tid) {
            if !plan.iter().any(|&(s, _, _)| s == item.0) { plan.push(item); }
        }
    }
    let inj_cnt = ((plan.iter().map(|&(_, _, s)| s).sum::<i64>() / 400_000) + 2) as i32;
    let inj_id = create_item_id(ctx, gm, TYPE_LARGE_INJECTOR, inj_cnt).await;
    transfer_ids.push(inj_id);
    let mut skill_books = Vec::new();
    for (sk, _, sp) in plan {
        let bk = create_item_id(ctx, gm, sk, 1).await;
        skill_books.push((sk, sp, bk));
        transfer_ids.push(bk);
    }

    let (mut guns, mut ammo_item) = (Vec::new(), 0_i64);
    if fit_guns {
        let n_guns = ctx.sde.types.get(&hull).and_then(|t| t.default_attributes.get(&102)).map(|&v| v as usize).unwrap_or(3).clamp(1, 7);
        for _ in 0..n_guns {
            let gid = create_item_id(ctx, gm, turret_tid, 1).await;
            guns.push(gid);
            transfer_ids.push(gid);
        }
        ammo_item = create_item_id(ctx, gm, ammo_tid, 10_000).await;
        transfer_ids.push(ammo_item);
    }
    let mut extra_ids = Vec::new();
    for &etid in extra_types {
        let eid = create_item_id(ctx, gm, etid, 1).await;
        extra_ids.push(eid);
        transfer_ids.push(eid);
    }
    for &iid in &transfer_ids {
        ok(ctx, gm, "invbroker", "Add", &[PyRep::Integer(iid), PyRep::Integer(p_stn), PyRep::Integer(4)]).await;
    }

    let create_rep = ok(ctx, gm, "contractProxy", "CreateContract", &[PyRep::keyval([
        ("contractType", PyRep::Integer(1)), ("availability", PyRep::Integer(1)),
        ("assigneeID", PyRep::Integer(p.char_id)), ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(1)), ("startStationID", PyRep::Integer(p_stn)),
        ("endStationID", PyRep::Integer(p_stn)), ("price", PyRep::Real(0.0)),
        ("reward", PyRep::Real(0.0)), ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("Fit".into())),
        ("itemList", PyRep::List(transfer_ids.iter().map(|&id| PyRep::Integer(id)).collect())),
    ])]).await;
    let cid_contract = create_rep.as_i64().or_else(|| create_rep.dict_get("contractID").and_then(|v| v.as_i64())).unwrap();
    ok(ctx, &mut p, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_contract), PyRep::Bool(false)]).await;

    for _ in 0..inj_cnt { ok(ctx, &mut p, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(inj_id)]).await; }
    for (sk, sp, bk) in skill_books { let _ = rpc(ctx, &mut p, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(bk)]).await; ok(ctx, &mut p, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(i64::from(sk)), PyRep::Integer(sp)]).await; }

    ctx.advance_secs(31);
    ok(ctx, &mut p, "ship", "Board", &[PyRep::Integer(ship_item)]).await;
    p.ship_id = ship_item;
    if fit_guns {
        for (idx, &gid) in guns.iter().enumerate() {
            ok(ctx, &mut p, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(ship_item), PyRep::Integer(27 + idx as i64)]).await;
        }
        ok(ctx, &mut p, "invbroker", "Add", &[PyRep::Integer(ammo_item), PyRep::Integer(ship_item), PyRep::Integer(5)]).await;
        let g_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
        ok(ctx, &mut p, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_item), PyRep::List(g_reps), PyRep::List(vec![PyRep::Integer(ammo_item)])]).await;
    }
    for &eid in &extra_ids {
        ok(ctx, &mut p, "invbroker", "Add", &[PyRep::Integer(eid), PyRep::Integer(ship_item), PyRep::Integer(5)]).await;
    }
    ctx.advance_secs(31);
    ok(ctx, &mut p, "stationSvc", "Undock", &[]).await;
    ctx.systems.get_or_spawn(p_sys).step_manual(32.0).await.unwrap();
    p.beyonce = bind_oid(ctx, &mut p, "beyonce", PyRep::Integer(i64::from(p_sys))).await;
    ctx.advance_secs(31);
    fly_stargates_to(ctx, &mut p, target_sys).await;
    let _ = drain(ctx, &mut p);
    (p, guns, extra_ids)
}

async fn warp_to_stuff_and_clear_site(
    ctx: &ServerContext, pilot: &mut LivePilot, sys_id: u32,
    stuff_type: &str, warp_target: PyRep, site_id: &str, guns: &[i64], final_shot_seed: Option<u64>,
) {
    if pilot.sess.solar_system_id != Some(sys_id) {
        fly_stargates_to(ctx, pilot, sys_id).await;
    }
    pilot.beyonce = bind_oid(ctx, pilot, "beyonce", PyRep::Integer(i64::from(sys_id))).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    ok(ctx, pilot, "beyonce", "CmdWarpToStuff", &[PyRep::String(stuff_type.into()), warp_target]).await;
    for _ in 0..80 {
        sys.step_manual(12.0).await.unwrap();
        ctx.advance_secs(12);
        if sys.get_ball_snapshot(pilot.ship_id).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) { break; }
    }
    let d_oid = bind_oid(ctx, pilot, "dogmaIM", PyRep::Integer(pilot.ship_id)).await;
    let mut seed_applied = false;
    for _wave in 0..6 {
        let mut npc_ids = Vec::new();
        for _ in 0..2 {
            if let PyRep::List(rows) = ok(ctx, pilot, "scanSvc", "GetCombatAnomalyInstances", &[]).await {
                if let Some(row) = rows.iter().find(|r| r.dict_get("siteID").or_else(|| r.dict_get("id")).and_then(|v| v.as_str()) == Some(site_id)) {
                    if let Some(PyRep::List(ids)) = row.dict_get("activeNpcBallIDs") {
                        npc_ids = ids.iter().filter_map(|v| v.as_i64()).collect();
                    }
                }
            }
            if !npc_ids.is_empty() { break; }
            if rpc(ctx, pilot, "scanSvc", "EnterCombatAnomaly", &[PyRep::String(site_id.to_string()), PyRep::Bool(true)]).await.is_err() {
                break;
            }
        }
        if npc_ids.is_empty() { break; }
        for (idx, nid) in npc_ids.iter().copied().enumerate() {
            let is_last = idx + 1 == npc_ids.len();
            if let (Some((_, sb)), Some((_, nb))) = (sys.get_ball_snapshot(pilot.ship_id).await.unwrap(), sys.get_ball_snapshot(nid).await.unwrap()) {
                if sb.position.distance(nb.position) > 12_000.0 {
                    let pt = nb.position + (sb.position - nb.position).normalize_or_zero() * 9_000.0;
                    ok(ctx, pilot, "beyonce", "CmdGotoPoint", &[PyRep::Real(pt.x), PyRep::Real(pt.y), PyRep::Real(pt.z)]).await;
                    for _ in 0..30 {
                        sys.step_manual(3.0).await.unwrap();
                        ctx.advance_secs(3);
                        if let (Some((_, sb2)), Some((_, nb2))) = (sys.get_ball_snapshot(pilot.ship_id).await.unwrap(), sys.get_ball_snapshot(nid).await.unwrap()) {
                            if sb2.position.distance(nb2.position) <= 12_000.0 { break; }
                        }
                    }
                    let _ = ok(ctx, pilot, "beyonce", "CmdStop", &[]).await;
                    for _ in 0..3 { sys.step_manual(2.0).await.unwrap(); ctx.advance_secs(2); }
                }
            }
            ctx.dispatch_rpc(&mut pilot.sess, "dogmaIM", Some(d_oid), "AddTarget", &[PyRep::Integer(nid)], &HashMap::new(), None).await.unwrap();
            ctx.advance_secs(8);
            sys.step_manual(8.0).await.unwrap();
            let mut start_hp = None;
            for _ in 0..60 {
                let Some(cs) = sys.get_combat_state(nid).await.unwrap() else { break; };
                let hp = cs.shield_hp + cs.armor_hp + cs.structure_hp;
                if is_last && !seed_applied && hp < *start_hp.get_or_insert(hp) * 0.85 {
                    if let Some(s) = final_shot_seed { ctx.set_mechanic_rng_seed(Some(s)); seed_applied = true; }
                }
                for &gid in guns { let _ = rpc(ctx, pilot, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(nid)]).await; }
                sys.step_manual(5.0).await.unwrap();
                ctx.advance_secs(5);
            }
            assert!(sys.get_ball_snapshot(nid).await.unwrap().is_none(), "NPC {nid} must be destroyed");
        }
    }
    if let Some(s) = final_shot_seed {
        if !seed_applied { ctx.set_mechanic_rng_seed(Some(s)); }
    }
}

#[tokio::test]
async fn test_live_escalations_and_multi_step_expeditions() {
    let ctx = fresh_ctx();

    let mut gm = gm_pilot_in_space(&ctx, 51, 96_100_001, 16_242, JITA_SYS).await;
    let (mut pilot, guns, _) = provision_contract_player_in_space(&ctx, &mut gm, 52, "live_esc_player", "Esc Explorer", 16_242, true, &[], JITA_SYS).await;
    ctx.advance_secs(31);
    ok(&ctx, &mut gm, "stationSvc", "Undock", &[]).await;

    assert!(extract_rowset_rows(&ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await).1.is_empty());

    let no_esc_seed = (10_u64..10_000).find(|&s| { let r = MechanicRng::seeded(s); (0..25).all(|_| !r.roll(0.20)) }).unwrap();

    let PyRep::List(anoms) = ok(&ctx, &mut pilot, "scanSvc", "GetCombatAnomalyInstances", &[]).await else { panic!("anoms") };
    let site_id_of = |r: &PyRep| r.dict_get("siteID").or_else(|| r.dict_get("id")).and_then(|v| v.as_str()).unwrap().to_string();
    let (site1, site2) = (site_id_of(&anoms[0]), site_id_of(&anoms[1]));

    // 1. Negative RNG roll: clears combat anomaly, drops commander loot in space, zero escalation created.
    ctx.set_mechanic_rng_seed(Some(no_esc_seed));
    let jita_sys = ctx.systems.get_or_spawn(JITA_SYS);
    let before1: HashSet<i64> = jita_sys.list_balls().await.unwrap().into_iter().map(|b| b.id).collect();
    warp_to_stuff_and_clear_site(&ctx, &mut pilot, JITA_SYS, "dungeon", PyRep::String(site1.clone()), &site1, &guns, None).await;
    let after1: HashSet<i64> = jita_sys.list_balls().await.unwrap().into_iter().map(|b| b.id).collect();
    assert!(after1.difference(&before1).count() >= 1, "clearing site must drop commander loot in space");
    assert!(extract_rowset_rows(&ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await).1.is_empty());
    let _ = drain(&ctx, &mut pilot);

    // 2. Positive RNG roll (seed 3): triggers multi-step escalation & expedition with journal entry, destination, 24h expiry, & bookmark.
    warp_to_stuff_and_clear_site(&ctx, &mut pilot, JITA_SYS, "dungeon", PyRep::String(site2.clone()), &site2, &guns, Some(3)).await;
    let esc_notifs = drain(&ctx, &mut pilot);
    assert!(esc_notifs.iter().any(|(e, _)| e == "OnEscalatingPathChange"));
    assert!(esc_notifs.iter().any(|(e, _)| e == "OnEscalatingPathMessage" || e == "OnFleetEscalatingPathMessage"));

    let esc_rep = ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
    let (cols, rows) = extract_rowset_rows(&esc_rep);
    assert_eq!(cols.len(), 17);
    assert_eq!(rows.len(), 1);
    let instance_id = row_col(&cols, &rows[0], "instanceID").and_then(|v| v.as_i64()).unwrap();
    let total_steps = row_col(&cols, &rows[0], "totalSteps").and_then(|v| v.as_i64()).unwrap();
    assert!(total_steps >= 2, "expedition must have >= 2 steps");
    assert_eq!(row_col(&cols, &rows[0], "stepIndex").and_then(|v| v.as_i64()), Some(1));
    let expires_at = row_col(&cols, &rows[0], "expiresAt").and_then(|v| v.as_i64()).unwrap();
    let created_at = row_col(&cols, &rows[0], "createdAt").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(expires_at - created_at, 86_400);
    assert!(row_col(&cols, &rows[0], "journalEntryTemplateID").and_then(|v| v.as_i64()).is_some_and(|t| t > 0));

    // 3. Multi-step expedition progression (1..=total_steps): bookmark coordinates, private visibility, warp, turret combat, & completion.
    for expected_step in 1..=total_steps {
        let cur_rep = ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
        let (c_cols, c_rows) = extract_rowset_rows(&cur_rep);
        assert_eq!(row_col(&c_cols, &c_rows[0], "stepIndex").and_then(|v| v.as_i64()), Some(expected_step));
        let dest_sys = row_col(&c_cols, &c_rows[0], "destinationSolarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;

        let bm_rep = ok(&ctx, &mut pilot, "bookmark", "GetBookmarks", &[]).await;
        let (b_cols, b_rows) = extract_rowset_rows(&bm_rep);
        let bm = b_rows.iter().find(|r| row_col(&b_cols, r, "itemID").and_then(|v| v.as_i64()) == Some(instance_id)).expect("step bookmark");
        assert_eq!(row_col(&b_cols, bm, "solarSystemID").and_then(|v| v.as_i64()), Some(i64::from(dest_sys)));
        let expected_x = -45_000_000.0 + (expected_step as f64) * 5_000_000.0;
        assert!((row_col(&b_cols, bm, "x").and_then(|v| v.as_f64()).unwrap_or(0.0) - expected_x).abs() < 1.0);

        let exp_site_id = format!("EXP-{instance_id}-{expected_step}");
        if gm.sess.solar_system_id != Some(dest_sys) {
            ctx.advance_secs(12);
            slash_cmd(&ctx, &mut gm.sess, &format!("/tr {dest_sys}")).await;
        }
        let PyRep::List(other_anoms) = ok(&ctx, &mut gm, "scanSvc", "GetCombatAnomalyInstances", &[]).await else { panic!("other anoms") };
        assert!(!other_anoms.iter().any(|a| site_id_of(a) == exp_site_id), "private expedition must not be visible to other pilots");

        let dest_h = ctx.systems.get_or_spawn(dest_sys);
        let dest_before: HashSet<i64> = dest_h.list_balls().await.unwrap().into_iter().map(|b| b.id).collect();
        warp_to_stuff_and_clear_site(&ctx, &mut pilot, dest_sys, "epinstance", PyRep::Integer(instance_id), &exp_site_id, &guns, None).await;

        let step_notifs = drain(&ctx, &mut pilot);
        assert!(step_notifs.iter().any(|(e, _)| e == "OnEscalatingPathChange"));
        let entered = ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered", &[PyRep::Integer(instance_id)]).await;
        if expected_step < total_steps {
            let PyRep::List(el) = entered else { panic!("entered list") };
            assert_eq!(el.len(), expected_step as usize + 1);
        } else {
            assert!(extract_rowset_rows(&ok(&ctx, &mut pilot, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await).1.is_empty());
            let dest_after: HashSet<i64> = dest_h.list_balls().await.unwrap().into_iter().map(|b| b.id).collect();
            assert!(dest_after.difference(&dest_before).count() >= 1, "completion overseer/loot must drop in space");
            assert!(step_notifs.iter().any(|(e, _)| e == "OnEscalatingPathMessage" || e == "OnFleetEscalatingPathMessage"));
        }
    }

    // 4. Verify 24h expiry pruning when advancing clock past expiresAt.
    dock_gm_at(&ctx, &mut gm, JITA_SYS, JITA_STATION).await;
    let (gm_turret_tid, gm_ammo_tid) = find_t1_small_turret_and_ammo(&ctx.sde);
    for (sk, lvl, _) in skill_plan_for_type(&ctx.sde, gm_turret_tid) { slash_cmd(&ctx, &mut gm.sess, &format!("/giveskill {sk} {lvl}")).await; }
    let mut gm_guns = Vec::new();
    for slot in 27..33 { gm_guns.push(stock_ship_bay(&ctx, &mut gm, gm_turret_tid, 1, slot).await); }
    let ammo_id = stock_ship_bay(&ctx, &mut gm, gm_ammo_tid, 1000, 5).await;
    let gm_sid = gm.ship_id;
    ok(&ctx, &mut gm, "dogmaIM", "LoadAmmo", &[PyRep::Integer(gm_sid), PyRep::List(gm_guns.iter().map(|&g| PyRep::Integer(g)).collect()), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await;
    ctx.advance_secs(31);
    ok(&ctx, &mut gm, "stationSvc", "Undock", &[]).await;
    let step4_sys = ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == JITA_SYS).map(|j| j.to_solar_system_id).min().unwrap();
    ctx.advance_secs(12);
    slash_cmd(&ctx, &mut gm.sess, &format!("/tr {step4_sys}")).await;
    gm.beyonce = bind_oid(&ctx, &mut gm, "beyonce", PyRep::Integer(i64::from(step4_sys))).await;
    let PyRep::List(more_anoms) = ok(&ctx, &mut gm, "scanSvc", "GetCombatAnomalyInstances", &[]).await else { panic!("more anoms") };
    let a_id = site_id_of(&more_anoms[0]);
    warp_to_stuff_and_clear_site(&ctx, &mut gm, step4_sys, "dungeon", PyRep::String(a_id.clone()), &a_id, &gm_guns, Some(3)).await;
    assert_eq!(extract_rowset_rows(&ok(&ctx, &mut gm, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await).1.len(), 1);
    ctx.advance_secs(86_405);
    assert!(extract_rowset_rows(&ok(&ctx, &mut gm, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await).1.is_empty());
}

#[tokio::test]
async fn test_live_proving_grounds_validation_queue_and_sphere_hazard() {
    let ctx = fresh_ctx();

    let (fil_2v2_tid, spec_2v2, tl_2v2) = ctx.sde.types.values()
        .filter(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
        .filter_map(|t| {
            let sp = get_pvp_filament_match_spec(&ctx.sde, t.type_id);
            let tl = sp.ship_restrictions_list_id.and_then(|lid| ctx.sde.type_lists.get(&lid))?;
            (sp.fleet_size == 2 && sp.number_of_fleets == 2
                && ctx.sde.types.values().any(|s| s.published && matches!(s.group_id, 25 | 420) && tl.contains(s))
                && ctx.sde.types.values().any(|s| s.published && matches!(s.group_id, 26 | 27) && !tl.contains(s)))
                .then_some((t.type_id, sp, tl))
        })
        .min_by_key(|(tid, _, _)| *tid)
        .expect("restricted 2v2 Proving Grounds filament");
    let allowed_hull = ctx.sde.types.values().filter(|t| t.published && matches!(t.group_id, 25 | 420) && tl_2v2.contains(t)).map(|t| t.type_id).min().unwrap();
    let disallowed_hull = ctx.sde.types.values().filter(|t| t.published && matches!(t.group_id, 26 | 27) && !tl_2v2.contains(t)).map(|t| t.type_id).min().unwrap();
    let disallowed_mod = ctx.sde.types.values().filter(|t| t.published && t.category_id == 7
        && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level)
        && spec_2v2.banned_modules_list_id.and_then(|lid| ctx.sde.type_lists.get(&lid)).is_none_or(|tl| !tl.contains(t)))
        .map(|t| t.type_id).min().unwrap_or(3170);
    let disallowed_imp = ctx.sde.types.values().filter(|t| t.published && t.category_id == 20
        && t.default_attributes.get(&331).copied().is_some_and(|s| (1.0..=10.0).contains(&s))
        && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level))
        .map(|t| t.type_id).min().unwrap_or(10212);

    let mut p1 = gm_pilot_in_space(&ctx, 61, 96_200_001, allowed_hull, JITA_SYS).await;
    let (mut p2, _, p2_extras) = provision_contract_player_in_space(&ctx, &mut p1, 62, "live_pvp_p2", "Arena Wing 2", allowed_hull, false, &[fil_2v2_tid], JITA_SYS).await;
    let fil_p2 = p2_extras[0];
    for (sk, lvl, _) in skill_plan_for_type(&ctx.sde, disallowed_hull) { slash_cmd(&ctx, &mut p1.sess, &format!("/giveskill {sk} {lvl}")).await; }
    let bad_p1 = create_item_id(&ctx, &mut p1, disallowed_hull, 1).await;
    ctx.advance_secs(31);
    ok(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(bad_p1)]).await;
    p1.ship_id = bad_p1;
    ctx.advance_secs(31);
    ok(&ctx, &mut p1, "stationSvc", "Undock", &[]).await;
    if p1.sess.solar_system_id != Some(JITA_SYS) { slash_cmd(&ctx, &mut p1.sess, &format!("/tr {JITA_SYS}")).await; }
    p1.beyonce = bind_oid(&ctx, &mut p1, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await;

    let mut p3 = gm_pilot_in_space(&ctx, 63, 96_200_003, allowed_hull, JITA_SYS).await;
    let mut p4 = gm_pilot_in_space(&ctx, 64, 96_200_004, allowed_hull, JITA_SYS).await;
    let jita_h = ctx.systems.get_or_spawn(JITA_SYS);
    let safe_origin = DVec3::new(10_000_000.0, 2_000_000.0, -5_000_000.0);
    for (idx, pl) in [&mut p1, &mut p2, &mut p3, &mut p4].iter_mut().enumerate() { jita_h.set_ball_position(pl.ship_id, safe_origin + DVec3::new((idx as f64) * 500.0, 0.0, 0.0)).await; }
    ctx.advance_secs(35);
    jita_h.step_manual(32.0).await.unwrap();

    // Form Fleet A (p1 boss, p2 member) & test JoinPVPQueue restrictions.
    let fleet_a = ok(&ctx, &mut p1, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().unwrap();
    p1.sess.fleet_id = Some(fleet_a);
    let fa_oid = bind_oid(&ctx, &mut p1, "fleetObjectHandler", PyRep::Integer(fleet_a)).await;
    ctx.dispatch_rpc(&mut p1.sess, "fleetObjectHandler", Some(fa_oid), "Invite", &[PyRep::Integer(p2.char_id), PyRep::None, PyRep::None, PyRep::Integer(4)], &HashMap::new(), None).await.unwrap();
    ok(&ctx, &mut p2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_a)]).await;
    p2.sess.fleet_id = Some(fleet_a);
    ctx.advance_secs(12);

    assert_eq!(rpc(&ctx, &mut p2, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p2), PyRep::Integer(i64::from(fil_2v2_tid))]).await.unwrap_err().code, "NotFleetBoss");
    let fil_p1 = stock_ship_bay(&ctx, &mut p1, fil_2v2_tid, 1, 5).await;
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1), PyRep::Integer(i64::from(fil_2v2_tid))]).await.unwrap_err().code, "InvalidShipForFilament");

    // Re-ship p1 into allowed_hull, test module meta tier, implant injection, and fleet > 30km range checks.
    dock_gm_at(&ctx, &mut p1, JITA_SYS, JITA_STATION).await;
    let good_p1 = create_item_id(&ctx, &mut p1, allowed_hull, 1).await;
    for (sk, lvl, _) in skill_plan_for_type(&ctx.sde, allowed_hull) {
        slash_cmd(&ctx, &mut p1.sess, &format!("/giveskill {sk} {lvl}")).await;
    }
    ctx.advance_secs(31);
    ok(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(good_p1)]).await;
    p1.ship_id = good_p1;
    let fil_p1_b = stock_ship_bay(&ctx, &mut p1, fil_2v2_tid, 1, 5).await;
    ctx.advance_secs(31);
    ok(&ctx, &mut p1, "stationSvc", "Undock", &[]).await;
    jita_h.set_ball_position(p1.ship_id, safe_origin).await;
    ctx.advance_secs(35);
    jita_h.step_manual(32.0).await.unwrap();

    let bad_mod_id = stock_ship_bay(&ctx, &mut p1, disallowed_mod, 1, 5).await;
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1_b), PyRep::Integer(i64::from(fil_2v2_tid))]).await.unwrap_err().code, "InvalidModuleTierForFilament");
    dock_gm_at(&ctx, &mut p1, JITA_SYS, JITA_STATION).await;
    ok(&ctx, &mut p1, "invbroker", "Add", &[PyRep::Integer(bad_mod_id), PyRep::Integer(JITA_STATION), PyRep::Integer(4)]).await;

    for (sk, lvl, _) in skill_plan_for_type(&ctx.sde, disallowed_imp) { slash_cmd(&ctx, &mut p1.sess, &format!("/giveskill {sk} {lvl}")).await; }
    let bad_imp_id = create_item_id(&ctx, &mut p1, disallowed_imp, 1).await;
    ok(&ctx, &mut p1, "dogmaIM", "InjectImplant", &[PyRep::Integer(bad_imp_id)]).await;
    ctx.advance_secs(31);
    ok(&ctx, &mut p1, "stationSvc", "Undock", &[]).await;
    jita_h.set_ball_position(p1.ship_id, safe_origin).await;
    ctx.advance_secs(35);
    jita_h.step_manual(32.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1_b), PyRep::Integer(i64::from(fil_2v2_tid))]).await.unwrap_err().code, "InvalidImplantsForFilament");
    ok(&ctx, &mut p1, "dogmaIM", "DestroyImplant", &[PyRep::Integer(bad_imp_id)]).await;

    jita_h.set_ball_position(p2.ship_id, safe_origin + DVec3::new(40_000.0, 0.0, 0.0)).await;
    jita_h.step_manual(0.1).await.unwrap();
    assert_eq!(rpc(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1_b), PyRep::Integer(i64::from(fil_2v2_tid))]).await.unwrap_err().code, "FleetMemberOutOfRange");
    jita_h.set_ball_position(p2.ship_id, safe_origin + DVec3::new(500.0, 0.0, 0.0)).await;

    // Test LeavePVPQueue then match Fleet A vs Fleet B in isolated arena pocket (32_000_001).
    ok(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1_b), PyRep::Integer(i64::from(fil_2v2_tid))]).await;
    ok(&ctx, &mut p1, "pvpFilamentMgr", "LeavePVPQueue", &[]).await;
    let fil_p1_c = match ctx.db.with_snapshot(|s| s.items.values().find(|it| it.location_id == p1.ship_id && it.type_id == fil_2v2_tid).map(|it| it.item_id)) {
        Some(existing) => existing,
        None => stock_ship_bay(&ctx, &mut p1, fil_2v2_tid, 1, 5).await,
    };
    ok(&ctx, &mut p1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p1_c), PyRep::Integer(i64::from(fil_2v2_tid))]).await;

    let fleet_b = ok(&ctx, &mut p3, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().unwrap();
    p3.sess.fleet_id = Some(fleet_b);
    let fb_oid = bind_oid(&ctx, &mut p3, "fleetObjectHandler", PyRep::Integer(fleet_b)).await;
    ctx.dispatch_rpc(&mut p3.sess, "fleetObjectHandler", Some(fb_oid), "Invite", &[PyRep::Integer(p4.char_id), PyRep::None, PyRep::None, PyRep::Integer(4)], &HashMap::new(), None).await.unwrap();
    ok(&ctx, &mut p4, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_b)]).await;
    p4.sess.fleet_id = Some(fleet_b);
    ctx.advance_secs(12);
    let fil_p3 = stock_ship_bay(&ctx, &mut p3, fil_2v2_tid, 1, 5).await;
    let join_rep = ok(&ctx, &mut p3, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fil_p3), PyRep::Integer(i64::from(fil_2v2_tid))]).await;
    assert_eq!(join_rep, PyRep::Integer(5));

    for pl in [&mut p1, &mut p2, &mut p3, &mut p4] {
        let notifs = drain(&ctx, pl);
        assert!(notifs.iter().any(|(e, _)| e == "OnMatchFound" || e == "OnProvingGroundsMatchFound"));
        assert!(notifs.iter().any(|(e, _)| e == "OnAddExpiryDurationForAbyssalPvpContent"));
    }

    let arena_sys = ctx.systems.get_or_spawn(32_000_001);
    let p1_pos = arena_sys.get_ball_snapshot(p1.ship_id).await.unwrap().unwrap().1.position;
    let p3_pos = arena_sys.get_ball_snapshot(p3.ship_id).await.unwrap().unwrap().1.position;
    let center = (p1_pos + p3_pos) * 0.5;
    assert!(p1_pos.distance(center) < 75_000.0 && p3_pos.distance(center) < 75_000.0);

    // Concurrent 2nd match spatial isolation check.
    if let Some(fil_1v1) = ctx.sde.types.values().filter(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
        .find(|t| {
            let sp = get_pvp_filament_match_spec(&ctx.sde, t.type_id);
            sp.fleet_size == 1
                && sp.number_of_fleets == 2
                && sp.ship_restrictions_list_id.is_none_or(|lid| ctx.sde.type_lists.get(&lid).is_some_and(|tl| tl.contains(&ctx.sde.types[&allowed_hull])))
        }).map(|t| t.type_id)
    {
        let mut p5 = gm_pilot_in_space(&ctx, 65, 96_200_005, allowed_hull, JITA_SYS).await;
        let mut p6 = gm_pilot_in_space(&ctx, 66, 96_200_006, allowed_hull, JITA_SYS).await;
        jita_h.set_ball_position(p5.ship_id, safe_origin + DVec3::new(1_000.0, 0.0, 0.0)).await;
        jita_h.set_ball_position(p6.ship_id, safe_origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
        jita_h.step_manual(32.0).await.unwrap();
        let (f5, f6) = (stock_ship_bay(&ctx, &mut p5, fil_1v1, 1, 5).await, stock_ship_bay(&ctx, &mut p6, fil_1v1, 1, 5).await);
        ok(&ctx, &mut p5, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f5), PyRep::Integer(i64::from(fil_1v1))]).await;
        ok(&ctx, &mut p6, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f6), PyRep::Integer(i64::from(fil_1v1))]).await;
        assert!(arena_sys.get_ball_snapshot(p5.ship_id).await.unwrap().unwrap().1.position.distance(center) > 1_000_000.0);
    }

    // Fly p3 & p4 outside 75 km boundary sphere -> escalating damage -> hull destruction + pod ejection -> winner extraction & pod destruction!
    let outside_pt = center + DVec3::new(82_000.0, 0.0, 0.0);
    p3.beyonce = bind_oid(&ctx, &mut p3, "beyonce", PyRep::Integer(32_000_001)).await;
    p4.beyonce = bind_oid(&ctx, &mut p4, "beyonce", PyRep::Integer(32_000_001)).await;
    ok(&ctx, &mut p3, "beyonce", "CmdGotoPoint", &[PyRep::Real(outside_pt.x), PyRep::Real(outside_pt.y), PyRep::Real(outside_pt.z)]).await;
    ok(&ctx, &mut p4, "beyonce", "CmdGotoPoint", &[PyRep::Real(outside_pt.x), PyRep::Real(outside_pt.y + 1_000.0), PyRep::Real(outside_pt.z)]).await;

    let (mut dmg_steps, mut prev_hp, old_p3, old_p4, mut p3_pod) = (Vec::new(), None, p3.ship_id, p4.ship_id, None);
    for _ in 0..160 {
        arena_sys.step_manual(2.0).await.unwrap();
        ctx.advance_secs(2);
        if let Some((_, b)) = arena_sys.get_ball_snapshot(old_p3).await.unwrap() {
            if b.position.distance(center) > 75_000.0 {
                if let Some(cs) = arena_sys.get_combat_state(old_p3).await.unwrap() {
                    let hp = cs.shield_hp + cs.armor_hp + cs.structure_hp;
                    if let Some(p) = prev_hp { if p - hp > 1.0 && dmg_steps.len() < 4 { dmg_steps.push(p - hp); } }
                    prev_hp = Some(hp);
                }
            }
        } else if p3_pod.is_none() {
            p3_pod = arena_sys.list_balls().await.unwrap().into_iter().find(|b| b.type_id == TYPE_CAPSULE && b.position.distance(center) > 75_000.0).map(|b| b.id);
        }
        if arena_sys.get_ball_snapshot(old_p3).await.unwrap().is_none() && arena_sys.get_ball_snapshot(old_p4).await.unwrap().is_none() { break; }
    }
    assert!(dmg_steps.len() >= 2 && dmg_steps[1] > dmg_steps[0], "escalating sphere hazard damage: {dmg_steps:?}");
    let ejected_pod = p3_pod.expect("ship destruction outside sphere must eject Capsule (670)");
    assert!(arena_sys.get_ball_snapshot(ejected_pod).await.unwrap().is_some());
    let end_notifs = drain(&ctx, &mut p1);
    assert!(end_notifs.iter().any(|(e, _)| e == "OnPVPFilamentsCharacterStatistics") && end_notifs.iter().any(|(e, _)| e == "OnPVPFilamentsLeaderboard"));
    assert!(!matches!(ok(&ctx, &mut p1, "pvpFilamentMgr", "GetLeaderboard", &[]).await, PyRep::None));

    let gate_id = arena_sys.list_balls().await.unwrap().into_iter().filter(|b| b.position.distance(center) < 75_000.0 && b.id != p1.ship_id && b.id != p2.ship_id).map(|b| b.id).min().unwrap();
    arena_sys.set_ball_position(p1.ship_id, center + DVec3::new(500.0, 0.0, 0.0)).await;
    ok(&ctx, &mut p1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(gate_id)]).await;
    assert_eq!(p1.sess.solar_system_id, Some(JITA_SYS));
    for _ in 0..25 {
        arena_sys.step_manual(2.0).await.unwrap();
        ctx.advance_secs(2);
        if arena_sys.get_ball_snapshot(ejected_pod).await.unwrap().is_none() { break; }
    }
    assert!(arena_sys.get_ball_snapshot(ejected_pod).await.unwrap().is_none());
    let _ = ok(&ctx, &mut p3, "account", "GetCashBalance", &[PyRep::Bool(false)]).await;
    assert!(p3.sess.station_id.is_some() && p3.sess.ship_id != Some(ejected_pod));
}

#[tokio::test]
async fn test_live_proving_grounds_arena_timer_expiry_destruction() {
    let ctx = fresh_ctx();

    let (fil_1v1_tid, spec_1v1) = ctx.sde.types.values().filter(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
        .filter_map(|t| { let sp = get_pvp_filament_match_spec(&ctx.sde, t.type_id); (sp.fleet_size == 1 && sp.number_of_fleets == 2).then_some((t.type_id, sp)) })
        .min_by_key(|(id, _)| *id).expect("1v1 Proving Grounds filament");
    let hull_1v1 = spec_1v1.ship_restrictions_list_id.and_then(|lid| ctx.sde.type_lists.get(&lid))
        .and_then(|tl| ctx.sde.types.values().filter(|t| t.published && tl.contains(t)).map(|t| t.type_id).min()).unwrap_or(TYPE_ATRON);

    let mut c1 = gm_pilot_in_space(&ctx, 71, 96_300_001, hull_1v1, JITA_SYS).await;
    let mut c2 = gm_pilot_in_space(&ctx, 72, 96_300_002, hull_1v1, JITA_SYS).await;
    let jita_h = ctx.systems.get_or_spawn(JITA_SYS);
    let origin = DVec3::new(15_000_000.0, 0.0, 0.0);
    jita_h.set_ball_position(c1.ship_id, origin).await;
    jita_h.set_ball_position(c2.ship_id, origin + DVec3::new(500.0, 0.0, 0.0)).await;
    jita_h.step_manual(0.1).await.unwrap();

    let (f1, f2) = (stock_ship_bay(&ctx, &mut c1, fil_1v1_tid, 1, 5).await, stock_ship_bay(&ctx, &mut c2, fil_1v1_tid, 1, 5).await);
    ok(&ctx, &mut c1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f1), PyRep::Integer(i64::from(fil_1v1_tid))]).await;
    ok(&ctx, &mut c2, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(f2), PyRep::Integer(i64::from(fil_1v1_tid))]).await;

    let arena_sys = ctx.systems.get_or_spawn(32_000_001);
    let old_ship1 = c1.ship_id;
    let cs_before = arena_sys.get_combat_state(old_ship1).await.unwrap().unwrap();
    let hp_before = cs_before.shield_hp + cs_before.armor_hp + cs_before.structure_hp;

    arena_sys.step_manual(1.0).await.unwrap();
    let cs_early = arena_sys.get_combat_state(old_ship1).await.unwrap().unwrap();
    assert!((cs_early.shield_hp + cs_early.armor_hp + cs_early.structure_hp - hp_before).abs() < 1e-6);

    ctx.advance_secs(902);
    let mut pod_id_opt = None;
    for _ in 0..30 {
        arena_sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
        if arena_sys.get_ball_snapshot(old_ship1).await.unwrap().is_none() {
            pod_id_opt = arena_sys.list_balls().await.unwrap().into_iter().filter(|b| b.type_id == TYPE_CAPSULE).map(|b| b.id).max();
            break;
        }
    }
    let pod_id = pod_id_opt.expect("timer expiry must destroy ship and eject Capsule (670)");
    assert!(arena_sys.get_ball_snapshot(pod_id).await.unwrap().is_some());

    for _ in 0..15 {
        arena_sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
        if arena_sys.get_ball_snapshot(pod_id).await.unwrap().is_none() { break; }
    }
    assert!(arena_sys.get_ball_snapshot(pod_id).await.unwrap().is_none());
    let _ = ok(&ctx, &mut c1, "account", "GetCashBalance", &[PyRep::Bool(false)]).await;
    assert!(c1.sess.station_id.is_some() && c1.sess.ship_id != Some(pod_id));
}
