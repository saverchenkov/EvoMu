//! Behavioral edge-case unit tests for `services::pve_expeditions_ops`.

#![forbid(unsafe_code)]

use super::*;
use eve_sde::{DungeonTemplate, SdeRegistry, SdeResourceInfo};

fn make_dungeon(
    dungeon_id: u32,
    name: &str,
    archetype_id: Option<u32>,
    faction_id: Option<u32>,
    difficulty: Option<u32>,
) -> DungeonTemplate {
    DungeonTemplate {
        dungeon_id,
        name: name.into(),
        archetype_id,
        faction_id,
        difficulty,
    }
}

/// Builds a minimal valid 136-byte `cfsd` binary matching `res:/staticdata/dungeonobjectives.fsdbinary`
/// (`stride = 56`, `mask_offset = 48`, key `U64`) so `sde.dungeon_objectives()` succeeds in tests
/// without touching global environment variables or external client files.
fn build_minimal_dungeon_objectives_cfsd(record_key: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 136];
    buf[40..48].copy_from_slice(&1u64.to_le_bytes()); // count = 1
    buf[48..56].copy_from_slice(&1u64.to_le_bytes()); // buckets = 1 (at dict_abs - 8 = 48)
    buf[56..64].copy_from_slice(&48u64.to_le_bytes()); // bucket 0 rel offset -> abs 80
    buf[72..80].copy_from_slice(&1u64.to_le_bytes()); // n = 1 entry in bucket 0
    buf[80..88].copy_from_slice(&record_key.to_le_bytes()); // record key U64
    // mask at 80 + 48 = 128 is 0 -> optional clientObjectives skipped cleanly
    buf
}

#[test]
fn escalation_chain_dungeon_for_step_handles_bounds_and_empty_chain() {
    let chain = SdeEscalationChain {
        faction_id: 500_010,
        step_dungeon_ids: vec![1420, 1421, 1422],
        expedition_name: "Guristas".into(),
        total_steps: 3,
        journal_template_id: 1420,
    };
    assert_eq!(chain.dungeon_for_step(0), 1420);
    assert_eq!(chain.dungeon_for_step(1), 1420);
    assert_eq!(chain.dungeon_for_step(2), 1421);
    assert_eq!(chain.dungeon_for_step(3), 1422);
    assert_eq!(chain.dungeon_for_step(99), 1422);

    let empty = SdeEscalationChain {
        faction_id: 500_010,
        step_dungeon_ids: vec![],
        expedition_name: "Empty".into(),
        total_steps: 1,
        journal_template_id: 1,
    };
    assert_eq!(empty.dungeon_for_step(1), 1422);
}

