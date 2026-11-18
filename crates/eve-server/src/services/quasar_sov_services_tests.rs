//! Unit tests for `quasar_sov_services` (Skyhook activation, Faction Warfare scores & leaderboards, Operation recommendations, Stargate lock notices).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN};
use eve_db::UpwellStructureRecord;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "quasar_sov_services_extra_unit_tests.rs"]
mod quasar_sov_services_extra_unit_tests;

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
        for id in [cid, 90_000_002_i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id;
            }
        }
        let mut c = { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = cid;
            __corp };
        c.members.extend([cid, 90_000_002]);
        snap.corporations.insert(corp_id, c);
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
        Some(90_000_002),
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
fn skyhook_deactivate_and_activate_reconcile_sov_hub_network_and_notices() {
    use super::super::quasar_sov_hub::{
        compute_hub_resources, planet_yield_for_planet, star_power_for_system, PendingNotices,
    };
    use super::super::quasar_sov_workforce::{exported_amount, systems_within_jumps};
    use super::super::sov_services::TYPE_SOVEREIGNTY_HUB;
    use eve_db::{PersistedHubUpgrade, PersistedSovHubData, PersistedWorkforceMode};

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    super::super::quasar_sov_hub::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    super::super::quasar_sov_workforce::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let (sys_a, planet_id, planet_wf, sys_b) = [30_000_669_u32, 30_000_142_u32]
        .into_iter()
        .flat_map(|seed| systems_within_jumps(&sde, seed, 8).into_keys())
        .filter(|&sa| star_power_for_system(&sde, sa) >= 250)
        .find_map(|sa| {
            let planet = sde
                .celestials_in_system_slice(sa)
                .iter()
                .filter(|c| c.group_id == 7)
                .find_map(|c| {
                    let (_, wf) = planet_yield_for_planet(c.type_id, c.radius);
                    (wf >= 3_000).then_some((c.celestial_id, wf))
                })?;
            let nb = systems_within_jumps(&sde, sa, 1)
                .into_iter()
                .find(|&(sb, hops)| {
                    hops == 1 && sb != sa && star_power_for_system(&sde, sb) >= 250
                })?
                .0;
            Some((sa, planet.0, planet.1, nb))
        })
        .expect("system pair with >=3000 workforce planet and neighbor system");

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let hub_a = 88_100_001_i64;
    let hub_b = 88_100_002_i64;
    let skyhook_id = 88_200_001_i64;
    let export_amt = 1_500_u64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_a;
        }
        let mut c = eve_db::CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = cid;
        c.ensure_wallet_divisions();
        c.members.insert(cid);
        snap.corporations.insert(corp_id, c);

        for (id, ty, sys) in [
            (hub_a, TYPE_SOVEREIGNTY_HUB, sys_a),
            (hub_b, TYPE_SOVEREIGNTY_HUB, sys_b),
            (skyhook_id, TYPE_ORBITAL_SKYHOOK, sys_a),
        ] {
            snap.structures.insert(
                id,
                UpwellStructureRecord {
                    structure_id: id,
                    type_id: ty,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("SyncStruct {id}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        }
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: export_amt,
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615, // 250 power, 1,500 workforce
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![sys_a],
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615, // 250 power, 1,500 workforce (fed by Hub A export)
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        let res_a = compute_hub_resources(snap, &sde, hub_a).expect("hub A resources");
        assert!(res_a.workforce_local >= planet_wf);
        assert_eq!(res_a.workforce_available, res_a.workforce_local - export_amt);
        let res_b = compute_hub_resources(snap, &sde, hub_b).expect("hub B resources");
        assert_eq!(res_b.workforce_available, export_amt);
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

    // 1. Deactivating the Skyhook drops local hub upgrade into low_power, clamps export to 0,
    //    starves the downstream importing hub into low_power, and emits all three Quasar notices.
    router.pending_notices.write().unwrap().clear();
    handlers["eve_public.sovereignty.skyhook.api.DeactivateRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("DeactivateRequest");

    db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_active[&skyhook_id]);
        assert!(
            snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power,
            "local Sov Hub overloaded upgrade must drop to low_power on Skyhook DeactivateRequest"
        );
        assert_eq!(exported_amount(snap, hub_a), 0);
        assert!(
            snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power,
            "downstream importing Sov Hub upgrade must drop to low_power on Skyhook DeactivateRequest"
        );
        let res_a = compute_hub_resources(snap, &sde, hub_a).expect("hub A post-deactivate");
        assert_eq!(res_a.workforce_local, 0);
    });
    {
        let notices: &PendingNotices = &router.pending_notices;
        let list = notices.read().unwrap();
        assert!(list.iter().any(|(_, a)| a.type_url.ends_with("skyhook.api.ActivationNotice")));
        assert!(
            list.iter().any(|(sc, a)| *sc == crate::quasar_gateway::NoticeScope::SolarSystem(sys_a)
                && a.type_url.ends_with("hub.api.ResourcesSimulatedNotice")),
            "expected ResourcesSimulatedNotice for local system"
        );
        assert!(
            list.iter().any(|(sc, a)| *sc == crate::quasar_gateway::NoticeScope::SolarSystem(sys_b)
                && a.type_url.ends_with("hub.api.ResourcesSimulatedNotice")),
            "expected ResourcesSimulatedNotice for downstream importing system"
        );
        assert!(
            list.iter().any(|(sc, a)| *sc == crate::quasar_gateway::NoticeScope::SolarSystem(sys_a)
                && a.type_url.ends_with("hub.upgrade.api.HubUpgradesSimulatedNotice")),
            "expected HubUpgradesSimulatedNotice for local system"
        );
        assert!(
            list.iter().any(|(sc, a)| *sc == crate::quasar_gateway::NoticeScope::SolarSystem(sys_b)
                && a.type_url.ends_with("hub.upgrade.api.HubUpgradesSimulatedNotice")),
            "expected HubUpgradesSimulatedNotice for downstream importing system"
        );
        assert!(
            list.iter().any(|(_, a)| a.type_url.ends_with("workforce.api.StateChangedNotice")),
            "expected workforce StateChangedNotice"
        );
    }

    // 2. Reactivating the Skyhook restores local workforce and emits ResourcesSimulatedNotice.
    router.pending_notices.write().unwrap().clear();
    handlers["eve_public.sovereignty.skyhook.api.ActivateRequest"](
        &dir_ctx,
        &dummy_any,
        &id_msg(1, skyhook_id),
    )
    .expect("ActivateRequest");
    db.with_snapshot(|snap| {
        assert!(snap.sov_runtime.skyhook_active[&skyhook_id]);
        let res_a = compute_hub_resources(snap, &sde, hub_a).expect("hub A post-activate");
        assert!(res_a.workforce_local >= planet_wf);
    });
    {
        let list = router.pending_notices.read().unwrap();
        assert!(list.iter().any(|(_, a)| a.type_url.ends_with("skyhook.api.ActivationNotice")));
        assert!(list.iter().any(|(sc, a)| {
            *sc == crate::quasar_gateway::NoticeScope::SolarSystem(sys_a)
                && a.type_url.ends_with("hub.api.ResourcesSimulatedNotice")
        }));
    }
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
