//! Additional edge-case unit tests for `combat::npc_ai`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneSpaceState, ExtendedNpcEntity,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::{ConstellationInfo, NpcSpawnGroupInfo, RegionInfo, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn clone_soldier_small_sde_and_region_faction_and_sleeper_interval() {
    let mut small_sde = SdeRegistry::default();
    let ctx = test_ctx();
    let mut bc = ctx.sde.types[&587].clone();
    bc.type_id = 33132;
    bc.group_id = 556;
    bc.name = "Guristas Clone Soldier Trainer".to_string();
    small_sde.types.insert(33132, bc);

    assert_eq!(
        clone_soldier_type_for_security_and_faction(&small_sde, 0.38, 500_010),
        Some(33132)
    );
    // Fallback when no matching type in small SDE
    assert_eq!(
        clone_soldier_type_for_security_and_faction(&small_sde, 0.28, 500_012),
        Some(33142)
    );

    // Region-level faction fallback when constellation has None
    let mut ctx_reg = test_ctx();
    let template_sys = ctx_reg.sde.solar_systems[&30_000_142].clone();
    {
        let sde_mut = Arc::make_mut(&mut ctx_reg.sde);
        sde_mut.constellations.insert(
            29_950_001,
            ConstellationInfo {
                constellation_id: 29_950_001,
                region_id: 19_950_001,
                name: "NoFacConst".into(),
                center: [0.0; 3],
                faction_id: None,
                wormhole_class_id: None,
            },
        );
        sde_mut.regions.insert(
            19_950_001,
            RegionInfo {
                region_id: 19_950_001,
                name: "FacRegion".into(),
                center: [0.0; 3],
                faction_id: Some(500_003), // Amarr -> Blood Raiders 500_012
                wormhole_class_id: None,
            },
        );
        let mut sys = template_sys;
        sys.solar_system_id = 39_950_001;
        sys.constellation_id = 29_950_001;
        sys.region_id = 19_950_001;
        sde_mut.solar_systems.insert(39_950_001, sys);
    }
    assert_eq!(regional_pirate_faction_id(&ctx_reg, 39_950_001), 500_012);

    // compute_sleeper_sentry_interval_s with present & missing type
    assert!((compute_sleeper_sentry_interval_s(&ctx_reg, 999_999_888) - 15.0).abs() < 1e-9);
    {
        let sde_mut = Arc::make_mut(&mut ctx_reg.sde);
        let mut t = sde_mut.types[&587].clone();
        t.type_id = 996_001;
        t.default_attributes.insert(51, 8_000.0);
        sde_mut.types.insert(996_001, t);
    }
    assert!((compute_sleeper_sentry_interval_s(&ctx_reg, 996_001) - 8.0).abs() < 1e-9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_group_and_spawn_belt_rats_for_system_all_security_bands() {
    let mut ctx = test_ctx();
    let mut mgr = NpcSpawnManager::new();

    // 1. Unknown spawn_group_id -> Err
    assert!(mgr
        .spawn_group(&ctx, 30_000_142, 999_888_777, DVec3::ZERO)
        .await
        .is_err());

    // 2. Valid spawn_group_id with known + unknown NPC type
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.npc_spawn_groups.insert(
            990_001,
            NpcSpawnGroupInfo {
                spawn_group_id: 990_001,
                faction_id: 500_010,
                ship_class: 1,
                group_id: 550,
                group_name: "Test Guristas".into(),
                npc_type_ids: vec![597, 999_999_555],
            },
        );
    }
    let spawned = mgr
        .spawn_group(&ctx, 30_000_142, 990_001, DVec3::new(10_000.0, 0.0, 0.0))
        .await
        .unwrap();
    assert_eq!(spawned.len(), 2);
    assert!(mgr.entities.contains_key(&spawned[0]));

    // 3. Wormhole / Drifter Hive system (31_000_001 and 31_000_100) + cooldown skip
    let d1 = mgr.spawn_belt_rats_for_system(&ctx, 31_000_001).await.unwrap();
    assert_eq!(d1.len(), 2);
    let d1_again = mgr.spawn_belt_rats_for_system(&ctx, 31_000_001).await.unwrap();
    assert!(d1_again.is_empty());

    let d2 = mgr.spawn_belt_rats_for_system(&ctx, 31_000_100).await.unwrap();
    assert_eq!(d2.len(), 2);

    // 4. High-sec system with belts (30_000_142 Jita or first highsec with belt)
    let high_belt_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status >= 0.45
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 9)
        })
        .map(|s| s.solar_system_id);
    if let Some(hs) = high_belt_sys {
        let mut hs_mgr = NpcSpawnManager::new();
        let spawned_hs = hs_mgr.spawn_belt_rats_for_system(&ctx, hs).await.unwrap();
        assert!(!spawned_hs.is_empty());
        // Calling again immediately skips existing belts
        let _ = hs_mgr.spawn_belt_rats_for_system(&ctx, hs).await.unwrap();
    }

    // 5. Low-sec system with belt & stargate -> spawns belt rats, stargate rats, Clone Soldier, and checks Mordu/Commander
    let low_belt_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && (0.05..0.45).contains(&s.security_status)
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 9)
        })
        .map(|s| s.solar_system_id);
    if let Some(ls) = low_belt_sys {
        let mut ls_mgr = NpcSpawnManager::new();
        let spawned_ls = ls_mgr.spawn_belt_rats_for_system(&ctx, ls).await.unwrap();
        assert!(!spawned_ls.is_empty());
    }

    // 6. True null-sec system (< 0.0) with belt -> spawns belt rats, gate rats, and exercises Officer spawn block
    let null_belt_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status < 0.0
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 9)
        })
        .map(|s| s.solar_system_id);
    if let Some(ns) = null_belt_sys {
        let mut ns_mgr = NpcSpawnManager::new();
        let spawned_ns = ns_mgr.spawn_belt_rats_for_system(&ctx, ns).await.unwrap();
        assert!(!spawned_ns.is_empty());
    }

    // 7. System with no belts -> empty vec
    let no_belt_sys = 39_999_777u32;
    let empty_res = mgr.spawn_belt_rats_for_system(&ctx, no_belt_sys).await.unwrap();
    assert!(empty_res.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_ai_chase_idle_threat_weights_and_ball_destructions() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char1 = 90_000_001;
    let char2 = 90_000_002;
    let ship_ewar = 100_950_001;
    let ship_logi = 100_950_002;
    let ship_capsule = 100_950_003;
    let drone_ball = 100_950_004;
    let ext_npc_ball = 100_950_005;

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, cid, tid) in [
            (ship_ewar, char1, 587u32),
            (ship_logi, char2, 587u32),
            (ship_capsule, char2, 670u32),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&char1) {
            ch.active_ship_id = ship_ewar;
            ch.solar_system_id = sys_id;
        }
        // Active EWAR module (group 201 / ECM or Warp Scrambler 52) on ship_ewar
        let ewar_mod_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 52)
            .map(|t| t.type_id)
            .unwrap_or(3242);
        snap.items.insert(
            100_950_101,
            InventoryItemRecord {
                item_id: 100_950_101,
                type_id: ewar_mod_tid,
                owner_id: char1,
                location_id: ship_ewar,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(100_950_101);
        // Active Logi module (group 41) on ship_logi
        let logi_mod_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 41)
            .map(|t| t.type_id)
            .unwrap_or(3581);
        snap.items.insert(
            100_950_102,
            InventoryItemRecord {
                item_id: 100_950_102,
                type_id: logi_mod_tid,
                owner_id: char2,
                location_id: ship_logi,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(100_950_102);
    });

    // Spawn player ships (ship_ewar at 20,000 m -> Chase state because > orbit_range*1.4)
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_ewar,
            type_id: 587,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char1, ship_ewar, 587),
    );
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_logi,
            type_id: 587,
            position: DVec3::new(25_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char2, ship_logi, 587),
    );
    // Capsule should be ignored by NPC AI
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_capsule,
            type_id: 670,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );

    let mut mgr = NpcSpawnManager::new();
    let npc_id = 100_950_200;
    let missing_npc_id = 100_950_201;
    let clone_soldier_missing = 100_950_202;

    sys.try_spawn_ball_with_combat(
        Ball {
            id: npc_id,
            type_id: 597,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 320.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(597),
    );
    mgr.entities.insert(
        npc_id,
        NpcCombatEntity {
            ball_id: npc_id,
            type_id: 597,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 42,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 50.0,
            damage_vector: faction_damage_vector(500_010, 50.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    // Two NPCs without balls in ballpark -> pruned to respawn_timers & clone_soldier_respawn_timers
    mgr.entities.insert(
        missing_npc_id,
        NpcCombatEntity {
            ball_id: missing_npc_id,
            type_id: 597,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 43,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 50.0,
            damage_vector: DamageVector::default(),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    mgr.entities.insert(
        clone_soldier_missing,
        NpcCombatEntity {
            ball_id: clone_soldier_missing,
            type_id: 33132,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 44,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 50.0,
            damage_vector: DamageVector::default(),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );

    // 1. step_ai picks ship_ewar (1.5x threat) and enters Chase state; prunes missing NPCs
    mgr.step_ai(&ctx, sys_id, 5, 1.0).await;
    assert_eq!(mgr.entities[&npc_id].state, NpcAiState::Chase);
    assert_eq!(mgr.entities[&npc_id].target_id, Some(ship_ewar));
    assert!(!mgr.entities.contains_key(&missing_npc_id));
    assert!(mgr.respawn_timers.contains_key(&(sys_id, 43)));
    assert!(mgr.clone_soldier_respawn_timers.contains_key(&sys_id));

    // 2. Move ships > 60 km away -> transitions to Idle
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_ewar).unwrap().position = DVec3::new(80_000.0, 0.0, 0.0);
        bp.balls.get_mut(&ship_logi).unwrap().position = DVec3::new(80_000.0, 0.0, 0.0);
    }
    mgr.step_ai_sync(&ctx, sys_id, 6, 1.0);
    assert_eq!(mgr.entities[&npc_id].state, NpcAiState::Idle);
    assert_eq!(mgr.entities[&npc_id].target_id, None);

    // 3. Move ship_ewar into weapon range with lethal NPC damage -> destroys ship_ewar and triggers death pipeline!
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_ewar).unwrap().position = DVec3::new(1_000.0, 0.0, 0.0);
        let scs = bp.combat_states.get_mut(&ship_ewar).unwrap();
        scs.shield_hp = 1.0;
        scs.armor_hp = 1.0;
        scs.structure_hp = 1.0;
    }
    mgr.entities.get_mut(&npc_id).unwrap().damage_vector =
        DamageVector::new(10_000.0, 10_000.0, 10_000.0, 10_000.0);
    mgr.step_ai_sync(&ctx, sys_id, 10, 1.0);

    // 4. Direct test of handle_npc_ai_ball_destructions: empty vec, drone removal, extended NPC removal
    handle_npc_ai_ball_destructions(&ctx, sys_id, Vec::new());

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_ball,
            DroneSpaceState {
                ball_id: drone_ball,
                drone_item_id: 500_991,
                type_id: 2454,
                group_id: 100,
                owner_char_id: char1,
                original_owner_char_id: char1,
                parent_ship_id: ship_ewar,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 800.0,
                optimal_range: 5_000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
        rt.npc_entities.insert(
            ext_npc_ball,
            ExtendedNpcEntity {
                ball_id: ext_npc_ball,
                type_id: 597,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: sys_id,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: std::collections::HashMap::new(),
                threat_table: std::collections::HashMap::new(),
                bounty_isk: 10_000.0,
                optimal_range: 5_000.0,
                orbit_range: 4_000.0,
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
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
                custom_cargo_loot: None,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            500_991,
            InventoryItemRecord {
                item_id: 500_991,
                type_id: 2454,
                owner_id: char1,
                location_id: sys_id as i64,
                ..Default::default()
            },
        );
        snap.items.insert(
            ext_npc_ball,
            InventoryItemRecord {
                item_id: ext_npc_ball,
                type_id: 597,
                owner_id: 1,
                location_id: sys_id as i64,
                ..Default::default()
            },
        );
    });

    handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![
            (
                npc_id,
                drone_ball,
                Ball {
                    id: drone_ball,
                    type_id: 2454,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 800.0,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 5.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            ),
            (
                npc_id,
                ext_npc_ball,
                Ball {
                    id: ext_npc_ball,
                    type_id: 597,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 320.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            ),
        ],
    );
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&drone_ball));
        assert!(!rt.npc_entities.contains_key(&ext_npc_ball));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&500_991));
        assert!(!snap.items.contains_key(&ext_npc_ball));
    });

    // 5. handle_npc_destroyed error on unknown ball_id
    assert!(mgr
        .handle_npc_destroyed(&ctx, sys_id, 999_888_111, char1, 10_000)
        .is_err());
}
