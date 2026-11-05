//! Integration tests for batch 37 exclusive `feat:` parity rows:
//! - `feat:pve.operations-tutorial`
//! - `feat:pve.seasonal-login`

#![forbid(unsafe_code)]

use std::{collections::{HashMap, HashSet}, sync::Arc};
use eve_db::{CharacterRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository, corp_roles};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{SessionState, service_roles};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
use eve_server::services::{
    account_services,
    combat_crimewatch::combat_npc_ai::{destroy_specialized_npc_and_queue_bounty, spawn_specialized_npc_entity},
    operations_manager_helpers::{OperationDefinitions, get_operation_defs},
    station_structure::structure_upwell_ops::{load_ext_state, save_ext_state},
};
use glam::DVec3;
use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const AMARR_SYS: u32 = 30_002_187;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(SdeRegistry::open(&SdeRegistry::default_db_path()).expect("open SDE"));
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
}

async fn rpc_kw(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep], kw: &HashMap<String, PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, kw, None).await
}

async fn login_demo_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    let (user, pass) = if char_id == PILOT_2 { ("other", "other-secret") } else { ("demo", "demo-secret") };
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SYS);
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    ctx.advance_secs(15);
    ctx.db.with_mut_snapshot(|snap| if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.solar_system_id = JITA_SYS; ch.station_id = Some(JITA_STATION); ch.structure_id = None; ch.home_station_id = JITA_STATION;
    });
    sess
}

fn seed_corp_and_pilots(ctx: &ServerContext, corp_id: i64, ceo_id: i64, members_with_roles: &[(i64, u64, &str)]) {
    let uid_map: HashMap<i64, u64> = members_with_roles.iter().map(|&(cid, _, _)| (cid, ctx.db.provision_player_account(&format!("b37_{cid}"), "b37-pw"))).collect();
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&PILOT_1).expect("demo pilot 1").clone();
        let (mut member_ids, mut role_map) = (HashSet::new(), HashMap::new());
        for &(cid, roles, name) in members_with_roles {
            let (uid, ship_id) = (uid_map[&cid], 770_000_000 + cid);
            member_ids.insert(cid);
            role_map.insert(cid, roles);
            snap.items.entry(ship_id).or_insert(InventoryItemRecord { item_id: ship_id, type_id: 587, owner_id: cid, location_id: i64::from(JITA_SYS), flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
            let ch = snap.characters.entry(cid).or_insert_with(|| CharacterRecord {
                character_id: cid, user_id: uid, name: name.to_string(), corporation_id: corp_id, solar_system_id: JITA_SYS, constellation_id: 20_000_020,
                region_id: 10_000_002, active_ship_id: ship_id, home_station_id: JITA_STATION, balance: IskCents(100_000_000_00), race_id: 1, bloodline_id: 1, ancestry_id: 1, clone_grade: 2, ..base_char.clone()
            });
            ch.user_id = uid; ch.corporation_id = corp_id; ch.solar_system_id = JITA_SYS; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
        }
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: format!("Corp {corp_id}"), ticker: "TST".into(), ceo_id, hq_station_id: Some(JITA_STATION),
            members: member_ids, member_roles: role_map, master_wallet: IskCents(500_000_000_00), ..Default::default()
        });
    });
}

async fn make_space_session(ctx: &ServerContext, char_id: i64, corp_id: i64, corp_role: u64) -> SessionState {
    let (uid, ship_id) = ctx.db.with_snapshot(|s| { let ch = s.characters.get(&char_id).unwrap(); (ch.user_id, ch.active_ship_id) });
    let _ = ctx.db.acquire_online_lock(uid, char_id as u64, Some(char_id), true, ctx.now_unix());
    let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(uid); sess.character_id = Some(char_id); sess.corporation_id = Some(corp_id); sess.corp_role = corp_role;
    sess.solar_system_id = Some(JITA_SYS); sess.constellation_id = Some(20_000_020); sess.region_id = Some(10_000_002); sess.ship_id = Some(ship_id);
    sess
}

async fn seed_structure_with_services(ctx: &ServerContext, structure_id: i64, owner_corp_id: i64, name: &str, state: &str, modules: &[(i64, u32, InventoryFlag, bool)]) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(structure_id, UpwellStructureRecord {
            structure_id, type_id: 35_833, owner_corp_id, solar_system_id: JITA_SYS, name: name.to_string(), position: [1_000.0, 0.0, 0.0], state: state.to_string(),
            fuel_blocks: 2_400, quantum_core_installed: true, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0, hull_hp: 1_000_000.0, reinforced_until_unix: if state == "Anchoring" { now + 3_600 } else { 0 }, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let lf = ext.lifecycle.entry(structure_id).or_default();
        lf.last_fuel_tick_unix = Some(now);
        for &(item_id, type_id, flag, online) in modules {
            snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id: owner_corp_id, location_id: structure_id, flag, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.module_online_states.insert(item_id, online);
            if online && !lf.online_modules.contains(&item_id) { lf.online_modules.push(item_id); }
        }
        save_ext_state(snap, &ext);
    });
    let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball { id: structure_id, type_id: 35_833, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0e9, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Rigid }).await;
}

