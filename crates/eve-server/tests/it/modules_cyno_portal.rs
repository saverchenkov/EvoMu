//! Cynosural field generators, fleet module beacons, jump portal generators and cyno jammers
//! (`feat:modules.cyno-*`, `feat:modules.jump-portal`). All ship/module/fuel facts are read from
//! the real local SDE (`EVOMU_SDE_PATH`).


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const C3: i64 = 90_000_003;

// SDE type IDs used as test fixtures (looked up in the real SDE by every assertion below).
const FALCON: u32 = 11957;
const BUSTARD: u32 = 12731;
const RHEA: u32 = 28844;
const RORQUAL: u32 = 28352;
const REVELATION: u32 = 19720;
const AVATAR: u32 = 11567;
const NYX: u32 = 23913;
const MARSHAL: u32 = 44996;
const RAVEN: u32 = 638;
const PURIFIER: u32 = 12038;
const CYNO: u32 = 21096;
const COVERT_CYNO: u32 = 28646;
const INDUSTRIAL_CYNO: u32 = 52694;
const JUMP_PORTAL: u32 = 23953;
const COVERT_JUMP_PORTAL: u32 = 28652;
const TENEBREX: u32 = 37534;
const CYNO_JAMMER_BEACON: u32 = 32798;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

fn attr(ctx: &ServerContext, type_id: u32, aid: u16) -> f64 {
    ctx.sde.types[&type_id].default_attributes.get(&aid).copied().unwrap_or(0.0)
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: owner,
                location_id: loc,
                flag,
                quantity: qty,
                singleton: qty == 1,
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

fn qty_of(ctx: &ServerContext, item_id: i64) -> i64 {
    ctx.db.with_snapshot(|s| s.items.get(&item_id).map(|i| i.quantity as i64).unwrap_or(0))
}

fn grant_required_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|(s, _)| ctx.sde.types[&type_id].default_attributes.get(s).map(|&v| v as u32))
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills {
            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
}

/// Logs in a pilot; `C3` is a third account/character cloned from `C2` for multi-pilot fleets.
async fn pilot(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    if char_id == C3 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert(
                "third".into(),
                eve_db::AccountRecord {
                    user_id: 3,
                    username: "third".into(),
                    password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut c = snap.characters[&C2].clone();
            c.character_id = C3;
            c.user_id = 3;
            c.name = "Third Pilot".into();
            snap.characters.insert(C3, c);
            let sk = snap.skills.get(&C2).cloned().unwrap_or_default();
            snap.skills.insert(C3, sk);
        });
    }
    let (user, pass) = match char_id {
        C1 => ("demo", "demo-secret"),
        C2 => ("other", "other-secret"),
        _ => ("third", "third-secret"),
    };
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess
}

/// Puts `char_id` in space in `ship_id` (seeded from SDE type `type_id`) at `pos` in `sys_id`.
async fn board(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    grant_required_skills(ctx, char_id, type_id);
    if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
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
    ctx.systems.get_or_spawn(sys_id).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ship");
}

fn fit(ctx: &ServerContext, char_id: i64, ship_id: i64, item_id: i64, type_id: u32, flag: InventoryFlag) {
    grant_required_skills(ctx, char_id, type_id);
    seed_item(ctx, item_id, type_id, char_id, ship_id, flag, 1);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
}

fn make_fleet(ctx: &ServerContext, fleet_id: i64, members: &[i64]) {
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            fleet_id,
            eve_db::FleetRecord { fleet_id, boss_id: members[0], members: members.to_vec(), ..Default::default() },
        );
    });
}

fn ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
    let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
    ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt()
}

/// `(origin, near, far)`: low/null-sec k-space systems with `near` < 2 LY and `far` > 15 LY from origin.
fn pick_systems(ctx: &ServerContext) -> (u32, u32, u32) {
    let mut sys: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
        .map(|s| s.solar_system_id)
        .collect();
    sys.sort_unstable();
    for &a in &sys {
        let near = sys.iter().copied().find(|&b| b != a && (0.3..2.0).contains(&ly(ctx, a, b)));
        let far = sys.iter().copied().find(|&b| ly(ctx, a, b) > 15.0);
        if let (Some(n), Some(f)) = (near, far) {
            return (a, n, f);
        }
    }
    panic!("no suitable systems in SDE");
}

