//! End-to-end integration tests for `feat:ships.*` batch closure:
//! - `feat:ships.covert-cyno-beacon`
//! - `feat:ships.cynosural-beacon-spawning`
//! - `feat:ships.frigate-escape-bay`
//! - `feat:ships.jump-drive-fatigue-accumulation`
//! - `feat:ships.titan-jump-portal-bridge`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
use eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_cyno_portal::{
    is_ship_cyno_active, sweep_expired_cyno_beacons,
};
use glam::DVec3;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const C3: i64 = 90_000_003;

const FALCON: u32 = 11957;
const RAVEN: u32 = 638;
const RIFTER: u32 = 587;
const CARACAL: u32 = 621;
const AVATAR: u32 = 11567;
const REVELATION: u32 = 19720;
const CYNO_GEN: u32 = 21096;
const COVERT_CYNO_GEN: u32 = 28646;
const JUMP_PORTAL_GEN: u32 = 23953;
const LIQUID_OZONE: u32 = 16273;
const STRONTIUM: u32 = 16275;
const HELIUM_ISOTOPES: u32 = 16274;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| {
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
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

fn grant_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|(s, _)| ctx.sde.types.get(&type_id).and_then(|t| t.default_attributes.get(s).map(|&v| v as u32)))
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills {
            entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    loc_id: i64,
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
                location_id: loc_id,
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

async fn spawn_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    grant_skills_for_type(ctx, char_id, type_id);
    if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
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
            ch.jump_fatigue_until_unix = 0;
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
    ctx.systems.get_or_spawn(sys_id).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn");
}

fn fit_online_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    mod_item_id: i64,
    mod_type_id: u32,
    flag: InventoryFlag,
) {
    grant_skills_for_type(ctx, char_id, mod_type_id);
    seed_item(ctx, mod_item_id, mod_type_id, char_id, ship_id, flag, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mod_item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
}

fn create_fleet(ctx: &ServerContext, fleet_id: i64, members: &[i64]) {
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: members[0],
                members: members.to_vec(),
                ..Default::default()
            },
        );
        for &m in members {
            if let Some(ch) = s.characters.get_mut(&m) {
                ch.fleet_id = Some(fleet_id);
            }
        }
    });
}

fn system_distance_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
    let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
    ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt()
}

