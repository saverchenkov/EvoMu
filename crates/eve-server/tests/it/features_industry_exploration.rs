//! Integration tests for open `feat:` rows (Batch 6).

#![forbid(unsafe_code)]

// Submodule: deployables_and_air_opportunities
#[allow(unused_imports, dead_code)]
mod deployables_and_air_opportunities {
    use super::*;

// Integration tests for open `feat:` rows (Batch 6).
//
// Covers FW militia LP store standing/enlistment gates, Mobile Scan Inhibitor
// d-scan/probe suppression + internal sensor inoperability + 1h expiry, Mobile
// Siphon Unit POS-only hourly material theft, AIR operations tutorial gates &
// server-observed progression, and Opportunities server-observed completion.


use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::quasar::QuasarMessage;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{ActiveScannerProbe, ServerContext};
use eve_server::services::operations_manager::get_operation_defs;
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;
const SYS_ID: u32 = 30_000_142;
const LOWSEC_SYS_ID: u32 = 30_000_015; // Sendaya (security ~0.29 < 0.4)

static NEXT_TMP_DB: AtomicU64 = AtomicU64::new(1);

struct TempDbGuard(PathBuf);
impl Drop for TempDbGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-shm"));
    }
}

fn new_persistent_ctx() -> (TempDbGuard, PathBuf, ServerContext) {
    let seq = NEXT_TMP_DB.fetch_add(1, Ordering::Relaxed);
    let db_path = std::env::temp_dir().join(format!("evomu_b6_{}_{}.sqlite", std::process::id(), seq));
    let _ = std::fs::remove_file(&db_path);
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"));
    let demo_snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    db.with_mut_snapshot(|s| *s = demo_snap);
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    eve_server::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
    crate::common::pin_clock(&ctx);
    (TempDbGuard(db_path.clone()), db_path, ctx)
}

fn reload_ctx(db_path: &PathBuf, sde: Arc<eve_sde::SdeRegistry>) -> ServerContext {
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(db_path).expect("reopen WorldRepository"));
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    eve_server::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
    crate::common::pin_clock(&ctx);
    ctx
}

fn make_ball(id: i64, type_id: u32, position: DVec3, radius: f64) -> Ball {
    Ball {
        id, type_id, position, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius, speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::GotoDirection { direction: DVec3::Z },
    }
}

fn char_session(ctx: &ServerContext, char_id: i64, station_id: Option<i64>, solar_system_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(char_id as u64));
    SessionState {
        user_id: Some(uid), character_id: Some(char_id), corporation_id: Some(1_000_044),
        station_id, solar_system_id, ship_id, ..SessionState::default()
    }
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn grant_anchoring_skill(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11_584,
            eve_db::TrainedSkillRecord { type_id: 11_584, level: 5, skill_points: 256_000.0 },
        );
    });
}

fn item_rec(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: eve_db::InventoryFlag, quantity: i32, singleton: bool) -> eve_db::InventoryItemRecord {
    eve_db::InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
}

fn dscan_ids(rep: PyRep) -> Vec<i64> {
    let PyRep::List(rows) = rep else { panic!("expected dscan list") };
    rows.iter().filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64())).collect()
}

