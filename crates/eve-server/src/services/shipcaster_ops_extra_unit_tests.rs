//! Extra branch-coverage and mutation-killing unit tests for `shipcaster_ops`.

use super::shipcaster_ops::*;
use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::{
    LandingPadRecord, LauncherRecord, StarterLauncherRecord, with_misc_state,
};
use eve_db::{FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

async fn call_shipcaster_err(
    ctx: &ServerContext,
    session: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> BridgeRpcError {
    let kw = HashMap::new();
    handle(ctx, session, svc, method, args, &kw)
        .await
        .expect("handled")
        .expect_err("expected error")
}

#[tokio::test]
async fn beacon_constructor_deploy_contribute_deposit_and_link_limits() {
    let ctx = test_ctx();
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 990_100_001;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            let r = (s.security_status * 10.0).round() / 10.0;
            s.solar_system_id < 30_100_000 && r > 0.0 && r < 0.45
        })
        .map(|s| s.solar_system_id)
        .expect("lowsec system exists");

    let comps = ctx.sde.shipcaster_sde_components().expect("sde comps");
    let (&ctor_tid, ctor_spec) = comps.under_construction.iter().next().expect("underConstruction");
    let ctor_faction = comps
        .landing_pad_factions
        .get(&ctor_spec.built_item_type_id)
        .copied()
        .unwrap_or(500_001) as i64;

    let mut sess = auth_session(1, char_id, 98_000_001, lowsec_sys);
    sess.ship_id = Some(ship_id);

    let unauth = SessionState::new(1);
    assert!(deploy_fw_beacon_constructor(&ctx, &unauth, 1, DVec3::ZERO).await.is_err());

    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &docked, 1, DVec3::ZERO).await.unwrap_err().code, "CantDoThatWhileDocked");

    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &no_ship, 1, DVec3::ZERO).await.unwrap_err().code, "CustomError");

    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &no_sys, 1, DVec3::ZERO).await.unwrap_err().code, "CustomError");

    let mut bad_sys = sess.clone();
    bad_sys.solar_system_id = Some(99_999_999);
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &bad_sys, 1, DVec3::ZERO).await.unwrap_err().code, "InvalidSolarSystem");

    let mut jita_sess = sess.clone();
    jita_sess.solar_system_id = Some(30_000_142);
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &jita_sess, 1, DVec3::ZERO).await.unwrap_err().code, "InvalidBeaconDeploymentSystem");

    with_misc_state(&ctx, |st| {
        st.landing_pads.retain(|p| p.solar_system_id != lowsec_sys);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.remove(&lowsec_sys);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
        }
    });
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, 1, DVec3::ZERO).await.unwrap_err().code, "InvalidBeaconDeploymentSystem");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            lowsec_sys,
            FwSystemOccupationRecord {
                solar_system_id: lowsec_sys,
                owner_faction_id: ctor_faction,
                occupier_faction_id: ctor_faction,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, 1, DVec3::ZERO).await.unwrap_err().code, "FactionShipcasterAccessDenied");

    let ctor_item_id = 990_200_001_i64;
    let wrong_item_id = 990_200_002_i64;
    let bad_flag_item_id = 990_200_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_099);
        }
        for (iid, tid, flag, qty) in [
            (bad_flag_item_id, ctor_tid, InventoryFlag::Hangar, 1),
            (wrong_item_id, 34, InventoryFlag::Cargo, 1),
            (ctor_item_id, ctor_tid, InventoryFlag::Cargo, 2),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, 888_888_888, DVec3::ZERO).await.unwrap_err().code, "ItemNotFound");
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, bad_flag_item_id, DVec3::ZERO).await.unwrap_err().code, "ItemNotInCargo");
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, wrong_item_id, DVec3::ZERO).await.unwrap_err().code, "InvalidBeaconConstructorType");
    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, ctor_item_id, DVec3::ZERO).await.unwrap_err().code, "FactionShipcasterAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(ctor_faction);
        }
    });
    let deployed = deploy_fw_beacon_constructor(&ctx, &sess, ctor_item_id, DVec3::ZERO).await.expect("deploy succeeds");
    assert!(!deployed.is_built);
    assert_eq!(deployed.type_id, ctor_tid);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ctor_item_id].quantity), 1);

    assert_eq!(deploy_fw_beacon_constructor(&ctx, &sess, ctor_item_id, DVec3::ZERO).await.unwrap_err().code, "BeaconAlreadyExistsInSystem");

    assert_eq!(contribute_fw_beacon_material(&ctx, &docked, deployed.item_id, 1, 5).await.unwrap_err().code, "CantDoThatWhileDocked");
    assert_eq!(contribute_fw_beacon_material(&ctx, &sess, -999_999, 1, 5).await.unwrap_err().code, "LandingPadNotFound");
    assert_eq!(contribute_fw_beacon_material(&ctx, &jita_sess, deployed.item_id, 1, 5).await.unwrap_err().code, "InvalidSolarSystem");

    let sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert_eq!(contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, 777_777_777, 5).await.unwrap_err().code, "ItemNotFound");
    assert_eq!(contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, wrong_item_id, 5).await.unwrap_err().code, "InvalidConstructionMaterial");

    let mut deposit_pairs = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        for (&mat_tid, &req_qty) in &deployed.required_materials {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: mat_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: req_qty as i32,
                    singleton: false,
                    ..Default::default()
                },
            );
            deposit_pairs.push((PyRep::Integer(mat_tid as i64), PyRep::Integer(req_qty as i64)));
        }
    });

    let dep_res = deposit_items_from_ship(&ctx, &sess, deployed.item_id, Some(&PyRep::Dict(deposit_pairs)))
        .await
        .expect("deposit completes construction");
    if let PyRep::Tuple(items) = dep_res {
        assert_eq!(items[0], PyRep::Bool(true));
        assert_eq!(items[1], PyRep::Dict(Vec::new()));
    } else {
        panic!("expected Tuple");
    }

    assert_eq!(contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, wrong_item_id, 1).await.unwrap_err().code, "BeaconAlreadyBuilt");

    with_misc_state(&ctx, |st| {
        if let Some(l) = st.launchers.iter_mut().find(|l| l.faction_id == ctor_faction as i32) {
            l.linked_pad_ids = vec![800_001, 800_002, 800_003];
        }
        if let Some(p) = st.landing_pads.iter_mut().find(|p| p.item_id == deployed.item_id) {
            p.link_timestamp = 0;
        }
    });
    assert_eq!(link_fw_landing_pad(&ctx, deployed.item_id).unwrap_err().code, "MaxLinkedLandingPadsReached");

    let destroyed = disrupt_or_destroy_fw_landing_pad(&ctx, deployed.item_id, true).await.expect("destroy succeeds");
    assert_eq!(destroyed.item_id, deployed.item_id);
    assert!(with_misc_state(&ctx, |st| !st.landing_pads.iter().any(|p| p.item_id == deployed.item_id)));
}

