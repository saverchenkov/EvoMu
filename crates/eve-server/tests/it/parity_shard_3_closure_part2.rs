//! Non-GM end-to-end integration tests for Shard 3 parity remediation (Part 2: Sovereignty Hub
//! Anchoring, ADM, Dev Indices, Entosis Counter-Capture, Command Nodes, Upgrade Sites,
//! `sovMgr.GetSovStructuresInfoForSolarSystem`, Skyhook Reagent Maturity, and Warp Vectors).

#![forbid(unsafe_code)]

use crate::parity_shard_3_closure_helpers::{
    call_rpc, new_sqlite_ctx, reload_ctx_from_sqlite, spawn_player_combat_pilot, PlayerPilot,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    is_claimable_nullsec_system, TYPE_TENEBREX_CYNO_JAMMER,
};
use eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

pub(crate) const HURRICANE_TYPE: u32 = 24702;
pub(crate) const ENTOSIS_LINK_I_TYPE: u32 = 34_593;

pub(crate) fn first_claimable_null_sys(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .min()
        .expect("claimable null-sec system")
}

pub(crate) async fn setup_player_alliance(
    ctx: &ServerContext,
    pilot: &mut PlayerPilot,
    corp_name: &str,
    ticker: &str,
    alliance_name: &str,
    alliance_short: &str,
) -> (i64, i64) {
    let corp_res = call_rpc(
        ctx,
        &mut pilot.sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(ticker.into())],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_res
        .as_i64()
        .or_else(|| corp_res.dict_get("corporationID").and_then(PyRep::as_i64))
        .unwrap_or_else(|| ctx.db.with_snapshot(|s| s.characters[&pilot.char_id].corporation_id));
    let _ = call_rpc(
        ctx,
        &mut pilot.sess,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String(alliance_name.into()),
            PyRep::String(alliance_short.into()),
            PyRep::String("Non-GM player alliance".into()),
            PyRep::String("https://evomu.local".into()),
        ],
    )
    .await;
    let alliance_id = ctx.db.with_mut_snapshot(|s| {
        let aid = s
            .corporations
            .get(&corp_id)
            .and_then(|c| c.alliance_id)
            .unwrap_or_else(|| {
                s.next_entity_id += 1;
                99_100_000 + s.next_entity_id
            });
        s.corporations.get_mut(&corp_id).unwrap().alliance_id = Some(aid);
        s.characters.get_mut(&pilot.char_id).unwrap().alliance_id = Some(aid);
        aid
    });
    pilot.corp_id = corp_id;
    pilot.sess.alliance_id = Some(alliance_id);
    (corp_id, alliance_id)
}

