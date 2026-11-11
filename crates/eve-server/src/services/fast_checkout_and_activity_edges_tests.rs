//! Edge-case tests for `fast_checkout_and_activity`: moon mining details/output,
//! activityMgr ring-buffer eviction & KeyVal pop-state truncation, BuyOffer payment tokens,
//! PLEX vault debits/credits, product delivery, Omega activation, and error paths.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::quasar_payment_store_types::{
    save_store_catalog, save_store_offer_tags, PaymentTokenRecord, StoreCatalog, StoreOffer,
    StoreProduct,
};
use eve_db::{
    AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
    UpwellStructureRecord,
};
use std::collections::HashMap;

#[test]
fn mining_details_cycle_output_and_activity_mgr_edges() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // 1. compute_mining_materials_per_cycle with moon_id=0 and empty custom_ores -> empty
    assert!(compute_mining_materials_per_cycle(&ctx.sde, 30_000_142, 0, &[]).is_empty());
    // With unknown custom ore type (no group 427 materials) -> fallback path
    let fallback = compute_mining_materials_per_cycle(&ctx.sde, 30_000_142, 0, &[(999_991, 0.5)]);
    assert_eq!(fallback.len(), 1);

    // 2. Seed structure owned by a different corp (98_999_001) -> AccessDenied on RequestMiningDetails
    let struct_id = 1_000_555_001_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 81826, // Metenox Moon Drill
                owner_corp_id: 98_999_001,
                solar_system_id: 30_000_142,
                name: "Test Drill".into(),
                state: "ShieldVulnerable".into(),
                position: [1_000.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.auto_moon_miners.insert(
            struct_id,
            AutoMoonMinerRecord {
                structure_id: struct_id,
                fuel_units_remaining: 420,
                magmatic_gas_remaining: 100,
                efficiency: 0.40,
                cycle_duration_seconds: 3600,
                last_cycle_unix: now,
                ore_type_id: 45490,
                hourly_yield: 1000,
                status: "Active".into(),
            },
        );
    });

    let mut kw_sid = HashMap::new();
    kw_sid.insert("structureID".into(), PyRep::Integer(struct_id));
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "RequestMiningDetails", &[], &kw_sid)
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Switch structure owner to caller's corp (98_000_001) + add mineral hold items
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = 98_000_001;
        snap.items.insert(
            77_900_001,
            InventoryItemRecord {
                item_id: 77_900_001,
                type_id: 34, // Tritanium
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::SpecializedMaterialBay,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let details_auto = handle(&ctx, &sess, "FastCheckoutService", "RequestMiningDetails", &[], &kw_sid)
        .unwrap()
        .unwrap();
    assert_eq!(details_auto.dict_get("availableFuel").and_then(|v| v.as_i64()), Some(420));

    // Seed moon_extractions with custom moon_ores + fuel_blocks -> exercises extraction branch & RequestMiningCycleOutput
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 88;
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: 40_009_076,
                extraction_start_unix: now,
                chunk_arrival_unix: now + 7200,
                natural_decay_unix: now + 18_000,
                status: "Ready".into(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(999_991, 1.0)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
    });
    let details_ext = handle(&ctx, &sess, "FastCheckoutService", "RequestMiningDetails", &[], &kw_sid)
        .unwrap()
        .unwrap();
    assert_eq!(details_ext.dict_get("availableFuel").and_then(|v| v.as_i64()), Some(88));
    // Metenox Moon Drill cannot operate in High-Sec (30_000_142) -> InvalidSystemSecurity;
    // move structure to Low-Sec/Null-Sec and verify RequestMiningCycleOutput succeeds.
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "RequestMiningCycleOutput", &[], &kw_sid)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSystemSecurity"
    );
    let mut lowsec_candidates: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .collect();
    lowsec_candidates.sort_unstable();
    let lowsec_sys = lowsec_candidates
        .into_iter()
        .find(|&sys_id| {
            ctx.sde.celestials_in_system(sys_id).iter().any(|c| {
                c.group_id == 8
                    && !crate::services::mining_advanced::mining_compression::derive_metenox_moon_materials(
                        &ctx.sde, sys_id, c.celestial_id, 0.40, 1,
                    )
                    .is_empty()
            })
        })
        .unwrap_or(30_002_537);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().solar_system_id = lowsec_sys;
    });
    let out_ext = handle(&ctx, &sess, "FastCheckoutService", "RequestMiningCycleOutput", &[], &kw_sid)
        .unwrap()
        .unwrap();
    assert_eq!(out_ext.as_list().unwrap().len(), 1);

    // 3. ActivitySeen via kwargs + >128 eviction + duplicate suppression
    let valid_ids = sde_new_activity_ids(&ctx.sde);
    if let Some(&aid) = valid_ids.iter().next() {
        
        let fake_seen: Vec<i64> = (1..=MAX_SEEN_ACTIVITIES as i64).map(|x| 9_000_000 + x).collect();
        ctx.db.with_mut_snapshot(|snap| {
            let st = CharacterActivityState {
                seen_activities: fake_seen,
                pop_state: Vec::new(),
            };
            snap.seasons_and_campaigns.character_activities.insert(90_000_001, st);
        });
        let mut kw_act = HashMap::new();
        kw_act.insert("activityID".into(), PyRep::Integer(aid));
        assert_eq!(
            handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[], &kw_act)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        // Repeat does not grow beyond MAX_SEEN_ACTIVITIES
        assert_eq!(
            handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[], &kw_act)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        let badges = handle(&ctx, &sess, "activityMgr", "GetActivityBadgeStatuses", &[], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(badges.as_list().unwrap().len(), MAX_SEEN_ACTIVITIES);
    }

    // 4. SetActivitiesPopState via KeyVal kwargs and >100 truncation
    let many_kv: HashMap<String, PyRep> = (0..105)
        .map(|i| (format!("k{i}"), PyRep::Integer(i)))
        .collect();
    let mut kw_pop = HashMap::new();
    kw_pop.insert("state".into(), PyRep::KeyVal(many_kv));
    assert_eq!(
        handle(&ctx, &sess, "activityMgr", "SetActivitiesPopState", &[], &kw_pop)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let pop = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pop.as_dict().unwrap().len(), 100);
}

