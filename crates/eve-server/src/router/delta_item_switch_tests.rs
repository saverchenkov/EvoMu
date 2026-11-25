//! Unit tests for per-class default-on item projection switches (`#931` Track 3B / 3C-1)
//! and Guardrail 1–5 invariants.

#![forbid(unsafe_code)]

use super::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
};
use super::delta_item_compare::{
    flush_item_compare_summary, item_compare_summary, normalize_dispatch_method_label,
    push_dispatch_rpc_method_guard, set_item_compare_override_for_test,
};
use super::delta_item_switch::*;
use super::ServerContext;
use crate::services::notify_emitters_character_services::{
    save_industry_material_monitors, IndustryMaterialMonitorSub,
};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use eve_testkit::shadow_client::ShadowClient;
use std::collections::BTreeMap;

pub(super) fn test_ctx() -> ServerContext {
    ServerContext::new()
}

pub(super) fn setup_online_pilot_in_station(
    ctx: &ServerContext,
    pilot_id: i64,
    station_id: i64,
    sys_id: u32,
    ship_id: i64,
) {
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    set_item_compare_override_for_test(ctx, Some(false), Some(None));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
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
    ctx.db
        .acquire_online_lock(uid, 9_310, Some(pilot_id), true, 1_700_000_500)
        .expect("acquire online lock");
    set_item_compare_override_for_test(ctx, None, None);
    let _ = ctx.drain_notifications_for_char(pilot_id);
}

#[test]
fn switch_config_defaults_on_and_parses_env_kill_switches() {
    let def = ProjectedItemSwitchConfig::default();
    assert!(def.char_same_scene_item_mutation);
    assert!(def.char_same_scene_item_deleted);
    assert!(def.char_same_scene_item_outside_rpc);
    assert!(def.char_same_scene_item_fitted);
    assert!(def.char_all_items);
    assert!(def.corp_all_items);
    assert!(def.is_any_enabled());
    assert_eq!(def, ProjectedItemSwitchConfig::enabled());
    assert_eq!(def, ProjectedItemSwitchConfig::all_enabled());
    assert!(ProjectedItemSwitchConfig::all_enabled().char_same_scene_item_outside_rpc);
    assert!(ProjectedItemSwitchConfig::all_enabled().char_same_scene_item_fitted);
    assert!(ProjectedItemSwitchConfig::all_enabled().char_all_items);
    assert!(ProjectedItemSwitchConfig::all_enabled().corp_all_items);
    assert!(
        !ProjectedItemSwitchConfig::enabled()
            .with_char_all_items(false)
            .char_all_items
    );
    assert_eq!(
        ProjectedItemSwitchConfig::enabled()
            .with_char_all_items(false)
            .with_char_all_items(true),
        ProjectedItemSwitchConfig::enabled()
    );
    assert!(
        !ProjectedItemSwitchConfig::enabled()
            .with_corp_all_items(false)
            .corp_all_items
    );
    assert_eq!(
        ProjectedItemSwitchConfig::enabled()
            .with_corp_all_items(false)
            .with_corp_all_items(true),
        ProjectedItemSwitchConfig::enabled()
    );
    assert!(
        !ProjectedItemSwitchConfig::enabled()
            .with_char_same_scene_item_fitted(false)
            .char_same_scene_item_fitted
    );
    assert!(
        !ProjectedItemSwitchConfig::enabled()
            .with_outside_rpc(false)
            .char_same_scene_item_outside_rpc
    );
    assert!(ProjectedItemSwitchConfig::enabled().is_corp_all_items_enabled_in_scope(true));
    assert!(
        !ProjectedItemSwitchConfig::enabled()
            .with_corp_all_items(false)
            .is_corp_all_items_enabled_in_scope(true)
    );
    assert!(
        !ProjectedItemSwitchConfig::all_enabled()
            .with_outside_rpc(false)
            .is_corp_all_items_enabled_in_scope(false)
    );
    let serde_def: ProjectedItemSwitchConfig = serde_json::from_str(
        r#"{"char_same_scene_item_mutation":true,"char_same_scene_item_deleted":true}"#,
    )
    .expect("deserialize ProjectedItemSwitchConfig with omitted outside_rpc, fitted, char_all_items, and corp_all_items");
    assert_eq!(serde_def, ProjectedItemSwitchConfig::default());

    let off = ProjectedItemSwitchConfig::disabled();
    assert!(!off.char_same_scene_item_mutation);
    assert!(!off.char_same_scene_item_deleted);
    assert!(!off.char_same_scene_item_outside_rpc);
    assert!(!off.char_same_scene_item_fitted);
    assert!(!off.char_all_items);
    assert!(!off.corp_all_items);
    assert!(!off.is_any_enabled());

    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(None, true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some(""), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some("unrecognized"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("0"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("false"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("FALSE"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("no"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("NO"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some("off"), true));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag_default(Some(" OFF "), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some("1"), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some("true"), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some("TRUE"), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some("yes"), true));
    assert!(ProjectedItemSwitchConfig::parse_env_flag_default(Some(" on "), true));

    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|_| None),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_MUTATION" => Some("0".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_DELETED" => Some("OFF".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_MUTATION" => Some("no".to_string()),
            "EVOMU_PROJECTED_ITEM_DELETED" => Some("false".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::retired_off_env_vars_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_MUTATION" => Some("no".to_string()),
            "EVOMU_PROJECTED_ITEM_DELETED" => Some("false".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("0".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some(" OFF ".to_string()),
            "EVOMU_SWITCH_CHAR_ALL_ITEMS" => Some("0".to_string()),
            "EVOMU_SWITCH_CORP_ALL_ITEMS" => Some("0".to_string()),
            _ => None,
        }),
        vec![
            "EVOMU_PROJECTED_ITEM_MUTATION",
            "EVOMU_PROJECTED_ITEM_DELETED",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED",
            "EVOMU_SWITCH_CHAR_ALL_ITEMS",
            "EVOMU_SWITCH_CORP_ALL_ITEMS",
        ],
    );
    assert!(
        ProjectedItemSwitchConfig::retired_off_env_vars_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_MUTATION" => Some("maybe".to_string()),
            "EVOMU_PROJECTED_ITEM_DELETED" => Some("".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("1".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some("true".to_string()),
            "EVOMU_SWITCH_CHAR_ALL_ITEMS" => Some("1".to_string()),
            "EVOMU_SWITCH_CORP_ALL_ITEMS" => Some("1".to_string()),
            _ => None,
        })
        .is_empty()
    );

    // False-default parser semantics remain intact in `parse_env_flag`.
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(None));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(Some("")));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(Some("0")));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(Some("false")));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(Some("no")));
    assert!(!ProjectedItemSwitchConfig::parse_env_flag(Some("off")));
    assert!(ProjectedItemSwitchConfig::parse_env_flag(Some("1")));
    assert!(ProjectedItemSwitchConfig::parse_env_flag(Some("true")));
    assert!(ProjectedItemSwitchConfig::parse_env_flag(Some("TRUE")));
    assert!(ProjectedItemSwitchConfig::parse_env_flag(Some("yes")));
    assert!(ProjectedItemSwitchConfig::parse_env_flag(Some(" on ")));
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("1".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some("1".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("true".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some("true".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    // Retired env kill-switches stay permanently ON even when explicitly set to off values.
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("0".to_string()),
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some("0".to_string()),
            "EVOMU_SWITCH_CHAR_ALL_ITEMS" => Some("0".to_string()),
            "EVOMU_SWITCH_CORP_ALL_ITEMS" => Some("0".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );
    for off_val in ["0", "false", "no", "off", " OFF "] {
        assert_eq!(
            ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
                "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some(off_val.to_string()),
                _ => None,
            }),
            ProjectedItemSwitchConfig::enabled(),
            "retired outside_rpc off_val={off_val:?} must stay enabled"
        );
        assert_eq!(
            ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
                "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some(off_val.to_string()),
                _ => None,
            }),
            ProjectedItemSwitchConfig::enabled(),
            "retired fitted off_val={off_val:?} must stay enabled"
        );
        assert_eq!(
            ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
                "EVOMU_SWITCH_CHAR_ALL_ITEMS" => Some(off_val.to_string()),
                _ => None,
            }),
            ProjectedItemSwitchConfig::enabled(),
            "retired char_all_items off_val={off_val:?} must stay enabled"
        );
        assert_eq!(
            ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
                "EVOMU_SWITCH_CORP_ALL_ITEMS" => Some(off_val.to_string()),
                _ => None,
            }),
            ProjectedItemSwitchConfig::enabled(),
            "retired corp_all_items off_val={off_val:?} must stay enabled"
        );
    }
    for def_on_val in ["", "maybe", "1", "true"] {
        assert_eq!(
            ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
                "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some(def_on_val.to_string()),
                "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED" => Some(def_on_val.to_string()),
                "EVOMU_SWITCH_CHAR_ALL_ITEMS" => Some(def_on_val.to_string()),
                "EVOMU_SWITCH_CORP_ALL_ITEMS" => Some(def_on_val.to_string()),
                _ => None,
            }),
            ProjectedItemSwitchConfig::enabled(),
            "def_on_val={def_on_val:?}"
        );
    }

    let ctx = test_ctx();
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
}

#[tokio::test]
async fn disabled_switches_preserve_handwritten_only_item_delivery() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::disabled());
    let (pilot_id, station_id, sys_id, ship_id, item_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_100_010_i64, 992_100_020_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::disabled());

    let item = InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 250,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("switch_off_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(item_id, item.clone());
                Ok(())
            })
            .expect("tx create");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "disabled switch must not emit OnItemChange from commit projection"
    );

    ctx.notify_item_mutation(pilot_id, None, Some(&item));
    let created = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        created.iter().filter(|n| n.event == "OnItemChange").count(),
        1
    );

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("switch_off_delete", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(item_id);
                Ok(())
            })
            .expect("tx delete");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "disabled deleted switch must not emit tombstone from commit projection"
    );

    ctx.emit_item_deleted_routed(pilot_id, &item);
    let deleted = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        deleted.iter().filter(|n| n.event == "OnItemChange").count(),
        1
    );
    assert!(
        !projected_item_switch_summary(&ctx).has_activity(),
        "disabled switches must not emit switch telemetry"
    );
}

#[tokio::test]
async fn mutation_switch_projects_once_dedupes_helpers_and_keeps_blueprints_compare_only() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_200_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: false,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let mut shadow = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    let trit_id = 992_200_020_i64;
    let trit = InventoryItemRecord {
        item_id: trit_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 500,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    // 1. Create inside dispatch: commit projection emits OnItemChange; notify_item_mutation skips duplicate.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("proj_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(trit_id, trit.clone());
                Ok(())
            })
            .expect("create tx");
        ctx.notify_item_mutation(pilot_id, None, Some(&trit));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let create_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        create_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "mutation switch must emit OnItemChange once on commit and suppress duplicate helper"
    );
    assert_eq!(
        create_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        1
    );
    for n in &create_notifs {
        shadow.apply_notification(&n.event, &n.args);
    }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 2. Same-scene move + stack update into active ship cargo + notify_item_record_change dedupe.
    let mut moved = trit.clone();
    moved.location_id = ship_id;
    moved.flag = InventoryFlag::Cargo;
    moved.quantity = 300;
    moved.version = 2;
    let mut dup_record = Vec::new();
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("proj_move", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(trit_id, moved.clone());
                Ok(())
            })
            .expect("move tx");
        dup_record = ctx.notify_item_record_change(
            pilot_id,
            &moved,
            vec![
                (3, PyRep::Integer(station_id)),
                (4, PyRep::Integer(InventoryFlag::Hangar as i64)),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(dup_record.is_empty(), "notify_item_record_change must consume dedupe token");
    let proj_move = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(proj_move.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    for n in &proj_move {
        shadow.apply_notification(&n.event, &n.args);
    }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 3. Blueprint creation: OnItemChange projects on commit, OnBlueprintsUpdated stays compare-only
    //    until notify_item_mutation emits it.
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type in sde");
    let bp_id = 992_200_030_i64;
    let bp = InventoryItemRecord {
        item_id: bp_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: true,
        blueprint_runs_remaining: Some(5),
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("proj_bp_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bp_id, bp.clone());
                Ok(())
            })
            .expect("bp tx");
        ctx.notify_item_mutation(pilot_id, None, Some(&bp));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let bp_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(bp_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert_eq!(
        bp_notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1
    );

    let summary = flush_item_compare_summary(&ctx);
    assert_eq!(summary.item_change.projected_only, 0, "summary={summary:#?}");
    assert_eq!(summary.item_change.hand_written_only, 0, "summary={summary:#?}");
    assert_eq!(summary.blueprints_updated.matched, 1, "summary={summary:#?}");
    assert_eq!(summary.blueprints_updated.projected_only, 0, "summary={summary:#?}");
}

#[tokio::test]
async fn mutation_switch_preserves_staged_causal_order_with_industry_materials() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, monitor_id, item_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        992_300_010_i64,
        77_931_i64,
        992_300_020_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: false,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    ctx.db.with_mut_snapshot(|snap| {
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

    let trit = InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1200,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("staged_ind_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(item_id, trit.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(pilot_id, None, Some(&trit));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    let pos_item = notifs.iter().position(|n| n.event == "OnItemChange").expect("OnItemChange");
    let pos_mat = notifs
        .iter()
        .position(|n| n.event == "OnIndustryMaterials")
        .expect("OnIndustryMaterials");
    let pos_multi = notifs
        .iter()
        .position(|n| n.event == "OnMultipleItemChange")
        .expect("OnMultipleItemChange");
    assert_eq!(notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert!(pos_item < pos_mat && pos_mat < pos_multi);
}

#[tokio::test]
async fn deleted_switch_emits_zero_quantity_tombstone_even_for_bpc_and_converges_shadow() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_400_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: false,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let bpc_id = 992_400_020_i64;
    let bpc = InventoryItemRecord {
        item_id: bpc_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: true,
        blueprint_runs_remaining: Some(3),
        version: 1,
        ..Default::default()
    };

    // Guardrail 4(b): tombstone builder must reset is_blueprint_copy so wire quantity is 0 (never -2).
    let (tomb, _) = ServerContext::build_item_tombstone(&bpc);
    assert!(!tomb.is_blueprint_copy);
    assert_eq!(tomb.location_id, 6);
    assert_eq!(tomb.quantity, 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(bpc_id, bpc.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let mut shadow = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("delete_bpc", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(bpc_id);
                Ok(())
            })
            .expect("delete tx");
        ctx.notify_item_mutation(pilot_id, Some(&bpc), None);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    let item_notifs: Vec<_> = notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(item_notifs.len(), 1, "projected tombstone must emit once and helper must dedupe");
    assert_eq!(item_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));
    assert_eq!(item_notifs[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(
        item_notifs[0].args[0].dbrow_get("quantity"),
        Some(&PyRep::Integer(0)),
        "deleted BPC tombstone must serialize wire quantity=0, never -2"
    );
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(),
        1
    );

    for n in &notifs {
        shadow.apply_notification(&n.event, &n.args);
    }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
}

#[tokio::test]
async fn guardrail3_excludes_corp_npc_corp_and_cross_owner_items() {
    let ctx = test_ctx();
    let (pilot_id, pilot2_id, corp_id, station_id, sys_id, ship_id) = (
        90_000_001_i64,
        90_000_002_i64,
        98_931_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        992_500_010_i64,
    );
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().expect("npc corp in sde");
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .insert(corp_id, CorporationRecord::new(corp_id, "G3 Corp", "G3C", pilot_id));
        if let Some(c) = snap.characters.get_mut(&pilot_id) {
            c.corporation_id = corp_id;
        }
        if let Some(c2) = snap.characters.get_mut(&pilot2_id) {
            c2.station_id = Some(station_id);
            c2.solar_system_id = sys_id;
        }
    });
    let uid2 = ctx.db.with_snapshot(|s| s.characters[&pilot2_id].user_id);
    ctx.db
        .acquire_online_lock(uid2, 9_311, Some(pilot2_id), true, 1_700_000_500)
        .expect("online pilot2");
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let _ = ctx.drain_notifications_for_char(pilot2_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g3_non_char", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    992_500_021,
                    InventoryItemRecord {
                        item_id: 992_500_021,
                        type_id: 34,
                        owner_id: corp_id,
                        location_id: station_id,
                        flag: InventoryFlag::CorpSAG1,
                        quantity: 100,
                        version: 1,
                        ..Default::default()
                    },
                );
                tx.snap.items.insert(
                    992_500_022,
                    InventoryItemRecord {
                        item_id: 992_500_022,
                        type_id: 34,
                        owner_id: npc_corp_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 100,
                        version: 1,
                        ..Default::default()
                    },
                );
                tx.snap.items.insert(
                    992_500_023,
                    InventoryItemRecord {
                        item_id: 992_500_023,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 10,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("seed tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    // Only char-owned 992_500_023 should project (not corp_id, not npc_corp_id).
    let p1 = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(p1.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert_eq!(p1[0].args[0].dbrow_get("itemID"), Some(&PyRep::Integer(992_500_023)));

    // Cross-owner transfer + corp delete stay unprojected even inside dispatch.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g3_cross_owner_and_corp_del", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.get_mut(&992_500_023).expect("item").owner_id = pilot2_id;
                tx.snap.remove_system_cleanup_item(992_500_021);
                tx.snap.remove_system_cleanup_item(992_500_022);
                Ok(())
            })
            .expect("cross-owner tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());
    assert!(ctx.drain_notifications_for_char(pilot2_id).is_empty());
}

#[tokio::test]
async fn guardrail2_excludes_transitions_cross_scene_and_remote_scene_items() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_600_010_i64);
    let remote_station_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&id| id != station_id)
        .expect("second station in sde");
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    let remote_item_id = 992_600_020_i64;
    let moved_cross_id = 992_600_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            remote_item_id,
            InventoryItemRecord {
                item_id: remote_item_id,
                type_id: 34,
                owner_id: pilot_id,
                location_id: remote_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            moved_cross_id,
            InventoryItemRecord {
                item_id: moved_cross_id,
                type_id: 34,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        // 1. Remote-station mutation & delete stay unprojected while pilot is in station_id.
        ctx.db
            .tx("g2_remote", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.get_mut(&remote_item_id).expect("remote").quantity = 40;
                Ok(())
            })
            .expect("remote update");
        ctx.db
            .tx("g2_remote_del", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(remote_item_id);
                Ok(())
            })
            .expect("remote delete");

        // 2. Cross-scene move (station A -> station B) without character transition stays unprojected.
        ctx.db
            .tx("g2_cross_scene", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.get_mut(&moved_cross_id).expect("item").location_id = remote_station_id;
                Ok(())
            })
            .expect("cross scene move");

        // 3. Undock character scene transition (`station_id: Some -> None` + ship moved to space)
        //    stays unprojected until `#1190`.
        ctx.db
            .tx("g2_undock", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").station_id = None;
                tx.snap.items.get_mut(&ship_id).expect("ship").location_id = i64::from(sys_id);
                tx.snap.items.get_mut(&ship_id).expect("ship").flag = InventoryFlag::None;
                Ok(())
            })
            .expect("undock tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    // 4a. Docked station ship-switch (`active_ship_id` only) DOES project (e.g. capsule tombstone).
    let capsule_id = 992_600_030_i64;
    let new_ship_id = 992_600_031_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.active_ship_id = capsule_id;
        snap.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
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
        snap.items.insert(
            new_ship_id,
            InventoryItemRecord {
                item_id: new_ship_id,
                type_id: 587,
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
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g2_docked_station_board_ship", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = new_ship_id;
                tx.snap.remove_system_cleanup_item(capsule_id);
                Ok(())
            })
            .expect("docked station board tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let docked_station_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        docked_station_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "docked station active_ship_id-only switch must project capsule tombstone: {docked_station_notifs:?}"
    );
    assert_eq!(
        docked_station_notifs[0]
            .args
            .first()
            .and_then(|r| r.dbrow_get("itemID"))
            .and_then(PyRep::as_i64),
        Some(capsule_id)
    );
    assert_eq!(
        docked_station_notifs[0]
            .args
            .first()
            .and_then(|r| r.dbrow_get("locationID"))
            .and_then(PyRep::as_i64),
        Some(6)
    );

    // 4b. Docked Upwell structure ship-switch (`active_ship_id` only) ALSO projects capsule tombstone.
    let structure_id = 1_030_000_992_601_i64;
    let struct_cap_id = 992_600_032_i64;
    let struct_ship_id = 992_600_033_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.active_ship_id = struct_cap_id;
        snap.items.insert(
            struct_cap_id,
            InventoryItemRecord {
                item_id: struct_cap_id,
                type_id: 670,
                owner_id: pilot_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            struct_ship_id,
            InventoryItemRecord {
                item_id: struct_ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g2_docked_structure_board_ship", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = struct_ship_id;
                tx.snap.remove_system_cleanup_item(struct_cap_id);
                Ok(())
            })
            .expect("docked structure board tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let docked_struct_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        docked_struct_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "docked structure active_ship_id-only switch must project capsule tombstone: {docked_struct_notifs:?}"
    );
    assert_eq!(
        docked_struct_notifs[0]
            .args
            .first()
            .and_then(|r| r.dbrow_get("locationID"))
            .and_then(PyRep::as_i64),
        Some(6)
    );

    // 4c. In-space active_ship_id transition (eject/board in space) stays unprojected.
    let space_cap_id = 992_600_034_i64;
    let space_ship_id = 992_600_035_i64;
    let space_cargo_id = 992_600_036_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = space_cap_id;
        snap.items.insert(
            space_cap_id,
            InventoryItemRecord {
                item_id: space_cap_id,
                type_id: 670,
                owner_id: pilot_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            space_ship_id,
            InventoryItemRecord {
                item_id: space_ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g2_in_space_board_ship", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = space_ship_id;
                tx.snap.remove_system_cleanup_item(space_cap_id);
                tx.snap.items.insert(
                    space_cargo_id,
                    InventoryItemRecord {
                        item_id: space_cargo_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: space_ship_id,
                        flag: InventoryFlag::Cargo,
                        quantity: 5,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("in-space board tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "in-space active_ship_id transition commit must hold back item projection"
    );
}

#[tokio::test]
async fn guardrail4a_in_tx_fitted_trashed_keeps_loc10_flag147_and_first_set_holds_hislot_charge() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, mod_id, charge_id, cargo_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        992_700_010_i64,
        992_700_020_i64,
        992_700_021_i64,
        992_700_022_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    // Explicitly verify compare OFF + deleted switch ON still suppresses conflicting tombstone.
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: false,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let mod_type_id = 482_u32; // turret/module
    let charge_rec = InventoryItemRecord {
        item_id: charge_id,
        type_id: 179,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::HiSlot0,
        quantity: 20,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    let cargo_rec = InventoryItemRecord {
        item_id: cargo_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 12,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_type_id,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(charge_id, charge_rec.clone());
        snap.items.insert(cargo_id, cargo_rec.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("g4a_destroy_fitted", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(mod_id);
                tx.snap.remove_system_cleanup_item(charge_id);
                tx.snap.remove_system_cleanup_item(cargo_id);
                ServerContext::push_tx_fitted_item_trashed(
                    tx.effects,
                    &ctx.sde,
                    pilot_id,
                    mod_id,
                    i64::from(mod_type_id),
                    1,
                    ship_id,
                    InventoryFlag::HiSlot0 as i64,
                );
                Ok(())
            })
            .expect("destroy fitted tx");
        // Deleted cargo item (`flag 5`, first set) consumes dedupe token; `HiSlot0` charge stays
        // held by First-set rule on commit and emits from hand-written helper.
        ctx.notify_item_mutation(pilot_id, Some(&cargo_rec), None);
        ctx.notify_item_mutation(pilot_id, Some(&charge_rec), None);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let committed = ctx.drain_notifications_for_char(pilot_id);
    let mod_notifs: Vec<_> = committed
        .iter()
        .filter(|n| n.event == "OnItemChange" && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(mod_id)))
        .collect();
    assert_eq!(mod_notifs.len(), 1, "fitted module must emit once with loc=10, flag=147");
    assert_eq!(mod_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(10)));
    assert_eq!(mod_notifs[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(147)));
    assert_eq!(mod_notifs[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(
        mod_notifs[0].args[1],
        PyRep::Dict(vec![
            (PyRep::Integer(3), PyRep::Integer(ship_id)),
            (PyRep::Integer(4), PyRep::Integer(InventoryFlag::HiSlot0 as i64)),
        ])
    );

    let cargo_notifs: Vec<_> = committed
        .iter()
        .filter(|n| n.event == "OnItemChange" && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(cargo_id)))
        .collect();
    assert_eq!(cargo_notifs.len(), 1, "first-set deleted Cargo item emits projected tombstone once");
    assert_eq!(cargo_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));

    let charge_notifs: Vec<_> = committed
        .iter()
        .filter(|n| n.event == "OnItemChange" && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(charge_id)))
        .collect();
    assert_eq!(charge_notifs.len(), 1, "held HiSlot0 charge still emitted once by hand-written helper");
    assert_eq!(charge_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));
    assert_eq!(item_compare_summary(&ctx).explicit_in_tx_effects, 0, "compare remains off");
}

#[tokio::test]
async fn dedupe_ledger_invalidates_stale_tokens_and_skips_offline_targets() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, item_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_800_010_i64, 992_800_020_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let item1 = InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        version: 1,
        ..Default::default()
    };
    let mut item2 = item1.clone();
    item2.quantity = 200;
    item2.version = 2;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(item_id, item1.clone());
        });
        // Commit #2 updates item2 after commit #1 without claiming commit #1 -> single fact now
        // records item2 (`item1` check is superseded, `item2` matches and persists across dispatch).
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(item_id, item2.clone());
        });
        assert!(
            ctx.try_consume_projected_item_mutation(pilot_id, &item1),
            "stale commit #1 row sees commit #2 fact with different row -> suppressed as superseded"
        );
        assert!(
            ctx.try_consume_projected_item_mutation(pilot_id, &item2),
            "latest commit #2 fact must match"
        );
        assert!(
            ctx.try_consume_projected_item_mutation(pilot_id, &item2),
            "dispatch fact persists for the entire dispatch and keeps suppressing matching calls"
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    // Once dispatch ends, dispatch facts are dropped so a later RPC without a commit sends (`NotSet`).
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        assert!(
            !ctx.try_consume_projected_item_mutation(pilot_id, &item2),
            "dispatch fact must be dropped when the previous dispatch ended"
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_mid = projected_item_switch_summary(&ctx);
    assert_eq!(sw_mid.replaced, 1, "sw={sw_mid:#?}");
    assert_eq!(sw_mid.superseded, 1, "sw={sw_mid:#?}");
    assert_eq!(sw_mid.by_scope.dispatch.would_announce, 2, "sw={sw_mid:#?}");
    assert_eq!(sw_mid.by_scope.dispatch.replaced, 2, "sw={sw_mid:#?}");
    assert_eq!(sw_mid.by_scope.dispatch.superseded, 1, "sw={sw_mid:#?}");
    assert_eq!(sw_mid.by_scope.dispatch.no_projected_event, 1, "sw={sw_mid:#?}");
    let two_commit_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(two_commit_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 2);
    assert_eq!(
        two_commit_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        2
    );

    // Offline character never emits or records tokens.
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db.release_online_lock(uid, 9_310);
    let mut item3 = item2;
    item3.quantity = 300;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&item_id).expect("item").quantity = 300;
        });
        assert!(!ctx.try_consume_projected_item_mutation(pilot_id, &item3));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    // Also ensure changed_chars / resolve_item_scene_in_view unit helper coverage for space scene.
    let space_ch = ctx.db.with_snapshot(|s| {
        let mut ch = s.characters[&pilot_id].clone();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch
    });
    let snap = ctx.db.with_snapshot(|s| s.clone());
    let skill_on_char = InventoryItemRecord {
        item_id: 992_800_099,
        type_id: 3400,
        owner_id: pilot_id,
        location_id: pilot_id,
        flag: InventoryFlag::Skill,
        quantity: 1,
        singleton: true,
        ..Default::default()
    };
    assert_eq!(
        resolve_item_scene_in_view(&skill_on_char, &space_ch, &snap, None, &ctx.sde),
        Some(ItemScene::Space { solar_system_id: sys_id })
    );
    let cargo_on_char = InventoryItemRecord {
        flag: InventoryFlag::Hangar,
        ..skill_on_char
    };
    assert_eq!(
        resolve_item_scene_in_view(&cargo_on_char, &space_ch, &snap, None, &ctx.sde),
        None
    );
}

