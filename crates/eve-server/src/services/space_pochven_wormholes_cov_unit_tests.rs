//! Focused coverage and mutation-killing unit tests for `space_pochven_wormholes`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::WorldRepository;
use eve_sde::{InvType, SdeRegistry, SolarSystem, StargateJump, WormholeTypeInfo};
use std::collections::HashMap;
use std::sync::Arc;

fn mk_sys(
    id: u32,
    constellation_id: u32,
    region_id: u32,
    pos_ly: [f64; 3],
    wh_class: Option<i32>,
) -> SolarSystem {
    SolarSystem {
        solar_system_id: id,
        constellation_id,
        region_id,
        name: format!("Sys-{id}"),
        security_status: 0.5,
        radius: 1.0e12,
        galactic_pos_ly: pos_ly,
        planet_ids: Vec::new(),
        stations: Vec::new(),
        wormhole_class_id: wh_class,
    }
}

fn mk_sig(sys_id: u32, wh_tid: Option<u32>, dest: Option<u32>, pos: [f64; 3]) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id: "TST-100".into(),
        solar_system_id: sys_id,
        sig_group: "Wormhole".into(),
        type_name: "Wormhole".into(),
        position: pos,
        signal_strength: 1.0,
        wormhole_type_id: wh_tid,
        destination_system_id: dest,
        created_unix: 1_000,
        lifetime_seconds: 3_600,
        max_mass_kg: 1.0e9,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 1.0e8,
        collapsed: false,
    }
}

#[test]
fn wormhole_type_ranges_and_synthetic_ly_distance_boundaries() {
    let mut sde = SdeRegistry::default();
    let poch_sys = 30_002_001_u32;
    let poch_sys_2 = 30_002_002_u32;
    let poch_zero = 30_002_003_u32;
    let k_exact = 30_999_999_u32; // < 31_000_000 boundary
    let w_boundary = 31_000_000_u32; // >= 31_000_000 excluded
    let abyss_special = 30_100_000_u32;
    let k_too_far = 30_000_500_u32;
    let k_zero_pos = 30_000_501_u32;

    sde.solar_systems
        .insert(poch_sys, mk_sys(poch_sys, 20_001, POCHVEN_REGION_ID, [1.0, 0.0, 0.0], None));
    sde.solar_systems.insert(
        poch_sys_2,
        mk_sys(poch_sys_2, 20_001, POCHVEN_REGION_ID, [2.0, 0.0, 0.0], None),
    );
    sde.solar_systems
        .insert(poch_zero, mk_sys(poch_zero, 20_001, POCHVEN_REGION_ID, [0.0, 0.0, 0.0], None));
    // Exact 5.0 LY from poch_sys ([1,0,0] -> [6,0,0])
    sde.solar_systems
        .insert(k_exact, mk_sys(k_exact, 20_002, 10_000_002, [6.0, 0.0, 0.0], None));
    sde.solar_systems.insert(
        w_boundary,
        mk_sys(w_boundary, 20_002, 10_000_002, [2.0, 0.0, 0.0], Some(4)),
    );
    sde.solar_systems.insert(
        abyss_special,
        mk_sys(abyss_special, 20_002, 10_000_002, [2.0, 0.0, 0.0], None),
    );
    sde.solar_systems.insert(
        k_too_far,
        mk_sys(k_too_far, 20_002, 10_000_002, [6.001, 0.0, 0.0], None),
    );
    sde.solar_systems.insert(
        k_zero_pos,
        mk_sys(k_zero_pos, 20_002, 10_000_002, [0.0, 0.0, 0.0], None),
    );
    // Excluded special regions (Jove / UUA-F4 / Polaris / Yasna Zakh)
    for (idx, reg) in [10_000_004_u32, 10_000_017, 10_000_019, 10_001_000]
        .into_iter()
        .enumerate()
    {
        let sid = 30_000_600 + idx as u32;
        sde.solar_systems
            .insert(sid, mk_sys(sid, 20_003, reg, [2.0, 0.0, 0.0], None));
    }

    // 1. `is_pochven_wormhole` exact wormhole type range boundaries + origin/dest region checks
    for &(tid, expected) in &[
        (Some(56_025), false),
        (Some(56_026), true),
        (Some(56_027), false),
        (Some(56_534), false),
        (Some(56_535), true),
        (Some(56_565), true),
        (Some(56_566), false),
        (Some(92_286), false),
        (Some(92_287), true),
        (Some(92_289), true),
        (Some(92_290), false),
        (Some(97_880), false),
        (Some(97_881), true),
        (Some(97_882), true),
        (Some(97_883), false),
        (None, false),
    ] {
        let sig = mk_sig(k_exact, tid, None, [10.0, 20.0, 30.0]);
        assert_eq!(is_pochven_wormhole(&sde, &sig), expected, "tid={tid:?}");
    }
    // Origin in Pochven with non-Pochven WH type and no dest -> true (+30km offset)
    let sig_from_poch = mk_sig(poch_sys, None, None, [100.0, 200.0, 300.0]);
    assert!(is_pochven_wormhole(&sde, &sig_from_poch));
    assert_eq!(
        pochven_wormhole_ball_position(&sde, &sig_from_poch),
        DVec3::new(30_100.0, 200.0, 300.0)
    );

    // 2. `pochven_kspace_neighbors_within_ly`: zero-pos origin returns empty; exact 5.0 LY included, >5.0 & excluded regions/IDs filtered
    assert!(pochven_kspace_neighbors_within_ly(&sde, poch_zero, 5.0).is_empty());
    let neighbors = pochven_kspace_neighbors_within_ly(&sde, poch_sys, 5.0);
    assert_eq!(neighbors, vec![k_exact]);

    // 3. `nearest_pochven_system_within_ly`: zero-pos K-space, >=31_000_000, ==30_100_000 -> None; seq cycles across matches
    assert_eq!(nearest_pochven_system_within_ly(&sde, k_zero_pos, 10.0, 0), None);
    assert_eq!(nearest_pochven_system_within_ly(&sde, w_boundary, 10.0, 0), None);
    assert_eq!(nearest_pochven_system_within_ly(&sde, abyss_special, 10.0, 0), None);
    let m0 = nearest_pochven_system_within_ly(&sde, k_exact, 5.0, 0).unwrap();
    let m1 = nearest_pochven_system_within_ly(&sde, k_exact, 5.0, 1).unwrap();
    assert_ne!(m0, m1);
    assert!(matches!(m0, 30_002_001 | 30_002_002));
    assert!(matches!(m1, 30_002_001 | 30_002_002));
}

