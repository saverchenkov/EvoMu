//! Security regression and exploit tests for Round-6 Track 4 (`#399` R6-T4-01, `#400` R6-T4-02, `#401` R6-T4-03).

#![forbid(unsafe_code)]

use crate::esi::economy_routes::dispatch_economy_route;
use crate::esi::sample_routes::RouteExecutionResult;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_OK, STATUS_UNAUTHORIZED,
};
use crate::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection};
use crate::router::ServerContext;
use bytes::{Buf, BytesMut};
use eve_db::accounts::{hmac_sha256, sha256_bytes};
use eve_db::{
    base64url_encode_no_pad, service_roles, ContractRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MarketOrderMeta, MarketOrderRecord, OwnerNoteRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, EvePublicEvent,
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use http::{Request, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

trait TestRouterExt {
    fn dispatch(&self, ctx: &AuthoritativeContext, req: &GoogleAny) -> Result<GoogleAny, (u32, String)>;
}

impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(&self, ctx: &AuthoritativeContext, req: &GoogleAny) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self.handlers.read().unwrap().get(norm).cloned().expect("handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value));
        handler(ctx, req, &dyn_msg)
    }
}

fn encode_id_req(type_name: &str, id: u64) -> GoogleAny {
    let mut id_msg = Vec::new();
    encode_varint_field(1, id, &mut id_msg);
    let mut req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut req);
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
}

fn make_auth_ctx(uid: u64, cid: i64, corp_id: i64, now_u: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(uid as i32),
        Some(cid as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1; 16],
        now_u,
    )
}