#[tokio::test]
async fn first_set_rule_positive_and_all_hold_exclusions() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, other_ship_id, structure_id, cont_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        992_900_010_i64,
        992_900_011_i64,
        88_900_001_i64,
        992_900_012_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    let citadel_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 65)
        .map(|t| t.type_id)
        .unwrap_or(35832);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            other_ship_id,
            InventoryItemRecord {
                item_id: other_ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
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
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: citadel_type_id,
                owner_corp_id: 98_000_123,
                solar_system_id: sys_id,
                name: "FirstSet Test Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Non-dispatch commit (tick / neither) MUST HOLD even for station flag 4 hangar item!
    let tick_hangar_id = 992_900_020_i64;
    ctx.db
        .tx("outside_dispatch", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(
                tick_hangar_id,
                InventoryItemRecord {
                    item_id: tick_hangar_id,
                    type_id: 34,
                    owner_id: pilot_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("outside dispatch tx");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "non-dispatch commit must not project under First-set rule"
    );

    // 2. Inside dispatch while docked at station: hold fitted/drone/deliveries/container/other-ship/system/skill;
    //    project station flag 4 & active ship flag 5.
    let kept_hangar_id = 992_900_030_i64;
    let kept_cargo_id = 992_900_031_i64;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("first_set_station_mix", |tx| -> Result<(), eve_db::TxError> {
                let mut insert_at = |id: i64, loc: i64, flag: InventoryFlag| {
                    tx.snap.items.insert(
                        id,
                        InventoryItemRecord {
                            item_id: id,
                            type_id: 34,
                            owner_id: pilot_id,
                            location_id: loc,
                            flag,
                            quantity: 10,
                            version: 1,
                            ..Default::default()
                        },
                    );
                };
                insert_at(kept_hangar_id, station_id, InventoryFlag::Hangar);
                insert_at(kept_cargo_id, ship_id, InventoryFlag::Cargo);
                insert_at(992_900_032, ship_id, InventoryFlag::HiSlot0);
                insert_at(992_900_033, ship_id, InventoryFlag::RigSlot0);
                insert_at(992_900_034, ship_id, InventoryFlag::DroneBay);
                insert_at(992_900_035, station_id, InventoryFlag::Deliveries);
                insert_at(992_900_036, other_ship_id, InventoryFlag::Cargo);
                insert_at(992_900_037, cont_id, InventoryFlag::Cargo);
                insert_at(992_900_038, i64::from(sys_id), InventoryFlag::None);
                insert_at(992_900_039, pilot_id, InventoryFlag::Skill);
                Ok(())
            })
            .expect("mix tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let mut emitted_ids: Vec<i64> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args[0].dbrow_get("itemID").and_then(PyRep::as_i64))
        .collect();
    emitted_ids.sort_unstable();
    assert_eq!(
        emitted_ids,
        vec![kept_hangar_id, kept_cargo_id],
        "only station flag 4 and active ship flag 5 project"
    );

    // 3. Move out of first set (`flag 4 -> DroneBay 87`) or into first set (`DroneBay 87 -> flag 5`)
    //    must BOTH stay held because row was not in first set both before and after.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("move_cross_boundary", |tx| -> Result<(), eve_db::TxError> {
                let h = tx.snap.items.get_mut(&kept_hangar_id).expect("hangar");
                h.location_id = ship_id;
                h.flag = InventoryFlag::DroneBay;
                let d = tx.snap.items.get_mut(&992_900_034).expect("drone");
                d.flag = InventoryFlag::Cargo;
                Ok(())
            })
            .expect("cross boundary tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    // 4. Docked in structure (`structure_id`, `flag 4`) and in space (`Cargo`, `flag 5`).
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        snap.items.get_mut(&ship_id).expect("ship").location_id = structure_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let struct_item_id = 992_900_040_i64;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("struct_hangar", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    struct_item_id,
                    InventoryItemRecord {
                        item_id: struct_item_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: structure_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 8,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("struct hangar tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let struct_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(struct_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert_eq!(
        struct_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        1
    );

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.structure_id = None;
        snap.items.get_mut(&ship_id).expect("ship").location_id = i64::from(sys_id);
        snap.items.get_mut(&ship_id).expect("ship").flag = InventoryFlag::None;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("space_cargo_and_station_held", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.get_mut(&kept_cargo_id).expect("cargo").quantity = 99;
                // Structure hangar item while undocked in space is NO LONGER docked hangar -> held.
                tx.snap.items.get_mut(&struct_item_id).expect("struct").quantity = 99;
                Ok(())
            })
            .expect("space tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let space_emitted: Vec<i64> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args[0].dbrow_get("itemID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(space_emitted, vec![kept_cargo_id]);
}

#[tokio::test]
async fn fix1_tokens_expire_at_end_of_dispatch_and_never_dedupe_across_dispatches() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, item_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        992_910_010_i64,
        992_910_020_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: false,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    let before = InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 100,
        version: 1,
        ..Default::default()
    };
    let mut after = before.clone();
    after.quantity = 150;
    after.version = 2;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(item_id, before.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // Dispatch 1: projects update, leaves token unconsumed when dispatch ends.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("d1_update", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(item_id, after.clone());
                Ok(())
            })
            .expect("d1 tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let d1_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(d1_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert_eq!(
        d1_notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .count(),
        1
    );
    assert!(
        !ctx.try_consume_projected_item_mutation(pilot_id, &after),
        "token must already be expired outside Dispatch 1"
    );

    // Dispatch 2: helper emitting identical row must NOT be suppressed and logs `helper_without_token`.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.notify_item_mutation(pilot_id, Some(&before), Some(&after));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let d2_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        d2_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "expired token from Dispatch 1 must never suppress helper in Dispatch 2"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.emitted, 1);
    assert_eq!(sw.new, 1);
    assert_eq!(sw.replaced, 0);
    assert_eq!(sw.helper_without_token, 1);
    assert_eq!(sw.helper_without_token_with_projected_event, 0);
    assert_eq!(sw.helper_without_token_no_projected_event, 1);
}

#[tokio::test]
async fn fix2_no_silent_eviction_above_4096_and_tracks_evictions_zero() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_920_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: false,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    const COUNT: i64 = 4_500;
    let items: Vec<InventoryItemRecord> = (0..COUNT)
        .map(|i| InventoryItemRecord {
            item_id: 993_000_000 + i,
            type_id: 34,
            owner_id: pilot_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 10 + (i as i32 % 50),
            version: 1,
            ..Default::default()
        })
        .collect();

    let mut emitted_count = 0usize;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        // 50 items/commit * 2 notifications (OnItemChange + OnMultipleItemChange) = 100 <= MAX_PENDING_NOTIFICATIONS_PER_CHAR (128).
        for chunk in items.chunks(50) {
            ctx.db
                .tx("bulk_4500_insert_chunk", |tx| -> Result<(), eve_db::TxError> {
                    for rec in chunk {
                        tx.snap.items.insert(rec.item_id, rec.clone());
                    }
                    Ok(())
                })
                .expect("bulk chunk tx");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
            emitted_count += ctx
                .drain_notifications_for_char(pilot_id)
                .into_iter()
                .filter(|n| n.event == "OnItemChange")
                .count();
        }
        for rec in &items {
            assert!(
                ctx.try_consume_projected_item_mutation(pilot_id, rec),
                "no token (even item #0) may be evicted above 4096 within one dispatch: {}",
                rec.item_id
            );
        }
    })
    .await;

    assert_eq!(emitted_count, COUNT as usize);
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.evictions, 0);
    assert_eq!(sw.emitted, COUNT as u64);
    assert_eq!(sw.replaced, COUNT as u64);
    assert_eq!(sw.new, 0);
    assert_eq!(sw.helper_without_token, 0);
    assert_eq!(sw.helper_without_token_with_projected_event, 0);
    assert_eq!(sw.helper_without_token_no_projected_event, 0);
}

#[tokio::test]
async fn fix3_and_fix5_change_dict_comparison_and_switched_telemetry() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_930_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let same_del = InventoryItemRecord {
        item_id: 993_100_001,
        type_id: 34,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 20,
        version: 1,
        ..Default::default()
    };
    let diff_del = InventoryItemRecord {
        item_id: 993_100_002,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 15,
        version: 1,
        ..Default::default()
    };
    let new_only = InventoryItemRecord {
        item_id: 993_100_003,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 7,
        version: 1,
        ..Default::default()
    };
    let no_token_hangar = InventoryItemRecord {
        item_id: 993_100_004,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 9,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(same_del.item_id, same_del.clone());
        s.items.insert(diff_del.item_id, diff_del.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("fix3_fix5_tx", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(same_del.item_id);
                tx.snap.remove_system_cleanup_item(diff_del.item_id);
                tx.snap.items.insert(new_only.item_id, new_only.clone());
                Ok(())
            })
            .expect("tx");

        // 1. Tombstone helper has identical {3, 4, 9} change dict -> dictionary_same.
        ctx.emit_item_deleted_routed(pilot_id, &same_del);
        // 2. Helper omitting col 4/9 ({3: station_id} like TrashItems) -> dictionary_differs.
        assert!(
            ctx.try_consume_projected_item_deleted_with_changes(
                pilot_id,
                &diff_del,
                &[(3, PyRep::Integer(station_id))],
                "TrashItems_style",
            ),
            "TrashItems-style delete helper must consume commit token cleanly"
        );
        // 3. Helper without commit token on a first-set row -> helper_without_token.
        ctx.notify_item_mutation(pilot_id, None, Some(&no_token_hangar));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.evictions, 0);
    assert_eq!(sw.emitted, 3, "sw={sw:#?}");
    assert_eq!(sw.replaced, 2, "sw={sw:#?}");
    assert_eq!(sw.new, 1, "sw={sw:#?}");
    assert_eq!(sw.emitted, sw.replaced + sw.new);
    assert_eq!(sw.helper_without_token, 1, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_with_projected_event, 0, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_no_projected_event, 1, "sw={sw:#?}");
    assert_eq!(sw.dictionary_same, 1, "sw={sw:#?}");
    assert_eq!(sw.dictionary_differs, 1, "sw={sw:#?}");
    assert_eq!(sw.sample_dictionary_differs.len(), 1, "sw={sw:#?}");
    assert!(
        sw.sample_dictionary_differs[0].contains("first_set=true"),
        "switch dictionary_differs sample must carry first_set=true: {:?}",
        sw.sample_dictionary_differs
    );
    assert_eq!(sw.sample_helper_without_token_no_projected_event.len(), 1);
    assert!(sw.sample_helper_without_token_with_projected_event.is_empty());
    assert!(
        sw.by_class
            .contains_key("before=ship_or_container,after=recycling_loc_6,flag=0,before_flag=5,owner=character,singleton=false")
    );
    assert!(
        sw.by_helper
            .get("emit_item_deleted_routed")
            .is_some_and(|h| h.replaced == 1 && h.dictionary_same == 1 && h.dictionary_differs == 0)
    );
    assert!(
        sw.by_helper
            .get("TrashItems_style")
            .is_some_and(|h| h.replaced == 1 && h.dictionary_same == 0 && h.dictionary_differs == 1)
    );
    assert!(
        sw.by_helper
            .get("emit_item_change_routed")
            .is_some_and(|h| {
                h.helper_without_token == 1
                    && h.helper_without_token_with_projected_event == 0
                    && h.helper_without_token_no_projected_event == 1
            }),
        "by_helper={:#?}",
        sw.by_helper
    );
    let md = sw.format_markdown_report();
    assert!(md.contains("helper_without_token_no_projected_event=1"), "{md}");
    assert!(md.contains("first_set=true"), "{md}");
}

