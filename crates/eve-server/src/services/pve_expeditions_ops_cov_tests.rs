//! Direct unit tests covering all branches of `pve_expeditions_ops`.

use super::super::ops::*;
use super::shared_sde;
use eve_sde::{ConstellationInfo, DungeonTemplate, RegionInfo, SolarSystem};

fn make_dungeon(
    dungeon_id: u32,
    name: &str,
    archetype_id: Option<u32>,
    faction_id: Option<u32>,
    difficulty: Option<u32>,
) -> DungeonTemplate {
    DungeonTemplate {
        dungeon_id,
        name: name.to_string(),
        archetype_id,
        faction_id,
        difficulty,
    }
}

#[test]
fn sde_escalation_chain_dungeon_for_step_and_room_count_edges() {
    let empty_chain = SdeEscalationChain {
        faction_id: 500_010,
        step_dungeon_ids: vec![],
        expedition_name: "Empty".into(),
        total_steps: 3,
        journal_template_id: 1,
    };
    assert_eq!(empty_chain.dungeon_for_step(0), 1422);
    assert_eq!(empty_chain.dungeon_for_step(1), 1422);

    let chain = SdeEscalationChain {
        faction_id: 500_010,
        step_dungeon_ids: vec![2001, 2002, 2003],
        expedition_name: "ThreeStep".into(),
        total_steps: 3,
        journal_template_id: 42,
    };
    assert_eq!(chain.dungeon_for_step(0), 2001);
    assert_eq!(chain.dungeon_for_step(1), 2001);
    assert_eq!(chain.dungeon_for_step(2), 2002);
    assert_eq!(chain.dungeon_for_step(3), 2003);
    assert_eq!(chain.dungeon_for_step(99), 2003);

    let sde = shared_sde();
    assert!(sde_dungeon_room_count(&sde, 1420) >= 1);
    assert_eq!(sde_dungeon_room_count(&sde, 999_999_999), 1);

    let (tpl_zero, hint_zero) = resolve_escalation_journal_template_and_hint(&sde, 0);
    assert_eq!(tpl_zero, 1);
    assert!(hint_zero.is_none());

    let (tpl_missing, hint_missing) = resolve_escalation_journal_template_and_hint(&sde, 999_999_999);
    assert_eq!(tpl_missing, 999_999_999);
    assert!(hint_missing.is_none());
}

