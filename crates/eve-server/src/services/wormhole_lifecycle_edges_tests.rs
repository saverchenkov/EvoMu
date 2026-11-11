//! Additional edge-case and branch coverage tests for `wormhole_lifecycle_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{CosmicSignatureRecord, InventoryFlag};
use eve_proto::PyRep;
use eve_sde::{ConstellationInfo, RegionInfo, SolarSystem};
use glam::DVec3;
use std::sync::Arc;

#[test]
fn resolve_cross_system_wormhole_polarization_all_paths() {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_sys = 31_000_005_u32;
    let sig_id = "POL-101";
    let wh_bid = wormhole_item_id_for_signature(sys_id, sig_id);
    let wh_exit_bid = wormhole_exit_item_id_for_signature(other_sys, "EXT-202");

    let mut non_char_sess = sess.clone();
    non_char_sess.character_id = None;
    assert!(resolve_cross_system_wormhole_polarization(
        &ctx,
        &non_char_sess,
        &[PyRep::Integer(wh_bid)],
    )
    .is_none());

    // Missing or too-small entity arg -> None
    assert!(resolve_cross_system_wormhole_polarization(&ctx, &sess, &[]).is_none());
    assert!(
        resolve_cross_system_wormhole_polarization(&ctx, &sess, &[PyRep::Integer(1_000)]).is_none()
    );

    // Non-700M ID with no matching signature -> None
    assert!(resolve_cross_system_wormhole_polarization(
        &ctx,
        &sess,
        &[PyRep::Integer(500_123_456)],
    )
    .is_none());

    // >= 700_000_000 ID without signature falls back to bid key
    let fallback_bid = 712_345_678_i64;
    let res_none = resolve_cross_system_wormhole_polarization(
        &ctx,
        &sess,
        &[PyRep::Integer(fallback_bid)],
    );
    assert_eq!(res_none, Some(Ok(PyRep::None)));

    let now_unix = ctx.clock.read().unwrap().now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: sig_id.into(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole".into(),
                signal_strength: 1.0,
                position: [0.0, 0.0, 0.0],
                wormhole_type_id: Some(30_001),
                destination_system_id: Some(other_sys),
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                max_mass_kg: 2.0e9,
                remaining_mass_kg: 2.0e9,
                max_jump_mass_kg: 3.0e8,
                collapsed: false,
            });
        snap.cosmic_signatures
            .entry(other_sys)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "EXT-202".into(),
                solar_system_id: other_sys,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole".into(),
                signal_strength: 1.0,
                position: [0.0, 0.0, 0.0],
                wormhole_type_id: Some(30_001),
                destination_system_id: Some(sys_id),
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                max_mass_kg: 2.0e9,
                remaining_mass_kg: 2.0e9,
                max_jump_mass_kg: 3.0e8,
                collapsed: false,
            });

        // Expired polarization on fallback_bid -> PyRep::None
        snap.wh_polarizations
            .insert((char_id, fallback_bid), now_unix.saturating_sub(10));
        // Active polarization on primary wormhole item id
        snap.wh_polarizations
            .insert((char_id, wh_bid), now_unix + 180);
        // Active polarization by exit destination system key
        snap.wh_polarizations
            .insert((char_id, sys_id as i64), now_unix + 120);
    });

    assert_eq!(
        resolve_cross_system_wormhole_polarization(&ctx, &sess, &[PyRep::Integer(fallback_bid)]),
        Some(Ok(PyRep::None))
    );

    let active_primary =
        resolve_cross_system_wormhole_polarization(&ctx, &sess, &[PyRep::Integer(wh_bid)])
            .unwrap()
            .unwrap();
    match active_primary {
        PyRep::Tuple(vals) => {
            assert_eq!(vals.len(), 2);
            assert_eq!(vals[1], PyRep::Integer(300 * 10_000_000));
        }
        other => panic!("expected Tuple, got {other:?}"),
    }

    // Match by exit wormhole item id + destination key
    sess.solar_system_id = Some(other_sys);
    let active_exit =
        resolve_cross_system_wormhole_polarization(&ctx, &sess, &[PyRep::Integer(wh_exit_bid)])
            .unwrap()
            .unwrap();
    assert!(matches!(active_exit, PyRep::Tuple(ref vals) if vals.len() == 2));
}

