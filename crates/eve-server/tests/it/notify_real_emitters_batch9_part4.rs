//! Real server-side emission tests for Notify Batch 9 (Part 4: Facilities, Industry, Trade, Market, Godma & Cache).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION,
    PILOT_A, PILOT_B, SYS_JITA,
};
use crate::notify_real_emitters_batch9::{login_docked, upwell_struct};
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use glam::DVec3;

// parity: notify:OnFacilitiesUpdated, notify:OnPrimingNeededForTradeItems, notify:OnSellRepackageErrors
#[tokio::test]
async fn notify_batch9_facilities_industry_trade_and_repackage_errors() {
    eve_testkit::attest(&[
        "notify:OnFacilitiesUpdated",
        "notify:OnPrimingNeededForTradeItems",
        "notify:OnSellRepackageErrors",
    ]);

    let ctx = new_ctx();
    let ship_a = 940_000_001_i64;
    let mut sess_a = login_in_space(&ctx, 9_401, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_docked(&ctx, 9_402, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnFacilitiesUpdated: SetFacilityTaxes on a player-owned structure
    let struct_id = 1_040_000_000_001_i64;
    let corp_id = 98_900_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Ind Corp".into(),
                ticker: "INDC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = SYS_JITA;
        }
        snap.structures.insert(
            struct_id,
            upwell_struct(struct_id, 35835, corp_id, SYS_JITA, "Test Athanor", [10_000.0, 0.0, 0.0]),
        );
    });
    sess_a.corporation_id = Some(corp_id);

    // Negative 1a: Non-owner character (PILOT_B) calling SetFacilityTaxes fails with FacilityPermissionDenied
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnFacilitiesUpdated"));

    // Negative 1b: Invalid negative tax rate fails with InvalidFacilityTaxRate
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(-0.1)],
    )
    .await
    .is_err());

    // Positive: CEO calling SetFacilityTaxes emits OnFacilitiesUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
    )
    .await
    .expect("SetFacilityTaxes");
    let fac_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&fac_notifs, "OnFacilitiesUpdated"),
        &[PyRep::List(vec![PyRep::Integer(struct_id)])]
    );

    // 2. OnIndustryMaterials: blueprintManager.GetBlueprintData
    let bp_item_id = 940_000_050_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bp_item_id,
            make_item(bp_item_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    // Negative 2a: Unknown blueprint type/item ID fails with BlueprintNotFound
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIndustryMaterials"));

    // Negative 2b: Unauthenticated GetBlueprintData does NOT emit OnIndustryMaterials
    let mut unauth = eve_machonet::SessionState::new(995);
    let _ = rpc(
        &ctx,
        &mut unauth,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bp_item_id)],
    )
    .await;
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIndustryMaterials"));

    // Read-only GetBlueprintData on owned blueprint does NOT emit OnIndustryMaterials
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bp_item_id)],
    )
    .await
    .expect("GetBlueprintData");
    let mat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!mat_notifs.iter().any(|(e, _)| e == "OnIndustryMaterials"));

    // 3. OnPrimingNeededForTradeItems: Direct station trade AddItem
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });
    sess_a.station_id = Some(JITA_STATION);
    let trade_res = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(PILOT_B)],
    )
    .await
    .expect("InitiateTrade");
    let trade_id = match trade_res {
        PyRep::KeyVal(kv) => kv.get("tradeContainerID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    assert!(trade_id > 0);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // Negative 3a: Adding non-existent item fails and does not emit OnPrimingNeededForTradeItems
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(940_999_999), PyRep::Integer(1)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
        .iter()
        .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

    // Negative 3b: OfferMoney does NOT emit OnPrimingNeededForTradeItems
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(100.0)],
    )
    .await
    .expect("OfferMoney");
    assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
        .iter()
        .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

    // Positive: Adding a valid hangar item emits OnPrimingNeededForTradeItems([item_id]) to partner PILOT_B
    let trade_item_id = 940_000_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            trade_item_id,
            make_item(trade_item_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 50, false),
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id), PyRep::Integer(10)],
    )
    .await
    .expect("AddItem");
    let partner_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&partner_notifs, "OnPrimingNeededForTradeItems"),
        &[PyRep::List(vec![PyRep::Integer(trade_item_id)])]
    );

    // 4. OnSellRepackageErrors: PlaceMultiSellOrder on an assembled (singleton) ship
    let assembled_ship_id = 940_000_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            assembled_ship_id,
            make_item(assembled_ship_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative 4a: Selling a clean, undamaged assembled ship fails with CannotSellAssembledItem and does NOT emit OnSellRepackageErrors
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("typeID", PyRep::Integer(587)),
        ("itemID", PyRep::Integer(assembled_ship_id)),
        ("price", PyRep::Real(100_000.0)),
        ("quantity", PyRep::Integer(1)),
        ("duration", PyRep::Integer(7)),
    ]);
    let clean_err = rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_entry.clone()]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .unwrap_err();
    assert_eq!(clean_err.code, "CannotSellAssembledItem");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSellRepackageErrors"));

    // Positive 4: Selling a damaged assembled ship fails with CantRepackageDamagedItem and emits OnSellRepackageErrors
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&assembled_ship_id) {
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 25.0);
        }
    });
    let dmg_err = rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_entry]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .unwrap_err();
    assert_eq!(dmg_err.code, "CantRepackageDamagedItem");
    let repack_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let repack_args = find_notif(&repack_notifs, "OnSellRepackageErrors");
    assert_eq!(repack_args.len(), 1);
    assert!(matches!(
        &repack_args[0],
        PyRep::Dict(entries) if entries.iter().any(|(k, v)| k.as_str() == Some("CantRepackageDamagedItem") && *v == PyRep::Integer(1))
    ));

    // Negative 4b: Selling a repackaged item with invalid price (0.0) fails without emitting OnSellRepackageErrors
    let repackaged_id = 940_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            repackaged_id,
            make_item(repackaged_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let bad_price_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("typeID", PyRep::Integer(587)),
        ("itemID", PyRep::Integer(repackaged_id)),
        ("price", PyRep::Real(0.0)),
        ("quantity", PyRep::Integer(1)),
        ("duration", PyRep::Integer(7)),
    ]);
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![bad_price_entry]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSellRepackageErrors"));
}

