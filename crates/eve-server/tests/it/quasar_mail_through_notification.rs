//! Behavioral integration tests for Quasar Mail, Market, Medal, Membership,
//! Mission, Module, Monolith, Moon, Narrative, and Notification services.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicEvent, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::NoticeScope;
use eve_server::router::ServerContext;
use eve_server::services::quasar_social_community::{
    publish_medal_awarded_notice, publish_narrative_warp_vector_notice,
    publish_notification_notice,
};

static CORR_SEQ: AtomicU64 = AtomicU64::new(500);

fn seed_test_fixtures(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        let (corp_id, char_id, alliance_id) = (1_000_170i64, 90_000_001i64, 99_000_001i64);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = char_id;
            __corp });
        corp.ceo_id = char_id;
        corp.members.insert(char_id);
        corp.members.insert(90_000_002);
        corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.market_orders.entry(770_000_001).or_insert_with(|| eve_db::MarketOrderRecord {
            order_id: 770_000_001, type_id: 34, owner_id: char_id, region_id: 10_000_002,
            station_id: 60_003_760, is_bid: false, price: eve_db::IskCents::from_isk(10),
            vol_entered: 50_000, vol_remaining: 45_000, min_volume: 1, is_npc: false,
            escrow: eve_db::IskCents::ZERO,
        });
        snap.market_orders.entry(770_000_002).or_insert_with(|| eve_db::MarketOrderRecord {
            order_id: 770_000_002, type_id: 34, owner_id: 90_000_002, region_id: 10_000_002,
            station_id: 60_003_760, is_bid: true, price: eve_db::IskCents::from_isk(8),
            vol_entered: 20_000, vol_remaining: 18_000, min_volume: 1, is_npc: false,
            escrow: eve_db::IskCents::from_isk(144_000),
        });
    });
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    sess
}

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

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-mn-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-mn".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

async fn quasar_publish_event(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, payload: Vec<u8>) {
    let ev = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        ..Default::default()
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "PublishEvent", &[PyRep::Bytes(ev.encode_proto())], &HashMap::new(), None)
        .await
        .expect("PublishEvent");
    assert_eq!(rep, PyRep::Bool(true));
}

async fn subscribe_to_notices(ctx: &ServerContext, sess: &mut SessionState, notice_types: &[&str]) {
    let args: Vec<PyRep> = notice_types.iter().map(|s| PyRep::String(s.to_string())).collect();
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "SubscribeToNotice", &args, &HashMap::new(), None) // rpc-lint: allow Quasar notice subscription and poll routes
        .await
        .expect("SubscribeToNotice");
}

