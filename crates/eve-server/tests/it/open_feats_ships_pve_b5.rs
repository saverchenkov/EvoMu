//! Integration tests for open ship and PvE feature rows (Batch 5).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30_000_142;
const RND_AGENT_ID: i64 = 3_009_318;
const RND_STATION_ID: i64 = 60_010_273;
const RND_SYSTEM_ID: u32 = 30_004_288;

const CRANE: u32 = 12_729; // Blockade Runner (group 1202)
const RAVEN: u32 = 638; // Battleship with Frigate Escape Bay
const RIFTER: u32 = 587; // Frigate
const FALCON: u32 = 11_957; // Force Recon (Covert Cyno)
const AVATAR: u32 = 11_567; // Titan (Jump Portal)
const CARGO_SCANNER: u32 = 442; // Cargo Scanner I (group 47)
const CYNO: u32 = 21_096;
const COVERT_CYNO: u32 = 28_646;
const JUMP_PORTAL: u32 = 23_953;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        PyRep::Tuple(items) => items.iter().find_map(|i| kv_get(i, key)),
        PyRep::Object { args, .. } => kv_get(args, key),
        _ => None,
    }
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    let (username, password) = if char_id == PILOT_2 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    common::login_pilot(ctx, &mut sess, username, password).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
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
                quantity,
                singleton: quantity == 1,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|(s, _)| {
            ctx.sde.types[&type_id]
                .default_attributes
                .get(s)
                .map(|&v| v as u32)
        })
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills {
            entry.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

async fn put_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.jump_cooldown_until_unix = 0;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.invalidate_ship_fitting(ship_id);
    let t = &ctx.sde.types[&type_id];
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 150.0,
        inertia: 1.0,
        mass: t.mass,
        radius: t.radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    ctx.systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ship");
}

#[tokio::test]
async fn test_blockade_runner_cargo_scan_immunity_and_frigate_escape_bay() {
    let ctx = new_ctx();
    let mut s1 = setup_pilot(&ctx, PILOT_1).await;
    let mut s2 = setup_pilot(&ctx, PILOT_2).await;

    let scanner_ship = 200_001;
    let br_ship = 200_002;
    let scanner_mod = 200_003;
    put_in_space(&ctx, &mut s1, PILOT_1, scanner_ship, RAVEN, JITA_SYSTEM, DVec3::ZERO).await;
    put_in_space(
        &ctx,
        &mut s2,
        PILOT_2,
        br_ship,
        CRANE,
        JITA_SYSTEM,
        DVec3::new(1000.0, 0.0, 0.0),
    )
    .await;

    grant_skills(&ctx, PILOT_1, CARGO_SCANNER);
    seed_item(
        &ctx,
        scanner_mod,
        CARGO_SCANNER,
        PILOT_1,
        scanner_ship,
        InventoryFlag::MedSlot0,
        1,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(scanner_mod, true);
    });
    ctx.invalidate_ship_fitting(scanner_ship);

    ctx.systems
        .get_or_spawn(JITA_SYSTEM)
        .add_target_lock(
            scanner_ship,
            eve_destiny::TargetLock {
                target_id: br_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("add_target_lock");

    let scan_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(scanner_mod),
            PyRep::String("cargoScan".into()),
            PyRep::Integer(br_ship),
        ],
    )
    .await
    .expect("Activate cargo scanner");
    assert_eq!(
        kv_get(&scan_res, "cargoScanImmune"),
        Some(&PyRep::Bool(true))
    );

    // Frigate Escape Bay: Raven (battleship) accepts 1 assembled frigate in FrigateEscapeBay (flag 179)
    let bs_id = 200_010;
    let frig_id = 200_011;
    seed_item(&ctx, bs_id, RAVEN, PILOT_1, 60_003_760, InventoryFlag::Hangar, 1);
    seed_item(&ctx, frig_id, RIFTER, PILOT_1, 60_003_760, InventoryFlag::Hangar, 1);
    s1.station_id = Some(60_003_760);
    s1.solar_system_id = Some(JITA_SYSTEM);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = Some(60_003_760);
        }
    });

    let kw = HashMap::new();
    let bind = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject");
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap_or(0) as u64,
        _ => 0,
    };
    let ship_inv = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(mgr_oid),
            "GetInventoryFromId",
            &[PyRep::Integer(bs_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetInventoryFromId");
    let ship_oid = ship_inv.as_i64().unwrap_or(0) as u64;
    let add_res = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_oid),
            "Add",
            &[
                PyRep::Integer(frig_id),
                PyRep::Integer(60_003_760),
                PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Add frigate to FrigateEscapeBay");
    assert_eq!(
        kv_get(&add_res, "flagID"),
        Some(&PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64))
    );
    assert_eq!(kv_get(&add_res, "itemID"), Some(&PyRep::Integer(frig_id)));
}

