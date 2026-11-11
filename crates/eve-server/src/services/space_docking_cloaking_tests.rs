//! Unit tests for `space_docking_cloaking` (wormhole bucket derivation, lifetime/mass stages, signature seeding, expiry sweeping, and cosmic wormhole jumping).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::types::CosmicSignatureRecord;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, WormholeTypeInfo};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_docking_cloaking_extra_unit_tests.rs"]
mod extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    mass: f64,
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.mass = mass;
    rec.default_attributes.clear();
    reg.types.insert(new_tid, rec);
}

fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(730);
    s.user_id = Some(40);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

fn make_ship_ball(ship_id: i64, type_id: u32, mass: f64, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn sample_wh_sig(
    sig_id: &str,
    wh_type_id: u32,
    created_unix: u64,
    lifetime_seconds: u64,
    max_mass_kg: f64,
    remaining_mass_kg: f64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id: sig_id.to_string(),
        solar_system_id: 30000142,
        sig_group: "Wormhole".to_string(),
        type_name: "Wormhole B274".to_string(),
        signal_strength: 100.0,
        position: [10_000.0, 20_000.0, 30_000.0],
        wormhole_type_id: Some(wh_type_id),
        destination_system_id: Some(31000005),
        created_unix,
        lifetime_seconds,
        max_mass_kg,
        remaining_mass_kg,
        max_jump_mass_kg: 5_000_000.0,
        collapsed: false,
    }
}

#[test]
fn derive_wormhole_buckets_covers_destinations_ship_sizes_lifetime_and_mass() {
    let mut ctx = test_ctx();
    let custom_wh_tid = 99_701;
    let reg = Arc::make_mut(&mut ctx.sde);
    reg.wormhole_types.insert(
        custom_wh_tid,
        WormholeTypeInfo {
            type_id: custom_wh_tid,
            name: "Wormhole Test".into(),
            target_system_class: 12, // Thera
            max_stable_time_min: 1440.0,
            max_stable_mass: 2_000_000_000.0,
            mass_regeneration: 0.0,
            max_jump_mass: 5_000_000.0, // Frigate / Destroyer small ship
        },
    );

    let now_u = 1_700_000_000;
    let sig = sample_wh_sig("WHB-001", custom_wh_tid, now_u - 1_000, 86_400, 2_000_000_000.0, 1_500_000_000.0);
    let b = derive_wormhole_buckets(&sig, now_u, &ctx.sde);
    assert_eq!(b.destination_class, "Thera");
    assert_eq!(b.max_ship_size, "Small");
    assert_eq!(b.lifetime_bucket, "<1 day");
    assert_eq!(b.mass_stability_bucket, ">50%");

    // Destabilized mass (< 50%) & < 4h lifetime
    let low_sig = sample_wh_sig("WHB-002", custom_wh_tid, now_u - 80_000, 86_400, 2_000_000_000.0, 600_000_000.0);
    let b_low = derive_wormhole_buckets(&low_sig, now_u, &ctx.sde);
    assert_eq!(b_low.lifetime_bucket, "<4h");
    assert_eq!(b_low.mass_stability_bucket, "<50%");

    // Critical mass (< 10%) & < 1h lifetime
    let crit_sig = sample_wh_sig("WHB-003", custom_wh_tid, now_u - 85_000, 86_400, 2_000_000_000.0, 100_000_000.0);
    let b_crit = derive_wormhole_buckets(&crit_sig, now_u, &ctx.sde);
    assert_eq!(b_crit.lifetime_bucket, "<1h");
    assert_eq!(b_crit.mass_stability_bucket, "<10%");

    // Direct raw bucket checks for XL capital, medium, large, and various system classes
    let xl = derive_wormhole_buckets_raw(30000142, 2_000_000_000.0, 2_000_000_000.0, 2_000_000_000.0, now_u - 10, 100_000, false, now_u, &ctx.sde);
    assert_eq!(xl.destination_class, "High-Sec");
    assert_eq!(xl.max_ship_size, "Extra Large");

    let med = derive_wormhole_buckets_raw(30000142, 50_000_000.0, 2_000_000_000.0, 2_000_000_000.0, now_u - 10, 86_400, false, now_u, &ctx.sde);
    assert_eq!(med.max_ship_size, "Medium");

    let large = derive_wormhole_buckets_raw(30000142, 300_000_000.0, 2_000_000_000.0, 2_000_000_000.0, now_u - 10, 86_400, true, now_u, &ctx.sde);
    assert_eq!(large.max_ship_size, "Large");
    assert_eq!(large.lifetime_bucket, "expired");
}

