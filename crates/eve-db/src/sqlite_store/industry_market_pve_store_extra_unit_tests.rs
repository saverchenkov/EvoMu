//! Deep branch-coverage and mutation-killing unit tests for `sqlite_store::industry_market_pve_store`.

use super::domain_rows::{
    apply_domain_row_changes, table_exists, validate_domain_row_changes, DomainRowChange,
    DOMAIN_CORP_STRUCTURE_AND_KILLMAILS, DOMAIN_PVE_AND_SPACE_EVENTS, DOMAIN_SERVICE_STATE_BLOBS,
    DOMAIN_SHIP_FITTING_AND_SKILLS, DOMAIN_WALLET_AND_MARKET,
};
use super::industry_market_pve_store::{
    load_industry_market_pve, load_industry_market_pve_domain_rows,
    sync_industry_market_pve_domain_rows, sync_mining_ledger, LEGACY_WORLD_KV_BLOBS_SQL,
};
use super::load::load_snapshot_from_sqlite;
use super::quarantine::LoadContext;
use super::schema::apply_migrations;
use super::sync::replace_sqlite_from_snapshot;
use crate::snapshot_ext::*;
use crate::types::*;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

fn populate_all_27_batch4_blobs_and_mining_ledger() -> WorldStateSnapshot {
    let mut snap = WorldStateSnapshot::default();
    snap.moon_extractions.insert(
        1_001,
        MoonExtractionRecord {
            structure_id: 1_001,
            moon_id: 40_000_001,
            extraction_start_unix: 1_000,
            chunk_arrival_unix: 5_000,
            natural_decay_unix: 9_000,
            status: "Started".into(),
            auto_fracture: true,
            event_id: 77,
            moon_ores: vec![(45490, 0.6)],
            spawned_asteroid_ids: vec![800_001],
        },
    );
    snap.auto_moon_miners.insert(
        1_002,
        AutoMoonMinerRecord {
            structure_id: 1_002,
            fuel_units_remaining: 50,
            magmatic_gas_remaining: 100,
            efficiency: 0.85,
            cycle_duration_seconds: 3_600,
            last_cycle_unix: 2_000,
            ore_type_id: 45490,
            hourly_yield: 500,
            status: "Active".into(),
        },
    );
    snap.observer_mining_ledger.insert(
        1_001,
        vec![ObserverMiningRecord {
            observer_id: 1_001,
            character_id: 90_000_001,
            recorded_corporation_id: 98_000_001,
            type_id: 45490,
            quantity: 1_200,
            last_updated_date: "2026-01-01".into(),
            timestamp_unix: 3_000,
        }],
    );
    snap.char_research_agents.insert(
        90_000_001,
        vec![CharacterResearchAgentRecord {
            agent_id: 3_019_001,
            skill_type_id: 11442,
            started_at_unix: 1_000,
            points_per_day: 55.5,
            remainder_points: 12.0,
        }],
    );
    snap.planetary_colony_layouts.insert(
        (90_000_001, 40_009_080),
        PlanetaryColonyLayoutRecord {
            pins: vec![PlanetaryPinRecord {
                pin_id: 10,
                type_id: 2254,
                latitude: 0.5,
                longitude: 1.2,
                schematic_id: Some(65),
                install_time_unix: Some(100),
                expiry_time_unix: Some(200),
                last_cycle_start_unix: Some(150),
                contents: vec![PlanetaryPinContentRecord {
                    type_id: 2389,
                    amount: 500,
                }],
            }],
            links: vec![PlanetaryLinkRecord {
                source_pin_id: 10,
                destination_pin_id: 11,
                link_level: 2,
            }],
            routes: vec![PlanetaryRouteRecord {
                route_id: 1,
                source_pin_id: 10,
                destination_pin_id: 11,
                content_type_id: 2389,
                quantity: 100,
                waypoints: vec![10, 11],
            }],
        },
    );
    snap.corp_customs_offices.insert(
        98_000_001,
        vec![CustomsOfficeRecord {
            office_id: 70_001,
            system_id: 30_000_142,
            reinforce_exit_start: 18,
            reinforce_exit_end: 20,
            allow_alliance_access: true,
            allow_access_with_standings: true,
            standing_level: Some("good".into()),
            corporation_tax_rate: Some(0.02),
            alliance_tax_rate: Some(0.03),
            excellent_standing_tax_rate: Some(0.04),
            good_standing_tax_rate: Some(0.05),
            neutral_standing_tax_rate: Some(0.10),
            bad_standing_tax_rate: None,
            terrible_standing_tax_rate: None,
        }],
    );
    snap.orbital_provision_types.insert(70_001, 2233);
    snap.wallet_journal_timestamps.insert(501, 1_700_000_501);
    snap.char_wallet_transactions.insert(
        90_000_001,
        vec![WalletTransactionRecord {
            transaction_id: 601,
            date_unix: 1_700_000_601,
            location_id: 60_003_760,
            type_id: 34,
            unit_price: 5.5,
            quantity: 1_000,
            client_id: 90_000_002,
            is_buy: true,
            is_personal: true,
            journal_ref_id: 501,
        }],
    );
    snap.corp_wallet_journals.insert(
        (98_000_001, 1000),
        vec![CorpWalletJournalRecord {
            id: 701,
            date_unix: 1_700_000_701,
            ref_type: "bounty_prizes".into(),
            description: "Tax".into(),
            amount: 250.0,
            balance: 10_250.0,
            first_party_id: Some(1_000_125),
            second_party_id: Some(98_000_001),
            reason: Some("NPC".into()),
        }],
    );
    snap.corp_wallet_transactions.insert(
        (98_000_001, 1000),
        vec![WalletTransactionRecord {
            transaction_id: 801,
            date_unix: 1_700_000_801,
            location_id: 60_003_760,
            type_id: 35,
            unit_price: 12.0,
            quantity: 500,
            client_id: 90_000_001,
            is_buy: false,
            is_personal: false,
            journal_ref_id: 701,
        }],
    );
    snap.market_order_meta.insert(
        901,
        MarketOrderMeta {
            issued_unix: 1_700_000_901,
            duration_days: 90,
            range: "region".into(),
            issued_by: Some(90_000_001),
            wallet_division: Some(1000),
            is_corporation: true,
            state: Some("open".into()),
        },
    );
    snap.market_history.insert(
        (10_000_002, 34),
        vec![MarketHistoryDayRecord {
            date: "2026-01-01".into(),
            average: 5.0,
            highest: 5.5,
            lowest: 4.8,
            order_count: 42,
            volume: 100_000,
            open_price: Some(4.9),
            close_price: Some(5.2),
        }],
    );
    snap.cosmic_signatures.insert(
        30_000_142,
        vec![CosmicSignatureRecord {
            signature_id: "ABC-123".into(),
            solar_system_id: 30_000_142,
            sig_group: "Relic Site".into(),
            type_name: "Ruined Guristas Monument".into(),
            position: [100.0, 200.0, 300.0],
            signal_strength: 0.25,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        }],
    );
    snap.cosmic_anomalies.insert(
        30_000_142,
        vec![CosmicAnomalyRecord {
            anomaly_id: "XYZ-999".into(),
            solar_system_id: 30_000_142,
            name: "Guristas Hideaway".into(),
            anomaly_type: "Combat".into(),
            position: [400.0, 500.0, 600.0],
            signal_strength: 1.0,
            ore_types: vec![1230],
            remaining_volume: 5_000.0,
            created_unix: 1_000,
            lifetime_seconds: 86_400,
            cleared: false,
        }],
    );
    snap.incursions.insert(
        20_000_020,
        IncursionRecord {
            constellation_id: 20_000_020,
            staging_solar_system_id: 30_000_142,
            infested_solar_systems: vec![30_000_142, 30_000_144],
            faction_id: 500_019,
            has_boss: true,
            influence: 0.75,
            state: "Mobilizing".into(),
            incursion_type: "Incursion".into(),
        },
    );
    snap.last_mission_decline_times.insert(90_000_001, 1_700_001_000);
    snap.module_online_states.insert(100_000_010, true);
    snap.module_active_states.insert(100_000_010);
    snap.module_active_charges.insert(100_000_010, 190);
    snap.ship_fit_inputs.insert(
        100_000_001,
        crate::ShipFitInputsRecord {
            active_stance_type_id: Some(34321),
            last_stance_switch_ms: Some(1_700_000_123_456),
            overloaded_modules: std::collections::BTreeSet::from([100_000_010]),
        },
    );
    snap.weapon_banks
        .entry(100_000_001)
        .or_default()
        .insert(100_000_010, vec![100_000_011, 100_000_012]);
    snap.paused_skill_queues.insert(90_000_001);
    snap.starbase_settings
        .insert(800_001, StarbaseDetailSettings::default());
    snap.quasar_state_blobs.insert("q_key".into(), "q_val".into());
    snap.service_state_blobs.insert("svc_key".into(), "svc_val".into());
    snap.killmail_details.insert(
        7001,
        crate::KillmailDetailRecord {
            kill_id: 7001,
            killmail_time_unix: 1_700_000_000,
            damage_taken: 1200,
            victim_position: Some([1.0, 2.0, 3.0]),
            attackers: Vec::new(),
        },
    );
    snap.station_office_rental_costs.insert(60_003_760, 50_000.0);
    snap.structure_esi_timer_meta
        .insert(1_020_000_000_001, crate::StructureEsiTimerMeta::default());
    snap.corp_project_contributors.insert(
        10,
        vec![crate::CorpProjectContributorEntry {
            character_id: 90_000_001,
            contributed_quantity: 5,
        }],
    );
    snap.loyalty_store_offers.insert(
        1_000_035,
        vec![crate::LoyaltyStoreOfferRecord {
            offer_id: 1,
            type_id: 17_703,
            quantity: 1,
            lp_cost: 100,
            isk_cost: 1_000,
            ak_cost: None,
            required_items: Vec::new(),
        }],
    );
    snap.system_jump_events.push(crate::SystemJumpEvent {
        solar_system_id: 30_000_142,
        character_id: 90_000_001,
        timestamp_unix: 1_700_000_100,
    });
    snap.system_kill_events.push(crate::SystemKillEvent {
        solar_system_id: 30_000_142,
        ship_kills: 1,
        npc_kills: 2,
        pod_kills: 0,
        timestamp_unix: 1_700_000_200,
    });
    snap.mining_ledger.push(MiningLedgerRecord {
        character_id: 90_000_001,
        solar_system_id: 30_000_142,
        type_id: 1230,
        quantity: 2_500,
        date: "2026-01-02".into(),
        timestamp_unix: 1_700_003_000,
    });
    snap.insurance_contracts.insert(
        100_000_001,
        crate::PersistedInsuranceContractRecord {
            ship_id: 100_000_001,
            ship_type_id: 587,
            owner_id: 90_000_001,
            level_name: "Platinum".into(),
            level_fraction: 1.0,
            cost_isk: 150_000.0,
            payout_isk: 500_000.0,
            start_unix: 1_700_000_000,
            end_unix: 1_700_000_000 + 84 * 86_400,
        },
    );
    snap
}

