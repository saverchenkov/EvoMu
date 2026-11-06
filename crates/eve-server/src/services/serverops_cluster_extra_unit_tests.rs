//! Deep branch-coverage and mutation-killing unit tests for `services::serverops_cluster`.

#![forbid(unsafe_code)]

use super::serverops_cluster::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::ledger::MarketTransactionRecord;
use eve_db::{
    AccountRecord, AsteroidRecord, ContractRecord, InventoryFlag, InventoryItemRecord, IskCents,
    LpRewardLogEntry, MarketHistoryDayRecord, OwnerNoteRecord, ServerClock, SystemDynamicState,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, StationInfo};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx(base_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[test]
fn compute_tidi_from_node_load_exact_arithmetic_and_boundaries() {
    // Threshold 0 clamps to 1
    assert_eq!(compute_tidi_from_node_load(0, 0), 1.0);
    assert_eq!(compute_tidi_from_node_load(1, 0), 1.0);
    assert_eq!(compute_tidi_from_node_load(2, 0), 0.50);
    assert_eq!(compute_tidi_from_node_load(4, 0), 0.25);
    assert_eq!(compute_tidi_from_node_load(20, 0), 0.10);

    // Exact threshold boundary (load <= threshold -> 1.0, load > threshold -> scaled)
    assert_eq!(compute_tidi_from_node_load(99, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(100, 100), 1.0);
    assert_eq!(compute_tidi_from_node_load(101, 100), 0.99);
    assert_eq!(compute_tidi_from_node_load(150, 100), 0.67);
    assert_eq!(compute_tidi_from_node_load(250, 100), 0.40);
    assert_eq!(compute_tidi_from_node_load(1_000, 100), 0.10);
    assert_eq!(compute_tidi_from_node_load(50_000, 100), 0.10);
}

#[tokio::test]
async fn system_tidi_factor_clamping_persistence_anchoring_and_override_clearing() {
    let (ctx, clock) = make_ctx(1_775_000_000);
    let jita = 30_000_142_u32;
    let unloaded_sys = 30_000_144_u32;
    let char_id = 90_000_001_i64;

    ctx.db
        .acquire_online_lock(1, 701, Some(char_id), true, 1_775_000_000)
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita;
        }
    });

    // Unloaded system reads from manual_tidi_overrides (clamped) or owner_notes (-7_500_000_000_000 - sys_id)
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);
    let nid = -(7_500_000_000_000_i64 + i64::from(unloaded_sys));
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -75,
                item_id: i64::from(unloaded_sys),
                label: format!("tidi:{unloaded_sys}"),
                note: "0.35".into(),
                timestamp_unix: 100,
            },
        );
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.35);

    // Persisted note below 0.10 or above 1.0 is clamped; invalid JSON falls back to 1.0
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "0.02".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.10);
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "2.50".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.get_mut(&nid).unwrap().note = "not_json".into();
    });
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 1.0);

    // Manual override on unloaded system takes precedence over owner_notes and clamps
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .manual_tidi_overrides
        .insert(unloaded_sys, 0.05);
    assert_eq!(get_system_tidi_factor(&ctx, unloaded_sys), 0.10);
    assert_eq!(clear_manual_system_tidi_override(&ctx, unloaded_sys), 1.0);

    // set_system_tidi_factor on active system: finite clamping, non-finite fallback, and DoSimClockRebase anchoring
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 0.50), 0.50);
    let notifs1 = ctx.drain_notifications_for_char(char_id);
    let rebase1 = notifs1
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase on first TiDi change");
    assert_eq!(rebase1.id_type, "solarsystemid2");
    let PyRep::Tuple(pair1) = &rebase1.args[0] else {
        panic!("expected tuple payload");
    };
    let (old_ft1, new_ft1) = (pair1[0].as_i64().unwrap(), pair1[1].as_i64().unwrap());
    // Seed elapsed with old_factor = 0.50 -> 5_000_000 ticks before wall clock
    assert_eq!(new_ft1 - old_ft1, 5_000_000);

    // Advance wall clock by 10 seconds (100_000_000 ticks) at 0.50 TiDi -> dilated elapsed is 50_000_000 ticks
    clock.advance_secs(10);
    assert_eq!(set_system_tidi_factor(&ctx, jita, f64::INFINITY), 1.0);
    let notifs2 = ctx.drain_notifications_for_char(char_id);
    let rebase2 = notifs2
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase on second TiDi change");
    let PyRep::Tuple(pair2) = &rebase2.args[0] else {
        panic!("expected tuple payload");
    };
    let (old_ft2, new_ft2) = (pair2[0].as_i64().unwrap(), pair2[1].as_i64().unwrap());
    assert_eq!(old_ft2, new_ft1);
    assert_eq!(new_ft2 - old_ft2, 50_000_000);

    assert_eq!(set_system_tidi_factor(&ctx, jita, f64::NEG_INFINITY), 1.0);
    assert_eq!(set_system_tidi_factor(&ctx, jita, 1.75), 1.0);
    assert_eq!(clear_manual_system_tidi_override(&ctx, jita), 1.0);
}