#[test]
fn synthetic_topology_static_wormhole_fallbacks_and_standing_boundary() {
    // 1. Synthetic Pochven jump graph exercising border, home (2 degree-3 neighbors), and internal pools
    let mut sde = SdeRegistry::default();
    // Constellation 1 triangle: 101 (home) connected to 102 and 103; 102 and 103 also connect to each other AND to border 201 (Constellation 2) -> degree(102)=3, degree(103)=3, degree(101)=2 -> 101 is home, 102 & 103 & 201 are border; 202 (degree 1 to 201) is internal!
    for &(sid, cid) in &[
        (30_010_101, 1),
        (30_010_102, 1),
        (30_010_103, 1),
        (30_010_201, 2),
        (30_010_202, 2),
    ] {
        sde.solar_systems
            .insert(sid, mk_sys(sid, cid, POCHVEN_REGION_ID, [0.0; 3], None));
    }
    for (idx, &(a, b)) in [
        (30_010_101, 30_010_102),
        (30_010_101, 30_010_103),
        (30_010_102, 30_010_103),
        (30_010_102, 30_010_201),
        (30_010_103, 30_010_201),
        (30_010_201, 30_010_202),
    ]
    .iter()
    .enumerate()
    {
        sde.jumps.insert(
            idx as i64 + 1,
            StargateJump {
                stargate_id: idx as i64 + 1,
                from_solar_system_id: a,
                to_solar_system_id: b,
                destination_stargate_id: idx as i64 + 100,
                position: DVec3::ZERO,
            },
        );
    }
    let (border, internal, home) = pochven_topology_pools(&sde);
    assert_eq!(border, vec![30_010_102, 30_010_103, 30_010_201]);
    assert_eq!(home, vec![30_010_101]);
    assert_eq!(internal, vec![30_010_202]);

    // 2. `pochven_static_wormholes` fallback when SDE is empty AND when ONLY Pochven systems exist
    let wh_empty = pochven_static_wormholes(&SdeRegistry::default(), 30_010_101, 2, 1_700_000_000);
    assert_eq!(wh_empty[0].destination_system_id, Some(30_010_101));
    assert_eq!(wh_empty[1].destination_system_id, Some(30_010_101));
    let wh_only_poch = pochven_static_wormholes(&sde, 30_010_101, 2, 1_700_000_000);
    assert_eq!(wh_only_poch.len(), 2);
    assert_eq!(wh_only_poch[0].lifetime_seconds, 43_200);
    assert!(sde.solar_systems.contains_key(&wh_only_poch[0].destination_system_id.unwrap()));
    assert_eq!(wh_only_poch[1].destination_system_id, Some(30_010_101));
    assert_eq!(wh_only_poch[0].position[1], 1.5 * 149_597_870_700.0); // 1.5 AU in meters

    // Add a far K-space system, a C2 wormhole system (no C4), and custom wormhole_types
    sde.solar_systems
        .insert(30_000_142, mk_sys(30_000_142, 99, 10_000_002, [500.0, 0.0, 0.0], None));
    sde.solar_systems
        .insert(31_000_111, mk_sys(31_000_111, 98, 11_000_001, [0.0; 3], Some(2)));
    sde.wormhole_types.insert(
        56_026,
        WormholeTypeInfo {
            type_id: 56_026,
            name: "C729".into(),
            target_system_class: 7,
            max_stable_time_min: 120.0,
            max_stable_mass: 2.0e9,
            mass_regeneration: 0.0,
            max_jump_mass: 3.0e8,
        },
    );
    let wh_fb2 = pochven_static_wormholes(&sde, 30_010_101, 0, 1_700_000_000);
    assert_eq!(wh_fb2[0].destination_system_id, Some(30_000_142));
    assert_eq!(wh_fb2[0].lifetime_seconds, 7_200);
    assert_eq!(wh_fb2[1].destination_system_id, Some(31_000_111));

    // 3. `resolve_pochven_filament_candidates` exact standing == 6.0 boundary + all filament variants
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (r_border, r_internal, r_home) = pochven_topology_pools(&ctx.sde);
    let poch_origin = r_border[0];
    let cid = 915_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, cid, "PochTester", 98_000_001, poch_origin, 100_000_001);
        ch.standings.insert(500_026, 5.999);
        snap.characters.insert(cid, ch);
    });
    // Missing character defaults to 0.0 -> Home-15 (56081) rejected
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_081, 30_000_142, 777_888_999)
            .unwrap_err()
            .code,
        "SystemCheck_JumpFailed_StandingsTooLow"
    );
    assert!(resolve_pochven_filament_candidates(&ctx, 56_081, 30_000_142, cid).is_err());

    // Exact 6.0 standing unlocks Home-5/15 (56080, 56081) and retains home in Krai-15 variants
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().standings.insert(500_026, 6.0);
    });
    for ftid in [56_080, 56_081] {
        assert_eq!(
            resolve_pochven_filament_candidates(&ctx, ftid, 30_000_142, cid).unwrap(),
            r_home
        );
    }
    for ftid in [56_071, 56_073, 56_075] {
        assert_eq!(
            resolve_pochven_filament_candidates(&ctx, ftid, 30_000_142, cid)
                .unwrap()
                .len(),
            9
        );
    }
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_077, 30_000_142, cid).unwrap(),
        r_border
    );
    assert_eq!(
        resolve_pochven_filament_candidates(&ctx, 56_079, 30_000_142, cid).unwrap(),
        r_internal
    );
    for ftid in [56_066, 56_067, 56_069] {
        assert!(!resolve_pochven_filament_candidates(&ctx, ftid, poch_origin, cid)
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn guard_idempotency_non_pochven_side_and_missing_npc_faction_defaults() {
    // 1. Idempotency on real SDE context: second call reuses existing balls and K162 signature
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (border, _, _) = pochven_topology_pools(&ctx.sde);
    let poch_sys = border[0];
    let dest_sys = 30_000_142_u32;
    let mut sig = pochven_static_wormholes(&ctx.sde, poch_sys, 0, 1_800_000_000)[0].clone();
    sig.destination_system_id = Some(dest_sys);
    sig.remaining_mass_kg = 0.0; // verifies `.max(1.0)` mass clamp

    let (_, g1) = open_pochven_wormhole_and_spawn_guards(&ctx, poch_sys, &sig, 910_001).await;
    let (_, g2) = open_pochven_wormhole_and_spawn_guards(&ctx, poch_sys, &sig, 910_001).await;
    assert_eq!(g1, g2);
    let k162_count = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&dest_sys)
            .unwrap()
            .iter()
            .filter(|s| s.signature_id == format!("K162-{}", sig.signature_id))
            .count()
    });
    assert_eq!(k162_count, 1, "re-opening wormhole must not duplicate K162 signature");

    // 2. Non-Pochven side (`is_pochven_side == false`, `wormhole_type_id == None`) spawns 2 guards and no K162
    let k_sig = mk_sig(dest_sys, None, Some(30_000_144), [500.0, 0.0, 0.0]);
    let (k_pos, k_guards) =
        open_pochven_wormhole_and_spawn_guards(&ctx, dest_sys, &k_sig, 920_001).await;
    assert_eq!(k_pos, DVec3::new(500.0, 0.0, 0.0));
    assert_eq!(k_guards.len(), 2);

    // 3. Synthetic SDE with `faction_id: None` on guard NPC types exercises 500_026 vs 500_024 fallback
    let mut synth_sde = SdeRegistry::default();
    synth_sde
        .solar_systems
        .insert(poch_sys, mk_sys(poch_sys, 1, POCHVEN_REGION_ID, [0.0; 3], None));
    for &(tid, mass, radius) in &[(48_087_u32, 8_000.0, 55.0), (47_958_u32, 12_000.0, 75.0)] {
        synth_sde.types.insert(
            tid,
            InvType {
                type_id: tid,
                group_id: 500,
                category_id: 11,
                name: format!("Guard-{tid}"),
                mass,
                volume: 1.0,
                capacity: 0.0,
                radius,
                base_price: 0.0,
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
                default_attributes: HashMap::new(),
            },
        );
    }
    let synth_ctx = ServerContext::with_sde_and_repository(
        Arc::new(synth_sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let spawned =
        ensure_pochven_side_guards(&synth_ctx, poch_sys, 930_000, DVec3::ZERO, true).await;
    assert_eq!(spawned.len(), 6);
    crate::services::combat_crimewatch::with_mut_combat_runtime(&synth_ctx, |rt| {
        assert_eq!(rt.npc_entities[&930_010].faction_id, 500_026);
        assert_eq!(rt.npc_entities[&930_011].faction_id, 500_024);
    });
}
