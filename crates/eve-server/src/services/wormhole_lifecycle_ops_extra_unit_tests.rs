//! Extra unit and mutation-killing tests for `services::wormhole_lifecycle_ops`
//! covering celestial fallback positions, effective ship jump mass (offline/online plates,
//! active/inactive prop mods, Zero-Point Mass Entangler `pct == 0.0` vs custom), small-ship
//! wormhole eligibility across all 17 ship groups and mass boundaries, wormhole type/destination
//! selectors, static specs, replacement wormhole generation, K162 54_000s boundary, and Drifter guards.

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::space_destiny::AU_IN_METERS;
use crate::services::wormhole_lifecycle_ops::*;
use eve_db::InventoryFlag;
use eve_sde::SolarSystem;
use std::sync::Arc;

#[tokio::test]
async fn pick_system_celestial_position_with_and_without_celestials() {
    let (mut ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // 1. System 39_999_999 has no celestials -> trigonometric 8.0 AU ring fallback & None celestial_id
    let (pos_empty, cel_empty) = pick_system_celestial_position(&ctx, 39_999_999, 0);
    assert!(cel_empty.is_none());
    assert!((pos_empty[0] - 8.0 * AU_IN_METERS).abs() < 1.0);
    assert!((pos_empty[1] - 1.5 * AU_IN_METERS).abs() < 1.0);
    assert!(pos_empty[2].abs() < 1.0);

    // 2. Attach default_db_path so celestials_in_system(sys_id) returns real celestials (+15_000, -10_000, +5_000)
    Arc::make_mut(&mut ctx.sde).db_path = Some(eve_sde::SdeRegistry::default_db_path());
    let celestials = ctx.sde.celestials_in_system(sys_id);
    assert!(!celestials.is_empty());
    let (pos_cel, cel_some) = pick_system_celestial_position(&ctx, sys_id, 0);
    assert_eq!(cel_some, Some(celestials[0].celestial_id));
    assert!((pos_cel[0] - (celestials[0].position.x + 15_000.0)).abs() < 1e-6);
    assert!((pos_cel[1] - (celestials[0].position.y - 10_000.0)).abs() < 1e-6);
    assert!((pos_cel[2] - (celestials[0].position.z + 5_000.0)).abs() < 1e-6);
}

#[tokio::test]
async fn compute_effective_ship_jump_mass_plates_prop_mods_and_entangler_branches() {
    let (mut ctx, _session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    let plate_tid = 980_001u32;
    let mwd_tid = 980_002u32;
    let entangler_zero_tid = 980_003u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        // Ship hull mass = 10,000,000 kg
        if let Some(ship_item) = ctx.db.get_item(ship_id) {
            if let Some(st) = sde_mut.types.get_mut(&ship_item.type_id) {
                st.mass = 10_000_000.0;
            }
        }
        sde_mut.types.insert(
            plate_tid,
            make_fighter_type(plate_tid, 329, 7, 10.0, vec![(796, 2_000_000.0)]),
        );
        // Group 46 = Propulsion Module (MWD/AB) adding 5,000,000 kg when active
        sde_mut.types.insert(
            mwd_tid,
            make_fighter_type(mwd_tid, 46, 7, 10.0, vec![(796, 5_000_000.0)]),
        );
        // Group 2008 = Zero-Point Mass Entangler with attr 1131 == 0.0 -> fallback 0.20x multiplier
        sde_mut.types.insert(
            entangler_zero_tid,
            make_fighter_type(entangler_zero_tid, 2008, 7, 10.0, vec![(1131, 0.0)]),
        );
    }

    let plate_id = 730_001i64;
    let cargo_plate_id = 730_002i64;
    let mwd_id = 730_003i64;
    let entangler_id = 730_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Fitted plate in LoSlot0, initially OFFLINE -> should NOT add mass
        snap.items.insert(
            plate_id,
            make_fighter_item(plate_id, plate_tid, char_id, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.module_online_states.insert(plate_id, false);
        // Plate in Cargo (not flag 11..=34) -> should NOT add mass even if online
        snap.items.insert(
            cargo_plate_id,
            make_fighter_item(cargo_plate_id, plate_tid, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
        snap.module_online_states.insert(cargo_plate_id, true);
        // MWD in MedSlot0, initially INACTIVE -> should NOT add prop_mass
        snap.items.insert(
            mwd_id,
            make_fighter_item(mwd_id, mwd_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
    });

    // 1. Offline plate & inactive MWD with ball_mass = 0.0 -> base_hull_mass (10M), prop = 0
    let (m0, p0) = compute_effective_ship_jump_mass(&ctx, ship_id, 0.0);
    assert!((m0 - 10_000_000.0).abs() < 1.0);
    assert_eq!(p0, 0.0);

    // 2. Online the fitted plate -> 12M kg
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(plate_id, true);
    });
    let (m1, p1) = compute_effective_ship_jump_mass(&ctx, ship_id, 0.0);
    assert!((m1 - 12_000_000.0).abs() < 1.0);
    assert_eq!(p1, 0.0);

    // 3. Activate MWD:
    //    - when ball_mass (12M) <= base + plate + 1.0 -> adds prop_mass (12M + 5M = 17M)
    //    - when ball_mass (17M) > base + plate + 1.0 -> already includes prop_mass in ball_mass (stays 17M, not 22M)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mwd_id);
    });
    let (m2_unbaked, p2) = compute_effective_ship_jump_mass(&ctx, ship_id, 12_000_000.0);
    assert!((m2_unbaked - 17_000_000.0).abs() < 1.0);
    assert!((p2 - 5_000_000.0).abs() < 1.0);

    let (m2_baked, _) = compute_effective_ship_jump_mass(&ctx, ship_id, 17_000_000.0);
    assert!((m2_baked - 17_000_000.0).abs() < 1.0);

    // 4. Activate Zero-Point Mass Entangler with attr 1131 == 0.0 -> 0.20x of (10M + 2M) = 2.4M, prop_add = 0.0
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            entangler_id,
            make_fighter_item(
                entangler_id,
                entangler_zero_tid,
                char_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.module_active_states.insert(entangler_id);
    });
    let (m3_ent, p3_ent) = compute_effective_ship_jump_mass(&ctx, ship_id, 12_000_000.0);
    assert!((m3_ent - 2_400_000.0).abs() < 1.0);
    assert_eq!(p3_ent, 0.0);

    // When ball_mass > 0.0 && ball_mass < base_hull_mass (e.g. 1_500_000.0 < 2_400_000.0), takes ball_mass.min(reduced_hull)
    let (m3_low_ball, _) = compute_effective_ship_jump_mass(&ctx, ship_id, 1_500_000.0);
    assert!((m3_low_ball - 1_500_000.0).abs() < 1.0);

    // Custom dynamic attribute 1131 = -50.0 -> 0.50x of 12M = 6.0M
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&entangler_id)
            .unwrap()
            .dynamic_attributes
            .insert(1131, -50.0);
    });
    let (m4_half, _) = compute_effective_ship_jump_mass(&ctx, ship_id, 12_000_000.0);
    assert!((m4_half - 6_000_000.0).abs() < 1.0);
}

