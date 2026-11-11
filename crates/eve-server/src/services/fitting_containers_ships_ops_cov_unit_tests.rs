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
    // ImportExportWithPlanet rejects empty / missing pin args
    let pi_err = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "ImportExportWithPlanet", &[], &kw)
        .await
        .expect("handled")
        .expect_err("empty PI transfer rejected");
    assert_eq!(pi_err.code, "PinNotFound");

    // Seed POCO + colony + launchpad + cargo for real ImportExportWithPlanet transfer + tax check
    let planet_id = 40_000_005i64;
    let poco_id = 97_200_301i64;
    let spaceport_pin_id = 500_101i64;
    let poco_cargo_item_id = 97_200_302i64;
    let secure_cont_id = 97_200_303i64;
    let secure_cont_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340)
        .expect("secure container type in canonical SDE")
        .type_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents(1_000_000_00);
        }
        snap.structures.insert(
            poco_id,
            eve_db::UpwellStructureRecord {
                structure_id: poco_id,
                type_id: 2233,
                owner_corp_id: 1_000_009,
                solar_system_id: 30_000_142,
                name: format!("Orbital:{planet_id}"),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        crate::services::industry_pi::save_orbital_settings(
            snap,
            poco_id,
            &crate::services::industry_pi::OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: 1_000_009,
                corporation_tax_rate: 0.05,
                alliance_tax_rate: 0.05,
                standing_high_tax_rate: 0.02,
                standing_good_tax_rate: 0.05,
                standing_neutral_tax_rate: 0.10,
                allow_alliance: true,
                allow_standings: true,
                minimum_standing: -10.0,
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            eve_db::PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1_775_400_000,
            },
        );
        let net = crate::services::industry_pi::PiColonyNetworkState {
            pins: vec![crate::services::industry_pi::PiPinState {
                pin_id: spaceport_pin_id,
                type_id: 2256,
                latitude: 0.1,
                longitude: 0.2,
                contents: HashMap::from([(2268u32, 20)]),
                ..Default::default()
            }],
            cpu_max: 5_000.0,
            pg_max: 9_000.0,
            ..Default::default()
        };
        crate::services::industry_pi::save_pi_network(snap, char_id, planet_id, &net);
        snap.items.insert(
            poco_cargo_item_id,
            InventoryItemRecord {
                item_id: poco_cargo_item_id,
                type_id: 2268,
                owner_id: char_id,
                location_id: poco_id,
                flag: InventoryFlag::Hangar,
                quantity: 15,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            secure_cont_id,
            InventoryItemRecord {
                item_id: secure_cont_id,
                type_id: secure_cont_type_id,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut st = crate::services::fitting_containers_ships::read_container_ship_state_snap(snap);
        st.set_container_password(secure_cont_id, 1, "open-sesame".to_string());
        crate::services::fitting_containers_ships::save_container_ship_state_snap(
            snap,
            ctx.now_unix(),
            &st,
        );
    });

    let poco_bound = BoundObjectEntry {
        bound_id: 77,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: poco_id,
        oid_str: format!("N=1:{poco_id}"),
        ref_id: 77,
    };
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            Some(&poco_bound),
            "invbroker",
            "ImportExportWithPlanet",
            &[
                PyRep::Integer(spaceport_pin_id),
                PyRep::Dict(vec![(PyRep::Integer(poco_cargo_item_id), PyRep::Integer(5))]),
                PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(10))]),
                PyRep::Real(0.05),
            ],
            &kw,
        )
        .await
        .expect("handled")
        .expect("pi transfer"),
        PyRep::None
    );
    ctx.db.with_snapshot(|s| {
        // Started at 15, imported 5 (-> 10), then exported 10 merged into same POCO stack (-> 20)
        assert_eq!(s.items.get(&poco_cargo_item_id).map(|i| i.quantity), Some(20));
        let total_poco_qty: i64 = s
            .items
            .values()
            .filter(|i| i.location_id == poco_id && i.owner_id == char_id && i.type_id == 2268)
            .map(|i| i64::from(i.quantity))
            .sum();
        assert_eq!(total_poco_qty, 20);
        let net = crate::services::industry_pi::load_pi_network(s, char_id, planet_id, 2);
        let pad = net.pins.iter().find(|p| p.pin_id == spaceport_pin_id).unwrap();
        // Started with 20, imported +5, exported -10 -> 15 left on pad
        assert_eq!(pad.contents.get(&2268).copied(), Some(15));
        assert!(s.characters[&char_id].balance.0 < bal_before);
    });

    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "Reprocess", &[], &kw)
            .await
            .expect("handled")
            .expect("reprocess"),
        PyRep::List(Vec::new())
    );

    // RetrievePassword: empty args error, non-container error, unset slot None, valid slot string + audit log
    let rp_empty = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "RetrievePassword", &[], &kw)
        .await
        .expect("handled")
        .expect_err("empty RetrievePassword rejected");
    assert_eq!(rp_empty.code, "InvalidArguments");
    let rp_ship = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "RetrievePassword",
        &[PyRep::Integer(ship_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("ship does not support container password");
    assert_eq!(rp_ship.code, "ContainerDoesNotSupportPassword");
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "RetrievePassword",
            &[PyRep::Integer(secure_cont_id), PyRep::Integer(2)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("unset config password"),
        PyRep::None
    );
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "RetrievePassword",
            &[PyRep::Integer(secure_cont_id), PyRep::Integer(1)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("ret pw"),
        PyRep::String("open-sesame".into())
    );
    ctx.db.with_snapshot(|s| {
        let st = crate::services::fitting_containers_ships::read_container_ship_state_snap(s);
        assert!(st.unlocked_containers.contains(&(secure_cont_id, char_id)));
        assert!(
            st.alsc_logs
                .get(&secure_cont_id)
                .is_some_and(|logs| logs.iter().any(|l| l.action == "ALSCActionRetrievePassword"))
        );
    });

    // ListIncludingContainers & ListStations (including OF-33 empty-station CRowset check)
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
    assert!(matches!(stations, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    let mut empty_sess = SessionState::new(97_199);
    empty_sess.user_id = Some(999);
    empty_sess.character_id = Some(90_999_999);
    empty_sess.corporation_id = Some(1_000_009);
    let empty_stations = handle_ext_tail(&ctx, &mut empty_sess, None, "invbroker", "ListStations", &[], &kw)
        .await
        .expect("handled")
        .expect("empty stations");
    assert!(matches!(empty_stations, PyRep::Rowset { ref rows, .. } if rows.is_empty()));

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

#[tokio::test]
async fn fit_fitting_guards_saved_fitting_3tuple_2tuple_and_slot_overflow() {
    let char_id = 9_610_001i64;
    let ship_id = 9_610_011i64;
    let station_id = 60_003_760i64;
    let (ctx, mut sess) = test_ctx_and_session(char_id, ship_id);
    let kw = HashMap::new();

    let hi_mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.effect_ids.contains(&12) && t.volume <= 10.0)
        .expect("high-slot module in canonical SDE")
        .type_id;
    let drone_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 18 && t.volume <= 5.0)
        .expect("light drone in canonical SDE")
        .type_id;
    let charge_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 8 && t.volume <= 0.1)
        .expect("charge in canonical SDE")
        .type_id;

    // 1. NotDocked check when station_id & structure_id are None
    sess.station_id = None;
    sess.structure_id = None;
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(ship_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDocked"
    );

    sess.station_id = Some(station_id);
    sess.ship_id = None;
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "invbroker", "FitFitting", &[PyRep::Integer(0)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidFittingTarget"
    );
    sess.ship_id = Some(ship_id);

    let mod_item_id = 9_610_101i64;
    let drone_item_id = 9_610_102i64;
    let charge_item_id = 9_610_103i64;
    let stranger_fit_id = 9_610_201i64;
    let owner_fit_id = 9_610_202i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_item_id,
            InventoryItemRecord {
                item_id: mod_item_id,
                type_id: hi_mod_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_item_id,
            InventoryItemRecord {
                item_id: drone_item_id,
                type_id: drone_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_item_id,
            InventoryItemRecord {
                item_id: charge_item_id,
                type_id: charge_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.saved_fittings.insert(
            stranger_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: stranger_fit_id,
                owner_id: char_id + 999,
                name: "StrangerFit".into(),
                ship_type_id: 587,
                description: String::new(),
                items: vec![(hi_mod_tid, InventoryFlag::HiSlot0, 1)],
            },
        );
        snap.saved_fittings.insert(
            owner_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: owner_fit_id,
                owner_id: char_id,
                name: "OwnerFit".into(),
                ship_type_id: 587,
                description: String::new(),
                items: vec![
                    (hi_mod_tid, InventoryFlag::HiSlot0, 1),
                    (charge_tid, InventoryFlag::Cargo, 25), // 10 present -> 15 missing
                ],
            },
        );
    });

    // FittingNotFound & stranger AccessDenied
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "FitFitting",
            &[PyRep::Integer(ship_id), PyRep::Integer(9_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FittingNotFound"
    );
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "FitFitting",
            &[PyRep::Integer(ship_id), PyRep::Integer(stranger_fit_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Valid owner saved fitting fits available module/charges and reports exact missing sorted tuple.
    let missing_rep = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[PyRep::Integer(ship_id), PyRep::Integer(owner_fit_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("saved fitting apply");
    assert_eq!(
        missing_rep,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(charge_tid as i64),
            PyRep::Integer(15),
        ])])
    );
    assert_eq!(ctx.db.get_item(mod_item_id).unwrap().location_id, ship_id);
    assert_eq!(ctx.db.get_item(mod_item_id).unwrap().flag, InventoryFlag::HiSlot0);

    // fitData 3-tuple invalid flag & invalid quantity errors.
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "FitFitting",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(hi_mod_tid as i64),
                    PyRep::Integer(999),
                    PyRep::Integer(1),
                ])]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFittingFlag"
    );
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "FitFitting",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(hi_mod_tid as i64),
                    PyRep::Integer(27),
                    PyRep::Integer(0),
                ])]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );

    // fitData 2-tuple places drone -> DroneBay (if ship has drone bay, or missing/overflow) and rejects slot overflow!
    let overflow_err = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "FitFitting",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(drone_tid as i64), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(hi_mod_tid as i64), PyRep::Integer(10)]),
            ]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(overflow_err.code, "UserError");
    assert_eq!(overflow_err.message, "SlotIndexExceedsHullSlots");
}

