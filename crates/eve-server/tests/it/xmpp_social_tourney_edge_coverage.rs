//! Integration tests for XMPP/Quasar social chat ACLs, moderation, presence,
//! character/GM notification emitters, petitioner/ingamereport/bug reporting,
//! lookup/bannedwords/search/userSvc, tourney arena/return ops, HyperNet raffle
//! validation/filters/expiry, Quasar inventory/cosmetics, and map heatmaps.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;

use crate::common;

async fn gm_slash(
    ctx: &ServerContext,
    gm: &mut SessionState,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        gm,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(cmd.into())],
        &kw,
        None,
    )
    .await
}

fn build_test_deflate_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut central_dir = Vec::new();
    for &(name, raw_data) in entries {
        let compressed = miniz_oxide::deflate::compress_to_vec(raw_data, 6);
        let local_offset = buf.len() as u32;
        let name_bytes = name.as_bytes();
        let comp_len = compressed.len() as u32;
        let uncomp_len = raw_data.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&8u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&comp_len.to_le_bytes());
        buf.extend_from_slice(&uncomp_len.to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&compressed);

        central_dir.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central_dir.extend_from_slice(&20u16.to_le_bytes());
        central_dir.extend_from_slice(&20u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&8u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u32.to_le_bytes());
        central_dir.extend_from_slice(&comp_len.to_le_bytes());
        central_dir.extend_from_slice(&uncomp_len.to_le_bytes());
        central_dir.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u16.to_le_bytes());
        central_dir.extend_from_slice(&0u32.to_le_bytes());
        central_dir.extend_from_slice(&local_offset.to_le_bytes());
        central_dir.extend_from_slice(name_bytes);
    }
    let cd_offset = buf.len() as u32;
    let cd_size = central_dir.len() as u32;
    let count = entries.len() as u16;
    buf.extend_from_slice(&central_dir);
    buf.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_offset.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA: u32 = 30_000_142;
const AMARR: u32 = 30_002_187;
const WORMHOLE_SYS: u32 = 31_000_005;
const ABYSSAL_SYS: u32 = 32_000_001;
const HYPERCORE_TYPE_ID: u32 = 52_568;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kw, None)
        .await
}

async fn login_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn login_gm(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let uname = format!("gm_t64_{char_id}");
    ctx.db
        .provision_gm_account(&uname, "gm64-secret", char_id, &format!("GM {char_id}"));
    login_char(ctx, sess_id, &uname, "gm64-secret", char_id).await
}

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(entries) => entries
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

fn encode_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

async fn send_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-t64-0000001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-t64-00000001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    let proto_bytes = match field(&rep, "response_proto") {
        PyRep::Bytes(b) => b,
        other => panic!("expected Bytes response_proto, got {other:?}"),
    };
    EvePublicResponse::decode_proto(proto_bytes).expect("decode EvePublicResponse")
}

