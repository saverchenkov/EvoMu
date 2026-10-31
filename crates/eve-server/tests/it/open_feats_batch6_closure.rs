//! Integration tests for open `feat:` rows (Batch 6).
//!
//! Covers FW-exclusive boosters in LP stores, Mobile Scan Inhibitor d-scan/probe
//! suppression, Mobile Siphon Unit refinery material theft, seasonal login
//! campaigns, AIR operations tutorial, Project Discovery, and Opportunities.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::quasar::QuasarMessage;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{ActiveScannerProbe, ServerContext};
use eve_server::services::operations_manager::get_operation_defs;
use eve_server::services::project_discovery::{
    configure_project_discovery_mmos, set_project_discovery_task_consensus, LOOT_CRATE_TYPE_ID,
};
use eve_server::services::quasar_achievements::AchievementManager;
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;
const SYS_ID: u32 = 30_000_142;

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
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let demo_snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    db.with_mut_snapshot(|s| *s = demo_snap);
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    crate::common::pin_clock(&ctx);
    (TempDbGuard(db_path.clone()), db_path, ctx)
}

fn reload_ctx(db_path: &PathBuf, sde: Arc<eve_sde::SdeRegistry>) -> ServerContext {
    let db = Arc::new(eve_db::WorldRepository::open_sqlite(db_path).expect("reopen WorldRepository"));
    let ctx = ServerContext::with_sde_and_repository(sde, db);
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

fn char_session(char_id: i64, station_id: Option<i64>, solar_system_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64), character_id: Some(char_id), corporation_id: Some(1_000_044),
        station_id, solar_system_id, ship_id, ..SessionState::default()
    }
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_fw_boosters_obtainable_from_lp_stores() {
    // parity: feat:fw.fw-boosters
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    // Verify all 4 FW militia corps offer all 4 families (Electronics, Defense, Mobility, Hardpoint I/II/III = 12 each, 48 total)
    let mut fw_booster_types = HashSet::new();
    for &(corp_id, sample_elec, sample_hp) in &[
        (1_000_179_i64, 91916_i64, 91962_i64),
        (1_000_180_i64, 91920_i64, 91963_i64),
        (1_000_181_i64, 91921_i64, 91964_i64),
        (1_000_182_i64, 91922_i64, 91965_i64),
    ] {
        let rep = rpc(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(corp_id)],
        )
        .await
        .expect("GetAvailableOffersFromCorp");
        let PyRep::List(offers) = rep else {
            panic!("expected list of LP offers");
        };
        let tids: Vec<i64> = offers
            .iter()
            .filter_map(|o| o.dict_get("typeID").and_then(|v| v.as_i64()))
            .filter(|&t| (91916..=91977).contains(&t) && t != 91938)
            .collect();
        assert_eq!(tids.len(), 12, "Militia corp {corp_id} must offer all 12 faction FW boosters");
        assert!(tids.contains(&sample_elec) && tids.contains(&sample_hp));
        fw_booster_types.extend(tids);
    }
    assert_eq!(fw_booster_types.len(), 48, "All 48 SDE FW-exclusive boosters must be offered");

    // Non-FW corp (1_000_044) must not offer FW-exclusive boosters
    let non_fw_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1_000_044)],
    )
    .await
    .expect("GetAvailableOffersFromCorp non-FW");
    if let PyRep::List(non_fw_offers) = non_fw_rep {
        assert!(
            !non_fw_offers.iter().any(|o| {
                o.dict_get("typeID")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|t| fw_booster_types.contains(&t))
            }),
            "Non-FW corp must not offer FW-exclusive boosters"
        );
    }

    // Enlist in Amarr militia (500_003), dock at 24th Imperial Crusade station (60_015_054), buy Electronics & Hardpoint boosters
    let fw_station_id: i64 = 60_015_054;
    let fw_sys_id: u32 = 30_002_978;
    sess.station_id = Some(fw_station_id);
    sess.solar_system_id = Some(fw_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.loyalty_points.insert(1_000_179, 10_000);
            ch.balance = eve_db::IskCents(50_000_000_00);
            ch.war_faction_id = Some(500_003);
            ch.station_id = Some(fw_station_id);
            ch.solar_system_id = fw_sys_id;
        }
    });

    let take_elec = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_179), PyRep::Integer(910_001), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter Electronics Booster");
    assert_eq!(take_elec.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(91916));

    let take_hp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_179), PyRep::Integer(910_010), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter Hardpoint Booster");
    assert_eq!(take_hp.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(91962));

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    ctx_reloaded.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&PILOT_1).expect("character persisted");
        assert_eq!(ch.loyalty_points.get(&1_000_179).copied(), Some(5_500));
        let elec_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == PILOT_1 && i.location_id == fw_station_id && i.type_id == 91916)
            .map(|i| i.quantity)
            .sum();
        let hp_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == PILOT_1 && i.location_id == fw_station_id && i.type_id == 91962)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(elec_qty, 2, "2x Imperial Electronics Booster I must persist in hangar");
        assert_eq!(hp_qty, 1, "1x Imperial Hardpoint Booster I must persist in hangar");
    });
}