// parity: notify:OnGodmaEffectInfo, notify:OnGodmaFlushLocationProfile, notify:OnGodmaPrimeItem, notify:OnInvalidateCacheForLocations, notify:OnMultiEvent
#[tokio::test]
async fn notify_batch9_godma_and_location_cache_invalidation() {
    eve_testkit::attest(&[
        "notify:OnGodmaEffectInfo",
        "notify:OnGodmaFlushLocationProfile",
        "notify:OnGodmaPrimeItem",
        "notify:OnInvalidateCacheForLocations",
        "notify:OnMultiEvent",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (940_000_101_i64, 940_000_102_i64);
    let mut sess_a = login_in_space(&ctx, 9_411, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let _sess_b = login_in_space(&ctx, 9_412, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let mod_id = 940_000_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 5-7. OnGodmaPrimeItem, OnGodmaEffectInfo, OnGodmaFlushLocationProfile
    // Negative 5a: SetModuleOnline on a ship owned by PILOT_B fails with ShipOwnershipViolation
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "SetModuleOnline",
        &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnGodmaPrimeItem"));

    // Positive: SetModuleOnline emits OnGodmaPrimeItem and OnGodmaEffectInfo
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "SetModuleOnline",
        &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
    )
    .await
    .expect("SetModuleOnline");
    let online_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let prime_args = find_notif(&online_notifs, "OnGodmaPrimeItem");
    assert_eq!(prime_args.len(), 2);
    assert_eq!(prime_args[0], PyRep::Integer(ship_a));
    assert_eq!(prime_args[1].dict_get("itemID"), Some(&PyRep::Integer(mod_id)));
    assert!(matches!(prime_args[1].dict_get("attributes"), Some(PyRep::Dict(d)) if d.len() > 2));
    let eff_args = find_notif(&online_notifs, "OnGodmaEffectInfo");
    assert_eq!(eff_args.len(), 4);
    assert_eq!(eff_args[0], PyRep::Integer(mod_id));
    assert_eq!(eff_args[1], PyRep::Integer(2873));

    // Negative 7a: TakeModuleOffline on unowned ship fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
    )
    .await
    .is_err());

    // Positive: TakeModuleOffline emits OnGodmaFlushLocationProfile([ship_a])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
    )
    .await
    .expect("TakeModuleOffline");
    let offline_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let flush_args = find_notif(&offline_notifs, "OnGodmaFlushLocationProfile");
    assert_eq!(flush_args.len(), 2);
    assert_eq!(flush_args[0], PyRep::Integer(ship_a));

    // 8. OnInvalidateCacheForLocations: invbroker.SetPassword on a passworded container
    let pw_container_id = 940_000_060_i64;
    let pw_container_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 448 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(17366);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.insert(
            pw_container_id,
            make_item(pw_container_id, pw_container_type, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    sess_a.station_id = Some(JITA_STATION);
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(pw_container_id), PyRep::String("secret123".into())],
    )
    .await
    .expect("SetPassword");
    let cache_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&cache_notifs, "OnInvalidateCacheForLocations"),
        &[PyRep::List(vec![PyRep::Integer(pw_container_id)])]
    );

    // 9. OnMultiEvent: dogmaIM.AddTarget & RemoveTarget
    // Negative 9a: AddTarget with missing targetID fails
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[]).await.is_err());

    // Negative 9b: AddTarget with invalid target ID 0 fails
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)])
        .await
        .is_err());

    // Positive: AddTarget(ship_b) emits OnMultiEvent([("OnTarget", "add", ship_b, ""), 0]) on lock start and lock completion
    let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)])
        .await
        .expect("AddTarget");
    let pre_lock_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&pre_lock_notifs, "OnMultiEvent"),
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("add".into()),
                PyRep::Integer(ship_b),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
    ctx.systems
        .get_or_spawn(SYS_JITA)
        .step_manual(10.0)
        .await
        .expect("step_manual");
    let target_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&target_notifs, "OnMultiEvent"),
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("add".into()),
                PyRep::Integer(ship_b),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
}