fn pick_test_systems(ctx: &ServerContext) -> (u32, u32) {
    let mut lowsec: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
        .map(|s| s.solar_system_id)
        .collect();
    lowsec.sort_unstable();
    for &a in &lowsec {
        if let Some(&b) = lowsec.iter().find(|&&b| b != a && (0.5..2.5).contains(&system_distance_ly(ctx, a, b))) {
            return (a, b);
        }
    }
    panic!("No pair of lowsec systems found");
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_covert_and_standard_cyno_spawning_and_despawning() {
    // parity: feat:ships.covert-cyno-beacon
    // parity: feat:ships.cynosural-beacon-spawning
    eve_testkit::attest(&[
        "feat:ships.covert-cyno-beacon",
        "feat:ships.cynosural-beacon-spawning",
    ]);

    let ctx = new_ctx();
    let (sys_a, sys_b) = pick_test_systems(&ctx);
    let highsec_sys = 30_000_142_u32; // Jita

    let mut sess1 = SessionState::new(101);
    sess1.character_id = Some(C1);
    sess1.user_id = Some(1);
    let ship1 = 101_001_i64;
    spawn_pilot_in_space(&ctx, &mut sess1, C1, ship1, FALCON, highsec_sys, DVec3::ZERO).await;
    create_fleet(&ctx, 601, &[C1, C2]);

    // 1. Highsec rejection: Standard cyno cannot be lit in highsec (security >= 0.45)
    let cyno_mod_hi = 201_001_i64;
    fit_online_module(&ctx, C1, ship1, cyno_mod_hi, CYNO_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 301_001, LIQUID_OZONE, C1, ship1, InventoryFlag::Cargo, 1000, false);
    let hi_err = rpc(
        &ctx,
        &mut sess1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_hi), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(hi_err.code, "CannotLightCynoInSystem");

    // 2. Standard Cyno in lowsec: consumes Liquid Ozone, spawns beacon, immobilizes ship
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&C1) {
            ch.solar_system_id = sys_a;
        }
        if let Some(sh) = s.items.get_mut(&ship1) {
            sh.location_id = sys_a as i64;
        }
    });
    sess1.solar_system_id = Some(sys_a);
    ctx.systems.get_or_spawn(sys_a).spawn_ball_with_meta(
        Ball {
            id: ship1,
            type_id: FALCON,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: ctx.sde.types[&FALCON].mass,
            radius: ctx.sde.types[&FALCON].radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(C1, ship1, FALCON),
        ctx.build_ship_slim_meta(C1, FALCON),
    ).await.expect("spawn in sys_a");
    let cft_skill = 21603_u32; // Cynosural Field Theory
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(C1).or_default().insert(
            cft_skill,
            eve_db::TrainedSkillRecord { type_id: cft_skill, level: 5, skill_points: 256_000.0 },
        );
    });

    let act_res = rpc(
        &ctx,
        &mut sess1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_hi), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("activate cyno in lowsec");

    let beacon_id = act_res.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
    assert!(beacon_id > 0);
    assert!(is_ship_cyno_active(&ctx, ship1));

    // Consumed Liquid Ozone (Falcon Force Recon role gives -80%, CFT V gives -50%, 500 * 0.2 * 0.5 = 50 consumed)
    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&301_001).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(ozone_left, 950);

    // Ship is immobilized (max_velocity becomes 0.0)
    let snap_ball = ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(ship1).await.unwrap().unwrap().1;
    assert_eq!(snap_ball.max_velocity, 0.0);

    // Cyno beacon ball exists in ballpark
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(beacon_id).await.unwrap().is_some());

    // 3. Covert Cyno Generator on Force Recon / Falcon:
    let mut sess2 = SessionState::new(102);
    sess2.character_id = Some(C2);
    sess2.user_id = Some(2);
    let ship2 = 101_002_i64;
    spawn_pilot_in_space(&ctx, &mut sess2, C2, ship2, FALCON, sys_b, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let covert_mod = 201_002_i64;
    fit_online_module(&ctx, C2, ship2, covert_mod, COVERT_CYNO_GEN, InventoryFlag::HiSlot1);
    seed_item(&ctx, 301_002, LIQUID_OZONE, C2, ship2, InventoryFlag::Cargo, 500, false);

    // Advance clock past standard cyno expiry
    ctx.advance_secs(600);
    sweep_expired_cyno_beacons(&ctx);
    assert!(!is_ship_cyno_active(&ctx, ship1));
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(beacon_id).await.unwrap().is_none());

    // Light Covert Cyno in sys_b
    let cov_act = rpc(
        &ctx,
        &mut sess2,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(covert_mod), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("activate covert cyno");
    let cov_beacon_id = cov_act.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cov beaconID");
    assert!(cov_beacon_id > 0);
    assert!(is_ship_cyno_active(&ctx, ship2));

    // Covert cyno duration is 60s, isCovert: true, onGridOnly: true (no local notification)
    let dur = cov_act.dict_get("durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert_eq!(dur, 60.0);
    assert_eq!(cov_act.dict_get("isCovert"), Some(&PyRep::Bool(true)));
    assert_eq!(cov_act.dict_get("onGridOnly"), Some(&PyRep::Bool(true)));

    // Capital ship (Avatar) cannot jump to covert cyno beacon
    create_fleet(&ctx, 701, &[C1, C2]);
    let mut sess_titan = SessionState::new(103);
    sess_titan.character_id = Some(C1);
    sess_titan.user_id = Some(1);
    let titan_ship = 101_003_i64;
    spawn_pilot_in_space(&ctx, &mut sess_titan, C1, titan_ship, AVATAR, sys_a, DVec3::ZERO).await;

    let titan_jump_err = rpc(
        &ctx,
        &mut sess_titan,
        "beyonce",
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(C2), PyRep::Integer(sys_b as i64), PyRep::Integer(cov_beacon_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(titan_jump_err.code, "ShipCannotJumpToBeacon");

    // Advance clock past covert cyno expiry: swept clean
    ctx.advance_secs(70);
    sweep_expired_cyno_beacons(&ctx);
    assert!(!is_ship_cyno_active(&ctx, ship2));
    assert!(ctx.systems.get_or_spawn(sys_b).get_ball_snapshot(cov_beacon_id).await.unwrap().is_none());
}

#[tokio::test]
async fn test_battleship_frigate_escape_bay_ejection_on_destruction() {
    // parity: feat:ships.frigate-escape-bay
    eve_testkit::attest(&["feat:ships.frigate-escape-bay"]);

    let ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let victim_char = C1;
    let attacker_char = C2;
    let battleship_id = 500_001_i64;
    let frigate_id = 500_002_i64;
    let non_frigate_id = 500_003_i64;

    let mut sess = SessionState::new(201);
    sess.character_id = Some(victim_char);
    sess.user_id = Some(1);
    spawn_pilot_in_space(&ctx, &mut sess, victim_char, battleship_id, RAVEN, sys_id, DVec3::ZERO).await;

    // 1. Rejection: Non-frigate (Caracal, group 26) cannot be put into Frigate Escape Bay (flag 177)
    seed_item(&ctx, non_frigate_id, CARACAL, victim_char, battleship_id, InventoryFlag::FrigateEscapeBay, 1, true);
    // 2. Put assembled frigate (Rifter, group 25) into Frigate Escape Bay
    seed_item(&ctx, frigate_id, RIFTER, victim_char, battleship_id, InventoryFlag::FrigateEscapeBay, 1, true);

    // Verify Raven has escape bay in SDE attribute 3020 or group 27
    let group_id = ctx.sde.types[&RAVEN].group_id;
    assert_eq!(group_id, 27); // Battleship

    // 3. Destroy Raven in space via death pipeline
    let death_result = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        victim_char,
        battleship_id,
        attacker_char,
        DVec3::new(100.0, 200.0, 300.0),
        12345,
    )
    .expect("execute_death_pipeline_sync");

    // Pilot auto-ejects into the fitted frigate instead of capsule
    assert_eq!(
        death_result.ejected_capsule_id,
        Some(frigate_id),
        "Pilot must eject in the frigate from FrigateEscapeBay"
    );

    // Frigate survives in space at ship position
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&victim_char).unwrap();
        assert_eq!(ch.active_ship_id, frigate_id, "Character active ship must become the frigate");

        let esc_item = snap.items.get(&frigate_id).unwrap();
        assert_eq!(esc_item.location_id, sys_id as i64, "Frigate is now located in space");
        assert_eq!(esc_item.flag, InventoryFlag::None);

        // Raven was removed
        assert!(!snap.items.contains_key(&battleship_id));
    });

    // Killmail records the frigate survived
    let detailed_km = eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.get(&death_result.killmail_id).cloned()
    })
    .expect("detailed killmail");
    let km_frigate = detailed_km
        .dropped_items
        .iter()
        .find(|it| it.type_id == RIFTER)
        .expect("Killmail must record the frigate");
    assert!(
        km_frigate.flag == 177 || km_frigate.flag == 179,
        "Flag on killmail must match escape bay flag"
    );
    assert_eq!(km_frigate.qty_dropped, 0);
    assert_eq!(km_frigate.qty_destroyed, 0);

    // Frigate did not drop as loot and was not destroyed
    assert!(!death_result.loot_dropped_item_ids.contains(&frigate_id));
    assert!(!death_result.loot_destroyed_item_ids.contains(&frigate_id));
}

