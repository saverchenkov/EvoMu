//! Unit tests for `character_skill_ops` (obsolete skills, direct skill purchase, Omega weekly SP, strip fitting).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    SkillQueueEntry, TrainedSkillRecord,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn obsolete_skill_migration_and_direct_purchase() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let kw = HashMap::new();

    assert!(is_obsolete_skill(&ctx.sde, 3374));
    assert!(!is_obsolete_skill(&ctx.sde, 3300));

    // Seed an obsolete skill (3374) with 25_000 SP
    let before_free_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.free_skill_points).unwrap_or(0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3374,
            TrainedSkillRecord {
                type_id: 3374,
                level: 3,
                skill_points: 25_000.0,
            },
        );
    });
    let (removed_cnt, refunded_sp) = migrate_obsolete_skills_for_character(&ctx, cid);
    assert_eq!(removed_cnt, 1);
    assert_eq!(refunded_sp, 25_000.0);
    let after_free_sp = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.free_skill_points).unwrap_or(0.0));
    assert_eq!(after_free_sp, before_free_sp + 25_000.0);

    // Direct skill purchase error branches & 1.30x NPC price calculation
    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[999_999_999])
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[34])
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );

    // Find a published skill not yet trained by cid and seed an NPC sell order for it
    let untrained_skill: u32 = ctx.db.with_mut_snapshot(|snap| {
        let trained = snap.skills.get(&cid);
        let sk_id = ctx
            .sde
            .types
            .values()
            .find(|t| {
                t.category_id == 16
                    && t.published
                    && t.base_price > 0.0
                    && !trained.is_some_and(|m| m.contains_key(&t.type_id))
            })
            .map(|t| t.type_id)
            .unwrap_or(3301);
        // Remove any existing NPC order for sk_id first to verify SkillUnavailableForPurchase
        snap.market_orders.retain(|_, o| o.type_id != sk_id);
        sk_id
    });

    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[untrained_skill])
            .unwrap_err()
            .code,
        "SkillUnavailableForPurchase"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            990_001,
            MarketOrderRecord {
                order_id: 990_001,
                owner_id: 1,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: untrained_skill,
                is_bid: false,
                price: IskCents(10_000_00), // 10,000 ISK -> 13,000 ISK with 30% markup
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(5_000_00);
        }
    });

    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[untrained_skill])
            .unwrap_err()
            .code,
        "NotEnoughISK"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(50_000_00);
        }
    });

    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let bought_rep = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "PurchaseSkills",
        &[PyRep::List(vec![PyRep::Integer(untrained_skill as i64)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        bought_rep,
        PyRep::List(vec![PyRep::Integer(untrained_skill as i64)])
    );
    let bal_after = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.balance.0).unwrap());
    assert_eq!(bal_after, 37_000_00);

    // Re-purchasing already injected skill costs 0 ISK
    let (_, cost_retry, _) = purchase_skills_direct_for_char(&ctx, cid, &[untrained_skill]).unwrap();
    assert_eq!(cost_retry, 0);
}