#[tokio::test]
async fn worker_node_resolution_reinforcement_and_overloaded_rebalancing() {
    let (ctx, _) = make_ctx(1_775_000_000);
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let sys_c = 30_002_187_u32;

    set_cluster_node_load_threshold(&ctx, 0);
    assert_eq!(
        ctx.systems
            .cluster_nodes
            .read()
            .unwrap()
            .node_load_tidi_threshold,
        1
    );

    // Persisted owner_note (-7_600_000_000_000 - sys_id) is honored unless that node is reinforced for a different system
    let nid_c = -(7_600_000_000_000_i64 + i64::from(sys_c));
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            nid_c,
            OwnerNoteRecord {
                note_id: nid_c,
                character_id: -76,
                item_id: i64::from(sys_c),
                label: format!("node:{sys_c}"),
                note: "3".into(),
                timestamp_unix: 100,
            },
        );
    });
    assert_eq!(get_system_worker_node(&ctx, sys_c), 3);

    // Mark node 3 as reinforced for sys_a -> sys_c can no longer use persisted node 3
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .reinforced_nodes
        .insert(3, sys_a);
    assert_ne!(get_system_worker_node(&ctx, sys_c), 3);

    // If default constellation candidate is reinforced for another system and ALL worker_nodes are reinforced, falls back to 1
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        let workers = cn.worker_nodes.clone();
        for w in workers {
            cn.reinforced_nodes.insert(w, sys_a);
        }
    }
    assert_eq!(get_system_worker_node(&ctx, sys_c), 1);
    ctx.systems
        .cluster_nodes
        .write()
        .unwrap()
        .reinforced_nodes
        .clear();

    // Spawn two active systems on the same worker node and populate balls so both have load > 0
    let ha = ctx.systems.get_or_spawn(sys_a);
    let hb = ctx.systems.get_or_spawn(sys_b);
    for (idx, h) in [(1_i64, &ha), (2_i64, &hb)] {
        for b_off in 0..3_i64 {
            let ball = Ball {
                id: 950_000 + idx * 10 + b_off,
                type_id: 587,
                position: DVec3::new((b_off as f64) * 1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            h.spawn_ball_with_meta(ball, CombatState::default(), SlimItemMeta::default())
                .await
                .unwrap();
        }
    }

    // Force both sys_a and sys_b onto general worker node 1 with threshold = 1
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        cn.worker_nodes = vec![1, 2, 3, 4];
        cn.system_node_overrides.insert(sys_a, 1);
        cn.system_node_overrides.insert(sys_b, 1);
        cn.node_load_tidi_threshold = 1;
    }
    assert!(get_worker_node_load(&ctx, 1) >= 2);

    // Recomputes load-induced TiDi (< 1.0) since load > threshold
    let tidi_map = recompute_cluster_node_tidi(&ctx);
    assert!(tidi_map[&sys_a] < 1.0);
    assert!(tidi_map[&sys_b] < 1.0);

    // Rebalance overloaded worker nodes migrates the lighter/secondary system off node 1 onto node 2
    let migrated_count = rebalance_overloaded_worker_nodes(&ctx);
    assert_eq!(migrated_count, 1);
    assert_ne!(
        get_system_worker_node(&ctx, sys_a),
        get_system_worker_node(&ctx, sys_b)
    );

    // Reinforcing onto node_id <= 16 adds to worker_nodes if not present and migrates siblings off that node
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        cn.system_node_overrides.insert(sys_b, 12);
    }
    let assigned = assign_or_reinforce_system_node(&ctx, sys_a, Some(12));
    assert_eq!(assigned, 12);
    assert_eq!(get_system_worker_node(&ctx, sys_a), 12);
    assert_ne!(get_system_worker_node(&ctx, sys_b), 12);
    assert!(ctx
        .cluster_state
        .read()
        .unwrap()
        .node_addresses
        .contains_key("127.0.0.1:26012"));
}

