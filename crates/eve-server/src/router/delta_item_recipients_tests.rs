//! Unit tests for Step 3a extended recipient rules (Rules 3–5), holder resolution,
//! Section 4D cross-id-type ledger suppression, and Section 4E telemetry (`#931`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{ChangeSet, CorporationRecord, InventoryFlag, InventoryItemRecord, RowChange};

use super::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
};
use super::delta_item_compare::{
    push_dispatch_rpc_method_guard, set_item_compare_override_for_test, with_compare_tick_scope,
};
use super::delta_item_recipients::{
    resolve_item_holder, ItemHolderKind, RULE_CORPORATION, RULE_FORMER_MEMBER,
    RULE_HOLDER_ACTING_PILOT, RULE_HOLDER_OWNER, RULE_OWNER, RULE_OWNERLESS,
};
use super::delta_item_switch::{
    projected_item_switch_summary, ItemBeforeAfter, ProjectedItemSwitchConfig,
};
use super::delta_item_switch_tests::{setup_online_pilot_in_station, test_ctx};
use super::dispatch_active_char::with_active_dispatch_char;
use super::ServerContext;

fn make_corp_with_members(
    ctx: &ServerContext,
    corp_id: i64,
    ceo_id: i64,
    members: &[i64],
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut c = CorporationRecord {
            corporation_id: corp_id,
            ceo_id,
            name: format!("Corp {corp_id}"),
            ticker: "T3A".into(),
            ..Default::default()
        };
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
async fn telemetry_hand_sent_and_projected_by_rule_record_expected_fields_and_bounds() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());
    let actor_id = 90_000_001_i64;
    let holder_owner_id = 90_000_002_i64;
    let corp_id = 98_931_701_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let actor_ship = 995_931_001_i64;
    let holder_ship = 995_931_002_i64;
    let space_box = 995_931_003_i64;

    setup_online_pilot_in_station(&ctx, actor_id, station_id, sys_id, actor_ship);
    setup_online_pilot_in_station(&ctx, holder_owner_id, station_id, sys_id, holder_ship);
    make_corp_with_members(&ctx, corp_id, holder_owner_id, &[holder_owner_id]);

    // Pre-seed space container (created before the commit so Rule 5 applies) and ownerless item inside it.
    let space_item_id = 995_931_010_i64;
    let ship_held_id = 995_931_011_i64;
    let corp_item_id = 995_931_012_i64;
    let own_item_id = 995_931_013_i64;
    let office_id = 995_931_014_i64;
    let corp_closing_id = 98_931_702_i64;
    let former_member_id = 96_931_703_i64;
    let former_ship = 995_931_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&actor_id].clone();
        ch.character_id = former_member_id;
        ch.user_id = former_member_id as u64;
        ch.name = format!("Former {former_member_id}");
        snap.characters.insert(former_member_id, ch);
    });
    setup_online_pilot_in_station(&ctx, former_member_id, station_id, sys_id, former_ship);
    make_corp_with_members(&ctx, corp_closing_id, former_member_id, &[former_member_id]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            space_box,
            InventoryItemRecord {
                item_id: space_box,
                type_id: 23, // Cargo Container
                owner_id: holder_owner_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            space_item_id,
            InventoryItemRecord {
                item_id: space_item_id,
                type_id: 34,
                owner_id: 0,
                location_id: space_box,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_held_id,
            InventoryItemRecord {
                item_id: ship_held_id,
                type_id: 34,
                owner_id: holder_owner_id,
                location_id: holder_ship,
                flag: InventoryFlag::FleetHangar,
                quantity: 50,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            office_id,
            InventoryItemRecord {
                item_id: office_id,
                type_id: 27,
                owner_id: corp_closing_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Trigger all 6 projected recipient rules across commits with active dispatch actor = actor_id:
    with_active_dispatch_char(Some(actor_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(&ctx, "invbroker.MultiAdd");
            ctx.db
                .tx("rules_dispatch_commit", |tx| -> Result<(), eve_db::TxError> {
                    // 1. Owner + 5b. Holder acting pilot (actor_id on own item inside station vs holder ship item)
                    tx.snap.items.insert(
                        own_item_id,
                        InventoryItemRecord {
                            item_id: own_item_id,
                            type_id: 34,
                            owner_id: actor_id,
                            location_id: station_id,
                            flag: InventoryFlag::Hangar,
                            quantity: 10,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    // 2. Corporation
                    tx.snap.items.insert(
                        corp_item_id,
                        InventoryItemRecord {
                            item_id: corp_item_id,
                            type_id: 34,
                            owner_id: corp_id,
                            location_id: station_id,
                            flag: InventoryFlag::CorpSAG1,
                            quantity: 20,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    // 3. Former member (close corp_closing_id + delete office + move member out)
                    tx.snap.items.remove(&office_id);
                    tx.snap.corporations.remove(&corp_closing_id);
                    if let Some(ch) = tx.snap.characters.get_mut(&former_member_id) {
                        ch.corporation_id = 1_000_002;
                    }
                    // 4. Ownerless (existing owner=0 inside station? Use item in space_box -> triggers ownerless for actor + holder owner for box owner!)
                    if let Some(it) = tx.snap.items.get_mut(&space_item_id) {
                        it.quantity = 90;
                        it.version += 1;
                    }
                    // 5. Ship holder item owned by holder_owner_id -> holder_owner gets Rule 1 ("owner"), actor gets "holder acting pilot";
                    //    And mutating space_box itself doesn't trigger holder; let's mutate ship_held_id owned by holder_owner_id:
                    if let Some(it) = tx.snap.items.get_mut(&ship_held_id) {
                        it.quantity = 45;
                        it.version += 1;
                    }
                    Ok(())
                })
                .expect("rules commit");
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    // Trigger unmatched hand-written events across dispatch (actor=1 & actor=0, holder=ship & holder=none),
    // tick (origin=tick, holder=space), and outside (origin=outside) + bounded sample check (>16).
    let hw_ship_item = InventoryItemRecord {
        item_id: 995_931_020,
        type_id: 34,
        owner_id: actor_id,
        location_id: actor_ship,
        flag: InventoryFlag::Cargo,
        quantity: 7,
        version: 1,
        ..Default::default()
    };
    let hw_station_item = InventoryItemRecord {
        item_id: 995_931_021,
        type_id: 34,
        owner_id: holder_owner_id,
        location_id: station_id,
        flag: InventoryFlag::Hangar,
        quantity: 8,
        version: 1,
        ..Default::default()
    };
    let hw_space_ownerless = InventoryItemRecord {
        item_id: space_item_id,
        type_id: 34,
        owner_id: 0,
        location_id: space_box,
        flag: InventoryFlag::Cargo,
        quantity: 80,
        version: 3,
        ..Default::default()
    };

    with_active_dispatch_char(Some(actor_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(&ctx, "invbroker.Add");
            ctx.notify_item_record_change(actor_id, &hw_ship_item, vec![(9, eve_proto::PyRep::Integer(5))]);
            ctx.notify_item_record_change(
                holder_owner_id,
                &hw_station_item,
                vec![(9, eve_proto::PyRep::Integer(4))],
            );
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    with_compare_tick_scope(async {
        ctx.notify_item_record_change(
            actor_id,
            &hw_space_ownerless,
            vec![(9, eve_proto::PyRep::Integer(90))],
        );
    })
    .await;

    for idx in 0..20_i64 {
        let mut outside_item = hw_station_item.clone();
        outside_item.item_id = 995_931_100 + idx;
        ctx.notify_item_record_change(
            holder_owner_id,
            &outside_item,
            vec![(9, eve_proto::PyRep::Integer(1))],
        );
    }

    let summary = projected_item_switch_summary(&ctx);
    assert!(summary.projected_by_rule.get(RULE_OWNER).copied().unwrap_or(0) >= 2);
    assert_eq!(summary.projected_by_rule.get(RULE_CORPORATION).copied(), Some(1));
    assert_eq!(summary.projected_by_rule.get(RULE_FORMER_MEMBER).copied(), Some(1));
    assert_eq!(summary.projected_by_rule.get(RULE_OWNERLESS).copied(), Some(1));
    assert_eq!(summary.projected_by_rule.get(RULE_HOLDER_ACTING_PILOT).copied(), Some(1));
    assert_eq!(summary.projected_by_rule.get(RULE_HOLDER_OWNER).copied(), Some(1));
    assert_eq!(
        summary
            .projected_by_rule_rpc
            .get(RULE_OWNERLESS)
            .and_then(|m| m.get("invbroker.MultiAdd"))
            .copied(),
        Some(1),
        "projected_by_rule_rpc={:?}",
        summary.projected_by_rule_rpc
    );

    let total_hand_sent: u64 = summary.hand_sent_by_call_site.values().sum();
    assert_eq!(total_hand_sent, 23, "hand_sent_by_call_site={:?}", summary.hand_sent_by_call_site);
    assert_eq!(summary.sample_hand_sent.len(), 16, "sample_hand_sent must remain bounded at 16");
    let s0 = &summary.sample_hand_sent[0];
    assert!(s0.contains("site="), "missing site=: {s0}");
    assert!(s0.contains("rpc=invbroker.Add"), "missing rpc=: {s0}");
    assert!(s0.contains("origin=dispatch"), "missing origin=dispatch: {s0}");
    assert!(s0.contains("class="), "missing class=: {s0}");
    assert!(s0.contains("holder=ship"), "missing holder=ship: {s0}");
    assert!(s0.contains("actor=1"), "missing actor=1: {s0}");

    let s1 = &summary.sample_hand_sent[1];
    assert!(s1.contains("origin=dispatch") && s1.contains("holder=none") && s1.contains("actor=0"), "{s1}");
    let s2 = &summary.sample_hand_sent[2];
    assert!(s2.contains("origin=tick") && s2.contains("holder=space") && s2.contains("actor=0"), "{s2}");
    let s3 = &summary.sample_hand_sent[3];
    assert!(s3.contains("origin=outside") && s3.contains("holder=none") && s3.contains("actor=0"), "{s3}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_outside_rpc_ownerless_and_same_commit_ownerless_container_emit_zero() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 995_932_001_i64;
    setup_online_pilot_in_station(&ctx, pilot_id, station_id, sys_id, ship_id);

    // Pre-seed an ownerless item outside RPC, then mutate it outside RPC.
    let loose_ownerless = 995_932_010_i64;
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));
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
    set_item_compare_override_for_test(&ctx, None, None);
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
            let _rpc = push_dispatch_rpc_method_guard(&ctx, "dungeon.Trigger");
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
    let summary = projected_item_switch_summary(&ctx);
    assert_eq!(summary.projected_by_rule.get(RULE_OWNERLESS).copied(), None);
    assert_eq!(summary.projected_by_rule.get(RULE_HOLDER_ACTING_PILOT).copied(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step3a_ledger_suppresses_either_id_type_without_stale_event_drop() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());
    let pilot_p = 90_000_001_i64;
    let outsider_q = 90_000_002_i64;
    let corp_id = 98_931_703_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_p = 995_933_001_i64;
    let ship_q = 995_933_002_i64;

    setup_online_pilot_in_station(&ctx, pilot_p, station_id, sys_id, ship_p);
    setup_online_pilot_in_station(&ctx, outsider_q, station_id, sys_id, ship_q);
    make_corp_with_members(&ctx, corp_id, pilot_p, &[pilot_p]);

    // Pre-seed corp fuel-bay item owned by corp_id (flag 133 / SpecializedFuelBay -> projects under corpid).
    let core_item_id = 995_933_010_i64;
    let core_pre = InventoryItemRecord {
        item_id: core_item_id,
        type_id: 56_201,
        owner_id: pilot_p,
        location_id: ship_p,
        flag: InventoryFlag::Cargo,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };
    let mut core_post = core_pre.clone();
    core_post.owner_id = corp_id;
    core_post.flag = InventoryFlag::QuantumCoreRoom;
    core_post.version = 2;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(core_item_id, core_pre.clone());
    });
    let _ = ctx.drain_notifications_for_char(pilot_p);

    // Dispatch: pilot_p becomes member of corp_id, move updates core to corp + deletes core,
    // and legacy hand-written emit targets Character(pilot_p) ("charid") with col 2 == pilot_p
    // AND legacy hand-written emit targets Character(outsider_q) (NO ledger fact for q).
    with_active_dispatch_char(Some(pilot_p), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(&ctx, "invbroker.Add");
            ctx.db
                .tx("core_step_1_2", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(core_item_id, core_post.clone());
                    Ok(())
                })
                .expect("step 1");
            ctx.db
                .tx("core_consume", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.remove(&core_item_id);
                    Ok(())
                })
                .expect("consume");
            // Hand-written follow-up for pilot_p as Character(pilot_p) ("charid") MUST be suppressed by corpid fact!
            ctx.notify_item_record_change(
                pilot_p,
                &core_post,
                vec![(2, eve_proto::PyRep::Integer(pilot_p)), (4, eve_proto::PyRep::Integer(5))],
            );
            // Hand-written emit for outsider_q MUST NOT be stale-dropped since projection has no fact for outsider_q.
            ctx.notify_item_record_change(
                outsider_q,
                &core_post,
                vec![(4, eve_proto::PyRep::Integer(5))],
            );
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;
    })
    .await;

    let n_p: Vec<_> = ctx
        .drain_notifications_for_char(pilot_p)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(n_p.len(), 1, "pilot_p must get only projected consumption event (no hand-written follow-up): {n_p:?}");
    let n_q: Vec<_> = ctx
        .drain_notifications_for_char(outsider_q)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert_eq!(n_q.len(), 1, "outsider_q with no ledger fact must still receive its hand-written emit: {n_q:?}");
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
            before: Some(Box::new(deleted_box_rec)),
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
