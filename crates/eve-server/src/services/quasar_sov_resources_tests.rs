//! Unit tests for `quasar_sov_resources` (Planet/Star resource definitions & configurations, Skyhook admin reagent/window endpoints).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
use eve_db::{corp_roles, UpwellStructureRecord};
use eve_proto::pb;
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
    let sys_id = 30_000_214_u32; // HE-V4V (sovereign null-sec with Ice, Lava, Temperate, Oceanic)
    let lava_planet = sde
        .celestials_in_system_slice(sys_id)
        .iter()
        .find(|c| c.type_id == 2015)
        .expect("HE-V4V lava planet")
        .celestial_id;

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
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, lava_planet);
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

    // 1. GetAllDefinitionsRequest: no version -> includes nullsec celestial definitions; matching version 1.0.0 -> 304 Not Modified
    let all_defs = handlers["eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest"](
        &gm_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetAllDefinitionsRequest");
    assert!(all_defs
        .type_url
        .ends_with("eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"));
    let all_defs_wire = WireMessage::decode(&all_defs.value).expect("decode GetAllDefinitionsResponse");
    let def_entries: Vec<&[u8]> = all_defs_wire
        .fields
        .iter()
        .filter_map(|f| match (&f.field_number, &f.value) {
            (1, eve_proto::quasar::WireValue::LengthDelimited(b)) => Some(b.as_slice()),
            _ => None,
        })
        .collect();
    assert!(def_entries.len() >= 1_000, "all null-sec planet celestials returned");
    let lava_cel = sde.celestial(lava_planet).expect("lava celestial");
    let expected_lava_yield = planet_hourly_reagent_yield_for_planet(lava_cel.type_id, lava_cel.radius);
    let expected_lava_amount = pb::message("eve_public.sovereignty.resource.planet.reagent.Definition")
        .set("amount_per_cycle", expected_lava_yield as u32)
        .must_encode();
    let expected_lava_sec_pct = pb::message("eve_public.sovereignty.resource.planet.reagent.Definition")
        .set("secured_percentage", 50u32)
        .must_encode();
    assert!(
        all_defs
            .value
            .windows(expected_lava_amount.len())
            .any(|w| w == expected_lava_amount.as_slice()),
        "Lava celestial definition must encode radius-scaled reagent yield"
    );
    assert!(
        all_defs
            .value
            .windows(expected_lava_sec_pct.len())
            .any(|w| w == expected_lava_sec_pct.as_slice()),
        "Reagent definition must encode secured_percentage = 50"
    );

    let mut ver_100 = DynamicQuasarMessage::new("Spec");
    ver_100.fields.insert(1, DynamicValue::Uint32(1));
    ver_100.fields.insert(2, DynamicValue::Uint32(0));
    ver_100.fields.insert(3, DynamicValue::Uint32(0));
    let mut cached_req = DynamicQuasarMessage::new("Req");
    cached_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ver_100.clone())));
    assert_eq!(
        handlers["eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest"](
            &gm_ctx,
            &dummy_any,
            &cached_req,
        )
        .unwrap_err()
        .0,
        STATUS_NOT_MODIFIED
    );

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

    // 5. ModifyReagentsRequest (valid omission default + negative validation guards)
    let mod_resp = handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
        &gm_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("ModifyReagentsRequest");
    assert!(mod_resp
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse"));

    // Empty reagents list (explicit empty vec, invalid type/capacity, or non-reagent planet on omitted reagents) -> STATUS_BAD_REQUEST
    let temp_planet = sde
        .celestials_in_system_slice(sys_id)
        .iter()
        .find(|c| c.type_id == 11)
        .expect("HE-V4V temperate planet")
        .celestial_id;
    let non_reagent_skyhook_id = 79_500_002_i64;
    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            non_reagent_skyhook_id,
            UpwellStructureRecord {
                structure_id: non_reagent_skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Temperate Skyhook".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_planets
            .insert(non_reagent_skyhook_id, temp_planet);
    });
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
            &gm_ctx,
            &dummy_any,
            &id_msg(1, non_reagent_skyhook_id),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST,
        "ModifyReagentsRequest omitting reagents must reject planets without harvestable reagents"
    );
    let mut bad_type_reag = id_msg(1, 34);
    bad_type_reag.fields.insert(2, DynamicValue::Uint64(100));
    let mut bad_type_req = id_msg(1, skyhook_id);
    bad_type_req.fields.insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(bad_type_reag))]));
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
            &gm_ctx,
            &dummy_any,
            &bad_type_req,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let mut overcap_reag = id_msg(1, i64::from(TYPE_MAGMATIC_GAS));
    overcap_reag.fields.insert(2, DynamicValue::Uint64(SKYHOOK_REAGENT_BAY_CAPACITY + 1));
    let mut overcap_req = id_msg(1, skyhook_id);
    overcap_req.fields.insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(overcap_reag))]));
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
            &gm_ctx,
            &dummy_any,
            &overcap_req,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

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

