//! Behavioral integration tests for Quasar Academy, Accounting, ACL, Agent, AI, Alliance,
//! Ancestry, App, Asset, and Assetholding services.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicEvent,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::NoticeScope;
use eve_server::router::ServerContext;
use eve_server::services::quasar_b2_services::{
    publish_academy_borrowed_notice, publish_acl_membership_updated_notice,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

fn seed_test_fixtures(ctx: &ServerContext) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        // 1. Seed wallet journal entry for character 90_000_001
        if !snap.wallet_journal.iter().any(|e| e.character_id == 90_000_001) {
            snap.wallet_journal.push(eve_db::WalletJournalEntry {
                entry_id: 880_000_001,
                character_id: 90_000_001,
                entry_type: "player_trading".to_string(),
                amount: eve_db::IskCents::from_isk(100_000),
                balance_after: eve_db::IskCents::from_isk(1_000_100_000),
                reason: "Quasar Accounting Test Payment".to_string(),
            });
        }

        // 2. Seed alliance for character's corp 1_000_170
        let alliance_id = 99_000_001i64;
        let mut member_corp_ids = std::collections::HashSet::new();
        member_corp_ids.insert(1_000_170);
        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Federation Alliance".to_string(),
                short_name: "FED".to_string(),
                executor_corp_id: 1_000_170,
                member_corp_ids,
            },
        );
        snap.alliance_lifecycle_meta.insert(
            alliance_id,
            eve_db::AllianceLifecycleMeta {
                creator_id: 90_000_001,
                creator_corporation_id: 1_000_170,
                date_founded_unix: now - 86400,
                faction_id: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(alliance_id);
        }
        if let Some(corp) = snap.corporations.get_mut(&1_000_170) {
            corp.ceo_id = 90_000_001;
            corp.members.insert(90_000_001);
        }
    });
}

static CORR_SEQ: AtomicU64 = AtomicU64::new(100);

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT;
    sess
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-it-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-it".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

async fn quasar_publish_event(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) {
    let ev = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        ..Default::default()
    };
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[PyRep::Bytes(ev.encode_proto())],
            &kw,
            None,
        )
        .await
        .expect("PublishEvent");
    assert_eq!(rep, PyRep::Bool(true));
}

async fn subscribe_to_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
    notice_types: &[&str],
) {
    let args: Vec<PyRep> = notice_types
        .iter()
        .map(|s| PyRep::String(s.to_string()))
        .collect();
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "publicGatewaySvc",
        None,
        "SubscribeToNotice",
        &args,
        &kw,
        None,
    )
    .await
    .expect("SubscribeToNotice");
}

async fn poll_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
) -> Vec<(String, Vec<u8>)> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "PollNotices",
            &[],
            &kw,
            None,
        )
        .await
        .expect("PollNotices");

    let mut out = Vec::new();
    if let PyRep::List(items) = rep {
        for it in items {
            if let PyRep::KeyVal(kv) = it {
                let mut type_url = String::new();
                let mut payload = Vec::new();
                for (k, v) in kv {
                    if k == "type_url" {
                        if let PyRep::String(s) = v {
                            type_url = s;
                        }
                    } else if k == "payload" {
                        if let PyRep::Bytes(b) = v {
                            payload = b;
                        }
                    }
                }
                out.push((type_url, payload));
            }
        }
    }
    out
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

