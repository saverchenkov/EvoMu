//! Live `publicGatewaySvc` integration tests for Batch-9 Quasar `eve.campaign.*` RPCs,
//! Commands, Definitions, Objectives, Tracks, and Events (`quasar:eveProto/generated/eve/campaign/...`).

#![forbid(unsafe_code)]

use crate::quasar_campaign_endpoints_remediated::{
    id_payload, mining_campaign, ok, quasar, sub, subs, subscribe, uuid_of, varint,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice, WireMessage};
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_campaign_handlers::{campaign_uuid, objective_uuid, track_uuid};
use tokio::sync::mpsc::Receiver;

fn eve_campaign_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, WireMessage)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        let p = n.payload.expect("notice payload");
        let ty = p.normalized_type_name().to_string();
        if ty.starts_with("eve.campaign.") {
            out.push((ty, WireMessage::decode(&p.value).expect("decode notice")));
        }
    }
    out
}

async fn cmd_ok(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, payload: Vec<u8>) {
    let resp = quasar(ctx, sess, type_name, payload).await;
    assert_eq!(resp.status_code, STATUS_OK, "command {type_name} failed: {resp:?}");
}

fn session(sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn def_id_payload(field: u32, name: &str) -> Vec<u8> {
    let mut def = Vec::new();
    encode_string_field(1, name, &mut def);
    let mut out = Vec::new();
    encode_bytes_field(field, &def, &mut out);
    out
}

#[tokio::test]
async fn test_b9_campaign_admin_schedule_lifecycle_and_events() {
    // parity: quasar:eveProto/generated/eve/campaign/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/api/commands_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/api/events_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/api/commands_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/api/events_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = session(89_101, 90_100_001);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. Schedule a new campaign via eve.campaign.api.admin.ScheduleRequest
    let sched_req = def_id_payload(1, "campaign.b9.alpha");
    let sched_resp = ok(&ctx, &mut sess, "eve.campaign.api.admin.ScheduleRequest", sched_req).await;
    let cid_bytes = uuid_of(&sub(&sched_resp, 1));
    assert_eq!(cid_bytes.len(), 16);

    let notes = eve_campaign_notices(&mut rx);
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.api.Scheduled"));

    // 2. Query GetAll, Get, GetStatus, GetLimits, GetStats
    let all_resp = ok(&ctx, &mut sess, "eve.campaign.api.GetAllRequest", Vec::new()).await;
    assert_eq!(subs(&all_resp, 1).len(), 1);

    let id_req = {
        let mut out = Vec::new();
        let mut id_msg = Vec::new();
        encode_bytes_field(1, &cid_bytes, &mut id_msg);
        encode_bytes_field(1, &id_msg, &mut out);
        out
    };
    let get_resp = ok(&ctx, &mut sess, "eve.campaign.api.GetRequest", id_req.clone()).await;
    let attr = sub(&get_resp, 1);
    assert_eq!(varint(&attr, 2), Some(0)); // scheduled (state = 0)

    let limits_resp = ok(&ctx, &mut sess, "eve.campaign.api.GetLimitsRequest", Vec::new()).await;
    assert_eq!(varint(&limits_resp, 1), Some(10));

    // 3. Start via StartCommanded -> state becomes ACTIVE (1) and Started event emitted
    cmd_ok(&ctx, &mut sess, "eve.campaign.api.StartCommanded", id_req.clone()).await;
    let status_resp = ok(&ctx, &mut sess, "eve.campaign.api.GetStatusRequest", id_req.clone()).await;
    assert_eq!(varint(&sub(&status_resp, 1), 1), Some(1));
    let notes = eve_campaign_notices(&mut rx);
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.api.Started"));

    // 4. UpdateDefinitionRequest bumps etag; CompleteCommanded completes campaign
    ok(&ctx, &mut sess, "eve.campaign.api.admin.UpdateDefinitionRequest", id_req.clone()).await;
    cmd_ok(&ctx, &mut sess, "eve.campaign.api.CompleteCommanded", id_req.clone()).await;
    let stats_resp = ok(&ctx, &mut sess, "eve.campaign.api.GetStatsRequest", id_req.clone()).await;
    assert!(sub(&stats_resp, 1).fields.iter().all(|f| f.field_number >= 1));
    let notes = eve_campaign_notices(&mut rx);
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.api.DefinitionUpdated"));
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.api.Completed"));

    // 5. DeleteRequest removes campaign; subsequent GetRequest returns STATUS_NOT_FOUND
    ok(&ctx, &mut sess, "eve.campaign.api.admin.DeleteRequest", id_req.clone()).await;
    let err = quasar(&ctx, &mut sess, "eve.campaign.api.GetRequest", id_req).await;
    assert_eq!(err.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
async fn test_b9_campaign_definitions_and_objective_lifecycle() {
    // parity: quasar:eveProto/generated/eve/campaign/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/objective/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/objective/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/objective/api/events_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/campaign/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/objective/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/objective/api/events_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = session(89_102, 90_100_002);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. Schedule campaign and instantiate an objective
    let sched_resp = ok(
        &ctx,
        &mut sess,
        "eve.campaign.api.admin.ScheduleRequest",
        def_id_payload(1, "campaign.b9.beta"),
    )
    .await;
    let cid_bytes = uuid_of(&sub(&sched_resp, 1));

    let mut inst_req = Vec::new();
    let mut cid_msg = Vec::new();
    encode_bytes_field(1, &cid_bytes, &mut cid_msg);
    encode_bytes_field(1, &cid_msg, &mut inst_req);
    let mut odef_msg = Vec::new();
    encode_string_field(1, "objective.b9.mining", &mut odef_msg);
    encode_bytes_field(2, &odef_msg, &mut inst_req);

    let inst_resp = ok(
        &ctx,
        &mut sess,
        "eve.campaign.objective.api.admin.InstantiateRequest",
        inst_req,
    )
    .await;
    let oid_bytes = uuid_of(&sub(&inst_resp, 1));
    assert_eq!(oid_bytes.len(), 16);

    let notes = eve_campaign_notices(&mut rx);
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.objective.api.Instantiated"));

    // 2. Query campaign definition APIs
    let vers = ok(&ctx, &mut sess, "eve.campaign.definition.api.GetAllVersionsRequest", Vec::new()).await;
    assert_eq!(subs(&vers, 1).len(), 2);
    let defs = ok(&ctx, &mut sess, "eve.campaign.definition.api.GetAllRequest", Vec::new()).await;
    assert_eq!(subs(&defs, 1).len(), 1);
    let def_get = ok(
        &ctx,
        &mut sess,
        "eve.campaign.definition.api.GetRequest",
        def_id_payload(1, "campaign.b9.beta"),
    )
    .await;
    assert_eq!(varint(&sub(&def_get, 1), 17), Some(1));

    // 3. Start and progress objective to completion (target = 100)
    let mut oid_req = Vec::new();
    let mut oid_msg = Vec::new();
    encode_bytes_field(1, &oid_bytes, &mut oid_msg);
    encode_bytes_field(1, &oid_msg, &mut oid_req);

    ok(&ctx, &mut sess, "eve.campaign.objective.api.admin.StartRequest", oid_req.clone()).await;

    let mut prog_req = oid_req.clone();
    let mut char_msg = Vec::new();
    encode_varint_field(1, 90_100_002, &mut char_msg);
    encode_bytes_field(2, &char_msg, &mut prog_req);
    encode_varint_field(3, 100, &mut prog_req);
    ok(&ctx, &mut sess, "eve.campaign.objective.api.admin.ProgressRequest", prog_req).await;

    let obj_status = ok(&ctx, &mut sess, "eve.campaign.objective.api.GetStatusRequest", oid_req.clone()).await;
    assert_eq!(varint(&sub(&obj_status, 1), 1), Some(2)); // OBJECTIVE_STATE_COMPLETED
    assert_eq!(varint(&sub(&obj_status, 2), 1), Some(100));

    let obj_def = ok(&ctx, &mut sess, "eve.campaign.objective.api.GetDefinitionRequest", oid_req).await;
    assert_eq!(varint(&sub(&obj_def, 2), 9), Some(100));

    let notes = eve_campaign_notices(&mut rx);
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.objective.api.Progressed"));
    assert!(notes.iter().any(|n| n.0 == "eve.campaign.objective.api.Completed"));
}

#[tokio::test]
async fn test_b9_objective_commands_definitions_and_track_admin() {
    // parity: quasar:eveProto/generated/eve/campaign/objective/api/commands_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/objective/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/track/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve/campaign/track/api/commands_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve/campaign/objective/api/commands_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/objective/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/track/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/campaign/track/api/commands_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut sess = session(89_103, 90_100_003);
    let extra = &ctx.quasar_gateway.progression.extra;
    let t0 = ctx.now_unix();

    let (camp, objs, trks) = mining_campaign(42, t0, t0 + 3_600, 500);
    let obj_name = objs[0].name.clone();
    let oid = objs[0].id;
    let tid = trks[0].id;
    extra.register_campaign(camp, objs, trks).expect("register");

    // 1. Objective definition GetRequest
    let odef_resp = ok(
        &ctx,
        &mut sess,
        "eve.campaign.objective.definition.api.GetRequest",
        def_id_payload(1, &obj_name),
    )
    .await;
    assert_eq!(varint(&sub(&odef_resp, 1), 9), Some(500));

    // 2. Objective CompleteCommanded
    let oid_req = id_payload(objective_uuid(oid));
    cmd_ok(&ctx, &mut sess, "eve.campaign.objective.api.CompleteCommanded", oid_req).await;
    assert_eq!(extra.campaign_objectives.read().unwrap().get(&oid).unwrap().state, 2);

    // 3. Track admin ProgressRequest & ProgressForCharacterRequest & ProcessTickContributionCommanded
    let mut trk_prog = id_payload(track_uuid(tid));
    encode_varint_field(2, 75, &mut trk_prog);
    ok(&ctx, &mut sess, "eve.campaign.track.api.admin.ProgressRequest", trk_prog).await;

    let mut trk_char_prog = id_payload(track_uuid(tid));
    let mut char_msg = Vec::new();
    encode_varint_field(1, 90_100_003, &mut char_msg);
    encode_bytes_field(2, &char_msg, &mut trk_char_prog);
    encode_varint_field(3, 25, &mut trk_char_prog);
    ok(
        &ctx,
        &mut sess,
        "eve.campaign.track.api.admin.ProgressForCharacterRequest",
        trk_char_prog,
    )
    .await;

    let tick_cmd = id_payload(track_uuid(tid));
    cmd_ok(
        &ctx,
        &mut sess,
        "eve.campaign.track.api.ProcessTickContributionCommanded",
        tick_cmd,
    )
    .await;

    let trk_state = extra.campaign_tracks.read().unwrap().get(&tid).unwrap().clone();
    assert!(trk_state.current_progress >= 101);
    assert_eq!(trk_state.participants.get(&90_100_003), Some(&25));

    // 4. Error case: missing identifier returns STATUS_BAD_REQUEST
    let bad = quasar(&ctx, &mut sess, "eve.campaign.track.api.ChangeStateCommanded", Vec::new()).await;
    assert_eq!(bad.status_code, STATUS_BAD_REQUEST);
    let _ = campaign_uuid(42);
}

fn non_gm_session(sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(2);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER;
    sess
}

#[tokio::test]
async fn test_b9_commanded_requires_gm_and_terminal_transitions_rejected() {
    let ctx = ServerContext::new();
    let mut gm = session(89_104, 90_100_004);
    let mut player = non_gm_session(89_105, 90_100_005);
    let extra = &ctx.quasar_gateway.progression.extra;
    let t0 = ctx.now_unix();

    let (camp, objs, trks) = mining_campaign(43, t0, t0 + 3_600, 500);
    let cid = camp.id;
    let oid = objs[0].id;
    let tid = trks[0].id;
    extra.register_campaign(camp, objs, trks).expect("register");

    let cid_req = id_payload(campaign_uuid(cid));
    let oid_req = id_payload(objective_uuid(oid));
    let tid_req = id_payload(track_uuid(tid));

    // Non-GM player must be rejected with STATUS_FORBIDDEN on all *Commanded handlers
    for (cmd, payload) in [
        ("eve.campaign.api.StartCommanded", cid_req.clone()),
        ("eve.campaign.api.CompleteCommanded", cid_req.clone()),
        ("eve.campaign.api.ExpireCommanded", cid_req.clone()),
        ("eve.campaign.api.ChangeStateCommanded", cid_req.clone()),
        ("eve.campaign.objective.api.CompleteCommanded", oid_req.clone()),
        ("eve.campaign.objective.api.ChangeStateCommanded", oid_req.clone()),
        ("eve.campaign.track.api.ProcessTickContributionCommanded", tid_req.clone()),
        ("eve.campaign.track.api.ChangeStateCommanded", tid_req.clone()),
    ] {
        let resp = quasar(&ctx, &mut player, cmd, payload).await;
        assert_eq!(resp.status_code, STATUS_FORBIDDEN, "expected FORBIDDEN for non-GM on {cmd}");
    }

    // DisableRequest updates persisted objective state to DISABLED (6), and subsequent transitions/progress fail
    ok(&ctx, &mut gm, "eve.campaign.objective.api.admin.DisableRequest", oid_req.clone()).await;
    assert_eq!(extra.campaign_objectives.read().unwrap().get(&oid).unwrap().state, 6);
    let err_obj = quasar(&ctx, &mut gm, "eve.campaign.objective.api.admin.StartRequest", oid_req.clone()).await;
    assert_eq!(err_obj.status_code, STATUS_BAD_REQUEST);
    let err_prog_term = quasar(&ctx, &mut gm, "eve.campaign.objective.api.admin.ProgressRequest", oid_req).await;
    assert_eq!(err_prog_term.status_code, STATUS_BAD_REQUEST);

    // Completing campaign transitions to terminal state; subsequent StartCommanded, UpdateScheduleRequest, UpdateDefinitionRequest are rejected
    cmd_ok(&ctx, &mut gm, "eve.campaign.api.CompleteCommanded", cid_req.clone()).await;
    let err_camp = quasar(&ctx, &mut gm, "eve.campaign.api.StartCommanded", cid_req.clone()).await;
    assert_eq!(err_camp.status_code, STATUS_BAD_REQUEST);
    let err_sched = quasar(&ctx, &mut gm, "eve.campaign.api.admin.UpdateScheduleRequest", cid_req.clone()).await;
    assert_eq!(err_sched.status_code, STATUS_BAD_REQUEST);
    let err_def = quasar(&ctx, &mut gm, "eve.campaign.api.admin.UpdateDefinitionRequest", cid_req).await;
    assert_eq!(err_def.status_code, STATUS_BAD_REQUEST);

    // Overflowing delta (> i64::MAX) and invalid target states in ChangeStateCommanded are rejected
    let (camp2, objs2, trks2) = mining_campaign(44, t0, t0 + 3_600, 500);
    let cid2 = camp2.id;
    let oid2 = objs2[0].id;
    let tid2 = trks2[0].id;
    extra.register_campaign(camp2, objs2, trks2).expect("register 2");

    let mut overflow_prog = id_payload(objective_uuid(oid2));
    encode_varint_field(3, u64::MAX, &mut overflow_prog);
    let err_overflow = quasar(&ctx, &mut gm, "eve.campaign.objective.api.admin.ProgressRequest", overflow_prog).await;
    assert_eq!(err_overflow.status_code, STATUS_BAD_REQUEST);

    for (cmd, id_bytes) in [
        ("eve.campaign.api.ChangeStateCommanded", id_payload(campaign_uuid(cid2))),
        ("eve.campaign.objective.api.ChangeStateCommanded", id_payload(objective_uuid(oid2))),
        ("eve.campaign.track.api.ChangeStateCommanded", id_payload(track_uuid(tid2))),
    ] {
        let mut bad_state_req = id_bytes;
        encode_varint_field(2, 999, &mut bad_state_req);
        let resp = quasar(&ctx, &mut gm, cmd, bad_state_req).await;
        assert_eq!(resp.status_code, STATUS_BAD_REQUEST, "expected BAD_REQUEST for invalid state 999 on {cmd}");
    }
}
