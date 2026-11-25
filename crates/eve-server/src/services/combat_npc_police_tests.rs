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

#[tokio::test]
async fn cmd_stargate_jump_contraband_confiscation_emits_on_item_change() {
    use crate::router::delta_item_switch::ItemWireFingerprint;
    use eve_sde::StargateJump;

    let (mut ctx, mut session, char_id, ship_id, dest_sys_id) = make_fighter_ctx_and_session();
    let origin_sys_id = 30_099_142_u32;
    let origin_gate_id = 50_099_001_i64;
    let dest_gate_id = 50_099_002_i64;
    let booster_type_id = 945_001_u32;
    let booster_item_id = 888_777_i64;

    let mut sde = (*ctx.sde).clone();
    // Use a subcapital frigate (Rifter 587, group 25) because carriers (23915) cannot jump stargates into high-sec.
    sde.types
        .entry(587)
        .or_insert_with(|| make_fighter_type(587, 25, 6, 27_000.0, vec![]));
    let mut booster = make_fighter_type(booster_type_id, 303, 20, 1.0, vec![(1089, 20.0)]);
    booster.base_price = 50_000.0;
    sde.types.insert(booster_type_id, booster);
    let mut origin_sys = sde
        .solar_systems
        .get(&dest_sys_id)
        .expect("canonical seed solar system")
        .clone();
    origin_sys.solar_system_id = origin_sys_id;
    origin_sys.name = "ContrabandOrigin".into();
    origin_sys.security_status = 0.2;
    origin_sys.stations.clear();
    sde.solar_systems.insert(origin_sys_id, origin_sys);
    if let Some(dest_sys) = sde.solar_systems.get_mut(&dest_sys_id) {
        dest_sys.security_status = dest_sys.security_status.max(0.9);
    }
    let origin_gate_pos = DVec3::new(10_000.0, 0.0, 0.0);
    sde.jumps.insert(
        origin_gate_id,
        StargateJump {
            stargate_id: origin_gate_id,
            from_solar_system_id: origin_sys_id,
            to_solar_system_id: dest_sys_id,
            destination_stargate_id: dest_gate_id,
            position: origin_gate_pos,
        },
    );
    sde.jumps.insert(
        dest_gate_id,
        StargateJump {
            stargate_id: dest_gate_id,
            from_solar_system_id: dest_sys_id,
            to_solar_system_id: origin_sys_id,
            destination_stargate_id: origin_gate_id,
            position: DVec3::new(-10_000.0, 0.0, 0.0),
        },
    );
    ctx = ServerContext::with_sde_and_repository(std::sync::Arc::new(sde), ctx.db.clone());
    ctx.db
        .acquire_online_lock(1, 99_201, Some(char_id), true, 1_700_000_500)
        .expect("online lock");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.balance = eve_db::IskCents::from_isk(1_000_000);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 587;
            ship.location_id = i64::from(origin_sys_id);
            ship.flag = InventoryFlag::None;
        }
        snap.items.insert(
            booster_item_id,
            make_fighter_item(
                booster_item_id,
                booster_type_id,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                2,
            ),
        );
    });
    session.session_id = 99_201;
    session.solar_system_id = Some(origin_sys_id);
    session.station_id = None;
    session.structure_id = None;
    session.ship_id = Some(ship_id);

    ctx.systems
        .get_or_spawn(origin_sys_id)
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: origin_gate_pos,
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
        .expect("spawn ship at origin gate");
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.dispatch_rpc(
        &mut session,
        "beyonce",
        None,
        "CmdStargateJump",
        &[eve_proto::PyRep::Integer(origin_gate_id)],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("CmdStargateJump into high-sec must succeed");

    assert_eq!(session.solar_system_id, Some(dest_sys_id));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&booster_item_id)));

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnRemoteMessage"),
        "expected ParamCustomsNotification OnRemoteMessage, got {notifs:?}"
    );
    let item_changes: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args.first().and_then(ItemWireFingerprint::from_pyrep_row))
        .filter(|(iid, _)| *iid == booster_item_id)
        .collect();
    assert_eq!(
        item_changes.len(),
        1,
        "CmdStargateJump contraband confiscation must deliver 1 OnItemChange even when char_id is in prior_transitioned_chars: {notifs:?}"
    );
    assert_eq!(item_changes[0].1.location_id, 6);
    assert_eq!(item_changes[0].1.flag_id, 0);
}

#[cfg(test)]
#[path = "combat_npc_police_extra_unit_tests.rs"]
mod combat_npc_police_extra_unit_tests;
