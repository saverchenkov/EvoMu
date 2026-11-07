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
            members: std::collections::HashSet::from([char_id]),
            member_roles: HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
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

    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&test_corp_id) {
            corp.member_roles.insert(char_id, 0);
        }
    });
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


pub(crate) mod ship_bays_boarding_and_corp_hangars {
    #[allow(unused_imports)]
    use super::*;

// Parity tests for ship bays, scoop variants, store vessel, board stored ship, and ship password POS forcefields.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
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

async fn spawn_space_item(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, group_id: u32, category_id: u32, owner_id: i64, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball_with_meta(
        Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000.0, radius: 20.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        CombatState::default(),
        SlimItemMeta { group_id, category_id, owner_id, corp_id: -1, alliance_id: -1, ..Default::default() },
    ).await;
}

#[tokio::test]
async fn test_ship_bays_scoop_store_board_space_and_password() {
    // parity: rpc:ship.Scoop
    // parity: rpc:ship.ScoopToFleetHangar
    // parity: rpc:ship.ScoopToInfrastructureHold
    // parity: rpc:ship.ScoopToMobileDepotHold
    // parity: rpc:ship.ScoopToSMA
    // parity: rpc:ship.SetShipPassword
    // parity: rpc:ship.StoreVessel
    eve_testkit::attest(&[
        "rpc:ship.Scoop",
        "rpc:ship.ScoopToFleetHangar",
        "rpc:ship.ScoopToInfrastructureHold",
        "rpc:ship.ScoopToMobileDepotHold",
        "rpc:ship.ScoopToSMA",
        "rpc:ship.SetShipPassword",
        "rpc:ship.StoreVessel",
    ]);

    let ctx = new_test_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(8803);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let orca_id = 820_010i64;
    seed_item(&ctx, orca_id, 28606, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let foreign_char_id = 90_000_002i64;
    let foreign_corp_id = 1_000_045i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&foreign_char_id) {
            ch.corporation_id = foreign_corp_id;
        }
    });

    let get_cfg = ctx.dispatch_rpc(&mut sess, "ship", None, "GetShipConfiguration", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    let PyRep::Dict(cfg_dict) = get_cfg else { panic!("expected Dict") };
    assert!(cfg_dict.len() >= 4);

    let new_cfg = vec![
        (PyRep::String("SMB_AllowCorpAccess".into()), PyRep::Bool(true)),
        (PyRep::String("FleetHangar_AllowCorpAccess".into()), PyRep::Bool(true)),
        (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(false)),
        (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(false)),
    ];
    ctx.dispatch_rpc(&mut sess, "ship", None, "ConfigureShip", &[PyRep::Integer(orca_id), PyRep::Dict(new_cfg)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().active_ship_id = orca_id; });
    sess.ship_id = Some(orca_id);

    // Set active ship password for POS forcefield
    let pw_res = ctx.dispatch_rpc(&mut sess, "ship", None, "SetShipPassword", &[PyRep::String("orca-pass".into())], &kw, None).await.unwrap();
    assert_eq!(pw_res, PyRep::None);

    let mut sess_foreign = SessionState::new(8804);
    common::login_pilot(&ctx, &mut sess_foreign, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess_foreign, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(foreign_char_id)], &kw, None).await.unwrap();

    let pw_err = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::Integer(orca_id), PyRep::String("hack".into())], &kw, None).await.unwrap_err();
    assert_eq!(pw_err.code, "OwnershipViolation");

    // Verify SetShipPassword persisted
    let saved_pw = ctx.db.with_snapshot(|snap| {
        snap.owner_notes.get(&-8_400_000_000_000)
            .and_then(|n| serde_json::from_str::<serde_json::Value>(&n.note).ok())
            .and_then(|v| v.get("ship_passwords")?.get(&orca_id.to_string())?.as_str().map(|s| s.to_string()))
    });
    assert_eq!(saved_pw, Some("orca-pass".into()));

    // R-PW-01: SetShipPassword gates POS forcefield coverage
    use eve_server::services::inventory_fitting::fitting_ship_bays::can_enter_forcefield;
    let tower_id = 999_999i64;
    let foreign_ship_id = 820_090i64;
    seed_item(&ctx, foreign_ship_id, 587, foreign_char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&foreign_char_id) { ch.active_ship_id = foreign_ship_id; }
    });
    sess_foreign.ship_id = Some(foreign_ship_id);

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(tower_id, InventoryItemRecord { item_id: tower_id, owner_id: char_id, ..Default::default() });
        let mut settings = eve_db::snapshot_ext::StarbaseDetailSettings::default();
        settings.password = Some("orca-pass".into());
        settings.allow_corporation_members = false;
        settings.allow_alliance_members = false;
        s.starbase_settings.insert(tower_id, settings);
    });

    // Tower owner can always enter without password match, even if POS password does not match ship password
    ctx.db.with_mut_snapshot(|s| {
        if let Some(settings) = s.starbase_settings.get_mut(&tower_id) { settings.password = Some("tower-secure-pass".into()); }
    });
    assert!(can_enter_forcefield(&ctx, char_id, orca_id, tower_id));
    let rpc_owner_assume = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw, None).await.unwrap();
    assert_eq!(rpc_owner_assume, PyRep::None);

    // Foreign pilot (not owner, allow_corp = false):
    // Without matching ship password cannot enter forcefield
    assert!(!can_enter_forcefield(&ctx, foreign_char_id, foreign_ship_id, tower_id));

    // Non-owner with wrong password calling AssumeStructureControl fails with ForcefieldPasswordMismatch
    let assume_err = ctx.dispatch_rpc(&mut sess_foreign, "posMgr", None, "AssumeStructureControl", &[PyRep::Integer(tower_id)], &kw, None).await.unwrap_err();
    assert_eq!(assume_err.code, "ForcefieldPasswordMismatch");

    // Foreign pilot sets ship password matching POS password -> can enter forcefield
    ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::String("tower-secure-pass".into())], &kw, None).await.unwrap();
    assert!(can_enter_forcefield(&ctx, foreign_char_id, foreign_ship_id, tower_id));

    // R-PW-01: SMA and Fleet Hangar access depends ONLY on config flags, NOT passwords
    let ore_id = 820_021i64;
    seed_item(&ctx, ore_id, 1228, foreign_char_id, station_id, InventoryFlag::Hangar, 20, false);
    let foreign_docked_ship = 820_091i64;
    seed_item(&ctx, foreign_docked_ship, 587, foreign_char_id, station_id, InventoryFlag::Hangar, 1, true);

    // Foreign character is NOT in same corp and NOT in same fleet -> denied access regardless of password
    let mut kw_with_pw = HashMap::new();
    kw_with_pw.insert("password".into(), PyRep::String("orca-pass".into()));
    let add_foreign = ctx.dispatch_rpc(&mut sess_foreign, "invbroker", None, "Add", &[PyRep::Integer(ore_id), PyRep::Integer(orca_id), PyRep::Integer(155)], &kw_with_pw, None).await.unwrap_err();
    assert_eq!(add_foreign.code, "FleetHangarAccessDenied");

    // Put foreign character in same fleet
    let fleet_id = 55_001i64;
    ctx.db.with_mut_snapshot(|s| {
        let mut fl = eve_db::FleetRecord::default();
        fl.fleet_id = fleet_id;
        fl.boss_id = char_id;
        fl.members = vec![char_id, foreign_char_id];
        s.fleets.insert(fleet_id, fl);
    });

    // Fleet access is currently false -> still denied
    let add_fleet_denied = ctx.dispatch_rpc(&mut sess_foreign, "invbroker", None, "Add", &[PyRep::Integer(ore_id), PyRep::Integer(orca_id), PyRep::Integer(155)], &kw, None).await.unwrap_err();
    assert_eq!(add_fleet_denied.code, "FleetHangarAccessDenied");

    // Enable fleet access flag on Orca
    let allow_fleet_cfg = vec![
        (PyRep::String("FleetHangar_AllowFleetAccess".into()), PyRep::Bool(true)),
        (PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(true)),
    ];
    ctx.dispatch_rpc(&mut sess, "ship", None, "ConfigureShip", &[PyRep::Integer(orca_id), PyRep::Dict(allow_fleet_cfg)], &kw, None).await.unwrap();

    // Fleet member requires ship password while one is configured (#176), succeeds with password, and succeeds without once cleared (#510)
    let fleet_pw_err = ctx.dispatch_rpc(&mut sess_foreign, "invbroker", None, "Add", &[PyRep::Integer(ore_id), PyRep::Integer(orca_id), PyRep::Integer(155)], &kw, None).await.unwrap_err();
    assert_eq!(fleet_pw_err.code, "FleetHangarAccessDenied");
    let add_fleet_pw = ctx.dispatch_rpc(&mut sess_foreign, "invbroker", None, "Add", &[PyRep::Integer(ore_id), PyRep::Integer(orca_id), PyRep::Integer(155)], &kw_with_pw, None).await.unwrap();
    assert!(matches!(add_fleet_pw, PyRep::KeyVal(_)));
    ctx.dispatch_rpc(&mut sess, "ship", None, "SetShipPassword", &[PyRep::Integer(orca_id), PyRep::String(String::new())], &kw, None).await.unwrap();
    let ore_id2 = 820_022i64;
    seed_item(&ctx, ore_id2, 1228, foreign_char_id, station_id, InventoryFlag::Hangar, 20, false);
    let add_fleet_ok = ctx.dispatch_rpc(&mut sess_foreign, "invbroker", None, "Add", &[PyRep::Integer(ore_id2), PyRep::Integer(orca_id), PyRep::Integer(155)], &kw, None).await.unwrap();
    assert!(matches!(add_fleet_ok, PyRep::KeyVal(_)));

    // Persistence across SQLite restart: POS forcefield password persists
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "ship_pw");
    assert!(can_enter_forcefield(&ctx_reloaded, foreign_char_id, foreign_ship_id, tower_id));

    // Store docked ship into Orca SMA
    let spare_rifter_id = 820_001i64;
    seed_item(&ctx, spare_rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let store_docked = ctx.dispatch_rpc(&mut sess, "ship", None, "StoreVessel", &[PyRep::Integer(spare_rifter_id), PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(sd_kv) = store_docked else { panic!("expected KeyVal") };
    assert_eq!(sd_kv.get("storedShipID").and_then(|v| v.as_i64()), Some(spare_rifter_id));
    assert_eq!(sd_kv.get("flagID").and_then(|v| v.as_i64()), Some(90));

    // Undock Orca into space
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(29637, eve_db::TrainedSkillRecord { type_id: 29637, level: 5, skill_points: 256_000.0 });
        sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();

    let sys = ctx.systems.get_or_spawn(sys_id);
    let orca_pos = sys.get_ball_snapshot(orca_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);

    // Negative test: ship.Scoop out-of-range (> 2500m)
    let far_can_id = 820_009i64;
    seed_item(&ctx, far_can_id, 3467, 0, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, far_can_id, 3467, 340, 2, 0, orca_pos + DVec3::new(5000.0, 0.0, 0.0)).await;
    let far_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(far_can_id)], &kw, None).await.unwrap_err();
    assert_eq!(far_err.code, "OutOfRange");

    // Foreign ship ScoopToSMA negative test
    let foreign_sma_ship = 820_092i64;
    seed_item(&ctx, foreign_sma_ship, 587, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, foreign_sma_ship, 587, 25, 6, foreign_char_id, orca_pos + DVec3::new(450.0, 0.0, 0.0)).await;
    let foreign_sma_err = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToSMA", &[PyRep::Integer(foreign_sma_ship)], &kw, None).await.unwrap_err();
    assert_eq!(foreign_sma_err.code, "OwnershipViolation");

    // Ship with non-charge cargo cannot be scooped into SMA
    let cargo_ship_id = 820_080i64;
    seed_item(&ctx, cargo_ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, 820_081i64, 34, char_id, cargo_ship_id, InventoryFlag::Cargo, 10, false);
    spawn_space_item(&ctx, sys_id, cargo_ship_id, 587, 25, 6, char_id, orca_pos + DVec3::new(480.0, 0.0, 0.0)).await;
    let sma_cargo_err = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToSMA", &[PyRep::Integer(cargo_ship_id)], &kw, None).await.unwrap_err();
    assert_eq!(sma_cargo_err.code, "ShipHasNonChargeCargo");

    // ScoopToSMA
    let space_rifter_id = 820_002i64;
    seed_item(&ctx, space_rifter_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, space_rifter_id, 587, 25, 6, char_id, orca_pos + DVec3::new(500.0, 0.0, 0.0)).await;

    let scoop_sma = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToSMA", &[PyRep::Integer(space_rifter_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ssma_kv) = scoop_sma else { panic!("expected KeyVal") };
    assert_eq!(ssma_kv.get("flagID").and_then(|v| v.as_i64()), Some(90));
    assert!(sys.get_ball_snapshot(space_rifter_id).await.unwrap().is_none());

    // ScoopToFleetHangar
    let space_ore_id = 820_003i64;
    seed_item(&ctx, space_ore_id, 1228, char_id, sys_id as i64, InventoryFlag::None, 50, false);
    spawn_space_item(&ctx, sys_id, space_ore_id, 1228, 18, 4, char_id, orca_pos + DVec3::new(400.0, 0.0, 0.0)).await;
    let scoop_fh = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToFleetHangar", &[PyRep::Integer(space_ore_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(sfh_kv) = scoop_fh else { panic!("expected KeyVal") };
    assert_eq!(sfh_kv.get("flagID").and_then(|v| v.as_i64()), Some(155));

    // ScoopToInfrastructureHold (Orca has no inf hold -> squall has inf hold)
    let infra_id = 820_004i64;
    seed_item(&ctx, infra_id, 12198, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, infra_id, 12198, 361, 22, char_id, orca_pos + DVec3::new(300.0, 0.0, 0.0)).await;
    let orca_no_inf = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToInfrastructureHold", &[PyRep::Integer(infra_id)], &kw, None).await.unwrap_err();
    assert_eq!(orca_no_inf.code, "NoInfrastructureHold");

    let squall_id = 820_030i64;
    seed_item(&ctx, squall_id, 81008, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, squall_id, 81008, 1202, 6, char_id, orca_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().active_ship_id = squall_id; });
    sess.ship_id = Some(squall_id);
    let scoop_inf = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToInfrastructureHold", &[PyRep::Integer(infra_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(sinf_kv) = scoop_inf else { panic!("expected KeyVal") };
    assert_eq!(sinf_kv.get("flagID"), Some(&PyRep::Integer(185)));

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().active_ship_id = orca_id; });
    sess.ship_id = Some(orca_id);

    // ScoopToMobileDepotHold
    let depot_id = 820_005i64;
    seed_item(&ctx, depot_id, 33474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, depot_id, 33474, 1246, 22, char_id, orca_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    let meta_id = 820_040i64;
    seed_item(&ctx, meta_id, 77114, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, meta_id, 77114, 25, 6, char_id, orca_pos + DVec3::new(150.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().active_ship_id = meta_id; });
    sess.ship_id = Some(meta_id);
    let scoop_depot = ctx.dispatch_rpc(&mut sess, "ship", None, "ScoopToMobileDepotHold", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(sdep_kv) = scoop_depot else { panic!("expected KeyVal") };
    assert_eq!(sdep_kv.get("flagID"), Some(&PyRep::Integer(179)));
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().active_ship_id = orca_id; });
    sess.ship_id = Some(orca_id);

    // Scoop (cargo hold) - foreign theft, anchored, and structure negative tests
    let can_theft_id = 820_006i64;
    seed_item(&ctx, can_theft_id, 3467, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, can_theft_id, 3467, 340, 2, foreign_char_id, orca_pos + DVec3::new(250.0, 0.0, 0.0)).await;
    let can_theft_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(can_theft_id)], &kw, None).await.unwrap_err();
    assert_eq!(can_theft_err.code, "OwnershipViolation");

    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.space_object_meta.insert(can_theft_id, eve_db::PersistedSpaceObjectMeta { anchor_completed: true, ..Default::default() }); });
    let anch_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(can_theft_id)], &kw, None).await.unwrap_err();
    assert_eq!(anch_err.code, "CannotScoopAnchored");
    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.space_object_meta.remove(&can_theft_id); });

    let struct_id = 820_082i64;
    seed_item(&ctx, struct_id, 35832, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, struct_id, 35832, 1657, 65, char_id, orca_pos + DVec3::new(260.0, 0.0, 0.0)).await;
    let struct_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(struct_id)], &kw, None).await.unwrap_err();
    assert_eq!(struct_err.code, "CannotScoopStructure");

    // Scoop unowned container into cargo hold
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&can_theft_id).unwrap().owner_id = 0; });
    let scoop_res = ctx.dispatch_rpc(&mut sess, "ship", None, "Scoop", &[PyRep::Integer(can_theft_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(sc_kv) = scoop_res else { panic!("expected KeyVal") };
    assert_eq!(sc_kv.get("flagID"), Some(&PyRep::Integer(5)));

    // Foreign pilot cannot store a ship owned by someone else from space
    let foreign_space_ship = 820_050i64;
    seed_item(&ctx, foreign_space_ship, 587, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, foreign_space_ship, 587, 25, 6, foreign_char_id, orca_pos + DVec3::new(300.0, 0.0, 0.0)).await;
    let store_foreign_space = ctx.dispatch_rpc(&mut sess, "ship", None, "StoreVessel", &[PyRep::Integer(foreign_space_ship), PyRep::Integer(orca_id)], &kw, None).await.unwrap_err();
    assert_eq!(store_foreign_space.code, "OwnershipViolation");

    // StoreVessel in space
    let live_frigate_id = 820_007i64;
    seed_item(&ctx, live_frigate_id, 587, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, live_frigate_id, 587, 25, 6, foreign_char_id, orca_pos + DVec3::new(1000.0, 0.0, 0.0)).await;

    sess_foreign.station_id = None;
    sess_foreign.structure_id = None;
    sess_foreign.solar_system_id = Some(sys_id);
    sess_foreign.ship_id = Some(live_frigate_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&foreign_char_id) {
            ch.station_id = None; ch.structure_id = None; ch.solar_system_id = sys_id; ch.active_ship_id = live_frigate_id;
        }
    });

    let store_space = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "StoreVessel", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ssp_kv) = store_space else { panic!("expected KeyVal") };
    let cap_id = ssp_kv.get("capsuleID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ssp_kv.get("storedShipID").and_then(|v| v.as_i64()), Some(live_frigate_id));
    assert_eq!(ssp_kv.get("hostShipID").and_then(|v| v.as_i64()), Some(orca_id));
    assert_eq!(ssp_kv.get("flagID").and_then(|v| v.as_i64()), Some(90));
    assert_eq!(sess_foreign.ship_id, Some(cap_id));
    assert!(sys.get_ball_snapshot(live_frigate_id).await.unwrap().is_none());
    assert!(sys.get_ball_snapshot(cap_id).await.unwrap().is_some());

    // Second frigate stored: capsule reuse on StoreVessel
    let second_frigate_id = 820_008i64;
    seed_item(&ctx, second_frigate_id, 587, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_space_item(&ctx, sys_id, second_frigate_id, 587, 25, 6, foreign_char_id, orca_pos + DVec3::new(600.0, 0.0, 0.0)).await;
    sess_foreign.ship_id = Some(second_frigate_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&foreign_char_id) { ch.active_ship_id = second_frigate_id; }
    });
    let store_second = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "StoreVessel", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ssp2_kv) = store_second else { panic!("expected KeyVal") };
    let cap_id2 = ssp2_kv.get("capsuleID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(cap_id2, cap_id, "StoreVessel must reuse character's existing capsule");

    // Foreign-corp POS forcefield gating on BoardStoredShip and StoreVessel
    ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::String("wrong-pass".into())], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|s| {
        if let Some(t) = s.items.get_mut(&tower_id) { t.location_id = sys_id as i64; t.type_id = 16213; t.singleton = true; }
        s.item_positions.insert(tower_id, [orca_pos.x + 200.0, orca_pos.y, orca_pos.z]);
        if let Some(st) = s.starbase_settings.get_mut(&tower_id) {
            st.password = Some("pos-hostile".into());
            st.allow_corporation_members = false;
            st.use_alliance_standings = true;
            st.attack_standing_threshold = Some(0.0);
        }
    });

    // R-PW-02: Stranger with use_alliance_standings=true but wrong password is denied forcefield entry
    assert!(!can_enter_forcefield(&ctx, foreign_char_id, second_frigate_id, tower_id));

    // BoardStoredShip fails due to forcefield password mismatch
    let ff_board_err = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "BoardStoredShip", &[PyRep::Integer(orca_id), PyRep::Integer(second_frigate_id)], &kw, None).await.unwrap_err();
    assert_eq!(ff_board_err.code, "ForcefieldAccessDenied");

    // BoardStoredShip: foreign pilot cannot board Orca owner's stored ship
    let theft_err = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "BoardStoredShip", &[PyRep::Integer(orca_id), PyRep::Integer(spare_rifter_id)], &kw, None).await.unwrap_err();
    assert_eq!(theft_err.code, "OwnershipViolation");

    // Setting matching ship password grants forcefield access to board
    ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::String("pos-hostile".into())], &kw, None).await.unwrap();

    let board_second = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "BoardStoredShip", &[PyRep::Integer(orca_id), PyRep::Integer(second_frigate_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(bsma2_kv) = board_second else { panic!("expected KeyVal") };
    assert_eq!(bsma2_kv.get("shipID").and_then(|v| v.as_i64()), Some(second_frigate_id));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&second_frigate_id).map(|i| i.owner_id)), Some(foreign_char_id));
    assert_eq!(sess_foreign.ship_id, Some(second_frigate_id));
    assert!(sys.get_ball_snapshot(cap_id).await.unwrap().is_none(), "capsule consumed on boarding stored ship in space");

    let bp = sys.ballpark.read().unwrap();
    let vacated_char = bp.ball_slim_meta.get(&live_frigate_id).and_then(|m| m.char_id);
    assert_eq!(vacated_char, None, "vacated ship slim item char_id must be None");
    drop(bp);

    // Foreign pilot now in second_frigate_id. Test StoreVessel against forcefield:
    ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::String("wrong-pass".into())], &kw, None).await.unwrap();
    let ff_store_err = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "StoreVessel", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap_err();
    assert_eq!(ff_store_err.code, "ForcefieldAccessDenied");

    // Setting matching password allows StoreVessel
    ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "SetShipPassword", &[PyRep::String("pos-hostile".into())], &kw, None).await.unwrap();
    let ff_store_ok = ctx.dispatch_rpc(&mut sess_foreign, "ship", None, "StoreVessel", &[PyRep::Integer(orca_id)], &kw, None).await.unwrap();
    let PyRep::KeyVal(ff_s_kv) = ff_store_ok else { panic!("expected KeyVal") };
    assert_eq!(ff_s_kv.get("storedShipID").and_then(|v| v.as_i64()), Some(second_frigate_id));
}

