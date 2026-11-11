//! Edge-case unit tests for `wormhole_lifecycle_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_sde::SolarSystem;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

use eve_sde::SdeRegistry;

#[test]
fn resolve_polarization_profiles_and_destination_edge_cases() {
    let (mut ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.sde = shared_sde();

    // Unauthenticated session -> None
    let mut unauth = eve_machonet::SessionState::default();
    assert!(resolve_cross_system_wormhole_polarization(&ctx, &unauth, &[eve_proto::PyRep::Integer(700_000_001)]).is_none());
    let _ = &mut unauth;

    // Empty args / bid < 40_000_000 -> None
    assert!(resolve_cross_system_wormhole_polarization(&ctx, &session, &[]).is_none());
    assert!(resolve_cross_system_wormhole_polarization(&ctx, &session, &[eve_proto::PyRep::Integer(39_999_999)]).is_none());

    // Signature not found and bid in 40_000_000..700_000_000 -> None
    assert!(resolve_cross_system_wormhole_polarization(&ctx, &session, &[eve_proto::PyRep::Integer(50_000_001)]).is_none());

    // bid >= 700_000_000 fallback: active polarization -> Tuple(end_ft, duration), expired -> PyRep::None
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let fallback_bid = 700_123_456_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.insert((char_id, fallback_bid), now + 180);
    });
    let res_active = resolve_cross_system_wormhole_polarization(&ctx, &session, &[eve_proto::PyRep::Integer(fallback_bid)])
        .expect("Some")
        .expect("Ok");
    assert!(matches!(res_active, eve_proto::PyRep::Tuple(ref v) if v.len() == 2));

    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.insert((char_id, fallback_bid), 10); // expired
    });
    assert_eq!(
        resolve_cross_system_wormhole_polarization(&ctx, &session, &[eve_proto::PyRep::Integer(fallback_bid)])
            .unwrap()
            .unwrap(),
        eve_proto::PyRep::None
    );

    // Match cosmic_signatures wormhole by exit K162 id and resolve via dest fallback
    let dest_sys = 31_000_005_u32;
    let sig = make_wh_sig(
        "POL-101".into(),
        sys_id,
        "Wormhole C1".into(),
        [1.0, 2.0, 3.0],
        30_671,
        dest_sys,
        now,
        86_400,
        1_000_000_000.0,
        300_000_000.0,
    );
    let k162_bid = wormhole_exit_item_id_for_signature(sys_id, &sig.signature_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(sig.clone());
        snap.wh_polarizations.insert((char_id, dest_sys as i64), now + 240);
    });
    let res_dest = resolve_cross_system_wormhole_polarization(&ctx, &session, &[eve_proto::PyRep::Integer(k162_bid)])
        .unwrap()
        .unwrap();
    assert!(matches!(res_dest, eve_proto::PyRep::Tuple(_)));

    // compute_single_system_profile fallback branches for systems absent from SDE solar_systems
    let tiny_sde = SdeRegistry::default();
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_001).0, 14);
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_002).0, 15);
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_003).0, 16);
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_004).0, 17);
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_005), (12, true, true, "None"));
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_000_006).0, 18);
    assert_eq!(compute_single_system_profile(&tiny_sde, 31_002_590), (13, false, true, "Wolf-Rayet"));
    assert!(compute_single_system_profile(&tiny_sde, 31_002_510).2); // standard shattered
    assert!(!compute_single_system_profile(&tiny_sde, 31_000_100).2); // regular w-space
    assert_eq!(compute_single_system_profile(&tiny_sde, 30_000_142).0, 0);

    // Constellation/region fallback for wormhole_class_id when solar_system.wormhole_class_id is None
    let mut custom_sde = SdeRegistry::default();
    custom_sde.solar_systems.insert(
        31_999_001,
        SolarSystem {
            solar_system_id: 31_999_001,
            name: "ConstWH".into(),
            region_id: 19_000_001,
            constellation_id: 29_000_001,
            security_status: -1.0,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    custom_sde.constellations.insert(
        29_000_001,
        eve_sde::ConstellationInfo {
            constellation_id: 29_000_001,
            name: "C".into(),
            region_id: 19_000_001,
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(4),
        },
    );
    assert_eq!(compute_single_system_profile(&custom_sde, 31_999_001).0, 4);

    // system_wormhole_profile_sde on full SDE hits >100 systems cache path
    let (thera_cls, is_thera, is_shattered, _) = system_wormhole_profile_sde(&ctx.sde, 31_000_005);
    assert_eq!(thera_cls, 12);
    assert!(is_thera && is_shattered);
    let _ = &mut session;
}

#[tokio::test]
async fn pick_destination_static_templates_replacement_k162_and_drifter_guards() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    ctx.sde = shared_sde();
    let now = 1_775_400_000_u64;

    // pick_destination_system_for_target_class_excluding fallback when exclusion empties candidate pool
    let mut small_sde = SdeRegistry::default();
    small_sde.solar_systems.insert(
        30_000_142,
        SolarSystem {
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Jita".into(),
            security_status: 0.95,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    small_sde.solar_systems.insert(
        30_000_144,
        SolarSystem {
            solar_system_id: 30_000_144,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Perimeter".into(),
            security_status: 0.90,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    // Excluding the only candidate 30_000_144 falls back to [30_000_144]
    assert_eq!(
        pick_destination_system_for_target_class_excluding(&small_sde, 30_000_142, 7, 1, Some(30_000_144)),
        30_000_144
    );
    // Unmatched target_class when source_sys_id == 0 falls back to a K-space system
    assert!([30_000_142, 30_000_144].contains(
        &pick_destination_system_for_target_class_excluding(&small_sde, 0, 12, 1, None)
    ));
    assert_eq!(
        pick_destination_system_for_target_class_excluding(&small_sde, 30_000_142, 12, 1, None),
        30_000_142
    );

    // Run static_wormhole_template with seq = 0 across Pochven, Thera, Drifter, C13, W-space (C1..C6), and K-space
    // so probabilistic branches (<35%, <12%, <8%, <25%) are all exercised deterministically.
    assert!(!static_wormhole_template(&ctx, 31_000_005, now, 0).is_empty()); // Thera
    assert!(!static_wormhole_template(&ctx, 31_000_001, now, 0).is_empty()); // Drifter
    let c13_sys = *ctx
        .sde
        .solar_systems
        .keys()
        .find(|&&id| system_wormhole_profile_sde(&ctx.sde, id).0 == 13)
        .unwrap_or(&31_002_580);
    for seq in 0..=1 {
        let _ = static_wormhole_template(&ctx, c13_sys, now, seq);
    }
    let poch_sys = *ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| &s.solar_system_id)
        .expect("Pochven system");
    let poch_tpl = static_wormhole_template(&ctx, poch_sys, now, 1);
    assert!(!poch_tpl.is_empty());

    for &sys_id in ctx
        .sde
        .solar_systems
        .keys()
        .filter(|&&id| (31_000_007..31_000_500).contains(&id))
        .take(40)
    {
        let _ = static_wormhole_template(&ctx, sys_id, now, 0);
    }
    for &sys_id in ctx
        .sde
        .solar_systems
        .keys()
        .filter(|&&id| (30_000_001..30_000_250).contains(&id))
        .take(40)
    {
        let _ = static_wormhole_template(&ctx, sys_id, now, 0);
    }

    // build_static_replacement_wormhole edge cases:
    // 1. Wandering prefixes rejected -> None
    for prefix in ["WND-100", "K162-100", "WTH-100", "WPC-100", "PKW-100"] {
        let mut s = make_wh_sig(prefix.into(), 31_000_010, "WH".into(), [0.0; 3], 30_671, 30_000_142, now, 86_400, 1e9, 3e8);
        s.wormhole_type_id = Some(30_671);
        assert!(build_static_replacement_wormhole(&ctx, 31_000_010, &s, now, 1).is_none());
    }
    // 2. Signature collision bumps eff_seq, unknown wormhole_type_id preserves collapsed.type_name & infers prev_dest class
    let w_sys = 31_000_010_u32;
    let first_tpl = static_wormhole_template(&ctx, w_sys, now, 1);
    let mut colliding = first_tpl[0].clone();
    colliding.wormhole_type_id = Some(999_999_999); // unknown in sde.wormhole_types
    colliding.type_name = "Custom Static WH".into();
    colliding.destination_system_id = Some(30_000_142); // >=0.45 sec -> target class 7
    let rep = build_static_replacement_wormhole(&ctx, w_sys, &colliding, now, 0).expect("replacement");
    assert_ne!(rep.signature_id, colliding.signature_id);
    assert_eq!(rep.type_name, "Custom Static WH");

    // Low-sec & null-sec & Pochven PCH replacement fallback
    colliding.destination_system_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id);
    assert!(build_static_replacement_wormhole(&ctx, w_sys, &colliding, now, 2).is_some());
    colliding.destination_system_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id);
    assert!(build_static_replacement_wormhole(&ctx, w_sys, &colliding, now, 3).is_some());

    let pch_collapsed = poch_tpl[0].clone();
    assert!(build_static_replacement_wormhole(&ctx, poch_sys, &pch_collapsed, now, 1).is_some());

    // K162 materialization duplicate check & sync_k162_mirror_in_snap None dest check
    let mut parent = first_tpl[0].clone();
    parent.destination_system_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        sync_k162_mirror_in_snap(snap, &parent);
        parent.destination_system_id = Some(30_000_142);
        let first = materialize_k162_exit_in_snap(&ctx, snap, &parent, 30_000_142);
        assert!(first.is_some());
        let dup = materialize_k162_exit_in_snap(&ctx, snap, &parent, 30_000_142);
        assert!(dup.is_none());
    });

    // seed_drifter_kspace_beacons_in_snap guard clauses (non-drifter sys, non-DRK, None dest, W-space dest, already exists)
    let mut sig_bad_prefix = make_wh_sig("OTH-1".into(), 31_000_001, "WH".into(), [0.0; 3], 34_494, 30_000_142, now, 86_400, 1e9, 3e8);
    let mut sig_no_dest = make_wh_sig("DRK-1".into(), 31_000_001, "WH".into(), [0.0; 3], 34_494, 30_000_142, now, 86_400, 1e9, 3e8);
    sig_no_dest.destination_system_id = None;
    let sig_w_dest = make_wh_sig("DRK-2".into(), 31_000_001, "WH".into(), [0.0; 3], 34_494, 31_000_010, now, 86_400, 1e9, 3e8);
    let mut sig_k_dest = make_wh_sig("DRK-3".into(), 31_000_001, "WH".into(), [0.0; 3], 34_494, 30_000_142, now, 86_400, 1e9, 3e8);
    sig_bad_prefix.wormhole_type_id = Some(34_494);
    sig_k_dest.wormhole_type_id = Some(34_494);
    ctx.db.with_mut_snapshot(|snap| {
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 30_000_142, &[sig_k_dest.clone()]);
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 31_000_001, &[sig_bad_prefix, sig_no_dest, sig_w_dest, sig_k_dest.clone()]);
        // Second call is idempotent (already exists continue branch)
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 31_000_001, &[sig_k_dest.clone()]);
    });

    // ensure_drifter_kspace_guards in W-space & non-drifter WH -> empty; valid -> spawns 2 guards & second call skips spawning
    assert!(ensure_drifter_kspace_guards(&ctx, 31_000_010, &sig_k_dest, 880_000_001, DVec3::ZERO).await.is_empty());
    let non_drifter = make_wh_sig("JMM-1".into(), 30_000_142, "Wormhole C1".into(), [0.0; 3], 30_671, 31_000_010, now, 86_400, 1e9, 3e8);
    assert!(ensure_drifter_kspace_guards(&ctx, 30_000_142, &non_drifter, 880_000_002, DVec3::ZERO).await.is_empty());

    let drifter_k_beacon = make_wh_sig("K162-DRK-3".into(), 30_000_142, "Unidentified Wormhole".into(), [0.0; 3], 34_494, 31_000_001, now, 86_400, 1e9, 3e8);
    let g1 = ensure_drifter_kspace_guards(&ctx, 30_000_142, &drifter_k_beacon, 880_000_010, DVec3::new(1000.0, 0.0, 0.0)).await;
    let g2 = ensure_drifter_kspace_guards(&ctx, 30_000_142, &drifter_k_beacon, 880_000_010, DVec3::new(1000.0, 0.0, 0.0)).await;
    assert_eq!(g1.len(), 2);
    assert_eq!(g1, g2);
}
