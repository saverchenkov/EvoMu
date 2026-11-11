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

fn find_claimable_null_planet(ctx: &ServerContext) -> Option<(u32, u32, glam::DVec3)> {
    for sys in ctx.sde.solar_systems.values() {
        if crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id) {
            let cels = ctx.sde.celestials_in_system(sys.solar_system_id);
            if let Some(p) = cels.iter().find(|c| c.group_id == 7 && c.type_id == 11) {
                return Some((sys.solar_system_id, p.celestial_id as u32, p.position));
            }
            if let Some(p) = cels.iter().find(|c| c.group_id == 7) {
                return Some((sys.solar_system_id, p.celestial_id as u32, p.position));
            }
        }
    }
    None
}

#[tokio::test]
async fn anchor_mercenary_den_component_damage_repair_and_special_structure_paths() {
    use crate::services::sov_gameplay::{
        apply_mercenary_den_shield_repair, apply_mercenary_den_weapon_damage,
        dispatch_special_structure_attack, handle_anchor_mercenary_den_rpc,
        handle_mercden_component_call, handle_normal_structure_damage_aftermath,
    };
    use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::{HashMap, HashSet};

    let ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let (sys_id, planet_id, planet_pos) =
        find_claimable_null_planet(&ctx).unwrap_or((30_004_759, 40_301_001, DVec3::ZERO));
    let highsec_sys_id: u32 = 30_000_142;
    let char_id = 93_100_001i64;
    let other_char_id = 93_100_002i64;
    let corp_id = 98_200_001i64;
    let ship_id = 10_931_000_001i64;
    let den_item_id = 10_931_000_002i64;
    let locked_den_id = 10_931_000_003i64;
    let locked_container_id = 10_931_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "MercDenPilot".to_string();
        c.corporation_id = corp_id;
        c.solar_system_id = highsec_sys_id;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "SovCorp".to_string();
        corp.ticker = "SOVC".to_string();
        corp.ceo_id = char_id;
        corp.members = HashSet::from([char_id]);
        corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Locked container + merc den inside it
        snap.items.insert(
            locked_container_id,
            InventoryItemRecord {
                item_id: locked_container_id,
                type_id: 23,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_den_id,
            InventoryItemRecord {
                item_id: locked_den_id,
                type_id: 85230,
                owner_id: char_id,
                location_id: locked_container_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            den_item_id,
            InventoryItemRecord {
                item_id: den_item_id,
                type_id: 85230,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::new(72_001);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    session.solar_system_id = Some(highsec_sys_id);
    session.station_id = Some(60_003_760);
    session.ship_id = Some(ship_id);

    assert!(handle_anchor_mercenary_den_rpc(&ctx, &mut session, &[]).await.is_err());
    assert!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut session, &[PyRep::Integer(den_item_id)])
            .await
            .is_err()
    );
    session.station_id = None;
    assert!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut session, &[PyRep::Integer(den_item_id)])
            .await
            .is_err()
    );

    session.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(planet_pos.x + 150_000.0, planet_pos.y, planet_pos.z),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut session, &[PyRep::Integer(locked_den_id)])
            .await
            .is_err()
    );

    let anchor_res = handle_anchor_mercenary_den_rpc(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(den_item_id),
            PyRep::String("TestMercDen".to_string()),
            PyRep::String("Desc".to_string()),
            PyRep::Real(planet_pos.x + 150_000.0),
            PyRep::Real(planet_pos.y),
            PyRep::Real(planet_pos.z),
            PyRep::Integer(i64::from(planet_id)),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await;

    let deployed_den_id = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.mercenary_dens.keys().next().copied());
    if let (Ok(_), Some(real_den_id)) = (&anchor_res, deployed_den_id) {
        let _ = handle_mercden_component_call(
            &ctx,
            &session,
            sys_id,
            other_char_id,
            ship_id,
            real_den_id,
            "mercenaryDen",
            "GetMercenaryDenInfo",
            &[],
        )
        .await;
        let _ = handle_mercden_component_call(
            &ctx,
            &session,
            sys_id,
            char_id,
            ship_id,
            real_den_id,
            "mercenaryDen",
            "extract_infomorphs_to_ship",
            &[],
        )
        .await;
        let _ = handle_mercden_component_call(
            &ctx,
            &session,
            sys_id,
            char_id,
            ship_id,
            real_den_id,
            "mercenaryDen",
            "UnknownMercMethod",
            &[],
        )
        .await;

        let _ = apply_mercenary_den_weapon_damage(&ctx, real_den_id, 5_000.0).await;
        let _ = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, real_den_id, 100.0).await;
        let _ = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, real_den_id, 1_000_000.0).await;
        let _ = dispatch_special_structure_attack(&ctx, char_id, real_den_id, 85230, false, 100.0, 1, &[]).await;
        let _ = apply_mercenary_den_weapon_damage(&ctx, real_den_id, 100_000_000.0).await;
        let _ = apply_mercenary_den_weapon_damage(&ctx, real_den_id, 100_000_000.0).await;
    }

    let _ = apply_mercenary_den_weapon_damage(&ctx, 999_888_777, 100.0).await;
    let _ = apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, 999_888_777, 100.0).await;
    handle_normal_structure_damage_aftermath(
        &ctx,
        highsec_sys_id,
        0.9,
        char_id,
        ship_id,
        35832,
        Default::default(),
        true,
        1_700_000_000,
    );
}

