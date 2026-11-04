//! End-to-end integration tests for 8 remediated Fleet & Industry parity features:
//! - `feat:fleet.boss-handover`
//! - `feat:fleet.stored-formations-and-warp-shapes`
//! - `feat:fleet.target-tagging`
//! - `feat:industry.blueprint-me-research`
//! - `feat:industry.capital-construction-restrictions`
//! - `feat:industry.eiv-adjusted-prices`
//! - `feat:industry.facility-tax`
//! - `feat:industry.job-cost`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, MarketHistoryDayRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{DVec3, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn grant_skill(ctx: &ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_type_id,
            TrainedSkillRecord { type_id: skill_type_id, level, skill_points: 256_000.0 },
        );
    });
}

fn add_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn seed_bp_materials(ctx: &ServerContext, bp_tid: u32, owner_id: i64, location_id: i64, runs: i32, base_id: i64) {
    let mats: Vec<(u32, u64)> = ctx
        .sde
        .blueprints
        .get(&bp_tid)
        .map(|bp| {
            if !bp.materials.is_empty() {
                bp.materials.iter().map(|m| (m.type_id, m.quantity)).collect()
            } else {
                bp.activities
                    .get("manufacturing")
                    .or_else(|| bp.activities.get("1"))
                    .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default();
    for (idx, (tid, qty)) in mats.into_iter().enumerate() {
        add_item(
            ctx,
            base_id + idx as i64,
            tid,
            owner_id,
            location_id,
            InventoryFlag::Hangar,
            (qty as i32).saturating_mul(runs),
            false,
        );
    }
}

async fn form_fleet_with_two(ctx: &ServerContext, s1: &mut SessionState, s2: &mut SessionState, p2: i64) -> i64 {
    let init = rpc(ctx, s1, "fleetObjectHandler", "CreateFleet", &[]).await.expect("CreateFleet");
    let fleet_id = init.as_i64().expect("fleetID");
    s1.fleet_id = Some(fleet_id);
    rpc(ctx, s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite p2");
    rpc(ctx, s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id), PyRep::Integer(587)])
        .await
        .expect("AcceptInvite p2");
    s2.fleet_id = Some(fleet_id);
    fleet_id
}

#[tokio::test]
async fn test_fleet_boss_handover_on_leave_and_disconnect() {
    // parity: feat:fleet.boss-handover
    let ctx = ServerContext::new();
    let p3: i64 = 90_000_003;
    ctx.db.provision_gm_account("pilot3", "pilot3-secret", p3, "PilotThree");
    let mut s1 = select_pilot(&ctx, 1, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 2, "other", "other-secret", PILOT_2).await;
    let mut s3 = select_pilot(&ctx, 3, "pilot3", "pilot3-secret", p3).await;

    let fleet_id = form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;
    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(p3), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
        .await
        .expect("Invite p3");
    rpc(&ctx, &mut s3, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id), PyRep::Integer(587)])
        .await
        .expect("AcceptInvite p3");
    s3.fleet_id = Some(fleet_id);

    ctx.drain_notifications_for_char(PILOT_2);
    ctx.drain_notifications_for_char(p3);

    // 1. Boss (PILOT_1) leaves fleet -> boss passes automatically to PILOT_2 and all members are notified
    rpc(&ctx, &mut s1, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("Boss LeaveFleet");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id)), Some(PILOT_2));

    let n2 = ctx.drain_notifications_for_char(PILOT_2);
    let n3 = ctx.drain_notifications_for_char(p3);
    assert!(n2.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n2.iter().any(|n| n.event == "OnFleetMemberChanged"));
    assert!(n3.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n3.iter().any(|n| n.event == "OnFleetMemberChanged"));

    // 2. New boss (PILOT_2) disconnects -> after grace expiry, boss passes to p3 and p3 is notified
    eve_server::services::fleet_social::fleet_social_notify::handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.advance_virtual_clock(125);
    eve_server::services::fleet_social::fleet_social_notify::tick_fleet_lifecycle(&ctx);

    assert_eq!(ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).map(|f| f.boss_id)), Some(p3));
    let n3_dc = ctx.drain_notifications_for_char(p3);
    assert!(n3_dc.iter().any(|n| n.event == "OnFleetLeave"));
    assert!(n3_dc.iter().any(|n| n.event == "OnFleetMemberChanged"));
}