#[tokio::test]
async fn test_lp_store_militia_enlistment_gate_and_clean_catalog() {
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let mut chosen_offer: Option<(i64, u32)> = None;
    for corp_id in 1_000_179_i64..=1_000_182 {
        let rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(corp_id)])
            .await
            .expect("GetAvailableOffersFromCorp");
        let PyRep::List(offers) = rep else { panic!("expected list of LP offers") };
        assert!(!offers.is_empty(), "Militia corp {corp_id} must serve TQ LP offers");
        for o in &offers {
            let oid = o.dict_get("offerID").and_then(|v| v.as_i64()).unwrap_or(0);
            let tid = o.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0);
            assert!(!(910_001..=910_048).contains(&oid), "Synthetic FW booster offer {oid} must not appear");
            assert!(!(91_916..=91_977).contains(&tid), "Unverified FW booster type {tid} must not appear");
            if corp_id == 1_000_179 && chosen_offer.is_none() {
                let no_req = matches!(o.dict_get("reqItems"), Some(PyRep::List(items)) if items.is_empty());
                let ak = o.dict_get("akCost").and_then(|v| v.as_i64()).unwrap_or(0);
                if no_req && ak == 0 && oid > 0 && tid > 0 {
                    chosen_offer = Some((oid, tid as u32));
                }
            }
        }
    }
    let (offer_id, reward_tid) = chosen_offer.expect("24th Imperial Crusade offer without trade-ins");

    let (fw_station_id, fw_sys_id) = (60_015_054_i64, 30_002_978_u32);
    sess.station_id = Some(fw_station_id);
    sess.solar_system_id = Some(fw_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).unwrap();
        ch.loyalty_points.insert(1_000_179, 500_000);
        ch.balance = eve_db::IskCents(500_000_000_00);
        ch.standings.insert(1_000_179, -1.0);
        ch.war_faction_id = None;
        ch.station_id = Some(fw_station_id);
        ch.solar_system_id = fw_sys_id;
    });

    // Negative 1: Unenlisted pilot with negative corp standing is rejected
    let args = [PyRep::Integer(1_000_179), PyRep::Integer(offer_id), PyRep::Integer(1)];
    let err_unenlisted = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.unwrap_err();
    assert_eq!(err_unenlisted.code, "InsufficientStanding");

    // Negative 2: Pilot enlisted in opposing militia (Minmatar 500_002) is rejected
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().war_faction_id = Some(500_002));
    let err_wrong_fac = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.unwrap_err();
    assert_eq!(err_wrong_fac.code, "InsufficientStanding");

    // Positive: Enlisted in matching Amarr militia (500_003) with 0.0 standing succeeds and persists
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_1).unwrap();
        ch.war_faction_id = Some(500_003);
        ch.standings.insert(1_000_179, 0.0);
    });
    let take_ok = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.expect("redeem offer");
    assert_eq!(take_ok.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(i64::from(reward_tid)));

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    ctx_reloaded.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&PILOT_1).expect("character persisted");
        assert!(ch.loyalty_points.get(&1_000_179).copied().unwrap_or(500_000) < 500_000);
        assert!(snap.items.values().any(|i| i.owner_id == PILOT_1 && i.location_id == fw_station_id && i.type_id == reward_tid));
    });
}

