use super::*;
use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND};
use eve_proto::pb;
use eve_proto::quasar::{QuasarDescriptorRegistry, WireField, WireMessage, WireValue};
use std::sync::OnceLock;

#[cfg(test)]
#[path = "quasar_space_domain_extra_unit_tests.rs"]
mod quasar_space_domain_extra_unit_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = Arc::new(PublicGatewayRouter::new(
        descriptors,
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    (router, db, sde)
}

fn invoke(
    router: &PublicGatewayRouter,
    req_type: &str,
    ctx: &AuthoritativeContext,
    raw_value: Vec<u8>,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(req_type)
        .cloned()
        .unwrap_or_else(|| panic!("missing handler for {req_type}"));
    let raw_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
        value: raw_value,
    };
    handler(ctx, &raw_any, msg)
}

fn user_session_ctx(char_id: u32) -> AuthoritativeContext {
    AuthoritativeContext::for_session(Some(1), Some(char_id), Some(1_000_009), 0, "tranquility", vec![1])
}

fn admin_ctx(char_id: u32) -> AuthoritativeContext {
    let mut ctx = user_session_ctx(char_id);
    ctx.service_role_mask = u64::MAX;
    ctx
}

#[test]
fn unit_helpers_and_encode_space_location() {
    assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
    assert_eq!(hex_decode("00abff"), vec![0x00, 0xab, 0xff]);
    assert!(hex_decode("zz").is_empty());

    let d_buf = pb::message("eve.space.measurement.Attributes")
        .set("value", 42.5)
        .must_encode();
    let wm = WireMessage::decode(&d_buf).unwrap();
    assert_eq!(wm.fields[0].field_number, 2);
    assert_eq!(wm.fields[0].value.as_fixed64(), Some(42.5f64.to_bits()));

    assert_eq!(as_u32(&DynamicValue::Uint32(7)), Some(7));
    assert_eq!(as_u32(&DynamicValue::Uint64(8)), Some(8));
    assert_eq!(as_u32(&DynamicValue::Int32(9)), Some(9));
    assert_eq!(as_u32(&DynamicValue::Int64(10)), Some(10));
    assert_eq!(as_u32(&DynamicValue::Int32(-1)), None);
    assert_eq!(as_u32(&DynamicValue::Bool(true)), None);

    assert_eq!(as_f64(&DynamicValue::Double(1.5)), Some(1.5));
    assert_eq!(as_f64(&DynamicValue::Float(2.5)), Some(2.5));
    assert_eq!(as_f64(&DynamicValue::Uint64(3)), Some(3.0));
    assert_eq!(as_f64(&DynamicValue::Int64(-4)), Some(-4.0));
    assert_eq!(as_f64(&DynamicValue::Bool(false)), None);

    let reg_only = encode_space_location(10_000_002, None, None);
    let wm_reg = WireMessage::decode(&reg_only).unwrap();
    assert!(wm_reg.fields.iter().any(|f| f.field_number == 1));
    assert!(wm_reg.fields.iter().any(|f| f.field_number == 3 && f.value.as_varint() == Some(1)));

    let con_only = encode_space_location(10_000_002, Some(20_000_020), None);
    let wm_con = WireMessage::decode(&con_only).unwrap();
    assert!(wm_con.fields.iter().any(|f| f.field_number == 2));

    let sys_loc = encode_space_location(10_000_002, Some(20_000_020), Some(30_000_142));
    let wm_sys = WireMessage::decode(&sys_loc).unwrap();
    let inner_con = wm_sys
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| f.value.as_bytes())
        .unwrap();
    let wm_inner = WireMessage::decode(inner_con).unwrap();
    assert!(wm_inner.fields.iter().any(|f| f.field_number == 2));
}

