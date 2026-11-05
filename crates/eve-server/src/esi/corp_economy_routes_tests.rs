//! Seeded-data integration tests covering corp governance, corp alliance, market, contract, and industry/PI ESI routes.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::{EsiHttpRequest, EsiRouter};
use eve_db::{
    AllianceLifecycleMeta, CorpAllianceHistoryEntry, CorpContainerLogEntry, CorpIssuedMedalEntry,
    CorpMedalEntry, CorpMemberRoleAssignment, CorpProjectContributorEntry, CorpProjectRecord,
    CorpRoleHistoryEntry, CorpShareholderEntry, CorpTitleEntry, CustomsOfficeRecord,
    InventoryFlag, InventoryItemRecord, IskCents, LoyaltyStoreOfferItemRecord,
    LoyaltyStoreOfferRecord, MarketHistoryDayRecord, MarketOrderMeta, MarketOrderRecord,
    MiningLedgerRecord, MoonExtractionRecord, ObserverMiningRecord, PlanetaryColonyLayoutRecord,
    StandingTransactionRecord, StarbaseDetailSettings, StructureEsiTimerMeta,
};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn esi_corp_governance_alliance_starbases_and_blueprints_populated() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;
    let structure_id = 1_055_000_001_i64;
    let starbase_id = 1_055_000_777_i64;
    let now = ctx.now_unix();
    let pos_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 365)
        .map(|t| t.type_id)
        .unwrap_or(12_235);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.description = "Sovereign Syndicate".to_string();
            c.url = "https://syndicate.eve".to_string();
            c.tax_rate = 0.05;
            c.hq_station_id = Some(60_003_760);
        }
        // Corp blueprints (singleton BPO, stacked BPO, and BPC in corp office)
        snap.items.insert(
            100_000_050,
            InventoryItemRecord {
                item_id: 100_000_050,
                type_id: 681,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::from_u16(116),
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(1112, 10.0), (1113, 20.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_000_051,
            InventoryItemRecord {
                item_id: 100_000_051,
                type_id: 681,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
            },
        );
        // Starbase (POS control tower) + settings
        snap.items.insert(
            starbase_id,
            InventoryItemRecord {
                item_id: starbase_id,
                type_id: pos_type_id,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(starbase_id, true);
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                allow_corporation_members: true,
                allow_alliance_members: true,
                use_alliance_standings: false,
                attack_if_other_security_status_dropping: true,
                attack_if_at_war: true,
                fuel_bay_view: "CorporationMember".into(),
                fuel_bay_take: "ConfigStarbaseEquipmentRole".into(),
                anchor: "ConfigStarbaseEquipmentRole".into(),
                unanchor: "ConfigStarbaseEquipmentRole".into(),
                online: "ConfigStarbaseEquipmentRole".into(),
                offline: "ConfigStarbaseEquipmentRole".into(),
                attack_standing_threshold: Some(-5.0),
                attack_security_status_threshold: Some(-2.0),
                active_controller_id: Some(char_id),
                password: None,
                notify_fuel: true,
                notify_calendar: false,
                notify_shield: Some(0.5),
                notify_armor: Some(0.5),
                notify_structure: Some(0.9),
                linked_resources: vec![],
                last_moon_process_unix: now,
            },
        );
        snap.structure_esi_timer_meta.insert(
            structure_id,
            StructureEsiTimerMeta {
                profile_id: 1,
                fuel_expires_unix: Some(now + 86_400),
                state_timer_start_unix: None,
                state_timer_end_unix: None,
                unanchors_at_unix: None,
                next_reinforce_hour: Some(2),
                next_reinforce_apply_unix: Some(now + 3_600),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            99_000_001,
            AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: now.saturating_sub(86_400),
                faction_id: None,
            },
        );
        snap.corp_alliance_history.insert(
            corp_id,
            vec![CorpAllianceHistoryEntry {
                record_id: 1,
                alliance_id: Some(99_000_001),
                start_date_unix: now.saturating_sub(86_400),
                is_deleted: None,
            }],
        );
        snap.corp_container_logs.insert(
            corp_id,
            vec![CorpContainerLogEntry {
                logged_at_unix: now,
                container_id: 900_300_001,
                container_type_id: 17_366,
                character_id: char_id,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".into(),
                action: "take".into(),
                password_type: None,
                type_id: Some(34),
                quantity: Some(100),
                old_config_bitmask: None,
                new_config_bitmask: None,
            }],
        );
        snap.corp_medals.insert(
            corp_id,
            vec![CorpMedalEntry {
                medal_id: 10,
                title: "Valor".into(),
                description: "Frontline".into(),
                creator_id: char_id,
                created_at_unix: now.saturating_sub(1_000),
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![CorpIssuedMedalEntry {
                medal_id: 10,
                character_id: char2_id,
                reason: "Fleet tackle".into(),
                status: "public".into(),
                issuer_id: char_id,
                issued_at_unix: now,
            }],
        );
        snap.corp_member_limits.insert(corp_id, 250);
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Fleet Officer".into(),
                roles: vec!["Director".into()],
                grantable_roles: vec![],
                roles_at_hq: vec![],
                grantable_roles_at_hq: vec![],
                roles_at_base: vec![],
                grantable_roles_at_base: vec![],
                roles_at_other: vec![],
                grantable_roles_at_other: vec![],
            }],
        );
        snap.corp_member_titles.insert((corp_id, char_id), vec![1]);
        snap.corp_role_assignments.insert(
            (corp_id, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: 1,
                grantable_roles: 1,
                roles_at_hq: 1,
                grantable_roles_at_hq: 1,
                roles_at_base: 1,
                grantable_roles_at_base: 1,
                roles_at_other: 1,
                grantable_roles_at_other: 1,
            },
        );
        snap.corp_role_history.insert(
            corp_id,
            vec![CorpRoleHistoryEntry {
                character_id: char2_id,
                changed_at_unix: now,
                issuer_id: char_id,
                role_type: "roles".into(),
                old_roles: vec![],
                new_roles: vec!["Hangar_Take_1".into()],
            }],
        );
        snap.corp_shareholders.insert(
            corp_id,
            vec![CorpShareholderEntry {
                shareholder_id: char_id,
                shareholder_type: "character".into(),
                share_count: 1000,
            }],
        );
        snap.corp_projects.insert(
            701,
            CorpProjectRecord {
                project_id: 701,
                corporation_id: corp_id,
                name: "Deliver Tritanium".into(),
                target_quantity: 10_000,
                current_quantity: 2_500,
                isk_per_unit: IskCents::from_isk(5),
                escrow_remaining: IskCents::from_isk(37_500),
            },
        );
        snap.corp_project_contributors.insert(
            701,
            vec![CorpProjectContributorEntry {
                character_id: char_id,
                contributed_quantity: 2_500,
            }],
        );
        snap.standing_transactions.insert(
            char_id,
            vec![StandingTransactionRecord {
                event_id: 1,
                from_id: 1_000_035,
                to_id: corp_id,
                modification: 0.5,
                event_type_id: 151,
            }],
        );
        snap.corp_wallet_transactions.insert(
            (corp_id, 1),
            snap.char_wallet_transactions[&char_id].clone(),
        );
        snap.loyalty_store_offers.insert(
            corp_id,
            vec![LoyaltyStoreOfferRecord {
                offer_id: 1,
                type_id: 587,
                quantity: 1,
                lp_cost: 500,
                isk_cost: 100_000,
                ak_cost: None,
                required_items: vec![LoyaltyStoreOfferItemRecord {
                    type_id: 34,
                    quantity: 100,
                }],
            }],
        );
    });

    for path in [
        format!("/latest/corporations/{corp_id}"),
        format!("/latest/corporations/{corp_id}/blueprints"),
        format!("/latest/corporations/{corp_id}/facilities"),
        format!("/latest/corporations/{corp_id}/structures"),
        format!("/latest/corporations/{corp_id}/members"),
        format!("/latest/corporations/{corp_id}/membertracking"),
        format!("/latest/corporations/{corp_id}/roles"),
        format!("/latest/corporations/{corp_id}/roles/history"),
        format!("/latest/corporations/{corp_id}/standings"),
        format!("/latest/corporations/{corp_id}/alliancehistory"),
        format!("/latest/corporations/{corp_id}/containers/logs"),
        format!("/latest/corporations/{corp_id}/divisions"),
        format!("/latest/corporations/{corp_id}/medals"),
        format!("/latest/corporations/{corp_id}/medals/issued"),
        format!("/latest/corporations/{corp_id}/members/limit"),
        format!("/latest/corporations/{corp_id}/titles"),
        format!("/latest/corporations/{corp_id}/members/titles"),
        format!("/latest/corporations/{corp_id}/shareholders"),
        format!("/latest/corporations/{corp_id}/starbases"),
        format!("/latest/corporations/{corp_id}/starbases/{starbase_id}?system_id=30000142"),
        format!("/latest/corporations/{corp_id}/projects"),
        format!("/latest/corporations/{corp_id}/wallets/1/journal"),
        format!("/latest/corporations/{corp_id}/wallets/1/transactions"),
        format!("/latest/loyalty/stores/{corp_id}/offers"),
        "/latest/alliances/99000001".to_string(),
        "/latest/alliances/99000001/corporations".to_string(),
        "/latest/alliances/99000001/icons".to_string(),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", &path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
    }
}

