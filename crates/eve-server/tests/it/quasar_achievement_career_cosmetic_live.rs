//! Live integration tests for 28 remediated Quasar rows across:
//! - AIR Achievements (`feat:quasar.achievement`, `achievement/*`)
//! - AIR Career Program (`feat:quasar.career`, `career/goal/*`)
//! - Daily Goals (`feat:quasar.dailygoal`, `dailygoal/*`)
//! - Cosmetics, Paragon Hub Market & SKINR Sequencing (`feat:quasar.cosmetic`, `cosmetic/*`)
//! - AI Assistant Chat, Campaign Trigger EventMatch Definitions & Faction Activity Admin

use crate::common::login_pilot;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK, STATUS_PRECONDITION_FAILED};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const JITA_STATION: i64 = 60_003_760;
const PLEX_TYPE_ID: u32 = 44_992;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, impl std::fmt::Debug> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash '{cmd}' failed: {e:?}"))
}

fn seq_msg(val: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, val, &mut b);
    b
}

fn uuid_msg(uuid: &[u8; 16]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    b
}

fn uuid_u32_msg(id: u32) -> Vec<u8> {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    uuid_msg(&uuid)
}

fn decode_wire(rsp: &EvePublicResponse) -> WireMessage {
    let payload = rsp.payload.as_ref().expect("response payload");
    WireMessage::decode(&payload.value).expect("decode WireMessage")
}

fn wire_sub(wm: &WireMessage, fno: u32) -> Option<WireMessage> {
    wm.fields
        .iter()
        .find(|f| f.field_number == fno)
        .and_then(|f| f.value.as_bytes())
        .and_then(|b| WireMessage::decode(b).ok())
}

