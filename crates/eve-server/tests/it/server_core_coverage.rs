//! Integration coverage tests for `eve-server` ESI routes, runtime state,
//! OpenAPI router dispatch, SSO endpoints, and core non-service subsystems.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceLifecycleMeta, AllianceRecord, ContractRecord, CorpAllianceHistoryEntry,
    CorpContainerLogEntry, CorpIssuedMedalEntry, CorpMedalEntry, CorpProjectContributorEntry,
    CorpProjectRecord, CorpRoleHistoryEntry, CorpShareholderEntry, CorpTitleEntry,
    CorpWalletJournalRecord, CustomsOfficeRecord, FleetMemberRecord, FleetRecord, FleetRole,
    FwCharacterStatsRecord, FwWindowedStat, IncursionRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, IskCents, KillmailRecord, MarketOrderMeta, MarketOrderRecord,
    SovCampaignScoreRecord, StarbaseDetailSettings, StructureEsiTimerMeta, TrainedSkillRecord,
    UpwellStructureRecord, WalletJournalEntry, WalletTransactionRecord, WarAllyRecord,
    WarLifecycleMeta, WarRecord, WorldRepository,
};
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| ServerContext::new().sde))
}

fn all_scopes() -> Vec<String> {
    let set: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    set.into_iter().collect()
}

fn mint_token(ctx: &ServerContext, username: &str, pass: &str, char_id: i64) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            username,
            pass,
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            all_scopes(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token
}

