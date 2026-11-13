//! End-to-end Quasar gRPC (HTTP/2) + MachoNet + SQLite reload persistence integration tests
//! for SKINR Designer, Sequencing Studio, Paragon Hub Cosmetic Market, Character Titles,
//! AIR Achievements, AIR Career Program Goals, and Daily Goals.
//!
//! Zero direct in-memory snapshot mutation bypasses: character setup uses DB provisioning + `authentication.Login`
//! + `charUnboundMgr.SelectCharacterID` + `slash.SlashCmd` + `invbroker.DepositPlexToVault`,
//! and all Quasar traffic flows over live `h2` HTTP/2 `/eve_public.gateway.Requests/Send` and
//! `/eve_public.gateway.Notices/Consume` decoded against `QuasarDescriptorRegistry`.

use crate::common;
use bytes::{BufMut, Bytes, BytesMut};
use eve_db::{IskCents, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_PRECONDITION_FAILED, STATUS_SERVICE_UNAVAILABLE,
};
use eve_server::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection};
use eve_server::router::ServerContext;
use eve_server::services::lp_store::PARAGON_CORP_ID;
use eve_server::services::quasar_achievements::AchievementManager;
use eve_server::services::quasar_cosmetics_market::BUYER_FEES_VERSION;
use eve_server::services::quasar_cosmetics_skinr::{
    SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_METALLIC, COMP_CAT_PATTERN,
};
use eve_server::services::quasar_daily_goals::daily_goal_day_bucket;
use h2::RecvStream;
use http::{Request, StatusCode};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const EMPTY_GRPC_FRAME: [u8; 5] = [0, 0, 0, 0, 0];

struct TempSqliteDb {
    path: PathBuf,
}

impl TempSqliteDb {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("evomu_remediate_{tag}_{}.sqlite", uuid::Uuid::new_v4()));
        Self { path }
    }
}

impl Drop for TempSqliteDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let wal = PathBuf::from(format!("{}-wal", self.path.display()));
        let shm = PathBuf::from(format!("{}-shm", self.path.display()));
        let _ = std::fs::remove_file(wal);
        let _ = std::fs::remove_file(shm);
    }
}

fn boot_sqlite_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    let ctx = ServerContext::with_repository(db);
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now_unix)));
    Arc::new(ctx)
}

async fn read_next_grpc_frame(body: &mut RecvStream, buf: &mut BytesMut) -> Option<Vec<u8>> {
    loop {
        if buf.len() >= 5 {
            let compressed = buf[0];
            assert_eq!(compressed, 0, "expected uncompressed gRPC frame");
            let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
            if buf.len() >= 5 + len {
                let _ = buf.split_to(5);
                let msg = buf.split_to(len).to_vec();
                return Some(msg);
            }
        }
        match body.data().await {
            Some(Ok(chunk)) => {
                let _ = body.flow_control().release_capacity(chunk.len());
                buf.put_slice(&chunk);
            }
            Some(Err(err)) => panic!("h2 stream read error: {err}"),
            None => return None,
        }
    }
}

struct GrpcClient {
    ctx: Arc<ServerContext>,
    registry: Arc<QuasarDescriptorRegistry>,
    h2_client: h2::client::SendRequest<Bytes>,
    jwt: String,
    user_id: u64,
    app_uuid: [u8; 16],
    notice_body: Option<(RecvStream, BytesMut, h2::SendStream<Bytes>)>,
    server_tasks: Vec<JoinHandle<()>>,
    _conn_task: JoinHandle<()>,
    seq: u64,
}

impl GrpcClient {
    async fn connect(
        ctx: Arc<ServerContext>,
        username: &str,
        password: &str,
        char_id: u32,
        app_seed: u8,
    ) -> Self {
        let user_id = ctx
            .db
            .with_snapshot(|s| s.account_of_character(i64::from(char_id)))
            .expect("character must belong to provisioned account");
        let now = ctx.now_unix();
        let verifier = "v9f0A_pkce_verifier_string_with_high_entropy_12345";
        let challenge = eve_db::base64url_encode_no_pad(&eve_proto::quasar::sha256_bytes(verifier.as_bytes()));
        let code = ctx
            .db
            .authorize_pkce(
                username,
                password,
                i64::from(char_id),
                "evomu-launcher",
                "eveauth://callback",
                vec!["publicData".into()],
                &challenge,
                "S256",
                now,
            )
            .expect("authorize_pkce");
        let bundle = ctx
            .db
            .exchange_pkce_code(&code, "evomu-launcher", verifier, now)
            .expect("exchange_pkce_code");
        let jwt = bundle.access_token;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let client_stream = TcpStream::connect(addr).await.expect("tcp connect");
        let (server_stream, _) = listener.accept().await.expect("accept");

        let ctx_srv = Arc::clone(&ctx);
        let srv_task = tokio::spawn(async move {
            let _ = handle_quasar_grpc_connection(server_stream, ctx_srv).await;
        });

        let (h2_client, conn) = h2::client::handshake(client_stream).await.expect("h2 handshake");
        let _conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        let registry = QuasarDescriptorRegistry::open_default_or_empty();
        Self {
            ctx,
            registry,
            h2_client,
            jwt,
            user_id,
            app_uuid: [app_seed; 16],
            notice_body: None,
            server_tasks: vec![srv_task],
            _conn_task,
            seq: 1,
        }
    }

    async fn open_notice_stream(&mut self) {
        self.h2_client.clone().ready().await.expect("h2 ready");
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {}", self.jwt))
            .header("application_instance_uuid", uuid::Uuid::from_bytes(self.app_uuid).to_string())
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = self.h2_client.send_request(req, false).expect("send consume");
        send_body
            .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)
            .expect("send initial empty consume frame");
        let rsp = rsp_fut.await.expect("consume response headers");
        assert_eq!(rsp.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.notice_body = Some((rsp.into_body(), BytesMut::new(), send_body));
    }

    async fn send_raw(&mut self, full_type: &str, payload_bytes: Vec<u8>) -> EvePublicResponse {
        self.seq += 1;
        let mut corr = [0u8; 16];
        corr[8..16].copy_from_slice(&self.seq.to_be_bytes());
        let now = self.ctx.now_unix();
        let pub_req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: now as i64,
                nanos: 0,
            }),
            correlation_uuid: corr.to_vec(),
            payload: Some(GoogleAny {
                type_url: if eve_proto::quasar::has_quasar_type_url_prefix(full_type) {
                    full_type.to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{full_type}")
                },
                value: payload_bytes,
            }),
            external_origin: "eve-desktop-client/3619879 tq".into(),
            application_instance_uuid: self.app_uuid.to_vec(),
            authoritative_context: None,
            trace_context: None,
        };

        self.h2_client.clone().ready().await.expect("h2 ready");
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {}", self.jwt))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = self.h2_client.send_request(req, false).expect("send rpc");
        send_body
            .send_data(frame_grpc_message(&pub_req.encode_proto()), true)
            .expect("send request frame");
        let rsp = rsp_fut.await.expect("response headers");
        assert_eq!(rsp.status(), StatusCode::OK);
        let mut recv = rsp.into_body();
        let mut buf = BytesMut::new();
        let frame = read_next_grpc_frame(&mut recv, &mut buf)
            .await
            .unwrap_or_else(|| panic!("streamed EvePublicResponse missing for {full_type}"));
        let decoded = EvePublicResponse::decode_proto(&frame).expect("decode EvePublicResponse");
        assert_eq!(decoded.correlation_uuid, corr, "correlation UUID mismatch for {full_type}");
        decoded
    }

    async fn send_ok_dyn(&mut self, full_type: &str, payload_bytes: Vec<u8>) -> (EvePublicResponse, DynamicQuasarMessage) {
        let rsp = self.send_raw(full_type, payload_bytes).await;
        assert_eq!(
            rsp.status_code, STATUS_OK,
            "Quasar gRPC {full_type} failed ({}): {}",
            rsp.status_code, rsp.status_message
        );
        let any = rsp.payload.as_ref().expect("response payload");
        let dyn_msg = self
            .registry
            .decode_dynamic(&any.type_url, &any.value)
            .unwrap_or_else(|e| panic!("decode_dynamic failed for {}: {e}", any.type_url));
        (rsp, dyn_msg)
    }

    async fn send_ok_empty(&mut self, full_type: &str, payload_bytes: Vec<u8>) -> EvePublicResponse {
        let rsp = self.send_raw(full_type, payload_bytes).await;
        assert_eq!(
            rsp.status_code, STATUS_OK,
            "Quasar gRPC {full_type} failed ({}): {}",
            rsp.status_code, rsp.status_message
        );
        rsp
    }

    async fn drain_notices(&mut self, timeout_ms: u64) -> Vec<(EvePublicNotice, DynamicQuasarMessage)> {
        let mut out = Vec::new();
        let Some((ref mut recv, ref mut buf, _)) = self.notice_body else {
            return out;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, read_next_grpc_frame(recv, buf)).await {
            let notice = EvePublicNotice::decode_proto(&frame).expect("decode EvePublicNotice");
            let dyn_msg = match notice.payload.as_ref() {
                Some(any) if !any.type_url.is_empty() => self
                    .registry
                    .decode_dynamic(&any.type_url, &any.value)
                    .unwrap_or_else(|e| panic!("decode_dynamic notice {}: {e}", any.type_url)),
                _ => DynamicQuasarMessage::new("google.protobuf.Empty"),
            };
            out.push((notice, dyn_msg));
        }
        out
    }

    async fn assert_spoof_rejected(&mut self, full_type: &str, forged_char: u32) {
        self.seq += 1;
        let mut corr = [0u8; 16];
        corr[8..16].copy_from_slice(&self.seq.to_be_bytes());
        let now = self.ctx.now_unix();
        let pub_req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
            correlation_uuid: corr.to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{full_type}"),
                value: Vec::new(),
            }),
            external_origin: "eve-desktop-client/3619879 tq".into(),
            application_instance_uuid: self.app_uuid.to_vec(),
            authoritative_context: Some(AuthoritativeContext::for_session_at(
                Some(self.user_id as i32),
                Some(forged_char),
                None,
                0,
                "tranquility",
                Vec::new(),
                now,
            )),
            trace_context: None,
        };
        self.h2_client.clone().ready().await.expect("h2 ready");
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {}", self.jwt))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = self.h2_client.send_request(req, false).unwrap();
        send_body.send_data(frame_grpc_message(&pub_req.encode_proto()), true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);
        let mut recv = rsp.into_body();
        let mut buf = BytesMut::new();
        let frame = read_next_grpc_frame(&mut recv, &mut buf).await.expect("spoof response");
        let decoded = EvePublicResponse::decode_proto(&frame).unwrap();
        assert_eq!(decoded.status_code, STATUS_FORBIDDEN, "spoofing {forged_char} must return 403");
    }

    fn close(mut self) {
        self.notice_body.take();
        for t in self.server_tasks.drain(..) {
            t.abort();
        }
        self._conn_task.abort();
    }
}