#[tokio::test]
async fn fix4_switches_off_build_item_tombstone_clears_is_blueprint_copy_qty_minus_2_to_0() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::disabled());
    let cfg = ctx.projected_item_switches();
    assert!(!cfg.char_same_scene_item_mutation);
    assert!(!cfg.char_same_scene_item_deleted);

    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_940_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type in sde");
    let bpc = InventoryItemRecord {
        item_id: 993_200_001,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: true,
        blueprint_runs_remaining: Some(4),
        version: 1,
        ..Default::default()
    };

    // With BOTH switches OFF, build_item_tombstone clears `is_blueprint_copy` so wire quantity
    // changes from `-2` (BPC) to `0` (recycled tombstone) cleanly.
    let (tomb, changes) = ServerContext::build_item_tombstone(&bpc);
    assert!(!tomb.is_blueprint_copy);
    assert!(!tomb.singleton);
    assert_eq!(tomb.location_id, 6);
    assert_eq!(tomb.flag, InventoryFlag::None);
    assert_eq!(tomb.quantity, 0);
    assert_eq!(
        crate::router::delta_projection::build_inv_item_dbrow(&bpc, &ctx.sde, "")
            .dbrow_get("quantity"),
        Some(&PyRep::Integer(-2))
    );
    assert_eq!(
        crate::router::delta_projection::build_inv_item_dbrow(&tomb, &ctx.sde, "")
            .dbrow_get("quantity"),
        Some(&PyRep::Integer(0))
    );
    assert_eq!(
        changes,
        vec![
            (3, PyRep::Integer(station_id)),
            (4, PyRep::Integer(InventoryFlag::Hangar as i64)),
            (9, PyRep::Integer(1)),
        ]
    );

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(bpc.item_id, bpc.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let mut shadow = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("fix4_bpc_delete_switches_off", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(bpc.item_id);
                Ok(())
            })
            .expect("delete tx");
        ctx.notify_item_mutation(pilot_id, Some(&bpc), None);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    let item_notif = notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("hand-written tombstone with switches off");
    assert_eq!(item_notif.args[0].dbrow_get("quantity"), Some(&PyRep::Integer(0)));
    assert_eq!(item_notif.args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));
    for n in &notifs {
        shadow.apply_notification(&n.event, &n.args);
    }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
}

#[tokio::test]
async fn check39_helper_without_token_splits_projected_intermediate_vs_no_projected_event() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_950_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let sell_item_id = 993_300_001_i64;
    let noop_item_id = 993_300_002_i64;
    let sell_start = InventoryItemRecord {
        item_id: sell_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1_000,
        version: 1,
        ..Default::default()
    };
    let noop_row = InventoryItemRecord {
        item_id: noop_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 50,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(sell_item_id, sell_start.clone());
        s.items.insert(noop_item_id, noop_row.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // Dispatch 1 (`marketProxy.PlaceMultiSellOrder`): one commit reduces stack 1000 -> 200;
    // helper still emits intermediate quantities (800, 600, 400), consumes final (200), and
    // another post-consumption emit (150) sees the item still had a projected event in this
    // dispatch even after its token was consumed.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("marketProxy", "PlaceMultiSellOrder"),
        );
        let mut final_after = sell_start.clone();
        final_after.quantity = 200;
        final_after.version = 2;
        ctx.db
            .tx("place_multi_sell_commit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(sell_item_id, final_after.clone());
                Ok(())
            })
            .expect("commit");

        for (prev_qty, next_qty) in [(1_000, 800), (800, 600), (600, 400)] {
            let mut step = sell_start.clone();
            step.quantity = next_qty;
            step.version = 2;
            ctx.emit_item_change_routed(pilot_id, &step, vec![(9, PyRep::Integer(prev_qty))]);
        }
        // Final state matches and consumes the commit token:
        ctx.notify_item_mutation(pilot_id, Some(&sell_start), Some(&final_after));
        // Even after token consumption, another un-tokenized emit for the same item in the same
        // dispatch classifies under `with_projected_event` (never `no_projected_event`).
        let mut stale_post = final_after.clone();
        stale_post.quantity = 150;
        ctx.emit_item_change_routed(pilot_id, &stale_post, vec![(9, PyRep::Integer(200))]);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let d1_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        d1_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "1 projected (qty=200) sends; 3 intermediate + 1 stale post helper calls are suppressed as superseded"
    );

    // Dispatch 2 (`invbroker.Add`): Dispatch 1 facts were dropped at dispatch end, so both
    // `noop_item_id` and `sell_item_id` have no set fact -> `no_projected_event` (both send).
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("invbroker", "Add"),
        );
        ctx.emit_item_change_routed(pilot_id, &noop_row, vec![(9, PyRep::Integer(40))]);
        // Dispatch 1's `sell_item_id` fact must not bleed into Dispatch 2:
        ctx.emit_item_change_routed(pilot_id, &sell_start, vec![(9, PyRep::Integer(900))]);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let d2_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        d2_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        2,
        "helper calls without a set fact in invbroker.Add must continue sending"
    );

    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.evictions, 0);
    assert_eq!(sw.emitted, 1, "sw={sw:#?}");
    assert_eq!(sw.replaced, 1, "sw={sw:#?}");
    assert_eq!(sw.superseded, 4, "sw={sw:#?}");
    assert_eq!(sw.new, 0, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token, 2, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_with_projected_event, 0, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_no_projected_event, 2, "sw={sw:#?}");
    assert_eq!(sw.by_scope.dispatch.would_announce, 1, "sw={sw:#?}");
    assert_eq!(sw.by_scope.dispatch.replaced, 1, "sw={sw:#?}");
    assert_eq!(sw.by_scope.dispatch.superseded, 4, "sw={sw:#?}");
    assert_eq!(sw.by_scope.dispatch.no_projected_event, 2, "sw={sw:#?}");

    let pms = sw
        .by_dispatch_method
        .get("marketProxy.PlaceMultiSellOrder")
        .expect("PlaceMultiSellOrder bucket");
    assert_eq!(pms.emitted, 1);
    assert_eq!(pms.replaced, 1);
    assert_eq!(pms.superseded, 4);
    assert_eq!(pms.helper_without_token, 0);

    let inv_add = sw
        .by_dispatch_method
        .get("invbroker.Add")
        .expect("invbroker.Add bucket");
    assert_eq!(inv_add.helper_without_token, 2);
    assert_eq!(inv_add.helper_without_token_with_projected_event, 0);
    assert_eq!(inv_add.helper_without_token_no_projected_event, 2);

    assert_eq!(
        sw.no_projected_event_rpcs(),
        BTreeMap::from([("invbroker.Add".to_string(), 2)])
    );
    assert_eq!(sw.sample_superseded.len(), 4);
    assert_eq!(sw.sample_helper_without_token_no_projected_event.len(), 2);
    assert!(sw
        .sample_helper_without_token_no_projected_event
        .iter()
        .all(|s| s.contains("had_projected_event=false")));

    let md = sw.format_markdown_report();
    assert!(md.contains("superseded=4"), "{md}");
    assert!(md.contains("helper_without_token_no_projected_event=2"), "{md}");
    assert!(md.contains("`invbroker.Add`"), "{md}");
}

#[tokio::test]
async fn r1_bf01_multi_commit_transition_dispatch_suppresses_commit2_item_projection() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_a_id, ship_b_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_960_010_i64, 992_960_011_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_a_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: true,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: true,
        corp_all_items: false,
    });

    let mut_item_id = 992_960_020_i64;
    let del_item_id = 992_960_021_i64;
    let mut_item = InventoryItemRecord {
        item_id: mut_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 200,
        version: 1,
        ..Default::default()
    };
    let del_item = InventoryItemRecord {
        item_id: del_item_id,
        type_id: 35,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 50,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            ship_b_id,
            InventoryItemRecord {
                item_id: ship_b_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(mut_item_id, mut_item);
        s.items.insert(del_item_id, del_item);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // Part A: In space (`station_id = None, structure_id = None`), Commit 1 changes `active_ship_id`
    // (`changes.items` empty); Commit 2 mutating the active ship itself (`ship_b_id == active_ship_id`)
    // remains suppressed (`0 OnItemChange`), while non-ship cargo item inserts (`space_cargo_id != active_ship_id`)
    // project normally under Track 5C Step 1 (`prior_transitioned_chars` active-ship scoping).
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_a_id;
        s.items.insert(
            ship_b_id,
            InventoryItemRecord {
                item_id: ship_b_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let space_cargo_id = 992_960_022_i64;
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("ship", "Board"),
        );
        ctx.db
            .tx("r1_bf01_commit1_space_ship_transition", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .active_ship_id = ship_b_id;
                Ok(())
            })
            .expect("commit 1");
        ctx.db
            .tx("r1_bf01_commit2_active_ship_only", |tx| -> Result<(), eve_db::TxError> {
                let ship = tx.snap.items.get_mut(&ship_b_id).expect("ship_b");
                ship.flag = InventoryFlag::Hangar;
                ship.version += 1;
                Ok(())
            })
            .expect("commit 2 active ship");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        0,
        "Commit 2 mutating the transitioned active ship itself must remain suppressed"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.emitted, 0, "sw={sw:#?}");
    assert_eq!(sw.replaced, 0, "sw={sw:#?}");
    assert_eq!(sw.new, 0, "sw={sw:#?}");

    // Follow-up in same dispatch style: Commit 2 inserting non-ship cargo item projects cleanly.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("ship", "Board"),
        );
        ctx.db
            .tx("r1_bf01_commit1b_space_ship_transition", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .solar_system_id = sys_id + 1;
                Ok(())
            })
            .expect("commit 1b");
        ctx.db
            .tx("r1_bf01_commit2b_space_cargo_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    space_cargo_id,
                    InventoryItemRecord {
                        item_id: space_cargo_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: ship_b_id,
                        flag: InventoryFlag::Cargo,
                        quantity: 42,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("commit 2b");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let cargo_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        cargo_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "Commit 2 non-ship cargo insert after transition must project normally: {cargo_notifs:?}"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot_id).expect("pilot").solar_system_id = sys_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // Part B: Docked in station (`station_id = Some(station_id)`), Commit 1 changes ONLY
    // `active_ship_id` (`ship_b_id -> ship_a_id`), Commit 2 mutates & deletes station hangar items
    // -> docked ship-switch is NOT a transition, so Commit 2 projects normally (`2 OnItemChange`s).
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.active_ship_id = ship_b_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("ship", "Board"),
        );
        ctx.db
            .tx("r1_bf01_docked_station_commit1_ship_only", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .active_ship_id = ship_a_id;
                Ok(())
            })
            .expect("docked station commit 1");
        ctx.db
            .tx("r1_bf01_docked_station_commit2_hangar_mutate_and_delete", |tx| -> Result<(), eve_db::TxError> {
                let row = tx.snap.items.get_mut(&mut_item_id).expect("mut_item");
                row.quantity = 120;
                row.version += 1;
                tx.snap.remove_system_cleanup_item(del_item_id);
                Ok(())
            })
            .expect("docked station commit 2");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let station_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        station_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        2,
        "Commit 2 after docked station active_ship_id-only switch must project normally: {station_notifs:?}"
    );

    // Part C: Docked in Upwell structure (`station_id = None, structure_id = Some(...)`),
    // Commit 1 changes ONLY `active_ship_id`, Commit 2 inserts a structure hangar item -> projects.
    let structure_id = 1_030_000_992_960_i64;
    let struct_item_id = 992_960_023_i64;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.active_ship_id = ship_a_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("ship", "Board"),
        );
        ctx.db
            .tx("r1_bf01_docked_struct_commit1_ship_only", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .active_ship_id = ship_b_id;
                Ok(())
            })
            .expect("docked struct commit 1");
        ctx.db
            .tx("r1_bf01_docked_struct_commit2_hangar_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    struct_item_id,
                    InventoryItemRecord {
                        item_id: struct_item_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: structure_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 10,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("docked struct commit 2");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let struct_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        struct_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "Commit 2 after docked structure active_ship_id-only switch must project normally: {struct_notifs:?}"
    );
}

#[tokio::test]
async fn r1_bf02_loc6_tombstone_via_emit_item_change_routed_and_notify_item_change_consumes_deleted_token() {
    for (config, use_notify_item_change) in [
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: false,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
                char_same_scene_item_fitted: false,
                char_all_items: false,
                corp_all_items: false,
            },
            false,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: true,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
                char_same_scene_item_fitted: false,
                char_all_items: false,
                corp_all_items: false,
            },
            false,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: false,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
                char_same_scene_item_fitted: false,
                char_all_items: false,
                corp_all_items: false,
            },
            true,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: true,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
                char_same_scene_item_fitted: false,
                char_all_items: false,
                corp_all_items: false,
            },
            true,
        ),
    ] {
        let ctx = test_ctx();
        let (pilot_id, station_id, sys_id, ship_id) =
            (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_970_010_i64);
        setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
        ctx.set_projected_item_switches(config);

        let item_id = 992_970_020_i64;
        let item = InventoryItemRecord {
            item_id,
            type_id: 34,
            owner_id: pilot_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 500,
            version: 1,
            ..Default::default()
        };
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(item_id, item.clone());
        });
        let _ = ctx.drain_notifications_for_char(pilot_id);

        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc_guard = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                normalize_dispatch_method_label("marketProxy", "PlaceMultiSellOrder"),
            );
            ctx.db
                .tx("r1_bf02_consume_full_stack", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.remove_system_cleanup_item(item_id);
                    Ok(())
                })
                .expect("delete stack");
            let (tomb, tomb_changes) = ServerContext::build_item_tombstone(&item);
            if use_notify_item_change {
                let cells = crate::services::inventory_bound::item_to_packed_cells(&ctx, &tomb);
                let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(cells);
                ctx.notify_item_change(pilot_id, row, tomb_changes);
            } else {
                ctx.emit_item_change_routed(pilot_id, &tomb, tomb_changes);
            }
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;

        let notifs = ctx.drain_notifications_for_char(pilot_id);
        let item_changes: Vec<_> = notifs.iter().filter(|n| n.event == "OnItemChange").collect();
        assert_eq!(
            item_changes.len(),
            1,
            "config={config:?} use_notify_item_change={use_notify_item_change}: expected 1 OnItemChange tombstone, got {}",
            item_changes.len()
        );
        assert_eq!(
            item_changes[0].args[0].dbrow_get("locationID"),
            Some(&PyRep::Integer(6))
        );
        if !use_notify_item_change {
            assert_eq!(
                notifs
                    .iter()
                    .filter(|n| n.event == "OnMultipleItemChange")
                    .count(),
                1
            );
        }
        let sw = projected_item_switch_summary(&ctx);
        assert_eq!(sw.emitted, 1, "sw={sw:#?}");
        assert_eq!(sw.replaced, 1, "sw={sw:#?}");
        assert_eq!(sw.new, 0, "sw={sw:#?}");
        assert_eq!(sw.helper_without_token, 0, "sw={sw:#?}");
        assert_eq!(sw.dictionary_same, 1, "sw={sw:#?}");
        assert_eq!(sw.dictionary_differs, 0, "sw={sw:#?}");
    }
}

#[tokio::test]
async fn r1_bf03_untokenized_deleted_helper_records_recycling_loc6_class_key() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_980_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: false,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });

    let old_item = InventoryItemRecord {
        item_id: 992_980_020_i64,
        type_id: 587,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(old_item.item_id, old_item.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("invbroker", "TrashItems"),
        );
        // No commit occurred for `old_item` in this dispatch -> un-tokenized `emit_item_deleted_routed`.
        ctx.emit_item_deleted_routed(pilot_id, &old_item);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);

    let expected_class =
        "before=station,after=recycling_loc_6,flag=0,before_flag=4,owner=character,singleton=false";
    let expected_crh = format!(
        "class={expected_class}|rpc=invbroker.TrashItems|helper=emit_item_deleted_routed"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.helper_without_token, 1, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_no_projected_event, 1, "sw={sw:#?}");
    assert_eq!(
        sw.by_class.get(expected_class).map(|c| c.helper_without_token),
        Some(1),
        "sw.by_class={:#?}",
        sw.by_class
    );
    assert_eq!(
        sw.by_class
            .get(expected_class)
            .map(|c| c.helper_without_token_no_projected_event),
        Some(1)
    );
    assert_eq!(
        sw.by_class_rpc_helper
            .get(&expected_crh)
            .map(|c| c.helper_without_token),
        Some(1),
        "sw.by_class_rpc_helper={:#?}",
        sw.by_class_rpc_helper
    );
}