async fn poll_notices(ctx: &ServerContext, sess: &mut SessionState) -> Vec<(String, Vec<u8>)> {
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "PollNotices", &[], &HashMap::new(), None) // rpc-lint: allow Quasar notice subscription and poll routes
        .await
        .expect("PollNotices");
    let mut out = Vec::new();
    if let PyRep::List(items) = rep {
        for it in items {
            if let PyRep::KeyVal(kv) = it {
                let mut type_url = String::new();
                let mut payload = Vec::new();
                for (k, v) in kv {
                    if k == "type_url" { if let PyRep::String(s) = v { type_url = s; } }
                    else if k == "payload" { if let PyRep::Bytes(b) = v { payload = b; } }
                }
                out.push((type_url, payload));
            }
        }
    }
    out
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_sender_field(field_num: u32, char_id: u64, out: &mut Vec<u8>) {
    let mut cid = Vec::new();
    encode_varint_field(1, char_id, &mut cid);
    let mut sender = Vec::new();
    encode_bytes_field(1, &cid, &mut sender);
    encode_bytes_field(field_num, &sender, out);
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

fn get_str<'a>(msg: &'a DynamicQuasarMessage, field: u32) -> Option<&'a str> {
    match msg.get(field) {
        Some(DynamicValue::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn get_bool(msg: &DynamicQuasarMessage, field: u32) -> Option<bool> {
    match msg.get(field) {
        Some(DynamicValue::Bool(b)) => Some(*b),
        Some(DynamicValue::Uint64(v)) => Some(*v != 0),
        Some(DynamicValue::Uint32(v)) => Some(*v != 0),
        _ => None,
    }
}

// parity: feat:quasar.mail, feat:quasar.notification
#[tokio::test]
async fn test_quasar_mail_and_notification() {
    eve_testkit::attest(&["feat:quasar.mail", "feat:quasar.notification"]);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Mail descriptor verification (5 proto files)
    let mail_files = ["eve/mail/body.proto", "eve/mail/cspa.proto", "eve/mail/list.proto", "eve/mail/mail.proto", "eve/mail/status.proto"];
    for f in mail_files { assert!(reg.files_by_proto_name.contains_key(f), "missing {f}"); }
    assert_eq!(reg.files_by_domain.get("mail").map(|v| v.len()), Some(5));

    // Dynamic encode and decode eve.mail.Attributes
    let mut mail_attr_buf = Vec::new();
    encode_id_field(1, 90_000_001, &mut mail_attr_buf);
    encode_string_field(2, "Operation Horizon Deployment", &mut mail_attr_buf);
    let mut ts_buf = Vec::new();
    encode_varint_field(1, 1_760_000_000, &mut ts_buf);
    encode_bytes_field(3, &ts_buf, &mut mail_attr_buf);
    let mut recips_buf = Vec::new();
    encode_id_field(1, 90_000_002, &mut recips_buf);
    encode_id_field(2, 1_000_170, &mut recips_buf);
    encode_bytes_field(5, &recips_buf, &mut mail_attr_buf);

    let dyn_mail = reg.decode_dynamic("eve.mail.Attributes", &mail_attr_buf).expect("decode mail.Attributes");
    assert_eq!(get_str(&dyn_mail, 2), Some("Operation Horizon Deployment"));

    // Dynamic encode and decode eve.mail.body.Attributes
    let mut body_buf = Vec::new();
    encode_string_field(1, "All fleet pilots mobilize to station staging hangar.", &mut body_buf);
    let dyn_body = reg.decode_dynamic("eve.mail.body.Attributes", &body_buf).expect("decode mail.body.Attributes");
    assert_eq!(get_str(&dyn_body, 1), Some("All fleet pilots mobilize to station staging hangar."));

    // Dynamic encode and decode eve.mail.list.Attributes
    let mut list_buf = Vec::new();
    encode_string_field(1, "Alliance Combat Command", &mut list_buf);
    let dyn_list = reg.decode_dynamic("eve.mail.list.Attributes", &list_buf).expect("decode mail.list.Attributes");
    assert_eq!(get_str(&dyn_list, 1), Some("Alliance Combat Command"));

    // Live server context for mail RPC dispatch
    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(501, 1001, 90_000_001, 1_000_170);

    // Call eve.mail.cspa.GetRequest
    let mut get_cspa = Vec::new();
    encode_id_field(1, 90_000_001, &mut get_cspa);
    let cspa_rsp = quasar_rpc(&ctx, &mut sess, "eve.mail.cspa.GetRequest", get_cspa).await;
    assert_eq!(cspa_rsp.status_code, 200);

    // Call eve.mail.CCPSendMailRequest
    let mut send_mail = Vec::new();
    encode_id_field(1, 90_000_002, &mut send_mail);
    encode_string_field(3, "Fleet Standby Order", &mut send_mail);
    encode_string_field(4, "Maintain defensive positions at docking perimeter.", &mut send_mail);
    let send_rsp = quasar_rpc(&ctx, &mut sess, "eve.mail.CCPSendMailRequest", send_mail).await;
    assert_eq!(send_rsp.status_code, 200);
    assert!(send_rsp.payload.unwrap().type_url.ends_with("eve.mail.CCPSendMailResponse"));

    // 2. Quasar Notification descriptor verification (4 proto files)
    let notif_files = ["eve/notification/contact.proto", "eve/notification/notification.proto", "eve/notification/sender.proto", "eve/notification/type.proto"];
    for f in notif_files { assert!(reg.files_by_proto_name.contains_key(f), "missing {f}"); }
    assert_eq!(reg.files_by_domain.get("notification").map(|v| v.len()), Some(4));

    // Dynamic encode and decode eve.notification.Attributes (unread, then read)
    let mut notif_buf = Vec::new();
    encode_id_field(1, 42, &mut notif_buf);
    encode_sender_field(2, 90_000_001, &mut notif_buf);
    let mut ts_n = Vec::new();
    encode_varint_field(1, 1_760_000_000, &mut ts_n);
    encode_bytes_field(3, &ts_n, &mut notif_buf);
    encode_varint_field(4, 0, &mut notif_buf);
    encode_string_field(5, "War declared by rival corporation", &mut notif_buf);

    let dyn_notif = reg.decode_dynamic("eve.notification.Attributes", &notif_buf).expect("decode notif unread");
    assert_eq!(get_bool(&dyn_notif, 4), Some(false));

    let mut notif_read_buf = Vec::new();
    encode_id_field(1, 42, &mut notif_read_buf);
    encode_sender_field(2, 90_000_001, &mut notif_read_buf);
    encode_bytes_field(3, &ts_n, &mut notif_read_buf);
    encode_varint_field(4, 1, &mut notif_read_buf);
    let dyn_notif_read = reg.decode_dynamic("eve.notification.Attributes", &notif_read_buf).expect("decode notif read");
    assert_eq!(get_bool(&dyn_notif_read, 4), Some(true));

    // Notification notice pipeline
    subscribe_to_notices(&ctx, &mut sess, &["eve.notification.Attributes"]).await;
    let published_n = publish_notification_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        42,
        90_000_002,
        false,
        ctx.now_unix(),
    ).expect("publish notif notice");
    assert!(published_n >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(notices.iter().any(|(t, _)| t.ends_with("eve.notification.Attributes")));
}

// parity: feat:quasar.market, feat:quasar.moon
#[tokio::test]
async fn test_quasar_market_and_moon() {
    eve_testkit::attest(&["feat:quasar.market", "feat:quasar.moon"]);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Market descriptor verification (16 proto files)
    assert_eq!(reg.files_by_domain.get("market").map(|v| v.len()), Some(16));
    assert!(reg.messages_by_full_name.contains_key("eve.market.orders.GetOrdersRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.market.orders.GetOrdersResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.market.price.history.GetForRegionAndTypeRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.market.character.sellorder.Attributes"));

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(502, 1002, 90_000_001, 1_000_170);

    // Call eve.market.orders.GetOrdersRequest for Tritanium (type 34) in Forge
    let mut orders_req = Vec::new();
    encode_id_field(1, 10_000_002, &mut orders_req);
    encode_id_field(2, 60_003_760, &mut orders_req);
    encode_id_field(3, 34, &mut orders_req);
    let orders_rsp = quasar_rpc(&ctx, &mut sess, "eve.market.orders.GetOrdersRequest", orders_req).await;
    assert_eq!(orders_rsp.status_code, 200, "GetOrdersRequest: {}", orders_rsp.status_message);
    let orders_any = orders_rsp.payload.expect("GetOrdersResponse");
    assert!(orders_any.type_url.ends_with("eve.market.orders.GetOrdersResponse"));
    let wm_orders = WireMessage::decode(&orders_any.value).expect("decode GetOrdersResponse");
    assert!(!wm_orders.fields.is_empty(), "Orders response must contain seeded orders");

    // Call eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest
    let mut buy_req = Vec::new();
    encode_id_field(1, 10_000_002, &mut buy_req);
    let buy_rsp = quasar_rpc(&ctx, &mut sess, "eve.market.orders.buyorder.GetAllPublicOpenInRegionRequest", buy_req).await;
    assert_eq!(buy_rsp.status_code, 200);

    // Call eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest
    let mut sell_req = Vec::new();
    encode_id_field(1, 10_000_002, &mut sell_req);
    let sell_rsp = quasar_rpc(&ctx, &mut sess, "eve.market.orders.sellorder.GetAllPublicOpenInRegionRequest", sell_req).await;
    assert_eq!(sell_rsp.status_code, 200);

    // Call eve.market.price.history.GetForRegionAndTypeRequest
    let mut hist_req = Vec::new();
    encode_id_field(1, 10_000_002, &mut hist_req);
    encode_id_field(2, 34, &mut hist_req);
    let hist_rsp = quasar_rpc(&ctx, &mut sess, "eve.market.price.history.GetForRegionAndTypeRequest", hist_req).await;
    assert_eq!(hist_rsp.status_code, 200);
    let hist_any = hist_rsp.payload.expect("GetForRegionAndTypeResponse");
    assert!(hist_any.type_url.ends_with("eve.market.price.history.GetForRegionAndTypeResponse"));

    // Dynamic encode and decode eve.market.character.sellorder.Attributes
    let mut char_sell = Vec::new();
    encode_id_field(1, 90_000_001, &mut char_sell);
    let mut generic_item = Vec::new();
    encode_id_field(1, 34, &mut generic_item);
    encode_varint_field(2, 10_000, &mut generic_item);
    encode_bytes_field(2, &generic_item, &mut char_sell);
    let dyn_sell = reg.decode_dynamic("eve.market.character.sellorder.Attributes", &char_sell).expect("decode sellorder");
    assert!(dyn_sell.get(1).is_some());

    // 2. Quasar Moon descriptor verification (1 proto file)
    assert_eq!(reg.files_by_domain.get("moon").map(|v| v.len()), Some(1));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.GetMoonsOfPlanetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.moon.GetMoonsOfPlanetResponse"));

    // Call eve.moon.GetRequest
    let mut moon_req = Vec::new();
    encode_id_field(1, 40_000_001, &mut moon_req);
    let moon_rsp = quasar_rpc(&ctx, &mut sess, "eve.moon.GetRequest", moon_req).await;
    assert_eq!(moon_rsp.status_code, 200);
    let moon_any = moon_rsp.payload.expect("moon GetResponse");
    assert!(moon_any.type_url.ends_with("eve.moon.GetResponse"));
    let wm_moon = WireMessage::decode(&moon_any.value).expect("decode moon GetResponse");
    let attr_bytes = wm_moon.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let attr_wm = WireMessage::decode(attr_bytes).expect("decode moon Attributes");
    let moon_name = attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert!(moon_name.contains("Moon #40000001"));

    // Call eve.moon.GetMoonsOfPlanetRequest
    let mut planet_req = Vec::new();
    encode_id_field(1, 40_000_100, &mut planet_req);
    let planet_rsp = quasar_rpc(&ctx, &mut sess, "eve.moon.GetMoonsOfPlanetRequest", planet_req).await;
    assert_eq!(planet_rsp.status_code, 200);
    assert!(planet_rsp.payload.unwrap().type_url.ends_with("eve.moon.GetMoonsOfPlanetResponse"));
}

// parity: feat:quasar.medal, feat:quasar.membership
#[tokio::test]
async fn test_quasar_medal_and_membership() {
    eve_testkit::attest(&["feat:quasar.medal", "feat:quasar.membership"]);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Medal descriptor verification (3 proto files)
    let medal_files = ["eve/medal/event.proto", "eve/medal/issued.proto", "eve/medal/medal.proto"];
    for f in medal_files { assert!(reg.files_by_proto_name.contains_key(f), "missing {f}"); }
    assert_eq!(reg.files_by_domain.get("medal").map(|v| v.len()), Some(3));

    // Dynamic encode and decode eve.medal.Attributes
    let mut medal_attr_buf = Vec::new();
    encode_string_field(1, "Distinguished Fleet Command Medal", &mut medal_attr_buf);
    encode_string_field(2, "Awarded for tactical brilliance in fleet defense", &mut medal_attr_buf);
    encode_id_field(3, 1_000_170, &mut medal_attr_buf);
    encode_id_field(4, 90_000_001, &mut medal_attr_buf);

    let dyn_medal = reg.decode_dynamic("eve.medal.Attributes", &medal_attr_buf).expect("decode medal.Attributes");
    assert_eq!(get_str(&dyn_medal, 1), Some("Distinguished Fleet Command Medal"));
    assert_eq!(get_str(&dyn_medal, 2), Some("Awarded for tactical brilliance in fleet defense"));

    // Dynamic encode and decode eve.medal.Graphics
    let mut g_item = Vec::new();
    encode_varint_field(1, 1, &mut g_item);
    encode_varint_field(2, 2, &mut g_item);
    encode_varint_field(3, 3, &mut g_item);
    encode_string_field(4, "ribbon_gold_star", &mut g_item);
    let mut graphics_buf = Vec::new();
    encode_bytes_field(1, &g_item, &mut graphics_buf);
    let dyn_graphics = reg.decode_dynamic("eve.medal.Graphics", &graphics_buf).expect("decode medal.Graphics");
    assert!(dyn_graphics.get(1).is_some());

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(503, 1003, 90_000_001, 1_000_170);

    // Medal notice pipeline
    subscribe_to_notices(&ctx, &mut sess, &["eve.medal.issued.Awarded"]).await;
    let published_m = publish_medal_awarded_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        1001,
        90_000_001,
        90_000_001,
        1_000_170,
        "Exemplary service in nullsec campaign",
        ctx.now_unix(),
    ).expect("publish medal notice");
    assert!(published_m >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(notices.iter().any(|(t, _)| t.ends_with("eve.medal.issued.Awarded")));

    // Medal event routed via publicGatewaySvc
    let mut medal_ev = Vec::new();
    encode_id_field(1, 1001, &mut medal_ev);
    encode_id_field(3, 90_000_001, &mut medal_ev);
    encode_id_field(4, 1_000_170, &mut medal_ev);
    encode_id_field(5, 90_000_001, &mut medal_ev);
    encode_string_field(6, "Fleet engagement citation", &mut medal_ev);
    quasar_publish_event(&ctx, &mut sess, "eve.medal.event.Awarded", medal_ev).await;
    let evts = ctx.quasar_gateway.published_events.read().unwrap();
    assert!(evts.iter().any(|e| e.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve.medal.event.Awarded"))));
    drop(evts);

    // 2. Quasar Membership descriptor verification (4 proto files)
    let mem_files = ["eve/membership/alliance.proto", "eve/membership/corporation.proto", "eve/membership/faction.proto", "eve/membership/membership.proto"];
    for f in mem_files { assert!(reg.files_by_proto_name.contains_key(f), "missing {f}"); }
    assert_eq!(reg.files_by_domain.get("membership").map(|v| v.len()), Some(4));

    // Call eve.membership.GetCharacterAllRequest
    let mut mem_all_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut mem_all_req);
    let mem_all_rsp = quasar_rpc(&ctx, &mut sess, "eve.membership.GetCharacterAllRequest", mem_all_req).await;
    assert_eq!(mem_all_rsp.status_code, 200);
    assert!(mem_all_rsp.payload.unwrap().type_url.ends_with("eve.membership.GetCharacterAllResponse"));

    // Call eve.membership.corporation.GetCharacterRequest
    let mut corp_mem_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut corp_mem_req);
    let corp_mem_rsp = quasar_rpc(&ctx, &mut sess, "eve.membership.corporation.GetCharacterRequest", corp_mem_req).await;
    assert_eq!(corp_mem_rsp.status_code, 200);
    let corp_mem_any = corp_mem_rsp.payload.expect("GetCharacterResponse");
    assert!(corp_mem_any.type_url.ends_with("eve.membership.corporation.GetCharacterResponse"));
    let wm_corp_mem = WireMessage::decode(&corp_mem_any.value).expect("decode GetCharacterResponse");
    let attr_f = wm_corp_mem.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap();
    let wm_attr = WireMessage::decode(attr_f).expect("decode Attributes");
    let title_str = wm_attr.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert!(title_str == "Director" || title_str == "Chief Executive Officer");

    // Dynamic encode and decode eve.membership.corporation.Attributes
    let mut title_buf = Vec::new();
    encode_string_field(1, "Senior Fleet Admiral", &mut title_buf);
    let dyn_corp_attr = reg.decode_dynamic("eve.membership.corporation.Attributes", &title_buf).expect("decode corp Attributes");
    assert_eq!(get_str(&dyn_corp_attr, 1), Some("Senior Fleet Admiral"));
}

// parity: feat:quasar.mission, feat:quasar.module, feat:quasar.monolith, feat:quasar.narrative
#[tokio::test]
async fn test_quasar_mission_module_monolith_narrative() {
    eve_testkit::attest(&["feat:quasar.mission", "feat:quasar.module", "feat:quasar.monolith", "feat:quasar.narrative"]);
    let reg = QuasarDescriptorRegistry::open_default().expect("registry open");

    // 1. Quasar Mission descriptor verification (9 proto files)
    assert_eq!(reg.files_by_domain.get("mission").map(|v| v.len()), Some(9));
    assert!(reg.messages_by_full_name.contains_key("eve.mission.api.GetCompletedCountRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.mission.api.GetCompletedCountResponse"));

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(504, 1004, 90_000_001, 1_000_170);

    // Call eve.mission.api.GetCompletedCountRequest
    let mut miss_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut miss_req);
    let miss_rsp = quasar_rpc(&ctx, &mut sess, "eve.mission.api.GetCompletedCountRequest", miss_req).await;
    assert_eq!(miss_rsp.status_code, 200);
    let miss_any = miss_rsp.payload.expect("GetCompletedCountResponse");
    assert!(miss_any.type_url.ends_with("eve.mission.api.GetCompletedCountResponse"));
    let wm_miss = WireMessage::decode(&miss_any.value).expect("decode GetCompletedCountResponse");
    let completed_count = wm_miss.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint().unwrap();
    assert_eq!(completed_count, 15);

    // Mission completion event routed via publicGatewaySvc
    let mut comp_ev = Vec::new();
    encode_id_field(1, 90_000_001, &mut comp_ev);
    encode_varint_field(3, 4, &mut comp_ev);
    quasar_publish_event(&ctx, &mut sess, "eve.mission.security.event.CharacterCompleted", comp_ev).await;

    // 2. Quasar Module descriptor verification (2 proto files)
    assert_eq!(reg.files_by_domain.get("module").map(|v| v.len()), Some(2));
    assert!(reg.messages_by_full_name.contains_key("eve.module.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.module.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.moduletype.Identifier"));

    // Verify SDE ship modules: Rifter (587) and 5MN MWD (440)
    let rifter = ctx.sde.types.get(&587).expect("Rifter in SDE");
    assert_eq!(rifter.name, "Rifter");
    let mwd = ctx.sde.types.get(&440).expect("MWD in SDE");
    assert_eq!(mwd.name, "5MN Microwarpdrive II");

    // Dynamic encode and decode eve.module.Attributes & Identifier
    let mut mod_attr_buf = Vec::new();
    encode_id_field(1, 440, &mut mod_attr_buf);
    let dyn_mod = reg.decode_dynamic("eve.module.Attributes", &mod_attr_buf).expect("decode module.Attributes");
    assert!(dyn_mod.get(1).is_some());

    let mut mod_id_buf = Vec::new();
    encode_varint_field(1, 100_000_001, &mut mod_id_buf);
    let dyn_mod_id = reg.decode_dynamic("eve.module.Identifier", &mod_id_buf).expect("decode module.Identifier");
    assert_eq!(get_u64(&dyn_mod_id, 1), Some(100_000_001));

    // 3. Quasar Monolith descriptor verification (10 proto files)
    assert_eq!(reg.files_by_domain.get("monolith").map(|v| v.len()), Some(10));
    assert!(reg.messages_by_full_name.contains_key("eve.monolith.GetClusterInfoRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.monolith.GetClusterInfoResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.monolith.character.location.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.monolith.character.logonstatus.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.monolith.legacy.json.Request"));

    // Call eve.monolith.GetClusterInfoRequest
    let cl_rsp = quasar_rpc(&ctx, &mut sess, "eve.monolith.GetClusterInfoRequest", Vec::new()).await;
    assert_eq!(cl_rsp.status_code, 200);
    let cl_any = cl_rsp.payload.expect("GetClusterInfoResponse");
    assert!(cl_any.type_url.ends_with("eve.monolith.GetClusterInfoResponse"));
    let wm_cl = WireMessage::decode(&cl_any.value).expect("decode GetClusterInfoResponse");
    let cl_bytes = wm_cl.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let wm_cl_inner = WireMessage::decode(cl_bytes).expect("decode Cluster");
    let cl_name = wm_cl_inner.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(cl_name, "macho.tranquility");

    // Call eve.monolith.character.location.GetRequest
    let mut loc_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut loc_req);
    let loc_rsp = quasar_rpc(&ctx, &mut sess, "eve.monolith.character.location.GetRequest", loc_req).await;
    assert_eq!(loc_rsp.status_code, 200);
    assert!(loc_rsp.payload.unwrap().type_url.ends_with("eve.monolith.character.location.GetResponse"));

    // Call eve.monolith.character.logonstatus.GetRequest
    let mut logon_req = Vec::new();
    encode_id_field(1, 90_000_001, &mut logon_req);
    let logon_rsp = quasar_rpc(&ctx, &mut sess, "eve.monolith.character.logonstatus.GetRequest", logon_req).await;
    assert_eq!(logon_rsp.status_code, 200);

    // Call eve.monolith.legacy.json.Request
    let mut leg_req = Vec::new();
    let mut hdr = Vec::new();
    encode_string_field(3, "corr-legacy-dispatch", &mut hdr);
    encode_bytes_field(1, &hdr, &mut leg_req);
    encode_string_field(3, r#"{"cmd":"check_health"}"#, &mut leg_req);
    let leg_rsp = quasar_rpc(&ctx, &mut sess, "eve.monolith.legacy.json.Request", leg_req).await;
    assert_eq!(leg_rsp.status_code, 200);
    let leg_any = leg_rsp.payload.expect("legacy Response");
    assert!(leg_any.type_url.ends_with("eve.monolith.legacy.json.Response"));
    let wm_leg = WireMessage::decode(&leg_any.value).expect("decode legacy Response");
    let body_str = wm_leg.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap();
    assert!(body_str.contains(r#""status":"ok""#));

    // Monolith event routed via publicGatewaySvc
    let mut sys_loaded = Vec::new();
    encode_id_field(1, 30_000_142, &mut sys_loaded);
    quasar_publish_event(&ctx, &mut sess, "eve.monolith.api.SolarSystemLoaded", sys_loaded).await;

    // 4. Quasar Narrative descriptor verification (1 proto file)
    assert_eq!(reg.files_by_domain.get("narrative").map(|v| v.len()), Some(1));
    assert!(reg.messages_by_full_name.contains_key("eve.narrative.api.EncryptedItemDecrypted"));
    assert!(reg.messages_by_full_name.contains_key("eve.narrative.api.WarpVectorItemActivated"));

    // Narrative event routed via publicGatewaySvc
    let mut enc_ev = Vec::new();
    encode_id_field(1, 90_000_001, &mut enc_ev);
    encode_id_field(2, 60, &mut enc_ev);
    quasar_publish_event(&ctx, &mut sess, "eve.narrative.api.EncryptedItemDecrypted", enc_ev).await;

    // Narrative notice pipeline
    subscribe_to_notices(&ctx, &mut sess, &["eve.narrative.api.WarpVectorItemActivated"]).await;
    let published_narr = publish_narrative_warp_vector_notice(
        &ctx.quasar_gateway,
        NoticeScope::Character(90_000_001),
        90_000_001,
        30_000_142,
        ctx.now_unix(),
    ).expect("publish narrative notice");
    assert!(published_narr >= 1);

    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(notices.iter().any(|(t, _)| t.ends_with("eve.narrative.api.WarpVectorItemActivated")));
}
