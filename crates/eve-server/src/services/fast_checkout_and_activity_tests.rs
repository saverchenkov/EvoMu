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
    // Zero-price offer in paid mode -> InvalidOffer
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
        "InvalidOffer"
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

    // InvalidOffer negative price_plex and unpriced offer in paid mode
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
        "InvalidOffer"
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
        "InvalidOffer"
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
            &[PyRep::Integer(5003), PyRep::Integer(2)],
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
