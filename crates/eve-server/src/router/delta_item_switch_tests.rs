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

fn test_ctx() -> ServerContext {
    ServerContext::new()
}

fn setup_online_pilot_in_station(
    ctx: &ServerContext,
    pilot_id: i64,
    station_id: i64,
    sys_id: u32,
    ship_id: i64,
) {
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_310, Some(pilot_id), true, 1_700_000_500)
        .expect("acquire online lock");
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

    // 4. Active ship transition (BoardShip / capsule delete) stays unprojected.
    let capsule_id = 992_600_030_i64;
    let new_ship_id = 992_600_031_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_id).expect("pilot").station_id = Some(station_id);
        snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = capsule_id;
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
            .tx("g2_board_ship", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = new_ship_id;
                tx.snap.remove_system_cleanup_item(capsule_id);
                Ok(())
            })
            .expect("board tx");
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "active_ship_id transition commit must hold back item projection"
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

    // Single RPC dispatch: Commit 1 changes `active_ship_id` (`changes.items` empty),
    // Commit 2 mutates & deletes station hangar items without modifying `changes.characters`.
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _rpc_guard = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("ship", "Board"),
        );
        ctx.db
            .tx("r1_bf01_commit1_ship_transition", |tx| -> Result<(), eve_db::TxError> {
                tx.snap
                    .characters
                    .get_mut(&pilot_id)
                    .expect("pilot")
                    .active_ship_id = ship_b_id;
                Ok(())
            })
            .expect("commit 1");
        ctx.db
            .tx("r1_bf01_commit2_hangar_mutate_and_delete", |tx| -> Result<(), eve_db::TxError> {
                let row = tx.snap.items.get_mut(&mut_item_id).expect("mut_item");
                row.quantity = 120;
                row.version += 1;
                tx.snap.remove_system_cleanup_item(del_item_id);
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
        "Commit 2 inside a character transition dispatch must emit 0 switched OnItemChange events"
    );
    let sw = projected_item_switch_summary(&ctx);
    assert_eq!(sw.emitted, 0, "sw={sw:#?}");
    assert_eq!(sw.replaced, 0, "sw={sw:#?}");
    assert_eq!(sw.new, 0, "sw={sw:#?}");
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

#[tokio::test]
async fn step1_outside_rpc_off_by_default_records_shadow_by_scope_without_altering_delivery() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_950_010_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    assert_eq!(ctx.projected_item_switches(), ProjectedItemSwitchConfig::default());
    assert!(!ctx.projected_item_switches().char_same_scene_item_outside_rpc);
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));

    let tick_probe_id = 993_950_021_i64;
    let neither_item_id = 993_950_022_i64;
    let no_proj_item_id = 993_950_023_i64;
    let dispatch_item_id = 993_950_024_i64;

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

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(neither_item_id, neither_hangar_before.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

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
    super::delta_item_compare::with_compare_tick_scope(async {
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
    super::delta_item_compare::with_compare_tick_scope(async {
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
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled().with_outside_rpc(true));

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
    super::delta_item_compare::with_compare_tick_scope(async {
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
        ctx.projected_item_switches()
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
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());

    let fitted_id = 993_970_021_i64;
    let space_item_id = 993_970_022_i64;
    let cont_item_id = 993_970_023_i64;
    let corp_item_id = 993_970_024_i64;
    let fuel_item_id = 993_970_025_i64;
    let exp_item_id = 993_970_026_i64;
    let scene_item_id = 993_970_027_i64;
    let offline_item_id = 993_970_028_i64;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&pilot_id].corporation_id);
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
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Fitted module, item in space, container item, non-char corp item -> all silent outside RPC.
    super::delta_item_compare::with_compare_tick_scope(async {
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

    // 4. Commit moving the pilot / changing active_ship_id holds item projection silent.
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
        "commit changing pilot scene/active_ship_id must stay silent outside RPC"
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
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::all_enabled());

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
