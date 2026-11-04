//! End-to-end V24.01 integration tests closing Small-Ship Shattered Wormholes (25 J000xxx),
//! Standard Shattered Wormholes (no moons, Upwell refused, K-space static, +/-1 class sites,
//! Shattered Debris Field + Shattered Ice Field with delayed Sleeper wave & stasis web),
//! Drifter Wormhole Systems (J200727, J164710, J174618, J055520, J110145 with C2 phenomena & K-space Drifter AI),
//! and Thera NPC Stations (31000005).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::space_destiny::{
    ensure_system_signatures_and_anomalies, wormhole_item_id_for_signature,
};
use eve_server::services::wormhole_lifecycle_ops::{
    statics_for_system_sde, system_wormhole_profile_sde,
};
use eve_server::services::wormhole_pve_escalations::{
    SHATTERED_DEBRIS_ORE_TYPES, SHATTERED_ICE_TYPES,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn ctx_with_real_sde() -> ServerContext {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

#[allow(clippy::too_many_arguments)]
async fn place_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
    mass_kg: f64,
) {
    let prev_sys = sess.solar_system_id.unwrap_or(sys_id);
    if prev_sys != sys_id {
        let _ = ctx.systems.get_or_spawn(prev_sys).remove_ball(ship_id).await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    let inertia = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&70).copied())
        .unwrap_or(0.5);
    let h = ctx.systems.get_or_spawn(sys_id);
    let _ = h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia,
                mass: mass_kg,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        )
        .await;
}

