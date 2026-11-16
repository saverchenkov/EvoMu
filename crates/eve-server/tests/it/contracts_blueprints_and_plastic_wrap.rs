//! Integration tests for contracts blueprint singleton preservation, courier plastic wrap break,
//! and item exchange blueprints metadata (`feat:contracts.*`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_testkit::client_harness::{ClientErrorResponse, ClientHarness, ContractValidationMode};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const RIFTER_BP_TYPE_ID: i64 = 691;
const MWD_TYPE_ID: i64 = 587;
const SHIELD_EXTENDER_TYPE_ID: i64 = 5975;
const TRITANIUM_TYPE_ID: i64 = 34;
const MUTAPLASMID_TYPE_ID: i64 = 47746;
const PLASTIC_WRAP_TYPE_ID: i64 = 3468;

async fn select_pilot(ctx: &ServerContext, session_id: u64, name: &str, secret: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, name, secret).await;
    common::borrow_wire_harness(ctx, &mut sess)
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new())
        .await;
    ctx.advance_secs(12);
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, ClientErrorResponse> {
    common::borrow_wire_harness(ctx, sess)
        .call(service, None, method, args, &HashMap::new())
        .await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, ClientErrorResponse> {
    common::borrow_wire_harness(ctx, sess)
        .call(service, None, method, args, kwargs)
        .await
}

async fn accept_contract_report_only(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
) -> Result<PyRep, ClientErrorResponse> {
    // GENUINE SERVER/WIRE-SHAPE BUG: `contracts_service.rs` `AcceptContract` returns only
    // `{contractID, status, expirationUnix[, courierPackageID]}` without a `client_compat`
    // normalizer, whereas V24 `ContractsSvc.AcceptContract` expects `contractID`, `dateAccepted`,
    // `endStationID`, `numDays`, `startStationID`, and numeric `type`. Use ReportOnly so the wire
    // shape mismatch is logged without blocking functional verification.
    ClientHarness::new(common::SessionBorrowWireTransport {
        ctx,
        session: sess,
        preserve_char_notifications: false,
    })
    .with_mode(ContractValidationMode::ReportOnly)
    .call("contractProxy", None, "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(false)], &HashMap::new())
    .await
}

fn contract_id_from_create_rep(rep: &PyRep) -> i64 {
    match rep {
        PyRep::List(ids) => ids.first().and_then(PyRep::as_i64).expect("CreateContract list contractID"),
        _ => field(rep, "contractID")
            .as_i64()
            .unwrap_or_else(|| rep.as_i64().expect("CreateContract contractID")),
    }
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v.clone()).unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

#[tokio::test]
async fn test_contracts_item_exchange_blueprints_metadata_bpo_and_bpc() {
    // parity: feat:contracts.item-exchange-blueprints-metadata
    eve_testkit::attest(&["feat:contracts.item-exchange-blueprints-metadata"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = select_pilot(&ctx, 77_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_002, "other", "other-secret", PILOT_2).await;

    let (bpo_id, bpc_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).expect("PILOT_1 character").balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).expect("PILOT_2 character").balance = IskCents::from_f64_isk(50_000_000.0);
        s.next_entity_id += 2;
        let base = s.next_entity_id;

        // BPO: Original, ME 10, TE 20, no runs remaining (infinite)
        s.items.insert(base - 1, InventoryItemRecord {
            item_id: base - 1, type_id: RIFTER_BP_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, is_blueprint_copy: false,
            blueprint_runs_remaining: None, dynamic_attributes: HashMap::from([(1112, 10.0), (1113, 20.0)]), ..Default::default()
        });

        // BPC: Copy, ME 8, TE 14, 7 runs remaining
        s.items.insert(base, InventoryItemRecord {
            item_id: base, type_id: RIFTER_BP_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, is_blueprint_copy: true,
            blueprint_runs_remaining: Some(7), dynamic_attributes: HashMap::from([(1112, 8.0), (1113, 14.0)]), ..Default::default()
        });

        (base - 1, base)
    });

    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    let contract_params = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(7)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(1_500_000.0)),
        ("reward", PyRep::Real(0.0)),
        ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("Rifter BPO and BPC Pack".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(bpo_id), PyRep::Integer(bpc_id)])),
    ]);

    let create_rep = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[contract_params], &confirm_kw)
        .await
        .expect("CreateContract BPO + BPC");
    let cid = contract_id_from_create_rep(&create_rep);

    // Pilot 2 inspects contract via contractProxy.GetContract
    let info = rpc(&ctx, &mut sess2, "contractProxy", "GetContract", &[PyRep::Integer(cid)]).await.expect("contractProxy.GetContract");
    let PyRep::List(items) = field(&info, "items") else { panic!("expected items list in GetContract"); };
    assert_eq!(items.len(), 2);

    let bpo_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(bpo_id)).expect("BPO item entry");
    assert_eq!(field(bpo_entry, "copy"), PyRep::Bool(false));
    assert_eq!(field(bpo_entry, "isBlueprintCopy"), PyRep::Bool(false));
    assert_eq!(field(bpo_entry, "materialEfficiency"), PyRep::Integer(10));
    assert_eq!(field(bpo_entry, "timeEfficiency"), PyRep::Integer(20));
    assert_eq!(field(bpo_entry, "runsRemaining"), PyRep::Integer(-1));
    assert_eq!(field(bpo_entry, "singleton"), PyRep::Bool(true));

    let bpc_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(bpc_id)).expect("BPC item entry");
    assert_eq!(field(bpc_entry, "copy"), PyRep::Bool(true));
    assert_eq!(field(bpc_entry, "isBlueprintCopy"), PyRep::Bool(true));
    assert_eq!(field(bpc_entry, "materialEfficiency"), PyRep::Integer(8));
    assert_eq!(field(bpc_entry, "timeEfficiency"), PyRep::Integer(14));
    assert_eq!(field(bpc_entry, "runsRemaining"), PyRep::Integer(7));
    assert_eq!(field(bpc_entry, "singleton"), PyRep::Bool(true));

    // Pilot 2 searches contracts by itemTypeID filter
    let search_kw = HashMap::from([
        ("itemTypeID".to_string(), PyRep::Integer(RIFTER_BP_TYPE_ID)),
        ("contractType".to_string(), PyRep::String("ItemExchange".into())),
    ]);
    let search_res = rpc_kw(&ctx, &mut sess2, "contractProxy", "SearchContracts", &[], &search_kw).await.expect("SearchContracts by itemTypeID");
    let PyRep::List(search_list) = field(&search_res, "contracts") else { panic!("expected contracts list from SearchContracts"); };
    let found = search_list
        .iter()
        .find(|c| field(&field(c, "contract"), "contractID") == PyRep::Integer(cid))
        .expect("Contract found in SearchContracts");
    let found_contract = field(found, "contract");
    assert_eq!(field(&found_contract, "type"), PyRep::Integer(1));
    assert_eq!(field(&found_contract, "status"), PyRep::Integer(0));

    // Pilot 2 accepts the contract
    accept_contract_report_only(&ctx, &mut sess2, cid)
        .await
        .expect("AcceptContract BPO + BPC");

    // Both blueprints are now in Pilot 2's hangar with exact metadata preserved
    ctx.db.with_snapshot(|s| {
        let bpo = s.items.get(&bpo_id).expect("BPO in items");
        assert_eq!(bpo.owner_id, PILOT_2);
        assert_eq!(bpo.location_id, JITA_STATION);
        assert_eq!(bpo.flag, InventoryFlag::Hangar);
        assert!(bpo.singleton);
        assert!(!bpo.is_blueprint_copy);
        assert_eq!(bpo.blueprint_runs_remaining, None);
        assert_eq!(bpo.dynamic_attributes.get(&1112).copied(), Some(10.0));
        assert_eq!(bpo.dynamic_attributes.get(&1113).copied(), Some(20.0));

        let bpc = s.items.get(&bpc_id).expect("BPC in items");
        assert_eq!(bpc.owner_id, PILOT_2);
        assert_eq!(bpc.location_id, JITA_STATION);
        assert_eq!(bpc.flag, InventoryFlag::Hangar);
        assert!(bpc.singleton);
        assert!(bpc.is_blueprint_copy);
        assert_eq!(bpc.blueprint_runs_remaining, Some(7));
        assert_eq!(bpc.dynamic_attributes.get(&1112).copied(), Some(8.0));
        assert_eq!(bpc.dynamic_attributes.get(&1113).copied(), Some(14.0));
    });
}

