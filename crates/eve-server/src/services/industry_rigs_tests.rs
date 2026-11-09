use super::*;
use crate::router::ServerContext;
use crate::services::industry_pi::IndustryJobCostMeta;
use eve_db::{
    CorpGovState, IndustryJobRecord, InventoryFlag, InventoryItemRecord, MarketHistoryDayRecord,
    SystemDynamicState, UpwellStructureRecord, WorldRepository,
};
use eve_sde::{BlueprintActivity, BlueprintInfo, BlueprintMaterial, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 6,
        name: name.into(),
        mass: 1.0,
        volume: 10.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 1000.0,
        published: true,
        portion_size: 1,
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
        default_attributes,
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: true,
        ..Default::default()
    }
}

#[test]
fn structure_rig_bonuses_and_security_multipliers_cover_hs_ls_ns_wh_and_reactions() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        990001,
        make_inv_type(
            990001,
            1934,
            "Standup M-Set Biochemical Reactor I",
            HashMap::from([(2713, -20.0), (2714, -2.0)]),
        ),
    );
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30000142),
            (0.0, 0.0, 0.0, 1.0)
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30002537),
            (0.0, 0.0, 0.0, 1.9)
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759),
            (0.0, 0.0, 0.0, 2.1)
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 31000005),
            (0.0, 0.0, 0.0, 2.1)
        );

        let mut rig_t1 = make_item(5001, 999990, 98000001, 5000, InventoryFlag::RigSlot0);
        rig_t1.dynamic_attributes.insert(2594, -2.0);
        rig_t1.dynamic_attributes.insert(2593, -20.0);
        rig_t1.dynamic_attributes.insert(2595, -10.0);
        snap.items.insert(5001, rig_t1);

        let (mat_hs, time_hs, cost_hs, sec_hs) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30000142);
        assert!((mat_hs - 0.02).abs() < 1e-9);
        assert!((time_hs - 0.20).abs() < 1e-9);
        assert!((cost_hs - 0.10).abs() < 1e-9);
        assert!((sec_hs - 1.0).abs() < 1e-9);

        let (mat_ls, time_ls, sec_ls) = resolve_structure_rig_bonus(&ctx, snap, 5000, 30002537);
        assert!((mat_ls - 0.038).abs() < 1e-9);
        assert!((time_ls - 0.38).abs() < 1e-9);
        assert!((sec_ls - 1.9).abs() < 1e-9);

        let mut rig_t2 = make_item(5002, 999991, 98000001, 5000, InventoryFlag::RigSlot1);
        rig_t2.dynamic_attributes.insert(422, 2.0);
        rig_t2.dynamic_attributes.insert(2594, -2.4);
        rig_t2.dynamic_attributes.insert(2593, -24.0);
        snap.items.insert(5002, rig_t2);
        let (mat_ns, time_ns, _, sec_ns) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759);
        assert!((mat_ns - 0.024 * 2.1).abs() < 1e-9);
        assert!((time_ns - 0.24 * 2.1).abs() < 1e-9);
        assert!((sec_ns - 2.1).abs() < 1e-9);

        snap.items.clear();
        let react_rig = make_item(5003, 990001, 98000001, 5000, InventoryFlag::RigSlot2);
        snap.items.insert(5003, react_rig);
        let (rm_hs, rt_hs, _, rsec_hs) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30000142);
        assert_eq!((rm_hs, rt_hs, rsec_hs), (0.0, 0.0, 0.0));
        let (rm_ls, rt_ls, _, rsec_ls) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30002537);
        assert!((rm_ls - 0.02).abs() < 1e-9);
        assert!((rt_ls - 0.20).abs() < 1e-9);
        assert!((rsec_ls - 1.0).abs() < 1e-9);
        let (rm_ns, rt_ns, _, rsec_ns) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759);
        assert!((rm_ns - 0.022).abs() < 1e-9);
        assert!((rt_ns - 0.22).abs() < 1e-9);
        assert!((rsec_ns - 1.1).abs() < 1e-9);
    });
}

