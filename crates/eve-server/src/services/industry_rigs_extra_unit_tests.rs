//! Deep branch-coverage and mutation-killing unit tests for `industry_rigs`.

#![forbid(unsafe_code)]

use super::industry_pi::industry_rigs::{
    clear_sde_adjusted_prices, collect_live_market_adjusted_prices, compute_activity_ptv_multiplier,
    compute_market_adjusted_price_from_snap, compute_ship_insurance_reference_value_with_prices,
    is_industry_loc_at_facility, resolve_corp_office_at_facility, resolve_facility_modifiers_and_system,
    resolve_industry_delivery_target, resolve_material_adjusted_price,
    resolve_structure_rig_bonuses_full, set_sde_adjusted_price, structure_sde_industry_bonuses,
    validate_capital_construction_restrictions,
};
use crate::router::ServerContext;
use crate::services::industry_pi::IndustryJobCostMeta;
use eve_db::{
    CorpGovState, IndustryJobRecord, InventoryFlag, InventoryItemRecord, MarketHistoryDayRecord,
    SystemDynamicState, UpwellStructureRecord, WorldRepository,
};
use eve_sde::{BlueprintActivity, BlueprintInfo, BlueprintMaterial, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_type(type_id: u32, group_id: u32, base_price: f64, attrs: &[(u16, f64)]) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 6,
        name: format!("Type_{type_id}"),
        mass: 1.0,
        volume: 1.0,
        capacity: 0.0,
        radius: 1.0,
        base_price,
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
        default_attributes: attrs.iter().copied().collect(),
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
fn rig_bonuses_cover_custom_sde_sec_multipliers_disallow_hs_t2_sde_attr_and_clamping() {
    let mut ctx = make_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);

    // Type 991_001: T2 rig via SDE attr 422 = 2.0, custom sec multipliers 2355=1.2, 2356=2.0, 2357=2.5
    sde_mut.types.insert(
        991_001,
        make_type(
            991_001,
            1800,
            1000.0,
            &[(422, 2.0), (2355, 1.2), (2356, 2.0), (2357, 2.5)],
        ),
    );
    // Type 991_002: Non-reaction rig with disallowInHighSec (1970 = 1.0) and only cost bonus (2595 = -0.15)
    sde_mut.types.insert(
        991_002,
        make_type(991_002, 1800, 1000.0, &[(1970, 1.0), (2595, -0.15)]),
    );
    // Type 991_003: Extreme bonus rig to test clamping at (0.50, 0.80, 0.50)
    sde_mut.types.insert(
        991_003,
        make_type(
            991_003,
            1800,
            1000.0,
            &[(2594, -80.0), (2593, -95.0), (2595, -80.0)],
        ),
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.clear();
        snap.items
            .insert(1, make_item(1, 991_001, 100, 5000, InventoryFlag::RigSlot0));

        // High-Sec (30_000_142) uses custom attr 2355 = 1.2 and T2 defaults (0.024, 0.24)
        let (m_hs, t_hs, c_hs, sec_hs) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_000_142);
        assert!((m_hs - 0.024 * 1.2).abs() < 1e-9);
        assert!((t_hs - 0.24 * 1.2).abs() < 1e-9);
        assert!((c_hs - 0.0).abs() < 1e-9);
        assert!((sec_hs - 1.2).abs() < 1e-9);

        // Low-Sec (30_002_537) uses custom attr 2356 = 2.0
        let (m_ls, t_ls, _, sec_ls) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_002_537);
        assert!((m_ls - 0.024 * 2.0).abs() < 1e-9);
        assert!((t_ls - 0.24 * 2.0).abs() < 1e-9);
        assert!((sec_ls - 2.0).abs() < 1e-9);

        // Null-Sec (30_004_759) uses custom attr 2357 = 2.5
        let (m_ns, t_ns, _, sec_ns) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_004_759);
        assert!((m_ns - 0.024 * 2.5).abs() < 1e-9);
        assert!((t_ns - 0.24 * 2.5).abs() < 1e-9);
        assert!((sec_ns - 2.5).abs() < 1e-9);

        // Test disallow_hs (1970 = 1.0) + cost-only SDE attribute (has_any_sde_ind_attr = true -> mat=0, time=0)
        snap.items.clear();
        snap.items
            .insert(2, make_item(2, 991_002, 100, 5000, InventoryFlag::RigSlot1));
        assert_eq!(
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_000_142),
            (0.0, 0.0, 0.0, 0.0)
        );
        let (m2_ls, t2_ls, c2_ls, sec2_ls) =
            resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_002_537);
        assert_eq!((m2_ls, t2_ls), (0.0, 0.0));
        assert!((c2_ls - 0.15 * 1.9).abs() < 1e-9);
        assert!((sec2_ls - 1.9).abs() < 1e-9);

        // Test upper clamping at (0.50, 0.80, 0.50)
        snap.items.clear();
        snap.items
            .insert(3, make_item(3, 991_003, 100, 5000, InventoryFlag::RigSlot2));
        let (mc, tc, cc, _) = resolve_structure_rig_bonuses_full(&ctx, snap, 5000, 30_004_759);
        assert!((mc - 0.50).abs() < 1e-9);
        assert!((tc - 0.80).abs() < 1e-9);
        assert!((cc - 0.50).abs() < 1e-9);
    });
}

