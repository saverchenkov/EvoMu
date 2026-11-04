//! Test coverage for Zarzakh Gate Lock (`feat:navigation.zarzakh-gate-lock`):
//! - Gate lock reset after entering through a different gate
//! - Gate lock cleared on clone jump and emits DeactivatedNotice
//! - Gate lock cleared on pod death and emits DeactivatedNotice

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const ZARZAKH_SYS: u32 = 30_100_000;
const TURNUR_SYS: u32 = 30_002_086;
const ALSAVOINON_SYS: u32 = 30_003_841;

const TURNUR_GATE_IN_TURNUR: i64 = 50_016_562;
const TURNUR_GATE_IN_ZARZAKH: i64 = 50_016_566;
const ALSAVOINON_GATE_IN_ALSAVOINON: i64 = 50_016_563;
const ALSAVOINON_GATE_IN_ZARZAKH: i64 = 50_016_567;

#[tokio::test]
async fn test_zarzakh_gate_lock_resets_after_entering_through_different_gate() {
    // parity: feat:navigation.zarzakh-gate-lock
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 101_000_101_i64;

    let mut sess = SessionState::new(884_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = TURNUR_SYS;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(100_000_000);
            ch.war_faction_id = None;
            ch.station_id = None;
            ch.structure_id = None;
        }
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 621, // Caracal (cruiser)
                owner_id: char_id,
                location_id: TURNUR_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(TURNUR_SYS);
    sess.ship_id = Some(ship_id);

    // 1. Enter Zarzakh from Turnur via gate 50016562
    let turnur_pos = ctx
        .sde
        .jumps
        .get(&TURNUR_GATE_IN_TURNUR)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let t_handle = ctx.systems.get_or_spawn(TURNUR_SYS);
    let _ = t_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: turnur_pos + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let turnur_beyonce = ctx.bind_object(&mut sess, "beyonce", TURNUR_SYS as i64).unwrap();
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(turnur_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_GATE_IN_TURNUR)],
            &kw,
            None,
        )
        .await
        .expect("Jump from Turnur into Zarzakh");
    assert_eq!(
        jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ZARZAKH_SYS as i64)
    );

    // Verify lock is set to Turnur gate in Zarzakh (50016566)
    let lock_gate = ctx
        .db
        .with_snapshot(|s| s.zarzakh_gate_locks.get(&char_id).map(|(g, _)| *g));
    assert_eq!(lock_gate, Some(TURNUR_GATE_IN_ZARZAKH));

    // 2. In Zarzakh, exiting via Alsavoinon gate (50016567) is refused with ZarzakhGateLocked
    ctx.advance_secs(15);
    let z_handle = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let alsa_z_pos = ctx
        .sde
        .jumps
        .get(&ALSAVOINON_GATE_IN_ZARZAKH)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = alsa_z_pos + DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    sess.solar_system_id = Some(ZARZAKH_SYS);
    let zarzakh_beyonce = ctx.bind_object(&mut sess, "beyonce", ZARZAKH_SYS as i64).unwrap();

    let err_alsa = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(zarzakh_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(ALSAVOINON_GATE_IN_ZARZAKH)],
            &kw,
            None,
        )
        .await
        .expect_err("Alsavoinon gate exit blocked by Turnur lock");
    assert_eq!(err_alsa.code, "ZarzakhGateLocked");

    // 3. Exiting via allowed Turnur gate (50016566) succeeds
    let turnur_z_pos = ctx
        .sde
        .jumps
        .get(&TURNUR_GATE_IN_ZARZAKH)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = turnur_z_pos + DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    ctx.advance_secs(15);

    let exit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(zarzakh_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_GATE_IN_ZARZAKH)],
            &kw,
            None,
        )
        .await
        .expect("Exit via Turnur gate succeeds");
    assert_eq!(
        exit_res.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(TURNUR_SYS as i64)
    );

    // 4. Pilot flies to Alsavoinon and enters Zarzakh through Alsavoinon gate (50016563)
    ctx.advance_secs(15);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = ALSAVOINON_SYS;
    });
    sess.solar_system_id = Some(ALSAVOINON_SYS);
    let a_handle = ctx.systems.get_or_spawn(ALSAVOINON_SYS);
    let alsa_pos = ctx
        .sde
        .jumps
        .get(&ALSAVOINON_GATE_IN_ALSAVOINON)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let _ = a_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: alsa_pos + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 10_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let alsa_beyonce = ctx.bind_object(&mut sess, "beyonce", ALSAVOINON_SYS as i64).unwrap();
    let reenter_alsa = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(alsa_beyonce),
            "CmdStargateJump",
            &[PyRep::Integer(ALSAVOINON_GATE_IN_ALSAVOINON)],
            &kw,
            None,
        )
        .await
        .expect("Jump from Alsavoinon into Zarzakh");
    assert_eq!(
        reenter_alsa.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ZARZAKH_SYS as i64)
    );

    // 5. Assert gate lock has RESET to Alsavoinon gate in Zarzakh (50016567)
    let reset_lock_gate = ctx
        .db
        .with_snapshot(|s| s.zarzakh_gate_locks.get(&char_id).map(|(g, _)| *g));
    assert_eq!(reset_lock_gate, Some(ALSAVOINON_GATE_IN_ZARZAKH));

    // 6. In Zarzakh, now Turnur gate is BLOCKED with ZarzakhGateLocked
    ctx.advance_secs(15);
    let z_handle = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = turnur_z_pos + DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    sess.solar_system_id = Some(ZARZAKH_SYS);
    let zarzakh_beyonce_2 = ctx.bind_object(&mut sess, "beyonce", ZARZAKH_SYS as i64).unwrap();

    let err_turnur = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(zarzakh_beyonce_2),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_GATE_IN_ZARZAKH)],
            &kw,
            None,
        )
        .await
        .expect_err("Turnur gate exit blocked after lock reset to Alsavoinon");
    assert_eq!(err_turnur.code, "ZarzakhGateLocked");

    // 7. And Alsavoinon gate exit succeeds
    let mut ship_b = z_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_b.position = alsa_z_pos + DVec3::new(100.0, 0.0, 0.0);
    let _ = z_handle.spawn_ball(ship_b).await;
    ctx.advance_secs(15);

    let exit_alsa = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(zarzakh_beyonce_2),
            "CmdStargateJump",
            &[PyRep::Integer(ALSAVOINON_GATE_IN_ZARZAKH)],
            &kw,
            None,
        )
        .await
        .expect("Exit via reset Alsavoinon gate succeeds");
    assert_eq!(
        exit_alsa.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ALSAVOINON_SYS as i64)
    );
}

