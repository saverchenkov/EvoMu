//! Security regression and exploit tests for Round-6 Track 4 (`#399` R6-T4-01, `#400` R6-T4-02, `#401` R6-T4-03).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_OK,
};
use crate::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection};
use crate::router::ServerContext;
use bytes::{Buf, BytesMut};
use eve_db::accounts::{hmac_sha256, sha256_bytes};
use eve_db::{
    base64url_encode_no_pad, service_roles, InventoryFlag,
    InventoryItemRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, EvePublicEvent,
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use http::{Request, StatusCode};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

pub(super) trait TestRouterExt {
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

pub(super) fn encode_id_req(type_name: &str, id: u64) -> GoogleAny {
    let mut id_msg = Vec::new();
    encode_varint_field(1, id, &mut id_msg);
    let mut req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut req);
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
}

pub(super) fn make_auth_ctx(uid: u64, cid: i64, corp_id: i64, now_u: u64) -> AuthoritativeContext {
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

pub(super) fn rpc_session(uid: u64, cid: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
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

pub(super) fn sign_test_hs256_jwt(header_json: &str, payload_json: &str, secret: &[u8]) -> String {
    let signing_input = format!(
        "{}.{}",
        base64url_encode_no_pad(header_json.as_bytes()),
        base64url_encode_no_pad(payload_json.as_bytes())
    );
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", base64url_encode_no_pad(&sig))
}

pub(super) fn cert_item(
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
            ch.corporation_id = 1_000_009;
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