#[test]
fn resolve_sde_escalation_chain_covers_factions_templates_and_fallbacks() {
    let mut sde = (*shared_sde()).clone();
    sde.dungeons.clear();

    // 1. Completely empty sde.dungeons falls back to expedition_dungeon_chain
    let fallback = resolve_sde_escalation_chain(&sde, 1420, 30_000_142);
    assert_eq!(fallback.total_steps, 3);
    assert_eq!(fallback.step_dungeon_ids, vec![1420, 1421, 1422]);
    assert_eq!(fallback.expedition_name, "Guristas Scout Outpost");

    // 2. Seed synthetic systems/regions/constellations to cover all 5 PirateFaction fallbacks
    let faction_systems: &[(u32, u32, u32, u32, &str)] = &[
        (9_000_001, 900_001, 90_001, 500_002, "Heimatar"),    // AngelCartel (500011)
        (9_000_002, 900_002, 90_002, 500_003, "Domain"),      // BloodRaiders (500012)
        (9_000_003, 900_003, 90_003, 500_007, "Stain"),       // Sansha (500019)
        (9_000_004, 900_004, 90_004, 500_004, "Sinq Laison"), // Serpentis (500020)
        (9_000_005, 900_005, 90_005, 500_001, "The Forge"),   // Guristas (500010)
    ];
    for &(sys_id, con_id, reg_id, fac_id, reg_name) in faction_systems {
        sde.regions.insert(
            reg_id,
            RegionInfo {
                region_id: reg_id,
                name: reg_name.into(),
                center: [0.0, 0.0, 0.0],
                faction_id: Some(fac_id),
                wormhole_class_id: None,
            },
        );
        sde.constellations.insert(
            con_id,
            ConstellationInfo {
                constellation_id: con_id,
                region_id: reg_id,
                name: format!("Con-{con_id}"),
                center: [0.0, 0.0, 0.0],
                faction_id: Some(fac_id),
                wormhole_class_id: None,
            },
        );
        sde.solar_systems.insert(
            sys_id,
            SolarSystem {
                solar_system_id: sys_id,
                constellation_id: con_id,
                region_id: reg_id,
                name: format!("Sys-{sys_id}"),
                security_status: 0.4,
                radius: 1.0e12,
                galactic_pos_ly: [0.0, 0.0, 0.0],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
    }

    // Seed escalation template dungeons (arch 31) and ignored test/QA templates
    sde.dungeons.insert(80_001, make_dungeon(80_001, "QA Test Angel", Some(31), Some(500_011), Some(3)));
    sde.dungeons.insert(80_002, make_dungeon(80_002, "DO NOT TRANSLATE", Some(31), Some(500_011), Some(4)));
    sde.dungeons.insert(80_003, make_dungeon(80_003, "Test Escalation", Some(31), Some(500_011), Some(5)));
    sde.dungeons.insert(80_010, make_dungeon(80_010, "Guristas Stage 1", Some(31), Some(500_010), Some(3)));
    sde.dungeons.insert(80_011, make_dungeon(80_011, "Guristas Stage 2", Some(31), Some(500_010), Some(4)));
    sde.dungeons.insert(80_012, make_dungeon(80_012, "Guristas Finale", Some(31), Some(500_010), Some(5)));

    // Unknown source_dungeon_id falls back to system faction and any arch=31 when faction has no arch=31
    assert_eq!(resolve_sde_escalation_chain(&sde, 999_001, 9_000_001).faction_id, 500_011);
    assert_eq!(resolve_sde_escalation_chain(&sde, 999_001, 9_000_002).faction_id, 500_012);
    assert_eq!(resolve_sde_escalation_chain(&sde, 999_001, 9_000_003).faction_id, 500_019);
    assert_eq!(resolve_sde_escalation_chain(&sde, 999_001, 9_000_004).faction_id, 500_020);
    let gur_chain = resolve_sde_escalation_chain(&sde, 999_001, 9_000_005);
    assert_eq!(gur_chain.faction_id, 500_010);
    assert_eq!(gur_chain.total_steps, 3);
    assert_eq!(gur_chain.step_dungeon_ids.len(), 3);

    // 3. Name-based faction inference when faction_id is None or rogue drone (500021 without arch=31)
    let name_cases: &[(u32, &str, Option<u32>, u32)] = &[
        (81_001, "Domination Hideout", None, 500_011),
        (81_002, "Crimson Hand Depot", None, 500_012),
        (81_003, "Centi Rally Point", None, 500_019),
        (81_004, "Shadow Coreli Base", None, 500_020),
        (81_005, "Pith Recon Outpost", None, 500_010),
        (81_006, "Alvum Drone Hive", Some(500_021), 500_021),
    ];
    for &(did, name, fid, expected_fid) in name_cases {
        sde.dungeons.insert(did, make_dungeon(did, name, Some(22), fid, Some(4)));
        let ch = resolve_sde_escalation_chain(&sde, did, 9_000_005);
        assert_eq!(ch.faction_id, expected_fid, "name inference failed for {name}");
        assert_eq!(ch.total_steps, 4);
    }

    // Direct arch=31 source dungeon with difficulty 3 -> 4-step chain starting at source
    sde.dungeons.insert(81_007, make_dungeon(81_007, "Drone Arch31", Some(31), Some(500_021), Some(3)));
    let drone_direct = resolve_sde_escalation_chain(&sde, 81_007, 9_000_005);
    assert_eq!(drone_direct.faction_id, 500_021);
    assert_eq!(drone_direct.step_dungeon_ids[0], 81_007);
    assert_eq!(drone_direct.total_steps, 4);

    // Direct Guristas arch=31 source dungeon walks ascending difficulties without repeating prev_id
    let gur_direct = resolve_sde_escalation_chain(&sde, 80_010, 9_000_005);
    assert_eq!(gur_direct.expedition_name, "Guristas Stage 1");
    assert_eq!(gur_direct.total_steps, 4);
    assert_eq!(gur_direct.step_dungeon_ids[0], 80_010);
    assert_eq!(gur_direct.step_dungeon_ids[1], 80_011);
    assert_eq!(gur_direct.step_dungeon_ids[2], 80_012);
    // When no distinct diff-5 candidate remains, falls back to source_dungeon_id (80_010)
    assert_eq!(gur_direct.step_dungeon_ids[3], 80_010);

    // Non-500021 explicit faction_id on source dungeon returns directly
    sde.dungeons.insert(81_008, make_dungeon(81_008, "Generic Site", Some(22), Some(500_019), Some(2)));
    let explicit_fac = resolve_sde_escalation_chain(&sde, 81_008, 9_000_005);
    assert_eq!(explicit_fac.faction_id, 500_019);
    assert_eq!(explicit_fac.total_steps, 3);

    // format_escalation_journal_text: known system vs unknown system fallback
    let txt_known = format_escalation_journal_text(&sde, "Guristas Stage 1", 2, 4, 999_999_999, 9_000_005);
    assert!(txt_known.contains("Sys-9000005 (9000005)"));
    assert!(txt_known.contains("Step 2/4"));
    let txt_unknown_sys = format_escalation_journal_text(&sde, "Unknown", 1, 3, 999_999_999, 77_777_777);
    assert!(txt_unknown_sys.contains("77777777 (77777777)"));
}
