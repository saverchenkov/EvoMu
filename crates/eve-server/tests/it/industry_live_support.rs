//! Shared live player-flow helpers for the industry integration suites. Industrialists are ordinary
//! (non-GM) pilots: ISK arrives via a GM treasury `account.GiveCash` transfer (incidental funding),
//! skills via `skillMgr2` queue training, every blueprint/material/structure item is bought on the
//! Jita market, hauled in a freighter, travelled by stargate and stood up with
//! `structureDeployment.Anchor` / `invbroker.Add` fitting. No world-state injection.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) const PILOT_1: i64 = 90_000_001;
pub(crate) const PILOT_2: i64 = 90_000_002;
pub(crate) const JITA_STATION: i64 = 60_003_760;
pub(crate) const PERIMETER_SYS: u32 = 30_000_144;
const FREIGHTER: u32 = 20_185;
/// Racial fuel blocks (Caldari, Minmatar, Amarr, Gallente); each Jita seller lists 1,000 units.
const FUEL_BLOCKS: [u32; 4] = [4_051, 4_246, 4_247, 4_312];

pub(crate) async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

pub(crate) fn int(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing int {key} in {rep:?}"))
}

pub(crate) fn real(rep: &PyRep, key: &str) -> f64 {
    rep.dict_get(key).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("missing real {key} in {rep:?}"))
}

pub(crate) async fn login(ctx: &ServerContext, sess_id: u64, user: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, &format!("{user}-secret")).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

/// Incidental funding: an ISD treasury GM wires ISK to the (non-GM) industrialists via `account.GiveCash`.
pub(crate) async fn fund_pilots(ctx: &ServerContext, targets: &[(i64, f64)]) {
    ctx.db.provision_gm_account("isd_treasury", "isd_treasury-secret", 90_000_099, "ISD Treasury");
    let mut gm = login(ctx, 7_999, "isd_treasury", 90_000_099).await;
    rpc(ctx, &mut gm, "slash", "SlashCmd", &[PyRep::String("/giveisk 900000000000".into())]).await.expect("GM /giveisk");
    for &(cid, amount) in targets {
        rpc(ctx, &mut gm, "account", "GiveCash", &[PyRep::Integer(cid), PyRep::Real(amount), PyRep::String("industry-grant".into())])
            .await
            .expect("GM GiveCash");
    }
}

fn collect_skill_topo(sde: &eve_sde::SdeRegistry, tid: u32, seen: &mut HashSet<u32>, out: &mut Vec<u32>) {
    if !seen.insert(tid) {
        return;
    }
    if let Some(t) = sde.types.get(&tid) {
        for attr in [182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&req) = t.default_attributes.get(&attr).filter(|v| **v > 0.0) {
                collect_skill_topo(sde, req as u32, seen, out);
            }
        }
        if t.category_id == 16 {
            out.push(tid);
        }
    }
}

/// Trains every skill required by `type_ids` (skills, ships, modules) to level V through the live skill queue.
/// Alpha clones stop at their cap (`strict = false` tolerates the refusal).
pub(crate) async fn train_skills(ctx: &ServerContext, sess: &mut SessionState, type_ids: &[u32], strict: bool) {
    let char_id = sess.character_id.unwrap();
    let (mut seen, mut ordered) = (HashSet::new(), Vec::new());
    for &tid in type_ids {
        collect_skill_topo(&ctx.sde, tid, &mut seen, &mut ordered);
    }
    for sid in ordered {
        let curr = ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
        if curr >= 5 {
            continue;
        }
        let _ = rpc(ctx, sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(i64::from(sid))])]).await;
        // Alpha clones cap some skills below V: fall back to the highest level the clone accepts.
        let mut queued = false;
        for top in ((curr + 1)..=5).rev() {
            let q: Vec<PyRep> = ((curr + 1)..=top).map(|l| PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(l))])).collect();
            if rpc(ctx, sess, "skillMgr2", "SaveNewQueue", &[PyRep::List(q)]).await.is_ok() {
                queued = true;
                break;
            }
        }
        assert!(queued || !strict, "SaveNewQueue skill {sid} failed");
        ctx.advance_virtual_clock(90 * 86_400);
        let _ = rpc(ctx, sess, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await;
    }
}