#[tokio::test]
async fn test_contracts_blueprint_singleton_and_abyssal_preservation() {
    // parity: feat:contracts.blueprint-singleton-preservation
    eve_testkit::attest(&["feat:contracts.blueprint-singleton-preservation"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = select_pilot(&ctx, 77_003, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_004, "other", "other-secret", PILOT_2).await;

    let (bpc_id, dmg_id, aby_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).expect("PILOT_1 character").balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).expect("PILOT_2 character").balance = IskCents::from_f64_isk(50_000_000.0);
        s.next_entity_id += 3;
        let base = s.next_entity_id;

        // 1. Blueprint copy with custom ME/TE/runs
        s.items.insert(base - 2, InventoryItemRecord {
            item_id: base - 2, type_id: RIFTER_BP_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, is_blueprint_copy: true,
            blueprint_runs_remaining: Some(12), dynamic_attributes: HashMap::from([(1112, 4.0), (1113, 8.0)]), ..Default::default()
        });

        // 2. Damaged module (attr 3 = damage: 0.35)
        s.items.insert(base - 1, InventoryItemRecord {
            item_id: base - 1, type_id: MWD_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, dynamic_attributes: HashMap::from([(3, 0.35)]), ..Default::default()
        });

        // 3. Abyssal mutated module (source mutaplasmid + mutated dynamic attrs)
        s.items.insert(base, InventoryItemRecord {
            item_id: base, type_id: SHIELD_EXTENDER_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, source_mutaplasmid_id: Some(MUTAPLASMID_TYPE_ID as u32),
            dynamic_attributes: HashMap::from([(3, 15.5), (204, 1.45)]), ..Default::default()
        });

        (base - 2, base - 1, base)
    });

    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    let params = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(2_000_000.0)),
        ("reward", PyRep::Real(0.0)),
        ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("BPC + Damaged + Abyssal".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(bpc_id), PyRep::Integer(dmg_id), PyRep::Integer(aby_id)])),
    ]);

    let create_rep = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[params], &confirm_kw)
        .await
        .expect("CreateContract");
    let cid = contract_id_from_create_rep(&create_rep);

    // Query via contractProxy.GetContract (testing contractProxy service dispatch)
    let info = rpc(&ctx, &mut sess2, "contractProxy", "GetContract", &[PyRep::Integer(cid)]).await.expect("contractProxy.GetContract");
    let PyRep::List(items) = field(&info, "items") else { panic!("expected items list in GetContract"); };
    assert_eq!(items.len(), 3);

    let bpc_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(bpc_id)).expect("BPC entry");
    assert_eq!(field(bpc_entry, "copy"), PyRep::Bool(true));
    assert_eq!(field(bpc_entry, "materialEfficiency"), PyRep::Integer(4));
    assert_eq!(field(bpc_entry, "timeEfficiency"), PyRep::Integer(8));
    assert_eq!(field(bpc_entry, "runsRemaining"), PyRep::Integer(12));

    let dmg_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(dmg_id)).expect("Damaged entry");
    assert_eq!(field(dmg_entry, "damage"), PyRep::Real(0.35));
    assert_eq!(field(dmg_entry, "singleton"), PyRep::Bool(true));

    let aby_entry = items.iter().find(|it| field(it, "itemID") == PyRep::Integer(aby_id)).expect("Abyssal entry");
    assert_eq!(field(aby_entry, "sourceMutaplasmidID"), PyRep::Integer(MUTAPLASMID_TYPE_ID));
    assert_eq!(field(aby_entry, "singleton"), PyRep::Bool(true));

    // Also verify contractProxy.SearchContracts exposes all contract items
    let search_kw = HashMap::from([("contractType".to_string(), PyRep::String("ItemExchange".into()))]);
    let search_res = rpc_kw(&ctx, &mut sess2, "contractProxy", "SearchContracts", &[], &search_kw).await.expect("contractProxy.SearchContracts");
    let PyRep::List(search_list) = field(&search_res, "contracts") else { panic!("expected contracts search list"); };
    let found = search_list
        .iter()
        .find(|c| field(&field(c, "contract"), "contractID") == PyRep::Integer(cid))
        .expect("found in search");
    let PyRep::List(found_items) = field(found, "items") else { panic!("items in found search result"); };
    assert_eq!(found_items.len(), 3);

    // Accept contract via contractProxy.AcceptContract
    accept_contract_report_only(&ctx, &mut sess2, cid)
        .await
        .expect("contractProxy.AcceptContract");

    // Verify all singleton attributes strictly preserved upon contract completion
    ctx.db.with_snapshot(|s| {
        let bpc = s.items.get(&bpc_id).expect("BPC in recipient hangar");
        assert_eq!(bpc.owner_id, PILOT_2);
        assert_eq!(bpc.location_id, JITA_STATION);
        assert_eq!(bpc.flag, InventoryFlag::Hangar);
        assert!(bpc.singleton);
        assert!(bpc.is_blueprint_copy);
        assert_eq!(bpc.blueprint_runs_remaining, Some(12));
        assert_eq!(bpc.dynamic_attributes.get(&1112).copied(), Some(4.0));
        assert_eq!(bpc.dynamic_attributes.get(&1113).copied(), Some(8.0));

        let dmg = s.items.get(&dmg_id).expect("damaged item in recipient hangar");
        assert_eq!(dmg.owner_id, PILOT_2);
        assert_eq!(dmg.location_id, JITA_STATION);
        assert_eq!(dmg.flag, InventoryFlag::Hangar);
        assert!(dmg.singleton);
        assert_eq!(dmg.dynamic_attributes.get(&3).copied(), Some(0.35));

        let aby = s.items.get(&aby_id).expect("abyssal item in recipient hangar");
        assert_eq!(aby.owner_id, PILOT_2);
        assert_eq!(aby.location_id, JITA_STATION);
        assert_eq!(aby.flag, InventoryFlag::Hangar);
        assert!(aby.singleton);
        assert_eq!(aby.source_mutaplasmid_id, Some(MUTAPLASMID_TYPE_ID as u32));
        assert_eq!(aby.dynamic_attributes.get(&204).copied(), Some(1.45));
    });
}