#[tokio::test]
async fn import_export_planet_multi_stack_tax_skill_corp_payout_and_capacity_errors() {
    let char_id = 90_000_001i64;
    let ship_id = 9_620_011i64;
    let (ctx, mut sess) = test_ctx_and_session(char_id, ship_id);
    let kw = HashMap::new();
    let planet_id = 40_000_005i64;
    let poco_id = 97_620_301i64;
    let spaceport_pin_id = 500_201i64;
    let bad_pin_id = 500_202i64;
    let stack1_id = 97_620_302i64;
    let stack2_id = 97_620_303i64;
    let poco_owner_corp_id = 98_620_009i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents(10_000_000_00);
            ch.corporation_id = 98_620_099;
        }
        snap.skills.entry(char_id).or_default().insert(
            33467,
            eve_db::TrainedSkillRecord {
                type_id: 33467,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = poco_owner_corp_id;
        corp.name = "PocoTaxCorp".into();
        corp.ticker = "PTC".into();
        corp.ensure_wallet_divisions();
        snap.corporations.insert(poco_owner_corp_id, corp);
        snap.structures.insert(
            poco_id,
            eve_db::UpwellStructureRecord {
                structure_id: poco_id,
                type_id: 2233,
                owner_corp_id: poco_owner_corp_id,
                solar_system_id: 30_000_142,
                name: format!("Orbital:{planet_id}"),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        crate::services::industry_pi::save_orbital_settings(
            snap,
            poco_id,
            &crate::services::industry_pi::OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: poco_owner_corp_id,
                corporation_tax_rate: 0.10,
                alliance_tax_rate: 0.10,
                standing_high_tax_rate: 0.10,
                standing_good_tax_rate: 0.10,
                standing_neutral_tax_rate: 0.10,
                allow_alliance: true,
                allow_standings: true,
                minimum_standing: -10.0,
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            eve_db::PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1_775_400_000,
            },
        );
        let net = crate::services::industry_pi::PiColonyNetworkState {
            pins: vec![
                crate::services::industry_pi::PiPinState {
                    pin_id: spaceport_pin_id,
                    type_id: 2256, // Spaceport / Launchpad
                    latitude: 0.1,
                    longitude: 0.2,
                    contents: HashMap::from([(2268u32, 5)]),
                    ..Default::default()
                },
                crate::services::industry_pi::PiPinState {
                    pin_id: bad_pin_id,
                    type_id: 2254, // Command Center (not a launchpad)
                    latitude: 0.3,
                    longitude: 0.4,
                    contents: HashMap::new(),
                    ..Default::default()
                },
            ],
            cpu_max: 5_000.0,
            pg_max: 9_000.0,
            ..Default::default()
        };
        crate::services::industry_pi::save_pi_network(snap, char_id, planet_id, &net);

        // Two separate stacks of commodity 2268 inside POCO hangar for multi-stack ByTypeId import
        snap.items.insert(
            stack1_id,
            InventoryItemRecord {
                item_id: stack1_id,
                type_id: 2268,
                owner_id: char_id,
                location_id: poco_id,
                flag: InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack2_id,
            InventoryItemRecord {
                item_id: stack2_id,
                type_id: 2268,
                owner_id: char_id,
                location_id: poco_id,
                flag: InventoryFlag::Hangar,
                quantity: 6,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let poco_bound = BoundObjectEntry {
        bound_id: 88,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: poco_id,
        oid_str: format!("N=1:{poco_id}"),
        ref_id: 88,
    };

    // Error guards: InvalidPinTypeForLaunch, InsufficientCommodityQuantity, ColonyNotInCurrentSystem.
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            Some(&poco_bound),
            "invbroker",
            "ImportExportWithPlanet",
            &[PyRep::Integer(bad_pin_id), PyRep::Dict(vec![]), PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(1))])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPinTypeForLaunch"
    );

    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            Some(&poco_bound),
            "invbroker",
            "ImportExportWithPlanet",
            &[PyRep::Integer(spaceport_pin_id), PyRep::Dict(vec![]), PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(99))])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCommoditiesForRouting"
    );

    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            Some(&poco_bound),
            "invbroker",
            "ImportExportWithPlanet",
            &[PyRep::Integer(spaceport_pin_id), PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(99))]), PyRep::Dict(vec![])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCommodityQuantity"
    );

    // Multi-stack ByTypeId import of 6 units consumes stack1 (4 -> removed) and stack2 (6 -> 4) + credits POCO owner corp!
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&poco_owner_corp_id].division_balance(1).0);
    handle_ext_tail(
        &ctx,
        &mut sess,
        Some(&poco_bound),
        "invbroker",
        "ImportExportWithPlanet",
        &[
            PyRep::Integer(spaceport_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(6))]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(2))]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("multi-stack ByTypeId ImportExportWithPlanet");

    assert!(ctx.db.get_item(stack1_id).is_none());
    // Imported 6 (stack1 4 + 2 from stack2 -> 4 left), then exported 2 back into stack2 -> 6!
    assert_eq!(ctx.db.get_item(stack2_id).unwrap().quantity, 6);
    let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&poco_owner_corp_id].division_balance(1).0);
    assert!(corp_bal_after > corp_bal_before, "POCO owner corp division 1 should receive tax share");

    // Different solar system fails ColonyNotInCurrentSystem.
    sess.solar_system_id = Some(30_002_187);
    assert_eq!(
        handle_ext_tail(
            &ctx,
            &mut sess,
            Some(&poco_bound),
            "invbroker",
            "ImportExportWithPlanet",
            &[PyRep::Integer(spaceport_pin_id), PyRep::Dict(vec![]), PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(1))])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ColonyNotInCurrentSystem"
    );
}

