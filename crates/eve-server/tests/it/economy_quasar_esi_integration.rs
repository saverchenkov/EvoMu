//! Round-4 Track 4 Regression Tests (#188 through #198):
//! Economy, Contracts, Industry, Quasar, ESI & Persistence.

#![forbid(unsafe_code)]

use crate::common::login_pilot;
use eve_db::{
    ContractRecord, CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketOrderMeta, MarketOrderRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    EvePublicRequest, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::esi::runtime_state::EsiCalendarEvent;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

fn make_quasar_req(
    char_id: i64,
    user_id: i32,
    role_mask: u64,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![1; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test".into(),
        application_instance_uuid: vec![2; 16],
        authoritative_context: Some(AuthoritativeContext::for_session_at(
            Some(user_id),
            Some(char_id as u32),
            None,
            role_mask,
            "tranquility",
            vec![3; 16],
            now,
        )),
        trace_context: None,
    }
}

fn make_quasar_sess(session_id: u64, user_id: u64, char_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.role = role;
    s
}

#[tokio::test]
async fn test_r4_188_character_transfer_blocks_paused_jobs_corp_orders_and_consent_mismatch() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let sess = make_quasar_sess(18801, 1, char_id, 0);
    let now = ctx.now_unix();

    // 1. Paused industry job blocks character transfer
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(1, 5_000);
        snap.industry_jobs.insert(
            918_801,
            IndustryJobRecord {
                job_id: 918_801,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 1,
                blueprint_type_id: 687,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Paused".into(),
                duration_seconds: 3600,
                start_unix: now,
            },
        );
    });
    let mut xfer_buf = Vec::new();
    encode_varint_field(91, char_id as u64, &mut xfer_buf);
    encode_varint_field(92, 2, &mut xfer_buf);
    let paused_err = ctx.quasar_gateway.handle_public_request(
        &sess,
        &make_quasar_req(
            char_id,
            1,
            0,
            "eve.user.character.transfer.QueueRequest",
            xfer_buf.clone(),
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(
        paused_err.status_code, STATUS_OK,
        "Paused industry job must block character transfer"
    );

    // 2. Corporate market order issued_by char_id blocks character transfer
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.market_orders.insert(
            918_802,
            MarketOrderRecord {
                order_id: 918_802,
                owner_id: 98_188_001, // corp order
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_order_meta.insert(
            918_802,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "region".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
    });
    let corp_order_err = ctx.quasar_gateway.handle_public_request(
        &sess,
        &make_quasar_req(
            char_id,
            1,
            0,
            "eve.user.character.transfer.QueueRequest",
            xfer_buf.clone(),
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(
        corp_order_err.status_code, STATUS_OK,
        "Active corporate market order issued by character must block transfer"
    );

    // 3. Destination consent token mismatch blocks transfer when consent token is configured
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.remove(&918_802);
        snap.market_order_meta.remove(&918_802);
        snap.service_state_blobs.insert(
            "char_transfer_consent_token:2".into(),
            "secret-consent-2".into(),
        );
    });
    let consent_err = ctx.quasar_gateway.handle_public_request(
        &sess,
        &make_quasar_req(
            char_id,
            1,
            0,
            "eve.user.character.transfer.QueueRequest",
            xfer_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(
        consent_err.status_code, STATUS_OK,
        "Missing/mismatched destination consent token must block transfer"
    );

    // Supplying matching consent token succeeds
    let mut xfer_with_tok = Vec::new();
    encode_varint_field(91, char_id as u64, &mut xfer_with_tok);
    encode_varint_field(92, 2, &mut xfer_with_tok);
    encode_string_field(93, "secret-consent-2", &mut xfer_with_tok);
    let ok_res = ctx.quasar_gateway.handle_public_request(
        &sess,
        &make_quasar_req(
            char_id,
            1,
            0,
            "eve.user.character.transfer.QueueRequest",
            xfer_with_tok,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(ok_res.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_r4_189_entitlement_redeem_rolls_back_on_second_stack_overflow() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let admin_sess = make_quasar_sess(18901, 1, char_id, u64::MAX);
    let user_sess = make_quasar_sess(18902, 1, char_id, 0);
    let now = ctx.now_unix();

    // Spawn an asset with 2 stacks: [(34, 1), (35, u32::MAX)] and grant 2 units.
    // Redeeming 2 units overflows i32 on the second stack (35) and must NOT leave the first stack (34) in snap.items!
    let (mut stack1, mut stack2, mut stacks, mut spawn_payload) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut t1 = Vec::new();
    encode_varint_field(1, 34, &mut t1);
    encode_bytes_field(1, &t1, &mut stack1);
    encode_varint_field(2, 1, &mut stack1);

    let mut t2 = Vec::new();
    encode_varint_field(1, 35, &mut t2);
    encode_bytes_field(1, &t2, &mut stack2);
    encode_varint_field(2, u32::MAX as u64, &mut stack2);

    encode_bytes_field(1, &stack1, &mut stacks);
    encode_bytes_field(1, &stack2, &mut stacks);
    encode_string_field(2, "Overflow Test Asset", &mut spawn_payload);
    encode_bytes_field(6, &stacks, &mut spawn_payload);

    let spawn_res = ctx.quasar_gateway.handle_public_request(
        &admin_sess,
        &make_quasar_req(
            char_id,
            1,
            u64::MAX,
            "eve.assetholding.asset.api.SpawnRequest",
            spawn_payload,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(spawn_res.status_code, STATUS_OK);

    let mut asset_uuid = [0u8; 16];
    asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
    asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());

    let mut char_id_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_id_msg);
    let mut holder_msg = Vec::new();
    encode_bytes_field(1, &char_id_msg, &mut holder_msg);

    let (mut a_msg, mut g_msg, mut ent_id) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &asset_uuid, &mut a_msg);
    encode_string_field(1, "r4-189-group", &mut g_msg);
    encode_bytes_field(2, b"GROUP_R4_189_001", &mut g_msg);
    encode_bytes_field(1, &holder_msg, &mut ent_id);
    encode_bytes_field(2, &a_msg, &mut ent_id);
    encode_bytes_field(3, &g_msg, &mut ent_id);

    // DirectDelivery of 2 units to char_id
    let mut entry = Vec::new();
    encode_bytes_field(2, &ent_id, &mut entry);
    encode_varint_field(10, 2, &mut entry);
    let mut dd_payload = Vec::new();
    encode_bytes_field(2, &entry, &mut dd_payload);

    let grant_res = ctx.quasar_gateway.handle_public_request(
        &admin_sess,
        &make_quasar_req(
            char_id,
            1,
            u64::MAX,
            "eve.assetholding.entitlement.api.DirectDeliveryRequest",
            dd_payload,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(grant_res.status_code, STATUS_OK);

    let items_before = ctx.db.with_snapshot(|s| s.items.len());

    // RedeemRequest for char_id + asset_uuid -> must fail on stack 2 overflow and roll back stack 1!
    let mut redeem_buf = Vec::new();
    encode_bytes_field(1, &ent_id, &mut redeem_buf);
    let redeem_res = ctx.quasar_gateway.handle_public_request(
        &user_sess,
        &make_quasar_req(
            char_id,
            1,
            0,
            "eve_public.assetholding.entitlement.api.RedeemRequest",
            redeem_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(redeem_res.status_code, STATUS_OK);
    let items_after = ctx.db.with_snapshot(|s| s.items.len());
    assert_eq!(
        items_after, items_before,
        "Partial item insertion from stack 1 must be rolled back when stack 2 overflows"
    );
}

#[tokio::test]
async fn test_r4_190_plex_store_validates_recipient_before_deducting_plex_and_allows_free_offers()
{
    let ctx = ServerContext::new();
    let now = ctx.now_unix();

    // Seed 500 PLEX for user 1, a paid offer 9189 (100 PLEX), and a free (0-PLEX) offer 9190 in catalog
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(1, 500);
        let mut catalog =
            eve_server::services::quasar_payment_store_types::read_store_catalog(snap);
        catalog.products.push(
            eve_server::services::quasar_payment_store_types::StoreProduct {
                id: 9001,
                name: "Promo Crate".into(),
                description: "Crate".into(),
                type_id: 34,
                quantity: 10,
            },
        );
        catalog.offers.push(
            eve_server::services::quasar_payment_store_types::StoreOffer {
                id: 9189,
                name: "Paid Bundle".into(),
                description: "100 PLEX bundle".into(),
                category_id: 2,
                price_plex: 100,
                product_ids: vec![9001],
                targeted: false,
                price_cents: None,
                currency: None,
            },
        );
        catalog.offers.push(
            eve_server::services::quasar_payment_store_types::StoreOffer {
                id: 9190,
                name: "Free Daily Gift".into(),
                description: "0 PLEX promotional bundle".into(),
                category_id: 2,
                price_plex: 0,
                product_ids: vec![9001],
                targeted: false,
                price_cents: None,
                currency: None,
            },
        );
        eve_server::services::quasar_payment_store_types::save_store_catalog(snap, &catalog);
    });

    // 1. Purchasing paid offer 9189 for nonexistent character 999_999_999 must error and preserve all 500 PLEX
    let bad_sess = make_quasar_sess(19001, 1, 999_999_999, 0);
    let good_sess = make_quasar_sess(19002, 1, 90_000_001, 0);
    let mut paid_id = Vec::new();
    encode_string_field(1, "9189", &mut paid_id);
    let mut paid_buf = Vec::new();
    encode_bytes_field(1, &paid_id, &mut paid_buf);
    encode_varint_field(2, 1, &mut paid_buf);
    let err_res = ctx.quasar_gateway.handle_public_request(
        &bad_sess,
        &make_quasar_req(
            999_999_999,
            1,
            0,
            "eve_public.plex.vault.api.PurchaseRequest",
            paid_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(err_res.status_code, STATUS_OK);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0)),
        500,
        "PLEX must not be deducted when recipient character validation fails"
    );

    // 2. Purchasing 0-PLEX offer 9190 for valid character 90_000_001 deducts 0 PLEX (not 1 PLEX)
    let mut free_id = Vec::new();
    encode_string_field(1, "9190", &mut free_id);
    let mut free_buf = Vec::new();
    encode_bytes_field(1, &free_id, &mut free_buf);
    encode_varint_field(2, 1, &mut free_buf);
    let ok_free = ctx.quasar_gateway.handle_public_request(
        &good_sess,
        &make_quasar_req(
            90_000_001,
            1,
            0,
            "eve_public.plex.vault.api.PurchaseRequest",
            free_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(ok_free.status_code, STATUS_OK);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0)),
        500,
        "0-PLEX offer must cost 0 PLEX"
    );
}

#[tokio::test]
async fn test_r4_191_corp_goal_close_and_delete_require_director_role_bit() {
    let ctx = ServerContext::new();
    let ceo_id = 90_000_001;
    let non_dir_id = 90_000_002;
    let ceo_sess = make_quasar_sess(19101, 1, ceo_id, 0);
    let non_dir_sess = make_quasar_sess(19102, 2, non_dir_id, 0);
    let corp_id = 98_191_001i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = corp_id;
        snap.characters.get_mut(&non_dir_id).unwrap().corporation_id = corp_id;
        let mut roles = HashMap::new();
        roles.insert(ceo_id, eve_db::corp_roles::ROLE_DIRECTOR);
        // non_dir_id has non-zero role bit (ROLE_ACCOUNTANT), but NOT ROLE_DIRECTOR (1)!
        roles.insert(non_dir_id, eve_db::corp_roles::ROLE_ACCOUNTANT);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Goal 191 Corp".into(),
                ticker: "G191".into(),
                ceo_id,
                members: HashSet::from([ceo_id, non_dir_id]),
                member_roles: roles,
                ..Default::default()
            },
        );
    });

    // CEO creates a corp goal
    let mut create_buf = Vec::new();
    encode_string_field(1, "Mine Veldspar", &mut create_buf);
    encode_string_field(2, "Corp mining goal", &mut create_buf);
    encode_varint_field(3, 100, &mut create_buf);
    let create_res = ctx.quasar_gateway.handle_public_request(
        &ceo_sess,
        &make_quasar_req(
            ceo_id,
            1,
            0,
            "eve_public.goal.api.CreateRequest",
            create_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(create_res.status_code, STATUS_OK);
    // CreateResponse payload is { Identifier goal = 1 }, which matches CloseRequest { Identifier goal = 1 }
    let close_buf = create_res.payload.as_ref().unwrap().value.clone();

    // Non-director with ROLE_ACCOUNTANT must be forbidden from closing the goal
    let non_dir_close = ctx.quasar_gateway.handle_public_request(
        &non_dir_sess,
        &make_quasar_req(
            non_dir_id,
            2,
            0,
            "eve_public.goal.api.CloseRequest",
            close_buf.clone(),
            now,
        ),
        None,
        false,
        now,
    );
    assert_ne!(
        non_dir_close.status_code, STATUS_OK,
        "Non-director corp member must not be able to close a corporation goal"
    );

    // CEO can close the goal
    let ceo_close = ctx.quasar_gateway.handle_public_request(
        &ceo_sess,
        &make_quasar_req(
            ceo_id,
            1,
            0,
            "eve_public.goal.api.CloseRequest",
            close_buf,
            now,
        ),
        None,
        false,
        now,
    );
    assert_eq!(ceo_close.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_r4_192_personal_contracts_excluded_from_corp_contract_queries() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(19201);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let other_member = 90_000_002;
    let corp_id = 98_192_001i64;
    let personal_cid = 880_192_001i64;
    let corp_cid = 880_192_002i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    sess.corporation_id = Some(corp_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        snap.characters
            .get_mut(&other_member)
            .unwrap()
            .corporation_id = corp_id;
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Contracts 192 Corp".into(),
                ticker: "C192".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id, other_member]),
                ..Default::default()
            },
        );
        // Personal contract issued by other_member (for_corp == false, but issuer_corp_id == corp_id)
        snap.contracts.insert(
            personal_cid,
            ContractRecord {
                contract_id: personal_cid,
                issuer_id: other_member,
                issuer_corp_id: corp_id,
                for_corp: false,
                availability: "Personal".into(),
                assignee_id: other_member,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                ..Default::default()
            },
        );
        // Corporate contract issued on behalf of corp_id (for_corp == true)
        snap.contracts.insert(
            corp_cid,
            ContractRecord {
                contract_id: corp_cid,
                issuer_id: other_member,
                issuer_corp_id: corp_id,
                for_corp: true,
                availability: "Public".into(),
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                ..Default::default()
            },
        );
    });

    // 1. GetContractListForOwner for corp_id returns only corp_cid, NOT personal_cid
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "contractProxy",
            None,
            "GetContractListForOwner",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetContractListForOwner corp");
    let PyRep::List(rows) = res else {
        panic!("expected List")
    };
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.dict_get("contractID").and_then(|v| v.as_i64()))
        .collect();
    assert!(ids.contains(&corp_cid));
    assert!(
        !ids.contains(&personal_cid),
        "Personal contract must not leak into corporate contract list"
    );

    // 2. ESI GET /corporations/{corporation_id}/contracts returns only corp_cid
    let esi_res = eve_server::esi::economy_routes::dispatch_economy_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/contracts",
        &HashMap::from([("corporation_id".into(), corp_id.to_string())]),
        &HashMap::new(),
        None,
        1,
    )
    .expect("ESI corp contracts route");
    assert_eq!(esi_res.status_code, 200);
    let arr = esi_res.body.unwrap();
    let esi_ids: Vec<i64> = arr
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["contract_id"].as_i64())
        .collect();
    assert!(esi_ids.contains(&corp_cid));
    assert!(
        !esi_ids.contains(&personal_cid),
        "Personal contract must not leak into ESI corporate contracts"
    );
}

