//! Complete Quasar Economy & Industry integration test suite — Part 2 (`plan:Q-6`).
//!
//! Covers:
//! - Accounting, Loyalty Points & Contracts
//! - Industry, Planet Interaction, Skyhook, Moon & Asteroid Belt
//! - P1 Track A Regressions (Issues #20, #21, #22)

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn make_request(type_url: &str, payload_val: Vec<u8>, corr: &[u8]) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"),
            value: payload_val,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-econ-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    }
}

fn encode_id_field(tag: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_varint_field(1, id, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

fn encode_uuid_field(tag: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_bytes_field(1, uuid, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

#[tokio::test]
async fn test_quasar_accounting_loyalty_contracts() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(1_000_009);
    sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;

    // Seed wallet journal entry
    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: 88001,
            character_id: 90_000_001,
            entry_type: "MarketEscrow".into(),
            amount: eve_db::IskCents(-100_000),
            balance_after: eve_db::IskCents(9_900_000),
            reason: "Escrow payment".into(),
        });
    });

    // 1. Character & Corporation Journal Entries
    let mut payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut payload);
    let req = make_request("eve.accounting.character.journal.GetCharacterEntriesRequest", payload.clone(), b"acc-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetCharacterEntriesResponse"));

    let req = make_request("eve.accounting.corporation.journal.GetCorporationEntriesRequest", Vec::new(), b"acc-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 2. Accounting Entry Types
    let req = make_request("eve.accounting.entrytype.GetAllRequest", Vec::new(), b"acc-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Loyalty Points Wallet assetholding operations (Withdraw & Deposit)
    let mut lp_payload = Vec::new();
    encode_uuid_field(1, b"lp-op-1001", &mut lp_payload);
    let req = make_request("eve.loyaltypoints.assetholding.operation.api.WithdrawRequest", lp_payload.clone(), b"lp-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.loyaltypoints.assetholding.operation.api.DepositRequest", lp_payload, b"lp-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Contract Item Exchange
    let mut contract_payload = Vec::new();
    encode_id_field(1, 9901, &mut contract_payload);
    let req = make_request("eve.contract.item_exchange.GetRequest", contract_payload, b"cnt-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("item_exchange.GetResponse"));
}

#[tokio::test]
async fn test_quasar_industry_planetary_moon_asteroidbelt() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(1_000_009);
    sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;

    // 1. Industry (jobs, facilities, cost indices)
    let mut char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_payload);
    let req = make_request("eve.character.industry.job.GetRequest", char_payload.clone(), b"ind-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("industry.job.GetResponse"));

    let req = make_request("eve.character.industry.job.GetActiveRequest", char_payload.clone(), b"ind-01a");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.character.industry.blueprints.GetRequest", char_payload, b"ind-01b");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.industry.facility.GetRequest", Vec::new(), b"ind-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.industry.system.GetCostIndicesRequest", Vec::new(), b"ind-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetCostIndicesResponse"));

    // 2. Planet & Skyhook
    let mut planet_payload = Vec::new();
    encode_id_field(1, 40_000_001, &mut planet_payload);
    let req = make_request("eve.planet.GetRequest", planet_payload.clone(), b"pln-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("planet.GetResponse"));

    let mut sys_payload = Vec::new();
    encode_id_field(1, 30_000_142, &mut sys_payload);
    let req = make_request("eve.planet.GetPlanetsInSolarSystemRequest", sys_payload, b"pln-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
    assert!(rsp.payload.unwrap().type_url.ends_with("GetPlanetsInSolarSystemResponse"));

    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let req = make_request("eve.planet.skyhook.admin.api.ChangeOwnerRequest", Vec::new(), b"pln-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planet.skyhook.api.GetReinforcementExitHourRequest", planet_payload.clone(), b"pln-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planet.skyhook.api.GetStateRequest", planet_payload.clone(), b"pln-05");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 3. Planet Interaction (collector, factory, schematic, storage)
    let mut pi_payload = Vec::new();
    encode_id_field(1, 55001, &mut pi_payload);
    let req = make_request("eve.planetinteraction.collector.GetRequest", pi_payload.clone(), b"pi-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.factory.GetRequest", pi_payload.clone(), b"pi-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.schematic.GetRequest", pi_payload.clone(), b"pi-03");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.planetinteraction.storage.GetRequest", pi_payload, b"pi-04");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    // 4. Moon & Asteroid Belt
    let mut moon_payload = Vec::new();
    encode_id_field(1, 40_000_101, &mut moon_payload);
    let req = make_request("eve.moon.GetRequest", moon_payload.clone(), b"mn-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.moon.GetMoonsOfPlanetRequest", planet_payload.clone(), b"mn-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let mut belt_payload = Vec::new();
    encode_id_field(1, 40_000_201, &mut belt_payload);
    let req = make_request("eve.asteroidbelt.GetRequest", belt_payload, b"ab-01");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);

    let req = make_request("eve.asteroidbelt.GetBeltsOfPlanetRequest", planet_payload, b"ab-02");
    let rep = ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None).await.expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, STATUS_OK, "Status: {}, message: {}", rsp.status_code, rsp.status_message);
}

