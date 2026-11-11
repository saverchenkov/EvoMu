//! Extra unit and mutation-killing tests for `services::space_destiny::space_docking_cloaking`
//! covering wormhole class/ship-size/lifetime/mass bucket boundaries, SDE fallback resolution,
//! signature reseeding, Drifter & Pochven wormhole seeding, K162 lifetime materialization,
//! and cosmic wormhole jump validation, mass limits, polarization, and environmental effects.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_destiny::space_docking_cloaking::*;
use crate::services::space_destiny::{
    clear_session_change_timer, record_session_change_timer, wormhole_item_id_for_signature,
};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{ConstellationInfo, InvType, RegionInfo, SdeRegistry, WormholeTypeInfo};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

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
    mass: f64,
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .unwrap_or_else(|| reg.types.values().next().cloned().expect("base sde type"));
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("WhTestType_{new_tid}");
    rec.mass = mass;
    rec.default_attributes.clear();
    reg.types.insert(new_tid, rec);
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(731);
    s.user_id = Some(41);
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
        max_velocity: 300.0,
        inertia: 2.0,
        mass,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_wh_sig(
    sig_id: &str,
    sys_id: u32,
    pos: [f64; 3],
    wh_type_id: Option<u32>,
    dest_sys: Option<u32>,
    created_unix: u64,
    lifetime_seconds: u64,
    max_mass_kg: f64,
    remaining_mass_kg: f64,
    max_jump_mass_kg: f64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id: sig_id.into(),
        solar_system_id: sys_id,
        sig_group: "Wormhole".into(),
        type_name: format!("Wormhole {sig_id}"),
        signal_strength: 100.0,
        position: pos,
        wormhole_type_id: wh_type_id,
        destination_system_id: dest_sys,
        created_unix,
        lifetime_seconds,
        max_mass_kg,
        remaining_mass_kg,
        max_jump_mass_kg,
        collapsed: false,
    }
}