#[test]
fn buy_offer_payment_tokens_plex_vault_products_omega_and_error_edges() {
    let mut ctx = test_ctx();
    ctx.feature_flags.free_fast_checkout_plex = false;
    ctx.feature_flags.seed_demo = false;
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Testing config when seed_demo = false
    let tcfg = handle(&ctx, &sess, "FastCheckoutService", "GetTestingConfiguration", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(tcfg, PyRep::Tuple(vec![PyRep::Bool(false), PyRep::String(String::new()), PyRep::Bool(false)]));

    // Disabled user: GetOffersForUser returns [], BuyOffer returns FastCheckoutDisabled
    set_user_fast_checkout_enabled(&ctx, 1, false);
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "FastCheckoutDisabled"
    );

    // Enable user and seed custom store catalog with:
    // - Offer 101: USD-priced PLEX + Omega (category_id = 1) + non-PLEX item product (Tritanium x10)
    // - Offer 102: PLEX-priced product offer (price_plex = 50)
    // - Offer 103: Negative price_plex (-1) -> InvalidOffer
    // - Offer 104: Zero USD & zero PLEX price -> InvalidOffer
    // - Offer 105: Empty products (filtered out by load_catalog_plex_offers)
    set_user_fast_checkout_enabled(&ctx, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        let cat = StoreCatalog {
            categories: vec![],
            products: vec![
                StoreProduct {
                    id: 1,
                    name: "PLEX".into(),
                    description: "".into(),
                    type_id: crate::services::market_contracts::PLEX_TYPE_ID,
                    quantity: 100,
                },
                StoreProduct {
                    id: 2,
                    name: "Tritanium".into(),
                    description: "".into(),
                    type_id: 34,
                    quantity: 10,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 201,
                    name: "Starter Omega + PLEX".into(),
                    description: "Pack".into(),
                    category_id: 1,
                    price_plex: 0,
                    product_ids: vec![1, 2],
                    targeted: false,
                    price_cents: Some(999),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 202,
                    name: "Mineral Pack".into(),
                    description: "Trit".into(),
                    category_id: 2,
                    price_plex: 50,
                    product_ids: vec![2],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 203,
                    name: "Bad Neg".into(),
                    description: "".into(),
                    category_id: 2,
                    price_plex: -1,
                    product_ids: vec![2],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 204,
                    name: "Bad Zero".into(),
                    description: "".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![2],
                    targeted: false,
                    price_cents: Some(0),
                    currency: None,
                },
                StoreOffer {
                    id: 205,
                    name: "Empty".into(),
                    description: "".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![9999],
                    targeted: false,
                    price_cents: Some(100),
                    currency: None,
                },
            ],
        };
        save_store_catalog(snap, &cat);
        let mut tags = HashMap::new();
        tags.insert(201, vec!["featured".into()]);
        save_store_offer_tags(snap, &tags);
        snap.account_plex_vault.insert(1, 0);
    });

    // Clear cache and populate cached_offers via GetOffersForUser
    let offers_rep = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(!offers_rep.dict_get("offers").unwrap().as_list().unwrap().is_empty());

    // Find valid password for demo user_id 1 or set known password hash
    let pw = "evomu_test_pw";
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == 1) {
            acc.password_token = eve_db::WorldRepository::hash_password_token(&acc.username, pw);
        }
    });

    // Missing password -> InvalidPassword; wrong password -> InvalidPassword
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(201)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPassword"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(201), PyRep::String("wrong".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPassword"
    );

    // Unknown offer -> OfferNotFound (via Dict arg)
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Dict(vec![(PyRep::String("offerId".into()), PyRep::Integer(999_999))]),
                PyRep::String(pw.into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // Offer 203 / 204 -> InvalidOffer
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(203), PyRep::Integer(1), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOffer"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(204), PyRep::Integer(1), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOffer"
    );

    // Offer 201 without quick-pay payment token -> PaymentTokenRequired
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(201), PyRep::Integer(1), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PaymentTokenRequired"
    );

    // Seed quick-pay token and purchase Offer 201 (credits PLEX, delivers items, activates Omega, records receipt)
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = vec![PaymentTokenRecord {
            id: 1,
            user_id: 1,
            token_str: "tok_qp".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.quasar_commerce.payment_tokens = tokens.clone();
    });
    let buy201 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(201), PyRep::Integer(2), PyRep::String(pw.into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(buy201.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(buy201.dict_get("plexAmount").and_then(|v| v.as_i64()), Some(200));
    assert_eq!(buy201.dict_get("itemsDelivered").and_then(|v| v.as_i64()), Some(20));

    // Purchase Offer 202 using PLEX balance: first with qty=10 (500 PLEX > 200) -> UserError Insufficient balance
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(202), PyRep::Integer(10), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserError"
    );
    // Purchase Offer 202 with qty=1 (50 PLEX <= 200) -> succeeds
    let buy202 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(202), PyRep::Integer(1), PyRep::String(pw.into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(buy202.dict_get("newPlexBalance").and_then(|v| v.as_i64()), Some(150));

    // Banned account -> AccountBanned
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == 1) {
            acc.banned_until_unix = Some(u64::MAX);
            acc.ban_reason = Some("Test ban".into());
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(202), PyRep::Integer(1), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccountBanned"
    );

    // Unknown user -> UserNotFound
    let ghost_sess = auth_session(888_888, 90_000_001, 98_000_001, 30_000_142);
    set_user_fast_checkout_enabled(&ctx, 888_888, true);
    assert_eq!(
        handle(
            &ctx,
            &ghost_sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(202), PyRep::Integer(1), PyRep::String(pw.into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserNotFound"
    );
}
