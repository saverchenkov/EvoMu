//! Unit tests for Step 3a extended recipient rules (Rules 3–5), holder resolution,
//! Section 4D cross-id-type ledger suppression, and Section 4E telemetry (`#931`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{
    AccountRecord, ChangeSet, CorporationRecord, InventoryFlag, InventoryItemRecord, RowChange,
};

use super::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
};
use super::delta_item_switch_report::{push_dispatch_rpc_method_guard, with_compare_tick_scope};
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
async fn telemetry_hand_sent_and_projected_by_rule_record_expected_fields_and_bounds() {
    let ctx = test_ctx();
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::disabled());
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
        snap.accounts.insert(
            format!("user_{former_member_id}"),
            AccountRecord {
                user_id: former_member_id as u64,
                username: format!("user_{former_member_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
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

    // Enable projection switches only after seeding test fixtures so fixture writes emit no telemetry.
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());

    // Trigger rules 1, 2, 4, 5 under acting pilot = actor_id, and rule 3 under acting pilot = former_member_id:
    with_active_dispatch_char(Some(actor_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "invbroker.MultiAdd".to_string(),
            );
            ctx.db
                .tx("rules_dispatch_commit", |tx| -> Result<(), eve_db::TxError> {
                    // 1. Owner
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
                    // 4. Ownerless (space_box -> actor gets ownerless, box owner gets holder owner)
                    if let Some(it) = tx.snap.items.get_mut(&space_item_id) {
                        it.quantity = 90;
                        it.version += 1;
                    }
                    // 5. Ship holder item owned by holder_owner_id -> holder_owner gets owner, actor gets holder acting pilot.
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

    // 3. Former member (close corp_closing_id + delete office + move leaving pilot out under acting pilot = former_member_id).
    with_active_dispatch_char(Some(former_member_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "corpRegistry.KickOutMember".to_string(),
            );
            ctx.db
                .tx("corp_close_commit", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.remove(&office_id);
                    tx.snap.corporations.remove(&corp_closing_id);
                    if let Some(ch) = tx.snap.characters.get_mut(&former_member_id) {
                        ch.corporation_id = 1_000_002;
                    }
                    Ok(())
                })
                .expect("corp close commit");
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
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "invbroker.Add".to_string(),
            );
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
    assert!(
        summary.projected_by_rule.get(RULE_OWNER).copied().unwrap_or(0) >= 2,
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
    assert_eq!(
        summary.projected_by_rule.get(RULE_CORPORATION).copied(),
        Some(1),
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
    assert_eq!(
        summary.projected_by_rule.get(RULE_FORMER_MEMBER).copied(),
        Some(1),
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
    assert_eq!(
        summary.projected_by_rule.get(RULE_OWNERLESS).copied(),
        Some(1),
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
    assert_eq!(
        summary
            .projected_by_rule
            .get(RULE_HOLDER_ACTING_PILOT)
            .copied(),
        Some(1),
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
    assert_eq!(
        summary.projected_by_rule.get(RULE_HOLDER_OWNER).copied(),
        Some(1),
        "projected_by_rule={:?}, projected_by_rule_rpc={:?}",
        summary.projected_by_rule,
        summary.projected_by_rule_rpc
    );
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
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "dungeon.Trigger".to_string(),
            );
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
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "invbroker.Add".to_string(),
            );
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
    assert_eq!(
        n_p.len(),
        2,
        "pilot_p must get only the 2 projected corpid events (move + consume), zero hand-written follow-up: {n_p:?}"
    );
    assert!(
        n_p.iter().all(|n| n.id_type == "corpid"),
        "hand-written charid follow-up must be suppressed by corpid ledger fact: {n_p:?}"
    );
    assert_eq!(
        n_p.last().and_then(|n| n.args[0].dbrow_get("locationID").and_then(eve_proto::PyRep::as_i64)),
        Some(6),
        "last OnItemChange must be consumption tombstone: {n_p:?}"
    );
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
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::disabled());
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

    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());

    // 1. Ship destruction commit: `victim_ship` is deleted alongside destroying `victim_mod` and
    //    moving `victim_cargo` into a new `wreck_id` -> killer must receive 0 `OnItemChange`.
    with_active_dispatch_char(Some(killer_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "dogmaIM.Activate".to_string(),
            );
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
    let after_kill_summary = projected_item_switch_summary(&ctx);
    assert_eq!(
        after_kill_summary.projected_by_rule.get(RULE_HOLDER_ACTING_PILOT).copied(),
        None,
        "destroyed ship holder must not trigger RULE_HOLDER_ACTING_PILOT: {:?}",
        after_kill_summary.projected_by_rule
    );

    // 2. Live non-owned ship bay + live space container mutations still emit RULE_HOLDER_ACTING_PILOT to actor.
    with_active_dispatch_char(Some(killer_id), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "invbroker.Add".to_string(),
            );
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
    let after_live_summary = projected_item_switch_summary(&ctx);
    assert_eq!(
        after_live_summary.projected_by_rule.get(RULE_HOLDER_ACTING_PILOT).copied(),
        Some(2),
        "projected_by_rule={:?}",
        after_live_summary.projected_by_rule
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

    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());

    // 1. Destroying a foreign space container/deployable AND deleting its enclosed contents
    //    (`ItemHolderKind::Space`, `after_opt.is_none()`) must emit zero `OnItemChange` to actor.
    with_active_dispatch_char(Some(acting_pilot), async {
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "dogmaIM.Activate".to_string(),
            );
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
            let _rpc = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                "entity.CmdSalvage".to_string(),
            );
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
    let summary = projected_item_switch_summary(&ctx);
    assert_eq!(
        summary.projected_by_rule.get(RULE_HOLDER_ACTING_PILOT).copied(),
        Some(1),
        "projected_by_rule={:?}",
        summary.projected_by_rule
    );
}
