//! Unit and mutation-killing tests for `belt_ores`.

use crate::{ConstellationInfo, GroupInfo, InvType, SdeRegistry, SolarSystem};
use std::collections::HashMap;

fn populate_synthetic_ore_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    let ore_groups = [
        (450, "Arkonor", 16.0),
        (451, "Bistot", 16.0),
        (452, "Crokite", 16.0),
        (453, "Dark Ochre", 8.0),
        (454, "Hedbergite", 3.0),
        (455, "Hemorphite", 3.0),
        (456, "Jaspet", 2.0),
        (457, "Kernite", 1.2),
        (458, "Plagioclase", 0.35),
        (459, "Pyroxeres", 0.3),
        (460, "Scordite", 0.15),
        (461, "Spodumain", 16.0),
        (462, "Veldspar", 0.1),
        (467, "Gneiss", 5.0),
        (468, "Mercoxit", 40.0),
        (469, "Omber", 0.6),
        (4029, "Talassonite", 16.0),
        (4030, "Rakovene", 16.0),
        (4031, "Bezdnacine", 16.0),
    ];

    for (idx, (gid, name, vol)) in ore_groups.into_iter().enumerate() {
        sde.groups.insert(
            gid,
            GroupInfo {
                group_id: gid,
                category_id: 25,
                name: name.to_string(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
        let tid = 1000 + idx as u32;
        sde.types.insert(
            tid,
            InvType {
                type_id: tid,
                group_id: gid,
                category_id: 25,
                name: name.to_string(),
                mass: 1.0,
                volume: vol,
                capacity: 0.0,
                radius: 1.0,
                base_price: 10.0,
                published: true,
                portion_size: 100,
                market_group_id: None,
                race_id: None,
                faction_id: None,
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
                default_attributes: HashMap::new(),
            },
        );
    }

    // Add filter edge cases: unpublished Veldspar, wrong category Veldspar, and variant name ("Concentrated Veldspar")
    for (tid, cat, pub_flag, name) in [
        (9001, 25, false, "Veldspar"),
        (9002, 6, true, "Veldspar"),
        (9003, 25, true, "Concentrated Veldspar"),
    ] {
        sde.types.insert(
            tid,
            InvType {
                type_id: tid,
                group_id: 462,
                category_id: cat,
                name: name.to_string(),
                mass: 1.0,
                volume: 0.1,
                capacity: 0.0,
                radius: 1.0,
                base_price: 10.0,
                published: pub_flag,
                portion_size: 100,
                market_group_id: None,
                race_id: None,
                faction_id: None,
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
                default_attributes: HashMap::new(),
            },
        );
    }

    // Constellations per empire faction (including a lower constellation_id with faction_id = None to test filter)
    sde.constellations.insert(
        1,
        ConstellationInfo {
            constellation_id: 1,
            region_id: 10,
            name: "NoFaction".to_string(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    for (cid, rid, fid) in [
        (10, 10, Some(500_003)), // Amarr
        (11, 10, Some(500_001)), // Higher constellation_id in region 10 ignored by min_by_key
        (20, 20, Some(500_001)), // Caldari
        (30, 30, Some(500_004)), // Gallente
        (40, 40, Some(500_002)), // Minmatar
    ] {
        sde.constellations.insert(
            cid,
            ConstellationInfo {
                constellation_id: cid,
                region_id: rid,
                name: format!("Const{cid}"),
                center: [0.0; 3],
                faction_id: fid,
                wormhole_class_id: None,
            },
        );
    }

    sde.solar_systems.insert(
        30000142,
        SolarSystem {
            solar_system_id: 30000142,
            constellation_id: 20,
            region_id: 20,
            name: "Jita".to_string(),
            security_status: 0.9,
            radius: 1.0,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );

    sde
}

#[test]
fn belt_ores_security_bands_factions_pochven_and_sorting() {
    let sde = populate_synthetic_ore_sde();
    let names = |sec: f64, reg: u32| -> Vec<String> {
        sde.belt_ores_for_security_and_region(sec, reg)
            .into_iter()
            .map(|t| t.name.clone())
            .collect()
    };

    // 1. Pochven (region 10_000_070) ignores security_status and includes Spodumain + Triglavian ores
    assert_eq!(
        names(0.9, 10_000_070),
        vec!["Spodumain", "Talassonite", "Rakovene", "Bezdnacine"]
    );
    assert_eq!(
        names(-1.0, 10_000_070),
        vec!["Spodumain", "Talassonite", "Rakovene", "Bezdnacine"]
    );

    // 1b. Yasna Zakh / Zarzakh (10_001_000) has no belts; Wormhole J-space (11_000_001) has J-space ores
    assert!(names(-1.0, 10_001_000).is_empty());
    assert_eq!(
        names(-1.0, 11_000_001),
        vec![
            "Pyroxeres",
            "Omber",
            "Kernite",
            "Gneiss",
            "Arkonor",
            "Bistot"
        ]
    );

    // 2. Core 1.0 highsec (`>= 0.95`) spawns only Veldspar & Scordite across all quarters
    assert_eq!(names(0.95, 10), vec!["Veldspar", "Scordite"]);
    assert_eq!(names(1.0, 20), vec!["Veldspar", "Scordite"]);
    assert_eq!(names(1.0, 30), vec!["Veldspar", "Scordite"]);

    // 3. Highsec 0.9..0.5 (`0.45..0.95`) Empire Quarter distributions
    assert_eq!(names(0.949, 10), vec!["Veldspar", "Scordite", "Pyroxeres"]); // Amarr 0.9
    assert_eq!(names(0.45, 10), vec!["Veldspar", "Scordite", "Pyroxeres"]); // Amarr 0.5
    assert_eq!(names(0.85, 20), vec!["Veldspar", "Scordite", "Pyroxeres"]); // Caldari 0.9..0.8 (Class B)
    assert_eq!(
        names(0.749, 20),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    ); // Caldari 0.7..0.5 (Class C)
    assert_eq!(names(0.85, 30), vec!["Veldspar", "Scordite", "Plagioclase"]); // Gallente 0.9..0.5
    assert_eq!(names(0.45, 30), vec!["Veldspar", "Scordite", "Plagioclase"]);
    assert_eq!(names(0.85, 40), vec!["Veldspar", "Scordite", "Plagioclase"]); // Minmatar 0.9..0.5
    assert_eq!(names(0.45, 40), vec!["Veldspar", "Scordite", "Plagioclase"]);
    assert_eq!(
        names(0.65, 999),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    ); // Unknown Empire fallback

    // 4. Lowsec 0.4..0.1 (`0.0 < sec < 0.45`) by Empire Quarter & sub-band (no Veldspar/Scordite)
    assert_eq!(names(0.449, 10), vec!["Pyroxeres", "Kernite", "Jaspet"]); // Amarr B2
    assert_eq!(
        names(0.249, 10),
        vec!["Pyroxeres", "Kernite", "Jaspet", "Hemorphite"]
    ); // Amarr B3
    assert_eq!(names(0.449, 20), vec!["Pyroxeres", "Kernite"]); // Caldari C1
    assert_eq!(names(0.249, 20), vec!["Pyroxeres", "Kernite", "Hedbergite"]); // Caldari C2
    assert_eq!(names(0.449, 30), vec!["Omber", "Jaspet"]); // Gallente D2
    assert_eq!(names(0.249, 30), vec!["Omber", "Jaspet", "Hemorphite"]); // Gallente D3
    assert_eq!(names(0.449, 40), vec!["Omber", "Kernite"]); // Minmatar E
    assert_eq!(names(0.249, 40), vec!["Omber", "Kernite", "Hedbergite"]); // Minmatar E1

    // 5. Nullsec (`<= 0.0`) by Security Class F..K
    assert!(names(-0.2, 10_000_005).is_empty()); // F..F2 Detorid
    assert_eq!(names(-0.5, 10_000_005), vec!["Bistot"]); // F3
    assert_eq!(names(-0.6, 10_000_005), vec!["Arkonor", "Bistot"]); // F4
    assert_eq!(
        names(-0.8, 10_000_005),
        vec!["Pyroxeres", "Arkonor", "Bistot", "Mercoxit"]
    ); // F6..F7
    assert_eq!(names(-0.2, 10_000_050), vec!["Kernite"]); // G..G1 Querious
    assert_eq!(
        names(-0.8, 10_000_050),
        vec!["Pyroxeres", "Kernite", "Bistot", "Mercoxit"]
    ); // G6
    assert_eq!(
        names(-0.9, 10_000_039),
        vec!["Pyroxeres", "Kernite", "Arkonor", "Bistot", "Mercoxit"]
    ); // G7 Esoteria rare Arkonor
    assert_eq!(names(-0.2, 10_000_010), vec!["Pyroxeres"]); // H..H2 Tribute
    assert_eq!(
        names(-0.8, 10_000_010),
        vec!["Pyroxeres", "Kernite", "Mercoxit"]
    ); // H6
    assert_eq!(
        names(-0.9, 10_000_010),
        vec!["Pyroxeres", "Kernite", "Bistot", "Mercoxit"]
    ); // H7
    assert!(names(-0.5, 10_000_014).is_empty()); // I..I4 Catch
    assert_eq!(names(-0.7, 10_000_014), vec!["Arkonor"]); // I5
    assert_eq!(names(-0.8, 10_000_014), vec!["Arkonor", "Mercoxit"]); // I6
    assert_eq!(names(-0.2, 10_000_058), vec!["Pyroxeres"]); // J..J2 Fountain
    assert_eq!(names(-0.5, 10_000_058), vec!["Pyroxeres", "Bistot"]); // J3..J5
    assert_eq!(
        names(-0.9, 10_000_058),
        vec!["Pyroxeres", "Arkonor", "Bistot", "Mercoxit"]
    ); // J7
    assert_eq!(names(-0.4, 10_000_013), vec!["Bistot"]); // K4 Malpais
    assert_eq!(
        names(-0.8, 10_000_013),
        vec!["Arkonor", "Bistot", "Mercoxit"]
    ); // K6..K7

    // 6. belt_ores_for_system found vs missing
    assert!(sde.belt_ores_for_system(99_999_999).is_empty());
    let jita_ores: Vec<&str> = sde
        .belt_ores_for_system(30000142)
        .into_iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(jita_ores, vec!["Veldspar", "Scordite", "Pyroxeres"]);
}

#[test]
fn belt_ores_tiebreaks_filtering_and_region_constellation_mutants() {
    let mut sde = populate_synthetic_ore_sde();

    // Decoys filtered out: wrong category_id, unpublished, and non-base name ("Dense Veldspar")
    let mut base_veld = sde.types[&1012].clone();
    base_veld.type_id = 9001;
    base_veld.category_id = 4;
    sde.types.insert(9001, base_veld.clone());

    let mut unpub_veld = sde.types[&1012].clone();
    unpub_veld.type_id = 9002;
    unpub_veld.published = false;
    sde.types.insert(9002, unpub_veld);

    let mut dense_veld = sde.types[&1012].clone();
    dense_veld.type_id = 9003;
    dense_veld.name = "Dense Veldspar".to_string();
    sde.types.insert(9003, dense_veld);

    // Same-volume tiebreak on type_id: two published types matching group name "Omber" (volume 0.6)
    let mut omber_tie = sde.types[&1015].clone();
    omber_tie.type_id = 500; // < 1015 -> must sort strictly before 1015
    sde.types.insert(500, omber_tie);

    let low_ids: Vec<u32> = sde
        .belt_ores_for_security_and_region(0.3, 30)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    let idx_500 = low_ids.iter().position(|&id| id == 500).unwrap();
    let idx_1015 = low_ids.iter().position(|&id| id == 1015).unwrap();
    assert!(idx_500 < idx_1015, "equal volume must sort ascending by type_id");
    assert!(!low_ids.contains(&9001));
    assert!(!low_ids.contains(&9002));
    assert!(!low_ids.contains(&9003));

    // Region with multiple constellations: lowest has faction_id=None (must skip),
    // middle has Some(500_003) (Amarr -> Pyroxeres), highest has Some(500_001) (Caldari -> Plagioclase).
    // Kills min_by_key -> max_by_key or dropping `faction_id.is_some()` mutants!
    sde.constellations.insert(
        20_000_001,
        ConstellationInfo {
            constellation_id: 20_000_001,
            region_id: 10_000_888,
            name: "NoFactionLow".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    sde.constellations.insert(
        20_000_002,
        ConstellationInfo {
            constellation_id: 20_000_002,
            region_id: 10_000_888,
            name: "AmarrMid".into(),
            center: [0.0; 3],
            faction_id: Some(500_003),
            wormhole_class_id: None,
        },
    );
    sde.constellations.insert(
        20_000_003,
        ConstellationInfo {
            constellation_id: 20_000_003,
            region_id: 10_000_888,
            name: "CaldariHigh".into(),
            center: [0.0; 3],
            faction_id: Some(500_001),
            wormhole_class_id: None,
        },
    );
    let mid_highsec: Vec<&str> = sde
        .belt_ores_for_security_and_region(0.7, 10_000_888)
        .into_iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(mid_highsec, vec!["Veldspar", "Scordite", "Pyroxeres"]);
}

#[test]
fn belt_ores_for_system_filters_unpublished_and_non_asteroid_types() {
    let mut sde = populate_synthetic_ore_sde();
    // 9001 = (cat 25, pub false), 9002 = (cat 6, pub true), 1012 = Veldspar (cat 25, pub true).
    std::sync::Arc::make_mut(&mut sde.asteroid_types_by_system).insert(
        30000142,
        vec![9001, 9002, 1012, 1012],
    );
    let ids: Vec<u32> = sde
        .belt_ores_for_system(30000142)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    assert_eq!(ids, vec![1012]);
}

#[test]
fn belt_ores_beltless_kspace_band_falls_back_to_canonical_ores() {
    let mut sde = populate_synthetic_ore_sde();
    // Global `asteroid_types_by_system` is populated only for Jita (30000142, Caldari).
    std::sync::Arc::make_mut(&mut sde.asteroid_types_by_system).insert(30000142, vec![1012]);
    // Insert beltless systems in Amarr (region 10, sec 0.7) and Gallente lowsec (region 30, sec 0.2),
    // plus a barren nullsec F-band system (Detorid 10_000_005, sec -0.2).
    for (sid, cid, rid, sec) in [
        (30_000_201, 10, 10, 0.7),
        (30_000_202, 30, 30, 0.2),
        (30_000_203, 10, 10_000_005, -0.2),
    ] {
        sde.solar_systems.insert(
            sid,
            SolarSystem {
                solar_system_id: sid,
                constellation_id: cid,
                region_id: rid,
                name: format!("Beltless{sid}"),
                security_status: sec,
                radius: 1.0,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
    }

    let names = |sec: f64, reg: u32| -> Vec<String> {
        sde.belt_ores_for_security_and_region(sec, reg)
            .into_iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(names(0.7, 10), vec!["Veldspar", "Scordite", "Pyroxeres"]);
    assert_eq!(names(0.2, 30), vec!["Omber", "Jaspet", "Hemorphite"]);
    assert!(names(-0.2, 10_000_005).is_empty());
}