#[tokio::test]
async fn test_r4_193_contract_deposit_refunds_and_unbid_corp_auction_item_return() {
    let ctx = ServerContext::new();
    let mut sess1 = SessionState::new(19301);
    let mut sess2 = SessionState::new(19302);
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let corp_id = 98_193_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(c1)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(c2)],
        &kw,
        None,
    )
    .await
    .unwrap();
    sess1.corporation_id = Some(corp_id);

    let reject_cid = 880_193_001i64;
    let corp_auc_cid = 880_193_002i64;
    let corp_item_id = 770_193_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().balance = IskCents(0);
        snap.characters.get_mut(&c1).unwrap().corporation_id = corp_id;
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Auction 193 Corp".into(),
                ticker: "A193".into(),
                ceo_id: c1,
                members: HashSet::from([c1]),
                ..Default::default()
            },
        );
        // 1. Contract assigned to c2 with a 10,000 ISK deposit in service_state_blobs
        snap.contracts.insert(
            reject_cid,
            ContractRecord {
                contract_id: reject_cid,
                issuer_id: c1,
                issuer_corp_id: corp_id,
                for_corp: false,
                availability: "Personal".into(),
                assignee_id: c2,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                ..Default::default()
            },
        );
        snap.service_state_blobs
            .insert(format!("contract_deposit:{reject_cid}"), "1000000".into()); // 10,000.00 ISK

        // 2. Expired unbid corporate auction with item corp_item_id
        snap.items.insert(
            corp_item_id,
            InventoryItemRecord {
                item_id: corp_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            corp_auc_cid,
            ContractRecord {
                contract_id: corp_auc_cid,
                issuer_id: c1,
                issuer_corp_id: corp_id,
                for_corp: true,
                availability: "Public".into(),
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                expiration_unix: 1,
                item_ids: vec![corp_item_id],
                ..Default::default()
            },
        );
    });

    // c2 rejects reject_cid -> c1 receives 10,000 ISK deposit refund
    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "RejectContract",
        &[PyRep::Integer(reject_cid)],
        &kw,
        None,
    )
    .await
    .expect("RejectContract");
    assert_eq!(
        ctx.db.get_character(c1).unwrap().balance,
        IskCents(1_000_000),
        "Issuer must be refunded contract_deposit on RejectContract"
    );

    // c1 finishes expired unbid corporate auction -> item returns to corp_id in CorpSAG1 (not c1)
    ctx.dispatch_rpc(
        &mut sess1,
        "contractProxy",
        None,
        "FinishAuction",
        &[PyRep::Integer(corp_auc_cid)],
        &kw,
        None,
    )
    .await
    .expect("FinishAuction");
    let item = ctx.db.get_item(corp_item_id).unwrap();
    assert_eq!(
        item.owner_id, corp_id,
        "Unbid corporate auction item must return to issuer_corp_id"
    );
    assert_eq!(item.flag, InventoryFlag::CorpSAG1);
}

