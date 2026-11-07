//! Clean end-to-end live integration tests for Excavator Drones, Mining Goals,
//! Mineral Market Supply Chain, Mobile Phase Anchor, Phased Asteroid Field Lifecycle,
//! and Disruptive Lance (`feat:mining.*`, `feat:modules.disruptive-lance`).

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use crate::quasar_progression_complete::quasar_rpc;
use eve_destiny::DVec3;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::collections::HashMap;

const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const NULL_SYS: u32 = 30_004_759; // 1DQ1-A
const LOW_SYS: u32 = 30_002_813; // Tama
const HIGH_SLOT_0: i64 = 27;
const HIGH_SLOT_1: i64 = 28;
const LOW_SLOT_0: i64 = 11;
const DRONE_BAY: i64 = 87;
const ORE_HOLD: i64 = 134;
const FLEET_HANGAR: i64 = 155;

fn paused_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx
}

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn ok(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    rpc(ctx, sess, svc, method, args).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn gm_pilot_fitted_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    extra_skills: &[(u32, u8)],
    modules: &[(u32, i64, Option<(u32, i32)>)],
) -> (LivePilot, Vec<i64>) {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    ok(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[int(char_id)]).await;

    let ship_id = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let mut stack = vec![ship_type_id];
    for &(mtid, _, c_opt) in modules {
        stack.push(mtid);
        if let Some((ctid, _)) = c_opt {
            stack.push(ctid);
        }
    }
    let mut visited = std::collections::HashSet::new();
    let mut ordered_skills = Vec::new();
    fn visit_prereqs(
        sde: &eve_sde::SdeRegistry,
        tid: u32,
        visited: &mut std::collections::HashSet<u32>,
        out: &mut Vec<u32>,
    ) {
        if !visited.insert(tid) {
            return;
        }
        if let Some(t) = sde.types.get(&tid) {
            for a in [182_u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&s) = t.default_attributes.get(&a) {
                    let sid = s.round() as u32;
                    if sid > 0 {
                        visit_prereqs(sde, sid, visited, out);
                    }
                }
            }
            if t.category_id == 16 {
                out.push(tid);
            }
        }
    }
    for tid in stack {
        visit_prereqs(&ctx.sde, tid, &mut visited, &mut ordered_skills);
    }
    for sid in ordered_skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for &(sid, lvl) in extra_skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} {lvl}")).await;
    }
    ctx.advance_secs(31);
    ok(ctx, &mut sess, "ship", "Board", &[int(ship_id)]).await;

    let mut fitted_ids = Vec::with_capacity(modules.len());
    for &(mtid, flag, charge_opt) in modules {
        let mid = common::slash_cmd(ctx, &mut sess, &format!("/createitem {mtid} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ok(ctx, &mut sess, "invbroker", "Add", &[int(mid), int(ship_id), int(flag)]).await;
        if let Some((ctid, cqty)) = charge_opt {
            let cid = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ctid} {cqty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
            ok(ctx, &mut sess, "dogmaIM", "LoadAmmo", &[int(ship_id), PyRep::List(vec![int(mid)]), PyRep::List(vec![int(cid)])]).await;
        }
        fitted_ids.push(mid);
    }
    if !fitted_ids.is_empty() {
        ok(ctx, &mut sess, "dogmaIM", "ShipOnlineModules", &[int(ship_id)]).await;
    }
    ctx.advance_secs(31);
    ok(ctx, &mut sess, "stationSvc", "Undock", &[]).await;
    common::slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let PyRep::Tuple(bound) = ok(ctx, &mut sess, "beyonce", "MachoBindObject", &[int(i64::from(sys_id)), PyRep::None]).await else { panic!("bind") };
    let beyonce = bound[0].as_i64().unwrap() as u64;
    ctx.advance_secs(31);
    common::slash_cmd(ctx, &mut sess, "/heal").await;
    (LivePilot { sess, char_id, ship_id, beyonce }, fitted_ids)
}

async fn list_bound_inv(ctx: &ServerContext, sess: &mut SessionState, loc_id: i64, flag: Option<i64>) -> Result<Vec<PyRep>, BridgeRpcError> {
    let list_args: Vec<PyRep> = flag.into_iter().map(int).collect();
    let rep = if sess.ship_id == Some(loc_id) || sess.station_id == Some(loc_id) {
        rpc(ctx, sess, "invbroker", "List", &list_args).await?
    } else {
        let mgr_loc = sess.station_id.or(sess.structure_id).or_else(|| sess.solar_system_id.map(i64::from)).unwrap();
        let PyRep::Tuple(m) = ok(ctx, sess, "invbroker", "MachoBindObject", &[int(mgr_loc), PyRep::None]).await else { return Ok(Vec::new()); };
        let inv_mgr = m[0].as_i64().unwrap() as u64;
        let cont_id = ctx.dispatch_rpc(sess, "invbroker", Some(inv_mgr), "GetInventoryFromId", &[int(loc_id), int(0)], &HashMap::new(), None).await?.as_i64().unwrap() as u64;
        ctx.dispatch_rpc(sess, "invbroker", Some(cont_id), "List", &list_args, &HashMap::new(), None).await?
    };
    Ok(rep.crowset_rows().unwrap_or_default().to_vec())
}

async fn qty_of(ctx: &ServerContext, sess: &mut SessionState, loc_id: i64, type_id: u32, flag: Option<i64>) -> i64 {
    let Ok(rows) = list_bound_inv(ctx, sess, loc_id, flag).await else { return 0; };
    rows.iter()
        .filter(|r| r.dbrow_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(type_id)))
        .map(|r| {
            let q = r.dbrow_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
            if q < 0 { 1 } else { q }
        })
        .sum()
}

