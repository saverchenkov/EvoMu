use eve_machonet::SessionState;
use crate::quasar_progression_complete::quasar_rpc;
use crate::quasar_space_cosmetics_complete::call_req;
use eve_proto::quasar::{
    encode_bytes_field, EvePublicNotice, WireField, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

fn make_sequential_id_request(id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(1, id as u64, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_id_request(id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

#[allow(dead_code)]
fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut notifs = Vec::new();
    while let Ok(n) = rx.try_recv() {
        notifs.push(n);
    }
    notifs
}

#[tokio::test]
async fn quasar_achievements_categories_scores_and_claims() {
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
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
        make_id_request(101),
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
        make_id_request(101),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.api.admin.CompleteRequest",
        make_id_request(101),
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
    assert!(total_score.unwrap_or(0) > 0, "Character score should be > 0");

    let unclaimed_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unclaimed_rsp.status_code, STATUS_OK);

    let mut claim_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(3, 1, &mut claim_bytes); // all_rewards = true
    let claim_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.reward.api.ClaimRequest",
        claim_bytes,
    )
    .await;
    assert_eq!(claim_rsp.status_code, STATUS_OK, "claim_rsp failed: {}", claim_rsp.status_message);

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
    // parity: quasar:eveProto/generated/eve_public/communitytrack/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/communitytrack/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/goal/api/requests_pb2.pyj
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

    let track_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", make_sequential_id_request(1)).await;
    assert_eq!(track_rsp.status_code, STATUS_OK, "track_rsp failed: {}", track_rsp.status_message);
    assert_eq!(track_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.communitytrack.api.GetResponse");
    let track_wm = WireMessage::decode(&track_rsp.payload.unwrap().value).expect("decode track GetResponse");
    let track_attr_wm = WireMessage::decode(track_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let track_def_wm = WireMessage::decode(track_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(track_def_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1));
    assert_eq!(track_attr_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()).unwrap_or(0), 0);

    let def_rsp = quasar_rpc(&ctx, &mut sess, "eve.communitytrack.api.GetRequest", make_sequential_id_request(1)).await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(def_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve.communitytrack.api.GetResponse");

    let mut add_prog_req = make_sequential_id_request(1);
    eve_proto::quasar::encode_varint_field(2, 25, &mut add_prog_req);
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve.communitytrack.api.AddProgressRequest", add_prog_req).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let track_after_wm = WireMessage::decode(&quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", make_sequential_id_request(1)).await.payload.unwrap().value).unwrap();
    let track_after_attr = WireMessage::decode(track_after_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(track_after_attr.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(25));

    let notifs = drain_notices(&mut stream_rx);
    assert!(!notifs.is_empty(), "Track notice should be emitted");
    assert_eq!(
        notifs[0].payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.communitytrack.api.ProgressedNotice"
    );

    let start_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.StartTaleRequest",
        make_sequential_id_request(1),
    )
    .await;
    assert_eq!(start_rsp.status_code, STATUS_OK);

    let count_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.GetActiveTalesCountRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(count_rsp.status_code, STATUS_OK);
    assert_eq!(
        count_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.tale.api.GetActiveTalesCountResponse"
    );

    let stop_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.tale.api.StopTalesRequest",
        make_sequential_id_request(1),
    )
    .await;
    assert_eq!(stop_rsp.status_code, STATUS_OK);

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

    ctx.quasar_gateway.progression.extra.register_campaign(
        eve_server::services::quasar_progression_extra::CampaignRecord {
            id: 1, name: "Amarr-Minmatar Frontline Campaign".into(), description: "Secure strategic outposts".into(), active: true, objectives: vec![101],
        },
        vec![eve_server::services::quasar_progression_extra::CampaignObjectiveRecord {
            id: 101, campaign_id: 1, name: "Capture Outposts".into(),
            current_progress: 0, target_progress: 100, reward_pool_isk: 0,
            committed_participants: Vec::new(), contributors: Default::default(),
        }],
    );
    let camp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await;
    assert_eq!(camp_rsp.status_code, STATUS_OK);
    assert_eq!(camp_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.campaign.api.GetAllResponse");

    // Empty or unknown campaign / objective must return STATUS_BAD_REQUEST / STATUS_NOT_FOUND without mutating state
    let empty_camp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.api.GetRequest", Vec::new()).await;
    assert_eq!(empty_camp.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);
    let empty_obj = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", Vec::new()).await;
    assert_eq!(empty_obj.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);
    let missing_obj = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", make_id_request(9999)).await;
    assert_eq!(missing_obj.status_code, eve_server::quasar_gateway::STATUS_NOT_FOUND);
    assert!(!ctx.quasar_gateway.progression.extra.campaign_objectives.read().unwrap().contains_key(&9999));

    // Commit to campaign objective 101: ordinary mining does not feed campaign; redeeming with 0 rewards returns 412; FW capture activity funds objective reward pool and credits balance + WalletJournalEntry
    let commit_camp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.CommitRequest", make_id_request(101)).await;
    assert_eq!(commit_camp.status_code, STATUS_OK);
    let red_empty = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.RedeemRewardsRequest", make_id_request(101)).await;
    assert_eq!(red_empty.status_code, eve_server::quasar_gateway::STATUS_PRECONDITION_FAILED);
    eve_server::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
        &ctx, 90_000_001, 100_000_000_101, 30_000_142, eve_db::InventoryFlag::Cargo, 1230, 5,
    );
    assert_eq!(
        ctx.quasar_gateway.progression.extra.campaign_objectives.read().unwrap().get(&101).and_then(|o| o.contributors.get(&90_000_001)).copied(),
        None,
        "Ordinary mining must not increment campaign contributor scores"
    );
    ctx.quasar_gateway.progression.extra.on_participant_activity(90_000_001, 5, 2);
    let (pre_bal, pre_jlen) = ctx.db.with_snapshot(|s| (s.characters[&90_000_001].balance.0, s.wallet_journal.len()));
    let red_camp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.objective.api.RedeemRewardsRequest", make_id_request(101)).await;
    assert_eq!(red_camp.status_code, STATUS_OK);
    ctx.db.with_snapshot(|s| {
        assert!(s.characters[&90_000_001].balance.0 > pre_bal, "Campaign reward funded by FW activity must credit character balance");
        assert_eq!(s.wallet_journal.len(), pre_jlen + 1, "Campaign reward must append wallet journal entry");
        assert_eq!(s.wallet_journal.last().unwrap().entry_type, "CampaignReward");
    });

    let chall_rsp = quasar_rpc(&ctx, &mut sess, "eve.seasonalevent.challenge.GetChallengesForCharacterRequest", make_sequential_id_request(90_000_001)).await;
    assert_eq!(chall_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn q9_corp_palette_titles_structure_paintwork_and_app_infra() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj, quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/app/eveonline/career/goal_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_006);
    session.user_id = Some(106);
    session.character_id = Some(90_000_106);
    session.corporation_id = Some(98_000_001);
    session.corp_role = 0x01; // Director role

    // 1. Corporation color palette
    let pal_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.corporation.palette.api.GetRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pal_rsp.status_code, STATUS_OK);

    // 2. Character titles (GetOwned, GetSelected, Select, ClearSelection)
    let tit_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        Vec::new(),
        true,
    );
    assert_eq!(tit_rsp.status_code, STATUS_OK);

    for req_name in [
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        "eve_public.cosmetic.character.title.api.EquipRequest",
        "eve_public.cosmetic.character.title.api.UnequipRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name} failed");
    }

    // 3. Structure paintwork & paintwork licenses
    let pnt_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.api.GetRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_rsp.status_code, STATUS_OK);

    let pnt_sys_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_sys_rsp.status_code, STATUS_OK);

    let pnt_lic_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest",
        Vec::new(),
        true,
    );
    assert_eq!(pnt_lic_rsp.status_code, STATUS_OK);

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
        pnt_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.cosmetic.structure.paintwork.api.GetResponse"
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
    assert!(!WireMessage::decode(&pnt_rsp.payload.as_ref().unwrap().value).unwrap().fields.is_empty());
}

