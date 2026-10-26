//! Remediated V24 Quasar campaign endpoints, driven end to end through `publicGatewaySvc`
//! requests and real gameplay (Miner I cycles via `dogmaIM.Activate`).
//!
//! Covers `eve_public.campaign.api` (requests + Started/Completed notices),
//! `eve_public.campaign.objective.api` (requests + ContributedNotice) and
//! `eve_public.campaign.track.api` (requests + ContributedNotice). Message and field numbers follow
//! the V24 descriptors in `data/quasar_descriptors.sqlite`.

#![forbid(unsafe_code)]

use crate::mining_cycle_engine::{mine_once, pilot, rock, seed_for, JITA, MINER_I, SKILL_MINING, SKILL_MINING_FRIGATE, VELDSPAR, VENTURE};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED};
use eve_server::router::ServerContext;
use eve_server::services::quasar_campaign_handlers::{campaign_uuid, objective_uuid, track_uuid};
use eve_server::services::quasar_campaign_state::{
    CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord, ObjectiveParameter, TickingContribution, TickingEvaluation,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

pub(crate) fn field<'a>(wm: &'a WireMessage, num: u32) -> Option<&'a WireValue> {
    wm.fields.iter().find(|f| f.field_number == num).map(|f| &f.value)
}

pub(crate) fn sub(wm: &WireMessage, num: u32) -> WireMessage {
    match field(wm, num) {
        Some(WireValue::LengthDelimited(b)) => WireMessage::decode(b).expect("decode sub-message"),
        other => panic!("field {num} is not a sub-message: {other:?}"),
    }
}

pub(crate) fn subs(wm: &WireMessage, num: u32) -> Vec<WireMessage> {
    wm.fields
        .iter()
        .filter(|f| f.field_number == num)
        .map(|f| match &f.value {
            WireValue::LengthDelimited(b) => WireMessage::decode(b).expect("decode repeated"),
            other => panic!("field {num} is not a sub-message: {other:?}"),
        })
        .collect()
}

pub(crate) fn varint(wm: &WireMessage, num: u32) -> Option<u64> {
    match field(wm, num)? {
        WireValue::Varint(v) => Some(*v),
        _ => None,
    }
}

pub(crate) fn string(wm: &WireMessage, num: u32) -> Option<String> {
    match field(wm, num)? {
        WireValue::LengthDelimited(b) => String::from_utf8(b.clone()).ok(),
        _ => None,
    }
}

/// uuid bytes of an `Identifier { bytes uuid = 1 }` sub-message.
pub(crate) fn uuid_of(wm: &WireMessage) -> Vec<u8> {
    match field(wm, 1) {
        Some(WireValue::LengthDelimited(b)) => b.clone(),
        other => panic!("Identifier.uuid missing: {other:?}"),
    }
}

pub(crate) fn id_payload(uuid: [u8; 16]) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_bytes_field(1, &uuid, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(1, &inner, &mut out);
    out
}

pub(crate) fn namespace_payload(ns: &str) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_string_field(1, ns, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(1, &inner, &mut out);
    out
}

pub(crate) async fn quasar(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: type_name.as_bytes().to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-campaign-tests".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(entries) = &rep else { panic!("SendRequest must return KeyVal, got {rep:?}") };
    let Some(PyRep::Bytes(b)) = entries.get("response_proto") else { panic!("response_proto missing") };
    EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse")
}

/// Asserts `STATUS_OK` and the response type, returning the decoded payload.
pub(crate) async fn ok(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> WireMessage {
    let rsp = quasar(ctx, sess, type_name, value).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{type_name}: {}", rsp.status_message);
    let payload = rsp.payload.expect("payload");
    assert_eq!(payload.normalized_type_name(), type_name.replace("Request", "Response"));
    WireMessage::decode(&payload.value).expect("decode response")
}

pub(crate) async fn subscribe(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("take_stream_receiver")
}

/// Drains the stream, keeping only `eve_public.campaign.*` notices as (type, decoded payload).
pub(crate) fn campaign_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, WireMessage)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        let p = n.payload.expect("notice payload");
        let ty = p.normalized_type_name().to_string();
        if ty.starts_with("eve_public.campaign.") {
            out.push((ty, WireMessage::decode(&p.value).expect("decode notice")));
        }
    }
    out
}

