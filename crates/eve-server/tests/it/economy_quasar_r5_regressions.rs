//! Round-5 Track 4 Regression Tests (#238 through #250):
//! Economy, Contracts, Industry, Quasar, ESI & SQLite Persistence.

#![forbid(unsafe_code)]

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
    let pub_item_resp = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000002/contracts/700250/items/").with_header("Authorization", &format!("Bearer {token2}")),
    );
    assert_eq!(pub_item_resp.status_code, 200);
}
