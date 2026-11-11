//! Semantic unit tests split from `serverops_cluster_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::serverops_cluster_extra_unit_tests::*;

use super::serverops_cluster::*;
use crate::router::ServerContext;
use eve_db::ledger::MarketTransactionRecord;
use eve_db::{
    AsteroidRecord, ContractRecord, InventoryFlag, InventoryItemRecord, IskCents,
    LpRewardLogEntry, MarketHistoryDayRecord, SystemDynamicState,
    UpwellStructureRecord,
};
use eve_sde::{InvType, StationInfo};
use glam::DVec3;
use std::sync::Arc;

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
        snap.mining_and_ess_state.ess_reserve_pulses.insert(
            30_004_760,
            eve_db::EssReservePulseState {
                pulses_total: 5,
                pulses_remaining: 3,
                unlocked_until_unix: dt_base + 600,
                ..Default::default()
            },
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
        // #865: Queue a pending FW system capture for daily downtime
        snap.fw_system_occupations.insert(
            30_045_307,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: 30_045_307,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 100,
                victory_points_threshold: 100,
            },
        );
        snap.fw_runtime.pending_downtime_captures.insert(30_045_307, 500_004);
    });

    let report = execute_daily_maintenance_jobs(&ctx);
    assert_eq!(report["is1100UtcWindow"], 1);
    assert_eq!(report["fwSystemsCaptured"], 1);
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
            .mining_and_ess_state
            .ess_reserve_pulses
            .contains_key(&30_004_760));
        assert!(!snap.last_mission_decline_times.contains_key(&90_000_001));
        assert!(snap.last_mission_decline_times.contains_key(&90_000_002));
        assert_eq!(snap.char_lp_logs[&90_000_001].len(), 500);
    });
}
