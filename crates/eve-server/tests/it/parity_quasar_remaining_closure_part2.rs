//! End-to-end parity closure tests for Quasar `sovereignty`, `space`,
//! `store`, and `user` domains (part 2).

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b11_part2::{quasar_rpc, select_char};
use crate::parity_quasar_remaining_closure::{
    drain_notices, encode_double_field, encode_seq_id,
};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_machonet::service_roles::ROLE_GM_MASK;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_auth_user::{
    activate_user_omega_license, expire_user_omega_license,
};

const P1: i64 = 90_000_001;
const JITA: i64 = 30_000_142;
const JITA_4_4: i64 = 60_003_760;

#[tokio::test]
async fn test_quasar_sovereignty_and_space_domains_e2e() {
    // parity: feat:quasar.sovereignty
    // parity: feat:quasar.space
    eve_testkit::attest(&["feat:quasar.sovereignty", "feat:quasar.space"]);

    let ctx = ServerContext::new();
    let mut p1 = select_char(&ctx, 81_006, "demo", "demo-secret", P1).await;
    p1.role |= ROLE_GM_MASK;
    let mut rx1 = ctx.quasar_gateway.subscribe_session(&p1, vec![], ctx.now_unix());

    // Sovereignty: planet & star resource definitions, skyhooks, upgrades, ADM
    let mut no_known = Vec::new();
    encode_varint_field(2, 1, &mut no_known);
    let planet_defs = quasar_rpc(
        &ctx,
        &mut p1,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        no_known.clone(),
    )
    .await;
    assert_eq!(planet_defs.status_code, STATUS_OK);
    assert!(!planet_defs.payload.expect("planet defs").value.is_empty());

    let star_cfgs = quasar_rpc(
        &ctx,
        &mut p1,
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        no_known.clone(),
    )
    .await;
    assert_eq!(star_cfgs.status_code, STATUS_OK);
    assert!(!star_cfgs.payload.expect("star cfgs").value.is_empty());

    let upg_defs = quasar_rpc(
        &ctx,
        &mut p1,
        "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
        no_known,
    )
    .await;
    assert_eq!(upg_defs.status_code, STATUS_OK);

    let mut sys_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(JITA as u64), &mut sys_req);
    let mut corp_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(1_000_001), &mut corp_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
            corp_req
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
            vec![]
        )
        .await
        .status_code,
        STATUS_OK
    );
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|sys| sys.security_status <= 0.0 && (30_000_001..=30_005_500).contains(&sys.solar_system_id))
        .expect("nullsec system")
        .solar_system_id;
    let mut sys_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(u64::from(null_sys)), &mut sys_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
            sys_req
        )
        .await
        .status_code,
        STATUS_OK
    );

    // Space: CDN, ResolveLocations, GetAllSolarSystems, GetLocationsWithinSecurity, NPCStation, Measurements, Tags
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.space.api.cdn.GetUpdateRequest", cdn_req)
            .await
            .status_code,
        STATUS_OK
    );

    let mut loc_entry = Vec::new();
    encode_bytes_field(2, &encode_seq_id(JITA as u64), &mut loc_entry);
    let mut res_req = Vec::new();
    encode_bytes_field(1, &loc_entry, &mut res_req);
    let res_rsp = quasar_rpc(&ctx, &mut p1, "eve.space.location.api.ResolveLocationsRequest", res_req).await;
    assert_eq!(res_rsp.status_code, STATUS_OK);
    assert!(!res_rsp.payload.expect("resolved").value.is_empty());

    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.space.location.api.GetAllSolarSystemsRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );

    let (mut min_s, mut max_s, mut sec_req) = (Vec::new(), Vec::new(), Vec::new());
    min_s.push((1 << 3) | 5);
    min_s.extend_from_slice(&0.8f32.to_le_bytes());
    max_s.push((1 << 3) | 5);
    max_s.extend_from_slice(&1.0f32.to_le_bytes());
    encode_bytes_field(1, &min_s, &mut sec_req);
    encode_bytes_field(2, &max_s, &mut sec_req);
    let sec_rsp = quasar_rpc(&ctx, &mut p1, "eve.space.api.GetLocationsWithinSecurityRequest", sec_req).await;
    assert_eq!(sec_rsp.status_code, STATUS_OK);
    assert!(!sec_rsp.payload.expect("highsec").value.is_empty());

    let mut st_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(JITA_4_4 as u64), &mut st_req);
    let st_rsp = quasar_rpc(&ctx, &mut p1, "eve.space.npcstation.api.GetRequest", st_req).await;
    assert_eq!(st_rsp.status_code, STATUS_OK);
    let st_wm = WireMessage::decode(&st_rsp.payload.expect("st rsp").value).expect("st wm");
    let jita_loc = st_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .expect("jita loc")
        .value
        .as_bytes()
        .expect("bytes")
        .to_vec();

    // Measurement Record + Query + Notice
    let (mut m_id, mut m_attr, mut m_rec) = (Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "npc_bounty_index", &mut m_id);
    encode_bytes_field(1, &jita_loc, &mut m_attr);
    encode_double_field(2, 1.25, &mut m_attr);
    encode_bytes_field(1, &m_id, &mut m_rec);
    encode_bytes_field(2, &m_attr, &mut m_rec);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.space.measurement.api.RecordRequest", m_rec)
            .await
            .status_code,
        STATUS_OK
    );
    let space_notices = drain_notices(&ctx, &mut rx1);
    assert!(space_notices.iter().any(|n| {
        n.payload
            .as_ref()
            .is_some_and(|p| p.type_url.contains("eve.space.measurement.api.Recorded"))
    }));

    let mut m_query = Vec::new();
    encode_bytes_field(1, &m_id, &mut m_query);
    let mq_rsp = quasar_rpc(&ctx, &mut p1, "eve.space.measurement.api.QueryRequest", m_query.clone()).await;
    assert_eq!(mq_rsp.status_code, STATUS_OK);
    assert!(!mq_rsp.payload.expect("m query").value.is_empty());

    // Space Tags Apply + GetTags
    let (mut t_id, mut t_spec, mut t_apply) = (Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "trade_hub", &mut t_id);
    encode_bytes_field(1, &t_id, &mut t_spec);
    encode_varint_field(2, 1, &mut t_spec);
    encode_bytes_field(2, &jita_loc, &mut t_apply);
    encode_bytes_field(3, &t_spec, &mut t_apply);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.space.tag.admin.api.ApplyRequest", t_apply)
            .await
            .status_code,
        STATUS_OK
    );

    let mut gt_loc = Vec::new();
    encode_bytes_field(3, &encode_seq_id(JITA as u64), &mut gt_loc);
    let mut get_tags_req = Vec::new();
    encode_bytes_field(1, &gt_loc, &mut get_tags_req);
    let gt_rsp = quasar_rpc(&ctx, &mut p1, "eve.space.location.api.GetTagsRequest", get_tags_req).await;
    assert_eq!(gt_rsp.status_code, STATUS_OK);
    assert!(!gt_rsp.payload.expect("tags").value.is_empty());

    // Persistence across SQLite reload
    let ctx_re = persist_and_reopen_sqlite(&ctx, "quasar_sov_space_closure_re");
    let mut p1_re = select_char(&ctx_re, 81_007, "demo", "demo-secret", P1).await;
    let mq_re = quasar_rpc(&ctx_re, &mut p1_re, "eve.space.measurement.api.QueryRequest", m_query).await;
    assert_eq!(mq_re.status_code, STATUS_OK);
    assert!(!mq_re.payload.expect("reloaded measurement").value.is_empty());
}

