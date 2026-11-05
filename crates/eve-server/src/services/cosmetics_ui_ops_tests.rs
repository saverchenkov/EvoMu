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

#[tokio::test]
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

#[tokio::test]
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

    // station.GetGuests
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
    });
    let guests = handle(&ctx, &mut sess, "station", "GetGuests", &[], &kw)
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