#[tokio::test]
async fn test_fleet_stored_formations_and_warp_shapes() {
    // parity: feat:fleet.stored-formations-and-warp-shapes
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 11, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 12, "other", "other-secret", PILOT_2).await;

    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    let ship1 = 200_880_001_i64;
    let ship2 = 200_880_002_i64;
    for (sid, x) in [(ship1, 10_000.0), (ship2, 20_000.0)] {
        let _ = sys.spawn_ball(eve_destiny::Ball {
            id: sid,
            type_id: 587,
            position: DVec3::new(x, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;
    }
    s1.ship_id = Some(ship1);
    s2.ship_id = Some(ship2);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.active_ship_id = ship1;
            c.station_id = None;
        }
        if let Some(c) = snap.characters.get_mut(&PILOT_2) {
            c.active_ship_id = ship2;
            c.station_id = None;
        }
    });

    form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;
    rpc(&ctx, &mut s2, "fleetObjectHandler", "SetTakesFleetWarp", &[PyRep::Bool(true)])
        .await
        .expect("SetTakesFleetWarp");

    let set_res = rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        "SetFleetFormation",
        &[PyRep::Integer(3), PyRep::Real(5_000.0), PyRep::Real(10_000.0)],
    )
    .await
    .expect("SetFleetFormation");
    assert_eq!(set_res.dict_get("formationName"), Some(&PyRep::String("Arrow".into())));
    // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
    let get_res = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetFleetFormation", &[]).await.expect("GetFleetFormation");
    assert_eq!(get_res.dict_get("formationID"), Some(&PyRep::Integer(3)));

    let dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z), PyRep::Real(0.0)],
    )
    .await
    .expect("FleetWarp");

    for _ in 0..100 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b1) = sys.get_ball_snapshot(ship1).await.unwrap().unwrap();
    let (_, b2) = sys.get_ball_snapshot(ship2).await.unwrap().unwrap();
    let sep = b1.position.distance(b2.position);
    assert!(
        (sep - 5_000.0 * 2.0_f64.sqrt()).abs() < 250.0,
        "Arrow formation must land members at deterministic 5km*sqrt(2) offset, got sep={sep}"
    );
}

#[tokio::test]
async fn test_fleet_target_tagging_validation_lock_and_destruction() {
    // parity: feat:fleet.target-tagging
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 21, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 22, "other", "other-secret", PILOT_2).await;

    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock s1");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[]).await.expect("Undock s2");
    let ship1 = s1.ship_id.expect("ship1");
    let ship2 = s2.ship_id.expect("ship2");

    form_fleet_with_two(&ctx, &mut s1, &mut s2, PILOT_2).await;

    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let err_unlocked = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetTargetTag", &[PyRep::Integer(ship2), PyRep::String("A".into())])
        .await
        .expect_err("Unlocked target must fail");
    assert_eq!(err_unlocked.code, "TargetNotLocked");

    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let err_invalid = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetTargetTag", &[PyRep::Integer(ship2), PyRep::String("AA".into())])
        .await
        .expect_err("Invalid tag must fail");
    assert_eq!(err_invalid.code, "InvalidTargetTag");

    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    sys.add_target_lock(
        ship1,
        TargetLock { target_id: ship2, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) },
    )
    .await
    .unwrap();
    ctx.drain_notifications_for_char(PILOT_2);

    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let tag_res = rpc(&ctx, &mut s1, "fleetObjectHandler", "SetTargetTag", &[PyRep::Integer(ship2), PyRep::String("A".into())])
        .await
        .expect("SetTargetTag A");
    assert_eq!(tag_res.dict_get("tag"), Some(&PyRep::String("A".into())));
    assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnFleetStateChange"));

    eve_server::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(&ctx, ship2);
    // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
    let tags_after = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetTargetTags", &[]).await.expect("GetTargetTags");
    assert_eq!(tags_after, PyRep::Dict(Vec::new()));
}