fn pick_sde_type_matching(ctx: &ServerContext, conds: &HashMap<String, Vec<String>>, default_tid: u32) -> u32 {
    if let Some(tid) = conds.get("typeID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()) { return tid; }
    if let Some(gid) = conds.get("groupID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()) {
        if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.group_id == gid) { return tid; }
    }
    default_tid
}

async fn ensure_dock_state(ctx: &ServerContext, sess: &mut SessionState, want_dock: bool) {
    if want_dock && sess.station_id.is_none() {
        ctx.advance_secs(15);
        let sid = sess.ship_id.unwrap();
        let sys = ctx.systems.get_or_spawn(JITA_SYS);
        let st_pos = sys.get_ball_snapshot(JITA_STATION).await.ok().flatten().map(|b| b.1.position).unwrap_or(DVec3::ZERO);
        let _ = sys.set_ball_position(sid, st_pos).await;
        rpc(ctx, sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION)]).await.unwrap();
    } else if !want_dock && sess.station_id.is_some() {
        ctx.advance_secs(15);
        rpc(ctx, sess, "stationSvc", "Undock", &[]).await.unwrap();
    }
}

async fn complete_op_authoritatively(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64, op_id: i64) {
    let defs = get_operation_defs(&ctx.sde);
    rpc(ctx, sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(JITA_SYS)), PyRep::Integer(cat_id)]).await.unwrap();
    for &tid in defs.op_tasks.get(&op_id).into_iter().flatten() {
        let ev = defs.task_event_names.get(&tid).cloned().unwrap_or_default();
        let target = defs.task_target_values.get(&tid).copied().unwrap_or(1);
        let conds = defs.task_event_conditions.get(&tid).cloned().unwrap_or_default();
        if ev.starts_with("client_") {
            let py_conds: Vec<(PyRep, PyRep)> = conds.iter().filter_map(|(k, vs)| {
                let v = vs.first()?;
                Some((PyRep::String(k.clone()), v.parse::<i64>().map(PyRep::Integer).unwrap_or_else(|_| PyRep::String(v.clone()))))
            }).collect();
            let mut kw = HashMap::new();
            if !py_conds.is_empty() { kw.insert("extra_data".into(), PyRep::Dict(py_conds)); }
            for _ in 0..target { rpc_kw(ctx, sess, "operationsManager", "process_client_event", &[PyRep::String(ev.clone())], &kw).await.unwrap(); }
            continue;
        }
        let saved_dock = sess.station_id.is_some();
        match ev.as_str() {
            "dockInStation" => { ensure_dock_state(ctx, sess, false).await; ensure_dock_state(ctx, sess, true).await; }
            "undockFromStation" => { ensure_dock_state(ctx, sess, true).await; ensure_dock_state(ctx, sess, false).await; }
            "shipBoarded" => {
                let stid = pick_sde_type_matching(ctx, &conds, 587);
                ensure_dock_state(ctx, sess, true).await;
                let sid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: stid, owner_id: char_id, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                ctx.advance_secs(15);
                rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(sid)]).await.unwrap();
            }
            "dungeonEntered" => {
                ensure_dock_state(ctx, sess, false).await;
                let site_id = defs.op_site_ids.get(&op_id).copied().or_else(|| conds.get("dungeonID").and_then(|v| v.first()).and_then(|s| s.parse::<i64>().ok())).unwrap_or(1);
                rpc(ctx, sess, "operationsManager", "start_site", &[PyRep::Integer(op_id), PyRep::Integer(site_id)]).await.unwrap();
            }
            "fitItemCount" => {
                ensure_dock_state(ctx, sess, true).await;
                let mtid = pick_sde_type_matching(ctx, &conds, 439);
                ctx.db.with_mut_snapshot(|s| {
                    if let Some(sid) = sess.ship_id { s.items.retain(|_, it| !(it.location_id == sid && matches!(it.flag as i64, 11..=34 | 92..=94))); }
                    let sk = s.skills.entry(char_id).or_default();
                    for (tid, _) in ctx.sde.required_skills_for_type(mtid).into_iter().chain(ctx.sde.required_skills_for_type(587)) {
                        sk.entry(tid).or_insert(eve_db::TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
                    }
                });
                let mut sid = sess.ship_id.unwrap();
                ctx.invalidate_ship_fitting(sid);
                let loc = sess.station_id.unwrap_or(sid);
                let flag = if sess.station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::Cargo };
                for _ in 0..target {
                    let mid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: mtid, owner_id: char_id, location_id: loc, flag, quantity: 1, singleton: false, version: 1, ..Default::default() }); id });
                    let (mut fitted, mut last_err) = (false, None);
                    for f in (27_i64..=34).chain(19..=26).chain(11..=18).chain(92..=94) {
                        match rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(sid), PyRep::Integer(f)]).await {
                            Ok(_) => { fitted = true; break; }
                            Err(e) => last_err = Some(e),
                        }
                    }
                    if !fitted {
                        let bsid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: 587, owner_id: char_id, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                        ctx.advance_secs(15);
                        rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(bsid)]).await.unwrap();
                        sid = sess.ship_id.unwrap();
                        for f in (27_i64..=34).chain(19..=26).chain(11..=18).chain(92..=94) {
                            match rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(sid), PyRep::Integer(f)]).await {
                                Ok(_) => { fitted = true; break; }
                                Err(e) => last_err = Some(e),
                            }
                        }
                    }
                    assert!(fitted, "failed to fit module {mtid} onto ship {sid}: {last_err:?}");
                }
            }
            "loadChargesCount" => {
                let ctid = pick_sde_type_matching(ctx, &conds, 222);
                let sid = sess.ship_id.unwrap();
                let loc = sess.station_id.unwrap_or(sid);
                let flag = if sess.station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::Cargo };
                let mut loaded = false;
                for try_tid in [ctid, 222, 182, 252, 209] {
                    let cid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: try_tid, owner_id: char_id, location_id: loc, flag, quantity: target as i32, singleton: false, version: 1, ..Default::default() }); id });
                    if rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(cid), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::HiSlot0 as i64)]).await.is_ok() { loaded = true; break; }
                }
                assert!(loaded, "failed to load charges onto ship {sid}");
            }
            "skillTrainingStarted" => {
                let sk_tid = conds.get("typeID").and_then(|v| v.first()).and_then(|s| s.parse::<u32>().ok()).unwrap_or(3_300);
                rpc(ctx, sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(i64::from(sk_tid)), PyRep::Integer(5)])])]).await.unwrap();
            }
            "timesWarped" | "warpToLocation" => {
                ensure_dock_state(ctx, sess, false).await;
                let sid = sess.ship_id.unwrap();
                let sys = ctx.systems.get_or_spawn(JITA_SYS);
                let _ = sys.set_ball_position(sid, DVec3::new(1_000_000_000.0, 0.0, 0.0)).await;
                rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION), PyRep::Real(0.0)]).await.unwrap();
            }
            "moduleActivate" | "activateModule" | "moduleActivated" | "activateShipModule" => {
                let mtid = pick_sde_type_matching(ctx, &conds, 439);
                let sid = sess.ship_id.unwrap();
                let mid = ctx.db.with_mut_snapshot(|s| { s.next_entity_id += 1; let id = s.next_entity_id; s.items.insert(id, InventoryItemRecord { item_id: id, type_id: mtid, owner_id: char_id, location_id: sid, flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() }); id });
                ctx.invalidate_ship_fitting(sid);
                ensure_dock_state(ctx, sess, false).await;
                rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(mid), PyRep::String("online".into())]).await.unwrap();
            }
            "lootItemCount" => {
                ensure_dock_state(ctx, sess, false).await;
                let sid = sess.ship_id.unwrap();
                let ltid = pick_sde_type_matching(ctx, &conds, 34);
                let (wreck_id, loot_id) = ctx.db.with_mut_snapshot(|s| {
                    s.next_entity_id += 1; let wid = s.next_entity_id;
                    s.items.insert(wid, InventoryItemRecord { item_id: wid, type_id: 26_557, owner_id: char_id, location_id: i64::from(JITA_SYS), flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
                    s.next_entity_id += 1; let lid = s.next_entity_id;
                    s.items.insert(lid, InventoryItemRecord { item_id: lid, type_id: ltid, owner_id: char_id, location_id: wid, flag: InventoryFlag::Cargo, quantity: target as i32, singleton: false, version: 1, ..Default::default() });
                    (wid, lid)
                });
                let sys = ctx.systems.get_or_spawn(JITA_SYS);
                let ship_pos = sys.get_ball_snapshot(sid).await.ok().flatten().map(|b| b.1.position).unwrap_or(DVec3::ZERO);
                let _ = sys.spawn_ball(eve_destiny::Ball { id: wreck_id, type_id: 26_557, position: ship_pos + DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 100.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }).await;
                rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
            }
            "somethingExplode" | "destroyHostileNPC" | "entityGroupDestroyed" => {
                ensure_dock_state(ctx, sess, false).await;
                let npc_tid = pick_sde_type_matching(ctx, &conds, 10_042);
                for k in 0..target {
                    let bid = spawn_specialized_npc_entity(ctx, JITA_SYS, npc_tid, "BeltRat", 0, DVec3::ZERO).await;
                    let _ = destroy_specialized_npc_and_queue_bounty(ctx, JITA_SYS, bid, char_id).await;
                    if k == 0 && target >= 2 && conds.is_empty() && ev == "destroyHostileNPC" {
                        let PyRep::Dict(p) = rpc(ctx, sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
                        assert!(p.iter().find(|(id, _)| id.as_i64() == Some(tid)).and_then(|(_, v)| v.as_i64()).unwrap_or(0) < 100);
                    }
                }
            }
            other => panic!("Unhandled SDE operation task event: {other}"),
        }
        ensure_dock_state(ctx, sess, saved_dock).await;
    }
}

