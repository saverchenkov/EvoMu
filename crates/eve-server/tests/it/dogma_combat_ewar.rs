//! Dogma Engine & Combat EWAR / Tackle / Capacitor Warfare integration tests (DG-5).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn seed_test_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0,
            is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(skill_id, level) in skills {
            entry.insert(skill_id, eve_db::TrainedSkillRecord { type_id: skill_id, level, skill_points: 256_000.0 });
        }
    });
}

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else { return; };
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
                    let lvl = t.default_attributes.get(&l_attr).copied().unwrap_or(5.0).max(5.0) as u8;
                    entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
                }
            }
        }
    });
}

fn extract_item_attr(info_rep: &PyRep, attr_id: i64) -> f64 {
    if let PyRep::Dict(entries) = info_rep.dict_get("attributes").expect("attributes dict in ItemGetInfo") {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) { return v.as_f64().unwrap_or(0.0); }
        }
    }
    0.0
}

#[tokio::test]
async fn test_dg5_web_grappler_neut_nos_burst_projector_and_targeters() {
    // parity: plan:DG-5, feat:modules.stasis-webifier, feat:modules.heavy-stasis-grappler-falloff, feat:modules.energy-neutralizer, feat:modules.nosferatu-transfer-rule, feat:modules.burst-projectors, feat:modules.auto-targeting-system, feat:modules.passive-targeter, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:modules.stasis-webifier",
        "feat:modules.heavy-stasis-grappler-falloff",
        "feat:modules.energy-neutralizer",
        "feat:modules.nosferatu-transfer-rule",
        "feat:modules.burst-projectors",
        "feat:modules.auto-targeting-system",
        "feat:modules.passive-targeter",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4150);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade attacker hull to Aeon (23919, Supercarrier, 8 HiSlots + 6 MedSlots) while docked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 23919;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    for tid in [23919u32, 526, 41057, 533, 530, 40698, 40635, 1182, 581, 587] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    grant_skills(
        &ctx,
        char_id,
        &[
            (3426, 5), // CPU Management
            (3413, 5), // Power Grid Management
            (3418, 5), // Capacitor Management
            (3417, 5), // Capacitor Systems Operation
            (3428, 4), // Target Management IV (base 2 + 4 = 6 max locked targets before Auto Targeting System)
        ],
    );

    let web_mod_id = 100_001_501i64;
    let grap_mod_id = 100_001_502i64;
    let passive_tgt_id = 100_001_503i64;
    let neut_mod_id = 100_001_504i64;
    let nos_mod_id = 100_001_505i64;
    let auto_tgt_id = 100_001_506i64;
    let web_bp_id = 100_001_507i64;
    let neut_bp_id = 100_001_508i64;

    seed_test_item(&ctx, web_mod_id, 526, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, grap_mod_id, 41057, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, passive_tgt_id, 581, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, neut_mod_id, 533, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, nos_mod_id, 530, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, auto_tgt_id, 1182, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, web_bp_id, 40698, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, neut_bp_id, 40635, char_id, station_id, InventoryFlag::Hangar, 1);

    // Check baseline maxLockedTargets (192) before fitting Auto Targeting System I (1182)
    let ship_info_before_ats = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo before ATS");
    let max_targets_before_ats = extract_item_attr(&ship_info_before_ats, 192);

    for (mid, flag) in [
        (web_mod_id, InventoryFlag::MedSlot0),
        (grap_mod_id, InventoryFlag::MedSlot1),
        (passive_tgt_id, InventoryFlag::MedSlot2),
        (neut_mod_id, InventoryFlag::HiSlot1),
        (nos_mod_id, InventoryFlag::HiSlot2),
        (auto_tgt_id, InventoryFlag::HiSlot3),
        (web_bp_id, InventoryFlag::HiSlot4),
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
        .expect("Fit module into slot via invbroker.Add");
    }

    // 1. Verify Auto Targeting System I (1182) passive +2 maxTargetBonus (feat:modules.auto-targeting-system)
    let ship_info_after_ats = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo after ATS");
    let max_targets_after_ats = extract_item_attr(&ship_info_after_ats, 192);
    assert_eq!(
        max_targets_after_ats,
        max_targets_before_ats + 2.0,
        "Auto Targeting System I (1182) must passively add +2 to maxLockedTargets (192)"
    );

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

    // Spawn victim_id at 1 km (within Heavy Stasis Grappler II optimal = 1 km) and far_id at 25 km
    let victim_char_id = 90_000_002i64;
    let victim_id = 100_001_520i64;
    let far_id = 100_001_521i64;
    seed_test_item(&ctx, victim_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_test_item(&ctx, far_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1);
    sys.spawn_ball_with_combat(
        Ball {
            id: victim_id,
            type_id: 587,
            position: ship_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .expect("spawn victim_id");
    sys.spawn_ball_with_combat(
        Ball {
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
        },
        CombatState::default(),
    )
    .await
    .expect("spawn far_id");

    // 2. Verify Auto Targeting System I active lock acquisition (feat:modules.auto-targeting-system)
    assert_eq!(sys.get_target_locks(ship_id).await.unwrap().len(), 0);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(auto_tgt_id),
            PyRep::String("autoTarget".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Auto Targeting System I");
    for _ in 0..35 {
        let _ = sys.tick().await;
    }
    let locks_after_ats = sys.get_target_locks(ship_id).await.unwrap();
    assert!(
        locks_after_ats.iter().any(|l| l.target_id == victim_id && l.locked),
        "Active Auto Targeting System I must automatically lock targets in range"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(auto_tgt_id),
            PyRep::String("autoTarget".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Auto Targeting System I");

    // 3. Verify Passive Targeter I (581) locks target without yellow-box targetedBy notification (feat:modules.passive-targeter)
    let _ = sys.clear_target_locks(ship_id).await;
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(passive_tgt_id),
            PyRep::String("targetPassively".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Passive Targeter I");
    let passive_notifs = sess.drain_notifications();
    assert!(
        !passive_notifs.iter().any(|(name, payload)| name == "OnTarget"
            && format!("{payload:?}").contains("targetedBy")),
        "Passive Targeter I must not emit an OnTarget targetedBy yellow-box notification"
    );
    let locks_after_passive = sys.get_target_locks(ship_id).await.unwrap();
    assert!(
        locks_after_passive.iter().any(|l| l.target_id == victim_id && l.locked),
        "Passive Targeter I must acquire a target lock on victim_id"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(passive_tgt_id),
            PyRep::String("targetPassively".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Passive Targeter I");

    // 4. Verify Stasis Webifier I (526) & Heavy Stasis Grappler II (41057) falloff decay (feat:modules.stasis-webifier, feat:modules.heavy-stasis-grappler-falloff)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Stasis Webifier I");
    let (_, victim_ball_webbed) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    assert!(
        (victim_ball_webbed.max_velocity - 365.0 * 0.50).abs() < 0.5,
        "Stasis Webifier I (-50%) must reduce victim ball max_velocity from 365.0 to 182.5, got {}",
        victim_ball_webbed.max_velocity
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Stasis Webifier I");
    let (_, victim_ball_unwebbed) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    assert!(
        (victim_ball_unwebbed.max_velocity - 365.0).abs() < 0.5,
        "Deactivating Stasis Webifier I must restore victim max_velocity to 365.0"
    );

    // Activate Heavy Stasis Grappler II (41057, speedFactor = -85%, optimal = 1 km, falloffEffectiveness = 10 km) at 1 km
    let (_, curr_ship) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    sys.set_ball_position(victim_id, curr_ship.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
    sys.set_ball_position(far_id, curr_ship.position + DVec3::new(25_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(grap_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Heavy Stasis Grappler II at 1 km");
    let (_, victim_ball_grap_1km) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    assert!(
        (victim_ball_grap_1km.max_velocity - 365.0 * 0.15).abs() < 0.5,
        "Heavy Stasis Grappler II at 1 km (optimal) must reduce max_velocity by 85% (to 54.75 m/s), got {}",
        victim_ball_grap_1km.max_velocity
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(grap_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Heavy Stasis Grappler II");

    // Lock far_id at 25 km and activate Heavy Stasis Grappler II -> decays to -15% floor (365 * 0.85 = 310.25)
    sys.add_target_lock(
        ship_id,
        TargetLock {
            target_id: far_id,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(grap_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(far_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Heavy Stasis Grappler II at 25 km");
    let (_, far_ball_grap_25km) = sys.get_ball_snapshot(far_id).await.unwrap().unwrap();
    assert!(
        far_ball_grap_25km.max_velocity > victim_ball_grap_1km.max_velocity
            && far_ball_grap_25km.max_velocity <= 365.0 * 0.85 + 0.5,
        "Heavy Stasis Grappler II at 25 km must decay across falloffEffectiveness and clamp at >=15% web floor, got {}",
        far_ball_grap_25km.max_velocity
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(grap_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Heavy Stasis Grappler II on far_id");

    // 5. Verify Energy Neutralizer (533) & Nosferatu Transfer Rule (530) (feat:modules.energy-neutralizer, feat:modules.nosferatu-transfer-rule)
    // Disable passive capacitor recharge on both hulls to isolate neutralizer/nosferatu cycle transfer amounts.
    let mut victim_cs = sys.get_combat_state(victim_id).await.unwrap().unwrap();
    victim_cs.capacitor_recharge_time_ms = 0.0;
    victim_cs.max_capacitor = 1000.0;
    sys.set_combat_state(victim_id, victim_cs).await.unwrap();
    let mut ship_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs.capacitor_recharge_time_ms = 0.0;
    sys.set_combat_state(ship_id, ship_cs).await.unwrap();

    sys.set_capacitor(victim_id, 200.0).await.unwrap();
    sys.set_capacitor(ship_id, 100.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(neut_mod_id),
            PyRep::String("energyNeutralizerFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Energy Neutralizer I");
    let victim_cap_after_neut = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    assert!(
        (victim_cap_after_neut - 155.0).abs() < 0.5,
        "Small Energy Neutralizer I (45 GJ) must drain victim capacitor from 200.0 to 155.0, got {victim_cap_after_neut}"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(neut_mod_id),
            PyRep::String("energyNeutralizerFalloff".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Small Energy Neutralizer I");

    // 5b. Nosferatu when target_cap (155.0) > self_cap (80.0): transfers capacitor to self at cycle end
    sys.set_capacitor(ship_id, 80.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(nos_mod_id),
            PyRep::String("energyNosferatuFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Energy Nosferatu I when target_cap > self_cap");
    for _ in 0..6 {
        let _ = sys.tick().await;
    }
    let victim_cap_after_nos1 = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let self_cap_after_nos1 = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(
        victim_cap_after_nos1 < 150.0 && self_cap_after_nos1 > 80.0,
        "Small Energy Nosferatu I must transfer capacitor from target ({victim_cap_after_nos1}) to self ({self_cap_after_nos1}) when target_cap > self_cap"
    );

    // 5c. Nosferatu when target_cap (50.0) <= self_cap (200.0): transfers 0.0 GJ!
    sys.set_capacitor(victim_id, 50.0).await.unwrap();
    sys.set_capacitor(ship_id, 200.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(nos_mod_id),
            PyRep::String("energyNosferatuFalloff".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Energy Nosferatu I when target_cap <= self_cap");
    for _ in 0..6 {
        let _ = sys.tick().await;
    }
    let victim_cap_after_nos2 = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let self_cap_after_nos2 = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!(
        (victim_cap_after_nos2 - 50.0).abs() < 1e-4 && (self_cap_after_nos2 - 200.0).abs() < 1e-4,
        "Small Energy Nosferatu I must NOT drain target or boost self when target_cap (50) <= self_cap (200) (got victim={victim_cap_after_nos2}, self={self_cap_after_nos2})"
    );

    // 6. Verify Supercarrier Burst Projectors (40698 Stasis Webification, 40635 Energy Neutralization) (feat:modules.burst-projectors)
    sys.set_capacitor(ship_id, 50_000.0).await.unwrap();
    sys.set_capacitor(victim_id, 250.0).await.unwrap();
    sys.set_capacitor(far_id, 250.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(web_bp_id),
            PyRep::String("doomsdayAOEWeb".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Stasis Webification Burst Projector");
    let (_, victim_ball_bp_web) = sys.get_ball_snapshot(victim_id).await.unwrap().unwrap();
    let (_, far_ball_bp_web) = sys.get_ball_snapshot(far_id).await.unwrap().unwrap();
    assert!(
        victim_ball_bp_web.max_velocity < 365.0,
        "Stasis Webification Burst Projector must slow ships within AoE radius (victim_id), got {}",
        victim_ball_bp_web.max_velocity
    );
    assert!(
        (far_ball_bp_web.max_velocity - 365.0).abs() < 0.5,
        "Stasis Webification Burst Projector must not slow far_id outside AoE radius"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(web_bp_id),
            PyRep::String("doomsdayAOEWeb".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Stasis Webification Burst Projector");

    // Swap Stasis Webification Burst Projector (40698) to Energy Neutralization Burst Projector (40635) in HiSlot4 (maxGroupFitted = 1)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m_item) = snap.items.get_mut(&web_bp_id) {
            m_item.type_id = 40635;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(web_bp_id),
            PyRep::String("doomsdayAOENeut".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Energy Neutralization Burst Projector");
    let victim_cap_bp_neut = sys.get_combat_state(victim_id).await.unwrap().unwrap().capacitor;
    let far_cap_bp_neut = sys.get_combat_state(far_id).await.unwrap().unwrap().capacitor;
    assert_eq!(
        victim_cap_bp_neut, 0.0,
        "Energy Neutralization Burst Projector (1200 GJ) must drain victim_id capacitor inside AoE cloud"
    );
    assert_eq!(
        far_cap_bp_neut, 250.0,
        "Energy Neutralization Burst Projector must not drain far_id outside AoE cloud"
    );
}
