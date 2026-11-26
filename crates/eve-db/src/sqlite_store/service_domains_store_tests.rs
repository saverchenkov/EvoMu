use crate::service_domains::{
    AbyssalPocketSession, CrabBeaconPhase, CrabBeaconRecord, EntityKvKey,
    EntityKvScope, IncursionTaleRecord, MobileDeployableRecord, MobileDeployableRuntimeState, Ns,
    TourneyMatch, TourneyStore, DEFAULT_ENTITY_KV_MAX_VALUE_BYTES, MAX_SERVICE_STATE_BLOB_KEYS,
    MAX_SERVICE_STATE_BLOB_TOTAL_BYTES, MAX_SERVICE_STATE_BLOB_VALUE_BYTES,
};
use crate::sqlite_store::sync::sync_snapshot_diff_for_test;
use crate::sqlite_store::{
    apply_migrations, load_snapshot_from_sqlite, load_snapshot_from_sqlite_with_options,
    replace_sqlite_from_snapshot,
};
use crate::types::{OwnerNoteRecord, WorldStateSnapshot};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeMap;

fn temp_db_path(label: &str) -> std::path::PathBuf {
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "evomu_service_domains_{label}_{}_{uniq}.sqlite",
        std::process::id()
    ))
}

#[test]
fn typed_service_domains_sqlite_roundtrip_and_rollback() {
    let path = temp_db_path("roundtrip");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut snap = WorldStateSnapshot::default();
    snap.pve_special_sites.initialized = true;
    snap.pve_special_sites.incursions.insert(
        20000001,
        IncursionTaleRecord {
            tale_id: 42,
            constellation_id: 20000001,
            constellation_name: "Kimotoro".into(),
            staging_solar_system_id: 30000142,
            vanguard_solar_system_ids: vec![30000142],
            assault_solar_system_ids: vec![30000143],
            hq_solar_system_id: 30000144,
            influence: 0.75,
            state: "active".into(),
            mothership_spawned: false,
            mothership_defeated: false,
            sites: vec![],
            lockout_until_filetime: None,
            tale_stop_published: false,
        },
    );
    snap.crab_beacons.beacons.insert(
        9001,
        CrabBeaconRecord {
            beacon_id: 9001,
            type_id: 60244,
            deployer_char_id: 2112000001,
            solar_system_id: 30000142,
            position: [1.0, 2.0, 3.0],
            deployed_at_sec: 1_700_000_000,
            phase: CrabBeaconPhase::Scanning,
            linked_char_id: Some(2112000001),
            linked_ship_id: Some(99001),
            link_started_at_sec: 1_700_000_010,
            link_completes_at_sec: 1_700_000_020,
            scan_completes_at_sec: 1_700_000_620,
            self_destruct_at_sec: 1_700_003_600,
            waves_spawned: 1,
            wave_npc_ball_ids: vec![88001],
            open_container_id: None,
        },
    );
    snap.fleet_and_social_ops.contract_deposits.insert(77, 25_000);
    snap.mining_and_ess_state.mining_ledger_waste.insert(0, 420);
    snap.mining_and_ess_state
        .belt_respawn_due_unix
        .insert(40_009_232, 1_796_468_400);
    snap.quasar_commerce.plex_invoices.push("inv-1".into());

    replace_sqlite_from_snapshot(&mut conn, &snap).expect("replace");
    let loaded = load_snapshot_from_sqlite(&conn).expect("load");
    assert_eq!(loaded.pve_special_sites, snap.pve_special_sites);
    assert_eq!(loaded.crab_beacons, snap.crab_beacons);
    assert_eq!(
        loaded.fleet_and_social_ops.contract_deposits,
        snap.fleet_and_social_ops.contract_deposits
    );
    assert_eq!(
        loaded.mining_and_ess_state.mining_ledger_waste,
        snap.mining_and_ess_state.mining_ledger_waste
    );
    assert_eq!(
        loaded.mining_and_ess_state.belt_respawn_due_unix,
        snap.mining_and_ess_state.belt_respawn_due_unix
    );
    assert_eq!(loaded.quasar_commerce.plex_invoices, vec!["inv-1".to_string()]);

    // Reload restores unmodified domain cleanly from SQLite.
    let reloaded_dom = load_snapshot_from_sqlite(&conn).expect("reload");
    assert_eq!(reloaded_dom.crab_beacons, snap.crab_beacons);
    assert_eq!(reloaded_dom.pve_special_sites, snap.pve_special_sites);
    let mut prev = loaded.clone();
    let mut dirty = loaded.clone();

    // Reverting to default deletes domain rows cleanly across both Quasar and belt respawn state.
    dirty.quasar_commerce.plex_invoices.clear();
    dirty
        .mining_and_ess_state
        .belt_respawn_due_unix
        .remove(&40_009_232);
    let tx = conn.transaction().expect("tx");
    let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &dirty, true).expect("sync");
    assert!(delta.full_snapshot.is_some());
    tx.commit().expect("commit");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1",
            [crate::sqlite_store::domain_rows::DOMAIN_QUASAR_COMMERCE],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 0);
    let belt_due_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1 AND key = 'belt_respawn_due_unix/40009232'",
            [crate::sqlite_store::domain_rows::DOMAIN_MINING_AND_ESS_STATE],
            |r| r.get(0),
        )
        .expect("belt due count");
    assert_eq!(belt_due_count, 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_service_blobs_and_sentinel_owner_notes_migrate_cleanly() {
    let path = temp_db_path("legacy_migrate");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut legacy = WorldStateSnapshot::default();
    let room_stub = |idx: u8| {
        json!({
            "room_index": idx,
            "dungeon_id": 100 + idx as u32,
            "dungeon_name": format!("Room {idx}"),
            "npc_ball_ids": [],
            "cache_item_id": 500 + idx as i64,
            "cleared": false
        })
    };
    let combined_pve = json!({
        "initialized": true,
        "incursions": {
            "20000005": {
                "tale_id": 7,
                "constellation_id": 20000005,
                "constellation_name": "TestConst",
                "staging_solar_system_id": 30000142,
                "vanguard_solar_system_ids": [],
                "assault_solar_system_ids": [],
                "hq_solar_system_id": 30000144,
                "influence": 0.5,
                "state": "active",
                "mothership_spawned": false,
                "mothership_defeated": false,
                "sites": []
            }
        },
        "crab_beacons": {
            "555": {
                "beacon_id": 555,
                "type_id": 60244,
                "deployer_char_id": 1001,
                "solar_system_id": 30000142,
                "position": [10.0, 20.0, 30.0],
                "deployed_at_sec": 100,
                "phase": "Idle",
                "linked_char_id": null,
                "linked_ship_id": null,
                "link_started_at_sec": 0,
                "link_completes_at_sec": 0,
                "scan_completes_at_sec": 0,
                "self_destruct_at_sec": 300,
                "waves_spawned": 0,
                "wave_npc_ball_ids": [],
                "orig_resonances": null,
                "open_container_id": null
            }
        },
        "abyssal_pockets": {
            "1001": {
                "pocket_id": 99,
                "character_id": 1001,
                "ship_id": 8001,
                "origin_solar_system_id": 30000142,
                "abyssal_solar_system_id": 32000001,
                "origin_position": [0.0, 0.0, 0.0],
                "grid_offset": [0.0, 0.0, 0.0],
                "filament_type_id": 47762,
                "tier": 2,
                "weather": "Electrical",
                "weather_penalty_pct": 0.1,
                "weather_bonus_pct": 0.2,
                "npc_difficulty_mult": 1.0,
                "current_room": 0,
                "rooms": [room_stub(0), room_stub(1), room_stub(2)],
                "started_at_sec": 100,
                "collapse_at_sec": 1300,
                "saved_ship_attrs": {},
                "completed": false,
                "collapsed_destroyed": false
            }
        },
        "sim_clock_sec": 77,
        "next_seq": 88
    });
    legacy
        .service_state_blobs
        .insert("incursions_state".into(), combined_pve.to_string());
    legacy
        .service_state_blobs
        .insert("contract_deposit:99".into(), "45000".into());
    let mut waste_map = BTreeMap::new();
    waste_map.insert(3usize, 150i64);
    legacy.service_state_blobs.insert(
        "mining_ledger_waste".into(),
        serde_json::to_string(&waste_map).expect("waste json"),
    );
    legacy
        .service_state_blobs
        .insert("__server_now_unix".into(), "1700000500".into());

    // Sentinel owner_notes for tournaments & deployables.
    let mut tourney_state = TourneyStore::default();
    tourney_state.matches.insert(
        1,
        TourneyMatch::new(1, 10, 30000142, 100, 600, 120, 1),
    );
    legacy.owner_notes.insert(
        -10_000_055,
        OwnerNoteRecord {
            note_id: -10_000_055,
            character_id: -10_000_055,
            item_id: -10_000_055,
            label: "tourney_state".into(),
            note: serde_json::to_string(&tourney_state).expect("tourney json"),
            timestamp_unix: 100,
        },
    );
    let mut dep_store = MobileDeployableRuntimeState::default();
    dep_store.deployables.insert(
        5001,
        MobileDeployableRecord {
            item_id: 5001,
            type_id: 33475,
            group_id: 1246,
            owner_id: 1001,
            solar_system_id: 30000142,
            anchor_start_unix: 100,
            anchor_duration_secs: 60,
            anchor_completed: true,
            is_unanchored: false,
            expires_at_unix: 5000,
            last_interaction_unix: 100,
            reinforced_until_unix: None,
            last_ping_unix: None,
            position: [1.0, 2.0, 3.0],
            deployed_fleet_id: None,
        },
    );
    legacy.owner_notes.insert(
        -8_650_000_000_000,
        OwnerNoteRecord {
            note_id: -8_650_000_000_000,
            character_id: -8_650_000_000_000,
            item_id: -8_650_000_000_000,
            label: "deployables_state".into(),
            note: serde_json::to_string(&dep_store).expect("dep json"),
            timestamp_unix: 100,
        },
    );

    // Seed directly into legacy SQLite tables before typed migration.
    replace_sqlite_from_snapshot(&mut conn, &legacy).expect("replace legacy");

    let snap = load_snapshot_from_sqlite(&conn).expect("load migrated");
    assert!(snap.pve_special_sites.initialized);
    assert!(snap.pve_special_sites.incursions.contains_key(&20000005));
    assert!(snap.crab_beacons.beacons.contains_key(&555));
    assert_eq!(
        snap.abyssal_state
            .pockets
            .get(&1001)
            .map(|p: &AbyssalPocketSession| p.pocket_id),
        Some(99)
    );
    assert!(snap.tourney_state.matches.contains_key(&1));
    assert!(snap.deployable_state.deployables.contains_key(&5001));
    assert_eq!(
        snap.fleet_and_social_ops.contract_deposits.get(&99),
        Some(&45000)
    );
    assert_eq!(
        snap.mining_and_ess_state.mining_ledger_waste.get(&3),
        Some(&150)
    );
    // Legacy migrated keys removed; small opaque metadata retained.
    assert!(!snap.service_state_blobs.contains_key("incursions_state"));
    assert!(!snap.service_state_blobs.contains_key("contract_deposit:99"));
    assert!(!snap.service_state_blobs.contains_key("mining_ledger_waste"));
    assert_eq!(
        snap.service_state_blobs.get("__server_now_unix").map(String::as_str),
        Some("1700000500")
    );
    assert!(!snap.owner_notes.contains_key(&-10_000_055));
    assert!(!snap.owner_notes.contains_key(&-8_650_000_000_000));

    // `load_snapshot_from_sqlite` already flushed migrated typed tables and stripped legacy rows,
    // so an immediate unchanged sync is a clean 0-write no-op.
    let mut prev = snap.clone();
    let tx = conn.transaction().expect("tx");
    let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &snap, true).expect("sync migrated");
    assert!(!delta.dirty);
    assert!(delta.full_snapshot.is_none());
    tx.commit().expect("commit");

    // Reload after startup migration preserves migrated domains.
    let reloaded = load_snapshot_from_sqlite(&conn).expect("reload");
    assert!(reloaded.crab_beacons.beacons.contains_key(&555));
    assert!(reloaded.tourney_state.matches.contains_key(&1));
    assert!(reloaded.deployable_state.deployables.contains_key(&5001));

    let legacy_blobs_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'world_kv_blobs'",
            [],
            |r| r.get(0),
        )
        .expect("check dropped world_kv_blobs");
    assert_eq!(legacy_blobs_exists, 0);

    let incursions_blob_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1 AND key = 'service_state_blobs/incursions_state'",
            [crate::sqlite_store::domain_rows::DOMAIN_SERVICE_STATE_BLOBS],
            |r| r.get(0),
        )
        .expect("check removed incursions_state row");
    assert_eq!(incursions_blob_count, 0);

    let server_now_val: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'service_state_blobs/__server_now_unix'",
            [crate::sqlite_store::domain_rows::DOMAIN_SERVICE_STATE_BLOBS],
            |r| r.get(0),
        )
        .expect("check preserved __server_now_unix row");
    assert!(!server_now_val.is_empty());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn service_state_blobs_enforces_size_and_key_limits() {
    let mut snap = WorldStateSnapshot::default();
    let oversized = "x".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1);
    assert!(snap
        .set_service_state_blob("cache_setting:big".into(), oversized)
        .is_err());

    for i in 0..MAX_SERVICE_STATE_BLOB_KEYS {
        snap.set_service_state_blob(format!("k_{i}"), "ok".into())
            .expect("within key count limit");
    }
    assert!(snap
        .set_service_state_blob("k_overflow".into(), "ok".into())
        .is_err());

    let mut snap_bytes = WorldStateSnapshot::default();
    let chunk = "a".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES);
    let max_chunks = MAX_SERVICE_STATE_BLOB_TOTAL_BYTES / MAX_SERVICE_STATE_BLOB_VALUE_BYTES;
    for i in 0..(max_chunks - 1) {
        snap_bytes
            .set_service_state_blob(format!("c_{i}"), chunk.clone())
            .expect("within total byte limit");
    }
    assert!(snap_bytes
        .set_service_state_blob("c_overflow".into(), chunk)
        .is_err());

    // Direct push bypassing set_service_state_blob is logged and increments counter without aborting sqlite sync (#902).
    let path = temp_db_path("blob_limit");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");
    replace_sqlite_from_snapshot(&mut conn, &WorldStateSnapshot::default()).expect("seed");
    let mut prev = WorldStateSnapshot::default();
    let mut bad = WorldStateSnapshot::default();
    bad.service_state_blobs
        .insert("bad_key".into(), "z".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1));
    let before = crate::service_domains::service_state_blob_limit_violations_total();
    let tx = conn.transaction().expect("tx");
    assert!(sync_snapshot_diff_for_test(&tx, &mut prev, &bad, false).is_ok());
    assert!(crate::service_domains::service_state_blob_limit_violations_total() > before);
    drop(tx);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn entity_kv_sqlite_roundtrip_per_row_diff_and_cascade() {
    // 1. Registry completeness & key serialization roundtrip
    assert_eq!(Ns::ALL.len(), 28);
    for &ns in Ns::ALL {
        let name = ns.as_str();
        assert_eq!(Ns::from_str_name(name), Some(ns));
        assert_eq!(ns.max_value_bytes(), DEFAULT_ENTITY_KV_MAX_VALUE_BYTES);
        assert_eq!(
            ns.cascade_on_owner_delete(),
            !matches!(ns.scope(), EntityKvScope::Global)
        );
        let key = EntityKvKey::new(ns, 90_000_001, "sub:1");
        let encoded = key.to_map_key_string();
        assert_eq!(EntityKvKey::from_map_key_str(&encoded).as_ref(), Ok(&key));
    }
    assert!(Ns::from_str_name("custom.unregistered").is_none());
    assert!(EntityKvKey::from_map_key_str("bad_key_format").is_err());

    let path = temp_db_path("entity_kv_roundtrip");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut snap = WorldStateSnapshot::default();
    snap.kv_mut(Ns::AlphaAvailNotified)
        .set_with_unix(90_000_001, "", &true, 1_700_000_001)
        .expect("set alpha");
    snap.kv_mut(Ns::HomeStationRemoteCooldown)
        .set_with_unix(90_000_001, "", &1_800_000_000_u64, 1_700_000_002)
        .expect("set home cooldown");
    snap.kv_mut(Ns::XmppAuthToken)
        .set_with_unix(90_000_001, "", &"tok-p1", 1_700_000_003)
        .expect("set xmpp token p1");
    snap.kv_mut(Ns::XmppAuthToken)
        .set_with_unix(90_000_002, "", &"tok-p2", 1_700_000_004)
        .expect("set xmpp token p2");
    snap.kv_mut(Ns::ContractOrigFlag)
        .set_with_unix(5001, "99001", &4_u16, 1_700_000_005)
        .expect("set contract flag 1");
    snap.kv_mut(Ns::ContractOrigFlag)
        .set_with_unix(5001, "99002", &116_u16, 1_700_000_006)
        .expect("set contract flag 2");
    snap.kv_mut(Ns::CacheSetting)
        .set_with_unix(0, "zsystem:motd", &"Welcome", 1_700_000_007)
        .expect("set cache setting");

    assert_eq!(
        snap.kv(Ns::ContractOrigFlag).iter_entity(5001).count(),
        2
    );
    assert_eq!(snap.kv(Ns::XmppAuthToken).iter_ns().count(), 2);

    // JSON snapshot serde roundtrip (`entity_kv_map_serde`)
    let json_bytes = serde_json::to_vec(&snap).expect("serialize snapshot json");
    let json_loaded: WorldStateSnapshot =
        serde_json::from_slice(&json_bytes).expect("deserialize snapshot json");
    assert_eq!(json_loaded.entity_kv, snap.entity_kv);

    // SQLite initial replace + load
    replace_sqlite_from_snapshot(&mut conn, &snap).expect("replace");
    let loaded = load_snapshot_from_sqlite(&conn).expect("load");
    assert_eq!(loaded.entity_kv, snap.entity_kv);
    assert_eq!(
        loaded.kv(Ns::HomeStationRemoteCooldown).get::<u64>(90_000_001, ""),
        Some(1_800_000_000)
    );
    assert_eq!(
        loaded
            .kv(Ns::HomeStationRemoteCooldown)
            .get_row(90_000_001, "")
            .map(|r| r.updated_unix),
        Some(1_700_000_002)
    );

    // Incremental per-row diff sync: update 1 row, delete 1 row, insert 1 row
    let mut prev = loaded.clone();
    let mut mutated = loaded.clone();
    mutated
        .kv_mut(Ns::XmppAuthToken)
        .set_with_unix(90_000_001, "", &"tok-p1-rotated", 1_700_000_010)
        .expect("rotate token");
    mutated
        .kv_mut(Ns::ContractOrigFlag)
        .remove(5001, "99001");
    mutated
        .kv_mut(Ns::MarketSeederVersion)
        .set_with_unix(0, "", &"v2-full", 1_700_000_011)
        .expect("set market seeder");

    let tx = conn.transaction().expect("tx");
    let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &mutated, false).expect("incremental sync");
    assert!(delta.dirty);
    tx.commit().expect("commit");

    let after_inc = load_snapshot_from_sqlite(&conn).expect("reload after inc");
    assert_eq!(after_inc.entity_kv, mutated.entity_kv);
    assert_eq!(
        after_inc.kv(Ns::XmppAuthToken).get::<String>(90_000_001, "").as_deref(),
        Some("tok-p1-rotated")
    );
    assert!(!after_inc.kv(Ns::ContractOrigFlag).contains(5001, "99001"));
    assert!(after_inc.kv(Ns::ContractOrigFlag).contains(5001, "99002"));

    // Cascade delete for character 90_000_001 removes all Character-scoped rows for 90_000_001
    // while preserving character 90_000_002, contract 5001, and global settings.
    let removed = mutated.cascade_delete_entity_kv(EntityKvScope::Character, 90_000_001);
    assert_eq!(removed, 3);
    assert!(!mutated.kv(Ns::AlphaAvailNotified).contains(90_000_001, ""));
    assert!(!mutated.kv(Ns::HomeStationRemoteCooldown).contains(90_000_001, ""));
    assert!(!mutated.kv(Ns::XmppAuthToken).contains(90_000_001, ""));
    assert!(mutated.kv(Ns::XmppAuthToken).contains(90_000_002, ""));
    assert!(mutated.kv(Ns::ContractOrigFlag).contains(5001, "99002"));
    assert!(mutated.kv(Ns::CacheSetting).contains(0, "zsystem:motd"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn entity_kv_1000_character_scale_and_per_row_64kib_budget() {
    let path = temp_db_path("entity_kv_scale");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut snap = WorldStateSnapshot::default();
    // 1,000 characters + 1,000 users across 6 namespaces = 6,000 rows (well past legacy 256-key / 64 KiB total blob caps)
    for i in 0..1_000_i64 {
        let cid = 90_000_001 + i;
        let uid = 10_001 + i;
        snap.kv_mut(Ns::AlphaAvailNotified)
            .set_with_unix(cid, "", &true, 1_750_000_000)
            .expect("alpha scale");
        snap.kv_mut(Ns::HomeStationRemoteCooldown)
            .set_with_unix(cid, "", &(1_800_000_000 + i as u64), 1_750_000_000)
            .expect("cooldown scale");
        snap.kv_mut(Ns::XmppAuthToken)
            .set_with_unix(cid, "", &format!("xmpp-secret-token-{cid}"), 1_750_000_000)
            .expect("xmpp scale");
        snap.kv_mut(Ns::CharTransferLastUnix)
            .set_with_unix(cid, "", &1_740_000_000_u64, 1_750_000_000)
            .expect("xfer scale");
        snap.kv_mut(Ns::StarterPlexGranted)
            .set_with_unix(uid, "", &true, 1_750_000_000)
            .expect("starter plex scale");
        snap.kv_mut(Ns::OmegaWeeklySpClaim)
            .set_with_unix(uid, "", &2890_u64, 1_750_000_000)
            .expect("omega claim scale");
    }
    assert_eq!(snap.entity_kv.len(), 6_000);

    replace_sqlite_from_snapshot(&mut conn, &snap).expect("replace 6000 rows");
    let mut prev = snap.clone();
    {
        let tx = conn.transaction().expect("tx");
        let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &snap, false).expect("noop sync before reload");
        assert!(!delta.dirty);
        tx.commit().expect("commit");
    }
    let loaded = load_snapshot_from_sqlite(&conn).expect("load 6000 rows");
    assert_eq!(loaded.entity_kv.len(), 6_000);
    assert_eq!(
        loaded.kv(Ns::XmppAuthToken).get::<String>(90_000_999, "").as_deref(),
        Some("xmpp-secret-token-90000999")
    );

    let mut prev_loaded = loaded.clone();
    let tx = conn.transaction().expect("tx");
    let delta = sync_snapshot_diff_for_test(&tx, &mut prev_loaded, &loaded, false).expect("noop sync");
    assert!(!delta.dirty);
    tx.commit().expect("commit");

    // Per-row 64 KiB budget check: oversized (> 65,536 bytes) and non-JSON values are rejected
    let oversized_str = "a".repeat(DEFAULT_ENTITY_KV_MAX_VALUE_BYTES + 1);
    assert!(snap
        .kv_mut(Ns::CacheSetting)
        .set(0, "oversized", &oversized_str)
        .is_err());
    assert!(snap
        .kv_mut(Ns::CacheSetting)
        .set_raw(0, "invalid_json", "{not_json".into(), 0)
        .is_err());
    assert!(snap
        .kv_mut(Ns::IndustrySystemCostIndicesDaily)
        .set(0, "", &f64::NAN)
        .is_err());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn entity_kv_legacy_blobs_and_945_owner_notes_migration_and_quarantine() {
    let path = temp_db_path("entity_kv_legacy_and_quarantine");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut legacy = WorldStateSnapshot::default();
    legacy
        .service_state_blobs
        .insert("__server_now_unix".into(), "1770000000".into());
    // #945 synthetic negative owner_note + legitimate positive player owner_note
    let pilot_id = 90_000_001_i64;
    let sentinel_nid = -10_000_099 - pilot_id;
    legacy.owner_notes.insert(
        sentinel_nid,
        OwnerNoteRecord {
            note_id: sentinel_nid,
            character_id: -pilot_id,
            item_id: 60_003_760,
            label: "home_station_remote_cooldown".into(),
            note: "1810000000".into(),
            timestamp_unix: 1_770_000_123,
        },
    );
    legacy.owner_notes.insert(
        42,
        OwnerNoteRecord {
            note_id: 42,
            character_id: pilot_id,
            item_id: 60_003_760,
            label: "N:My Note".into(),
            note: "Keep this player note".into(),
            timestamp_unix: 1_770_000_000,
        },
    );

    // Legacy service_state_blobs entries across all migrated namespaces
    legacy.service_state_blobs.insert(
        "alpha_avail_notified".into(),
        json!({ "90000001": true, "90000002": false }).to_string(),
    );
    legacy.service_state_blobs.insert(
        "starter_plex_granted_users".into(),
        json!([1001_u64, 1002_u64]).to_string(),
    );
    legacy
        .service_state_blobs
        .insert("omega_weekly_sp_claim:1001".into(), "2888".into());
    legacy
        .service_state_blobs
        .insert("contract_orig_flag:701:88001".into(), "116".into());
    legacy
        .service_state_blobs
        .insert("contract_bid_div:701:98000001".into(), "3".into());
    legacy.service_state_blobs.insert(
        "char_transfer_consent_token:2002".into(),
        "consent-xyz".into(),
    );
    legacy.service_state_blobs.insert(
        "char_transfer_last_unix:90000001".into(),
        "1765000000".into(),
    );
    legacy.service_state_blobs.insert(
        "char_transfer_evict_session:90000001".into(),
        "1765000005".into(),
    );
    legacy
        .service_state_blobs
        .insert("xmpp:token:90000001".into(), "xmpp-tok-90000001".into());
    legacy
        .service_state_blobs
        .insert("xmpp:help_channel:de".into(), "hilfe_de".into());
    legacy
        .service_state_blobs
        .insert("xmpp:hostname".into(), "xmpp.custom.local".into());
    legacy
        .service_state_blobs
        .insert("cache_setting:zsystem:key1".into(), "val1".into());
    legacy
        .service_state_blobs
        .insert("bannedwords:words".into(), "bad1,bad2".into());
    legacy
        .service_state_blobs
        .insert("bannedwords:regexes".into(), "^spam.*".into());
    legacy.service_state_blobs.insert(
        "kiring:eveguard_server_address".into(),
        "127.0.0.1:9999".into(),
    );
    legacy
        .service_state_blobs
        .insert("community_track_qa_mode".into(), "true".into());
    legacy
        .service_state_blobs
        .insert("zarzakh_horizon_siege_active".into(), "true".into());
    legacy.service_state_blobs.insert(
        "industry_system_cost_indices_daily".into(),
        json!({ "recomputed_at_unix": 1770000000, "active_systems": 5 }).to_string(),
    );
    legacy.service_state_blobs.insert(
        "dynamic_resource_settings".into(),
        json!({ "mainBankRate": 1.25 }).to_string(),
    );
    legacy
        .service_state_blobs
        .insert("market_seeder_version".into(), "v2-full-jita".into());

    replace_sqlite_from_snapshot(&mut conn, &legacy).expect("seed legacy");
    let migrated = load_snapshot_from_sqlite(&conn).expect("load migrated");
    let mut prev = migrated.clone();

    // Verify #945 sentinel note migrated and removed while player note 42 remains intact
    assert!(!migrated.owner_notes.contains_key(&sentinel_nid));
    assert!(migrated.owner_notes.contains_key(&42));
    assert_eq!(
        migrated
            .kv(Ns::HomeStationRemoteCooldown)
            .get::<u64>(pilot_id, ""),
        Some(1_810_000_000)
    );
    assert_eq!(
        migrated.kv(Ns::AlphaAvailNotified).get::<bool>(90_000_001, ""),
        Some(true)
    );
    assert!(!migrated.kv(Ns::AlphaAvailNotified).contains(90_000_002, ""));
    assert_eq!(
        migrated.kv(Ns::StarterPlexGranted).get::<bool>(1001, ""),
        Some(true)
    );
    assert_eq!(
        migrated.kv(Ns::StarterPlexGranted).get::<bool>(1002, ""),
        Some(true)
    );
    assert_eq!(
        migrated.kv(Ns::OmegaWeeklySpClaim).get::<u64>(1001, ""),
        Some(2888)
    );
    assert_eq!(
        migrated.kv(Ns::ContractOrigFlag).get::<u16>(701, "88001"),
        Some(116)
    );
    assert_eq!(
        migrated.kv(Ns::ContractBidDiv).get::<u8>(701, "98000001"),
        Some(3)
    );
    assert_eq!(
        migrated
            .kv(Ns::CharTransferConsentToken)
            .get::<String>(2002, "")
            .as_deref(),
        Some("consent-xyz")
    );
    assert_eq!(
        migrated
            .kv(Ns::CharTransferLastUnix)
            .get::<u64>(90_000_001, ""),
        Some(1_765_000_000)
    );
    assert_eq!(
        migrated
            .kv(Ns::CharTransferEvictSession)
            .get::<u64>(90_000_001, ""),
        Some(1_765_000_005)
    );
    assert_eq!(
        migrated
            .kv(Ns::XmppAuthToken)
            .get::<String>(90_000_001, "")
            .as_deref(),
        Some("xmpp-tok-90000001")
    );
    assert_eq!(
        migrated
            .kv(Ns::XmppHelpChannel)
            .get::<String>(0, "de")
            .as_deref(),
        Some("hilfe_de")
    );
    assert_eq!(
        migrated.kv(Ns::XmppHostname).get::<String>(0, "").as_deref(),
        Some("xmpp.custom.local")
    );
    assert_eq!(
        migrated
            .kv(Ns::CacheSetting)
            .get::<String>(0, "zsystem:key1")
            .as_deref(),
        Some("val1")
    );
    assert_eq!(
        migrated
            .kv(Ns::BannedWords)
            .get::<String>(0, "words")
            .as_deref(),
        Some("bad1,bad2")
    );
    assert_eq!(
        migrated
            .kv(Ns::BannedWords)
            .get::<String>(0, "regexes")
            .as_deref(),
        Some("^spam.*")
    );
    assert_eq!(
        migrated
            .kv(Ns::KiringConfig)
            .get::<String>(0, "eveguard_server_address")
            .as_deref(),
        Some("127.0.0.1:9999")
    );
    assert_eq!(
        migrated.kv(Ns::CommunityTrackQaMode).get::<bool>(0, ""),
        Some(true)
    );
    assert_eq!(
        migrated
            .kv(Ns::ZarzakhHorizonSiegeActive)
            .get::<bool>(0, ""),
        Some(true)
    );
    assert_eq!(
        migrated
            .kv(Ns::MarketSeederVersion)
            .get::<String>(0, "")
            .as_deref(),
        Some("v2-full-jita")
    );
    // All migrated keys were stripped from service_state_blobs, leaving only __server_now_unix
    assert_eq!(migrated.service_state_blobs.len(), 1);
    assert!(migrated.service_state_blobs.contains_key("__server_now_unix"));

    // Immediate sync after startup migration is a 0-write no-op
    let tx = conn.transaction().expect("tx");
    let delta = sync_snapshot_diff_for_test(&tx, &mut prev, &migrated, true).expect("sync");
    assert!(!delta.dirty);
    tx.commit().expect("commit");

    // Inject an unregistered namespace row and a corrupt JSON row directly into SQLite `entity_kv`:
    // fail-closed mode rejects load; quarantine mode quarantines and deletes bad rows.
    conn.execute(
        "INSERT INTO entity_kv (ns, entity_id, sub_key, value, updated_unix) VALUES ('unknown.ns', 1, '', '\"x\"', 0)",
        [],
    )
    .expect("insert unknown ns");
    conn.execute(
        "INSERT INTO entity_kv (ns, entity_id, sub_key, value, updated_unix) VALUES (?1, 999, '', ?2, 0)",
        rusqlite::params![Ns::XmppAuthToken.as_str(), b"{bad-json" as &[u8]],
    )
    .expect("insert corrupt json");

    let fail_closed = load_snapshot_from_sqlite_with_options(&conn, false);
    assert!(fail_closed.is_err());

    let quarantined_snap = load_snapshot_from_sqlite_with_options(&conn, true)
        .expect("quarantine load succeeds");
    assert_eq!(quarantined_snap.entity_kv, migrated.entity_kv);
    let q_rows = crate::list_quarantined_rows(&conn).expect("list quarantined");
    assert!(q_rows.iter().filter(|r| r.source_table == "entity_kv").count() >= 2);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_service_domain_state_migrates_to_domain_rows_and_drops_legacy_table() {
    let path = temp_db_path("legacy_table_drop");
    let conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    // Simulate a pre-#916 SQLite database carrying retired `service_domain_state` rows.
    conn.execute_batch(crate::sqlite_store::service_domains_store::LEGACY_SERVICE_DOMAIN_STATE_SQL)
        .expect("create legacy table");

    let mut legacy_mining = crate::service_domains::MiningAndEssRuntimeStore::default();
    legacy_mining.metenox_last_tick_unix = 1_700_000_777;
    legacy_mining.mining_ledger_waste.insert(4, 1_250);

    let mut legacy_fleet = crate::service_domains::FleetAndSocialOpsStore::default();
    legacy_fleet.contract_deposits.insert(101, 50_000);
    legacy_fleet.mission_taxed_journal.insert(8801);

    let mut legacy_quasar = crate::service_domains::QuasarCommerceStore::default();
    legacy_quasar.plex_invoices.push("inv-legacy".into());
    legacy_quasar.lp_asset_ops_completed.insert("op-1".into());

    for (key, payload) in [
        (
            "mining_and_ess_state",
            serde_json::to_string(&legacy_mining).expect("json mining"),
        ),
        (
            "fleet_and_social_ops",
            serde_json::to_string(&legacy_fleet).expect("json fleet"),
        ),
        (
            "quasar_commerce",
            serde_json::to_string(&legacy_quasar).expect("json quasar"),
        ),
    ] {
        conn.execute(
            "INSERT INTO service_domain_state (domain_key, payload_json) VALUES (?1, ?2)",
            rusqlite::params![key, payload],
        )
        .expect("seed legacy row");
    }

    let migrated = load_snapshot_from_sqlite(&conn).expect("load + migrate");
    assert_eq!(*migrated.mining_and_ess_state, legacy_mining);
    assert_eq!(*migrated.fleet_and_social_ops, legacy_fleet);
    assert_eq!(*migrated.quasar_commerce, legacy_quasar);

    // Retired `service_domain_state` must be dropped while `entity_kv` stays intact and
    // decomposed entries live in `domain_rows`.
    let legacy_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'service_domain_state'",
            [],
            |r| r.get(0),
        )
        .expect("check dropped table");
    assert_eq!(legacy_exists, 0);

    let entity_kv_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'entity_kv'",
            [],
            |r| r.get(0),
        )
        .expect("check entity_kv");
    assert_eq!(entity_kv_exists, 1);

    let migrated_waste: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'mining_ledger_waste/4'",
            [crate::sqlite_store::domain_rows::DOMAIN_MINING_AND_ESS_STATE],
            |r| r.get(0),
        )
        .expect("migrated waste row");
    assert_eq!(migrated_waste, "1250");

    let reloaded = load_snapshot_from_sqlite(&conn).expect("reload post-migration");
    assert_eq!(*reloaded.mining_and_ess_state, legacy_mining);
    assert_eq!(*reloaded.fleet_and_social_ops, legacy_fleet);
    assert_eq!(*reloaded.quasar_commerce, legacy_quasar);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn service_domains_tx_writes_single_row_with_zero_cold_clones_and_rejects_non_finite() {
    use crate::sqlite_store::domain_rows::{
        apply_domain_row_changes, validate_domain_row_changes, DOMAIN_FLEET_AND_SOCIAL_OPS,
        DOMAIN_PVE_SPECIAL_SITES,
    };
    use crate::tx_engine::{ChangeSet, TxError, TxSqlExecutor};

    let path = temp_db_path("tx_row_granularity");
    let _ = std::fs::remove_file(&path);
    let repo = crate::persistence::WorldRepository::open_sqlite(&path).expect("open_sqlite");

    let seed_res: Result<(), TxError> = repo.tx("seed_service_domains", |wtx| {
        wtx.snap.pve_special_sites.initialized = true;
        wtx.snap.pve_special_sites.incursions.insert(
            20_000_001,
            IncursionTaleRecord {
                tale_id: 1,
                constellation_id: 20_000_001,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: 30_000_142,
                vanguard_solar_system_ids: vec![30_000_142],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: 30_000_144,
                influence: 0.5,
                state: "active".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![],
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
        wtx.snap.fleet_and_social_ops.contract_deposits.insert(10, 1_000);
        wtx.snap.fleet_and_social_ops.contract_deposits.insert(20, 2_000);
        Ok(())
    });
    seed_res.expect("seed tx");

    // Mutating 1 entry inside `fleet_and_social_ops` executes with 0 cold domain syncs.
    crate::persistence::reset_thread_cold_domain_syncs_for_test();
    let update_res: Result<(), TxError> = repo.tx("update_one_deposit", |wtx| {
        wtx.snap.fleet_and_social_ops.contract_deposits.insert(10, 9_500);
        Ok(())
    });
    update_res.expect("update tx");
    assert_eq!(
        crate::persistence::thread_cold_domain_syncs_for_test(),
        0,
        "migrated service_domains mutation must trigger 0 cold domain syncs"
    );

    // Direct `ChangeSet::diff` + `apply_domain_row_changes` mutates strictly 1 SQLite row.
    let before_snap = repo.with_snapshot(Clone::clone);
    let mut after_snap = before_snap.clone();
    after_snap.fleet_and_social_ops.contract_deposits.insert(20, 7_777);
    let changes = ChangeSet::diff(2, &before_snap, &after_snap);
    assert_eq!(changes.domain_rows.len(), 1);
    assert!(!changes.cold_domains.service_domains);

    let mut mem_conn = Connection::open_in_memory().expect("open mem sqlite");
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
        "mutating 1 service domain map entry must write strictly 1 SQLite row"
    );
    let row_val: String = mem_conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'contract_deposits/20'",
            [DOMAIN_FLEET_AND_SOCIAL_OPS],
            |r| r.get(0),
        )
        .expect("query updated row");
    assert_eq!(row_val, "7777");

    // Non-finite float (NaN / Inf) inside a service domain fails tx naming domain and key.
    for bad_influence in [f64::NAN, f64::INFINITY] {
        let mut nan_after = before_snap.clone();
        nan_after
            .pve_special_sites
            .incursions
            .get_mut(&20_000_001)
            .unwrap()
            .influence = bad_influence;
        let nan_changes = ChangeSet::diff(3, &before_snap, &nan_after);
        let check_err = validate_domain_row_changes(&nan_changes.domain_rows, 1024 * 1024)
            .expect_err("non-finite must fail validation");
        assert!(
            matches!(check_err, TxError::NonFinite { .. }),
            "expected NonFinite, got: {check_err}"
        );
        let msg = check_err.to_string();
        assert!(msg.contains(DOMAIN_PVE_SPECIAL_SITES), "{msg}");
        assert!(msg.contains("incursions/20000001"), "{msg}");

        let tx_err: Result<(), TxError> = repo.tx("reject_bad_float", |wtx| {
            wtx.snap
                .pve_special_sites
                .incursions
                .get_mut(&20_000_001)
                .unwrap()
                .influence = bad_influence;
            Ok(())
        });
        let err_msg = tx_err.expect_err("repo.tx must fail on NaN/Inf").to_string();
        assert!(err_msg.contains(DOMAIN_PVE_SPECIAL_SITES), "{err_msg}");
        assert!(err_msg.contains("incursions/20000001"), "{err_msg}");
    }
    assert_eq!(
        repo.with_snapshot(|s| s.pve_special_sites.incursions[&20_000_001].influence),
        0.5
    );

    drop(repo);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_service_domains_checked_fails_closed_without_mutating_snapshot() {
    use crate::sqlite_store::domain_rows::{
        DOMAIN_FLEET_AND_SOCIAL_OPS, DOMAIN_PVE_SPECIAL_SITES,
    };
    use crate::sqlite_store::service_domains_store::load_service_domains_checked;
    use crate::sqlite_store::LoadContext;

    let path = temp_db_path("fail_closed_load");
    let conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'sim_clock_sec', '999')",
        [DOMAIN_PVE_SPECIAL_SITES],
    )
    .expect("seed valid pve row");
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'contract_deposits/1', '{corrupt')",
        [DOMAIN_FLEET_AND_SOCIAL_OPS],
    )
    .expect("seed corrupt fleet row");

    let mut snap = WorldStateSnapshot::default();
    snap.pve_special_sites.sim_clock_sec = 42;
    snap.node_graph_state.client_node_graph_seq = 77;
    snap.fleet_and_social_ops.contract_deposits.insert(1, 500);

    let mut ctx = LoadContext::new(false);
    let err = load_service_domains_checked(&conn, &mut snap, &mut ctx).unwrap_err();
    assert!(err.contains(DOMAIN_FLEET_AND_SOCIAL_OPS), "unexpected err: {err}");
    assert_eq!(snap.pve_special_sites.sim_clock_sec, 42);
    assert_eq!(snap.node_graph_state.client_node_graph_seq, 77);
    assert_eq!(snap.fleet_and_social_ops.contract_deposits.get(&1), Some(&500));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn legacy_container_ship_owner_note_migrates_to_per_entity_kv_and_caps_alsc_logs() {
    use crate::service_domains::{
        AlscLogEntryRecord, ContainerConfigRecord, ShipAccessConfigRecord,
        MAX_ALSC_LOGS_PER_CONTAINER,
    };

    let path = temp_db_path("legacy_container_ship_migration");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut legacy_snap = WorldStateSnapshot::default();
    let container_a = 800_001_i64;
    let ship_b = 800_002_i64;
    let locked_item = 800_003_i64;
    let flagged_item = 800_004_i64;
    let char_id = 90_000_001_i64;

    let oversized_logs: Vec<serde_json::Value> = (0..(MAX_ALSC_LOGS_PER_CONTAINER + 60))
        .map(|i| {
            json!({
                "timestamp_unix": 1_700_000_000_u64 + i as u64,
                "actor_id": char_id,
                "action": "ALSCActionPasswordFailure",
                "item_id": container_a,
                "item_type_id": 17_366_u32,
                "quantity": 1_i32
            })
        })
        .collect();

    let legacy_json = json!({
        "container_passwords": { container_a.to_string(): "open-pw" },
        "container_config_passwords": { container_a.to_string(): "cfg-pw" },
        "unlocked_containers": [[container_a, char_id], [char_id, ship_b]],
        "alsc_locked_items": [locked_item],
        "alsc_logs": { container_a.to_string(): oversized_logs },
        "ship_configs": {
            ship_b.to_string(): {
                "allow_fleet_smb": false,
                "allow_corp_smb": true,
                "allow_fleet_hangar": true,
                "allow_corp_hangar": false,
                "password": null
            }
        },
        "ship_passwords": { ship_b.to_string(): "ship-pw" },
        "custom_item_flags": { flagged_item.to_string(): 155_u16 },
        "container_configs": {
            container_a.to_string(): {
                "lock_by_default": true,
                "password_per_action": true,
                "config_bitmask": 3_u32
            }
        }
    })
    .to_string();

    legacy_snap.owner_notes.insert(
        -8_400_000_000_000,
        OwnerNoteRecord {
            note_id: -8_400_000_000_000,
            character_id: -84,
            item_id: 0,
            label: "S:CONTAINER_SHIP_RUNTIME_STATE".into(),
            note: legacy_json,
            timestamp_unix: 1_700_001_000,
        },
    );
    replace_sqlite_from_snapshot(&mut conn, &legacy_snap).expect("seed legacy sqlite");
    drop(conn);

    // Loading via WorldRepository::open_sqlite runs one-time migration and commits the
    // migrated per-entity `entity_kv` rows while removing the legacy `-8_400_000_000_000` note.
    let repo = crate::persistence::WorldRepository::open_sqlite(&path).expect("open_sqlite");
    repo.with_snapshot(|snap| {
        assert!(!snap.owner_notes.contains_key(&-8_400_000_000_000));
        assert_eq!(
            snap.kv(Ns::ContainerPassword).get::<String>(container_a, "1"),
            Some("open-pw".into())
        );
        assert_eq!(
            snap.kv(Ns::ContainerPassword).get::<String>(container_a, "2"),
            Some("cfg-pw".into())
        );
        assert_eq!(
            snap.kv(Ns::ContainerUnlockedChar)
                .get::<bool>(container_a, &char_id.to_string()),
            Some(true)
        );
        assert_eq!(
            snap.kv(Ns::ContainerUnlockedChar)
                .get::<bool>(ship_b, &char_id.to_string()),
            Some(true)
        );
        assert!(snap.is_item_in_alsc_lock(locked_item));
        assert_eq!(
            snap.kv(Ns::CustomItemFlag).get::<u16>(flagged_item, ""),
            Some(155)
        );
        assert_eq!(
            snap.kv(Ns::ContainerConfig)
                .get::<ContainerConfigRecord>(container_a, ""),
            Some(ContainerConfigRecord {
                lock_by_default: true,
                password_per_action: true,
                config_bitmask: 3,
            })
        );
        assert_eq!(
            snap.kv(Ns::ShipAccessConfig)
                .get::<ShipAccessConfigRecord>(ship_b, ""),
            Some(ShipAccessConfigRecord {
                allow_fleet_smb: false,
                allow_corp_smb: true,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("ship-pw".into()),
            })
        );
        let logs = snap
            .kv(Ns::ContainerAlscLogs)
            .get::<Vec<AlscLogEntryRecord>>(container_a, "")
            .expect("migrated logs");
        assert_eq!(logs.len(), MAX_ALSC_LOGS_PER_CONTAINER);
        assert_eq!(logs.first().map(|e| e.timestamp_unix), Some(1_700_000_060));
        assert_eq!(
            logs.last().map(|e| e.timestamp_unix),
            Some(1_700_000_000 + (MAX_ALSC_LOGS_PER_CONTAINER + 59) as u64)
        );
    });
    drop(repo);

    // Verify reopened DB still has migrated entity_kv and zero legacy note in SQLite.
    let reloaded = load_snapshot_from_sqlite(&Connection::open(&path).expect("reopen")).expect("load");
    assert!(!reloaded.owner_notes.contains_key(&-8_400_000_000_000));
    assert_eq!(
        reloaded
            .kv(Ns::ContainerPassword)
            .get::<String>(container_a, "1"),
        Some("open-pw".into())
    );
    assert!(reloaded.is_item_in_alsc_lock(locked_item));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn all_17_service_domains_survive_tx_commit_restart_and_scalar_reset() {
    use crate::persistence::WorldRepository;
    use crate::service_domains::*;
    use crate::tx_engine::TxError;
    use crate::types::IskCents;
    use eve_proto::PyRep;
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

    let path = temp_db_path("all_17_domains_roundtrip");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open_sqlite");

    let expected_pve = PveSpecialSitesState {
        initialized: true,
        incursions: HashMap::from([(
            20_000_001,
            IncursionTaleRecord {
                tale_id: 1,
                constellation_id: 20_000_001,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: 30_000_142,
                ..Default::default()
            },
        )]),
        incursion_lifecycle: HashMap::from([(
            20_000_001,
            IncursionLifecycleMeta {
                spawned_at_sec: 1_700_000_000,
                last_site_completed_sec: 1_700_000_100,
                ..Default::default()
            },
        )]),
        incursion_site_waves: HashMap::from([(
            "site-1".into(),
            IncursionSiteWaveMeta {
                current_wave: 2,
                total_waves: 4,
            },
        )]),
        world_event_tales: HashMap::from([(
            10,
            WorldEventTaleRecord {
                tale_id: 10,
                template_class_id: "event".into(),
                ..Default::default()
            },
        )]),
        incursion_lp_logs: HashMap::from([(
            90_000_001,
            vec![IncursionLPLogEntry {
                log_id: 1,
                character_id: 90_000_001,
                concord_lp_earned: 1_500,
                ..Default::default()
            }],
        )]),
        homefront_sites: HashMap::from([(
            30_000_142,
            vec![HomefrontSiteRecord {
                instance_id: 20,
                dungeon_name: "Raid".into(),
                ..Default::default()
            }],
        )]),
        roaming_storms: vec![RoamingStormRecord {
            storm_id: 30,
            center_solar_system_id: 30_000_142,
            ..Default::default()
        }],
        drifter_hives: HashMap::from([(
            30_000_143,
            DrifterHiveRecord {
                site_id: "hive-1".into(),
                solar_system_id: 30_000_143,
                ..Default::default()
            },
        )]),
        triglavian_sites: HashMap::from([(
            30_000_144,
            vec![TriglavianSiteRecord {
                instance_id: 40,
                solar_system_id: 30_000_144,
                ..Default::default()
            }],
        )]),
        edencom_fortress_systems: vec![30_000_145],
        edencom_minor_victory_systems: vec![30_000_146],
        triglavian_minor_victory_systems: vec![30_000_147],
        interference_by_system: HashMap::from([(
            30_000_142,
            SystemInterferenceRecord {
                solar_system_id: 30_000_142,
                interference_level: 0.4,
                band: 1,
                last_updated_sec: 1_700_000_200,
            },
        )]),
        seasonal_event_sites: HashMap::from([(
            30_000_142,
            vec![SeasonalEventSiteRecord {
                instance_id: 50,
                season_id: 1,
                ..Default::default()
            }],
        )]),
        sim_clock_sec: 12_345,
        next_seq: 99,
    };

    let expected_crab = CrabBeaconStore {
        beacons: HashMap::from([(
            801,
            CrabBeaconRecord {
                beacon_id: 801,
                phase: CrabBeaconPhase::Scanning,
                ..Default::default()
            },
        )]),
    };

    let expected_abyssal = AbyssalRuntimeStore {
        pockets: HashMap::from([(
            901,
            AbyssalPocketSession {
                pocket_id: 901,
                character_id: 90_000_001,
                ship_id: 100_000_001,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: 32_000_001,
                ..Default::default()
            },
        )]),
        traces: HashMap::from([(
            902,
            AbyssalTraceRecord {
                trace_ball_id: 902,
                char_id: 90_000_001,
                ..Default::default()
            },
        )]),
        void_space_sessions: HashMap::from([(
            90_000_002,
            VoidSpaceSessionRecord {
                char_id: 90_000_002,
                ship_id: 100_000_002,
                ..Default::default()
            },
        )]),
        active_clouds: HashMap::from([(903, 50_001)]),
        boundary_ticks: HashMap::from([(100_000_001, 4)]),
        void_space_content: HashMap::from([(904, 7)]),
        deathzone_active_ships: BTreeSet::from([100_000_001]),
        next_seq: 55,
    };

    let expected_dungeon_rt = DungeonRuntimeStore {
        instances: BTreeMap::from([(
            1,
            DungeonRuntimeInstance {
                instance_id: 1,
                dungeon_id: 100,
                dungeon_name: "Deadspace".into(),
                ..Default::default()
            },
        )]),
        homefront_runtimes: BTreeMap::from([(
            2,
            HomefrontRuntimeRecord {
                instance_id: 2,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        )]),
        next_instance_id: 42,
    };

    let expected_dungeon_ed = DungeonEditorStore {
        rooms_by_dungeon: BTreeMap::from([(
            100,
            vec![DungeonEditorRoom {
                room_id: 1,
                room_name: "Entry".into(),
                dungeon_id: 100,
                ..Default::default()
            }],
        )]),
        dungeon_disallow_mwd: BTreeMap::from([(100, true)]),
        active_editors: BTreeMap::from([(
            90_000_001,
            GmActiveEditorState {
                dungeon_id: Some(100),
                godmode: true,
                ..Default::default()
            },
        )]),
        next_custom_room_id: 15,
        next_custom_object_id: 25,
    };

    let expected_fob = FobObservatoryStore {
        observatory_sites: HashMap::from([(
            701,
            ObservatorySiteState {
                site_id: 701,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        )]),
        observatory_respawn_until: HashMap::from([(30_000_142, 1_700_005_000)]),
        fob_combatants: HashMap::from([(702, BTreeSet::from([90_000_001]))]),
        fob_factions: HashMap::from([(702, 500_010)]),
        fob_respawn_until: HashMap::from([(30_000_143, 1_700_006_000)]),
        fob_defense_triggered: BTreeSet::from([702]),
    };

    let expected_mining_ess = MiningAndEssRuntimeStore {
        ess_reserve_pulses: HashMap::from([(
            30_000_142,
            EssReservePulseState {
                pulses_total: 10,
                pulses_remaining: 7,
                ..Default::default()
            },
        )]),
        phased_asteroid_fields: HashMap::from([(
            30_000_143,
            PhasedAsteroidFieldState {
                signature_id: "SIG-001".into(),
                solar_system_id: 30_000_143,
                progress: 0.5,
                phased_in: true,
                ..Default::default()
            },
        )]),
        phased_field_last_tick_unix: 1_700_000_500,
        moon_fractured_fields: vec![(80_000_001, 30_000_142, 1_700_010_000, vec![90_000_001])],
        mining_ledger_waste: BTreeMap::from([(0usize, 125)]),
        metenox_last_tick_unix: 1_700_000_600,
        belt_respawn_due_unix: HashMap::from([(40_000_001, 1_700_008_000)]),
    };

    let expected_pvp = PvpFilamentStore {
        queue: HashMap::from([(
            90_000_001,
            PvpQueueCharacterEntry {
                item_id: 100_000_001,
                type_id: 587,
                ..Default::default()
            },
        )]),
        stats: HashMap::from([(90_000_001, (3, 1, 2, 0, 125_000.0))]),
        event_stats: HashMap::from([(1, HashMap::from([(90_000_001, (1, 0, 1, 0, 50_000.0))]))]),
        events: vec![PvpEventSchedule {
            event_id: 1,
            event_name: "Frigate 1v1".into(),
            ..Default::default()
        }],
        seeded_default_events: true,
        active_matches: HashMap::from([(
            10,
            PvpArenaMatchInstance {
                match_id: 10,
                event_id: 1,
                active: true,
                ..Default::default()
            },
        )]),
        match_teams: HashMap::from([(10, vec![vec![90_000_001], vec![90_000_002]])]),
        match_eliminated: HashMap::from([(10, HashSet::from([90_000_002]))]),
        match_arena_offsets: HashMap::from([(10, [1000.0, 0.0, 0.0])]),
        match_filament_types: HashMap::from([(10, 56_001)]),
        hazard_ticks: HashMap::from([(10, 5)]),
        queue_refund_targets: HashMap::from([(1, (90_000_001, 56_001))]),
        queue_tickets: HashMap::from([(90_000_001, 1)]),
        next_ticket_id: 2,
        origin_positions: HashMap::from([(90_000_001, [10.0, 20.0, 30.0])]),
        next_match_id: 11,
    };

    let expected_tourney = TourneyStore {
        matches: HashMap::from([(
            1,
            TourneyMatch {
                match_id: 1,
                tourney_id: 20,
                red_team_name: "Red".into(),
                blue_team_name: "Blue".into(),
                ..Default::default()
            },
        )]),
        active_match_id: Some(1),
        ban_sessions: HashMap::from([(1, vec![587, 588])]),
        tournaments: BTreeMap::from([(20, "Alliance Tournament".into())]),
        arena_systems: BTreeMap::from([(
            30_000_142,
            TourneyArenaSystem {
                combat_beacons: vec![80_001],
                warp_bubbles: vec![80_002],
            },
        )]),
    };

    let expected_deployable = MobileDeployableRuntimeState {
        deployables: HashMap::from([(
            100_000_050,
            MobileDeployableRecord {
                item_id: 100_000_050,
                type_id: 33_474,
                anchor_completed: true,
                ..Default::default()
            },
        )]),
        damaged_items: HashMap::from([(
            100_000_050,
            PersistedDamageRecord {
                shield_hp: 500.0,
                max_shield_hp: 1_000.0,
                ..Default::default()
            },
        )]),
        observatory_pings_done: HashMap::from([(100_000_050, 2)]),
        cloak_activated_unix: HashMap::from([(100_000_051, 1_700_000_100)]),
        mmju_spools: HashMap::from([(
            100_000_052,
            MmjuSpoolEntry {
                ship_id: 100_000_001,
                char_id: 90_000_001,
                ..Default::default()
            },
        )]),
        siphon_last_cycle_unix: HashMap::from([(100_000_053, 1_700_000_200)]),
        pending_decloak_notifications: vec![PendingDecloakNotice {
            char_id: 90_000_001,
            ship_id: 100_000_001,
            module_id: 200_000_001,
        }],
    };

    let expected_superweapon = PersistedSuperweaponRuntime {
        active_cycles: vec![PersistedSuperweaponCycle {
            sys_id: 30_000_142,
            char_id: 90_000_001,
            ship_id: 100_000_001,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [0.0, 0.0, 0.0],
                sphere_radius_m: 15_000.0,
            },
            started_at_unix: 1_700_000_000,
            warmup_secs: 5,
            total_ticks: 10,
            ticks_applied: 2,
            immobilize_until_unix: 1_700_000_030,
            orig_max_velocity: 120.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: false,
        }],
        warp_suppressions: vec![PersistedWarpSuppression {
            sys_id: 30_000_142,
            ship_id: 100_000_002,
            suppressed_until_unix: 1_700_000_060,
            orig_warp_scramble_strength: 1,
        }],
    };

    let expected_insurgency_ops = InsurgencyComplexOpsState {
        outpost_raids: HashMap::from([(
            501,
            OutpostRaidInstance {
                gate_id: 501,
                campaign_id: 901,
                ..Default::default()
            },
        )]),
        mining_ambushes: HashMap::from([(
            30_000_142,
            MiningAmbushInstance {
                campaign_id: 901,
                solar_system_id: 30_000_142,
                ..Default::default()
            },
        )]),
        subpoint_remainders: HashMap::from([("corruption:30000142".into(), 3)]),
        named_roamer_rolls: HashMap::from([("roamer:1".into(), true)]),
        ice_heist_respawn_until: HashMap::from([(30_000_142, 1_700_007_000)]),
    };

    let expected_node_graph = NodeGraphAndProgressionStore {
        air_npe_runners: HashMap::from([(
            90_000_001,
            AirNpeRunnerState {
                char_id: 90_000_001,
                state_id: 1,
                completed: true,
                ..Default::default()
            },
        )]),
        client_node_graphs: HashMap::from([(
            90_000_001,
            BTreeMap::from([(
                1,
                ActiveClientNodeGraph {
                    instance_id: 1,
                    node_graph_id: 99,
                    ..Default::default()
                },
            )]),
        )]),
        client_node_graph_seq: 17,
        node_graph_blackboards: HashMap::from([(
            (90_000_001, 1),
            BTreeMap::from([("step".into(), "done".into())]),
        )]),
        expert_systems: HashMap::from([(
            90_000_001,
            BTreeMap::from([(
                57_695,
                ExpertSystemRecord {
                    type_id: 57_695,
                    expires_at_ft: 133_500_000_000,
                    ..Default::default()
                },
            )]),
        )]),
        certificates: HashMap::from([(
            90_000_001,
            CharacterCertificateState {
                certificates: HashMap::from([(12, 3)]),
                flags: HashMap::from([(12, 1)]),
            },
        )]),
    };

    let sample_season = SeasonConfig {
        season_id: 7,
        template_id: 3,
        max_active_challenges: 3,
        max_season_points: 1_000,
        season_name: "Winter Nexus".into(),
        title_message_id: 100,
        description_message_id: 101,
        hint_message_id: 102,
        large_selection_title_id: 103,
        navigation_card_picture_path: "res:/ui/winter.png".into(),
        background_no_logo: "res:/ui/bg.png".into(),
        start_time_ft: 133_000_000_000,
        end_time_ft: 133_100_000_000,
        multiple_season_running: false,
    };

    let expected_seasons = SeasonsAndCampaignsStore {
        season_states: HashMap::from([(
            90_000_001,
            CharacterSeasonState {
                selected_season_id: 7,
                season_points: 250,
                challenges: HashMap::from([(
                    1,
                    CharacterChallenge {
                        challenge_id: 1,
                        season_id: 7,
                        ..Default::default()
                    },
                )]),
                claimed_challenges: HashMap::new(),
                completed_once_per_season_ids: HashSet::from([1]),
            },
        )]),
        active_seasons: HashMap::from([(7, 1)]),
        season_configuration: Some(sample_season.clone()),
        seasons_catalog: BTreeMap::from([(7, sample_season)]),
        challenge_definitions: vec![SeasonalChallengeDef {
            challenge_id: 1,
            season_id: 7,
            title: "Scan Sites".into(),
            ..Default::default()
        }],
        season_loot_tables: HashMap::from([(1, vec![(34, 100, 0.5)])]),
        announced_season_init: true,
        announced_season_states: BTreeMap::from([(7, "active".into())]),
        hacking_wins: HashMap::from([(90_000_001, 4)]),
        milestone_playtime: HashMap::from([(
            90_000_001,
            CharacterMilestoneState {
                first_logon_unix: 1_700_000_000,
                last_activity_unix: 1_700_000_500,
                accumulated_playtime_secs: 500,
                active_session: true,
            },
        )]),
        login_campaign_enrollments: HashMap::from([(90_000_001, 5)]),
        daily_login_campaign_schedule: Some(ScheduledDailyCampaign {
            campaign_id: 5,
            start_unix: 1_700_000_000,
            ..Default::default()
        }),
        seasonal_login_schedule: Some(SeasonalCampaignSchedule {
            campaign_id: 6,
            start_unix: 1_700_000_000,
            end_unix: 1_700_500_000,
            turnover_secs: 86_400,
        }),
        auto_injected_rewards: HashMap::from([(90_000_001, vec![34, 35])]),
        login_reward_expiry: HashMap::from([(90_000_001, 1_700_086_400)]),
        project_discovery_store: ProjectDiscoveryStore {
            tasks: HashMap::from([(
                1,
                ProjectDiscoveryTaskData {
                    task_id: "pd-1".into(),
                    task_number: 1,
                    ..Default::default()
                },
            )]),
            mmos_configured: true,
            ..Default::default()
        },
        project_discovery_players: HashMap::from([(
            90_000_001,
            PlayerProjectState {
                rank: 5,
                experience: 120.5,
                accuracy: 0.92,
                score: 88.0,
                tasks_submitted: 10,
                analysis_kredits: 500,
                tutorial_level: 1,
                tutorial_complete: true,
                current_task_index: 2,
                bonus_samples_left: 5,
                tutorial_reward_claimed: true,
                tutorial_skipped: false,
                max_bonus_samples: 10,
                project_id: "exoplanets".into(),
                last_submitted_task_index: 1,
                last_bonus_refill_unix: 1_700_000_000,
                last_submission_unix: 1_700_000_100,
            },
        )]),
        project_discovery_votes: HashMap::from([(
            1,
            TaskVoteBucket {
                submissions: vec![(90_000_001, [0.5, 0.5], true)],
                consensus_centroid: Some([0.5, 0.5]),
            },
        )]),
        character_activities: HashMap::from([(
            90_000_001,
            CharacterActivityState {
                seen_activities: vec![1, 2],
                pop_state: vec![(PyRep::Integer(1), PyRep::Bool(true))],
            },
        )]),
        fast_checkout_states: HashMap::from([(
            1,
            UserFastCheckoutState {
                enabled: true,
                cached_session_token: Some("tok-1".into()),
                cached_offers: None,
            },
        )]),
    };

    let expected_quasar = QuasarCommerceStore {
        payment_tokens: vec![PaymentTokenRecord {
            id: 1,
            user_id: 1,
            token_str: "ptok".into(),
            ..Default::default()
        }],
        payment_receipts: vec![PaymentReceiptRecord {
            receipt_id: 2,
            user_id: 1,
            offer_id: "offer-1".into(),
            ..Default::default()
        }],
        payment_schedules: vec![PaymentScheduleRecord {
            id: 3,
            user_id: 1,
            offer_id: "sub-1".into(),
            ..Default::default()
        }],
        plex_tx_logs: vec![PlexTransactionRecord {
            id: 4,
            user_id: 1,
            amount: 500,
            balance_after: 500,
            ..Default::default()
        }],
        nes_store_catalog: Some(StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "Skins".into(),
                ..Default::default()
            }],
            products: vec![StoreProduct {
                id: 2,
                name: "Skin Pack".into(),
                ..Default::default()
            }],
            offers: vec![StoreOffer {
                id: 3,
                name: "Offer".into(),
                price_plex: 110,
                ..Default::default()
            }],
        }),
        nes_store_offer_tags: HashMap::from([(3, vec!["featured".into()])]),
        plex_invoices: vec!["inv-1".into()],
        cosmetics_market: Some(CosmeticsMarketData {
            component_listings: BTreeMap::from([(
                "comp-1".into(),
                ComponentListingRecord {
                    uuid: [1; 16],
                    sale_end_unix: 1_700_050_000,
                    ..Default::default()
                },
            )]),
            skin_listings: BTreeMap::from([(
                "skin-1".into(),
                SkinListingRecord {
                    uuid: [2; 16],
                    target: ListingTargetRecord::Corporation(98_000_001),
                    ..Default::default()
                },
            )]),
            transactions: BTreeMap::from([(
                "tx-1".into(),
                CosmeticTransactionRecord {
                    uuid: [3; 16],
                    status: 1,
                    ..Default::default()
                },
            )]),
            next_seq: 12,
        }),
        orchestrator: OrchestratorState {
            default_tenant_version: SemVerSpec {
                major: 1,
                minor: 2,
                patch: 3,
            },
            tenant_version: Some(SemVerSpec {
                major: 1,
                minor: 3,
                patch: 0,
            }),
            deployments: BTreeMap::from([(
                1,
                DeploymentRecord {
                    id: 1,
                    name: "dep-1".into(),
                    ..Default::default()
                },
            )]),
            content_groups: BTreeMap::from([(
                2,
                ContentGroupRecord {
                    id: 2,
                    name: "cg-1".into(),
                    ..Default::default()
                },
            )]),
            content_types: BTreeMap::from([(
                3,
                ContentTypeRecord {
                    id: 3,
                    name: "ct-1".into(),
                    ..Default::default()
                },
            )]),
            instances: vec![DeploymentInstanceRecord {
                uuid: vec![9; 16],
                deployment_id: 1,
                ..Default::default()
            }],
        },
        failed_fulfillments: vec![FailedFulfillmentRecord {
            id: 1,
            user_id: 1,
            plex_units: 100,
            ..Default::default()
        }],
        targeted_offers: BTreeMap::from([(
            "target-1".into(),
            TargetedOfferAdminRecord {
                user_id: 1,
                catalogue_entry_name: "entry-1".into(),
                promoted: true,
                ..Default::default()
            },
        )]),
        acl_registry: Some(vec![QuasarAclRecord {
            acl_id: 1,
            name: "everyone".into(),
            everyone: true,
            ..Default::default()
        }]),
        lp_asset_ops_completed: HashSet::from(["op-1".into()]),
        store_purchase_idempotency: BTreeMap::from([(
            "1:plex_vault:idem-1".into(),
            CommittedStorePurchaseRecord {
                buyer_user_id: 1,
                channel: "plex_vault".into(),
                idempotency_key: "idem-1".into(),
                offer_id: 3,
                quantity: 1,
                gift_char_id: None,
                active_char_id: Some(90_000_001),
                payment_token_id: None,
                tx_id: 1001,
                plex_spent: 110,
                plex_credited: 0,
                is_free_mode_settlement: false,
                delivered_item_id: 2001,
                delivered_items_count: 1,
                committed_unix: 1_700_000_000,
                commit_seq: 1001,
            },
        )]),
    };

    let expected_fleet_social = FleetAndSocialOpsStore {
        fleet_saved_setups: HashMap::from([(
            90_000_001,
            HashMap::from([(
                "Default".into(),
                FleetSetupRecord {
                    setup_name: "Default".into(),
                    max_size: 256,
                    ..Default::default()
                },
            )]),
        )]),
        fleet_stored_formations: HashMap::from([(1001, (1, 10_000.0, 2_500.0))]),
        fleet_adverts: HashMap::from([(
            1001,
            FleetAdvertRecord {
                fleet_id: 1001,
                fleet_name: "Public Roam".into(),
                ..Default::default()
            },
        )]),
        fleet_join_requests: HashMap::from([(1001, HashSet::from([90_000_002]))]),
        fleet_watchlist: HashMap::from([(90_000_001, HashSet::from([90_000_002]))]),
        fleet_voice_enabled: HashMap::from([(1001, true)]),
        fleet_loot_logging: HashMap::from([(1001, true)]),
        corp_recruitment_ads: vec![CorpRecruitmentAd {
            ad_id: 500,
            corp_id: 98_000_001,
            title: "Recruiting".into(),
            ..Default::default()
        }],
        corp_allow_war: HashMap::from([(98_000_001, true)]),
        hypernet_subs: BTreeMap::from([("node-1".into(), vec![90_000_001])]),
        ingamereport_tickets: vec![InGameReportRecord {
            report_id: 801,
            reporter_char_id: 90_000_001,
            reason: "bot".into(),
            ..Default::default()
        }],
        contract_deposits: HashMap::from([(6001, 50_000)]),
        lp_store_offers: Some(vec![LPOffer {
            offer_id: 9001,
            corp_id: 1_000_035,
            type_id: 587,
            isk_cost: IskCents(100_000),
            ..Default::default()
        }]),
        delayed_rewards: HashMap::from([(
            90_000_001,
            HashMap::from([("reward-1".into(), vec![json!({"isk": 1000})])]),
        )]),
        mission_taxed_journal: HashSet::from([44_001]),
        agent_global_disabled_missions: HashSet::from([77]),
        agent_char_disabled_missions: HashMap::from([(90_000_001, HashSet::from([88]))]),
        agent_replay_ts: HashMap::from([((90_000_001, 3_018_601), 1_700_000_123)]),
        signal_tracker_subs: HashMap::from([(30_000_142, vec![90_000_001])]),
        pending_probe_scans: vec![(
            90_000_001,
            vec![100_000_001],
            vec![PyRep::Integer(1)],
            vec![PyRep::String("sig".into())],
            vec![PyRep::Bool(true)],
            1_700_000_200,
        )],
        wh_local_witnesses: HashMap::from([("J100001".into(), HashSet::from([90_000_001]))]),
    };

    let expected_cluster = ClusterAndRuntimeStore {
        cluster_misc_state: Some(PersistedClusterMiscState {
            persisted_shipcaster_state: true,
            char_settings: HashMap::from([(
                90_000_001,
                BTreeMap::from([("ui".into(), PyRep::Integer(1))]),
            )]),
            landing_pads: Vec::new(),
            launchers: vec![PersistedLauncherRecord {
                item_id: 7001,
                type_id: 77_266,
                solar_system_id: 30_000_142,
                faction_id: 500_001,
                linked_pad_ids: vec![7002],
                position: [1.0, 2.0, 3.0],
            }],
            starter_launchers: Vec::new(),
            used_starter_shipcaster_chars: BTreeSet::from([90_000_001]),
            offered_load_slots: HashMap::from([(
                7001,
                OfferedLoadSlot {
                    slot_key: "slot-1".into(),
                    original_system_id: 30_000_142,
                    offered_system_ids: vec![30_000_143],
                    expires_unix: 1_700_005_000,
                },
            )]),
            char_energy: HashMap::from([(
                90_000_001,
                CharacterEnergyState {
                    current_energy: 50,
                    max_energy: 100,
                    last_update_time: 1_700_000_000,
                    recharge_rate: 1.5,
                    energy_rem: 0.25,
                },
            )]),
            target_cooldowns: HashMap::from([(7002, 1_700_002_000)]),
            target_charges: HashMap::from([(7002, 3)]),
            zarzakh_toll_quotes: HashMap::from([(
                100_000_001,
                PendingZarzakhTollQuote {
                    from_gate_id: 50_000_001,
                    to_gate_id: 50_000_002,
                    ship_id: 100_000_001,
                    quoted_toll_cents: IskCents(1_000_000),
                    expires_unix: 1_700_000_300,
                },
            )]),
            zarzakh_horizon_siege: ZarzakhHorizonSiegeState {
                phase: HorizonSiegePhase::Active,
                phase_started_unix: 1_700_000_000,
                active_start_unix: 1_700_000_100,
                active_until_unix: 1_700_003_700,
                cooldown_until_unix: 1_700_007_300,
                instability_points: 25,
                triggered_by_campaign_id: Some(901),
                fulcrum_restricted_to_pirates: true,
            },
        }),
        clone_vat_bay_state: CloneVatBayState {
            active_by_ship: HashMap::from([(100_000_001, (30_000_142, 1_700_001_000))]),
        },
        structure_defense: HashMap::from([(
            600_000_001,
            StructureDefenseSettings {
                structure_id: 600_000_001,
                auto_defense_enabled: true,
                auto_target_attackers: true,
                auto_target_war_targets: false,
                standing_threshold: -5.0,
                target_blocked_acl: true,
                known_attackers: vec![90_000_002],
                known_attacker_timestamps: HashMap::from([(90_000_002, 1_700_000_500)]),
            },
        )]),
        alliance_sov_cfg: HashMap::from([(
            99_000_001,
            AllianceSovConfig {
                capital_system_id: Some(30_000_142),
                pending_capital_system_id: Some(30_000_143),
                capital_valid_after_unix: 1_700_100_000,
                current_prime_hour: 18,
                new_prime_hour: 20,
                prime_valid_after_unix: 1_700_200_000,
            },
        )]),
        load_service_state: LoadServiceState {
            active: true,
            config: BTreeMap::from([("mode".into(), PyRep::String("soak".into()))]),
            runs_started: 3,
            total_requests: 120,
            systems_stepped: 10,
            ticks_executed: 500,
            sde_types_sampled: 20,
            sde_solar_systems_sampled: 5,
            world_items_sampled: 40,
            world_characters_sampled: 8,
            active_sessions_sampled: 2,
            last_started_unix: 1_700_000_000,
            last_stopped_unix: 1_700_000_500,
        },
        active_sessions: vec![1, 2, 3],
        session_registry: HashMap::from([("sess-1".into(), json!({"char_id": 90_000_001}))]),
        cluster_node_table: Some(BTreeMap::from([("node-1".into(), "127.0.0.1:26000".into())])),
        cluster_admin_broadcasts: vec![json!({"msg": "reboot"})],
        cluster_service_states: BTreeMap::from([("market".into(), true)]),
        cluster_attacks: vec![json!({"kind": "probe"})],
        external_queue_events: vec![json!({"topic": "login"})],
        cluster_downtime_state: PersistedDowntimeState {
            emitted_warnings_by_day: BTreeMap::from([(19_850, vec![300, 60])]),
            last_shutdown_epoch_day: Some(19_849),
            cluster_restart_epoch: 4,
            last_maintenance_unix: 1_700_000_000,
            downtime_until_unix: Some(1_700_000_900),
        },
        macho_client_code_hash: Some(json!({"v": "24.01"})),
        macho_global_config: HashMap::from([("max_clients".into(), PyRep::Integer(500))]),
        user_sessions: vec![json!({"user_id": 1})],
        gm_audit_log: vec![json!({"gm": 90_000_001, "cmd": "/heal"})],
        alert_bean_counts: HashMap::from([("warn".into(), 3)]),
        alert_log_modes: HashMap::from([("client".into(), "full".into())]),
        alert_client_traces: Some(json!(["trace-a"])),
        alert_proxy_traces: Some(json!(["proxy-a"])),
        alert_group_bean_bytes: Some(json!({"bytes": 256})),
        browser_lockdown_policy: Some(BrowserLockdownPolicyState {
            lockdown: true,
            default_home_page: "https://eve.invalid".into(),
            flagged_sites: vec![FlaggedSiteEntry {
                site_url: "https://bad.invalid".into(),
                site_flag: 2,
            }],
        }),
        localization_qa_wrap_users: BTreeMap::from([(90_000_001, true)]),
        client_stat_logger_strings: vec!["fps=60".into()],
        blacklisted_planets: Some(vec![40_009_082]),
        portraits: HashMap::from([("90000001".into(), "portrait.png".into())]),
        paperdoll_states: HashMap::from([(90_000_001, "sculpted".into())]),
        char_system_visits: HashMap::from([(
            90_000_001,
            HashMap::from([(
                30_000_142,
                SolarSystemVisitRecord {
                    visits: 4,
                    last_visited_ticks: 1_700_000_000,
                },
            )]),
        )]),
        module_reload_until_unix: HashMap::from([(200_000_001, 1_700_000_010)]),
        auto_reloaded_event: HashMap::from([(200_000_001, (vec![200_000_002], 218, 12.5))]),
        warp_vector_cooldown_until: HashMap::from([(100_000_001, 1_700_000_020)]),
        ship_clone_offers: HashMap::from([(100_000_001, (90_000_002, 1_700_000_030))]),
        mct_slots: HashMap::from([(1, vec![90_000_002])]),
        account_resculpt_tokens: HashMap::from([("acct-1".into(), 2)]),
        resculpt_pending: HashSet::from([90_000_001]),
        char_cohorts: HashMap::from([(90_000_001, vec!["alpha".into()])]),
        user_cohorts: HashMap::from([(1, vec!["beta".into()])]),
        character_locks: HashMap::from([(90_000_001, "transfer".into())]),
        omega_downgraded: HashSet::from([1]),
        server_runtime_json: Some("{\"cluster_ok\":true}".into()),
    };

    repo.tx("seed_all_17_service_domains", |wtx| -> Result<(), TxError> {
        wtx.snap.pve_special_sites = expected_pve.clone().into();
        wtx.snap.crab_beacons = expected_crab.clone().into();
        wtx.snap.abyssal_state = expected_abyssal.clone().into();
        wtx.snap.dungeon_runtime = expected_dungeon_rt.clone().into();
        wtx.snap.dungeon_editor = expected_dungeon_ed.clone().into();
        wtx.snap.fob_observatory_state = expected_fob.clone().into();
        wtx.snap.mining_and_ess_state = expected_mining_ess.clone().into();
        wtx.snap.pvp_filament_state = expected_pvp.clone().into();
        wtx.snap.tourney_state = expected_tourney.clone().into();
        wtx.snap.deployable_state = expected_deployable.clone().into();
        wtx.snap.superweapon_runtime = expected_superweapon.clone().into();
        wtx.snap.insurgency_complex_ops = expected_insurgency_ops.clone().into();
        wtx.snap.node_graph_state = expected_node_graph.clone().into();
        wtx.snap.seasons_and_campaigns = expected_seasons.clone().into();
        wtx.snap.quasar_commerce = expected_quasar.clone().into();
        wtx.snap.fleet_and_social_ops = expected_fleet_social.clone().into();
        wtx.snap.cluster_and_runtime = expected_cluster.clone().into();
        Ok(())
    })
    .expect("commit all 17 service domains");
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&path).expect("reopen sqlite");
    reopened.with_snapshot(|snap| {
        assert_eq!(&*snap.pve_special_sites, &expected_pve);
        assert_eq!(&*snap.crab_beacons, &expected_crab);
        assert_eq!(&*snap.abyssal_state, &expected_abyssal);
        assert_eq!(&*snap.dungeon_runtime, &expected_dungeon_rt);
        assert_eq!(&*snap.dungeon_editor, &expected_dungeon_ed);
        assert_eq!(&*snap.fob_observatory_state, &expected_fob);
        assert_eq!(&*snap.mining_and_ess_state, &expected_mining_ess);
        assert_eq!(&*snap.pvp_filament_state, &expected_pvp);
        assert_eq!(&*snap.tourney_state, &expected_tourney);
        assert_eq!(&*snap.deployable_state, &expected_deployable);
        assert_eq!(&*snap.superweapon_runtime, &expected_superweapon);
        assert_eq!(&*snap.insurgency_complex_ops, &expected_insurgency_ops);
        assert_eq!(&*snap.node_graph_state, &expected_node_graph);
        assert_eq!(&*snap.seasons_and_campaigns, &expected_seasons);
        assert_eq!(&*snap.quasar_commerce, &expected_quasar);
        assert_eq!(&*snap.fleet_and_social_ops, &expected_fleet_social);
        assert_eq!(&*snap.cluster_and_runtime, &expected_cluster);
    });

    // Reset scalar fields across service domains to Default and confirm deletes survive restart.
    reopened
        .tx("reset_service_domain_scalars", |wtx| -> Result<(), TxError> {
            wtx.snap.pve_special_sites.initialized = false;
            wtx.snap.pve_special_sites.sim_clock_sec = 0;
            wtx.snap.tourney_state.active_match_id = None;
            wtx.snap.seasons_and_campaigns.season_configuration = None;
            wtx.snap.cluster_and_runtime.server_runtime_json = None;
            wtx.snap.cluster_and_runtime.browser_lockdown_policy = None;
            Ok(())
        })
        .expect("reset service scalars");
    drop(reopened);

    let reopened_reset = WorldRepository::open_sqlite(&path).expect("reopen after scalar reset");
    reopened_reset.with_snapshot(|snap| {
        assert!(!snap.pve_special_sites.initialized);
        assert_eq!(snap.pve_special_sites.sim_clock_sec, 0);
        assert_eq!(snap.tourney_state.active_match_id, None);
        assert_eq!(snap.seasons_and_campaigns.season_configuration, None);
        assert_eq!(snap.cluster_and_runtime.server_runtime_json, None);
        assert_eq!(snap.cluster_and_runtime.browser_lockdown_policy, None);
        assert_eq!(snap.pve_special_sites.next_seq, expected_pve.next_seq);
        assert_eq!(snap.cluster_and_runtime.active_sessions, expected_cluster.active_sessions);
    });

    drop(reopened_reset);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn homefront_legacy_row_without_sim_fields_seeds_elapsed_sim_secs_on_sqlite_load() {
    use crate::persistence::WorldRepository;
    use crate::service_domains::{HomefrontBlueprintRecord, HomefrontProgressRecord, HomefrontRuntimeRecord};

    let path = temp_db_path("homefront_legacy_seed");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    let mut snap = WorldStateSnapshot::default();
    let legacy_rec = HomefrontRuntimeRecord {
        blueprint: HomefrontBlueprintRecord {
            time_limit_secs: 900,
            ..Default::default()
        },
        solar_system_id: 30_000_142,
        instance_id: 77_001,
        started: true,
        pocket_spawned: true,
        started_at_sec: 1_800_000_000,
        expires_at_sec: 1_800_000_900,
        last_tick_sec: 1_800_000_245,
        progress: HomefrontProgressRecord::default(),
        ..Default::default()
    };
    let post_migration_zero_rec = HomefrontRuntimeRecord {
        sim_clock_initialized: true,
        elapsed_sim_secs: 0.0,
        last_sim_elapsed_secs: Some(42.0),
        instance_id: 77_002,
        ..legacy_rec.clone()
    };
    snap.dungeon_runtime.homefront_runtimes.insert(77_001, legacy_rec);
    snap.dungeon_runtime
        .homefront_runtimes
        .insert(77_002, post_migration_zero_rec);
    replace_sqlite_from_snapshot(&mut conn, &snap).expect("persist");

    // Strip the new sim-clock fields from row 77001 to simulate a pre-migration row; leave 77002 intact.
    let raw_json: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = 'service_domains.dungeon_runtime' AND key = 'homefront_runtimes/77001'",
            [],
            |row| row.get(0),
        )
        .expect("fetch raw homefront_runtimes row");
    let mut val: serde_json::Value = serde_json::from_str(&raw_json).expect("parse json");
    if let Some(obj) = val.as_object_mut() {
        obj.remove("elapsed_sim_secs");
        obj.remove("last_sim_elapsed_secs");
        obj.remove("sim_clock_initialized");
    }
    conn.execute(
        "UPDATE domain_rows SET value = ?1 WHERE domain = 'service_domains.dungeon_runtime' AND key = 'homefront_runtimes/77001'",
        rusqlite::params![val.to_string()],
    )
    .expect("write pre-migration json row");
    drop(conn);

    let reopened = WorldRepository::open_sqlite(&path).expect("reopen sqlite");
    reopened.with_snapshot(|loaded| {
        let r = &loaded.dungeon_runtime.homefront_runtimes[&77_001];
        assert_eq!(r.elapsed_sim_secs, 245.0);
        assert_eq!(r.last_sim_elapsed_secs, None);
        assert!(r.sim_clock_initialized);

        let r2 = &loaded.dungeon_runtime.homefront_runtimes[&77_002];
        assert_eq!(r2.elapsed_sim_secs, 0.0);
        assert_eq!(r2.last_sim_elapsed_secs, None);
        assert!(r2.sim_clock_initialized);
    });
    drop(reopened);
    let _ = std::fs::remove_file(&path);
}
