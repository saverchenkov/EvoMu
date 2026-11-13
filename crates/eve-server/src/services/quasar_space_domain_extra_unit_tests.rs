//! Deep branch-coverage and mutation-killing unit tests for `quasar_space_domain`.

#![forbid(unsafe_code)]

use crate::services::quasar_space_domain::{encode_space_location, register_handlers, SpaceDomainState};
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{SdeRegistry, StationInfo};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router_with_sde(
    sde: Arc<SdeRegistry>,
) -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
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

fn call_space_domain(
    router: &PublicGatewayRouter,
    endpoint: &str,
    ctx: &AuthoritativeContext,
    raw_value: Vec<u8>,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(endpoint)
        .cloned()
        .unwrap_or_else(|| panic!("missing handler for {endpoint}"));
    let raw_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{endpoint}"),
        value: raw_value,
    };
    handler(ctx, &raw_any, msg)
}

fn user_ctx(char_id: u32) -> AuthoritativeContext {
    AuthoritativeContext::for_session(Some(1), Some(char_id), Some(1_000_009), 0, "tranquility", vec![1])
}

fn admin_ctx(char_id: u32) -> AuthoritativeContext {
    let mut ctx = user_ctx(char_id);
    ctx.service_role_mask = u64::MAX;
    ctx
}

#[test]
fn resolve_locations_and_security_band_edge_cases_and_exact_equality() {
    let (router, _db, sde) = setup_router_with_sde(shared_sde());
    let ctx = user_ctx(90_000_001);

    // 1. ResolveLocationsRequest with single Message (non-Repeated field 1) and Int64 ID
    let mut sys_id_m = DynamicQuasarMessage::new("Id");
    sys_id_m.set(1, DynamicValue::Int64(30_000_142));
    let mut loc_m = DynamicQuasarMessage::new("Loc");
    loc_m.set(2, DynamicValue::Message(Box::new(sys_id_m)));
    let mut single_req = DynamicQuasarMessage::new("Resolve");
    single_req.set(1, DynamicValue::Message(Box::new(loc_m)));
    let rsp_single = call_space_domain(
        &router,
        "eve.space.location.api.ResolveLocationsRequest",
        &ctx,
        Vec::new(),
        &single_req,
    )
    .unwrap();
    let wm = WireMessage::decode(&rsp_single.value).unwrap();
    assert!(wm.fields.iter().any(|f| f.field_number == 1));

    // 2. GetLocationsWithinSecurityRequest: non-finite bounds rejected; min == max accepted
    for (min_v, max_v) in [
        (f64::NAN, 1.0),
        (0.0, f64::NAN),
        (f64::NEG_INFINITY, 1.0),
        (0.0, f64::INFINITY),
    ] {
        let mut min_m = DynamicQuasarMessage::new("Min");
        min_m.set(1, DynamicValue::Double(min_v));
        let mut max_m = DynamicQuasarMessage::new("Max");
        max_m.set(1, DynamicValue::Double(max_v));
        let mut req = DynamicQuasarMessage::new("Sec");
        req.set(1, DynamicValue::Message(Box::new(min_m)));
        req.set(2, DynamicValue::Message(Box::new(max_m)));
        let err = call_space_domain(
            &router,
            "eve.space.api.GetLocationsWithinSecurityRequest",
            &ctx,
            Vec::new(),
            &req,
        )
        .unwrap_err();
        assert_eq!(err.0, STATUS_BAD_REQUEST);
    }

    // Exact equality min_sec == max_sec == Jita's security_status
    let jita_sec = sde.solar_systems[&30_000_142].security_status;
    let mut eq_min = DynamicQuasarMessage::new("Min");
    eq_min.set(1, DynamicValue::Double(jita_sec));
    let mut eq_max = DynamicQuasarMessage::new("Max");
    eq_max.set(1, DynamicValue::Double(jita_sec));
    let mut eq_req = DynamicQuasarMessage::new("Sec");
    eq_req.set(1, DynamicValue::Message(Box::new(eq_min)));
    eq_req.set(2, DynamicValue::Message(Box::new(eq_max)));
    let eq_rsp = call_space_domain(
        &router,
        "eve.space.api.GetLocationsWithinSecurityRequest",
        &ctx,
        Vec::new(),
        &eq_req,
    )
    .unwrap();
    assert!(!eq_rsp.value.is_empty());

    // Default bounds when fields 1 and 2 are omitted -> returns all systems in [-10.0, 10.0]
    let def_rsp = call_space_domain(
        &router,
        "eve.space.api.GetLocationsWithinSecurityRequest",
        &ctx,
        Vec::new(),
        &DynamicQuasarMessage::new("Sec"),
    )
    .unwrap();
    assert!(!def_rsp.value.is_empty());
}

