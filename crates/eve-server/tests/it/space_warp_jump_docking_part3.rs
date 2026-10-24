//! Integration tests for `SP-4` (Stargate Capital Restrictions, Session-Change Timers, & Jump Cloak Immunities).

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp4_stargate_capital_restriction_session_timers_and_jump_cloak_immunities() {
    // parity: plan:SP-4
    // parity: feat:navigation.stargate-capital-restriction, feat:navigation.jump-cloak-immunities, rpc:beyonce.SetSessionCheck, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.stargate-capital-restriction", "feat:navigation.jump-cloak-immunities"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_sys = 30_000_142;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);

    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .cloned()
        .expect("Jita stargate");
    let gate_id = jita_gate.stargate_id;
    let gate_pos = jita_gate.position;

    jita_handle
        .set_ball_position(ship_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    // 1. Stargate Capital Ship Restrictions (`feat:navigation.stargate-capital-restriction`)
    for &(cap_type_id, expected_group_id) in &[
        (23757_u32, 547_u32),  // Archon (Carrier)
        (19720_u32, 485_u32),  // Revelation (Dreadnought)
        (37604_u32, 1538_u32), // Apostle (Force Auxiliary)
        (23919_u32, 659_u32),  // Aeon (Supercarrier)
        (11567_u32, 30_u32),   // Avatar (Titan)
        (77281_u32, 4594_u32), // Bane (Lancer Dreadnought)
        (28352_u32, 883_u32),  // Rorqual (Capital Industrial Ship)
    ] {
        assert_eq!(ctx.sde.types[&cap_type_id].group_id, expected_group_id);
        let (_, mut b) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        b.type_id = cap_type_id;
        jita_handle.spawn_ball(b).await.unwrap();

        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "CapitalShipCannotJumpStargate");
    }

    for &(allowed_type_id, expected_group_id) in &[
        (20183_u32, 513_u32), // Providence (Freighter)
        (28844_u32, 902_u32), // Ark (Jump Freighter)
        (28606_u32, 941_u32), // Orca (Industrial Command Ship)
        (34328_u32, 513_u32), // Bowhead (Freighter)
    ] {
        assert_eq!(ctx.sde.types[&allowed_type_id].group_id, expected_group_id);
    }

    let (_, mut orca_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    orca_ball.type_id = 28606; // Orca
    jita_handle.spawn_ball(orca_ball).await.unwrap();

    let jump_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Orca (28606) must be allowed to jump normal stargates");
    let PyRep::KeyVal(jkv) = jump_ok else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let dest_sys_id = jkv.get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let dest_beyonce_id = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(dest_sys_id, jita_gate.to_solar_system_id);
    assert_eq!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap(), 0.0);
    assert_eq!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap(), 0.0);

    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);

    // 2. Post-Jump Cloak (60s) Immunities (`feat:navigation.jump-cloak-immunities`)
    let dest_cs = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(dest_cs.is_cloaked);
    assert_eq!(dest_cs.gate_cloak_remaining_ticks, 60);

    let shield_before_bomb = dest_cs.shield_hp;
    dest_handle
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap();
    let dest_cs_after_bomb = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(dest_cs_after_bomb.shield_hp, shield_before_bomb);

    let (_, jumped_ball) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let ecm_source_id = 930_001;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: ecm_source_id,
                type_id: 587,
                position: jumped_ball.position + DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();
    let jammed_ids = dest_handle
        .apply_ecm_burst(
            ecm_source_id,
            5_000.0,
            [100.0, 100.0, 100.0, 100.0],
            1.0,
            0.0,
        )
        .await
        .unwrap();
    assert!(!jammed_ids.contains(&ship_id));

    let bumper_id = 930_002;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: bumper_id,
                type_id: 642,
                position: jumped_ball.position + DVec3::new(10.0, 0.0, 0.0),
                velocity: DVec3::new(-500.0, 0.0, 0.0),
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 200.0,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::NEG_X,
                },
            },
            ctx.build_type_combat_state(642),
        )
        .await
        .unwrap();
    dest_handle
        .step_manual(1.0)
        .await
        .expect("step with overlapping bumper");
    let (_, jumped_ball_after_bump) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_after_bump = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_bump.is_cloaked && cs_after_bump.gate_cloak_remaining_ticks == 59);
    assert_eq!(jumped_ball_after_bump.velocity, DVec3::ZERO);
    let _ = dest_handle.remove_ball(bumper_id).await;
    let _ = dest_handle.remove_ball(ecm_source_id).await;

    // 3. 10s Session-Change Timer Triggers (`feat:navigation.session-change-timer-triggers`)
    let return_gate_id = jita_gate.destination_stargate_id;
    let return_gate_pos = ctx.sde.jumps[&return_gate_id].position;
    dest_handle
        .set_ball_position(ship_id, return_gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    let early_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(early_jump_err.code, "SessionChangeInProgress");

    for _ in 0..10 {
        dest_handle.step_manual(1.0).await.unwrap();
    }

    for trigger in [
        "login",
        "wormhole_jump",
        "jump_bridge",
        "jump_drive",
        "dock",
        "undock",
        "fleet_join",
        "fleet_position_change",
        "ship_board",
        "ship_eject",
        "pod_transition",
        "clone_jump",
        "clone_swap",
        "corp_join",
        "corp_leave",
        "wallet_division_change",
    ] {
        let trig_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("SetSessionCheck({trigger}) failed: {e:?}"));
        let PyRep::KeyVal(tkv) = trig_res else {
            panic!("Expected KeyVal from SetSessionCheck");
        };
        assert_eq!(
            tkv.get("sessionChangeTimerSeconds")
                .unwrap()
                .as_i64()
                .unwrap(),
            10
        );

        let blocked = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked.code, "SessionChangeInProgress");

        for _ in 0..10 {
            dest_handle.step_manual(1.0).await.unwrap();
        }
    }

    let jump_back = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Jump back to Jita after 10s session timer elapsed");
    let PyRep::KeyVal(jb_kv) = jump_back else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let jita_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let cs_cloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked && cs_cloaked.gate_cloak_remaining_ticks == 60);

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_b_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("Move ship to break gate cloak");
    let cs_uncloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_uncloaked.is_cloaked && cs_uncloaked.gate_cloak_remaining_ticks == 0);
}
