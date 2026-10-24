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
        Vec::new(),
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

    let track_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.communitytrack.api.GetRequest",
        make_sequential_id_request(1),
    )
    .await;
    assert_eq!(track_rsp.status_code, STATUS_OK, "track_rsp failed: {}", track_rsp.status_message);
    assert_eq!(
        track_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.communitytrack.api.GetResponse"
    );

    let def_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.communitytrack.api.GetRequest",
        make_sequential_id_request(1),
    )
    .await;
    assert_eq!(def_rsp.status_code, STATUS_OK);
    assert_eq!(
        def_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.communitytrack.api.GetResponse"
    );

    let prog_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.communitytrack.api.AddProgressRequest",
        make_sequential_id_request(1),
    )
    .await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);

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

    let goal_create_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.goal.api.CreateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(goal_create_rsp.status_code, STATUS_OK);

    let titles_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(titles_rsp.status_code, STATUS_OK);

    let camp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(camp_rsp.status_code, STATUS_OK);
    assert_eq!(
        camp_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.campaign.api.GetAllResponse"
    );

    let chall_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        make_sequential_id_request(90_000_001),
    )
    .await;
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
    let pal_wm = WireMessage::decode(&pal_rsp.payload.as_ref().unwrap().value).expect("decode palette");
    assert!(!pal_wm.fields.is_empty(), "Palette response should contain fields");
    let tit_wm = WireMessage::decode(&tit_rsp.payload.as_ref().unwrap().value).expect("decode title");
    assert!(!tit_wm.fields.is_empty(), "Title response should contain fields");
    let pnt_wm = WireMessage::decode(&pnt_rsp.payload.as_ref().unwrap().value).expect("decode paintwork");
    assert!(!pnt_wm.fields.is_empty(), "Paintwork response should contain fields");
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
    for req_name in [
        "eve_public.cosmetic.ship.logo.DisplayRequest",
        "eve_public.cosmetic.ship.logo.ClearRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }

    // 2. SKINR third-party sequencing pricing & jobs
    let seq_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
        true,
    );
    assert_eq!(seq_rsp.status_code, STATUS_OK);

    // 3. Cosmetic Licenses (GetOwned)
    let lic_rsp = call_req(
        &ctx,
        &session,
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
        true,
    );
    assert_eq!(lic_rsp.status_code, STATUS_OK);

    // 4. Saved SKINR Design Drafts
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
    ] {
        let rsp = call_req(&ctx, &session, req_name, Vec::new(), true);
        assert_eq!(rsp.status_code, STATUS_OK, "{req_name}: {}", rsp.status_message);
    }
    assert_eq!(
        seq_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    );
    assert_eq!(
        lic_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse"
    );
    let seq_wm = WireMessage::decode(&seq_rsp.payload.as_ref().unwrap().value).expect("decode pricing");
    assert!(!seq_wm.fields.is_empty(), "Pricing response should contain fields");
    let lic_wm = WireMessage::decode(&lic_rsp.payload.as_ref().unwrap().value).expect("decode licenses");
    assert!(!lic_wm.fields.is_empty(), "License response should contain fields");
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

    // 1. Mutate Freelance via CreateRequest
    let mut create_bytes = Vec::new();
    eve_proto::quasar::encode_string_field(1, "Mining Expedition", &mut create_bytes);
    eve_proto::quasar::encode_string_field(2, "Mine ore", &mut create_bytes);
    eve_proto::quasar::encode_varint_field(3, 100, &mut create_bytes);
    let fl_rsp = call_req(
        &ctx,
        &session,
        "eve_public.freelance.project.api.CreateRequest",
        create_bytes,
        true,
    );
    assert_eq!(fl_rsp.status_code, STATUS_OK, "CreateRequest failed: {}", fl_rsp.status_message);

    // 2. Mutate Corp Goal via RedeemMyRewardsRequest
    let cg_rsp = call_req(
        &ctx,
        &session,
        "eve_public.corporationgoal.api.RedeemMyRewardsRequest",
        Vec::new(),
        true,
    );
    assert_eq!(cg_rsp.status_code, STATUS_OK, "Corp goal RedeemMyRewardsRequest failed: {}", cg_rsp.status_message);

    // 3. Mutate Cosmetics (SKINR draft save and title equip)
    let draft_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        vec![0x0a, 0x04, 0x74, 0x65, 0x73, 0x74],
        true,
    );
    assert_eq!(draft_rsp.status_code, STATUS_OK, "SaveRequest failed: {}", draft_rsp.status_message);

    let eq_bytes = make_id_request(101);
    let eq_rsp = call_req(
        &ctx,
        &session,
        "eve_public.cosmetic.character.title.api.EquipRequest",
        eq_bytes,
        true,
    );
    assert_eq!(eq_rsp.status_code, STATUS_OK, "EquipRequest failed: {}", eq_rsp.status_message);

    // 4. Mutate Achievements (unlock achievement first, then claim rewards)
    let comp_bytes = make_id_request(101);
    let comp_rsp = call_req(
        &ctx,
        &session,
        "eve_public.achievement.api.admin.CompleteRequest",
        comp_bytes,
        true,
    );
    assert_eq!(comp_rsp.status_code, STATUS_OK, "CompleteRequest failed: {}", comp_rsp.status_message);

    let mut claim_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(3, 1, &mut claim_bytes);
    let ach_rsp = call_req(
        &ctx,
        &session,
        "eve_public.achievement.reward.api.ClaimRequest",
        claim_bytes,
        true,
    );
    assert_eq!(ach_rsp.status_code, STATUS_OK, "ClaimRequest failed: {}", ach_rsp.status_message);

    // Persist to SQLite and reopen a second ServerContext
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "quasar_blobs");

    // Verify all four quasar_state_blobs are present in SQLite
    ctx_reloaded.db.with_snapshot(|s| {
        assert!(s.quasar_state_blobs.contains_key("quasar_freelance"), "quasar_freelance blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_corp_goals"), "quasar_corp_goals blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_cosmetics"), "quasar_cosmetics blob must persist in SQLite");
        assert!(s.quasar_state_blobs.contains_key("quasar_achievements"), "quasar_achievements blob must persist in SQLite");
    });

    // Actually invoke Quasar gateway handlers on ctx_reloaded and assert non-empty, valid state responses
    let r_title = call_req(&ctx_reloaded, &session, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new(), true);
    assert_eq!(r_title.status_code, STATUS_OK, "GetOwnedRequest after reload failed");
    assert!(r_title.payload.is_some(), "GetOwnedRequest response payload missing");

    let r_goal = call_req(&ctx_reloaded, &session, "eve_public.goal.api.GetAllRequest", Vec::new(), true);
    assert_eq!(r_goal.status_code, STATUS_OK, "Goal GetAllRequest after reload failed");
    assert!(r_goal.payload.is_some(), "Goal GetAllRequest payload missing");

    let r_daily = call_req(&ctx_reloaded, &session, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new(), true);
    assert_eq!(r_daily.status_code, STATUS_OK, "Daily goal GetAllCurrentRequest after reload failed");
    assert!(r_daily.payload.is_some(), "Daily goal payload missing");

    let r_career = call_req(&ctx_reloaded, &session, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), true);
    assert_eq!(r_career.status_code, STATUS_OK, "Career goal GetDefinitionsRequest after reload failed");
    assert!(r_career.payload.is_some(), "Career goal definitions payload missing");

    let r_fl = call_req(&ctx_reloaded, &session, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new(), true);
    assert_eq!(r_fl.status_code, STATUS_OK, "Freelance GetAllActiveRequest after reload failed");
    assert!(r_fl.payload.is_some(), "Freelance GetAllActiveRequest payload missing");
}
