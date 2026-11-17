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
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = cid;
                __corp },
        );
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

#[test]
fn destroyed_skyhook_exclusion_and_lazy_advancement_across_queries() {
    let sys_id = 30_000_669_u32; // N3-JBX
    let sde = shared_sde();
    let sys_cels = sde.celestials_in_system_slice(sys_id);
    let lava_planet = sys_cels
        .iter()
        .find(|c| c.type_id == 2015)
        .expect("N3-JBX lava planet")
        .celestial_id;
    let temp_planet = sys_cels
        .iter()
        .find(|c| c.type_id == 11)
        .expect("N3-JBX temperate planet")
        .celestial_id;

    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_query_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let sh_live_reagent = 79_100_001_i64;
    let sh_live_workforce = 79_100_002_i64;
    let sh_destroyed = 79_100_003_i64;
    let t0 = 1_850_000_000_u64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        snap.corporations.entry(corp_id).or_default().ceo_id = cid;
        for (sh_id, st_state) in [
            (sh_live_reagent, "ShieldVulnerable"),
            (sh_live_workforce, "ShieldVulnerable"),
            (sh_destroyed, "Destroyed"),
        ] {
            snap.structures.insert(
                sh_id,
                UpwellStructureRecord {
                    structure_id: sh_id,
                    type_id: TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Skyhook {sh_id}"),
                    state: st_state.into(),
                    ..Default::default()
                },
            );
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }
        snap.sov_runtime.skyhook_planets.insert(sh_live_reagent, lava_planet);
        snap.sov_runtime
            .skyhook_planets
            .insert(sh_live_workforce, temp_planet);
        snap.sov_runtime.skyhook_planets.insert(sh_destroyed, temp_planet);
        snap.sov_runtime
            .skyhook_reagents
            .insert(sh_live_reagent, (crate::services::sov_services::TYPE_MAGMATIC_GAS, 0));
        snap.sov_runtime.skyhook_reagent_bays.insert(
            sh_live_reagent,
            (crate::services::sov_services::TYPE_MAGMATIC_GAS, 0, 0, t0),
        );
        snap.sov_runtime.skyhook_last_harvest_unix.insert(sh_live_reagent, t0);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_live_workforce, (t0, t0 + 3_600));
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_destroyed, (t0, u64::MAX / 2));

        // Destroyed Skyhook must be excluded even if called directly via encode_skyhook_with_mode
        assert!(!is_skyhook(snap, sh_destroyed));
        assert_eq!(
            sorted_skyhooks(snap, |s| s.owner_corp_id == corp_id),
            vec![sh_live_reagent, sh_live_workforce]
        );
        let dead_wm = WireMessage::decode(&encode_skyhook(snap, &sde, sh_destroyed, t0)).unwrap();
        let active_val = dead_wm
            .fields
            .iter()
            .find(|f| f.field_number == 3)
            .and_then(|f| f.value.as_varint());
        assert_eq!(active_val, Some(0), "destroyed Skyhook must not encode active=true");
        let wf_bytes = dead_wm
            .fields
            .iter()
            .find(|f| f.field_number == 7)
            .and_then(|f| f.value.as_bytes())
            .expect("workforce field");
        let wf_wm = WireMessage::decode(wf_bytes).unwrap();
        assert_eq!(
            wf_wm
                .fields
                .iter()
                .find(|f| f.field_number == 2)
                .and_then(|f| f.value.as_varint()),
            Some(1),
            "destroyed Skyhook must encode workforce.none=true"
        );
        assert!(wf_wm.fields.iter().all(|f| f.field_number != 1));
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let ctx_at = |ts: u64| {
        AuthoritativeContext::for_session_at(
            Some(1),
            Some(cid as u32),
            Some(corp_id as u32),
            corp_roles::ROLE_DIRECTOR,
            "tranquility",
            vec![1],
            ts,
        )
    };
    let ctx0 = ctx_at(t0);

    // Destroyed Skyhook is 404 on GetRequest and excluded from GetAllLocal / GetAllByCorporation / vuln queries
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetRequest"](
            &ctx0,
            &dummy_any,
            &id_msg(1, sh_destroyed),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        WireMessage::decode(
            &handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
                &ctx0,
                &dummy_any,
                &id_msg(1, i64::from(sys_id)),
            )
            .unwrap()
            .value
        )
        .unwrap()
        .fields
        .iter()
        .filter(|f| f.field_number == 1)
        .count(),
        1,
        "only non-destroyed sh_live_workforce is vulnerable at t0"
    );

    // Advance clock 2 hours WITHOUT running tick_skyhooks:
    // 1) GetAllLocalRequest lazily harvests/matures reagents on sh_live_reagent
    let ctx2h = ctx_at(t0 + 7_200);
    let local_bytes = handlers["eve_public.sovereignty.skyhook.api.GetAllLocalRequest"](
        &ctx2h,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap()
    .value;
    let local_wm = WireMessage::decode(&local_bytes).unwrap();
    let local_items: Vec<&[u8]> = local_wm
        .fields
        .iter()
        .filter(|f| f.field_number == 2)
        .filter_map(|f| f.value.as_bytes())
        .collect();
    assert_eq!(local_items.len(), 2, "destroyed Skyhook must be absent from GetAllLocalResponse");
    db.with_snapshot(|snap| {
        let (_, sec, insec, _) = snap.sov_runtime.skyhook_reagent_bays[&sh_live_reagent];
        assert!(sec > 0 && insec > 0, "GetAllLocalRequest must lazily advance reagent stocks");
    });

    // Advance another 1h and check GetAllByCorporationRequest also lazily advances reagents and excludes destroyed
    let ctx3h = ctx_at(t0 + 10_800);
    let corp_bytes = handlers["eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest"](
        &ctx3h,
        &dummy_any,
        &id_msg(1, corp_id),
    )
    .unwrap()
    .value;
    let corp_wm = WireMessage::decode(&corp_bytes).unwrap();
    let corp_items = corp_wm.fields.iter().filter(|f| f.field_number == 1).count();
    assert_eq!(corp_items, 2, "destroyed Skyhook must be absent from GetAllByCorporationResponse");
    let hourly = planet_hourly_reagent_yield(&sde, Some(lava_planet), Some(sys_id), crate::services::sov_services::TYPE_MAGMATIC_GAS);
    db.with_snapshot(|snap| {
        let (_, sec, insec, _) = snap.sov_runtime.skyhook_reagent_bays[&sh_live_reagent];
        assert_eq!(sec + insec, 3 * hourly);
    });

    // Non-reagent Skyhook sh_live_workforce had theft window [t0, t0 + 3_600]; at t0 + 10_800
    // vulnerability queries lazily roll expired windows forward instead of returning stale openness.
    let vuln_sys = handlers["eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest"](
        &ctx3h,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap()
    .value;
    assert!(
        WireMessage::decode(&vuln_sys).unwrap().fields.is_empty(),
        "expired theft window must be lazily rolled forward without waiting for tick_skyhooks"
    );
    let vuln_in_sys = handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
        &ctx3h,
        &dummy_any,
        &id_msg(1, i64::from(sys_id)),
    )
    .unwrap()
    .value;
    assert!(WireMessage::decode(&vuln_in_sys).unwrap().fields.is_empty());
}