async fn fit_and_activate_zpme(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    mod_id: i64,
) {
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 49_099, // Zero-Point Mass Entangler
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(
        sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate Zero-Point Mass Entangler via live dogmaIM.Activate");
}

// parity: feat:navigation.small-ship-shattered-wormholes, feat:navigation.standard-shattered-wormholes, feat:navigation.drifter-wormhole-systems, feat:navigation.thera-npc-stations
#[tokio::test]
async fn test_small_ship_standard_shattered_drifter_and_thera_systems() {
    eve_testkit::attest(&[
        "feat:navigation.small-ship-shattered-wormholes",
        "feat:navigation.standard-shattered-wormholes",
        "feat:navigation.drifter-wormhole-systems",
        "feat:navigation.thera-npc-stations",
    ]);
    let ctx = ctx_with_real_sde();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 620_000_001_i64;
    let mut sess = SessionState::new(930_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Small-ship shattered wormholes: all 25 J000xxx systems (31002580..=31002604)
    let c13_count = (31_002_580_u32..=31_002_604_u32)
        .filter(|&sid| system_wormhole_profile_sde(&ctx.sde, sid).0 == 13)
        .count();
    assert_eq!(c13_count, 25, "All 25 J000xxx systems must resolve to wormhole class 13");

    let c13_sys = 31_002_585_u32; // J000452
    let (_, _, c13_shattered, c13_eff) = system_wormhole_profile_sde(&ctx.sde, c13_sys);
    assert!(c13_shattered);
    assert_eq!(c13_eff, "Wolf-Rayet", "C13 systems apply C6 Wolf-Rayet effects");

    let _ = ensure_system_signatures_and_anomalies(&ctx, c13_sys).await;
    let _ = ensure_system_pve_sites_seeded(&ctx, c13_sys).await;
    let c13_wh = ctx
        .db
        .with_snapshot(|s| {
            s.cosmic_signatures
                .get(&c13_sys)
                .and_then(|l| l.iter().find(|w| w.sig_group == "Wormhole"))
                .cloned()
        })
        .expect("C13 wormhole");
    assert_eq!(c13_wh.max_jump_mass_kg, 5_000_000.0);
    let c13_wh_id = wormhole_item_id_for_signature(c13_sys, &c13_wh.signature_id);
    let c13_wh_pos = DVec3::from_array(c13_wh.position);

    let (c13_site_count, c13_min_cls, c13_max_cls, c13_life) =
        with_pve_exploration_reg(&ctx, |reg| {
            let sites = reg.sites_by_system.get(&c13_sys).cloned().unwrap_or_default();
            let min_c = sites.iter().filter_map(|s| s.wormhole_class).min().unwrap_or(0);
            let max_c = sites.iter().filter_map(|s| s.wormhole_class).max().unwrap_or(0);
            let life = sites.first().map(|s| s.lifetime_seconds).unwrap_or(0);
            (sites.len(), min_c, max_c, life)
        });
    let normal_c2 = 31_000_500_u32;
    let (normal_c2_cls, _, normal_c2_shattered, _) = system_wormhole_profile_sde(&ctx.sde, normal_c2);
    assert!((1..=3).contains(&normal_c2_cls) && !normal_c2_shattered);
    let _ = ensure_system_signatures_and_anomalies(&ctx, normal_c2).await;
    let _ = ensure_system_pve_sites_seeded(&ctx, normal_c2).await;
    let normal_site_count = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.get(&normal_c2).map(|s| s.len()).unwrap_or(0)
    });
    let c13_sig_count = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures
            .get(&c13_sys)
            .map(|l| l.iter().filter(|sig| !sig.signature_id.starts_with("K162-")).count())
            .unwrap_or(0)
    });
    assert!(
        c13_site_count > normal_site_count,
        "C13 must spawn more sites than normal C1-C3 W-space ({c13_site_count} vs {normal_site_count})"
    );
    assert!(
        c13_sig_count >= 10,
        "C13 must spawn elevated signature count (got {c13_sig_count})"
    );
    assert_eq!((c13_min_cls, c13_max_cls), (1, 3), "C13 spawns C1-C3 sites");
    assert_eq!(c13_life, 14_400, "C13 sites respawn every 4h (14400s)");

    // Frigate (Rifter 587, 1.067M kg) and Destroyer (Thrasher 16242, 1.55M kg) admitted directly
    let frig_ship_id = 620_000_015_i64;
    place_ship(&ctx, &mut sess, char_id, c13_sys, frig_ship_id, 587, c13_wh_pos, 1_067_000.0).await;
    ctx.advance_secs(15);
    let frig_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect("Frigate must be admitted to small-ship wormhole");
    assert_eq!(frig_rep.dict_get("success").and_then(|v| v.as_bool()), Some(true));

    ctx.advance_secs(315);
    let dest_ship_id = 620_000_016_i64;
    place_ship(&ctx, &mut sess, char_id, c13_sys, dest_ship_id, 16_242, c13_wh_pos, 1_550_000.0).await;
    let dest_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect("Destroyer must be admitted to small-ship wormhole");
    assert_eq!(dest_rep.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    ctx.advance_secs(315);

    // Cruiser (Caracal 621, 11.3M kg) rejected
    place_ship(&ctx, &mut sess, char_id, c13_sys, ship_id, 621, c13_wh_pos, 11_300_000.0).await;
    ctx.advance_secs(15);
    let cruiser_err = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect_err("Cruiser must be rejected from small-ship wormhole");
    assert_eq!(cruiser_err.code, "ShipTooHeavyForWormhole");

    // Unentangled HIC (Devoter 12017, 13.5M kg) rejected; ZPME activated via live dogmaIM.Activate admitted!
    let hic_ship_id = 620_000_002_i64;
    place_ship(&ctx, &mut sess, char_id, c13_sys, hic_ship_id, 12017, c13_wh_pos, 13_500_000.0).await;
    let hic_heavy_err = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect_err("Unentangled HIC must be rejected from small-ship wormhole");
    assert_eq!(hic_heavy_err.code, "ShipTooHeavyForWormhole");

    fit_and_activate_zpme(&ctx, &mut sess, char_id, hic_ship_id, 620_000_050).await;
    // Clear ZPME self-warp-scramble on ballpark after cycle completion or fit fresh ship for return test
    let _ = ctx.systems.get_or_spawn(c13_sys).step_manual(31.0).await;
    fit_and_activate_zpme(&ctx, &mut sess, char_id, hic_ship_id, 620_000_051).await;
    let hic_ok = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect("Mass-entangled HIC admitted through small-ship wormhole");
    assert_eq!(hic_ok.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    let c13_dest_sys = hic_ok.dict_get("destinationSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let c13_exit_id = hic_ok.dict_get("exitWormholeID").and_then(|v| v.as_i64()).unwrap();
    let c13_exit_pos = ctx.systems.get_or_spawn(c13_dest_sys).get_ball_snapshot(c13_exit_id).await.unwrap().unwrap().1.position;
    let arrived_pos = ctx.systems.get_or_spawn(c13_dest_sys).get_ball_snapshot(hic_ship_id).await.unwrap().unwrap().1.position;
    let arrival_dist = arrived_pos.distance(c13_exit_pos);
    assert!(
        arrival_dist <= 6_000.0,
        "Arriving ship must spawn within ~5 km of exit wormhole (got {arrival_dist:.1} m)"
    );
    let base_devoter_armor = ctx.build_ship_combat_state(char_id, hic_ship_id, 12017).max_armor_hp;

    ctx.advance_secs(315);
    let _ = ctx.systems.get_or_spawn(c13_dest_sys).step_manual(31.0).await;
    fit_and_activate_zpme(&ctx, &mut sess, char_id, hic_ship_id, 620_000_052).await;
    let jump_into_c13 = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_exit_id)], &kw, None)
        .await
        .expect("Mass-entangled HIC jumps back into C13 system");
    assert_eq!(jump_into_c13.dict_get("environmentalEffect").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
    let c13_cs = ctx.systems.get_or_spawn(c13_sys).get_combat_state(hic_ship_id).await.unwrap().unwrap();
    assert!(
        c13_cs.max_armor_hp > base_devoter_armor * 1.5,
        "C13 must apply C6 Wolf-Rayet armor bonus: {} vs base {}",
        c13_cs.max_armor_hp,
        base_devoter_armor
    );

    // Porpoise (42244, 4.9M kg) admitted directly; Odysseus (89607, 15.0M kg) admitted via live dogmaIM.Activate ZPME
    ctx.advance_secs(315);
    let porpoise_id = 620_000_003_i64;
    place_ship(&ctx, &mut sess, char_id, c13_sys, porpoise_id, 42_244, c13_wh_pos, 4_900_000.0).await;
    let porpoise_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect("Porpoise must be admitted to C13 wormhole");
    assert_eq!(porpoise_rep.dict_get("success").and_then(|v| v.as_bool()), Some(true));

    ctx.advance_secs(315);
    let odysseus_id = 620_000_004_i64;
    place_ship(&ctx, &mut sess, char_id, c13_sys, odysseus_id, 89_607, c13_wh_pos, 15_000_000.0).await;
    let odysseus_unentangled = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect_err("Unentangled Odysseus (15M kg) must be rejected from small-ship wormhole");
    assert_eq!(odysseus_unentangled.code, "ShipTooHeavyForWormhole");
    fit_and_activate_zpme(&ctx, &mut sess, char_id, odysseus_id, 620_000_053).await;
    let odysseus_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c13_wh_id)], &kw, None)
        .await
        .expect("Mass-entangled Odysseus must be admitted to C13 wormhole");
    assert_eq!(odysseus_rep.dict_get("success").and_then(|v| v.as_bool()), Some(true));

    // 2. Standard Shattered Wormholes (31002538 C4 Shattered)
    let shat_c4 = 31_002_538_u32;
    let (shat_cls, _, shat_flag, _) = system_wormhole_profile_sde(&ctx.sde, shat_c4);
    assert_eq!(shat_cls, 4);
    assert!(shat_flag);
    let moon_count = ctx.sde.celestials_in_system(shat_c4).into_iter().filter(|c| c.group_id == 8).count();
    assert_eq!(moon_count, 0, "Shattered wormhole system must have 0 moons");

    ctx.advance_secs(15);
    place_ship(&ctx, &mut sess, char_id, shat_c4, ship_id, 652, DVec3::new(10_000_000.0, 0.0, 0.0), 15_000_000.0).await;
    let anchor_err = ctx
        .dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832)], &kw, None)
        .await
        .expect_err("Upwell anchoring must be refused in shattered wormhole system");
    assert_eq!(anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

    // C4 shattered has 2 C4 W-space statics + 1 extra C3-type K-space static (D845)
    let shat_statics = statics_for_system_sde(&ctx.sde, shat_c4);
    assert_eq!(shat_statics.len(), 3, "Standard shattered C4 must have 2 C4 W-space statics + 1 K-space static");
    assert_eq!(shat_statics[2].1, "D845", "C4 shattered system uses C3-type K-space static D845");

    // Seed sites via live scanSvc.GetCombatAnomalyInstances RPC
    let _ = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[], &kw, None)
        .await
        .expect("scanSvc.GetCombatAnomalyInstances in shattered C4");
    let (has_debris, ice_site_id, combat_classes, pre_warp_ice_guards) =
        with_pve_exploration_reg(&ctx, |reg| {
            let sites = reg.sites_by_system.get(&shat_c4).cloned().unwrap_or_default();
            let deb = sites.iter().any(|s| s.dungeon_name == "Shattered Debris Field");
            let ice = sites.iter().find(|s| s.dungeon_name == "Shattered Ice Field");
            let classes: Vec<u8> = sites
                .iter()
                .filter(|s| s.site_category == "WormholeCombat")
                .filter_map(|s| s.wormhole_class)
                .collect();
            let guards = ice.map(|s| s.active_npc_ball_ids.len()).unwrap_or(99);
            (deb, ice.map(|s| s.site_id.clone()), classes, guards)
        });
    assert!(has_debris && ice_site_id.is_some());
    assert_eq!(combat_classes, vec![3, 4, 5], "C4 shattered spawns C3, C4, C5 sites (own class +/- 1)");
    assert_eq!(pre_warp_ice_guards, 0, "Shattered Ice Field Sleeper wave is delayed until warp-in/mining");

    let (ore_types_found, ice_types_found) = ctx.db.with_snapshot(|snap| {
        let mut ores = std::collections::HashSet::new();
        let mut ices = std::collections::HashSet::new();
        for a in snap.asteroids.values().filter(|a| a.solar_system_id == shat_c4) {
            if SHATTERED_DEBRIS_ORE_TYPES.contains(&a.type_id) {
                ores.insert(a.type_id);
            }
            if SHATTERED_ICE_TYPES.contains(&a.type_id) {
                ices.insert(a.type_id);
            }
        }
        (ores.len(), ices.len())
    });
    assert_eq!(ore_types_found, 15, "Shattered Debris Field must contain all 15 ore types");
    assert_eq!(ice_types_found, 8, "Shattered Ice Field must contain all 8 ice types");

    let normal_wh = 31_001_500_u32;
    let _ = ensure_system_pve_sites_seeded(&ctx, normal_wh).await;
    let normal_wh_ice = ctx.db.with_snapshot(|snap| {
        snap.asteroids
            .values()
            .filter(|a| a.solar_system_id == normal_wh && SHATTERED_ICE_TYPES.contains(&a.type_id))
            .count()
    });
    assert_eq!(normal_wh_ice, 0, "Normal W-space systems must have zero ice");

    // Live warp via CmdWarpToStuff onto Shattered Ice Field -> triggers delayed Sleeper wave & stasis web!
    place_ship(&ctx, &mut sess, char_id, shat_c4, ship_id, 621, DVec3::new(10_000_000.0, 0.0, 0.0), 11_300_000.0).await;
    let bey_shat = ctx.bind_object(&mut sess, "beyonce", shat_c4 as i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bey_shat),
        "CmdWarpToStuff",
        &[PyRep::String("anomaly".into()), PyRep::String(ice_site_id.unwrap()), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("Warp to Shattered Ice Field");
    let shat_handle = ctx.systems.get_or_spawn(shat_c4);
    for _ in 0..45 {
        let _ = shat_handle.step_manual(1.0).await;
    }
    let post_warp_ice_guards = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&shat_c4)
            .and_then(|l| l.iter().find(|s| s.dungeon_name == "Shattered Ice Field"))
            .map(|s| s.active_npc_ball_ids.len())
            .unwrap_or(0)
    });
    assert!(post_warp_ice_guards > 0, "Delayed Sleeper wave must spawn on live Shattered Ice Field warp-in");

    // Issue CmdGotoDirection (speed_fraction = 1.0) and verify Sleeper stasis web reduces target velocity
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bey_shat),
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoDirection on Shattered Ice Field");
    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    let _ = shat_handle.step_manual(1.0).await;
    let webbed_ball = shat_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(
        webbed_ball.speed_fraction < 1.0,
        "Sleeper negative web_speed_factor must reduce target speed_fraction < 1.0, got {}",
        webbed_ball.speed_fraction
    );

    // 3. Drifter Wormhole Systems (J200727, J164710, J174618, J055520, J110145)
    let drifter_systems = [
        (31_000_001_u32, "Sentinel MZ", "J200727", 35_650_u32, 14_u8),
        (31_000_002_u32, "Liberated Barbican", "J164710", 35_651_u32, 15_u8),
        (31_000_003_u32, "Sanctified Vidette", "J174618", 35_652_u32, 16_u8),
        (31_000_004_u32, "Conflux Eyrie", "J055520", 35_653_u32, 17_u8),
        (31_000_006_u32, "Azdaja Redoubt", "J110145", 35_654_u32, 18_u8),
    ];
    for (dsys, expected_sde_name, j_code, expected_k_tid, expected_d_cls) in drifter_systems {
        if let Some(sys_rec) = ctx.sde.solar_systems.get(&dsys) {
            assert_eq!(sys_rec.name, expected_sde_name);
        }
        assert_eq!(system_wormhole_profile_sde(&ctx.sde, dsys).0, expected_d_cls);
        let d_statics = statics_for_system_sde(&ctx.sde, dsys);
        let codes: Vec<&str> = d_statics.iter().map(|s| s.1.as_str()).collect();
        assert_eq!(
            codes,
            vec!["Z647", "D382", "O477", "Y683", "N062", "R474"],
            "Drifter system {j_code} ({expected_sde_name}) must have the 6 C1-C6 statics"
        );
        let _ = ensure_system_signatures_and_anomalies(&ctx, dsys).await;
        let dsigs = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&dsys).cloned().unwrap_or_default());
        assert!(dsigs.iter().all(|s| s.sig_group == "Wormhole"), "Every signature in Drifter system must be a wormhole");
        let kspace_conns: Vec<_> = dsigs.iter().filter(|s| s.destination_system_id.is_some_and(|d| d < 31_000_000)).collect();
        assert!((50..=70).contains(&kspace_conns.len()), "Drifter system {j_code} must have 50-70 K-space connections");
        assert_eq!(kspace_conns[0].wormhole_type_id, Some(expected_k_tid));
        let k_dest = kspace_conns[0].destination_system_id.unwrap();
        let k_beacon_id = format!("K162-{}", kspace_conns[0].signature_id);
        let k_beacon = ctx
            .db
            .with_snapshot(|s| s.cosmic_signatures.get(&k_dest).and_then(|l| l.iter().find(|sig| sig.signature_id == k_beacon_id)).cloned())
            .expect("K-space side Unidentified Wormhole beacon");
        assert_eq!(k_beacon.type_name, "Unidentified Wormhole");
        assert_eq!(k_beacon.signal_strength, 1.0, "K-space Drifter beacon needs no scanning");
    }

    // Verify 80 km deadspace warp-in on K-space side + live Drifter battleship guard AI combat + C2 system effects inside J055520 (Conflux Eyrie = C2 Pulsar)
    let drifter_conflux = 31_000_004_u32; // Conflux Eyrie (C2 Pulsar: +44% shield HP)
    let drifter_k_sig = ctx
        .db
        .with_snapshot(|s| {
            s.cosmic_signatures
                .get(&drifter_conflux)
                .and_then(|l| l.iter().find(|sig| sig.signature_id.starts_with("DRK-")))
                .cloned()
        })
        .expect("DRK signature in Conflux Eyrie");
    let k_side_sys = drifter_k_sig.destination_system_id.unwrap();
    let k_beacon_sig_id = format!("K162-{}", drifter_k_sig.signature_id);
    let k_beacon_rec = ctx
        .db
        .with_snapshot(|s| {
            s.cosmic_signatures
                .get(&k_side_sys)
                .and_then(|l| l.iter().find(|sig| sig.signature_id == k_beacon_sig_id))
                .cloned()
        })
        .expect("K-space beacon record");
    let k_beacon_wh_id = wormhole_item_id_for_signature(k_side_sys, &k_beacon_sig_id);
    let k_beacon_pos = DVec3::from_array(k_beacon_rec.position);

    ctx.advance_secs(15);
    let _ = ensure_system_signatures_and_anomalies(&ctx, k_side_sys).await;
    let drifter_test_ship = 620_000_005_i64;
    let base_bs_shield = ctx.build_ship_combat_state(char_id, drifter_test_ship, 644).max_shield_hp;
    place_ship(&ctx, &mut sess, char_id, k_side_sys, drifter_test_ship, 644, k_beacon_pos + DVec3::new(500_000.0, 0.0, 0.0), 103_600_000.0).await;
    let bey_k = ctx.bind_object(&mut sess, "beyonce", k_side_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_k), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(k_beacon_wh_id), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("Warp to K-space Unidentified Wormhole beacon without scanning");
    let k_handle = ctx.systems.get_or_spawn(k_side_sys);
    for _ in 0..60 {
        let _ = k_handle.step_manual(1.0).await;
    }
    let ship_after_k_warp = k_handle.get_ball_snapshot(drifter_test_ship).await.unwrap().unwrap().1.position;
    assert!(
        (ship_after_k_warp.distance(k_beacon_pos) - 80_000.0).abs() <= 2_500.0,
        "Deadspace warp-in on K-space side of Drifter wormhole must land ~80 km off, got {}",
        ship_after_k_warp.distance(k_beacon_pos)
    );

    // Verify K-space Drifter battleship guards (34495, 34561) engage, web, warp-scramble, and damage the ship!
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_k), "CmdGotoDirection", &[PyRep::Real(-1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("CmdGotoDirection toward K-space Drifter beacon");
    let pre_guard_shield = k_handle.get_combat_state(drifter_test_ship).await.unwrap().unwrap().shield_hp;
    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    let post_guard_cs = k_handle.get_combat_state(drifter_test_ship).await.unwrap().unwrap();
    let post_guard_ball = k_handle.get_ball_snapshot(drifter_test_ship).await.unwrap().unwrap().1;
    assert!(post_guard_cs.shield_hp < pre_guard_shield, "K-space Drifter battleship guards must deal weapon damage");
    assert!(post_guard_cs.is_warp_scrambled(), "K-space Drifter battleship guards must warp-scramble target");
    assert!(post_guard_ball.speed_fraction < 1.0, "K-space Drifter battleship guards must web target");

    // Jump into Conflux Eyrie (31000004) via wormholeMgr.WormholeJump and assert C2 Pulsar (+44% shield HP) system effect!
    let _ = k_handle.set_ball_position(drifter_test_ship, k_beacon_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let jump_drifter_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(k_beacon_wh_id)], &kw, None)
        .await
        .expect("Jump through K-space beacon into Conflux Eyrie");
    assert_eq!(jump_drifter_rep.dict_get("destinationSystemID").and_then(|v| v.as_i64()), Some(drifter_conflux as i64));
    let conflux_handle = ctx.systems.get_or_spawn(drifter_conflux);
    let conflux_cs = conflux_handle.get_combat_state(drifter_test_ship).await.unwrap().expect("combat state in Conflux Eyrie");
    let shield_ratio = conflux_cs.max_shield_hp / base_bs_shield;
    assert!(
        (1.35..=1.55).contains(&shield_ratio),
        "Conflux Eyrie must apply C2 Pulsar (+44% shield HP, not C6 +100%): got ratio {shield_ratio:.3}"
    );

    // Also verify 80 km deadspace warp-in on the Drifter system side (requires probe scan unlike K-space beacon)
    let d_wh_id = wormhole_item_id_for_signature(drifter_conflux, &drifter_k_sig.signature_id);
    let d_wh_pos = DVec3::from_array(drifter_k_sig.position);
    let _ = conflux_handle.set_ball_position(drifter_test_ship, d_wh_pos + DVec3::new(500_000.0, 0.0, 0.0)).await;
    let _ = common::probe_scan(&ctx, &mut sess, d_wh_pos, 0.25 * 149_597_870_700.0).await;
    let bey_d = ctx.bind_object(&mut sess, "beyonce", drifter_conflux as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_d), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(d_wh_id), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("Warp to Drifter-side wormhole");
    for _ in 0..60 {
        let _ = conflux_handle.step_manual(1.0).await;
    }
    let ship_after_d_warp = conflux_handle.get_ball_snapshot(drifter_test_ship).await.unwrap().unwrap().1.position;
    assert!(
        (ship_after_d_warp.distance(d_wh_pos) - 80_000.0).abs() <= 2_500.0,
        "Deadspace warp-in on Drifter side must also land ~80 km off, got {}",
        ship_after_d_warp.distance(d_wh_pos)
    );

    // 4. Thera NPC Stations (31000005)
    let thera_sys = 31_000_005_u32;
    let thera_stations: Vec<i64> = ctx.sde.stations.values().filter(|st| st.solar_system_id == thera_sys).map(|st| st.station_id).collect();
    assert_eq!(thera_stations.len(), 4, "Thera (31000005) has 4 Sisters of EVE NPC stations");
    let _ = ensure_system_signatures_and_anomalies(&ctx, thera_sys).await;
    let thera_sigs = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&thera_sys).cloned().unwrap_or_default());
    let has_kspace_conn = thera_sigs.iter().any(|s| s.destination_system_id.is_some_and(|d| d < 31_000_000));
    let has_wspace_conn = thera_sigs.iter().any(|s| s.destination_system_id.is_some_and(|d| d >= 31_000_000 && d != thera_sys));
    assert!(has_kspace_conn && has_wspace_conn, "Thera spawns connections to both K-space and W-space");

    let thera_st_id = 60_015_148_i64;
    let st_pos = ctx.sde.stations.get(&thera_st_id).map(|s| s.position).unwrap_or(DVec3::new(50_000_000.0, 0.0, 0.0));
    ctx.advance_secs(15);
    place_ship(&ctx, &mut sess, char_id, thera_sys, ship_id, 621, st_pos + DVec3::new(500.0, 0.0, 0.0), 11_300_000.0).await;
    let bey_thera = ctx.bind_object(&mut sess, "beyonce", thera_sys as i64).unwrap();
    let dock_rep = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_thera), "CmdDock", &[PyRep::Integer(thera_st_id)], &kw, None)
        .await
        .expect("Dock at Thera Sisters of EVE NPC station 60015148");
    assert_eq!(dock_rep.dict_get("locationID").and_then(|v| v.as_i64()), Some(thera_st_id));
    assert_eq!(sess.station_id, Some(thera_st_id));

    let st_info = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "GetStation", &[PyRep::Integer(thera_st_id)], &kw, None)
        .await
        .expect("stationSvc.GetStation in Thera");
    assert_eq!(st_info.dict_get("stationID").and_then(|v| v.as_i64()), Some(thera_st_id));
    assert_eq!(st_info.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(thera_sys as i64));

    let clone_state = ctx
        .dispatch_rpc(&mut sess, "jumpCloneSvc", None, "GetStationCloneState", &[], &kw, None)
        .await
        .expect("jumpCloneSvc.GetStationCloneState in Thera");
    assert_ne!(clone_state, PyRep::None, "Thera station exposes clone bay state");

    sess.region_id = ctx.sde.solar_systems.get(&thera_sys).map(|s| s.region_id);
    let station_asks = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetStationAsks", &[], &kw, None)
        .await
        .expect("marketProxy.GetStationAsks in Thera");
    assert_ne!(station_asks, PyRep::None, "Thera station exposes market service");
}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod wormhole_w31_systems_ext {
    use super::*;

// Smoke integration tests for mining SDE, asteroid belt balls, NPC mining convoys,
// agent/scan RPCs, SQLite persistence, and Quasar gateway endpoints.

use std::collections::HashMap;

use eve_destiny::Ballpark;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldDirector, NpcArchetype};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;