#[tokio::test]
async fn t3c1_commit_companions_on_industry_materials_and_multi_item_without_helpers_and_no_double_emit() {
    let ctx = test_ctx();
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_990_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);

    let monitor_id = 77_940_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
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

    // 1. Pure commit insert with NO hand-written helper emits OnItemChange -> OnIndustryMaterials -> OnMultipleItemChange.
    // Use Nocxium (typeID 38) which has zero pre-existing demo stock in Hangar so delete cleanly yields 0.
    const TEST_MAT_TYPE: u32 = 38;
    let trit_id = 992_990_020_i64;
    let trit = InventoryItemRecord {
        item_id: trit_id,
        type_id: TEST_MAT_TYPE,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1500,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("t3c1_pure_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(trit_id, trit.clone());
                Ok(())
            })
            .expect("insert tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let insert_notifs = ctx.drain_notifications_for_char(pilot_id);
    let insert_events: Vec<&str> = insert_notifs.iter().map(|n| n.event.as_str()).collect();
    assert_eq!(
        insert_events,
        vec!["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]
    );
    assert_eq!(insert_notifs[1].args[0], PyRep::Integer(monitor_id));
    let has_trit_1500 = matches!(
        &insert_notifs[1].args[1],
        PyRep::Dict(entries) if entries.iter().any(|(k, v)| k == &PyRep::Integer(i64::from(TEST_MAT_TYPE)) && v == &PyRep::Integer(1500))
    );
    assert!(has_trit_1500, "OnIndustryMaterials args={:?}", insert_notifs[1].args);
    match &insert_notifs[2].args[0] {
        PyRep::List(items) if items.len() == 1 => {
            assert_eq!(items[0].dbrow_get("itemID"), Some(&PyRep::Integer(trit_id)));
            assert_eq!(items[0].dbrow_get("quantity"), Some(&PyRep::Integer(1500)));
        }
        other => panic!("expected 1-row OnMultipleItemChange list, got {other:?}"),
    }

    // 2. Commit update + emit_item_change_routed must suppress all 3 on helper path (no double-emit).
    let mut updated = trit.clone();
    updated.quantity = 500;
    updated.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("t3c1_update_routed", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(trit_id, updated.clone());
                Ok(())
            })
            .expect("update tx");
        ctx.emit_item_change_routed(pilot_id, &updated, vec![(9, PyRep::Integer(1500))]);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let update_notifs = ctx.drain_notifications_for_char(pilot_id);
    let update_events: Vec<&str> = update_notifs.iter().map(|n| n.event.as_str()).collect();
    assert_eq!(
        update_events,
        vec!["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]
    );
    let has_trit_500 = matches!(
        &update_notifs[1].args[1],
        PyRep::Dict(entries) if entries.iter().any(|(k, v)| k == &PyRep::Integer(i64::from(TEST_MAT_TYPE)) && v == &PyRep::Integer(500))
    );
    assert!(has_trit_500, "OnIndustryMaterials args={:?}", update_notifs[1].args);

    // 3a. Commit delete emits 0-quantity OnIndustryMaterials entry before OnMultipleItemChange,
    //     and emit_item_deleted_routed skips all 3 on the helper path (no double-emit).
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("t3c1_delete_trit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(trit_id);
                Ok(())
            })
            .expect("delete trit tx");
        ctx.emit_item_deleted_routed(pilot_id, &updated);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let del_trit_notifs = ctx.drain_notifications_for_char(pilot_id);
    let del_trit_events: Vec<&str> = del_trit_notifs.iter().map(|n| n.event.as_str()).collect();
    assert_eq!(
        del_trit_events,
        vec!["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]
    );
    let has_trit_0 = matches!(
        &del_trit_notifs[1].args[1],
        PyRep::Dict(entries) if entries.iter().any(|(k, v)| k == &PyRep::Integer(i64::from(TEST_MAT_TYPE)) && v == &PyRep::Integer(0))
    );
    assert!(has_trit_0, "expected 0 qty for consumed mat: {:?}", del_trit_notifs[1].args);

    // 3b. Blueprint deletion in ship Cargo alongside notify_item_mutation emits exactly
    //     OnItemChange -> OnMultipleItemChange -> OnBlueprintsUpdated without duplicate emits.
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let bp_id = 992_990_021_i64;
    let bp = InventoryItemRecord {
        item_id: bp_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(bp_id, bp.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("t3c1_delete_bp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(bp_id);
                Ok(())
            })
            .expect("delete bp tx");
        ctx.notify_item_mutation(pilot_id, Some(&bp), None);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let del_bp_notifs = ctx.drain_notifications_for_char(pilot_id);
    let del_bp_events: Vec<&str> = del_bp_notifs.iter().map(|n| n.event.as_str()).collect();
    assert_eq!(
        del_bp_events,
        vec!["OnItemChange", "OnMultipleItemChange", "OnBlueprintsUpdated"]
    );
}

#[tokio::test]
async fn t3c1_track_caller_telemetry_records_call_sites_across_wrappers_and_non_first_set() {
    use super::cache_notifications::NotificationTarget;

    let ctx = test_ctx();
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
    // This test specifically verifies pre-char_all_items/pre-corp_all_items non-first-set fallback.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_char_all_items(false)
            .with_corp_all_items(false),
    );
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_995_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let file_norm = file!().replace('\\', "/");
    let mk_hangar = |item_id: i64, qty: i32| InventoryItemRecord {
        item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: qty,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    let id_inv_mut = 992_995_101_i64;
    let id_inv_del = 992_995_102_i64;
    let id_mut_helper = 992_995_103_i64;
    let id_target_hit = 992_995_104_i64;
    let id_stale_routed = 992_995_105_i64;
    let id_no_proj_del = 992_995_106_i64;

    let item_inv_del = mk_hangar(id_inv_del, 10);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(id_inv_del, item_inv_del.clone());
    });

    let (
        site_inv_mut,
        site_inv_del,
        site_mut_helper,
        site_target_hit,
        site_with_proj_routed,
        site_no_proj_deleted,
        site_non_first_target,
        site_non_first_drone,
    ) = with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("invbroker", "MoveItems"),
        );

        let item_inv_mut = mk_hangar(id_inv_mut, 20);
        let item_mut_helper = mk_hangar(id_mut_helper, 30);
        let item_target_hit = mk_hangar(id_target_hit, 40);
        let item_stale_v1 = mk_hangar(id_stale_routed, 50);
        let item_stale_v2 = mk_hangar(id_stale_routed, 60);

        ctx.db
            .tx("t3c1_tc_commits", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(id_inv_mut, item_inv_mut.clone());
                tx.snap.remove_system_cleanup_item(id_inv_del);
                tx.snap.items.insert(id_mut_helper, item_mut_helper.clone());
                tx.snap.items.insert(id_target_hit, item_target_hit.clone());
                tx.snap.items.insert(id_stale_routed, item_stale_v1.clone());
                Ok(())
            })
            .expect("commit 1");
        ctx.db
            .tx("t3c1_tc_stale_v2", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(id_stale_routed, item_stale_v2.clone());
                Ok(())
            })
            .expect("commit 2");

        // Replaced via emit_item_change_routed (same routed path)
        let site_inv_mut = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_change_routed(pilot_id, &item_inv_mut, vec![(3, PyRep::Integer(0))]);

        // Replaced via emit_item_deleted_routed (same routed path)
        let site_inv_del = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_deleted_routed(pilot_id, &item_inv_del);

        // Replaced via notify_item_mutation
        let site_mut_helper = format!("{file_norm}:{}", line!() + 1);
        ctx.notify_item_mutation(pilot_id, None, Some(&item_mut_helper));

        // Replaced via emit_item_change_with_target
        let site_target_hit = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_change_with_target(NotificationTarget::Character(pilot_id), "charid", &item_target_hit, vec![(3, PyRep::Integer(0))]);

        // superseded via emit_item_change_routed (stale qty=50 sees v2 qty=60 fact -> not sent)
        let site_with_proj_routed = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_change_routed(pilot_id, &item_stale_v1, vec![(3, PyRep::Integer(0))]);

        // no_projected_event via emit_item_deleted_routed (no commit for id_no_proj_del)
        let item_no_proj_del = mk_hangar(id_no_proj_del, 5);
        let site_no_proj_deleted = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_deleted_routed(pilot_id, &item_no_proj_del);

        // non_first_set_emitted via emit_item_change_with_target (Corporation target)
        let pilot_corp = ctx.db.with_snapshot(|s| s.characters[&pilot_id].corporation_id);
        let corp_item = InventoryItemRecord {
            item_id: 992_995_107_i64,
            type_id: 34,
            owner_id: pilot_corp,
            location_id: station_id,
            flag: InventoryFlag::CorpSAG1,
            quantity: 100,
            ..Default::default()
        };
        let site_non_first_target = format!("{file_norm}:{}", line!() + 1);
        ctx.emit_item_change_with_target(NotificationTarget::Corporation(pilot_corp), "corpid", &corp_item, vec![(3, PyRep::Integer(0))]);

        // non_first_set_emitted via notify_item_mutation (non-first-set DroneBay flag=87)
        let drone_item = InventoryItemRecord {
            item_id: 992_995_108_i64,
            type_id: 2454,
            owner_id: pilot_id,
            location_id: ship_id,
            flag: InventoryFlag::DroneBay,
            quantity: 5,
            ..Default::default()
        };
        let site_non_first_drone = format!("{file_norm}:{}", line!() + 1);
        ctx.notify_item_mutation(pilot_id, None, Some(&drone_item));

        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        (
            site_inv_mut,
            site_inv_del,
            site_mut_helper,
            site_target_hit,
            site_with_proj_routed,
            site_no_proj_deleted,
            site_non_first_target,
            site_non_first_drone,
        )
    })
    .await;

    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.replaced, 4, "sw={sw:#?}");
    assert_eq!(sw.superseded, 1, "sw={sw:#?}");
    assert_eq!(sw.non_first_set_emitted, 2, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_with_projected_event, 0, "sw={sw:#?}");
    assert_eq!(sw.helper_without_token_no_projected_event, 1, "sw={sw:#?}");

    for site in [
        &site_inv_mut,
        &site_inv_del,
        &site_mut_helper,
        &site_target_hit,
    ] {
        assert_eq!(
            sw.by_call_site.get(site),
            Some(&SwitchCallSiteCounts {
                replaced: 1,
                superseded: 0,
                non_first_set_emitted: 0,
                helper_without_token_with_projected_event: 0,
                helper_without_token_no_projected_event: 0,
            }),
            "site={site} by_call_site={:#?}",
            sw.by_call_site
        );
    }
    assert_eq!(
        sw.by_call_site.get(&site_with_proj_routed),
        Some(&SwitchCallSiteCounts {
            replaced: 0,
            superseded: 1,
            non_first_set_emitted: 0,
            helper_without_token_with_projected_event: 0,
            helper_without_token_no_projected_event: 0,
        })
    );
    assert_eq!(
        sw.by_call_site.get(&site_no_proj_deleted),
        Some(&SwitchCallSiteCounts {
            replaced: 0,
            superseded: 0,
            non_first_set_emitted: 0,
            helper_without_token_with_projected_event: 0,
            helper_without_token_no_projected_event: 1,
        })
    );
    for site in [&site_non_first_target, &site_non_first_drone] {
        assert_eq!(
            sw.by_call_site.get(site),
            Some(&SwitchCallSiteCounts {
                replaced: 0,
                superseded: 0,
                non_first_set_emitted: 1,
                helper_without_token_with_projected_event: 0,
                helper_without_token_no_projected_event: 0,
            }),
            "site={site} by_call_site={:#?}",
            sw.by_call_site
        );
    }

    for k in sw.by_call_site.keys() {
        assert!(
            k.contains("delta_item_switch_tests.rs:"),
            "call site must propagate through wrappers to caller, got {k}"
        );
    }

    let md = sw.format_markdown_report();
    assert!(md.contains("| Call Site |"), "{md}");
    assert!(md.contains(&site_inv_mut), "{md}");
    assert!(md.contains(&site_non_first_drone), "{md}");

    let json = serde_json::to_value(&sw).expect("serialize summary");
    assert_eq!(json["non_first_set_emitted"], 2);
    assert_eq!(json["superseded"], 1);
    assert_eq!(json["by_call_site"][&site_inv_mut]["replaced"], 1);
    assert_eq!(json["by_call_site"][&site_with_proj_routed]["superseded"], 1);
    assert_eq!(json["by_call_site"][&site_no_proj_deleted]["helper_without_token_no_projected_event"], 1);
    assert_eq!(json["by_call_site"][&site_non_first_target]["non_first_set_emitted"], 1);
}

#[tokio::test]
async fn retired_kill_switches_env_off_still_emit_on_item_change_for_deleted_sites() {
    struct EnvReset(Vec<(&'static str, Option<String>)>);
    impl Drop for EnvReset {
        fn drop(&mut self) {
            for (k, prev) in &self.0 {
                match prev {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
    use crate::log_ring_buffer::{LogRingBuffer, LogRingLayer};
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt;

    let ring = std::sync::Arc::new(LogRingBuffer::new(64));
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(Targets::new().with_target("eve_server", tracing::Level::WARN))
            .with(LogRingLayer::new(std::sync::Arc::clone(&ring))),
    );
    let ctx = {
        let _log_guard = tracing::dispatcher::set_default(&dispatch);
        let _env_guard = crate::lock_test_env_write();
        let keys = [
            "EVOMU_PROJECTED_ITEM_MUTATION",
            "EVOMU_PROJECTED_ITEM_DELETED",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED",
            "EVOMU_SWITCH_CHAR_ALL_ITEMS",
            "EVOMU_SWITCH_CORP_ALL_ITEMS",
        ];
        let _reset = EnvReset(keys.iter().map(|&k| (k, std::env::var(k).ok())).collect());
        for k in keys {
            std::env::set_var(k, "0");
        }
        ServerContext::new()
    };
    let startup_warnings: Vec<String> = ring
        .snapshot_recent(64)
        .into_iter()
        .filter(|line| line.contains("Ignoring retired item projection kill-switch env var(s)"))
        .collect();
    assert_eq!(
        startup_warnings.len(),
        1,
        "expected exactly one startup WARN log for retired kill switches, got: {startup_warnings:?}"
    );
    assert!(
        startup_warnings[0].contains("EVOMU_PROJECTED_ITEM_MUTATION")
            && startup_warnings[0].contains("EVOMU_PROJECTED_ITEM_DELETED")
            && startup_warnings[0].contains("EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC")
            && startup_warnings[0].contains("EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED")
            && startup_warnings[0].contains("EVOMU_SWITCH_CHAR_ALL_ITEMS")
            && startup_warnings[0].contains("EVOMU_SWITCH_CORP_ALL_ITEMS"),
        "expected startup WARN log to list all 6 retired env vars, got: {:?}",
        startup_warnings[0]
    );
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
    assert!(ctx.projected_item_switches().char_same_scene_item_outside_rpc);
    assert!(ctx.projected_item_switches().char_same_scene_item_fitted);
    assert!(ctx.projected_item_switches().char_all_items);
    assert!(ctx.projected_item_switches().corp_all_items);
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_900_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    let mut sess = eve_machonet::SessionState {
        user_id: Some(uid),
        character_id: Some(pilot_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id: Some(station_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let skills: Vec<u32> = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 16
                && t.published
                && eve_dogma::REQUIRED_SKILL_ATTR_IDS
                    .iter()
                    .all(|a| !t.default_attributes.contains_key(a))
                && !ctx.db.with_snapshot(|s| {
                    s.skills.get(&pilot_id).is_some_and(|m| m.contains_key(&t.type_id))
                })
        })
        .take(2)
        .map(|t| t.type_id)
        .collect();
    assert_eq!(skills.len(), 2, "expected 2 uninjected skills in SDE");
    let (hangar_book_id, cargo_book_id) = (992_900_021_i64, 992_900_022_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, loc, flag) in [
            (hangar_book_id, skills[0], station_id, InventoryFlag::Hangar),
            (cargo_book_id, skills[1], ship_id, InventoryFlag::Cargo),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id, type_id: tid, owner_id: pilot_id, location_id: loc,
                flag, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        }
        snap.adjust_plex_vault(pilot_id, 50);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let kw = std::collections::HashMap::new();
    for (svc, meth, args, label) in [
        ("skillMgr2", "InjectSkillIntoBrain", vec![PyRep::Integer(hangar_book_id)], "hangar skill"),
        ("skillMgr2", "InjectSkillIntoBrain", vec![PyRep::Integer(cargo_book_id)], "cargo skill"),
        ("invbroker", "WithdrawPlexFromVault", vec![PyRep::Integer(10), PyRep::Integer(station_id)], "plex withdraw"),
    ] {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let res = match svc {
                "skillMgr2" => crate::services::character_skills::handle(&ctx, &mut sess, svc, meth, &args, &kw),
                _ => crate::services::wallet_ledger::handle(&ctx, &mut sess, svc, meth, &args, &kw),
            };
            res.expect("handler matched").expect(label);
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
        let on_item = ctx.drain_notifications_for_char(pilot_id).into_iter().filter(|n| n.event == "OnItemChange").count();
        assert_eq!(on_item, 1, "{label} must still emit 1 OnItemChange even with retired env kill switches=0");
    }

    // Outside-RPC mutations must also still emit OnItemChange when all retired env vars are 0.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            992_900_023_i64,
            InventoryItemRecord {
                item_id: 992_900_023_i64,
                type_id: 34,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let outside_on_item = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .count();
    assert_eq!(
        outside_on_item, 1,
        "outside-RPC commit must still emit OnItemChange when EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC=0"
    );
}

#[test]
fn character_scene_or_ship_transition_predicate_covers_all_edges() {
    use eve_db::CharacterRecord;
    use std::collections::HashMap;

    let cid = 90_000_001_i64;
    let base = CharacterRecord {
        character_id: cid,
        user_id: 1,
        solar_system_id: 30_000_142,
        station_id: Some(60_003_760),
        structure_id: None,
        active_ship_id: 100_001,
        ..Default::default()
    };

    // Absent from changed_chars -> false.
    let empty: HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)> = HashMap::new();
    assert!(!is_character_scene_or_ship_transition(cid, &empty));

    // Character created (None -> Some) or deleted (Some -> None) -> true.
    let created = HashMap::from([(cid, (None, Some(&base)))]);
    assert!(is_character_scene_or_ship_transition(cid, &created));
    let deleted = HashMap::from([(cid, (Some(&base), None))]);
    assert!(is_character_scene_or_ship_transition(cid, &deleted));

    // Unchanged record -> false.
    let same = base.clone();
    assert!(!is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&base), Some(&same)))])
    ));

    // solar_system_id changed -> true.
    let mut diff_sys = base.clone();
    diff_sys.solar_system_id = 30_000_144;
    assert!(is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&base), Some(&diff_sys)))])
    ));

    // station_id changed -> true.
    let mut diff_station = base.clone();
    diff_station.station_id = Some(60_003_761);
    assert!(is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&base), Some(&diff_station)))])
    ));

    // structure_id changed -> true (undocked/docked in structure or structure A -> B).
    let mut in_struct = base.clone();
    in_struct.station_id = None;
    in_struct.structure_id = Some(1_030_000_001);
    assert!(is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&base), Some(&in_struct)))])
    ));
    let mut diff_struct = in_struct.clone();
    diff_struct.structure_id = Some(1_030_000_002);
    assert!(is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&in_struct), Some(&diff_struct)))])
    ));

    // Docked in station: active_ship_id-only change -> false.
    let mut docked_station_ship_swap = base.clone();
    docked_station_ship_swap.active_ship_id = 100_002;
    assert!(!is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&base), Some(&docked_station_ship_swap)))])
    ));

    // Docked in structure: active_ship_id-only change -> false.
    let mut docked_struct_ship_swap = in_struct.clone();
    docked_struct_ship_swap.active_ship_id = 100_002;
    assert!(!is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&in_struct), Some(&docked_struct_ship_swap)))])
    ));

    // In space (`station_id == None && structure_id == None`): active_ship_id change -> true.
    let mut in_space = base.clone();
    in_space.station_id = None;
    in_space.structure_id = None;
    let mut in_space_ship_swap = in_space.clone();
    in_space_ship_swap.active_ship_id = 100_002;
    assert!(is_character_scene_or_ship_transition(
        cid,
        &HashMap::from([(cid, (Some(&in_space), Some(&in_space_ship_swap)))])
    ));
}