#[test]
fn small_ship_wormhole_detection_and_eligibility_across_all_groups_and_mass_bounds() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // 1. is_small_ship_wormhole conditions
    assert!(is_small_ship_wormhole(None, 300_000_000.0, 13, 1));
    assert!(is_small_ship_wormhole(None, 300_000_000.0, 1, 13));
    for tid in [34_134u32, 34_135, 34_136, 34_137, 34_138, 34_139, 34_140, 34_439] {
        assert!(is_small_ship_wormhole(Some(tid), 300_000_000.0, 1, 1));
    }
    assert!(is_small_ship_wormhole(None, 5_000_000.0, 1, 1));
    assert!(!is_small_ship_wormhole(None, 5_000_000.1, 1, 1));
    assert!(!is_small_ship_wormhole(None, 0.0, 1, 1));

    // 2. check_small_ship_wormhole_eligibility across all 17 eligible groups + cruiser rejection
    let eligible_groups: &[u32] = &[
        25, 31, 237, 324, 830, 831, 834, 893, 1283, 1527, // frigates/shuttle/corvette
        420, 541, 1305, 1534,                             // destroyers
        941,                                              // industrial command (Porpoise)
        894, 4902,                                        // HIC / Odysseus
    ];
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    for (idx, &gid) in eligible_groups.iter().enumerate() {
        let tid = 970_000 + idx as u32;
        sde_mut.types.insert(tid, make_fighter_type(tid, gid, 6, 1_000_000.0, vec![]));
        assert!(
            check_small_ship_wormhole_eligibility(sde_mut, tid, 5_000_000.0).is_ok(),
            "group {gid} at exactly 5,000,000 kg should be eligible"
        );
        assert_eq!(
            check_small_ship_wormhole_eligibility(sde_mut, tid, 5_000_000.1)
                .unwrap_err()
                .code,
            "ShipTooHeavyForWormhole"
        );
    }
    // Ineligible Cruiser (group 26) even at light mass
    sde_mut
        .types
        .insert(970_999, make_fighter_type(970_999, 26, 6, 1_000_000.0, vec![]));
    assert_eq!(
        check_small_ship_wormhole_eligibility(sde_mut, 970_999, 2_000_000.0)
            .unwrap_err()
            .code,
        "ShipTooHeavyForWormhole"
    );
}