#[test]
fn facility_modifiers_and_adjusted_prices_resolve_correctly() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        35825,
        make_inv_type(
            35825,
            1404,
            "Raitaru",
            HashMap::from([(2600, 0.99), (2602, 0.85), (2601, 0.97)]),
        ),
    );
    ctx.sde = Arc::new(sde);

    assert_eq!(
        structure_sde_industry_bonuses(&ctx, 99999999),
        (1.0, 1.0, 1.0)
    );
    let (m, t, c) = structure_sde_industry_bonuses(&ctx, 35825);
    assert!((m - 0.99).abs() < 1e-9 && (t - 0.85).abs() < 1e-9 && (c - 0.97).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_000_100,
            UpwellStructureRecord {
                structure_id: 1_000_000_000_100,
                type_id: 35825,
                owner_corp_id: 98000001,
                solar_system_id: 30000142,
                name: "Test Raitaru".into(),
                state: "Destroyed".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        assert!(resolve_facility_modifiers_and_system(&ctx, snap, 1_000_000_000_100).is_none());

        snap.structures.get_mut(&1_000_000_000_100).unwrap().state = "ShieldVulnerable".into();
        let resolved =
            resolve_facility_modifiers_and_system(&ctx, snap, 1_000_000_000_100).unwrap();
        assert_eq!(resolved.0, 35825);
        assert_eq!(resolved.1, 98000001);
        assert_eq!(resolved.2, 30000142);

        let item_fac = make_item(
            1_000_000_000_200,
            35825,
            98000002,
            30000142,
            InventoryFlag::Unlocked,
        );
        snap.items.insert(1_000_000_000_200, item_fac);
        let item_res =
            resolve_facility_modifiers_and_system(&ctx, snap, 1_000_000_000_200).unwrap();
        assert_eq!(item_res.0, 35825);
        assert_eq!(item_res.1, 98000002);
        assert_eq!(item_res.2, 30000142);

        assert!(resolve_facility_modifiers_and_system(&ctx, snap, 999_999_999).is_none());
    });

    clear_sde_adjusted_prices();
    set_sde_adjusted_prices(HashMap::from([(34, 5.5)]));
    set_sde_adjusted_price(35, 12.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.insert(
            (10000002, 36),
            vec![
                MarketHistoryDayRecord {
                    date: "2026-01-01".into(),
                    lowest: 90.0,
                    highest: 110.0,
                    average: 100.0,
                    volume: 1000,
                    order_count: 10,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: "2026-01-02".into(),
                    lowest: 250.0,
                    highest: 350.0,
                    average: 300.0,
                    volume: 1000,
                    order_count: 10,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
        let prices = collect_live_market_adjusted_prices(snap);
        assert!((prices[&34] - 5.5).abs() < 1e-9);
        assert!((prices[&35] - 12.0).abs() < 1e-9);
        let expected_36 = (100.0 * 27.0 + 200.0) / 28.0;
        assert!((prices[&36] - expected_36).abs() < 1e-9);
        assert!(
            (compute_market_adjusted_price_from_snap(snap, 36).unwrap() - expected_36).abs() < 1e-9
        );
        assert!(compute_market_adjusted_price_from_snap(snap, 999888).is_none());
        assert!((resolve_material_adjusted_price(&ctx, Some(snap), 34) - 5.5).abs() < 1e-9);
        assert!((resolve_material_adjusted_price(&ctx, Some(snap), 999888) - 10.0).abs() < 1e-9);
    });
    assert!((resolve_material_adjusted_price(&ctx, None, 35) - 12.0).abs() < 1e-9);
    let ins_val = compute_ship_insurance_reference_value_for_ctx(&ctx, 587);
    assert!(ins_val >= 0.0);
    clear_sde_adjusted_prices();
}

#[test]
fn industry_loc_office_and_delivery_target_helpers_work() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let docked_id = 60_003_760;

    ctx.db.with_mut_snapshot(|snap| {
        assert!(is_industry_loc_at_facility(
            snap, station_id, station_id, docked_id, char_id, false, char_id
        ));
        assert!(!is_industry_loc_at_facility(
            snap, 7777, station_id, docked_id, char_id, false, char_id
        ));

        snap.items.insert(
            7001,
            make_item(7001, 27, corp_id, station_id, InventoryFlag::Unlocked),
        );
        snap.items.insert(
            7002,
            make_item(7002, 17366, corp_id, 7001, InventoryFlag::CorpSAG1),
        );
        assert!(!is_industry_loc_at_facility(
            snap, 7001, station_id, docked_id, char_id, false, char_id
        ));
        assert!(is_industry_loc_at_facility(
            snap, 7001, station_id, docked_id, char_id, true, corp_id
        ));
        assert!(is_industry_loc_at_facility(
            snap, 7002, station_id, docked_id, char_id, true, corp_id
        ));

        assert_eq!(
            resolve_corp_office_at_facility(snap, false, corp_id, station_id),
            None
        );
        assert_eq!(
            resolve_corp_office_at_facility(snap, true, corp_id, station_id),
            Some(7001)
        );

        let mut gov = CorpGovState::default();
        gov.offices.insert(station_id, 7001);
        snap.corp_governance.insert(corp_id, gov);
        assert_eq!(
            resolve_corp_office_at_facility(snap, true, corp_id, station_id),
            Some(7001)
        );

        let job = IndustryJobRecord {
            job_id: 1,
            installer_id: char_id,
            facility_id: station_id,
            blueprint_item_id: 8000,
            blueprint_type_id: 683,
            activity_id: 1,
            runs: 1,
            product_type_id: 587,
            status: "Active".into(),
            duration_seconds: 600,
            start_unix: 1000,
            blueprint_location_id: station_id,
            output_location_id: 7001,
            cost: Some(100.0),
            licensed_runs: None,
            probability: None,
            completed_unix: None,
            completed_character_id: None,
        };
        let mut meta = IndustryJobCostMeta::default();
        meta.use_corp = true;
        meta.owner_id = corp_id;
        meta.blueprint_original_flag = Some(InventoryFlag::CorpSAG3);
        let (loc, flag) =
            resolve_industry_delivery_target(snap, &job, &meta, char_id, corp_id, station_id);
        assert_eq!(loc, station_id);
        assert_eq!(flag, InventoryFlag::CorpSAG3);

        let mut job_cont = job.clone();
        job_cont.output_location_id = 7002;
        let (loc2, flag2) =
            resolve_industry_delivery_target(snap, &job_cont, &meta, char_id, corp_id, station_id);
        assert_eq!(loc2, 7002);
        assert_eq!(flag2, InventoryFlag::Unlocked);
    });
}

