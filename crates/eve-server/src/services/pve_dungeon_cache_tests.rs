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

#[tokio::test]
async fn unseeded_pochven_and_wormhole_mining_warp_validation() {
    let sde = shared_sde();
    let kw = HashMap::new();
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 777_000_003;

    let pochven_sys = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE contains Pochven solar system");
    let wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, shat, _) = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&sde, sid);
            (1..=4).contains(&c) && !shat
        })
        .expect("SDE contains non-shattered C1-C4 wormhole");
    let c56_wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, shat, _) = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&sde, sid);
            (5..=6).contains(&c) && !shat
        })
        .expect("SDE contains non-shattered C5-C6 wormhole");
    let shat_wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, _, _) = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&sde, sid);
            c == 13
        })
        .expect("SDE contains C13 shattered wormhole");

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dng_poch_wh", "pw", char_id, "DngPochWh");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = pochven_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: pochven_sys as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::clone(&sde), db));
    let mut sess = SessionState::new(923);
    sess.user_id = Some(94);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);

    let err_ship_not_in_space = "ShipNotInSpace";
    let err_sig_not_scanned = "SignatureNotScanned";
    let err_site_not_found = "SiteNotFound";
    let err_unknown_solar_system = "UnknownSolarSystem";
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Unseeded Pochven ore anomalies (deterministic [primary_did, 8974]) match mining need and stop at ShipNotInSpace before seeding.
    sess.solar_system_id = Some(pochven_sys);
    const POCHVEN_SPECIAL_ORE_DUNGEONS: [u32; 5] = [8987, 8988, 8989, 8990, 8991];
    let primary_poch_did =
        POCHVEN_SPECIAL_ORE_DUNGEONS[(pochven_sys as usize) % POCHVEN_SPECIAL_ORE_DUNGEONS.len()];
    for did in [primary_poch_did, 8974] {
        let arg = PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(did)))]);
        let err = handle(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[arg], &kw)
            .await
            .expect("handled")
            .expect_err("unspawned ship must hit ShipNotInSpace rather than SiteNotFound");
        assert_eq!(err.code, err_ship_not_in_space, "Pochven ore dungeon {did}");
    }

    // 2. Unseeded WH ore anomalies match mining need cleanly (ShipNotInSpace) without consuming RNG, and WH gas returns SignatureNotScanned read-only.
    sess.solar_system_id = Some(wh_sys);
    let (wh_c, _, _, _) = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&sde, wh_sys);
    let wh_ore_did = crate::services::mining_sites::wormhole_ore_sites_for_class(wh_c)
        .into_iter()
        .filter_map(|d| crate::services::mining_sites::resolve_wormhole_ore_site_with_dungeon(&sde, d).ok())
        .map(|(did, _)| did)
        .next()
        .expect("C1-C6 WH ore dungeon");
    let err_wh_ore = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(wh_ore_did)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unspawned ship must hit ShipNotInSpace on WH ore dungeon");
    assert_eq!(err_wh_ore.code, err_ship_not_in_space);

    let wh_gas_did = crate::services::mining_sites::WORMHOLE_GAS_SITES
        .first()
        .and_then(|d| crate::services::mining_sites::resolve_wormhole_gas_site(&sde, d).ok())
        .map(|(did, _)| did)
        .expect("WH gas dungeon");
    let err_wh_gas = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(wh_gas_did)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unscanned WH gas signature must reject with SignatureNotScanned");
    assert_eq!(err_wh_gas.code, err_sig_not_scanned);

    // Non-deterministic C5/C6 WH ore (4-of-10 random draw) must reject with SiteNotFound read-only (F-03).
    sess.solar_system_id = Some(c56_wh_sys);
    let (c56_c, _, _, _) = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&sde, c56_wh_sys);
    let c56_ore_did = crate::services::mining_sites::wormhole_ore_sites_for_class(c56_c)
        .into_iter()
        .filter_map(|d| crate::services::mining_sites::resolve_wormhole_ore_site_with_dungeon(&sde, d).ok())
        .map(|(did, _)| did)
        .next()
        .expect("C5-C6 WH ore dungeon");
    let err_c56_ore = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(c56_ore_did)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded C5/C6 WH ore must reject with SiteNotFound without mutating DB");
    assert_eq!(err_c56_ore.code, err_site_not_found);

    // Shattered WH guarantees Shattered Debris Field deterministically while rejecting non-guaranteed draws.
    sess.solar_system_id = Some(shat_wh_sys);
    let shat_defs = crate::services::mining_sites::wormhole_ore_sites_for_class(13);
    let debris_did = shat_defs
        .iter()
        .find(|d| d.dungeon_name == "Shattered Debris Field")
        .and_then(|d| crate::services::mining_sites::resolve_wormhole_ore_site_with_dungeon(&sde, d).ok())
        .map(|(did, _)| did)
        .expect("Shattered Debris Field dungeon");
    let non_debris_did = shat_defs
        .iter()
        .find(|d| d.dungeon_name != "Shattered Debris Field")
        .and_then(|d| crate::services::mining_sites::resolve_wormhole_ore_site_with_dungeon(&sde, d).ok())
        .map(|(did, _)| did)
        .expect("non-debris shattered WH ore dungeon");
    let err_debris = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(debris_did)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("deterministic Shattered Debris Field must reach ShipNotInSpace");
    assert_eq!(err_debris.code, err_ship_not_in_space);
    let err_non_debris = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(non_debris_did)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-guaranteed shattered WH ore must reject with SiteNotFound read-only");
    assert_eq!(err_non_debris.code, err_site_not_found);

    // Unknown solar system on get_content_data (P2 #3) and negative dungeonID wrap rejection (P2 #4).
    sess.solar_system_id = Some(99_999_999);
    let err_unk_sys = handle(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[], &kw)
        .await
        .expect("handled")
        .expect_err("unknown SDE solar system must reject with UnknownSolarSystem");
    assert_eq!(err_unk_sys.code, err_unknown_solar_system);
    sess.solar_system_id = Some(pochven_sys);
    let err_neg_did = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(-1))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("negative dungeonID must reject with SiteNotFound");
    assert_eq!(err_neg_did.code, err_site_not_found);

    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 3. Once ship is spawned in Pochven Ballpark, warp_to_content to 8974 lazily seeds mining and warps.
    sess.solar_system_id = Some(pochven_sys);
    let _ = ctx
        .systems
        .get_or_spawn(pochven_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    let warp_rep = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(8974))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("Pochven mining warp_to_content must succeed");
    assert_eq!(warp_rep, PyRep::None);
    assert!(ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&pochven_sys)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_id.starts_with("DNG-8974-")))
    }));

    // 4. F-04: Unseeded designated IceBelt + respawn-ready cleared IceBelt + cleared SovOre deposit
    //    must resolve and seed/respawn cleanly even when OreAnomaly already exists (`mining_seeded = true`).
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));

    let ice_sys: u32 = 30_000_180;
    let ice_site = crate::services::mining_sites::resolve_regional_ice_belt(&sde, ice_sys)
        .expect("designated ice system")
        .expect("resolved regional ice belt");
    sess.solar_system_id = Some(ice_sys);
    let err_ice_unspawned = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(ice_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded designated IceBelt must hit ShipNotInSpace read-only before ship spawn");
    assert_eq!(err_ice_unspawned.code, err_ship_not_in_space);
    assert!(ctx.db.with_snapshot(|snap| !snap.cosmic_anomalies.contains_key(&ice_sys)));

    let _ = ctx
        .systems
        .get_or_spawn(ice_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    // Pre-seed OreAnomaly so `mining_seeded == true`, yet no IceBelt exists yet.
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(ice_sys).or_default().push(eve_db::CosmicAnomalyRecord {
            anomaly_id: "DNG-1-101".into(),
            solar_system_id: ice_sys,
            name: "Preexisting Ore".into(),
            anomaly_type: "OreAnomaly".into(),
            position: [1_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![1230],
            remaining_volume: 1_000.0,
            created_unix: ctx.now_unix_secs(),
            lifetime_seconds: 0,
            cleared: false,
        });
    });
    let ice_warp = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(ice_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("designated IceBelt warp_to_content must seed and succeed even with OreAnomaly present");
    assert_eq!(ice_warp, PyRep::None);
    let ice_prefix = format!("DNG-{}-", ice_site.dungeon_id);
    assert!(ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.get(&ice_sys).is_some_and(|list| {
            list.iter()
                .any(|a| a.anomaly_type == "IceBelt" && !a.cleared && a.anomaly_id.starts_with(&ice_prefix))
        })
    }));

    // Cleared IceBelt on cooldown must fail with SiteNotFound read-only; once respawn-ready, warp respawns it.
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.retain(|_, a| !(a.solar_system_id == ice_sys && a.belt_id == 0));
        if let Some(list) = snap.cosmic_anomalies.get_mut(&ice_sys) {
            for a in list.iter_mut().filter(|a| a.anomaly_type == "IceBelt") {
                a.cleared = true;
                a.created_unix = ctx.now_unix_secs();
            }
        }
    });
    let ice_cooldown_e = ctx.db.state_epoch();
    let ice_cooldown_c = ctx.db.tx_commit_count();
    let err_ice_cd = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(ice_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cleared IceBelt on cooldown must return SiteNotFound without DB mutation");
    assert_eq!(err_ice_cd.code, err_site_not_found);
    assert_eq!(ctx.db.state_epoch(), ice_cooldown_e);
    assert_eq!(ctx.db.tx_commit_count(), ice_cooldown_c);

    ctx.advance_secs(crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS + 5);
    let ice_respawn_warp = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(ice_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("respawn-ready designated IceBelt must respawn and warp");
    assert_eq!(ice_respawn_warp, PyRep::None);
    assert!(ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&ice_sys)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "IceBelt" && !a.cleared))
    }));

    // Cleared SovOre + installed SOV_PROSPECTING_DEPOSITS upgrade respects cooldown and respawns when elapsed.
    let sov_sys: u32 = 30_004_759;
    let dep = &crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS[0];
    let sov_site = crate::services::mining_sites::resolve_ore_site(&sde, &dep.site)
        .expect("resolved SOV prospecting deposit");
    let hub_id = 1_055_777_001_i64;
    let sov_now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32_458,
                owner_corp_id: 98_000_001,
                solar_system_id: sov_sys,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_055_777_002,
            eve_db::InventoryItemRecord {
                item_id: 1_055_777_002,
                type_id: dep.upgrade_type_id,
                owner_id: 98_000_001,
                location_id: hub_id,
                flag: eve_db::InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.cosmic_anomalies.entry(sov_sys).or_default().extend([
            eve_db::CosmicAnomalyRecord {
                anomaly_id: "DNG-1-202".into(),
                solar_system_id: sov_sys,
                name: "Preexisting Null Ore".into(),
                anomaly_type: "OreAnomaly".into(),
                position: [1_000_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 1_000.0,
                created_unix: sov_now,
                lifetime_seconds: 0,
                cleared: false,
            },
            eve_db::CosmicAnomalyRecord {
                anomaly_id: "DNG-OLD-SOV-2".into(),
                solar_system_id: sov_sys,
                name: "Cleared Sov Ore".into(),
                anomaly_type: "SovOre".into(),
                position: [2_000_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 0.0,
                created_unix: sov_now,
                lifetime_seconds: dep.respawn_seconds,
                cleared: true,
            },
        ]);
    });
    sess.solar_system_id = Some(sov_sys);
    let _ = ctx
        .systems
        .get_or_spawn(sov_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    let sov_cd_e = ctx.db.state_epoch();
    let sov_cd_c = ctx.db.tx_commit_count();
    let err_sov_cd = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(sov_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cleared SovOre on cooldown must return SiteNotFound without DB mutation");
    assert_eq!(err_sov_cd.code, err_site_not_found);
    assert_eq!(ctx.db.state_epoch(), sov_cd_e);
    assert_eq!(ctx.db.tx_commit_count(), sov_cd_c);

    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        sov_now + dep.respawn_seconds + 10,
    )));
    let sov_respawn_warp = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(sov_site.dungeon_id)))])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("elapsed cleared SovOre must respawn and warp");
    assert_eq!(sov_respawn_warp, PyRep::None);
    let sov_prefix = format!("DNG-{}-", sov_site.dungeon_id);
    assert!(ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.get(&sov_sys).is_some_and(|list| {
            list.iter().any(|a| {
                a.anomaly_type == "SovOre"
                    && !a.cleared
                    && a.anomaly_id != "DNG-OLD-SOV-2"
                    && a.anomaly_id.starts_with(&sov_prefix)
            })
        })
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agency_get_content_data_and_warp_prechecks_are_fail_closed_and_read_pure() {
    let sde = shared_sde();
    let ice_sys: SolarSystemId = 30_000_180;

    let db = Arc::new(WorldRepository::new_empty());
    let char_id = 90_099_180_i64;
    db.provision_gm_account("agency_precheck", "pw", char_id, "AgencyPrecheck");
    let (uid, ship_id) = db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char");
        ch.solar_system_id = ice_sys;
        ch.station_id = None;
        ch.structure_id = None;
        let uid = ch.user_id;
        let ship_id = ch.active_ship_id;
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(ice_sys);
        }
        (uid, ship_id)
    });

    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde.clone(), db));
    let ore_did = *ore_anomaly_instances_readonly(&ctx, ice_sys)
        .keys()
        .next()
        .expect("ore anomaly dungeon for 30000180");
    let sys_h = ctx.systems.get_or_spawn(ice_sys);
    sys_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await
        .expect("spawn ball");

    let kw = HashMap::new();
    let mut unauth = SessionState::new(9910);
    assert_eq!(
        handle(
            &ctx,
            &mut unauth,
            "custom_agency_provider",
            "get_content_data",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .unwrap_err()
        .code,
        "Unauthorized"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut unauth,
            "custom_agency_provider",
            "warp_to_content",
            &[PyRep::keyval([(
                "dungeonID",
                PyRep::Integer(i64::from(ore_did)),
            )])],
            &kw,
        )
        .await
        .expect("handled")
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess = SessionState::new(9911);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(ice_sys);
    sess.ship_id = Some(ship_id);

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    let unseeded_content = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "get_content_data",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("unseeded get_content_data");
    let PyRep::List(unseeded_items) = &unseeded_content else {
        panic!("expected content list");
    };
    assert!(!unseeded_items.is_empty());
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.get(&ice_sys).copied().unwrap_or(false)
    }));

    let target = unseeded_items[0].clone();
    let mining_target = PyRep::keyval([("dungeonID", PyRep::Integer(i64::from(ore_did)))]);

    async fn assert_agency_warp_err_pure(
        ctx: &ServerContext,
        sess: &mut SessionState,
        kw: &HashMap<String, PyRep>,
        arg: PyRep,
        expected_code: &str,
    ) {
        let (ep, cm) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let err = handle(
            ctx,
            sess,
            "custom_agency_provider",
            "warp_to_content",
            &[arg],
            kw,
        )
        .await
        .expect("handled")
        .unwrap_err();
        assert_eq!(err.code, expected_code);
        assert_eq!(ctx.db.state_epoch(), ep);
        assert_eq!(ctx.db.tx_commit_count(), cm);
    }

    // 1. Warp scrambler fails closed with zero DB seeding/mutation on both PvE and mining unseeded targets.
    sys_h.set_warp_scramble_strength(ship_id, 2).await.unwrap();
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, target.clone(), "WarpScrambled").await;
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, mining_target.clone(), "WarpScrambled")
        .await;
    sys_h.set_warp_scramble_strength(ship_id, 0).await.unwrap();

    // 2. Warp disruption bubble fails closed with zero DB seeding/mutation.
    sys_h
        .add_warp_bubble(990_001, glam::DVec3::ZERO, 20_000.0)
        .await
        .unwrap();
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, target.clone(), "WarpScrambled").await;
    sys_h.remove_warp_bubble(990_001).await.unwrap();

    // 3. Non-covert cloak fails closed with zero DB seeding/mutation.
    sys_h
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
            cs.gate_cloak_remaining_ticks = 0;
        })
        .await
        .unwrap();
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, target.clone(), "CloakedCannotWarp").await;
    sys_h
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = false;
        })
        .await
        .unwrap();

    // 4. Active warp fails closed with zero DB seeding/mutation.
    sys_h
        .set_movement_mode(
            ship_id,
            eve_destiny::MovementMode::Warp {
                origin: glam::DVec3::ZERO,
                destination: glam::DVec3::new(1_000_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 5.0,
                total_distance: 1_000_000_000.0,
                phase: eve_destiny::WarpPhase::Accelerating,
                elapsed_warp_time: 1.0,
            },
        )
        .await
        .unwrap();
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, target.clone(), "ShipAlreadyInWarp").await;
    let mut taken_ship = sys_h.take_ball(ship_id).await.expect("taken ship ball");
    taken_ship.ball.mode = eve_destiny::MovementMode::Stop;
    sys_h.restore_ball(taken_ship).await.unwrap();

    // 5. Insufficient capacitor for minimum 150 km warp fails closed before DB seeding.
    sys_h.set_capacitor(ship_id, 0.0).await.unwrap();
    assert_agency_warp_err_pure(&ctx, &mut sess, &kw, target.clone(), "NotEnoughCapacitor").await;
    sys_h.set_capacitor(ship_id, 300.0).await.unwrap();

    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.get(&ice_sys).copied().unwrap_or(false)
    }));
    assert!(ctx
        .db
        .with_snapshot(|s| !s.cosmic_anomalies.contains_key(&ice_sys)));

    // 6. Active immobilizing module (Bastion 33400) fails closed before DB seeding.
    let bastion_mod_id = 99_180_334_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bastion_mod_id,
            eve_db::InventoryItemRecord {
                item_id: bastion_mod_id,
                type_id: 33_400,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(bastion_mod_id);
    });
    assert_agency_warp_err_pure(
        &ctx,
        &mut sess,
        &kw,
        target.clone(),
        eve_proto::UserErrorKey::WarpDisrupted.as_str(),
    )
    .await;
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.get(&ice_sys).copied().unwrap_or(false)
    }));
    assert!(ctx
        .db
        .with_snapshot(|s| !s.cosmic_anomalies.contains_key(&ice_sys)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&bastion_mod_id);
        snap.items.remove(&bastion_mod_id);
    });

    // 7. Clearing all guards allows warp_to_content to seed and initiate warp, while get_content_data preserves output.
    let warp_ok = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[target],
        &kw,
    )
    .await
    .expect("handled")
    .expect("warp_to_content succeeds once warpable");
    assert_eq!(warp_ok, PyRep::None);
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.get(&ice_sys).copied().unwrap_or(false)
    }));

    let epoch_after_seed = ctx.db.state_epoch();
    let commits_after_seed = ctx.db.tx_commit_count();
    let seeded_content = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "get_content_data",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("seeded get_content_data");
    assert_eq!(seeded_content, unseeded_content);
    assert_eq!(ctx.db.state_epoch(), epoch_after_seed);
    assert_eq!(ctx.db.tx_commit_count(), commits_after_seed);

    sys_h
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await
        .unwrap();
    sys_h.set_capacitor(ship_id, 300.0).await.unwrap();
    let mining_ok = handle(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[mining_target],
        &kw,
    )
    .await
    .expect("handled")
    .expect("mining warp_to_content succeeds once warpable");
    assert_eq!(mining_ok, PyRep::None);
    assert!(ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&ice_sys)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "OreAnomaly"))
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_instance_cache_mgr_docked_and_range_reads_are_read_pure_and_fail_closed() {
    let sde = shared_sde();
    let ice_sys: SolarSystemId = 30_000_180;
    let fw_sys: SolarSystemId = 30_002_055;
    let db = Arc::new(WorldRepository::new_empty());
    db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            fw_sys,
            eve_db::snapshot_ext::FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: 500_002,
                occupier_faction_id: 500_002,
                victory_points: 0,
                victory_points_threshold: 75_000,
            },
        );
    });
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let kw = HashMap::new();

    // Unauthenticated callers fail closed with zero DB writes on every getter including FW battlefield.
    let mut unauth = SessionState::new(9920);
    unauth.solar_system_id = Some(fw_sys);
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    for meth in [
        "GetFactionWarfareBattlefieldInstances",
        "GetCombatAnomalyInstances",
        "GetOreAnomalyInstances",
        "GetIceBeltInstances",
        "GetFactionWarfareInstances",
        "GetSignatureInstances",
    ] {
        let err = handle(&ctx, &mut unauth, "dungeonInstanceCacheMgr", meth, &[], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, "Unauthorized", "{meth}");
    }
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.is_empty()));

    // Docked reads stay strictly read-pure while returning SDE-derived counts and instances.
    let mut docked = SessionState::new(9921);
    docked.user_id = Some(91);
    docked.character_id = Some(90_099_181);
    docked.solar_system_id = Some(ice_sys);
    docked.station_id = Some(60_003_760);

    let docked_combat = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetCombatAnomalyInstances");
    let docked_combat_cnt = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomaliesCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetCombatAnomaliesCount");
    let docked_ore = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetOreAnomalyInstances");
    let docked_ore_cnt = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomaliesCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetOreAnomaliesCount");
    let docked_ice = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetIceBeltInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetIceBeltInstances");
    let docked_ice_cnt = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetIceBeltsCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetIceBeltsCount");
    let PyRep::Dict(ref ice_cnt_entries) = docked_ice_cnt else {
        panic!("expected dict for GetIceBeltsCount");
    };
    assert_eq!(
        ice_cnt_entries.first().and_then(|(_, v)| v.as_i64()),
        Some(1)
    );
    let _ = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetBulwarkMiningSiteCounts",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetBulwarkMiningSiteCounts");
    let _ = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetSignatureInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetSignatureInstances");

    // Docked FW / Homefront queries in occupied FW system must not materialize complexes.
    docked.solar_system_id = Some(fw_sys);
    let _ = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetFactionWarfareInstances");
    assert!(handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[PyRep::Integer(i64::from(fw_sys))],
        &kw,
    )
    .await
    .is_none());
    let _ = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(fw_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked GetHomefrontSiteInstances");
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.is_empty()));

    // Undocked GetOreAnomaliesCountInRange stays strictly read-pure across BFS neighbors.
    let mut undocked = docked.clone();
    undocked.station_id = None;
    undocked.solar_system_id = Some(ice_sys);
    let range_res = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomaliesCountInRange",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetOreAnomaliesCountInRange");
    assert!(matches!(range_res, PyRep::Dict(ref d) if !d.is_empty()));
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.is_empty()));

    // Undocked counts/instances match the read-only docked view.
    let undocked_combat = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetCombatAnomalyInstances");
    let undocked_combat_cnt = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomaliesCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetCombatAnomaliesCount");
    let undocked_ore = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetOreAnomalyInstances");
    let undocked_ore_cnt = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomaliesCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetOreAnomaliesCount");
    let undocked_ice = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetIceBeltInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetIceBeltInstances");
    let undocked_ice_cnt = handle(
        &ctx,
        &mut undocked,
        "dungeonInstanceCacheMgr",
        "GetIceBeltsCount",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("undocked GetIceBeltsCount");
    assert_eq!(docked_combat, undocked_combat);
    assert_eq!(docked_combat_cnt, undocked_combat_cnt);
    assert_eq!(docked_ore_cnt, undocked_ore_cnt);
    assert_eq!(docked_ice_cnt, undocked_ice_cnt);
    let shape = |rep: &PyRep| -> std::collections::BTreeMap<i64, std::collections::BTreeMap<i64, usize>> {
        let PyRep::Dict(systems) = rep else {
            panic!("expected outer dict, got {rep:?}");
        };
        systems
            .iter()
            .map(|(sk, sv)| {
                let PyRep::Dict(dungeons) = sv else {
                    panic!("expected inner dict, got {sv:?}");
                };
                let inner = dungeons
                    .iter()
                    .map(|(dk, dv)| {
                        let PyRep::List(ids) = dv else {
                            panic!("expected id list, got {dv:?}");
                        };
                        (dk.as_i64().expect("dungeon id"), ids.len())
                    })
                    .collect();
                (sk.as_i64().expect("system id"), inner)
            })
            .collect()
    };
    assert_eq!(shape(&docked_ore), shape(&undocked_ore));
    assert_eq!(shape(&docked_ice), shape(&undocked_ice));

    // Once seeded by in-space calls, subsequent docked reads return identical DB instance IDs purely.
    let epoch_seeded = ctx.db.state_epoch();
    let commits_seeded = ctx.db.tx_commit_count();
    docked.solar_system_id = Some(ice_sys);
    let docked_ore_seeded = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("seeded docked GetOreAnomalyInstances");
    let docked_ice_seeded = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetIceBeltInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("seeded docked GetIceBeltInstances");
    assert_eq!(docked_ore_seeded, undocked_ore);
    assert_eq!(docked_ice_seeded, undocked_ice);
    assert_eq!(ctx.db.state_epoch(), epoch_seeded);
    assert_eq!(ctx.db.tx_commit_count(), commits_seeded);
}