#[test]
fn encode_planet_definitions_and_register_handlers_contract() {
    let sde = shared_sde();
    let mut encoded = Vec::new();
    encode_planet_definitions(&sde, &mut encoded);
    assert!(!encoded.is_empty());
    let planets = sovereign_planet_definitions(&sde);
    assert!(planets.len() >= 1_000);
    for cel in planets.iter().take(16) {
        let (power, workforce) = planet_yield_for_planet(cel.type_id, cel.radius);
        let expected_def = pb::message("eve_public.sovereignty.resource.planet.Definition")
            .set("planet.sequential", cel.celestial_id as u64)
            .set("power", power)
            .set("workforce", workforce)
            .must_encode();
        assert!(
            encoded
                .windows(expected_def.len())
                .any(|w| w == expected_def.as_slice()),
            "encoded planet definitions missing celestial_id={}",
            cel.celestial_id
        );
    }

    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let handlers = router.handlers.read().unwrap();
    for expected in [
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
        "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest",
    ] {
        assert!(handlers.contains_key(expected), "missing handler {expected}");
    }

    // Even when client provides version 1.0.0, no_known = true forces definitions to be encoded.
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let mut ver_100 = DynamicQuasarMessage::new("Spec");
    ver_100.fields.insert(1, DynamicValue::Uint32(1));
    ver_100.fields.insert(2, DynamicValue::Uint32(0));
    ver_100.fields.insert(3, DynamicValue::Uint32(0));
    let mut force_req = DynamicQuasarMessage::new("Req");
    force_req.fields.insert(1, DynamicValue::Message(Box::new(ver_100)));
    force_req.fields.insert(2, DynamicValue::Bool(true));

    let resp = handlers["eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest"](
        &ctx,
        &GoogleAny::default(),
        &force_req,
    )
    .expect("GetAllDefinitionsRequest with no_known=true");
    assert!(resp.value.len() >= encoded.len());

    let ver_resp = handlers["eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest"](
        &ctx,
        &GoogleAny::default(),
        &DynamicQuasarMessage::new("VerReq"),
    )
    .expect("GetDefinitionsVersionRequest");
    assert_eq!(ver_resp.value, encoded);
}

