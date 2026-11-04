//! behavioral verification V24 Quasar campaign endpoints, driven end to end through `publicGatewaySvc`
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
    assert_eq!(bal(ctx) - before, (obj_units as i64 * 100_000 + trk_units as i64 * 50_000) * 100, "back-to-back redeems credit once");

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

    // Racing redeems of the intervals earned by the second cycle (500/100 - first/100) pay exactly once.
    let pending = (5 - first / 100) as i64;
    let before_race = bal(ctx);
    let extra = &ctx.quasar_gateway.progression.extra;
    let wins = std::thread::scope(|sc| {
        let racers: Vec<_> = (0..2).map(|_| sc.spawn(|| extra.redeem_objective_rewards(char_id, oid).is_ok())).collect();
        racers.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count()
    });
    assert_eq!(wins, 1, "exactly one concurrent redeem succeeds");
    assert_eq!(bal(ctx) - before_race, pending * 100_000 * 100, "concurrent redeems credit the wallet once");

    // Persisted state survives a SQLite round trip and is served by the reopened server.
    let reopened = crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(ctx, "campaign_endpoints");
    crate::common::pin_clock(&reopened);
    let mut sess2 = sess.clone();
    let ps = ok(&reopened, &mut sess2, "eve_public.campaign.objective.api.GetParticipantStatusRequest", obj_req.clone()).await;
    assert_eq!(varint(&ps, 1), Some(1));
    assert_eq!(varint(&sub(&sub(&sub(&ps, 2), 2), 1), 1), Some(500));
    let st = ok(&reopened, &mut sess2, "eve_public.campaign.api.GetStatusRequest", id_payload(campaign_uuid(7))).await;
    assert_eq!(varint(&sub(&st, 1), 1), Some(2));
}

pub(crate) mod campaign_version_ticking_expiry_lifecycle {
    // behavioral verification V24 Quasar campaign lifecycle: scheduled start, ticking track contributions,
    // definition-version broadcast (`CurrentVersionNotice`), expiry (`ExpiredNotice`) and operator
    // authoring validation, all observed through `publicGatewaySvc` requests and notices.


    use crate::quasar_campaigns_and_goals::{
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
}

pub(crate) mod career_goals_milestones_campaign_definitions {
    // Integration tests validating behavioral verification of Quasar Career, Campaign Definitions,
    // and Achievements authentic data-driven semantics.

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
    async fn test_authentic_career_goals_and_milestones_progression() {
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
    async fn test_authentic_campaign_definitions_catalog() {
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
    async fn test_authentic_achievements_and_cdn_metadata() {
        // DATA GAP: Achievement showcase scores, category bucketing, and milestone reward amounts are server-authored CDN Checkpoint data in V24 and not present in res:/staticdata/achievements.static.
        // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj",
        ]);

        let ctx = std::sync::Arc::new(ServerContext::new());
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

        // Verify multi-condition client achievement evaluation via monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
        let empty_kw = HashMap::new();
        let zoom_in_args = [
            PyRep::List(vec![]),
            PyRep::Dict(vec![(PyRep::String("client_mouseZoom_in".into()), PyRep::Integer(3))]),
        ];
        ctx.dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "UpdateClientAchievmentsAndCounters", &zoom_in_args, &empty_kw, None)
            .await
            .expect("UpdateClientAchievmentsAndCounters zoomIn");
        let ach60_mid = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_012)
            .into_iter().find(|a| a.def.id == 60).unwrap();
        assert_eq!(ach60_mid.progress, 1, "Zooming in multiple times only satisfies condition 0 (1/2)");
        assert!(!ach60_mid.completed, "Achievement 60 must not complete until condition 1 (zoomOut 124) also fires");

        let zoom_out_args = [
            PyRep::List(vec![]),
            PyRep::Dict(vec![(PyRep::String("client_mouseZoom_out".into()), PyRep::Integer(1))]),
        ];
        ctx.dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "UpdateClientAchievmentsAndCounters", &zoom_out_args, &empty_kw, None)
            .await
            .expect("UpdateClientAchievmentsAndCounters zoomOut");
        let ach60_done = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_012)
            .into_iter().find(|a| a.def.id == 60).unwrap();
        assert_eq!(ach60_done.progress, 2, "Zooming out satisfies condition 1 (2/2)");
        assert!(ach60_done.completed, "Achievement 60 completes once both conditions are met");

        // Negative security test: client RPC must NOT complete or advance non-client server achievements (2, 12, 59) or unbounded/unknown counters
        let oversized_key = "x".repeat(256);
        let spoof_server_args = [
            PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(12), PyRep::Integer(59)]),
            PyRep::Dict(vec![
                (PyRep::String("refineOreCount".into()), PyRep::Integer(99)),
                (PyRep::String("arbitrary_unknown_counter".into()), PyRep::Integer(42)),
                (PyRep::String("999999".into()), PyRep::Integer(7)),
                (PyRep::String(oversized_key.clone()), PyRep::Integer(1)),
            ]),
        ];
        ctx.dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "UpdateClientAchievmentsAndCounters", &spoof_server_args, &empty_kw, None)
            .await
            .expect("UpdateClientAchievmentsAndCounters spoof server achievements");
        let post_spoof = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(90_000_012);
        for server_id in [2u32, 12, 59] {
            let st = post_spoof.iter().find(|a| a.def.id == server_id).expect("server achievement");
            assert!(!st.def.is_client_achievement, "SDE achievement {server_id} must have is_client_achievement == false");
            assert_eq!(st.progress, 0, "Client RPC must not advance server achievement {server_id}");
            assert!(!st.completed, "Client RPC must not complete server achievement {server_id}");
        }
        let tracker_state = ctx.dispatch_rpc(&mut sess, "monolithTaskTrackerMgr", None, "GetCompletedAchievementsAndClientEventCount", &[], &empty_kw, None)
            .await
            .expect("GetCompletedAchievementsAndClientEventCount after spoof");
        let completed_dict = tracker_state.dict_get("completedDict").expect("completedDict");
        if let PyRep::Dict(entries) = completed_dict {
            for sid in [2i64, 12, 59] {
                assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(sid)), "Server achievement {sid} must not appear in completedDict");
            }
        }
        let ev_dict = tracker_state.dict_get("eventDict").unwrap();
        for bad_key in ["refineOreCount", "arbitrary_unknown_counter", "999999", oversized_key.as_str()] {
            assert_eq!(ev_dict.dict_get(bad_key), None, "Disallowed counter key {bad_key} must be rejected");
        }
        assert_eq!(ev_dict.dict_get("client_mouseZoom_in"), Some(&PyRep::Integer(3)), "Valid SDE client key must persist");

        // 1. CDN GetLatestRequest returns valid checkpoint metadata with patch 60 (enabled SDE count) and matching local HTTP payload CRC32
        let cdn_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new(), b"cdn-01").await;
        assert_eq!(cdn_rsp.status_code, STATUS_OK);
        let cdn_payload = cdn_rsp.payload.expect("cdn payload");
        assert_eq!(cdn_payload.normalized_type_name(), "eve_public.achievement.definition.api.cdn.GetLatestResponse");

        let wm_cdn = WireMessage::decode(&cdn_payload.value).expect("decode GetLatestResponse");
        let cp_field = wm_cdn.fields.iter().find(|f| f.field_number == 1).expect("checkpoint field");
        let cp_wm = WireMessage::decode(cp_field.value.as_bytes().expect("cp bytes")).expect("decode Checkpoint");

        let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("url");
        let cdn_path = url_str.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
        assert!(cdn_path.starts_with("/cdn/achievements/v24.1.60/"), "URL must reference 60 enabled achievement definitions: {url_str}");

        let spec_field = cp_wm.fields.iter().find(|f| f.field_number == 2).expect("spec field");
        let spec_wm = WireMessage::decode(spec_field.value.as_bytes().expect("spec bytes")).expect("decode spec");
        assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(24), "Major 24");
        assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1), "Minor 1");
        assert_eq!(spec_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(60), "Patch 60 (60 enabled SDE achievements)");

        let crc_field = cp_wm.fields.iter().find(|f| f.field_number == 3).expect("crc field");
        let crc_wm = WireMessage::decode(crc_field.value.as_bytes().expect("crc bytes")).expect("decode crc");
        let crc_val = crc_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).expect("crc val");
        assert!(crc_val > 0, "CRC32 checksum must be non-zero");
        let esi = eve_server::esi::router::EsiRouter::new(std::sync::Arc::clone(&ctx));
        let cdn_http = esi.handle_request(&eve_server::esi::EsiHttpRequest::new("GET", cdn_path));
        assert_eq!(cdn_http.status_code, 200, "EsiRouter must serve /cdn/achievements JSON");
        assert_eq!(eve_server::router::cache_notifications::crc32_ieee(&cdn_http.body) as u64, crc_val, "Served JSON CRC32 must match checkpoint CRC32");

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

        // Complete achievement #2 ("Mine Ore", target 3) and #59 ("Refine Ore", target 1) via live gameplay RPCs (no admin CompleteRequest)
        let mod_id = 910_001_001_i64;
        let ast_id = 910_001_002_i64;
        let mut mp = crate::mining_cycle_engine::pilot(
            crate::mining_cycle_engine::VENTURE,
            &[(crate::mining_cycle_engine::SKILL_MINING, 5), (crate::mining_cycle_engine::SKILL_MINING_FRIGATE, 5)],
            &[(mod_id, crate::mining_cycle_engine::MINER_I, eve_db::InventoryFlag::HiSlot0)],
        ).await;
        crate::mining_cycle_engine::rock(&mp, ast_id, crate::mining_cycle_engine::VELDSPAR, 50_000, 1_000.0).await;
        for cycle in 1..=3_u64 {
            let outcome = crate::mining_cycle_engine::mine_once(&mp.ctx, &mut mp.sess, mod_id, ast_id).await.expect("mine_once");
            assert!(outcome.units_delivered > 0, "Cycle {cycle} must harvest Veldspar");
            let a2_state = mp.ctx.quasar_gateway.progression.achievements
                .get_or_init_achievements(crate::mining_cycle_engine::CHAR_ID as u32)
                .into_iter().find(|a| a.def.id == 2).unwrap();
            assert_eq!(a2_state.progress, cycle, "Achievement 2 progress must advance on live mining cycle {cycle}");
            assert_eq!(a2_state.completed, cycle == 3, "Achievement 2 completes on 3rd live mining cycle");
        }

        // Reprocess 100 units of Veldspar via reprocessingSvc.Reprocess to complete achievement #59 via live gameplay
        let reprocess_station = 60_003_760_i64;
        let ore_item_id = 910_002_001_i64;
        mp.sess.station_id = Some(reprocess_station);
        mp.sess.ship_id = None;
        mp.ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&crate::mining_cycle_engine::CHAR_ID) {
                ch.station_id = Some(reprocess_station);
                ch.standings.insert(1_000_035, 10.0);
            }
            snap.items.insert(ore_item_id, eve_db::InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230,
                owner_id: crate::mining_cycle_engine::CHAR_ID,
                location_id: reprocess_station,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            });
        });
        mp.ctx.dispatch_rpc(
            &mut mp.sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(ore_item_id)],
            &empty_kw,
            None,
        ).await.expect("reprocessingSvc.Reprocess");

        let get_rsp2 = quasar_rpc(&mp.ctx, &mut mp.sess, "eve_public.achievement.api.GetRequest", req_ach2, b"ach-03").await;
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

        let get_rsp59 = quasar_rpc(&mp.ctx, &mut mp.sess, "eve_public.achievement.api.GetRequest", make_ident_req(59), b"ach-59").await;
        assert_eq!(get_rsp59.status_code, STATUS_OK);
        let wm_get59 = WireMessage::decode(&get_rsp59.payload.unwrap().value).expect("decode GetResponse 59");
        assert!(wm_get59.fields.iter().any(|f| f.field_number == 1), "Achievement 59 completed_date must be present after live reprocessing");
        assert_eq!(wm_get59.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(1), "Achievement 59 progress must equal 1");

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
}

