//! Integration tests for Quasar ThirdPartyApi, Tournament, UI, Universe, Vanguard,
//! Vendors, War, Wormhole, and Serverops Config-File services.

use eve_db::{CorporationRecord, WarRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::config::ServerConfig;
use eve_server::quasar_gateway::NoticeScope;
use eve_server::router::ServerContext;
use eve_server::services::quasar_vendors_and_war_services::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static CORR_SEQ: AtomicU64 = AtomicU64::new(100);

fn seed_test_fixtures(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
            ch.home_station_id = 60_003_760;
        }
    });
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMH;
    sess
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    encode_bytes_field(field_num, &inner, out);
}

fn encode_uuid_field(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut inner = Vec::new();
    encode_bytes_field(1, uuid, &mut inner);
    encode_bytes_field(field_num, &inner, out);
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-vw-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-vw".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
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

async fn subscribe_to_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
    notice_types: &[&str],
) {
    let args: Vec<PyRep> = notice_types
        .iter()
        .map(|s| PyRep::String(s.to_string()))
        .collect();
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "publicGatewaySvc",
        None,
        "Subscribe",
        &args,
        &kw,
        None,
    )
    .await
    .expect("Subscribe");
}

fn poll_notices(
    ctx: &ServerContext,
    sess: &SessionState,
) -> Vec<(String, Vec<u8>)> {
    ctx.quasar_gateway
        .poll_notices(sess.session_id)
        .into_iter()
        .filter_map(|n| n.payload.map(|any| (any.type_url, any.value)))
        .collect()
}

