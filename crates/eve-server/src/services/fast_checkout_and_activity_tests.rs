#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use std::collections::HashMap;

#[test]
fn fast_checkout_state_and_offers_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &sess, unknown_svc, "RequestMiningDetails", &[], &kw).is_none());
    assert!(handle(&ctx, &sess, "FastCheckoutService", unknown_method, &[], &kw).is_none());

    let unauth = SessionState::default();
    assert!(handle(&ctx, &unauth, "FastCheckoutService", "IsFastCheckoutEnabledForUser", &[], &kw)
        .unwrap()
        .is_err());

    set_user_fast_checkout_enabled(&ctx, 1, false);
    assert!(!get_user_fast_checkout_state(&ctx, 1).enabled);
    set_user_fast_checkout_enabled(&ctx, 1, true);
    assert!(get_user_fast_checkout_state(&ctx, 1).enabled);
    assert!(is_fast_checkout_enabled_for_user(&ctx, 1));

    let enabled_rep = handle(&ctx, &sess, "FastCheckoutService", "IsFastCheckoutEnabledForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(enabled_rep, PyRep::Bool(true));

    let offers_rep = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let plex_list = offers_rep.dict_get("plex").and_then(|v| v.as_list()).unwrap();
    assert!(!plex_list.is_empty());
    let first_offer = &plex_list[0];
    assert!(first_offer.dict_get("offerID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(first_offer.dict_get("baseQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let cleared = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(cleared, PyRep::None);
    assert!(get_user_fast_checkout_state(&ctx, 1).cached_offers.is_none());

    // Global config override test
    set_user_fast_checkout_enabled(&ctx, 9999, false);
    if let Ok(mut g) = ctx.global_config_values.write() {
        g.insert("fastCheckoutEnabled".into(), PyRep::String("true".into()));
    }
    assert!(is_fast_checkout_enabled_for_user(&ctx, 9999));
    if let Ok(mut g) = ctx.global_config_values.write() {
        g.insert("fastCheckoutEnabled".into(), PyRep::Integer(0));
    }
    assert!(!is_fast_checkout_enabled_for_user(&ctx, 9999));
    if let Ok(mut g) = ctx.global_config_values.write() {
        g.remove("fastCheckoutEnabled");
    }

    let test_cfg = handle(&ctx, &sess, "FastCheckoutService", "GetTestingConfiguration", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::Tuple(cfg_items) = test_cfg else {
        panic!("expected tuple");
    };
    assert_eq!(cfg_items.len(), 3);

    // Tutorial RPCs
    let init_tut = handle(&ctx, &sess, "FastCheckoutService", "initialize_tutorial_status", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(init_tut, PyRep::Bool(false));

    let skip_tut = handle(&ctx, &sess, "FastCheckoutService", "skip_tutorial", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(skip_tut, PyRep::Bool(true));

    let status_after_skip = handle(&ctx, &sess, "FastCheckoutService", "get_tutorial_completion_status", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(status_after_skip, PyRep::Bool(true));

    let reset_tut = handle(&ctx, &sess, "FastCheckoutService", "reset_tutorial", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(reset_tut, PyRep::Bool(true));

    let status_after_reset = handle(&ctx, &sess, "FastCheckoutService", "get_tutorial_completion_status", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(status_after_reset, PyRep::Bool(false));

    let pd_enabled = handle(&ctx, &sess, "FastCheckoutService", "is_enabled", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(pd_enabled, PyRep::Bool(_)));

    // BuyOffer validation branches
    assert!(handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(0)], &kw).unwrap().is_err());
    assert!(handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(1), PyRep::Integer(0)],
        &kw
    )
    .unwrap()
    .is_err());

    // ActivityMgr RPCs
    let badges_init = handle(&ctx, &sess, "activityMgr", "GetActivityBadgeStatuses", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(badges_init, PyRep::PySet(_)));

    let pop_init = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pop_init, PyRep::Dict(vec![]));

    assert!(handle(&ctx, &sess, "activityMgr", "SetActivitiesPopState", &[PyRep::Integer(5)], &kw)
        .unwrap()
        .is_err());

    let set_pop = handle(
        &ctx,
        &sess,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::Dict(vec![(PyRep::String("tab1".into()), PyRep::Integer(1))])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_pop, PyRep::Bool(true));

    let pop_after = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pop_after.dict_get("tab1"), Some(&PyRep::Integer(1)));

    // ActivitySeen validation
    assert!(handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(0)], &kw)
        .unwrap()
        .is_err());
    assert!(handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(999_999_999)], &kw)
        .unwrap()
        .is_err());
    let valid_ids = sde_new_activity_ids(&ctx.sde);
    if let Some(&aid) = valid_ids.iter().next() {
        let seen_ok = handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(aid)], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(seen_ok, PyRep::Bool(true));
    }

    // Mining materials computation
    let mats = compute_mining_materials_per_cycle(&ctx.sde, 30000142, 0, &[(1228, 1.0)]);
    assert!(!mats.is_empty());

    // Mining details & cycle output validation
    assert!(handle(&ctx, &sess, "FastCheckoutService", "RequestMiningCycleOutput", &[], &kw)
        .unwrap()
        .is_err());
    assert!(handle(&ctx, &sess, "FastCheckoutService", "RequestMiningDetails", &[], &kw)
        .unwrap()
        .is_err());
    assert!(handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(999_888_777)],
        &kw
    )
    .unwrap()
    .is_err());

    sess.solar_system_id = Some(30000142);
}