fn assert_all_34_industry_market_pve_fields_eq(lhs: &WorldStateSnapshot, rhs: &WorldStateSnapshot) {
    assert_eq!(lhs.moon_extractions, rhs.moon_extractions);
    assert_eq!(lhs.auto_moon_miners, rhs.auto_moon_miners);
    assert_eq!(lhs.observer_mining_ledger, rhs.observer_mining_ledger);
    assert_eq!(lhs.char_research_agents, rhs.char_research_agents);
    assert_eq!(lhs.planetary_colony_layouts, rhs.planetary_colony_layouts);
    assert_eq!(lhs.corp_customs_offices, rhs.corp_customs_offices);
    assert_eq!(lhs.orbital_provision_types, rhs.orbital_provision_types);
    assert_eq!(lhs.wallet_journal_timestamps, rhs.wallet_journal_timestamps);
    assert_eq!(lhs.char_wallet_transactions, rhs.char_wallet_transactions);
    assert_eq!(lhs.corp_wallet_journals, rhs.corp_wallet_journals);
    assert_eq!(lhs.corp_wallet_transactions, rhs.corp_wallet_transactions);
    assert_eq!(lhs.market_order_meta, rhs.market_order_meta);
    assert_eq!(lhs.market_history, rhs.market_history);
    assert_eq!(lhs.cosmic_signatures, rhs.cosmic_signatures);
    assert_eq!(lhs.cosmic_anomalies, rhs.cosmic_anomalies);
    assert_eq!(lhs.incursions, rhs.incursions);
    assert_eq!(lhs.last_mission_decline_times, rhs.last_mission_decline_times);
    assert_eq!(lhs.module_online_states, rhs.module_online_states);
    assert_eq!(lhs.module_active_states, rhs.module_active_states);
    assert_eq!(lhs.module_active_charges, rhs.module_active_charges);
    assert_eq!(lhs.ship_fit_inputs, rhs.ship_fit_inputs);
    assert_eq!(lhs.weapon_banks, rhs.weapon_banks);
    assert_eq!(lhs.paused_skill_queues, rhs.paused_skill_queues);
    assert_eq!(lhs.starbase_settings, rhs.starbase_settings);
    assert_eq!(lhs.killmail_details, rhs.killmail_details);
    assert_eq!(lhs.station_office_rental_costs, rhs.station_office_rental_costs);
    assert_eq!(lhs.structure_esi_timer_meta, rhs.structure_esi_timer_meta);
    assert_eq!(lhs.corp_project_contributors, rhs.corp_project_contributors);
    assert_eq!(lhs.loyalty_store_offers, rhs.loyalty_store_offers);
    assert_eq!(lhs.system_jump_events, rhs.system_jump_events);
    assert_eq!(lhs.system_kill_events, rhs.system_kill_events);
    assert_eq!(lhs.quasar_state_blobs, rhs.quasar_state_blobs);
    assert_eq!(lhs.service_state_blobs, rhs.service_state_blobs);
    assert_eq!(lhs.insurance_contracts, rhs.insurance_contracts);
}