#[tokio::test]
async fn test_quasar_thirdpartyapi_tournament_and_ui() {
    // parity: feat:quasar.thirdpartyapi, quasar:eveProto/generated/eve/thirdpartyapi/events_pb2.pyj
    // parity: feat:quasar.tournament, quasar:eveProto/generated/eve/tournament/tournament_pb2.pyj, quasar:eveProto/generated/eve/tournament/arena/api/requests_pb2.pyj, quasar:eveProto/generated/eve/tournament/ruleset/ruleset_pb2.pyj
    // parity: feat:quasar.ui, quasar:eveProto/generated/eve_public/ui/icon_pb2.pyj, quasar:eveProto/generated/eve_public/ui/annotation_pb2.pyj, quasar:eveProto/generated/eve_public/ui/image_pb2.pyj, quasar:eveProto/generated/eve_public/ui/contenttag_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.thirdpartyapi",
        "quasar:eveProto/generated/eve/thirdpartyapi/events_pb2.pyj",
        "feat:quasar.tournament",
        "quasar:eveProto/generated/eve/tournament/tournament_pb2.pyj",
        "quasar:eveProto/generated/eve/tournament/arena/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve/tournament/ruleset/ruleset_pb2.pyj",
        "feat:quasar.ui",
        "quasar:eveProto/generated/eve_public/ui/icon_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/annotation_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/image_pb2.pyj",
        "quasar:eveProto/generated/eve_public/ui/contenttag_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(201, 1001, 90_000_001, 1_000_170);

    // 1. Quasar ThirdPartyApi: telemetry events & notice publishing
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.thirdpartyapi.RequestReceived"],
    )
    .await;

    let published_tpa = publish_thirdpartyapi_request_received(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        "tranquility",
        "/characters/90000001/assets",
        1,
        "PyEveClient/24.01",
        ctx.now_unix(),
    )
    .expect("publish thirdpartyapi notice");
    assert!(published_tpa >= 1);

    let notices = poll_notices(&ctx, &sess);
    assert!(
        notices.iter().any(|(t, _)| t.ends_with("eve.thirdpartyapi.RequestReceived")),
        "Must receive RequestReceived notice"
    );

    let mut tpa_req = Vec::new();
    encode_id_field(2, 90_000_001, &mut tpa_req);
    encode_string_field(3, "tranquility", &mut tpa_req);
    encode_string_field(5, "/corporations/1000170", &mut tpa_req);
    let tpa_rsp = quasar_rpc(&ctx, &mut sess, "eve.thirdpartyapi.RequestReceived", tpa_req).await;
    assert_eq!(tpa_rsp.status_code, 200, "TPA RequestReceived: {}", tpa_rsp.status_message);

    // 2. Quasar Tournament: arenas, layouts, rulesets & match notices
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.tournament.arena.Created"],
    )
    .await;

    let published_arena = publish_tournament_arena_created(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        b"arena-alpha-1234",
        "Amarr Championship Arena",
        30_000_142,
        ctx.now_unix(),
    )
    .expect("publish arena notice");
    assert!(published_arena >= 1);

    let tourney_notices = poll_notices(&ctx, &sess);
    assert!(
        tourney_notices.iter().any(|(t, _)| t.ends_with("eve.tournament.arena.Created")),
        "Must receive arena Created notice"
    );

    let all_arenas_rsp = quasar_rpc(&ctx, &mut sess, "eve.tournament.arena.api.GetAllRequest", Vec::new()).await;
    assert_eq!(all_arenas_rsp.status_code, 200, "GetAllArenas: {}", all_arenas_rsp.status_message);

    let settings_rsp = quasar_rpc(&ctx, &mut sess, "eve.tournament.arena.api.GetSettingsRequest", Vec::new()).await;
    assert_eq!(settings_rsp.status_code, 200, "GetArenaSettings: {}", settings_rsp.status_message);

    let mut layout_req = Vec::new();
    encode_uuid_field(1, b"layout-10v10", &mut layout_req);
    let layout_rsp = quasar_rpc(&ctx, &mut sess, "eve.tournament.arena.layout.api.GetRequest", layout_req).await;
    assert_eq!(layout_rsp.status_code, 200, "GetLayout: {}", layout_rsp.status_message);

    let all_rulesets_rsp = quasar_rpc(&ctx, &mut sess, "eve.tournament.ruleset.api.GetAllRequest", Vec::new()).await;
    assert_eq!(all_rulesets_rsp.status_code, 200, "GetAllRulesets: {}", all_rulesets_rsp.status_message);

    let mut ruleset_req = Vec::new();
    encode_uuid_field(1, b"ruleset-standard-v1", &mut ruleset_req);
    let ruleset_rsp = quasar_rpc(&ctx, &mut sess, "eve.tournament.ruleset.api.GetRequest", ruleset_req).await;
    assert_eq!(ruleset_rsp.status_code, 200, "GetRuleset: {}", ruleset_rsp.status_message);

    // 3. Quasar UI: model messages for annotations, content tags, icons, and images
    let annot_bytes = encode_ui_annotation("banner_key", "Crucible Seasonal Event");
    assert!(!annot_bytes.is_empty(), "Annotation wire must not be empty");

    let tag_bytes = encode_ui_contenttag("faction_warfare_frontline");
    assert!(!tag_bytes.is_empty(), "ContentTag wire must not be empty");

    let icon_bytes = encode_ui_icon("ui_icon_fw_caldari");
    assert!(!icon_bytes.is_empty(), "Icon wire must not be empty");

    let img_bytes = encode_ui_image("res:/UI/Texture/Icons/banner.png", 1024, 768);
    assert!(!img_bytes.is_empty(), "Image wire must not be empty");

    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    let dyn_annot = reg.decode_dynamic("eve_public.ui.Annotation", &annot_bytes).expect("decode Annotation");
    assert_eq!(dyn_annot.full_name, "eve_public.ui.Annotation");

    let dyn_tag = reg.decode_dynamic("eve_public.ui.contenttag.Identifier", &tag_bytes).expect("decode ContentTag");
    assert_eq!(dyn_tag.full_name, "eve_public.ui.contenttag.Identifier");

    let dyn_icon = reg.decode_dynamic("eve_public.ui.icon.Identifier", &icon_bytes).expect("decode Icon");
    assert_eq!(dyn_icon.full_name, "eve_public.ui.icon.Identifier");

    let attr_bytes = encode_ui_image_attributes(1024, 768);
    let dyn_img = reg.decode_dynamic("eve_public.ui.image.Attributes", &attr_bytes).expect("Decodes image attributes");
    assert_eq!(dyn_img.full_name, "eve_public.ui.image.Attributes");
}

