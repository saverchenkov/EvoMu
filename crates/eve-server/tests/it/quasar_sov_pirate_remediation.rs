//! Remediation tests for Quasar Pirate Insurgency, Stargate Lock, Freelance Contribution Method,
//! Mercenary Den, Skyhook admin and Star resource endpoints.
//!
//! Validates data-driven responses, state mutations, and notices emitted by real server state
//! transitions (RPCs / stargate jumps) — no test calls a `publish_*` helper directly.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, structure, subscribe, varint};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;
const TURNUR: u32 = 30_002_086;
const ZARZAKH: u32 = 30_100_000;
/// SDE stargate Turnur -> Zarzakh (`mapJumps`).
const TURNUR_ZARZAKH_GATE: i64 = 50_016_562;

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

fn build_sess(sid: u64, cid: u32, corp_id: u32, sys_id: u32) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(corp_id as i64);
    sess.solar_system_id = Some(sys_id);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess.corp_role = 0x01; // Director role
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
        application_instance_uuid: b"app-sov-pirate-01".to_vec(),
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

fn encode_id_req(id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut out);
    out
}

#[tokio::test]
async fn test_authentic_freelance_pirate_stargate_remediated() {
    // parity: quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let zarzakh_sys = ZARZAKH;
    let mut sess = build_sess(60_001, 90_000_021, 98_000_001, zarzakh_sys);

    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

    // 1. Freelance contribution method definitions: GetAllLatestRequest and GetAllLatestWithinMajorRequest
    let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest", Vec::new(), b"fl-01").await;
    assert_eq!(r1.status_code, STATUS_OK);
    let r1_payload = r1.payload.expect("r1 payload");
    assert_eq!(r1_payload.normalized_type_name(), "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse");

    let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllLatestResponse");
    // Verify field 1: Version specification
    let ver_field = wm1.fields.iter().find(|f| f.field_number == 1).expect("version field");
    let ver_wm = WireMessage::decode(ver_field.value.as_bytes().expect("ver bytes")).expect("decode ver");
    assert_eq!(ver_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1), "Major 1");

    // Verify field 2: Repeated definition entries (>= 20 authentic methods)
    let def_fields: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 2).collect();
    assert_eq!(def_fields.len(), 22, "all 22 SDE contributionmethods.fsdbinary records");
    // SDE record `ManufactureItem`: titleID 665394, progressDescriptionID 696246, 3 parameters.
    let manufacture = def_fields
        .iter()
        .filter_map(|f| f.value.as_bytes())
        .find(|d| field(d, 1).and_then(|id| WireMessage::decode(&id).ok()).is_some_and(|m| m.fields.iter().any(|f| f.value.as_bytes() == Some(b"ManufactureItem".as_slice()))))
        .expect("ManufactureItem definition");
    let attrs = field(manufacture, 2).expect("attributes");
    assert_eq!(field(&attrs, 1).and_then(|t| varint(&t, 1)), Some(665_394), "SDE titleID");
    assert_eq!(field(&attrs, 3).and_then(|t| varint(&t, 1)), Some(696_246), "SDE progressDescriptionID");
    let params = fields(&attrs, 11);
    assert_eq!(params.len(), 3, "SDE parameter definitions encoded");
    let first = field(&params[0], 1).expect("matcher parameter");
    assert_eq!(field(&first, 1).as_deref(), Some(b"item_type".as_slice()));

    let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest", Vec::new(), b"fl-02").await;
    assert_eq!(r2.status_code, STATUS_OK);

    // 2. Pirate Insurgency Corruption & Suppression: GetSystemInfoRequest & GetStageThresholdsRequest
    //    DERIVED: stage thresholds `[200, 400, 600, 800, 1000]` over denominator `1000`
    //    (`quasar_space_services::INSURGENCY_STAGE_THRESHOLDS`).
    let gate_pos = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].position.to_array();
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaigns.insert(
            1,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: eve_server::services::insurgency_services::FACTION_GURISTAS,
                target_faction_id: 500001,
                fob_solar_system_id: zarzakh_sys,
                systems: vec![zarzakh_sys, TURNUR],
                created_at_unix: ctx.now_unix(),
                forecasting_end_unix: ctx.now_unix(),
                active_end_unix: ctx.now_unix() + 86400,
                winddown_end_unix: ctx.now_unix() + 172800,
                corruption_points: [(zarzakh_sys, 450), (TURNUR, 200)].into_iter().collect(),
                suppression_points: [(zarzakh_sys, 250), (TURNUR, 150)].into_iter().collect(),
            },
        );
        s.system_states.insert(
            zarzakh_sys,
            eve_db::SystemDynamicState {
                solar_system_id: zarzakh_sys,
                insurgency_corruption_stage: 3,
                insurgency_suppression_stage: 2,
                ..Default::default()
            },
        );
        s.system_states.insert(
            TURNUR,
            eve_db::SystemDynamicState {
                solar_system_id: TURNUR,
                fw_complex_position: Some(gate_pos),
                fw_capture_duration_seconds: 600,
                insurgency_corruption_stage: 1,
                insurgency_suppression_stage: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", encode_id_req(99_999_999), b"pi-404c").await.status_code,
        eve_server::quasar_gateway::STATUS_NOT_FOUND
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", encode_id_req(99_999_999), b"pi-404s").await.status_code,
        eve_server::quasar_gateway::STATUS_NOT_FOUND
    );

    let corr_info = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", encode_id_req(zarzakh_sys as u64), b"pi-01").await;
    assert_eq!(corr_info.status_code, STATUS_OK);
    let c_bytes = corr_info.payload.expect("corr payload").value;
    let c_wm = WireMessage::decode(&c_bytes).expect("decode corr info");
    assert_eq!(c_wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_varint()), Some(3), "Stage 3");
    assert_eq!(field(&c_bytes, 1).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(450), Some(1000))));

    let corr_th = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetStageThresholdsRequest", Vec::new(), b"pi-02").await;
    assert_eq!(corr_th.status_code, STATUS_OK);
    let cth_bytes = corr_th.payload.expect("corr th payload").value;
    assert_eq!(
        fields(&cth_bytes, 1).iter().map(|f| (varint(f, 1), varint(f, 2))).collect::<Vec<_>>(),
        vec![(Some(200), Some(1000)), (Some(400), Some(1000)), (Some(600), Some(1000)), (Some(800), Some(1000)), (Some(1000), Some(1000))]
    );

    let supp_info = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", encode_id_req(zarzakh_sys as u64), b"pi-03").await;
    assert_eq!(supp_info.status_code, STATUS_OK);
    let s_bytes = supp_info.payload.expect("supp payload").value;
    let s_wm = WireMessage::decode(&s_bytes).expect("decode supp info");
    assert_eq!(s_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(2), "Stage 2");
    assert_eq!(field(&s_bytes, 1).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(250), Some(1000))));

    let supp_th = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetStageThresholdsRequest", Vec::new(), b"pi-04").await;
    assert_eq!(supp_th.status_code, STATUS_OK);
    let sth_bytes = supp_th.payload.expect("supp th payload").value;
    assert_eq!(fields(&sth_bytes, 1).len(), 5);

    // 3. Point decreases emit SetNotice (not IncreasedNotice).
    eve_server::services::insurgency_services::add_system_corruption(&ctx, 1, zarzakh_sys, -50);
    eve_server::services::insurgency_services::add_system_suppression(&ctx, 1, zarzakh_sys, -50);
    let dec_nots = drain_notices(&mut stream_rx);
    assert!(!dec_nots.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("api.IncreasedNotice"))));
    let c_set = dec_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with("pirate.corruption.api.SetNotice"))).expect("corruption SetNotice");
    assert_eq!(field(&c_set.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
    assert_eq!(field(&c_set.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(400), Some(1000))));
    let s_set = dec_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with("pirate.suppression.api.SetNotice"))).expect("suppression SetNotice");
    assert_eq!(field(&s_set.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
    assert_eq!(field(&s_set.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(200), Some(1000))));

    // 4. Real client-facing complex capture RPC in Turnur emits IncreasedNotice for corruption/suppression,
    //    followed by a real CmdStargateJump Turnur -> Zarzakh that creates the 6h gate lock.
    let kw = HashMap::new();
    let mut jsess = SessionState::new(60_010);
    crate::common::login_pilot(&ctx, &mut jsess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut jsess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT)], &kw, None).await.expect("SelectCharacterID");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
    ctx.dispatch_rpc(&mut jsess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = jsess.ship_id.expect("active ship");
    let mut ball = ctx.systems.get_or_spawn(30_000_142).remove_ball(ship_id).await.expect("ball in Jita");
    ctx.db.set_character_solar_system(PILOT, TURNUR, &ctx.sde).expect("move to Turnur");
    ball.position = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].position;
    ball.mode = MovementMode::Stop;
    ctx.systems.get_or_spawn(TURNUR).spawn_ball(ball).await.expect("spawn in Turnur");
    jsess.solar_system_id = Some(TURNUR);
    ctx.dispatch_rpc(&mut jsess, "publicGatewaySvc", None, "Subscribe", &[], &kw, None).await.expect("Subscribe");
    let mut jump_rx = ctx.quasar_gateway.take_stream_receiver(jsess.session_id).expect("jump stream rx");

    // Enlist in Guristas (500_010) and query GetLocalCampaignClientSnapshot on the complex beacon -> emits corruption IncreasedNotice.
    ctx.dispatch_rpc(&mut jsess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_010)], &kw, None).await.expect("Join Guristas");
    ctx.dispatch_rpc(&mut jsess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.expect("GetLocalCampaignClientSnapshot pirate");
    let c_inc = drain_notices(&mut jump_rx)
        .into_iter()
        .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice")))
        .expect("corruption IncreasedNotice from complex pulse");
    assert_eq!(field(&c_inc.value, 1).and_then(|i| varint(&i, 1)), Some(TURNUR as u64));
    assert_eq!(field(&c_inc.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(200), Some(1000))));
    assert_eq!(field(&c_inc.value, 3).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(250), Some(1000))));
    assert_eq!((varint(&c_inc.value, 4), varint(&c_inc.value, 5)), (Some(1), Some(0)));

    // Enlist in Caldari State (500_001), reset complex tick guard, and query GetLocalCampaignClientSnapshot -> emits suppression IncreasedNotice.
    ctx.db.with_mut_snapshot(|s| { s.system_states.get_mut(&TURNUR).unwrap().last_fw_capture_tick = 0; });
    ctx.dispatch_rpc(&mut jsess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_001)], &kw, None).await.expect("Join Caldari");
    ctx.dispatch_rpc(&mut jsess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.expect("GetLocalCampaignClientSnapshot anti-pirate");
    let s_inc = drain_notices(&mut jump_rx)
        .into_iter()
        .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("pirate.suppression.api.IncreasedNotice")))
        .expect("suppression IncreasedNotice from complex pulse");
    assert_eq!(field(&s_inc.value, 1).and_then(|i| varint(&i, 1)), Some(TURNUR as u64));
    assert_eq!(field(&s_inc.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(150), Some(1000))));
    assert_eq!(field(&s_inc.value, 3).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(200), Some(1000))));
    assert_eq!((varint(&s_inc.value, 4), varint(&s_inc.value, 5)), (Some(1), Some(0)));

    // Ending the insurgency campaign emits ResetNotice (0/1000) for both corruption and suppression.
    eve_server::services::insurgency_services::end_insurgency_campaign(&ctx, 1);
    let reset_nots = drain_notices(&mut stream_rx);
    for suffix in ["pirate.corruption.api.ResetNotice", "pirate.suppression.api.ResetNotice"] {
        let r_not = reset_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with(suffix))).expect(suffix);
        assert_eq!(field(&r_not.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
        assert_eq!(field(&r_not.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(0), Some(1000))));
    }

    let beyonce = ctx.bind_object(&mut jsess, "beyonce", TURNUR as i64).expect("bind beyonce");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
    ctx.dispatch_rpc(&mut jsess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(TURNUR_ZARZAKH_GATE)], &kw, None)
        .await
        .expect("jump into Zarzakh");
    assert_eq!(jsess.solar_system_id, Some(ZARZAKH));
    let dest_gate = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].destination_stargate_id;
    let lock_not = drain_notices(&mut jump_rx)
        .into_iter()
        .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("stargate.lock.api.ActivatedNotice")))
        .expect("jumping pilot receives ActivatedNotice");
    let lock_wm = WireMessage::decode(&lock_not.value).expect("decode ActivatedNotice");
    let sub = |n: u32| lock_wm.fields.iter().find(|f| f.field_number == n).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
    assert_eq!(sub(1).and_then(|m| m.fields[0].value.as_varint()), Some(ZARZAKH as u64));
    assert_eq!(sub(2).and_then(|m| m.fields[0].value.as_varint()), Some(dest_gate as u64));

    let lock_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRequest", Vec::new(), b"gl-01").await;
    assert_eq!(lock_rsp.status_code, STATUS_OK);
    let lock_wm = WireMessage::decode(&lock_rsp.payload.expect("lock payload").value).expect("decode lock info");
    let gate = lock_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
    assert_eq!(gate.and_then(|m| m.fields[0].value.as_varint()), Some(dest_gate as u64), "lock bound to arrival gate");

    let rest_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Vec::new(), b"gl-02").await;
    assert_eq!(rest_rsp.status_code, STATUS_OK);
    let rest_wm = WireMessage::decode(&rest_rsp.payload.expect("rest payload").value).expect("decode rest");
    let rest_sys = rest_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
    assert_eq!(rest_sys.and_then(|m| m.fields[0].value.as_varint()), Some(ZARZAKH as u64), "restricted system is Zarzakh");

    // After the 6h lock expires neither request reports it.
    ctx.advance_secs(6 * 3_600 + 1);
    let lock_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRequest", Vec::new(), b"gl-03").await;
    assert!(lock_rsp.payload.expect("lock payload").value.is_empty(), "expired lock not reported");
    let rest_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Vec::new(), b"gl-04").await;
    assert!(rest_rsp.payload.expect("rest payload").value.is_empty(), "no restricted systems once expired");
}

