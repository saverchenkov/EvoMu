//! Quasar NPC, Omega, Ore, Pirate Stronghold, Planet, Planet Interaction,
//! Presence, Probe, Project Discovery, and PvP parity integration suite.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field,
    DynamicQuasarMessage, DynamicValue, EvePublicRequest, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX, WireMessage,
};
use eve_server::quasar_gateway::{
    NoticeScope, STATUS_FORBIDDEN, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;

fn make_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30_000_142);
    s.role = eve_machonet::service_roles::ROLE_PLAYER;
    s.corp_role = 0;
    s
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> eve_proto::quasar::EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-quasar-npc-pvp".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-npc-pvp".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now)
}

fn encode_id_msg(field_num: u32, id: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn encode_typed_id_req(ctx: &ServerContext, type_name: &str, id: u64) -> Vec<u8> {
    eve_server::services::quasar_space_services::encode_domain_message(
        &ctx.quasar_gateway.descriptors,
        type_name,
        |reg, req| {
            eve_server::services::quasar_space_services::set_domain_field(reg, req, 1, id, "");
        },
    )
    .expect("encode typed id req")
    .value
}

fn poll_notice_names(ctx: &ServerContext, sess: &SessionState) -> Vec<String> {
    ctx.quasar_gateway
        .poll_notices(sess.session_id)
        .into_iter()
        .filter_map(|n| n.payload.map(|p| p.normalized_type_name().to_string()))
        .collect()
}

fn get_u64(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field) {
        Some(DynamicValue::Uint64(v)) => Some(*v),
        Some(DynamicValue::Uint32(v)) => Some(*v as u64),
        Some(DynamicValue::Int64(v)) => Some(*v as u64),
        Some(DynamicValue::Int32(v)) => Some(*v as u64),
        Some(DynamicValue::Enum(v)) => Some(*v as u64),
        _ => None,
    }
}

