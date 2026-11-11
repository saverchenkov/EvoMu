//! Comprehensive unit tests for `pve_special_sites_ops` helpers.

#![forbid(unsafe_code)]

use super::*;
use crate::services::pve_metaliminal_storms::storm_weather_beacons;
use crate::services::pve_special_sites_ops::{
    allocate_drifter_hive_bonus_loot, apply_triglavian_standings_and_isk,
    build_triglavian_sites_from_sde, compute_homefront_base_pilot_isk,
    compute_homefront_combat_pilot_isk, ensure_full_roaming_storm_set,
    init_drifter_hives_from_sde, notify_storm_ship_dogma_updates,
    resolve_homefront_faction_boss_types, resolve_sde_homefront_templates,
};
use eve_db::TrainedSkillRecord;
use std::collections::{HashMap, HashSet};

#[test]
fn homefront_templates_factions_and_isk_payout_formulas() {
    let sde = shared_sde();

    let templates = resolve_sde_homefront_templates(&sde);
    assert!(!templates.is_empty());
    assert!(templates.windows(2).all(|w| w[0].dungeon_id <= w[1].dungeon_id));

    // Resolve faction bosses across all 4 empire factions + unknown fallback
    let mut tested_factions = HashSet::new();
    for sys in sde.solar_systems.values() {
        let fid = sde
            .constellations
            .get(&sys.constellation_id)
            .and_then(|c| c.faction_id)
            .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.faction_id));
        if let Some(f) = fid {
            if matches!(f, 500_001 | 500_002 | 500_003 | 500_004) && tested_factions.insert(f) {
                let (b1, b2) = resolve_homefront_faction_boss_types(&sde, sys.solar_system_id);
                assert!(b1 > 0 && b2 > 0);
            }
        }
        if tested_factions.len() == 4 {
            break;
        }
    }
    let fallback_bosses = resolve_homefront_faction_boss_types(&sde, 99_999_999);
    assert!(fallback_bosses.0 > 0 && fallback_bosses.1 > 0);

    // Difficulty tiers (0/1, 2, 3, 4+) and slot_index even vs odd (0.90x)
    let sys_id = find_highsec_system(&sde);
    let d1_even = compute_homefront_base_pilot_isk(&sde, sys_id, 1, 0);
    let d1_odd = compute_homefront_base_pilot_isk(&sde, sys_id, 1, 1);
    let d2 = compute_homefront_base_pilot_isk(&sde, sys_id, 2, 0);
    let d3 = compute_homefront_base_pilot_isk(&sde, sys_id, 3, 0);
    let d4 = compute_homefront_base_pilot_isk(&sde, sys_id, 5, 0);
    assert!(d1_odd < d1_even);
    assert!(d1_even < d2 && d2 < d3 && d3 < d4);
    assert_eq!(d1_even % 10_000, 0);
    // Unknown system falls back to sec = 0.5 -> sec_mult = 1.125
    let unknown_d2 = compute_homefront_base_pilot_isk(&sde, 99_999_999, 2, 0);
    assert_eq!(unknown_d2, 19_380_000);

    // Combat fleet split: 0 clamped to 1, <= optimal is 1.0x, > optimal splits proportionally
    assert_eq!(compute_homefront_combat_pilot_isk(10_000_000, 5, 0), 10_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(10_000_000, 5, 5), 10_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(10_000_000, 5, 10), 5_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(10_000_000, 0, 4), 2_500_000);
}