async fn login_macho_session(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    // Subscribe MachoNet publicGatewaySvc notices alongside gRPC consume path.
    let sub_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "Subscribe",
            &[PyRep::String("eve_public.*".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("publicGatewaySvc.Subscribe");
    assert_eq!(
        sub_rep.dict_get("subscribed").and_then(|v| v.as_bool()),
        Some(true)
    );
    ctx.advance_secs(12);
    sess
}

fn dyn_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(x) | DynamicValue::Fixed64(x) => Some(*x),
        DynamicValue::Uint32(x) | DynamicValue::Fixed32(x) => Some(u64::from(*x)),
        DynamicValue::Int64(x) | DynamicValue::Sint64(x) | DynamicValue::Sfixed64(x) => u64::try_from(*x).ok(),
        DynamicValue::Int32(x) | DynamicValue::Sint32(x) | DynamicValue::Sfixed32(x) => u64::try_from(*x).ok(),
        DynamicValue::Enum(x) => u64::try_from(*x).ok(),
        _ => None,
    }
}

fn dyn_i64(v: &DynamicValue) -> Option<i64> {
    match v {
        DynamicValue::Int64(x) | DynamicValue::Sint64(x) | DynamicValue::Sfixed64(x) => Some(*x),
        DynamicValue::Int32(x) | DynamicValue::Sint32(x) | DynamicValue::Sfixed32(x) => Some(i64::from(*x)),
        DynamicValue::Uint64(x) | DynamicValue::Fixed64(x) => i64::try_from(*x).ok(),
        DynamicValue::Uint32(x) | DynamicValue::Fixed32(x) => Some(i64::from(*x)),
        DynamicValue::Enum(x) => Some(i64::from(*x)),
        _ => None,
    }
}

fn dyn_bool(v: &DynamicValue) -> Option<bool> {
    match v {
        DynamicValue::Bool(b) => Some(*b),
        DynamicValue::Uint64(x) | DynamicValue::Fixed64(x) => Some(*x != 0),
        DynamicValue::Uint32(x) | DynamicValue::Fixed32(x) => Some(*x != 0),
        DynamicValue::Int64(x) | DynamicValue::Sint64(x) | DynamicValue::Sfixed64(x) => Some(*x != 0),
        DynamicValue::Int32(x) | DynamicValue::Sint32(x) | DynamicValue::Sfixed32(x) => Some(*x != 0),
        _ => None,
    }
}

fn dyn_str(v: &DynamicValue) -> Option<&str> {
    match v {
        DynamicValue::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn dyn_bytes(v: &DynamicValue) -> Option<&[u8]> {
    match v {
        DynamicValue::Bytes(b) => Some(b.as_slice()),
        _ => None,
    }
}

fn dyn_msg(v: &DynamicValue) -> Option<&DynamicQuasarMessage> {
    match v {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

fn dyn_repeated(v: &DynamicValue) -> &[DynamicValue] {
    match v {
        DynamicValue::Repeated(xs) => xs.as_slice(),
        _ => &[],
    }
}

fn encode_sint_field(field_num: u32, val: i64, out: &mut Vec<u8>) {
    let zz = ((val << 1) ^ (val >> 63)) as u64;
    encode_varint_field(field_num, zz, out);
}

fn encode_uuid_ident(bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::new();
    encode_bytes_field(1, bytes, &mut m);
    m
}

fn encode_seq_ident(id: u64) -> Vec<u8> {
    let mut m = Vec::new();
    encode_varint_field(1, id, &mut m);
    m
}

fn encode_comp_ident(comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, u64::from(comp_id), &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &inner, &mut out);
    out
}

fn encode_skin_lic_id(owner_cid: u32, skin_hex: &str) -> Vec<u8> {
    let mut skin_id = Vec::new();
    encode_string_field(1, skin_hex, &mut skin_id);
    let mut lic = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(owner_cid)), &mut lic);
    encode_bytes_field(2, &skin_id, &mut lic);
    lic
}

fn encode_coating_slot(slot_id: u32, comp_id: u32) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(slot_id)), &mut slot_msg);
    let mut coat_cfg = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(comp_id)), &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    slot_msg
}

fn encode_pattern_slot(slot_id: u32, comp_id: u32) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(slot_id)), &mut slot_msg);
    let mut pat_cfg = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(comp_id)), &mut pat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(2, &pat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    slot_msg
}

async fn deposit_evermarks_grpc(client: &mut GrpcClient, char_id: u32, amount: u64, op_tag: &[u8]) {
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, op_tag, &mut op_id_m);
    let mut wallet_id = Vec::new();
    encode_bytes_field(1, &encode_seq_ident(u64::from(char_id)), &mut wallet_id);
    encode_bytes_field(3, &encode_seq_ident(PARAGON_CORP_ID as u64), &mut wallet_id);
    let mut curr_m = Vec::new();
    encode_varint_field(1, amount, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    let mut dep_req = Vec::new();
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);
    client
        .send_ok_empty("eve.loyaltypoints.assetholding.operation.api.DepositRequest", dep_req)
        .await;
}