#[tokio::test]
async fn sov_hub_anchor_skyhook_confirm_data_hack_and_tick_entosis_lifecycle() {
    use crate::services::sov_gameplay::{
        handle_anchor_sov_hub_rpc, handle_confirm_skyhook_sov_management, try_handle_sov_hub_data_hack,
    };
    use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::{HashMap, HashSet};

    let ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let (sys_id, _planet_id, _) =
        find_claimable_null_planet(&ctx).unwrap_or((30_004_759, 40_301_001, DVec3::ZERO));
    let char_id = 93_100_050i64;
    let corp_id = 98_200_050i64;
    let alliance_id = 99_200_050i64;
    let ship_id = 10_931_000_050i64;
    let hub_item_id = 10_931_000_051i64;
    let mod_id = 10_931_000_052i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "SovCEO".to_string();
        c.corporation_id = corp_id;
        c.alliance_id = Some(alliance_id);
        c.solar_system_id = sys_id;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "AllianceSovCorp".to_string();
        corp.ticker = "ASOV".to_string();
        corp.ceo_id = char_id;
        corp.alliance_id = Some(alliance_id);
        corp.members = HashSet::from([char_id]);
        corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
        snap.corporations.insert(corp_id, corp);

        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "AllianceSov".to_string(),
                short_name: "ASOV".to_string(),
                executor_corp_id: corp_id,
                ..Default::default()
            },
        );

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            hub_item_id,
            InventoryItemRecord {
                item_id: hub_item_id,
                type_id: 32458,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
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
        })
        .await;

    let mut session = SessionState::new(72_002);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.alliance_id = Some(alliance_id);
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let _ = handle_anchor_sov_hub_rpc(&ctx, &mut session, &[]).await;
    assert!(
        handle_anchor_sov_hub_rpc(&ctx, &mut session, &[PyRep::Integer(99_999_888)])
            .await
            .is_err()
    );
    let _ = handle_anchor_sov_hub_rpc(
        &ctx,
        &mut session,
        &[PyRep::Integer(hub_item_id), PyRep::String("Unit Hub".to_string())],
    )
    .await;

    assert!(handle_confirm_skyhook_sov_management(&ctx, &session, &[]).await.is_err());
    let _ = handle_confirm_skyhook_sov_management(
        &ctx,
        &session,
        &[PyRep::Integer(99_887_766), PyRep::Integer(25)],
    )
    .await;

    let data_mod = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 538 && !t.name.contains("Relic"))
        .cloned()
        .unwrap_or_else(|| ctx.sde.types.values().next().unwrap().clone());
    assert!(
        try_handle_sov_hub_data_hack(&ctx, char_id, 999_111_222, mod_id, &data_mod, 50.0, 20.0)
            .is_none()
    );
    if let Some(hub_id) = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.sov_hubs.keys().next().copied())
    {
        let _ = try_handle_sov_hub_data_hack(&ctx, char_id, hub_id, mod_id, &data_mod, 50.0, 20.0);
        ctx.db.with_snapshot(|snap| {
            let _ = is_hub_within_vulnerability_window_in_snap(snap, hub_id, 1_700_000_000);
        });
    }

    tick_sov_gameplay(&ctx, 1_700_000_000).await;
    tick_sov_gameplay(&ctx, 1_700_086_500).await;
}

