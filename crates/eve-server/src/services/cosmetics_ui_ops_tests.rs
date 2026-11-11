//! Unit tests for `cosmetics_ui_ops` (`shipCosmeticsMgr`, `overviewPresetMgr`, `sharedSettingsMgr`, `station`, `monolithTaskTrackerMgr`, `contextualOfferMgr`, `nes_intro`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skin_licenses_expiry_apply_and_gm_tools() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Find a valid category 91 SKIN license and its compatible ship type
    let (lic_tid, skin_id, ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, sk.skin_id, sk.types[0]))
        })
        .unwrap();

    // Test record_skin_license_activation & purge_expired_skins_for_char directly
    ctx.db.with_mut_snapshot(|snap| {
        let exp1 = record_skin_license_activation(snap, cid, lic_tid, skin_id, 7, 1_000);
        assert_eq!(exp1, Some(1_000 + 7 * 86_400));
        // Extending an active timed license adds to existing expiry
        let exp2 = record_skin_license_activation(snap, cid, lic_tid, skin_id, 3, 2_000);
        assert_eq!(exp2, Some(1_000 + 10 * 86_400));
        // Permanent license (duration <= 0) sets expires_at to None and is never downgraded
        assert_eq!(
            record_skin_license_activation(snap, cid, lic_tid, skin_id, -1, 3_000),
            None
        );
        assert_eq!(
            record_skin_license_activation(snap, cid, lic_tid, skin_id, 5, 4_000),
            None
        );

        // Now set an expired timed license and verify purge_expired_skins_for_char strips ship skin
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = cid;
            ship.type_id = ship_tid;
            ship.location_id = station_id;
        }
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(station_id);
        }
        let mut ext = load_cosmetics_ui_state(snap);
        ext.skin_license_entries.insert(
            cid,
            vec![SkinLicenseEntry {
                skin_id: 999_888,
                license_type_id: 999_889,
                activated_at_unix: 100,
                expires_at_unix: Some(500),
            }],
        );
        save_cosmetics_ui_state(snap, &ext);
        snap.licensed_skins.entry(cid).or_default().push(999_888);
        snap.applied_ship_skins.insert(ship_id, 999_888);

        let purged = purge_expired_skins_for_char(snap, cid, 1_000);
        assert_eq!(purged, vec![(999_888, ship_id, ship_tid)]);
        assert!(!snap.applied_ship_skins.contains_key(&ship_id));
    });

    // ActivateSkinLicense + ApplySkinToShip + GetLicencedSkins + GetFirstPartySkinData
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            940_001,
            InventoryItemRecord {
                item_id: 940_001,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let act = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act.dict_get("licensed"), Some(&PyRep::Bool(true)));

    let apply = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::Integer(skin_id as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        apply.dict_get("skinID"),
        Some(&PyRep::Integer(skin_id as i64))
    );

    // Clear skin with None
    let clear = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(clear.dict_get("skinID"), Some(&PyRep::None));

    let lics = handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkins", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!lics.as_list().unwrap().is_empty());

    let lics_for_ship = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetLicencedSkinsForShipType",
        &[PyRep::Integer(ship_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!lics_for_ship.as_list().unwrap().is_empty());

    let fp_data = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetFirstPartySkinData",
        &[PyRep::Integer(skin_id as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        fp_data.dict_get("skinID"),
        Some(&PyRep::Integer(skin_id as i64))
    );

    // GM RemoveSkin & GMExpireSkinLicense
    sess.role = service_roles::ROLE_GMH;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "RemoveSkin",
            &[PyRep::Integer(skin_id as i64)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GMExpireSkinLicense",
            &[PyRep::Integer(lic_tid as i64)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overview_presets_shared_settings_guests_achievements_offers_and_nes_intro() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();

    // Overview presets & shared settings round-trip
    let stored_ov = handle(
        &ctx,
        &mut sess,
        "overviewPresetMgr",
        "StoreLinkAndGetID",
        &[
            PyRep::String("MyPvP".into()),
            PyRep::String("yaml: preset_data".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let sq_id = stored_ov.dict_get("sqID").and_then(|v| v.as_i64()).unwrap();
    let hash_val = stored_ov
        .dict_get("hashvalue")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "overviewPresetMgr",
            "GetStoredPreset",
            &[PyRep::Integer(sq_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::String("yaml: preset_data".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "overviewPresetMgr",
            "GetStoredPreset",
            &[PyRep::String(hash_val)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::String("yaml: preset_data".into())
    );

    let stored_sh = handle(
        &ctx,
        &mut sess,
        "sharedSettingsMgr",
        "StoreSettingLinkAndGetID",
        &[
            PyRep::String("window_colors".into()),
            PyRep::String("theme: dark".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let link_id = stored_sh
        .dict_get("linkID")
        .and_then(|v| v.as_i64())
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "sharedSettingsMgr",
            "GetStoredSharedSetting",
            &[PyRep::Integer(link_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::String("theme: dark".into())
    );

    // station.GetGuests is handled by station_structure (removed from cosmetics_ui_ops)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
    });
    assert!(handle(&ctx, &mut sess, "station", "GetGuests", &[], &kw).await.is_none());
    let guests = crate::services::station_structure::handle(&ctx, &mut sess, "station", "GetGuests", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(!guests.as_list().unwrap().is_empty());

    // monolithTaskTrackerMgr
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "monolithTaskTrackerMgr",
            "HasEverWarped",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "monolithTaskTrackerMgr",
            "UpdateClientAchievmentsAndCounters",
            &[
                PyRep::List(vec![PyRep::Integer(1)]),
                PyRep::Dict(vec![(PyRep::String("evt".into()), PyRep::Integer(5))]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let ach = handle(
        &ctx,
        &mut sess,
        "monolithTaskTrackerMgr",
        "GetCompletedAchievementsAndClientEventCount",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ach.dict_get("completedDict").is_some());

    // contextualOfferMgr & nes_intro
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "contextualOfferMgr",
            "TrackOfferSeen",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOfferID"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "contextualOfferMgr",
            "TrackOfferSeen",
            &[PyRep::Integer(101)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    assert_eq!(
        handle(&ctx, &mut sess, "nes_intro", "get_nes_intro_state", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(&ctx, &mut sess, "nes_intro", "skip_nes_intro", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "nes_intro", "get_nes_intro_state", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cosmetics_ui_ops_validation_errors_and_fallbacks() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Unknown service/method -> None
    let non_matching_svc = "other";
    let non_matching_method = "Foo";
    assert!(handle(&ctx, &mut sess, non_matching_svc, non_matching_method, &[], &kw).await.is_none());

    // Find valid SKIN license & compatible ship type
    let (lic_tid, skin_id, ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, sk.skin_id, sk.types[0]))
        })
        .unwrap();
    let lic_name = ctx.sde.types[&lic_tid].name.clone();

    // ActivateSkinLicense error paths
    for args in [
        vec![],
        vec![PyRep::Integer(-1)],
        vec![PyRep::Integer(999_999_999)],
        vec![PyRep::Integer(34)],
        vec![PyRep::Integer(lic_tid as i64), PyRep::Integer(34)],
        vec![PyRep::Integer(lic_tid as i64)],
    ] {
        assert!(
            handle(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &args, &kw)
                .await
                .unwrap()
                .is_err()
        );
    }

    // ActivateSkinLicense with quantity=2 decrements quantity to 1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = cid;
            ship.type_id = ship_tid;
            ship.location_id = station_id;
        }
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            941_001,
            InventoryItemRecord {
                item_id: 941_001,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
        // Non-ship item and foreign ship for ApplySkinToShip
        snap.items.insert(
            941_002,
            InventoryItemRecord {
                item_id: 941_002,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            941_003,
            InventoryItemRecord {
                item_id: 941_003,
                type_id: ship_tid,
                owner_id: other_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // ApplySkinToShip error paths before licensing
    for args in [
        vec![],
        vec![PyRep::Integer(941_002), PyRep::Integer(skin_id as i64)],
        vec![PyRep::Integer(941_003), PyRep::Integer(skin_id as i64)],
        vec![PyRep::Integer(999_999_999), PyRep::Integer(skin_id as i64)],
        vec![PyRep::Integer(ship_id), PyRep::Integer(-1)],
        vec![PyRep::Integer(ship_id), PyRep::Integer(999_999_999)],
        vec![PyRep::Integer(ship_id), PyRep::String("Nonexistent Skin XYZ".into())],
        vec![PyRep::Integer(ship_id), PyRep::Bool(false)],
        vec![PyRep::Integer(ship_id), PyRep::Integer(34)], // invalid ship? wait, ship_id ok, 34 not skin
        vec![PyRep::Integer(ship_id), PyRep::Integer(skin_id as i64)], // not licensed yet
    ] {
        assert!(
            handle(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &args, &kw)
                .await
                .unwrap()
                .is_err()
        );
    }

    // Activate qty=2 -> qty=1, then apply by design name, then clear with None
    handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&941_001).map(|i| i.quantity)),
        Some(1)
    );

    let applied_name = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::String(lic_name)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(applied_name.dict_get("sequenced"), Some(&PyRep::Bool(true)));

    let cleared = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cleared.dict_get("sequenced"), Some(&PyRep::Bool(false)));

    // RemoveSkin & GMExpireSkinLicense non-GM rejected, then GM accepted
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "RemoveSkin", &[PyRep::Integer(skin_id as i64)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GMExpireSkinLicense", &[PyRep::Integer(skin_id as i64)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    let mut gm_sess = sess.clone();
    gm_sess.role = service_roles::ROLE_GMH;
    assert_eq!(
        handle(&ctx, &mut gm_sess, "shipCosmeticsMgr", "GMExpireSkinLicense", &[PyRep::Integer(skin_id as i64)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // GetLicencedSkinsForShipType (invalid, 1-arg, 2-arg) & GetEnabledCosmetics
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(34)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidShipTypeForSkin"
    );
    assert!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(ship_tid as i64)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(cid), PyRep::Integer(ship_tid as i64)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &kw)
            .await
            .unwrap()
            .is_ok()
    );

    // GetFirstPartySkinData edge cases
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetFirstPartySkinData", &[PyRep::Integer(other_cid), PyRep::Integer(skin_id as i64)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterOwnershipMismatch"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetFirstPartySkinData", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GetFirstPartySkinData", &[PyRep::Integer(999_999_111)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // OverviewPreset & SharedSettings error paths
    assert_eq!(
        handle(&ctx, &mut sess, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String("  ".into()), PyRep::String("x".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLinkName"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "overviewPresetMgr", "StoreLinkAndGetID", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "EmptyLinkPayload"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String("x".repeat(70_000))], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "PayloadTooLarge"
    );
    let stored = handle(&ctx, &mut sess, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String("myPreset".into()), PyRep::String("yaml: 1".into())], &kw)
        .await
        .unwrap()
        .unwrap();
    let hash_str = match stored.dict_get("hashvalue").unwrap() {
        PyRep::String(s) => s.clone(),
        _ => panic!("expected hashvalue"),
    };
    assert_eq!(
        handle(&ctx, &mut sess, "overviewPresetMgr", "GetStoredPreset", &[PyRep::String(hash_str)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::String("yaml: 1".into())
    );
    assert_eq!(
        handle(&ctx, &mut sess, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StoredLinkNotFound"
    );

    // station.GetGuests when undocked vs docked
    let mut undocked = sess.clone();
    undocked.station_id = None;
    let undock_cid = undocked.character_id.unwrap();
    let saved_station = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&undock_cid).unwrap();
        let prev = ch.station_id;
        ch.station_id = None;
        prev
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut undocked, "station", None, "GetGuests", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "NotDockedInStation"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&undock_cid).unwrap().station_id = saved_station;
    });
    assert!(ctx.dispatch_rpc(&mut sess, "station", None, "GetGuests", &[], &kw, None).await.is_ok());

    // UpdateClientAchievmentsAndCounters with Tuple & Dict
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "monolithTaskTrackerMgr",
            "UpdateClientAchievmentsAndCounters",
            &[
                PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Bool(true))]),
                PyRep::Dict(vec![(PyRep::Integer(99), PyRep::Integer(3))]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // contextualOfferMgr remaining methods & crateService delegation
    for m in [
        "TrackOfferBuyPress",
        "TrackOfferClosed",
        "TrackOfferDelivered",
        "TrackOfferPurchased",
        "TrackOfferPurchasedSeen",
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "contextualOfferMgr", m, &[PyRep::Integer(42)], &kw)
                .await
                .unwrap()
                .unwrap(),
            PyRep::None
        );
    }
    for m in [
        "OpenCrate",
        "ClaimLoot",
        "ClaimSkillBundle",
        "ClaimFitting",
        "ClaimLootFromCrateStack",
        "GetCrateLocation",
        "PeekInsideFixedCrate",
    ] {
        assert!(
            handle(&ctx, &mut sess, "crateService", m, &[PyRep::Integer(0)], &kw)
                .await
                .unwrap()
                .is_err()
        );
    }

    // Exercise claimed_days_by_char_campaign_serde (colon + legacy bare char key),
    // purge_expired_skins_for_char + notify_skins_removed with applied ship,
    // MAX_STORED_LINKS eviction, MAX_TRACKED_OFFER_EVENTS eviction, and UpdateClientAchievmentsAndCounters with counters + client ach IDs.
    let parsed_ext: CosmeticsUiExtendedState = serde_json::from_str(
        r#"{"skin_license_entries":{},"next_link_id":0,"overview_presets":{},"shared_settings":{},"achievements_by_char":{},"achievement_counters_by_char":{},"contextual_offer_events":{},"nes_intro_skipped":{},"opened_crates":{},"claimed_crate_loot":{},"claimed_milestones":{},"login_campaign_claimed_days":{"1001:2":[1,3],"1002":[5]}}"#,
    )
    .unwrap();
    assert_eq!(parsed_ext.login_campaign_claimed_days.get(&(1001, 2)), Some(&vec![1, 3]));
    assert!(parsed_ext.login_campaign_claimed_days.contains_key(&(1002, crate::services::campaigns_and_milestones::DEFAULT_LOGIN_CAMPAIGN_ID)));
    let reencoded = serde_json::to_string(&parsed_ext).unwrap();
    assert!(reencoded.contains("1001:2"));

    // Expired skin applied to ship + unapplied expired skin -> purged on ApplySkinToShip(ship, None)
    let cid = sess.character_id.unwrap();
    let ship_id = ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id).unwrap_or(0));
    let ship_tid = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id).unwrap_or(588));
    let (sk1, sk2) = {
        let mut it = ctx.sde.skins.keys().copied();
        (it.next().unwrap_or(1), it.next().unwrap_or(2))
    };
    let past = ctx.now_unix().saturating_sub(100);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.skin_license_entries.insert(cid, vec![
            SkinLicenseEntry { skin_id: sk1, license_type_id: sk1, activated_at_unix: past.saturating_sub(86_400), expires_at_unix: Some(past) },
            SkinLicenseEntry { skin_id: sk2, license_type_id: sk2, activated_at_unix: past.saturating_sub(86_400), expires_at_unix: Some(past) },
        ]);
        for i in 1..=10_000_i64 {
            ext.shared_settings.insert(i, StoredLinkRecord {
                owner_char_id: cid, name: format!("s{i}"), data: PyRep::String("y".into()),
                hash: format!("h{i}"), setting_type: "t".into(),
            });
        }
        ext.contextual_offer_events.insert(cid, (0..256).map(|n| ("TrackOfferShown".into(), n, past)).collect());
        save_cosmetics_ui_state(snap, &ext);
        snap.applied_ship_skins.insert(ship_id, sk1);
    });
    assert!(handle(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(i64::from(ship_tid)), PyRep::None], &kw).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "sharedSettingsMgr", "StoreSettingLinkAndGetID", &[PyRep::String("snew".into()), PyRep::String("yaml: 1".into())], &kw).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "contextualOfferMgr", "TrackOfferDelivered", &[PyRep::Integer(999)], &kw).await.unwrap().is_ok());

    // UpdateClientAchievmentsAndCounters with valid counter key "client_zoom" & any client achievement ID
    let client_aids: Vec<PyRep> = ctx.quasar_gateway.progression.achievements.definitions
        .iter()
        .filter(|d| d.is_client_achievement)
        .take(2)
        .map(|d| PyRep::Integer(i64::from(d.id)))
        .collect();
    let upd_rep = handle(
        &ctx,
        &mut sess,
        "monolithTaskTrackerMgr",
        "UpdateClientAchievmentsAndCounters",
        &[
            PyRep::List(client_aids),
            PyRep::Dict(vec![
                (PyRep::String("client_zoom".into()), PyRep::Integer(7)),
                (PyRep::String("".into()), PyRep::Integer(1)),
            ]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(upd_rep, PyRep::None);
}

#[cfg(test)]
#[path = "cosmetics_ui_ops_extra_unit_tests.rs"]
mod cosmetics_ui_ops_extra_unit_tests;
