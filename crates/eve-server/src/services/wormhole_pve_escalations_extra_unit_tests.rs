//! Extra branch-coverage and mutation-killing unit tests for `wormhole_pve_escalations.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use crate::services::wormhole_pve_escalations::{
    on_asteroid_mined_in_system, on_decloaked_relay_attacked_sync,
    process_wormhole_sites_on_downtime, spawn_decloaked_transmission_relay_core,
    step_wormhole_pve_system,
};
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SolarSystemId;
use glam::DVec3;

fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: SolarSystemId,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(base) = snap.characters.values().next().cloned() {
            let mut ch = base;
            ch.character_id = char_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn relay_qualification_capital_detection_arithmos_vs_hyleus_and_roaming_warp() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    let dread_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "C4-COMBAT".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(4),
                    position: [0.0, 0.0, 0.0],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "C5-GAS".into(),
                    solar_system_id: wh_sys,
                    site_category: "Gas".into(),
                    wormhole_class: Some(5),
                    position: [0.0, 0.0, 0.0],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "C5-RELIC-SUBCAP".into(),
                    solar_system_id: wh_sys,
                    site_category: "Relic".into(),
                    wormhole_class: Some(5),
                    position: [200_000.0, 0.0, 0.0],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "C6-COMBAT-CAP".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(6),
                    position: [800_000.0, 0.0, 0.0],
                    ..Default::default()
                },
            ],
        );
    });

    for non_qual in ["NON-EXISTENT", "C4-COMBAT", "C5-GAS"] {
        assert!(spawn_decloaked_transmission_relay_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            wh_sys,
            non_qual,
            [0.0, 0.0, 0.0]
        )
        .await
        .is_none());
    }

    let subcap_relay = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C5-RELIC-SUBCAP",
        [200_000.0, 0.0, 0.0],
    )
    .await
    .expect("C5 Relic site spawns relay");
    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C5-RELIC-SUBCAP",
        [200_000.0, 0.0, 0.0],
    )
    .await
    .is_none());

    let (_hyleus_bid, hyleus_tid) =
        on_decloaked_relay_attacked_sync(&ctx, wh_sys, subcap_relay).unwrap();
    assert_eq!(hyleus_tid, 86_498);

    let sys_handle = ctx.systems.get_or_spawn(wh_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 77_001,
            type_id: dread_tid,
            position: DVec3::new(810_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let cap_relay = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C6-COMBAT-CAP",
        [800_000.0, 0.0, 0.0],
    )
    .await
    .expect("C6 Combat site spawns relay");

    // Damage the relay's shield -> step_wormhole_pve_system auto-triggers Arithmos Tyrannos (37_473)
    sys_handle.try_update_combat_state(cap_relay, |cs| {
        cs.shield_hp = (cs.max_shield_hp - 10.0).max(0.0);
    });
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    let arithmos_bid = with_pve_exploration_reg(&ctx, |reg| {
        let info = &reg.active_relays[&cap_relay];
        assert!(info.triggered);
        assert_eq!(info.spawned_drifter_type_id, Some(37_473));
        info.spawned_drifter_ball_id.unwrap()
    });

    // When warp-scrambled (strength 4 > core 3), Drifter does not warp even after 240s
    sys_handle.try_update_combat_state(arithmos_bid, |cs| {
        cs.warp_scramble_strength = 4;
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.active_relays.get_mut(&cap_relay).unwrap().last_warp_unix = 0;
    });
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.active_relays[&cap_relay].warp_hop_count),
        0
    );

    // Clear warp scramble -> next tick warps Arithmos and increments warp_hop_count
    sys_handle.try_update_combat_state(arithmos_bid, |cs| {
        cs.warp_scramble_strength = 0;
    });
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.active_relays[&cap_relay].warp_hop_count),
        1
    );
}