#[tokio::test]
async fn test_cyno_covert_cyno_jump_fatigue_and_titan_portal() {
    let ctx = new_ctx();
    let mut s1 = setup_pilot(&ctx, PILOT_1).await;
    let mut s2 = setup_pilot(&ctx, PILOT_2).await;

    let lowsec_sys = 30_002_537; // Amamake (0.4 low-sec)
    let origin_sys = 30_002_538; // Vard (0.4 low-sec, ~1.2 LY from Amamake)
    let beacon_ship = 210_001;
    let titan_ship = 210_002;
    let covert_mod = 210_003;
    let portal_mod = 210_004;

    put_in_space(&ctx, &mut s1, PILOT_1, beacon_ship, FALCON, lowsec_sys, DVec3::ZERO).await;
    put_in_space(&ctx, &mut s2, PILOT_2, titan_ship, AVATAR, origin_sys, DVec3::ZERO).await;

    grant_skills(&ctx, PILOT_1, COVERT_CYNO);
    seed_item(
        &ctx,
        covert_mod,
        COVERT_CYNO,
        PILOT_1,
        beacon_ship,
        InventoryFlag::HiSlot0,
        1,
    );
    seed_item(
        &ctx,
        210_005,
        16_273,
        PILOT_1,
        beacon_ship,
        InventoryFlag::Cargo,
        500,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(covert_mod, true);
        s.fleets.insert(
            777,
            eve_db::FleetRecord {
                fleet_id: 777,
                boss_id: PILOT_1,
                members: vec![PILOT_1, PILOT_2],
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(beacon_ship);

    let cyno_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(covert_mod),
            PyRep::String("cynosuralGeneration".into()),
        ],
    )
    .await
    .expect("Activate covert cyno");
    assert!(kv_get(&cyno_res, "beaconTypeID").is_some());
    let ozone_after_covert = ctx.db.with_snapshot(|s| {
        s.items
            .get(&210_005)
            .map(|it| it.quantity)
            .unwrap_or(0)
    });
    assert_eq!(
        ozone_after_covert, 495,
        "Falcon (-80% role bonus) with CFT V (-50%) consumes 5 Liquid Ozone (50 * 0.2 * 0.5)"
    );

    // Verify jump fatigue accumulation
    let (cd_min, fat_min) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 4.5, 0.0);
    assert!(cd_min > 0.0 && fat_min > 0.0);

    // Verify concurrent cyno activation on the same ship is rejected
    let dup_cyno_mod = 210_009;
    grant_skills(&ctx, PILOT_1, CYNO);
    seed_item(
        &ctx,
        dup_cyno_mod,
        CYNO,
        PILOT_1,
        beacon_ship,
        InventoryFlag::HiSlot1,
        1,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(dup_cyno_mod, true);
    });
    ctx.invalidate_ship_fitting(beacon_ship);
    let dup_err = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(dup_cyno_mod),
            PyRep::String("cynosuralGeneration".into()),
        ],
    )
    .await
    .expect_err("concurrent cyno on same ship must be rejected");
    assert_eq!(dup_err.code, "CannotLightCynoInSystem");

    // Verify Titan Jump Portal generator bridge to standard cyno on a separate ship
    let std_beacon_ship = 210_008;
    let std_cyno_mod = 210_007;
    put_in_space(&ctx, &mut s1, PILOT_1, std_beacon_ship, FALCON, lowsec_sys, DVec3::new(5_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, std_cyno_mod, CYNO, PILOT_1, std_beacon_ship, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, 210_010, 16_273, PILOT_1, std_beacon_ship, InventoryFlag::Cargo, 500);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(std_cyno_mod, true); });
    ctx.invalidate_ship_fitting(std_beacon_ship);
    let std_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno_mod), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Activate standard cyno");
    let beacon_id = kv_get(&std_cyno_res, "beaconID").cloned().unwrap_or(PyRep::Integer(1));
    let beacon_type_id = kv_get(&std_cyno_res, "beaconTypeID").cloned().unwrap_or(PyRep::Integer(21096));

    grant_skills(&ctx, PILOT_2, JUMP_PORTAL);
    seed_item(&ctx, portal_mod, JUMP_PORTAL, PILOT_2, titan_ship, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, 210_006, 16_275, PILOT_2, titan_ship, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(portal_mod, true);
        if let Some(ch) = s.characters.get_mut(&PILOT_2) { ch.jump_cooldown_until_unix = 0; }
    });
    ctx.invalidate_ship_fitting(titan_ship);

    let portal_res = rpc(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64), beacon_id, beacon_type_id])
        .await
        .expect("Activate Titan Jump Portal");
    assert_ne!(portal_res, PyRep::None);

    // Verify bridge_to_fleet_beacon is rejected when the ship already has an active cyno
    let (active_cyno_portal_mod, active_cyno_stront) = (210_011, 210_012);
    seed_item(&ctx, active_cyno_portal_mod, JUMP_PORTAL, PILOT_1, std_beacon_ship, InventoryFlag::HiSlot1, 1);
    seed_item(&ctx, active_cyno_stront, 16_275, PILOT_1, std_beacon_ship, InventoryFlag::Cargo, 1_000);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(active_cyno_portal_mod, true); });
    ctx.invalidate_ship_fitting(std_beacon_ship);
    let _ = ctx.systems.get_or_spawn(lowsec_sys).remove_module_cycle(std_beacon_ship, std_cyno_mod).await;
    let portal_on_cyno_err = rpc(&ctx, &mut s1, "beyonce", "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64)])
        .await
        .expect_err("bridge_to_fleet_beacon must fail when ship has an active cyno");
    assert_eq!(portal_on_cyno_err.code, "CynosuralFieldActive");
    let stront_left = ctx.db.with_snapshot(|s| s.items.get(&active_cyno_stront).map(|it| it.quantity).unwrap_or(0));
    assert_eq!(stront_left, 1_000, "rejected portal must not consume Strontium Clathrates");

    // Advance past cyno expiry, light 0-ozone Covert Cyno on Anathema (-100% fuel bonus), then verify 2-arg jump (beacon_id == 0)
    ctx.advance_time_seconds(700);
    let (covops_ship, covops_cyno_mod) = (210_013, 210_014);
    put_in_space(&ctx, &mut s1, PILOT_1, covops_ship, 11_188, lowsec_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    seed_item(&ctx, covops_cyno_mod, COVERT_CYNO, PILOT_1, covops_ship, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(covops_cyno_mod, true); });
    ctx.invalidate_ship_fitting(covops_ship);
    let covops_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(covops_cyno_mod), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Anathema with 0 Liquid Ozone must light Covert Cyno");
    assert!(kv_get(&covops_cyno_res, "beaconTypeID").is_some());

    ctx.advance_time_seconds(700);
    put_in_space(&ctx, &mut s1, PILOT_1, std_beacon_ship, FALCON, lowsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno_mod), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("Re-activate standard cyno after expiry");
    seed_item(&ctx, 210_015, 16_274, PILOT_2, titan_ship, InventoryFlag::Cargo, 5_000);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&PILOT_2) { ch.jump_cooldown_until_unix = 0; }
    });
    let jump_2arg_res = rpc(&ctx, &mut s2, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64)])
        .await
        .expect("2-arg jump after reship must match live beacon ahead of expired beacons");
    assert_ne!(jump_2arg_res, PyRep::None);
}

