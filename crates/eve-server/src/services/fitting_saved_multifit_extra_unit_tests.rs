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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insurance_corp_roles_exact_isk_cost_uninsure_get_contracts_and_prices() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
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

    let _ = ctx.db.acquire_online_lock(ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id)), 1, Some(char_id), false, ctx.now_unix());
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
    let expected_bal = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().balance.to_wire_f64());
    let ins_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(ins_notifs.iter().any(|n| n.event == "OnShipInsured"));
    assert!(
        ins_notifs.iter().any(|n| {
            n.event == "OnAccountChange"
                && n.args
                    == vec![
                        PyRep::String("cash".into()),
                        PyRep::Integer(char_id),
                        PyRep::Real(expected_bal),
                    ]
        }),
        "InsureShip must emit OnAccountChange (#772), got {ins_notifs:?}"
    );

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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(
                sess.character_id.unwrap(),
                corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
            );
        }
    });
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repackage_assemble_insurance_and_dynamic_item_error_branches() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
    let active_ship = sess.ship_id.unwrap();
    let kw = HashMap::new();

    // 1. InsuranceSvc error guards: NoActiveShip, OwnershipViolation, ShipNotFound
    let other_ship = 96_800_001_i64;
    let unins_ship = 96_800_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(other_ship, make_item(other_ship, 587, 99_888_777, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(unins_ship, make_item(unins_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(other_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(other_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(99_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "ShipNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(unins_ship)], &kw)
            .await.unwrap().unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "UnInsureShip", &[PyRep::Integer(other_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    sess.ship_id = None;
    for meth in ["GetInsurancePrice", "GetContractForShip", "UnInsureShip"] {
        let res = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", meth, &[], &kw).await.unwrap();
        // rpc-lint: allow error code check
        assert_eq!(res.unwrap_err().code, "NoActiveShip");
    }
    sess.ship_id = Some(active_ship);

    // 2. dynamicItemService.GetDynamicItemInfo missing item
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemService", "GetDynamicItemInfo", &[PyRep::Integer(99_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );

    // 3. RepackageItems error guards + dict/tuple inputs
    let spare_ship = 96_800_010_i64;
    let fitted_mod = 96_800_011_i64;
    let bpc_item = 96_800_012_i64;
    let locked_item = 96_800_013_i64;
    let damaged_item = 96_800_014_i64;
    let container_id = 96_800_015_i64;
    let in_cont_item = 96_800_016_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(spare_ship, make_item(spare_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(fitted_mod, make_item(fitted_mod, 439, char_id, spare_ship, InventoryFlag::MedSlot0, 1, true));
        let mut bpc = make_item(bpc_item, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        snap.items.insert(bpc_item, bpc);
        snap.items.insert(locked_item, make_item(locked_item, 587, char_id, station_id, InventoryFlag::Locked, 1, true));
        let mut dmg = make_item(damaged_item, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        dmg.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(damaged_item, dmg);
        snap.items.insert(container_id, make_item(container_id, 3293, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(in_cont_item, make_item(in_cont_item, 587, char_id, container_id, InventoryFlag::Hangar, 1, true));
    });

    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![])], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(active_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "CannotRepackageActiveShip"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(other_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipOrLocationViolation"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(fitted_mod)], &kw)
            .await.unwrap().unwrap_err().code,
        "CanNotUnassembleInThisItemLoc"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(in_cont_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemInsideContainer"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(locked_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(spare_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "CannotRepackageNonEmptyItem"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpc_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "CannotRepackageBlueprintCopy"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(damaged_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "CantRepackageDamagedItem"
    );

    // Undocked RepackageItems & AssembleShip -> NotDocked
    sess.station_id = None;
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(bpc_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotDocked"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(bpc_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotDocked"
    );
    sess.station_id = Some(station_id);

    // 4. AssembleShip error guards: ItemNotFound, OwnershipOrLocationViolation, CanNotUnassembleInThisItemLoc, ItemLocked (corp flag)
    let corp_bad_flag = 96_800_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(corp_bad_flag, make_item(corp_bad_flag, 587, corp_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(99_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(other_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipOrLocationViolation"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(fitted_mod)], &kw)
            .await.unwrap().unwrap_err().code,
        "CanNotUnassembleInThisItemLoc"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(corp_bad_flag)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemLocked"
    );

    // Successful repackage via Dict arg with Tuple entry after removing fitted mod from spare_ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&fitted_mod);
    });
    let dict_arg = PyRep::Dict(vec![(PyRep::Integer(station_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(spare_ship)])]))]);
    let rep_ok = handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[dict_arg], &kw)
        .await
        .unwrap()
        .expect("dict repackage ok");
    assert_eq!(rep_ok.dict_get("repackagedCount"), Some(&PyRep::Integer(1)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insurance_prices_dynamic_item_validation_repackage_damage_and_asset_safety_wrap() {
    let (ctx, _, _, _, _) = make_ctx_and_session();
    let char_id = 90_000_001_i64;
    let corp_id = 98_650_001_i64;
    let station_id = 60_003_760_i64;
    let other_char = 90_000_002_i64;
    let ship_id = 96_900_001_i64;
    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(station_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. GetInsurancePrices: StationServiceUnavailable at station without insurance service 21, plus active_ship fallback & SDE fallback
    if let Some(&no_ins_st) = ctx
        .sde
        .stations
        .keys()
        .find(|&&sid| !crate::services::station_structure::npc_station_offers_service(&ctx.sde, sid, 21))
    {
        sess.station_id = Some(no_ins_st);
        assert_eq!(
            handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrices", &[], &kw)
                .await.unwrap().unwrap_err().code,
            "StationServiceUnavailable"
        );
        sess.station_id = Some(station_id);
    }
    // Single-integer arg form
    let p_single = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrices", &[PyRep::Integer(587)], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(p_single, PyRep::List(_) | PyRep::Dict(_)));
    // Remove all char_id items so empty args falls back to active_ship_id, then clear active_ship_id for SDE top-50 fallback
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| it.owner_id != char_id);
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    let p_active = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrices", &[], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(p_active, PyRep::List(_) | PyRep::Dict(_)));
    sess.ship_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ship_id);
    });
    let p_sde = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetInsurancePrices", &[], &kw)
        .await.unwrap().unwrap();
    assert!(matches!(p_sde, PyRep::Dict(_)));

    // 2. CreateDynamicItem error guards: InvalidMutaplasmid, TargetItemNotFound, OwnershipViolation, CannotMutateFittedItem, TargetItemLockedOrUnreachable
    sess.ship_id = Some(ship_id);
    let muta_tid = ctx.sde.dynamic_item_attributes.keys().next().copied().unwrap_or(47740);
    let tgt_item = 96_900_010_i64;
    let fitted_item = 96_900_011_i64;
    let other_item = 96_900_012_i64;
    let remote_item = 96_900_013_i64;
    let dam_ship = 96_900_014_i64;
    let wrap_item = 96_900_015_i64;
    let corp_ship = 96_900_016_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "FitTestCorp".into(); __corp.ticker = "FTC".into(); __corp.ceo_id = char_id; __corp.ensure_wallet_divisions(); __corp });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(tgt_item, make_item(tgt_item, 12058, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(fitted_item, make_item(fitted_item, 12058, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(other_item, make_item(other_item, 12058, other_char, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(remote_item, make_item(remote_item, 12058, char_id, station_id + 1, InventoryFlag::Hangar, 1, false));
        snap.items.insert(dam_ship, make_item(dam_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(wrap_item, make_item(wrap_item, 587, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(corp_ship, make_item(corp_ship, 587, corp_id, station_id, InventoryFlag::CorpSAG2, 1, false));
        // Put wrap_item inside an unpaid Asset Safety wrap
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.insert(888_777, crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
            wrap_id: 888_777,
            owner_id: char_id,
            is_corp: false,
            source_structure_id: station_id + 100,
            source_structure_name: "Test".into(),
            solar_system_id: 30_000_142,
            created_unix: 100,
            deliverable_after_unix: 200,
            delivered_to_id: None,
            item_ids: vec![wrap_item],
            estimated_value_isk: 1000.0,
            nearest_npc_station_id: Some(station_id),
            auto_delivered: false,
            retrieval_fee_paid: false,
        });
        let _ = crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidMutaplasmid"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(muta_tid as i64)], &kw)
            .await.unwrap().unwrap_err().code,
        "TargetItemNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(999_999), PyRep::Integer(tgt_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidMutaplasmid"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(muta_tid as i64), PyRep::Integer(99_999_999)], &kw)
            .await.unwrap().unwrap_err().code,
        "TargetItemNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(muta_tid as i64), PyRep::Integer(other_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(muta_tid as i64), PyRep::Integer(fitted_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "CannotMutateFittedItem"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(muta_tid as i64), PyRep::Integer(remote_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "TargetItemLockedOrUnreachable"
    );

    // 3. RepackageItems: empty list ItemNotFound, active ship CannotRepackageActiveShip, CantRepackageDamagedItem (heat), single int arg
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![])], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(ship_id)], &kw)
            .await.unwrap().unwrap_err().code,
        "CannotRepackageActiveShip"
    );
    ctx.ship_fittings
        .write()
        .unwrap()
        .entry(dam_ship)
        .or_insert_with(|| eve_dogma::ShipFittingEngine::from_sde(dam_ship, 587, &ctx.sde))
        .rack_heat = [10.0, 0.0, 0.0];
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::Integer(dam_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "CantRepackageDamagedItem"
    );

    // 4. AssembleShip: Asset Safety wrap ItemLocked + remote location + corp hangar success (CEO has take access)
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(wrap_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(remote_item)], &kw)
            .await.unwrap().unwrap_err().code,
        "OwnershipOrLocationViolation"
    );
    let _ = handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(corp_ship)], &kw)
        .await.unwrap().unwrap();

    // 5. CreateDynamicItem in-place (quantity == 1) + GetDynamicItemInfo empty ItemNotFound
    let ab_item = 96_900_020_i64;
    let muta_item = 96_900_021_i64;
    let courier_wrap = 96_900_022_i64;
    let wrapped_ship = 96_900_023_i64;
    let cargo_ship = 96_900_024_i64;
    let courier_label_item = 96_900_025_i64;
    let space_ship = 96_900_026_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(ab_item, make_item(ab_item, 439, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(muta_item, make_item(muta_item, 47_740, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(courier_wrap, make_item(courier_wrap, 3468, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(wrapped_ship, make_item(wrapped_ship, 587, other_char, courier_wrap, InventoryFlag::Hangar, 1, true));
        snap.contracts.insert(909_001, ContractRecord {
            contract_id: 909_001,
            issuer_id: other_char,
            acceptor_id: Some(char_id),
            contract_type: "Courier".to_string(),
            status: "InProgress".to_string(),
            item_ids: vec![wrapped_ship],
            ..Default::default()
        });
        snap.items.insert(cargo_ship, make_item(cargo_ship, 587, char_id, station_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(courier_label_item, make_item(courier_label_item, 439, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.item_custom_names.insert(courier_label_item, "courier_cargo:job1".to_string());
        snap.items.insert(space_ship, make_item(space_ship, 587, char_id, 30_000_142, InventoryFlag::Hangar, 1, true));
    });
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemSvc", "CreateDynamicItem", &[PyRep::Integer(47_740), PyRep::Integer(ab_item)], &kw)
        .await.unwrap().is_ok());
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "dynamicItemService", "GetDynamicItemInfo", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );

    // 6. RepackageItems breaking a courier plastic wrap (3468) fails contract & strips cosmetics
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(courier_wrap)])], &kw)
        .await.unwrap().is_ok());

    // 7. AssembleShip: None + empty -> ItemNotFound, Cargo flag -> ItemLocked, persisted_dock mismatch -> NotDocked
    let saved_ship_id = sess.ship_id;
    sess.ship_id = None;
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    sess.ship_id = saved_ship_id;
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(cargo_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    sess.station_id = Some(station_id + 55);
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(corp_ship)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotDocked"
    );
    sess.station_id = Some(station_id);

    // 8. repairSvc dispatch arms
    for method in ["RepairItems", "RepairItemsInStation", "RepairItemsInStructure"] {
        let _ = handle_ships_and_services(&ctx, &mut sess, None, "repairSvc", method, &[PyRep::List(vec![PyRep::Integer(ship_id)])], &kw)
            .await.unwrap();
    }
    let _ = handle_ships_and_services(&ctx, &mut sess, None, "repairSvc", "RepairAll", &[], &kw)
        .await.unwrap();

    // 9. invbroker SetLabel: existing courier_cargo custom name -> CustomNameNotAllowed, and space ship rename -> OnSlimItemChange
    assert_eq!(
        handle_ships_and_services(&ctx, &mut sess, None, "invbroker", "SetLabel", &[PyRep::Integer(courier_label_item), PyRep::String("NewName".into())], &kw)
            .await.unwrap().unwrap_err().code,
        "CustomNameNotAllowed"
    );
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "invbroker", "SetLabel", &[PyRep::Integer(space_ship), PyRep::String("SpaceRifter".into())], &kw)
        .await.unwrap().is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ships_services_get_contracts_variants_assemble_version_and_set_label_roles() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
    let ship_id = sess.ship_id.unwrap();
    let corp_ship_id = 96_950_001_i64;
    let stack_ship = 96_950_002_i64;
    let single_ship = 96_950_003_i64;
    let dyn_ab = 96_950_004_i64;
    let muta_mod = 96_950_005_i64;
    let repack_ship = 96_950_006_i64;
    let bound_ship = 96_950_007_i64;
    let corp_label_ship = 96_950_008_i64;
    let other_corp_id = 98_700_999_i64;
    let other_corp_ship = 96_950_009_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR);
        }
        snap.corporations.insert(other_corp_id, {
            let mut c = CorporationRecord::default();
            c.corporation_id = other_corp_id;
            c.name = "Other Corp".into();
            c.ticker = "OTHC".into();
            c.ceo_id = char_id; // character is NOT a member of other_corp_id, only CEO on paper
            c.ensure_wallet_divisions();
            c
        });

        let mut stack_rec = make_item(stack_ship, 587, char_id, station_id, InventoryFlag::Hangar, 3, false);
        stack_rec.version = 7;
        snap.items.insert(stack_ship, stack_rec);

        let mut single_rec = make_item(single_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
        single_rec.version = 7;
        snap.items.insert(single_ship, single_rec);

        snap.items.insert(corp_ship_id, make_item(corp_ship_id, 587, corp_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(dyn_ab, make_item(dyn_ab, 439, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(muta_mod, make_item(muta_mod, 47_740, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(repack_ship, make_item(repack_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(bound_ship, make_item(bound_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(corp_label_ship, make_item(corp_label_ship, 587, corp_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(other_corp_ship, make_item(other_corp_ship, 587, other_corp_id, station_id, InventoryFlag::Hangar, 1, true));
    });

    // 1. insuranceSvc.GetContracts across all arg forms (None, Bool(false/true), Integer(0/1), empty)
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)], &kw).await.unwrap().is_ok());
    assert!(handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "InsureShip", &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)], &kw).await.unwrap().is_ok());

    for personal_args in [vec![], vec![PyRep::None], vec![PyRep::Bool(false)], vec![PyRep::Integer(0)]] {
        let rep = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContracts", &personal_args, &kw)
            .await
            .unwrap()
            .unwrap();
        let PyRep::List(rows) = rep else { panic!("expected List from GetContracts"); };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
    }
    for corp_args in [vec![PyRep::Bool(true)], vec![PyRep::Integer(1)]] {
        let rep = handle_ships_and_services(&ctx, &mut sess, None, "insuranceSvc", "GetContracts", &corp_args, &kw)
            .await
            .unwrap()
            .unwrap();
        let PyRep::List(rows) = rep else { panic!("expected List from GetContracts"); };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dict_get("shipID").and_then(|v| v.as_i64()), Some(corp_ship_id));
    }

    // 2. CreateDynamicItem & RepackageItems explicit non-default return verification
    let dyn_res = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "dynamicItemService",
        "CreateDynamicItem",
        &[PyRep::Integer(47_740), PyRep::Integer(dyn_ab)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dyn_res.dict_get("resultingTypeID").and_then(|v| v.as_i64()), Some(47_702));

    let rep_res = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::Integer(repack_ship)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rep_res.dict_get("itemID").and_then(|v| v.as_i64()), Some(repack_ship));
    assert_eq!(rep_res.dict_get("singleton"), Some(&PyRep::Bool(false)));
    assert_eq!(rep_res.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));

    // 3. AssembleShip: stack (qty 3 > 1, !singleton) splits & increments orig version 7 -> 8; single (qty 1, !singleton) increments version 7 -> 8
    let split_asm = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(stack_ship)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let split_new_id = split_asm.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(split_new_id, stack_ship);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stack_ship].quantity, 2);
        assert_eq!(snap.items[&stack_ship].version, 8);
        assert!(snap.items[&split_new_id].singleton);
    });

    let single_asm = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "ship",
        "AssembleShip",
        &[PyRep::Integer(single_ship)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(single_asm.dict_get("itemID").and_then(|v| v.as_i64()), Some(single_ship));
    assert_eq!(single_asm.dict_get("version").and_then(|v| v.as_i64()), Some(8));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items[&single_ship].singleton);
        assert_eq!(snap.items[&single_ship].quantity, 1);
        assert_eq!(snap.items[&single_ship].version, 8);
    });

    // 4. invbroker.SetLabel: 1-arg bound_entry form (args.len() < 2) + corp role authorization edge matrix
    let bound = BoundObjectEntry {
        bound_id: 91,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: bound_ship,
        oid_str: format!("invbroker:{bound_ship}"),
        ref_id: 1,
    };
    let lbl_bound = handle_ships_and_services(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "SetLabel",
        &[PyRep::String("BoundShipName".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(lbl_bound.dict_get("itemID").and_then(|v| v.as_i64()), Some(bound_ship));
    assert_eq!(lbl_bound.dict_get("name").and_then(|v| v.as_str()), Some("BoundShipName"));

    // Cross-corp rename denied even if other_corp has char_id as ceo_id (kills L1086/1087/1088)
    assert_eq!(
        handle_ships_and_services(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(other_corp_ship), PyRep::String("Hack".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );

    // Same-corp rename denied with roles == 0 or unrelated role == 2 (not Director=1 or EquipmentConfig=2199023255552)
    for denied_roles in [0_u64, 2] {
        ctx.db.with_mut_snapshot(|snap| {
            let corp = snap.corporations.get_mut(&corp_id).unwrap();
            corp.ceo_id = 96_700_999;
            corp.member_roles.insert(char_id, denied_roles);
        });
        assert_eq!(
            handle_ships_and_services(
                &ctx,
                &mut sess,
                None,
                "invbroker",
                "SetLabel",
                &[PyRep::Integer(corp_label_ship), PyRep::String("DeniedCorp".into())],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "OwnershipViolation"
        );
    }
    // Allowed with ONLY Director (1), ONLY EquipmentConfig (2_199_023_255_552), or CEO with roles=0
    for (ceo_id, roles, new_name) in [
        (96_700_999_i64, 1_u64, "DirCorpShip"),
        (96_700_999, 2_199_023_255_552_u64, "EquipCorpShip"),
        (char_id, 0_u64, "CeoCorpShip"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            let corp = snap.corporations.get_mut(&corp_id).unwrap();
            corp.ceo_id = ceo_id;
            corp.member_roles.insert(char_id, roles);
        });
        let ok = handle_ships_and_services(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(corp_label_ship), PyRep::String(new_name.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(ok.dict_get("name").and_then(|v| v.as_str()), Some(new_name));
    }
}
