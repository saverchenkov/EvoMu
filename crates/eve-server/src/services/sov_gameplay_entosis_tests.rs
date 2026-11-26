//! Unit tests for `sov_gameplay_entosis` (ADM vulnerability windows, Entosis precheck & capture tick, Tenebrex, Flex upgrades).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
use crate::services::tactical_modules::tactical_test_helpers::{
    clear_active_entosis_links, insert_active_entosis_link, insert_foreign_active_entosis_link,
    remove_active_entosis_link, set_active_entosis_link_expires_unix,
};
use crate::services::tactical_modules::ActiveEntosisLinkState;
use eve_db::durable_domain_types::PersistedHubUpgrade;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

#[cfg(test)]
#[path = "sov_gameplay_entosis_capture_edge_tests.rs"]
mod sov_gameplay_entosis_capture_edge_tests;

pub(super) fn test_ctx() -> ServerContext {
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

#[test]
fn adm_vulnerability_windows_midnight_wrap_and_flex_upgrade_gates() {
    assert_eq!(compute_adm_vulnerability_seconds(1.0), 18 * 3_600);
    assert_eq!(compute_adm_vulnerability_seconds(6.0), 3 * 3_600);
    assert_eq!(compute_adm_vulnerability_seconds(10.0), 3 * 3_600);
    assert_eq!(compute_adm_vulnerability_seconds(0.5), 18 * 3_600);

    assert_eq!(strategic_level_for_days(0), 0);
    assert_eq!(strategic_level_for_days(7), 1);
    assert_eq!(strategic_level_for_days(21), 2);
    assert_eq!(strategic_level_for_days(35), 3);
    assert_eq!(strategic_level_for_days(65), 4);
    assert_eq!(strategic_level_for_days(100), 5);

    assert_eq!(
        required_upgrade_for_flex_structure(TYPE_ANSIBLEX_JUMP_BRIDGE),
        Some(UPGRADE_ADVANCED_LOGISTICS_NETWORK)
    );
    assert_eq!(
        required_upgrade_for_flex_structure(TYPE_TENEBREX_CYNO_JAMMER),
        Some(UPGRADE_CYNOSURAL_SUPPRESSION)
    );
    assert_eq!(required_upgrade_for_flex_structure(999_999), None);

    let ctx = test_ctx();
    let hub_id = 1_080_000_001_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(null_sys).or_default();
        st.solar_system_id = null_sys;
        st.military_index = 5;
        st.industrial_index = 5;
        st.strategic_index = 5; // ADM = 6.0 => 3h (10_800s) window
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Flex Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );

        // Center at 23:00 UTC with 3h window => [21:30, 00:30] wrapping midnight!
        let (sv, ev) = apply_adm_scaled_vulnerability_window_in_snap(snap, hub_id, 23).unwrap();
        assert_eq!(sv, 21 * 3_600 + 1_800);
        assert_eq!(ev, 1_800);
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 22 * 3_600));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 900));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 12 * 3_600));
    });

    // High-sec rejects Flex structures
    let hs_err = require_flex_sov_upgrade_online(&ctx, 30_000_142, TYPE_ANSIBLEX_JUMP_BRIDGE).unwrap_err();
    assert_eq!(hs_err.code, "CannotAnchorFlexOutsideSovNull");

    // Null-sec without online upgrade rejects Ansiblex
    let upg_err = require_flex_sov_upgrade_online(&ctx, null_sys, TYPE_ANSIBLEX_JUMP_BRIDGE).unwrap_err();
    assert_eq!(upg_err.code, "RequiredSovUpgradeOffline");

    // Install online upgrade -> passes
    ctx.db.with_mut_snapshot(|snap| {
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.upgrades.push(PersistedHubUpgrade {
            type_id: UPGRADE_ADVANCED_LOGISTICS_NETWORK,
            online: true,
            low_power: false,
        });
    });
    assert!(require_flex_sov_upgrade_online(&ctx, null_sys, TYPE_ANSIBLEX_JUMP_BRIDGE).is_ok());

    // Hacked hub blocks Flex structure anchoring
    crate::services::sov_services::record_hub_hack(&ctx, null_sys, hub_id, 600);
    let hack_err = require_flex_sov_upgrade_online(&ctx, null_sys, TYPE_ANSIBLEX_JUMP_BRIDGE).unwrap_err();
    assert_eq!(hack_err.code, "SovHubHacked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entosis_precheck_capture_progress_strontium_and_reinforcement() {
    let ctx = test_ctx();
    let att_char = 90_000_001_i64;
    let def_char = 90_000_002_i64;
    let att_ally = 99_000_101_i64;
    let def_ally = 99_000_102_i64;
    let hub_id = 1_080_000_020_i64;
    let att_ship = 1_080_000_021_i64;
    let def_ship = 1_080_000_022_i64;
    let mod_id = 1_080_000_023_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&att_char) {
            ch.alliance_id = Some(att_ally);
        }
        let mut def = snap.characters[&att_char].clone();
        def.character_id = def_char;
        def.user_id = 2;
        def.alliance_id = Some(def_ally);
        snap.characters.insert(def_char, def);

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(def_ally);
        st.military_index = 0;
        st.industrial_index = 0;
        st.strategic_index = 0; // ADM 1.0 => 600s hub capture

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_099,
                solar_system_id: sys_id,
                name: "Contested Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        // Add Strontium Clathrates (16275) in attacker ship cargo for cycle renewal
        snap.next_entity_id += 1;
        let stront_id = snap.next_entity_id;
        snap.items.insert(
            stront_id,
            InventoryItemRecord {
                item_id: stront_id,
                type_id: 16_275,
                owner_id: att_char,
                location_id: att_ship,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos) in [
        (att_ship, DVec3::new(30_000.0, 0.0, 0.0)),
        (def_ship, DVec3::new(40_000.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Defender cannot precheck uncontested friendly hub
    let friendly_err = precheck_entosis_activation(&ctx, hub_id, def_ship, def_char)
        .await
        .unwrap_err();
    assert_eq!(friendly_err.code, "CannotEntosisFriendlyStructure");

    // Attacker precheck succeeds
    assert!(precheck_entosis_activation(&ctx, hub_id, att_ship, att_char)
        .await
        .unwrap());

    // Register attacker link & capture target
    insert_foreign_active_entosis_link(
        &ctx,
        701,
        ActiveEntosisLinkState {
            module_id: mod_id + 701,
            char_id: att_char,
            ship_id: 701,
            side_id: att_ally,
            target_id: hub_id,
            cycle_duration_s: 300.0,
            started_unix: now,
            expires_unix: now + 300,
            cycle_index: 1,
            is_capital: false,
        },
    );
    for (sid, tid, side) in [(702, hub_id + 99, att_ally), (703, hub_id, def_ally)] {
        insert_active_entosis_link(
            &ctx,
            sid,
            ActiveEntosisLinkState {
                module_id: mod_id + sid,
                char_id: att_char,
                ship_id: sid,
                side_id: side,
                target_id: tid,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, def_ship, def_char)
            .await
            .unwrap_err()
            .code,
        "CannotEntosisFriendlyStructure"
    );
    clear_active_entosis_links(&ctx);
    insert_active_entosis_link(
        &ctx,
        att_ship,
        ActiveEntosisLinkState {
            module_id: mod_id,
            char_id: att_char,
            ship_id: att_ship,
            side_id: att_ally,
            target_id: hub_id,
            cycle_duration_s: 300.0,
            started_unix: now,
            expires_unix: now + 300,
            cycle_index: 1,
            is_capital: false,
        },
    );
    assert!(precheck_entosis_activation(&ctx, hub_id, def_ship, def_char)
        .await
        .unwrap());
    register_entosis_capture_target(&ctx, hub_id, att_ship, att_char, now);

    // Tick +200s -> 200/600 accrued
    tick_entosis_captures(&ctx, now + 200).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((p.3 - 200.0).abs() < 1e-6);
    });

    // Defender counter-link pauses progress
    insert_active_entosis_link(
        &ctx,
        def_ship,
        ActiveEntosisLinkState {
            module_id: mod_id + 1,
            char_id: def_char,
            ship_id: def_ship,
            side_id: def_ally,
            target_id: hub_id,
            cycle_duration_s: 300.0,
            started_unix: now + 200,
            expires_unix: now + 500,
            cycle_index: 1,
            is_capital: false,
        },
    );
    tick_entosis_captures(&ctx, now + 260).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((p.3 - 200.0).abs() < 1e-6);
    });

    // Remove defender link & advance past 600s total (consuming Strontium for cycle 2) -> Hub reinforced!
    remove_active_entosis_link(&ctx, def_ship);
    tick_entosis_captures(&ctx, now + 670).await;
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&hub_id].state, "HullReinforced");
        assert!(!s.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_node_capture_tenebrex_jamming_and_npc_death_cleanup() {
    let ctx = test_ctx();
    let att_char = 90_000_001_i64;
    let att_ally = 99_000_201_i64;
    let def_ally = 99_000_202_i64;
    let hub_id = 1_080_000_200_i64;
    let node_id = 1_080_000_201_i64;
    let ship_id = 1_080_000_202_i64;
    let tenebrex_id = 1_080_000_203_i64;
    let mto_npc_id = 1_080_000_204_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&att_char) {
            ch.alliance_id = Some(att_ally);
            ch.solar_system_id = sys_id;
        }
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(def_ally);

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_099,
                solar_system_id: sys_id,
                name: "Reinforced Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "HullReinforced".to_string(),
                fuel_blocks: 500,
                reinforced_until_unix: now + 3_600,
                ..Default::default()
            },
        );

        // Online Tenebrex Cynosural Jammer blocks normal cyno in system
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.upgrades.push(PersistedHubUpgrade {
            type_id: UPGRADE_CYNOSURAL_SUPPRESSION,
            online: true,
            low_power: false,
        });
        snap.structures.insert(
            tenebrex_id,
            UpwellStructureRecord {
                structure_id: tenebrex_id,
                type_id: TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: 98_000_099,
                solar_system_id: sys_id,
                name: "Tenebrex Jammer".to_string(),
                position: [20_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .tenebrex_spool_ready_unix
            .insert(tenebrex_id, now);

        // Seed Command Node at 2% defender score so capturing this node wins for attacker!
        snap.sov_runtime
            .command_nodes
            .insert(node_id, (hub_id, sys_id, 0));
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(def_ally, 0.02)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_id, (att_char, att_ally, ship_id, 5_000.0, now));
        snap.sov_runtime
            .sov_upgrade_combat_npcs
            .insert(mto_npc_id);
    });

    assert!(ctx.db.with_snapshot(|s| is_tenebrex_jammer_active_in_snap(s, sys_id, now)));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: node_id,
            type_id: TYPE_STRUCTURE_COMMAND_NODE,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e7,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    insert_active_entosis_link(
        &ctx,
        ship_id,
        ActiveEntosisLinkState {
            module_id: 9_999_001,
            char_id: att_char,
            ship_id,
            side_id: att_ally,
            target_id: node_id,
            cycle_duration_s: 300.0,
            started_unix: now,
            expires_unix: now + 300,
            cycle_index: 1,
            is_capital: false,
        },
    );

    tick_entosis_captures(&ctx, now + 20).await;
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.command_nodes.contains_key(&node_id));
    });

    on_space_entity_destroyed(&ctx, sys_id, None, mto_npc_id, 587);
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.sov_upgrade_combat_npcs.contains(&mto_npc_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entosis_capture_branches_tick_sov_gameplay_and_windows() {
    use eve_db::durable_domain_types::{PersistedMercenaryActivity, PersistedSovHubData};
    use eve_db::CosmicAnomalyRecord;

    let ctx = test_ctx();
    let sys_id = 30_004_759_u32;
    let hub_id = 1_077_000_001_i64;
    let hub_id_2 = 1_077_000_002_i64;
    let node_win_id = 1_077_000_010_i64;
    let node_cont_id = 1_077_000_011_i64;
    let ship_a = 100_777_001_i64;
    let ship_b = 100_777_002_i64;
    let ship_def = 100_777_003_i64;
    let ship_oos = 100_777_004_i64;
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let char_def = 90_000_003_i64;
    let char_no_ally = 90_000_004_i64;
    let def_ally = 99_777_001_i64;
    let att_ally_a = 99_777_002_i64;
    let att_ally_b = 99_777_003_i64;
    let mto_keep_id = 1_077_000_050_i64;
    let mto_drop_id = 1_077_000_051_i64;
    let now = ctx.now_unix();

    let threat_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4768 && t.name.contains("Threat Detection"))
        .map(|t| t.type_id)
        .unwrap_or(81_606);
    let prospect_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4768 && t.name.contains("Prospecting Array"))
        .map(|t| t.type_id)
        .unwrap_or(81_619);
    let explore_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4768 && t.name.contains("Exploration Detector"))
        .map(|t| t.type_id)
        .unwrap_or(81_614);

    // 1. register_entosis_capture_target & vulnerability window edge cases
    ctx.db.with_mut_snapshot(|snap| {
        let ch_tmpl = snap.characters[&char_a].clone();
        for &(cid, corp, ally) in &[
            (char_a, 98_777_001, Some(att_ally_a)),
            (char_b, 98_777_002, Some(att_ally_b)),
            (char_def, 98_777_003, Some(def_ally)),
            (char_no_ally, 98_777_004, None),
        ] {
            let mut c = ch_tmpl.clone();
            c.character_id = cid;
            c.corporation_id = corp;
            c.alliance_id = ally;
            c.solar_system_id = sys_id;
            snap.characters.insert(cid, c);
            snap.corporations.insert(
                corp,
                { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp; __corp.ceo_id = cid; __corp.alliance_id = ally; __corp.name = format!("EntCorp {corp}"); __corp.ticker = "EC".into(); __corp.ensure_wallet_divisions(); __corp },
            );
        }
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(def_ally);
        st.strategic_index = 0;

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_777_003,
                solar_system_id: sys_id,
                name: "Target Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "HullReinforced".into(),
                fuel_blocks: 500,
                reinforced_until_unix: now.saturating_sub(10),
                ..Default::default()
            },
        );
        // Ensure hub_id is unconditionally inside vulnerability window for command node spawn + keep hub_id_win for (12, 4) test
        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 86_400));
        snap.structure_vulnerability_windows.insert(hub_id_2, (12, 4));
        snap.sov_runtime
            .sov_claimed_since_unix
            .insert(sys_id, now.saturating_sub(15 * 86_400));

        // Hacked expired hub_id_2 -> ArmorReinforced transitions back to ShieldVulnerable & low_power=false
        snap.structures.insert(
            hub_id_2,
            UpwellStructureRecord {
                structure_id: hub_id_2,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_777_003,
                solar_system_id: sys_id,
                name: "Expired Hack Hub".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                fuel_blocks: 500,
                reinforced_until_unix: now.saturating_sub(5),
                ..Default::default()
            },
        );
        snap.sov_runtime.hacked_hubs.insert(hub_id_2, now.saturating_sub(5));
        snap.sov_runtime.sov_hubs.insert(
            hub_id_2,
            PersistedSovHubData {
                hub_id: hub_id_2,
                upgrades: vec![
                    PersistedHubUpgrade { type_id: threat_tid, online: true, low_power: true },
                    PersistedHubUpgrade { type_id: prospect_tid, online: true, low_power: false },
                    PersistedHubUpgrade { type_id: explore_tid, online: true, low_power: false },
                    PersistedHubUpgrade { type_id: UPGRADE_CYNOSURAL_SUPPRESSION, online: false, low_power: false },
                ],
                ..Default::default()
            },
        );

        // Pre-seed cleared Combat anomaly ready to re-seed
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: format!("SOV-{hub_id_2}-{threat_tid}"),
            solar_system_id: sys_id,
            name: "Forsaken Hub".into(),
            anomaly_type: "Combat".into(),
            position: [165_000.0, 25_000.0, 0.0],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: now.saturating_sub(5),
            lifetime_seconds: 86_400,
            cleared: true,
        });

        // Tenebrex ready to spool & emit OnCynoJammerChanged
        snap.structures.insert(
            1_077_000_020,
            UpwellStructureRecord {
                structure_id: 1_077_000_020,
                type_id: TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: 98_777_003,
                solar_system_id: sys_id,
                name: "Tenebrex".into(),
                state: "Online".into(),
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        snap.sov_runtime.tenebrex_spool_ready_unix.insert(1_077_000_020, now.saturating_sub(1));

        // MTO keep & drop balls (den must be in mercenary_dens for refresh_activities to run)
        snap.sov_runtime.mercenary_dens.insert(
            1_077_000_090,
            eve_db::durable_domain_types::PersistedMercenaryDen {
                den_id: 1_077_000_090,
                owner_char_id: char_a,
                skyhook_id: 1_077_000_091,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                simulated_at_unix: now,
                infomorphs: 0,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.sov_runtime.mercenary_activities.insert(
            "act_keep".into(),
            PersistedMercenaryActivity {
                uuid: b"act_keep".to_vec(),
                den_id: 1_077_000_090,
                solar_system_id: sys_id,
                dungeon_id: 1,
                started: true,
                started_by: char_a,
                expiry_unix: now + 3_600,
                development_impact: 1,
                anarchy_impact: 1,
                infomorph_bonus: 5,
            },
        );
        snap.sov_runtime.mercenary_activities.insert(
            "act_drop".into(),
            PersistedMercenaryActivity {
                uuid: b"act_drop".to_vec(),
                den_id: 1_077_000_090,
                solar_system_id: sys_id,
                dungeon_id: 1,
                started: true,
                started_by: char_a,
                expiry_unix: now.saturating_sub(1),
                development_impact: 1,
                anarchy_impact: 1,
                infomorph_bonus: 5,
            },
        );
        snap.sov_runtime.mto_site_balls.insert(mto_keep_id, "act_keep".into());
        snap.sov_runtime.mto_site_balls.insert(mto_drop_id, "6163745f64726f70".into());
    });

    // register_entosis_capture_target branches: missing char, no alliance, holder same side, existing<=0 reset, same-side ship==0 update
    register_entosis_capture_target(&ctx, hub_id, ship_a, 999_999_111, now);
    register_entosis_capture_target(&ctx, hub_id, ship_a, char_no_ally, now);
    register_entosis_capture_target(&ctx, hub_id, ship_def, char_def, now);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(hub_id, (char_b, att_ally_b, 0, 0.0, now));
    });
    register_entosis_capture_target(&ctx, hub_id, ship_a, char_a, now + 10);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.sov_runtime.entosis_capture_progress[&hub_id].1, att_ally_a);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(hub_id, (char_a, att_ally_a, 0, 50.0, now));
    });
    register_entosis_capture_target(&ctx, hub_id, ship_a, char_a, now + 30);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.sov_runtime.entosis_capture_progress[&hub_id].2, ship_a);
        assert_eq!(snap.sov_runtime.entosis_capture_progress[&hub_id].4, now + 30);
    });

    // window helper checks including start > 86_400 unix window branch
    ctx.db.with_snapshot(|snap| {
        let _ = is_hub_within_vulnerability_window_in_snap(snap, hub_id_2, now);
    });
    assert!(is_second_in_window(now, now - 5, now + 5));

    // tick_sov_gameplay covers: expire_hacked_hubs, upgrades (Combat/Ore/Data/Relic + cleared Combat respawn + offline despawn), Tenebrex spool, strategic_index update, hub->command nodes, MTO spawn/despawn!
    tick_sov_gameplay(&ctx, now).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.hacked_hubs.contains_key(&hub_id_2));
        assert_eq!(snap.structures[&hub_id_2].state, "ShieldVulnerable");
        assert!(snap.sov_runtime.tenebrex_spool_notified.get(&1_077_000_020) == Some(&true));
        assert!(snap.system_states[&sys_id].strategic_index > 0);
        assert!(!snap.sov_runtime.command_nodes.is_empty());
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_drop_id));
    });

    // Space balls for ship_a, ship_b, ship_def, ship_oos (far out of range) + command nodes
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(sid, pos) in &[
        (ship_a, DVec3::new(1_000.0, 0.0, 0.0)),
        (ship_b, DVec3::new(2_000.0, 0.0, 0.0)),
        (ship_def, DVec3::new(3_000.0, 0.0, 0.0)),
        (ship_oos, DVec3::new(900_000.0, 0.0, 0.0)),
        (node_win_id, DVec3::new(500.0, 0.0, 0.0)),
        (node_cont_id, DVec3::new(500.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Exercise tick_entosis_captures branches:
    // (a) Out-of-range ship removed + unattended regen removes <= 1e-6 or retains > 1e-6
    insert_active_entosis_link(
        &ctx,
        ship_oos,
        ActiveEntosisLinkState {
            module_id: 8001,
            char_id: char_a,
            ship_id: ship_oos,
            side_id: att_ally_a,
            target_id: hub_id,
            cycle_duration_s: 300.0,
            started_unix: now,
            expires_unix: now + 300,
            cycle_index: 1,
            is_capital: false,
        },
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(hub_id, (char_a, att_ally_a, ship_oos, 0.1, now));
        snap.sov_runtime.entosis_capture_progress.insert(9_999_888, (char_a, att_ally_a, 0, 1.0, now));
    });
    tick_entosis_captures(&ctx, now + 500).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&9_999_888));
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // (b) Contested >1 side -> paused, defender counter-entosis drops to partial then 0, opposing side de-capture flips side, out-of-stront stops cleanly
    for &(sid, cid, side, mid, tgt, exp) in &[
        (ship_a, char_a, att_ally_a, 8002, node_cont_id, now + 9_000),
        (ship_b, char_b, att_ally_b, 8003, node_cont_id, now + 9_000),
        (ship_def, char_def, def_ally, 8004, hub_id, now + 9_000),
    ] {
        insert_active_entosis_link(
            &ctx,
            sid,
            ActiveEntosisLinkState {
                module_id: mid,
                char_id: cid,
                ship_id: sid,
                side_id: side,
                target_id: tgt,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: exp,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.command_nodes.insert(node_cont_id, (hub_id_2, sys_id, 0));
        snap.sov_runtime.entosis_capture_progress.insert(node_cont_id, (char_a, att_ally_a, ship_a, 50.0, now));
        snap.sov_runtime.entosis_capture_progress.insert(hub_id, (char_a, att_ally_a, ship_a, 30.0, now));
    });
    tick_entosis_captures(&ctx, now + 10).await; // contested node paused, defender reduces hub from 30 -> 20
    tick_entosis_captures(&ctx, now + 40).await; // defender reduces hub from 20 -> 0 & clears link

    // Opposing side de-captures partial then flips, and stront expiry drops link
    remove_active_entosis_link(&ctx, ship_a);
    set_active_entosis_link_expires_unix(&ctx, ship_b, now + 50); // expires at +50 with no stront!
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(node_cont_id, (char_a, att_ally_a, 0, 50.0, now + 10));
    });
    tick_entosis_captures(&ctx, now + 20).await; // eff_dt=10 < 50 -> de-capture 50 -> 40
    tick_entosis_captures(&ctx, now + 80).await; // expired at +50 -> eff_dt=30 < 40 & !stront_alive -> link removed!

    // Opposing flip when eff_dt > accrued_secs and stront_alive
    insert_active_entosis_link(
        &ctx,
        ship_b,
        ActiveEntosisLinkState {
            module_id: 8005,
            char_id: char_b,
            ship_id: ship_b,
            side_id: att_ally_b,
            target_id: node_cont_id,
            cycle_duration_s: 300.0,
            started_unix: now + 80,
            expires_unix: now + 500,
            cycle_index: 1,
            is_capital: false,
        },
    );
    insert_active_entosis_link(
        &ctx,
        ship_a,
        ActiveEntosisLinkState {
            module_id: 8006,
            char_id: char_a,
            ship_id: ship_a,
            side_id: att_ally_a,
            target_id: node_win_id,
            cycle_duration_s: 300.0,
            started_unix: now + 80,
            expires_unix: now + 500,
            cycle_index: 1,
            is_capital: false,
        },
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(node_cont_id, (char_a, att_ally_a, 0, 5.0, now + 80));
        // Attacker wins Command Node on hub_id_2 -> hub destroyed & upgrades dropped as loot!
        snap.sov_runtime.command_nodes.insert(node_win_id, (hub_id_2, sys_id, 0));
        snap.sov_runtime.command_node_scores.insert(hub_id_2, vec![(def_ally, 0.02)]);
        snap.sov_runtime.entosis_capture_progress.insert(node_win_id, (char_a, att_ally_a, ship_a, 5_000.0, now + 80));
    });
    tick_entosis_captures(&ctx, now + 100).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&hub_id_2].state, "Destroyed");
        assert!(snap.system_states[&sys_id].sov_alliance_id.is_none());
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&hub_id_2));
    });

    // Cover hub upgrade site spawning (Ore, Combat, Data/Relic), NPC destruction clearing Sov anomaly + MTO site kill,
    // and upgrade low_power/hacked despawn in tick_entosis_captures!
    let mut ore_upg = None;
    let mut combat_upg = None;
    let mut data_upg = None;
    for t in ctx.sde.types.values().filter(|t| t.group_id == 4768) {
        let specs = crate::services::sov_warzone_dynamic::sov_upgrade_site_specs(&t.name, -0.5);
        for (arch, _, _, ores, _) in specs {
            if arch == "Ore" && !ores.is_empty() && ore_upg.is_none() {
                ore_upg = Some(t.type_id);
            } else if arch == "Combat" && combat_upg.is_none() {
                combat_upg = Some(t.type_id);
            } else if matches!(arch, "Data" | "Relic") && data_upg.is_none() {
                data_upg = Some(t.type_id);
            }
        }
    }
    let upg_hub_id = hub_id_2 + 50;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(upg_hub_id, eve_db::UpwellStructureRecord {
            structure_id: upg_hub_id,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_corp_id: 98_000_099,
            solar_system_id: sys_id,
            name: "UpgHub".into(),
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            ..Default::default()
        });
        let mut hub = snap.sov_runtime.sov_hubs.entry(upg_hub_id).or_default().clone();
        hub.hub_id = upg_hub_id;
        hub.upgrades.clear();
        for tid in [ore_upg, combat_upg, data_upg].into_iter().flatten() {
            hub.upgrades.push(eve_db::PersistedHubUpgrade {
                type_id: tid,
                online: true,
                low_power: false,
            });
        }
        snap.sov_runtime.sov_hubs.insert(upg_hub_id, hub);
        // Seed MTO site ball + mercenary activity for on_space_entity_destroyed_ext MTO completion branch
        let mto_act_key = "mto_kill_test".to_string();
        snap.sov_runtime.mto_site_balls.insert(1_990_000_001, mto_act_key.clone());
        snap.sov_runtime.mercenary_activities.insert(mto_act_key, eve_db::PersistedMercenaryActivity {
            uuid: vec![1, 2, 3, 4],
            den_id: 1_077_000_090,
            solar_system_id: sys_id,
            dungeon_id: 1,
            started: true,
            started_by: char_a,
            expiry_unix: now + 3_600,
            development_impact: 1,
            anarchy_impact: 1,
            infomorph_bonus: 1,
        });
    });
    tick_entosis_captures(&ctx, now + 110).await;

    // Destroy all sov_upgrade_combat_npcs + MTO ball -> clears Sov anomaly & completes MTO activity
    let combat_nids: Vec<i64> = ctx.db.with_snapshot(|snap| snap.sov_runtime.sov_upgrade_combat_npcs.iter().copied().collect());
    for nid in combat_nids {
        on_space_entity_destroyed(&ctx, sys_id, Some(char_a), nid, 37_453);
    }
    on_space_entity_destroyed(&ctx, sys_id, Some(char_a), 1_990_000_001, 37_453);

    // Hack hub & tick again -> hits despawn branch
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.insert(upg_hub_id, now + 500);
    });
    tick_entosis_captures(&ctx, now + 120).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_entosis_strategic_command_nodes_mto_and_kills_cov() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    // Pick a K-space nullsec system with the most planets (so Skyhooks supply plenty of power/workforce)
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .max_by_key(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .iter()
                .filter(|c| c.group_id == 7)
                .count()
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_751);
    let planets: Vec<i64> = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .filter(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .collect();

    let char_id = 90_000_810;
    let corp_id = 98_000_810;
    let alliance_id = 99_000_810;
    let ship_id = 1_088_810;
    let hub_id = 1_088_811;
    let upg_hub_ore = 1_088_812;
    let upg_hub_combat = 1_088_822;
    let upg_hub_data = 1_088_832;
    let exp_hub_id = 1_088_842;
    let jammer_id = 1_088_813;
    let cmd_node_id = 1_088_814;

    // Pick lowest-power upgrade of each archetype in group 4768 so hub never overloads
    let mut ore_upg: Option<(u32, u64)> = None;
    let mut combat_upg: Option<(u32, u64)> = None;
    let mut data_upg: Option<(u32, u64)> = None;
    for t in ctx.sde.types.values().filter(|t| t.group_id == 4768) {
        let req_p = crate::services::quasar_sov_hub::upgrade_definition(&ctx.sde, t.type_id)
            .map(|d| d.power_required + d.workforce_required)
            .unwrap_or(u64::MAX);
        let specs = crate::services::sov_warzone_dynamic::sov_upgrade_site_specs(&t.name, -0.5);
        for (arch, _, _, ores, _) in specs {
            if arch == "Ore" && !ores.is_empty() && ore_upg.is_none_or(|(_, p)| req_p < p) {
                ore_upg = Some((t.type_id, req_p));
            } else if arch == "Combat" && combat_upg.is_none_or(|(_, p)| req_p < p) {
                combat_upg = Some((t.type_id, req_p));
            } else if matches!(arch, "Data" | "Relic") && data_upg.is_none_or(|(_, p)| req_p < p) {
                data_upg = Some((t.type_id, req_p));
            }
        }
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 588,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(alliance_id);
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

        let st = snap.system_states.entry(sys_id).or_default();
        st.sov_alliance_id = Some(99_000_809);
        st.strategic_index = 0;
        st.military_index = 2;
        st.industrial_index = 1;
        snap.sov_runtime.sov_claimed_since_unix.insert(sys_id, now);
        // (0, 0) means always open vulnerability window; also populate structure_vulnerability_windows for win_hub (exp_hub_id)
        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 0));
        snap.structure_vulnerability_windows.insert(exp_hub_id, (12, 2));

        // Harvest every planet in sys_id with an Orbital Skyhook so local power/workforce is abundant
        for (idx, &pid) in planets.iter().enumerate() {
            let sh_id = 1_088_900 + idx as i64;
            snap.structures.insert(
                sh_id,
                UpwellStructureRecord {
                    structure_id: sh_id,
                    type_id: crate::services::sov_services::TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: 98_000_809,
                    solar_system_id: sys_id,
                    name: format!("Skyhook-{idx}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
            snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }

        // HullReinforced Sovereignty Hub ready for Command Nodes
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_809,
                solar_system_id: sys_id,
                name: "ReinforcedHub".into(),
                state: "HullReinforced".into(),
                reinforced_until_unix: now,
                ..Default::default()
            },
        );

        // Online Tenebrex jammer ready to finish spooling
        snap.structures.insert(
            jammer_id,
            UpwellStructureRecord {
                structure_id: jammer_id,
                type_id: crate::services::sov_services::TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: 98_000_809,
                solar_system_id: sys_id,
                name: "Jammer".into(),
                state: "Online".into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.sov_runtime.tenebrex_spool_ready_unix.insert(jammer_id, now + 100);

        // 3 separate active hubs (one upgrade each) + 1 expired hacked hub (hits 135-158)
        for (hid, tid_opt) in [
            (upg_hub_ore, ore_upg.map(|x| x.0)),
            (upg_hub_combat, combat_upg.map(|x| x.0)),
            (upg_hub_data, data_upg.map(|x| x.0)),
        ] {
            if let Some(tid) = tid_opt {
                snap.structures.insert(
                    hid,
                    UpwellStructureRecord {
                        structure_id: hid,
                        type_id: TYPE_SOVEREIGNTY_HUB,
                        owner_corp_id: 98_000_809,
                        solar_system_id: sys_id,
                        name: format!("ActiveUpgHub-{hid}"),
                        state: "ShieldVulnerable".into(),
                        quantum_core_installed: true,
                        ..Default::default()
                    },
                );
                let mut hub = snap.sov_runtime.sov_hubs.entry(hid).or_default().clone();
                hub.hub_id = hid;
                hub.upgrades = vec![PersistedHubUpgrade {
                    type_id: tid,
                    online: true,
                    low_power: false,
                }];
                snap.sov_runtime.sov_hubs.insert(hid, hub);
            }
        }

        // Expired hacked hub in ArmorReinforced state (covers expire_hacked_hubs_in_snap 135-158)
        snap.structures.insert(
            exp_hub_id,
            UpwellStructureRecord {
                structure_id: exp_hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_809,
                solar_system_id: sys_id,
                name: "ExpHackedHub".into(),
                state: "ArmorReinforced".into(),
                reinforced_until_unix: now,
                ..Default::default()
            },
        );
        let mut exp_hub = snap.sov_runtime.sov_hubs.entry(exp_hub_id).or_default().clone();
        exp_hub.hub_id = exp_hub_id;
        if let Some((tid, _)) = combat_upg {
            exp_hub.upgrades = vec![PersistedHubUpgrade {
                type_id: tid,
                online: true,
                low_power: true,
            }];
        }
        snap.sov_runtime.sov_hubs.insert(exp_hub_id, exp_hub);
        snap.sov_runtime.hacked_hubs.insert(exp_hub_id, now);

        // Seed Command Node for precheck_entosis_activation (250-257)
        snap.sov_runtime.command_nodes.insert(cmd_node_id, (upg_hub_ore, sys_id, 0));
        snap.item_positions.insert(cmd_node_id, [10_000.0, 0.0, 0.0]);

        // Seed MTO site ball + mercenary activity (started: true -> spawns MTO ball & vulnerable combat state)
        let mto_key = "mto_spawn_cov".to_string();
        snap.sov_runtime.mto_site_balls.insert(1_990_000_810, mto_key.clone());
        snap.sov_runtime.mercenary_activities.insert(
            mto_key,
            eve_db::PersistedMercenaryActivity {
                uuid: vec![9, 8, 7, 6],
                den_id: 1_088_820,
                solar_system_id: sys_id,
                dungeon_id: 1,
                started: true,
                started_by: char_id,
                expiry_unix: now + 86_400 * 50,
                development_impact: 1,
                anarchy_impact: 1,
                infomorph_bonus: 1,
            },
        );
        snap.fw_runtime.battlefield_npc_waves.insert(0, vec![1_888_888]);
    });

    // precheck_entosis_activation targeting a Command Node (correct arg order: target_id, ship_id, char_id -> hits 250-257!)
    let _ = precheck_entosis_activation(&ctx, cmd_node_id, ship_id, char_id).await;
    // Also test is_hub_within_vulnerability_window_in_snap hour+duration fallback (111-119)
    ctx.db.with_snapshot(|snap| {
        let _ = is_hub_within_vulnerability_window_in_snap(snap, exp_hub_id, now);
    });

    // on_space_entity_destroyed: player structure early return, player ship early return, FW/convoy NPC kill
    on_space_entity_destroyed(&ctx, sys_id, Some(char_id), upg_hub_ore, TYPE_SOVEREIGNTY_HUB);
    on_space_entity_destroyed(&ctx, sys_id, Some(char_id), ship_id, 588);
    on_space_entity_destroyed(&ctx, sys_id, Some(char_id), 1_888_888, 37_453);

    // Run tick_sov_gameplay: expired hacked hub + tenebrex spool + strategic index + command nodes + Ore/Combat/Data upgrades + MTO!
    tick_sov_gameplay(&ctx, now + 35 * 86_400).await;

    // Second tick after combat anomaly cleared & MTO expired + hubs hacked -> covers combat reset & despawn branches (698-702, 832-846)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
            for a in list.iter_mut() {
                a.cleared = true;
            }
        }
        snap.sov_runtime.hacked_hubs.insert(upg_hub_ore, now + 40 * 86_400);
        snap.sov_runtime.hacked_hubs.insert(upg_hub_data, now + 40 * 86_400);
        if let Some(a) = snap.sov_runtime.mercenary_activities.get_mut("mto_spawn_cov") {
            a.expiry_unix = now + 1;
        }
    });
    tick_sov_gameplay(&ctx, now + 35 * 86_400 + 10).await;
}