pub(crate) mod pve_quasar_daily_goals_sde {
    // behavioral verification integration tests for V24 SDE-backed Quasar Daily Goals & AIR Daily Operations
    // (`feat:pve.daily-goals`, `feat:quasar.dailygoal`,
    // `quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj`,
    // `quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj`).


    use crate::common;
    use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, DynamicValue, EvePublicNotice, EvePublicRequest, EvePublicResponse,
        GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireField, WireMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use tokio::sync::mpsc::Receiver;

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("Expected KeyVal with response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        value: Vec<u8>,
    ) -> EvePublicResponse {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: format!("dg-remed-{n:06}").into_bytes(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"dg-remediation-01".to_vec(),
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
                &HashMap::new(),
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &uuid, &mut id_bytes);
        let mut out = Vec::new();
        encode_bytes_field(1, &id_bytes, &mut out);
        out
    }

    fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            if let Some(p) = n.payload {
                out.push(p);
            }
        }
        out
    }

    #[tokio::test]
    async fn test_pve_and_quasar_daily_goals_sde_seeding_and_gameplay_progression() {
        // parity: feat:pve.daily-goals, feat:quasar.dailygoal, quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj
        eve_testkit::attest(&[
            "feat:pve.daily-goals",
            "feat:quasar.dailygoal",
            "quasar:eveProto/generated/eve_public/dailygoal/api/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/dailygoal/api/notices_pb2.pyj",
        ]);

        let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let char_id = 90_000_001u32;
        let mut sess = SessionState::new(11_101);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id as i64);
        sess.corporation_id = Some(98_000_001);
        sess.solar_system_id = Some(30_000_142);
        sess.station_id = Some(60_003_760);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

        // 1. Out-of-the-box ServerContext seeds all 5 V24 AIR Daily Operations from contributionmethods.fsdbinary
        let methods = ctx
            .sde
            .contribution_methods()
            .expect("SDE contribution_methods must load from contributionmethods.fsdbinary");
        let mut expected_titles = Vec::new();
        for key in [
            "KillNPC",
            "MineOre",
            "ScanSignature",
            "SpaceJump",
            "ManufactureItem",
        ] {
            let entry = methods.get(key).unwrap_or_else(|| {
                panic!(
                    "SDE contribution_methods missing key {key}; available keys: {:?}",
                    methods.keys().collect::<Vec<_>>()
                )
            });
            let title_id = entry
                .get("titleID")
                .and_then(|v| v.as_u64())
                .filter(|&v| v > 0)
                .unwrap_or_else(|| panic!("SDE contribution_methods[{key}] missing positive titleID")) as u32;
            expected_titles.push(title_id);
        }

        let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
        assert_eq!(all_rsp.status_code, STATUS_OK);
        let wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllCurrentResponse");
        let goal_fields: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
        assert_eq!(goal_fields.len(), 6, "ServerContext must seed 5 SDE daily goals + 1 daily bonus goal without manual setup");

        let mut uuids = Vec::new();
        for f in &goal_fields {
            let id_dyn = reg
                .decode_dynamic("eve_public.dailygoal.Identifier", f.value.as_bytes().unwrap())
                .expect("decode Identifier");
            let Some(DynamicValue::Bytes(b)) = id_dyn.get(1) else {
                panic!("missing uuid bytes");
            };
            let mut arr = [0u8; 16];
            arr.copy_from_slice(b);
            uuids.push(arr);
        }

        // Verify GetRequest decodes real SDE contributionmethods.fsdbinary titleIDs & Paragon assigner 1_000_419
        let mut seen_title_ids = Vec::new();
        for uuid in &uuids {
            let get_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetRequest", make_uuid_request(*uuid)).await;
            assert_eq!(get_rsp.status_code, STATUS_OK);
            let get_dyn = reg
                .decode_dynamic("eve_public.dailygoal.api.GetResponse", &get_rsp.payload.unwrap().value)
                .expect("decode GetResponse");
            let Some(DynamicValue::Message(attrs)) = get_dyn.get(1) else {
                panic!("missing goal Attributes");
            };
            let Some(DynamicValue::Message(name_msg)) = attrs.get(1) else {
                panic!("missing name_message");
            };
            let Some(DynamicValue::Uint32(title_id)) = name_msg.get(1) else {
                panic!("missing title_id");
            };
            seen_title_ids.push(*title_id);
            let Some(DynamicValue::Message(assigner_msg)) = attrs.get(4) else {
                panic!("missing assigner");
            };
            assert_eq!(assigner_msg.get(1), Some(&DynamicValue::Uint32(1_000_419)));
            assert!(matches!(get_dyn.get(4), Some(DynamicValue::Repeated(v)) if v.len() == 2));
        }
        for expected_title in expected_titles {
            assert!(
                seen_title_ids.contains(&expected_title),
                "Missing SDE contributionmethod titleID {expected_title} in {seen_title_ids:?}"
            );
        }

        // Subscribe to publicGatewaySvc for ProgressedNotice, CompletedNotice, and RedeemedNotice
        ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
            .await
            .expect("Subscribe");
        let mut rx = ctx
            .quasar_gateway
            .take_stream_receiver(sess.session_id)
            .expect("take_stream_receiver");
        let _ = drain_notices(&mut rx);

        let (initial_isk, initial_sp) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&(char_id as i64)];
            (ch.balance.0, ch.free_skill_points)
        });

        // 2. Trigger real gameplay pathways across all 5 daily goal categories:
        // (a) ScanSignature (target = 5)
        for sig_id in 800_001..=800_005 {
            ctx.mark_signature_scanned(char_id as i64, 30_000_142, sig_id);
        }
        let notices_scan = drain_notices(&mut rx);
        assert!(
            notices_scan
                .iter()
                .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.ProgressedNotice"),
            "ScanSignature must emit ProgressedNotice"
        );
        assert!(
            notices_scan
                .iter()
                .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.CompletedNotice"),
            "Completing ScanSignature target must emit CompletedNotice"
        );

        // (b) MineAsteroid (target = 2,000 units)
        eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(
            &ctx,
            char_id as i64,
            2_000,
            30_000_142,
            1230,
        );
        let _ = drain_notices(&mut rx);

        // (c) ManufactureItem (target = 1 job delivered via industryManager.CompleteJob)
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                990_001,
                InventoryItemRecord {
                    item_id: 990_001,
                    type_id: 691,
                    owner_id: char_id as i64,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Locked,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.industry_jobs.insert(
                77_001,
                IndustryJobRecord {
                    job_id: 77_001,
                    installer_id: char_id as i64,
                    facility_id: 60_003_760,
                    blueprint_item_id: 990_001,
                    blueprint_type_id: 691,
                    product_type_id: 587,
                    activity_id: 1,
                    runs: 1,
                    duration_seconds: 60,
                    start_unix: now.saturating_sub(120),
                    status: "Active".to_string(),
                },
            );
        });
        ctx.dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(77_001)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CompleteJob manufacturing");
        let _ = drain_notices(&mut rx);

        // (d) DestroyNPC (1 NPC kill -> partial progress 1/25)
        let npc_ball = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
            &ctx,
            30_000_142,
            10035,
            "BeltRat",
            1,
            Default::default(),
        )
        .await;
        eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
            &ctx,
            30_000_142,
            npc_ball,
            char_id as i64,
        )
        .await;
        let _ = drain_notices(&mut rx);

        // 3. Verify 3 completed goals (ScanSignature, MineAsteroid, ManufactureItem) and 1 incomplete goal (DestroyNPC)
        let comp_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(comp_rsp.status_code, STATUS_OK);
        let comp_wm = WireMessage::decode(&comp_rsp.payload.unwrap().value).expect("decode completed");
        assert_eq!(comp_wm.fields.iter().filter(|f| f.field_number == 1).count(), 3);

        let goals_now = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
        let combat_goal = goals_now.iter().find(|g| g.template.category == 1).unwrap();
        let scan_goal = goals_now.iter().find(|g| g.template.category == 3).unwrap();
        assert_eq!(combat_goal.current_progress, 1);
        assert!(!combat_goal.completed);
        assert!(scan_goal.completed && !scan_goal.claimed);

        // Negative check: redeeming incomplete DestroyNPC goal fails with 403 Forbidden
        let bad_redeem = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.RedeemRequest",
            make_uuid_request(combat_goal.uuid),
        )
        .await;
        assert_eq!(bad_redeem.status_code, STATUS_FORBIDDEN);

        // Positive RedeemRequest on ScanSignature -> credits ISK & SP and emits RedeemedNotice
        let redeem_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.RedeemRequest",
            make_uuid_request(scan_goal.uuid),
        )
        .await;
        assert_eq!(redeem_rsp.status_code, STATUS_OK);
        let notices_redeem = drain_notices(&mut rx);
        assert!(
            notices_redeem
                .iter()
                .any(|n| n.normalized_type_name() == "eve_public.dailygoal.api.RedeemedNotice"),
            "RedeemRequest must emit RedeemedNotice"
        );

        // Negative check: double-redeeming already-claimed goal fails with 403 Forbidden
        let dup_redeem = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.RedeemRequest",
            make_uuid_request(scan_goal.uuid),
        )
        .await;
        assert_eq!(dup_redeem.status_code, STATUS_FORBIDDEN);

        // RedeemAllRequest claims the remaining 2 completed goals (MineAsteroid + ManufactureItem)
        let redeem_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemAllRequest", Vec::new()).await;
        assert_eq!(redeem_all_rsp.status_code, STATUS_OK);

        let (after_isk, after_sp) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&(char_id as i64)];
            (ch.balance.0, ch.free_skill_points)
        });
        assert_eq!(after_isk, initial_isk + IskCents::from_isk(1_500_000).0);
        assert!((after_sp - (initial_sp + 15_000.0)).abs() < 1e-6);

        // 4. Advance virtual clock by 86,400s (next 11:00 UTC downtime bucket) -> daily goals reset cleanly
        ctx.advance_virtual_clock(86_400);
        let next_day_goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(char_id);
        assert_eq!(next_day_goals.len(), 6);
        assert!(next_day_goals
            .iter()
            .all(|g| g.current_progress == 0 && !g.completed && !g.claimed));
    }

    #[tokio::test]
    async fn test_daily_goals_reset_at_1100_utc_downtime_not_midnight() {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        // Advance to 23:59:00 UTC on the next UTC day
        let next_utc_midnight = (ctx.now_unix() / 86_400 + 1) * 86_400;
        let t_2359_utc = next_utc_midnight + 86_340;
        ctx.advance_virtual_clock(t_2359_utc - ctx.now_unix());

        let char_id = 90_000_002u32;
        let mut sess = SessionState::new(11_102);
        sess.user_id = Some(2);
        sess.character_id = Some(char_id as i64);
        sess.corporation_id = Some(98_000_001);
        sess.solar_system_id = Some(30_000_142);
        sess.station_id = Some(60_003_760);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

        let goals_init = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(char_id, ctx.now_unix());
        assert_eq!(goals_init.len(), 6);
        let scan_uuid = goals_init.iter().find(|g| g.template.category == 3).unwrap().uuid;

        // Progress & claim ScanSignature at 23:59 UTC
        ctx.quasar_gateway
            .progression
            .daily_goals
            .add_progress(char_id, &scan_uuid, 5, ctx.now_unix());
        let redeem_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.dailygoal.api.RedeemRequest",
            make_uuid_request(scan_uuid),
        )
        .await;
        assert_eq!(redeem_rsp.status_code, STATUS_OK);

        // Advance 120s to 00:01 UTC (2026-01-02 00:01:00 UTC): must NOT reset across 00:00 UTC
        ctx.advance_virtual_clock(120);
        let goals_0001 = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(char_id, ctx.now_unix());
        let scan_0001 = goals_0001.iter().find(|g| g.uuid == scan_uuid).unwrap();
        assert!(scan_0001.claimed, "daily goal must NOT reset at 00:00 UTC");
        assert_eq!(scan_0001.current_progress, 5);

        // Advance to 10:59 UTC (10h 58m = 39_480s later): still same downtime bucket, must NOT reset
        ctx.advance_virtual_clock(39_480);
        let goals_1059 = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(char_id, ctx.now_unix());
        let scan_1059 = goals_1059.iter().find(|g| g.uuid == scan_uuid).unwrap();
        assert!(scan_1059.claimed, "daily goal must NOT reset before 11:00 UTC downtime");

        // Advance 120s to 11:01 UTC: crosses 11:00 UTC downtime -> MUST reset
        ctx.advance_virtual_clock(120);
        let goals_1101 = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(char_id, ctx.now_unix());
        let scan_1101 = goals_1101.iter().find(|g| g.uuid == scan_uuid).unwrap();
        assert!(!scan_1101.claimed, "daily goal MUST reset after 11:00 UTC downtime");
        assert!(!scan_1101.completed);
        assert_eq!(scan_1101.current_progress, 0);
    }
}