#[tokio::test]
async fn test_industry_blueprint_me_research_exponential_and_bom_reduction() {
    // parity: feat:industry.blueprint-me-research
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 31, "demo", "demo-secret", PILOT_1).await;
    grant_skill(&ctx, PILOT_1, 3380, 5);
    grant_skill(&ctx, PILOT_1, 3387, 5);
    grant_skill(&ctx, PILOT_1, 3388, 5);
    grant_skill(&ctx, PILOT_1, 3409, 5);

    let bpo_id = 880_000_001;
    let bpo_unresearched = 880_000_002;
    add_item(&ctx, bpo_id, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    add_item(&ctx, bpo_unresearched, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, 687, PILOT_1, JITA_STATION, 25, 880_000_100);

    let rank = ctx.sde.blueprints.get(&687).unwrap().research_me_time_sec.max(1) as f64 / 105.0;
    let me_1 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("ME 0->1");
    let dur_1 = me_1.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(dur_1, (105.0 * rank * 0.75 * 0.85).round() as i64);
    let job_1 = me_1.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_virtual_clock((dur_1 + 10) as u64);
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(job_1)]).await.expect("Complete ME 1");

    let me_9 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(9)),
        ])],
    )
    .await
    .expect("ME 1->10");
    let dur_9 = me_9.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let exp_9_sum: u64 = [250, 595, 1_414, 3_360, 8_000, 19_000, 45_255, 107_700, 256_000].iter().sum();
    assert_eq!(dur_9, ((exp_9_sum as f64) * rank * 0.75 * 0.85).round() as i64);

    let job_9 = me_9.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_virtual_clock((dur_9 + 10) as u64);
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(job_9)]).await.expect("Complete ME 10");

    let mfg_me0 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_unresearched)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(10)),
        ])],
    )
    .await
    .expect("Mfg ME 0");
    let trit_me0 = mfg_me0.dict_get("tritaniumRequired").and_then(|v| v.as_i64()).unwrap();

    let mfg_me10 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(10)),
        ])],
    )
    .await
    .expect("Mfg ME 10");
    let trit_me10 = mfg_me10.dict_get("tritaniumRequired").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(trit_me10, ((trit_me0 as f64) * 0.90).ceil() as i64);
}

