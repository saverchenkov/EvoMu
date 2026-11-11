#![allow(unused_imports)]
//! Extra branch-coverage and mutation-killing unit tests for `character_skill_ops`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::character_skill_ops::{
    abort_training, get_respec_info, get_skill_changes_for_isis, handle,
    handle_omega_weekly_free_sp_offer, migrate_obsolete_skills_for_character,
    post_inventory_fitting_hook, purchase_skills_direct_for_char,
    verify_in_space_container_proximity,
};
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    SkillQueueEntry, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[cfg(test)]
#[path = "character_skill_training_and_fitting_unit_tests.rs"]
mod character_skill_training_and_fitting_unit_tests;

pub(super) fn unauth_session() -> SessionState {
    SessionState::new(999_999)
}

pub(super) fn dispatch(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, sess, svc, method, args, kw)
}

pub(super) fn mk_order(
    order_id: i64,
    type_id: u32,
    is_bid: bool,
    is_npc: bool,
    vol_remaining: i32,
    price_cents: i64,
) -> MarketOrderRecord {
    MarketOrderRecord {
        order_id,
        owner_id: 1,
        station_id: 60_003_760,
        region_id: 10_000_002,
        type_id,
        is_bid,
        price: IskCents(price_cents),
        vol_entered: 100,
        vol_remaining,
        min_volume: 1,
        is_npc,
        escrow: IskCents(0),
    }
}

pub(super) fn mk_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    is_bpc: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: true,
        is_blueprint_copy: is_bpc,
        ..Default::default()
    }
}

#[test]
fn obsolete_skills_boundary_ids_and_sp_clamping() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;

    // Missing character or character without skills entry returns (0, 0.0)
    assert_eq!(migrate_obsolete_skills_for_character(&ctx, 999_999_999), (0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&cid);
    });
    assert_eq!(migrate_obsolete_skills_for_character(&ctx, cid), (0, 0.0));

    // Only valid published skills -> no obsolete candidates
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 2,
                skill_points: 1_415.0,
            },
        );
    });
    assert_eq!(migrate_obsolete_skills_for_character(&ctx, cid), (0, 0.0));

    // Seed boundary obsolete IDs (3374..=3379, 12376, 12383, 12385..=12387) and non-skill type 34 (Tritanium)
    ctx.db.with_mut_snapshot(|snap| {
        let map = snap.skills.entry(cid).or_default();
        for &obs_id in &[34_u32, 3374, 3379, 12376, 12383, 12385, 12387] {
            map.insert(
                obs_id,
                TrainedSkillRecord {
                    type_id: obs_id,
                    level: 1,
                    skill_points: 1_000.0,
                },
            );
        }
        // Set character total SP lower than refunded SP to verify `.max(0.0)` clamping
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.skill_points = 2_500.0;
            ch.free_skill_points = 500.0;
        }
        snap.skill_history.remove(&cid);
    });

    let (removed, refunded) = migrate_obsolete_skills_for_character(&ctx, cid);
    assert_eq!(removed, 7);
    assert_eq!(refunded, 7_000.0);

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&cid).unwrap();
        assert_eq!(ch.skill_points, 0.0);
        assert_eq!(ch.free_skill_points, 7_500.0);
        let hist = snap.skill_history.get(&cid).unwrap();
        assert_eq!(hist.len(), 7);
        assert!(hist
            .iter()
            .all(|h| h.event_type_id == 38 && h.skill_level == 0 && h.absolute_points == 0.0));
        let map = snap.skills.get(&cid).unwrap();
        assert!(map.contains_key(&3300) && !map.contains_key(&3374));
    });

    // Second call after migration finds no remaining obsolete skills
    assert_eq!(migrate_obsolete_skills_for_character(&ctx, cid), (0, 0.0));
}