#[test]
fn facility_modifiers_and_sde_bonuses_cover_tatara_2721_item_fallbacks_and_npc_station() {
    let mut ctx = make_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Tatara (35836) uses attribute 2721 for reaction time modifier when 2602 is absent
    sde_mut.types.insert(
        35_836,
        make_type(35_836, 1406, 1_000_000.0, &[(2600, 1.0), (2721, 0.75), (2601, 0.95)]),
    );
    assert_eq!(structure_sde_industry_bonuses(&ctx, 35_836), (1.0, 0.75, 0.95));

    ctx.db.with_mut_snapshot(|snap| {
        let mut tmpl_char = snap.characters.values().next().unwrap().clone();
        snap.structures.clear();
        snap.items.clear();
        snap.characters.clear();

        // 1. Item facility located inside an NPC station (60_003_760 -> Jita 30_000_142 via sde.location_solar_system)
        snap.items.insert(
            700_001,
            make_item(700_001, 35_836, 98_000_077, 60_003_760, InventoryFlag::Hangar),
        );
        let res_st = resolve_facility_modifiers_and_system(&ctx, snap, 700_001).unwrap();
        assert_eq!(res_st.0, 35_836);
        assert_eq!(res_st.2, 30_000_142);

        // 2. Item facility with non-station location_id resolved via corporation member's solar_system_id
        tmpl_char.character_id = 90_000_088;
        tmpl_char.corporation_id = 98_000_088;
        tmpl_char.solar_system_id = 30_002_537;
        snap.characters.insert(90_000_088, tmpl_char);
        snap.items.insert(
            700_002,
            make_item(700_002, 35_836, 98_000_088, 999_999_999, InventoryFlag::Unlocked),
        );
        let res_corp = resolve_facility_modifiers_and_system(&ctx, snap, 700_002).unwrap();
        assert_eq!(res_corp.2, 30_002_537);

        // 3. Direct NPC station ID (60_003_760) resolves from SDE stations with (1.0, 1.0, 1.0) modifiers
        let res_npc = resolve_facility_modifiers_and_system(&ctx, snap, 60_003_760).unwrap();
        assert_eq!(res_npc.2, 30_000_142);
        assert_eq!((res_npc.4, res_npc.5, res_npc.6), (1.0, 1.0, 1.0));
    });
}

#[test]
fn rolling_28_day_ema_window_and_insurance_activity_fallback_and_hull_adj_fallback() {
    clear_sde_adjusted_prices();
    let mut ctx = make_ctx();
    let sde_mut = Arc::make_mut(&mut ctx.sde);

    // Type with base_price = 0.0 falls back to 10.0 in resolve_material_adjusted_price
    sde_mut.types.insert(880_001, make_type(880_001, 18, 0.0, &[]));
    // Blueprint with empty top-level `materials`, using `activities["1"]` instead
    sde_mut.blueprints.insert(
        880_100,
        BlueprintInfo {
            blueprint_type_id: 880_100,
            product_type_id: 880_101,
            product_quantity: 1,
            max_production_limit: 10,
            manufacturing_time_sec: 1000,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.0,
            materials: Vec::new(),
            activities: HashMap::from([(
                "1".into(),
                BlueprintActivity {
                    time: 1000,
                    materials: vec![BlueprintMaterial {
                        type_id: 880_001,
                        quantity: 100,
                    }],
                    products: Vec::new(),
                    skills: Vec::new(),
                },
            )]),
        },
    );

    ctx.db.with_mut_snapshot(|snap| {
        assert!((resolve_material_adjusted_price(&ctx, Some(snap), 880_001) - 10.0).abs() < 1e-9);

        // Seed 30 days of history: day 01 has huge price 1_000_000 (must be dropped by recent 28 slice!),
        // days 03..=30 (28 days) all have average = 50.0, plus one invalid average = 0.0 record.
        let mut days = vec![
            MarketHistoryDayRecord {
                date: "2026-01-01".into(),
                lowest: 1_000_000.0,
                highest: 1_000_000.0,
                average: 1_000_000.0,
                volume: 10,
                order_count: 1,
                open_price: None,
                close_price: None,
            },
            MarketHistoryDayRecord {
                date: "2026-01-02".into(),
                lowest: 0.0,
                highest: 0.0,
                average: 0.0,
                volume: 0,
                order_count: 0,
                open_price: None,
                close_price: None,
            },
        ];
        for d in 3..=30 {
            days.push(MarketHistoryDayRecord {
                date: format!("2026-01-{d:02}"),
                lowest: 50.0,
                highest: 50.0,
                average: 50.0,
                volume: 100,
                order_count: 5,
                open_price: None,
                close_price: None,
            });
        }
        snap.market_history.insert((10_000_002, 880_001), days);

        let adj = compute_market_adjusted_price_from_snap(snap, 880_001).unwrap();
        assert!((adj - 50.0).abs() < 1e-9);
        let map = collect_live_market_adjusted_prices(snap);
        assert!((map[&880_001] - 50.0).abs() < 1e-9);
    });

    // Insurance reference value:
    // 1. Empty live_prices returns base_hull_price
    let empty_prices = HashMap::new();
    let v_empty = compute_ship_insurance_reference_value_with_prices(&ctx.sde, &empty_prices, 880_101);
    assert_eq!(v_empty, 0.0);

    // 2. Blueprint activity "1" materials with base_hull_price == 0.0 returns live_sum (100 * 25.0 = 2500.0)
    let live_mat = HashMap::from([(880_001, 25.0)]);
    let v_act = compute_ship_insurance_reference_value_with_prices(&ctx.sde, &live_mat, 880_101);
    assert!((v_act - 2_500.0).abs() < 1e-9);

    // 3. Ship without material match in live_prices falls back to hull's own live_prices entry
    let live_hull = HashMap::from([(880_101, 77_777.0)]);
    let v_hull = compute_ship_insurance_reference_value_with_prices(&ctx.sde, &live_hull, 880_101);
    assert!((v_hull - 77_777.0).abs() < 1e-9);

    set_sde_adjusted_price(880_001, 0.0); // non-positive price ignored by cache lookup
    clear_sde_adjusted_prices();
}

