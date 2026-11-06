//! Extra branch-coverage and mutation-killing unit tests for `cosmetics_ui_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::cosmetics_ui_ops::{
    handle, load_cosmetics_ui_state, notify_skins_removed, purge_expired_skins_for_char,
    record_skin_license_activation, save_cosmetics_ui_state, SkinLicenseEntry,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn dispatch_ui(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, sess, svc, method, args, &kw).await
}

async fn call_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    dispatch_ui(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect("ok")
}

async fn call_err(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> String {
    dispatch_ui(ctx, sess, svc, method, args)
        .await
        .expect("handled")
        .expect_err("err")
        .code
}

#[test]
fn skin_activation_expiry_boundaries_and_multi_ship_purge() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let ship_a = 100_000_001_i64;
    let ship_b = 100_000_902_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let exp1 = record_skin_license_activation(snap, cid, 50_001, 601, 2, 10_000);
        assert_eq!(exp1, Some(10_000 + 2 * 86_400));

        // Boundary: when existing exp == now_unix (not > now_unix), it resets from now_unix
        let exp_at_boundary =
            record_skin_license_activation(snap, cid, 50_001, 601, 1, 10_000 + 2 * 86_400);
        assert_eq!(exp_at_boundary, Some(10_000 + 3 * 86_400));

        // When existing exp < now_unix (already expired), it starts from now_unix
        let exp_after_expired =
            record_skin_license_activation(snap, cid, 50_001, 601, 1, 2_000_000);
        assert_eq!(exp_after_expired, Some(2_000_000 + 86_400));

        // duration_days == 0 means permanent (None)
        assert_eq!(record_skin_license_activation(snap, cid, 50_001, 601, 0, 2_000_100), None);
        assert!(purge_expired_skins_for_char(snap, cid, 3_000_000).is_empty());

        if let Some(sa) = snap.items.get_mut(&ship_a) {
            sa.owner_id = cid;
            sa.type_id = 587;
        }
        snap.items.insert(
            ship_b,
            InventoryItemRecord {
                item_id: ship_b,
                type_id: 588,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut ext = load_cosmetics_ui_state(snap);
        ext.skin_license_entries.insert(
            cid,
            vec![
                SkinLicenseEntry {
                    skin_id: 701,
                    license_type_id: 50_701,
                    activated_at_unix: 100,
                    expires_at_unix: Some(5_000),
                },
                SkinLicenseEntry {
                    skin_id: 702,
                    license_type_id: 50_702,
                    activated_at_unix: 100,
                    expires_at_unix: Some(4_999),
                },
                SkinLicenseEntry {
                    skin_id: 703,
                    license_type_id: 50_703,
                    activated_at_unix: 100,
                    expires_at_unix: Some(5_001),
                },
            ],
        );
        save_cosmetics_ui_state(snap, &ext);
        snap.licensed_skins
            .insert(cid, vec![701, 50_701, 702, 50_702, 703, 50_703]);
        snap.applied_ship_skins.insert(ship_a, 50_701);
        snap.applied_ship_skins.insert(ship_b, 701);

        let removed = purge_expired_skins_for_char(snap, cid, 5_000);
        assert!(removed.contains(&(701, ship_a, 587)));
        assert!(removed.contains(&(701, ship_b, 588)));
        assert!(removed.contains(&(702, 0, 0)));
        assert!(!removed.iter().any(|&(sk, _, _)| sk == 703));
        assert_eq!(snap.licensed_skins.get(&cid), Some(&vec![703, 50_703]));
        assert!(!snap.applied_ship_skins.contains_key(&ship_a));
        assert!(!snap.applied_ship_skins.contains_key(&ship_b));
    });

    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    notify_skins_removed(&ctx, cid, &mut sess, &[(701, ship_a, 587), (702, 0, 0)]);
}