fn esi_call(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

fn seed_rich_ctx() -> (Arc<ServerContext>, String, String, i64, i64, i64, i64, i64) {
    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(
            shared_sde(),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED),
    );
    let char_id = 90_000_001_i64;
    let char2_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    let structure_id = 1_055_000_001_i64;
    let starbase_id = 1_055_000_100_i64;
    let fuel_item_id = 1_055_000_104_i64;
    let poco_id = 1_055_000_105_i64;
    let tcu_id = 9_001_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(ch2) = snap.characters.get_mut(&char2_id) {
            ch2.user_id = 1;
            ch2.corporation_id = corp_id;
            ch2.alliance_id = Some(alliance_id);
            ch2.station_id = Some(60_003_760);
        }
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.active_implants.insert(char_id, vec![19540]);

        let mut corp = snap.corporations.get(&corp_id).unwrap().clone();
        corp.corporation_id = corp_id;
        corp.name = "ESI Cov Corp".to_string();
        corp.ticker = "ESCC".to_string();
        corp.ceo_id = char_id;
        corp.alliance_id = Some(alliance_id);
        corp.members.insert(char_id);
        corp.members.insert(char2_id);
        corp.member_roles.insert(char_id, !0u64);
        corp.member_roles.insert(char2_id, 0);
        snap.corporations.insert(corp_id, corp);

        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "ESI Cov Alliance".into(),
                short_name: "ESCA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            alliance_id,
            AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: now.saturating_sub(10_000),
                faction_id: Some(500_001),
            },
        );
        snap.corp_alliance_history.insert(
            corp_id,
            vec![
                CorpAllianceHistoryEntry {
                    record_id: 1,
                    alliance_id: None,
                    start_date_unix: now.saturating_sub(20_000),
                    is_deleted: None,
                },
                CorpAllianceHistoryEntry {
                    record_id: 2,
                    alliance_id: Some(alliance_id),
                    start_date_unix: now.saturating_sub(10_000),
                    is_deleted: Some(false),
                },
            ],
        );
        snap.corp_container_logs.insert(
            corp_id,
            vec![CorpContainerLogEntry {
                logged_at_unix: now.saturating_sub(120),
                container_id: 100_099_100,
                container_type_id: 17_366,
                character_id: char_id,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".into(),
                action: "lock".into(),
                password_type: Some("config".into()),
                type_id: Some(34),
                quantity: Some(500),
                old_config_bitmask: Some(1),
                new_config_bitmask: Some(3),
            }],
        );
        snap.corp_hangar_divisions.insert(
            corp_id,
            HashMap::from([(1, "Main Hangar".into()), (2, "Ops Hangar".into())]),
        );
        snap.corp_medals.insert(
            corp_id,
            vec![CorpMedalEntry {
                medal_id: 401,
                title: "Founders Cross".into(),
                description: "Founding member".into(),
                creator_id: char_id,
                created_at_unix: now.saturating_sub(5000),
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![CorpIssuedMedalEntry {
                medal_id: 401,
                character_id: char2_id,
                reason: "Loyalty".into(),
                status: "public".into(),
                issuer_id: char_id,
                issued_at_unix: now.saturating_sub(4000),
            }],
        );
        snap.corp_member_titles.insert((corp_id, char_id), vec![1]);
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Fleet Boss".into(),
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
        snap.corp_role_history.insert(
            corp_id,
            vec![CorpRoleHistoryEntry {
                character_id: char2_id,
                changed_at_unix: now.saturating_sub(300),
                issuer_id: char_id,
                role_type: "roles".into(),
                old_roles: vec![],
                new_roles: vec!["Accountant".into()],
            }],
        );
        snap.corp_shareholders.insert(
            corp_id,
            vec![CorpShareholderEntry {
                shareholder_id: char_id,
                shareholder_type: "character".into(),
                share_count: 800,
            }],
        );
        snap.corp_projects.insert(
            801,
            CorpProjectRecord {
                project_id: 801,
                corporation_id: corp_id,
                name: "Mine Scordite".into(),
                target_quantity: 1_000,
                current_quantity: 250,
                isk_per_unit: IskCents::from_isk(100),
                escrow_remaining: IskCents::from_isk(75_000),
            },
        );
        snap.corp_project_contributors.insert(
            801,
            vec![CorpProjectContributorEntry {
                character_id: char_id,
                contributed_quantity: 250,
            }],
        );

        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Astrahus Cov".into(),
                position: [10_000.0, 0.0, 0.0],
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
        snap.structure_esi_timer_meta.insert(
            structure_id,
            StructureEsiTimerMeta {
                profile_id: 12,
                fuel_expires_unix: Some(now + 86_400),
                state_timer_start_unix: Some(now),
                state_timer_end_unix: Some(now + 3600),
                unanchors_at_unix: Some(now + 172_800),
                next_reinforce_hour: Some(18),
                next_reinforce_apply_unix: Some(now + 86_400),
            },
        );
        snap.structures.insert(
            tcu_id,
            UpwellStructureRecord {
                structure_id: tcu_id,
                type_id: 32_226,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Cov TCU".into(),
                position: [0.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: now + 3_600,
                ..Default::default()
            },
        );
        snap.sov_campaign_scores.insert(
            tcu_id,
            SovCampaignScoreRecord {
                structure_id: tcu_id,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.incursions.insert(
            20_000_020,
            IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.75,
                state: "Mobilizing".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );

        // Starbase tower + fuel + settings + customs office + blueprints + assets
        snap.items.insert(
            starbase_id,
            InventoryItemRecord {
                item_id: starbase_id,
                type_id: 12_235,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(starbase_id, [50_000.0, 10_000.0, 0.0]);
        snap.items.insert(
            fuel_item_id,
            InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: 4051,
                owner_id: corp_id,
                location_id: starbase_id,
                flag: InventoryFlag::None,
                quantity: 120,
                singleton: false,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                attack_standing_threshold: Some(5.0),
                attack_security_status_threshold: Some(-2.0),
                ..StarbaseDetailSettings::default()
            },
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![CustomsOfficeRecord {
                office_id: poco_id,
                system_id: 30_000_142,
                reinforce_exit_start: 18,
                reinforce_exit_end: 20,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: Some("neutral".into()),
                corporation_tax_rate: Some(0.01),
                alliance_tax_rate: Some(0.02),
                excellent_standing_tax_rate: Some(0.03),
                good_standing_tax_rate: Some(0.04),
                neutral_standing_tax_rate: Some(0.05),
                bad_standing_tax_rate: Some(0.10),
                terrible_standing_tax_rate: Some(0.15),
            }],
        );
        snap.items.insert(
            1_055_000_101,
            InventoryItemRecord {
                item_id: 1_055_000_101,
                type_id: 681,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(10),
            },
        );
        snap.items.insert(
            1_055_000_102,
            InventoryItemRecord {
                item_id: 1_055_000_102,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(1_055_000_102, [1_200.0, -3_400.0, 5_600.0]);
        snap.items.insert(
            1_055_000_103,
            InventoryItemRecord {
                item_id: 1_055_000_103,
                type_id: 34,
                owner_id: char_id,
                location_id: 1_055_000_102,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                dynamic_attributes: HashMap::new(),
                version: 1,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );

        // Contracts
        snap.contracts.insert(
            700_001,
            ContractRecord {
                contract_id: 700_001,
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(1_000_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![1_055_000_103],
                expiration_unix: now + 86_400,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                availability: "Public".into(),
                buyout: IskCents::from_isk(5_000_000),
                current_bid: IskCents::from_isk(1_500_000),
                current_bidder_id: Some(char2_id),
                requested_items: vec![(34, 100)],
                bid_history: vec![(char2_id, IskCents::from_isk(1_500_000), now - 50)],
                date_issued_unix: now - 100,
                title: "Auction Cov".into(),
                for_corp: false,
                num_days: 7,
            },
        );
        snap.contracts.insert(
            700_002,
            ContractRecord {
                contract_id: 700_002,
                issuer_id: char_id,
                acceptor_id: Some(corp_id),
                contract_type: "ItemExchange".into(),
                status: "InProgress".into(),
                start_station_id: structure_id,
                end_station_id: structure_id,
                price: IskCents::from_isk(250_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![1_055_000_101],
                expiration_unix: now + 86_400,
                issuer_corp_id: corp_id,
                assignee_id: corp_id,
                availability: "Corporation".into(),
                buyout: IskCents(0),
                current_bid: IskCents(0),
                current_bidder_id: None,
                requested_items: Vec::new(),
                bid_history: Vec::new(),
                date_issued_unix: now - 200,
                title: "Corp Contract".into(),
                for_corp: true,
                num_days: 3,
            },
        );

        // Market orders + Fortizar Market Hub
        let fortizar_id = 1_055_000_002_i64;
        snap.structures.insert(
            fortizar_id,
            UpwellStructureRecord {
                structure_id: fortizar_id,
                type_id: 35_833,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Fortizar Market".into(),
                position: [20_000.0, 0.0, 0.0],
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
        snap.items.insert(
            1_055_000_106,
            InventoryItemRecord {
                item_id: 1_055_000_106,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: fortizar_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(1_055_000_106, true);

        snap.market_orders.insert(
            800_001,
            MarketOrderRecord {
                order_id: 800_001,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 1_000,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            800_001,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(300),
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: None,
            },
        );
        snap.market_orders.insert(
            800_002,
            MarketOrderRecord {
                order_id: 800_002,
                owner_id: corp_id,
                station_id: fortizar_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 2_000,
                vol_remaining: 1_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(4_000),
            },
        );
        snap.market_order_meta.insert(
            800_002,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(200),
                duration_days: 90,
                range: "region".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );

        // Wallet journal/tx & Corp wallet journal/tx
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 99_001,
            character_id: char_id,
            entry_type: "bounty_prizes".into(),
            amount: IskCents::from_isk(50_000),
            balance_after: IskCents::from_isk(1_050_000),
            reason: "Rat".into(),
        });
        snap.char_wallet_transactions
            .entry(char_id)
            .or_default()
            .push(WalletTransactionRecord {
                transaction_id: 99_101,
                date_unix: now,
                location_id: 60_003_760,
                type_id: 34,
                unit_price: 5.0,
                quantity: 100,
                client_id: char2_id,
                is_buy: true,
                is_personal: true,
                journal_ref_id: 99_001,
            });
        snap.corp_wallet_journals
            .entry((corp_id, 1))
            .or_default()
            .push(CorpWalletJournalRecord {
                id: 99_201,
                date_unix: now,
                ref_type: "corporation_account_withdrawal".into(),
                description: "Ops".into(),
                amount: 10_000.0,
                balance: 5_000_000.0,
                first_party_id: Some(corp_id),
                second_party_id: Some(char_id),
                reason: Some("Ops".into()),
            });
        snap.corp_wallet_transactions
            .entry((corp_id, 1))
            .or_default()
            .push(WalletTransactionRecord {
                transaction_id: 99_202,
                date_unix: now,
                location_id: 60_003_760,
                type_id: 34,
                unit_price: 4.5,
                quantity: 200,
                client_id: char2_id,
                is_buy: false,
                is_personal: false,
                journal_ref_id: 99_201,
            });

        // Industry, FW, War, Killmail
        snap.industry_jobs.insert(
            940_001,
            IndustryJobRecord {
                job_id: 940_001,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 1_055_000_101,
                blueprint_type_id: 681,
                activity_id: 1,
                runs: 2,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix: now.saturating_sub(100),
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(1_000.0),
                licensed_runs: Some(10),
                probability: Some(1.0),
                completed_unix: None,
                completed_character_id: None,
            },
        );
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now.saturating_sub(3_600)),
                current_rank: 2,
                highest_rank: 3,
                kills: FwWindowedStat {
                    yesterday: 1,
                    last_week: 2,
                    total: 5,
                },
                victory_points: FwWindowedStat {
                    yesterday: 100,
                    last_week: 250,
                    total: 800,
                },
            },
        );
        snap.wars.insert(
            950_001,
            WarRecord {
                war_id: 950_001,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: now.saturating_sub(500),
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            950_001,
            WarLifecycleMeta {
                declared_unix: Some(now.saturating_sub(600)),
                started_unix: Some(now.saturating_sub(500)),
                retracted_unix: None,
                finished_unix: None,
                open_for_allies: Some(true),
                allies: vec![WarAllyRecord {
                    corporation_id: Some(1_000_125),
                    alliance_id: None,
                }],
            },
        );
        snap.killmails.insert(
            960_001,
            KillmailRecord {
                kill_id: 960_001,
                victim_character_id: char2_id,
                attacker_character_id: char_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 250_000.0,
                killed_unix: now.saturating_sub(100),
                victim_corp_id: corp_id,
                victim_alliance_id: Some(alliance_id),
                attacker_corp_id: corp_id,
                attacker_alliance_id: Some(alliance_id),
            },
        );
    });

    // Online session for UI routes
    let _ = ctx
        .db
        .acquire_online_lock(1, 77_001, Some(char_id), false, now);
    let tok1 = mint_token(&ctx, "demo", "demo-secret", char_id);
    let tok2 = mint_token(&ctx, "demo", "demo-secret", char2_id);
    (
        ctx,
        tok1,
        tok2,
        char_id,
        char2_id,
        corp_id,
        alliance_id,
        structure_id,
    )
}

#[test]
fn esi_character_corp_alliance_and_fleet_routes_full_paths() {
    let (ctx, tok1, tok2, cid1, cid2, corp_id, alliance_id, structure_id) = seed_rich_ctx();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let starbase_id = 1_055_000_100_i64;

    // Character affiliation error branches + valid call
    assert_eq!(
        esi_call(&router, "POST", "/characters/affiliation", None, None).0,
        400
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/characters/affiliation",
            None,
            Some(json!([cid1, cid2]))
        )
        .0,
        200
    );

    for path in [
        format!("/characters/{cid1}"),
        format!("/characters/{cid1}/portrait"),
        format!("/characters/{cid1}/standings"),
        format!("/characters/{cid1}/fatigue"),
        format!("/characters/{cid1}/blueprints"),
        format!("/characters/{cid1}/clones"),
        format!("/characters/{cid1}/implants"),
        format!("/characters/{cid1}/skills"),
        format!("/characters/{cid1}/attributes"),
        format!("/characters/{cid1}/skillqueue"),
        format!("/characters/{cid1}/location"),
        format!("/characters/{cid1}/online"),
        format!("/characters/{cid1}/ship"),
        format!("/characters/{cid1}/fittings"),
        format!("/characters/{cid1}/assets"),
        format!("/characters/{cid1}/medals"),
        format!("/characters/{cid1}/roles"),
        format!("/characters/{cid1}/titles"),
        format!("/characters/{cid1}/agents_research"),
        format!("/characters/{cid1}/corporationhistory"),
    ] {
        let (st, _) = esi_call(&router, "GET", &path, Some(&tok1), None);
        assert_eq!(st, 200, "GET {path} failed");
    }

    // Fittings POST + DELETE (invalid body, valid create, delete, missing delete)
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/characters/{cid1}/fittings"),
            Some(&tok1),
            Some(json!({"name": ""}))
        )
        .0,
        400
    );
    let (st_fit, v_fit) = esi_call(
        &router,
        "POST",
        &format!("/characters/{cid1}/fittings"),
        Some(&tok1),
        Some(json!({
            "name": "CovRifter",
            "description": "Cov test fit",
            "ship_type_id": 587,
            "items": [{"type_id": 2873, "flag": "HiSlot0", "quantity": 1}]
        })),
    );
    assert_eq!(st_fit, 201);
    let fit_id = v_fit["fitting_id"].as_i64().unwrap();
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/characters/{cid1}/fittings/{fit_id}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );

    // Assets locations & names for character & corp
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/characters/{cid1}/assets/locations"),
            Some(&tok1),
            Some(json!([1_055_000_102, 1_055_000_103]))
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/characters/{cid1}/assets/names"),
            Some(&tok1),
            Some(json!([1_055_000_102]))
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/assets"),
            Some(&tok1),
            None
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/corporations/{corp_id}/assets/locations"),
            Some(&tok1),
            Some(json!([starbase_id, 1_055_000_101]))
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/corporations/{corp_id}/assets/names"),
            Some(&tok1),
            Some(json!([starbase_id]))
        )
        .0,
        200
    );

    // Alliance & Corp routes
    for path in [
        format!("/alliances"),
        format!("/alliances/{alliance_id}"),
        format!("/alliances/{alliance_id}/corporations"),
        format!("/alliances/{alliance_id}/icons"),
        format!("/corporations/{corp_id}"),
        format!("/corporations/{corp_id}/alliancehistory"),
        format!("/corporations/{corp_id}/blueprints"),
        format!("/corporations/{corp_id}/containers/logs"),
        format!("/corporations/{corp_id}/customs_offices"),
        format!("/corporations/{corp_id}/divisions"),
        format!("/corporations/{corp_id}/facilities"),
        format!("/corporations/{corp_id}/icons"),
        format!("/corporations/{corp_id}/medals"),
        format!("/corporations/{corp_id}/medals/issued"),
        format!("/corporations/{corp_id}/members"),
        format!("/corporations/{corp_id}/members/limit"),
        format!("/corporations/{corp_id}/members/titles"),
        format!("/corporations/{corp_id}/membertracking"),
        format!("/corporations/{corp_id}/roles"),
        format!("/corporations/{corp_id}/roles/history"),
        format!("/corporations/{corp_id}/shareholders"),
        format!("/corporations/{corp_id}/standings"),
        format!("/corporations/{corp_id}/starbases"),
        format!("/corporations/{corp_id}/starbases/{starbase_id}?system_id=30000142"),
        format!("/corporations/{corp_id}/structures"),
        format!("/corporations/{corp_id}/titles"),
    ] {
        let (st, _) = esi_call(&router, "GET", &path, Some(&tok1), None);
        assert_eq!(st, 200, "GET {path} failed");
    }

    // Starbase detail validation error branches
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/starbases/{starbase_id}"),
            Some(&tok1),
            None
        )
        .0,
        400
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/starbases/{starbase_id}?system_id=99999999"),
            Some(&tok1),
            None
        )
        .0,
        404
    );

    // Fleet lifecycle: create fleet via DB seed, then exercise all GET/POST/PUT/DELETE fleet routes
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/fleet"), Some(&tok1), None).0,
        404
    );
    let fleet_id = 600_001_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: cid1,
                members: vec![cid1],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: false,
                is_registered: true,
                wing_details: Default::default(),
                squad_details: Default::default(),
                member_details: HashMap::from([(
                    cid1,
                    FleetMemberRecord {
                        character_id: cid1,
                        role: FleetRole::FleetCommander,
                        wing_id: None,
                        squad_id: None,
                        takes_fleet_warp: true,
                        accepts_regroup: true,
                        accepts_conduit_jumps: true,
                        job: 1,
                        role_name: "Fleet Commander (Boss)".into(),
                        join_time_unix: now,
                    },
                )]),
                invites: Default::default(),
                motd: "Initial".into(),
            },
        );
    });
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/fleet"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/fleets/{fleet_id}"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/fleets/{fleet_id}"),
            Some(&tok1),
            Some(json!({"motd": "Updated MOTD", "is_free_move": true}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/fleets/{fleet_id}/members"), Some(&tok1), None).0,
        200
    );

    // Create/Rename/Delete wing + squad + invite/move/kick member
    let (st_w, v_w) = esi_call(
        &router,
        "POST",
        &format!("/fleets/{fleet_id}/wings"),
        Some(&tok1),
        None,
    );
    assert_eq!(st_w, 201);
    let wid = v_w["wing_id"].as_i64().unwrap();
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/fleets/{fleet_id}/wings/{wid}"),
            Some(&tok1),
            Some(json!({"name": "Alpha"}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/fleets/{fleet_id}/wings/{wid}"),
            Some(&tok1),
            Some(json!({"name": "TooLongWingName123"}))
        )
        .0,
        400
    );
    let (st_sq, v_sq) = esi_call(
        &router,
        "POST",
        &format!("/fleets/{fleet_id}/wings/{wid}/squads"),
        Some(&tok1),
        None,
    );
    assert_eq!(st_sq, 201);
    let sid = v_sq["squad_id"].as_i64().unwrap();
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/fleets/{fleet_id}/squads/{sid}"),
            Some(&tok1),
            Some(json!({"name": "Sq1"}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/fleets/{fleet_id}/wings"), Some(&tok1), None).0,
        200
    );

    // Invite char2 into wing/squad and simulate join + move + kick
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/fleets/{fleet_id}/members"),
            Some(&tok1),
            Some(json!({"character_id": cid2, "role": "squad_member", "wing_id": wid, "squad_id": sid}))
        )
        .0,
        204
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(f) = snap.fleets.get_mut(&fleet_id) {
            if !f.members.contains(&cid2) {
                f.members.push(cid2);
            }
            f.member_details.insert(
                cid2,
                FleetMemberRecord {
                    character_id: cid2,
                    role: FleetRole::SquadMember,
                    wing_id: Some(wid),
                    squad_id: Some(sid),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Member".into(),
                    join_time_unix: now,
                },
            );
        }
    });
    assert_eq!(
        esi_call(&router, "GET", &format!("/fleets/{fleet_id}/members"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/fleets/{fleet_id}/members/{cid2}"),
            Some(&tok1),
            Some(json!({"role": "squad_commander", "wing_id": wid, "squad_id": sid}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/fleets/{fleet_id}/members/{cid1}"),
            Some(&tok1),
            None
        )
        .0,
        403
    );
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/fleets/{fleet_id}/members/{cid2}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/fleets/{fleet_id}/squads/{sid}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/fleets/{fleet_id}/wings/{wid}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );

    // UI routes (online char -> 204, offline char2 -> 404/err)
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/ui/openwindow/contract?contract_id=700001",
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/ui/openwindow/information?target_id={cid1}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/ui/openwindow/marketdetails?type_id=34",
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/ui/openwindow/newmail",
            Some(&tok1),
            Some(json!({"body": "hi", "recipients": [cid2], "subject": "sub"}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/ui/openwindow/information?target_id=587",
            Some(&tok1),
            None
        )
        .0,
        501
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/ui/openwindow/marketdetails?type_id=34",
            Some(&tok2),
            None
        )
        .0,
        204
    );
    let _ = structure_id;
}