// ---------------------------------------------------------------------------
// 1. XMPP / LSC Chat ACLs, Passwords, Mutes, Rate Limits, CSPA, & Prefs
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_xmpp_chat_acl_moderation_mute_rate_limit_cspa_and_prefs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_char(&ctx, 64_101, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_char(&ctx, 64_102, "other", "other-secret", PILOT_2).await;
    let gm_cid = 90_064_101;
    let mut gm = login_gm(&ctx, 64_103, gm_cid).await;

    // 1. GetHelpChannel language mapping + session fallback
    for (lang, expected_ch) in [
        ("de", "help_de"),
        ("fr", "help_fr"),
        ("ru", "help_ru"),
        ("ja", "help_ja"),
        ("zh", "help_zh"),
        ("ko", "help_ko"),
        ("es", "help_es"),
        ("zz", "help_en"),
    ] {
        let ch = rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "GetHelpChannel",
            &[PyRep::String(lang.into())],
        )
        .await
        .expect("GetHelpChannel");
        assert_eq!(ch, PyRep::String(expected_ch.into()));
    }
    s1.language_id = "FR".into();
    let fr_fallback = rpc(&ctx, &mut s1, "XmppChatMgr", "GetHelpChannel", &[])
        .await
        .expect("GetHelpChannel fallback");
    assert_eq!(fr_fallback, PyRep::String("help_fr".into()));

    // 2. GetRookieChannel (demo seeded at 1_704_067_200 -> "rookie_help")
    let rookie = rpc(&ctx, &mut s1, "XmppChatMgr", "GetRookieChannel", &[])
        .await
        .expect("GetRookieChannel rookie");
    assert_eq!(rookie, PyRep::String("rookie_help".into()));

    // 3. Create player channel & private 1:1 channel, test missing args, non-op invite denial, and missing channel errors
    assert_eq!(
        rpc(&ctx, &mut s1, "XmppChatMgr", "CreatePrivateChannel", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let priv_1on1 = rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "CreatePrivateChannel",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("CreatePrivateChannel");
    assert!(matches!(priv_1on1, PyRep::KeyVal(_)));

    let priv_ch = "channel:t64_vault";
    rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("t64_vault".into())],
    )
    .await
    .expect("CreatePlayerOwnedChannel");

    // Non-operator inviting someone else -> ChannelAccessDenied
    let non_op_add = rpc(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String(priv_ch.into()), PyRep::Integer(gm_cid)],
    )
    .await
    .unwrap_err();
    assert_eq!(non_op_add.code, "ChannelAccessDenied");

    // Missing channel name -> InvalidArgument
    let empty_add = rpc(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[])
        .await
        .unwrap_err();
    assert_eq!(empty_add.code, "InvalidArgument");

    // Non-GM adding another user to non-existent channel -> ChannelAccessDenied
    let missing_add = rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("channel:no_such_64".into()), PyRep::Integer(PILOT_2)],
    )
    .await
    .unwrap_err();
    assert_eq!(missing_add.code, "ChannelAccessDenied");

    // Join private channel succeeds after creation / invite
    rpc(
        &ctx,
        &mut s2,
        "LSC",
        "JoinChannel",
        &[PyRep::String(priv_ch.into())],
    )
    .await
    .expect("JoinChannel priv");

    // 4. AccessControl modes 3 (allow), 2 (operator), 1 (voice/unmute), -1 (mute), -2 (deny), 0 (clear)
    // Non-operator calling AccessControl -> NotChannelOperator
    let acl_denied = rpc(
        &ctx,
        &mut s2,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(gm_cid),
            PyRep::Integer(2),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(acl_denied.code, "NotChannelOperator");

    // Operator mutes PILOT_2 (-1) -> MutedInChannel on SendMessage
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(-1),
        ],
    )
    .await
    .expect("AccessControl -1");
    let ch_muted_err = rpc(
        &ctx,
        &mut s2,
        "LSC",
        "SendMessage",
        &[PyRep::String(priv_ch.into()), PyRep::String("muted!".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(ch_muted_err.code, "MutedInChannel");

    // Check GetDeprecatedPrefsFallback reflects muted status while joined
    let p2_prefs = rpc(&ctx, &mut s2, "XmppChatMgr", "GetDeprecatedPrefsFallback", &[])
        .await
        .expect("GetDeprecatedPrefsFallback");
    assert_eq!(
        field(p2_prefs.dict_get(priv_ch).expect("priv_ch entry"), "isMuted"),
        &PyRep::Bool(true)
    );

    // Promote PILOT_2 to operator (2) & verify SetChannelMOTD + unmute (1)
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("AccessControl 2");
    rpc(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "SetMotd",
        &[PyRep::String(priv_ch.into()), PyRep::String("Op MOTD".into())],
    )
    .await
    .expect("SetMotd by op");

    // GrantChannelOwnership errors: non-owner, missing channel, system channel, missing args
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(priv_ch.into()), PyRep::Integer(PILOT_2)],
        )
        .await
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String("channel:missing".into()), PyRep::Integer(PILOT_2)],
        )
        .await
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[
                PyRep::String(format!("solarsystemid2:{JITA}")),
                PyRep::Integer(PILOT_2)
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "XmppChatMgr", "GrantChannelOwnership", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // Non-operator SetMotd fails -> NotChannelOperator; allow & deny list checks
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "SetMotd",
            &[PyRep::String("channel:no_such_motd".into()), PyRep::String("hi".into())],
        )
        .await
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );

    // Deny list (-2) kicks and blocks re-join unless cleared (0) or allowed (3)
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(-2),
        ],
    )
    .await
    .expect("AccessControl -2");
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "LSC",
            "JoinChannel",
            &[PyRep::String(priv_ch.into())],
        )
        .await
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("AccessControl 1 allow");
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("AccessControl 0 clear");
    rpc(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(3),
        ],
    )
    .await
    .expect("AccessControl 3 owner");

    // 5. SendMessage validation: empty, >2000 chars, banned words, global mute, rate limit
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[PyRep::String(priv_ch.into()), PyRep::String("".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidChatMessageLength"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[
                PyRep::String(priv_ch.into()),
                PyRep::String("x".repeat(2001)),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidChatMessageLength"
    );
    let banned_err = rpc(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[
            PyRep::String(priv_ch.into()),
            PyRep::String("badword_pilot spam".into()),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(banned_err.code, "UserError");
    assert!(
        banned_err.message.contains("ChatContentInvalidBannedWords"),
        "unexpected banned-word error: {banned_err:?}"
    );

    // Global GM mute / unmute + unauthorized rejection
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "XmppChatMgr",
            "GMMute",
            &[PyRep::Integer(PILOT_1), PyRep::Integer(60)],
        )
        .await
        .unwrap_err()
        .code,
        "ChatAccessDenied"
    );
    rpc(
        &ctx,
        &mut gm,
        "XmppChatMgr",
        "GMMute",
        &[
            PyRep::Integer(PILOT_1),
            PyRep::Integer(60),
            PyRep::String("Spam".into()),
        ],
    )
    .await
    .expect("GMMute");
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[PyRep::String(priv_ch.into()), PyRep::String("hello".into())],
        )
        .await
        .unwrap_err()
        .code,
        "MutedInChannel"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "XmppChatMgr", "GMUnmute", &[PyRep::Integer(PILOT_1)])
            .await
            .unwrap_err()
            .code,
        "ChatAccessDenied"
    );
    rpc(&ctx, &mut gm, "XmppChatMgr", "GMUnmute", &[PyRep::Integer(PILOT_1)])
        .await
        .expect("GMUnmute");

    // Rate limit (>20 messages within same clock second -> ChatRateLimitExceeded)
    for i in 0..20 {
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "SendMessage",
            &[
                PyRep::String(priv_ch.into()),
                PyRep::String(format!("burst msg {i}")),
            ],
        )
        .await
        .expect("within rate limit");
    }
    let rl_err = rpc(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[PyRep::String(priv_ch.into()), PyRep::String("burst 21".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(rl_err.code, "ChatRateLimitExceeded");

    // Check GetChannels & GetMembers errors on missing/unauthorized
    let channels = rpc(&ctx, &mut s1, "LSC", "GetChannels", &[])
        .await
        .expect("GetChannels");
    assert!(
        matches!(channels, PyRep::Rowset { ref rows, .. } if !rows.is_empty()),
        "expected rowset: {channels:?}"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "GetMembers",
            &[PyRep::String("channel:no_such".into())]
        )
        .await
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );
    let denied_ch = rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("t64_denied".into())],
    )
    .await
    .expect("CreatePlayerOwnedChannel denied_ch");
    let denied_id = denied_ch.as_str().unwrap_or("channel:t64_denied").to_string();
    assert_eq!(
        rpc(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(denied_id)])
            .await
            .unwrap_err()
            .code,
        "ChannelAccessDenied"
    );

    // 6. ResyncSystemChannelAccess errors, Wormhole delayed local, Abyssal local join denial
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "ResyncSystemChannelAccess",
            &[PyRep::String(format!("solarsystemid2:{AMARR}"))],
        )
        .await
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "JoinChannel",
            &[PyRep::String(format!("solarsystemid2:{ABYSSAL_SYS}"))],
        )
        .await
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LSC",
            "JoinChannel",
            &[PyRep::String("corpid:99999999".into())],
        )
        .await
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    // Teleport GM to Wormhole -> ResyncSystemChannelAccess omits local and clears non-matching system
    gm_slash(&ctx, &mut gm, &format!("/tr {WORMHOLE_SYS}"))
        .await
        .expect("/tr wormhole");
    gm.solar_system_id = Some(WORMHOLE_SYS);
    let wh_local = format!("solarsystemid2:{WORMHOLE_SYS}");
    let wh_sync = rpc(
        &ctx,
        &mut gm,
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        &[PyRep::String(wh_local.clone())],
    )
    .await
    .expect("ResyncSystemChannelAccess wormhole");
    assert_eq!(field(&wh_sync, "channelID"), &PyRep::String(wh_local.clone()));
    assert_eq!(field(&wh_sync, "isDelayedLocal"), &PyRep::Bool(true));
    rpc(&ctx, &mut gm, "LSC", "JoinChannel", &[PyRep::String(wh_local.clone())])
        .await
        .expect("Join delayed WH local");
    let members_before = rpc(&ctx, &mut gm, "LSC", "GetMembers", &[PyRep::String(wh_local.clone())])
        .await
        .expect("GetMembers WH silent");
    assert_eq!(field(&members_before, "isDelayedLocal"), &PyRep::Bool(true));
    assert!(
        matches!(field(&members_before, "members"), PyRep::Rowset { rows, .. } if rows.is_empty()),
        "expected empty delayed WH members before speaking: {members_before:?}"
    );
    // Speaking in delayed local makes GM visible until leaving
    ctx.advance_secs(2);
    rpc(
        &ctx,
        &mut gm,
        "LSC",
        "SendMessage",
        &[PyRep::String(wh_local.clone()), PyRep::String("hello w-space".into())],
    )
    .await
    .expect("SendMessage in WH local");
    let members_after = rpc(&ctx, &mut gm, "LSC", "GetMembers", &[PyRep::String(wh_local.clone())])
        .await
        .expect("GetMembers WH revealed");
    assert!(
        matches!(
            field(&members_after, "members"),
            PyRep::Rowset { rows, .. } if rows.iter().any(|r| r.first() == Some(&PyRep::Integer(gm_cid)))
        ),
        "expected GM revealed in delayed WH local: {members_after:?}"
    );
    rpc(&ctx, &mut gm, "LSC", "LeaveChannel", &[PyRep::String(wh_local)])
        .await
        .expect("LeaveChannel WH local");

    // 7. Invite + blocked contact + CSPA charge + charMgr settings/notes
    rpc(
        &ctx,
        &mut s2,
        "charMgr",
        "SetNote",
        &[PyRep::Integer(PILOT_1), PyRep::String("ally note".into())],
    )
    .await
    .expect("SetNote");
    assert_eq!(
        rpc(&ctx, &mut s2, "charMgr", "GetNote", &[PyRep::Integer(PILOT_1)])
            .await
            .expect("GetNote"),
        PyRep::String("ally note".into())
    );
    rpc(
        &ctx,
        &mut s1,
        "charMgr",
        "LogSettings",
        &[PyRep::Dict(vec![(PyRep::String("cspa".into()), PyRep::Integer(100))])],
    )
    .await
    .expect("LogSettings");
    let settings = rpc(&ctx, &mut s1, "charMgr", "GetSettingsInfo", &[])
        .await
        .expect("GetSettingsInfo");
    assert!(matches!(settings, PyRep::Tuple(_)));

    let cspa_single = rpc(
        &ctx,
        &mut s1,
        "charMgr",
        "CSPAChargedAction",
        &[PyRep::String("CSPAChargedAction".into()), PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("CSPAChargedAction");
    assert_eq!(cspa_single, PyRep::Real(2950.0));
    let cspa_many = rpc(
        &ctx,
        &mut s1,
        "charMgr",
        "CSPAChargedActionForMany",
        &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(gm_cid)])],
    )
    .await
    .expect("CSPAChargedActionForMany");
    assert_eq!(cspa_many, PyRep::Real(5900.0));

    // Transfer ownership back from PILOT_2 (promoted to level 3 creator above) to PILOT_1
    rpc(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[PyRep::String(priv_ch.into()), PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("GrantChannelOwnership");
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(priv_ch.into()), PyRep::Integer(PILOT_2)],
        )
        .await
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
}