fn unpack_system_dungeon_instances(
    rep: &PyRep,
    expected_sys: SolarSystemId,
) -> std::collections::BTreeMap<u32, Vec<i64>> {
    let PyRep::Dict(systems) = rep else {
        panic!("expected outer dict, got {rep:?}");
    };
    assert_eq!(systems.len(), 1, "expected single system entry in {rep:?}");
    let (sk, sv) = &systems[0];
    assert_eq!(sk.as_i64(), Some(i64::from(expected_sys)));
    let PyRep::Dict(dungeons) = sv else {
        panic!("expected inner dungeon dict, got {sv:?}");
    };
    dungeons
        .iter()
        .map(|(dk, dv)| {
            let did = dk
                .as_i64()
                .and_then(|v| u32::try_from(v).ok())
                .expect("u32 dungeon id");
            let PyRep::List(ids) = dv else {
                panic!("expected instance id list, got {dv:?}");
            };
            let iids = ids
                .iter()
                .map(|v| v.as_i64().expect("i64 instance id"))
                .collect();
            (did, iids)
        })
        .collect()
}

fn unpack_system_counts_dict(rep: &PyRep) -> std::collections::BTreeMap<SolarSystemId, i64> {
    let PyRep::Dict(entries) = rep else {
        panic!("expected count dict, got {rep:?}");
    };
    entries
        .iter()
        .map(|(sk, cv)| {
            let sid = sk
                .as_i64()
                .and_then(|v| u32::try_from(v).ok())
                .expect("u32 solar system id");
            let cnt = cv.as_i64().expect("i64 count");
            (sid, cnt)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docked_pochven_and_wormhole_ore_anomaly_reads_match_seeded_counts_without_db_writes() {
    use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;

    let sde = shared_sde();
    let kw = HashMap::new();

    let pochven_sys = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .min()
        .expect("SDE contains Pochven solar system");
    let wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (1..=4).contains(&c) && !shat
        })
        .min()
        .expect("SDE contains non-shattered C1-C4 wormhole");
    let c56_wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (5..=6).contains(&c) && !shat
        })
        .min()
        .expect("SDE contains non-shattered C5-C6 wormhole");
    let shat_wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| system_wormhole_profile_sde(&sde, sid).0 == 13)
        .min()
        .expect("SDE contains C13 shattered wormhole");

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        db,
    ));

    let mut docked = SessionState::new(9931);
    docked.user_id = Some(95);
    docked.character_id = Some(90_099_195);
    docked.station_id = Some(60_003_760);

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    // 1. Unseeded Pochven: deterministic [primary_did, 8974] read-only, matches undocked seeded shape/count.
    docked.solar_system_id = Some(pochven_sys);
    let poch_docked_rep = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked Pochven GetOreAnomalyInstances");
    let poch_docked_cnt = unpack_system_counts_dict(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomaliesCount",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked Pochven GetOreAnomaliesCount"),
    );
    let poch_range_cnt = unpack_system_counts_dict(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomaliesCountInRange",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked Pochven GetOreAnomaliesCountInRange"),
    );
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.is_empty()));

    let poch_docked_map = unpack_system_dungeon_instances(&poch_docked_rep, pochven_sys);
    assert_eq!(poch_docked_map.len(), 2, "Pochven must expose primary + 8974 ore dungeons");
    assert_eq!(poch_docked_cnt.get(&pochven_sys).copied(), Some(2));
    assert_eq!(poch_range_cnt.get(&pochven_sys).copied(), Some(2));
    for (&sid, &cnt) in &poch_range_cnt {
        if sde.solar_systems.get(&sid).is_some_and(|s| s.region_id == 10_000_070) {
            assert_eq!(cnt, 2, "Pochven neighbor {sid} count in range");
        }
    }

    // 2. Unseeded non-shattered C1-C4 WH: all 4 deterministic ore dungeons, zero DB mutations.
    docked.solar_system_id = Some(wh_sys);
    let wh_docked_rep = handle(
        &ctx,
        &mut docked,
        "dungeonInstanceCacheMgr",
        "GetOreAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked WH GetOreAnomalyInstances");
    let wh_docked_cnt = unpack_system_counts_dict(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomaliesCount",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked WH GetOreAnomaliesCount"),
    );
    let wh_range_cnt = unpack_system_counts_dict(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomaliesCountInRange",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked WH GetOreAnomaliesCountInRange"),
    );
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.is_empty()));

    let wh_docked_map = unpack_system_dungeon_instances(&wh_docked_rep, wh_sys);
    assert_eq!(wh_docked_map.len(), 4, "C1-C4 WH must expose all 4 ore dungeons");
    assert_eq!(wh_docked_cnt.get(&wh_sys).copied(), Some(4));
    assert_eq!(wh_range_cnt.get(&wh_sys).copied(), Some(4));

    // 3. Shattered C13 WH exposes deterministic Shattered Debris Field; C5/C6 random draw stays 0 unseeded.
    docked.solar_system_id = Some(shat_wh_sys);
    let shat_docked_map = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked C13 GetOreAnomalyInstances"),
        shat_wh_sys,
    );
    assert_eq!(shat_docked_map.len(), 1);
    docked.solar_system_id = Some(c56_wh_sys);
    let c56_docked_cnt = unpack_system_counts_dict(
        &handle(
            &ctx,
            &mut docked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomaliesCount",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked C5-C6 GetOreAnomaliesCount"),
    );
    assert_eq!(c56_docked_cnt.get(&c56_wh_sys).copied(), Some(0));
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);

    // 4. Undocked seeded parity for Pochven and C1-C4 WH matches unseeded dungeon IDs and counts.
    let mut undocked = docked.clone();
    undocked.station_id = None;
    undocked.solar_system_id = Some(pochven_sys);
    let poch_seeded_map = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut undocked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("undocked Pochven GetOreAnomalyInstances"),
        pochven_sys,
    );
    let poch_seeded_keys: std::collections::BTreeSet<u32> = poch_seeded_map.keys().copied().collect();
    let poch_docked_keys: std::collections::BTreeSet<u32> = poch_docked_map.keys().copied().collect();
    assert_eq!(poch_docked_keys, poch_seeded_keys);

    undocked.solar_system_id = Some(wh_sys);
    let wh_seeded_map = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut undocked,
            "dungeonInstanceCacheMgr",
            "GetOreAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("undocked WH GetOreAnomalyInstances"),
        wh_sys,
    );
    let wh_seeded_keys: std::collections::BTreeSet<u32> = wh_seeded_map.keys().copied().collect();
    let wh_docked_keys: std::collections::BTreeSet<u32> = wh_docked_map.keys().copied().collect();
    assert_eq!(wh_docked_keys, wh_seeded_keys);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docked_synthetic_ore_and_ice_instance_ids_warp_before_and_after_seeding() {
    use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;

    const SITE_NOT_FOUND_ERR: &str = "SiteNotFound";
    let sde = shared_sde();
    let kw = HashMap::new();
    let designated_ice_sys: SolarSystemId = 30_000_180;
    let pochven_sys = sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .min()
        .expect("SDE contains Pochven solar system");
    let wh_sys = sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (1..=4).contains(&c) && !shat
        })
        .min()
        .expect("SDE contains non-shattered C1-C4 wormhole");

    let probe_ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::new_empty()),
    );
    let non_designated_ice_sys = sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| {
            !crate::services::mining_sites::is_designated_ice_system(sid)
                && sde
                    .solar_systems
                    .get(&sid)
                    .is_some_and(|s| s.region_id != 10_000_070)
                && unseeded_ice_belt_dungeon_for_system(&probe_ctx, sid).is_some()
        })
        .min()
        .expect("SDE contains non-designated known-space ice belt system");

    async fn setup_space_session(
        sde: &Arc<SdeRegistry>,
        sys_id: SolarSystemId,
        char_id: i64,
        uname: &str,
    ) -> (Arc<ServerContext>, SessionState, i64) {
        let db = Arc::new(WorldRepository::new_empty());
        db.provision_gm_account(uname, "pw", char_id, uname);
        let (uid, ship_id) = db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).expect("char");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            let uid = ch.user_id;
            let ship_id = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = i64::from(sys_id);
            }
            (uid, ship_id)
        });
        let ctx = Arc::new(ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            db,
        ));
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: ship_id,
                    type_id: 587,
                    position: glam::DVec3::ZERO,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(char_id, ship_id, 587),
            )
            .await
            .expect("spawn ball");
        let mut sess = SessionState::new(9950 + (char_id % 1000) as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        (ctx, sess, ship_id)
    }

    async fn assert_warps_to_anomaly(
        ctx: &ServerContext,
        sess: &mut SessionState,
        sys_id: SolarSystemId,
        ship_id: i64,
        arg: PyRep,
        expected_did: u32,
        anomaly_type: &str,
        synthetic_iid: i64,
        kw: &HashMap<String, PyRep>,
    ) {
        let rep = handle(ctx, sess, "custom_agency_provider", "warp_to_content", &[arg], kw)
            .await
            .expect("handled")
            .expect("warp_to_content succeeds");
        assert_eq!(rep, PyRep::None);

        let (seeded_iid, anom_pos) = ctx.db.with_snapshot(|snap| {
            let prefix = format!("DNG-{expected_did}-");
            let anom = snap
                .cosmic_anomalies
                .get(&sys_id)
                .into_iter()
                .flatten()
                .find(|a| {
                    a.anomaly_type == anomaly_type
                        && !a.cleared
                        && a.anomaly_id.starts_with(&prefix)
                })
                .expect("matching seeded anomaly");
            let (_, seq) = parse_dng_site_id(&anom.anomaly_id).expect("DNG anomaly_id");
            (seq, glam::DVec3::from_array(anom.position))
        });
        assert_ne!(
            seeded_iid, synthetic_iid,
            "seeded sequential DB id must differ from pre-seed docked synthetic id"
        );
        let sys_h = ctx.systems.get_handle(sys_id).expect("ballpark handle");
        let (_, ball) = sys_h
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .expect("ship ball");
        match ball.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert_eq!(destination, anom_pos);
            }
            other => panic!("expected Warp mode, got {other:?}"),
        }
        sys_h
            .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
            .await
            .unwrap();
        sys_h.set_capacitor(ship_id, 300.0).await.unwrap();
    }

    async fn assert_post_seed_synthetic_warps_no_reseed(
        ctx: &ServerContext,
        sess: &mut SessionState,
        sys_id: SolarSystemId,
        ship_id: i64,
        expected_did: u32,
        anomaly_type: &str,
        synthetic_iid: i64,
        kw: &HashMap<String, PyRep>,
    ) {
        let char_id = sess.character_id.expect("char");
        let (seeded_anoms, seeded_sigs, seeded_asteroids, seeded_next_id) =
            ctx.db.with_snapshot(|s| {
                (
                    s.cosmic_anomalies.get(&sys_id).cloned(),
                    s.cosmic_signatures.get(&sys_id).cloned(),
                    s.asteroids.len(),
                    s.next_entity_id,
                )
            });
        // Direct read-only lookup of synthetic instanceID (with and without dungeonID) preserves DB state.
        let e0 = ctx.db.state_epoch();
        let c0 = ctx.db.tx_commit_count();
        for did_opt in [Some(expected_did), None] {
            let (pos, unscanned, _, _) = resolve_warp_target_readonly(
                ctx,
                char_id,
                sys_id,
                Some(synthetic_iid),
                did_opt,
            );
            assert!(pos.is_some());
            assert!(!unscanned);
        }
        assert_eq!(ctx.db.state_epoch(), e0);
        assert_eq!(ctx.db.tx_commit_count(), c0);

        for after_arg in [
            PyRep::keyval([
                ("dungeonID", PyRep::Integer(i64::from(expected_did))),
                ("instanceID", PyRep::Integer(synthetic_iid)),
            ]),
            PyRep::Integer(synthetic_iid),
        ] {
            assert_warps_to_anomaly(
                ctx,
                sess,
                sys_id,
                ship_id,
                after_arg,
                expected_did,
                anomaly_type,
                synthetic_iid,
                kw,
            )
            .await;
        }
        let (after_anoms, after_sigs, after_asteroids, after_next_id) =
            ctx.db.with_snapshot(|s| {
                (
                    s.cosmic_anomalies.get(&sys_id).cloned(),
                    s.cosmic_signatures.get(&sys_id).cloned(),
                    s.asteroids.len(),
                    s.next_entity_id,
                )
            });
        assert_eq!(after_anoms, seeded_anoms);
        assert_eq!(after_sigs, seeded_sigs);
        assert_eq!(after_asteroids, seeded_asteroids);
        assert_eq!(after_next_id, seeded_next_id);
    }

    // 1. Ore anomalies (High-Sec & Pochven with contentPiece, WH with bare synthetic instanceID), before & after seeding.
    for (idx, (sys_id, use_bare_before)) in [
        (designated_ice_sys, false),
        (pochven_sys, false),
        (wh_sys, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (ctx, mut sess, ship_id) =
            setup_space_session(&sde, sys_id, 90_099_300 + idx as i64, &format!("ore_iid_{idx}"))
                .await;

        sess.station_id = Some(60_003_760);
        let docked_map = unpack_system_dungeon_instances(
            &handle(
                &ctx,
                &mut sess,
                "dungeonInstanceCacheMgr",
                "GetOreAnomalyInstances",
                &[],
                &kw,
            )
            .await
            .expect("handled")
            .expect("docked GetOreAnomalyInstances"),
            sys_id,
        );
        assert!(ctx.db.with_snapshot(|s| !s.cosmic_anomalies.contains_key(&sys_id)));
        let (&ore_did, ore_iids) = docked_map
            .iter()
            .min_by_key(|(&did, _)| did)
            .expect("at least one ore dungeon");
        let synth_iid = ore_iids[0];
        sess.station_id = None;

        // Cross-system synthetic ID & mismatched {dungeonID, instanceID} must stay SiteNotFound read-only.
        let e0 = ctx.db.state_epoch();
        let c0 = ctx.db.tx_commit_count();
        let wrong_sys_iid = deterministic_mining_instance_id(ore_did, sys_id.wrapping_add(1));
        for bad_arg in [
            PyRep::Integer(wrong_sys_iid),
            PyRep::keyval([
                ("dungeonID", PyRep::Integer(i64::from(ore_did + 1))),
                ("instanceID", PyRep::Integer(synth_iid)),
            ]),
        ] {
            let err = handle(
                &ctx,
                &mut sess,
                "custom_agency_provider",
                "warp_to_content",
                &[bad_arg],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_err();
            assert_eq!(err.code, SITE_NOT_FOUND_ERR);
        }
        assert_eq!(ctx.db.state_epoch(), e0);
        assert_eq!(ctx.db.tx_commit_count(), c0);

        let before_arg = if use_bare_before {
            PyRep::Integer(synth_iid)
        } else {
            PyRep::keyval([
                ("dungeonID", PyRep::Integer(i64::from(ore_did))),
                ("instanceID", PyRep::Integer(synth_iid)),
            ])
        };
        assert_warps_to_anomaly(
            &ctx,
            &mut sess,
            sys_id,
            ship_id,
            before_arg,
            ore_did,
            "OreAnomaly",
            synth_iid,
            &kw,
        )
        .await;

        assert_post_seed_synthetic_warps_no_reseed(
            &ctx,
            &mut sess,
            sys_id,
            ship_id,
            ore_did,
            "OreAnomaly",
            synth_iid,
            &kw,
        )
        .await;
    }

    // 2. Ice belts (designated AND non-designated), before & after seeding via docked synthetic instanceIDs.
    for (idx, (sys_id, use_bare_before)) in [
        (designated_ice_sys, false),
        (non_designated_ice_sys, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (ctx, mut sess, ship_id) =
            setup_space_session(&sde, sys_id, 90_099_400 + idx as i64, &format!("ice_iid_{idx}"))
                .await;

        sess.station_id = Some(60_003_760);
        let docked_ice = unpack_system_dungeon_instances(
            &handle(
                &ctx,
                &mut sess,
                "dungeonInstanceCacheMgr",
                "GetIceBeltInstances",
                &[],
                &kw,
            )
            .await
            .expect("handled")
            .expect("docked GetIceBeltInstances"),
            sys_id,
        );
        assert!(ctx.db.with_snapshot(|s| !s.cosmic_anomalies.contains_key(&sys_id)));
        let (&ice_did, ice_iids) = docked_ice
            .iter()
            .min_by_key(|(&did, _)| did)
            .expect("ice belt dungeon");
        let synth_ice_iid = ice_iids[0];
        sess.station_id = None;

        let before_arg = if use_bare_before {
            PyRep::Integer(synth_ice_iid)
        } else {
            PyRep::keyval([
                ("dungeonID", PyRep::Integer(i64::from(ice_did))),
                ("instanceID", PyRep::Integer(synth_ice_iid)),
            ])
        };
        assert_warps_to_anomaly(
            &ctx,
            &mut sess,
            sys_id,
            ship_id,
            before_arg,
            ice_did,
            ICE_BELT_ANOMALY_TYPE,
            synth_ice_iid,
            &kw,
        )
        .await;

        assert_post_seed_synthetic_warps_no_reseed(
            &ctx,
            &mut sess,
            sys_id,
            ship_id,
            ice_did,
            ICE_BELT_ANOMALY_TYPE,
            synth_ice_iid,
            &kw,
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_combat_anomaly_instances_seasonal_cooldown_and_readonly_purity() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let pilot_id: i64 = 90_099_701;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("season_cache_pilot", "pw", pilot_id, "SeasonCachePilot");
    let (uid, ship_id) = db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).expect("char");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        (ch.user_id, ch.active_ship_id)
    });
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    let mut sess = SessionState::new(9950 + (pilot_id % 1000) as u64);
    sess.user_id = Some(uid);
    sess.character_id = Some(pilot_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = Default::default();

    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_775_800_000,
    )));
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.season_id = 1;
        cfg.season_name = "Crimson Harvest".into();
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 7_200 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    // 1. Docked GetCombatAnomalyInstances is strictly read-only and must not spawn seasonal sites.
    sess.station_id = Some(60_003_760);
    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();
    let docked_pre = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetCombatAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked GetCombatAnomalyInstances"),
        sys_id,
    );
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);
    assert!(
        ctx.db.with_snapshot(|s| !s
            .cosmic_anomalies
            .get(&sys_id)
            .into_iter()
            .flatten()
            .any(|a| a.anomaly_type == "SeasonalEventSite")),
        "docked GetCombatAnomalyInstances must not spawn seasonal sites ({docked_pre:?})"
    );

    // 2. Undocked GetCombatAnomalyInstances seeds one seasonal site at SDE-derived coordinates.
    sess.station_id = None;
    let undocked_spawned = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetCombatAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("undocked GetCombatAnomalyInstances"),
        sys_id,
    );
    let (site1, pos1) = ctx.db.with_snapshot(|snap| {
        let rec = snap.pve_special_sites.seasonal_event_sites[&sys_id][0].clone();
        let anom = snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
            .expect("spawned seasonal anomaly");
        assert!(
            undocked_spawned
                .get(&rec.dungeon_id)
                .is_some_and(|ids| ids.contains(&rec.instance_id)),
            "undocked reply must expose spawned seasonal site"
        );
        (rec, anom.position)
    });

    // 3. Complete all objectives -> immediate GetCombatAnomalyInstances (docked & undocked) must NOT respawn.
    assert!(
        crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site1.boss_npc_ball_id,
            Some(pilot_id),
        )
        .await
        .is_empty()
    );
    assert!(
        crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site1.escort_ball_ids[0],
            Some(pilot_id),
        )
        .await
        .is_empty()
    );
    assert_eq!(
        crate::services::pve_special_sites_combat::on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site1.escort_ball_ids[1],
            Some(pilot_id),
        )
        .await
        .len(),
        1
    );

    let during_cooldown = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetCombatAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("cooldown GetCombatAnomalyInstances"),
        sys_id,
    );
    assert!(
        !during_cooldown.values().flatten().any(|&id| id == site1.instance_id),
        "completed seasonal site must stay absent"
    );
    assert!(
        ctx.db.with_snapshot(|s| !s
            .cosmic_anomalies
            .get(&sys_id)
            .into_iter()
            .flatten()
            .any(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)),
        "GetCombatAnomalyInstances must not respawn seasonal site before cooldown expires"
    );

    ctx.db.clock().advance_secs(500);
    let _ = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("mid-cooldown GetCombatAnomalyInstances");
    assert!(
        ctx.db.with_snapshot(|s| !s
            .cosmic_anomalies
            .get(&sys_id)
            .into_iter()
            .flatten()
            .any(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)),
        "partial cooldown advance must still block seasonal respawn"
    );

    // 4. After cooldown expires, undocked GetCombatAnomalyInstances respawns at new coordinates.
    ctx.db.clock().advance_secs(
        crate::services::pve_special_sites_combat::SEASONAL_RESPAWN_COOLDOWN_SECS,
    );
    let after_cooldown = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetCombatAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("post-cooldown GetCombatAnomalyInstances"),
        sys_id,
    );
    let (site2_iid, site2_did, pos2) = ctx.db.with_snapshot(|snap| {
        let rec = snap.pve_special_sites.seasonal_event_sites[&sys_id]
            .iter()
            .find(|s| !s.completed)
            .expect("respawned site after cooldown");
        let anom = snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
            .expect("respawned anomaly");
        assert!(after_cooldown.get(&rec.dungeon_id).is_some_and(|ids| ids.contains(&rec.instance_id)));
        (rec.instance_id, rec.dungeon_id, anom.position)
    });
    assert_ne!(site2_iid, site1.instance_id);
    assert_ne!(pos2, pos1, "respawned seasonal site must not reuse identical coordinates");

    // 5. Advance clock past window end -> docked read-only call hides expired seasonal site with zero writes.
    ctx.db.clock().advance_secs(10_000);
    sess.station_id = Some(60_003_760);
    let epoch_pre_exp = ctx.db.state_epoch();
    let commits_pre_exp = ctx.db.tx_commit_count();
    let docked_expired = unpack_system_dungeon_instances(
        &handle(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetCombatAnomalyInstances",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("docked expired GetCombatAnomalyInstances"),
        sys_id,
    );
    assert_eq!(ctx.db.state_epoch(), epoch_pre_exp);
    assert_eq!(ctx.db.tx_commit_count(), commits_pre_exp);
    assert!(
        !docked_expired
            .get(&site2_did)
            .is_some_and(|ids| ids.contains(&site2_iid)),
        "docked read-only GetCombatAnomalyInstances must hide expired seasonal anomalies"
    );
}
