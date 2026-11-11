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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    ctx.db
        .acquire_online_lock(1, 9812, Some(player_char), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(player_char);
    mgr.step_ai_sync(&ctx, sys_id, 12, 1.0);
    assert_eq!(mgr.entities[&npc_ball_id].last_attack_tick, 12);
    let hit_notifs = ctx.drain_notifications_for_char(player_char);
    assert!(
        hit_notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "step_ai_sync attack must emit OnDamageMessage to victim"
    );
    assert!(
        hit_notifs.iter().any(|n| n.event == "OnDamageStateChange"),
        "step_ai_sync attack must emit OnDamageStateChange to victim"
    );
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        let obs_evs = bp.drain_observer_events(player_ship);
        assert!(
            obs_evs
                .iter()
                .any(|e| matches!(e, eve_destiny::DestinyEvent::OnSpecialFX { ball_id, .. } if *ball_id == npc_ball_id)),
            "step_ai_sync attack must queue OnSpecialFX on ballpark"
        );
    }

    // Verify step_ai_sync never targets category 11 NPCs or category 25 asteroids (#792)
    let other_npc_ball = 100_400_020;
    let ast_ball = 100_400_021;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.remove_ball(player_ship);
    }
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: other_npc_ball,
                type_id: 597,
                position: DVec3::new(4_100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(597),
            eve_destiny::SlimItemMeta {
                group_id: 550,
                category_id: 11,
                owner_id: 500_010,
                corp_id: 500_010,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ast_ball,
                type_id: 1228,
                position: DVec3::new(4_200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(1228),
            eve_destiny::SlimItemMeta {
                group_id: 462,
                category_id: 25,
                owner_id: 1,
                ..Default::default()
            },
        )
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 15, 1.0);
    assert_eq!(
        mgr.entities[&npc_ball_id].state,
        NpcAiState::Idle,
        "NPC must transition to Idle when only category 11 NPCs and category 25 asteroids are on grid"
    );
    assert_eq!(mgr.entities[&npc_ball_id].target_id, None);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_spawn_belt_rats_drifters_mordu_commanders_officers_and_small_sde() {
    // 1. Small SDE branch (<= 1000 types) for clone_soldier_type_for_security_and_faction & compute_sleeper_sentry_interval_s
    let mut small_sde = SdeRegistry::default();
    assert_eq!(
        clone_soldier_type_for_security_and_faction(&small_sde, 0.25, 500_010),
        Some(33142)
    );
    let mut sentry_attrs = HashMap::new();
    sentry_attrs.insert(51, 8_000.0);
    small_sde.types.insert(
        77_001,
        eve_sde::InvType {
            type_id: 77_001,
            group_id: 556,
            category_id: 11,
            name: "Guristas Clone Soldier Recruiter".to_string(),
            mass: 1_000.0,
            volume: 1_000.0,
            capacity: 0.0,
            radius: 40.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: Some(500_010),
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: sentry_attrs,
        },
    );
    assert_eq!(
        clone_soldier_type_for_security_and_faction(&small_sde, 0.25, 500_010),
        Some(77_001)
    );
    let small_ctx = ServerContext::with_sde_and_repository(
        Arc::new(small_sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    assert!((compute_sleeper_sentry_interval_s(&small_ctx, 77_001) - 8.0).abs() < 1e-12);
    assert!((compute_sleeper_sentry_interval_s(&small_ctx, 999_999) - 15.0).abs() < 1e-12);

    // 2. Wormhole / Drifter system spawning: bs_group 1530 (31000001..=31000006) & 1310 (31000010), plus cooldown
    let ctx = test_ctx();
    let mut mgr = NpcSpawnManager::new();
    let wh1 = mgr.spawn_belt_rats_for_system(&ctx, 31_000_001).await.unwrap();
    assert_eq!(wh1.len(), 2);
    // Second call while alive & on cooldown returns empty
    assert!(mgr.spawn_belt_rats_for_system(&ctx, 31_000_001).await.unwrap().is_empty());
    let wh2 = mgr.spawn_belt_rats_for_system(&ctx, 31_000_010).await.unwrap();
    assert_eq!(wh2.len(), 2);

    // 3. System with no belts returns empty
    let find_sys = |pred: &dyn Fn(&eve_sde::SolarSystem) -> bool| -> u32 {
        let mut candidates: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 31_000_000 && pred(s))
            .map(|s| s.solar_system_id)
            .collect();
        candidates.sort_unstable();
        candidates
            .into_iter()
            .find(|&sid| {
                let cels = ctx.sde.celestials_in_system(sid);
                cels.iter().any(|c| c.group_id == 9) && cels.iter().any(|c| c.group_id == 10)
            })
            .expect("matching SDE solar system with belt + stargate")
    };
    let no_belt_sys = ctx
        .sde
        .solar_systems
        .values()
        .map(|s| s.solar_system_id)
        .find(|&sid| sid < 31_000_000 && !ctx.sde.celestials_in_system(sid).iter().any(|c| c.group_id == 9))
        .unwrap_or(39_800_001);
    let lowsec_sys_035 = find_sys(&|s| (0.35..0.45).contains(&s.security_status));
    let lowsec_sys_020 = find_sys(&|s| (0.15..0.30).contains(&s.security_status));
    let lowsec_sys_010 = find_sys(&|s| (0.05..0.15).contains(&s.security_status));
    let nullsec_angel = find_sys(&|s| s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_011);
    let nullsec_blood = find_sys(&|s| s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_012);
    let nullsec_sansha = find_sys(&|s| s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_019);
    let nullsec_serpentis = find_sys(&|s| s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_020);
    let nullsec_guristas = find_sys(&|s| s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_010);

    assert!(mgr.spawn_belt_rats_for_system(&ctx, no_belt_sys).await.unwrap().is_empty());

    // Find a deterministic seed where the first 3 Bernoulli draws are all < 0.08 (triggers Mordu + Commander + Officer)
    let lucky_seed = (1u64..10_000)
        .find(|&s| {
            let rng = crate::rng::MechanicRng::seeded(s);
            rng.roll(0.08) && rng.roll(0.08) && rng.roll(0.08)
        })
        .expect("find lucky seed");

    // Low-sec bands: 0.35 (Mordu grp 1285), 0.20 (1286), 0.10 (1287) + gate + Clone Soldier + cooldown sync
    let now = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.clone_soldier_cooldown_until.insert(lowsec_sys_035, now + 600);
    });
    for sid in [lowsec_sys_035, lowsec_sys_020, lowsec_sys_010] {
        ctx.set_mechanic_rng_seed(Some(lucky_seed));
        let spawned = mgr.spawn_belt_rats_for_system(&ctx, sid).await.unwrap();
        assert!(!spawned.is_empty());
        // Calling again immediately hits belt_alive / respawn_timers / mordu_cd / cmd_cd skip branches
        let again = mgr.spawn_belt_rats_for_system(&ctx, sid).await.unwrap();
        assert!(again.is_empty());
    }
    assert!(mgr.clone_soldier_respawn_timers.contains_key(&lowsec_sys_035));

    // Null-sec across all 5 regional pirate factions: spawns belt rats + gate rats + Faction Commander + Named Officer
    for sid in [
        nullsec_angel,
        nullsec_blood,
        nullsec_sansha,
        nullsec_serpentis,
        nullsec_guristas,
    ] {
        ctx.set_mechanic_rng_seed(Some(lucky_seed));
        let spawned = mgr.spawn_belt_rats_for_system(&ctx, sid).await.unwrap();
        assert!(!spawned.is_empty());
        // Re-check Officer/Commander cooldown branches
        assert!(mgr.spawn_belt_rats_for_system(&ctx, sid).await.unwrap().is_empty());
    }

    // 4. Synthetic candidate_groups fallback when SDE has no matching class/faction group
    let highsec_sys = find_sys(&|s| {
        s.security_status >= 0.45
            && ctx
                .sde
                .celestials_in_system(s.solar_system_id)
                .iter()
                .any(|c| c.group_id == 9)
    });
    let mut fallback_sde = SdeRegistry::default();
    fallback_sde.db_path = ctx.sde.db_path.clone();
    fallback_sde
        .solar_systems
        .insert(highsec_sys, ctx.sde.solar_systems[&highsec_sys].clone());
    for c in ctx.sde.celestials_in_system_slice(highsec_sys) {
        fallback_sde.insert_celestial(c.clone());
    }
    fallback_sde.npc_spawn_groups.insert(
        999_001,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: 999_001,
            faction_id: 500_012, // mismatch with Guristas 500_010
            ship_class: 6,       // > max_ship_class (2)
            group_id: 550,
            group_name: "FallbackBS".to_string(),
            npc_type_ids: vec![597],
        },
    );
    let fb_ctx = ServerContext::with_sde_and_repository(
        Arc::new(fallback_sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let mut fb_mgr = NpcSpawnManager::new();
    let fb_spawned = fb_mgr.spawn_belt_rats_for_system(&fb_ctx, highsec_sys).await.unwrap();
    assert!(!fb_spawned.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_chase_idle_ewar_logi_threat_and_ball_destruction_branches() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now = ctx.now_unix();

    let player_char = 90_000_001;
    let player_ship = 100_450_001;
    let logi_ship = 100_450_002;
    let ewar_ship = 100_450_003;
    let disc_char = 90_000_002;
    let disc_ship = 100_450_004;
    let drone_id = 100_450_005;
    let ext_npc_id = 100_450_006;

    // Look up one EWAR module (group 52) and one Logi module (group 41) in SDE
    let ewar_mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 52)
        .map(|t| t.type_id)
        .expect("group 52 EWAR type");
    let logi_mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 41)
        .map(|t| t.type_id)
        .expect("group 41 Logi type");

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, owner) in [(player_ship, player_char), (logi_ship, 90_000_002), (ewar_ship, 90_000_003)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: owner,
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
        }
        if let Some(ch) = snap.characters.get_mut(&player_char) {
            ch.active_ship_id = player_ship;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        // Active Logi module on logi_ship & active EWAR module on ewar_ship
        snap.items.insert(
            100_450_101,
            InventoryItemRecord {
                item_id: 100_450_101,
                type_id: logi_mod_tid,
                owner_id: 90_000_002,
                location_id: logi_ship,
                flag: InventoryFlag::MedSlot0,
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
        snap.items.insert(
            100_450_102,
            InventoryItemRecord {
                item_id: 100_450_102,
                type_id: ewar_mod_tid,
                owner_id: 90_000_003,
                location_id: ewar_ship,
                flag: InventoryFlag::MedSlot0,
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
        snap.module_active_states.insert(100_450_101);
        snap.module_active_states.insert(100_450_102);

        // Disconnected ship entry
        snap.items.insert(
            disc_ship,
            InventoryItemRecord {
                item_id: disc_ship,
                type_id: 587,
                owner_id: disc_char,
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
        snap.sp6_lifecycle.disconnected_ships.insert(
            disc_char,
            eve_db::DisconnectedShipEntry {
                char_id: disc_char,
                ship_id: disc_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now.saturating_sub(10),
                remove_at_unix: now + 900,
                emergency_warp_dest: None,
                last_position: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );

        // Drone and extended NPC items
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
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
        snap.items.insert(
            ext_npc_id,
            InventoryItemRecord {
                item_id: ext_npc_id,
                type_id: 597,
                owner_id: 1_000_125,
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
    });

    // Spawn player_ship (20km away -> Chase), logi_ship (30km), ewar_ship (35km -> highest threat 1.5x)
    for (bid, pos) in [
        (player_ship, DVec3::new(20_000.0, 0.0, 0.0)),
        (logi_ship, DVec3::new(30_000.0, 0.0, 0.0)),
        (ewar_ship, DVec3::new(35_000.0, 0.0, 0.0)),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(player_char, bid, 587),
        );
    }

    let mut mgr = NpcSpawnManager::new();
    let npc_ball = 100_450_201;
    let missing_clone_ball = 100_450_202;
    let missing_rat_ball = 100_450_203;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: npc_ball,
            type_id: 597,
            position: DVec3::ZERO,
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
        npc_ball,
        NpcCombatEntity {
            ball_id: npc_ball,
            type_id: 597,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 11,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 40.0,
            damage_vector: faction_damage_vector(500_010, 40.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    // Two entities NOT present in ballpark -> triggers `to_remove` CloneSoldier & normal respawn timers
    mgr.entities.insert(
        missing_clone_ball,
        NpcCombatEntity {
            ball_id: missing_clone_ball,
            type_id: 33132,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 12,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 40.0,
            damage_vector: faction_damage_vector(500_010, 40.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    mgr.entities.insert(
        missing_rat_ball,
        NpcCombatEntity {
            ball_id: missing_rat_ball,
            type_id: 597,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 13,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 10_000.0,
            optimal_range: 5_000.0,
            orbit_range: 4_000.0,
            base_dps: 40.0,
            damage_vector: faction_damage_vector(500_010, 40.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );

    // Step AI: selects ewar_ship (1.5x threat) and transitions to Chase
    mgr.step_ai(&ctx, sys_id, 5, 1.0).await;
    assert_eq!(mgr.entities[&npc_ball].state, NpcAiState::Chase);
    assert_eq!(mgr.entities[&npc_ball].target_id, Some(ewar_ship));
    assert!(!mgr.entities.contains_key(&missing_clone_ball));
    assert!(!mgr.entities.contains_key(&missing_rat_ball));
    assert!(mgr.clone_soldier_respawn_timers.contains_key(&sys_id));
    assert!(mgr.respawn_timers.contains_key(&(sys_id, 13)));

    // Move all ships beyond 50 km -> NPC transitions back to Idle with MovementMode::Stop
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        for bid in [player_ship, logi_ship, ewar_ship] {
            if let Some(b) = bp.balls.get_mut(&bid) {
                b.position = DVec3::new(80_000.0, 0.0, 0.0);
            }
        }
    }
    mgr.step_ai_sync(&ctx, sys_id, 6, 1.0);
    assert_eq!(mgr.entities[&npc_ball].state, NpcAiState::Idle);
    assert_eq!(mgr.entities[&npc_ball].target_id, None);

    // Move player_ship to 1km with 1 HP and huge NPC DPS -> step_ai_sync destroys player_ship with assist log!
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.entry(player_ship).or_default().push(
            crate::services::combat_crimewatch::AssistLogEntry {
                attacker_char_id: 90_000_003,
                corporation_id: 1_000_035,
                alliance_id: None,
                ship_type_id: 587,
                weapon_type_id: 486,
                damage_done: 10.0,
                is_ewar: false,
                last_action_unix: now,
            },
        );
        rt.active_drones_in_space.insert(
            drone_id,
            crate::services::combat_crimewatch::DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: player_char,
                original_owner_char_id: player_char,
                parent_ship_id: player_ship,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1000.0,
                optimal_range: 5000.0,
                damage_vector: DamageVector::new(0.0, 0.0, 0.0, 0.0),
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
            ext_npc_id,
            crate::services::combat_crimewatch::ExtendedNpcEntity {
                ball_id: ext_npc_id,
                type_id: 597,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: sys_id,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 1_000.0,
                optimal_range: 5_000.0,
                orbit_range: 4_000.0,
                damage_vector: DamageVector::new(0.0, 0.0, 0.0, 0.0),
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
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&player_ship) {
            b.position = DVec3::new(1_000.0, 0.0, 0.0);
        }
        if let Some(cs) = bp.combat_states.get_mut(&player_ship) {
            cs.shield_hp = 0.0;
            cs.armor_hp = 0.0;
            cs.structure_hp = 1.0;
            cs.shield_resonance = [1.0; 4];
            cs.armor_resonance = [1.0; 4];
            cs.structure_resonance = [1.0; 4];
        }
    }
    mgr.entities.get_mut(&npc_ball).unwrap().damage_vector =
        DamageVector::new(1_000.0, 1_000.0, 1_000.0, 1_000.0);
    mgr.step_ai_sync(&ctx, sys_id, 10, 1.0);

    // Direct handle_npc_ai_ball_destructions coverage: empty no-op, drone, disconnected ship, unpiloted ship, and extended NPC
    handle_npc_ai_ball_destructions(&ctx, sys_id, Vec::new());
    let dummy_ball = |id: i64| Ball {
        id,
        type_id: 587,
        position: DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![
            (npc_ball, drone_id, dummy_ball(drone_id)),
            (npc_ball, disc_ship, dummy_ball(disc_ship)),
            (npc_ball, logi_ship, dummy_ball(logi_ship)),
            (npc_ball, ext_npc_id, dummy_ball(ext_npc_id)),
        ],
    );
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&drone_id));
        assert!(!rt.npc_entities.contains_key(&ext_npc_id));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&drone_id));
        assert!(!snap.items.contains_key(&ext_npc_id));
    });
}