#[tokio::test]
async fn test_quasar_universe_vanguard_and_vendors() {
    // parity: feat:quasar.universe, quasar:eveProto/generated/eve/universe/navigation/location_pb2.pyj, quasar:eveProto/generated/eve/universe/navigation/station_pb2.pyj, quasar:eveProto/generated/eve/universe/constellation_pb2.pyj
    // parity: feat:quasar.vanguard, quasar:eveProto/generated/eve/vanguard/commission/commission_pb2.pyj, quasar:eveProto/generated/eve/vanguard/skirmish/api/events_pb2.pyj, quasar:eveProto/generated/eve/vanguard/contract/api/events_pb2.pyj
    // parity: feat:quasar.vendors, quasar:eveProto/generated/eve/vendors/epic/api_pb2.pyj, quasar:eveProto/generated/eve/vendors/steam/user/api_pb2.pyj, quasar:eveProto/generated/eve/vendors/twitch/entitlement/api/api_pb2.pyj, quasar:eveProto/generated/eve/vendors/contentful/asset/asset_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.universe",
        "quasar:eveProto/generated/eve/universe/navigation/location_pb2.pyj",
        "quasar:eveProto/generated/eve/universe/navigation/station_pb2.pyj",
        "quasar:eveProto/generated/eve/universe/constellation_pb2.pyj",
        "feat:quasar.vanguard",
        "quasar:eveProto/generated/eve/vanguard/commission/commission_pb2.pyj",
        "quasar:eveProto/generated/eve/vanguard/skirmish/api/events_pb2.pyj",
        "quasar:eveProto/generated/eve/vanguard/contract/api/events_pb2.pyj",
        "feat:quasar.vendors",
        "quasar:eveProto/generated/eve/vendors/epic/api_pb2.pyj",
        "quasar:eveProto/generated/eve/vendors/steam/user/api_pb2.pyj",
        "quasar:eveProto/generated/eve/vendors/twitch/entitlement/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve/vendors/contentful/asset/asset_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(202, 1002, 90_000_001, 1_000_170);

    // 1. Quasar Universe: Navigation location jump count, station jump count, region constellations
    let mut loc_req = Vec::new();
    encode_id_field(1, 30_000_142, &mut loc_req);
    encode_id_field(2, 30_000_144, &mut loc_req);
    let loc_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.universe.navigation.location.GetShortestGateJumpCountRequest",
        loc_req,
    )
    .await;
    assert_eq!(loc_rsp.status_code, 200, "GetShortestGateJumpCount: {}", loc_rsp.status_message);

    let mut stn_req = Vec::new();
    encode_id_field(1, 60_003_760, &mut stn_req);
    encode_id_field(2, 60_008_499, &mut stn_req);
    let stn_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.universe.navigation.station.GetShortestSystemJumpCountRequest",
        stn_req,
    )
    .await;
    assert_eq!(stn_rsp.status_code, 200, "GetShortestSystemJumpCount: {}", stn_rsp.status_message);

    let mut const_req = Vec::new();
    encode_id_field(1, 10_000_002, &mut const_req);
    let const_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.universe.constellation.GetAllInRegionRequest",
        const_req,
    )
    .await;
    assert_eq!(const_rsp.status_code, 200, "GetAllInRegion: {}", const_rsp.status_message);

    // 2. Quasar Vanguard: Commissions, skirmishes, contracts & notice publishing
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &[
            "eve.vanguard.commission.assignment.api.Completed",
            "eve.vanguard.skirmish.api.SkirmishCompleted",
            "eve.vanguard.contract.api.CorruptionContractCompleted",
        ],
    )
    .await;

    let pub_comm = publish_vanguard_commission_completed(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        b"comm-uuid-9900",
        "recon",
        "infantry_assault",
        500_001,
        ctx.now_unix(),
    )
    .expect("publish vanguard commission notice");
    assert!(pub_comm >= 1);

    let pub_skirm = publish_vanguard_skirmish_completed(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        77_8899,
        1, // outcome victory
        ctx.now_unix(),
    )
    .expect("publish vanguard skirmish notice");
    assert!(pub_skirm >= 1);

    let pub_ctr = publish_vanguard_contract_completed(
        &ctx.quasar_gateway,
        NoticeScope::SolarSystem(30_000_142),
        true,
        ctx.now_unix(),
    )
    .expect("publish vanguard contract notice");
    assert!(pub_ctr >= 1);

    let vg_notices = poll_notices(&ctx, &sess);
    assert!(
        vg_notices.iter().any(|(t, _)| t.ends_with("eve.vanguard.commission.assignment.api.Completed")),
        "Must receive assignment Completed notice"
    );
    assert!(
        vg_notices.iter().any(|(t, _)| t.ends_with("eve.vanguard.skirmish.api.SkirmishCompleted")),
        "Must receive SkirmishCompleted notice"
    );

    let mut admin_vg_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut admin_vg_req);
    let mut asgn_id = Vec::new();
    encode_uuid_field(1, b"comm-uuid-9900", &mut asgn_id);
    encode_string_field(2, "recon", &mut asgn_id);
    encode_bytes_field(2, &asgn_id, &mut admin_vg_req);
    let admin_vg_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest",
        admin_vg_req,
    )
    .await;
    assert_eq!(admin_vg_rsp.status_code, 200, "Vanguard admin trigger: {}", admin_vg_rsp.status_message);

    // 3. Quasar Vendors: Epic, Steam, Twitch, FusionAuth, Payletter, Contentful
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &[
            "eve.vendors.twitch.entitlement.api.DropEventReceived",
            "eve.vendors.fusionauth.api.UserLinked",
        ],
    )
    .await;

    let pub_twitch = publish_twitch_drop_received(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        b"twitch-drop-1234",
        501,
        ctx.now_unix(),
    )
    .expect("publish twitch drop notice");
    assert!(pub_twitch >= 1);

    let pub_fa = publish_fusionauth_user_linked(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        b"fusionauth-uuid-88",
        1002,
        ctx.now_unix(),
    )
    .expect("publish fusionauth notice");
    assert!(pub_fa >= 1);

    let vendor_notices = poll_notices(&ctx, &sess);
    assert!(
        vendor_notices.iter().any(|(t, _)| t.ends_with("DropEventReceived")),
        "Must receive Twitch drop notice"
    );
    assert!(
        vendor_notices.iter().any(|(t, _)| t.ends_with("UserLinked")),
        "Must receive FusionAuth user linked notice"
    );

    let mut epic_req = Vec::new();
    encode_id_field(1, 1002, &mut epic_req);
    let epic_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.epic.api.GetUserByEveUserIDRequest", epic_req).await;
    assert_eq!(epic_rsp.status_code, 200, "Epic GetUser: {}", epic_rsp.status_message);

    let mut deliver_epic_req = Vec::new();
    encode_id_field(1, 1002, &mut deliver_epic_req);
    let mut epic_ent = Vec::new();
    encode_string_field(1, "epic_ent_omega_30d", &mut epic_ent);
    encode_bytes_field(2, &epic_ent, &mut deliver_epic_req);
    let deliver_epic_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.epic.api.DeliverEntitlementRequest", deliver_epic_req).await;
    assert_eq!(deliver_epic_rsp.status_code, 200, "Epic Deliver: {}", deliver_epic_rsp.status_message);

    let mut get_epic_ent_req = Vec::new();
    encode_id_field(1, 1002, &mut get_epic_ent_req);
    let get_epic_ent_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.epic.api.GetEntitlementsRequest", get_epic_ent_req).await;
    assert_eq!(get_epic_ent_rsp.status_code, 200, "Epic GetEntitlements: {}", get_epic_ent_rsp.status_message);

    let mut steam_req = Vec::new();
    let mut steam_id_msg = Vec::new();
    encode_string_field(1, "76561198000000000", &mut steam_id_msg);
    encode_bytes_field(1, &steam_id_msg, &mut steam_req);
    let steam_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.steam.user.api.GetRequest", steam_req).await;
    assert_eq!(steam_rsp.status_code, 200, "Steam Get: {}", steam_rsp.status_message);

    let mut steam_by_eve_req = Vec::new();
    encode_id_field(1, 1002, &mut steam_by_eve_req);
    let steam_by_eve_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.steam.user.api.GetByEveUserRequest", steam_by_eve_req).await;
    assert_eq!(steam_by_eve_rsp.status_code, 200, "Steam GetByEveUser: {}", steam_by_eve_rsp.status_message);

    let mut fa_req = Vec::new();
    encode_uuid_field(1, b"fa-uuid-1234", &mut fa_req);
    let fa_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.fusionauth.api.LinkUserRequest", fa_req).await;
    assert_eq!(fa_rsp.status_code, 200, "FusionAuth Link: {}", fa_rsp.status_message);

    let mut payletter_req = Vec::new();
    encode_id_field(1, 1002, &mut payletter_req);
    let payletter_rsp = quasar_rpc(&ctx, &mut sess, "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest", payletter_req).await;
    assert_eq!(payletter_rsp.status_code, 200, "Payletter GetReceipt: {}", payletter_rsp.status_message);

    let contentful_wire = encode_contentful_asset("asset_news_header_v24");
    assert!(!contentful_wire.is_empty(), "Contentful asset wire must not be empty");
}