#[tokio::test]
async fn test_quasar_academy_agent_ancestry_and_acl() {
    // parity: feat:quasar.academy
    // parity: feat:quasar.acl
    // parity: feat:quasar.agent
    // parity: feat:quasar.ancestry
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(10, 1001, 90_000_001, 1_000_170);

    // 1. Quasar Academy: Notice subscription, notice delivery & public event publication
    subscribe_to_notices(&ctx, &mut sess, &["eve.academy.ships.events.Borrowed"]).await;
    let published_academy = publish_academy_borrowed_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        587,
        100_000_001,
        ctx.now_unix(),
    )
    .expect("publish academy notice");
    assert!(published_academy >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(
        notices
            .iter()
            .any(|(t, _)| t.ends_with("eve.academy.ships.events.Borrowed")),
        "Subscribed session must receive eve.academy.ships.events.Borrowed notice"
    );

    let mut exit_ev = Vec::new();
    encode_id_field(1, 90_000_001, &mut exit_ev);
    encode_id_field(2, 587, &mut exit_ev);
    encode_id_field(3, 30_000_142, &mut exit_ev);
    quasar_publish_event(
        &ctx,
        &mut sess,
        "eve_public.academy.ships.events.ExitAttempted",
        exit_ev,
    )
    .await;
    let events = ctx.quasar_gateway.published_events.read().unwrap();
    assert!(
        events.iter().any(|e| e
            .payload
            .as_ref()
            .is_some_and(|p| p.type_url.ends_with("eve_public.academy.ships.events.ExitAttempted"))),
        "Published events buffer must contain ExitAttempted"
    );
    drop(events);

    // 2. Quasar Agent: GetRequest by agent ID, 404 rejection & SearchRequest
    let mut ag_req = Vec::new();
    encode_id_field(1, 3008416, &mut ag_req);
    let ag_rsp = quasar_rpc(&ctx, &mut sess, "eve.agent.GetRequest", ag_req).await;
    assert_eq!(ag_rsp.status_code, 200, "ag_rsp: {}", ag_rsp.status_message);
    let ag_any = ag_rsp.payload.expect("agent GetResponse");
    assert!(ag_any.type_url.ends_with("eve.agent.GetResponse"));
    assert!(!ag_any.value.is_empty());

    let mut bad_ag = Vec::new();
    encode_id_field(1, 99_999_999, &mut bad_ag);
    let bad_rsp = quasar_rpc(&ctx, &mut sess, "eve.agent.GetRequest", bad_ag).await;
    assert_eq!(bad_rsp.status_code, 404, "bad_rsp: {}", bad_rsp.status_message);

    let mut search_req = Vec::new();
    let mut query_sub = Vec::new();
    encode_string_field(1, "Aura", &mut query_sub);
    encode_bytes_field(1, &query_sub, &mut search_req);
    let s_rsp = quasar_rpc(&ctx, &mut sess, "eve.agent.SearchRequest", search_req).await;
    assert_eq!(s_rsp.status_code, 200, "s_rsp: {}", s_rsp.status_message);
    let s_any = s_rsp.payload.expect("agent SearchResponse");
    assert!(s_any.type_url.ends_with("eve.agent.SearchResponse"));
    assert!(!s_any.value.is_empty());

    // 3. Quasar Ancestry: GetAllRequest
    let anc_rsp = quasar_rpc(&ctx, &mut sess, "eve.ancestry.GetAllRequest", Vec::new()).await;
    assert_eq!(anc_rsp.status_code, 200);
    let anc_any = anc_rsp.payload.expect("ancestry GetAllResponse");
    assert!(anc_any.type_url.ends_with("eve.ancestry.GetAllResponse"));
    assert!(!anc_any.value.is_empty());

    // 4. Quasar ACL: GetRequest, CanManageRequest, GetManagedRequest, notice delivery
    let mut acl_req = Vec::new();
    encode_id_field(1, 1, &mut acl_req);
    let acl_rsp = quasar_rpc(&ctx, &mut sess, "eve.acl.api.GetRequest", acl_req).await;
    assert_eq!(acl_rsp.status_code, 200);
    let acl_any = acl_rsp.payload.expect("acl GetResponse");
    assert!(acl_any.type_url.ends_with("eve.acl.api.GetResponse"));
    assert!(!acl_any.value.is_empty());

    let mut cm_req = Vec::new();
    encode_id_field(1, 1, &mut cm_req);
    encode_id_field(2, 90_000_001, &mut cm_req);
    let cm_rsp = quasar_rpc(&ctx, &mut sess, "eve.acl.api.CanManageRequest", cm_req).await;
    assert_eq!(cm_rsp.status_code, 200);
    let cm_any = cm_rsp.payload.expect("CanManageResponse");
    assert!(cm_any.type_url.ends_with("eve.acl.api.CanManageResponse"));
    assert_eq!(cm_any.value, vec![0x08, 0x01]);

    let mut cm_req2 = Vec::new();
    encode_id_field(1, 1, &mut cm_req2);
    encode_id_field(2, 90_000_002, &mut cm_req2);
    let cm_rsp2 = quasar_rpc(&ctx, &mut sess, "eve.acl.api.CanManageRequest", cm_req2).await;
    assert_eq!(cm_rsp2.status_code, 200);
    let cm_any2 = cm_rsp2.payload.expect("CanManageResponse");
    assert_eq!(cm_any2.value, vec![0x08, 0x00]);

    let mut gm_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut gm_req);
    let gm_rsp = quasar_rpc(&ctx, &mut sess, "eve.acl.api.GetManagedRequest", gm_req).await;
    assert_eq!(gm_rsp.status_code, 200);
    let gm_any = gm_rsp.payload.expect("GetManagedResponse");
    assert!(gm_any.type_url.ends_with("eve.acl.api.GetManagedResponse"));
    assert!(!gm_any.value.is_empty());

    subscribe_to_notices(&ctx, &mut sess, &["eve.acl.api.MembershipUpdated"]).await;
    publish_acl_membership_updated_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        1,
        2,
        ctx.now_unix(),
    )
    .expect("publish acl notice");
    let acl_notices = poll_notices(&ctx, &mut sess).await;
    assert!(acl_notices
        .iter()
        .any(|(t, _)| t.ends_with("eve.acl.api.MembershipUpdated")));
}

