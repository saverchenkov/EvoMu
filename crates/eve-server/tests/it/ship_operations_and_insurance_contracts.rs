//! Parity tests for ship operations and insurance contracts.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn new_test_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    common::pin_clock(&ctx);
    ctx
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() });
    });
}

#[tokio::test]
async fn test_insurance_contracts_get_for_ship_and_corp_roles() {
    // parity: rpc:insuranceSvc.GetContractForShip
    // parity: rpc:insuranceSvc.GetInsurancePrice
    // parity: feat:combat.insurance-contracts
    eve_testkit::attest(&[
        "rpc:insuranceSvc.GetContractForShip",
        "rpc:insuranceSvc.GetInsurancePrice",
        "feat:combat.insurance-contracts",
    ]);
    let ctx = new_test_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(8801);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 920_001i64;
    seed_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // GetInsurancePrice: takes ship typeID or shipID and returns float base price
    let expected_ref_val = eve_server::formulas::compute_ship_insurance_reference_value(&ctx.sde, 587);
    assert!(expected_ref_val > 0.0);
    let p_type = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(587)], &kw, None).await.unwrap();
    assert_eq!(p_type.as_f64(), Some(expected_ref_val));
    let p_ship = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    assert_eq!(p_ship.as_f64(), Some(expected_ref_val));

    // GetInsurancePrice with foreign ship item fails with OwnershipViolation
    let foreign_test_ship = 920_002i64;
    seed_item(&ctx, foreign_test_ship, 587, 90_000_002, station_id, InventoryFlag::Hangar, 1, true);
    let p_foreign_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(foreign_test_ship)], &kw, None).await.unwrap_err();
    assert_eq!(p_foreign_err.code, "OwnershipViolation");

    // InsureShip with missing tier/cost argument fails with InsureShipInvalidCost
    let missing_tier_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap_err();
    assert_eq!(missing_tier_err.code, "InsureShipInvalidCost");

    let initial = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    assert_eq!(initial, PyRep::None);

    let bal0 = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    // Insure with fractional tier input 0.5 (Basic)
    let insure_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(0.5)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ins_kv) = insure_res else { panic!("expected KeyVal") };
    let cost = ins_kv.get("costISK").and_then(|v| v.as_f64()).unwrap();
    let payout = ins_kv.get("payoutISK").and_then(|v| v.as_f64()).unwrap();
    assert!(cost > 0.0 && payout > cost);
    let bal1 = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(bal0.0 - bal1.0, eve_db::IskCents::from_f64_isk(cost).0);

    // Attempting to insure an already insured ship without voidOld fails with InsureShipFailedSingleContract
    let single_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(0.5)], &kw, None).await.unwrap_err();
    assert_eq!(single_err.code, "InsureShipFailedSingleContract");
    assert_eq!(single_err.message, "InsureShipFailedSingleContract");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance), bal1);

    // Contract details after first insurance
    let contract_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(c_kv) = contract_res else { panic!("expected KeyVal") };
    assert_eq!(c_kv.get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
    assert_eq!(c_kv.get("shipTypeID").and_then(|v| v.as_i64()), Some(587));
    assert_eq!(c_kv.get("ownerID").and_then(|v| v.as_i64()), Some(char_id));
    let frac = c_kv.get("fraction").and_then(|v| v.as_f64()).unwrap();
    assert!((frac - 0.5).abs() < 1e-6);
    let initial_j = ctx.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "19"));
    assert!(initial_j, "InsureShip must create WalletJournalEntry type 19");

    // Insure with ISK premium and voidOld=True (replaces existing contract with Platinum)
    let platinum_premium = expected_ref_val * 0.30;
    let mut kw_void = HashMap::new();
    kw_void.insert("voidOld".into(), PyRep::Bool(true));
    let up_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(platinum_premium)], &kw_void, None).await.unwrap();
    let PyRep::KeyVal(up_ins_kv) = up_res else { panic!("expected KeyVal") };
    let up_cost = up_ins_kv.get("costISK").and_then(|v| v.as_f64()).unwrap();
    assert!((up_cost - platinum_premium).abs() < 1e-4);
    let bal2 = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(bal1.0 - bal2.0, eve_db::IskCents::from_f64_isk(platinum_premium).0);

    let replaced_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(rep_kv) = replaced_res else { panic!("expected KeyVal") };
    let rep_frac = rep_kv.get("fraction").and_then(|v| v.as_f64()).unwrap();
    assert!((rep_frac - 1.0).abs() < 1e-6);

    // Negative test: invalid client-supplied ISK premium (R-INS-02)
    let bal_before_invalid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    let inv_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.01)], &kw_void, None).await.unwrap_err();
    assert_eq!(inv_err.code, "InsureShipInvalidCost");
    let bal_after_invalid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(bal_before_invalid, bal_after_invalid);

    // Test all 6 tiers via their advertised ISK premiums from GetInsurancePrices (R-INS-03)
    let prices_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrices", &[PyRep::Integer(587)], &kw, None).await.unwrap();
    let PyRep::List(prices_list) = prices_res else { panic!("expected List") };
    let PyRep::KeyVal(price_entry) = &prices_list[0] else { panic!("expected KeyVal") };
    let PyRep::List(tiers) = price_entry.get("levels").unwrap() else { panic!("expected List") };
    assert_eq!(tiers.len(), 6);

    let expected_fractions = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
    for (i, tier_rep) in tiers.iter().enumerate() {
        let PyRep::KeyVal(tier_kv) = tier_rep else { panic!("expected KeyVal") };
        let name = tier_kv.get("name").and_then(|v| v.as_str()).unwrap();
        let cost_isk = tier_kv.get("costISK").and_then(|v| v.as_f64()).unwrap();
        let payout_isk = tier_kv.get("payoutISK").and_then(|v| v.as_f64()).unwrap();
        let exp_frac = expected_fractions[i];

        let tier_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(cost_isk)], &kw_void, None).await.unwrap();
        let PyRep::KeyVal(t_kv) = tier_res else { panic!("expected KeyVal") };
        assert_eq!(t_kv.get("level").and_then(|v| v.as_str()), Some(name));
        assert_eq!(t_kv.get("costISK").and_then(|v| v.as_f64()), Some(cost_isk));
        assert_eq!(t_kv.get("payoutISK").and_then(|v| v.as_f64()), Some(payout_isk));
        assert_eq!(t_kv.get("fraction").and_then(|v| v.as_f64()), Some(exp_frac));
        assert!(t_kv.get("startDate").and_then(|v| v.as_i64()).is_some());
        assert!(t_kv.get("endDate").and_then(|v| v.as_i64()).is_some());

        let contract_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
        let PyRep::KeyVal(c_kv) = contract_res else { panic!("expected KeyVal") };
        assert_eq!(c_kv.get("level").and_then(|v| v.as_str()), Some(name));
        assert_eq!(c_kv.get("costISK").and_then(|v| v.as_f64()), Some(cost_isk));
        assert_eq!(c_kv.get("payoutISK").and_then(|v| v.as_f64()), Some(payout_isk));
        assert_eq!(c_kv.get("fraction").and_then(|v| v.as_f64()), Some(exp_frac));
        assert!(c_kv.get("startDate").and_then(|v| v.as_i64()).is_some());
        assert!(c_kv.get("endDate").and_then(|v| v.as_i64()).is_some());
    }

    // Non-ship category rejection
    let non_ship_id = 920_009i64;
    seed_item(&ctx, non_ship_id, 34, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let non_ship_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(non_ship_id), PyRep::Real(1.0)], &kw, None).await.unwrap_err();
    assert_eq!(non_ship_err.code, "InsureShipFailed");
    assert_eq!(non_ship_err.message, "InsureShipFailed");

    // Test packaged ship rejection
    let packaged_ship_id = 920_003i64;
    seed_item(&ctx, packaged_ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    let pkg_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(packaged_ship_id), PyRep::Real(1.0)], &kw, None).await.unwrap_err();
    assert_eq!(pkg_err.code, "InsureShipNotAssembled");

    // Corporate ship insurance and R-INS-01 payout routing
    let test_corp_id = 1_000_175i64;
    let corp_ship_id = 920_004i64;
    seed_item(&ctx, corp_ship_id, 587, test_corp_id, station_id, InventoryFlag::Hangar, 1, true);
    let corp_initial_bal = eve_db::IskCents::from_isk(50_000_000);
    ctx.db.with_mut_snapshot(|s| {
        let corp = eve_db::CorporationRecord {
            corporation_id: test_corp_id, name: "Corp Ins".into(), ticker: "CI".into(), ceo_id: 999_888,
            master_wallet: corp_initial_bal,
            wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(corp_initial_bal),
            ..Default::default()
        };
        s.corporations.insert(test_corp_id, corp);
        if let Some(c) = s.characters.get_mut(&char_id) { c.corporation_id = test_corp_id; }
    });
    sess.corporation_id = Some(test_corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let mut kw_corp = HashMap::new();
    kw_corp.insert("isCorpItem".into(), PyRep::Bool(true));
    let corp_ins = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(corp_ship_id), PyRep::Real(1.0)], &kw_corp, None).await.unwrap();
    assert!(matches!(corp_ins, PyRep::KeyVal(_)));
    let corp_j = ctx.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == test_corp_id && j.entry_type == "19"));
    assert!(corp_j, "Corporate insurance must append WalletJournalEntry type 19 with corp as payer");

    // R-INS-01: Destroy corp-insured ship and verify payout goes to corp master wallet, leaving pilot balance untouched
    let pilot_bal_before_payout = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    let corp_bal_before_payout = ctx.db.with_snapshot(|s| s.corporations.get(&test_corp_id).unwrap().master_wallet);
    let payout_returned = eve_server::services::combat_crimewatch::combat_wrecks_killmails::process_ship_insurance_payout(
        &ctx, char_id, corp_ship_id, 587, false,
    );
    assert!(payout_returned > 0.0);
    let corp_bal_after_payout = ctx.db.with_snapshot(|s| s.corporations.get(&test_corp_id).unwrap().master_wallet);
    let pilot_bal_after_payout = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(pilot_bal_after_payout, pilot_bal_before_payout, "Pilot personal balance must NOT change for corp-insured ship payout!");
    assert_eq!(corp_bal_after_payout.0 - corp_bal_before_payout.0, eve_db::IskCents::from_f64_isk(payout_returned).0, "Corp master wallet must increase by exact insurance payout!");
    let corp_div1_bal = ctx.db.with_snapshot(|s| s.corporations.get(&test_corp_id).unwrap().wallet_divisions[0].balance);
    assert_eq!(corp_div1_bal, corp_bal_after_payout, "Corp division 1 must match master wallet");
    let corp_payout_j = ctx.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == test_corp_id && j.entry_type == "insurance_payout"));
    assert!(corp_payout_j, "Wallet journal must record insurance_payout with corp as character_id");

    sess.corp_role = 0;

    // Test persistence across SQLite restart
    let pid = std::process::id();
    let json_path = std::env::temp_dir().join(format!("evomu_ins_{pid}.json"));
    let sqlite_path = std::env::temp_dir().join(format!("evomu_ins_{pid}.sqlite"));
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    ctx.db.save_snapshot_to_file(&json_path).expect("save snapshot");
    {
        let writer = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open sqlite");
        writer.load_snapshot_from_file(&json_path).expect("load snapshot");
    }
    let reopened_repo = std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen"));
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    let ctx2 = ServerContext::with_repository(reopened_repo);
    let restored_res = ctx2.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(res_kv) = restored_res else { panic!("expected KeyVal") };
    assert!((res_kv.get("fraction").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-6);

    let not_found_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(999_999_999)], &kw, None).await.unwrap_err();
    assert_eq!(not_found_err.code, "ShipNotFound");

    let foreign_ship_id = 920_002i64;
    seed_item(&ctx, foreign_ship_id, 587, 90_000_002, station_id, InventoryFlag::Hangar, 1, true);
    let owner_err = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(foreign_ship_id)], &kw, None).await.unwrap_err();
    assert_eq!(owner_err.code, "OwnershipViolation");

    let my_contracts = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContracts", &[PyRep::Bool(false)], &kw, None).await.unwrap();
    let PyRep::List(c_list) = my_contracts else { panic!("expected List") };
    assert!(!c_list.is_empty());

    let corp_denied = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContracts", &[PyRep::Bool(true)], &kw, None).await.unwrap_err();
    assert_eq!(corp_denied.code, "CorpRoleDenied");

    sess.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR;
    let corp_contracts = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContracts", &[PyRep::Bool(true)], &kw, None).await.unwrap();
    let PyRep::List(_) = corp_contracts else { panic!("expected List") };

    let unins_res = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "UnInsureShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(unins_kv) = unins_res else { panic!("expected KeyVal") };
    assert_eq!(unins_kv.get("voided"), Some(&PyRep::Bool(true)));

    let contract_after_void = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "GetContractForShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    assert_eq!(contract_after_void, PyRep::None);
}

