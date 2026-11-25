//! Unit tests for Step 3a extended recipient rules (Rules 3–5), holder resolution,
//! Section 4D cross-id-type ledger suppression, and live commit-time item/blueprint/industry
//! projection invariants (`#931`).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use eve_db::{
    AccountRecord, ChangeSet, CharacterRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, RowChange,
};
use eve_proto::PyRep;
use eve_testkit::shadow_client::ShadowClient;

use crate::services::industry_pi::{ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE};
use crate::services::notify_emitters_character_services::{
    save_industry_material_monitors, IndustryMaterialMonitorSub,
};
use crate::services::tactical_modules::db_ptr_key;

use super::cache_notifications::{
    assemble_pending_notification, enqueue_or_stage_pending_batch,
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
    PendingMachoNotification, MAX_PENDING_NOTIFICATIONS_PER_CHAR,
};
use super::delta_item_recipients::{resolve_item_holder, ItemHolderKind};
use super::delta_item_switch::{is_character_scene_or_ship_transition, ItemBeforeAfter};
use super::dispatch_active_char::with_active_dispatch_char;
use super::ServerContext;

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
    let _ = ctx.drain_notifications_for_char(pilot_id);
}

fn make_corp_with_members(
    ctx: &ServerContext,
    corp_id: i64,
    ceo_id: i64,
    members: &[i64],
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = ceo_id;
        c.name = format!("Corp {corp_id}");
        c.ticker = "T3A".into();
        for &m in members {
            c.members.insert(m);
            if let Some(ch) = snap.characters.get_mut(&m) {
                ch.corporation_id = corp_id;
            }
        }
        snap.corporations.insert(corp_id, c);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_outside_rpc_ownerless_and_same_commit_ownerless_container_emit_zero() {
    let ctx = test_ctx();
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 995_932_001_i64;
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);

    // Pre-seed an ownerless item outside RPC, then mutate it outside RPC.
    let loose_ownerless = 995_932_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            loose_ownerless,
            InventoryItemRecord {
                item_id: loose_ownerless,
                type_id: 34,
                owner_id: 0,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 50,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    ctx.db
        .tx("outside_rpc_ownerless_mut", |tx| -> Result<(), eve_db::TxError> {
            if let Some(it) = tx.snap.items.get_mut(&loose_ownerless) {
                it.quantity = 40;
                it.version += 1;
            }
            Ok(())
        })
        .expect("outside rpc tx");
    assert!(
        ctx.drain_notifications_for_char(pilot_id).is_empty(),
        "outside RPC ownerless mutation must emit zero projected notifications"
    );

    // Inside RPC: commit creating an ownerless container in space together with ownerless contents
    // must send the acting pilot nothing.
    let new_box_id = 995_932_020_i64;
    let new_loot_id = 995_932_021_i64;
    with_active_dispatch_char(Some(pilot_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("same_commit_ownerless_box", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(
                        new_box_id,
                        InventoryItemRecord {
                            item_id: new_box_id,
                            type_id: 23,
                            owner_id: 0,
                            location_id: i64::from(sys_id),
                            flag: InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    tx.snap.items.insert(
                        new_loot_id,
                        InventoryItemRecord {
                            item_id: new_loot_id,
                            type_id: 34,
                            owner_id: 0,
                            location_id: new_box_id,
                            flag: InventoryFlag::Cargo,
                            quantity: 25,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    Ok(())
                })
                .expect("same commit ownerless box");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let pilot_notifs = ctx.drain_notifications_for_char(pilot_id);
    assert!(
        pilot_notifs.is_empty(),
        "same-commit ownerless container + contents must emit nothing to acting pilot: {pilot_notifs:?}"
    );
}

#[test]
fn step3a_holder_rules_cover_deleted_container_nested_ship_and_exclude_station_structure_office() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let pilot_id = 90_000_001_i64;

    let ship_id = 995_934_001_i64;
    let c_in_ship = 995_934_002_i64;
    let c_in_station = 995_934_003_i64;
    let struct_id = 995_934_004_i64;
    let c_in_struct = 995_934_005_i64;
    let office_id = 995_934_006_i64;
    let c_in_office = 995_934_007_i64;
    let deleted_space_box = 995_934_008_i64;

    let struct_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 65)
        .map(|t| t.type_id)
        .expect("Upwell structure type");

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc, flag) in [
            (ship_id, 587, station_id, InventoryFlag::Hangar),
            (c_in_ship, 23, ship_id, InventoryFlag::Cargo),
            (c_in_station, 23, station_id, InventoryFlag::Hangar),
            (struct_id, struct_type_id, i64::from(sys_id), InventoryFlag::Hangar),
            (c_in_struct, 23, struct_id, InventoryFlag::Hangar),
            (office_id, 27, station_id, InventoryFlag::Hangar),
            (c_in_office, 23, office_id, InventoryFlag::CorpSAG1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: pilot_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let deleted_box_rec = InventoryItemRecord {
        item_id: deleted_space_box,
        type_id: 23,
        owner_id: pilot_id,
        location_id: i64::from(sys_id),
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let deleted_changes = ChangeSet {
        items: vec![RowChange {
            key: deleted_space_box,
            before: Some(Arc::new(deleted_box_rec)),
            after: None,
        }],
        ..Default::default()
    };
    let mut item_changes_by_id: HashMap<i64, ItemBeforeAfter<'_>> = HashMap::new();
    for ch in &deleted_changes.items {
        item_changes_by_id.insert(ch.key, (ch.before.as_deref(), ch.after.as_deref()));
    }

    ctx.db.with_snapshot(|snap| {
        let sde = &ctx.sde;
        // Nested container inside ship resolves nearest ship.
        let ship_res = resolve_item_holder(995_934_900, c_in_ship, snap, None, None, sde);
        assert_eq!(ship_res.map(|(k, r)| (k, r.item_id)), Some((ItemHolderKind::Ship, ship_id)));

        // Station / Upwell / Corp office containers have NO holder.
        assert_eq!(resolve_item_holder(995_934_901, c_in_station, snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_902, c_in_struct, snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_903, struct_id, snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_904, c_in_office, snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_905, office_id, snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_906, i64::from(sys_id), snap, None, None, sde), None);
        assert_eq!(resolve_item_holder(995_934_907, station_id, snap, None, None, sde), None);

        // Deleted container in space resolves via item_changes_by_id before-image.
        let del_res = resolve_item_holder(
            995_934_908,
            deleted_space_box,
            snap,
            Some(&deleted_changes),
            Some(&item_changes_by_id),
            sde,
        );
        assert_eq!(
            del_res.map(|(k, r)| (k, r.item_id)),
            Some((ItemHolderKind::Space, deleted_space_box))
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3b1_acting_pilot_skips_destroyed_ship_holder_and_keeps_live_ship_and_space_holders() {
    let ctx = test_ctx();
    let killer_id = 90_000_001_i64;
    let victim_id = 90_000_002_i64;
    let other_pilot_id = 96_931_803_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let killer_ship = 995_935_001_i64;
    let victim_ship = 995_935_002_i64;
    let other_ship = 995_935_003_i64;
    let live_space_box = 995_935_004_i64;

    setup_online_pilot_in_station(&ctx, killer_id, station_id, sys_id, killer_ship);
    setup_online_pilot_in_station(&ctx, victim_id, station_id, sys_id, victim_ship);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&killer_id].clone();
        ch.character_id = other_pilot_id;
        ch.user_id = other_pilot_id as u64;
        ch.name = format!("Other {other_pilot_id}");
        snap.characters.insert(other_pilot_id, ch);
        snap.accounts.insert(
            format!("user_{other_pilot_id}"),
            AccountRecord {
                user_id: other_pilot_id as u64,
                username: format!("user_{other_pilot_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    setup_online_pilot_in_station(&ctx, other_pilot_id, station_id, sys_id, other_ship);

    let victim_mod = 995_935_010_i64;
    let victim_cargo = 995_935_011_i64;
    let wreck_id = 995_935_012_i64;
    let live_bay_item = 995_935_013_i64;
    let live_box_item = 995_935_014_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&victim_ship) {
            ship.location_id = i64::from(sys_id);
            ship.flag = InventoryFlag::None;
        }
        snap.items.insert(
            victim_mod,
            InventoryItemRecord {
                item_id: victim_mod,
                type_id: 2048,
                owner_id: victim_id,
                location_id: victim_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            victim_cargo,
            InventoryItemRecord {
                item_id: victim_cargo,
                type_id: 34,
                owner_id: victim_id,
                location_id: victim_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            live_space_box,
            InventoryItemRecord {
                item_id: live_space_box,
                type_id: 23,
                owner_id: other_pilot_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            live_bay_item,
            InventoryItemRecord {
                item_id: live_bay_item,
                type_id: 34,
                owner_id: other_pilot_id,
                location_id: other_ship,
                flag: InventoryFlag::FleetHangar,
                quantity: 50,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            live_box_item,
            InventoryItemRecord {
                item_id: live_box_item,
                type_id: 34,
                owner_id: other_pilot_id,
                location_id: live_space_box,
                flag: InventoryFlag::Cargo,
                quantity: 20,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(killer_id);
    let _ = ctx.drain_notifications_for_char(victim_id);
    let _ = ctx.drain_notifications_for_char(other_pilot_id);

    // 1. Ship destruction commit: `victim_ship` is deleted alongside destroying `victim_mod` and
    //    moving `victim_cargo` into a new `wreck_id` -> killer must receive 0 `OnItemChange`.
    with_active_dispatch_char(Some(killer_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("destroy_victim_ship", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.remove(&victim_mod);
                    tx.snap.items.insert(
                        wreck_id,
                        InventoryItemRecord {
                            item_id: wreck_id,
                            type_id: 23,
                            owner_id: victim_id,
                            location_id: i64::from(sys_id),
                            flag: InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    if let Some(cargo) = tx.snap.items.get_mut(&victim_cargo) {
                        cargo.location_id = wreck_id;
                        cargo.version += 1;
                    }
                    tx.snap.items.remove(&victim_ship);
                    Ok(())
                })
                .expect("destroy victim ship commit");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let killer_kill_notifs: Vec<_> = ctx
        .drain_notifications_for_char(killer_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert!(
        killer_kill_notifs.is_empty(),
        "destroying another pilot's ship must emit 0 OnItemChange to killer: {killer_kill_notifs:?}"
    );
    let victim_kill_notifs: Vec<_> = ctx
        .drain_notifications_for_char(victim_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert!(
        !victim_kill_notifs.is_empty(),
        "victim must still receive OnItemChange for destroyed ship/cargo"
    );

    // 2. Live non-owned ship bay + live space container mutations still emit RULE_HOLDER_ACTING_PILOT to actor.
    with_active_dispatch_char(Some(killer_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("live_holder_ops", |tx| -> Result<(), eve_db::TxError> {
                    if let Some(it) = tx.snap.items.get_mut(&live_bay_item) {
                        it.quantity = 30;
                        it.version += 1;
                    }
                    if let Some(it) = tx.snap.items.get_mut(&live_box_item) {
                        it.quantity = 15;
                        it.version += 1;
                    }
                    Ok(())
                })
                .expect("live holder commit");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let killer_live_ids: Vec<i64> = ctx
        .drain_notifications_for_char(killer_id)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(eve_proto::PyRep::as_i64))
        .collect();
    assert_eq!(
        killer_live_ids,
        vec![live_bay_item, live_box_item],
        "live non-owned ship bay and space container must still emit RULE_HOLDER_ACTING_PILOT to acting pilot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3b1_destroyed_space_holder_drops_leak_when_deleted_and_notifies_when_loot_relocated() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let acting_pilot = 90_000_001_i64;
    let owner_pilot = 90_000_002_i64;
    let acting_ship = 995_936_001_i64;
    let owner_ship = 995_936_002_i64;
    let doomed_box = 995_936_010_i64;
    let doomed_box_loot = 995_936_011_i64;
    let salvage_wreck = 995_936_020_i64;
    let salvage_loot = 995_936_021_i64;
    let spawned_can = 995_936_022_i64;

    setup_online_pilot_in_station(&ctx, acting_pilot, station_id, sys_id, acting_ship);
    setup_online_pilot_in_station(&ctx, owner_pilot, station_id, sys_id, owner_ship);

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc, flag, qty, single) in [
            (doomed_box, 23_u32, i64::from(sys_id), InventoryFlag::None, 1, true),
            (doomed_box_loot, 34_u32, doomed_box, InventoryFlag::Cargo, 40, false),
            (salvage_wreck, 26_557_u32, i64::from(sys_id), InventoryFlag::None, 1, true),
            (salvage_loot, 34_u32, salvage_wreck, InventoryFlag::Cargo, 25, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner_pilot,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: single,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let _ = ctx.drain_notifications_for_char(acting_pilot);
    let _ = ctx.drain_notifications_for_char(owner_pilot);

    // 1. Destroying a foreign space container/deployable AND deleting its enclosed contents
    //    (`ItemHolderKind::Space`, `after_opt.is_none()`) must emit zero `OnItemChange` to actor.
    with_active_dispatch_char(Some(acting_pilot), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("destroy_space_box", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.remove(&doomed_box_loot);
                    tx.snap.items.remove(&doomed_box);
                    Ok(())
                })
                .expect("destroy space box commit");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let actor_destroy_notifs: Vec<_> = ctx
        .drain_notifications_for_char(acting_pilot)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert!(
        actor_destroy_notifs.is_empty(),
        "destroying space container with deleted contents must emit 0 OnItemChange to attacker: {actor_destroy_notifs:?}"
    );
    let owner_destroy_ids: Vec<i64> = ctx
        .drain_notifications_for_char(owner_pilot)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| {
            n.args
                .first()
                .and_then(|r| r.dbrow_get("itemID"))
                .and_then(eve_proto::PyRep::as_i64)
        })
        .collect();
    assert!(
        owner_destroy_ids.contains(&doomed_box) && owner_destroy_ids.contains(&doomed_box_loot),
        "owner must still receive OnItemChange for destroyed container + contents: {owner_destroy_ids:?}"
    );

    // 2. Relocating surviving loot out of a deleted space wreck into a newly created jetcan
    //    (`ItemHolderKind::Space`, `after_opt.is_some()`) notifies the salvager via `RULE_HOLDER_ACTING_PILOT`.
    with_active_dispatch_char(Some(acting_pilot), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("salvage_relocate_loot", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(
                        spawned_can,
                        InventoryItemRecord {
                            item_id: spawned_can,
                            type_id: 23,
                            owner_id: owner_pilot,
                            location_id: i64::from(sys_id),
                            flag: InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    if let Some(loot) = tx.snap.items.get_mut(&salvage_loot) {
                        loot.location_id = spawned_can;
                        loot.version += 1;
                    }
                    tx.snap.items.remove(&salvage_wreck);
                    Ok(())
                })
                .expect("salvage relocate commit");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let actor_salvage_ids: Vec<i64> = ctx
        .drain_notifications_for_char(acting_pilot)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| {
            n.args
                .first()
                .and_then(|r| r.dbrow_get("itemID"))
                .and_then(eve_proto::PyRep::as_i64)
        })
        .collect();
    assert_eq!(
        actor_salvage_ids,
        vec![salvage_loot],
        "salvaging foreign wreck with loot must emit OnItemChange for moved loot to acting pilot"
    );
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

async fn run_dispatch_tx<F: FnOnce(&mut eve_db::WorldTx<'_>) -> Result<(), eve_db::TxError>>(ctx: &ServerContext, label: &'static str, f: F) {
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.tx(label, f).expect(label);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    }).await;
}

async fn run_dispatch_2tx<F1, F2>(ctx: &ServerContext, l1: &'static str, f1: F1, l2: &'static str, f2: F2)
where
    F1: FnOnce(&mut eve_db::WorldTx<'_>) -> Result<(), eve_db::TxError>,
    F2: FnOnce(&mut eve_db::WorldTx<'_>) -> Result<(), eve_db::TxError>,
{
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db.tx(l1, f1).expect(l1);
        ctx.db.tx(l2, f2).expect(l2);
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    }).await;
}

fn item_change_notifs(notifs: &[PendingMachoNotification]) -> Vec<&PendingMachoNotification> {
    notifs.iter().filter(|n| n.event == "OnItemChange").collect()
}

fn ev_cnt(notifs: &[PendingMachoNotification], ev: &str) -> usize { notifs.iter().filter(|n| n.event == ev).count() }
fn ic_cnt(notifs: &[PendingMachoNotification], id_type: &str) -> usize { notifs.iter().filter(|n| n.event == "OnItemChange" && n.id_type == id_type).count() }

fn industry_mat_qty(notifs: &[PendingMachoNotification], mat_type_id: u32) -> Vec<i64> {
    notifs.iter().filter(|n| n.event == "OnIndustryMaterials").filter_map(|n| match n.args.get(1) {
        Some(PyRep::Dict(entries)) => entries.iter().find_map(|(k, v)| (k == &PyRep::Integer(i64::from(mat_type_id))).then(|| v.as_i64()).flatten()),
        _ => None,
    }).collect()
}

#[test]
fn character_scene_or_ship_transition_predicate_covers_all_edges() {
    let cid = 90_000_001_i64;
    let chk = |b: Option<&CharacterRecord>, a: Option<&CharacterRecord>| {
        is_character_scene_or_ship_transition(cid, &HashMap::from([(cid, (b, a))]))
    };
    let base = CharacterRecord {
        character_id: cid, user_id: 1, solar_system_id: 30_000_142,
        station_id: Some(60_003_760), structure_id: None, active_ship_id: 100_001, ..Default::default()
    };
    let in_struct = CharacterRecord { station_id: None, structure_id: Some(1_030_000_001), ..base.clone() };
    let in_space = CharacterRecord { station_id: None, structure_id: None, ..base.clone() };
    assert!(!is_character_scene_or_ship_transition(cid, &HashMap::new()));
    assert!(chk(None, Some(&base)) && chk(Some(&base), None) && !chk(Some(&base), Some(&base)));
    assert!(chk(Some(&base), Some(&CharacterRecord { solar_system_id: 30_000_144, ..base.clone() })));
    assert!(chk(Some(&base), Some(&CharacterRecord { station_id: Some(60_003_761), ..base.clone() })));
    assert!(chk(Some(&base), Some(&in_struct)));
    assert!(chk(Some(&in_struct), Some(&CharacterRecord { structure_id: Some(1_030_000_002), ..in_struct.clone() })));
    assert!(!chk(Some(&base), Some(&CharacterRecord { active_ship_id: 100_002, ..base.clone() })));
    assert!(!chk(Some(&in_struct), Some(&CharacterRecord { active_ship_id: 100_002, ..in_struct.clone() })));
    assert!(chk(Some(&in_space), Some(&CharacterRecord { active_ship_id: 100_002, ..in_space.clone() })));
}

#[tokio::test]
async fn transition_suppression_docked_ship_swaps_and_post_jump_active_ship_scoping() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_a, ship_b, structure_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_600_010_i64, 992_600_011_i64, 1_030_000_992_601_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_a);
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    let (cap_st, cap_sr, cap_sp, mut_id, cargo_id, contra_id) =
        (992_600_030_i64, 992_600_032_i64, 992_600_034_i64, 992_600_040_i64, 992_600_041_i64, 992_600_042_i64);
    ctx.db.with_mut_snapshot(|s| {
        for rec in [
            mk_item(ship_b, 587, pilot_id, station_id, InventoryFlag::Hangar, 1, true),
            mk_item(cap_st, 670, pilot_id, station_id, InventoryFlag::Hangar, 1, true),
            mk_item(cap_sr, 670, pilot_id, structure_id, InventoryFlag::Hangar, 1, true),
            mk_item(mut_id, 34, pilot_id, station_id, InventoryFlag::Hangar, 200, false),
        ] { s.items.insert(rec.item_id, rec); }
        s.characters.get_mut(&pilot_id).expect("pilot").active_ship_id = cap_st;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Docked station & structure `active_ship_id`-only swaps project capsule tombstones both in
    //    dispatch and outside RPC, and subsequent Commit 2 hangar edits in dispatch are NOT blocked.
    run_dispatch_2tx(
        &ctx,
        "docked_st_swap", |tx| { tx.snap.characters.get_mut(&pilot_id).expect("p").active_ship_id = ship_a; tx.snap.remove_system_cleanup_item(cap_st); Ok(()) },
        "docked_st_hangar_c2", |tx| { tx.snap.items.get_mut(&mut_id).expect("mut").quantity = 120; Ok(()) },
    ).await;
    let st_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&st_n, "OnItemChange"), item_change_notifs(&st_n)[0].args[0].dbrow_get("locationID")), (2, Some(&PyRep::Integer(6))));

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).expect("p");
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.active_ship_id = cap_sr;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    ctx.db.tx("outside_sr_swap", |tx| -> Result<(), eve_db::TxError> {
        tx.snap.characters.get_mut(&pilot_id).expect("p").active_ship_id = ship_b;
        tx.snap.remove_system_cleanup_item(cap_sr);
        Ok(())
    }).expect("sr swap");
    let sr_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&sr_n, "OnItemChange"), item_change_notifs(&sr_n)[0].args[0].dbrow_get("locationID")), (1, Some(&PyRep::Integer(6))));

    // 2. Scene transitions & in-space `active_ship_id` suppress transition commit + Commit 2 active ship edits,
    //    while Commit 2 non-ship cargo insert / post-jump contraband confiscation still project.
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).expect("p");
        ch.structure_id = None;
        ch.station_id = None;
        ch.active_ship_id = cap_sp;
        s.items.insert(cap_sp, mk_item(cap_sp, 670, pilot_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        s.items.insert(ship_b, mk_item(ship_b, 587, pilot_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        s.items.insert(contra_id, mk_item(contra_id, 34, pilot_id, ship_b, InventoryFlag::Cargo, 15, false));
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    run_dispatch_2tx(
        &ctx,
        "space_ship_swap", |tx| { tx.snap.characters.get_mut(&pilot_id).expect("p").active_ship_id = ship_b; tx.snap.remove_system_cleanup_item(cap_sp); Ok(()) },
        "space_c2_ship_edit", |tx| { tx.snap.items.get_mut(&ship_b).expect("ship_b").flag = InventoryFlag::Hangar; Ok(()) },
    ).await;
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 0);

    run_dispatch_2tx(
        &ctx,
        "stargate_jump", |tx| { tx.snap.characters.get_mut(&pilot_id).expect("p").solar_system_id = sys_id + 1; Ok(()) },
        "post_jump_contra_and_cargo", |tx| {
            tx.snap.remove_system_cleanup_item(contra_id);
            tx.snap.items.get_mut(&ship_b).expect("ship_b").location_id = i64::from(sys_id + 1);
            tx.snap.items.insert(cargo_id, mk_item(cargo_id, 34, pilot_id, ship_b, InventoryFlag::Cargo, 42, false));
            Ok(())
        },
    ).await;
    let mut jump_ids: Vec<(i64, Option<i64>)> = item_change_notifs(&ctx.drain_notifications_for_char(pilot_id)).into_iter()
        .map(|n| (n.args[0].dbrow_get("itemID").and_then(PyRep::as_i64).unwrap_or(0), n.args[0].dbrow_get("locationID").and_then(PyRep::as_i64)))
        .collect();
    jump_ids.sort_unstable();
    assert_eq!(jump_ids, vec![(cargo_id, Some(ship_b)), (contra_id, Some(6))]);

    // 3. Outside-RPC transition + char create/delete + offline pilot emit 0 notifications.
    let (new_cid, new_uid) = (90_992_600_i64, 992_600_u64);
    let base_ch = ctx.db.with_snapshot(|s| s.characters[&pilot_id].clone());
    ctx.db.with_mut_snapshot(|s| {
        s.accounts.insert(format!("tr_{new_uid}"), AccountRecord {
            user_id: new_uid, username: format!("tr_{new_uid}"), password_token: "t".into(),
            role: 0, banned_until_unix: None, ban_reason: None,
        });
    });
    ctx.db.acquire_online_lock(new_uid, 9_315, Some(new_cid), true, 1_700_000_900).expect("lock");
    ctx.db.tx("outside_sys_and_create", |tx| -> Result<(), eve_db::TxError> {
        tx.snap.characters.get_mut(&pilot_id).expect("p").solar_system_id = sys_id;
        tx.snap.items.insert(992_600_050, mk_item(992_600_050, 34, pilot_id, ship_b, InventoryFlag::Cargo, 2, false));
        tx.snap.characters.insert(new_cid, CharacterRecord {
            character_id: new_cid, user_id: new_uid, station_id: Some(station_id), structure_id: None, ..base_ch
        });
        tx.snap.items.insert(992_600_051, mk_item(992_600_051, 34, new_cid, station_id, InventoryFlag::Hangar, 7, false));
        Ok(())
    }).expect("outside sys/create");
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty() && ctx.drain_notifications_for_char(new_cid).is_empty());

    ctx.db.release_online_lock(uid, 9_310);
    ctx.db.tx("outside_offline_and_delete", |tx| -> Result<(), eve_db::TxError> {
        tx.snap.characters.remove(&new_cid);
        tx.snap.items.insert(992_600_052, mk_item(992_600_052, 34, pilot_id, ship_b, InventoryFlag::Cargo, 3, false));
        tx.snap.items.insert(992_600_053, mk_item(992_600_053, 34, new_cid, station_id, InventoryFlag::Hangar, 3, false));
        Ok(())
    }).expect("offline+del");
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty() && ctx.drain_notifications_for_char(new_cid).is_empty());
    ctx.db.release_online_lock(new_uid, 9_315);
}

#[tokio::test]
async fn commit_industry_materials_causal_order_queue_cap_and_singleton_bpo() {
    const MAT_TYPE: u32 = 38;
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, monitor_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 992_990_010_i64, 77_940_001_i64);
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        save_industry_material_monitors(snap, &BTreeMap::from([(monitor_id, IndustryMaterialMonitorSub {
            char_id: pilot_id, owner_id: pilot_id, location_id: station_id, flag_id: InventoryFlag::Hangar as i64,
        })]));
    });
    let (mat_id, bp_type_id, bp_id) = (992_990_020_i64, *ctx.sde.blueprints.keys().next().expect("bp"), 992_990_021_i64);

    // 1. Insert -> update (outside RPC) -> delete emit exact causal order OnItemChange -> OnIndustryMaterials -> OnMultipleItemChange.
    run_dispatch_tx(&ctx, "ind_insert", |tx| { tx.snap.items.insert(mat_id, mk_item(mat_id, MAT_TYPE, pilot_id, station_id, InventoryFlag::Hangar, 1500, false)); Ok(()) }).await;
    let ins = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(ins.iter().map(|n| n.event.as_str()).collect::<Vec<_>>(), ["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]);
    assert_eq!((ins[1].args[0].clone(), industry_mat_qty(&ins, MAT_TYPE)), (PyRep::Integer(monitor_id), vec![1500]));

    ctx.db.tx("outside_ind_update", |tx| -> Result<(), eve_db::TxError> { tx.snap.items.get_mut(&mat_id).expect("mat").quantity = 500; Ok(()) }).expect("upd");
    let upd = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(upd.iter().map(|n| n.event.as_str()).collect::<Vec<_>>(), ["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]);
    assert_eq!(industry_mat_qty(&upd, MAT_TYPE), vec![500]);

    run_dispatch_tx(&ctx, "ind_delete", |tx| { tx.snap.remove_system_cleanup_item(mat_id); Ok(()) }).await;
    let del = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(del.iter().map(|n| n.event.as_str()).collect::<Vec<_>>(), ["OnItemChange", "OnIndustryMaterials", "OnMultipleItemChange"]);
    assert_eq!(industry_mat_qty(&del, MAT_TYPE), vec![0]);

    // 2. Singleton BPO flag change in monitored hangar emits 0 OnIndustryMaterials; unlocking + consume omits BPO typeID.
    ctx.db.with_mut_snapshot(|s| {
        let mut bp = mk_item(bp_id, bp_type_id, pilot_id, station_id, InventoryFlag::Hangar, 1, true);
        bp.blueprint_runs_remaining = Some(-1);
        s.items.insert(bp_id, bp);
        s.items.insert(mat_id, mk_item(mat_id, MAT_TYPE, pilot_id, station_id, InventoryFlag::Hangar, 500, false));
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    run_dispatch_tx(&ctx, "lock_bpo", |tx| { tx.snap.items.get_mut(&bp_id).expect("bp").flag = InventoryFlag::Locked; Ok(()) }).await;
    let lock_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&lock_n, "OnIndustryMaterials"), ev_cnt(&lock_n, "OnBlueprintsUpdated")), (0, 1));

    run_dispatch_tx(&ctx, "unlock_bpo_and_consume", |tx| {
        tx.snap.items.get_mut(&bp_id).expect("bp").flag = InventoryFlag::Hangar;
        tx.snap.items.get_mut(&mat_id).expect("mat").quantity = 300;
        Ok(())
    }).await;
    let unlock_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((industry_mat_qty(&unlock_n, MAT_TYPE), industry_mat_qty(&unlock_n, bp_type_id).is_empty()), (vec![300], true));

    // 3. Large commit (> MAX_PENDING_NOTIFICATIONS_PER_CHAR) preserves OnItemChange & coalesced OnIndustryMaterials before final OnMultipleItemChange.
    let base_qty: i64 = ctx.db.with_snapshot(|s| s.items.values()
        .filter(|i| i.owner_id == pilot_id && i.location_id == station_id && i.flag == InventoryFlag::Hangar && i.type_id == MAT_TYPE)
        .map(|i| i64::from(i.quantity)).sum());
    let item_count = MAX_PENDING_NOTIFICATIONS_PER_CHAR + 24;
    ctx.db.tx("large_ind_commit", |tx| -> Result<(), eve_db::TxError> {
        for idx in 0..item_count {
            let iid = 992_996_100 + idx as i64;
            tx.snap.items.insert(iid, mk_item(iid, MAT_TYPE, pilot_id, station_id, InventoryFlag::Hangar, 10, false));
        }
        Ok(())
    }).expect("large commit");
    let big = ctx.drain_notifications_for_char(pilot_id);
    let ind_pos = big.iter().position(|n| n.event == "OnIndustryMaterials").expect("OnIndustryMaterials kept");
    let last_multi = big.iter().rposition(|n| n.event == "OnMultipleItemChange").expect("trailing multi");
    assert_eq!(big.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(ev_cnt(&big, "OnItemChange") >= MAX_PENDING_NOTIFICATIONS_PER_CHAR / 2 - 1);
    assert!(ind_pos > 0 && ind_pos < last_multi && big[ind_pos].seq_id > big[ind_pos - 1].seq_id && big[ind_pos].seq_id < big[last_multi].seq_id);
    assert_eq!(industry_mat_qty(&big, MAT_TYPE), vec![base_qty + (item_count as i64) * 10]);

    // 4. 20k batch cap retains exact trailing MAX_PENDING_NOTIFICATIONS_PER_CHAR entries.
    let batch: Vec<_> = (0..20_000_i64)
        .map(|i| assemble_pending_notification(pilot_id, Some(1), "OnItemChange", "charid", vec![PyRep::Integer(i)], i)).collect();
    enqueue_or_stage_pending_batch(&*ctx.pending_notifications, &ctx.notify_tx, &batch);
    let queued = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(queued.len(), MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert_eq!((queued[0].args.clone(), queued[MAX_PENDING_NOTIFICATIONS_PER_CHAR - 1].args.clone()), (vec![PyRep::Integer(20_000 - MAX_PENDING_NOTIFICATIONS_PER_CHAR as i64)], vec![PyRep::Integer(19_999)]));
}

#[tokio::test]
async fn commit_bpc_tombstone_effects_suppression_and_generalized_char_shadow_convergence() {
    let ctx = test_ctx();
    let (pilot_id, station_id, sys_id, ship_id, other_ship_id, cont_id) =
        (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 993_910_010_i64, 993_910_011_i64, 993_910_012_i64);
    let other_station = ctx.sde.stations.keys().copied().find(|&id| id != station_id).expect("other station");
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let implant_type = ctx.sde.types.values().find(|t| t.category_id == 20).map(|t| t.type_id).unwrap_or(9899);
    let (bpc_id, mod_id, charge_id, fuel_id, ore_id, drone_id, imp_id, boost_id, remote_id, inactive_id, box_item_id, locked_id, probe_id, resv_id, exp_id) = (
        993_910_020_i64, 993_910_021_i64, 993_910_022_i64, 993_910_023_i64, 993_910_024_i64,
        993_910_025_i64, 993_910_026_i64, 993_910_027_i64, 993_910_028_i64, 993_910_029_i64,
        993_910_030_i64, 993_910_031_i64, 993_910_032_i64, 993_910_033_i64, 993_910_034_i64,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(other_ship_id, mk_item(other_ship_id, 587, pilot_id, station_id, InventoryFlag::Hangar, 1, true));
        s.items.insert(cont_id, mk_item(cont_id, 17_366, pilot_id, station_id, InventoryFlag::Hangar, 1, true));
        let mut bpc = mk_item(bpc_id, bp_type_id, pilot_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        bpc.blueprint_runs_remaining = Some(3);
        s.items.insert(bpc_id, bpc);
        s.items.insert(mod_id, mk_item(mod_id, 487, pilot_id, station_id, InventoryFlag::Hangar, 1, true));
        s.items.insert(charge_id, mk_item(charge_id, 179, pilot_id, ship_id, InventoryFlag::Cargo, 50, false));
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);

    // 1. Deleted BPC tombstone emits quantity=0 (never -2) + OnBlueprintsUpdated and converges ShadowClient.
    let mut shadow_bpc = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));
    run_dispatch_tx(&ctx, "delete_bpc", |tx| { tx.snap.remove_system_cleanup_item(bpc_id); Ok(()) }).await;
    let bpc_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(item_change_notifs(&bpc_n)[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(0)));
    assert_eq!((item_change_notifs(&bpc_n)[0].args[0].dbrow_get("locationID"), ev_cnt(&bpc_n, "OnBlueprintsUpdated")), (Some(&PyRep::Integer(6)), 1));
    for n in &bpc_n { shadow_bpc.apply_notification(&n.event, &n.args); }
    shadow_bpc.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 2. Fitted lifecycle (fit -> unfit -> fit+load -> outside-RPC swap/consume -> destroy with
    //    `push_tx_fitted_item_trashed` loc=10/flag=147 + co-deleted charge loc=6) + reserve/effect suppression.
    run_dispatch_tx(&ctx, "fit_mod", |tx| { let m = tx.snap.items.get_mut(&mod_id).expect("m"); m.location_id = ship_id; m.flag = InventoryFlag::HiSlot0; Ok(()) }).await;
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 1);
    run_dispatch_tx(&ctx, "unfit_mod", |tx| { let m = tx.snap.items.get_mut(&mod_id).expect("m"); m.location_id = station_id; m.flag = InventoryFlag::Hangar; Ok(()) }).await;
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 1);
    run_dispatch_tx(&ctx, "fit_and_load", |tx| {
        let m = tx.snap.items.get_mut(&mod_id).expect("m"); m.location_id = ship_id; m.flag = InventoryFlag::HiSlot0;
        tx.snap.items.get_mut(&charge_id).expect("c").flag = InventoryFlag::HiSlot1;
        Ok(())
    }).await;
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 2);

    ctx.db.tx("outside_swap_and_charge_tick", |tx| -> Result<(), eve_db::TxError> {
        tx.snap.items.get_mut(&mod_id).expect("m").flag = InventoryFlag::HiSlot1;
        tx.snap.items.get_mut(&charge_id).expect("c").quantity = 47;
        Ok(())
    }).expect("outside fitted");
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 2);

    run_dispatch_tx(&ctx, "destroy_fitted_and_charge", |tx| {
        tx.snap.remove_system_cleanup_item(mod_id);
        tx.snap.remove_system_cleanup_item(charge_id);
        ServerContext::push_tx_fitted_item_trashed(tx.effects, &ctx.sde, pilot_id, mod_id, 487, 1, ship_id, InventoryFlag::HiSlot1 as i64);
        Ok(())
    }).await;
    let fit_del = ctx.drain_notifications_for_char(pilot_id);
    let by_item = |id: i64| item_change_notifs(&fit_del).into_iter().filter(|n| n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(id))).collect::<Vec<_>>();
    let (mod_ev, chg_ev) = (by_item(mod_id), by_item(charge_id));
    assert_eq!((mod_ev.len(), mod_ev[0].args[0].dbrow_get("locationID"), mod_ev[0].args[0].dbrow_get("flagID")), (1, Some(&PyRep::Integer(10)), Some(&PyRep::Integer(147))));
    assert_eq!((chg_ev.len(), chg_ev[0].args[0].dbrow_get("locationID")), (1, Some(&PyRep::Integer(6))));

    let _guard = ctx.active_fuel_reserves.begin_reserve(db_ptr_key(&ctx.db), [resv_id]);
    ctx.db.tx("outside_reserved", |tx| -> Result<(), eve_db::TxError> { tx.snap.items.insert(resv_id, mk_item(resv_id, 34, pilot_id, station_id, InventoryFlag::Hangar, 50, false)); Ok(()) }).expect("resv");
    drop(_guard);
    assert!(ctx.drain_notifications_for_char(pilot_id).is_empty());

    let exp_item = mk_item(exp_id, 34, pilot_id, station_id, InventoryFlag::Hangar, 60, false);
    ctx.db.tx("outside_explicit_effect", |tx| -> Result<(), eve_db::TxError> {
        tx.snap.items.insert(exp_id, exp_item.clone());
        tx.effects.notify_character(pilot_id, "OnItemChange", "charid", vec![super::delta_projection::build_inv_item_dbrow(&exp_item, &ctx.sde, ""), PyRep::Dict(vec![])]);
        Ok(())
    }).expect("explicit");
    assert_eq!(ev_cnt(&ctx.drain_notifications_for_char(pilot_id), "OnItemChange"), 1);

    // 3. Generalized character items + in-place ME/TE + explicit `tx.effects` `OnBlueprintsUpdated`/`OnItemsChanged`
    //    + markerless `tokio::spawn` probe + drone launch/scoop converge `ShadowClient`.
    let mut shadow = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));
    let batch_items = vec![
        mk_item(fuel_id, 16_274, pilot_id, ship_id, InventoryFlag::SpecializedFuelBay, 100, false),
        mk_item(ore_id, 1230, pilot_id, ship_id, InventoryFlag::SpecializedOreHold, 250, false),
        mk_item(drone_id, 2454, pilot_id, ship_id, InventoryFlag::DroneBay, 1, true),
        mk_item(imp_id, implant_type, pilot_id, pilot_id, InventoryFlag::Implant, 1, true),
        mk_item(boost_id, 28_672, pilot_id, pilot_id, InventoryFlag::Booster, 1, true),
        mk_item(inactive_id, 34, pilot_id, other_ship_id, InventoryFlag::Cargo, 40, false),
        mk_item(box_item_id, 35, pilot_id, cont_id, InventoryFlag::None, 80, false),
        mk_item(remote_id, 34, pilot_id, other_station, InventoryFlag::Hangar, 50, false),
        mk_item(locked_id, bp_type_id, pilot_id, station_id, InventoryFlag::Locked, 1, true),
    ];
    run_dispatch_tx(&ctx, "char_all_items_batch", |tx| { for rec in &batch_items { tx.snap.items.insert(rec.item_id, rec.clone()); } Ok(()) }).await;
    let batch_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&batch_n, "OnItemChange"), ev_cnt(&batch_n, "OnBlueprintsUpdated")), (batch_items.len(), 1));
    for n in &batch_n { shadow.apply_notification(&n.event, &n.args); }
    shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    run_dispatch_tx(&ctx, "bp_in_place_me_te", |tx| {
        let bp = tx.snap.items.get_mut(&locked_id).expect("bp");
        bp.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 10.0);
        bp.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 20.0);
        Ok(())
    }).await;
    let me_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&me_n, "OnItemChange"), ev_cnt(&me_n, "OnBlueprintsUpdated")), (0, 1));

    run_dispatch_tx(&ctx, "bp_and_strip_explicit_tx_effects", |tx| {
        let bp = tx.snap.items.get_mut(&locked_id).expect("bp");
        bp.blueprint_runs_remaining = Some(4);
        let row = super::delta_projection::build_inv_item_dbrow(bp, &ctx.sde, "");
        tx.effects.notify_character(pilot_id, "OnItemsChanged", "charid", vec![PyRep::List(vec![row]), PyRep::Dict(vec![]), PyRep::None]);
        tx.effects.notify_character(pilot_id, "OnBlueprintsUpdated", "ownerID", vec![PyRep::Integer(pilot_id)]);
        Ok(())
    }).await;
    let eff_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!((ev_cnt(&eff_n, "OnItemChange"), ev_cnt(&eff_n, "OnBlueprintsUpdated")), (0, 1));

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot_id).expect("p").station_id = None;
        let sh = s.items.get_mut(&ship_id).expect("sh"); sh.location_id = i64::from(sys_id); sh.flag = InventoryFlag::None;
    });
    let _ = ctx.drain_notifications_for_char(pilot_id);
    let mut shadow_sp = ShadowClient::from_world(pilot_id, &ctx.db, Some(&ctx.sde));
    let db_clone = Arc::clone(&ctx.db);
    tokio::spawn(async move {
        db_clone.tx("markerless_probe", |tx| -> Result<(), eve_db::TxError> {
            tx.snap.items.insert(probe_id, mk_item(probe_id, 30_013, pilot_id, ship_id, InventoryFlag::Cargo, 8, false));
            Ok(())
        }).expect("markerless probe");
    }).await.expect("spawn");
    let probe_n = ctx.drain_notifications_for_char(pilot_id);
    assert_eq!(probe_n.iter().map(|n| n.event.as_str()).collect::<Vec<_>>(), ["OnItemChange", "OnMultipleItemChange"]);
    for n in &probe_n { shadow_sp.apply_notification(&n.event, &n.args); }

    run_dispatch_tx(&ctx, "launch_drone", |tx| { let d = tx.snap.items.get_mut(&drone_id).expect("d"); d.location_id = i64::from(sys_id); d.flag = InventoryFlag::None; Ok(()) }).await;
    for n in ctx.drain_notifications_for_char(pilot_id) { shadow_sp.apply_notification(&n.event, &n.args); }
    run_dispatch_tx(&ctx, "scoop_drone", |tx| { let d = tx.snap.items.get_mut(&drone_id).expect("d"); d.location_id = ship_id; d.flag = InventoryFlag::DroneBay; Ok(()) }).await;
    for n in ctx.drain_notifications_for_char(pilot_id) { shadow_sp.apply_notification(&n.event, &n.args); }
    shadow_sp.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
}

#[tokio::test]
async fn cross_owner_char_corp_upwell_npc_and_role_gated_industry_projection() {
    const MAT_TYPE: u32 = 38;
    let ctx = test_ctx();
    let (p1_id, p2_id, station_id, structure_id, sys_id, ship1, ship2, corp1_id, corp2_id) = (
        90_000_001_i64, 90_000_002_i64, 60_003_760_i64, 1_030_000_995_001_i64,
        30_000_142_u32, 993_960_010_i64, 993_960_011_i64, 98_000_601_i64, 98_000_602_i64,
    );
    setup_online_pilot_in_station(&ctx, p1_id, station_id, sys_id, ship1);
    setup_online_pilot_in_station(&ctx, p2_id, station_id, sys_id, ship2);
    let bp_type_id = *ctx.sde.blueprints.keys().next().expect("bp type");
    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().expect("npc corp");
    let (xfer_id, xfer_bp_id, corp_mat_id, corp_bp_id, deliv_id, locked_id, upwell_hi_id, upwell_fuel_id, c2c_id, npc_hangar_id, npc_space_id) = (
        993_960_101_i64, 993_960_102_i64, 993_960_103_i64, 993_960_104_i64,
        993_960_105_i64, 993_960_106_i64, 993_960_107_i64, 993_960_108_i64,
        993_960_109_i64, 993_960_110_i64, 993_960_111_i64,
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(xfer_id, mk_item(xfer_id, 34, p1_id, station_id, InventoryFlag::Hangar, 75, false));
        s.items.insert(xfer_bp_id, mk_item(xfer_bp_id, bp_type_id, p1_id, station_id, InventoryFlag::Locked, 1, true));
    });
    let _ = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));

    // 1. Cross-owner Char <-> Char (stack + BPO) notifies both recipients and converges both ShadowClients.
    let (mut sh1, mut sh2) = (ShadowClient::from_world(p1_id, &ctx.db, Some(&ctx.sde)), ShadowClient::from_world(p2_id, &ctx.db, Some(&ctx.sde)));
    run_dispatch_tx(&ctx, "char_to_char_xfer", |tx| {
        tx.snap.items.get_mut(&xfer_id).expect("x").owner_id = p2_id;
        let bp = tx.snap.items.get_mut(&xfer_bp_id).expect("bp"); bp.owner_id = p2_id; bp.flag = InventoryFlag::Hangar;
        Ok(())
    }).await;
    let (n1_cc, n2_cc) = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));
    assert_eq!((ev_cnt(&n1_cc, "OnItemChange"), ev_cnt(&n2_cc, "OnItemChange"), ev_cnt(&n1_cc, "OnBlueprintsUpdated"), ev_cnt(&n2_cc, "OnBlueprintsUpdated")), (2, 2, 1, 1));
    for n in &n1_cc { sh1.apply_notification(&n.event, &n.args); }
    for n in &n2_cc { sh2.apply_notification(&n.event, &n.args); }
    sh1.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
    sh2.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

    // 2. Corp & Upwell projection, Section 4C Char->Corp & Corp->Char transfer isolation, role-gated
    //    corp OnIndustryMaterials, partial transition delivery, and Corp->Corp transfer.
    make_corp_with_members(&ctx, corp1_id, p1_id, &[p1_id, p2_id]);
    ctx.db.with_mut_snapshot(|s| {
        let mut corp2 = CorporationRecord::new(corp2_id, "Corp Two", "C2", p2_id);
        corp2.members.insert(p2_id);
        s.corporations.insert(corp2_id, corp2);
        s.items.insert(corp_mat_id, mk_item(corp_mat_id, MAT_TYPE, corp1_id, station_id, InventoryFlag::CorpSAG1, 500, false));
        s.items.insert(c2c_id, mk_item(c2c_id, 34, corp1_id, station_id, InventoryFlag::CorpSAG1, 10, false));
        save_industry_material_monitors(s, &BTreeMap::from([
            (77_601, IndustryMaterialMonitorSub { char_id: p1_id, owner_id: corp1_id, location_id: station_id, flag_id: InventoryFlag::CorpSAG1 as i64 }),
            (77_602, IndustryMaterialMonitorSub { char_id: p2_id, owner_id: corp1_id, location_id: station_id, flag_id: InventoryFlag::CorpSAG1 as i64 }),
        ]));
    });
    let _ = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));

    ctx.db.tx("outside_corp_and_upwell", |tx| -> Result<(), eve_db::TxError> {
        let x = tx.snap.items.get_mut(&xfer_id).expect("x"); x.owner_id = corp1_id; x.flag = InventoryFlag::CorpSAG1;
        tx.snap.items.insert(corp_bp_id, mk_item(corp_bp_id, bp_type_id, corp1_id, station_id, InventoryFlag::CorpSAG1, 1, true));
        tx.snap.items.get_mut(&corp_mat_id).expect("m").quantity = 200;
        tx.snap.items.insert(deliv_id, mk_item(deliv_id, 35, corp1_id, station_id, InventoryFlag::CorpDeliveries, 40, false));
        tx.snap.items.insert(locked_id, mk_item(locked_id, 36, corp1_id, station_id, InventoryFlag::Locked, 12, false));
        tx.snap.items.insert(upwell_hi_id, mk_item(upwell_hi_id, 27_148, corp1_id, structure_id, InventoryFlag::HiSlot0, 1, true));
        tx.snap.items.insert(upwell_fuel_id, mk_item(upwell_fuel_id, 16_274, corp1_id, structure_id, InventoryFlag::StructureFuel, 500, false));
        Ok(())
    }).expect("outside corp");
    let (np1, np2) = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));
    assert_eq!(
        (
            ic_cnt(&np1, "charid"),
            ic_cnt(&np1, "corpid"),
            ic_cnt(&np2, "charid"),
            ic_cnt(&np2, "corpid"),
        ),
        (0, 7, 0, 7)
    );
    assert_eq!(np1.iter().filter(|n| n.event == "OnBlueprintsUpdated" && n.args == vec![PyRep::Integer(corp1_id)]).count(), 1);
    assert_eq!(np2.iter().filter(|n| n.event == "OnBlueprintsUpdated" && n.args == vec![PyRep::Integer(corp1_id)]).count(), 1);
    assert_eq!((industry_mat_qty(&np1, MAT_TYPE), industry_mat_qty(&np1, bp_type_id).is_empty(), ev_cnt(&np2, "OnIndustryMaterials")), (vec![200], true, 0));

    run_dispatch_tx(&ctx, "corp_bp_lock", |tx| { tx.snap.items.get_mut(&corp_bp_id).expect("bp").flag = InventoryFlag::Locked; Ok(()) }).await;
    assert_eq!((ev_cnt(&ctx.drain_notifications_for_char(p1_id), "OnIndustryMaterials"), ev_cnt(&ctx.drain_notifications_for_char(p2_id), "OnBlueprintsUpdated")), (0, 1));
    run_dispatch_tx(&ctx, "corp_bp_me", |tx| { tx.snap.items.get_mut(&corp_bp_id).expect("bp").dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 7.0); Ok(()) }).await;
    let me_p2 = ctx.drain_notifications_for_char(p2_id);
    assert_eq!((ev_cnt(&me_p2, "OnItemChange"), ev_cnt(&me_p2, "OnBlueprintsUpdated")), (0, 1));
    let _ = ctx.drain_notifications_for_char(p1_id);

    run_dispatch_tx(&ctx, "corp_to_char", |tx| { let x = tx.snap.items.get_mut(&xfer_id).expect("x"); x.owner_id = p1_id; x.flag = InventoryFlag::Hangar; Ok(()) }).await;
    let (c2p_1, c2p_2) = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));
    assert_eq!((ic_cnt(&c2p_1, "charid"), ic_cnt(&c2p_1, "corpid"), ic_cnt(&c2p_2, "corpid")), (1, 0, 1));

    run_dispatch_tx(&ctx, "partial_corp_transition", |tx| {
        tx.snap.characters.get_mut(&p1_id).expect("p1").solar_system_id = sys_id + 1;
        tx.snap.items.get_mut(&upwell_fuel_id).expect("f").quantity = 400;
        Ok(())
    }).await;
    assert_eq!((ev_cnt(&ctx.drain_notifications_for_char(p1_id), "OnItemChange"), ic_cnt(&ctx.drain_notifications_for_char(p2_id), "corpid")), (0, 1));

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&p2_id).expect("p2").corporation_id = corp2_id; });
    let _ = ctx.drain_notifications_for_char(p2_id);
    run_dispatch_tx(&ctx, "corp_to_corp", |tx| { tx.snap.items.get_mut(&c2c_id).expect("c2c").owner_id = corp2_id; Ok(()) }).await;
    assert_eq!((ic_cnt(&ctx.drain_notifications_for_char(p1_id), "corpid"), ic_cnt(&ctx.drain_notifications_for_char(p2_id), "corpid")), (1, 1));

    // 3. NPC corp scoping: CorpSAG1 + CorpSAG1 -> Hangar exit project corpid to member p1 (0 to p2),
    //    while non-hangar non-blueprint NPC space item emits 0 broadcasts.
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&p1_id).expect("p1").corporation_id = npc_corp_id; });
    let _ = (ctx.drain_notifications_for_char(p1_id), ctx.drain_notifications_for_char(p2_id));
    run_dispatch_tx(&ctx, "npc_corp_scoping", |tx| {
        tx.snap.items.insert(npc_hangar_id, mk_item(npc_hangar_id, 34, npc_corp_id, station_id, InventoryFlag::CorpSAG1, 10, false));
        tx.snap.items.insert(npc_space_id, mk_item(npc_space_id, 34, npc_corp_id, i64::from(sys_id), InventoryFlag::None, 5, false));
        Ok(())
    }).await;
    assert_eq!(ic_cnt(&ctx.drain_notifications_for_char(p1_id), "corpid"), 1);
    assert!(ctx.drain_notifications_for_char(p2_id).is_empty());

    run_dispatch_tx(&ctx, "npc_corp_hangar_exit", |tx| { tx.snap.items.get_mut(&npc_hangar_id).expect("npc").flag = InventoryFlag::Hangar; Ok(()) }).await;
    assert_eq!(ic_cnt(&ctx.drain_notifications_for_char(p1_id), "corpid"), 1);
}
