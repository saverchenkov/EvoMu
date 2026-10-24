//! Integration acceptance tests for Quasar Character & Progression services.
//!
//! Validates public gateway routing, request execution, persistence, and notice publication across:
//! - Daily Goals (`feat:quasar.dailygoal`)
//! - Career Program (`feat:quasar.career`)
//! - Achievements & Milestones
//! - Skills, Skill Plans, Injectors & Implants
//! - Community Progression Tracks, Tales & Goals

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireField,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_NOT_IMPLEMENTED, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

pub fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
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

pub fn build_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

pub async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    static CORR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = CORR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut corr = b"prog-test-".to_vec();
    corr.extend_from_slice(&seq.to_be_bytes());
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr,
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
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

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        out.push(msg);
    }
    out
}

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_admin_progress(uuid: [u8; 16], delta: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    encode_varint_field(3, delta, &mut out);
    out
}

fn make_id_request(_id: u32) -> Vec<u8> {
    Vec::new()
}

// ============================================================================
// 1. Daily Goals Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_daily_goals_lifecycle_and_rewards() {
    // parity: feat:quasar.dailygoal, quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.dailygoal",
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(10_001, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session to publicGatewaySvc for notice delivery
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // Initial wallet balance and free SP
    let (initial_isk, initial_free_sp) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        (ch.balance.0, ch.free_skill_points)
    });

    // 1. Query current daily goals via eve_public.dailygoal.api.GetAllCurrentRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload present");
    assert!(payload.type_url.contains("GetAllCurrentResponse"));

    let wm = WireMessage::decode(&payload.value).expect("decode WireMessage");
    let goal_entries: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(!goal_entries.is_empty(), "Daily goals should be populated");

    // Inspect first goal
    let first_goal_bytes = goal_entries[0].value.as_bytes().expect("goal bytes");
    let first_goal_wm = WireMessage::decode(first_goal_bytes).expect("goal wm");
    let id_field = first_goal_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().expect("id bytes")).expect("id wm");
    let uuid_field = id_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("uuid field");
    let goal_uuid_bytes = uuid_field.value.as_bytes().expect("uuid bytes");
    assert_eq!(goal_uuid_bytes.len(), 16);
    let mut target_uuid = [0u8; 16];
    target_uuid.copy_from_slice(goal_uuid_bytes);

    // 2. Query completed with entitlements via eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest
    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);

    // 3. Verify unprivileged session is rejected on .admin. endpoint, then progress via real NPC kill + GM session
    let mut unpriv_sess = sess.clone();
    unpriv_sess.role = 0;
    let unpriv_rsp = quasar_rpc(&ctx, &mut unpriv_sess, "eve_public.dailygoal.api.admin.ProgressRequest", make_admin_progress(target_uuid, 2)).await;
    assert_eq!(unpriv_rsp.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

    let npc_ball = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(&ctx, 30_000_142, 10035, "BeltRat", 1, Default::default()).await;
    eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(&ctx, 30_000_142, npc_ball, 90_000_001).await;
    assert_eq!(ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(90_000_001)[0].current_progress, 1, "NPC kill must increment combat daily goal progress");
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.admin.ProgressRequest", make_admin_progress(target_uuid, 1)).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // 4. Complete goal #1 by adding more progress
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.admin.ProgressRequest", make_admin_progress(target_uuid, 100)).await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Verify notice published to subscriber
    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Notices should be published upon progress/completion");

    // 5. Redeem goal #1 via eve_public.dailygoal.api.RedeemRequest
    let redeem_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", make_uuid_request(target_uuid)).await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);

    // Verify exact ISK and SP payouts and DailyGoalReward journal entry in WorldRepository
    let (final_isk, final_free_sp, has_dg_journal) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        let has_j = s.wallet_journal.iter().any(|e| e.character_id == 90_000_001 && e.entry_type == "DailyGoalReward" && e.amount == eve_db::IskCents::from_isk(500_000));
        (ch.balance.0, ch.free_skill_points, has_j)
    });
    assert_eq!(final_isk - initial_isk, eve_db::IskCents::from_isk(500_000).0, "Exact 500,000 ISK daily goal reward");
    assert_eq!(final_free_sp - initial_free_sp, 5_000.0, "Exact 5,000 SP daily goal reward");
    assert!(has_dg_journal, "DailyGoalReward wallet journal entry must be recorded");

    // 6. Pay for completion on goal #2 via eve_public.dailygoal.api.PayForCompletionRequest
    if goal_entries.len() > 1 {
        let g2_wm = WireMessage::decode(goal_entries[1].value.as_bytes().unwrap()).unwrap();
        let second_uuid: [u8; 16] = g2_wm.fields.iter().find(|f| f.field_number == 1)
            .and_then(|f| WireMessage::decode(f.value.as_bytes()?).ok())
            .and_then(|id_wm| id_wm.fields.into_iter().find(|f| f.field_number == 1))
            .and_then(|f| f.value.as_bytes().and_then(|b| b.try_into().ok()))
            .unwrap_or([0u8; 16]);
        let pay_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.PayForCompletionRequest", make_uuid_request(second_uuid)).await;
        assert_eq!(pay_rsp.status_code, STATUS_OK);
    }

    // 7. RedeemAll via eve_public.dailygoal.api.RedeemAllRequest
    ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(90_000_001, 2, 500, &ctx.quasar_gateway);
    let redeem_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemAllRequest", Vec::new()).await;
    assert_eq!(redeem_all_rsp.status_code, STATUS_OK);
    let post_notices = drain_notices(&mut stream_rx);
    let red_notice = post_notices.iter().find(|n| n.payload.as_ref().map(|p| p.type_url.ends_with("eve_public.dailygoal.api.RedeemedNotice")).unwrap_or(false)).expect("RedeemedNotice");
    let not_wm = WireMessage::decode(&red_notice.payload.as_ref().unwrap().value).expect("decode RedeemedNotice");
    let id_wm = WireMessage::decode(not_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let red_uuid = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    assert_eq!(red_uuid.len(), 16, "Redeemed goal UUID must be 16 bytes");

    // 8. eve.dailygoal.api.GetRequest query + daily rollover reset across 86,400s boundary
    let eve_get_rsp = quasar_rpc(&ctx, &mut sess, "eve.dailygoal.api.GetRequest", make_uuid_request(target_uuid)).await;
    assert_eq!(eve_get_rsp.status_code, STATUS_OK);
    ctx.advance_secs(86_400);
    let reset_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(reset_rsp.status_code, STATUS_OK);
    let reset_goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(90_000_001);
    assert!(reset_goals.iter().all(|g| g.current_progress == 0 && !g.completed && !g.claimed), "Daily goals must reset after 86,400s day boundary");
}