#[tokio::test]
async fn activate_and_apply_skin_validation_and_stack_consumption() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    let (lic_tid, lic_name, skin_id, ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, st.name.clone(), sk.skin_id, sk.types[0]))
        })
        .unwrap();
    let wrong_ship_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.category_id == 6
                && t.type_id != ship_tid
                && !ctx.sde.skins[&skin_id].types.contains(&t.type_id)
        })
        .unwrap()
        .type_id;

    assert!(dispatch_ui(&ctx, &mut sess, "dogmaIM", "GetAllInfo", &[]).await.is_none());

    let mut unauth = SessionState::default();
    assert!(dispatch_ui(&ctx, &mut unauth, "shipCosmeticsMgr", "ActivateSkinLicense", &[])
        .await
        .unwrap()
        .is_err());

    for bad_arg in [vec![], vec![PyRep::Integer(-5)], vec![PyRep::Integer(999_999_999)], vec![PyRep::Integer(34)]] {
        assert_eq!(
            call_err(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &bad_arg).await,
            "InvalidSkinLicenseType"
        );
    }
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64), PyRep::Integer(wrong_ship_tid as i64)]
        )
        .await,
        "InvalidShipTypeForSkin"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64)]
        )
        .await,
        "SkinLicenseItemNotFound"
    );

    let lic_item_id = 945_001_i64;
    let other_ship_id = 945_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lic_item_id,
            InventoryItemRecord {
                item_id: lic_item_id,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_ship_id,
            InventoryItemRecord {
                item_id: other_ship_id,
                type_id: ship_tid,
                owner_id: other_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = cid;
            sh.type_id = ship_tid;
            sh.location_id = station_id;
        }
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(station_id);
        }
    });
    let act_res = call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64), PyRep::Integer(ship_tid as i64)],
    )
    .await;
    assert_eq!(act_res.dict_get("skinID"), Some(&PyRep::Integer(skin_id as i64)));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&lic_item_id).expect("stack of 2 should remain with qty 1");
        assert_eq!((it.quantity, it.version), (1, 2));
    });

    for bad_ship_args in [
        vec![],
        vec![PyRep::Integer(lic_item_id), PyRep::None],
        vec![PyRep::Integer(888_888_888), PyRep::None],
        vec![PyRep::Integer(wrong_ship_tid as i64), PyRep::Integer(skin_id as i64)],
    ] {
        assert_eq!(
            call_err(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &bad_ship_args).await,
            "InvalidShipTypeForSkin"
        );
    }
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[PyRep::Integer(other_ship_id), PyRep::None]
        )
        .await,
        "OwnershipViolation"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[PyRep::Integer(wrong_ship_tid as i64), PyRep::Integer(0)]
        )
        .await,
        "ShipTypeMismatch"
    );

    for bad_skin_arg in [
        PyRep::Integer(-1),
        PyRep::Integer(999_999_999),
        PyRep::String("Nonexistent SKIN Name".into()),
        PyRep::Bool(true),
    ] {
        assert_eq!(
            call_err(
                &ctx,
                &mut sess,
                "shipCosmeticsMgr",
                "ApplySkinToShip",
                &[PyRep::Integer(ship_id), bad_skin_arg]
            )
            .await,
            "SkinNotLicensed"
        );
    }

    let by_name = call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_tid as i64), PyRep::String(lic_name.to_uppercase())],
    )
    .await;
    assert_eq!(by_name.dict_get("sequenced"), Some(&PyRep::Bool(true)));

    let by_lic_id = call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::Integer(lic_tid as i64)],
    )
    .await;
    assert_eq!(by_lic_id.dict_get("skinID"), Some(&PyRep::Integer(skin_id as i64)));

    let cleared = call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_tid as i64), PyRep::Integer(0)],
    )
    .await;
    assert_eq!(cleared.dict_get("sequenced"), Some(&PyRep::Bool(false)));
}

#[tokio::test]
async fn gm_skin_roles_first_party_data_enabled_cosmetics_and_ship_type_queries() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let target_cid = 90_000_002_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);

    let c1 = call_err(&ctx, &mut sess, "shipCosmeticsMgr", "RemoveSkin", &[PyRep::Integer(100)]).await;
    let c2 = call_err(&ctx, &mut sess, "shipCosmeticsMgr", "GMExpireSkinLicense", &[PyRep::Integer(100)]).await;
    assert_eq!((c1.as_str(), c2.as_str()), ("InsufficientRole", "InsufficientRole"));
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GetFirstPartySkinData",
            &[PyRep::Integer(target_cid), PyRep::Integer(100)]
        )
        .await,
        "CharacterOwnershipMismatch"
    );
    assert_eq!(
        call_ok(&ctx, &mut sess, "shipCosmeticsMgr", "GetFirstPartySkinData", &[]).await,
        PyRep::None
    );
    assert_eq!(
        call_ok(&ctx, &mut sess, "shipCosmeticsMgr", "GetFirstPartySkinData", &[PyRep::Integer(0)]).await,
        PyRep::None
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.licensed_skins.insert(target_cid, vec![777, 778]);
    });
    sess.role = service_roles::ROLE_GML;
    let fp = call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetFirstPartySkinData",
        &[PyRep::Integer(target_cid), PyRep::Integer(777)],
    )
    .await;
    assert_eq!(fp.dict_get("licenseeID"), Some(&PyRep::Integer(target_cid)));

    call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "RemoveSkin",
        &[PyRep::Integer(777), PyRep::Integer(target_cid)],
    )
    .await;
    assert_eq!(
        call_ok(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GetFirstPartySkinData",
            &[PyRep::Integer(target_cid), PyRep::Integer(777)]
        )
        .await,
        PyRep::None
    );
    call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GMExpireSkinLicense",
        &[PyRep::Integer(778), PyRep::Integer(target_cid)],
    )
    .await;
    assert!(ctx.db.with_snapshot(|s| s.licensed_skins[&target_cid].is_empty()));

    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GetLicencedSkinsForShipType",
            &[PyRep::Integer(cid), PyRep::Integer(34)]
        )
        .await,
        "InvalidShipTypeForSkin"
    );
    assert!(call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetLicencedSkinsForShipType",
        &[PyRep::Integer(cid), PyRep::Integer(587)]
    )
    .await
    .as_list()
    .is_some());
    assert!(call_ok(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetEnabledCosmetics",
        &[PyRep::Integer(100_000_001)]
    )
    .await
    .as_dict()
    .is_some());
}

