//! Comprehensive unit tests covering edge cases for `services::sov_gameplay_entosis_capture`.

use super::*;
use crate::router::ServerContext;
use crate::services::tactical_modules::{db_ptr_key, ActiveEntosisLinkState};
use eve_db::durable_domain_types::{PersistedHubUpgrade, PersistedSovHubData};
use eve_db::{InventoryFlag, InventoryItemRecord, SystemDynamicState, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_ball_pos(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.try_spawn_ball_with_combat(
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
}

fn setup_hub(ctx: &ServerContext, sys_id: u32, hub_id: i64, holder_alliance: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                sov_alliance_id: holder_alliance,
                military_index: 0,
                industrial_index: 0,
                strategic_index: 0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                solar_system_id: sys_id,
                owner_corp_id: 98_000_001,
                name: "Test Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "Online".into(),
                reinforced_until_unix: 0,
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    spawn_ball_pos(ctx, sys_id, hub_id, TYPE_SOVEREIGNTY_HUB, DVec3::ZERO);
}

fn insert_entosis_link(
    ctx: &ServerContext,
    ship_id: i64,
    char_id: i64,
    side_id: i64,
    module_id: i64,
    target_id: i64,
    started_unix: u64,
    expires_unix: u64,
) {
    let db_key = db_ptr_key(&ctx.db);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(module_id);
    });
    let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
    st.entosis_active_links.insert(
        (db_key, ship_id),
        ActiveEntosisLinkState {
            ship_id,
            char_id,
            side_id,
            module_id,
            target_id,
            cycle_index: 1,
            cycle_duration_s: 120.0,
            started_unix,
            expires_unix,
            is_capital: false,
        },
    );
    st.entosis_target_cycles
        .insert((db_key, target_id, ship_id), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_entosis_capture_target_validation_and_overwrite_rules() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let hub_id = 1_000_500_001;
    let holder_alliance = 99_000_001;
    let attacker_alliance = 99_000_002;
    setup_hub(&ctx, sys_id, hub_id, Some(holder_alliance));

    // Unknown char -> no-op
    register_entosis_capture_target(&ctx, hub_id, 5001, 999_999_888, 1000);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // Char with no alliance -> no-op
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = None;
            snap.corporations.entry(ch.corporation_id).and_modify(|c| c.alliance_id = None);
        }
    });
    register_entosis_capture_target(&ctx, hub_id, 5001, 90_000_001, 1000);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // Char in holder's own alliance -> no-op
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(holder_alliance);
        }
    });
    register_entosis_capture_target(&ctx, hub_id, 5001, 90_000_001, 1000);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // Char in attacker alliance -> registers new entry
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(attacker_alliance);
        }
    });
    register_entosis_capture_target(&ctx, hub_id, 5001, 90_000_001, 1000);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.entosis_capture_progress.get(&hub_id).copied(),
            Some((90_000_001, attacker_alliance, 5001, 0.0, 1000))
        );
    });

    // Existing entry with ship_id != 0 -> not overwritten
    register_entosis_capture_target(&ctx, hub_id, 5002, 90_000_001, 2000);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.entosis_capture_progress[&hub_id].2,
            5001
        );
    });

    // Existing entry with ship_id == 0 and accrued <= 1e-6 -> overwritten cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_002, 99_000_099, 0, 0.0, 500));
    });
    register_entosis_capture_target(&ctx, hub_id, 5002, 90_000_001, 2000);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.entosis_capture_progress.get(&hub_id).copied(),
            Some((90_000_001, attacker_alliance, 5002, 0.0, 2000))
        );
    });

    // Existing entry with ship_id == 0, same alliance, partial accrued -> updates ship_id and max warmup
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_002, attacker_alliance, 0, 50.0, 2500));
    });
    register_entosis_capture_target(&ctx, hub_id, 5003, 90_000_001, 2000);
    ctx.db.with_snapshot(|snap| {
        let e = snap.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(e.0, 90_000_001);
        assert_eq!(e.2, 5003);
        assert_eq!(e.4, 2500);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_entosis_captures_contested_unattended_counter_decapture_stront_and_hub_completion() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let hub_id = 1_000_510_001;
    let holder_alliance = 99_000_001;
    let att_alliance = 99_000_002;
    let third_alliance = 99_000_003;

    // 1. Empty progress returns early
    tick_entosis_captures(&ctx, 1_000).await;

    // 2. Unknown / destroyed target is pruned from entosis_capture_progress
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(999_888_777, (90_000_001, att_alliance, 5001, 10.0, 900));
    });
    tick_entosis_captures(&ctx, 1_000).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&999_888_777));
    });

    // Setup Hub & ships
    setup_hub(&ctx, sys_id, hub_id, Some(holder_alliance));
    let ship_a = 100_510_010;
    let ship_b = 100_510_011;
    spawn_ball_pos(&ctx, sys_id, ship_a, 587, DVec3::new(10_000.0, 0.0, 0.0));
    spawn_ball_pos(&ctx, sys_id, ship_b, 587, DVec3::new(10_000.0, 0.0, 0.0));

    // 3. Out-of-range link is removed, and unattended partial progress decays
    spawn_ball_pos(&ctx, sys_id, ship_b, 587, DVec3::new(ENTOSIS_MAX_RANGE_M + 100.0, 0.0, 0.0));
    insert_entosis_link(&ctx, ship_b, 90_000_002, att_alliance, 8001, hub_id, 900, 1_200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_002, att_alliance, ship_b, 100.0, 1_000));
    });
    // now_unix <= last_ts -> skips time update, but still prunes out-of-range ship_b link
    tick_entosis_captures(&ctx, 1_000).await;
    {
        let st = ctx.tactical_state.read().unwrap();
        assert!(!st.entosis_active_links.contains_key(&(db_ptr_key(&ctx.db), ship_b)));
    }
    // Unattended decay at 1_100: regen_rate = 600 / 3000 = 0.2/s -> 100 - 20 = 80.0
    tick_entosis_captures(&ctx, 1_100).await;
    ctx.db.with_snapshot(|snap| {
        let e = snap.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((e.3 - 80.0).abs() < 1e-6);
        assert_eq!(e.2, 0);
    });
    // Full decay removes entry
    tick_entosis_captures(&ctx, 2_000).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // 4. Contested pause when two distinct sides link simultaneously
    spawn_ball_pos(&ctx, sys_id, ship_b, 587, DVec3::new(10_000.0, 0.0, 0.0));
    insert_entosis_link(&ctx, ship_a, 90_000_001, att_alliance, 8002, hub_id, 2_000, 2_500);
    insert_entosis_link(&ctx, ship_b, 90_000_002, holder_alliance, 8003, hub_id, 2_000, 2_500);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_001, att_alliance, ship_a, 120.0, 2_000));
    });
    tick_entosis_captures(&ctx, 2_100).await;
    ctx.db.with_snapshot(|snap| {
        let e = snap.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((e.3 - 120.0).abs() < 1e-6);
        assert_eq!(e.4, 2_100);
    });

    // 5. Hub defender counter-entosising uncontested reduces accrued progress down to 0 and clears link
    remove_ship_entosis_link(&ctx, ship_a);
    tick_entosis_captures(&ctx, 2_150).await; // 120 - 50 = 70
    ctx.db.with_snapshot(|snap| {
        assert!((snap.sov_runtime.entosis_capture_progress[&hub_id].3 - 70.0).abs() < 1e-6);
    });
    tick_entosis_captures(&ctx, 2_250).await; // 70 - 100 -> 0 -> removed
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // 6. Opposing side de-capturing partial progress: first partial, then flipping ownership
    insert_entosis_link(&ctx, ship_a, 90_000_001, third_alliance, 8004, hub_id, 2_200, 2_400);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_002, att_alliance, 0, 100.0, 2_250));
    });
    // dt = 40 < 100 -> reduces att_alliance progress to 60
    tick_entosis_captures(&ctx, 2_290).await;
    ctx.db.with_snapshot(|snap| {
        let e = snap.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(e.1, att_alliance);
        assert!((e.3 - 60.0).abs() < 1e-6);
    });
    // Now expire at 2_400 without stront -> eff_dt = 2400 - 2290 = 110 >= 60 -> flips to third_alliance with 50.0,
    // and since stront expired and 50 < 600, link is removed!
    tick_entosis_captures(&ctx, 2_450).await;
    ctx.db.with_snapshot(|snap| {
        let e = snap.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(e.1, third_alliance);
        assert!((e.3 - 50.0).abs() < 1e-6);
        assert_eq!(e.2, 0);
    });

    // 7. Stront cycle renewal & Hub capture completion -> HullReinforced
    // Give ship_a 2 units of Stront (16_275): 1st consumed to renew, 2nd kept
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 16_275,
                owner_id: 90_000_001,
                location_id: ship_a,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (90_000_001, third_alliance, ship_a, 500.0, 2_500));
    });
    insert_entosis_link(&ctx, ship_a, 90_000_001, third_alliance, 8005, hub_id, 2_400, 2_520);
    // Tick at 2_550: renews cycle using 1 stront, accrued = 550 < 600
    tick_entosis_captures(&ctx, 2_550).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&777_001].quantity, 1);
        assert!((snap.sov_runtime.entosis_capture_progress[&hub_id].3 - 550.0).abs() < 1e-6);
    });
    // Tick at 2_610: accrued = 610 >= 600 -> Hub captured -> HullReinforced!
    tick_entosis_captures(&ctx, 2_610).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
        assert_eq!(snap.structures[&hub_id].state, "HullReinforced");
        assert_eq!(snap.structures[&hub_id].reinforced_until_unix, 2_610 + 48 * 3_600);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_node_capture_intermediate_defender_win_and_attacker_win() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let hub_id = 1_000_520_001;
    let holder_alliance = 99_000_010;
    let att_alliance = 99_000_011;
    setup_hub(&ctx, sys_id, hub_id, Some(holder_alliance));

    let ship_def = 100_520_010;
    let ship_att = 100_520_011;
    spawn_ball_pos(&ctx, sys_id, ship_def, 587, DVec3::new(120_000.0, 0.0, 0.0));
    spawn_ball_pos(&ctx, sys_id, ship_att, 587, DVec3::new(120_000.0, 0.0, 0.0));

    // Set up characters in holder and attacker alliances
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(holder_alliance);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.alliance_id = Some(att_alliance);
        }
    });

    // 1. Intermediate node capture respawns command nodes up to 4
    let node1 = 1_000_520_101;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.command_nodes.insert(node1, (hub_id, sys_id, 0));
        snap.item_positions.insert(node1, [120_000.0, 0.0, 0.0]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node1, (90_000_001, holder_alliance, ship_def, 200.0, 1_000));
    });
    insert_entosis_link(&ctx, ship_def, 90_000_001, holder_alliance, 8101, node1, 1_000, 1_500);
    tick_entosis_captures(&ctx, 1_050).await;
    ctx.db.with_snapshot(|snap| {
        let node_cnt = snap
            .sov_runtime
            .command_nodes
            .values()
            .filter(|&&(hid, _, _)| hid == hub_id)
            .count();
        assert_eq!(node_cnt, 4);
        assert!(snap.sov_campaign_scores.contains_key(&hub_id));
    });

    // 2. Defender win (progress >= 1.0): set score to 0.96 and capture 1 node as defender
    let node_win = 1_000_520_199;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().state = "HullReinforced".into();
        snap.sov_runtime
            .command_nodes
            .insert(node_win, (hub_id, sys_id, 0));
        snap.item_positions.insert(node_win, [120_000.0, 0.0, 0.0]);
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(holder_alliance, 0.96)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_win, (90_000_001, holder_alliance, ship_def, 200.0, 1_100));
    });
    insert_entosis_link(&ctx, ship_def, 90_000_001, holder_alliance, 8102, node_win, 1_100, 1_500);
    tick_entosis_captures(&ctx, 1_150).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&hub_id].state, "Online");
        assert!(snap.sov_runtime.command_nodes.is_empty());
        assert!(!snap.sov_campaign_scores.contains_key(&hub_id));
    });

    // 3. Attacker win (progress <= 0.0): hub destroyed, upgrades/fuel loot dropped, sov cleared
    let node_att = 1_000_520_201;
    let node_extra = 1_000_520_202;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .sov_claimed_since_unix
            .insert(sys_id, 500);
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![
                    PersistedHubUpgrade {
                        type_id: 81_615,
                        online: true,
                        low_power: false,
                    },
                    PersistedHubUpgrade {
                        type_id: 81_619,
                        online: true,
                        low_power: false,
                    },
                ],
                fuel: [(44_265, 5)].into_iter().collect(),
                ..Default::default()
            },
        );
        snap.sov_runtime.command_nodes.insert(node_att, (hub_id, sys_id, 0));
        snap.sov_runtime.command_nodes.insert(node_extra, (hub_id, sys_id, 0));
        snap.item_positions.insert(node_att, [120_000.0, 0.0, 0.0]);
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(holder_alliance, 0.05)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_att, (90_000_002, att_alliance, ship_att, 200.0, 1_200));
    });
    insert_entosis_link(&ctx, ship_att, 90_000_002, att_alliance, 8103, node_att, 1_200, 1_600);
    tick_entosis_captures(&ctx, 1_250).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&hub_id].state, "Destroyed");
        assert_eq!(snap.system_states[&sys_id].sov_alliance_id, None);
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&hub_id));
        // Dropped 1st upgrade (idx 0) and (5+1)/2 = 3 fuel units
        assert!(snap.items.values().any(|i| i.type_id == 81_615 && i.location_id == sys_id as i64));
        assert!(snap.items.values().any(|i| i.type_id == 44_265 && i.quantity == 3 && i.location_id == sys_id as i64));
    });
}

#[test]
fn remove_ship_entosis_link_clears_active_links_target_cycles_and_module_state() {
    let ctx = test_ctx();
    let ship_id = 1_000_599_001_i64;
    let char_id = 90_000_001_i64;
    let ally_id = 99_599_001_i64;
    let mod_id = 88_599_001_i64;
    let target_id = 1_000_599_100_i64;
    let db_key = db_ptr_key(&ctx.db);

    insert_entosis_link(&ctx, ship_id, char_id, ally_id, mod_id, target_id, 100, 500);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
    });

    remove_ship_entosis_link(&ctx, ship_id);

    let st = ctx.tactical_state.read().unwrap();
    assert!(!st.entosis_active_links.contains_key(&(db_key, ship_id)));
    assert!(!st.entosis_target_cycles.contains_key(&(db_key, target_id, ship_id)));
    drop(st);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
    });
}