fn pb_id_msg(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

fn pb_read_varint(buf: &[u8], idx: &mut usize) -> u64 {
    let (mut val, mut shift) = (0u64, 0u32);
    while *idx < buf.len() {
        let b = buf[*idx];
        *idx += 1;
        val |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 { break; }
        shift += 7;
    }
    val
}

fn pb_get_bytes(buf: &[u8], target_tag: u32) -> Option<Vec<u8>> {
    let mut idx = 0;
    while idx < buf.len() {
        let key = pb_read_varint(buf, &mut idx);
        let tag = (key >> 3) as u32;
        let wt = (key & 7) as u32;
        match wt {
            0 => { pb_read_varint(buf, &mut idx); }
            1 => idx += 8,
            2 => {
                let len = pb_read_varint(buf, &mut idx) as usize;
                let end = (idx + len).min(buf.len());
                if tag == target_tag { return Some(buf[idx..end].to_vec()); }
                idx = end;
            }
            5 => idx += 4,
            _ => break,
        }
    }
    None
}

fn pb_get_varint(buf: &[u8], target_tag: u32) -> Option<u64> {
    let mut idx = 0;
    while idx < buf.len() {
        let key = pb_read_varint(buf, &mut idx);
        let tag = (key >> 3) as u32;
        let wt = (key & 7) as u32;
        match wt {
            0 => {
                let v = pb_read_varint(buf, &mut idx);
                if tag == target_tag { return Some(v); }
            }
            1 => idx += 8,
            2 => {
                let len = pb_read_varint(buf, &mut idx) as usize;
                idx = (idx + len).min(buf.len());
            }
            5 => idx += 4,
            _ => break,
        }
    }
    None
}

async fn spawn_asteroid(ctx: &ServerContext, sess: &mut SessionState, ast_type: u32, _pos: DVec3) -> i64 {
    let rep = common::slash_cmd(ctx, sess, &format!("/spawn {ast_type} 1")).await;
    rep.dict_get("spawnedID").or_else(|| rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap()
}

#[tokio::test]
async fn test_excavator_drones_live_bay_core_and_skill_scaling() {
    // parity: feat:mining.excavator-drones
    eve_testkit::attest(&["feat:mining.excavator-drones"]);

    let ctx = paused_ctx();
    let skills = [(3436_u32, 5_u8), (3442, 1), (22_541, 1), (28_374, 1), (43_702, 1), (43_703, 1)];
    let (mut p, mods) = gm_pilot_fitted_in_space(&ctx, 71_001, 91_100_001, 28_352 /* Rorqual */, NULL_SYS, &skills, &[(28_583, HIGH_SLOT_0, None)]).await;
    let core_id = mods[0];

    // 1. V24 Rorqual SDE droneCapacity = 8,800 m3 (8x 1,100 m3 Excavators fit; 9th rejected with CapacityExceeded)
    assert_eq!(ctx.sde.types[&28_352].default_attributes.get(&283).copied(), Some(8_800.0));
    let stack8 = common::stock_ship_bay(&ctx, &mut p, 41_030, 8, DRONE_BAY).await;
    let mut d_ids = vec![stack8];
    for _ in 0..7 {
        let split = ok(&ctx, &mut p.sess, "invbroker", "SplitStack", &[int(stack8), int(1)]).await;
        d_ids.push(split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap());
    }
    assert_eq!(qty_of(&ctx, &mut p.sess, p.ship_id, 41_030, Some(DRONE_BAY)).await, 8);
    let extra_drone = common::slash_cmd(&ctx, &mut p.sess, "/createitem 41030 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let bay_err = rpc(&ctx, &mut p.sess, "invbroker", "Add", &[int(extra_drone), int(p.ship_id), int(DRONE_BAY)]).await.unwrap_err();
    assert_eq!(bay_err.code, "CapacityExceeded", "9th Excavator (9,900 m3 > 8,800 m3 bay) must fail with CapacityExceeded");

    // Launching > 5 drones rejects over maxActiveDrones (5); launching 5 succeeds
    let over_launch = rpc(&ctx, &mut p.sess, "ship", "LaunchDrones", &[PyRep::List(d_ids.iter().copied().map(int).collect())]).await;
    assert!(over_launch.is_err(), "Launching 8 drones must exceed maxActiveDrones (5)");
    let active_5 = &d_ids[..5];
    let PyRep::List(launched_ore_balls) = ok(&ctx, &mut p.sess, "ship", "LaunchDrones", &[PyRep::List(active_5.iter().copied().map(int).collect())]).await else { panic!("launched drones list") };
    assert_eq!(launched_ore_balls.len(), 5);

    // 2. Fuel Industrial Core & verify inactive Core rejects mining, while active Core mines Veldspar (1230)
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 3442 1").await;
    common::slash_cmd(&ctx, &mut p.sess, "/createitem 16274 5000").await;
    let ast_veld = spawn_asteroid(&ctx, &mut p.sess, 1230, DVec3::new(11_000.0, 0.0, 0.0)).await;
    let no_core_err = rpc(&ctx, &mut p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(active_5.iter().copied().map(int).collect()), int(ast_veld)]).await.unwrap_err();
    assert_eq!(no_core_err.code, "IndustrialCoreRequired");

    ok(&ctx, &mut p.sess, "dogmaIM", "Activate", &[int(core_id)]).await;
    ok(&ctx, &mut p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(active_5.iter().copied().map(int).collect()), int(ast_veld)]).await;
    let null_h = ctx.systems.get_or_spawn(NULL_SYS);
    ctx.advance_secs(130);
    null_h.step_manual(130.0).await.unwrap();
    tick_background_cluster(&ctx).await;
    let mined_ore_l1 = qty_of(&ctx, &mut p.sess, p.ship_id, 1230, Some(ORE_HOLD)).await;
    assert!(mined_ore_l1 > 0, "5 Excavator drones with active Industrial Core must mine Veldspar into OreHold");

    // 3. Return ore drones, compare Drone Interfacing (3442) L1 vs L5 per-cycle yield on Veldspar, then harvest Clear Icicle with 5 Ice Excavator Drones (43681)
    ok(&ctx, &mut p.sess, "entity", "CmdReturnBay", &[PyRep::List(launched_ore_balls)]).await;
    ctx.advance_secs(30);
    null_h.step_manual(30.0).await.unwrap();
    let PyRep::List(launched_l5_check) = ok(&ctx, &mut p.sess, "ship", "LaunchDrones", &[PyRep::List(active_5.iter().copied().map(int).collect())]).await else { panic!("relaunch drones") };
    assert_eq!(launched_l5_check.len(), 5);
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 3442 5").await;
    let ast_veld_l5 = spawn_asteroid(&ctx, &mut p.sess, 1230, DVec3::new(11_000.0, 0.0, 0.0)).await;
    ok(&ctx, &mut p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(active_5.iter().copied().map(int).collect()), int(ast_veld_l5)]).await;
    ctx.advance_secs(130);
    null_h.step_manual(130.0).await.unwrap();
    tick_background_cluster(&ctx).await;
    let mined_ore_l5 = qty_of(&ctx, &mut p.sess, p.ship_id, 1230, Some(ORE_HOLD)).await - mined_ore_l1;
    assert!(mined_ore_l5 > mined_ore_l1, "Drone Interfacing V must yield strictly more per cycle than L1: l1={mined_ore_l1}, l5={mined_ore_l5}");

    ok(&ctx, &mut p.sess, "entity", "CmdReturnBay", &[PyRep::List(launched_l5_check)]).await;
    ctx.advance_secs(30);
    null_h.step_manual(30.0).await.unwrap();
    for &did in active_5 {
        ok(&ctx, &mut p.sess, "invbroker", "Add", &[int(did), int(p.ship_id), int(FLEET_HANGAR)]).await;
    }
    let ice_stack = common::stock_ship_bay(&ctx, &mut p, 43_681, 5, DRONE_BAY).await;
    let mut ice_drones = vec![ice_stack];
    for _ in 0..4 {
        let split = ok(&ctx, &mut p.sess, "invbroker", "SplitStack", &[int(ice_stack), int(1)]).await;
        ice_drones.push(split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap());
    }
    let PyRep::List(launched_ice_balls) = ok(&ctx, &mut p.sess, "ship", "LaunchDrones", &[PyRep::List(ice_drones.iter().copied().map(int).collect())]).await else { panic!("launched ice drones") };
    assert_eq!(launched_ice_balls.len(), 5);
    let ast_ice = spawn_asteroid(&ctx, &mut p.sess, 16_262 /* Clear Icicle */, DVec3::new(11_000.0, 0.0, 0.0)).await;

    ok(&ctx, &mut p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(ice_drones.iter().copied().map(int).collect()), int(ast_ice)]).await;
    ctx.advance_secs(210);
    null_h.step_manual(210.0).await.unwrap();
    tick_background_cluster(&ctx).await;
    let ice_harvested = qty_of(&ctx, &mut p.sess, p.ship_id, 16_262, Some(ORE_HOLD)).await;
    assert!(ice_harvested > 0, "5 Ice Excavator drones must harvest Clear Icicle");
}