#[tokio::test]
async fn test_mobile_scan_inhibitor_hides_ships_from_dscan_and_probes() {
    // parity: feat:structures.mobile-scan-inhibitor
    let (_dir, _db_path, ctx) = new_persistent_ctx();
    grant_anchoring_skill(&ctx, PILOT_1);
    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS_ID);

    let base = DVec3::splat(2.0e11);
    let (scanner_ship_id, hidden_ship_id, visible_ship_id, deployer_ship_id, inhibitor_id) =
        (700_001, 700_002, 700_003, 700_004, 700_005);
    for (bid, pos) in [
        (scanner_ship_id, base),
        (hidden_ship_id, base + DVec3::new(100_000.0, 0.0, 0.0)),
        (visible_ship_id, base + DVec3::new(200_000.0, 0.0, 0.0)),
        (deployer_ship_id, base + DVec3::new(110_000.0, 0.0, 0.0)),
    ] {
        sys_handle.spawn_ball(make_ball(bid, 587, pos, 40.0)).await.unwrap();
    }

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).unwrap();
        ch.station_id = None;
        ch.solar_system_id = LOWSEC_SYS_ID;
        ch.active_ship_id = deployer_ship_id;
        snap.items.insert(deployer_ship_id, item_rec(deployer_ship_id, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true));
        snap.items.insert(inhibitor_id, item_rec(inhibitor_id, 33_589, PILOT_1, deployer_ship_id, eve_db::InventoryFlag::Cargo, 1, false));
    });

    // Live deployment via posMgr.AnchorStructure (60s activation delay + 3600s active lifetime)
    let mut deploy_sess = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_ship_id));
    let anchor_rep = rpc(&ctx, &mut deploy_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhibitor_id)])
        .await
        .expect("posMgr.AnchorStructure Mobile Scan Inhibitor");
    assert_eq!(anchor_rep.dict_get("anchoringDelaySeconds").and_then(|v| v.as_f64()), Some(60.0));

    // Negative 1: Mobile Scan Inhibitor (group 1275) cannot be unanchored / scooped once deployed
    let scoop_err = rpc(&ctx, &mut deploy_sess, "entity", "CmdUnanchor", &[PyRep::Integer(inhibitor_id)]).await.unwrap_err();
    assert_eq!(scoop_err.code, "CannotUnanchor");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().active_ship_id = scanner_ship_id;
        let c2 = s.characters.get_mut(&90_000_002).unwrap();
        c2.station_id = None;
        c2.solar_system_id = LOWSEC_SYS_ID;
        c2.active_ship_id = hidden_ship_id;
    });

    let inhib_pos = [base.x + 110_250.0, base.y, base.z];
    let vis_pos = [base.x + 200_000.0, base.y, base.z];
    if let Ok(mut probes) = ctx.active_scanner_probes.write() {
        for (pid, cid, sid, pos) in [(800_001, PILOT_1, scanner_ship_id, inhib_pos), (800_002, 90_000_002, hidden_ship_id, vis_pos)] {
            probes.insert(pid, ActiveScannerProbe {
                probe_id: pid, owner_char_id: cid, ship_id: sid, solar_system_id: LOWSEC_SYS_ID,
                probe_type_id: 30_486, position: pos, destination: pos, range_step: 1,
                scan_range_m: 1_000_000.0, base_sensor_strength: 44.0, launcher_module_id: 0,
                max_flight_time_s: 7_200.0, launched_at_unix: ctx.now_unix(), state: 1, warp_end_ft: None,
            });
        }
    }

    // Negative 2: At t + 30s (during 60s warmup), inhibitor does NOT mask ships
    ctx.advance_secs(30);
    let mut ext_sess = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(scanner_ship_id));
    let mut inside_sess = char_session(&ctx, 90_000_002, None, Some(LOWSEC_SYS_ID), Some(hidden_ship_id));
    let dscan_args = [PyRep::Real(360.0), PyRep::Real(1_000_000.0)];
    let warmup_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
    assert!(warmup_ids.contains(&hidden_ship_id), "Ship must remain visible before 60s inhibitor activation completes");

    // Positive: Advance past 60s activation delay (t + 65s) -> active suppression & internal sensor lockout
    ctx.advance_secs(35);
    let active_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
    assert!(!active_ids.contains(&hidden_ship_id), "Ship inside active 30km sphere must be hidden from d-scan");
    assert!(active_ids.contains(&visible_ship_id) && active_ids.contains(&inhibitor_id));

    let PyRep::List(probe_rows) = rpc(&ctx, &mut ext_sess, "scanMgr", "RequestScans", &[PyRep::List(Vec::new())]).await.unwrap() else {
        panic!("expected probe scan list");
    };
    let probe_ids: Vec<i64> = probe_rows.iter().filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64())).collect();
    assert!(!probe_ids.contains(&hidden_ship_id), "Ship inside active 30km sphere must be hidden from combat probes");
    assert!(probe_ids.contains(&visible_ship_id) && probe_ids.contains(&inhibitor_id));

    // Ships INSIDE the active 30km sphere have inoperable d-scan & probe scanning sensors
    assert_eq!(rpc(&ctx, &mut inside_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap(), PyRep::List(Vec::new()));
    assert_eq!(rpc(&ctx, &mut inside_sess, "scanMgr", "RequestScans", &[PyRep::List(Vec::new())]).await.unwrap(), PyRep::List(Vec::new()));

    // Expiry after 1 hour active lifetime -> actor tick despawns inhibitor and restores scanning
    ctx.advance_secs(3_600);
    sys_handle.step_manual(1.0).await.unwrap();
    assert!(sys_handle.get_ball_snapshot(inhibitor_id).await.unwrap().is_none(), "Expired inhibitor must despawn");
    let post_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
    assert!(post_ids.contains(&hidden_ship_id));
}

