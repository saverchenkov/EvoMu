//! Edge-case, ACL/militia/connectivity/cooldown/weapon-timer guards, and full jump execution
//! tests for `shipcaster_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::{
    LandingPadRecord, LauncherRecord, StarterLauncherRecord, with_misc_state,
};
use crate::services::insurgency_services::{with_insurgency_runtime, InsurgencyCampaign};
use eve_db::{FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

async fn call_err(
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

async fn spawn_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id,
            type_id,
            position: pos,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deploy_partial_contribute_deposit_and_link_disrupt_edges() {
    let ctx = test_ctx();
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 995_100_001;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            let r = (s.security_status * 10.0).round() / 10.0;
            s.solar_system_id < 30_100_000 && r > 0.0 && r < 0.45
        })
        .map(|s| s.solar_system_id)
        .expect("lowsec system");

    let comps = ctx.sde.shipcaster_sde_components().expect("comps");
    let (&ctor_tid, ctor_spec) = comps.under_construction.iter().next().expect("underConstruction");
    let ctor_faction = comps
        .landing_pad_factions
        .get(&ctor_spec.built_item_type_id)
        .copied()
        .unwrap_or(500_001) as i64;
    let (&mat_tid, mat_spec) = ctor_spec.input_items.iter().next().expect("input material");

    let mut sess = auth_session(1, char_id, 98_000_001, lowsec_sys);
    sess.ship_id = Some(ship_id);

    with_misc_state(&ctx, |st| {
        st.landing_pads.retain(|p| p.solar_system_id != lowsec_sys);
    });

    let ctor_id = 995_200_001_i64;
    let mat_id = 995_200_002_i64;
    let wrong_flag_mat = 995_200_003_i64;
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
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(ctor_faction);
        }
        snap.items.insert(
            ctor_id,
            InventoryItemRecord {
                item_id: ctor_id,
                type_id: ctor_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1, // qty == 1 -> removed cleanly on deploy
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            mat_id,
            InventoryItemRecord {
                item_id: mat_id,
                type_id: mat_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: (mat_spec.required_qty + 5) as i32,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrong_flag_mat,
            InventoryItemRecord {
                item_id: wrong_flag_mat,
                type_id: mat_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let deployed = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, DVec3::ZERO)
        .await
        .expect("deploy qty=1 succeeds");
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&ctor_id)));

    // Spawn ship far away first -> proximity check fails; then move within range
    spawn_ball(&ctx, lowsec_sys, ship_id, 587, DVec3::new(500_000.0, 0.0, 0.0)).await;
    assert!(contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, mat_id, 1)
        .await
        .is_err());
    let _ = ctx
        .systems
        .get_or_spawn(lowsec_sys)
        .set_ball_position(ship_id, DVec3::new(100.0, 0.0, 0.0))
        .await;

    // Auth / ship / system / wrong-flag / wrong-faction checks
    let unauth = SessionState::new(1);
    assert!(contribute_fw_beacon_material(&ctx, &unauth, deployed.item_id, mat_id, 1)
        .await
        .is_err());
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        contribute_fw_beacon_material(&ctx, &no_ship, deployed.item_id, mat_id, 1)
            .await
            .unwrap_err()
            .code,
        "CustomError"
    );
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        contribute_fw_beacon_material(&ctx, &no_sys, deployed.item_id, mat_id, 1)
            .await
            .unwrap_err()
            .code,
        "CustomError"
    );
    assert_eq!(
        contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, wrong_flag_mat, 1)
            .await
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_099);
    });
    assert_eq!(
        contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, mat_id, 1)
            .await
            .unwrap_err()
            .code,
        "FactionShipcasterAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(ctor_faction);
    });

    // Partial contribution (1 unit) -> !newly_built, decrements mat_id
    let partial = contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, mat_id, 1)
        .await
        .expect("partial contribution");
    assert_eq!(partial.contributed_materials.get(&mat_tid), Some(&1));

    // Complete mat_tid via remaining required qty -> then contributing same mat_tid again -> MaterialRequirementAlreadyMet
    let rem = mat_spec.required_qty.saturating_sub(1);
    if rem > 0 {
        let _ = contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, mat_id, rem)
            .await
            .expect("finish mat_tid");
    }
    if ctx.db.with_snapshot(|s| s.items.contains_key(&mat_id))
        && with_misc_state(&ctx, |st| {
            st.landing_pads
                .iter()
                .find(|p| p.item_id == deployed.item_id)
                .is_some_and(|p| !p.is_built)
        })
    {
        assert_eq!(
            contribute_fw_beacon_material(&ctx, &sess, deployed.item_id, mat_id, 1)
                .await
                .unwrap_err()
                .code,
            "MaterialRequirementAlreadyMet"
        );
    }

    // deposit_items_from_ship with no ship, non-integer keys, zero qty, and missing material -> failed_to_move
    assert_eq!(
        deposit_items_from_ship(&ctx, &no_ship, deployed.item_id, None)
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    let dep = deposit_items_from_ship(
        &ctx,
        &sess,
        deployed.item_id,
        Some(&PyRep::Dict(vec![
            (PyRep::String("bad".into()), PyRep::Integer(5)),
            (PyRep::Integer(34), PyRep::Integer(0)),
            (PyRep::Integer(999_999), PyRep::Integer(3)),
        ])),
    )
    .await
    .unwrap();
    let PyRep::Tuple(dep_t) = dep else { panic!() };
    assert_eq!(dep_t[0], PyRep::Bool(false));
    assert!(matches!(&dep_t[1], PyRep::Dict(v) if v.len() == 1));

    // link_fw_landing_pad guards: unbuilt pad -> LandingPadUnavailable; unknown faction -> LauncherNotFound
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: 995_300_001,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: lowsec_sys,
            faction_id: 500_001,
            is_built: false,
            link_timestamp: 0,
            is_disrupted: false,
            position: DVec3::ZERO,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: 995_300_002,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: lowsec_sys,
            faction_id: 500_099,
            is_built: true,
            link_timestamp: 0,
            is_disrupted: false,
            position: DVec3::ZERO,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
    });
    assert_eq!(
        link_fw_landing_pad(&ctx, 995_300_001).unwrap_err().code,
        "LandingPadUnavailable"
    );
    assert_eq!(
        link_fw_landing_pad(&ctx, 995_300_002).unwrap_err().code,
        "LauncherNotFound"
    );
    assert_eq!(
        disrupt_or_destroy_fw_landing_pad(&ctx, -888_888, false)
            .await
            .unwrap_err()
            .code,
        "LandingPadNotFound"
    );
    assert!(active_target_pad_for_faction(&ctx, 500_099).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_and_interstellar_shipcaster_all_guards_and_full_jump_execution() {
    let ctx = test_ctx();
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 996_100_001;
    let origin_sys: u32 = 30_000_142;
    let dest_sys: u32 = 30_000_144;
    let zarzakh_sys: u32 = 30_100_000;
    let mut sess = auth_session(1, char_id, 98_000_001, origin_sys);
    sess.ship_id = Some(ship_id);

    // GetFactionLandingPads negative arg
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(-5)]
        )
        .await
        .code,
        "InvalidArguments"
    );

    // No ship / no system guards on both jump RPCs
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        call_err(
            &ctx,
            &mut no_ship,
            "starterShipcasterTravelMgr",
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(1)]
        )
        .await
        .code,
        "CustomError"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut no_ship,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1)]
        )
        .await
        .code,
        "CustomError"
    );
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        call_err(
            &ctx,
            &mut no_sys,
            "starterShipcasterTravelMgr",
            "CmdJumpThroughStarterShipcaster",
            &[PyRep::Integer(1)]
        )
        .await
        .code,
        "CustomError"
    );
    assert_eq!(
        call_err(
            &ctx,
            &mut no_sys,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1)]
        )
        .await
        .code,
        "CustomError"
    );

    let starter_id = 776_001_i64;
    let emp_launcher_id = 886_001_i64;
    let emp_remote_launcher_id = 886_004_i64;
    let zarzakh_inbound_launcher = 886_002_i64;
    let zarzakh_outbound_launcher = 886_003_i64;
    let emp_pad_id = 886_101_i64;
    let zarzakh_pad_id = 886_102_i64;
    let fob_pad_id = 886_103_i64;

    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.clear();
        st.launchers.retain(|l| l.faction_id != 500_001);
        st.starter_launchers.push(StarterLauncherRecord {
            item_id: starter_id,
            type_id: 93968,
            solar_system_id: origin_sys,
            faction_id: 500_001,
            staging_system_id: dest_sys,
            staging_station_id: 60_003_760,
            position: DVec3::ZERO,
        });
        st.launchers.push(LauncherRecord {
            item_id: emp_launcher_id,
            type_id: 76465,
            solar_system_id: origin_sys,
            faction_id: 500_001,
            linked_pad_ids: vec![emp_pad_id],
            position: DVec3::ZERO,
        });
        st.launchers.push(LauncherRecord {
            item_id: emp_remote_launcher_id,
            type_id: 76465,
            solar_system_id: dest_sys,
            faction_id: 500_001,
            linked_pad_ids: vec![emp_pad_id],
            position: DVec3::ZERO,
        });
        st.launchers.push(LauncherRecord {
            item_id: zarzakh_inbound_launcher,
            type_id: 79018,
            solar_system_id: origin_sys,
            faction_id: 0,
            linked_pad_ids: vec![zarzakh_pad_id],
            position: DVec3::ZERO,
        });
        st.launchers.push(LauncherRecord {
            item_id: zarzakh_outbound_launcher,
            type_id: 79018,
            solar_system_id: zarzakh_sys,
            faction_id: 0,
            linked_pad_ids: vec![fob_pad_id],
            position: DVec3::ZERO,
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: emp_pad_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: dest_sys,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: 100,
            is_disrupted: false,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: zarzakh_pad_id,
            type_id: 79019,
            built_type_id: 79019,
            solar_system_id: zarzakh_sys,
            faction_id: 0,
            is_built: true,
            link_timestamp: 100,
            is_disrupted: false,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: fob_pad_id,
            type_id: 79019,
            built_type_id: 79019,
            solar_system_id: dest_sys,
            faction_id: 500_010,
            is_built: true,
            link_timestamp: 100,
            is_disrupted: false,
            position: DVec3::new(15_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
    });

    let now = ctx.now_unix();
    let freighter_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 513)
        .map(|t| t.type_id)
        .unwrap_or(20185);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now.saturating_sub(STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS + 10);
            ch.active_ship_id = ship_id;
            ch.solar_system_id = origin_sys;
            ch.war_faction_id = Some(500_001);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: freighter_tid,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    spawn_ball(&ctx, origin_sys, ship_id, freighter_tid, DVec3::new(50_000.0, 0.0, 0.0)).await;
    spawn_ball(&ctx, origin_sys, starter_id, 93968, DVec3::ZERO).await;
    spawn_ball(&ctx, origin_sys, emp_launcher_id, 76465, DVec3::ZERO).await;
    spawn_ball(&ctx, origin_sys, zarzakh_inbound_launcher, 79018, DVec3::ZERO).await;

    // Starter shipcaster: ineligible (>30d), then eligible but out-of-range
    assert!(!call_err(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(dest_sys as i64)] // hits staging_system_id fallback lookup!
    )
    .await
    .code
    .is_empty());
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().create_date_unix = now.saturating_sub(60);
    });
    assert!(!call_err(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_id)]
    )
    .await
    .code
    .is_empty());

    // Interstellar shipcaster guards:
    // 1. LandingPadNotFound
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(999_999)]
        )
        .await
        .code,
        "LandingPadNotFound"
    );
    // 2. CapitalShipCannotJumpShipcaster (Freighter)
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "CapitalShipCannotJumpShipcaster"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
    });
    // 3. EmpireMilitiaRefused when jumping into Zarzakh while enlisted in Empire militia
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_inbound_launcher), PyRep::Integer(zarzakh_sys as i64), PyRep::Integer(zarzakh_pad_id)]
        )
        .await
        .code,
        "EmpireMilitiaRefused"
    );
    // 4. FactionShipcasterAccessDenied when Empire militia mismatches launcher faction
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_002);
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "FactionShipcasterAccessDenied"
    );
    // 5. HorizonSiegeActive when jumping into Zarzakh during siege
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = None;
        snap.service_state_blobs
            .insert("zarzakh_horizon_siege_active".into(), "true".into());
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_inbound_launcher), PyRep::Integer(zarzakh_sys as i64), PyRep::Integer(zarzakh_pad_id)]
        )
        .await
        .code,
        "HorizonSiegeActive"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove("zarzakh_horizon_siege_active");
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
    });

    // 6. InvalidSolarSystem (launcher in different system), LandingPadUnavailable, LandingPadNotLinked, InvalidDestinationSystem
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_remote_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "InvalidSolarSystem"
    );
    with_misc_state(&ctx, |st| {
        st.landing_pads.iter_mut().find(|p| p.item_id == emp_pad_id).unwrap().is_disrupted = true;
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "LandingPadUnavailable"
    );
    with_misc_state(&ctx, |st| {
        st.landing_pads.iter_mut().find(|p| p.item_id == emp_pad_id).unwrap().is_disrupted = false;
        st.landing_pads.iter_mut().find(|p| p.item_id == emp_pad_id).unwrap().link_timestamp = 0;
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "LandingPadNotLinked"
    );
    with_misc_state(&ctx, |st| {
        st.landing_pads.iter_mut().find(|p| p.item_id == emp_pad_id).unwrap().link_timestamp = 100;
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(origin_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "InvalidDestinationSystem"
    );

    // 7. WeaponsTimerActive & proximity failure
    {
        ctx.crimewatch
            .write()
            .unwrap()
            .get_or_create(char_id)
            .weapon_timer_until_unix = now + 60;
    }
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
        )
        .await
        .code,
        "WeaponsTimerActive"
    );
    ctx.advance_secs(120);
    assert!(call_err(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)]
    )
    .await
    .code
    .len() > 0);

    // 8. Move ship into proximity -> FULL successful Empire CmdJumpThroughShipcaster to dest_sys
    let _ = ctx
        .systems
        .get_or_spawn(origin_sys)
        .set_ball_position(ship_id, DVec3::new(100.0, 0.0, 0.0))
        .await;
    let kw = HashMap::new();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(emp_launcher_id), PyRep::Integer(dest_sys as i64), PyRep::Integer(emp_pad_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    // 9. InsurgencyWarzoneNotConnected & full Pirate FOB jump from Zarzakh
    ctx.advance_secs(15);
    sess.solar_system_id = Some(zarzakh_sys);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = zarzakh_sys;
        ch.war_faction_id = Some(500_010);
    });
    spawn_ball(&ctx, zarzakh_sys, zarzakh_outbound_launcher, 79018, DVec3::ZERO).await;
    spawn_ball(&ctx, zarzakh_sys, ship_id, 587, DVec3::new(100.0, 0.0, 0.0)).await;
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.insert(
            999,
            InsurgencyCampaign {
                campaign_id: 999,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: 30_002_537,
                systems: vec![30_002_537],
                created_at_unix: now,
                forecasting_end_unix: now,
                active_end_unix: ctx.now_unix() + 1800,
                winddown_end_unix: ctx.now_unix() + 3600,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    assert_eq!(
        call_err(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_outbound_launcher), PyRep::Integer(dest_sys as i64), PyRep::Integer(fob_pad_id)]
        )
        .await
        .code,
        "InsurgencyWarzoneNotConnected"
    );
    // Include dest_sys in active campaign -> Pirate FOB jump succeeds & applies 120s target cooldown
    with_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.get_mut(&999).unwrap().systems.push(dest_sys);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(zarzakh_outbound_launcher), PyRep::Integer(dest_sys as i64), PyRep::Integer(fob_pad_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(with_misc_state(&ctx, |st| {
        st.target_cooldowns.get(&fob_pad_id).copied().unwrap_or(0) > 0
    }));
}