#[test]
fn compute_wormhole_stages_reports_all_mass_and_lifetime_states() {
    let now_u = 1_700_000_000;
    let healthy = sample_wh_sig("WHS-1", 30001, now_u - 3_600, 100_000, 1_000_000_000.0, 800_000_000.0);
    assert_eq!(compute_wormhole_stages(&healthy, now_u), ("Stable", 1.0, "Healthy", 0, false));

    let decaying = sample_wh_sig("WHS-2", 30001, now_u - 40_000, 100_000, 1_000_000_000.0, 400_000_000.0);
    assert_eq!(
        compute_wormhole_stages(&decaying, now_u),
        ("Destabilized", 0.5, "BeginningToDecay", 1, false)
    );

    let crit = sample_wh_sig("WHS-3", 30001, now_u - 90_000, 100_000, 1_000_000_000.0, 50_000_000.0);
    assert_eq!(
        compute_wormhole_stages(&crit, now_u),
        ("Critical", 0.1, "ReachingEndOfLifetime", 2, false)
    );

    let expired = sample_wh_sig("WHS-4", 30001, now_u - 120_000, 100_000, 1_000_000_000.0, 0.0);
    assert_eq!(
        compute_wormhole_stages(&expired, now_u),
        ("Collapsed_Exhausted", 0.0, "Collapsed_Expired", 3, true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_system_signatures_seeds_and_expires_wormholes_and_sites() {
    let ctx = test_ctx();
    let wh_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| (31_000_000..32_000_000).contains(&s.solar_system_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(31000001);

    let (sigs, _anoms) = ensure_system_signatures_and_anomalies(&ctx, wh_sys).await;
    assert!(!sigs.is_empty());
    let has_wh = sigs.iter().any(|s| s.sig_group == "Wormhole");
    assert!(has_wh);

    // Insert an expired wormhole signature and sweep it
    let now_u = ctx.now_unix();
    let expired_id = "EXP-999".to_string();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(wh_sys).or_default().push(CosmicSignatureRecord {
            signature_id: expired_id.clone(),
            solar_system_id: wh_sys,
            sig_group: "Wormhole".to_string(),
            type_name: "Wormhole B274".to_string(),
            signal_strength: 100.0,
            position: [5000.0, 0.0, 0.0],
            wormhole_type_id: Some(sigs.iter().find(|s| s.sig_group == "Wormhole").and_then(|s| s.wormhole_type_id).unwrap_or(30001)),
            destination_system_id: Some(30000142),
            created_unix: now_u.saturating_sub(200_000),
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 1_000_000_000.0,
            max_jump_mass_kg: 20_000_000.0,
            collapsed: false,
        });
    });

    collapse_expired_wormholes_in_system(&ctx, wh_sys, now_u).await;
    let is_expired_collapsed = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&wh_sys)
            .and_then(|v| v.iter().find(|s| s.signature_id == expired_id))
            .map(|s| s.collapsed)
            .unwrap_or(true)
    });
    assert!(is_expired_collapsed);

    expire_and_respawn_wormholes_in_cluster(&ctx, now_u).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_cosmic_wormhole_jump_roundtrips_k162_and_handles_collapse_and_polarization() {
    let mut ctx = test_ctx();
    let wh_type_id = 99_710;
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.wormhole_types.insert(
            wh_type_id,
            WormholeTypeInfo {
                type_id: wh_type_id,
                name: "Wormhole Custom".into(),
                target_system_class: 7,
                max_stable_time_min: 1440.0,
                max_stable_mass: 10_000_000.0,
                mass_regeneration: 0.0,
                max_jump_mass: 20_000_000.0,
            },
        );
    }

    let ship_tid = 99_711;
    clone_type_with(&mut ctx.sde, 588, ship_tid, 25, 6, 2500.0, 150.0, 2_000_000.0);

    let host_sys = 30000142;
    let dest_sys = 30000144;
    let char_id = 96_101;
    let ship_id = 78_101;
    let sig_id = "WHJ-100".to_string();
    let wh_pos = DVec3::new(10_000.0, 0.0, 0.0);
    let now_u = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = host_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: host_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.cosmic_signatures.insert(
            host_sys,
            vec![CosmicSignatureRecord {
                signature_id: sig_id.clone(),
                solar_system_id: host_sys,
                sig_group: "Wormhole".to_string(),
                type_name: "Wormhole Custom".to_string(),
                signal_strength: 100.0,
                position: [wh_pos.x, wh_pos.y, wh_pos.z],
                wormhole_type_id: Some(wh_type_id),
                destination_system_id: Some(dest_sys),
                created_unix: now_u.saturating_sub(60),
                lifetime_seconds: 86_400,
                max_mass_kg: 10_000_000.0,
                remaining_mass_kg: 5_500_000.0,
                max_jump_mass_kg: 20_000_000.0,
                collapsed: false,
            }],
        );
    });

    let host_h = ctx.systems.get_or_spawn(host_sys);
    let _ = host_h
        .spawn_ball(make_ship_ball(ship_id, ship_tid, 2_000_000.0, DVec3::new(20_000.0, 0.0, 0.0)))
        .await;

    let mut sess = make_session(char_id, 98000001, Some(host_sys), Some(ship_id));

    // Out-of-range check fails (> 2500m from wormhole ball)
    let far_ball = make_ship_ball(ship_id, ship_tid, 2_000_000.0, DVec3::new(20_000.0, 0.0, 0.0));
    let far_res = execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        host_sys,
        ship_id,
        far_ball,
        None,
        Some(&sig_id),
    )
    .await
    .expect("matched")
    .expect_err("too far");
    assert_eq!(far_res.code, "OutsideWormholeJumpRange");

    // Clear session-change timer before valid jump
    crate::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    // Valid forward jump within 2500m reduces mass from 5.5M to 3.5M
    let near_ball = make_ship_ball(ship_id, ship_tid, 2_000_000.0, DVec3::new(11_000.0, 0.0, 0.0));
    let jump_ok = execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        host_sys,
        ship_id,
        near_ball.clone(),
        None,
        Some(&sig_id),
    )
    .await
    .expect("matched")
    .expect("forward wh jump ok");
    assert!(matches!(jump_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    let rem_mass = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&host_sys)
            .and_then(|v| v.iter().find(|s| s.signature_id == sig_id))
            .map(|s| s.remaining_mass_kg)
            .unwrap_or(-1.0)
    });
    assert!((rem_mass - 3_500_000.0).abs() < 1e-3);

    // Reverse K162 jump reduces mass from 3.5M to 1.5M and returns to host_sys
    crate::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let dest_h = ctx.systems.get_or_spawn(dest_sys);
    let (_, dest_ball) = dest_h
        .get_ball_snapshot(ship_id)
        .await
        .expect("snap")
        .expect("ship ball in dest");
    let rev_ok = execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        dest_sys,
        ship_id,
        dest_ball,
        None,
        Some(&format!("K162-{sig_id}")),
    )
    .await
    .expect("matched")
    .expect("reverse K162 jump ok");
    assert!(matches!(rev_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.solar_system_id, Some(host_sys));

    // Immediate second forward jump hits polarization timer
    crate::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let pol_err = execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        host_sys,
        ship_id,
        near_ball.clone(),
        None,
        Some(&sig_id),
    )
    .await
    .expect("matched")
    .expect_err("polarized");
    assert_eq!(pol_err.code, "WormholePolarized");

    // Clear polarization and jump forward again, collapsing the wormhole (1.5M - 2.0M <= 0)
    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.clear();
    });
    let collapse_ok = execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        host_sys,
        ship_id,
        near_ball,
        None,
        Some(&sig_id),
    )
    .await
    .expect("matched")
    .expect("forward collapse jump ok");
    assert!(matches!(collapse_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    let is_collapsed_after = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&host_sys)
            .and_then(|v| v.iter().find(|s| s.signature_id == sig_id))
            .map(|s| s.collapsed)
            .unwrap_or(true)
    });
    assert!(is_collapsed_after);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn criminal_cosmic_wormhole_jump_into_highsec_schedules_concord() {
    let mut ctx = test_ctx();
    let wh_type_id = 99_801_u32;
    let ship_tid = 99_811_u32;
    clone_type_with(&mut ctx.sde, 31_790, wh_type_id, 988, 2, 10.0, 0.0, 20_000_000.0);
    clone_type_with(&mut ctx.sde, 588, ship_tid, 25, 6, 2500.0, 150.0, 2_000_000.0);

    let host_sys = 31_000_005_u32; // Wormhole space
    let dest_sys = 30_000_142_u32; // Jita (1.0 high-sec)
    let char_id = 96_201_i64;
    let ship_id = 78_201_i64;
    let sig_id = "WHC-200".to_string();
    let wh_pos = DVec3::new(10_000.0, 0.0, 0.0);
    let now_u = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.solar_system_id = host_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: host_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.cosmic_signatures.insert(
            host_sys,
            vec![CosmicSignatureRecord {
                signature_id: sig_id.clone(),
                solar_system_id: host_sys,
                sig_group: "Wormhole".to_string(),
                type_name: "Wormhole Custom".to_string(),
                signal_strength: 100.0,
                position: [wh_pos.x, wh_pos.y, wh_pos.z],
                wormhole_type_id: Some(wh_type_id),
                destination_system_id: Some(dest_sys),
                created_unix: now_u.saturating_sub(60),
                lifetime_seconds: 86_400,
                max_mass_kg: 100_000_000.0,
                remaining_mass_kg: 100_000_000.0,
                max_jump_mass_kg: 20_000_000.0,
                collapsed: false,
            }],
        );
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now_u + 900;
    }

    let near_ball = make_ship_ball(ship_id, ship_tid, 2_000_000.0, DVec3::new(11_000.0, 0.0, 0.0));
    let _ = ctx.systems.get_or_spawn(host_sys).spawn_ball(near_ball.clone()).await;
    let mut sess = make_session(char_id, 98000001, Some(host_sys), Some(ship_id));
    execute_cosmic_wormhole_jump(
        &ctx,
        &mut sess,
        char_id,
        host_sys,
        ship_id,
        near_ball,
        None,
        Some(&sig_id),
    )
    .await
    .expect("matched")
    .expect("wormhole jump into highsec ok");

    let pending = ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival;
    assert_eq!(pending.map(|p| (p.0, p.2)), Some((dest_sys, ship_id)));
}
