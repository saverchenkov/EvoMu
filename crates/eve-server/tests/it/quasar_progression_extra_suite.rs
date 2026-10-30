use eve_machonet::SessionState;
use crate::quasar_progression_complete::quasar_rpc;
use crate::quasar_space_cosmetics_complete::call_req;
use eve_proto::quasar::{
    encode_bytes_field, EvePublicNotice, GoogleAny, QuasarMessage, WireField, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

fn make_sequential_id_request(id: u32) -> Vec<u8> {
    let (mut out, mut id_bytes) = (Vec::new(), Vec::new());
    eve_proto::quasar::encode_varint_field(1, id as u64, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

/// `StartTaleRequest { spawn_template = 1, solar_system = 2 }`.
fn make_tale_start(template: u32, system: u32) -> Vec<u8> {
    let (mut out, mut sys) = (make_sequential_id_request(template), Vec::new());
    eve_proto::quasar::encode_varint_field(1, system as u64, &mut sys);
    encode_bytes_field(2, &sys, &mut out);
    out
}

fn make_id_request(id: u32) -> Vec<u8> {
    let (mut out, mut id_bytes, mut uuid_bytes) = (Vec::new(), Vec::new(), [0u8; 16]);
    uuid_bytes[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

#[allow(dead_code)]
fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let (mut out, mut id_bytes) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut notifs = Vec::new();
    for _ in 0..1024 {
        match rx.try_recv() {
            Ok(n) => notifs.push(n),
            Err(_) => break,
        }
    }
    notifs
}

#[tokio::test]
async fn quasar_achievements_categories_scores_and_claims() {
    // DATA GAP: Achievement category bucketing, showcase scores, and reward amounts are server-only and not present in res:/staticdata/achievements.static.
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = crate::quasar_progression_complete::build_sess(10_003, 90_000_001, 98_000_001);
    sess.role |= eve_machonet::service_roles::ROLE_GML;
    let empty_kw: HashMap<String, PyRep> = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    let rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.GetRequest",
        make_id_request(2),
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert_eq!(
        rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.api.GetResponse"
    );

    let cat_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.category.api.GetRequest",
        make_id_request(1),
    )
    .await;
    assert_eq!(cat_rsp.status_code, STATUS_OK, "cat_rsp failed: {}", cat_rsp.status_message);
    let cat_payload = cat_rsp.payload.expect("cat payload present");
    let cat_wm = WireMessage::decode(&cat_payload.value).expect("decode cat");
    let cats: Vec<&WireField> = cat_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(cats.len(), 1, "Category score returned");

    let cdn_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);

    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.AddProgressRequest",
        make_id_request(2),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.CompleteRequest",
        make_id_request(2),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Achievement notices should be emitted");

    let score_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(score_rsp.status_code, STATUS_OK);
    let score_payload = score_rsp.payload.expect("score payload");
    let score_wm = WireMessage::decode(&score_payload.value).expect("decode score");
    let total_score = score_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_varint());
    assert_eq!(total_score.unwrap_or(0), 0, "DATA GAP: score is 0 without CDN checkpoints");

    let unclaimed_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new()).await;
    assert_eq!(unclaimed_rsp.status_code, STATUS_OK);

    let mut claim_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(3, 1, &mut claim_bytes);
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", claim_bytes).await;
    assert_eq!(claim_rsp.status_code, eve_server::quasar_gateway::STATUS_PRECONDITION_FAILED);

    let reset_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.ResetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(reset_rsp.status_code, STATUS_OK);

    let reset_all_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.ResetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(reset_all_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn quasar_community_progression_tales_and_goals() {
    // parity: feat:quasar.tale, feat:quasar.communitytrack, feat:quasar.goal, feat:quasar.opportunity, quasar:eveProto/generated/eve_public/communitytrack/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/communitytrack/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/goal/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.tale",
        "feat:quasar.communitytrack",
        "feat:quasar.goal",
        "feat:quasar.opportunity",
        "quasar:eveProto/generated/eve_public/communitytrack/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/communitytrack/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/goal/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = crate::quasar_progression_complete::build_sess(10_005, 90_000_001, 98_000_001);
    sess.role |= eve_machonet::service_roles::ROLE_GML;
    let empty_kw: HashMap<String, PyRep> = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx
        .quasar_gateway
        .take_stream_receiver(sess.session_id)
        .expect("take_stream_receiver");

    // Unactivated SDE template 1 is not pre-populated as an active instance
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", make_sequential_id_request(1)).await.status_code, 404);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.communitytrack.api.StartRequest", make_sequential_id_request(1)).await.status_code, STATUS_OK);
    let tid = ctx.quasar_gateway.progression.extra.community_tracks.read().unwrap().values().find(|t| t.template_id == 1).map(|t| t.track_id).expect("activated track instance");
    assert_ne!(tid, 1, "active track instance ID must differ from template_id");

    let track_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", make_sequential_id_request(tid)).await;
    assert_eq!(track_rsp.status_code, STATUS_OK, "track_rsp failed: {}", track_rsp.status_message);
    assert_eq!(track_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.communitytrack.api.GetResponse");
    let track_wm = WireMessage::decode(&track_rsp.payload.unwrap().value).expect("decode track GetResponse");
    let track_attr_wm = WireMessage::decode(track_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let track_def_wm = WireMessage::decode(track_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(track_def_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1));
    assert_eq!(track_attr_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()).unwrap_or(0), 0);

    let def_rsp = quasar_rpc(&ctx, &mut sess, "eve.communitytrack.api.GetRequest", make_sequential_id_request(tid)).await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(def_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.communitytrack.api.GetResponse");

    let mut add_prog_req = make_sequential_id_request(tid);
    eve_proto::quasar::encode_varint_field(2, 25, &mut add_prog_req);
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve.communitytrack.api.AddProgressRequest", add_prog_req).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let track_after_wm = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", make_sequential_id_request(tid)).await.payload.unwrap().value).unwrap();
    let track_after_attr = WireMessage::decode(track_after_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(track_after_attr.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(25));

    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Track notice should be emitted");
    assert_eq!(notifs[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.communitytrack.api.ProgressedNotice");

    // Starting Vanguard Nemesis tale 466 emits StartedNotice only (does not advance track); stopping advances track once (+1 -> 26)
    let start_rsp = quasar_rpc(&ctx, &mut sess, "eve.tale.api.StartTaleRequest", make_tale_start(466, 30_000_142)).await;
    assert_eq!(start_rsp.status_code, STATUS_OK);
    let tale_start_notifs = drain_notices(&mut stream_rx);
    assert_eq!(tale_start_notifs.len(), 1, "StartedNotice should be emitted");
    assert_eq!(tale_start_notifs[0].payload.as_ref().unwrap().normalized_type_name(), "eve_public.tale.api.StartedNotice");
    assert_eq!(ctx.quasar_gateway.progression.extra.get_community_track(tid).unwrap().current_progress, 25);

    let count_rsp = quasar_rpc(&ctx, &mut sess, "eve.tale.api.GetActiveTalesCountRequest", Vec::new()).await;
    assert_eq!(count_rsp.status_code, STATUS_OK);
    let count_wm = WireMessage::decode(&count_rsp.payload.unwrap().value).expect("decode count response");
    assert_eq!(count_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1));

    let stop_rsp = quasar_rpc(&ctx, &mut sess, "eve.tale.api.StopTalesRequest", make_sequential_id_request(466)).await;
    assert_eq!(stop_rsp.status_code, STATUS_OK);
    let tale_stop_notifs = drain_notices(&mut stream_rx);
    assert_eq!(tale_stop_notifs.len(), 2, "ProgressedNotice + StoppedNotice should be emitted on StopTalesRequest");
    assert!(tale_stop_notifs.iter().any(|n| n.payload.as_ref().unwrap().normalized_type_name() == "eve_public.tale.api.StoppedNotice"));
    assert!(tale_stop_notifs.iter().any(|n| n.payload.as_ref().unwrap().normalized_type_name() == "eve_public.communitytrack.api.ProgressedNotice"));
    assert_eq!(ctx.quasar_gateway.progression.extra.get_community_track(tid).unwrap().current_progress, 26);
    let count_after_stop = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.GetActiveTalesCountRequest",
        Vec::new(),
    )
    .await;
    let count_after_wm = WireMessage::decode(&count_after_stop.payload.unwrap().value).expect("decode count response");
    assert_eq!(
        count_after_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).unwrap_or(0),
        0
    );

    // Negative tests for unauthorized callers (P1-1)
    let mut unauth_sess = crate::quasar_progression_complete::build_sess(10_099, 90_000_002, 98_000_001);
    unauth_sess.role = 0;
    unauth_sess.corp_role = 0;
    let unauth_start = quasar_rpc(&ctx, &mut unauth_sess, "eve.tale.api.StartTaleRequest", make_sequential_id_request(1)).await;
    assert_eq!(unauth_start.status_code, 403, "StartTaleRequest must reject unprivileged caller");

    let goal_create_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.goal.api.CreateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(goal_create_rsp.status_code, STATUS_OK);

    let goal_get = quasar_rpc(&ctx, &mut sess, "eve_public.goal.api.GetRequest", goal_create_rsp.payload.as_ref().unwrap().value.clone()).await;
    assert_eq!(goal_get.status_code, STATUS_OK);
    let goal_get_all = quasar_rpc(&ctx, &mut sess, "eve_public.goal.api.GetAllRequest", Vec::new()).await;
    assert_eq!(goal_get_all.status_code, STATUS_OK);
    let goal_cap = quasar_rpc(&ctx, &mut sess, "eve_public.goal.api.GetCapacityRequest", Vec::new()).await;
    assert_eq!(goal_cap.status_code, STATUS_OK);

    let unauth_close = quasar_rpc(&ctx, &mut unauth_sess, "eve_public.goal.api.CloseRequest", goal_create_rsp.payload.as_ref().unwrap().value.clone()).await;
    assert_eq!(unauth_close.status_code, 403, "CloseRequest must reject non-creator");
    let unauth_del = quasar_rpc(&ctx, &mut unauth_sess, "eve_public.goal.api.DeleteRequest", goal_create_rsp.payload.as_ref().unwrap().value.clone()).await;
    assert_eq!(unauth_del.status_code, 403, "DeleteRequest must reject non-creator");

    let titles_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(titles_rsp.status_code, STATUS_OK);

    let chall_rsp = quasar_rpc(&ctx, &mut sess, "eve.seasonalevent.challenge.GetChallengesForCharacterRequest", make_sequential_id_request(90_000_001)).await;
    assert_eq!(chall_rsp.status_code, STATUS_OK);

    // Job Board Opportunities: Viewed, Tracked, Untracked, Completed, Discovered via publicGatewaySvc.PublishEventPayload
    let make_opp_ev = |cid: u32, story_name: &str| {
        let (mut ch, mut story, mut opp, mut ev) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        eve_proto::quasar::encode_varint_field(1, cid as u64, &mut ch);
        eve_proto::quasar::encode_string_field(1, story_name, &mut story);
        encode_bytes_field(5, &story, &mut opp);
        encode_bytes_field(1, &ch, &mut ev);
        encode_bytes_field(2, &opp, &mut ev);
        ev
    };
    async fn pub_opp(ctx: &ServerContext, s: &mut SessionState, type_name: &str, value: Vec<u8>) {
        let any = GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value };
        ctx.dispatch_rpc(s, "publicGatewaySvc", None, "PublishEventPayload", &[PyRep::Bytes(any.encode_proto())], &HashMap::new(), None).await.expect("PublishEventPayload");
    }
    // Foreign character session attempting to track for 90_000_001 is ignored
    pub_opp(&ctx, &mut unauth_sess, "eve_public.opportunity.api.Tracked", make_opp_ev(90_000_001, "air_npe_storylin1")).await;
    assert!(!ctx.db.with_snapshot(|s| s.quasar_state_blobs.contains_key("quasar_opportunities")));
    // Two distinct opportunity IDs of identical byte length ("air_npe_storylin1" and "air_npe_storylin2", 17 bytes) both persist distinctly
    let opp1 = make_opp_ev(90_000_001, "air_npe_storylin1");
    let opp2 = make_opp_ev(90_000_001, "air_npe_storylin2");
    assert_eq!(opp1.len(), opp2.len(), "Both opportunity event payloads have identical byte length");
    for ev_name in ["eve_public.opportunity.api.Viewed", "eve_public.opportunity.api.Tracked", "eve_public.opportunity.api.Completed"] {
        pub_opp(&ctx, &mut sess, ev_name, opp1.clone()).await;
    }
    pub_opp(&ctx, &mut sess, "eve_public.opportunity.api.Tracked", opp2).await;
    let opp_state: serde_json::Value = ctx.db.with_snapshot(|s| serde_json::from_str(&s.quasar_state_blobs["quasar_opportunities"]).unwrap());
    let tracked = opp_state["90000001"]["tracked"].as_array().unwrap();
    assert_eq!(tracked.len(), 2, "Both same-length opportunity IDs must be tracked distinctly");
    assert!(tracked.iter().any(|x| x.as_str() == Some("v5:1=air_npe_storylin1")));
    assert!(tracked.iter().any(|x| x.as_str() == Some("v5:1=air_npe_storylin2")));
    pub_opp(&ctx, &mut sess, "eve_public.opportunity.api.Untracked", opp1).await;
    let opp_state2: serde_json::Value = ctx.db.with_snapshot(|s| serde_json::from_str(&s.quasar_state_blobs["quasar_opportunities"]).unwrap());
    let tracked2 = opp_state2["90000001"]["tracked"].as_array().unwrap();
    assert_eq!(tracked2.len(), 1, "Untracked removes only opp1 while keeping opp2");
    assert_eq!(tracked2[0].as_str(), Some("v5:1=air_npe_storylin2"));
    let (mut ctag, mut disc_opp, mut disc_ev) = (Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "feature_combat_anomalies", &mut ctag);
    encode_bytes_field(1, &ctag, &mut disc_opp);
    eve_proto::quasar::encode_varint_field(2, 3, &mut disc_opp);
    encode_bytes_field(1, &disc_opp, &mut disc_ev);
    pub_opp(&ctx, &mut sess, "eve_public.opportunity.api.Discovered", disc_ev).await;
    let opp_state3: serde_json::Value = ctx.db.with_snapshot(|s| serde_json::from_str(&s.quasar_state_blobs["quasar_opportunities"]).unwrap());
    assert!(opp_state3["90000001"]["history"].as_array().unwrap().iter().any(|h| h["key"].as_str() == Some("discovered:feature_combat_anomalies")));
    assert!(ctx.quasar_gateway.poll_notices(sess.session_id).iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("achievement.api.ProgressedNotice"))));
    assert!(ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_001).iter().any(|a| a.def.id == 102 && a.progress >= 3));
}