#[test]
fn resolve_locations_and_get_all_solar_systems_and_security_band() {
    let (router, _db, _sde) = setup_router();
    let ctx = user_session_ctx(90_000_001);

    // 1. ResolveLocationsRequest: valid constellation + valid solar system
    let mut loc_con_id = DynamicQuasarMessage::new("Id");
    loc_con_id.set(1, DynamicValue::Uint32(20_000_020));
    let mut loc_con = DynamicQuasarMessage::new("Loc");
    loc_con.set(1, DynamicValue::Message(Box::new(loc_con_id)));

    let mut loc_sys_id = DynamicQuasarMessage::new("Id");
    loc_sys_id.set(1, DynamicValue::Uint32(30_000_142));
    let mut loc_sys = DynamicQuasarMessage::new("Loc");
    loc_sys.set(2, DynamicValue::Message(Box::new(loc_sys_id)));

    let mut req = DynamicQuasarMessage::new("eve.space.location.api.ResolveLocationsRequest");
    req.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(loc_con)),
            DynamicValue::Message(Box::new(loc_sys)),
        ]),
    );
    let rsp = invoke(
        &router,
        "eve.space.location.api.ResolveLocationsRequest",
        &ctx,
        Vec::new(),
        &req,
    )
    .expect("resolve valid");
    let wm = WireMessage::decode(&rsp.value).unwrap();
    assert!(wm.fields.iter().any(|f| f.field_number == 1));
    assert!(!wm.fields.iter().any(|f| f.field_number == 3));

    // Missing constellation + missing solar system -> populates field 3
    let mut bad_con_id = DynamicQuasarMessage::new("Id");
    bad_con_id.set(1, DynamicValue::Uint32(99_999_999));
    let mut bad_con = DynamicQuasarMessage::new("Loc");
    bad_con.set(1, DynamicValue::Message(Box::new(bad_con_id)));

    let mut bad_sys_id = DynamicQuasarMessage::new("Id");
    bad_sys_id.set(1, DynamicValue::Uint32(88_888_888));
    let mut bad_sys = DynamicQuasarMessage::new("Loc");
    bad_sys.set(2, DynamicValue::Message(Box::new(bad_sys_id)));

    let mut bad_req = DynamicQuasarMessage::new("eve.space.location.api.ResolveLocationsRequest");
    bad_req.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(bad_con)),
            DynamicValue::Message(Box::new(bad_sys)),
        ]),
    );
    let bad_rsp = invoke(
        &router,
        "eve.space.location.api.ResolveLocationsRequest",
        &ctx,
        Vec::new(),
        &bad_req,
    )
    .expect("resolve missing");
    let wm_bad = WireMessage::decode(&bad_rsp.value).unwrap();
    assert!(wm_bad.fields.iter().any(|f| f.field_number == 3));

    // 2. GetAllSolarSystemsRequest
    let all_rsp = invoke(
        &router,
        "eve.space.location.api.GetAllSolarSystemsRequest",
        &ctx,
        Vec::new(),
        &DynamicQuasarMessage::new("eve.space.location.api.GetAllSolarSystemsRequest"),
    )
    .expect("all solar systems");
    assert!(!all_rsp.value.is_empty());

    // 3. GetLocationsWithinSecurityRequest
    let mut min_m = DynamicQuasarMessage::new("Sec");
    min_m.set(1, DynamicValue::Double(0.8));
    let mut max_m = DynamicQuasarMessage::new("Sec");
    max_m.set(1, DynamicValue::Double(1.0));
    let mut sec_req = DynamicQuasarMessage::new("eve.space.api.GetLocationsWithinSecurityRequest");
    sec_req.set(1, DynamicValue::Message(Box::new(min_m)));
    sec_req.set(2, DynamicValue::Message(Box::new(max_m)));
    let sec_rsp = invoke(
        &router,
        "eve.space.api.GetLocationsWithinSecurityRequest",
        &ctx,
        Vec::new(),
        &sec_req,
    )
    .expect("security band");
    assert!(!sec_rsp.value.is_empty());

    // Invalid security band (min > max)
    let mut bad_min = DynamicQuasarMessage::new("Sec");
    bad_min.set(1, DynamicValue::Double(1.0));
    let mut bad_max = DynamicQuasarMessage::new("Sec");
    bad_max.set(1, DynamicValue::Double(0.5));
    let mut bad_sec = DynamicQuasarMessage::new("eve.space.api.GetLocationsWithinSecurityRequest");
    bad_sec.set(1, DynamicValue::Message(Box::new(bad_min)));
    bad_sec.set(2, DynamicValue::Message(Box::new(bad_max)));
    let err = invoke(
        &router,
        "eve.space.api.GetLocationsWithinSecurityRequest",
        &ctx,
        Vec::new(),
        &bad_sec,
    )
    .unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);
}

