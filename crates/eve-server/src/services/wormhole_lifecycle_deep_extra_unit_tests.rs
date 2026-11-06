//! Deep branch-coverage and mutation-killing unit tests for `wormhole_lifecycle_ops.rs`,
//! `space_docking_cloaking.rs`, and `space_cyno_filaments.rs` (wormhole lifecycle, K162 mirrors,
//! Drifter beacons/guards, stage/bucket matrices, cosmic wormhole jumps, and jump filaments).

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::space_destiny::{
    clear_session_change_timer, compute_wormhole_stages_with_sde, derive_wormhole_buckets,
    handle as handle_space_destiny, record_session_change_timer, wormhole_item_id_for_signature,
};
use crate::services::wormhole_lifecycle_ops::*;
use eve_db::{CosmicSignatureRecord, FleetRecord, InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_sde::SolarSystem;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn system_profiles_thera_drifter_c13_templates_and_replacement_collision_branches() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

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
        sde_mut.solar_systems.insert(31_000_005, mk_sys(31_000_005, 11_000_031, -1.0, Some(12)));
        for c in 1..=6 {
            let sid = 31_000_100 + c as u32;
            sde_mut
                .solar_systems
                .insert(sid, mk_sys(sid, 11_000_000 + c as u32, -1.0, Some(c)));
        }
    }

    // 1. system_wormhole_profile_sde across Drifter (14..=18), Thera (12), C13 (13), shattered C1..=C6, and K-space (0)
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_001).0, 14);
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_002).0, 15);
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_003).0, 16);
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_004).0, 17);
    assert_eq!(system_wormhole_profile_sde(&ctx.sde, 31_000_006).0, 18);
    assert_eq!(
        system_wormhole_profile_sde(&ctx.sde, 31_000_005),
        (12, true, true, "None")
    );
    let c13_prof = system_wormhole_profile_sde(&ctx.sde, 31_002_580);
    assert_eq!((c13_prof.0, c13_prof.1, c13_prof.2), (13, false, true));
    let shat_prof = system_wormhole_profile_sde(&ctx.sde, 31_002_505);
    assert_eq!((shat_prof.0, shat_prof.1, shat_prof.2), (1, false, true));
    assert_eq!(
        system_wormhole_profile_sde(&ctx.sde, 30_000_142),
        (0, false, false, "None")
    );

    // 2. Thera (31_000_005) static template has 4 statics, all capped at <= 375,000,000 kg jump mass
    let thera_statics = statics_for_system_sde(&ctx.sde, 31_000_005);
    assert_eq!(thera_statics.len(), 4);
    let thera_tpl = static_wormhole_template(&ctx, 31_000_005, 1_700_000_000, 0);
    assert_eq!(thera_tpl.len(), 4);
    assert!(thera_tpl.iter().all(|w| w.max_jump_mass_kg <= 375_000_000.0));

    // 3. Drifter seq == 0 has 6 W-space statics + 56 DRK K-space wormholes = 62 total
    let drifter_statics = statics_for_system_sde(&ctx.sde, 31_000_001);
    assert_eq!(drifter_statics.len(), 6);
    let drifter_tpl0 = static_wormhole_template(&ctx, 31_000_001, 1_700_000_000, 0);
    assert_eq!(drifter_tpl0.len(), 62);
    assert_eq!(
        drifter_tpl0
            .iter()
            .filter(|w| w.signature_id.starts_with("DRK-"))
            .count(),
        56
    );

    // 4. C13 (31_002_580) static template has 5,000,000 kg max_jump_mass_kg
    let c13_statics = statics_for_system_sde(&ctx.sde, 31_002_580);
    assert_eq!(c13_statics.len(), 1);
    let c13_tpl = static_wormhole_template(&ctx, 31_002_580, 1_700_000_000, 0);
    assert_eq!(c13_tpl.len(), 1);
    assert!((c13_tpl[0].max_jump_mass_kg - 5_000_000.0).abs() < 1.0);

    // 5. build_static_replacement_wormhole: collision bump when signature_id matches collapsed_wh.signature_id
    let mut col_wh = thera_tpl[0].clone();
    let rep_first = build_static_replacement_wormhole(&ctx, 31_000_005, &col_wh, 1_700_001_000, 1).unwrap();
    col_wh.signature_id = rep_first.signature_id.clone();
    let rep_bumped = build_static_replacement_wormhole(&ctx, 31_000_005, &col_wh, 1_700_001_000, 1).unwrap();
    assert_ne!(rep_bumped.signature_id, col_wh.signature_id);
}

