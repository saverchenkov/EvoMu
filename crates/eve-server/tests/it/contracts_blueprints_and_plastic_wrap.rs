//! Integration tests for contracts blueprint singleton preservation, courier plastic wrap break,
//! and item exchange blueprints metadata (`feat:contracts.*`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

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
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None).await
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
    let mut sess1 = select_pilot(&ctx, 77_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_002, "other", "other-secret", PILOT_2).await;

    let (bpo_id, bpc_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
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
    let cid = field(&create_rep, "contractID").as_i64().unwrap_or_else(|| create_rep.as_i64().unwrap());

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
    let PyRep::List(search_list) = search_res else { panic!("expected list from SearchContracts"); };
    let found = search_list.iter().find(|c| field(c, "contractID") == PyRep::Integer(cid)).expect("Contract found in SearchContracts");
    assert_eq!(field(found, "type"), PyRep::String("ItemExchange".into()));
    assert_eq!(field(found, "status"), PyRep::String("Outstanding".into()));

    // Pilot 2 accepts the contract
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(false)])
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
    let mut sess1 = select_pilot(&ctx, 77_003, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_004, "other", "other-secret", PILOT_2).await;

    let (bpc_id, dmg_id, aby_id) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_f64_isk(50_000_000.0);
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
    let cid = field(&create_rep, "contractID").as_i64().unwrap_or_else(|| create_rep.as_i64().unwrap());

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
    let PyRep::List(search_list) = search_res else { panic!("expected search list"); };
    let found = search_list.iter().find(|c| field(c, "contractID") == PyRep::Integer(cid)).expect("found in search");
    let PyRep::List(found_items) = field(found, "items") else { panic!("items in found search result"); };
    assert_eq!(found_items.len(), 3);

    // Accept contract via contractProxy.AcceptContract
    rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(false)])
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
    let mut sess1 = select_pilot(&ctx, 77_005, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 77_006, "other", "other-secret", PILOT_2).await;

    let (cargo_1, cargo_2) = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_f64_isk(100_000_000.0);
        s.characters.get_mut(&PILOT_2).unwrap().balance = IskCents::from_f64_isk(100_000_000.0);
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
    let cid_a = field(&create_a, "contractID").as_i64().unwrap_or_else(|| create_a.as_i64().unwrap());

    // Pilot 2 accepts courier contract A -> pays 2,000,000 ISK collateral
    let accept_a = rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_a), PyRep::Bool(false)])
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
    let cid_b = field(&create_b, "contractID").as_i64().unwrap_or_else(|| create_b.as_i64().unwrap());

    // Pilot 2 accepts courier contract B
    let accept_b = rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_b), PyRep::Bool(false)])
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