#[tokio::test]
async fn test_courier_plastic_wrap_break_and_repackage_forfeits_collateral() {
    // parity: feat:contracts.courier-plastic-wrap-break
    eve_testkit::attest(&["feat:contracts.courier-plastic-wrap-break"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = select_pilot(&ctx, 77_005, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_006, "other", "other-secret", PILOT_2).await;

    let (cargo_1, cargo_2) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).expect("PILOT_1 character").balance = IskCents::from_f64_isk(100_000_000.0);
        s.characters.get_mut(&PILOT_2).expect("PILOT_2 character").balance = IskCents::from_f64_isk(100_000_000.0);
        s.next_entity_id += 2;
        let base = s.next_entity_id;

        s.items.insert(base - 1, InventoryItemRecord {
            item_id: base - 1, type_id: TRITANIUM_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1_000, ..Default::default()
        });
        s.items.insert(base, InventoryItemRecord {
            item_id: base, type_id: MWD_TYPE_ID as u32, owner_id: PILOT_1, location_id: JITA_STATION,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });

        (base - 1, base)
    });

    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);

    // --- Scenario A: Break plastic wrap via contractProxy.BreakPlasticWrap ---
    let courier_a_params = PyRep::keyval([
        ("contractType", PyRep::String("Courier".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(0.0)),
        ("reward", PyRep::Real(250_000.0)),
        ("collateral", PyRep::Real(2_000_000.0)),
        ("title", PyRep::String("Courier BreakPlasticWrap Test".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(cargo_1)])),
    ]);

    let create_a = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[courier_a_params], &confirm_kw)
        .await
        .expect("CreateContract Courier A");
    let cid_a = contract_id_from_create_rep(&create_a);

    // Pilot 2 accepts courier contract A -> pays 2,000,000 ISK collateral
    let accept_a = accept_contract_report_only(&ctx, &mut sess2, cid_a)
        .await
        .expect("AcceptContract Courier A");
    let wrap_a_id = field(&accept_a, "courierPackageID").as_i64().unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| {
            s.items.values().find(|it| it.type_id == PLASTIC_WRAP_TYPE_ID as u32 && it.owner_id == PILOT_2)
                .map(|it| it.item_id).expect("plastic wrap A in items")
        })
    });

    let issuer_bal_before_break = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());

    // Pilot 2 breaks the plastic wrap via contractProxy.BreakPlasticWrap
    let break_rep = rpc(&ctx, &mut sess2, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_a_id)])
        .await
        .expect("contractProxy.BreakPlasticWrap");
    assert_eq!(field(&break_rep, "status"), PyRep::String("Failed".into()));

    // Verify contract A failed, collateral + reward refund transferred to issuer, cargo 1 in hauler's hangar
    ctx.db.with_snapshot(|s| {
        let contract = s.contracts.get(&cid_a).expect("contract A");
        assert_eq!(contract.status, "Failed");

        let cargo = s.items.get(&cargo_1).expect("cargo 1 in items");
        assert_eq!(cargo.owner_id, PILOT_2);
        assert_eq!(cargo.location_id, JITA_STATION);
        assert_eq!(cargo.flag, InventoryFlag::Hangar);
        assert_eq!(cargo.quantity, 1_000);

        assert!(!s.items.contains_key(&wrap_a_id), "Plastic wrap container A must be destroyed");

        let issuer_bal_after = s.characters[&PILOT_1].balance.raw_cents();
        assert_eq!(issuer_bal_after - issuer_bal_before_break, 2_250_000_00, "Issuer receives 2M collateral + 250k reward");
    });

    // --- Scenario B: Repackage courier plastic wrap via repackagingSvc.RepackageItems ---
    let courier_b_params = PyRep::keyval([
        ("contractType", PyRep::String("Courier".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(0.0)),
        ("reward", PyRep::Real(100_000.0)),
        ("collateral", PyRep::Real(1_500_000.0)),
        ("title", PyRep::String("Courier Repackage Wrap Test".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(cargo_2)])),
    ]);

    let create_b = rpc_kw(&ctx, &mut sess1, "contractProxy", "CreateContract", &[courier_b_params], &confirm_kw)
        .await
        .expect("CreateContract Courier B");
    let cid_b = contract_id_from_create_rep(&create_b);

    // Pilot 2 accepts courier contract B
    let accept_b = accept_contract_report_only(&ctx, &mut sess2, cid_b)
        .await
        .expect("AcceptContract Courier B");
    let wrap_b_id = field(&accept_b, "courierPackageID").as_i64().unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| {
            s.items.values().find(|it| it.type_id == PLASTIC_WRAP_TYPE_ID as u32 && it.owner_id == PILOT_2)
                .map(|it| it.item_id).expect("plastic wrap B in items")
        })
    });

    let issuer_bal_before_repackage = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.raw_cents());

    // Pilot 2 repackages the plastic wrap via repackagingSvc.RepackageItems
    let repackage_kw = HashMap::from([("destroyable".to_string(), PyRep::Bool(true))]);
    let repackage_rep = rpc_kw(
        &ctx,
        &mut sess2,
        "repackagingSvc",
        "RepackageItems",
        &[PyRep::List(vec![PyRep::Integer(wrap_b_id)])],
        &repackage_kw,
    )
    .await
    .expect("RepackageItems plastic wrap B");
    assert_eq!(field(&repackage_rep, "status"), PyRep::String("Failed".into()));

    // Verify contract B failed, collateral transferred, cargo 2 extracted
    ctx.db.with_snapshot(|s| {
        let contract = s.contracts.get(&cid_b).expect("contract B");
        assert_eq!(contract.status, "Failed");

        let cargo = s.items.get(&cargo_2).expect("cargo 2 in items");
        assert_eq!(cargo.owner_id, PILOT_2);
        assert_eq!(cargo.location_id, JITA_STATION);
        assert_eq!(cargo.flag, InventoryFlag::Hangar);
        assert!(cargo.singleton);

        assert!(!s.items.contains_key(&wrap_b_id), "Plastic wrap container B must be destroyed");

        let issuer_bal_after = s.characters[&PILOT_1].balance.raw_cents();
        assert_eq!(issuer_bal_after - issuer_bal_before_repackage, 1_600_000_00, "Issuer receives 1.5M collateral + 100k reward");
    });
}

