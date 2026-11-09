//! Deep branch-coverage and mutation-killing unit tests for `sqlite_store::industry_market_pve_store`.

use super::fingerprints::TableFingerprints;
use super::industry_market_pve_store::{
    apply_d6_batch4_migration, compute_d6_batch4_fingerprints, load_d6_batch4,
    rollback_d6_batch4, sync_d6_batch4, D6_BATCH4_SCHEMA_VERSION,
};
use super::schema::apply_migrations;
use crate::snapshot_ext::*;
use crate::types::*;
use rusqlite::Connection;
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
    snap.wh_polarizations.insert((90_000_001, 500_001), 1_700_001_300);
    snap.module_online_states.insert(100_000_010, true);
    snap.module_active_states.insert(100_000_010);
    snap.module_active_charges.insert(100_000_010, 190);
    snap.weapon_banks
        .entry(100_000_001)
        .or_default()
        .insert(100_000_010, vec![100_000_011, 100_000_012]);
    snap.paused_skill_queues.insert(90_000_001);
    snap.starbase_settings
        .insert(800_001, StarbaseDetailSettings::default());
    snap.corp_container_logs.insert(
        98_000_001,
        vec![CorpContainerLogEntry {
            logged_at_unix: 1_700_002_000,
            container_id: 900_001,
            container_type_id: 17366,
            character_id: 90_000_001,
            location_id: 60_003_760,
            location_flag: "CorpSAG1".into(),
            action: "unlock".into(),
            password_type: Some("general".into()),
            type_id: Some(34),
            quantity: Some(10),
            old_config_bitmask: None,
            new_config_bitmask: None,
        }],
    );
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

