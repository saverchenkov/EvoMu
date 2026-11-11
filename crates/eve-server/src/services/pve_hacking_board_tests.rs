//! Unit and integration tests for `pve_hacking_board`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_exploration::PveSiteInstance;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn build_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

fn find_analyzer_types(sde: &SdeRegistry) -> (u32, u32) {
    let mut data_id = None;
    let mut relic_id = None;
    for t in sde.types.values() {
        if t.published
            && t.group_id == 538
            && !t.name.contains("Blueprint")
            && t.default_attributes.get(&1909).copied().unwrap_or(0.0) > 0.0
        {
            if t.name.contains("Relic") || t.default_attributes.get(&183) == Some(&13278.0) {
                relic_id.get_or_insert(t.type_id);
            } else if !t.name.contains("Integrated") && !t.name.contains("QA") {
                data_id.get_or_insert(t.type_id);
            }
        }
    }
    (data_id.unwrap_or(3082), relic_id.unwrap_or(22177))
}

#[test]
fn test_resolve_fitted_analyzer_stats_skills_hulls_and_storms() {
    let ctx = build_ctx();
    let char_id = 900101_i64;
    let ship_id = 910101_i64;
    let sys_id: SolarSystemId = 30_000_142;

    // Without ship -> AnalyzerModuleRequired
    let mut session = SessionState::default();
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    let err = resolve_fitted_analyzer_virus_stats(&ctx, &session, char_id, false).unwrap_err();
    assert!(err.message.contains("AnalyzerModuleRequired"));

    // Ship without analyzer -> AnalyzerModuleRequired
    let hull_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.default_attributes.contains_key(&1915))
        .map(|t| t.type_id)
        .unwrap_or(605);
    let (data_tid, relic_tid) = find_analyzer_types(&ctx.sde);
    let rig_relic_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.name.contains("Emission Scope Sharpener I"))
        .map(|t| t.type_id)
        .unwrap_or(31117);
    let rig_data_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.name.contains("Memetic Algorithm Bank I"))
        .map(|t| t.type_id)
        .unwrap_or(31125);

    ctx.db.provision_gm_account("scanner_user", "pw", char_id, "Scanner");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: hull_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert!(resolve_fitted_analyzer_virus_stats(&ctx, &session, char_id, false).is_err());

    // Fit Data & Relic analyzers + rigs + skills + strong electrical storm beacon
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id + 1,
            InventoryItemRecord {
                item_id: ship_id + 1,
                type_id: data_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 2,
            InventoryItemRecord {
                item_id: ship_id + 2,
                type_id: relic_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 3,
            InventoryItemRecord {
                item_id: ship_id + 3,
                type_id: rig_relic_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 4,
            InventoryItemRecord {
                item_id: ship_id + 4,
                type_id: rig_data_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 5,
            InventoryItemRecord {
                item_id: ship_id + 5,
                type_id: 47382, // Strong Electrical Storm beacon (+25 coherence)
                owner_id: 1,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                ..Default::default()
            },
        );
        let mut skills = HashMap::new();
        skills.insert(
            21718,
            TrainedSkillRecord {
                type_id: 21718,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            13278,
            TrainedSkillRecord {
                type_id: 13278,
                level: 4,
                skill_points: 181_019.0,
            },
        );
        snap.skills.insert(char_id, skills);
    });

    let (coh_data, str_data, slots_data, range_data) =
        resolve_fitted_analyzer_virus_stats(&ctx, &session, char_id, false).unwrap();
    let (coh_relic, str_relic, slots_relic, range_relic) =
        resolve_fitted_analyzer_virus_stats(&ctx, &session, char_id, true).unwrap();
    assert!(coh_data >= 85);
    assert!(coh_relic >= 75);
    assert!(str_data >= 10 && str_relic >= 10);
    assert!(slots_data >= 2 && slots_relic >= 2);
    assert!(range_data >= 1_000.0 && range_relic >= 1_000.0);
}

#[test]
fn test_grid_generation_node_stats_and_distance_indicator() {
    let sde = shared_sde();
    let (d0, s0) = resolve_hacking_node_stats(None, None, false, 0);
    assert_eq!(d0, 2);
    assert_eq!(s0.core_coh, 40);
    let (d4, s4_relic) = resolve_hacking_node_stats(Some(&sde), None, true, 4);
    assert_eq!(d4, 4);
    assert_eq!(s4_relic.core_coh, 70);

    let tiles_easy = build_hacking_grid_procedural(5001, false, 1, 42);
    assert_eq!(tiles_easy.len(), 24);
    assert!(tiles_easy[0].revealed);
    assert_eq!(tiles_easy[0].kind, HackingNodeKind::Empty);
    assert_eq!(
        tiles_easy
            .iter()
            .filter(|t| t.kind == HackingNodeKind::SystemCore)
            .count(),
        1
    );
    // Diff 1 never spawns Antivirus directly
    assert_eq!(
        tiles_easy
            .iter()
            .filter(|t| t.kind == HackingNodeKind::Antivirus)
            .count(),
        0
    );

    let mut tiles_hard = build_hacking_grid_procedural_with_sde(Some(&sde), 5002, None, true, 5, 99);
    assert_eq!(tiles_hard.len(), 24);
    assert!(tiles_hard
        .iter()
        .any(|t| t.kind == HackingNodeKind::Antivirus));
    assert!(tiles_hard
        .iter()
        .any(|t| t.kind == HackingNodeKind::RestorationNode));
    assert!(tiles_hard
        .iter()
        .any(|t| t.kind == HackingNodeKind::VirusSuppressor));

    let dist = hacking_tile_distance_indicator(0, &tiles_hard);
    assert!((1..=5).contains(&dist));

    // When all caches/utilities/core are already revealed, fallback distance to core still returns 1..=5
    for t in &mut tiles_hard {
        t.revealed = true;
    }
    let fallback_dist = hacking_tile_distance_indicator(0, &tiles_hard);
    assert!((1..=5).contains(&fallback_dist));
}

#[test]
fn test_hacking_turn_combat_utilities_suppressor_restorer_and_core_win() {
    let mut tiles = build_hacking_grid_procedural(777, false, 4, 123);
    let orig_core = tiles
        .iter()
        .position(|t| t.kind == HackingNodeKind::SystemCore)
        .unwrap();
    let free: Vec<u32> = (1..24_u32).filter(|&i| (i as usize) != orig_core).collect();
    let sup_idx = free[0];
    let dc_util_idx = free[1];
    let dc_odd_idx = *free.iter().find(|&&i| i != sup_idx && i != dc_util_idx && i % 2 == 1).unwrap();
    let dc_even_idx = *free
        .iter()
        .find(|&&i| i != sup_idx && i != dc_util_idx && i != dc_odd_idx && i % 2 == 0)
        .unwrap();
    let util_sub_idx = *free
        .iter()
        .find(|&&i| i != sup_idx && i != dc_util_idx && i != dc_odd_idx && i != dc_even_idx)
        .unwrap();
    let rest_idx = *free
        .iter()
        .find(|&&i| {
            i != sup_idx
                && i != dc_util_idx
                && i != dc_odd_idx
                && i != dc_even_idx
                && i != util_sub_idx
        })
        .unwrap();

    tiles[sup_idx as usize] = HackingTileState {
        tile_index: sup_idx,
        revealed: true,
        blocked_by_defensive: true,
        kind: HackingNodeKind::VirusSuppressor,
        coherence: 30,
        strength: 10,
        utility_drop: None,
    };
    tiles[dc_util_idx as usize] = HackingTileState {
        tile_index: dc_util_idx,
        revealed: false,
        blocked_by_defensive: false,
        kind: HackingNodeKind::DataCache,
        coherence: 0,
        strength: 0,
        utility_drop: Some(HackingUtilityKind::SelfRepair),
    };
    tiles[dc_odd_idx as usize] = HackingTileState {
        tile_index: dc_odd_idx,
        revealed: false,
        blocked_by_defensive: false,
        kind: HackingNodeKind::DataCache,
        coherence: 0,
        strength: 0,
        utility_drop: None, // odd index -> concealed Antivirus
    };
    tiles[dc_even_idx as usize] = HackingTileState {
        tile_index: dc_even_idx,
        revealed: false,
        blocked_by_defensive: false,
        kind: HackingNodeKind::DataCache,
        coherence: 0,
        strength: 0,
        utility_drop: None, // even index -> concealed Firewall
    };
    tiles[util_sub_idx as usize] = HackingTileState {
        tile_index: util_sub_idx,
        revealed: false,
        blocked_by_defensive: false,
        kind: HackingNodeKind::UtilitySubsystem,
        coherence: 0,
        strength: 0,
        utility_drop: Some(HackingUtilityKind::KernelRot),
    };
    tiles[rest_idx as usize] = HackingTileState {
        tile_index: rest_idx,
        revealed: true,
        blocked_by_defensive: true,
        kind: HackingNodeKind::RestorationNode,
        coherence: 20,
        strength: 10,
        utility_drop: None,
    };
    let core_idx = tiles
        .iter()
        .find(|t| t.kind == HackingNodeKind::SystemCore)
        .map(|t| t.tile_index)
        .unwrap();

    let mut game = HackingGameSession {
        game_id: 1,
        character_id: 1001,
        solar_system_id: 30_000_142,
        container_id: 777,
        site_id: "SITE_1".into(),
        is_relic: false,
        is_ghost_site: false,
        is_sleeper_cache_puzzle: false,
        virus_coherence: 100,
        max_virus_coherence: 100,
        base_virus_strength: 35,
        polymorphic_shield_charges: 1,
        secondary_vector_target: Some((sup_idx, 2)),
        utilities: vec![None, None, None],
        tiles,
        status: "active".into(),
    };

    // Turn 1: DataCache with utility drop; SecondaryVector ticks sup_idx (-20 -> 10)
    let (k2, _, eff, done) = resolve_hacking_turn_combat(&mut game, dc_util_idx);
    assert_eq!(k2, "DataCache");
    assert_eq!(eff, 20); // 35 - 15 suppressor
    assert!(!done);
    assert_eq!(game.utilities[0], Some(HackingUtilityKind::SelfRepair));

    // Turn 2: DataCache concealed Antivirus (odd index); SecondaryVector ticks sup_idx (-20 -> 0, expires)
    let (k3, _, _, _) = resolve_hacking_turn_combat(&mut game, dc_odd_idx);
    assert_eq!(k3, "Antivirus");
    assert!(game.secondary_vector_target.is_none());

    // Turn 3: DataCache concealed Firewall (even index)
    let (k4, _, _, _) = resolve_hacking_turn_combat(&mut game, dc_even_idx);
    assert_eq!(k4, "Firewall");

    // Turn 4: UtilitySubsystem -> collects KernelRot
    let (k5, _, _, _) = resolve_hacking_turn_combat(&mut game, util_sub_idx);
    assert_eq!(k5, "UtilitySubsystem");
    assert_eq!(game.utilities[1], Some(HackingUtilityKind::KernelRot));

    // Turn 5: Attack concealed Antivirus with PolymorphicShield active -> no virus coh loss
    let coh_before_shield = game.virus_coherence;
    let (_, _, _, _) = resolve_hacking_turn_combat(&mut game, dc_odd_idx);
    assert_eq!(game.virus_coherence, coh_before_shield);
    assert_eq!(game.polymorphic_shield_charges, 0);

    // Turn 6: Attack concealed Firewall without shield -> takes retaliation, RestorationNode heals a defensive node
    resolve_hacking_turn_combat(&mut game, dc_even_idx);
    assert!(game.virus_coherence < coh_before_shield);

    // Turn 7: Strike SystemCore until destroyed
    game.tiles[core_idx as usize].coherence = 15;
    let (kc, rem, _, won) = resolve_hacking_turn_combat(&mut game, core_idx);
    assert_eq!(kc, "SystemCore");
    assert_eq!(rem, 0);
    assert!(won);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hack_failure_explosion_and_ghost_site_detonation() {
    let ctx = build_ctx();
    let sys_id: SolarSystemId = 30_000_142; // Jita (highsec)
    let container_id = 88001_i64;
    let loot_id = 88002_i64;
    let ship_id = 88003_i64;

    // 1. Structure container skips physical explosion
    let (exploded, dmg, npcs, destroyed) =
        handle_second_hack_failure_explosion(&ctx, sys_id, "STRUCTURE_1", container_id, false).await;
    assert!(!exploded);
    assert_eq!(dmg, 0.0);
    assert!(npcs.is_empty());
    assert_eq!(destroyed, 0);

    // 2. Space container + child loot + ball removal
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 22266,
                location_id: sys_id as i64,
                ..Default::default()
            },
        );
        snap.items.insert(
            loot_id,
            InventoryItemRecord {
                item_id: loot_id,
                type_id: 34,
                location_id: container_id,
                quantity: 10,
                ..Default::default()
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: container_id,
            type_id: 22266,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let (exploded2, _, _, destroyed2) =
        handle_second_hack_failure_explosion(&ctx, sys_id, "DATA_SITE_1", container_id, false).await;
    assert!(exploded2);
    assert_eq!(destroyed2, 2);

    // 3. Ghost site detonation (highsec = 6000.0 + nearby ship damaged + pirate ambush spawned)
    let mut session = SessionState::default();
    let ghost_can = 88010_i64;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ghost_can,
            type_id: 33001,
            position: glam::DVec3::new(1000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587, // Rifter (category_id == 6) within 10km of ghost_can
            position: glam::DVec3::new(2500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "GHOST_1".into(),
                dungeon_id: 5534,
                solar_system_id: sys_id,
                position: [1000.0, 0.0, 0.0],
                hackable_can_ids: vec![ghost_can],
                ..Default::default()
            }],
        );
    });

    let (aoe_dmg, ambush) = detonate_ghost_site(&ctx, &mut session, sys_id, "GHOST_1").await;
    assert_eq!(aoe_dmg, 6_000.0);
    assert!(!ambush.is_empty());

    // Second detonation does not respawn ambush waves
    let (aoe_dmg2, ambush2) =
        detonate_ghost_site_with_can(&ctx, &mut session, sys_id, "GHOST_1", Some(99999)).await;
    assert_eq!(aoe_dmg2, 6_000.0);
    assert!(ambush2.is_empty());

    // Wormhole Ghost site -> 12,000 explosive damage
    let wh_sys: SolarSystemId = 31_000_005;
    let (wh_dmg, _) =
        detonate_ghost_site_with_can(&ctx, &mut session, wh_sys, "MISSING_WH_GHOST", Some(777)).await;
    assert_eq!(wh_dmg, 12_000.0);
}

#[cfg(test)]
#[path = "pve_hacking_board_extra_unit_tests.rs"]
mod pve_hacking_board_extra_unit_tests;