#[test]
fn all_34_industry_market_pve_collections_and_mining_ledger_roundtrip_and_reload() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let empty = WorldStateSnapshot::default();
    let full = populate_all_27_batch4_blobs_and_mining_ledger();

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_industry_market_pve_domain_rows(&tx, &full).unwrap());
        assert!(sync_mining_ledger(&tx, Some(&empty), &full).unwrap());
        tx.commit().unwrap();
    }
    // Second sync with identical snapshot is a no-op
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_industry_market_pve_domain_rows(&tx, &full).unwrap());
        assert!(!sync_mining_ledger(&tx, Some(&full), &full).unwrap());
        tx.commit().unwrap();
    }

    let mut loaded = WorldStateSnapshot::default();
    load_industry_market_pve(&conn, &mut loaded);
    assert_all_34_industry_market_pve_fields_eq(&loaded, &full);
    let loaded_full = load_snapshot_from_sqlite(&conn).expect("load_snapshot_from_sqlite");
    assert_all_34_industry_market_pve_fields_eq(&loaded_full, &full);
    assert_eq!(loaded_full.mining_ledger, full.mining_ledger);
    let _ = (HashMap::<i64, i64>::new(), HashSet::<i64>::new());
}

#[test]
fn industry_market_pve_tx_single_row_granularity_zero_cold_clones_and_rejects_non_finite() {
    use crate::persistence::{
        reset_thread_cold_domain_syncs_for_test, thread_cold_domain_syncs_for_test, WorldRepository,
    };
    use crate::tx_engine::{ChangeSet, TxError, TxSqlExecutor};

    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_imp_tx_granularity_{}_{:?}.sqlite",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&tmp_path);
    let repo = WorldRepository::open_sqlite(&tmp_path).expect("open_sqlite");

    let seed = populate_all_27_batch4_blobs_and_mining_ledger();
    let rental_station_id = *seed.station_office_rental_costs.keys().next().unwrap();
    let orig_cost = seed.station_office_rental_costs[&rental_station_id];

    let seed_res: Result<(), TxError> = repo.tx("seed_industry_market_pve", |wtx| {
        *wtx.snap = seed.clone();
        Ok(())
    });
    seed_res.expect("seed tx");

    // Mutating a single Family 5 map entry executes with 0 cold domain syncs.
    reset_thread_cold_domain_syncs_for_test();
    let update_res: Result<(), TxError> = repo.tx("update_one_rental_cost", |wtx| {
        wtx.snap
            .station_office_rental_costs
            .insert(rental_station_id, orig_cost + 5_000.0);
        Ok(())
    });
    update_res.expect("update tx");
    assert_eq!(
        thread_cold_domain_syncs_for_test(),
        0,
        "migrated industry_market_pve mutation must trigger 0 cold domain syncs"
    );

    // Direct `ChangeSet::diff` + `apply_domain_row_changes` writes strictly 1 SQLite row.
    let before_snap = repo.with_snapshot(Clone::clone);
    let mut after_snap = before_snap.clone();
    after_snap
        .station_office_rental_costs
        .insert(rental_station_id, orig_cost + 12_345.0);
    let changes = ChangeSet::diff(2, &before_snap, &after_snap);
    assert_eq!(changes.domain_rows.len(), 1);
    assert!(!changes.cold_domains.d6_batch4);
    assert!(matches!(
        &changes.domain_rows[0],
        DomainRowChange::Upsert { domain, key, .. }
            if *domain == DOMAIN_CORP_STRUCTURE_AND_KILLMAILS
                && key == &format!("station_office_rental_costs/{rental_station_id}")
    ));

    let mut mem_conn = Connection::open_in_memory().expect("open mem");
    apply_migrations(&mem_conn).expect("migrate mem");
    replace_sqlite_from_snapshot(&mut mem_conn, &before_snap).expect("seed mem");
    let before_changes = mem_conn.total_changes();
    {
        let sql_tx = mem_conn.transaction().expect("begin tx");
        let mut exec = TxSqlExecutor::new(&sql_tx, crate::tx_engine::TxFaultPlan::None);
        apply_domain_row_changes(&mut exec, &changes.domain_rows).expect("apply 1 row");
        sql_tx.commit().expect("commit 1 row");
    }
    assert_eq!(
        mem_conn.total_changes() - before_changes,
        1,
        "mutating 1 Family 5 entry must touch strictly 1 SQLite row"
    );

    // Non-finite floats (NaN / Inf) fail validation before BEGIN IMMEDIATE without mutating snapshot.
    for bad_float in [f64::NAN, f64::INFINITY] {
        let mut bad_after = before_snap.clone();
        bad_after
            .station_office_rental_costs
            .insert(rental_station_id, bad_float);
        let bad_changes = ChangeSet::diff(3, &before_snap, &bad_after);
        let check_err = validate_domain_row_changes(&bad_changes.domain_rows, 1024 * 1024)
            .expect_err("non-finite must fail preflight validation");
        assert!(matches!(check_err, TxError::NonFinite { .. }), "{check_err}");
        let msg = check_err.to_string();
        assert!(msg.contains(DOMAIN_CORP_STRUCTURE_AND_KILLMAILS), "{msg}");
        assert!(
            msg.contains(&format!("station_office_rental_costs/{rental_station_id}")),
            "{msg}"
        );

        let tx_err: Result<(), TxError> = repo.tx("reject_bad_float", |wtx| {
            wtx.snap
                .station_office_rental_costs
                .insert(rental_station_id, bad_float);
            Ok(())
        });
        assert!(matches!(tx_err, Err(TxError::NonFinite { .. })));
    }
    assert_eq!(
        repo.with_snapshot(|s| s.station_office_rental_costs[&rental_station_id]),
        orig_cost + 5_000.0
    );

    drop(repo);
    let _ = std::fs::remove_file(&tmp_path);
}

