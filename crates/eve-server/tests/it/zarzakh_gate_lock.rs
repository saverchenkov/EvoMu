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
