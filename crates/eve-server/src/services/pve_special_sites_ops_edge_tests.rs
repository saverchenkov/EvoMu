//! Comprehensive unit tests for `pve_special_sites_ops` helpers.

#![forbid(unsafe_code)]

use super::*;
use crate::services::pve_metaliminal_storms::storm_weather_beacons;
use crate::services::pve_special_sites_ops::{
    allocate_drifter_hive_bonus_loot, apply_triglavian_standings_and_isk,
    build_triglavian_sites_from_sde, compute_homefront_combat_pilot_isk,
    compute_homefront_fleet_payouts, ensure_full_roaming_storm_set,
    init_drifter_hives_from_sde, is_homefront_exordium_system,
    notify_storm_ship_dogma_updates, resolve_homefront_archetype_blueprint,
    resolve_sde_homefront_templates, resolve_sde_homefront_templates_for_system,
    validate_homefront_ship_eligibility, HomefrontArchetype,
    HomefrontParticipantContribution,
};
use eve_db::TrainedSkillRecord;
use std::collections::{HashMap, HashSet};

#[test]
fn homefront_archetypes_ship_restrictions_and_role_cap_payouts() {
    let sde = shared_sde();

    let templates = resolve_sde_homefront_templates(&sde);
    assert!(templates.len() >= 24);
    assert!(templates.windows(2).all(|w| w[0].dungeon_id <= w[1].dungeon_id));

    let covered: HashSet<HomefrontArchetype> = templates
        .iter()
        .map(|t| HomefrontArchetype::from_dungeon(t.dungeon_id, &t.dungeon_name))
        .collect();
    for arch in [
        HomefrontArchetype::Raid,
        HomefrontArchetype::EmergencyAid,
        HomefrontArchetype::MetaliminalMeteoroid,
        HomefrontArchetype::DreadAssault,
        HomefrontArchetype::AbyssalArtifactRecovery,
        HomefrontArchetype::SuspiciousSignal,
        HomefrontArchetype::SalvageResearch,
        HomefrontArchetype::StabilizeRift,
        HomefrontArchetype::TrafficStop,
    ] {
        assert!(covered.contains(&arch), "Missing V24 SDE archetype {arch:?}");
    }

    // Verify real SDE multi-role blueprints across regional hubs (no cruiser stub 48_090)
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let jita_templates = resolve_sde_homefront_templates_for_system(&sde, jita);
    assert!(jita_templates.len() >= 6);
    for (did, sys_id, exp_isk, exp_opt) in [
        (10_347_u32, amarr, 15_000_000_i64, 5_usize), // Raid: Hall of Sacrifice
        (10_373, jita, 15_000_000, 5),                // Emergency Aid: Convoy
        (10_387, jita, 12_000_000, 5),                // Metaliminal Meteoroid: Caldari Mining
        (10_320, amarr, 15_000_000, 5),               // Dread Assault: Blood Raider Temple
        (10_346, jita, 67_500_000, 5),                // Abyssal Artifact Recovery
        (10_367, jita, 15_000_000, 5),                // Suspicious Signal: Block the Broadcast
        (10_713, jita, 12_000_000, 3),                // 3-pilot Salvage Research
    ] {
        let tpl = templates.iter().find(|t| t.dungeon_id == did).unwrap();
        let bp = resolve_homefront_archetype_blueprint(
            &sde,
            tpl.dungeon_id,
            &tpl.dungeon_name,
            sys_id,
            tpl.difficulty,
        );
        assert_ne!(bp.dispatch_commander_type_id, 48_090);
        assert_eq!(bp.base_pilot_isk, exp_isk);
        assert_eq!(bp.optimal_fleet_size, exp_opt);
        assert_eq!(bp.max_payout_pilots, exp_opt * 2);
        let cmd_t = &sde.types[&bp.dispatch_commander_type_id];
        assert!(cmd_t.default_attributes.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied().unwrap_or(0.0) > 0.0);
        assert!(cmd_t.default_attributes.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(0.0) > 0.0);
        assert!(cmd_t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(0.0) > 0.0);
    }

    // Starter / Exordium system halves payout vs Jita
    assert!(!is_homefront_exordium_system(&sde, jita));
    assert!(is_homefront_exordium_system(&sde, 30_003_489)); // Imperial Academy (Chaven)
    let ex_bp = resolve_homefront_archetype_blueprint(
        &sde,
        10_309,
        "Suspicious Signal: Secure the Intel",
        30_003_489,
        2,
    );
    assert_eq!(ex_bp.base_pilot_isk, 7_500_000);

    // Acceleration-gate ship restriction enforcement (5-pilot vs 3-pilot)
    for &ok_5p in &[597_u32, 32_880, 89_648, 16_236, 89_240, 91_174, 621, 81_008, 2_998] {
        assert!(
            validate_homefront_ship_eligibility(&sde, ok_5p, HomefrontArchetype::Raid).is_ok(),
            "Expected ship {ok_5p} allowed in 5-pilot Homefront"
        );
    }
    for &bad_5p in &[670_u32, 11_134, 11_993, 17_709, 17_715, 47_270, 22_544, 24_698, 638, 28_606] {
        assert_eq!(
            validate_homefront_ship_eligibility(&sde, bad_5p, HomefrontArchetype::DreadAssault)
                .expect_err("disallowed hull must fail gate")
                .code,
            "ShipRestrictedByGate"
        );
    }
    assert!(validate_homefront_ship_eligibility(&sde, 32_880, HomefrontArchetype::TrafficStop).is_ok());
    assert!(validate_homefront_ship_eligibility(&sde, 621, HomefrontArchetype::TrafficStop).is_err());
    assert!(validate_homefront_ship_eligibility(&sde, 73_789, HomefrontArchetype::TrafficStop).is_err());

    // Canonical 5-pilot role cap payout scaling (no infinite/linear-over-cap ISK/LP multiplication)
    let contribs: Vec<HomefrontParticipantContribution> = (1..=12)
        .map(|cid| HomefrontParticipantContribution {
            character_id: cid,
            damage_dealt: if cid == 12 { 500.0 } else { 2_500.0 },
            ..Default::default()
        })
        .collect();
    let p5 = compute_homefront_fleet_payouts(15_000_000, 100, 5, &contribs[..5]);
    assert_eq!(p5.eligible_pilot_count, 5);
    assert_eq!(p5.total_fleet_isk, 75_000_000);
    assert_eq!(p5.total_fleet_lp, 500);

    let p10 = compute_homefront_fleet_payouts(15_000_000, 100, 5, &contribs[..10]);
    assert_eq!(p10.eligible_pilot_count, 10);
    assert_eq!(p10.total_fleet_isk, 75_000_000);
    assert_eq!(p10.payouts[0].isk, 7_500_000);
    assert_eq!(p10.payouts[0].lp, 50);

    // 11 qualified pilots (plus pilot 12 below interaction threshold) -> max 10 paid, pilot 11 & 12 earn 0
    let p12 = compute_homefront_fleet_payouts(15_000_000, 100, 5, &contribs);
    assert_eq!(p12.eligible_pilot_count, 10);
    assert_eq!(p12.total_fleet_isk, 75_000_000);
    assert!(!p12.payouts[10].eligible && p12.payouts[10].isk == 0 && p12.payouts[10].lp == 0);
    assert!(!p12.payouts[11].eligible && p12.payouts[11].isk == 0);

    // Combat helper preserves capped fleet pool
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 0), 15_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 5), 15_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 10), 7_500_000);
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 15), 5_000_000);
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
