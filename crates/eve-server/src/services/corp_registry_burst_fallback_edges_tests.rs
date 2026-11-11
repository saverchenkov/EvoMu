#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use eve_db::{
    ContractRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag,
    InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;

fn make_fleet_session(char_id: i64, ship_id: i64, sys_id: u32, fleet_id: i64) -> SessionState {
    SessionState {
        session_id: 77_001,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        fleet_id: Some(fleet_id),
        ..Default::default()
    }
}

fn find_burst_module_type(ctx: &ServerContext) -> u32 {
    ctx.sde
        .types
        .values()
        .find(|t| t.group_id == 1770)
        .map(|t| t.type_id)
        .unwrap_or(42_990)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burst_fallback_routing_and_validation_errors() {
    let ctx = test_ctx();
    let burst_tid = find_burst_module_type(&ctx);
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 1_880_000_100_i64;
    let mod_id = 1_880_000_200_i64;
    let fleet_id = 500_001_i64;

    let sess = make_fleet_session(char_id, ship_id, sys_id, fleet_id);

    // 1. Non-dogmaIM service, non-Activate/Deactivate method, no fleet_id, invalid args -> None
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "account", "GetCashBalance", &[PyRep::Integer(mod_id)]).await.is_none());
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(mod_id)]).await.is_none());
    let mut no_fleet = sess.clone();
    no_fleet.fleet_id = None;
    assert!(handle_fleet_burst_fallback(&ctx, &no_fleet, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.is_none());
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[]).await.is_none());
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(0)]).await.is_none());

    // 2. Unauthenticated session -> Unauthorized
    let mut unauth = sess.clone();
    unauth.character_id = None;
    unauth.user_id = None;
    let err_unauth = handle_fleet_burst_fallback(&ctx, &unauth, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_unauth.code, "Unauthorized");

    // 3. Defers (None) when ship_id is an Upwell structure or has full ship fitting in snap.items
    let struct_id = 1_880_000_300_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_id,
                ..Default::default()
            },
        );
        // Full hull item with a valid ship type (e.g. 587 Rifter) triggers get_or_rebuild_ship_fitting == Ok
        snap.items.insert(
            1_880_000_301,
            InventoryItemRecord {
                item_id: 1_880_000_301,
                type_id: 587,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: burst_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_880_000_201,
            InventoryItemRecord {
                item_id: 1_880_000_201,
                type_id: 34, // Tritanium, not group 1770
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut struct_sess = sess.clone();
    struct_sess.ship_id = Some(struct_id);
    assert!(handle_fleet_burst_fallback(&ctx, &struct_sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.is_none());

    let mut fitted_sess = sess.clone();
    fitted_sess.ship_id = Some(1_880_000_301);
    assert!(handle_fleet_burst_fallback(&ctx, &fitted_sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.is_none());

    // 4. Non-burst module or missing module -> None
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(9_999_999)]).await.is_none());
    assert!(handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(1_880_000_201)]).await.is_none());

    // 5. Docked in station or structure -> ShipDocked
    let mut docked_st = sess.clone();
    docked_st.station_id = Some(60_003_760);
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &docked_st, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ShipDocked"
    );
    let mut docked_up = sess.clone();
    docked_up.structure_id = Some(struct_id);
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &docked_up, "dogmaIM", "Deactivate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ShipDocked"
    );

    // 6. NoActiveShip & NoSolarSystem
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &no_ship, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "NoActiveShip"
    );
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &no_sys, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "NoSolarSystem"
    );

    // 7. ShipOwnershipViolation when ship item exists in snap.items with different owner (non-ship type so get_or_rebuild_ship_fitting fails)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 34,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ShipOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ship_id);
    });

    // 8. ModuleNotOwnedByShip (wrong location_id or wrong owner_id)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().location_id = 60_003_760;
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ModuleNotOwnedByShip"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let m = snap.items.get_mut(&mod_id).unwrap();
        m.location_id = ship_id;
        m.owner_id = 90_000_002;
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ModuleNotOwnedByShip"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().owner_id = char_id;
    });

    // 9. ModuleNotFitted (wrong flag or locked in contract)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::Cargo;
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ModuleNotFitted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::HiSlot0;
        snap.contracts.insert(
            77_100,
            ContractRecord {
                contract_id: 77_100,
                status: "Outstanding".into(),
                item_ids: vec![mod_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ModuleNotFitted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.remove(&77_100);
    });

    // 10. ModuleOffline for Activate when module_online_states has false
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, false);
    });
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ModuleOffline"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });

    // 11. ShipNotInSpace when ball is not spawned
    assert_eq!(
        handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)]).await.unwrap().unwrap_err().code,
        "ShipNotInSpace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burst_fallback_activate_aoe_range_charge_and_deactivate() {
    let ctx = test_ctx();
    let burst_tid = find_burst_module_type(&ctx);
    let sys_id = 30_000_142_u32;
    let booster_cid = 90_000_001_i64;
    let near_cid = 90_000_101_i64;
    let far_cid = 90_000_102_i64;
    let unspawned_cid = 90_000_103_i64;
    let docked_cid = 90_000_104_i64;

    let booster_ship = 1_880_001_100_i64;
    let near_ship = 1_880_001_101_i64;
    let far_ship = 1_880_001_102_i64;
    let unspawned_ship = 1_880_001_103_i64;
    let docked_ship = 1_880_001_104_i64;

    let mod_id = 1_880_001_200_i64;
    let charge_id = 1_880_001_201_i64;
    let charge_tid = 42_832_u32; // Shield Harmonizing Charge (or any real charge type)
    let fleet_id = 500_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&booster_cid).unwrap().clone();
        for (cid, sid, docked) in [
            (near_cid, near_ship, false),
            (far_cid, far_ship, false),
            (unspawned_cid, unspawned_ship, false),
            (docked_cid, docked_ship, true),
        ] {
            let mut c = base_ch.clone();
            c.character_id = cid;
            c.solar_system_id = sys_id;
            c.active_ship_id = sid;
            c.station_id = if docked { Some(60_003_760) } else { None };
            c.structure_id = None;
            snap.characters.insert(cid, c);
        }

        let mut fl = FleetRecord {
            fleet_id,
            boss_id: booster_cid,
            members: vec![booster_cid, near_cid, far_cid, unspawned_cid, docked_cid],
            ..Default::default()
        };
        fl.member_details.insert(
            booster_cid,
            FleetMemberRecord {
                character_id: booster_cid,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            },
        );
        for cid in [near_cid, far_cid, unspawned_cid, docked_cid] {
            fl.member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 0,
                },
            );
        }
        snap.fleets.insert(fleet_id, fl);

        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: burst_tid,
                owner_id: booster_cid,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos) in [
        (booster_ship, DVec3::new(0.0, 0.0, 0.0)),
        (near_ship, DVec3::new(5_000.0, 0.0, 0.0)),
        (far_ship, DVec3::new(200_000.0, 0.0, 0.0)),
    ] {
        let ball = Ball {
            id: sid,
            type_id: 587,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        sys.spawn_ball(ball).await.unwrap();
    }

    let sess = make_fleet_session(booster_cid, booster_ship, sys_id, fleet_id);

    // Activate WITHOUT charge
    let rep_no_charge = handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rep_no_charge.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(rep_no_charge.dict_get("chargeTypeID"), Some(&PyRep::None));
    let affected = rep_no_charge.dict_get("affectedMemberIDs").unwrap();
    if let PyRep::List(ids) = affected {
        assert!(ids.contains(&PyRep::Integer(booster_cid)));
        assert!(ids.contains(&PyRep::Integer(near_cid)));
        assert!(!ids.contains(&PyRep::Integer(far_cid)));
        assert!(!ids.contains(&PyRep::Integer(unspawned_cid)));
        assert!(!ids.contains(&PyRep::Integer(docked_cid)));
    } else {
        panic!("expected affectedMemberIDs list");
    }

    // Add loaded charge in same flag slot and Activate WITH charge
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: charge_tid,
                owner_id: booster_cid,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let rep_with_charge = handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rep_with_charge.dict_get("chargeTypeID"),
        Some(&PyRep::Integer(i64::from(charge_tid)))
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.module_active_states.contains(&mod_id));
        assert_eq!(snap.module_active_charges.get(&mod_id), Some(&charge_tid));
    });

    // Deactivate clears active state & active charge
    let rep_deact = handle_fleet_burst_fallback(&ctx, &sess, "dogmaIM", "Deactivate", &[PyRep::Integer(mod_id)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rep_deact.dict_get("active"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_charges.contains_key(&mod_id));
    });
}