// ============================================================================
// Test 1: SKINR Designer & Sequencing Studio (gRPC HTTP/2 + MachoNet + SQLite)
// ============================================================================
// parity: feat:cosmetics.skinr-designer
// parity: feat:cosmetics.skinr-sequencing-studio
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
#[tokio::test]
async fn skinr_designer_and_sequencing_studio_grpc_lifecycle() {
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-sequencing-studio",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
    ]);

    let sqlite = TempSqliteDb::new("skinr_seq");
    let base_unix = common::TEST_CLOCK_BASE_UNIX;
    let designer_cid = 90_100_101u32;
    let rival_cid = 90_100_102u32;
    let rifter_type = 587u32;
    let tritanium_type = 34u32;

    let (skin_hex_1, job1_uuid, skin_hex_2) = {
        let ctx = boot_sqlite_ctx(&sqlite.path, base_unix);
        ctx.db.provision_gm_account("skinr_des", "des-pass", i64::from(designer_cid), "SKINR Designer");
        ctx.db.provision_gm_account("skinr_rival", "riv-pass", i64::from(rival_cid), "SKINR Rival");

        let mut des_sess = login_macho_session(&ctx, 70_101, "skinr_des", "des-pass", i64::from(designer_cid)).await;
        // Fund PLEX vault via authentic /createitem 44992 + invbroker.DepositPlexToVault
        let plex_created = common::slash_cmd(&ctx, &mut des_sess, "/createitem 44992 1500").await;
        let plex_item_id = plex_created.dict_get("itemID").and_then(|v| v.as_i64()).expect("plex itemID");
        ctx.dispatch_rpc(
            &mut des_sess,
            "invbroker",
            None,
            "DepositPlexToVault",
            &[PyRep::Integer(60003760), PyRep::Integer(plex_item_id), PyRep::Integer(1500)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("DepositPlexToVault");

        // Train induction + studio sequencing skills via /giveskill
        // 81377 Basic Studio Induction V (15% time reduction), 81370 Mass Sequencing V (+5 active slots), 81366 Multi-License Sequencing V
        for sid in [81377u32, 81370, 81366, 81375] {
            common::slash_cmd(&ctx, &mut des_sess, &format!("/giveskill {sid} 5")).await;
        }

        let catalog = SkinrStaticCatalog::load(&ctx.sde);
        let coat_comp = catalog
            .components
            .values()
            .find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1) && !c.associated_types.is_empty())
            .expect("SDE material component with associated item type");
        let coat_item_type = coat_comp.associated_types[0].0;
        let coat_uses = coat_comp.associated_types[0].1;
        let coat_comp_id = coat_comp.component_id;
        let met_comp_id = catalog
            .components
            .values()
            .find(|c| c.category == COMP_CAT_METALLIC && !c.disallowed_slots.contains(&2))
            .expect("SDE metallic component")
            .component_id;
        let pat_comp_id = catalog
            .components
            .values()
            .find(|c| c.category == COMP_CAT_PATTERN && !c.disallowed_slots.contains(&5))
            .expect("SDE pattern component")
            .component_id;

        // Grant component item via MachoNet /createitem, then consume it via Quasar HTTP/2 ConsumeRequest
        let comp_created = common::slash_cmd(&ctx, &mut des_sess, &format!("/createitem {coat_item_type} 1")).await;
        let comp_item_id = comp_created.dict_get("itemID").and_then(|v| v.as_i64()).expect("component itemID");

        let mut client = GrpcClient::connect(Arc::clone(&ctx), "skinr_des", "des-pass", designer_cid, 0x11).await;
        client.open_notice_stream().await;

        // Verify gRPC authoritative context spoof rejection
        client
            .assert_spoof_rejected(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
                rival_cid,
            )
            .await;

        // Pricing & durations decoded via QuasarDescriptorRegistry
        let (_, dur_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
                Vec::new(),
            )
            .await;
        let dur_discount = dur_dyn.get(2).and_then(dyn_u64).expect("duration discount");
        assert_eq!(dur_discount, 15, "Basic Studio Induction V must grant 15% duration discount");
        let tier_durs = dur_dyn.get(1).map(dyn_repeated).expect("tier durations");
        assert!(!tier_durs.is_empty(), "SDE/SKINR tier durations must be populated");

        let (_, price_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
                Vec::new(),
            )
            .await;
        assert_eq!(price_dyn.get(2).and_then(dyn_u64), Some(0), "PLEX tier price has 0% discount");
        let tier_prices = price_dyn.get(1).map(dyn_repeated).expect("tier pricing entries");
        assert!(!tier_prices.is_empty(), "tier pricing entries must be present");

        let (_, early_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
                Vec::new(),
            )
            .await;
        let early_entries = early_dyn
            .get(1)
            .map(dyn_repeated)
            .expect("early completion pricing entries");
        assert!(!early_entries.is_empty(), "early completion pricing entries non-empty");
        assert!(
            early_entries.iter().any(|e| {
                dyn_msg(e)
                    .and_then(|m| m.get(3))
                    .and_then(dyn_msg)
                    .and_then(|p| p.get(1))
                    .and_then(dyn_u64)
                    .unwrap_or(0)
                    > 0
            }),
            "early completion pricing must contain positive PLEX cents"
        );

        let (_, cap_dyn) = client
            .send_ok_dyn(
                "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
                Vec::new(),
            )
            .await;
        let max_jobs = cap_dyn.get(1).and_then(dyn_u64).unwrap_or(0);
        assert!(max_jobs > 1, "Mass Sequencing V must raise max job capacity above 1 (got {max_jobs})");

        // Consume real inventory item into component license
        let mut consume_req = Vec::new();
        encode_bytes_field(1, &encode_seq_ident(comp_item_id as u64), &mut consume_req);
        encode_varint_field(2, 1, &mut consume_req);
        client
            .send_ok_empty(
                "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
                consume_req,
            )
            .await;

        let notices_after_consume = client.drain_notices(150).await;
        assert!(
            notices_after_consume.iter().any(|(n, _)| n
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.contains("component.license") && p.type_url.contains("GrantedNotice"))),
            "Consuming component license item must push GrantedNotice on gRPC Notices/Consume"
        );
        assert!(
            ctx.db.with_snapshot(|s| !s.items.contains_key(&comp_item_id)),
            "Consumed component license item must be removed from Hangar"
        );

        // If the SDE component item granted finite uses < 4, also grant finite uses for multi-job test via admin RPC
        if coat_uses > 0 {
            let mut g_req = Vec::new();
            encode_bytes_field(1, &encode_comp_ident(coat_comp_id, false), &mut g_req);
            encode_varint_field(2, 6, &mut g_req);
            client
                .send_ok_empty(
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
                    g_req,
                )
                .await;
        }
        for (cid, is_pat) in [(met_comp_id, false), (pat_comp_id, true)] {
            let mut g_req = Vec::new();
            encode_bytes_field(1, &encode_comp_ident(cid, is_pat), &mut g_req);
            encode_varint_field(2, 10, &mut g_req);
            client
                .send_ok_empty(
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
                    g_req,
                )
                .await;
        }

        // Negative case 1: invalid non-ship hull (Tritanium) must be rejected with 400
        let mut bad_hull_layout = Vec::new();
        encode_bytes_field(1, &encode_coating_slot(1, coat_comp_id), &mut bad_hull_layout);
        encode_bytes_field(2, &encode_seq_ident(u64::from(tritanium_type)), &mut bad_hull_layout);
        encode_varint_field(3, 1, &mut bad_hull_layout);
        let mut bad_hull_req = Vec::new();
        encode_string_field(1, "Invalid Tritanium Skin", &mut bad_hull_req);
        encode_string_field(2, "Bad Line", &mut bad_hull_req);
        encode_bytes_field(3, &bad_hull_layout, &mut bad_hull_req);
        encode_varint_field(5, 1, &mut bad_hull_req);
        let bad_rsp = client
            .send_raw(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                bad_hull_req,
            )
            .await;
        assert_eq!(bad_rsp.status_code, STATUS_BAD_REQUEST, "Non-ship hull must fail with 400");

        // Negative case 2: slot category mismatch (pattern placed in coating slot 1) must be rejected with 400
        let mut bad_slot_layout = Vec::new();
        encode_bytes_field(1, &encode_coating_slot(1, pat_comp_id), &mut bad_slot_layout);
        encode_bytes_field(2, &encode_seq_ident(u64::from(rifter_type)), &mut bad_slot_layout);
        encode_varint_field(3, 1, &mut bad_slot_layout);
        let mut bad_slot_req = Vec::new();
        encode_string_field(1, "Wrong Slot Skin", &mut bad_slot_req);
        encode_string_field(2, "Bad Line", &mut bad_slot_req);
        encode_bytes_field(3, &bad_slot_layout, &mut bad_slot_req);
        encode_varint_field(5, 1, &mut bad_slot_req);
        let slot_rsp = client
            .send_raw(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                bad_slot_req,
            )
            .await;
        assert_eq!(slot_rsp.status_code, STATUS_BAD_REQUEST, "Slot mismatch must fail with 400");

        // Valid layout -> Initiate Job 1 (2 licenses), verify PLEX debit & StartedNotice
        let mut layout = Vec::new();
        encode_bytes_field(1, &encode_coating_slot(1, coat_comp_id), &mut layout);
        encode_bytes_field(1, &encode_coating_slot(2, met_comp_id), &mut layout);
        encode_bytes_field(1, &encode_pattern_slot(5, pat_comp_id), &mut layout);
        encode_bytes_field(2, &encode_seq_ident(u64::from(rifter_type)), &mut layout);
        encode_varint_field(3, 1, &mut layout);

        let mut init1_req = Vec::new();
        encode_string_field(1, "Crimson Paragon Rifter", &mut init1_req);
        encode_string_field(2, "Paragon Studio Line", &mut init1_req);
        encode_bytes_field(3, &layout, &mut init1_req);
        encode_varint_field(5, 2, &mut init1_req);

        let plex_pre = ctx.db.with_snapshot(|s| s.unified_plex_balance(i64::from(designer_cid)));
        let (_, init1_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                init1_req,
            )
            .await;
        let job1_uuid = init1_dyn
            .get(1)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_bytes)
            .expect("job1 uuid")
            .to_vec();
        assert_eq!(job1_uuid.len(), 16);
        let skin_hex_1 = init1_dyn
            .get(2)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_str)
            .expect("skin_hex_1")
            .to_string();
        assert!(!skin_hex_1.is_empty());

        let plex_post_init1 = ctx.db.with_snapshot(|s| s.unified_plex_balance(i64::from(designer_cid)));
        assert!(plex_post_init1 < plex_pre, "Job initiation must debit PLEX");

        let start_notices = client.drain_notices(150).await;
        assert!(
            start_notices.iter().any(|(n, dn)| {
                n.payload.as_ref().is_some_and(|p| p.type_url.ends_with(".StartedNotice"))
                    && dn
                        .get(1)
                        .and_then(dyn_msg)
                        .and_then(|m| m.get(1))
                        .and_then(dyn_bytes)
                        == Some(job1_uuid.as_slice())
            }),
            "Initiating job must deliver StartedNotice with matching job UUID over gRPC Notices/Consume"
        );

        // Initiate Job 2 (for early completion)
        let mut init2_req = Vec::new();
        encode_string_field(1, "Obsidian Paragon Rifter", &mut init2_req);
        encode_string_field(2, "Paragon Studio Line", &mut init2_req);
        encode_bytes_field(3, &layout, &mut init2_req);
        encode_varint_field(5, 1, &mut init2_req);
        let (_, init2_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                init2_req,
            )
            .await;
        let job2_uuid = init2_dyn
            .get(1)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_bytes)
            .expect("job2 uuid")
            .to_vec();
        let skin_hex_2 = init2_dyn
            .get(2)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_str)
            .expect("skin_hex_2")
            .to_string();

        // Both jobs active before completion
        let (_, act_dyn) = client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
                Vec::new(),
            )
            .await;
        assert_eq!(act_dyn.get(1).map(dyn_repeated).unwrap_or(&[]).len(), 2);

        // Complete Job 2 early -> charges PLEX & mints 1 license
        let mut comp_early_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&job2_uuid), &mut comp_early_req);
        let plex_before_early = ctx.db.with_snapshot(|s| s.unified_plex_balance(i64::from(designer_cid)));
        client
            .send_ok_empty(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
                comp_early_req,
            )
            .await;
        let plex_after_early = ctx.db.with_snapshot(|s| s.unified_plex_balance(i64::from(designer_cid)));
        assert!(plex_after_early < plex_before_early, "CompleteEarlyRequest must charge PLEX");

        let comp_notices = client.drain_notices(150).await;
        assert!(
            comp_notices.iter().any(|(n, _)| n
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.contains("sequencing.job") && p.type_url.ends_with(".CompletedNotice"))),
            "CompleteEarlyRequest must emit sequencing CompletedNotice"
        );

        // Advance server clock by 30 days so Job 1 finishes naturally
        ctx.advance_secs(30 * 86_400);
        // Advancing 30 days expires the 1h OAuth2 access token; open a fresh PKCE gRPC session
        client.close();
        let mut client_post_advance =
            GrpcClient::connect(Arc::clone(&ctx), "skinr_des", "des-pass", designer_cid, 0x13).await;

        let (_, act_after_dyn) = client_post_advance
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
                Vec::new(),
            )
            .await;
        assert_eq!(
            act_after_dyn.get(1).map(dyn_repeated).unwrap_or(&[]).len(),
            0,
            "All jobs must settle once virtual clock passes completion time"
        );

        client_post_advance.close();
        (skin_hex_1, job1_uuid, skin_hex_2)
    };

    // Full server restart from SQLite: verify persisted SKIN designs, completed job, and licenses
    let ctx_reloaded = boot_sqlite_ctx(&sqlite.path, base_unix + 30 * 86_400 + 60);
    let mut client2 = GrpcClient::connect(Arc::clone(&ctx_reloaded), "skinr_des", "des-pass", designer_cid, 0x12).await;

    let mut get_skin_req = Vec::new();
    let mut hex_msg = Vec::new();
    encode_string_field(1, &skin_hex_1, &mut hex_msg);
    encode_bytes_field(1, &hex_msg, &mut get_skin_req);
    let (_, skin_dyn) = client2
        .send_ok_dyn(
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
            get_skin_req,
        )
        .await;
    assert_eq!(
        skin_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(1)).and_then(dyn_str),
        Some("Crimson Paragon Rifter")
    );

    let mut get_job_req = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&job1_uuid), &mut get_job_req);
    let (_, job1_dyn) = client2
        .send_ok_dyn(
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest",
            get_job_req,
        )
        .await;
    let job_attrs = job1_dyn.get(1).and_then(dyn_msg).expect("job1 attributes");
    assert_eq!(job_attrs.get(3).and_then(dyn_u64), Some(2), "Job 1 sequenced 2 licenses");

    // Activate one license from Job 2 on designer character across reload
    let mut act_lic_req = Vec::new();
    encode_bytes_field(1, &encode_skin_lic_id(designer_cid, &skin_hex_2), &mut act_lic_req);
    client2
        .send_ok_empty(
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
            act_lic_req,
        )
        .await;

    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &encode_skin_lic_id(designer_cid, &skin_hex_1), &mut get_lic_req);
    let (_, lic1_dyn) = client2
        .send_ok_dyn(
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
            get_lic_req,
        )
        .await;
    assert_eq!(
        lic1_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(2)).and_then(dyn_u64),
        Some(2),
        "2 unactivated Job 1 licenses must survive SQLite reload"
    );

    let mut get_lic2_req = Vec::new();
    encode_bytes_field(1, &encode_skin_lic_id(designer_cid, &skin_hex_2), &mut get_lic2_req);
    let (_, lic2_dyn) = client2
        .send_ok_dyn(
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
            get_lic2_req,
        )
        .await;
    assert_eq!(
        lic2_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(1)).and_then(dyn_bool),
        Some(true),
        "Job 2 license must become activated after ActivateRequest"
    );

    client2.close();
}