#[tokio::test]
async fn step1_fitted_switch_projects_fit_unfit_swap_charges_autoload_strip_and_destroy_when_enabled()
{
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_990_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);

    let mod_a_id = 992_990_021_i64;
    let mod_b_id = 992_990_022_i64;
    let mod_c_id = 992_990_023_i64;
    let rig_a_id = 992_990_024_i64;
    let charge_a_id = 992_990_025_i64;
    let charge_c_id = 992_990_026_i64;
    let mod_type_id = 487_u32;
    let rig_type_id = 31716_u32;
    let charge_type_id = 179_u32;

    let mut mod_a = InventoryItemRecord {
        item_id: mod_a_id,
        type_id: mod_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let mut charge_a = InventoryItemRecord {
        item_id: charge_a_id,
        type_id: charge_type_id,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 200,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_a_id, mod_a.clone());
        snap.items.insert(charge_a_id, charge_a.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Kill-switch off (`char_same_scene_item_fitted = false`, pre-`char_all_items`): fit / slot-swap / charge / unfit hold on commit
    //    and hand-written `notify_item_mutation` still emits.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled()
            .with_char_same_scene_item_fitted(false)
            .with_char_all_items(false),
    );
    assert!(!ctx.projected_item_switches().char_same_scene_item_fitted);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        mod_a.location_id = ship_id;
        mod_a.flag = InventoryFlag::HiSlot0;
        mod_a.version = 2;
        ctx.db
            .tx("fitted_off_fit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(mod_a_id, mod_a.clone());
                Ok(())
            })
            .expect("fitted_off_fit");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "fitted=false must keep Hangar -> HiSlot0 commit held on commit"
    );

    // Move back to hangar before testing `char_same_scene_item_fitted = true`.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let mod_prev = mod_a.clone();
        mod_a.location_id = station_id;
        mod_a.flag = InventoryFlag::Hangar;
        mod_a.version = 3;
        ctx.db
            .tx("fitted_off_unfit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(mod_a_id, mod_a.clone());
                Ok(())
            })
            .expect("fitted_off_unfit");
        ctx.notify_item_mutation(pilot_id, Some(&mod_prev), Some(&mod_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "fitted=false lets hand-written fitted unfit emit normally"
    );

    // 2. `char_same_scene_item_fitted = true`: fit, slot swap, charge load, autoload supersede (200 -> 199),
    //    unfit, StripFitting (OnItemsChanged + 0 duplicate OnItemChange), and DestroyFitting.
    ctx.set_projected_item_switches(
        ProjectedItemSwitchConfig::enabled().with_char_same_scene_item_fitted(true),
    );
    assert!(ctx.projected_item_switches().char_same_scene_item_fitted);

    // 2a. Fit (`Hangar -> HiSlot0`) with duplicate hand-written helper consumed.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let mod_prev = mod_a.clone();
        mod_a.location_id = ship_id;
        mod_a.flag = InventoryFlag::HiSlot0;
        mod_a.version += 1;
        ctx.db
            .tx("fitted_on_fit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(mod_a_id, mod_a.clone());
                Ok(())
            })
            .expect("fitted_on_fit");
        ctx.notify_item_mutation(pilot_id, Some(&mod_prev), Some(&mod_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let fit_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(fit_notifs.len(), 1, "fit emits once cleanly: {fit_notifs:?}");
    assert_eq!(
        fit_notifs[0].args[0].dbrow_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::HiSlot0 as i64))
    );

    // 2b. Slot-to-slot swap (`HiSlot0 -> HiSlot1`) projects + consumes hand-written helper.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let mod_prev = mod_a.clone();
        mod_a.flag = InventoryFlag::HiSlot1;
        mod_a.version += 1;
        ctx.db
            .tx("fitted_on_swap", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(mod_a_id, mod_a.clone());
                Ok(())
            })
            .expect("fitted_on_swap");
        ctx.notify_item_mutation(pilot_id, Some(&mod_prev), Some(&mod_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let swap_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(swap_notifs.len(), 1, "slot swap emits once: {swap_notifs:?}");
    assert_eq!(
        swap_notifs[0].args[0].dbrow_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::HiSlot1 as i64))
    );
    assert_eq!(
        swap_notifs[0].args[1],
        PyRep::Dict(vec![(
            PyRep::Integer(4),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64)
        )])
    );

    // 2c. Charge load (`Cargo -> HiSlot1`, qty=200) and immediate autoload-consume (`200 -> 199`)
    //     where intermediate autoload `qty=200` emit is superseded and final `qty=199` is replaced.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let charge_pre = charge_a.clone();
        charge_a.location_id = ship_id;
        charge_a.flag = InventoryFlag::HiSlot1;
        charge_a.version += 1;
        ctx.db
            .tx("fitted_charge_load", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(charge_a_id, charge_a.clone());
                Ok(())
            })
            .expect("fitted_charge_load");
        ctx.notify_item_mutation(pilot_id, Some(&charge_pre), Some(&charge_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .into_iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1,
        "charge load must project once"
    );

    let superseded_before = projected_item_switch_summary(&ctx).superseded;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let charge_200 = charge_a.clone();
        let mut charge_autoload_before = charge_a.clone();
        charge_autoload_before.flag = InventoryFlag::Cargo;
        charge_a.quantity = 199;
        charge_a.version += 1;
        ctx.db
            .tx("fitted_autoload_consume_199", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(charge_a_id, charge_a.clone());
                Ok(())
            })
            .expect("fitted_autoload_consume_199");
        // Intermediate hand-written autoload (qty=200) followed by final consume (qty=199):
        ctx.notify_item_mutation(pilot_id, Some(&charge_autoload_before), Some(&charge_200));
        ctx.notify_item_mutation(pilot_id, Some(&charge_200), Some(&charge_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let autoload_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        autoload_notifs.len(),
        1,
        "intermediate qty=200 autoload must be superseded cleanly: {autoload_notifs:?}"
    );
    assert_eq!(
        autoload_notifs[0].args[0].dbrow_get("quantity"),
        Some(&PyRep::Integer(199))
    );
    assert!(
        projected_item_switch_summary(&ctx).superseded > superseded_before,
        "intermediate qty=200 helper must increment superseded counter"
    );

    // 2d. Unfit (`HiSlot1 -> Hangar`) projects once cleanly.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let mod_prev = mod_a.clone();
        mod_a.location_id = station_id;
        mod_a.flag = InventoryFlag::Hangar;
        mod_a.version += 1;
        ctx.db
            .tx("fitted_on_unfit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(mod_a_id, mod_a.clone());
                Ok(())
            })
            .expect("fitted_on_unfit");
        ctx.notify_item_mutation(pilot_id, Some(&mod_prev), Some(&mod_a));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .into_iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        1
    );

    // 2e. `StripFitting` emits `OnItemsChanged` (canonical DBRows, location = None) via `tx.effects`,
    //     zero server-pushed `OnModuleUnfitted`, and zero duplicate singular `OnItemChange` even with `fitted = true`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_b_id,
            InventoryItemRecord {
                item_id: mod_b_id,
                type_id: mod_type_id,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            rig_a_id,
            InventoryItemRecord {
                item_id: rig_a_id,
                type_id: rig_type_id,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let mut sess = eve_machonet::SessionState {
        user_id: Some(uid),
        character_id: Some(pilot_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id: Some(station_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = std::collections::HashMap::new();
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        crate::services::character_skill_ops::handle(
            &ctx,
            &mut sess,
            "dogmaIM",
            "StripFitting",
            &[PyRep::Integer(ship_id)],
            &kw,
        )
        .expect("StripFitting routed")
        .expect("StripFitting ok");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let strip_notifs = ctx.drain_notifications_for_char(pilot_id);
    let items_changed: Vec<_> = strip_notifs
        .iter()
        .filter(|n| n.event == "OnItemsChanged")
        .collect();
    assert!(
        !items_changed.is_empty(),
        "StripFitting must keep notify:OnItemsChanged: {strip_notifs:?}"
    );
    for n in &items_changed {
        assert_eq!(n.args.get(2), Some(&PyRep::None));
        assert!(matches!(
            n.args.first(),
            Some(PyRep::List(rows)) if !rows.is_empty() && rows.iter().all(|r| matches!(r, PyRep::DBRow { .. }))
        ));
    }
    assert!(
        strip_notifs.iter().all(|n| n.event != "OnModuleUnfitted"),
        "StripFitting must not push server-side OnModuleUnfitted: {strip_notifs:?}"
    );
    assert!(
        strip_notifs.iter().all(|n| n.event != "OnItemChange"),
        "StripFitting tx.effects OnItemsChanged must suppress duplicate singular OnItemChange: {strip_notifs:?}"
    );

    // 2f. `DestroyFitting` explicit `push_tx_fitted_item_trashed` keeps `loc=10, flag=147` for the
    //     destroyed module while projecting fitted `charge_c` deletion once cleanly.
    let charge_c = InventoryItemRecord {
        item_id: charge_c_id,
        type_id: charge_type_id,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::HiSlot0,
        quantity: 10,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_c_id,
            InventoryItemRecord {
                item_id: mod_c_id,
                type_id: mod_type_id,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(charge_c_id, charge_c.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("fitted_destroy_g4", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(mod_c_id);
                tx.snap.remove_system_cleanup_item(charge_c_id);
                ServerContext::push_tx_fitted_item_trashed(
                    tx.effects,
                    &ctx.sde,
                    pilot_id,
                    mod_c_id,
                    i64::from(mod_type_id),
                    1,
                    ship_id,
                    InventoryFlag::HiSlot0 as i64,
                );
                Ok(())
            })
            .expect("fitted_destroy_g4");
        ctx.notify_item_mutation(pilot_id, Some(&charge_c), None);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let destroy_notifs = ctx.drain_notifications_for_char(pilot_id);
    let mod_c_notifs: Vec<_> = destroy_notifs
        .iter()
        .filter(|n| n.event == "OnItemChange" && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(mod_c_id)))
        .collect();
    assert_eq!(mod_c_notifs.len(), 1);
    assert_eq!(mod_c_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(10)));
    assert_eq!(mod_c_notifs[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(147)));
    let charge_c_notifs: Vec<_> = destroy_notifs
        .iter()
        .filter(|n| n.event == "OnItemChange" && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(charge_c_id)))
        .collect();
    assert_eq!(
        charge_c_notifs.len(),
        1,
        "fitted charge deletion must project once and consume hand-written helper: {charge_c_notifs:?}"
    );
    assert_eq!(charge_c_notifs[0].args[0].dbrow_get("locationID"), Some(&PyRep::Integer(6)));
}

#[test]
fn large_outside_rpc_commit_preserves_item_change_and_industry_materials_under_queue_cap() {
    use super::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR;

    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled().with_char_all_items(false));
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_996_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);

    const MAT_TYPE: u32 = 38;
    let monitor_id = 77_996_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
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

    let base_mat_qty: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.owner_id == pilot_id
                    && it.location_id == station_id
                    && it.flag == InventoryFlag::Hangar
                    && it.type_id == MAT_TYPE
            })
            .map(|it| i64::from(it.quantity))
            .sum()
    });

    let item_count = MAX_PENDING_NOTIFICATIONS_PER_CHAR + 24;
    let base_item_id = 992_996_100_i64;
    let per_item_qty = 10_i32;
    ctx.db
        .tx("large_outside_rpc_commit_pairing", |tx| -> Result<(), eve_db::TxError> {
            for idx in 0..item_count {
                let item_id = base_item_id + idx as i64;
                tx.snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: MAT_TYPE,
                        owner_id: pilot_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: per_item_qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            Ok(())
        })
        .expect("large outside-RPC commit");

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(notifs.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);

    let item_change_count = notifs.iter().filter(|n| n.event == "OnItemChange").count();
    assert!(
        item_change_count >= MAX_PENDING_NOTIFICATIONS_PER_CHAR / 2 - 1,
        "expected >= {} OnItemChange notifications under pairing, got {item_change_count}",
        MAX_PENDING_NOTIFICATIONS_PER_CHAR / 2 - 1,
    );

    let ind_pos = notifs
        .iter()
        .position(|n| n.event == "OnIndustryMaterials")
        .expect("coalesced OnIndustryMaterials must survive large commit under queue cap");
    assert_eq!(notifs[ind_pos].args[0], PyRep::Integer(monitor_id));
    let expected_total = base_mat_qty + (item_count as i64) * i64::from(per_item_qty);
    let has_total = matches!(
        &notifs[ind_pos].args[1],
        PyRep::Dict(entries)
            if entries.iter().any(|(k, v)| {
                k == &PyRep::Integer(i64::from(MAT_TYPE)) && v == &PyRep::Integer(expected_total)
            })
    );
    assert!(
        has_total,
        "expected total {expected_total} for type {MAT_TYPE} in {:?}",
        notifs[ind_pos].args
    );

    let last_multi_pos = notifs
        .iter()
        .rposition(|n| n.event == "OnMultipleItemChange")
        .expect("final OnMultipleItemChange");
    assert!(
        ind_pos < last_multi_pos,
        "OnIndustryMaterials ({ind_pos}) must precede final OnMultipleItemChange ({last_multi_pos})"
    );
    assert!(ind_pos > 0, "expected OnItemChange immediately before coalesced OnIndustryMaterials");
    assert!(
        notifs[ind_pos].seq_id > notifs[ind_pos - 1].seq_id
            && notifs[ind_pos].seq_id < notifs[last_multi_pos].seq_id,
        "coalesced OnIndustryMaterials seq_id ({}) must sit strictly between preceding ({}) and trailing OnMultipleItemChange ({})",
        notifs[ind_pos].seq_id,
        notifs[ind_pos - 1].seq_id,
        notifs[last_multi_pos].seq_id
    );
}

#[tokio::test]
async fn track5b_s1_char_all_items_specialized_holds_locked_drone_implant_remote_and_off_gate() {
    let ctx = test_ctx();
    let (pilot_id, station_id, other_station, sys_id, active_ship, other_ship, cont_id) = (
        90_000_001_i64,
        60_003_760_i64,
        60_003_761_i64,
        30_000_142_u32,
        993_910_010_i64,
        993_910_011_i64,
        993_910_012_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, active_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            other_ship,
            InventoryItemRecord {
                item_id: other_ship,
                type_id: 587,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 17_366,
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

    let fuel_id = 993_910_021_i64;
    let ore_id = 993_910_022_i64;
    let drone_id = 993_910_023_i64;
    let implant_id = 993_910_024_i64;
    let booster_id = 993_910_025_i64;
    let remote_id = 993_910_026_i64;
    let inactive_ship_cargo_id = 993_910_027_i64;
    let container_item_id = 993_910_028_i64;
    let locked_id = 993_910_029_i64;

    let drone_type = 2454_u32;
    let implant_type = *ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 20)
        .map(|t| &t.type_id)
        .unwrap_or(&9899);

    let mk_item = |id: i64, type_id: u32, loc: i64, flag: InventoryFlag, qty: i32, singleton: bool| {
        InventoryItemRecord {
            item_id: id,
            type_id,
            owner_id: pilot_id,
            location_id: loc,
            flag,
            quantity: qty,
            singleton,
            version: 1,
            ..Default::default()
        }
    };

    // Part A: With `char_all_items == false` (kill-switch override), none of these generalize -> commit emits 0,
    // hand-written emit succeeds untouched.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_char_all_items(false));
    let off_fuel = mk_item(fuel_id, 16_274, active_ship, InventoryFlag::SpecializedFuelBay, 100, false);
    let off_implant = mk_item(implant_id, implant_type, pilot_id, InventoryFlag::Implant, 1, true);
    let off_remote = mk_item(remote_id, 34, other_station, InventoryFlag::Hangar, 50, false);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("char_all_off_non_slot_insert", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(fuel_id, off_fuel.clone());
                tx.snap.items.insert(implant_id, off_implant.clone());
                tx.snap.items.insert(remote_id, off_remote.clone());
                Ok(())
            })
            .expect("tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "default char_all_items=false must leave specialized/implant/remote commits silent"
    );

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.notify_item_mutation(pilot_id, None, Some(&off_fuel));
        ctx.notify_item_mutation(pilot_id, None, Some(&off_implant));
        ctx.notify_item_mutation(pilot_id, None, Some(&off_remote));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .count(),
        3,
        "default char_all_items=false preserves legacy hand-written OnItemChange for generalized locs/flags"
    );
    let sum_off = projected_item_switch_summary(&ctx);
    assert!(
        sum_off.not_covered_by_reason.contains_key("flag is not 4 or 5")
            || sum_off
                .not_covered_by_reason
                .contains_key("location is not the docked hangar or the active ship"),
        "expected default char_all_items=false not_covered_by_reason: {:?}",
        sum_off.not_covered_by_reason
    );

    // Part B: With `char_all_items == true`, ALL 5 categories project on commit and dedupe helpers.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let mut shadow = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));

    let items_to_insert = vec![
        mk_item(ore_id, 1230, active_ship, InventoryFlag::SpecializedOreHold, 250, false),
        mk_item(drone_id, drone_type, active_ship, InventoryFlag::DroneBay, 1, true),
        mk_item(booster_id, 28_672, pilot_id, InventoryFlag::Booster, 1, true),
        mk_item(inactive_ship_cargo_id, 34, other_ship, InventoryFlag::Cargo, 40, false),
        mk_item(container_item_id, 35, cont_id, InventoryFlag::None, 80, false),
        mk_item(locked_id, 36, station_id, InventoryFlag::Locked, 15, false),
    ];
    let mut fuel_v2 = off_fuel.clone();
    fuel_v2.flag = InventoryFlag::SpecializedFuelBay;
    fuel_v2.quantity = 200;
    fuel_v2.version = 2;
    let mut remote_v2 = off_remote.clone();
    remote_v2.quantity = 120;
    remote_v2.version = 2;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("char_all_on_bulk_create_and_update", |tx| -> Result<(), eve_db::TxError> {
                for rec in &items_to_insert {
                    tx.snap.items.insert(rec.item_id, rec.clone());
                }
                tx.snap.items.insert(fuel_id, fuel_v2.clone());
                tx.snap.items.insert(remote_id, remote_v2.clone());
                Ok(())
            })
            .expect("tx");
        for rec in &items_to_insert {
            ctx.notify_item_mutation(pilot_id, None, Some(rec));
        }
        ctx.notify_item_mutation(pilot_id, Some(&off_fuel), Some(&fuel_v2));
        ctx.notify_item_mutation(pilot_id, Some(&off_remote), Some(&remote_v2));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        items_to_insert.len() + 2,
        "expected exactly 1 projected OnItemChange per generalized item and 0 duplicates"
    );
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnMultipleItemChange").count(),
        items_to_insert.len() + 2
    );
    for n in &notifs {
        shadow.apply_notification(&n.event, &n.args);
    }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // Scenario 3 continuation: launch drone from DroneBay -> space, then scoop back to DroneBay.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.solar_system_id = sys_id;
    });
    let mut shadow_space = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));
    let drone_in_bay = items_to_insert[1].clone();
    let mut drone_in_space = drone_in_bay.clone();
    drone_in_space.location_id = i64::from(sys_id);
    drone_in_space.flag = InventoryFlag::None;
    drone_in_space.version += 1;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("launch_drone_in_space", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(drone_id, drone_in_space.clone());
                Ok(())
            })
            .expect("launch drone tx");
        ctx.notify_item_mutation(pilot_id, Some(&drone_in_bay), Some(&drone_in_space));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let launch_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(launch_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    for n in &launch_notifs {
        shadow_space.apply_notification(&n.event, &n.args);
    }

    let mut drone_returned = drone_in_space.clone();
    drone_returned.location_id = active_ship;
    drone_returned.flag = InventoryFlag::DroneBay;
    drone_returned.version += 1;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("scoop_drone_to_bay", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(drone_id, drone_returned.clone());
                Ok(())
            })
            .expect("scoop drone tx");
        ctx.notify_item_mutation(pilot_id, Some(&drone_in_space), Some(&drone_returned));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let scoop_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(scoop_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    for n in &scoop_notifs {
        shadow_space.apply_notification(&n.event, &n.args);
    }
    shadow_space.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
}

#[tokio::test]
async fn track5b_s1_blueprints_created_in_place_locked_dedupe_exclusions_and_compare() {
    use crate::services::industry_pi::{ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE};

    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, corp_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_920_010_i64,
        98_000_001_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .entry(corp_id)
            .or_insert_with(|| CorporationRecord::new(corp_id, "Tel Corp", "TEL", pilot_id));
        snap.characters.get_mut(&pilot_id).expect("pilot").corporation_id = corp_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));
    let _ = flush_item_compare_summary(&ctx);

    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let bpo_id = 993_920_021_i64;
    let bpo_v1 = InventoryItemRecord {
        item_id: bpo_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Locked,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: false,
        blueprint_runs_remaining: Some(-1),
        version: 1,
        ..Default::default()
    };

    // 1. Blueprint created in Locked flag: commit emits OnItemChange + OnMultipleItemChange +
    //    OnBlueprintsUpdated(pilot_id), dedupes notify_item_mutation AND direct OnBlueprintsUpdated,
    //    and leaves compare blueprints_updated.{projected_only, hand_written_only} == 0.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("bp_locked_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bpo_id, bpo_v1.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(pilot_id, None, Some(&bpo_v1));
        ctx.notify(
            super::NotificationTarget::Character(pilot_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(pilot_id)],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let create_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(create_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    let bp_creates: Vec<_> = create_notifs
        .iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(bp_creates.len(), 1, "expected single projected+deduped OnBlueprintsUpdated: {bp_creates:?}");
    assert_eq!(bp_creates[0].id_type, "ownerID");
    assert_eq!(bp_creates[0].args, vec![PyRep::Integer(pilot_id)]);

    // 2. In-place ME/TE edit (`dynamic_attributes` only, empty item-row diff): commit STILL emits
    //    OnBlueprintsUpdated(pilot_id) and dedupes notify_item_mutation without emitting OnItemChange.
    let mut bpo_v2 = bpo_v1.clone();
    bpo_v2.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 10.0);
    bpo_v2.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 20.0);
    bpo_v2.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("bp_in_place_me_te_edit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bpo_id, bpo_v2.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(pilot_id, Some(&bpo_v1), Some(&bpo_v2));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let edit_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(edit_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 0);
    let bp_edits: Vec<_> = edit_notifs
        .iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(bp_edits.len(), 1, "in-place ME/TE edit must project exactly 1 OnBlueprintsUpdated");
    assert_eq!(bp_edits[0].args, vec![PyRep::Integer(pilot_id)]);

    let cmp = flush_item_compare_summary(&ctx);
    assert_eq!(cmp.blueprints_updated.projected_only, 0);
    assert_eq!(cmp.blueprints_updated.hand_written_only, 0);

    // 2b. Corp-owner OnBlueprintsUpdated addressed to pilot (owner_id != char_id) must NOT be swallowed.
    ctx.notify(
        super::NotificationTarget::Character(pilot_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(corp_id)],
    );
    let corp_bp_passthrough: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(corp_bp_passthrough.len(), 1);
    assert_eq!(corp_bp_passthrough[0].args, vec![PyRep::Integer(corp_id)]);

    // 3. Explicit Effects OnBlueprintsUpdated suppresses commit-time projection and invalidates
    //    earlier dispatch blueprint dedupe facts.
    let mut bpo_v3 = bpo_v2.clone();
    bpo_v3.blueprint_runs_remaining = Some(4);
    bpo_v3.is_blueprint_copy = true;
    bpo_v3.version = 3;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("bp_seed_dispatch_fact", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bpo_id, bpo_v3.clone());
                Ok(())
            })
            .expect("tx1");
        let mut bpo_v4 = bpo_v3.clone();
        bpo_v4.blueprint_runs_remaining = Some(3);
        bpo_v4.version = 4;
        ctx.db
            .tx("bp_explicit_effect_override", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bpo_id, bpo_v4);
                tx.effects.notify_character(
                    pilot_id,
                    "OnBlueprintsUpdated",
                    "ownerID",
                    vec![PyRep::Integer(pilot_id)],
                );
                Ok(())
            })
            .expect("tx2");
        // Subsequent hand-written OnBlueprintsUpdated in this scope must NOT be wrongly suppressed
        // after explicit-effect commit invalidated the earlier commit's blueprint fact.
        ctx.notify(
            super::NotificationTarget::Character(pilot_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(pilot_id)],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let exp_bp_notifs: Vec<_> = ctx
        .drain_notifications_for_char(pilot_id)
        .into_iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(
        exp_bp_notifs.len(),
        3,
        "tx1 commit (1) + tx2 explicit effect (1) + post-invalidation hand-written (1)"
    );

    // 4. Pilot scene transition suppresses blueprint projection and invalidates dispatch bp facts.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let mut bpo_v5 = bpo_v3.clone();
        bpo_v5.version = 5;
        ctx.db
            .tx("bp_transition_suppression", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").solar_system_id = sys_id + 1;
                tx.snap.items.insert(bpo_id, bpo_v5);
                Ok(())
            })
            .expect("tx");
        ctx.notify(
            super::NotificationTarget::Character(pilot_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(pilot_id)],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(pilot_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1,
        "transition commit stays silent and leaves hand-written OnBlueprintsUpdated intact"
    );
}

