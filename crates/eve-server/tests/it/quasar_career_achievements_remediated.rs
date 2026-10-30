//! Integration tests validating remediation of Quasar Career, Campaign Definitions,
//! and Achievements authentic data-driven semantics.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

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

fn make_char(char_id: i64, corp_id: i64) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        character_id: char_id,
        user_id: 1,
        name: format!("Pilot {char_id}"),
        corporation_id: corp_id,
        alliance_id: None,
        war_faction_id: None,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        station_id: None,
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 80_000_001,
        balance: eve_db::IskCents::from_isk(100_000),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60003760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 1,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 1_000_000.0,
        activity_status: None,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        version: 1,
        system_epoch: 1,
    }
}

fn build_sess(ctx: &ServerContext, sid: u64, cid: u32, corp_id: u32) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.entry(cid as i64).or_insert_with(|| make_char(cid as i64, corp_id as i64));
    });
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-remediated-01".to_vec(),
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
    for _ in 0..1024 {
        match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn test_authentic_career_goals_and_milestones_remediated() {
    // DATA GAP: Individual AIR Career Program goal UUIDs, targets, and rewards are server-authored and not shipped in V24 client FSD.
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(&ctx, 41_001, 90_000_010, 98_000_001);

    // 1. GetAllRequest returns all 188 authentic career goals
    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new(), b"cg-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    let r1_payload = r1.payload.expect("r1 payload");
    assert_eq!(r1_payload.normalized_type_name(), "eve_public.career.goal.api.GetAllResponse");

    let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllResponse");
    let goal_fields: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(
        goal_fields.len(),
        196,
        "Must return 188 AIR Career Program goals + 8 career/overall milestones"
    );

    // Inspect first goal: verify field 1 is Identifier { uuid: bytes }
    let g1_bytes = goal_fields[0].value.as_bytes().expect("g1 bytes");
    let g1_wm = WireMessage::decode(g1_bytes).expect("decode g1");
    let id_field = g1_wm.fields.iter().find(|f| f.field_number == 1).expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().expect("id bytes")).expect("decode id");
    let uuid_bytes = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("uuid");
    assert_eq!(uuid_bytes.len(), 16, "Career goal identifier uuid must be 16 bytes");

    // 2. GetDefinitionsRequest returns all 188 career goal definitions + 8 milestone definitions
    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), b"cg-02").await;
    assert_eq!(r2.status_code, STATUS_OK);
    let r2_payload = r2.payload.expect("r2 payload");
    assert_eq!(r2_payload.normalized_type_name(), "eve_public.career.goal.api.GetDefinitionsResponse");
    let wm2 = WireMessage::decode(&r2_payload.value).expect("decode GetDefinitionsResponse");
    let def_fields: Vec<_> = wm2.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(
        def_fields.len(),
        196,
        "Must return 188 career goal definitions + 8 milestone definitions"
    );
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");
    let defs_dyn = reg
        .decode_dynamic("eve_public.career.goal.api.GetDefinitionsResponse", &r2_payload.value)
        .expect("decode GetDefinitionsResponse via QuasarDescriptorRegistry");
    let first_goal = match defs_dyn.get(1) {
        Some(eve_proto::quasar::DynamicValue::Repeated(items)) => match &items[0] {
            eve_proto::quasar::DynamicValue::Message(m) => m,
            other => panic!("expected Goal message, got {other:?}"),
        },
        other => panic!("expected Repeated Goal messages, got {other:?}"),
    };
    let first_attrs = match first_goal.get(2) {
        Some(eve_proto::quasar::DynamicValue::Message(m)) => m,
        other => panic!("expected Attributes message, got {other:?}"),
    };
    assert!(matches!(first_attrs.get(1), Some(eve_proto::quasar::DynamicValue::Message(m)) if matches!(m.get(1), Some(eve_proto::quasar::DynamicValue::Uint32(v)) if *v >= 582_516)));
    assert!(matches!(first_attrs.get(2), Some(eve_proto::quasar::DynamicValue::Message(m)) if matches!(m.get(1), Some(eve_proto::quasar::DynamicValue::Uint32(v)) if *v >= 659_200)));
    assert!(matches!(first_attrs.get(4), Some(eve_proto::quasar::DynamicValue::Uint32(v)) if *v >= 1) || matches!(first_attrs.get(4), Some(eve_proto::quasar::DynamicValue::Uint64(v)) if *v >= 1));
    assert!(matches!(first_attrs.get(6), Some(eve_proto::quasar::DynamicValue::Repeated(v)) if v.len() == 4));
    assert!(matches!(first_attrs.get(7), Some(eve_proto::quasar::DynamicValue::Uint32(v)) if *v >= 1) || matches!(first_attrs.get(7), Some(eve_proto::quasar::DynamicValue::Int32(v)) if *v >= 1));
    assert!(matches!(first_attrs.get(12), Some(eve_proto::quasar::DynamicValue::Repeated(v)) if !v.is_empty()));

    // 3. Admin ProgressRequest and CompleteRequest with live notice publication
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

    let enforcer_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut prog_req = Vec::new();
    let mut id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut prog_req);
    encode_varint_field(2, 5, &mut prog_req);

    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", prog_req, b"cg-03").await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let notices = drain_notices(&mut stream_rx);
    assert!(!notices.is_empty(), "ProgressedNotice must be emitted");
    let prog_not = notices.iter().find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("ProgressedNotice"))).expect("ProgressedNotice");
    let not_wm = WireMessage::decode(&prog_not.payload.as_ref().unwrap().value).expect("decode notice");
    assert!(not_wm.fields.iter().any(|f| f.field_number == 1), "Notice must contain goal Identifier");

    // 4. Admin CompleteRequest marks goal completed and emits CompletedNotice
    let mut comp_req = Vec::new();
    let mut comp_id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut comp_id_inner);
    encode_bytes_field(1, &comp_id_inner, &mut comp_req);
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.CompleteRequest", comp_req, b"cg-04").await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    let comp_notices = drain_notices(&mut stream_rx);
    assert!(comp_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("CompletedNotice"))), "CompletedNotice must be emitted");

    // 5. ClaimRewardRequest credits character balance and appends WalletJournalEntry
    let (bal_before, sp_before) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_010).expect("character");
        (ch.balance, ch.free_skill_points)
    });

    let mut claim_req = Vec::new();
    let mut claim_id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut claim_id_inner);
    encode_bytes_field(1, &claim_id_inner, &mut claim_req);
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", claim_req, b"cg-05").await;
    assert_eq!(claim_rsp.status_code, STATUS_OK);

    let (bal_after, sp_after, has_journal, valid_journal_meta) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_010).expect("character");
        let entry = snap.wallet_journal.iter().find(|j| {
            j.character_id == 90_000_010 && j.entry_type == "CAREER_PROGRAM_REWARD"
        });
        let meta = entry.and_then(|e| snap.wallet_journal_meta.get(&e.entry_id));
        let valid_meta = meta.is_some_and(|m| m.ref_type_id == 170 && m.first_party_id == 1000125);
        (ch.balance, ch.free_skill_points, entry.is_some(), valid_meta)
    });
    assert!(bal_after > bal_before, "Character ISK balance must increase upon claiming career goal reward");
    assert!(has_journal, "WalletJournalEntry with CAREER_PROGRAM_REWARD must be recorded");
    assert!(valid_journal_meta, "WalletJournalMeta must record ref_type_id 170 and first_party_id 1000125 (CONCORD)");
    assert_eq!(sp_after, sp_before, "Individual career goal does not award SP directly (SP is awarded via milestones)");

    // 6. Complete career goals to reach 1,000 CP for Enforcer (>= 750 threshold) and claim milestone SP
    let enforcer_goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(90_000_010);
    for g in enforcer_goals {
        if g.def.career_id == 3 && !g.completed {
            let _ = ctx.quasar_gateway.progression.career_goals.mark_completed(90_000_010, &g.uuid, ctx.now_unix());
        }
    }

    let milestones = ctx.quasar_gateway.progression.career_goals.get_character_milestones(90_000_010);
    let m3 = milestones.iter().find(|(m, _, _)| m.milestone_id == 3).expect("Enforcer graduation milestone");
    assert!(m3.1, "Enforcer graduation milestone must be reached (1000 CP >= 750)");
    assert!(!m3.2, "Enforcer graduation milestone not yet claimed");

    // Claim milestone 3 (Enforcer graduation) via ClaimRewardRequest with milestone UUID
    let mut ms_claim_req = Vec::new();
    let mut ms_id_inner = Vec::new();
    encode_bytes_field(1, &eve_server::services::quasar_career_goals::career_milestone_uuid(3), &mut ms_id_inner);
    encode_bytes_field(1, &ms_id_inner, &mut ms_claim_req);
    let ms_claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", ms_claim_req, b"cg-06").await;
    assert_eq!(ms_claim_rsp.status_code, STATUS_OK);

    let sp_after_ms = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&90_000_010).unwrap().free_skill_points
    });
    assert_eq!(sp_after_ms - sp_before, 100_000.0, "Milestone claim must award 100,000 SP");

    let milestones_post = ctx.quasar_gateway.progression.career_goals.get_character_milestones(90_000_010);
    let m3_post = milestones_post.iter().find(|(m, _, _)| m.milestone_id == 3).unwrap();
    assert!(m3_post.2, "Milestone 3 must now be marked claimed");

    let has_ms_blob = ctx.db.with_snapshot(|snap| {
        snap.quasar_state_blobs.contains_key("quasar_career_milestones")
    });
    assert!(has_ms_blob, "Milestone claims must persist to SQLite quasar_career_milestones blob");
}