#[test]
fn fast_checkout_mining_details_and_paid_omega_buy_offer_paths() {
    use crate::services::quasar_payment_store_types::{
        save_store_catalog, PaymentTokenRecord, StoreCatalog, StoreCategory, StoreOffer,
        StoreProduct,
    };
    use eve_db::{
        AutoMoonMinerRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
        UpwellStructureRecord,
    };

    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let other_corp = 98_000_999_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(uid, cid, corp_id, sys_id);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();

    // 1. Mining materials computation with empty ores vs valid moon ores
    assert!(compute_mining_materials_per_cycle(&ctx.sde, sys_id, 0, &[]).is_empty());
    let mats = compute_mining_materials_per_cycle(&ctx.sde, sys_id, 40_000_001, &[(1228, 0.5), (1230, 0.5)]);
    assert!(!mats.is_empty());

    // 2. RequestMiningCycleOutput & RequestMiningDetails with owned structure, moon extraction, fuel & output bay
    let struct_id = 1_070_000_001_i64;
    let foreign_struct = 1_070_000_002_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 81826, // Metenox Moon Drill
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Metenox".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            foreign_struct,
            UpwellStructureRecord {
                structure_id: foreign_struct,
                type_id: 81826,
                owner_corp_id: other_corp,
                solar_system_id: sys_id,
                name: "Foreign Metenox".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.auto_moon_miners.insert(
            struct_id,
            AutoMoonMinerRecord {
                structure_id: struct_id,
                fuel_units_remaining: 25,
                magmatic_gas_remaining: 25,
                efficiency: 0.4,
                cycle_duration_seconds: 3600,
                last_cycle_unix: now,
                ore_type_id: 1228,
                hourly_yield: 100,
                status: "Active".into(),
            },
        );
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: 40_000_001,
                extraction_start_unix: now,
                chunk_arrival_unix: now + 7200,
                natural_decay_unix: now + 10_800,
                status: "Ready".into(),
                auto_fracture: false,
                event_id: 0,
                moon_ores: vec![(1228, 1.0)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
        // Output bay item to exercise used_vol calculation
        snap.items.insert(
            970_001,
            InventoryItemRecord {
                item_id: 970_001,
                type_id: 34,
                owner_id: corp_id,
                location_id: struct_id,
                flag: InventoryFlag::SpecializedMineralHold,
                quantity: 100,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningDetails",
            &[PyRep::Integer(foreign_struct)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    let details = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(details.dict_get("availableFuel"), Some(&PyRep::Integer(50)));
    assert_eq!(details.dict_get("moonID"), Some(&PyRep::Integer(40_000_001)));

    // High-sec structure triggers InvalidSystemSecurity for RequestMiningCycleOutput
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(struct_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSystemSecurity"
    );
    // Move structure to null-sec so RequestMiningCycleOutput succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.solar_system_id = 30_004_759;
        }
    });
    let cycle_out = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(cycle_out.as_list().is_some_and(|l| !l.is_empty()));

    // Remove moon_extractions so RequestMiningDetails & RequestMiningCycleOutput fall back to auto_moon_miner
    ctx.db.with_mut_snapshot(|snap| {
        snap.moon_extractions.remove(&struct_id);
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.fuel_blocks = 0;
        }
    });
    let details2 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(details2.dict_get("availableFuel"), Some(&PyRep::Integer(25)));
    assert!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(struct_id)],
            &kw,
        )
        .unwrap()
        .is_ok()
    );

    // 3. BuyOffer with paid USD offer, Omega offer (category 1), PLEX offer, password & token errors
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    set_user_fast_checkout_enabled(&ctx, uid, true);
    let usd_offer_id = 88_001_u32;
    let omega_offer_id = 88_002_u32;
    let plex_offer_id = 88_003_u32;
    let cat = StoreCatalog {
        categories: vec![StoreCategory { id: 1, name: "Omega".into(), description: "".into() }],
        products: vec![StoreProduct {
            id: 901,
            name: "Tritanium".into(),
            description: "Ore".into(),
            type_id: 34,
            quantity: 10,
        }],
        offers: vec![
            StoreOffer {
                id: usd_offer_id,
                name: "100 PLEX".into(),
                description: "USD PLEX".into(),
                price_cents: Some(499),
                price_plex: 0,
                category_id: 2,
                product_ids: vec![901],
                targeted: false,
                currency: Some("USD".into()),
            },
            StoreOffer {
                id: omega_offer_id,
                name: "30d Omega".into(),
                description: "Omega".into(),
                price_cents: Some(1999),
                price_plex: 500,
                category_id: 1,
                product_ids: vec![901],
                targeted: false,
                currency: Some("USD".into()),
            },
            StoreOffer {
                id: plex_offer_id,
                name: "Invalid negative PLEX offer".into(),
                description: "Neg".into(),
                price_cents: Some(0),
                price_plex: -1,
                category_id: 2,
                product_ids: vec![901],
                targeted: false,
                currency: None,
            },
        ],
    };
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(snap, &cat);
    });
    let usd_offer_id = usd_offer_id as i64;
    let omega_offer_id = omega_offer_id as i64;
    let plex_offer_id = plex_offer_id as i64;
    // Actually register password via db helper or check how authenticate_at works: let's provision an account with known password!
    let paid_uid = 2002_u64;
    let paid_cid = 90_200_002_i64;
    ctx.db.provision_gm_account("fastpay_user", "secretpw", paid_cid, "FastPayPilot");
    let real_uid = ctx
        .db
        .with_snapshot(|s| s.accounts.get("fastpay_user").map(|a| a.user_id).unwrap());
    set_user_fast_checkout_enabled(&ctx, real_uid, true);
    let mut psess = auth_session(real_uid, paid_cid, corp_id, sys_id);
    psess.station_id = Some(station_id);
    let _ = handle(&ctx, &psess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let advertised = handle(&ctx, &psess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let advertised_list = advertised.dict_get("offers").and_then(|v| v.as_list()).unwrap();
    assert!(
        !advertised_list
            .iter()
            .any(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(plex_offer_id)),
        "negative-price offer must be omitted from GetOffersForUser"
    );
    let adv_omega = advertised_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(omega_offer_id))
        .expect("dual-priced Omega offer in GetOffersForUser");
    assert_eq!(adv_omega.dict_get("price"), Some(&PyRep::Real(500.0)));
    assert_eq!(adv_omega.dict_get("currency"), Some(&PyRep::String("PLX".into())));
    let adv_usd = advertised_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(usd_offer_id))
        .expect("USD offer in GetOffersForUser");
    assert_eq!(adv_usd.dict_get("price"), Some(&PyRep::Real(4.99)));
    assert_eq!(adv_usd.dict_get("currency"), Some(&PyRep::String("USD".into())));

    // Missing password -> InvalidPassword
    assert_eq!(
        handle(
            &ctx,
            &psess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(usd_offer_id), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    // OfferNotFound
    assert_eq!(
        handle(
            &ctx,
            &psess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(999_888_777), PyRep::String("secretpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    // Wrong password -> InvalidPassword
    assert_eq!(
        handle(
            &ctx,
            &psess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(usd_offer_id), PyRep::String("wrongpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    // Excluded negative-price catalog offer -> OfferNotFound
    assert_eq!(
        handle(
            &ctx,
            &psess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(plex_offer_id), PyRep::Integer(1), PyRep::String("secretpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    // No quick-pay token -> PaymentTokenRequired
    assert_eq!(
        handle(
            &ctx,
            &psess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(usd_offer_id), PyRep::Integer(1), PyRep::String("secretpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PaymentTokenRequired"
    );

    // Add quick-pay token and buy USD offer (delivers product & writes payment receipt!)
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = vec![PaymentTokenRecord {
            id: 1,
            user_id: real_uid,
            token_str: "qp-1".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.quasar_commerce.payment_tokens = tokens.clone();
        snap.account_plex_vault.insert(real_uid, 1000);
    });
    let bought_usd = handle(
        &ctx,
        &psess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(usd_offer_id), PyRep::Integer(2), PyRep::String("secretpw".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bought_usd.dict_get("status"), Some(&PyRep::String("CONFIRMED".into())));

    // Buy Omega offer via PLEX (category_id == 1 -> pushes notice to pending_notices!)
    let bought_omega = handle(
        &ctx,
        &psess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(omega_offer_id), PyRep::Integer(1), PyRep::String("secretpw".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bought_omega.dict_get("status"), Some(&PyRep::String("CONFIRMED".into())));

    // Exercise UpdatePopState with PyRep::KeyVal + >100 truncation, cached_offers hit in GetOffersForUser/BuyOffer,
    // BuyOffer overflow guards (total_plex, total_cost_plex, amount_in_cents, vault overflow, product qty overflow),
    // and character_id=None / station_id=None fallbacks.
    let big_kv: Vec<(String, PyRep)> = (0..105).map(|i| (format!("k{i}"), PyRep::Bool(true))).collect();
    assert_eq!(
        handle(&ctx, &sess, "activityMgr", "SetActivitiesPopState", &[PyRep::KeyVal(big_kv.into_iter().collect())], &kw).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    // Call GetOffersForUser again to hit cached_offers branch
    let offers2 = handle(&ctx, &psess, "FastCheckoutService", "GetOffersForUser", &[], &kw).unwrap().unwrap();
    assert!(offers2.dict_get("plex").is_some());

    // quantity = i64::MAX -> total_plex / total_cost_plex ArithmeticOverflow
    assert_eq!(
        handle(&ctx, &psess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(omega_offer_id), PyRep::Integer(i64::MAX), PyRep::String("secretpw".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ArithmeticOverflow"
    );
    // USD offer with quantity = i64::from(u32::MAX) -> amount_in_cents ArithmeticOverflow
    assert_eq!(
        handle(&ctx, &psess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(usd_offer_id), PyRep::Integer(i64::from(u32::MAX)), PyRep::String("secretpw".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ArithmeticOverflow"
    );
    // Product quantity i32 overflow + character_id=None / station_id=None fallbacks
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(real_uid, i64::MAX / 2);
        if let Some(ch) = snap.characters.get_mut(&psess.character_id.unwrap_or(0)) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.home_station_id = 60_003_760;
        }
    });
    let mut psess_nochar = psess.clone();
    psess_nochar.character_id = None;
    psess_nochar.station_id = None;
    psess_nochar.structure_id = None;
    assert_eq!(
        handle(&ctx, &psess_nochar, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(omega_offer_id), PyRep::Integer(i64::from(i32::MAX) + 1), PyRep::String("secretpw".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ArithmeticOverflow"
    );
    // Account PLEX vault overflow on USD PLEX purchase when vault near i64::MAX
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(real_uid, i64::MAX - 1);
        let tokens = vec![PaymentTokenRecord {
            id: 2,
            user_id: real_uid,
            token_str: "qp-2".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.quasar_commerce.payment_tokens = tokens.clone();
    });
    assert!(
        handle(&ctx, &psess_nochar, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(usd_offer_id), PyRep::Integer(1), PyRep::String("secretpw".into())], &kw)
            .unwrap()
            .is_ok()
    );
    let _ = paid_uid;
}

#[test]
fn fast_checkout_activity_eviction_and_offer_id_variants_cov() {
    let ctx = test_ctx();
    let sess = auth_session(77, 90_000_077, 98_000_077, 30_000_142);
    let kw = HashMap::new();
    set_user_fast_checkout_enabled(&ctx, 77, true);

    // Fill seen_activities to MAX_SEEN_ACTIVITIES then mark a valid SDE activity seen -> hits eviction branch (723-728)
    let valid_ids: Vec<i64> = sde_new_activity_ids(&ctx.sde).iter().copied().collect();
    if let Some(&first_act) = valid_ids.first() {
        
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = CharacterActivityState::default();
            st.seen_activities = (1..=(MAX_SEEN_ACTIVITIES as i64)).map(|i| -i - 10_000).collect();
            snap.seasons_and_campaigns.character_activities.insert(90_000_077, st);
        });
        let seen_rep = handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(first_act)], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(seen_rep, PyRep::Bool(true));
    }

    // Exercise extract_offer_id Dict / KeyVal / invalid branches (937-943)
    let dict_offer = PyRep::Dict(vec![(PyRep::String("offerId".into()), PyRep::Integer(999_999))]);
    assert!(handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[dict_offer], &kw).unwrap().is_err());

    let kv_offer = PyRep::Object {
        type_name: "util.KeyVal".into(),
        args: Box::new(PyRep::Dict(vec![(PyRep::String("offer_id".into()), PyRep::Integer(888_888))])),
    };
    assert!(handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[kv_offer], &kw).unwrap().is_err());

    assert!(handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[PyRep::String("bad".into())], &kw).unwrap().is_err());
}

#[test]
fn fast_checkout_buy_offer_edge_errors_and_free_plex_delivery() {
    use crate::services::quasar_payment_store_types::{
        save_store_catalog, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
    };

    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = false;
    set_user_fast_checkout_enabled(&ctx, 1, true);

    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "BuyOffer", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(2001), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let catalog = StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "PLEX".into(),
                description: "Store".into(),
            }],
            products: vec![
                StoreProduct {
                    id: 501,
                    name: "500 PLEX".into(),
                    description: "PLEX".into(),
                    type_id: crate::services::market_contracts::PLEX_TYPE_ID,
                    quantity: 500,
                },
                StoreProduct {
                    id: 502,
                    name: "Big Ship".into(),
                    description: "Ship".into(),
                    type_id: 587,
                    quantity: u32::MAX,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 5001,
                    name: "500 PLEX Pack".into(),
                    description: "PLEX".into(),
                    category_id: 1,
                    price_plex: 0,
                    product_ids: vec![501],
                    targeted: false,
                    price_cents: Some(1999),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 5002,
                    name: "Neg PLEX Price".into(),
                    description: "Broken".into(),
                    category_id: 2,
                    price_plex: -5,
                    product_ids: vec![501],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 5003,
                    name: "Zero Price".into(),
                    description: "Unpriced".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![502],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 5004,
                    name: "Priced Huge Qty".into(),
                    description: "Overflow test offer".into(),
                    category_id: 2,
                    price_plex: 0,
                    product_ids: vec![502],
                    targeted: false,
                    price_cents: Some(100),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &catalog);
    });

    // UserNotFound when session user has no AccountRecord
    let mut unknown_sess = auth_session(888_777, 90_000_001, 98_000_001, 30_000_142);
    unknown_sess.station_id = Some(60_003_760);
    set_user_fast_checkout_enabled(&ctx, 888_777, true);
    assert_eq!(
        handle(
            &ctx,
            &unknown_sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(5001), PyRep::String("pw".into())],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserNotFound"
    );

    // OfferNotFound
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(999_555), PyRep::String("demo-secret".into())],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // Filtered negative price_plex (5002) and unpriced offer (5003) -> OfferNotFound
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(5002), PyRep::String("demo-secret".into())],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(5003), PyRep::String("demo-secret".into())],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // ArithmeticOverflow on PLEX quantity multiplication and i32 product quantity overflow
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(5001),
                PyRep::Integer(i64::MAX / 100),
                PyRep::String("demo-secret".into())
            ],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ArithmeticOverflow"
    );

    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .free_fast_checkout_plex = true;
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(5004), PyRep::Integer(2)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ArithmeticOverflow"
    );

    // 2-arg form (offerID, passwordString) in free_fast_checkout_plex mode grants PLEX and logs notice
    let before_plex = ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0));
    let free_buy = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(5001), PyRep::String("demo-secret".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        free_buy.dict_get("status").and_then(|v| v.as_str()),
        Some("COMPLETED")
    );
    assert_eq!(
        free_buy.dict_get("newPlexBalance").and_then(|v| v.as_i64()),
        Some(before_plex + 500)
    );
}

