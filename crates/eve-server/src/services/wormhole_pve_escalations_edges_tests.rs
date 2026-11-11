//! Deep edge-case and branch coverage tests for `wormhole_pve_escalations`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_type,
};
use crate::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, WarpPhase};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::sync::Arc;

fn seed_ship_in_space_snapshot(
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
async fn spawn_missing_sde_warping_capital_non_cap_and_drifter_core_strength() {
    let (mut ctx, _sess, char_id, _ship_id, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    // 1. Warping capital + off-grid (>100km) capital do NOT set capital_was_on_grid -> Hyleus (86_498)
    let dread_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);
    let warp_cap = 222_000_101_i64;
    let far_cap = 222_000_102_i64;
    seed_ship_in_space_snapshot(&ctx, char_id, warp_cap, dread_tid, wh_sys);
    seed_ship_in_space_snapshot(&ctx, char_id + 1, far_cap, dread_tid, wh_sys);

    let sys_h = ctx.systems.get_or_spawn(wh_sys);
    let warp_mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    for (sid, pos, mode) in [
        (warp_cap, DVec3::new(5_000.0, 0.0, 0.0), warp_mode),
        (far_cap, DVec3::new(150_000.0, 0.0, 0.0), MovementMode::Stop),
    ] {
        sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: dread_tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1.0e9,
                    radius: 2_000.0,
                    speed_fraction: 0.0,
                    mode,
                },
                CombatState {
                    shield_hp: 100_000.0,
                    max_shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    max_armor_hp: 100_000.0,
                    structure_hp: 100_000.0,
                    max_structure_hp: 100_000.0,
                    ..CombatState::default()
                },
                SlimItemMeta {
                    char_id: Some(char_id),
                    owner_id: char_id,
                    group_id: 485,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![PveSiteInstance {
                site_id: "C5-EDGE-1".into(),
                dungeon_id: 3716,
                dungeon_name: "Core Bastion".into(),
                solar_system_id: wh_sys,
                site_category: "WormholeCombat".into(),
                wormhole_class: Some(5),
                position: [0.0, 0.0, 0.0],
                cleared: true,
                ..Default::default()
            }],
        );
    });

    let relay_bid = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C5-EDGE-1",
        [0.0, 0.0, 0.0],
    )
    .await
    .expect("relay spawned");

    // Unknown relay ball id -> None
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_sys, 999_888_777).is_none());

    let (drifter_bid, drifter_tid) =
        on_decloaked_relay_attacked_sync(&ctx, wh_sys, relay_bid).expect("Hyleus expected");
    assert_eq!(drifter_tid, 86_498);

    // Drifter gets warp core strength = 3 in both system actor_state and ballpark CombatState
    assert_eq!(
        sys_h
            .actor_state
            .read()
            .unwrap()
            .base_core_strengths
            .get(&drifter_bid)
            .copied(),
        Some(3)
    );
    let dr_cs = sys_h.get_combat_state(drifter_bid).await.unwrap().unwrap();
    assert_eq!(dr_cs.warp_core_strength, 3);

    // Missing SDE types 37_472 & 41_589 return empty / None
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.types.remove(&37_472);
    sde_mut.types.remove(&41_589);
    assert!(spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        6,
        DVec3::ZERO,
        2,
    )
    .await
    .is_empty());
    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C5-EDGE-1",
        [0.0, 0.0, 0.0],
    )
    .await
    .is_none());
}

#[test]
fn pick_drifter_roam_destination_celestial_standoff_and_ring_fallback() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let custom_sys = 31_998_111_u32;
    let origin = DVec3::new(10_000.0, 0.0, 0.0);

    // 1. Empty celestials -> 6 AU ring fallback
    let fallback_dest = pick_drifter_roam_destination(&ctx.sde, custom_sys, origin, 1);
    let expected_angle = ((1_u32.wrapping_mul(73) % 360) as f64).to_radians();
    let r = 6.0 * crate::services::space_destiny::AU_IN_METERS;
    let expected_fallback = DVec3::new(
        r * expected_angle.cos(),
        1.2 * crate::services::space_destiny::AU_IN_METERS,
        r * expected_angle.sin(),
    );
    assert!((fallback_dest - expected_fallback).length() < 1e-3);

    // 2. Real SDE celestials in Jita (30_000_142): group 6/7/8 uses radius+35_000 standoff; other groups use fixed 35_000 standoff
    let jita_sys = 30_000_142_u32;
    let celestials = ctx.sde.celestials_in_system(jita_sys);
    assert!(!celestials.is_empty());

    let check_hop = |hop_idx: u32, expect_planet_like: bool| {
        let idx = (hop_idx as usize) % celestials.len();
        let c = &celestials[idx];
        let dest = pick_drifter_roam_destination(&ctx.sde, jita_sys, origin, hop_idx);
        let dir = (c.position - origin).normalize_or_zero();
        let standoff = if expect_planet_like {
            c.radius.max(0.0) + 35_000.0
        } else {
            35_000.0
        };
        let expected = c.position - dir * standoff + DVec3::new(0.0, 15_000.0, -20_000.0);
        if expected.distance(origin) >= 1_000_000.0 {
            assert!((dest - expected).length() < 1e-3);
        }
    };

    if let Some((i, _)) = celestials
        .iter()
        .enumerate()
        .find(|(_, c)| matches!(c.group_id, 6 | 7 | 8) && c.position.distance(origin) > 2_000_000.0)
    {
        check_hop(i as u32, true);
    }
    if let Some((i, _)) = celestials
        .iter()
        .enumerate()
        .find(|(_, c)| !matches!(c.group_id, 6 | 7 | 8) && c.position.distance(origin) > 2_000_000.0)
    {
        check_hop(i as u32, false);
    }
}

