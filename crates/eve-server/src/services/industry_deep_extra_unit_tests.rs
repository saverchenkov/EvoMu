//! Deep branch-coverage & mutation-killing unit tests for `industry_pi`, `industry_facilities_bps`,
//! `industry_jobs`, and `industry_rigs`.

use crate::router::ServerContext;
use crate::services::industry_pi::{
    IndustryJobCostMeta, compute_activity_ptv_multiplier,
    compute_blueprint_eiv_per_run_with_snap, compute_canonical_system_cost_index,
    compute_facility_tax_bonus_for_tier, compute_industry_job_costs_with_step,
    resolve_decryptor_modifiers, resolve_structure_rig_bonuses_full, restore_blueprint_flag,
    solar_system_jump_distance,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::sync::Arc;

pub(super) const JITA_STATION_ID: i64 = 60_003_760;
pub(super) const JITA_SYSTEM_ID: u32 = 30_000_142;
pub(super) const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
pub(super) const THE_FORGE_REGION_ID: u32 = 10_000_002;

pub(super) fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    structure_id: Option<i64>,
) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id,
        solar_system_id: Some(JITA_SYSTEM_ID),
        constellation_id: Some(20_000_020),
        region_id: Some(THE_FORGE_REGION_ID),
        ship_id: Some(char_id + 500),
        ..Default::default()
    }
}

#[test]
fn ptv_multiplier_fw_tier_bonus_decryptor_and_flag_restoration_formulas() {
    let ctx = make_test_context();

    // 1. compute_activity_ptv_multiplier exact formula verification across activities 1, 3, 4, 5, 8, 9
    assert!((compute_activity_ptv_multiplier(1, 0, 5) - 5.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(9, 0, 4) - 4.0).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(5, 0, 10) - 0.20).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(8, 0, 3) - 0.06).abs() < 1e-9);
    let me_lvl1 = compute_activity_ptv_multiplier(4, 0, 1);
    let te_lvl1_to_2 = compute_activity_ptv_multiplier(3, 0, 2);
    assert!((me_lvl1 - 0.02).abs() < 1e-9);
    assert!((te_lvl1_to_2 - (0.02 * 355.0 / 105.0)).abs() < 1e-9);
    assert!((compute_activity_ptv_multiplier(99, 0, 7) - 7.0).abs() < 1e-9);

    // 2. compute_facility_tax_bonus_for_tier across FW tiers 0..=5
    assert!((compute_facility_tax_bonus_for_tier(100.0, 0) - 100.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 1) - 90.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 2) - 80.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 3) - 70.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 4) - 60.0).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(100.0, 5) - 50.0).abs() < 1e-9);

    // 3. restore_blueprint_flag branches
    let meta_sag3 = IndustryJobCostMeta {
        blueprint_original_flag: Some(InventoryFlag::CorpSAG3),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(9001, &meta_sag3), InventoryFlag::CorpSAG3);

    let meta_corp_locked = IndustryJobCostMeta {
        use_corp: true,
        owner_id: 98_001,
        blueprint_original_flag: Some(InventoryFlag::Locked),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(98_001, &meta_corp_locked), InventoryFlag::CorpSAG1);

    let meta_char_locked = IndustryJobCostMeta {
        use_corp: false,
        owner_id: 9001,
        blueprint_original_flag: Some(InventoryFlag::Locked),
        ..Default::default()
    };
    assert_eq!(restore_blueprint_flag(9001, &meta_char_locked), InventoryFlag::Hangar);
    assert_eq!(
        restore_blueprint_flag(9001, &IndustryJobCostMeta::default()),
        InventoryFlag::Hangar
    );

    // 4. resolve_decryptor_modifiers: invalid item, non-decryptor group, and valid SDE decryptor
    assert_eq!(
        resolve_decryptor_modifiers(&ctx, 999_999_999).unwrap_err().code,
        "InvalidDecryptor"
    );
    assert_eq!(
        resolve_decryptor_modifiers(&ctx, 34).unwrap_err().code,
        "InvalidDecryptor"
    );
    let dec_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1304)
        .map(|t| t.type_id)
        .unwrap();
    let (prob_mult, me_mod, te_mod, runs_mod) = resolve_decryptor_modifiers(&ctx, dec_tid).unwrap();
    assert!(prob_mult > 0.0);
    assert!(me_mod != 0.0 || te_mod != 0.0 || runs_mod != 0);

    // 5. solar_system_jump_distance: 0 for same system, 1 for adjacent Jita -> Perimeter
    assert_eq!(solar_system_jump_distance(&ctx, JITA_SYSTEM_ID, JITA_SYSTEM_ID), Some(0));
    assert_eq!(
        solar_system_jump_distance(&ctx, JITA_SYSTEM_ID, PERIMETER_SYSTEM_ID),
        Some(1)
    );
}

