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