fn rpc_session(uid: u64, cid: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(77_000 + uid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(10_000_002);
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

fn sign_test_hs256_jwt(header_json: &str, payload_json: &str, secret: &[u8]) -> String {
    let signing_input = format!(
        "{}.{}",
        base64url_encode_no_pad(header_json.as_bytes()),
        base64url_encode_no_pad(payload_json.as_bytes())
    );
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", base64url_encode_no_pad(&sig))
}

fn cert_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: false,
        version: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn r6_t4_01_quasar_grpc_rejects_dev_jwt_forgeries_gm_elevation_and_impersonation() {
    let ctx = Arc::new(ServerContext::new());
    let now_u = ctx.now_unix();
    let attacker_uid = ctx.db.provision_player_account("r6_t4_attacker", "pass_a");
    let victim_uid = ctx.db.provision_player_account("r6_t4_victim", "pass_v");
    let gm_uid = ctx.db.provision_player_account("r6_t4_gm", "pass_gm");
    let gm_cid = 91_000_103i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(gm_acc) = snap.accounts.get_mut("r6_t4_gm") {
            gm_acc.role |= service_roles::ROLE_GM_MASK;
        }
        for (cid, name, uid) in [
            (91_000_101, "Attacker", attacker_uid),
            (91_000_102, "Victim", victim_uid),
            (gm_cid, "GMUser", gm_uid),
        ] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.user_id = uid;
            snap.characters.insert(cid, ch);
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection(sock, c).await;
            });
        }
    });

    let tcp = TcpStream::connect(addr).await.expect("connect");
    let (mut client, conn) = h2::client::handshake(tcp).await.expect("h2");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // 1. Hardcoded "dev" secret, alg="none", missing exp, and expired exp MUST return gRPC 16
    let forged_dev = sign_test_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{victim_uid}","exp":{}}}"#, now_u + 3600),
        b"dev",
    );
    let alg_none = sign_test_hs256_jwt(
        r#"{"alg":"none","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{victim_uid}","exp":{}}}"#, now_u + 3600),
        b"",
    );
    let no_exp = sign_test_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{victim_uid}"}}"#),
        b"dev",
    );
    let expired_dev = sign_test_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{victim_uid}","exp":{}}}"#, now_u.saturating_sub(10)),
        b"dev",
    );

    for bad_token in [&forged_dev, &alg_none, &no_exp, &expired_dev] {
        for path in [
            "/eve_public.gateway.Requests/Send",
            "/eve_public.gateway.Notices/Consume",
            "/eve_public.gateway.Events/Publish",
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(format!("http://127.0.0.1{path}"))
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {bad_token}"))
                .body(())
                .unwrap();
            let (rsp_fut, _send) = client.send_request(req, true).unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(
                rsp.headers().get("grpc-status").and_then(|v| v.to_str().ok()),
                Some("16"),
                "Forged token must be rejected with UNAUTHENTICATED on {path}"
            );
        }
    }

    // 2. Local-dev token for a GM account MUST NOT inherit ROLE_GM_MASK over Quasar gRPC
    let gm_dev_jwt = ctx
        .db
        .issue_gateway_dev_token(gm_uid, Some(gm_cid), "tranquility", now_u + 3600, now_u)
        .expect("issue_gateway_dev_token");
    let mut gm_ctx = make_auth_ctx(gm_uid, gm_cid, 1_000_009, now_u);
    gm_ctx.service_role_mask = u64::MAX;
    let gm_only_req = EvePublicRequest {
        authoritative_context: Some(gm_ctx),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.user.api.GrantRequest"),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    let req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {gm_dev_jwt}"))
        .body(())
        .unwrap();
    let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
    send_body.send_data(frame_grpc_message(&gm_only_req.encode_proto()), true).unwrap();
    let rsp = rsp_fut.await.unwrap();
    assert_eq!(rsp.status(), StatusCode::OK);
    let frame = rsp.into_body().data().await.unwrap().unwrap();
    let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
    let gateway_rsp = EvePublicResponse::decode_proto(&frame[5..5 + len]).unwrap();
    assert_eq!(gateway_rsp.status_code, STATUS_FORBIDDEN);

    // 3. Cross-character spoofing on Requests/Send -> 403; Events/Publish -> gRPC 7; legitimate caller -> 200
    let attacker_jwt = ctx
        .db
        .issue_gateway_dev_token(attacker_uid, Some(91_000_101), "tranquility", now_u + 3600, now_u)
        .expect("attacker dev jwt");
    let verifier = "pkce_verifier_secret_0123456789abcdef";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let code = ctx
        .db
        .authorize_pkce(
            "r6_t4_attacker",
            "pass_a",
            91_000_101,
            "evomu",
            "http://127.0.0.1/cb",
            Vec::new(),
            &challenge,
            "S256",
            now_u,
        )
        .unwrap();
    let sso_jwt = ctx.db.exchange_pkce_code(&code, "evomu", verifier, now_u).unwrap().access_token;

    let spoof_ctx = make_auth_ctx(attacker_uid, 91_000_102, 1_000_009, now_u);
    let legit_ctx = make_auth_ctx(attacker_uid, 91_000_101, 1_000_009, now_u);
    let spoof_req = EvePublicRequest {
        authoritative_context: Some(spoof_ctx.clone()),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
            value: vec![0x08, attacker_uid as u8],
        }),
        ..Default::default()
    };
    let legit_req = EvePublicRequest {
        authoritative_context: Some(legit_ctx),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
            value: vec![0x08, attacker_uid as u8],
        }),
        ..Default::default()
    };

    for tok in [&attacker_jwt, &sso_jwt] {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {tok}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        send_body.send_data(frame_grpc_message(&spoof_req.encode_proto()), false).unwrap();
        send_body.send_data(frame_grpc_message(&legit_req.encode_proto()), true).unwrap();
        let mut body = rsp_fut.await.unwrap().into_body();
        let mut buf = BytesMut::new();
        while let Some(chunk) = body.data().await {
            buf.extend_from_slice(&chunk.unwrap());
        }
        let len1 = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        buf.advance(5);
        assert_eq!(EvePublicResponse::decode_proto(&buf.split_to(len1)).unwrap().status_code, STATUS_FORBIDDEN);
        let len2 = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        buf.advance(5);
        assert_eq!(EvePublicResponse::decode_proto(&buf.split_to(len2)).unwrap().status_code, STATUS_OK);
    }

    let spoof_ev = EvePublicEvent {
        authoritative_context: Some(spoof_ctx),
        ..Default::default()
    };
    let req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {attacker_jwt}"))
        .body(())
        .unwrap();
    let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
    send_body.send_data(frame_grpc_message(&spoof_ev.encode_proto()), true).unwrap();
    let mut body = rsp_fut.await.unwrap().into_body();
    let _ = body.data().await;
    let trailers = body.trailers().await.unwrap().unwrap();
    assert_eq!(trailers.get("grpc-status").and_then(|v| v.to_str().ok()), Some("7"));
}

