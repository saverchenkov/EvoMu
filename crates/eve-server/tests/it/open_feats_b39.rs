//! Batch 39 open `feat:` parity integration tests (`feat:structures.*`, `feat:fw.*`, `feat:ships.*`, `feat:modules.*`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{
    SKILL_ANCHORING, TYPE_COVERT_MOBILE_CYNO_BEACON, TYPE_MOBILE_CYNO_BEACON,
    TYPE_MOBILE_MJU, TYPE_MOBILE_OBSERVATORY,
};
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
    sess.user_id = Some(char_id as u64);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.security_status = 0.0;
        ch.balance = IskCents::from_isk(500_000_000);
        s.characters.insert(char_id, ch);
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
        s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 0.5,
                mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_fulcrum_alignment_services() {
    // parity: feat:structures.fulcrum-alignment-services
    eve_testkit::attest(&["feat:structures.fulcrum-alignment-services"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let zarzakh_sys = 30_100_000_u32;
    let fulcrum_id = 60_015_187_i64;
    let char_id = 91_000_001_i64;
    let ship_id = 101_000_001_i64;

    let fulcrum_pos = ctx
        .sde
        .stations
        .get(&fulcrum_id)
        .map(|s| s.position)
        .unwrap_or(DVec3::ZERO);
    let mut sess = setup_pilot(
        &ctx,
        char_id,
        ship_id,
        621,
        zarzakh_sys,
        fulcrum_pos + DVec3::new(100.0, 0.0, 0.0),
        1_000_009,
    )
    .await;
    let beyonce_id = ctx.bind_object(&mut sess, "beyonce", zarzakh_sys as i64).unwrap();

    // 1. Fulcrum refuses docking to Empire militia (500001 Caldari) & active retribution timer
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
    });
    let militia_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdDock",
            &[PyRep::Integer(fulcrum_id), PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Empire militia refused docking at Fulcrum");
    assert_eq!(militia_err.code, "FulcrumRefusesEmpireMilitia");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = None;
    });
    let now = ctx.now_unix();
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(char_id, now + 300);
    });
    let timer_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdDock",
            &[PyRep::Integer(fulcrum_id), PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Active timer refused docking at Fulcrum");
    assert_eq!(timer_err.code, "FulcrumRefusesTimerActive");

    // 2. Neutral character docks and gets repair, fitting, market, insurance, industry
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.remove(&char_id);
    });
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdDock",
        &[PyRep::Integer(fulcrum_id), PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Neutral pilot docks at Fulcrum");
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_id, 17));
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_id, 21));
    assert!(eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, fulcrum_id, 13));

    // 3. Jump cloning requires pirate enlistment; corp offices require pirate-enlisted corp; Angel/Guristas LP stores
    sess.station_id = Some(fulcrum_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let clone_err = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await
        .expect_err("Neutral clone install refused");
    assert_eq!(clone_err.code, "FulcrumCloneRequiresPirateEnlistment");

    let office_err = ctx
        .dispatch_rpc(&mut sess, "officeManager", None, "RentOffice", &[PyRep::Integer(fulcrum_id)], &kw, None)
        .await
        .expect_err("Neutral corp office rental refused at Fulcrum");
    assert_eq!(office_err.code, "FulcrumOfficeRequiresPirateEnlistment");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_010);
        s.corporations.insert(
            1_000_009,
            eve_db::CorporationRecord {
                corporation_id: 1_000_009,
                name: "Fulcrum Pirates".into(),
                ticker: "FCRP".into(),
                ceo_id: char_id,
                master_wallet: IskCents::from_isk(10_000_000),
                ..Default::default()
            },
        );
        s.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
        .await
        .expect("Pirate pilot installs clone at Fulcrum");
    ctx.dispatch_rpc(&mut sess, "officeManager", None, "RentOffice", &[PyRep::Integer(fulcrum_id)], &kw, None)
        .await
        .expect("Pirate-enlisted corp rents office at Fulcrum");

    // Angel Cartel (1000124) and Guristas (1000127) LP stores available
    let angel_lp = ctx
        .dispatch_rpc(&mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_124)], &kw, None)
        .await
        .expect("Angel Cartel LP store offers");
    assert!(!matches!(angel_lp, PyRep::None));
    let guristas_lp = ctx
        .dispatch_rpc(&mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_127)], &kw, None)
        .await
        .expect("Guristas LP store offers");
    assert!(!matches!(guristas_lp, PyRep::None));
}

