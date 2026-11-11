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
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 1.0 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30002537),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 1.9 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 2.1 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 31000005),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 2.1 }
        );

        let mut rig_t1 = make_item(5001, 999990, 98000001, 5000, InventoryFlag::RigSlot0);
        rig_t1.dynamic_attributes.insert(2594, -2.0);
        rig_t1.dynamic_attributes.insert(2593, -20.0);
        rig_t1.dynamic_attributes.insert(2595, -10.0);
        snap.items.insert(5001, rig_t1);

        let hs = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30000142);
        assert!((hs.mat_bonus - 0.02).abs() < 1e-9);
        assert!((hs.time_bonus - 0.20).abs() < 1e-9);
        assert!((hs.cost_bonus - 0.10).abs() < 1e-9);
        assert!((hs.sec_multiplier - 1.0).abs() < 1e-9);

        let ls = resolve_structure_rig_bonus(&ctx, snap, 5000, 30002537);
        assert!((ls.mat_bonus - 0.038).abs() < 1e-9);
        assert!((ls.time_bonus - 0.38).abs() < 1e-9);
        assert!((ls.sec_multiplier - 1.9).abs() < 1e-9);

        let mut rig_t2 = make_item(5002, 999991, 98000001, 5000, InventoryFlag::RigSlot1);
        rig_t2.dynamic_attributes.insert(422, 2.0);
        rig_t2.dynamic_attributes.insert(2594, -2.4);
        rig_t2.dynamic_attributes.insert(2593, -24.0);
        snap.items.insert(5002, rig_t2);
        let ns = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759);
        assert!((ns.mat_bonus - 0.024 * 2.1).abs() < 1e-9);
        assert!((ns.time_bonus - 0.24 * 2.1).abs() < 1e-9);
        assert!((ns.sec_multiplier - 2.1).abs() < 1e-9);

        snap.items.clear();
        let react_rig = make_item(5003, 990001, 98000001, 5000, InventoryFlag::RigSlot2);
        snap.items.insert(5003, react_rig);
        let r_hs = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30000142);
        assert_eq!((r_hs.mat_bonus, r_hs.time_bonus, r_hs.sec_multiplier), (0.0, 0.0, 0.0));
        let r_ls = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30002537);
        assert!((r_ls.mat_bonus - 0.02).abs() < 1e-9);
        assert!((r_ls.time_bonus - 0.20).abs() < 1e-9);
        assert!((r_ls.sec_multiplier - 1.0).abs() < 1e-9);
        let r_ns = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30004759);
        assert!((r_ns.mat_bonus - 0.022).abs() < 1e-9);
        assert!((r_ns.time_bonus - 0.22).abs() < 1e-9);
        assert!((r_ns.sec_multiplier - 1.1).abs() < 1e-9);
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
        StructureIndustryBonuses { material_modifier: 1.0, time_modifier: 1.0, cost_modifier: 1.0 }
    );
    let hull = structure_sde_industry_bonuses(&ctx, 35825);
    assert!(
        (hull.material_modifier - 0.99).abs() < 1e-9
            && (hull.time_modifier - 0.85).abs() < 1e-9
            && (hull.cost_modifier - 0.97).abs() < 1e-9
    );

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
        assert_eq!(resolved.type_id, 35825);
        assert_eq!(resolved.owner_corp_id, 98000001);
        assert_eq!(resolved.solar_system_id, 30000142);

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
        assert_eq!(item_res.type_id, 35825);
        assert_eq!(item_res.owner_corp_id, 98000002);
        assert_eq!(item_res.solar_system_id, 30000142);

        assert!(resolve_facility_modifiers_and_system(&ctx, snap, 999_999_999).is_none());
    });

    let _adj_lock = SDE_ADJ_PRICE_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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

pub(super) static SDE_ADJ_PRICE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let ammo = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30000142,
            Some(1),
            Some(222),
        );
        assert!(ammo.mat_bonus > 0.0, "Ammunition rig must bonus ammo manufacturing");
        assert_eq!(ammo.time_bonus, 0.0);

        // Does NOT bonus Capital Ship manufacturing (19720 Revelation)
        let cap = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30000142,
            Some(1),
            Some(19720),
        );
        assert_eq!((cap.mat_bonus, cap.time_bonus), (0.0, 0.0));

        // Does NOT bonus Invention (activity 8) or Reactions (activity 9)
        let inv = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30004759,
            Some(8),
            Some(222),
        );
        assert_eq!((inv.mat_bonus, inv.time_bonus), (0.0, 0.0));

        let react = resolve_structure_rig_bonuses_for_job(
            &ctx,
            snap,
            5000,
            30004759,
            Some(9),
            Some(222),
        );
        assert_eq!((react.mat_bonus, react.time_bonus), (0.0, 0.0));
    });
}

