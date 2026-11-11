//! Additional unit and mutation-killing tests for `eve-sde` (`lib.rs`).

#![forbid(unsafe_code)]

use super::*;

fn sample_inv_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    published: bool,
    market_group_id: Option<u32>,
    variation_parent_type_id: Option<TypeId>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type_{type_id}"),
        mass: 100.0,
        volume: 10.0,
        capacity: 5.0,
        radius: 2.0,
        base_price: 1_000.0,
        published,
        portion_size: 1,
        market_group_id,
        race_id: Some(1),
        faction_id: Some(500_001),
        variation_parent_type_id,
        description_id: Some(10),
        icon_id: Some(20),
        graphic_id: Some(30),
        sound_id: Some(40),
        meta_group_id: Some(1),
        meta_level: Some(0),
        tech_level: Some(1),
        default_effect_id: Some(16),
        description: format!("Desc_{type_id}"),
        effect_ids: vec![16],
        default_attributes: HashMap::from([(attr_ids::MASS, 100.0)]),
    }
}

#[test]
fn test_type_list_info_contains_all_inclusion_and_exclusion_branches() {
    let base_ty = sample_inv_type(100, 20, 6, true, None, None);

    // 1. Empty list matches nothing
    let empty_list = TypeListInfo::default();
    assert!(!empty_list.contains(&base_ty));

    // 2. Inclusion by type_id only
    let by_type = TypeListInfo {
        included_type_ids: vec![100],
        ..Default::default()
    };
    assert!(by_type.contains(&base_ty));
    assert!(!by_type.contains(&sample_inv_type(101, 20, 6, true, None, None)));

    // 3. Inclusion by group_id only
    let by_group = TypeListInfo {
        included_group_ids: vec![20],
        ..Default::default()
    };
    assert!(by_group.contains(&base_ty));
    assert!(!by_group.contains(&sample_inv_type(100, 21, 6, true, None, None)));

    // 4. Inclusion by category_id only
    let by_cat = TypeListInfo {
        included_category_ids: vec![6],
        ..Default::default()
    };
    assert!(by_cat.contains(&base_ty));
    assert!(!by_cat.contains(&sample_inv_type(100, 20, 7, true, None, None)));

    // 5. Exclusion by type_id overrides category/group inclusion
    let excl_type = TypeListInfo {
        included_category_ids: vec![6],
        excluded_type_ids: vec![100],
        ..Default::default()
    };
    assert!(!excl_type.contains(&base_ty));
    assert!(excl_type.contains(&sample_inv_type(101, 20, 6, true, None, None)));

    // 6. Exclusion by group_id overrides category/type inclusion
    let excl_group = TypeListInfo {
        included_category_ids: vec![6],
        excluded_group_ids: vec![20],
        ..Default::default()
    };
    assert!(!excl_group.contains(&base_ty));
    assert!(excl_group.contains(&sample_inv_type(100, 21, 6, true, None, None)));

    // 7. Exclusion by category_id overrides type/group inclusion
    let excl_cat = TypeListInfo {
        included_type_ids: vec![100],
        excluded_category_ids: vec![6],
        ..Default::default()
    };
    assert!(!excl_cat.contains(&base_ty));
}

#[test]
fn test_sde_registry_market_groups_variations_show_info_and_accessors() {
    let mut sde = SdeRegistry::default();

    // Populate types: parent 500, child variations 501 and 502, unpublished 503, non-market 504
    sde.types
        .insert(500, sample_inv_type(500, 25, 6, true, Some(10), None));
    sde.types
        .insert(501, sample_inv_type(501, 25, 6, true, Some(10), Some(500)));
    sde.types
        .insert(502, sample_inv_type(502, 25, 6, true, Some(20), Some(500)));
    sde.types
        .insert(503, sample_inv_type(503, 25, 6, false, Some(10), None));
    sde.types
        .insert(504, sample_inv_type(504, 25, 6, true, None, None));
    sde.variations_by_parent.insert(500, vec![501, 502]);

    // variations_of
    assert_eq!(sde.variations_of(500), &[501, 502]);
    assert!(sde.variations_of(999_999).is_empty());

    // types_in_market_group filters by published AND exact market_group_id
    assert_eq!(sde.types_in_market_group(10), vec![500, 501]);
    assert_eq!(sde.types_in_market_group(20), vec![502]);
    assert!(sde.types_in_market_group(99).is_empty());

    // published_market_types excludes unpublished (503) and market_group_id == None (504)
    assert_eq!(sde.published_market_types(), vec![500, 501, 502]);

    // market_groups_for_parent sorts by market_group_id and filters by parent_group_id
    sde.market_groups.insert(
        20,
        MarketGroupInfo {
            market_group_id: 20,
            parent_group_id: Some(10),
            name: "Child B".into(),
            has_types: true,
            icon_id: None,
        },
    );
    sde.market_groups.insert(
        15,
        MarketGroupInfo {
            market_group_id: 15,
            parent_group_id: Some(10),
            name: "Child A".into(),
            has_types: true,
            icon_id: Some(5),
        },
    );
    sde.market_groups.insert(
        10,
        MarketGroupInfo {
            market_group_id: 10,
            parent_group_id: None,
            name: "Root".into(),
            has_types: false,
            icon_id: None,
        },
    );
    let roots = sde.market_groups_for_parent(None);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].market_group_id, 10);
    let children = sde.market_groups_for_parent(Some(10));
    assert_eq!(
        children.iter().map(|c| c.market_group_id).collect::<Vec<_>>(),
        vec![15, 20]
    );
    assert!(sde.market_groups_for_parent(Some(999)).is_empty());

    // show_info_for_type for parent (500), child variation (501), and missing (999)
    assert!(sde.show_info_for_type(999).is_none());
    let parent_info = sde.show_info_for_type(500).unwrap();
    assert_eq!(parent_info.variations, vec![501, 502]);
    assert_eq!(parent_info.variation_parent_type_id, None);

    let child_info = sde.show_info_for_type(501).unwrap();
    assert_eq!(child_info.variations, vec![500, 502]);
    assert_eq!(child_info.variation_parent_type_id, Some(500));

    // Type / category / material accessors
    assert_eq!(sde.get_type_category(500), Some(6));
    assert_eq!(sde.get_type_category(999), None);
    assert!(sde.type_materials_for(500).is_empty());
    assert!(sde.randomized_type_materials_for(500).is_empty());

    sde.randomized_type_materials.insert(
        500,
        vec![RandomizedMaterialEntry {
            material_type_id: 34,
            quantity_min: 100,
            quantity_max: 200,
        }],
    );
    assert_eq!(sde.randomized_type_materials_for(500).len(), 1);

    // Serde defaults for InvType::portion_size (1) and StationInfo::has_clone_bay (true)
    let st: StationInfo = serde_json::from_str(
        r#"{"station_id":60000004,"station_type_id":1529,"name":"Test Station","position":[0.0,0.0,0.0]}"#,
    )
    .expect("deserialize StationInfo with default_has_clone_bay");
    assert!(st.has_clone_bay);
    assert_eq!(default_portion_size(), 1);
    assert!(default_has_clone_bay());
}

