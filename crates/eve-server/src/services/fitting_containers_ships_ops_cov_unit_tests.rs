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