#[tokio::test]
async fn test_r2_track4_corp_hangars_structures_and_market() {
    let ctx = new_test_ctx();
    let mut sess = SessionState::new(8901);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let foreign_char_id = 90_000_002i64;
    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    let corp_id = sess.corporation_id.unwrap_or(98_000_001);
    sess.corp_role = u64::MAX;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, u64::MAX);
        }
        s.corp_role_assignments.insert(
            (corp_id, char_id),
            eve_db::snapshot_ext::CorpMemberRoleAssignment {
                character_id: char_id,
                roles: u64::MAX,
                roles_at_hq: u64::MAX,
                roles_at_base: u64::MAX,
                roles_at_other: u64::MAX,
                ..Default::default()
            },
        );
    });

    // 1. Issue #86: Splitting a stack from CorpSAG1 preserves corp_id on the residual stack
    let corp_stack_id = 890_001i64;
    seed_item(&ctx, corp_stack_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 100, false);
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(40));
    let _ = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(corp_stack_id), PyRep::Integer(station_id), PyRep::Integer(4)], &split_kw, None).await.unwrap();
    let (res_owner, res_qty) = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&corp_stack_id).unwrap();
        (it.owner_id, it.quantity)
    });
    assert_eq!(res_owner, corp_id, "Residual stack in CorpSAG1 must retain corporation owner_id");
    assert_eq!(res_qty, 60);

    // 2. Issue #97: GetBlueprintDataByOwner & GetJobsByOwner reject foreign owner_id
    let bp_err = ctx.dispatch_rpc(&mut sess, "blueprintManager", None, "GetBlueprintDataByOwner", &[PyRep::Integer(foreign_char_id)], &kw, None).await.unwrap_err();
    assert_eq!(bp_err.code, "OwnerAccessDenied");
    let job_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "GetJobsByOwner", &[PyRep::Integer(foreign_char_id)], &kw, None).await.unwrap_err();
    assert_eq!(job_err.code, "OwnerAccessDenied");

    // 3. Issue #82: LootAll rejects foreign Mobile Depot (group 1246, type 33474)
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    spawn_space_item(&ctx, sys_id, ship_id, 587, 25, 6, char_id, DVec3::ZERO).await;
    let foreign_depot_id = 890_002i64;
    seed_item(&ctx, foreign_depot_id, 33474, foreign_char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, 890_003i64, 34, foreign_char_id, foreign_depot_id, InventoryFlag::Cargo, 10, false);
    spawn_space_item(&ctx, sys_id, foreign_depot_id, 33474, 1246, 22, foreign_char_id, DVec3::new(500.0, 0.0, 0.0)).await;
    let loot_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(foreign_depot_id)], &kw, None).await.unwrap_err();
    assert_eq!(loot_err.code, "AccessDenied");
}