#[test]
fn test_type_list_info_contains_exhaustive_matrix_and_boundary_ids() {
    let base = sample_inv_type(500, 25, 6, true, None, None);

    // Check every include x exclude combination to kill any `||` / `&&` / `!` mutations
    for (incl_t, incl_g, incl_c) in [
        (vec![500], vec![], vec![]),
        (vec![], vec![25], vec![]),
        (vec![], vec![], vec![6]),
    ] {
        // Without matching exclusion: matches base, misses off-by-one neighbors
        let allow = TypeListInfo {
            type_list_id: 1,
            name: "allow".into(),
            included_type_ids: incl_t.clone(),
            included_group_ids: incl_g.clone(),
            included_category_ids: incl_c.clone(),
            ..Default::default()
        };
        assert!(allow.contains(&base));
        assert!(!allow.contains(&sample_inv_type(501, 26, 7, true, None, None)));

        // Every exclusion dimension overrides any inclusion dimension
        for (excl_t, excl_g, excl_c) in [
            (vec![500], vec![], vec![]),
            (vec![], vec![25], vec![]),
            (vec![], vec![], vec![6]),
        ] {
            let deny = TypeListInfo {
                type_list_id: 2,
                name: "deny".into(),
                included_type_ids: incl_t.clone(),
                included_group_ids: incl_g.clone(),
                included_category_ids: incl_c.clone(),
                excluded_type_ids: excl_t,
                excluded_group_ids: excl_g,
                excluded_category_ids: excl_c,
            };
            assert!(!deny.contains(&base));
        }

        // Non-matching exclusions do NOT block inclusion
        let non_matching_excl = TypeListInfo {
            type_list_id: 3,
            name: "allow_with_other_excl".into(),
            included_type_ids: incl_t,
            included_group_ids: incl_g,
            included_category_ids: incl_c,
            excluded_type_ids: vec![501],
            excluded_group_ids: vec![26],
            excluded_category_ids: vec![7],
        };
        assert!(non_matching_excl.contains(&base));
    }
}

#[test]
fn test_show_info_for_type_dedup_sorting_and_full_field_fidelity() {
    let mut sde = SdeRegistry::default();

    // Parent ID (900) > child ID (705) with duplicates and self inside variations_by_parent
    let parent = InvType {
        type_id: 900,
        group_id: 25,
        category_id: 6,
        name: "Parent Hull".into(),
        mass: 500_000.0,
        volume: 20_000.0,
        capacity: 150.0,
        radius: 30.0,
        base_price: 500_000.0,
        published: true,
        portion_size: 1,
        market_group_id: Some(10),
        race_id: Some(1),
        faction_id: None,
        variation_parent_type_id: None,
        description_id: Some(101),
        icon_id: Some(102),
        graphic_id: Some(103),
        sound_id: Some(104),
        meta_group_id: Some(1),
        meta_level: Some(0),
        tech_level: Some(1),
        default_effect_id: Some(16),
        description: "Parent desc".into(),
        effect_ids: vec![16],
        default_attributes: HashMap::from([(attr_ids::MASS, 500_000.0)]),
    };
    let child = InvType {
        type_id: 705,
        group_id: 26,
        category_id: 6,
        name: "Variant Hull".into(),
        mass: 1_234_567.5,
        volume: 25_000.25,
        capacity: 410.5,
        radius: 42.0,
        base_price: 9_876_543.0,
        published: false,
        portion_size: 5,
        market_group_id: Some(42),
        race_id: Some(4),
        faction_id: Some(500_002),
        variation_parent_type_id: Some(900),
        description_id: Some(777),
        icon_id: Some(888),
        graphic_id: Some(999),
        sound_id: Some(1_111),
        meta_group_id: Some(2),
        meta_level: Some(5),
        tech_level: Some(2),
        default_effect_id: Some(99),
        description: "Custom ship description".into(),
        effect_ids: vec![16, 99],
        default_attributes: HashMap::from([(attr_ids::MAX_VELOCITY, 395.0), (attr_ids::MASS, 1_234_567.5)]),
    };
    let orphan_child = sample_inv_type(706, 26, 6, true, Some(42), Some(9999));

    sde.types.insert(900, parent);
    sde.types.insert(705, child.clone());
    sde.types.insert(706, orphan_child);
    // Include parent (900), self (705), duplicates (720, 720), and unsorted siblings
    sde.variations_by_parent.insert(900, vec![900, 720, 705, 710, 720, 708]);
    // Unrelated parent with duplicates
    sde.types.insert(800, sample_inv_type(800, 25, 6, true, None, None));
    sde.variations_by_parent.insert(800, vec![820, 810, 820]);

    let child_info = sde.show_info_for_type(705).expect("child show info");
    assert_eq!(child_info.variations, vec![708, 710, 720, 900]);
    assert_eq!(child_info.type_id, 705);
    assert_eq!(child_info.name, "Variant Hull");
    assert_eq!(child_info.group_id, 26);
    assert_eq!(child_info.category_id, 6);
    assert_eq!(child_info.mass, 1_234_567.5);
    assert_eq!(child_info.volume, 25_000.25);
    assert_eq!(child_info.capacity, 410.5);
    assert_eq!(child_info.portion_size, 5);
    assert_eq!(child_info.base_price, 9_876_543.0);
    assert!(!child_info.published);
    assert_eq!(child_info.market_group_id, Some(42));
    assert_eq!(child_info.race_id, Some(4));
    assert_eq!(child_info.faction_id, Some(500_002));
    assert_eq!(child_info.variation_parent_type_id, Some(900));
    assert_eq!(child_info.description_id, Some(777));
    assert_eq!(child_info.icon_id, Some(888));
    assert_eq!(child_info.graphic_id, Some(999));
    assert_eq!(child_info.sound_id, Some(1_111));
    assert_eq!(child_info.meta_group_id, Some(2));
    assert_eq!(child_info.meta_level, Some(5));
    assert_eq!(child_info.tech_level, Some(2));
    assert_eq!(child_info.description, "Custom ship description");
    assert_eq!(child_info.default_attributes, child.default_attributes);
    assert_eq!(child_info.effect_ids, vec![16, 99]);

    let parent_info = sde.show_info_for_type(800).expect("parent show info");
    assert_eq!(parent_info.variations, vec![810, 820]);

    let orphan_info = sde.show_info_for_type(706).expect("orphan child show info");
    assert_eq!(orphan_info.variations, vec![9999]);
}

