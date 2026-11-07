//! Integration tests for Zarzakh & Deathless Shipcaster `feat:navigation.*` rows:
//! - `feat:navigation.deathless-shipcaster-zarzakh`
//! - `feat:navigation.zarzakh-emanation-lock`
//! - `feat:navigation.zarzakh-fulcrum-gates`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{
    set_zarzakh_horizon_siege_active, ClusterMiscRuntimeState,
};

use crate::common;
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;

pub(crate) const ZARZAKH_SYS: u32 = 30_100_000;
pub(crate) const FULCRUM_STATION_ID: i64 = 60_015_187;
pub(crate) const JITA_STATION_ID: i64 = 60_003_760;
pub(crate) const TURNUR_SYS: u32 = 30_002_086;
pub(crate) const VARD_SYS: u32 = 30_002_538;
pub(crate) const ALSAVOINON_SYS: u32 = 30_003_841;
pub(crate) const HIKKOKEN_SYS: u32 = 30_045_338;

pub(crate) const TURNUR_GATE_ID: i64 = 50_016_562;
pub(crate) const ALSAVOINON_GATE_ID: i64 = 50_016_563;
pub(crate) const ZARZAKH_TURNUR_GATE_ID: i64 = 50_016_566;
pub(crate) const ZARZAKH_ALSAVOINON_GATE_ID: i64 = 50_016_567;
pub(crate) const ZARZAKH_PAD_ID: i64 = 104_000_030;

const ERR_EMPIRE_MILITIA_REFUSED: &str = "EmpireMilitiaRefused";
const ERR_CAP_SHIPCASTER: &str = "CapitalShipCannotJumpShipcaster";
const ERR_OUT_OF_RANGE: &str = "OutsideOperationalRange";
pub(crate) const ERR_HORIZON_SIEGE_CASTER: &str = "HorizonSiegeActive";
const ERR_GATE_LOCKED: &str = "ZarzakhGateLocked";
const ERR_CAP_STARGATE: &str = "ShipCannotUseStargate";
const ERR_NO_DSCAN: &str = "CannotDscanInThisSystem";
const ERR_DISALLOWED_HAZARD_MOD: &str = "ModuleDisallowedInHazardSystem";
pub(crate) const ERR_ANCHORING_ZARZAKH: &str = "AnchoringDisallowedInZarzakh";
const ERR_FULCRUM_EMPIRE: &str = "FulcrumRefusesEmpireMilitia";

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None).await
}

pub(crate) async fn gate_jump(
    ctx: &ServerContext,
    sess: &mut SessionState,
    from_gate_id: i64,
    to_gate_id: i64,
    ship_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    rpc(
        ctx,
        sess,
        "beyonce",
        "CmdStargateJump",
        &[PyRep::Integer(from_gate_id), PyRep::Integer(to_gate_id), PyRep::Integer(ship_id)],
    )
    .await
}

