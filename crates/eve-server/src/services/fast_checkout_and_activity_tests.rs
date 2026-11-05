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
