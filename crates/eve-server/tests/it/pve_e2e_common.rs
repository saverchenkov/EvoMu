//! Shared non-GM player test helpers for Parity Shard 2 E2E verification.
#![forbid(unsafe_code)]
#![allow(dead_code)]
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
pub const JITA_SYS: SolarSystemId = 30_000_142;
pub const JITA_STATION: i64 = 60_003_760;
pub const LOWSEC_NOURVUKAIKEN_SYS: SolarSystemId = 30_002_757;

fn shared_base_world() -> &'static (Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot) {
    static BASE: std::sync::OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let base_ctx = ServerContext::new();
        let _ = eve_server::world_seeder::seed_full_market_catalog(&base_ctx.db, &base_ctx.sde);
        let mut keep: HashSet<u32> = HashSet::from([
            608, 623, 642, 644, 2006, 11_567, 11_963, 19_720, 19_722, 22_428, 23_911, 23_915, 28_352, 28_659, 28_665, 178, 181, 183, 184, 185, 193,
            198, 201, 204, 205, 230, 238, 254, 262, 267, 380, 484, 486, 488, 494, 519, 1319, 1978, 2364, 2420, 2506, 2508, 2510, 2873, 2929, 3057,
            3186, 3300, 3327, 3412, 3413, 3417, 3418, 3426, 3449, 3452, 3455, 3456, 3581, 3831, 9327, 10_190, 11_269, 11_299, 11_489, 12_775, 12_779,
            12_816, 16_273, 16_274, 16_275, 17_648, 17_674, 17_686, 17_887, 17_888, 17_889, 17_938, 20_280, 20_316, 20_452, 21_096, 21_611, 22_175,
            22_177, 24_550, 25_810, 25_811, 25_936, 28_756, 28_758, 30_013, 30_488, 33_180, 33_201, 33_400, 37_296, 37_298, 37_300, 37_304, 40_631, 41_153,
            41_314, 41_330, 41_332, 44_992, 60_244, 78_848,
        ]);
        for t in base_ctx.sde.types.values() {
            let is_pg = matches!(t.group_id, 4050 | 4743);
            let is_mod = matches!(t.name.as_str(), "Data Analyzer I" | "500MN Microwarpdrive II" | "50MN Microwarpdrive II" | "10MN Afterburner II" | "Gyrostabilizer II" | "Magnetic Field Stabilizer II" | "Heat Sink II") || t.default_attributes.get(&1136).copied().unwrap_or(0.0) > 0.0;
            let is_t1 = t.published && t.meta_group_id.unwrap_or(1) == 1 && !t.name.starts_with("Civilian") && matches!(t.group_id, 53 | 55 | 74 | 83 | 85 | 86);
            if is_mod || is_t1 || is_pg { keep.insert(t.type_id); }
            if is_pg {
                if let Some(st) = eve_server::services::pvp_filament_mgr::get_pvp_filament_match_spec(&base_ctx.sde, t.type_id).ship_restrictions_list_id.and_then(|lid| base_ctx.sde.type_lists.get(&lid)).and_then(|tl| base_ctx.sde.types.values().filter(|x| x.published && tl.contains(x)).min_by_key(|x| x.type_id)) {
                    keep.insert(st.type_id);
                    if let Some(alt) = base_ctx.sde.types.values().filter(|x| x.published && x.category_id == 6 && x.group_id == st.group_id).max_by_key(|x| x.default_attributes.get(&102).copied().unwrap_or(0.0) as i32) { keep.insert(alt.type_id); }
                }
            }
        }
        let (seeds, mut req) = (keep.iter().copied().collect::<Vec<_>>(), std::collections::BTreeMap::new());
        for tid in seeds { collect_req_skills(&base_ctx.sde, tid, &mut req, 0); }
        keep.extend(req.keys().copied());
        base_ctx.db.with_mut_snapshot(|s| {
            s.market_orders.retain(|_, o| o.station_id == JITA_STATION && (!o.is_bid || o.type_id == 44_992) && keep.contains(&o.type_id));
            for o in s.market_orders.values_mut().filter(|o| o.is_bid && o.type_id == 44_992) {
                o.price = eve_db::IskCents::from_isk(5_000_000); o.vol_remaining = 10_000_000; o.vol_entered = 10_000_000; o.is_npc = true; o.escrow = eve_db::IskCents::from_isk(5_000_000 * 10_000_000);
            }
        });
        (base_ctx.sde.clone(), base_ctx.db.with_snapshot(Clone::clone))
    })
}
pub fn fresh_player_ctx() -> ServerContext {
    let (sde, base_snap) = shared_base_world();
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    let mut ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.feature_flags.free_fast_checkout_plex = true;
    crate::common::pin_clock(&ctx);
    ctx
}
pub struct TempSqliteFile(pub std::path::PathBuf);
impl Drop for TempSqliteFile { fn drop(&mut self) { for ext in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{ext}", self.0.display())); } } }
fn reload_ctx_with_blob(ctx: &ServerContext, blob: Option<(&str, &str)>) -> ServerContext {
    let u = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let _g = TempSqliteFile(std::env::temp_dir().join(format!("evomu_parity_shard2_{}_{u}.sqlite", std::process::id())));
    ctx.db.save_to_sqlite(&_g.0).expect("save_to_sqlite");
    if let Some((blob_key, blob_val)) = blob {
        let conn = rusqlite::Connection::open(&_g.0).expect("open sqlite for blob config");
        let existing: Option<String> = conn.query_row("SELECT blob_json FROM world_kv_blobs WHERE blob_key = 'service_state_blobs'", [], |r| r.get(0)).ok();
        let mut blobs: Vec<(String, String)> = existing.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        blobs.retain(|(k, _)| k != blob_key);
        blobs.push((blob_key.to_string(), blob_val.to_string()));
        conn.execute("INSERT OR REPLACE INTO world_kv_blobs (blob_key, blob_json) VALUES ('service_state_blobs', ?1)", rusqlite::params![serde_json::to_string(&blobs).expect("json")]).expect("update blob");
    }
    let db2 = Arc::new(eve_db::WorldRepository::open_sqlite(&_g.0).expect("open_sqlite reload"));
    db2.set_clock(ctx.db.clock());
    let mut ctx2 = ServerContext::with_sde_and_repository(ctx.sde.clone(), db2);
    ctx2.feature_flags.free_fast_checkout_plex = true;
    ctx2
}
pub fn reload_ctx_from_sqlite(ctx: &ServerContext) -> ServerContext { reload_ctx_with_blob(ctx, None) }
pub fn configure_sqlite_blob_and_reload(ctx: &ServerContext, blob_key: &str, blob_val: &str) -> ServerContext { reload_ctx_with_blob(ctx, Some((blob_key, blob_val))) }
pub async fn rpc_res(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> { ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await }
pub async fn rpc_ok(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> PyRep { rpc_res(ctx, sess, service, method, args).await.unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}")) }
pub async fn rpc_err(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> BridgeRpcError { rpc_res(ctx, sess, service, method, args).await.expect_err(&format!("Expected {service}.{method} to return Err")) }
fn extract_bound_id(rep: &PyRep) -> Option<i64> {
    match rep {
        PyRep::Integer(v) => Some(*v), PyRep::Tuple(t) => t.iter().find_map(extract_bound_id),
        PyRep::Bytes(b) => std::str::from_utf8(b).ok()?.rsplit(':').next()?.parse().ok(),
        PyRep::String(s) => s.rsplit(':').next()?.parse().ok(),
        PyRep::SubStruct(s) | PyRep::SubStream(s) => extract_bound_id(s), _ => None,
    }
}
pub async fn bind_service(ctx: &ServerContext, sess: &mut SessionState, service: &str, oid: PyRep) -> i64 {
    extract_bound_id(&rpc_ok(ctx, sess, service, "MachoBindObject", &[oid, PyRep::None]).await).unwrap_or(1)
}
fn sync_sess_char(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> i64 {
    let mut ship_id = sess.ship_id.unwrap_or(0);
    if let Ok(ch) = ctx.db.get_character(char_id) {
        sess.character_id = Some(ch.character_id); sess.corporation_id = Some(ch.corporation_id);
        sess.solar_system_id = Some(ch.solar_system_id); sess.station_id = ch.station_id;
        sess.region_id = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.region_id);
        if ch.active_ship_id > 0 { ship_id = ch.active_ship_id; sess.ship_id = Some(ship_id); }
    }
    assert!(ship_id > 0, "active ship id");
    ship_id
}
pub async fn login_existing_player(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> (SessionState, i64) {
    let mut sess = SessionState::new(sess_id);
    sess.address = "127.0.0.1:26000".into();
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc_ok(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    let ship_id = sync_sess_char(ctx, &mut sess, char_id);
    (sess, ship_id)
}
fn bfs_hops(sde: &eve_sde::SdeRegistry, start: SolarSystemId, goal: SolarSystemId) -> usize {
    let (mut vis, mut q) = (HashSet::from([start]), VecDeque::from([(start, 0usize)]));
    while let Some((cur, d)) = q.pop_front() {
        if cur == goal { return d; }
        for j in sde.jumps.values().filter(|j| j.from_solar_system_id == cur) { if vis.insert(j.to_solar_system_id) { q.push_back((j.to_solar_system_id, d + 1)); } }
    }
    999
}

pub async fn create_player_pilot(ctx: &ServerContext, sess_id: u64, user: &str, name: &str, solar_system_id: SolarSystemId) -> (SessionState, i64, i64) {
    let pass = format!("{user}-secret");
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key(user)) { ctx.db.provision_player_account(user, &pass); }
    let mut sess = SessionState::new(sess_id);
    sess.address = "127.0.0.1:26000".into();
    crate::common::login_pilot(ctx, &mut sess, user, &pass).await;
    let doll = PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]);
    let portrait = PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]);
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().filter(|s| s.race_id == 1).min_by_key(|s| bfs_hops(&ctx.sde, s.solar_system_id, JITA_SYS)).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let char_id = rpc_ok(ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String(name.into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(anc),
        doll, portrait, PyRep::Integer(sch), PyRep::None, PyRep::Integer(i64::from(solar_system_id.max(JITA_SYS))),
    ]).await.as_i64().expect("CreateCharacterWithDoll returned char_id");
    rpc_ok(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    let ship_id = sync_sess_char(ctx, &mut sess, char_id);
    fund_pilot_isk(ctx, &mut sess, 3).await;
    if solar_system_id > 0 {
        if let Some(cur_sys) = sess.solar_system_id.filter(|&s| s != JITA_SYS) {
            if sess.station_id.is_some() { undock_from_station(ctx, &mut sess, cur_sys).await; }
            fly_stargates_to(ctx, &mut sess, JITA_SYS).await;
        }
        if sess.station_id != Some(JITA_STATION) { warp_and_dock_at_station(ctx, &mut sess, ship_id, JITA_SYS, JITA_STATION).await; }
    }
    (sess, char_id, ship_id)
}

pub async fn fund_pilot_isk(ctx: &ServerContext, sess: &mut SessionState, packs: i64) {
    let packs = packs.max(20);
    rpc_ok(ctx, sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(4), PyRep::Integer(packs)]).await;
    let plex_best = rpc_ok(ctx, sess, "marketProxy", "GetPlexBest", &[]).await;
    let p = (plex_best.dict_get("bestBidPrice").and_then(PyRep::as_f64).unwrap_or(5_000_000.0) * 0.99).max(1_000.0);
    let s = 10.0_f64.powi(p.log10().floor() as i32 - 3);
    rpc_ok(ctx, sess, "marketProxy", "PlacePlexSellOrder", &[PyRep::Real((p / s).floor() * s), PyRep::Integer(packs * 500), PyRep::Integer(90)]).await;
}

pub fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        PyRep::Object { args, .. } => kv_get(args, key),
        PyRep::SubStruct(inner) | PyRep::SubStream(inner) => kv_get(inner, key),
        _ => None,
    }
}