/// Unlocked stacks (job-locked blueprints excluded) of `type_id` owned by `owner` directly inside `loc`.
pub(crate) fn items_at(ctx: &ServerContext, owner: i64, loc: i64, type_id: u32) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == owner && i.location_id == loc && i.type_id == type_id && i.flag != eve_db::InventoryFlag::Locked).map(|i| (i.item_id, i64::from(i.quantity))).collect()
    });
    v.sort_unstable();
    v
}

pub(crate) fn owned_qty(ctx: &ServerContext, owner: i64, loc: i64, type_id: u32) -> i64 {
    items_at(ctx, owner, loc, type_id).iter().map(|x| x.1).sum()
}

/// Buys `qty` of `type_id` from Jita sell orders (pilot docked in Jita 4-4); returns the new hangar item IDs.
pub(crate) async fn buy(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i64) -> Vec<i64> {
    let owner = sess.character_id.unwrap();
    let before: HashSet<i64> = items_at(ctx, owner, JITA_STATION, type_id).into_iter().map(|x| x.0).collect();
    let ask = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| o.station_id == JITA_STATION && !o.is_bid && o.type_id == type_id && o.vol_remaining > 0)
            .map(|o| o.price.to_wire_f64())
            .fold(0.0_f64, f64::max)
    });
    assert!(ask > 0.0, "no Jita sell order for type {type_id}");
    // Round the limit price up to the 4-significant-figure market tick.
    let tick = 10_f64.powi(ask.log10().floor() as i32 - 3).max(0.01);
    let ask = ((ask / tick).ceil() * tick * 100.0).round() / 100.0;
    let args = [
        PyRep::Integer(JITA_STATION),
        PyRep::Integer(i64::from(type_id)),
        PyRep::Real(ask),
        PyRep::Integer(qty),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Bool(false),
    ];
    rpc(ctx, sess, "marketProxy", "PlaceBuyOrder", &args).await.unwrap_or_else(|e| panic!("buy {type_id} x{qty}: {e:?}"));
    items_at(ctx, owner, JITA_STATION, type_id).into_iter().map(|x| x.0).filter(|id| !before.contains(id)).collect()
}

/// Uses hangar stock in Jita first and buys only the shortfall; returns every Jita stack of `type_id`.
pub(crate) async fn acquire(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i64) -> Vec<i64> {
    let owner = sess.character_id.unwrap();
    let have = owned_qty(ctx, owner, JITA_STATION, type_id);
    if have < qty {
        buy(ctx, sess, type_id, qty - have).await;
    }
    items_at(ctx, owner, JITA_STATION, type_id).into_iter().map(|x| x.0).collect()
}

/// Manufacturing bill of materials for `runs` runs of `bp_tid`.
pub(crate) fn bom(ctx: &ServerContext, bp_tid: u32, runs: i64) -> Vec<(u32, i64)> {
    ctx.sde.blueprints[&bp_tid].materials.iter().map(|m| (m.type_id, m.quantity as i64 * runs)).collect()
}

/// Moves `qty` units of `stack` into `dest`/`flag`, returning the item ID now holding them at `dest`.
pub(crate) async fn split_off(ctx: &ServerContext, sess: &mut SessionState, stack: i64, dest: i64, flag: i64, qty: i64) -> i64 {
    // Any owner: moving into a corp office hands the stack to the corporation.
    let at_dest = |tid: u32| -> Vec<i64> {
        let mut v: Vec<i64> = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == dest && i.type_id == tid).map(|i| i.item_id).collect());
        v.sort_unstable();
        v
    };
    let tid = ctx.db.with_snapshot(|s| s.items[&stack].type_id);
    let before: HashSet<i64> = at_dest(tid).into_iter().collect();
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(stack), PyRep::Integer(dest), PyRep::Integer(flag), PyRep::Integer(qty)])
        .await
        .unwrap_or_else(|e| panic!("split {stack} -> {dest}: {e:?}"));
    let after = at_dest(tid);
    after.iter().copied().find(|id| !before.contains(id)).unwrap_or_else(|| if after.contains(&stack) { stack } else { after[0] })
}