#[tokio::test]
async fn test_industry_capital_construction_restrictions_and_jump_freighters() {
    // parity: feat:industry.capital-construction-restrictions
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 41, "demo", "demo-secret", PILOT_1).await;
    grant_skill(&ctx, PILOT_1, 3387, 5);

    let (dread_bp_tid, jf_bp_tid) = {
        let mut d_tid = 0;
        let mut jf_tid = 0;
        for (&bp_tid, bp) in &ctx.sde.blueprints {
            if let Some(pt) = ctx.sde.types.get(&bp.product_type_id) {
                if pt.group_id == 485 && d_tid == 0 {
                    d_tid = bp_tid;
                }
                if pt.group_id == 902 && jf_tid == 0 {
                    jf_tid = bp_tid;
                }
            }
        }
        (d_tid, jf_tid)
    };
    assert!(dread_bp_tid > 0 && jf_bp_tid > 0);

    let dread_bpo = 881_000_001;
    let jf_bpo = 881_000_002;
    add_item(&ctx, dread_bpo, dread_bp_tid, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    add_item(&ctx, jf_bpo, jf_bp_tid, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, jf_bp_tid, PILOT_1, JITA_STATION, 1, 881_000_100);

    let err_hs = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(dread_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Dreadnought in High-Sec must fail");
    assert_eq!(err_hs.code, "CapitalConstructionForbiddenInHighSec");

    let jf_res = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(jf_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Jump Freighter in High-Sec must succeed");
    assert!(jf_res.dict_get("jobID").is_some());

    let low_sys: u32 = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.region_id != 10_000_070 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap();
    let azbel_id: i64 = 1_000_881_001;
    s1.solar_system_id = Some(low_sys);
    s1.station_id = Some(azbel_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = low_sys;
            ch.station_id = Some(azbel_id);
        }
        snap.structures.insert(
            azbel_id,
            UpwellStructureRecord {
                structure_id: azbel_id,
                type_id: 35826,
                solar_system_id: low_sys,
                owner_corp_id: 1_000_035,
                name: "LowSec Azbel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                ..Default::default()
            },
        );
    });
    add_item(&ctx, 881_000_098, 35878, 1_000_035, azbel_id, InventoryFlag::ServiceSlot1, 1, true);
    add_item(&ctx, dread_bpo, dread_bp_tid, PILOT_1, azbel_id, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, dread_bp_tid, PILOT_1, azbel_id, 1, 881_000_200);
    let err_no_yard = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(dread_bpo)),
            ("facilityID", PyRep::Integer(azbel_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect_err("Azbel without Capital Shipyard must fail");
    assert_eq!(err_no_yard.code, "FacilityNotCapitalCapable");

    add_item(&ctx, 881_000_099, 35881, 1_000_035, azbel_id, InventoryFlag::ServiceSlot0, 1, true);
    let cap_ok = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(dread_bpo)),
            ("facilityID", PyRep::Integer(azbel_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Azbel with Standup Capital Shipyard I must succeed");
    assert!(cap_ok.dict_get("jobID").is_some());
}

#[tokio::test]
async fn test_industry_eiv_adjusted_prices_ema() {
    // parity: feat:industry.eiv-adjusted-prices
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 51, "demo", "demo-secret", PILOT_1).await;

    let bpo_id = 882_000_001;
    add_item(&ctx, bpo_id, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, 687, PILOT_1, JITA_STATION, 1, 882_000_100);

    ctx.db.with_mut_snapshot(|snap| {
        let hist: Vec<MarketHistoryDayRecord> = (1..=28)
            .map(|d| MarketHistoryDayRecord {
                date: format!("2026-10-{d:02}"),
                average: 50.0,
                highest: 52.0,
                lowest: 48.0,
                order_count: 100,
                volume: 1_000_000,
                open_price: Some(50.0),
                close_price: Some(50.0),
            })
            .collect();
        snap.market_history.insert((10_000_002, 34), hist);
    });

    let res = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("InstallJob with market history EIV");
    let eiv = res.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    assert!(eiv >= 27_000_000.0, "EIV must use 28-day market_history adjusted prices, got {eiv}");
}

#[tokio::test]
async fn test_industry_facility_tax_npc_and_structure_cap() {
    // parity: feat:industry.facility-tax
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 61, "demo", "demo-secret", PILOT_1).await;
    grant_skill(&ctx, PILOT_1, 3387, 5);

    let bpo_id = 883_000_001;
    add_item(&ctx, bpo_id, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, 687, PILOT_1, JITA_STATION, 1, 883_000_100);

    let npc_res = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("NPC InstallJob");
    let npc_eiv = npc_res.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let npc_tax = npc_res.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((npc_tax - (npc_eiv * 0.0025 * 100.0).round() / 100.0).abs() < 0.02);

    let raitaru_id: i64 = 1_000_883_001;
    let corp_id = ctx.db.get_character(PILOT_1).unwrap().corporation_id;
    s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(raitaru_id, UpwellStructureRecord {
            structure_id: raitaru_id, type_id: 35825, solar_system_id: JITA_SYS,
            owner_corp_id: corp_id, name: "Tax Raitaru".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 1_000,
            ..Default::default()
        });
    });
    add_item(&ctx, 883_000_099, 35878, corp_id, raitaru_id, InventoryFlag::ServiceSlot0, 1, true);
    let cap_err = rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &[
        PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.15))]),
    ]).await.expect_err("Structure industry tax > 10% must fail");
    assert_eq!(cap_err.code, "InvalidFacilityTaxRate");

    rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &[
        PyRep::Integer(raitaru_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.075))]),
    ]).await.expect("SetFacilityTaxes 7.5%");

    let bpo_2 = 883_000_003;
    add_item(&ctx, bpo_2, 687, PILOT_1, raitaru_id, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, 687, PILOT_1, raitaru_id, 1, 883_000_200);
    let st_res = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_2)), ("facilityID", PyRep::Integer(raitaru_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("Structure InstallJob");
    let st_eiv = st_res.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let st_tax = st_res.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((st_tax - (st_eiv * 0.075 * 100.0).round() / 100.0).abs() < 0.02);
}

#[tokio::test]
async fn test_industry_job_cost_canonical_formula_and_sci() {
    // parity: feat:industry.job-cost
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 71, "demo", "demo-secret", PILOT_1).await;
    grant_skill(&ctx, PILOT_1, 3387, 5);

    let bpo_id = 884_000_001;
    add_item(&ctx, bpo_id, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_bp_materials(&ctx, 687, PILOT_1, JITA_STATION, 3, 884_000_100);

    let res = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_id)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(2)),
    ])]).await.expect("InstallJob cost check");

    let eiv_per_run = res.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let total_eiv = eiv_per_run * 2.0;
    let sci = res.dict_get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
    let base_cost = res.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
    let fac_tax = res.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    let scc = res.dict_get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
    let total = res.dict_get("totalJobCost").and_then(|v| v.as_f64()).unwrap();

    let exp_base = (total_eiv * sci * 1.0 * 100.0).round() / 100.0;
    let exp_tax = (total_eiv * 0.0025 * 100.0).round() / 100.0;
    let exp_scc = (total_eiv * 0.04 * 100.0).round() / 100.0;
    assert!((base_cost - exp_base).abs() < 0.02);
    assert!((fac_tax - exp_tax).abs() < 0.02);
    assert!((scc - exp_scc).abs() < 0.02);
    assert!((total - (exp_base + exp_tax + exp_scc)).abs() < 0.02);

    let bpo_2 = 884_000_003;
    add_item(&ctx, bpo_2, 687, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
    let res2 = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_2)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("Second InstallJob SCI check");
    let sci2 = res2.dict_get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
    assert!((sci2 - 0.50).abs() < 1e-6);
}