#[test]
fn derive_wormhole_buckets_and_stages_kill_boundary_and_sde_fallback_mutants() {
    let mut ctx = test_ctx();
    let reg = Arc::make_mut(&mut ctx.sde);
    let base_sys = reg.solar_systems.values().next().unwrap().clone();
    let base_const: ConstellationInfo = reg.constellations.values().next().unwrap().clone();
    let base_reg: RegionInfo = reg.regions.values().next().unwrap().clone();

    let reg_c4 = 99_900_004;
    let const_c6 = 99_800_006;
    let const_none = 99_800_000;
    reg.regions.insert(
        reg_c4,
        RegionInfo {
            region_id: reg_c4,
            name: "RegC4".into(),
            wormhole_class_id: Some(4),
            ..base_reg
        },
    );
    reg.constellations.insert(
        const_c6,
        ConstellationInfo {
            constellation_id: const_c6,
            region_id: reg_c4,
            name: "ConstC6".into(),
            wormhole_class_id: Some(6),
            ..base_const.clone()
        },
    );
    reg.constellations.insert(
        const_none,
        ConstellationInfo {
            constellation_id: const_none,
            region_id: reg_c4,
            name: "ConstNone".into(),
            wormhole_class_id: None,
            ..base_const
        },
    );

    let mut sys_c6 = base_sys.clone();
    sys_c6.solar_system_id = 39_000_001;
    sys_c6.constellation_id = const_c6;
    sys_c6.region_id = reg_c4;
    sys_c6.wormhole_class_id = None;
    reg.solar_systems.insert(39_000_001, sys_c6);

    let mut sys_c4 = base_sys.clone();
    sys_c4.solar_system_id = 39_000_002;
    sys_c4.constellation_id = const_none;
    sys_c4.region_id = reg_c4;
    sys_c4.wormhole_class_id = None;
    reg.solar_systems.insert(39_000_002, sys_c4);

    for (sys_id, wh_cls, sec) in [
        (39_000_010, Some(1), -1.0),
        (39_000_011, Some(3), -1.0),
        (39_000_012, Some(13), -1.0),
        (39_000_013, Some(5), -1.0),
        (39_000_014, Some(7), 0.8),
        (39_000_015, Some(8), 0.3),
        (39_000_016, Some(9), -0.5),
        (39_000_017, Some(11), -0.5),
        (39_000_018, Some(12), -1.0),
        (39_000_019, Some(14), -1.0),
        (39_000_020, Some(18), -1.0),
        (39_000_021, Some(25), -1.0),
        (39_000_022, Some(99), 0.45),
        (39_000_023, Some(99), 0.44),
        (39_000_024, Some(99), 0.0),
    ] {
        let mut s = base_sys.clone();
        s.solar_system_id = sys_id;
        s.wormhole_class_id = wh_cls;
        s.security_status = sec;
        reg.solar_systems.insert(sys_id, s);
    }

    let fallback_wh_tid = 99_750;
    reg.wormhole_types.insert(
        fallback_wh_tid,
        WormholeTypeInfo {
            type_id: fallback_wh_tid,
            name: "Wormhole Fallback".into(),
            target_system_class: 25,
            max_stable_time_min: 1500.0,
            max_stable_mass: 1_000_000_000.0,
            mass_regeneration: 0.0,
            max_jump_mass: 62_000_000.0,
        },
    );

    let now = 1_800_000_000;
    for (sys_id, expected_class) in [
        (39_000_001, "C6"),
        (39_000_002, "C4-5"),
        (39_000_010, "C1-3"),
        (39_000_011, "C1-3"),
        (39_000_012, "C1-3"),
        (39_000_013, "C4-5"),
        (39_000_014, "High-Sec"),
        (39_000_015, "Low-Sec"),
        (39_000_016, "Null-Sec"),
        (39_000_017, "Null-Sec"),
        (39_000_018, "Thera"),
        (39_000_019, "Drifter"),
        (39_000_020, "Drifter"),
        (39_000_021, "Triglavian"),
        (39_000_022, "High-Sec"),
        (39_000_023, "Low-Sec"),
        (39_000_024, "Null-Sec"),
    ] {
        let b = derive_wormhole_buckets_raw(
            sys_id,
            100_000_000.0,
            500_000_000.0,
            1_000_000_000.0,
            now - 100,
            86_400,
            false,
            now,
            &ctx.sde,
        );
        assert_eq!(b.destination_class, expected_class, "sys_id={sys_id}");
    }

    let b_fb = derive_wormhole_buckets_for_type(
        Some(fallback_wh_tid),
        0,
        0.0,
        500_000_000.0,
        0.0,
        now - 1_000,
        0,
        false,
        now,
        &ctx.sde,
    );
    assert_eq!(b_fb.destination_class, "Triglavian");
    assert_eq!(b_fb.max_ship_size, "Medium");
    assert_eq!(b_fb.lifetime_bucket, ">1 day");
    assert!(b_fb.lifetime_description.contains("not yet begun"));
    assert_eq!(b_fb.mass_stability_bucket, ">50%");

    for (jump_mass, expected_size) in [
        (0.0, "Large"),
        (20_000_000.0, "Small"),
        (20_000_001.0, "Medium"),
        (62_000_000.0, "Medium"),
        (62_000_001.0, "Large"),
        (500_000_000.0, "Large"),
        (500_000_001.0, "Extra Large"),
    ] {
        let b = derive_wormhole_buckets_raw(
            39_000_014,
            jump_mass,
            500.0,
            1000.0,
            now,
            10_000,
            false,
            now,
            &ctx.sde,
        );
        assert_eq!(b.max_ship_size, expected_size, "jump_mass={jump_mass}");
    }

    for (rem_secs, exp_bucket, exp_stage, exp_age) in [
        (0, "expired", "Collapsed_Expired", 3),
        (3_599, "<1h", "ReachingEndOfLifetime", 2),
        (3_600, "<4h", "ReachingEndOfLifetime", 2),
        (4 * 3_600 - 1, "<4h", "ReachingEndOfLifetime", 2),
        (4 * 3_600, "<1 day", "BeginningToDecay", 1),
        (24 * 3_600, "<1 day", "BeginningToDecay", 1),
        (24 * 3_600 + 1, ">1 day", "Healthy", 0),
    ] {
        let b = derive_wormhole_buckets_raw(
            39_000_014,
            20_000_000.0,
            500.0,
            1000.0,
            now,
            rem_secs,
            false,
            now,
            &ctx.sde,
        );
        assert_eq!(b.lifetime_bucket, exp_bucket, "rem_secs={rem_secs}");

        let sig = make_wh_sig(
            "BND-1",
            39_000_014,
            [0.0; 3],
            Some(fallback_wh_tid),
            Some(39_000_014),
            now,
            rem_secs,
            1000.0,
            500.0,
            20_000_000.0,
        );
        if rem_secs > 0 {
            let (_, _, stage, age, collapsed) =
                compute_wormhole_stages_with_sde(&sig, now, Some(&ctx.sde));
            assert_eq!((stage, age, collapsed), (exp_stage, exp_age, false));
        }
    }

    for (rem_mass, exp_m_bucket, exp_m_stage, exp_m_size) in [
        (0.0, "<10%", "Collapsed_Exhausted", 0.0),
        (99.9, "<10%", "Critical", 0.1),
        (100.0, "<50%", "Destabilized", 0.5),
        (499.9, "<50%", "Destabilized", 0.5),
        (500.0, ">50%", "Stable", 1.0),
    ] {
        let b = derive_wormhole_buckets_raw(
            39_000_014,
            20_000_000.0,
            rem_mass,
            1000.0,
            now,
            10_000,
            false,
            now,
            &ctx.sde,
        );
        assert_eq!(b.mass_stability_bucket, exp_m_bucket, "rem_mass={rem_mass}");

        let sig = make_wh_sig(
            "MBND-1",
            39_000_014,
            [0.0; 3],
            None,
            None,
            now,
            10_000,
            1000.0,
            rem_mass,
            20_000_000.0,
        );
        let (m_stage, m_size, _, _, _) = compute_wormhole_stages(&sig, now);
        assert_eq!((m_stage, m_size), (exp_m_stage, exp_m_size));
    }
}

