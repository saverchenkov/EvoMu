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
    assert!(!def.char_same_scene_item_outside_rpc);
    assert!(def.is_any_enabled());
    assert_eq!(def, ProjectedItemSwitchConfig::enabled());
    assert!(ProjectedItemSwitchConfig::all_enabled().char_same_scene_item_outside_rpc);
    assert_eq!(
        ProjectedItemSwitchConfig::enabled().with_outside_rpc(true),
        ProjectedItemSwitchConfig::all_enabled()
    );

    let off = ProjectedItemSwitchConfig::disabled();
    assert!(!off.char_same_scene_item_mutation);
    assert!(!off.char_same_scene_item_deleted);
    assert!(!off.char_same_scene_item_outside_rpc);
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
            _ => None,
        }),
        vec!["EVOMU_PROJECTED_ITEM_MUTATION", "EVOMU_PROJECTED_ITEM_DELETED"],
    );
    assert!(
        ProjectedItemSwitchConfig::retired_off_env_vars_lookup(|name| match name {
            "EVOMU_PROJECTED_ITEM_MUTATION" => Some("maybe".to_string()),
            "EVOMU_PROJECTED_ITEM_DELETED" => Some("".to_string()),
            _ => None,
        })
        .is_empty()
    );

    // False-default parser semantics remain intact for future switch classes.
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
        ProjectedItemSwitchConfig::all_enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("true".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::all_enabled(),
    );
    assert_eq!(
        ProjectedItemSwitchConfig::from_env_lookup(|name| match name {
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC" => Some("0".to_string()),
            _ => None,
        }),
        ProjectedItemSwitchConfig::enabled(),
    );

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
    // (`changes.items` empty); Commit 2 inserts/deletes active-ship cargo without modifying
    // `changes.characters` -> transition holds Commit 2 silent (`0 OnItemChange`).
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
            .tx("r1_bf01_commit2_space_cargo_insert", |tx| -> Result<(), eve_db::TxError> {
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
            .expect("commit 2");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let notifs = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(
        notifs.iter().filter(|n| n.event == "OnItemChange").count(),
        0,
        "Commit 2 inside an in-space active_ship_id transition dispatch must emit 0 switched OnItemChange events"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.emitted, 0, "sw={sw:#?}");
    assert_eq!(sw.replaced, 0, "sw={sw:#?}");
    assert_eq!(sw.new, 0, "sw={sw:#?}");

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
            },
            false,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: true,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
            },
            false,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: false,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
            },
            true,
        ),
        (
            ProjectedItemSwitchConfig {
                char_same_scene_item_mutation: true,
                char_same_scene_item_deleted: true,
                char_same_scene_item_outside_rpc: false,
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

    // 3b. Blueprint deletion in ship Cargo alongside notify_item_mutation skips helper item/multi
    //     companions while leaving OnBlueprintsUpdated compare-only & intact.
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

        // Replaced via emit_inv_item_change (inventory_bound wrapper)
        let site_inv_mut = format!("{file_norm}:{}", line!() + 1);
        crate::services::inventory_bound::emit_inv_item_change(&ctx, pilot_id, &item_inv_mut, vec![(3, PyRep::Integer(0))]);

        // Replaced via emit_inv_item_deleted (inventory_bound wrapper)
        let site_inv_del = format!("{file_norm}:{}", line!() + 1);
        crate::services::inventory_bound::emit_inv_item_deleted(&ctx, pilot_id, &item_inv_del);

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
    let ctx = {
        let _env_guard = crate::lock_test_env_write();
        let keys = ["EVOMU_PROJECTED_ITEM_MUTATION", "EVOMU_PROJECTED_ITEM_DELETED"];
        let _reset = EnvReset(keys.iter().map(|&k| (k, std::env::var(k).ok())).collect());
        for k in keys {
            std::env::set_var(k, "0");
        }
        ServerContext::new()
    };
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::enabled());
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
