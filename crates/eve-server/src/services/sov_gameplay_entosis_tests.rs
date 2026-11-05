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

#[tokio::test]
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

#[tokio::test]
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

