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
            ch.station_id = None;
            ch.structure_id = None;
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

    // Verify SDE-derived wrecks, size-scaled scraps, and weapon-matched ammo loot (and zero wreck in Abyssal system 32_000_000)
    for (idx, (type_id, fid, expected_wreck, expected_scrap_qty, expected_ammo)) in [
        (2_382u32, 500_010u32, 26_573u32, 2i32, Some((222u32, 25i32))),   // Guristas Arrogator (Frigate Hybrid)
        (2_381u32, 500_020u32, 26_586u32, 5i32, Some((230u32, 60i32))),   // Serpentis Chief Spy (Cruiser Hybrid)
        (17_641u32, 500_011u32, 26_563u32, 10i32, Some((199u32, 120i32))), // Arch Angel Raelek (Battleship Projectile)
        (2_386u32, 500_010u32, 26_573u32, 2i32, Some((266u32, 25i32))),   // Guristas Plunderer (Frigate Missile)
        (79_589u32, 500_014u32, 79_586u32, 20i32, None),                   // ORE Rorqual (Capital unarmed)
    ]
    .into_iter()
    .enumerate()
    {
        let bid = 100_400_100 + idx as i64;
        let spawn_pos = DVec3::new((idx as f64 + 1.0) * 1_000.0, 0.0, 0.0);
        mgr.entities.insert(
            bid,
            NpcCombatEntity {
                ball_id: bid,
                type_id,
                faction_id: fid,
                state: NpcAiState::Idle,
                spawn_group_id: 10 + idx as u32,
                spawn_pos,
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
        let before_wrecks: std::collections::HashSet<i64> = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == sys_id as i64 && i.type_id == expected_wreck)
                .map(|i| i.item_id)
                .collect()
        });
        mgr.handle_npc_destroyed(&ctx, sys_id, bid, player_char, 20_000)
            .unwrap();
        ctx.db.with_snapshot(|snap| {
            let new_wreck = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == sys_id as i64
                        && i.type_id == expected_wreck
                        && i.owner_id == player_char
                        && i.singleton
                        && !before_wrecks.contains(&i.item_id)
                })
                .expect("SDE wreck item must spawn in system");
            let contents: Vec<_> = snap
                .items
                .values()
                .filter(|i| i.location_id == new_wreck.item_id)
                .collect();
            assert!(
                contents.iter().any(|i| i.type_id == 15_331 && i.quantity == expected_scrap_qty && i.owner_id == player_char),
                "expected {expected_scrap_qty}x Metal Scraps inside wreck {expected_wreck} for npc {type_id}"
            );
            match expected_ammo {
                Some((ammo_tid, ammo_qty)) => assert!(
                    contents.iter().any(|i| i.type_id == ammo_tid && i.quantity == ammo_qty && i.owner_id == player_char),
                    "expected {ammo_qty}x weapon ammo {ammo_tid} inside wreck {expected_wreck} for npc {type_id}"
                ),
                None => assert_eq!(
                    contents.len(),
                    1,
                    "unarmed npc {type_id} should drop only metal scraps, no weapon ammo"
                ),
            }
        });
    }

    // Abyssal system (32_000_000) does not spawn wrecks or loot items
    let abyss_bid = 100_400_200;
    let items_before = ctx.db.with_snapshot(|s| s.items.len());
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
    let items_after = ctx.db.with_snapshot(|s| s.items.len());
    assert_eq!(items_after, items_before);
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
                last_velocity: [0.0, 0.0, 0.0],
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