#[test]
fn legacy_world_kv_blobs_migrates_preserving_newer_domain_rows_and_drops_table() {
    fn map_vec_json<K: serde::Serialize, V: serde::Serialize>(m: &HashMap<K, V>) -> String {
        let entries: Vec<(&K, &V)> = m.iter().collect();
        serde_json::to_string(&entries).unwrap()
    }

    let conn = Connection::open_in_memory().expect("open");
    apply_migrations(&conn).expect("migrate");
    conn.execute_batch(LEGACY_WORLD_KV_BLOBS_SQL)
        .expect("create legacy world_kv_blobs");

    let legacy = populate_all_27_batch4_blobs_and_mining_ledger();

    let wb_entries: Vec<(i64, Vec<(i64, &Vec<i64>)>)> = legacy
        .weapon_banks
        .iter()
        .map(|(&k1, inner)| (k1, inner.iter().map(|(&k2, v)| (k2, v)).collect()))
        .collect();
    let set_json = |s: &HashSet<i64>| -> String {
        serde_json::to_string(&s.iter().copied().collect::<Vec<i64>>()).unwrap()
    };

    let rows: Vec<(&str, String)> = vec![
        ("moon_extractions", map_vec_json(&legacy.moon_extractions)),
        ("auto_moon_miners", map_vec_json(&legacy.auto_moon_miners)),
        ("observer_mining_ledger", map_vec_json(&legacy.observer_mining_ledger)),
        ("char_research_agents", map_vec_json(&legacy.char_research_agents)),
        ("planetary_colony_layouts", map_vec_json(&legacy.planetary_colony_layouts)),
        ("corp_customs_offices", map_vec_json(&legacy.corp_customs_offices)),
        ("orbital_provision_types", map_vec_json(&legacy.orbital_provision_types)),
        ("wallet_journal_timestamps", map_vec_json(&legacy.wallet_journal_timestamps.iter().map(|(&k, &v)| (k, v)).collect())),
        ("char_wallet_transactions", map_vec_json(&legacy.char_wallet_transactions)),
        ("corp_wallet_journals", map_vec_json(&legacy.corp_wallet_journals)),
        ("corp_wallet_transactions", map_vec_json(&legacy.corp_wallet_transactions)),
        ("market_order_meta", map_vec_json(&legacy.market_order_meta)),
        ("market_history", map_vec_json(&legacy.market_history)),
        ("cosmic_signatures", map_vec_json(&legacy.cosmic_signatures)),
        ("cosmic_anomalies", map_vec_json(&legacy.cosmic_anomalies)),
        ("incursions", map_vec_json(&legacy.incursions)),
        ("last_mission_decline_times", map_vec_json(&legacy.last_mission_decline_times)),
        ("module_online_states", map_vec_json(&legacy.module_online_states)),
        ("module_active_states", set_json(&legacy.module_active_states)),
        ("module_active_charges", map_vec_json(&legacy.module_active_charges)),
        ("ship_fit_inputs", map_vec_json(&legacy.ship_fit_inputs)),
        ("weapon_banks", serde_json::to_string(&wb_entries).unwrap()),
        ("paused_skill_queues", set_json(&legacy.paused_skill_queues)),
        ("starbase_settings", map_vec_json(&legacy.starbase_settings)),
        ("killmail_details", map_vec_json(&legacy.killmail_details)),
        ("station_office_rental_costs", map_vec_json(&legacy.station_office_rental_costs)),
        ("structure_esi_timer_meta", map_vec_json(&legacy.structure_esi_timer_meta)),
        ("corp_project_contributors", map_vec_json(&legacy.corp_project_contributors)),
        ("loyalty_store_offers", map_vec_json(&legacy.loyalty_store_offers)),
        ("system_jump_events", serde_json::to_string(&legacy.system_jump_events).unwrap()),
        ("system_kill_events", serde_json::to_string(&legacy.system_kill_events).unwrap()),
        ("quasar_state_blobs", map_vec_json(&legacy.quasar_state_blobs)),
        ("service_state_blobs", map_vec_json(&legacy.service_state_blobs)),
        ("insurance_contracts", map_vec_json(&legacy.insurance_contracts)),
        // Include retired Family 4 duplicates + an unknown key to ensure clean tolerance.
        ("corp_container_logs", "[]".into()),
        ("wh_polarizations", "[]".into()),
        ("future_unknown_legacy_blob", "[]".into()),
    ];
    for (k, v) in rows {
        conn.execute(
            "INSERT INTO world_kv_blobs (blob_key, blob_json) VALUES (?1, ?2)",
            params![k, v],
        )
        .expect("seed legacy world_kv_blobs");
    }

    // Pre-seed newer `domain_rows` overrides so migration overlays cleanly without clobbering.
    let existing_service_key = legacy.service_state_blobs.keys().next().unwrap().clone();
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
        params![
            DOMAIN_SERVICE_STATE_BLOBS,
            format!("service_state_blobs/{existing_service_key}"),
            serde_json::to_string("newer-domain-value").unwrap()
        ],
    )
    .expect("seed newer service_state_blobs row");

    let newer_jump = vec![SystemJumpEvent {
        solar_system_id: 30_000_142,
        character_id: 90_000_777,
        timestamp_unix: 1_799_999_999,
    }];
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'system_jump_events', ?2)",
        params![
            DOMAIN_PVE_AND_SPACE_EVENTS,
            serde_json::to_string(&newer_jump).unwrap()
        ],
    )
    .expect("seed newer system_jump_events row");

    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'paused_skill_queues/90999001', 'true')",
        params![DOMAIN_SHIP_FITTING_AND_SKILLS],
    )
    .expect("seed newer paused_skill_queues row");

    let migrated = load_snapshot_from_sqlite(&conn).expect("load + migrate");
    assert!(
        !table_exists(&conn, "world_kv_blobs").unwrap(),
        "retired world_kv_blobs table must be dropped after migration"
    );

    // Newer domain_rows win; non-conflicting legacy entries migrate intact.
    assert_eq!(
        migrated.service_state_blobs.get(&existing_service_key).map(String::as_str),
        Some("newer-domain-value")
    );
    assert_eq!(*migrated.system_jump_events, newer_jump);
    assert!(migrated.paused_skill_queues.contains(&90_999_001));
    assert!(
        legacy
            .paused_skill_queues
            .iter()
            .all(|id| migrated.paused_skill_queues.contains(id))
    );
    assert_eq!(migrated.moon_extractions, legacy.moon_extractions);
    assert_eq!(migrated.weapon_banks, legacy.weapon_banks);
    assert_eq!(migrated.insurance_contracts, legacy.insurance_contracts);
    assert_eq!(migrated.quasar_state_blobs, legacy.quasar_state_blobs);

    // Reloading post-migration reads cleanly straight from `domain_rows`.
    let reloaded = load_snapshot_from_sqlite(&conn).expect("reload post-migration");
    assert_all_34_industry_market_pve_fields_eq(&reloaded, &migrated);
}