#[test]
fn esi_contacts_mail_economy_markets_contracts_sov_fw_and_sample_routes() {
    let (ctx, tok1, _tok2, cid1, cid2, corp_id, alliance_id, structure_id) = seed_rich_ctx();
    let router = EsiRouter::new(Arc::clone(&ctx));

    // Contacts (POST/PUT/GET/DELETE + error branches)
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/characters/{cid1}/contacts?standing=99"),
            Some(&tok1),
            Some(json!([cid2]))
        )
        .0,
        400
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            &format!("/characters/{cid1}/contacts?standing=5.0&watched=true"),
            Some(&tok1),
            Some(json!([cid2]))
        )
        .0,
        201
    );
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/characters/{cid1}/contacts?standing=-5.0"),
            Some(&tok1),
            Some(json!([cid2]))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/contacts"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/characters/{cid1}/contacts/labels"),
            Some(&tok1),
            None
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "DELETE",
            &format!("/characters/{cid1}/contacts?contact_ids={cid2}"),
            Some(&tok1),
            None
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/corporations/{corp_id}/contacts"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/contacts/labels"),
            Some(&tok1),
            None
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/alliances/{alliance_id}/contacts"), Some(&tok1), None)
            .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/alliances/{alliance_id}/contacts/labels"),
            Some(&tok1),
            None
        )
        .0,
        200
    );

    // Mail (POST send -> GET list -> GET detail -> PUT read -> DELETE -> labels & lists)
    let (st_m, v_m) = esi_call(
        &router,
        "POST",
        &format!("/characters/{cid1}/mail"),
        Some(&tok1),
        Some(json!({
            "subject": "Cov Mail",
            "body": "Hello",
            "recipients": [{"recipient_id": cid2, "recipient_type": "character"}],
            "approved_cost": 0
        })),
    );
    assert_eq!(st_m, 201);
    let mid = v_m.as_i64().unwrap();
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/mail"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/mail/{mid}"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "PUT",
            &format!("/characters/{cid1}/mail/{mid}"),
            Some(&tok1),
            Some(json!({"read": true, "labels": [1]}))
        )
        .0,
        204
    );
    assert_eq!(
        esi_call(&router, "DELETE", &format!("/characters/{cid1}/mail/{mid}"), Some(&tok1), None).0,
        204
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/mail/labels"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/mail/lists"), Some(&tok1), None).0,
        200
    );

    // Corp wallets & character wallet journal/tx
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/wallet"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/characters/{cid1}/wallet/journal"), Some(&tok1), None)
            .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/characters/{cid1}/wallet/transactions"),
            Some(&tok1),
            None
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(&router, "GET", &format!("/corporations/{corp_id}/wallets"), Some(&tok1), None).0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/wallets/1/journal"),
            Some(&tok1),
            None
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "GET",
            &format!("/corporations/{corp_id}/wallets/1/transactions"),
            Some(&tok1),
            None
        )
        .0,
        200
    );

    // Economy: Contracts, Markets, Industry
    for path in [
        format!("/characters/{cid1}/contracts"),
        format!("/characters/{cid1}/contracts/700001/items"),
        format!("/characters/{cid1}/contracts/700001/bids"),
        format!("/corporations/{corp_id}/contracts"),
        format!("/corporations/{corp_id}/contracts/700002/items"),
        format!("/corporations/{corp_id}/contracts/700002/bids"),
        "/contracts/public/10000002".to_string(),
        "/contracts/public/items/700001".to_string(),
        "/contracts/public/bids/700001".to_string(),
        "/markets/10000002/orders?order_type=all".to_string(),
        "/markets/10000002/orders?order_type=buy&type_id=34".to_string(),
        "/markets/10000002/orders?order_type=sell".to_string(),
        "/markets/10000002/history?type_id=34".to_string(),
        "/markets/10000002/types".to_string(),
        "/markets/prices".to_string(),
        "/markets/structures/1055000002".to_string(),
        format!("/characters/{cid1}/orders"),
        format!("/characters/{cid1}/orders/history"),
        format!("/corporations/{corp_id}/orders"),
        format!("/corporations/{corp_id}/orders/history"),
        format!("/characters/{cid1}/industry/jobs"),
        format!("/corporations/{corp_id}/industry/jobs"),
    ] {
        let (st, _) = esi_call(&router, "GET", &path, Some(&tok1), None);
        assert_eq!(st, 200, "GET {path} failed");
    }
    assert_eq!(
        esi_call(&router, "GET", "/markets/10000002/orders?order_type=invalid", None, None).0,
        400
    );
    assert_eq!(
        esi_call(&router, "GET", "/contracts/public/99999999", None, None).0,
        404
    );

    // Sov, Incursions, FW, Universe, Wars, Insurance, Status, Governance, Killmails
    for path in [
        "/status".to_string(),
        "/insurance/prices".to_string(),
        "/sovereignty/map".to_string(),
        "/sovereignty/structures".to_string(),
        "/sovereignty/campaigns".to_string(),
        "/incursions".to_string(),
        "/fw/wars".to_string(),
        "/fw/stats".to_string(),
        "/fw/leaderboards".to_string(),
        "/fw/leaderboards/characters".to_string(),
        "/fw/leaderboards/corporations".to_string(),
        format!("/characters/{cid1}/fw/stats"),
        format!("/corporations/{corp_id}/fw/stats"),
        "/universe/factions".to_string(),
        "/universe/bloodlines".to_string(),
        "/universe/ancestries".to_string(),
        "/universe/stations/60003760".to_string(),
        "/universe/structures".to_string(),
        format!("/universe/structures/{structure_id}"),
        "/wars".to_string(),
        "/wars/950001".to_string(),
        "/wars/950001/killmails".to_string(),
        format!("/characters/{cid1}/killmails/recent"),
        format!("/corporations/{corp_id}/killmails/recent"),
        format!("/corporations/{corp_id}/projects"),
    ] {
        let (st, _) = esi_call(&router, "GET", &path, Some(&tok1), None);
        assert_eq!(st, 200, "GET {path} failed");
    }
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/universe/names",
            None,
            Some(json!([34, 30_000_142, cid1, corp_id, alliance_id]))
        )
        .0,
        200
    );
    assert_eq!(
        esi_call(
            &router,
            "POST",
            "/universe/ids",
            None,
            Some(json!(["Tritanium", "Jita"]))
        )
        .0,
        200
    );
}