#[tokio::test]
async fn test_mobile_scan_inhibitor_hides_ships_from_dscan_and_probes() {
    // parity: feat:structures.mobile-scan-inhibitor
    let (_dir, _db_path, ctx) = new_persistent_ctx();
    let sys_handle = ctx.systems.get_or_spawn(SYS_ID);

    let scanner_ship_id = 700_001;
    let hidden_ship_id = 700_002;
    let visible_ship_id = 700_003;
    let inhibitor_id = 700_004;

    sys_handle
        .spawn_ball(make_ball(scanner_ship_id, 587, DVec3::new(0.0, 0.0, 0.0), 40.0))
        .await
        .unwrap();
    sys_handle
        .spawn_ball(make_ball(hidden_ship_id, 587, DVec3::new(100_000.0, 0.0, 0.0), 40.0))
        .await
        .unwrap();
    sys_handle
        .spawn_ball(make_ball(visible_ship_id, 587, DVec3::new(200_000.0, 0.0, 0.0), 40.0))
        .await
        .unwrap();
    sys_handle
        .spawn_ball(make_ball(inhibitor_id, 33_589, DVec3::new(110_000.0, 0.0, 0.0), 500.0))
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
            ch.solar_system_id = SYS_ID;
        }
    });

    // External scanner cannot d-scan hidden_ship_id inside 30km sphere, but sees visible_ship_id and inhibitor
    let mut sess = char_session(PILOT_1, None, Some(SYS_ID), Some(scanner_ship_id));
    let dscan_rep = rpc(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)],
    )
    .await
    .expect("ConeScan external");
    let PyRep::List(dscan_rows) = dscan_rep else {
        panic!("expected d-scan list");
    };
    let dscan_ids: Vec<i64> = dscan_rows
        .iter()
        .filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64()))
        .collect();
    assert!(!dscan_ids.contains(&hidden_ship_id), "Ship within 30km of Mobile Scan Inhibitor must be hidden from d-scan");
    assert!(dscan_ids.contains(&visible_ship_id), "Ship outside 30km must be visible on d-scan");
    assert!(dscan_ids.contains(&inhibitor_id), "Mobile Scan Inhibitor itself must be visible on d-scan");

    // Pilot INSIDE the 30km sphere can still d-scan outward
    let mut inside_sess = char_session(PILOT_1, None, Some(SYS_ID), Some(hidden_ship_id));
    let outward_rep = rpc(
        &ctx,
        &mut inside_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)],
    )
    .await
    .expect("ConeScan outward from inside inhibitor");
    let PyRep::List(outward_rows) = outward_rep else {
        panic!("expected outward d-scan list");
    };
    let outward_ids: Vec<i64> = outward_rows
        .iter()
        .filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64()))
        .collect();
    assert!(outward_ids.contains(&scanner_ship_id) && outward_ids.contains(&visible_ship_id), "Ship inside inhibitor sphere must still d-scan external ships");

    // Combat probes scanning the same area
    if let Ok(mut probes) = ctx.active_scanner_probes.write() {
        probes.insert(
            800_001,
            ActiveScannerProbe {
                probe_id: 800_001,
                owner_char_id: PILOT_1,
                ship_id: scanner_ship_id,
                solar_system_id: SYS_ID,
                probe_type_id: 30_486, // Sisters Combat Scanner Probe
                position: [110_000.0, 0.0, 0.0],
                destination: [110_000.0, 0.0, 0.0],
                range_step: 1,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 44.0,
                launcher_module_id: 0,
                max_flight_time_s: 3600.0,
                launched_at_unix: ctx.now_unix(),
                state: 1,
                warp_end_ft: None,
            },
        );
    }
    let probe_rep = rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[PyRep::List(Vec::new())])
        .await
        .expect("RequestScans");
    let PyRep::List(probe_rows) = probe_rep else {
        panic!("expected probe scan list");
    };
    let probe_ids: Vec<i64> = probe_rows
        .iter()
        .filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64()))
        .collect();
    assert!(!probe_ids.contains(&hidden_ship_id), "Ship within 30km of Mobile Scan Inhibitor must be hidden from combat probes");
    assert!(probe_ids.contains(&visible_ship_id), "Ship outside 30km must be visible to combat probes");
    assert!(probe_ids.contains(&inhibitor_id), "Mobile Scan Inhibitor itself must be visible to combat probes");
}