#[tokio::test]
async fn mercden_anchor_extract_repair_special_damage_and_aftermath_full_matrix() {
    use crate::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
    use crate::services::sov_gameplay::{
        apply_mercenary_den_shield_repair, dispatch_special_structure_attack,
        handle_anchor_mercenary_den_rpc, handle_mercden_component_call,
        handle_normal_structure_damage_aftermath, is_claimable_nullsec_system,
        TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
    };
    use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_TENEBREX_CYNO_JAMMER};
    use eve_db::{
        ContractRecord, InventoryFlag, InventoryItemRecord, ServerClock, TrainedSkillRecord,
        VirtualServerClock,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let mut sde = (*test_ctx().sde).clone();
    if let Some(seed_t) = sde.types.get(&648).cloned().or_else(|| sde.types.values().next().cloned()) {
        let mut t = seed_t.clone();
        t.type_id = 648;
        t.capacity = 5_000.0;
        t.volume = 25_000.0;
        sde.types.insert(648, t);

        let mut inf = seed_t;
        inf.type_id = TYPE_INFOMORPH_DECRYPTION_KEY;
        inf.volume = 10.0;
        inf.name = "Infomorph Decryption Key".into();
        sde.types.insert(TYPE_INFOMORPH_DECRYPTION_KEY, inf);
    }
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);

    // Verify is_claimable_nullsec_system negative & positive branches
    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_000_142)); // Jita highsec
    assert!(!is_claimable_nullsec_system(&ctx.sde, 31_000_001)); // Wormhole
    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_100_000)); // Zarzakh
    assert!(!is_claimable_nullsec_system(&ctx.sde, 999_999_999)); // Nonexistent
    if let Some(poch_sys) = ctx.sde.solar_systems.values().find(|s| s.region_id == 10_000_070) {
        assert!(!is_claimable_nullsec_system(&ctx.sde, poch_sys.solar_system_id));
    }
    if let Some(npc_null) = ctx.sde.solar_systems.values().find(|s| {
        s.security_status <= 0.0
            && s.solar_system_id < 31_000_000
            && s.region_id != 10_000_070
            && (ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_some()
                || ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_some())
    }) {
        assert!(!is_claimable_nullsec_system(&ctx.sde, npc_null.solar_system_id));
    }

    // Find claimable nullsec system with BOTH temperate planet (type 11) and non-temperate planet if available
    let mut picked = None;
    for sys in ctx.sde.solar_systems.values() {
        if is_claimable_nullsec_system(&ctx.sde, sys.solar_system_id) {
            let cels = ctx.sde.celestials_in_system(sys.solar_system_id);
            if let Some(temp_p) = cels.iter().find(|c| c.type_id == 11) {
                let non_temp_id = cels
                    .iter()
                    .find(|c| c.type_id != 11)
                    .map(|c| c.celestial_id)
                    .unwrap_or(temp_p.celestial_id + 99_999);
                picked = Some((sys.solar_system_id, temp_p.celestial_id, temp_p.position, non_temp_id));
                break;
            }
        }
    }
    let (sys_id, temp_planet_id, temp_pos, non_temp_planet_id) =
        picked.expect("SDE must contain a claimable nullsec system with a temperate planet");
    assert!(is_claimable_nullsec_system(&ctx.sde, sys_id));

    let char_id = 94_500_001i64;
    let other_char_id = 94_500_002i64;
    let corp_id = 98_500_001i64;
    let alliance_id = 99_500_001i64;
    let ship_id = 1_945_000_010i64;
    let skyhook_id = 1_945_000_100i64;
    let skyhook2_id = 1_945_000_101i64;
    let den_item_id = 1_945_000_200i64;
    let den_item_2 = 1_945_000_201i64;
    let den_item_other = 1_945_000_202i64;
    let den_item_locked = 1_945_000_203i64;
    let charge_id = 1_945_000_300i64;
    let cargo_filler_id = 1_945_000_301i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = snap.characters.values().next().cloned().expect("seed char");
        c1.character_id = char_id;
        c1.corporation_id = corp_id;
        c1.alliance_id = Some(alliance_id);
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.active_ship_id = ship_id;
        snap.characters.insert(char_id, c1.clone());

        let mut c2 = c1;
        c2.character_id = other_char_id;
        snap.characters.insert(other_char_id, c2);

        let mk_it = |item_id, type_id, owner_id, location_id, flag, qty| InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: qty,
            singleton: false,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk_it(ship_id, 648, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        snap.items.insert(den_item_id, mk_it(den_item_id, TYPE_MERCENARY_DEN, char_id, ship_id, InventoryFlag::Cargo, 2));
        snap.items.insert(den_item_2, mk_it(den_item_2, TYPE_MERCENARY_DEN, char_id, 60_003_760, InventoryFlag::Hangar, 1));
        snap.items.insert(den_item_other, mk_it(den_item_other, TYPE_MERCENARY_DEN, other_char_id, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(den_item_locked, mk_it(den_item_locked, TYPE_MERCENARY_DEN, char_id, ship_id, InventoryFlag::Cargo, 1));
        snap.items.insert(charge_id, mk_it(charge_id, 178, char_id, ship_id, InventoryFlag::HiSlot0, 5));

        let mut locked_contract = ContractRecord::default();
        locked_contract.contract_id = 88_900_001;
        locked_contract.status = "Outstanding".into();
        locked_contract.item_ids.push(den_item_locked);
        snap.contracts.insert(88_900_001, locked_contract);

        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Skyhook".into(),
                position: [temp_pos.x, temp_pos.y, temp_pos.z],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState::new(94_500);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Anchor guards: docked, no sys, no ship, ship not in space
    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipNotInSpace");
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipNotInSpace");

    // Spawn ship far from skyhook
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: temp_pos + DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess.solar_system_id = Some(30_000_142);
    let _ = ctx.systems
        .get_or_spawn(30_000_142)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SovereigntyNullsecRequired");
    sess.solar_system_id = Some(sys_id);

    // SkyhookNotFound -> restore skyhook -> MercenaryDenTooFarFromSkyhook
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&skyhook_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SkyhookNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&skyhook_id).unwrap().state = "ShieldVulnerable".into();
    });
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "MercenaryDenTooFarFromSkyhook");

    // Move ship near skyhook; test SkyhookPlanetUnknown -> DenPlanetMismatch -> PlanetNotTemperate -> SovereigntyNotClaimed -> CapExceeded
    let _ = sys_handle.set_ball_position(ship_id, temp_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SkyhookPlanetUnknown");
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet_id)]).await.unwrap_err().code, "PlanetNotTemperate");
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet_id);
    });
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet_id)]).await.unwrap_err().code, "DenPlanetMismatch");
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)]).await.unwrap_err().code, "SovereigntyNotClaimed");
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(alliance_id);
    });
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)]).await.unwrap_err().code, "MercenaryDenCapExceeded");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, skill_points: 256_000.0, level: 2 },
        );
    });

    // Item ownership/cargo/locked/missing guards
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(999_888_777)]).await.unwrap_err().code, "MissingMercenaryDenItem");
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_other)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_2)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_locked)]).await.unwrap_err().code, "ItemLockedInContractOrJob");

    // Anchor success via 10-arg form
    let anchor_args = vec![
        PyRep::Integer(i64::from(TYPE_MERCENARY_DEN)),
        PyRep::Real(temp_pos.x + 1_500.0),
        PyRep::Real(temp_pos.z),
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::String("Custom Merc Den".into()),
        PyRep::String(String::new()),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Dict(vec![
            (PyRep::String("skyhookID".into()), PyRep::Integer(skyhook_id)),
            (PyRep::String("planetID".into()), PyRep::Integer(temp_planet_id)),
        ]),
    ];
    let anchor_ok = handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &anchor_args).await.expect("anchor den");
    let den_id = anchor_ok.dict_get("denID").and_then(|v| v.as_i64()).expect("denID");
    assert_eq!(
        handle_anchor_mercenary_den_rpc(&ctx, &mut sess, &[PyRep::Integer(den_item_id)]).await.unwrap_err().code,
        "MercenaryDenAlreadyAnchoredOnSkyhook"
    );

    // 2. Component extraction & guards
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "bad_method", &[]).await.unwrap_err().code,
        "UnsupportedComponentCall"
    );
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id + 99, den_id, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "ShipNotInSpace"
    );
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id + 999, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "MercenaryDenNotFound"
    );
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, other_char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "NotMercenaryDenOwner"
    );
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "NoInfomorphsAvailable"
    );

    // Seed infomorphs + tax, reinforce then repair, then extract partial & full
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().infomorphs = 10;
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_id, 20);
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (1_850_010_000, 0, 20));
        snap.structures.get_mut(&den_id).unwrap().shield_hp = 1_000.0;
    });
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "MercenaryDenReinforced"
    );
    assert_eq!(apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 600.0).await.unwrap_err().code, "MercenaryDenStillInReinforcement");

    clock.step_tick(20_000);
    // Partial repair < 50% then full restore >= 50%
    assert_eq!(apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 500.0).await.unwrap(), (30, false));
    assert_eq!(apply_mercenary_den_shield_repair(&ctx, sys_id, ship_id, den_id, 3_000.0).await.unwrap(), (90, true));

    // NotEnoughCargoSpace when cargo full -> clear filler -> extract with tax
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_filler_id,
            InventoryItemRecord {
                item_id: cargo_filler_id,
                type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[]).await.unwrap_err().code,
        "NotEnoughCargoSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&cargo_filler_id);
    });
    assert!(handle_mercden_component_call(&ctx, &sess, sys_id, char_id, ship_id, den_id, "mercenaryDen", "extract_infomorphs_to_ship", &[]).await.is_ok());

    // 3. Dispatch special structure attack & normal structure damage aftermath
    assert!(dispatch_special_structure_attack(&ctx, char_id, den_id, TYPE_MERCENARY_DEN, false, 100.0, 5, &[charge_id]).await.is_ok());
    assert!(dispatch_special_structure_attack(&ctx, char_id, skyhook_id, TYPE_ORBITAL_SKYHOOK, false, 100.0, 6, &[]).await.is_ok());
    assert_eq!(
        dispatch_special_structure_attack(&ctx, char_id, skyhook2_id, 35832, false, 100.0, 7, &[]).await.unwrap_err().code,
        "InvalidSpecialStructure"
    );
    handle_normal_structure_damage_aftermath(&ctx, 30_000_142, 0.9, char_id, ship_id, TYPE_TENEBREX_CYNO_JAMMER, "ArmorReinforced", true, ctx.now_unix());
}