#[tokio::test]
async fn r6_t4_02_certificate_activation_enforces_locks_escrow_trades_location_and_replay() {
    let ctx = ServerContext::new();
    let uid = ctx.db.provision_player_account("r6_t4_mct_user", "pass_m");
    let char_id = 91_000_201i64;
    let corp_id = 98_000_201i64;
    let jita = 60_003_760i64;
    let amarr = 60_008_494i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = char_id;
        ch.name = "CertTester".into();
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.station_id = Some(jita);
        ch.solar_system_id = 30_000_142;
        snap.characters.insert(char_id, ch);

        for rec in [
            cert_item(910_001, 33027, char_id, jita, InventoryFlag::Locked),
            cert_item(910_002, 48582, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_003, 28662, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_004, 34189, char_id, jita, InventoryFlag::Hangar),
            cert_item(910_005, 33027, char_id, amarr, InventoryFlag::Hangar),
            cert_item(910_006, 28662, char_id, 910_999, InventoryFlag::Unlocked),
            cert_item(910_007, 33027, corp_id, jita, InventoryFlag::Hangar),
        ] {
            snap.items.insert(rec.item_id, rec);
        }
        snap.contracts.insert(
            910_100,
            ContractRecord {
                contract_id: 910_100,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                item_ids: vec![910_002, 910_003],
                expiration_unix: ctx.now_unix() + 86_400,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            -7_300_000_000_999,
            OwnerNoteRecord {
                note_id: -7_300_000_000_999,
                character_id: -73,
                item_id: 999,
                label: "trade:999".into(),
                note: serde_json::json!({
                    "trade_id": 999,
                    "station_id": jita,
                    "char_a": char_id,
                    "char_b": 90_000_002,
                    "items_a": [[910_004, 1]],
                    "items_b": [],
                    "money_a_cents": 0,
                    "money_b_cents": 0,
                    "accept_a": false,
                    "accept_b": false,
                    "completed": false,
                    "cancelled": false
                })
                .to_string(),
                timestamp_unix: ctx.now_unix(),
            },
        );
    });

    let empty_kw = HashMap::new();
    let mut sess = rpc_session(uid, char_id, corp_id, Some(jita));

    for method in [
        "ActivateMultiTraining",
        "ActivateSoulboundMultiTraining",
        "ActivateCharacterReSculpt",
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, "userSvc", None, method, &[], &empty_kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "UserError", "{method} must reject non-consumable certificates");
    }
    ctx.db.with_snapshot(|snap| {
        for iid in 910_001..=910_007 {
            assert!(snap.items.contains_key(&iid), "Locked item {iid} must remain intact");
        }
    });

    // Unlocked certificates in Jita hangar must still be rejected while undocked in space!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_008, cert_item(910_008, 33027, char_id, jita, InventoryFlag::Hangar));
        snap.items.insert(910_009, cert_item(910_009, 33502, char_id, jita, InventoryFlag::Hangar));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    let mut space_sess = rpc_session(uid, char_id, corp_id, None);
    for method in ["ActivateMultiTraining", "ActivateCharacterReSculpt"] {
        assert!(
            ctx.dispatch_rpc(&mut space_sess, "userSvc", None, method, &[], &empty_kw, None)
                .await
                .is_err(),
            "{method} must reject hangar certificate when character is undocked in space"
        );
    }

    // Dock back at Jita: each single certificate succeeds ONCE and replay immediately fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(jita);
        }
    });
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateMultiTraining", &[], &empty_kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateMultiTraining", &[], &empty_kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateCharacterReSculpt", &[], &empty_kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut sess, "userSvc", None, "ActivateCharacterReSculpt", &[], &empty_kw, None).await.is_err());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&910_008));
        assert!(!snap.items.contains_key(&910_009));
    });
}