#[tokio::test]
async fn test_mobile_siphon_steals_material_from_pos_within_range() {
    // parity: feat:structures.mobile-siphon
    let (_dir, db_path, ctx) = new_persistent_ctx();
    grant_anchoring_skill(&ctx, PILOT_1);
    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS_ID);

    let base = DVec3::splat(2.0e11);
    let (pos_tower_id, pos_silo_id, athanor_id) = (710_001, 710_002, 710_003);
    let (deployer_near_ship, deployer_far_ship) = (710_004, 710_005);
    let (siphon_near_id, siphon_far_id, unanchored_siphon_id) = (710_006, 710_007, 710_008);
    let (pos_silo_stack_id, pos_hangar_stack_id, athanor_stack_id) = (710_010, 710_011, 710_012);

    for (bid, tid, dx, rad) in [
        (pos_tower_id, 16_213, 0.0, 2_000.0),
        (pos_silo_id, 14_343, 2_000.0, 500.0),
        (athanor_id, 35_835, 100_000.0, 5_000.0),
        (deployer_near_ship, 587, 40_000.0, 40.0),
        (deployer_far_ship, 587, 250_000.0, 40.0),
        (unanchored_siphon_id, 33_477, 25_000.0, 100.0),
    ] {
        let p = base + DVec3::new(dx, 0.0, 0.0);
        sys_handle.spawn_ball(make_ball(bid, tid, p, rad)).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions.insert(bid, [p.x, p.y, p.z]);
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).unwrap();
        ch.station_id = None;
        ch.solar_system_id = LOWSEC_SYS_ID;
        ch.active_ship_id = deployer_near_ship;
        for (iid, tid, owner, loc, flag, qty, sing) in [
            (pos_tower_id, 16_213, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (pos_silo_id, 14_343, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (athanor_id, 35_835, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (deployer_near_ship, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (deployer_far_ship, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (siphon_near_id, 33_477, PILOT_1, deployer_near_ship, eve_db::InventoryFlag::Cargo, 1, false),
            (siphon_far_id, 33_477, PILOT_1, deployer_far_ship, eve_db::InventoryFlag::Cargo, 1, false),
            (unanchored_siphon_id, 33_477, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
            (pos_silo_stack_id, 16_634, 1_000_044, pos_silo_id, eve_db::InventoryFlag::Cargo, 200, false),
            (pos_hangar_stack_id, 16_634, 90_000_002, pos_silo_id, eve_db::InventoryFlag::Hangar, 200, false),
            (athanor_stack_id, 16_634, 1_000_044, athanor_id, eve_db::InventoryFlag::Cargo, 200, false),
        ] {
            snap.items.insert(iid, item_rec(iid, tid, owner, loc, flag, qty, sing));
        }
    });

    let mut sess_near = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_near_ship));
    rpc(&ctx, &mut sess_near, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_near_id)]).await.expect("Anchor near siphon");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().active_ship_id = deployer_far_ship);
    let mut sess_far = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_far_ship));
    let err_far = rpc(&ctx, &mut sess_far, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_far_id)]).await.unwrap_err();
    assert_eq!(err_far.code, "NoSiphonTargetInRange");

    // Negative 1: Zero theft during anchoring and immediately upon anchor completion (t = 60s)
    ctx.advance_secs(60);
    sys_handle.step_manual(1.0).await.unwrap();
    let cap_t0 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await.unwrap().as_f64().unwrap_or(1.0);
    assert_eq!(cap_t0, 0.0, "Zero theft must occur before a full hourly POS cycle elapses");

    // Positive: System actor tick runs cycle 1 -> 60 stolen + 6 waste = 66 drained from POS Silo (134 left)
    ctx.advance_secs(3_600);
    sys_handle.step_manual(1.0).await.unwrap();
    let cap1 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await.unwrap().as_f64().unwrap_or(0.0);
    assert!(cap1 > 0.0, "Anchored siphon within 100km of POS Silo must steal raw material on hourly tick");

    // Second hourly cycle -> stacks to 120 in near siphon; Athanor, private Hangar, unanchored & far siphons untouched
    ctx.advance_secs(3_600);
    sys_handle.step_manual(1.0).await.unwrap();
    let cap2 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await.unwrap().as_f64().unwrap_or(0.0);
    assert!(cap2 > cap1);
    assert_eq!(rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_far_id)]).await.unwrap().as_f64(), Some(0.0));
    assert_eq!(rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(unanchored_siphon_id)]).await.unwrap().as_f64(), Some(0.0));

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    ctx_reloaded.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&pos_silo_stack_id].quantity, 68, "POS Silo stack must lose 2x(60+6) units");
        assert_eq!(snap.items[&pos_hangar_stack_id].quantity, 200, "Private player Hangar stack must never be siphoned");
        assert_eq!(snap.items[&athanor_stack_id].quantity, 200, "Upwell Refinery cargo must never be siphoned");
        let stolen: Vec<_> = snap.items.values().filter(|i| i.location_id == siphon_near_id && i.type_id == 16_634).collect();
        assert_eq!(stolen.len(), 1);
        assert_eq!(stolen[0].quantity, 120);
    });
}

