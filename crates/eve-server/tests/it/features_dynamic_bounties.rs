//! Dedicated integration tests for open `feat:navigation.*` parity rows (Batch 35, Part 3).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::features_bounties_ess::{give_item, set_skill, spawn_ship_in_space};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

const ZARZAKH_SYS: u32 = 30_100_000;

#[tokio::test]
async fn test_b35_10_align_server_tick_rounding() {
    // parity: feat:navigation.align-server-tick-rounding
    eve_testkit::attest(&["feat:navigation.align-server-tick-rounding"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2): unauthenticated & invalid-target CmdWarpToStuff rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40301369)], &kw, None).await.is_err());

    let target_celestial_id = 40301369; // 1DQ1-A I planet in NULL_SYS

    // Three stationary Executioners (589) spawned via real Dogma fitting (`ensure_player_ship_ball_spawned`)
    // and commanded to warp from rest via live `beyonce.CmdWarpToStuff`:
    //   Case 0: SC IV, EM V, 2x Inertial Stabilizers II -> ~1.964s (< 2.0s) -> enters warp on tick 2!
    //   Case 1: SC III, EM V, 2x Inertial Stabilizers II -> ~2.007s (> 2.0s) -> enters warp on tick 3!
    //   Case 2: SC V, EM IV, 0x Inertial Stabilizers II  -> ~3.101s (> 3.0s) -> enters warp on tick 4!
    for (idx, sc_lvl, em_lvl, n_istab, expected_ticks) in [
        (0i64, 4u8, 5u8, 2usize, 2u64),
        (1, 3, 5, 2, 3),
        (2, 5, 4, 0, 4),
    ] {
        let cid = 953_100 + idx;
        let sid = 963_100 + idx;
        let mut sess = seed_pilot(&ctx, cid, sid, 589, NULL_SYS, false);
        set_skill(&ctx, cid, 3331, 5); // Amarr Frigate
        set_skill(&ctx, cid, 3394, 5); // Hull Upgrades
        set_skill(&ctx, cid, 3426, 5); // CPU Management
        set_skill(&ctx, cid, 3413, 5); // Power Grid Management
        set_skill(&ctx, cid, 3327, sc_lvl); // Spaceship Command
        set_skill(&ctx, cid, 3453, em_lvl); // Evasive Maneuvering
        if n_istab >= 1 {
            give_item(&ctx, 973_110 + idx * 4, 1405, cid, sid, InventoryFlag::LoSlot0, 1);
        }
        if n_istab >= 2 {
            give_item(&ctx, 973_111 + idx * 4, 1405, cid, sid, InventoryFlag::LoSlot1, 1);
        }
        ctx.invalidate_ship_fitting(sid);
        ctx.db.with_mut_snapshot(|s| {
            s.item_positions.insert(sid, [idx as f64 * 10_000.0, 0.0, 0.0]);
        });
        let beyonce_b = ctx.bind_object(&mut sess, "beyonce", NULL_SYS as i64).unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "UpdateStateRequest", &[], &kw, None)
            .await
            .expect("Spawn player ship ball via UpdateStateRequest");
        let mut cs = sys.get_combat_state(sid).await.unwrap().unwrap();
        cs.undock_invulnerable_remaining_ticks = 0;
        sys.set_combat_state(sid, cs).await.unwrap();

        if idx == 0 {
            assert!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(-999)], &kw, None).await.is_err());
        }

        let (_, b0) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
        assert_eq!(b0.mode, MovementMode::Stop, "Ship must start completely stationary before CmdWarpToStuff");
        assert_eq!(b0.align_server_ticks(), expected_ticks, "Dogma align_server_ticks mismatch for case {idx} (align_s={})", b0.align_time_seconds());
        let align_threshold = b0.max_velocity * 0.75;

        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None)
            .await
            .expect("CmdWarpToStuff from stationary");

        for _ in 0..(expected_ticks - 1) {
            let _ = sys.step_manual(1.0).await;
        }
        let (_, b_before) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
        assert!(
            matches!(b_before.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }),
            "Case {idx} ({expected_ticks} ticks): must still be Aligning at tick {}",
            expected_ticks - 1
        );
        assert!(b_before.velocity.length() < align_threshold, "Velocity before tick {expected_ticks} must be < 75% max velocity");

        let _ = sys.step_manual(1.0).await;
        let (_, b_enter) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
        assert!(
            matches!(b_enter.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Accelerating | eve_destiny::WarpPhase::Cruising, .. }),
            "Case {idx}: must enter warp on whole server tick {expected_ticks}"
        );
        assert!(b_enter.velocity.length() >= align_threshold, "Velocity on tick {expected_ticks} must reach >= 75% max velocity");
        let _ = sys.remove_ball(sid).await;
    }
}