fn wire_bytes(wm: &WireMessage, fno: u32) -> Vec<u8> {
    wm.fields
        .iter()
        .find(|f| f.field_number == fno)
        .and_then(|f| f.value.as_bytes())
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

fn wire_str(wm: &WireMessage, fno: u32) -> String {
    String::from_utf8(wire_bytes(wm, fno)).unwrap_or_default()
}

async fn boot_gm(ctx: &ServerContext) -> SessionState {
    ctx.db.provision_gm_account("gm_batch_a", "gm-pass", 90_000_003, "ISD BatchA");
    let mut gm = SessionState::new(99_001);
    login_pilot(ctx, &mut gm, "gm_batch_a", "gm-pass").await;
    rpc(
        ctx,
        &mut gm,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
    )
    .await
    .expect("select gm");
    gm
}

async fn create_live_pilot(
    ctx: &ServerContext,
    gm: &mut SessionState,
    sid: u64,
    user: &str,
    char_name: &str,
    is_gm: bool,
) -> (SessionState, u32) {
    let _ = ctx.db.provision_player_account(user, "pilot-pass");
    if is_gm {
        slash(ctx, gm, &format!("/role {user} GM")).await;
    }
    let mut sess = SessionState::new(sid);
    login_pilot(ctx, &mut sess, user, "pilot-pass").await;
    let doll = PyRep::Dict(vec![(PyRep::String("hair".into()), PyRep::String("braids".into()))]);
    let pose = PyRep::Dict(vec![(PyRep::String("poseID".into()), PyRep::Integer(1))]);
    let cid = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(char_name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            doll,
            pose,
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll")
    .as_i64()
    .expect("char id") as u32;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(cid as i64)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(2);
    (sess, cid)
}

async fn q_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-live-batch-a".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-live-batch-a".to_vec(),
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
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(&b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Missing response_proto in publicGatewaySvc.SendRequest");
}

fn subscribe(ctx: &ServerContext, sess: &SessionState) -> Receiver<EvePublicNotice> {
    ctx.quasar_gateway
        .subscribe_session(sess, Vec::new(), ctx.now_unix())
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        out.push(n);
    }
    out
}

fn has_notice(notices: &[EvePublicNotice], expected: &str) -> bool {
    notices.iter().any(|n| {
        n.payload
            .as_ref()
            .map(|p| p.normalized_type_name() == expected)
            .unwrap_or(false)
    })
}

#[tokio::test]
async fn test_quasar_achievements_career_and_daily_goals_live() {
    // parity: feat:quasar.achievement
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj
    // parity: feat:quasar.career
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj
    // parity: feat:quasar.dailygoal
    // parity: quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.achievement",
        "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/category/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/reward/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/showcase/api/requests_pb2.pyj",
        "feat:quasar.career",
        "quasar:eveProto/generated/eve_public/career/goal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/career/goal/api/requests_pb2.pyj",
        "feat:quasar.dailygoal",
        "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut gm = boot_gm(&ctx).await;
    let (mut pilot, cid) = create_live_pilot(&ctx, &mut gm, 91_001, "ach_pilot", "Ach Pilot", true).await;
    let mut rx = subscribe(&ctx, &pilot);

    // 1. Achievement CDN GetLatestRequest (no_local = true) -> local CDN URL + CRC
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let cdn_rsp = q_rpc(&ctx, &mut pilot, "eve_public.achievement.definition.api.cdn.GetLatestRequest", cdn_req).await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    let cdn_wm = decode_wire(&cdn_rsp);
    let cp_wm = wire_sub(&cdn_wm, 1).expect("checkpoint");
    let cdn_url = wire_str(&cp_wm, 1);
    assert!(cdn_url.contains("/achievements/v24.1."));

    // 2. Achievement GetRequest, AddProgressRequest -> ProgressedNotice, Category GetRequest & ResetRequest, Showcase, Reward
    let first_ach_id = ctx
        .sde
        .load_achievements()
        .expect("SDE achievements")
        .values()
        .filter(|a| a.is_enabled)
        .map(|a| a.achievement_id)
        .min()
        .expect("at least one enabled SDE achievement");
    let mut ach_get_req = Vec::new();
    encode_bytes_field(1, &uuid_u32_msg(first_ach_id), &mut ach_get_req);
    let ach_get = q_rpc(&ctx, &mut pilot, "eve_public.achievement.api.GetRequest", ach_get_req).await;
    assert_eq!(ach_get.status_code, STATUS_OK);

    let mut add_prog = Vec::new();
    encode_bytes_field(1, &uuid_u32_msg(first_ach_id), &mut add_prog);
    encode_varint_field(2, 1, &mut add_prog);
    let prog_rsp = q_rpc(&ctx, &mut pilot, "eve_public.achievement.api.admin.AddProgressRequest", add_prog).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK);
    let ach_notices = drain_notices(&mut rx);
    assert!(has_notice(&ach_notices, "eve_public.achievement.api.ProgressedNotice"));

    let mut cat_get = Vec::new();
    encode_bytes_field(1, &uuid_u32_msg(1), &mut cat_get);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.achievement.category.api.GetRequest", cat_get.clone()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.achievement.category.api.admin.ResetRequest", cat_get).await.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut rx), "eve_public.achievement.category.api.ProgressedNotice"));

    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.achievement.reward.api.ClaimRequest", Vec::new()).await.status_code, STATUS_PRECONDITION_FAILED);

    // 3. Career Goals: GetDefinitionsRequest, GetAllRequest, admin.ProgressRequest, admin.CompleteRequest, ClaimRewardRequest
    let defs_rsp = q_rpc(&ctx, &mut pilot, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new()).await;
    assert_eq!(defs_rsp.status_code, STATUS_OK);
    let all_cg_rsp = q_rpc(&ctx, &mut pilot, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await;
    assert_eq!(all_cg_rsp.status_code, STATUS_OK);
    let all_cg_wm = decode_wire(&all_cg_rsp);
    let first_goal = wire_sub(&all_cg_wm, 1).expect("first career goal");
    let first_goal_uuid: [u8; 16] = wire_sub(&first_goal, 1)
        .map(|id_m| wire_bytes(&id_m, 1))
        .expect("career goal uuid")
        .as_slice()
        .try_into()
        .unwrap();
    assert_eq!(first_goal_uuid.len(), 16);
    assert_ne!(first_goal_uuid, [0u8; 16]);

    let mut cg_prog = Vec::new();
    encode_bytes_field(1, &uuid_msg(&first_goal_uuid), &mut cg_prog);
    encode_varint_field(2, 1, &mut cg_prog);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.career.goal.api.admin.ProgressRequest", cg_prog).await.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut rx), "eve_public.career.goal.api.ProgressedNotice"));

    let mut cg_comp = Vec::new();
    encode_bytes_field(1, &uuid_msg(&first_goal_uuid), &mut cg_comp);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.career.goal.api.admin.CompleteRequest", cg_comp.clone()).await.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut rx), "eve_public.career.goal.api.CompletedNotice"));
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.career.goal.api.ClaimRewardRequest", cg_comp).await.status_code, STATUS_OK);

    // 4. Daily Goals: SDE-seeded on fresh ServerContext::new(), GetAllCurrentRequest, GetRequest, admin.ProgressRequest -> notices, RedeemRequest
    let dg_all = q_rpc(&ctx, &mut pilot, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(dg_all.status_code, STATUS_OK);
    let dg_wm = decode_wire(&dg_all);
    let first_dg_goal = wire_sub(&dg_wm, 1).expect("seeded daily goal");
    let first_dg_id = wire_sub(&first_dg_goal, 1).expect("seeded daily goal id");
    let dg_uuid: [u8; 16] = wire_bytes(&first_dg_id, 1)
        .as_slice()
        .try_into()
        .unwrap();
    assert_eq!(dg_uuid.len(), 16);
    assert_ne!(dg_uuid, [0u8; 16]);

    let mut dg_get = Vec::new();
    encode_bytes_field(1, &uuid_msg(&dg_uuid), &mut dg_get);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.dailygoal.api.GetRequest", dg_get.clone()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.dailygoal.api.GetAllWithRewardsRequest", Vec::new()).await.status_code, STATUS_OK);

    let mut dg_prog = Vec::new();
    encode_bytes_field(1, &uuid_msg(&dg_uuid), &mut dg_prog);
    encode_bytes_field(2, &seq_msg(cid as u64), &mut dg_prog);
    encode_varint_field(3, 5_000, &mut dg_prog);
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.dailygoal.api.admin.ProgressRequest", dg_prog).await.status_code, STATUS_OK);
    let dg_notices = drain_notices(&mut rx);
    assert!(has_notice(&dg_notices, "eve_public.dailygoal.api.ProgressedNotice"));
    assert!(has_notice(&dg_notices, "eve_public.dailygoal.api.CompletedNotice"));
    assert_eq!(q_rpc(&ctx, &mut pilot, "eve_public.dailygoal.api.RedeemRequest", dg_get).await.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_cosmetics_market_and_skinr_live() {
    // parity: feat:quasar.cosmetic
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve/cosmetic/market/skin/listing/api/request_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.cosmetic",
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve/cosmetic/market/skin/listing/api/request_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut gm = boot_gm(&ctx).await;
    let (mut seller, seller_cid) = create_live_pilot(&ctx, &mut gm, 92_001, "skin_seller", "Skin Seller", true).await;
    let (mut buyer, _buyer_cid) = create_live_pilot(&ctx, &mut gm, 92_002, "skin_buyer", "Skin Buyer", true).await;

    slash(&ctx, &mut seller, "/giveisk 10000000").await;
    slash(&ctx, &mut buyer, "/giveisk 10000000").await;
    let plex_kv = slash(&ctx, &mut seller, &format!("/giveitem {PLEX_TYPE_ID} 500")).await;
    let plex_item_id = plex_kv.dict_get("itemID").and_then(|v| v.as_i64()).expect("plex itemID");
    let seller_station = seller.station_id.unwrap_or(JITA_STATION);
    rpc(&ctx, &mut seller, "invbroker", "DepositPlexToVault", &[PyRep::Integer(seller_station), PyRep::Integer(plex_item_id), PyRep::Integer(500)]).await.expect("DepositPlexToVault");

    let mut seller_rx = subscribe(&ctx, &seller);
    let mut buyer_rx = subscribe(&ctx, &buyer);

    // 1. Character Title Definitions: GetLatestRequest & GetAllRequest
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.character.title.definition.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);

    // 2. Paragon Hub Market Fees & Component Listings
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new()).await.status_code, STATUS_OK);
    let fees_def_rsp = q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest", Vec::new()).await;
    assert_eq!(fees_def_rsp.status_code, STATUS_OK);
    let buyer_fees_ver = wire_str(&decode_wire(&fees_def_rsp), 2);

    let comp_list_rsp = q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(comp_list_rsp.status_code, STATUS_OK);
    let comp_wm = decode_wire(&comp_list_rsp);
    let first_comp = wire_sub(&comp_wm, 1).expect("seeded component listing");
    let comp_uuid: [u8; 16] = wire_sub(&first_comp, 1)
        .map(|m| wire_bytes(&m, 1))
        .expect("comp uuid")
        .as_slice()
        .try_into()
        .unwrap();
    assert_eq!(comp_uuid.len(), 16);
    assert_ne!(comp_uuid, [0u8; 16]);

    let mut buy_comp = Vec::new();
    encode_bytes_field(1, &uuid_msg(&comp_uuid), &mut buy_comp);
    encode_varint_field(2, 2, &mut buy_comp);
    encode_varint_field(3, 1, &mut buy_comp);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest", buy_comp).await.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut seller_rx), "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"));

    // 3. SKINR Sequencing Pricing, Durations & Natural Job Completion
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest", Vec::new()).await.status_code, STATUS_OK);

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coat_id = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1))
        .expect("coating comp")
        .component_id;
    let mut comp_id_m = Vec::new();
    encode_bytes_field(1, &seq_msg(coat_id as u64), &mut comp_id_m);
    let mut grant_coat = Vec::new();
    encode_bytes_field(1, &comp_id_m, &mut grant_coat);
    encode_varint_field(2, 5, &mut grant_coat);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_coat).await.status_code, STATUS_OK);
    let _ = drain_notices(&mut seller_rx);

    let rifter_type_id = ctx.sde.types.values().find(|t| t.name == "Rifter").map(|t| t.type_id).unwrap_or(587);
    let mut coat_cfg = Vec::new();
    encode_bytes_field(1, &seq_msg(coat_id as u64), &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &seq_msg(1), &mut slot_msg);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    encode_bytes_field(2, &seq_msg(rifter_type_id as u64), &mut layout);
    encode_varint_field(3, 1, &mut layout);
    let mut init_job = Vec::new();
    encode_string_field(1, "Live Rifter SKIN", &mut init_job);
    encode_string_field(2, "Republic", &mut init_job);
    encode_bytes_field(3, &layout, &mut init_job);
    encode_varint_field(5, 1, &mut init_job);

    let init_rsp = q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_job).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut seller_rx), "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice"));

    let init_wm = decode_wire(&init_rsp);
    let job_uuid: [u8; 16] = wire_sub(&init_wm, 1).map(|m| wire_bytes(&m, 1)).expect("job uuid").as_slice().try_into().unwrap();
    assert_eq!(job_uuid.len(), 16);
    assert_ne!(job_uuid, [0u8; 16]);
    let job_attrs = wire_sub(&init_wm, 2).expect("job attrs");
    let skin_hex = wire_sub(&job_attrs, 1).map(|s| wire_str(&s, 1)).expect("skin hex");

    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await.status_code, STATUS_OK);
    ctx.advance_secs(100_000);
    let mut get_job = Vec::new();
    encode_bytes_field(1, &uuid_msg(&job_uuid), &mut get_job);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest", get_job).await.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut seller_rx), "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"));

    // 4. Paragon Hub SKIN Listing & Purchase
    let mut price_isk = Vec::new();
    encode_varint_field(1, 200_000 * 2, &mut price_isk);
    let mut skin_id = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id);
    let mut lic_id = Vec::new();
    encode_bytes_field(1, &seq_msg(seller_cid as u64), &mut lic_id);
    encode_bytes_field(2, &skin_id, &mut lic_id);
    let mut dur = Vec::new();
    encode_varint_field(1, 86_400, &mut dur);
    let mut target = Vec::new();
    encode_varint_field(1, 1, &mut target);
    let mut create_listing = Vec::new();
    encode_bytes_field(1, &price_isk, &mut create_listing);
    encode_bytes_field(3, &lic_id, &mut create_listing);
    encode_bytes_field(4, &dur, &mut create_listing);
    encode_varint_field(5, 1, &mut create_listing);
    encode_bytes_field(6, &target, &mut create_listing);

    let create_rsp = q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", create_listing).await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    assert!(has_notice(&drain_notices(&mut seller_rx), "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"));

    let get_all_skins = q_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(get_all_skins.status_code, STATUS_OK);
    let get_all_wm = decode_wire(&get_all_skins);
    let first_listing = wire_sub(&get_all_wm, 1).expect("skin listing");
    let listing_uuid: [u8; 16] = wire_sub(&first_listing, 1).map(|m| wire_bytes(&m, 1)).expect("listing uuid").as_slice().try_into().unwrap();
    assert_eq!(listing_uuid.len(), 16);
    assert_ne!(listing_uuid, [0u8; 16]);
    assert_eq!(q_rpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest", Vec::new()).await.status_code, STATUS_OK);

    let mut get_listing = Vec::new();
    encode_bytes_field(1, &uuid_msg(&listing_uuid), &mut get_listing);
    assert_eq!(q_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.GetRequest", get_listing.clone()).await.status_code, STATUS_OK);

    let mut buy_skin = get_listing;
    encode_varint_field(2, 1, &mut buy_skin);
    encode_string_field(3, &buyer_fees_ver, &mut buy_skin);
    let buy_skin_rsp = q_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", buy_skin).await;
    assert_eq!(buy_skin_rsp.status_code, STATUS_PRECONDITION_FAILED);
    assert!(has_notice(&drain_notices(&mut buyer_rx), "eve_public.cosmetic.market.transaction.api.TransactionFailedNotice"));
}

#[tokio::test]
async fn test_quasar_ai_chat_campaign_triggers_and_faction_admin_live() {
    // parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut gm = boot_gm(&ctx).await;
    let (mut gm_pilot, _gm_cid) = create_live_pilot(&ctx, &mut gm, 93_001, "fw_gm_pilot", "FW GM Pilot", true).await;
    let (mut norm_pilot, _norm_cid) = create_live_pilot(&ctx, &mut gm, 93_002, "fw_norm_pilot", "FW Norm Pilot", false).await;

    // 1. AI Assistant Chat QueryRequest
    let mut chat_req = Vec::new();
    encode_string_field(1, "Where am I located?", &mut chat_req);
    let chat_rsp = q_rpc(&ctx, &mut norm_pilot, "eve_public.ai.assistant.chat.api.QueryRequest", chat_req).await;
    assert_eq!(chat_rsp.status_code, STATUS_OK);
    let chat_wm = decode_wire(&chat_rsp);
    assert!(!wire_str(&chat_wm, 1).is_empty());

    // 2. Campaign Trigger EventMatch Definitions GetAllRequest (SDE-backed)
    let em_rsp = q_rpc(&ctx, &mut norm_pilot, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new()).await;
    assert_eq!(em_rsp.status_code, STATUS_OK);
    let expected_triggers = ctx.sde.load_trigger_types().expect("SDE trigger types").len();
    let em_wm = decode_wire(&em_rsp);
    let em_count = em_wm.fields.iter().filter(|f| f.field_number == 1).count();
    assert_eq!(em_count, expected_triggers);

    // 3. Faction Activity Admin: IncrementContributionScoreRequest & SetContributionScoreRequest + Notices + 403
    let fw_sys = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.keys().next().copied())
        .expect("FW warzone system");
    slash(&ctx, &mut gm_pilot, &format!("/tr {fw_sys}")).await;
    gm_pilot.solar_system_id = Some(fw_sys);
    let mut rx = subscribe(&ctx, &gm_pilot);

    let mut inc_req = Vec::new();
    encode_bytes_field(1, &seq_msg(fw_sys as u64), &mut inc_req);
    encode_varint_field(3, 250, &mut inc_req);
    assert_eq!(
        q_rpc(&ctx, &mut norm_pilot, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone())
            .await
            .status_code,
        STATUS_FORBIDDEN
    );
    let inc_rsp = q_rpc(&ctx, &mut gm_pilot, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req).await;
    assert_eq!(inc_rsp.status_code, STATUS_OK);
    let fw_notices = drain_notices(&mut rx);
    assert!(has_notice(&fw_notices, "eve_public.faction.activity.ScoreContributionNotice"));
    assert!(has_notice(&fw_notices, "eve_public.faction.activity.SolarSystemScoresNotice"));

    let mut set_req = Vec::new();
    encode_bytes_field(1, &seq_msg(fw_sys as u64), &mut set_req);
    encode_varint_field(3, 500, &mut set_req);
    assert_eq!(
        q_rpc(&ctx, &mut gm_pilot, "eve_public.faction.activity.admin.SetContributionScoreRequest", set_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert!(has_notice(&drain_notices(&mut rx), "eve_public.faction.activity.SolarSystemScoresNotice"));
}