#[tokio::test]
async fn test_mobile_siphon_steals_material_from_refineries_within_range() {
    // parity: feat:structures.mobile-siphon
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let sys_handle = ctx.systems.get_or_spawn(SYS_ID);

    let refinery_id = 710_001;
    let siphon_near_id = 710_002;
    let siphon_far_id = 710_003;
    let ref_stack_id = 710_010;

    sys_handle
        .spawn_ball(make_ball(refinery_id, 35_835, DVec3::new(0.0, 0.0, 0.0), 5000.0))
        .await
        .unwrap();
    sys_handle
        .spawn_ball(make_ball(siphon_near_id, 33_477, DVec3::new(50_000.0, 0.0, 0.0), 100.0))
        .await
        .unwrap();
    sys_handle
        .spawn_ball(make_ball(siphon_far_id, 33_477, DVec3::new(250_000.0, 0.0, 0.0), 100.0))
        .await
        .unwrap();

    // Small Mobile Siphon Unit (33477): capacity=900 m3, siphonRawMaterial=60, siphonWasteAmount=10%
    // Atmospheric Gases (16634): group=427 (Moon Materials), volume=0.05 m3
    ctx.db.with_mut_snapshot(|snap| {
        for sid in [siphon_near_id, siphon_far_id] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 33_477,
                    owner_id: PILOT_1,
                    location_id: SYS_ID as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            ref_stack_id,
            eve_db::InventoryItemRecord {
                item_id: ref_stack_id,
                type_id: 16_634,
                owner_id: 1_000_044,
                location_id: refinery_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = char_session(PILOT_1, None, Some(SYS_ID), Some(700_001));
    let cap1 = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await
        .expect("GetCapacityOfSiphon cycle 1")
        .as_f64()
        .unwrap_or(0.0);
    assert!(cap1 > 0.0, "Siphon within 100km of Athanor refinery must steal raw material");

    // Repeated call at same timestamp must be idempotent (60 stolen + 6 wasted = 66 drained -> 134 left)
    let cap1_repeat = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await
        .expect("GetCapacityOfSiphon idempotent repeat")
        .as_f64()
        .unwrap_or(0.0);
    assert!((cap1 - cap1_repeat).abs() < 1e-9, "Repeated GetCapacityOfSiphon at same timestamp must be idempotent");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ref_stack_id].quantity, 134, "60 stolen + 10% (6) waste must drain 66 units");
    });

    // Advance 1 hour -> second siphon cycle stacks onto existing cargo stack (120 stolen, 68 left in refinery)
    ctx.advance_secs(3_600);
    let cap2 = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await
        .expect("GetCapacityOfSiphon cycle 2")
        .as_f64()
        .unwrap_or(0.0);
    assert!(cap2 > cap1, "Second hourly siphon cycle must accumulate stolen material");

    let far_cap = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_far_id)])
        .await
        .expect("GetCapacityOfSiphon far")
        .as_f64()
        .unwrap_or(1.0);
    assert_eq!(far_cap, 0.0, "Siphon beyond 100km of refinery must not steal material");

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    ctx_reloaded.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ref_stack_id].quantity, 68, "Drained refinery stack must persist");
        let siphon_stacks: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.location_id == siphon_near_id && i.type_id == 16_634)
            .collect();
        assert_eq!(siphon_stacks.len(), 1, "Stolen material must stack cleanly in siphon cargo");
        assert_eq!(siphon_stacks[0].quantity, 120, "Two cycles of 60 units must persist in siphon cargo");
    });
}