#[tokio::test]
async fn test_authentic_campaign_definitions_remediated() {
    // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(&ctx, 41_002, 90_000_011, 98_000_001);

    // 1. Contribution method definitions: GetAllRequest
    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new(), b"cm-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    let r1_payload = r1.payload.expect("r1 payload");
    assert_eq!(r1_payload.normalized_type_name(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse");
    assert!(!r1_payload.value.is_empty(), "Contribution methods payload must not be empty");

    let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllResponse");
    let methods: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(methods.len(), 22, "Must return 22 authentic contribution methods");

    // Inspect first contribution method definition
    let m1_bytes = methods[0].value.as_bytes().expect("m1 bytes");
    let m1_wm = WireMessage::decode(m1_bytes).expect("decode m1");
    let id_f = m1_wm.fields.iter().find(|f| f.field_number == 1).expect("definition Identifier");
    let id_wm = WireMessage::decode(id_f.value.as_bytes().expect("id bytes")).expect("decode Identifier");
    let name_str = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("method name");
    assert!(!name_str.is_empty(), "Method name must be non-empty");

    // 2. Trigger eventmatch definitions: GetAllRequest
    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new(), b"em-01").await;
    assert_eq!(r2.status_code, STATUS_OK);
    let r2_payload = r2.payload.expect("r2 payload");
    assert_eq!(r2_payload.normalized_type_name(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse");
    assert!(!r2_payload.value.is_empty(), "Trigger eventmatches payload must not be empty");

    let wm2 = WireMessage::decode(&r2_payload.value).expect("decode GetAllResponse");
    let triggers: Vec<_> = wm2.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(triggers.len(), 10, "Must return 10 authentic trigger eventmatches");
}

#[tokio::test]
async fn test_authentic_achievements_and_cdn_metadata_remediated() {
    // DATA GAP: Achievement showcase scores, category bucketing, and milestone reward amounts are server-authored CDN Checkpoint data in V24 and not present in res:/staticdata/achievements.static.
    // parity: feat:quasar.achievement
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.achievement",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = build_sess(&ctx, 41_003, 90_000_012, 98_000_001);
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    // 0. Verify parsed authentic SDE achievement attributes (60 enabled; 8 disabled excluded)
    let ach_defs = &ctx.quasar_gateway.progression.achievements.definitions;
    assert_eq!(ach_defs.len(), 60, "Must load 60 enabled SDE achievements (8 disabled excluded)");
    for disabled_id in [3u32, 4, 14, 37, 51, 54, 56, 65] {
        assert!(!ach_defs.iter().any(|d| d.id == disabled_id), "Disabled SDE achievement {disabled_id} must be excluded");
    }
    let ach2 = ach_defs.iter().find(|d| d.id == 2).expect("SDE achievement 2");
    assert_eq!(ach2.name, "Mine Ore", "SDE achievement 2 localized name must be 'Mine Ore'");
    assert_eq!(ach2.target, 3, "SDE achievement 2 authentic target condition must be 3");
    assert_eq!(ach2.category_id, 0, "DATA GAP: category_id must not be fabricated");
    assert_eq!(ach2.score, 0, "DATA GAP: score must not be fabricated");
    assert!(ach2.milestone_targets.is_empty(), "DATA GAP: intermediate milestones must not be fabricated");
    assert_eq!(ach2.reward_isk, 0, "DATA GAP: reward_isk must not be fabricated");
    assert_eq!(ach2.reward_sp, 0, "DATA GAP: reward_sp must not be fabricated");
    assert!(!ach2.description.is_empty(), "SDE achievement 2 description must be resolved from localization pickle");

    let ach60 = ach_defs.iter().find(|d| d.id == 60).expect("SDE achievement 60");
    assert_eq!(ach60.name, "Zooming the camera", "SDE achievement 60 localized name must be 'Zooming the camera'");
    assert_eq!(ach60.conditions.len(), 2, "SDE achievement 60 has 2 simpleConditions (zoomIn 138 + zoomOut 124)");
    assert_eq!(ach60.target, 2, "SDE achievement 60 combined condition target must be 2");

    let ach102 = ach_defs.iter().find(|d| d.id == 102).expect("SDE achievement 102");
    assert_eq!(ach102.name, "Open the Agency", "SDE achievement 102 (bitfield 0x0b without notificationTextID) localized name must be 'Open the Agency'");
    assert_eq!(ach102.target, 1, "SDE achievement 102 target must be 1");

    let ach12 = ach_defs.iter().find(|d| d.id == 12).expect("SDE achievement 12");
    assert_eq!(ach12.name, "Destroy your target", "SDE achievement 12 localized name must be 'Destroy your target'");
    assert_eq!(ach12.target, 1, "SDE achievement 12 target must be 1");

    // Verify multi-condition statistic event evaluation on ach 60 (138 = zoomIn, 124 = zoomOut)
    ctx.quasar_gateway.progression.achievements.record_statistic_event(90_000_012, 138, 3, ctx.now_unix());
    let ach60_mid = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_012)
        .into_iter().find(|a| a.def.id == 60).unwrap();
    assert_eq!(ach60_mid.progress, 1, "Zooming in multiple times only satisfies condition 0 (1/2)");
    assert!(!ach60_mid.completed, "Achievement 60 must not complete until condition 1 (zoomOut 124) also fires");

    ctx.quasar_gateway.progression.achievements.record_statistic_event(90_000_012, 124, 1, ctx.now_unix());
    let ach60_done = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_012)
        .into_iter().find(|a| a.def.id == 60).unwrap();
    assert_eq!(ach60_done.progress, 2, "Zooming out satisfies condition 1 (2/2)");
    assert!(ach60_done.completed, "Achievement 60 completes once both conditions are met");

    // 1. CDN GetLatestRequest returns valid checkpoint metadata with patch 60 (enabled SDE count) and CRC32 IEEE
    let cdn_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new(), b"cdn-01").await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    let cdn_payload = cdn_rsp.payload.expect("cdn payload");
    assert_eq!(cdn_payload.normalized_type_name(), "eve_public.achievement.definition.api.cdn.GetLatestResponse");

    let wm_cdn = WireMessage::decode(&cdn_payload.value).expect("decode GetLatestResponse");
    let cp_field = wm_cdn.fields.iter().find(|f| f.field_number == 1).expect("checkpoint field");
    let cp_wm = WireMessage::decode(cp_field.value.as_bytes().expect("cp bytes")).expect("decode Checkpoint");

    let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("url");
    assert!(url_str.contains("v24.1.60") && url_str.contains("achievements"), "URL must reference 60 enabled achievement definitions: {url_str}");

    let spec_field = cp_wm.fields.iter().find(|f| f.field_number == 2).expect("spec field");
    let spec_wm = WireMessage::decode(spec_field.value.as_bytes().expect("spec bytes")).expect("decode spec");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(24), "Major 24");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1), "Minor 1");
    assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(60), "Patch 60 (60 enabled SDE achievements)");

    let crc_field = cp_wm.fields.iter().find(|f| f.field_number == 3).expect("crc field");
    let crc_wm = WireMessage::decode(crc_field.value.as_bytes().expect("crc bytes")).expect("decode crc");
    let crc_val = crc_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).expect("crc val");
    assert!(crc_val > 0, "CRC32 checksum must be non-zero");

    // 2. eve_public.achievement.api.GetRequest -> GetResponse (V24 oneof completion) + disabled achievement excluded
    let make_ident_req = |id: u32| -> Vec<u8> {
        let mut req_body = Vec::new();
        let mut id_bytes = Vec::new();
        let mut uuid = [0u8; 16];
        uuid[12..16].copy_from_slice(&id.to_be_bytes());
        encode_bytes_field(1, &uuid, &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut req_body);
        req_body
    };

    let disabled_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ident_req(3), b"ach-disabled").await;
    assert_eq!(disabled_rsp.status_code, STATUS_NOT_FOUND, "Disabled SDE achievement 3 must return NOT_FOUND");

    let req_ach2 = make_ident_req(2);
    let get_rsp1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", req_ach2.clone(), b"ach-01").await;
    assert_eq!(get_rsp1.status_code, STATUS_OK);
    let get_payload1 = get_rsp1.payload.expect("get payload");
    let wm_get1 = WireMessage::decode(&get_payload1.value).expect("decode GetResponse");
    assert!(wm_get1.fields.iter().all(|f| f.field_number != 1), "completed_date must be absent when incomplete");
    assert_eq!(wm_get1.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1), "not_completed must be 1 initially");
    assert_eq!(wm_get1.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(0), "progress must be 0 initially");
    let dyn_get1 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &get_payload1.value).expect("decode GetResponse dynamic");
    assert!(dyn_get1.get(1).is_none());
    assert!(matches!(dyn_get1.get(2), Some(eve_proto::quasar::DynamicValue::Bool(true))));

    // Complete achievement #2 and #12 via admin CompleteRequest
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", req_ach2.clone(), b"ach-02").await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);
    let comp12_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", make_ident_req(12), b"ach-02b").await;
    assert_eq!(comp12_rsp.status_code, STATUS_OK);

    let get_rsp2 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", req_ach2, b"ach-03").await;
    assert_eq!(get_rsp2.status_code, STATUS_OK);
    let get_payload2 = get_rsp2.payload.expect("get_payload2");
    let wm_get2 = WireMessage::decode(&get_payload2.value).expect("decode GetResponse");
    assert!(wm_get2.fields.iter().any(|f| f.field_number == 1), "completed_date timestamp must be present");
    assert!(wm_get2.fields.iter().all(|f| f.field_number != 2), "not_completed must NOT be emitted when completed_date is set (oneof completion)");
    assert_eq!(wm_get2.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(ach2.target), "progress must equal target");
    assert!(wm_get2.fields.iter().all(|f| f.field_number != 5), "DATA GAP: no fabricated intermediate milestones emitted");
    let dyn_get2 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &get_payload2.value).expect("decode completed GetResponse dynamic");
    assert!(dyn_get2.get(1).is_some(), "completed_date must decode cleanly via descriptor");
    assert!(dyn_get2.get(2).is_none(), "not_completed must be None in oneof completion");

    // 3. eve_public.achievement.category.api.GetRequest -> GetResponse (0 score without CDN checkpoint)
    let cat_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", make_ident_req(1), b"cat-01").await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    let cat_payload = cat_rsp.payload.expect("cat payload");
    let dyn_cat = reg.decode_dynamic("eve_public.achievement.category.api.GetResponse", &cat_payload.value).expect("decode category GetResponse");
    assert!(matches!(dyn_cat.get(1), Some(eve_proto::quasar::DynamicValue::Uint32(0))));

    // 4. eve_public.achievement.reward.api.GetUnclaimedRequest & ClaimRequest fail-closed DATA GAP (no ISK/SP minted)
    let (bal_before, sp_before) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_012).unwrap();
        (ch.balance, ch.free_skill_points)
    });
    let uncl_rsp1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new(), b"ach-04").await;
    assert_eq!(uncl_rsp1.status_code, STATUS_OK);
    let wm_uncl1 = WireMessage::decode(&uncl_rsp1.payload.unwrap().value).expect("decode GetUnclaimedResponse");
    assert!(wm_uncl1.fields.is_empty(), "DATA GAP: no fabricated milestone rewards emitted");

    let mut claim_all_req = Vec::new();
    encode_varint_field(3, 1, &mut claim_all_req);
    let claim_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", claim_all_req, b"ach-05c").await;
    assert_eq!(claim_all_rsp.status_code, STATUS_PRECONDITION_FAILED, "ClaimRequest must fail closed with PRECONDITION_FAILED until CDN checkpoint data exists");

    let (bal_after, sp_after, has_journal) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_012).unwrap();
        let j = snap.wallet_journal.iter().any(|j| j.character_id == 90_000_012 && j.entry_type == "achievement_reward");
        (ch.balance, ch.free_skill_points, j)
    });
    assert_eq!(bal_after, bal_before, "ClaimRequest must not mint ISK");
    assert_eq!(sp_after, sp_before, "ClaimRequest must not grant free SP");
    assert!(!has_journal, "ClaimRequest must not write achievement_reward wallet journal entries");

    // 5. eve_public.achievement.showcase.api.GetCharacterScoreRequest -> 0 without CDN checkpoint + negative input rejection
    let score_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new(), b"ach-07").await;
    assert_eq!(score_rsp.status_code, STATUS_OK);
    let dyn_score = reg.decode_dynamic("eve_public.achievement.showcase.api.GetCharacterScoreResponse", &score_rsp.payload.unwrap().value).expect("decode GetCharacterScoreResponse");
    assert!(matches!(dyn_score.get(1), Some(eve_proto::quasar::DynamicValue::Uint32(0))));

    // Invalid / zero DynamicValue inputs must be rejected with STATUS_BAD_REQUEST
    let mut zero_char_ident = Vec::new();
    encode_varint_field(1, 0, &mut zero_char_ident);
    let mut zero_score_req = Vec::new();
    encode_bytes_field(1, &zero_char_ident, &mut zero_score_req);
    let zero_score_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", zero_score_req, b"ach-neg-01").await;
    assert_eq!(zero_score_rsp.status_code, STATUS_BAD_REQUEST, "Zero character_id in GetCharacterScoreRequest must return BAD_REQUEST");

    let mut zero_delta_req = Vec::new();
    encode_varint_field(2, 0, &mut zero_delta_req);
    let zero_delta_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", zero_delta_req, b"ach-neg-02").await;
    assert_eq!(zero_delta_rsp.status_code, STATUS_BAD_REQUEST, "Zero progress_delta in AddProgressRequest must return BAD_REQUEST");
}