pub fn round_four_sig_figs(val: f64) -> f64 {
    if val <= 0.0 { return 100.0; }
    let scale = 10.0_f64.powi(val.log10().floor() as i32 - 3);
    (val / scale).ceil() * scale
}

pub async fn buy_item_at_station(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, station_id: i64, type_id: u32, qty: i64) -> i64 {
    let before_ids: HashSet<i64> = ctx.db.with_snapshot(|s| s.items.values().filter(|it| it.owner_id == char_id && it.type_id == type_id).map(|it| it.item_id).collect());
    let is_skill = ctx.sde.types.get(&type_id).is_some_and(|t| t.category_id == 16);
    let mut buy_price = ctx.sde.types.get(&type_id).map_or(5_000.0, |t| (t.base_price.max(100.0)) * 1.25);
    let station_id = sess.station_id.unwrap_or(station_id);
    if !is_skill {
        if let Some(min_sell) = ctx.db.with_snapshot(|s| s.market_orders.values().filter(|o| !o.is_bid && o.station_id == station_id && o.type_id == type_id && o.vol_remaining > 0).map(|o| o.price.to_f64_isk()).reduce(f64::min)) {
            buy_price = (min_sell * 1.05).max(1.0);
        }
    }
    rpc_ok(ctx, sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(station_id), PyRep::Integer(i64::from(type_id)), PyRep::Real(round_four_sig_figs(buy_price)),
        PyRep::Integer(qty), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ]).await;
    ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|it| it.owner_id == char_id && it.type_id == type_id && !before_ids.contains(&it.item_id))
            .or_else(|| snap.items.values().find(|it| it.owner_id == char_id && it.type_id == type_id)).map(|it| it.item_id)
    }).unwrap_or_else(|| panic!("Purchased item type {type_id} not found for {char_id}"))
}

