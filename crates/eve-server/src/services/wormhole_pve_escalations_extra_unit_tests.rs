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

#[test]
fn ops_sentry_stats_npc_builder_and_drifter_boss_configuration_kill_mutations() {
    use crate::services::combat_crimewatch::ExtendedNpcEntity;
    use crate::services::wormhole_pve_escalations_ops::{
        build_sde_sleeper_sentry_npc, configure_drifter_response_boss_entity,
        resolve_sleeper_sentry_stats,
    };
    use eve_destiny::{DamageVector, MovementMode};
    use eve_sde::{InvType, SdeRegistry};
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    let base_sde = Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())));
    let wh_sys = 31_000_001_u32;

    // 1. Unknown sentry falls back to canonical constants
    assert_eq!(
        resolve_sleeper_sentry_stats(&base_sde, 999_999_901),
        (250_000.0, 15_000.0, 50.0, 180.0)
    );
    let pos = DVec3::new(1_000.0, 2_000.0, -3_000.0);
    let (fb_ball, fb_cs, fb_npc) =
        build_sde_sleeper_sentry_npc(&base_sde, wh_sys, 71_000_001, 999_999_901, pos, 5);
    assert_eq!(fb_ball.mass, 10_000_000.0);
    assert_eq!(fb_ball.radius, 450.0);
    assert_eq!(fb_ball.max_velocity, 0.0);
    assert!(matches!(fb_ball.mode, MovementMode::Stop));
    assert_eq!(fb_cs.signature_radius, 50.0);
    assert_eq!(fb_npc.faction_id, 500_005);
    assert_eq!(fb_npc.archetype, "SleeperSentry");
    assert_eq!(fb_npc.sleeper_wh_class, 5);
    assert_eq!(fb_npc.optimal_range, 250_000.0);
    assert_eq!(fb_npc.orbit_range, 0.0);
    assert_eq!(fb_npc.damage_vector.em, 90.0);
    assert_eq!(fb_npc.damage_vector.thermal, 90.0);
    assert_eq!(fb_npc.damage_vector.kinetic, 0.0);
    assert_eq!(fb_npc.damage_vector.explosive, 0.0);

    // 2. Known SDE sentry types (30460, 30461, 30462)
    for sentry_tid in [30_460_u32, 30_461, 30_462] {
        let (opt, orb, sig, em_th) = resolve_sleeper_sentry_stats(&base_sde, sentry_tid);
        assert!(opt >= 250_000.0, "sentry {sentry_tid} opt {opt}");
        assert!(orb > 0.0, "sentry {sentry_tid} orb {orb}");
        assert!(sig > 0.0, "sentry {sentry_tid} sig {sig}");
        assert!(em_th > 0.0, "sentry {sentry_tid} em_th {em_th}");
        let (_b, cs, npc) =
            build_sde_sleeper_sentry_npc(&base_sde, wh_sys, 71_000_010 + sentry_tid as i64, sentry_tid, pos, 6);
        assert_eq!(cs.signature_radius, sig);
        assert_eq!(npc.optimal_range, opt);
        assert_eq!(npc.orbit_range, 0.0);
        assert!(orb > 0.0);
        assert!(npc.damage_vector.em + npc.damage_vector.thermal > 0.0);
    }

    // 3. Custom SDE entries testing clamping, non-positive fallbacks, raw vs split EM+Thermal, and Drifter boss configuration
    let mut sde_mut = (*base_sde).clone();
    let mut template = sde_mut.types.values().next().unwrap().clone();
    template.type_id = 99_101;
    template.mass = 0.0;
    template.radius = 0.0;
    template.default_attributes = HashMap::from([
        (eve_sde::attr_ids::MAX_RANGE, 120_000.0),  // < 250_000 -> clamps to 250_000
        (eve_sde::attr_ids::SPEED, 0.0),            // <= 0 -> 15_000
        (eve_sde::attr_ids::SIGNATURE_RADIUS, -5.0), // <= 0 -> 50
        (eve_sde::attr_ids::DAMAGE_MULTIPLIER, 0.0), // <= 0 -> mult 1.0
        (eve_sde::attr_ids::EM_DAMAGE, 0.0),
        (eve_sde::attr_ids::THERMAL_DAMAGE, 0.0),   // em + th <= 0 -> 180 fallback split 90/90
    ]);
    sde_mut.types.insert(99_101, template.clone());

    assert_eq!(
        resolve_sleeper_sentry_stats(&sde_mut, 99_101),
        (250_000.0, 15_000.0, 50.0, 180.0)
    );
    let (b_zero, _cs_zero, npc_zero) =
        build_sde_sleeper_sentry_npc(&sde_mut, wh_sys, 71_000_002, 99_101, pos, 3);
    assert_eq!(b_zero.mass, 0.0);
    assert_eq!(b_zero.radius, 0.0);
    assert_eq!(npc_zero.damage_vector.em, 90.0);
    assert_eq!(npc_zero.damage_vector.thermal, 90.0);

    let mut custom_pos = template.clone();
    custom_pos.type_id = 99_102;
    custom_pos.mass = 4_200_000.0;
    custom_pos.radius = 320.0;
    custom_pos.default_attributes = HashMap::from([
        (eve_sde::attr_ids::MAX_RANGE, 320_000.0),
        (eve_sde::attr_ids::SPEED, 22_000.0),
        (eve_sde::attr_ids::SIGNATURE_RADIUS, 125.0),
        (eve_sde::attr_ids::DAMAGE_MULTIPLIER, 2.5),
        (eve_sde::attr_ids::EM_DAMAGE, 100.0),
        (eve_sde::attr_ids::THERMAL_DAMAGE, 60.0),
    ]);
    sde_mut.types.insert(99_102, custom_pos);
    assert_eq!(
        resolve_sleeper_sentry_stats(&sde_mut, 99_102),
        (320_000.0, 22_000.0, 125.0, 400.0)
    );
    let (b_pos, cs_pos, npc_pos) =
        build_sde_sleeper_sentry_npc(&sde_mut, wh_sys, 71_000_003, 99_102, pos, 4);
    assert_eq!(b_pos.mass, 4_200_000.0);
    assert_eq!(b_pos.radius, 320.0);
    assert_eq!(cs_pos.signature_radius, 125.0);
    assert_eq!(npc_pos.damage_vector.em, 250.0);
    assert_eq!(npc_pos.damage_vector.thermal, 150.0);

    // Drifter boss configuration: unknown type_id still populates all fallback stats & Drifter loot
    let base_entity = |tid: u32| ExtendedNpcEntity {
        ball_id: 71_000_050,
        type_id: tid,
        faction_id: 500_024,
        archetype: "Drifter".into(),
        solar_system_id: wh_sys,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: 0.0,
        orbit_range: 0.0,
        damage_vector: DamageVector::default(),
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: pos,
        sleeper_wh_class: 6,
        custom_cargo_loot: None,
    };

    let mut missing_boss = base_entity(999_999_902);
    configure_drifter_response_boss_entity(&sde_mut, &mut missing_boss);
    assert_eq!(missing_boss.optimal_range, 65_000.0);
    assert_eq!(missing_boss.orbit_range, 28_000.0);
    assert_eq!(missing_boss.tag_drop_type_id, Some(34_575));
    assert_eq!(missing_boss.custom_cargo_loot, Some((30_747, 60)));

    // Custom boss with dmg_mult = 0.5 (clamped to 1.0) & empty attributes hits every fallback constant
    let mut fallback_boss_type: InvType = template;
    fallback_boss_type.type_id = 99_103;
    fallback_boss_type.default_attributes = HashMap::from([(64, 0.5)]);
    sde_mut.types.insert(99_103, fallback_boss_type);

    let mut fb_boss = base_entity(99_103);
    fb_boss.damage_vector.explosive = 55.0; // em + th + kin == 0 still triggers population
    configure_drifter_response_boss_entity(&sde_mut, &mut fb_boss);
    assert_eq!(fb_boss.damage_vector.em, 120.0);
    assert_eq!(fb_boss.damage_vector.thermal, 120.0);
    assert_eq!(fb_boss.damage_vector.kinetic, 120.0);
    assert_eq!(fb_boss.damage_vector.explosive, 120.0);
    assert_eq!(fb_boss.optimal_range, 65_000.0);
    assert_eq!(fb_boss.orbit_range, 28_000.0);
    assert_eq!(fb_boss.web_speed_factor, -60.0);
    assert_eq!(fb_boss.web_range, 30_000.0);
    assert_eq!(fb_boss.scram_strength, 3.0);
    assert_eq!(fb_boss.scram_range, 24_000.0);
    assert_eq!(fb_boss.neut_amount, 600.0);
    assert_eq!(fb_boss.tag_drop_type_id, Some(34_575));
    assert_eq!(fb_boss.custom_cargo_loot, Some((30_747, 60)));

    // Preservation guard: pre-populated positive attributes & negative web_speed_factor are preserved
    let mut preserved = base_entity(99_103);
    preserved.damage_vector.em = 777.0;
    preserved.optimal_range = 99_000.0;
    preserved.orbit_range = 44_000.0;
    preserved.web_speed_factor = -35.0;
    preserved.web_range = 19_000.0;
    preserved.scram_strength = 5.0;
    preserved.scram_range = 21_000.0;
    preserved.neut_amount = 900.0;
    configure_drifter_response_boss_entity(&sde_mut, &mut preserved);
    assert_eq!(preserved.damage_vector.em, 777.0);
    assert_eq!(preserved.damage_vector.thermal, 0.0);
    assert_eq!(preserved.optimal_range, 99_000.0);
    assert_eq!(preserved.orbit_range, 44_000.0);
    assert_eq!(preserved.web_speed_factor, -35.0);
    assert_eq!(preserved.web_range, 19_000.0);
    assert_eq!(preserved.scram_strength, 5.0);
    assert_eq!(preserved.scram_range, 21_000.0);
    assert_eq!(preserved.neut_amount, 900.0);
    assert_eq!(preserved.tag_drop_type_id, Some(34_575));
    assert_eq!(preserved.custom_cargo_loot, Some((30_747, 60)));

    // Canonical Drifter bosses (Arithmos 37473, Hyleus 86498)
    for boss_tid in [37_473_u32, 86_498] {
        let mut boss = base_entity(boss_tid);
        configure_drifter_response_boss_entity(&base_sde, &mut boss);
        assert!(boss.optimal_range > 0.0);
        assert!(boss.orbit_range > 0.0);
        assert!(boss.web_speed_factor < 0.0);
        assert!(boss.web_range > 0.0);
        assert!(boss.scram_strength > 0.0);
        assert!(boss.scram_range > 0.0);
        assert!(boss.neut_amount > 0.0);
        assert_eq!(boss.tag_drop_type_id, Some(34_575));
        assert_eq!(boss.custom_cargo_loot, Some((30_747, 60)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalations_edge_branches_caps_bubble_scram_warping_guard_shattered_and_downtime_boundaries() {
    use crate::router::ServerContext;
    use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
    use crate::services::wormhole_pve_escalations::{
        on_asteroid_mined_in_system, on_decloaked_relay_attacked_sync,
        process_wormhole_sites_on_downtime, spawn_decloaked_transmission_relay_core,
        spawn_upgraded_avengers_core, step_wormhole_pve_system,
    };
    use eve_db::clock::{ServerClock, VirtualServerClock};
    use eve_db::WorldRepository;
    use eve_destiny::{Ball, MovementMode};
    use eve_sde::SdeRegistry;
    use glam::DVec3;
    use std::sync::{Arc, OnceLock};

    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    let sde = Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())));
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    ctx.set_clock(clock.clone());

    let wh_c5 = 31_001_901_u32;
    let wh_c3 = 31_001_101_u32;

    // 1. spawn_upgraded_avengers_core returns empty when type 37472 missing from SDE
    let mut empty_sde = (*sde).clone();
    empty_sde.types.remove(&37_472);
    let av_empty = spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &Arc::new(empty_sde),
        &ctx.combat_ephemeral,
        wh_c5,
        5,
        DVec3::ZERO,
        0,
    )
    .await;
    assert!(av_empty.is_empty());

    // 2. Non-W-space early returns
    step_wormhole_pve_system(&ctx, 30_000_142, 1.0).await;
    on_asteroid_mined_in_system(&ctx, 30_000_142, 90_000_001);
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);

    // 3. Qualifying vs non-qualifying relay spawns & capital in warp ignored -> Hyleus Tyrannos
    let cap_warp_ship = 960_001_001_i64;
    let shat_site_pos = [200_000_000.0, 0.0, 0.0];
    let c3_pos = [300_000_000.0, 0.0, 0.0];
    let ore_pos = [400_000_000.0, 0.0, 0.0];
    let relay_site_pos = [500_000_000.0, 0.0, 0.0];

    let h_c5 = ctx.systems.get_or_spawn(wh_c5);
    let _ = h_c5
        .spawn_ball(Ball {
            id: cap_warp_ship,
            type_id: 19_720, // Revelation (dread) currently warping!
            position: DVec3::from_array(relay_site_pos),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::from_array(relay_site_pos), 1.5),
        })
        .await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_c3,
            vec![
                PveSiteInstance {
                    site_id: "C3-SITE".into(),
                    solar_system_id: wh_c3,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(3),
                    position: c3_pos,
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "SHAT-STEP".into(),
                    solar_system_id: wh_c3,
                    site_category: "Ore".into(),
                    wormhole_class: Some(3),
                    position: shat_site_pos,
                    is_shattered_resource_site: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "SHAT-MINED-CLEARED".into(),
                    solar_system_id: wh_c3,
                    site_category: "Ore".into(),
                    wormhole_class: Some(3),
                    position: [c3_pos[0] + 1_000_000.0, 0.0, 0.0],
                    is_shattered_resource_site: true,
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "NON-WH-SITE".into(),
                    solar_system_id: wh_c3,
                    site_category: "Combat".into(),
                    wormhole_class: None,
                    position: [0.0; 3],
                    activated_at_downtime: Some(0),
                    cleared: true,
                    ..Default::default()
                },
            ],
        );
        reg.sites_by_system.insert(
            wh_c5,
            vec![
                PveSiteInstance {
                    site_id: "C5-ORE".into(),
                    solar_system_id: wh_c5,
                    site_category: "Ore".into(),
                    wormhole_class: Some(5),
                    position: ore_pos,
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "C5-CLEARED-FOR-RELAY".into(),
                    solar_system_id: wh_c5,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: relay_site_pos,
                    cleared: true,
                    ..Default::default()
                },
            ],
        );
    });

    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems, &ctx.db, &ctx.sde, &ctx.combat_ephemeral, wh_c5, "MISSING-SITE", [0.0; 3]
    ).await.is_none());
    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems, &ctx.db, &ctx.sde, &ctx.combat_ephemeral, wh_c3, "C3-SITE", c3_pos
    ).await.is_none());
    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems, &ctx.db, &ctx.sde, &ctx.combat_ephemeral, wh_c5, "C5-ORE", ore_pos
    ).await.is_none());
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_c5, 999_999_999).is_none());

    // Step C5: C5-CLEARED-FOR-RELAY spawns relay with capital_was_on_grid = false because capital was in Warp!
    step_wormhole_pve_system(&ctx, wh_c5, 1.0).await;
    let relay_bid = with_pve_exploration_reg(&ctx, |reg| {
        let (&bid, info) = reg.active_relays.iter().next().expect("relay spawned");
        assert!(!info.capital_was_on_grid);
        assert!(!info.triggered);
        bid
    });
    // Damage armor HP on relay and step -> auto-triggers Hyleus Tyrannos (86498)
    let _ = h_c5.ballpark.write().unwrap().combat_states.get_mut(&relay_bid).map(|cs| {
        cs.armor_hp = (cs.max_armor_hp - 10.0).max(0.0);
    });
    step_wormhole_pve_system(&ctx, wh_c5, 1.0).await;
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_c5, relay_bid).is_none());
    let drifter_bid = with_pve_exploration_reg(&ctx, |reg| {
        let info = &reg.active_relays[&relay_bid];
        assert!(info.triggered);
        assert_eq!(info.spawned_drifter_type_id, Some(86_498));
        info.spawned_drifter_ball_id.unwrap()
    });

    // Bubble-scramble Drifter (no point needed!) suppresses roaming after 240s advance
    let drifter_pos = h_c5.ballpark.read().unwrap().balls[&drifter_bid].position;
    let _ = h_c5.add_warp_bubble(960_002_001, drifter_pos, 15_000.0).await;
    clock.advance_secs(245);
    step_wormhole_pve_system(&ctx, wh_c5, 1.0).await;
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.active_relays[&relay_bid].warp_hop_count, 0);
    });
    // Remove bubble -> next step initiates roam warp; subsequent +245s while still in Warp skips duplicate warp
    let _ = h_c5.remove_warp_bubble(960_002_001).await;
    step_wormhole_pve_system(&ctx, wh_c5, 1.0).await;
    let first_hop_mode = h_c5.ballpark.read().unwrap().balls[&drifter_bid].mode.clone();
    assert!(matches!(first_hop_mode, MovementMode::Warp { .. }));
    clock.advance_secs(245);
    step_wormhole_pve_system(&ctx, wh_c5, 1.0).await;
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.active_relays[&relay_bid].warp_hop_count, 1);
    });

    // 4. Shattered site activation via step_wormhole_pve_system & mining no-ops
    let scout_char = 960_003_001_i64;
    let scout_ship = 960_003_002_i64;
    seed_pilot_and_ship(&ctx, scout_char, scout_ship, 587, wh_c3);
    let _ = ctx
        .systems
        .get_or_spawn(wh_c3)
        .spawn_ball(Ball {
            id: scout_ship,
            type_id: 587,
            position: DVec3::from_array(shat_site_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    step_wormhole_pve_system(&ctx, wh_c3, 1.0).await;
    with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&wh_c3].iter().find(|s| s.site_id == "SHAT-STEP").unwrap();
        assert!(s.delayed_wave_triggered);
        assert!(s.npc_guards_spawned);
        assert!(!s.active_npc_ball_ids.is_empty());
        assert_eq!(s.activated_at_downtime, Some(1));
        // SHAT-MINED-CLEARED stayed untriggered
        let sc = reg.sites_by_system[&wh_c3].iter().find(|s| s.site_id == "SHAT-MINED-CLEARED").unwrap();
        assert!(!sc.delayed_wave_triggered);
    });
    // on_asteroid_mined_in_system does not re-trigger SHAT-STEP or trigger cleared SHAT-MINED-CLEARED
    on_asteroid_mined_in_system(&ctx, wh_c3, scout_char);
    with_pve_exploration_reg(&ctx, |reg| {
        let sc = reg.sites_by_system[&wh_c3].iter().find(|s| s.site_id == "SHAT-MINED-CLEARED").unwrap();
        assert!(!sc.delayed_wave_triggered);
    });

    // 5. Downtime despawn boundary (prior call at dt=1): elapsed_dt 1..2 keep cleared; 3 despawns cleared; 3 keeps unfinished; 4 despawns unfinished; NON-WH-SITE forever kept
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0); // dt=2 (SHAT-STEP elapsed=1, cleared sites stamped act=2)
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0); // dt=3 (SHAT-STEP elapsed=2, cleared elapsed=1)
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(c3) = reg.sites_by_system.get_mut(&wh_c3) {
            c3[0].activated_at_downtime = Some(1); // C3-SITE cleared at dt=1 -> elapsed 3 at dt=4 -> despawns while SHAT-STEP (unfinished, act=1) stays!
        }
    });
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1); // dt=4 -> only C3-SITE despawns!
    // dt=5 -> only SHAT-STEP (unfinished, act=Some(1), elapsed=4) despawns (None-activated cleared sites + NON-WH-SITE stay!)
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1);
    with_pve_exploration_reg(&ctx, |reg| {
        let rem_c3: Vec<&str> = reg.sites_by_system[&wh_c3].iter().map(|s| s.site_id.as_str()).collect();
        assert!(rem_c3.contains(&"NON-WH-SITE"));
        assert!(!rem_c3.contains(&"C3-SITE"));
        assert!(!rem_c3.contains(&"SHAT-STEP"));
    });
}