#[test]
fn drifter_types_small_wh_types_and_wormhole_type_lookup_branches() {
    let (ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // 1. drifter_kspace_wh_type_for_class for 14..=18 and fallback
    assert_eq!(drifter_kspace_wh_type_for_class(14), 35_650);
    assert_eq!(drifter_kspace_wh_type_for_class(15), 35_651);
    assert_eq!(drifter_kspace_wh_type_for_class(16), 35_652);
    assert_eq!(drifter_kspace_wh_type_for_class(17), 35_653);
    assert_eq!(drifter_kspace_wh_type_for_class(18), 35_654);
    assert_eq!(drifter_kspace_wh_type_for_class(99), 35_650);

    // 2. small_ship_wormhole_type_for_target_class for 1..=6, 9, and fallback
    assert_eq!(small_ship_wormhole_type_for_target_class(1), 34_134);
    assert_eq!(small_ship_wormhole_type_for_target_class(2), 34_135);
    assert_eq!(small_ship_wormhole_type_for_target_class(3), 34_136);
    assert_eq!(small_ship_wormhole_type_for_target_class(4), 34_137);
    assert_eq!(small_ship_wormhole_type_for_target_class(5), 34_138);
    assert_eq!(small_ship_wormhole_type_for_target_class(6), 34_139);
    assert_eq!(small_ship_wormhole_type_for_target_class(9), 34_140);
    assert_eq!(small_ship_wormhole_type_for_target_class(7), 34_439);

    // 3. is_drifter_wormhole across origin, dest, wormhole_type_id, and type_name
    let base_sig = make_wh_sig(
        "TEST-001".into(),
        30_000_142,
        "Wormhole".into(),
        [1.0, 0.0, 0.0],
        30_579,
        30_002_187,
        1_000,
        3_600,
        1_000.0,
        1_000.0,
    );
    assert!(!is_drifter_wormhole(&ctx.sde, &base_sig));
    let mut sig_orig = base_sig.clone();
    sig_orig.solar_system_id = 31_000_002;
    assert!(is_drifter_wormhole(&ctx.sde, &sig_orig));

    let mut sig_dest = base_sig.clone();
    sig_dest.destination_system_id = Some(31_000_006);
    assert!(is_drifter_wormhole(&ctx.sde, &sig_dest));

    let mut sig_tid = base_sig.clone();
    sig_tid.wormhole_type_id = Some(34_494);
    assert!(is_drifter_wormhole(&ctx.sde, &sig_tid));

    let mut sig_name = base_sig.clone();
    sig_name.type_name = "Unidentified Wormhole".into();
    assert!(is_drifter_wormhole(&ctx.sde, &sig_name));

    // 4. make_site_sig helper
    let site = make_site_sig("REL-101".into(), 30_000_142, "Relic", "Ruined Core", [2.0, -1.0, 0.5], 5_000);
    assert_eq!(site.sig_group, "Relic");
    assert_eq!(site.lifetime_seconds, 86_400);
    assert!(site.wormhole_type_id.is_none());

    // 5. lookup_wormhole_type_for_origin_and_target across origin classes 0..=6, 13, and impossible target_class fallback (30_579)
    for orig in [0u8, 1, 2, 3, 4, 5, 6, 13] {
        let tid = lookup_wormhole_type_for_origin_and_target(&ctx.sde, orig, 3, 7);
        assert!(tid > 0);
    }
    assert_eq!(lookup_wormhole_type_for_target_class(&ctx.sde, 999), 30_579);
}

#[test]
fn destination_picking_static_specs_templates_and_k162_lifetime_boundaries() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Seed systems for target_class 7 (HighSec), 8 (LowSec), 9 (NullSec), 12 (Thera), 25 (Pochven), and C1..=C6
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mk_sys = |id: u32, reg: u32, sec: f64, wh_cls: Option<i32>| SolarSystem {
            solar_system_id: id,
            constellation_id: 20_000_001 + (id % 12),
            region_id: reg,
            name: format!("Sys-{id}"),
            security_status: sec,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: wh_cls,
        };
        sde_mut.solar_systems.insert(30_000_142, mk_sys(30_000_142, 10_000_002, 0.9, Some(7)));
        sde_mut.solar_systems.insert(30_000_144, mk_sys(30_000_144, 10_000_002, 0.9, Some(7)));
        sde_mut.solar_systems.insert(30_002_537, mk_sys(30_002_537, 10_000_030, 0.3, Some(8)));
        sde_mut.solar_systems.insert(30_004_759, mk_sys(30_004_759, 10_000_060, -0.5, Some(9)));
        sde_mut.solar_systems.insert(30_000_157, mk_sys(30_000_157, 10_000_070, -1.0, Some(25)));
        sde_mut.solar_systems.insert(31_000_005, mk_sys(31_000_005, 11_000_031, -1.0, Some(12)));
        for c in 1..=6 {
            let sid = 31_000_100 + c as u32;
            sde_mut.solar_systems.insert(sid, mk_sys(sid, 11_000_000 + c as u32, -1.0, Some(c)));
        }
    }

    // 1. pick_destination_system_for_target_class_excluding respects exclude_sys_id when alternatives exist
    let d_hs = pick_destination_system_for_target_class_excluding(
        &ctx.sde,
        31_000_005,
        7,
        0,
        Some(30_000_142),
    );
    assert_ne!(d_hs, 30_000_142);
    assert_eq!(
        pick_destination_system_for_target_class(&ctx.sde, 31_000_005, 12, 0),
        31_000_005
    );
    // Impossible class falls back to source_sys_id
    assert_eq!(
        pick_destination_system_for_target_class(&ctx.sde, 30_000_142, 999, 0),
        30_000_142
    );

    // 2. wspace_static_specs_for_system and statics_for_system_sde across C1..=C6, shattered C1..=C6, and K-space
    for c in 1..=6u32 {
        let normal_sys = 31_000_006 + c;
        let specs = wspace_static_specs_for_system(&ctx.sde, normal_sys);
        assert!(!specs.is_empty());
        let statics = statics_for_system_sde(&ctx.sde, normal_sys);
        assert_eq!(statics.len(), specs.len());

        // Shattered C1..=C6 systems (31_002_505..=31_002_510) include an extra "WSK" KSpace-Shattered-Static
        let shat_sys = 31_002_504 + c;
        let shat_specs = wspace_static_specs_for_system(&ctx.sde, shat_sys);
        assert!(shat_specs.iter().any(|(prefix, _, _, _)| *prefix == "WSK"));
    }
    let k_statics = statics_for_system_sde(&ctx.sde, 30_000_142);
    assert_eq!(k_statics[0].0, "Wandering");

    // 3. static_wormhole_template for seq > 0 in Drifter, C13, W-space, K-space, and Pochven
    let drifter_seq1 = static_wormhole_template(&ctx, 31_000_001, 1_700_000_000, 1);
    assert_eq!(drifter_seq1.len(), 6, "Drifter seq > 0 only rolls the 6 W-space statics");
    let c13_seq1 = static_wormhole_template(&ctx, 31_002_585, 1_700_000_000, 1);
    assert_eq!(c13_seq1.len(), 1);
    let wspace_seq1 = static_wormhole_template(&ctx, 31_000_008, 1_700_000_000, 1);
    assert!(!wspace_seq1.is_empty());
    let kspace_seq1 = static_wormhole_template(&ctx, 30_000_142, 1_700_000_000, 1);
    assert_eq!(kspace_seq1.len(), 1);
    assert!(kspace_seq1[0].signature_id.starts_with("STA-"));
    let poch_tpl = static_wormhole_template(&ctx, 30_000_157, 1_700_000_000, 0);
    assert!(!poch_tpl.is_empty());

    // 4. build_static_replacement_wormhole returns None for wandering/exit prefixes and Some for static prefixes
    for non_static_prefix in ["WND-101", "K162-WST-101", "WTH-701", "WPC-601", "PKW-501"] {
        let mut wh = wspace_seq1[0].clone();
        wh.signature_id = non_static_prefix.into();
        assert!(build_static_replacement_wormhole(&ctx, 31_000_008, &wh, 1_700_001_000, 1).is_none());
    }
    let wst_rep = build_static_replacement_wormhole(&ctx, 31_000_008, &wspace_seq1[0], 1_700_001_000, 1);
    assert!(wst_rep.is_some());

    // 5. should_materialize_k162_by_lifetime exact 54_000s (15h) boundary & negative guards
    let mut sig = wspace_seq1[0].clone();
    sig.created_unix = 10_000;
    sig.lifetime_seconds = 86_400;
    // Remaining = 86_400 - (42_400 - 10_000) = 54_000 -> NOT < 54_000 (false)
    assert!(!should_materialize_k162_by_lifetime(&sig, 42_400));
    // Remaining = 86_400 - (42_401 - 10_000) = 53_999 -> < 54_000 (true)
    assert!(should_materialize_k162_by_lifetime(&sig, 42_401));
    // Expired (now_unix == created + lifetime) -> false
    assert!(!should_materialize_k162_by_lifetime(&sig, 96_400));
    // K162 prefix -> false
    let mut k162_sig = sig.clone();
    k162_sig.signature_id = "K162-WST-101".into();
    assert!(!should_materialize_k162_by_lifetime(&k162_sig, 42_401));
}