#[test]
fn delivery_targets_stale_office_fallback_orca_exemption_and_supercap_structure_upgrade() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.clear();
        snap.corp_governance.clear();

        // Stale office ID (9999) in corp_governance falls back to scanning snap.items for type 27
        let mut gov = CorpGovState::default();
        gov.offices.insert(station_id, 9999);
        snap.corp_governance.insert(corp_id, gov);
        snap.items.insert(
            7010,
            make_item(7010, 27, corp_id, station_id, InventoryFlag::Unlocked),
        );
        assert_eq!(
            resolve_corp_office_at_facility(snap, true, corp_id, station_id),
            Some(7010)
        );

        // Personal nested containers (parent -> grandparent -> station)
        snap.items.insert(
            7020,
            make_item(7020, 17_366, char_id, station_id, InventoryFlag::Hangar),
        );
        snap.items.insert(
            7021,
            make_item(7021, 17_366, char_id, 7020, InventoryFlag::Unlocked),
        );
        assert!(is_industry_loc_at_facility(
            snap, 7021, station_id, station_id, char_id, false, char_id
        ));

        // Delivery target: job.output_location_id = 0, meta.output_location_id = 0 -> fallback_loc + Hangar
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
            output_location_id: 0,
            cost: None,
            licensed_runs: None,
            probability: None,
            completed_unix: None,
            completed_character_id: None,
        };
        let meta = IndustryJobCostMeta::default();
        assert_eq!(
            resolve_industry_delivery_target(snap, &job, &meta, char_id, char_id, station_id),
            (station_id, InventoryFlag::Hangar)
        );

        // Orca (product_type_id = 28606, group 883) is exempt from High-Sec capital prohibition
        let orca_bp = BlueprintInfo {
            blueprint_type_id: 28_607,
            product_type_id: 28_606,
            product_quantity: 1,
            max_production_limit: 1,
            manufacturing_time_sec: 1000,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: Vec::new(),
            activities: HashMap::new(),
        };
        assert!(validate_capital_construction_restrictions(
            &ctx, snap, &orca_bp, 1, 30_000_142, 35_825, station_id
        )
        .is_ok());

        // Supercapital in wormhole (31_000_005) rejected even though sec < 0.0
        let titan_bp = BlueprintInfo {
            blueprint_type_id: 11_568,
            product_type_id: 11_567,
            ..orca_bp
        };
        assert_eq!(
            validate_capital_construction_restrictions(
                &ctx, snap, &titan_bp, 1, 31_000_005, 35_827, 100
            )
            .unwrap_err()
            .code,
            "SupercapitalRequiresSovereignNullSec"
        );

        // Supercapital upgrade (2009) installed on a non-destroyed structure in sovereign null-sec is accepted
        snap.system_states.insert(
            30_004_759,
            SystemDynamicState {
                solar_system_id: 30_004_759,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
        snap.structures.insert(
            555_000,
            UpwellStructureRecord {
                structure_id: 555_000,
                type_id: 35_827,
                owner_corp_id: corp_id,
                solar_system_id: 30_004_759,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            9101,
            make_item(9101, 2009, corp_id, 555_000, InventoryFlag::ServiceSlot0),
        );
        snap.items.insert(
            9102,
            make_item(9102, 35_877, corp_id, 555_000, InventoryFlag::ServiceSlot1),
        );
        assert!(validate_capital_construction_restrictions(
            &ctx, snap, &titan_bp, 1, 30_004_759, 35_827, 555_000
        )
        .is_ok());
    });

    // PTV multiplier out-of-bounds start_step >= 10 clamps cleanly to 0.0
    assert_eq!(compute_activity_ptv_multiplier(3, 12, 5), 0.0);
}
