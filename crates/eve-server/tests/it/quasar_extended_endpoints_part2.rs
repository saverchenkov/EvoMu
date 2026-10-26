//! Integration tests for Extended Quasar Endpoints across Community Tracks
//! and Campaigns (Part 2) (Phase Q, tasks `Q-5` and `Q-6`).

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

    let (mut req_body, mut id_bytes) = (Vec::new(), Vec::new());
    encode_varint_field(1, 1, &mut id_bytes);
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