#[tokio::test]
async fn track5b_s1_cross_owner_char_transfer_both_recipients_and_outgoing_dedupe() {
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, sys_id, ship1, ship2, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_930_010_i64,
        993_930_011_i64,
        98_000_001_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let xfer_item_id = 993_930_021_i64;
    let xfer_bpo_id = 993_930_022_i64;
    let corp_to_char_id = 993_930_023_i64;

    let item_p1 = InventoryItemRecord {
        item_id: xfer_item_id,
        type_id: 34,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 75,
        singleton: false,
        version: 1,
        ..Default::default()
    };
    let bpo_p1 = InventoryItemRecord {
        item_id: xfer_bpo_id,
        type_id: bp_type_id,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Locked,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let corp_item_pre = InventoryItemRecord {
        item_id: corp_to_char_id,
        type_id: 35,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 25,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(xfer_item_id, item_p1.clone());
        snap.items.insert(xfer_bpo_id, bpo_p1.clone());
        snap.items.insert(corp_to_char_id, corp_item_pre.clone());
        snap.corporations
            .entry(corp_id)
            .or_insert_with(|| CorporationRecord::new(corp_id, "Test Corp", "TST", p1_id));
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = corp_id;
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    // Part A: With `char_all_items = false` (kill-switch override), cross-owner transfer emits 0 on commit,
    // and hand-written notify_item_mutation delivers to both P2 and P1.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_char_all_items(false));
    let mut item_p2_step1 = item_p1.clone();
    item_p2_step1.owner_id = p2_id;
    item_p2_step1.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("off_cross_owner_transfer", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(xfer_item_id, item_p2_step1.clone());
                Ok(())
            })
            .expect("tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let p1_off = ctx.drain_notifications_for_char(p1_id);
    assert!(p1_off.is_empty(), "expected empty p1 notifs when char_all_items=false, got: {p1_off:?}");
    let p2_off = ctx.drain_notifications_for_char(p2_id);
    assert!(p2_off.is_empty(), "expected empty p2 notifs when char_all_items=false, got: {p2_off:?}");

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.notify_item_mutation(p2_id, Some(&item_p1), Some(&item_p2_step1));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(ctx.drain_notifications_for_char(p1_id).iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert_eq!(ctx.drain_notifications_for_char(p2_id).iter().filter(|n| n.event == "OnItemChange").count(), 1);

    // Part B: With `char_all_items = true` (`corp_all_items = false`), cross-owner transfer P2 -> P1
    // (and BPO P1 -> P2) projects to BOTH recipients, dedupes notify_item_mutation &
    // OnMultipleItemChange & OnBlueprintsUpdated, and converges both ShadowClients cleanly!
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_corp_all_items(false));
    let mut shadow_p1 = ShadowClient::from_world(p1_id, &ctx.db, Some(&ctx.sde));
    let mut shadow_p2 = ShadowClient::from_world(p2_id, &ctx.db, Some(&ctx.sde));

    let mut item_back_p1 = item_p2_step1.clone();
    item_back_p1.owner_id = p1_id;
    item_back_p1.version = 3;
    let mut bpo_p2 = bpo_p1.clone();
    bpo_p2.owner_id = p2_id;
    bpo_p2.flag = InventoryFlag::Hangar;
    bpo_p2.version = 2;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("on_cross_owner_transfers", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(xfer_item_id, item_back_p1.clone());
                tx.snap.items.insert(xfer_bpo_id, bpo_p2.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(p1_id, Some(&item_p2_step1), Some(&item_back_p1));
        ctx.notify_item_mutation(p2_id, Some(&bpo_p1), Some(&bpo_p2));
        // Direct hand-written OnMultipleItemChange to outgoing owner P2 carrying ixOwnerID=P2:
        let multi_row = super::delta_projection::build_inv_item_dbrow(&item_back_p1, &ctx.sde, "");
        ctx.notify(
            super::NotificationTarget::Character(p2_id),
            "OnMultipleItemChange",
            "charid",
            vec![
                PyRep::List(vec![multi_row]),
                PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Integer(p2_id))]),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let n_p1 = ctx.drain_notifications_for_char(p1_id);
    let n_p2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(n_p1.iter().filter(|n| n.event == "OnItemChange").count(), 2);
    assert_eq!(n_p2.iter().filter(|n| n.event == "OnItemChange").count(), 2);
    assert_eq!(n_p1.iter().filter(|n| n.event == "OnMultipleItemChange").count(), 2);
    assert_eq!(n_p2.iter().filter(|n| n.event == "OnMultipleItemChange").count(), 2);
    assert_eq!(n_p1.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);
    assert_eq!(n_p2.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);

    for n in &n_p1 {
        shadow_p1.apply_notification(&n.event, &n.args);
    }
    for n in &n_p2 {
        shadow_p2.apply_notification(&n.event, &n.args);
    }
    shadow_p1.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
    shadow_p2.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // Mixed Corp -> Char transfer: char target P2 is projected & deduped, Corp C retains legacy emit.
    let mut corp_to_char_post = corp_item_pre.clone();
    corp_to_char_post.owner_id = p2_id;
    corp_to_char_post.flag = InventoryFlag::Hangar;
    corp_to_char_post.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("corp_to_char_mixed_transfer", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(corp_to_char_id, corp_to_char_post.clone());
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(p2_id, Some(&corp_item_pre), Some(&corp_to_char_post));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let n_corp_actor = ctx.drain_notifications_for_char(p1_id);
    let n_char_recv = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(
        n_char_recv.iter().filter(|n| n.event == "OnItemChange").count(),
        1,
        "char target P2 must receive exactly commit OnItemChange and skip hand-written duplicate"
    );
    assert!(
        n_corp_actor
            .iter()
            .any(|n| n.event == "OnItemChange" && n.id_type == "corpid"),
        "corporation side of mixed transfer must retain legacy hand-written corpid notification"
    );
}

#[tokio::test]
async fn track5b_s1_char_all_items_telemetry_and_20k_pending_batch_cap() {
    use super::cache_notifications::{
        assemble_pending_notification, enqueue_or_stage_pending_batch,
        MAX_PENDING_NOTIFICATIONS_PER_CHAR,
    };

    let ctx = test_ctx();
    let (pilot_id, offline_id, station_id, sys_id, ship_id, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_940_010_i64,
        98_000_001_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .entry(corp_id)
            .or_insert_with(|| CorporationRecord::new(corp_id, "Tel Corp", "TEL", pilot_id));
        snap.characters.get_mut(&pilot_id).expect("pilot").corporation_id = corp_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());

    let report_on = projected_item_switch_summary(&ctx).format_markdown_report();
    assert!(
        report_on.contains("char_all_items=true") && report_on.contains("corp_all_items=true"),
        "report should expose char_all_items=true, corp_all_items=true: {report_on}"
    );
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_corp_all_items(false));

    // Offline non-hangar item must report OwnerOffline (never FlagNot4Or5 / LocationNotDockedHangarOrActiveShip).
    let off_item = InventoryItemRecord {
        item_id: 993_940_021,
        type_id: 34,
        owner_id: offline_id,
        location_id: ship_id,
        flag: InventoryFlag::SpecializedFuelBay,
        quantity: 10,
        ..Default::default()
    };
    let corp_item = InventoryItemRecord {
        item_id: 993_940_022,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 10,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.notify_item_mutation(offline_id, None, Some(&off_item));
        ctx.notify_item_mutation(pilot_id, None, Some(&corp_item));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sum = projected_item_switch_summary(&ctx);
    assert!(sum.not_covered_by_reason.contains_key("owner offline"));
    assert!(sum.not_covered_by_reason.contains_key("not character-owned"));
    assert!(
        !sum.not_covered_by_reason.contains_key("flag is not 4 or 5")
            && !sum
                .not_covered_by_reason
                .contains_key("location is not the docked hangar or the active ship"),
        "char_all_items=true must never blame flag/location: {:?}",
        sum.not_covered_by_reason
    );

    // 20,000-item pending batch sanity check: linear-time cap retention + exact trailing order.
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let batch: Vec<_> = (0..20_000_i64)
        .map(|i| {
            assemble_pending_notification(
                pilot_id,
                Some(1),
                "OnItemChange",
                "charid",
                vec![PyRep::Integer(i)],
                i,
            )
        })
        .collect();
    enqueue_or_stage_pending_batch(&*ctx.pending_notifications, &ctx.notify_tx, &batch);
    let queued = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(queued.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    let expected_first = 20_000_i64 - (MAX_PENDING_NOTIFICATIONS_PER_CHAR as i64);
    assert_eq!(queued[0].args, vec![PyRep::Integer(expected_first)]);
    assert_eq!(
        queued[MAX_PENDING_NOTIFICATIONS_PER_CHAR - 1].args,
        vec![PyRep::Integer(19_999)]
    );
}

#[tokio::test]
async fn bf01_dispatch_covered_industry_monitors_invalidated_across_commits_and_transitions() {
    use crate::services::notify_emitters_character_services::emit_industry_materials_for_scopes;

    const TEST_MAT_TYPE: u32 = 38;
    for char_all_items in [false, true] {
        let ctx = test_ctx();
        let (pilot_id, station_id, sys_id, ship_id) =
            (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_945_010_i64);
        setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
        ctx.set_projected_item_switches(
            ProjectedItemSwitchConfig::enabled().with_char_all_items(char_all_items),
        );

        let monitor_id = 77_945_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
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

        let item_a_id = 993_945_021_i64;
        let item_b_id = 993_945_022_i64;
        let item_a_v1 = InventoryItemRecord {
            item_id: item_a_id,
            type_id: TEST_MAT_TYPE,
            owner_id: pilot_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 1000,
            singleton: false,
            version: 1,
            ..Default::default()
        };

        // 1. Multi-commit invalidation inside one dispatch (Commit 1 projected -> Commit 2
        //    non-projected explicit effect touching the same monitored type_id -> manual
        //    `emit_industry_materials_for_scopes` must emit refreshed quantity 400).
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("bf01_commit1_projected", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(item_a_id, item_a_v1.clone());
                    Ok(())
                })
                .expect("commit 1");
            // Calling helper right after Commit 1 is suppressed while still covered.
            emit_industry_materials_for_scopes(
                &ctx,
                &[(pilot_id, station_id, InventoryFlag::Hangar as i64, TEST_MAT_TYPE)],
            );

            let mut item_a_v2 = item_a_v1.clone();
            item_a_v2.quantity = 400;
            item_a_v2.version = 2;
            ctx.db
                .tx("bf01_commit2_explicit_effect", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(item_a_id, item_a_v2.clone());
                    let row = super::delta_projection::build_inv_item_dbrow(&item_a_v2, &ctx.sde, "");
                    tx.effects.notify_character(
                        pilot_id,
                        "OnItemChange",
                        "charid",
                        vec![row, PyRep::Dict(vec![(PyRep::Integer(9), PyRep::Integer(1000))])],
                    );
                    Ok(())
                })
                .expect("commit 2");
            // Commit 2 invalidated Commit 1's monitor coverage so Commit 2's refresh MUST emit 400.
            emit_industry_materials_for_scopes(
                &ctx,
                &[(pilot_id, station_id, InventoryFlag::Hangar as i64, TEST_MAT_TYPE)],
            );
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;

        let mats_1: Vec<i64> = ctx
            .drain_notifications_for_char(pilot_id)
            .into_iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .filter_map(|n| match &n.args[1] {
                PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
                    (k == &PyRep::Integer(i64::from(TEST_MAT_TYPE))).then(|| v.as_i64()).flatten()
                }),
                _ => None,
            })
            .collect();
        assert_eq!(
            mats_1,
            vec![1000, 400],
            "char_all_items={char_all_items}: expected Commit 1 (1000), suppressed mid-emit, and Commit 2 refreshed (400)"
        );

        // 2. Multi-item batch invalidation + transition invalidation inside one dispatch:
        //    Commit 1 (2 items -> batch branch) covers monitor; Commit 2 transitions pilot without
        //    even emitting item_specs (`transitioned_chars`) and invalidates monitor coverage!
        let mut item_a_v3 = item_a_v1.clone();
        item_a_v3.quantity = 600;
        item_a_v3.version = 3;
        let item_b = InventoryItemRecord {
            item_id: item_b_id,
            type_id: TEST_MAT_TYPE,
            owner_id: pilot_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity: 250,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("bf01_batch_commit1_projected", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(item_a_id, item_a_v3);
                    tx.snap.items.insert(item_b_id, item_b);
                    Ok(())
                })
                .expect("batch commit 1");
            // Commit 2 moves pilot to another solar system (no item deltas -> `bucket.facts` unchanged
            // except transition clears monitor coverage for `pilot_id`).
            ctx.db
                .tx("bf01_commit2_transition", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.characters.get_mut(&pilot_id).expect("pilot").solar_system_id =
                        sys_id + 1;
                    Ok(())
                })
                .expect("transition commit 2");
            emit_industry_materials_for_scopes(
                &ctx,
                &[(pilot_id, station_id, InventoryFlag::Hangar as i64, TEST_MAT_TYPE)],
            );
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;

        let mats_2: Vec<i64> = ctx
            .drain_notifications_for_char(pilot_id)
            .into_iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .filter_map(|n| match &n.args[1] {
                PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
                    (k == &PyRep::Integer(i64::from(TEST_MAT_TYPE))).then(|| v.as_i64()).flatten()
                }),
                _ => None,
            })
            .collect();
        assert_eq!(
            mats_2,
            vec![850, 850],
            "char_all_items={char_all_items}: transition commit 2 must invalidate dispatch monitor coverage"
        );
    }
}

#[tokio::test]
async fn singleton_bpo_flag_change_in_monitored_hangar_emits_zero_on_industry_materials() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_948_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    assert!(ctx.projected_item_switches().char_all_items);

    let monitor_id = 77_948_001_i64;
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let bp_id = 993_948_021_i64;
    let mat_id = 993_948_022_i64;
    const MAT_TYPE: u32 = 38;

    let bp_hangar = InventoryItemRecord {
        item_id: bp_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: false,
        blueprint_runs_remaining: Some(-1),
        version: 1,
        ..Default::default()
    };
    let mat_hangar = InventoryItemRecord {
        item_id: mat_id,
        type_id: MAT_TYPE,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 500,
        singleton: false,
        version: 1,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
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
        snap.items.insert(bp_id, bp_hangar.clone());
        snap.items.insert(mat_id, mat_hangar.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Locking singleton BPO (Hangar -> Locked) inside monitored hangar must emit 0 OnIndustryMaterials.
    let mut bp_locked = bp_hangar.clone();
    bp_locked.flag = InventoryFlag::Locked;
    bp_locked.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("lock_singleton_bpo", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bp_id, bp_locked.clone());
                Ok(())
            })
            .expect("lock singleton bpo tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let lock_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        lock_notifs.iter().filter(|n| n.event == "OnIndustryMaterials").count(),
        0,
        "locking a singleton BPO in a monitored hangar must emit 0 OnIndustryMaterials: {lock_notifs:?}"
    );
    assert_eq!(
        lock_notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        1
    );
    assert_eq!(
        lock_notifs.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(),
        1
    );

    // 2. Unlocking singleton BPO alongside consuming non-singleton material emits OnIndustryMaterials
    //    only for the non-singleton material (never injecting {bp_type_id: 0}).
    let mut bp_unlocked = bp_locked.clone();
    bp_unlocked.flag = InventoryFlag::Hangar;
    bp_unlocked.version = 3;
    let mut mat_after = mat_hangar.clone();
    mat_after.quantity = 300;
    mat_after.version = 2;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("unlock_bpo_and_consume_mat", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bp_id, bp_unlocked.clone());
                tx.snap.items.insert(mat_id, mat_after.clone());
                Ok(())
            })
            .expect("unlock+consume tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let unlock_notifs = ctx.drain_notifications_for_char(pilot_id);
    let ind_notifs: Vec<_> = unlock_notifs
        .iter()
        .filter(|n| n.event == "OnIndustryMaterials")
        .collect();
    assert_eq!(ind_notifs.len(), 1, "ind_notifs={ind_notifs:?}");
    let PyRep::Dict(entries) = &ind_notifs[0].args[1] else {
        panic!("expected Dict in OnIndustryMaterials: {:?}", ind_notifs[0].args);
    };
    assert!(
        !entries.iter().any(|(k, _)| k.as_i64() == Some(i64::from(bp_type_id))),
        "singleton BPO typeID {bp_type_id} must never appear in OnIndustryMaterials: {entries:?}"
    );
    assert!(
        entries.iter().any(|(k, v)| k.as_i64() == Some(i64::from(MAT_TYPE)) && v.as_i64() == Some(300)),
        "consumed material {MAT_TYPE} must reflect updated qty 300: {entries:?}"
    );
}