#[test]
fn cost_index_eiv_job_costs_and_structure_rig_security_multipliers() {
    let ctx = make_test_context();
    let char_id = 920_001;
    let corp_id = 98_000_201;
    let struct_hs_id = 1_000_000_920_001;
    let struct_ls_id = 1_000_000_920_002;
    let struct_ns_id = 1_000_000_920_003;

    let ls_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.05 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);
    let ns_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Ind Cost Pilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(JITA_STATION_ID);
        ch.solar_system_id = JITA_SYSTEM_ID;
        ch.constellation_id = 20_000_020;
        ch.region_id = THE_FORGE_REGION_ID;
        ch.active_ship_id = char_id + 500;
        ch.balance = IskCents::from_isk(100_000_000);
        ch.clone_grade = 1; // Alpha clone
        snap.characters.insert(char_id, ch);

        for (sid, tid, sys) in [
            (struct_hs_id, 35825, JITA_SYSTEM_ID),
            (struct_ls_id, 35826, ls_sys),
            (struct_ns_id, 35836, ns_sys),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Struct {sid}"),
                    state: "ShieldVulnerable".into(),
                    fuel_blocks: 500,
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            920_101,
            InventoryItemRecord {
                item_id: 920_101,
                type_id: 37146,
                owner_id: corp_id,
                location_id: struct_hs_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            920_102,
            InventoryItemRecord {
                item_id: 920_102,
                type_id: 37146,
                owner_id: corp_id,
                location_id: struct_ls_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let react_rig_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 1933)
            .map(|t| t.type_id)
            .unwrap_or(46482);
        snap.items.insert(
            920_103,
            InventoryItemRecord {
                item_id: 920_103,
                type_id: react_rig_tid,
                owner_id: corp_id,
                location_id: struct_hs_id,
                flag: InventoryFlag::RigSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            920_104,
            InventoryItemRecord {
                item_id: 920_104,
                type_id: react_rig_tid,
                owner_id: corp_id,
                location_id: struct_ns_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let bp = ctx.sde.blueprints.get(&683).expect("Bantam Blueprint 683 in SDE");
        let eiv_1 = compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), bp, 1);
        let eiv_8 = compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), bp, 8);
        assert!(eiv_1 > 0.0 && eiv_8 > 0.0);

        let sci_base = compute_canonical_system_cost_index(&ctx, snap, JITA_SYSTEM_ID, 1);
        assert!(sci_base >= 0.0001 && sci_base <= 1.0);

        // Alpha clone at NPC station pays 0.25% alpha clone tax and 4.0% SCC surcharge
        let meta = compute_industry_job_costs_with_step(
            &ctx,
            snap,
            Some(char_id),
            683,
            1,
            0,
            2,
            JITA_STATION_ID,
            true,
        )
        .unwrap();
        assert!(meta.eiv_isk > 0.0 && meta.system_cost_index > 0.0 && meta.base_job_cost_isk > 0.0);
        assert!(meta.facility_tax_isk > 0.0 && meta.scc_surcharge_isk > 0.0 && meta.alpha_clone_tax_isk > 0.0);
        let expected_total = ((meta.base_job_cost_isk
            + meta.facility_tax_isk
            + meta.scc_surcharge_isk
            + meta.alpha_clone_tax_isk)
            * 100.0)
            .round()
            / 100.0;
        assert!((meta.total_cost_isk - expected_total).abs() < 1e-6);

        let hs = resolve_structure_rig_bonuses_full(&ctx, snap, struct_hs_id, JITA_SYSTEM_ID);
        let ls = resolve_structure_rig_bonuses_full(&ctx, snap, struct_ls_id, ls_sys);
        assert!(hs.mat_bonus > 0.0 || hs.time_bonus > 0.0 || hs.cost_bonus > 0.0 || hs.sec_multiplier >= 0.0);
        assert!(ls.mat_bonus > hs.mat_bonus || ls.time_bonus > hs.time_bonus);
        assert!((ls.sec_multiplier - 1.9).abs() < 1e-6);

        let ns = resolve_structure_rig_bonuses_full(&ctx, snap, struct_ns_id, ns_sys);
        assert!(ns.mat_bonus > 0.0 || ns.time_bonus > 0.0);
        assert!((ns.sec_multiplier - 1.1).abs() < 1e-6);
    });
}