// ---------------------------------------------------------------------------
// 2. Quasar Local Chat, Delayed Wormhole Presence, & Gateway Errors
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_quasar_local_chat_presence_and_wormhole_delayed_mode() {
    use eve_server::services::quasar_social_chat as qchat;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_char(&ctx, 64_201, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_char(&ctx, 64_202, "other", "other-secret", PILOT_2).await;
    let gm_cid = 90_064_201;
    let mut gm = login_gm(&ctx, 64_203, gm_cid).await;
    let _nochar_uid = ctx.db.provision_player_account("nochar_t64", "nc64-pw");
    let mut nochar = SessionState::new(64_204);
    common::login_pilot(&ctx, &mut nochar, "nochar_t64", "nc64-pw").await;

    assert!(qchat::is_wormhole_system(WORMHOLE_SYS));
    assert!(!qchat::is_wormhole_system(JITA));
    assert!(qchat::is_delayed_local_system(WORMHOLE_SYS));

    // Unauth / missing char / invalid requests on GetMembershipListRequest
    let nochar_rsp = send_quasar(
        &ctx,
        &mut nochar,
        "eve_public.chat.local.api.GetMembershipListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(nochar_rsp.status_code, STATUS_FORBIDDEN);

    // K-space membership list returns pilots in Jita
    let now_u = ctx.now_unix();
    let _ = qchat::on_character_enter_local_chat(&ctx.quasar_gateway, &ctx.db, PILOT_1 as u32, JITA, now_u);
    let _ = qchat::on_character_enter_local_chat(&ctx.quasar_gateway, &ctx.db, PILOT_2 as u32, JITA, now_u);
    let mem_rsp = send_quasar(
        &ctx,
        &mut s1,
        "eve_public.chat.local.api.GetMembershipListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(mem_rsp.status_code, STATUS_OK);
    let q_mod = qchat::ChatModerationState::new(&ctx.db);
    let (_members, _cids) =
        qchat::get_solar_system_members(&ctx.db, None, JITA, Some(PILOT_1 as u32), None, Some(&q_mod));

    // BroadcastMessageRequest missing text -> 400 BAD_REQUEST
    let empty_msg_rsp = send_quasar(
        &ctx,
        &mut s1,
        "eve_public.chat.local.api.BroadcastMessageRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(empty_msg_rsp.status_code, STATUS_BAD_REQUEST);

    // Teleport GM to wormhole -> delayed mode reveals on speak, Abyssal -> 403
    gm_slash(&ctx, &mut gm, &format!("/tr {WORMHOLE_SYS}"))
        .await
        .expect("/tr gm wh");
    gm.solar_system_id = Some(WORMHOLE_SYS);
    let _ = qchat::on_character_enter_local_chat(
        &ctx.quasar_gateway,
        &ctx.db,
        gm_cid as u32,
        WORMHOLE_SYS,
        ctx.now_unix(),
    );

    let mut msg_buf = Vec::new();
    encode_string_field(1, "Wormhole ping", &mut msg_buf);
    let wh_bcast = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.BroadcastMessageRequest",
        msg_buf.clone(),
    )
    .await;
    assert_eq!(wh_bcast.status_code, STATUS_OK);
    assert!(wh_bcast
        .payload
        .as_ref()
        .is_some_and(|p| p.type_url.ends_with("BroadcastMessageResponse")));

    // Move GM to Abyssal -> GetMembershipListRequest and BroadcastMessageRequest -> 403
    gm_slash(&ctx, &mut gm, &format!("/tr {ABYSSAL_SYS}"))
        .await
        .expect("/tr gm abyss");
    gm.solar_system_id = Some(ABYSSAL_SYS);
    let aby_mem = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.GetMembershipListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(aby_mem.status_code, STATUS_FORBIDDEN);
    let aby_bcast = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.BroadcastMessageRequest",
        msg_buf.clone(),
    )
    .await;
    assert_eq!(aby_bcast.status_code, STATUS_FORBIDDEN);

    // Move GM back to Jita -> test admin.MuteRequest non-admin, bad input, not found, valid mute + expiry
    gm_slash(&ctx, &mut gm, &format!("/tr {JITA}"))
        .await
        .expect("/tr gm jita");
    gm.solar_system_id = Some(JITA);
    let _ = qchat::on_character_enter_local_chat(
        &ctx.quasar_gateway,
        &ctx.db,
        PILOT_1 as u32,
        JITA,
        ctx.now_unix(),
    );

    let non_admin_mute = send_quasar(
        &ctx,
        &mut s1,
        "eve_public.chat.local.api.admin.MuteRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(non_admin_mute.status_code, STATUS_FORBIDDEN);

    let bad_mute = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.admin.MuteRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bad_mute.status_code, STATUS_BAD_REQUEST);

    let mut mute_unknown = Vec::new();
    encode_seq_id(1, 999_999_999, &mut mute_unknown);
    let mut dur_buf = Vec::new();
    encode_varint_field(1, 60, &mut dur_buf);
    encode_bytes_field(2, &dur_buf, &mut mute_unknown);
    encode_string_field(3, "test reason", &mut mute_unknown);
    let nf_mute = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.admin.MuteRequest",
        mute_unknown,
    )
    .await;
    assert_eq!(nf_mute.status_code, STATUS_NOT_FOUND);

    let mut mute_p2 = Vec::new();
    encode_seq_id(1, PILOT_2 as u64, &mut mute_p2);
    encode_bytes_field(2, &dur_buf, &mut mute_p2);
    encode_string_field(3, "quasar mute", &mut mute_p2);
    let ok_mute = send_quasar(
        &ctx,
        &mut gm,
        "eve_public.chat.local.api.admin.MuteRequest",
        mute_p2,
    )
    .await;
    assert_eq!(ok_mute.status_code, STATUS_OK);
    let q_mod_after = qchat::ChatModerationState::new(&ctx.db);
    assert!(qchat::check_active_chat_mute(&q_mod_after, &ctx.db, PILOT_2 as u32, ctx.now_unix()).is_some());

    let _ = qchat::on_character_enter_local_chat(
        &ctx.quasar_gateway,
        &ctx.db,
        PILOT_2 as u32,
        JITA,
        ctx.now_unix(),
    );
    let muted_bcast = send_quasar(
        &ctx,
        &mut s2,
        "eve_public.chat.local.api.BroadcastMessageRequest",
        msg_buf,
    )
    .await;
    assert_eq!(muted_bcast.status_code, STATUS_FORBIDDEN);

    ctx.advance_secs(65);
    assert!(qchat::check_active_chat_mute(&q_mod_after, &ctx.db, PILOT_2 as u32, ctx.now_unix()).is_none());

    // Leave local chat & membership refresh + presence endpoints
    let _ = qchat::on_character_local_chat_membership_refreshed(
        &ctx.quasar_gateway,
        &ctx.db,
        PILOT_1 as u32,
        ctx.now_unix(),
    );
    let _ = qchat::on_character_leave_local_chat(
        &ctx.quasar_gateway,
        &ctx.db,
        PILOT_1 as u32,
        WORMHOLE_SYS,
        ctx.now_unix(),
    );
    let _ = qchat::on_character_leave_local_chat(
        &ctx.quasar_gateway,
        &ctx.db,
        PILOT_1 as u32,
        JITA,
        ctx.now_unix(),
    );

    let mut cid_arg = Vec::new();
    encode_seq_id(1, PILOT_1 as u64, &mut cid_arg);
    assert_eq!(
        send_quasar(
            &ctx,
            &mut s1,
            "eve_public.presence.api.GetCorporationRequest",
            cid_arg.clone(),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &mut s1,
            "eve_public.presence.api.GetAllianceRequest",
            cid_arg.clone(),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &mut s1,
            "eve_public.presence.api.GetFactionRequest",
            cid_arg,
        )
        .await
        .status_code,
        STATUS_OK
    );
    // Unknown char id -> 404 STATUS_NOT_FOUND
    let mut unknown_cid_arg = Vec::new();
    encode_seq_id(1, 999_999_999, &mut unknown_cid_arg);
    assert_eq!(
        send_quasar(
            &ctx,
            &mut s1,
            "eve_public.presence.api.GetCorporationRequest",
            unknown_cid_arg,
        )
        .await
        .status_code,
        STATUS_NOT_FOUND
    );
}

// ---------------------------------------------------------------------------
// 3. Notification Emitters & Batch3 GM Slash Commands
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_notify_emitters_character_and_batch3_gm_slash_cmds() {
    use eve_server::services::notify_emitters_character_services as ne;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_char(&ctx, 64_301, "demo", "demo-secret", PILOT_1).await;
    let mut gm = login_gm(&ctx, 64_302, 90_064_301).await;
    let uid1 = s1.user_id.expect("uid1");

    // Exercise direct notification emitters cleanly
    ne::emit_setattr_notifications(&ctx, PILOT_1, 30_000_142, 900_001, 12, 13_350_000_000_000_0000, 0.0, 1.0);
    ne::emit_setattr_notifications(&ctx, PILOT_1, 60_003_760, 900_002, 12, 13_350_000_000_000_0000, 42.0, 10.0);
    ne::emit_cluster_shutdown_cancelled(&ctx, "");
    ne::emit_cluster_shutdown_cancelled(&ctx, "UI/CancelledByGM");
    ne::emit_global_config_updated(&ctx, "testKey", &PyRep::Integer(7));
    ne::emit_air_npe_skipped(&ctx, PILOT_1);
    ne::emit_datacore_bought(&ctx, PILOT_1, 3019499, 1_500.0);
    ne::emit_agent_mission_dialogue(&ctx, PILOT_1, 3019499, 10, "Epic Title", "Epic Body", true);
    ne::emit_agent_mission_dialogue(&ctx, PILOT_1, 3019499, 1, "Std Title", "Std Body", false);
    ne::emit_allow_war_updated_for_alliance(&ctx, 99_000_001, true);
    ne::emit_invalidate_cache_for_locations(&ctx, eve_server::router::NotificationTarget::Character(PILOT_1), &[]);
    ne::emit_invalidate_cache_for_locations(&ctx, eve_server::router::NotificationTarget::Character(PILOT_1), &[60_003_760]);
    ne::emit_campaign_enrollment_changed(&ctx, PILOT_1);
    ne::emit_challenge_expired(&ctx, PILOT_1, 77, PyRep::keyval([("id", PyRep::Integer(77))]));
    ne::emit_contextual_offer_purchased(&ctx, PILOT_1, 0);
    ne::emit_contextual_offer_purchased(&ctx, PILOT_1, 88);
    ne::emit_crate_loot_spawned(&ctx, PILOT_1);
    ne::emit_fast_checkout_enabled_changed(&ctx, uid1, true);
    ne::emit_facilities_updated(&ctx, None, PILOT_1, 60_003_760);
    ne::emit_facilities_updated(&ctx, Some(JITA), PILOT_1, 60_003_760);
    ne::emit_logon_skills_trained(&ctx, PILOT_1, &[(3300, 2, 3, 12_000.0)], true);
    ne::emit_destination_change(&ctx, PILOT_1, JITA as i64, false, false);
    ne::emit_fleet_respawn_points_if_any(&ctx, 0);
    ne::emit_fleet_loot_event_if_in_fleet(&ctx, PILOT_1, None, &[(587, 1)]);
    ne::emit_alpha_injection_available(&ctx, PILOT_1);
    ne::emit_ccp_notification(
        &ctx,
        eve_server::router::NotificationTarget::Character(PILOT_1),
        "Title",
        "Subtitle",
        "Body",
        5,
        1,
    );
    ne::emit_community_fittings_updated(&ctx);
    ne::emit_login_milestone_achieved(&ctx, PILOT_1, 1, 50_000.0);
    ne::emit_milestone_timer_started(&ctx, PILOT_1, 300);

    let _monitors = ctx.db.with_snapshot(|snap| ne::load_industry_material_monitors(snap));
    ne::emit_industry_materials_consumed(
        &ctx,
        PILOT_1,
        9001,
        60_003_760,
        60_003_760,
        PILOT_1,
        false,
        eve_db::InventoryFlag::Hangar,
        None,
        &[(34, 25)],
    );

    let drained = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        drained.iter().any(|n| n.event == "OnModuleAttributeChanges" || n.event == "OnCCPNotification"),
        "expected pending notifications for PILOT_1"
    );

    // Non-GM slash rejections
    let deny = gm_slash(&ctx, &mut s1, "/fastcheckout 1")
        .await
        .unwrap_err();
    assert_eq!(deny.code, "InsufficientRole");

    // Batch 3 GM slash commands (valid + error paths)
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/chatban")
            .await
            .unwrap_err()
            .code,
        "InvalidSlashArguments"
    );
    gm_slash(&ctx, &mut gm, &format!("/chatban {PILOT_2} 120 test ban"))
        .await
        .expect("/chatban duration");
    gm_slash(&ctx, &mut gm, &format!("/chatban {PILOT_2} perm"))
        .await
        .expect("/chatban perm");
    gm_slash(&ctx, &mut gm, &format!("/chatunban {PILOT_2}"))
        .await
        .expect("/chatunban");

    gm_slash(&ctx, &mut gm, "/achievements reset")
        .await
        .expect("/achievements reset");
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/campaign enroll 0")
            .await
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );
    gm_slash(&ctx, &mut gm, "/campaign enroll 1")
        .await
        .expect("/campaign enroll 1");
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/campaign seasonal")
            .await
            .unwrap_err()
            .code,
        "InvalidSlashArguments"
    );
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/campaign seasonal 0")
            .await
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );
    gm_slash(&ctx, &mut gm, "/campaign seasonal 1 14 00:05:00")
        .await
        .expect("/campaign seasonal on");
    gm_slash(&ctx, &mut gm, "/campaign seasonal off")
        .await
        .expect("/campaign seasonal off");

    gm_slash(&ctx, &mut gm, "/shutdown 5 maintenance")
        .await
        .expect("/shutdown");
    gm_slash(&ctx, &mut gm, "/shutdown cancel")
        .await
        .expect("/shutdown cancel");
    gm_slash(&ctx, &mut gm, "/fastcheckout 0")
        .await
        .expect("/fastcheckout 0");
    gm_slash(&ctx, &mut gm, "/advancetime 60")
        .await
        .expect("/advancetime 60");
    gm_slash(&ctx, &mut gm, "/completeskills")
        .await
        .expect("/completeskills");
    gm_slash(&ctx, &mut gm, "/cleartimers")
        .await
        .expect("/cleartimers");
}

