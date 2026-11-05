//! Deep branch and mutation-killing unit tests for `sov_gameplay_entosis` (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::sov_gameplay::sov_gameplay_entosis::*;
use crate::services::sov_services::{TYPE_PHAROLUX_CYNO_BEACON, TYPE_SOVEREIGNTY_HUB};
use crate::services::tactical_modules::{db_ptr_key, ActiveEntosisLinkState};
use eve_db::durable_domain_types::PersistedHubUpgrade;
use eve_db::{CosmicAnomalyRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{InvType, SdeRegistry};
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
fn vulnerability_window_timestamp_modes_and_flex_pharolux_low_power_guards() {
    let ctx = test_ctx();
    let hub_id = 1_085_000_001_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);

    assert_eq!(
        required_upgrade_for_flex_structure(TYPE_PHAROLUX_CYNO_BEACON),
        Some(UPGRADE_CYNOSURAL_NAVIGATION)
    );

    // MissingSovereigntyHub when no hub in null_sys
    assert_eq!(
        require_flex_sov_upgrade_online(&ctx, null_sys, TYPE_PHAROLUX_CYNO_BEACON)
            .unwrap_err()
            .code,
        "MissingSovereigntyHub"
    );

    ctx.db.with_mut_snapshot(|snap| {
        // Missing structure -> apply_adm_scaled returns None
        assert!(apply_adm_scaled_vulnerability_window_in_snap(snap, 999_999, 12).is_none());

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Window Hub".into(),
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        // (0, 0) unconfigured window -> always vulnerable
        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 0));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 12_345));

        // Explicit unix timestamp window (> 86_400)
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (1_700_000_000, 1_700_010_000));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 1_700_005_000));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 1_700_020_000));

        // Non-wrapping daily window [10:00, 14:00]
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (10 * 3_600, 14 * 3_600));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 12 * 3_600));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 8 * 3_600));

        // Install Pharolux upgrade in low_power mode -> RequiredSovUpgradeOffline
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.upgrades.push(PersistedHubUpgrade {
            type_id: UPGRADE_CYNOSURAL_NAVIGATION,
            online: true,
            low_power: true,
        });
    });

    assert_eq!(
        require_flex_sov_upgrade_online(&ctx, null_sys, TYPE_PHAROLUX_CYNO_BEACON)
            .unwrap_err()
            .code,
        "RequiredSovUpgradeOffline"
    );
}

#[tokio::test]
async fn precheck_entosis_activation_all_error_branches_and_range_boundary() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let att_char = 90_000_001_i64;
    let no_ally_char = 90_888_001_i64;
    let att_ally = 99_000_301_i64;
    let def_ally = 99_000_302_i64;
    let hub_id = 1_085_000_010_i64;
    let ship_id = 1_085_000_011_i64;

    // 1. Non-sov target returns Ok(false)
    assert_eq!(
        precheck_entosis_activation(&ctx, 999_999_999, ship_id, att_char)
            .await
            .unwrap(),
        false
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&att_char) {
            ch.alliance_id = Some(att_ally);
        }
        let mut c2 = snap.characters[&att_char].clone();
        c2.character_id = no_ally_char;
        c2.alliance_id = None;
        c2.corporation_id = 1_000_009;
        snap.characters.insert(no_ally_char, c2);

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
                name: "Precheck Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "HullReinforced".into(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );
    });

    // 2. CharNotFound & MustBeInAlliance
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, 777_777_777)
            .await
            .unwrap_err()
            .code,
        "CharNotFound"
    );
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, no_ally_char)
            .await
            .unwrap_err()
            .code,
        "MustBeInAlliance"
    );

    // 3. SovHubAlreadyReinforced
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, att_char)
            .await
            .unwrap_err()
            .code,
        "SovHubAlreadyReinforced"
    );

    // 4. SovHubInvulnerableOutsideWindow
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().state = "ShieldVulnerable".into();
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (1_900_000_000, 1_900_003_600));
    });
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, att_char)
            .await
            .unwrap_err()
            .code,
        "SovHubInvulnerableOutsideWindow"
    );

    // 5. ShipNotInSpace & TargetOutOfEntosisRange (250,000.1 m fails, 250,000.0 m passes)
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 0));
    });
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, att_char)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(250_000.1, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, att_char)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfEntosisRange"
    );

    sys_h
        .set_ball_position(ship_id, DVec3::new(250_000.0, 0.0, 0.0))
        .await;
    assert!(precheck_entosis_activation(&ctx, hub_id, ship_id, att_char)
        .await
        .unwrap());
}