#[test]
fn countdown_warnings_history_bound_and_gm_preserving_downtime_shutdown() {
    let (ctx, _) = make_ctx(1_767_225_600);
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();

    // Seed 10 prior days in cluster_downtime_state to verify history pruning down to <= 9 days
    for day in 1..=10_u64 {
        let day_dt = day * 86_400 + dt_sec;
        let _ = step_downtime_countdown_warnings(&ctx, day_dt - 3_600);
    }
    let stored_days: usize = ctx.db.with_snapshot(|snap| {
        let raw = snap.service_state_blobs.get("cluster_downtime_state").unwrap();
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        v["emitted_warnings_by_day"].as_object().unwrap().len()
    });
    assert!(stored_days <= 9);

    // Exact boundary at secs_until_dt == 0 and secs_until_dt == 3601 -> no warnings
    let test_dt = 20 * 86_400 + dt_sec;
    assert!(step_downtime_countdown_warnings(&ctx, test_dt).is_empty());
    assert!(step_downtime_countdown_warnings(&ctx, test_dt - 3_601).is_empty());
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 3_600),
        vec![3_600]
    );
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 900),
        vec![900]
    );
    assert_eq!(
        step_downtime_countdown_warnings(&ctx, test_dt - 300),
        vec![300]
    );

    // GM session vs non-GM session during initiate_cluster_downtime_shutdown
    let gm_uid = 777_u64;
    let player_uid = 888_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "gm_acct".into(),
            AccountRecord {
                user_id: gm_uid,
                username: "gm_acct".into(),
                password_token: "x".into(),
                role: eve_db::service_roles::ROLE_GMH,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.accounts.insert(
            "player_acct".into(),
            AccountRecord {
                user_id: player_uid,
                username: "player_acct".into(),
                password_token: "x".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(gm_uid, 9001, Some(90_000_001), true, 1_767_225_600)
        .unwrap();
    ctx.db
        .acquire_online_lock(player_uid, 9002, Some(90_000_002), true, 1_767_225_600)
        .unwrap();

    let disconnected = initiate_cluster_downtime_shutdown(&ctx);
    assert_eq!(disconnected, 1);
    let online_after = ctx.db.online_sessions_snapshot();
    assert!(online_after.iter().any(|&(u, s)| u == gm_uid && s == 9001));
    assert!(!online_after.iter().any(|&(u, _)| u == player_uid));

    let epoch = complete_cluster_downtime_restart(&ctx);
    assert!(epoch >= 1);
    assert!(!ctx.is_shutting_down());
}

#[test]
fn daily_maintenance_jobs_asteroids_market_ohlcv_contracts_sov_ess_and_declines() {
    let dt_base = 1_767_225_600_u64 + 11 * 3_600 + 60; // Inside 11:00–11:15 UTC window
    let (mut ctx, clock) = make_ctx(dt_base);

    // Add a station with region_id == 0 and an ice type (group_id == 465) + in-space ore type (category_id == 25)
    let mut sde = (*ctx.sde).clone();
    let st_zero_reg = StationInfo {
        station_id: 60_999_001,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 0,
        station_type_id: 1529,
        name: "Zero Region Station".into(),
        position: DVec3::ZERO,
        owner_corporation_id: 1_000_035,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    };
    sde.stations.insert(60_999_001, st_zero_reg);
    let mut ice_type: InvType = sde.types.get(&34).unwrap().clone();
    ice_type.type_id = 999_465;
    ice_type.group_id = 465;
    ice_type.category_id = 25;
    sde.types.insert(999_465, ice_type);
    if let Some(sys) = sde.solar_systems.get_mut(&30_000_142) {
        let mut sys_only_st = sde.stations.get(&60_999_001).unwrap().clone();
        sys_only_st.station_id = 60_999_002;
        sys_only_st.region_id = 10_000_002;
        sys.stations.push(sys_only_st);
    }
    ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::clone(&ctx.db));
    ctx.set_clock(clock);

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Unloaded belt 1 (all empty -> restored) vs belt 2 (partially full -> not restored) + ice asteroid (partially mined -> restored)
        snap.asteroids.insert(
            700_001,
            AsteroidRecord {
                asteroid_id: 700_001,
                solar_system_id: 30_000_144,
                belt_id: 40_000_001,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.asteroids.insert(
            700_002,
            AsteroidRecord {
                asteroid_id: 700_002,
                solar_system_id: 30_000_144,
                belt_id: 40_000_002,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.asteroids.insert(
            700_003,
            AsteroidRecord {
                asteroid_id: 700_003,
                solar_system_id: 30_000_144,
                belt_id: 40_000_002,
                type_id: 1230,
                quantity: 100,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.asteroids.insert(
            700_004,
            AsteroidRecord {
                asteroid_id: 700_004,
                solar_system_id: 30_000_144,
                belt_id: 40_000_003,
                type_id: 999_465,
                quantity: 200,
                initial_quantity: 1_000,
                position: (0.0, 0.0, 0.0),
                radius: 200.0,
            },
        );

        // In-space category 25 asteroid item with custom target qty in attr 1805
        let mut ore_item = InventoryItemRecord {
            item_id: 700_010,
            type_id: 999_465,
            owner_id: 1,
            location_id: 30_000_144,
            flag: InventoryFlag::None,
            quantity: 100,
            singleton: false,
            ..Default::default()
        };
        ore_item.dynamic_attributes.insert(1805, 800.0);
        snap.items.insert(700_010, ore_item);

        // 2. Structure for structure-located market transaction & Sov Hub for ADM window scaling
        snap.structures.insert(
            1_030_000_555,
            UpwellStructureRecord {
                structure_id: 1_030_000_555,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_004_759,
                name: "Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0; 3],
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(1_030_000_555, (36_000, 50_400));

        // Seed 401 historical days on (10_000_002, 34) to test > 400 series trimming + open/high/low/close rollup
        let series = snap.market_history.entry((10_000_002, 34)).or_default();
        for d in 0..401_u32 {
            series.push(MarketHistoryDayRecord {
                date: format!("2020-01-{d:03}"),
                average: 1.0,
                highest: 1.0,
                lowest: 1.0,
                order_count: 1,
                volume: 1,
                open_price: Some(1.0),
                close_price: Some(1.0),
            });
        }

        // Transactions across station with region_id=0, solar_system_id, structure_id, and sys.stations
        for (tid_tx, ts, qty, isk, loc) in [
            (8001, dt_base - 1_000, 10, 100, 60_999_001_i64), // earliest -> open = 100
            (8002, dt_base - 500, 20, 250, 30_000_142_i64),   // high = 250
            (8003, dt_base - 200, 10, 50, 60_999_002_i64),    // low = 50
            (8004, dt_base - 10, 10, 150, 1_030_000_555_i64), // structure in another region
            (8005, dt_base - 5, 10, 120, 60_003_760_i64),     // latest in The Forge -> close = 120
            (8006, dt_base - 100, 0, 999, 60_003_760_i64),    // skipped (qty == 0)
            (8007, dt_base - 90_000, 10, 999, 60_003_760_i64), // skipped (outside 24h window)
            (8008, dt_base - 100, 10, 999, 99_999_999_i64),   // skipped (unknown location)
        ] {
            snap.market_transactions.push(MarketTransactionRecord {
                transaction_id: tid_tx,
                date_unix: ts,
                type_id: 34,
                quantity: qty,
                price: IskCents::from_isk(isk),
                buyer_id: 90_000_001,
                seller_id: 90_000_002,
                location_id: loc,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: tid_tx,
            });
        }

        // 3. Expired Outstanding contract
        snap.contracts.insert(
            77_001,
            ContractRecord {
                contract_id: 77_001,
                issuer_id: 90_000_001,
                issuer_corp_id: 98_000_001,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                date_issued_unix: dt_base - 100_000,
                expiration_unix: dt_base - 1,
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents(100),
                ..Default::default()
            },
        );

        // 4. Sov decay: claimed system (floors at 1) vs unclaimed system (floors at 0) + ESS reserve pulse blob
        snap.system_states.insert(
            30_004_759,
            SystemDynamicState {
                solar_system_id: 30_004_759,
                sov_alliance_id: Some(99_000_001),
                military_index: 2,
                industrial_index: 1,
                strategic_index: 3,
                ess_cycle_started_unix: dt_base - 300,
                ..Default::default()
            },
        );
        snap.system_states.insert(
            30_004_760,
            SystemDynamicState {
                solar_system_id: 30_004_760,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 0,
                ..Default::default()
            },
        );
        snap.service_state_blobs.insert(
            "ess_reserve_pulses:30004760".into(),
            serde_json::json!({"pulses_remaining": 3, "pulses_total": 5, "unlocked_until_unix": dt_base + 600}).to_string(),
        );

        // 5. Mission decline cooldowns (1 stale >= 4h, 1 fresh < 4h) & >500 LP log entries
        snap.last_mission_decline_times
            .insert(90_000_001, dt_base - 15_000);
        snap.last_mission_decline_times
            .insert(90_000_002, dt_base - 3_600);
        let lp_logs = snap.char_lp_logs.entry(90_000_001).or_default();
        for i in 0..505_i64 {
            lp_logs.push(LpRewardLogEntry {
                log_id: i,
                corp_id: 1_000_035,
                amount: 10,
                reason: "Mission".into(),
                timestamp_unix: dt_base,
            });
        }
    });

    let report = execute_daily_maintenance_jobs(&ctx);
    assert_eq!(report["is1100UtcWindow"], 1);
    assert!(report["replenishedAsteroids"] >= 3);
    assert!(report["rolledUpMarketHistoryPairs"] >= 1);
    assert!(report["expiredContracts"] >= 1);
    assert!(report["sovSystemsDecayed"] >= 2);
    assert!(report["essBanksRolledOver"] >= 2);
    assert_eq!(report["missionDeclinesReset"], 1);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids[&700_001].quantity, 5_000);
        assert_eq!(snap.asteroids[&700_002].quantity, 0); // belt 2 was not fully empty
        assert_eq!(snap.asteroids[&700_004].quantity, 1_000); // ice replenished
        assert_eq!(snap.items[&700_010].quantity, 800); // in-space category 25 replenished to attr 1805

        let series = &snap.market_history[&(10_000_002, 34)];
        assert_eq!(series.len(), 400);
        let latest = series.last().unwrap();
        assert_eq!(latest.open_price, Some(100.0));
        assert_eq!(latest.close_price, Some(120.0));
        assert_eq!(latest.highest, 250.0);
        assert_eq!(latest.lowest, 50.0);
        assert_eq!(latest.volume, 50);
        assert_eq!(latest.order_count, 4);

        assert_eq!(snap.system_states[&30_004_759].military_index, 1);
        assert_eq!(snap.system_states[&30_004_759].industrial_index, 1);
        assert_eq!(snap.system_states[&30_004_760].military_index, 0);
        assert_eq!(snap.system_states[&30_004_760].industrial_index, 0);
        assert!(!snap
            .service_state_blobs
            .contains_key("ess_reserve_pulses:30004760"));
        assert!(!snap.last_mission_decline_times.contains_key(&90_000_001));
        assert!(snap.last_mission_decline_times.contains_key(&90_000_002));
        assert_eq!(snap.char_lp_logs[&90_000_001].len(), 500);
    });
}