#[test]
fn station_handlers_validate_and_return_station_data() {
    let (router, _db, _sde) = setup_router();
    let ctx = user_session_ctx(90_000_001);

    let mut st_id_msg = DynamicQuasarMessage::new("Id");
    st_id_msg.set(1, DynamicValue::Uint32(60_003_760));
    let mut req = DynamicQuasarMessage::new("StationReq");
    req.set(1, DynamicValue::Message(Box::new(st_id_msg)));

    let mut bad_id_msg = DynamicQuasarMessage::new("Id");
    bad_id_msg.set(1, DynamicValue::Uint32(69_999_999));
    let mut not_found_req = DynamicQuasarMessage::new("StationReq");
    not_found_req.set(1, DynamicValue::Message(Box::new(bad_id_msg)));

    let empty_req = DynamicQuasarMessage::new("StationReq");

    for endpoint in [
        "eve.space.npcstation.api.GetRequest",
        "eve_public.station.api.GetRequest",
        "eve.station.GetRequest",
        "eve.station.GetNameRequest",
    ] {
        let rsp = invoke(&router, endpoint, &ctx, Vec::new(), &req).expect(endpoint);
        assert!(!rsp.value.is_empty());

        let err_missing = invoke(&router, endpoint, &ctx, Vec::new(), &empty_req).unwrap_err();
        assert_eq!(err_missing.0, STATUS_BAD_REQUEST);

        let err_nf = invoke(&router, endpoint, &ctx, Vec::new(), &not_found_req).unwrap_err();
        assert_eq!(err_nf.0, STATUS_NOT_FOUND);
    }

    // GetStationsInSolarSystemRequest
    let mut sys_id_msg = DynamicQuasarMessage::new("SysId");
    sys_id_msg.set(1, DynamicValue::Uint32(30_000_142));
    let mut sys_req = DynamicQuasarMessage::new("eve.station.GetStationsInSolarSystemRequest");
    sys_req.set(1, DynamicValue::Message(Box::new(sys_id_msg)));
    let sys_rsp = invoke(
        &router,
        "eve.station.GetStationsInSolarSystemRequest",
        &ctx,
        Vec::new(),
        &sys_req,
    )
    .expect("stations in jita");
    assert!(!sys_rsp.value.is_empty());

    let err_no_sys = invoke(
        &router,
        "eve.station.GetStationsInSolarSystemRequest",
        &ctx,
        Vec::new(),
        &empty_req,
    )
    .unwrap_err();
    assert_eq!(err_no_sys.0, STATUS_BAD_REQUEST);
}