#[tokio::test]
async fn step_wormhole_pve_system_warping_cap_grid_leave_armor_attack_and_drifter_warp() {
    let (mut ctx, _sess, char_id, ship_id, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    let dread_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);
    // Ensure non-capital ship type exists for non-cap branch
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .entry(587)
        .or_insert_with(|| make_fighter_type(587, 25, 6, 35.0, vec![]));

    let site_origin = [0.0, 0.0, 0.0];
    let cap_id = 223_000_201_i64;
    let warp_ship_id = 223_000_202_i64;
    let frigate_id = ship_id;
    seed_ship_in_space_snapshot(&ctx, char_id + 10, cap_id, dread_tid, wh_sys);
    seed_ship_in_space_snapshot(&ctx, char_id + 11, warp_ship_id, 587, wh_sys);

    let sys_h = ctx.systems.get_or_spawn(wh_sys);
    // Spawn warping ship inside 100km of unactivated site -> must NOT activate site
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: warp_ship_id,
                type_id: 587,
                position: DVec3::new(500_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1_000_000.0, 0.0, 0.0),
                    warp_speed_au_s: 3.0,
                    total_distance: 1_000_000.0,
                    phase: WarpPhase::Cruising,
                    elapsed_warp_time: 1.0,
                },
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(char_id + 11),
                owner_id: char_id + 11,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Non-capital frigate near escalation site -> must NOT escalate
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: frigate_id,
                type_id: 587,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(char_id),
                owner_id: char_id,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Pre-spawn Avengers tracked on active_escalation_capitals with cap_id currently > 100km from warp_in_pos -> despawns Avengers!
    let avengers = spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        5,
        DVec3::ZERO,
        0,
    )
    .await;
    assert_eq!(avengers.len(), 3);

    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: cap_id,
                type_id: dread_tid,
                position: DVec3::new(150_000.0, 0.0, 0.0), // > 100km from warp_in_pos
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1.0e9,
                radius: 2_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(char_id + 10),
                owner_id: char_id + 10,
                group_id: 485,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                // Non-WH site skipped
                PveSiteInstance {
                    site_id: "NON-WH".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: None,
                    position: [500_000.0, 0.0, 0.0],
                    ..Default::default()
                },
                // Main C5 site with active escalation capital leaving grid & armor-damaged relay & missing relay entry
                PveSiteInstance {
                    site_id: "SITE-MAIN".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: site_origin,
                    cleared: true,
                    active_escalation_capitals: vec![ActiveCapitalEscalationEntry {
                        ship_id: cap_id,
                        character_id: char_id + 10,
                        is_dreadnought: true,
                        warp_in_pos: site_origin,
                        avenger_ball_ids: avengers.clone(),
                    }],
                    ..Default::default()
                },
                // Second C5 site hosting the already-warping Drifter relay
                PveSiteInstance {
                    site_id: "SITE-WARP".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: [2_000.0, 0.0, 0.0],
                    cleared: true,
                    ..Default::default()
                },
            ],
        );
    });

    // Relay 1: damaged armor triggers Drifter; Relay 2: missing ball triggers Drifter; Drifter already in Warp skips re-warp
    let relay_armor = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "SITE-MAIN",
        site_origin,
    )
    .await
    .unwrap();
    // Damage armor (leave shield untouched)
    let mut cs_armor = sys_h.get_combat_state(relay_armor).await.unwrap().unwrap();
    cs_armor.armor_hp = (cs_armor.max_armor_hp - 50.0).max(1.0);
    let _ = sys_h.set_combat_state(relay_armor, cs_armor).await;

    // Spawn a triggered relay + Drifter already in MovementMode::Warp -> elapsed_s >= 240.0 should NOT reset spawn_position
    let relay_for_warp = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "SITE-WARP",
        [2_000.0, 0.0, 0.0],
    )
    .await
    .unwrap();
    let (drifter_warp_bid, _) =
        on_decloaked_relay_attacked_sync(&ctx, wh_sys, relay_for_warp).unwrap();
    sys_h
        .set_movement_mode(
            drifter_warp_bid,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        )
        .await
        .unwrap();
    with_mut_combat_ephemeral(&ctx, |rt| {
        if let Some(ent) = rt.npc_entities.get_mut(&drifter_warp_bid) {
            ent.spawn_position = DVec3::new(42.0, 42.0, 42.0);
        }
    });

    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(ri) = reg.active_relays.get_mut(&relay_for_warp) {
            ri.last_warp_sim_s = 0.0;
        }
        reg.active_relays.insert(
            999_111_222,
            DecloakedRelayInfo {
                relay_ball_id: 999_111_222,
                solar_system_id: wh_sys,
                site_id: "SITE-MAIN".into(),
                position: site_origin,
                capital_was_on_grid: false,
                triggered: false,
                spawned_drifter_ball_id: None,
                spawned_drifter_type_id: None,
                last_warp_unix: 0,
                last_warp_sim_s: 0.0,
                warp_hop_count: 0,
            },
        );
    });

    step_wormhole_pve_system(&ctx, wh_sys, 300.0).await;

    // Verify warping ship didn't activate NON-WH, capital >100km despawned avengers, armor-damaged AND missing relays triggered drifter,
    // and already-warping Drifter retained spawn_position (42,42,42).
    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&wh_sys];
        assert!(sites[0].activated_at_downtime.is_none());
        assert!(sites[1].active_escalation_capitals.is_empty());
        assert!(reg.active_relays[&relay_armor].triggered);
        assert!(reg.active_relays[&999_111_222].triggered);
    });
    for avg_id in avengers {
        assert!(sys_h.get_ball_snapshot(avg_id).await.unwrap().is_none());
    }
    with_combat_ephemeral(&ctx, |rt| {
        assert_eq!(
            rt.npc_entities[&drifter_warp_bid].spawn_position,
            DVec3::new(42.0, 42.0, 42.0)
        );
    });
}