/// Cheapest-to-haul market blueprint: a published module whose per-run materials are all minerals
/// (`34..=40`) of at most 60 units each (fits a frigate hold; Jita lists 1,000 per mineral).
pub(crate) fn small_mineral_bp(ctx: &ServerContext) -> u32 {
    *ctx.sde
        .blueprints
        .iter()
        .filter(|(tid, b)| {
            ctx.sde.types.get(tid).is_some_and(|t| t.published && t.market_group_id.is_some())
                && ctx.sde.types.get(&b.product_type_id).is_some_and(|p| p.published && p.category_id == 7)
                && !b.materials.is_empty()
                && b.materials.iter().all(|m| (34..=40).contains(&m.type_id) && m.quantity <= 60)
                && b.manufacturing_time_sec >= 300
        })
        .map(|(tid, _)| tid)
        .min()
        .expect("small mineral-only module blueprint")
}

pub(crate) async fn move_item(ctx: &ServerContext, sess: &mut SessionState, item: i64, dest: i64, flag: i64) {
    let qty = ctx.db.with_snapshot(|s| i64::from(s.items[&item].quantity));
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(item), PyRep::Integer(dest), PyRep::Integer(flag), PyRep::Integer(qty)])
        .await
        .unwrap_or_else(|e| panic!("move {item} -> {dest}/{flag}: {e:?}"));
}

/// Buys, assembles and boards a Charon in Jita, then acquires `cargo` (hangar stock + market) into its hold. Returns the freighter ID.
pub(crate) async fn board_loaded_freighter(ctx: &ServerContext, sess: &mut SessionState, cargo: &[(u32, i64)]) -> i64 {
    let freighter = buy(ctx, sess, FREIGHTER, 1).await[0];
    rpc(ctx, sess, "ship", "AssembleShip", &[PyRep::Integer(freighter)]).await.expect("AssembleShip Charon");
    ctx.advance_secs(12);
    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(freighter)]).await.expect("Board Charon");
    for &(tid, qty) in cargo {
        for item in acquire(ctx, sess, tid, qty).await {
            move_item(ctx, sess, item, freighter, 5).await;
        }
    }
    freighter
}

pub(crate) async fn install(ctx: &ServerContext, sess: &mut SessionState, bp: i64, facility: i64, activity: i64, runs: i64, extra: &[(&str, PyRep)]) -> Result<PyRep, BridgeRpcError> {
    let mut kv = vec![
        ("blueprintID", PyRep::Integer(bp)),
        ("facilityID", PyRep::Integer(facility)),
        ("activityID", PyRep::Integer(activity)),
        ("runs", PyRep::Integer(runs)),
    ];
    kv.extend(extra.iter().cloned());
    rpc(ctx, sess, "industryManager", "InstallJob", &[PyRep::keyval(kv)]).await
}

async fn settle(ctx: &ServerContext, sess: &SessionState, max_steps: usize) {
    let (sys, ship) = (sess.solar_system_id.unwrap(), sess.ship_id.unwrap());
    let h = ctx.systems.get_or_spawn(sys);
    for i in 0..max_steps {
        let _ = h.step_manual(1.0).await;
        let warping = matches!(h.get_ball_snapshot(ship).await, Ok(Some((_, ref b))) if matches!(b.mode, eve_destiny::MovementMode::Warp { .. }));
        if i > 20 && !warping {
            break;
        }
    }
    ctx.advance_secs(15);
}