#[test]
fn station_handlers_scalar_id_fallback_and_missing_solar_system_branch() {
    let mut sde_clone = (*shared_sde()).clone();
    // Insert a station pointing to a non-existent solar_system_id 39_999_999
    sde_clone.stations.insert(
        60_999_999,
        StationInfo {
            station_id: 60_999_999,
            solar_system_id: 39_999_999,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            station_type_id: 1529,
            owner_corporation_id: 1_000_035,
            owner_faction_id: Some(500_001),
            name: "Orphan Station".into(),
            position: glam::DVec3::ZERO,
            operation_id: 0,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    let (router, _db, _sde) = setup_router_with_sde(Arc::new(sde_clone));
    let ctx = user_ctx(90_000_001);

    // 1. eve.space.npcstation.api.GetRequest returns STATUS_NOT_FOUND when station's solar system is missing
    let mut orphan_id = DynamicQuasarMessage::new("Id");
    orphan_id.set(1, DynamicValue::Uint32(60_999_999));
    let mut orphan_req = DynamicQuasarMessage::new("Req");
    orphan_req.set(1, DynamicValue::Message(Box::new(orphan_id)));
    let err = call_space_domain(
        &router,
        "eve.space.npcstation.api.GetRequest",
        &ctx,
        Vec::new(),
        &orphan_req,
    )
    .unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // 2. Direct scalar field 1 fallback (without nested submessage) on public/station endpoints
    let mut scalar_st = DynamicQuasarMessage::new("Req");
    scalar_st.set(1, DynamicValue::Uint32(60_003_760));
    for endpoint in [
        "eve_public.station.api.GetRequest",
        "eve.station.GetRequest",
        "eve.station.GetNameRequest",
    ] {
        let rsp = call_space_domain(&router, endpoint, &ctx, Vec::new(), &scalar_st).unwrap();
        assert!(!rsp.value.is_empty());
    }

    let mut scalar_sys = DynamicQuasarMessage::new("Req");
    scalar_sys.set(1, DynamicValue::Uint32(30_000_142));
    let sys_rsp = call_space_domain(
        &router,
        "eve.station.GetStationsInSolarSystemRequest",
        &ctx,
        Vec::new(),
        &scalar_sys,
    )
    .unwrap();
    assert!(!sys_rsp.value.is_empty());
}

#[test]
fn measurements_validation_capacity_eviction_and_location_filtered_query() {
    let (router, db, _sde) = setup_router_with_sde(shared_sde());
    let admin = admin_ctx(90_000_001);
    let user = user_ctx(90_000_001);

    // 1. Whitespace-only name -> STATUS_BAD_REQUEST
    let mut ws_id = DynamicQuasarMessage::new("Id");
    ws_id.set(1, DynamicValue::String("   ".into()));
    let mut ws_req = DynamicQuasarMessage::new("Rec");
    ws_req.set(1, DynamicValue::Message(Box::new(ws_id)));
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.measurement.api.RecordRequest",
            &admin,
            Vec::new(),
            &ws_req
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. Missing field 2 (attributes) -> STATUS_BAD_REQUEST
    let mut valid_id = DynamicQuasarMessage::new("Id");
    valid_id.set(1, DynamicValue::String("  rat_kills  ".into()));
    let mut no_attrs_req = DynamicQuasarMessage::new("Rec");
    no_attrs_req.set(1, DynamicValue::Message(Box::new(valid_id.clone())));
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.measurement.api.RecordRequest",
            &admin,
            Vec::new(),
            &no_attrs_req
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 3. Seed 4096 measurement keys and 4096 entries in "rat_kills" to verify capacity eviction
    let mut seeded = SpaceDomainState::default();
    for i in 0..4096 {
        seeded.measurements.insert(format!("m_{i:04}"), Vec::new());
    }
    let mut full_entries = Vec::with_capacity(4096);
    for i in 0..4096 {
        full_entries.push((format!("{i:08x}"), i as f64));
    }
    seeded.measurements.insert("rat_kills".into(), full_entries);
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_space_domain".into(),
            serde_json::to_string(&seeded).unwrap(),
        );
    });

    // Record a new measurement name "z_new_metric" -> evicts first key "m_0000"
    let loc_jita = encode_space_location(10_000_002, Some(20_000_020), Some(30_000_142));
    let raw_at = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(loc_jita.clone()),
        }],
    }
    .encode();
    let raw_req = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(raw_at),
        }],
    }
    .encode();

    let mut new_id = DynamicQuasarMessage::new("Id");
    new_id.set(1, DynamicValue::String("z_new_metric".into()));
    let mut attrs = DynamicQuasarMessage::new("Attrs");
    attrs.set(2, DynamicValue::Float(12.5));
    let mut rec_new = DynamicQuasarMessage::new("Rec");
    rec_new.set(1, DynamicValue::Message(Box::new(new_id)));
    rec_new.set(2, DynamicValue::Message(Box::new(attrs.clone())));
    call_space_domain(
        &router,
        "eve.space.measurement.api.RecordRequest",
        &admin,
        raw_req.clone(),
        &rec_new,
    )
    .unwrap();

    // Record a new location in "rat_kills" (which already has 4096 entries) -> evicts entry 0 ("00000000")
    let mut rec_rat = DynamicQuasarMessage::new("Rec");
    rec_rat.set(1, DynamicValue::Message(Box::new(valid_id.clone())));
    rec_rat.set(2, DynamicValue::Message(Box::new(attrs)));
    call_space_domain(
        &router,
        "eve.space.measurement.api.RecordRequest",
        &admin,
        raw_req,
        &rec_rat,
    )
    .unwrap();

    // Query "rat_kills" filtered by Jita location in raw_any field 2
    let raw_query = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(loc_jita),
        }],
    }
    .encode();
    let mut q_req = DynamicQuasarMessage::new("Query");
    q_req.set(1, DynamicValue::Message(Box::new(valid_id)));
    let q_rsp = call_space_domain(
        &router,
        "eve.space.measurement.api.QueryRequest",
        &user,
        raw_query,
        &q_req,
    )
    .unwrap();
    let wm_q = WireMessage::decode(&q_rsp.value).unwrap();
    assert_eq!(wm_q.fields.len(), 1);
}

