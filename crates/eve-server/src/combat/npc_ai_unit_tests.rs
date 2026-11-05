//! Unit and mutation-killing tests for `combat::npc_ai`.

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{ConstellationInfo, RegionInfo, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn faction_damage_vectors_and_sec_status_gain_exact_formulas() {
    for &fid in &[500_010u32, 500_001] {
        let d = faction_damage_vector(fid, 100.0);
        assert_eq!(d.em, 0.0);
        assert!((d.thermal - 30.0).abs() < 1e-12);
        assert!((d.kinetic - 70.0).abs() < 1e-12);
        assert_eq!(d.explosive, 0.0);
    }
    for &fid in &[500_012u32, 500_019, 500_003] {
        let d = faction_damage_vector(fid, 100.0);
        assert!((d.em - 60.0).abs() < 1e-12);
        assert!((d.thermal - 40.0).abs() < 1e-12);
        assert_eq!(d.kinetic, 0.0);
        assert_eq!(d.explosive, 0.0);
    }
    for &fid in &[500_020u32, 500_004] {
        let d = faction_damage_vector(fid, 100.0);
        assert_eq!(d.em, 0.0);
        assert!((d.thermal - 60.0).abs() < 1e-12);
        assert!((d.kinetic - 40.0).abs() < 1e-12);
        assert_eq!(d.explosive, 0.0);
    }
    for &fid in &[500_011u32, 500_002] {
        let d = faction_damage_vector(fid, 100.0);
        assert_eq!(d.em, 0.0);
        assert!((d.thermal - 10.0).abs() < 1e-12);
        assert!((d.kinetic - 30.0).abs() < 1e-12);
        assert!((d.explosive - 60.0).abs() < 1e-12);
    }
    let def = faction_damage_vector(0, 100.0);
    assert!((def.em - 25.0).abs() < 1e-12);
    assert!((def.thermal - 25.0).abs() < 1e-12);
    assert!((def.kinetic - 25.0).abs() < 1e-12);
    assert!((def.explosive - 25.0).abs() < 1e-12);

    // compute_sec_status_gain boundaries and clamping
    assert_eq!(compute_sec_status_gain(5.0, 500_000.0), 0.0);
    assert_eq!(compute_sec_status_gain(6.0, 500_000.0), 0.0);
    assert_eq!(compute_sec_status_gain(0.0, 0.0), 0.0);
    assert_eq!(compute_sec_status_gain(0.0, -10.0), 0.0);
    assert!((compute_sec_status_gain(0.0, 500_000.0) - 0.05).abs() < 1e-12);
    assert!((compute_sec_status_gain(0.0, 1_000_000.0) - 0.10).abs() < 1e-12);
    assert!((compute_sec_status_gain(4.9, 100.0) - 0.001).abs() < 1e-12);
    assert!((compute_sec_status_gain(4.9995, 100.0) - 0.0005).abs() < 1e-12);
    assert!((compute_sec_status_gain(-10.0, 10_000_000.0) - 0.5).abs() < 1e-12);
}

#[test]
fn clone_soldier_and_regional_pirate_faction_all_branches() {
    assert_eq!(clone_soldier_tier_for_security(0.049), None);
    assert_eq!(clone_soldier_tier_for_security(0.05), Some("Negotiator"));
    assert_eq!(clone_soldier_tier_for_security(0.149), Some("Negotiator"));
    assert_eq!(clone_soldier_tier_for_security(0.15), Some("Transporter"));
    assert_eq!(clone_soldier_tier_for_security(0.249), Some("Transporter"));
    assert_eq!(clone_soldier_tier_for_security(0.25), Some("Recruiter"));
    assert_eq!(clone_soldier_tier_for_security(0.349), Some("Recruiter"));
    assert_eq!(clone_soldier_tier_for_security(0.35), Some("Trainer"));
    assert_eq!(clone_soldier_tier_for_security(0.449), Some("Trainer"));
    assert_eq!(clone_soldier_tier_for_security(0.45), None);

    assert_eq!(clone_soldier_type_for_security(0.45), None);
    assert_eq!(clone_soldier_type_for_security(0.35), Some(33132));
    assert_eq!(clone_soldier_type_for_security(0.25), Some(33142));
    assert_eq!(clone_soldier_type_for_security(0.15), Some(33163));
    assert_eq!(clone_soldier_type_for_security(0.05), Some(33175));

    let mut ctx = test_ctx();
    assert_eq!(
        clone_soldier_type_for_security_and_faction(&ctx.sde, 0.5, 500_010),
        None
    );
    for fid in [500_011, 500_012, 500_010, 500_019, 500_020, 999_999] {
        assert!(clone_soldier_type_for_security_and_faction(&ctx.sde, 0.25, fid).is_some());
    }

    // Unknown system -> 500_010
    assert_eq!(regional_pirate_faction_id(&ctx, 999_999_999), 500_010);

    // Constellation faction_id mapping & region_id fallback
    let template_sys = ctx.sde.solar_systems[&30_000_142].clone();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        for (idx, (cfid, expected)) in [
            (Some(500_010u32), 500_010u32),
            (Some(500_011), 500_011),
            (Some(500_015), 500_011),
            (Some(500_012), 500_012),
            (Some(500_008), 500_012),
            (Some(500_019), 500_019),
            (Some(500_007), 500_019),
            (Some(500_020), 500_020),
            (Some(500_009), 500_020),
            (Some(500_014), 500_020),
        ]
        .into_iter()
        .enumerate()
        {
            let sid = 39_900_000 + idx as u32;
            let cid = 29_900_000 + idx as u32;
            sde_mut.constellations.insert(
                cid,
                ConstellationInfo {
                    constellation_id: cid,
                    region_id: 10_999_999,
                    name: format!("C{idx}"),
                    center: [0.0, 0.0, 0.0],
                    faction_id: cfid,
                    wormhole_class_id: None,
                },
            );
            let mut sys = template_sys.clone();
            sys.solar_system_id = sid;
            sys.constellation_id = cid;
            sys.region_id = 10_999_999;
            sde_mut.solar_systems.insert(sid, sys);
            let _ = expected;
        }
        // Region fallback systems (constellation has no faction_id, region has no faction_id)
        for (idx, (rid, _)) in [
            (10_000_002u32, 500_010u32),
            (10_000_020, 500_012),
            (10_000_001, 500_019),
            (10_000_027, 500_020),
            (10_000_005, 500_011),
            (10_999_888, 500_010),
        ]
        .into_iter()
        .enumerate()
        {
            let sid = 39_900_100 + idx as u32;
            let cid = 29_900_100 + idx as u32;
            sde_mut.constellations.insert(
                cid,
                ConstellationInfo {
                    constellation_id: cid,
                    region_id: rid,
                    name: format!("RC{idx}"),
                    center: [0.0, 0.0, 0.0],
                    faction_id: None,
                    wormhole_class_id: None,
                },
            );
            sde_mut.regions.insert(
                rid,
                RegionInfo {
                    region_id: rid,
                    name: format!("R{idx}"),
                    center: [0.0, 0.0, 0.0],
                    faction_id: None,
                    wormhole_class_id: None,
                },
            );
            let mut sys = template_sys.clone();
            sys.solar_system_id = sid;
            sys.constellation_id = cid;
            sys.region_id = rid;
            sde_mut.solar_systems.insert(sid, sys);
        }
    }

    for (idx, expected) in [
        500_010u32, 500_011, 500_011, 500_012, 500_012, 500_019, 500_019, 500_020, 500_020,
        500_020,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(regional_pirate_faction_id(&ctx, 39_900_000 + idx as u32), expected);
    }
    for (idx, expected) in [500_010u32, 500_012, 500_019, 500_020, 500_011, 500_010]
        .into_iter()
        .enumerate()
    {
        assert_eq!(regional_pirate_faction_id(&ctx, 39_900_100 + idx as u32), expected);
    }
}