#[tokio::test]
async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
    // parity: feat:pve.operations-tutorial
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().home_station_id = 60_015_054);
    let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), Some(ship_id));
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await.unwrap();
    sess.station_id = Some(STATION_ID);
    sess.solar_system_id = Some(SYS_ID);
    sess.ship_id = Some(ship_id);

    let op_defs = get_operation_defs(&ctx.sde);
    let (&cat_id, cat_ops) = op_defs
        .category_operations
        .iter()
        .filter(|(&cid, ops)| (1..=1_000).contains(&cid) && !ops.is_empty())
        .min_by_key(|(cid, ops)| (!op_defs.category_isk_rewards.contains_key(cid), ops.len()))
        .expect("SDE must define at least one operation category with constituent operations");
    let cat_ops = cat_ops.clone();
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);

    // Negative 1: Non-GM cannot complete category before completing its SDE operations
    let cat_err = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap_err();
    assert!(cat_err.code == "UserError" || cat_err.code.starts_with("OperationCategory"));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_before);

    // Negative 2: Non-SDE operation ID is rejected by activate_operation_in_solar_system
    let bad_op = rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(99_999), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap_err();
    assert_eq!(bad_op.code, "CustomInfo");

    // Negative 3: process_client_event with no active operation cannot complete tasks
    let any_tid = *op_defs.task_to_op.keys().next().expect("SDE task");
    let no_active = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(any_tid)]).await.unwrap();
    assert_eq!(no_active, PyRep::Bool(false));

    rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("start_tutorial".into()), PyRep::Integer(1)]).await.unwrap();
    let (&undock_tid, _) = op_defs
        .task_event_names
        .iter()
        .find(|(tid, ev)| {
            ev.as_str() == "undockFromStation"
                && !op_defs.client_completable_tasks.contains(tid)
                && !op_defs.task_is_client_skippable.contains(tid)
                && !op_defs.task_is_optional.contains(tid)
                && op_defs.task_to_op.contains_key(tid)
        })
        .expect("SDE server-observed undockFromStation operation task");
    let undock_op_id = op_defs.task_to_op[&undock_tid];
    sess.role |= eve_machonet::service_roles::ROLE_GMH;
    rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(undock_op_id), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap();
    for &t in op_defs.op_tasks.get(&undock_op_id).into_iter().flatten() {
        if t == undock_tid { break; }
        let _ = rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(cat_id), PyRep::Integer(undock_op_id), PyRep::Integer(t)]).await;
    }
    sess.role &= !eve_machonet::service_roles::ROLE_GMH;

    // Server-observed task rejects non-GM skip_task and pays no op/category ISK
    let skip_err = rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(undock_tid)])
        .await
        .expect_err("non-GM skip_task on server-observed task must fail");
    assert!(skip_err.code == "CustomInfo" || skip_err.code.starts_with("TaskNot"));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_before);

    // Server-observed task rejects client spoofing, then advances on real stationSvc.Undock
    let spoof_res = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(undock_tid)]).await.unwrap();
    assert_eq!(spoof_res, PyRep::Bool(false));
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(ship_id)]).await.expect("stationSvc.Undock");
    let undock_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        undock_notifs.iter().any(|n| {
            n.event == "OnOperationTaskCompleted"
                && n.args == vec![PyRep::Integer(cat_id), PyRep::Integer(undock_op_id), PyRep::Integer(undock_tid)]
        }),
        "stationSvc.Undock must emit OnOperationTaskCompleted for server-observed task (cat={cat_id}, op={undock_op_id}, tid={undock_tid}), got: {:?}",
        undock_notifs.iter().map(|n| (&n.event, &n.args)).collect::<Vec<_>>()
    );
    if ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance) > bal_before {
        assert!(
            undock_notifs.iter().any(|n| {
                n.event == "OnAccountChange" && n.args.first().and_then(|a| a.as_str()) == Some("cash")
            }),
            "stationSvc.Undock ISK reward must emit OnAccountChange('cash')"
        );
    }
    let PyRep::Dict(undock_prog) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else {
        panic!("expected progress dict after Undock");
    };
    assert!(
        undock_prog.iter().any(|(k, v)| k.as_i64() == Some(undock_tid) && v.as_i64() == Some(100)),
        "stationSvc.Undock must complete active SDE undockFromStation operation task"
    );

    let mut ops_to_complete = cat_ops.clone();
    if let Some((&isk_op, _)) = op_defs
        .op_isk_rewards
        .iter()
        .find(|(oid, _)| op_defs.op_tasks.get(oid).is_some_and(|ts| !ts.is_empty()))
    {
        if !ops_to_complete.contains(&isk_op) {
            ops_to_complete.push(isk_op);
        }
    }
    for &oid in &ops_to_complete {
        sess.role |= eve_machonet::service_roles::ROLE_GMH;
        rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(oid), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap();
        sess.role &= !eve_machonet::service_roles::ROLE_GMH;
        sess.role |= eve_machonet::service_roles::ROLE_GMH;
        for &tid in op_defs.op_tasks.get(&oid).into_iter().flatten() {
            let _ = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(tid)]).await.unwrap();
        }
        sess.role &= !eve_machonet::service_roles::ROLE_GMH;
    }

    let cat_ok = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.expect("complete category");
    assert_eq!(cat_ok, PyRep::Bool(true));
    let bal_after_cat = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
    assert!(bal_after_cat >= bal_before, "SDE operation/category ISK rewards must not reduce character balance");
    let _ = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_after_cat, "Category reward must be idempotent");

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    let mut sess_reloaded = char_session(&ctx_reloaded, PILOT_1, None, Some(SYS_ID), Some(ship_id));
    let PyRep::Dict(prog_entries) = rpc(&ctx_reloaded, &mut sess_reloaded, "operationsManager", "get_character_progress", &[]).await.unwrap() else {
        panic!("expected progress dict");
    };
    assert!(prog_entries.iter().any(|(k, v)| k.as_i64() == Some(cat_id) && v.as_i64() == Some(100)));
}

