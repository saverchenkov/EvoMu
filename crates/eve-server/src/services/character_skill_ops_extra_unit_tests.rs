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
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn unauth_session() -> SessionState {
    SessionState::new(999_999)
}

fn dispatch(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, sess, svc, method, args, kw)
}

fn mk_order(
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

fn mk_item(
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("subscription_expiry:{uid}"),
            now.saturating_sub(10).to_string(),
        );
    });
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

#[test]
fn abort_training_and_respec_info_branches() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let unauth = unauth_session();

    assert!(abort_training(&ctx, &unauth).is_err());
    assert!(get_respec_info(&ctx, &unauth).is_err());
    assert!(get_skill_changes_for_isis(&unauth).is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().remove(&99999);
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 99999,
                target_level: 1,
                started_at_unix: None,
                duration_seconds: 100.0,
                start_sp: 123.5,
                end_sp: 250.0,
            }],
        );
    });
    assert_eq!(abort_training(&ctx, &sess).unwrap(), PyRep::Bool(true));
    let head = ctx.db.with_snapshot(|s| s.skill_queues.get(&cid).unwrap()[0].clone());
    assert_eq!(head.start_sp, 123.5);
    assert!(head.started_at_unix.is_some());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.neural.bonus_remaps = 3;
            ch.neural.last_remap_unix = Some(1_700_000_000);
            ch.neural.remap_cooldown_until_unix = Some(1_731_536_000);
        }
    });
    let info = get_respec_info(&ctx, &sess).unwrap();
    assert_eq!(info.dict_get("freeRespecs"), Some(&PyRep::Integer(3)));
    assert_eq!(
        info.dict_get("lastRespecDate"),
        Some(&PyRep::Integer(1_700_000_000))
    );
    assert_eq!(
        info.dict_get("nextTimedRespec"),
        Some(&PyRep::Integer(1_731_536_000))
    );

    let ghost_sess = auth_session(99, 888_777_666, 98_000_001, 30_000_142);
    assert_eq!(
        get_respec_info(&ctx, &ghost_sess).unwrap_err().code,
        "CharacterNotFound"
    );
}

#[tokio::test]
async fn strip_fitting_validation_flags_and_container_hooks() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let struct_id = 1_040_000_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "shield_vulnerable".into(),
                ..Default::default()
            },
        );
    });

    // Structure without quantum core rejects StripFitting with QuantumCoreRequired
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "QuantumCoreRequired"
    );

    // Dock at station for ship validation branches
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    sess.ship_id = None;

    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut kw_ship = HashMap::new();
    kw_ship.insert("shipID".into(), PyRep::Integer(999_888_777));
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw_ship)
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = 90_000_002;
            ship.location_id = station_id;
        }
    });
    sess.ship_id = Some(ship_id);
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = cid;
            ship.location_id = struct_id;
        }
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotAtStation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = station_id;
        }
        snap.contracts.insert(
            702,
            ContractRecord {
                contract_id: 702,
                issuer_id: cid,
                status: "in_progress".into(),
                item_ids: vec![ship_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLockedInContract"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&702) {
            c.status = "finished".into();
        }
        snap.items.insert(
            891_001,
            mk_item(891_001, 487, cid, ship_id, InventoryFlag::LoSlot0, true),
        );
        snap.module_online_states.insert(891_001, true);
        snap.module_active_states.insert(891_001);
        snap.module_active_charges.insert(891_001, 891_002);
        snap.items.insert(
            891_003,
            mk_item(891_003, 487, cid, ship_id, InventoryFlag::SubSystemSlot0, false),
        );
    });

    let strip_ok = dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
        .unwrap()
        .unwrap();
    let stripped_ids = strip_ok
        .dict_get("strippedItemIDs")
        .and_then(|v| v.as_list())
        .unwrap();
    assert!(stripped_ids.contains(&PyRep::Integer(891_001)));
    assert!(!stripped_ids.contains(&PyRep::Integer(891_003)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_online_states.contains_key(&891_001));
        assert!(!snap.module_active_states.contains(&891_001));
        assert!(!snap.module_active_charges.contains_key(&891_001));
    });

    // In-space proximity checks (in-range vs out-of-range) and post_inventory_fitting_hook branches
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let cont_id = 892_001_i64;
    let cont_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 12)
        .map(|t| t.type_id)
        .unwrap_or(23);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            cont_id,
            mk_item(cont_id, cont_type_id, cid, sys_id as i64, InventoryFlag::None, false),
        );
        snap.sp6_lifecycle.space_object_meta.remove(&cont_id);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, radius, mode) in [
        (ship_id, 606_u32, glam::DVec3::ZERO, 50.0, MovementMode::Stop),
        (
            cont_id,
            cont_type_id,
            glam::DVec3::new(1_000.0, 0.0, 0.0),
            20.0,
            MovementMode::Rigid,
        ),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1.0e5,
                radius,
                speed_fraction: 0.0,
                mode,
            })
            .await;
    }

    assert!(verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id)
        .await
        .is_ok());

    post_inventory_fitting_hook(&ctx, &sess, None, "Drop", &[]).await;
    assert!(ctx
        .db
        .with_snapshot(|s| s.sp6_lifecycle.space_object_meta.contains_key(&cont_id)));

    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: cid,
        service: "invbroker".into(),
        object_id: cont_id,
        oid_str: "oid=(1, 1)".into(),
        ref_id: 1,
    };
    post_inventory_fitting_hook(
        &ctx,
        &sess,
        Some(&bound),
        "ValidateItemListCanBeOpened",
        &[PyRep::List(vec![PyRep::Integer(cont_id)])],
    )
    .await;
    post_inventory_fitting_hook(&ctx, &sess, None, "Activate", &[]).await;

    let _ = sys_h.remove_ball(cont_id).await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: cont_id,
            type_id: cont_type_id,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e3,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    assert_eq!(
        verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
}
