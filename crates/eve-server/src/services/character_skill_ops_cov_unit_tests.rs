//! Focused coverage and mutation-killing unit tests for `character_skill_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::quasar_payment_store_types::{save_store_catalog, StoreCatalog, StoreOffer};
use crate::sp6_lifecycle::PersistedSpaceObjectMeta;
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    SkillQueueEntry, TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{InvType, SdeRegistry};
use std::sync::Arc;

fn mk_skill_inv(type_id: u32, category_id: u32, published: bool, base_price: f64) -> InvType {
    InvType {
        type_id,
        group_id: 255,
        category_id,
        name: format!("Skill-{type_id}"),
        mass: 0.0,
        volume: 0.01,
        capacity: 0.0,
        radius: 1.0,
        base_price,
        published,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn tr_skill(type_id: u32, level: u8, skill_points: f64) -> TrainedSkillRecord {
    TrainedSkillRecord {
        type_id,
        level,
        skill_points,
    }
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

#[allow(clippy::too_many_arguments)]
fn mk_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
    singleton: bool,
    is_bpc: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton,
        is_blueprint_copy: is_bpc,
        ..Default::default()
    }
}

fn strip_err(ctx: &ServerContext, sess: &mut SessionState, kw: &HashMap<String, PyRep>) -> String {
    handle(ctx, sess, "dogmaIM", "StripFitting", &[], kw)
        .unwrap()
        .unwrap_err()
        .code
}

#[test]
fn obsolete_skill_ranges_core_skill_guard_and_sp_clamping() {
    let mut sde = SdeRegistry::default();
    for &(sk_id, expected) in &[
        (3373, false),
        (3374, true),
        (3379, true),
        (3380, false),
        (12375, false),
        (12376, true),
        (12377, false),
        (12382, false),
        (12383, true),
        (12384, false),
        (12385, true),
        (12387, true),
        (12388, false),
    ] {
        assert_eq!(is_obsolete_skill(&sde, sk_id), expected, "sk_id={sk_id}");
    }
    sde.types.insert(50_001, mk_skill_inv(50_001, 16, false, 10_000.0));
    sde.types.insert(50_002, mk_skill_inv(50_002, 4, true, 10_000.0));
    sde.types.insert(3300, mk_skill_inv(3300, 16, true, 10_000.0));
    assert!(is_obsolete_skill(&sde, 50_001));
    assert!(is_obsolete_skill(&sde, 50_002));
    assert!(!is_obsolete_skill(&sde, 3300));

    // Core skill guard: having ONLY 3300 (without 3327) short-circuits to (0, 0.0)
    let cid = 90_000_001_i64;
    let repo = || Arc::new(WorldRepository::with_demo_seed());
    let partial_ctx = ServerContext::with_sde_and_repository(Arc::new(sde.clone()), repo());
    partial_ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(3374, tr_skill(3374, 2, 5_000.0));
    });
    assert_eq!(migrate_obsolete_skills_for_character(&partial_ctx, cid), (0, 0.0));

    // Add 3327 so core guard passes; character absent from `snap.skills` -> (0, 0.0)
    sde.types.insert(3327, mk_skill_inv(3327, 16, true, 10_000.0));
    let synth_ctx = ServerContext::with_sde_and_repository(Arc::new(sde), repo());
    assert_eq!(migrate_obsolete_skills_for_character(&synth_ctx, 777_888_999), (0, 0.0));

    // Valid character with `skill_points < total_sp` clamps `skill_points` to 0.0 and logs event 38
    synth_ctx.db.with_mut_snapshot(|snap| {
        let map = snap.skills.entry(cid).or_default();
        map.clear();
        map.insert(3300, tr_skill(3300, 1, 250.0));
        for &obs in &[50_001_u32, 50_002, 3374] {
            map.insert(obs, tr_skill(obs, 2, 2_000.0));
        }
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.skill_points = 1_500.0;
            ch.free_skill_points = 100.0;
        }
        snap.skill_history.remove(&cid);
    });
    assert_eq!(migrate_obsolete_skills_for_character(&synth_ctx, cid), (3, 6_000.0));
    synth_ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&cid];
        assert_eq!(ch.skill_points, 0.0);
        assert_eq!(ch.free_skill_points, 6_100.0);
        assert_eq!(snap.skill_history[&cid].len(), 3);
        assert!(snap.skill_history[&cid].iter().all(|h| h.event_type_id == 38));
    });
    assert_eq!(migrate_obsolete_skills_for_character(&synth_ctx, cid), (0, 0.0));
}

