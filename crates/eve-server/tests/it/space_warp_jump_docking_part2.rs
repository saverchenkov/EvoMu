//! Integration tests for `SP-3` (Warp Navigation, Disruption, Capacitor Cost, & Exit Invulnerability).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp3_warp_to_disrupted_capacitor_cost_and_exit_invulnerability() {
    // parity: plan:SP-3
    // parity: feat:navigation.warp-to, feat:navigation.warp-disrupted, feat:navigation.warp-capacitor-cost, feat:navigation.warp-exit-invulnerability, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.warp-to", "feat:navigation.warp-disrupted", "feat:navigation.warp-capacitor-cost", "feat:navigation.warp-exit-invulnerability"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            89_001_001,
            InventoryItemRecord {
                item_id: 89_001_001,
                type_id: 53977,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

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
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Warp Disruption & Interdiction Bubble checks (`feat:navigation.warp-disrupted`)
    let station_pos = ctx.sde.stations[&60003760].position;
    let start_pos = station_pos - DVec3::new(10_000_000.0, 0.0, 0.0);
    sys_handle.set_ball_position(ship_id, start_pos).await;

    let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.warp_scramble_strength = 2;
    cs.warp_core_strength = 0;
    sys_handle.set_combat_state(ship_id, cs.clone()).await.unwrap();

    let scram_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(20_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(scram_err.code, "WarpScrambled");

    cs.warp_scramble_strength = 0;
    sys_handle.set_combat_state(ship_id, cs.clone()).await.unwrap();
    let bubble_id = 920_001;
    sys_handle
        .add_warp_bubble(bubble_id, start_pos, 20_000.0)
        .await
        .expect("add warp bubble");

    let bubble_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(20_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bubble_err.code, "WarpScrambled");
    sys_handle
        .remove_warp_bubble(bubble_id)
        .await
        .expect("remove warp bubble");

    // 2. Warp Capacitor Cost & Partial Warp (`feat:navigation.warp-capacitor-cost`)
    let mut low_cap_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let full_cap = low_cap_cs.max_capacitor;
    low_cap_cs.capacitor = 0.000_001;
    sys_handle
        .set_combat_state(ship_id, low_cap_cs.clone())
        .await
        .unwrap();

    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");

    let far_warp_start = station_pos - DVec3::new(2.0 * eve_destiny::AU_METERS, 0.0, 0.0);
    sys_handle.set_ball_position(ship_id, far_warp_start).await;
    let (_, ship_b) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let warp_cap_need = ctx
        .sde
        .types
        .get(&ship_b.type_id)
        .and_then(|t| t.default_attributes.get(&153).copied())
        .unwrap_or(1e-6);
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level)
            .unwrap_or(0)
    });
    let eff_warp_cap_need = warp_cap_need * (1.0 - 0.10 * (wdo_lvl.min(5) as f64));
    let full_2au_cost = eff_warp_cap_need * ship_b.mass * 2.0;
    low_cap_cs.capacitor = full_2au_cost * 0.5;
    sys_handle
        .set_combat_state(ship_id, low_cap_cs.clone())
        .await
        .unwrap();

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(60003760),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Partial warp with 50% capacitor");

    let (_, partial_warp_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let partial_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(partial_cs.capacitor < 1e-6);
    let MovementMode::Warp {
        destination: partial_dest,
        total_distance: partial_dist,
        ..
    } = partial_warp_ball.mode
    else {
        panic!("Expected MovementMode::Warp for partial warp");
    };
    assert!((partial_dist - eve_destiny::AU_METERS).abs() < 1e6);
    assert!(
        partial_dest.distance(far_warp_start + DVec3::new(eve_destiny::AU_METERS, 0.0, 0.0)) < 1e6
    );
    let cap_notes = sess.drain_notifications();
    assert!(cap_notes.iter().any(|n| n.0 == "OnCapacitorChange"));
    assert!(cap_notes.iter().any(|n| n.0 == "OnWarpStarted"));

    // 3. Full Warp with minRange (30 km), phases Accelerating -> Cruising -> Decelerating, smartbomb hit in Decelerating,
    //    and 10s Post-Warp Invulnerability (`feat:navigation.warp-to`, `feat:navigation.warp-exit-invulnerability`)
    let mut full_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    full_cs.capacitor = full_cap;
    sys_handle.set_combat_state(ship_id, full_cs).await.unwrap();
    let (_, mut reset_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    reset_ball.position = start_pos;
    reset_ball.velocity = DVec3::ZERO;
    reset_ball.speed_fraction = 0.0;
    reset_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(reset_ball).await.unwrap();

    let min_range = 30_000.0;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(60003760),
            PyRep::Real(min_range),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff at 30 km minRange");

    let expected_landing = station_pos - DVec3::new(min_range, 0.0, 0.0);
    let mut saw_accel = false;
    let mut saw_decel_and_tested_smartbomb = false;
    for _ in 0..40 {
        sys_handle.step_manual(1.0).await.expect("step warp progression");
        let (_, b_w) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        match b_w.mode {
            MovementMode::Warp {
                phase: eve_destiny::WarpPhase::Accelerating,
                ..
            } => {
                saw_accel = true;
            }
            MovementMode::Warp {
                phase: eve_destiny::WarpPhase::Decelerating,
                ..
            } => {
                if !saw_decel_and_tested_smartbomb {
                    saw_decel_and_tested_smartbomb = true;
                    let cs_pre_sb = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
                    sys_handle
                        .apply_damage(
                            ship_id,
                            eve_destiny::DamageVector {
                                em: 50.0,
                                thermal: 0.0,
                                kinetic: 0.0,
                                explosive: 0.0,
                            },
                        )
                        .await
                        .unwrap();
                    let cs_post_sb = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
                    assert!(cs_post_sb.shield_hp < cs_pre_sb.shield_hp);
                }
            }
            MovementMode::Stop => break,
            _ => {}
        }
    }
    assert!(saw_accel);
    assert!(saw_decel_and_tested_smartbomb);

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .expect("UpdateStateRequest after warp");
    let fin_notes = sess.drain_notifications();
    assert!(fin_notes.iter().any(|n| n.0 == "OnWarpFinished"));

    let (_, landed_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(landed_ball.mode, MovementMode::Stop);
    assert!(landed_ball.position.distance(expected_landing) <= 2_500.0);

    let post_warp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(post_warp_cs.warp_exit_invulnerable_remaining_ticks, 10);

    let hp_at_warp_exit = post_warp_cs.shield_hp;
    sys_handle
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 200.0,
                thermal: 200.0,
                kinetic: 200.0,
                explosive: 200.0,
            },
        )
        .await
        .unwrap();
    let cs_after_invuln_hit = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_invuln_hit.shield_hp, hp_at_warp_exit);

    let attacker_ball_id = 920_099;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: attacker_ball_id,
                type_id: 587,
                position: landed_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
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
    assert!(
        sys_handle
            .add_target_lock(
                attacker_ball_id,
                eve_destiny::TargetLock {
                    target_id: ship_id,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 1_000.0,
                    locked: false,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .is_err()
    );
    let attacker_locks = sys_handle.get_target_locks(attacker_ball_id).await.unwrap();
    assert!(attacker_locks.is_empty());

    ctx.advance_secs(12);
    let fil_err = ctx
        .dispatch_rpc(
            &mut sess,
            "randomJumpMgr",
            None,
            "ActivateRandomJumpFilament",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(fil_err.code, "ShipInvulnerable");

    let abyss_unimpl = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(abyss_unimpl.code, "UnimplementedRPC");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction breaks post-warp invulnerability");
    let cs_after_move = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_move.warp_exit_invulnerable_remaining_ticks, 0);
}