#[tokio::test]
async fn test_mining_goals_and_mineral_market_supply_chain_live() {
    // parity: feat:mining.mining-goals
    // parity: feat:mining.mining-market
    eve_testkit::attest(&["feat:mining.mining-goals", "feat:mining.mining-market"]);

    let ctx = paused_ctx();
    let skills = [(3386_u32, 5_u8), (3385, 5), (3389, 5), (12_180, 5)];
    let (mut seller, mods) = gm_pilot_fitted_in_space(&ctx, 71_002, 91_100_002, 32_880 /* Venture */, JITA_SYS, &skills, &[(483, HIGH_SLOT_0, None)]).await;
    let miner_id = mods[0];
    common::slash_cmd(&ctx, &mut seller.sess, "/giveisk 10000000").await;

    // 1. Negative client-supplied event must NOT credit mining ledger or challenges
    let _ = rpc(&ctx, &mut seller.sess, "seasonManager", "process_client_event", &[PyRep::String("totalMined".into()), int(50_000)]).await;
    let PyRep::List(logs_before) = ok(&ctx, &mut seller.sess, "characterMiningLedger", "GetCharacterLogs", &[]).await else { panic!("logs list") };
    assert_eq!(logs_before.len(), 0, "Client-supplied event must not create mining logs");

    // 2. Create Quasar Corp Goal for mining Veldspar (1230)
    let mut create_goal = Vec::new();
    encode_string_field(1, "Mine Veldspar Corp Project", &mut create_goal);
    encode_string_field(2, "Mine Veldspar", &mut create_goal);
    encode_varint_field(3, 5_000, &mut create_goal);
    let mut tid_filter = Vec::new();
    encode_bytes_field(1, &pb_id_msg(1230), &mut tid_filter);
    let mut ast_filter = Vec::new();
    encode_bytes_field(1, &tid_filter, &mut ast_filter);
    let mut mine_cfg = Vec::new();
    encode_bytes_field(1, &ast_filter, &mut mine_cfg);
    let mut contrib_cfg = Vec::new();
    encode_bytes_field(5, &mine_cfg, &mut contrib_cfg);
    encode_bytes_field(4, &contrib_cfg, &mut create_goal);
    let g_rsp = quasar_rpc(&ctx, &mut seller.sess, "eve_public.goal.api.CreateRequest", create_goal).await;
    assert_eq!(g_rsp.status_code, 200);
    let goal_id_bytes = pb_get_bytes(&g_rsp.payload.unwrap().value, 1).expect("created goal uuid");

    let mut get_goal_req = Vec::new();
    encode_bytes_field(1, &goal_id_bytes, &mut get_goal_req);
    let g0 = quasar_rpc(&ctx, &mut seller.sess, "eve_public.goal.api.GetRequest", get_goal_req.clone()).await.payload.unwrap().value;
    let goal_msg0 = pb_get_bytes(&g0, 1).unwrap_or_default();
    let prog0 = pb_get_bytes(&goal_msg0, 5).and_then(|p| pb_get_varint(&p, 2)).unwrap_or(0);
    assert_eq!(prog0, 0, "Goal progress must be 0 before mining");

    // 3. Mine real Veldspar asteroid in space via dogmaIM.Activate
    let ast_id = spawn_asteroid(&ctx, &mut seller.sess, 1230, DVec3::new(11_000.0, 0.0, 0.0)).await;
    ok(&ctx, &mut seller.sess, "dogmaIM", "AddTarget", &[int(ast_id)]).await;
    ctx.advance_secs(5);
    ctx.systems.get_or_spawn(JITA_SYS).step_manual(5.0).await.unwrap();
    ok(&ctx, &mut seller.sess, "dogmaIM", "Activate", &[int(miner_id), int(ast_id)]).await;
    ctx.advance_secs(62);
    tick_background_cluster(&ctx).await;
    ok(&ctx, &mut seller.sess, "dogmaIM", "Deactivate", &[int(miner_id)]).await;

    let mined_qty = qty_of(&ctx, &mut seller.sess, seller.ship_id, 1230, Some(ORE_HOLD)).await;
    assert!(mined_qty > 0, "Miner I cycle must mine Veldspar into OreHold");
    let PyRep::List(logs_after) = ok(&ctx, &mut seller.sess, "characterMiningLedger", "GetCharacterLogs", &[]).await else { panic!("logs list") };
    assert_eq!(logs_after.len(), 1);
    assert_eq!(logs_after[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(mined_qty));

    let g1 = quasar_rpc(&ctx, &mut seller.sess, "eve_public.goal.api.GetRequest", get_goal_req).await.payload.unwrap().value;
    let goal_msg1 = pb_get_bytes(&g1, 1).unwrap();
    let prog1 = pb_get_bytes(&goal_msg1, 5).and_then(|p| pb_get_varint(&p, 2));
    assert_eq!(prog1, Some(mined_qty as u64), "Quasar Corp Mining Goal must advance to exact mined quantity");

    let challenges = ok(&ctx, &mut seller.sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await;
    assert!(!matches!(challenges, PyRep::None));

    // 4. Verify SDE volumetric ratios & jump stargate into Perimeter to reprocess & trade
    assert_eq!((ctx.sde.types[&1230].volume, ctx.sde.types[&62_516].volume, ctx.sde.types[&34].volume), (0.10, 0.001, 0.01));

    let gate_to_perim = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA_SYS && j.to_solar_system_id == PERIMETER_SYS).unwrap();
    let gate_pos = ctx.sde.stargates()[&gate_to_perim.stargate_id].position;
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(seller.ship_id, gate_pos).await;
    ok(&ctx, &mut seller.sess, "beyonce", "CmdStargateJump", &[int(gate_to_perim.stargate_id)]).await;
    assert_eq!(seller.sess.solar_system_id, Some(PERIMETER_SYS));
    ctx.advance_secs(11);

    let st_p = ctx.sde.stations.values().find(|s| s.solar_system_id == PERIMETER_SYS).expect("Perimeter station");
    let perim_station = st_p.station_id;
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(seller.ship_id, DVec3::from(st_p.position)).await;
    ok(&ctx, &mut seller.sess, "stationSvc", "Dock", &[int(perim_station)]).await;

    let raw_stack = common::slash_cmd(&ctx, &mut seller.sess, "/createitem 1230 1000").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut seller.sess, "reprocessingSvc", "Reprocess", &[PyRep::List(vec![int(raw_stack)]), int(perim_station), int(4), PyRep::Bool(false), PyRep::List(vec![])]).await;
    assert!(qty_of(&ctx, &mut seller.sess, perim_station, 34, Some(4)).await > 0, "Reprocessing Veldspar must yield Tritanium");

    let raw_sell = common::slash_cmd(&ctx, &mut seller.sess, "/createitem 1230 100").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let comp_sell = common::slash_cmd(&ctx, &mut seller.sess, "/createitem 62516 100").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let hangar_items = list_bound_inv(&ctx, &mut seller.sess, perim_station, Some(4)).await.unwrap();
    let trit_sell = hangar_items.iter().find(|i| i.dbrow_get("typeID").and_then(|v| v.as_i64()) == Some(34)).and_then(|i| i.dbrow_get("itemID").and_then(|v| v.as_i64())).unwrap();

    for (iid, tid, price, qty) in [(raw_sell, 1230_i64, 10.0, 50), (comp_sell, 62_516, 1_000.0, 20), (trit_sell, 34, 5.0, 100)] {
        ok(&ctx, &mut seller.sess, "marketProxy", "PlaceMultiSellOrder", &[
            PyRep::List(vec![PyRep::keyval([("stationID", int(perim_station)), ("typeID", int(tid)), ("itemID", int(iid)), ("price", PyRep::Real(price)), ("quantity", int(qty))])]),
            PyRep::Bool(false),
            int(90),
        ]).await;
    }

    let (mut buyer, _) = gm_pilot_fitted_in_space(&ctx, 71_003, 91_100_003, 606, PERIMETER_SYS, &[], &[]).await;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(buyer.char_id).or_default().insert(
            3447,
            eve_db::TrainedSkillRecord { type_id: 3447, skill_points: 250.0, level: 1 },
        );
    });
    common::slash_cmd(&ctx, &mut buyer.sess, "/giveisk 1000000").await;
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(buyer.ship_id, DVec3::from(st_p.position)).await;
    ok(&ctx, &mut buyer.sess, "stationSvc", "Dock", &[int(perim_station)]).await;
    let buyer_isk_0 = ok(&ctx, &mut buyer.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap();
    for (tid, price, qty) in [(1230_i64, 10.0, 50_i64), (62_516, 1_000.0, 20), (34, 5.0, 100)] {
        ok(&ctx, &mut buyer.sess, "marketProxy", "PlaceBuyOrder", &[int(perim_station), int(tid), PyRep::Real(price), int(qty), int(0), int(1), int(0)]).await;
    }
    let buyer_isk_1 = ok(&ctx, &mut buyer.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap();
    assert!(((buyer_isk_0 - buyer_isk_1) - (50.0 * 10.0 + 20.0 * 1_000.0 + 100.0 * 5.0)).abs() < 1e-2);
    assert_eq!(qty_of(&ctx, &mut buyer.sess, perim_station, 1230, Some(4)).await, 50);
    assert_eq!(qty_of(&ctx, &mut buyer.sess, perim_station, 62_516, Some(4)).await, 20);
    assert_eq!(qty_of(&ctx, &mut buyer.sess, perim_station, 34, Some(4)).await, 100);
}

#[tokio::test]
async fn test_phased_asteroid_field_lifecycle_and_mobile_phase_anchor_live() {
    // parity: feat:mining.mobile-phase-anchor
    // parity: feat:mining.phased-asteroid-field-lifecycle
    eve_testkit::attest(&["feat:mining.mobile-phase-anchor", "feat:mining.phased-asteroid-field-lifecycle"]);

    let ctx = paused_ctx();
    let (mut p_hs, _) = gm_pilot_fitted_in_space(&ctx, 71_004, 91_100_004, 28_606 /* Orca */, JITA_SYS, &[], &[]).await;
    let anchor_hs = common::slash_cmd(&ctx, &mut p_hs.sess, "/createitem 90037 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let hs_drop_err = rpc(&ctx, &mut p_hs.sess, "ship", "Drop", &[int(anchor_hs)]).await.unwrap_err();
    assert_eq!(hs_drop_err.code, "CannotDeployInHighSec");

    ctx.set_mechanic_rng_seed(Some(2));
    let (mut orca1, orca1_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_005, 91_100_005, 28_606 /* Orca */, NULL_SYS,
        &[(3412, 5), (3426, 5)],
        &[(17_938, HIGH_SLOT_0, Some((30_013, 8))), (58_950 /* Industrial Core I (50 energy) */, HIGH_SLOT_1, None)],
    ).await;
    let (launcher_id, core1) = (orca1_mods[0], orca1_mods[1]);
    common::slash_cmd(&ctx, &mut orca1.sess, "/createitem 16274 5000").await;
    let (mut miner_p, m_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_006, 91_100_006, 17_476 /* Covetor */, NULL_SYS, &[(3386, 5), (3410, 5), (3420, 5), (17_940, 5)],
        &[(28_754 /* ORE Strip Miner */, HIGH_SLOT_0, None), (28_576 /* Mining Laser Upgrade II */, LOW_SLOT_0, None), (28_576, LOW_SLOT_0 + 1, None)],
    ).await;
    let v_miner = m_mods[0];

    ok(&ctx, &mut orca1.sess, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[]).await;
    tick_background_cluster(&ctx).await;
    ok(&ctx, &mut orca1.sess, "dogmaIM", "Activate", &[int(launcher_id), PyRep::String("launchScanners".into()), PyRep::None, int(0)]).await;
    let scan_rows = common::probe_scan(&ctx, &mut orca1.sess, DVec3::ZERO, 40_000_000_000.0).await;
    let phased_row = scan_rows.iter().find(|r| r.dict_get("type").and_then(|v| v.as_str()) == Some("Phased Asteroid Field")).expect("Phased Asteroid Field cosmic signature");
    let sig_id = phased_row.dict_get("signatureID").and_then(|v| v.as_str()).unwrap().to_string();
    assert_eq!(common::scan_row_field(&scan_rows, &sig_id, "signalStrength").as_f64(), Some(1.0));

    let pris_ball_id = ctx.systems.get_or_spawn(NULL_SYS).list_balls().await.unwrap().into_iter().find(|b| b.type_id == 90_041).map(|b| b.id).expect("Prismaticite asteroid ball");
    let pris_pos = ctx.systems.get_or_spawn(NULL_SYS).get_ball_snapshot(pris_ball_id).await.unwrap().unwrap().1.position;
    ctx.systems.get_or_spawn(NULL_SYS).set_ball_position(orca1.ship_id, pris_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    ctx.systems.get_or_spawn(NULL_SYS).set_ball_position(miner_p.ship_id, pris_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;

    ok(&ctx, &mut miner_p.sess, "dogmaIM", "AddTarget", &[int(pris_ball_id)]).await;
    ctx.advance_secs(5);
    ctx.systems.get_or_spawn(NULL_SYS).step_manual(5.0).await.unwrap();
    ok(&ctx, &mut miner_p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![int(v_miner)]), int(pris_ball_id)]).await;
    let breached_yield = qty_of(&ctx, &mut miner_p.sess, miner_p.ship_id, 90_041, Some(ORE_HOLD)).await;
    assert!(breached_yield > 0, "Breached Prismaticite must yield non-zero base ore");

    let anoms_early = ok(&ctx, &mut orca1.sess, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[]).await.as_i64().unwrap();
    ctx.advance_secs(7_250);
    tick_background_cluster(&ctx).await;
    let anoms_half = ok(&ctx, &mut orca1.sess, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[]).await.as_i64().unwrap();
    assert_eq!(anoms_half, anoms_early + 1, "50% fracture must spawn OreAnomaly beacon");

    let anchor_id = common::slash_cmd(&ctx, &mut orca1.sess, "/createitem 90037 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut orca1.sess, "ship", "Drop", &[int(anchor_id)]).await;
    ok(&ctx, &mut orca1.sess, "dogmaIM", "Activate", &[int(core1)]).await;

    let fid = ok(&ctx, &mut orca1.sess, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().unwrap();
    orca1.sess.fleet_id = Some(fid);

    let (mut porpoise, porp_mods) = gm_pilot_fitted_in_space(&ctx, 71_007, 91_100_007, 42_244 /* Porpoise (35 energy) */, NULL_SYS, &[], &[(62_591 /* Medium Industrial Core II (35 energy) */, HIGH_SLOT_0, None)]).await;
    ok(&ctx, &mut orca1.sess, "fleetObjectHandler", "Invite", &[int(porpoise.char_id)]).await;
    ok(&ctx, &mut porpoise.sess, "fleetObjectHandler", "AcceptInvite", &[int(fid)]).await;
    porpoise.sess.fleet_id = Some(fid);
    common::slash_cmd(&ctx, &mut porpoise.sess, "/createitem 16274 5000").await;
    ctx.systems.get_or_spawn(NULL_SYS).set_ball_position(porpoise.ship_id, pris_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    ok(&ctx, &mut porpoise.sess, "dogmaIM", "Activate", &[int(porp_mods[0])]).await;
    tick_background_cluster(&ctx).await;

    ok(&ctx, &mut miner_p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![int(v_miner)]), int(pris_ball_id)]).await;
    let after_85_yield = qty_of(&ctx, &mut miner_p.sess, miner_p.ship_id, 90_041, Some(ORE_HOLD)).await - breached_yield;
    assert_eq!(after_85_yield, breached_yield, "85 energy (<100) must keep Phased Asteroid Field breached (10% efficiency)");

    let (mut orca2, orca2_mods) = gm_pilot_fitted_in_space(&ctx, 71_008, 91_100_008, 28_606 /* Orca (50 energy) */, NULL_SYS, &[], &[(58_950 /* Industrial Core I (50 energy) */, HIGH_SLOT_0, None)]).await;
    ok(&ctx, &mut orca1.sess, "fleetObjectHandler", "Invite", &[int(orca2.char_id)]).await;
    ok(&ctx, &mut orca2.sess, "fleetObjectHandler", "AcceptInvite", &[int(fid)]).await;
    orca2.sess.fleet_id = Some(fid);
    common::slash_cmd(&ctx, &mut orca2.sess, "/createitem 16274 5000").await;
    ctx.systems.get_or_spawn(NULL_SYS).set_ball_position(orca2.ship_id, pris_pos + DVec3::new(4_000.0, 0.0, 0.0)).await;
    ok(&ctx, &mut orca2.sess, "dogmaIM", "Activate", &[int(orca2_mods[0])]).await;
    tick_background_cluster(&ctx).await;
    ok(&ctx, &mut miner_p.sess, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![int(v_miner)]), int(pris_ball_id)]).await;
    let phased_yield = qty_of(&ctx, &mut miner_p.sess, miner_p.ship_id, 90_041, Some(ORE_HOLD)).await - (breached_yield + after_85_yield);
    assert_eq!(phased_yield, breached_yield * 10, "Phased-in field must yield exact 10x breached quantity");
    ctx.advance_secs(7_500);
    tick_background_cluster(&ctx).await;
    let anoms_collapsed = ok(&ctx, &mut orca1.sess, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[]).await.as_i64().unwrap();
    assert_eq!(anoms_collapsed, anoms_early, "Collapsed field must remove OreAnomaly beacon");
}

#[tokio::test]
async fn test_disruptive_lance_live_constraints_skills_neut_debuff_and_locks() {
    // parity: feat:modules.disruptive-lance
    eve_testkit::attest(&["feat:modules.disruptive-lance"]);
    let ctx = paused_ctx();
    let (mut lancer_hs, hs_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_009, 91_100_009, 77_283 /* Karura */, JITA_SYS,
        &[(77_739, 1), (77_738, 1)],
        &[(77_399, HIGH_SLOT_0, None)],
    ).await;
    let hs_fire_err = rpc(&ctx, &mut lancer_hs.sess, "dogmaIM", "Activate", &[int(hs_mods[0])]).await.unwrap_err();
    assert!(!hs_fire_err.code.is_empty());
    let jita_st = ctx.sde.stations.values().find(|s| s.solar_system_id == JITA_SYS).unwrap();
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(lancer_hs.ship_id, DVec3::from(jita_st.position)).await;
    ok(&ctx, &mut lancer_hs.sess, "stationSvc", "Dock", &[int(jita_st.station_id)]).await;
    let cloak_hs = common::slash_cmd(&ctx, &mut lancer_hs.sess, "/createitem 11578 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let cloak_err = rpc(&ctx, &mut lancer_hs.sess, "invbroker", "Add", &[int(cloak_hs), int(lancer_hs.ship_id), int(HIGH_SLOT_1)]).await.unwrap_err();
    assert_eq!(cloak_err.code, "FittingValidationFailed");
    let (mut lancer, l_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_010, 91_100_010, 77_283 /* Karura */, LOW_SYS,
        &[(77_739, 1), (77_738, 1)],
        &[(77_399, HIGH_SLOT_0, None), (20_280, HIGH_SLOT_1, None)],
    ).await;
    let (lance_id, siege_id) = (l_mods[0], l_mods[1]);
    common::slash_cmd(&ctx, &mut lancer.sess, "/createitem 16274 200000").await;
    common::slash_cmd(&ctx, &mut lancer.sess, "/createitem 17888 50000").await;
    common::slash_cmd(&ctx, &mut lancer.sess, "/createitem 16275 50000").await;
    common::slash_cmd(&ctx, &mut lancer.sess, "/createitem 16273 50000").await;
    let siege_err = rpc(&ctx, &mut lancer.sess, "dogmaIM", "Activate", &[int(lance_id)]).await.unwrap_err();
    assert_eq!(siege_err.code, "SiegeRequiredForLance");

    let (beam_victim, _) = gm_pilot_fitted_in_space(&ctx, 71_011, 91_100_011, 28_352 /* Rorqual */, LOW_SYS, &[], &[]).await;
    let (mut area_victim, _) = gm_pilot_fitted_in_space(&ctx, 71_012, 91_100_012, 28_352 /* Rorqual */, LOW_SYS, &[], &[]).await;
    let (far_ship, _) = gm_pilot_fitted_in_space(&ctx, 71_013, 91_100_013, 28_352 /* Rorqual */, LOW_SYS, &[], &[]).await;
    let rep_tid = ctx.sde.types.values().find(|t| t.name == "Medium Remote Armor Repairer II").unwrap().type_id;
    let (mut logi, logi_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_014, 91_100_014, 643 /* Armageddon */, LOW_SYS,
        &[(3413, 5), (3417, 5), (3432, 5), (3451, 5), (3452, 5), (16_069, 5), (27_902, 5)],
        &[(rep_tid, HIGH_SLOT_0, None)],
    ).await;
    let (mut gunner, g_mods) = gm_pilot_fitted_in_space(
        &ctx, 71_015, 91_100_015, 17_740 /* Vindicator */, LOW_SYS,
        &[(3300, 5), (3302, 5), (3308, 5), (3413, 5), (12_209, 5)],
        &[
            (2929 /* 800mm Repeating Cannon II */, HIGH_SLOT_0, Some((21_894 /* Republic Fleet EMP L */, 120))),
            (2929, HIGH_SLOT_0 + 1, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 2, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 3, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 4, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 5, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 6, Some((21_894, 120))),
            (2929, HIGH_SLOT_0 + 7, Some((21_894, 120))),
        ],
    ).await;
    let rep_id = logi_mods[0];

    let low_h = ctx.systems.get_or_spawn(LOW_SYS);
    low_h.set_ball_position(far_ship.ship_id, DVec3::new(20_000.0, 15_000.0, 0.0)).await;
    low_h.set_ball_position(gunner.ship_id, DVec3::new(20_000.0, 15_000.0, 1_500.0)).await;
    let far_ar_full = low_h.get_combat_state(far_ship.ship_id).await.unwrap().unwrap().max_armor_hp;
    ok(&ctx, &mut gunner.sess, "dogmaIM", "AddTarget", &[int(far_ship.ship_id)]).await;
    ctx.advance_secs(15);
    low_h.step_manual(15.0).await.unwrap();
    for _ in 0..160 {
        for &m in &g_mods {
            let _ = rpc(&ctx, &mut gunner.sess, "dogmaIM", "Activate", &[int(m), PyRep::String("targetAttack".into()), int(far_ship.ship_id)]).await;
        }
        ctx.advance_secs(8);
        low_h.step_manual(8.0).await.unwrap();
        if low_h.get_combat_state(far_ship.ship_id).await.unwrap().unwrap().armor_hp < far_ar_full - 200.0 { break; }
    }
    for &m in &g_mods { ok(&ctx, &mut gunner.sess, "dogmaIM", "Deactivate", &[int(m)]).await; }

    low_h.set_ball_position(far_ship.ship_id, DVec3::new(20_000.0, 15_000.0, 0.0)).await;
    low_h.set_ball_position(logi.ship_id, DVec3::new(20_000.0, 18_000.0, 0.0)).await;
    let far_ar_before = low_h.get_combat_state(far_ship.ship_id).await.unwrap().unwrap().armor_hp;
    assert!(far_ar_before < far_ar_full, "Gunner must damage far_ship armor before baseline remote repair");
    ok(&ctx, &mut logi.sess, "dogmaIM", "AddTarget", &[int(far_ship.ship_id)]).await;
    for _ in 0..15 {
        ctx.advance_secs(1);
        low_h.step_manual(1.0).await.unwrap();
        if low_h.get_target_locks(logi.ship_id).await.unwrap().iter().any(|l| l.target_id == far_ship.ship_id && l.locked) { break; }
    }
    ok(&ctx, &mut logi.sess, "dogmaIM", "Activate", &[int(rep_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), int(far_ship.ship_id)]).await;
    for _ in 0..8 { ctx.advance_secs(1); low_h.step_manual(1.0).await.unwrap(); }
    ok(&ctx, &mut logi.sess, "dogmaIM", "Deactivate", &[int(rep_id)]).await;
    let baseline_repped = low_h.get_combat_state(far_ship.ship_id).await.unwrap().unwrap().armor_hp - far_ar_before;
    assert!(baseline_repped > 0.0, "Baseline remote armor repair must restore armor HP");

    low_h.set_ball_position(lancer.ship_id, DVec3::ZERO).await;
    low_h.set_ball_position(beam_victim.ship_id, DVec3::new(20_000.0, 0.0, 0.0)).await;
    low_h.set_ball_position(area_victim.ship_id, DVec3::new(20_000.0, 6_000.0, 0.0)).await;
    ok(&ctx, &mut lancer.sess, "dogmaIM", "Activate", &[int(siege_id)]).await;
    let cap_beam_before = low_h.get_combat_state(beam_victim.ship_id).await.unwrap().unwrap().capacitor;
    let cap_area_before = low_h.get_combat_state(area_victim.ship_id).await.unwrap().unwrap().capacitor;
    let fire_l1 = ok(&ctx, &mut lancer.sess, "dogmaIM", "Activate", &[int(lance_id), int(beam_victim.ship_id)]).await;
    let cap_beam_after = low_h.get_combat_state(beam_victim.ship_id).await.unwrap().unwrap().capacitor;
    let cap_area_after = low_h.get_combat_state(area_victim.ship_id).await.unwrap().unwrap().capacitor;
    assert!((cap_beam_before - cap_beam_after - 35_000.0).abs() < 1e-2, "Beam victim must lose 35,000 GJ capacitor");
    assert!((cap_area_before - cap_area_after - 35_000.0).abs() < 1e-2, "Area victim at 6km (<10km neut radius) must lose 35,000 GJ capacitor");

    let st_tama = ctx.sde.stations.values().find(|s| s.solar_system_id == LOW_SYS).unwrap();
    low_h.set_ball_position(area_victim.ship_id, DVec3::from(st_tama.position)).await;
    assert_eq!(rpc(&ctx, &mut area_victim.sess, "stationSvc", "Dock", &[int(st_tama.station_id)]).await.unwrap_err().code, "DisruptiveLanceDebuffActive");
    let tama_gate = ctx.sde.jumps.values().find(|j| {
        j.from_solar_system_id == LOW_SYS
            && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| s.security_status < 0.45)
    }).unwrap();
    assert_eq!(rpc(&ctx, &mut area_victim.sess, "beyonce", "CmdStargateJump", &[int(tama_gate.stargate_id)]).await.unwrap_err().code, "DisruptiveLanceDebuffActive");
    assert_eq!(rpc(&ctx, &mut area_victim.sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), int(tama_gate.stargate_id)]).await.unwrap_err().code, "DisruptiveLanceDebuffActive");

    low_h.set_ball_position(beam_victim.ship_id, DVec3::new(20_000.0, 0.0, 0.0)).await;
    low_h.set_ball_position(logi.ship_id, DVec3::new(20_000.0, 3_000.0, 0.0)).await;
    let beam_ar_before = low_h.get_combat_state(beam_victim.ship_id).await.unwrap().unwrap().armor_hp;
    ok(&ctx, &mut logi.sess, "dogmaIM", "AddTarget", &[int(beam_victim.ship_id)]).await;
    for _ in 0..15 {
        ctx.advance_secs(1);
        low_h.step_manual(1.0).await.unwrap();
        if low_h.get_target_locks(logi.ship_id).await.unwrap().iter().any(|l| l.target_id == beam_victim.ship_id && l.locked) { break; }
    }
    ok(&ctx, &mut logi.sess, "dogmaIM", "Activate", &[int(rep_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), int(beam_victim.ship_id)]).await;
    for _ in 0..8 { ctx.advance_secs(1); low_h.step_manual(1.0).await.unwrap(); }
    let debuffed_repped = low_h.get_combat_state(beam_victim.ship_id).await.unwrap().unwrap().armor_hp - beam_ar_before;
    assert!((debuffed_repped - 0.5 * baseline_repped).abs() < 1e-2, "Debuffed remote armor repair must be exact 50% of baseline: got {debuffed_repped} vs {}", 0.5 * baseline_repped);

    assert_eq!(rpc(&ctx, &mut lancer.sess, "beyonce", "CmdStargateJump", &[int(tama_gate.stargate_id)]).await.unwrap_err().code, "LanceFiringLockActive");

    common::slash_cmd(&ctx, &mut lancer.sess, "/giveskill 77739 5").await;
    common::slash_cmd(&ctx, &mut lancer.sess, "/giveskill 77738 5").await;
    ok(&ctx, &mut lancer.sess, "dogmaIM", "Deactivate", &[int(siege_id)]).await;
    ctx.advance_secs(301);
    low_h.step_manual(301.0).await.unwrap();
    low_h.set_ball_position(lancer.ship_id, ctx.sde.stargates()[&tama_gate.stargate_id].position).await;
    ok(&ctx, &mut lancer.sess, "beyonce", "CmdStargateJump", &[int(tama_gate.stargate_id)]).await;
    ctx.advance_secs(31);
    let ret_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == tama_gate.to_solar_system_id && j.to_solar_system_id == LOW_SYS).unwrap();
    ctx.systems.get_or_spawn(tama_gate.to_solar_system_id).set_ball_position(lancer.ship_id, ctx.sde.stargates()[&ret_gate.stargate_id].position).await;
    ok(&ctx, &mut lancer.sess, "beyonce", "CmdStargateJump", &[int(ret_gate.stargate_id)]).await;
    ctx.advance_secs(61);
    low_h.step_manual(61.0).await.unwrap();
    common::slash_cmd(&ctx, &mut lancer.sess, "/heal").await;
    ok(&ctx, &mut lancer.sess, "dogmaIM", "Activate", &[int(siege_id)]).await;
    low_h.set_ball_position(lancer.ship_id, DVec3::ZERO).await;
    low_h.set_ball_position(far_ship.ship_id, DVec3::new(20_000.0, 15_000.0, 0.0)).await;
    ok(&ctx, &mut lancer.sess, "dogmaIM", "AddTarget", &[int(far_ship.ship_id)]).await;
    let mut far_lock_ok = false;
    for _ in 0..60 {
        if low_h.get_target_locks(lancer.ship_id).await.unwrap().iter().any(|l| l.target_id == far_ship.ship_id && l.locked) {
            far_lock_ok = true;
            break;
        }
        ctx.advance_secs(1);
        low_h.step_manual(1.0).await.unwrap();
    }
    assert!(far_lock_ok, "Targeting must succeed after Level 5 150s lock timer expires");
    let fire_l5 = ok(&ctx, &mut lancer.sess, "dogmaIM", "Activate", &[int(lance_id), int(far_ship.ship_id)]).await;

    let cap1 = fire_l1.dict_get("capacitorCost").and_then(|v| v.as_f64()).unwrap();
    let cap5 = fire_l5.dict_get("capacitorCost").and_then(|v| v.as_f64()).unwrap();
    assert!((cap5 / cap1 - (0.75 / 0.95)).abs() < 1e-4, "Skill 77739 L5 vs L1 cap ratio must match 0.75/0.95");
    let dmg1 = fire_l1.dict_get("totalDamageApplied").and_then(|v| v.as_f64()).unwrap();
    let dmg5 = fire_l5.dict_get("totalDamageApplied").and_then(|v| v.as_f64()).unwrap();
    assert!((dmg5 / dmg1 - (1.50 / 1.10)).abs() < 1e-4, "Skill 77738 L5 vs L1 damage ratio must match 1.50/1.10");
}