#[test]
fn effective_jump_mass_and_small_ship_eligibility_boundaries() {
    let (mut ctx, _sess, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Missing ship hull -> falls back to ball_mass; plate without SDE/non-positive qty skipped
    let missing_hull_ship = 299_000_999_i64;
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(20_353, make_fighter_type(20_353, 329, 7, 10.0, vec![(796, 2_000_000.0)]));
    // Custom entangler with -99.0 pct -> clamped at 0.05 floor
    sde_mut
        .types
        .insert(49_099, make_fighter_type(49_099, 2008, 7, 10.0, vec![(1131, -99.0)]));
    // Module with type 49_099 replacement test: positive +50% mass mod in group 2008 -> clamped at 1.0
    sde_mut
        .types
        .insert(990_001, make_fighter_type(990_001, 2008, 7, 10.0, vec![(1131, 50.0)]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            901_001,
            make_fighter_item(901_001, 20_353, char_id, missing_hull_ship, InventoryFlag::LoSlot0, 0),
        );
        snap.module_online_states.insert(901_001, true);
        snap.items.insert(
            901_002,
            make_fighter_item(901_002, 49_099, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_active_states.insert(901_002);
        snap.items.insert(
            901_003,
            make_fighter_item(901_003, 990_001, char_id, ship_id, InventoryFlag::MedSlot1, 1),
        );
        snap.module_active_states.insert(901_003);
    });

    // Missing hull + zero qty plate -> exactly ball_mass
    let (m_missing, p_missing) =
        compute_effective_ship_jump_mass(&ctx, missing_hull_ship, 12_345_678.0);
    assert!((m_missing - 12_345_678.0).abs() < 1e-6);
    assert_eq!(p_missing, 0.0);

    // Ball mass higher than base + plates; entangler clamps floor at 0.05 while +50% clamps at 1.0
    let ship_hull_tid = ctx.db.with_snapshot(|snap| snap.items[&ship_id].type_id);
    let base_hull_mass = ctx.sde.types[&ship_hull_tid].mass;
    let (m_ship, _) = compute_effective_ship_jump_mass(&ctx, ship_id, 500_000_000.0);
    assert!((m_ship - base_hull_mass * 0.05).abs() < 1.0);

    // Porpoise (42244) & Odysseus (89607) small-ship bypass vs unknown type rejected
    let sde_mut2 = Arc::make_mut(&mut ctx.sde);
    sde_mut2.types.remove(&42_244);
    sde_mut2.types.remove(&89_607);
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 42_244, 4_999_999.0).is_ok());
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 89_607, 4_999_999.0).is_ok());
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 42_244, 5_000_001.0).is_err());
    assert!(check_small_ship_wormhole_eligibility(&ctx.sde, 999_888_777, 1_000.0).is_err());
}