#[tokio::test]
async fn test_zarzakh_gate_lock_clearing_on_clone_jump() {
    // parity: feat:navigation.zarzakh-gate-lock
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let fulcrum_station = 60_015_187_i64;
    let jita_station = 60_003_760_i64;
    let capsule_id = 101_000_102_i64;
    let remote_clone_id = 82_000_102_i64;

    let mut sess = SessionState::new(884_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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
    ctx.advance_secs(11);

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = ZARZAKH_SYS;
        ch.station_id = Some(fulcrum_station);
        ch.structure_id = None;
        ch.active_ship_id = capsule_id;
        ch.balance = IskCents::from_isk(50_000_000);
        ch.war_faction_id = None;
        ch.last_clone_jump_unix = 0;

        s.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
                type_id: 670, // Capsule
                owner_id: char_id,
                location_id: fulcrum_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Seed Infomorph Psychology skill
        s.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 3,
                skill_points: 45_000.0,
            },
        );
        // Seed remote jump clone in Jita
        s.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
            jump_clone_id: remote_clone_id,
            character_id: char_id,
            location_id: jita_station,
            implants: vec![],
        });
        // Insert active Zarzakh gate lock
        s.zarzakh_gate_locks.insert(char_id, (TURNUR_GATE_IN_ZARZAKH, ctx.now_unix() + 21_600));
    });

    sess.solar_system_id = Some(ZARZAKH_SYS);
    sess.station_id = Some(fulcrum_station);
    sess.ship_id = Some(capsule_id);
    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // Verify lock is currently active
    assert!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

    // Perform CloneJump to Jita station
    let cj_res = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(remote_clone_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump to Jita");
    assert!(matches!(cj_res, PyRep::KeyVal(_)));

    // Assert Zarzakh gate lock has been CLEARED
    assert!(
        !ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)),
        "Zarzakh gate lock must be cleared on clone jump"
    );

    // Assert Quasar gateway delivered DeactivatedNotice for stargate lock
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| {
            p.type_url.ends_with("stargate.lock.api.DeactivatedNotice")
        })),
        "DeactivatedNotice must be delivered upon clone jumping out of Zarzakh"
    );
}

