//! Unit and mutation-killing tests for `fw_seed.rs` (`DEFAULT_FW_WARZONE_SYSTEMS` and `backfill_fw_system_occupations`).

use super::*;
use std::collections::HashSet;

#[test]
fn warzone_registry_has_175_unique_systems_and_valid_empire_factions() {
    assert_eq!(DEFAULT_FW_WARZONE_SYSTEMS.len(), 175);
    let mut seen = HashSet::with_capacity(175);
    let mut caldari_cnt = 0usize;
    let mut minmatar_cnt = 0usize;
    let mut amarr_cnt = 0usize;
    let mut gallente_cnt = 0usize;

    for &(sys_id, faction_id) in DEFAULT_FW_WARZONE_SYSTEMS {
        assert!(seen.insert(sys_id), "duplicate FW system id {sys_id}");
        assert!(sys_id >= 30_000_000 && sys_id < 31_000_000);
        match faction_id {
            500001 => caldari_cnt += 1,
            500002 => minmatar_cnt += 1,
            500003 => amarr_cnt += 1,
            500004 => gallente_cnt += 1,
            other => panic!("unexpected FW owner faction id {other}"),
        }
    }
    assert!(caldari_cnt > 0);
    assert!(minmatar_cnt > 0);
    assert!(amarr_cnt > 0);
    assert!(gallente_cnt > 0);
    assert_eq!(caldari_cnt + minmatar_cnt + amarr_cnt + gallente_cnt, 175);

    // Check specific canonical warzone systems (Tama 30002813, Amamake 30002537, Kamela 30003067, Vlillirier 30003837)
    let map: std::collections::HashMap<u32, i64> =
        DEFAULT_FW_WARZONE_SYSTEMS.iter().copied().collect();
    assert_eq!(map.get(&30002813), Some(&500001));
    assert_eq!(map.get(&30002537), Some(&500002));
    assert_eq!(map.get(&30003067), Some(&500003));
    assert_eq!(map.get(&30003837), Some(&500004));
}

#[test]
fn backfill_migrates_zero_threshold_and_preserves_custom_thresholds_and_extra_systems() {
    let mut snap = WorldStateSnapshot::default();
    // 1. System with 0 threshold should migrate to 75_000 while keeping occupier & VP
    snap.fw_system_occupations.insert(
        30002813,
        FwSystemOccupationRecord {
            solar_system_id: 30002813,
            owner_faction_id: 500001,
            occupier_faction_id: 500004,
            victory_points: 42_500,
            victory_points_threshold: 0,
        },
    );
    // 2. System with legacy 3_000 threshold should migrate to 75_000
    snap.fw_system_occupations.insert(
        30002537,
        FwSystemOccupationRecord {
            solar_system_id: 30002537,
            owner_faction_id: 500002,
            occupier_faction_id: 500003,
            victory_points: 2_900,
            victory_points_threshold: 3_000,
        },
    );
    // 3. System with non-legacy custom threshold (e.g. 90_000) must NOT be overwritten
    snap.fw_system_occupations.insert(
        30003067,
        FwSystemOccupationRecord {
            solar_system_id: 30003067,
            owner_faction_id: 500003,
            occupier_faction_id: 500002,
            victory_points: 15_000,
            victory_points_threshold: 90_000,
        },
    );
    // 4. Extra custom system outside the 175 list is preserved
    snap.fw_system_occupations.insert(
        30000142,
        FwSystemOccupationRecord {
            solar_system_id: 30000142,
            owner_faction_id: 500001,
            occupier_faction_id: 500001,
            victory_points: 100,
            victory_points_threshold: 50_000,
        },
    );

    snap.backfill_fw_system_occupations();
    assert_eq!(snap.fw_system_occupations.len(), 176);

    let tama = &snap.fw_system_occupations[&30002813];
    assert_eq!(tama.owner_faction_id, 500001);
    assert_eq!(tama.occupier_faction_id, 500004);
    assert_eq!(tama.victory_points, 42_500);
    assert_eq!(tama.victory_points_threshold, 75_000);

    let amamake = &snap.fw_system_occupations[&30002537];
    assert_eq!(amamake.occupier_faction_id, 500003);
    assert_eq!(amamake.victory_points, 2_900);
    assert_eq!(amamake.victory_points_threshold, 75_000);

    let kamela = &snap.fw_system_occupations[&30003067];
    assert_eq!(kamela.occupier_faction_id, 500002);
    assert_eq!(kamela.victory_points, 15_000);
    assert_eq!(kamela.victory_points_threshold, 90_000);

    let jita = &snap.fw_system_occupations[&30000142];
    assert_eq!(jita.victory_points, 100);
    assert_eq!(jita.victory_points_threshold, 50_000);
}