#[tokio::test]
async fn r6_t4_03_quasar_market_acl_redaction_and_esi_public_assigned_contracts() {
    let ctx = ServerContext::new();
    let owner_corp = 98_000_301i64;
    let outsider_corp = 98_000_302i64;
    let member_cid = 91_000_301i64;
    let outsider_cid = 91_000_302i64;
    let struct_id = 1_040_000_000_301i64;
    let jita = 60_003_760i64;
    let now_u = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp,
            CorporationRecord {
                corporation_id: owner_corp,
                name: "OwnerCorp".into(),
                ticker: "OWNR".into(),
                ceo_id: member_cid,
                ..Default::default()
            },
        );
        for (cid, corp) in [(member_cid, owner_corp), (outsider_cid, outsider_corp)] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.corporation_id = corp;
            ch.user_id = cid as u64;
            snap.characters.insert(cid, ch);
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35833,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                name: "Private Market Fortizar".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            910_350,
            cert_item(910_350, 35_892, owner_corp, struct_id, InventoryFlag::ServiceSlot0),
        );
        snap.module_online_states.insert(910_350, true);
        snap.market_orders.insert(
            88_001,
            MarketOrderRecord {
                order_id: 88_001,
                owner_id: member_cid,
                station_id: struct_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(1_000),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500_000),
            },
        );
        snap.market_order_meta.insert(
            88_001,
            MarketOrderMeta {
                issued_unix: 1_765_432_100,
                duration_days: 14,
                range: "station".into(),
                issued_by: Some(member_cid),
                wallet_division: Some(1),
                is_corporation: false,
                state: Some("Open".into()),
            },
        );
        for (cid, assignee, avail) in [
            (92_001, 0, "Public"),
            (92_002, outsider_cid, "Public"),
            (92_003, outsider_cid, "Personal"),
        ] {
            snap.contracts.insert(
                cid,
                ContractRecord {
                    contract_id: cid,
                    issuer_id: member_cid,
                    issuer_corp_id: owner_corp,
                    assignee_id: assignee,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    availability: avail.into(),
                    status: "Outstanding".into(),
                    title: format!("Contract {cid}"),
                    price: IskCents::from_isk(10_000),
                    start_station_id: jita,
                    end_station_id: jita,
                    date_issued_unix: now_u,
                    num_days: 7,
                    expiration_unix: now_u + 86_400,
                    item_ids: vec![910_350],
                    bid_history: vec![(outsider_cid, IskCents::from_isk(12_000), now_u)],
                    ..Default::default()
                },
            );
        }
    });

    // Part A: Quasar Structure Market ACL & Order Field Redaction
    let req_any = encode_id_req("eve.market.orders.GetOpenInStructureRequest", struct_id as u64);
    let anon_ctx = AuthoritativeContext::default();
    let outsider_ctx = make_auth_ctx(outsider_cid as u64, outsider_cid, outsider_corp, now_u);
    let member_ctx = make_auth_ctx(member_cid as u64, member_cid, owner_corp, now_u);

    assert_eq!(ctx.quasar_gateway.dispatch(&anon_ctx, &req_any).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(ctx.quasar_gateway.dispatch(&outsider_ctx, &req_any).unwrap_err().0, STATUS_FORBIDDEN);
    let ok_struct = ctx.quasar_gateway.dispatch(&member_ctx, &req_any).unwrap();
    let pub_wm = WireMessage::decode(&ok_struct.value).unwrap();
    let comp_wm = WireMessage::decode(pub_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let buy_entry_wm = WireMessage::decode(comp_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let pub_attrs_bytes = buy_entry_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap();
    let pub_attrs = WireMessage::decode(pub_attrs_bytes).unwrap();
    assert!(
        pub_attrs.fields.iter().all(|f| f.field_number != 1 && f.field_number != 15),
        "Public market order response must omit issuer_character (1) and escrow (15)"
    );
    assert_eq!(
        pub_attrs.fields.iter().find(|f| f.field_number == 6).and_then(|f| f.value.as_varint()),
        Some(14)
    );

    // Private GetOpenForCharacterRequest retains field 1 (issuer_character) & field 15 (escrow)
    let char_ord_any = encode_id_req("eve.market.orders.GetOpenForCharacterRequest", member_cid as u64);
    let priv_rsp = ctx.quasar_gateway.dispatch(&member_ctx, &char_ord_any).unwrap();
    let priv_comp = WireMessage::decode(WireMessage::decode(&priv_rsp.value).unwrap().fields[0].value.as_bytes().unwrap()).unwrap();
    let priv_buy = WireMessage::decode(priv_comp.fields[0].value.as_bytes().unwrap()).unwrap();
    let priv_attrs = WireMessage::decode(priv_buy.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert!(priv_attrs.fields.iter().any(|f| f.field_number == 1));
    assert!(priv_attrs.fields.iter().any(|f| f.field_number == 15));

    // Part B: ESI /contracts/public/* hides assignee_id != 0 and non-Public contracts, items, and bids
    let empty_q = HashMap::new();
    let reg_params = HashMap::from([("region_id".into(), "10000002".into())]);
    let pub_list: RouteExecutionResult =
        dispatch_economy_route(&ctx, "GET", "/contracts/public/{region_id}", &reg_params, &empty_q, None, 1).unwrap();
    assert_eq!(pub_list.status_code, 200);
    let ids: Vec<i64> = pub_list
        .body
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row.get("contract_id").and_then(Value::as_i64))
        .collect();
    assert!(ids.contains(&92_001));
    assert!(!ids.contains(&92_002));
    assert!(!ids.contains(&92_003));

    for (path, cid, expected) in [
        ("/contracts/public/items/{contract_id}", "92001", 200),
        ("/contracts/public/items/{contract_id}", "92002", 404),
        ("/contracts/public/items/{contract_id}", "92003", 404),
        ("/contracts/public/bids/{contract_id}", "92001", 200),
        ("/contracts/public/bids/{contract_id}", "92002", 404),
        ("/contracts/public/bids/{contract_id}", "92003", 404),
    ] {
        let p = HashMap::from([("contract_id".into(), cid.into())]);
        let r = dispatch_economy_route(&ctx, "GET", path, &p, &empty_q, None, 1).unwrap();
        assert_eq!(r.status_code, expected, "{path} for {cid}");
    }

    // Part C: CreateContract with assigneeID > 0 normalizes default availability to "Personal"
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&member_cid) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.items.insert(
            910_351,
            InventoryItemRecord {
                type_id: 34,
                quantity: 10,
                ..cert_item(910_351, 34, member_cid, jita, InventoryFlag::Hangar)
            },
        );
    });
    let mut issuer_sess = rpc_session(member_cid as u64, member_cid, owner_corp, Some(jita));
    let contract_payload = PyRep::keyval([
        ("contractType", PyRep::Integer(3)),
        ("assigneeID", PyRep::Integer(outsider_cid)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(jita)),
        ("endStationID", PyRep::Integer(jita)),
        ("reward", PyRep::Real(5_000.0)),
        ("collateral", PyRep::Real(1_000.0)),
        ("items", PyRep::List(vec![PyRep::Integer(910_351)])),
    ]);
    let created_rep = ctx
        .dispatch_rpc(
            &mut issuer_sess,
            "contractProxy",
            None,
            "CreateContract",
            &[contract_payload],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    let created_cid = created_rep
        .as_i64()
        .or_else(|| created_rep.dict_get("contractID").and_then(|v| v.as_i64()))
        .expect("created contractID");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&created_cid].assignee_id, outsider_cid);
        assert_eq!(snap.contracts[&created_cid].availability, "Personal");
    });
}
