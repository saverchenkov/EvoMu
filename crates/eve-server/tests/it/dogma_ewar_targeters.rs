//! Dogma Engine & Combat integration tests (DG-4, DG-5, C-1, C-2).
//!
//! Covers:
//! - DG-4: Module activation cycles, capacitor recharge ODE & stability, overheat & rack heat damage,
//!         shield booster & armor repairer cycle healing, and reactivation delay.
//! - DG-5: Combat target locking state machine, racial sensor strengths, ECM jam chance & lock break,
//!         sensor dampeners, target painters, tracking disruptors, stasis webifiers, warp disruptors & scramblers.
//! - C-1: Turret tracking formula, wrecking shots, and missile flight time & damage application.
//! - C-2: Dogma-initialized CombatState, 4-channel resonance HP deduction, and OnDamageStateChange notifications.


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{
    Ball, CombatState, DVec3, DestinyEvent, MovementMode, TargetLock,
};
use eve_dogma::*;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn seed_test_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: true,
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

fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(skill_id, level) in skills {
            entry.insert(
                skill_id,
                eve_db::TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL),
        (eve_sde::attr_ids::REQUIRED_SKILL_2, eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL),
        (eve_sde::attr_ids::REQUIRED_SKILL_3, eve_sde::attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

fn extract_item_attr(info_rep: &PyRep, attr_id: i64) -> f64 {
    let attrs = info_rep.dict_get("attributes").expect("attributes dict in ItemGetInfo");
    if let PyRep::Dict(entries) = attrs {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
}

#[tokio::test]
async fn test_dg5_targeting_state_machine_sensor_strength_and_ewar() {
    // parity: plan:DG-5, feat:dogma.targeting-ships-sensor-strength, feat:modules.ecm, feat:modules.burst-jammer, feat:modules.sensor-dampener, feat:modules.tracking-disruptor, feat:modules.target-painter, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:dogma.targeting-ships-sensor-strength", "feat:modules.ecm", "feat:modules.burst-jammer", "feat:modules.sensor-dampener", "feat:modules.tracking-disruptor", "feat:modules.target-painter"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade attacker hull to Scorpion (640, group 27 Battleship, 8 MedSlots) while docked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 640;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    for tid in [
        640u32, 1957, 580, 2108, 37543, 1968, 12709, 29005, 29007, 40334, 40335, 29013, 29015,
        2889, 499, 210,
    ] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    grant_skills(&ctx, char_id, &[
        (3426, 5), // CPU Management
        (3413, 5), // Power Grid Management
        (3418, 5), // Capacitor Management
        (3417, 5), // Capacitor Systems Operation
        (3427, 5), // Electronic Warfare
        (3435, 5), // Weapon Disruption
        (28164, 5), // Thermodynamics
    ]);

    // 1. Verify Racial Sensor Strengths across all 4 factions (feat:dogma.targeting-ships-sensor-strength)
    // Caldari Merlin (603): Gravimetric (211) = 11.0
    let merlin_state = DogmaItemState::from_sde(1, 603, &ctx.sde);
    let (m_type, m_strength) = get_ship_sensor_strength(&merlin_state, &ctx.sde);
    assert_eq!(m_type, ShipSensorType::Gravimetric);
    assert_eq!(m_strength, 11.0);

    // Minmatar Rifter (587): Ladar (209) = 8.0
    let rifter_state = DogmaItemState::from_sde(2, 587, &ctx.sde);
    let (r_type, r_strength) = get_ship_sensor_strength(&rifter_state, &ctx.sde);
    assert_eq!(r_type, ShipSensorType::Ladar);
    assert_eq!(r_strength, 8.0);

    // Gallente Tristan (593): Magnetometric (210) = 9.0
    let tristan_state = DogmaItemState::from_sde(3, 593, &ctx.sde);
    let (t_type, t_strength) = get_ship_sensor_strength(&tristan_state, &ctx.sde);
    assert_eq!(t_type, ShipSensorType::Magnetometric);
    assert_eq!(t_strength, 9.0);

    // Amarr Executioner (589): Radar (208) = 8.0
    let exec_state = DogmaItemState::from_sde(4, 589, &ctx.sde);
    let (e_type, e_strength) = get_ship_sensor_strength(&exec_state, &ctx.sde);
    assert_eq!(e_type, ShipSensorType::Radar);
    assert_eq!(e_strength, 8.0);

    // 2. Verify ECM Jammer & Burst Jammer fitting and activation via dogmaIM.Activate (feat:modules.ecm)
    let ecm_item = DogmaItemState::from_sde(5, 1957, &ctx.sde);
    let jam_opt = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 10_000.0, &ctx.sde);
    assert!((jam_opt - 0.20).abs() < 1e-4);
    let jam_falloff = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 19_200.0 + 18_000.0, &ctx.sde);
    assert!((jam_falloff - 0.10).abs() < 1e-4);

    // Fit Multispectral ECM I (1957), Burst Jammer I (580), Tracking Disruptor I (2108), and Guidance Disruptor I (37543) via invbroker.Add
    let ecm_mod_id = 100_000_931i64;
    let burst_mod_id = 100_000_932i64;
    let td_mod_id = 100_000_933i64;
    let gd_mod_id = 100_000_934i64;
    let damp_mod_id = 100_000_921i64;
    let tp_mod_id = 100_000_922i64;
    seed_test_item(&ctx, ecm_mod_id, 1957, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, burst_mod_id, 580, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, td_mod_id, 2108, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, gd_mod_id, 37543, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, damp_mod_id, 1968, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, tp_mod_id, 12709, char_id, station_id, InventoryFlag::Hangar, 1);

    let opt_script_id = 100_000_935i64;
    let trk_script_id = 100_000_936i64;
    let m_rng_script_id = 100_000_937i64;
    let m_prec_script_id = 100_000_938i64;
    let damp_rng_script_id = 100_000_923i64;
    let damp_scan_script_id = 100_000_924i64;
    seed_test_item(&ctx, opt_script_id, 29005, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, trk_script_id, 29007, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, m_rng_script_id, 40334, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, m_prec_script_id, 40335, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, damp_rng_script_id, 29015, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, damp_scan_script_id, 29013, char_id, ship_id, InventoryFlag::Cargo, 1);

    for (mid, flag) in [
        (ecm_mod_id, InventoryFlag::MedSlot0),
        (burst_mod_id, InventoryFlag::MedSlot1),
        (td_mod_id, InventoryFlag::MedSlot2),
        (gd_mod_id, InventoryFlag::MedSlot3),
        (damp_mod_id, InventoryFlag::MedSlot4),
        (tp_mod_id, InventoryFlag::MedSlot5),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mid),
                PyRep::Integer(ship_id),
                PyRep::Integer(flag as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit EWAR module into MedSlot via invbroker.Add");
    }

    // Undock into solar system 30_000_142
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys_id = sess.solar_system_id.expect("solar_system_id after Undock");
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball in space");

    // Seed target ship (victim_id) fitted with 200mm AutoCannon II (2889) and Light Missile Launcher I (499) + Scourge Light Missile (210)
    let target_char_id = 90_000_002i64;
    for tid in [587u32, 2889, 499, 210] {
        grant_all_required_skills_for_type(&ctx, target_char_id, tid);
    }
    grant_skills(&ctx, target_char_id, &[
        (3426, 5), // CPU Management
        (3413, 5), // Power Grid Management
    ]);
    let victim_id = 100_000_940i64;
    let target_turret_id = 100_000_941i64;
    let target_launcher_id = 100_000_942i64;
    let target_missile_id = 100_000_943i64;
    seed_test_item(&ctx, victim_id, 587, target_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_test_item(&ctx, target_turret_id, 2889, target_char_id, victim_id, InventoryFlag::HiSlot0, 1);
    seed_test_item(&ctx, target_launcher_id, 499, target_char_id, victim_id, InventoryFlag::HiSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            target_missile_id,
            InventoryItemRecord {
                item_id: target_missile_id,
                type_id: 210, // Scourge Light Missile
                owner_id: target_char_id,
                location_id: victim_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 40,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Spawn victim_id at 5 km (inside 10 km Burst Jammer range) and far_id at 25 km (outside 10 km Burst Jammer range)
    let far_id = 100_000_945i64;
    sys.spawn_ball(Ball {
        id: victim_id,
        type_id: 587,
        position: ship_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn victim ball");
    sys.spawn_ball(Ball {
        id: far_id,
        type_id: 587,
        position: ship_ball.position + DVec3::new(25_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn far ball");

    // Lock victim_id from attacker ship_id, and give victim_id a lock on ship_id
    sys.add_target_lock(
        ship_id,
        TargetLock {
            target_id: victim_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        victim_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1);

    // Activate Multispectral ECM I (1957) on victim_id via dogmaIM.Activate until probabilistic CSPRNG jam roll lands
    let mut ecm_jammed = false;
    for _ in 0..100 {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(ecm_mod_id),
                PyRep::String("targetHostile".into()),
                PyRep::Integer(victim_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Multispectral ECM I via dogmaIM.Activate");
        let jammed_s = sys
            .get_combat_state(victim_id)
            .await
            .unwrap()
            .unwrap()
            .ecm_jammed_remaining_s;
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[
                PyRep::Integer(ecm_mod_id),
                PyRep::String("targetHostile".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deactivate Multispectral ECM I");
        if jammed_s > 0.0 {
            ecm_jammed = true;
            break;
        }
    }
    assert!(
        ecm_jammed,
        "Victim must be ECM-jammed after Multispectral ECM I activation"
    );
    assert_eq!(
        sys.get_target_locks(victim_id).await.unwrap().len(),
        0,
        "Targeted Multispectral ECM I activation must break all target locks on victim"
    );

    // Verify 20-second ECM jam duration and targeting lockout across system steps (feat:modules.ecm)
    let initial_jam_rem = sys
        .get_combat_state(victim_id)
        .await
        .unwrap()
        .unwrap()
        .ecm_jammed_remaining_s;
    assert!(
        (initial_jam_rem - 20.0).abs() < 1e-4,
        "Successful ECM jam must apply a 20.0s jam timer, got {initial_jam_rem}"
    );
    let mut victim_sess = SessionState::new(4104);
    victim_sess.user_id = Some(2);
    victim_sess.character_id = Some(target_char_id);
    victim_sess.ship_id = Some(victim_id);
    victim_sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&target_char_id) {
            ch.active_ship_id = victim_id;
            ch.solar_system_id = sys_id;
        }
    });
    let lock_while_jammed = ctx
        .dispatch_rpc(
            &mut victim_sess,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await;
    assert!(
        lock_while_jammed.is_err(),
        "Victim must not be able to lock targets during the 20s ECM jam window"
    );
    sys.step_manual(10.0).await.unwrap();
    let mid_jam_rem = sys
        .get_combat_state(victim_id)
        .await
        .unwrap()
        .unwrap()
        .ecm_jammed_remaining_s;
    assert!(
        (mid_jam_rem - 10.0).abs() < 1e-4,
        "After 10s, ECM jam timer must have 10.0s remaining, got {mid_jam_rem}"
    );
    assert!(
        ctx.dispatch_rpc(
            &mut victim_sess,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .is_err(),
        "Victim must still be blocked from locking at 10s into the 20s ECM jam window"
    );
    sys.step_manual(10.0).await.unwrap();
    let expired_jam_rem = sys
        .get_combat_state(victim_id)
        .await
        .unwrap()
        .unwrap()
        .ecm_jammed_remaining_s;
    assert!(
        expired_jam_rem.abs() < 1e-4,
        "After 20s, ECM jam timer must expire to 0.0s, got {expired_jam_rem}"
    );
    ctx.dispatch_rpc(
        &mut victim_sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Victim must be able to lock targets again once the 20s ECM jam expires");
    let _ = sys.clear_target_locks(victim_id).await;

    // Re-lock from both victim_id (5 km) and far_id (25 km)
    sys.add_target_lock(
        victim_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        far_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    // Activate Burst Jammer I (580, ecmBurstRange = 10,000m) via dogmaIM.Activate until probabilistic CSPRNG jam roll lands on victim_id (5 km)
    let mut burst_jammed = false;
    for _ in 0..100 {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(burst_mod_id),
                PyRep::String("ecmBurst".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Burst Jammer I via dogmaIM.Activate");
        assert_eq!(
            sys.get_target_locks(far_id).await.unwrap().len(),
            1,
            "Burst Jammer I must NOT break locks on targets outside its 10km ecmBurstRange (far_id at 25km)"
        );
        assert_eq!(
            sys.get_combat_state(far_id)
                .await
                .unwrap()
                .unwrap()
                .ecm_jammed_remaining_s,
            0.0,
            "far_id at 25km must not be jammed by 10km Burst Jammer I"
        );
        let jammed_s = sys
            .get_combat_state(victim_id)
            .await
            .unwrap()
            .unwrap()
            .ecm_jammed_remaining_s;
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[
                PyRep::Integer(burst_mod_id),
                PyRep::String("ecmBurst".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deactivate Burst Jammer I");
        if jammed_s > 0.0 {
            burst_jammed = true;
            break;
        }
    }
    assert!(
        burst_jammed,
        "Victim at 5km must be jammed by Burst Jammer I"
    );
    assert_eq!(
        sys.get_target_locks(victim_id).await.unwrap().len(),
        0,
        "Burst Jammer I must break locks on targets within its 10km ecmBurstRange (victim at 5km)"
    );

    // 3. Verify Remote Sensor Dampener & Scripts via dogmaIM.Activate and ItemGetInfo (feat:modules.sensor-dampener)
    let damp_item = DogmaItemState::from_sde(6, 1968, &ctx.sde);
    let (damp_scan_un, damp_range_un) = eval_scripted_sensor_dampener(&damp_item, None, 10_000.0, &ctx.sde);
    assert!(damp_scan_un < 1.0 && damp_range_un < 1.0);
    assert!((damp_scan_un - (1.0 - 0.137)).abs() < 1e-4);
    assert!((damp_range_un - (1.0 - 0.137)).abs() < 1e-4);

    let (damp_scan_scr, damp_range_scr) = eval_scripted_sensor_dampener(&damp_item, Some(29013), 10_000.0, &ctx.sde);
    assert!(damp_scan_scr < damp_scan_un, "Scripted scan dampening must double scan resolution reduction");
    assert_eq!(damp_range_scr, 1.0, "Scan resolution script zeroes range reduction");

    let (damp_scan_rng, damp_range_rng) = eval_scripted_sensor_dampener(&damp_item, Some(29015), 10_000.0, &ctx.sde);
    assert_eq!(damp_scan_rng, 1.0, "Targeting range script zeroes scan res reduction");
    assert!(damp_range_rng < damp_range_un, "Scripted range dampening must double range reduction");

    let base_victim_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo baseline victim ship");
    let base_victim_range = extract_item_attr(&base_victim_info, 76);
    let base_victim_scan = extract_item_attr(&base_victim_info, 564);
    let base_victim_sig = extract_item_attr(&base_victim_info, 552);
    assert!(base_victim_range > 0.0 && base_victim_scan > 0.0 && base_victim_sig > 0.0);

    // 3a. Activate unscripted Remote Sensor Dampener I (1968) against victim_id
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate unscripted Remote Sensor Dampener I");
    let damp_un_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under unscripted Sensor Dampener");
    let victim_range_un = extract_item_attr(&damp_un_info, 76);
    let victim_scan_un = extract_item_attr(&damp_un_info, 564);
    assert_eq!(
        (victim_range_un < base_victim_range, victim_scan_un < base_victim_scan),
        (true, true),
        "Unscripted Remote Sensor Dampener I must reduce victim maxTargetRange (76): {victim_range_un} vs {base_victim_range} and scanResolution (564): {victim_scan_un} vs {base_victim_scan}"
    );
    assert_eq!(
        (victim_range_un / base_victim_range * 1000.0).round() as i64,
        (damp_range_un * 1000.0).round() as i64
    );
    assert_eq!(
        (victim_scan_un / base_victim_scan * 1000.0).round() as i64,
        (damp_scan_un * 1000.0).round() as i64
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate unscripted Remote Sensor Dampener I");

    // 3b. Load Targeting Range Dampening Script (29015) and activate against victim_id
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(damp_rng_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot4 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Targeting Range Dampening Script into Remote Sensor Dampener I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Range-scripted Remote Sensor Dampener I");
    let damp_rng_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Range-scripted Sensor Dampener");
    let victim_range_scr = extract_item_attr(&damp_rng_info, 76);
    let victim_scan_rng_scr = extract_item_attr(&damp_rng_info, 564);
    assert!(
        victim_range_scr < victim_range_un,
        "Targeting Range Dampening Script must double maxTargetRange reduction: {victim_range_scr} vs {victim_range_un}"
    );
    assert!(
        (victim_scan_rng_scr - base_victim_scan).abs() < 1e-4,
        "Targeting Range Dampening Script must leave scanResolution unpenalized"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Range-scripted Remote Sensor Dampener I");

    // 3c. Load Scan Resolution Dampening Script (29013) and activate against victim_id
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(damp_scan_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot4 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Scan Resolution Dampening Script into Remote Sensor Dampener I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate ScanRes-scripted Remote Sensor Dampener I");
    let damp_scan_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under ScanRes-scripted Sensor Dampener");
    let victim_range_scan_scr = extract_item_attr(&damp_scan_info, 76);
    let victim_scan_scr = extract_item_attr(&damp_scan_info, 564);
    assert!(
        (victim_range_scan_scr - base_victim_range).abs() < 1e-4,
        "Scan Resolution Dampening Script must leave maxTargetRange unpenalized"
    );
    assert!(
        victim_scan_scr < victim_scan_un,
        "Scan Resolution Dampening Script must double scanResolution reduction: {victim_scan_scr} vs {victim_scan_un}"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(damp_mod_id),
            PyRep::String("remoteSensorDampFalloff".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate ScanRes-scripted Remote Sensor Dampener I");

    // 4. Verify Tracking Disruptor & Guidance Disruptor with Scripts via dogmaIM.Activate (feat:modules.tracking-disruptor)
    // Baseline target turret (2889 200mm AutoCannon II) and missile launcher (499 Light Missile Launcher I + 210 Scourge Light Missile) attributes
    let base_turret_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo baseline turret");
    let base_optimal = extract_item_attr(&base_turret_info, 54);
    let base_falloff = extract_item_attr(&base_turret_info, 158);
    let base_tracking = extract_item_attr(&base_turret_info, 160);
    assert!(base_optimal > 0.0 && base_falloff > 0.0 && base_tracking > 0.0);

    let base_launcher_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo baseline launcher");
    let base_missile_vel = extract_item_attr(&base_launcher_info, 37);
    let base_flight_time = extract_item_attr(&base_launcher_info, 281);
    let base_exp_vel = extract_item_attr(&base_launcher_info, 653);
    let base_exp_radius = extract_item_attr(&base_launcher_info, 654);
    assert!(
        base_missile_vel > 0.0
            && base_flight_time > 0.0
            && base_exp_vel > 0.0
            && base_exp_radius > 0.0
    );

    // 4a. Unscripted Tracking Disruptor I (2108) -> reduces turret optimal (54), falloff (158), and trackingSpeed (160)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate unscripted Tracking Disruptor I");
    let td_unscripted_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under unscripted Tracking Disruptor I");
    let td_un_opt = extract_item_attr(&td_unscripted_info, 54);
    let td_un_fall = extract_item_attr(&td_unscripted_info, 158);
    let td_un_trk = extract_item_attr(&td_unscripted_info, 160);
    assert!(td_un_opt < base_optimal, "Unscripted TD must reduce target turret optimal range");
    assert!(td_un_fall < base_falloff, "Unscripted TD must reduce target turret falloff");
    assert!(td_un_trk < base_tracking, "Unscripted TD must reduce target turret tracking speed");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Tracking Disruptor I");

    // 4b. Tracking Disruptor I loaded with Optimal Range Disruption Script (29005) -> doubles range/falloff penalty, zeroes tracking penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(opt_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Optimal Range Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Optimal Range scripted Tracking Disruptor I");
    let td_opt_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under Optimal Range scripted TD");
    let td_opt_opt = extract_item_attr(&td_opt_info, 54);
    let td_opt_fall = extract_item_attr(&td_opt_info, 158);
    let td_opt_trk = extract_item_attr(&td_opt_info, 160);
    assert!(td_opt_opt < td_un_opt, "Optimal Range Disruption Script must double optimal range reduction");
    assert!(td_opt_fall < td_un_fall, "Optimal Range Disruption Script must double falloff reduction");
    assert!((td_opt_trk - base_tracking).abs() < 1e-4, "Optimal Range Disruption Script must leave tracking speed unpenalized");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Optimal Range scripted TD");

    // 4c. Tracking Disruptor I loaded with Tracking Speed Disruption Script (29007) -> doubles tracking penalty, zeroes range/falloff penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(trk_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Tracking Speed Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Tracking Speed scripted Tracking Disruptor I");
    let td_trk_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_turret_id)], &kw, None).await
        .expect("ItemGetInfo under Tracking Speed scripted TD");
    let td_trk_opt = extract_item_attr(&td_trk_info, 54);
    let td_trk_fall = extract_item_attr(&td_trk_info, 158);
    let td_trk_trk = extract_item_attr(&td_trk_info, 160);
    assert!((td_trk_opt - base_optimal).abs() < 1e-4, "Tracking Speed Disruption Script must leave optimal range unpenalized");
    assert!((td_trk_fall - base_falloff).abs() < 1e-4, "Tracking Speed Disruption Script must leave falloff unpenalized");
    assert!(td_trk_trk < td_un_trk, "Tracking Speed Disruption Script must double tracking speed reduction");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Tracking Speed scripted TD");

    // 4d. Unscripted Guidance Disruptor I (37543) -> reduces missile velocity (37), flight time (281), explosion velocity (653), and expands explosion radius (654)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate unscripted Guidance Disruptor I");
    let gd_un_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under unscripted Guidance Disruptor I");
    let gd_un_vel = extract_item_attr(&gd_un_info, 37);
    let gd_un_time = extract_item_attr(&gd_un_info, 281);
    let gd_un_exp_vel = extract_item_attr(&gd_un_info, 653);
    let gd_un_exp_rad = extract_item_attr(&gd_un_info, 654);
    assert!(gd_un_vel < base_missile_vel, "Unscripted Guidance Disruptor must reduce target missile velocity");
    assert!(gd_un_time < base_flight_time, "Unscripted Guidance Disruptor must reduce target missile flight time");
    assert!(gd_un_exp_vel < base_exp_vel, "Unscripted Guidance Disruptor must reduce target missile explosion velocity");
    assert!(gd_un_exp_rad > base_exp_radius, "Unscripted Guidance Disruptor must increase target missile explosion radius");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate unscripted Guidance Disruptor I");

    // 4e. Guidance Disruptor I loaded with Missile Range Disruption Script (40334) -> doubles velocity/flight-time penalty, zeroes precision penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(m_rng_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot3 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Missile Range Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Missile Range scripted Guidance Disruptor I");
    let gd_rng_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under Missile Range scripted GD");
    let gd_rng_vel = extract_item_attr(&gd_rng_info, 37);
    let gd_rng_time = extract_item_attr(&gd_rng_info, 281);
    let gd_rng_exp_vel = extract_item_attr(&gd_rng_info, 653);
    let gd_rng_exp_rad = extract_item_attr(&gd_rng_info, 654);
    assert!(gd_rng_vel < gd_un_vel, "Missile Range Disruption Script must double missile velocity reduction");
    assert!(gd_rng_time < gd_un_time, "Missile Range Disruption Script must double missile flight time reduction");
    assert!((gd_rng_exp_vel - base_exp_vel).abs() < 1e-4, "Missile Range Disruption Script must leave explosion velocity unpenalized");
    assert!((gd_rng_exp_rad - base_exp_radius).abs() < 1e-4, "Missile Range Disruption Script must leave explosion radius unpenalized");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Missile Range scripted GD");

    // 4f. Guidance Disruptor I loaded with Missile Precision Disruption Script (40335) -> doubles explosion velocity/radius penalty, zeroes range penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(m_prec_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot3 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Missile Precision Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Missile Precision scripted Guidance Disruptor I");
    let gd_prec_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(target_launcher_id)], &kw, None).await
        .expect("ItemGetInfo under Missile Precision scripted GD");
    let gd_prec_vel = extract_item_attr(&gd_prec_info, 37);
    let gd_prec_time = extract_item_attr(&gd_prec_info, 281);
    let gd_prec_exp_vel = extract_item_attr(&gd_prec_info, 653);
    let gd_prec_exp_rad = extract_item_attr(&gd_prec_info, 654);
    assert!((gd_prec_vel - base_missile_vel).abs() < 1e-4, "Missile Precision Disruption Script must leave missile velocity unpenalized");
    assert!((gd_prec_time - base_flight_time).abs() < 1e-4, "Missile Precision Disruption Script must leave missile flight time unpenalized");
    assert!(gd_prec_exp_vel < gd_un_exp_vel, "Missile Precision Disruption Script must double explosion velocity reduction");
    assert!(gd_prec_exp_rad > gd_un_exp_rad, "Missile Precision Disruption Script must double explosion radius expansion");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Missile Precision scripted GD");

    // 5. Verify Target Painter with Overload via dogmaIM.Activate and ItemGetInfo (feat:modules.target-painter)
    // Target Painter I (12709): signatureRadiusBonus = 25.0%
    let tp_item = DogmaItemState::from_sde(8, 12709, &ctx.sde);
    let tp_unloaded = eval_target_painter_with_overload(&tp_item, false, 10_000.0, &ctx.sde);
    assert!((tp_unloaded - 1.25).abs() < 1e-4, "Target painter must expand sig radius by 25%");

    // Overloaded painter gains +20% bonus (25% * 1.20 = 30% bonus -> 1.30 multiplier)
    let tp_overloaded = eval_target_painter_with_overload(&tp_item, true, 10_000.0, &ctx.sde);
    assert!((tp_overloaded - 1.30).abs() < 1e-4, "Overloaded target painter must expand sig radius by 30%");

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(tp_mod_id),
            PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Target Painter I via dogmaIM.Activate");
    let tp_norm_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Target Painter I");
    let victim_sig_norm = extract_item_attr(&tp_norm_info, 552);
    assert!(
        victim_sig_norm > base_victim_sig * 1.24,
        "Activated Target Painter I must bloom victim signatureRadius (552) by >=25%: {victim_sig_norm} vs {base_victim_sig}"
    );
    assert_eq!(
        (victim_sig_norm / base_victim_sig * 100.0).round() as i64,
        125
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(tp_mod_id),
            PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Target Painter I");

    // Overload Target Painter I and re-activate
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(tp_mod_id)], &kw, None).await
    .expect("Overload Target Painter I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(tp_mod_id),
            PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Overloaded Target Painter I");
    let tp_ovl_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(victim_id)], &kw, None).await
        .expect("ItemGetInfo victim under Overloaded Target Painter I");
    let victim_sig_ovl = extract_item_attr(&tp_ovl_info, 552);
    assert!(
        victim_sig_ovl > victim_sig_norm,
        "Overloaded Target Painter I must bloom victim signatureRadius (552) more than non-overloaded: {victim_sig_ovl} vs {victim_sig_norm}"
    );
    assert_eq!(
        (victim_sig_ovl / base_victim_sig * 100.0).round() as i64,
        130
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(tp_mod_id),
            PyRep::String("targetGunneryMaxRangeAndTrackingSpeedBonusHostile".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Overloaded Target Painter I");

    // 6. Verify Stasis Webifier & Warp Scrambler (DG-5)
    // Stasis Webifier I (526): speedFactor = -50.0%
    let web_item = DogmaItemState::from_sde(9, 526, &ctx.sde);
    let slowed_vel = eval_stasis_webifier_stacking(&[&web_item], &[5_000.0], 365.0, &ctx.sde);
    assert!((slowed_vel - 182.5).abs() < 1e-2, "Webifier must reduce velocity by 50%");

    // Warp Scrambler I (447): 2 warp scramble points and blocks MWD
    let scram_item = DogmaItemState::from_sde(10, 447, &ctx.sde);
    let scram_state = eval_warp_scrambler_effects(&[&scram_item], &[5_000.0], &ctx.sde);
    assert_eq!(scram_state.scramble_strength, 2);
    assert!(scram_state.blocks_mwd, "Warp scrambler must suppress MWD");

    // 7. Verify Target Lock Time & Max Locked Targets (DG-5)
    // Target lock time formula: t = 40000 / (scan_res * asinh(sig)^2)
    let t_lock_fast = compute_target_lock_time(660.0, 100.0); // cruiser size
    let t_lock_slow = compute_target_lock_time(660.0, 35.0);  // frigate size
    assert!(t_lock_slow > t_lock_fast, "Smaller signature radius takes longer to lock");

    // Max locked targets calculation with skills
    // Hull has 4 max targets, Target Management IV (+4) -> 2 + 4 = 6, capped by hull (4)
    let max_t1 = compute_effective_max_locked_targets(4, 4, 0);
    assert_eq!(max_t1, 4);
    // Hull has 10 max targets (Battleship), TM V (+5) -> 2 + 5 = 7
    let max_t2 = compute_effective_max_locked_targets(10, 5, 0);
    assert_eq!(max_t2, 7);
    // Battleship with TM V and Advanced TM III (+3) -> 2 + 5 + 3 = 10
    let max_t3 = compute_effective_max_locked_targets(10, 5, 3);
    assert_eq!(max_t3, 10);

    // Lock break on range: target moves beyond maxTargetRange
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
    let src_ball_id = 990_000_040i64;
    let tgt_ball_id = 990_000_041i64;
    ballpark.add_ball(Ball {
        id: src_ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(src_ball_id, CombatState::default());
    ballpark.add_ball(Ball {
        id: tgt_ball_id,
        type_id: 587,
        position: DVec3::new(20_000.0, 0.0, 0.0), // 20 km
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(tgt_ball_id, CombatState::default());
    ballpark.add_target_lock(src_ball_id, TargetLock {
        target_id: tgt_ball_id,
        lock_time_ms: 1000.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: Some(25_000.0), // 25 km max range
    });
    // Within range -> retained
    ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 1);

    // Target moves to 30 km (> 25 km max range) -> lock breaks!
    if let Some(tb) = ballpark.balls.get_mut(&tgt_ball_id) {
        tb.position = DVec3::new(30_000.0, 0.0, 0.0);
    }
    let step_events = ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 0, "Lock must break when target exceeds max range");
    assert!(step_events.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, .. } if mode == "lost")));
}