#[test]
fn capital_construction_restrictions_and_ptv_multiplier_cover_all_branches() {
    let ctx = make_test_context();
    let titan_bp = BlueprintInfo {
        blueprint_type_id: 11568,
        product_type_id: 11567,
        product_quantity: 1,
        max_production_limit: 1,
        manufacturing_time_sec: 1000,
        research_me_time_sec: 100,
        research_te_time_sec: 100,
        copy_time_sec: 100,
        invention_time_sec: 0,
        invention_base_probability: 0.0,
        materials: vec![],
        activities: HashMap::new(),
    };
    let dread_bp = BlueprintInfo {
        blueprint_type_id: 19721,
        product_type_id: 19720,
        ..titan_bp.clone()
    };

    ctx.db.with_mut_snapshot(|snap| {
        assert!(validate_capital_construction_restrictions(
            &ctx, snap, &titan_bp, 3, 30000142, 35825, 100
        )
        .is_ok());
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &titan_bp, 1, 30000142, 35827, 100
            )
            .unwrap_err()
            .code,
            "SupercapitalRequiresSovereignNullSec"
        );
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &titan_bp, 1, 30004759, 35827, 100
            )
            .unwrap_err()
            .code,
            "SupercapitalRequiresSovereignNullSec"
        );
        snap.system_states.insert(
            30004759,
            SystemDynamicState {
                solar_system_id: 30004759,
                sov_alliance_id: Some(99000001),
                ..Default::default()
            },
        );
        snap.items.insert(
            9001,
            make_item(9001, 81623, 98000001, 30004759, InventoryFlag::Unlocked),
        );
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &titan_bp, 1, 30004759, 35827, 100
            )
            .unwrap_err()
            .code,
            "SupercapitalRequiresSotiyoSupercapitalShipyard"
        );
        snap.items.insert(
            9002,
            make_item(9002, 35877, 98000001, 100, InventoryFlag::ServiceSlot0),
        );
        assert!(validate_capital_construction_restrictions(
            &ctx, snap, &titan_bp, 1, 30004759, 35827, 100
        )
        .is_ok());

        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &dread_bp, 1, 30000157, 35826, 200
            )
            .unwrap_err()
            .code,
            "CapitalConstructionForbiddenInPochven"
        );
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &dread_bp, 1, 30000142, 35826, 200
            )
            .unwrap_err()
            .code,
            "CapitalConstructionForbiddenInHighSec"
        );
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &dread_bp, 1, 30002537, 35826, 200
            )
            .unwrap_err()
            .code,
            "FacilityNotCapitalCapable"
        );
        snap.items.insert(
            9003,
            make_item(9003, 35881, 98000001, 200, InventoryFlag::ServiceSlot0),
        );
        assert!(validate_capital_construction_restrictions(
            &ctx, snap, &dread_bp, 1, 30002537, 35826, 200
        )
        .is_ok());
    });

    assert!((compute_activity_ptv_multiplier(1, 0, 5) - 5.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(9, 0, 3) - 3.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(5, 0, 10) - 0.20).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(8, 0, 2) - 0.04).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(4, 0, 1) - 0.02).abs() < 1e-9);
    assert!(
        (compute_activity_ptv_multiplier(3, 0, 2) - 0.02 * (105.0 + 250.0) / 105.0).abs() < 1e-9
    );
}