pub async fn undock_from_station(ctx: &ServerContext, sess: &mut SessionState, sys_id: SolarSystemId) {
    rpc_ok(ctx, sess, "stationSvc", "Undock", &[]).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.step_manual(2.0).await;
    ctx.advance_secs(32);
    let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await;
}

pub async fn wait_for_warp_end(ctx: &ServerContext, sys_id: SolarSystemId, ship_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut saw_warp = false;
    for i in 0..80 {
        match sys_h.get_ball_snapshot(ship_id).await.ok().flatten() {
            Some((_, b)) if matches!(b.mode, eve_destiny::MovementMode::Warp { .. }) => { saw_warp = true; let _ = sys_h.step_manual(60.0).await; }
            _ if saw_warp || i >= 1 => break,
            _ => { let _ = sys_h.step_manual(15.0).await; }
        }
    }
}

pub async fn move_ship_to_ball(ctx: &ServerContext, sess: &mut SessionState, sys_id: SolarSystemId, ship_id: i64, stuff_type: &str, target_rep: PyRep) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let b_oid = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await as u64;
    let empty = HashMap::new();
    for hop in 0..24 {
        if let Some(tid) = target_rep.as_i64() {
            let Some(((_, s), (_, t))) = sys_h.get_ball_snapshot(ship_id).await.ok().flatten().zip(sys_h.get_ball_snapshot(tid).await.ok().flatten()) else { break };
            if s.position.distance(t.position) < 151_000.0 { break; }
        } else if hop > 0 { break; }
        match ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdWarpToStuff", &[PyRep::String(stuff_type.into()), target_rep.clone()], &empty, None).await {
            Ok(_) => wait_for_warp_end(ctx, sys_id, ship_id).await,
            Err(e) if e.code == "ShipInvulnerable" && hop < 22 => {
                let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdStop", &[], &empty, None).await;
                let _ = sys_h.step_manual(60.0).await;
                let _ = sys_h.step_manual(10.0).await;
            }
            Err(e) if e.code == "NotEnoughCapacitor" && hop < 22 => { for _ in 0..4 { let _ = sys_h.step_manual(60.0).await; } }
            Err(_) => break,
        }
    }
    let Some(tid) = target_rep.as_i64() else { return; };
    let is_surf_target = ctx.sde.jumps.contains_key(&tid) || ctx.sde.stations.contains_key(&tid);
    let in_range = |d: f64, sr: f64, tr: f64| if is_surf_target { (d - tr - sr).max(0.0) <= 2_100.0 } else { d <= 2_100.0 };
    let Some(((_, s), (_, t))) = sys_h.get_ball_snapshot(ship_id).await.ok().flatten().zip(sys_h.get_ball_snapshot(tid).await.ok().flatten()) else { return; };
    let d0 = s.position.distance(t.position);
    if in_range(d0, s.radius, t.radius) || d0 >= 200_000.0 { return; }
    let prop_id = ctx.db.with_snapshot(|s| s.items.values().find(|it| it.location_id == ship_id && (19..=26).contains(&(it.flag as u16)) && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 46)).map(|it| it.item_id));
    if d0 > 5_000.0 {
        if let Some(pid) = prop_id {
            if ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pid)], &empty, None).await.is_err() {
                let _ = sys_h.step_manual(65.0).await;
                let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pid)], &empty, None).await;
            }
        }
    }
    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdGotoPoint", &[PyRep::Real(t.position.x), PyRep::Real(t.position.y), PyRep::Real(t.position.z)], &empty, None).await;
    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &empty, None).await;
    for iter in 0..120_u32 {
        let Some(((_, s2), (_, t2))) = sys_h.get_ball_snapshot(ship_id).await.ok().flatten().zip(sys_h.get_ball_snapshot(tid).await.ok().flatten()) else { break };
        let d = s2.position.distance(t2.position);
        if in_range(d, s2.radius, t2.radius) { break; }
        let rem_d = if is_surf_target { (d - t2.radius - s2.radius).max(0.0) } else { d };
        if rem_d < 4_500.0 {
            if let Some(pid) = prop_id { let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(pid)], &empty, None).await; }
        }
        if iter % 15 == 14 || !matches!(s2.mode, eve_destiny::MovementMode::GotoDirection { .. } | eve_destiny::MovementMode::Follow { .. }) || s2.speed_fraction < 0.99 {
            let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdGotoPoint", &[PyRep::Real(t2.position.x), PyRep::Real(t2.position.y), PyRep::Real(t2.position.z)], &empty, None).await;
            let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &empty, None).await;
        }
        let max_step = if rem_d < 4_500.0 { 1.0 } else { 55.0 };
        let _ = sys_h.step_manual(((rem_d / s2.max_velocity.max(50.0)) * 0.85).clamp(0.4, max_step)).await;
    }
    if let Some(pid) = prop_id { let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(pid)], &empty, None).await; }
    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdStop", &[], &empty, None).await;
}

