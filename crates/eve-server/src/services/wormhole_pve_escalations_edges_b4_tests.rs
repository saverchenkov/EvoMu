//! Edge-case, early-return, despawn-reason, and roaming fallback tests for `wormhole_pve_escalations`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::Arc;

fn seed_ship(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_sde_types_and_empty_registry_early_returns() {
    let empty_sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&empty_sde), Arc::clone(&db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let wh_sys: SolarSystemId = 31_000_005;

    // spawn_upgraded_avengers_core returns empty when type 37_472 missing from SDE
    assert!(spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        5,
        DVec3::ZERO,
        0,
    )
    .await
    .is_empty());

    // Empty registry downtime maintenance returns 0
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);

    // on_asteroid_mined_in_system on system with no sites / non-qualifying sites
    on_asteroid_mined_in_system(&ctx, wh_sys, 1);

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![PveSiteInstance {
                site_id: "C6-NO-SDE".into(),
                solar_system_id: wh_sys,
                site_category: "WormholeCombat".into(),
                wormhole_class: Some(6),
                position: [0.0, 0.0, 0.0],
                cleared: true,
                ..Default::default()
            }],
        );
    });
    // Missing relay type 41_589 in empty SDE -> returns None
    assert!(spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        "C6-NO-SDE",
        [0.0, 0.0, 0.0],
    )
    .await
    .is_none());

    // Unknown relay ID in on_decloaked_relay_attacked_sync -> None
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_sys, 999_888).is_none());

    // Active untriggered relay with missing Drifter type in SDE -> None
    with_pve_exploration_reg(&ctx, |reg| {
        reg.active_relays.insert(
            555_001,
            DecloakedRelayInfo {
                relay_ball_id: 555_001,
                solar_system_id: wh_sys,
                site_id: "C6-NO-SDE".into(),
                position: [0.0, 0.0, 0.0],
                capital_was_on_grid: false,
                triggered: false,
                ..Default::default()
            },
        );
    });
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_sys, 555_001).is_none());

    // on_asteroid_mined_in_system when only non-shattered site exists -> no-op
    on_asteroid_mined_in_system(&ctx, wh_sys, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_wormhole_cleared_relay_spawn_destroyed_relay_trigger_and_despawn_cases() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    let dread_tid: u32 = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);

    let ship_warp_in = 1_600_001_i64;
    let ship_leave_sys = 1_600_002_i64;
    let ship_move_warpin = 1_600_003_i64;
    seed_ship(&ctx, 95_000_001, ship_warp_in, dread_tid, wh_sys);
    seed_ship(&ctx, 95_000_002, ship_leave_sys, dread_tid, wh_sys);
    seed_ship(&ctx, 95_000_003, ship_move_warpin, dread_tid, wh_sys);

    let sys_h = ctx.systems.get_or_spawn(wh_sys);
    // ship_warp_in is within 100km of site but in MovementMode::Warp -> ignored on escalation & despawned
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_warp_in,
                type_id: dread_tid,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
            },
            CombatState::default(),
        )
        .await;
    // ship_move_warpin starts at [-80_000, 0, 0] (within 100km of site [0,0,0])
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_move_warpin,
                type_id: dread_tid,
                position: DVec3::new(-80_000.0, 0.0, 0.0),
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
    // ship_leave_sys is at [1_000, 0, 0]
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_leave_sys,
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

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                // 1. Non-wormhole site in system (skipped by step & retained on downtime)
                PveSiteInstance {
                    site_id: "NON-WH".into(),
                    solar_system_id: wh_sys,
                    site_category: "Combat".into(),
                    wormhole_class: None,
                    position: [0.0, 0.0, 0.0],
                    ..Default::default()
                },
                // 2. Pre-cleared C6 combat site with relay_spawned == false -> spawns relay on step
                PveSiteInstance {
                    site_id: "C6-CLEARED".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(6),
                    position: [900_000.0, 0.0, 0.0],
                    cleared: true,
                    relay_spawned: false,
                    ..Default::default()
                },
                // 3. Empty C5 site (no ships on grid) -> hits on_grid_ships.is_empty()
                PveSiteInstance {
                    site_id: "C5-EMPTY".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: [2_000_000.0, 0.0, 0.0],
                    cleared: false,
                    ..Default::default()
                },
                // 4. Active C5 escalation site
                PveSiteInstance {
                    site_id: "C5-ACTIVE".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: [0.0, 0.0, 0.0],
                    cleared: false,
                    ..Default::default()
                },
            ],
        );
        // Untriggered relay whose ball is missing from ball_map -> triggers on step (None => !contains_key)
        reg.active_relays.insert(
            888_001,
            DecloakedRelayInfo {
                relay_ball_id: 888_001,
                solar_system_id: wh_sys,
                site_id: "C6-CLEARED".into(),
                position: [900_000.0, 0.0, 0.0],
                capital_was_on_grid: false,
                triggered: false,
                ..Default::default()
            },
        );
        // Relay in another system + triggered relay with no spawned_drifter_ball_id + missing drifter ball
        reg.active_relays.insert(
            888_002,
            DecloakedRelayInfo {
                relay_ball_id: 888_002,
                solar_system_id: 31_000_006,
                site_id: "OTHER".into(),
                triggered: true,
                ..Default::default()
            },
        );
        reg.active_relays.insert(
            888_003,
            DecloakedRelayInfo {
                relay_ball_id: 888_003,
                solar_system_id: wh_sys,
                site_id: "NONE".into(),
                triggered: true,
                spawned_drifter_ball_id: None,
                ..Default::default()
            },
        );
        reg.active_relays.insert(
            888_004,
            DecloakedRelayInfo {
                relay_ball_id: 888_004,
                solar_system_id: wh_sys,
                site_id: "NONE".into(),
                triggered: true,
                spawned_drifter_ball_id: Some(999_777_666),
                ..Default::default()
            },
        );
    });

    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    let (cleared_relay_spawned, active_cap_count, drifter_888001) = with_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&wh_sys];
        (
            sites[1].relay_spawned,
            sites[3].active_escalation_capitals.len(),
            reg.active_relays[&888_001].spawned_drifter_ball_id,
        )
    });
    assert!(cleared_relay_spawned);
    assert_eq!(active_cap_count, 2); // ship_warp_in was warping so excluded; only 2 triggered
    assert!(drifter_888001.is_some());

    // Now move ship_move_warpin to [50_000, 0, 0] (still within 100km of site [0,0,0],
    // but 130km > 100km from its warp_in_pos [-80_000, 0, 0]), and remove ship_leave_sys from system.
    let _ = sys_h
        .set_ball_position(ship_move_warpin, DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&95_000_002).unwrap().solar_system_id = 30_000_142;
    });
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.sites_by_system[&wh_sys][3]
            .active_escalation_capitals
            .is_empty());
    });

    // Test downtime despawn with remaining active_escalation_capitals + active_npc_ball_ids
    with_pve_exploration_reg(&ctx, |reg| {
        let s = &mut reg.sites_by_system.get_mut(&wh_sys).unwrap()[3];
        s.active_npc_ball_ids.push(777_101);
        s.active_escalation_capitals.push(ActiveCapitalEscalationEntry {
            ship_id: 1,
            character_id: 1,
            is_dreadnought: true,
            warp_in_pos: [0.0; 3],
            avenger_ball_ids: vec![777_102],
        });
    });
    // C5-ACTIVE was activated at dt=0 and cleared=false -> despawns at dt=4
    for _ in 0..3 {
        let _ = process_wormhole_sites_on_downtime(&ctx);
    }
    let dt4 = process_wormhole_sites_on_downtime(&ctx);
    assert_eq!(dt4, 1);
    // NON-WH, C6-CLEARED (activated_at_downtime == None), and C5-EMPTY (unactivated) remain
    with_pve_exploration_reg(&ctx, |reg| {
        let ids: Vec<&str> = reg.sites_by_system[&wh_sys]
            .iter()
            .map(|s| s.site_id.as_str())
            .collect();
        assert_eq!(ids, vec!["NON-WH", "C6-CLEARED", "C5-EMPTY"]);
    });
}