#[tokio::test]
async fn capital_escalation_caps_despawn_on_grid_leave_and_auto_complete_data_site() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    step_wormhole_pve_system(&ctx, 30_000_142, 1.0).await;
    on_asteroid_mined_in_system(&ctx, 30_000_142, 1);

    let dread_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);
    let carrier_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 547)
        .map(|t| t.type_id)
        .unwrap_or(23_757);

    let sys_handle = ctx.systems.get_or_spawn(wh_sys);
    for i in 1..=3_i64 {
        let d_cid = 94_000_100 + i;
        let d_sid = 1_400_100 + i;
        seed_pilot_and_ship(&ctx, d_cid, d_sid, dread_tid, wh_sys);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: d_sid,
                    type_id: dread_tid,
                    position: DVec3::new(1_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await;

        let c_cid = 94_000_200 + i;
        let c_sid = 1_400_200 + i;
        seed_pilot_and_ship(&ctx, c_cid, c_sid, carrier_tid, wh_sys);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: c_sid,
                    type_id: carrier_tid,
                    position: DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await;
    }

    let can_id: i64 = 1_499_001;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacked_containers.insert((wh_sys, can_id));
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "C5-ESC-SITE".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: [0.0, 0.0, 0.0],
                    is_shattered_resource_site: true,
                    delayed_wave_triggered: false,
                    cleared: false,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "C5-DATA-AUTO".into(),
                    solar_system_id: wh_sys,
                    site_category: "Data".into(),
                    wormhole_class: Some(5),
                    position: [500_000.0, 0.0, 0.0],
                    hackable_can_ids: vec![can_id],
                    active_npc_ball_ids: Vec::new(),
                    cleared: false,
                    ..Default::default()
                },
            ],
        );
    });

    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    let (target_ship_id, first_dread_avengers) = with_pve_exploration_reg(&ctx, |reg| {
        let esc_site = &reg.sites_by_system[&wh_sys][0];
        assert_eq!(esc_site.dread_escalation_triggers, 2);
        assert_eq!(esc_site.carrier_escalation_triggers, 2);
        assert_eq!(esc_site.active_escalation_capitals.len(), 4);
        assert!(esc_site.delayed_wave_triggered);
        assert!(esc_site.npc_guards_spawned);

        let data_site = &reg.sites_by_system[&wh_sys][1];
        assert!(data_site.cleared);
        assert!(data_site.relay_spawned);

        let entry = &esc_site.active_escalation_capitals[0];
        (entry.ship_id, entry.avenger_ball_ids.clone())
    });
    assert_eq!(first_dread_avengers.len(), 3);

    let _ = sys_handle
        .set_ball_position(target_ship_id, DVec3::new(250_000.0, 0.0, 0.0))
        .await;
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(
            reg.sites_by_system[&wh_sys][0]
                .active_escalation_capitals
                .len(),
            3
        );
    });
    for bid in first_dread_avengers {
        assert!(!ctx
            .combat_ephemeral
            .read()
            .unwrap()
            .npc_entities
            .contains_key(&bid));
    }
}

#[tokio::test]
async fn downtime_cleanup_removes_anomalies_signatures_cans_and_site_asteroids() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;
    let can_id: i64 = 1_550_001;
    let ast_id: i64 = 1_550_002;
    let belt_ast_id: i64 = 1_550_003;

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(wh_sys).or_default().push(
            CosmicAnomalyRecord {
                anomaly_id: "DT-SITE-1".into(),
                solar_system_id: wh_sys,
                name: "Core Stronghold".into(),
                anomaly_type: "Combat".into(),
                position: [10_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: 100,
                lifetime_seconds: 86_400,
                cleared: true,
            },
        );
        snap.cosmic_signatures.entry(wh_sys).or_default().push(
            CosmicSignatureRecord {
                signature_id: "DT-SITE-1".into(),
                solar_system_id: wh_sys,
                sig_group: "Combat".into(),
                type_name: "Core Stronghold".into(),
                position: [10_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 100,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            },
        );
        snap.items.insert(
            can_id,
            InventoryItemRecord {
                item_id: can_id,
                type_id: 22_270,
                owner_id: 1,
                location_id: wh_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(can_id, [10_000.0, 0.0, 0.0]);
        for (aid, bid) in [(ast_id, 0), (belt_ast_id, 40_000_001)] {
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: wh_sys,
                    belt_id: bid,
                    type_id: 1230,
                    quantity: 1000,
                    initial_quantity: 1000,
                    position: (10_500.0, 0.0, 0.0),
                    radius: 150.0,
                },
            );
        }
    });

    with_pve_exploration_reg(&ctx, |reg| {
        reg.downtime_counter = 10;
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "UNACTIVATED-SITE".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(5),
                    activated_at_downtime: None,
                    dread_escalation_triggers: 2,
                    carrier_escalation_triggers: 2,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DT-SITE-1".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(5),
                    position: [10_000.0, 0.0, 0.0],
                    cleared: true,
                    activated_at_downtime: Some(8),
                    hackable_can_ids: vec![can_id],
                    ..Default::default()
                },
            ],
        );
    });

    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1);

    with_pve_exploration_reg(&ctx, |reg| {
        let surv = &reg.sites_by_system[&wh_sys][0];
        assert_eq!(surv.site_id, "UNACTIVATED-SITE");
        assert_eq!(surv.dread_escalation_triggers, 0);
        assert_eq!(surv.carrier_escalation_triggers, 0);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&can_id));
        assert!(!snap.item_positions.contains_key(&can_id));
        assert!(!snap.asteroids.contains_key(&ast_id));
        assert!(snap.asteroids.contains_key(&belt_ast_id));
    });
}