#[test]
fn dungeon_objectives_paths_cover_err_fallback_and_valid_cfsd_decode() {
    let mut sde = SdeRegistry::default();
    sde.resources.remove("res:/staticdata/dungeonobjectives.fsdbinary");

    // Missing resource -> Err fallback
    assert_eq!(sde_dungeon_room_count(&sde, 1420), 2);
    assert_eq!(resolve_escalation_journal_template_and_hint(&sde, 0), (1, None));
    assert_eq!(
        resolve_escalation_journal_template_and_hint(&sde, 1420),
        (1420, None)
    );

    // Valid synthetic cfsd file via absolute temp path in SdeResourceInfo
    let unique = format!(
        "evomu_dungeonobj_{}_{}.fsdbinary",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let tmp_path = std::env::temp_dir().join(unique);
    let bytes = build_minimal_dungeon_objectives_cfsd(1420);
    std::fs::write(&tmp_path, &bytes).expect("write temp cfsd");

    sde.resources.insert(
        "res:/staticdata/dungeonobjectives.fsdbinary".into(),
        SdeResourceInfo {
            res_path: "res:/staticdata/dungeonobjectives.fsdbinary".into(),
            cdn_relative_path: tmp_path.to_string_lossy().into_owned(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: "cfsd".into(),
            record_count: 1,
            spot: String::new(),
        },
    );

    assert_eq!(sde_dungeon_room_count(&sde, 1420), 1);
    assert_eq!(
        resolve_escalation_journal_template_and_hint(&sde, 1420),
        (1420, None)
    );

    let _ = std::fs::remove_file(&tmp_path);
}

#[test]
fn resolve_sde_escalation_chain_covers_all_faction_inference_and_template_branches() {
    let mut sde = SdeRegistry::default();
    sde.dungeons.clear();
    let origin_sys = 30_000_142;
    sde.solar_systems.insert(
        origin_sys,
        eve_sde::SolarSystem {
            solar_system_id: origin_sys,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Jita".into(),
            security_status: 0.9,
            radius: 1.0e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );

    // 1. No arch=31 dungeons at all -> fallback to pve_expeditions::expedition_dungeon_chain
    let fallback = resolve_sde_escalation_chain(&sde, 1420, origin_sys);
    assert_eq!(fallback.expedition_name, "Guristas Scout Outpost");
    assert_eq!(fallback.total_steps, 3);
    assert_eq!(fallback.step_dungeon_ids, vec![1420, 1421, 1422]);

    // 2. Test name-based faction inference on non-arch31 source dungeons + cross-faction arch=31 fallback
    sde.dungeons.insert(
        5001,
        make_dungeon(5001, "QA Test Angel Site", Some(31), Some(500_011), Some(3)),
    );
    sde.dungeons.insert(
        5002,
        make_dungeon(5002, "DO NOT TRANSLATE", Some(31), Some(500_011), Some(4)),
    );
    sde.dungeons.insert(
        5003,
        make_dungeon(5003, "Test Escalation", Some(31), Some(500_011), Some(5)),
    );
    // Valid arch=31 templates for Guristas (500_010) and Angel (500_011)
    sde.dungeons.insert(
        6001,
        make_dungeon(6001, "Guristas Escalation I", Some(31), Some(500_010), Some(2)),
    );
    sde.dungeons.insert(
        6002,
        make_dungeon(6002, "Guristas Escalation II", Some(31), Some(500_010), Some(4)),
    );
    sde.dungeons.insert(
        6003,
        make_dungeon(6003, "Guristas Finale", Some(31), Some(500_010), Some(5)),
    );
    sde.dungeons.insert(
        6010,
        make_dungeon(6010, "Angel Strike", Some(31), Some(500_011), Some(4)),
    );

    // Direct arch=31 source with diff <= 2 -> 3 steps, skips QA/Test templates
    let c_gur_arch = resolve_sde_escalation_chain(&sde, 6001, origin_sys);
    assert_eq!(c_gur_arch.faction_id, 500_010);
    assert_eq!(c_gur_arch.total_steps, 3);
    assert_eq!(c_gur_arch.step_dungeon_ids[0], 6001);
    assert_eq!(c_gur_arch.expedition_name, "Guristas Escalation I");

    // Direct arch=31 source with diff > 2 and no distinct candidate -> 4 steps, reuses own id
    let c_ang_arch = resolve_sde_escalation_chain(&sde, 6010, origin_sys);
    assert_eq!(c_ang_arch.faction_id, 500_011);
    assert_eq!(c_ang_arch.total_steps, 4);
    assert_eq!(c_ang_arch.step_dungeon_ids, vec![6010, 6010, 6010, 6010]);

    // Source dungeons without faction_id inferred by name keywords
    let name_cases = [
        (7001, "Angel Cartel Outpost", 500_011_u32),
        (7002, "Crimson Blood Hideout", 500_012),
        (7003, "Sansha Nation Rally", 500_019),
        (7004, "Serpentis Shadow Base", 500_020),
        (7005, "Guristas Pith Refuge", 500_010),
        (7006, "Rogue Drone Hive", 500_021),
    ];
    for (did, name, expected_fid) in name_cases {
        sde.dungeons
            .insert(did, make_dungeon(did, name, Some(10), None, Some(3)));
        let ch = resolve_sde_escalation_chain(&sde, did, origin_sys);
        assert_eq!(ch.faction_id, expected_fid, "failed for {name}");
        assert_eq!(ch.total_steps, 4);
        assert_eq!(ch.step_dungeon_ids.len(), 4);
    }

    // Rogue Drone faction_id 500_021 WITH an arch=31 drone dungeon -> preserves 500_021
    sde.dungeons.insert(
        7010,
        make_dungeon(7010, "Drone Overmind", Some(10), Some(500_021), Some(2)),
    );
    sde.dungeons.insert(
        7011,
        make_dungeon(7011, "Drone Escalation", Some(31), Some(500_021), Some(3)),
    );
    let drone_ch = resolve_sde_escalation_chain(&sde, 7010, origin_sys);
    assert_eq!(drone_ch.faction_id, 500_021);
    assert_eq!(drone_ch.total_steps, 3);
    assert_eq!(drone_ch.step_dungeon_ids[0], 7011);

    // Non-500021/26/27 explicit faction_id on source dungeon
    sde.dungeons.insert(
        7020,
        make_dungeon(7020, "Blood Source", Some(10), Some(500_012), Some(1)),
    );
    let blood_ch = resolve_sde_escalation_chain(&sde, 7020, origin_sys);
    assert_eq!(blood_ch.faction_id, 500_012);
    assert_eq!(blood_ch.total_steps, 3);

    // Format escalation journal text with known and unknown solar system IDs
    let sys_name = sde.solar_systems[&origin_sys].name.clone();
    let txt_known = format_escalation_journal_text(&sde, "The Maze", 2, 4, 6001, origin_sys);
    assert!(txt_known.contains("The Maze"));
    assert!(txt_known.contains("Step 2/4"));
    assert!(txt_known.contains(&sys_name));

    let txt_unknown = format_escalation_journal_text(&sde, "The Maze", 1, 3, 6001, 39_999_999);
    assert!(txt_unknown.contains("39999999 (39999999)"));
}