#[tokio::test]
async fn test_r4_194_take_offer_for_corporation_deducts_analysis_kredits() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(19401);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let corp_id = 98_194_001i64;
    let npc_corp = 1_000_125i64;
    let offer_id = 919_401i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.station_id = Some(60_012_256); // Yulai IX - CONCORD Bureau (1_000_125)

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = corp_id;
        ch.station_id = Some(60_012_256);
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "LP 194 Corp".into(),
            ticker: "LP94".into(),
            ceo_id: char_id,
            members: HashSet::from([char_id]),
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_isk(10_000_000);
        snap.corporations.insert(corp_id, corp);
        snap.corp_lp_balances.insert((corp_id, npc_corp), 50_000);
        let offers = vec![eve_server::services::lp_store::LPOffer {
            offer_id,
            corp_id: npc_corp,
            type_id: 34,
            quantity: 1,
            lp_cost: 100,
            isk_cost: IskCents(0),
            req_items: vec![],
            ak_cost: 500, // Requires 500 Analysis Kredits
            min_standing: None,
        }];
        snap.service_state_blobs.insert(
            "lp_store_offers".into(),
            serde_json::to_string(&offers).unwrap(),
        );
    });

    // Pilot has 0 Analysis Kredits -> TakeOfferForCorporation must fail
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCorporation",
            &[
                PyRep::Integer(npc_corp),
                PyRep::Integer(offer_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        err.code.contains("AnalysisKredits") || err.message.contains("Analysis Kredits"),
        "Expected Analysis Kredits error, got: {err:?}"
    );
}