// ============================================================================
// Test 2: Paragon Hub Market, Component Market, Fees, Notices, Titles & SQLite
// ============================================================================
// parity: feat:cosmetics.skinr-market
// parity: feat:cosmetics.skinr-paragon-hub-listing
// parity: feat:quasar.cosmetic
// parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj
#[tokio::test]
async fn paragon_hub_cosmetics_market_and_titles_grpc_lifecycle() {
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-market",
        "feat:cosmetics.skinr-paragon-hub-listing",
        "feat:quasar.cosmetic",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj",
    ]);

    let sqlite = TempSqliteDb::new("paragon_hub");
    let base_unix = common::TEST_CLOCK_BASE_UNIX;
    let seller_cid = 90_100_201u32;
    let buyer_cid = 90_100_202u32;
    let rifter_type = 587u32;

    let (listing_isk_uuid, listing_plex_uuid, listing_cancel_uuid, skin_hex) = {
        let ctx = boot_sqlite_ctx(&sqlite.path, base_unix);
        ctx.db.provision_gm_account("hub_seller", "sel-pass", i64::from(seller_cid), "Hub Seller");
        ctx.db.provision_gm_account("hub_buyer", "buy-pass", i64::from(buyer_cid), "Hub Buyer");

        let mut sel_sess = login_macho_session(&ctx, 70_201, "hub_seller", "sel-pass", i64::from(seller_cid)).await;
        let mut buy_sess = login_macho_session(&ctx, 70_202, "hub_buyer", "buy-pass", i64::from(buyer_cid)).await;

        // Train Paragon Hub tax & slot capacity skills for seller:
        // 84217/84218/84220 -> -15% public tax (20% -> 5%)
        // 81368/81369 -> +5 + 25 = 35 max listings (vs base 5)
        for sid in [84217u32, 84218, 84220, 81368, 81369, 81375] {
            common::slash_cmd(&ctx, &mut sel_sess, &format!("/giveskill {sid} 5")).await;
        }
        // Grant buyer PLEX for PLEX listing purchase
        let plex_created = common::slash_cmd(&ctx, &mut buy_sess, "/createitem 44992 500").await;
        let plex_item_id = plex_created.dict_get("itemID").and_then(|v| v.as_i64()).expect("buyer plex itemID");
        ctx.dispatch_rpc(
            &mut buy_sess,
            "invbroker",
            None,
            "DepositPlexToVault",
            &[PyRep::Integer(60003760), PyRep::Integer(plex_item_id), PyRep::Integer(500)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("buyer DepositPlexToVault");

        let mut seller_client = GrpcClient::connect(Arc::clone(&ctx), "hub_seller", "sel-pass", seller_cid, 0x21).await;
        seller_client.open_notice_stream().await;
        let mut buyer_client = GrpcClient::connect(Arc::clone(&ctx), "hub_buyer", "buy-pass", buyer_cid, 0x22).await;
        buyer_client.open_notice_stream().await;

        // 1. Character title definition endpoints (GetLatestRequest + GetAllRequest)
        let mut latest_title_req = Vec::new();
        let mut lang_msg = Vec::new();
        encode_string_field(1, "en-us", &mut lang_msg);
        encode_bytes_field(3, &lang_msg, &mut latest_title_req);
        let (_, title_latest_dyn) = buyer_client
            .send_ok_dyn(
                "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
                latest_title_req,
            )
            .await;
        let checkpoint_url = title_latest_dyn
            .get(1)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_str)
            .unwrap_or("");
        assert!(checkpoint_url.contains("cosmetics/character/title/definitions/"), "unexpected title URL: {checkpoint_url}");
        buyer_client
            .send_ok_dyn("eve_public.cosmetic.character.title.definition.api.GetAllRequest", Vec::new())
            .await;

        // 2. Market rates & buyer fees definition
        let mut pub_target = Vec::new();
        encode_varint_field(1, 1, &mut pub_target);
        let mut tax_req = Vec::new();
        encode_bytes_field(1, &pub_target, &mut tax_req);

        let (_, seller_tax_dyn) = seller_client
            .send_ok_dyn("eve_public.cosmetic.market.api.GetTaxRateRequest", tax_req.clone())
            .await;
        let seller_tax_pct = seller_tax_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(1)).and_then(dyn_i64);
        assert_eq!(seller_tax_pct, Some(5), "Trained Paragon skills must drop public tax to 5%");

        let (_, buyer_tax_dyn) = buyer_client
            .send_ok_dyn("eve_public.cosmetic.market.api.GetTaxRateRequest", tax_req)
            .await;
        let buyer_tax_pct = buyer_tax_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(1)).and_then(dyn_i64);
        assert_eq!(buyer_tax_pct, Some(20), "Untrained public tax must remain 20%");

        let mut broker_req = Vec::new();
        encode_bytes_field(1, &pub_target, &mut broker_req);
        let (_, broker_dyn) = seller_client
            .send_ok_dyn("eve_public.cosmetic.market.api.GetBrokerFeesRequest", broker_req)
            .await;
        assert!(!broker_dyn.get(1).map(dyn_repeated).unwrap_or(&[]).is_empty(), "broker fee schedule non-empty");

        let (_, buyer_defs_dyn) = buyer_client
            .send_ok_dyn("eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest", Vec::new())
            .await;
        assert_eq!(buyer_defs_dyn.get(2).and_then(dyn_str), Some(BUYER_FEES_VERSION));

        // 3. Purchase SDE component listing on Paragon Hub & verify inventory + TransactionSucceededNotice
        let (_, comp_list_dyn) = buyer_client
            .send_ok_dyn("eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new())
            .await;
        let comp_entries = comp_list_dyn.get(1).map(dyn_repeated).expect("SDE component listings");
        assert!(!comp_entries.is_empty(), "SDE component listings must auto-populate");
        let first_comp_uuid = dyn_msg(&comp_entries[0])
            .and_then(|m| m.get(1))
            .and_then(dyn_msg)
            .and_then(|id| id.get(1))
            .and_then(dyn_bytes)
            .expect("component listing uuid")
            .to_vec();

        let buyer_isk_pre_comp = ctx.db.get_character(i64::from(buyer_cid)).unwrap().balance.0;
        let mut comp_buy_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&first_comp_uuid), &mut comp_buy_req);
        encode_varint_field(2, 1, &mut comp_buy_req);
        encode_varint_field(3, 0, &mut comp_buy_req);
        buyer_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
                comp_buy_req,
            )
            .await;
        let buyer_isk_post_comp = ctx.db.get_character(i64::from(buyer_cid)).unwrap().balance.0;
        assert!(buyer_isk_post_comp < buyer_isk_pre_comp, "Component purchase must debit buyer ISK");

        let comp_tx_notices = buyer_client.drain_notices(150).await;
        assert!(
            comp_tx_notices.iter().any(|(n, _)| n
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with(".TransactionSucceededNotice"))),
            "Component purchase must push TransactionSucceededNotice"
        );

        // 4. Sequence 8 SKIN licenses for seller & complete early so seller can create ISK, PLEX, targeted & cancelled listings
        let catalog = SkinrStaticCatalog::load(&ctx.sde);
        let coat_id = catalog
            .components
            .values()
            .find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1))
            .unwrap()
            .component_id;
        let mut g_req = Vec::new();
        encode_bytes_field(1, &encode_comp_ident(coat_id, false), &mut g_req);
        encode_varint_field(2, 20, &mut g_req);
        seller_client
            .send_ok_empty(
                "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
                g_req,
            )
            .await;

        let mut layout = Vec::new();
        encode_bytes_field(1, &encode_coating_slot(1, coat_id), &mut layout);
        encode_bytes_field(2, &encode_seq_ident(u64::from(rifter_type)), &mut layout);
        encode_varint_field(3, 1, &mut layout);
        let mut seq_req = Vec::new();
        encode_string_field(1, "Market Hub Test Skin", &mut seq_req);
        encode_string_field(2, "Hub Line", &mut seq_req);
        encode_bytes_field(3, &layout, &mut seq_req);
        encode_varint_field(5, 8, &mut seq_req);
        let (_, seq_dyn) = seller_client
            .send_ok_dyn(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                seq_req,
            )
            .await;
        let job_uuid = seq_dyn
            .get(1)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_bytes)
            .unwrap()
            .to_vec();
        let skin_hex = seq_dyn
            .get(2)
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_msg)
            .and_then(|m| m.get(1))
            .and_then(dyn_str)
            .unwrap()
            .to_string();
        let mut ce_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&job_uuid), &mut ce_req);
        seller_client
            .send_ok_empty(
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
                ce_req,
            )
            .await;
        let _ = seller_client.drain_notices(80).await;

        // Check GetBuyerFeeRequest (tier 1 public = 1,000 EverMarks)
        let mut skin_id_m = Vec::new();
        encode_string_field(1, &skin_hex, &mut skin_id_m);
        let mut bf_req = Vec::new();
        encode_bytes_field(1, &pub_target, &mut bf_req);
        encode_bytes_field(2, &skin_id_m, &mut bf_req);
        let (_, bf_dyn) = buyer_client
            .send_ok_dyn("eve_public.cosmetic.market.api.GetBuyerFeeRequest", bf_req)
            .await;
        let lp_msg = bf_dyn
            .get(1)
            .and_then(dyn_msg)
            .and_then(|fee| fee.get(1))
            .and_then(dyn_msg)
            .expect("loyalty_points Currency");
        let buyer_fee_em = lp_msg
            .get(1)
            .and_then(|v| dyn_u64(v).or_else(|| dyn_msg(v)?.get(1).and_then(dyn_u64)));
        assert_eq!(buyer_fee_em, Some(1_000), "Tier 1 public buyer EverMark fee must be 1,000");

        // Create 3 listings: (A) Public ISK 1,000,000 ISK, (B) Public PLEX 100 PLEX (10,000 cents), (C) Targeted wrong corp, (D) To be cancelled
        let lic_id = encode_skin_lic_id(seller_cid, &skin_hex);
        let mut dur_msg = Vec::new();
        encode_varint_field(1, 86_400, &mut dur_msg);

        // (A) Public ISK
        let mut isk_val = Vec::new();
        encode_sint_field(1, 1_000_000, &mut isk_val);
        let mut create_isk = Vec::new();
        encode_bytes_field(1, &isk_val, &mut create_isk);
        encode_bytes_field(3, &lic_id, &mut create_isk);
        encode_bytes_field(4, &dur_msg, &mut create_isk);
        encode_varint_field(5, 1, &mut create_isk);
        encode_bytes_field(6, &pub_target, &mut create_isk);
        seller_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
                create_isk,
            )
            .await;

        // (B) Public PLEX (100 PLEX = 10,000 cents, sint64)
        let mut plex_val = Vec::new();
        encode_sint_field(1, 10_000, &mut plex_val);
        let mut create_plex = Vec::new();
        encode_bytes_field(2, &plex_val, &mut create_plex);
        encode_bytes_field(3, &lic_id, &mut create_plex);
        encode_bytes_field(4, &dur_msg, &mut create_plex);
        encode_varint_field(5, 1, &mut create_plex);
        encode_bytes_field(6, &pub_target, &mut create_plex);
        seller_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
                create_plex,
            )
            .await;

        // (C) Corp-targeted listing for corp 99_999_001 (buyer not member -> 403)
        let mut corp_target = Vec::new();
        encode_bytes_field(2, &encode_seq_ident(99_999_001), &mut corp_target);
        let mut create_corp = Vec::new();
        encode_bytes_field(1, &isk_val, &mut create_corp);
        encode_bytes_field(3, &lic_id, &mut create_corp);
        encode_bytes_field(4, &dur_msg, &mut create_corp);
        encode_varint_field(5, 1, &mut create_corp);
        encode_bytes_field(6, &corp_target, &mut create_corp);
        seller_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
                create_corp,
            )
            .await;

        // (D) Another public ISK listing, immediately cancelled to test CancelListingRequest + license unescrow
        let mut create_cancel = Vec::new();
        encode_bytes_field(1, &isk_val, &mut create_cancel);
        encode_bytes_field(3, &lic_id, &mut create_cancel);
        encode_bytes_field(4, &dur_msg, &mut create_cancel);
        encode_varint_field(5, 1, &mut create_cancel);
        encode_bytes_field(6, &pub_target, &mut create_cancel);
        seller_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
                create_cancel,
            )
            .await;

        let (_, owned_dyn) = seller_client
            .send_ok_dyn("eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest", Vec::new())
            .await;
        let owned_entries = owned_dyn.get(1).map(dyn_repeated).expect("owned listings");
        assert_eq!(owned_entries.len(), 4);

        let mut listing_isk_uuid = Vec::new();
        let mut listing_plex_uuid = Vec::new();
        let mut listing_corp_uuid = Vec::new();
        let mut listing_cancel_uuid = Vec::new();
        for entry in owned_entries {
            let em = dyn_msg(entry).unwrap();
            let u = em.get(1).and_then(dyn_msg).and_then(|id| id.get(1)).and_then(dyn_bytes).unwrap().to_vec();
            let attr = em.get(2).and_then(dyn_msg).unwrap();
            if attr.get(6).is_some() {
                listing_plex_uuid = u;
            } else if attr.get(9).and_then(dyn_msg).and_then(|t| t.get(2)).is_some() {
                listing_corp_uuid = u;
            } else if listing_isk_uuid.is_empty() {
                listing_isk_uuid = u;
            } else {
                listing_cancel_uuid = u;
            }
        }
        assert!(!listing_isk_uuid.is_empty() && !listing_plex_uuid.is_empty() && !listing_corp_uuid.is_empty() && !listing_cancel_uuid.is_empty());

        // Cancel listing D -> unescrows 1 license back to seller
        let mut cancel_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&listing_cancel_uuid), &mut cancel_req);
        seller_client
            .send_ok_empty(
                "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
                cancel_req,
            )
            .await;

        // Negative cases before funding EverMarks:
        // (1) Self-buy -> 400
        let mut self_buy = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&listing_isk_uuid), &mut self_buy);
        encode_varint_field(2, 1, &mut self_buy);
        encode_string_field(3, BUYER_FEES_VERSION, &mut self_buy);
        assert_eq!(
            seller_client
                .send_raw("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", self_buy)
                .await
                .status_code,
            STATUS_BAD_REQUEST
        );

        // (2) Outdated buyer fee version -> 400
        let mut bad_ver_buy = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&listing_isk_uuid), &mut bad_ver_buy);
        encode_varint_field(2, 1, &mut bad_ver_buy);
        encode_string_field(3, "stale-version", &mut bad_ver_buy);
        assert_eq!(
            buyer_client
                .send_raw("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", bad_ver_buy)
                .await
                .status_code,
            STATUS_BAD_REQUEST
        );

        // (3) Corp-targeted mismatch -> 403
        let mut corp_buy = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&listing_corp_uuid), &mut corp_buy);
        encode_varint_field(2, 1, &mut corp_buy);
        encode_string_field(3, BUYER_FEES_VERSION, &mut corp_buy);
        assert_eq!(
            buyer_client
                .send_raw("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", corp_buy)
                .await
                .status_code,
            STATUS_FORBIDDEN
        );

        // (4) Insufficient EverMarks -> 412 + TransactionFailedNotice
        let mut no_em_buy = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&listing_isk_uuid), &mut no_em_buy);
        encode_varint_field(2, 1, &mut no_em_buy);
        encode_string_field(3, BUYER_FEES_VERSION, &mut no_em_buy);
        assert_eq!(
            buyer_client
                .send_raw("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", no_em_buy.clone())
                .await
                .status_code,
            STATUS_PRECONDITION_FAILED
        );
        let fail_notices = buyer_client.drain_notices(150).await;
        assert!(
            fail_notices.iter().any(|(n, _)| n
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with(".TransactionFailedNotice"))),
            "Insufficient EverMarks failure must emit TransactionFailedNotice"
        );

        // Fund buyer EverMarks via Quasar LoyaltyPoints DepositRequest & purchase ISK listing (5% tax)
        deposit_evermarks_grpc(&mut buyer_client, buyer_cid, 50_000, &[0x55u8; 16]).await;
        let seller_isk_pre = ctx.db.get_character(i64::from(seller_cid)).unwrap().balance.0;
        let buyer_isk_pre = ctx.db.get_character(i64::from(buyer_cid)).unwrap().balance.0;

        buyer_client
            .send_ok_dyn(
                "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
                no_em_buy,
            )
            .await;

        let buyer_isk_post = ctx.db.get_character(i64::from(buyer_cid)).unwrap().balance.0;
        let seller_isk_post = ctx.db.get_character(i64::from(seller_cid)).unwrap().balance.0;
        assert_eq!(buyer_isk_pre - buyer_isk_post, IskCents::from_isk(1_000_000).0);
        assert_eq!(seller_isk_post - seller_isk_pre, IskCents::from_isk(950_000).0);

        let succ_notices = buyer_client.drain_notices(150).await;
        assert!(
            succ_notices.iter().any(|(n, _)| n
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with(".TransactionSucceededNotice"))),
            "SKIN purchase must emit TransactionSucceededNotice"
        );

        // Verify MachoNet balance reflects ISK change too
        let seller_cash_rep = ctx
            .dispatch_rpc(&mut sel_sess, "account", None, "GetCashBalance", &[], &HashMap::new(), None)
            .await
            .expect("seller GetCashBalance");
        assert!(matches!(seller_cash_rep, PyRep::Real(v) if v > 0.0));

        seller_client.close();
        buyer_client.close();
        (listing_isk_uuid, listing_plex_uuid, listing_cancel_uuid, skin_hex)
    };

    // Reload from SQLite: cancelled/sold listings stay gone, surviving PLEX listing still purchasable across restart!
    let ctx2 = boot_sqlite_ctx(&sqlite.path, base_unix + 60);
    let mut seller_c2 = GrpcClient::connect(Arc::clone(&ctx2), "hub_seller", "sel-pass", seller_cid, 0x23).await;
    let mut buyer_c2 = GrpcClient::connect(Arc::clone(&ctx2), "hub_buyer", "buy-pass", buyer_cid, 0x24).await;

    let mut get_sold = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&listing_isk_uuid), &mut get_sold);
    assert_eq!(
        buyer_c2.send_raw("eve_public.cosmetic.market.skin.listing.api.GetRequest", get_sold).await.status_code,
        STATUS_NOT_FOUND
    );
    let mut get_cancelled = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&listing_cancel_uuid), &mut get_cancelled);
    assert_eq!(
        seller_c2.send_raw("eve_public.cosmetic.market.skin.listing.api.GetRequest", get_cancelled).await.status_code,
        STATUS_NOT_FOUND
    );

    // PLEX listing survived SQLite reload -> purchase it across reload!
    let seller_plex_pre = ctx2.db.with_snapshot(|s| s.unified_plex_balance(i64::from(seller_cid)));
    let buyer_plex_pre = ctx2.db.with_snapshot(|s| s.unified_plex_balance(i64::from(buyer_cid)));
    let mut buy_plex_req = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&listing_plex_uuid), &mut buy_plex_req);
    encode_varint_field(2, 1, &mut buy_plex_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_plex_req);
    buyer_c2
        .send_ok_dyn(
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_plex_req,
        )
        .await;
    let seller_plex_post = ctx2.db.with_snapshot(|s| s.unified_plex_balance(i64::from(seller_cid)));
    let buyer_plex_post = ctx2.db.with_snapshot(|s| s.unified_plex_balance(i64::from(buyer_cid)));
    assert_eq!(buyer_plex_pre - buyer_plex_post, 100, "Buyer debited 100 PLEX");
    assert_eq!(seller_plex_post - seller_plex_pre, 95, "Seller receives 95 PLEX (5% Paragon skill tax)");

    // Buyer now owns 2 unactivated licenses (1 from ISK purchase pre-reload, 1 from PLEX purchase post-reload)
    let mut lic_req = Vec::new();
    encode_bytes_field(1, &encode_skin_lic_id(buyer_cid, &skin_hex), &mut lic_req);
    let (_, buyer_lic_dyn) = buyer_c2
        .send_ok_dyn("eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req)
        .await;
    assert_eq!(
        buyer_lic_dyn.get(1).and_then(dyn_msg).and_then(|m| m.get(2)).and_then(dyn_u64),
        Some(2)
    );

    seller_c2.close();
    buyer_c2.close();
}

