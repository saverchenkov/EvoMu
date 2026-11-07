//! Semantic unit tests split from `fitting_ship_bays_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::fitting_ship_bays_extra_unit_tests::*;
use super::inventory_fitting::fitting_ship_bays::*;
use eve_db::{
    InventoryFlag, PersistedSpaceObjectMeta,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn ship_jettison_and_ship_drop_validation_and_phase_anchor_branches() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let jita_sys = 30_000_142;
    let ship_id = 1_200_001;
    let kw = HashMap::new();

    spawn_ball_at(&ctx, low_sys, ship_id, 998_001, char_id, DVec3::new(500_000.0, 0.0, 0.0)).await;
    put_item(&ctx, ship_id, 998_001, char_id, low_sys as i64, InventoryFlag::None, 1, true);

    let (sma_ship_id, implant_id, cargo_ship_id, cont_id, huge_ore_id) =
        (1_200_010, 1_200_011, 1_200_012, 1_200_013, 1_200_014);
    let cont_tid = ctx.sde.types.values().find(|t| t.group_id == 340).unwrap().type_id;
    put_item(&ctx, sma_ship_id, 587, char_id, ship_id, InventoryFlag::ShipHangar, 1, true);
    put_item(&ctx, implant_id, 34, char_id, ship_id, InventoryFlag::Implant, 1, false);
    put_item(&ctx, cargo_ship_id, 587, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    put_item(&ctx, cont_id, cont_tid, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    put_item(&ctx, huge_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 10_000_000, false);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(low_sys),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    assert_eq!(
        ship_jettison(&ctx, &mut sess, &[], &kw).await.unwrap_err().code,
        "InvalidArgument"
    );
    for (id, expected_code) in [
        (999_999_999, "ItemNotFound"),
        (sma_ship_id, "CannotJettisonStoredShip"),
        (implant_id, "CannotJettisonItem"),
        (cargo_ship_id, "CannotJettisonShip"),
        (cont_id, "CannotPutContainerInContainer"),
        (huge_ore_id, "CapacityExceeded"),
    ] {
        let mut kw_id = HashMap::new();
        kw_id.insert("itemID".into(), PyRep::Integer(id));
        assert_eq!(
            ship_jettison(&ctx, &mut sess, &[], &kw_id).await.unwrap_err().code,
            expected_code
        );
    }

    // ship_drop guards: docked, foreign owner, fitted, ShipHangar, Zarzakh, security band, Phase Anchor
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        ship_drop(&ctx, &mut docked, &[PyRep::Integer(cont_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDropWhileDocked"
    );
    assert_eq!(
        ship_drop(
            &ctx,
            &mut sess,
            &[PyRep::Integer(cont_id), PyRep::Integer(99_888_777)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    assert_eq!(
        ship_drop(&ctx, &mut sess, &[PyRep::Integer(sma_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDropStoredShip"
    );

    let (sec_depot_id, phase_anchor_id) = (1_200_020, 1_200_021);
    put_item(&ctx, sec_depot_id, 998_004, char_id, ship_id, InventoryFlag::Cargo, 1, true);
    put_item(
        &ctx,
        phase_anchor_id,
        crate::services::mining_phased_autotarget::TYPE_MOBILE_PHASE_ANCHOR,
        char_id,
        ship_id,
        InventoryFlag::Cargo,
        1,
        true,
    );

    // Zarzakh (30_100_000) and High-sec (30_000_142) drop guards
    let mut zar_sess = sess.clone();
    zar_sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        ship_drop(&ctx, &mut zar_sess, &[PyRep::Integer(sec_depot_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInZarzakh"
    );

    let mut hs_sess = sess.clone();
    hs_sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        ship_drop(&ctx, &mut hs_sess, &[PyRep::Integer(phase_anchor_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotDeployInHighSec"
    );
    assert_eq!(
        ship_drop(&ctx, &mut hs_sess, &[PyRep::Integer(sec_depot_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "UserError"
    );

    // In low-sec, full-stack drop of Mobile Phase Anchor succeeds
    let drop_ok = ship_drop(&ctx, &mut sess, &[PyRep::Integer(phase_anchor_id)], &kw)
        .await
        .expect("drop phase anchor in low-sec");
    assert!(matches!(drop_ok, PyRep::Dict(_)));
}

#[tokio::test]
async fn ship_scoop_and_store_feb_fleet_hangar_infra_depot_and_cargo_holds() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let kw = HashMap::new();

    let (pod_id, feb_host_id, frig1_id, frig2_id, hauler_id) =
        (1_300_001, 1_300_002, 1_300_003, 1_300_004, 1_300_005);
    put_item(&ctx, pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, feb_host_id, 998_002, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, frig1_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, frig2_id, 998_003, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, hauler_id, 648, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let mut docked_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(pod_id),
        ..Default::default()
    };

    // Docked StoreVessel into FEB-only host: non-frigate rejected, first frigate stored in FrigateEscapeBay, second exceeds capacity
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut docked_sess,
            "StoreVessel",
            &[PyRep::Integer(hauler_id), PyRep::Integer(feb_host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidFrigateGroup"
    );
    let feb_ok = ship_scoop_and_store(
        &ctx,
        &mut docked_sess,
        "StoreVessel",
        &[PyRep::Integer(frig1_id), PyRep::Integer(feb_host_id)],
        &kw,
    )
    .await
    .expect("store frigate in FEB");
    assert_eq!(
        feb_ok.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64))
    );
    assert_eq!(
        ship_scoop_and_store(
            &ctx,
            &mut docked_sess,
            "StoreVessel",
            &[PyRep::Integer(frig2_id), PyRep::Integer(feb_host_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );

    // Scoop while docked -> MustBeInSpace
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut docked_sess, "Scoop", &[PyRep::Integer(frig2_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "MustBeInSpace"
    );

    // Move to space with MultiBay Capital (998_001)
    let (cap_id, active_frig_id, unpiloted_frig_id, depot_id, ore_can_id, anchored_id) =
        (1_300_010, 1_300_011, 1_300_012, 1_300_013, 1_300_014, 1_300_015);
    let sec_cont_tid = ctx.sde.types.values().find(|t| t.group_id == 340).unwrap().type_id;
    put_item(&ctx, cap_id, 998_001, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, active_frig_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, unpiloted_frig_id, 998_003, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, depot_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, ore_can_id, sec_cont_tid, 0, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, anchored_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.insert(
            anchored_id,
            PersistedSpaceObjectMeta {
                anchor_completed: true,
                ..Default::default()
            },
        );
    });

    for (id, tid) in [
        (cap_id, 998_001),
        (active_frig_id, 998_003),
        (unpiloted_frig_id, 998_003),
        (depot_id, 33_474),
        (ore_can_id, sec_cont_tid),
        (anchored_id, 33_474),
    ] {
        spawn_ball_at(&ctx, sys_id, id, tid, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    }

    // 1. In-space StoreVessel on unpiloted_frig_id while piloting active_frig_id
    let mut space_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(active_frig_id),
        ..Default::default()
    };
    let unpiloted_res = ship_scoop_and_store(
        &ctx,
        &mut space_sess,
        "StoreVessel",
        &[PyRep::Integer(unpiloted_frig_id), PyRep::Integer(cap_id)],
        &kw,
    )
    .await
    .expect("store unpiloted ship in host bay");
    assert_eq!(unpiloted_res.dict_get("storedShipID"), Some(&PyRep::Integer(unpiloted_frig_id)));

    // 2. Pilot cap_id and test ScoopToMobileDepotHold, ScoopToInfrastructureHold, ScoopToFleetHangar, and Scoop
    space_sess.ship_id = Some(cap_id);
    assert_eq!(
        ship_scoop_and_store(&ctx, &mut space_sess, "Scoop", &[PyRep::Integer(anchored_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotScoopAnchored"
    );

    let depot_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToMobileDepotHold", &[PyRep::Integer(depot_id)], &kw)
        .await
        .expect("scoop mobile depot to mobile depot hold");
    assert_eq!(depot_scoop.dict_get("flagID"), Some(&PyRep::Integer(179)));

    // Put depot back in space and scoop to InfrastructureHold (185)
    put_item(&ctx, depot_id, 33_474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, sys_id, depot_id, 33_474, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    let infra_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToInfrastructureHold", &[PyRep::Integer(depot_id)], &kw)
        .await
        .expect("scoop deployable to infrastructure hold");
    assert_eq!(infra_scoop.dict_get("flagID"), Some(&PyRep::Integer(185)));

    // Scoop abandoned container to FleetHangar (155) -> transfers ownership to char_id
    let fh_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "ScoopToFleetHangar", &[PyRep::Integer(ore_can_id)], &kw)
        .await
        .expect("scoop container to fleet hangar");
    assert_eq!(fh_scoop.dict_get("flagID"), Some(&PyRep::Integer(155)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ore_can_id].owner_id), char_id);

    // Put container back in space and Scoop to standard Cargo (5)
    put_item(&ctx, ore_can_id, sec_cont_tid, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ball_at(&ctx, sys_id, ore_can_id, sec_cont_tid, char_id, DVec3::new(100.0, 0.0, 0.0)).await;
    let cargo_scoop = ship_scoop_and_store(&ctx, &mut space_sess, "Scoop", &[PyRep::Integer(ore_can_id)], &kw)
        .await
        .expect("scoop container to cargo");
    assert_eq!(cargo_scoop.dict_get("flagID"), Some(&PyRep::Integer(5)));
}