#[tokio::test]
async fn test_zarzakh_gate_lock_clearing_on_pod_death() {
    // parity: feat:navigation.zarzakh-gate-lock
    let ctx = ServerContext::new();
    let char_id = 91_000_103_i64;
    let capsule_id = 101_000_103_i64;
    let jita_station = 60_003_760_i64;

    let mut sess = SessionState::new(884_103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = "Zarzakh Pod Death Pilot".into();
        ch.solar_system_id = ZARZAKH_SYS;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = capsule_id;
        ch.balance = IskCents::from_isk(10_000_000);
        ch.war_faction_id = None;
        ch.home_station_id = jita_station;
        s.characters.insert(char_id, ch);

        s.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
                type_id: 670, // Capsule
                owner_id: char_id,
                location_id: ZARZAKH_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Insert active Zarzakh gate lock
        s.zarzakh_gate_locks.insert(char_id, (TURNUR_GATE_IN_ZARZAKH, ctx.now_unix() + 21_600));
    });

    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(ZARZAKH_SYS);
    sess.ship_id = Some(capsule_id);
    ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), ctx.now_unix());

    // Verify lock is currently active
    assert!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

    // Execute pod death in Zarzakh
    let death_res = execute_death_pipeline_sync(
        &ctx,
        ZARZAKH_SYS,
        char_id,
        capsule_id,
        0,
        DVec3::ZERO,
        1,
    );
    assert!(death_res.is_ok(), "execute_death_pipeline_sync on capsule succeeded");

    // Assert Zarzakh gate lock has been CLEARED
    assert!(
        !ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)),
        "Zarzakh gate lock must be cleared on pod death"
    );

    // Assert Quasar gateway delivered DeactivatedNotice for stargate lock
    let notices = ctx.quasar_gateway.poll_notices(sess.session_id);
    assert!(
        notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| {
            p.type_url.ends_with("stargate.lock.api.DeactivatedNotice")
        })),
        "DeactivatedNotice must be delivered upon pod death in Zarzakh"
    );
}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod navigation_warp_zarzakh_ext {
    use super::*;

// Integration tests for Space Navigation: Supercap Docking and Zarzakh Capital Gate Ban
// closing 2 canonical plan rows under `feat:navigation.*`.


use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const TURNUR_SYSTEM: u32 = 30_002_086;
const SOLAR_SYSTEM_ZARZAKH: u32 = 30_100_000;
const PILOT_1: i64 = 93_100_001;
const SHIP_1: i64 = 83_100_001;
const TURNUR_ZARZAKH_GATE: i64 = 50_016_562;

async fn setup_nav_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("NavPilot {char_id}"),
                corporation_id: 1_000_001,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(500_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 5.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes::default(),
                skill_points: 15_000_000.0,
                activity_status: None,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let mut sess = SessionState::new(8_000 + char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_001);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

#[tokio::test]
async fn test_space_navigation_warp_landing_supercap_docking_and_zarzakh_gates() {
    // parity: feat:navigation.npc-station-supercap-docking
    // parity: feat:navigation.zarzakh-capital-gate-ban
    eve_testkit::attest(&[
        "feat:navigation.npc-station-supercap-docking",
        "feat:navigation.zarzakh-capital-gate-ban",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let lowsec_sys = 30_000_012u32; // Asabona (low-sec, security 0.323 < 0.45)
    let npc_station_id = 60_012_292i64; // Asabona low-sec NPC station
    let lowsec_sec = ctx.sde.solar_systems[&lowsec_sys].security_status;
    assert!(lowsec_sec > 0.0 && lowsec_sec < 0.45, "Expected low-sec system, got {lowsec_sec}");
    let station_pos = ctx.sde.stations[&npc_station_id].position;
    let mut sess =
        setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 22440, lowsec_sys, station_pos).await;

    // =========================================================================
    // 1. feat:navigation.npc-station-supercap-docking
    // Checklist:
    // [x] Clause 1: Supercarriers (group 659) and Titans (group 30) refused docking at NPC stations via V24 beyonce.CmdDock (DockingRequestDenied, dialog_id 808)
    // [x] Clause 2: Standard subcapital and non-supercapital capital ships (in low/null-sec) permitted to dock
    // =========================================================================
    let client_dialogs = ctx.sde.load_dialogs().expect("load V24.01 dialogs.static");
    assert_eq!(client_dialogs["DockingRequestDenied"].dialog_id, 808);
    assert_eq!(client_dialogs["ShipCannotUseStargate"].dialog_id, 7882);
    let bid_lowsec = ctx
        .bind_object(&mut sess, "beyonce", lowsec_sys as i64)
        .expect("bind beyonce");

    // Negative 1: Titan (type 11567, group 30) refused via V24 beyonce.CmdDock(stationID, shipID)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 11567;
    });
    let titan_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_lowsec),
            "CmdDock",
            &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
            &kw,
            None,
        )
        .await;
    let titan_err = titan_dock.unwrap_err().code;
    assert_eq!(titan_err, "DockingRequestDenied", "Titan docking at NPC station must be refused");

    // Negative 2: Supercarrier (type 23919, group 659) refused via V24 beyonce.CmdDock(stationID, shipID)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 23919;
    });
    let super_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_lowsec),
            "CmdDock",
            &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
            &kw,
            None,
        )
        .await;
    let super_err = super_dock.unwrap_err().code;
    assert_eq!(super_err, "DockingRequestDenied", "Supercarrier docking at NPC station must be refused");

    // Positive 1: Standard subcapital ship (Dominix 645, group 27 Battleship) docks successfully
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
    });
    let subcap_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_lowsec),
            "CmdDock",
            &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
            &kw,
            None,
        )
        .await;
    assert!(
        subcap_dock.is_ok(),
        "Subcapital ship docks successfully at NPC station: {subcap_dock:?}"
    );
    assert_eq!(sess.station_id, Some(npc_station_id));

    // Undock back to space
    sess.station_id = None;
    let bid_lowsec = ctx.bind_object(&sess, "beyonce", lowsec_sys as i64).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });

    // Positive 2: Non-supercapital capital ship (Archon Carrier group 547, type 23757) docks at low-sec NPC station
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 23757;
    });
    let sys_low = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = sys_low
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 23757,
            position: station_pos,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 2.0,
            mass: 1_200_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let carrier_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_lowsec),
            "CmdDock",
            &[PyRep::Integer(npc_station_id), PyRep::Integer(SHIP_1)],
            &kw,
            None,
        )
        .await;
    assert!(
        carrier_dock.is_ok(),
        "Archon carrier docks successfully at low-sec NPC station: {carrier_dock:?}"
    );
    assert_eq!(sess.station_id, Some(npc_station_id));

    // Undock back to space
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });

    // =========================================================================
    // 2. feat:navigation.zarzakh-capital-gate-ban
    // Checklist:
    // [x] Clause 1: Capital hulls refused at every stargate into Zarzakh (ShipCannotUseStargate, dialogs.static dialog_id 7882)
    // [x] Clause 2: Freighters (including Bowhead) refused at every stargate into Zarzakh
    // [x] Clause 3: Orca is exempt and allowed through Zarzakh stargates
    // [x] Clause 4: Subcapitals allowed through Zarzakh stargates
    // Abuse / Negative Tests:
    // - Negative 1: Dreadnought jumping into Zarzakh -> rejected (ShipCannotUseStargate)
    // - Negative 2: Standard freighter (Charon) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
    // - Negative 3: Bowhead freighter (type 34328) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
    // - Negative 4: Jump freighter (Anshar) jumping into Zarzakh -> rejected (ShipCannotUseStargate)
    // - Positive 1: Orca (type 28606, group 941) jumping into Zarzakh -> allowed!
    // - Positive 2: Standard subcapital ship jumping into Zarzakh -> allowed!
    // =========================================================================

    // Move pilot to Turnur at Turnur-Zarzakh stargate
    sess.solar_system_id = Some(TURNUR_SYSTEM);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = TURNUR_SYSTEM;
            ch.station_id = None;
        }
    });
    let gate_jump = &ctx.sde.jumps[&TURNUR_ZARZAKH_GATE];
    let gate_pos = gate_jump.position;

    let sys_turnur = ctx.systems.get_or_spawn(TURNUR_SYSTEM);
    let _ = sys_turnur
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 645,
            position: gate_pos + DVec3::new(500.0, 0.0, 0.0), // Within 2,500m of stargate
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let bid_turnur = ctx
        .bind_object(&mut sess, "beyonce", TURNUR_SYSTEM as i64)
        .expect("bind turnur beyonce");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    // Negative 1: Dreadnought (Revelation 19720, group 485)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 19720;
    });
    if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
        b.type_id = 19720;
        let _ = sys_turnur.spawn_ball(b).await;
    }
    let dread_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    let dread_err = dread_jump.unwrap_err().code;
    assert_eq!(dread_err, "ShipCannotUseStargate", "Dreadnought jumping into Zarzakh must be rejected");

    // Negative 2: Standard Freighter (Charon 20185, group 513)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 20185;
    });
    if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
        b.type_id = 20185;
        let _ = sys_turnur.spawn_ball(b).await;
    }
    let freighter_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    let freighter_err = freighter_jump.unwrap_err().code;
    assert_eq!(freighter_err, "ShipCannotUseStargate", "Freighter jumping into Zarzakh must be rejected");

    // Negative 3: Bowhead Freighter (Bowhead 34328, group 513)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 34328;
    });
    if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
        b.type_id = 34328;
        let _ = sys_turnur.spawn_ball(b).await;
    }
    let bowhead_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    let bowhead_err = bowhead_jump.unwrap_err().code;
    assert_eq!(bowhead_err, "ShipCannotUseStargate", "Bowhead freighter jumping into Zarzakh must be rejected");

    // Negative 4: Jump freighter (Anshar 28848, group 902) jumping into Zarzakh -> rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28848;
    });
    if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
        b.type_id = 28848;
        let _ = sys_turnur.spawn_ball(b).await;
    }
    let anshar_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    let anshar_err = anshar_jump.unwrap_err().code;
    assert_eq!(anshar_err, "ShipCannotUseStargate", "Anshar jump freighter jumping into Zarzakh must be rejected");

    // Positive 1: Orca (type 28606, group 941) is exempt and allowed into Zarzakh!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28606;
    });
    if let Ok(Some((_, mut b))) = sys_turnur.get_ball_snapshot(SHIP_1).await {
        b.type_id = 28606;
        let _ = sys_turnur.spawn_ball(b).await;
    }
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let orca_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    assert!(
        orca_jump.is_ok(),
        "Orca is exempt from capital ban and allowed through Zarzakh stargate: {orca_jump:?}"
    );
    assert_eq!(sess.solar_system_id, Some(SOLAR_SYSTEM_ZARZAKH));

    // Reset session back to Turnur for subcapital test
    sess.solar_system_id = Some(TURNUR_SYSTEM);
    let bid_turnur = ctx.bind_object(&sess, "beyonce", TURNUR_SYSTEM as i64).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = TURNUR_SYSTEM;
        }
    });

    // Positive 2: Standard subcapital ship (Dominix 645) jumping into Zarzakh -> allowed!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
    });
    let _ = sys_turnur
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 645,
            position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let subcap_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid_turnur),
            "CmdStargateJump",
            &[PyRep::Integer(TURNUR_ZARZAKH_GATE)],
            &kw,
            None,
        )
        .await;
    assert!(
        subcap_jump.is_ok(),
        "Standard subcapital is allowed through Zarzakh stargate: {subcap_jump:?}"
    );
    assert_eq!(sess.solar_system_id, Some(SOLAR_SYSTEM_ZARZAKH));
}

}