#[test]
fn system_profile_fallbacks_destination_exclusions_and_static_templates() {
    let (mut ctx, _sess, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Find a real WH system with moons vs a shattered system without moons in SDE
    let with_moon_wh = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.wormhole_class_id.is_some_and(|c| (1..=6).contains(&c))
                && ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 8)
        })
        .map(|s| s.solar_system_id);
    assert!(with_moon_wh.map_or(true, |sid| !super::super::compute_single_system_profile(&ctx.sde, sid).1));

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    // Synthetic systems for constellation/region wormhole_class_id fallbacks + moon/no-moon shattered
    let reg_id = 19_999_001_u32;
    let con_id = 29_999_001_u32;
    let con_no_wh = 29_999_002_u32;
    sde_mut.regions.insert(
        reg_id,
        RegionInfo {
            region_id: reg_id,
            name: "SyntheticWHRegion".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(4),
        },
    );
    sde_mut.constellations.insert(
        con_id,
        ConstellationInfo {
            constellation_id: con_id,
            region_id: reg_id,
            name: "ConWithClass".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(2),
        },
    );
    sde_mut.constellations.insert(
        con_no_wh,
        ConstellationInfo {
            constellation_id: con_no_wh,
            region_id: reg_id,
            name: "ConFallbackRegion".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    let make_sys = |sid: u32, cid: u32, wh_cls: Option<i32>| SolarSystem {
        solar_system_id: sid,
        constellation_id: cid,
        region_id: reg_id,
        name: format!("Sys{sid}"),
        security_status: -1.0,
        radius: 1.0e12,
        galactic_pos_ly: [0.0; 3],
        planet_ids: vec![],
        stations: vec![],
        wormhole_class_id: wh_cls,
    };
    sde_mut.solar_systems.insert(31_999_001, make_sys(31_999_001, con_id, None));
    sde_mut.solar_systems.insert(31_999_002, make_sys(31_999_002, con_no_wh, None));
    sde_mut.solar_systems.insert(31_999_003, make_sys(31_999_003, con_no_wh, Some(99)));

    // Ensure missing SDE IDs map to expected fallbacks
    sde_mut.solar_systems.remove(&31_002_580);
    sde_mut.solar_systems.remove(&31_002_510);

    assert_eq!(super::super::compute_single_system_profile(&ctx.sde, 31_999_001).0, 2);
    assert_eq!(super::super::compute_single_system_profile(&ctx.sde, 31_999_002).0, 4);
    assert_eq!(super::super::compute_single_system_profile(&ctx.sde, 31_999_003).0, 0);
    assert_eq!(
        (
            super::super::compute_single_system_profile(&ctx.sde, 31_002_580).0,
            super::super::compute_single_system_profile(&ctx.sde, 31_002_580).2
        ),
        (13, true)
    );
    assert_eq!(
        (
            super::super::compute_single_system_profile(&ctx.sde, 31_002_510).0,
            super::super::compute_single_system_profile(&ctx.sde, 31_002_510).2
        ),
        (6, true)
    );
    assert_eq!(
        (
            system_wormhole_profile(31_002_510).0,
            system_wormhole_profile(31_002_510).2
        ),
        (1, true)
    );
    assert_eq!(super::super::compute_single_system_profile(&ctx.sde, 31_998_777).0, 5);
    assert_eq!(super::super::compute_single_system_profile(&ctx.sde, 30_999_888).0, 0);

    // Destination picker with NullSec exclusions and exclude_sys_id fallback
    let null_pick =
        pick_destination_system_for_target_class_excluding(&ctx.sde, 30_000_142, 9, 0, Some(77));
    assert!(null_pick > 0);
    if let Some(ss) = ctx.sde.solar_systems.get(&null_pick) {
        assert!(!matches!(
            ss.region_id,
            10_000_070 | 10_000_004 | 10_000_017 | 10_000_019 | 10_001_000 | 30_100_000
        ));
    }
    let no_match_dest =
        pick_destination_system_for_target_class_excluding(&ctx.sde, 30_000_142, 255, 0, Some(1));
    assert_eq!(no_match_dest, 30_000_142);

    // Single-candidate exclude fallback returns the single matching candidate rather than dropping
    let thera_only =
        pick_destination_system_for_target_class_excluding(&ctx.sde, 30_000_142, 12, 1, Some(31_000_005));
    assert_eq!(thera_only, 31_000_005);

    // Deterministic static_wormhole_template for K-space (seq=0 and seq=1) and W-space (31_000_100)
    let kspace_static0 = static_wormhole_template(&ctx, 30_000_142, 1_700_000_000, 0);
    for sig in &kspace_static0 {
        assert!(
            sig.signature_id.starts_with("JMM-")
                || sig.signature_id.starts_with("PKW-")
                || sig.signature_id.starts_with("WND-")
        );
    }
    let kspace_static1 = static_wormhole_template(&ctx, 30_000_142, 1_700_000_000, 1);
    assert_eq!(kspace_static1.len(), 1);
    assert!(kspace_static1[0].signature_id.starts_with("STA-"));

    let wspace_static = static_wormhole_template(&ctx, 31_000_100, 1_700_000_000, 0);
    assert!(!wspace_static.is_empty());
    assert!(
        wspace_static[0].signature_id.starts_with("WST-")
            || wspace_static[0].signature_id.starts_with("WST2-")
            || wspace_static[0].signature_id.starts_with("WSK-")
            || wspace_static[0].signature_id.starts_with("WND-")
            || wspace_static[0].signature_id.starts_with("WPC-")
            || wspace_static[0].signature_id.starts_with("WTH-")
    );

    // build_wh_from_type fallback on missing wormhole type
    let fallback_wh = build_wh_from_type(
        &ctx.sde,
        "TST-001".into(),
        31_999_001,
        [0.0, 0.0, 0.0],
        999_777_666,
        30_000_142,
        1_700_000_000,
        "UnknownWH",
        86_400,
        2_000_000_000.0,
        300_000_000.0,
    );
    assert_eq!(fallback_wh.lifetime_seconds, 86_400);
    assert_eq!(fallback_wh.max_mass_kg, 2_000_000_000.0);
    assert_eq!(fallback_wh.max_jump_mass_kg, 300_000_000.0);
}

#[tokio::test]
async fn replacement_wormholes_k162_edge_guards_and_drifter_beacon_idempotency() {
    let (ctx, _sess, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // build_static_replacement_wormhole for PCH (replaces with different Pochven system), WTH (roaming -> None), WST (replaces static)
    let pch_parent = CosmicSignatureRecord {
        signature_id: "PCH-009".into(),
        solar_system_id: 31_000_005,
        sig_group: "Wormhole".into(),
        type_name: "Unstable Wormhole".into(),
        signal_strength: 1.0,
        position: [1_000.0, 2_000.0, 3_000.0],
        wormhole_type_id: None,
        destination_system_id: Some(30_002_444),
        created_unix: 1_700_000_000,
        lifetime_seconds: 57_600,
        max_mass_kg: 1.0e9,
        remaining_mass_kg: 1.0e9,
        max_jump_mass_kg: 3.0e8,
        collapsed: false,
    };
    let rep_pch = build_static_replacement_wormhole(&ctx, 31_000_005, &pch_parent, 1_700_010_000, 3)
        .expect("PCH replacement");
    assert_ne!(rep_pch.destination_system_id, Some(30_002_444));
    assert!(rep_pch.wormhole_type_id.is_some());

    let wth_parent = CosmicSignatureRecord {
        signature_id: "WTH-003".into(),
        destination_system_id: Some(31_000_005),
        wormhole_type_id: Some(999_666_555),
        ..pch_parent.clone()
    };
    assert!(build_static_replacement_wormhole(&ctx, 31_000_001, &wth_parent, 1_700_010_000, 4).is_none());

    let wst_parent = CosmicSignatureRecord {
        signature_id: "WST-001".into(),
        type_name: "CustomStatic".into(),
        wormhole_type_id: Some(999_666_555),
        ..pch_parent.clone()
    };
    let rep_wst = build_static_replacement_wormhole(&ctx, 31_000_001, &wst_parent, 1_700_010_000, 4)
        .expect("WST replacement");
    assert_eq!(rep_wst.type_name, "CustomStatic");

    // Duplicate K162 guard -> first materializes, second returns None; sync_k162 on None dest is a no-op
    let mut no_dest = pch_parent.clone();
    no_dest.destination_system_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        assert!(materialize_k162_exit_in_snap(&ctx, snap, &pch_parent, 30_000_142).is_some());
        assert!(materialize_k162_exit_in_snap(&ctx, snap, &pch_parent, 30_000_142).is_none());
        sync_k162_mirror_in_snap(snap, &no_dest);
    });

    // ensure_drifter_kspace_guards returns empty on non-DRK signature, and is idempotent on second call
    let non_drk_guards =
        ensure_drifter_kspace_guards(&ctx, 30_000_142, &pch_parent, 960_000, DVec3::ZERO).await;
    assert!(non_drk_guards.is_empty());

    let drifter_tpl = static_wormhole_template(&ctx, 31_000_001, 1_700_000_000, 0);
    let drk_sig = drifter_tpl
        .iter()
        .find(|s| s.signature_id.starts_with("DRK-"))
        .expect("DRK static");
    let kspace_dest = drk_sig.destination_system_id.unwrap();
    let g1 = ensure_drifter_kspace_guards(&ctx, kspace_dest, drk_sig, 961_000, DVec3::ZERO).await;
    let g2 = ensure_drifter_kspace_guards(&ctx, kspace_dest, drk_sig, 961_000, DVec3::ZERO).await;
    assert_eq!(g1.len(), 2);
    assert_eq!(g2, g1);

    // seed_drifter_kspace_beacons_in_snap is idempotent when called twice
    ctx.db.with_mut_snapshot(|snap| {
        let count_before = snap
            .items
            .values()
            .filter(|i| i.location_id == kspace_dest as i64 && i.type_id == 41_547)
            .count();
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 31_000_001, &drifter_tpl);
        let count_after = snap
            .items
            .values()
            .filter(|i| i.location_id == kspace_dest as i64 && i.type_id == 41_547)
            .count();
        assert_eq!(count_before, count_after);
    });
}