pub(crate) async fn warp_to(ctx: &ServerContext, sess: &mut SessionState, target: i64, dist: f64) {
    match rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target), PyRep::Real(dist)]).await {
        Ok(_) => settle(ctx, sess, 4000).await,
        Err(e) if e.code == "WarpDistanceTooShort" => {}
        Err(e) => panic!("warp to {target}: {e:?}"),
    }
}

async fn undock(ctx: &ServerContext, sess: &mut SessionState) {
    if sess.structure_id.is_some() {
        rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("structure Undock");
    } else if sess.station_id.is_some() {
        rpc(ctx, sess, "stationSvc", "Undock", &[]).await.expect("station Undock");
    }
    ctx.advance_secs(15);
}

/// Shortest stargate route (gate IDs) between two systems over SDE jumps.
fn route(ctx: &ServerContext, from: u32, to: u32) -> Vec<i64> {
    let mut prev: HashMap<u32, (u32, i64)> = HashMap::new();
    let mut q = VecDeque::from([from]);
    let mut seen = HashSet::from([from]);
    while let Some(sys) = q.pop_front() {
        if sys == to {
            break;
        }
        let mut gates: Vec<(&i64, &eve_sde::StargateJump)> = ctx.sde.jumps.iter().filter(|(_, j)| j.from_solar_system_id == sys).collect();
        gates.sort_by_key(|g| *g.0);
        for (&gid, j) in gates {
            if seen.insert(j.to_solar_system_id) {
                prev.insert(j.to_solar_system_id, (sys, gid));
                q.push_back(j.to_solar_system_id);
            }
        }
    }
    let mut path = Vec::new();
    let mut cur = to;
    while cur != from {
        let (p, gid) = prev[&cur];
        path.push(gid);
        cur = p;
    }
    path.reverse();
    path
}

/// Nearest system (by stargate jumps from `from`) satisfying `pred`.
pub(crate) fn nearest_system(ctx: &ServerContext, from: u32, pred: impl Fn(u32) -> bool) -> u32 {
    let mut q = VecDeque::from([from]);
    let mut seen = HashSet::from([from]);
    while let Some(sys) = q.pop_front() {
        if pred(sys) {
            return sys;
        }
        let mut next: Vec<u32> = ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == sys).map(|j| j.to_solar_system_id).collect();
        next.sort_unstable();
        for n in next {
            if seen.insert(n) {
                q.push_back(n);
            }
        }
    }
    panic!("no reachable system matches");
}

/// Undocks and flies the active ship by stargate to `dest_sys`.
pub(crate) async fn travel(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    undock(ctx, sess).await;
    for gate in route(ctx, sess.solar_system_id.unwrap(), dest_sys) {
        warp_to(ctx, sess, gate, 0.0).await;
        rpc(ctx, sess, "beyonce", "CmdStargateJump", &[PyRep::Integer(gate)]).await.unwrap_or_else(|e| panic!("jump {gate}: {e:?}"));
        ctx.advance_secs(15);
    }
    assert_eq!(sess.solar_system_id, Some(dest_sys));
}