#[tokio::test]
async fn tick_sov_gameplay_and_mto_kill_clamp_export_before_overload_and_reconcile_vuln_expiry() {
    use crate::services::quasar_sov_hub::{
        all_upgrade_definitions, compute_hub_resources, max_exportable_workforce,
        planet_yield_for_planet, system_yield, UpgradeDefinition,
    };
    use crate::services::quasar_sov_mercden::anarchy_workforce_disruption_pct;
    use crate::services::quasar_sov_workforce::{exported_amount, imports};
    use crate::services::sov_gameplay::TYPE_MERCENARY_DEN;
    use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
    use eve_db::durable_domain_types::{PersistedMercenaryActivity, PersistedSovHubData};
    use eve_db::{PersistedMercenaryDen, PersistedWorkforceMode};

    let ctx = test_ctx();

    let sys_a: u32 = 30_000_669; // N3-JBX
    let sys_b: u32 = 30_000_671; // GN-PDU (1 jump)
    let corp_id: i64 = 98_772_001;
    let char_id: i64 = 2_117_720_001;
    let hub_a: i64 = 1_097_720_001;
    let hub_b: i64 = 1_097_720_002;
    let skyhook_a: i64 = 1_097_720_010;
    let den_id: i64 = 1_097_720_020;
    let mto_ball_id: i64 = 1_997_720_001;
    let now = ctx.now_unix();

    let mut sess_a = eve_machonet::SessionState::new(7_720_001);
    sess_a.solar_system_id = Some(sys_a);
    let mut rx_a = ctx.quasar_gateway.subscribe_session(&sess_a, Vec::new(), now);
    let mut sess_b = eve_machonet::SessionState::new(7_720_002);
    sess_b.solar_system_id = Some(sys_b);
    let mut rx_b = ctx.quasar_gateway.subscribe_session(&sess_b, Vec::new(), now);

    let (planet_wf_id, planet_wf) = ctx
        .sde
        .celestials_in_system_slice(sys_a)
        .iter()
        .filter(|c| c.group_id == 7)
        .map(|c| (c.celestial_id, planet_yield_for_planet(c.type_id, c.radius).1))
        .max_by_key(|&(_, wf)| wf)
        .expect("SYS_A workforce planet");
    let sys_a_power: u64 = system_yield(&ctx.sde, sys_a).stars.iter().map(|(_, p)| *p).sum();
    let sys_b_power: u64 = system_yield(&ctx.sde, sys_b).stars.iter().map(|(_, p)| *p).sum();

    let disrupted_planet =
        planet_wf - (planet_wf * anarchy_workforce_disruption_pct(5)) / 100;
    let defs: Vec<UpgradeDefinition> = all_upgrade_definitions(&ctx.sde);
    let hub_a_local = defs
        .iter()
        .find(|d| {
            d.workforce_produced == 0
                && d.workforce_required > 0
                && d.workforce_required < disrupted_planet
                && d.power_required <= sys_a_power
                && d.fuel_type == 0
        })
        .cloned()
        .expect("SYS_A zero-fuel upgrade fitting under disrupted planet workforce");
    let max_exp_full = planet_wf - hub_a_local.workforce_required;
    let max_exp_disrupted = disrupted_planet - hub_a_local.workforce_required;
    assert!(max_exp_disrupted < max_exp_full);

    let hub_b_upgrades: Vec<UpgradeDefinition> = if let Some(u) = defs.iter().find(|d| {
        d.workforce_produced == 0
            && d.workforce_required > max_exp_disrupted
            && d.workforce_required <= max_exp_full
            && d.power_required <= sys_b_power
    }) {
        vec![u.clone()]
    } else {
        let mut pair = Vec::new();
        'outer: for (i, a) in defs.iter().enumerate() {
            if a.workforce_produced > 0 || a.workforce_required == 0 || a.power_required > sys_b_power {
                continue;
            }
            for b in defs.iter().skip(i + 1) {
                if b.workforce_produced > 0
                    || b.workforce_required == 0
                    || (!a.exclusive_group.is_empty() && a.exclusive_group == b.exclusive_group)
                {
                    continue;
                }
                let tot_wf = a.workforce_required + b.workforce_required;
                if a.power_required + b.power_required <= sys_b_power
                    && tot_wf > max_exp_disrupted
                    && tot_wf <= max_exp_full
                {
                    pair = vec![a.clone(), b.clone()];
                    break 'outer;
                }
            }
        }
        assert!(!pair.is_empty(), "expected SYS_B upgrades straddling [{max_exp_disrupted}, {max_exp_full}]");
        pair
    };
    let hub_b_demand: u64 = hub_b_upgrades.iter().map(|u| u.workforce_required).sum();
    // Over-export amount exceeds Hub A's max_exportable_workforce AND exceeds hub_b_demand,
    // so clamping alone leaves Hub B starved ONLY if a further demand exceeds max_exp_full or
    // when Den re-enables; pick Hub B demand for Case A using `planet_wf` > `max_exp_full`.
    let hub_b_over_cap_upgrade = defs
        .iter()
        .find(|d| {
            d.workforce_produced == 0
                && d.workforce_required > max_exp_full
                && d.workforce_required <= planet_wf
                && d.power_required <= sys_b_power
        })
        .cloned();

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, hid, name) in [(sys_a, hub_a, "Hub A"), (sys_b, hub_b, "Hub B")] {
            snap.system_states.entry(sid).or_default().sov_alliance_id = Some(99_772_001);
            snap.structures.insert(
                hid,
                UpwellStructureRecord {
                    structure_id: hid,
                    type_id: TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_id,
                    solar_system_id: sid,
                    name: name.into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
        snap.structures.insert(
            skyhook_a,
            UpwellStructureRecord {
                structure_id: skyhook_a,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Skyhook A".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_a, planet_wf_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_a, true);

        // Case 1: Hub A starts with export == planet_wf (> max_exportable_workforce = max_exp_full)
        // so without clamp-before-overload, Hub A's own `hub_a_local` would be overloaded!
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: hub_a_local.type_id,
                    online: true,
                    low_power: false,
                }],
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: planet_wf,
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );
        let b_upgs = if let Some(ref over_u) = hub_b_over_cap_upgrade {
            vec![over_u.clone()]
        } else {
            hub_b_upgrades.clone()
        };
        let mut fuel_b = std::collections::BTreeMap::new();
        for u in &b_upgs {
            if u.fuel_type > 0 {
                fuel_b.insert(u.fuel_type, u.fuel_startup + u.fuel_per_hour * 100);
            }
        }
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                upgrades: b_upgs
                    .iter()
                    .map(|u| PersistedHubUpgrade {
                        type_id: u.type_id,
                        online: true,
                        low_power: false,
                    })
                    .collect(),
                fuel: fuel_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![sys_a],
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );
    });

    tick_sov_gameplay(&ctx, now + 10).await;

    ctx.db.with_snapshot(|snap| {
        assert_eq!(max_exportable_workforce(snap, &ctx.sde, hub_a), Some(max_exp_full));
        assert_eq!(exported_amount(snap, hub_a), max_exp_full);
        assert!(
            !snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power,
            "Hub A local upgrade must remain online when export is clamped before overload"
        );
        assert_eq!(imports(snap, &ctx.sde, hub_b), vec![(sys_a, max_exp_full)]);
        if hub_b_over_cap_upgrade.is_some() {
            assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
        }
    });
    let mut urls1 = Vec::new();
    while let Ok(n) = rx_a.try_recv() {
        if let Some(p) = n.payload {
            urls1.push(p.type_url);
        }
    }
    while rx_b.try_recv().is_ok() {}
    assert!(urls1
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(urls1
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // Case 2: MTO NPC destruction (`on_space_entity_destroyed`) also clamps export BEFORE
    // evaluating Hub A overload and reconciles downstream Hub B + notices.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: 98_772_099,
                solar_system_id: sys_a,
                name: "Test MercDen".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_char_id: char_id,
                skyhook_id: skyhook_a,
                solar_system_id: sys_a,
                planet_id: planet_wf_id,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 10,
                anarchy_level: 10, // Stage 1 -> 0% disruption
                infomorphs: 0,
                simulated_at_unix: now + 10,
                next_activity_generation_unix: now + 3_610,
            },
        );
        let act_key = "entosis_mto_clamp".to_string();
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id, act_key.clone());
        snap.sov_runtime.mercenary_activities.insert(
            act_key,
            PersistedMercenaryActivity {
                uuid: vec![0xB2; 16],
                den_id,
                solar_system_id: sys_a,
                dungeon_id: 12_367,
                started: true,
                started_by: char_id,
                expiry_unix: now + 3_600,
                development_impact: 5,
                anarchy_impact: -5,
                infomorph_bonus: 0,
            },
        );
        if let Some(hub) = snap.sov_runtime.sov_hubs.get_mut(&hub_a) {
            hub.workforce = PersistedWorkforceMode::Export {
                destination: Some(sys_b),
                amount: planet_wf,
            };
            hub.upgrades[0].low_power = false;
        }
        if let Some(hub) = snap.sov_runtime.sov_hubs.get_mut(&hub_b) {
            for u in &mut hub.upgrades {
                u.low_power = false;
            }
        }
    });

    on_space_entity_destroyed(&ctx, sys_a, Some(char_id), mto_ball_id, 12_345);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_id));
        assert_eq!(exported_amount(snap, hub_a), max_exp_full);
        assert!(!snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power);
        assert_eq!(imports(snap, &ctx.sde, hub_b), vec![(sys_a, max_exp_full)]);
    });
    let mut urls2 = Vec::new();
    while let Ok(n) = rx_a.try_recv() {
        if let Some(p) = n.payload {
            urls2.push(p.type_url);
        }
    }
    while rx_b.try_recv().is_ok() {}
    assert!(urls2
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(urls2
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // Case 3: tick_sov_gameplay past MercDen vuln_end re-enables Stage 5 Den and cascades export clamp to Hub B.
    let vuln_end = now + 100;
    ctx.db.with_mut_snapshot(|snap| {
        let mut fuel_b = std::collections::BTreeMap::new();
        for u in &hub_b_upgrades {
            if u.fuel_type > 0 {
                fuel_b.insert(u.fuel_type, u.fuel_startup + u.fuel_per_hour * 100);
            }
        }
        if let Some(hb) = snap.sov_runtime.sov_hubs.get_mut(&hub_b) {
            hb.fuel = fuel_b;
            hb.upgrades = hub_b_upgrades
                .iter()
                .map(|u| PersistedHubUpgrade {
                    type_id: u.type_id,
                    online: true,
                    low_power: false,
                })
                .collect();
        }
        let st = snap.structures.get_mut(&den_id).unwrap();
        st.state = "HullVulnerable".into();
        st.reinforced_until_unix = vuln_end;
        st.shield_hp = 0.0;
        let den = snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap();
        den.enabled = false;
        den.anarchy_level = 100;
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (now + 20, vuln_end, 0));
        assert!(hub_b_demand > max_exp_disrupted && hub_b_demand <= max_exp_full);
    });

    tick_sov_gameplay(&ctx, vuln_end).await;
    ctx.db.with_snapshot(|snap| {
        assert!(snap.sov_runtime.mercenary_dens[&den_id].enabled);
        assert_eq!(snap.structures[&den_id].state, "ShieldVulnerable");
        assert_eq!(
            compute_hub_resources(snap, &ctx.sde, hub_a).unwrap().workforce_local,
            disrupted_planet
        );
        assert_eq!(exported_amount(snap, hub_a), max_exp_disrupted);
        assert!(!snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power);
        assert_eq!(imports(snap, &ctx.sde, hub_b), vec![(sys_a, max_exp_disrupted)]);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades.last().unwrap().low_power);
    });
    let mut urls3_a = Vec::new();
    while let Ok(n) = rx_a.try_recv() {
        if let Some(p) = n.payload {
            urls3_a.push(p.type_url);
        }
    }
    assert!(urls3_a
        .iter()
        .any(|u| u.ends_with("mercenaryden.api.EvolutionStateChangedNotice")));
    assert!(urls3_a
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(urls3_a
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    let mut urls3_b = Vec::new();
    while let Ok(n) = rx_b.try_recv() {
        if let Some(p) = n.payload {
            urls3_b.push(p.type_url);
        }
    }
    assert!(urls3_b
        .iter()
        .any(|u| u.ends_with("upgrade.api.HubUpgradesSimulatedNotice")));
}

#[tokio::test]
async fn hacked_sov_hub_despawns_all_five_major_threat_detection_sites_and_gates() {
    use crate::services::quasar_sov_hub::{all_upgrade_definitions, planet_yield_for_planet, system_yield};
    use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
    use eve_db::durable_domain_types::PersistedSovHubData;

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let threat_iii_def = all_upgrade_definitions(&ctx.sde)
        .into_iter()
        .find(|d| {
            ctx.sde.types.get(&d.type_id).is_some_and(|t| {
                crate::services::sov_warzone_dynamic::sov_upgrade_site_specs(&t.name, -0.6).len()
                    == 5
            })
        })
        .expect("5-site Major Threat Detection Array III upgrade definition in SDE");
    let threat_iii_tid = threat_iii_def.type_id;

    let (sys_id, planets): (u32, Vec<i64>) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= -0.45)
        .find_map(|s| {
            let y = system_yield(&ctx.sde, s.solar_system_id);
            let mut tot_p: u64 = y.stars.iter().map(|(_, p)| *p).sum();
            let mut tot_w: u64 = 0;
            let mut pids = Vec::new();
            for c in ctx.sde.celestials_in_system_slice(s.solar_system_id) {
                if c.group_id == 7 {
                    let (p, w) = planet_yield_for_planet(c.type_id, c.radius);
                    tot_p += p;
                    tot_w += w;
                    pids.push(c.celestial_id);
                }
            }
            (tot_p >= threat_iii_def.power_required
                && tot_w >= threat_iii_def.workforce_required)
                .then_some((s.solar_system_id, pids))
        })
        .expect("nullsec system with sufficient power and workforce for Major Threat Detection Array III");

    let hub_id = 1_077_005_001_i64;
    let corp_id = 98_777_055_i64;
    let mut fuel = std::collections::BTreeMap::new();
    if threat_iii_def.fuel_type > 0 {
        fuel.insert(
            threat_iii_def.fuel_type,
            threat_iii_def.fuel_startup + threat_iii_def.fuel_per_hour * 10,
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states
            .entry(sys_id)
            .or_default()
            .sov_alliance_id = Some(99_077_055);
        for (idx, pid) in planets.iter().copied().enumerate() {
            let sh_id = 1_077_006_000 + idx as i64;
            snap.structures.insert(
                sh_id,
                UpwellStructureRecord {
                    structure_id: sh_id,
                    type_id: TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Skyhook {idx}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
            snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "5-Site Threat Hub".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                fuel,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: threat_iii_tid,
                    online: true,
                    low_power: false,
                }],
                last_updated_unix: now,
                ..Default::default()
            },
        );
    });

    tick_sov_gameplay(&ctx, now).await;

    let base_id = 1_940_000_000
        + (hub_id.unsigned_abs() as i64 % 100_000) * 100
        + i64::from(threat_iii_tid % 100) * 10;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for s_idx in 0..5_i64 {
        let site_base_id = base_id + s_idx * 10;
        assert!(
            sys_handle.get_ball_snapshot(site_base_id).await.unwrap().is_some(),
            "site {s_idx} gate/anchor ball {site_base_id} must be spawned before hack"
        );
        for off in 1..=5_i64 {
            let npc_id = site_base_id + off;
            assert!(
                sys_handle.get_ball_snapshot(npc_id).await.unwrap().is_some(),
                "site {s_idx} NPC ball {npc_id} must be spawned before hack"
            );
        }
    }
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .sov_runtime
            .sov_upgrade_combat_npcs
            .contains(&(base_id + 45)));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.insert(hub_id, now + 3_600);
    });
    tick_sov_gameplay(&ctx, now + 1).await;

    for s_idx in 0..5_i64 {
        let site_base_id = base_id + s_idx * 10;
        assert!(
            sys_handle.get_ball_snapshot(site_base_id).await.unwrap().is_none(),
            "site {s_idx} gate/anchor ball {site_base_id} must despawn on hub hack"
        );
        for off in 1..=5_i64 {
            let npc_id = site_base_id + off;
            assert!(
                sys_handle.get_ball_snapshot(npc_id).await.unwrap().is_none(),
                "site {s_idx} NPC ball {npc_id} (including +41..=+45) must despawn on hub hack"
            );
            assert!(
                !crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, npc_id),
                "site {s_idx} combat NPC entity {npc_id} must be removed"
            );
        }
    }
    ctx.db.with_snapshot(|snap| {
        let prefix = format!("SOV-{hub_id}-");
        assert!(
            snap.cosmic_anomalies
                .get(&sys_id)
                .into_iter()
                .flatten()
                .all(|a| !a.anomaly_id.starts_with(&prefix)),
            "hacked hub must remove all upgrade cosmic anomalies"
        );
        for s_idx in 0..5_i64 {
            for off in 1..=5_i64 {
                assert!(!snap
                    .sov_runtime
                    .sov_upgrade_combat_npcs
                    .contains(&(base_id + s_idx * 10 + off)));
            }
        }
    });
}