#[tokio::test]
async fn test_ship_lifecycle_leave_ship_jettison_and_drop() {
    // parity: rpc:ship.LeaveShip
    // parity: rpc:ship.Jettison
    // parity: rpc:ship.Drop
    eve_testkit::attest(&["rpc:ship.LeaveShip", "rpc:ship.Jettison", "rpc:ship.Drop"]);
    let ctx = new_test_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(8802);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let ship_id = 100_000_001i64;

    let cargo_item_id = 930_001i64;
    seed_item(&ctx, cargo_item_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false);

    let jet_dock_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_item_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_dock_err.code, "CannotJettisonWhileDocked");

    let drop_dock_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Integer(cargo_item_id)])], &kw, None).await.unwrap_err();
    assert_eq!(drop_dock_err.code, "CannotDropWhileDocked");

    ctx.advance_secs(12);
    let leave_res = ctx.dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None).await.unwrap();
    let capsule_id = leave_res.as_i64().unwrap();
    assert_eq!(sess.ship_id, Some(capsule_id));

    let old_ship = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(old_ship, Some((station_id, InventoryFlag::Hangar)));

    let leave_cap_err = ctx.dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None).await.unwrap_err();
    assert_eq!(leave_cap_err.code, "CannotEjectFromCapsule");

    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    ctx.advance_secs(12);
    let leave_reuse = ctx.dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None).await.unwrap();
    assert_eq!(leave_reuse.as_i64(), Some(capsule_id), "LeaveShip must reuse character's existing capsule");
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();

    let leave_sp_err = ctx.dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None).await.unwrap_err();
    assert_eq!(leave_sp_err.code, "CannotLeaveShipInSpace");

    let fitted_mod_id = 930_090i64;
    seed_item(&ctx, fitted_mod_id, 34, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
    let jet_fit_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(fitted_mod_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_fit_err.code, "CannotJettisonFittedModule");

    let rig_id = 930_091i64;
    seed_item(&ctx, rig_id, 31047, char_id, ship_id, InventoryFlag::RigSlot0, 1, true);
    let jet_rig_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(rig_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_rig_err.code, "CannotJettisonFittedModule");

    let stored_ship_id = 930_092i64;
    seed_item(&ctx, stored_ship_id, 587, char_id, ship_id, InventoryFlag::ShipHangar, 1, true);
    let jet_ship_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(stored_ship_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_ship_err.code, "CannotJettisonStoredShip");

    let cargo_ship_id = 930_093i64;
    seed_item(&ctx, cargo_ship_id, 587, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    let jet_cat6_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_ship_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_cat6_err.code, "CannotJettisonShip");

    let cont_id = 930_094i64;
    seed_item(&ctx, cont_id, 3465, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    let jet_cont_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cont_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_cont_err.code, "CannotPutContainerInContainer");

    let huge_cargo_id = 930_095i64;
    seed_item(&ctx, huge_cargo_id, 34, char_id, ship_id, InventoryFlag::Cargo, 10_000_000, false);
    let jet_cap_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(huge_cargo_id)])], &kw, None).await.unwrap_err();
    assert_eq!(jet_cap_err.code, "CapacityExceeded");

    let jet_res = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_item_id)])], &kw, None).await.unwrap();
    let PyRep::Tuple(jet_tup) = jet_res else { panic!("expected Tuple") };
    let jetcan_id = match &jet_tup[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
    let PyRep::List(jet_items) = &jet_tup[1] else { panic!("expected List") };
    assert_eq!(jet_items.len(), 1);
    assert_eq!(jet_items[0].as_i64(), Some(cargo_item_id));

    let item_loc = ctx.db.with_snapshot(|s| s.items.get(&cargo_item_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(item_loc, Some((jetcan_id, InventoryFlag::Cargo)));

    let sys = ctx.systems.get_or_spawn(sys_id);
    assert!(sys.get_ball_snapshot(jetcan_id).await.unwrap().is_some());

    let cargo2_id = 930_002i64;
    seed_item(&ctx, cargo2_id, 34, char_id, ship_id, InventoryFlag::Cargo, 50, false);
    let cd_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo2_id)])], &kw, None).await.unwrap_err();
    assert_eq!(cd_err.code, "JettisonCooldownActive");

    let drop_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(cargo2_id), PyRep::Integer(1)])])], &kw, None).await.unwrap_err();
    assert_eq!(drop_err.code, "UserError");

    let bubble_id = 930_096i64;
    seed_item(&ctx, bubble_id, 12198, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    let drop_bubble_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(bubble_id), PyRep::Integer(1)])])], &kw, None).await.unwrap_err();
    assert_eq!(drop_bubble_err.code, "UserError");

    let drone_id = 930_097i64;
    seed_item(&ctx, drone_id, 2454, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    let drop_drone_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(drone_id), PyRep::Integer(1)])])], &kw, None).await.unwrap_err();
    assert_eq!(drop_drone_err.code, "UserError");

    let depot_item_id = 930_003i64;
    seed_item(&ctx, depot_item_id, 33474, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    let drop_foreign_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(depot_item_id), PyRep::Integer(1)])]), PyRep::Integer(999_999_999)], &kw, None).await.unwrap_err();
    assert_eq!(drop_foreign_err.code, "UserError");

    let drop_res = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(depot_item_id), PyRep::Integer(1)])])], &kw, None).await.unwrap();
    let PyRep::Dict(dp_dict) = drop_res else { panic!("expected Dict") };
    let launched = dp_dict.iter().find(|(k, _)| k.as_i64() == Some(depot_item_id)).map(|(_, v)| v).unwrap();
    let PyRep::List(ball_ids) = launched else { panic!("expected List") };
    assert_eq!(ball_ids.len(), 1);
    assert_eq!(ball_ids[0].as_i64(), Some(depot_item_id));

    let depot_loc = ctx.db.with_snapshot(|s| s.items.get(&depot_item_id).map(|i| (i.location_id, i.type_id)));
    assert_eq!(depot_loc, Some((sys_id as i64, 33474)));
    assert!(sys.get_ball_snapshot(depot_item_id).await.unwrap().is_some());

    let stack_depot_id = 930_004i64;
    seed_item(&ctx, stack_depot_id, 33474, char_id, ship_id, InventoryFlag::Cargo, 5, false);
    let drop_stack_res = ctx.dispatch_rpc(&mut sess, "ship", None, "Drop", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(stack_depot_id), PyRep::Integer(2)])])], &kw, None).await.unwrap();
    let PyRep::Dict(stack_dict) = drop_stack_res else { panic!("expected Dict") };
    let launched_stack_id = stack_dict.iter().find(|(k, _)| k.as_i64() == Some(stack_depot_id)).and_then(|(_, v)| match v {
        PyRep::List(l) => l.first().and_then(|x| x.as_i64()),
        _ => None,
    }).unwrap();
    assert_ne!(launched_stack_id, stack_depot_id);
    let rem_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_depot_id).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(3));

    // LaunchFromContainer item validation, parent location check, item_positions, and SlimItemMeta.owner_id
    let unlaunched_cargo_id = 930_050i64;
    seed_item(&ctx, unlaunched_cargo_id, 34, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    let inv_launch = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchFromContainer", &[PyRep::Integer(ship_id), PyRep::Integer(unlaunched_cargo_id)], &kw, None).await.unwrap_err();
    assert_eq!(inv_launch.code, "InvalidLaunchItem");

    let parent_docked = 930_051i64;
    let docked_container = 930_052i64;
    seed_item(&ctx, parent_docked, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, docked_container, 3465, char_id, parent_docked, InventoryFlag::Cargo, 1, true);
    let not_in_sp = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchFromContainer", &[PyRep::Integer(parent_docked), PyRep::Integer(docked_container)], &kw, None).await.unwrap_err();
    assert_eq!(not_in_sp.code, "LaunchAccessDenied");

    // Launch valid assembled container from active ship in space
    let launchable_cont_id = 930_053i64;
    seed_item(&ctx, launchable_cont_id, 3465, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    let ok_launch = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchFromContainer", &[PyRep::Integer(ship_id), PyRep::Integer(launchable_cont_id)], &kw, None).await.unwrap();
    assert!(matches!(ok_launch, PyRep::KeyVal(_)));
    assert!(ctx.db.with_snapshot(|s| s.item_positions.contains_key(&launchable_cont_id)));
    let launched_ball_meta = sys.ballpark.read().unwrap().ball_slim_meta.get(&launchable_cont_id).cloned();
    assert_eq!(launched_ball_meta.map(|m| m.owner_id), Some(char_id));
}