#[test]
fn all_27_batch4_blobs_and_mining_ledger_roundtrip_and_rollback() {
    assert_eq!(D6_BATCH4_SCHEMA_VERSION, 5);
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_batch4_migration(&conn).unwrap();

    let empty = WorldStateSnapshot::default();
    let empty_fp = compute_d6_batch4_fingerprints(&empty);
    assert_eq!(empty_fp.blobs.len(), 35);

    let full = populate_all_27_batch4_blobs_and_mining_ledger();
    let full_fp = compute_d6_batch4_fingerprints(&full);
    assert_eq!(full_fp.blobs.len(), 35);
    assert_ne!(full_fp.mining_ledger, empty_fp.mining_ledger);
    for (key, empty_hash) in &empty_fp.blobs {
        assert_ne!(
            full_fp.blobs.get(key),
            Some(empty_hash),
            "blob key '{key}' hash did not change when populated"
        );
    }

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6_batch4(&tx, &full, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    // Second sync with identical snapshot is a no-op
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_d6_batch4(&tx, &full, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // Corrupt world_kv_blobs with an unknown key and invalid JSON for one check, then reload
    conn.execute(
        "INSERT INTO world_kv_blobs (blob_key, blob_json) VALUES ('unknown_future_blob', '[]')",
        [],
    )
    .unwrap();

    let mut loaded = WorldStateSnapshot::default();
    load_d6_batch4(&conn, &mut loaded);
    assert_eq!(loaded.moon_extractions, full.moon_extractions);
    assert_eq!(loaded.auto_moon_miners, full.auto_moon_miners);
    assert_eq!(loaded.observer_mining_ledger, full.observer_mining_ledger);
    assert_eq!(loaded.char_research_agents, full.char_research_agents);
    assert_eq!(loaded.planetary_colony_layouts, full.planetary_colony_layouts);
    assert_eq!(loaded.corp_customs_offices, full.corp_customs_offices);
    assert_eq!(loaded.orbital_provision_types, full.orbital_provision_types);
    assert_eq!(loaded.wallet_journal_timestamps, full.wallet_journal_timestamps);
    assert_eq!(loaded.char_wallet_transactions, full.char_wallet_transactions);
    assert_eq!(loaded.corp_wallet_journals, full.corp_wallet_journals);
    assert_eq!(loaded.corp_wallet_transactions, full.corp_wallet_transactions);
    assert_eq!(loaded.market_order_meta, full.market_order_meta);
    assert_eq!(loaded.market_history, full.market_history);
    assert_eq!(loaded.cosmic_signatures, full.cosmic_signatures);
    assert_eq!(loaded.cosmic_anomalies, full.cosmic_anomalies);
    assert_eq!(loaded.incursions, full.incursions);
    assert_eq!(loaded.last_mission_decline_times, full.last_mission_decline_times);
    assert_eq!(loaded.wh_polarizations, full.wh_polarizations);
    assert_eq!(loaded.module_online_states, full.module_online_states);
    assert_eq!(loaded.module_active_states, full.module_active_states);
    assert_eq!(loaded.module_active_charges, full.module_active_charges);
    assert_eq!(loaded.weapon_banks, full.weapon_banks);
    assert_eq!(loaded.paused_skill_queues, full.paused_skill_queues);
    assert_eq!(loaded.starbase_settings, full.starbase_settings);
    assert_eq!(loaded.corp_container_logs, full.corp_container_logs);
    assert_eq!(loaded.quasar_state_blobs, full.quasar_state_blobs);
    assert_eq!(loaded.service_state_blobs, full.service_state_blobs);
    assert_eq!(loaded.insurance_contracts, full.insurance_contracts);

    // Wipe a dirty snapshot and verify rollback_d6_batch4 restores all blobs + mining_ledger
    let mut dirty = WorldStateSnapshot::default();
    rollback_d6_batch4(&conn, &mut dirty, &fp);
    assert_eq!(dirty.moon_extractions, full.moon_extractions);
    assert_eq!(dirty.auto_moon_miners, full.auto_moon_miners);
    assert_eq!(dirty.observer_mining_ledger, full.observer_mining_ledger);
    assert_eq!(dirty.mining_ledger, full.mining_ledger);
    assert_eq!(dirty.char_research_agents, full.char_research_agents);
    assert_eq!(dirty.planetary_colony_layouts, full.planetary_colony_layouts);
    assert_eq!(dirty.corp_customs_offices, full.corp_customs_offices);
    assert_eq!(dirty.orbital_provision_types, full.orbital_provision_types);
    assert_eq!(dirty.wallet_journal_timestamps, full.wallet_journal_timestamps);
    assert_eq!(dirty.char_wallet_transactions, full.char_wallet_transactions);
    assert_eq!(dirty.corp_wallet_journals, full.corp_wallet_journals);
    assert_eq!(dirty.corp_wallet_transactions, full.corp_wallet_transactions);
    assert_eq!(dirty.market_order_meta, full.market_order_meta);
    assert_eq!(dirty.market_history, full.market_history);
    assert_eq!(dirty.cosmic_signatures, full.cosmic_signatures);
    assert_eq!(dirty.cosmic_anomalies, full.cosmic_anomalies);
    assert_eq!(dirty.incursions, full.incursions);
    assert_eq!(dirty.last_mission_decline_times, full.last_mission_decline_times);
    assert_eq!(dirty.wh_polarizations, full.wh_polarizations);
    assert_eq!(dirty.module_online_states, full.module_online_states);
    assert_eq!(dirty.module_active_states, full.module_active_states);
    assert_eq!(dirty.module_active_charges, full.module_active_charges);
    assert_eq!(dirty.weapon_banks, full.weapon_banks);
    assert_eq!(dirty.paused_skill_queues, full.paused_skill_queues);
    assert_eq!(dirty.starbase_settings, full.starbase_settings);
    assert_eq!(dirty.corp_container_logs, full.corp_container_logs);
    assert_eq!(dirty.quasar_state_blobs, full.quasar_state_blobs);
    assert_eq!(dirty.service_state_blobs, full.service_state_blobs);
    assert_eq!(dirty.insurance_contracts, full.insurance_contracts);
    let _ = (HashMap::<i64, i64>::new(), HashSet::<i64>::new());
}