#[tokio::test]
async fn test_r2_track5_economy_market_and_trading() {
    let ctx = new_test_ctx();
    let mut sess = SessionState::new(8951);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

    // 1. Issue #96: GetCorporationOrders rejects foreign corp_id
    let foreign_corp_err = ctx.dispatch_rpc(&mut sess, "marketProxy", None, "GetCorporationOrders", &[PyRep::Integer(98_999_999)], &kw, None).await.unwrap_err();
    assert_eq!(foreign_corp_err.code, "AccessDenied");

    // 2. Issue #99: WithdrawPlexFromVault rejects i32::MAX + 10 overflow BEFORE debiting vault
    ctx.db.with_mut_snapshot(|s| {
        s.adjust_plex_vault(char_id, 5_000_000_000);
    });
    let bal_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id));
    let overflow_qty = (i32::MAX as i64) + 10;
    let plex_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "WithdrawPlexFromVault", &[PyRep::Integer(overflow_qty), PyRep::Integer(station_id), PyRep::Integer(4)], &kw, None).await.unwrap_err();
    assert_eq!(plex_err.code, "InvalidArgument");
    let bal_after = ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id));
    assert_eq!(bal_before, bal_after, "PLEX vault must not be debited on i32 overflow");

    // Also reject invalid fitted flag (e.g. LoSlot0 = 11)
    let flag_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "WithdrawPlexFromVault", &[PyRep::Integer(10), PyRep::Integer(station_id), PyRep::Integer(11)], &kw, None).await.unwrap_err();
    assert_eq!(flag_err.code, "InvalidFlag");

    // 3. Issue #100: Direct trade rejects OfferMoney / MakeOffer if a participant undocks
    let char_b = 90_000_002i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(cb) = s.characters.get_mut(&char_b) {
            cb.station_id = Some(station_id);
        }
    });
    let init_res = ctx.dispatch_rpc(&mut sess, "trademgr", None, "InitiateTrade", &[PyRep::Integer(char_b)], &kw, None).await.unwrap();
    let trade_id = match init_res {
        PyRep::KeyVal(kv) => kv.get("tradeContainerID").or_else(|| kv.get("tradeID")).and_then(|v| v.as_i64()).unwrap_or(0),
        PyRep::Integer(id) => id,
        _ => 0,
    };
    ctx.db.with_mut_snapshot(|s| {
        if let Some(cb) = s.characters.get_mut(&char_b) {
            cb.station_id = None;
            cb.structure_id = None;
        }
    });
    let trade_err = ctx.dispatch_rpc(&mut sess, "trademgr", None, "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(1000.0)], &kw, None).await.unwrap_err();
    assert_eq!(trade_err.code, "NotDockedAtStation");
}