#[tokio::test]
async fn ensure_signatures_reseeds_k162_only_expires_anomalies_and_sweeps_k162_materialization() {
    let ctx = test_ctx();
    let kspace_sys = 30_000_142_u32;
    let dest_sys = 30_000_144_u32;
    let drifter_sys = 31_000_001_u32;
    let now_u = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            kspace_sys,
            vec![make_wh_sig(
                "K162-OLD",
                kspace_sys,
                [1_000.0, 0.0, 0.0],
                Some(26_272),
                Some(dest_sys),
                now_u.saturating_sub(100),
                86_400,
                1_000_000_000.0,
                1_000_000_000.0,
                300_000_000.0,
            )],
        );
        snap.cosmic_anomalies.insert(
            kspace_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ANO-EXP".into(),
                solar_system_id: kspace_sys,
                name: "Expired Deposit".into(),
                anomaly_type: "Ore".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: now_u.saturating_sub(100_000),
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
    });

    let (sigs, anoms) = ensure_system_signatures_and_anomalies(&ctx, kspace_sys).await;
    assert!(sigs.len() > 1, "should reseed when only K162-* existed");
    assert!(anoms.iter().any(|a| a.anomaly_id == "ANO-EXP" && a.cleared));

    let (drifter_sigs, drifter_anoms) =
        ensure_system_signatures_and_anomalies(&ctx, drifter_sys).await;
    assert!(!drifter_sigs.is_empty() && drifter_anoms.is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(kspace_sys).or_default().extend([
            make_wh_sig(
                "MAT-15H",
                kspace_sys,
                [5_000.0, 0.0, 0.0],
                Some(30_001),
                Some(dest_sys),
                now_u.saturating_sub(40_000),
                86_400,
                2_000_000_000.0,
                1_500_000_000.0,
                300_000_000.0,
            ),
            make_wh_sig(
                "K162-EXPIRED",
                kspace_sys,
                [8_000.0, 0.0, 0.0],
                Some(26_272),
                Some(dest_sys),
                now_u.saturating_sub(90_000),
                86_400,
                2_000_000_000.0,
                1_500_000_000.0,
                300_000_000.0,
            ),
        ]);
    });

    collapse_expired_wormholes_in_system(&ctx, kspace_sys, now_u).await;

    let (k162_expired_collapsed, k162_mat_present) = ctx.db.with_snapshot(|snap| {
        let exp_col = snap
            .cosmic_signatures
            .get(&kspace_sys)
            .and_then(|l| l.iter().find(|s| s.signature_id == "K162-EXPIRED"))
            .is_some_and(|s| s.collapsed);
        let mat_ok = snap
            .cosmic_signatures
            .get(&dest_sys)
            .is_some_and(|l| l.iter().any(|s| s.signature_id == "K162-MAT-15H" && !s.collapsed));
        (exp_col, mat_ok)
    });
    assert!(k162_expired_collapsed && k162_mat_present);

    let _ = ensure_system_signatures_and_anomalies(&ctx, kspace_sys).await;
}

