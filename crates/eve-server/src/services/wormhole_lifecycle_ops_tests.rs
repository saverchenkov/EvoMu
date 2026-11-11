use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use std::sync::Arc;

#[cfg(test)]
#[path = "wormhole_lifecycle_edges_tests.rs"]
mod wormhole_lifecycle_edges_tests;

#[tokio::test]
async fn test_wormhole_lifecycle_profiles_mass_statics_and_k162() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(20_353, make_fighter_type(20_353, 329, 7, 10.0, vec![(796, 4_000_000.0)]));
    sde_mut
        .types
        .insert(49_099, make_fighter_type(49_099, 2008, 7, 10.0, vec![(1131, -80.0)]));

    // Test wormhole_exit_item_id_for_signature and pick_system_celestial_position
    let exit_id = wormhole_exit_item_id_for_signature(sys_id, "WST-101");
    assert!(exit_id > 0);
    let (pos, _) = pick_system_celestial_position(&ctx, sys_id, 42);
    assert!(pos[0] != 0.0 || pos[1] != 0.0);

    // Test compute_effective_ship_jump_mass with fitted plate, active prop mod, and Zero-Point Mass Entangler
    let plate_id = 720_001_i64;
    let entangler_id = 720_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plate_id,
            make_fighter_item(plate_id, 20_353, char_id, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.module_online_states.insert(plate_id, true);
    });
    let (mass_base, _) = compute_effective_ship_jump_mass(&ctx, ship_id, 10_000_000.0);
    assert!(mass_base >= 10_000_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            entangler_id,
            make_fighter_item(entangler_id, 49_099, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_active_states.insert(entangler_id);
    });
    let (mass_entangled, prop_add) = compute_effective_ship_jump_mass(&ctx, ship_id, 10_000_000.0);
    assert_eq!(prop_add, 0.0);
    assert!(mass_entangled < mass_base);

    // Test is_small_ship_wormhole & check_small_ship_wormhole_eligibility
    assert!(is_small_ship_wormhole(Some(34_134), 5_000_000.0, 1, 1));
    assert!(is_small_ship_wormhole(None, 375_000_000.0, 13, 1));
    assert!(!is_small_ship_wormhole(Some(30_660), 20_000_000.0, 1, 7));

    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 42_244, 4_500_000.0).is_ok());
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 89_607, 4_500_000.0).is_ok());
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 42_244, 6_000_000.0).is_err());

    // Test system_wormhole_profile_sde, is_drifter_system, drifter_kspace_wh_type_for_class
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_005).0, 12);
    assert!(system_wormhole_profile_sde(&ctx.sde, 31_000_005).1); // Thera
    assert!(is_drifter_system(31_000_001));
    assert!(!is_drifter_system(30_000_142));
    assert_eq!(drifter_kspace_wh_type_for_class(14), 35_650);
    assert_eq!(drifter_kspace_wh_type_for_class(18), 35_654);

    // Test statics_for_system_sde & static_wormhole_template for Thera, Drifter, C13, W-space, K-space
    assert_eq!(statics_for_system_sde(&ctx.sde, 31_000_005).len(), 4);
    assert_eq!(statics_for_system_sde(&ctx.sde, 31_000_001).len(), 6);
    assert_eq!(statics_for_system_sde(&ctx.sde, 31_002_585).len(), 1);

    let thera_tpl = static_wormhole_template(&ctx, 31_000_005, 1_700_000_000, 0);
    assert_eq!(thera_tpl.len(), 4);
    let drifter_tpl = static_wormhole_template(&ctx, 31_000_001, 1_700_000_000, 0);
    assert!(drifter_tpl.len() >= 6);
    let c13_tpl = static_wormhole_template(&ctx, 31_002_585, 1_700_000_000, 0);
    assert!(!c13_tpl.is_empty());

    // Test K162 lifetime materialization, materialize_k162_exit_in_snap, sync_k162_mirror_in_snap, and replacement
    let mut parent = thera_tpl[0].clone();
    parent.created_unix = 1_000;
    parent.lifetime_seconds = 57_600;
    assert!(!should_materialize_k162_by_lifetime(&parent, 2_000));
    assert!(should_materialize_k162_by_lifetime(&parent, 6_000));

    let dest_sys = parent.destination_system_id.unwrap_or(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mat = materialize_k162_exit_in_snap(&ctx, snap, &parent, dest_sys);
        assert!(mat.is_some());
        assert!(is_k162_materialized_in_snap(snap, dest_sys, &parent.signature_id));
        parent.remaining_mass_kg = 123_456.0;
        parent.collapsed = true;
        sync_k162_mirror_in_snap(snap, &parent);
        assert!(!is_k162_materialized_in_snap(snap, dest_sys, &parent.signature_id));
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 31_000_001, &drifter_tpl);
    });

    let rep = build_static_replacement_wormhole(&ctx, 31_000_005, &parent, 1_700_005_000, 1);
    assert!(rep.is_some());

    // Test ensure_drifter_kspace_guards
    let drk_sig = drifter_tpl.iter().find(|s| s.signature_id.starts_with("DRK-")).unwrap();
    let guards = ensure_drifter_kspace_guards(&ctx, sys_id, drk_sig, 950_000, DVec3::ZERO).await;
    assert_eq!(guards.len(), 2);

    // Verify full-SDE `WORMHOLE_PROFILES_CACHE` does not shadow a subsequent synthetic `SdeRegistry`
    let full_ctx = crate::router::ServerContext::for_tests();
    assert_eq!(system_wormhole_profile_sde(&full_ctx.sde, 31_000_005).0, 12);
    let mut custom_sde = eve_sde::SdeRegistry::default();
    custom_sde.solar_systems.insert(
        31_000_007,
        eve_sde::SolarSystem {
            solar_system_id: 31_000_007,
            constellation_id: 21_000_001,
            region_id: 11_000_001,
            name: "J-SyntheticC6".into(),
            security_status: -1.0,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: Some(6),
        },
    );
    assert_eq!(
        system_wormhole_profile_sde(&custom_sde, 31_000_007).0,
        6,
        "synthetic SdeRegistry with <= 100 systems must not be shadowed by WORMHOLE_PROFILES_CACHE"
    );
}
