//! Part 2 negative/regression integration tests for CRAB Beacon Exploits
//! (`11019` subcapital rejection, simultaneous double-link refusal, non-compounding
//! dogma resonance bonus/restore, Rogue Drone scan-jamming) and Overlapping Metaliminal
//! Storms with Cross-Instance Isolation (`feat:pve.crab-link-activation`,
//! `feat:pve.crab-ceq-signal-interference`, `feat:pve.metaliminal-storm-movement`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
use eve_server::services::pve_crab_beacons::{
    tick_all_crab_beacons, CrabBeaconPhase, CRAB_LINK_DURATION_SECS, CRAB_LOOT_EXCLUSIVE_SECS,
    CRAB_SCAN_DURATION_SECS, TYPE_CRAB_BEACON,
};
use eve_server::services::pve_metaliminal_storms::{
    compute_storm_footprint, sync_metaliminal_storm_effects_and_sites,
};
use eve_server::services::pve_special_sites::{
    with_pve_special_reg, AbyssalWeatherKind, RoamingStormRecord,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
const TYPE_ARCHON: u32 = 23_757; // Carrier
const TYPE_COCKROACH: u32 = 11_019; // Subcapital QA frigate (must NOT bypass capital check)
const TYPE_EM_SHIELD_AMP_I: u32 = 1_808; // Passive EM Shield Amplifier I (T1)

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11_584,
            TrainedSkillRecord {
                type_id: 11_584,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3_425,
            TrainedSkillRecord {
                type_id: 3_425,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 450.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

// ============================================================================
// 1. CRAB Exploit Guards: 11019 Subcap, Simultaneous Link, Dogma Resonance & Drone Jam
// ============================================================================
#[tokio::test]
async fn test_crab_negative_exploits_subcap_11019_double_link_resonance_and_drone_jam() {
    // parity: feat:pve.crab-link-activation, feat:pve.crab-ceq-signal-interference
    eve_testkit::attest(&[
        "feat:pve.crab-link-activation",
        "feat:pve.crab-ceq-signal-interference",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let pos = DVec3::new(320_000_000.0, 20_000_000.0, 120_000_000.0);

    // 1. Subcapital type_id 11019 (Cockroach) must be rejected with IneligibleShipForCrabLink
    let roach_id = 731_001_001i64;
    let mut sess_roach = setup_pilot(&ctx, 8501, PILOT_1, roach_id, TYPE_COCKROACH, LOWSEC_SYS, pos).await;
    let b1 = 731_100_001i64;
    let b2 = 731_100_002i64;
    for bid in [b1, b2] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: PILOT_1,
                    location_id: roach_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut sess_roach, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
    }

    let roach_err = ctx
        .dispatch_rpc(
            &mut sess_roach,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b1),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("11019 Cockroach must not bypass CRAB capital ship restriction");
    assert_eq!(roach_err.code, "IneligibleShipForCrabLink");

    // 2. Carrier links b1; attempting to link b2 while already linked to b1 fails with ShipAlreadyLinkedToCrab
    //    WITHOUT consuming CEQ or increasing signal interference!
    let carrier_id = 731_001_002i64;
    let mut sess_cap = setup_pilot(&ctx, 8502, PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, pos).await;
    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let clean_base_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;

    let link1 = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b1),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Carrier links b1");
    assert_eq!(link1.dict_get("ceqRemaining").and_then(|v| v.as_i64()), Some(70));

    let double_err = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b2),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Already-linked carrier cannot simultaneously link a 2nd CRAB");
    assert_eq!(double_err.code, "ShipAlreadyLinkedToCrab");
    let ceq_after_reject = ctx
        .dispatch_rpc(&mut sess_cap, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(
        ceq_after_reject.dict_get("currentEnergy").and_then(|v| v.as_i64()),
        Some(70),
        "Rejected double-link must not deduct CEQ"
    );

    // 3. Fit a passive shield resistance module DURING the active link; when the 4-minute link ends,
    //    clear_crab_link_ship_effects must rebuild dogma resonances (preserving the fitted module)
    //    rather than overwriting with pre-link snapshot resonances or compounding 0.90!
    let amp_id = 731_009_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            amp_id,
            InventoryItemRecord {
                item_id: amp_id,
                type_id: TYPE_EM_SHIELD_AMP_I,
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(amp_id, true);
    });
    ctx.invalidate_ship_fitting(carrier_id);
    let expected_post_fit_em_res = ctx.build_ship_combat_state(PILOT_1, carrier_id, TYPE_ARCHON).shield_resonance[0];
    assert!(
        expected_post_fit_em_res < clean_base_res[0],
        "Fitted EM Shield Amplifier lowers EM damage resonance"
    );

    ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    let post_unlink_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;
    assert!(
        (post_unlink_res[0] - expected_post_fit_em_res).abs() < 1e-6,
        "Unlinking must restore current dogma-fitted EM resonance ({expected_post_fit_em_res}), got {}",
        post_unlink_res[0]
    );

    // 4. Rogue Drone Jamming: advance 10m (600s) + extra 300s WITHOUT killing Rogue Drones ->
    //    CRAB beacon stays in Scanning phase (`CrabScanInProgress`), and ONLY transitions to
    //    LootRestricted with a full 120s deployer window once all 3 Rogue Drone waves are killed!
    ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 300);
    let (phase_jammed, waves_jammed, live_drones) = with_pve_special_reg(&ctx, |reg| {
        let b = &reg.crab_beacons[&b1];
        (b.phase, b.waves_spawned, b.wave_npc_ball_ids.clone())
    });
    assert_eq!(waves_jammed, 3);
    assert!(!live_drones.is_empty());
    assert_eq!(
        phase_jammed,
        CrabBeaconPhase::Scanning,
        "CRAB scan must remain paused while Rogue Drone waves are alive"
    );
    assert_eq!(
        validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.unwrap_err().code,
        "CrabScanInProgress"
    );

    for bid in live_drones {
        let _ = sys_handle
            .apply_damage_with_attacker(
                bid,
                DamageVector {
                    em: 1.0e6,
                    thermal: 1.0e6,
                    kinetic: 1.0e6,
                    explosive: 1.0e6,
                },
                Some(PILOT_1),
            )
            .await;
    }
    tick_all_crab_beacons(&ctx);
    let (phase_cleared, sd_at) = with_pve_special_reg(&ctx, |reg| {
        let b = &reg.crab_beacons[&b1];
        (b.phase, b.self_destruct_at_sec)
    });
    assert_eq!(phase_cleared, CrabBeaconPhase::LootRestricted);
    assert_eq!(sd_at, ctx.now_unix() + CRAB_LOOT_EXCLUSIVE_SECS);
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.is_ok());
}

