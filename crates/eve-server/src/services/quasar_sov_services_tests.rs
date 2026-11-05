//! Unit tests for `quasar_sov_services` (Skyhook activation, Faction Warfare scores & leaderboards, Operation recommendations, Stargate lock notices).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN};
use eve_db::UpwellStructureRecord;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
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
fn skyhook_activation_and_stargate_lock_notices() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    assert!(publish_stargate_lock_activated_notice(&router, 90_000_001, 30_100_000, 50_000_001, 2_000, 1_000).is_ok());
    publish_stargate_lock_deactivated_notice(&router, 90_000_001, 30_100_000, 50_000_001, 2_000);

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let skyhook_id = 78_000_001_i64;
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
                name: "Test Skyhook".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, 40_009_080);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let grunt_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(cid as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
    );

    // Non-director cannot deactivate skyhook
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.DeactivateRequest"](
            &grunt_ctx,
            &dummy_any,
            &id_msg(1, skyhook_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Already active -> ActivateRequest fails with BAD_REQUEST
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.ActivateRequest"](
            &dir_ctx,
            &dummy_any,
            &id_msg(1, skyhook_id),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Deactivate then Activate
    let deact = handlers["eve_public.sovereignty.skyhook.api.DeactivateRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("deactivate skyhook");
    assert!(deact
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.DeactivateResponse"));

    let act = handlers["eve_public.sovereignty.skyhook.api.ActivateRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("activate skyhook");
    assert!(act
        .type_url
        .ends_with("eve_public.sovereignty.skyhook.api.ActivateResponse"));
}

#[test]
fn faction_warfare_activity_and_operation_recommendation_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let fw_sys = db.with_mut_snapshot(|snap| {
        if snap.fw_system_occupations.is_empty() {
            snap.backfill_fw_system_occupations();
        }
        *snap.fw_system_occupations.keys().next().expect("fw system")
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // Faction GetRequest & SearchRequest
    let fac_resp = handlers["eve.faction.api.GetRequest"](&gm_ctx, &dummy_any, &id_msg(1, 500_001))
        .expect("faction GetRequest");
    assert!(fac_resp.type_url.ends_with("eve.faction.api.GetResponse"));
    assert!(!fac_resp.value.is_empty());

    let mut search_msg = DynamicQuasarMessage::new("Search");
    search_msg.fields.insert(1, DynamicValue::String("Caldari".into()));
    let search_resp = handlers["eve.faction.SearchRequest"](&gm_ctx, &dummy_any, &search_msg)
        .expect("faction SearchRequest");
    assert!(search_resp.type_url.ends_with("eve.faction.SearchResponse"));
    assert!(!search_resp.value.is_empty());

    // Faction activity scores & admin score mutation
    let mut score_msg = id_msg(1, fw_sys as i64);
    score_msg.fields.insert(2, DynamicValue::Int64(500_001));
    score_msg.fields.insert(3, DynamicValue::Int64(250));

    for (req, suffix) in [
        (
            "eve.faction.activity.GetSolarSystemScoresRequest",
            "eve.faction.activity.GetSolarSystemScoresResponse",
        ),
        (
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            "eve_public.faction.activity.GetSolarSystemScoresResponse",
        ),
        (
            "eve.faction.activity.api.GetAllSolarSystemScoresRequest",
            "eve.faction.activity.api.GetAllSolarSystemScoresResponse",
        ),
        (
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            "eve_public.faction.activity.admin.IncrementContributionScoreResponse",
        ),
        (
            "eve_public.faction.activity.admin.SetContributionScoreRequest",
            "eve_public.faction.activity.admin.SetContributionScoreResponse",
        ),
        (
            "eve.faction.activity.admin.IncrementContributionScoreRequest",
            "eve.faction.activity.admin.IncrementContributionScoreResponse",
        ),
        (
            "eve.faction.activity.admin.SetContributionScoreRequest",
            "eve.faction.activity.admin.SetContributionScoreResponse",
        ),
    ] {
        let r = handlers[req](&gm_ctx, &dummy_any, &score_msg).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Faction warfare leaderboards, stats, wars, warzones, operation recommendations
    for (req, suffix) in [
        ("eve.factionwarfare.systems.GetRequest", "eve.factionwarfare.systems.GetResponse"),
        (
            "eve.factionwarfare.leaderboard.characters.GetRequest",
            "eve.factionwarfare.leaderboard.characters.GetResponse",
        ),
        (
            "eve.factionwarfare.leaderboard.corporations.GetRequest",
            "eve.factionwarfare.leaderboard.corporations.GetResponse",
        ),
        (
            "eve.factionwarfare.leaderboard.factions.GetRequest",
            "eve.factionwarfare.leaderboard.factions.GetResponse",
        ),
        (
            "eve.factionwarfare.stats.character.GetRequest",
            "eve.factionwarfare.stats.character.GetResponse",
        ),
        ("eve.factionwarfare.wars.GetRequest", "eve.factionwarfare.wars.GetResponse"),
        (
            "eve.factionwarfare.warzone.api.GetAllRequest",
            "eve.factionwarfare.warzone.api.GetAllResponse",
        ),
        (
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            "eve.factionwarfare.warzone.api.GetParticipantsResponse",
        ),
        (
            "eve_public.operation.recommendation.GetRequest",
            "eve_public.operation.recommendation.GetResponse",
        ),
    ] {
        let r = handlers[req](&gm_ctx, &dummy_any, &id_msg(1, 1)).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    assert_eq!(
        handlers["eve.factionwarfare.warzone.api.GetParticipantsRequest"](
            &gm_ctx,
            &dummy_any,
            &id_msg(1, 99),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
}