fn make_target_ball(
    id: i64,
    type_id: u32,
    position: DVec3,
    velocity: DVec3,
    radius: f64,
) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity,
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_sponge_combat_state(shield_hp: f64, sig_radius: f64) -> eve_destiny::CombatState {
    eve_destiny::CombatState {
        max_shield_hp: shield_hp,
        shield_hp,
        max_armor_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        max_structure_hp: 1_000_000.0,
        structure_hp: 1_000_000.0,
        shield_resonance: [1.0; 4],
        armor_resonance: [1.0; 4],
        structure_resonance: [1.0; 4],
        signature_radius: sig_radius,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_sde_dps_within_one_percent_across_five_ship_classes_and_factions() {
    // (#1396) Verify 5 real V24.02 NPC classes/factions use SDE weapon stats (not bounty/500)
    // and match analytical SDE DPS expectation within 1% on stationary large targets.
    let cases = [
        (16_936_u32, 500_012_u32, 1_u32, "Corpii Follower (Frigate/Laser)"),
        (23_974_u32, 500_011_u32, 2_u32, "Gistior Defacer (Destroyer/Projectile)"),
        (17_039_u32, 500_020_u32, 3_u32, "Corelum Chief Guard (Cruiser/Hybrid)"),
        (24_019_u32, 500_019_u32, 4_u32, "Centatis Specter (BC/Laser)"),
        (16_984_u32, 500_010_u32, 5_u32, "Pith Dismantler (BS/Missile)"),
    ];
    let n_ticks = 6_000_u64;
    let initial_shield = 1e12_f64;

    for (idx, &(npc_type_id, faction_id, ship_class, label)) in cases.iter().enumerate() {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        let sg_id = 998_100 + idx as u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id,
                ship_class,
                group_id: 550,
                group_name: label.to_string(),
                npc_type_ids: vec![npc_type_id],
            },
        );

        let (profile, fallbacks) = inspect_npc_sde_profile(&ctx.sde, npc_type_id, None, faction_id);
        assert!(
            fallbacks.is_empty(),
            "{label} ({npc_type_id}) should resolve without SDE fallbacks, got {fallbacks:?}"
        );
        assert!(!profile.is_synthetic_fallback);

        let legacy_bounty_dps = (profile.bounty_isk / 500.0).clamp(20.0, 250.0);
        assert!(
            (profile.total_nominal_dps - legacy_bounty_dps).abs() > 1.0,
            "{label} SDE DPS ({:.3}) should differ from legacy bounty/500 ({:.3})",
            profile.total_nominal_dps,
            legacy_bounty_dps
        );

        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr
            .spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO)
            .await
            .expect("spawn_group");
        let npc_bid = spawned[0];
        let ent = &mgr.entities[&npc_bid];
        assert!(
            (ent.base_dps - profile.total_nominal_dps).abs() < 1e-6,
            "{label} entity.base_dps mismatch"
        );

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;
        let target_bid = 900_100_000 + idx as i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                587,
                npc_pos + DVec3::new(800.0, 0.0, 0.0),
                DVec3::ZERO,
                40.0,
            ),
            make_sponge_combat_state(initial_shield, 1_000.0),
        );

        for tick in 1..=n_ticks {
            mgr.step_ai_sync(&ctx, sys_id, tick, 1.0);
            if tick % 500 == 0 {
                sys_handle.ballpark.write().unwrap().pending_events.clear();
            }
        }

        let remaining_shield = sys_handle
            .ballpark
            .read()
            .unwrap()
            .combat_states[&target_bid]
            .shield_hp;
        let empirical_dps = (initial_shield - remaining_shield) / (n_ticks as f64);

        // Analytical turret expectation at P_hit = 1.0:
        // E[turret_shot_damage] = 0.01*3.0 + integral_{0.01}^{1.0} (0.49 + u) du = 1.01505 * shot_damage.
        let turret_expected_dps = profile
            .turret
            .as_ref()
            .map(|w| (w.shot_damage.total() / w.cycle_duration_s) * 1.01505)
            .unwrap_or(0.0);
        let missile_expected_dps = profile
            .missile
            .as_ref()
            .map(|m| m.volley_damage.total() / m.cycle_duration_s)
            .unwrap_or(0.0);
        let expected_dps = turret_expected_dps + missile_expected_dps;
        let rel_err = (empirical_dps - expected_dps).abs() / expected_dps.max(1e-6);
        assert!(
            rel_err <= 0.01,
            "{label}: empirical DPS {empirical_dps:.4} vs expected {expected_dps:.4} (rel_err={:.3}%)",
            rel_err * 100.0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_turret_normal_hit_rules_angular_velocity_sig_and_range_cutoff() {
    // (#1396) Serpentis BS Hybrid NPC (17038 Core Baron): opt=4000, fall=6000, trk=0.05, res=400.
    let run_scenario = |target_type_id: u32,
                        center_dist_m: f64,
                        target_vel: DVec3,
                        target_sig: f64,
                        n_ticks: u64| async move {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        let sg_id = 998_201_u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id: 500_020,
                ship_class: 5,
                group_id: 550,
                group_name: "Core Baron".to_string(),
                npc_type_ids: vec![17_038],
            },
        );
        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr
            .spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO)
            .await
            .unwrap();
        let npc_bid = spawned[0];
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;
        let initial_shield = 1e9_f64;
        let target_bid = 900_200_001_i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                target_type_id,
                npc_pos + DVec3::new(center_dist_m, 0.0, 0.0),
                target_vel,
                37.0,
            ),
            make_sponge_combat_state(initial_shield, target_sig),
        );
        for tick in 1..=n_ticks {
            mgr.step_ai_sync(&ctx, sys_id, tick, 1.0);
        }
        let remaining = sys_handle
            .ballpark
            .read()
            .unwrap()
            .combat_states[&target_bid]
            .shield_hp;
        initial_shield - remaining
    };

    // 1. Stationary optimal (4,000m) -> full DPS
    let dmg_stationary = run_scenario(587, 4_000.0, DVec3::ZERO, 400.0, 400).await;
    // 2. Slow-orbiting small frigate (4,000m, sig 40m, 20 m/s perpendicular -> omega 0.005, P_hit ~ 0.50)
    let dmg_orbit_frigate = run_scenario(587, 4_000.0, DVec3::new(0.0, 20.0, 0.0), 40.0, 400).await;
    // 3. Fast-orbiting small frigate (4,000m, sig 40m, 200 m/s perpendicular -> P_hit ~ 0)
    let dmg_fast_frigate = run_scenario(587, 4_000.0, DVec3::new(0.0, 200.0, 0.0), 40.0, 400).await;
    // 4. Sig-bloomed frigate (200m vs 40m at identical 20 m/s orbit -> higher chance to hit)
    let dmg_bloomed_frigate = run_scenario(587, 4_000.0, DVec3::new(0.0, 20.0, 0.0), 200.0, 400).await;
    // 5. Falloff target (10,500m > old 1.25 * 4,000 = 5,000m cliff, still <= opt + 3*fall = 22,000m)
    let dmg_falloff = run_scenario(587, 10_500.0, DVec3::ZERO, 400.0, 400).await;
    // 6. Beyond opt + 3*fall (23,500m) -> strictly 0 damage (no wrecking hits out of range)
    let dmg_out_of_range = run_scenario(587, 23_500.0, DVec3::ZERO, 400.0, 100).await;

    assert!(dmg_stationary > 10_000.0);
    assert!(
        dmg_orbit_frigate > 0.0 && dmg_orbit_frigate < dmg_stationary * 0.65,
        "orbiting frigate ({dmg_orbit_frigate}) must take clearly less damage than stationary ({dmg_stationary})"
    );
    assert!(
        dmg_fast_frigate < dmg_orbit_frigate * 0.1,
        "high-transversal frigate ({dmg_fast_frigate}) must almost never be hit by BS turrets"
    );
    assert!(
        dmg_bloomed_frigate > dmg_orbit_frigate * 1.4,
        "MWD/bloomed sig ({dmg_bloomed_frigate}) must take substantially more turret damage than 40m ({dmg_orbit_frigate})"
    );
    assert!(
        dmg_falloff > 0.0 && dmg_falloff < dmg_stationary * 0.65,
        "falloff target ({dmg_falloff}) must still take partial hits beyond 1.25*optimal"
    );
    assert_eq!(dmg_out_of_range, 0.0, "out-of-range turret target must take zero damage");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_missile_application_sig_speed_and_flight_range_cutoff() {
    // (#1396) Guristas BS Missile (16984 Pith Dismantler) + Rocket Frigate (2386 Guristas Plunderer).
    let run_missile = |npc_type_id: u32, dist_m: f64, target_vel: DVec3, target_sig: f64| async move {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        let sg_id = 998_301_u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id: 500_010,
                ship_class: 5,
                group_id: 550,
                group_name: "Missile Test".to_string(),
                npc_type_ids: vec![npc_type_id],
            },
        );
        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr
            .spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO)
            .await
            .unwrap();
        let npc_bid = spawned[0];
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;
        let initial_shield = 1e6_f64;
        let target_bid = 900_300_001_i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                587,
                npc_pos + DVec3::new(dist_m, 0.0, 0.0),
                target_vel,
                37.0,
            ),
            make_sponge_combat_state(initial_shield, target_sig),
        );
        mgr.step_ai_sync(&ctx, sys_id, 1, 1.0);
        let remaining = sys_handle
            .ballpark
            .read()
            .unwrap()
            .combat_states[&target_bid]
            .shield_hp;
        initial_shield - remaining
    };

    // 1. Pith Dismantler (16984): Cruise er ~ 257m, ev ~ 99.5 m/s, volley 558.75
    let dmg_large_slow = run_missile(16_984, 5_000.0, DVec3::ZERO, 500.0).await;
    let dmg_small_slow = run_missile(16_984, 5_000.0, DVec3::ZERO, 40.0).await;
    let dmg_small_fast = run_missile(16_984, 5_000.0, DVec3::new(0.0, 400.0, 0.0), 40.0).await;

    assert!((dmg_large_slow - 558.75).abs() < 1e-3);
    assert!(
        dmg_small_slow > 0.0 && dmg_small_slow < dmg_large_slow * 0.25,
        "40m sig frigate ({dmg_small_slow}) must mitigate Cruise missile ({dmg_large_slow})"
    );
    assert!(
        dmg_small_fast > 0.0 && dmg_small_fast < dmg_small_slow * 0.5,
        "400 m/s fast frigate ({dmg_small_fast}) must mitigate further than stationary ({dmg_small_slow})"
    );

    // 2. Guristas Plunderer (2386): Rocket max_flight_range = 5,670.03m
    let dmg_rocket_oor = run_missile(2_386, 10_000.0, DVec3::ZERO, 40.0).await;
    let dmg_rocket_in = run_missile(2_386, 4_000.0, DVec3::ZERO, 40.0).await;
    assert_eq!(dmg_rocket_oor, 0.0, "missile beyond max flight range must apply 0 damage");
    assert!(dmg_rocket_in > 0.0, "missile inside max flight range must hit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_on_special_fx_guids_and_movement_cruise_fraction() {
    // (#1396) Check weapon-kind OnSpecialFX GUIDs and SDE entityFlyRange/2786 + entityCruiseSpeed.
    let fx_cases = [
        (16_936_u32, 500_012_u32, "effects.Laser"),
        (17_039_u32, 500_020_u32, "effects.HybridFired"),
        (23_974_u32, 500_011_u32, "effects.ProjectileFired"),
        (16_984_u32, 500_010_u32, "effects.MissileDeployment"),
        (48_092_u32, 500_026_u32, "effects.TriglavianBeam"),
        (56_188_u32, 500_027_u32, "effects.AttackMode"),
    ];

    for (idx, &(npc_type_id, faction_id, expected_guid)) in fx_cases.iter().enumerate() {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        let sg_id = 998_400 + idx as u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id,
                ship_class: 2,
                group_id: 550,
                group_name: format!("FX {npc_type_id}"),
                npc_type_ids: vec![npc_type_id],
            },
        );
        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr
            .spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO)
            .await
            .unwrap();
        let npc_bid = spawned[0];
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;
        let target_bid = 900_400_000 + idx as i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                587,
                npc_pos + DVec3::new(1_000.0, 0.0, 0.0),
                DVec3::ZERO,
                37.0,
            ),
            make_sponge_combat_state(1e6, 400.0),
        );
        {
            let mut bp = sys_handle.ballpark.write().unwrap();
            bp.pending_events.clear();
        }
        mgr.step_ai_sync(&ctx, sys_id, 1, 1.0);
        let (fx_evs, encoded_fx_wires) = {
            let mut bp = sys_handle.ballpark.write().unwrap();
            let evs = std::mem::take(&mut bp.pending_events);
            let encoded: Vec<eve_proto::PyRep> = evs
                .iter()
                .filter(|ev| matches!(ev, eve_destiny::DestinyEvent::OnSpecialFX { ball_id, .. } if *ball_id == npc_bid))
                .map(|ev| eve_destiny::wire::encode_destiny_event(1, ev, Some(&bp)))
                .collect();
            (evs, encoded)
        };
        assert!(
            fx_evs.iter().any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::OnSpecialFX { ball_id, module_id: None, guid, is_active: true }
                    if *ball_id == npc_bid && guid == expected_guid
            )),
            "type_id {npc_type_id} must emit OnSpecialFX(module_id: None) {expected_guid}, got {fx_evs:?}"
        );
        assert_eq!(encoded_fx_wires.len(), 1, "expected 1 encoded OnSpecialFX for {npc_type_id}");
        let eve_proto::PyRep::Tuple(top) = &encoded_fx_wires[0] else {
            panic!("expected DestinyUpdate tuple, got {:?}", encoded_fx_wires[0]);
        };
        let eve_proto::PyRep::Tuple(act) = &top[1] else {
            panic!("expected action tuple, got {:?}", top[1]);
        };
        assert_eq!(act[0], eve_proto::PyRep::String("OnSpecialFX".into()));
        let eve_proto::PyRep::Tuple(args) = &act[1] else {
            panic!("expected 14-arg OnSpecialFX tuple");
        };
        assert_eq!(args.len(), 14);
        assert_eq!(args[0], eve_proto::PyRep::Integer(npc_bid));
        assert_eq!(args[1], eve_proto::PyRep::None, "NPC OnSpecialFX moduleID must be None");
        assert_eq!(
            args[3],
            eve_proto::PyRep::Integer(target_bid),
            "NPC OnSpecialFX targetID must encode target ball ID"
        );
        if npc_type_id == 16_984 {
            assert_eq!(args[2], eve_proto::PyRep::None);
            assert!(
                matches!(args[4], eve_proto::PyRep::Integer(mtid) if mtid > 0),
                "missile NPC OnSpecialFX must set otherTypeID to missile type ID, got {:?}",
                args[4]
            );
        } else {
            assert!(
                matches!(args[2], eve_proto::PyRep::Integer(tid) if tid > 0),
                "turret/vorton NPC OnSpecialFX must set moduleTypeID, got {:?}",
                args[2]
            );
            assert_eq!(args[4], eve_proto::PyRep::None);
        }
        assert_eq!(args[5], eve_proto::PyRep::String(expected_guid.into()));
        assert_eq!(args[6], eve_proto::PyRep::Bool(true), "isOffensive must be true");
        assert_eq!(args[7], eve_proto::PyRep::Integer(1));
        assert_eq!(args[8], eve_proto::PyRep::Integer(1));
        assert!(
            matches!(args[9], eve_proto::PyRep::Real(d) if d > 0.0),
            "duration_ms must be positive Real, got {:?}",
            args[9]
        );
        assert_eq!(args[10], eve_proto::PyRep::Integer(1));
    }

    // Movement check: 16936 Corpii Follower (maxVelocity=440, entityCruiseSpeed=220, entityFlyRange=3625)
    // and 47847 Sparkneedle Tessella (2786=5000, maxVelocity=1500, entityCruiseSpeed=1000).
    let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let sys_id = 30_000_142_u32;
    Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
        998_499,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: 998_499,
            faction_id: 500_012,
            ship_class: 1,
            group_id: 550,
            group_name: "Move Test".to_string(),
            npc_type_ids: vec![16_936, 47_847],
        },
    );
    let mut mgr = NpcSpawnManager::new();
    let spawned = mgr
        .spawn_group(&ctx, sys_id, 998_499, DVec3::ZERO)
        .await
        .unwrap();
    let follower_bid = spawned[0];
    let tessella_bid = spawned[1];
    assert_eq!(mgr.entities[&follower_bid].orbit_range, 3_625.0);
    assert_eq!(mgr.entities[&tessella_bid].orbit_range, 5_000.0);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let follower_pos = sys_handle
        .get_ball_snapshot(follower_bid)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    let target_bid = 900_499_001_i64;
    // Start 20 km away -> Chase at speed_fraction 1.0 (maxVelocity 440)
    sys_handle.try_spawn_ball_with_combat(
        make_target_ball(
            target_bid,
            587,
            follower_pos + DVec3::new(20_000.0, 0.0, 0.0),
            DVec3::ZERO,
            37.0,
        ),
        make_sponge_combat_state(1e6, 400.0),
    );
    {
        sys_handle.ballpark.write().unwrap().pending_events.clear();
    }
    mgr.step_ai_sync(&ctx, sys_id, 1, 1.0);
    assert_eq!(mgr.entities[&follower_bid].state, NpcAiState::Chase);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        let b = &bp.balls[&follower_bid];
        assert_eq!(b.max_velocity, 440.0);
        assert_eq!(b.speed_fraction, 1.0);
        assert!(matches!(
            b.mode,
            MovementMode::Follow { range, .. } if (range - 3_625.0).abs() < 1e-6
        ));
        let chase_sf_count = bp
            .pending_events
            .iter()
            .filter(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::SetSpeedFraction { ball_id, fraction }
                    if *ball_id == follower_bid && (*fraction - 1.0).abs() < 1e-6
            ))
            .count();
        assert_eq!(
            chase_sf_count, 1,
            "mobile Chase Follow transition must emit exactly one SetSpeedFraction(1.0)"
        );
    }

    // Move inside 3 km -> Orbit at entityCruiseSpeed / maxVelocity (220 / 440 = 0.5)
    sys_handle
        .set_ball_position(target_bid, follower_pos + DVec3::new(3_000.0, 0.0, 0.0))
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 2, 1.0);
    assert!(matches!(
        mgr.entities[&follower_bid].state,
        NpcAiState::Orbit | NpcAiState::Attack
    ));
    {
        let bp = sys_handle.ballpark.read().unwrap();
        let b = &bp.balls[&follower_bid];
        assert!((b.speed_fraction - 0.5).abs() < 1e-6);
        assert!(matches!(
            b.mode,
            MovementMode::Orbit { range, .. } if (range - 3_625.0).abs() < 1e-6
        ));
        let tb = &bp.balls[&tessella_bid];
        assert_eq!(tb.max_velocity, 1_500.0);
        if matches!(mgr.entities[&tessella_bid].state, NpcAiState::Orbit | NpcAiState::Attack) {
            assert!((tb.speed_fraction - (1_000.0 / 1_500.0)).abs() < 1e-6);
        }
    }

    // Move back to 20 km -> Orbit -> Chase must emit exactly one Follow and one SetSpeedFraction(1.0) (BF-R2-03)
    {
        sys_handle.ballpark.write().unwrap().pending_events.clear();
    }
    sys_handle
        .set_ball_position(target_bid, follower_pos + DVec3::new(20_000.0, 0.0, 0.0))
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 3, 1.0);
    assert_eq!(mgr.entities[&follower_bid].state, NpcAiState::Chase);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        let b = &bp.balls[&follower_bid];
        assert!((b.speed_fraction - 1.0).abs() < 1e-6);
        assert!(matches!(
            b.mode,
            MovementMode::Follow { target_id, range }
                if target_id == target_bid && (range - 3_625.0).abs() < 1e-6
        ));
        let follow_count = bp
            .pending_events
            .iter()
            .filter(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::Follow { ball_id, target_id, .. }
                    if *ball_id == follower_bid && *target_id == target_bid
            ))
            .count();
        let sf_fractions: Vec<f64> = bp
            .pending_events
            .iter()
            .filter_map(|ev| match ev {
                eve_destiny::DestinyEvent::SetSpeedFraction { ball_id, fraction }
                    if *ball_id == follower_bid =>
                {
                    Some(*fraction)
                }
                _ => None,
            })
            .collect();
        assert_eq!(follow_count, 1, "Orbit -> Chase must emit exactly one Follow event");
        assert_eq!(
            sf_fractions.len(),
            1,
            "Orbit -> Chase must emit exactly one SetSpeedFraction event, got {sf_fractions:?}"
        );
        assert!(
            (sf_fractions[0] - 1.0).abs() < 1e-6,
            "Orbit -> Chase SetSpeedFraction must be 1.0, got {:?}",
            sf_fractions[0]
        );
    }
}

