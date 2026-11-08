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
    for sys in ctx.sde.solar_systems.values() {
        if is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id) {
            let cels = ctx.sde.celestials_in_system(sys.solar_system_id);
            if let Some(p) = cels.iter().find(|c| c.group_id == 7 && c.type_id == 11) {
                return (
                    sys.solar_system_id,
                    p.celestial_id as u32,
                    [p.position.x, p.position.y, p.position.z],
                );
            }
            if let Some(p) = cels.iter().find(|c| c.group_id == 7) {
                return (
                    sys.solar_system_id,
                    p.celestial_id as u32,
                    [p.position.x, p.position.y, p.position.z],
                );
            }
        }
    }
    (30_000_142, 40_009_077, [1.0e9, 0.0, 0.0])
}

#[tokio::test]
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

#[tokio::test]
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