pub(crate) mod extract_response_proto_build_sess {
    // Integration tests for Extended Quasar Endpoints across Career, Skills, Achievements,
    // Campaigns, and Community Tracks (Phase Q, tasks `Q-3` through `Q-8`).

    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_varint_field, DynamicValue, EvePublicEvent, EvePublicNotice,
        EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
        QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{NoticeScope, STATUS_BAD_REQUEST, STATUS_OK};
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

    fn build_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
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
        corr_id: &[u8],
    ) -> EvePublicResponse {
        let empty_kw = HashMap::new();
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr_id.to_vec(),
            payload: Some(GoogleAny {
                type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                    type_url.to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
                },
                value: payload_value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-extended-01".to_vec(),
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

    fn make_ach_req(id: u32) -> Vec<u8> {
        let (mut uuid, mut b, mut out) = ([0u8; 16], Vec::new(), Vec::new());
        uuid[12..16].copy_from_slice(&id.to_be_bytes());
        encode_bytes_field(1, &uuid, &mut b);
        encode_bytes_field(1, &b, &mut out);
        out
    }

    fn make_ach_progress(id: u32, delta: u64) -> Vec<u8> {
        let mut out = make_ach_req(id);
        encode_varint_field(2, delta, &mut out);
        out
    }

    // ============================================================================
    // 1. Career Program Extended Endpoints & Definitions
    // ============================================================================

    #[tokio::test]
    async fn quasar_career_extended_endpoints_and_definitions() {
        // parity: quasar:eveProto/generated/eve_public/career/career_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/career/goal_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/career/career_pb2.pyj",
            "quasar:eveProto/generated/eve_public/career/goal_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_001, 90_000_001, 98_000_001);

        let reg = QuasarDescriptorRegistry::open_default().expect("open default registry");
        let career_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/career/career_pb2.pyj")
            .expect("career_pb2");
        assert_eq!(career_fd.package, "eve_public.career");
        let career_enum = career_fd
            .enums
            .iter()
            .find(|e| e.name == "Path")
            .expect("Path enum");
        assert_eq!(career_enum.values.len(), 5); // UNSPECIFIED, EXPLORATION, INDUSTRIALIST, ENFORCER, SOLDIER_OF_FORTUNE

        let goal_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/career/goal_pb2.pyj")
            .expect("goal_pb2");
        assert_eq!(goal_fd.package, "eve_public.career.goal");

        // 1. Query career goals GetAllRequest
        let rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.GetAllRequest",
            Vec::new(),
            b"corr-career-01",
        )
        .await;
        assert_eq!(rsp.status_code, STATUS_OK);
        assert_eq!(rsp.status_message, "OK");
        assert_eq!(rsp.correlation_uuid, b"corr-career-01");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(
            payload.normalized_type_name(),
            "eve_public.career.goal.api.GetAllResponse"
        );

        let wm = WireMessage::decode(&payload.value).expect("decode GetAllResponse");
        let goal_entries: Vec<_> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
        assert_eq!(goal_entries.len(), 196);

        // 2. Query GetDefinitionsRequest
        let def_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.GetDefinitionsRequest",
            Vec::new(),
            b"corr-career-02",
        )
        .await;
        assert_eq!(def_rsp.status_code, STATUS_OK);
        assert_eq!(
            def_rsp.payload.unwrap().normalized_type_name(),
            "eve_public.career.goal.api.GetDefinitionsResponse"
        );

        // 3. Emit live ProgressedNotice and verify notice envelope
        let empty_kw = HashMap::new();
        ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
            .await
            .expect("Subscribe");
        let mut stream_rx = ctx
            .quasar_gateway
            .take_stream_receiver(sess.session_id)
            .expect("take_stream_receiver");

        let mut notice_data = Vec::new();
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &[0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut notice_data);
        encode_varint_field(2, 10, &mut notice_data);

        let notice_payload = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.ProgressedNotice"),
            value: notice_data,
        };
        let (published, count) = ctx
            .quasar_gateway
            .publish_notice(NoticeScope::Character(90_000_001), notice_payload, ctx.now_unix())
            .expect("publish_notice");
        assert_eq!(count, 1);
        assert_eq!(published.internal_origin, "evomu.quasar.notice_publisher");

        let received = drain_notices(&mut stream_rx);
        assert_eq!(received.len(), 1);
        assert_eq!(
            received[0].payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.career.goal.api.ProgressedNotice"
        );
    }

    // ============================================================================
    // 2. Skill Plan Extended Endpoints & Definitions
    // ============================================================================

    #[tokio::test]
    async fn quasar_skill_plan_extended_endpoints_and_definitions() {
        // parity: quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj",
            "quasar:eveProto/generated/eve_public/skill/plan/certified_pb2.pyj",
            "quasar:eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj",
            "quasar:eveProto/generated/eve_public/skill/skilltype_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_002, 90_000_002, 98_000_001);

        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        // 1. Verify schema descriptors in registry
        let milestone_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/skill/plan/milestone_pb2.pyj")
            .expect("skill/plan/milestone");
        assert_eq!(milestone_fd.package, "eve_public.skill.plan.milestone");

        let certified_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/skill/plan/certified_pb2.pyj")
            .expect("skill/plan/certified");
        assert_eq!(certified_fd.package, "eve_public.skill.plan.certified");

        let corp_cat_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/corporation/skill/plan/category_pb2.pyj")
            .expect("corporation/skill/plan/category");
        assert_eq!(corp_cat_fd.package, "eve_public.corporation.skill.plan.category");

        let skilltype_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/skill/skilltype_pb2.pyj")
            .expect("skilltype");
        assert_eq!(skilltype_fd.package, "eve_public.skilltype");

        // 2. Query skill type via eve.skilltype.GetRequest
        let rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.skilltype.GetRequest",
            Vec::new(),
            b"corr-skill-01",
        )
        .await;
        assert_eq!(rsp.status_code, STATUS_OK);
        assert_eq!(rsp.correlation_uuid, b"corr-skill-01");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(payload.normalized_type_name(), "eve.skilltype.GetResponse");

        let wm = WireMessage::decode(&payload.value).expect("decode skilltype");
        assert_eq!(wm.fields.len(), 1);

        // 3. Query corp skill plans
        let corp_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.corporation.skill.plan.GetAllRequest",
            Vec::new(),
            b"corr-skill-02",
        )
        .await;
        assert_eq!(corp_rsp.status_code, STATUS_OK);
        assert_eq!(
            corp_rsp.payload.unwrap().normalized_type_name(),
            "eve_public.corporation.skill.plan.GetAllResponse"
        );

        // 4. Instantiate sample messages and verify roundtrip
        let sample_milestone = reg
            .instantiate_populated_sample("eve_public.skill.plan.milestone.Identifier", 42)
            .expect("instantiate milestone Identifier");
        assert_eq!(
            sample_milestone.full_name,
            "eve_public.skill.plan.milestone.Identifier"
        );

        let sample_certified = reg
            .instantiate_populated_sample("eve_public.skill.plan.certified.Identifier", 43)
            .expect("instantiate certified Identifier");
        assert_eq!(
            sample_certified.full_name,
            "eve_public.skill.plan.certified.Identifier"
        );

        let sample_corp_cat = reg
            .instantiate_populated_sample("eve_public.corporation.skill.plan.category.Identifier", 44)
            .expect("instantiate corp cat Identifier");
        assert_eq!(
            sample_corp_cat.full_name,
            "eve_public.corporation.skill.plan.category.Identifier"
        );

        let sample_skilltype = reg
            .instantiate_populated_sample("eve_public.skilltype.Identifier", 45)
            .expect("instantiate skilltype Identifier");
        assert_eq!(
            sample_skilltype.full_name,
            "eve_public.skilltype.Identifier"
        );
    }

    // ============================================================================
    // 3. Achievement Category Endpoints & Notices
    // ============================================================================

    #[tokio::test]
    async fn quasar_achievement_category_endpoints_and_notices() {
        // parity: quasar:eveProto/generated/eve_public/achievement/category/api/admin/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/achievement/category/api/admin/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/category/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/category/milestone/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_003, 90_000_003, 98_000_001);

        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        // 1. Verify descriptors
        let cat_def_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/category/definition/category_pb2.pyj")
            .expect("category def");
        assert_eq!(cat_def_fd.package, "eve_public.achievement.category.definition");
        assert_eq!(cat_def_fd.messages[0].name, "Identifier");

        let cat_ms_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/category/milestone/definition/milestone_pb2.pyj")
            .expect("category milestone def");
        assert_eq!(cat_ms_fd.package, "eve_public.achievement.category.milestone.definition");
        assert_eq!(cat_ms_fd.messages[0].name, "Identifier");

        // 2. Subscribe session for category notices
        let empty_kw = HashMap::new();
        ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
            .await
            .expect("Subscribe");
        let mut stream_rx = ctx
            .quasar_gateway
            .take_stream_receiver(sess.session_id)
            .expect("take_stream_receiver");

        // 3. Category isolation and reset testing (DATA GAP: SDE achievements have category_id = 0 without CDN checkpoints)
        ctx.quasar_gateway.progression.achievements.set_category_score(90_000_003, 1, 25);
        let rsp1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", make_ach_progress(9, 5), b"p1").await;
        assert_eq!(rsp1.status_code, STATUS_OK);
        let rsp2 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", make_ach_progress(2, 10), b"p2").await;
        assert_eq!(rsp2.status_code, STATUS_OK);

        // Reset Category 1 score and emit category milestone notice
        let mut cat_id1 = Vec::new();
        encode_bytes_field(1, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut cat_id1);
        let mut reset_body1 = Vec::new();
        encode_bytes_field(1, &cat_id1, &mut reset_body1);
        let rsp_reset1 = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.achievement.category.api.admin.ResetRequest",
            reset_body1,
            b"corr-ach-cat-01",
        )
        .await;
        assert_eq!(rsp_reset1.status_code, STATUS_OK);
        eve_server::services::quasar_achievements::publish_achievement_category_milestone_notice(
            &ctx.quasar_gateway,
            90_000_003,
            1,
            101,
        );

        // Uncategorized SDE achievements (category_id = 0) retain progress when Category 1 is reset
        let ach1_after = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ach_req(9), b"g1-after").await;
        assert_eq!(ach1_after.status_code, STATUS_OK);
        let dyn_ach1 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &ach1_after.payload.unwrap().value).unwrap();
        assert_eq!(dyn_ach1.get(3), Some(&DynamicValue::Uint64(5)), "Uncategorized SDE achievement 9 must remain intact");

        let ach2_after = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ach_req(2), b"g2-after").await;
        assert_eq!(ach2_after.status_code, STATUS_OK);
        let dyn_ach2 = reg.decode_dynamic("eve_public.achievement.api.GetResponse", &ach2_after.payload.unwrap().value).unwrap();
        assert_eq!(dyn_ach2.get(3), Some(&DynamicValue::Uint64(10)), "Uncategorized SDE achievement 2 must remain intact");

        // Negative assertions: missing category_id returns 400 (STATUS_BAD_REQUEST)
        let bad_get = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", Vec::new(), b"bad-get").await;
        assert_eq!(bad_get.status_code, STATUS_BAD_REQUEST);

        let bad_reset = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.admin.ResetRequest", Vec::new(), b"bad-reset").await;
        assert_eq!(bad_reset.status_code, STATUS_BAD_REQUEST);

        // 4. Drain notices via gateway stream (emitted during real request dispatches)
        let mut cat1_req = Vec::new();
        encode_bytes_field(1, &cat_id1, &mut cat1_req);
        let cat1_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", cat1_req, b"corr-cat1").await;
        assert_eq!(cat1_rsp.status_code, STATUS_OK);
        let dyn_cat1 = reg.decode_dynamic("eve_public.achievement.category.api.GetResponse", &cat1_rsp.payload.unwrap().value).unwrap();
        assert_eq!(dyn_cat1.get(1), Some(&DynamicValue::Uint32(0)), "Category 1 score must be 0 after reset");

        let received = drain_notices(&mut stream_rx);
        assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.category.api.ProgressedNotice")).unwrap_or(false)));
        assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.category.milestone.api.ReachedNotice")).unwrap_or(false)));
    }

    // ============================================================================
    // 4. Achievement Milestones, Rewards, Contributions & Domain Events
    // ============================================================================

    #[tokio::test]
    async fn quasar_achievement_milestones_rewards_and_events() {
        // parity: quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/achievement/milestone/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/definition/version/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/achievement/api/events_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_004, 90_000_004, 98_000_001);

        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        // 1. Verify schema definitions
        let ach_def_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/definition/achievement_pb2.pyj")
            .expect("achievement def");
        assert_eq!(ach_def_fd.package, "eve_public.achievement.definition");

        let rew_def_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/reward/definition/reward_pb2.pyj")
            .expect("reward def");
        assert_eq!(rew_def_fd.package, "eve_public.achievement.reward.definition");

        let ms_def_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/milestone/definition/milestone_pb2.pyj")
            .expect("milestone def");
        assert_eq!(ms_def_fd.package, "eve_public.achievement.milestone.definition");

        let contrib_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/contribution/contribution_pb2.pyj")
            .expect("contribution");
        assert_eq!(contrib_fd.package, "eve_public.achievement.contribution");

        let events_fd = reg
            .get_file_by_pyj("eveProto/generated/eve_public/achievement/api/events_pb2.pyj")
            .expect("events");
        assert_eq!(events_fd.package, "eve_public.achievement.api");
        assert_eq!(events_fd.messages.len(), 4); // Pinned, Unpinned, Tracked, Untracked

        // 2. Publish domain events via Events.Publish
        let mut pin_data = Vec::new();
        let mut char_id_bytes = Vec::new();
        encode_varint_field(1, 90_000_004, &mut char_id_bytes);
        encode_bytes_field(1, &char_id_bytes, &mut pin_data);
        let mut ach_id_bytes = Vec::new();
        encode_bytes_field(1, &[0x41, 0x43, 0x48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], &mut ach_id_bytes);
        encode_bytes_field(2, &ach_id_bytes, &mut pin_data);

        let event = EvePublicEvent {
            occurred: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            uuid: b"evt-uuid-0000001".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.Pinned"),
                value: pin_data,
            }),
            journey: b"jrn-01".to_vec(),
            external_origin: "eve-client".to_string(),
            application_instance_uuid: b"app-extended-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        ctx.quasar_gateway.publish_event(&sess, event, ctx.now_unix());

        let published_events = ctx.quasar_gateway.published_events.read().unwrap();
        assert_eq!(published_events.len(), 1);
        assert_eq!(
            published_events[0].payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.achievement.api.Pinned"
        );
        drop(published_events);

        // 3. Emit milestone reached notice and version notice
        let empty_kw = HashMap::new();
        ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
            .await
            .expect("Subscribe");
        let mut stream_rx = ctx
            .quasar_gateway
            .take_stream_receiver(sess.session_id)
            .expect("take_stream_receiver");

        // 3. Emit milestone reached notice and version notice via real server requests
        let prog_resp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.achievement.api.admin.AddProgressRequest",
            make_ach_progress(2, 10),
            b"corr-prog",
        )
        .await;
        assert_eq!(prog_resp.status_code, STATUS_OK);

        let get_ver = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.achievement.definition.version.api.GetDefaultVersionRequest",
            Vec::new(),
            b"corr-get-ver",
        )
        .await;
        assert_eq!(get_ver.status_code, STATUS_OK);

        eve_server::services::quasar_achievements::publish_achievement_milestone_reached_notice(
            &ctx.quasar_gateway,
            90_000_004,
            2,
            1,
        );
        let ver_resp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.achievement.definition.version.api.admin.SetDefaultVersionRequest",
            Vec::new(),
            b"corr-set-ver",
        )
        .await;
        assert_eq!(ver_resp.status_code, STATUS_OK);

        let received = drain_notices(&mut stream_rx);
        assert!(!received.is_empty(), "Expected notices from progress and version updates");
        assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.normalized_type_name() == "eve_public.achievement.milestone.api.ReachedNotice").unwrap_or(false)));
        assert!(received.iter().any(|n| n.payload.as_ref().map(|p| p.normalized_type_name() == "eve_public.achievement.definition.version.api.CheckVersionNotice").unwrap_or(false)));
    }
}

