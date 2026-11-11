//! Unit tests for `cosmetics_events` (`materialize_system_lod2`, `interdict_convoy`, `shipCosmeticsMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lod2_materialization_convoy_interdiction_and_skin_grants() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    // Non-GM rejected on materialize_system_lod2, interdict_convoy, GiveSkin
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[]).await.unwrap_err().code,
        "InsufficientRole"
    );
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[]).await.unwrap_err().code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );

    sess.role = service_roles::ROLE_GMH;

    // materialize_system_lod2 validation & execution
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(30_000_144)])
            .await
            .unwrap_err()
            .code,
        "CannotMaterializeRemoteSystem"
    );
    assert!(materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(sys_id as i64)])
        .await
        .is_ok());

    // interdict_convoy validation branches
    sess.station_id = Some(station_id);
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(100_000_001);
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "ConvoyNotFound"
    );

    // Find a valid permanent SKIN license in SDE (category 91)
    let (lic_tid, skin_id, valid_ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, sk.skin_id, sk.types[0]))
        })
        .expect("SDE must contain at least one category 91 skin license");

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GiveSkin",
            &[PyRep::Integer(skin_id as i64), PyRep::Integer(34)], // 34 is Tritanium, not a compatible ship
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    let give_rep = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[
            PyRep::Integer(skin_id as i64),
            PyRep::Integer(valid_ship_tid as i64),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give_rep.dict_get("licensed"), Some(&PyRep::Bool(true)));

    // ActivateSkinLicense
    sess.station_id = Some(station_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(34)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SkinLicenseItemNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            920_001,
            InventoryItemRecord {
                item_id: 920_001,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    let act_rep = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act_rep.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&920_001).map(|i| i.quantity)),
        Some(1)
    );

    // ActivateSkinLicense with quantity == 1 removes item, plus error branches
    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut sess, "shipCosmeticsMgr", unknown_method, &[], &kw).await.is_none());
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[PyRep::Integer(-5)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    // GiveSkin using license typeID resolves via skin_licenses fallback
    let give_by_lic = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give_by_lic.dict_get("licensed"), Some(&PyRep::Bool(true)));

    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(-1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64), PyRep::Integer(34)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );
    let act_rep2 = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64), PyRep::Integer(valid_ship_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act_rep2.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&920_001)));

    // materialize_system_lod2 missing/unknown solarSystemID branches
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(99_999_999)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    let mut no_sys_sess = sess.clone();
    no_sys_sess.solar_system_id = None;
    assert_eq!(
        materialize_system_lod2(&ctx, &no_sys_sess, &[])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    no_sys_sess.solar_system_id = Some(99_999_999);
    assert_eq!(
        materialize_system_lod2(&ctx, &no_sys_sess, &[])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interdict_convoy_ballpark_range_and_happy_path() {
    use crate::living_world::LivingWorldConvoy;
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    sess.role = service_roles::ROLE_GMH;
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let convoy_id = 880_000_101_i64;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: crate::living_world::NpcArchetype::HaulerConvoy,
        ship_type_id: 1944,
        origin_system_id: sys_id,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 100,
        lod_level: 2,
        destroyed: false,
    });
    // Wrong system
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "ConvoyWrongSystem"
    );

    // Move convoy to sys_id; attacker ship not in space yet
    let convoy_id2 = 880_000_102_i64;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id: convoy_id2,
        archetype: crate::living_world::NpcArchetype::HaulerConvoy,
        ship_type_id: 1944,
        origin_system_id: sys_id,
        current_system_id: sys_id,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 100,
        lod_level: 2,
        destroyed: false,
    });
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(convoy_id2)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn attacker ship; convoy not materialized on grid
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(convoy_id2)])
            .await
            .unwrap_err()
            .code,
        "ConvoyNotMaterializedOnGrid"
    );

    // Spawn convoy > 30km away -> OutsideConvoyInterdictionRange
    let _ = sys_h
        .spawn_ball(Ball {
            id: convoy_id2,
            type_id: 1944,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(convoy_id2)])
            .await
            .unwrap_err()
            .code,
        "OutsideConvoyInterdictionRange"
    );

    // Move convoy within 30km -> happy path interdiction!
    let _ = sys_h.set_ball_position(convoy_id2, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let res = interdict_convoy(&ctx, &sess, &[PyRep::Integer(convoy_id2)])
        .await
        .expect("interdict_convoy should succeed within 30km");
    assert_eq!(res.dict_get("convoyID"), Some(&PyRep::Integer(convoy_id2)));
    assert!(res.dict_get("wreckItemID").and_then(|v| v.as_i64()).is_some_and(|id| id > 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cosmetics_events_edge_cases_and_convoy_interdiction_success() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let kw = HashMap::new();

    // 1. Unauthenticated errors
    let mut unauth = SessionState::default();
    assert!(materialize_system_lod2(&ctx, &unauth, &[]).await.is_err());
    assert!(interdict_convoy(&ctx, &unauth, &[]).await.is_err());
    assert!(handle(&ctx, &mut unauth, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle(&ctx, &mut unauth, "shipCosmeticsMgr", "ActivateSkinLicense", &[], &kw)
        .await
        .unwrap()
        .is_err());
    let non_cosmetics_svc = ["other", "Service"].concat();
    let unknown_method = ["Unknown", "Method"].concat();
    assert!(handle(&ctx, &mut unauth, &non_cosmetics_svc, "GiveSkin", &[], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut unauth, "shipCosmeticsMgr", &unknown_method, &[], &kw)
        .await
        .is_none());

    // 2. materialize_system_lod2: missing solar system, unknown SDE system, no-arg happy path
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    sess.role = service_roles::ROLE_GMH;
    sess.solar_system_id = None;
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[]).await.unwrap_err().code,
        "UnknownSolarSystem"
    );
    sess.solar_system_id = Some(99_999_999);
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[]).await.unwrap_err().code,
        "UnknownSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        materialize_system_lod2(&ctx, &sess, &[PyRep::Integer(99_999_999)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    assert!(materialize_system_lod2(&ctx, &sess, &[]).await.is_ok());

    // 3. interdict_convoy: structure_id docked, ConvoyWrongSystem, ShipNotInSpace, ConvoyNotMaterializedOnGrid,
    //    OutsideConvoyInterdictionRange, and successful interdiction!
    sess.structure_id = Some(1_000_000_001_000);
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_001)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.living_world.seed_default_convoys();
    // Convoy 800_000_002 is in 30_000_144 while session is in 30_000_142
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_002)])
            .await
            .unwrap_err()
            .code,
        "ConvoyWrongSystem"
    );
    // Convoy 800_000_001 is in 30_000_142, but ship is not yet in ballpark
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_001)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship far away (0,0,0), before convoy ball is materialized
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_001)])
            .await
            .unwrap_err()
            .code,
        "ConvoyNotMaterializedOnGrid"
    );

    // Materialize LOD2 so convoy 800_000_001 is on grid
    let _ = materialize_system_lod2(&ctx, &sess, &[]).await.unwrap();
    let (_, convoy_ball) = sys_handle
        .get_ball_snapshot(800_000_001)
        .await
        .unwrap()
        .expect("convoy ball materialized");
    // Ship is far (>30 km) -> OutsideConvoyInterdictionRange
    assert_eq!(
        interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_001)])
            .await
            .unwrap_err()
            .code,
        "OutsideConvoyInterdictionRange"
    );

    // Move ship within 5 km of convoy ball -> interdict succeeds!
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: convoy_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let res = interdict_convoy(&ctx, &sess, &[PyRep::Integer(800_000_001)])
        .await
        .expect("interdict succeeds");
    assert_eq!(res.dict_get("convoyID"), Some(&PyRep::Integer(800_000_001)));
    let wreck_id = res.dict_get("wreckItemID").and_then(|v| v.as_i64()).unwrap();
    assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(800_000_001).await.unwrap().is_none());
    assert!(ctx.db.with_snapshot(|s| s.item_positions.contains_key(&wreck_id)));

    // 4. GiveSkin & ActivateSkinLicense validation & edge cases
    assert_eq!(
        handle(&ctx, &mut sess, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GiveSkin",
            &[PyRep::Integer(-1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "GiveSkin",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );

    let (lic_tid, _skin_id, valid_ship_tid) = ctx
        .sde
        .skin_licenses
        .iter()
        .find_map(|(&ltid, lic)| {
            let st = ctx.sde.types.get(&ltid).filter(|t| t.category_id == 91)?;
            let sk = ctx.sde.skins.get(&lic.skin_id).filter(|s| !s.types.is_empty())?;
            Some((st.type_id, sk.skin_id, sk.types[0]))
        })
        .expect("SDE category 91 skin license");

    // GiveSkin by passing skin_license typeID instead of skin_id
    let give_lic = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(lic_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give_lic.dict_get("licensed"), Some(&PyRep::Bool(true)));

    // ActivateSkinLicense: missing arg, negative, unknown type, incompatible ship, quantity==1 removal
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(-5)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(lic_tid as i64), PyRep::Integer(34)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            920_002,
            InventoryItemRecord {
                item_id: 920_002,
                type_id: lic_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let act_single = handle(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(lic_tid as i64), PyRep::Integer(valid_ship_tid as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act_single.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&920_002)));
}