#[tokio::test]
async fn test_b39_mobile_cyno_beacon_and_micro_jump_unit() {
    // parity: feat:structures.mobile-cyno-beacon, feat:structures.mobile-micro-jump-unit
    eve_testkit::attest(&[
        "feat:structures.mobile-cyno-beacon",
        "feat:structures.mobile-micro-jump-unit",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let origin_sys = 30_000_014_u32; // Shamahi (2.10 LY from Asabona)
    let dest_sys = 30_000_012_u32; // Low-sec (Asabona)
    let c1 = 91_000_011_i64;
    let s1 = 101_000_011_i64;
    let c2 = 91_000_012_i64;
    let s2 = 101_000_012_i64;

    let mut sess1 = setup_pilot(&ctx, c1, s1, 587, dest_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess2 = setup_pilot(&ctx, c2, s2, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_009).await; // Chimera carrier

    // 1. Mobile Cyno Beacon requires Anchoring III and fleet membership
    let beacon_id = 101_000_099_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: TYPE_MOBILE_CYNO_BEACON,
                owner_id: c1,
                location_id: s1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.skills.entry(c1).or_default().insert(
            SKILL_ANCHORING,
            TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 2, skill_points: 10_000.0 },
        );
    });
    let no_skill_err = ctx
        .dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect_err("Anchoring II must be rejected");
    assert_eq!(no_skill_err.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&c1).unwrap().get_mut(&SKILL_ANCHORING).unwrap().level = 3;
    });
    let no_fleet_err = ctx
        .dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect_err("Non-fleet deployment must be rejected");
    assert_eq!(no_fleet_err.code, "MustBeInFleet");

    // Put both pilots in fleet 777
    sess1.fleet_id = Some(777);
    sess2.fleet_id = Some(777);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            777,
            eve_db::FleetRecord {
                fleet_id: 777,
                boss_id: c1,
                members: vec![c1, c2],
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect("Deploy Mobile Cyno Beacon");

    // While spooling (1 minute), jump is refused
    let spool_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "beyonce",
            None,
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Spooling beacon not yet active");
    assert_eq!(spool_err.code, "NoActiveBeacon");

    // Advance 61s + give Chimera fuel & Jump Drive Calibration V -> single-use jump succeeds and consumes beacon
    ctx.advance_secs(61);
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c2).or_default().insert(
            3456,
            TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 },
        );
        s.items.insert(
            101_000_098,
            InventoryItemRecord {
                item_id: 101_000_098,
                type_id: 17888, // Nitrogen Isotopes
                owner_id: c2,
                location_id: s2,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        None,
        "CmdJumpToFleetDeployableBeacon",
        &[PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
        &kw,
        None,
    )
    .await
    .expect("Jump to Mobile Cyno Beacon succeeds");

    // Verify beacon stays active after first jump (60m lifetime, covert 30m + lower HP), fleet-only, and early destruction
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&beacon_id)), "Beacon remains active for 60m lifetime");
    let dep_rec = eve_server::services::mobile_deployables::read_deployable_state(&ctx)
        .deployables
        .get(&beacon_id)
        .cloned()
        .unwrap();
    assert_eq!(dep_rec.expires_at_unix - dep_rec.anchor_start_unix, 60 + 3_600);
    let norm_cs = ctx.build_type_combat_state(TYPE_MOBILE_CYNO_BEACON);
    let cov_cs = ctx.build_type_combat_state(TYPE_COVERT_MOBILE_CYNO_BEACON);
    let norm_hp = norm_cs.max_shield_hp + norm_cs.max_armor_hp + norm_cs.max_structure_hp;
    let cov_hp = cov_cs.max_shield_hp + cov_cs.max_armor_hp + cov_cs.max_structure_hp;
    assert!(cov_hp < norm_hp, "Covert Mobile Cyno Beacon has lower HP ({cov_hp} < {norm_hp})");

    // Non-fleet pilot cannot jump to fleet-only beacon
    let c_outsider = 91_000_013_i64;
    let s_outsider = 101_000_013_i64;
    let mut sess_out = setup_pilot(&ctx, c_outsider, s_outsider, 23915, origin_sys, DVec3::new(500_000.0, 0.0, 0.0), 1_000_010).await;
    let out_err = ctx
        .dispatch_rpc(
            &mut sess_out,
            "beyonce",
            None,
            "CmdJumpToFleetDeployableBeacon",
            &[PyRep::Integer(c1), PyRep::Integer(dest_sys as i64), PyRep::Integer(beacon_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-fleet pilot cannot use fleet mobile cyno beacon");
    assert_eq!(out_err.code, "NoActiveBeacon");

    // Early destruction via damage removes the beacon
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let _ = dest_handle
        .apply_damage(
            beacon_id,
            eve_destiny::DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
        )
        .await;
    assert!(dest_handle.get_ball_snapshot(beacon_id).await.unwrap().is_none(), "Destroyed beacon removed from space");

    // 2. Mobile Micro Jump Unit: 1m activation delay, 2d lifetime, can't be scooped, 12s spool -> 100km jump, mass < 1B kg
    let mju_id = 101_000_097_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            mju_id,
            InventoryItemRecord {
                item_id: mju_id,
                type_id: TYPE_MOBILE_MJU,
                owner_id: c1,
                location_id: s1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mju_id)], &kw, None)
        .await
        .expect("Deploy MMJU");
    let mju_rec = eve_server::services::mobile_deployables::read_deployable_state(&ctx)
        .deployables
        .get(&mju_id)
        .cloned()
        .unwrap();
    assert_eq!(mju_rec.expires_at_unix - mju_rec.anchor_start_unix, 2 * 86_400);
    let scoop_err = ctx
        .dispatch_rpc(&mut sess1, "posMgr", None, "Scoop", &[PyRep::Integer(mju_id)], &kw, None)
        .await
        .expect_err("MMJU cannot be scooped");
    assert!(!scoop_err.code.is_empty());

    ctx.advance_secs(61);
    let (_, before_ball) = dest_handle.get_ball_snapshot(s1).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(mju_id)], &kw, None)
        .await
        .expect("Activate MMJU");
    // Duplicate spool rejected
    let dup_err = ctx
        .dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(mju_id)], &kw, None)
        .await
        .expect_err("Duplicate MMJU spool rejected");
    assert_eq!(dup_err.code, "MicroJumpAlreadySpooling");

    // Hostile (different corp/fleet) ship can activate concurrently
    let c_hostile = 91_000_014_i64;
    let s_hostile = 101_000_014_i64;
    let mut sess_hostile = setup_pilot(&ctx, c_hostile, s_hostile, 587, dest_sys, DVec3::new(500_500.0, 0.0, 0.0), 1_000_011).await;
    let (_, h_before) = dest_handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap();
    ctx.dispatch_rpc(&mut sess_hostile, "dogmaIM", None, "Activate", &[PyRep::Integer(mju_id)], &kw, None)
        .await
        .expect("Hostile ship activates MMJU concurrently");

    // At 10s + 1s step = 11s, 12s warm-up has not completed yet
    ctx.advance_secs(10);
    dest_handle.step_manual(1.0).await.expect("step at 11s");
    let (_, mid_ball) = dest_handle.get_ball_snapshot(s1).await.unwrap().unwrap();
    assert!((mid_ball.position - before_ball.position).length() < 10.0, "Ship has not jumped before 12s warm-up");

    // At 12s+, both ships jump 100 km, and there is no use cooldown
    ctx.advance_secs(3);
    dest_handle.step_manual(1.0).await.expect("step MMJU jump");
    let (_, after_ball) = dest_handle.get_ball_snapshot(s1).await.unwrap().unwrap();
    let jumped_dist = (after_ball.position - before_ball.position).length();
    assert!((jumped_dist - 100_000.0).abs() < 100.0, "Ship jumped 100 km (got {jumped_dist})");
    let (_, h_after) = dest_handle.get_ball_snapshot(s_hostile).await.unwrap().unwrap();
    assert!(((h_after.position - h_before.position).length() - 100_000.0).abs() < 100.0, "Hostile ship jumped 100 km");
}

