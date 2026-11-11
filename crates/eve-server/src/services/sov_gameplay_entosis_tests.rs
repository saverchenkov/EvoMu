//! Unit tests for `sov_gameplay_entosis` (ADM vulnerability windows, Entosis precheck & capture tick, Tenebrex, Flex upgrades).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
use crate::services::tactical_modules::{db_ptr_key, ActiveEntosisLinkState};
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
    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        for ((k, sid), tid, side) in [
            ((db_key.wrapping_add(1), 701), hub_id, att_ally),
            ((db_key, 702), hub_id + 99, att_ally),
            ((db_key, 703), hub_id, def_ally),
        ] {
            tst.entosis_active_links.insert(
                (k, sid),
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
    }
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, def_ship, def_char)
            .await
            .unwrap_err()
            .code,
        "CannotEntosisFriendlyStructure"
    );
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.clear();
        tst.entosis_active_links.insert(
            (db_key, att_ship),
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
    }
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
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.insert(
            (db_key, def_ship),
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
    }
    tick_entosis_captures(&ctx, now + 260).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((p.3 - 200.0).abs() < 1e-6);
    });

    // Remove defender link & advance past 600s total (consuming Strontium for cycle 2) -> Hub reinforced!
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.remove(&(db_key, def_ship));
    }
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

    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.insert(
            (db_key, ship_id),
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
    }

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
    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.insert(
            (db_key, ship_oos),
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
    }
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
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        for &(sid, cid, side, mid, tgt, exp) in &[
            (ship_a, char_a, att_ally_a, 8002, node_cont_id, now + 9_000),
            (ship_b, char_b, att_ally_b, 8003, node_cont_id, now + 9_000),
            (ship_def, char_def, def_ally, 8004, hub_id, now + 9_000),
        ] {
            tst.entosis_active_links.insert(
                (db_key, sid),
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
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.command_nodes.insert(node_cont_id, (hub_id_2, sys_id, 0));
        snap.sov_runtime.entosis_capture_progress.insert(node_cont_id, (char_a, att_ally_a, ship_a, 50.0, now));
        snap.sov_runtime.entosis_capture_progress.insert(hub_id, (char_a, att_ally_a, ship_a, 30.0, now));
    });
    tick_entosis_captures(&ctx, now + 10).await; // contested node paused, defender reduces hub from 30 -> 20
    tick_entosis_captures(&ctx, now + 40).await; // defender reduces hub from 20 -> 0 & clears link

    // Opposing side de-captures partial then flips, and stront expiry drops link
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.remove(&(db_key, ship_a));
        if let Some(l) = tst.entosis_active_links.get_mut(&(db_key, ship_b)) {
            l.expires_unix = now + 50; // expires at +50 with no stront!
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.entosis_capture_progress.insert(node_cont_id, (char_a, att_ally_a, 0, 50.0, now + 10));
    });
    tick_entosis_captures(&ctx, now + 20).await; // eff_dt=10 < 50 -> de-capture 50 -> 40
    tick_entosis_captures(&ctx, now + 80).await; // expired at +50 -> eff_dt=30 < 40 & !stront_alive -> link removed!

    // Opposing flip when eff_dt > accrued_secs and stront_alive
    {
        let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        tst.entosis_active_links.insert(
            (db_key, ship_b),
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
        tst.entosis_active_links.insert(
            (db_key, ship_a),
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
    }
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

