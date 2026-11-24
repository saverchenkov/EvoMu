//! Unit tests for Step 1 outside-RPC (`"tick"` class) item projection switch,
//! shadow dry-run telemetry, single-fact deduplication, exclusions, and offline clearing (`#931`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;

use crate::services::notify_emitters_character_services::{
    save_industry_material_monitors, IndustryMaterialMonitorSub,
};

use super::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
};
use super::delta_item_compare::{
    flush_item_compare_summary, set_item_compare_override_for_test, with_compare_tick_scope,
};
use super::delta_item_switch::{
    projected_item_switch_summary, ProjectedItemSwitchConfig, ProjectedItemSwitchSummary,
    SwitchScopeOutcomeCounts,
};
use super::delta_item_switch_tests::{setup_online_pilot_in_station, test_ctx};

#[tokio::test]
async fn step1_outside_rpc_test_override_off_records_shadow_by_scope_without_altering_delivery() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_950_010_i64);
    let tick_probe_id = 993_950_021_i64;
    let neither_item_id = 993_950_022_i64;
    let no_proj_item_id = 993_950_023_i64;
    let dispatch_item_id = 993_950_024_i64;

    let neither_hangar_before = InventoryItemRecord {
        item_id: neither_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    // Seed pre-existing hangar stack while pilot is offline so setup itself does not count as
    // a `would_announce` commit in shadow `by_scope.neither`.
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(neither_item_id, neither_hangar_before.clone());
    });

    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::default());
    assert!(ctx.projected_item_switches().char_same_scene_item_outside_rpc);
    assert!(ctx.projected_item_switches().char_same_scene_item_fitted);
    assert!(
        ProjectedItemSwitchConfig::from_env_lookup(|k| {
            (k == "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC").then(|| "0".to_string())
        })
        .char_same_scene_item_outside_rpc
    );
    assert!(
        ProjectedItemSwitchConfig::from_env_lookup(|k| {
            (k == "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED").then(|| "0".to_string())
        })
        .char_same_scene_item_fitted
    );
    assert_eq!(
        ProjectedItemSwitchConfig::retired_off_env_vars_lookup(|k| match k {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC"
            | "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some("0".to_string()),
            _ => None,
        }),
        vec![
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED",
        ]
    );
    assert!(ctx.projected_item_switches().char_all_items);
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_outside_rpc(false)
            .with_char_all_items(false),
    );
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));

    let tick_probe_after = InventoryItemRecord {
        item_id: tick_probe_id,
        type_id: 30_013,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 8,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    let mut neither_hangar_after = neither_hangar_before.clone();
    neither_hangar_after.quantity = 250;
    neither_hangar_after.version = 2;

    let no_proj_hangar = InventoryItemRecord {
        item_id: no_proj_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 42,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    let dispatch_hangar = InventoryItemRecord {
        item_id: dispatch_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 77,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    // 1. Inside-dispatch baseline works as before even when outside-RPC is OFF.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("step1_dispatch_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(dispatch_item_id, dispatch_hangar.clone());
                Ok(())
            })
            .expect("dispatch insert");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1
    );

    // 2. Tick-scoped commit (probe launched -> active ship cargo) emits 0 live projection
    //    notifications while Step 1 is off, but populates shadow `by_scope.tick`.
    with_compare_tick_scope(async {
        ctx.db
            .tx("step1_tick_probe_return", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(tick_probe_id, tick_probe_after.clone());
                Ok(())
            })
            .expect("tick probe commit");
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "Step 1 off by default must emit 0 OnItemChange from tick commit"
    );

    // 3. Markerless spawned-task/neither commit populates shadow `by_scope.neither` identically.
    let db_clone = ctx.db.clone();
    let neither_after_spawn = neither_hangar_after.clone();
    tokio::spawn(async move {
        db_clone
            .tx("step1_neither_hangar_update", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(neither_item_id, neither_after_spawn.clone());
                Ok(())
            })
            .expect("neither commit");
    })
    .await
    .expect("spawned task");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "Step 1 off by default must emit 0 OnItemChange from markerless outside-RPC commit"
    );

    // 4. Hand-written helpers outside RPC still send normally in Step 1 while classifying
    //    `replaced`, `superseded`, and `no_projected_event` into `by_scope.{tick, neither}`.
    with_compare_tick_scope(async {
        ctx.notify_item_mutation(pilot_id, None, Some(&tick_probe_after));
    })
    .await;
    let tick_helper_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        tick_helper_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "hand-written helper in tick must still emit live while Step 1 is OFF"
    );

    ctx.emit_item_change_routed(
        pilot_id,
        &neither_hangar_before,
        vec![(9, PyRep::Integer(50))],
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&no_proj_hangar));
    let neither_helper_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        neither_helper_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        2,
        "hand-written helpers in neither must still emit live while Step 1 is OFF"
    );

    let sw = projected_item_switch_summary(&ctx);
    assert!(!sw.config.char_same_scene_item_outside_rpc);
    assert_eq!(sw.emitted, 1, "only dispatch commit emitted live; sw={sw:#?}");
    assert_eq!(sw.replaced, 0, "sw={sw:#?}");
    assert_eq!(sw.superseded, 0, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token, 0, "sw={sw:#?}");
    assert_eq!(
        sw.not_covered_by_reason.get("not in an RPC").copied(),
        Some(3),
        "outside helpers keep reporting NotInAnRpc while Step 1 switch is OFF; sw={sw:#?}"
    );
    assert_eq!(sw.by_scope.dispatch.would_announce, 1, "sw={sw:#?}");
    assert_eq!(
        sw.by_scope.tick,
        SwitchScopeOutcomeCounts {
            would_announce: 1,
            replaced: 1,
            superseded: 0,
            no_projected_event: 0,
        },
        "sw={sw:#?}"
    );
    assert_eq!(
        sw.by_scope.neither,
        SwitchScopeOutcomeCounts {
            would_announce: 1,
            replaced: 0,
            superseded: 1,
            no_projected_event: 1,
        },
        "sw={sw:#?}"
    );

    let md = sw.format_markdown_report();
    assert!(md.contains("char_same_scene_item_outside_rpc=false"), "{md}");
    assert!(md.contains("Per-Scope Projection & Dedupe Outcomes (`by_scope`)"), "{md}");
    assert!(md.contains("| `tick` | 1 | 1 | 0 | 0 |"), "{md}");
    assert!(md.contains("| `neither` | 1 | 0 | 1 | 1 |"), "{md}");

    let round_trip: ProjectedItemSwitchSummary =
        serde_json::from_value(serde_json::to_value(&sw).expect("serialize")).expect("deserialize");
    assert_eq!(round_trip.by_scope, sw.by_scope);

    // Marker-free compare check: both `Tick` and `Neither` first-set rows qualify as `"tick"`
    // candidates without needing any telemetry marker.
    let cmp = flush_item_compare_summary(&ctx);
    assert!(
        cmp.item_change
            .by_scope
            .tick
            .would_qualify
            .get("tick")
            .is_some_and(|c| c.matched == 1),
        "tick probe return should match in compare telemetry: {:#?}",
        cmp.item_change.by_scope.tick
    );
    assert!(
        cmp.item_change
            .by_scope
            .neither
            .would_qualify
            .get("tick")
            .is_some_and(|c| c.matched == 0 && c.hand_written_only == 2 && c.projected_only == 1),
        "markerless neither commit + helpers should classify in would_qualify.tick: {:#?}",
        cmp.item_change.by_scope.neither
    );
    let cmp_md = cmp.format_markdown_report();
    assert!(cmp_md.contains("Next-Class Candidate Qualification by Scope"), "{cmp_md}");
    set_item_compare_override_for_test(&ctx, None, None);
}