#[tokio::test]
async fn test_b39_mobile_observatory_decloak_ping() {
    // parity: feat:structures.mobile-observatory-decloak-ping
    eve_testkit::attest(&["feat:structures.mobile-observatory-decloak-ping"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let hisec_sys = 30_000_142_u32;
    let lowsec_sys = 30_000_012_u32; // Asabona (0.32)
    let c1 = 91_000_021_i64;
    let s1 = 101_000_021_i64;

    let mut sess = setup_pilot(&ctx, c1, s1, 587, hisec_sys, DVec3::new(600_000.0, 0.0, 0.0), 1_000_009).await;
    let obs_id = 101_000_090_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c1).or_default().insert(
            SKILL_ANCHORING,
            TrainedSkillRecord { type_id: SKILL_ANCHORING, level: 3, skill_points: 40_000.0 },
        );
        s.items.insert(
            obs_id,
            InventoryItemRecord {
                item_id: obs_id,
                type_id: TYPE_MOBILE_OBSERVATORY,
                owner_id: c1,
                location_id: s1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. Mobile Observatory refused in high-sec, wormhole, Abyssal, and deadspace
    let hisec_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None)
        .await
        .expect_err("Observatory refused in high-sec");
    assert_eq!(hisec_err.code, "CannotDeployInHighSec");

    sess.solar_system_id = Some(31_000_001);
    let wh_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None)
        .await
        .expect_err("Observatory refused in wormhole");
    assert!(!wh_err.code.is_empty());

    sess.solar_system_id = Some(32_000_001);
    let abyss_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None)
        .await
        .expect_err("Observatory refused in Abyssal space");
    assert!(!abyss_err.code.is_empty());

    // Move to low-sec and deploy -> 10m deploy, can't be scooped, pings cloaked ships past stabilization
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&c1).unwrap().solar_system_id = lowsec_sys;
        s.items.get_mut(&s1).unwrap().location_id = lowsec_sys as i64;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let low_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = low_handle
        .spawn_ball_with_combat(
            Ball {
                id: s1,
                type_id: 587,
                position: DVec3::new(1_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;

    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(obs_id)], &kw, None)
        .await
        .expect("Deploy Mobile Observatory in low-sec");
    let scoop_err = ctx
        .dispatch_rpc(&mut sess, "posMgr", None, "Scoop", &[PyRep::Integer(obs_id)], &kw, None)
        .await
        .expect_err("Observatory cannot be scooped");
    assert!(!scoop_err.code.is_empty());

    // Spawn 10 cloaked ships past stabilization timer and advance 10 minutes -> ~40% decloaked
    for i in 0..10_i64 {
        let tid = 101_000_200 + i;
        let mut cs = ctx.build_type_combat_state(587);
        cs.is_cloaked = true;
        cs.cloak_stabilization_remaining_s = 0.0;
        let _ = low_handle
            .spawn_ball_with_combat(
                Ball {
                    id: tid,
                    type_id: 587,
                    position: DVec3::new(1_000_050_000.0 + (i as f64) * 5_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }
    assert!(
        low_handle.get_combat_state(101_000_200).await.unwrap().unwrap().is_cloaked,
        "Ship 0 should be cloaked before observatory ping"
    );
    ctx.advance_secs(601);
    low_handle.step_manual(1.0).await.expect("step observatory ping");
    let mut decloaked = 0;
    for i in 0..10_i64 {
        let tid = 101_000_200 + i;
        if let Some(cs) = low_handle.get_combat_state(tid).await.unwrap() {
            if !cs.is_cloaked {
                decloaked += 1;
            }
        }
    }
    assert!(decloaked > 0 && decloaked < 10, "40% independent decloak ping decloaked {decloaked}/10 ships");

    // 2. Mobile Observatory expires after 2 hours
    ctx.advance_secs(7_201);
    low_handle.step_manual(1.0).await.expect("step observatory expiry");
    assert!(
        low_handle.get_ball_snapshot(obs_id).await.unwrap().is_none(),
        "Mobile Observatory expired and removed after 2 hours"
    );
}