pub async fn warp_and_dock_at_station(ctx: &ServerContext, sess: &mut SessionState, ship_id: i64, sys_id: SolarSystemId, target_station_id: i64) {
    if sess.station_id == Some(target_station_id) { return; }
    if sess.station_id.is_some() { undock_from_station(ctx, sess, sess.solar_system_id.unwrap_or(sys_id)).await; }
    move_ship_to_ball(ctx, sess, sys_id, ship_id, "item", PyRep::Integer(target_station_id)).await;
    ctx.advance_secs(11);
    let b_oid = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await as u64;
    ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdDock", &[PyRep::Integer(target_station_id)], &HashMap::new(), None).await.expect("CmdDock");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..10 {
        if sess.character_id.is_some_and(|cid| ctx.db.get_character(cid).ok().and_then(|c| c.station_id) == Some(target_station_id)) { break; }
        let _ = sys_h.step_manual(10.0).await; ctx.advance_secs(10);
    }
    if let Some(ch) = sess.character_id.and_then(|cid| ctx.db.get_character(cid).ok()) {
        sess.solar_system_id = Some(ch.solar_system_id); sess.station_id = ch.station_id; sess.region_id = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.region_id);
    }
}
pub async fn fly_stargates_to(ctx: &ServerContext, sess: &mut SessionState, dest_sys: SolarSystemId) {
    let start_sys = sess.solar_system_id.unwrap_or(JITA_SYS);
    if start_sys == dest_sys { return; }
    if sess.station_id.is_some() { undock_from_station(ctx, sess, start_sys).await; ctx.advance_secs(11); }
    let ship_id = sess.ship_id.expect("active ship in fly_stargates_to");
    let (mut visited, mut prev, mut queue) = (HashSet::from([start_sys]), HashMap::new(), VecDeque::from([start_sys]));
    while let Some(cur) = queue.pop_front() {
        if cur == dest_sys { break; }
        for j in ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == cur && (j.to_solar_system_id != 30_100_000 || dest_sys == 30_100_000)) { if visited.insert(j.to_solar_system_id) { prev.insert(j.to_solar_system_id, (cur, j.stargate_id, j.destination_stargate_id)); queue.push_back(j.to_solar_system_id); } }
    }
    let (mut hops, mut trace) = (Vec::new(), dest_sys);
    while trace != start_sys { let (p_sys, fg, tg) = prev[&trace]; hops.push((p_sys, fg, tg, trace)); trace = p_sys; }
    hops.reverse();
    for (cur_sys, fg, _tg, next_sys) in hops {
        let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(cur_sys))).await;
        move_ship_to_ball(ctx, sess, cur_sys, ship_id, "item", PyRep::Integer(fg)).await;
        ctx.advance_secs(11);
        rpc_ok(ctx, sess, "beyonce", "CmdStargateJump", &[PyRep::Integer(fg), PyRep::Integer(i64::from(next_sys))]).await;
        sess.solar_system_id = Some(next_sys);
    }
    sess.solar_system_id = Some(dest_sys); sess.station_id = None; sess.region_id = ctx.sde.solar_systems.get(&dest_sys).map(|s| s.region_id); let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(dest_sys))).await;
}
pub fn drain_all_notifications(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    if let Ok(mut pending) = ctx.pending_notifications.write() { if let Some(items) = pending.remove(&char_id) { out.extend(items.into_iter().map(|i| (i.event, i.args))); } }
    out
}
pub async fn upgrade_to_omega(ctx: &ServerContext, sess: &mut SessionState) {
    if sess.address.is_empty() { sess.address = "127.0.0.1:26000".into(); }
    rpc_ok(ctx, sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(4), PyRep::Integer(35)]).await;
    rpc_ok(ctx, sess, "storeManager", "buy_offer", &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(60)]).await;
}
fn collect_req_skills(sde: &eve_sde::SdeRegistry, tid: u32, out: &mut std::collections::BTreeMap<u32, u8>, depth: u8) {
    if depth > 8 { return; }
    let Some(t) = sde.types.get(&tid) else { return };
    for (sa, la) in [(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
        if let Some(&sf) = t.default_attributes.get(&sa) {
            let sid = sf.round() as u32;
            if sid > 0 {
                let lvl = t.default_attributes.get(&la).copied().unwrap_or(1.0).round().clamp(1.0, 5.0) as u8;
                out.entry(sid).and_modify(|e| *e = (*e).max(lvl)).or_insert(lvl);
                collect_req_skills(sde, sid, out, depth + 1);
            }
        }
    }
}
pub async fn ensure_skills_for_types(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, station_id: i64, type_ids: &[u32]) {
    let mut req = std::collections::BTreeMap::from([(3300_u32, 1_u8), (3327, 1), (3413, 4), (3417, 4), (3418, 4), (3426, 4), (3449, 4), (3452, 4), (3455, 4)]);
    let mut needs_omega = false;
    for &tid in type_ids {
        collect_req_skills(&ctx.sde, tid, &mut req, 0);
        if let Some(t) = ctx.sde.types.get(&tid) {
            if t.category_id == 16 { req.entry(tid).and_modify(|e| *e = (*e).max(4)).or_insert(4); }
            if t.default_attributes.get(&861).copied().unwrap_or(0.0) > 0.0 {
                for &(js, jl) in &[(3456_u32, 4_u8), (21_611, 5)] { req.entry(js).and_modify(|e| *e = (*e).max(jl)).or_insert(jl); collect_req_skills(&ctx.sde, js, &mut req, 0); }
            }
            if matches!(t.group_id, 485 | 547 | 4594 | 1538 | 30 | 659 | 883) || (t.category_id == 6 && !matches!(t.group_id, 25 | 237 | 31)) { needs_omega = true; }
        }
    }
    let race_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.race_id).unwrap_or(1));
    if needs_omega || req.iter().any(|(&sid, &lvl)| lvl > eve_server::services::character_skills::alpha_skill_cap_for_character(&ctx.sde, race_id, sid)) {
        upgrade_to_omega(ctx, sess).await;
    }
    for _ in 0..24 {
        let mut queue: Vec<PyRep> = Vec::new();
        for (&sid, &req_lvl) in &req {
            let cur_lvl = ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
            if cur_lvl >= req_lvl { continue; }
            let mut sub = std::collections::BTreeMap::new();
            collect_req_skills(&ctx.sde, sid, &mut sub, 7);
            if !sub.iter().all(|(&p_id, &p_lvl)| ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&p_id)).map(|r| r.level).unwrap_or(0)) >= p_lvl) { continue; }
            if ctx.db.with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).is_none()) {
                let book_id = buy_item_at_station(ctx, sess, char_id, station_id, sid, 1).await;
                rpc_ok(ctx, sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(book_id)]).await;
            }
            for l in (cur_lvl + 1)..=req_lvl { queue.push(PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(l))])); }
        }
        if queue.is_empty() { break; }
        rpc_ok(ctx, sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(queue)]).await;
        ctx.advance_secs(86_400 * 22);
        let _ = rpc_ok(ctx, sess, "skillMgr2", "GetSkills", &[]).await;
    }
}