#[tokio::test]
async fn test_b35_11_cloak_velocity_penalty() {
    // parity: feat:navigation.cloak-velocity-penalty
    eve_testkit::attest(&["feat:navigation.cloak-velocity-penalty"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2):
    // 1. Unauthenticated Activate rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "Activate", &[PyRep::Integer(1)], &kw, None).await.is_err());

    let c_clk = 953_102;
    let s_clk = 963_102;
    let mut sess_clk = seed_pilot(&ctx, c_clk, s_clk, 641, NULL_SYS, false); // Megathron
    set_skill(&ctx, c_clk, 3336, 5);  // Gallente Battleship V
    set_skill(&ctx, c_clk, 11579, 4); // Cloaking IV
    spawn_ship_in_space(&ctx, c_clk, s_clk, 641, NULL_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;
    let (_, b_init) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    let base_vmax = b_init.max_velocity;

    // 2. Exploit regression check (BF-B35-04): Covert Ops Cloaking Device II (11578) on Megathron (641) MUST FAIL!
    let illegal_cov = 973_199;
    give_item(&ctx, illegal_cov, 11578, c_clk, s_clk, InventoryFlag::HiSlot2, 1);
    ctx.invalidate_ship_fitting(s_clk);
    assert!(
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(illegal_cov), PyRep::String("cloakingWarpSafe".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.is_err(),
        "Megathron battleship must NEVER be allowed to fit or activate Covert Ops Cloaking Device II (11578)"
    );
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&illegal_cov); });
    ctx.invalidate_ship_fitting(s_clk);

    // 1. Prototype Cloaking Device I (11370): -90% velocity (0.10x), propulsion blocked while cloaked, persists across ticks, restored on live Deactivate
    let proto_clk = 973_101;
    let ab_mod = 973_109;
    set_skill(&ctx, c_clk, 3449, 5); // Navigation V
    set_skill(&ctx, c_clk, 3450, 5); // Afterburner V
    give_item(&ctx, proto_clk, 11370, c_clk, s_clk, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, ab_mod, 12058, c_clk, s_clk, InventoryFlag::MedSlot0, 1); // 100MN Afterburner II
    ctx.invalidate_ship_fitting(s_clk);
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Prototype Cloak");
    let (_, b_cloaked) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_cloaked.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Prototype cloak must reduce max velocity by 90% (got {} vs {})", b_cloaked.max_velocity, base_vmax * 0.10);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_mod), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.unwrap_err().code,
        "CannotActivateCloaked",
        "Activating Afterburner while cloaked under Prototype Cloak must be rejected"
    );
    let _ = sys.step_manual(3.0).await;
    let (_, b_cloaked_3s) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_cloaked_3s.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Cloak velocity penalty must remain enforced across server ticks");

    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Deactivate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into())], &kw, None).await.expect("Deactivate Prototype Cloak");
    let _ = sys.step_manual(1.0).await;
    let (_, b_unclk) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_unclk.max_velocity - base_vmax).abs() < 1e-3, "Deactivating cloak must restore max velocity");

    // 2. Improved Cloaking Device II (11577): -75% velocity (0.25x), propulsion blocked while cloaked, restored on live Deactivate
    ctx.advance_time_seconds(35);
    let imp_clk = 973_102;
    give_item(&ctx, imp_clk, 11577, c_clk, s_clk, InventoryFlag::HiSlot1, 1);
    ctx.invalidate_ship_fitting(s_clk);
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(imp_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Improved Cloak");
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_mod), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.unwrap_err().code,
        "CannotActivateCloaked",
        "Activating Afterburner while cloaked under Improved Cloak must be rejected"
    );
    let (_, b_imp) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_imp.max_velocity - base_vmax * 0.25).abs() < 1e-3, "Improved cloak must reduce max velocity by 75%");
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Deactivate", &[PyRep::Integer(imp_clk), PyRep::String("cloaking".into())], &kw, None).await.expect("Deactivate Improved Cloak");
    let _ = sys.step_manual(1.0).await;

    // 3. Covert Ops Cloaking Device II (11578) on legal Covert Ops hull (Helios 11172): 0% velocity penalty (1.00x)
    let c_cov = 953_120;
    let s_cov = 963_120;
    let mut sess_cov = seed_pilot(&ctx, c_cov, s_cov, 11172, NULL_SYS, false);
    set_skill(&ctx, c_cov, 3328, 5);  // Gallente Frigate V
    set_skill(&ctx, c_cov, 12093, 4); // Covert Ops IV
    set_skill(&ctx, c_cov, 11579, 4); // Cloaking IV
    spawn_ship_in_space(&ctx, c_cov, s_cov, 11172, NULL_SYS, DVec3::new(500_100_000.0, 0.0, 0.0)).await;
    let (_, b_cov_init) = sys.get_ball_snapshot(s_cov).await.unwrap().unwrap();
    let cov_base_vmax = b_cov_init.max_velocity;

    let cov_clk = 973_103;
    give_item(&ctx, cov_clk, 11578, c_cov, s_cov, InventoryFlag::HiSlot0, 1);
    ctx.invalidate_ship_fitting(s_cov);
    ctx.dispatch_rpc(&mut sess_cov, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_clk), PyRep::String("cloakingWarpSafe".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Covert Ops Cloak on Helios");
    let (_, b_cov) = sys.get_ball_snapshot(s_cov).await.unwrap().unwrap();
    assert!((b_cov.max_velocity - cov_base_vmax).abs() < 1e-3, "Covert Ops cloak on Helios must incur 0% velocity penalty");
}