#[test]
fn space_measurements_record_update_and_query_top_and_bottom() {
    let (router, _db, _sde) = setup_router();
    let user_ctx = user_session_ctx(90_000_001);
    let admin = admin_ctx(90_000_001);

    // Non-admin rejected
    let err_unauth = invoke(
        &router,
        "eve.space.measurement.api.RecordRequest",
        &user_ctx,
        Vec::new(),
        &DynamicQuasarMessage::new("eve.space.measurement.api.RecordRequest"),
    )
    .unwrap_err();
    assert_ne!(err_unauth.0, 0);

    // Missing name -> BAD_REQUEST
    let err_no_name = invoke(
        &router,
        "eve.space.measurement.api.RecordRequest",
        &admin,
        Vec::new(),
        &DynamicQuasarMessage::new("eve.space.measurement.api.RecordRequest"),
    )
    .unwrap_err();
    assert_eq!(err_no_name.0, STATUS_BAD_REQUEST);

    // Helper to record a measurement at a location
    let record = |endpoint: &str, name: &str, sys_id: u32, val: f64| {
        let loc_bytes = encode_space_location(10_000_002, Some(20_000_020), Some(sys_id));
        let raw_at = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(loc_bytes),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::Fixed64(val.to_bits()),
                },
            ],
        }
        .encode();
        let raw_req = WireMessage {
            fields: vec![WireField {
                field_number: 2,
                value: WireValue::LengthDelimited(raw_at),
            }],
        }
        .encode();

        let mut id_m = DynamicQuasarMessage::new("Id");
        id_m.set(1, DynamicValue::String(name.to_string()));
        let mut at_m = DynamicQuasarMessage::new("Attrs");
        at_m.set(2, DynamicValue::Double(val));
        let mut req = DynamicQuasarMessage::new(endpoint);
        req.set(1, DynamicValue::Message(Box::new(id_m)));
        req.set(2, DynamicValue::Message(Box::new(at_m)));
        invoke(&router, endpoint, &admin, raw_req, &req)
    };

    record("eve.space.measurement.api.RecordRequest", "jumps_1h", 30_000_142, 100.0).expect("rec 1");
    record("eve.space.measurement.admin.api.RecordRequest", "jumps_1h", 30_000_144, 50.0).expect("rec 2");
    // Update existing location value
    record("eve.space.measurement.api.RecordRequest", "jumps_1h", 30_000_142, 150.0).expect("rec update");

    // Non-finite rejected
    let err_nan = record("eve.space.measurement.api.RecordRequest", "jumps_1h", 30_000_142, f64::NAN).unwrap_err();
    assert_eq!(err_nan.0, STATUS_BAD_REQUEST);

    // Query top 1
    let mut q_id = DynamicQuasarMessage::new("Id");
    q_id.set(1, DynamicValue::String("jumps_1h".into()));
    let mut q_top = DynamicQuasarMessage::new("eve.space.measurement.api.QueryRequest");
    q_top.set(1, DynamicValue::Message(Box::new(q_id.clone())));
    q_top.set(3, DynamicValue::Uint32(1));
    let rsp_top = invoke(
        &router,
        "eve.space.measurement.api.QueryRequest",
        &user_ctx,
        Vec::new(),
        &q_top,
    )
    .expect("query top 1");
    let wm_top = WireMessage::decode(&rsp_top.value).unwrap();
    assert_eq!(wm_top.fields.len(), 1);

    // Query bottom 1
    let mut q_bot = DynamicQuasarMessage::new("eve.space.measurement.api.QueryRequest");
    q_bot.set(1, DynamicValue::Message(Box::new(q_id)));
    q_bot.set(4, DynamicValue::Uint32(1));
    let rsp_bot = invoke(
        &router,
        "eve.space.measurement.api.QueryRequest",
        &user_ctx,
        Vec::new(),
        &q_bot,
    )
    .expect("query bot 1");
    let wm_bot = WireMessage::decode(&rsp_bot.value).unwrap();
    assert_eq!(wm_bot.fields.len(), 1);
    assert_ne!(rsp_top.value, rsp_bot.value);
}