use crate::common::login_pilot;

async fn authed_demo_session(ctx: &ServerContext, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID 90_000_001");
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"plan-w31-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        ..Default::default()
    };
    let req_bytes = req.encode_proto();
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req_bytes)],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.send_request");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_plan_c5_mn01_mn02_mining_sde_and_belt_balls() {
    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // MN-01: Veldspar (1230) -> Tritanium (34) only
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);

    // Scordite (1228) -> Tritanium (34) + Pyerite (35)
    let scordite_mats: Vec<u32> = sde
        .type_materials_for(1228)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // Clear Icicle (16262) yields its isotope and fuel materials
    let ice_mats: Vec<u32> = sde
        .type_materials_for(16262)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    for expected in [16272u32, 16273, 16274, 16275] {
        assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
    }

    // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
    let conc_veld_mats = sde.type_materials_for(17470);
    assert_eq!(conc_veld_mats.len(), 1);
    assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

    // MN-02 / C-5: Asteroid belts as balls in Perimeter (30000144, highsec) & LZ-6SU (30000208, nullsec)
    let mut highsec_bp = Ballpark::new(30_000_144);
    let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, sde);
    assert!(!highsec_states.is_empty(), "Perimeter must spawn asteroid belt entities");
    for st in &highsec_states {
        assert!(st.remaining_units > 0);
        let ball = highsec_bp.balls.get(&st.asteroid_id).expect("asteroid ball in ballpark");
        assert!(ball.radius >= 60.0);
    }

    let mut null_bp = Ballpark::new(30_000_208);
    let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, sde);
    assert!(!null_states.is_empty(), "Nullsec 30000208 must spawn asteroid belt entities");
}