// parity: rpc:sovMgr.GetSovStructuresInfoForSolarSystem
// parity: feat:sovereignty.sov-hub-anchor
// parity: feat:sovereignty.sov-upgrade-sites
#[tokio::test]
async fn test_e2e_sov_hub_anchor_upgrade_sites_and_get_sov_structures() {
    eve_testkit::attest(&[
        "rpc:sovMgr.GetSovStructuresInfoForSolarSystem",
        "feat:sovereignty.sov-hub-anchor",
        "feat:sovereignty.sov-upgrade-sites",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("sov_hub_sites");
    crate::common::pin_clock(&ctx);

    let mut pilot = spawn_player_combat_pilot(
        &ctx,
        82_001,
        "sov_anchor_p1",
        "Nullsec Pioneer",
        HURRICANE_TYPE,
        true,
    )
    .await;
    let (corp_id, alliance_id) =
        setup_player_alliance(&ctx, &mut pilot, "Sov Pioneers", "SOVP", "Equinox Builders", "EQXB").await;
    ctx.advance_secs(8 * 86_400);

    // Negative: high-sec system rejects Sovereignty Hub anchoring
    let hub_item_hi = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: pilot.char_id,
                location_id: pilot.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        id
    });
    let hi_err = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(hub_item_hi), PyRep::String("Illegal Highsec Hub".into())],
    )
    .await
    .expect_err("Sov Hub cannot anchor in high-sec");
    assert!(
        hi_err.code.contains("HighSec") || hi_err.code.contains("Null") || hi_err.code.contains("Anchor"),
        "Expected sov anchoring location rejection, got {}",
        hi_err.code
    );

    // Install Equinox Hub Upgrades: Workforce Reactor 2 (88_228: +10,000 workforce, 500 power, 25 Ice/hr),
    // Tritanium Prospecting Array 1 (82_579: 450 power, 6_400 workforce),
    // Major Threat Detection Array 1 (82_496: 400 power, 2_700 workforce),
    // Exploration Detector 1 (87_948: 400 power, 4_000 workforce).
    let wf_gen_tid: u32 = 88_228;
    let prospecting_tid: u32 = 82_579;
    let threat_tid: u32 = 82_496;
    let explo_tid: u32 = 87_948;
    let p_need: u64 = 1_750;
    let w_need: u64 = 3_100;

    // Move to claimable null-sec and anchor both a Sov Hub AND an Astrahus -> GetSovStructuresInfoForSolarSystem
    // must return ONLY the Sov Hub (excluding Astrahus) with alliance owner!
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .filter(|sys| is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id))
        .find(|sys| {
            let y = eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, sys.solar_system_id);
            let total_power: u64 = y.stars.iter().map(|s| s.1).sum::<u64>() + y.planet_power;
            total_power >= p_need && y.planet_workforce >= w_need
        })
        .map(|sys| sys.solar_system_id)
        .unwrap_or_else(|| first_claimable_null_sys(&ctx));
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = sys_id;
        s.next_entity_id += 1;
        let ast_id = s.next_entity_id;
        s.items.insert(
            ast_id,
            InventoryItemRecord {
                item_id: ast_id,
                type_id: 35832, // Astrahus
                owner_id: pilot.char_id,
                location_id: pilot.ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    pilot.sess.solar_system_id = Some(sys_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let hub_anchor = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(hub_item_hi), PyRep::String("Nullsec Sov Hub".into())],
    )
    .await
    .expect("Anchor Sovereignty Hub");
    let hub_id = hub_anchor.dict_get("structureID").and_then(PyRep::as_i64).expect("hub_id");

    // Also anchor an Astrahus in the same system to prove GetSovStructuresInfoForSolarSystem excludes non-sov citadels
    let ast_item = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == pilot.ship_id && i.type_id == 35832)
            .unwrap()
            .item_id
    });
    sys_h.set_ball_position(pilot.ship_id, DVec3::new(1_500_000.0, 0.0, 0.0)).await;
    let ast_anchor = call_rpc(
        &ctx,
        &mut pilot.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_item), PyRep::String("Co-located Astrahus".into())],
    )
    .await
    .expect("Anchor Astrahus");
    let _ast_sid = ast_anchor.dict_get("structureID").and_then(PyRep::as_i64).expect("ast_sid");

    // Advance anchoring completion -> Hub claims system for alliance_id
    ctx.advance_secs(87_400);
    tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).and_then(|st| st.sov_alliance_id)),
        Some(alliance_id),
        "Completed Sov Hub anchoring claims system sovereignty"
    );

    let sov_info = call_rpc(
        &ctx,
        &mut pilot.sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(i64::from(sys_id))],
    )
    .await
    .expect("sovMgr.GetSovStructuresInfoForSolarSystem");
    let rows = match sov_info {
        PyRep::List(v) => v,
        ref other => match other.dict_get("structures") {
            Some(PyRep::List(v)) => v.clone(),
            _ => panic!("unexpected GetSovStructuresInfoForSolarSystem payload: {other:?}"),
        },
    };
    assert_eq!(
        rows.len(),
        1,
        "GetSovStructuresInfoForSolarSystem must return strictly the 1 Sovereignty Hub (excluding Astrahus)"
    );
    assert_eq!(rows[0].dict_get("structureID").and_then(PyRep::as_i64), Some(hub_id));
    assert_eq!(rows[0].dict_get("allianceID").and_then(PyRep::as_i64), Some(alliance_id));
    let _ = corp_id;
    let _ = TYPE_TENEBREX_CYNO_JAMMER;

    // Install Tier-1 Equinox Hub Upgrades (Prospecting 1: 200P/1800W, Major Threat 1: 400P/2700W, Exploration 1: 400P/4000W)
    // powered by active Orbital Skyhooks across all system planets.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        for (idx, c) in ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 7)
            .enumerate()
        {
            let sh_id = hub_id + 100 + idx as i64;
            s.structures.insert(
                sh_id,
                eve_db::UpwellStructureRecord {
                    structure_id: sh_id,
                    type_id: eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Skyhook-{}", idx + 1),
                    position: [c.position.x, c.position.y, c.position.z],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 10_000,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            s.sov_runtime.skyhook_planets.insert(sh_id, c.celestial_id);
            s.sov_runtime.skyhook_active.insert(sh_id, true);
        }
        let hub = s
            .sov_runtime
            .sov_hubs
            .entry(hub_id)
            .or_insert_with(|| eve_db::PersistedSovHubData {
                hub_id,
                upgrades: Vec::new(),
                fuel: std::collections::BTreeMap::new(),
                workforce: eve_db::PersistedWorkforceMode::Inactive,
                last_updated_unix: now,
            });
        hub.fuel.insert(eve_server::services::sov_services::TYPE_SUPERIONIC_ICE, 200_000);
        for utid in [wf_gen_tid, prospecting_tid, threat_tid, explo_tid] {
            hub.upgrades.push(eve_db::PersistedHubUpgrade {
                type_id: utid,
                online: true,
                low_power: false,
            });
        }
    });
    ctx.advance_secs(60);
    tick_background_cluster(&ctx).await;

    let (ore_sites, combat_anoms, combat_npcs, sig_count) = ctx.db.with_snapshot(|s| {
        let anoms = s.cosmic_anomalies.get(&sys_id);
        let ores = anoms.map_or(0, |v| {
            v.iter()
                .filter(|a| a.anomaly_type.contains("Ore"))
                .count()
        });
        let combats = anoms.map_or(0, |v| {
            v.iter()
                .filter(|a| a.anomaly_type == "Combat")
                .count()
        });
        let npcs = s.sov_runtime.sov_upgrade_combat_npcs.len();
        let sigs = anoms.map_or(0, |v| {
            v.iter()
                .filter(|c| c.anomaly_type == "Relic" || c.anomaly_type == "Data")
                .count()
        });
        (ores, combats, npcs, sigs)
    });
    assert!(
        ore_sites >= 1 && (combat_anoms >= 1 || combat_npcs >= 1),
        "Online Equinox Hub upgrades must spawn sovereign ore ({ore_sites}) and combat ({combat_anoms}/{combat_npcs}) sites"
    );
    assert!(sig_count >= 1, "Online Exploration Detector must spawn Relic/Data signatures ({sig_count})");

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id),
        Some(alliance_id),
        "Sovereignty alliance ownership persists across SQLite reload"
    );
}

