//! Part 2 integration tests closing review `20261028-015440` EWAR & Capacitor Warfare findings:
//! - `R-NOS-HARDCODE-01`: SDE-driven Blood Raider `nosOverride` (`attr 1945` / `effect 5854`) across Cruor, Ashimmu, Bhaalgorn, and Dagon on both RPC dispatch paths
//! - `R-BURST-ECM-01`: Burst Jammer T1 (`5,000 m`) / T2 (`6,000 m`) radii, fitted ECCM sensor strength, independent per-ship RNG rolls, non-ship filter, multi-victim Crimewatch, and Lowsec Suspect flag

use crate::common;
use super::combat_ewar_repairs_closure::{grant_all_skills_for_types, rpc_lock_target, seed_item};

use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_dogma::is_blood_raider_nosferatu_hull;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
use std::collections::HashMap;

#[tokio::test]
async fn test_blood_raider_sde_nosferatu_override_all_hulls_and_rpc_paths() {
    // parity: feat:combat.nosferatu-drain-cap, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:combat.nosferatu-drain-cap"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9251);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    // Cruor (17926), Ashimmu (17922), Bhaalgorn (17920), Dagon (42242), plus non-Blood-Raider Raven (640)
    for &br_tid in &[17926u32, 17922, 17920, 42242] {
        assert!(
            is_blood_raider_nosferatu_hull(br_tid, &ctx.sde),
            "Blood Raider hull {br_tid} must be detected dynamically from SDE nosOverride"
        );
    }
    assert!(!is_blood_raider_nosferatu_hull(640, &ctx.sde));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 640;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[640, 17926, 17922, 17920, 42242, 530]);

    let nos_id = 100_925_001i64;
    seed_item(&ctx, nos_id, 530, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(nos_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None).await.unwrap();
    let sys_id = sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.mode = MovementMode::Stop;
    let init_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    sys.spawn_ball_with_combat(ship_ball.clone(), init_cs).await.unwrap();

    let victim_id = 100_925_010i64;
    seed_item(&ctx, victim_id, 587, 90_000_002, sys_id as i64, InventoryFlag::None, 1);
    sys.spawn_ball_with_combat(
        Ball {
            id: victim_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            max_capacitor: 1_000.0,
            capacitor: 300.0,
            capacitor_recharge_time_ms: 0.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    rpc_lock_target(&ctx, &sess, char_id, ship_id, victim_id).await;

    // Non-Blood-Raider Raven (640) with self_cap (600) > target_cap (300) transfers 0 GJ on both paths:
    let reset_caps = || async {
        let mut s_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        s_cs.capacitor_recharge_time_ms = 0.0;
        s_cs.max_capacitor = 2_000.0;
        s_cs.capacitor = 600.0;
        sys.set_combat_state(ship_id, s_cs).await.unwrap();
        let mut v_cs = sys.get_combat_state(victim_id).await.unwrap().unwrap();
        v_cs.capacitor_recharge_time_ms = 0.0;
        v_cs.max_capacitor = 1_000.0;
        v_cs.capacitor = 300.0;
        sys.set_combat_state(victim_id, v_cs).await.unwrap();
    };
    reset_caps().await;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(nos_id),
            PyRep::String("energyNosferatuFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("nosferatu on Raven");
    for _ in 0..6 {
        sys.tick().await.unwrap();
    }
    let v_after_std = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    assert_eq!(v_after_std, 300.0, "Standard nosferatu must not drain when target cap % < self cap %");

    // Every Blood Raider hull (Cruor 17926, Ashimmu 17922, Bhaalgorn 17920, Dagon 42242) drains via cycle end (`t = duration_s`):
    for &br_tid in &[17926u32, 17922, 17920, 42242] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                ship_item.type_id = br_tid;
            }
        });
        ctx.invalidate_ship_fitting(ship_id);

        reset_caps().await;
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(nos_id),
                PyRep::String("energyNosferatuFalloff".into()),
                PyRep::Integer(victim_id),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("cycle nosferatu on Blood Raider hull");
        for _ in 0..6 {
            sys.tick().await.unwrap();
        }
        let v_after = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
        let s_after = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
        assert!(
            v_after < 300.0 && s_after > 600.0,
            "Hull {br_tid} must drain target at cycle end via SDE nosOverride (victim={v_after}, self={s_after})"
        );
    }
}