#[test]
fn purchase_skills_drop_only_unpublished_kwargs_and_journal_verification() {
    let mut sde = SdeRegistry::default();
    let (sk_pub1, sk_pub2, sk_unpub, sk_drop) = (60_001_u32, 60_002, 60_003, 60_004);
    sde.types.insert(sk_pub1, mk_skill_inv(sk_pub1, 16, true, 10_000.0));
    sde.types.insert(sk_pub2, mk_skill_inv(sk_pub2, 16, true, 5_000.0));
    sde.types.insert(sk_unpub, mk_skill_inv(sk_unpub, 16, false, 10_000.0));
    sde.types.insert(sk_drop, mk_skill_inv(sk_drop, 16, true, 0.0));

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let cid = 90_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let mut unauth = SessionState::new(999);
    let empty_kw = HashMap::new();

    assert!(handle(&ctx, &mut sess, "", "", &[], &empty_kw).is_none());
    assert!(handle(&ctx, &mut unauth, "skillMgr2", "PurchaseSkills", &[], &empty_kw).unwrap().is_err());

    assert_eq!(purchase_skills_direct_for_char(&ctx, cid, &[sk_unpub]).unwrap_err().code, "InvalidSkillType");
    assert_eq!(purchase_skills_direct_for_char(&ctx, cid, &[sk_drop]).unwrap_err().code, "SkillUnavailableForPurchase");

    // Non-matching market orders (bid, non-npc, zero vol) ignored; min NPC order chosen
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().clear();
        snap.market_orders.insert(1, mk_order(1, sk_pub1, true, true, 10, 100));
        snap.market_orders.insert(2, mk_order(2, sk_pub1, false, false, 10, 100));
        snap.market_orders.insert(3, mk_order(3, sk_pub1, false, true, 0, 100));
        snap.market_orders.insert(4, mk_order(4, sk_pub1, false, true, 5, 20_000_00));
        snap.market_orders.insert(5, mk_order(5, sk_pub1, false, true, 5, 10_000_00));
        snap.market_orders.insert(6, mk_order(6, sk_pub2, false, true, 5, 5_000_00));
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(13_000_00);
    });

    // Purchase via single Integer kwarg `"typeID"` at exact `balance == cost` boundary
    let mut kw_tid = HashMap::new();
    kw_tid.insert("typeID".into(), PyRep::Integer(i64::from(sk_pub1)));
    let res = handle(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[], &kw_tid).unwrap().unwrap();
    assert_eq!(res, PyRep::List(vec![PyRep::Integer(i64::from(sk_pub1))]));

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].balance.0, 0);
        assert!(snap.wallet_journal.iter().any(|j| {
            j.character_id == cid
                && j.entry_type == "skill_purchase"
                && j.amount == IskCents(-13_000_00)
                && j.balance_after == IskCents(0)
        }));
        assert!(snap.skill_history[&cid].iter().any(|h| h.event_type_id == 36 && h.skill_type_id == sk_pub1));
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(6_500_00);
    });

    // Purchase via Set in `"typeIDs"` with already-injected sk_pub1 (0 ISK) + new sk_pub2 + invalid <=0 filtered
    let mut kw_set = HashMap::new();
    kw_set.insert(
        "typeIDs".into(),
        PyRep::Set(vec![
            PyRep::Integer(0),
            PyRep::Integer(-3),
            PyRep::Integer(i64::from(sk_pub1)),
            PyRep::Integer(i64::from(sk_pub2)),
        ]),
    );
    let res2 = handle(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[], &kw_set).unwrap().unwrap();
    assert_eq!(res2, PyRep::List(vec![PyRep::Integer(i64::from(sk_pub1)), PyRep::Integer(i64::from(sk_pub2))]));
    assert_eq!(purchase_skills_direct_for_char(&ctx, 777_666_555, &[sk_pub1]).unwrap_err().code, "CharacterNotFound");
}

