//! Unit tests for `quasar_sov_skyhook` (Orbital Skyhook queries, reagent simulation, theft vulnerability windows).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::sov_services::TYPE_SUPERIONIC_ICE;
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
fn skyhook_helpers_and_query_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_query_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let skyhook_id = 79_000_001_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Skyhook".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_reagents
            .insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 1_200));
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 800, 400, 1_000));
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (0, u64::MAX / 2));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, 40_009_080);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
    });

    db.with_snapshot(|snap| {
        assert!(is_skyhook(snap, skyhook_id));
        assert!(!is_skyhook(snap, 999_999));
        assert!(open_window(snap, skyhook_id, 1_000).is_some());
        assert!(!encode_skyhook(snap, &sde, skyhook_id, 1_000).is_empty());
        assert_eq!(sorted_skyhooks(snap, |s| s.owner_corp_id == corp_id), vec![skyhook_id]);
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);

    // GetRequest: found & not found
    let get_resp = handlers["eve_public.sovereignty.skyhook.api.GetRequest"](
        &ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("skyhook GetRequest");
    assert!(get_resp
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.GetResponse"));
    assert!(!get_resp.value.is_empty());

    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetRequest"](
            &ctx,
            &dummy_any,
            &id_msg(1, 999_999_999),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // GetAllLocalRequest: unauth rejected, auth succeeds
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetAllLocalRequest"](
            &unauth_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Req"),
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    let local_resp = handlers["eve_public.sovereignty.skyhook.api.GetAllLocalRequest"](
        &ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetAllLocalRequest");
    assert!(local_resp
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.GetAllLocalResponse"));
    assert!(!local_resp.value.is_empty());

    // GetAllByCorporationRequest
    let corp_resp = handlers["eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest"](
        &ctx,
        &dummy_any,
        &id_msg(1, corp_id),
    )
    .expect("GetAllByCorporationRequest");
    assert!(corp_resp
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse"));
    assert!(!corp_resp.value.is_empty());

    // Theft vulnerable skyhook queries
    let vuln_sys = handlers["eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest"](
        &ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetSolarSystemsWithTheftVulnerableSkyhooksRequest");
    assert!(vuln_sys
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksResponse"));
    assert!(!vuln_sys.value.is_empty());

    let vuln_in_sys = handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
        &ctx,
        &dummy_any,
        &id_msg(1, sys_id as i64),
    )
    .expect("GetTheftVulnerableSkyhooksInSolarSystemRequest");
    assert!(vuln_in_sys
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemResponse"));
    assert!(!vuln_in_sys.value.is_empty());
}
