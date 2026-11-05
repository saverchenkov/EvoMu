//! Unit and mutation-killing tests for `belt_ores`.

use crate::{ConstellationInfo, GroupInfo, InvType, SdeRegistry, SolarSystem};
use std::collections::HashMap;

fn populate_synthetic_ore_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    let ore_groups = [
        (450, "Arkonor", 16.0),
        (451, "Bistot", 16.0),
        (452, "Dark Ochre", 8.0),
        (453, "Gneiss", 5.0),
        (454, "Hedbergite", 3.0),
        (455, "Hemorphite", 3.0),
        (456, "Jaspet", 2.0),
        (457, "Kernite", 1.2),
        (458, "Plagioclase", 0.35),
        (459, "Pyroxeres", 0.3),
        (460, "Scordite", 0.15),
        (461, "Spodumain", 16.0),
        (462, "Veldspar", 0.1),
        (467, "Crokite", 16.0),
        (468, "Mercoxit", 40.0),
        (469, "Omber", 0.6),
        (4029, "Bezdnacine", 16.0),
        (4030, "Rakovene", 16.0),
        (4031, "Talassonite", 16.0),
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

    // 1. Pochven (region 10_000_070) ignores security_status and sorts equal-volume ores by type_id
    assert_eq!(
        names(0.9, 10_000_070),
        vec!["Bezdnacine", "Rakovene", "Talassonite"]
    );
    assert_eq!(
        names(-1.0, 10_000_070),
        vec!["Bezdnacine", "Rakovene", "Talassonite"]
    );

    // 2. High-sec >= 0.85 boundary (0.85 vs 0.849)
    assert_eq!(names(0.85, 10), vec!["Veldspar", "Scordite"]);
    assert_eq!(names(1.0, 10), vec!["Veldspar", "Scordite"]);

    // 3. 0.65..0.85 empire faction variations (sorted by volume ascending)
    assert_eq!(names(0.849, 10), vec!["Veldspar", "Scordite", "Pyroxeres"]); // Amarr
    assert_eq!(names(0.65, 20), vec!["Veldspar", "Scordite", "Plagioclase"]); // Caldari
    assert_eq!(names(0.70, 30), vec!["Veldspar", "Scordite", "Plagioclase"]); // Gallente
    assert_eq!(
        names(0.65, 40),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    ); // Minmatar
    assert_eq!(
        names(0.65, 999),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    ); // Unknown faction fallback

    // 4. 0.45..0.65 boundary
    assert_eq!(
        names(0.649, 10),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    );
    assert_eq!(
        names(0.45, 10),
        vec!["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
    );

    // 5. Low-sec 0.05..0.45 boundary (sorted by volume: Pyroxeres 0.3, Omber 0.6, Kernite 1.2, Jaspet 2.0, Hedbergite 3.0, Hemorphite 3.0)
    assert_eq!(
        names(0.449, 10),
        vec![
            "Pyroxeres",
            "Omber",
            "Kernite",
            "Jaspet",
            "Hedbergite",
            "Hemorphite"
        ]
    );
    assert_eq!(
        names(0.05, 10),
        vec![
            "Pyroxeres",
            "Omber",
            "Kernite",
            "Jaspet",
            "Hedbergite",
            "Hemorphite"
        ]
    );

    // 6. High null-sec -0.45..0.05 boundary (Gneiss 5, Dark Ochre 8, Spodumain 16, Crokite 16, Mercoxit 40)
    assert_eq!(
        names(0.049, 10),
        vec!["Gneiss", "Dark Ochre", "Spodumain", "Crokite", "Mercoxit"]
    );
    assert_eq!(
        names(-0.45, 10),
        vec!["Gneiss", "Dark Ochre", "Spodumain", "Crokite", "Mercoxit"]
    );

    // 7. Deep null-sec < -0.45 boundary (adds Arkonor & Bistot at volume 16.0)
    assert_eq!(
        names(-0.451, 10),
        vec![
            "Gneiss",
            "Dark Ochre",
            "Arkonor",
            "Bistot",
            "Spodumain",
            "Crokite",
            "Mercoxit"
        ]
    );

    // 8. belt_ores_for_system found vs missing
    assert!(sde.belt_ores_for_system(99_999_999).is_empty());
    let jita_ores: Vec<&str> = sde
        .belt_ores_for_system(30000142)
        .into_iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(jita_ores, vec!["Veldspar", "Scordite"]);
}