pub(crate) async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    war_faction_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    if char_id != 90_000_001 {
        ctx.db.with_mut_snapshot(|s| {
            if !s.characters.contains_key(&char_id) {
                let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
                ch.character_id = char_id;
                ch.user_id = 2;
                ch.name = format!("Zarzakh Pilot {char_id}");
                s.characters.insert(char_id, ch);
            }
        });
        common::login_pilot(ctx, &mut sess, "other", "other-secret").await;
    } else {
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    }
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(11);

    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let mass = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.mass)
        .filter(|&m| m > 0.0)
        .unwrap_or(1_200_000.0);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = war_faction_id;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys
        .spawn_ball_with_meta(
            ball,
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

#[tokio::test]
async fn test_zarzakh_deathless_shipcaster_entry_beacons() {
    // parity: feat:navigation.deathless-shipcaster-zarzakh
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let beacon_pos = DVec3::new(250_000.0, 0.0, 0.0);

    // 0. Spawning Turnur, Vard, Alsavoinon, Hikkoken, and Zarzakh populates Deathless Shipcaster balls + SlimItemMeta,
    //    and `ClusterMiscRuntimeState::from_db` merges canonical Deathless beacons/pads onto pre-existing DB blobs
    for (sys_id, item_id) in [
        (TURNUR_SYS, 105_000_011_i64),
        (VARD_SYS, 105_000_012_i64),
        (ALSAVOINON_SYS, 105_000_013_i64),
        (HIKKOKEN_SYS, 105_000_014_i64),
        (ZARZAKH_SYS, 105_000_010_i64),
        (ZARZAKH_SYS, ZARZAKH_PAD_ID),
    ] {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let bp = sys_h.ballpark.read().unwrap();
        assert_eq!(bp.balls.get(&item_id).map(|b| b.type_id), Some(79_018));
        assert_eq!(bp.ball_slim_meta.get(&item_id).map(|m| m.category_id), Some(65));
    }
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert(
            "cluster_misc_state".into(),
            r#"{"landing_pads":[{"item_id":104000001,"type_id":75929,"solar_system_id":30002811,"faction_id":500001,"is_built":true,"link_timestamp":1700000000,"is_disrupted":false,"position":[100000.0,0.0,0.0]}],"launchers":[{"item_id":105000001,"type_id":76466,"solar_system_id":30045324,"faction_id":500001,"linked_pad_ids":[104000001],"position":[200000.0,0.0,0.0]}]}"#.into(),
        );
    });
    let merged = ClusterMiscRuntimeState::from_db(&ctx.db);
    assert!(merged.landing_pads.iter().any(|p| p.item_id == ZARZAKH_PAD_ID));
    for bid in 105_000_010_i64..=105_000_014 {
        assert!(merged.launchers.iter().any(|l| l.item_id == bid));
    }

    // 1. All 4 Empire militia factions (500_001..=500_004) are refused at Deathless Shipcaster beacons
    for emp_faction in 500_001_i64..=500_004 {
        let mut sess = setup_space_pilot(
            &ctx,
            884_100 + emp_faction as u64,
            char_id,
            ship_id,
            587,
            TURNUR_SYS,
            beacon_pos,
            Some(emp_faction),
        )
        .await;
        let emp_err = rpc(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_011), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
        )
        .await
        .expect_err("Empire militia must be refused by Deathless Shipcaster");
        assert_eq!(emp_err.code, ERR_EMPIRE_MILITIA_REFUSED);
    }

    // 2. Capital / Freighter ships (Chimera 23915, Charon 20185) are refused
    for (idx, cap_tid) in [23_915_u32, 20_185_u32].into_iter().enumerate() {
        let mut cap_sess = setup_space_pilot(
            &ctx,
            884_120 + idx as u64,
            char_id,
            ship_id,
            cap_tid,
            VARD_SYS,
            beacon_pos,
            None,
        )
        .await;
        let cap_err = rpc(
            &ctx,
            &mut cap_sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_012), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
        )
        .await
        .expect_err("Capital/Freighter ship must be refused by Deathless Shipcaster");
        assert_eq!(cap_err.code, ERR_CAP_SHIPCASTER);
    }

    // 3. Ship outside 5,000m operational range is refused
    let mut far_sess = setup_space_pilot(
        &ctx,
        884_130,
        char_id,
        ship_id,
        587,
        ALSAVOINON_SYS,
        beacon_pos + DVec3::new(12_000.0, 0.0, 0.0),
        None,
    )
    .await;
    let range_err = rpc(
        &ctx,
        &mut far_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_013), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
    )
    .await
    .expect_err("Ship beyond 5,000m from Deathless beacon must be refused");
    assert_eq!(range_err.code, ERR_OUT_OF_RANGE);

    // 4. Horizon Siege active blocks Deathless Shipcaster entry into Zarzakh
    let mut siege_sess = setup_space_pilot(&ctx, 884_135, char_id, ship_id, 587, ALSAVOINON_SYS, beacon_pos, None).await;
    set_zarzakh_horizon_siege_active(&ctx, true);
    let siege_err = rpc(
        &ctx,
        &mut siege_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_013), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
    )
    .await
    .expect_err("Horizon Siege must block Deathless Shipcaster into Zarzakh");
    assert_eq!(siege_err.code, ERR_HORIZON_SIEGE_CASTER);
    set_zarzakh_horizon_siege_active(&ctx, false);

    // 5. All 4 beacons (Turnur, Vard, Alsavoinon, Hikkoken) jump eligible subcaps into Zarzakh
    for (idx, (origin_sys, beacon_id, faction_opt)) in [
        (TURNUR_SYS, 105_000_011_i64, None),
        (VARD_SYS, 105_000_012_i64, Some(500_010_i64)),
        (ALSAVOINON_SYS, 105_000_013_i64, Some(500_011_i64)),
        (HIKKOKEN_SYS, 105_000_014_i64, None),
    ]
    .into_iter()
    .enumerate()
    {
        let mut jump_sess = setup_space_pilot(
            &ctx,
            884_140 + idx as u64,
            char_id,
            ship_id,
            587,
            origin_sys,
            beacon_pos + DVec3::new(500.0, 0.0, 0.0),
            faction_opt,
        )
        .await;
        rpc(
            &ctx,
            &mut jump_sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(beacon_id), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
        )
        .await
        .expect("Eligible subcap should jump into Zarzakh via Deathless Shipcaster beacon");
        assert_eq!(jump_sess.solar_system_id, Some(ZARZAKH_SYS));
    }

    // 6. SQLite reload persists pilot and ship arrival in Zarzakh
    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_deathless_shipcaster");
    let (persisted_sys, persisted_ship_loc) = reloaded.db.with_snapshot(|s| {
        (
            s.characters.get(&char_id).map(|c| c.solar_system_id),
            s.items.get(&ship_id).map(|i| i.location_id),
        )
    });
    assert_eq!(persisted_sys, Some(ZARZAKH_SYS));
    assert_eq!(persisted_ship_loc, Some(ZARZAKH_SYS as i64));
}