async fn complete_tree_ops(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64) {
    let defs = get_operation_defs(&ctx.sde);
    let tree_ops = defs.cat_operation_trees.get(&cat_id).cloned().unwrap_or_default();
    let PyRep::List(done_list) = rpc(ctx, sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    let mut done: HashSet<i64> = done_list.iter().filter_map(|x| x.as_i64()).collect();
    for _ in 0..tree_ops.len() {
        for &oid in &tree_ops {
            if !done.contains(&oid) && defs.are_prerequisites_met(oid, &done) {
                complete_op_authoritatively(ctx, sess, char_id, cat_id, oid).await;
                done.insert(oid);
            }
        }
    }
}

fn is_supported_op(defs: &OperationDefinitions, oid: i64) -> bool {
    let ok_one = |id: i64| defs.op_tasks.get(&id).is_some_and(|ts| ts.iter().all(|t| defs.task_event_names.get(t).is_some_and(|ev| {
        ev.starts_with("client_") || matches!(ev.as_str(), "dockInStation" | "undockFromStation" | "shipBoarded" | "dungeonEntered"
            | "fitItemCount" | "loadChargesCount" | "skillTrainingStarted" | "timesWarped" | "warpToLocation" | "moduleActivate"
            | "activateModule" | "moduleActivated" | "activateShipModule" | "lootItemCount" | "somethingExplode" | "destroyHostileNPC" | "entityGroupDestroyed")
    })));
    ok_one(oid) && defs.op_prerequisites.get(&oid).into_iter().flatten().chain(defs.op_tree_parents.get(&oid).into_iter().flatten()).all(|&p| ok_one(p))
}

async fn unlock_prereqs_for_op(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, cat_id: i64, target_op: i64) {
    let defs = get_operation_defs(&ctx.sde);
    let PyRep::List(dl) = rpc(ctx, sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    let (mut done, mut candidates, mut stack) = (dl.iter().filter_map(|x| x.as_i64()).collect::<HashSet<i64>>(), Vec::new(), vec![target_op]);
    for _ in 0..=defs.op_prerequisites.len() + defs.op_tree_parents.len() {
        let Some(cur) = stack.pop() else { break; };
        for &p in defs.op_prerequisites.get(&cur).into_iter().flatten().chain(defs.op_tree_parents.get(&cur).into_iter().flatten()) {
            if !candidates.contains(&p) { candidates.push(p); stack.push(p); }
        }
    }
    for _ in 0..candidates.len().max(1) {
        if defs.are_prerequisites_met(target_op, &done) { return; }
        for &oid in &candidates {
            if !done.contains(&oid) && defs.are_prerequisites_met(oid, &done) { complete_op_authoritatively(ctx, sess, char_id, cat_id, oid).await; done.insert(oid); }
        }
    }
}

#[tokio::test]
// parity: feat:pve.operations-tutorial
async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
    eve_testkit::attest(&["feat:pve.operations-tutorial"]);
    let ctx = new_ctx();
    common::pin_clock(&ctx);
    let mut anon = SessionState::new(77);
    assert!(rpc(&ctx, &mut anon, "operationsManager", "get_character_progress", &[]).await.is_err());

    let mut sess = login_demo_pilot(&ctx, PILOT_1).await;
    let defs = get_operation_defs(&ctx.sde);
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(i64::from(AMARR_SYS)), PyRep::Integer(31)]).await.unwrap_err().code, "CustomInfo");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_main_tutorial_finished", &[PyRep::Integer(PILOT_2)]).await.unwrap_err().code, "UserError");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_2), PyRep::Integer(31)]).await.unwrap_err().code, "InsufficientRole"); // rpc-lint: allow error code assertion
    let child_op = defs.sde_operations.iter().copied().find(|&oid| !defs.are_prerequisites_met(oid, &HashSet::new())).expect("child op with prereq");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(child_op), PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap_err().code, "OperationPrerequisitesNotMet"); // rpc-lint: allow error code assertion

    // Intro tutorial Op 157 (Cat 31, SDE tasks [344=dockInStation, 345=shipBoarded, 346=client_agencyContentExpanded])
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(1));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(31)]).await.unwrap_err().code, "OperationCategoryIncomplete"); // rpc-lint: allow error code assertion

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(157), PyRep::Integer(i64::from(JITA_SYS)), PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationCategoryChanged" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157)]));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[]).await.unwrap(), PyRep::Integer(31));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(2));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(157), PyRep::Integer(1)]).await.unwrap_err().code, "CustomInfo");

    rpc(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("dockInStation".into())]).await.unwrap(), PyRep::Bool(false));
    rpc(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationTaskCompleted" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(344)]));
    let PyRep::Dict(prog_mid) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
    assert!((1..100).contains(&prog_mid.iter().find(|(k, _)| k.as_i64() == Some(157)).and_then(|(_, v)| v.as_i64()).unwrap_or(0)));

    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(345)]).await.unwrap_err().code, "TaskNotSkippable"); // rpc-lint: allow error code assertion
    let spoof_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("task_id".into()), PyRep::Integer(345))]))]);
    assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_progress".into())], &spoof_kw).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("operation_completed".into())]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_claim_rewards_btn_click".into())]).await.unwrap(), PyRep::Bool(false));
    let board_sid = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord { item_id: id, type_id: 587, owner_id: PILOT_1, location_id: JITA_STATION, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
        id
    });
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(board_sid)]).await.expect("ship.Board");
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationTaskCompleted" && n.args == vec![PyRep::Integer(31), PyRep::Integer(157), PyRep::Integer(345)]));

    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())]).await.unwrap(), PyRep::Bool(false));
    let bad_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("contentPieceType".into()), PyRep::Integer(99))]))]);
    assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())], &bad_kw).await.unwrap(), PyRep::Bool(false));
    let ok_kw = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String("contentPieceType".into()), PyRep::Integer(18))]))]);
    assert_eq!(rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("client_agencyContentExpanded".into())], &ok_kw).await.unwrap(), PyRep::Bool(true));
    let PyRep::List(done_157) = rpc(&ctx, &mut sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    assert!(done_157.contains(&PyRep::Integer(157)));

    complete_tree_ops(&ctx, &mut sess, PILOT_1, 31).await;
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnOperationCategoryCompleted" && n.args == vec![PyRep::Integer(31)]));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Integer(3));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(31)]).await.unwrap(), PyRep::Bool(true));

    let p1_race = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().race_id);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_2).unwrap().race_id = p1_race);
    let rw_item_op = defs.sde_operations.iter().copied().filter(|&oid| {
        defs.op_tasks.get(&oid).is_some_and(|ts| ts.iter().any(|t| defs.task_event_names.get(t).is_some_and(|e| !e.starts_with("client_"))))
            && (defs.op_completion_rewards.get(&oid).is_some_and(|rs| rs.iter().any(|r| r.race_id.is_none_or(|rc| rc == 0 || rc == p1_race)))
                || defs.op_skill_rewards.get(&oid).is_some_and(|r| !r.is_empty()))
    }).min_by_key(|&oid| defs.op_default_category.get(&oid).and_then(|c| defs.cat_operation_trees.get(c)).map_or(99, Vec::len)).expect("SDE item/skill-rewarding op");
    let rw_op_cat = defs.op_default_category.get(&rw_item_op).copied().unwrap_or(31);
    unlock_prereqs_for_op(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
    let items_before: usize = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count());
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    complete_op_authoritatively(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
    let items_after: usize = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count());
    assert!(items_after > items_before, "docked completion of {rw_item_op} must deliver SDE item/skillbook rewards");
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnItemChange"), "docked item delivery must emit OnItemChange");
    complete_op_authoritatively(&ctx, &mut sess, PILOT_1, rw_op_cat, rw_item_op).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).count()), items_after);

    let mut sess2 = login_demo_pilot(&ctx, PILOT_2).await;
    ensure_dock_state(&ctx, &mut sess2, false).await;
    unlock_prereqs_for_op(&ctx, &mut sess2, PILOT_2, rw_op_cat, rw_item_op).await;
    let uid2 = sess2.user_id.unwrap();
    let toks_before = ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid2).map_or(0, Vec::len));
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    complete_op_authoritatively(&ctx, &mut sess2, PILOT_2, rw_op_cat, rw_item_op).await;
    assert!(ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid2).map_or(0, Vec::len)) > toks_before);
    assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let PyRep::List(done_p2_list) = rpc(&ctx, &mut sess2, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    let done_p2: HashSet<i64> = done_p2_list.iter().filter_map(|x| x.as_i64()).collect();
    if let Some((&rw_isk_op, &op_isk)) = defs.op_isk_rewards.iter().filter(|(&oid, &isk)| {
        isk > 0 && !done_p2.contains(&oid) && is_supported_op(defs, oid)
            && defs.op_tasks.get(&oid).is_some_and(|ts| ts.iter().any(|t| defs.task_event_names.get(t).is_some_and(|e| !e.starts_with("client_"))))
    }).min_by_key(|(oid, _)| *oid) {
        let isk_cat = defs.op_default_category.get(&rw_isk_op).copied().unwrap_or(31);
        unlock_prereqs_for_op(&ctx, &mut sess2, PILOT_2, isk_cat, rw_isk_op).await;
        let bal_pre = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.0);
        let _ = ctx.drain_notifications_for_char(PILOT_2);
        complete_op_authoritatively(&ctx, &mut sess2, PILOT_2, isk_cat, rw_isk_op).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.0) - bal_pre, (op_isk as i64) * 100);
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnAccountChange"));
    }
    let rw_cat_id = defs.cat_isk_rewards.iter().filter(|(cid, isk)| **isk > 0 && defs.cat_operation_trees.get(cid).is_some_and(|t| !t.is_empty() && t.iter().all(|&o| is_supported_op(defs, o)))).map(|(c, _)| *c).min()
        .or_else(|| defs.cat_operation_trees.iter().filter(|(&cid, t)| cid != 31 && t.len() >= 2 && t.iter().all(|&o| is_supported_op(defs, o))).min_by_key(|(_, t)| t.len()).map(|(&c, _)| c)).expect("multi-op SDE category");
    let cat_isk = defs.cat_isk_rewards.get(&rw_cat_id).copied().unwrap_or(0);
    let bal_pre_cat = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap_err().code, "OperationCategoryIncomplete"); // rpc-lint: allow error code assertion
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0), bal_pre_cat);
    complete_tree_ops(&ctx, &mut sess, PILOT_1, rw_cat_id).await;
    let bal_before_claim = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0) - bal_before_claim, (cat_isk as i64) * 100);
    if cat_isk > 0 { assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnAccountChange")); }
    rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(PILOT_1), PyRep::Integer(rw_cat_id)]).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0) - bal_before_claim, (cat_isk as i64) * 100);
}