#[tokio::test]
async fn k162_materialization_mirror_sync_drifter_kspace_beacons_and_guards() {
    let (ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let w_sys = 31_000_001_u32;
    let k_sys = 30_000_142_u32;
    let now = 1_700_000_000_u64;

    let parent_sig = CosmicSignatureRecord {
        signature_id: "DRK-101".into(),
        solar_system_id: w_sys,
        sig_group: "Wormhole".into(),
        type_name: "Unidentified Wormhole".into(),
        position: [10_000.0, 0.0, 0.0],
        signal_strength: 0.0,
        wormhole_type_id: Some(34_494),
        destination_system_id: Some(k_sys),
        created_unix: now,
        lifetime_seconds: 57_600,
        max_mass_kg: 750_000_000.0,
        remaining_mass_kg: 750_000_000.0,
        max_jump_mass_kg: 300_000_000.0,
        collapsed: false,
    };

    let exit_id = wormhole_exit_item_id_for_signature(k_sys, "DRK-101");
    assert!((760_000_000..860_000_000).contains(&exit_id));

    ctx.db.with_mut_snapshot(|snap| {
        assert!(!is_k162_materialized_in_snap(snap, k_sys, "DRK-101"));
        let mat1 = materialize_k162_exit_in_snap(&ctx, snap, &parent_sig, k_sys);
        assert!(mat1.is_some());
        let (mat_exit_id, mat_rec, _) = mat1.unwrap();
        assert_eq!(mat_exit_id, exit_id);
        assert_eq!(mat_rec.signature_id, "K162-DRK-101");
        assert_eq!(mat_rec.wormhole_type_id, Some(26_272));
        assert_eq!(mat_rec.signal_strength, 0.0);
        assert!(is_k162_materialized_in_snap(snap, k_sys, "DRK-101"));

        // Second call is idempotent -> returns None
        assert!(materialize_k162_exit_in_snap(&ctx, snap, &parent_sig, k_sys).is_none());

        let mut updated_parent = parent_sig.clone();
        updated_parent.remaining_mass_kg = 123_456.0;
        updated_parent.collapsed = true;
        sync_k162_mirror_in_snap(snap, &updated_parent);

        let k_mirror = snap.cosmic_signatures[&k_sys]
            .iter()
            .find(|s| s.signature_id == "K162-DRK-101")
            .unwrap();
        assert!((k_mirror.remaining_mass_kg - 123_456.0).abs() < 1.0);
        assert!(k_mirror.collapsed);
    });

    let k_sys2 = 30_002_187_u32;
    let mut drk2 = parent_sig.clone();
    drk2.signature_id = "DRK-202".into();
    drk2.destination_system_id = Some(k_sys2);
    ctx.db.with_mut_snapshot(|snap| {
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, w_sys, std::slice::from_ref(&drk2));
        assert!(is_k162_materialized_in_snap(snap, k_sys2, "DRK-202"));
        let mut non_drk = drk2.clone();
        non_drk.signature_id = "DRK-303".into();
        seed_drifter_kspace_beacons_in_snap(&ctx, snap, 31_000_008, &[non_drk]);
        assert!(!is_k162_materialized_in_snap(snap, k_sys2, "DRK-303"));
    });

    // ensure_drifter_kspace_guards spawns 1x 34561 + 1x 34495 Drifter guards in K-space, and no-ops in W-space
    ensure_drifter_kspace_guards(&ctx, w_sys, &parent_sig, exit_id, DVec3::ZERO).await;
    let w_balls = ctx.systems.get_or_spawn(w_sys).list_balls().await.unwrap();
    assert!(!w_balls.iter().any(|b| b.type_id == 34_495));

    ensure_drifter_kspace_guards(&ctx, k_sys, &parent_sig, exit_id, DVec3::new(5000.0, 0.0, 0.0)).await;
    let k_balls = ctx.systems.get_or_spawn(k_sys).list_balls().await.unwrap();
    assert_eq!(k_balls.iter().filter(|b| b.type_id == 34_561).count(), 1);
    assert_eq!(k_balls.iter().filter(|b| b.type_id == 34_495).count(), 1);
}

