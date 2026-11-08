//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_saved_multifit`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::fitting_saved_multifit::handle_ships_and_services;
use eve_db::{
    corp_roles, ContractRecord, CorpWalletDivisionRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{MutaplasmidInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn make_ctx_and_session() -> (ServerContext, SessionState, i64, i64, i64) {
    let mut sde = (*shared_sde()).clone();
    // Ensure mutaplasmid 47740 is registered for type 439 (1MN Afterburner I -> 47702)
    sde.dynamic_item_attributes.entry(47_740).or_insert_with(|| MutaplasmidInfo {
        mutaplasmid_type_id: 47_740,
        applicable_type_ids: vec![439],
        resulting_type_id: 47_702,
        attribute_ranges: HashMap::from([(20, [0.9, 1.2]), (6, [0.8, 1.1])]),
    });
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_700_001i64;
    let corp_id = 98_700_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_700_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_f64_isk(500_000_000.0);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Multifit Test Corp".into();
                __corp.ticker = "MFTC".into();
                __corp.ceo_id = 96_700_999;
                __corp.wallet_divisions = vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_f64_isk(500_000_000.0),
                }];
                let __mw = IskCents::from_f64_isk(500_000_000.0);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(ship_id);
    (ctx, sess, char_id, corp_id, station_id)
}