#[test]
fn ship_insurance_reference_value_with_prices_scales_by_live_material_basket() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.blueprints.retain(|_, bp| bp.product_type_id != 587);
    sde.blueprints.insert(
        999_001,
        BlueprintInfo {
            blueprint_type_id: 999_001,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 30,
            manufacturing_time_sec: 6000,
            research_me_time_sec: 1000,
            research_te_time_sec: 1000,
            copy_time_sec: 1000,
            invention_time_sec: 1000,
            invention_base_probability: 0.3,
            materials: vec![BlueprintMaterial {
                type_id: 34,
                quantity: 30_000,
            }],
            activities: HashMap::from([(
                "manufacturing".into(),
                BlueprintActivity {
                    time: 6000,
                    materials: vec![BlueprintMaterial {
                        type_id: 34,
                        quantity: 30_000,
                    }],
                    products: vec![],
                    skills: vec![],
                },
            )]),
        },
    );
    ctx.sde = Arc::new(sde);

    let base_val = compute_ship_insurance_reference_value_with_prices(
        &ctx.sde,
        &HashMap::from([(34, 2.0)]),
        587,
    );
    let doubled_val = compute_ship_insurance_reference_value_with_prices(
        &ctx.sde,
        &HashMap::from([(34, 4.0)]),
        587,
    );
    assert!((doubled_val - base_val * 2.0).abs() < 1e-3);
}

#[test]
fn sde_rig_target_filters_scope_bonuses_by_activity_and_product() {
    let ctx = make_test_context();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.clear();
        // Standup M-Set Ammunition Manufacturing Material Efficiency I (37158)
        snap.items.insert(
            5101,
            make_item(5101, 37158, 98000001, 5000, InventoryFlag::RigSlot0),
        );

        // Matches Ammunition manufacturing (activity 1, Antimatter Charge S = 222)
        let (ammo_mat, ammo_time, _, _) = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30000142,
            Some(1),
            Some(222),
        );
        assert!(ammo_mat > 0.0, "Ammunition rig must bonus ammo manufacturing");
        assert_eq!(ammo_time, 0.0);

        // Does NOT bonus Capital Ship manufacturing (19720 Revelation)
        let (cap_mat, cap_time, _, _) = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30000142,
            Some(1),
            Some(19720),
        );
        assert_eq!((cap_mat, cap_time), (0.0, 0.0));

        // Does NOT bonus Invention (activity 8) or Reactions (activity 9)
        let (inv_mat, inv_time, _, _) = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30004759,
            Some(8),
            Some(222),
        );
        assert_eq!((inv_mat, inv_time), (0.0, 0.0));

        let (react_mat, react_time, _, _) = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30004759,
            Some(9),
            Some(222),
        );
        assert_eq!((react_mat, react_time), (0.0, 0.0));
    });
}