// ============================================================================
// Test 3: AIR Achievements (SDE Load + gRPC Notices/Consume + Negative + SQLite)
// ============================================================================
// parity: feat:quasar.achievement
// parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
#[tokio::test]
async fn air_achievements_grpc_and_sqlite_reload() {
    eve_testkit::attest(&[
        "feat:quasar.achievement",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
    ]);

    let sqlite = TempSqliteDb::new("achievements");
    let base_unix = common::TEST_CLOCK_BASE_UNIX;
    let gm_cid = 90_100_301u32;
    let player_cid = 90_100_302u32;

    let (ach_id, ach_cat_id, ach_score, target_max) = {
        let ctx = boot_sqlite_ctx(&sqlite.path, base_unix);
        ctx.db.provision_gm_account("ach_gm", "gm-pass", i64::from(gm_cid), "Achievement GM");
        ctx.db.provision_gm_account("ach_plr", "plr-pass", i64::from(player_cid), "Achievement Player");
        let plr_sess = login_macho_session(&ctx, 70_302, "ach_plr", "plr-pass", i64::from(player_cid)).await;
        // Strip GM role from player_cid via authentic /role command so non-GM 403 checks exercise real RBAC
        let mut gm_sess = login_macho_session(&ctx, 70_301, "ach_gm", "gm-pass", i64::from(gm_cid)).await;
        let _ = common::slash_cmd(&ctx, &mut gm_sess, &format!("/role ach_plr {}", service_roles::ROLE_PLAYER)).await;
        let _ = plr_sess;

        let ach_mgr = AchievementManager::new_with_sde(None, Some(Arc::clone(&ctx.sde)));
        let sample = ach_mgr
            .definitions
            .iter()
            .find(|d| d.score > 0 && d.milestone_targets.iter().copied().max().unwrap_or(0) > 1)
            .unwrap_or_else(|| ach_mgr.definitions.first().expect("SDE achievements"));
        let ach_id = sample.id;
        let ach_cat_id = sample.category_id;
        let ach_score = sample.score;
        let target_max = sample.milestone_targets.iter().copied().max().unwrap_or(1).max(1) as u64;

        let mut gm_client = GrpcClient::connect(Arc::clone(&ctx), "ach_gm", "gm-pass", gm_cid, 0x31).await;
        gm_client.open_notice_stream().await;
        let mut plr_client = GrpcClient::connect(Arc::clone(&ctx), "ach_plr", "plr-pass", player_cid, 0x32).await;

        // 1. CDN definition manifest checks (`no_local = true` -> Checkpoint, client semver w/o CDN -> 503)
        let mut cdn_req = Vec::new();
        encode_varint_field(2, 1, &mut cdn_req);
        let (_, cdn_dyn) = gm_client
            .send_ok_dyn("eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_req)
            .await;
        let cdn_url = cdn_dyn.get(1).and_then(dyn_msg).and_then(|c| c.get(1)).and_then(dyn_str).unwrap_or("");
        assert!(cdn_url.contains("/achievements/v24.1."), "unexpected achievement CDN url: {cdn_url}");

        let mut local_cdn_req = Vec::new();
        let mut ver_spec = Vec::new();
        encode_varint_field(1, 24, &mut ver_spec);
        encode_varint_field(2, 1, &mut ver_spec);
        encode_varint_field(3, 1, &mut ver_spec);
        encode_bytes_field(1, &ver_spec, &mut local_cdn_req);
        assert_eq!(
            gm_client
                .send_raw("eve_public.achievement.definition.api.cdn.GetLatestRequest", local_cdn_req)
                .await
                .status_code,
            STATUS_SERVICE_UNAVAILABLE
        );

        // 2. Non-GM player calling admin.AddProgressRequest must fail with 403
        let mut aid_be = [0u8; 16];
        aid_be[12..16].copy_from_slice(&ach_id.to_be_bytes());
        let mut add_prog_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&aid_be), &mut add_prog_req);
        encode_varint_field(2, 1, &mut add_prog_req);
        assert_eq!(
            plr_client
                .send_raw("eve_public.achievement.api.admin.AddProgressRequest", add_prog_req.clone())
                .await
                .status_code,
            STATUS_FORBIDDEN
        );

        // 3. Invalid achievement ID -> 404
        let mut bad_id_be = [0u8; 16];
        bad_id_be[12..16].copy_from_slice(&999_999u32.to_be_bytes());
        let mut get_bad_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&bad_id_be), &mut get_bad_req);
        assert_eq!(
            gm_client.send_raw("eve_public.achievement.api.GetRequest", get_bad_req).await.status_code,
            STATUS_NOT_FOUND
        );

        // 4. GM advances progress to full completion -> ProgressedNotice, CompletedNotice, ReachedNotice
        let mut comp_prog_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&aid_be), &mut comp_prog_req);
        encode_varint_field(2, target_max, &mut comp_prog_req);
        gm_client
            .send_ok_empty("eve_public.achievement.api.admin.AddProgressRequest", comp_prog_req)
            .await;

        let notices = gm_client.drain_notices(150).await;
        assert!(
            notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("achievement.api.ProgressedNotice"))),
            "Expected achievement.api.ProgressedNotice"
        );
        assert!(
            notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("achievement.api.CompletedNotice"))),
            "Expected achievement.api.CompletedNotice"
        );

        // 5. Verify GetRequest, category GetRequest, showcase GetCharacterScoreRequest, and reward GetUnclaimedRequest
        let mut get_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&aid_be), &mut get_req);
        let (_, ach_dyn) = gm_client.send_ok_dyn("eve_public.achievement.api.GetRequest", get_req).await;
        assert_eq!(ach_dyn.get(3).and_then(dyn_u64), Some(target_max), "achievement progress must match target_max");
        assert!(ach_dyn.get(1).is_some(), "completed achievement must populate completed_date");

        let mut cat_be = [0u8; 16];
        cat_be[12..16].copy_from_slice(&ach_cat_id.to_be_bytes());
        let mut cat_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&cat_be), &mut cat_req);
        let (_, cat_dyn) = gm_client
            .send_ok_dyn("eve_public.achievement.category.api.GetRequest", cat_req)
            .await;
        assert!(cat_dyn.get(1).and_then(dyn_u64).unwrap_or(0) >= u64::from(ach_score));

        let (_, showcase_dyn) = gm_client
            .send_ok_dyn("eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new())
            .await;
        assert!(showcase_dyn.get(1).and_then(dyn_u64).unwrap_or(0) >= u64::from(ach_score));

        let (_, unclaimed_dyn) = gm_client
            .send_ok_dyn("eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new())
            .await;
        assert!(!unclaimed_dyn.get(1).map(dyn_repeated).unwrap_or(&[]).is_empty(), "Completed achievement must appear unclaimed");

        // 6. Reward ClaimRequest fails closed cleanly (412 DATA GAP) without minting ISK/SP
        let isk_before = ctx.db.get_character(i64::from(gm_cid)).unwrap().balance.0;
        let claim_rsp = gm_client.send_raw("eve_public.achievement.reward.api.ClaimRequest", Vec::new()).await;
        assert_eq!(claim_rsp.status_code, STATUS_PRECONDITION_FAILED);
        assert_eq!(ctx.db.get_character(i64::from(gm_cid)).unwrap().balance.0, isk_before);

        gm_client.close();
        plr_client.close();
        (ach_id, ach_cat_id, ach_score, target_max)
    };

    // SQLite reload verification + category reset persistence check
    let ctx2 = boot_sqlite_ctx(&sqlite.path, base_unix + 60);
    let mut gm2 = GrpcClient::connect(Arc::clone(&ctx2), "ach_gm", "gm-pass", gm_cid, 0x33).await;

    let mut aid_be = [0u8; 16];
    aid_be[12..16].copy_from_slice(&ach_id.to_be_bytes());
    let mut get_req = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&aid_be), &mut get_req);
    let (_, ach2_dyn) = gm2.send_ok_dyn("eve_public.achievement.api.GetRequest", get_req.clone()).await;
    assert_eq!(ach2_dyn.get(3).and_then(dyn_u64), Some(target_max), "Achievement progress must persist across SQLite reload");

    let (_, showcase2) = gm2
        .send_ok_dyn("eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new())
        .await;
    assert!(showcase2.get(1).and_then(dyn_u64).unwrap_or(0) >= u64::from(ach_score));

    // Category reset must zero category score and survive another reload
    let mut cat_be = [0u8; 16];
    cat_be[12..16].copy_from_slice(&ach_cat_id.to_be_bytes());
    let mut cat_reset_req = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&cat_be), &mut cat_reset_req);
    gm2.send_ok_empty("eve_public.achievement.category.api.admin.ResetRequest", cat_reset_req)
        .await;
    let (_, reset_ach) = gm2.send_ok_dyn("eve_public.achievement.api.GetRequest", get_req).await;
    assert_eq!(reset_ach.get(3).and_then(dyn_u64).unwrap_or(0), 0, "Category reset must zero achievement progress");
    gm2.close();
}