#[tokio::test]
async fn step1_outside_rpc_enabled_announces_markerless_commits_and_isolates_dispatch_vs_outside_facts() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_960_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_outside_rpc(true)
            .with_char_all_items(false),
    );

    let monitor_id = 77_960_001_i64;
    let mat_type_id: u32 = 38;
    let probe_spawn_id = 993_960_021_i64;
    let probe_tick_id = 993_960_022_i64;
    let hangar_item_id = 993_960_023_i64;
    let rpc_only_item_id = 993_960_024_i64;
    let silent_clear_id = 993_960_025_i64;
    let dispatch_silent_clear_id = 993_960_026_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Place pilot in space in active ship for probe-to-cargo checks, with an existing cargo probe stack.
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = InventoryFlag::None;
        }
        snap.items.insert(
            probe_tick_id,
            InventoryItemRecord {
                item_id: probe_tick_id,
                type_id: 30_013,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let mut monitors = BTreeMap::new();
        monitors.insert(
            monitor_id,
            IndustryMaterialMonitorSub {
                char_id: pilot_id,
                owner_id: pilot_id,
                location_id: station_id,
                flag_id: InventoryFlag::Hangar as i64,
            },
        );
        save_industry_material_monitors(snap, &monitors);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Markerless spawned task recovers probe from space (`None`) into active ship cargo (`Cargo`),
    //    even without any telemetry tick scope -> announces immediately via commit listener.
    let mut probe_spawn_cargo = InventoryItemRecord {
        item_id: probe_spawn_id,
        type_id: 30_013,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 8,
        singleton: false,
        version: 2,
        ..Default::default()
    };
    let db_clone = ctx.db.clone();
    let probe_spawn_commit = probe_spawn_cargo.clone();
    tokio::spawn(async move {
        db_clone
            .tx("markerless_probe_recovery", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(probe_spawn_id, probe_spawn_commit.clone());
                Ok(())
            })
            .expect("spawned probe commit");
    })
    .await
    .expect("spawn");
    let spawn_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        spawn_notifs.iter().map(|n| n.event.as_str()).collect::<Vec<_>>(),
        vec!["OnItemChange", "OnMultipleItemChange"],
        "markerless spawned commit returning probe to cargo in space must announce immediately"
    );

    // 2. Repeated matching helper outside RPC -> suppressed (`replaced`); stale row -> suppressed (`superseded`).
    ctx.notify_item_mutation(pilot_id, None, Some(&probe_spawn_cargo));
    ctx.notify_item_mutation(pilot_id, None, Some(&probe_spawn_cargo));
    probe_spawn_cargo.quantity = 4;
    ctx.emit_item_change_routed(pilot_id, &probe_spawn_cargo, vec![(9, PyRep::Integer(8))]);
    probe_spawn_cargo.quantity = 8;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "repeated equal and stale helper calls outside RPC must stay suppressed"
    );

    // 3. Outside fact does NOT leak into inside-dispatch hand-written calls (which check dispatch facts only),
    //    and an RPC call inside dispatch does NOT clear the outside fact for later outside calls.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.emit_item_change_routed(pilot_id, &probe_spawn_cargo, vec![(9, PyRep::Integer(1))]);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "hand-written call inside dispatch must only check dispatch facts -> sends when only outside fact is set"
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&probe_spawn_cargo));
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "outside fact must still exist right after an unrelated RPC dispatch"
    );

    // 4. Conversely, a commit inside dispatch sets ONLY dispatch facts (dropped at dispatch end),
    //    so a later outside-RPC helper still sends (`no_projected_event`).
    let mut rpc_row = InventoryItemRecord {
        item_id: rpc_only_item_id,
        type_id: 30_013,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 2,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("rpc_only_cargo_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(rpc_only_item_id, rpc_row.clone());
                Ok(())
            })
            .expect("rpc insert");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1
    );
    rpc_row.version = 1;
    ctx.notify_item_mutation(pilot_id, None, Some(&rpc_row));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "outside call must check outside facts only, never stale dispatch facts"
    );

    // 5. Tick-scoped probe update + docked hangar commit with OnIndustryMaterials causal ordering.
    let tick_probe_cargo = InventoryItemRecord {
        item_id: probe_tick_id,
        type_id: 30_013,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 8,
        version: 2,
        ..Default::default()
    };
    with_compare_tick_scope(async {
        ctx.db
            .tx("tick_probe_return", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(probe_tick_id, tick_probe_cargo.clone());
                Ok(())
            })
            .expect("tick probe");
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .map(|n| n.event.as_str())
            .collect::<Vec<_>>(),
        vec!["OnItemChange", "OnMultipleItemChange"]
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = station_id;
            ship.flag = InventoryFlag::Hangar;
        }
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let hangar_row = InventoryItemRecord {
        item_id: hangar_item_id,
        type_id: mat_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 500,
        version: 1,
        ..Default::default()
    };
    ctx.db
        .tx("outside_hangar_with_industry_monitor", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(hangar_item_id, hangar_row.clone());
            Ok(())
        })
        .expect("hangar commit");
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .map(|n| n.event.as_str())
            .collect::<Vec<_>>(),
        vec!["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]
    );
    assert!(
        ctx.projected_item_switches
            .is_industry_monitor_covered_in_scope(None, monitor_id, &[mat_type_id]),
        "outside industry monitor cover flag must be recorded while outside switch is ON"
    );

    // 6. Any commit touching an item that does not announce it clears the fact in BOTH places:
    //    (a) Outside fact cleared by silent non-first-set commit (moved to DroneBay).
    let silent_first_set = InventoryItemRecord {
        item_id: silent_clear_id,
        type_id: 2454,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 2,
        version: 1,
        ..Default::default()
    };
    ctx.db
        .tx("outside_set_before_silent_clear", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(silent_clear_id, silent_first_set.clone());
            Ok(())
        })
        .expect("set outside fact");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    ctx.db
        .tx("silent_move_to_dronebay", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.get_mut(&silent_clear_id).expect("item").flag = InventoryFlag::DroneBay;
            Ok(())
        })
        .expect("silent move");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "Cargo -> DroneBay transition is not first-set and stays silent"
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&silent_first_set));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "silent commit touching the item must clear its outside fact so later hand-written call sends"
    );

    //    (b) Dispatch fact cleared by a subsequent silent commit in the same dispatch (AND outside fact cleared too).
    ctx.db
        .tx("reset_outside_fact_for_cross_clear", |tx| -> Result<(), eve_db::TxError> {
            let r = tx.snap.items.get_mut(&silent_clear_id).expect("item");
            r.flag = InventoryFlag::Cargo;
            r.quantity = 3;
            Ok(())
        })
        .expect("reset silent_clear_id");
    let outside_reset_hangar = InventoryItemRecord {
        item_id: silent_clear_id,
        type_id: 2454,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 3,
        version: 4,
        ..Default::default()
    };
    ctx.db
        .tx("set_outside_hangar_fact_before_dispatch_clear", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(silent_clear_id, outside_reset_hangar.clone());
            Ok(())
        })
        .expect("set outside hangar fact");
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let dispatch_clear_row = InventoryItemRecord {
        item_id: dispatch_silent_clear_id,
        type_id: 2454,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 6,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("dispatch_commit_sets_fact", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(dispatch_silent_clear_id, dispatch_clear_row.clone());
                Ok(())
            })
            .expect("dispatch commit 1");
        ctx.db
            .tx("dispatch_silent_commit_clears_both", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.get_mut(&dispatch_silent_clear_id).expect("d").flag =
                    InventoryFlag::DroneBay;
                tx.snap.items.get_mut(&dispatch_silent_clear_id).expect("d").location_id = ship_id;
                tx.snap.items.get_mut(&silent_clear_id).expect("s").flag = InventoryFlag::DroneBay;
                tx.snap.items.get_mut(&silent_clear_id).expect("s").location_id = ship_id;
                Ok(())
            })
            .expect("silent commit inside dispatch");
        ctx.notify_item_mutation(pilot_id, None, Some(&dispatch_clear_row));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let disp_clear_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        disp_clear_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        2,
        "Commit 1 emitted once, silent Commit 2 cleared dispatch fact so hand-written emit sent again"
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&outside_reset_hangar));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "silent Commit 2 inside dispatch also cleared the prior outside fact for silent_clear_id"
    );

    let sw = projected_item_switch_summary(&ctx);
    assert!(sw.by_scope.tick.would_announce >= 1, "sw={sw:#?}");
    assert!(sw.by_scope.neither.would_announce >= 3, "sw={sw:#?}");
    assert!(sw.by_scope.neither.replaced >= 2, "sw={sw:#?}");
    assert!(sw.by_scope.neither.superseded >= 1, "sw={sw:#?}");
    assert!(sw.by_scope.neither.no_projected_event >= 2, "sw={sw:#?}");
    assert!(sw.by_scope.dispatch.no_projected_event >= 2, "sw={sw:#?}");
}