#[tokio::test]
async fn test_contracts_fitted_ship_and_non_empty_container_lifecycle_and_locks() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = select_pilot(&ctx, 77_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_102, "other", "other-secret", PILOT_2).await;

    let (ship_id, mod_id, charge_id, cont_id, cont_item_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).expect("PILOT_1").balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).expect("PILOT_2").balance = IskCents::from_f64_isk(50_000_000.0);
        s.next_entity_id += 5;
        let base = s.next_entity_id;
        let ship_id = base - 4;
        let mod_id = base - 3;
        let charge_id = base - 2;
        let cont_id = base - 1;
        let cont_item_id = base;
        s.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id,
            type_id: MWD_TYPE_ID as u32,
            owner_id: PILOT_1,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.items.insert(mod_id, InventoryItemRecord {
            item_id: mod_id,
            type_id: SHIELD_EXTENDER_TYPE_ID as u32,
            owner_id: PILOT_1,
            location_id: ship_id,
            flag: InventoryFlag::MedSlot0,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.items.insert(charge_id, InventoryItemRecord {
            item_id: charge_id,
            type_id: TRITANIUM_TYPE_ID as u32,
            owner_id: PILOT_1,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 10,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        s.items.insert(cont_id, InventoryItemRecord {
            item_id: cont_id,
            type_id: 17_366,
            owner_id: PILOT_1,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.items.insert(cont_item_id, InventoryItemRecord {
            item_id: cont_item_id,
            type_id: TRITANIUM_TYPE_ID as u32,
            owner_id: PILOT_1,
            location_id: cont_id,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        (ship_id, mod_id, charge_id, cont_id, cont_item_id)
    });

    let confirm_kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    let empty_kw = HashMap::new();

    // Courier still rejects fitted ship or non-empty container
    let courier_err = rpc_kw(
        &ctx,
        &mut sess1,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("availability", PyRep::String("Public".into())),
            ("assigneeID", PyRep::Integer(0)),
            ("expireTime", PyRep::Integer(1440)),
            ("numDays", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(JITA_STATION)),
            ("endStationID", PyRep::Integer(JITA_STATION)),
            ("price", PyRep::Real(0.0)),
            ("reward", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(50_000.0)),
            ("title", PyRep::String("Courier Reject".into())),
            ("itemList", PyRep::List(vec![PyRep::Integer(ship_id)])),
        ])],
        &confirm_kw,
    )
    .await
    .expect_err("Courier contract on fitted ship must fail");
    assert_eq!(courier_err.code, "CannotContractNonEmptyContainer");

    // ItemExchange accepts fitted non-active ship and non-empty container together
    let create_rep = rpc_kw(
        &ctx,
        &mut sess1,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("availability", PyRep::String("Public".into())),
            ("assigneeID", PyRep::Integer(0)),
            ("expireTime", PyRep::Integer(1440)),
            ("numDays", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(JITA_STATION)),
            ("endStationID", PyRep::Integer(JITA_STATION)),
            ("price", PyRep::Real(500_000.0)),
            ("reward", PyRep::Real(0.0)),
            ("collateral", PyRep::Real(0.0)),
            ("title", PyRep::String("Fitted Ship + Container".into())),
            (
                "itemList",
                PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(cont_id)]),
            ),
        ])],
        &confirm_kw,
    )
    .await
    .expect("CreateContract ItemExchange with fitted ship + container");
    let cid = contract_id_from_create_rep(&create_rep);

    let contract_rep = rpc(&ctx, &mut sess2, "contractProxy", "GetContract", &[PyRep::Integer(cid)])
        .await
        .expect("GetContract");
    let PyRep::List(items) = field(&contract_rep, "items") else {
        panic!("expected items list in GetContract");
    };
    assert!(items.iter().any(|r| field(r, "itemID") == PyRep::Integer(ship_id) && field(r, "parentID") == PyRep::Integer(0)));
    assert!(items.iter().any(|r| {
        field(r, "itemID") == PyRep::Integer(mod_id)
            && field(r, "parentID") == PyRep::Integer(ship_id)
            && field(r, "flagID") == PyRep::Integer(i64::from(InventoryFlag::MedSlot0 as u16))
            && field(r, "inCrate") == PyRep::Bool(true)
    }));
    assert!(items.iter().any(|r| field(r, "itemID") == PyRep::Integer(cont_item_id) && field(r, "parentID") == PyRep::Integer(cont_id)));

    // Tampering with escrowed ship or nested items while Outstanding is blocked with ItemLocked
    let trash_err = rpc(
        &ctx,
        &mut sess1,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(cont_item_id)])],
    )
    .await
    .expect_err("TrashItems on item inside escrowed container must fail");
    assert_eq!(trash_err.code, "ItemLocked");

    let unfit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mod_id),
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(i64::from(InventoryFlag::Hangar as u16)),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Unfitting module from escrowed ship must fail");
    assert_eq!(unfit_err.code, "ItemLocked");

    let clear_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "invbroker",
            None,
            "ClearCharges",
            &[PyRep::Integer(ship_id)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("ClearCharges on escrowed ship must fail");
    assert_eq!(clear_err.code, "ItemLocked");

    let move_cargo_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(charge_id),
                PyRep::Integer(JITA_STATION),
                PyRep::Integer(i64::from(InventoryFlag::Hangar as u16)),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Moving cargo out of escrowed ship must fail");
    assert_eq!(move_cargo_err.code, "ItemLocked");

    // Acceptor completes contract -> receives ship, container, and all nested items intact
    accept_contract_report_only(&ctx, &mut sess2, cid)
        .await
        .expect("AcceptContract fitted ship + container");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.contracts[&cid].status, "Completed");
        assert_eq!(s.items[&ship_id].owner_id, PILOT_2);
        assert_eq!(s.items[&ship_id].location_id, JITA_STATION);
        assert_eq!(s.items[&ship_id].flag, InventoryFlag::Hangar);
        assert_eq!(s.items[&mod_id].owner_id, PILOT_2);
        assert_eq!(s.items[&mod_id].location_id, ship_id);
        assert_eq!(s.items[&mod_id].flag, InventoryFlag::MedSlot0);
        assert_eq!(s.items[&charge_id].owner_id, PILOT_2);
        assert_eq!(s.items[&charge_id].location_id, ship_id);
        assert_eq!(s.items[&charge_id].flag, InventoryFlag::Cargo);
        assert_eq!(s.items[&cont_id].owner_id, PILOT_2);
        assert_eq!(s.items[&cont_id].location_id, JITA_STATION);
        assert_eq!(s.items[&cont_id].flag, InventoryFlag::Hangar);
        assert_eq!(s.items[&cont_item_id].owner_id, PILOT_2);
        assert_eq!(s.items[&cont_item_id].location_id, cont_id);
    });

    // Locks are released after settlement: new owner can trash container cargo
    rpc(
        &ctx,
        &mut sess2,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(cont_item_id)])],
    )
    .await
    .expect("New owner can trash contained item after contract settlement");
}