#[test]
fn pick_drifter_roam_destination_covers_celestial_standoff_and_empty_fallback() {
    let empty_sde = SdeRegistry::default();
    let wh_sys: SolarSystemId = 31_000_005;

    // Empty celestials (no db_path) -> trigonometric AU fallback
    let fallback = pick_drifter_roam_destination(&empty_sde, wh_sys, DVec3::ZERO, 2);
    assert!(fallback.length() > 1.0e11);

    // Real SDE celestials in Jita (30_000_142) include planets/moons/stargates (group_id 6/7/8 and others)
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let celestials = ctx.sde.celestials_in_system(30_000_142);
    assert!(!celestials.is_empty());
    let chosen_planet = pick_drifter_roam_destination(&ctx.sde, 30_000_142, DVec3::ZERO, 0);
    assert!(chosen_planet.distance(DVec3::ZERO) >= 1_000_000.0);
    if let Some(non_planet_idx) = celestials.iter().position(|c| !matches!(c.group_id, 6 | 7 | 8)) {
        let chosen_other = pick_drifter_roam_destination(&ctx.sde, 30_000_142, DVec3::ZERO, non_planet_idx as u32);
        assert!(chosen_other.distance(DVec3::ZERO) >= 1_000_000.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_wormhole_pve_system_direct_spawns_relay_on_cleared_site() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_099;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![PveSiteInstance {
                site_id: "C6-DIRECT-RELAY".into(),
                solar_system_id: wh_sys,
                site_category: "WormholeCombat".into(),
                wormhole_class: Some(6),
                position: [1_500_000.0, 0.0, 0.0],
                cleared: true,
                relay_spawned: false,
                ..Default::default()
            }],
        );
    });

    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.sites_by_system[&wh_sys][0].relay_spawned);
        assert!(reg
            .active_relays
            .values()
            .any(|r| r.solar_system_id == wh_sys && r.site_id == "C6-DIRECT-RELAY"));
    });
}
