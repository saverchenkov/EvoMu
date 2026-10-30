//! Integration tests for open `feat:` rows (Batch 6).
//!
//! Covers FW-exclusive boosters in LP stores, Mobile Scan Inhibitor d-scan/probe
//! suppression, Mobile Siphon Unit refinery material theft, seasonal login
//! campaigns, AIR operations tutorial, Project Discovery, and Opportunities.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::quasar::QuasarMessage;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{ActiveScannerProbe, ServerContext};
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;
const SYS_ID: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_ball(id: i64, type_id: u32, position: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::GotoDirection { direction: DVec3::Z },
    }
}

fn char_session(char_id: i64, station_id: Option<i64>, solar_system_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
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

#[tokio::test]
async fn test_fw_boosters_obtainable_from_lp_stores() {
    // parity: feat:fw.fw-boosters
    let ctx = new_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    // 24th Imperial Crusade (Amarr FW militia corp 1_000_179) offers Imperial Electronics Booster I (91916)
    let offers_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_179)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected list of LP offers");
    };
    assert!(
        offers.iter().any(|o| o.dict_get("typeID").and_then(|v| v.as_i64()) == Some(91916)),
        "FW militia LP store must offer Imperial Electronics Booster I (91916)"
    );

    // Non-FW corp (1_000_044) does not offer FW-exclusive boosters
    let non_fw_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_044)])
        .await
        .expect("GetAvailableOffersFromCorp non-FW");
    if let PyRep::List(non_fw_offers) = non_fw_rep {
        assert!(
            !non_fw_offers.iter().any(|o| o.dict_get("typeID").and_then(|v| v.as_i64()) == Some(91916)),
            "Non-FW corp must not offer FW-exclusive boosters"
        );
    }

    // Grant LP & ISK, enlist in Amarr militia (500_003), and dock at a 24th Imperial Crusade station (60_015_054)
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

    let take_res = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_179), PyRep::Integer(910_001), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter FW booster");
    assert_eq!(
        take_res.dict_get("deliveredTypeID").and_then(|v| v.as_i64()),
        Some(91916)
    );

    let delivered = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == PILOT_1 && i.location_id == fw_station_id && i.type_id == 91916)
            .map(|i| i.quantity)
            .sum::<i32>()
    });
    assert_eq!(delivered, 2, "2x Imperial Electronics Booster I must be delivered to station hangar");
}

#[tokio::test]
async fn test_mobile_scan_inhibitor_hides_ships_from_dscan_and_probes() {
    // parity: feat:structures.mobile-scan-inhibitor
    let ctx = new_ctx();
    let sys_handle = ctx.systems.get_or_spawn(SYS_ID);

    let scanner_ship_id = 700_001;
    let hidden_ship_id = 700_002;
    let visible_ship_id = 700_003;
    let inhibitor_id = 700_004;

    let scanner_ball = make_ball(scanner_ship_id, 587, DVec3::new(0.0, 0.0, 0.0), 40.0);
    let hidden_ball = make_ball(hidden_ship_id, 587, DVec3::new(100_000.0, 0.0, 0.0), 40.0);
    let visible_ball = make_ball(visible_ship_id, 587, DVec3::new(200_000.0, 0.0, 0.0), 40.0);
    let inhibitor_ball = make_ball(inhibitor_id, 33_589, DVec3::new(110_000.0, 0.0, 0.0), 500.0);

    sys_handle.spawn_ball(scanner_ball).await.unwrap();
    sys_handle.spawn_ball(hidden_ball).await.unwrap();
    sys_handle.spawn_ball(visible_ball).await.unwrap();
    sys_handle.spawn_ball(inhibitor_ball).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
            ch.solar_system_id = SYS_ID;
        }
    });

    let mut sess = char_session(PILOT_1, None, Some(SYS_ID), Some(scanner_ship_id));
    let dscan_rep = rpc(&ctx, &mut sess, "scanMgr", "ConeScan", &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)])
        .await
        .expect("ConeScan");
    let PyRep::List(dscan_rows) = dscan_rep else {
        panic!("expected d-scan list");
    };
    let dscan_ids: Vec<i64> = dscan_rows
        .iter()
        .filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64()))
        .collect();
    assert!(!dscan_ids.contains(&hidden_ship_id), "Ship within 30km of Mobile Scan Inhibitor must be hidden from d-scan");
    assert!(dscan_ids.contains(&visible_ship_id), "Ship outside 30km of Mobile Scan Inhibitor must be visible on d-scan");
    assert!(dscan_ids.contains(&inhibitor_id), "Mobile Scan Inhibitor itself must be visible on d-scan");

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
    assert!(probe_ids.contains(&visible_ship_id), "Ship outside 30km of Mobile Scan Inhibitor must be visible to combat probes");
    assert!(probe_ids.contains(&inhibitor_id), "Mobile Scan Inhibitor itself must be visible to combat probes");
}