#[tokio::test]
// parity: feat:pve.seasonal-login
async fn test_seasonal_login_campaign_active_claim_and_history() {
    eve_testkit::attest(&["feat:pve.seasonal-login"]);
    let ctx = new_ctx();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_700_044_800)));

    let mut anon = SessionState::new(88);
    assert!(rpc(&ctx, &mut anon, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.is_err());

    let mut sess = login_demo_pilot(&ctx, PILOT_1).await;
    let uid = sess.user_id.expect("authenticated user_id");
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
    assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("no seasonal campaign"));

    assert_eq!(rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.unwrap_err().code, "InsufficientRole");
    let mut gm_sess = login_demo_pilot(&ctx, PILOT_2).await;
    gm_sess.role = service_roles::ROLE_GML | service_roles::ROLE_ADMIN;
    assert_eq!(rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 99999999 30 11:00:00".into())]).await.unwrap_err().code, "CampaignNotFound");

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.expect("GM schedule 2323");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&PILOT_1).unwrap().clone_grade = 1);

    let PyRep::Tuple(active_fields) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
    assert_eq!(active_fields.len(), 4);
    assert_eq!(active_fields[0], PyRep::Integer(2323));
    assert_eq!(active_fields[2], PyRep::String("11:00:00".into()));
    let initial_rem = active_fields[3].as_i64().expect("num_remaining_rewards");
    assert!(initial_rem > 0);

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnItemChange"), "docked seasonal claim must emit OnItemChange");
    assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 87_900)));
    assert!(!ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.type_id == 49_809)));
    assert!(!ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let PyRep::Dict(hist_day0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(hist_day0.len(), 1);
    let ord0 = hist_day0[0].0.as_i64().unwrap();
    assert_eq!(active_fields[1], PyRep::Integer(ord0 + 30), "end_date_ordinal before 11:00:00 UTC turnover must honor turnover_secs");
    assert_eq!(hist_day0[0].1, PyRep::Integer(0), "Alpha claim state must be CLONE_STATE_ALPHA (0)");
    let PyRep::Tuple(after_d0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
    assert_eq!(after_d0[3], PyRep::Integer(initial_rem - 1));

    ctx.advance_secs(600);
    assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("already claimed"));

    ctx.advance_secs(1_200);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { let ch = snap.characters.get_mut(&PILOT_1).unwrap(); ch.clone_grade = 2; ch.station_id = None; });
    let tokens_before = ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid).map_or(0, Vec::len));
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| {
        let toks = account_services::load_store(s).redeem_tokens.get(&uid).cloned().unwrap_or_default();
        toks.len() > tokens_before && toks.iter().any(|t| t.type_id == 49_809)
    }));
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let PyRep::Dict(hist_day1) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(hist_day1.len(), 2);
    assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0)).and_then(|(_, v)| v.as_i64()), Some(1));
    assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0 + 1)).and_then(|(_, v)| v.as_i64()), Some(1));

    ctx.advance_secs(31 * 86_400);
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
    assert!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err().message.to_ascii_lowercase().contains("no seasonal campaign"));
}

