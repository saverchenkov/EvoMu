//! Focused unit tests for uncovered branches in `fitting_saved_multifit`
//! (`insuranceSvc`, `dynamicItemService`, `repackagingSvc`, `ship`, `repairSvc`, `invbroker`).

use super::*;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn setup_insurance_fixture() -> (ServerContext, i64, i64, i64, i64, i64, i64) {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_id = 96_610_001i64;
    let other_char_id = 96_610_002i64;
    let corp_id = 98_610_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_610_100i64;
    let corp_ship_id = 96_610_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9001;
        ch.name = "InsurePilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_f64_isk(500_000_000.0);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch.clone());

        let mut other = ch;
        other.character_id = other_char_id;
        other.user_id = 9002;
        other.name = "OtherPilot".into();
        snap.characters.insert(other_char_id, other);

        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Insure Corp".into(),
            ticker: "INSC".into(),
            ceo_id: char_id,
            master_wallet: IskCents::from_f64_isk(500_000_000.0),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

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
            corp_ship_id,
            InventoryItemRecord {
                item_id: corp_ship_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    (ctx, char_id, other_char_id, corp_id, station_id, ship_id, corp_ship_id)
}

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, ship_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(9001),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        solar_system_id: Some(30_000_142),
        ship_id,
        ..Default::default()
    }
}

