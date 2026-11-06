//! Unit tests covering `fitting_containers_ships_ops` (`handle_clear_charges`,
//! `invbroker` ops, `itemLocking`, and `encodedItems.DecodeItem`).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx_and_session(char_id: i64, ship_id: i64) -> (ServerContext, SessionState) {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(38u16, 400.0)]),
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(97_100);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(ship_id);
    (ctx, sess)
}

#[tokio::test]
async fn clear_charges_unloads_merges_splits_and_enforces_guards() {
    let char_id = 90_000_001i64;
    let ship_id = 97_200_100i64;
    let (ctx, mut sess) = test_ctx_and_session(char_id, ship_id);

    // Invalid ship target
    let bad_err = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(99_999_999)])
        .await
        .expect_err("missing ship rejected");
    assert_eq!(bad_err.code, "ShipNotFound");

    // Seed module + charge stack + existing partial cargo stack
    let mod_id = 97_200_101i64;
    let charge1_id = 97_200_102i64;
    let charge2_id = 97_200_103i64;
    let existing_cargo_id = 97_200_104i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 2873, // 150mm Light AutoCannon I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge1_id,
            InventoryItemRecord {
                item_id: charge1_id,
                type_id: 192, // Fusion S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge2_id,
            InventoryItemRecord {
                item_id: charge2_id,
                type_id: 193, // Phased Plasma S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 15,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            existing_cargo_id,
            InventoryItemRecord {
                item_id: existing_cargo_id,
                type_id: 192,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let res = handle_clear_charges(&ctx, &sess, None, &[PyRep::Integer(ship_id)])
        .await
        .expect("clear charges succeeds");
    assert_eq!(res, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        // charge1 merged into existing_cargo_id (10 + 20 = 30) and removed
        assert!(!snap.items.contains_key(&charge1_id));
        assert_eq!(snap.items.get(&existing_cargo_id).map(|i| i.quantity), Some(30));
        // charge2 moved directly to Cargo flag
        assert_eq!(
            snap.items.get(&charge2_id).map(|i| (i.flag, i.quantity)),
            Some((InventoryFlag::Cargo, 15))
        );
    });

    // Locked ship rejected
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.flag = InventoryFlag::Locked;
        }
    });
    sess.ship_id = Some(ship_id);
    let locked_err = handle_clear_charges(&ctx, &sess, None, &[])
        .await
        .expect_err("locked ship rejected");
    assert_eq!(locked_err.code, "ItemLocked");
}

#[tokio::test]
async fn ext_tail_covers_invbroker_item_locking_and_encoded_items() {
    let char_id = 90_000_001i64;
    let ship_id = 97_200_200i64;
    let (ctx, mut sess) = test_ctx_and_session(char_id, ship_id);
    let kw = HashMap::new();

    // Seed PLEX item + corp vote-locked item + custom named item with mutaplasmid
    let plex_id = 97_200_201i64;
    let locked_item_id = 97_200_202i64;
    let encoded_item_id = 97_200_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_id,
            InventoryItemRecord {
                item_id: plex_id,
                type_id: 44_992,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_item_id,
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: 587,
                owner_id: 1_000_009,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            encoded_item_id,
            InventoryItemRecord {
                item_id: encoded_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                is_blueprint_copy: true,
                source_mutaplasmid_id: Some(49_722),
                dynamic_attributes: HashMap::from([(9u16, 125.0)]),
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(encoded_item_id, "Abyssal Rifter".into());
    });

    // GetAssetWorth
    let worth = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "GetAssetWorth", &[], &kw)
        .await
        .expect("handled")
        .expect("worth");
    assert!(matches!(worth, PyRep::Tuple(t) if t.len() == 2));

    // GetPriceForItems
    let price = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "GetPriceForItems",
        &[PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(plex_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("price");
    assert!(matches!(price, PyRep::Real(_)));

    // HasExistingPasswordSet, ImportExportWithPlanet, Reprocess, RetrievePassword
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "HasExistingPasswordSet", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .expect("has pw"),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[], &kw)
            .await
            .expect("handled")
            .expect("pi"),
        PyRep::None
    );
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "Reprocess", &[], &kw)
            .await
            .expect("handled")
            .expect("reprocess"),
        PyRep::List(Vec::new())
    );
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[], &kw)
            .await
            .expect("handled")
            .expect("ret pw"),
        PyRep::None
    );

    // ListIncludingContainers & ListStations
    let listed = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ListIncludingContainers",
        &[PyRep::Integer(60_003_760)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("list");
    assert!(!matches!(listed, PyRep::None));

    let stations = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "ListStations", &[], &kw)
        .await
        .expect("handled")
        .expect("stations");
    assert!(matches!(stations, PyRep::Rowset { .. }));

    // itemLocking
    let locked_locs = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "itemLocking",
        "GetLockedItemLocations",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("locked locs");
    assert_eq!(locked_locs, PyRep::List(vec![PyRep::Integer(60_003_760)]));

    let locked_by_loc = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "itemLocking",
        "GetItemsByLocation",
        &[PyRep::Integer(60_003_760)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("locked by loc");
    assert!(matches!(locked_by_loc, PyRep::Dict(d) if !d.is_empty()));

    // encodedItems.DecodeItem (itemID, string link, missing arg error, unknown type error)
    let dec = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "encodedItems",
        "DecodeItem",
        &[PyRep::Integer(encoded_item_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("decode item");
    assert_eq!(dec.dict_get("customName"), Some(&PyRep::String("Abyssal Rifter".into())));
    assert_eq!(dec.dict_get("isBlueprintCopy"), Some(&PyRep::Bool(true)));
    assert_eq!(dec.dict_get("sourceMutaplasmidID"), Some(&PyRep::Integer(49_722)));

    let dec_str = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "encodedItems",
        "DecodeItem",
        &[PyRep::String("587:587".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("decode str link");
    assert_eq!(dec_str.dict_get("typeID"), Some(&PyRep::Integer(587)));

    let missing_err = handle_ext_tail(&ctx, &mut sess, None, "encodedItems", "DecodeItem", &[], &kw)
        .await
        .expect("handled")
        .expect_err("missing arg rejected");
    assert_eq!(missing_err.code, "InvalidArgument");

    let unknown_type_err = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "encodedItems",
        "DecodeItem",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unknown type rejected");
    assert_eq!(unknown_type_err.code, "TypeNotFound");
}