#[tokio::test]
async fn test_zarzakh_emanation_lock_lifecycle_and_exits() {
    // parity: feat:navigation.zarzakh-emanation-lock
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;

    let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
    let alsa_gate_pos = ctx.sde.jumps[&ALSAVOINON_GATE_ID].position;
    let z_turnur_pos = ctx.sde.jumps[&ZARZAKH_TURNUR_GATE_ID].position;
    let z_alsa_pos = ctx.sde.jumps[&ZARZAKH_ALSAVOINON_GATE_ID].position;

    let mut sess = setup_space_pilot(
        &ctx,
        884_201,
        char_id,
        ship_id,
        621,
        TURNUR_SYS,
        turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
        Some(500_010),
    )
    .await;

    // 1. Entering Zarzakh via Turnur gate locks pilot to Zarzakh-Turnur gate (50016566) for 6h (21,600s)
    gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect("Jump into Zarzakh via Turnur gate");
    assert_eq!(sess.solar_system_id, Some(ZARZAKH_SYS));
    let (locked_gate, lock_exp) = ctx.db.with_snapshot(|s| s.zarzakh_gate_locks[&char_id]);
    assert_eq!(locked_gate, ZARZAKH_TURNUR_GATE_ID);
    assert!(lock_exp >= ctx.now_unix() + 21_590);

    // 2. Exiting via Alsavoinon gate (50016567) is refused; exiting via locked Turnur gate (50016566) succeeds
    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_alsa_pos;
    ctx.advance_secs(12);
    let lock_err = gate_jump(&ctx, &mut sess, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_GATE_ID, ship_id)
        .await
        .expect_err("Different Zarzakh exit gate must be refused while 6h lock is active");
    assert_eq!(lock_err.code, ERR_GATE_LOCKED);

    z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_turnur_pos;
    gate_jump(&ctx, &mut sess, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_id)
        .await
        .expect("Same Zarzakh gate exit must succeed");
    assert_eq!(sess.solar_system_id, Some(TURNUR_SYS));

    // 3. Entry from outside is never blocked by old lock, and entering via Alsavoinon resets lock to 50016567
    let mut alsa_sess = setup_space_pilot(
        &ctx, 884_202, char_id, ship_id, 621, ALSAVOINON_SYS, alsa_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
    ).await;
    gate_jump(&ctx, &mut alsa_sess, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ship_id)
        .await
        .expect("External entry via Alsavoinon gate must succeed and reset lock");
    assert_eq!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.get(&char_id).map(|v| v.0)), Some(ZARZAKH_ALSAVOINON_GATE_ID));

    // 4. After 6h (21,601s) expiry, exiting via Turnur gate (50016566) is allowed
    z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_turnur_pos;
    ctx.advance_secs(21_605);
    gate_jump(&ctx, &mut alsa_sess, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_id)
        .await
        .expect("Zarzakh gate exit via another gate must succeed once 6h lock expires");

    // 5. Leaving via Deathless Shipcaster (105_000_010 -> 104_000_020) is allowed while locked and clears lock
    let mut caster_sess = setup_space_pilot(
        &ctx, 884_203, char_id, ship_id, 621, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
    ).await;
    gate_jump(&ctx, &mut caster_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect("Re-enter Zarzakh to acquire fresh gate lock");
    z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = DVec3::new(250_000.0, 0.0, 0.0);
    ctx.advance_secs(12);
    rpc(
        &ctx,
        &mut caster_sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30_002_056), PyRep::Integer(104_000_020)],
    )
    .await
    .expect("Leaving Zarzakh via Deathless Shipcaster must succeed while gate-locked");
    assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

    // 6. Live gate entry -> Fulcrum CloneJump preserves lock; Jita CloneJump & SelfDestruct pod death clear lock
    let (fulcrum_clone_id, jita_clone_id, pod_id, pod_b_id, char_b_id) =
        (82_000_880_i64, 82_000_881_i64, 100_000_881_i64, 100_000_882_i64, 90_000_882_i64);
    let mut fulcrum_swap_sess = setup_space_pilot(
        &ctx, 884_204, char_b_id, pod_b_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_b_id).unwrap();
        ch.last_clone_jump_unix = 0;
        ch.home_station_id = JITA_STATION_ID;
        s.skills.entry(char_b_id).or_default().insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 3, skill_points: 45_000.0 });
        s.jump_clones.entry(char_b_id).or_default().push(JumpCloneRecord {
            jump_clone_id: fulcrum_clone_id,
            character_id: char_b_id,
            location_id: FULCRUM_STATION_ID,
            implants: vec![],
        });
    });
    gate_jump(&ctx, &mut fulcrum_swap_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_b_id)
        .await
        .expect("Enter Zarzakh via Turnur gate to acquire live gate lock");
    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
    z_sys.ballpark.write().unwrap().balls.get_mut(&pod_b_id).unwrap().position =
        fulcrum_pos + DVec3::new(100.0, 0.0, 0.0);
    ctx.advance_secs(12);
    rpc(&ctx, &mut fulcrum_swap_sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect("Dock capsule at The Fulcrum");
    ctx.advance_secs(12);
    rpc(&ctx, &mut fulcrum_swap_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(fulcrum_clone_id)])
        .await
        .expect("Same-station clone swap inside The Fulcrum must succeed");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.get(&char_b_id).map(|v| v.0)),
        Some(ZARZAKH_TURNUR_GATE_ID),
        "Swapping jump clones inside The Fulcrum must NOT clear Zarzakh 6-hour gate lock"
    );

    let mut clone_sess = setup_space_pilot(
        &ctx, 884_205, char_id, pod_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.last_clone_jump_unix = 0;
        ch.home_station_id = JITA_STATION_ID;
        s.skills.entry(char_id).or_default().insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 3, skill_points: 45_000.0 });
        s.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
            jump_clone_id: jita_clone_id,
            character_id: char_id,
            location_id: JITA_STATION_ID,
            implants: vec![],
        });
    });
    gate_jump(&ctx, &mut clone_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_id)
        .await
        .expect("Enter Zarzakh via Turnur gate before Jita CloneJump");
    z_sys.ballpark.write().unwrap().balls.get_mut(&pod_id).unwrap().position =
        fulcrum_pos + DVec3::new(100.0, 0.0, 0.0);
    ctx.advance_secs(12);
    rpc(&ctx, &mut clone_sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect("Dock capsule at The Fulcrum before Jita CloneJump");
    ctx.advance_secs(12);
    rpc(&ctx, &mut clone_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jita_clone_id)])
        .await
        .expect("CloneJump out of Zarzakh to Jita must succeed while locked");
    assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

    let mut pod_sess = setup_space_pilot(
        &ctx, 884_206, char_id, pod_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
    ).await;
    gate_jump(&ctx, &mut pod_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_id)
        .await
        .expect("Re-enter Zarzakh via Turnur gate before capsule self-destruct");
    assert!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));
    rpc(&ctx, &mut pod_sess, "ship", "SelfDestruct", &[])
        .await
        .expect("Start capsule SelfDestruct countdown in Zarzakh");
    ctx.advance_secs(125);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].station_id),
        Some(JITA_STATION_ID),
        "Capsule self-destruct countdown tick must detonate pod and respawn pilot at home station"
    );
    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_emanation_lock");
    assert!(!reloaded.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));
}