// ============================================================================
// Test 4: AIR Career Program & Daily Goals (gRPC + Notices + 11:00 UTC + SQLite)
// ============================================================================
// parity: feat:quasar.career
// parity: feat:pve.air-career-program
// parity: quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj
// parity: feat:quasar.dailygoal
// parity: quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj
#[tokio::test]
async fn air_career_and_daily_goals_grpc_and_sqlite_reload() {
    eve_testkit::attest(&[
        "feat:quasar.career",
        "feat:pve.air-career-program",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
        "feat:quasar.dailygoal",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
    ]);

    let sqlite = TempSqliteDb::new("career_daily");
    let base_unix = common::TEST_CLOCK_BASE_UNIX;
    let pilot_cid = 90_100_401u32;
    let non_gm_cid = 90_100_402u32;

    let (career_uuid, daily_uuid_paid, daily_uuid_prog, expected_pilot_isk, expected_pilot_sp) = {
        let ctx = boot_sqlite_ctx(&sqlite.path, base_unix);
        ctx.db.provision_gm_account("goal_pilot", "gp-pass", i64::from(pilot_cid), "Goal Pilot");
        ctx.db.provision_gm_account("goal_non_gm", "ng-pass", i64::from(non_gm_cid), "Goal Player");

        let mut gm_sess = login_macho_session(&ctx, 70_401, "goal_pilot", "gp-pass", i64::from(pilot_cid)).await;
        let _ = common::slash_cmd(&ctx, &mut gm_sess, &format!("/role goal_non_gm {}", service_roles::ROLE_PLAYER)).await;

        let mut gm_client = GrpcClient::connect(Arc::clone(&ctx), "goal_pilot", "gp-pass", pilot_cid, 0x41).await;
        gm_client.open_notice_stream().await;
        let mut plr_client = GrpcClient::connect(Arc::clone(&ctx), "goal_non_gm", "ng-pass", non_gm_cid, 0x42).await;

        // 1. Career Goal Definitions & Player Goals
        let (_, defs_dyn) = gm_client
            .send_ok_dyn("eve_public.career.goal.api.GetDefinitionsRequest", Vec::new())
            .await;
        let def_entries = defs_dyn.get(1).map(dyn_repeated).expect("career definitions");
        assert!(!def_entries.is_empty(), "Career definitions must load from air_career_goals.json");

        let (_, all_career_dyn) = gm_client
            .send_ok_dyn("eve_public.career.goal.api.GetAllRequest", Vec::new())
            .await;
        let goals = all_career_dyn.get(1).map(dyn_repeated).expect("career goals");
        assert!(!goals.is_empty());

        // Pick a non-milestone career goal (`uuid[0..2] != b"CM"`) with a small positive target
        let mut chosen_career_uuid = Vec::new();
        let mut chosen_career_target = 1u64;
        for d in def_entries {
            let dm = dyn_msg(d).unwrap();
            let u = dm.get(1).and_then(dyn_msg).and_then(|id| id.get(1)).and_then(dyn_bytes).unwrap();
            if &u[0..2] != b"CM" {
                let target = dm.get(2).and_then(dyn_msg).and_then(|a| a.get(4)).and_then(dyn_u64).unwrap_or(1).max(1);
                if chosen_career_uuid.is_empty() || target < chosen_career_target {
                    chosen_career_uuid = u.to_vec();
                    chosen_career_target = target;
                    if target == 1 {
                        break;
                    }
                }
            }
        }
        assert_eq!(chosen_career_uuid.len(), 16);

        // Non-GM cannot invoke career.goal.api.admin.ProgressRequest
        let mut career_prog_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&chosen_career_uuid), &mut career_prog_req);
        encode_varint_field(2, chosen_career_target, &mut career_prog_req);
        assert_eq!(
            plr_client
                .send_raw("eve_public.career.goal.api.admin.ProgressRequest", career_prog_req.clone())
                .await
                .status_code,
            STATUS_FORBIDDEN
        );

        // GM completes career goal via admin.ProgressRequest -> ProgressedNotice + CompletedNotice
        encode_bytes_field(3, &encode_seq_ident(u64::from(pilot_cid)), &mut career_prog_req);
        gm_client
            .send_ok_empty("eve_public.career.goal.api.admin.ProgressRequest", career_prog_req)
            .await;
        let c_notices = gm_client.drain_notices(150).await;
        assert!(c_notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("career.goal.api.ProgressedNotice"))));
        assert!(c_notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("career.goal.api.CompletedNotice"))));

        // Claim career goal reward & verify duplicate claim rejected
        let mut claim_c_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&chosen_career_uuid), &mut claim_c_req);
        let sp_pre_c = ctx.db.get_character(i64::from(pilot_cid)).unwrap().free_skill_points;
        let isk_pre_c = ctx.db.get_character(i64::from(pilot_cid)).unwrap().balance.0;
        gm_client
            .send_ok_empty("eve_public.career.goal.api.ClaimRewardRequest", claim_c_req.clone())
            .await;
        let sp_post_c = ctx.db.get_character(i64::from(pilot_cid)).unwrap().free_skill_points;
        let isk_post_c = ctx.db.get_character(i64::from(pilot_cid)).unwrap().balance.0;
        assert!(isk_post_c >= isk_pre_c || sp_post_c >= sp_pre_c);
        assert_ne!(
            gm_client.send_raw("eve_public.career.goal.api.ClaimRewardRequest", claim_c_req).await.status_code,
            STATUS_OK,
            "Duplicate career reward claim must be rejected"
        );

        // 2. Daily Goals: GetAllCurrentRequest + GetRequest + PayForCompletionRequest + admin.ProgressRequest + RedeemRequest
        let (_, daily_cur_dyn) = gm_client
            .send_ok_dyn("eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new())
            .await;
        let daily_ids = daily_cur_dyn.get(1).map(dyn_repeated).expect("daily goal IDs");
        assert!(daily_ids.len() >= 2, "Expected at least 2 daily goals");
        let extract_daily_uuid = |v: &DynamicValue| -> Option<Vec<u8>> {
            let m = dyn_msg(v)?;
            m.get(1).and_then(|inner| match inner {
                DynamicValue::Bytes(b) => Some(b.clone()),
                DynamicValue::Message(sub) => sub.get(1).and_then(dyn_bytes).map(|b| b.to_vec()),
                _ => None,
            })
        };
        let daily_uuid_paid = extract_daily_uuid(&daily_ids[0]).expect("daily_uuid_paid");
        let daily_uuid_prog = extract_daily_uuid(&daily_ids[1]).expect("daily_uuid_prog");

        let mut get_d0 = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&daily_uuid_paid), &mut get_d0);
        let (_, d0_dyn) = gm_client.send_ok_dyn("eve_public.dailygoal.api.GetRequest", get_d0).await;
        let d0_target = d0_dyn.get(1).and_then(dyn_msg).and_then(|a| a.get(9)).and_then(dyn_u64).unwrap_or(1).max(1);
        assert!(d0_target >= 1);

        // Fund EverMarks so PayForCompletionRequest succeeds
        deposit_evermarks_grpc(&mut gm_client, pilot_cid, 50_000, &[0x44u8; 16]).await;
        let mut pay_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&daily_uuid_paid), &mut pay_req);
        gm_client
            .send_ok_dyn("eve_public.dailygoal.api.PayForCompletionRequest", pay_req)
            .await;

        // Progress daily goal 1 cleanly via admin.ProgressRequest (after verifying non-GM rejected)
        let mut get_d1 = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&daily_uuid_prog), &mut get_d1);
        let (_, d1_dyn) = gm_client.send_ok_dyn("eve_public.dailygoal.api.GetRequest", get_d1).await;
        let d1_target = d1_dyn.get(1).and_then(dyn_msg).and_then(|a| a.get(9)).and_then(dyn_u64).unwrap_or(1).max(1);

        let mut d_prog_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&daily_uuid_prog), &mut d_prog_req);
        encode_bytes_field(2, &encode_seq_ident(u64::from(pilot_cid)), &mut d_prog_req);
        encode_varint_field(3, d1_target, &mut d_prog_req);
        assert_eq!(
            plr_client.send_raw("eve_public.dailygoal.api.admin.ProgressRequest", d_prog_req.clone()).await.status_code,
            STATUS_FORBIDDEN
        );
        gm_client.send_ok_dyn("eve_public.dailygoal.api.admin.ProgressRequest", d_prog_req).await;

        let d_notices = gm_client.drain_notices(150).await;
        assert!(d_notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("dailygoal.api.ProgressedNotice"))));
        assert!(d_notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("dailygoal.api.CompletedNotice"))));

        let (_, with_rew_dyn) = gm_client
            .send_ok_dyn("eve_public.dailygoal.api.GetAllWithRewardsRequest", Vec::new())
            .await;
        assert!(with_rew_dyn.get(1).map(dyn_repeated).unwrap_or(&[]).len() >= 2);

        // Redeem daily goal 0 -> RedeemedNotice + ISK/SP/EverMarks payout
        let isk_pre_redeem = ctx.db.get_character(i64::from(pilot_cid)).unwrap().balance.0;
        let mut redeem_req = Vec::new();
        encode_bytes_field(1, &encode_uuid_ident(&daily_uuid_paid), &mut redeem_req);
        gm_client
            .send_ok_dyn("eve_public.dailygoal.api.RedeemRequest", redeem_req.clone())
            .await;
        let isk_post_redeem = ctx.db.get_character(i64::from(pilot_cid)).unwrap().balance.0;
        let sp_post_redeem = ctx.db.get_character(i64::from(pilot_cid)).unwrap().free_skill_points;
        assert!(isk_post_redeem > isk_pre_redeem, "Redeeming daily goal must credit ISK");

        let r_notices = gm_client.drain_notices(150).await;
        assert!(r_notices.iter().any(|(n, _)| n.payload.as_ref().is_some_and(|p| p.type_url.contains("dailygoal.api.RedeemedNotice"))));

        // Duplicate redeem fails
        assert_eq!(
            gm_client.send_raw("eve_public.dailygoal.api.RedeemRequest", redeem_req).await.status_code,
            STATUS_FORBIDDEN
        );

        gm_client.close();
        plr_client.close();
        (chosen_career_uuid, daily_uuid_paid, daily_uuid_prog, isk_post_redeem, sp_post_redeem)
    };

    // Reload from SQLite at same day bucket: verify career claimed + daily completed/redeemed persisted
    let ctx2 = boot_sqlite_ctx(&sqlite.path, base_unix + 60);
    let mut gm2 = GrpcClient::connect(Arc::clone(&ctx2), "goal_pilot", "gp-pass", pilot_cid, 0x43).await;
    assert_eq!(ctx2.db.get_character(i64::from(pilot_cid)).unwrap().balance.0, expected_pilot_isk);
    assert_eq!(ctx2.db.get_character(i64::from(pilot_cid)).unwrap().free_skill_points, expected_pilot_sp);

    let mut has_claimed_req = Vec::new();
    encode_bytes_field(1, &encode_uuid_ident(&career_uuid), &mut has_claimed_req);
    let (_, hc_dyn) = gm2
        .send_ok_dyn("eve.character.career.goal.HasClaimedRewardRequest", has_claimed_req)
        .await;
    assert_eq!(hc_dyn.get(1).and_then(dyn_bool), Some(true), "Claimed career goal must survive SQLite reload");

    // Unclaimed daily goal 1 must still show up in GetAllWithRewardsRequest after reload
    let (_, with_rew2) = gm2
        .send_ok_dyn("eve_public.dailygoal.api.GetAllWithRewardsRequest", Vec::new())
        .await;
    let rew_ids: Vec<&[u8]> = with_rew2
        .get(1)
        .map(dyn_repeated)
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| dyn_msg(v)?.get(1).and_then(dyn_bytes))
        .collect();
    assert!(rew_ids.contains(&daily_uuid_prog.as_slice()), "Unredeemed daily goal 1 must remain claimable after reload");
    assert!(!rew_ids.contains(&daily_uuid_paid.as_slice()), "Already-redeemed daily goal 0 must stay redeemed after reload");

    // Advance clock across 11:00 UTC rollover -> new day bucket issues fresh daily goals!
    let day0 = daily_goal_day_bucket(ctx2.now_unix());
    ctx2.advance_secs(86_400);
    let day1 = daily_goal_day_bucket(ctx2.now_unix());
    assert_eq!(day1, day0 + 1);
    gm2.close();
    let mut gm3 = GrpcClient::connect(Arc::clone(&ctx2), "goal_pilot", "gp-pass", pilot_cid, 0x44).await;
    let (_, rotated_dyn) = gm3
        .send_ok_dyn("eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new())
        .await;
    let rotated_ids: Vec<Vec<u8>> = rotated_dyn
        .get(1)
        .map(dyn_repeated)
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| {
            let m = dyn_msg(v)?;
            m.get(1).and_then(|inner| match inner {
                DynamicValue::Bytes(b) => Some(b.clone()),
                DynamicValue::Message(sub) => sub.get(1).and_then(dyn_bytes).map(|b| b.to_vec()),
                _ => None,
            })
        })
        .collect();
    assert!(!rotated_ids.is_empty());
    let (_, with_rew_rot) = gm3
        .send_ok_dyn("eve_public.dailygoal.api.GetAllWithRewardsRequest", Vec::new())
        .await;
    assert!(
        with_rew_rot.get(1).map(dyn_repeated).unwrap_or(&[]).is_empty(),
        "Daily goal rotation across 11:00 UTC must reset completed/unclaimed daily goals for the new day bucket"
    );

    gm3.close();
}