// ---------------------------------------------------------------------------
// 4. Petitioner, InGameReport, & Bug Reporting Edge/Error Paths
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_petitioner_ingamereport_and_bug_reporting_edge_paths() {
    use eve_server::bug_reporting::{
        build_client_global_config, extract_text_files_from_zip, handle_bugs_http_request,
    };
    use eve_server::services::ingamereport_ops;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut p1 = login_char(&ctx, 64_401, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 64_402, "other", "other-secret", PILOT_2).await;
    let mut gm1 = login_gm(&ctx, 64_403, 90_064_401).await;
    let mut gm2 = login_gm(&ctx, 64_404, 90_064_402).await;

    // Petitioner CreatePetition validation errors: (subject, description, category_id)
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "CreatePetition",
            &[
                PyRep::String("  ".into()),
                PyRep::String("body".into()),
                PyRep::Integer(4),
                PyRep::List(vec![]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPetitionSubject"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "CreatePetition",
            &[
                PyRep::String("Subj".into()),
                PyRep::String("body".into()),
                PyRep::Integer(999_999),
                PyRep::List(vec![]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPetitionCategory"
    );

    // Valid petition created by PILOT_1
    let pid_rep = rpc(
        &ctx,
        &mut p1,
        "petitioner",
        "CreatePetition",
        &[
            PyRep::String("Stuck ship".into()),
            PyRep::String("Please help".into()),
            PyRep::Integer(4),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("CreatePetition");
    let pid = match field(&pid_rep, "petitionID") {
        PyRep::Integer(v) => *v,
        other => panic!("expected Integer petitionID field, got {other:?}"),
    };

    // Wrong owner / non-GM access rejections
    assert_eq!(
        rpc(&ctx, &mut p2, "petitioner", "CancelPetition", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        rpc(&ctx, &mut p2, "petitioner", "ClaimPetition", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "petitioner",
            "PetitioneeChat",
            &[PyRep::Integer(pid), PyRep::String("hijack".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        rpc(&ctx, &mut p2, "petitioner", "GetLog", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut p1, "petitioner", "GetPetitionQueue", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );

    // Unclaimed UnClaim / Close / Escalate errors, then GM1 claims & GM2 conflict
    assert_eq!(
        rpc(&ctx, &mut gm1, "petitioner", "UnClaimPetition", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "PetitionNotClaimed"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm1,
            "petitioner",
            "EscalatePetition",
            &[PyRep::Integer(pid), PyRep::Integer(999)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPetitionQueue"
    );
    rpc(&ctx, &mut gm1, "petitioner", "ClaimPetition", &[PyRep::Integer(pid)])
        .await
        .expect("GM1 ClaimPetition");
    assert_eq!(
        rpc(&ctx, &mut gm2, "petitioner", "ClaimPetition", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "PetitionAlreadyClaimed"
    );

    // Rating before close -> PetitionNotClosed
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "AddPetitionRating",
            &[PyRep::Integer(pid), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "PetitionNotClosed"
    );

    // Chat, escalate, unclaim, close, rate, and inspect log/messages
    assert_eq!(
        rpc(
            &ctx,
            &mut gm1,
            "petitioner",
            "PetitionerChat",
            &[PyRep::Integer(pid), PyRep::String("non-petitioner".into())],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    rpc(
        &ctx,
        &mut p1,
        "petitioner",
        "PetitionerChat",
        &[PyRep::Integer(pid), PyRep::String("Thanks GM".into())],
    )
    .await
    .expect("PetitionerChat");
    rpc(
        &ctx,
        &mut gm1,
        "petitioner",
        "PetitioneeChat",
        &[PyRep::Integer(pid), PyRep::String("GM inspecting".into())],
    )
    .await
    .expect("PetitioneeChat");
    rpc(&ctx, &mut gm1, "petitioner", "UnClaimPetition", &[PyRep::Integer(pid)])
        .await
        .expect("UnClaimPetition");
    rpc(
        &ctx,
        &mut gm1,
        "petitioner",
        "EscalatePetition",
        &[PyRep::Integer(pid), PyRep::Integer(2)],
    )
    .await
    .expect("EscalatePetition");
    assert_eq!(
        rpc(&ctx, &mut gm1, "petitioner", "UnClaimPetition", &[PyRep::Integer(pid)])
            .await
            .unwrap_err()
            .code,
        "PetitionNotClaimed"
    );
    rpc(&ctx, &mut gm1, "petitioner", "ClosePetition", &[PyRep::Integer(pid)])
        .await
        .expect("ClosePetition");
    rpc(
        &ctx,
        &mut p1,
        "petitioner",
        "AddPetitionRating",
        &[PyRep::Integer(pid), PyRep::Integer(10), PyRep::String("Great".into())],
    )
    .await
    .expect("AddPetitionRating clamped");

    // Closed petition chat fails
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "PetitionerChat",
            &[PyRep::Integer(pid), PyRep::String("late msg".into())],
        )
        .await
        .unwrap_err()
        .code,
        "PetitionClosed"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm1,
            "petitioner",
            "PetitioneeChat",
            &[PyRep::Integer(pid), PyRep::String("late msg".into())],
        )
        .await
        .unwrap_err()
        .code,
        "PetitionClosed"
    );

    let log = rpc(&ctx, &mut gm1, "petitioner", "GetLog", &[PyRep::Integer(pid)])
        .await
        .expect("GetLog");
    assert!(matches!(log, PyRep::List(ref l) if l.len() >= 4));
    rpc(&ctx, &mut p1, "petitioner", "MarkAsRead", &[PyRep::Integer(pid)])
        .await
        .expect("MarkAsRead");
    let _ = rpc(&ctx, &mut p1, "petitioner", "GetPetitionMessages", &[PyRep::Integer(pid)])
        .await
        .expect("GetPetitionMessages");

    // Metadata/Picker RPCs
    for cat_id in [4, 401, 102, 101, 2, 201, 202, 1] {
        let props = rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "GetCategoryProperties",
            &[PyRep::Integer(cat_id)],
        )
        .await
        .expect("GetCategoryProperties");
        assert!(matches!(props, PyRep::List(_)));
    }
    for cat_id in [-6, -4, -3, -2, -1, -7, 1] {
        let may = rpc(&ctx, &mut p1, "petitioner", "MayPetition", &[PyRep::Integer(cat_id)])
            .await
            .expect("MayPetition");
        assert!(matches!(may, PyRep::Integer(_)));
    }
    for pop_type in ["SolarSystem", "Character", "OwnedShip"] {
        let pop = rpc(
            &ctx,
            &mut p1,
            "petitioner",
            "PropertyPopulationInfo",
            &[PyRep::String(pop_type.into())],
        )
        .await
        .expect("PropertyPopulationInfo");
        assert!(matches!(pop, PyRep::List(_)));
    }
    let jwt = rpc(&ctx, &mut p1, "petitioner", "GetZendeskJwtLink", &[])
        .await
        .expect("GetZendeskJwtLink");
    assert!(
        matches!(jwt, PyRep::String(ref s) if s.contains("https://support.eveonline.com/access/jwt")),
        "unexpected zendesk jwt: {jwt:?}"
    );
    let _ = rpc(&ctx, &mut p1, "petitioner", "GetUserCatalogCountry", &[])
        .await
        .expect("GetUserCatalogCountry");

    // ingamereport all 8 RPCs + V24 chat evidence
    for m in [
        "report_exploit_abuse",
        "report_gambling",
        "report_inappropriate_character_name",
        "report_inappropriate_language",
        "report_macro_use",
        "report_offensive_mail",
        "report_other",
        "report_questionable_transaction",
    ] {
        let res = rpc(
            &ctx,
            &mut p1,
            "ingamereport",
            m,
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Integer(1_760_000_100),
                PyRep::String(format!("solarsystemid2:{JITA}")),
                PyRep::String("evidence line".into()),
                PyRep::String("chat message".into()),
            ],
        )
        .await
        .expect(m);
        assert_eq!(res, PyRep::None);
    }
    let persisted = ingamereport_ops::get_persisted_reports(&ctx);
    assert_eq!(persisted.len(), 8);

    // bug_reporting HTTP & ZIP extraction & global config edge cases
    let h = HashMap::new();
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/status", &h, b"").status_code, 200);
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/categories", &h, b"").status_code, 200);
    assert_eq!(handle_bugs_http_request(&ctx, "GET", "/bugs/api/nope", &h, b"").status_code, 404);
    assert_eq!(handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &h, b"{invalid").status_code, 400);
    assert_eq!(
        handle_bugs_http_request(&ctx, "POST", "/bugs/api/attachment/../bad", &h, b"").status_code,
        400
    );

    assert!(extract_text_files_from_zip(b"too short", &["logs.txt"]).is_empty());
    let zip_bytes = build_test_deflate_zip(&[("logs.txt", b"log line 1\n")]);
    let extracted = extract_text_files_from_zip(&zip_bytes, &["logs.txt"]);
    assert_eq!(extracted.get("logs.txt").map(String::as_str), Some("log line 1\n"));

    let cfg_v6 = build_client_global_config(&ctx, Some("2001:db8::1"));
    assert_eq!(
        cfg_v6.get("ejabberdAddress"),
        Some(&PyRep::ascii_bytes(b"[2001:db8::1]")),
        "expected bracketed IPv6 ejabberdAddress: {cfg_v6:?}"
    );
    let cfg_loop = build_client_global_config(&ctx, Some("::1"));
    assert_eq!(
        cfg_loop.get("ejabberdAddress"),
        Some(&PyRep::ascii_bytes(b"[::1]")),
    );
}

// ---------------------------------------------------------------------------
// 5. lookup_svc (bannedwords / search / lookupSvc) & account_services
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_lookup_bannedwords_search_and_account_user_svc() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut p1 = login_char(&ctx, 64_501, "demo", "demo-secret", PILOT_1).await;
    let mut gm = login_gm(&ctx, 64_502, 90_064_501).await;

    // 1. bannedwords methods (string, list, tuple, none, disallowed)
    let regexes = rpc(&ctx, &mut p1, "bannedwords", "fetch_regexes", &[])
        .await
        .expect("fetch_regexes");
    assert!(matches!(regexes, PyRep::List(ref l) if !l.is_empty()));

    for method in ["check_words_allowed", "check_chat_words_allowed"] {
        assert_eq!(
            rpc(&ctx, &mut p1, "bannedwords", method, &[PyRep::String("safe text".into())])
                .await
                .expect(method),
            PyRep::None
        );
    }
    assert_eq!(
        rpc(&ctx, &mut p1, "bannedwords", "check_search_words_allowed", &[PyRep::String("safe text".into())])
            .await
            .expect("check_search_words_allowed"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "bannedwords",
            "check_words_allowed",
            &[PyRep::List(vec![
                PyRep::String("clean".into()),
                PyRep::String("badword_pilot".into())
            ])],
        )
        .await
        .expect("check_words_allowed list"),
        PyRep::List(vec![PyRep::Bool(true), PyRep::Bool(false)])
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "bannedwords",
            "check_words_allowed",
            &[PyRep::String("badword_pilot".into())],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // 2. search.Query & search.QuickQuery error paths + group filters
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "search",
            "Query",
            &[PyRep::String("".into()), PyRep::List(vec![PyRep::Integer(2)])],
        )
        .await
        .unwrap_err()
        .code,
        "SearchStringTooShort"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "search",
            "QuickQuery",
            &[PyRep::String("badword_pilot".into()), PyRep::List(vec![PyRep::Integer(2)])],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    let mut kw = HashMap::new();
    kw.insert("exact".into(), PyRep::Bool(false));
    kw.insert("hideNPC".into(), PyRep::Bool(true));
    let q_res = rpc_kw(
        &ctx,
        &mut p1,
        "search",
        "Query",
        &[
            PyRep::String("Jita".into()),
            PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::Integer(3),
                PyRep::Integer(4),
                PyRep::Integer(5),
                PyRep::Integer(6),
                PyRep::Integer(7),
                PyRep::Integer(8),
                PyRep::Integer(9),
                PyRep::Integer(10),
                PyRep::Integer(11),
                PyRep::Integer(15),
                PyRep::Integer(19),
                PyRep::Integer(32),
                PyRep::Integer(1404),
                PyRep::Integer(1657),
            ]),
        ],
        &kw,
    )
    .await
    .expect("search.Query all groups");
    assert!(matches!(q_res, PyRep::Dict(_)));

    // lookupSvc coverage
    let owners = rpc(
        &ctx,
        &mut p1,
        "lookupSvc",
        "LookupOwners",
        &[PyRep::String("Caldari".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupOwners");
    assert!(matches!(owners, PyRep::List(ref l) if !l.is_empty()));
    let locs = rpc(
        &ctx,
        &mut p1,
        "lookupSvc",
        "LookupLocationsByGroup",
        &[PyRep::Integer(5), PyRep::String("Jita".into())],
    )
    .await
    .expect("LookupLocationsByGroup");
    assert!(matches!(locs, PyRep::List(_)));

    // 3. account_services (userSvc, slash /setomega /setunderage, AmUnderage)
    assert_eq!(
        rpc(&ctx, &mut p1, "userSvc", "CastUpvote", &[PyRep::String("csm".into())])
            .await
            .expect("CastUpvote"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut p1, "userSvc", "CastDownvote", &[PyRep::String("csm".into())])
            .await
            .expect("CastDownvote"),
        PyRep::Bool(true)
    );
    assert!(matches!(
        rpc(&ctx, &mut p1, "userSvc", "ReportBot", &[PyRep::Integer(PILOT_2), PyRep::String("bot".into())])
            .await
            .expect("ReportBot"),
        PyRep::Integer(id) if id > 0
    ));
    assert!(matches!(
        rpc(&ctx, &mut p1, "userSvc", "ReportISKSpammer", &[PyRep::Integer(PILOT_2), PyRep::String("spam".into())])
            .await
            .expect("ReportISKSpammer"),
        PyRep::Integer(id) if id > 0
    ));
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "userSvc",
            "CreateRedeemTokenQA",
            &[PyRep::Integer(34), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "userSvc",
            "CreateRedeemTokenQA",
            &[PyRep::Integer(999_999_999), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidTypeID"
    );

    let uid1 = p1.user_id.expect("uid1");
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/setomega bad")
            .await
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        gm_slash(&ctx, &mut gm, "/setomega 999999999 2000000000")
            .await
            .unwrap_err()
            .code,
        "AccountNotFound"
    );
    gm_slash(&ctx, &mut gm, &format!("/setunderage {uid1} 1"))
        .await
        .expect("/setunderage 1");
    assert_eq!(
        rpc(&ctx, &mut p1, "authentication", "AmUnderage", &[])
            .await
            .expect("AmUnderage"),
        PyRep::Bool(true)
    );
    gm_slash(&ctx, &mut gm, &format!("/setomega {uid1} 1900000000"))
        .await
        .expect("/setomega");
}

// ---------------------------------------------------------------------------
// 6. Tournament Arena, Return Ops, TourneyMgr Edges & Anomaly/Channel Emitters
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tourney_arena_return_ops_and_anomaly_emitters() {
    use eve_server::services::notify_emitters_channel_events as nce;
    use eve_server::services::notify_emitters_tournaments_anomalies as nta;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut p1 = login_char(&ctx, 64_601, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 64_602, "other", "other-secret", PILOT_2).await;
    let gm_cid = 90_064_601;
    let mut gm = login_gm(&ctx, 64_603, gm_cid).await;

    // Direct anomaly, tournament, and channel-event emitters
    nta::emit_tournament_match_state_change(&ctx, 10, 5, 1, &[PILOT_1], &[PILOT_2], &[gm_cid]);
    nta::emit_enter_tournament_system(&ctx, PILOT_1);
    nta::emit_leave_tournament_system(&ctx, PILOT_1);
    nta::emit_proving_grounds_match_found(&ctx, PILOT_1);
    nta::emit_pvp_matchmaking_failure(&ctx, PILOT_1, vec!["QueueTimeout".into()]);
    nce::emit_operation_deactivated(&ctx, PILOT_1);
    nce::emit_operation_skipped(&ctx, PILOT_1, 1, 2);
    nce::emit_operations_progress_reset(&ctx, PILOT_1, &[1, 2], &[3], 1, 1, 1);
    nce::emit_operations_server_fsd_reload(&ctx, PILOT_1);
    nce::emit_dungeon_camera_interest_core(&ctx.db, None, None, PILOT_1, 100, 1.0, 2.0);
    nce::emit_dungeon_trigger_ambient_sound_core(&ctx.db, None, None, PILOT_1, 5, 10, 100, 0);
    nce::emit_dungeon_trigger_audio_core(&ctx.db, None, None, PILOT_1, 5, "aud");
    nce::emit_dungeon_trigger_message_core(&ctx.db, None, None, PILOT_1, 101, 202);
    nce::emit_dungeon_trigger_music_core(&ctx.db, None, None, PILOT_1, 5, 99);

    // Non-GM CreateMatch fails; GM CreateMatch succeeds
    assert_eq!(
        rpc(
            &ctx,
            &mut p1,
            "tourneyMgr",
            "CreateMatch",
            &[
                PyRep::Integer(1),
                PyRep::Integer(1001),
                PyRep::Integer(100),
                PyRep::Integer(600),
                PyRep::Integer(2),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "AdminAccessDenied"
    );
    let mid_rep = rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1001),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(2),
            PyRep::Integer(JITA as i64),
        ],
    )
    .await
    .expect("CreateMatch");
    let mid = match mid_rep {
        PyRep::Integer(id) => id,
        PyRep::Tuple(ref elems) => match elems.first() {
            Some(PyRep::Integer(id)) => *id,
            other => panic!("expected Tuple starting with match_id Integer, got {other:?}"),
        },
        other => panic!("expected match_id Integer or Tuple, got {other:?}"),
    };

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(mid), PyRep::Integer(0), PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("AddPlayer red");
    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "LockMemberList",
        &[PyRep::Integer(mid), PyRep::Integer(0)],
    )
    .await
    .expect("LockMemberList");
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "AddPlayer",
            &[PyRep::Integer(mid), PyRep::Integer(0), PyRep::Integer(PILOT_2)],
        )
        .await
        .unwrap_err()
        .code,
        "MemberListLocked"
    );

    // BanShip before autoban fills limit, then StartAutobanning fills limit -> next BanShipType hits TourneyBanLimitExceeded
    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "BanShip",
        &[
            PyRep::Integer(mid),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(590)]),
        ],
    )
    .await
    .expect("BanShip");
    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "StartAutobanning",
        &[
            PyRep::Integer(mid),
            PyRep::Integer(2),
            PyRep::Integer(PILOT_1),
            PyRep::Integer(PILOT_2),
            PyRep::Integer(2),
            PyRep::List(vec![PyRep::Integer(587), PyRep::Integer(588), PyRep::Integer(589), PyRep::Integer(590)]),
        ],
    )
    .await
    .expect("StartAutobanning");
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "BanShipType",
            &[PyRep::Integer(mid), PyRep::Integer(0), PyRep::Integer(592)],
        )
        .await
        .unwrap_err()
        .code,
        "TourneyBanLimitExceeded"
    );
    rpc(&ctx, &mut gm, "tourneyMgr", "FinalizeBans", &[PyRep::Integer(mid)])
        .await
        .expect("FinalizeBans");
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "BanShipType",
            &[PyRep::Integer(mid), PyRep::Integer(0), PyRep::Integer(591)],
        )
        .await
        .unwrap_err()
        .code,
        "TourneyBansFinalized"
    );

    // Override* flags, QueryShipList, GetFancyDetails, IsOtherMatchInProgress, CleanupGrid, Arena RPCs
    for m in [
        "OverrideDroneRestrict",
        "OverrideLockRestrict",
        "OverrideMoveRestrict",
        "OverrideWarpRestrict",
    ] {
        rpc(&ctx, &mut gm, "tourneyMgr", m, &[PyRep::Integer(mid), PyRep::Bool(true)])
            .await
            .expect(m);
    }
    let _ = rpc(&ctx, &mut gm, "tourneyMgr", "QueryShipList", &[PyRep::Integer(mid)])
        .await
        .expect("QueryShipList");
    let _ = rpc(&ctx, &mut gm, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(mid)])
        .await
        .expect("GetFancyDetails");
    let _ = rpc(&ctx, &mut gm, "tourneyMgr", "IsOtherMatchInProgress", &[PyRep::Integer(mid)])
        .await
        .expect("IsOtherMatchInProgress");
    rpc(&ctx, &mut gm, "tourneyMgr", "CleanupGrid", &[PyRep::Integer(mid)])
        .await
        .expect("CleanupGrid");

    // TourneyArena error paths: non-referee & InvalidLocation
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "tourneyMgr",
            "TeleportToArena",
            &[PyRep::Integer(mid), PyRep::Integer(0), PyRep::Integer(999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "tourneyMgr",
            "TeleportToArena",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "TeleportToArena",
            &[PyRep::Integer(mid)],
        )
        .await
        .expect("TeleportToArena"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "tourneyMgr",
            "TeleportToStation",
            &[PyRep::Integer(mid), PyRep::Integer(60_003_760)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "TeleportToStation",
            &[PyRep::Integer(mid), PyRep::Integer(60_003_760)],
        )
        .await
        .expect("TeleportToStation GM"),
        PyRep::Bool(true)
    );
    rpc(&ctx, &mut gm, "tourneyMgr", "ReturnAllPlayers", &[PyRep::Integer(mid)])
        .await
        .expect("ReturnAllPlayers");
    rpc(&ctx, &mut gm, "tourneyMgr", "TeleportRefHome", &[PyRep::Integer(mid)])
        .await
        .expect("TeleportRefHome");
}