#[tokio::test]
async fn test_zarzakh_fulcrum_gates_dscan_size_bubbles_and_docking() {
    // parity: feat:navigation.zarzakh-fulcrum-gates
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
    let z_alsa_pos = ctx.sde.jumps[&ZARZAKH_ALSAVOINON_GATE_ID].position;

    // 1. Capital/Freighter hulls refused on Zarzakh stargates; Orca (28606) allowed and receives 6h lockout
    let mut dread_sess = setup_space_pilot(
        &ctx, 884_301, char_id, ship_id, 19_720, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), None,
    ).await;
    let dread_err = gate_jump(&ctx, &mut dread_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("Dreadnought must be refused on Zarzakh stargate");
    assert_eq!(dread_err.code, ERR_CAP_STARGATE);

    let mut orca_sess = setup_space_pilot(
        &ctx, 884_302, char_id, ship_id, 28_606, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), None,
    ).await;
    let prompt_err = gate_jump(&ctx, &mut orca_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("First 3-arg Orca jump must prompt for Zarzakh gate toll confirmation");
    assert_eq!(prompt_err.code, "AskConfirmZarzakhGateToll");
    gate_jump(&ctx, &mut orca_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect("Confirmed Orca jump must be allowed through Zarzakh stargate");
    assert_eq!(orca_sess.solar_system_id, Some(ZARZAKH_SYS));

    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_alsa_pos;
    ctx.advance_secs(12);
    let back_err = gate_jump(&ctx, &mut orca_sess, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_GATE_ID, ship_id)
        .await
        .expect_err("Horizon lockout must prevent back-jump via different Zarzakh gate");
    assert_eq!(back_err.code, ERR_GATE_LOCKED);

    // 2. Directional scanner rejected via BOTH V24 `scanSvc.ConeScan` and `scanMgr.ConeScan`
    for svc in ["scanSvc", "scanMgr"] {
        let dscan_err = rpc(&ctx, &mut orca_sess, svc, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0)])
            .await
            .expect_err("Directional scan must be disabled in Zarzakh");
        assert_eq!(dscan_err.code, ERR_NO_DSCAN);
    }

    // 3. Zarzakh no-bubble rules (`disallowInHazardSystem` attr 5561): ISL, unscripted WDFG, and Mobile Warp Disruptor
    let (sabre_ship_id, sabre_mod_id) = (100_000_303_i64, 200_000_301_i64);
    let mut sabre_sess = setup_space_pilot(&ctx, 884_303, char_id, sabre_ship_id, 22_456, ZARZAKH_SYS, DVec3::ZERO, None).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(sabre_mod_id, InventoryItemRecord {
            item_id: sabre_mod_id, type_id: 22_782, owner_id: char_id, location_id: sabre_ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(sabre_ship_id);
    let isl_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", &[PyRep::Integer(sabre_mod_id)])
        .await
        .expect_err("Interdiction Sphere Launcher must be disallowed in Zarzakh");
    assert_eq!(isl_err.code, ERR_DISALLOWED_HAZARD_MOD);

    let (onyx_ship_id, wdfg_mod_id, script_id, bubble_deploy_id, target_ship_id) =
        (100_000_305_i64, 200_000_302, 200_000_303, 200_000_304, 100_000_399);
    let _target_sess = setup_space_pilot(
        &ctx, 884_304, 90_000_302, target_ship_id, 587, ZARZAKH_SYS, DVec3::new(5_000.0, 0.0, 0.0), None,
    ).await;
    let mut onyx_sess = setup_space_pilot(&ctx, 884_305, char_id, onyx_ship_id, 11_995, ZARZAKH_SYS, DVec3::ZERO, None).await;
    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(char_id).or_default();
        sk.insert(11_584, TrainedSkillRecord { type_id: 11_584, level: 5, skill_points: 256_000.0 });
        sk.insert(3_435, TrainedSkillRecord { type_id: 3_435, level: 5, skill_points: 256_000.0 });
        s.items.insert(wdfg_mod_id, InventoryItemRecord {
            item_id: wdfg_mod_id, type_id: 28_654, owner_id: char_id, location_id: onyx_ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        s.items.insert(bubble_deploy_id, InventoryItemRecord {
            item_id: bubble_deploy_id, type_id: 12_198, owner_id: char_id, location_id: onyx_ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(onyx_ship_id);
    let unscripted_err = rpc(&ctx, &mut onyx_sess, "dogmaIM", "Activate", &[PyRep::Integer(wdfg_mod_id)])
        .await
        .expect_err("Unscripted WDFG bubble must be disallowed in Zarzakh");
    assert_eq!(unscripted_err.code, ERR_DISALLOWED_HAZARD_MOD);

    let mob_bubble_err = rpc(&ctx, &mut onyx_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_deploy_id)])
        .await
        .expect_err("Mobile Warp Disruptor anchoring must be disallowed in Zarzakh");
    assert_eq!(mob_bubble_err.code, ERR_ANCHORING_ZARZAKH);

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(script_id, InventoryItemRecord {
            item_id: script_id, type_id: 29_003, owner_id: char_id, location_id: onyx_ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, ..Default::default()
        });
    });
    ctx.invalidate_ship_fitting(onyx_ship_id);
    let _ = z_sys.add_target_lock(onyx_ship_id, TargetLock {
        target_id: target_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await;
    rpc(&ctx, &mut onyx_sess, "dogmaIM", "Activate", &[PyRep::Integer(wdfg_mod_id), PyRep::String(String::new()), PyRep::Integer(target_ship_id)])
        .await
        .expect("Focused-scripted WDFG point (attr 5561=0) should succeed in Zarzakh");

    // 4. Docking at The Fulcrum (60015187): Empire militia refused, neutral docks and persists across SQLite reload
    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
    let mut emp_dock = setup_space_pilot(
        &ctx, 884_306, char_id, ship_id, 621, ZARZAKH_SYS, fulcrum_pos + DVec3::new(100.0, 0.0, 0.0), Some(500_001),
    ).await;
    let emp_dock_err = rpc(&ctx, &mut emp_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect_err("Empire militia must be refused docking at The Fulcrum");
    assert_eq!(emp_dock_err.code, ERR_FULCRUM_EMPIRE);

    let mut neutral_dock = setup_space_pilot(
        &ctx, 884_307, char_id, ship_id, 621, ZARZAKH_SYS, fulcrum_pos + DVec3::new(100.0, 0.0, 0.0), None,
    ).await;
    let timer_err = rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect_err("Active weapons timer from scripted WDFG point must block Fulcrum docking");
    assert_eq!(timer_err.code, "FulcrumRefusesTimerActive");
    ctx.advance_secs(65);
    let retribution_err = rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect_err("15-minute Zarzakh disapproval timer must still block Fulcrum docking at +65s");
    assert_eq!(retribution_err.code, "FulcrumRefusesTimerActive");
    ctx.advance_secs(845);
    rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect("Neutral pilot should dock at The Fulcrum once weapons and Zarzakh disapproval timers expire");
    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_fulcrum_dock");
    assert_eq!(reloaded.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.station_id)), Some(FULCRUM_STATION_ID));
}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod zarzakh_gate_toll_and_horizon_siege {
    use super::*;

// Integration tests for Zarzakh gate toll and horizon siege `feat:navigation.*` rows:
// - `feat:navigation.zarzakh-gate-toll`
// - `feat:navigation.zarzakh-local-horizon-siege`


use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::DVec3;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    GoogleTimestamp, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{
    get_zarzakh_horizon_siege_state, is_zarzakh_horizon_siege_active, HorizonSiegePhase,
};
use eve_server::services::fw_gameplay::{
    apply_pirate_fob_structure_damage, ensure_campaign_pirate_fob, tick_insurgency_campaigns,
};
use eve_server::services::insurgency_services::{
    add_system_suppression, init_insurgency_campaign,
};
use eve_server::services::quasar_social_chat::on_character_local_chat_membership_refreshed;

use crate::features_fitting_dogma_rpcs::{
    gate_jump, rpc, setup_space_pilot, ALSAVOINON_GATE_ID, ALSAVOINON_SYS, ERR_ANCHORING_ZARZAKH,
    ERR_HORIZON_SIEGE_CASTER, FULCRUM_STATION_ID, TURNUR_GATE_ID, TURNUR_SYS,
    ZARZAKH_ALSAVOINON_GATE_ID, ZARZAKH_SYS, ZARZAKH_TURNUR_GATE_ID,
};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;

const ERR_HORIZON_SIEGE_GATE: &str = "HorizonSiegeGateAccessDenied";
const ERR_TOLL_PROMPT: &str = "AskConfirmZarzakhGateToll";
const ERR_TOLL_DECLINED: &str = "ZarzakhGateTollDeclined";
const ERR_TOLL_FUNDS: &str = "InsufficientFundsForZarzakhGateToll";
const ERR_CYNO_ZARZAKH: &str = "CynoDisallowedInZarzakh";
const ERR_DISALLOWED_HAZARD_MOD: &str = "ModuleDisallowedInHazardSystem";

fn make_public_req(type_name: &str, value: Vec<u8>, now_unix: u64) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now_unix as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-zarzakh-b8".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-zarzakh-b8".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

#[tokio::test]
async fn test_zarzakh_gate_toll_mass_scaling_decline_and_pirates() {
    // parity: feat:navigation.zarzakh-gate-toll
    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
    let alsa_gate_pos = ctx.sde.jumps[&ALSAVOINON_GATE_ID].position;

    // 1. Two-step confirmation challenge: first 3-arg call quotes toll (`AskConfirmZarzakhGateToll`),
    //    debits 0 ISK, and declining (`confirmToll: false`) aborts jump with `ZarzakhGateTollDeclined`
    let mut sess = setup_space_pilot(
        &ctx,
        884_401,
        char_id,
        ship_id,
        670, // Capsule (10,000 kg -> 10,000 ISK floor)
        TURNUR_SYS,
        turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
        None,
    )
    .await;
    let bal_init = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let quote_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("Unconfirmed 3-arg Zarzakh gate jump must return AskConfirmZarzakhGateToll");
    assert_eq!(quote_err.code, ERR_TOLL_PROMPT);
    assert!(quote_err.message.contains("10000 ISK"));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

    let mut decline_kw = HashMap::new();
    decline_kw.insert("confirmToll".into(), PyRep::Bool(false));
    let dec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_GATE_ID), PyRep::Integer(ZARZAKH_TURNUR_GATE_ID), PyRep::Integer(ship_id)],
            &decline_kw,
            None,
        )
        .await
        .expect_err("Declining Zarzakh gate toll must abort jump");
    assert_eq!(dec_err.code, ERR_TOLL_DECLINED);
    assert_eq!(sess.solar_system_id, Some(TURNUR_SYS));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);

    // 2. Insufficient funds after quote confirmation aborts jump BEFORE mutating character system state
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000));
    let _ = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id).await;
    let broke_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
        .await
        .expect_err("Insufficient ISK must abort confirmed Zarzakh gate jump");
    assert_eq!(broke_err.code, ERR_TOLL_FUNDS);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

    // 3. Real SDE hull mass scaling (zero `ball.mass` overrides):
    //    Capsule (670, 10k kg) -> 10,000 ISK floor; Caracal (621, 11.91M kg) -> 7,940,000 ISK;
    //    Raven (638, 99.3M kg) -> 66,200,000 ISK; Orca (28606, 150M kg) -> 100,000,000 ISK cap
    for (idx, (type_id, gate_id, to_gate_id, sys_id, gate_pos, expected_toll_isk)) in [
        (670_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 10_000_i64),
        (621_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 7_940_000_i64),
        (638_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 66_200_000_i64),
        (28_606_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 100_000_000_i64),
    ]
    .into_iter()
    .enumerate()
    {
        let mut hull_sess = setup_space_pilot(
            &ctx,
            884_402 + idx as u64,
            char_id,
            ship_id + idx as i64,
            type_id,
            sys_id,
            gate_pos + DVec3::new(200.0, 0.0, 0.0),
            None,
        )
        .await;
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(200_000_000));
        let _ = ctx.drain_notifications_for_char(char_id);
        let q_err = gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
            .await
            .expect_err("Step 1 of 2-step Zarzakh toll must return quote");
        assert_eq!(q_err.code, ERR_TOLL_PROMPT);
        gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
            .await
            .expect("Step 2 confirmed Zarzakh toll jump must succeed");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
            IskCents::from_isk(200_000_000 - expected_toll_isk)
        );
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(
            notes.iter().any(|n| n.event == "OnAccountChange"),
            "Zarzakh gate toll debit must emit OnAccountChange"
        );
    }

    // 4. Both Angel Cartel (500_010) and Guristas (500_011) pirate militias pay 0 ISK without toll prompt
    for (idx, pirate_faction) in [500_010_i64, 500_011_i64].into_iter().enumerate() {
        let mut p_sess = setup_space_pilot(
            &ctx,
            884_410 + idx as u64,
            char_id,
            ship_id,
            621,
            TURNUR_SYS,
            turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
            Some(pirate_faction),
        )
        .await;
        let p_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
        gate_jump(&ctx, &mut p_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect("Pirate militia pilot must jump into Zarzakh free of toll");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), p_before);
    }

    // 5. SQLite reload preserves journaled `zarzakh_gate_toll` entries
    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_gate_toll");
    let toll_entries = reloaded.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .filter(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll")
            .count()
    });
    assert_eq!(toll_entries, 4, "All 4 confirmed non-pirate SDE hull jumps must be journaled");
}