#[tokio::test]
async fn lock_rank_enforcement_across_fixed_subsystems_and_inversion_panic() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let now = ctx.now_unix();

    // 1. Auth runtime prune + empty lock stack.
    let _pruned = ctx.db.prune_expired_auth_runtime(now);
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());

    // 2. SystemHandle ranked locks + try_sync_ship_ballpark_stats.
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let ship_id = 980_001_001;
    let init_cs = eve_destiny::CombatState {
        shield_hp: 250.0,
        max_shield_hp: 250.0,
        armor_hp: 250.0,
        max_armor_hp: 250.0,
        structure_hp: 250.0,
        max_structure_hp: 250.0,
        capacitor: 500.0,
        max_capacitor: 500.0,
        base_max_velocity: Some(300.0),
        ..Default::default()
    };
    sys.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        init_cs.clone(),
    )
    .await
    .expect("spawn_ball_with_combat");
    let mut updated_cs = init_cs;
    updated_cs.base_max_velocity = Some(410.0);
    assert!(sys.try_sync_ship_ballpark_stats(ship_id, &updated_cs));

    // 3. machoNet.GetMetrics avoids nested cluster_state -> notification_records locks (#910).
    let mut gm = eve_machonet::SessionState::default();
    gm.user_id = Some(1);
    gm.character_id = Some(90_000_001);
    gm.solar_system_id = Some(30_000_142);
    gm.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;
    let metrics = ctx
        .dispatch_rpc(&mut gm, "machoNet", None, "GetMetrics", &[], &HashMap::new(), None)
        .await
        .expect("machoNet.GetMetrics");
    assert!(matches!(metrics, eve_proto::PyRep::List(_)));

    // 4. Consolidated FighterRuntimeState + lenses, CrimeWatchLock + tick_concord_responses, target-tag cleanup.
    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((ship_id, 0), 980_001_002);
    assert_eq!(
        ctx.fighters.read().unwrap().tube_loads.get(&(ship_id, 0)).copied(),
        Some(980_001_002)
    );
    eve_server::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(
        &ctx, ship_id,
    );
    {
        let wg_noop = ctx.crimewatch.write().unwrap();
        let _ = wg_noop.states.len();
    }
    assert!(!ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty));
    eve_server::combat::crimewatch::tick_concord_responses(&ctx, now).await;

    // 5. Explicit lock-rank inversions panic when enforcement is active.
    if cfg!(any(debug_assertions, feature = "lock-rank")) {
        let leaf_world = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _leaf = ctx.pending_notifications.read().unwrap();
            ctx.db.with_snapshot(|_| ());
        }));
        assert!(leaf_world.is_err(), "RANK_LEAF -> RANK_WORLD_INNER must panic");

        let leaf_cw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _leaf = ctx.pending_notifications.read().unwrap();
            let _ = ctx.crimewatch.read();
        }));
        assert!(leaf_cw.is_err(), "RANK_LEAF -> RANK_CRIMEWATCH must panic");

        let leaf_bp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _leaf = ctx.pending_notifications.read().unwrap();
            let _ = sys.ballpark.read();
        }));
        assert!(leaf_bp.is_err(), "RANK_LEAF -> RANK_BALLPARK must panic");

        let leaf_leaf = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _leaf = ctx.pending_notifications.read().unwrap();
            let _ = ctx.notification_records.read();
        }));
        assert!(leaf_leaf.is_err(), "RANK_LEAF -> RANK_LEAF must panic");
    }
    eve_db::lock_rank::assert_no_locks_held();
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());
}