#[test]
fn tuesday_downtime_boundaries_omega_catalog_override_abort_and_respec() {
    assert_eq!(current_tuesday_downtime_week_id(0), 0);
    assert_eq!(current_tuesday_downtime_week_id(471_599), 0);
    assert_eq!(current_tuesday_downtime_week_id(471_600), 1);
    assert_eq!(current_tuesday_downtime_week_id(471_600 + 604_799), 1);
    assert_eq!(current_tuesday_downtime_week_id(471_600 + 604_800), 2);

    let ctx = test_ctx();
    let (uid, cid) = (1_u64, 90_000_001_i64);
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    // When offer 1005 IS present in NES store catalog, `character_skill_ops::handle` returns None
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: Vec::new(),
                products: Vec::new(),
                offers: vec![StoreOffer {
                    id: 1005,
                    name: "Catalog 1005".into(),
                    description: String::new(),
                    category_id: 1,
                    price_plex: 10,
                    product_ids: Vec::new(),
                    targeted: false,
                    price_cents: None,
                    currency: None,
                }],
            },
        );
    });
    let mut kw_oid = HashMap::new();
    kw_oid.insert("offerID".into(), PyRep::Integer(1005));
    assert!(handle(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[], &kw_oid).is_none());

    // Remove catalog entry -> falls through to weekly Omega SP handler; test `exp == now` vs `now + 1`
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
        snap.service_state_blobs.remove(&format!("omega_weekly_sp_claim:{uid}"));
        snap.characters.get_mut(&cid).unwrap().clone_grade = 2;
    });
    crate::services::quasar_auth_user::with_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.insert(uid, now);
    });
    assert_eq!(
        handle_omega_weekly_free_sp_offer(&ctx, &mut sess, 1005, 15_000, true).unwrap_err().code,
        "OmegaRequired"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[], &kw_oid).unwrap().unwrap(),
        PyRep::Bool(false)
    );

    crate::services::quasar_auth_user::with_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.insert(uid, now + 1);
    });
    let granted = handle_omega_weekly_free_sp_offer(&ctx, &mut sess, 1005, 0, true).unwrap();
    assert_eq!(granted.dict_get("freeSkillPointsGranted"), Some(&PyRep::Integer(1)));

    // Missing character: false for check, CharacterNotFound for buy & GetRespecInfo
    let mut ghost = auth_session(99, 777_888_999, 98_000_001, 30_000_142);
    assert_eq!(handle_omega_weekly_free_sp_offer(&ctx, &mut ghost, 1005, 15_000, false).unwrap(), PyRep::Bool(false));
    assert_eq!(handle_omega_weekly_free_sp_offer(&ctx, &mut ghost, 1005, 15_000, true).unwrap_err().code, "CharacterNotFound");
    assert_eq!(get_respec_info(&ctx, &ghost).unwrap_err().code, "CharacterNotFound");

    // `abort_training` when head skill is absent from `snap.skills` preserves `head.start_sp`
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().remove(&88_888);
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 88_888,
                target_level: 1,
                started_at_unix: None,
                duration_seconds: 60.0,
                start_sp: 321.0,
                end_sp: 500.0,
            }],
        );
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.neural.bonus_remaps = 2;
        ch.neural.last_remap_unix = Some(1_700_000_000);
        ch.neural.remap_cooldown_until_unix = Some(1_731_536_000);
    });
    assert_eq!(abort_training(&ctx, &sess).unwrap(), PyRep::Bool(true));
    assert_eq!(ctx.db.with_snapshot(|s| s.skill_queues[&cid][0].start_sp), 321.0);
    let info = get_respec_info(&ctx, &sess).unwrap();
    assert_eq!(info.dict_get("lastRespecDate"), Some(&PyRep::Integer(1_700_000_000)));
    assert_eq!(info.dict_get("nextTimedRespec"), Some(&PyRep::Integer(1_731_536_000)));
}