// parity: feat:quasar.npc, feat:quasar.omega
#[tokio::test]
async fn test_quasar_npc_and_omega() {
    eve_testkit::attest(&[
        "feat:quasar.npc",
        "feat:quasar.omega",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar NPC descriptor verification (7 proto files)
    let npc_files = [
        "eve/npc/barter.proto",
        "eve/npc/event.proto",
        "eve/npc/loot/api/events.proto",
        "eve/npc/loot/source_type.proto",
        "eve/npc/ship/api/event.proto",
        "eve/npc/ship/api/requests.proto",
        "eve/npc/ship/ship.proto",
    ];
    for f in npc_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing npc proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("npc").map(|v| v.len()), Some(7));
    assert!(reg.messages_by_full_name.contains_key("eve.npc.ship.api.GetOwnerRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.npc.ship.api.GetOwnerResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.npc.event.BountyPaid"));
    assert!(reg.messages_by_full_name.contains_key("eve.npc.loot.api.Spawned"));
    assert!(reg.messages_by_full_name.contains_key("eve.npc.ship.api.Exploded"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut sess = make_session(1001, 101, 90_000_101, 1_000_101);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Route eve.npc.ship.api.GetOwnerRequest
    let req_val = encode_typed_id_req(&ctx, "eve.npc.ship.api.GetOwnerRequest", 600_001);
    let rsp = quasar_call(&ctx, &sess, "eve.npc.ship.api.GetOwnerRequest", req_val);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload");
    assert_eq!(payload.normalized_type_name(), "eve.npc.ship.api.GetOwnerResponse");

    // Publish NoticeScope::Character eve.npc.event.BountyPaid
    let mut bounty_buf = Vec::new();
    encode_varint_field(1, 90_000_101, &mut bounty_buf); // recipient
    encode_varint_field(2, 500_001, &mut bounty_buf);     // target_faction
    encode_varint_field(3, 250_000, &mut bounty_buf);     // net_income
    encode_varint_field(4, 30_000_142, &mut bounty_buf);  // solar_system

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(90_000_101),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.npc.event.BountyPaid"),
            value: bounty_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.npc.event.BountyPaid".to_string()));

    // 2. Quasar Omega descriptor verification (1 proto file)
    let omega_file = reg.files_by_proto_name.get("eve_public/omega/benefit.proto").expect("benefit.proto");
    assert_eq!(reg.files[*omega_file].package, "eve_public.omega.benefit");
    assert!(reg.messages_by_full_name.contains_key("eve_public.omega.benefit.Identifier"));
    assert!(reg.enums_by_full_name.contains_key("eve_public.omega.benefit.Type"));

    // Verify Omega BenefitType enum and dynamic message encoding
    // TYPE_SKILLS_FASTER_TRAINING = 6, TYPE_SHIPS_FULL_ACCESS = 9
    let mut benefit_buf = Vec::new();
    encode_varint_field(1, 6, &mut benefit_buf); // type = 6
    let dyn_ben = reg.decode_dynamic("eve_public.omega.benefit.Identifier", &benefit_buf)
        .expect("decode omega.benefit.Identifier");
    assert_eq!(get_u64(&dyn_ben, 1), Some(6));

    // Verify SDE clone states and subscription attributes
    let alpha_rate = ctx.sde.attributes.get(&2161); // baseSkillAcceleration or cloneState
    assert!(alpha_rate.is_some() || ctx.sde.types.contains_key(&3363));
}

// parity: feat:quasar.ore, feat:quasar.piratestronghold
#[tokio::test]
async fn test_quasar_ore_and_piratestronghold() {
    eve_testkit::attest(&[
        "feat:quasar.ore",
        "feat:quasar.piratestronghold",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Ore descriptor verification (5 proto files)
    let ore_files = [
        "eve/ore/group.proto",
        "eve/ore/ore.proto",
        "eve/ore/ore_type.proto",
        "eve_public/ore/group.proto",
        "eve_public/ore/ore_type.proto",
    ];
    for f in ore_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing ore proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("ore").map(|v| v.len()), Some(5));
    assert!(reg.messages_by_full_name.contains_key("eve.ore.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.ore.group.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.ore.oretype.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ore.group.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.ore.oretype.Identifier"));

    // Dynamic encode eve.ore.oretype.Identifier (Veldspar type 1230, group 18)
    let mut grp_buf = Vec::new();
    encode_varint_field(1, 18, &mut grp_buf);
    let mut ore_buf = Vec::new();
    encode_varint_field(1, 1230, &mut ore_buf);
    encode_bytes_field(2, &grp_buf, &mut ore_buf);

    let dyn_ore = reg.decode_dynamic("eve.ore.oretype.Identifier", &ore_buf).expect("decode oretype");
    assert_eq!(get_u64(&dyn_ore, 1), Some(1230));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // Verify Veldspar in SDE
    let veldspar = ctx.sde.types.get(&1230).expect("Veldspar in SDE");
    assert_eq!(veldspar.name, "Veldspar");

    // 2. Quasar Pirate Stronghold descriptor verification (2 proto files)
    let ps_files = [
        "eve/piratestronghold/event.proto",
        "eve/piratestronghold/piratestronghold_type.proto",
    ];
    for f in ps_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing piratestronghold proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("piratestronghold").map(|v| v.len()), Some(2));
    assert!(reg.messages_by_full_name.contains_key("eve.piratestronghold.event.DestructionParticipated"));
    assert!(reg.messages_by_full_name.contains_key("eve.piratestronghold.event.RewardReceived"));
    assert!(reg.messages_by_full_name.contains_key("eve.piratestronghold.piratestrongholdtype.Identifier"));

    let mut sess = make_session(1002, 102, 90_000_102, 1_000_102);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Publish NoticeScope::Character eve.piratestronghold.event.RewardReceived
    let mut rew_buf = Vec::new();
    encode_varint_field(1, 90_000_102, &mut rew_buf); // character
    encode_varint_field(2, 45_001, &mut rew_buf);     // stronghold type
    encode_varint_field(3, 30_000_000, &mut rew_buf); // isk_received

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(90_000_102),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.piratestronghold.event.RewardReceived"),
            value: rew_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.piratestronghold.event.RewardReceived".to_string()));

    // Unauthenticated call rejected
    let unauth = SessionState::new(999);
    let unauth_rsp = quasar_call(&ctx, &unauth, "eve.npc.ship.api.GetOwnerRequest", Vec::new());
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);
}

// parity: feat:quasar.planet, feat:quasar.planetinteraction
#[tokio::test]
async fn test_quasar_planet_and_planetinteraction() {
    eve_testkit::attest(&[
        "feat:quasar.planet",
        "feat:quasar.planetinteraction",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Planet descriptor verification (11 proto files)
    let planet_files = [
        "eve/planet/pin.proto",
        "eve/planet/pin_type.proto",
        "eve/planet/planet.proto",
        "eve/planet/planet_type.proto",
        "eve/planet/skyhook/admin/api/requests.proto",
        "eve/planet/skyhook/api/events.proto",
        "eve/planet/skyhook/api/requests.proto",
        "eve/planet/skyhook/skyhook.proto",
        "eve/planet/skyhook/state.proto",
        "eve_public/planet/planet.proto",
        "eve_public/planet/skyhook/skyhook.proto",
    ];
    for f in planet_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing planet proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("planet").map(|v| v.len()), Some(11));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sess = make_session(1003, 103, 90_000_103, 1_000_103);

    // Call eve.planet.GetRequest
    let req_val = encode_id_msg(1, 40_000_001);
    let rsp = quasar_call(&ctx, &sess, "eve.planet.GetRequest", req_val);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload");
    assert_eq!(payload.normalized_type_name(), "eve.planet.GetResponse");
    let wm = WireMessage::decode(&payload.value).expect("decode GetResponse");
    let attr_bytes = wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let attr_wm = WireMessage::decode(attr_bytes).expect("decode Attributes");
    let name_field = attr_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(name_field, "Planet #40000001");

    // Call eve.planet.GetPlanetsInSolarSystemRequest
    let sys_req = encode_id_msg(1, 30_000_142);
    let sys_rsp = quasar_call(&ctx, &sess, "eve.planet.GetPlanetsInSolarSystemRequest", sys_req);
    assert_eq!(sys_rsp.status_code, STATUS_OK);
    let sys_payload = sys_rsp.payload.expect("sys payload");
    assert_eq!(sys_payload.normalized_type_name(), "eve.planet.GetPlanetsInSolarSystemResponse");
    let sys_wm = WireMessage::decode(&sys_payload.value).expect("decode GetPlanetsInSolarSystemResponse");
    let planet_count = sys_wm.fields.iter().filter(|f| f.field_number == 1).count();
    assert_eq!(planet_count, 8);

    // Admin skyhook request denied for regular player
    let skyhook_rsp = quasar_call(&ctx, &sess, "eve.planet.skyhook.admin.api.ChangeOwnerRequest", Vec::new());
    assert_eq!(skyhook_rsp.status_code, STATUS_FORBIDDEN);

    // 2. Quasar Planet Interaction descriptor verification (8 proto files)
    let pi_files = [
        "eve/planetinteraction/collector/collector.proto",
        "eve/planetinteraction/colony/colony.proto",
        "eve/planetinteraction/factory/factory.proto",
        "eve/planetinteraction/link/link.proto",
        "eve/planetinteraction/pin/pin.proto",
        "eve/planetinteraction/route/route.proto",
        "eve/planetinteraction/schematic/schematic.proto",
        "eve/planetinteraction/storage/storage.proto",
    ];
    for f in pi_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing pi proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("planetinteraction").map(|v| v.len()), Some(8));

    // Call eve.planetinteraction.collector.GetRequest
    let col_rsp = quasar_call(&ctx, &sess, "eve.planetinteraction.collector.GetRequest", encode_id_msg(1, 501));
    assert_eq!(col_rsp.status_code, STATUS_OK);
    assert_eq!(col_rsp.payload.unwrap().normalized_type_name(), "eve.planetinteraction.collector.GetResponse");

    // Call eve.planetinteraction.factory.GetRequest
    let fact_rsp = quasar_call(&ctx, &sess, "eve.planetinteraction.factory.GetRequest", encode_id_msg(1, 502));
    assert_eq!(fact_rsp.status_code, STATUS_OK);
    assert_eq!(fact_rsp.payload.unwrap().normalized_type_name(), "eve.planetinteraction.factory.GetResponse");

    // Call eve.planetinteraction.schematic.GetRequest
    let sch_rsp = quasar_call(&ctx, &sess, "eve.planetinteraction.schematic.GetRequest", encode_id_msg(1, 70));
    assert_eq!(sch_rsp.status_code, STATUS_OK);
    assert_eq!(sch_rsp.payload.unwrap().normalized_type_name(), "eve.planetinteraction.schematic.GetResponse");

    // Call eve.planetinteraction.storage.GetRequest
    let stor_rsp = quasar_call(&ctx, &sess, "eve.planetinteraction.storage.GetRequest", encode_id_msg(1, 503));
    assert_eq!(stor_rsp.status_code, STATUS_OK);
    assert_eq!(stor_rsp.payload.unwrap().normalized_type_name(), "eve.planetinteraction.storage.GetResponse");
}

// parity: feat:quasar.presence, feat:quasar.probe
#[tokio::test]
async fn test_quasar_presence_and_probe() {
    eve_testkit::attest(&[
        "feat:quasar.presence",
        "feat:quasar.probe",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Presence descriptor verification (1 proto file)
    let pres_file = reg.files_by_proto_name.get("eve_public/presence/api/request.proto").expect("presence request.proto");
    assert_eq!(reg.files[*pres_file].package, "eve_public.presence.api");
    assert_eq!(reg.files_by_domain.get("presence").map(|v| v.len()), Some(1));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetCorporationRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetCorporationResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetAllianceRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetAllianceResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetFactionRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.presence.api.GetFactionResponse"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_104u32;
    let corp_id = 1_000_104u32;
    let alliance_id = 99_000_104i64;
    let faction_id = 500_001i64;

    // Seed character in DB with corp, alliance, faction
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().unwrap();
        let ch = eve_db::CharacterRecord {
            character_id: char_id as i64,
            corporation_id: corp_id as i64,
            alliance_id: Some(alliance_id),
            war_faction_id: Some(faction_id),
            ..base_char
        };
        snap.characters.insert(char_id as i64, ch);
    });

    let sess = make_session(1004, 104, char_id, corp_id);

    // Call eve_public.presence.api.GetCorporationRequest
    let corp_req = encode_id_msg(1, char_id as u64);
    let corp_rsp = quasar_call(&ctx, &sess, "eve_public.presence.api.GetCorporationRequest", corp_req);
    assert_eq!(corp_rsp.status_code, STATUS_OK);
    let corp_payload = corp_rsp.payload.expect("corp payload");
    assert_eq!(corp_payload.normalized_type_name(), "eve_public.presence.api.GetCorporationResponse");
    let corp_wm = WireMessage::decode(&corp_payload.value).expect("decode GetCorporationResponse");
    let id_bytes = corp_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let id_wm = WireMessage::decode(id_bytes).expect("decode id");
    assert_eq!(id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint(), Some(corp_id as u64));

    // Call eve_public.presence.api.GetAllianceRequest
    let all_req = encode_id_msg(1, char_id as u64);
    let all_rsp = quasar_call(&ctx, &sess, "eve_public.presence.api.GetAllianceRequest", all_req);
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let all_payload = all_rsp.payload.expect("alliance payload");
    assert_eq!(all_payload.normalized_type_name(), "eve_public.presence.api.GetAllianceResponse");

    // Call eve_public.presence.api.GetFactionRequest
    let fac_req = encode_id_msg(1, char_id as u64);
    let fac_rsp = quasar_call(&ctx, &sess, "eve_public.presence.api.GetFactionRequest", fac_req);
    assert_eq!(fac_rsp.status_code, STATUS_OK);
    let fac_payload = fac_rsp.payload.expect("faction payload");
    assert_eq!(fac_payload.normalized_type_name(), "eve_public.presence.api.GetFactionResponse");

    // 2. Quasar Probe descriptor verification (2 proto files)
    let probe_files = [
        "eve/probe/event.proto",
        "eve/probe/exploration_scanresult.proto",
    ];
    for f in probe_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing probe proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("probe").map(|v| v.len()), Some(2));
    assert!(reg.messages_by_full_name.contains_key("eve.probe.event.ExplorationSignatureScanned"));
    assert!(reg.messages_by_full_name.contains_key("eve.probe.explorationscanresult.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.probe.explorationscanresult.Attributes"));

    let mut sub_sess = make_session(1005, 105, 90_000_105, 1_000_105);
    ctx.dispatch_rpc(&mut sub_sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe sub_sess");

    // Publish NoticeScope::SolarSystem eve.probe.event.ExplorationSignatureScanned
    let mut probe_buf = Vec::new();
    encode_varint_field(1, char_id as u64, &mut probe_buf); // character
    encode_varint_field(2, 901, &mut probe_buf);            // scan_result
    encode_varint_field(3, 4, &mut probe_buf);              // archetype Relic
    encode_varint_field(4, 2, &mut probe_buf);              // signature_type
    encode_varint_field(5, 30_000_142, &mut probe_buf);     // solar_system

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.probe.event.ExplorationSignatureScanned"),
            value: probe_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sub_sess);
    assert!(notices.contains(&"eve.probe.event.ExplorationSignatureScanned".to_string()));
}

// parity: feat:quasar.projectdiscovery, feat:quasar.pvp
#[tokio::test]
async fn test_quasar_projectdiscovery_and_pvp() {
    eve_testkit::attest(&[
        "feat:quasar.projectdiscovery",
        "feat:quasar.pvp",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Project Discovery descriptor verification (1 proto file, 10 messages)
    let pd_file = reg.files_by_proto_name.get("eve/projectdiscovery/event.proto").expect("projectdiscovery event.proto");
    assert_eq!(reg.files[*pd_file].package, "eve.projectdiscovery.event");
    assert_eq!(reg.files_by_domain.get("projectdiscovery").map(|v| v.len()), Some(1));
    assert!(reg.messages_by_full_name.contains_key("eve.projectdiscovery.event.TaskFetched"));
    assert!(reg.messages_by_full_name.contains_key("eve.projectdiscovery.event.RankIncreased"));
    assert!(reg.messages_by_full_name.contains_key("eve.projectdiscovery.event.AnalysisSubmitted"));
    assert!(reg.messages_by_full_name.contains_key("eve.projectdiscovery.event.LootBoxGiven"));
    assert!(reg.messages_by_full_name.contains_key("eve.projectdiscovery.event.AnalysisKreditsGiven"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_106u32;
    let mut sess = make_session(1006, 106, char_id, 1_000_106);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Publish NoticeScope::Character eve.projectdiscovery.event.RankIncreased
    let mut pd_buf = Vec::new();
    encode_varint_field(1, char_id as u64, &mut pd_buf);

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(char_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.projectdiscovery.event.RankIncreased"),
            value: pd_buf,
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.contains(&"eve.projectdiscovery.event.RankIncreased".to_string()));

    // 2. Quasar PvP descriptor verification (6 proto files)
    let pvp_files = [
        "eve/pvp/provinggrounds/matchtype.proto",
        "eve/pvp/provinggrounds/provinggrounds.proto",
        "eve/pvp/provinggrounds/schedule.proto",
        "eve/pvp/pvp.proto",
        "eve/pvp/ship/api.proto",
        "eve/pvp/structure/api/events.proto",
    ];
    for f in pvp_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing pvp proto {f}");
    }
    assert_eq!(reg.files_by_domain.get("pvp").map(|v| v.len()), Some(6));
    assert!(reg.messages_by_full_name.contains_key("eve.pvp.ship.api.Destroyed"));
    assert!(reg.messages_by_full_name.contains_key("eve.pvp.ship.api.Damaged"));
    assert!(reg.messages_by_full_name.contains_key("eve.pvp.structure.api.Damaged"));
    assert!(reg.messages_by_full_name.contains_key("eve.pvp.provinggrounds.CharacterStatistics"));
    assert!(reg.messages_by_full_name.contains_key("eve.pvp.provinggrounds.LeaderboardEntry"));

    // Dynamic encode and decode provinggrounds CharacterStatistics
    let mut stats_inner = Vec::new();
    let mut char_inner = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_inner);
    encode_bytes_field(1, &char_inner, &mut stats_inner);
    encode_varint_field(2, 1, &mut stats_inner);              // rank = 1
    encode_varint_field(3, 15, &mut stats_inner);             // wins = 15
    encode_varint_field(4, 2, &mut stats_inner);              // losses = 2
    encode_varint_field(5, 0, &mut stats_inner);              // draws = 0

    let dyn_stats = reg.decode_dynamic("eve.pvp.provinggrounds.CharacterStatistics", &stats_inner)
        .expect("decode CharacterStatistics");
    assert_eq!(get_u64(&dyn_stats, 2), Some(1));
    assert_eq!(get_u64(&dyn_stats, 3), Some(15));
    assert_eq!(get_u64(&dyn_stats, 4), Some(2));

    // Publish NoticeScope::SolarSystem eve.pvp.ship.api.Destroyed
    let mut pvp_dest_buf = Vec::new();
    encode_varint_field(1, char_id as u64, &mut pvp_dest_buf);  // destroyer
    encode_varint_field(2, 90_000_999, &mut pvp_dest_buf);      // destroyed
    encode_varint_field(4, 30_000_142, &mut pvp_dest_buf);      // solar_system
    encode_varint_field(9, 1, &mut pvp_dest_buf);               // executioner_player = true

    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::SolarSystem(30_000_142),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pvp.ship.api.Destroyed"),
            value: pvp_dest_buf,
        },
        ctx.now_unix(),
    );
    let notices2 = poll_notice_names(&ctx, &sess);
    assert!(notices2.contains(&"eve.pvp.ship.api.Destroyed".to_string()));
}