#[tokio::test]
async fn get_asset_worth_plex_list_stations_filters_and_decode_item_variants() {
    let char_id = 9_630_001i64;
    let ship_id = 9_630_011i64;
    let corp_id = 98_630_001i64;
    let station_a = 60_003_760i64;
    let station_b = 60_008_494i64;
    let (ctx, mut sess) = test_ctx_and_session(char_id, ship_id);
    sess.corporation_id = Some(corp_id);
    let kw = HashMap::new();

    let bp_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 9)
        .expect("blueprint type in SDE")
        .type_id;
    let plex_tid = 44_992u32;

    let plex_id = 9_630_101i64;
    let bp_item_id = 9_630_102i64;
    let normal_item_id = 9_630_103i64;
    let corp_item_id = 9_630_104i64;
    let decoded_item_id = 9_630_105i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_id,
            InventoryItemRecord {
                item_id: plex_id,
                type_id: plex_tid,
                owner_id: char_id,
                location_id: station_a,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            bp_item_id,
            InventoryItemRecord {
                item_id: bp_item_id,
                type_id: bp_tid,
                owner_id: char_id,
                location_id: station_a,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            normal_item_id,
            InventoryItemRecord {
                item_id: normal_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_b,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_item_id,
            InventoryItemRecord {
                item_id: corp_item_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: station_b,
                flag: InventoryFlag::CorpHangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(9u16, 777.0);
        snap.items.insert(
            decoded_item_id,
            InventoryItemRecord {
                item_id: decoded_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_a,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                source_mutaplasmid_id: Some(47_800),
                dynamic_attributes: dyn_attrs,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names.insert(decoded_item_id, "Mutated Test Rifter".into());
    });

    // GetAssetWorth splits PLEX (44992) into second tuple element.
    let expected_plex_val = ctx.sde.types.get(&plex_tid).map(|t| t.base_price).unwrap_or(0.0) * 10.0;
    let worth = handle_ext_tail(&ctx, &mut sess, None, "invbroker", "GetAssetWorth", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(w_parts) = worth else {
        panic!("expected tuple");
    };
    assert!((w_parts[1].as_f64().unwrap() - expected_plex_val).abs() < 1e-3);

    // ListStations filters by blueprint_only and is_corp.
    let bp_stations = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ListStations",
        &[PyRep::Bool(true), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: bp_rows, .. } = bp_stations else {
        panic!("expected rowset");
    };
    assert_eq!(bp_rows.len(), 1);
    assert_eq!(bp_rows[0][0], PyRep::Integer(station_a));

    let corp_stations = handle_ext_tail(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "ListStations",
        &[PyRep::Bool(false), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: corp_rows, .. } = corp_stations else {
        panic!("expected rowset");
    };
    assert_eq!(corp_rows.len(), 1);
    assert_eq!(corp_rows[0][0], PyRep::Integer(station_b));

    // encodedItems.DecodeItem validates empty args & handles Tuple, "id:tid" String, and kwargs + dynamicAttrs.
    assert_eq!(
        handle_ext_tail(&ctx, &mut sess, None, "encodedItems", "DecodeItem", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let mut kw_dec = HashMap::new();
    kw_dec.insert("itemID".into(), PyRep::Integer(decoded_item_id));
    kw_dec.insert("typeID".into(), PyRep::Integer(587));
    for (rep_args, rep_kw) in [
        (vec![PyRep::Tuple(vec![PyRep::Integer(decoded_item_id), PyRep::Integer(587)])], kw.clone()),
        (vec![PyRep::String(format!("{decoded_item_id}:587"))], kw.clone()),
        (vec![], kw_dec),
    ] {
        let dec = handle_ext_tail(&ctx, &mut sess, None, "encodedItems", "DecodeItem", &rep_args, &rep_kw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dec.dict_get("customName"), Some(&PyRep::String("Mutated Test Rifter".into())));
        assert_eq!(dec.dict_get("isBlueprintCopy"), Some(&PyRep::Bool(true)));
        assert_eq!(dec.dict_get("sourceMutaplasmidID"), Some(&PyRep::Integer(47_800)));
    }
}