#[test]
fn modify_reagents_validates_planet_rejects_destroyed_and_syncs_maturity_baseline() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_sov_skyhook::register_query_handlers(
        &router,
        Arc::clone(&db),
        Arc::clone(&sde),
    );

    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_214_u32; // HE-V4V
    let lava_planet = sde
        .celestials_in_system_slice(sys_id)
        .iter()
        .find(|c| c.type_id == 2015)
        .expect("HE-V4V lava planet")
        .celestial_id;
    let temp_planet = sde
        .celestials_in_system_slice(sys_id)
        .iter()
        .find(|c| c.type_id == 11)
        .expect("HE-V4V temperate planet")
        .celestial_id;

    let sh_lava = 79_600_001_i64;
    let sh_temp = 79_600_002_i64;
    let sh_destroyed = 79_600_003_i64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        let corp = snap.corporations.entry(corp_id).or_default();
        corp.corporation_id = corp_id;
        corp.ceo_id = cid;
        corp.members.insert(cid);
        corp.member_roles.insert(cid, corp_roles::ROLE_DIRECTOR);
        for (sh_id, st_state, planet) in [
            (sh_lava, "ShieldVulnerable", lava_planet),
            (sh_temp, "ShieldVulnerable", temp_planet),
            (sh_destroyed, "Destroyed", lava_planet),
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
            snap.sov_runtime.skyhook_planets.insert(sh_id, planet);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }
        // Seed stale harvest & maturity state on sh_lava to verify ModifyReagentsRequest resets them.
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(sh_lava, (TYPE_MAGMATIC_GAS, 10, 20, 1_000));
        snap.sov_runtime
            .skyhook_reagents
            .insert(sh_lava, (TYPE_MAGMATIC_GAS, 30));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(sh_lava, 1_000);
        snap.sov_runtime.skyhook_maturity_rem.insert(sh_lava, (999, 50_000));
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let gm_ctx_at = |ts: u64| {
        let mut c = AuthoritativeContext::for_session_at(
            Some(1),
            Some(cid as u32),
            Some(corp_id as u32),
            corp_roles::ROLE_DIRECTOR,
            "tranquility",
            vec![1],
            ts,
        );
        c.service_role_mask = u64::MAX;
        c
    };
    let make_mod_req = |sh_id: i64, reag_tid: u32, sec: u64, unsec: u64, cycle_ts: Option<u64>| {
        let mut entry = id_msg(1, i64::from(reag_tid));
        entry.fields.insert(2, DynamicValue::Uint64(sec));
        entry.fields.insert(3, DynamicValue::Uint64(unsec));
        if let Some(t) = cycle_ts {
            let mut ts_m = DynamicQuasarMessage::new("Timestamp");
            ts_m.fields.insert(1, DynamicValue::Uint64(t));
            entry.fields.insert(5, DynamicValue::Message(Box::new(ts_m)));
        }
        let mut req = id_msg(1, sh_id);
        req.fields
            .insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(entry))]));
        req
    };

    let ts = 20_000_u64;
    let ctx_ts = gm_ctx_at(ts);

    // 1. Explicit reagent list on a non-reagent Temperate planet is rejected without mutating state.
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
            &ctx_ts,
            &dummy_any,
            &make_mod_req(sh_temp, TYPE_MAGMATIC_GAS, 100, 50, Some(ts)),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. Explicit mismatched reagent type (Superionic Ice on Lava planet) is rejected without mutating state.
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
            &ctx_ts,
            &dummy_any,
            &make_mod_req(sh_lava, TYPE_SUPERIONIC_ICE, 100, 50, Some(ts)),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 3. Destroyed skyhook rejects ModifyReagentsRequest & ScheduleTheftVulnerabilityWindowRequest with STATUS_NOT_FOUND.
    let mut ts_start = DynamicQuasarMessage::new("Timestamp");
    ts_start.fields.insert(1, DynamicValue::Uint64(ts + 100));
    let mut ts_end = DynamicQuasarMessage::new("Timestamp");
    ts_end.fields.insert(1, DynamicValue::Uint64(ts + 3_700));
    let mut sched_dead = id_msg(1, sh_destroyed);
    sched_dead
        .fields
        .insert(2, DynamicValue::Message(Box::new(ts_start)));
    sched_dead
        .fields
        .insert(3, DynamicValue::Message(Box::new(ts_end)));
    for (ep, req) in [
        (
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
            id_msg(1, sh_destroyed),
        ),
        (
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
            make_mod_req(sh_destroyed, TYPE_MAGMATIC_GAS, 100, 50, Some(ts)),
        ),
        (
            "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest",
            sched_dead,
        ),
    ] {
        assert_eq!(
            handlers[ep](&ctx_ts, &dummy_any, &req).unwrap_err().0,
            STATUS_NOT_FOUND,
            "{ep} must return STATUS_NOT_FOUND on destroyed skyhook"
        );
    }
    assert!(
        router.pending_notices.read().unwrap().is_empty(),
        "rejected admin calls must emit zero notices"
    );
    db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&sh_temp));
        assert!(!snap.sov_runtime.skyhook_reagent_bays.contains_key(&sh_temp));
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&sh_destroyed));
        assert!(!snap.sov_runtime.skyhook_reagent_bays.contains_key(&sh_destroyed));
        assert!(!snap.sov_runtime.skyhook_last_harvest_unix.contains_key(&sh_destroyed));
        assert!(!snap.sov_runtime.skyhook_maturity_rem.contains_key(&sh_destroyed));
        assert!(!snap.sov_runtime.skyhook_theft_windows.contains_key(&sh_destroyed));
        assert_eq!(snap.sov_runtime.skyhook_last_harvest_unix.get(&sh_lava), Some(&1_000));
        assert_eq!(snap.sov_runtime.skyhook_maturity_rem.get(&sh_lava), Some(&(999, 50_000)));
    });

    // 4. Valid explicit ModifyReagentsRequest resets harvest/maturity baselines and preserves GetRequest continuity.
    handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
        &ctx_ts,
        &dummy_any,
        &make_mod_req(sh_lava, TYPE_MAGMATIC_GAS, 100, 240, Some(ts)),
    )
    .expect("valid explicit ModifyReagentsRequest");
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.skyhook_reagent_bays.get(&sh_lava),
            Some(&(TYPE_MAGMATIC_GAS, 100, 240, ts))
        );
        assert_eq!(
            snap.sov_runtime.skyhook_reagents.get(&sh_lava),
            Some(&(TYPE_MAGMATIC_GAS, 340))
        );
        assert_eq!(snap.sov_runtime.skyhook_last_harvest_unix.get(&sh_lava), Some(&ts));
        assert_eq!(snap.sov_runtime.skyhook_maturity_rem.get(&sh_lava), Some(&(240, 0)));
    });

    // Immediate GetRequest at `ts` must preserve exact set values without retroactive harvest or maturity.
    handlers["eve_public.sovereignty.skyhook.api.GetRequest"](
        &ctx_ts,
        &dummy_any,
        &id_msg(1, sh_lava),
    )
    .expect("immediate skyhook GetRequest");
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.skyhook_reagent_bays.get(&sh_lava),
            Some(&(TYPE_MAGMATIC_GAS, 100, 240, ts))
        );
    });

    // GetRequest 1h later harvests 1 hour of Lava gas and matures 240 * 3600 / 86400 = 10 units.
    let hourly = crate::services::quasar_sov_skyhook::planet_hourly_reagent_yield(
        &sde,
        Some(lava_planet),
        Some(sys_id),
        TYPE_MAGMATIC_GAS,
    );
    assert!(hourly > 0);
    let to_sec = (hourly * REAGENT_SECURED_PERCENTAGE) / 100;
    let to_insec = hourly - to_sec;
    let ctx_1h = gm_ctx_at(ts + 3_600);
    handlers["eve_public.sovereignty.skyhook.api.GetRequest"](
        &ctx_1h,
        &dummy_any,
        &id_msg(1, sh_lava),
    )
    .expect("1h skyhook GetRequest");
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.sov_runtime.skyhook_reagent_bays.get(&sh_lava),
            Some(&(TYPE_MAGMATIC_GAS, 110 + to_sec, 230 + to_insec, ts + 3_600))
        );
        assert_eq!(
            snap.sov_runtime.skyhook_last_harvest_unix.get(&sh_lava),
            Some(&(ts + 3_600))
        );
        assert_eq!(
            snap.sov_runtime.skyhook_maturity_rem.get(&sh_lava),
            Some(&(230 + to_insec, 0))
        );
    });

    // Setting unsecured_stock = 0 zeroes skyhook_maturity_rem.
    handlers["eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"](
        &ctx_1h,
        &dummy_any,
        &make_mod_req(sh_lava, TYPE_MAGMATIC_GAS, 500, 0, Some(ts + 3_600)),
    )
    .expect("zero-unsecured ModifyReagentsRequest");
    db.with_snapshot(|snap| {
        assert_eq!(snap.sov_runtime.skyhook_maturity_rem.get(&sh_lava), Some(&(0, 0)));
    });
}