#[tokio::test]
async fn tick_sov_gameplay_spawns_despawns_mto_upgrades_tenebrex_and_rat_kills() {
    use crate::services::sov_gameplay::is_claimable_nullsec_system;
    use crate::services::sov_services::{
        TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_TENEBREX_CYNO_JAMMER,
    };
    use eve_db::durable_domain_types::PersistedMercenaryActivity;
    use eve_db::{
        InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, PersistedSovHubData,
        VirtualServerClock,
    };
    use eve_destiny::CombatState;

    let mut sde = (*test_ctx().sde).clone();
    let seed_up = sde
        .types
        .values()
        .find(|t| t.group_id == 4768)
        .cloned()
        .unwrap_or_else(|| sde.types.values().next().cloned().unwrap());
    let mk_up = |id: u32, name: &str| {
        let mut u = seed_up.clone();
        u.type_id = id;
        u.group_id = 4768;
        u.name = name.into();
        u
    };
    sde.types.insert(88_901, mk_up(88_901, "Major Threat Detection Array 3"));
    sde.types.insert(88_902, mk_up(88_902, "Tritanium Prospecting Array 3"));
    sde.types.insert(88_903, mk_up(88_903, "Exploration Detector II"));
    // Also ensure NPC + ore types exist in SDE
    let seed_any = sde.types.values().next().cloned().unwrap();
    for tid in [37_453u32, 24_690, 1230, 18, 28_729, 28_730] {
        sde.types.entry(tid).or_insert_with(|| {
            let mut t = seed_any.clone();
            t.type_id = tid;
            t.category_id = if tid == 37_453 || tid == 24_690 { 11 } else { 25 };
            t.volume = 1.0;
            t
        });
    }
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_900_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id) && s.security_status <= -0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let hub_id = 1_960_000_001i64;
    let den_id = 1_960_000_002i64;
    let mto_ball_id = 1_960_000_050i64;
    let jammer_id = 1_960_000_060i64;
    let player_ship_id = 1_960_000_070i64;
    let now = 1_900_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.sov_alliance_id = Some(99_600_001);
        st.strategic_index = 0;

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_600_001,
                solar_system_id: sys_id,
                name: "Cov Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            jammer_id,
            UpwellStructureRecord {
                structure_id: jammer_id,
                type_id: TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: 98_600_001,
                solar_system_id: sys_id,
                name: "Tenebrex".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.tenebrex_spool_ready_unix.insert(jammer_id, now - 10);
        snap.sov_runtime.sov_claimed_since_unix.insert(sys_id, now - 40 * 86_400);

        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![
                    PersistedHubUpgrade { type_id: 88_901, online: true, low_power: false },
                    PersistedHubUpgrade { type_id: 88_902, online: true, low_power: false },
                    PersistedHubUpgrade { type_id: 88_903, online: true, low_power: false },
                ],
                fuel: Default::default(),
                workforce: Default::default(),
                last_updated_unix: now,
            },
        );

        // Anchor Mercenary Den + MTO site ball + activity
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_char_id: 94_500_001,
                skyhook_id: 1_960_000_099,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 1,
                anarchy_level: 0,
                infomorphs: 0,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3600,
            },
        );
        let mut act = PersistedMercenaryActivity {
            uuid: vec![1; 16],
            den_id,
            solar_system_id: sys_id,
            dungeon_id: 1,
            started: true,
            started_by: 94_500_001,
            expiry_unix: now + 3_600,
            development_impact: 5,
            anarchy_impact: -2,
            infomorph_bonus: 3,
        };
        let key = "01".repeat(16);
        snap.sov_runtime.mercenary_activities.insert(key.clone(), act.clone());
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id, key);
        let _ = &mut act;
        let _ = TYPE_ORBITAL_SKYHOOK;

        snap.items.insert(
            player_ship_id,
            InventoryItemRecord {
                item_id: player_ship_id,
                type_id: 587,
                owner_id: 94_500_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Tick 1: spawns Ore / Combat / Data / Relic sites & MTO ball + emits CynoJammer + DevIndex
    tick_sov_gameplay(&ctx, now).await;

    // Destroy player ship / pod (ignored) and all upgrade combat NPCs & MTO ball -> tests on_space_entity_destroyed_ext + record_rat_kill_military_index
    on_space_entity_destroyed_ext(&ctx, sys_id, Some(94_500_001), player_ship_id, 670, false);
    on_space_entity_destroyed_ext(&ctx, sys_id, Some(94_500_001), player_ship_id, 587, false);
    let upg_npcs: Vec<i64> = ctx.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.iter().copied().collect());
    assert!(!upg_npcs.is_empty());
    for nid in upg_npcs {
        on_space_entity_destroyed_ext(&ctx, sys_id, Some(94_500_001), nid, 37_453, true);
    }
    on_space_entity_destroyed_ext(&ctx, sys_id, Some(94_500_001), mto_ball_id, 24_690, true);

    // Tick after combat respawn delay -> respawns combat NPCs, then mark hub hacked -> tick despawns -> expire_hacked_hubs restores
    tick_sov_gameplay(&ctx, now + SOV_COMBAT_ANOMALY_RESPAWN_SECS + 5).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.insert(hub_id, now + SOV_COMBAT_ANOMALY_RESPAWN_SECS + 500);
        snap.structures.get_mut(&hub_id).unwrap().state = "ArmorReinforced".into();
        for u in &mut snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap().upgrades {
            u.low_power = true;
        }
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id + 1, "expired_key".into());
        let _ = CombatState::default();
    });
    tick_sov_gameplay(&ctx, now + SOV_COMBAT_ANOMALY_RESPAWN_SECS + 10).await;
    tick_sov_gameplay(&ctx, now + SOV_COMBAT_ANOMALY_RESPAWN_SECS + 600).await;
    let hub_state = ctx.db.with_snapshot(|s| s.structures.get(&hub_id).unwrap().state.clone());
    assert_eq!(hub_state, "ShieldVulnerable");
}

