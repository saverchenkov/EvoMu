//! Extra branch-coverage and mutation-killing unit tests for `quasar_sov_services`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_sov_services::{
    authorize_skyhook, publish_stargate_lock_activated_notice,
    publish_stargate_lock_deactivated_notice, register_handlers,
};
use crate::services::sov_services::TYPE_ORBITAL_SKYHOOK;
use eve_db::{
    corp_roles, PersistedInsurgencyCampaign, SystemDynamicState, UpwellStructureRecord,
    WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn call_handler(
    router: &PublicGatewayRouter,
    name: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handlers = router.handlers.read().unwrap();
    let h = handlers.get(name).expect(name);
    h(ctx, &GoogleAny::default(), msg)
}

#[test]
fn skyhook_authorization_and_planetless_activation_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let skyhook_id = 78_100_001_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Planetless Skyhook".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        // Deliberately omit skyhook_planets entry and set skyhook_active = false
        snap.sov_runtime.skyhook_planets.remove(&skyhook_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, false);
    });

    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);

    db.with_snapshot(|snap| {
        assert_eq!(
            authorize_skyhook(&unauth_ctx, snap, skyhook_id, "test").unwrap_err().0,
            STATUS_UNAUTHORIZED
        );
        assert_eq!(
            authorize_skyhook(&dir_ctx, snap, 999_999_999, "test").unwrap_err().0,
            STATUS_NOT_FOUND
        );
        assert_eq!(authorize_skyhook(&dir_ctx, snap, skyhook_id, "test").unwrap(), sys_id);
    });

    // Missing structure ID in ActivateRequest / DeactivateRequest -> STATUS_BAD_REQUEST
    let empty_msg = DynamicQuasarMessage::new("Empty");
    assert_eq!(
        call_handler(
            &router,
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            &dir_ctx,
            &empty_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_handler(
            &router,
            "eve_public.sovereignty.skyhook.api.DeactivateRequest",
            &dir_ctx,
            &empty_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Already inactive -> DeactivateRequest returns STATUS_BAD_REQUEST
    let mut id_msg = DynamicQuasarMessage::new("Req");
    id_msg.fields.insert(1, DynamicValue::Int64(skyhook_id));
    assert_eq!(
        call_handler(
            &router,
            "eve_public.sovereignty.skyhook.api.DeactivateRequest",
            &dir_ctx,
            &id_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Activate succeeds when planet_id is None, then Deactivate succeeds when planet_id is None
    assert!(call_handler(
        &router,
        "eve_public.sovereignty.skyhook.api.ActivateRequest",
        &dir_ctx,
        &id_msg
    )
    .is_ok());
    assert!(call_handler(
        &router,
        "eve_public.sovereignty.skyhook.api.DeactivateRequest",
        &dir_ctx,
        &id_msg
    )
    .is_ok());

    // Stargate lock notices
    assert!(publish_stargate_lock_activated_notice(&router, cid, sys_id, 50_000_010, 5_000, 4_000).is_ok());
    publish_stargate_lock_deactivated_notice(&router, cid, sys_id, 50_000_010, 5_000);
}

#[test]
fn faction_races_search_shapes_and_fw_score_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    )
    .with_service_roles(u64::MAX);
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);

    // 1. eve.faction.api.GetRequest: missing ID, unknown ID, and all empire + non-empire race mappings
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.api.GetRequest",
            &admin_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut bad_fac = DynamicQuasarMessage::new("Fac");
    bad_fac.fields.insert(1, DynamicValue::Int64(999_999));
    assert_eq!(
        call_handler(&router, "eve.faction.api.GetRequest", &admin_ctx, &bad_fac)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    for fid in [500_001_i64, 500_002, 500_003, 500_004, 500_005, 500_010] {
        if sde.factions.contains_key(&(fid as u32)) {
            let mut m = DynamicQuasarMessage::new("Fac");
            m.fields.insert(1, DynamicValue::Int64(fid));
            let res = call_handler(&router, "eve.faction.api.GetRequest", &admin_ctx, &m).unwrap();
            assert!(!res.value.is_empty());
        }
    }

    // 2. eve.faction.SearchRequest: empty query rejected, nested single string, nested repeated strings
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.SearchRequest",
            &admin_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut blank_search = DynamicQuasarMessage::new("Search");
    blank_search.fields.insert(1, DynamicValue::String("   ".into()));
    assert_eq!(
        call_handler(&router, "eve.faction.SearchRequest", &admin_ctx, &blank_search)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut inner_single = DynamicQuasarMessage::new("Query");
    inner_single.fields.insert(1, DynamicValue::String("Amarr".into()));
    let mut nested_single = DynamicQuasarMessage::new("Search");
    nested_single
        .fields
        .insert(1, DynamicValue::Message(Box::new(inner_single)));
    let s1 = call_handler(&router, "eve.faction.SearchRequest", &admin_ctx, &nested_single).unwrap();
    assert!(!s1.value.is_empty());

    let mut inner_rep = DynamicQuasarMessage::new("Query");
    inner_rep.fields.insert(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::String("Caldari".into()),
            DynamicValue::String("State".into()),
        ]),
    );
    let mut nested_rep = DynamicQuasarMessage::new("Search");
    nested_rep
        .fields
        .insert(1, DynamicValue::Message(Box::new(inner_rep)));
    let s2 = call_handler(&router, "eve.faction.SearchRequest", &admin_ctx, &nested_rep).unwrap();
    assert!(!s2.value.is_empty());

    // 3. Faction activity score errors and advantage floor encoding
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.activity.GetSolarSystemScoresRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    // Jita (30000142) is not in FW warzone -> STATUS_BAD_REQUEST via character fallback
    assert_eq!(
        call_handler(
            &router,
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            &admin_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let fw_sys = db.with_mut_snapshot(|snap| {
        if snap.fw_system_occupations.is_empty() {
            snap.backfill_fw_system_occupations();
        }
        let sys = *snap.fw_system_occupations.keys().next().unwrap();
        snap.fw_runtime.system_advantage.insert(sys, (0.45, 0.10));
        snap.system_states.insert(
            sys,
            SystemDynamicState {
                solar_system_id: sys,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = sys;
        }
        sys
    });

    // Character solar_system_id fallback when request has no field 1
    let pub_scores = call_handler(
        &router,
        "eve_public.faction.activity.GetSolarSystemScoresRequest",
        &admin_ctx,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!pub_scores.value.is_empty());

    // Admin score mutation: unauthenticated, non-admin, non-FW system, and omitted faction fallback
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.activity.admin.IncrementContributionScoreRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.activity.admin.IncrementContributionScoreRequest",
            &player_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    let mut non_fw_adm = DynamicQuasarMessage::new("Adm");
    non_fw_adm.fields.insert(1, DynamicValue::Int64(30_000_142));
    assert_eq!(
        call_handler(
            &router,
            "eve.faction.activity.admin.SetContributionScoreRequest",
            &admin_ctx,
            &non_fw_adm
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Omitted field 1 (uses character's fw_sys) and omitted field 2 (uses occupier_faction_id)
    let mut inc_fallback = DynamicQuasarMessage::new("Adm");
    inc_fallback.fields.insert(3, DynamicValue::Int64(500));
    assert!(call_handler(
        &router,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        &admin_ctx,
        &inc_fallback
    )
    .is_ok());
    let contested = db.with_snapshot(|s| s.system_states[&fw_sys].fw_contested_pct);
    assert!(contested > 0.0);
}

#[test]
fn warzone_participants_insurgency_override_and_operation_recommendations() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);

    // Clear fw_system_occupations to exercise backfill_fw_system_occupations in warzone.api.GetAllRequest
    db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: 30_002_813,
                systems: vec![30_002_813],
                created_at_unix: 1_000,
                forecasting_end_unix: 2_000,
                active_end_unix: 3_000,
                winddown_end_unix: 4_000,
                corruption_points: std::collections::HashMap::new(),
                suppression_points: std::collections::HashMap::new(),
            },
        );
    });

    let all_wz = call_handler(
        &router,
        "eve.factionwarfare.warzone.api.GetAllRequest",
        &player_ctx,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!all_wz.value.is_empty());

    // GetParticipantsRequest for warzone 1 and warzone 2
    for wz_id in [1_i64, 2] {
        let mut m = DynamicQuasarMessage::new("Wz");
        m.fields.insert(1, DynamicValue::Int64(wz_id));
        let p = call_handler(
            &router,
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            &player_ctx,
            &m,
        )
        .unwrap();
        assert!(!p.value.is_empty());
    }

    // eve.factionwarfare.systems.GetRequest with empty message uses character / occupation fallback
    assert!(call_handler(
        &router,
        "eve.factionwarfare.systems.GetRequest",
        &player_ctx,
        &DynamicQuasarMessage::new("Empty")
    )
    .is_ok());

    // eve_public.operation.recommendation.GetRequest: unauthenticated -> STATUS_UNAUTHORIZED,
    // plus allow_old = 0 vs allow_old = 1 with completed operation
    assert_eq!(
        call_handler(
            &router,
            "eve_public.operation.recommendation.GetRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );

    let mut rec_msg = DynamicQuasarMessage::new("Rec");
    rec_msg.fields.insert(1, DynamicValue::Int64(50)); // clamped to 20
    rec_msg.fields.insert(2, DynamicValue::Int64(1)); // allow_old = true
    assert!(call_handler(
        &router,
        "eve_public.operation.recommendation.GetRequest",
        &player_ctx,
        &rec_msg
    )
    .is_ok());
}