async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: Vec<PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None).await
}

async fn activate(ctx: &ServerContext, sess: &mut SessionState, module_id: i64, effect: &str) -> Result<PyRep, BridgeRpcError> {
    call(ctx, sess, "dogmaIM", "Activate", vec![PyRep::Integer(module_id), PyRep::String(effect.into())]).await
}

fn int(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

fn real(rep: &PyRep, key: &str) -> f64 {
    rep.dict_get(key).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

fn beacon_args(cyno_char: i64, cyno: &PyRep) -> Vec<PyRep> {
    vec![PyRep::Integer(cyno_char), PyRep::Integer(int(cyno, "solarSystemID")), PyRep::Integer(int(cyno, "beaconID"))]
}

async fn max_velocity(ctx: &ServerContext, sys: u32, ship: i64) -> f64 {
    ctx.systems.get_or_spawn(sys).get_ball_snapshot(ship).await.unwrap().unwrap().1.max_velocity
}

#[tokio::test]
async fn test_cyno_field_beacon_duration_ozone_and_immobilization() {
    // parity: feat:modules.cyno-field
    eve_testkit::attest(&["feat:modules.cyno-field"]);
    let ctx = new_ctx();
    let (a, b, _) = pick_systems(&ctx);
    let mut s1 = pilot(&ctx, C1, 9101).await;
    let mut s2 = pilot(&ctx, C2, 9102).await;
    let mut s3 = pilot(&ctx, C3, 9103).await;
    make_fleet(&ctx, 9_100_001, &[C1, C2, C3]);

    // Falcon (Force Recon) with Cynosural Field Generator I; Cynosural Field Theory V trained.
    board(&ctx, &mut s1, C1, 910_001, FALCON, 30_000_142, DVec3::ZERO).await;
    fit(&ctx, C1, 910_001, 910_002, CYNO, InventoryFlag::HiSlot0);
    let err = activate(&ctx, &mut s1, 910_002, "cynosuralGeneration").await.expect_err("high-sec cyno");
    assert_eq!(err.code, "CannotLightCynoInSystem");

    board(&ctx, &mut s1, C1, 910_001, FALCON, b, DVec3::ZERO).await;
    let err = activate(&ctx, &mut s1, 910_002, "cynosuralGeneration").await.expect_err("no ozone");
    assert_eq!(err.code, "NotEnoughFuel");
    let ozone_type = attr(&ctx, CYNO, 713) as u32;
    assert_eq!(ctx.sde.types[&ozone_type].name, "Liquid Ozone");
    seed_item(&ctx, 910_003, ozone_type, C1, 910_001, InventoryFlag::Cargo, 2_000);
    let cyno = activate(&ctx, &mut s1, 910_002, "cynosuralGeneration").await.expect("light cyno");

    // 5-minute beacon: SDE duration (600 s) x Force Recon role bonus (attribute 66 = -50%).
    let expected_s = attr(&ctx, CYNO, attr_ids::DURATION) * (1.0 + attr(&ctx, FALCON, 66) / 100.0) / 1000.0;
    assert_eq!(expected_s, 300.0);
    assert_eq!(real(&cyno, "durationSeconds"), 300.0);
    assert_eq!(int(&cyno, "expiresAtUnix") - ctx.now_unix_secs() as i64, 300);
    // Liquid Ozone consumed: exactly the reported consumptionQuantity, which is the SDE quantity
    // reduced by the Falcon's role bonus and Cynosural Field Theory V (both
    // `consumptionQuantityBonusPercentage` via effect `cynosuralTheoryConsumptionBonus`).
    let consumed = int(&cyno, "consumptionQuantity");
    assert_eq!(int(&cyno, "consumptionTypeID"), ozone_type as i64);
    assert_eq!(2_000 - qty_of(&ctx, 910_003), consumed);
    let base_qty = attr(&ctx, CYNO, 714);
    let hull_bonus = attr(&ctx, FALCON, 1296);
    let skill_bonus = attr(&ctx, 21603, 1296);
    let expected_qty = base_qty * (1.0 + hull_bonus / 100.0) * (1.0 + 5.0 * skill_bonus / 100.0);
    assert_eq!(consumed as f64, expected_qty.ceil());
    assert!(consumed > 0);
    // Ship fixed in place and the beacon ball is in space with the SDE beacon type.
    assert_eq!(max_velocity(&ctx, b, 910_001).await, 0.0);
    let beacon_id = int(&cyno, "beaconID");
    let beacon_type = int(&cyno, "beaconTypeID") as u32;
    assert_eq!(ctx.sde.types[&beacon_type].name, "Cynosural Field");
    let beacon_ball = ctx.systems.get_or_spawn(b).get_ball_snapshot(beacon_id).await.unwrap().unwrap().1;
    assert_eq!(beacon_ball.type_id, beacon_type);

    // Still live at +290 s: a dreadnought jumps in.
    let rev_fuel = attr(&ctx, REVELATION, 866) as u32;
    board(&ctx, &mut s3, C3, 910_010, REVELATION, a, DVec3::ZERO).await;
    seed_item(&ctx, 910_011, rev_fuel, C3, 910_010, InventoryFlag::Cargo, 100_000);
    ctx.advance_virtual_clock(290);
    let jump = call(&ctx, &mut s3, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("dread jump");
    assert_eq!(s3.solar_system_id, Some(b));
    assert_eq!(int(&jump, "fuelConsumed"), 100_000 - qty_of(&ctx, 910_011));
    assert!(int(&jump, "fuelConsumed") > 0);

    // Collapsed after 5 minutes (+301 s): further jumps are refused.
    let jf_fuel = attr(&ctx, RHEA, 866) as u32;
    board(&ctx, &mut s2, C2, 910_020, RHEA, a, DVec3::ZERO).await;
    seed_item(&ctx, 910_021, jf_fuel, C2, 910_020, InventoryFlag::Cargo, 100_000);
    ctx.advance_virtual_clock(11);
    let err = call(&ctx, &mut s2, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect_err("expired");
    assert_eq!(err.code, "NoActiveBeacon");
    assert_eq!(s2.solar_system_id, Some(a));
}

#[tokio::test]
async fn test_industrial_cyno_only_jump_freighters_and_rorquals() {
    // parity: feat:modules.cyno-industrial
    eve_testkit::attest(&["feat:modules.cyno-industrial"]);
    let ctx = new_ctx();
    let (a, b, _) = pick_systems(&ctx);
    let mut s1 = pilot(&ctx, C1, 9201).await;
    let mut s2 = pilot(&ctx, C2, 9202).await;
    let mut s3 = pilot(&ctx, C3, 9203).await;
    make_fleet(&ctx, 9_200_001, &[C1, C2, C3]);
    let ozone = attr(&ctx, INDUSTRIAL_CYNO, 713) as u32;

    // Hull restrictions come from SDE canFitShipGroup/canFitShipType: a Falcon cannot run an
    // Industrial Cyno, a Deep Space Transport cannot run a standard one.
    board(&ctx, &mut s1, C1, 920_001, FALCON, b, DVec3::ZERO).await;
    fit(&ctx, C1, 920_001, 920_002, INDUSTRIAL_CYNO, InventoryFlag::HiSlot0);
    seed_item(&ctx, 920_003, ozone, C1, 920_001, InventoryFlag::Cargo, 5_000);
    assert_eq!(activate(&ctx, &mut s1, 920_002, "cynosuralGeneration").await.unwrap_err().code, "CannotLightCynoInSystem");
    board(&ctx, &mut s1, C1, 920_010, BUSTARD, b, DVec3::new(50_000.0, 0.0, 0.0)).await;
    fit(&ctx, C1, 920_010, 920_011, CYNO, InventoryFlag::HiSlot0);
    fit(&ctx, C1, 920_010, 920_012, INDUSTRIAL_CYNO, InventoryFlag::HiSlot1);
    seed_item(&ctx, 920_013, ozone, C1, 920_010, InventoryFlag::Cargo, 5_000);
    assert_eq!(activate(&ctx, &mut s1, 920_011, "cynosuralGeneration").await.unwrap_err().code, "CannotLightCynoInSystem");
    let cyno = activate(&ctx, &mut s1, 920_012, "cynosuralGeneration").await.expect("industrial cyno");
    let beacon_type = int(&cyno, "beaconTypeID") as u32;
    assert_eq!(attr(&ctx, beacon_type, 2826), 1.0, "beacon must be the SDE isIndustrialCyno beacon");

    // Dreadnought, Titan and Supercarrier jump drives refuse the industrial beacon.
    for (i, hull) in [REVELATION, AVATAR, NYX].into_iter().enumerate() {
        let ship = 920_100 + i as i64 * 10;
        board(&ctx, &mut s3, C3, ship, hull, a, DVec3::ZERO).await;
        seed_item(&ctx, ship + 1, attr(&ctx, hull, 866) as u32, C3, ship, InventoryFlag::Cargo, 500_000);
        let err = call(&ctx, &mut s3, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect_err("capital");
        assert_eq!(err.code, "ShipCannotJumpToBeacon", "{hull} must not jump to an industrial cyno");
        assert_eq!(qty_of(&ctx, ship + 1), 500_000);
        assert_eq!(s3.solar_system_id, Some(a));
    }

    // Jump Freighter and Rorqual can.
    board(&ctx, &mut s2, C2, 920_200, RHEA, a, DVec3::ZERO).await;
    seed_item(&ctx, 920_201, attr(&ctx, RHEA, 866) as u32, C2, 920_200, InventoryFlag::Cargo, 500_000);
    let jf = call(&ctx, &mut s2, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("JF jump");
    assert_eq!(s2.solar_system_id, Some(b));
    assert_eq!(int(&jf, "fuelConsumed"), 500_000 - qty_of(&ctx, 920_201));
    assert!((real(&jf, "distanceLY") - ly(&ctx, a, b)).abs() < 1e-9);

    board(&ctx, &mut s3, C3, 920_300, RORQUAL, a, DVec3::ZERO).await;
    seed_item(&ctx, 920_301, attr(&ctx, RORQUAL, 866) as u32, C3, 920_300, InventoryFlag::Cargo, 500_000);
    let rq = call(&ctx, &mut s3, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("Rorqual jump");
    assert_eq!(s3.solar_system_id, Some(b));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&920_300].location_id), b as i64);
    assert!(int(&rq, "fuelConsumed") > 0);
}

#[tokio::test]
async fn test_covert_cyno_black_ops_jump_and_covert_portal() {
    // parity: feat:modules.cyno-covert
    eve_testkit::attest(&["feat:modules.cyno-covert"]);
    let ctx = new_ctx();
    let (a, b, _) = pick_systems(&ctx);
    let mut s1 = pilot(&ctx, C1, 9301).await;
    let mut s2 = pilot(&ctx, C2, 9302).await;
    let mut s3 = pilot(&ctx, C3, 9303).await;
    make_fleet(&ctx, 9_300_001, &[C1, C2, C3]);

    board(&ctx, &mut s1, C1, 930_001, FALCON, b, DVec3::ZERO).await;
    fit(&ctx, C1, 930_001, 930_002, COVERT_CYNO, InventoryFlag::HiSlot0);
    seed_item(&ctx, 930_003, attr(&ctx, COVERT_CYNO, 713) as u32, C1, 930_001, InventoryFlag::Cargo, 1_000);
    let cyno = activate(&ctx, &mut s1, 930_002, "cynosuralGeneration").await.expect("covert cyno");
    let beacon_type = int(&cyno, "beaconTypeID") as u32;
    assert_eq!(attr(&ctx, beacon_type, 1252), 1.0, "beacon must be the SDE isCovert beacon");

    // Capital jump drives and Titan portals cannot use a covert cyno.
    board(&ctx, &mut s3, C3, 930_010, REVELATION, a, DVec3::ZERO).await;
    seed_item(&ctx, 930_011, attr(&ctx, REVELATION, 866) as u32, C3, 930_010, InventoryFlag::Cargo, 500_000);
    let err = call(&ctx, &mut s3, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.unwrap_err();
    assert_eq!(err.code, "ShipCannotJumpToBeacon");
    board(&ctx, &mut s3, C3, 930_020, AVATAR, a, DVec3::ZERO).await;
    fit(&ctx, C3, 930_020, 930_021, JUMP_PORTAL, InventoryFlag::HiSlot0);
    seed_item(&ctx, 930_022, attr(&ctx, JUMP_PORTAL, 713) as u32, C3, 930_020, InventoryFlag::Cargo, 5_000);
    let err = call(&ctx, &mut s3, "beyonce", "CmdBridgeToFleetModuleBeacon", beacon_args(C1, &cyno)).await.unwrap_err();
    assert_eq!(err.code, "ShipCannotBridgeToBeacon");
    assert_eq!(qty_of(&ctx, 930_022), 5_000);

    // Black Ops covert jump portal bridges to it.
    let bo_fuel = attr(&ctx, MARSHAL, 866) as u32;
    board(&ctx, &mut s2, C2, 930_030, MARSHAL, a, DVec3::ZERO).await;
    fit(&ctx, C2, 930_030, 930_031, COVERT_JUMP_PORTAL, InventoryFlag::HiSlot0);
    seed_item(&ctx, 930_032, bo_fuel, C2, 930_030, InventoryFlag::Cargo, 500_000);
    let bridge = call(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("BO bridge");
    assert_eq!(int(&bridge, "beaconID"), int(&cyno, "beaconID"));

    // Covert portal passengers need the SDE isBlackOpsJumpPortalPassenger flag.
    board(&ctx, &mut s3, C3, 930_040, RAVEN, a, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let through = vec![PyRep::Integer(C2), PyRep::Integer(930_030)];
    let err = call(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", through.clone()).await.unwrap_err();
    assert_eq!(err.code, "JumpBridgeRestrictedShip");
    board(&ctx, &mut s3, C3, 930_050, PURIFIER, a, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let jt = call(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", through).await.expect("bomber through covert portal");
    assert_eq!(s3.solar_system_id, Some(b));
    assert_eq!(int(&jt, "fuelConsumed"), 500_000 - qty_of(&ctx, 930_032));

    // A Black Ops jump drive can jump to the covert cyno itself.
    let before = qty_of(&ctx, 930_032);
    let bo_jump = call(&ctx, &mut s2, "beyonce", "CmdJumpToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("BO jump");
    assert_eq!(s2.solar_system_id, Some(b));
    assert_eq!(int(&bo_jump, "fuelConsumed"), before - qty_of(&ctx, 930_032));
}

#[tokio::test]
async fn test_titan_jump_portal_fleet_bridge_fuel_and_range() {
    // parity: feat:modules.jump-portal
    eve_testkit::attest(&["feat:modules.jump-portal"]);
    let ctx = new_ctx();
    let (a, b, far) = pick_systems(&ctx);
    let mut s1 = pilot(&ctx, C1, 9401).await;
    let mut s2 = pilot(&ctx, C2, 9402).await;
    let mut s3 = pilot(&ctx, C3, 9403).await;
    make_fleet(&ctx, 9_400_001, &[C1, C2, C3]);
    let ozone = attr(&ctx, CYNO, 713) as u32;
    let stront = attr(&ctx, JUMP_PORTAL, 713) as u32;
    let titan_fuel = attr(&ctx, AVATAR, 866) as u32;

    board(&ctx, &mut s2, C2, 940_010, AVATAR, a, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(C2).or_default().remove(&21610); // Jump Fuel Conservation untrained
    });
    fit(&ctx, C2, 940_010, 940_011, JUMP_PORTAL, InventoryFlag::HiSlot0);
    seed_item(&ctx, 940_012, stront, C2, 940_010, InventoryFlag::Cargo, 5_000);
    seed_item(&ctx, 940_013, titan_fuel, C2, 940_010, InventoryFlag::Cargo, 1_000_000);

    // Range: a cyno > 15 LY away is beyond the Titan's jumpDriveRange.
    board(&ctx, &mut s1, C1, 940_001, FALCON, far, DVec3::ZERO).await;
    fit(&ctx, C1, 940_001, 940_002, CYNO, InventoryFlag::HiSlot0);
    seed_item(&ctx, 940_003, ozone, C1, 940_001, InventoryFlag::Cargo, 5_000);
    let far_cyno = activate(&ctx, &mut s1, 940_002, "cynosuralGeneration").await.expect("far cyno");
    let err = call(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", beacon_args(C1, &far_cyno)).await.unwrap_err();
    assert_eq!(err.code, "JumpDistanceOutOfRange");
    assert_eq!(qty_of(&ctx, 940_012), 5_000);

    // Relight within range once the first field collapses.
    ctx.advance_virtual_clock(301);
    board(&ctx, &mut s1, C1, 940_001, FALCON, b, DVec3::ZERO).await;
    let cyno = activate(&ctx, &mut s1, 940_002, "cynosuralGeneration").await.expect("near cyno");

    // Passenger outside the fleet is refused.
    board(&ctx, &mut s3, C3, 940_020, RAVEN, a, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let bridge = call(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", beacon_args(C1, &cyno)).await.expect("bridge");
    assert_eq!(int(&bridge, "activationConsumptionTypeID"), stront as i64);
    assert_eq!(int(&bridge, "activationConsumptionQuantity"), 5_000 - qty_of(&ctx, 940_012));
    assert!(int(&bridge, "activationConsumptionQuantity") > 0);
    assert!((real(&bridge, "distanceLY") - ly(&ctx, a, b)).abs() < 1e-9);
    assert_eq!(real(&bridge, "durationSeconds"), attr(&ctx, JUMP_PORTAL, 1002) / 1000.0);
    assert_eq!(max_velocity(&ctx, a, 940_010).await, 0.0, "titan is fixed while the portal is open");

    let through = vec![PyRep::Integer(C2), PyRep::Integer(940_010)];
    make_fleet(&ctx, 9_400_001, &[C1, C2]);
    assert_eq!(call(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", through.clone()).await.unwrap_err().code, "FleetNotInFleet");
    make_fleet(&ctx, 9_400_001, &[C1, C2, C3]);

    // Capital passengers are excluded by the SDE isTitanJumpPortalPassenger flag.
    board(&ctx, &mut s3, C3, 940_030, REVELATION, a, DVec3::new(2_000.0, 0.0, 0.0)).await;
    assert_eq!(call(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", through.clone()).await.unwrap_err().code, "JumpBridgeRestrictedShip");

    // A battleship is bridged; the Titan pays isotopes scaled by passenger mass and distance.
    board(&ctx, &mut s3, C3, 940_020, RAVEN, a, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let jt = call(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", through).await.expect("bridge passenger");
    let fuel = int(&jt, "fuelConsumed");
    assert_eq!(int(&jt, "fuelTypeID"), titan_fuel as i64);
    assert_eq!(1_000_000 - qty_of(&ctx, 940_013), fuel);
    let raven_mass = ctx.sde.types[&RAVEN].mass;
    let factor = attr(&ctx, JUMP_PORTAL, 1001);
    // DERIVED formula (see fitting_cyno_portal.rs): Titan jumpDriveConsumptionAmount x
    // jumpPortalConsumptionMassFactor x passenger mass x LY (no Jump Fuel Conservation trained).
    let expected = (attr(&ctx, AVATAR, 868) * factor * raven_mass * ly(&ctx, a, b)).ceil() as i64;
    assert_eq!(fuel, expected, "fuel must scale with passenger mass x distance");
    assert!(fuel > 0);
    assert_eq!(s3.solar_system_id, Some(b));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&940_020].location_id), b as i64);
    assert!(ctx.systems.get_or_spawn(b).get_ball_snapshot(940_020).await.unwrap().is_some());
}

#[tokio::test]
async fn test_cyno_jammer_blocks_non_covert_cynos_and_reports_state() {
    // parity: feat:modules.cyno-jammer
    eve_testkit::attest(&["feat:modules.cyno-jammer"]);
    let ctx = new_ctx();
    let (_, b, _) = pick_systems(&ctx);
    let mut s1 = pilot(&ctx, C1, 9501).await;
    board(&ctx, &mut s1, C1, 950_001, FALCON, b, DVec3::ZERO).await;
    fit(&ctx, C1, 950_001, 950_002, CYNO, InventoryFlag::HiSlot0);
    fit(&ctx, C1, 950_001, 950_003, COVERT_CYNO, InventoryFlag::HiSlot1);
    seed_item(&ctx, 950_004, attr(&ctx, CYNO, 713) as u32, C1, 950_001, InventoryFlag::Cargo, 10_000);

    let state = call(&ctx, &mut s1, "beyonce", "GetCynoJammerState", vec![]).await.expect("state");
    assert_eq!(state, PyRep::None);

    // Online, fuelled Tenebrex Cyno Jammer (its Standup Cynosural System Jammer service module
    // carries cynoJammerActivationDelay and canFitShipType1 = Tenebrex in the SDE).
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            950_100,
            eve_db::UpwellStructureRecord {
                structure_id: 950_100,
                type_id: TENEBREX,
                owner_corp_id: 1_000_001,
                solar_system_id: b,
                name: "Jammer".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
    let state = call(&ctx, &mut s1, "beyonce", "GetCynoJammerState", vec![]).await.expect("state");
    assert_eq!(state.as_i64(), Some(ctx.now_blueos_filetime_ticks()));
    let err = activate(&ctx, &mut s1, 950_002, "cynosuralGeneration").await.unwrap_err();
    assert_eq!(err.code, "CynoJammed");
    assert_eq!(qty_of(&ctx, 950_004), 10_000, "no ozone spent on a jammed cyno");
    // Covert cynos are exempt.
    let covert = activate(&ctx, &mut s1, 950_003, "cynosuralGeneration").await.expect("covert ignores jammer");
    assert_eq!(attr(&ctx, int(&covert, "beaconTypeID") as u32, 1252), 1.0);

    // Jammer destroyed -> state clears; an in-space isCynoJammer beacon jams again.
    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&950_100).unwrap().state = "Destroyed".into());
    assert_eq!(call(&ctx, &mut s1, "beyonce", "GetCynoJammerState", vec![]).await.unwrap(), PyRep::None);
    seed_item(&ctx, 950_200, CYNO_JAMMER_BEACON, 1_000_001, b as i64, InventoryFlag::None, 1);
    assert!(call(&ctx, &mut s1, "beyonce", "GetCynoJammerState", vec![]).await.unwrap().as_i64().is_some());
    assert_eq!(activate(&ctx, &mut s1, 950_002, "cynosuralGeneration").await.unwrap_err().code, "CynoJammed");
    ctx.db.with_mut_snapshot(|s| s.items.remove(&950_200));
    ctx.advance_virtual_clock(120);
    let lit = activate(&ctx, &mut s1, 950_002, "cynosuralGeneration").await.expect("unjammed cyno");
    assert_eq!(attr(&ctx, int(&lit, "beaconTypeID") as u32, 1252), 0.0);
}