#[test]
fn industry_market_pve_strict_load_fails_closed_and_quarantine_preserves_valid_rows() {
    let conn = Connection::open_in_memory().expect("open");
    apply_migrations(&conn).expect("migrate");

    // 1. Valid row + corrupt JSON + unknown key in `domain_rows`.
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'wallet_journal_timestamps/1001', '1700000100'), (?1, 'wallet_journal_timestamps/bad', '{corrupt'), (?1, 'unknown_field/1', '{}')",
        [DOMAIN_WALLET_AND_MARKET],
    )
    .expect("seed domain_rows");

    let mut strict_snap = WorldStateSnapshot::default();
    strict_snap.wallet_journal_timestamps.insert(777, 888);
    assert!(
        load_industry_market_pve_domain_rows(&conn, &mut strict_snap, &mut LoadContext::new(false))
            .is_err(),
        "strict load must fail closed without mutating snapshot"
    );
    assert_eq!(strict_snap.wallet_journal_timestamps.get(&777), Some(&888));
    assert!(!strict_snap.wallet_journal_timestamps.contains_key(&1001));

    let mut q_ctx = LoadContext::quarantine_in_memory();
    assert!(!load_industry_market_pve_domain_rows(&conn, &mut strict_snap, &mut q_ctx).unwrap());
    assert_eq!(strict_snap.wallet_journal_timestamps.get(&1001), Some(&1_700_000_100));
    assert_eq!(q_ctx.quarantined.len(), 2);

    // 2. Corrupt legacy `world_kv_blobs` row fails closed under strict mode and quarantines under quarantine mode.
    conn.execute_batch(LEGACY_WORLD_KV_BLOBS_SQL).unwrap();
    conn.execute(
        "INSERT INTO world_kv_blobs (blob_key, blob_json) VALUES ('moon_extractions', '{corrupt')",
        [],
    )
    .unwrap();
    let mut legacy_strict = WorldStateSnapshot::default();
    assert!(load_industry_market_pve_domain_rows(
        &conn,
        &mut legacy_strict,
        &mut LoadContext::new(false),
    )
    .is_err());

    let mut q_legacy = LoadContext::quarantine_in_memory();
    assert!(
        load_industry_market_pve_domain_rows(&conn, &mut legacy_strict, &mut q_legacy).unwrap()
    );
    assert_eq!(legacy_strict.wallet_journal_timestamps.get(&1001), Some(&1_700_000_100));
    assert_eq!(q_legacy.quarantined.len(), 3);
}