#[tokio::test]
async fn starter_and_interstellar_shipcaster_jump_validation_and_execution() {
    let ctx = test_ctx();
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 990_300_001;
    let origin_sys: u32 = 30_000_142;
    let dest_sys: u32 = 30_000_144;
    let mut sess = auth_session(1, char_id, 98_000_001, origin_sys);
    sess.ship_id = Some(ship_id);

    let mut unauth = SessionState::new(99);
    for (svc, m) in [
        ("shipCasterLandingPadMgr", "GetAllLandingPads"),
        ("shipCasterLandingPadMgr", "GetFactionLandingPads"),
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster"),
        ("starterShipcasterTravelMgr", "CanCharacterJump"),
        ("starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster"),
        ("shipcasterTravelMgr", "CmdJumpThroughShipcaster"),
    ] {
        assert!(!call_shipcaster_err(&ctx, &mut unauth, svc, m, &[]).await.code.is_empty());
    }

    assert_eq!(
        call_shipcaster_err(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[PyRep::Integer(i64::MAX)]).await.code,
        "InvalidArguments"
    );

    for (args, expected) in [
        (&[][..], "InvalidArguments"),
        (&[PyRep::Integer(0)][..], "TargetNotFound"),
        (&[PyRep::Integer(999_888_777)][..], "LauncherNotFound"),
    ] {
        assert_eq!(
            call_shipcaster_err(&ctx, &mut sess, "starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster", args).await.code,
            expected
        );
    }

    let starter_id = 770_001_i64;
    let remote_starter_id = 770_002_i64;
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.remove(&char_id);
        for (iid, sys, stg) in [(starter_id, origin_sys, dest_sys), (remote_starter_id, dest_sys, origin_sys)] {
            st.starter_launchers.push(StarterLauncherRecord {
                item_id: iid,
                type_id: 93968,
                solar_system_id: sys,
                faction_id: 500_001,
                staging_system_id: stg,
                staging_station_id: 60_003_760,
                position: DVec3::ZERO,
            });
        }
    });

    assert_eq!(
        call_shipcaster_err(&ctx, &mut sess, "starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(30_002_187)]).await.code,
        "InvalidDestinationSystem"
    );
    assert_eq!(
        call_shipcaster_err(&ctx, &mut sess, "starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(remote_starter_id)]).await.code,
        "InvalidSolarSystem"
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now.saturating_sub(3600);
            ch.active_ship_id = ship_id;
            ch.solar_system_id = origin_sys;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_orig = ctx.systems.get_or_spawn(origin_sys);
    for (bid, tid) in [(ship_id, 587_u32), (starter_id, 93968_u32)] {
        let _ = sys_orig
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
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
    }

    let kw = HashMap::new();
    assert_eq!(
        handle(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[], &kw).await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut sess, "starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(starter_id)], &kw).await.unwrap().unwrap(),
        PyRep::None
    );
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(with_misc_state(&ctx, |st| st.used_starter_shipcaster_chars.contains(&char_id)));

    for (args, expected) in [
        (&[PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)][..], "TargetNotFound"),
        (&[PyRep::Integer(1)][..], "InvalidArguments"),
        (&[PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(1)][..], "TargetNotFound"),
        (&[PyRep::Integer(1), PyRep::Integer(1)][..], "InvalidArguments"),
        (&[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)][..], "TargetNotFound"),
    ] {
        assert_eq!(
            call_shipcaster_err(&ctx, &mut sess, "shipcasterTravelMgr", "CmdJumpThroughShipcaster", args).await.code,
            expected
        );
    }

    let zarzakh_launcher_id = 880_001_i64;
    let emp_launcher_id = 880_002_i64;
    let fob_pad_id = 880_101_i64;
    let emp_pad_1 = 880_102_i64;
    let emp_pad_2 = 880_103_i64;
    with_misc_state(&ctx, |st| {
        st.launchers
            .retain(|l| l.faction_id != 500_001 && l.solar_system_id != 30_100_000);
        st.launchers.push(LauncherRecord {
            item_id: zarzakh_launcher_id,
            type_id: 79018,
            solar_system_id: 30_100_000,
            faction_id: 0,
            linked_pad_ids: vec![fob_pad_id],
            position: DVec3::ZERO,
        });
        st.launchers.push(LauncherRecord {
            item_id: emp_launcher_id,
            type_id: 76465,
            solar_system_id: dest_sys,
            faction_id: 500_001,
            linked_pad_ids: vec![emp_pad_1, emp_pad_2],
            position: DVec3::ZERO,
        });
        for (pid, tid, fid) in [(fob_pad_id, 79019, 500_010), (emp_pad_1, 75928, 500_001), (emp_pad_2, 75928, 500_001)] {
            st.landing_pads.push(LandingPadRecord {
                item_id: pid,
                type_id: tid,
                built_type_id: tid,
                solar_system_id: origin_sys,
                faction_id: fid,
                is_built: true,
                link_timestamp: 100,
                is_disrupted: false,
                position: DVec3::ZERO,
                required_materials: BTreeMap::new(),
                contributed_materials: BTreeMap::new(),
            });
        }
    });

    for (wf, expected) in [(500_001, "NonPirateRefused"), (500_011, "FobFactionMismatch")] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.war_faction_id = Some(wf);
            }
        });
        assert_eq!(
            call_shipcaster_err(
                &ctx,
                &mut sess,
                "shipcasterTravelMgr",
                "CmdJumpThroughShipcaster",
                &[PyRep::Integer(zarzakh_launcher_id), PyRep::Integer(origin_sys as i64), PyRep::Integer(fob_pad_id)],
            )
            .await
            .code,
            expected
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    with_misc_state(&ctx, |st| {
        st.target_cooldowns.insert(fob_pad_id, now + 600);
    });
    assert_eq!(
        call_shipcaster_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_launcher_id), PyRep::Integer(origin_sys as i64), PyRep::Integer(fob_pad_id)],
        )
        .await
        .code,
        "ShipcasterTargetOnCooldown"
    );

    with_misc_state(&ctx, |st| {
        st.target_cooldowns.remove(&fob_pad_id);
        st.target_charges.insert(fob_pad_id, 0);
    });
    assert_eq!(
        call_shipcaster_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_launcher_id), PyRep::Integer(origin_sys as i64), PyRep::Integer(fob_pad_id)],
        )
        .await
        .code,
        "ShipcasterTargetNoCharges"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    let active_pad = active_target_pad_for_faction(&ctx, 500_001).unwrap();
    let inactive_pad_id = if active_pad.item_id == emp_pad_1 { emp_pad_2 } else { emp_pad_1 };
    assert_eq!(
        call_shipcaster_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(origin_sys as i64), PyRep::Integer(inactive_pad_id)],
        )
        .await
        .code,
        "ShipcasterTargetNotActiveInCycle"
    );
}