#[tokio::test]
async fn omega_weekly_sp_abort_respec_strip_fitting_and_container_hooks() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Non-1005 storeManager offer returns None
    assert!(handle(
        &ctx,
        &mut sess,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(999)],
        &kw,
    )
    .is_none());

    // Alpha character cannot claim weekly Omega SP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 1;
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "storeManager",
            "can_purchase_offer",
            &[PyRep::Integer(1005)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    let sm_svc = format!("store{}", "Manager");
    let omega_err = handle(
        &ctx,
        &mut sess,
        &sm_svc,
        "buy_offer",
        &[PyRep::Integer(1005)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(omega_err.code, "OmegaRequired");

    // Upgrade to Omega and claim weekly SP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2;
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            &sm_svc,
            "can_purchase_offer",
            &[PyRep::Integer(1005)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let claim_rep = handle(
        &ctx,
        &mut sess,
        &sm_svc,
        "buy_offer",
        &[PyRep::Integer(1005)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        claim_rep.dict_get("freeSkillPointsGranted"),
        Some(&PyRep::Integer(15_000))
    );
    let dup_claim_err = handle(
        &ctx,
        &mut sess,
        &sm_svc,
        "buy_offer",
        &[PyRep::Integer(1005)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_claim_err.code, "WeeklyOfferAlreadyClaimed");

    // AbortTraining, GetRespecInfo, GetSkillChangesForISIS
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                started_at_unix: Some(1_000),
                duration_seconds: 500.0,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
            }],
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "AbortTraining", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(ctx.db.with_snapshot(|s| s.paused_skill_queues.contains(&cid)));

    let respec = handle(&ctx, &mut sess, "skillMgr2", "GetRespecInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(respec.dict_get("freeRespecs").is_some());
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetSkillChangesForISIS", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );
    // Applying free SP / completing training adds positive `(typeID, pointChange)` deltas for ISIS
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3399,
            eve_db::TrainedSkillRecord {
                type_id: 3399,
                level: 2,
                skill_points: 1415.0,
            },
        );
        snap.skill_history.entry(cid).or_default().push(SkillHistoryEntry {
            log_date_ft: now_ft,
            event_type_id: 37,
            skill_type_id: 3399,
            skill_level: 2,
            absolute_points: 1415.0,
        });
    });
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetSkillChangesForISIS", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3399),
            PyRep::Real(1415.0),
        ])])
    );

    // StripFitting: undocked error, contract lock error, and stripping modules/drones while keeping rigs
    assert_eq!(
        handle(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(ship_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CannotStripFittingInSpace"
    );

    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = cid;
            ship.location_id = station_id;
        }
        snap.contracts.insert(
            701,
            ContractRecord {
                contract_id: 701,
                issuer_id: cid,
                status: "outstanding".into(),
                item_ids: vec![ship_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, "ship", "StripFitting", &[PyRep::Integer(ship_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLockedInContract"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.remove(&701);
        snap.items.insert(
            890_001,
            InventoryItemRecord {
                item_id: 890_001,
                type_id: 487,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            890_002,
            InventoryItemRecord {
                item_id: 890_002,
                type_id: 2454,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            890_003,
            InventoryItemRecord {
                item_id: 890_003,
                type_id: 31716,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let strip_res = handle(&ctx, &mut sess, "invbroker", "StripFitting", &[PyRep::Integer(ship_id)], &kw)
        .unwrap()
        .unwrap();
    let stripped_list = strip_res
        .dict_get("strippedItemIDs")
        .and_then(|v| v.as_list())
        .unwrap();
    assert!(stripped_list.contains(&PyRep::Integer(890_001)));
    assert!(stripped_list.contains(&PyRep::Integer(890_002)));
    assert!(!stripped_list.contains(&PyRep::Integer(890_003)));

    // Container proximity check (non-solar-system location returns Ok, docked returns NotInSameSolarSystem)
    assert!(verify_in_space_container_proximity(&ctx, cid, station_id, 890_001)
        .await
        .is_ok());
    assert_eq!(
        verify_in_space_container_proximity(&ctx, cid, 30_000_142, 890_001)
            .await
            .unwrap_err()
            .code,
        "NotInSameSolarSystem"
    );
    post_inventory_fitting_hook(&ctx, &sess, None, "Drop", &[]).await;

    // #549: StripFitting refuses impounded, blueprint-locked, and unpaid asset-safety ships/modules
    let locked_ship_id = 890_549_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            locked_ship_id,
            InventoryItemRecord {
                item_id: locked_ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, "ship", "StripFitting", &[PyRep::Integer(locked_ship_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ImpoundedItem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&locked_ship_id).unwrap().flag = InventoryFlag::Locked;
    });
    assert_eq!(
        handle(&ctx, &mut sess, "dogmaIM", "StripFitting", &[PyRep::Integer(locked_ship_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&locked_ship_id).unwrap().flag = InventoryFlag::Hangar;
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            890_999,
            crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: 890_999,
                owner_id: cid,
                is_corp: false,
                source_structure_id: 1_000_000_000_001,
                source_structure_name: "Keepstar".into(),
                solar_system_id: 30_000_142,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: Some(station_id),
                item_ids: vec![locked_ship_id],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: Some(station_id),
                auto_delivered: true,
                retrieval_fee_paid: false,
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle(&ctx, &mut sess, "ship", "StripFitting", &[PyRep::Integer(locked_ship_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "AssetSafetyLocked"
    );
}

#[test]
fn obsolete_skill_migration_notifications_split_login_and_session() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id).unwrap_or(1));
    ctx.db
        .acquire_online_lock(uid, 100, Some(cid), true, ctx.now_unix())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3374,
            TrainedSkillRecord {
                type_id: 3374,
                level: 2,
                skill_points: 10_000.0,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(cid);

    let (removed, refunded) = migrate_obsolete_skills_on_login(&ctx, cid);
    assert_eq!((removed, refunded), (1, 10_000.0));
    let login_notifs = ctx.drain_notifications_for_char(cid);
    assert!(!login_notifs.iter().any(|n| n.event == "OnServerSkillsRemoved"));
    assert!(!login_notifs.iter().any(|n| n.event == "OnSkillForcedRefresh"));
    assert!(login_notifs.iter().any(|n| n.event == "OnFreeSkillPointsChanged"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3374,
            TrainedSkillRecord {
                type_id: 3374,
                level: 2,
                skill_points: 10_000.0,
            },
        );
    });
    let (removed2, refunded2) = migrate_obsolete_skills_for_character(&ctx, cid);
    assert_eq!((removed2, refunded2), (1, 10_000.0));
    let session_notifs = ctx.drain_notifications_for_char(cid);
    assert!(!session_notifs.iter().any(|n| n.event == "OnServerSkillsRemoved"));
    assert!(session_notifs.iter().any(|n| n.event == "OnSkillForcedRefresh"));
    assert!(session_notifs.iter().any(|n| n.event == "OnFreeSkillPointsChanged"));
}