#[tokio::test]
async fn test_seasonal_login_campaign_active_claim_and_history() {
    // parity: feat:pve.seasonal-login
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let items_before = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == PILOT_1).count()
    });

    let active = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
        .await
        .expect("get_active_campaign");
    assert_ne!(active, PyRep::None, "Active seasonal login campaign must be returned");

    let claim = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    assert_eq!(claim, PyRep::Bool(true), "Claiming seasonal login reward must succeed");

    let dup_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await;
    assert!(dup_err.is_err(), "Second seasonal login claim on same day must fail");

    ctx.advance_secs(86_400);
    let claim_day2 = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    assert_eq!(claim_day2, PyRep::Bool(true));

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    let mut sess_reloaded = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);
    let hist = rpc(&ctx_reloaded, &mut sess_reloaded, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history after reload");
    let PyRep::Dict(entries) = hist else {
        panic!("expected claim history dict");
    };
    assert_eq!(entries.len(), 2, "Claim history must persist both claimed seasonal reward days");
    let items_after = ctx_reloaded.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == PILOT_1).count()
    });
    assert!(items_after > items_before, "Claimed SDE seasonal login rewards must persist in hangar");
}

#[tokio::test]
async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
    // parity: feat:pve.operations-tutorial
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let op_defs = get_operation_defs(&ctx.sde);
    let (&reward_op_id, op_tasks) = op_defs
        .op_tasks
        .iter()
        .find(|(oid, tasks)| {
            !tasks.is_empty()
                && (op_defs.op_isk_rewards.contains_key(oid)
                    || op_defs.op_completion_rewards.contains_key(oid)
                    || tasks.iter().any(|t| op_defs.task_completion_rewards.contains_key(t)))
        })
        .expect("SDE operations must include at least one operation with tasks and rewards");
    let op_tasks = op_tasks.clone();
    let reward_cat_id = op_defs
        .category_isk_rewards
        .keys()
        .copied()
        .find(|&cid| (1..=1_000).contains(&cid))
        .unwrap_or(1);

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
    let items_before = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1).count());

    let _ = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("start_tutorial".into()), PyRep::Integer(1)])
        .await
        .expect("start_tutorial");
    let state = rpc(&ctx, &mut sess, "operationsManager", "get_tutorial_state", &[])
        .await
        .expect("get_tutorial_state");
    assert_eq!(state.dict_get("is_tutorial_active"), Some(&PyRep::Bool(true)));

    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(reward_op_id), PyRep::Integer(SYS_ID as i64), PyRep::Integer(reward_cat_id)],
    )
    .await
    .expect("activate_operation_in_solar_system");

    for tid in &op_tasks {
        let _ = rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("task_complete".into()), PyRep::Integer(*tid)],
        )
        .await
        .expect("complete SDE operation task");
    }

    let _ = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(reward_cat_id)],
    )
    .await
    .expect("complete_category_for_character");

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    let mut sess_reloaded = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);
    let progress = rpc(&ctx_reloaded, &mut sess_reloaded, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress after reload");
    let PyRep::Dict(prog_entries) = progress else {
        panic!("expected progress dict");
    };
    assert!(
        prog_entries.iter().any(|(k, v)| k.as_i64() == Some(reward_cat_id) && v.as_i64() == Some(100)),
        "Completed category must report 100% progress after reload"
    );
    let bal_after = ctx_reloaded.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
    let items_after = ctx_reloaded.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1).count());
    assert!(
        bal_after > bal_before || items_after > items_before,
        "Completing SDE operation tasks/category must grant persisted ISK and/or item rewards"
    );
}

