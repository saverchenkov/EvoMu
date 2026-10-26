//! Remediated V24 Quasar campaign lifecycle: scheduled start, ticking track contributions,
//! definition-version broadcast (`CurrentVersionNotice`), expiry (`ExpiredNotice`) and operator
//! authoring validation, all observed through `publicGatewaySvc` requests and notices.

#![forbid(unsafe_code)]

use crate::quasar_campaign_endpoints_remediated::{
    campaign_notices, id_payload, mining_campaign, ok, quasar, string, sub, subs, subscribe, uuid_of, varint,
};
use eve_machonet::SessionState;
use eve_server::quasar_gateway::{STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED};
use eve_server::router::ServerContext;
use eve_server::services::quasar_campaign_handlers::{campaign_uuid, objective_uuid, track_uuid};
use eve_server::services::quasar_campaign_state::ObjectiveParameter;

fn session(sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess
}

#[tokio::test]
async fn test_campaign_version_ticking_and_expiry_lifecycle() {
    // parity: quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = session(78_001, 90_000_001);
    let mut rx = subscribe(&ctx, &mut sess).await;
    let extra = &ctx.quasar_gateway.progression.extra;
    let t0 = ctx.now_unix();

    // Operator authoring is validated: objectives must use an authentic contribution method.
    let (camp, mut objs, trks) = mining_campaign(9, t0, t0 + 3_600, 1_000);
    objs[0].contribution_method = "InventedMethod".into();
    assert!(extra.register_campaign(camp.clone(), objs.clone(), trks.clone()).is_err());
    assert!(extra.campaigns.read().unwrap().is_empty(), "rejected definition must not be stored");
    objs[0].contribution_method = "MineAsteroid".into();
    objs[0].parameters = vec![ObjectiveParameter { key: "faction".into(), values: vec!["500001".into()] }];
    assert!(extra.register_campaign(camp.clone(), objs.clone(), trks.clone()).is_err(), "MineAsteroid has no faction parameter");
    objs[0].parameters.clear();
    extra.register_campaign(camp.clone(), objs.clone(), trks.clone()).expect("register");
    let (oid, tid) = (objs[0].id, trks[0].id);

    // The start time has been reached: the first request starts the campaign.
    let all = ok(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await;
    assert_eq!(uuid_of(&sub(&all, 1)), campaign_uuid(9).to_vec());
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), vec!["eve_public.campaign.api.StartedNotice"]);

    // Re-registering the definition bumps its etag and broadcasts CurrentVersionNotice once.
    let mut renamed = camp.clone();
    renamed.description = "Revised operator copy".into();
    extra.register_campaign(renamed, objs.clone(), trks.clone()).expect("re-register");
    ok(&ctx, &mut sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(9))).await;
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "eve_public.campaign.api.CurrentVersionNotice");
    let ver = sub(&notes[0].1, 1);
    assert_eq!(uuid_of(&sub(&ver, 1)), campaign_uuid(9).to_vec());
    assert_eq!(varint(&sub(&ver, 2), 2), Some(2), "etag 1 -> 2");
    ok(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await;
    assert!(campaign_notices(&mut rx).is_empty(), "version notice is not repeated");

    // Ticks accrue every 300 s at 1 + 4 x committed pilots. The lead commits at t0; the wing commits
    // at t0 + 600, which first settles ticks 1-2 at one pilot (5 each) before the wing counts.
    ok(&ctx, &mut sess, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(oid))).await;
    ctx.advance_virtual_clock(600);
    let mut wing = session(78_002, 90_000_002);
    ok(&ctx, &mut wing, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(oid))).await;
    let settled = ok(&ctx, &mut sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(9))).await;
    assert_eq!(varint(&sub(&sub(&settled, 3), 3), 1), Some(10), "ticks 1-2 settled at 1 pilot: 2 x 5");
    let pred = ok(&ctx, &mut sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", id_payload(track_uuid(tid))).await;
    assert_eq!(varint(&sub(&pred, 1), 3), Some(9));
    assert_eq!(varint(&subs(&sub(&pred, 1), 2)[1], 3), Some(2), "units_count = committed pilots");
    ctx.advance_virtual_clock(300);
    let status = ok(&ctx, &mut sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(9))).await;
    let trk = sub(&status, 3);
    assert_eq!(uuid_of(&sub(&trk, 1)), track_uuid(tid).to_vec());
    assert_eq!(varint(&sub(&trk, 2), 1), Some(1), "track STATE_STARTED");
    let trk_stats = sub(&trk, 3);
    assert_eq!(varint(&trk_stats, 1), Some(19), "2 ticks x 5 then 1 tick x 9");
    let tick = sub(&trk_stats, 2);
    assert_eq!(string(&tick, 1).as_deref(), Some("fleet_upkeep"));
    assert_eq!(varint(&sub(&tick, 3), 1), Some(t0 + 1_200), "next_tick");
    let get = ok(&ctx, &mut sess, "eve_public.campaign.objective.api.GetRequest", id_payload(objective_uuid(oid))).await;
    let oattrs = sub(&get, 1);
    assert_eq!((varint(&oattrs, 6), varint(&oattrs, 11), string(&oattrs, 16).as_deref()), (Some(1), Some(1_000), Some("mine_veldspar")));
    let isk = sub(&sub(&oattrs, 14), 1);
    assert_eq!((varint(&sub(&isk, 1), 1), varint(&isk, 2)), (Some(200_000), Some(100)), "100,000 ISK (zigzag) per 100 progress");
    assert_eq!(uuid_of(&sub(&sub(&get, 2), 1)), track_uuid(tid).to_vec());
    let ostats = ok(&ctx, &mut sess, "eve_public.campaign.objective.api.GetStatusRequest", id_payload(objective_uuid(oid))).await;
    assert_eq!((varint(&sub(&ostats, 2), 2), varint(&sub(&ostats, 2), 3)), (Some(2), Some(2)), "committed / total participants");

    // Resigning lowers units_count for later ticks.
    ok(&ctx, &mut wing, "eve_public.campaign.objective.api.ResignRequest", id_payload(objective_uuid(oid))).await;
    let pred = ok(&ctx, &mut sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", id_payload(track_uuid(tid))).await;
    assert_eq!(varint(&sub(&pred, 1), 3), Some(5));

    // Expiry: ticks stop at the expiry time, ExpiredNotice is broadcast, objective/track close.
    ctx.advance_virtual_clock(3_600);
    let status = ok(&ctx, &mut sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(9))).await;
    assert_eq!(varint(&sub(&status, 1), 1), Some(3), "campaign STATE_EXPIRED");
    assert_eq!(varint(&sub(&sub(&status, 1), 2), 1), Some(t0 + 3_600), "transition_time = scheduled expiry");
    let trk = sub(&status, 3);
    assert_eq!(varint(&sub(&trk, 2), 1), Some(3), "track STATE_STOPPED");
    assert_eq!(varint(&sub(&trk, 3), 1), Some(19 + 9 * 5), "ticks until expiry: 19 then 9 x 5");
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), vec!["eve_public.campaign.api.ExpiredNotice"]);
    assert_eq!(uuid_of(&sub(&notes[0].1, 1)), campaign_uuid(9).to_vec());
    let ost = ok(&ctx, &mut sess, "eve_public.campaign.objective.api.GetStatusRequest", id_payload(objective_uuid(oid))).await;
    assert_eq!(varint(&sub(&ost, 1), 1), Some(3), "objective STATE_EXPIRED");
    let late = quasar(&ctx, &mut wing, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(oid))).await;
    assert_eq!(late.status_code, STATUS_PRECONDITION_FAILED);
    let attrs = sub(&ok(&ctx, &mut sess, "eve_public.campaign.api.GetRequest", id_payload(campaign_uuid(9))).await, 1);
    assert_eq!(varint(&sub(&attrs, 20), 1), Some(t0 + 3_600), "finish_time");
    assert!(subs(&ok(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await, 1).is_empty());
    let missing = quasar(&ctx, &mut sess, "eve_public.campaign.objective.api.GetStatsRequest", id_payload(objective_uuid(4_242))).await;
    assert_eq!(missing.status_code, STATUS_NOT_FOUND);
}