#[test]
fn fast_checkout_tutorial_activity_mgr_and_mining_edge_branches() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Project discovery feature flag + corrupt tutorial JSON reset + skip/reset roundtrip
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_project_discovery = false;
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "is_enabled", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_project_discovery = true;
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "is_enabled", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.project_discovery_players.remove(&90_000_001);
    });
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "initialize_tutorial_status",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "skip_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "initialize_tutorial_status",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &sess, "FastCheckoutService", "reset_tutorial", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "get_tutorial_completion_status",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    // activityMgr validation + invalid SetActivitiesPopState + MAX_SEEN_ACTIVITIES eviction + getters
    assert_eq!(
        handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "activityMgr",
            "ActivitySeen",
            &[PyRep::Integer(999_888_777)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidActivityID"
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "activityMgr",
            "SetActivitiesPopState",
            &[PyRep::Integer(42)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    let sde_ids = sde_new_activity_ids(&ctx.sde);
    if let Some(&valid_id) = sde_ids.iter().next() {
        ctx.db.with_mut_snapshot(|snap| {
            let pre_seen: Vec<i64> = (10_000_001_i64..10_000_129_i64).collect();
            let blob = serde_json::json!({
                "seen_activities": pre_seen,
                "pop_state": [[PyRep::String("k".into()), PyRep::Integer(1)]]
            });
            snap.seasons_and_campaigns.character_activities.insert(90_000_001, serde_json::from_value(blob).unwrap());
        });
        let mut akw = HashMap::new();
        akw.insert("activityID".to_string(), PyRep::Integer(valid_id));
        assert_eq!(
            handle(&ctx, &sess, "activityMgr", "ActivitySeen", &[], &akw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
        let badges = handle(
            &ctx,
            &sess,
            "activityMgr",
            "GetActivityBadgeStatuses",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap();
        let PyRep::PySet(items) = badges else {
            panic!("expected PySet");
        };
        assert_eq!(items.len(), 128);
        assert!(items.contains(&PyRep::Integer(valid_id)));
    }

    let pop = handle(&ctx, &sess, "activityMgr", "GetActivitiesPopState", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(pop, PyRep::Dict(_)));

    // compute_mining_materials_per_cycle empty + non-427 custom ore fallback + RequestMiningDetails StructureNotFound
    assert!(compute_mining_materials_per_cycle(&ctx.sde, 30_000_142, 0, &[]).is_empty());
    let fallback_mats = compute_mining_materials_per_cycle(&ctx.sde, 30_000_142, 0, &[(34, 0.5)]);
    assert_eq!(fallback_mats.len(), 1);
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "RequestMiningDetails",
            &[PyRep::Integer(777_666_555)],
            &kw
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
}

#[test]
fn fast_checkout_no_free_pack_fallback_in_paid_mode_emits_item_change_and_readonly_mining() {
    use crate::services::quasar_payment_store_types::{save_store_catalog, StoreCatalog, StoreOffer, StoreProduct};
    use eve_db::TargetedOfferAdminRecord;

    let ctx = test_ctx();
    let uid = ctx.db.provision_player_account("fc_paid_bundle", "fc_pw");
    let cid = ctx
        .db
        .create_character(uid, "Paid Bundle", 1, 1, 11, 17, &ctx.sde)
        .expect("create char")
        .character_id;
    let _ = ctx
        .db
        .acquire_online_lock(uid, 1, Some(cid), false, ctx.now_unix());
    let sess = SessionState {
        station_id: Some(60_003_760),
        ..auth_session(uid, cid, 98_000_001, 30_000_142)
    };
    set_user_fast_checkout_enabled(&ctx, uid, true);
    let kw = HashMap::new();

    // 1. Paid mode with empty catalog does NOT fabricate free packs 1..=4
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = false;
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(snap, &StoreCatalog::default());
    });
    let empty_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        empty_offers
            .dict_get("offers")
            .and_then(|v| v.as_list())
            .map(<[PyRep]>::len),
        Some(0)
    );
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::String("fc_pw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // 2. Switching free_fast_checkout_plex true -> false invalidates cached synthetic packs automatically
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = true;
    let free_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(free_offers
        .dict_get("offers")
        .and_then(|v| v.as_list())
        .is_some_and(|l| !l.is_empty()));
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = false;
    let invalidated_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        invalidated_offers
            .dict_get("offers")
            .and_then(|v| v.as_list())
            .map(<[PyRep]>::len),
        Some(0)
    );

    // 3. BuyOffer enforces targeted eligibility, activates non-cat-1 Omega bundle, AND emits OnItemChange
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 1;
        }
        snap.account_plex_vault.insert(uid, 500);
        let cat = StoreCatalog {
            categories: Vec::new(),
            products: vec![
                StoreProduct {
                    id: 8101,
                    name: "30d Omega Pass".into(),
                    description: "Omega time".into(),
                    type_id: 0,
                    quantity: 1,
                },
                StoreProduct {
                    id: 8102,
                    name: "Bundle SKIN".into(),
                    description: "Physical SKIN".into(),
                    type_id: 34_599,
                    quantity: 1,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 8101,
                    name: "Omega Pilot Bundle".into(),
                    description: "Non-cat-1 Omega + physical item".into(),
                    category_id: 3,
                    price_plex: 120,
                    product_ids: vec![8101, 8102],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 8102,
                    name: "Targeted Exclusive".into(),
                    description: "Restricted to another user".into(),
                    category_id: 2,
                    price_plex: 50,
                    product_ids: vec![8102],
                    targeted: true,
                    price_cents: None,
                    currency: None,
                },
            ],
        };
        save_store_catalog(snap, &cat);
    });
    // BF-02: When snap.quasar_commerce.targeted_offers is empty, targeted offer 8102 still fails closed.
    assert!(ctx.db.with_snapshot(|s| s.quasar_commerce.targeted_offers.is_empty()));
    let _ = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let empty_ta_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let empty_ta_list = empty_ta_offers.dict_get("offers").and_then(|v| v.as_list()).unwrap();
    assert!(!empty_ta_list.iter().any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(8102)));
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(8102), PyRep::Integer(1), PyRep::String("fc_pw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.targeted_offers.insert(
            "9999:8102".into(),
            TargetedOfferAdminRecord {
                user_id: 9999,
                catalogue_entry_name: "8102".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
    });
    let _ = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "ClearFastCheckoutCacheForUser",
        &[],
        &kw,
    );
    let visible_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let visible_list = visible_offers.dict_get("offers").and_then(|v| v.as_list()).unwrap();
    assert!(visible_list
        .iter()
        .any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(8101)));
    assert!(!visible_list
        .iter()
        .any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(8102)));

    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(8102), PyRep::Integer(1), PyRep::String("fc_pw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    let _ = ctx.drain_notifications_for_char(cid);
    let res = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(8101), PyRep::Integer(1), PyRep::String("fc_pw".into())],
        &kw,
    )
    .unwrap()
    .expect("BuyOffer non-cat-1 Omega bundle ok");
    assert_eq!(res.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(cid)), 380);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.clone_grade)), Some(2));
    assert!(ctx.db.with_snapshot(|s| {
        s.items.values().any(|it| it.owner_id == cid && it.type_id == 34_599)
    }));
    let notifs = ctx.drain_notifications_for_char(cid);
    assert!(
        notifs.iter().any(|n| n.event == "OnItemChange"),
        "FastCheckout BuyOffer delivering inventory items must emit OnItemChange"
    );

    // 3b. BF-04 & BF-05: Missing station / active char fail closed with 0 debit, and multi-month Omega uses product quantity.
    let items_before_neg = ctx.db.with_snapshot(|s| s.items.len());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.home_station_id = 0;
        }
    });
    let mut sess_nostat = sess.clone();
    sess_nostat.station_id = None;
    assert_eq!(
        handle(
            &ctx,
            &sess_nostat,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(8101), PyRep::Integer(1), PyRep::String("fc_pw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLocation"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(cid)), 380);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.len()), items_before_neg);

    let uid_nochar = ctx.db.provision_player_account("fc_nochar_paid", "fc_pw");
    set_user_fast_checkout_enabled(&ctx, uid_nochar, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid_nochar, 500);
    });
    let mut sess_nochar = auth_session(uid_nochar, 0, 98_000_001, 30_000_142);
    sess_nochar.character_id = None;
    sess_nochar.station_id = None;
    assert_eq!(
        handle(
            &ctx,
            &sess_nochar,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(8101), PyRep::Integer(1), PyRep::String("fc_pw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.account_plex_vault.get(&uid_nochar).copied()),
        Some(500)
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut cat = crate::services::quasar_payment_store_types::read_store_catalog(snap);
        cat.products.push(StoreProduct {
            id: 8103,
            name: "3 Months Omega".into(),
            description: "90 days Omega".into(),
            type_id: 0,
            quantity: 3,
        });
        cat.offers.push(StoreOffer {
            id: 8103,
            name: "3 Months Omega Offer".into(),
            description: "90d Omega".into(),
            category_id: 1,
            price_plex: 100,
            product_ids: vec![8103],
            targeted: false,
            price_cents: None,
            currency: None,
        });
        save_store_catalog(snap, &cat);
    });
    let _ = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let omega_before = crate::services::quasar_auth_user::read_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.get(&uid).copied().unwrap_or(0)
    });
    let res_90d = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(8103), PyRep::Integer(1), PyRep::String("fc_pw".into())],
        &kw,
    )
    .unwrap()
    .expect("BuyOffer 8103 3-month Omega ok");
    assert_eq!(res_90d.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    let omega_after = crate::services::quasar_auth_user::read_identity_store(&ctx.db, |st| {
        st.omega_subscriptions.get(&uid).copied().unwrap_or(0)
    });
    assert!(
        omega_after >= omega_before + 90 * 86_400,
        "expected >= 90 days Omega added, before={omega_before} after={omega_after}"
    );

    // 4. RequestMiningDetails leaves WorldStateSnapshot untouched
    let (next_eid_before, ledger_before, structs_before) = ctx.db.with_snapshot(|s| {
        (
            s.next_entity_id,
            s.mining_ledger.len(),
            s.structures.len(),
        )
    });
    let _ = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "RequestMiningDetails",
        &[PyRep::Integer(777_666_555)],
        &kw,
    );
    let (next_eid_after, ledger_after, structs_after) = ctx.db.with_snapshot(|s| {
        (
            s.next_entity_id,
            s.mining_ledger.len(),
            s.structures.len(),
        )
    });
    assert_eq!(
        (next_eid_before, ledger_before, structs_before),
        (next_eid_after, ledger_after, structs_after)
    );
}