#[test]
fn derive_wormhole_buckets_and_compute_wormhole_stages_full_matrix() {
    let (mut ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mk_sys = |id: u32, reg: u32, sec: f64, wh_cls: Option<i32>| SolarSystem {
            solar_system_id: id,
            constellation_id: 20_000_001,
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
        sde_mut.solar_systems.insert(30_002_537, mk_sys(30_002_537, 10_000_030, 0.3, Some(8)));
        sde_mut.solar_systems.insert(30_004_759, mk_sys(30_004_759, 10_000_060, -0.5, Some(9)));
        sde_mut.solar_systems.insert(30_000_157, mk_sys(30_000_157, 10_000_070, -1.0, Some(25)));
        sde_mut.solar_systems.insert(31_000_005, mk_sys(31_000_005, 11_000_031, -1.0, Some(12)));
        sde_mut.solar_systems.insert(31_000_001, mk_sys(31_000_001, 11_000_032, -1.0, Some(14)));
        sde_mut.solar_systems.insert(31_000_102, mk_sys(31_000_102, 11_000_002, -1.0, Some(2)));
        sde_mut.solar_systems.insert(31_000_104, mk_sys(31_000_104, 11_000_004, -1.0, Some(4)));
        sde_mut.solar_systems.insert(31_000_106, mk_sys(31_000_106, 11_000_006, -1.0, Some(6)));
    }

    let now = 1_700_000_000_u64;
    let mut sig = CosmicSignatureRecord {
        signature_id: "WST-101".into(),
        solar_system_id: 30_000_142,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: [0.0, 0.0, 0.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(30_579),
        destination_system_id: Some(30_000_142),
        created_unix: now,
        lifetime_seconds: 172_800,
        max_mass_kg: 1_000_000_000.0,
        remaining_mass_kg: 800_000_000.0,
        max_jump_mass_kg: 5_000_000.0,
        collapsed: false,
    };

    for (dest_sys, expected_class) in [
        (31_000_005, "Thera"),
        (31_000_001, "Drifter"),
        (30_000_157, "Triglavian"),
        (31_000_102, "C1-3"),
        (31_000_104, "C4-5"),
        (31_000_106, "C6"),
        (30_000_142, "High-Sec"),
        (30_002_537, "Low-Sec"),
        (30_004_759, "Null-Sec"),
    ] {
        sig.destination_system_id = Some(dest_sys);
        let b = derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.destination_class, expected_class);
    }

    for (jump_mass, expected_size) in [
        (5_000_000.0, "Small"),
        (62_000_000.0, "Medium"),
        (300_000_000.0, "Large"),
        (1_350_000_000.0, "Extra Large"),
    ] {
        sig.max_jump_mass_kg = jump_mass;
        let b = derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.max_ship_size, expected_size);
    }

    let (m_st, wh_sz0, l_st, wh_age, col) = compute_wormhole_stages_with_sde(&sig, now, Some(&ctx.sde));
    let b0 = derive_wormhole_buckets(&sig, now, &ctx.sde);
    assert_eq!((m_st, wh_sz0, l_st, wh_age, col), ("Stable", 1.0, "Healthy", 0, false));
    assert_eq!((b0.lifetime_bucket, b0.mass_stability_bucket), (">1 day", ">50%"));

    sig.lifetime_seconds = 50_000;
    sig.remaining_mass_kg = 400_000_000.0;
    let (m_st1, wh_sz1, l_st1, wh_age1, _) = compute_wormhole_stages_with_sde(&sig, now, Some(&ctx.sde));
    let b1 = derive_wormhole_buckets(&sig, now, &ctx.sde);
    assert_eq!((m_st1, wh_sz1, l_st1, wh_age1), ("Destabilized", 0.5, "BeginningToDecay", 1));
    assert_eq!((b1.lifetime_bucket, b1.mass_stability_bucket), ("<1 day", "<50%"));

    sig.lifetime_seconds = 10_000;
    sig.remaining_mass_kg = 50_000_000.0;
    let (m_st2, _, l_st2, wh_age2, _) = compute_wormhole_stages_with_sde(&sig, now, Some(&ctx.sde));
    let b2 = derive_wormhole_buckets(&sig, now, &ctx.sde);
    assert_eq!((m_st2, l_st2, wh_age2), ("Critical", "ReachingEndOfLifetime", 2));
    assert_eq!((b2.lifetime_bucket, b2.mass_stability_bucket), ("<4h", "<10%"));

    sig.lifetime_seconds = 1_800;
    let b3 = derive_wormhole_buckets(&sig, now, &ctx.sde);
    assert_eq!(b3.lifetime_bucket, "<1h");

    sig.remaining_mass_kg = 0.0;
    let (m_st4, _, l_st4, wh_age4, col4) =
        compute_wormhole_stages_with_sde(&sig, now + 2_000, Some(&ctx.sde));
    assert_eq!(
        (m_st4, l_st4, wh_age4, col4),
        ("Collapsed_Exhausted", "Collapsed_Expired", 3, true)
    );
}