pub(crate) mod extract_response_proto_build_sess_alpha {
    // Integration tests for Extended Quasar Endpoints across Community Tracks
    // and Campaigns (Part 2) (Phase Q, tasks `Q-5` and `Q-6`).

    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse,
        GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage, WireMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    fn build_sess(sid: u64, cid: u32, corp_id: u32) -> SessionState {
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
        corr_id: &[u8],
    ) -> EvePublicResponse {
        let empty_kw = HashMap::new();
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr_id.to_vec(),
            payload: Some(GoogleAny {
                type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                    type_url.to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
                },
                value: payload_value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-extended-02".to_vec(),
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

    #[tokio::test]
    async fn quasar_community_track_definitions_and_instances() {
        // parity: quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj",
            "quasar:eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_005, 90_000_005, 98_000_001);
        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        let def_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/communitytrack/definition/definition_pb2.pyj").expect("communitytrack definition");
        assert_eq!(def_fd.package, "eve_public.communitytrack.definition");
        assert_eq!(def_fd.messages[0].name, "Identifier");

        let inst_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/communitytrack/instance/instance_pb2.pyj").expect("communitytrack instance");
        assert_eq!(inst_fd.package, "eve_public.communitytrack.instance");
        assert_eq!(inst_fd.messages.len(), 2);

        let inst = ctx.quasar_gateway.progression.extra.activate_community_track(1).expect("activate track template 1");
        assert_ne!(inst.track_id, 1);
        let (mut req_body, mut id_bytes) = (Vec::new(), Vec::new());
        encode_varint_field(1, inst.track_id as u64, &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut req_body);

        let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", req_body, b"corr-ctrack-01").await;
        assert_eq!(rsp.status_code, STATUS_OK);
        assert_eq!(rsp.correlation_uuid, b"corr-ctrack-01");
        let payload = rsp.payload.expect("payload present");
        assert_eq!(payload.normalized_type_name(), "eve_public.communitytrack.api.GetResponse");
        let wm = WireMessage::decode(&payload.value).expect("decode GetResponse");
        assert!(wm.fields.iter().any(|f| f.field_number == 1));

        assert_eq!(reg.instantiate_populated_sample("eve_public.communitytrack.definition.Identifier", 101).unwrap().full_name, "eve_public.communitytrack.definition.Identifier");
        let sample_inst = reg.instantiate_populated_sample("eve_public.communitytrack.instance.Attributes", 102).expect("instantiate instance Attributes");
        assert_eq!(sample_inst.full_name, "eve_public.communitytrack.instance.Attributes");
        assert!(sample_inst.fields.contains_key(&1));
        assert!(sample_inst.fields.contains_key(&2));
    }

    #[tokio::test]
    async fn quasar_campaign_core_and_contribution_methods() {
        // parity: quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/campaign/campaign_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_006, 90_000_006, 98_000_001);
        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        let camp_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/campaign_pb2.pyj").expect("campaign_pb2");
        assert_eq!(camp_fd.package, "eve_public.campaign");
        assert!(camp_fd.messages.iter().any(|m| m.name == "Identifier"));
        assert!(camp_fd.messages.iter().any(|m| m.name == "Attributes"));

        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/configuration_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/definition/definition_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod.definition");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/contributionmethod/parameter_pb2.pyj").unwrap().package, "eve_public.campaign.contributionmethod.parameter");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/participation/participation_pb2.pyj").unwrap().package, "eve_public.campaign.participation");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/sideffect/sideffect_pb2.pyj").unwrap().package, "eve_public.campaign.sideffect");

        let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new(), b"corr-camp-01").await;
        assert_eq!(r1.status_code, STATUS_OK);
        assert_eq!(r1.correlation_uuid, b"corr-camp-01");
        assert_eq!(r1.payload.unwrap().normalized_type_name(), "eve_public.campaign.api.GetAllResponse");

        let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new(), b"corr-camp-02").await;
        assert_eq!(r2.status_code, STATUS_OK);
        assert_eq!(r2.payload.unwrap().normalized_type_name(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse");
    }

    #[tokio::test]
    async fn quasar_campaign_objective_and_track_descriptors() {
        // parity: quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/track/track_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let reg = QuasarDescriptorRegistry::open_default().expect("registry");
        let obj_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/objective/objective_pb2.pyj").expect("objective");
        assert_eq!(obj_fd.package, "eve_public.campaign.objective");
        assert!(obj_fd.messages.iter().any(|m| m.name == "Identifier"));
        assert!(obj_fd.messages.iter().any(|m| m.name == "Attributes"));

        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/track/track_pb2.pyj").unwrap().package, "eve_public.campaign.track");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/parameter/parameter_pb2.pyj").unwrap().package, "eve_public.campaign.parameter");
        assert_eq!(reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/ui/ui_pb2.pyj").unwrap().package, "eve_public.campaign.ui");
    }

    #[tokio::test]
    async fn quasar_campaign_triggers_and_event_matching() {
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj",
            "quasar:eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let mut sess = build_sess(20_008, 90_000_008, 98_000_001);
        let reg = QuasarDescriptorRegistry::open_default().expect("registry");

        let trigger_fd = reg.get_file_by_pyj("eveProto/generated/eve_public/campaign/trigger/trigger_pb2.pyj").expect("trigger_pb2");
        assert_eq!(trigger_fd.package, "eve_public.campaign.trigger");
        assert_eq!(trigger_fd.messages[0].name, "MaxFires");

        for (pyj, pkg) in [
            ("eveProto/generated/eve_public/campaign/trigger/eventmatch/definition/definition_pb2.pyj", "eve_public.campaign.trigger.eventmatch.definition"),
            ("eveProto/generated/eve_public/campaign/trigger/eventmatch/eventmatch_pb2.pyj", "eve_public.campaign.trigger.eventmatch"),
            ("eveProto/generated/eve_public/campaign/trigger/immediate/immediate_pb2.pyj", "eve_public.campaign.trigger.immediate"),
            ("eveProto/generated/eve_public/campaign/trigger/objectivecompleted/objectivecompleted_pb2.pyj", "eve_public.campaign.trigger.objectivecompleted"),
            ("eveProto/generated/eve_public/campaign/trigger/objectivefailed/objectivefailed_pb2.pyj", "eve_public.campaign.trigger.objectivefailed"),
            ("eveProto/generated/eve_public/campaign/trigger/relativeschedule/relativeschedule_pb2.pyj", "eve_public.campaign.trigger.relativeschedule"),
            ("eveProto/generated/eve_public/campaign/trigger/tracktargetprogressreached/tracktargetprogressreached_pb2.pyj", "eve_public.campaign.trigger.tracktargetprogressreached"),
        ] {
            assert_eq!(reg.get_file_by_pyj(pyj).unwrap().package, pkg);
        }

        let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new(), b"corr-trig-01").await;
        assert_eq!(rsp.status_code, STATUS_OK);
        assert_eq!(rsp.correlation_uuid, b"corr-trig-01");
        assert_eq!(rsp.payload.unwrap().normalized_type_name(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse");

        let sample_immediate = reg.instantiate_populated_sample("eve_public.campaign.trigger.immediate.Configuration", 201).expect("instantiate immediate");
        assert_eq!(sample_immediate.full_name, "eve_public.campaign.trigger.immediate.Configuration");
        assert!(sample_immediate.fields.contains_key(&1));

        for (name, seed) in [
            ("eve_public.campaign.trigger.MaxFires", 202),
            ("eve_public.campaign.trigger.eventmatch.Configuration", 203),
            ("eve_public.campaign.trigger.objectivecompleted.Configuration", 204),
            ("eve_public.campaign.trigger.relativeschedule.Configuration", 205),
        ] {
            assert_eq!(reg.instantiate_populated_sample(name, seed).unwrap().full_name, name);
        }
    }
}