#[tokio::test]
async fn test_opportunities_complete_from_events_and_persist() {
    // parity: feat:pve.opportunities
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let (ship_id, account_id) = ctx.db.with_snapshot(|s| (s.characters[&PILOT_1].active_ship_id, s.characters[&PILOT_1].user_id));
    let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), Some(ship_id));
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await.unwrap();
    sess.station_id = Some(STATION_ID);
    sess.solar_system_id = Some(SYS_ID);
    sess.ship_id = Some(ship_id);
    sess.user_id = Some(account_id);

    let undock_ach_id = ctx
        .quasar_gateway
        .progression
        .achievements
        .definitions
        .iter()
        .find(|d| !d.is_client_achievement && d.target == 1 && d.conditions.iter().any(|c| c.event_name == "undockFromStation"))
        .map(|d| d.id)
        .expect("SDE server-observed undockFromStation opportunity");

    // Negative: Client opportunity PublishEventPayload records UI tracking/history ONLY and NEVER completes opportunities
    let (mut ctag, mut disc_opp, mut disc_ev) = (Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "opp_undock_01", &mut ctag);
    eve_proto::quasar::encode_bytes_field(1, &ctag, &mut disc_opp);
    eve_proto::quasar::encode_varint_field(2, 999, &mut disc_opp);
    eve_proto::quasar::encode_bytes_field(1, &disc_opp, &mut disc_ev);
    let any = eve_proto::quasar::GoogleAny {
        type_url: "type.googleapis.com/eve_public.opportunity.api.Discovered".to_string(),
        value: disc_ev,
    };
    rpc(&ctx, &mut sess, "publicGatewaySvc", "PublishEventPayload", &[PyRep::Bytes(any.encode_proto())]).await.unwrap();

    ctx.db.with_snapshot(|s| {
        let opp_json: serde_json::Value = s.quasar_state_blobs.get("quasar_opportunities").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        let completed = opp_json.get(PILOT_1.to_string()).and_then(|c| c.get("completed")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        assert!(completed.is_empty(), "Client PublishEventPayload must never mark server opportunities completed");
    });

    // Positive: Real stationSvc.Undock records server-observed opportunity completion in quasar_opportunities
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(ship_id)]).await.expect("stationSvc.Undock");
    ctx.quasar_gateway.progression.achievements.save_to_db();

    let reloaded_db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen WorldRepository"));
    reloaded_db.with_snapshot(|s| {
        let opp_json: serde_json::Value = serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").unwrap()).unwrap();
        let ch_obj = &opp_json[PILOT_1.to_string()];
        let completed = ch_obj["completed"].as_array().expect("completed array");
        assert!(completed.iter().any(|x| x.as_u64() == Some(u64::from(undock_ach_id))));
        assert!(ch_obj["history"].to_string().contains("opp_undock_01"));
    });
}

}

#[allow(unused_imports, dead_code)]
mod mission_timers_ship_boarding_and_mercenary_dens {
    use super::*;

// Integration tests for mission timers, ship boarding, and Mercenary Den `feat:` rows.
//
// Covers mission bonus timers, mission offer expiry without penalty,
// mission level clone/space restrictions, research agent daily missions,
// ship boarding skill/clone checks, and Mercenary Den anchoring & loot.


use std::collections::HashMap;
use std::sync::Arc;

use crate::quasar_sov_equinox_hub::{id_req, rpc as q_sov_rpc, structure};
use crate::sovereignty_equinox_gameplay::{
    find_nullsec_planets, make_ball, register_online_pilot, rpc_call, ALLIANCE, CORP, PILOT,
    PILOT_SHIP,
};
use eve_db::{
    corp_roles, ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::STATUS_CONFLICT;
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::{save_rnd_research_entries, RndResearchEntry};
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    apply_mercenary_den_weapon_damage, MERC_DEN_BASE_HP, MERC_DEN_REINFORCEMENT_SECS,
    TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
};
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

const PILOT_A: i64 = 90_100_001;
const STATION_ID: i64 = 60_003_760;
const SYS_ID: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    crate::common::pin_clock(&ctx);
    ctx
}

fn char_session(
    char_id: i64,
    station_id: Option<i64>,
    solar_system_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_044);
    sess.station_id = station_id;
    sess.solar_system_id = solar_system_id;
    sess.ship_id = ship_id;
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn reload_db(ctx: &ServerContext) -> Arc<WorldRepository> {
    let db = WorldRepository::new_empty();
    let snap = ctx.db.with_snapshot(|s| s.clone());
    db.with_mut_snapshot(|s| *s = snap);
    Arc::new(db)
}

