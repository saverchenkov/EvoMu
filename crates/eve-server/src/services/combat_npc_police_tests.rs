//! Unit and mutation-killing tests for `combat_npc_police`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{InventoryFlag, SystemDynamicState};
use eve_destiny::{Ball, CombatState, MovementMode};
use glam::DVec3;

#[test]
fn faction_police_and_navy_lookup_tables_and_combat_properties() {
    assert_eq!(police_ship_type_for_faction(500_001), 9_970);
    assert_eq!(police_ship_type_for_faction(500_002), 10_046);
    assert_eq!(police_ship_type_for_faction(500_003), 3_768);
    assert_eq!(police_ship_type_for_faction(500_004), 9_984);
    assert_eq!(police_ship_type_for_faction(999_999), 9_970);

    assert_eq!(navy_archetype_for_faction(500_001), "CaldariNavy");
    assert_eq!(navy_archetype_for_faction(500_002), "MinmatarNavy");
    assert_eq!(navy_archetype_for_faction(500_003), "AmarrNavy");
    assert_eq!(navy_archetype_for_faction(500_004), "GallenteNavy");
    assert_eq!(navy_archetype_for_faction(999_999), "Navy");

    assert_eq!(navy_patrol_ship_types_for_faction(500_001), (52_936, 52_934));
    assert_eq!(navy_patrol_ship_types_for_faction(500_002), (52_959, 52_957));
    assert_eq!(navy_patrol_ship_types_for_faction(500_003), (52_927, 52_925));
    assert_eq!(navy_patrol_ship_types_for_faction(500_004), (52_950, 52_948));
    assert_eq!(navy_patrol_ship_types_for_faction(999_999), (52_936, 52_934));

    let mut npc = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        1, 9_970, 30_000_142, "Navy",
    );
    configure_navy_combat_properties(&mut npc, 500_001, 1001, 0.5, 5.0);
    assert_eq!(npc.archetype, "CaldariNavy");
    assert_eq!(npc.target_id, Some(1001));
    assert!((npc.scram_range - 24_000.0).abs() < 1e-6);
    assert!((npc.web_speed_factor - (-40.0)).abs() < 1e-6);
    assert!(npc.damage_vector.kinetic > npc.damage_vector.thermal);

    configure_navy_combat_properties(&mut npc, 500_002, 0, 1.0, 0.0);
    assert_eq!(npc.archetype, "MinmatarNavy");
    assert!((npc.web_speed_factor - (-65.0)).abs() < 1e-6);
    assert!(npc.damage_vector.explosive > npc.damage_vector.kinetic);

    configure_navy_combat_properties(&mut npc, 500_003, 0, 1.0, 0.0);
    assert_eq!(npc.archetype, "AmarrNavy");
    assert!((npc.neut_amount - 250.0).abs() < 1e-6);
    assert!(npc.damage_vector.em > npc.damage_vector.thermal);

    configure_navy_combat_properties(&mut npc, 500_004, 0, 1.0, 0.0);
    assert_eq!(npc.archetype, "GallenteNavy");
    assert!((npc.scram_strength - 3.0).abs() < 1e-6);
    assert!(npc.damage_vector.thermal > npc.damage_vector.kinetic);

    configure_navy_combat_properties(&mut npc, 999_999, 0, 1.0, 0.0);
    assert_eq!(npc.archetype, "Navy");
    assert!((npc.damage_vector.em - 60.0).abs() < 1e-6);
}

#[tokio::test]
async fn outlaw_docking_contraband_confiscation_police_spawns_and_corruption_suppression() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    // Contraband booster (group 303, side-effect chance attr 1089 = 20.0, base_price = 50_000)
    let mut booster = make_fighter_type(945_001, 303, 20, 1.0, vec![(1089, 20.0)]);
    booster.base_price = 50_000.0;
    sde.types.insert(945_001, booster);
    sde.types
        .insert(670, make_fighter_type(670, 29, 6, 100.0, vec![]));
    ctx.sde = std::sync::Arc::new(sde);

    // 1. Lawful pilot in high-sec is allowed to dock
    assert!(!is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, ship_id));

    // 2. Outlaw pilot (sec_status = -5.5) in ship is refused docking, but in capsule (670) is allowed
    let pod_id = 200_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = -5.5;
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    assert!(is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, ship_id));
    assert!(!is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, pod_id));

    // 3. Seed contraband booster in cargo and spawn ship in space
    let booster_item_id = 888_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.standings.insert(500_001, -6.0);
        snap.items.insert(
            booster_item_id,
            make_fighter_item(booster_item_id, 945_001, char_id, ship_id, InventoryFlag::Cargo, 2),
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
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

    // 4. check_and_spawn_faction_police confiscates contraband, fines wallet, and spawns both Navy & FactionPolice
    let spawned = check_and_spawn_faction_police(&ctx, sys_id).await;
    assert_eq!(spawned.len(), 2);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&booster_item_id));
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "contraband_fine"));
        assert!(snap
            .standing_transactions
            .get(&char_id)
            .is_some_and(|v| v.iter().any(|t| t.event_type_id == 85)));
    });

    // 5. Insurgency corruption stage 2 suppresses faction police; stage 5 suppresses CONCORD
    assert!(!is_concord_suppressed_in_system(&ctx, sys_id));
    assert!(!is_faction_police_suppressed(&ctx, sys_id));
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = SystemDynamicState::default();
        st.solar_system_id = sys_id;
        st.insurgency_corruption_stage = 5;
        snap.system_states.insert(sys_id, st);
        snap.insurgency_runtime.campaigns.insert(
            1,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(200),
                forecasting_end_unix: now.saturating_sub(100),
                active_end_unix: now + 1800,
                winddown_end_unix: now + 3600,
                corruption_points: [(sys_id, 1_000)].into_iter().collect(),
                suppression_points: Default::default(),
            },
        );
    });
    assert!(is_concord_suppressed_in_system(&ctx, sys_id));
    assert!(is_faction_police_suppressed(&ctx, sys_id));
    assert!(!is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, ship_id));
    assert!(step_empire_navy_patrols(&ctx, sys_id).await.is_empty());
}
