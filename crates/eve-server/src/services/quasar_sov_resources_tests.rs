//! Unit tests for `quasar_sov_resources` (Planet/Star resource definitions & configurations, Skyhook admin reagent/window endpoints).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
use eve_db::{corp_roles, UpwellStructureRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn id_msg(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn planet_star_resources_and_skyhook_admin_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let skyhook_id = 79_500_001_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = cid;
            __corp });
        corp.members.insert(cid);
        corp.member_roles.insert(cid, corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Admin Skyhook".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // 1. GetAllDefinitionsRequest: no version -> includes definitions; matching version 1.0.0 -> only version
    let all_defs = handlers["eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest"](
        &gm_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetAllDefinitionsRequest");
    assert!(all_defs
        .type_url
        .ends_with("eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"));
    assert!(all_defs.value.len() > 20);

    let mut ver_100 = DynamicQuasarMessage::new("Spec");
    ver_100.fields.insert(1, DynamicValue::Uint32(1));
    ver_100.fields.insert(2, DynamicValue::Uint32(0));
    ver_100.fields.insert(3, DynamicValue::Uint32(0));
    let mut cached_req = DynamicQuasarMessage::new("Req");
    cached_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ver_100.clone())));
    let cached_defs = handlers["eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest"](
        &gm_ctx,
        &dummy_any,
        &cached_req,
    )
    .expect("cached GetAllDefinitionsRequest");
    assert!(cached_defs.value.len() < all_defs.value.len());

    // 2. GetDefinitionsVersionRequest: major 1 ok, major 99 NOT_FOUND
    let ver_ok = handlers["eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest"](
        &gm_ctx,
        &dummy_any,
        &cached_req,
    )
    .expect("GetDefinitionsVersionRequest v1");
    assert!(ver_ok
        .type_url
        .ends_with("eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse"));

    let mut ver_99 = DynamicQuasarMessage::new("Spec");
    ver_99.fields.insert(1, DynamicValue::Uint32(99));
    let mut bad_ver_req = DynamicQuasarMessage::new("Req");
    bad_ver_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ver_99)));
    assert_eq!(
        handlers["eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest"](
            &gm_ctx,
            &dummy_any,
            &bad_ver_req,
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 3 & 4. Planet & Star GetAllConfigurationsRequest
    let planet_cfgs = handlers["eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest"](
        &gm_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("planet GetAllConfigurationsRequest");
    assert!(planet_cfgs
        .type_url
        .ends_with("eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse"));

    let star_cfgs = handlers["eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest"](
        &gm_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("star GetAllConfigurationsRequest");
    assert!(star_cfgs
        .type_url
        .ends_with("eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"));
    assert!(!star_cfgs.value.is_empty());

    // 5. ModifyReagentsRequest
    let mod_resp = handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
        &gm_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("ModifyReagentsRequest");
    assert!(mod_resp
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse"));

    // 6. ScheduleTheftVulnerabilityWindowRequest: valid window vs > MAX_THEFT_WINDOW_SECS
    let now = gm_ctx.now_unix().max(1_000);
    let mut ts_start = DynamicQuasarMessage::new("Timestamp");
    ts_start.fields.insert(1, DynamicValue::Uint64(now + 100));
    let mut ts_end = DynamicQuasarMessage::new("Timestamp");
    ts_end.fields.insert(1, DynamicValue::Uint64(now + 3_700));
    let mut sched_msg = id_msg(1, skyhook_id);
    sched_msg
        .fields
        .insert(2, DynamicValue::Message(Box::new(ts_start.clone())));
    sched_msg
        .fields
        .insert(3, DynamicValue::Message(Box::new(ts_end)));

    let sched_ok = handlers["eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest"](
        &gm_ctx,
        &dummy_any,
        &sched_msg,
    )
    .expect("ScheduleTheftVulnerabilityWindowRequest");
    assert!(sched_ok
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse"));

    let mut ts_too_far = DynamicQuasarMessage::new("Timestamp");
    ts_too_far
        .fields
        .insert(1, DynamicValue::Uint64(now + 100 + MAX_THEFT_WINDOW_SECS + 1));
    sched_msg
        .fields
        .insert(3, DynamicValue::Message(Box::new(ts_too_far)));
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest"](
            &gm_ctx,
            &dummy_any,
            &sched_msg,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}