#[tokio::test]
async fn test_r2_track6_quasar_esi_and_protocol() {
    let ctx = new_test_ctx();
    let mut sess = SessionState::new(8991);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

    // 1. Issue #101: LSC SendMessage rejects spoofing a foreign solar system or corporation channel
    let chat_sys_err = ctx.dispatch_rpc(
        &mut sess,
        "LSC",
        None,
        "SendMessage",
        &[PyRep::String("solarsystemid2:30002187".into()), PyRep::String("spoofed".into())],
        &kw,
        None,
    ).await.unwrap_err();
    assert_eq!(chat_sys_err.code, "ChannelAccessDenied");

    let chat_corp_err = ctx.dispatch_rpc(
        &mut sess,
        "LSC",
        None,
        "SendMessage",
        &[PyRep::String("corpid:98999999".into()), PyRep::String("spoofed".into())],
        &kw,
        None,
    ).await.unwrap_err();
    assert_eq!(chat_corp_err.code, "ChannelAccessDenied");

    // 2. Issue #101: mailMgr.SendMail rejects sending mass mail to a foreign corporation
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.insert(
            98_999_999,
            eve_db::CorporationRecord {
                corporation_id: 98_999_999,
                name: "Foreign Corp".into(),
                ticker: "FRGN".into(),
                ceo_id: 90_000_002,
                members: [90_000_002].into_iter().collect(),
                ..Default::default()
            },
        );
    });
    let mail_err = ctx.dispatch_rpc(
        &mut sess,
        "mailMgr",
        None,
        "SendMail",
        &[
            PyRep::List(Vec::new()),
            PyRep::None,
            PyRep::Integer(98_999_999),
            PyRep::String("Spam".into()),
            PyRep::String("Body".into()),
        ],
        &kw,
        None,
    ).await.unwrap_err();
    assert_eq!(mail_err.code, "CorpMailAccessDenied");

    // 3. Issue #109: shipCosmeticsMgr.GetEnabledCosmetics returns persisted ship_logos from quasar_cosmetics_live
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    ctx.db.with_mut_snapshot(|s| {
        let mut live = eve_server::services::quasar_cosmetics_live::CosmeticsLiveData::default();
        let mut slots = std::collections::BTreeMap::new();
        slots.insert(0, 2u8);
        live.ship_logos.insert(ship_id as u64, slots);
        s.quasar_state_blobs.insert("quasar_cosmetics_live".into(), serde_json::to_string(&live).unwrap());
    });
    let enabled_res = ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "GetEnabledCosmetics",
        &[PyRep::Integer(ship_id)],
        &kw,
        None,
    ).await.unwrap();
    let PyRep::Dict(entries) = enabled_res else { panic!("expected Dict") };
    assert_eq!(entries.len(), 1);
}

}