#[tokio::test]
async fn links_guests_achievements_offer_ring_buffer_and_crate_delegation() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);

    sess.station_id = None;
    assert_eq!(call_err(&ctx, &mut sess, "station", "GetGuests", &[]).await, "NotDockedInStation");
    assert_eq!(
        call_err(&ctx, &mut sess, "overviewPresetMgr", "StoreLinkAndGetID", &[]).await,
        "EmptyLinkPayload"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "overviewPresetMgr",
            "StoreLinkAndGetID",
            &[PyRep::String("   ".into()), PyRep::String("data".into())]
        )
        .await,
        "InvalidLinkName"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "overviewPresetMgr",
            "StoreLinkAndGetID",
            &[PyRep::String("valid".into()), PyRep::None]
        )
        .await,
        "EmptyLinkPayload"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "sharedSettingsMgr",
            "StoreSettingLinkAndGetID",
            &[PyRep::String("x".repeat(65_537))]
        )
        .await,
        "PayloadTooLarge"
    );

    let single_stored = call_ok(
        &ctx,
        &mut sess,
        "sharedSettingsMgr",
        "StoreSettingLinkAndGetID",
        &[PyRep::Integer(42)],
    )
    .await;
    assert_eq!(single_stored.dict_get("settingType"), Some(&PyRep::String("default".into())));
    assert_eq!(
        call_ok(
            &ctx,
            &mut sess,
            "sharedSettingsMgr",
            "GetStoredSharedSetting",
            &[PyRep::String("default".into())]
        )
        .await,
        PyRep::String("Integer(42)".into())
    );

    assert_eq!(
        call_err(&ctx, &mut sess, "overviewPresetMgr", "GetStoredPreset", &[]).await,
        "MissingLinkId"
    );
    for nf_arg in [PyRep::Integer(999_999), PyRep::Bool(false)] {
        assert_eq!(
            call_err(&ctx, &mut sess, "overviewPresetMgr", "GetStoredPreset", &[nf_arg]).await,
            "StoredLinkNotFound"
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.characters_warped.push(cid);
        let log = ext.contextual_offer_events.entry(cid).or_default();
        log.clear();
        for i in 1..=256 {
            log.push(("TrackOfferSeen".into(), i, 1_000));
        }
        save_cosmetics_ui_state(snap, &ext);
    });
    assert_eq!(
        call_ok(&ctx, &mut sess, "monolithTaskTrackerMgr", "HasEverWarped", &[]).await,
        PyRep::Bool(true)
    );
    call_ok(
        &ctx,
        &mut sess,
        "monolithTaskTrackerMgr",
        "UpdateClientAchievmentsAndCounters",
        &[
            PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Bool(true))]),
            PyRep::Dict(vec![(PyRep::Integer(100), PyRep::Integer(5))]),
        ],
    )
    .await;

    for (idx, m) in [
        "TrackOfferBuyPress",
        "TrackOfferClosed",
        "TrackOfferDelivered",
        "TrackOfferPurchased",
        "TrackOfferPurchasedSeen",
        "TrackOfferSeen",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            call_ok(&ctx, &mut sess, "contextualOfferMgr", m, &[PyRep::Integer(500 + idx as i64)]).await,
            PyRep::None
        );
    }
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let log = &ext.contextual_offer_events[&cid];
        assert_eq!(log.len(), 256);
        assert_eq!((log.first().unwrap().1, log.last().unwrap().1), (7, 505));
    });
}
