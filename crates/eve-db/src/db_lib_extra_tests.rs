//! Additional unit and mutation-killing tests for `eve-db` (`lib.rs` and core helpers).

#![forbid(unsafe_code)]

use super::*;
use std::collections::{HashMap, HashSet};

#[test]
fn test_sp6_persisted_state_and_entries_serde_defaults_and_roundtrip() {
    // 1. Empty JSON object must deserialize to Sp6PersistedState::default()
    let empty: Sp6PersistedState = serde_json::from_str("{}").expect("empty JSON Sp6PersistedState");
    assert_eq!(empty, Sp6PersistedState::default());

    // 2. DisconnectedShipEntry without pre_warp_position defaults to None
    let disc_minimal: DisconnectedShipEntry = serde_json::from_str(
        r#"{
            "char_id": 90000001,
            "ship_id": 100000001,
            "ship_type_id": 587,
            "solar_system_id": 30000142,
            "disconnected_at_unix": 1700000000,
            "remove_at_unix": 1700000060,
            "emergency_warp_dest": [1.0, 2.0, 3.0],
            "last_position": [4.0, 5.0, 6.0],
            "removed_from_space": false
        }"#,
    )
    .expect("deserialize DisconnectedShipEntry without pre_warp_position");
    assert_eq!(disc_minimal.pre_warp_position, None);

    // 3. Full Sp6PersistedState roundtrip with all 14 fields populated
    let mut full = Sp6PersistedState {
        last_downtime_day_idx: Some(19_850),
        missions_awaiting_downtime_respawn: vec![(90_000_001, 1001)],
        ..Default::default()
    };
    full.safe_logoffs.insert(
        90_000_001,
        SafeLogoffEntry {
            char_id: 90_000_001,
            ship_id: 100_000_001,
            solar_system_id: 30_000_142,
            started_unix: 1_700_000_000,
            complete_at_unix: 1_700_000_030,
            completed: false,
        },
    );
    full.disconnected_ships.insert(
        90_000_002,
        DisconnectedShipEntry {
            pre_warp_position: Some([10.0, 20.0, 30.0]),
            ..disc_minimal
        },
    );
    full.self_destructs.insert(
        100_000_003,
        SelfDestructPersistedEntry {
            ship_id: 100_000_003,
            char_id: 90_000_003,
            solar_system_id: 30_000_142,
            started_unix: 1_700_000_000,
            detonate_at_unix: 1_700_000_120,
        },
    );
    full.abyssal_sessions.insert(
        90_000_004,
        AbyssalPersistedSession {
            char_id: 90_000_004,
            ship_id: 100_000_004,
            pocket_system_id: 32_000_001,
            origin_system_id: 30_000_142,
            entered_unix: 1_700_000_000,
            collapse_at_unix: 1_700_001_200,
        },
    );
    full.pvp_timers_until.insert(90_000_001, 1_700_000_900);
    full.npc_timers_until.insert(90_000_001, 1_700_000_300);
    full.weapon_timers_until.insert(90_000_001, 1_700_000_060);
    full.criminal_timers_until.insert(90_000_001, 1_700_000_900);
    full.ship_combat_hp.insert(
        100_000_001,
        PersistedCombatHp {
            shield_hp: 250.0,
            armor_hp: 300.0,
            hull_hp: 350.0,
        },
    );
    full.space_object_meta.insert(
        800_000_001,
        PersistedSpaceObjectMeta {
            created_unix: Some(1_700_000_000),
            is_npc_wreck: true,
            anchor_start_unix: Some(1_700_000_010),
            anchoring_delay_secs: 60.0,
            anchor_completed: true,
            bubble_radius_m: Some(20_000.0),
            drone_ball_id: Some(800_000_002),
            planet_id: Some(40_009_082),
        },
    );
    full.mission_initial_npc_counts
        .insert("90000001:1001".to_string(), 5);
    full.mission_initial_npc_types
        .insert("90000001:1001".to_string(), vec![10_001, 10_002]);
    full.active_scanner_probes.insert(
        900_000_001,
        crate::PersistedScannerProbe {
            probe_id: 900_000_001,
            probe_type_id: 30_013,
            owner_char_id: 90_000_001,
            ship_id: 100_000_001,
            launcher_module_id: 100_000_002,
            solar_system_id: 30_000_142,
            position: [10.0, 20.0, 30.0],
            destination: [40.0, 50.0, 60.0],
            range_step: 4,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3_600.0,
            launched_at_unix: 1_700_000_000,
            state: 1,
            warp_end_ft: None,
        },
    );

    let json = serde_json::to_string(&full).expect("serialize full Sp6PersistedState");
    let decoded: Sp6PersistedState =
        serde_json::from_str(&json).expect("deserialize full Sp6PersistedState");
    assert_eq!(decoded, full);
}

