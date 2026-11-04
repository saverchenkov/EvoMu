//! Quasar Asteroid Belts, Auth, Battlepass, Bloodlines, Blueprints, Bookmarks,
//! Bubbles, Calendar, Campaigns, and Capacitor parity integration suite.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    DynamicQuasarMessage, DynamicValue, EvePublicRequest, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX, WireMessage,
};
use eve_server::quasar_gateway::{
    NoticeScope, STATUS_OK, STATUS_UNAUTHORIZED,
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
        correlation_uuid: b"corr-quasar-suite".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-suite".to_vec(),
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
        _ => None,
    }
}

fn get_i64(msg: &DynamicQuasarMessage, field: u32) -> Option<i64> {
    match msg.get(field) {
        Some(DynamicValue::Sint64(v)) => Some(*v),
        Some(DynamicValue::Int64(v)) => Some(*v),
        Some(DynamicValue::Int32(v)) => Some(*v as i64),
        Some(DynamicValue::Sint32(v)) => Some(*v as i64),
        _ => None,
    }
}

fn get_str<'a>(msg: &'a DynamicQuasarMessage, field: u32) -> Option<&'a str> {
    match msg.get(field) {
        Some(DynamicValue::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn get_bool(msg: &DynamicQuasarMessage, field: u32) -> Option<bool> {
    match msg.get(field) {
        Some(DynamicValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

// parity: feat:quasar.asteroidbelt, feat:quasar.capacitor
#[tokio::test]
async fn test_quasar_asteroidbelt_and_capacitor() {
    eve_testkit::attest(&[
        "feat:quasar.asteroidbelt",
        "feat:quasar.capacitor",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Asteroid Belt descriptor verification (1 proto file)
    let ab_idx = reg
        .files_by_proto_name
        .get("eve/asteroidbelt/asteroidbelt.proto")
        .expect("asteroidbelt.proto");
    let ab_file = &reg.files[*ab_idx];
    assert_eq!(ab_file.package, "eve.asteroidbelt");
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.GetBeltsOfPlanetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.asteroidbelt.GetBeltsOfPlanetResponse"));
    assert_eq!(reg.files_by_domain.get("asteroidbelt").map(|v| v.len()), Some(1));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sess = make_session(101, 1001, 90_000_001, 1_000_001);

    // Call eve.asteroidbelt.GetRequest
    let req_val = encode_id_msg(1, 40_000_001);
    let rsp = quasar_call(&ctx, &sess, "eve.asteroidbelt.GetRequest", req_val);
    assert_eq!(rsp.status_code, STATUS_OK);
    let payload = rsp.payload.expect("payload");
    assert_eq!(payload.normalized_type_name(), "eve.asteroidbelt.GetResponse");
    let wm = WireMessage::decode(&payload.value).expect("decode GetResponse");
    let attr_bytes = wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let attr_wm = WireMessage::decode(attr_bytes).expect("decode Attributes");
    let name_field = attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert!(name_field.contains("Asteroid Belt #40000001"));

    // Call eve.asteroidbelt.GetBeltsOfPlanetRequest
    let planet_req = encode_id_msg(1, 40_000_100);
    let planet_rsp = quasar_call(&ctx, &sess, "eve.asteroidbelt.GetBeltsOfPlanetRequest", planet_req);
    assert_eq!(planet_rsp.status_code, STATUS_OK);
    let planet_payload = planet_rsp.payload.expect("payload");
    assert_eq!(planet_payload.normalized_type_name(), "eve.asteroidbelt.GetBeltsOfPlanetResponse");
    let planet_wm = WireMessage::decode(&planet_payload.value).expect("decode GetBeltsOfPlanetResponse");
    let belt_fields: Vec<_> = planet_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(belt_fields.len(), 2);

    // 2. Quasar Capacitor descriptor verification (1 proto file)
    let cap_idx = reg
        .files_by_proto_name
        .get("eve/capacitor/capacitor.proto")
        .expect("capacitor.proto");
    let cap_file = &reg.files[*cap_idx];
    assert_eq!(cap_file.package, "eve.capacitor");
    let val_desc = reg.get_message("eve.capacitor.Value").expect("Value message");
    assert_eq!(val_desc.fields.iter().find(|f| f.number == 1).map(|f| f.name.as_str()), Some("units"));
    assert_eq!(val_desc.fields.iter().find(|f| f.number == 2).map(|f| f.name.as_str()), Some("nanos"));
    assert_eq!(reg.files_by_domain.get("capacitor").map(|v| v.len()), Some(1));

    // Encode capacitor Value message: 4200 units, 500,000,000 nanos (= 4200.5 GJ)
    let mut cap_buf = Vec::new();
    encode_varint_field(1, 4200, &mut cap_buf);
    encode_varint_field(2, 500_000_000, &mut cap_buf);

    let dyn_cap = reg.decode_dynamic("eve.capacitor.Value", &cap_buf).expect("decode capacitor.Value");
    assert_eq!(get_u64(&dyn_cap, 1), Some(4200));
    assert_eq!(get_u64(&dyn_cap, 2), Some(500_000_000));

    // Verify ship capacitor attributes in SDE: Rifter (587) and dogma attribute 482
    let rifter_type = ctx.sde.types.get(&587).expect("Rifter type in SDE");
    assert_eq!(rifter_type.name, "Rifter");
    let cap_attr = ctx.sde.attributes.get(&482).expect("capacitorCapacity attribute in SDE");
    assert_eq!(cap_attr.name, "capacitorCapacity");
}

// parity: feat:quasar.auth, feat:quasar.bubble
#[tokio::test]
async fn test_quasar_auth_and_bubble() {
    eve_testkit::attest(&[
        "feat:quasar.auth",
        "feat:quasar.bubble",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Auth descriptor verification (4 proto files)
    let auth_files = [
        "eve/auth/api/events.proto",
        "eve_public/auth/auth.proto",
        "eve_public/auth/crypto/rsa.proto",
        "eve_public/auth/steam/steam.proto",
    ];
    for f in auth_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing auth proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("auth").map(|v| v.len()), Some(4));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // Provision account for auth test
    ctx.db.provision_player_account("quasar_pilot", "hunter2");

    let unauth_sess = SessionState::new(201);
    let mut authed_sess = make_session(202, 1002, 90_000_002, 1_000_001);
    ctx.dispatch_rpc(&mut authed_sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Call eve_public.auth.crypto.rsa.GetPublicKeyRequest
    let rsa_rsp = quasar_call(&ctx, &authed_sess, "eve_public.auth.crypto.rsa.GetPublicKeyRequest", Vec::new());
    assert_eq!(rsa_rsp.status_code, STATUS_OK);
    let rsa_payload = rsa_rsp.payload.expect("rsa payload");
    assert_eq!(rsa_payload.normalized_type_name(), "eve_public.auth.crypto.rsa.GetPublicKeyResponse");
    let rsa_wm = WireMessage::decode(&rsa_payload.value).expect("decode rsa");
    let pub_key = rsa_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(pub_key, "evomu-rsa-pub-v24");

    // Call eve_public.auth.AuthenticateUsernameRequest with valid credentials
    let mut enc = Vec::new();
    encode_bytes_field(2, b"hunter2", &mut enc);
    let mut creds_inner = Vec::new();
    encode_string_field(1, "quasar_pilot", &mut creds_inner);
    encode_bytes_field(2, &enc, &mut creds_inner);
    let mut auth_req = Vec::new();
    encode_bytes_field(1, &creds_inner, &mut auth_req);
    let auth_rsp = quasar_call(&ctx, &authed_sess, "eve_public.auth.AuthenticateUsernameRequest", auth_req);
    assert_eq!(auth_rsp.status_code, STATUS_OK);
    let auth_payload = auth_rsp.payload.expect("auth payload");
    assert_eq!(auth_payload.normalized_type_name(), "eve_public.auth.AuthenticateUsernameResponse");

    // Call eve_public.auth.AuthenticateUsernameRequest with invalid credentials
    let mut bad_enc = Vec::new();
    encode_bytes_field(2, b"wrong_password", &mut bad_enc);
    let mut bad_creds_inner = Vec::new();
    encode_string_field(1, "quasar_pilot", &mut bad_creds_inner);
    encode_bytes_field(2, &bad_enc, &mut bad_creds_inner);
    let mut bad_req = Vec::new();
    encode_bytes_field(1, &bad_creds_inner, &mut bad_req);
    let bad_rsp = quasar_call(&ctx, &authed_sess, "eve_public.auth.AuthenticateUsernameRequest", bad_req);
    assert_eq!(bad_rsp.status_code, STATUS_UNAUTHORIZED);

    // Steam auth check: unauthorized session returns 401
    let steam_unauth = quasar_call(&ctx, &unauth_sess, "eve_public.auth.steam.AuthenticateSteamRequest", Vec::new());
    assert_eq!(steam_unauth.status_code, STATUS_UNAUTHORIZED);

    // Publish auth event notice TwoFactorEnabled to User scope
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::User(authed_sess.user_id.unwrap() as i32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.auth.api.TwoFactorEnabled"),
            value: Vec::new(),
        },
        ctx.now_unix(),
    );
    let notices = poll_notice_names(&ctx, &authed_sess);
    assert!(notices.contains(&"eve.auth.api.TwoFactorEnabled".to_string()));

    // 2. Quasar Bubble descriptor verification (4 proto files)
    let bubble_files = [
        "eve/bubble/bubble.proto",
        "eve/bubble/presence/api/requests.proto",
        "eve/bubble/presence/presence.proto",
        "eve_public/bubble/bubble.proto",
    ];
    for f in bubble_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing bubble proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("bubble").map(|v| v.len()), Some(4));

    // Call eve.bubble.presence.api.GetInBubbleRequest
    let in_bubble_rsp = quasar_call(&ctx, &authed_sess, "eve.bubble.presence.api.GetInBubbleRequest", Vec::new());
    assert_eq!(in_bubble_rsp.status_code, STATUS_OK);
    let in_bubble_payload = in_bubble_rsp.payload.expect("bubble payload");
    assert_eq!(in_bubble_payload.normalized_type_name(), "eve.bubble.presence.api.GetInBubbleResponse");

    // Call eve.bubble.presence.api.GetInBubbleByCharacterRequest
    let by_char_req = encode_id_msg(1, 90_000_002);
    let by_char_rsp = quasar_call(&ctx, &authed_sess, "eve.bubble.presence.api.GetInBubbleByCharacterRequest", by_char_req);
    assert_eq!(by_char_rsp.status_code, STATUS_OK);
    let by_char_payload = by_char_rsp.payload.expect("by char payload");
    assert_eq!(by_char_payload.normalized_type_name(), "eve.bubble.presence.api.GetInBubbleByCharacterResponse");

    // Set bubble instance on subscriber and publish NoticeScope::BubbleInstance
    let bubble_uuid = b"bubble-inst-test".to_vec();
    if let Some(sub) = ctx.quasar_gateway.subscribers.write().unwrap().get_mut(&authed_sess.session_id) {
        sub.bubble_instance_uuid = Some(bubble_uuid.clone());
    }
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::BubbleInstance(bubble_uuid),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.bubble.presence.Member"),
            value: Vec::new(),
        },
        ctx.now_unix(),
    );
    let bubble_notices = poll_notice_names(&ctx, &authed_sess);
    assert!(bubble_notices.contains(&"eve.bubble.presence.Member".to_string()));
}

// parity: feat:quasar.battlepass, feat:quasar.campaign
#[tokio::test]
async fn test_quasar_battlepass_and_campaign() {
    eve_testkit::attest(&[
        "feat:quasar.battlepass",
        "feat:quasar.campaign",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Battlepass descriptor verification (6 proto files)
    let bp_files = [
        "eve/battlepass/battlepass.proto",
        "eve/battlepass/quest/quest.proto",
        "eve/battlepass/quest/user.proto",
        "eve/battlepass/season/reward_block.proto",
        "eve/battlepass/season/season.proto",
        "eve/battlepass/season/user.proto",
    ];
    for f in bp_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing battlepass proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("battlepass").map(|v| v.len()), Some(6));

    // Dynamic encode and decode battlepass quest Attributes
    let mut quest_attr = Vec::new();
    encode_varint_field(1, 50_000, &mut quest_attr); // max_progress
    encode_varint_field(2, 250, &mut quest_attr);    // reward_score
    let dyn_quest = reg.decode_dynamic("eve.battlepass.quest.Attributes", &quest_attr).expect("decode quest Attributes");
    assert_eq!(get_u64(&dyn_quest, 1), Some(50_000));
    assert_eq!(get_u64(&dyn_quest, 2), Some(250));

    // Dynamic encode and decode battlepass quest user ProgressAdded
    let mut prog_added = Vec::new();
    encode_varint_field(2, 500, &mut prog_added); // added
    encode_varint_field(3, 1500, &mut prog_added); // new_progress
    let dyn_prog = reg.decode_dynamic("eve.battlepass.quest.user.ProgressAdded", &prog_added).expect("decode ProgressAdded");
    assert_eq!(get_u64(&dyn_prog, 2), Some(500));
    assert_eq!(get_u64(&dyn_prog, 3), Some(1500));

    // Dynamic encode and decode battlepass season user ScoreAdded
    let mut score_added = Vec::new();
    encode_varint_field(2, 100, &mut score_added); // added
    encode_varint_field(3, 750, &mut score_added); // new_score
    let dyn_score = reg.decode_dynamic("eve.battlepass.season.user.ScoreAdded", &score_added).expect("decode ScoreAdded");
    assert_eq!(get_u64(&dyn_score, 2), Some(100));
    assert_eq!(get_u64(&dyn_score, 3), Some(750));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = make_session(301, 1003, 90_000_003, 1_000_001);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Publish battlepass event notice
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(sess.character_id.unwrap() as u32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.battlepass.quest.user.ProgressAdded"),
            value: prog_added,
        },
        ctx.now_unix(),
    );
    let bp_notices = poll_notice_names(&ctx, &sess);
    assert!(bp_notices.contains(&"eve.battlepass.quest.user.ProgressAdded".to_string()));

    // 2. Quasar Campaign descriptor verification (63 proto files)
    assert_eq!(reg.files_by_domain.get("campaign").map(|v| v.len()), Some(63));

    // Call eve_public.campaign.api.GetAllRequest
    let all_camp_rsp = quasar_call(&ctx, &sess, "eve_public.campaign.api.GetAllRequest", Vec::new());
    assert_eq!(all_camp_rsp.status_code, STATUS_OK);
    let all_camp_payload = all_camp_rsp.payload.expect("all campaigns payload");
    assert_eq!(all_camp_payload.normalized_type_name(), "eve_public.campaign.api.GetAllResponse");

    // Call eve_public.campaign.contributionmethod.definition.api.GetAllRequest
    let cm_rsp = quasar_call(&ctx, &sess, "eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new());
    assert_eq!(cm_rsp.status_code, STATUS_OK);
    let cm_payload = cm_rsp.payload.expect("contribution method payload");
    assert_eq!(cm_payload.normalized_type_name(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse");
    let cm_wm = WireMessage::decode(&cm_payload.value).expect("decode cm response");
    assert!(!cm_wm.fields.is_empty(), "campaign contribution methods must be populated");

    // Call eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest
    let em_rsp = quasar_call(&ctx, &sess, "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new());
    assert_eq!(em_rsp.status_code, STATUS_OK);
    let em_payload = em_rsp.payload.expect("event match payload");
    assert_eq!(em_payload.normalized_type_name(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse");

    // Publish campaign notice StartedNotice
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(sess.character_id.unwrap() as u32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.campaign.api.StartedNotice"),
            value: Vec::new(),
        },
        ctx.now_unix(),
    );
    let camp_notices = poll_notice_names(&ctx, &sess);
    assert!(camp_notices.contains(&"eve_public.campaign.api.StartedNotice".to_string()));
}

// parity: feat:quasar.bloodline, feat:quasar.blueprint
#[tokio::test]
async fn test_quasar_bloodline_and_blueprint() {
    eve_testkit::attest(&[
        "feat:quasar.bloodline",
        "feat:quasar.blueprint",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Bloodline descriptor verification (1 proto file)
    assert_eq!(reg.files_by_domain.get("bloodline").map(|v| v.len()), Some(1));
    assert!(reg.messages_by_full_name.contains_key("eve.bloodline.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.bloodline.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.bloodline.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.bloodline.GetAllResponse"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sess = make_session(401, 1004, 90_000_004, 1_000_001);

    // Call eve.bloodline.GetAllRequest
    let bl_rsp = quasar_call(&ctx, &sess, "eve.bloodline.GetAllRequest", Vec::new());
    assert_eq!(bl_rsp.status_code, STATUS_OK);
    let bl_payload = bl_rsp.payload.expect("bloodline payload");
    assert_eq!(bl_payload.normalized_type_name(), "eve.bloodline.GetAllResponse");
    let bl_wm = WireMessage::decode(&bl_payload.value).expect("decode bloodlines");
    assert_eq!(
        bl_wm.fields.len(),
        ctx.sde.bloodlines.len(),
        "Bloodlines count must match SDE bloodlines"
    );

    // Verify individual bloodline attributes (Deteis: id=1, race=1)
    let deteis = ctx.sde.bloodlines.get(&1).expect("Deteis in SDE");
    assert_eq!(deteis.name, "Deteis");
    assert_eq!(deteis.race_id, 1);
    assert!(deteis.charisma > 0);
    assert!(deteis.intelligence > 0);

    // 2. Quasar Blueprint descriptor verification (2 proto files)
    let bp_files = [
        "eve/blueprint/blueprint.proto",
        "eve/blueprint/blueprint_type.proto",
    ];
    for f in bp_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing blueprint proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("blueprint").map(|v| v.len()), Some(2));

    let bp_attr_desc = reg.get_message("eve.blueprint.Attributes").expect("blueprint Attributes");
    assert_eq!(bp_attr_desc.fields.iter().find(|f| f.number == 1).map(|f| f.name.as_str()), Some("type"));
    assert_eq!(bp_attr_desc.fields.iter().find(|f| f.number == 2).map(|f| f.name.as_str()), Some("blueprint_original"));
    assert_eq!(bp_attr_desc.fields.iter().find(|f| f.number == 3).map(|f| f.name.as_str()), Some("material_efficiency"));
    assert_eq!(bp_attr_desc.fields.iter().find(|f| f.number == 4).map(|f| f.name.as_str()), Some("time_efficiency"));
    assert_eq!(bp_attr_desc.fields.iter().find(|f| f.number == 5).map(|f| f.name.as_str()), Some("runs_remaining"));

    // Dynamic encode and decode blueprint Attributes (BPO ME 10, TE 20, infinite runs)
    let mut bp_buf = Vec::new();
    encode_id_msg(1, 687); // Rifter Blueprint
    let mut bpo_entry = Vec::new();
    encode_id_msg(1, 687);
    encode_varint_field(2, 1, &mut bpo_entry); // blueprint_original = true
    encode_varint_field(3, 10, &mut bpo_entry); // ME = 10
    encode_varint_field(4, 20, &mut bpo_entry); // TE = 20
    encode_varint_field(5, encode_zigzag64(-1), &mut bpo_entry); // infinite runs
    bp_buf.extend_from_slice(&bpo_entry);

    let dyn_bp = reg.decode_dynamic("eve.blueprint.Attributes", &bp_buf).expect("decode blueprint.Attributes");
    assert_eq!(get_bool(&dyn_bp, 2), Some(true));
    assert_eq!(get_u64(&dyn_bp, 3), Some(10));
    assert_eq!(get_u64(&dyn_bp, 4), Some(20));
    assert_eq!(get_i64(&dyn_bp, 5), Some(-1));

    // Call eve.character.industry.blueprints.GetRequest
    let ind_bp_rsp = quasar_call(&ctx, &sess, "eve.character.industry.blueprints.GetRequest", Vec::new());
    assert_eq!(ind_bp_rsp.status_code, STATUS_OK);
    let ind_payload = ind_bp_rsp.payload.expect("payload");
    assert_eq!(ind_payload.normalized_type_name(), "eve.character.industry.blueprints.GetResponse");
}

// parity: feat:quasar.bookmark, feat:quasar.calendar
#[tokio::test]
async fn test_quasar_bookmark_and_calendar() {
    eve_testkit::attest(&[
        "feat:quasar.bookmark",
        "feat:quasar.calendar",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Bookmark descriptor verification (3 proto files)
    let bm_files = [
        "eve/bookmark/bookmark.proto",
        "eve/bookmark/folder.proto",
        "eve/bookmark/subfolder.proto",
    ];
    for f in bm_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing bookmark proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("bookmark").map(|v| v.len()), Some(3));

    // Dynamic encode and decode eve.bookmark.Attributes
    let mut bm_attr = Vec::new();
    encode_string_field(1, "Insta-Undock Perch 1500km", &mut bm_attr);
    encode_string_field(2, "Station align tactical bookmark", &mut bm_attr);
    encode_varint_field(4, 1, &mut bm_attr); // no_expiry = true

    let dyn_bm = reg.decode_dynamic("eve.bookmark.Attributes", &bm_attr).expect("decode bookmark Attributes");
    assert_eq!(get_str(&dyn_bm, 1), Some("Insta-Undock Perch 1500km"));
    assert_eq!(get_str(&dyn_bm, 2), Some("Station align tactical bookmark"));
    assert_eq!(get_bool(&dyn_bm, 4), Some(true));

    // Dynamic encode and decode eve.bookmark.folder.Attributes
    let mut folder_attr = Vec::new();
    encode_string_field(1, "Corporate Tactical Bookmarks", &mut folder_attr);
    encode_string_field(2, "War room shared pings", &mut folder_attr);
    let dyn_folder = reg.decode_dynamic("eve.bookmark.folder.Attributes", &folder_attr).expect("decode folder Attributes");
    assert_eq!(get_str(&dyn_folder, 1), Some("Corporate Tactical Bookmarks"));
    assert_eq!(get_str(&dyn_folder, 2), Some("War room shared pings"));

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = make_session(501, 1005, 90_000_005, 1_000_001);
    ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
        .await
        .expect("Subscribe session");

    // Publish bookmark notice to Character scope
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(sess.character_id.unwrap() as u32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.bookmark.Added"),
            value: Vec::new(),
        },
        ctx.now_unix(),
    );
    let bm_notices = poll_notice_names(&ctx, &sess);
    assert!(bm_notices.contains(&"eve_public.app.eveonline.agency.ui.bookmark.Added".to_string()));

    // 2. Quasar Calendar descriptor verification (5 proto files)
    let cal_files = [
        "eve/calendar/alliance_event.proto",
        "eve/calendar/calendar_event.proto",
        "eve/calendar/corporation_event.proto",
        "eve/calendar/global_event.proto",
        "eve/calendar/personal_event.proto",
    ];
    for f in cal_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing calendar proto file {f}");
    }
    assert_eq!(reg.files_by_domain.get("calendar").map(|v| v.len()), Some(5));

    // Call eve.calendar.personal_event.GetRequest
    let p_req = encode_id_msg(1, 10);
    let p_rsp = quasar_call(&ctx, &sess, "eve.calendar.personal_event.GetRequest", p_req);
    assert_eq!(p_rsp.status_code, STATUS_OK);
    let p_payload = p_rsp.payload.expect("personal payload");
    assert_eq!(p_payload.normalized_type_name(), "eve.calendar.personal_event.GetResponse");
    let p_wm = WireMessage::decode(&p_payload.value).expect("decode personal event");
    let p_attr_bytes = p_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let p_attr_wm = WireMessage::decode(p_attr_bytes).expect("decode personal attributes");
    let p_name = p_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(p_name, "Mining Operation");

    // Call eve.calendar.corporation_event.GetRequest
    let c_req = encode_id_msg(1, 20);
    let c_rsp = quasar_call(&ctx, &sess, "eve.calendar.corporation_event.GetRequest", c_req);
    assert_eq!(c_rsp.status_code, STATUS_OK);
    let c_payload = c_rsp.payload.expect("corp payload");
    assert_eq!(c_payload.normalized_type_name(), "eve.calendar.corporation_event.GetResponse");
    let c_wm = WireMessage::decode(&c_payload.value).expect("decode corp event");
    let c_attr_bytes = c_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let c_attr_wm = WireMessage::decode(c_attr_bytes).expect("decode corp attributes");
    let c_name = c_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(c_name, "Strategic Citadel Defense");

    // Call eve.calendar.alliance_event.GetRequest
    let a_req = encode_id_msg(1, 30);
    let a_rsp = quasar_call(&ctx, &sess, "eve.calendar.alliance_event.GetRequest", a_req);
    assert_eq!(a_rsp.status_code, STATUS_OK);
    let a_payload = a_rsp.payload.expect("alliance payload");
    assert_eq!(a_payload.normalized_type_name(), "eve.calendar.alliance_event.GetResponse");
    let a_wm = WireMessage::decode(&a_payload.value).expect("decode alliance event");
    let a_attr_bytes = a_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let a_attr_wm = WireMessage::decode(a_attr_bytes).expect("decode alliance attributes");
    let a_name = a_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(a_name, "Coalition Meeting");

    // Publish calendar event notice to Character scope
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(sess.character_id.unwrap() as u32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.calendar.personal_event.Created"),
            value: Vec::new(),
        },
        ctx.now_unix(),
    );
    let cal_notices = poll_notice_names(&ctx, &sess);
    assert!(cal_notices.contains(&"eve.calendar.personal_event.Created".to_string()));
}
