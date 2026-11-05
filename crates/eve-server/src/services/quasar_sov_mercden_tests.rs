//! Unit tests for `quasar_sov_mercden` (Mercenary Dens, Evolution/Infomorph Simulation, Mercenary Tactical Operations).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{corp_roles, TrainedSkillRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
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
fn mercenary_den_formulas_simulation_and_activity_completion() {
    assert_eq!(infomorph_cargo_capacity(1), 3_000);
    assert_eq!(stage_for_level(0), 1);
    assert_eq!(stage_for_level(19), 1);
    assert_eq!(stage_for_level(20), 2);
    assert_eq!(stage_for_level(40), 3);
    assert_eq!(stage_for_level(70), 4);
    assert_eq!(stage_for_level(100), 5);

    assert_eq!(infomorphs_per_hour(1), 65);
    assert_eq!(infomorphs_per_hour(2), 68);
    assert_eq!(infomorphs_per_hour(3), 71);
    assert_eq!(infomorphs_per_hour(4), 74);
    assert_eq!(infomorphs_per_hour(5), 78);

    assert_eq!(anarchy_workforce_disruption_pct(1), 0);
    assert_eq!(anarchy_workforce_disruption_pct(2), 0);
    assert_eq!(anarchy_workforce_disruption_pct(3), 10);
    assert_eq!(anarchy_workforce_disruption_pct(4), 20);
    assert_eq!(anarchy_workforce_disruption_pct(5), 40);

    assert_eq!(activity_capacity(1), 3);
    assert_eq!(activity_capacity(5), 7);

    let mut den = PersistedMercenaryDen {
        den_id: 91_000_001,
        owner_char_id: 90_000_001,
        skyhook_id: 79_000_001,
        solar_system_id: 30_000_142,
        planet_id: 40_009_080,
        type_id: 85_230,
        enabled: true,
        cargo_extraction_enabled: true,
        development_level: 18,
        anarchy_level: 10,
        infomorphs: 0,
        simulated_at_unix: 1_000,
        next_activity_generation_unix: 1_000,
    };

    // Simulate 24 hours: +5 development (18 -> 23, stage 1 -> 2), +5 anarchy (10 -> 15), 24*65 = 1560 infomorphs
    assert!(simulate_den(&mut den, 1_000 + EVOLUTION_UNIT_SECS));
    assert_eq!(den.development_level, 23);
    assert_eq!(den.anarchy_level, 15);
    assert_eq!(den.infomorphs, 24 * 65);
    assert!(!encode_evolution_simulation(&den).is_empty());

    let db = WorldRepository::with_demo_seed();
    let pending: PendingNotices = Arc::new( std::sync::RwLock::new(Vec::new()) );
    push_definitions_updated_notice(&pending, &den);

    let act = PersistedMercenaryActivity {
        uuid: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        den_id: den.den_id,
        solar_system_id: den.solar_system_id,
        dungeon_id: 123,
        started: true,
        started_by: 90_000_001,
        expiry_unix: 5_000,
        development_impact: 10,
        anarchy_impact: -10,
        infomorph_bonus: 350,
    };
    push_activity_expiry_changed_notice(&pending, den.solar_system_id, &act, 4_000);

    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(den.den_id, den.clone());
        complete_mercenary_activity_in_snap(snap, &pending, &act, den.den_id, den.solar_system_id);
        let updated = &snap.sov_runtime.mercenary_dens[&den.den_id];
        assert_eq!(updated.development_level, 33);
        assert_eq!(updated.anarchy_level, 5);
        assert_eq!(updated.infomorphs, 24 * 65 + 350);
    });
}

#[test]
fn mercenary_den_and_activity_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let den_id = 91_000_010_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.skills.entry(cid).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: cid,
                skyhook_id: 79_000_001,
                solar_system_id: sys_id,
                planet_id: 40_009_080,
                type_id: 85_230,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 25,
                anarchy_level: 15,
                infomorphs: 200,
                simulated_at_unix: 0,
                next_activity_generation_unix: 0,
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

    // GetAllOwned, GetMaximumForCharacter, GetAsOwner
    for (req, suffix) in [
        (
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse",
        ),
    ] {
        let r = handlers[req](&gm_ctx, &dummy_any, &id_msg(1, den_id)).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Admin ModifyDevelopmentLevel & ModifyAnarchyLevel
    let mut mod_lvl = id_msg(1, den_id);
    mod_lvl.fields.insert(2, DynamicValue::Int64(60));
    handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest"](
        &gm_ctx,
        &dummy_any,
        &mod_lvl,
    )
    .expect("ModifyDevelopmentLevelRequest");
    handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest"](
        &gm_ctx,
        &dummy_any,
        &mod_lvl,
    )
    .expect("ModifyAnarchyLevelRequest");

    let mut bad_lvl = id_msg(1, den_id);
    bad_lvl.fields.insert(2, DynamicValue::Int64(150));
    assert_eq!(
        handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest"](
            &gm_ctx,
            &dummy_any,
            &bad_lvl,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Activity StartRequest
    let act_uuid = db.with_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_activities
            .values()
            .next()
            .expect("generated activity")
            .uuid
            .clone()
    });
    let mut act_id_inner = DynamicQuasarMessage::new("ActId");
    act_id_inner.fields.insert(1, DynamicValue::Bytes(act_uuid));
    let mut start_msg = DynamicQuasarMessage::new("Start");
    start_msg
        .fields
        .insert(1, DynamicValue::Message(Box::new(act_id_inner)));

    let start_resp = handlers["eve_public.sovereignty.mercenaryden.activity.api.StartRequest"](
        &gm_ctx,
        &dummy_any,
        &start_msg,
    )
    .expect("StartRequest");
    assert!(start_resp
        .type_url
        .ends_with("eve_public.sovereignty.mercenaryden.activity.api.StartResponse"));

    // Starting same activity twice returns CONFLICT
    assert_eq!(
        handlers["eve_public.sovereignty.mercenaryden.activity.api.StartRequest"](
            &gm_ctx,
            &dummy_any,
            &start_msg,
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );
}