// ---------------------------------------------------------------------------
// 7. HyperNet Raffle Validation, Filters, Subscriptions, Award & Expiry
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hypernet_raffle_validation_filters_and_expiry() {
    use eve_server::services::hypernet_raffle_ops::{compute_required_hypercores, sweep_expired_raffles};

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut p1 = login_char(&ctx, 64_701, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 64_702, "other", "other-secret", PILOT_2).await;
    let mut gm = login_gm(&ctx, 64_703, 90_064_701).await;

    // 1. compute_required_hypercores validation
    for bad_tickets in [0, 7, 999] {
        assert_eq!(
            compute_required_hypercores(10_000_000.0, bad_tickets).unwrap_err().code,
            "InvalidRaffleTicketCount"
        );
    }
    for bad_price in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            compute_required_hypercores(bad_price, 8).unwrap_err().code,
            "InvalidRafflePrice"
        );
    }
    assert!(compute_required_hypercores(50_000_000.0, 8).expect("valid cores") >= 1);

    // 2. Subscriptions: negative ID error & >256 eviction
    assert_eq!(
        rpc(&ctx, &mut p1, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    for id in 1..=260_i64 {
        rpc(&ctx, &mut p1, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(id)])
            .await
            .expect("SubscribeToRaffle");
    }
    rpc(&ctx, &mut p1, "raffleProxy", "UnsubscribeFromRaffle", &[PyRep::Integer(1)])
        .await
        .expect("UnsubscribeFromRaffle");
    assert_eq!(
        rpc(&ctx, &mut p1, "raffleProxy", "AmIBanned", &[])
            .await
            .expect("AmIBanned"),
        PyRep::Bool(false)
    );

    // 3. CreateRaffle error paths (duration, missing item, missing hypercores, then valid creation by GM)
    let gm_cid = 90_064_701;
    gm_slash(&ctx, &mut gm, "/createitem 587 1")
        .await
        .expect("/createitem ship");
    let gm_active_ship = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&gm_cid).map(|c| c.active_ship_id))
        .unwrap_or(0);
    let ship_item_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|it| {
                    it.owner_id == gm_cid
                        && it.type_id == 587
                        && it.flag == eve_db::InventoryFlag::Hangar
                        && it.item_id != gm_active_ship
                })
                .map(|it| it.item_id)
                .min()
        })
        .expect("created non-active 587 item for gm");

    // Bad duration (0)
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "raffleProxy",
            "CreateRaffle",
            &[
                PyRep::Integer(ship_item_id),
                PyRep::Integer(8),
                PyRep::Real(100_000.0),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRaffleDuration"
    );
    // Missing item
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "raffleProxy",
            "CreateRaffle",
            &[
                PyRep::Integer(999_999_888),
                PyRep::Integer(8),
                PyRep::Real(100_000.0),
                PyRep::Integer(3600),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    // InsufficientHyperCores before granting 52568
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "raffleProxy",
            "CreateRaffle",
            &[
                PyRep::Integer(ship_item_id),
                PyRep::Integer(8),
                PyRep::Real(100_000.0),
                PyRep::Integer(3600),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientHyperCores"
    );

    // Grant HyperCores via GM /createitem & create raffle successfully
    gm_slash(&ctx, &mut gm, &format!("/createitem {HYPERCORE_TYPE_ID} 500"))
        .await
        .expect("/createitem hypercores");

    let raffle_kv = rpc(
        &ctx,
        &mut gm,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(ship_item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
            PyRep::Integer(3600),
        ],
    )
    .await
    .expect("CreateRaffle");
    let rid = match field(&raffle_kv, "raffle_id") {
        PyRep::Integer(id) => *id,
        other => panic!("expected raffle_id integer, got {other:?}"),
    };

    // BuyTicket out-of-range, BuyRandomTickets <=0 & too many, valid ticket purchase by p2
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(rid), PyRep::Integer(99)],
        )
        .await
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(rid), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(rid), PyRep::Integer(20)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughTickets"
    );

    rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(rid), PyRep::Integer(0)],
    )
    .await
    .expect("BuyTicket 0");
    assert_eq!(
        rpc(
            &ctx,
            &mut p2,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(rid), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );
    assert_eq!(
        rpc(&ctx, &mut p2, "raffleProxy", "AwardItem", &[PyRep::Integer(rid)])
            .await
            .unwrap_err()
            .code,
        "NotRaffleWinner"
    );

    // FilteredGrab + Grab + GetMyActiveTickets + GetCreatedParticipated + GetActiveHistoricPrices
    let grab = rpc(
        &ctx,
        &mut p2,
        "raffleProxy",
        "FilteredGrab",
        &[PyRep::keyval([
            ("type_id", PyRep::Integer(587)),
            ("ticket_count", PyRep::Integer(8)),
            ("min_price", PyRep::Integer(0)),
            ("max_price", PyRep::Integer(10_000_000)),
        ])],
    )
    .await
    .expect("FilteredGrab");
    assert!(matches!(grab, PyRep::List(ref l) if !l.is_empty()));
    let _ = rpc(&ctx, &mut p2, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(587)])
        .await
        .expect("GetActiveHistoricPrices");

    // Advance past raffle expiry -> sweep_expired_raffles refunds p2 and returns item
    ctx.advance_secs(4000);
    let swept = sweep_expired_raffles(&ctx);
    assert_eq!(swept, 1);

    // QA_SeedRaffles non-GM denial & GM execution
    assert_eq!(
        rpc(&ctx, &mut p1, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    rpc(&ctx, &mut gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(2)])
        .await
        .expect("QA_SeedRaffles");
}