#[tokio::test]
async fn step1_outside_rpc_exclusions_hold_silent_commits() {
    let ctx = test_ctx();
    let (pilot_id, station_id, other_station, sys_id, ship_id, other_ship_id, cont_id) = (
        90_000_001_i64,
        60_003_760_i64,
        60_003_761_i64,
        30_000_142_u32,
        993_970_010_i64,
        993_970_011_i64,
        993_970_012_i64,
    );
    // Seed station container while pilot is offline and outside switch is OFF so setup does not
    // project or record an outside-RPC emission.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 23,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_char_same_scene_item_fitted(false)
            .with_char_all_items(false),
    );

    let fitted_id = 993_970_021_i64;
    let space_item_id = 993_970_022_i64;
    let cont_item_id = 993_970_023_i64;
    let corp_item_id = 993_970_024_i64;
    let fuel_item_id = 993_970_025_i64;
    let exp_item_id = 993_970_026_i64;
    let scene_item_id = 993_970_027_i64;
    let offline_item_id = 993_970_028_i64;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&pilot_id].corporation_id);

    // 1. Fitted module, item in space, container item, non-char corp item -> all silent outside RPC.
    with_compare_tick_scope(async {
        ctx.db
            .tx("outside_non_first_set_exclusions", |tx| -> Result<(), eve_db::TxError> {
                let mut ins = |id: i64, owner: i64, loc: i64, flag: InventoryFlag| {
                    tx.snap.items.insert(
                        id,
                        InventoryItemRecord {
                            item_id: id,
                            type_id: 34,
                            owner_id: owner,
                            location_id: loc,
                            flag,
                            quantity: 5,
                            version: 1,
                            ..Default::default()
                        },
                    );
                };
                ins(fitted_id, pilot_id, ship_id, InventoryFlag::HiSlot0);
                ins(space_item_id, pilot_id, i64::from(sys_id), InventoryFlag::None);
                ins(cont_item_id, pilot_id, cont_id, InventoryFlag::Cargo);
                ins(corp_item_id, corp_id, station_id, InventoryFlag::CorpSAG1);
                Ok(())
            })
            .expect("exclusions tx");
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "fitted/space/container/corp items must never project outside RPC"
    );

    // 2. Fuel reservation guard silences outside commit and skips setting an outside fact.
    let fuel_item = InventoryItemRecord {
        item_id: fuel_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        version: 1,
        ..Default::default()
    };
    let _fuel_guard = ctx
        .active_fuel_reserves
        .begin_reserve(crate::services::tactical_modules::db_ptr_key(&ctx.db), [fuel_item_id]);
    ctx.db
        .tx("outside_fuel_reserve_tx", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(fuel_item_id, fuel_item.clone());
            Ok(())
        })
        .expect("fuel tx");
    drop(_fuel_guard);
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "fuel reservation commit must keep projection silent outside RPC"
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&fuel_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "fuel reservation exclusion must keep outside fact unset so hand-written helper still sends"
    );

    // 3. Explicit TxNotificationEffect on item suppresses outside projection & leaves outside fact unset.
    let exp_item = InventoryItemRecord {
        item_id: exp_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 60,
        version: 1,
        ..Default::default()
    };
    ctx.db
        .tx("outside_explicit_effect_tx", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(exp_item_id, exp_item.clone());
            let row = super::delta_projection::build_inv_item_dbrow(&exp_item, &ctx.sde, "");
            tx.effects
                .notify_character(pilot_id, "OnItemChange", "charid", vec![row, PyRep::Dict(vec![])]);
            Ok(())
        })
        .expect("exp tx");
    let exp_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        exp_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "explicit tx effect emits once; outside projection stays silent"
    );
    ctx.notify_item_mutation(pilot_id, None, Some(&exp_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "explicit effect commit must not install an outside dedupe fact"
    );

    // 4. Commit changing pilot station/scene holds item projection silent outside RPC.
    ctx.db
        .tx("outside_scene_transition_tx", |tx| -> Result<(), eve_db::TxError> {
            let ch = tx.snap.characters.get_mut(&pilot_id).expect("pilot");
            ch.station_id = Some(other_station);
            ch.active_ship_id = other_ship_id;
            tx.snap.items.insert(
                scene_item_id,
                InventoryItemRecord {
                    item_id: scene_item_id,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: other_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 12,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("scene change tx");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "commit changing pilot station_id must stay silent outside RPC"
    );

    // 5. Offline pilot stays silent outside RPC.
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db.release_online_lock(uid, 9_310);
    ctx.db
        .tx("outside_offline_tx", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(
                offline_item_id,
                InventoryItemRecord {
                    item_id: offline_item_id,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: other_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 33,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("offline tx");
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.emitted, 0, "all excluded commits must stay unprojected: {sw:#?}");
    assert!(sw.not_covered_by_reason.contains_key("explicit effect"), "{sw:#?}");
    assert!(sw.not_covered_by_reason.contains_key("fuel reservation"), "{sw:#?}");
}

#[tokio::test]
async fn step1_outside_rpc_unbounded_untimed_persistence_and_offline_clear() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_980_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled().with_char_all_items(false));

    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    let alt_char_id = 90_931_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut alt = snap.characters[&pilot_id].clone();
        alt.character_id = alt_char_id;
        alt.name = "Alt Char".to_string();
        snap.characters.insert(alt_char_id, alt);
    });

    // 1. Unbounded persistence: >4,096 distinct items outside RPC never evict item #0.
    const COUNT: usize = 4_150;
    const BASE_ID: i64 = 994_000_000;
    let mk_hangar = |item_id: i64| InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 5,
        version: 1,
        ..Default::default()
    };
    for batch_start in (0..COUNT).step_by(40) {
        let batch_end = (batch_start + 40).min(COUNT);
        ctx.db
            .tx("outside_unbounded_batch", |tx| -> Result<(), eve_db::TxError> {
                for i in batch_start..batch_end {
                    let id = BASE_ID + i as i64;
                    tx.snap.items.insert(id, mk_hangar(id));
                }
                Ok(())
            })
            .expect("batch commit");
        let drained = ctx.drain_notifications_for_char(pilot_id);
        assert_eq!(
            drained.iter().filter(|n| n.event == "OnItemChange").count(),
            batch_end - batch_start
        );
    }

    let first_item = mk_hangar(BASE_ID);
    ctx.notify_item_mutation(pilot_id, None, Some(&first_item));
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "item #0 must still be present in outside_facts after 4,150 items with zero cap/eviction"
    );
    assert_eq!(projected_item_switch_summary(&ctx).evictions, 0);

    // 2. Disconnect (`release_online_lock`) clears outside facts even when reconnecting with the SAME session_id.
    assert!(ctx.db.release_online_lock(uid, 9_310));
    ctx.db
        .acquire_online_lock(uid, 9_310, Some(pilot_id), true, 1_700_000_600)
        .expect("reconnect same session_id");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.notify_item_mutation(pilot_id, None, Some(&first_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "reconnect even with same session_id must see outside facts cleared on prior offline"
    );

    // 3. Character switch on the same account clears prior character's outside facts.
    let switch_item = mk_hangar(994_100_001);
    ctx.db
        .tx("outside_before_char_switch", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(switch_item.item_id, switch_item.clone());
            Ok(())
        })
        .expect("char switch setup");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db
        .acquire_online_lock(uid, 9_310, Some(alt_char_id), true, 1_700_000_700)
        .expect("switch to alt");
    ctx.db
        .acquire_online_lock(uid, 9_310, Some(pilot_id), true, 1_700_000_710)
        .expect("switch back to pilot");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.notify_item_mutation(pilot_id, None, Some(&switch_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "switching characters must clear previous character's outside facts"
    );

    // 4. Direct session removal (`kick_user`/teardown via WorldRepository::tx) clears outside facts.
    let kick_item = mk_hangar(994_100_002);
    ctx.db
        .tx("outside_before_kick_remove", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(kick_item.item_id, kick_item.clone());
            Ok(())
        })
        .expect("kick setup");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db
        .tx("simulate_kick_session_removal", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.sessions.remove(&uid);
            Ok(())
        })
        .expect("kick tx");
    ctx.db
        .acquire_online_lock(uid, 9_312, Some(pilot_id), true, 1_700_000_800)
        .expect("relogin after kick");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.notify_item_mutation(pilot_id, None, Some(&kick_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "direct session removal (kick) must clear outside facts"
    );

    // 5. `clear_notifications_for_char` also clears outside facts immediately.
    let clear_item = mk_hangar(994_100_003);
    ctx.db
        .tx("outside_before_clear_helper", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(clear_item.item_id, clear_item.clone());
            Ok(())
        })
        .expect("clear helper setup");
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.clear_notifications_for_char(pilot_id);
    ctx.notify_item_mutation(pilot_id, None, Some(&clear_item));
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "clear_notifications_for_char must clear outside facts"
    );
}