#[test]
fn test_tuple_map_and_sorted_set_serde_deterministic_ordering() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "d6_types::tuple_map_serde")]
        pairs: HashMap<(i64, u32), String>,
        #[serde(with = "d6_types::sorted_set_serde")]
        ids: HashSet<i64>,
    }

    let mut w = Wrapper {
        pairs: HashMap::new(),
        ids: HashSet::new(),
    };
    w.pairs.insert((20, 2), "twenty-two".into());
    w.pairs.insert((10, 5), "ten-five".into());
    w.pairs.insert((10, 1), "ten-one".into());
    w.ids.extend([300, 100, 200]);

    let json = serde_json::to_string(&w).expect("serialize Wrapper");
    // Verify sorted order in JSON output
    assert!(json.contains(r#"[[[10,1],"ten-one"],[[10,5],"ten-five"],[[20,2],"twenty-two"]]"#));
    assert!(json.contains(r#"[100,200,300]"#));

    let roundtrip: Wrapper = serde_json::from_str(&json).expect("deserialize Wrapper");
    assert_eq!(roundtrip, w);
}

#[test]
fn test_virtual_server_clock_filetime_pause_and_wall_tick_branches() {
    // 1. Fixed base clock: initial 0 clamps to 1
    let c0 = VirtualServerClock::with_fixed_base(0);
    assert_eq!(c0.now_unix_secs(), 1);
    assert_eq!(c0.now_subsec_nanos(), 0);
    assert_eq!(
        c0.now_blueos_filetime_ticks(),
        (1 + 11_644_473_600) * 10_000_000
    );

    // 2. Multiplier 0 clamps to 1; tick_wall_second delegates to step_tick(1) on fixed clock
    c0.set_acceleration_multiplier(0);
    assert_eq!(c0.acceleration_multiplier(), 1);
    c0.set_acceleration_multiplier(5);
    assert_eq!(c0.tick_wall_second(), 5);
    assert_eq!(c0.now_unix_secs(), 6);

    // 3. pause_on_tick pauses automatically after step_tick
    c0.set_pause_on_tick(true);
    assert!(c0.pause_on_tick());
    assert_eq!(c0.step_tick(2), 10);
    assert!(c0.is_paused());
    assert_eq!(c0.step_tick(2), 0);
    assert_eq!(c0.tick_wall_second(), 0);

    // 4. Wall-anchored clock: pause, unpause, tick_wall_second with multiplier > 1 and pause_on_tick
    let wall = VirtualServerClock::default();
    assert!(!wall.is_paused());
    wall.set_paused(true);
    assert_eq!(wall.now_subsec_nanos(), 0);
    let t_paused = wall.now_unix_secs();
    assert_eq!(wall.now_unix_secs(), t_paused);
    wall.set_paused(false);

    wall.set_acceleration_multiplier(4);
    wall.set_pause_on_tick(true);
    let before = wall.now_unix_secs();
    assert_eq!(wall.tick_wall_second(), 4);
    assert!(wall.is_paused());
    assert!(wall.now_unix_secs() >= before + 3);
}

#[test]
fn test_fw_backfill_preserves_custom_thresholds_and_migrates_zero() {
    let mut snap = WorldStateSnapshot::default();
    snap.fw_system_occupations.insert(
        30_000_012,
        snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: 30_000_012,
            owner_faction_id: 500_003,
            occupier_faction_id: 500_003,
            victory_points: 500,
            victory_points_threshold: 0,
        },
    );
    snap.fw_system_occupations.insert(
        30_002_055,
        snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: 30_002_055,
            owner_faction_id: 500_002,
            occupier_faction_id: 500_002,
            victory_points: 100,
            victory_points_threshold: 45_000,
        },
    );
    snap.backfill_fw_system_occupations();
    assert_eq!(
        snap.fw_system_occupations[&30_000_012].victory_points_threshold,
        75_000
    );
    assert_eq!(
        snap.fw_system_occupations[&30_002_055].victory_points_threshold,
        45_000
    );
}