#[test]
fn fast_checkout_filters_free_weekly_sp_and_aligns_dual_price_billing() {
    use crate::services::quasar_payment_store_types::{
        ensure_store_catalog_seeded, read_store_catalog, save_store_catalog, PaymentTokenRecord,
        StoreOffer, StoreProduct,
    };

    let ctx = test_ctx();
    let kw = HashMap::new();
    let cid = 90_300_555_i64;
    ctx.db.provision_gm_account("fc_billing_pilot", "billpw", cid, "BillingPilot");
    let (uid, corp_id, sys_id, station_id) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&cid).unwrap();
        (ch.user_id, ch.corporation_id, ch.solar_system_id, ch.home_station_id)
    });
    let mut sess = auth_session(uid, cid, corp_id, sys_id);
    sess.station_id = Some(station_id);
    set_user_fast_checkout_enabled(&ctx, uid, true);

    // Seed default NES catalog (includes dual-priced 1001..=1004 and 0-price 1005 weekly free SP)
    // plus a custom fiat-only PLEX pack offer 6001 ($14.99).
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
        ensure_store_catalog_seeded(snap, &ctx.sde);
        let mut cat = read_store_catalog(snap);
        cat.products.push(StoreProduct {
            id: 601,
            name: "300 PLEX".into(),
            description: "300 PLEX".into(),
            type_id: crate::services::market_contracts::PLEX_TYPE_ID,
            quantity: 300,
        });
        cat.offers.push(StoreOffer {
            id: 6001,
            name: "300 PLEX USD".into(),
            description: "Fiat PLEX offer".into(),
            category_id: 1,
            price_plex: 0,
            product_ids: vec![601],
            targeted: false,
            price_cents: Some(1499),
            currency: Some("USD".into()),
        });
        save_store_catalog(snap, &cat);
        snap.account_plex_vault.insert(uid, 700);
        snap.quasar_commerce.payment_tokens = vec![PaymentTokenRecord {
            id: 600_001,
            user_id: uid,
            token_str: "tok_6001".into(),
            is_quick_pay: true,
            consumed: false,
        }];
    });

    // 1. Paid mode: GetOffersForUser omits 1005 (and custom 0-price), advertises 1001/1002 in PLX and 6001 in USD
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = false;
    let _ = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let paid_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let paid_list = paid_offers.dict_get("offers").and_then(PyRep::as_list).unwrap();
    assert!(
        !paid_list
            .iter()
            .any(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1005)),
        "Offer 1005 (0-price weekly free SP) must never appear in FastCheckoutService.GetOffersForUser"
    );

    let o1001 = paid_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1001))
        .expect("1001 in GetOffersForUser");
    assert_eq!(o1001.dict_get("price"), Some(&PyRep::Real(500.0)));
    assert_eq!(o1001.dict_get("pricePlex"), Some(&PyRep::Integer(500)));
    assert_eq!(o1001.dict_get("currency"), Some(&PyRep::String("PLX".into())));

    let o1002 = paid_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1002))
        .expect("1002 in GetOffersForUser");
    assert_eq!(o1002.dict_get("price"), Some(&PyRep::Real(110.0)));
    assert_eq!(o1002.dict_get("currency"), Some(&PyRep::String("PLX".into())));

    let o6001 = paid_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(6001))
        .expect("6001 in GetOffersForUser");
    assert_eq!(o6001.dict_get("price"), Some(&PyRep::Real(14.99)));
    assert_eq!(o6001.dict_get("currency"), Some(&PyRep::String("USD".into())));

    // BuyOffer(1005) returns OfferNotFound
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1005), PyRep::Integer(1), PyRep::String("billpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    // Even if an old cached_offers snapshot still contains 1005, resolve/GetOffersForUser strips it
    let mut stale = get_user_fast_checkout_state(&ctx, uid);
    stale.cached_offers.get_or_insert_with(Vec::new).push(FastCheckoutOfferRecord {
        offer_id: 1005,
        plex_amount: 0,
        price_usd: 0.0,
        price_plex: 0,
        name: "Omega Weekly 15,000 Free SP".into(),
        description: "Stale cache entry".into(),
        tags: vec!["omega-weekly-free-sp".into()],
        products: vec![FastCheckoutProductRecord {
            type_id: 0,
            quantity: 15_000,
        }],
        category_id: 3,
    });
    save_user_fast_checkout_state(&ctx, uid, &stale);
    let stale_checked = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(
        !stale_checked
            .dict_get("offers")
            .and_then(PyRep::as_list)
            .unwrap()
            .iter()
            .any(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1005)),
        "stale cached Offer 1005 must be filtered out"
    );

    // 2. Dual-priced Offer 1002 debits 110 PLEX without consuming the quick-pay token or writing USD receipt
    let buy_1002 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(1002), PyRep::Integer(1), PyRep::String("billpw".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(buy_1002.dict_get("status"), Some(&PyRep::String("CONFIRMED".into())));
    assert_eq!(buy_1002.dict_get("newPlexBalance"), Some(&PyRep::Integer(590)));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.quasar_commerce.payment_receipts.is_empty());
        assert!(!snap.quasar_commerce.payment_tokens[0].consumed);
    });

    // 3. Fiat offer 6001 consumes quick-pay token once, writes receipt, and blocks second purchase
    let buy_6001 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(6001), PyRep::Integer(1), PyRep::String("billpw".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(buy_6001.dict_get("newPlexBalance"), Some(&PyRep::Integer(890)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.quasar_commerce.payment_receipts.len(), 1);
        assert_eq!(snap.quasar_commerce.payment_receipts[0].amount_in_cents, 1499);
        assert!(snap.quasar_commerce.payment_tokens[0].consumed);
    });
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(6001), PyRep::Integer(1), PyRep::String("billpw".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PaymentTokenRequired"
    );

    // 4. Free mode (`free_fast_checkout_plex = true`) keeps 1005 omitted, advertises 6001 as 0.0 FREE
    //    while keeping PLEX-priced 1002 consistent at 110.0 PLX
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = true;
    let _ = handle(&ctx, &sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[], &kw);
    let free_offers = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    let free_list = free_offers.dict_get("offers").and_then(PyRep::as_list).unwrap();
    assert!(
        !free_list
            .iter()
            .any(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1005)),
        "Offer 1005 must remain hidden in free FastCheckout mode"
    );
    let free_6001 = free_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(6001))
        .expect("6001 in free mode");
    assert_eq!(free_6001.dict_get("price"), Some(&PyRep::Real(0.0)));
    assert_eq!(free_6001.dict_get("currency"), Some(&PyRep::String("FREE".into())));
    let free_1002 = free_list
        .iter()
        .find(|o| o.dict_get("offerId").and_then(PyRep::as_i64) == Some(1002))
        .expect("1002 in free mode");
    assert_eq!(free_1002.dict_get("price"), Some(&PyRep::Real(110.0)));
    assert_eq!(free_1002.dict_get("currency"), Some(&PyRep::String("PLX".into())));

    let free_buy_6001 = handle(
        &ctx,
        &sess,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(6001), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(free_buy_6001.dict_get("status"), Some(&PyRep::String("COMPLETED".into())));
    assert_eq!(free_buy_6001.dict_get("newPlexBalance"), Some(&PyRep::Integer(1190)));

    // 5. Malformed non-catalog cached_offers are hidden by GetOffersForUser and rejected by BuyOffer
    ctx.feature_flags.write().unwrap().free_fast_checkout_plex = false;
    let nan_record = FastCheckoutOfferRecord {
        offer_id: 91_003,
        plex_amount: 100,
        price_usd: f64::NAN,
        price_plex: 0,
        name: "Bad NaN usd".into(),
        description: String::new(),
        tags: Vec::new(),
        products: Vec::new(),
        category_id: 1,
    };
    assert_eq!(
        resolve_fast_checkout_billing(&nan_record, false)
            .unwrap_err()
            .code,
        "InvalidOffer"
    );
    let bad_records = vec![
        FastCheckoutOfferRecord {
            offer_id: 91_001,
            plex_amount: 100,
            price_usd: 0.0,
            price_plex: -1,
            name: "Bad neg plex".into(),
            description: String::new(),
            tags: Vec::new(),
            products: Vec::new(),
            category_id: 1,
        },
        FastCheckoutOfferRecord {
            offer_id: 91_002,
            plex_amount: 100,
            price_usd: -5.0,
            price_plex: 0,
            name: "Bad neg usd".into(),
            description: String::new(),
            tags: Vec::new(),
            products: Vec::new(),
            category_id: 1,
        },
        FastCheckoutOfferRecord {
            offer_id: 91_004,
            plex_amount: 100,
            price_usd: 0.0,
            price_plex: 0,
            name: "Bad zero price".into(),
            description: String::new(),
            tags: Vec::new(),
            products: Vec::new(),
            category_id: 1,
        },
        FastCheckoutOfferRecord {
            offer_id: 91_005,
            plex_amount: 100,
            price_usd: 1e12,
            price_plex: 0,
            name: "Bad overflow usd".into(),
            description: String::new(),
            tags: Vec::new(),
            products: Vec::new(),
            category_id: 1,
        },
    ];
    for rec in &bad_records {
        assert!(resolve_fast_checkout_billing(rec, false).is_err());
    }
    let mut bad_state = get_user_fast_checkout_state(&ctx, uid);
    bad_state.cached_offers = Some(bad_records);
    save_user_fast_checkout_state(&ctx, uid, &bad_state);
    let filtered_bad = handle(&ctx, &sess, "FastCheckoutService", "GetOffersForUser", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        filtered_bad
            .dict_get("offers")
            .and_then(PyRep::as_list)
            .map(<[PyRep]>::len),
        Some(0),
        "malformed cached records must never be advertised"
    );
    for (bad_id, expected_code) in [
        (91_001, "InvalidOffer"),
        (91_002, "InvalidOffer"),
        (91_004, "InvalidOffer"),
        (91_005, "ArithmeticOverflow"),
    ] {
        assert_eq!(
            handle(
                &ctx,
                &sess,
                "FastCheckoutService",
                "BuyOffer",
                &[PyRep::Integer(bad_id), PyRep::Integer(1), PyRep::String("billpw".into())],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            expected_code
        );
    }
}