#[tokio::test]
async fn test_zarzakh_local_horizon_siege_chat_anchoring_and_cyno() {
    // parity: feat:navigation.zarzakh-local-horizon-siege
    let ctx = ServerContext::new();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_502_i64;
    let ship_a = 100_000_501_i64;
    let ship_b = 100_000_502_i64;

    // 1. Delayed local chat in Zarzakh (Quasar `eve_public.chat.local.api.*` AND `LSC.GetMembers`)
    let mut sess_a = setup_space_pilot(&ctx, 884_501, char_a, ship_a, 11_957, ZARZAKH_SYS, DVec3::ZERO, None).await;
    let sess_b = setup_space_pilot(
        &ctx,
        884_502,
        char_b,
        ship_b,
        587,
        ZARZAKH_SYS,
        DVec3::new(1_000.0, 0.0, 0.0),
        None,
    )
    .await;
    let now = ctx.now_unix();
    ctx.quasar_gateway.subscribe_session(&sess_a, vec![], now);
    ctx.quasar_gateway.subscribe_session(&sess_b, vec![], now);
    let _ = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    ctx.sync_character_local_chat(char_b, Some(TURNUR_SYS), ZARZAKH_SYS);

    let entry_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    assert!(
        !entry_notices
            .iter()
            .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("JoinNotice"))),
        "Entering Zarzakh must not emit JoinNotice before speaking"
    );
    let mem_req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
    let mem_rsp = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now);
    assert_eq!(mem_rsp.status_code, STATUS_OK);
    let mem_wm = WireMessage::decode(&mem_rsp.payload.unwrap().value).unwrap();
    assert_eq!(
        mem_wm.fields.iter().filter(|f| f.field_number == 1).count(),
        0,
        "Silent Zarzakh pilots must stay hidden"
    );

    let z_chan = format!("solarsystemid2:{ZARZAKH_SYS}");
    let lsc_before = rpc(&ctx, &mut sess_a, "LSC", "GetMembers", &[PyRep::String(z_chan)])
        .await
        .expect("LSC.GetMembers in Zarzakh");
    if let PyRep::Dict(entries) = lsc_before {
        assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(char_b)));
    }

    // Silent pilot B must NOT leak presence via `on_character_local_chat_membership_refreshed` or `MuteRequest`
    let ref_delivered = on_character_local_chat_membership_refreshed(&ctx.quasar_gateway, &ctx.db, char_b as u32, now)
        .expect("on_character_local_chat_membership_refreshed for silent Zarzakh pilot");
    assert_eq!(ref_delivered, 0, "Silent Zarzakh pilot must not emit MembershipRefreshedNotice");
    let mut mute_payload = Vec::new();
    let mut char_id_msg = Vec::new();
    encode_varint_field(1, char_b as u64, &mut char_id_msg);
    encode_bytes_field(1, &char_id_msg, &mut mute_payload);
    let mute_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", mute_payload, now);
    let _ = ctx.quasar_gateway.handle_public_request(&sess_a, &mute_req, None, true, now);
    let mute_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    assert!(
        !mute_notices
            .iter()
            .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("MembershipRefreshedNotice"))),
        "MuteRequest on silent Zarzakh pilot must not leak MembershipRefreshedNotice"
    );

    // Pilot B speaks -> JoinNotice + MessageBroadcastNotice emitted and Pilot B appears in Zarzakh local
    let mut chat_msg = Vec::new();
    encode_string_field(1, "o7 from Zarzakh", &mut chat_msg);
    let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", chat_msg, now + 400);
    let bcast_rsp = ctx.quasar_gateway.handle_public_request(&sess_b, &bcast_req, None, false, now + 400);
    assert_eq!(bcast_rsp.status_code, STATUS_OK);
    let mem_rsp_after = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now + 400);
    let mem_wm_after = WireMessage::decode(&mem_rsp_after.payload.unwrap().value).unwrap();
    assert_eq!(mem_wm_after.fields.iter().filter(|f| f.field_number == 1).count(), 1);

    // 2. Capsuleer Upwell structure + mobile deployable anchoring/launching prohibited in Zarzakh
    let (depot_id, cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id, mjfg_mod_id) = (
        200_000_510_i64,
        200_000_511_i64,
        200_000_512_i64,
        200_000_513_i64,
        200_000_514_i64,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33_474,
                owner_id: char_a,
                location_id: ship_a,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        for (mid, tid, flag) in [
            (cyno_mod_id, 21_096_u32, InventoryFlag::HiSlot0),     // Cynosural Field Generator I
            (cov_cyno_mod_id, 28_646_u32, InventoryFlag::HiSlot1), // Covert Cynosural Field Generator I
            (ind_cyno_mod_id, 52_694_u32, InventoryFlag::HiSlot2), // Industrial Cynosural Field Generator I
            (mjfg_mod_id, 4_248_u32, InventoryFlag::HiSlot3),      // Warp Disruption Field Generator II (attr 5561=1)
        ] {
            s.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_a,
                    location_id: ship_a,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    let upwell_err = rpc(&ctx, &mut sess_a, "structureDeployment", "Anchor", &[PyRep::Integer(35_832)])
        .await
        .expect_err("Upwell structure anchoring must be disallowed in Zarzakh");
    assert_eq!(upwell_err.code, ERR_ANCHORING_ZARZAKH);
    let depot_anchor_err = rpc(&ctx, &mut sess_a, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)])
        .await
        .expect_err("Deployable anchoring must be disallowed in Zarzakh");
    assert_eq!(depot_anchor_err.code, ERR_ANCHORING_ZARZAKH);
    let depot_launch_err = rpc(
        &ctx,
        &mut sess_a,
        "ship",
        "Drop",
        &[PyRep::List(vec![PyRep::Integer(depot_id)])],
    )
    .await
    .expect_err("Launching deployables from ship bay must be disallowed in Zarzakh");
    assert_eq!(depot_launch_err.code, ERR_ANCHORING_ZARZAKH);

    // 3. Cynosural fields (standard 21096, covert 28646, industrial 52694) & hazard-banned modules (37480) prohibited
    for mid in [cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id] {
        let cyno_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mid)])
            .await
            .expect_err("Cynosural field activation must be disallowed in Zarzakh");
        assert_eq!(cyno_err.code, ERR_CYNO_ZARZAKH);
    }
    let mjfg_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mjfg_mod_id)])
        .await
        .expect_err("Micro Jump Field Generator (attr 5561=1) must be disallowed in Zarzakh");
    assert_eq!(mjfg_err.code, ERR_DISALLOWED_HAZARD_MOD);

    // 4. Live Horizon Siege state machine: Insurgency FOB armor reinforcement -> Warning (300s) -> Active (3600s) -> Stabilizing -> Normal
    let camp_id = 777_001_i64;
    let camp_systems = vec![TURNUR_SYS, ALSAVOINON_SYS, 30_002_538, 30_045_338, 30_002_056];
    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_002, TURNUR_SYS, camp_systems.clone());
    ctx.advance_secs(48 * 3_600 + 10);
    for &sys in &camp_systems {
        let _ = add_system_suppression(&ctx, camp_id, sys, 1_000);
    }
    let fob_id = ensure_campaign_pirate_fob(&ctx, camp_id).await.expect("spawn pirate FOB");
    apply_pirate_fob_structure_damage(&ctx, char_a, fob_id, 25_000_000.0)
        .await
        .expect("Reinforcing Pirate FOB armor must schedule Zarzakh Horizon Siege Warning");
    let warning_st = get_zarzakh_horizon_siege_state(&ctx);
    assert_eq!(warning_st.phase, HorizonSiegePhase::Warning);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));

    ctx.advance_secs(305);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    let active_st = get_zarzakh_horizon_siege_state(&ctx);
    assert_eq!(active_st.phase, HorizonSiegePhase::Active);
    assert!(active_st.fulcrum_restricted_to_pirates);
    assert!(is_zarzakh_horizon_siege_active(&ctx));

    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
    let mut neutral_fulcrum = setup_space_pilot(
        &ctx,
        884_508,
        char_b,
        ship_b,
        587,
        ZARZAKH_SYS,
        fulcrum_pos + DVec3::new(100.0, 0.0, 0.0),
        None,
    )
    .await;
    let fulcrum_siege_err = rpc(&ctx, &mut neutral_fulcrum, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
        .await
        .expect_err("Non-pirate must be refused Fulcrum docking during active Horizon Siege");
    assert_eq!(fulcrum_siege_err.code, "HorizonSiegeFulcrumLockdown");

    let reloaded = persist_and_reopen_sqlite(&ctx, "b8_horizon_siege");
    assert!(is_zarzakh_horizon_siege_active(&reloaded));

    let z_turnur_pos = reloaded.sde.jumps[&ZARZAKH_TURNUR_GATE_ID].position;
    let mut siege_exit = setup_space_pilot(
        &reloaded,
        884_510,
        char_a,
        ship_a,
        621,
        ZARZAKH_SYS,
        z_turnur_pos + DVec3::new(200.0, 0.0, 0.0),
        Some(500_010),
    )
    .await;
    let gate_siege_err = gate_jump(&reloaded, &mut siege_exit, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_a)
        .await
        .expect_err("Horizon Siege must block Zarzakh stargates after SQLite reload");
    assert_eq!(gate_siege_err.code, ERR_HORIZON_SIEGE_GATE);

    reloaded
        .systems
        .get_or_spawn(ZARZAKH_SYS)
        .ballpark
        .write()
        .unwrap()
        .balls
        .get_mut(&ship_a)
        .unwrap()
        .position = DVec3::new(250_000.0, 0.0, 0.0);
    let caster_siege_err = rpc(
        &reloaded,
        &mut siege_exit,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(105_000_010), PyRep::Integer(30_002_056), PyRep::Integer(104_000_020)],
    )
    .await
    .expect_err("Horizon Siege must block Zarzakh Deathless Shipcaster after SQLite reload");
    assert_eq!(caster_siege_err.code, ERR_HORIZON_SIEGE_CASTER);

    let after_siege_ts = get_zarzakh_horizon_siege_state(&reloaded).active_until_unix + 5;
    tick_insurgency_campaigns(&reloaded, after_siege_ts).await;
    assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Stabilizing);
    assert!(!is_zarzakh_horizon_siege_active(&reloaded));
    let after_cooldown_ts = get_zarzakh_horizon_siege_state(&reloaded).cooldown_until_unix + 5;
    tick_insurgency_campaigns(&reloaded, after_cooldown_ts).await;
    assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Normal);
}

}