#[tokio::test]
async fn test_project_discovery_get_new_task_and_submit_rewards() {
    // parity: feat:pve.project-discovery
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    ctx.db.with_mut_snapshot(|snap| {
        configure_project_discovery_mmos(snap, 100.0, 250.0, 150.0, 50.0, 25);
        set_project_discovery_task_consensus(snap, 1, true);
    });
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);

    let _ = rpc(&ctx, &mut sess, "ProjectDiscovery", "skip_tutorial", &[])
        .await
        .expect("skip_tutorial");
    let task = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = task
        .dict_get("id")
        .or_else(|| task.dict_get("taskId"))
        .cloned()
        .unwrap_or(PyRep::Integer(1));

    let submit_res = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[task_id, PyRep::List(vec![PyRep::Integer(1)])],
    )
    .await
    .expect("ProjectDiscovery.post_classification");
    assert_eq!(submit_res.dict_get("XP_Reward").and_then(|v| v.as_f64()), Some(150.0));
    assert_eq!(submit_res.dict_get("AK_Reward").and_then(|v| v.as_i64()), Some(25));
    let isk_rw = submit_res.dict_get("ISK_Reward").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(isk_rw > 0.0, "Accurate classification with bonus sample must award ISK");
    assert_eq!(submit_res.dict_get("rank").and_then(|v| v.as_i64()), Some(2));

    let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
    let mut sess_reloaded = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);
    let ak_after = rpc(&ctx_reloaded, &mut sess_reloaded, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits");
    assert_eq!(ak_after, PyRep::Integer(25));
    ctx_reloaded.db.with_snapshot(|snap| {
        assert!(snap.characters[&PILOT_1].balance > bal_before, "Project Discovery ISK reward must persist");
        let crates: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == PILOT_1 && i.type_id == LOOT_CRATE_TYPE_ID)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(crates, 1, "Rank-up Flow Cytometry Rewards Crate (55358) must persist in hangar");
    });
}

#[tokio::test]
async fn test_opportunities_complete_from_events_and_persist() {
    // parity: feat:pve.opportunities
    let (_dir, db_path, ctx) = new_persistent_ctx();
    let (ship_id, account_id) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&PILOT_1];
        (ch.active_ship_id, ch.user_id)
    });
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), Some(ship_id));
    sess.user_id = Some(account_id);

    let undock_ach_id = ctx
        .quasar_gateway
        .progression
        .achievements
        .definitions
        .iter()
        .find(|d| !d.is_client_achievement && d.target == 1 && d.conditions.iter().any(|c| c.event_name == "undockFromStation"))
        .map(|d| d.id)
        .expect("SDE achievements must include server-observed undockFromStation opportunity");

    // Trigger real server-observed gameplay action: stationSvc.Undock
    let _ = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(ship_id)])
        .await
        .expect("stationSvc.Undock");

    // Also verify client opportunity tracking via publicGatewaySvc.PublishEventPayload
    let (mut ctag, mut disc_opp, mut disc_ev) = (Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "opp_undock_01", &mut ctag);
    eve_proto::quasar::encode_bytes_field(1, &ctag, &mut disc_opp);
    eve_proto::quasar::encode_varint_field(2, 1, &mut disc_opp);
    eve_proto::quasar::encode_bytes_field(1, &disc_opp, &mut disc_ev);
    let any = eve_proto::quasar::GoogleAny {
        type_url: "type.googleapis.com/eve_public.opportunity.api.Discovered".to_string(),
        value: disc_ev,
    };
    let _ = rpc(
        &ctx,
        &mut sess,
        "publicGatewaySvc",
        "PublishEventPayload",
        &[PyRep::Bytes(any.encode_proto())],
    )
    .await
    .expect("PublishEventPayload opportunity Discovered");

    // Reload from disk and verify server-observed completion persisted in both achievements & opportunities stores
    let reloaded_db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen WorldRepository"));
    let reloaded_mgr = AchievementManager::new_with_sde(Some(Arc::clone(&reloaded_db)), Some(Arc::clone(&ctx.sde)));
    let char_achs = reloaded_mgr
        .character_achievements
        .read()
        .unwrap()
        .get(&(PILOT_1 as u32))
        .cloned()
        .unwrap_or_default();
    assert!(
        char_achs.iter().any(|a| a.def.id == undock_ach_id && a.completed),
        "Server-observed undockFromStation opportunity ({undock_ach_id}) must persist completed=true across reload"
    );
    let opp_blob = reloaded_db.with_snapshot(|s| s.quasar_state_blobs.get("quasar_opportunities").cloned().unwrap_or_default());
    assert!(opp_blob.contains(&undock_ach_id.to_string()) && opp_blob.contains("opp_undock_01"));
}
