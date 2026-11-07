use crate::durable_domain_types::{
    CorpGovState, PersistedAllianceMeta, PersistedHubUpgrade, PersistedProjectDetails,
    PersistedSovHubData, PersistedWorkforceMode, SocialContactNotificationRecord,
};
use crate::ledger::{MarketFill, ACCOUNTING_KEY_CASH};
use crate::persistence::WorldRepository;
use crate::types::*;
use crate::{
    AbyssalPersistedSession, DisconnectedShipEntry, IskCents, PersistedCombatHp,
    PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry,
};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_db_path(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("evomu_{label}_{}_{nanos}.sqlite", std::process::id()))
}

#[test]
fn runtime_domain_states_sp6_and_rollback_reload() {
    let path = temp_db_path("runtime_domain_sp6");
    let _ = std::fs::remove_file(&path);

    let repo = WorldRepository::open_sqlite(&path).expect("open sqlite");
    repo.with_mut_snapshot(|snap| {
        super::world_snapshot_sync_rollback_tests::populate_world_core_entities(snap);

        snap.char_contact_notifications.insert(
            2001,
            vec![SocialContactNotificationRecord {
                notification_id: 1,
                sender_character_id: 2002,
                send_date_unix: 1000,
                standing_level: 5.0,
                message: "Fly safe".into(),
            }],
        );
        snap.alliance_war_state
            .project_meta
            .insert(9801, ("Build".into(), true, false));
        snap.war_state.next_negotiation_id = 77;
        snap.fw_runtime.battlefield_npc_waves.insert(1, vec![71001]);
        snap.sov_runtime.sov_hubs.insert(
            1_020_000_000_010,
            PersistedSovHubData {
                hub_id: 1_020_000_000_010,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81600,
                    online: true,
                    low_power: false,
                }],
                fuel: BTreeMap::from([(81143, 500)]),
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(30000144),
                    amount: 100,
                },
                last_updated_unix: 1000,
            },
        );
        snap.sov_runtime.sov_npc_kills.insert(30000142, 42);
        snap.insurgency_runtime.pirate_ambition_modifier.insert(1, 2);
        snap.combat_crimewatch_state.safety_levels.insert(2001, 2);
        snap.combat_crimewatch_state
            .drone_settings
            .insert(2001, (true, false));
        snap.combat_crimewatch_state
            .duel_challenges
            .insert((2001, 2002), 1800);
        snap.combat_crimewatch_state
            .player_bounty_pool
            .insert(2002, 50_000.0);

        snap.sp6_lifecycle.ship_combat_hp.insert(
            5001,
            PersistedCombatHp {
                shield_hp: 100.0,
                armor_hp: 200.0,
                hull_hp: 300.0,
            },
        );
        snap.sp6_lifecycle.space_object_meta.insert(
            5002,
            PersistedSpaceObjectMeta {
                created_unix: Some(1000),
                is_npc_wreck: false,
                anchor_completed: true,
                ..PersistedSpaceObjectMeta::default()
            },
        );
        snap.sp6_lifecycle.safe_logoffs.insert(
            2001,
            SafeLogoffEntry {
                char_id: 2001,
                ship_id: 5001,
                solar_system_id: 30000142,
                started_unix: 1000,
                complete_at_unix: 1030,
                completed: false,
            },
        );
        snap.sp6_lifecycle.self_destructs.insert(
            2001,
            SelfDestructPersistedEntry {
                ship_id: 5001,
                char_id: 2001,
                solar_system_id: 30000142,
                started_unix: 1000,
                detonate_at_unix: 1120,
            },
        );
        snap.sp6_lifecycle.disconnected_ships.insert(
            5001,
            DisconnectedShipEntry {
                char_id: 2001,
                ship_id: 5001,
                ship_type_id: 606,
                solar_system_id: 30000142,
                disconnected_at_unix: 1000,
                remove_at_unix: 2000,
                emergency_warp_dest: Some([10.0, 20.0, 30.0]),
                last_position: [1.0, 2.0, 3.0],
                removed_from_space: false,
                pre_warp_position: Some([0.0, 0.0, 0.0]),
            },
        );
        snap.sp6_lifecycle.abyssal_sessions.insert(
            2001,
            AbyssalPersistedSession {
                char_id: 2001,
                ship_id: 5001,
                pocket_system_id: 32000001,
                origin_system_id: 30000142,
                entered_unix: 1000,
                collapse_at_unix: 2200,
            },
        );
    });

    let err = repo.try_with_mut_snapshot(|snap| {
        snap.char_contact_notifications.clear();
        snap.bookmark_subfolders.clear();
        snap.bookmark_folder_known_by.clear();
        snap.char_notes.clear();
        snap.wh_polarizations.clear();
        snap.mining_ledger.clear();
        snap.observer_mining_ledger.clear();
        snap.system_jump_events.clear();
        snap.system_kill_events.clear();
        snap.fw_wars.clear();
        snap.sp6_lifecycle.ship_combat_hp.clear();
        snap.combat_crimewatch_state.safety_levels.clear();
        snap.sov_runtime.sov_hubs.clear();
        Err::<(), DbError>(DbError::ValidationError("rollback sp6".into()))
    });
    assert!(matches!(err, Err(DbError::ValidationError(_))));
    repo.with_snapshot(|snap| {
        assert_eq!(snap.char_contact_notifications.len(), 1);
        assert_eq!(snap.mining_ledger.len(), 1);
        assert_eq!(snap.sp6_lifecycle.ship_combat_hp.len(), 1);
        assert_eq!(snap.combat_crimewatch_state.safety_levels[&2001], 2);
        assert_eq!(snap.sov_runtime.sov_hubs.len(), 1);
    });
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&path).expect("reopen");
    reopened.with_snapshot(|loaded| {
        assert_eq!(loaded.sp6_lifecycle.ship_combat_hp[&5001].hull_hp, 300.0);
        assert!(loaded.sp6_lifecycle.space_object_meta[&5002].anchor_completed);
        assert_eq!(loaded.sp6_lifecycle.safe_logoffs[&2001].complete_at_unix, 1030);
        assert_eq!(loaded.sp6_lifecycle.self_destructs[&2001].detonate_at_unix, 1120);
        assert_eq!(loaded.sp6_lifecycle.disconnected_ships[&5001].char_id, 2001);
        assert_eq!(loaded.sp6_lifecycle.abyssal_sessions[&2001].pocket_system_id, 32000001);
        assert_eq!(loaded.war_state.next_negotiation_id, 77);
        assert_eq!(loaded.sov_runtime.sov_npc_kills[&30000142], 42);
    });

    let _ = std::fs::remove_file(&path);
}