#[tokio::test]
async fn test_b35_12_collision_bumping() {
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let c_heavy = 953_201;
    let s_heavy = 963_201;
    seed_pilot(&ctx, c_heavy, s_heavy, 641, NULL_SYS, false);
    let m_heavy = ctx.sde.types.get(&641).map(|t| t.mass).unwrap_or(100_000_000.0);
    let r_heavy = ctx.sde.types.get(&641).map(|t| t.radius).unwrap_or(300.0);

    let c_light = 953_202;
    let s_light = 963_202;
    seed_pilot(&ctx, c_light, s_light, 587, NULL_SYS, false);
    let m_light = ctx.sde.types.get(&587).map(|t| t.mass).unwrap_or(1_000_000.0);
    let r_light = ctx.sde.types.get(&587).map(|t| t.radius).unwrap_or(40.0);

    let _ = sys.spawn_ball(Ball {
        id: s_heavy, type_id: 641, position: DVec3::ZERO, velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 100.0, inertia: 1_000_000.0, mass: m_heavy, radius: r_heavy, speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let _ = sys.spawn_ball(Ball {
        id: s_light, type_id: 587, position: DVec3::new(200.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1_000_000.0, mass: m_light, radius: r_light, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let _ = sys.step_manual(1.0).await;
    let (_, b_h) = sys.get_ball_snapshot(s_heavy).await.unwrap().unwrap();
    let (_, b_l) = sys.get_ball_snapshot(s_light).await.unwrap().unwrap();
    assert!(b_h.position.distance(b_l.position) >= r_heavy + r_light - 1.0, "Colliding ships must separate with no interpenetration");
    assert!(b_l.velocity.length() > (b_h.velocity - DVec3::new(100.0, 0.0, 0.0)).length(), "Lighter ship must receive larger velocity impulse than heavier ship");
}

#[tokio::test]
async fn test_b35_13_drifter_wormhole_systems() {
    let ctx = build_ctx();
    for sys_id in [31_000_001, 31_000_002, 31_000_003, 31_000_004, 31_000_006] {
        let (sigs, _) = eve_server::services::space_destiny::space_docking_cloaking::ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
        let wh_sigs: Vec<_> = sigs.iter().filter(|s| s.wormhole_type_id.is_some()).collect();
        assert_eq!(wh_sigs.len(), sigs.len(), "Every signature in Drifter system {sys_id} must be a wormhole");
        assert!(wh_sigs.len() >= 62, "Drifter system {sys_id} must have 6 statics + 56 K-space connections, got {}", wh_sigs.len());
    }
}

#[tokio::test]
async fn test_b35_14_gravity_hazard_safe_zones() {
    let ctx = build_ctx();
    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);

    // 1. Inside Jovian safe zone (The Fulcrum): 0 damage on live 1 Hz server tick (`try_step_full_tick`)
    let c_safe = 953_103;
    let s_safe = 963_103;
    seed_pilot(&ctx, c_safe, s_safe, 587, ZARZAKH_SYS, false);
    let fulcrum_pos = ctx.sde.stations.get(&60015187).map(|s| s.position).unwrap_or(DVec3::ZERO);
    spawn_ship_in_space(&ctx, c_safe, s_safe, 587, ZARZAKH_SYS, fulcrum_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;

    // 2. Outside safe zones: takes periodic HULL-ONLY damage (shield & armor remain 100% untouched),
    // and reaching 0 hull triggers full death/killmail pipeline (wreck + capsule + killmail)!
    let c_haz = 953_104;
    let s_haz = 963_104;
    seed_pilot(&ctx, c_haz, s_haz, 587, ZARZAKH_SYS, false); // Rifter (~350 structure HP)
    spawn_ship_in_space(&ctx, c_haz, s_haz, 587, ZARZAKH_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;

    let safe_before = z_sys.get_combat_state(s_safe).await.unwrap().unwrap();
    let haz_before = z_sys.get_combat_state(s_haz).await.unwrap().unwrap();

    // Step 1 live server tick via `try_step_full_tick(1.0)`
    let _ = z_sys.try_step_full_tick(1.0);
    tokio::task::yield_now().await;

    let safe_after = z_sys.get_combat_state(s_safe).await.unwrap().unwrap();
    let haz_after = z_sys.get_combat_state(s_haz).await.unwrap().unwrap();
    assert_eq!(safe_after.structure_hp, safe_before.structure_hp, "Ship inside Zarzakh safe zone must take 0 hull damage");
    assert_eq!(haz_after.shield_hp, haz_before.shield_hp, "Zarzakh gravity hazard must leave shield HP untouched");
    assert_eq!(haz_after.armor_hp, haz_before.armor_hp, "Zarzakh gravity hazard must leave armor HP untouched");
    assert!((haz_before.structure_hp - haz_after.structure_hp - 50.0).abs() < 1e-3, "Zarzakh gravity hazard must deal periodic hull damage");

    // Step live ticks until hull reaches 0 -> full death pipeline spawns wreck, capsule, and killmail!
    for _ in 0..10 {
        let _ = z_sys.try_step_full_tick(1.0);
        tokio::task::yield_now().await;
    }
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(z_sys.get_ball_snapshot(s_haz).await.unwrap().is_none(), "Destroyed ship ball must be removed at 0 hull");
    let (active_ship_type, has_killmail, has_wreck) = ctx.db.with_snapshot(|snap| {
        let active_sid = snap.characters.get(&c_haz).map(|c| c.active_ship_id).unwrap_or(0);
        let active_tid = snap.items.get(&active_sid).map(|i| i.type_id).unwrap_or(0);
        let km = snap.killmails.values().any(|k| k.victim_character_id == c_haz && k.solar_system_id == ZARZAKH_SYS);
        let wreck = snap.items.values().any(|i| i.location_id == ZARZAKH_SYS as i64 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186));
        (active_tid, km, wreck)
    });
    assert_eq!(active_ship_type, 670, "Destroyed pilot in Zarzakh hazard must transition into a Capsule (670)");
    assert!(has_wreck, "Zarzakh hazard ship destruction must spawn a wreck");
    assert!(has_killmail, "Zarzakh hazard ship destruction must generate a killmail");
}

#[tokio::test]
async fn test_b35_15_interdiction_nullification_module() {
    // parity: feat:navigation.interdiction-nullification-module
    eve_testkit::attest(&["feat:navigation.interdiction-nullification-module"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse case 1: unauthenticated Activate rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "Activate", &[PyRep::Integer(1)], &kw, None).await.is_err());

    // 1. Passive penalties on fitting: scan resolution, targeting range, and -50% drone bandwidth
    let c_base = 953_106;
    let s_base = 963_106;
    let mut sess_drag = seed_pilot(&ctx, c_base, s_base, 11172, NULL_SYS, false); // Helios (unnullified control for drag bubble!)
    set_skill(&ctx, c_base, 3328, 5);
    set_skill(&ctx, c_base, 12093, 4);
    let ev_before = ctx.get_or_rebuild_ship_fitting(c_base, s_base).unwrap().evaluated_context(&ctx.sde);
    let scan_res_before = ev_before.ship.get_attribute(564, &ctx.sde);
    let target_range_before = ev_before.ship.get_attribute(76, &ctx.sde);
    assert_eq!(ev_before.ship.get_attribute(1271, &ctx.sde), 5.0);

    let c_null = 953_105;
    let s_null = 963_105;
    let mut sess_null = seed_pilot(&ctx, c_null, s_null, 11172, NULL_SYS, false);
    set_skill(&ctx, c_null, 3328, 5);
    set_skill(&ctx, c_null, 12093, 4);
    set_skill(&ctx, c_null, 3455, 4); // Warp Drive Operation / Navigation req for 58919
    spawn_ship_in_space(&ctx, c_null, s_null, 11172, NULL_SYS, DVec3::ZERO).await;
    let null_mod = 973_104;
    give_item(&ctx, null_mod, 58919, c_null, s_null, InventoryFlag::HiSlot0, 1); // Interdiction Nullifier I
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(null_mod, true); });
    ctx.invalidate_ship_fitting(s_null);
    let ev_after = ctx.get_or_rebuild_ship_fitting(c_null, s_null).unwrap().evaluated_context(&ctx.sde);
    assert!(ev_after.ship.get_attribute(564, &ctx.sde) < scan_res_before);
    assert!(ev_after.ship.get_attribute(76, &ctx.sde) < target_range_before);
    assert!((ev_after.ship.get_attribute(1271, &ctx.sde) - 2.5).abs() < 1e-3);

    // 2. Origin bubble + Destination drag/catch bubble along warp vector to planet 40301369
    let target_celestial_id = 40301369;
    let planet = ctx.sde.celestials_in_system(NULL_SYS).into_iter().find(|c| c.celestial_id == target_celestial_id).unwrap();
    let planet_pos = planet.position;
    let warp_dir = planet_pos.normalize();
    let drag_bubble_pos = planet_pos - warp_dir * (planet.radius + 2_500.0 + 120_000.0); // Catch bubble 120km short of planet warp-in point!
    let _ = sys.add_warp_bubble(999_888, DVec3::ZERO, 20_000.0).await;
    let _ = sys.add_warp_bubble(999_889, drag_bubble_pos, 20_000.0).await;

    // Negative/abuse case 2: unnullified warp inside origin bubble fails with WarpScrambled
    let warp_err = ctx.dispatch_rpc(&mut sess_null, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err();
    assert_eq!(warp_err.code, "WarpScrambled");

    // Spawn unnullified Helios outside origin bubble (50km perpendicular) and warp to same planet -> caught by drag bubble!
    spawn_ship_in_space(&ctx, c_base, s_base, 11172, NULL_SYS, DVec3::new(0.0, 50_000.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess_drag, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("Unnullified warp from outside origin bubble");

    // Activate Interdiction Nullifier on s_null -> escapes origin bubble AND ignores destination drag/catch bubble!
    ctx.dispatch_rpc(&mut sess_null, "dogmaIM", None, "Activate", &[PyRep::Integer(null_mod), PyRep::String("defaultEffect".into()), PyRep::None, PyRep::Integer(1)], &kw, None).await.expect("Activate Interdiction Nullifier");
    assert!(sys.get_combat_state(s_null).await.unwrap().unwrap().is_interdiction_nullified);
    ctx.dispatch_rpc(&mut sess_null, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_celestial_id), PyRep::Real(0.0)], &kw, None).await.expect("Nullified warp from inside origin bubble");

    // Step warp to completion so both ships exit warp
    for _ in 0..80 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b_dragged) = sys.get_ball_snapshot(s_base).await.unwrap().unwrap();
    let (_, b_null_landed) = sys.get_ball_snapshot(s_null).await.unwrap().unwrap();
    let dist_dragged = b_dragged.position.distance(planet_pos);
    let dist_null = b_null_landed.position.distance(planet_pos);
    assert!(
        dist_dragged > dist_null + 80_000.0,
        "Unnullified ship must be pulled short by catch bubble ({dist_dragged}m from planet vs nullified {dist_null}m)"
    );

    let _ = sys.remove_warp_bubble(999_888).await;
    let _ = sys.remove_warp_bubble(999_889).await;
}