pub(crate) async fn dock_station(ctx: &ServerContext, sess: &mut SessionState, station_id: i64) {
    travel(ctx, sess, ctx.sde.stations[&station_id].solar_system_id).await;
    warp_to(ctx, sess, station_id, 0.0).await;
    rpc(ctx, sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await.unwrap_or_else(|e| panic!("Dock {station_id}: {e:?}"));
    ctx.advance_secs(15);
}

pub(crate) async fn dock_structure(ctx: &ServerContext, sess: &mut SessionState, structure_id: i64) {
    let sys = ctx.db.with_snapshot(|s| s.structures[&structure_id].solar_system_id);
    if sess.solar_system_id != Some(sys) || sess.station_id.is_some() || sess.structure_id.is_some() {
        travel(ctx, sess, sys).await;
        warp_to(ctx, sess, structure_id, 0.0).await;
    }
    let PyRep::Tuple(bound) = rpc(ctx, sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(sys)), PyRep::None]).await.expect("bind beyonce") else {
        panic!("MachoBindObject tuple");
    };
    let bound_id = bound[0].as_i64().unwrap() as u64;
    ctx.dispatch_rpc(sess, "beyonce", Some(bound_id), "CmdFollowBall", &[PyRep::Integer(structure_id), PyRep::Real(0.0)], &HashMap::new(), None)
        .await
        .expect("Approach structure");
    ctx.dispatch_rpc(sess, "beyonce", Some(bound_id), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None)
        .await
        .expect("Full speed");
    let h = ctx.systems.get_or_spawn(sys);
    for attempt in 0..80 {
        for _ in 0..15 {
            let _ = h.step_manual(1.0).await;
        }
        ctx.advance_secs(15);
        match rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await {
            Ok(_) => break,
            Err(e) if e.code == "OutsideDockingPerimeter" && attempt < 79 => continue,
            Err(e) => panic!("Dock {structure_id}: {e:?}"),
        }
    }
    ctx.advance_secs(15);
}

/// Freighter cargo manifest (hull, core, service modules) needed to stand up `hull_tid`.
pub(crate) fn upwell_cargo(ctx: &ServerContext, hull_tid: u32, services: &[u32]) -> Vec<(u32, i64)> {
    let core = ctx.sde.types[&hull_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let mut v = vec![(hull_tid, 1), (core, 1)];
    v.extend(services.iter().map(|&s| (s, 1)));
    v
}

/// The entire Jita fuel-block stock (1,000 of each racial block).
pub(crate) fn fuel_cargo() -> Vec<(u32, i64)> {
    FUEL_BLOCKS.iter().map(|&f| (f, 1_000)).collect()
}

/// Flies the loaded freighter to `sys_id`, anchors `hull_tid` near the sun, inserts core + fuel, onlines
/// it, docks, and fits `services` (each onlined) into service slots 0... Returns `(structure, fitted)`.
pub(crate) async fn deploy_online_upwell(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, hull_tid: u32, services: &[u32], fuel_qty: i64) -> (i64, Vec<i64>) {
    let owner = sess.character_id.unwrap();
    travel(ctx, sess, sys_id).await;
    let sun = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
    warp_to(ctx, sess, sun, 100_000.0).await;
    let anc = rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(i64::from(hull_tid)), PyRep::String("Live Industry Complex".into())])
        .await
        .expect("Anchor Upwell structure");
    let st_id = int(&anc, "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;
    dock_structure(ctx, sess, st_id).await;
    let ship = sess.ship_id.unwrap();
    let core = ctx.sde.types[&hull_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let core_item = items_at(ctx, owner, ship, core)[0].0;
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(st_id), PyRep::Integer(172)]).await.expect("Insert Quantum Core");
    let mut fuel_left = fuel_qty;
    for (item, qty) in FUEL_BLOCKS.iter().flat_map(|&f| items_at(ctx, owner, ship, f)) {
        let take = qty.min(fuel_left);
        if take > 0 {
            rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(take)])
                .await
                .expect("Load Fuel Bay");
            fuel_left -= take;
        }
    }
    assert_eq!(fuel_left, 0, "freighter must carry {fuel_qty} fuel blocks");
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock for onlining");
    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    dock_structure(ctx, sess, st_id).await;
    let mut fitted = Vec::new();
    for (idx, &stid) in services.iter().enumerate() {
        let item = items_at(ctx, owner, ship, stid)[0].0;
        let fit = rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(164 + idx as i64)])
            .await
            .expect("Fit & online service module");
        assert_eq!(fit.dict_get("online").and_then(|v| v.as_bool()), Some(true));
        fitted.push(int(&fit, "itemID"));
    }
    (st_id, fitted)
}