#[tokio::test]
async fn q9_corp_palette_titles_structure_paintwork_and_app_infra() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_006);
    session.user_id = Some(106);
    session.character_id = Some(90_000_106);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    // 1. Corporation color palette
    let pal_rsp = call_req(&ctx, &session, "eve.cosmetic.corporation.palette.api.GetRequest", Vec::new(), true);
    assert_eq!(pal_rsp.status_code, STATUS_OK);

    // 2. Character titles (GetOwned, GetSelected, Select, ClearSelection)
    call_req(&ctx, &session, "eve_public.cosmetic.character.title.api.admin.GrantRequest", make_id_request(101), true);
    let tit_rsp = call_req(&ctx, &session, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new(), true);
    assert_eq!(tit_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        "eve_public.cosmetic.character.title.api.EquipRequest",
        "eve_public.cosmetic.character.title.api.UnequipRequest",
    ] {
        let payload = if req_name.ends_with(".EquipRequest") { make_id_request(101) } else { Vec::new() };
        let rsp = call_req(&ctx, &session, req_name, payload, true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    // 3. Structure paintwork & licenses: covered state-backed in quasar_cosmetics_live_state.rs

    // 4. Quasar configuration
    let cfg_rsp = call_req(
        &ctx,
        &session,
        "eve.quasar.config.api.GetRequest",
        Vec::new(),
        false,
    );
    assert_eq!(cfg_rsp.status_code, STATUS_OK);

    // 5. AIR Career Program Goals
    let goal_rsp = call_req(
        &ctx,
        &session,
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Vec::new(),
        true,
    );
    assert_eq!(goal_rsp.status_code, STATUS_OK);
    assert_eq!(
        pal_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.corporation.palette.api.GetResponse"
    );
    assert_eq!(
        tit_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.cosmetic.character.title.api.GetOwnedResponse"
    );
    assert_eq!(
        cfg_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.quasar.config.api.GetResponse"
    );
    assert_eq!(
        goal_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.app.eveonline.career.goal.GetAllResponse"
    );
    assert!(!WireMessage::decode(&pal_rsp.payload.as_ref().unwrap().value).unwrap().fields.is_empty());
    assert!(!WireMessage::decode(&tit_rsp.payload.as_ref().unwrap().value).unwrap().fields.is_empty());
}

#[tokio::test]
async fn q9_ship_cosmetics_skinr_and_emblems() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_005);
    session.user_id = Some(105);
    session.character_id = Some(90_000_105);
    session.solar_system_id = Some(30_000_142);

    // 1. Ship logo display & clear: covered state-backed in quasar_cosmetics_live_state.rs

    // 2. SKINR third-party sequencing pricing & jobs
    let seq_rsp = call_req(&ctx, &session, "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new(), true);
    assert_eq!(seq_rsp.status_code, STATUS_OK);

    // 3. Cosmetic Licenses (GetOwned)
    let lic_rsp = call_req(&ctx, &session, "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new(), true);
    assert_eq!(lic_rsp.status_code, STATUS_OK);

    // 4. Saved SKINR Design Drafts: authorization & distinct draft IDs
    let mut no_char = SessionState::new(20_099);
    no_char.user_id = Some(105);
    let unauth = call_req(&ctx, &no_char, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", Vec::new(), true);
    assert_eq!(unauth.status_code, 403, "SaveRequest must require character");

    let d1 = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", Vec::new(), true);
    assert_eq!(d1.status_code, STATUS_OK);
    let d2 = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", Vec::new(), true);
    assert_eq!(d2.status_code, STATUS_OK);
    assert_ne!(d1.payload.as_ref().unwrap().value, d2.payload.as_ref().unwrap().value, "Draft IDs must be distinct");

    let d1_id = WireMessage::decode(&d1.payload.unwrap().value).unwrap().fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();
    let mut d_req = Vec::new();
    encode_bytes_field(1, &d1_id, &mut d_req);

    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
    ] {
        let payload = if req_name.ends_with(".GetAllSavedRequest") || req_name.ends_with(".GetSaveCapacityRequest") { Vec::new() } else { d_req.clone() };
        let rsp = call_req(&ctx, &session, req_name, payload, true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }

    // 5. Sequencing job cap: 5 active jobs succeed, 6th rejected with 400
    for _ in 0..5 {
        let r = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", Vec::new(), true);
        assert_eq!(r.status_code, STATUS_OK);
    }
    let r_cap = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", Vec::new(), true);
    assert_eq!(r_cap.status_code, 400, "6th sequencing job must exceed cap");
    let r_done = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", Vec::new(), true);
    assert_eq!(r_done.status_code, STATUS_OK);

    assert_eq!(seq_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse");
    assert_eq!(lic_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse");
    assert!(!WireMessage::decode(&seq_rsp.payload.unwrap().value).unwrap().fields.is_empty());
    assert!(!WireMessage::decode(&lic_rsp.payload.unwrap().value).unwrap().fields.is_empty());
}

#[tokio::test]
async fn test_quasar_state_blobs_persistence_reload_sqlite() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_010);
    session.user_id = Some(110);
    session.character_id = Some(90_000_110);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role
    session.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    // Fund corporation master wallet with 100M ISK for freelance escrow
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.entry(98_000_001).or_insert_with(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = 98_000_001;
            c
        });
        corp.ceo_id = 90_000_110;
        corp.master_wallet = eve_db::IskCents::from_f64_isk(100_000_000.0);
    });

    // 1. Mutate Freelance via CreateRequest (with required_type_id = 35 Pyerite, batch_size = 10)
    let (mut create_bytes, mut cfg_b, mut param_b, mut opts_b, mut mod_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "Mining Expedition", &mut create_bytes);
    eve_proto::quasar::encode_string_field(2, "Mine ore", &mut create_bytes);
    eve_proto::quasar::encode_varint_field(3, 100, &mut create_bytes);
    eve_proto::quasar::encode_string_field(1, "35", &mut opts_b);
    encode_bytes_field(3, &opts_b, &mut param_b);
    encode_bytes_field(3, &param_b, &mut cfg_b);
    encode_bytes_field(6, &cfg_b, &mut create_bytes);
    eve_proto::quasar::encode_varint_field(4, 10, &mut mod_b);
    encode_bytes_field(7, &mod_b, &mut create_bytes);
    let fl_rsp = call_req(&ctx, &session, "eve_public.freelance.project.api.CreateRequest", create_bytes, true);
    assert_eq!(fl_rsp.status_code, STATUS_OK, "CreateRequest failed: {}", fl_rsp.status_message);

    // 2. Mutate Corp Goal via CreateRequest (with reward_pools) + server-observed mining + RedeemMyRewardsRequest
    ctx.db.with_mut_snapshot(|s| {
        let mut c = s.characters[&90_000_001].clone();
        c.character_id = 90_000_110;
        c.corporation_id = 98_000_001;
        s.characters.insert(90_000_110, c);
    });
    let (mut cg_req_b, mut curr_b, mut isk_b, mut pool_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_varint_field(1, eve_proto::quasar::encode_zigzag64(500_000), &mut curr_b);
    encode_bytes_field(1, &curr_b, &mut isk_b);
    encode_bytes_field(2, &isk_b, &mut pool_b);
    encode_bytes_field(8, &pool_b, &mut cg_req_b);
    let cg_create = call_req(&ctx, &session, "eve_public.goal.api.CreateRequest", cg_req_b, true);
    assert_eq!(cg_create.status_code, STATUS_OK);
    let cg_id_bytes = WireMessage::decode(&cg_create.payload.unwrap().value).unwrap().fields[0].value.as_bytes().unwrap().to_vec();
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx, 90_000_110, 100_000_000_110, 30_000_142, eve_db::InventoryFlag::Cargo, 1230, 50,
    );
    let mut cg_red_b = Vec::new();
    encode_bytes_field(1, &cg_id_bytes, &mut cg_red_b);
    let cg_rsp = call_req(&ctx, &session, "eve_public.corporationgoal.api.RedeemMyRewardsRequest", cg_red_b, true);
    assert_eq!(cg_rsp.status_code, STATUS_OK, "Corp goal RedeemMyRewardsRequest failed: {}", cg_rsp.status_message);

    // 3. Mutate Cosmetics (SKINR draft save and title equip)
    assert_eq!(call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", vec![0x0a, 0x04, 0x74, 0x65, 0x73, 0x74], true).status_code, STATUS_OK);
    call_req(&ctx, &session, "eve_public.cosmetic.character.title.api.admin.GrantRequest", make_id_request(101), true);
    assert_eq!(call_req(&ctx, &session, "eve_public.cosmetic.character.title.api.EquipRequest", make_id_request(101), true).status_code, STATUS_OK);

    // 4. Mutate Achievements (unlock achievement first, then claim rewards)
    assert_eq!(call_req(&ctx, &session, "eve_public.achievement.api.admin.CompleteRequest", make_id_request(2), true).status_code, STATUS_OK);
    let mut claim_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(3, 1, &mut claim_bytes);
    assert_eq!(call_req(&ctx, &session, "eve_public.achievement.reward.api.ClaimRequest", claim_bytes, true).status_code, eve_server::quasar_gateway::STATUS_PRECONDITION_FAILED);

    // 5. Mutate Tales, Career Goals, and Corp Skill Plans before reload
    assert_eq!(call_req(&ctx, &session, "eve.tale.api.StartTaleRequest", make_tale_start(42, 30_000_142), true).status_code, STATUS_OK);
    let mut prog_req = make_uuid_request([0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    eve_proto::quasar::encode_varint_field(2, 5, &mut prog_req);
    assert_eq!(call_req(&ctx, &session, "eve_public.career.goal.api.admin.ProgressRequest", prog_req, true).status_code, STATUS_OK);

    let (mut sp_attr, mut sp_val) = (Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "Persisted Corp Plan", &mut sp_attr);
    encode_bytes_field(1, &sp_attr, &mut sp_val);
    let sp_req = call_req(&ctx, &session, "eve_public.corporation.skill.plan.CreateRequest", sp_val, true);
    assert_eq!(sp_req.status_code, STATUS_OK, "Skill plan CreateRequest failed: {}", sp_req.status_message);

    let mut ch_state = eve_server::services::season_manager::CharacterSeasonState::default();
    eve_server::services::season_manager::ensure_challenges(&mut ch_state, 133_000_000_000_000_000);
    if let Some(c) = ch_state.challenges.get_mut(&1002) { c.progress = 3; }
    ctx.db.with_mut_snapshot(|s| {
        eve_server::services::season_manager::save_season_state_to_blobs(s, 90_000_106, &ch_state);
    });

    // Persist to SQLite and reopen a second ServerContext
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "quasar_blobs");

    // Verify all quasar_state_blobs are present in SQLite
    ctx_reloaded.db.with_snapshot(|s| {
        for key in &["quasar_freelance", "quasar_corp_goals", "quasar_cosmetics", "quasar_achievements", "quasar_active_tales"] {
            assert!(s.quasar_state_blobs.contains_key(*key), "{key} blob must persist in SQLite");
        }
    });

    // Actually invoke Quasar gateway handlers on ctx_reloaded and assert non-empty, valid state responses
    let r_title = call_req(&ctx_reloaded, &session, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new(), true);
    assert_eq!(r_title.status_code, STATUS_OK, "GetOwnedRequest after reload failed");

    let r_goal = call_req(&ctx_reloaded, &session, "eve_public.goal.api.GetAllRequest", Vec::new(), true);
    assert_eq!(r_goal.status_code, STATUS_OK, "Goal GetAllRequest after reload failed");

    let r_daily = call_req(&ctx_reloaded, &session, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new(), true);
    assert_eq!(r_daily.status_code, STATUS_OK, "Daily goal GetAllCurrentRequest after reload failed");

    let r_tales = call_req(&ctx_reloaded, &session, "eve.tale.api.GetActiveTalesCountRequest", Vec::new(), true);
    assert_eq!(r_tales.status_code, STATUS_OK, "GetActiveTalesCountRequest failed");
    let tales_wm = WireMessage::decode(&r_tales.payload.as_ref().unwrap().value).expect("decode tales");
    assert_eq!(tales_wm.fields.first().and_then(|f| f.value.as_varint()), Some(1), "active_tales must persist across reload");

    let r_ach = call_req(&ctx_reloaded, &session, "eve_public.achievement.api.GetRequest", make_id_request(2), true);
    assert_eq!(r_ach.status_code, STATUS_OK, "Achievement GetRequest failed");
    let ach_wm = WireMessage::decode(&r_ach.payload.as_ref().unwrap().value).expect("decode ach");
    assert!(ach_wm.fields.iter().any(|f| f.field_number == 1), "Achievement 2 must have completed_date set");
    assert!(ach_wm.fields.iter().all(|f| f.field_number != 2), "Achievement 2 must not emit not_completed when completed");

    let r_career = call_req(&ctx_reloaded, &session, "eve_public.app.eveonline.career.goal.GetAllRequest", Vec::new(), true);
    assert_eq!(r_career.status_code, STATUS_OK, "Career GetAllRequest failed");
    let car_wm = WireMessage::decode(&r_career.payload.as_ref().unwrap().value).expect("decode career");
    let has_p5 = car_wm.fields.iter().filter(|f| f.field_number == 1).any(|f| {
        f.value.as_bytes().and_then(|b| WireMessage::decode(b).ok()).map_or(false, |gw| {
            gw.fields.iter().any(|gf| gf.field_number == 2 && gf.value.as_varint() == Some(5))
        })
    });
    assert!(has_p5, "Career goal progress 5 must persist across SQLite reload");

    let r_sp = call_req(&ctx_reloaded, &session, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), true);
    assert_eq!(r_sp.status_code, STATUS_OK, "Corp skill plans GetAllRequest failed");
    let sp_wm = WireMessage::decode(&r_sp.payload.as_ref().unwrap().value).expect("decode sp");
    assert!(!sp_wm.fields.is_empty(), "Corp skill plans must persist across reload");

    let r_fl = call_req(&ctx_reloaded, &session, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new(), true);
    assert_eq!(r_fl.status_code, STATUS_OK, "Freelance GetAllActiveRequest after reload failed");
    let fl_wm = WireMessage::decode(&r_fl.payload.as_ref().unwrap().value).expect("decode freelance");
    assert!(!fl_wm.fields.is_empty(), "Freelance project must persist across reload");

    let r_ch = call_req(&ctx_reloaded, &session, "eve.seasonalevent.challenge.GetChallengesForCharacterRequest", make_sequential_id_request(90_000_106), true);
    assert_eq!(r_ch.status_code, STATUS_OK, "GetChallengesForCharacterRequest after reload failed");
    let ch_wm = WireMessage::decode(&r_ch.payload.as_ref().unwrap().value).expect("decode challenges");
    let ch1002_cur = ch_wm.fields.iter().filter(|f| f.field_number == 1).find_map(|f| {
        let b = f.value.as_bytes()?;
        let m = WireMessage::decode(b).ok()?;
        if m.fields.iter().find(|x| x.field_number == 1)?.value.as_varint()? == 1002 {
            m.fields.iter().find(|x| x.field_number == 3)?.value.as_varint()
        } else {
            None
        }
    });
    assert_eq!(ch1002_cur, Some(3), "Seasonal challenge 1002 progress 3 must persist across SQLite reload");
}