#[tokio::test]
async fn strip_fitting_variants_container_range_boundary_and_post_hooks() {
    let ctx = test_ctx();
    let (cid, sys_id, station_id) = (90_000_001_i64, 30_000_142_u32, 60_003_760_i64);
    let (struct_id, ship_id) = (1_045_000_000_001_i64, 100_000_001_i64);
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    // Structure without quantum core -> QuantumCoreRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "shield_vulnerable".into(),
                ..Default::default()
            },
        );
    });
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);
    assert_eq!(strip_err(&ctx, &mut sess, &kw), "QuantumCoreRequired");

    // Station validation errors: NoActiveShip, ShipNotFound, OwnershipViolation, ShipNotAtStation, inprogress contract
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    sess.ship_id = None;
    assert_eq!(strip_err(&ctx, &mut sess, &kw), "NoActiveShip");
    let mut kw_missing = HashMap::new();
    kw_missing.insert("shipID".into(), PyRep::Integer(999_888_777));
    assert_eq!(strip_err(&ctx, &mut sess, &kw_missing), "ShipNotFound");

    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.items.get_mut(&ship_id).unwrap();
        s.owner_id = 90_000_002;
        s.location_id = station_id;
    });
    assert_eq!(strip_err(&ctx, &mut sess, &kw), "OwnershipViolation");
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.items.get_mut(&ship_id).unwrap();
        s.owner_id = cid;
        s.location_id = struct_id;
    });
    assert_eq!(strip_err(&ctx, &mut sess, &kw), "ShipNotAtStation");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = station_id;
        snap.contracts.insert(
            777,
            ContractRecord {
                contract_id: 777,
                issuer_id: cid,
                status: "inprogress".into(),
                item_ids: vec![ship_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(strip_err(&ctx, &mut sess, &kw), "ItemLockedInContract");

    // Empty ship strip succeeds with empty list, then strip BPC + unknown-type item + service-slot item
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.remove(&777);
        snap.items.retain(|_, i| i.location_id != ship_id);
    });
    let empty_strip = handle(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw).unwrap().unwrap();
    assert_eq!(empty_strip.dict_get("strippedItemIDs"), Some(&PyRep::List(Vec::new())));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(895_001, mk_item(895_001, 487, cid, ship_id, InventoryFlag::LoSlot0, 1, true, true));
        snap.items.insert(895_002, mk_item(895_002, 999_777_666, cid, ship_id, InventoryFlag::ServiceSlot0, 3, false, false));
        snap.module_online_states.insert(895_001, true);
        snap.module_active_states.insert(895_001);
        snap.module_active_charges.insert(895_001, 895_099);
    });
    let strip_ok = handle(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw).unwrap().unwrap();
    assert_eq!(strip_ok.dict_get("strippedItemIDs").and_then(|v| v.as_list()).unwrap().len(), 2);

    // Container range boundaries: non-solar-system boundaries 29_999_999 & 33_000_001 -> Ok; edge_dist 2_500.0 vs 2_500.5
    assert!(verify_in_space_container_proximity(&ctx, cid, 29_999_999, 896_001).await.is_ok());
    assert!(verify_in_space_container_proximity(&ctx, cid, 33_000_001, 896_001).await.is_ok());

    let cont_id = 896_001_i64;
    let cont_tid = ctx.sde.types.values().find(|t| t.group_id == 12).map(|t| t.type_id).unwrap_or(23);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.items.insert(cont_id, mk_item(cont_id, cont_tid, cid, sys_id as i64, InventoryFlag::None, 1, true, false));
        snap.sp6_lifecycle.space_object_meta.remove(&cont_id);
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, x, r) in [(ship_id, 606_u32, 0.0, 50.0), (cont_id, cont_tid, 2_570.0, 20.0)] {
        let _ = sys_h
            .spawn_ball(Ball {
                id,
                type_id: tid,
                position: glam::DVec3::new(x, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0e4,
                radius: r,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    assert!(verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id).await.is_ok());

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    post_inventory_fitting_hook(&ctx, &SessionState::new(1), None, "Drop", &[]).await;
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    post_inventory_fitting_hook(&ctx, &no_sys, None, "Drop", &[]).await;

    post_inventory_fitting_hook(&ctx, &sess, None, "Drop", &[]).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.insert(
            cont_id,
            PersistedSpaceObjectMeta {
                anchor_start_unix: Some(1),
                created_unix: Some(1),
                ..Default::default()
            },
        );
    });
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
    post_inventory_fitting_hook(&ctx, &sess, Some(&bound), "ValidateItemCanBeOpened", &[PyRep::Integer(cont_id)]).await;
    post_inventory_fitting_hook(&ctx, &sess, None, "List", &[PyRep::Tuple(vec![PyRep::Integer(cont_id)])]).await;
    assert!(ctx.db.with_snapshot(|s| s.sp6_lifecycle.space_object_meta[&cont_id].anchor_start_unix != Some(1)));
    for op in ["Activate", "Overload", "Deactivate", "TakeModuleOffline", "DestroyFitting", "Add", "MultiAdd", ""] {
        post_inventory_fitting_hook(&ctx, &sess, None, op, &[]).await;
    }

    let _ = sys_h.set_ball_position(cont_id, glam::DVec3::new(2_570.5, 0.0, 0.0)).await;
    assert_eq!(verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id).await.unwrap_err().code, "OutOfRange");
}