#[tokio::test]
async fn execute_cosmic_wormhole_jump_covers_timers_mass_limits_collapsed_and_env_effects() {
    let mut ctx = test_ctx();
    let (cruiser_tid, capital_tid, frig_tid) = (99_760, 99_761, 99_762);
    clone_type_with(&mut ctx.sde, 624, cruiser_tid, 26, 6, 12_000_000.0);
    clone_type_with(&mut ctx.sde, 19720, capital_tid, 485, 6, 450_000_000.0);
    clone_type_with(&mut ctx.sde, 587, frig_tid, 25, 6, 1_500_000.0);

    let (host_sys, c13_dest, char_id, ship_id) = (30_000_142_u32, 31_002_580_u32, 96_201_i64, 78_201_i64);
    let now_u = ctx.now_unix();
    let pos = [10_000.0, 0.0, 0.0];

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
                type_id: cruiser_tid,
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
            vec![
                make_wh_sig("SM-WH", host_sys, pos, Some(34_134), Some(c13_dest), now_u - 60, 86_400, 100_000_000.0, 100_000_000.0, 5_000_000.0),
                make_wh_sig("ZERO-MASS", host_sys, pos, Some(30_001), Some(30_000_144), now_u - 60, 86_400, 100_000_000.0, 0.0, 300_000_000.0),
                make_wh_sig("EXP-LIFE", host_sys, pos, Some(30_001), Some(30_000_144), now_u - 90_000, 86_400, 100_000_000.0, 50_000_000.0, 300_000_000.0),
                make_wh_sig("MED-WH", host_sys, pos, Some(30_001), Some(30_000_144), now_u - 60, 86_400, 500_000_000.0, 500_000_000.0, 20_000_000.0),
            ],
        );
    });

    let mut sess = make_session(char_id, 98_000_001, Some(host_sys), Some(ship_id));
    let near_ball = make_ship_ball(ship_id, cruiser_tid, 12_000_000.0, DVec3::new(11_000.0, 0.0, 0.0));

    record_session_change_timer(&ctx, char_id, now_u, host_sys, 1, "undock");
    let timer_err = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, near_ball.clone(), None, Some("SM-WH"))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(timer_err.code, "SessionChangeInProgress");
    clear_session_change_timer(&ctx, char_id);

    assert!(execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, near_ball.clone(), Some(999_999_999), None).await.is_none());

    for (sig_name, expected_err) in [
        ("NO-SUCH-WH", "WormholeNotFound"),
        ("SM-WH", "ShipTooHeavyForWormhole"),
    ] {
        let err = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, near_ball.clone(), None, Some(sig_name))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, expected_err);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = capital_tid;
    });
    let cap_ball = make_ship_ball(ship_id, capital_tid, 450_000_000.0, DVec3::new(11_000.0, 0.0, 0.0));
    let cap_err = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, cap_ball, None, Some("MED-WH"))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(cap_err.code, "ShipTooHeavyForWormhole");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = frig_tid;
    });
    let frig_ball = make_ship_ball(ship_id, frig_tid, 1_500_000.0, DVec3::new(11_000.0, 0.0, 0.0));
    for (sig_name, expected_err) in [
        ("ZERO-MASS", "WormholeMassCollapsed"),
        ("EXP-LIFE", "WormholeCollapsed"),
    ] {
        let err = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, frig_ball.clone(), None, Some(sig_name))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, expected_err);
    }

    let host_h = ctx.systems.get_or_spawn(host_sys);
    let mut cs = CombatState::default();
    cs.max_shield_hp = 1000.0;
    cs.shield_hp = 500.0;
    cs.max_armor_hp = 1000.0;
    cs.armor_hp = 250.0;
    let _ = host_h.spawn_ball_with_meta(frig_ball.clone(), cs, SlimItemMeta::default()).await;
    let sm_wh_item_id = wormhole_item_id_for_signature(host_sys, "SM-WH");
    let jump_res = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, frig_ball, Some(sm_wh_item_id), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jump_res.dict_get("destinationSystemID").and_then(PyRep::as_i64), Some(c13_dest as i64));
    assert_eq!(jump_res.dict_get("delayedLocal").and_then(PyRep::as_bool), Some(true));
}