#[test]
fn space_tags_apply_query_get_tags_get_locations_and_remove() {
    let (router, _db, _sde) = setup_router();
    let user_ctx = user_session_ctx(90_000_001);
    let admin = admin_ctx(90_000_001);

    let jita_loc = encode_space_location(10_000_002, Some(20_000_020), Some(30_000_142));
    let raw_apply = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(jita_loc),
        }],
    }
    .encode();

    // Missing tags -> BAD_REQUEST
    let empty_apply = DynamicQuasarMessage::new("eve.space.tag.admin.api.ApplyRequest");
    let err_empty = invoke(
        &router,
        "eve.space.tag.admin.api.ApplyRequest",
        &admin,
        raw_apply.clone(),
        &empty_apply,
    )
    .unwrap_err();
    assert_eq!(err_empty.0, STATUS_BAD_REQUEST);

    // Apply two tags ("trade_hub", "highsec")
    let make_spec = |t: &str| {
        let mut ti = DynamicQuasarMessage::new("TagId");
        ti.set(1, DynamicValue::String(t.to_string()));
        let mut spec = DynamicQuasarMessage::new("Spec");
        spec.set(1, DynamicValue::Message(Box::new(ti)));
        DynamicValue::Message(Box::new(spec))
    };
    let mut apply_req = DynamicQuasarMessage::new("eve.space.tag.admin.api.ApplyRequest");
    apply_req.set(
        3,
        DynamicValue::Repeated(vec![make_spec("trade_hub"), make_spec("highsec")]),
    );
    invoke(
        &router,
        "eve.space.tag.admin.api.ApplyRequest",
        &admin,
        raw_apply.clone(),
        &apply_req,
    )
    .expect("apply tags");

    // GetTagsRequest for solar system 30_000_142
    let mut sid_m = DynamicQuasarMessage::new("SysId");
    sid_m.set(1, DynamicValue::Uint32(30_000_142));
    let mut req_loc = DynamicQuasarMessage::new("ReqLoc");
    req_loc.set(3, DynamicValue::Message(Box::new(sid_m)));
    let mut gt_req = DynamicQuasarMessage::new("eve.space.location.api.GetTagsRequest");
    gt_req.set(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_loc))]));
    let gt_rsp = invoke(
        &router,
        "eve.space.location.api.GetTagsRequest",
        &user_ctx,
        Vec::new(),
        &gt_req,
    )
    .expect("get tags");
    assert!(!gt_rsp.value.is_empty());

    // GetLocationsRequest for "trade_hub"
    let mut ti = DynamicQuasarMessage::new("TagId");
    ti.set(1, DynamicValue::String("trade_hub".into()));
    let mut gl_req = DynamicQuasarMessage::new("eve.space.tag.api.GetLocationsRequest");
    gl_req.set(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(ti.clone()))]));
    let gl_rsp = invoke(
        &router,
        "eve.space.tag.api.GetLocationsRequest",
        &user_ctx,
        Vec::new(),
        &gl_req,
    )
    .expect("get locations");
    assert!(!gl_rsp.value.is_empty());

    // QueryRequest for "trade_hub"
    let mut term = DynamicQuasarMessage::new("Term");
    term.set(2, DynamicValue::Message(Box::new(ti.clone())));
    let mut tq_req = DynamicQuasarMessage::new("eve.space.tag.api.QueryRequest");
    tq_req.set(1, DynamicValue::Message(Box::new(term)));
    let tq_rsp = invoke(
        &router,
        "eve.space.tag.api.QueryRequest",
        &user_ctx,
        Vec::new(),
        &tq_req,
    )
    .expect("tag query");
    assert!(!tq_rsp.value.is_empty());

    // RemoveRequest for "trade_hub"
    let mut rm_req = DynamicQuasarMessage::new("eve.space.tag.admin.api.RemoveRequest");
    rm_req.set(3, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(ti))]));
    invoke(
        &router,
        "eve.space.tag.admin.api.RemoveRequest",
        &admin,
        raw_apply,
        &rm_req,
    )
    .expect("remove tag");

    // QueryRequest for "trade_hub" is now empty
    let tq_after = invoke(
        &router,
        "eve.space.tag.api.QueryRequest",
        &user_ctx,
        Vec::new(),
        &tq_req,
    )
    .expect("tag query after remove");
    assert!(tq_after.value.is_empty());
}
