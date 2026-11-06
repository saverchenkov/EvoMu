//! Deep branch-coverage and mutation-killing unit tests for `combat_npc_police`.

#![forbid(unsafe_code)]

use super::combat_npc_police::{
    check_and_spawn_faction_police, configure_navy_combat_properties,
    is_concord_suppressed_in_system, is_faction_police_suppressed, is_outlaw_ship_docking_refused,
    is_sentry_disabled_by_corruption, scan_and_enforce_highsec_contraband,
    step_empire_navy_patrols,
};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity;
use eve_db::{InventoryFlag, IskCents, PersistedInsurgencyCampaign, SystemDynamicState};
use eve_destiny::{Ball, CombatState, MovementMode};
use glam::DVec3;
use std::sync::Arc;

#[test]
fn configure_navy_combat_properties_verifies_exact_scaling_and_zero_target_preservation() {
    let mut npc = make_test_npc_entity(10, 52_936, 30_000_142, "Navy");
    npc.target_id = Some(777);

    // When target_id == 0, existing target_id is preserved and threat tables are not modified for 0
    configure_navy_combat_properties(&mut npc, 500_001, 0, 0.5, 5.0);
    assert_eq!(npc.target_id, Some(777));
    assert!(!npc.aggression_threat.contains_key(&0));

    // sec_scaling = 1.0 + (1.0 - 0.5) * 0.5 = 1.25
    // standing_scaling = 1.0 + (5.0 / 5.0) * 0.5 = 1.50
    // base_dps = 240.0 * 1.25 * 1.50 = 450.0
    assert!((npc.optimal_range - 25_000.0).abs() < 1e-6);
    assert!((npc.orbit_range - 7_500.0).abs() < 1e-6);
    assert!((npc.damage_vector.kinetic - 450.0 * 0.70).abs() < 1e-6);
    assert!((npc.damage_vector.thermal - 450.0 * 0.30).abs() < 1e-6);
    assert_eq!(npc.damage_vector.em, 0.0);
    assert_eq!(npc.damage_vector.explosive, 0.0);
    assert!((npc.scram_strength - 2.0).abs() < 1e-6);
    assert!((npc.scram_range - 24_000.0).abs() < 1e-6);

    // When target_id != 0, sets target_id and inserts 800.0 into both threat maps
    configure_navy_combat_properties(&mut npc, 500_003, 999, 1.0, -2.0);
    assert_eq!(npc.target_id, Some(999));
    assert_eq!(npc.aggression_threat.get(&999), Some(&800.0));
    assert_eq!(npc.threat_table.get(&999), Some(&800.0));
    // sys_sec = 1.0 -> sec_scaling = 1.0; negative standing_deficit -> standing_scaling = 1.0; base_dps = 240.0
    assert!((npc.damage_vector.em - 240.0 * 0.65).abs() < 1e-6);
    assert!((npc.damage_vector.thermal - 240.0 * 0.35).abs() < 1e-6);
    assert!((npc.neut_amount - 250.0).abs() < 1e-6);
    assert!((npc.web_speed_factor - (-35.0)).abs() < 1e-6);
    assert!((npc.web_range - 15_000.0).abs() < 1e-6);
}

