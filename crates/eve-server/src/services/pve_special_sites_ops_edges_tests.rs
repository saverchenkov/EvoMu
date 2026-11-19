//! Comprehensive unit tests covering edge cases for `services::pve_special_sites_ops`.

use super::*;
use crate::router::ServerContext;
use eve_db::{TrainedSkillRecord, WorldRepository};
use eve_sde::{DungeonTemplate, SdeRegistry};
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
fn resolve_homefront_archetype_blueprints_cover_all_factions_and_stripped_fallbacks() {
    let ctx = test_ctx();

    let templates = resolve_sde_homefront_templates(&ctx.sde);
    for win in templates.windows(2) {
        assert!(win[0].dungeon_id <= win[1].dungeon_id);
    }

    // Synthetic archetype 70 dungeon fallback uses dungeon.name when operations.json lacks it
    let mut sde_clone = (*ctx.sde).clone();
    sde_clone.dungeons.insert(
        9_999_001,
        DungeonTemplate {
            dungeon_id: 9_999_001,
            name: "Synthetic Dread Assault Op".to_string(),
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
    assert_eq!(synth.dungeon_name, "Synthetic Dread Assault Op");
    assert_eq!(synth.difficulty, 2);

    let synth_bp = resolve_homefront_archetype_blueprint(
        &sde_clone,
        synth.dungeon_id,
        &synth.dungeon_name,
        30_000_142,
        synth.difficulty,
    );
    assert_eq!(synth_bp.archetype, HomefrontArchetype::DreadAssault);
    assert_eq!(synth_bp.objective_structure_type_id, Some(77_251));
    assert_eq!(synth_bp.support_entity_type_id, Some(77_184));

    // Regional filtering returns empire-appropriate + Abyssal 5-pilot sites across all 4 empires
    for &(sys_id, exp_sov) in &[
        (30_002_187_u32, 500_003_u32), // Amarr
        (30_000_142, 500_001),         // Caldari
        (30_002_659, 500_004),         // Gallente
        (30_002_510, 500_002),         // Minmatar
    ] {
        assert_eq!(resolve_homefront_sovereign_faction(&ctx.sde, sys_id), exp_sov);
        let regional = resolve_sde_homefront_templates_for_system(&ctx.sde, sys_id);
        assert!(regional.len() >= 6);
    }

    // Stripped empty SdeRegistry falls back gracefully without 48_090 cruiser stub
    let empty_sde = SdeRegistry::default();
    let empty_bp = resolve_homefront_archetype_blueprint(
        &empty_sde,
        10_320,
        "Dread Assault: Blood Raider Temple",
        30_000_142,
        2,
    );
    assert_ne!(empty_bp.dispatch_commander_type_id, 48_090);
    assert_eq!(empty_bp.base_pilot_isk, 15_000_000);
}

#[test]
fn homefront_wave_exordium_and_3_vs_5_pilot_cap_edge_cases() {
    let ctx = test_ctx();
    let sde = Arc::clone(&ctx.sde);

    // Abyssal Artifact Recovery 9-wave totals: 3*5M + 3*7.5M + 3*10M = 67.5M (halved in Exordium)
    let aar_bp = resolve_homefront_archetype_blueprint(
        &sde,
        10_346,
        "Abyssal Artifact Recovery",
        30_000_142,
        3,
    );
    assert_eq!(aar_bp.abyssal_wave_payouts_isk.len(), 9);
    assert_eq!(aar_bp.abyssal_wave_payouts_isk.iter().sum::<i64>(), 67_500_000);
    assert_eq!(aar_bp.base_pilot_isk, 67_500_000);

    let ex_aar = resolve_homefront_archetype_blueprint(
        &sde,
        10_346,
        "Abyssal Artifact Recovery",
        30_001_407, // Todaki starter system
        3,
    );
    assert!(ex_aar.is_exordium_system);
    assert_eq!(ex_aar.abyssal_wave_payouts_isk.iter().sum::<i64>(), 33_750_000);

    // 3-pilot role cap (`opt = 3, max = 6`): <= 3 gets 12M each (36M), 6 get 6M (36M), 7th gets 0
    let p3_contribs: Vec<HomefrontParticipantContribution> = (1..=8)
        .map(|cid| HomefrontParticipantContribution {
            character_id: cid,
            ore_mined_units: if cid == 1 { 100 } else { 0 },
            arrays_hacked: if cid != 1 { 1 } else { 0 },
            ..Default::default()
        })
        .collect();
    let p3_cap = compute_homefront_fleet_payouts(12_000_000, 60, 3, &p3_contribs[..3]);
    assert_eq!(p3_cap.total_fleet_isk, 36_000_000);
    assert_eq!(p3_cap.total_fleet_lp, 180);

    let p3_over = compute_homefront_fleet_payouts(12_000_000, 60, 3, &p3_contribs);
    assert_eq!(p3_over.eligible_pilot_count, 6);
    assert_eq!(p3_over.total_fleet_isk, 36_000_000);
    assert_eq!(p3_over.total_fleet_lp, 180);
    assert!(!p3_over.payouts[6].eligible && p3_over.payouts[6].isk == 0);
    assert!(!p3_over.payouts[7].eligible && p3_over.payouts[7].isk == 0);

    // Combat pilot ISK split edges + zero clamp
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 0), 15_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 5), 15_000_000);
    assert_eq!(compute_homefront_combat_pilot_isk(15_000_000, 5, 10), 7_500_000);
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