#[tokio::test]
async fn track5c_s1_corp_all_items_off_default_and_on_projects_corp_hangar_upwell_and_npc() {
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, structure_id, sys_id, ship1, ship2, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        1_030_000_995_001_i64,
        30_000_142_u32,
        993_950_010_i64,
        993_950_011_i64,
        98_000_501_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "Track5C Corp", "T5C", p1_id);
        corp.members.insert(p1_id);
        corp.members.insert(p2_id);
        snap.corporations.insert(corp_id, corp);
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = corp_id;
        snap.characters.get_mut(&p2_id).expect("p2").corporation_id = corp_id;
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    // 1. Default `corp_all_items = true`, and explicit test override `corp_all_items = false`:
    //    when overridden OFF, corp item commits project 0 notifications and preserve legacy `"corpid"` behavior.
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
    assert!(ctx.projected_item_switches().corp_all_items);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_corp_all_items(false));
    let off_item_id = 993_950_101_i64;
    let off_item = InventoryItemRecord {
        item_id: off_item_id,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 25,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_off_corp_create", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(off_item_id, off_item.clone());
                Ok(())
            })
            .expect("off corp tx");
        ctx.notify_item_mutation(p1_id, None, Some(&off_item));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(projected_item_switch_summary(&ctx).emitted, 0);
    assert_eq!(projected_item_switch_summary(&ctx).replaced, 0);
    for cid in [p1_id, p2_id] {
        let notifs = ctx.drain_notifications_for_char(cid);
        assert_eq!(
            notifs
                .iter()
                .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
                .count(),
            1,
            "corp_all_items=false must emit legacy corpid OnItemChange for {cid}: {notifs:?}"
        );
    }

    // 2. Default-ON `corp_all_items = true`: CorpSAG1, CorpDeliveries, Locked, and Upwell structure
    //    HiSlot0 + StructureFuel mutations emit `id_type = "corpid"` + OnMultipleItemChange to BOTH
    //    online corp members and dedupe legacy helpers cleanly.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());
    let (sag_id, deliv_id, locked_id, upwell_hi_id, upwell_fuel_id) = (
        993_950_102_i64,
        993_950_103_i64,
        993_950_104_i64,
        993_950_105_i64,
        993_950_106_i64,
    );
    let sag_rec = InventoryItemRecord {
        item_id: sag_id,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 100,
        version: 1,
        ..Default::default()
    };
    let deliv_rec = InventoryItemRecord {
        item_id: deliv_id,
        type_id: 35,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpDeliveries,
        quantity: 40,
        version: 1,
        ..Default::default()
    };
    let locked_rec = InventoryItemRecord {
        item_id: locked_id,
        type_id: 36,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::Locked,
        quantity: 12,
        version: 1,
        ..Default::default()
    };
    let upwell_hi_rec = InventoryItemRecord {
        item_id: upwell_hi_id,
        type_id: 27_148,
        owner_id: corp_id,
        location_id: structure_id,
        flag: InventoryFlag::HiSlot0,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let upwell_fuel_rec = InventoryItemRecord {
        item_id: upwell_fuel_id,
        type_id: 16_274,
        owner_id: corp_id,
        location_id: structure_id,
        flag: InventoryFlag::StructureFuel,
        quantity: 500,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_on_corp_and_upwell", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(sag_id, sag_rec.clone());
                tx.snap.items.insert(deliv_id, deliv_rec.clone());
                tx.snap.items.insert(locked_id, locked_rec.clone());
                tx.snap.items.insert(upwell_hi_id, upwell_hi_rec.clone());
                tx.snap.items.insert(upwell_fuel_id, upwell_fuel_rec.clone());
                Ok(())
            })
            .expect("on corp + upwell tx");
        // Exercise legacy emitters (single + routed + target + direct notify + multi-row):
        ctx.notify_item_mutation(p1_id, None, Some(&sag_rec));
        ctx.emit_item_change_with_target(
            super::NotificationTarget::Corporation(corp_id),
            "corpid",
            &deliv_rec,
            vec![(3, PyRep::Integer(0))],
        );
        ctx.notify_item_mutation(p1_id, None, Some(&locked_rec));
        ctx.notify_item_mutation(p1_id, None, Some(&upwell_hi_rec));
        ctx.notify_item_mutation(p1_id, None, Some(&upwell_fuel_rec));
        let multi_rows = vec![
            super::delta_projection::build_inv_item_dbrow(&sag_rec, &ctx.sde, ""),
            super::delta_projection::build_inv_item_dbrow(&deliv_rec, &ctx.sde, ""),
        ];
        ctx.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnMultipleItemChange",
            "corpid",
            vec![
                PyRep::List(multi_rows),
                PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
            ],
        );
        assert!(ctx.should_skip_projected_item_change_for_target(
            &super::NotificationTarget::Corporation(corp_id),
            &sag_rec,
            &[(3, PyRep::Integer(0))],
            "test_should_skip_change",
        ));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    // Also verify corp deletion projection + `should_skip_projected_item_deleted_for_target`.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_on_corp_delete", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(deliv_id);
                Ok(())
            })
            .expect("on corp delete tx");
        assert!(ctx.should_skip_projected_item_deleted_for_target(
            &super::NotificationTarget::Corporation(corp_id),
            &deliv_rec,
            &[],
            "test_should_skip_deleted",
        ));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    for cid in [p1_id, p2_id] {
        let notifs = ctx.drain_notifications_for_char(cid);
        let item_notifs: Vec<_> = notifs
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .collect();
        let multi_notifs: Vec<_> = notifs
            .iter()
            .filter(|n| n.event == "OnMultipleItemChange")
            .collect();
        assert_eq!(item_notifs.len(), 6, "5 creates + 1 delete for cid={cid}: {notifs:?}");
        assert_eq!(multi_notifs.len(), 6, "5 creates + 1 delete for cid={cid}: {notifs:?}");
        assert!(item_notifs.iter().all(|n| n.id_type == "corpid"));
        assert!(multi_notifs.iter().all(|n| n.id_type == "corpid"));
    }
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.replaced, 9, "expected exact unique replaced corp facts in sw={sw:#?}");
    assert!(
        sw.by_scope.dispatch.replaced >= 15,
        "expected deduped corp emit calls across multi-row + should_skip helpers in sw={sw:#?}"
    );

    // 3. Partial multi-recipient dedupe: p1 moves in Commit 1 (excluded on that transition commit
    //    while CorpSAG1 projects to non-transitioned p2); legacy Corporation(corp_id) notify
    //    delivers ONLY to unprojected p1 and drops duplicate for p2.
    let partial_item_id = 993_950_107_i64;
    let partial_rec = InventoryItemRecord {
        item_id: partial_item_id,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 77,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_partial_corp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&p1_id)
                    .expect("p1")
                    .solar_system_id = sys_id + 1;
                tx.snap.items.insert(partial_item_id, partial_rec.clone());
                Ok(())
            })
            .expect("partial corp tx");
        ctx.emit_item_change_routed(p1_id, &partial_rec, vec![(3, PyRep::Integer(0))]);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let p1_partial: Vec<_> = ctx
        .drain_notifications_for_char(p1_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    let p2_partial: Vec<_> = ctx
        .drain_notifications_for_char(p2_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(p1_partial.len(), 1, "unprojected p1 still gets legacy deliver once");
    assert_eq!(p1_partial[0].id_type, "corpid");
    assert_eq!(p2_partial.len(), 1, "projected p2 gets commit deliver with 0 duplicate: {p2_partial:?}");
    assert_eq!(p2_partial[0].id_type, "corpid");

    // 4. NPC corp scoping: CorpSAG1 projects to online NPC-corp member p1 only, while non-hangar
    //    non-blueprint NPC space item (InventoryFlag::None) emits 0 broadcasts.
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().expect("npc corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = npc_corp_id;
    });
    let npc_hangar_id = 993_950_108_i64;
    let npc_space_id = 993_950_109_i64;
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_npc_corp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    npc_hangar_id,
                    InventoryItemRecord {
                        item_id: npc_hangar_id,
                        type_id: 34,
                        owner_id: npc_corp_id,
                        location_id: station_id,
                        flag: InventoryFlag::CorpSAG1,
                        quantity: 10,
                        version: 1,
                        ..Default::default()
                    },
                );
                tx.snap.items.insert(
                    npc_space_id,
                    InventoryItemRecord {
                        item_id: npc_space_id,
                        type_id: 34,
                        owner_id: npc_corp_id,
                        location_id: i64::from(sys_id),
                        flag: InventoryFlag::None,
                        quantity: 5,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("npc corp tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let n_npc_p1 = ctx.drain_notifications_for_char(p1_id);
    let n_npc_p2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(
        n_npc_p1
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        1,
        "only NPC corp hangar item projects to p1 (non-hangar NPC space item excluded): {n_npc_p1:?}"
    );
    assert!(n_npc_p2.is_empty(), "player corp p2 must not receive NPC corp items");
}

#[tokio::test]
async fn track5c_s1_cross_owner_char_corp_isolation_corp_blueprints_and_corp_industry_materials() {
    use crate::services::industry_pi::ATTR_BLUEPRINT_ME;
    use crate::services::notify_emitters_character_services::{
        emit_industry_materials_consumed, emit_industry_materials_for_scopes,
    };

    const MAT_TYPE: u32 = 38;
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, sys_id, ship1, ship2, corp1_id, corp2_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_960_010_i64,
        993_960_011_i64,
        98_000_601_i64,
        98_000_602_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let xfer_id = 993_960_101_i64;
    let corp_xfer_id = 993_960_102_i64;
    let bp_id = 993_960_103_i64;
    let mat_id = 993_960_104_i64;
    let (mon_p1_id, mon_p2_unauth_id) = (77_601_i64, 77_602_i64);

    let xfer_pre_char = InventoryItemRecord {
        item_id: xfer_id,
        type_id: 34,
        owner_id: p1_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 25,
        version: 1,
        ..Default::default()
    };
    let corp_xfer_pre = InventoryItemRecord {
        item_id: corp_xfer_id,
        type_id: 35,
        owner_id: corp1_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 10,
        version: 1,
        ..Default::default()
    };
    let mat_pre = InventoryItemRecord {
        item_id: mat_id,
        type_id: MAT_TYPE,
        owner_id: corp1_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 500,
        version: 1,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        // p1 is CEO of corp1; p2 is regular member with roles = 0 (no hangar query role).
        let mut corp1 = CorporationRecord::new(corp1_id, "Corp One", "C1", p1_id);
        corp1.members.insert(p1_id);
        corp1.members.insert(p2_id);
        snap.corporations.insert(corp1_id, corp1);
        let mut corp2 = CorporationRecord::new(corp2_id, "Corp Two", "C2", p2_id);
        corp2.members.insert(p2_id);
        snap.corporations.insert(corp2_id, corp2);
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = corp1_id;
        snap.characters.get_mut(&p2_id).expect("p2").corporation_id = corp1_id;
        snap.items.insert(xfer_id, xfer_pre_char.clone());
        snap.items.insert(corp_xfer_id, corp_xfer_pre.clone());
        snap.items.insert(mat_id, mat_pre.clone());
        let mut monitors = BTreeMap::new();
        for (mid, cid) in [(mon_p1_id, p1_id), (mon_p2_unauth_id, p2_id)] {
            monitors.insert(
                mid,
                IndustryMaterialMonitorSub {
                    char_id: cid,
                    owner_id: corp1_id,
                    location_id: station_id,
                    flag_id: InventoryFlag::CorpSAG1 as i64,
                },
            );
        }
        save_industry_material_monitors(snap, &monitors);
    });
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    // 1. Cross-owner Char(p1) -> Corp(corp1) + Corp1 -> Char(p1)? Keep Char->Corp + Corp BP create + Corp material update.
    let mut xfer_post_corp = xfer_pre_char.clone();
    xfer_post_corp.owner_id = corp1_id;
    xfer_post_corp.flag = InventoryFlag::CorpSAG1;
    xfer_post_corp.version = 2;
    let corp_bp_create = InventoryItemRecord {
        item_id: bp_id,
        type_id: bp_type_id,
        owner_id: corp1_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 1,
        singleton: true,
        blueprint_runs_remaining: Some(-1),
        version: 1,
        ..Default::default()
    };
    let mut mat_post = mat_pre.clone();
    mat_post.quantity = 200;
    mat_post.version = 2;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_xfer_bp_mat", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(xfer_id, xfer_post_corp.clone());
                tx.snap.items.insert(bp_id, corp_bp_create.clone());
                tx.snap.items.insert(mat_id, mat_post.clone());
                Ok(())
            })
            .expect("tx");
        // Verify dedupe for both charid & corpid on cross-owner transfer:
        ctx.notify_item_mutation(p1_id, Some(&xfer_pre_char), Some(&xfer_post_corp));
        // Multi-recipient legacy OnBlueprintsUpdated dedupe:
        ctx.notify(
            super::NotificationTarget::Corporation(corp1_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(corp1_id)],
        );
        // Legacy OnIndustryMaterials refreshes must dedupe covered corp monitor:
        emit_industry_materials_for_scopes(
            &ctx,
            &[(corp1_id, station_id, InventoryFlag::CorpSAG1 as i64, MAT_TYPE)],
        );
        emit_industry_materials_consumed(
            &ctx,
            p1_id,
            1,
            station_id,
            station_id,
            corp1_id,
            true,
            InventoryFlag::CorpSAG1,
            None,
            &[(MAT_TYPE, 300)],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let n_p1 = ctx.drain_notifications_for_char(p1_id);
    let n_p2 = ctx.drain_notifications_for_char(p2_id);
    // Section 4C: p1 receives exactly ONE OnItemChange per item (`corpid` from new_item), zero duplicate `charid`!
    assert_eq!(
        n_p1.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "charid")
            .count(),
        0,
        "n_p1={n_p1:?}"
    );
    assert_eq!(
        n_p1.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        3,
        "n_p1={n_p1:?}"
    );
    assert_eq!(
        n_p2.iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        3,
        "n_p2={n_p2:?}"
    );
    // Corp OnBlueprintsUpdated once per online member, multi-recipient legacy notify deduped:
    for (label, notifs) in [("p1", &n_p1), ("p2", &n_p2)] {
        let bps: Vec<_> = notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .collect();
        assert_eq!(bps.len(), 1, "{label}: {notifs:?}");
        assert_eq!(bps[0].id_type, "ownerID");
        assert_eq!(bps[0].args, vec![PyRep::Integer(corp1_id)]);
    }
    // Role-gated Corp OnIndustryMaterials: CEO p1 gets 1 (qty=200, no singleton BPO entry),
    // unprivileged p2 gets 0!
    let ind_p1: Vec<_> = n_p1
        .iter()
        .filter(|n| n.event == "OnIndustryMaterials")
        .collect();
    assert_eq!(ind_p1.len(), 1, "ind_p1={ind_p1:?}");
    let PyRep::Dict(entries) = &ind_p1[0].args[1] else {
        panic!("expected Dict: {:?}", ind_p1[0].args);
    };
    assert!(
        entries
            .iter()
            .any(|(k, v)| k.as_i64() == Some(i64::from(MAT_TYPE)) && v.as_i64() == Some(200)),
        "expected updated material 200 in {entries:?}"
    );
    assert!(
        !entries.iter().any(|(k, _)| k.as_i64() == Some(i64::from(bp_type_id))),
        "singleton corp BPO must never inject spurious type entry: {entries:?}"
    );
    assert_eq!(
        n_p2.iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .count(),
        0,
        "unauthorized p2 must receive 0 corp OnIndustryMaterials"
    );

    // 2. Lock singleton corp BPO (`CorpSAG1 -> Locked`) emits OnBlueprintsUpdated without spurious
    //    OnIndustryMaterials, followed by Corp -> Char transfer + in-place BPO ME update.
    let mut bp_locked = corp_bp_create.clone();
    bp_locked.flag = InventoryFlag::Locked;
    bp_locked.version = 2;
    let mut bp_me = bp_locked.clone();
    bp_me.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 7.0);
    bp_me.version = 3;
    let mut corp_xfer_to_char = corp_xfer_pre.clone();
    corp_xfer_to_char.owner_id = p1_id;
    corp_xfer_to_char.flag = InventoryFlag::Hangar;
    corp_xfer_to_char.version = 2;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_bp_lock", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bp_id, bp_locked.clone());
                Ok(())
            })
            .expect("lock corp bpo tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let n_p1_lock = ctx.drain_notifications_for_char(p1_id);
    let n_p2_lock = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(n_p1_lock.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);
    assert_eq!(n_p2_lock.iter().filter(|n| n.event == "OnBlueprintsUpdated").count(), 1);
    assert_eq!(
        n_p1_lock.iter().filter(|n| n.event == "OnIndustryMaterials").count(),
        0,
        "locking singleton corp BPO must emit 0 OnIndustryMaterials: {n_p1_lock:?}"
    );
    assert_eq!(
        n_p1_lock.iter().filter(|n| n.event == "OnItemChange" && n.id_type == "corpid").count(),
        1
    );

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_corp_to_char", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(corp_xfer_id, corp_xfer_to_char.clone());
                Ok(())
            })
            .expect("corp_to_char tx");
        ctx.notify_item_mutation(p1_id, Some(&corp_xfer_pre), Some(&corp_xfer_to_char));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let n_p1_step2 = ctx.drain_notifications_for_char(p1_id);
    let n_p2_step2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!(
        n_p1_step2
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "charid")
            .count(),
        1
    );
    assert_eq!(
        n_p1_step2
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        0
    );
    assert_eq!(
        n_p2_step2
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        1
    );

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_bp_me", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bp_id, bp_me.clone());
                Ok(())
            })
            .expect("bp me tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1
    );
    assert_eq!(
        ctx.drain_notifications_for_char(p2_id)
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1
    );

    // 3. Corp(corp1) <-> Corp(corp2) cross-corp transfer notifies members of both corps cleanly.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p2_id).expect("p2").corporation_id = corp2_id;
    });
    let corp_to_corp_id = 993_960_105_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_to_corp_id,
            InventoryItemRecord {
                item_id: corp_to_corp_id,
                type_id: 34,
                owner_id: corp1_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 10,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("track5c_corp_to_corp", |tx| -> Result<(), eve_db::TxError> {
                let rec = tx.snap.items.get_mut(&corp_to_corp_id).expect("corp_to_corp");
                rec.owner_id = corp2_id;
                rec.version += 1;
                Ok(())
            })
            .expect("corp to corp tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert_eq!(
        ctx.drain_notifications_for_char(p1_id)
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        1,
        "old corp1 member p1 must receive outgoing corpid OnItemChange"
    );
    assert_eq!(
        ctx.drain_notifications_for_char(p2_id)
            .iter()
            .filter(|n| n.event == "OnItemChange" && n.id_type == "corpid")
            .count(),
        1,
        "new corp2 member p2 must receive incoming corpid OnItemChange"
    );
}

#[tokio::test]
async fn track5c_s1_post_jump_contraband_confiscation_unblocked_and_active_ship_suppressed() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, contraband_id) = (
        90_000_001_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_970_010_i64,
        993_970_021_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let contraband = InventoryItemRecord {
        item_id: contraband_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 15,
        version: 1,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("pilot");
        ch.station_id = None;
        ch.structure_id = None;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(contraband_id, contraband.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        // Commit 1: CmdStargateJump changes solar_system_id (`prior_transitioned_chars` records pilot).
        ctx.db
            .tx("cmd_stargate_jump", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .solar_system_id = sys_id + 1;
                Ok(())
            })
            .expect("stargate jump commit");

        // Commit 2: post-jump contraband confiscation removes cargo item + updates active ship state.
        ctx.db
            .tx("scan_and_enforce_highsec_contraband", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.remove_system_cleanup_item(contraband_id);
                let ship = tx.snap.items.get_mut(&ship_id).expect("ship");
                ship.location_id = i64::from(sys_id + 1);
                ship.version += 1;
                Ok(())
            })
            .expect("contraband confiscation commit");
        // Legacy combat_npc_police.rs:516 emitter must be cleanly deduped!
        ctx.emit_item_deleted_routed(pilot_id, &contraband);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    let item_changes: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        item_changes.len(),
        1,
        "contraband cargo deletion must emit exactly 1 projected OnItemChange (0 duplicate, ship_id suppressed): {notifs:?}"
    );
    let (_, fp) = ItemWireFingerprint::from_pyrep_row(&item_changes[0].args[0]).expect("row fp");
    assert_eq!(fp.location_id, 6, "confiscated cargo must tombstone to locationID=6");
    let sw = projected_item_switch_summary(&ctx);
    assert!(sw.replaced >= 1, "legacy contraband delete must consume token cleanly: {sw:#?}");
}