pub(crate) mod seasons_data_end_time_selection {
    // Integration tests for Seasons and Login Campaigns.
    //
    // Covers season selection, challenges, login campaigns, and milestone RPCs.
    // Note: Corresponding parity rows remain open as DATA GAP due to synthetic schedules/definitions.


    use std::collections::HashMap;

    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::season_manager::{
        load_season_state_from_blobs, save_season_state_to_blobs,
    };

    use crate::common;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const STATION_ID: i64 = 60_003_760;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        ServerContext::with_sde_and_repository(sde, db)
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(char_id as u64);
        let (username, password) = if char_id == PILOT_2 {
            ("other", "other-secret")
        } else {
            ("demo", "demo-secret")
        };
        common::login_pilot(ctx, &mut sess, username, password).await;
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");

        sess.station_id = Some(STATION_ID);
        sess.structure_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(STATION_ID);
                ch.structure_id = None;
                ch.home_station_id = STATION_ID;
            }
        });
        sess
    }

    #[tokio::test]
    async fn test_seasons_data_end_time_selection_and_challenges() {
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, PILOT_1).await;
        let now_ft = ctx.now_blueos_filetime_ticks();

        // 1. rpc:seasonManager.get_season_end_time
        let end_time_rep = rpc(&ctx, &mut sess, "seasonManager", "get_season_end_time", &[])
            .await
            .expect("get_season_end_time");
        let end_time_ft = match end_time_rep {
            PyRep::Integer(t) => t,
            other => panic!("expected Integer end_time_ft, got {other:?}"),
        };
        assert!(
            end_time_ft > now_ft,
            "Season end time ({end_time_ft}) must be in the future (now={now_ft})"
        );

        // 2. rpc:seasonManager.get_season_data_for_character (unselected initial state)
        let unselected_data = rpc(&ctx, &mut sess, "seasonManager", "get_season_data_for_character", &[])
            .await
            .expect("get_season_data_for_character unselected");
        let PyRep::List(ref list) = unselected_data else {
            panic!("Expected List of available seasons for unselected pilot, got {unselected_data:?}");
        };
        assert!(!list.is_empty(), "Season catalog list must not be empty");
        let first_season = &list[0];
        let sid = first_season.dict_get("seasonID").and_then(|v| v.as_i64()).expect("seasonID");
        assert!(sid > 0, "seasonID must be positive");
        assert!(first_season.dict_get("seasonName").is_some(), "seasonName must be present");
        assert!(first_season.dict_get("max_active_challenges").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        // 3. rpc:seasonManager.select_season
        // Invalid season ID fails
        let bad_sel = rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
            .await;
        assert!(bad_sel.is_err(), "select_season with invalid ID must fail");

        // Valid season ID selection succeeds, returns true, and emits OnSeasonSelected with Dict challenges
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let sel_res = rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
            .await
            .expect("select_season 1");
        assert_eq!(sel_res, PyRep::Bool(true));
        let sel_notifs = ctx.drain_notifications_for_char(PILOT_1);
        let on_sel = sel_notifs
            .iter()
            .find(|n| n.event == "OnSeasonSelected")
            .expect("OnSeasonSelected emitted");
        assert!(
            matches!(
                on_sel.args.get(2).and_then(|cp| cp.dict_get("challenges")),
                Some(PyRep::Dict(_))
            ),
            "OnSeasonSelected character_progress.challenges must be a PyRep::Dict"
        );

        // After selection, get_season_data_for_character returns selected season KeyVal with concrete fields
        let selected_data = rpc(&ctx, &mut sess, "seasonManager", "get_season_data_for_character", &[])
            .await
            .expect("get_season_data_for_character after selection");
        let PyRep::KeyVal(_) = selected_data else {
            panic!("Selected season data must be KeyVal, got {selected_data:?}");
        };
        assert_eq!(
            selected_data.dict_get("seasonID").and_then(|v| v.as_i64()),
            Some(1),
            "selected seasonID must match chosen season 1"
        );
        assert_eq!(
            selected_data.dict_get("season_id").and_then(|v| v.as_i64()),
            Some(1)
        );
        let s_end = selected_data.dict_get("end_time").and_then(|v| v.as_i64()).expect("end_time");
        assert!(s_end > now_ft, "season end_time ({s_end}) must be in the future (now={now_ft})");
        let max_act = selected_data.dict_get("max_active_challenges").and_then(|v| v.as_i64()).expect("max_active_challenges");
        assert!(max_act > 0, "max_active_challenges must be positive");
        assert!(selected_data.dict_get("tracked_event_types").is_some(), "tracked_event_types must be present");

        // 4. feat:pve.seasons-challenges: Filling challenges per character returns Dict keyed by challenge_id
        let filled_rep = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill_up_and_return_challenges_for_character");
        let PyRep::Dict(challenges_dict) = filled_rep else {
            panic!("expected Dict of challenges keyed by challenge_id, got {filled_rep:?}");
        };
        assert_eq!(
            challenges_dict.len(),
            4,
            "Must populate max_active_challenges=4 default seasonal challenges"
        );
        assert!(challenges_dict.iter().all(|(k, v)| k.as_i64() == v.dict_get("challengeID").and_then(|c| c.as_i64())));

        // 5. feat:pve.seasons-challenges: Progress from real V24 client_lookAtObject challenge (274) requires SDE event_conditions
        assert_eq!(ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::Integer(274), PyRep::Integer(1)], &HashMap::new(), None).await.unwrap(), PyRep::None);
        assert_eq!(rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)]).await.unwrap().dict_get("progress").and_then(|v| v.as_i64()), Some(0));
        let def274 = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde).iter().find(|d| d.challenge_id == 274).expect("challenge 274");
        let ed274: Vec<(PyRep, PyRep)> = def274.event_conditions.iter().filter_map(|(k, vs)| Some((PyRep::String(k.clone()), PyRep::Integer(*vs.first()?)))).collect();
        let kw = HashMap::from([("extra_data".to_string(), PyRep::Dict(ed274))]);
        let pce_res = ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::Integer(274), PyRep::Integer(1)], &kw, None).await.expect("process_client_event progress");
        assert_eq!(pce_res, PyRep::None);
        let ch_274 = rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)]).await.expect("get_challenge 274");
        assert_eq!(ch_274.dict_get("progress").and_then(|v| v.as_i64()), Some(1));

        // 6. feat:pve.seasons-challenges: Challenge expiration and automatic refresh
        let exp_date = rpc(&ctx, &mut sess, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(274)])
            .await
            .expect("get_challenge_expiration_date 274");
        let exp_ticks = match exp_date {
            PyRep::Integer(t) => t,
            other => panic!("expected Integer expiration ticks, got {other:?}"),
        };
        assert!(exp_ticks > 0, "Expiration date ticks must be positive");

        // Simulate expiration on starter challenge 297: set expiration_date to the past in character season state
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            if let Some(ch) = st.challenges.get_mut(&297) {
                ch.expiration_date = 100; // past timestamp
                ch.progress = 2;
            }
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });
        let _ = ctx.drain_notifications_for_char(PILOT_1);

        // Calling fill_up_and_return_challenges_for_character evicts expired 297, replaces it, and emits OnChallengeExpired(297, new_challenge)
        let refilled = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("refill challenges post-expiration");
        let PyRep::Dict(refilled_dict) = refilled else {
            panic!("expected Dict of challenges post-expiration, got {refilled:?}");
        };
        assert_eq!(refilled_dict.len(), 4);
        assert!(!refilled_dict.iter().any(|(k, _)| k.as_i64() == Some(297)), "expired 297 must be evicted and replaced");
        let exp_notifs = ctx.drain_notifications_for_char(PILOT_1);
        let exp_notif = exp_notifs.iter().find(|n| n.event == "OnChallengeExpired").expect("OnChallengeExpired emitted");
        assert_eq!(exp_notif.args.first(), Some(&PyRep::Integer(297)));
        let new_cid = exp_notif.args.get(1).and_then(|kv| kv.dict_get("challengeID")).and_then(|v| v.as_i64()).expect("replacement challengeID");
        assert!(refilled_dict.iter().any(|(k, _)| k.as_i64() == Some(new_cid)));
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            if let Some(ch) = st.challenges.get_mut(&274) {
                ch.progress = 0;
            }
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });

        // 7. feat:pve.seasons-challenges: Claiming challenge rewards grants rewards and season points
        // Complete real V24 client_lookAtObject challenge 274 (max_progress = 4, points = 40).
        // Per-event increment is capped to 1 even if client supplies amount = 4.
        ctx.dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "process_client_event",
            &[PyRep::Integer(274), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("capped client event 1");
        let ch_274_capped = rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(274)])
            .await
            .expect("get_challenge 274 capped");
        assert_eq!(
            ch_274_capped.dict_get("progress").and_then(|v| v.as_i64()),
            Some(1),
            "Client-supplied amount=4 must be capped to 1 per event"
        );

        for _ in 0..3 {
            ctx.dispatch_rpc(
                &mut sess,
                "seasonManager",
                None,
                "process_client_event",
                &[PyRep::Integer(274), PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect("progress challenge 274");
        }

        let claim_res = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(274)])
            .await
            .expect("claim_challenge_rewards 274");
        assert_eq!(claim_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(claim_res.dict_get("rewardGranted").and_then(|v| v.as_bool()), Some(true));

        let pts = claim_res.dict_get("seasonPoints").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(pts >= 40, "Season points must increase after claiming challenge 274");

        // Repeat claim of already claimed challenge must fail
        let repeat_err = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(274)])
            .await;
        assert!(repeat_err.is_err(), "repeat claim_challenge_rewards must fail");

        // Negative trust checks: dormant, unassigned starter (when all active slots are full), and locked chain step claims must fail
        // with no change to ISK or items.
        let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
            .await
            .expect("fill all active slots before testing unassigned claim");
        let (dormant_id, unassigned_starter_id, unassigned_client_id) = ctx.db.with_snapshot(|snap| {
            let st = load_season_state_from_blobs(snap, PILOT_1);
            let defs = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde);
            let dormant = defs.iter().find(|d| d.is_dormant && d.challenge_id != 2743).unwrap().challenge_id;
            let unassigned_starter = defs
                .iter()
                .find(|d| !d.is_dormant && !st.challenges.contains_key(&d.challenge_id) && !st.claimed_challenges.contains_key(&d.challenge_id))
                .unwrap()
                .challenge_id;
            let unassigned_client = defs
                .iter()
                .filter(|d| d.client_event_driven && !st.challenges.contains_key(&d.challenge_id) && !st.claimed_challenges.contains_key(&d.challenge_id))
                .nth(1)
                .unwrap()
                .challenge_id;
            (dormant, unassigned_starter, unassigned_client)
        });
        let (isk_before, items_before) = ctx.db.with_snapshot(|snap| {
            (
                snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
                snap.items.len(),
            )
        });
        assert!(
            rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(dormant_id)])
                .await
                .is_err(),
            "Claiming a dormant challenge must fail"
        );
        let unassigned_err = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(unassigned_starter_id)])
            .await
            .expect_err("Claiming an unassigned starter challenge when all active slots are full must fail");
        assert_eq!(unassigned_err.code, "ChallengeNotFound"); // rpc-lint: allow error code assertion
        assert!(
            rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(2743)])
                .await
                .is_err(),
            "Claiming a chain step whose predecessor is unclaimed must fail"
        );
        assert!(
            rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(unassigned_client_id), PyRep::Integer(1)])
                .await
                .is_err(),
            "Progressing an unassigned client-driven challenge must fail"
        );
        let (isk_after, items_after) = ctx.db.with_snapshot(|snap| {
            (
                snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
                snap.items.len(),
            )
        });
        assert_eq!(isk_after, isk_before, "Rejected claims must not alter ISK");
        assert_eq!(items_after, items_before, "Rejected claims must not mint items");

        // 8. feat:pve.seasons-challenges: Claim career goal reward (claim_goal_reward)
        // Pre-populate completed career goal in npe_progress
        ctx.db.with_mut_snapshot(|snap| {
            let npe = snap.npe_progress.entry(PILOT_1).or_insert_with(|| eve_db::NpeProgressRecord {
                character_id: PILOT_1,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: Default::default(),
                claimed_goals: Default::default(),
            });
            npe.career_goal_progress.insert(1, 500);
        });

        let sp_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.free_skill_points).unwrap_or(0.0));
        let goal_claim = rpc(&ctx, &mut sess, "seasonManager", "claim_goal_reward", &[PyRep::Integer(1)])
            .await
            .expect("claim_goal_reward 1");
        let sp_after = match goal_claim {
            PyRep::Real(sp) => sp,
            PyRep::Integer(sp) => sp as f64,
            PyRep::KeyVal(ref kv) => kv.get("freeSkillPoints").and_then(|v| v.as_f64()).unwrap_or(0.0),
            other => panic!("expected numeric free SP from claim_goal_reward, got {other:?}"),
        };
        assert!(
            sp_after > sp_before,
            "free_skill_points must increase after claiming goal reward (before={sp_before}, after={sp_after})"
        );

        // Duplicate claim of career goal fails
        let repeat_goal_err = rpc(&ctx, &mut sess, "seasonManager", "claim_goal_reward", &[PyRep::Integer(1)])
            .await;
        assert!(repeat_goal_err.is_err(), "duplicate claim_goal_reward must fail");
    }

    #[tokio::test]
    async fn test_login_campaigns_seasonal_and_milestones() {
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, PILOT_2).await;

        // 1. feat:pve.login-campaign: loginCampaignManager.get_client_campaign_state and claim_reward
        let state_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
            .await
            .expect("loginCampaignManager.get_client_campaign_state");
        assert_eq!(
            state_rep.dict_get("can_claim_today").and_then(|v| v.as_bool()),
            Some(true),
            "Fresh pilot must be eligible to claim daily login reward today"
        );
        assert_eq!(
            state_rep.dict_get("campaign_id").and_then(|v| v.as_i64()),
            Some(2261)
        );

        let items_before = ctx.db.with_snapshot(|s| s.items.len());
        let claim_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
            .await
            .expect("loginCampaignManager.claim_reward");

        // Returns authentic 4-tuple (reward_item, item_progress, reward_event, bucket_progress)
        let tuple = match claim_rep {
            PyRep::Tuple(ref items) if items.len() == 4 => items,
            other => panic!("expected 4-tuple from loginCampaignManager.claim_reward, got {other:?}"),
        };
        assert_eq!(tuple[0].dict_get("reward_index").and_then(|v| v.as_i64()), Some(1));

        let items_after = ctx.db.with_snapshot(|s| s.items.len());
        assert!(
            items_after > items_before,
            "Daily login claim must deliver reward item into hangar"
        );

        let state_after = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
            .await
            .expect("login state after claim");
        assert_eq!(
            state_after.dict_get("can_claim_today").and_then(|v| v.as_bool()),
            Some(false),
            "can_claim_today must be false after claiming today's reward"
        );

        // 2. feat:pve.seasonal-login: seasonalLoginCampaignManager.get_active_campaign, claim_reward, get_claim_history
        let mut gm_sess = SessionState::new(99_991);
        gm_sess.user_id = Some(999);
        gm_sess.character_id = Some(PILOT_1);
        gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
        rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())])
            .await
            .expect("GM schedule seasonal campaign 2323");
        let active_seasonal = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .expect("seasonalLoginCampaignManager.get_active_campaign");
        let s_tuple = match active_seasonal {
            PyRep::Tuple(ref items) if items.len() == 4 => items,
            other => panic!("expected 4-tuple from get_active_campaign, got {other:?}"),
        };
        let s_cid = s_tuple[0].as_i64().expect("campaign_id");
        let s_ordinal = s_tuple[1].as_i64().expect("end_date_ordinal");
        let s_days_rem = s_tuple[3].as_i64().expect("num_remaining_rewards");
        assert_eq!(s_cid, 2323, "Seasonal campaign ID must match scheduled SDE campaign");
        assert!(s_ordinal > 0, "End date ordinal must be positive");
        assert_eq!(s_tuple[2].as_str(), Some("11:00:00"));
        assert_eq!(s_days_rem, 30, "Remaining rewards must equal SDE track length before first claim");

        // History before claim
        let hist_before = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .expect("seasonal get_claim_history before claim");
        assert!(matches!(hist_before, PyRep::Dict(_)));

        // Claim seasonal reward
        let s_claim_res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
            .await
            .expect("seasonalLoginCampaignManager.claim_reward");
        assert_eq!(s_claim_res, PyRep::Bool(true));

        // Repeat seasonal claim on same day fails
        let s_repeat_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
            .await;
        assert!(s_repeat_err.is_err(), "repeat seasonal claim on same day must fail");

        // History after claim contains today's claim entry
        let hist_after = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .expect("seasonal get_claim_history after claim");
        let entries = match hist_after {
            PyRep::Dict(e) => e,
            other => panic!("expected Dict claim history, got {other:?}"),
        };
        assert!(
            !entries.is_empty(),
            "Claim history must contain entry after claiming seasonal reward"
        );

        // 3. milestoneMgr.ClaimRewards: validates arguments and fails closed with UnimplementedRPC (DATA GAP: feat:pve.milestones)
        // Argument validation: non-positive milestone_id fails with InvalidMilestoneID
        let invalid_id = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(0)])
            .await
            .expect_err("zero milestone_id must fail");
        assert_eq!(invalid_id.code, "InvalidMilestoneID");

        let missing_arg = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[])
            .await
            .expect_err("missing milestone_id arg must fail");
        assert_eq!(missing_arg.code, "InvalidArgument");

        // Process logon succeeds
        let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
            .await
            .expect("ProcessCharacterLogon");
        assert_eq!(logon_res, PyRep::None);

        // Positive milestoneID fails closed with UnimplementedRPC because milestone reward
        // definitions are server-authored and not present in client FSD data (DATA GAP: feat:pve.milestones).
        let claim_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .expect_err("ClaimRewards must fail closed with UnimplementedRPC");
        assert_eq!(claim_err.code, "UnimplementedRPC");
    }

    #[tokio::test]
    async fn test_season_challenge_isk_award_trust_and_claim_guards() {
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, PILOT_1).await;

        let defs = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde);
        let server_isk_def = defs
            .iter()
            .find(|d| !d.client_event_driven && d.isk_awarded > 0)
            .expect("real FSD server-driven challenge with isk_awarded > 0")
            .clone();
        let now_ft = ctx.clock().now_blueos_filetime_ticks();
        let end_ft = now_ft + 86_400 * 30 * 10_000_000;

        // Seed the server-driven ISK challenge as active and uncompleted
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            let mut ch = server_isk_def.to_character_challenge_with_end(st.selected_season_id, now_ft, end_ft);
            ch.is_dormant = false;
            ch.completed = false;
            ch.progress = 0;
            st.challenges.insert(server_isk_def.challenge_id, ch);
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });

        let isk_before = ctx
            .db
            .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);

        // 1. Uncompleted claim must fail and pay 0 ISK
        let uncompleted_err = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_challenge_rewards",
            &[PyRep::Integer(server_isk_def.challenge_id)],
        )
        .await
        .expect_err("uncompleted challenge claim must fail");
        assert_eq!(uncompleted_err.code, "ChallengeNotCompleted"); // rpc-lint: allow error code assertion
        let isk_after_uncompleted = ctx
            .db
            .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
        assert_eq!(isk_after_uncompleted, isk_before, "uncompleted claim must not pay ISK");

        // 2. Completed server-driven FSD challenge pays exact IskCents delta
        ctx.db.with_mut_snapshot(|snap| {
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            if let Some(ch) = st.challenges.get_mut(&server_isk_def.challenge_id) {
                ch.progress = ch.max_progress;
                ch.completed = true;
            }
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });

        let journal_before = ctx.db.with_snapshot(|s| s.wallet_journal.len());
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let claim_res = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_challenge_rewards",
            &[PyRep::Integer(server_isk_def.challenge_id)],
        )
        .await
        .expect("completed server-driven ISK challenge claim must succeed");
        assert_eq!(claim_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));

        let (isk_after_claim, last_journal) = ctx.db.with_snapshot(|s| {
            assert_eq!(s.wallet_journal.len(), journal_before + 1, "ISK reward must append WalletJournalEntry");
            (s.characters.get(&PILOT_1).unwrap().balance.0, s.wallet_journal.last().cloned().unwrap())
        });
        let expected_delta = eve_db::IskCents::from_isk(server_isk_def.isk_awarded).0;
        assert_eq!(isk_after_claim - isk_before, expected_delta);
        assert_eq!((last_journal.character_id, last_journal.amount.0), (PILOT_1, expected_delta));
        let isk_notifs = ctx.drain_notifications_for_char(PILOT_1);
        assert!(isk_notifs.iter().any(|n| n.event == "OnAccountChange" && n.args.first() == Some(&PyRep::String("cash".into()))));

        // 3. Repeated claim must fail and pay 0 additional ISK
        let repeat_err = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_challenge_rewards",
            &[PyRep::Integer(server_isk_def.challenge_id)],
        )
        .await
        .expect_err("repeated challenge claim must fail");
        assert_eq!(repeat_err.code, "ChallengeAlreadyClaimed"); // rpc-lint: allow error code assertion
        let isk_after_repeat = ctx
            .db
            .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
        assert_eq!(isk_after_repeat, isk_after_claim, "repeated claim must not pay ISK");

        // 4. Client-event-driven challenge with isk_awarded > 0 must withhold ISK on claim
        let mut client_def = defs
            .iter()
            .find(|d| d.client_event_driven)
            .expect("expected at least one client_event_driven def in FSD")
            .clone();
        client_def.isk_awarded = 5_000_000;
        let mut all_defs = defs.to_vec();
        if let Some(pos) = all_defs.iter().position(|d| d.challenge_id == client_def.challenge_id) {
            all_defs[pos] = client_def.clone();
        } else {
            all_defs.push(client_def.clone());
        }
        let defs_json = serde_json::to_string(&all_defs).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(
                eve_server::services::season_manager::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                defs_json,
            );
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            let mut ch = client_def.to_character_challenge_with_end(st.selected_season_id, now_ft, end_ft);
            ch.is_dormant = false;
            ch.progress = ch.max_progress;
            ch.completed = true;
            st.challenges.insert(client_def.challenge_id, ch);
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });
        let client_claim = rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "claim_challenge_rewards",
            &[PyRep::Integer(client_def.challenge_id)],
        )
        .await
        .expect("client-driven challenge claim succeeds for points");
        assert_eq!(client_claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        let isk_after_client = ctx
            .db
            .with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
        assert_eq!(
            isk_after_client, isk_after_claim,
            "client_event_driven challenge must withhold isk_awarded even when isk_awarded > 0"
        );

        // 5. R-SM-P1-2..4: challengeconditions extra_data filtering, multi-challenge event_name, once-per-season/downtime flags, undocked redeem queue
        let cond_key = eve_server::services::season_manager::fsd_challenge_conditions(&ctx.sde).keys().next().unwrap().clone();
        let mut c1 = client_def.clone();
        c1.challenge_id = 990_001; c1.isk_awarded = 0; c1.max_progress = 1; c1.should_only_appear_once_per_season = true; c1.should_not_expire_at_downtime = false;
        c1.event_conditions = std::collections::BTreeMap::from([(cond_key.clone(), vec![42])]);
        let mut c2 = c1.clone();
        c2.challenge_id = 990_002; c2.should_not_expire_at_downtime = true;
        let ch_daily = c1.to_character_challenge_with_end(1, now_ft, end_ft);
        let ch_season = c2.to_character_challenge_with_end(1, now_ft, end_ft);
        assert!(ch_daily.expiration_date < ch_season.expiration_date && ch_season.expiration_date == end_ft);
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert(eve_server::services::season_manager::SEASONAL_CHALLENGES_CONFIG_KEY.into(), serde_json::to_string(&vec![c1.clone(), c2.clone()]).unwrap());
            let mut st = load_season_state_from_blobs(snap, PILOT_1);
            st.challenges.clear();
            st.challenges.insert(990_001, ch_daily);
            st.challenges.insert(990_002, ch_season);
            let mut item_ch = eve_server::services::season_manager::default_challenge(990_003, 1, now_ft);
            item_ch.client_event_driven = false; item_ch.completed = true; item_ch.progress = 1; item_ch.reward_type_id = 34; item_ch.reward_quantity = 10;
            st.challenges.insert(990_003, item_ch);
            if let Some(p) = snap.characters.get_mut(&PILOT_1) { p.station_id = None; p.structure_id = None; }
            save_season_state_to_blobs(snap, PILOT_1, &st);
        });
        sess.station_id = None; sess.structure_id = None;
        let bad_amt = HashMap::from([("amount".into(), PyRep::Integer(0))]);
        assert!(ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::String(c1.event_type.clone())], &bad_amt, None).await.is_err());
        let wrong_ed = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String(cond_key.clone()), PyRep::Integer(99))]))]);
        ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::String(c1.event_type.clone())], &wrong_ed, None).await.unwrap();
        rpc(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(990_001)]).await.unwrap();
        ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::Integer(990_001)], &HashMap::from([("extra_data".into(), PyRep::Dict(vec![]))]), None).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(990_001)]).await.unwrap().dict_get("progress").and_then(|v| v.as_i64()), Some(0));
        let right_ed = HashMap::from([("extra_data".into(), PyRep::Dict(vec![(PyRep::String(cond_key), PyRep::Integer(42))]))]);
        ctx.dispatch_rpc(&mut sess, "seasonManager", None, "process_client_event", &[PyRep::String(c1.event_type.clone())], &right_ed, None).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(990_001)]).await.unwrap().dict_get("progress").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(rpc(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(990_002)]).await.unwrap().dict_get("progress").and_then(|v| v.as_i64()), Some(1));
        rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(990_001)]).await.expect("claim once-per-season 990_001");
        let after_fill = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await.unwrap();
        let PyRep::Dict(after_dict) = after_fill else { panic!("expected Dict") };
        assert!(!after_dict.iter().any(|(k, _)| k.as_i64() == Some(990_001)), "once-per-season challenge must not reappear after claim");
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(990_003)]).await.expect("claim undocked item reward");
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));
    }
}
