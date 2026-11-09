use crate::service_domains::{
    AbyssalPocketSession, CrabBeaconPhase, CrabBeaconRecord, IncursionTaleRecord,
    MobileDeployableRecord, MobileDeployableRuntimeState, TourneyMatch, TourneyStore,
    MAX_SERVICE_STATE_BLOB_KEYS, MAX_SERVICE_STATE_BLOB_TOTAL_BYTES,
    MAX_SERVICE_STATE_BLOB_VALUE_BYTES,
};
use crate::sqlite_store::{
    apply_migrations, load_snapshot_from_sqlite, replace_sqlite_from_snapshot,
    rollback_dirty_from_sqlite, sync_dirty_to_sqlite,
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
    snap.quasar_commerce.plex_invoices.push("inv-1".into());

    let mut fp = replace_sqlite_from_snapshot(&mut conn, &snap).expect("replace");
    let (loaded, loaded_fp) = load_snapshot_from_sqlite(&conn).expect("load");
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
    assert_eq!(loaded.quasar_commerce.plex_invoices, vec!["inv-1".to_string()]);

    // Rollback restores modified domain without touching unrelated domains.
    let mut dirty = loaded.clone();
    dirty.crab_beacons.beacons.clear();
    dirty.pve_special_sites.incursions.clear();
    rollback_dirty_from_sqlite(&conn, &mut dirty, &loaded_fp).expect("rollback");
    assert_eq!(dirty.crab_beacons, snap.crab_beacons);
    assert_eq!(dirty.pve_special_sites, snap.pve_special_sites);

    // Reverting to default deletes domain row cleanly.
    dirty.quasar_commerce.plex_invoices.clear();
    let tx = conn.transaction().expect("tx");
    let delta = sync_dirty_to_sqlite(&tx, &dirty, &mut fp, true).expect("sync");
    assert!(delta.full_snapshot.is_some());
    tx.commit().expect("commit");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM service_domain_state WHERE domain_key = 'quasar_commerce'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 0);

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
    let _fp_old = replace_sqlite_from_snapshot(&mut conn, &legacy).expect("replace legacy");

    let (snap, mut fp) = load_snapshot_from_sqlite(&conn).expect("load migrated");
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

    // `load_snapshot_from_sqlite` already flushed migrated typed tables and stripped legacy rows
    // before computing baseline `fp`, so an immediate unchanged sync is a clean 0-write no-op.
    let tx = conn.transaction().expect("tx");
    let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).expect("sync migrated");
    assert!(!delta.dirty);
    assert!(delta.full_snapshot.is_none());
    tx.commit().expect("commit");

    // Aborting a subsequent mutation and rolling back against `fp` must preserve migrated domains.
    let mut aborted = snap.clone();
    aborted.crab_beacons.beacons.clear();
    aborted.tourney_state.matches.clear();
    aborted.deployable_state.deployables.clear();
    super::rollback::rollback_dirty_from_sqlite(&conn, &mut aborted, &fp)
        .expect("rollback after startup migration");
    assert!(aborted.crab_beacons.beacons.contains_key(&555));
    assert!(aborted.tourney_state.matches.contains_key(&1));
    assert!(aborted.deployable_state.deployables.contains_key(&5001));

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).expect("reload");
    assert!(reloaded.crab_beacons.beacons.contains_key(&555));
    assert!(reloaded.tourney_state.matches.contains_key(&1));
    assert!(reloaded.deployable_state.deployables.contains_key(&5001));

    let blob_json: String = conn
        .query_row(
            "SELECT blob_json FROM world_kv_blobs WHERE blob_key = 'service_state_blobs'",
            [],
            |r| r.get(0),
        )
        .expect("blobs row");
    assert!(!blob_json.contains("incursions_state"));
    assert!(blob_json.contains("__server_now_unix"));

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

    // Direct push bypassing set_service_state_blob is also rejected by sqlite sync.
    let path = temp_db_path("blob_limit");
    let mut conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &WorldStateSnapshot::default())
        .expect("seed");
    let mut bad = WorldStateSnapshot::default();
    bad.service_state_blobs
        .insert("bad_key".into(), "z".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1));
    let tx = conn.transaction().expect("tx");
    assert!(sync_dirty_to_sqlite(&tx, &bad, &mut fp, false).is_err());
    drop(tx);
    let _ = std::fs::remove_file(&path);
}