#[tokio::test]
async fn q9_ship_cosmetics_skinr_and_emblems() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/draft/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut session = SessionState::new(20_005);
    session.user_id = Some(105);
    session.character_id = Some(90_000_105);
    session.solar_system_id = Some(30_000_142);

    // 1. Ship logo display & clear
    for req_name in ["eve_public.cosmetic.ship.logo.DisplayRequest", "eve_public.cosmetic.ship.logo.ClearRequest"] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }

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
    assert_ne!(d1.payload.unwrap().value, d2.payload.unwrap().value, "Draft IDs must be distinct");

    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
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
    let draft_rsp = call_req(&ctx, &session, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", vec![0x0a, 0x04, 0x74, 0x65, 0x73, 0x74], true);
    assert_eq!(draft_rsp.status_code, STATUS_OK, "SaveRequest failed: {}", draft_rsp.status_message);

    let eq_rsp = call_req(&ctx, &session, "eve_public.cosmetic.character.title.api.EquipRequest", make_id_request(101), true);
    assert_eq!(eq_rsp.status_code, STATUS_OK, "EquipRequest failed: {}", eq_rsp.status_message);

    // 4. Mutate Achievements (unlock achievement first, then claim rewards)
    let comp_rsp = call_req(&ctx, &session, "eve_public.achievement.api.admin.CompleteRequest", make_id_request(101), true);
    assert_eq!(comp_rsp.status_code, STATUS_OK, "CompleteRequest failed: {}", comp_rsp.status_message);

    let mut claim_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(3, 1, &mut claim_bytes);
    let ach_rsp = call_req(&ctx, &session, "eve_public.achievement.reward.api.ClaimRequest", claim_bytes, true);
    assert_eq!(ach_rsp.status_code, STATUS_OK, "ClaimRequest failed: {}", ach_rsp.status_message);

    // 5. Mutate Tales, Career Goals, and Corp Skill Plans before reload
    let start_tale = call_req(&ctx, &session, "eve.tale.api.StartTaleRequest", make_sequential_id_request(42), true);
    assert_eq!(start_tale.status_code, STATUS_OK, "StartTaleRequest failed");

    let mut prog_req = make_uuid_request([0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    eve_proto::quasar::encode_varint_field(2, 5, &mut prog_req);
    let cg_prog = call_req(&ctx, &session, "eve_public.career.goal.api.admin.ProgressRequest", prog_req, true);
    assert_eq!(cg_prog.status_code, STATUS_OK, "Career goal ProgressRequest failed");

    let mut sp_attr = Vec::new();
    eve_proto::quasar::encode_string_field(1, "Persisted Corp Plan", &mut sp_attr);
    let mut sp_val = Vec::new();
    encode_bytes_field(1, &sp_attr, &mut sp_val);
    let sp_req = call_req(&ctx, &session, "eve_public.corporation.skill.plan.CreateRequest", sp_val, true);
    assert_eq!(sp_req.status_code, STATUS_OK, "Skill plan CreateRequest failed: {}", sp_req.status_message);

    // Persist to SQLite and reopen a second ServerContext
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "quasar_blobs");

    // Verify all quasar_state_blobs are present in SQLite
    ctx_reloaded.db.with_snapshot(|s| {
        assert!(s.quasar_state_blobs.contains_key("quasar_freelance"), "quasar_freelance blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_corp_goals"), "quasar_corp_goals blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_cosmetics"), "quasar_cosmetics blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_achievements"), "quasar_achievements blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_active_tales"), "quasar_active_tales blob must persist in SQLite");
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

    let r_ach = call_req(&ctx_reloaded, &session, "eve_public.achievement.api.GetRequest", make_id_request(101), true);
    assert_eq!(r_ach.status_code, STATUS_OK, "Achievement GetRequest failed");
    let ach_wm = WireMessage::decode(&r_ach.payload.as_ref().unwrap().value).expect("decode ach");
    assert_eq!(ach_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(0), "Achievement 101 must be completed");

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
}