/// Operator-authored campaign: one `MineAsteroid` objective and one participant track with a
/// ticking contribution.
pub(crate) fn mining_campaign(cid: u64, start: u64, expiry: u64, target: i64) -> (CampaignRecord, Vec<CampaignObjectiveRecord>, Vec<CampaignTrackRecord>) {
    let camp = CampaignRecord {
        id: cid,
        name: format!("evomu_ore_drive_{cid}"),
        description: "Operator-authored mining drive".into(),
        namespace: "evomu".into(),
        scheduled_start_unix: start,
        scheduled_expiry_unix: expiry,
        ..Default::default()
    };
    let obj = CampaignObjectiveRecord {
        id: cid * 100 + 1,
        campaign_id: cid,
        name: "mine_veldspar".into(),
        target_progress: target,
        contribution_method: "MineAsteroid".into(),
        parameters: vec![
            ObjectiveParameter { key: "ore_type".into(), values: vec![VELDSPAR.to_string()] },
            ObjectiveParameter { key: "solar_system".into(), values: vec![JITA.to_string()] },
        ],
        isk_reward_per_interval: 100_000,
        isk_reward_progress_interval: 100,
        ..Default::default()
    };
    let trk = CampaignTrackRecord {
        id: cid * 1000 + 1,
        campaign_id: cid,
        key: "ore_delivered".into(),
        target_progress_value: 100_000,
        isk_reward_per_interval: 50_000,
        isk_reward_progress_interval: 200,
        ticking: vec![TickingContribution {
            key: "fleet_upkeep".into(),
            interval_secs: 300,
            evaluations: vec![
                TickingEvaluation { key: "base".into(), constant: Some(1), per_unit_value: None },
                TickingEvaluation { key: "per_pilot".into(), constant: None, per_unit_value: Some(4) },
            ],
            last_tick_unix: 0,
        }],
        ..Default::default()
    };
    (camp, vec![obj], vec![trk])
}