#[test]
fn docking_refusal_and_sentry_corruption_cover_lowsec_standing_and_stage_boundaries() {
    let (mut ctx, _session, char_id, ship_id, highsec_id) = make_fighter_ctx_and_session();
    let lowsec_id = 30_002_537_u32; // Amamake (low-sec)
    let nullsec_id = 30_004_759_u32;

    // Seed non-670 capsule type (33328 Genolution Capsule, group 29)
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(33_328, make_fighter_type(33_328, 29, 6, 100.0, vec![]));

    let geno_pod_id = 300_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            geno_pod_id,
            make_fighter_item(geno_pod_id, 33_328, char_id, highsec_id as i64, InventoryFlag::None, 1),
        );
    });

    // 1. Pilot with clean sec_status (0.0) but faction standing == -5.0 is refused docking in ship, allowed in group-29 pod
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.security_status = 0.0;
        ch.standings.insert(500_001, -5.0);
    });
    assert!(is_outlaw_ship_docking_refused(&ctx, highsec_id, char_id, ship_id));
    assert!(!is_outlaw_ship_docking_refused(&ctx, highsec_id, char_id, geno_pod_id));

    // Standing just above threshold (-4.99) is allowed
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(500_001, -4.99);
    });
    assert!(!is_outlaw_ship_docking_refused(&ctx, highsec_id, char_id, ship_id));

    // 2. Outlaw (-10.0) in low-sec is never refused docking by high-sec faction police
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = -10.0;
    });
    assert!(!is_outlaw_ship_docking_refused(&ctx, lowsec_id, char_id, ship_id));

    // 3. Sentry corruption disabling only applies in low-sec (0.0 < sec < 0.45) at corruption stage >= 5
    let now = ctx.now_unix();
    let seed_corruption = |ctx: &crate::router::ServerContext, sys: u32, stage: u8, pts: i64| {
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.insert(
                sys,
                SystemDynamicState {
                    solar_system_id: sys,
                    insurgency_corruption_stage: stage,
                    ..Default::default()
                },
            );
            snap.insurgency_runtime.campaigns.insert(
                sys as i64,
                PersistedInsurgencyCampaign {
                    campaign_id: sys as i64,
                    pirate_faction_id: 500_010,
                    target_faction_id: 500_001,
                    fob_solar_system_id: sys,
                    systems: vec![sys],
                    created_at_unix: now.saturating_sub(200),
                    forecasting_end_unix: now.saturating_sub(100),
                    active_end_unix: now + 1800,
                    winddown_end_unix: now + 3600,
                    corruption_points: [(sys, pts)].into_iter().collect(),
                    suppression_points: Default::default(),
                },
            );
        });
    };

    // Stage 2 in highsec: suppresses faction police, does NOT suppress CONCORD, does NOT disable lowsec sentries
    seed_corruption(&ctx, highsec_id, 2, 100);
    assert!(is_faction_police_suppressed(&ctx, highsec_id));
    assert!(!is_concord_suppressed_in_system(&ctx, highsec_id));
    assert!(!is_sentry_disabled_by_corruption(&ctx, highsec_id));

    // Stage 5 in highsec and nullsec: sentry_disabled_by_corruption is false (not low-sec!)
    seed_corruption(&ctx, highsec_id, 5, 1_000);
    seed_corruption(&ctx, nullsec_id, 5, 1_000);
    assert!(!is_sentry_disabled_by_corruption(&ctx, highsec_id));
    assert!(!is_sentry_disabled_by_corruption(&ctx, nullsec_id));

    // Stage 4 in lowsec -> false; Stage 5 in lowsec -> true
    seed_corruption(&ctx, lowsec_id, 4, 400);
    assert!(!is_sentry_disabled_by_corruption(&ctx, lowsec_id));
    seed_corruption(&ctx, lowsec_id, 5, 1_000);
    assert!(is_sentry_disabled_by_corruption(&ctx, lowsec_id));
}

#[tokio::test]
async fn contraband_edge_cases_and_idempotent_police_and_navy_patrol_spawning() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Legal booster (group 303, side-effect attr 1089 == 0.0) -> NOT confiscated
    sde_mut
        .types
        .insert(946_001, make_fighter_type(946_001, 303, 20, 1.0, vec![(1089, 0.0)]));
    // Contraband booster with low base_price (100.0 -> clamped to 10_000.0 per unit)
    let mut illegal_booster = make_fighter_type(946_002, 303, 20, 1.0, vec![(1089, 15.0)]);
    illegal_booster.base_price = 100.0;
    sde_mut.types.insert(946_002, illegal_booster);

    // 1.Docked character is not scanned
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
        snap.items.insert(
            8001,
            make_fighter_item(8001, 946_002, char_id, ship_id, InventoryFlag::Cargo, 3),
        );
    });
    assert!(scan_and_enforce_highsec_contraband(&ctx, sys_id, ship_id).is_empty());

    // 2. Undock character, add legal booster + non-cargo illegal booster (in Hangar) + cargo illegal booster
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.balance = IskCents(500_000); // 5,000 ISK (less than 30,000 ISK fine -> deducts only available 5,000 ISK)
        snap.items.insert(
            8002,
            make_fighter_item(8002, 946_001, char_id, ship_id, InventoryFlag::Cargo, 2),
        );
        snap.items.insert(
            8003,
            make_fighter_item(8003, 946_002, char_id, ship_id, InventoryFlag::Hangar, 1),
        );
    });

    let found = scan_and_enforce_highsec_contraband(&ctx, sys_id, ship_id);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, 946_002);
    assert_eq!(found[0].1, 3);
    assert!((found[0].2 - 30_000.0).abs() < 1e-6);

    ctx.db.with_snapshot(|snap| {
        // Illegal cargo item removed, legal booster (8002) and non-cargo item (8003) untouched
        assert!(!snap.items.contains_key(&8001));
        assert!(snap.items.contains_key(&8002));
        assert!(snap.items.contains_key(&8003));
        // Balance clamped at 0 instead of going negative
        assert_eq!(snap.characters[&char_id].balance, IskCents(0));
    });

    // 3. Spawn empire navy patrols -> spawns 2 ships (cruiser + frigate), second call is idempotent
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let patrol_first = step_empire_navy_patrols(&ctx, sys_id).await;
    assert_eq!(patrol_first.len(), 2);
    let patrol_second = step_empire_navy_patrols(&ctx, sys_id).await;
    assert!(patrol_second.is_empty());

    // 4. Outlaw check_and_spawn_faction_police is idempotent on second call
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.security_status = -6.0;
        ch.standings.insert(500_001, -6.0);
    });
    let police_first = check_and_spawn_faction_police(&ctx, sys_id).await;
    assert_eq!(police_first.len(), 2);
    let police_second = check_and_spawn_faction_police(&ctx, sys_id).await;
    assert!(police_second.is_empty());
}