#[tokio::test]
async fn test_quasar_accounting_asset_and_assetholding() {
    // parity: feat:quasar.accounting
    // parity: feat:quasar.asset
    // parity: feat:quasar.assetholding
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(11, 1001, 90_000_001, 1_000_170);

    // 1. Quasar Accounting: Character entries, Corporation entries, EntryType list
    let mut acct_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut acct_req);
    let acct_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.accounting.character.journal.GetCharacterEntriesRequest",
        acct_req,
    )
    .await;
    assert_eq!(acct_rsp.status_code, 200);
    let acct_any = acct_rsp.payload.expect("GetCharacterEntriesResponse");
    assert!(acct_any
        .type_url
        .ends_with("eve.accounting.character.journal.GetCharacterEntriesResponse"));
    assert!(!acct_any.value.is_empty());

    let mut corp_id_sub = Vec::new();
    encode_varint_field(1, 1_000_170, &mut corp_id_sub);
    let mut corp_wallet_sub = Vec::new();
    encode_bytes_field(1, &corp_id_sub, &mut corp_wallet_sub);
    let mut corp_acct = Vec::new();
    encode_bytes_field(1, &corp_wallet_sub, &mut corp_acct);
    let corp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.accounting.corporation.journal.GetCorporationEntriesRequest",
        corp_acct,
    )
    .await;
    assert_eq!(corp_rsp.status_code, 200, "corp_rsp error: {}", corp_rsp.status_message);

    let et_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.accounting.entrytype.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(et_rsp.status_code, 200);

    // 2. Quasar Asset: GetAssetsItemsRequest & GetShipAssetsRequest
    let mut asset_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut asset_req);
    let asset_rsp =
        quasar_rpc(&ctx, &mut sess, "eve.asset.GetAssetsItemsRequest", asset_req).await;
    assert_eq!(asset_rsp.status_code, 200);
    let asset_any = asset_rsp.payload.expect("GetAssetItemsResponse");
    assert!(asset_any
        .type_url
        .ends_with("eve.asset.GetAssetItemsResponse"));
    assert!(!asset_any.value.is_empty());

    let mut ship_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut ship_req);
    let ship_rsp =
        quasar_rpc(&ctx, &mut sess, "eve.asset.GetShipAssetsRequest", ship_req).await;
    assert_eq!(ship_rsp.status_code, 200);
    let ship_any = ship_rsp.payload.expect("GetShipAssetsResponse");
    assert!(ship_any
        .type_url
        .ends_with("eve.asset.GetShipAssetsResponse"));

    // 3. Quasar Assetholding: Spawn, DirectDelivery, Entitlement query & GrantedNotice
    let (mut stack, mut stacks, mut spawn_payload) = (Vec::new(), Vec::new(), Vec::new());
    encode_id_field(1, 34, &mut stack); // Tritanium
    encode_varint_field(2, 50, &mut stack);
    encode_bytes_field(1, &stack, &mut stacks);
    encode_string_field(2, "Starter Pack Asset", &mut spawn_payload);
    encode_bytes_field(6, &stacks, &mut spawn_payload);

    let spawn_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.assetholding.asset.api.SpawnRequest",
        spawn_payload,
    )
    .await;
    assert_eq!(spawn_rsp.status_code, 200, "spawn_rsp error: {}", spawn_rsp.status_message);

    let mut asset_uuid = [0u8; 16];
    asset_uuid[0..8].copy_from_slice(b"ASSETHLD");
    asset_uuid[8..16].copy_from_slice(&1u64.to_be_bytes());

    let mut char_id_msg = Vec::new();
    encode_varint_field(1, 90_000_001, &mut char_id_msg);
    let mut holder_msg = Vec::new();
    encode_bytes_field(1, &char_id_msg, &mut holder_msg);

    let mut a_msg = Vec::new();
    encode_bytes_field(1, &asset_uuid, &mut a_msg);
    let mut g_msg = Vec::new();
    encode_string_field(1, "starter_group", &mut g_msg);
    let mut e_id = Vec::new();
    encode_bytes_field(1, &holder_msg, &mut e_id);
    encode_bytes_field(2, &a_msg, &mut e_id);
    encode_bytes_field(3, &g_msg, &mut e_id);

    let mut entry = Vec::new();
    encode_bytes_field(2, &e_id, &mut entry);
    encode_varint_field(10, 2, &mut entry);
    let mut dd_payload = Vec::new();
    encode_bytes_field(2, &entry, &mut dd_payload);

    let dd_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        dd_payload,
    )
    .await;
    assert_eq!(dd_rsp.status_code, 200);

    let mut ent_payload = Vec::new();
    encode_bytes_field(1, &e_id, &mut ent_payload);
    let get_ent_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.assetholding.entitlement.api.GetRequest",
        ent_payload,
    )
    .await;
    assert_eq!(get_ent_rsp.status_code, 200);
    let ent_any = get_ent_rsp.payload.expect("entitlement GetResponse");
    assert!(ent_any
        .type_url
        .ends_with("eve_public.assetholding.entitlement.api.GetResponse"));

    let mut unred_req = Vec::new();
    encode_bytes_field(1, &holder_msg, &mut unred_req);
    encode_string_field(2, "starter_group", &mut unred_req);
    let unred_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest",
        unred_req,
    )
    .await;
    assert_eq!(unred_rsp.status_code, 200);
    let unred_any = unred_rsp
        .payload
        .expect("GetUnredeemedByHolderAndGroupNameResponse");
    assert!(unred_any
        .type_url
        .ends_with("eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameResponse"));
    assert!(!unred_any.value.is_empty());

    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve_public.assetholding.entitlement.api.GrantedNotice"],
    )
    .await;
    let mut granted_payload = Vec::new();
    encode_id_field(1, 90_000_001, &mut granted_payload);
    encode_varint_field(2, 2, &mut granted_payload);
    let any_granted = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GrantedNotice"
        ),
        value: granted_payload,
    };
    ctx.quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            any_granted,
            ctx.now_unix(),
        )
        .expect("publish granted notice");
    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(notices
        .iter()
        .any(|(t, _)| t.ends_with("eve_public.assetholding.entitlement.api.GrantedNotice")));
}