#[test]
fn purchase_skills_direct_kwargs_variants_and_market_filtering() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let mut unauth = unauth_session();
    let kw = HashMap::new();

    assert!(dispatch(&ctx, &mut unauth, "skillMgr2", "PurchaseSkills", &[], &kw)
        .unwrap()
        .is_err());

    for bad_arg in [PyRep::Integer(0), PyRep::String("bad".into())] {
        let err = dispatch(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[bad_arg], &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    let (sk1, sk2) = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().clear();
        let mut iter = ctx
            .sde
            .types
            .values()
            .filter(|t| t.category_id == 16 && t.published && t.base_price > 0.0)
            .map(|t| t.type_id);
        let a = iter.next().unwrap_or(3300);
        let b = iter.next().unwrap_or(3301);
        snap.market_orders.retain(|_, o| o.type_id != a && o.type_id != b);
        (a, b)
    });

    // Seed ignored market orders (bid, non-npc, zero vol_remaining) -> still SkillUnavailableForPurchase
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders
            .insert(991_001, mk_order(991_001, sk1, true, true, 10, 1_000_00));
        snap.market_orders
            .insert(991_002, mk_order(991_002, sk1, false, false, 10, 1_000_00));
        snap.market_orders
            .insert(991_003, mk_order(991_003, sk1, false, true, 0, 1_000_00));
    });
    assert_eq!(
        purchase_skills_direct_for_char(&ctx, cid, &[sk1])
            .unwrap_err()
            .code,
        "SkillUnavailableForPurchase"
    );

    // Seed two valid NPC orders for sk1 (20_000 ISK and 10_000 ISK) and one for sk2 (5_000 ISK)
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders
            .insert(991_004, mk_order(991_004, sk1, false, true, 10, 20_000_00));
        snap.market_orders
            .insert(991_005, mk_order(991_005, sk1, false, true, 10, 10_000_00));
        snap.market_orders
            .insert(991_006, mk_order(991_006, sk2, false, true, 10, 5_000_00));
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(13_000_00);
        }
    });

    // Purchase sk1 via kwargs "typeID" (single Integer) with exact balance -> succeeds with 0 remaining balance
    let mut kw_single = HashMap::new();
    kw_single.insert("typeID".into(), PyRep::Integer(sk1 as i64));
    let res1 = dispatch(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[], &kw_single)
        .unwrap()
        .unwrap();
    assert_eq!(res1, PyRep::List(vec![PyRep::Integer(sk1 as i64)]));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&cid).unwrap().balance.0),
        0
    );

    // Purchase [sk1, sk2] via kwargs "typeIDs" (Set with negative/zero filtered out)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(6_500_00);
        }
    });
    let mut kw_set = HashMap::new();
    kw_set.insert(
        "typeIDs".into(),
        PyRep::Set(vec![
            PyRep::Integer(-5),
            PyRep::Integer(0),
            PyRep::Integer(sk1 as i64),
            PyRep::Integer(sk2 as i64),
        ]),
    );
    let res2 = dispatch(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[], &kw_set)
        .unwrap()
        .unwrap();
    assert_eq!(
        res2,
        PyRep::List(vec![PyRep::Integer(sk1 as i64), PyRep::Integer(sk2 as i64)])
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&cid).unwrap().balance.0),
        0
    );

    assert_eq!(
        purchase_skills_direct_for_char(&ctx, 777_777_777, &[sk1])
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
}

#[test]
fn omega_weekly_free_sp_kwargs_subscription_expiry_and_min_grant() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    let mut kw1 = HashMap::new();
    kw1.insert("offer_id".into(), PyRep::Integer(1005));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2;
        }
        snap.service_state_blobs.remove(&format!("omega_weekly_sp_claim:{uid}"));
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[], &kw1)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Expired subscription blocks Omega weekly claim even if clone_grade == 2
    let now = ctx.now_unix();
    crate::services::account_services::set_subscription_expiry(&ctx, uid, now.saturating_sub(10));
    crate::services::quasar_auth_user::with_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.insert(uid, now.saturating_sub(10));
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[], &kw1)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_omega_weekly_free_sp_offer(&ctx, &mut sess, 1005, 15_000, true)
            .unwrap_err()
            .code,
        "OmegaRequired"
    );

    // Active future subscription and sp_grant <= 0 clamped to 1
    crate::services::account_services::set_subscription_expiry(&ctx, uid, now + 86_400);
    crate::services::quasar_auth_user::with_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.insert(uid, now + 86_400);
    });
    let rep = handle_omega_weekly_free_sp_offer(&ctx, &mut sess, 1005, 0, true).unwrap();
    assert_eq!(
        rep.dict_get("freeSkillPointsGranted"),
        Some(&PyRep::Integer(1))
    );

    let mut ghost_sess = auth_session(99, 777_666_555, 98_000_001, 30_000_142);
    assert_eq!(
        handle_omega_weekly_free_sp_offer(&ctx, &mut ghost_sess, 1005, 15_000, false).unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_omega_weekly_free_sp_offer(&ctx, &mut ghost_sess, 1005, 15_000, true)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
}