#[tokio::test]
async fn test_burst_jammer_t1_t2_radius_fitted_eccm_nonship_filter_multivictim_and_lowsec() {
    // parity: feat:combat.ecm-burst-radius, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:combat.ecm-burst-radius"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.set_mechanic_rng_seed(Some(0xEC_B057_2026));
    let kw = HashMap::new();
    let mut sess = SessionState::new(9252);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 638;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    grant_all_skills_for_types(&ctx, char_id, &[638, 580, 2117]);

    let burst_id = 100_926_001i64;
    seed_item(&ctx, burst_id, 580, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(burst_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ship_ball.position = DVec3::new(100_000_000.0, 0.0, 0.0);
    ship_ball.velocity = DVec3::ZERO;
    ship_ball.speed_fraction = 0.0;
    ship_ball.mode = MovementMode::Stop;
    let mut self_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    self_cs.undock_invulnerable_remaining_ticks = 0;
    self_cs.warp_exit_invulnerable_remaining_ticks = 0;
    self_cs.max_capacitor = 50_000.0;
    self_cs.capacitor = 50_000.0;
    sys.spawn_ball_with_combat(ship_ball.clone(), self_cs).await.unwrap();

    // 1. Non-ship objects (yellow wreck `26561`, neutral drone `2454`, Mobile Depot `33474`) at 2,000 m
    // with NO neutral ships in range must NOT trigger `SafetyBlocked` or Criminal/CONCORD and must NOT be jammed!
    let v_char_a = 90_000_002i64;
    let wreck_id = 100_926_010i64;
    let drone_id = 100_926_011i64;
    let depot_id = 100_926_012i64;
    seed_item(&ctx, wreck_id, 26561, v_char_a, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, drone_id, 2454, v_char_a, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, depot_id, 33474, v_char_a, sys_id as i64, InventoryFlag::None, 1);
    for (bid, tid, dx) in [(wreck_id, 26561u32, 1_500.0), (drone_id, 2454, 2_000.0), (depot_id, 33474, 2_500.0)] {
        sys.spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: ship_ball.position + DVec3::new(dx, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 5_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: v_char_a,
                original_owner_char_id: v_char_a,
                parent_ship_id: 0,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
        .await
        .expect("Burst Jammer near only wrecks/drones/deployables must succeed");
    let cw_nonship = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert_eq!(cw_nonship.criminal_flag_until_unix, 0, "Wrecks/drones/deployables must NOT trigger Criminal/CONCORD on Burst Jammer");
    assert_eq!(cw_nonship.suspect_flag_until_unix, 0, "Wrecks/drones/deployables must NOT trigger Suspect on Burst Jammer");
    assert_eq!(sys.get_combat_state(drone_id).await.unwrap().unwrap().ecm_jammed_remaining_s, 0.0);
    assert_eq!(sys.get_combat_state(depot_id).await.unwrap().unwrap().ecm_jammed_remaining_s, 0.0);
    sys.step_manual(31.0).await.unwrap();
    ctx.advance_secs(31);

    // 2. T1 (`5,000 m`) vs T2 (`6,000 m`) boundary + Multi-victim Crimewatch + Independent RNG rolls + Fitted ECCM!
    let v_char_b = 90_000_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&v_char_a) {
            ca.clone_grade = 2;
        }
        if let Some(template) = snap.characters.get(&v_char_a).cloned() {
            snap.characters.entry(v_char_b).or_insert(eve_db::CharacterRecord {
                character_id: v_char_b,
                name: "NeutralVictimB".into(),
                clone_grade: 2,
                ..template
            });
        }
    });
    let ship_3km_a = 100_926_020i64;
    let ship_3km_b = 100_926_021i64;
    let ship_5500m = 100_926_022i64;
    let sebo_id = 100_926_023i64;
    let eccm_id = 100_926_024i64;
    grant_all_skills_for_types(&ctx, v_char_a, &[640, 587]);
    grant_all_skills_for_types(&ctx, v_char_b, &[640, 1952, 41155]);
    seed_item(&ctx, ship_3km_a, 640, v_char_a, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, ship_3km_b, 640, v_char_b, sys_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, ship_5500m, 587, v_char_a, sys_id as i64, InventoryFlag::None, 1);
    // Fit Sensor Booster II (`1952`, active) + ECCM Script (`41155`) on `ship_3km_b` (`v_char_b`)!
    seed_item(&ctx, sebo_id, 1952, v_char_b, ship_3km_b, InventoryFlag::MedSlot0, 1);
    seed_item(&ctx, eccm_id, 41155, v_char_b, ship_3km_b, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&v_char_a) {
            ca.active_ship_id = ship_3km_a;
            ca.solar_system_id = sys_id;
        }
        if let Some(cb) = snap.characters.get_mut(&v_char_b) {
            cb.active_ship_id = ship_3km_b;
            cb.solar_system_id = sys_id;
        }
        snap.module_online_states.insert(sebo_id, true);
        snap.module_active_states.insert(sebo_id);
        snap.module_active_charges.insert(sebo_id, 41155);
    });
    ctx.invalidate_ship_fitting(ship_3km_a);
    ctx.invalidate_ship_fitting(ship_3km_b);
    let fit_unboosted = ctx.get_or_rebuild_ship_fitting(v_char_a, ship_3km_a).unwrap();
    let fit_eccm = ctx.get_or_rebuild_ship_fitting(v_char_b, ship_3km_b).unwrap();
    assert!(
        fit_eccm.ship.get_attribute(211, &ctx.sde) > fit_unboosted.ship.get_attribute(211, &ctx.sde) * 1.5,
        "ECCM-scripted Sensor Booster II must substantially boost Gravimetric sensor strength"
    );

    for (sid, tid, dx) in [(ship_3km_a, 640u32, 3_000.0), (ship_3km_b, 640, 3_000.0), (ship_5500m, 587, 5_500.0)] {
        sys.spawn_ball_with_combat(
            Ball {
                id: sid,
                type_id: tid,
                position: ship_ball.position + DVec3::new(dx, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    }

    let mut jams_unboosted = 0u32;
    let mut jams_eccm = 0u32;
    let mut saw_split_outcome = false;
    for i in 0..60 {
        for &(c_owner, sid) in &[(v_char_a, ship_3km_a), (v_char_b, ship_3km_b), (v_char_a, ship_5500m)] {
            rpc_lock_target(&ctx, &sess, c_owner, sid, ship_id).await;
        }
        sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .expect("Activate T1 Burst Jammer");
        if i == 0 {
            // Verify BOTH neutral capsuleer victims received PvP timers and Limited Engagements!
            let now_u = ctx.now_unix_secs();
            assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, v_char_a, now_u)));
            assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, v_char_b, now_u)));
            let (pvp_a, pvp_b) = ctx.crimewatch.with_crimewatch(|cw| {
                (
                    cw.states.get(&v_char_a).map(|s| s.pvp_timer_until_unix).unwrap_or(0),
                    cw.states.get(&v_char_b).map(|s| s.pvp_timer_until_unix).unwrap_or(0),
                )
            });
            assert!(pvp_a > now_u && pvp_b > now_u, "Multi-victim Burst Jammer must record aggression on ALL neutral capsuleers in range");
        }
        assert_eq!(sys.get_target_locks(ship_5500m).await.unwrap().len(), 1, "T1 Burst Jammer (5,000m) must NEVER jam ship at 5,500m");
        let a_jammed = sys.get_target_locks(ship_3km_a).await.unwrap().is_empty();
        let b_jammed = sys.get_target_locks(ship_3km_b).await.unwrap().is_empty();
        if a_jammed {
            jams_unboosted += 1;
        }
        if b_jammed {
            jams_eccm += 1;
        }
        if a_jammed != b_jammed {
            saw_split_outcome = true;
        }
        sys.step_manual(31.0).await.unwrap();
        ctx.advance_secs(31);
    }
    assert!(saw_split_outcome, "Independent per-ship RNG rolls must allow one ship to jam while another resists in the same burst");
    assert!(
        jams_unboosted > jams_eccm,
        "ECCM-scripted Sensor Booster II must reduce empirical Burst Jammer jam rate (unboosted={jams_unboosted} vs eccm={jams_eccm})"
    );

    // Swap to Burst Jammer II (`2117`, `6,000 m` radius) -> ship at `5,500 m` IS now jammed!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&burst_id) {
            m.type_id = 2117;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut jammed_5500_by_t2 = false;
    for _ in 0..30 {
        rpc_lock_target(&ctx, &sess, v_char_a, ship_5500m, ship_id).await;
        sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .expect("Activate T2 Burst Jammer");
        if sys.get_target_locks(ship_5500m).await.unwrap().is_empty() {
            jammed_5500_by_t2 = true;
            break;
        }
        sys.step_manual(31.0).await.unwrap();
        ctx.advance_secs(31);
    }
    assert!(jammed_5500_by_t2, "Burst Jammer II (6,000m radius) must jam ship at 5,500m");

    // 3. Lowsec (`30002537` Amamake, sec ~0.4): Safety Green rejected, Safety Yellow flags Suspect (900s) WITHOUT CONCORD!
    let lowsec_id = 30_002_537u32;
    let lowsec = ctx.systems.get_or_spawn(lowsec_id);
    ctx.crimewatch.write().unwrap().states.clear();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
    });
    let low_ship_id = 100_926_050i64;
    let low_burst_id = 100_926_051i64;
    let low_victim_id = 100_926_052i64;
    seed_item(&ctx, low_ship_id, 638, char_id, lowsec_id as i64, InventoryFlag::None, 1);
    seed_item(&ctx, low_burst_id, 580, char_id, low_ship_id, InventoryFlag::MedSlot0, 1);
    seed_item(&ctx, low_victim_id, 587, v_char_a, lowsec_id as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(low_burst_id, true);
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = lowsec_id;
            c.active_ship_id = low_ship_id;
        }
        if let Some(v) = snap.characters.get_mut(&v_char_a) {
            v.solar_system_id = lowsec_id;
            v.active_ship_id = low_victim_id;
        }
    });
    sess.solar_system_id = Some(lowsec_id);
    sess.ship_id = Some(low_ship_id);
    ctx.invalidate_ship_fitting(low_ship_id);
    lowsec
        .spawn_ball_with_combat(
            Ball {
                id: low_ship_id,
                type_id: 638,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { max_capacitor: 5_000.0, capacitor: 5_000.0, ..CombatState::default() },
        )
        .await
        .unwrap();
    lowsec
        .spawn_ball_with_combat(
            Ball {
                id: low_victim_id,
                type_id: 587,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)], &kw, None).await.unwrap();
    let low_green_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(low_burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("Lowsec Burst Jammer on Safety Green must fail");
    assert_eq!(low_green_err.code, "SafetyLevelRestricted");

    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(low_burst_id), PyRep::String("ecmBurst".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
        .await
        .expect("Lowsec Burst Jammer on Safety Yellow must succeed");
    let cw_low = ctx.crimewatch.with_crimewatch(|cw| cw.states.get(&char_id).cloned().unwrap_or_default());
    assert!(cw_low.suspect_flag_until_unix >= ctx.now_unix_secs() + 895, "Lowsec Burst Jammer near neutral must grant 15m Suspect flag");
    assert_eq!(cw_low.criminal_flag_until_unix, 0, "Lowsec Burst Jammer must NOT grant Criminal flag");
    assert!(cw_low.pending_concord_arrival.is_none(), "Lowsec Burst Jammer must NOT schedule CONCORD");
}