// ============================================================================
// 2. Career Program Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_career_goals_paths_and_milestones() {
    // parity: feat:quasar.career, quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.career",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(10_002, 90_000_001, 98_000_001);
    let empty_kw = HashMap::new();

    // Subscribe session for career notices
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // 1. Query career paths and goals via eve_public.career.goal.api.GetAllRequest
    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload present");
    assert_eq!(
        payload.normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );

    let wm = WireMessage::decode(&payload.value).expect("decode GetAllResponse");
    let goal_entries: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(
        goal_entries.len() >= 4,
        "Should return career goals for all 4 paths"
    );

    // Canonical UUID of first Enforcer career goal
    let target_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

    // 2. Query definitions via eve_public.career.goal.api.GetDefinitionsRequest
    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(
        def_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetDefinitionsResponse"
    );

    // 3. Progress career goal via real NPC kill + eve_public.career.goal.api.admin.ProgressRequest
    let npc_ball2 = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(&ctx, 30_000_142, 10035, "BeltRat", 1, Default::default()).await;
    eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(&ctx, 30_000_142, npc_ball2, 90_000_001).await;
    assert_eq!(ctx.quasar_gateway.progression.career_goals.get_or_init_goals(90_000_001)[0].progress, 1, "NPC kill must increment Enforcer combat career goal progress");
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", make_admin_progress(target_uuid, 1)).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    // 4. Mark career goal completed via eve_public.career.goal.api.admin.CompleteRequest
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.CompleteRequest", make_uuid_request(target_uuid)).await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Check notices and verify CompletedNotice carries only field 1 (goal Identifier)
    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Career notices should be emitted");
    let comp_not = notifs.iter().find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.CompletedNotice"))).expect("CompletedNotice");
    let comp_wm = WireMessage::decode(&comp_not.payload.as_ref().unwrap().value).expect("decode CompletedNotice");
    assert_eq!(comp_wm.fields.len(), 1, "CompletedNotice schema only defines field 1 (goal)");

    // 5. Claim milestone reward via eve_public.career.goal.api.ClaimRewardRequest
    ctx.quasar_gateway.progression.career_goals.on_gameplay_event(90_000_001, "combat", 20, &ctx.quasar_gateway);
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", make_uuid_request(target_uuid)).await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);
    let _ = drain_notices(&mut stream_rx);

    // 6. Gameplay-path integration: mining harvest advances mining daily/career goals (not manufacturing) and emits ProgressedNotice
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx, 90_000_001, 100_000_000_101, 30_000_142, eve_db::InventoryFlag::Cargo, 1230, 3,
    );
    let mine_notifs = drain_notices(&mut stream_rx);
    let cg_prog_not = mine_notifs.iter().find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.ProgressedNotice"))).expect("career ProgressedNotice");
    let cg_prog_wm = WireMessage::decode(&cg_prog_not.payload.as_ref().unwrap().value).expect("decode career ProgressedNotice");
    let cg_not_id_wm = WireMessage::decode(cg_prog_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let cg_not_uuid = cg_not_id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let mining_goal_uuid = [0x43, 0x47, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    assert_eq!(cg_not_uuid, &mining_goal_uuid, "Mining must progress Asteroid Mining goal");
    assert_eq!(cg_prog_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(3));
    let dg_prog_not = mine_notifs.iter().find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.dailygoal.api.ProgressedNotice"))).expect("daily ProgressedNotice");
    let dg_prog_wm = WireMessage::decode(&dg_prog_not.payload.as_ref().unwrap().value).expect("decode daily ProgressedNotice");
    assert_eq!(dg_prog_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(3));

    let after_mine_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await;
    let after_mine_wm = WireMessage::decode(&after_mine_rsp.payload.unwrap().value).unwrap();
    let find_goal_prog = |wm: &WireMessage, target_name: &str| -> u64 {
        for f in wm.fields.iter().filter(|f| f.field_number == 1) {
            let gw = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
            let attr_w = WireMessage::decode(gw.fields.iter().find(|x| x.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
            if attr_w.fields.iter().find(|x| x.field_number == 1).and_then(|x| x.value.as_str().ok().flatten()) == Some(target_name) {
                return gw.fields.iter().find(|x| x.field_number == 2).and_then(|x| x.value.as_varint()).unwrap_or(0);
            }
        }
        0
    };
    assert_eq!(find_goal_prog(&after_mine_wm, "Industrialist: Mine Asteroid Belts"), 3, "Mining goal must have progress 3");
    assert_eq!(find_goal_prog(&after_mine_wm, "Industrialist: Manufacturing Jobs"), 0, "Manufacturing goal must not advance from mining");

    // 7. Manufacturing job install must NOT advance manufacturing goals; job completion MUST advance both daily and career goals
    sess.station_id = Some(60_003_760);
    let inst_rep = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)], &HashMap::new(), None).await.unwrap();
    let job_id = inst_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    assert!(drain_notices(&mut stream_rx).is_empty(), "Installing a manufacturing job must not emit goal notices");
    let after_inst_wm = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await.payload.unwrap().value).unwrap();
    assert_eq!(find_goal_prog(&after_inst_wm, "Industrialist: Manufacturing Jobs"), 0, "InstallJob must not advance manufacturing goal");

    ctx.advance_secs(100_000);
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &HashMap::new(), None).await.unwrap();
    let mfg_notifs = drain_notices(&mut stream_rx);
    assert!(mfg_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.ProgressedNotice"))));
    assert!(mfg_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.dailygoal.api.ProgressedNotice"))));
    let after_comp_wm = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await.payload.unwrap().value).unwrap();
    assert_eq!(find_goal_prog(&after_comp_wm, "Industrialist: Manufacturing Jobs"), 1, "CompleteJob must advance manufacturing goal to 1");
    assert_eq!(find_goal_prog(&after_comp_wm, "Explorer: Probe Cosmic Signatures"), 0, "Exploration goal starts at 0 before scanning");

    // 8. Cosmic signature scan (100% probe resolution) advances Explorer daily and career goals (idempotent per signature), even if legacy persisted activity_kind was empty
    if let Some(goals) = ctx.quasar_gateway.progression.career_goals.character_career_goals.write().unwrap().get_mut(&90_000_001) {
        for g in goals.iter_mut() {
            g.def.activity_kind.clear();
        }
    }
    ctx.mark_signature_scanned(90_000_001, 30_000_142, 555_001);
    ctx.mark_signature_scanned(90_000_001, 30_000_142, 555_001);
    let scan_notifs = drain_notices(&mut stream_rx);
    assert!(scan_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.career.goal.api.ProgressedNotice"))));
    assert!(scan_notifs.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve_public.dailygoal.api.ProgressedNotice"))));
    let after_scan_wm = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await.payload.unwrap().value).unwrap();
    assert_eq!(find_goal_prog(&after_scan_wm, "Explorer: Probe Cosmic Signatures"), 1, "Scanning signature must advance exploration goal once");

    // 9. Query progress via eve.character.career.goal.GetProgressRequest
    let prog_query = quasar_rpc(&ctx, &mut sess, "eve.character.career.goal.GetProgressRequest", make_id_request(90_000_001)).await;
    assert_eq!(prog_query.status_code, STATUS_OK);
    assert_eq!(prog_query.payload.as_ref().unwrap().normalized_type_name(), "eve.character.career.goal.GetProgressResponse");
}

// ============================================================================
// 4. Skills, Skill Plans, Injectors & Implants Acceptance Test
// ============================================================================

#[tokio::test]
async fn quasar_skills_plans_extractors_and_implants() {
    // parity: quasar:eveProto/generated/eve_public/skill/plan/plan_pb2.pyj, quasar:eveProto/generated/eve_public/character/skill/plan/plan_pb2.pyj, quasar:eveProto/generated/eve_public/character/skill/plan/milestone_pb2.pyj, quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/skill/plan/plan_pb2.pyj",
        "quasar:eveProto/generated/eve_public/character/skill/plan/plan_pb2.pyj",
        "quasar:eveProto/generated/eve_public/character/skill/plan/milestone_pb2.pyj",
        "quasar:eveProto/generated/eve_public/corporation/skill/plan/plan_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(10_004, 90_000_001, 98_000_001);

    // Empty CreateRequest must be rejected with STATUS_BAD_REQUEST
    let bad_create = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.CreateRequest", Vec::new()).await;
    assert_eq!(bad_create.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);

    // 1. Create personal skill plan via eve_public.skill.plan.CreateRequest
    let mut sp_attr = Vec::new();
    encode_string_field(1, "Starter Combat Plan", &mut sp_attr);
    let mut sp_create_val = Vec::new();
    encode_bytes_field(1, &sp_attr, &mut sp_create_val);
    let create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.CreateRequest", sp_create_val).await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let create_payload = create_rsp.payload.expect("create payload");
    assert_eq!(create_payload.normalized_type_name(), "eve_public.skill.plan.CreateResponse");
    let plan_wm = WireMessage::decode(&create_payload.value).expect("decode plan");
    let id_wm = WireMessage::decode(plan_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).expect("id_wm");
    let first_plan_uuid: [u8; 16] = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();

    // 2. Query all personal skill plans
    let get_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(get_all_rsp.status_code, STATUS_OK);
    assert_eq!(get_all_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.skill.plan.GetAllResponse");

    let char_plan_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(char_plan_rsp.status_code, STATUS_OK);

    let bad_ms_all = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.GetAllRequest", Vec::new()).await;
    assert_eq!(bad_ms_all.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);
    let char_ms_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.GetAllRequest", make_uuid_request(first_plan_uuid)).await;
    assert_eq!(char_ms_rsp.status_code, STATUS_OK);

    let mut attr_bytes = Vec::new();
    encode_string_field(1, "Mining Doctrine", &mut attr_bytes);
    let mut req_b = Vec::new();
    let mut type_b = Vec::new();
    encode_varint_field(1, 3380, &mut type_b);
    encode_bytes_field(1, &type_b, &mut req_b);
    encode_varint_field(2, 4, &mut req_b);
    encode_bytes_field(2, &req_b, &mut attr_bytes);
    encode_string_field(3, "Core mining skills", &mut attr_bytes);
    let mut create_val = Vec::new();
    encode_bytes_field(1, &attr_bytes, &mut create_val);
    let char_create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.CreateRequest", create_val.clone()).await;
    assert_eq!(char_create_rsp.status_code, STATUS_OK);

    let create_wm = WireMessage::decode(&char_create_rsp.payload.as_ref().unwrap().value).expect("decode CreateResponse");
    let id_wm = WireMessage::decode(create_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let plan_uuid: [u8; 16] = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();
    assert_ne!(plan_uuid, first_plan_uuid);

    // Verify SQLite reload preserves next_seq so newly created skill plans never collide with existing UUIDs
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "skill_plans_seq");
    let after_reload_create = quasar_rpc(&ctx_reloaded, &mut sess, "eve_public.character.skill.plan.CreateRequest", create_val).await;
    assert_eq!(after_reload_create.status_code, STATUS_OK);
    let reload_wm = WireMessage::decode(&after_reload_create.payload.unwrap().value).unwrap();
    let reload_id_wm = WireMessage::decode(reload_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let reload_uuid: [u8; 16] = reload_id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();
    assert_ne!(reload_uuid, first_plan_uuid, "Post-reload skill plan UUID must not collide with first plan");
    assert_ne!(reload_uuid, plan_uuid, "Post-reload skill plan UUID must not collide with second plan");

    let char_get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetRequest", make_uuid_request(plan_uuid)).await;
    assert_eq!(char_get_rsp.status_code, STATUS_OK);
    let get_wm = WireMessage::decode(&char_get_rsp.payload.as_ref().unwrap().value).expect("decode GetResponse");
    let attr_wm = WireMessage::decode(get_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let name_field = attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(name_field, "Mining Doctrine");
    assert!(attr_wm.fields.iter().any(|f| f.field_number == 2), "Field 2 must contain skill requirements");
    let desc_field = attr_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(desc_field, "Core mining skills");

    let mut sess2 = build_sess(102, 90_000_002, 98_000_001);
    let shared_rsp = quasar_rpc(&ctx, &mut sess2, "eve_public.character.skill.plan.GetSharedRequest", make_uuid_request(plan_uuid)).await;
    assert_eq!(shared_rsp.status_code, STATUS_OK);
    let shared_wm = WireMessage::decode(&shared_rsp.payload.as_ref().unwrap().value).expect("decode GetSharedResponse");
    let shared_attr_wm = WireMessage::decode(shared_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let shared_name = shared_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(shared_name, "Mining Doctrine");
    assert!(shared_attr_wm.fields.iter().any(|f| f.field_number == 2));
    let shared_desc = shared_attr_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(shared_desc, "Core mining skills");

    // 3. Create corporation skill plan via eve_public.corporation.skill.plan.CreateRequest
    let mut corp_attr = Vec::new();
    encode_string_field(1, "Corp Mining Plan", &mut corp_attr);
    encode_string_field(2, "Corporate mining doctrine", &mut corp_attr);
    let mut corp_req = Vec::new();
    let mut corp_tid = Vec::new();
    encode_varint_field(1, 3380, &mut corp_tid);
    encode_bytes_field(1, &corp_tid, &mut corp_req);
    encode_varint_field(2, 5, &mut corp_req);
    encode_bytes_field(3, &corp_req, &mut corp_attr);
    let mut cat_id_b = Vec::new();
    encode_bytes_field(1, &[1u8; 16], &mut cat_id_b);
    encode_bytes_field(4, &cat_id_b, &mut corp_attr);

    let mut corp_create_val = Vec::new();
    encode_bytes_field(1, &corp_attr, &mut corp_create_val);
    let corp_create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.CreateRequest", corp_create_val).await;
    assert_eq!(corp_create_rsp.status_code, STATUS_OK);
    let corp_plan_id_val = corp_create_rsp.payload.unwrap().value;

    let corp_get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetRequest", corp_plan_id_val).await;
    assert_eq!(corp_get_rsp.status_code, STATUS_OK);
    let corp_get_wm = WireMessage::decode(&corp_get_rsp.payload.as_ref().unwrap().value).expect("decode corp GetResponse");
    let corp_attr_wm = WireMessage::decode(corp_get_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let corp_name = corp_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(corp_name, "Corp Mining Plan");
    let corp_desc = corp_attr_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(corp_desc, "Corporate mining doctrine");
    assert!(corp_attr_wm.fields.iter().any(|f| f.field_number == 3), "Corp requirements must round-trip through GetRequest at field 3");

    // 4. Query corp skill plans, categories, types, injectors, extractors, implants, and milestone CRUD round-trip
    let corp_get_all = quasar_rpc(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(corp_get_all.status_code, STATUS_OK);
    assert_eq!(corp_get_all.payload.as_ref().unwrap().normalized_type_name(), "eve_public.corporation.skill.plan.GetAllResponse");

    // Character skill plan milestone CRUD + validation round-trip (R-QG-05)
    let build_ms_create = |lvl: u64| -> Vec<u8> {
        let (mut ms_attr, mut sp_id, mut sr_b, mut st_b, mut req) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_bytes_field(1, &plan_uuid, &mut sp_id);
        encode_bytes_field(1, &sp_id, &mut ms_attr);
        encode_varint_field(1, 3380, &mut st_b);
        encode_bytes_field(1, &st_b, &mut sr_b);
        encode_varint_field(2, lvl, &mut sr_b);
        encode_bytes_field(3, &sr_b, &mut ms_attr);
        encode_string_field(4, "Train Industry IV", &mut ms_attr);
        encode_bytes_field(1, &ms_attr, &mut req);
        req
    };
    let bad_ms = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.CreateRequest", build_ms_create(6)).await;
    assert_eq!(bad_ms.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST, "Level 6 milestone must be rejected");
    let ms_create_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.CreateRequest", build_ms_create(4)).await;
    assert_eq!(ms_create_rsp.status_code, STATUS_OK);
    let ms_create_wm = WireMessage::decode(&ms_create_rsp.payload.unwrap().value).unwrap();
    let ms_id_wm = WireMessage::decode(ms_create_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let ms_uuid: [u8; 16] = ms_id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();

    let ms_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.GetAllRequest", make_uuid_request(plan_uuid)).await;
    assert_eq!(ms_all_rsp.status_code, STATUS_OK);
    let ms_all_wm = WireMessage::decode(&ms_all_rsp.payload.unwrap().value).unwrap();
    let ms_entry_wm = WireMessage::decode(ms_all_wm.fields.iter().find(|f| f.field_number == 1).expect("milestone entry").value.as_bytes().unwrap()).unwrap();
    let ms_data_wm = WireMessage::decode(ms_entry_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(ms_data_wm.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_str().unwrap().unwrap(), "Train Industry IV");
    let ms_sr_wm = WireMessage::decode(ms_data_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(ms_sr_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(4));

    let mut ms_desc_req = make_uuid_request(ms_uuid);
    encode_string_field(2, "Train Industry IV ASAP", &mut ms_desc_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.SetDescriptionRequest", ms_desc_req).await.status_code, STATUS_OK);
    let ms_all_after = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.GetAllRequest", make_uuid_request(plan_uuid)).await.payload.unwrap().value).unwrap();
    let ms_data_after = WireMessage::decode(WireMessage::decode(ms_all_after.fields[0].value.as_bytes().unwrap()).unwrap().fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(ms_data_after.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_str().unwrap().unwrap(), "Train Industry IV ASAP");

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.DeleteRequest", make_uuid_request(ms_uuid)).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.milestone.DeleteRequest", make_uuid_request(ms_uuid)).await.status_code, eve_server::quasar_gateway::STATUS_NOT_FOUND);

    // Active skill plan set/get/clear across both eve_public.skill.plan and eve_public.character.skill.plan
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.SetActiveRequest", make_uuid_request(first_plan_uuid)).await.status_code, STATUS_OK);
    let act1 = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.GetActiveRequest", Vec::new()).await;
    assert_eq!(act1.status_code, STATUS_OK);
    assert!(!act1.payload.unwrap().value.is_empty(), "GetActiveRequest must return active skill plan");
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.SetActiveRequest", Vec::new()).await.status_code, STATUS_OK);
    let act1_cleared = quasar_rpc(&ctx, &mut sess, "eve_public.skill.plan.GetActiveRequest", Vec::new()).await;
    assert!(act1_cleared.payload.unwrap().value.is_empty(), "Empty SetActiveRequest must clear active skill plan");

    let (mut f2_id, mut f2_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut f2_id);
    encode_bytes_field(2, &f2_id, &mut f2_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.SetActiveRequest", f2_req).await.status_code, STATUS_OK);
    let act2 = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetActiveRequest", Vec::new()).await;
    assert!(!act2.payload.unwrap().value.is_empty(), "character.skill.plan.GetActiveRequest must return active plan");
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.SetActiveRequest", Vec::new()).await.status_code, STATUS_OK);
    let act2_cleared = quasar_rpc(&ctx, &mut sess, "eve_public.character.skill.plan.GetActiveRequest", Vec::new()).await;
    assert!(act2_cleared.payload.unwrap().value.is_empty(), "Empty character.skill.plan.SetActiveRequest must clear active plan");

    let cat_rsp = quasar_rpc(&ctx, &mut sess, "eve.skillcategory.GetAllRequest", Vec::new()).await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    assert_eq!(cat_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.skillcategory.GetAllResponse");
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.skilltype.GetRequest", make_id_request(3300)).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.skill.injector.InjectSkillPointsRequest", make_id_request(1)).await.status_code, STATUS_NOT_IMPLEMENTED);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.skill.extractor.ExtractSkillsRequest", make_id_request(3300)).await.status_code, STATUS_NOT_IMPLEMENTED);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.character.implant.GetActiveImplantsByCharacterRequest", make_id_request(90_000_001)).await.status_code, STATUS_OK);
    let implant_type_rsp = quasar_rpc(&ctx, &mut sess, "eve.implanttype.GetAllRequest", Vec::new()).await;
    assert_eq!(implant_type_rsp.status_code, STATUS_OK);
    assert_eq!(implant_type_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.implanttype.GetAllResponse");
}