#[test]
fn test_sde_registry_all_accessors_sorting_errors_and_serde_defaults() {
    let mut sde = SdeRegistry::default();

    // Check default_db_path() honors EVOMU_SDE_PATH if non-empty
    let db_path = SdeRegistry::default_db_path();
    assert!(!db_path.as_os_str().is_empty());
    if let Ok(env_p) = std::env::var("EVOMU_SDE_PATH") {
        if !env_p.trim().is_empty() {
            assert_eq!(db_path, PathBuf::from(env_p));
        }
    }

    // Multiple market roots + children -> verify sorted ordering
    for (id, parent) in [(30, None), (10, None), (20, None), (109, Some(10)), (101, Some(10)), (105, Some(10))] {
        sde.market_groups.insert(
            id,
            MarketGroupInfo {
                market_group_id: id,
                parent_group_id: parent,
                name: format!("MG_{id}"),
                has_types: parent.is_some(),
                icon_id: None,
            },
        );
    }
    assert_eq!(
        sde.market_groups_for_parent(None)
            .into_iter()
            .map(|m| m.market_group_id)
            .collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
    assert_eq!(
        sde.market_groups_for_parent(Some(10))
            .into_iter()
            .map(|m| m.market_group_id)
            .collect::<Vec<_>>(),
        vec![101, 105, 109]
    );

    // Market types filtering + sorting
    sde.types.insert(910, sample_inv_type(910, 25, 6, true, Some(42), None));
    sde.types.insert(901, sample_inv_type(901, 25, 6, true, Some(42), None));
    sde.types.insert(905, sample_inv_type(905, 25, 6, true, Some(42), None));
    sde.types.insert(902, sample_inv_type(902, 25, 6, false, Some(42), None));
    sde.types.insert(903, sample_inv_type(903, 25, 6, true, Some(43), None));
    sde.types.insert(904, sample_inv_type(904, 25, 6, true, None, None));
    assert_eq!(sde.types_in_market_group(42), vec![901, 905, 910]);
    assert_eq!(sde.published_market_types(), vec![901, 903, 905, 910]);

    // Populate every table backing SdeRegistry one-liner accessors
    sde.categories.insert(
        6,
        CategoryInfo { category_id: 6, name: "Ship".into(), published: true },
    );
    sde.groups.insert(
        25,
        GroupInfo {
            group_id: 25,
            category_id: 6,
            name: "Frigate".into(),
            anchorable: false,
            anchored: false,
            fittable_non_singleton: false,
            use_base_price: true,
            published: true,
        },
    );
    sde.attributes.insert(
        attr_ids::MAX_VELOCITY,
        DogmaAttributeType {
            attribute_id: attr_ids::MAX_VELOCITY,
            name: "maxVelocity".into(),
            default_value: 0.0,
            high_is_good: true,
            stackable: false,
            published: true,
        },
    );
    sde.effects.insert(
        16,
        DogmaEffectType {
            effect_id: 16,
            name: "online".into(),
            effect_category: 4,
            ..Default::default()
        },
    );
    sde.type_materials.insert(
        901,
        vec![TypeMaterialEntry { material_type_id: 34, quantity: 500 }],
    );
    sde.blueprints.insert(
        999,
        BlueprintInfo {
            blueprint_type_id: 999,
            product_type_id: 901,
            product_quantity: 1,
            manufacturing_time_sec: 120,
            max_production_limit: 10,
            research_me_time_sec: 60,
            research_te_time_sec: 60,
            copy_time_sec: 60,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![],
            activities: HashMap::new(),
        },
    );
    sde.solar_systems.insert(
        30_000_142,
        SolarSystem {
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Jita".into(),
            security_status: 0.946,
            radius: 1.0e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    sde.jumps.insert(
        50_001_249,
        StargateJump {
            stargate_id: 50_001_249,
            from_solar_system_id: 30_000_142,
            to_solar_system_id: 30_000_144,
            destination_stargate_id: 50_002_185,
            position: DVec3::new(1.0, 2.0, 3.0),
        },
    );
    sde.stations.insert(
        60_003_760,
        StationInfo {
            station_id: 60_003_760,
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            station_type_id: 1529,
            name: "Jita 4-4".into(),
            position: DVec3::ZERO,
            owner_corporation_id: 1_000_035,
            owner_faction_id: Some(500_001),
            operation_id: 14,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    sde.agents.insert(
        3_008_416,
        AgentInfo {
            agent_id: 3_008_416,
            name: "Test Agent".into(),
            corporation_id: 1_000_002,
            station_id: Some(60_003_760),
            solar_system_id: Some(30_000_142),
            level: 4,
            division_id: 24,
            agent_type_id: 2,
            is_locator: true,
            research_skills: vec![],
        },
    );
    sde.planet_schematics.insert(
        65,
        PlanetSchematicInfo {
            schematic_id: 65,
            name: "Robotics".into(),
            cycle_time: 3600,
            pins: vec![2470],
            types: vec![],
        },
    );
    sde.schools.insert(
        17,
        SchoolInfo {
            school_id: 17,
            race_id: 1,
            corporation_id: 1_000_167,
            station_id: 60_003_760,
            solar_system_id: 30_000_142,
            name: "State War Academy".into(),
        },
    );
    sde.races.insert(
        1,
        RaceInfo {
            race_id: 1,
            name: "Caldari".into(),
            corvette_type_id: Some(601),
            starter_skills: HashMap::new(),
            description: String::new(),
        },
    );
    sde.bloodlines.insert(
        1,
        BloodlineInfo {
            bloodline_id: 1,
            race_id: 1,
            name: "Deteis".into(),
            corporation_id: 1_000_006,
            ship_type_id: Some(601),
            charisma: 5,
            intelligence: 8,
            memory: 7,
            perception: 6,
            willpower: 7,
            description: String::new(),
        },
    );
    sde.ancestries.insert(
        1,
        AncestryInfo {
            ancestry_id: 1,
            bloodline_id: 1,
            name: "Tube Child".into(),
            charisma: 1,
            intelligence: 2,
            memory: 2,
            perception: 1,
            willpower: 1,
            description: String::new(),
            short_description: String::new(),
        },
    );
    sde.factions.insert(
        500_001,
        FactionInfo {
            faction_id: 500_001,
            name: "Caldari State".into(),
            corporation_id: Some(1_000_035),
            militia_corporation_id: Some(1_000_180),
            solar_system_id: Some(30_000_142),
            size_factor: 5.0,
            description: String::new(),
        },
    );
    sde.npc_corporations.insert(
        1_000_035,
        NpcCorporationInfo {
            corporation_id: 1_000_035,
            name: "Caldari Navy".into(),
            ticker_name: "CN".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.1,
        },
    );

    // Hit checks for all 23 accessors
    assert_eq!(sde.get_type(901).unwrap().name, "Type_901");
    assert!(sde.get_type(88_888).is_none());
    assert_eq!(sde.get_type_category(901), Some(6));
    assert_eq!(sde.get_type_category(88_888), None);
    assert_eq!(sde.group(25).unwrap().name, "Frigate");
    assert!(sde.group(88_888).is_none());
    assert_eq!(sde.category(6).unwrap().name, "Ship");
    assert!(sde.category(88_888).is_none());
    assert_eq!(sde.dogma_attributes().len(), 1);
    assert_eq!(sde.dogma_attribute(attr_ids::MAX_VELOCITY).unwrap().name, "maxVelocity");
    assert!(sde.dogma_attribute(9999).is_none());
    assert_eq!(sde.dogma_effects().len(), 1);
    assert_eq!(sde.dogma_effect(16).unwrap().name, "online");
    assert!(sde.dogma_effect(9999).is_none());
    assert_eq!(
        sde.type_materials_for(901),
        &[TypeMaterialEntry { material_type_id: 34, quantity: 500 }]
    );
    assert!(sde.type_materials_for(88_888).is_empty());
    assert_eq!(sde.blueprint(999).unwrap().product_type_id, 901);
    assert!(sde.blueprint(88_888).is_none());
    assert_eq!(sde.solar_system(30_000_142).unwrap().name, "Jita");
    assert!(sde.solar_system(88_888).is_none());
    assert_eq!(sde.stargates().len(), 1);
    assert_eq!(sde.stargate(50_001_249).unwrap().to_solar_system_id, 30_000_144);
    assert!(sde.stargate(88_888).is_none());
    assert_eq!(sde.station(60_003_760).unwrap().name, "Jita 4-4");
    assert!(sde.station(88_888).is_none());
    assert_eq!(sde.agent(3_008_416).unwrap().name, "Test Agent");
    assert!(sde.agent(88_888).is_none());
    assert_eq!(sde.planet_schematic(65).unwrap().name, "Robotics");
    assert!(sde.planet_schematic(88_888).is_none());
    assert_eq!(sde.school(17).unwrap().name, "State War Academy");
    assert!(sde.school(88_888).is_none());
    assert_eq!(sde.race(1).unwrap().name, "Caldari");
    assert!(sde.race(88_888).is_none());
    assert_eq!(sde.bloodline(1).unwrap().name, "Deteis");
    assert!(sde.bloodline(88_888).is_none());
    assert_eq!(sde.ancestry(1).unwrap().name, "Tube Child");
    assert!(sde.ancestry(88_888).is_none());
    assert_eq!(sde.faction(500_001).unwrap().name, "Caldari State");
    assert!(sde.faction(88_888).is_none());
    assert_eq!(sde.npc_corporation(1_000_035).unwrap().ticker_name, "CN");
    assert!(sde.npc_corporation(88_888).is_none());

    // Serde defaults and aliases (portion_size, CloneSkillCap, BlueprintMaterial/Product/Skill, RawModifier)
    let raw_inv: InvType = serde_json::from_str(
        r#"{"type_id":1,"group_id":2,"category_id":3,"name":"Minimal","mass":1.0,"volume":1.0,"capacity":0.0,"radius":1.0,"base_price":0.0,"published":true,"default_attributes":{}}"#,
    )
    .expect("minimal InvType defaults");
    assert_eq!(raw_inv.portion_size, 1);

    let bp_mat: BlueprintMaterial = serde_json::from_str(r#"{"typeID":34,"quantity":10}"#).unwrap();
    let bp_prod: BlueprintProduct = serde_json::from_str(r#"{"typeID":587,"quantity":1,"probability":0.25}"#).unwrap();
    let bp_skill: BlueprintSkill = serde_json::from_str(r#"{"typeID":3300,"level":3}"#).unwrap();
    assert_eq!(bp_mat.type_id, 34);
    assert_eq!(bp_prod.probability, Some(0.25));
    assert_eq!(bp_skill.level, 3);

    let cap1: RawCloneSkillCap = serde_json::from_str(r#"{"typeID":3300,"level":3}"#).unwrap();
    let cap2: RawCloneSkillCap = serde_json::from_str(r#"{"typeID":3301,"maxTrainingLevel":4}"#).unwrap();
    assert_eq!(cap1.type_id, 3300);
    assert_eq!(cap1.level, 3);
    assert_eq!(cap2.level, 4);

    let rm: RawModifier = serde_json::from_str(r#"{"domain":"shipID","operator":6}"#).unwrap();
    assert_eq!(rm.domain.as_deref(), Some("shipID"));
    assert_eq!(rm.operator, Some(6));
    let muta: RawMutaMapping = serde_json::from_str(r#"{"resulting_type_id":49710,"applicable_type_ids":[12058]}"#).unwrap();
    assert_eq!(muta.resulting_type_id, 49710);
    assert_eq!(muta.applicable_type_ids, vec![12058]);

    // SdeError formatting
    assert!(SdeError::MissingDatabase("x.sqlite".into()).to_string().contains("x.sqlite"));
    assert!(SdeError::InvalidSchema("bad col".into()).to_string().contains("bad col"));
    let json_err: SdeError = serde_json::from_str::<InvType>("{").unwrap_err().into();
    assert!(json_err.to_string().contains("JSON"));
    let sql_err: SdeError = rusqlite::Error::QueryReturnedNoRows.into();
    assert!(sql_err.to_string().contains("SQLite"));
}

#[test]
fn salvage_pools_wreck_resolution_celestials_and_industry_helpers() {
    let mut sde = SdeRegistry::default();
    sde.type_isis_groups.insert(587, 8);
    assert_eq!(sde.type_isis_group_id(587), Some(8));
    assert_eq!(sde.type_isis_group_id(999), None);

    // 1. SalvageMaterialPools::from_types_and_blueprints (both blueprint-driven and synthetic fallback)
    let mut types = HashMap::new();
    for tid in [25595u32, 25596, 25597, 25598] {
        let mut t = sample_inv_type(tid, 754, 4, true, None, None);
        t.race_id = None;
        types.insert(tid, t);
    }
    let mut anc = sample_inv_type(30001, 966, 4, true, None, None);
    anc.default_attributes.insert(161, 1.0);
    types.insert(30001, anc);
    let mut anc_empty = sample_inv_type(30002, 966, 4, true, None, None);
    anc_empty.default_attributes.clear(); // empty attrs -> skipped
    types.insert(30002, anc_empty);

    let pools_fallback = SalvageMaterialPools::from_types_and_blueprints(&types, &HashMap::new());
    assert_eq!(pools_fallback.t1_standard, vec![25595, 25596]);
    assert_eq!(pools_fallback.t2_intact, vec![25597, 25598]);
    assert_eq!(pools_fallback.ancient, vec![30001]);

    // Blueprint-driven partitioning
    let mut t1_rig = sample_inv_type(31001, 773, 7, true, None, None);
    t1_rig.tech_level = Some(1);
    t1_rig.meta_group_id = Some(1);
    types.insert(31001, t1_rig);
    let mut t2_rig = sample_inv_type(31002, 773, 7, true, None, None);
    t2_rig.tech_level = Some(2);
    t2_rig.meta_group_id = Some(2);
    types.insert(31002, t2_rig);
    let mut bps = HashMap::new();
    bps.insert(
        32001,
        BlueprintInfo {
            blueprint_type_id: 32001,
            product_type_id: 31001,
            product_quantity: 1,
            manufacturing_time_sec: 60,
            max_production_limit: 10,
            research_me_time_sec: 0,
            research_te_time_sec: 0,
            copy_time_sec: 0,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![
                BlueprintMaterial { type_id: 25595, quantity: 5 },
                BlueprintMaterial { type_id: 30001, quantity: 1 },
            ],
            activities: HashMap::new(),
        },
    );
    bps.insert(
        32002,
        BlueprintInfo {
            blueprint_type_id: 32002,
            product_type_id: 31002,
            product_quantity: 1,
            manufacturing_time_sec: 60,
            max_production_limit: 10,
            research_me_time_sec: 0,
            research_te_time_sec: 0,
            copy_time_sec: 0,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![
                BlueprintMaterial { type_id: 25595, quantity: 2 },
                BlueprintMaterial { type_id: 25598, quantity: 3 },
            ],
            activities: HashMap::new(),
        },
    );
    let pools_bp = SalvageMaterialPools::from_types_and_blueprints(&types, &bps);
    assert_eq!(pools_bp.t1_standard, vec![25595]);
    assert_eq!(pools_bp.t2_intact, vec![25598]);
    assert_eq!(pools_bp.ancient, vec![30001]);

    sde.types = types;
    sde.salvage_pools = Arc::new(pools_fallback);

    // 2. resolve_wreck_type_id across race_override, direct wrecks, faction->race fallback, and group default
    let mut victim = sample_inv_type(5001, 25, 6, true, None, None);
    victim.race_id = None;
    victim.faction_id = Some(500_010); // Guristas -> race 32
    sde.types.insert(5001, victim);
    let mut wrecks_map = HashMap::new();
    wrecks_map.insert(
        25,
        HashMap::from([
            (1u32, 26551u32),
            (2u32, 26553u32),
            (4u32, 26555u32),
            (8u32, 26557u32),
            (32u32, 26901u32),
            (64u32, 26902u32),
            (128u32, 26903u32),
            (135u32, 26904u32),
            (0u32, 26500u32),
        ]),
    );
    wrecks_map.insert(26, HashMap::from([(99u32, 26999u32)]));
    wrecks_map.insert(27, HashMap::from([(1u32, 26888u32), (2u32, 26889u32)]));
    sde.wrecks_by_group_and_race = Arc::new(wrecks_map);
    assert_eq!(sde.resolve_wreck_type_id(5001, Some(2)), Some(26553));
    assert_eq!(sde.resolve_wreck_type_id(5001, None), Some(26901));
    for (fid, expected_wid) in [
        (500_001, 26551),
        (500_002, 26553),
        (500_003, 26555),
        (500_007, 26555),
        (500_016, 26555),
        (500_004, 26557),
        (500_009, 26557),
        (500_011, 26901),
        (500_012, 26901),
        (500_019, 26901),
        (500_020, 26901),
        (500_014, 26903),
        (500_018, 26902),
        (500_021, 26902),
        (500_026, 26904),
        (999_999, 26500), // unmapped faction -> falls back to race 0
    ] {
        sde.types.get_mut(&5001).unwrap().faction_id = Some(fid);
        assert_eq!(sde.resolve_wreck_type_id(5001, None), Some(expected_wid));
    }
    // Single-entry race_map fallback (`race_map.len() == 1`) vs multi-entry without race 0 (`None`)
    let mut v26 = sample_inv_type(5002, 26, 6, true, None, None);
    v26.race_id = None;
    v26.faction_id = None;
    sde.types.insert(5002, v26);
    assert_eq!(sde.resolve_wreck_type_id(5002, None), Some(26999));
    let mut v27 = sample_inv_type(5003, 27, 6, true, None, None);
    v27.race_id = None;
    v27.faction_id = None;
    sde.types.insert(5003, v27);
    assert_eq!(sde.resolve_wreck_type_id(5003, None), None);
    sde.wrecks.insert(5001, 26599);
    assert_eq!(sde.resolve_wreck_type_id(5001, None), Some(26599));

    // solar_system_faction, stargate_type_and_name, wreck_group_and_race
    Arc::make_mut(&mut sde.solar_system_factions).insert(30_000_142, 500_001);
    assert_eq!(sde.solar_system_faction(30_000_142), Some(500_001));
    assert_eq!(sde.solar_system_faction(30_000_999), None);
    Arc::make_mut(&mut sde.stargate_meta).insert(50_000_001, (16, "Stargate (Perimeter)".into()));
    assert_eq!(
        sde.stargate_type_and_name(50_000_001),
        Some((16, "Stargate (Perimeter)"))
    );
    assert_eq!(sde.stargate_type_and_name(50_000_999), None);
    Arc::make_mut(&mut sde.wreck_group_race_by_wreck_type).insert(26800, (25, 64));
    assert_eq!(sde.wreck_group_and_race(26800), Some((25, 64)));
    assert_eq!(sde.wreck_group_and_race(99999), None);

    // salvage_materials_for_wreck across ancient, T2, and T1 wrecks
    let mut w_anc = sample_inv_type(26800, 186, 2, true, None, Some(64));
    w_anc.default_attributes.insert(901, -20.0);
    sde.types.insert(26800, w_anc);
    let mut w_t2 = sample_inv_type(26801, 186, 2, true, None, Some(1));
    w_t2.default_attributes.insert(901, -10.0);
    sde.types.insert(26801, w_t2);
    let mut w_t1 = sample_inv_type(26802, 186, 2, true, None, Some(2));
    w_t1.default_attributes.insert(901, 5.0);
    sde.types.insert(26802, w_t1);
    assert_eq!(sde.salvage_materials_for_wreck(26800, true), vec![(30001, 2)]);
    assert!(!sde.salvage_materials_for_wreck(26801, true).is_empty());
    assert!(!sde.salvage_materials_for_wreck(26802, true).is_empty());

    // Multi-element pools to distinguish every racial index arm in T2 and T1 + difficulty threshold (<= 10.0)
    sde.salvage_pools = Arc::new(SalvageMaterialPools {
        t1_standard: (25500..25520).collect(),
        t2_intact: (25600..25620).collect(),
        ancient: vec![30001, 30002, 30003],
    });
    assert_eq!(
        sde.salvage_materials_for_wreck(26800, true),
        vec![(30003, 2), (30002, 3)]
    );
    for (rid, exp) in [
        (1u32, vec![(25600, 2), (25601, 3)]),
        (2, vec![(25607, 2), (25614, 3)]),
        (4, vec![(25609, 2), (25617, 3)]),
        (8, vec![(25608, 2), (25609, 3)]),
        (32, vec![(25609, 2), (25615, 3)]),
    ] {
        sde.types.get_mut(&26801).unwrap().race_id = Some(rid);
        assert_eq!(sde.salvage_materials_for_wreck(26801, true), exp);
    }
    for (rid, exp) in [
        (1u32, vec![(25500, 8), (25501, 4), (25512, 6)]),
        (2, vec![(25507, 8), (25505, 4), (25514, 6)]),
        (4, vec![(25500, 8), (25517, 4), (25515, 6)]),
        (8, vec![(25508, 8), (25509, 4), (25517, 6)]),
        (32, vec![(25501, 8), (25507, 4), (25512, 6)]),
        (128, vec![(25500, 8), (25507, 4), (25509, 6)]),
    ] {
        sde.types.get_mut(&26802).unwrap().race_id = Some(rid);
        sde.types.get_mut(&26802).unwrap().default_attributes.insert(901, 10.0);
        assert_eq!(sde.salvage_materials_for_wreck(26802, true), exp);
    }
    // `scale_by_difficulty = false` or `wreck_diff > 10.0` uses `size_mult = 1`
    assert_eq!(
        sde.salvage_materials_for_wreck(26802, false),
        vec![(25500, 4), (25507, 2), (25509, 3)]
    );
    sde.types.get_mut(&26802).unwrap().default_attributes.insert(901, 10.5);
    assert_eq!(
        sde.salvage_materials_for_wreck(26802, true),
        vec![(25500, 4), (25507, 2), (25509, 3)]
    );

    // 3. insert_celestial, celestial, celestials_in_system_slice, insert_star, star
    let c1 = CelestialInfo {
        celestial_id: 40_000_002,
        solar_system_id: 30_000_142,
        type_id: 11,
        group_id: 7,
        name: "Jita IV".into(),
        radius: 5000.0,
        position: DVec3::ZERO,
        orbit_id: None,
        orbit_index: Some(4),
        celestial_index: Some(4),
    };
    sde.insert_celestial(c1.clone());
    assert_eq!(sde.celestial_count, 1);
    assert_eq!(sde.celestial(40_000_002).unwrap().name, "Jita IV");
    assert_eq!(sde.celestials_in_system_slice(30_000_142).len(), 1);
    // Re-inserting into a different system moves the celestial without double-counting
    let mut c1_moved = c1;
    c1_moved.solar_system_id = 30_000_144;
    sde.insert_celestial(c1_moved);
    assert_eq!(sde.celestial_count, 1);
    assert!(sde.celestials_in_system_slice(30_000_142).is_empty());
    assert_eq!(sde.celestials_in_system_slice(30_000_144).len(), 1);

    sde.insert_star(StarInfo {
        star_id: 40_000_001,
        solar_system_id: 30_000_142,
        type_id: 45041,
        radius: 100_000,
        age: 1_000,
        luminosity: 0.5,
        temperature: 5000,
        spectral_class: "G2 V".into(),
    });
    assert_eq!(sde.star(40_000_001).unwrap().spectral_class, "G2 V");

    // 4. industry_activity_key, industry_target_filter_matches, rig_dogma_source_attribute, installation_assembly_lines_for_activity
    assert_eq!(sde.industry_activity_key(1), Some("manufacturing"));
    assert_eq!(sde.industry_activity_key(11), Some("reaction"));
    assert_eq!(sde.industry_activity_key(99), None);

    Arc::make_mut(&mut sde.industry_target_filters).insert(
        10,
        IndustryTargetFilterInfo {
            filter_id: 10,
            name: "Rigs".into(),
            category_ids: vec![7],
            group_ids: vec![],
        },
    );
    assert!(sde.industry_target_filter_matches(None, Some(31001)));
    assert!(sde.industry_target_filter_matches(Some(10), None));
    assert!(sde.industry_target_filter_matches(Some(10), Some(31001)));
    assert!(!sde.industry_target_filter_matches(Some(10), Some(25595)));
    assert!(!sde.industry_target_filter_matches(Some(999), Some(31001)));

    let mut rig = sample_inv_type(37100, 1816, 66, true, None, None);
    rig.default_attributes.insert(2594, -2.0);
    rig.effect_ids.push(6800);
    sde.effects.insert(
        6800,
        DogmaEffectType {
            effect_id: 6800,
            name: "rig_me".into(),
            modifiers: vec![EffectModifierSpec {
                domain: "structureID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 2546,
                modifying_attribute_id: 2594,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    assert_eq!(sde.rig_dogma_source_attribute(&rig, None, 2594), Some(-2.0));
    assert_eq!(sde.rig_dogma_source_attribute(&rig, None, 2546), Some(-2.0));
    assert_eq!(sde.rig_dogma_source_attribute(&rig, None, 9999), None);

    Arc::make_mut(&mut sde.industry_assembly_lines).insert(
        5,
        IndustryAssemblyLineInfo {
            id: 5,
            activity: 1,
            name: "Mfg Line".into(),
            base_time_multiplier: 1.0,
            base_material_multiplier: 1.0,
            base_cost_multiplier: 1.0,
            details_per_group: vec![],
            details_per_category: vec![],
        },
    );
    Arc::make_mut(&mut sde.industry_installation_types).insert(35825, vec![5]);
    assert_eq!(sde.installation_assembly_lines_for_activity(35825, 1).len(), 1);
    assert!(sde.installation_assembly_lines_for_activity(35825, 8).is_empty());
    assert!(sde.installation_assembly_lines_for_activity(99999, 1).is_empty());
}

#[test]
fn sde_mutant_killer_salvage_wreck_celestial_faction_and_activity_edges() {
    // 1. SalvageMaterialPools::from_types_and_blueprints fallback & T2 predicate variants
    let mut types = HashMap::new();
    let m1 = sample_inv_type(25501, 754, 4, true, None, None);
    let mut m1 = m1;
    m1.race_id = None;
    let mut m2 = sample_inv_type(25502, 754, 4, true, None, None);
    m2.race_id = None;
    let mut m3 = sample_inv_type(25503, 754, 4, true, None, None);
    m3.race_id = None;
    let mut m_racial = sample_inv_type(25590, 754, 4, true, None, None);
    m_racial.race_id = Some(1);
    let mut m_unpub_other = sample_inv_type(25599, 18, 4, false, None, None);
    m_unpub_other.race_id = None;

    let mut prod_t2_tech_only = sample_inv_type(31101, 773, 7, true, None, None);
    prod_t2_tech_only.tech_level = Some(2);
    prod_t2_tech_only.meta_group_id = Some(1);
    let mut prod_t2_meta_only = sample_inv_type(31102, 773, 7, true, None, None);
    prod_t2_meta_only.tech_level = Some(1);
    prod_t2_meta_only.meta_group_id = Some(2);
    let mut prod_t1 = sample_inv_type(31103, 773, 7, true, None, None);
    prod_t1.tech_level = Some(1);
    prod_t1.meta_group_id = Some(1);

    for t in [
        m1,
        m2,
        m3,
        m_racial,
        m_unpub_other,
        prod_t2_tech_only,
        prod_t2_meta_only,
        prod_t1,
    ] {
        types.insert(t.type_id, t);
    }

    // 1a. No blueprints: fallback partition excludes racial 754 and unpublished non-754 (kills 915:62)
    let fallback_only = SalvageMaterialPools::from_types_and_blueprints(&types, &HashMap::new());
    assert_eq!(fallback_only.t1_standard, vec![25501]);
    assert_eq!(fallback_only.t2_intact, vec![25502, 25503]);

    // 1b. tech_level == Some(2) OR meta_group_id == Some(2) without both being 2 (kills 892:51)
    let mut bps_both_t2 = HashMap::new();
    let mk_bp = |bp_id: u32, prod_id: u32, mat_id: u32| BlueprintInfo {
        blueprint_type_id: bp_id,
        product_type_id: prod_id,
        product_quantity: 1,
        manufacturing_time_sec: 10,
        max_production_limit: 10,
        research_me_time_sec: 0,
        research_te_time_sec: 0,
        copy_time_sec: 0,
        invention_time_sec: 0,
        invention_base_probability: 0.0,
        materials: vec![BlueprintMaterial {
            type_id: mat_id,
            quantity: 1,
        }],
        activities: HashMap::new(),
    };
    bps_both_t2.insert(32101, mk_bp(32101, 31101, 25501));
    bps_both_t2.insert(32102, mk_bp(32102, 31102, 25502));
    bps_both_t2.insert(32103, mk_bp(32103, 31103, 25503));
    let pools_split_t2 = SalvageMaterialPools::from_types_and_blueprints(&types, &bps_both_t2);
    assert_eq!(pools_split_t2.t1_standard, vec![25503]);
    assert_eq!(pools_split_t2.t2_intact, vec![25501, 25502]);

    // 1c. T1 populated & T2 empty -> keeps blueprint T1 [25503], fills T2 from fallback (kills 912:35 & 920:39)
    let bps_t1_only = HashMap::from([(32103, mk_bp(32103, 31103, 25503))]);
    let pools_t1_only = SalvageMaterialPools::from_types_and_blueprints(&types, &bps_t1_only);
    assert_eq!(pools_t1_only.t1_standard, vec![25503]);
    assert_eq!(pools_t1_only.t2_intact, vec![25502, 25503]);

    // 1d. T2 populated & T1 empty -> keeps blueprint T2 [25501], fills T1 from fallback (kills 912:35 & 925:37)
    let bps_t2_only = HashMap::from([(32101, mk_bp(32101, 31101, 25501))]);
    let pools_t2_only = SalvageMaterialPools::from_types_and_blueprints(&types, &bps_t2_only);
    assert_eq!(pools_t2_only.t1_standard, vec![25501]);
    assert_eq!(pools_t2_only.t2_intact, vec![25501]);

    // 2. solar_system_faction_id filter `f > 0` (kills 1147:93)
    let mut sde = SdeRegistry::default();
    Arc::make_mut(&mut sde.solar_system_factions).insert(30_099_001, 500_005);
    assert_eq!(sde.solar_system_faction_id(30_099_001), Some(500_005));
    Arc::make_mut(&mut sde.solar_system_factions).insert(30_099_002, 0);
    sde.constellations.insert(
        20_099_002,
        ConstellationInfo {
            constellation_id: 20_099_002,
            region_id: 10_099_002,
            name: "C".into(),
            center: [0.0; 3],
            faction_id: Some(500_002),
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_099_002,
        SolarSystem {
            solar_system_id: 30_099_002,
            constellation_id: 20_099_002,
            region_id: 10_099_002,
            name: "ZeroFacSys".into(),
            security_status: 0.5,
            radius: 1.0,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    assert_eq!(sde.solar_system_faction_id(30_099_002), Some(500_002));

    // 3. celestial() unsorted fallback + missing ID inside non-empty list (kills 1276:61)
    let mk_cel = |cid: i64| CelestialInfo {
        celestial_id: cid,
        solar_system_id: 30_000_142,
        type_id: 11,
        group_id: 7,
        name: format!("C_{cid}"),
        radius: 100.0,
        position: DVec3::ZERO,
        orbit_id: None,
        orbit_index: None,
        celestial_index: None,
    };
    Arc::make_mut(&mut sde.celestials_by_system)
        .insert(30_000_142, vec![mk_cel(40_999_002), mk_cel(40_999_001)]);
    Arc::make_mut(&mut sde.celestial_system_by_id).insert(40_999_001, 30_000_142);
    Arc::make_mut(&mut sde.celestial_system_by_id).insert(40_999_002, 30_000_142);
    Arc::make_mut(&mut sde.celestial_system_by_id).insert(40_999_003, 30_000_142);
    assert_eq!(sde.celestial(40_999_001).unwrap().celestial_id, 40_999_001);
    assert!(sde.celestial(40_999_003).is_none());

    // 4. resolve_wreck_type_id `r != 0` filters (kills 1357:66, 1372:28, 1373:49)
    let mut v1 = sample_inv_type(50_001, 25, 6, true, None, None);
    v1.race_id = Some(2);
    v1.faction_id = None;
    let mut v2 = sample_inv_type(50_002, 26, 6, true, None, None);
    v2.race_id = Some(4);
    v2.faction_id = None;
    let mut v3 = sample_inv_type(50_003, 27, 6, true, None, None);
    v3.race_id = Some(0);
    v3.faction_id = Some(500_001); // maps to race 1
    let mut v4 = sample_inv_type(50_004, 27, 6, true, None, None);
    v4.race_id = Some(1);
    v4.faction_id = None;
    for t in [v1, v2, v3, v4] {
        sde.types.insert(t.type_id, t);
    }
    sde.wrecks.insert(50_001, 88_001);
    Arc::make_mut(&mut sde.wrecks_by_group_and_race).insert(
        25,
        HashMap::from([(0, 88_000), (2, 88_002)]),
    );
    Arc::make_mut(&mut sde.wrecks_by_group_and_race).insert(
        26,
        HashMap::from([(0, 88_100), (4, 88_104)]),
    );
    Arc::make_mut(&mut sde.wrecks_by_group_and_race).insert(
        27,
        HashMap::from([(0, 88_200), (1, 88_201), (2, 88_202)]),
    );
    assert_eq!(sde.resolve_wreck_type_id(50_001, Some(0)), Some(88_001));
    assert_eq!(sde.resolve_wreck_type_id(50_001, Some(2)), Some(88_002));
    assert_eq!(sde.resolve_wreck_type_id(50_002, Some(0)), Some(88_104));
    assert_eq!(sde.resolve_wreck_type_id(50_003, None), Some(88_201));
    assert_eq!(sde.resolve_wreck_type_id(50_004, None), Some(88_201));

    // 5. salvage_materials_for_wreck: ancient OR/modulo and 0.0 < 0.0 boundary (kills 1431:27, 1435:35, 1443:23)
    sde.salvage_pools = Arc::new(SalvageMaterialPools {
        t1_standard: (70_000..70_019).collect(),
        t2_intact: (71_000..71_019).collect(),
        ancient: vec![90_001, 90_002, 90_003, 90_004, 90_005],
    });
    let mut sleeper_high_diff = sample_inv_type(60_001, 182, 2, true, None, None);
    sleeper_high_diff.race_id = Some(64);
    sleeper_high_diff.default_attributes.insert(901, 10.0);
    sde.types.insert(60_001, sleeper_high_diff);
    assert_eq!(
        sde.salvage_materials_for_wreck(60_001, true),
        vec![(90_004, 2), (90_003, 3)]
    );

    let mut deep_diff_non_sleeper = sample_inv_type(60_002, 182, 2, true, None, None);
    deep_diff_non_sleeper.race_id = Some(1);
    deep_diff_non_sleeper.default_attributes.insert(901, -15.0);
    sde.types.insert(60_002, deep_diff_non_sleeper);
    assert_eq!(
        sde.salvage_materials_for_wreck(60_002, true),
        vec![(90_004, 2), (90_003, 3)]
    );

    let mut zero_diff_wreck = sample_inv_type(60_003, 182, 2, true, None, None);
    zero_diff_wreck.race_id = Some(1);
    zero_diff_wreck.default_attributes.insert(901, 0.0);
    sde.types.insert(60_003, zero_diff_wreck);
    assert_eq!(
        sde.salvage_materials_for_wreck(60_003, true),
        vec![(70_000, 8), (70_001, 4), (70_012, 6)]
    );

    // 6. industry_activity_key: non-empty activity filter & arms 3, 4, 5, 8 (kills 1498:16, 1504..1507)
    let acts = Arc::make_mut(&mut sde.industry_activities);
    for id in [3u32, 4, 5, 8, 9] {
        acts.insert(
            id,
            IndustryActivityInfo {
                activity_id: id,
                activity_name: format!("Act{id}"),
                description: String::new(),
            },
        );
    }
    assert_eq!(sde.industry_activity_key(1), None);
    assert_eq!(sde.industry_activity_key(3), Some("research_time"));
    assert_eq!(sde.industry_activity_key(4), Some("research_material"));
    assert_eq!(sde.industry_activity_key(5), Some("copying"));
    assert_eq!(sde.industry_activity_key(8), Some("invention"));
    assert_eq!(sde.industry_activity_key(9), Some("reaction"));
    assert_eq!(sde.industry_activity_key(11), Some("reaction"));
}