#[test]
fn corp_wallet_account_restriction_and_auction_expiry_edges() {
    let mut corp = CorporationRecord {
        corporation_id: 3001,
        master_wallet: IskCents(50_000),
        ..CorporationRecord::default()
    };
    corp.ensure_wallet_divisions();
    assert_eq!(corp.division_balance(1), IskCents(50_000));
    corp.set_division_balance(2, IskCents(12_500));
    assert_eq!(corp.division_balance(2), IskCents(12_500));

    let suspended = AccountRecord {
        user_id: 1,
        username: "u".into(),
        password_token: "t".into(),
        role: 1,
        banned_until_unix: Some(200),
        ban_reason: Some("SUSPENDED:cooloff".into()),
    };
    assert!(matches!(
        suspended.check_active_restriction(100),
        Err(DbError::AccountSuspended { until_unix: 200, .. })
    ));
    assert!(suspended.check_active_restriction(250).is_ok());

    let banned = AccountRecord {
        ban_reason: Some("perm".into()),
        ..suspended
    };
    assert!(matches!(
        banned.check_active_restriction(100),
        Err(DbError::AccountBanned { until_unix: 200, .. })
    ));

    let attrs = NeuralAttributes::default();
    assert_eq!(attrs.get_attribute_by_id(164), 19.0);
    assert_eq!(attrs.get_attribute_by_id(168), 20.0);
    assert!(InventoryFlag::ServiceSlot0.is_service_slot());
    assert!(!InventoryFlag::Hangar.is_service_slot());
    assert_eq!(InventoryFlag::from_u16(9999), InventoryFlag::None);

    let mut snap = WorldStateSnapshot::default();
    snap.corporations.insert(3001, corp);
    snap.service_state_blobs
        .insert("contract_deposit:10".into(), "2500".into());
    snap.contracts.insert(
        10,
        ContractRecord {
            contract_id: 10,
            issuer_id: 2001,
            issuer_corp_id: 3001,
            for_corp: true,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            start_station_id: 60003760,
            current_bid: IskCents(9000),
            current_bidder_id: Some(2002),
            expiration_unix: 100,
            ..ContractRecord::default()
        },
    );
    snap.items.insert(
        701,
        InventoryItemRecord {
            item_id: 701,
            owner_id: 10,
            location_id: 10,
            ..InventoryItemRecord::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(10, 150), Some("Completed"));
    assert_eq!(snap.contracts[&10].status, "Completed");
    assert_eq!(snap.items[&701].owner_id, 2002);
    assert_eq!(snap.items[&701].location_id, 60003760);
    assert_eq!(
        snap.corporations[&3001].master_wallet,
        IskCents(50_000 + 9000 + 2500)
    );

    snap.contracts.insert(
        11,
        ContractRecord {
            contract_id: 11,
            issuer_id: 2001,
            issuer_corp_id: 3001,
            for_corp: true,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: 60003760,
            expiration_unix: 100,
            ..ContractRecord::default()
        },
    );
    snap.items.insert(
        702,
        InventoryItemRecord {
            item_id: 702,
            owner_id: 11,
            location_id: 11,
            ..InventoryItemRecord::default()
        },
    );
    assert_eq!(snap.resolve_expired_contract(11, 150), Some("Expired"));
    assert_eq!(snap.items[&702].owner_id, 3001);
    assert_eq!(snap.items[&702].location_id, 60003760);
    let jid = snap.append_journal(2001, "bounty", IskCents(100), IskCents(100), "rat");
    assert!(jid > 0);
}

#[test]
fn repositories_ledger_flags_and_governance_coverage() {
    for raw_flag in 0..=190u16 {
        let _ = InventoryFlag::from_u16(raw_flag);
    }

    let path = temp_db_path("repo_ledger_gov");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open sqlite");

    repo.with_mut_snapshot(|snap| {
        super::world_snapshot_sync_rollback_tests::populate_world_core_entities(snap);

        let mut gov = CorpGovState::default();
        gov.pending_friendly_fire = Some((true, 2000));
        gov.member_limit = Some(100);
        gov.applications_enabled = Some(true);
        gov.action_log.push(("promote".into(), 2001, 1000));
        snap.corp_governance.insert(3001, gov);

        snap.alliance_war_state
            .alliances
            .insert(4001, PersistedAllianceMeta::default());
        snap.alliance_war_state.project_details.insert(
            9801,
            PersistedProjectDetails {
                uuid_bytes: vec![1, 2, 3, 4],
                status: "Active".into(),
                ..PersistedProjectDetails::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            1_020_000_000_011,
            PersistedSovHubData {
                hub_id: 1_020_000_000_011,
                workforce: PersistedWorkforceMode::Import { sources: vec![1] },
                ..PersistedSovHubData::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            1_020_000_000_012,
            PersistedSovHubData {
                hub_id: 1_020_000_000_012,
                workforce: PersistedWorkforceMode::Transit,
                ..PersistedSovHubData::default()
            },
        );

        let tx1 = snap.journal_market_fill(MarketFill {
            buyer_id: 2001,
            seller_id: 3001,
            type_id: 34,
            quantity: 25,
            price: IskCents(600),
            location_id: 60003760,
            date_unix: 1_700_000_000,
            gross: IskCents(15_000),
            buyer_balance_after: Some(IskCents(1_485_000)),
            seller_net: IskCents(14_250),
            seller_balance_after: Some(IskCents(10_014_250)),
            sales_tax: IskCents(750),
            tax_collector_id: 1000125,
            region_id: None,
        });
        assert!(tx1 > 0);

        let tx2 = snap.journal_market_fill(MarketFill {
            buyer_id: 3001,
            seller_id: 2001,
            type_id: 34,
            quantity: 10,
            price: IskCents(500),
            location_id: 60003760,
            date_unix: 1_700_000_010,
            gross: IskCents(5_000),
            buyer_balance_after: None,
            seller_net: IskCents(5_000),
            seller_balance_after: None,
            sales_tax: IskCents(0),
            tax_collector_id: 1000125,
            region_id: Some(10000002),
        });
        assert!(tx2 > tx1);
        assert_eq!(ACCOUNTING_KEY_CASH, 1000);
    });

    // Trigger corp_governance FNV hash update path on second commit
    repo.with_mut_snapshot(|snap| {
        snap.corp_governance.get_mut(&3001).unwrap().company_shares = 500;
    });

    assert_eq!(repo.get_characters_for_user(1001).len(), 1);
    assert!(repo.get_character(2001).is_ok());
    assert!(repo.get_character(999999).is_err());
    assert!(repo.select_character_for_user(1001, 2001).is_ok());
    assert!(matches!(
        repo.select_character_for_user(999, 2001),
        Err(DbError::CharacterOwnershipViolation { .. })
    ));

    let (_uid, uname, _prev) = repo.set_account_role("1001", 6).expect("set role by id");
    assert_eq!(uname, "alice");
    assert!(repo.set_account_role("missing_user", 1).is_err());

    let uid = repo
        .suspend_account_by_id_or_username("1001", 5_000, "test")
        .expect("suspend by id");
    assert_eq!(uid, 1001);
    assert!(repo
        .suspend_account_by_id_or_username("missing_user", 5_000, "x")
        .is_err());

    let kicked_susp = repo.check_session_revoked(1001, 1, 1_000);
    assert_eq!(kicked_susp.as_ref().map(|(k, _)| k.as_str()), Some("AccountSuspended"));

    let _ = repo.revoke_sso_refresh_token("nonexistent_token");
    assert!(repo.check_session_revoked(1001, 1, 6_000).is_none());

    let _ = repo.get_asteroid(71001);
    let _ = repo.get_asteroids_in_system(30000142);

    // Exercise inventory error validation paths
    assert!(matches!(
        repo.move_item_transactional(9999, 5001, 60003760, InventoryFlag::Hangar, None, None),
        Err(DbError::CharacterNotFound(9999))
    ));
    assert!(matches!(
        repo.move_item_transactional(2001, 5001, 5001, InventoryFlag::Hangar, None, None),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    assert!(matches!(
        repo.move_item_transactional(2001, 5002, 60003760, InventoryFlag::Locked, None, None),
        Err(DbError::ItemLocked(5002))
    ));
    assert!(matches!(
        repo.move_item_transactional(2001, 5001, 99_999_999, InventoryFlag::Hangar, None, None),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    assert!(matches!(
        repo.repackage_item(9999, 5001),
        Err(DbError::CharacterNotFound(9999))
    ));
    assert!(matches!(
        repo.repackage_item(2001, 5001),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.repackage_item(2001, 9_999_999),
        Err(DbError::ItemNotFound(9_999_999))
    ));
    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            5010,
            InventoryItemRecord {
                item_id: 5010,
                owner_id: 2001,
                location_id: 60003760,
                type_id: 34,
                quantity: 100,
                flag: InventoryFlag::Hangar,
                singleton: false,
                ..InventoryItemRecord::default()
            },
        );
    });
    assert!(matches!(
        repo.stack_items(2001, 5010, 5010),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.stack_items(2001, 5001, 5010),
        Err(DbError::ItemLocked(5001))
    ));
    assert!(matches!(
        repo.stack_items(2001, 5010, 9_999_999),
        Err(DbError::ItemNotFound(9_999_999))
    ));
    assert!(matches!(
        repo.stack_items(2001, 9_999_999, 5010),
        Err(DbError::ItemNotFound(9_999_999))
    ));

    // Commit full deletion across all persisted collections to exercise every SQLite DELETE path
    repo.with_mut_snapshot(|snap| {
        *snap = WorldStateSnapshot::default();
    });
    drop(repo);

    let cleared = WorldRepository::open_sqlite(&path).expect("reopen after delete commit");
    cleared.with_snapshot(|snap| {
        assert!(snap.accounts.is_empty());
        assert!(snap.planetary_colonies.is_empty());
        assert!(snap.structures.is_empty());
        assert!(snap.asteroids.is_empty());
        assert!(snap.mail_labels.is_empty());
        assert!(snap.corp_shareholders.is_empty());
        assert!(snap.loyalty_store_offers.is_empty());
        assert!(snap.corp_customs_offices.is_empty());
    });

    let _ = std::fs::remove_file(&path);
}

#[test]
fn demo_snapshot_population_populates_all_core_collections() {
    let mut snap = WorldStateSnapshot::default();
    WorldRepository::populate_demo_snapshot(&mut snap);
    assert!(!snap.accounts.is_empty());
    assert!(!snap.characters.is_empty());
    assert!(!snap.items.is_empty());
    assert!(snap.next_entity_id > 0);
}

