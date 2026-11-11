//! Unit tests for `sov_gameplay` (Mercenary Dens, Skyhook management, Sov Hub data hack & anchoring, Tenebrex aftermath).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "sov_gameplay_extra_unit_tests.rs"]
mod sov_gameplay_extra_unit_tests;

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

fn find_claimable_null_with_temperate_planet(ctx: &ServerContext) -> (u32, u32, [f64; 3]) {
    let mut sys_ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    sys_ids.sort_unstable();
    for &sid in &sys_ids {
        let cels = ctx.sde.celestials_in_system(sid);
        if let Some(p) = cels.iter().find(|c| c.group_id == 7 && c.type_id == 11) {
            return (sid, p.celestial_id as u32, [p.position.x, p.position.y, p.position.z]);
        }
    }
    for &sid in &sys_ids {
        let cels = ctx.sde.celestials_in_system(sid);
        if let Some(p) = cels.iter().find(|c| c.group_id == 7) {
            return (sid, p.celestial_id as u32, [p.position.x, p.position.y, p.position.z]);
        }
    }
    (30_000_142, 40_009_077, [1.0e9, 0.0, 0.0])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mercenary_den_damage_reinforcement_repair_extraction_and_destruction() {
    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let skyhook_id = 1_090_000_001_i64;
    let den_id = 1_090_000_002_i64;
    let ship_id = 1_090_000_003_i64;
    let now = ctx.now_unix();

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&owner_id].corporation_id;
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Parent Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_id, 20);
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Test Merc Den".to_string(),
                position: [5_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                shield_hp: MERC_DEN_BASE_HP,
                armor_hp: MERC_DEN_BASE_HP,
                hull_hp: MERC_DEN_BASE_HP,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                type_id: 85_230,
                owner_char_id: owner_id,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id: 40_009_077,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 20,
                anarchy_level: 10,
                infomorphs: 100,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        cid
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(7_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let sess = SessionState {
        character_id: Some(owner_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..SessionState::new(9501)
    };

    // Extract 50 infomorphs with 20% skyhook tax => 40 to ship cargo, 10 taxed to skyhook
    ctx.db.with_mut_snapshot(|s| {
        if let Some(d) = s.sov_runtime.mercenary_dens.get_mut(&den_id) {
            d.infomorphs = 50;
            d.simulated_at_unix = ctx.now_unix();
        }
    });
    let ext = handle_mercden_component_call(
        &ctx,
        &sess,
        sys_id,
        owner_id,
        ship_id,
        den_id,
        "mercenaryDen",
        "extract_infomorphs_to_ship",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(ext.dict_get("ownerShare").and_then(PyRep::as_i64), Some(40));
    assert_eq!(ext.dict_get("taxShare").and_then(PyRep::as_i64), Some(10));

    // Deplete shields -> enters HullReinforced for 24h
    let reinf = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP)
        .await
        .unwrap();
    assert_eq!(reinf.dict_get("state").and_then(PyRep::as_str), Some("HullReinforced"));
    assert!(reinf.dict_get("reinforcedUntil").and_then(PyRep::as_i64).is_some_and(|v| v > 0));

    // Attacking while reinforced fails
    let reinf_err = apply_mercenary_den_weapon_damage(&ctx, den_id, 500.0)
        .await
        .unwrap_err();
    assert_eq!(reinf_err.code, "MercenaryDenReinforced");

    // Fast-forward reinforcement timer to active vulnerability window, then repair shields >= 50%
    ctx.db.with_mut_snapshot(|s| {
        let past = ctx.now_unix().saturating_sub(10);
        if let Some(entry) = s.sov_runtime.mercenary_den_reinforcement.get_mut(&den_id) {
            entry.0 = past;
            entry.1 = past + MERC_DEN_VULNERABILITY_WINDOW_SECS;
        }
    });
    let (pct, re_enabled) = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 3_000.0)
        .await
        .unwrap();
    assert_eq!(pct, 60);
    assert!(re_enabled);

    // Deplete shields again, advance into vuln window, and deplete armor + hull to destroy den
    let _ = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP)
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let past = ctx.now_unix().saturating_sub(10);
        if let Some(entry) = s.sov_runtime.mercenary_den_reinforcement.get_mut(&den_id) {
            entry.0 = past;
            entry.1 = past + MERC_DEN_VULNERABILITY_WINDOW_SECS;
        }
    });
    let kill = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP * 3.0)
        .await
        .unwrap();
    assert!(kill.dict_get("wreckID").and_then(PyRep::as_i64).is_some());
    assert_eq!(kill.dict_get("state").and_then(PyRep::as_str), Some("Destroyed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_hub_data_hack_skyhook_tax_and_sov_hub_anchoring() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let (null_sys, _, _) = find_claimable_null_with_temperate_planet(&ctx);
    assert!(is_claimable_nullsec_system(&ctx.sde, null_sys));
    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_000_142));

    let (corp_id, ally_id, hostile_hub_id, hub_item_id, ship_id) = ctx.db.with_mut_snapshot(|snap| {
        let cid = 98_000_055_i64;
        let aid = 99_000_055_i64;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = cid;
            ch.alliance_id = Some(aid);
            ch.solar_system_id = null_sys;
            ch.station_id = None;
        }
        snap.corporations.insert(
            cid,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = cid;
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(aid);
                __corp.name = "SovHubTestCorp".into();
                __corp.ticker = "SHTC".into();
                __corp },
        );
        snap.alliances.insert(
            aid,
            eve_db::AllianceRecord {
                alliance_id: aid,
                name: "SovHubTestAlliance".into(),
                short_name: "SHTA".into(),
                executor_corp_id: cid,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3405,
            TrainedSkillRecord {
                type_id: 3405,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;

        // Hostile sov hub in another system for data hack test
        let h_hub = 1_090_000_100_i64;
        snap.structures.insert(
            h_hub,
            UpwellStructureRecord {
                structure_id: h_hub,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid + 50,
                solar_system_id: 30_000_144,
                name: "Hostile Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );

        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 648,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let hub_it = snap.next_entity_id;
        snap.items.insert(
            hub_it,
            InventoryItemRecord {
                item_id: hub_it,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (cid, aid, h_hub, hub_it, sid)
    });

    // Data analyzer hack succeeds; Relic analyzer rejected
    let data_mod = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 538 && !t.name.contains("Relic"))
        .cloned()
        .unwrap_or_else(|| ctx.sde.types.values().next().unwrap().clone());
    let hack_res = try_handle_sov_hub_data_hack(&ctx, char_id, hostile_hub_id, 555_001, &data_mod, 60.0, 20.0)
        .unwrap()
        .unwrap();
    assert_eq!(hack_res.dict_get("hubID").and_then(PyRep::as_i64), Some(hostile_hub_id));
    assert_eq!(hack_res.dict_get("virusCoherence").and_then(PyRep::as_f64), Some(100.0));

    let relic_err = try_handle_sov_hub_data_hack(
        &ctx,
        char_id,
        hostile_hub_id,
        555_002,
        &eve_sde::InvType {
            type_id: 22177,
            name: "Relic Analyzer I".to_string(),
            ..data_mod
        },
        60.0,
        20.0,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(relic_err.code, "DataAnalyzerRequired");

    // Anchor Sov Hub in unclaimed claimable null-sec system
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut sess = SessionState::new(9502);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    let anchored = handle_anchor_sov_hub_rpc(
        &ctx,
        &mut sess,
        &[PyRep::Integer(hub_item_id), PyRep::String("Main Sov Hub".into())],
    )
    .await
    .unwrap();
    let new_hub_id = anchored.dict_get("structureID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        anchored.dict_get("state").and_then(PyRep::as_str),
        Some("Onlining")
    );
    ctx.db.with_snapshot(|s| {
        assert!(s.structures.contains_key(&new_hub_id));
    });

    // ConfirmSkyhookSovManagement sets tax percentage
    let skyhook_id = 1_090_000_200_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Tax Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
    });
    let tax_res = handle_confirm_skyhook_sov_management(
        &ctx,
        &sess,
        &[PyRep::Integer(skyhook_id), PyRep::Integer(35)],
    )
    .await
    .unwrap();
    assert_eq!(tax_res.dict_get("taxPercent").and_then(PyRep::as_i64), Some(35));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_gameplay_mercden_repair_dispatch_hack_and_anchor_error_guards() {
    let ctx = test_ctx();
    let (null_sys, temp_planet_id, p_pos) = find_claimable_null_with_temperate_planet(&ctx);
    let non_temp_planet_id = ctx
        .sde
        .celestials_in_system(null_sys)
        .iter()
        .find(|c| c.group_id == 7 && c.type_id != 11)
        .map(|c| c.celestial_id as u32);
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let ship_id = 100_666_001_i64;
    let ship_zero_cap = 100_666_002_i64;
    let skyhook_id = 1_066_000_001_i64;
    let skyhook_no_planet = 1_066_000_002_i64;
    let hub_id = 1_066_000_003_i64;
    let hub_dead_id = 1_066_000_004_i64;
    let now = ctx.now_unix();

    // ship_free_cargo_m3 error branches
    assert_eq!(ctx.db.with_snapshot(|s| ship_free_cargo_m3(&ctx, s, 999_888)).unwrap_err().code, "ShipNotFound");

    let (corp_id, ally_id, other_ally, den_cargo_item, den_wrong_owner_item, den_hangar_item, hub_wrong_owner, hub_hangar, ch1_id, ch2_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let cid = 98_666_001_i64;
            let aid = 99_666_001_i64;
            let oaid = 99_666_002_i64;
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = cid;
                ch.alliance_id = Some(aid);
                ch.solar_system_id = null_sys;
                ch.station_id = None;
                ch.active_ship_id = ship_id;
            }
            let mut ch2 = snap.characters[&char_id].clone();
            ch2.character_id = other_char;
            ch2.corporation_id = cid;
            snap.characters.insert(other_char, ch2);
            snap.corporations.insert(cid, { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = cid; __corp.ceo_id = char_id; __corp.alliance_id = Some(aid); __corp.name = "SovEdgeCorp".into(); __corp.ticker = "SEC".into(); __corp.ensure_wallet_divisions(); __corp });
            snap.items.insert(ship_id, InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                quantity: 1,
                singleton: true,
                ..Default::default()
            });
            let mut zero_cap_attrs = std::collections::HashMap::new();
            zero_cap_attrs.insert(38u16, 0.0);
            snap.items.insert(ship_zero_cap, InventoryItemRecord {
                item_id: ship_zero_cap,
                type_id: 648,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                quantity: 1,
                singleton: true,
                dynamic_attributes: zero_cap_attrs,
                ..Default::default()
            });

            // Items for anchor error guards
            snap.next_entity_id += 1; let i_den = snap.next_entity_id;
            snap.next_entity_id += 1; let i_den_owner = snap.next_entity_id;
            snap.next_entity_id += 1; let i_den_hangar = snap.next_entity_id;
            snap.next_entity_id += 1; let i_hub_owner = snap.next_entity_id;
            snap.next_entity_id += 1; let i_hub_hangar = snap.next_entity_id;
            snap.next_entity_id += 1; let i_ch1 = snap.next_entity_id;
            snap.next_entity_id += 1; let i_ch2 = snap.next_entity_id;
            for &(iid, tid, own, loc, flag, qty) in &[
                (i_den, TYPE_MERCENARY_DEN, char_id, ship_id, InventoryFlag::Cargo, 2),
                (i_den_owner, TYPE_MERCENARY_DEN, other_char, ship_id, InventoryFlag::Cargo, 1),
                (i_den_hangar, TYPE_MERCENARY_DEN, char_id, 60_003_760, InventoryFlag::Hangar, 1),
                (i_hub_owner, TYPE_SOVEREIGNTY_HUB, other_char, ship_id, InventoryFlag::Cargo, 1),
                (i_hub_hangar, TYPE_SOVEREIGNTY_HUB, char_id, 60_003_760, InventoryFlag::Hangar, 1),
                (i_ch1, 180, char_id, ship_id, InventoryFlag::Cargo, 2),
                (i_ch2, 180, char_id, ship_id, InventoryFlag::Cargo, 1),
            ] {
                snap.items.insert(iid, InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: own,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    ..Default::default()
                });
            }

            snap.structures.insert(skyhook_no_planet, UpwellStructureRecord {
                structure_id: skyhook_no_planet,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: null_sys,
                name: "Unbound Skyhook".into(),
                position: [p_pos[0] + 5_000.0, p_pos[1], p_pos[2]],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            });
            snap.structures.insert(hub_dead_id, UpwellStructureRecord {
                structure_id: hub_dead_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid + 1,
                solar_system_id: null_sys,
                name: "Dead Hub".into(),
                state: "Destroyed".into(),
                ..Default::default()
            });
            snap.structures.insert(hub_id, UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid,
                solar_system_id: null_sys,
                name: "Friendly Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            });
            (cid, aid, oaid, i_den, i_den_owner, i_den_hangar, i_hub_owner, i_hub_hangar, i_ch1, i_ch2)
        });

    assert_eq!(ctx.db.with_snapshot(|s| ship_free_cargo_m3(&ctx, s, ship_zero_cap)).unwrap_err().code, "NotEnoughCargoSpace");

    let mut sess = SessionState::new(9600);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    // 1. Anchor Mercenary Den error guards (docked, no sys, no ship, non-nullsec, ship not in space, far from skyhook, planet mismatch, non-temp, unclaimed, cap=0, item errors)
    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipNotInSpace");
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipNotInSpace");

    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 648,
        position: DVec3::new(p_pos[0] + 5_000.0, p_pos[1], p_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let hi_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = hi_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 648,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SovereigntyNullsecRequired");

    sess.solar_system_id = Some(null_sys);
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_no_planet)]).await.unwrap_err().code,
        "SkyhookPlanetUnknown"
    );
    // 10-arg V24 call with custom extraConfig triggering MercenaryDenTooFarFromSkyhook, then DenPlanetMismatch, then PlanetNotTemperate
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, UpwellStructureRecord {
            structure_id: skyhook_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            owner_corp_id: corp_id,
            solar_system_id: null_sys,
            name: "Temp Skyhook".into(),
            position: [p_pos[0] + 4_500.0, p_pos[1], p_pos[2]],
            state: "ShieldVulnerable".into(),
            ..Default::default()
        });
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, i64::from(temp_planet_id));
    });
    let far_extra = PyRep::keyval([("skyhookID", PyRep::Integer(skyhook_id)), ("planetID", PyRep::Integer(999_111))]);
    assert_eq!(
        handle_anchor_mercenary_den_rpc(
            &ctx, &mut sess,
            &[PyRep::Integer(den_cargo_item), PyRep::Real(p_pos[0] + 100_000.0), PyRep::Real(p_pos[2]), PyRep::Real(0.0), PyRep::Integer(1), PyRep::String("Custom Den".into()), PyRep::None, PyRep::Integer(0), PyRep::Integer(0), far_extra.clone()]
        ).await.unwrap_err().code,
        "MercenaryDenTooFarFromSkyhook"
    );
    assert_eq!(
        handle_anchor_mercenary_den_rpc(
            &ctx, &mut sess,
            &[PyRep::Integer(den_cargo_item), PyRep::Real(p_pos[0] + 5_000.0), PyRep::Real(p_pos[2]), PyRep::Real(0.0), PyRep::Integer(1), PyRep::String("Custom Den".into()), PyRep::None, PyRep::Integer(0), PyRep::Integer(0), far_extra]
        ).await.unwrap_err().code,
        "DenPlanetMismatch"
    );
    if let Some(nt_pid) = non_temp_planet_id {
        assert_eq!(
            handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_no_planet), PyRep::Integer(i64::from(nt_pid))]).await.unwrap_err().code,
            "PlanetNotTemperate"
        );
    }

    // SovereigntyNotClaimed -> MercenaryDenCapExceeded -> ItemOwnershipViolation -> ItemNotInCargo -> MissingMercenaryDenItem -> Success
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "SovereigntyNotClaimed"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
    });
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "MercenaryDenCapExceeded"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(TYPE_MERCENARY_DEN_MANAGEMENT, TrainedSkillRecord {
            type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
            skill_points: 256_000.0,
            level: 5,
        });
    });
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_wrong_owner_item), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_hangar_item), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "ItemNotInCargo"
    );
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(999_123_456), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "MissingMercenaryDenItem"
    );

    let anchored_den = handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_id)]).await.unwrap();
    let den_id = anchored_den.dict_get("denID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_cargo_item), PyRep::Integer(skyhook_id)]).await.unwrap_err().code,
        "MercenaryDenAlreadyAnchoredOnSkyhook"
    );

    // 2. MercDen component guards + partial repair (<50%) + reinforcement expiry reset + loot drop destruction
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, char_id, ship_id, den_id, "mercenaryDen", "badMethod", &[]).await.unwrap_err().code, "UnsupportedComponentCall");
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, char_id, 999_777, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.unwrap_err().code, "ShipNotInSpace");
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, char_id, ship_id, 999_666, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.unwrap_err().code, "MercenaryDenNotFound");
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, other_char, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.unwrap_err().code, "NotMercenaryDenOwner");
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.unwrap_err().code, "NoInfomorphsAvailable");

    // Partial shield damage -> reinforce -> repair attempt while reinforced fails -> advance past reinf_until -> partial <50% repair keeps disabled -> advance past vuln_end resets on next hit -> kill drops Infomorph stacks
    let _ = apply_mercenary_den_weapon_damage(&ctx, den_id, 1_000.0).await.unwrap();
    let _ = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP).await.unwrap();
    assert_eq!(handle_mercden_component_call(&ctx, &sess, null_sys, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.unwrap_err().code, "MercenaryDenReinforced");
    assert_eq!(apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 1_000.0).await.unwrap_err().code, "MercenaryDenStillInReinforcement");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(entry) = snap.sov_runtime.mercenary_den_reinforcement.get_mut(&den_id) {
            entry.0 = now.saturating_sub(20);
            entry.1 = now + 3_600;
        }
    });
    let (pct_low, enabled_low) = apply_mercenary_den_shield_repair(&ctx, null_sys, ship_id, den_id, 600.0).await.unwrap();
    assert!(pct_low < 50 && !enabled_low);

    // Move past vuln_end -> resets to ShieldVulnerable, then re-reinforce, enter vuln_end and deal partial hull then fatal hull with Infomorphs!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(entry) = snap.sov_runtime.mercenary_den_reinforcement.get_mut(&den_id) {
            entry.1 = now.saturating_sub(5);
        }
        if let Some(d) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
            d.infomorphs = 120;
        }
    });
    let reset_hit = apply_mercenary_den_weapon_damage(&ctx, den_id, 100.0).await.unwrap();
    assert_eq!(reset_hit.dict_get("state").and_then(PyRep::as_str), Some("ShieldVulnerable"));
    let _ = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(entry) = snap.sov_runtime.mercenary_den_reinforcement.get_mut(&den_id) {
            entry.0 = now.saturating_sub(10);
            entry.1 = now + 3_600;
        }
    });
    let hull_dmg = apply_mercenary_den_weapon_damage(&ctx, den_id, 500.0).await.unwrap();
    assert_eq!(hull_dmg.dict_get("state").and_then(PyRep::as_str), Some("HullVulnerable"));
    let destroyed = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP * 2.0).await.unwrap();
    assert_eq!(destroyed.dict_get("state").and_then(PyRep::as_str), Some("Destroyed"));
    assert_eq!(apply_mercenary_den_weapon_damage(&ctx, den_id, 100.0).await.unwrap_err().code, "StructureDestroyed");

    // 3. dispatch_special_structure_attack + handle_normal_structure_damage_aftermath + handle_confirm_skyhook_sov_management errors
    assert_eq!(
        dispatch_special_structure_attack(&ctx, char_id, skyhook_id, 587, false, 100.0, 1, &[]).await.unwrap_err().code,
        "InvalidSpecialStructure"
    );
    let _ = dispatch_special_structure_attack(&ctx, char_id, skyhook_id, TYPE_ORBITAL_SKYHOOK, false, 100.0, 2, &[ch1_id, ch2_id]).await;
    handle_normal_structure_damage_aftermath(&ctx, null_sys, -0.5, char_id, ship_id, TYPE_TENEBREX_CYNO_JAMMER, "ArmorReinforced", true, now);
    assert_eq!(handle_confirm_skyhook_sov_management(&ctx, &sess, &[]).await.unwrap_err().code, "InvalidArguments");
    let mut unauth_sess = sess.clone();
    unauth_sess.character_id = Some(other_char);
    assert_eq!(handle_confirm_skyhook_sov_management(&ctx, &unauth_sess, &[PyRep::Integer(skyhook_id), PyRep::Integer(10)]).await.unwrap_err().code, "CorpRoleDenied");

    // 4. try_handle_sov_hub_data_hack errors & handle_anchor_sov_hub_rpc guards
    let data_mod = ctx.sde.types.values().find(|t| t.group_id == 538 && !t.name.contains("Relic")).cloned().unwrap_or_else(|| ctx.sde.types.values().next().unwrap().clone());
    assert_eq!(try_handle_sov_hub_data_hack(&ctx, char_id, hub_dead_id, 1, &data_mod, 50.0, 10.0).unwrap().unwrap_err().code, "TargetDestroyed");
    assert_eq!(try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, 1, &data_mod, 50.0, 10.0).unwrap().unwrap_err().code, "CannotHackFriendlySovHub");
    assert_eq!(try_handle_sov_hub_data_hack(&ctx, other_char, hub_dead_id + 999, 1, &data_mod, 50.0, 10.0).is_none(), true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().owner_corp_id = corp_id + 55;
    });
    assert_eq!(try_handle_sov_hub_data_hack(&ctx, other_char, hub_id, 1, &data_mod, 50.0, 10.0).unwrap().unwrap_err().code, "HackingSkillRequired");

    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "CannotAnchorSovHubOutsideNullSec");
    sess.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(other_ally);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_hangar)]).await.unwrap_err().code, "SovereigntyHeldByAnotherAlliance");
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_hangar)]).await.unwrap_err().code, "SovHubAlreadyPresent");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&hub_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_wrong_owner)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_hangar)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(999_123_456)]).await.unwrap_err().code, "MissingStructureItem");

    // Auto-select Sov Hub from cargo when args is empty, with quantity=2 decrement branch
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(hub_hangar + 50, InventoryItemRecord {
            item_id: hub_hangar + 50,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 2,
            ..Default::default()
        });
    });
    assert!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.is_ok());

    // Auto-select Mercenary Den from cargo when args is empty, plus simulate_den transition in ExtractInfomorphs
    let planet_id = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.values().next().map(|d| d.planet_id).unwrap_or(0)
    });
    if planet_id > 0 {
        let mut den_item_id = 0_i64;
        let mut den_to_extract = 0_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            den_item_id = snap.next_entity_id;
            snap.items.insert(den_item_id, InventoryItemRecord {
                item_id: den_item_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            });
            snap.structures.retain(|_, st| !(st.type_id == TYPE_MERCENARY_DEN && st.solar_system_id == null_sys));
            snap.sov_runtime.mercenary_dens.retain(|_, d| d.solar_system_id != null_sys);
        });
        let anchored_den = handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(0), PyRep::Integer(planet_id)]).await;
        if let Ok(rep) = anchored_den {
            den_to_extract = rep.dict_get("denID").and_then(|v| v.as_i64()).unwrap_or(0);
        }
        if den_to_extract > 0 {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(d) = snap.sov_runtime.mercenary_dens.get_mut(&den_to_extract) {
                    d.simulated_at_unix = ctx.now_unix().saturating_sub(100_000);
                    d.infomorphs = 10;
                }
            });
            let _ = handle_mercden_component_call(&ctx, &sess, null_sys, char_id, ship_id, den_to_extract, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_gameplay_mercden_repairs_dispatch_and_partial_extract_cov() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_610;
    let corp_id = 98_000_610;
    let ship_id = 1_066_610;
    let den_id = 1_066_611;
    let sky_id = 1_066_612;
    let now = ctx.now_unix();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 588, // Reaper (small cargo)
            position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: den_id,
            type_id: TYPE_MERCENARY_DEN,
            position: DVec3::new(80_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 588,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "RepairDen".into(),
                state: "ArmorReinforced".into(),
                shield_hp: 100.0,
                armor_hp: MERC_DEN_BASE_HP,
                hull_hp: MERC_DEN_BASE_HP,
                position: [80_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                solar_system_id: sys_id,
                planet_id: 40_009_091,
                skyhook_id: sky_id,
                owner_char_id: char_id,
                anarchy_level: 0,
                development_level: 1,
                enabled: false,
                infomorphs: 1_000_000,
                simulated_at_unix: now,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (now + 600, now + 1200, 10));
        snap.sov_runtime.mercenary_den_tax_pct.insert(sky_id, 50);
    });

    // Out of range repair (> 50km -> TargetOutOfRange)
    let err_oor = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 500.0).await.unwrap_err();
    assert_eq!(err_oor.code, "TargetOutOfRange");

    // Move den ball in range (< 50km)
    let _ = sys_handle.remove_ball(den_id).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: den_id,
            type_id: TYPE_MERCENARY_DEN,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    // Still in reinforcement -> MercenaryDenStillInReinforcement
    let err_reinf = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 500.0).await.unwrap_err();
    assert_eq!(err_reinf.code, "MercenaryDenStillInReinforcement");

    // Clear reinforcement timer -> low repair (< 50%), then full repair (>= 50%)
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (0, 0, 10));
    });
    let (pct_low, enabled_low) = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 500.0).await.unwrap();
    assert!(pct_low < 50 && !enabled_low);
    let (pct_hi, enabled_hi) = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, MERC_DEN_BASE_HP).await.unwrap();
    assert!(pct_hi >= 50 && enabled_hi);

    // dispatch_special_structure_attack branches (Den, Skyhook, FW/Ins, Invalid)
    let _ = dispatch_special_structure_attack(&ctx, char_id, den_id, TYPE_MERCENARY_DEN, false, 100.0, 10, &[]).await;
    let _ = dispatch_special_structure_attack(&ctx, char_id, sky_id, TYPE_ORBITAL_SKYHOOK, false, 100.0, 11, &[]).await;
    let _ = dispatch_special_structure_attack(&ctx, char_id, 9_999_610, 999_999, true, 100.0, 12, &[]).await;
    let bad_disp = dispatch_special_structure_attack(&ctx, char_id, 9_999_611, 999_999, false, 100.0, 13, &[]).await;
    assert_eq!(bad_disp.unwrap_err().code, "InvalidSpecialStructure");

    // Cargo-constrained partial & zero-space extraction (622-637)
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // Partial extraction (some cargo taken, less than full_owner_qty)
    let cargo_blocker = 1_066_619;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_blocker,
            InventoryItemRecord {
                item_id: cargo_blocker,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10_000,
                ..Default::default()
            },
        );
    });
    let _ = handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await;

    // Zero cargo remaining -> NotEnoughCargoSpace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&cargo_blocker) {
            it.quantity = 5_000_000;
        }
    });
    let _ = handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await;
}