#[test]
fn esi_markets_contracts_and_industry_mining_pi_pocos_populated() {
    let (ctx, token, char_id, corp_id, planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let structure_id = 1_055_000_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&66_001) {
            c.assignee_id = char_id;
        }
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.type_id = 35_833;
            st.quantum_core_installed = true;
            st.fuel_blocks = 500;
        }
        snap.items.insert(
            1_055_000_099,
            InventoryItemRecord {
                item_id: 1_055_000_099,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(1_055_000_099, true);
        snap.market_order_meta.insert(
            88_001,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(60),
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: None,
            },
        );
        // Corp structure buy order + historical expired order
        snap.market_orders.insert(
            88_002,
            MarketOrderRecord {
                order_id: 88_002,
                owner_id: corp_id,
                station_id: structure_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 2_000,
                vol_remaining: 1_500,
                min_volume: 10,
                is_npc: false,
                escrow: IskCents::from_isk(6_000),
            },
        );
        snap.market_order_meta.insert(
            88_002,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(120),
                duration_days: 30,
                range: "region".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            88_003,
            MarketOrderRecord {
                order_id: 88_003,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(6),
                vol_entered: 100,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            88_003,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(3600),
                duration_days: 7,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: Some("expired".into()),
            },
        );
        snap.market_history.insert(
            (10_000_002, 34),
            vec![MarketHistoryDayRecord {
                date: "2026-01-15".into(),
                average: 4.5,
                highest: 5.0,
                lowest: 4.0,
                order_count: 12,
                volume: 50_000,
                open_price: Some(4.2),
                close_price: Some(4.8),
            }],
        );
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 2_500,
            date: "2026-01-15".into(),
            timestamp_unix: now,
        });
        snap.moon_extractions.insert(
            structure_id,
            MoonExtractionRecord {
                structure_id,
                moon_id: 40_009_049,
                extraction_start_unix: now.saturating_sub(3_600),
                chunk_arrival_unix: now + 3_600,
                natural_decay_unix: now + 10_800,
                status: "Started".into(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(45_490, 1.0)],
                spawned_asteroid_ids: vec![],
            },
        );
        snap.observer_mining_ledger.insert(
            structure_id,
            vec![ObserverMiningRecord {
                observer_id: structure_id,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 45_490,
                quantity: 1_200,
                last_updated_date: "2026-01-15".into(),
                timestamp_unix: now,
            }],
        );
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id as i64),
            PlanetaryColonyLayoutRecord {
                pins: vec![],
                links: vec![],
                routes: vec![],
            },
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![CustomsOfficeRecord {
                office_id: 1_055_000_888,
                system_id: 30_000_142,
                reinforce_exit_start: 18,
                reinforce_exit_end: 22,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: Some("neutral".into()),
                corporation_tax_rate: Some(0.05),
                alliance_tax_rate: Some(0.05),
                excellent_standing_tax_rate: Some(0.01),
                good_standing_tax_rate: Some(0.02),
                neutral_standing_tax_rate: Some(0.05),
                bad_standing_tax_rate: Some(0.10),
                terrible_standing_tax_rate: Some(0.20),
            }],
        );
    });

    for path in [
        "/latest/markets/prices".to_string(),
        "/latest/markets/10000002/orders?order_type=buy&type_id=34".to_string(),
        "/latest/markets/10000002/orders?order_type=sell".to_string(),
        "/latest/markets/10000002/history?type_id=34".to_string(),
        "/latest/markets/10000002/types".to_string(),
        format!("/latest/markets/structures/{structure_id}"),
        format!("/latest/characters/{char_id}/orders"),
        format!("/latest/characters/{char_id}/orders/history"),
        format!("/latest/corporations/{corp_id}/orders"),
        format!("/latest/corporations/{corp_id}/orders/history"),
        format!("/latest/characters/{char_id}/contracts"),
        format!("/latest/characters/{char_id}/contracts/66001/items"),
        format!("/latest/characters/{char_id}/contracts/66001/bids"),
        "/latest/contracts/public/10000002".to_string(),
        "/latest/contracts/public/items/66001".to_string(),
        "/latest/contracts/public/bids/66001".to_string(),
        format!("/latest/corporations/{corp_id}/contracts"),
        format!("/latest/corporations/{corp_id}/contracts/66001/items"),
        format!("/latest/corporations/{corp_id}/contracts/66001/bids"),
        "/latest/industry/facilities".to_string(),
        format!("/latest/characters/{char_id}/industry/jobs?include_completed=true"),
        format!("/latest/corporations/{corp_id}/industry/jobs?include_completed=true"),
        format!("/latest/characters/{char_id}/mining"),
        format!("/latest/corporation/{corp_id}/mining/extractions"),
        format!("/latest/corporation/{corp_id}/mining/observers"),
        format!("/latest/corporation/{corp_id}/mining/observers/{structure_id}"),
        format!("/latest/characters/{char_id}/planets/{planet_id}"),
        format!("/latest/corporations/{corp_id}/customs_offices"),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", &path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
    }

    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/latest/markets/10000002/orders?order_type=invalid"))
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(&EsiHttpRequest::new("GET", "/latest/markets/10000002/orders?type_id=abc"))
            .status_code,
        400
    );
}