#[tokio::test]
async fn insurance_svc_personal_corp_void_old_and_error_branches() {
    let (ctx, char_id, other_char_id, corp_id, station_id, ship_id, corp_ship_id) =
        setup_insurance_fixture();
    let empty_kw = HashMap::new();

    // Unauthenticated rejected
    let mut unauth = SessionState::default();
    assert!(handle_ships_and_services(&ctx, &mut unauth, None, "insuranceSvc", "InsureShip", &[], &empty_kw)
        .await
        .expect("handled")
        .is_err());

    // InsureShip at an Upwell structure (stationID = 1_040_000_000_001) rejected as StationServiceUnavailable
    let mut struct_sess = make_session(char_id, corp_id, Some(1_040_000_000_001), Some(ship_id));
    let err = handle_ships_and_services(
        &ctx,
        &mut struct_sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("structure without insurance service");
    assert_eq!(err.code, "StationServiceUnavailable");

    // Insuring from another NPC station with insurance service (Amarr VIII 60_008_494) succeeds,
    // and calling again without voidOld=true rejects duplicate active contract (InsureShipFailedSingleContract)
    let mut amarr_station = make_session(char_id, corp_id, Some(60_008_494), Some(ship_id));
    let remote_rep = handle_ships_and_services(
        &ctx,
        &mut amarr_station,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("ship insured from remote station with insurance service");
    assert!(matches!(remote_rep, PyRep::KeyVal(_)));

    let err_dup = handle_ships_and_services(
        &ctx,
        &mut amarr_station,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("duplicate contract without voidOld rejected");
    assert_eq!(err_dup.code, "InsureShipFailedSingleContract");

    // Clear test contract before checking ownership, negative cost, and Basic -> Platinum lifecycle
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::fitting_containers_ships::write_container_ship_state_snap(snap, |st| {
            st.insurance_contracts.remove(&ship_id);
        });
    });

    // Ownership violation
    let mut other_sess = make_session(other_char_id, 1_000_009, Some(station_id), Some(ship_id));
    let err = handle_ships_and_services(
        &ctx,
        &mut other_sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("not owner");
    assert_eq!(err.code, "OwnershipViolation");

    // Invalid cost / non-positive fraction rejected
    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(ship_id));
    let err = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(-0.5)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("negative cost");
    assert!(err.code.contains("InsureShipInvalidCost") || err.message.contains("InsureShipInvalidCost"));

    // Personal Basic (0.5), duplicate without voidOld fails, then voidOld=true succeeds via exact ISK cost
    handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(0.5)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("basic insurance ok");

    let dup_err = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0), PyRep::Bool(false), PyRep::Bool(false)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("duplicate without voidOld");
    assert!(dup_err.message.contains("InsureShipFailedSingleContract") || dup_err.code == "UserError");

    // Compute exact Platinum cost and void_old=true via kwargs
    let ref_val = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(ship_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("ref price")
    .as_f64()
    .unwrap();
    let plat_cost = crate::formulas::compute_insurance_levels(ref_val)[5].1;
    let kw_void = HashMap::from([("voidOld".to_string(), PyRep::Bool(true))]);
    handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(plat_cost)],
        &kw_void,
    )
    .await
    .expect("handled")
    .expect("replace with exact platinum cost");

    // Corp ship insurance: wrong corp -> OwnershipViolation; member without director/accountant role -> CorpRoleDenied; CEO succeeds
    let mut wrong_corp = make_session(char_id, 999_999, Some(station_id), None);
    let err_wc = handle_ships_and_services(
        &ctx, &mut wrong_corp, None, "insuranceSvc", "InsureShip",
        &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)], &empty_kw,
    ).await.expect("handled").expect_err("wrong corp rejected");
    assert_eq!(err_wc.code, "OwnershipViolation");

    let mut non_dir = make_session(other_char_id, corp_id, Some(station_id), None);
    let err = handle_ships_and_services(
        &ctx, &mut non_dir, None, "insuranceSvc", "InsureShip",
        &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)], &empty_kw,
    ).await.expect("handled").expect_err("corp role denied");
    assert_eq!(err.code, "CorpRoleDenied");

    handle_ships_and_services(
        &ctx, &mut sess, None, "insuranceSvc", "InsureShip",
        &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0), PyRep::Bool(true)], &empty_kw,
    ).await.expect("handled").expect("CEO corp insure succeeds");

    // GetContracts personal (0) and corporate (1)
    let contracts_personal = handle_ships_and_services(
        &ctx, &mut sess, None, "insuranceSvc", "GetContracts", &[PyRep::Integer(0)], &empty_kw,
    ).await.expect("handled").expect("personal contracts");
    assert!(matches!(contracts_personal, PyRep::List(ref items) if items.len() == 1));

    let contracts_corp = handle_ships_and_services(
        &ctx, &mut sess, None, "insuranceSvc", "GetContracts", &[PyRep::Integer(1)], &empty_kw,
    ).await.expect("handled").expect("corp contracts");
    assert!(matches!(contracts_corp, PyRep::List(ref items) if items.len() == 1));

    // GetInsurancePrice by ship typeID directly and GetInsurancePrices
    let type_price = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(587)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("type price");
    assert!(type_price.as_f64().unwrap() > 0.0);

    let all_prices = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetInsurancePrices",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("all prices");
    assert!(matches!(all_prices, PyRep::List(ref items) if !items.is_empty()));

    // UnInsureShip + verify GetContractForShip returns None afterward
    let unins = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "UnInsureShip",
        &[PyRep::Integer(ship_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("uninsure ok");
    assert!(matches!(unins, PyRep::KeyVal(_)));

    let after = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "insuranceSvc",
        "GetContractForShip",
        &[PyRep::Integer(ship_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("get contract after void");
    assert!(matches!(after, PyRep::None));
}

#[tokio::test]
async fn dynamic_item_repackage_assemble_repair_and_label_paths() {
    let (ctx, char_id, other_char_id, corp_id, station_id, ship_id, _) = setup_insurance_fixture();
    let empty_kw = HashMap::new();
    let ab_stack_id = 96_610_201i64;
    let muta_stack_id = 96_610_202i64;
    let fitted_ab_id = 96_610_203i64;
    let packaged_ship_id = 96_610_204i64;

    let (&muta_type_id, muta_info) = ctx
        .sde
        .dynamic_item_attributes
        .iter()
        .find(|(_, info)| !info.applicable_type_ids.is_empty())
        .expect("seeded dynamic item mutaplasmid");
    let target_mod_type_id = muta_info.applicable_type_ids[0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ab_stack_id,
            InventoryItemRecord {
                item_id: ab_stack_id,
                type_id: target_mod_type_id,
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
            muta_stack_id,
            InventoryItemRecord {
                item_id: muta_stack_id,
                type_id: muta_type_id,
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
            fitted_ab_id,
            InventoryItemRecord {
                item_id: fitted_ab_id,
                type_id: target_mod_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 5.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            packaged_ship_id,
            InventoryItemRecord {
                item_id: packaged_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(ship_id));

    // Cannot mutate fitted item
    let err = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "dynamicItemService",
        "CreateDynamicItem",
        &[PyRep::Integer(muta_type_id as i64), PyRep::Integer(fitted_ab_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("fitted module cannot be mutated");
    assert_eq!(err.code, "CannotMutateFittedItem");

    // Mutate stacked item (quantity 2 -> splits off 1 singleton + decrements stack to 1 and consumes 1 mutaplasmid)
    let created = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "dynamicItemService",
        "CreateDynamicItem",
        &[PyRep::Integer(muta_stack_id), PyRep::Integer(ab_stack_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("stack mutation succeeds");
    assert!(matches!(created, PyRep::KeyVal(_)));

    // Mutate remaining item (quantity == 1 transforms in-place)
    let created_single = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "dynamicItemService",
        "CreateDynamicItem",
        &[PyRep::Integer(muta_type_id as i64), PyRep::Integer(ab_stack_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("single item mutation succeeds");
    assert!(matches!(created_single, PyRep::KeyVal(_)));

    // GetDynamicItemInfo by non-owner without contract -> OwnershipViolation
    let mut other = make_session(other_char_id, 1_000_009, Some(station_id), None);
    let err = handle_ships_and_services(
        &ctx,
        &mut other,
        None,
        "dynamicItemService",
        "GetDynamicItemInfo",
        &[PyRep::Integer(ab_stack_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("ownership violation");
    assert_eq!(err.code, "OwnershipViolation");

    // Cannot repackage mutated item
    let err = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(ab_stack_id)])],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("mutated item cannot be repackaged");
    assert_eq!(err.code, "CannotRepackageMutatedItem");

    // Cannot repackage active ship
    let err = handle_ships_and_services(
        &ctx,
        &mut sess,
        None,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::Integer(ship_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect_err("active ship cannot be repackaged");
    assert_eq!(err.code, "CannotRepackageActiveShip");

    // RepairAll repairs fitted_ab_id damage & charges ISK
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let rep = handle_ships_and_services(&ctx, &mut sess, None, "repairSvc", "RepairAll", &[], &empty_kw)
        .await.expect("handled").expect("repair all");
    assert!(matches!(rep, PyRep::KeyVal(_)));
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert!(bal_after < bal_before);

    // AssembleShip on packaged_ship_id, then SetLabel with reserved prefix rejected & valid label succeeds
    handle_ships_and_services(&ctx, &mut sess, None, "ship", "AssembleShip", &[PyRep::Integer(packaged_ship_id)], &empty_kw)
        .await.expect("handled").expect("assemble ship");

    let lbl_err = handle_ships_and_services(
        &ctx, &mut sess, None, "invbroker", "SetLabel",
        &[PyRep::Integer(packaged_ship_id), PyRep::String("courier_cargo:hack".into())], &empty_kw,
    ).await.expect("handled").expect_err("courier_cargo prefix blocked");
    assert_eq!(lbl_err.code, "CustomNameNotAllowed");

    handle_ships_and_services(
        &ctx, &mut sess, None, "invbroker", "SetLabel",
        &[PyRep::Integer(packaged_ship_id), PyRep::String("Fleet Rifter".into())], &empty_kw,
    ).await.expect("handled").expect("valid custom name");
    let saved_label = ctx.db.with_snapshot(|s| s.item_custom_names.get(&packaged_ship_id).cloned());
    assert_eq!(saved_label.as_deref(), Some("Fleet Rifter"));
}
