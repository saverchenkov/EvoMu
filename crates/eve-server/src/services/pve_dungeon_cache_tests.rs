//! Unit and integration tests for `pve_dungeon_cache`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "pve_dungeon_cache_extra_unit_tests.rs"]
mod extra_unit_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn dungeon_id_parser_fnv_hash_and_ice_belt_eligibility() {
    assert_eq!(parse_dng_site_id("DNG-2045-987654"), Some((2045, 987_654)));
    assert_eq!(parse_dng_site_id("ANOM-1"), None);

    let h1 = registry_instance_id("RELIC-30000142-1");
    let h2 = registry_instance_id("RELIC-30000142-2");
    assert_ne!(h1, h2);
    assert!(h1 > 0);

    let sde = shared_sde();
    let eligible = eligible_ice_belts(&sde, 0.4);
    assert!(!eligible.is_empty());

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    // Find a system that qualifies for an ice belt in SDE
    let ice_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| ice_belt_dungeon_for_system(&ctx, sid).is_some())
        .expect("SDE has at least one ice belt system");
    assert!(ice_belt_dungeon_for_system(&ctx, ice_sys).is_some());
    assert!(ice_belt_dungeon_for_system(&ctx, 31_000_001).is_none());
    seed_system_ice_belt_from_sde(&ctx, ice_sys);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_cache_and_editor_metadata_rpc_handlers() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_201;
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dng_cache", "pw", char_id, "DngCache");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_700_000)));

    let mut session = SessionState::new(502);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.role = service_roles::ROLE_GM_MASK;
    let empty_kw = HashMap::new();

    // Unhandled service returns None
    assert!(handle(&ctx, &mut session, "", "", &[], &empty_kw).await.is_none());

    // Dungeon editor metadata endpoints
    let archs = handle(&ctx, &mut session, "dungeon", "DEGetArchetypes", &[], &empty_kw)
        .await
        .expect("handled")
        .expect("DEGetArchetypes succeeds");
    assert!(!matches!(archs, PyRep::None));

    let dungeons = handle(&ctx, &mut session, "dungeon", "DEGetDungeons", &[], &empty_kw)
        .await
        .expect("handled")
        .expect("DEGetDungeons succeeds");
    assert!(!matches!(dungeons, PyRep::None));

    // Instance cache endpoints across anomaly, ore, ice, bulwark, FW, homefront, and signatures
    for method in [
        "GetCombatAnomalyInstances",
        "GetCombatAnomaliesCount",
        "GetOreAnomalyInstances",
        "GetOreAnomaliesCount",
        "GetOreAnomaliesCountInRange",
        "GetIceBeltInstances",
        "GetIceBeltsCount",
        "GetBulwarkMiningSiteCounts",
        "GetFactionWarfareInstances",
        "GetHomefrontSiteInstances",
        "GetSignatureInstances",
    ] {
        let res = handle(
            &ctx,
            &mut session,
            "dungeonInstanceCacheMgr",
            method,
            &[],
            &empty_kw,
        )
        .await
        .expect("handled")
        .expect(method);
        assert!(!matches!(res, PyRep::None));
    }

    // GM dungeon authoring metadata calls
    let factions = handle(
        &ctx,
        &mut session,
        "dungeon",
        "DEGetFactions",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("DEGetFactions");
    assert!(!matches!(factions, PyRep::None));

    let palette = handle(
        &ctx,
        &mut session,
        "dungeon",
        "DEGetRoomObjectPaletteData",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("DEGetRoomObjectPaletteData");
    assert!(matches!(palette, PyRep::List(_)));

    let any_did = ctx
        .sde
        .dungeons
        .keys()
        .copied()
        .next()
        .expect("SDE has dungeons") as i64;
    let rooms = handle(
        &ctx,
        &mut session,
        "dungeon",
        "DEGetRooms",
        &[PyRep::Integer(any_did)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("DEGetRooms");
    assert!(!matches!(rooms, PyRep::None));

    // Agency provider content list and warp-to-content flow
    let ship_id: i64 = 1_050_200_777;
    session.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let agency_list = handle(
        &ctx,
        &mut session,
        "custom_agency_provider",
        "get_content_data",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("get_content_data");
    if let PyRep::List(items) = agency_list {
        assert!(!items.is_empty());
        let first_piece = items[0].clone();
        let warp_ok = handle(
            &ctx,
            &mut session,
            "custom_agency_provider",
            "warp_to_content",
            &[first_piece],
            &empty_kw,
        )
        .await
        .expect("handled")
        .expect("warp_to_content");
        assert!(matches!(warp_ok, PyRep::None));
    }

    // Non-GM access rejection on dungeon editor endpoints
    let mut non_gm = session.clone();
    non_gm.role = 0;
    let err = handle(
        &ctx,
        &mut non_gm,
        "dungeon",
        "DEGetFactions",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "InsufficientRole");
}