#[tokio::test]
async fn test_mobile_siphon_steals_material_from_refineries_within_range() {
    // parity: feat:structures.mobile-siphon
    let ctx = new_ctx();
    let sys_handle = ctx.systems.get_or_spawn(SYS_ID);

    let refinery_id = 710_001;
    let siphon_near_id = 710_002;
    let siphon_far_id = 710_003;

    let ref_ball = make_ball(refinery_id, 35_835, DVec3::new(0.0, 0.0, 0.0), 5000.0);
    let near_ball = make_ball(siphon_near_id, 33_477, DVec3::new(50_000.0, 0.0, 0.0), 100.0);
    let far_ball = make_ball(siphon_far_id, 33_477, DVec3::new(250_000.0, 0.0, 0.0), 100.0);

    sys_handle.spawn_ball(ref_ball).await.unwrap();
    sys_handle.spawn_ball(near_ball).await.unwrap();
    sys_handle.spawn_ball(far_ball).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siphon_near_id,
            eve_db::InventoryItemRecord {
                item_id: siphon_near_id,
                type_id: 33_477,
                owner_id: PILOT_1,
                location_id: SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            siphon_far_id,
            eve_db::InventoryItemRecord {
                item_id: siphon_far_id,
                type_id: 33_477,
                owner_id: PILOT_1,
                location_id: SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            710_010,
            eve_db::InventoryItemRecord {
                item_id: 710_010,
                type_id: 45_490, // Zeolites moon ore
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
    let near_cap = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
        .await
        .expect("GetCapacityOfSiphon near")
        .as_f64()
        .unwrap_or(0.0);
    assert!(near_cap > 0.0, "Siphon within range of Athanor refinery must steal material (got cap {near_cap})");

    let far_cap = rpc(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_far_id)])
        .await
        .expect("GetCapacityOfSiphon far")
        .as_f64()
        .unwrap_or(1.0);
    assert_eq!(far_cap, 0.0, "Siphon out of range of refinery must not steal material");
}

#[tokio::test]
async fn test_seasonal_login_campaign_active_claim_and_history() {
    // parity: feat:pve.seasonal-login
    let ctx = new_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let active = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
        .await
        .expect("get_active_campaign");
    assert_ne!(active, PyRep::None, "Active seasonal login campaign must be returned");

    let claim = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_ne!(claim, PyRep::None, "Claiming seasonal login reward must succeed");

    let dup_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await;
    assert!(dup_err.is_err(), "Second seasonal login claim on same day must fail");

    let hist = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history");
    let PyRep::Dict(entries) = hist else {
        panic!("expected claim history dict");
    };
    assert_eq!(entries.len(), 1, "Claim history must record the claimed seasonal reward");
}

#[tokio::test]
async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
    // parity: feat:pve.operations-tutorial
    let ctx = new_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let state = rpc(&ctx, &mut sess, "operationsManager", "get_tutorial_state", &[])
        .await
        .expect("get_tutorial_state");
    assert_ne!(state, PyRep::None);

    let active_cat = rpc(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[])
        .await
        .expect("get_active_category_id");
    let cat_id = active_cat.as_i64().unwrap_or(1);

    let _ = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)])
        .await
        .expect("complete_category_for_character");

    let progress = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[])
        .await
        .expect("get_character_progress");
    assert_ne!(progress, PyRep::None, "Operations character progress must reflect completed category");
}

#[tokio::test]
async fn test_project_discovery_get_new_task_and_submit_rewards() {
    // parity: feat:pve.project-discovery
    let ctx = new_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let _ = rpc(&ctx, &mut sess, "ProjectDiscovery", "skip_tutorial", &[])
        .await
        .expect("skip_tutorial");
    let task = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    assert_ne!(task, PyRep::None, "ProjectDiscovery.get_new_task must return a task");

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
    assert_ne!(submit_res, PyRep::None, "ProjectDiscovery.post_classification must return scoring and reward payload");
}

#[tokio::test]
async fn test_opportunities_complete_from_events_and_persist() {
    // parity: feat:pve.opportunities
    let ctx = new_ctx();
    let mut sess = char_session(PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

    let (mut ctag, mut disc_opp, mut disc_ev) = (Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "opp_mining_01", &mut ctag);
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

    let persisted = ctx.db.with_snapshot(|snap| {
        snap.quasar_state_blobs
            .get("quasar_opportunities")
            .cloned()
            .unwrap_or_default()
    });
    assert!(
        persisted.contains("opp_mining_01"),
        "Opportunity completion/discovery must persist in quasar_opportunities (got: {persisted})"
    );
}