#[tokio::test]
async fn test_quasar_alliance_ai_and_app() {
    // parity: feat:quasar.alliance
    // parity: feat:quasar.ai
    // parity: feat:quasar.app
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(12, 1001, 90_000_001, 1_000_170);

    // 1. Quasar Alliance: GetAllRequest, GetRequest, SearchRequest, GetMembersRequest
    let all_rsp = quasar_rpc(&ctx, &mut sess, "eve.alliance.GetAllRequest", Vec::new()).await;
    assert_eq!(all_rsp.status_code, 200);
    let all_any = all_rsp.payload.expect("GetAllResponse");
    assert!(all_any.type_url.ends_with("eve.alliance.GetAllResponse"));
    assert!(!all_any.value.is_empty());

    let mut get_all = Vec::new();
    encode_id_field(1, 99_000_001, &mut get_all);
    let get_rsp = quasar_rpc(&ctx, &mut sess, "eve.alliance.GetRequest", get_all).await;
    assert_eq!(get_rsp.status_code, 200);
    let get_any = get_rsp.payload.expect("GetResponse");
    assert!(get_any.type_url.ends_with("eve.alliance.GetResponse"));

    let mut query_sub = Vec::new();
    encode_string_field(1, "Federation", &mut query_sub);
    let mut search_all = Vec::new();
    encode_bytes_field(1, &query_sub, &mut search_all);
    let s_rsp = quasar_rpc(&ctx, &mut sess, "eve.alliance.SearchRequest", search_all).await;
    assert_eq!(s_rsp.status_code, 200, "alliance search error: {}", s_rsp.status_message);
    let s_any = s_rsp.payload.expect("SearchResponse");
    assert!(s_any.type_url.ends_with("eve.alliance.SearchResponse"));
    assert!(!s_any.value.is_empty());

    let mut mem_req = Vec::new();
    encode_id_field(1, 99_000_001, &mut mem_req);
    let mem_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.alliance.corporation.GetMembersRequest",
        mem_req,
    )
    .await;
    assert_eq!(mem_rsp.status_code, 200);

    // 2. Quasar AI Assistant: QueryRequest and Queried notice
    subscribe_to_notices(&ctx, &mut sess, &["eve.ai.assistant.chat.api.Queried"]).await;
    let mut ai_req = Vec::new();
    encode_string_field(1, "Jita", &mut ai_req);
    let ai_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ai.assistant.chat.api.QueryRequest",
        ai_req,
    )
    .await;
    assert_eq!(ai_rsp.status_code, 200, "ai_rsp error: {}", ai_rsp.status_message);
    let ai_any = ai_rsp.payload.expect("QueryResponse");
    assert!(ai_any
        .type_url
        .ends_with("eve_public.ai.assistant.chat.api.QueryResponse"));
    assert!(!ai_any.value.is_empty());

    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(
        notices
            .iter()
            .any(|(t, _)| t.ends_with("eve.ai.assistant.chat.api.Queried")),
        "Subscribed session must receive eve.ai.assistant.chat.api.Queried notice"
    );

    // 3. Quasar App: Career goals query and Agency UI action / bookmark events
    let goals_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(goals_rsp.status_code, 200);

    let mut act_ev = Vec::new();
    encode_varint_field(1, 10, &mut act_ev);
    encode_varint_field(2, 1, &mut act_ev);
    quasar_publish_event(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.agency.ui.action.Taken",
        act_ev,
    )
    .await;

    let mut bm_ev = Vec::new();
    encode_varint_field(1, 42, &mut bm_ev);
    quasar_publish_event(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.agency.ui.bookmark.Added",
        bm_ev,
    )
    .await;
}
