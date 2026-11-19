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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_id_parser_fnv_hash_and_ice_belt_eligibility() {
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
        if matches!(method, "GetCombatAnomaliesCount" | "GetOreAnomaliesCount" | "GetIceBeltsCount") {
            assert!(matches!(res, PyRep::Dict(_)), "{method} must return Dict (#942)");
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agency_get_content_data_is_read_pure_and_warp_error_paths_do_not_seed_db() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let mut sess = eve_machonet::SessionState::new(910);
    sess.user_id = Some(91);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();
    let (err_not_in_space, err_site_not_found, err_ship_not_in_space) =
        ("NotInSpace", "SiteNotFound", "ShipNotInSpace");

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    let content = handle(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[], &kw)
        .await
        .expect("handled")
        .expect("get_content_data");
    let PyRep::List(items) = content else {
        panic!("expected content list");
    };
    assert!(!items.is_empty());
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);

    // Docked, no-ship, missing-site, and unspawned-ship error paths on warp_to_content must not seed DB.
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[items[0].clone()], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        err_not_in_space
    );
    sess.station_id = None;
    sess.ship_id = Some(777_000_001);
    assert_eq!(
        handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[PyRep::Integer(999_888_777)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        err_site_not_found
    );
    assert_eq!(
        handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[items[0].clone()], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        err_ship_not_in_space
    );
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unseeded_wh_alignment_and_signature_warp_purity() {
    use crate::services::mining_sites::{resolve_gas_site, resolve_ore_site, SiteKind, COSMIC_ORE_SITES, GAS_SITES};
    use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;

    let sde = shared_sde();
    let kw = HashMap::new();

    // Locate Drifter WH (14..=18), C13 (13), shattered C1..=6, and Low-Sec The Forge dynamically in SDE.
    let drifter_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| (14..=18).contains(&system_wormhole_profile_sde(&sde, sid).0))
        .unwrap_or(31_000_001);
    let c13_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| system_wormhole_profile_sde(&sde, sid).0 == 13)
        .expect("SDE contains C13 wormhole");
    let shattered_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (1..=6).contains(&c) && shat
        })
        .expect("SDE contains shattered C1-C6 wormhole");
    let lowsec_forge_sys = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_002 && s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .expect("SDE contains Low-Sec system in The Forge");

    // 1. Compare unseeded get_content_data vs freshly seeded systems for Drifter WH, C13, and Shattered WH.
    for wh_sys in [drifter_sys, c13_sys, shattered_sys] {
        let unseeded_ctx = Arc::new(ServerContext::with_sde_and_repository(
            Arc::clone(&sde),
            Arc::new(WorldRepository::new_empty()),
        ));
        let mut sess = SessionState::new(921);
        sess.user_id = Some(92);
        sess.character_id = Some(90_000_001);
        sess.solar_system_id = Some(wh_sys);

        let e0 = unseeded_ctx.db.state_epoch();
        let c0 = unseeded_ctx.db.tx_commit_count();
        let rep = handle(&unseeded_ctx, &mut sess, "custom_agency_provider", "get_content_data", &[], &kw)
            .await
            .expect("handled")
            .expect("get_content_data");
        let PyRep::List(items) = rep else {
            panic!("expected list from get_content_data");
        };
        assert_eq!(unseeded_ctx.db.state_epoch(), e0);
        assert_eq!(unseeded_ctx.db.tx_commit_count(), c0);
        assert!(unseeded_ctx.systems.get_handle(wh_sys).is_none());

        let mut unseeded_pairs: Vec<(i64, i64)> = items
            .iter()
            .filter_map(|it| {
                Some((
                    it.dict_get("dungeonID").and_then(|v| v.as_i64())?,
                    it.dict_get("instanceID").and_then(|v| v.as_i64())?,
                ))
            })
            .collect();
        unseeded_pairs.sort_unstable();

        let seeded_ctx = Arc::new(ServerContext::with_sde_and_repository(
            Arc::clone(&sde),
            Arc::new(WorldRepository::new_empty()),
        ));
        ensure_system_pve_sites_seeded(&seeded_ctx, wh_sys).await;
        let mut expected_pairs: Vec<(i64, i64)> = read_pve_exploration_reg(&seeded_ctx, |reg| {
            reg.sites_by_system
                .get(&wh_sys)
                .into_iter()
                .flatten()
                .filter(|s| !s.probe_required && !s.cleared)
                .map(|s| (i64::from(s.dungeon_id), registry_instance_id(&s.site_id)))
                .collect()
        });
        expected_pairs.sort_unstable();
        assert_eq!(
            unseeded_pairs, expected_pairs,
            "unseeded agency anomalies must match seeded anomalies for WH system {wh_sys}"
        );
        if wh_sys == drifter_sys {
            assert!(unseeded_pairs.is_empty(), "Drifter WH must not emit combat anomalies");
        }
    }

    // 2. Unseeded probe-required signatures (PvE DED/DSG/DAT/REL/GHO/SLC, Ore Signature, Gas) return SignatureNotScanned read-only.
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&sde), db));
    let mut sess = SessionState::new(922);
    sess.user_id = Some(93);
    sess.character_id = Some(90_000_001);
    sess.ship_id = Some(777_000_002);
    sess.solar_system_id = Some(lowsec_forge_sys);

    let err_sig_not_scanned = "SignatureNotScanned";
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    for (sig_did, sig_id) in unseeded_pve_signature_specs(&ctx, lowsec_forge_sys) {
        let sig_iid = registry_instance_id(&sig_id);
        for arg in [
            PyRep::Integer(sig_iid),
            PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(sig_did)))]),
        ] {
            let err = handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[arg], &kw)
                .await
                .expect("handled")
                .expect_err("unscanned PvE signature must reject");
            assert_eq!(err.code, err_sig_not_scanned, "{sig_id}");
        }
    }

    let ls_sys = sde.solar_systems.get(&lowsec_forge_sys).unwrap();
    let ore_sig_did = COSMIC_ORE_SITES
        .iter()
        .filter(|d| d.kind == SiteKind::Signature && d.band.matches_system(ls_sys))
        .filter_map(|d| resolve_ore_site(&sde, d).ok())
        .map(|s| s.dungeon_id)
        .next()
        .expect("Low-Sec ore signature");
    let gas_did = GAS_SITES
        .iter()
        .find(|d| d.band.matches_system(ls_sys) && d.region_names.contains(&"The Forge"))
        .and_then(|d| resolve_gas_site(&sde, d).ok())
        .map(|s| s.dungeon_id)
        .expect("The Forge Low-Sec gas signature");
    for did in [ore_sig_did, gas_did] {
        let arg = PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(did)))]);
        let err = handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[arg], &kw)
            .await
            .expect("handled")
            .expect_err("unscanned ore/gas signature must reject");
        assert_eq!(err.code, err_sig_not_scanned, "dungeon {did}");
    }

    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);
    assert!(read_pve_exploration_reg(&ctx, |r| r.seeded_systems.is_empty()));
    assert!(ctx.systems.get_handle(lowsec_forge_sys).is_none());
}