#[tokio::test]
async fn npc_ai_orbit_attack_cooldown_and_destruction_factions_and_abyssal() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let player_char = 90_000_001;
    let player_ship = 100_400_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            player_ship,
            InventoryItemRecord {
                item_id: player_ship,
                type_id: 587,
                owner_id: player_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&player_char) {
            ch.active_ship_id = player_ship;
            ch.solar_system_id = sys_id;
        }
    });
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: player_ship,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(player_char, player_ship, 587),
    );

    let mut mgr = NpcSpawnManager::new();
    let npc_ball_id = 100_400_010;
    // Set optimal_range = 4_000.0 (1.25x = 5_000.0) and orbit_range = 6_000.0 (1.4x = 8_400.0)
    // At dist = 7_000.0: 5_000.0 < 7_000.0 <= 8_400.0 -> transitions to NpcAiState::Orbit without attacking!
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: npc_ball_id,
            type_id: 597,
            position: DVec3::new(7_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 320.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 45.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(597),
    );
    mgr.entities.insert(
        npc_ball_id,
        NpcCombatEntity {
            ball_id: npc_ball_id,
            type_id: 597,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 1,
            spawn_pos: DVec3::new(7_000.0, 0.0, 0.0),
            target_id: None,
            bounty_isk: 50_000.0,
            optimal_range: 4_000.0,
            orbit_range: 6_000.0,
            base_dps: 100.0,
            damage_vector: faction_damage_vector(500_010, 100.0),
            last_attack_tick: 10,
            has_shield_rep: true,
            has_armor_rep: true,
        },
    );

    mgr.step_ai_sync(&ctx, sys_id, 11, 1.0);
    assert_eq!(mgr.entities[&npc_ball_id].state, NpcAiState::Orbit);
    assert_eq!(mgr.entities[&npc_ball_id].last_attack_tick, 10);

    // Move NPC to 4_000.0 m (<= optimal * 1.25): at tick 11 (< 10 + 2), state is Attack but cooldown prevents firing
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&npc_ball_id) {
            b.position = DVec3::new(4_000.0, 0.0, 0.0);
        }
    }
    mgr.step_ai_sync(&ctx, sys_id, 11, 1.0);
    assert_eq!(mgr.entities[&npc_ball_id].state, NpcAiState::Attack);
    assert_eq!(mgr.entities[&npc_ball_id].last_attack_tick, 10);

    // At tick 12 (== 10 + 2), fires damage and updates last_attack_tick to 12
    mgr.step_ai_sync(&ctx, sys_id, 12, 1.0);
    assert_eq!(mgr.entities[&npc_ball_id].last_attack_tick, 12);

    // Verify all faction ammo loot drops in normal system and no wreck in Abyssal system (32_000_000)
    for (idx, (fid, expected_ammo)) in [
        (500_010u32, 204u32),
        (500_012, 178),
        (500_020, 230),
        (500_011, 213),
        (999_999, 212),
    ]
    .into_iter()
    .enumerate()
    {
        let bid = 100_400_100 + idx as i64;
        mgr.entities.insert(
            bid,
            NpcCombatEntity {
                ball_id: bid,
                type_id: 597,
                faction_id: fid,
                state: NpcAiState::Idle,
                spawn_group_id: 10 + idx as u32,
                spawn_pos: DVec3::ZERO,
                target_id: None,
                bounty_isk: 20_000.0,
                optimal_range: 5_000.0,
                orbit_range: 4_000.0,
                base_dps: 40.0,
                damage_vector: faction_damage_vector(fid, 40.0),
                last_attack_tick: 0,
                has_shield_rep: false,
                has_armor_rep: false,
            },
        );
        mgr.handle_npc_destroyed(&ctx, sys_id, bid, player_char, 20_000)
            .unwrap();
        let has_ammo = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .any(|i| i.owner_id == player_char && i.type_id == expected_ammo && i.quantity == 100)
        });
        assert!(has_ammo);
    }

    // Abyssal system (32_000_000) does not spawn wreck 26557
    let abyss_bid = 100_400_200;
    let wrecks_before = ctx
        .db
        .with_snapshot(|s| s.items.values().filter(|i| i.type_id == 26557).count());
    mgr.entities.insert(
        abyss_bid,
        NpcCombatEntity {
            ball_id: abyss_bid,
            type_id: 33132, // CloneSoldier Trainer -> also tests CloneSoldier respawn timer
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 99,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 50_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 40.0,
            damage_vector: faction_damage_vector(500_010, 40.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    mgr.handle_npc_destroyed(&ctx, 32_000_000, abyss_bid, player_char, 25_000)
        .unwrap();
    let wrecks_after = ctx
        .db
        .with_snapshot(|s| s.items.values().filter(|i| i.type_id == 26557).count());
    assert_eq!(wrecks_after, wrecks_before);
    assert_eq!(mgr.clone_soldier_respawn_timers.get(&32_000_000), Some(&26_200));
}