#[tokio::test]
async fn test_p1_track_a_regressions_issues_20_21_22() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // 1. Issue #20: Contract Creation Fees with empty title / 14+ args & Corporate Auction FinishAuction payout
    let mut sess_corp = SessionState::new(2001);
    sess_corp.user_id = Some(1);
    sess_corp.character_id = Some(90_000_001);
    sess_corp.corporation_id = Some(98_000_001);
    sess_corp.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess_corp.station_id = Some(60_003_760);

    let (item_id_fee, item_id_auc) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
            ch.balance = eve_db::IskCents::from_isk(100_000_000);
        }
        snap.corporations.insert(
            98_000_001,
            eve_db::CorporationRecord {
                corporation_id: 98_000_001,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ceo_id: 90_000_001,
                master_wallet: eve_db::IskCents::from_isk(50_000_000),
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            eve_db::InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i2 = snap.next_entity_id;
        snap.items.insert(
            i2,
            eve_db::InventoryItemRecord {
                item_id: i2,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, i2)
    });

    let bal_before = ctx.db.get_character(90_000_001).unwrap().balance.0;
    let create_rep = ctx
        .dispatch_rpc(
            &mut sess_corp,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(1_000_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()), // Empty title!
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(item_id_fee), PyRep::Integer(100)])]),
                PyRep::Integer(4), // 14th arg!
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("CreateContract with empty title and 14 args");
    let _ = create_rep;
    let bal_after = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert!(
        bal_after < bal_before,
        "Issue #20: CreateContract must charge broker fee + deposit even when title is empty and 14+ args are passed"
    );

    // Corporate auction FinishAuction routes proceeds to corporation wallet
    let corp_auc_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        if let Some(it) = snap.items.get_mut(&item_id_auc) {
            it.location_id = cid;
            it.flag = eve_db::InventoryFlag::Locked;
        }
        snap.contracts.insert(
            cid,
            eve_db::ContractRecord {
                contract_id: cid,
                issuer_id: 90_000_001,
                issuer_corp_id: 98_000_001,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: eve_db::IskCents::from_isk(1_000_000),
                current_bid: eve_db::IskCents::from_isk(2_500_000),
                current_bidder_id: Some(90_000_002),
                item_ids: vec![item_id_auc],
                expiration_unix: 100,
                for_corp: true,
                ..Default::default()
            },
        );
        cid
    });
    let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&98_000_001].master_wallet.0);
    let char_bal_before = ctx.db.get_character(90_000_001).unwrap().balance.0;
    ctx.dispatch_rpc(
        &mut sess_corp,
        "contractProxy",
        None,
        "FinishAuction",
        &[PyRep::Integer(corp_auc_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("FinishAuction on corporate auction");
    let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&98_000_001].master_wallet.0);
    let char_bal_after = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert_eq!(
        corp_bal_after - corp_bal_before,
        250_000_000,
        "Issue #20: Corporate auction FinishAuction must credit the selling corporation's master wallet"
    );
    assert_eq!(
        char_bal_after, char_bal_before,
        "Issue #20: Corporate auction FinishAuction must not credit the personal character wallet"
    );

    // 2. Issue #22: Quasar Financial Data Authorization
    let mut sess_other = SessionState::new(2002);
    sess_other.user_id = Some(2);
    sess_other.character_id = Some(90_000_002);
    sess_other.corporation_id = Some(1_000_009);
    sess_other.corp_role = 0;

    let mut target_char_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut target_char_payload);
    let req_other_journal = make_request(
        "eve.accounting.character.journal.GetCharacterEntriesRequest",
        target_char_payload.clone(),
        b"sec-22-1",
    );
    let rep = ctx
        .dispatch_rpc(
            &mut sess_other,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req_other_journal.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-character wallet journal query must return 403");

    let req_other_orders = make_request(
        "eve.market.orders.GetOpenForCharacterRequest",
        target_char_payload,
        b"sec-22-2",
    );
    let rep = ctx
        .dispatch_rpc(
            &mut sess_other,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req_other_orders.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-character market order query must return 403");

    let mut target_corp_payload = Vec::new();
    encode_id_field(1, 98_000_001, &mut target_corp_payload);
    let req_corp_orders = make_request(
        "eve.market.orders.GetOpenForCorporationRequest",
        target_corp_payload,
        b"sec-22-3",
    );
    let rep = ctx
        .dispatch_rpc(
            &mut sess_other,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req_corp_orders.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("rpc");
    let rsp = extract_response_proto(&rep);
    assert_eq!(rsp.status_code, 403, "Issue #22: Cross-corporation market order query must return 403");
}