// ---------------------------------------------------------------------------
// 8. Quasar Inventory/Asset, Cosmetics/EverMarks, & Map Service Heatmaps
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_quasar_inventory_cosmetics_and_map_service_heatmap() {
    use eve_server::services::map_service;
    use eve_server::services::quasar_cosmetics_live::{
        activate_ship_emblem_license, broadcast_ship_skin_cosmetic_change, get_catalogue_price,
        ship_emblem_license_spec,
    };

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut p1 = login_char(&ctx, 64_801, "demo", "demo-secret", PILOT_1).await;
    let mut p2 = login_char(&ctx, 64_802, "other", "other-secret", PILOT_2).await;
    let mut gm = login_gm(&ctx, 64_803, 90_064_801).await;
    let mut anon = SessionState::new(64_804);

    // 1. Quasar Inventory & Asset endpoints
    let mut cat_req = Vec::new();
    encode_seq_id(1, 6, &mut cat_req);
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.inventory.category.GetRequest", cat_req)
            .await
            .status_code,
        STATUS_OK
    );
    let mut bad_cat = Vec::new();
    encode_seq_id(1, 999_999_999, &mut bad_cat);
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.inventory.category.GetRequest", bad_cat)
            .await
            .status_code,
        STATUS_NOT_FOUND
    );
    let mut search_req = Vec::new();
    encode_string_field(1, "Rifter", &mut search_req);
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.inventory.genericitem.SearchRequest", search_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.inventory.market_group.GetAllRequest", Vec::new())
            .await
            .status_code,
        STATUS_OK
    );

    // Asset auth / cross-character protection
    let mut p1_asset_req = Vec::new();
    encode_seq_id(1, PILOT_1 as u64, &mut p1_asset_req);
    assert_eq!(
        send_quasar(&ctx, &mut anon, "eve.asset.GetAssetsItemsRequest", p1_asset_req.clone())
            .await
            .status_code,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        send_quasar(&ctx, &mut p2, "eve.asset.GetAssetsItemsRequest", p1_asset_req.clone())
            .await
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.asset.GetAssetsItemsRequest", p1_asset_req.clone())
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(&ctx, &mut p2, "eve.asset.GetShipAssetsRequest", p1_asset_req.clone())
            .await
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &mut p1, "eve.asset.GetShipAssetsRequest", p1_asset_req)
            .await
            .status_code,
        STATUS_OK
    );

    // 2. Quasar Cosmetics / Paintwork / Emblem Licenses
    assert!(get_catalogue_price(35_832, 30 * 86_400).is_some());
    assert!(get_catalogue_price(999_999_999, 30 * 86_400).is_none());
    assert!(ship_emblem_license_spec(&ctx.sde, 34).is_none());

    assert_eq!(
        send_quasar(
            &ctx,
            &mut p1,
            "eve_public.cosmetic.structure.paintwork.license.api.GetCatalogueRequest",
            Vec::new(),
        )
        .await
        .status_code,
        STATUS_OK
    );
    // Empty paintwork IssueRequest -> 400 BAD_REQUEST
    assert_eq!(
        send_quasar(
            &ctx,
            &mut p1,
            "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest",
            Vec::new(),
        )
        .await
        .status_code,
        STATUS_BAD_REQUEST
    );
    // Palette CanEdit + GetOwn (initially 404 until SetRequest creates Corp 1 palette -> 200)
    assert_eq!(
        send_quasar(
            &ctx,
            &mut p1,
            "eve_public.cosmetic.corporation.palette.api.CanEditRequest",
            Vec::new(),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &mut p1,
            "eve_public.cosmetic.corporation.palette.api.GetOwnRequest",
            Vec::new(),
        )
        .await
        .status_code,
        STATUS_NOT_FOUND
    );
    let mut rgb_msg = Vec::new();
    encode_varint_field(1, 255, &mut rgb_msg);
    encode_varint_field(2, 128, &mut rgb_msg);
    encode_varint_field(3, 64, &mut rgb_msg);
    let mut palette_attrs = Vec::new();
    encode_bytes_field(1, &rgb_msg, &mut palette_attrs);
    let mut set_palette_req = Vec::new();
    encode_bytes_field(1, &palette_attrs, &mut set_palette_req);
    assert_eq!(
        send_quasar(
            &ctx,
            &mut p1,
            "eve_public.cosmetic.corporation.palette.api.SetRequest",
            set_palette_req,
        )
        .await
        .status_code,
        STATUS_FORBIDDEN
    );

    // activate_ship_emblem_license errors
    let bad_lic = activate_ship_emblem_license(&ctx, &mut p1, PILOT_1, 34, None);
    assert_eq!(bad_lic.unwrap_err().code, "InvalidSkinLicenseType");

    let active_ship = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.active_ship_id))
        .unwrap_or(1_000_001);
    broadcast_ship_skin_cosmetic_change(&ctx, PILOT_1, active_ship, 587, None, Some(1)).await;

    // 3. map_service: record visits/jumps + all GetHistory stat types (1..=5) + Abyssal guard
    map_service::record_solar_system_visit(&ctx, PILOT_1, JITA);
    map_service::record_solar_system_jump(&ctx, PILOT_1, AMARR);

    for stat_type in 1..=5_i64 {
        let hist = rpc(
            &ctx,
            &mut p1,
            "map",
            "GetHistory",
            &[PyRep::Integer(stat_type), PyRep::Integer(24)],
        )
        .await
        .expect("map.GetHistory");
        assert!(matches!(hist, PyRep::Object { .. } | PyRep::ObjectEx { .. } | PyRep::List(_)));
    }
    for method in [
        "GetBeaconCount",
        "GetConstellationLPData",
        "GetDeadspaceAgentsMap",
        "GetDeadspaceComplexMap",
        "GetEdencomFortressSystems",
        "GetEdencomMinorVictorySystems",
        "GetFacWarZoneInfo",
        "GetIncursionGlobalReport",
        "GetMyExtraMapInfo",
        "GetMyExtraMapInfoAgents",
        "GetRecentSovActivity",
        "GetSolarSystemVisits",
        "GetStationCount",
        "GetSystemsInIncursions",
        "GetTriglavianMinorVictorySystems",
        "GetAllRoamingWeatherSystems",
    ] {
        let res = rpc(&ctx, &mut p1, "map", method, &[]).await.expect(method);
        assert!(matches!(
            res,
            PyRep::List(_)
                | PyRep::Dict(_)
                | PyRep::Tuple(_)
                | PyRep::KeyVal(_)
                | PyRep::Object { .. }
                | PyRep::ObjectEx { .. }
                | PyRep::Integer(_)
        ));
    }

    // Abyssal map error guard
    gm_slash(&ctx, &mut gm, &format!("/tr {ABYSSAL_SYS}"))
        .await
        .expect("/tr gm abyssal");
    gm.solar_system_id = Some(ABYSSAL_SYS);
    assert_eq!(
        rpc(&ctx, &mut gm, "map", "GetHistory", &[PyRep::Integer(1), PyRep::Integer(24)])
            .await
            .unwrap_err()
            .code,
        "MapDisabledInAbyssalSpace"
    );
}