#[tokio::test]
async fn test_structure_profiles_acl_mapping_and_delayed_apply() {
    let ctx = new_ctx();
    common::pin_clock(&ctx);
    const CORP_OWNER: i64 = 98_600_001;
    const CORP_GUEST: i64 = 98_600_002;
    const DIR_PILOT: i64 = 95_600_001;
    const GRUNT_PILOT: i64 = 95_600_002;
    const VIP_PILOT: i64 = 95_600_003;
    const PUB_PILOT: i64 = 95_600_004;
    const BLOCKED_CORP_PILOT: i64 = 95_600_005;
    const OUTSIDER_PILOT: i64 = 95_600_006;
    const ST_ID: i64 = 1_045_000_000_001;

    seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_PILOT, &[(DIR_PILOT, corp_roles::ROLE_DIRECTOR, "Owner Dir"), (GRUNT_PILOT, 0, "Owner Grunt")]);
    seed_corp_and_pilots(&ctx, CORP_GUEST, VIP_PILOT, &[(VIP_PILOT, corp_roles::ROLE_DIRECTOR, "VIP"), (PUB_PILOT, 0, "Pub"), (BLOCKED_CORP_PILOT, 0, "Blocked")]);
    seed_corp_and_pilots(&ctx, 98_600_003, OUTSIDER_PILOT, &[(OUTSIDER_PILOT, corp_roles::ROLE_DIRECTOR, "Outsider")]);
    seed_structure_with_services(&ctx, ST_ID, CORP_OWNER, "Profile Citadel", "ShieldVulnerable", &[
        (880_001, 35_892, InventoryFlag::ServiceSlot0, true), // Market (svc 1)
        (880_002, 35_894, InventoryFlag::ServiceSlot1, true), // Clone Bay (svc 2)
    ]).await;

    let mut dir_sess = make_space_session(&ctx, DIR_PILOT, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
    let mut grunt_sess = make_space_session(&ctx, GRUNT_PILOT, CORP_OWNER, 0).await;
    let mut vip_sess = make_space_session(&ctx, VIP_PILOT, CORP_GUEST, corp_roles::ROLE_DIRECTOR).await;
    let mut pub_sess = make_space_session(&ctx, PUB_PILOT, CORP_GUEST, 0).await;
    let mut blk_sess = make_space_session(&ctx, BLOCKED_CORP_PILOT, CORP_GUEST, 0).await;
    let mut out_sess = make_space_session(&ctx, OUTSIDER_PILOT, 98_600_003, corp_roles::ROLE_DIRECTOR).await;
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Unauthorized".into()), PyRep::String("".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

    let vip_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let pub_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Public ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let def_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Gunners ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(vip_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(CORP_GUEST)]), PyRep::Bool(false)]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(BLOCKED_CORP_PILOT)]), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();

    let prof_1 = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Tiered Trade".into()), PyRep::String("ACL tiered".into())]).await.unwrap().as_i64().unwrap();
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), PyRep::Dict(vec![])]).await.unwrap_err().code, "StructureOwnershipViolation");
    let bad_tax = PyRep::Dict(vec![(PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(1.5))])]))]);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), bad_tax]).await.unwrap_err().code, "InvalidStructureTaxRate");
    let bad_flat_fee = PyRep::Dict(vec![(PyRep::Integer(23), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(-10.0))])]))]);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), bad_flat_fee]).await.unwrap_err().code, "InvalidStructureTaxRate");

    let v24_settings = PyRep::Dict(vec![
        (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Integer(1))])])),
        (PyRep::Integer(4), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Real(0.05))])])),
        (PyRep::Integer(23), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(500_000.0))])])),
        (PyRep::Integer(20), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(1_000_000.0))])])),
        (PyRep::Integer(17), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(def_acl)), ("value", PyRep::Integer(1))])])),
    ]);
    let _ = ctx.drain_notifications_for_char(DIR_PILOT);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), v24_settings]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));

    let dup_id = rpc(&ctx, &mut dir_sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_1)]).await.unwrap().as_i64().unwrap();
    let PyRep::List(profs) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfiles", &[]).await.unwrap() else { panic!("List") };
    assert_eq!(profs.len(), 2);
    let PyRep::Dict(saved_cfg) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_1)]).await.unwrap() else { panic!("Dict") };
    assert_eq!(saved_cfg.len(), 5);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_id)]).await.unwrap(), PyRep::Bool(true));

    let _ = ctx.drain_notifications_for_char(DIR_PILOT);
    let _ = ctx.drain_notifications_for_char(VIP_PILOT);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_ID)]), PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(DIR_PILOT).iter().any(|n| n.event == "OnProfileSettingsChange" && n.args == vec![PyRep::List(vec![PyRep::Integer(ST_ID)])]));
    assert!(ctx.drain_notifications_for_char(VIP_PILOT).iter().any(|n| n.event == "OnDockingAccessChangedForCurrentSolarSystem"));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_1)]).await.unwrap_err().code, "ProfileInUse");

    assert_eq!(rpc(&ctx, &mut blk_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
    if let Some(&undock_op) = get_operation_defs(&ctx.sde).op_tasks.iter().find_map(|(o, ts)| ts.first().filter(|t| get_operation_defs(&ctx.sde).task_event_names.get(t).map(|s| s.as_str()) == Some("undockFromStation")).map(|_| o)) {
        let mut gm_s = vip_sess.clone(); gm_s.role |= eve_machonet::service_roles::ROLE_GMH;
        let _ = rpc(&ctx, &mut gm_s, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(undock_op)]).await;
    }
    let _ = ctx.drain_notifications_for_char(VIP_PILOT);
    rpc(&ctx, &mut vip_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("VIP docks");
    assert!(ctx.drain_notifications_for_char(VIP_PILOT).iter().all(|n| n.event != "OnOperationTaskCompleted"));
    rpc(&ctx, &mut pub_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("Public corp member docks");

    assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(5)]).await.unwrap().as_f64().unwrap() - 0.01).abs() < 1e-6);
    assert!((rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(5)]).await.unwrap().as_f64().unwrap() - 0.05).abs() < 1e-6);
    assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap().as_f64().unwrap() - 500_000.0).abs() < 1e-3);
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(ST_ID), PyRep::Integer(6)]).await.unwrap_err().code, "StructureServiceAccessDenied");

    let owner_wal_0 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
    rpc(&ctx, &mut vip_sess, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(ST_ID), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(100), PyRep::Integer(90)]).await.expect("VIP PlaceBuyOrder");
    let owner_wal_1 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
    assert_eq!(owner_wal_1 - owner_wal_0, 100_00);
    rpc(&ctx, &mut pub_sess, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(ST_ID), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(100), PyRep::Integer(90)]).await.expect("Pub PlaceBuyOrder");
    let owner_wal_2 = ctx.db.with_snapshot(|s| s.corporations.get(&CORP_OWNER).unwrap().master_wallet.0);
    assert_eq!(owner_wal_2 - owner_wal_1, 500_00);
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "DefenseAccessDenied");
    rpc(&ctx, &mut vip_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP gunner TakeControl");
    rpc(&ctx, &mut vip_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP ReleaseControl");
    let _ = ctx.drain_notifications_for_char(PUB_PILOT);
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(CORP_GUEST)])]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_ID)])]).await.unwrap(), PyRep::List(vec![]));
}