#[tokio::test]
async fn sov_gameplay_weapon_damage_loot_drop_and_anchor_hub_validation_matrix() {
    use crate::services::sov_gameplay::{
        apply_mercenary_den_weapon_damage, handle_anchor_sov_hub_rpc,
        is_claimable_nullsec_system, TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
    };
    use crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
    use eve_db::{
        ContractRecord, InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, ServerClock,
        VirtualServerClock,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let mut sde = (*test_ctx().sde).clone();
    if let Some(seed) = sde.types.values().next().cloned() {
        for tid in [TYPE_SOVEREIGNTY_HUB, TYPE_MERCENARY_DEN, TYPE_INFOMORPH_DECRYPTION_KEY] {
            sde.types.entry(tid).or_insert_with(|| {
                let mut t = seed.clone();
                t.type_id = tid;
                t.volume = 1.0;
                t
            });
        }
    }
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_910_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let char_id = 94_700_001i64;
    let other_char_id = 94_700_002i64;
    let corp_id = 98_700_001i64;
    let ally_id = 99_700_001i64;
    let ship_id = 1_947_000_010i64;
    let den_destroyed = 1_947_000_100i64;
    let den_expired = 1_947_000_101i64;
    let den_live = 1_947_000_102i64;
    let hub_other = 1_947_000_200i64;
    let hub_station = 1_947_000_201i64;
    let hub_locked = 1_947_000_202i64;
    let hub_ok = 1_947_000_203i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.corporation_id = corp_id;
        c.alliance_id = Some(ally_id);
        c.solar_system_id = sys_id;
        c.station_id = None;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.alliance_id = Some(ally_id);
        corp.ceo_id = other_char_id;
        snap.corporations.insert(corp_id, corp);

        let mk_den_struct = |id, state: &str, shield: f64, hull: f64| UpwellStructureRecord {
            structure_id: id,
            type_id: TYPE_MERCENARY_DEN,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "Merc Den Cov".into(),
            position: [1_000.0, 0.0, 0.0],
            state: state.into(),
            quantum_core_installed: true,
            fuel_blocks: 50,
            shield_hp: shield,
            armor_hp: 5_000.0,
            hull_hp: hull,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        snap.structures.insert(den_destroyed, mk_den_struct(den_destroyed, "Destroyed", 0.0, 0.0));
        snap.structures.insert(den_expired, mk_den_struct(den_expired, "HullReinforced", 0.0, 5_000.0));
        snap.structures.insert(den_live, mk_den_struct(den_live, "ShieldVulnerable", 500.0, 5_000.0));

        snap.sov_runtime.mercenary_den_reinforcement.insert(den_expired, (now.saturating_sub(2_000), now.saturating_sub(10), 0));
        snap.sov_runtime.mercenary_dens.insert(
            den_live,
            PersistedMercenaryDen {
                den_id: den_live,
                type_id: TYPE_MERCENARY_DEN,
                owner_char_id: char_id,
                skyhook_id: 1_947_000_999,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 0,
                anarchy_level: 0,
                infomorphs: 250,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3_600,
            },
        );
        for k in 0..8i64 {
            snap.items.insert(
                1_947_500_000 + k,
                InventoryItemRecord {
                    item_id: 1_947_500_000 + k,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: char_id,
                    location_id: den_live,
                    flag: InventoryFlag::Cargo,
                    quantity: 17 + k as i32,
                    ..Default::default()
                },
            );
        }

        let mk_hub = |id, owner, loc, flag| InventoryItemRecord {
            item_id: id,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_id: owner,
            location_id: loc,
            flag,
            quantity: 1,
            singleton: false,
            ..Default::default()
        };
        snap.items.insert(hub_other, mk_hub(hub_other, other_char_id, ship_id, InventoryFlag::Cargo));
        snap.items.insert(hub_station, mk_hub(hub_station, char_id, 60_003_760, InventoryFlag::Hangar));
        snap.items.insert(hub_locked, mk_hub(hub_locked, char_id, ship_id, InventoryFlag::Cargo));
        snap.items.insert(hub_ok, mk_hub(hub_ok, char_id, ship_id, InventoryFlag::Cargo));

        let mut ct = ContractRecord::default();
        ct.contract_id = 88_947_001;
        ct.status = "Outstanding".into();
        ct.item_ids.push(hub_locked);
        snap.contracts.insert(88_947_001, ct);
    });

    // 1. Mercenary Den weapon damage lifecycle (not found -> destroyed -> expired window reset -> reinf -> partial hull -> destroy + loot)
    assert_eq!(apply_mercenary_den_weapon_damage(&ctx, 99_999_777, 100.0).await.unwrap_err().code, "MercenaryDenNotFound");
    assert_eq!(apply_mercenary_den_weapon_damage(&ctx, den_destroyed, 100.0).await.unwrap_err().code, "StructureDestroyed");
    let reset_res = apply_mercenary_den_weapon_damage(&ctx, den_expired, 500.0).await.expect("expired reset");
    assert_eq!(reset_res.dict_get("state").and_then(PyRep::as_str), Some("ShieldVulnerable"));

    let reinf_res = apply_mercenary_den_weapon_damage(&ctx, den_live, 600.0).await.expect("reinforce");
    assert_eq!(reinf_res.dict_get("state").and_then(PyRep::as_str), Some("HullReinforced"));
    clock.step_tick(86_410);
    let hull_dmg = apply_mercenary_den_weapon_damage(&ctx, den_live, 1_000.0).await.expect("partial hull");
    assert_eq!(hull_dmg.dict_get("state").and_then(PyRep::as_str), Some("HullVulnerable"));
    let kill_res = apply_mercenary_den_weapon_damage(&ctx, den_live, 10_000.0).await.expect("destroy den");
    assert_eq!(kill_res.dict_get("state").and_then(PyRep::as_str), Some("Destroyed"));
    assert!(kill_res.dict_get("droppedStacks").and_then(PyRep::as_i64).unwrap_or(0) > 0);

    // 2. handle_anchor_sov_hub_rpc guards and anchor success
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    let mut sess = SessionState::new(94_700);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(ally_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "CannotAnchorSovHubOutsideNullSec");
    sess.solar_system_id = Some(sys_id);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "CorpRoleDenied");

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ally_id + 1);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "SovereigntyHeldByAnotherAlliance");

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ally_id);
    });
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_other)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_station)]).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_locked)]).await.unwrap_err().code, "ItemLockedInContractOrJob");
    assert!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_ok), PyRep::String("Cov Anchor Hub".into())]).await.is_ok());
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_ok)]).await.unwrap_err().code, "SovHubAlreadyPresent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn precheck_entosis_link_filter_clauses_and_tick_merc_den_precheck_mutants() {
    let ctx = test_ctx();
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_728_043_200)); // multiple of 86_400 + 43_200 (12:00 UTC)
    ctx.set_clock(clock.clone());
    let now = ctx.now_unix();
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_505);
    let hub_id = 1_092_000_001_i64;
    let other_hub_id = 1_092_000_002_i64;
    let non_sov_struct_id = 1_092_000_003_i64;
    let ally_def = 99_100_001_i64;
    let ally_att = 99_100_002_i64;
    let def_char = 95_920_001_i64;
    let att_char = 95_920_002_i64;
    let def_ship = 1_092_100_001_i64;
    let att_ship = 1_092_100_002_i64;

    // 1. Non-sov target returns Ok(false)
    assert!(!precheck_entosis_activation(&ctx, non_sov_struct_id, def_ship, def_char)
        .await
        .expect("non-sov struct"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ally_def);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                position: [100_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        let _ = apply_adm_scaled_vulnerability_window_in_snap(snap, hub_id, 12);
        let template_char = eve_db::WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters.values().next().unwrap().clone());
        let mut def_rec = template_char.clone();
        def_rec.character_id = def_char;
        def_rec.user_id = def_char as u64;
        def_rec.name = "Defender".into();
        def_rec.corporation_id = 98_000_001;
        def_rec.alliance_id = Some(ally_def);
        def_rec.solar_system_id = sys_id;
        def_rec.active_ship_id = def_ship;
        snap.characters.insert(def_char, def_rec);

        let mut att_rec = template_char;
        att_rec.character_id = att_char;
        att_rec.user_id = att_char as u64;
        att_rec.name = "Attacker".into();
        att_rec.corporation_id = 98_000_002;
        att_rec.alliance_id = Some(ally_att);
        att_rec.solar_system_id = sys_id;
        att_rec.active_ship_id = att_ship;
        snap.characters.insert(att_char, att_rec);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos_x) in [(def_ship, 105_000.0), (att_ship, 106_000.0)] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: DVec3::new(pos_x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Seed distractor active links covering every clause of `(k == db_key && l.target_id == target_id)` and `side_id != attacker_alliance`
    let db_key = db_ptr_key(&ctx.db);
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        // Wrong db_key, matching target_id, hostile side_id
        tst.entosis_active_links.insert(
            (db_key.wrapping_add(1), 8_001),
            ActiveEntosisLinkState {
                module_id: 8_001,
                char_id: att_char,
                ship_id: 8_001,
                side_id: ally_att,
                target_id: hub_id,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
        // Matching db_key, wrong target_id, hostile side_id
        tst.entosis_active_links.insert(
            (db_key, 8_002),
            ActiveEntosisLinkState {
                module_id: 8_002,
                char_id: att_char,
                ship_id: 8_002,
                side_id: ally_att,
                target_id: other_hub_id,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
        // Matching db_key and target_id, friendly side_id (NOT attacker!)
        tst.entosis_active_links.insert(
            (db_key, 8_003),
            ActiveEntosisLinkState {
                module_id: 8_003,
                char_id: def_char,
                ship_id: 8_003,
                side_id: ally_def,
                target_id: hub_id,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }

    // Friendly defender still blocked because no matching hostile link on (db_key, hub_id) exists; attacker succeeds!
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, def_ship, def_char)
            .await
            .unwrap_err()
            .code,
        "CannotEntosisFriendlyStructure"
    );
    assert!(precheck_entosis_activation(&ctx, hub_id, att_ship, att_char)
        .await
        .expect("hostile attacker permitted"));

    // Adding a real attacker link on (db_key, hub_id) unlocks counter-entosis for friendly defender
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, att_ship),
            ActiveEntosisLinkState {
                module_id: 8_004,
                char_id: att_char,
                ship_id: att_ship,
                side_id: ally_att,
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
        .expect("contested friendly defense permitted"));

    // 2. Clean WorldRepository::new_empty() isolates tick_sov_gameplay mercenary den prechecks
    let empty_db = Arc::new(eve_db::WorldRepository::new_empty());
    let clean_ctx = ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), empty_db);
    let clean_clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    clean_ctx.set_clock(clean_clock.clone());
    let t_now = clean_ctx.now_unix();
    let den1 = 1_093_000_001_i64;
    let den2 = 1_093_000_002_i64;
    let non_den = 1_093_000_003_i64;
    let den_idle = 1_093_000_004_i64;

    // 2a. Only mercenary_den_reinforcement populated (mercenary_dens empty, reinforced_until_unix == 0)
    clean_ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den1,
            UpwellStructureRecord {
                structure_id: den1,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "HullVulnerable".into(),
                reinforced_until_unix: 0,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den1, (t_now - 10, t_now - 5, 0));
    });
    tick_sov_gameplay(&clean_ctx, t_now).await;
    clean_ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.structures[&den1].state, "ShieldVulnerable");
        snap.structures.remove(&den1);
        snap.sov_runtime.mercenary_den_reinforcement.clear();
    });

    // 2b. mercenary_den_reinforcement & mercenary_dens empty; only structure HullVulnerable + reinforced_until_unix > 0 triggers recovery
    clean_ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den2,
            UpwellStructureRecord {
                structure_id: den2,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "HullVulnerable".into(),
                reinforced_until_unix: t_now - 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            non_den,
            UpwellStructureRecord {
                structure_id: non_den,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "HullVulnerable".into(),
                reinforced_until_unix: t_now - 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            den_idle,
            UpwellStructureRecord {
                structure_id: den_idle,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "HullReinforced".into(),
                reinforced_until_unix: 0,
                ..Default::default()
            },
        );
    });
    tick_sov_gameplay(&clean_ctx, t_now).await;
    clean_ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&den2].state, "ShieldVulnerable");
        assert_eq!(snap.structures[&den2].reinforced_until_unix, 0);
        assert_eq!(snap.structures[&non_den].state, "HullVulnerable");
        assert_eq!(snap.structures[&den_idle].state, "HullReinforced");
    });
}