#[tokio::test]
async fn test_plan_mn17_mn18_npc_mining_ops_and_mining_missions() {
    let ctx = ServerContext::new();

    // MN-17: LivingWorldDirector seeded convoys include MiningOp fleets
    let director = LivingWorldDirector::new_seeded();
    let convoys = director.list_convoys();
    assert!(!convoys.is_empty(), "Seeded LivingWorldDirector must have convoys");
    assert!(
        convoys.iter().any(|c| c.archetype == NpcArchetype::MiningOp),
        "Seeded convoys must include MiningOp archetype"
    );

    // MN-18: Agent mission RPCs + negative unauthenticated check
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(7101);
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "agentMgr",
            None,
            "GetMyJournalDetails",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unauthenticated agentMgr.GetMyJournalDetails must fail");
    assert!(!format!("{err:?}").is_empty());

    let mut authed = authed_demo_session(&ctx, 7102).await;
    let agents_rep = ctx
        .dispatch_rpc(
            &mut authed,
            "agentMgr",
            None,
            "GetAgents",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated agentMgr.GetAgents");
    assert!(!matches!(agents_rep, PyRep::None));
}

#[tokio::test]
async fn test_plan_so5_exploration_anomalies_probes_dscan_and_dungeons() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // Negative/auth check on scanSvc
    let mut unauth = SessionState::new(7201);
    let err = ctx
        .dispatch_rpc(
            &mut unauth,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unauthenticated scanSvc.GetCombatAnomalyInstances must fail");
    assert!(!format!("{err:?}").is_empty());

    // Authenticated anomaly & signature query
    let mut authed = authed_demo_session(&ctx, 7202).await;
    let anomalies = ctx
        .dispatch_rpc(
            &mut authed,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated scanSvc.GetCombatAnomalyInstances");
    assert_ne!(anomalies, PyRep::None);
    assert!(format!("{anomalies:?}").len() > 4);
}

#[tokio::test]
async fn test_plan_d6_single_source_of_truth_and_sqlite_restart() {
    let db_path = std::env::temp_dir().join(format!("plan_w31_d6_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let empty_kw = HashMap::new();

        // Negative/auth check on charMgr.AddContact
        let mut unauth = SessionState::new(7301);
        let err = ctx
            .dispatch_rpc(
                &mut unauth,
                "charMgr",
                None,
                "AddContact",
                &[
                    PyRep::Integer(90_000_002),
                    PyRep::Real(5.0),
                    PyRep::Integer(0),
                    PyRep::Bool(false),
                ],
                &empty_kw,
                None,
            )
            .await
            .expect_err("unauthenticated charMgr.AddContact must fail");
        assert!(!format!("{err:?}").is_empty());

        // Authenticated contact mutation persisted to SQLite
        let mut authed = authed_demo_session(&ctx, 7302).await;
        ctx.dispatch_rpc(
            &mut authed,
            "charMgr",
            None,
            "AddContact",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Real(5.0),
                PyRep::Integer(0),
                PyRep::Bool(false),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("authenticated charMgr.AddContact");
    }

    // Reopen SQLite and verify contact survived restart
    let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
    let mut authed2 = authed_demo_session(&ctx2, 7303).await;
    let contacts = ctx2
        .dispatch_rpc(
            &mut authed2,
            "charMgr",
            None,
            "GetContacts",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("charMgr.GetContacts after restart");
    assert_ne!(contacts, PyRep::None);
    assert!(
        format!("{contacts:?}").contains("90000002"),
        "Contact 90000002 must persist across SQLite restart: {contacts:?}"
    );
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_plan_t8_t9_atomic_rollback_and_crash_restart_invariants() {
    let db_path = std::env::temp_dir().join(format!("plan_w31_t8_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let (initial_isk, initial_plex) = {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let _sess = authed_demo_session(&ctx, 7401).await;
        let (before_isk, before_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");

        // T-8: Failing transaction via try_with_mut_snapshot must roll back atomically
        let rollback_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.balance.0 += 999_999_999;
                ch.plex_balance += 500;
            }
            Err("simulated_handler_failure".to_string())
        });
        assert_eq!(rollback_res, Err("simulated_handler_failure".to_string()));

        let (after_isk, after_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");
        assert_eq!(before_isk, after_isk, "ISK must roll back on error");
        assert_eq!(before_plex, after_plex, "PLEX must roll back on error");
        (before_isk, before_plex)
    };

    // T-9: Crash/restart recovery preserves committed state with zero duplication/loss
    let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
    let (restarted_isk, restarted_plex) = ctx2
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&90_000_001)
                .map(|c| (c.balance, c.plex_balance))
        })
        .expect("character 90_000_001 exists after restart");
    assert_eq!(initial_isk, restarted_isk);
    assert_eq!(initial_plex, restarted_plex);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_plan_q3_q4_q5_quasar_gateway_domains() {
    let ctx = ServerContext::new();

    // Negative/auth check: unauthenticated Quasar request must return STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(7501);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.character.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    // Authenticated Q-3 (Identity/Entitlements), Q-4 (Character/Progression), Q-5 (Social/Corp)
    let mut authed = authed_demo_session(&ctx, 7502).await;
    let q3_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q3_resp.status_code, STATUS_OK);
    assert_eq!(q3_resp.correlation_uuid, b"plan-w31-closure-01");
    assert_eq!(
        q3_resp.payload.as_ref().unwrap().type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse")
    );

    let q4_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_resp.status_code, STATUS_OK);
    assert_eq!(q4_resp.correlation_uuid, b"plan-w31-closure-01");

    let q5_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.corporationgoal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q5_resp.status_code, STATUS_OK);
    assert_eq!(q5_resp.correlation_uuid, b"plan-w31-closure-01");
}


#[tokio::test]
async fn test_plan_q6_q7_q8_t10_quasar_and_client_oracle() {
    let ctx = ServerContext::new();
    let mut authed = authed_demo_session(&ctx, 7601).await;

    let q6_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve.market.orders.GetOrdersRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q6_resp.status_code, STATUS_OK);
    assert_eq!(q6_resp.correlation_uuid, b"plan-w31-closure-01");

    let mut cdn_req = Vec::new();
    eve_proto::quasar::encode_varint_field(2, 1, &mut cdn_req);
    let q7_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(q7_resp.status_code, STATUS_OK);
    assert_eq!(q7_resp.correlation_uuid, b"plan-w31-closure-01");

    let q8_resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_resp.status_code, STATUS_OK);
    assert_eq!(q8_resp.correlation_uuid, b"plan-w31-closure-01");
}

}
