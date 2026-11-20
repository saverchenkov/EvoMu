use crate::service_domains::{
    AbyssalPocketSession, CrabBeaconPhase, CrabBeaconRecord, EntityKvKey,
    EntityKvScope, IncursionTaleRecord, MobileDeployableRecord, MobileDeployableRuntimeState, Ns,
    TourneyMatch, TourneyStore, DEFAULT_ENTITY_KV_MAX_VALUE_BYTES, MAX_SERVICE_STATE_BLOB_KEYS,
    MAX_SERVICE_STATE_BLOB_TOTAL_BYTES, MAX_SERVICE_STATE_BLOB_VALUE_BYTES,
};
use crate::sqlite_store::sync::sync_snapshot_diff_for_test;
use crate::sqlite_store::{
    apply_migrations, load_snapshot_from_sqlite, replace_sqlite_from_snapshot,
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
    assert_eq!(Ns::ALL.len(), 21);
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

    let fail_closed = crate::with_quarantine_unreadable_for_test(false, || {
        load_snapshot_from_sqlite(&conn)
    });
    assert!(fail_closed.is_err());

    let quarantined_snap = crate::with_quarantine_unreadable_for_test(true, || {
        load_snapshot_from_sqlite(&conn)
    })
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