#[tokio::test]
async fn outside_rpc_narrowed_pilot_moved_rule_suppresses_transitions_and_projects_docked_ship_switches() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_a, ship_b) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_980_010_i64,
        993_980_011_i64,
    );
    let structure_id = 1_030_000_993_980_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_a);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled().with_char_all_items(false));

    // 1. Docked in station: active_ship_id-only switch projects outside RPC normally.
    let station_cap_id = 993_980_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = station_cap_id;
        snap.items.insert(
            station_cap_id,
            InventoryItemRecord {
                item_id: station_cap_id,
                type_id: 670,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db
        .tx("outside_docked_station_ship_swap", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = ship_a;
            tx.snap.remove_system_cleanup_item(station_cap_id);
            Ok(())
        })
        .expect("outside station ship swap");
    let st_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        st_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "docked station active_ship_id-only change must project outside RPC: {st_notifs:?}"
    );

    // 2. Docked in structure: active_ship_id-only switch projects outside RPC normally.
    let struct_item_id = 993_980_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.active_ship_id = ship_a;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db
        .tx("outside_docked_struct_ship_swap", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = ship_b;
            tx.snap.items.insert(
                struct_item_id,
                InventoryItemRecord {
                    item_id: struct_item_id,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: structure_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("outside struct ship swap");
    let sr_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        sr_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "docked structure active_ship_id-only change must project outside RPC: {sr_notifs:?}"
    );

    // 3. Suppression on structure_id change, solar_system_id change, and in-space active_ship_id change.
    let suppressed_a = 993_980_022_i64;
    let suppressed_b = 993_980_023_i64;
    let suppressed_c = 993_980_024_i64;
    ctx.db
        .tx("outside_struct_transition", |tx| -> Result<(), eve_db::TxError> {
            let ch = tx.snap.characters.get_mut(&pilot_id).expect("pilot");
            ch.structure_id = Some(structure_id + 1);
            tx.snap.items.insert(
                suppressed_a,
                InventoryItemRecord {
                    item_id: suppressed_a,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: structure_id + 1,
                    flag: InventoryFlag::Hangar,
                    quantity: 3,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("struct id transition");
    ctx.db
        .tx("outside_undock_and_sys_transition", |tx| -> Result<(), eve_db::TxError> {
            let ch = tx.snap.characters.get_mut(&pilot_id).expect("pilot");
            ch.structure_id = None;
            ch.station_id = None;
            ch.solar_system_id = 30_000_144;
            tx.snap.items.insert(
                suppressed_b,
                InventoryItemRecord {
                    item_id: suppressed_b,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: ship_b,
                    flag: InventoryFlag::Cargo,
                    quantity: 4,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("sys transition");
    ctx.db
        .tx("outside_space_ship_transition", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = ship_a;
            tx.snap.items.insert(
                suppressed_c,
                InventoryItemRecord {
                    item_id: suppressed_c,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: ship_a,
                    flag: InventoryFlag::Cargo,
                    quantity: 6,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("space ship transition");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "structure_id, solar_system_id, and in-space active_ship_id transitions must stay silent outside RPC"
    );

    // 4. Suppression on character create (`None -> Some`) and character delete (`Some -> None`).
    let new_cid = 90_993_980_i64;
    let new_uid = 993_980_u64;
    let created_item_id = 993_980_025_i64;
    let base_char = ctx.db.with_snapshot(|s| s.characters[&pilot_id].clone());
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("outside_char_{new_uid}"),
            eve_db::AccountRecord {
                user_id: new_uid,
                username: format!("outside_char_{new_uid}"),
                password_token: "test".to_string(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(new_uid, 9_315, Some(new_cid), true, 1_700_000_900)
        .expect("online lock new char");
    ctx.db
        .tx("outside_char_create_transition", |tx| -> Result<(), eve_db::TxError> {
            let mut created = base_char.clone();
            created.character_id = new_cid;
            created.user_id = new_uid;
            created.station_id = Some(station_id);
            created.structure_id = None;
            created.active_ship_id = ship_a;
            tx.snap.characters.insert(new_cid, created);
            tx.snap.items.insert(
                created_item_id,
                InventoryItemRecord {
                    item_id: created_item_id,
                    type_id: 34,
                    owner_id: new_cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 7,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("char create tx");
    assert!(
        ctx.drain_notifications_for_char(new_cid).is_empty(),
        "character creation transition must hold back projection outside RPC"
    );
    ctx.db
        .tx("outside_char_delete_transition", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.characters.remove(&pilot_id);
            tx.snap.items.insert(
                suppressed_c + 1,
                InventoryItemRecord {
                    item_id: suppressed_c + 1,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: ship_a,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("char delete tx");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "character deletion transition must hold back projection outside RPC"
    );
    ctx.db.release_online_lock(uid, 9_310);
    ctx.db.release_online_lock(new_uid, 9_315);
}

#[tokio::test]
async fn step1_fitted_outside_rpc_requires_both_fitted_and_outside_rpc_switches() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_995_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));

    let charge_id = 993_995_021_i64;
    let mod_id = 993_995_022_i64;
    let hangar_item_id = 993_995_023_i64;
    let mut charge = InventoryItemRecord {
        item_id: charge_id,
        type_id: 179,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::HiSlot0,
        quantity: 50,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    let mut fitted_mod = InventoryItemRecord {
        item_id: mod_id,
        type_id: 487,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::HiSlot0,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let mut hangar_item = InventoryItemRecord {
        item_id: hangar_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 10,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(charge_id, charge.clone());
        snap.items.insert(mod_id, fitted_mod.clone());
        snap.items.insert(hangar_item_id, hangar_item.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. `fitted = true, outside_rpc = false` (pre-`char_all_items`): outside-RPC fitted charge tick does NOT project.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_char_same_scene_item_fitted(true)
            .with_outside_rpc(false)
            .with_char_all_items(false),
    );
    with_compare_tick_scope(async {
        charge.quantity = 49;
        charge.version += 1;
        ctx.db
            .tx("tick_fitted_only_switch", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(charge_id, charge.clone());
                Ok(())
            })
            .expect("tick_fitted_only_switch");
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "fitted=true with outside_rpc=false must never project outside RPC"
    );

    // 2. `fitted = false, outside_rpc = true` (pre-`char_all_items`): direct-slot Hangar item projects outside RPC,
    //    while fitted charge tick stays silent.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_char_same_scene_item_fitted(false)
            .with_outside_rpc(true)
            .with_char_all_items(false),
    );
    with_compare_tick_scope(async {
        charge.quantity = 48;
        charge.version += 1;
        hangar_item.quantity = 12;
        hangar_item.version += 1;
        ctx.db
            .tx("tick_outside_only_switch", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(charge_id, charge.clone());
                tx.snap.items.insert(hangar_item_id, hangar_item.clone());
                Ok(())
            })
            .expect("tick_outside_only_switch");
    })
    .await;
    let only_hangar: Vec<i64> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args[0].dbrow_get("itemID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(
        only_hangar,
        vec![hangar_item_id],
        "fitted=false, outside_rpc=true projects direct-slot Hangar but holds fitted charge"
    );

    // 3. BOTH `char_same_scene_item_fitted = true` && `char_same_scene_item_outside_rpc = true`:
    //    tick-scoped and markerless outside-RPC commits project fitted charge ticks, slot swaps,
    //    and deletions, while consuming duplicate hand-written helpers cleanly.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled().with_char_all_items(false));
    with_compare_tick_scope(async {
        let charge_prev = charge.clone();
        charge.quantity = 47;
        charge.version += 1;
        ctx.db
            .tx("tick_fitted_charge_consume", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(charge_id, charge.clone());
                Ok(())
            })
            .expect("tick_fitted_charge_consume");
        ctx.notify_item_mutation(pilot_id, Some(&charge_prev), Some(&charge));
    })
    .await;
    let tick_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        tick_notifs.len(),
        1,
        "tick fitted charge consume emits once and consumes hand-written helper: {tick_notifs:?}"
    );
    assert_eq!(tick_notifs[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(47)));

    // Markerless outside-RPC slot swap + fitted charge depletion.
    let mod_prev = fitted_mod.clone();
    fitted_mod.flag = InventoryFlag::HiSlot1;
    fitted_mod.version += 1;
    ctx.db
        .tx("neither_fitted_swap_and_deplete", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(mod_id, fitted_mod.clone());
            tx.snap.items.remove(&charge_id);
            Ok(())
        })
        .expect("neither_fitted_swap_and_deplete");
    ctx.notify_item_mutation(pilot_id, Some(&mod_prev), Some(&fitted_mod));
    ctx.notify_item_mutation(pilot_id, Some(&charge), None);

    let markerless_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        markerless_notifs.len(),
        2,
        "markerless fitted swap + depleted charge must project once each and consume hand-written helpers: {markerless_notifs:?}"
    );
}

#[tokio::test]
async fn step5b_outside_rpc_char_all_items_projects_and_invalidates_blueprints_and_transfers() {
    use crate::services::industry_pi::ATTR_BLUEPRINT_ME;

    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, other_station, sys_id, ship1, ship2) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        60_003_761_i64,
        30_000_142_u32,
        993_995_010_i64,
        993_995_011_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let fuel_id = 993_995_021_i64;
    let remote_id = 993_995_022_i64;
    let bpo_id = 993_995_023_i64;
    let xfer_id = 993_995_024_i64;

    let mut fuel = InventoryItemRecord {
        item_id: fuel_id,
        type_id: 16_274,
        owner_id: p1_id,
        location_id: ship1,
        flag: InventoryFlag::SpecializedFuelBay,
        quantity: 20,
        version: 1,
        ..Default::default()
    };
    let remote = InventoryItemRecord {
        item_id: remote_id,
        type_id: 34,
        owner_id: p1_id,
        location_id: other_station,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        version: 1,
        ..Default::default()
    };
    let mut bpo = InventoryItemRecord {
        item_id: bpo_id,
        type_id: bp_type_id,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Locked,
        quantity: 1,
        singleton: true,
        blueprint_runs_remaining: Some(-1),
        version: 1,
        ..Default::default()
    };
    let xfer_pre = InventoryItemRecord {
        item_id: xfer_id,
        type_id: 35,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Deliveries,
        quantity: 10,
        version: 1,
        ..Default::default()
    };
    let mut xfer_post = xfer_pre.clone();
    xfer_post.owner_id = p2_id;
    xfer_post.flag = InventoryFlag::Hangar;
    xfer_post.version = 2;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(xfer_id, xfer_pre.clone());
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    // 1. Outside-RPC <tick>: generalized items + locked blueprint + cross-owner transfer.
    with_compare_tick_scope(async {
        ctx.db
            .tx("outside_5b_all", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(fuel_id, fuel.clone());
                tx.snap.items.insert(remote_id, remote.clone());
                tx.snap.items.insert(bpo_id, bpo.clone());
                tx.snap.items.insert(xfer_id, xfer_post.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(p1_id, None, Some(&fuel));
        ctx.notify_item_mutation(p1_id, None, Some(&remote));
        ctx.notify_item_mutation(p1_id, None, Some(&bpo));
        ctx.notify_item_mutation(p2_id, Some(&xfer_pre), Some(&xfer_post));
    })
    .await;

    let n1 = ctx.drain_notifications_for_char(p1_id);
    let n2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(n1.iter().filter(|n| n.event == "OnItemChange").count(), 4);
    assert_eq!(n1.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);
    assert_eq!(n2.iter().filter(|n| n.event == "OnItemChange").count(), 1);

    // Non-blueprint commit preserves P1's outside blueprint dedupe fact; offline clear resets it.
    fuel.quantity = 25;
    fuel.version = 2;
    ctx.db
        .tx("outside_non_bp_preserves_fact", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(fuel_id, fuel.clone());
            Ok(())
        })
        .expect("tx");
    let _ = ctx.drain_notifications_for_char(p1_id);
    ctx.notify(
        super::NotificationTarget::Character(p1_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(p1_id)],
    );
    assert!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .all(|n| n.event != "OnBlueprintsUpdated"),
        "non-blueprint commit must preserve outside-RPC blueprint dedupe fact"
    );

    // Explicit-effect blueprint commit invalidates outside-RPC blueprint fact for P1.
    let mut bpo_me = bpo.clone();
    bpo_me.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 5.0);
    bpo_me.version = 2;
    ctx.db
        .tx("outside_bp_explicit_invalidation", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(bpo_id, bpo_me.clone());
            tx.effects.notify_character(
                p1_id,
                "OnBlueprintsUpdated",
                "ownerID",
                vec![PyRep::Integer(p1_id)],
            );
            Ok(())
        })
        .expect("tx");
    let _ = ctx.drain_notifications_for_char(p1_id);
    ctx.notify(
        super::NotificationTarget::Character(p1_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(p1_id)],
    );
    assert_eq!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1,
        "explicit-effect blueprint commit must invalidate prior outside blueprint dedupe fact"
    );

    // In-place blueprint commit sets fact, then logout clears outside blueprint dedupe fact.
    bpo = bpo_me;
    bpo.blueprint_runs_remaining = Some(2);
    bpo.version = 3;
    ctx.db
        .tx("outside_bp_in_place", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(bpo_id, bpo);
            Ok(())
        })
        .expect("tx");
    assert_eq!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1
    );
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&p1_id].user_id);
    assert!(ctx.db.release_online_lock(uid1, 9_310));
    ctx.db
        .acquire_online_lock(uid1, 9_310, Some(p1_id), true, 1_700_000_900)
        .expect("relogin");
    let _ = ctx.drain_notifications_for_char(p1_id);
    ctx.notify(
        super::NotificationTarget::Character(p1_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(p1_id)],
    );
    assert_eq!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1,
        "logout/offline clear must invalidate outside-RPC blueprint dedupe facts"
    );

    // P2-3: Shadow dry-run (`char_all_items=true, outside_rpc=false`) checks `shadow.facts` in
    // `has_fact_in_scope(None, ...)` even when outgoing `OnItemChange` omits `ixOwnerID (2)`.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::all_enabled().with_outside_rpc(false),
    );
    let shadow_xfer_id = 993_995_025_i64;
    let shadow_pre = InventoryItemRecord {
        item_id: shadow_xfer_id,
        type_id: 34,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 42,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(shadow_xfer_id, shadow_pre);
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let shadow_post = InventoryItemRecord {
        item_id: shadow_xfer_id,
        type_id: 34,
        owner_id: p2_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 42,
        version: 2,
        ..Default::default()
    };
    ctx.db
        .tx("shadow_outgoing_transfer", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(shadow_xfer_id, shadow_post.clone());
            Ok(())
        })
        .expect("shadow_outgoing_transfer");
    let before_nc = projected_item_switch_summary(&ctx)
        .not_covered_by_reason
        .get("not character-owned")
        .copied()
        .unwrap_or(0);
    // Hand-written emit to outgoing owner `p1_id` without `ixOwnerID` in dict:
    let row = super::delta_projection::build_inv_item_dbrow(&shadow_post, &ctx.sde, "");
    ctx.notify(
        super::NotificationTarget::Character(p1_id),
        "OnItemChange",
        "charid",
        vec![row, PyRep::Dict(vec![])],
    );
    let after_nc = projected_item_switch_summary(&ctx)
        .not_covered_by_reason
        .get("not character-owned")
        .copied()
        .unwrap_or(0);
    assert_eq!(
        after_nc, before_nc,
        "shadow dry-run outgoing transfer without ixOwnerID must resolve shadow.facts instead of NotCharacterOwned"
    );
    let _ = ctx.drain_notifications_for_char(p1_id);
}

#[tokio::test]
async fn track5c_s1_outside_rpc_corp_projection_and_shadow_isolation() {
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, sys_id, ship1, ship2, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_996_010_i64,
        993_996_011_i64,
        98_000_701_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let xfer_id = 993_996_101_i64;
    let corp_bp_id = 993_996_102_i64;
    let xfer_pre = InventoryItemRecord {
        item_id: xfer_id,
        type_id: 34,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 33,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Outside Corp", "OUT", p1_id);
        corp.members.insert(p1_id);
        corp.members.insert(p2_id);
        snap.corporations.insert(corp_id, corp);
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = corp_id;
        snap.characters.get_mut(&p2_id).expect("p2").corporation_id = corp_id;
        snap.items.insert(xfer_id, xfer_pre.clone());
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    // 1. Outside-RPC <tick> with `corp_all_items=true, outside_rpc=true`: Char->Corp transfer +
    //    corp BPO project `"charid"` and `"corpid"` cleanly without fact key collision, and dedupe
    //    legacy multi-recipient Corporation emissions.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let mut xfer_post = xfer_pre.clone();
    xfer_post.owner_id = corp_id;
    xfer_post.flag = InventoryFlag::CorpSAG1;
    xfer_post.version = 2;
    let corp_bp = InventoryItemRecord {
        item_id: corp_bp_id,
        type_id: bp_type_id,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 1,
        singleton: true,
        blueprint_runs_remaining: Some(-1),
        version: 1,
        ..Default::default()
    };
    with_compare_tick_scope(async {
        ctx.db
            .tx("outside_5c_corp_tick", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(xfer_id, xfer_post.clone());
                tx.snap.items.insert(corp_bp_id, corp_bp.clone());
                Ok(())
            })
            .expect("outside corp tx");
        ctx.notify_item_mutation(p1_id, Some(&xfer_pre), Some(&xfer_post));
        ctx.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(corp_id)],
        );
    })
    .await;
    let n1 = ctx.drain_notifications_for_char(p1_id);
    let n2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(
        n1.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "charid")
            .count(),
        1,
        "p1 charid: {n1:?}"
    );
    assert_eq!(
        n1.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        2,
        "p1 corpid: {n1:?}"
    );
    assert_eq!(
        n2.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        2,
        "p2 corpid: {n2:?}"
    );
    assert_eq!(n1.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);
    assert_eq!(n2.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);

    // 2. Shadow dry-run (`corp_all_items=true, outside_rpc=false`): 0 projected notifications,
    //    legacy corp emission records `NotInAnRpc` (not `NotCharacterOwned`).
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled().with_outside_rpc(false));
    let shadow_corp_id = 993_996_103_i64;
    let shadow_corp_item = InventoryItemRecord {
        item_id: shadow_corp_id,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 9,
        version: 1,
        ..Default::default()
    };
    let before_not_char = projected_item_switch_summary(&ctx)
        .not_covered_by_reason
        .get("not character-owned")
        .copied()
        .unwrap_or(0);
    let before_not_rpc = projected_item_switch_summary(&ctx)
        .not_covered_by_reason
        .get("not in an RPC")
        .copied()
        .unwrap_or(0);
    ctx.db
        .tx("outside_5c_shadow_dry", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(shadow_corp_id, shadow_corp_item.clone());
            Ok(())
        })
        .expect("shadow dry tx");
    let row = super::delta_projection::build_inv_item_dbrow(&shadow_corp_item, &ctx.sde, "");
    ctx.notify(
        super::NotificationTarget::Corporation(corp_id),
        "OnItemChange",
        "corpid",
        vec![row, PyRep::Dict(vec![])],
    );
    let after = projected_item_switch_summary(&ctx);
    assert_eq!(
        after.not_covered_by_reason.get("not character-owned").copied().unwrap_or(0),
        before_not_char,
        "shadow corp outside_rpc=false must not record NotCharacterOwned: {:?}",
        after.not_covered_by_reason
    );
    assert!(
        after.not_covered_by_reason.get("not in an RPC").copied().unwrap_or(0)
            > before_not_rpc,
        "shadow corp outside_rpc=false must record NotInAnRpc: {:?}",
        after.not_covered_by_reason
    );
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);
}