#[test]
fn w3_mutant_killers_rig_security_boundaries_activity_products_and_hull_bonuses() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();

    // Synthetic solar systems at exact security boundaries: 0.45 HS, 0.449 LS, 0.0 NS, 0.90 WH (>= 31_000_000)
    let base_sys = sde.solar_systems.get(&30000142).unwrap().clone();
    for (sys_id, sec) in [
        (30_999_001_u32, 0.45_f64),
        (30_999_002_u32, 0.449_f64),
        (30_999_003_u32, 0.0_f64),
        (31_000_000_u32, 0.90_f64),
    ] {
        let mut sys = base_sys.clone();
        sys.solar_system_id = sys_id;
        sys.security_status = sec;
        sde.solar_systems.insert(sys_id, sys);
    }

    let base_type = sde.types.get(&35825).unwrap().clone();
    // Outpost conversion rig (group_id == 1984)
    let mut outpost_rig = base_type.clone();
    outpost_rig.type_id = 990_101;
    outpost_rig.group_id = 1984;
    outpost_rig.default_attributes = HashMap::from([(2594, -2.0), (2593, -20.0), (2595, -10.0)]);
    sde.types.insert(990_101, outpost_rig);

    // Reaction rig via group 1933..=1939 and explicit 2713/2714
    let mut react_rig = base_type.clone();
    react_rig.type_id = 990_102;
    react_rig.group_id = 1935;
    react_rig.default_attributes = HashMap::from([(2714, -2.4), (2713, -24.0)]);
    sde.types.insert(990_102, react_rig);

    // Engineering rig with disallowInHighSec (1970 = 1.0) and custom sec mults (2356 = 2.0, 2357 = 2.5)
    let mut disallow_hs_rig = base_type.clone();
    disallow_hs_rig.type_id = 990_103;
    disallow_hs_rig.group_id = 1816;
    disallow_hs_rig.default_attributes = HashMap::from([
        (1970, 1.0),
        (2356, 2.0),
        (2357, 2.5),
        (2594, -0.25), // already fractional <= 1.0 -> norm_frac preserves 0.25 (25% * 2.5 = 62.5% -> clamped to 0.50)
        (2593, -0.40), // 0.40 * 2.5 = 1.0 -> clamped to 0.80
        (2595, -0.30), // 0.30 * 2.5 = 0.75 -> clamped to 0.50
    ]);
    sde.types.insert(990_103, disallow_hs_rig);

    // Custom structure hull not in industry_modifier_sources to exercise fallback attributes 2600/2601/2602/2721
    let mut custom_hull = base_type.clone();
    custom_hull.type_id = 990_104;
    custom_hull.default_attributes = HashMap::from([
        (2600, 0.99),
        (2601, 0.95),
        (2602, 0.80),
        (2721, 0.75),
    ]);
    sde.types.insert(990_104, custom_hull);

    ctx.sde = Arc::new(sde);

    // 1. resolve_blueprint_activity_product across activities 1, 8, 9, 11, and zero product fallback
    let mut bp = BlueprintInfo {
        blueprint_type_id: 990_200,
        product_type_id: 587,
        product_quantity: 1,
        max_production_limit: 10,
        manufacturing_time_sec: 100,
        research_me_time_sec: 100,
        research_te_time_sec: 100,
        copy_time_sec: 100,
        invention_time_sec: 100,
        invention_base_probability: 0.4,
        materials: vec![],
        activities: HashMap::from([
            (
                "manufacturing".into(),
                BlueprintActivity {
                    time: 100,
                    materials: vec![],
                    products: vec![eve_sde::BlueprintProduct {
                        type_id: 588,
                        quantity: 1,
                        probability: None,
                    }],
                    skills: vec![],
                },
            ),
            (
                "8".into(),
                BlueprintActivity {
                    time: 100,
                    materials: vec![],
                    products: vec![eve_sde::BlueprintProduct {
                        type_id: 11379,
                        quantity: 1,
                        probability: Some(0.3),
                    }],
                    skills: vec![],
                },
            ),
            (
                "9".into(),
                BlueprintActivity {
                    time: 100,
                    materials: vec![],
                    products: vec![eve_sde::BlueprintProduct {
                        type_id: 16670,
                        quantity: 100,
                        probability: None,
                    }],
                    skills: vec![],
                },
            ),
        ]),
    };
    assert_eq!(resolve_blueprint_activity_product(&bp, 1), Some(588));
    assert_eq!(resolve_blueprint_activity_product(&bp, 8), Some(11379));
    assert_eq!(resolve_blueprint_activity_product(&bp, 9), Some(16670));
    assert_eq!(resolve_blueprint_activity_product(&bp, 11), Some(16670));
    bp.activities.clear();
    assert_eq!(resolve_blueprint_activity_product(&bp, 1), Some(587));
    bp.product_type_id = 0;
    assert_eq!(resolve_blueprint_activity_product(&bp, 1), None);

    // 2. structure_sde_industry_bonuses_for_activity fallback & unknown activity
    assert_eq!(
        structure_sde_industry_bonuses_for_activity(&ctx, 999_888_777, Some(1)),
        StructureIndustryBonuses { material_modifier: 1.0, time_modifier: 1.0, cost_modifier: 1.0 }
    );
    assert_eq!(
        structure_sde_industry_bonuses_for_activity(&ctx, 990_104, Some(1)),
        StructureIndustryBonuses { material_modifier: 0.99, time_modifier: 0.80, cost_modifier: 0.95 }
    );
    assert_eq!(
        structure_sde_industry_bonuses_for_activity(&ctx, 990_104, Some(9)),
        StructureIndustryBonuses { material_modifier: 1.0, time_modifier: 0.75, cost_modifier: 1.0 }
    );
    assert_eq!(
        structure_sde_industry_bonuses_for_activity(&ctx, 990_104, Some(11)),
        StructureIndustryBonuses { material_modifier: 1.0, time_modifier: 0.75, cost_modifier: 1.0 }
    );

    // 3. Security boundary & rig multiplier checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.clear();
        // Empty rigs -> default_sec_mult: 0.45 HS -> 1.0, 0.449 LS -> 1.9, 0.0 NS -> 2.1, 31_000_000 WH -> 2.1
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_001, Some(1), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 1.0 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_002, Some(1), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 1.9 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_003, Some(1), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 2.1 }
        );
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 31_000_000, Some(1), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 2.1 }
        );

        // Outpost conversion rig (1984): 1.0x even in Null-Sec!
        snap.items.insert(
            6101,
            make_item(6101, 990_101, 98000001, 6000, InventoryFlag::RigSlot0),
        );
        let out = resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_003, Some(1), None);
        assert!((out.mat_bonus - 0.02).abs() < 1e-9);
        assert!((out.time_bonus - 0.20).abs() < 1e-9);
        assert!((out.cost_bonus - 0.10).abs() < 1e-9);
        assert!((out.sec_multiplier - 1.0).abs() < 1e-9);
        // Unknown activity_id returns zero bonuses + default_sec_mult
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_003, Some(99), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 2.1 }
        );

        // Reaction rig (990_102): HS -> 0.0x, LS -> 1.0x, NS -> 1.1x
        snap.items.clear();
        snap.items.insert(
            6102,
            make_item(6102, 990_102, 98000001, 6000, InventoryFlag::RigSlot1),
        );
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_001, Some(9), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 0.0 }
        );
        let r_ls = resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_002, Some(9), None);
        assert!((r_ls.mat_bonus - 0.024).abs() < 1e-9);
        assert!((r_ls.time_bonus - 0.24).abs() < 1e-9);
        assert!((r_ls.sec_multiplier - 1.0).abs() < 1e-9);
        let r_ns = resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_003, Some(9), None);
        assert!((r_ns.mat_bonus - 0.024 * 1.1).abs() < 1e-9);
        assert!((r_ns.time_bonus - 0.24 * 1.1).abs() < 1e-9);
        assert!((r_ns.sec_multiplier - 1.1).abs() < 1e-9);

        // Disallow-in-HS engineering rig (990_103) with custom 2356/2357 & cap clamping (0.50 / 0.80 / 0.50)
        snap.items.clear();
        snap.items.insert(
            6103,
            make_item(6103, 990_103, 98000001, 6000, InventoryFlag::RigSlot2),
        );
        assert_eq!(
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_001, Some(1), None),
            StructureRigBonuses { mat_bonus: 0.0, time_bonus: 0.0, cost_bonus: 0.0, sec_multiplier: 0.0 }
        );
        let capped =
            resolve_structure_rig_bonuses_for_job(&ctx, snap, 6000, 30_999_003, Some(1), None);
        assert!((capped.mat_bonus - 0.50).abs() < 1e-9);
        assert!((capped.time_bonus - 0.80).abs() < 1e-9);
        assert!((capped.cost_bonus - 0.50).abs() < 1e-9);
        assert!((capped.sec_multiplier - 2.5).abs() < 1e-9);

        // `resolve_structure_rig_bonus` view struct and `resolve_facility_modifiers_and_system_for_job`
        let view3 = resolve_structure_rig_bonus(&ctx, snap, 6000, 30_999_003);
        assert!((view3.mat_bonus - 0.50).abs() < 1e-9);
        assert!((view3.time_bonus - 0.80).abs() < 1e-9);
        assert!((view3.sec_multiplier - 2.5).abs() < 1e-9);

        snap.structures.insert(
            6000,
            UpwellStructureRecord {
                structure_id: 6000,
                type_id: 35825,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_999_003,
                name: "Raitaru".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0; 3],
                ..Default::default()
            },
        );
        let fac_live =
            resolve_facility_modifiers_and_system_for_job(&ctx, snap, 6000, Some(1), None)
                .expect("active structure resolves facility modifiers");
        assert_eq!(fac_live.type_id, 35825);
        assert_eq!(fac_live.owner_corp_id, 98_000_001);
        assert_eq!(fac_live.solar_system_id, 30_999_003);
        assert!(
            (fac_live.material_modifier - 0.99).abs() < 1e-6
                && fac_live.time_modifier < 0.20
                && fac_live.cost_modifier < 0.50
        );

        snap.structures.get_mut(&6000).unwrap().state = "Destroyed".into();
        assert!(
            resolve_facility_modifiers_and_system_for_job(&ctx, snap, 6000, Some(1), None)
                .is_none()
        );
    });
}

#[cfg(test)]
#[path = "industry_rigs_extra_unit_tests.rs"]
mod extra_unit_tests;