fn ensure_char(ctx: &ServerContext, char_id: i64, name: &str, war_fac: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| template);
        ch.character_id = char_id;
        ch.name = name.to_string();
        ch.corporation_id = 1_000_044;
        ch.solar_system_id = SYS_ID;
        ch.station_id = Some(STATION_ID);
        ch.active_ship_id = char_id + 10_000;
        ch.balance = IskCents::from_isk(10_000_000);
        ch.war_faction_id = war_fac;
    });
}

fn colocate_with_agent(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, agent_id: i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(SYS_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
}

#[tokio::test]
async fn test_pve_mission_bonus_timer_payout_and_expiry() {
    // parity: feat:pve.mission-bonus-timer
    eve_testkit::attest(&["feat:pve.mission-bonus-timer"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "BonusRunner", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 2 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .expect("Level 2 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(agent.corporation_id, 5.0);
    });

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
        .await
        .expect("Offer mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let accept_rep = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await
        .expect("Accept mission");
    let bonus_expiry = accept_rep.dict_get("bonusExpiryUnix").and_then(|v| v.as_i64()).unwrap() as u64;
    assert!(bonus_expiry > ctx.now_unix());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid)) {
            m.npcs_remaining = 0;
        }
    });
    let comp = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect("Complete within bonus window");
    let bonus_paid = comp.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap();
    assert!(bonus_paid > 0);

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Completed");
    });

    let agent_l1 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.agent_id != agent.agent_id && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent_l1.agent_id);
    let offer_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_l1.agent_id)])
        .await
        .expect("Offer mission B");
    let mid_b = offer_b.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid_b as i64)])
        .await
        .expect("Accept mission B");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid_b)) {
            m.npcs_remaining = 0;
        }
    });
    ctx.advance_secs(10_800);
    let comp_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid_b as i64)])
        .await
        .expect("Complete after bonus window");
    assert_eq!(comp_b.dict_get("bonusReward").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_pve_mission_offer_expiry_no_penalty_and_cosmos_lockout() {
    // parity: feat:pve.mission-offer-expiry-no-penalty
    eve_testkit::attest(&["feat:pve.mission-offer-expiry-no-penalty"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "OfferExpiryPilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id != 18 && a.solar_system_id.is_some())
        .expect("Level 1 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
        .await
        .expect("Offer mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let standing_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0)
    });

    ctx.advance_secs(605_000);
    let _ = rpc(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Expired");
        let standing_after = snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0);
        assert_eq!(standing_before, standing_after);
    });

    let cosmos_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5 && a.solar_system_id.is_some())
        .expect("Data Center agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, cosmos_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(cosmos_agent.corporation_id, 5.0);
        snap.active_missions.insert(
            (PILOT_A, 99_999),
            ActiveMissionRecord {
                mission_id: 99_999,
                character_id: PILOT_A,
                agent_id: cosmos_agent.agent_id,
                solar_system_id: SYS_ID,
                state: "Expired".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });
    let reoffer_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(cosmos_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(reoffer_err.code, "CosmosMissionExpired");
}

#[tokio::test]
async fn test_pve_mission_level_clone_and_space_restrictions() {
    // parity: feat:pve.mission-level-clone-and-space-restrictions
    eve_testkit::attest(&["feat:pve.mission-level-clone-and-space-restrictions"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "CloneSpacePilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let l4_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 4 && a.solar_system_id.is_some() && a.station_id.is_some() && a.agent_type_id == 2 && a.division_id != 18)
        .expect("Level 4 agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, l4_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_A).unwrap();
        ch.clone_grade = 1;
        ch.standings.insert(l4_agent.corporation_id, 8.0);
        ch.standings.insert(l4_agent.agent_id, 8.0);
    });

    let alpha_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(alpha_err.code, "CantDoMissionAlphaClone");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().clone_grade = 2;
    });
    let omega_ok = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
        .await
        .expect("Omega clone L4 mission");
    assert!(omega_ok.dict_get("missionID").and_then(|v| v.as_i64()).is_some());

    let l5_highsec = ctx.sde.agents.get(&3008513).expect("L5 highsec agent");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, l5_highsec.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(l5_highsec.corporation_id, 9.0);
    });
    let space_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l5_highsec.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(space_err.code, "CantDoMission");
}