#[tokio::test]
async fn mutant_kill_wormhole_jump_phenomena_multipliers_and_fallback_branches() {
    use std::collections::HashMap;

    let mut ctx = test_ctx();
    let _ = system_wormhole_profile_sde(&ctx.sde, 31_000_001);

    let frig_tid = 99_772_u32;
    clone_type_with(&mut ctx.sde, 587, frig_tid, 25, 6, 1_500_000.0);

    let find_sys = |sde: &SdeRegistry, phenom: &str| -> u32 {
        *sde.solar_systems
            .keys()
            .find(|&&id| {
                eve_dogma::resolve_system_wormhole_phenomenon(sde, id, None)
                    .is_some_and(|(_, _, name, _)| name == phenom)
            })
            .expect("phenomenon system")
    };

    let bh_sys = find_sys(&ctx.sde, "Black Hole");
    let pulsar_sys = find_sys(&ctx.sde, "Pulsar");
    let wr_sys = find_sys(&ctx.sde, "Wolf-Rayet");
    let cv_sys = find_sys(&ctx.sde, "Cataclysmic Variable");

    let bh_eff_tid = eve_dogma::resolve_system_wormhole_phenomenon(&ctx.sde, bh_sys, None)
        .unwrap()
        .3;
    let mut eff_attrs = HashMap::from([
        (eve_sde::attr_ids::MAX_VELOCITY, 300.0_f64),
        (eve_sde::attr_ids::AGILITY, 2.0_f64),
    ]);
    assert!(eve_dogma::apply_wormhole_phenomenon_to_attributes(
        &ctx.sde,
        bh_eff_tid,
        6,
        false,
        &mut eff_attrs,
    ));
    let expected_v_mult = eff_attrs[&eve_sde::attr_ids::MAX_VELOCITY] / 300.0;
    let expected_in_mult = eff_attrs[&eve_sde::attr_ids::AGILITY] / 2.0;

    let host_sys = 30_000_142_u32;
    let char_id = 96_299_i64;
    let ship_id = 78_299_i64;
    let now_u = ctx.now_unix();
    let pos = [10_000.0, 0.0, 0.0];

    let seed_and_jump = |ctx: &ServerContext, dest_sys: u32, sig_id: &str| {
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
                    type_id: frig_tid,
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
                vec![make_wh_sig(
                    sig_id,
                    host_sys,
                    pos,
                    Some(30_001),
                    Some(dest_sys),
                    now_u - 60,
                    86_400,
                    500_000_000.0,
                    500_000_000.0,
                    20_000_000.0,
                )],
            );
        });
    };

    // Part 1: Real SDE phenomena (Black Hole modifies velocity + inertia, Pulsar leaves them None)
    {
        seed_and_jump(&ctx, bh_sys, "WH-BH");
        clear_session_change_timer(&ctx, char_id);
        let host_h = ctx.systems.get_or_spawn(host_sys);
        let ball = make_ship_ball(ship_id, frig_tid, 1_500_000.0, DVec3::new(11_000.0, 0.0, 0.0));
        let _ = host_h.spawn_ball_with_meta(ball.clone(), CombatState::default(), SlimItemMeta::default()).await;
        let mut sess = make_session(char_id, 98_000_001, Some(host_sys), Some(ship_id));
        let res = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, ball, None, Some("WH-BH"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res.dict_get("destinationSystemID"), Some(&PyRep::Integer(bh_sys as i64)));
        let dest_h = ctx.systems.get_or_spawn(bh_sys);
        let bp = dest_h.ballpark.read().unwrap();
        let cs = bp.combat_states.get(&ship_id).expect("Black Hole combat state");
        assert!(
            (cs.external_velocity_multiplier.unwrap() - expected_v_mult).abs() < 1e-6,
            "got {:?}, expected {expected_v_mult}",
            cs.external_velocity_multiplier
        );
        assert!(
            (cs.external_inertia_multiplier.unwrap() - expected_in_mult).abs() < 1e-6,
            "got {:?}, expected {expected_in_mult}",
            cs.external_inertia_multiplier
        );
    }
    {
        seed_and_jump(&ctx, pulsar_sys, "WH-PUL");
        clear_session_change_timer(&ctx, char_id);
        let host_h = ctx.systems.get_or_spawn(host_sys);
        let ball = make_ship_ball(ship_id, frig_tid, 1_500_000.0, DVec3::new(11_000.0, 0.0, 0.0));
        let _ = host_h.spawn_ball_with_meta(ball.clone(), CombatState::default(), SlimItemMeta::default()).await;
        let mut sess = make_session(char_id, 98_000_001, Some(host_sys), Some(ship_id));
        let res = execute_cosmic_wormhole_jump(&ctx, &mut sess, char_id, host_sys, ship_id, ball, None, Some("WH-PUL"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res.dict_get("destinationSystemID"), Some(&PyRep::Integer(pulsar_sys as i64)));
        let dest_h = ctx.systems.get_or_spawn(pulsar_sys);
        let bp = dest_h.ballpark.read().unwrap();
        let cs = bp.combat_states.get(&ship_id).expect("Pulsar combat state");
        assert!(cs.external_velocity_multiplier.is_none());
        assert!(cs.external_inertia_multiplier.is_none());
    }

    // Part 2: Clear wormhole_class_id on cloned SDE for 4 distinct system IDs so
    // resolve_system_wormhole_phenomenon returns None while system_wormhole_profile_sde
    // still returns Pulsar / Wolf-Rayet / Black Hole / Cataclysmic Variable -> fallback match arms
    let mut fb_ctx = test_ctx();
    clone_type_with(&mut fb_ctx.sde, 587, frig_tid, 25, 6, 1_500_000.0);
    {
        let reg = Arc::make_mut(&mut fb_ctx.sde);
        let mut no_wh_reg = reg.regions.values().next().unwrap().clone();
        no_wh_reg.region_id = 99_777_002;
        no_wh_reg.wormhole_class_id = None;
        reg.regions.insert(99_777_002, no_wh_reg);
        let mut no_wh_const = reg.constellations.values().next().unwrap().clone();
        no_wh_const.constellation_id = 99_777_001;
        no_wh_const.region_id = 99_777_002;
        no_wh_const.wormhole_class_id = None;
        reg.constellations.insert(99_777_001, no_wh_const);
        for sys_id in [pulsar_sys, wr_sys, bh_sys, cv_sys] {
            if let Some(sys) = reg.solar_systems.get_mut(&sys_id) {
                sys.wormhole_class_id = None;
                sys.constellation_id = 99_777_001;
                sys.region_id = 99_777_002;
            }
        }
    }

    for (dest_sys, sig_id, label) in [
        (pulsar_sys, "FB-PUL", "Pulsar"),
        (wr_sys, "FB-WR", "Wolf-Rayet"),
        (bh_sys, "FB-BH", "Black Hole"),
        (cv_sys, "FB-CV", "Cataclysmic Variable"),
    ] {
        assert!(eve_dogma::resolve_system_wormhole_phenomenon(&fb_ctx.sde, dest_sys, None).is_none());
        assert_eq!(system_wormhole_profile_sde(&fb_ctx.sde, dest_sys).3, label);

        fb_ctx.db.with_mut_snapshot(|snap| {
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
                    type_id: frig_tid,
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
                vec![make_wh_sig(
                    sig_id,
                    host_sys,
                    pos,
                    Some(30_001),
                    Some(dest_sys),
                    now_u - 60,
                    86_400,
                    500_000_000.0,
                    500_000_000.0,
                    20_000_000.0,
                )],
            );
        });
        clear_session_change_timer(&fb_ctx, char_id);
        let host_h = fb_ctx.systems.get_or_spawn(host_sys);
        let ball = make_ship_ball(ship_id, frig_tid, 1_500_000.0, DVec3::new(11_000.0, 0.0, 0.0));
        let _ = host_h.spawn_ball_with_meta(ball.clone(), CombatState::default(), SlimItemMeta::default()).await;
        let mut sess = make_session(char_id, 98_000_001, Some(host_sys), Some(ship_id));
        execute_cosmic_wormhole_jump(&fb_ctx, &mut sess, char_id, host_sys, ship_id, ball, None, Some(sig_id))
            .await
            .unwrap()
            .unwrap();
        let dest_h = fb_ctx.systems.get_or_spawn(dest_sys);
        let bp = dest_h.ballpark.read().unwrap();
        let cs = bp.combat_states.get(&ship_id).expect("fallback env combat state");
        match label {
            "Pulsar" => {
                assert_eq!(cs.env_max_shield_multiplier, Some(1.30));
                assert_eq!(cs.env_sig_multiplier, Some(1.30));
            }
            "Wolf-Rayet" => {
                assert_eq!(cs.env_max_armor_multiplier, Some(1.30));
                assert_eq!(cs.env_sig_multiplier, Some(0.85));
            }
            "Black Hole" => {
                assert_eq!(cs.external_velocity_multiplier, Some(1.30));
                assert_eq!(cs.external_inertia_multiplier, Some(1.15));
            }
            "Cataclysmic Variable" => {
                assert_eq!(cs.env_max_cap_multiplier, Some(1.30));
            }
            _ => unreachable!(),
        }
    }
}