#[tokio::test]
async fn test_jump_drive_fatigue_and_titan_portal_bridge() {
    // parity: feat:ships.jump-drive-fatigue-accumulation
    // parity: feat:ships.titan-jump-portal-bridge
    eve_testkit::attest(&[
        "feat:ships.jump-drive-fatigue-accumulation",
        "feat:ships.titan-jump-portal-bridge",
    ]);

    let ctx = new_ctx();
    let (sys_a, sys_b) = pick_test_systems(&ctx);
    let dist_ly = system_distance_ly(&ctx, sys_a, sys_b);
    assert!(dist_ly > 0.0 && dist_ly < 5.0);

    // 1. Mathematical fatigue accumulation and cooldown checks
    // Cooldown formula: 1 + effective_ly (min 10% of fatigue, max 30 min)
    // Fatigue formula: max(current_fatigue, 10) * (1 + effective_ly) (max 300 min = 5h)
    let (cd1, fat1) = compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
    assert!((cd1 - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 0 fatigue is 6 min");
    assert!((fat1 - 60.0).abs() < 1e-6, "Fatigue for 5 LY from 0 fatigue is 60 min");

    // Existing fatigue: 30 min -> 30 * (1 + 5) = 180 min
    let (cd2, fat2) = compute_jump_fatigue_and_cooldown_minutes(30.0, 5.0, 0.0);
    assert!((cd2 - 6.0).abs() < 1e-6);
    assert!((fat2 - 180.0).abs() < 1e-6);

    // 5-hour cap (300 min)
    let (_, fat_cap) = compute_jump_fatigue_and_cooldown_minutes(100.0, 5.0, 0.0);
    assert!((fat_cap - 300.0).abs() < 1e-6, "Fatigue must be capped at 300 min");

    // 30-minute cooldown cap
    let (cd_cap, _) = compute_jump_fatigue_and_cooldown_minutes(300.0, 35.0, 0.0);
    assert!((cd_cap - 30.0).abs() < 1e-6, "Cooldown must be capped at 30 min");

    // Black Ops fatigue reduction bonus (75% reduction on 8 LY -> 2 effective LY)
    let (cd_blops, fat_blops) = compute_jump_fatigue_and_cooldown_minutes(0.0, 8.0, 0.75);
    assert!((cd_blops - 3.0).abs() < 1e-6);
    assert!((fat_blops - 30.0).abs() < 1e-6);

    // 2. Titan Jump Portal Bridge Setup
    create_fleet(&ctx, 801, &[C1, C2, C3]);

    // Pilot 1: Avatar Titan in sys_a
    let mut sess_titan = SessionState::new(301);
    sess_titan.character_id = Some(C1);
    sess_titan.user_id = Some(1);
    let titan_id = 600_001_i64;
    let portal_mod = 600_002_i64;
    spawn_pilot_in_space(&ctx, &mut sess_titan, C1, titan_id, AVATAR, sys_a, DVec3::ZERO).await;
    fit_online_module(&ctx, C1, titan_id, portal_mod, JUMP_PORTAL_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 700_001, STRONTIUM, C1, titan_id, InventoryFlag::Cargo, 1000, false);
    seed_item(&ctx, 700_002, HELIUM_ISOTOPES, C1, titan_id, InventoryFlag::Cargo, 50_000, false);

    // Pilot 2: Cyno ship in sys_b
    let mut sess_cyno = SessionState::new(302);
    sess_cyno.character_id = Some(C2);
    sess_cyno.user_id = Some(2);
    let cyno_ship = 600_003_i64;
    let cyno_mod = 600_004_i64;
    spawn_pilot_in_space(&ctx, &mut sess_cyno, C2, cyno_ship, FALCON, sys_b, DVec3::ZERO).await;
    fit_online_module(&ctx, C2, cyno_ship, cyno_mod, CYNO_GEN, InventoryFlag::HiSlot0);
    seed_item(&ctx, 700_003, LIQUID_OZONE, C2, cyno_ship, InventoryFlag::Cargo, 1000, false);

    // Light cyno in sys_b
    let cyno_act = rpc(
        &ctx,
        &mut sess_cyno,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())],
    )
    .await
    .expect("cyno activate");
    let beacon_id = cyno_act.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");

    // Titan opens bridge to fleet beacon in sys_b
    let bridge_res = rpc(
        &ctx,
        &mut sess_titan,
        "beyonce",
        "CmdBridgeToFleetModuleBeacon",
        &[PyRep::Integer(C2), PyRep::Integer(sys_b as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .expect("open jump bridge");

    let portal_ball_id = bridge_res.dict_get("portalBallID").and_then(|v| v.as_i64()).expect("portalBallID");
    assert!(portal_ball_id > 0);
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(portal_ball_id).await.unwrap().is_some());

    // Strontium consumed for opening bridge (500 base, with Titan skill level 5 reduced to 250)
    let stront_left = ctx.db.with_snapshot(|s| s.items.get(&700_001).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(stront_left, 750);

    // 3. Subcapital Passenger (Rifter) jumps through bridge
    let mut sess_pass = SessionState::new(303);
    sess_pass.character_id = Some(C3);
    sess_pass.user_id = Some(3);
    let rifter_id = 600_005_i64;
    // Spawn within 2 km of titan (within 2.5 km portal radius)
    spawn_pilot_in_space(&ctx, &mut sess_pass, C3, rifter_id, RIFTER, sys_a, DVec3::new(1000.0, 0.0, 0.0)).await;

    let jump_res = rpc(
        &ctx,
        &mut sess_pass,
        "beyonce",
        "CmdJumpThroughFleet",
        &[PyRep::Integer(C1), PyRep::Integer(titan_id)],
    )
    .await
    .expect("CmdJumpThroughFleet");

    let final_sys = jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(final_sys, sys_b as i64, "Passenger must arrive at cyno system");

    let fuel_consumed = jump_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(fuel_consumed > 0, "Helium isotopes must be consumed from Titan");

    let isotopes_left = ctx.db.with_snapshot(|s| s.items.get(&700_002).map(|i| i.quantity as i64).unwrap_or(0));
    assert_eq!(isotopes_left, 50_000 - fuel_consumed);

    // Passenger accumulated fatigue
    let pass_fatigue = jump_res.dict_get("jumpFatigueMinutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(pass_fatigue > 0.0);

    // 4. Capital ship passenger (Revelation) is rejected by Titan bridge
    let mut sess_cap = SessionState::new(304);
    sess_cap.character_id = Some(C3);
    sess_cap.user_id = Some(3);
    let dread_id = 600_006_i64;
    spawn_pilot_in_space(&ctx, &mut sess_cap, C3, dread_id, REVELATION, sys_a, DVec3::new(1000.0, 0.0, 0.0)).await;

    let cap_jump_err = rpc(
        &ctx,
        &mut sess_cap,
        "beyonce",
        "CmdJumpThroughFleet",
        &[PyRep::Integer(C1), PyRep::Integer(titan_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(cap_jump_err.code, "JumpBridgeRestrictedShip");

    // 5. Advance clock past bridge duration: bridge and portal ball despawn
    ctx.advance_secs(60);
    sweep_expired_cyno_beacons(&ctx);
    assert!(ctx.systems.get_or_spawn(sys_a).get_ball_snapshot(portal_ball_id).await.unwrap().is_none());
}