#[tokio::test]
async fn test_rnd_research_agents_and_datacore_purchase() {
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1).await;
    sess.station_id = Some(RND_STATION_ID);
    sess.solar_system_id = Some(RND_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = Some(RND_STATION_ID);
            ch.solar_system_id = RND_SYSTEM_ID;
        }
    });

    // Without required science skill 11448, StartResearch (action 12) must fail
    let err = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(12),
            PyRep::Integer(RND_AGENT_ID),
            PyRep::Integer(11_448),
        ],
    )
    .await
    .expect_err("missing research skill must fail");
    assert_eq!(err.code, "ErrorNoRelevantResearchSkills");

    // Grant Science V (3402), Mechanics V (3392), and field skill 11448 at level 3 and start research
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(
            3_402,
            eve_db::TrainedSkillRecord {
                type_id: 3_402,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3_392,
            eve_db::TrainedSkillRecord {
                type_id: 3_392,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            11_448,
            eve_db::TrainedSkillRecord {
                type_id: 11_448,
                level: 3,
                skill_points: 40_000.0,
            },
        );
    });
    let start_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(12),
            PyRep::Integer(RND_AGENT_ID),
            PyRep::Integer(11_448),
        ],
    )
    .await
    .expect("StartResearch");
    assert_eq!(
        kv_get(&start_res, "researchStarted"),
        Some(&PyRep::Bool(true))
    );
    let points_per_day = match kv_get(&start_res, "pointsPerDay") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected pointsPerDay Real, got {other:?}"),
    };
    assert!(points_per_day > 0.0, "pointsPerDay must be positive");

    let isk_before = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0));

    // Advance clock by 6 days to accumulate RP via the real RP formula, then redeem 1 datacore (action 14)
    ctx.advance_secs(86_400 * 6);
    let buy_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(14),
            PyRep::Integer(RND_AGENT_ID),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("BuyDatacores");
    let dc_item_id = match kv_get(&buy_res, "datacoreItemID") {
        Some(PyRep::Integer(id)) => *id,
        other => panic!("expected datacoreItemID Integer, got {other:?}"),
    };
    let dc_type_id = match kv_get(&buy_res, "datacoreTypeID") {
        Some(PyRep::Integer(tid)) => *tid as u32,
        other => panic!("expected datacoreTypeID Integer, got {other:?}"),
    };
    assert!(dc_type_id > 0);
    assert_eq!(kv_get(&buy_res, "quantity"), Some(&PyRep::Integer(1)));
    let rem_rp = match kv_get(&buy_res, "remainingResearchPoints") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected remainingResearchPoints Real, got {other:?}"),
    };
    let expected_rp = points_per_day * 6.0 - 100.0;
    assert!(
        (rem_rp - expected_rp).abs() < 1e-6,
        "expected remaining RP {expected_rp}, got {rem_rp}"
    );
    let isk_after = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0));
    assert_eq!(isk_before - isk_after, 1_000_000, "10,000 ISK fee must be debited");
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&dc_item_id).expect("datacore item in inventory");
        assert_eq!(item.type_id, dc_type_id);
        assert_eq!(item.quantity, 1);
        assert_eq!(item.owner_id, PILOT_1);
        assert_eq!(item.location_id, RND_STATION_ID);
    });
}