#[tokio::test]
async fn test_structure_directory_accessible_structures_and_services() {
    let ctx = new_ctx();
    common::pin_clock(&ctx);
    const CORP_OWNER: i64 = 98_700_001; const CORP_ALLY: i64 = 98_700_002;
    const DIR_ID: i64 = 95_700_001; const GRUNT_ID: i64 = 95_700_002; const VIP_ID: i64 = 95_700_003; const MKT_ONLY_ID: i64 = 95_700_004; const OUT_ID: i64 = 95_700_005;
    const ST_HUB: i64 = 1_046_000_000_001; const ST_ANCHORING: i64 = 1_046_000_000_002; const ST_PRIVATE: i64 = 1_046_000_000_003;
    seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_ID, &[(DIR_ID, corp_roles::ROLE_DIRECTOR, "Dir"), (GRUNT_ID, 0, "Grunt")]);
    seed_corp_and_pilots(&ctx, CORP_ALLY, VIP_ID, &[(VIP_ID, corp_roles::ROLE_DIRECTOR, "VIP"), (MKT_ONLY_ID, 0, "MarketOnly")]);
    seed_corp_and_pilots(&ctx, 98_700_003, OUT_ID, &[(OUT_ID, corp_roles::ROLE_DIRECTOR, "Outsider")]);
    seed_structure_with_services(&ctx, ST_HUB, CORP_OWNER, "Public Trade Hub", "ShieldVulnerable", &[
        (890_001, 35_892, InventoryFlag::ServiceSlot0, true),  // Market (1) online
        (890_002, 35_899, InventoryFlag::ServiceSlot1, true),  // Reprocessing (3) online
        (890_003, 35_894, InventoryFlag::ServiceSlot2, false), // Clone Bay (2) OFFLINE
    ]).await;
    seed_structure_with_services(&ctx, ST_ANCHORING, CORP_OWNER, "Anchoring Raitaru", "Anchoring", &[]).await;
    seed_structure_with_services(&ctx, ST_PRIVATE, CORP_OWNER, "Private Sotiyo", "ShieldVulnerable", &[(890_004, 35_878, InventoryFlag::ServiceSlot0, true)]).await;
    let mut dir_sess = make_space_session(&ctx, DIR_ID, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
    let mut grunt_sess = make_space_session(&ctx, GRUNT_ID, CORP_OWNER, 0).await;
    let mut vip_sess = make_space_session(&ctx, VIP_ID, CORP_ALLY, corp_roles::ROLE_DIRECTOR).await;
    let mut mkt_sess = make_space_session(&ctx, MKT_ONLY_ID, CORP_ALLY, 0).await;
    let mut out_sess = make_space_session(&ctx, OUT_ID, 98_700_003, corp_roles::ROLE_DIRECTOR).await;
    let mut anon = SessionState::new(99);
    assert!(rpc(&ctx, &mut anon, "structureDirectory", "GetMyCharacterStructures", &[]).await.is_err());

    let g_vip = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let g_mkt = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("MktOnly".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_vip), PyRep::List(vec![PyRep::Integer(VIP_ID)])]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_mkt), PyRep::List(vec![PyRep::Integer(MKT_ONLY_ID)])]).await.unwrap();

    let p_hub = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Hub ACL".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    let hub_cfg = PyRep::Dict(vec![
        (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Integer(1))])])),
        (PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Real(0.03))])])),
        (PyRep::Integer(3), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.02))])])),
    ]);
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_hub), hub_cfg]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_HUB)]), PyRep::Integer(p_hub)]).await.unwrap();
    out_sess.solar_system_id = Some(AMARR_SYS);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&OUT_ID).unwrap().solar_system_id = AMARR_SYS);
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_PRIVATE)]).await.unwrap_err().code, "StructureAccessDenied");
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(999_999_999)]).await.unwrap_err().code, "StructureNotFound");
    let PyRep::Dict(corp_map) = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(corp_map.len(), 3);
    let hub_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_HUB)).unwrap().1;
    assert_eq!(hub_dir.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
    assert_eq!(hub_dir.dict_get("fuelBlocks"), Some(&PyRep::Integer(2_400)));
    assert_eq!(hub_dir.dict_get("profileID"), Some(&PyRep::Integer(p_hub)));
    let anch_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_ANCHORING)).unwrap().1;
    assert_eq!(anch_dir.dict_get("services"), Some(&PyRep::Dict(vec![])));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)]));
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_HUB)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));
    let PyRep::Dict(vip_map) = rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(vip_map.len(), 1);
    assert_eq!(vip_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
    assert_eq!(vip_map[0].1.dict_get("profileID"), Some(&PyRep::None));
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));
    let PyRep::Dict(mkt_map) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(mkt_map.len(), 1);
    assert_eq!(mkt_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(1)), (PyRep::Integer(4), PyRep::Integer(1))])));
    let PyRep::List(map_rows) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap() else { panic!("List") };
    assert_eq!(map_rows.len(), 3);
    let hub_map_row = map_rows.iter().find(|r| format!("{r:?}").contains(&ST_HUB.to_string())).unwrap();
    assert!(format!("{hub_map_row:?}").contains("5") && format!("{hub_map_row:?}").contains("4"));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)])]).await.unwrap(), PyRep::List(vec![]));
}