#[tokio::test]
async fn on_asteroid_mined_noops_and_downtime_preserves_non_wh_and_distant_asteroids() {
    let (ctx, _sess, char_id, _ship_id, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    // No sites registered -> no-op
    on_asteroid_mined_in_system(&ctx, wh_sys, char_id);

    let avengers = spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        5,
        DVec3::ZERO,
        0,
    )
    .await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "ALREADY-TRIGGERED".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(3),
                    is_shattered_resource_site: true,
                    delayed_wave_triggered: true,
                    cleared: false,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "CLEARED-SHATTERED".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(3),
                    is_shattered_resource_site: true,
                    delayed_wave_triggered: false,
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "NOT-SHATTERED".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(3),
                    is_shattered_resource_site: false,
                    delayed_wave_triggered: false,
                    cleared: false,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "KSPACE-NO-WH".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: None,
                    activated_at_downtime: Some(0),
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "WH-CLEARED-DESPAWN".into(),
                    solar_system_id: wh_sys,
                    wormhole_class: Some(5),
                    activated_at_downtime: Some(0),
                    cleared: true,
                    position: [1_000.0, 0.0, 0.0],
                    active_npc_ball_ids: vec![888_111],
                    active_escalation_capitals: vec![ActiveCapitalEscalationEntry {
                        ship_id: 223_000_999,
                        character_id: char_id,
                        is_dreadnought: true,
                        warp_in_pos: [1_000.0, 0.0, 0.0],
                        avenger_ball_ids: avengers.clone(),
                    }],
                    ..Default::default()
                },
            ],
        );
    });

    // All three shattered/non-shattered candidates are ineligible -> all remain untouched
    on_asteroid_mined_in_system(&ctx, wh_sys, char_id);
    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&wh_sys];
        assert!(!sites[1].delayed_wave_triggered);
        assert!(!sites[2].delayed_wave_triggered);
    });

    // Seed two belt_id == 0 asteroids: one near (<50km) and one far (>50km)
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            901_001,
            AsteroidRecord {
                asteroid_id: 901_001,
                solar_system_id: wh_sys,
                belt_id: 0,
                type_id: 1223,
                quantity: 100,
                initial_quantity: 100,
                position: (5_000.0, 0.0, 0.0),
                radius: 500.0,
            },
        );
        snap.asteroids.insert(
            901_002,
            AsteroidRecord {
                asteroid_id: 901_002,
                solar_system_id: wh_sys,
                belt_id: 0,
                type_id: 1223,
                quantity: 100,
                initial_quantity: 100,
                position: (90_000.0, 0.0, 0.0), // > 50_000m from site
                radius: 500.0,
            },
        );
    });

    // Advance downtime 3 times -> cleared WH site despawns on 3rd; KSPACE-NO-WH untouched; near asteroid removed, far kept
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1);

    with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&wh_sys];
        assert!(sites.iter().any(|s| s.site_id == "KSPACE-NO-WH"));
        assert!(!sites.iter().any(|s| s.site_id == "WH-CLEARED-DESPAWN"));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.asteroids.contains_key(&901_001));
        assert!(snap.asteroids.contains_key(&901_002));
    });
}