pub async fn buy_fit_combat_ship(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, station_id: i64, ship_type_id: u32, turret_type_id: u32, ammo_type_id: u32, gun_count: usize) -> (i64, Vec<i64>) {
    buy_fit_combat_ship_with_ammo(ctx, sess, char_id, station_id, ship_type_id, turret_type_id, ammo_type_id, gun_count, 900).await
}

pub async fn buy_fit_combat_ship_with_ammo(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, station_id: i64, mut ship_type_id: u32, mut turret_type_id: u32, mut ammo_type_id: u32, mut gun_count: usize, ammo_qty: i64) -> (i64, Vec<i64>) {
    let slots = |tid: u32| ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&102).copied()).unwrap_or(0.0).max(0.0) as usize;
    if gun_count > 0 && slots(ship_type_id) == 0 {
        if let Some(grp) = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id) {
            ship_type_id = if grp == 26 && slots(623) > 0 { 623 } else {
                ctx.sde.types.values().filter(|t| t.published && t.category_id == 6 && t.group_id == grp && slots(t.type_id) > 0)
                    .max_by_key(|t| slots(t.type_id)).map_or(ship_type_id, |t| t.type_id)
            };
        }
    }
    gun_count = gun_count.min(slots(ship_type_id));
    let mut extras: Vec<(u32, i64)> = Vec::new();
    let mut train_tids: Vec<u32> = vec![ship_type_id];
    if gun_count > 0 {
        let ship_pg = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&11).copied()).unwrap_or(100.0);
        let mid_slots = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&13).copied()).unwrap_or(0.0) as usize;
        let low_slots = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&12).copied()).unwrap_or(0.0) as usize;
        let want_sz = ctx.sde.types.get(&ship_type_id).map_or(1.0, |s| {
            if matches!(s.group_id, 30 | 485 | 513 | 547 | 659 | 883 | 4594) || s.volume >= 1_000_000.0 { 4.0 }
            else if s.volume >= 200_000.0 { 3.0 } else if s.volume >= 50_000.0 { 2.0 } else { 1.0 }
        });
        let cur_sz = ctx.sde.types.get(&turret_type_id).and_then(|t| t.default_attributes.get(&128).copied()).unwrap_or(1.0);
        let cur_pg = ctx.sde.types.get(&turret_type_id).and_then(|t| t.default_attributes.get(&30).copied()).unwrap_or(10.0);
        if (cur_sz - want_sz).abs() > 0.1 || cur_pg > ship_pg || ctx.sde.types.get(&turret_type_id).is_some_and(|t| !matches!(t.group_id, 53 | 55 | 74 | 1986)) {
            let mut cands: Vec<_> = ctx.sde.types.values().filter(|t| {
                t.published && t.meta_group_id.unwrap_or(1) == 1 && matches!(t.group_id, 55 | 53 | 74) && t.base_price > 0.0 && !t.name.starts_with("Civilian")
                    && t.default_attributes.get(&128).copied().is_some_and(|sz| (sz - want_sz).abs() < 0.1 || (want_sz > 3.5 && (sz - 3.0).abs() < 0.1))
                    && t.default_attributes.get(&30).copied().unwrap_or(10.0) <= ship_pg
                    && t.default_attributes.get(&604).is_some_and(|&cg| ctx.sde.types.values().any(|a| {
                        a.published && a.category_id == 8 && a.group_id == cg as u32 && a.default_attributes.get(&128).copied() == t.default_attributes.get(&128).copied()
                    }))
            }).collect();
            cands.sort_by_key(|t| {
                let dps = t.default_attributes.get(&64).copied().unwrap_or(1.0) / (t.default_attributes.get(&51).copied().unwrap_or(5000.0) / 1000.0);
                (u8::from(t.default_attributes.get(&128).copied() != Some(want_sz)), u8::from(t.group_id != 55), -(dps * 10_000.0) as i64)
            });
            if let Some(alt_t) = cands.first() { turret_type_id = alt_t.type_id; }
        }
        let gun_pg = ctx.sde.types.get(&turret_type_id).and_then(|t| t.default_attributes.get(&30).copied()).unwrap_or(10.0).max(1.0);
        gun_count = gun_count.min((((ship_pg * 0.80) / gun_pg).floor() as usize).max(1));
        if let Some(t) = ctx.sde.types.get(&turret_type_id) {
            let (rg, rs) = (t.default_attributes.get(&604).map(|&v| v as u32), t.default_attributes.get(&128).copied());
            if !ctx.sde.types.get(&ammo_type_id).is_some_and(|a| Some(a.group_id) == rg && a.default_attributes.get(&128).copied() == rs) {
                if let Some(aa) = ctx.sde.types.values().filter(|a| a.published && a.meta_group_id.unwrap_or(1) == 1 && a.category_id == 8 && Some(a.group_id) == rg && !a.name.starts_with("Civilian") && a.default_attributes.get(&128).copied() == rs).max_by_key(|a| ((a.default_attributes.get(&114).copied().unwrap_or(0.0) + a.default_attributes.get(&116).copied().unwrap_or(0.0) + a.default_attributes.get(&117).copied().unwrap_or(0.0) + a.default_attributes.get(&118).copied().unwrap_or(0.0)) * 100.0) as i64) { ammo_type_id = aa.type_id; }
            }
        }
        train_tids.extend([turret_type_id, ammo_type_id]);
        let ship_cpu = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&48).copied()).unwrap_or(200.0) * 1.20;
        let gun_cpu = ctx.sde.types.get(&turret_type_id).and_then(|t| t.default_attributes.get(&50).copied()).unwrap_or(5.0);
        let mut free_pg = (ship_pg - gun_pg * (gun_count as f64)).max(0.0);
        let mut free_cpu = (ship_cpu - gun_cpu * (gun_count as f64)).max(0.0);
        let pg_of = |id: u32| ctx.sde.types.get(&id).and_then(|t| t.default_attributes.get(&30).copied()).unwrap_or(999_999.0);
        let cpu_of = |id: u32| ctx.sde.types.get(&id).and_then(|t| t.default_attributes.get(&50).copied()).unwrap_or(0.0);
        let prop_name = if want_sz >= 3.0 { "500MN Microwarpdrive II" } else if want_sz >= 2.0 { "50MN Microwarpdrive II" } else { "10MN Afterburner II" };
        if let Some(prop) = ctx.sde.types.values().find(|t| t.published && t.category_id == 7 && t.name == prop_name).map(|t| t.type_id) {
            if mid_slots >= 2 && pg_of(prop) <= free_pg && cpu_of(prop) <= free_cpu { free_pg -= pg_of(prop); free_cpu -= cpu_of(prop); extras.push((prop, 20)); train_tids.push(prop); }
        }
        let shield = if pg_of(3831) <= free_pg && cpu_of(3831) <= free_cpu { 3831 } else { 380 };
        if mid_slots >= 3 && pg_of(shield) <= free_pg && cpu_of(shield) <= free_cpu { free_pg -= pg_of(shield); free_cpu -= cpu_of(shield); extras.push((shield, 21)); train_tids.push(shield); }
        let armor = if pg_of(11269) <= free_pg && cpu_of(11269) <= free_cpu { 11269 } else { 11299 };
        if low_slots >= 1 && pg_of(armor) <= free_pg && cpu_of(armor) <= free_cpu { free_pg -= pg_of(armor); free_cpu -= cpu_of(armor); extras.push((armor, 11)); train_tids.push(armor); }
        let dmg_name = match ctx.sde.types.get(&turret_type_id).map(|t| t.group_id) {
            Some(55) => "Gyrostabilizer II", Some(74) => "Magnetic Field Stabilizer II", Some(1986) => "Entropic Radiation Sink II", _ => "Heat Sink II",
        };
        if let Some(dmg_mod) = ctx.sde.types.values().find(|t| t.published && t.category_id == 7 && t.name == dmg_name).map(|t| t.type_id) {
            for slot_idx in 0..low_slots.saturating_sub(1).min(3) {
                if pg_of(dmg_mod) <= free_pg && cpu_of(dmg_mod) <= free_cpu { free_pg -= pg_of(dmg_mod); free_cpu -= cpu_of(dmg_mod); extras.push((dmg_mod, 12 + slot_idx as i64)); }
            }
            train_tids.push(dmg_mod);
        }
    }
    ensure_skills_for_types(ctx, sess, char_id, station_id, &train_tids).await;
    let ship_id = buy_item_at_station(ctx, sess, char_id, station_id, ship_type_id, 1).await;
    ctx.advance_secs(11);
    rpc_ok(ctx, sess, "ship", "AssembleShip", &[PyRep::List(vec![PyRep::Integer(ship_id)])]).await;
    rpc_ok(ctx, sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await;
    ctx.advance_secs(11);
    sess.ship_id = Some(ship_id);
    let mut guns = Vec::new();
    for idx in 0..gun_count {
        let gid = buy_item_at_station(ctx, sess, char_id, station_id, turret_type_id, 1).await;
        rpc_ok(ctx, sess, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(ship_id), PyRep::Integer(27 + idx as i64)]).await;
        guns.push(gid);
    }
    for (eid, flag) in extras {
        let mid_item = buy_item_at_station(ctx, sess, char_id, station_id, eid, 1).await;
        rpc_ok(ctx, sess, "invbroker", "Add", &[PyRep::Integer(mid_item), PyRep::Integer(ship_id), PyRep::Integer(flag)]).await;
    }
    if !guns.is_empty() {
        let ammo_id = buy_item_at_station(ctx, sess, char_id, station_id, ammo_type_id, ammo_qty.max(1)).await;
        rpc_ok(ctx, sess, "invbroker", "Add", &[PyRep::Integer(ammo_id), PyRep::Integer(ship_id), PyRep::Integer(5)]).await;
        let gun_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
        rpc_ok(ctx, sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(gun_reps), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await;
    }
    (ship_id, guns)
}
pub async fn warp_and_engage_ball(ctx: &ServerContext, sess: &mut SessionState, sys_id: SolarSystemId, ship_id: i64, guns: &[i64], target_ball_id: i64, warp_stuff_type: &str, warp_target: PyRep) {
    fleet_warp_and_engage_ball(ctx, &mut [(sess, ship_id, guns)], sys_id, target_ball_id, warp_stuff_type, warp_target).await;
}
pub async fn fleet_warp_and_engage_ball(ctx: &ServerContext, fleet: &mut [(&mut SessionState, i64, &[i64])], sys_id: SolarSystemId, target_ball_id: i64, warp_stuff_type: &str, warp_target: PyRep) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys.get_combat_state(target_ball_id).await.ok().flatten().is_none() || sys.get_ball_snapshot(target_ball_id).await.ok().flatten().is_none() { return; }
    let empty = HashMap::new();
    let mut best_dist = 25_000.0_f64;
    let mut w_cyc = 8.0_f64;
    let mut b_oids = Vec::with_capacity(fleet.len());
    for (sess, ship_id, guns) in fleet.iter_mut() {
        let b_oid = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await as u64;
        b_oids.push(b_oid);
        let (w_opt, w_fo, cyc, is_msl) = guns.first().and_then(|&gid| ctx.db.with_snapshot(|s| s.items.get(&gid).and_then(|it| ctx.sde.types.get(&it.type_id)))).map_or((12_000.0, 8_000.0, 8.0, false), |t| (
            t.default_attributes.get(&54).copied().unwrap_or(10_000.0),
            t.default_attributes.get(&158).copied().unwrap_or(5_000.0),
            (t.default_attributes.get(&51).copied().unwrap_or(8_000.0) / 1000.0).clamp(4.0, 15.0),
            matches!(t.group_id, 506..=511),
        ));
        w_cyc = w_cyc.min(cyc);
        let engage_dist = if is_msl { 25_000.0 } else { (w_opt + w_fo * 0.65).max(8_000.0) };
        if b_oids.len() == 1 { best_dist = engage_dist; } else { best_dist = best_dist.min(engage_dist); }
        let dist_to = || async { sys.get_ball_snapshot(*ship_id).await.ok().flatten().zip(sys.get_ball_snapshot(target_ball_id).await.ok().flatten()).map_or(0.0, |((_, sb), (_, tb))| sb.position.distance(tb.position)) };
        for hop in 0..24 {
            if dist_to().await < 151_000.0 { break; }
            if hop > 0 { let _ = sys.step_manual(240.0).await; }
            match ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdWarpToStuff", &[PyRep::String(warp_stuff_type.into()), warp_target.clone()], &empty, None).await {
                Ok(_) => wait_for_warp_end(ctx, sys_id, *ship_id).await,
                Err(e) if (e.code == "NotEnoughCapacitor" || e.code == "ShipInvulnerable") && hop < 22 => { let _ = sys.step_manual(65.0).await; }
                Err(_) => break,
            }
        }
        if dist_to().await >= 151_000.0 {
            if let Some((_, tb)) = sys.get_ball_snapshot(target_ball_id).await.ok().flatten() {
                if let Some(rigid) = sys.list_balls().await.unwrap_or_default().into_iter().filter(|b| b.id != *ship_id && matches!(b.mode, eve_destiny::MovementMode::Rigid)).min_by(|a, b| a.position.distance(tb.position).partial_cmp(&b.position.distance(tb.position)).unwrap_or(std::cmp::Ordering::Equal)) {
                    let _ = sys.step_manual(240.0).await;
                    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(rigid.id)], &empty, None).await;
                    wait_for_warp_end(ctx, sys_id, *ship_id).await;
                }
            }
        }
        let prop_id = ctx.db.with_snapshot(|s| s.items.values().find(|it| it.location_id == *ship_id && (19..=26).contains(&(it.flag as u16)) && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 46)).map(|it| it.item_id));
        if dist_to().await > engage_dist {
            if let Some(pid) = prop_id {
                if ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pid)], &empty, None).await.is_err() {
                    let _ = sys.step_manual(65.0).await;
                    let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(pid)], &empty, None).await;
                }
            }
            let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdFollowBall", &[PyRep::Integer(target_ball_id), PyRep::Real(1_500.0)], &empty, None).await;
            let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &empty, None).await;
            for _ in 0..80 {
                let Some(((_, sb), (_, tb))) = sys.get_ball_snapshot(*ship_id).await.ok().flatten().zip(sys.get_ball_snapshot(target_ball_id).await.ok().flatten()) else { break };
                let d = sb.position.distance(tb.position);
                if d <= engage_dist * 0.85 { break; }
                let max_s = if d < engage_dist * 1.5 { 2.0 } else { 55.0 };
                let _ = sys.step_manual(((d - engage_dist * 0.7) / sb.max_velocity.max(80.0) * 0.8).clamp(0.5, max_s)).await;
            }
        }
        if let Some(pid) = prop_id { let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(pid)], &empty, None).await; }
        let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oid), "CmdStop", &[], &empty, None).await;
        if let Ok(locks) = sys.get_target_locks(*ship_id).await {
            for lk in locks.into_iter().filter(|l| l.target_id != target_ball_id) { let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(lk.target_id)], &empty, None).await; }
        }
        let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_ball_id)], &empty, None).await;
    }
    for _ in 0..4 {
        let mut locked = true;
        for (_, sid, _) in fleet.iter() { if !sys.get_target_locks(*sid).await.ok().is_some_and(|ls| ls.iter().any(|l| l.target_id == target_ball_id && l.locked)) { locked = false; break; } }
        if locked { break; }
        let _ = sys.step_manual(24.0).await;
    }
    for _step in 0..220 {
        if sys.get_combat_state(target_ball_id).await.ok().flatten().is_none() || sys.get_ball_snapshot(target_ball_id).await.ok().flatten().is_none() { break; }
        let mut any_alive = false;
        for (idx, (sess, ship_id, guns)) in fleet.iter_mut().enumerate() {
            if sys.get_combat_state(*ship_id).await.ok().flatten().is_none() || sys.get_ball_snapshot(*ship_id).await.ok().flatten().is_none() { continue; }
            any_alive = true;
            if let Some(((_, sb), (_, tb))) = sys.get_ball_snapshot(*ship_id).await.ok().flatten().zip(sys.get_ball_snapshot(target_ball_id).await.ok().flatten()) {
                if sb.position.distance(tb.position) > best_dist * 1.15 {
                    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oids[idx]), "CmdFollowBall", &[PyRep::Integer(target_ball_id), PyRep::Real((best_dist * 0.5).max(1_200.0))], &empty, None).await;
                    let _ = ctx.dispatch_rpc(sess, "beyonce", Some(b_oids[idx]), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &empty, None).await;
                }
            }
            let (mut need_reload, mut wait_reload) = (false, false);
            for &gid in *guns {
                match ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(target_ball_id)], &empty, None).await {
                    Ok(rep) if rep.dict_get("destroyed") == Some(&PyRep::Bool(true)) => { let _ = sys.step_manual(1.0).await; return; }
                    Err(e) if e.code == "NoAmmoLoaded" => { need_reload = true; }
                    Err(e) if matches!(e.code.as_str(), "ModuleIsReloading" | "ModuleReloading") => { wait_reload = true; }
                    Ok(_) | Err(_) => {}
                }
            }
            if need_reload { if let Some(aid) = guns.first().and_then(|gid| ctx.db.with_snapshot(|s| s.items.get(gid).and_then(|m| ctx.sde.types.get(&m.type_id)).and_then(|wt| s.items.values().find(|i| i.location_id == *ship_id && matches!(i.flag as u16, 5 | 136) && i.quantity >= guns.len() as i32 && ctx.sde.types.get(&i.type_id).is_some_and(|ct| ServerContext::is_charge_compatible_with_weapon(wt, ct))).map(|i| i.item_id)))) { let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(*ship_id), PyRep::List(guns.iter().map(|&g| PyRep::Integer(g)).collect()), PyRep::List(vec![PyRep::Integer(aid)])], &empty, None).await; wait_reload = true; } }
            if wait_reload { ctx.advance_secs(11); }
        }
        if !any_alive { return; }
        let _ = sys.step_manual(w_cyc).await;
    }
}
pub async fn find_kspace_entrance_to_c5(ctx: &ServerContext, required_jump_mass_kg: f64) -> (SolarSystemId, String, SolarSystemId) {
    let jita_pos = ctx.sde.solar_systems.get(&JITA_SYS).map(|s| glam::DVec3::from_array(s.galactic_pos_ly)).unwrap_or(glam::DVec3::ZERO);
    let mut kspace: Vec<(SolarSystemId, f64)> = ctx.sde.solar_systems.iter().filter(|(&sid, sys)| sid < 31_000_000 && (sid as usize) % 6 == 4 && sys.region_id != 10_000_070 && sys.security_status < 0.45).filter_map(|(&sid, sys)| { let d = glam::DVec3::from_array(sys.galactic_pos_ly).distance(jita_pos); (d <= 12.0).then_some((sid, d)) }).collect();
    kspace.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    for (k_sys, _) in kspace {
        let (sigs, _) = eve_server::services::space_destiny::ensure_system_signatures_and_anomalies(ctx, k_sys).await;
        if let Some((sig_id, dest)) = sigs.into_iter().find_map(|s| (s.sig_group == "Wormhole" && !s.signature_id.starts_with("K162-") && !s.collapsed && s.max_jump_mass_kg >= required_jump_mass_kg && s.remaining_mass_kg > 0.0).then_some(s.destination_system_id).flatten().filter(|&d| eve_server::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, d).0 == 5).map(|d| (s.signature_id, d))) { return (k_sys, sig_id, dest); }
    }
    panic!("seeded K-space wormhole entrance into C5 wormhole system");
}
pub async fn jump_through_wormhole_to(ctx: &ServerContext, sess: &mut SessionState, ship_id: i64, k_entry_sys: SolarSystemId, k_sig_id: &str, target_wh_sys: SolarSystemId) {
    fly_stargates_to(ctx, sess, k_entry_sys).await;
    let wh_ball_id = k_sig_id.strip_prefix("K162-").map_or_else(|| eve_server::services::space_destiny::wormhole_item_id_for_signature(k_entry_sys, k_sig_id), |orig| eve_server::services::space_destiny::wormhole_exit_item_id_for_signature(k_entry_sys, orig));
    move_ship_to_ball(ctx, sess, k_entry_sys, ship_id, "item", PyRep::Integer(wh_ball_id)).await;
    let sys_h = ctx.systems.get_or_spawn(k_entry_sys);
    let wh_pos = ctx.db.with_snapshot(|snap| snap.cosmic_signatures.get(&k_entry_sys).and_then(|sigs| sigs.iter().find(|s| s.signature_id == k_sig_id || k_sig_id.strip_prefix("K162-") == Some(&s.signature_id)).map(|s| eve_server::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(&ctx.sde, s))));
    if let Some(pos) = wh_pos { let _ = sys_h.set_ball_position(ship_id, pos + glam::DVec3::new(500.0, 0.0, 0.0)).await; }
    ctx.advance_secs(11);
    rpc_ok(ctx, sess, "wormholeMgr", "WormholeJump", &[PyRep::String(k_sig_id.into())]).await;
    let _ = ctx.systems.get_or_spawn(target_wh_sys).step_manual(8.0).await;
    sess.solar_system_id = Some(target_wh_sys); sess.station_id = None; sess.region_id = ctx.sde.solar_systems.get(&target_wh_sys).map(|s| s.region_id); let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(target_wh_sys))).await;
    assert_eq!(sess.solar_system_id, Some(target_wh_sys));
}
