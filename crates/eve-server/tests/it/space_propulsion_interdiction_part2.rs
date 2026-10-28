//! Integration tests for `SP-3` (Propulsion Modules, Agility, MJD/MJFG/Capital MJD, & Approach) — Part 2.

use crate::common;
use crate::space_propulsion_interdiction::{grant_all_test_skills, make_item};

use eve_db::types::InventoryFlag;
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp3_propulsion_agility_mjd_mjfg_and_approach() {
    // parity: plan:SP-3
    // parity: feat:navigation.approach, feat:dogma.velocity-agility, feat:modules.afterburner, feat:modules.microwarpdrive, feat:modules.micro-jump-drive, feat:modules.micro-jump-field-generator, feat:modules.micro-jump-field-boosh-limit, feat:modules.mjfg-fleet-jump, feat:modules.capital-micro-jump-drive, feat:navigation.directional-jump-drive, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:SP-3", "feat:navigation.approach", "feat:dogma.velocity-agility", "feat:modules.afterburner", "feat:modules.microwarpdrive", "feat:modules.micro-jump-drive", "feat:modules.micro-jump-field-generator", "feat:modules.micro-jump-field-boosh-limit", "feat:modules.mjfg-fleet-jump", "feat:modules.capital-micro-jump-drive", "feat:navigation.directional-jump-drive"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(2002);
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

    let ab_item_id = 60_000_201;
    let mwd_item_id = 60_000_202;
    let mjd_item_id = 60_000_203;
    grant_all_test_skills(&ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.get_mut(&ship_id).unwrap().type_id = 638; // Raven (Battleship, group 27)
        for item in [
            make_item(ab_item_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1, true),
            make_item(mwd_item_id, 440, char_id, ship_id, InventoryFlag::MedSlot1, 1, true),
            make_item(mjd_item_id, 4383, char_id, ship_id, InventoryFlag::MedSlot2, 1, true),
        ] {
            snap.items.insert(item.item_id, item);
        }
        snap.module_online_states.insert(mwd_item_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);

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

    // 1. Subwarp Velocity & Agility (`feat:dogma.velocity-agility`)
    let (_, mut base_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let base_vmax = base_ball.max_velocity;
    let base_mass = base_ball.mass;
    let base_inertia = base_ball.inertia;
    let base_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let base_sig = base_cs.signature_radius;

    let fit_eval = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .evaluated_context(&ctx.sde);
    let base_cap = fit_eval.ship.get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde);
    assert!(
        (base_vmax - fit_eval.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde)).abs() < 1e-3
    );
    base_ball.velocity = DVec3::ZERO;
    base_ball.speed_fraction = 0.0;
    base_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball_with_combat(base_ball, base_cs.clone()).await.unwrap();
    let tau = (base_inertia * base_mass) / 1_000_000.0;
    let align_ticks = (4.0_f64.ln() * tau).ceil() as usize;
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None).await.unwrap();
    for t in 1..=align_ticks {
        sys_handle.step_manual(1.0).await.unwrap();
        let (_, b_step) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let expected_v = base_vmax * (1.0 - (-(t as f64) / tau).exp());
        assert!((b_step.velocity.length() - expected_v).abs() < 1.0);
        if t < align_ticks {
            assert!(b_step.velocity.length() < 0.75 * base_vmax);
        } else {
            assert!(b_step.velocity.length() >= 0.75 * base_vmax);
        }
    }

    // 2. Approach (`feat:navigation.approach`)
    let target_ball_id = 89_001;
    let (_, cur_ship) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let target_pos = cur_ship.position + DVec3::new(15_000.0, 0.0, 0.0);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_ball_id,
                type_id: 587,
                position: target_pos,
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
        .expect("spawn approach target");

    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdFollowBall", &[PyRep::Integer(target_ball_id), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("CmdFollowBall");
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .expect("CmdSetSpeedFraction");

    let dist_before = cur_ship.position.distance(target_pos);
    for _ in 0..5 {
        sys_handle.step_manual(1.0).await.expect("step_manual");
    }
    let (_, ship_after_approach) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ship_after_approach.position.distance(target_pos) < dist_before);

    // 3. Afterburner (`feat:modules.afterburner`)
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_item_id)], &kw, None)
        .await
        .expect("Activate Afterburner");
    let (_, ab_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let ab_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(ab_ball.max_velocity > base_vmax);
    assert_eq!(ab_ball.mass, base_mass + 500_000.0);
    assert!((ab_cs.signature_radius - base_sig).abs() < 1e-3);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_item_id)], &kw, None)
        .await
        .expect("Deactivate Afterburner");

    // 4. Microwarpdrive (`feat:modules.microwarpdrive`) — swap online state from AB to MWD (maxGroupOnline = 1)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ab_item_id, false);
        snap.module_online_states.insert(mwd_item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mwd_fit_eval = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .evaluated_context(&ctx.sde);
    assert!(
        mwd_fit_eval.ship.get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde) < base_cap,
        "Online Microwarpdrive must apply passive capacitor capacity penalty"
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_item_id)], &kw, None)
        .await
        .expect("Activate MWD");
    let (_, mwd_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let mwd_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(mwd_ball.max_velocity > ab_ball.max_velocity);
    assert_eq!(mwd_ball.mass, base_mass + 500_000.0);
    assert!(mwd_cs.signature_radius > base_sig * 4.0);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mwd_item_id)], &kw, None)
        .await
        .expect("Deactivate MWD");

    // Apply an active Warp Scrambler tackle cycle on ship_id (2 scramble points + MWD/MJD suppression)
    sys_handle
        .add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
            ship_id: target_ball_id,
            module_id: 999_001,
            module_type_id: 447,
            effect_name: "warpScrambleBlockMWDWithNPCEffect".into(),
            target_id: Some(ship_id),
            repeat: 1,
            duration_s: 60.0,
            rack_idx: 1,
            warp_scramble_points: 2,
            disables_target_mwd_mjd: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let mwd_scram_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_item_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(mwd_scram_err.code, "WarpScrambled");

    // 5. Micro Jump Drive (`feat:modules.micro-jump-drive`, `feat:navigation.directional-jump-drive`)
    let mjd_scram_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_item_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(mjd_scram_err.code, "WarpScrambled");

    // Afterburner is NOT disabled by warp scramblers
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_item_id, false);
        snap.module_online_states.insert(ab_item_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_item_id)], &kw, None)
        .await
        .expect("Afterburner must activate even while warp scrambled");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_item_id)], &kw, None)
        .await
        .expect("Deactivate Afterburner");

    sys_handle.remove_module_cycle(target_ball_id, 999_001).await.unwrap();

    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("Set heading +X");
    sys_handle.step_manual(1.0).await.unwrap();
    let (_, ball_before_mjd) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let pos_before = ball_before_mjd.position;

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_item_id)], &kw, None)
        .await
        .expect("Activate MJD");

    for _ in 0..5 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, ball_mid_mjd) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_mid_mjd = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(ball_mid_mjd.position.distance(pos_before) < 5_000.0, "MJD must not jump before 12s spool-up completes");
    assert!(cs_mid_mjd.signature_radius > base_sig * 2.0, "MJD spool-up must bloom signature radius");
    for _ in 0..7 {
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let (_, ball_after_mjd) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((ball_after_mjd.position.distance(pos_before) - 100_000.0).abs() < 2_500.0);

    // 6. Micro Jump Field Generator, 25-ship Boosh Limit, Fleet Jump, & Capital MJD
    // (`feat:modules.micro-jump-field-generator`, `feat:modules.micro-jump-field-boosh-limit`, `feat:modules.mjfg-fleet-jump`, `feat:modules.capital-micro-jump-drive`)
    let mut park = Ballpark::new(30_004_608);
    let bifrost_id = 500_001;
    let origin = DVec3::new(1_000_000.0, 0.0, 0.0);
    park.add_ball(Ball {
        id: bifrost_id,
        type_id: 37480, // Bifrost (Command Destroyer)
        position: origin,
        velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 65.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });

    // Spawn 30 eligible ships within 6 km, plus 1 scrambled ship and 1 cloaked ship
    for i in 0..30 {
        let bid = 501_000 + i;
        park.add_ball(Ball {
            id: bid,
            type_id: 587,
            position: origin + DVec3::new((i as f64) * 150.0, 100.0, 0.0), // 0..4,350 m (<= 6,000 m)
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        });
    }
    let scrammed_victim_id = 502_001;
    park.add_ball(Ball {
        id: scrammed_victim_id,
        type_id: 587,
        position: origin + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states
        .get_mut(&scrammed_victim_id)
        .unwrap()
        .is_mwd_mjd_disabled = true;

    // Trigger Command Destroyer 9s MJFG spool-up via capacitor drop on Bifrost (37480)
    park.combat_states.get_mut(&bifrost_id).unwrap().last_step_capacitor = Some(250.0);
    park.combat_states.get_mut(&bifrost_id).unwrap().capacitor = 150.0;
    let base_bifrost_sig = park.combat_states[&bifrost_id].signature_radius;

    park.step(1.0);
    assert!(
        (park.combat_states[&bifrost_id].signature_radius - base_bifrost_sig * 2.5).abs() < 1e-3,
        "MJFG spool-up must apply +150% signature radius bloom (2.5x)"
    );
    for _ in 0..8 {
        park.step(1.0);
    }

    // Verify Bifrost + exactly 25 closest eligible ships booshed 100 km, while ships 25..30 and scrambled ship stayed behind
    assert!(
        park.balls[&bifrost_id].position.x > origin.x + 99_000.0,
        "Bifrost must translate 100 km along +X upon completing 9s MJFG spool-up"
    );
    let booshed_count = (0..30)
        .filter(|&i| park.balls[&(501_000 + i)].position.x > origin.x + 90_000.0)
        .count();
    assert_eq!(
        1 + booshed_count,
        25,
        "Command Destroyer MJFG must boosh at most 25 total ships within 6 km"
    );
    assert!(
        park.balls[&scrammed_victim_id].position.x < origin.x + 5_000.0,
        "Warp-scrambled ships must NOT be moved by MJFG boosh"
    );

    // Test Capital Micro Jump Drive (250 km jump for capital hull 19720 Revelation) & Capital MJFG fleet jump (250 km / 10 km / 50 ships)
    let dread_id = 503_001;
    let fleet_ally_id = 503_002;
    park.add_ball(Ball {
        id: dread_id,
        type_id: 19720, // Revelation (Dreadnought)
        position: origin,
        velocity: DVec3::new(50.0, 0.0, 0.0),
        max_velocity: 80.0,
        inertia: 0.1,
        mass: 1_200_000_000.0,
        radius: 1_200.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    park.add_ball(Ball {
        id: fleet_ally_id,
        type_id: 621,
        position: origin + DVec3::new(8_500.0, 0.0, 0.0), // 8.5 km (within 10 km Capital MJFG radius)
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 2.0,
        mass: 10_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // Capital MJD translates capital ship 250,000 m
    let cap_mjd_pos = park.micro_jump(dread_id, 100_000.0).expect("Capital MJD jump");
    assert!(
        (cap_mjd_pos.distance(origin) - 250_000.0).abs() < 1.0,
        "Capital Micro Jump Drive must jump 250,000 m along ship heading"
    );

    // Reset dread to origin and run Capital MJFG (250 km jump, 10 km field, up to 50 subcapitals)
    park.set_ball_position(dread_id, origin);
    let moved_fleet = park
        .micro_jump_field(dread_id, 250_000.0, 10_000.0, 50)
        .expect("Capital MJFG fleet jump");
    assert!(
        moved_fleet.contains(&dread_id) && moved_fleet.contains(&fleet_ally_id),
        "Capital MJFG must jump the capital ship and subcapitals within 10 km by 250 km"
    );
    assert!((park.balls[&fleet_ally_id].position.x - (origin.x + 8_500.0 + 250_000.0)).abs() < 1.0);
}