#[tokio::test]
async fn cosmic_wormhole_jump_polarization_mass_collapse_replacement_and_filaments() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let dest_sys = 31_000_005_u32;
    let now = ctx.now_unix();
    let empty_kw = HashMap::new();

    Arc::make_mut(&mut ctx.sde).solar_systems.insert(
        dest_sys,
        SolarSystem {
            solar_system_id: dest_sys,
            constellation_id: 20_000_005,
            region_id: 11_000_031,
            name: "Thera".into(),
            security_status: -1.0,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: Some(12),
        },
    );

    session.station_id = None;
    session.structure_id = None;
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let sig_id = "WST-500";
    let wh_item_id = wormhole_item_id_for_signature(sys_id, sig_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.solar_system_id = sys_id;
        }
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: sig_id.into(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: "Wormhole".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(30_579),
                destination_system_id: Some(dest_sys),
                created_unix: now,
                lifetime_seconds: 86_400,
                max_mass_kg: 10_000_000.0,
                remaining_mass_kg: 1_500_000.0,
                max_jump_mass_kg: 20_000_000.0,
                collapsed: false,
            }],
        );
    });

    let mk_ball = |pos: DVec3| Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(mk_ball(DVec3::new(10_000.0, 0.0, 0.0))).await;
    assert_eq!(
        handle_space_destiny(&ctx, &mut session, None, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_item_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "OutsideWormholeJumpRange"
    );

    let _ = sys_handle.spawn_ball(mk_ball(DVec3::new(1_200.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.insert((char_id, wh_item_id), now + 180);
    });
    assert_eq!(
        handle_space_destiny(&ctx, &mut session, None, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_item_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "WormholePolarized"
    );

    ctx.db.with_mut_snapshot(|snap| { snap.wh_polarizations.clear(); });
    let jump_ok = handle_space_destiny(
        &ctx, &mut session, None, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_item_id)], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(jump_ok.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    assert_eq!(jump_ok.dict_get("collapsed"), Some(&PyRep::Bool(true)));
    assert_eq!(session.solar_system_id, Some(dest_sys));
    ctx.db.with_snapshot(|snap| {
        let sigs = &snap.cosmic_signatures[&sys_id];
        assert!(sigs.iter().any(|s| s.signature_id == sig_id && s.collapsed));
        assert!(sigs.len() >= 2);
    });
    clear_session_change_timer(&ctx, char_id);

    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let _ = dest_handle.spawn_ball(mk_ball(DVec3::new(50_000_000.0, 0.0, 0.0))).await;

    ctx.db.with_mut_snapshot(|snap| { snap.fleets.clear(); });
    assert_eq!(
        handle_space_destiny(&ctx, &mut session, None, "randomJumpMgr", "ActivateRandomJumpFilament", &[], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "NotInFleet"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(1, FleetRecord { fleet_id: 1, boss_id: char_id, members: vec![char_id], ..Default::default() });
        snap.structures.insert(
            999_111,
            UpwellStructureRecord {
                structure_id: 999_111,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: dest_sys,
                name: "Nearby Struct".into(),
                position: [50_100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_space_destiny(&ctx, &mut session, None, "randomJumpMgr", "ActivateRandomJumpFilament", &[], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "TooCloseToStructureOrStation"
    );

    let (poch_exit_item_id, needlejack_type_id, needlejack_item_id) = (910_001_i64, 54_830_u32, 910_002_i64);
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.solar_systems.insert(
            30_004_759,
            SolarSystem {
                solar_system_id: 30_004_759,
                constellation_id: 20_000_696,
                region_id: 10_000_060,
                name: "1DQ1-A".into(),
                security_status: -0.9,
                radius: 1e12,
                galactic_pos_ly: [0.0, 0.0, 0.0],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: Some(9),
            },
        );
        sde_mut.types.insert(56_065, make_fighter_type(56_065, 4087, 17, 0.1, vec![(2832, 1.0)]));
        sde_mut.types.insert(
            needlejack_type_id,
            make_fighter_type(needlejack_type_id, 4041, 17, 0.1, vec![(2832, 5.0), (3025, 1.0)]),
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.clear();
        snap.items.insert(poch_exit_item_id, make_fighter_item(poch_exit_item_id, 56_065, char_id, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(needlejack_item_id, make_fighter_item(needlejack_item_id, needlejack_type_id, char_id, ship_id, InventoryFlag::Cargo, 1));
    });
    assert_eq!(
        handle_space_destiny(&ctx, &mut session, None, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(poch_exit_item_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "PochvenExitFilamentRequiresPochvenOrigin"
    );

    let fil_ok = handle_space_destiny(
        &ctx, &mut session, None, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(needlejack_item_id)], &empty_kw,
    ).await.unwrap().unwrap();
    assert!(fil_ok.dict_get("solarSystemID").is_some());
    assert!(ctx.db.get_item(needlejack_item_id).is_none());
    let _ = record_session_change_timer;
}