#[test]
fn drifter_hives_storms_triglavian_sites_and_dogma_notifications() {
    let sde = shared_sde();
    let hs_sys = find_highsec_system(&sde);
    let (ctx, _, char_id, ship_id) = make_ctx_and_pilot(hs_sys, true);

    // 1. Drifter hives seeding & idempotency + bonus loot
    let mut reg = PveSpecialSitesRegistry::default();
    init_drifter_hives_from_sde(&sde, &mut reg);
    assert_eq!(reg.drifter_hives.len(), 5);
    assert!(reg.drifter_hives.values().all(|h| h.rooms.len() == 3 && !h.cleared));
    init_drifter_hives_from_sde(&sde, &mut reg);
    assert_eq!(reg.drifter_hives.len(), 5);

    let loot1 = allocate_drifter_hive_bonus_loot(&mut reg, 34_557);
    let loot2 = allocate_drifter_hive_bonus_loot(&mut reg, 34_558);
    assert_eq!(loot1.len(), 3);
    assert_eq!(loot1[0].1, 34_557);
    assert_eq!(loot1[1].1, 34_575);
    assert_eq!(loot1[2].1, 30_745);
    assert!(loot2[0].0 > loot1[0].0 && loot2[1].0 > loot1[1].0 && loot2[2].0 > loot1[2].0);

    // 2. Full roaming storm set (8 storms: 2 per weather type) + no-op when already >= 8
    ensure_full_roaming_storm_set(&ctx, &mut reg);
    assert_eq!(reg.roaming_storms.len(), 8);
    for w in [
        AbyssalWeatherKind::Gamma,
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Exotic,
    ] {
        assert_eq!(
            reg.roaming_storms.iter().filter(|s| s.weather == w).count(),
            2
        );
    }
    ensure_full_roaming_storm_set(&ctx, &mut reg);
    assert_eq!(reg.roaming_storms.len(), 8);

    // 3. notify_storm_ship_dogma_updates: empty early return, docked skipped, strong/weak/cleared scales
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_001, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock");
    let _ = ctx.drain_notifications_for_char(char_id);
    notify_storm_ship_dogma_updates(&ctx, &[], &HashMap::new());
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let (weak_b, strong_b) = storm_weather_beacons(AbyssalWeatherKind::Electrical);
    let mut eff_map = HashMap::new();
    eff_map.insert(hs_sys, vec![strong_b, weak_b]);
    notify_storm_ship_dogma_updates(&ctx, &[hs_sys], &eff_map);
    let n1 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(n1.len(), 1);
    assert_eq!(n1[0].event, "OnAttributes");
    assert!(format!("{:?}", n1[0].args).contains("2.5"), "strong 1.25 * 2 effects = 2.5");

    // Weak periphery (1.10 * 1)
    eff_map.insert(hs_sys, vec![weak_b]);
    notify_storm_ship_dogma_updates(&ctx, &[hs_sys], &eff_map);
    let n2 = ctx.drain_notifications_for_char(char_id);
    assert!(format!("{:?}", n2[0].args).contains("1.1"));

    // Cleared storm (0 effects -> scale 1.0)
    notify_storm_ship_dogma_updates(&ctx, &[hs_sys], &HashMap::new());
    let n3 = ctx.drain_notifications_for_char(char_id);
    assert!(format!("{:?}", n3[0].args).contains("1.0"));

    // Docked character is skipped
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    notify_storm_ship_dogma_updates(&ctx, &[hs_sys], &eff_map);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 4. apply_triglavian_standings_and_isk with Social level 5 & missing char_id
    let (bal_opt, pair_opt) = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3355,
            TrainedSkillRecord {
                type_id: 3355,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let res = apply_triglavian_standings_and_isk(
            snap,
            char_id,
            500_026,
            500_027,
            2_000_000,
            "Observatory Flashpoint",
        );
        let missing = apply_triglavian_standings_and_isk(
            snap,
            999_888_777,
            500_026,
            500_027,
            1_000_000,
            "Missing",
        );
        assert_eq!(missing, (None, None));
        assert!(snap
            .wallet_journal
            .iter()
            .any(|e| e.character_id == char_id && e.reason.contains("Observatory Flashpoint")));
        res
    });
    assert_eq!(bal_opt, Some(3_000_000.0));
    let (g, l) = pair_opt.unwrap();
    // Social 5 => mult = 1.25 => gain = 10 * 0.015 * 1.25 = 0.1875, lose = -0.15
    assert!((g - 0.1875).abs() < 1e-6);
    assert!((l - (-0.15)).abs() < 1e-6);
    let _ = ship_id;

    // 5. build_triglavian_sites_from_sde across EDENCOM, Pochven, and K-space
    let pochven_sys = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);
    let eden = build_triglavian_sites_from_sde(&sde, hs_sys, true, (111, 222, 333));
    assert_eq!(eden.len(), 2);
    assert_eq!(eden[0].faction_owner_id, 500_027);
    assert_eq!(eden[0].boss_type_id, 111);
    assert_eq!(eden[0].escort_type_ids, vec![222, 333]);

    let poch = build_triglavian_sites_from_sde(&sde, pochven_sys, false, (111, 222, 333));
    assert_eq!(poch.len(), 2);
    assert_eq!(poch[0].faction_owner_id, 500_026);
    if pochven_sys != 30_000_142 {
        assert_eq!(poch[0].base_isk, 40_000_000);
        assert_eq!(poch[0].survey_db_drop, 200);
    }

    let kspace = build_triglavian_sites_from_sde(&sde, hs_sys, false, (111, 222, 333));
    assert_eq!(kspace.len(), 2);
    assert_eq!(kspace[0].base_isk, 35_000_000);
    assert_eq!(kspace[0].survey_db_drop, 150);
}