fn den_record(den_id: i64, skyhook_id: i64, now: u64) -> eve_db::PersistedMercenaryDen {
    eve_db::PersistedMercenaryDen {
        den_id,
        type_id: 85_230,
        owner_char_id: PILOT,
        skyhook_id,
        solar_system_id: 30_000_142,
        planet_id: 40_009_077,
        enabled: true,
        cargo_extraction_enabled: true,
        development_level: 15,
        anarchy_level: 0,
        infomorphs: 0,
        simulated_at_unix: now,
        next_activity_generation_unix: 0,
    }
}

// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
#[tokio::test]
async fn test_mercenary_den_activities_and_evolution_state_backed() {
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (sys_id, skyhook_id, den_id) = (30_000_142u32, 1_000_000_050i64, 1_000_000_060i64);
    let mut s = sess(60_003, PILOT, 98_000_001, sys_id, 0);
    let mut rx = subscribe(&ctx, &mut s).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, structure(skyhook_id, 98_000_001, sys_id, TYPE_ORBITAL_SKYHOOK));
        snap.structures.insert(den_id, structure(den_id, 98_000_001, sys_id, 85_230));
        snap.sov_runtime.mercenary_dens.insert(den_id, den_record(den_id, skyhook_id, ctx.now_unix()));
        snap.skills.entry(PILOT).or_default().insert(85_233, eve_db::TrainedSkillRecord { type_id: 85_233, level: 3, skill_points: 0.0 });
    });

    let max = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await;
    let max = max.payload.expect("max").value;
    assert_eq!((varint(&max, 1), varint(&max, 2)), (Some(3), Some(5)), "Mercenary Den Management level 3");
    let owned = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await.payload.expect("owned").value;
    assert_eq!(fields(&owned, 1).iter().map(|i| varint(i, 1)).collect::<Vec<_>>(), vec![Some(den_id as u64)]);
    let mut other = sess(60_004, OTHER, 98_000_002, sys_id, 0);
    assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, STATUS_FORBIDDEN);

    // Lazy evolution: 6 hours lift development 15 -> 21 (stage 1 -> 2) and emit the notice.
    ctx.advance_secs(6 * 3_600);
    let den = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.expect("den").value;
    let dev = field(&field(&field(&den, 3).unwrap(), 2).unwrap(), 3).unwrap();
    assert_eq!((varint(&dev, 1), varint(&dev, 2)), (Some(21), Some(2)));
    assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));

    // GM admin development change -> EvolutionStateChangedNotice; players are rejected.
    let mut modify = id_req(1, den_id as u64);
    encode_varint_field(2, 85, &mut modify);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest", modify.clone()).await.status_code, STATUS_FORBIDDEN);
    let mut gm = sess(60_005, OTHER, 98_000_002, sys_id, 0);
    gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(rpc(&ctx, &mut gm, "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest", modify).await.status_code, STATUS_OK);
    let gm_notices = drain(&mut rx);
    assert!(gm_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));
    assert!(gm_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));

    // Activity generation at stage 5 fills 7 slots (AddedNotice each); capacity matches.
    let acts = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.expect("acts").value;
    let list = fields(&acts, 1);
    assert_eq!(list.len(), 7);
    assert_eq!(drain(&mut rx).iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 7);
    let cap = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.payload.expect("cap").value;
    assert_eq!(varint(&cap, 1), Some(7));
    let all = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.expect("all").value;
    assert_eq!(fields(&all, 1).len(), 7);

    // Start one operation: StartedNotice, ExpiryChangedNotice, persisted, and cannot start twice.
    let start_req = field(&list[0], 1).map(|id| {
        let mut b = Vec::new();
        encode_bytes_field(1, &id, &mut b);
        b
    }).expect("activity id");
    let started = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req.clone()).await;
    assert_eq!(started.status_code, STATUS_OK);
    assert_eq!(field(&started.payload.unwrap().value, 1).and_then(|a| varint(&a, 3)), Some(1));
    let start_notices = drain(&mut rx);
    assert!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));
    assert!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice"));
    assert_ne!(rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.mercenary_activities.values().filter(|a| a.started).count()), 1);

    // After completion window (1800s), activity completes via real server tick/refresh.
    ctx.advance_secs(2_000);
    rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    let complete_notices = drain(&mut rx);
    assert!(complete_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.mercenary_activities.values().filter(|a| a.started).count()), 0);

    // Four days later unstarted operations expire (RemovedNotice) and the generator refills.
    ctx.advance_secs(4 * 86_400);
    rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    let n = drain(&mut rx);
    assert_eq!(n.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice").count(), 6);
    assert_eq!(n.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 7);
}