// ============================================================================
// 2. Overlapping Storms Stack Both Weather Modifiers & Isolate Per-ServerContext
// ============================================================================
#[tokio::test]
async fn test_overlapping_storms_stack_effects_without_cross_context_leakage() {
    // parity: feat:pve.metaliminal-storm-movement
    eve_testkit::attest(&["feat:pve.metaliminal-storm-movement"]);

    let ctx1 = ServerContext::new();
    let ctx2 = ServerContext::new();
    crate::common::pin_clock(&ctx1);
    crate::common::pin_clock(&ctx2);

    let overlap_sys = 30_000_208_u32;
    let ship_id = 731_050_001_i64;
    let _sess1 = setup_pilot(&ctx1, 8510, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;
    let _sess2 = setup_pilot(&ctx2, 8511, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;

    let (core_a, periph_a) = compute_storm_footprint(&ctx1, overlap_sys);
    let (core_b, periph_b) = compute_storm_footprint(&ctx1, overlap_sys);
    with_pve_special_reg(&ctx1, |reg| {
        reg.roaming_storms = vec![
            RoamingStormRecord {
                storm_id: 201,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: overlap_sys,
                strong_core_systems: core_a,
                weak_periphery_systems: periph_a,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
            RoamingStormRecord {
                storm_id: 202,
                weather: AbyssalWeatherKind::Firestorm,
                center_solar_system_id: overlap_sys,
                strong_core_systems: core_b,
                weak_periphery_systems: periph_b,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
        ];
    });
    sync_metaliminal_storm_effects_and_sites(&ctx1);

    // Electrical lowers capacitor recharge time (attr 55: *0.50); Firestorm boosts armor HP (attr 265: *1.50)
    let mut attrs1 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx1, ship_id, &mut attrs1);
    assert!((attrs1[&55] - 50.0).abs() < 1e-6, "Electrical storm modifier applied");
    assert!((attrs1[&265] - 1_500.0).abs() < 1e-6, "Firestorm modifier stacked simultaneously");

    // Separate ServerContext ctx2 in the same solar system has NO storm beacons and must stay unaffected
    let mut attrs2 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx2, ship_id, &mut attrs2);
    assert_eq!(attrs2[&55], 100.0, "Storm effects in ctx1 must not leak into ctx2");
    assert_eq!(attrs2[&265], 1_000.0, "Storm effects in ctx1 must not leak into ctx2");
}

// ============================================================================
// 3. Electrical Storm Hacking Virus Coherence Bonus (+25 Core / +10 Periphery)
// ============================================================================
#[tokio::test]
async fn test_electrical_storm_grants_hacking_virus_coherence_per_context() {
    // parity: feat:pve.metaliminal-storms, feat:pve.hacking-node-types
    eve_testkit::attest(&["feat:pve.metaliminal-storms", "feat:pve.hacking-node-types"]);

    let ctx1 = ServerContext::new();
    let ctx2 = ServerContext::new();
    crate::common::pin_clock(&ctx1);
    crate::common::pin_clock(&ctx2);

    let center_sys = 30_000_208_u32;
    let (core_sys_list, periph_sys_list) = compute_storm_footprint(&ctx1, center_sys);
    let periph_sys = periph_sys_list
        .iter()
        .copied()
        .find(|s| !core_sys_list.contains(s))
        .expect("distinct periphery system");

    let ship_core = 731_060_001_i64;
    let mut sess_core = setup_pilot(&ctx1, 8520, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;
    let sess_iso = setup_pilot(&ctx2, 8522, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;

    for (ctx, char_id, ship_id, mod_id) in [
        (&ctx1, PILOT_1, ship_core, 731_060_101_i64),
        (&ctx2, PILOT_1, ship_core, 731_060_103_i64),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                21718,
                eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 },
            );
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 22175, // Data Analyzer I (40 base coherence, 20 strength)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
    }

    let (base_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("baseline analyzer stats");
    assert_eq!(base_coh, 90, "Baseline Data Analyzer I + Hacking V coherence = 90");

    with_pve_special_reg(&ctx1, |reg| {
        reg.roaming_storms = vec![RoamingStormRecord {
            storm_id: 301,
            weather: AbyssalWeatherKind::Electrical,
            center_solar_system_id: center_sys,
            strong_core_systems: core_sys_list,
            weak_periphery_systems: periph_sys_list,
            step_count: 0,
            next_move_at_sec: u64::MAX,
            recent_centers: vec![center_sys],
        }];
    });
    sync_metaliminal_storm_effects_and_sites(&ctx1);

    let (core_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("core storm analyzer stats");
    let (iso_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx2, &sess_iso, PILOT_1, false,
    )
    .expect("isolated context analyzer stats");

    assert_eq!(core_coh, 115, "Strong Electrical Storm core grants +25 virus coherence (90 -> 115)");
    assert_eq!(iso_coh, 90, "Electrical Storm in ctx1 must not leak coherence bonus into ctx2");

    // Move pilot in ctx1 into the weak storm periphery system and verify +10 coherence bonus.
    sess_core.solar_system_id = Some(periph_sys);
    ctx1.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = periph_sys;
        }
        if let Some(ship) = snap.items.get_mut(&ship_core) {
            ship.location_id = i64::from(periph_sys);
        }
    });
    let (periph_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("periphery storm analyzer stats");
    assert_eq!(periph_coh, 100, "Weak Electrical Storm periphery grants +10 virus coherence (90 -> 100)");
}