#[tokio::test]
async fn test_quasar_war_wormhole_and_serverops() {
    // parity: feat:quasar.war, quasar:eveProto/generated/eve/war/war_pb2.pyj, quasar:eveProto/generated/eve/war/entity_pb2.pyj
    // parity: feat:quasar.wormhole, quasar:eveProto/generated/eve/wormhole/wormhole_pb2.pyj
    // parity: feat:serverops.config-file
    eve_testkit::attest(&[
        "feat:quasar.war",
        "quasar:eveProto/generated/eve/war/war_pb2.pyj",
        "quasar:eveProto/generated/eve/war/entity_pb2.pyj",
        "feat:quasar.wormhole",
        "quasar:eveProto/generated/eve/wormhole/wormhole_pb2.pyj",
        "feat:serverops.config-file",
    ]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(203, 1003, 90_000_001, 1_000_170);

    // Seed war fixtures in DB
    ctx.db.with_mut_snapshot(|snap| {
        let war = WarRecord {
            war_id: 42,
            aggressor_corp_id: 1_000_170,
            defender_corp_id: 1_000_171,
            mutual: false,
            started_unix: 1_700_000_000,
            active: true,
        };
        snap.wars.insert(42, war);

        let mut corp2 = CorporationRecord::default();
        corp2.corporation_id = 1_000_171;
        corp2.name = "Defender Federal Corp".into();
        snap.corporations.insert(1_000_171, corp2);
    });

    // 1. Quasar War: Declarations, destruction stats, allies & notices
    subscribe_to_notices(
        &ctx,
        &mut sess,
        &["eve.war.Declared", "eve.war.EndScheduled"],
    )
    .await;

    let pub_war = publish_war_declared_notice(
        &ctx.quasar_gateway,
        NoticeScope::Corporation(1_000_170),
        42,
        1_000_170,
        1_000_171,
        ctx.now_unix(),
    )
    .expect("publish war declared notice");
    assert!(pub_war >= 1);

    let pub_end = publish_war_end_scheduled_notice(
        &ctx.quasar_gateway,
        NoticeScope::Corporation(1_000_170),
        42,
        ctx.now_unix() + 7 * 86400,
        ctx.now_unix(),
    )
    .expect("publish war end scheduled notice");
    assert!(pub_end >= 1);

    let war_notices = poll_notices(&ctx, &sess);
    assert!(
        war_notices.iter().any(|(t, _)| t.ends_with("eve.war.Declared")),
        "Must receive war Declared notice"
    );
    assert!(
        war_notices.iter().any(|(t, _)| t.ends_with("eve.war.EndScheduled")),
        "Must receive war EndScheduled notice"
    );

    let mut get_war_req = Vec::new();
    encode_id_field(1, 42, &mut get_war_req);
    let get_war_rsp = quasar_rpc(&ctx, &mut sess, "eve.war.GetRequest", get_war_req).await;
    assert_eq!(get_war_rsp.status_code, 200, "War Get: {}", get_war_rsp.status_message);

    let mut all_wars_req = Vec::new();
    encode_varint_field(1, 1, &mut all_wars_req); // head: true
    encode_varint_field(3, 50, &mut all_wars_req); // page_size: 50
    let all_wars_rsp = quasar_rpc(&ctx, &mut sess, "eve.war.GetAllRequest", all_wars_req).await;
    assert_eq!(all_wars_rsp.status_code, 200, "War GetAll: {}", all_wars_rsp.status_message);

    let mut war_stats_req = Vec::new();
    encode_id_field(1, 42, &mut war_stats_req);
    let war_stats_rsp = quasar_rpc(&ctx, &mut sess, "eve.war.GetDestructionStatisticsRequest", war_stats_req).await;
    assert_eq!(war_stats_rsp.status_code, 200, "War DestructionStats: {}", war_stats_rsp.status_message);

    let mut war_allies_req = Vec::new();
    encode_id_field(1, 42, &mut war_allies_req);
    let war_allies_rsp = quasar_rpc(&ctx, &mut sess, "eve.war.GetAlliesRequest", war_allies_req).await;
    assert_eq!(war_allies_rsp.status_code, 200, "War GetAllies: {}", war_allies_rsp.status_message);

    // 2. Quasar Wormhole: Wormhole identifier & attributes
    let wh_wire = encode_wormhole_identifier(100_001);
    assert!(!wh_wire.is_empty(), "Wormhole wire must not be empty");

    let mut wh_req = Vec::new();
    encode_id_field(1, 100_001, &mut wh_req);
    let wh_rsp = quasar_rpc(&ctx, &mut sess, "eve.wormhole.GetRequest", wh_req).await;
    assert_eq!(wh_rsp.status_code, 200, "Wormhole Get: {}", wh_rsp.status_message);

    // 3. Serverops Config-File: Ports, DB, cluster layout, feature toggles and startup validation
    let valid_toml = r#"
bind_address = "0.0.0.0"
macho_port = 26001
bridge_port = 26003
esi_port = 26005
quasar_grpc_port = 26007
sde_path = "data/evomu_sde.sqlite"
world_db_path = "data/evomu_world.sqlite"
belt_respawn_interval_secs = 86400
downtime_utc_secs_of_day = 39600

[[admin_accounts]]
username = "admin_root"
password_token = "admin_pass_hash"
character_id = 90000003
character_name = "ISD Polaris"

[[player_accounts]]
username = "pilot_alpha"
password_token = "secret123"

[feature_flags]
enable_quasar_gateway = true
enable_esi_http = true
enable_living_world = true
seed_demo = false
enable_project_discovery = true
enable_info_gathering = true
info_gathering_interval_secs = 300
"#;
    let cfg = ServerConfig::from_toml_str(valid_toml).expect("Valid config must parse cleanly");
    assert_eq!(cfg.bind_address, "0.0.0.0");
    assert_eq!(cfg.macho_port, 26001);
    assert_eq!(cfg.bridge_port, 26003);
    assert_eq!(cfg.esi_port, 26005);
    assert_eq!(cfg.quasar_grpc_port, 26007);
    assert_eq!(cfg.sde_path, "data/evomu_sde.sqlite");
    assert_eq!(cfg.world_db_path, "data/evomu_world.sqlite");
    assert_eq!(cfg.belt_respawn_interval_secs, 86_400);
    assert_eq!(cfg.downtime_utc_secs_of_day, 39_600);
    assert_eq!(cfg.admin_accounts.len(), 1);
    assert_eq!(cfg.admin_accounts[0].username, "admin_root");
    assert_eq!(cfg.player_accounts.len(), 1);
    assert_eq!(cfg.player_accounts[0].username, "pilot_alpha");
    assert!(cfg.feature_flags.enable_quasar_gateway);
    assert!(cfg.feature_flags.enable_esi_http);
    assert!(cfg.feature_flags.enable_living_world);
    assert!(!cfg.feature_flags.seed_demo);
    assert!(cfg.feature_flags.enable_project_discovery);
    assert!(cfg.feature_flags.enable_info_gathering);
    assert_eq!(cfg.feature_flags.info_gathering_interval_secs, 300);

    // Startup validation checks: ensure invalid inputs fail with helpful error messages
    let empty_bind = ServerConfig::from_toml_str(r#"bind_address = "   ""#);
    assert!(empty_bind.is_err(), "Empty bind_address must fail validation");

    let empty_sde = ServerConfig::from_toml_str(r#"sde_path = "   ""#);
    assert!(empty_sde.is_err(), "Empty sde_path must fail validation");

    let empty_db = ServerConfig::from_toml_str(r#"world_db_path = "   ""#);
    assert!(empty_db.is_err(), "Empty world_db_path must fail validation");

    let zero_respawn = ServerConfig::from_toml_str(r#"belt_respawn_interval_secs = 0"#);
    assert!(zero_respawn.is_err(), "Zero belt_respawn_interval_secs must fail validation");

    let excessive_respawn = ServerConfig::from_toml_str(r#"belt_respawn_interval_secs = 700000"#);
    assert!(excessive_respawn.is_err(), "Excessive belt_respawn_interval_secs must fail validation");

    let invalid_downtime = ServerConfig::from_toml_str(r#"downtime_utc_secs_of_day = 86400"#);
    assert!(invalid_downtime.is_err(), "Invalid downtime (>= 86400) must fail validation");

    let invalid_info_interval = ServerConfig::from_toml_str(r#"
[feature_flags]
info_gathering_interval_secs = 0
"#);
    assert!(invalid_info_interval.is_err(), "Zero info_gathering_interval_secs must fail validation");
}