#[tokio::test]
async fn track5c_s1_ragged_multi_recipient_corp_on_multiple_item_change_splits_per_recipient() {
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, sys_id, ship1, ship2, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_980_010_i64,
        993_980_011_i64,
        98_000_801_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "Ragged Corp", "RAG", p1_id);
        corp.members.insert(p1_id);
        corp.members.insert(p2_id);
        snap.corporations.insert(corp_id, corp);
        snap.characters.get_mut(&p1_id).expect("p1").corporation_id = corp_id;
        snap.characters.get_mut(&p2_id).expect("p2").corporation_id = corp_id;
    });
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let _ = ctx.drain_notifications_for_char(p1_id);
    let _ = ctx.drain_notifications_for_char(p2_id);

    let (item0_id, item1_id) = (993_980_101_i64, 993_980_102_i64);
    let item0 = InventoryItemRecord {
        item_id: item0_id,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 11,
        version: 1,
        ..Default::default()
    };
    let item1 = InventoryItemRecord {
        item_id: item1_id,
        type_id: 35,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 22,
        version: 1,
        ..Default::default()
    };
    let row0 = super::delta_projection::build_inv_item_dbrow(&item0, &ctx.sde, "");
    let row1 = super::delta_projection::build_inv_item_dbrow(&item1, &ctx.sde, "");

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        // Commit 1: p1 moves while item0 is committed -> item0 projects ONLY to p2 (consumed[0] = [false, true]).
        ctx.db
            .tx("ragged_commit1", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&p1_id)
                    .expect("p1")
                    .solar_system_id = sys_id + 1;
                tx.snap.items.insert(item0_id, item0.clone());
                Ok(())
            })
            .expect("commit 1");
        // Commit 2: p2 moves while item1 is committed -> item1 projects ONLY to p1 (consumed[1] = [true, false]).
        ctx.db
            .tx("ragged_commit2", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&p2_id)
                    .expect("p2")
                    .solar_system_id = sys_id + 1;
                tx.snap.items.insert(item1_id, item1.clone());
                Ok(())
            })
            .expect("commit 2");
        // Legacy shared 2-row corp OnMultipleItemChange -> ragged 2x2 matrix must split per recipient!
        ctx.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnMultipleItemChange",
            "corpid",
            vec![
                PyRep::List(vec![row0, row1]),
                PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let extract_multi_item_ids = |char_id: i64| -> (Vec<String>, Vec<i64>) {
        let notifs = ctx.drain_notifications_for_char(char_id);
        let mut id_types = Vec::new();
        let mut ids = Vec::new();
        for n in &notifs {
            if n.event == "OnMultipleItemChange" {
                id_types.push(n.id_type.clone());
                if let Some(PyRep::List(rows)) = n.args.first() {
                    for entry in rows {
                        if let Some((iid, _)) = ItemWireFingerprint::from_pyrep_row(entry) {
                            ids.push(iid);
                        }
                    }
                }
            }
        }
        (id_types, ids)
    };

    let (p1_id_types, mut p1_item_ids) = extract_multi_item_ids(p1_id);
    let (p2_id_types, mut p2_item_ids) = extract_multi_item_ids(p2_id);
    assert!(p1_id_types.iter().all(|t| t == "corpid"), "p1 id_types={p1_id_types:?}");
    assert!(p2_id_types.iter().all(|t| t == "corpid"), "p2 id_types={p2_id_types:?}");
    p1_item_ids.sort_unstable();
    p2_item_ids.sort_unstable();
    assert_eq!(
        p1_item_ids,
        vec![item0_id, item1_id],
        "p1 must receive item1 once at commit + item0 once from ragged split (0 duplicates)"
    );
    assert_eq!(
        p2_item_ids,
        vec![item0_id, item1_id],
        "p2 must receive item0 once at commit + item1 once from ragged split (0 duplicates)"
    );
}

#[tokio::test]
async fn track5c_s1_corp_off_telemetry_single_increment_and_deleted_only_per_kind_gating() {
    let ctx = test_ctx();
    let (p1_id, p2_id, p3_id, station_id, sys_id, ship1, ship2, corp_id) = (
        90_000_001_i64,
        90_000_002_i64,
        90_000_003_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_990_010_i64,
        993_990_011_i64,
        98_000_901_i64,
    );
    let setup_corp_3_members = |target_ctx: &ServerContext, corp_name: &str, ticker: &str| {
        setup_online_pilot_in_station(target_ctx, p1_id, station_id, sys_id, ship1);
        setup_online_pilot_in_station(target_ctx, p2_id, station_id, sys_id, ship2);
        target_ctx.db.with_mut_snapshot(|snap| {
            let mut acct3 = snap.accounts["demo"].clone();
            acct3.user_id = 90_000_003;
            acct3.username = "pilot3".to_string();
            snap.accounts.insert("pilot3".to_string(), acct3);
            let mut ch3 = snap.characters[&p1_id].clone();
            ch3.character_id = p3_id;
            ch3.user_id = 90_000_003;
            ch3.name = "Pilot Three".to_string();
            ch3.active_ship_id = 993_990_012;
            snap.characters.insert(p3_id, ch3);
            let mut corp = CorporationRecord::new(corp_id, corp_name, ticker, p1_id);
            corp.members.insert(p1_id);
            corp.members.insert(p2_id);
            corp.members.insert(p3_id);
            snap.corporations.insert(corp_id, corp);
            for cid in [p1_id, p2_id, p3_id] {
                snap.characters.get_mut(&cid).expect("char").corporation_id = corp_id;
            }
        });
        target_ctx
            .db
            .acquire_online_lock(90_000_003, 9_310, Some(p3_id), true, 1_700_000_500)
            .expect("lock p3");
    };
    setup_corp_3_members(&ctx, "TelGate Corp", "TGC");

    let item = InventoryItemRecord {
        item_id: 993_990_101,
        type_id: 34,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 15,
        version: 1,
        ..Default::default()
    };
    let row = super::delta_projection::build_inv_item_dbrow(&item, &ctx.sde, "");
    let change_dict = PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]);

    // 1. Multi-member (N=3) corp OnItemChange under test-overridden `corp_all_items = false` increments
    //    `NotCharacterOwned` and `non_first_set_emitted` ONCE per broadcast, not 3x.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_corp_all_items(false));
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnItemChange",
            "corpid",
            vec![row.clone(), change_dict.clone()],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_default = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw_default.not_covered_by_reason.get("not character-owned").copied().unwrap_or(0),
        1,
        "3-member corp OnItemChange must increment NotCharacterOwned once, got: {sw_default:#?}"
    );
    assert_eq!(sw_default.non_first_set_emitted, 1, "sw={sw_default:#?}");
    for cid in [p1_id, p2_id, p3_id] {
        assert_eq!(ctx.drain_notifications_for_char(cid).len(), 1);
    }

    // 2. Deleted-only switch config (`mutation=false, char_all_items=false, corp_all_items=false`):
    //    corp CreatedOrUpdated OnItemChange (both N=3 and N=1) and single-member OnMultipleItemChange
    //    must increment neither `NotCharacterOwned` nor `non_first_set_emitted`.
    let ctx_del_only = test_ctx();
    setup_corp_3_members(&ctx_del_only, "DelOnly Corp", "DOC");
    ctx_del_only.set_projected_item_switches(ProjectedItemSwitchConfig {
        char_same_scene_item_mutation: false,
        char_same_scene_item_deleted: true,
        char_same_scene_item_outside_rpc: false,
        char_same_scene_item_fitted: false,
        char_all_items: false,
        corp_all_items: false,
    });
    with_staged_dispatch_notifications(&*ctx_del_only.pending_notifications, &ctx_del_only.notify_tx, async {
        // N=3 OnItemChange
        ctx_del_only.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnItemChange",
            "corpid",
            vec![row.clone(), change_dict.clone()],
        );
        // Log off p2 & p3 so N=1 OnItemChange + OnMultipleItemChange hit per-recipient inner path:
        assert!(ctx_del_only.db.release_online_lock(
            ctx_del_only.db.with_snapshot(|s| s.characters[&p2_id].user_id),
            9_310,
        ));
        assert!(ctx_del_only.db.release_online_lock(90_000_003, 9_310));
        ctx_del_only.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnItemChange",
            "corpid",
            vec![row.clone(), change_dict.clone()],
        );
        ctx_del_only.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnMultipleItemChange",
            "corpid",
            vec![PyRep::List(vec![row.clone()]), change_dict.clone()],
        );
        flush_staged_dispatch_notifications(&*ctx_del_only.pending_notifications, &ctx_del_only.notify_tx);
    })
    .await;
    let sw_del_only = projected_item_switch_summary(&ctx_del_only);
    assert_eq!(
        sw_del_only.not_covered_by_reason.get("not character-owned").copied().unwrap_or(0),
        0,
        "deleted-only switch must never blame NotCharacterOwned for CreatedOrUpdated: {sw_del_only:#?}"
    );
    assert_eq!(sw_del_only.non_first_set_emitted, 0, "sw={sw_del_only:#?}");
    for cid in [p1_id, p2_id, p3_id] {
        let _ = ctx_del_only.drain_notifications_for_char(cid);
    }
}

#[tokio::test]
async fn track5c_s1_p2_flag_only_npc_corp_hangar_exit_and_commit2_non_ship_pilot_moved_scoping() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_995_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().expect("npc corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").corporation_id = npc_corp_id;
    });
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // P2-02: Same-station NPC corp flag exit (`CorpSAG1 -> Hangar`, ONLY `col 4` in dict, `col 3` omitted)
    // must match `CorpSAG1` eligibility via `col 4` cleanly without falling back to `NotCharacterOwned`.
    let npc_item_id = 993_995_101_i64;
    let npc_after = InventoryItemRecord {
        item_id: npc_item_id,
        type_id: 34,
        owner_id: npc_corp_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 20,
        version: 2,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        let mut pre = npc_after.clone();
        pre.flag = InventoryFlag::CorpSAG1;
        pre.version = 1;
        snap.items.insert(npc_item_id, pre);
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("p2_02_npc_flag_only_exit", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(npc_item_id, npc_after.clone());
                Ok(())
            })
            .expect("npc flag exit tx");
        let row = super::delta_projection::build_inv_item_dbrow(&npc_after, &ctx.sde, "");
        // Emit legacy corpid notification carrying ONLY col 4 = CorpSAG1 (116), omitting col 3:
        ctx.notify(
            super::NotificationTarget::Corporation(npc_corp_id),
            "OnItemChange",
            "corpid",
            vec![
                row,
                PyRep::Dict(vec![(
                    PyRep::Integer(4),
                    PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
                )]),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_npc = projected_item_switch_summary(&ctx);
    assert_eq!(sw_npc.replaced, 1, "sw_npc={sw_npc:#?}");
    assert_eq!(
        sw_npc.not_covered_by_reason.get("not character-owned").copied().unwrap_or(0),
        0,
        "flag-only NPC corp exit must not misclassify NotCharacterOwned: {sw_npc:#?}"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // Also test flag-only NPC corp exit with NO prior commit fact so `has_corp_owner_match_for_recipient`
    // is the sole gate: must resolve CorpSAG1 from `col 4` (`0 NotCharacterOwned`, `helper_without_token == 1`).
    let npc_unmatched = InventoryItemRecord {
        item_id: 993_995_109,
        type_id: 34,
        owner_id: npc_corp_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 7,
        version: 2,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let row = super::delta_projection::build_inv_item_dbrow(&npc_unmatched, &ctx.sde, "");
        ctx.notify(
            super::NotificationTarget::Corporation(npc_corp_id),
            "OnItemChange",
            "corpid",
            vec![
                row,
                PyRep::Dict(vec![(
                    PyRep::Integer(4),
                    PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
                )]),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_npc_unmatched = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw_npc_unmatched
            .not_covered_by_reason
            .get("not character-owned")
            .copied()
            .unwrap_or(0),
        0,
        "unmatched flag-only NPC corp exit must use col 4 instead of NotCharacterOwned: {sw_npc_unmatched:#?}"
    );
    assert!(
        sw_npc_unmatched.helper_without_token >= 1,
        "expected unmatched helper match via col 4: {sw_npc_unmatched:#?}"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // P2-03: Commit 1 transitions pilot; Commit 2 mutates another item cleanly (`latest_commit_transitioned_chars`
    // clears on Commit 2), so an unmatched non-ship Commit 2 helper is NOT falsely blamed on `PilotMovedInThisCommit`.
    let (other_item_id, untracked_non_ship_id) = (993_995_102_i64, 993_995_103_i64);
    let untracked_item = InventoryItemRecord {
        item_id: untracked_non_ship_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 5,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("p2_03_commit1_move", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .solar_system_id = sys_id + 1;
                Ok(())
            })
            .expect("commit 1");
        ctx.db
            .tx("p2_03_commit2_other_item", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    other_item_id,
                    InventoryItemRecord {
                        item_id: other_item_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("commit 2");
        // Unmatched legacy emit for non-ship item in Commit 2 must NOT mask under PilotMovedInThisCommit,
        // while unmatched emit for the transitioned active ship itself (`ship_id`) DOES report PilotMovedInThisCommit.
        ctx.notify_item_mutation(pilot_id, None, Some(&untracked_item));
        let sw_mid = projected_item_switch_summary(&ctx);
        assert_eq!(
            sw_mid.not_covered_by_reason.get("pilot moved in this commit").copied().unwrap_or(0),
            0,
            "Commit 2 non-ship unmatched helper must not mask under PilotMovedInThisCommit: {sw_mid:#?}"
        );
        assert!(
            sw_mid.helper_without_token >= 1,
            "Commit 2 non-ship unmatched helper must record helper_without_token: {sw_mid:#?}"
        );

        let ship_rec = ctx.db.with_snapshot(|s| s.items[&ship_id].clone());
        ctx.notify_item_mutation(pilot_id, None, Some(&ship_rec));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_end = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw_end.not_covered_by_reason.get("pilot moved in this commit").copied().unwrap_or(0),
        1,
        "Commit 2 unmatched active-ship emit must still report PilotMovedInThisCommit: {sw_end:#?}"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);
}

#[tokio::test]
async fn track5c_s2_r2_p2_bp_invalidation_oid_only_and_deferred_commit1_transition_telemetry() {
    let ctx = test_ctx();
    let (pilot_id, corp_id, station_id, sys_id, ship_id) = (
        90_000_001_i64,
        98_000_512_i64,
        60_003_760_i64,
        30_000_142_u32,
        993_996_010_i64,
    );
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "R2P2 Corp", "R2P2", pilot_id);
        corp.members.insert(pilot_id);
        snap.corporations.insert(corp_id, corp);
        snap.characters.get_mut(&pilot_id).expect("pilot").corporation_id = corp_id;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Step 1 R2 P2 #1 (dispatch + outside-RPC):
    //    Personal blueprint commit for `pilot_id` must NOT evict `(pilot_id, corp_id)` corp blueprint fact.
    let (corp_bp_id, personal_bp_id) = (993_996_101_i64, 993_996_102_i64);
    let corp_bp = InventoryItemRecord {
        item_id: corp_bp_id,
        type_id: bp_type_id,
        owner_id: corp_id,
        location_id: station_id,
        flag: InventoryFlag::CorpSAG1,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let personal_bp = InventoryItemRecord {
        item_id: personal_bp_id,
        type_id: bp_type_id,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("r2p2_dispatch_corp_bp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(corp_bp_id, corp_bp.clone());
                Ok(())
            })
            .expect("corp bp tx");
        ctx.db
            .tx("r2p2_dispatch_personal_bp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(personal_bp_id, personal_bp.clone());
                Ok(())
            })
            .expect("personal bp tx");
        // Legacy OnBlueprintsUpdated(corp_id) must STILL be deduped against `(pilot_id, corp_id)`.
        ctx.notify(
            super::NotificationTarget::Corporation(corp_id),
            "OnBlueprintsUpdated",
            "ownerID",
            vec![PyRep::Integer(corp_id)],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let disp_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        disp_notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated" && n.args == vec![PyRep::Integer(corp_id)])
            .count(),
        1,
        "personal bp commit must not evict dispatch (pilot_id, corp_id) bp fact (must stay 1, not 2): {disp_notifs:?}"
    );

    // Outside-RPC equivalent of Step 1 R2 P2 #1 (`OutsideRpcScopeFacts::invalidate_blueprints`):
    let (corp_bp2_id, personal_bp2_id) = (993_996_103_i64, 993_996_104_i64);
    ctx.db
        .tx("r2p2_outside_corp_bp", |tx| -> Result<(), eve_db::TxError> {
            let mut bp2 = corp_bp.clone();
            bp2.item_id = corp_bp2_id;
            tx.snap.items.insert(corp_bp2_id, bp2);
            Ok(())
        })
        .expect("outside corp bp tx");
    ctx.db
        .tx("r2p2_outside_personal_bp", |tx| -> Result<(), eve_db::TxError> {
            let mut pbp2 = personal_bp.clone();
            pbp2.item_id = personal_bp2_id;
            tx.snap.items.insert(personal_bp2_id, pbp2);
            Ok(())
        })
        .expect("outside personal bp tx");
    assert!(
        ctx.projected_item_switches
            .has_blueprint_fact_in_scope(None, pilot_id, corp_id),
        "outside-RPC personal bp commit must not evict (pilot_id, corp_id) bp fact"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.notify(
        super::NotificationTarget::Corporation(corp_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(corp_id)],
    );
    let out_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert!(
        !out_notifs.iter().any(|n| n.event == "OnBlueprintsUpdated"),
        "personal bp commit outside RPC must not evict (pilot_id, corp_id) fact: {out_notifs:?}"
    );

    // 2. Step 1 R2 P2 #2:
    //    (a) Commit 1 transitions pilot + mutates `commit1_item_id`, Commit 2 mutates `commit2_other_id`
    //        (non-transition): deferred Commit 1 emission for `commit1_item_id` still records
    //        `PilotMovedInThisCommit`.
    //    (b) Commit 1 transitions pilot (no items), Commit 2 updates character only (`changes.items`
    //        empty, non-transition): deferred Commit 1 emission still records `PilotMovedInThisCommit`.
    let (commit1_item_id, commit2_other_id) = (993_996_201_i64, 993_996_202_i64);
    let commit1_item = InventoryItemRecord {
        item_id: commit1_item_id,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 42,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("r2p2_c1_move_with_item", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .solar_system_id = sys_id + 1;
                tx.snap.items.insert(commit1_item_id, commit1_item.clone());
                Ok(())
            })
            .expect("c1");
        ctx.db
            .tx("r2p2_c2_other_item", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(
                    commit2_other_id,
                    InventoryItemRecord {
                        item_id: commit2_other_id,
                        type_id: 34,
                        owner_id: pilot_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 3,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("c2");
        ctx.notify_item_mutation(pilot_id, None, Some(&commit1_item));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_p2_a = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw_p2_a
            .not_covered_by_reason
            .get("pilot moved in this commit")
            .copied()
            .unwrap_or(0),
        1,
        "deferred Commit 1 item emission after non-transition Commit 2 must record PilotMovedInThisCommit: {sw_p2_a:#?}"
    );
    assert_eq!(
        sw_p2_a.helper_without_token, 0,
        "deferred Commit 1 emission must not miscount as helper_without_token: {sw_p2_a:#?}"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);

    let deferred_probe = InventoryItemRecord {
        item_id: 993_996_203,
        type_id: 34,
        owner_id: pilot_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 9,
        version: 1,
        ..Default::default()
    };
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("r2p2_c1_move_only", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .solar_system_id = sys_id + 2;
                Ok(())
            })
            .expect("c1 move");
        ctx.db
            .tx("r2p2_c2_char_only_no_items", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .security_status = 1.5;
                Ok(())
            })
            .expect("c2 char only");
        ctx.notify_item_mutation(pilot_id, None, Some(&deferred_probe));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    let sw_p2_b = projected_item_switch_summary(&ctx);
    assert_eq!(
        sw_p2_b
            .not_covered_by_reason
            .get("pilot moved in this commit")
            .copied()
            .unwrap_or(0),
        2,
        "deferred emission after char-only non-transition Commit 2 must keep PilotMovedInThisCommit: {sw_p2_b:#?}"
    );
    assert_eq!(
        sw_p2_b.helper_without_token, 0,
        "char-only Commit 2 must not force deferred Commit 1 into helper_without_token: {sw_p2_b:#?}"
    );
    let _ = ctx.drain_notifications_for_char(pilot_id);
}