#[tokio::test]
async fn defender_counter_entosis_decay_node_respawn_and_hub_loot_drop() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let att_char = 90_000_001_i64;
    let def_char = 90_000_002_i64;
    let att_ally = 99_000_401_i64;
    let def_ally = 99_000_402_i64;
    let hub_id = 1_085_000_100_i64;
    let node_mid = 1_085_000_101_i64;
    let node_win = 1_085_000_102_i64;
    let att_ship = 1_085_000_103_i64;
    let def_ship = 1_085_000_104_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&att_char) {
            ch.alliance_id = Some(att_ally);
            ch.solar_system_id = sys_id;
        }
        let mut def = snap.characters[&att_char].clone();
        def.character_id = def_char;
        def.alliance_id = Some(def_ally);
        snap.characters.insert(def_char, def);

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(def_ally);
        snap.sov_runtime
            .sov_claimed_since_unix
            .insert(sys_id, now.saturating_sub(105 * 86_400));

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_099,
                solar_system_id: sys_id,
                name: "Campaign Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "HullReinforced".into(),
                fuel_blocks: 101,
                ..Default::default()
            },
        );
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.fuel.insert(4247, 101);
        hub.upgrades = vec![
            PersistedHubUpgrade {
                type_id: UPGRADE_ADVANCED_LOGISTICS_NETWORK,
                online: true,
                low_power: false,
            },
            PersistedHubUpgrade {
                type_id: UPGRADE_CYNOSURAL_NAVIGATION,
                online: true,
                low_power: false,
            },
        ];

        // Seed unattended partial capture on hub_id (ship_id = 0) -> decays over time
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (att_char, att_ally, 0, 100.0, now));

        // Seed node_mid at 50% score -> capturing respawns up to 4 constellation nodes
        snap.sov_runtime.command_nodes.insert(node_mid, (hub_id, sys_id, 0));
        snap.sov_runtime.command_node_scores.insert(hub_id, vec![(def_ally, 0.50)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_mid, (att_char, att_ally, att_ship, 5_000.0, now));
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for id in [att_ship, def_ship, node_mid, node_win] {
        sys_h
            .spawn_ball(Ball {
                id,
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
            .await
            .unwrap();
    }

    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, att_ship),
            ActiveEntosisLinkState {
                module_id: 88_001,
                char_id: att_char,
                ship_id: att_ship,
                side_id: att_ally,
                target_id: node_mid,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }

    // Tick via tick_sov_gameplay: decays unattended hub progress, updates strategic_index to 5, and captures node_mid!
    tick_sov_gameplay(&ctx, now + 100).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.system_states[&sys_id].strategic_index, 5);
        assert!(snap.sov_runtime.entosis_capture_progress[&hub_id].3 < 100.0);
        // Intermediate node capture spawned replacement nodes so total active command_nodes == 4
        assert_eq!(snap.sov_runtime.command_nodes.len(), 4);
    });

    // Now set score to 0.02 and capture node_win as attacker -> destroys hub and drops 50% upgrades & (101+1)/2 = 51 fuel!
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.command_nodes.insert(node_win, (hub_id, sys_id, 0));
        snap.sov_runtime.command_node_scores.insert(hub_id, vec![(def_ally, 0.02)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_win, (att_char, att_ally, att_ship, 5_000.0, now + 100));
    });
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, att_ship),
            ActiveEntosisLinkState {
                module_id: 88_001,
                char_id: att_char,
                ship_id: att_ship,
                side_id: att_ally,
                target_id: node_win,
                cycle_duration_s: 300.0,
                started_unix: now + 100,
                expires_unix: now + 400,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    tick_sov_gameplay(&ctx, now + 120).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&hub_id].state, "Destroyed");
        assert_eq!(snap.system_states[&sys_id].sov_alliance_id, None);
        // Verify wreck dropped UPGRADE_ADVANCED_LOGISTICS_NETWORK (idx 0) and 51 fuel blocks (4247)
        assert!(snap.items.values().any(|i| i.type_id == UPGRADE_ADVANCED_LOGISTICS_NETWORK));
        assert!(snap.items.values().any(|i| i.type_id == 4247 && i.quantity == 51));
    });
}

#[test]
fn rat_kills_mining_activity_and_anomaly_npc_site_clearing() {
    let mut ctx = test_ctx();
    let npc_tid = 919_701_u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut t: InvType = sde.types.values().next().unwrap().clone();
        t.type_id = npc_tid;
        t.category_id = 11; // Entity / NPC
        sde.types.insert(npc_tid, t);
    }

    let sys_id = 30_000_142_u32;
    let hub_id = 1_085_000_999_i64;
    let upg_tid = UPGRADE_MAJOR_THREAT_DETECTION;
    let base = 1_940_000_000 + (hub_id.unsigned_abs() as i64 % 100_000) * 100 + i64::from(upg_tid % 100) * 10;
    let npc_ball_id = base + 1;
    let sig_id = format!("SOV-{hub_id}-{upg_tid}");

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_555);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.upgrades.push(PersistedHubUpgrade {
            type_id: upg_tid,
            online: true,
            low_power: false,
        });
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: sig_id.clone(),
                solar_system_id: sys_id,
                name: "Sov Combat Site".into(),
                anomaly_type: "Combat".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: 1000,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.sov_runtime.sov_upgrade_combat_npcs.insert(npc_ball_id);
    });

    // Pod (670) and player structure are ignored
    on_space_entity_destroyed(&ctx, sys_id, Some(90_000_001), 111, 670);
    on_space_entity_destroyed(&ctx, sys_id, Some(90_000_001), hub_id, 587);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&sys_id).copied().unwrap_or(0)),
        0
    );

    // Destroying tracked NPC clears the anomaly, sets 900s respawn delay, and increments military index
    on_space_entity_destroyed(&ctx, sys_id, Some(90_000_001), npc_ball_id, npc_tid);
    record_sov_system_activity(&ctx, sys_id, 500, 100_000.0);
    record_rat_kill_military_index(&ctx, sys_id, 999_001, npc_tid, true);

    ctx.db.with_snapshot(|snap| {
        let anom = &snap.cosmic_anomalies[&sys_id][0];
        assert!(anom.cleared);
        assert!(anom.created_unix > 1000);
        let st = &snap.system_states[&sys_id];
        assert_eq!(snap.sov_runtime.sov_npc_kills[&sys_id], 502);
        assert_eq!(st.military_index, 5);
        assert_eq!(st.industrial_index, 5);
    });
}