#[tokio::test]
async fn test_pve_research_agent_daily_mission_and_persistence() {
    // parity: feat:pve.research-agent-daily-mission
    eve_testkit::attest(&["feat:pve.research-agent-daily-mission"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "RndPilot", None);
    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

    let rnd_agent = ctx.sde.agents.get(&3009318).expect("R&D agent 3009318");
    colocate_with_agent(&ctx, &mut sess, PILOT_A, rnd_agent.agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_A).unwrap();
        ch.clone_grade = 2;
        ch.standings.insert(rnd_agent.corporation_id, 6.0);
    });

    let no_res_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
        .await
        .unwrap_err();
    assert_eq!(no_res_err.code, "ResearchNotActive");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        save_rnd_research_entries(
            snap,
            PILOT_A,
            &[RndResearchEntry {
                agent_id: rnd_agent.agent_id,
                research_type_id: 11444,
                points_per_day: 120.0,
                initial_points: 50.0,
                started_unix: now,
            }],
        );
    });

    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
        .await
        .expect("Offer daily R&D mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(mid > 0);

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Offered");
    });
}

#[tokio::test]
async fn test_ships_board_ship_skill_and_clone_check_and_persistence() {
    // parity: feat:ships.board-ship-skill-and-clone-check
    eve_testkit::attest(&["feat:ships.board-ship-skill-and-clone-check"]);
    let ctx = new_ctx();
    ensure_char(&ctx, PILOT_A, "ShipBoarder", None);
    let target_ship_id = 910_001i64;
    let frigate_type_id = 587u32; // Rifter (requires Minmatar Frigate 3330)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&PILOT_A);
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: frigate_type_id,
                owner_id: PILOT_A,
                location_id: STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));
    let skill_err = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
        .await
        .unwrap_err();
    assert_eq!(skill_err.code, "ShipHasSkillPrerequisites");

    let saved_skills = ctx.db.with_snapshot(|s| s.skills.get(&90_000_001).cloned().unwrap_or_default());
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.insert(PILOT_A, saved_skills);
    });
    let board_rep = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
        .await
        .expect("Board ship with required skill");
    let PyRep::Tuple(ship_state) = board_rep else {
        panic!("Expected 4-tuple shipState from ship.Board, got {board_rep:?}");
    };
    assert_eq!(ship_state.len(), 4);
    assert_eq!(sess.ship_id, Some(target_ship_id));

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert_eq!(snap.characters[&PILOT_A].active_ship_id, target_ship_id);
    });
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_anchoring_validation_and_persistence() {
    // parity: feat:sovereignty.mercenary-den-anchoring
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-anchoring"]);
    let ctx = new_ctx();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        72_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        DVec3::new(50_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_201,
            InventoryItemRecord {
                item_id: 990_201,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenTooFarFromSkyhook"
    );

    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet)])
            .await
            .unwrap_err()
            .code,
        "PlanetNotTemperate"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
    });
    let den_rep = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
        .await
        .expect("Anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let reloaded = reload_db(&ctx);
    reloaded.with_snapshot(|snap| {
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_id));
    });
}

async fn setup_anchored_merc_den(ctx: &ServerContext, sess_id: u64, with_hub: bool) -> (SessionState, i64, i64) {
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(ctx);
    let hub_id = 1_050_000_001i64;
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);
    let mut s_owner = register_online_pilot(
        ctx, sess_id, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        if with_hub {
            snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        }
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_200 + sess_id as i64,
            InventoryItemRecord {
                item_id: 990_200 + sess_id as i64,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;
    let den_rep = rpc_call(
        ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    )
    .await
    .expect("Anchor Mercenary Den");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    (s_owner, hub_id, den_id)
}

#[tokio::test]
async fn test_sovereignty_mercenary_den_reinforcement_loot_and_wreck() {
    // parity: feat:sovereignty.mercenary-den-reinforcement-loot
    eve_testkit::attest(&["feat:sovereignty.mercenary-den-reinforcement-loot"]);
    let ctx = new_ctx();
    let (mut s_owner, _hub_id, den_id) = setup_anchored_merc_den(&ctx, 72_004, false).await;

    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..8i64 {
            snap.items.insert(
                994_000 + idx,
                InventoryItemRecord {
                    item_id: 994_000 + idx,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: PILOT,
                    location_id: den_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        }
    });

    let reinf_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .expect("Deplete shield into reinforcement");
    assert_eq!(reinf_rep.dict_get("state").and_then(|v| v.as_str()), Some("HullReinforced"));
    assert_eq!(
        q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
            .await
            .status_code,
        STATUS_CONFLICT
    );

    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 120);
    let dest_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .expect("Deplete hull in vulnerability window");
    assert_eq!(dest_rep.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
    let dropped_stacks = dest_rep.dict_get("droppedStacks").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!((1..=7).contains(&dropped_stacks));
}

}
