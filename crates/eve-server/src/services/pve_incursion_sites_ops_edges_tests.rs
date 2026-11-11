//! Behavioral edge-case unit tests for `services::pve_incursion_sites_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::pve_incursion_sites::{IncursionLPLogEntry, IncursionTaleRecord};
use crate::services::pve_special_sites::PveSpecialSitesRegistry;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};

fn make_sansha_sde(include_variants: bool) -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    sde.types.clear();

    let mut add_npc = |type_id: u32, group_id: u32, name: &str, published: bool| {
        sde.types.insert(
            type_id,
            InvType {
                type_id,
                group_id,
                category_id: 11,
                name: name.into(),
                mass: 1_000_000.0,
                volume: 1_000.0,
                capacity: 0.0,
                radius: 100.0,
                base_price: 0.0,
                published,
                portion_size: 1,
                market_group_id: None,
                race_id: None,
                faction_id: Some(500_019),
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
                default_attributes: Default::default(),
            },
        );
    };

    // Unpublished candidate should be ignored
    add_npc(2000, 570, "Unpublished Sansha BS", false);
    add_npc(2845, 570, "Sansha BS 0", true);
    if include_variants {
        add_npc(2846, 570, "Sansha BS 1", true);
        add_npc(2847, 570, "Sansha BS 2", true);
        add_npc(2936, 571, "Sansha CR 0", true);
        add_npc(2937, 571, "Sansha CR 1", true);
        add_npc(2909, 572, "Sansha FR 0", true);
        add_npc(2910, 572, "Sansha FR 1", true);
        add_npc(2911, 572, "Sansha FR 2", true);
        add_npc(2192, 570, "Kundalini Manifest", true);
        add_npc(3492, 571, "Vanguard Boss", true);
    }
    sde
}

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
        _ => None,
    }
}

#[test]
fn allocate_persistent_incursion_tale_id_handles_normal_and_collision_paths() {
    let mut reg = PveSpecialSitesRegistry::default();
    let id1 = allocate_persistent_incursion_tale_id(&mut reg, 20_000_001);
    assert!(id1 > 810_000);

    // Force collision on the next seq (id1 + 1)
    let expected_next = id1 + 1;
    reg.incursions.insert(
        20_000_001,
        IncursionTaleRecord {
            tale_id: expected_next,
            constellation_id: 20_000_001,
            constellation_name: "Kimotoro".into(),
            staging_solar_system_id: 30_000_142,
            vanguard_solar_system_ids: vec![],
            assault_solar_system_ids: vec![],
            hq_solar_system_id: 30_000_142,
            influence: 0.5,
            state: "Mobilizing".into(),
            mothership_spawned: false,
            mothership_defeated: false,
            sites: vec![],
            lockout_until_filetime: None,
            tale_stop_published: false,
        },
    );
    let id2 = allocate_persistent_incursion_tale_id(&mut reg, 20_000_002);
    assert_eq!(id2, expected_next + 20_000_002_i64 * 10);
}

#[test]
fn resolve_sansha_encounter_covers_empty_full_variants_and_fallback_tiers() {
    // 1. Empty SDE -> None
    let mut empty_sde = SdeRegistry::default();
    empty_sde.types.clear();
    assert!(resolve_sansha_encounter_from_sde(&empty_sde, 500, 5165, "Vanguard").is_none());

    // 2. Full Sansha SDE across all tiers
    let full = make_sansha_sde(true);
    let (ms_boss, ms_esc) =
        resolve_sansha_encounter_from_sde(&full, 100, 6000, "Mothership").unwrap();
    assert_eq!(ms_boss, 2192);
    assert_eq!(ms_esc.len(), 9);
    assert_eq!(ms_esc[0].npc_ball_id, 1001);
    assert!(!ms_esc[0].destroyed);

    let (boss_boss, boss_esc) =
        resolve_sansha_encounter_from_sde(&full, 101, 6001, "Boss").unwrap();
    assert_eq!(boss_boss, 2192);
    assert_eq!(boss_esc.len(), 9);

    let (hq_boss, hq_esc) =
        resolve_sansha_encounter_from_sde(&full, 102, 5164, "Headquarters").unwrap();
    assert!([2192, 2845, 2846, 2847].contains(&hq_boss));
    assert_eq!(hq_esc.len(), 7);

    let (as_boss, as_esc) =
        resolve_sansha_encounter_from_sde(&full, 103, 5164, "Assault").unwrap();
    assert!([2192, 2845, 2846, 2847].contains(&as_boss));
    assert_eq!(as_esc.len(), 5);

    let (vg_5165_boss, vg_5165_esc) =
        resolve_sansha_encounter_from_sde(&full, 104, 5165, "Vanguard").unwrap();
    assert_eq!(vg_5165_boss, 2845);
    assert_eq!(vg_5165_esc.len(), 3);

    let (vg_other_boss, vg_other_esc) =
        resolve_sansha_encounter_from_sde(&full, 105, 5166, "Vanguard").unwrap();
    assert_eq!(vg_other_boss, 3492);
    assert_eq!(vg_other_esc.len(), 3);

    let (scout_boss, scout_esc) =
        resolve_sansha_encounter_from_sde(&full, 106, 5166, "Scout").unwrap();
    assert_eq!(scout_boss, 3492);
    assert_eq!(scout_esc.len(), 3);

    // 3. Minimal SDE (only 1 BS, no CR/FR, no 3492) falls back to cr0/defaults
    let minimal = make_sansha_sde(false);
    let (min_boss, min_esc) =
        resolve_sansha_encounter_from_sde(&minimal, 200, 5166, "Scout").unwrap();
    assert_eq!(min_boss, 2936);
    assert_eq!(min_esc.len(), 3);
    assert_eq!(min_esc[0].type_id, 2936);
    assert_eq!(min_esc[2].type_id, 2909);
}