#[test]
fn npc_ai_all_spawn_group_npcs_sde_profile_audit_and_once_per_type_fallback_log() {
    // (#1396) Audit every NPC type referenced in sde_npc_spawn_groups and report fallback counts.
    let mut ctx = test_ctx();
    let mut all_type_ids = std::collections::BTreeSet::new();
    for sg in ctx.sde.npc_spawn_groups.values() {
        for &tid in &sg.npc_type_ids {
            all_type_ids.insert(tid);
        }
    }
    assert!(
        all_type_ids.len() >= 7_000,
        "expected full V24 spawn group catalog, found {}",
        all_type_ids.len()
    );

    let mut turret_only = 0_usize;
    let mut missile_only = 0_usize;
    let mut both_weapons = 0_usize;
    let mut unarmed = 0_usize;
    let mut fallback_types = 0_usize;
    let mut reason_counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();

    for &tid in &all_type_ids {
        let (profile, fallbacks) = inspect_npc_sde_profile(&ctx.sde, tid, None, 0);
        assert!(
            !profile.is_synthetic_fallback,
            "spawn-group NPC type {tid} must resolve as real SDE category 11 entity"
        );
        assert!(
            profile.bounty_isk.is_finite()
                && profile.max_velocity.is_finite()
                && profile.cruise_velocity.is_finite()
                && profile.orbit_range.is_finite()
                && profile.total_nominal_dps.is_finite(),
            "non-finite profile values for type {tid}"
        );
        assert!(profile.max_velocity >= 0.0 && profile.orbit_range >= 0.0);
        match (profile.turret.is_some(), profile.missile.is_some()) {
            (true, false) => turret_only += 1,
            (false, true) => missile_only += 1,
            (true, true) => both_weapons += 1,
            (false, false) => unarmed += 1,
        }
        if profile.turret.is_some() || profile.missile.is_some() {
            assert!(
                profile.total_nominal_dps > 0.0,
                "armed NPC type {tid} must have positive SDE DPS"
            );
        }
        if !fallbacks.is_empty() {
            fallback_types += 1;
            for reason in fallbacks {
                *reason_counts.entry(reason).or_insert(0) += 1;
            }
        }
    }

    println!(
        "SDE spawn-group NPC audit (#1396): total={}, turret_only={}, missile_only={}, both={}, unarmed={}, types_with_fallbacks={}, reason_counts={:?}",
        all_type_ids.len(),
        turret_only,
        missile_only,
        both_weapons,
        unarmed,
        fallback_types,
        reason_counts
    );

    // Verify once-per-type_id deduplication in fallback_log_registry
    let custom_tid = 999_888_139_u32;
    {
        let mut custom_type = ctx.sde.types[&16_936].clone();
        custom_type.type_id = custom_tid;
        custom_type.name = "Synthetic Missing Attr Test NPC".to_string();
        custom_type.default_attributes.clear();
        custom_type.default_attributes.insert(eve_sde::attr_ids::EM_DAMAGE, 10.0);
        Arc::make_mut(&mut ctx.sde).types.insert(custom_tid, custom_type);
    }
    fallback_log_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&custom_tid);

    let p1 = resolve_npc_combat_profile(&ctx.sde, custom_tid, None, 500_012);
    assert!(p1.turret.is_some());
    assert!(
        fallback_log_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&custom_tid),
        "first fallback resolution must record type_id once"
    );
    let p2 = resolve_npc_combat_profile(&ctx.sde, custom_tid, None, 500_012);
    assert_eq!(p1.total_nominal_dps, p2.total_nominal_dps);
    assert!(
        !fallback_log_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(custom_tid),
        "subsequent resolution must remain deduplicated per type_id"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_vorton_uses_missile_drf_within_optimal_without_wrecking_or_tracking() {
    // (#1396 BF-02) Skybreaker Disparu Troop (56188): effect 8088, optimal 19,200m,
    // cycle 6s, damage 444 ((97 EM + 14 Kin) * 4.0 mult), er=43, ev=190, drf=0.6, no trackingSpeed(160).
    let base_ctx = test_ctx();
    let (profile, fallbacks) = inspect_npc_sde_profile(&base_ctx.sde, 56_188, None, 500_027);
    assert!(
        fallbacks.is_empty(),
        "Vorton NPC 56188 must resolve cleanly from SDE without fallbacks, got {fallbacks:?}"
    );
    let turret = profile.turret.as_ref().expect("Vorton profile");
    assert_eq!(turret.kind, NpcWeaponKind::Vorton);
    assert_eq!(turret.fx_guid, "effects.AttackMode");
    assert_eq!(turret.optimal_range, 19_200.0);
    let shot_dmg = turret.shot_damage.total();
    assert!((shot_dmg - 444.0).abs() < 1e-6);

    let run_vorton = |npc_tid: u32, dist_m: f64, vel: DVec3, sig: f64, n_ticks: u64, mutate: bool| async move {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        if mutate {
            let sde = Arc::make_mut(&mut ctx.sde);
            if let Some(t) = sde.types.get_mut(&npc_tid) {
                t.default_attributes.insert(eve_sde::attr_ids::FALLOFF, 5_000.0);
            }
        }
        let sg_id = 998_601_u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id: 500_027,
                ship_class: 1,
                group_id: 1982,
                group_name: "Vorton Test".to_string(),
                npc_type_ids: vec![npc_tid],
            },
        );
        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr.spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO).await.unwrap();
        let npc_bid = spawned[0];
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;
        let initial_shield = 1e6_f64;
        let target_bid = 900_600_001_i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                587,
                npc_pos + DVec3::new(dist_m, 0.0, 0.0),
                vel,
                37.0,
            ),
            make_sponge_combat_state(initial_shield, sig),
        );
        for tick in 1..=n_ticks {
            mgr.step_ai_sync(&ctx, sys_id, tick, 1.0);
        }
        let remaining = sys_handle
            .ballpark
            .read()
            .unwrap()
            .combat_states[&target_bid]
            .shield_hp;
        initial_shield - remaining
    };

    // 1. Over 600 ticks (100 shots at 6s cycle), zero 3x wrecking hits: exact 100 * 444 = 44,400.
    let dmg_large_600s = run_vorton(56_188, 10_000.0, DVec3::ZERO, 500.0, 600, false).await;
    assert!(
        (dmg_large_600s - 100.0 * shot_dmg).abs() < 1e-6,
        "Vorton must apply exact deterministic DRF (no 3x wrecking variance), got {dmg_large_600s}"
    );

    // 2. High angular velocity at close range (500m, 100 m/s -> omega=0.2 rad/s, large 500m sig):
    // Vorton ignores turret angular-velocity tracking misses and applies full DRF=1.0 damage.
    let dmg_high_omega = run_vorton(56_188, 500.0, DVec3::new(0.0, 100.0, 0.0), 500.0, 1, false).await;
    assert!(
        (dmg_high_omega - shot_dmg).abs() < 1e-6,
        "Vorton must ignore turret angular velocity tracking, got {dmg_high_omega}"
    );

    // 3. DRF mitigation for small slow (40m, 0 m/s) and small fast (40m, 400 m/s) targets.
    let dmg_small_slow = run_vorton(56_188, 5_000.0, DVec3::ZERO, 40.0, 1, false).await;
    let dmg_small_fast = run_vorton(56_188, 5_000.0, DVec3::new(0.0, 400.0, 0.0), 40.0, 1, false).await;
    let exp_small_slow = shot_dmg * crate::formulas::missile_damage_factor(40.0, 0.0, 43.0, 190.0, 0.6);
    let exp_small_fast = shot_dmg * crate::formulas::missile_damage_factor(40.0, 400.0, 43.0, 190.0, 0.6);
    assert!((dmg_small_slow - exp_small_slow).abs() < 1e-6);
    assert!((dmg_small_fast - exp_small_fast).abs() < 1e-6);
    assert!(dmg_small_fast < dmg_small_slow);

    // 4. Beyond optimal range (19,500m > 19,200m), even with non-zero falloff, Vorton deals 0 damage.
    let dmg_beyond_opt = run_vorton(56_188, 19_500.0, DVec3::ZERO, 500.0, 20, true).await;
    assert_eq!(dmg_beyond_opt, 0.0, "Vorton must not hit beyond optimal range");

    // 5. SDE sde.effects.guid metadata takes precedence over hardcoded FX default.
    let mut custom_ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let custom_sg = 998_602_u32;
    {
        let sde = Arc::make_mut(&mut custom_ctx.sde);
        if let Some(eff) = sde.effects.get_mut(&8088) {
            eff.guid = Some("effects.CustomVortonArc,effects.Fallback".to_string());
        }
        sde.npc_spawn_groups.insert(
            custom_sg,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: custom_sg,
                faction_id: 500_027,
                ship_class: 1,
                group_id: 1982,
                group_name: "Custom FX Vorton".to_string(),
                npc_type_ids: vec![56_188],
            },
        );
    }
    let custom_prof = inspect_npc_sde_profile(&custom_ctx.sde, 56_188, None, 500_027).0;
    assert_eq!(
        custom_prof.turret.unwrap().fx_guid,
        "effects.CustomVortonArc"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_stationary_armed_npc_has_zero_orbit_range_and_no_speed_churn() {
    // (#1396 BF-03) Stationary armed NPC (1194 Sentry Gun, group 99, max_velocity=0.0):
    // must have orbit_range=0.0 and emit zero Follow/Orbit/Stop/SetSpeedFraction events.
    let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let (profile, _fallbacks) = inspect_npc_sde_profile(&ctx.sde, 1_194, None, 0);
    assert_eq!(profile.max_velocity, 0.0);
    assert_eq!(profile.cruise_velocity, 0.0);
    assert_eq!(profile.orbit_range, 0.0);
    assert!(profile.turret.is_some());

    let sys_id = 30_000_142_u32;
    let sg_id = 998_603_u32;
    Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
        sg_id,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: sg_id,
            faction_id: 0,
            ship_class: 1,
            group_id: 99,
            group_name: "Single Sentry Test".to_string(),
            npc_type_ids: vec![1_194],
        },
    );
    let mut mgr = NpcSpawnManager::new();
    let spawned = mgr.spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO).await.unwrap();
    let sentry_bid = spawned[0];
    assert_eq!(mgr.entities[&sentry_bid].orbit_range, 0.0);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let sentry_pos = {
        let bp = sys_handle.ballpark.read().unwrap();
        let b = &bp.balls[&sentry_bid];
        assert_eq!(b.max_velocity, 0.0);
        assert_eq!(b.speed_fraction, 0.0);
        b.position
    };

    // Step idle with no target -> Attack at 25km -> Attack at 4km -> Idle at 80km
    {
        sys_handle.ballpark.write().unwrap().pending_events.clear();
    }
    mgr.step_ai_sync(&ctx, sys_id, 1, 1.0);
    assert_eq!(mgr.entities[&sentry_bid].state, NpcAiState::Idle);

    let target_bid = 900_603_001_i64;
    sys_handle.try_spawn_ball_with_combat(
        make_target_ball(
            target_bid,
            587,
            sentry_pos + DVec3::new(25_000.0, 0.0, 0.0),
            DVec3::ZERO,
            37.0,
        ),
        make_sponge_combat_state(1e6, 400.0),
    );
    mgr.step_ai_sync(&ctx, sys_id, 2, 1.0);
    assert_eq!(mgr.entities[&sentry_bid].state, NpcAiState::Attack);

    sys_handle
        .set_ball_position(target_bid, sentry_pos + DVec3::new(4_000.0, 0.0, 0.0))
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 3, 1.0);
    assert_eq!(mgr.entities[&sentry_bid].state, NpcAiState::Attack);

    sys_handle
        .set_ball_position(target_bid, sentry_pos + DVec3::new(80_000.0, 0.0, 0.0))
        .await;
    mgr.step_ai_sync(&ctx, sys_id, 4, 1.0);
    assert_eq!(mgr.entities[&sentry_bid].state, NpcAiState::Idle);

    let bp = sys_handle.ballpark.read().unwrap();
    let movement_churn: Vec<_> = bp
        .pending_events
        .iter()
        .filter(|ev| match ev {
            eve_destiny::DestinyEvent::Follow { ball_id, .. }
            | eve_destiny::DestinyEvent::Orbit { ball_id, .. }
            | eve_destiny::DestinyEvent::Stop { ball_id }
            | eve_destiny::DestinyEvent::SetSpeedFraction { ball_id, .. } => *ball_id == sentry_bid,
            _ => false,
        })
        .collect();
    assert!(
        movement_churn.is_empty(),
        "stationary armed NPC must emit zero Follow/Orbit/Stop/SetSpeedFraction churn, got {movement_churn:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_ai_killing_blow_on_special_fx_retains_wire_meta_across_next_tick_encode() {
    // (#1396 BF-R2-02) When an NPC shot destroys its target, remove_ball_state_only prunes
    // bp.special_fx_meta immediately, yet next-tick encode_destiny_event for both pending_events
    // and observer_events must preserve full V24 OnSpecialFX metadata until emit_tick + 1 expires.
    let cases = [
        (16_936_u32, 500_012_u32, "effects.Laser", false),
        (16_984_u32, 500_010_u32, "effects.MissileDeployment", true),
    ];

    for (idx, &(npc_type_id, faction_id, expected_guid, is_missile)) in cases.iter().enumerate() {
        let mut ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let sys_id = 30_000_142_u32;
        let sg_id = 998_700 + idx as u32;
        Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
            sg_id,
            eve_sde::NpcSpawnGroupInfo {
                spawn_group_id: sg_id,
                faction_id,
                ship_class: 2,
                group_id: 550,
                group_name: format!("Lethal FX {npc_type_id}"),
                npc_type_ids: vec![npc_type_id],
            },
        );
        let mut mgr = NpcSpawnManager::new();
        let spawned = mgr.spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO).await.unwrap();
        let npc_bid = spawned[0];
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let npc_pos = sys_handle
            .get_ball_snapshot(npc_bid)
            .await
            .unwrap()
            .unwrap()
            .1
            .position;

        let observer_bid = 900_700_100 + idx as i64;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                observer_bid,
                587,
                npc_pos + DVec3::new(80_000.0, 0.0, 0.0),
                DVec3::ZERO,
                37.0,
            ),
            make_sponge_combat_state(1e6, 400.0),
        );
        let target_bid = 900_700_200 + idx as i64;
        let mut fragile_cs = make_sponge_combat_state(0.1, 400.0);
        fragile_cs.max_armor_hp = 0.1;
        fragile_cs.armor_hp = 0.1;
        fragile_cs.max_structure_hp = 0.1;
        fragile_cs.structure_hp = 0.1;
        sys_handle.try_spawn_ball_with_combat(
            make_target_ball(
                target_bid,
                587,
                npc_pos + DVec3::new(1_000.0, 0.0, 0.0),
                DVec3::ZERO,
                37.0,
            ),
            fragile_cs,
        );
        let emit_tick = {
            let mut bp = sys_handle.ballpark.write().unwrap();
            bp.register_observer(observer_bid);
            let _ = bp.drain_all_observer_events();
            bp.pending_events.clear();
            bp.current_tick
        };

        mgr.step_ai_sync(&ctx, sys_id, emit_tick, 1.0);

        {
            let bp = sys_handle.ballpark.read().unwrap();
            assert!(
                !bp.balls.contains_key(&target_bid),
                "target {target_bid} must be destroyed by lethal NPC shot ({npc_type_id})"
            );
            assert!(
                !bp.special_fx_meta
                    .values()
                    .any(|m| m.target_id == Some(target_bid)),
                "special_fx_meta must still be pruned when target is destroyed"
            );
        }

        // Next simulation tick (emit_tick + 1): matches cluster_tick.rs encoding tick N's queued FX on tick N + 1.
        let (pending_after_step, obs_after_step) = {
            let mut bp = sys_handle.ballpark.write().unwrap();
            let pending = bp.step(1.0);
            assert_eq!(bp.current_tick, emit_tick + 1);
            let obs = bp.drain_observer_events(observer_bid);
            (pending, obs)
        };

        let bp_guard = sys_handle.ballpark.read().unwrap();
        let stamp = bp_guard.current_tick as u32;
        for (source_label, ev_list) in [
            ("pending_events", &pending_after_step),
            ("observer_events", &obs_after_step),
        ] {
            let fx_ev = ev_list
                .iter()
                .find(|ev| matches!(
                    ev,
                    eve_destiny::DestinyEvent::OnSpecialFX { ball_id, guid, is_active: true, .. }
                        if *ball_id == npc_bid && guid == expected_guid
                ))
                .unwrap_or_else(|| panic!("{source_label} missing killing-blow OnSpecialFX for {npc_type_id}: {ev_list:?}"));
            let wire = eve_destiny::wire::encode_destiny_event(stamp, fx_ev, Some(&bp_guard));
            let eve_proto::PyRep::Tuple(top) = &wire else {
                panic!("expected DestinyUpdate tuple");
            };
            let eve_proto::PyRep::Tuple(act) = &top[1] else {
                panic!("expected action tuple");
            };
            assert_eq!(act[0], eve_proto::PyRep::String("OnSpecialFX".into()));
            let eve_proto::PyRep::Tuple(args) = &act[1] else {
                panic!("expected 14-arg OnSpecialFX tuple");
            };
            assert_eq!(args.len(), 14);
            assert_eq!(args[0], eve_proto::PyRep::Integer(npc_bid));
            assert_eq!(args[1], eve_proto::PyRep::None);
            assert_eq!(
                args[3],
                eve_proto::PyRep::Integer(target_bid),
                "{source_label} killing-blow OnSpecialFX lost targetID for {npc_type_id}"
            );
            if is_missile {
                assert_eq!(args[2], eve_proto::PyRep::None);
                assert!(matches!(args[4], eve_proto::PyRep::Integer(t) if t > 0));
            } else {
                assert!(matches!(args[2], eve_proto::PyRep::Integer(t) if t > 0));
                assert_eq!(args[4], eve_proto::PyRep::None);
            }
            assert_eq!(args[5], eve_proto::PyRep::String(expected_guid.into()));
            assert_eq!(args[6], eve_proto::PyRep::Bool(true));
            assert!(matches!(args[9], eve_proto::PyRep::Real(d) if d > 0.0));
            assert_eq!(args[10], eve_proto::PyRep::Integer(1));
        }
        let sample_fx = obs_after_step
            .iter()
            .find(|ev| matches!(ev, eve_destiny::DestinyEvent::OnSpecialFX { ball_id, .. } if *ball_id == npc_bid))
            .unwrap()
            .clone();
        drop(bp_guard);

        // Advancing to emit_tick + 2 must expire the deferred pruned FX metadata.
        {
            let mut bp = sys_handle.ballpark.write().unwrap();
            let _ = bp.step(1.0);
            assert_eq!(bp.current_tick, emit_tick + 2);
            let expired_wire = eve_destiny::wire::encode_destiny_event(
                bp.current_tick as u32,
                &sample_fx,
                Some(&bp),
            );
            let eve_proto::PyRep::Tuple(top) = &expired_wire else {
                panic!("expected DestinyUpdate tuple");
            };
            let eve_proto::PyRep::Tuple(act) = &top[1] else {
                panic!("expected action tuple");
            };
            let eve_proto::PyRep::Tuple(args) = &act[1] else {
                panic!("expected 14-arg tuple");
            };
            assert_eq!(args[3], eve_proto::PyRep::None, "deferred pruned FX metadata must expire at emit_tick + 2");
        }
    }
}