#[tokio::test]
async fn insurance_corp_roles_exact_isk_cost_uninsure_get_contracts_and_prices() {
    let (ctx, mut sess, _char_id, corp_id, station_id) = make_ctx_and_session();
    let ship_id = sess.ship_id.unwrap();
    let corp_ship_id = 96_700_101i64;
    let kw = HashMap::new();

    // Validation errors on InsureShip
    for (args, expected_code) in [
        (vec![PyRep::Integer(ship_id)], "InsureShipInvalidCost"),
        (vec![PyRep::Integer(ship_id), PyRep::Real(0.0)], "InsureShipInvalidCost"),
        (vec![PyRep::Integer(ship_id), PyRep::Real(999_999_999.0)], "InsureShipInvalidCost"),
        (vec![PyRep::Integer(999_999_999), PyRep::Real(0.5)], "ShipNotFound"),
    ] {
        assert_eq!(
            handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "InsureShip", &args, &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // GetInsurancePrices with explicit type ID, then InsureShip using exact ISK cost > 1.0
    let prices_rep = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetInsurancePrices",
        &[PyRep::Integer(587)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let ref_val = match &prices_rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(587))
            .and_then(|(_, v)| v.as_f64())
            .expect("expected Rifter typeID 587 entry in GetInsurancePrices dict"),
        other => panic!("expected Dict from GetInsurancePrices, got {other:?}"),
    };
    let levels = crate::formulas::compute_insurance_levels(ref_val);
    let plat_cost = levels
        .iter()
        .find(|(name, _, _)| *name == "Platinum")
        .map(|(_, cost, _)| *cost)
        .unwrap();
    assert!(plat_cost > 1.0);

    let ins_ok = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(plat_cost)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ins_ok.dict_get("level").and_then(|v| v.as_str()), Some("Platinum"));

    // Re-insuring without voidOld fails; with voidOld = true succeeds
    assert!(handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(0.5)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    assert!(handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(0.5), PyRep::Bool(false), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // Corporate ship insurance: denied without role, allowed with ROLE_ACCOUNTANT
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(corp_ship_id, make_item(corp_ship_id, 587, corp_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(
        handle_ships_and_services(
            &ctx,
            &mut sess,
            None,
            "insuranceSvc",
            "InsureShip",
            &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContracts", &[PyRep::Bool(true)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    sess.corp_role = corp_roles::ROLE_ACCOUNTANT;
    assert!(handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    let corp_contracts = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetContracts",
        &[PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(corp_contracts, PyRep::List(ref v) if v.len() == 1));

    // GetInsurancePrice by ship typeID (587) vs invalid typeID (34)
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(587)], &kw)
        .await
        .unwrap()
        .is_ok());
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(34)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    // UnInsureShip voids personal contract, then GetContractForShip returns None
    let unins = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "UnInsureShip", &[PyRep::Integer(ship_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unins.dict_get("voided"), Some(&PyRep::Bool(true)));
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(ship_id)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
}

#[tokio::test]
async fn create_dynamic_item_stack_splitting_and_contract_info_access() {
    let (ctx, mut sess, char_id, _corp_id, station_id) = make_ctx_and_session();
    let kw = HashMap::new();
    let ab_stack_id = 96_700_201i64;
    let fitted_ab_id = 96_700_202i64;
    let wrong_mod_id = 96_700_203i64;
    let muta_item_id = 96_700_204i64;
    let foreign_dyn_id = 96_700_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ab_stack_id, make_item(ab_stack_id, 439, char_id, station_id, InventoryFlag::Hangar, 2, false));
        snap.items.insert(fitted_ab_id, make_item(fitted_ab_id, 439, char_id, sess.ship_id.unwrap(), InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(wrong_mod_id, make_item(wrong_mod_id, 34, char_id, station_id, InventoryFlag::Hangar, 1, true));
        let mut fd = make_item(foreign_dyn_id, 47_702, 96_700_999, station_id, InventoryFlag::Hangar, 1, true);
        fd.source_mutaplasmid_id = Some(47_740);
        snap.items.insert(foreign_dyn_id, fd);
    });

    // Validation errors on CreateDynamicItem
    for (target_id, expected_code) in [
        (fitted_ab_id, "CannotMutateFittedItem"),
        (wrong_mod_id, "InapplicableMutaplasmid"),
        (ab_stack_id, "MutaplasmidItemNotFound"),
    ] {
        assert_eq!(
            handle_ships_and_services(
                &ctx,
                &mut sess,
                None,
                "dynamicItemService",
                "CreateDynamicItem",
                &[PyRep::Integer(47_740), PyRep::Integer(target_id)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            expected_code
        );
    }

    // Give character 2 mutaplasmids and mutate the 2-unit stack -> splits a new item and decrements both stacks!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(muta_item_id, make_item(muta_item_id, 47_740, char_id, station_id, InventoryFlag::Hangar, 2, false));
    });
    let created = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "dynamicItemSvc",
        "CreateDynamicItem",
        &[PyRep::Integer(muta_item_id), PyRep::Integer(ab_stack_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let new_item_id = created.dict_get("itemID").unwrap().as_i64().unwrap();
    assert_ne!(new_item_id, ab_stack_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ab_stack_id].quantity, 1);
        assert_eq!(snap.items[&muta_item_id].quantity, 1);
        assert_eq!(snap.items[&new_item_id].type_id, 47_702);
    });

    // Foreign dynamic item blocked until placed in a contract
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemService", "GetDynamicItemInfo", &[PyRep::Integer(foreign_dyn_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(501, ContractRecord { contract_id: 501, item_ids: vec![foreign_dyn_id], ..Default::default() });
    });
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemService", "GetDynamicItemInfo", &[PyRep::Integer(foreign_dyn_id)], &kw)
        .await
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn repackage_bpo_reset_assemble_stack_repair_all_and_corp_set_label() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
    let kw = HashMap::new();
    let bpo_id = 96_700_301i64;
    let bpc_id = 96_700_302i64;
    let ship_stack_id = 96_700_303i64;
    let corp_cont_id = 96_700_304i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut bpo = make_item(bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpo.dynamic_attributes = HashMap::from([(1112, 10.0), (1113, 20.0)]);
        snap.items.insert(bpo_id, bpo);
        let mut bpc = make_item(bpc_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        snap.items.insert(bpc_id, bpc);
        snap.items.insert(ship_stack_id, make_item(ship_stack_id, 588, char_id, station_id, InventoryFlag::Hangar, 3, false));
        snap.items.insert(corp_cont_id, make_item(corp_cont_id, 17_366, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));
    });

    // BPC cannot be repackaged; researched BPO requires confirm_reset = true (args[1])
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpc_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotRepackageBlueprintCopy"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpo_id), PyRep::Bool(false)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotRepackageMutatedItem"
    );
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpo_id), PyRep::Bool(true)], &kw)
        .await
        .unwrap()
        .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items[&bpo_id].singleton);
        assert!(snap.items[&bpo_id].dynamic_attributes.is_empty());
    });

    // AssembleShip on stacked corvette (quantity 3) splits 1 assembled singleton ship
    let asm = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "ship",
        "AssembleShip",
        &[PyRep::List(vec![PyRep::Integer(ship_stack_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let new_ship_id = asm.dict_get("itemID").unwrap().as_i64().unwrap();
    assert_ne!(new_ship_id, ship_stack_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_stack_id].quantity, 2);
        assert!(snap.items[&new_ship_id].singleton);
    });

    // SetLabel validation: courier_cargo prefix, >32 chars on ship, corp container rename permission
    assert_eq!(
        handle_ships_and_services(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(new_ship_id), PyRep::String("courier_cargo:secret".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CustomNameNotAllowed"
    );
    assert_eq!(
        handle_ships_and_services(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(new_ship_id), PyRep::String("X".repeat(33))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLabel"
    );
    let bound = BoundObjectEntry {
        bound_id: 1,
        oid_str: "OID-1".into(),
        service: "invbroker".into(),
        object_id: corp_cont_id,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        ref_id: corp_cont_id,
    };
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, Some(&bound), "invbroker", "SetLabel", &[PyRep::String("Corp Box".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, 1);
    });
    assert!(handle_ships_and_services(&ctx, &mut sess, Some(&bound), "invbroker", "SetLabel", &[PyRep::String("Corp Box".into())], &kw)
        .await
        .unwrap()
        .is_ok());
}