#[tokio::test]
async fn test_r4_195_paused_industry_job_keeps_blueprint_item_locked() {
    let ctx = ServerContext::new();
    let bp_id = 770_195_001i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            919_501,
            IndustryJobRecord {
                job_id: 919_501,
                installer_id: 90_000_001,
                facility_id: 60_003_760,
                blueprint_item_id: bp_id,
                blueprint_type_id: 687,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Paused".into(),
                duration_seconds: 3600,
                start_unix: now,
            },
        );
    });

    let locked = ctx
        .db
        .with_snapshot(|snap| snap.is_item_locked_in_contract_or_job(bp_id));
    assert!(
        locked,
        "Blueprint in a Paused industry job must be reported as locked by is_item_locked_in_contract_or_job"
    );
}

#[tokio::test]
async fn test_r4_196_get_blueprint_data_rejects_unowned_foreign_blueprint_item() {
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(19601);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let other_id = 90_000_002;
    let foreign_bp_id = 770_196_001i64;
    let kw = HashMap::new();

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            foreign_bp_id,
            InventoryItemRecord {
                item_id: foreign_bp_id,
                type_id: 687, // Rifter Blueprint
                owner_id: other_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "blueprintManager",
            None,
            "GetBlueprintData",
            &[PyRep::Integer(foreign_bp_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");
}

#[tokio::test]
async fn test_r4_197_esi_calendar_blocks_uninvited_rsvp_and_allows_corp_event_access() {
    let ctx = ServerContext::new();
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let stranger = 90_000_099;
    let corp_id = 1_000_009; // c1 and c2 are in 1000009
    let corp_ev_id = 919_701i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().corporation_id = corp_id;
        snap.characters.get_mut(&c2).unwrap().corporation_id = corp_id;
        let mut ch99 = snap.characters[&c1].clone();
        ch99.character_id = stranger;
        ch99.corporation_id = 1_000_125; // Different corp
        snap.characters.insert(stranger, ch99);
    });

    // Seed a corporation calendar event where c1 already RSVPed (so attendees = [c1])
    ctx.esi_state
        .calendar_events
        .write()
        .unwrap()
        .insert(
            corp_ev_id,
            EsiCalendarEvent {
                event_id: corp_ev_id,
                owner_id: corp_id,
                owner_name: "Caldari Provisions".into(),
                owner_type: "corporation".into(),
                event_date_unix: 1_800_000_000,
                title: "Corp Fleet Op".into(),
                duration_minutes: 60,
                importance: 1,
                text: "Form up in Jita".into(),
                attendees: vec![c1],
            },
        );

    // 1. Fellow corporation member c2 CAN view the event details and attendees even though attendees is non-empty ([c1])
    let c2_get = eve_server::esi::char_social_routes::dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/calendar/{event_id}",
        &HashMap::from([
            ("character_id".into(), c2.to_string()),
            ("event_id".into(), corp_ev_id.to_string()),
        ]),
        &HashMap::new(),
        &[],
    )
    .unwrap();
    assert_eq!(
        c2_get.status_code, 200,
        "Corporation member must be allowed to view corporation calendar event"
    );

    // 2. Uninvited stranger in a different corp CANNOT RSVP via PUT /characters/{stranger}/calendar/{corp_ev_id}
    let stranger_put = eve_server::esi::char_social_routes::dispatch_char_social_route(
        &ctx,
        "PUT",
        "/characters/{character_id}/calendar/{event_id}",
        &HashMap::from([
            ("character_id".into(), stranger.to_string()),
            ("event_id".into(), corp_ev_id.to_string()),
        ]),
        &HashMap::new(),
        br#"{"response":"accepted"}"#,
    )
    .unwrap();
    assert_eq!(
        stranger_put.status_code, 403,
        "Uninvited character must receive 403 when attempting to RSVP to a private calendar event"
    );
}

pub(crate) mod contracts_fingerprint_deposit_corp_accept {
    // Round-5 Track 4 Regression Tests (#238 through #250):
    // Economy, Contracts, Industry, Quasar, ESI & SQLite Persistence.