#[tokio::test]
async fn belt_rat_sec_tiers_clone_soldier_exclusion_and_decoupled_slot_cooldowns() {
    use std::collections::HashSet;

    let ctx = test_ctx();
    let find_sys = |pred: &dyn Fn(&eve_sde::SolarSystem) -> bool| -> u32 {
        let mut ids: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| {
                s.solar_system_id < 31_000_000
                    && pred(s)
                    && ctx.sde.celestials_in_system(s.solar_system_id).iter().filter(|c| c.group_id == 9).count() >= 3
                    && ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 10)
            })
            .map(|s| s.solar_system_id)
            .collect();
        ids.sort_unstable();
        ids[0]
    };
    let hs_sys = find_sys(&|s| {
        s.security_status >= 0.45 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_010
    });
    let ls_sys = find_sys(&|s| {
        (0.05..0.45).contains(&s.security_status)
            && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_010
    });
    let ns_sys = find_sys(&|s| {
        s.security_status < 0.0 && regional_pirate_faction_id(&ctx, s.solar_system_id) == 500_010
    });

    let no_cs = |sg: &eve_sde::NpcSpawnGroupInfo| {
        sg.npc_type_ids
            .iter()
            .all(|tid| !ctx.sde.types[tid].name.contains("Clone Soldier"))
    };

    // 1. High-sec: Frigate-only capped to 1 ship per belt and free of Clone Soldiers.
    let mut mgr = NpcSpawnManager::new();
    let (hs_plan, _) = mgr.plan_belt_rats_for_system(&ctx, hs_sys, false, None, true, true, true);
    assert_eq!(hs_plan.len(), 3);
    for (gid, sg, _) in &hs_plan {
        assert!(ctx.sde.npc_spawn_groups[gid].group_name.contains("Frigate"));
        assert_eq!(sg.npc_type_ids.len(), 1);
        assert!(no_cs(sg));
    }

    // 2. Synthetic Clone Soldier exclusion even when npc_type_ids.len() <= squad_limit, plus high-sec Frigate lock.
    let cs_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.name.contains("Clone Soldier"))
        .map(|(&tid, _)| tid)
        .expect("SDE Clone Soldier type");
    let normal_tid = 2_382u32; // Guristas Arrogator (Frigate)
    let cruiser_tid = 2_387u32; // Guristas Silencer (Cruiser)
    let mut syn_sde = (*ctx.sde).clone();
    syn_sde.npc_spawn_groups.clear();
    syn_sde.npc_spawn_groups.insert(
        900_001,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: 900_001,
            faction_id: 500_010,
            ship_class: 1,
            group_id: 562,
            group_name: "Asteroid Guristas Frigate".to_string(),
            npc_type_ids: vec![normal_tid],
        },
    );
    syn_sde.npc_spawn_groups.insert(
        900_002,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: 900_002,
            faction_id: 500_010,
            ship_class: 1,
            group_id: 579,
            group_name: "Asteroid Guristas Destroyer".to_string(),
            npc_type_ids: vec![cruiser_tid, cs_tid], // len == 2 <= low-sec squad_limit (2)
        },
    );
    let syn_ctx = ServerContext::with_sde_and_repository(
        Arc::new(syn_sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let mut syn_mgr = NpcSpawnManager::new();
    let (syn_hs, _) = syn_mgr.plan_belt_rats_for_system(&syn_ctx, hs_sys, false, None, true, true, true);
    assert_eq!(syn_hs.len(), 3);
    assert!(syn_hs.iter().all(|(gid, sg, _)| *gid == 900_001 && sg.npc_type_ids == vec![normal_tid]));
    let (syn_ls, _) = syn_mgr.plan_belt_rats_for_system(&syn_ctx, ls_sys, false, None, true, true, true);
    let dest_spawn = syn_ls.iter().find(|(gid, _, _)| *gid == 900_002).expect("Destroyer slot in low-sec");
    assert_eq!(dest_spawn.1.npc_type_ids, vec![cruiser_tid], "Clone Soldier must be retained out before truncation even when len <= limit");

    // 3. Low-sec & Null-sec: distinct groups across belts + <0.45 stargate reaching Cruiser / Battlecruiser / Battleship.
    let (ls_plan, _) = mgr.plan_belt_rats_for_system(&ctx, ls_sys, false, None, true, true, true);
    assert_eq!(ls_plan.len(), 4);
    let ls_gids: HashSet<u32> = ls_plan.iter().map(|(gid, _, _)| *gid).collect();
    assert_eq!(ls_gids.len(), 4, "low-sec belts and stargate must use distinct spawn groups");
    assert!(ls_plan.iter().any(|(gid, _, _)| {
        let n = &ctx.sde.npc_spawn_groups[gid].group_name;
        n.contains("Cruiser") && !n.contains("BattleCruiser")
    }));
    assert!(ls_plan.iter().any(|(gid, _, _)| ctx.sde.npc_spawn_groups[gid].group_name.contains("BattleCruiser")));
    assert_eq!(ls_plan[3].1.npc_type_ids.len(), 1);
    assert!(ls_plan.iter().all(|(_, sg, _)| no_cs(sg)));

    let (ns_plan, _) = mgr.plan_belt_rats_for_system(&ctx, ns_sys, false, None, true, true, true);
    assert_eq!(ns_plan.len(), 4);
    let ns_gids: HashSet<u32> = ns_plan.iter().map(|(gid, _, _)| *gid).collect();
    assert_eq!(ns_gids.len(), 4, "null-sec belts and stargate must use distinct spawn groups");
    assert!(ns_plan.iter().any(|(gid, _, _)| {
        let n = &ctx.sde.npc_spawn_groups[gid].group_name;
        n.contains("Cruiser") && !n.contains("BattleCruiser")
    }));
    assert!(ns_plan.iter().any(|(gid, _, _)| ctx.sde.npc_spawn_groups[gid].group_name.contains("BattleCruiser")));
    assert!(ns_plan.iter().any(|(gid, _, _)| ctx.sde.npc_spawn_groups[gid].group_name.contains("Battleship")));
    assert_eq!(ns_plan[3].1.npc_type_ids.len(), 1);
    assert!(ns_plan.iter().all(|(_, sg, _)| no_cs(sg)));

    // 4. Clearing Belt 1 must NOT put Belt 2, Belt 3, or the Stargate on a 20m cooldown (both shared-gid high-sec & multi-gid low-sec).
    let belt_pos_for = |sys_id: u32, idx: usize| {
        ctx.sde.celestials_in_system(sys_id).into_iter().filter(|c| c.group_id == 9).nth(idx).unwrap().position
    };
    let now = ctx.now_unix();
    let mut hs_mgr = NpcSpawnManager::new();
    let _ = hs_mgr.spawn_belt_rats_for_system(&ctx, hs_sys).await.unwrap();
    let hs_b0 = belt_pos_for(hs_sys, 0);
    let hs_b0_ids: Vec<i64> = hs_mgr
        .entities
        .values()
        .filter(|e| e.spawn_pos.distance(hs_b0) < 25_000.0)
        .map(|e| e.ball_id)
        .collect();
    assert!(!hs_b0_ids.is_empty());
    hs_mgr.entities.retain(|id, _| hs_b0_ids.contains(id));
    for id in hs_b0_ids {
        hs_mgr.remove_npc_and_record_cooldown(&ctx, hs_sys, id, now);
    }
    let (hs_after, _) = hs_mgr.plan_belt_rats_for_system(&ctx, hs_sys, false, None, true, true, true);
    assert_eq!(hs_after.len(), 2, "only Belt 1 should be on cooldown in high-sec");
    assert!(hs_after.iter().all(|(_, _, pos)| pos.distance(hs_b0) >= 25_000.0));

    let mut ls_mgr = NpcSpawnManager::new();
    let _ = ls_mgr.spawn_belt_rats_for_system(&ctx, ls_sys).await.unwrap();
    let ls_b0 = belt_pos_for(ls_sys, 0);
    let ls_b0_ids: Vec<i64> = ls_mgr
        .entities
        .values()
        .filter(|e| e.spawn_pos.distance(ls_b0) < 25_000.0)
        .map(|e| e.ball_id)
        .collect();
    assert!(!ls_b0_ids.is_empty());
    ls_mgr.entities.retain(|id, _| ls_b0_ids.contains(id));
    for id in ls_b0_ids {
        ls_mgr.remove_npc_and_record_cooldown(&ctx, ls_sys, id, now);
    }
    let (ls_after, _) = ls_mgr.plan_belt_rats_for_system(&ctx, ls_sys, false, None, true, true, true);
    assert_eq!(ls_after.len(), 3, "only Belt 1 should be on cooldown in low-sec; Belts 2, 3, and gate stay eligible");
    assert!(ls_after.iter().all(|(_, _, pos)| pos.distance(ls_b0) >= 25_000.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmb51_npc_spawn_ai_step_via_actor_mailbox_applies_damage_and_skips_non_free_balls() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sg_id = 999_142_u32;
    Arc::make_mut(&mut ctx.sde).npc_spawn_groups.insert(
        sg_id,
        eve_sde::NpcSpawnGroupInfo {
            spawn_group_id: sg_id,
            faction_id: 500_012,
            ship_class: 1,
            group_id: 550,
            group_name: "Corpii Test Group".to_string(),
            npc_type_ids: vec![16_936],
        },
    );

    let mut mgr = NpcSpawnManager::new();
    let spawned = mgr
        .spawn_group(&ctx, sys_id, sg_id, DVec3::ZERO)
        .await
        .expect("spawn_group");
    let npc_bid = spawned[0];
    ctx.npc_spawns.write().unwrap().insert(sys_id, mgr);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let npc_pos = sys_handle.view().ball_position(npc_bid).expect("npc position on view");

    // Pinned non-free ball (even with >0 max_velocity) must appear on SystemView::non_free_ball_ids and be skipped by AI targeting.
    let pinned_id = 900_901_001_i64;
    sys_handle
        .spawn_ball_with_meta(
            make_target_ball(pinned_id, 587, npc_pos + DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, 40.0),
            make_sponge_combat_state(10_000.0, 40.0),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.mark_ball_non_free(pinned_id, false);
    }
    assert!(sys_handle.view().is_non_free_ball(pinned_id));

    let target_id = 900_901_002_i64;
    sys_handle
        .spawn_ball_with_meta(
            make_target_ball(target_id, 587, npc_pos + DVec3::new(800.0, 0.0, 0.0), DVec3::ZERO, 40.0),
            make_sponge_combat_state(10_000.0, 1_000.0),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_002,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for tick in 1..=5 {
        step_system_npc_spawns_ai_sync(&ctx, sys_id, tick, 1.0);
    }

    let view = sys_handle.view();
    assert!(
        (view.combat_state(pinned_id).unwrap().shield_hp - 10_000.0).abs() < 1e-6,
        "non-free ball must not be targeted or damaged by NPC AI"
    );
    assert!(
        view.combat_state(target_id).unwrap().shield_hp < 10_000.0,
        "free target ball must take NPC AI damage via ApplyNpcAiStepEffects actor command"
    );
}