#[test]
fn payout_scale_and_lp_log_encoding_and_sync_filters_work_end_to_end() {
    // Payout scaling: under min, optimal, over max, states
    let under = compute_incursion_fleet_payout_scale(2, 5, 10, "Established");
    assert!((under - 0.4).abs() < 1e-9);
    let zero_min = compute_incursion_fleet_payout_scale(0, 0, 10, "Established");
    assert_eq!(zero_min, 0.0);
    let opt = compute_incursion_fleet_payout_scale(10, 5, 10, "Mobilizing");
    assert!((opt - 1.05).abs() < 1e-9);
    let withdraw = compute_incursion_fleet_payout_scale(10, 5, 10, "Withdrawing");
    assert!((withdraw - 1.15).abs() < 1e-9);
    let over2 = compute_incursion_fleet_payout_scale(12, 5, 10, "Established");
    assert!((over2 - 0.845).abs() < 1e-9);
    let over_cap = compute_incursion_fleet_payout_scale(20, 5, 10, "Established");
    assert_eq!(over_cap, 0.0);

    // LP log encoding & filtering
    let mut reg = PveSpecialSitesRegistry::default();
    let char_id = 90_000_001;
    assert!(sync_incursion_lp_log_rows(&mut reg, char_id, &[], 5_000).is_empty());

    reg.incursion_lp_logs.insert(
        char_id,
        vec![
            IncursionLPLogEntry {
                log_id: 1,
                character_id: char_id,
                site_id: "site-A".into(),
                tier: "Vanguard".into(),
                constellation_id: 20_000_001,
                solar_system_id: 30_000_142,
                isk_earned: 10_375_000,
                concord_lp_earned: 1_400,
                fleet_size: 10,
                fleet_scale_factor: 1.0,
                timestamp_filetime: 0,
            },
            IncursionLPLogEntry {
                log_id: 2,
                character_id: char_id,
                site_id: "site-B".into(),
                tier: "Assault".into(),
                constellation_id: 20_000_002,
                solar_system_id: 30_000_143,
                isk_earned: 18_200_000,
                concord_lp_earned: 3_500,
                fleet_size: 15,
                fleet_scale_factor: 1.05,
                timestamp_filetime: 999_999,
            },
        ],
    );

    // Filter by site_id string; also normalizes timestamp_filetime <= 0 to now_ft
    let by_site = sync_incursion_lp_log_rows(
        &mut reg,
        char_id,
        &[PyRep::String("site-A".into())],
        7_777_777,
    );
    assert_eq!(by_site.len(), 1);
    assert_eq!(
        kv_get(&by_site[0], "siteID"),
        Some(&PyRep::String("site-A".into()))
    );
    assert_eq!(
        kv_get(&by_site[0], "timestamp"),
        Some(&PyRep::Integer(7_777_777))
    );
    assert_eq!(
        kv_get(&by_site[0], "iskEarned"),
        Some(&PyRep::Integer(10_375_000))
    );

    // Filter by constellation_id integer
    let by_const = sync_incursion_lp_log_rows(
        &mut reg,
        char_id,
        &[PyRep::Integer(20_000_002)],
        8_888_888,
    );
    assert_eq!(by_const.len(), 1);
    assert_eq!(kv_get(&by_const[0], "logID"), Some(&PyRep::Integer(2)));
    assert_eq!(
        kv_get(&by_const[0], "timestamp"),
        Some(&PyRep::Integer(999_999))
    );

    // No filter returns all rows (and negative integer is ignored as invalid u32, returning all)
    let all_rows = sync_incursion_lp_log_rows(&mut reg, char_id, &[], 8_888_888);
    assert_eq!(all_rows.len(), 2);
    let neg_filter =
        sync_incursion_lp_log_rows(&mut reg, char_id, &[PyRep::Integer(-1)], 8_888_888);
    assert_eq!(neg_filter.len(), 2);
}