    use crate::common::login_pilot;
    use eve_db::{
        corp_roles, AccountRecord, ContractRecord, CorporationRecord, InventoryFlag,
        InventoryItemRecord, IskCents, TrainedSkillRecord, WalletJournalEntry, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
        EvePublicRequest, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&character_id) {
                ch.user_id = 1;
            }
        });
        let now = ctx.now_unix();
        let scope_vec: Vec<String> = scopes.into_iter().map(str::to_string).collect();
        let code = ctx
            .db
            .authorize_pkce("demo", "demo-secret", character_id, "evomu-r5-test", "https://localhost/callback", scope_vec, PKCE_CHALLENGE, "S256", now)
            .expect("authorize_pkce");
        ctx.db.exchange_pkce_code(&code, "evomu-r5-test", PKCE_VERIFIER, now).expect("exchange_pkce_code").access_token
    }

    fn make_quasar_req(char_id: i64, user_id: i32, role_mask: u64, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicRequest {
        EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
            correlation_uuid: vec![1; 16],
            payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
            external_origin: "test".into(),
            application_instance_uuid: vec![2; 16],
            authoritative_context: Some(AuthoritativeContext::for_session_at(
                if user_id > 0 { Some(user_id) } else { None },
                if char_id > 0 { Some(char_id as u32) } else { None },
                None, role_mask, "tranquility", vec![3; 16], now,
            )),
            trace_context: None,
        }
    }

    fn kv_i64(rep: &PyRep, key: &str) -> i64 {
        match rep {
            PyRep::KeyVal(kv) => kv.get(key).and_then(|v| v.as_i64()).unwrap(),
            PyRep::Dict(d) => d.iter().find(|(k, _)| k.as_str() == Some(key)).and_then(|(_, v)| v.as_i64()).unwrap(),
            PyRep::Integer(v) => *v,
            other => panic!("expected {key} in {other:?}"),
        }
    }

    fn make_quasar_sess(session_id: u64, user_id: Option<u64>, char_id: Option<i64>, role: u64) -> SessionState {
        let mut s = SessionState::new(session_id);
        s.user_id = user_id;
        s.character_id = char_id;
        s.role = role;
        s
    }

    #[tokio::test]
    async fn test_r5_238_243_244_245_contracts_fingerprint_deposit_corp_accept_and_tradein_guards() {
        let ctx = ServerContext::new();
        let now = ctx.now_unix();
        let mut s_issuer = SessionState::new(23801);
        login_pilot(&ctx, &mut s_issuer, "demo", "demo-secret").await;
        let kw = HashMap::new();
        ctx.dispatch_rpc(&mut s_issuer, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

        let mut s_acceptor = SessionState::new(23802);
        s_acceptor.user_id = Some(2);
        s_acceptor.character_id = Some(90_000_002);
        s_acceptor.corporation_id = Some(1_000_009);
        s_acceptor.station_id = Some(60_003_760);
        s_acceptor.ship_id = Some(100_000_002);

        // #238: SQLite incremental fingerprint persists contract bid/buyout/requested_items/title updates
        let db_path = std::env::temp_dir().join(format!("r5_238_contracts_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        ctx.db.with_mut_snapshot(|snap| {
            snap.contracts.insert(700_238, ContractRecord {
                contract_id: 700_238, issuer_id: 90_000_001, issuer_corp_id: 1_000_009, assignee_id: 0, acceptor_id: None,
                contract_type: "Auction".into(), status: "Outstanding".into(), title: "Initial Title".into(),
                for_corp: false, availability: "Public".into(), start_station_id: 60_003_760, end_station_id: 60_003_760,
                date_issued_unix: now, expiration_unix: now + 86_400, num_days: 1, price: IskCents::from_isk(1_000_000),
                reward: IskCents(0), collateral: IskCents(0), buyout: IskCents::from_isk(5_000_000), item_ids: vec![],
                requested_items: vec![(34, 100)], current_bid: IskCents(0), current_bidder_id: None, bid_history: vec![],
            });
        });
        ctx.db.save_to_sqlite(&db_path).expect("initial save_to_sqlite");
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.contracts.get_mut(&700_238).unwrap();
            c.title = "Updated Title".into();
            c.buyout = IskCents::from_isk(6_000_000);
            c.requested_items = vec![(34, 250)];
            c.current_bid = IskCents::from_isk(2_000_000);
            c.current_bidder_id = Some(90_000_002);
            c.bid_history.push((90_000_002, IskCents::from_isk(2_000_000), now + 10));
        });
        ctx.db.save_to_sqlite(&db_path).expect("incremental save_to_sqlite");
        let reloaded = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
        reloaded.with_snapshot(|snap| {
            let c = snap.contracts.get(&700_238).expect("reloaded contract");
            assert_eq!(c.title, "Updated Title");
            assert_eq!(c.buyout, IskCents::from_isk(6_000_000));
            assert_eq!(c.requested_items, vec![(34, 250)]);
            assert_eq!(c.current_bid, IskCents::from_isk(2_000_000));
            assert_eq!(c.current_bidder_id, Some(90_000_002));
            assert_eq!(c.bid_history.len(), 1);
        });
        let _ = std::fs::remove_file(&db_path);

        // #243: Courier contract deposit is refunded to issuer upon acceptance
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(700_243, InventoryItemRecord {
                item_id: 700_243, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
                flag: InventoryFlag::Hangar, quantity: 100, singleton: false, version: 1, ..Default::default()
            });
        });
        let courier_args = vec![
            PyRep::String("Courier".into()), PyRep::None, PyRep::None, PyRep::None, PyRep::None,
            PyRep::Integer(60_003_760), PyRep::Integer(60_003_760), PyRep::Real(0.0), PyRep::Real(100_000.0),
            PyRep::Real(200_000.0), PyRep::None, PyRep::None, PyRep::List(vec![PyRep::Integer(700_243)]),
        ];
        let courier_rep = ctx
            .dispatch_rpc(&mut s_issuer, "contractProxy", None, "CreateContract", &courier_args, &HashMap::new(), None)
            .await.expect("CreateContract Courier");
        let courier_cid = kv_i64(&courier_rep, "contractID");
        assert!(ctx.db.with_snapshot(|s| s.service_state_blobs.contains_key(&format!("contract_deposit:{courier_cid}"))));
        let issuer_before_accept = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance.0);
        ctx.dispatch_rpc(&mut s_acceptor, "contractProxy", None, "AcceptContract", &[PyRep::Integer(courier_cid)], &HashMap::new(), None)
            .await.expect("AcceptContract Courier");
        let issuer_after_accept = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance.0);
        assert!(issuer_after_accept > issuer_before_accept, "Courier deposit must be refunded on acceptance");
        assert!(ctx.db.with_snapshot(|s| !s.service_state_blobs.contains_key(&format!("contract_deposit:{courier_cid}"))));

        // #244 & #245: Corporate contract acceptance role/wallet/delivery & trade-in item state guards
        let corp_id = 98_244_001_i64;
        s_acceptor.corporation_id = Some(corp_id);
        s_acceptor.corp_role = 0;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.corporation_id = corp_id; }
            let mut corp = CorporationRecord {
                corporation_id: corp_id, name: "R5 Contract Corp".into(), ticker: "R5CC".into(), ceo_id: 90_000_001,
                alliance_id: None, tax_rate: 0.0, master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([90_000_002]), ..Default::default()
            };
            corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            snap.corporations.insert(corp_id, corp);
            snap.items.insert(700_244, InventoryItemRecord {
                item_id: 700_244, type_id: 587, owner_id: 90_000_001, location_id: 60_003_760,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(700_245_1, InventoryItemRecord {
                item_id: 700_245_1, type_id: 687, owner_id: corp_id, location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: true, version: 1,
                is_blueprint_copy: true, blueprint_runs_remaining: Some(5), ..Default::default()
            });
            let mut dmg_attrs = HashMap::new();
            dmg_attrs.insert(3, 15.0);
            snap.items.insert(700_245_2, InventoryItemRecord {
                item_id: 700_245_2, type_id: 687, owner_id: corp_id, location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: dmg_attrs, ..Default::default()
            });
        });
        let mut ie_kw = HashMap::new();
        ie_kw.insert("requestItemTypeList".into(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(687), PyRep::Integer(1)])]));
        let ie_args = vec![
            PyRep::String("ItemExchange".into()), PyRep::None, PyRep::None, PyRep::None, PyRep::None,
            PyRep::Integer(60_003_760), PyRep::Integer(60_003_760), PyRep::Real(500_000.0), PyRep::Real(0.0),
            PyRep::Real(0.0), PyRep::None, PyRep::None, PyRep::List(vec![PyRep::Integer(700_244)]),
        ];
        let ie_rep = ctx
            .dispatch_rpc(&mut s_issuer, "contractProxy", None, "CreateContract", &ie_args, &ie_kw, None)
            .await.expect("CreateContract ItemExchange");
        let ie_cid = kv_i64(&ie_rep, "contractID");

        let mut accept_corp_kw = HashMap::new();
        accept_corp_kw.insert("forCorp".into(), PyRep::Bool(true));
        let role_err = ctx
            .dispatch_rpc(&mut s_acceptor, "contractProxy", None, "AcceptContract", &[PyRep::Integer(ie_cid)], &accept_corp_kw, None)
            .await.expect_err("Member without Contract Manager role cannot accept for corp");
        assert_eq!(role_err.code, "CrpAccessDenied");

        s_acceptor.corp_role = corp_roles::ROLE_CONTRACT_MANAGER;
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(90_000_002, corp_roles::ROLE_CONTRACT_MANAGER);
        });
        let tradein_err = ctx
            .dispatch_rpc(&mut s_acceptor, "contractProxy", None, "AcceptContract", &[PyRep::Integer(ie_cid)], &accept_corp_kw, None)
            .await.expect_err("BPC and damaged item must not satisfy contract requested_items");
        assert_eq!(tradein_err.code, "MissingRequestedContractItems");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(700_245_3, InventoryItemRecord {
                item_id: 700_245_3, type_id: 687, owner_id: corp_id, location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        });
        ctx.dispatch_rpc(&mut s_acceptor, "contractProxy", None, "AcceptContract", &[PyRep::Integer(ie_cid)], &accept_corp_kw, None)
            .await.expect("AcceptContract forCorp with clean BPO");
        ctx.db.with_snapshot(|snap| {
            let c = snap.contracts.get(&ie_cid).unwrap();
            assert_eq!(c.acceptor_id, Some(corp_id));
            let corp = snap.corporations.get(&corp_id).unwrap();
            assert_eq!(corp.division_balance(1), IskCents::from_isk(9_500_000));
            assert_eq!(corp.master_wallet, IskCents::from_isk(9_500_000));
            let delivered = snap.items.get(&700_244).unwrap();
            assert_eq!(delivered.owner_id, corp_id);
            assert_eq!(delivered.flag, InventoryFlag::CorpDeliveries);
            assert!(snap.items.contains_key(&700_245_1));
            assert!(snap.items.contains_key(&700_245_2));
        });
    }

    #[tokio::test]
    async fn test_r5_239_240_246_247_quasar_transfer_steam_omega_gift_and_entitlement_guards() {
        let ctx = ServerContext::new();
        let now = ctx.now_unix();
        let char_id = 90_000_001_i64;
        let sess_u1 = make_quasar_sess(23901, Some(1), Some(char_id), 0);

        // #239: Character transfer destination account validation, self-transfer block, consent consumption, Omega clone sync
        ctx.db.with_mut_snapshot(|snap| {
            snap.account_plex_vault.insert(1, 5_000);
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.clone_grade = 2; }
            snap.accounts.insert("pilot2".into(), AccountRecord {
                user_id: 2, username: "pilot2".into(), password_token: "password".into(),
                role: 0, banned_until_unix: None, ban_reason: None,
            });
        });
        let mut nonexist_buf = Vec::new();
        encode_varint_field(91, char_id as u64, &mut nonexist_buf);
        encode_varint_field(92, 9999, &mut nonexist_buf);
        let r_nonexist = ctx.quasar_gateway.handle_public_request(
            &sess_u1, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", nonexist_buf, now), None, false, now,
        );
        assert_ne!(r_nonexist.status_code, STATUS_OK);

        let mut self_buf = Vec::new();
        encode_varint_field(91, char_id as u64, &mut self_buf);
        encode_varint_field(92, 1, &mut self_buf);
        let r_self = ctx.quasar_gateway.handle_public_request(
            &sess_u1, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", self_buf, now), None, false, now,
        );
        assert_ne!(r_self.status_code, STATUS_OK);

        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("char_transfer_consent_token:2".into(), "consent-tok-2".into());
        });
        let mut exec_buf = Vec::new();
        encode_varint_field(91, char_id as u64, &mut exec_buf);
        encode_varint_field(92, 2, &mut exec_buf);
        encode_string_field(93, "consent-tok-2", &mut exec_buf);
        let r_exec = ctx.quasar_gateway.handle_public_request(
            &sess_u1, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", exec_buf, now), None, false, now,
        );
        assert_eq!(r_exec.status_code, STATUS_OK, "QueueRequest failed: {}", r_exec.status_message);
        ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id).unwrap();
            assert_eq!(ch.user_id, 2);
            assert_eq!(ch.clone_grade, 1, "Transferred character must sync clone_grade to destination account");
            assert!(!snap.service_state_blobs.contains_key("char_transfer_consent_token:2"));
            assert!(snap.service_state_blobs.contains_key(&format!("char_transfer_evict_session:{char_id}")));
        });

        // #240: AuthenticateSteamRequest requires authenticated caller and blocks cross-account hijacking
        let admin_role = u64::MAX;
        let sess_admin = make_quasar_sess(24701, Some(1), Some(char_id), admin_role);
        let mut link_steam = Vec::new();
        encode_string_field(91, "76561198000000001", &mut link_steam);
        encode_varint_field(92, 1, &mut link_steam);
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(
                &sess_admin, &make_quasar_req(char_id, 1, admin_role, "eve.vendors.steam.user.api.admin.LinkRequest", link_steam, now), None, false, now,
            ).status_code,
            STATUS_OK
        );

        let sess_unauth = make_quasar_sess(24001, None, None, 0);
        let mut steam_buf = Vec::new();
        encode_string_field(91, "76561198000000001", &mut steam_buf);
        let r_unauth_steam = ctx.quasar_gateway.handle_public_request(
            &sess_unauth, &make_quasar_req(0, 0, 0, "eve_public.auth.steam.AuthenticateSteamRequest", steam_buf.clone(), now), None, false, now,
        );
        assert_ne!(r_unauth_steam.status_code, STATUS_OK);

        let r_u1_steam = ctx.quasar_gateway.handle_public_request(
            &sess_u1, &make_quasar_req(char_id, 1, 0, "eve_public.auth.steam.AuthenticateSteamRequest", steam_buf.clone(), now), None, false, now,
        );
        assert_eq!(r_u1_steam.status_code, STATUS_OK);

        let sess_u2 = make_quasar_sess(24002, Some(2), Some(90_000_002), 0);
        let r_u2_hijack = ctx.quasar_gateway.handle_public_request(
            &sess_u2, &make_quasar_req(90_000_002, 2, 0, "eve_public.auth.steam.AuthenticateSteamRequest", steam_buf, now), None, false, now,
        );
        assert_ne!(r_u2_hijack.status_code, STATUS_OK);

        // #246: PurchaseRequest for Omega offer (category_id == 1) with recipient on User 2 activates Omega on User 2
        ctx.db.with_mut_snapshot(|snap| {
            let mut catalog = eve_server::services::quasar_payment_store_types::read_store_catalog(snap);
            catalog.offers.push(eve_server::services::quasar_payment_store_types::StoreOffer {
                id: 9246,
                name: "1 Month Omega".into(),
                description: "Omega time".into(),
                category_id: 1,
                price_plex: 500,
                product_ids: vec![],
                targeted: false,
                price_cents: None,
                currency: None,
            });
            eve_server::services::quasar_payment_store_types::save_store_catalog(snap, &catalog);
        });
        let mut offer_id_msg = Vec::new();
        encode_string_field(1, "9246", &mut offer_id_msg);
        let mut omega_purch = Vec::new();
        encode_bytes_field(1, &offer_id_msg, &mut omega_purch);
        encode_varint_field(2, 1, &mut omega_purch);
        let r_omega = ctx.quasar_gateway.handle_public_request(
            &sess_u1, &make_quasar_req(char_id, 1, 0, "eve_public.plex.vault.api.PurchaseRequest", omega_purch, now), None, false, now,
        );
        assert_eq!(r_omega.status_code, STATUS_OK);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters.get(&char_id).unwrap().clone_grade, 2);
        });

        // #247: Entitlement DirectDelivery enforces finite capacity; non-admin DeleteRequest blocks redeemed entitlement
        let (mut skin_hex_msg, mut skin_unit, mut create_unit, mut create_asset_buf) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_string_field(1, "skin_r5_247", &mut skin_hex_msg);
        encode_bytes_field(1, &skin_hex_msg, &mut skin_unit);
        encode_bytes_field(10, &skin_unit, &mut create_unit);
        encode_bytes_field(4, &create_unit, &mut create_asset_buf);
        encode_varint_field(6, 2, &mut create_asset_buf); // finite capacity = 2
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(
                &sess_admin, &make_quasar_req(char_id, 1, admin_role, "eve.assetholding.asset.api.CreateRequest", create_asset_buf, now), None, false, now,
            ).status_code,
            STATUS_OK
        );
        let mut asset_uuid = [0u8; 16];
        asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
        asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());

        let (mut h_inner, mut h_msg, mut a_msg, mut g_msg, mut ent_id) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 90_000_002, &mut h_inner);
        encode_bytes_field(1, &h_inner, &mut h_msg);
        encode_bytes_field(1, &asset_uuid, &mut a_msg);
        encode_string_field(1, "default", &mut g_msg);
        encode_bytes_field(2, b"GROUP_R5_247_001", &mut g_msg);
        encode_bytes_field(1, &h_msg, &mut ent_id);
        encode_bytes_field(2, &a_msg, &mut ent_id);
        encode_bytes_field(3, &g_msg, &mut ent_id);

        let (mut dd_over, mut dd_entry_over) = (Vec::new(), Vec::new());
        encode_bytes_field(2, &ent_id, &mut dd_entry_over);
        encode_varint_field(10, 3, &mut dd_entry_over);
        encode_bytes_field(2, &dd_entry_over, &mut dd_over);
        let r_dd_over = ctx.quasar_gateway.handle_public_request(
            &sess_admin, &make_quasar_req(char_id, 1, admin_role, "eve.assetholding.entitlement.api.DirectDeliveryRequest", dd_over, now), None, false, now,
        );
        assert_ne!(r_dd_over.status_code, STATUS_OK);

        let (mut dd_ok, mut dd_entry_ok) = (Vec::new(), Vec::new());
        encode_bytes_field(2, &ent_id, &mut dd_entry_ok);
        encode_varint_field(10, 1, &mut dd_entry_ok);
        encode_bytes_field(2, &dd_entry_ok, &mut dd_ok);
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(
                &sess_admin, &make_quasar_req(char_id, 1, admin_role, "eve.assetholding.entitlement.api.DirectDeliveryRequest", dd_ok, now), None, false, now,
            ).status_code,
            STATUS_OK
        );
        let mut redeem_msg = Vec::new();
        encode_bytes_field(1, &ent_id, &mut redeem_msg);
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(
                &sess_u2, &make_quasar_req(90_000_002, 2, 0, "eve_public.assetholding.entitlement.api.RedeemRequest", redeem_msg.clone(), now), None, false, now,
            ).status_code,
            STATUS_OK
        );
        let r_del_nonadmin = ctx.quasar_gateway.handle_public_request(
            &sess_u2, &make_quasar_req(90_000_002, 2, 0, "eve.assetholding.entitlement.api.DeleteRequest", redeem_msg, now), None, false, now,
        );
        assert_ne!(r_del_nonadmin.status_code, STATUS_OK);
    }

    #[tokio::test]
    async fn test_r5_241_242_248_249_250_corp_wallet_sync_lp_tax_goal_rewards_industry_and_esi() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let now = ctx.now_unix();
        let char_id = 90_000_001_i64;
        let corp_id = 98_241_001_i64;
        let mut s1 = SessionState::new(24101);
        login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        let kw = HashMap::new();
        ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
        s1.corporation_id = Some(corp_id);
        s1.corp_role = u64::MAX;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.corporation_id = corp_id; }
            snap.skills.entry(char_id).or_default().insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
            let mut corp = CorporationRecord {
                corporation_id: corp_id, name: "R5 Econ Corp".into(), ticker: "R5EC".into(), ceo_id: char_id,
                alliance_id: None, tax_rate: 0.10, master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([char_id]), ..Default::default()
            };
            corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            snap.corporations.insert(corp_id, corp);
        });

        // #241 & #248: Corp goal CreateRequest syncs division 1 & master_wallet and uses proportional per-unit reward
        let sess_q = make_quasar_sess(24801, Some(1), Some(char_id), 0);
        let (mut create_goal, mut curr_b, mut isk_b, mut pool_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_string_field(1, "Small Pool Goal", &mut create_goal);
        encode_varint_field(3, 100, &mut create_goal);
        encode_varint_field(1, eve_proto::quasar::encode_zigzag64(10_000), &mut curr_b);
        encode_bytes_field(1, &curr_b, &mut isk_b);
        encode_bytes_field(2, &isk_b, &mut pool_b);
        encode_bytes_field(8, &pool_b, &mut create_goal);
        let r_goal = ctx.quasar_gateway.handle_public_request(
            &sess_q, &make_quasar_req(char_id, 1, 0, "eve_public.goal.api.CreateRequest", create_goal, now), None, false, now,
        );
        assert_eq!(r_goal.status_code, STATUS_OK);
        let cg_wm = eve_proto::quasar::WireMessage::decode(&r_goal.payload.unwrap().value).unwrap();
        let goal_id_bytes = cg_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();
        ctx.db.with_snapshot(|snap| {
            let corp = snap.corporations.get(&corp_id).unwrap();
            assert_eq!(corp.master_wallet, IskCents::from_isk(990_000));
            assert_eq!(corp.division_balance(1), IskCents::from_isk(990_000));
        });
        eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
            &ctx, char_id, 100_000_000_101, 30_000_142, InventoryFlag::Cargo, 1230, 10,
        );
        let bal_before_redeem = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.0);
        let mut redeem_goal = Vec::new();
        encode_bytes_field(1, &goal_id_bytes, &mut redeem_goal);
        assert_eq!(
            ctx.quasar_gateway.handle_public_request(
                &sess_q, &make_quasar_req(char_id, 1, 0, "eve_public.corporationgoal.api.RedeemMyRewardsRequest", redeem_goal, now), None, false, now,
            ).status_code,
            STATUS_OK
        );
        let bal_after_redeem = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.0);
        assert_eq!(bal_after_redeem - bal_before_redeem, IskCents::from_isk(1_000).0);

        // #242: record_lp_reward_log filters journal by character_id and prevents double-taxation
        ctx.db.with_mut_snapshot(|snap| {
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: 880_001, character_id: char_id, entry_type: "agent_mission_reward".into(),
                amount: IskCents::from_isk(200_000), balance_after: IskCents::from_isk(10_000_000), reason: "Mission reward".into(),
            });
            for i in 0..5 {
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: 880_010 + i, character_id: 90_000_002, entry_type: "MarketEscrow".into(),
                    amount: IskCents::from_isk(-1_000), balance_after: IskCents::from_isk(1_000_000), reason: "Other char".into(),
                });
            }
        });
        let corp_before_m1 = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().division_balance(1).0);
        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::lp_store::record_lp_reward_log(snap, char_id, 1_000_125, 100, "Mission completion".to_string(), now);
        });
        let corp_after_m1 = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().division_balance(1).0);
        assert_eq!(corp_after_m1 - corp_before_m1, IskCents::from_isk(20_000).0);
        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::lp_store::record_lp_reward_log(snap, char_id, 1_000_125, 100, "Mission completion".to_string(), now + 5);
        });
        let corp_after_m2 = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().division_balance(1).0);
        assert_eq!(corp_after_m2, corp_after_m1);

        // #249 & #250: Corporate industry output flag + ESI corp jobs & contract isolation
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(700_249, InventoryItemRecord {
                item_id: 700_249, type_id: 691, owner_id: corp_id, location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG3, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        });
        let mut copy_kw = HashMap::new();
        copy_kw.insert("blueprintID".into(), PyRep::Integer(700_249));
        copy_kw.insert("activityID".into(), PyRep::Integer(5));
        copy_kw.insert("runs".into(), PyRep::Integer(1));
        copy_kw.insert("useCorp".into(), PyRep::Bool(true));
        let copy_res = ctx.dispatch_rpc(&mut s1, "industryManager", None, "InstallJob", &[PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(700_249)),
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(5)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ])], &copy_kw, None).await.expect("InstallJob corp copy");
        let copy_jid = kv_i64(&copy_res, "jobID");
        ctx.db.with_mut_snapshot(|snap| {
            snap.industry_jobs.get_mut(&copy_jid).unwrap().start_unix = now.saturating_sub(100_000);
        });
        ctx.dispatch_rpc(&mut s1, "industryManager", None, "CompleteJob", &[PyRep::Integer(copy_jid)], &HashMap::new(), None).await.expect("CompleteJob corp copy");
        ctx.db.with_snapshot(|snap| {
            let bpc = snap.items.values().find(|i| i.owner_id == corp_id && i.type_id == 691 && i.is_blueprint_copy).expect("Output BPC");
            assert_eq!(bpc.flag, InventoryFlag::CorpSAG3);
        });

        // #250: ESI corporation contracts must exclude historical for_corp contract from a previous corporation
        ctx.db.with_mut_snapshot(|snap| {
            snap.contracts.insert(700_250, ContractRecord {
                contract_id: 700_250, issuer_id: char_id, issuer_corp_id: 98_999_999, assignee_id: 0, acceptor_id: None,
                contract_type: "ItemExchange".into(), status: "Outstanding".into(), title: "Old Corp Contract".into(),
                for_corp: true, availability: "Public".into(), start_station_id: 60_003_760, end_station_id: 60_003_760,
                date_issued_unix: now, expiration_unix: now + 86_400, num_days: 1, price: IskCents::from_isk(100_000),
                reward: IskCents(0), collateral: IskCents(0), buyout: IskCents(0), item_ids: vec![],
                requested_items: vec![], current_bid: IskCents(0), current_bidder_id: None, bid_history: vec![],
            });
        });
        let router = EsiRouter::new(Arc::clone(&ctx));
        let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-contracts.read_character_contracts.v1", "esi-contracts.read_corporation_contracts.v1"]);
        let corp_contracts_resp = router.handle_request(
            &EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_id}/contracts/")).with_header("Authorization", &format!("Bearer {token}")),
        );
        assert_eq!(corp_contracts_resp.status_code, 200);
        let corp_contracts: serde_json::Value = serde_json::from_slice(&corp_contracts_resp.body).unwrap();
        assert!(!corp_contracts.as_array().unwrap().iter().any(|c| c["contract_id"] == 700_250));
        let token2 = issue_token_with_scopes(&ctx, 90_000_002, vec!["esi-contracts.read_character_contracts.v1"]);
        let char_non_party_resp = router.handle_request(
            &EsiHttpRequest::new("GET", "/v1/characters/90000002/contracts/700250/items/").with_header("Authorization", &format!("Bearer {token2}")),
        );
        assert_eq!(char_non_party_resp.status_code, 403);
        let pub_item_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/contracts/public/items/700250/"));
        assert_eq!(pub_item_resp.status_code, 200);
    }
}