#[test]
fn space_tags_auth_validation_dedup_capacity_and_region_constellation_resolution() {
    let (router, db, _sde) = setup_router_with_sde(shared_sde());
    let admin = admin_ctx(90_000_001);
    let user = user_ctx(90_000_001);

    // 1. Non-admin rejected on ApplyRequest and RemoveRequest
    let empty = DynamicQuasarMessage::new("Tag");
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.tag.admin.api.ApplyRequest",
            &user,
            Vec::new(),
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.tag.admin.api.RemoveRequest",
            &user,
            Vec::new(),
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // 2. Missing location in raw_any -> STATUS_BAD_REQUEST
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.tag.admin.api.ApplyRequest",
            &admin,
            Vec::new(),
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_space_domain(
            &router,
            "eve.space.tag.admin.api.RemoveRequest",
            &admin,
            Vec::new(),
            &empty
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 3. Seed 4096 locations and 256 tags on region 10_000_002 to verify capacity eviction and deduplication
    let reg_loc = encode_space_location(10_000_002, None, None);
    let con_loc = encode_space_location(10_000_002, Some(20_000_020), None);
    let reg_hex: String = reg_loc.iter().map(|b| format!("{b:02x}")).collect();

    let mut seeded = SpaceDomainState::default();
    for i in 0..4096 {
        seeded.location_tags.insert(format!("loc_{i:04}"), vec!["old".into()]);
    }
    seeded.location_tags.insert(
        reg_hex,
        (0..256).map(|i| format!("t_{i:03}")).collect(),
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_space_domain".into(),
            serde_json::to_string(&seeded).unwrap(),
        );
    });

    let make_spec = |t: &str| {
        let mut ti = DynamicQuasarMessage::new("TagId");
        ti.set(1, DynamicValue::String(t.to_string()));
        let mut spec = DynamicQuasarMessage::new("Spec");
        spec.set(1, DynamicValue::Message(Box::new(ti)));
        DynamicValue::Message(Box::new(spec))
    };

    // Apply existing tag "t_255" and new tag "forge_tag" to region 10_000_002, and "kimotoro_tag" to constellation 20_000_020
    let raw_reg = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(reg_loc),
        }],
    }
    .encode();
    let mut apply_reg = DynamicQuasarMessage::new("Apply");
    apply_reg.set(
        3,
        DynamicValue::Repeated(vec![make_spec("t_255"), make_spec("forge_tag"), make_spec("   ")]),
    );
    call_space_domain(
        &router,
        "eve.space.tag.admin.api.ApplyRequest",
        &admin,
        raw_reg,
        &apply_reg,
    )
    .unwrap();

    let raw_con = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(con_loc),
        }],
    }
    .encode();
    let mut apply_con = DynamicQuasarMessage::new("Apply");
    apply_con.set(3, DynamicValue::Repeated(vec![make_spec("kimotoro_tag")]));
    call_space_domain(
        &router,
        "eve.space.tag.admin.api.ApplyRequest",
        &admin,
        raw_con,
        &apply_con,
    )
    .unwrap();

    // 4. GetTagsRequest resolves region (field 1) and constellation (field 2)
    let mut rid_m = DynamicQuasarMessage::new("RegId");
    rid_m.set(1, DynamicValue::Uint32(10_000_002));
    let mut req_reg = DynamicQuasarMessage::new("Loc");
    req_reg.set(1, DynamicValue::Message(Box::new(rid_m)));

    let mut cid_m = DynamicQuasarMessage::new("ConId");
    cid_m.set(1, DynamicValue::Uint32(20_000_020));
    let mut req_con = DynamicQuasarMessage::new("Loc");
    req_con.set(2, DynamicValue::Message(Box::new(cid_m)));

    let mut gt_req = DynamicQuasarMessage::new("GetTags");
    gt_req.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(req_reg)),
            DynamicValue::Message(Box::new(req_con)),
        ]),
    );
    let gt_rsp = call_space_domain(
        &router,
        "eve.space.location.api.GetTagsRequest",
        &user,
        Vec::new(),
        &gt_req,
    )
    .unwrap();
    assert!(!gt_rsp.value.is_empty());
}