#[tokio::test]
async fn test_quasar_store_and_user_domains_e2e() {
    // parity: feat:quasar.store
    // parity: feat:quasar.user
    eve_testkit::attest(&["feat:quasar.store", "feat:quasar.user"]);

    let ctx = ServerContext::new();
    let mut p1 = select_char(&ctx, 81_008, "demo", "demo-secret", P1).await;
    p1.role |= ROLE_GM_MASK;
    let mut rx1 = ctx.quasar_gateway.subscribe_session(&p1, vec![], ctx.now_unix());

    // Store: CreateCategory, CreateProduct, CreateOffer -> GetAll / Get / Promote / Retract
    let (mut cat_attrs, mut cat_req) = (Vec::new(), Vec::new());
    encode_string_field(1, "Apparel", &mut cat_attrs);
    encode_bytes_field(1, &cat_attrs, &mut cat_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.store.admin.api.CreateCategoryRequest", cat_req)
            .await
            .status_code,
        STATUS_OK
    );
    let (mut prod_item, mut prod_attrs, mut prod_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &encode_seq_id(44_992), &mut prod_item);
    encode_string_field(1, "Capsuleer Jacket", &mut prod_attrs);
    encode_bytes_field(2, &prod_item, &mut prod_attrs);
    encode_varint_field(1, 1, &mut prod_req);
    encode_bytes_field(2, &prod_attrs, &mut prod_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.store.admin.api.CreateProductRequest", prod_req)
            .await
            .status_code,
        STATUS_OK
    );
    let (mut plex_price, mut off_attrs, mut off_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 12_000, &mut plex_price);
    encode_string_field(1, "Jacket Offer", &mut off_attrs);
    encode_bytes_field(5, &plex_price, &mut off_attrs);
    encode_bytes_field(1, &off_attrs, &mut off_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.store.admin.api.CreateOfferRequest", off_req)
            .await
            .status_code,
        STATUS_OK
    );
    let cats = quasar_rpc(&ctx, &mut p1, "eve_public.store.category.api.GetAllRequest", vec![]).await;
    assert_eq!(cats.status_code, STATUS_OK);
    assert!(!cats.payload.expect("cats").value.is_empty());
    let offs = quasar_rpc(&ctx, &mut p1, "eve_public.store.offer.api.GetAllRequest", vec![]).await;
    assert_eq!(offs.status_code, STATUS_OK);
    assert!(!offs.payload.expect("offs").value.is_empty());

    let (mut cat_sub, mut ta_id, mut prom_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "featured_omega", &mut cat_sub);
    encode_bytes_field(1, &encode_seq_id(1), &mut ta_id);
    encode_bytes_field(2, &cat_sub, &mut ta_id);
    encode_bytes_field(1, &ta_id, &mut prom_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve.store.targetaudience.offer.api.admin.PromoteRequest",
            prom_req.clone()
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve.store.targetaudience.offer.api.admin.RetractRequest",
            prom_req
        )
        .await
        .status_code,
        STATUS_OK
    );

    // User: License activation + ActivatedNotice, Legal agreement Accept + GetLatest, Character Transfer, Expiry
    let now = ctx.now_unix();
    activate_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, 1, 30 * 86_400, now);
    let lic_notices = drain_notices(&ctx, &mut rx1);
    assert!(lic_notices.iter().any(|n| {
        n.payload
            .as_ref()
            .is_some_and(|p| p.type_url.contains("user.license.api.ActivatedNotice"))
    }));
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.user.license.api.GetRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );

    let (mut doc_id, mut accept_req) = (Vec::new(), Vec::new());
    encode_string_field(1, "eula_2026_v1", &mut doc_id);
    encode_bytes_field(3, &doc_id, &mut accept_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.user.legal.agreement.api.AcceptRequest", accept_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.legal.agreement.api.GetLatestRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );

    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.user.character.transfer.GetAvailabilityRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.user.character.transfer.GetPendingByUserRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );

    // Persistence across SQLite reload + license expiry
    let ctx_re = persist_and_reopen_sqlite(&ctx, "quasar_store_user_closure_re");
    let mut p1_re = select_char(&ctx_re, 81_009, "demo", "demo-secret", P1).await;
    assert_eq!(
        quasar_rpc(&ctx_re, &mut p1_re, "eve_public.store.offer.api.GetAllRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    let mut rx_re = ctx_re.quasar_gateway.subscribe_session(&p1_re, vec![], ctx_re.now_unix());
    expire_user_omega_license(&ctx_re.db, &ctx_re.quasar_gateway.pending_notices, 1);
    let exp_notices = drain_notices(&ctx_re, &mut rx_re);
    assert!(exp_notices.iter().any(|n| {
        n.payload
            .as_ref()
            .is_some_and(|p| p.type_url.contains("user.license.api.ExpiredNotice"))
    }));
}
