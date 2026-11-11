//! Comprehensive unit tests covering edge cases for `services::pve_special_sites_ops`.

use super::*;
use crate::router::ServerContext;
use eve_db::{TrainedSkillRecord, WorldRepository};
use eve_sde::{DungeonTemplate, SdeRegistry, SolarSystem};
use std::collections::HashMap;
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
fn resolve_homefront_templates_and_faction_boss_types_cover_all_factions() {
    let mut ctx = test_ctx();

    let templates = resolve_sde_homefront_templates(&ctx.sde);
    // Verify sorted order by dungeon_id
    for win in templates.windows(2) {
        assert!(win[0].dungeon_id <= win[1].dungeon_id);
    }
    // Also verify synthetic archetype 70 dungeon fallback to d.name
    let mut sde_clone = (*ctx.sde).clone();
    sde_clone.dungeons.insert(
        9_999_001,
        DungeonTemplate {
            dungeon_id: 9_999_001,
            name: "Synthetic Homefront Op".to_string(),
            archetype_id: Some(70),
            difficulty: None,
            faction_id: Some(500_001),
        },
    );
    let templates2 = resolve_sde_homefront_templates(&sde_clone);
    let synth = templates2
        .iter()
        .find(|t| t.dungeon_id == 9_999_001)
        .expect("synthetic archetype 70 template");
    assert_eq!(synth.dungeon_name, "Synthetic Homefront Op");
    assert_eq!(synth.difficulty, 2);

    // Unknown system falls back to Caldari (500_001 -> Guristas 500_010)
    let (b1_unk, b2_unk) = resolve_homefront_faction_boss_types(&ctx.sde, 999_999_999);
    assert!(b1_unk > 0 && b2_unk > 0);

    // Exercise all 5 faction branches via synthetic solar systems + constellations/regions
    let template_sys = ctx.sde.solar_systems[&30_000_142].clone();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let template_type = sde_mut.types[&587].clone();
        // Ensure each hostile faction has at least 2 candidate cruisers in group 550..=590 with shield cap > 0
        for (idx, &hostile_fac) in [500_012u32, 500_020, 500_011, 500_010, 500_019]
            .iter()
            .enumerate()
        {
            for sub in 0..2u32 {
                let tid = 998_100 + (idx as u32) * 10 + sub;
                let mut t = template_type.clone();
                t.type_id = tid;
                t.published = true;
                t.faction_id = Some(hostile_fac);
                t.group_id = 560;
                t.default_attributes
                    .insert(eve_sde::attr_ids::SHIELD_CAPACITY, 5_000.0);
                sde_mut.types.insert(tid, t);
            }
        }
        // Create 5 systems with constellation/region/station fallback
        for (idx, &sys_fac) in [500_003u32, 500_004, 500_002, 500_001, 500_007]
            .iter()
            .enumerate()
        {
            let sid = 39_800_000 + idx as u32;
            let cid = 29_800_000 + idx as u32;
            let rid = 19_800_000 + idx as u32;
            // For idx == 1 (Gallente), put faction on region instead of constellation
            let (cfac, rfac) = if idx == 1 {
                (None, Some(sys_fac))
            } else {
                (Some(sys_fac), None)
            };
            sde_mut.constellations.insert(
                cid,
                eve_sde::ConstellationInfo {
                    constellation_id: cid,
                    region_id: rid,
                    name: format!("HF-C{idx}"),
                    center: [0.0; 3],
                    faction_id: cfac,
                    wormhole_class_id: None,
                },
            );
            sde_mut.regions.insert(
                rid,
                eve_sde::RegionInfo {
                    region_id: rid,
                    name: format!("HF-R{idx}"),
                    center: [0.0; 3],
                    faction_id: rfac,
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

    for idx in 0..5u32 {
        let (b1, b2) = resolve_homefront_faction_boss_types(&ctx.sde, 39_800_000 + idx);
        assert!(b1 > 0 && b2 > 0);
    }

    // Empty cruiser candidates fallback -> (48_090, 48_090)
    let mut empty_sde = SdeRegistry::default();
    empty_sde.solar_systems.insert(30_000_142, template_sys);
    assert_eq!(
        resolve_homefront_faction_boss_types(&empty_sde, 30_000_142),
        (48_090, 48_090)
    );
}

#[test]
fn compute_homefront_base_and_combat_pilot_isk_all_tiers_and_edge_cases() {
    let mut sde = SdeRegistry::default();
    sde.solar_systems.insert(
        30_000_100,
        SolarSystem {
            solar_system_id: 30_000_100,
            constellation_id: 20_000_001,
            region_id: 10_000_001,
            name: "High10".into(),
            security_status: 1.0,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_000_101,
        SolarSystem {
            solar_system_id: 30_000_101,
            constellation_id: 20_000_001,
            region_id: 10_000_001,
            name: "LowClamp".into(),
            security_status: 0.1, // clamped to 0.45
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );

    // Difficulty 0, 1, 2, 3, 4+ and even vs odd slot_index
    let d0_even = compute_homefront_base_pilot_isk(&sde, 30_000_100, 0, 0);
    let d1_even = compute_homefront_base_pilot_isk(&sde, 30_000_100, 1, 2);
    assert_eq!(d0_even, 15_500_000);
    assert_eq!(d1_even, 15_500_000);

    let d2_odd = compute_homefront_base_pilot_isk(&sde, 30_000_100, 2, 1);
    assert_eq!(d2_odd, 15_510_000); // 17_230_000 * 0.90 = 15_507_000 -> rounded to 10k = 15_510_000

    let d3_even = compute_homefront_base_pilot_isk(&sde, 30_000_100, 3, 0);
    assert_eq!(d3_even, 18_500_000);

    let d4_even = compute_homefront_base_pilot_isk(&sde, 30_000_100, 4, 0);
    assert_eq!(d4_even, 20_000_000);

    // Clamped sec 0.45 -> sec_mult = 1 + 0.55 * 0.25 = 1.1375
    let low_d4 = compute_homefront_base_pilot_isk(&sde, 30_000_101, 5, 0);
    assert_eq!(low_d4, 22_750_000);

    // Missing system -> default 0.5 security
    let unk_d2 = compute_homefront_base_pilot_isk(&sde, 999_999, 2, 0);
    assert!(unk_d2 > 17_230_000);

    // Combat pilot ISK split
    assert_eq!(compute_homefront_combat_pilot_isk(17_000_000, 5, 0), 17_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(17_000_000, 5, 5), 17_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(17_000_000, 5, 10), 8_500_000);
    assert_eq!(compute_homefront_combat_pilot_isk(10_000_000, 0, 4), 2_500_000);
}

#[test]
fn drifter_hives_and_bonus_loot_and_triglavian_sites_and_standings() {
    let ctx = test_ctx();
    let mut reg = PveSpecialSitesRegistry::default();
    init_drifter_hives_from_sde(&ctx.sde, &mut reg);
    assert_eq!(reg.drifter_hives.len(), 5);
    for sys_id in 31_000_001..=31_000_005 {
        let hive = reg.drifter_hives.get(&sys_id).expect("drifter hive");
        assert_eq!(hive.rooms.len(), 3);
        assert_eq!(hive.total_rooms, 3);
        assert_eq!(hive.current_room, 1);
    }
    // Calling a second time is idempotent
    init_drifter_hives_from_sde(&ctx.sde, &mut reg);
    assert_eq!(reg.drifter_hives.len(), 5);

    // Bonus loot allocates 3 distinct IDs
    let loot1 = allocate_drifter_hive_bonus_loot(&mut reg, 34_557);
    let loot2 = allocate_drifter_hive_bonus_loot(&mut reg, 34_558);
    assert_eq!(loot1.len(), 3);
    assert_eq!(loot1[0].1, 34_557);
    assert_eq!(loot1[0].2, 1);
    assert_eq!(loot1[1].1, 34_575);
    assert_eq!(loot1[1].2, 2);
    assert_eq!(loot1[2].1, 30_745);
    assert_eq!(loot1[2].2, 5);
    assert_ne!(loot1[0].0, loot2[0].0);

    // Build Triglavian / EDENCOM sites across all 3 branches
    let edencom_sites = build_triglavian_sites_from_sde(&ctx.sde, 30_000_142, true, (1, 2, 3));
    assert_eq!(edencom_sites.len(), 2);
    assert_eq!(edencom_sites[0].faction_owner_id, 500_027);
    assert_eq!(edencom_sites[0].boss_type_id, 1);
    assert_eq!(edencom_sites[0].escort_type_ids, vec![2, 3]);

    let kspace_trig = build_triglavian_sites_from_sde(&ctx.sde, 30_000_142, false, (1, 2, 3));
    assert_eq!(kspace_trig.len(), 2);
    assert_eq!(kspace_trig[0].faction_owner_id, 500_026);
    assert_eq!(kspace_trig[0].base_isk, 35_000_000);

    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id);
    if let Some(psid) = pochven_sys {
        let pochven_sites = build_triglavian_sites_from_sde(&ctx.sde, psid, false, (1, 2, 3));
        assert_eq!(pochven_sites.len(), 2);
        assert_eq!(pochven_sites[0].base_isk, 40_000_000);
        assert_eq!(pochven_sites[0].survey_db_drop, 200);
    }

    // apply_triglavian_standings_and_isk: missing char -> (None, None)
    ctx.db.with_mut_snapshot(|snap| {
        let (bal_none, st_none) =
            apply_triglavian_standings_and_isk(snap, 999_999_999, 500_026, 500_027, 1_000_000, "Test");
        assert!(bal_none.is_none());
        assert!(st_none.is_none());

        // With Social skill level 5 (3355)
        let cid = 90_000_001;
        let bal_before = snap.characters[&cid].balance.raw_cents() / 100;
        snap.skills.entry(cid).or_default().insert(
            3355,
            TrainedSkillRecord {
                type_id: 3355,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let (bal_after, pair) = apply_triglavian_standings_and_isk(
            snap,
            cid,
            500_026,
            500_027,
            5_000_000,
            "Conduit",
        );
        assert!(bal_after.is_some());
        let (g, l) = pair.expect("standing pair");
        // gain: 0 + 10 * 0.015 * 1.25 = 0.1875; loss: 0 - 10 * 0.015 = -0.15
        assert!((g - 0.1875).abs() < 1e-9);
        assert!((l + 0.15).abs() < 1e-9);
        assert_eq!(snap.characters[&cid].balance.raw_cents() / 100, bal_before + 5_000_000);
    });
}

#[test]
fn roaming_storms_and_storm_dogma_notifications() {
    let ctx = test_ctx();
    let mut reg = PveSpecialSitesRegistry::default();
    reg.sim_clock_sec = 10_000;
    ensure_full_roaming_storm_set(&ctx, &mut reg);
    assert!(reg.roaming_storms.len() >= 8);
    let len_after_full = reg.roaming_storms.len();
    // Second call returns early
    ensure_full_roaming_storm_set(&ctx, &mut reg);
    assert_eq!(reg.roaming_storms.len(), len_after_full);

    // Partial storm set (< 8) tops back up to 8
    reg.roaming_storms.truncate(3);
    ensure_full_roaming_storm_set(&ctx, &mut reg);
    assert_eq!(reg.roaming_storms.len(), 8);

    // notify_storm_ship_dogma_updates: empty affected_systems -> no-op
    let mut effect_map: HashMap<SolarSystemId, Vec<TypeId>> = HashMap::new();
    notify_storm_ship_dogma_updates(&ctx, &[], &effect_map);

    // Put two pilots in space in 30_000_142 and 30_000_144, and one docked in 30_000_142
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = 100_900_001;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = 100_900_002;
        }
    });
    let (weak_gamma, strong_gamma) = storm_weather_beacons(AbyssalWeatherKind::Gamma);
    effect_map.insert(30_000_142, vec![strong_gamma]);
    effect_map.insert(30_000_144, vec![weak_gamma, weak_gamma]);
    // Also include a system with no effects in map (storm_count == 0 -> strength_mult 1.0)
    notify_storm_ship_dogma_updates(&ctx, &[30_000_142, 30_000_144], &effect_map);
    effect_map.clear();
    notify_storm_ship_dogma_updates(&ctx, &[30_000_142], &effect_map);
}