#[tokio::test]
async fn test_campaign_objective_track_requests_and_notices_from_mining() {
    // parity: quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/campaign/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/objective/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/campaign/track/api/notices_pb2.pyj",
    ]);
    let laser = 800_700_001;
    let mut p = pilot(VENTURE, &[(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5)], &[(laser, MINER_I, InventoryFlag::HiSlot0)]).await;
    let char_id = p.sess.character_id.unwrap() as u32;
    let (ctx, sess) = (&p.ctx, &mut p.sess);
    let t0 = ctx.now_unix();
    let (camp, objs, trks) = mining_campaign(7, t0 + 600, 0, 500);
    let (oid, tid) = (objs[0].id, trks[0].id);
    let extra = &ctx.quasar_gateway.progression.extra;
    extra.register_campaign(camp, objs, trks).expect("register campaign");
    let mut rx = subscribe(ctx, sess).await;

    // Before the scheduled start: not listed, only "upcoming" for its namespace; commit is refused.
    assert!(subs(&ok(ctx, sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await, 1).is_empty());
    let ns = ok(ctx, sess, "eve_public.campaign.api.GetAllForNamespaceRequest", namespace_payload("evomu")).await;
    let upcoming = subs(&ns, 2);
    assert_eq!(upcoming.len(), 1);
    assert_eq!(string(&upcoming[0], 1).as_deref(), Some("evomu_ore_drive_7"));
    assert_eq!(varint(&sub(&upcoming[0], 3), 1), Some(t0 + 600));
    let early = quasar(ctx, sess, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(oid))).await;
    assert_eq!(early.status_code, STATUS_PRECONDITION_FAILED);
    assert!(campaign_notices(&mut rx).is_empty());

    // Clock passes the start: the next campaign request starts it and broadcasts StartedNotice.
    ctx.advance_virtual_clock(600);
    let all = ok(ctx, sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await;
    assert_eq!(subs(&all, 1).iter().map(uuid_of).collect::<Vec<_>>(), vec![campaign_uuid(7).to_vec()]);
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "eve_public.campaign.api.StartedNotice");
    assert_eq!(uuid_of(&sub(&notes[0].1, 1)), campaign_uuid(7).to_vec());

    // GetRequest: Attributes / Stats / Track / objective identifiers per the V24 schema.
    let get = ok(ctx, sess, "eve_public.campaign.api.GetRequest", id_payload(campaign_uuid(7))).await;
    let attrs = sub(&get, 1);
    assert_eq!(varint(&attrs, 14), Some(1), "total_objectives");
    assert_eq!(varint(&attrs, 16), Some(1), "state = STATE_ACTIVE");
    assert_eq!(varint(&sub(&attrs, 17), 1), Some(t0 + 600), "scheduled_start_time");
    assert_eq!(varint(&attrs, 19), Some(1), "not_finished");
    assert_eq!(varint(&attrs, 21), Some(500), "target_progress");
    assert_eq!(string(&attrs, 23).as_deref(), Some("evomu_ore_drive_7"));
    assert_eq!(string(&sub(&attrs, 24), 1).as_deref(), Some("evomu"));
    let avail = subs(&get, 2);
    assert_eq!(uuid_of(&sub(&avail[0], 1)), objective_uuid(oid).to_vec());
    assert_eq!(string(&sub(&sub(&sub(&avail[0], 2), 13), 1), 1).as_deref(), Some("MineAsteroid"));
    assert_eq!(varint(&sub(&get, 4), 1), Some(0), "current_campaign_progress");
    let track = sub(&get, 5);
    assert_eq!(uuid_of(&sub(&track, 1)), track_uuid(tid).to_vec());
    assert_eq!(string(&sub(&track, 2), 1).as_deref(), Some("ore_delivered"));
    assert_eq!(varint(&sub(&track, 2), 3), Some(100_000));
    assert_eq!(uuid_of(&sub(&get, 6)), objective_uuid(oid).to_vec());
    assert_eq!(varint(&ok(ctx, sess, "eve_public.campaign.api.GetLimitsRequest", Vec::new()).await, 1), Some(10));

    // Unauthenticated / malformed / unknown requests.
    let mut anon = SessionState::new(77_001);
    anon.user_id = Some(77_001);
    let forbidden = quasar(ctx, &mut anon, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(oid))).await;
    assert_eq!(forbidden.status_code, STATUS_FORBIDDEN);
    assert_eq!(quasar(ctx, sess, "eve_public.campaign.api.GetRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(quasar(ctx, sess, "eve_public.campaign.api.GetRequest", id_payload(campaign_uuid(99))).await.status_code, STATUS_NOT_FOUND);
    let no_trk = quasar(ctx, sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", id_payload(track_uuid(99))).await;
    assert_eq!(no_trk.status_code, STATUS_NOT_FOUND);

    // Commit / participant status / resign / re-commit.
    let obj_req = id_payload(objective_uuid(oid));
    let ps = ok(ctx, sess, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&ps, 3), Some(1), "no_commitment before CommitRequest");
    ok(ctx, sess, "eve_public.campaign.objective.api.CommitRequest", obj_req.clone()).await;
    let ps = ok(ctx, sess, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&ps, 1), Some(1), "COMMITMENT_STATE_COMMITTED");
    assert_eq!(varint(&sub(&sub(&ps, 2), 2), 2), Some(1), "no_participant_track before contributing");
    ok(ctx, sess, "eve_public.campaign.objective.api.ResignRequest", obj_req.clone()).await;
    let ps = ok(ctx, sess, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&ps, 1), Some(2), "COMMITMENT_STATE_RESIGNED");
    let resign_again = quasar(ctx, sess, "eve_public.campaign.objective.api.ResignRequest", obj_req.clone()).await;
    assert_eq!(resign_again.status_code, STATUS_PRECONDITION_FAILED);
    ok(ctx, sess, "eve_public.campaign.objective.api.CommitRequest", obj_req.clone()).await;

    // Non-matching objectives in a second campaign: a `MineIce` objective and a `MineAsteroid`
    // objective constrained to Scordite (1228). Mining Veldspar must credit neither.
    let (mut camp8, mut objs8, _) = mining_campaign(8, ctx.now_unix(), 0, 500);
    camp8.namespace = "evomu_other".into();
    objs8[0].contribution_method = "MineIce".into();
    let mut scordite = objs8[0].clone();
    scordite.id = 802;
    scordite.contribution_method = "MineAsteroid".into();
    scordite.parameters = vec![ObjectiveParameter { key: "ore_type".into(), values: vec!["1228".into()] }];
    objs8.push(scordite);
    let ice_req = id_payload(objective_uuid(801));
    extra.register_campaign(camp8, objs8, Vec::new()).expect("register campaign 8");
    ok(ctx, sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(8))).await;
    assert_eq!(campaign_notices(&mut rx).iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), vec!["eve_public.campaign.api.StartedNotice"]);
    for o in [801, 802] {
        ok(ctx, sess, "eve_public.campaign.objective.api.CommitRequest", id_payload(objective_uuid(o))).await;
    }

    // Ticking prediction = constant 1 + per_unit_value 4 x 1 committed pilot.
    let pred = ok(ctx, sess, "eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", id_payload(track_uuid(tid))).await;
    let contrib = sub(&pred, 1);
    assert_eq!(string(&contrib, 1).as_deref(), Some("fleet_upkeep"));
    let evals = subs(&contrib, 2);
    assert_eq!((string(&evals[0], 1).as_deref(), varint(&evals[0], 2)), (Some("base"), Some(2)), "constant 1 (zigzag)");
    assert_eq!((string(&evals[1], 1).as_deref(), varint(&evals[1], 3)), (Some("per_pilot"), Some(1)), "units_count");
    assert_eq!(varint(&contrib, 3), Some(5));

    // Real gameplay: one Miner I cycle credits the committed MineAsteroid objective and its track.
    let rock_id = 750_700_001;
    rock(&p, rock_id, VELDSPAR, 50_000, 3_000.0).await;
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false, false], 0.01)));
    let first = mine_once(&p.ctx, &mut p.sess, laser, rock_id).await.expect("first cycle").units_delivered as u64;
    assert!(first > 0 && first < 500, "one cycle delivers part of the 500 target: {first}");
    let (ctx, sess) = (&p.ctx, &mut p.sess);
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), vec![
        "eve_public.campaign.objective.api.ContributedNotice",
        "eve_public.campaign.track.api.ContributedNotice",
    ]);
    assert_eq!((uuid_of(&sub(&notes[0].1, 1)), varint(&notes[0].1, 2)), (objective_uuid(oid).to_vec(), Some(first)));
    assert_eq!((uuid_of(&sub(&notes[1].1, 1)), varint(&notes[1].1, 2)), (track_uuid(tid).to_vec(), Some(first)));

    let other = ok(ctx, sess, "eve_public.campaign.objective.api.GetStatsRequest", [ice_req.clone(), id_payload(objective_uuid(802))].concat()).await;
    for e in subs(&other, 1) {
        assert_eq!(varint(&sub(&e, 2), 1), Some(0), "MineIce / Scordite-only objectives are not credited by Veldspar");
    }

    // Objective status / stats / participant stats reflect the delivered ore.
    let st = ok(ctx, sess, "eve_public.campaign.objective.api.GetStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&sub(&st, 1), 1), Some(1));
    assert_eq!((varint(&sub(&st, 2), 1), varint(&sub(&st, 2), 2), varint(&sub(&st, 2), 4)), (Some(first), Some(1), Some(1)));
    assert_eq!(varint(&sub(&sub(&st, 3), 3), 1), Some(first), "track current_progress");
    let stats = ok(ctx, sess, "eve_public.campaign.objective.api.GetStatsRequest", obj_req.clone()).await;
    assert_eq!(varint(&sub(&sub(&stats, 1), 2), 1), Some(first));
    let pstats = ok(ctx, sess, "eve_public.campaign.objective.api.GetParticipantStatsRequest", obj_req.clone()).await;
    let pentry = sub(&pstats, 1);
    assert_eq!(uuid_of(&sub(&pentry, 1)), objective_uuid(oid).to_vec());
    assert_eq!((varint(&sub(&pentry, 2), 1), varint(&sub(&pentry, 2), 2)), (Some(1), Some(first)));
    let cps = ok(ctx, sess, "eve_public.campaign.api.GetParticipantStatsRequest", id_payload(campaign_uuid(7))).await;
    assert_eq!(varint(&sub(&sub(&cps, 1), 2), 2), Some(first));
    let cstats = ok(ctx, sess, "eve_public.campaign.api.GetStatsRequest", id_payload(campaign_uuid(7))).await;
    assert_eq!((varint(&sub(&cstats, 1), 1), varint(&sub(&cstats, 1), 2)), (Some(first), Some(1)));
    let ps = ok(ctx, sess, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&sub(&sub(&sub(&ps, 2), 2), 1), 1), Some(first), "current_participant_contribution");

    // Unredeemed rewards: objective ISK intervals of 100 progress, track intervals of 200.
    let obj_units = first / 100;
    let trk_units = first / 200;
    let unredeemed = ok(ctx, sess, "eve_public.campaign.api.GetAllUnredeemedRewardsRequest", Vec::new()).await;
    assert_eq!(varint(&sub(&unredeemed, 1), 2), Some(obj_units));
    let unredeemed_p = ok(ctx, sess, "eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest", namespace_payload("evomu")).await;
    assert_eq!(varint(&sub(&unredeemed_p, 1), 2), Some(trk_units));
    let bal = |c: &ServerContext| c.db.with_snapshot(|s| s.characters[&(char_id as i64)].balance.0);
    let before = bal(ctx);
    ok(ctx, sess, "eve_public.campaign.objective.api.RedeemRewardsRequest", obj_req.clone()).await;
    assert_eq!(bal(ctx) - before, (obj_units as i64) * 100_000 * 100, "objective intervals x 100,000 ISK (cents)");
    ok(ctx, sess, "eve_public.campaign.track.api.RedeemParticipantRewardsRequest", id_payload(track_uuid(tid))).await;
    assert_eq!(bal(ctx) - before, (obj_units as i64 * 100_000 + trk_units as i64 * 50_000) * 100);
    ctx.db.with_snapshot(|s| assert_eq!(s.wallet_journal.last().unwrap().entry_type, "CampaignReward"));
    for (ty, req) in [
        ("eve_public.campaign.objective.api.RedeemRewardsRequest", obj_req.clone()),
        ("eve_public.campaign.track.api.RedeemParticipantRewardsRequest", id_payload(track_uuid(tid))),
    ] {
        assert_eq!(quasar(ctx, sess, ty, req).await.status_code, STATUS_PRECONDITION_FAILED, "{ty} twice");
    }

    // Second cycle reaches the 500 target: objective completes, then the campaign broadcasts CompletedNotice.
    mine_once(&p.ctx, &mut p.sess, laser, rock_id).await.expect("second cycle");
    let (ctx, sess) = (&p.ctx, &mut p.sess);
    let notes = campaign_notices(&mut rx);
    assert_eq!(notes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), vec![
        "eve_public.campaign.objective.api.ContributedNotice",
        "eve_public.campaign.track.api.ContributedNotice",
        "eve_public.campaign.api.CompletedNotice",
    ]);
    assert_eq!(varint(&notes[0].1, 2), Some(500), "objective credit capped at target_progress");
    assert_eq!(uuid_of(&sub(&notes[2].1, 1)), campaign_uuid(7).to_vec());
    let status = ok(ctx, sess, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(7))).await;
    assert_eq!(varint(&sub(&status, 1), 1), Some(2), "campaign STATE_COMPLETED");
    assert_eq!(varint(&sub(&status, 2), 1), Some(500));
    assert_eq!(varint(&sub(&sub(&status, 3), 2), 1), Some(3), "track STATE_STOPPED");
    let done = quasar(ctx, sess, "eve_public.campaign.objective.api.CommitRequest", obj_req.clone()).await;
    assert_eq!(done.status_code, STATUS_PRECONDITION_FAILED);

    // Persisted state survives a SQLite round trip and is served by the reopened server.
    let reopened = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(ctx, "campaign_endpoints");
    crate::common::pin_clock(&reopened);
    let mut sess2 = sess.clone();
    let ps = ok(&reopened, &mut sess2, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&ps, 1), Some(1));
    assert_eq!(varint(&sub(&sub(&sub(&ps, 2), 2), 1), 1), Some(500));
    let st = ok(&reopened, &mut sess2, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(7))).await;
    assert_eq!(varint(&sub(&st, 1), 1), Some(2));
}