#[tokio::test]
async fn test_skyhook_admin_auth_window_and_star_resources() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (sys_id, skyhook_id) = (30_000_142u32, 1_000_000_050i64);
    let mut s = sess(60_006, PILOT, 98_000_001, sys_id, eve_db::corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, structure(skyhook_id, 98_000_001, sys_id, TYPE_ORBITAL_SKYHOOK));
    });

    // Star configurations come from the SDE star celestial of the caller's system (Jita - Star).
    let stars = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await.payload.expect("stars").value;
    let star = fields(&stars, 1);
    assert_eq!(star.len(), 1);
    assert_eq!(field(&star[0], 1).and_then(|i| varint(&i, 1)), Some(40_009_076));
    assert_eq!(varint(&star[0], 2), Some(800), "O-class star power");

    // ModifyReagents: ownership + director role enforced; state mutated on success.
    let mut reag = Vec::new();
    encode_bytes_field(1, &idm(81_143), &mut reag);
    encode_varint_field(2, 1_200, &mut reag);
    let mut mod_req = id_req(1, skyhook_id as u64);
    encode_bytes_field(2, &reag, &mut mod_req);
    let mut other = sess(60_007, OTHER, 98_000_002, sys_id, eve_db::corp_roles::ROLE_DIRECTOR);
    let mut no_role = sess(60_008, PILOT, 98_000_001, sys_id, 0);
    // `.admin.` packages are GM-gated by the gateway; give every session GM bits so the
    // handler's own ownership + director checks are what reject `other` / `no_role`.
    for x in [&mut s, &mut other, &mut no_role] {
        x.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    }
    assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_reagents.get(&skyhook_id).copied()), Some((81_143, 1_200)));
    assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice"));

    // Theft window: requested timestamps decoded and validated against the server clock.
    let now = ctx.now_unix();
    let window = |start: u64, end: u64| {
        let mut b = id_req(1, skyhook_id as u64);
        encode_bytes_field(2, &idm(start), &mut b);
        encode_bytes_field(3, &idm(end), &mut b);
        b
    };
    let sched = "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest";
    assert_eq!(rpc(&ctx, &mut s, sched, window(now - 60, now + 3_600)).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(rpc(&ctx, &mut s, sched, window(now + 3_600, now + 1_800)).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(rpc(&ctx, &mut no_role, sched, window(now + 3_600, now + 7_200)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut s, sched, window(now + 3_600, now + 7_200)).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied()), Some((now + 3_600, now + 7_200)));
    let n = drain(&mut rx);
    let notice = n.iter().find(|(t, _)| t == "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice").expect("scheduled notice");
    assert_eq!(field(&notice.1, 2).and_then(|t| varint(&t, 1)), Some(now + 3_600));
    assert_eq!(field(&notice.1, 3).and_then(|t| varint(&t, 1)), Some(now + 7_200));

    // Queries are state-backed: unknown skyhooks 404, corp listing only returns skyhooks, and the
    // theft window shows up as vulnerable only once it opens on the server clock.
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, 1_000_000_001)).await.status_code, eve_server::quasar_gateway::STATUS_NOT_FOUND);
    ctx.db.with_mut_snapshot(|snap| { snap.structures.insert(1_000_000_051, structure(1_000_000_051, 98_000_001, sys_id, 35_832)); });
    let by_corp = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest", id_req(1, 98_000_001)).await.payload.expect("by corp").value;
    assert_eq!(fields(&by_corp, 1).iter().map(|sk| field(sk, 1).and_then(|i| varint(&i, 1))).collect::<Vec<_>>(), vec![Some(skyhook_id as u64)]);
    let vuln = "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest";
    assert!(rpc(&ctx, &mut s, vuln, Vec::new()).await.payload.expect("vuln").value.is_empty(), "window not open yet");
    ctx.advance_secs(3_600);
    let systems = rpc(&ctx, &mut s, vuln, Vec::new()).await.payload.expect("vuln").value;
    assert_eq!(fields(&systems, 1).iter().map(|f| varint(f, 1)).collect::<Vec<_>>(), vec![Some(sys_id as u64)]);
    let in_sys = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest", id_req(1, sys_id as u64)).await.payload.expect("in sys").value;
    assert_eq!(field(&fields(&in_sys, 1)[0], 3).and_then(|t| varint(&t, 1)), Some(now + 7_200), "window end");

    // Activate / Deactivate require ownership + director, persist in `skyhook_active` without
    // mutating `UpwellStructureRecord.state`, reject redundant transitions without leaking notices,
    // and emit `ActivationNotice` with `solar_system(1)`, `planet(2)`, `skyhook(3)`, and `active(4)`.
    assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_BAD_REQUEST, "already active");
    assert!(drain(&mut rx).is_empty(), "failed ActivateRequest must not publish notices");
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.structures[&skyhook_id].state.clone()), "ShieldVulnerable", "UpwellStructureRecord.state preserved");
    let sk_deact = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await.payload.expect("get skyhook").value;
    assert_eq!(varint(&sk_deact, 3), Some(0), "skyhook active==false after DeactivateRequest");
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_BAD_REQUEST, "already inactive");
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
    let sk_act = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await.payload.expect("get skyhook").value;
    assert_eq!(varint(&sk_act, 3), Some(1), "skyhook active==true after ActivateRequest");
    let act_notices: Vec<_> = drain(&mut rx).into_iter().filter(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ActivationNotice").collect();
    assert_eq!(act_notices.len(), 2);
    for (idx, expected_active) in [(0usize, 0u64), (1usize, 1u64)] {
        let body = &act_notices[idx].1;
        assert_eq!(field(body, 1).and_then(|i| varint(&i, 1)), Some(sys_id as u64), "solar_system(1)");
        assert_eq!(field(body, 2).and_then(|i| varint(&i, 1)), Some(40_009_077), "planet(2) Jita I");
        assert_eq!(field(body, 3).and_then(|i| varint(&i, 1)), Some(skyhook_id as u64), "skyhook(3)");
        assert_eq!(varint(body, 4), Some(expected_active), "active(4)");
    }
}
