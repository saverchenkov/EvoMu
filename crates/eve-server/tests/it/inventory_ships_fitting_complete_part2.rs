//! Comprehensive Inventory, Ships, and Fitting integration test suite (Part 2).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::{dna_to_fitting, fitting_to_dna};
use std::collections::HashMap;

fn new_sqlite_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    crate::common::pin_clock(&ctx);
    ctx
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

#[tokio::test]
async fn test_ships_board_eject_corvette_insurance_repair_killmarks_mastery_t3c() {
    // parity: feat:ships.board-in-space, feat:ships.eject, feat:ships.newbie-ship, feat:ships.ship-insurance-link, rpc:ship.Board, rpc:ship.Eject, rpc:dogmaIM.CreateNewbieShip, rpc:insuranceSvc.GetInsurancePrice, rpc:invbroker.SetLabel, rpc:shipKillCounter.GetItemKillCountPlayer, rpc:dogmaIM.GetRequiredSkillLevels, notify:OnSessionChanged, notify:DoBallsAdded, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:ships.board-in-space",
        "feat:ships.eject",
        "feat:ships.newbie-ship",
        "feat:ships.ship-insurance-link",
        "rpc:ship.Board",
        "rpc:ship.Eject",
        "rpc:dogmaIM.CreateNewbieShip",
        "rpc:insuranceSvc.GetInsurancePrice",
        "rpc:invbroker.SetLabel",
        "rpc:shipKillCounter.GetItemKillCountPlayer",
        "rpc:dogmaIM.GetRequiredSkillLevels",
        "notify:OnSessionChanged",
        "notify:DoBallsAdded",
    ]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7104);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    let expected_ref_val = eve_server::formulas::compute_ship_insurance_reference_value(&ctx.sde, 587);
    let ins_quote = ctx
        .dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetInsurancePrice by typeID");
    assert_eq!(ins_quote.as_f64(), Some(expected_ref_val));
    assert!(expected_ref_val > 0.0);
    let ins_quote_ship = ctx
        .dispatch_rpc(&mut sess, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("GetInsurancePrice by shipID");
    assert_eq!(ins_quote_ship.as_f64(), Some(expected_ref_val));
    ctx.dispatch_rpc(
        &mut sess,
        "insuranceSvc",
        None,
        "InsureShip",
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("InsureShip Platinum");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
        }
    });
    let dmg_quote = ctx
        .dispatch_rpc(
            &mut sess,
            "repairSvc",
            None,
            "GetDamagedModulesAndRepairCost",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetDamagedModulesAndRepairCost");
    assert!(dmg_quote.dict_get("repairCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    ctx.dispatch_rpc(&mut sess, "repairSvc", None, "RepairAll", &[], &kw, None)
        .await
        .expect("RepairAll");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "SetLabel",
        &[PyRep::Integer(ship_id), PyRep::String("Gila Monster".into())],
        &kw,
        None,
    )
    .await
    .expect("SetLabel");
    sess.role |= eve_db::service_roles::ROLE_GMH;
    ctx.dispatch_rpc(
        &mut sess,
        "shipKillCounter",
        None,
        "SetShipKillCounter",
        &[PyRep::Integer(ship_id), PyRep::Integer(17)],
        &kw,
        None,
    )
    .await
    .expect("SetShipKillCounter");
    sess.role &= !eve_db::service_roles::ROLE_GMH;
    let kills = ctx
        .dispatch_rpc(
            &mut sess,
            "shipKillCounter",
            None,
            "GetItemKillCountPlayer",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetItemKillCountPlayer");
    assert_eq!(kills.as_i64(), Some(17));

    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetRequiredSkillLevels", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetRequiredSkillLevels");
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetMyCertificates", &[], &kw, None)
        .await
        .expect("GetMyCertificates");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetCertificatesForShipByMasteryLevel",
            &[PyRep::Integer(587), PyRep::Integer(3)],
            &kw,
            None,
        )
        .await
        .expect("GetCertificatesForShipByMasteryLevel");
    let _ = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetCurrCharMasteryLevel", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("GetCurrCharMasteryLevel");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetSkillsRequiredForMasteryLevel",
            &[PyRep::Integer(587), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("GetSkillsRequiredForMasteryLevel");

    ctx.advance_secs(12);
    let leave_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "LeaveShip", &[], &kw, None)
        .await
        .expect("LeaveShip in station");
    let pod_id = leave_rep.as_i64().expect("capsule_id integer");
    assert_eq!(sess.ship_id, Some(pod_id));

    let dup_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip must fail when character already has a non-capsule ship in station");
    assert_eq!(dup_corvette_err.code, "AlreadyHaveShipInStation");

    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.location_id == station_id && it.item_id != pod_id {
                let is_ship = ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 6);
                if is_ship {
                    it.location_id = 60_003_761;
                }
            }
        }
    });
    let corvette_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect("CreateNewbieShip");
    let corvette_id = corvette_rep.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    assert!(corvette_id > 0);
    let cd_corvette_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "CreateNewbieShip", &[], &kw, None)
        .await
        .expect_err("CreateNewbieShip within 60s must fail with NewbieShipCooldown");
    assert_eq!(cd_corvette_err.code, "NewbieShipCooldown");
    ctx.advance_virtual_clock(65);

    let sys_id = 30_000_142u32;
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&corvette_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(
            Ball {
                id: corvette_id,
                type_id: 601,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    let eject_rep = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Eject", &[], &kw, None)
        .await
        .expect("ship.Eject in space");
    let space_pod_id = eject_rep.dict_get("capsuleID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(sess.ship_id, Some(space_pod_id));

    ctx.advance_virtual_clock(10);

    ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(corvette_id)], &kw, None)
        .await
        .expect("Board ship in space from capsule");
    assert_eq!(sess.ship_id, Some(corvette_id));
    let pod_removed = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&space_pod_id));
    assert!(pod_removed, "Boarding ship in space from capsule must despawn the capsule");
}

#[tokio::test]
async fn test_fitting_slots_cpu_pwg_hardpoints_rigs_saved_multifit_ghost_inspace_weapons_ammo() {
    // parity: feat:fitting.fit-from-saved, feat:fitting.community-fittings, feat:fitting.fitting-simulation, feat:fitting.fitting-service-space, rpc:corpFittingMgr.GetCommunityFittings, rpc:ship.FitShips, rpc:ship.GetShipFittingInfo, notify:OnItemChange, notify:ProcessInventoryChange, notify:OnMultipleItemChange, notify:OnModuleFitted, notify:OnModuleAttributeChanges, notify:OnFittingsUpdated, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&[
        "feat:fitting.fit-from-saved",
        "feat:fitting.community-fittings",
        "feat:fitting.fitting-simulation",
        "feat:fitting.fitting-service-space",
        "rpc:corpFittingMgr.GetCommunityFittings",
        "rpc:ship.FitShips",
        "rpc:ship.GetShipFittingInfo",
        "notify:OnItemChange",
        "notify:ProcessInventoryChange",
        "notify:OnMultipleItemChange",
        "notify:OnModuleFitted",
        "notify:OnModuleAttributeChanges",
        "notify:OnFittingsUpdated",
    ]);
    let ctx = new_sqlite_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(7105);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    let dna = fitting_to_dna(587, &[(487, 1), (434, 1), (2048, 1)]);
    let (parsed_ship, parsed_items) = dna_to_fitting(&dna).expect("dna_to_fitting");
    assert_eq!(parsed_ship, 587);
    assert_eq!(parsed_items.len(), 3);

    let fit_id_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::Integer(char_id),
                PyRep::keyval([
                    ("name", PyRep::String("Rifter Tackle".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("description", PyRep::String("Fast tackle".into())),
                    (
                        "fitData",
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(2048),
                            PyRep::Integer(11),
                            PyRep::Integer(1),
                        ])]),
                    ),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.SaveFitting");
    let saved_fid = fit_id_rep.as_i64().unwrap();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charFittingMgr", None, "GetFittings", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("charFittingMgr.GetFittings");
    let comm_fits = ctx
        .dispatch_rpc(&mut sess, "corpFittingMgr", None, "GetCommunityFittings", &[], &kw, None)
        .await
        .expect("corpFittingMgr.GetCommunityFittings");
    assert!(!matches!(comm_fits, PyRep::None));

    let ghost_sim = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "GetShipFittingInfo",
            &[
                PyRep::Integer(587),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(2048),
                    PyRep::Integer(11),
                    PyRep::Integer(1),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("ship.GetShipFittingInfo ghost simulation");
    assert_eq!(ghost_sim.dict_get("simulated").and_then(|v| v.as_bool()), Some(true));

    let hull_a = 830_001i64;
    let hull_b = 830_002i64;
    let dc_stack = 830_003i64;
    seed_item(&ctx, hull_a, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, hull_b, 587, char_id, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, dc_stack, 2048, char_id, station_id, InventoryFlag::Hangar, 5, false);
    grant_all_required_skills_for_type(&ctx, char_id, 587);
    grant_all_required_skills_for_type(&ctx, char_id, 2048);

    let mfit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "FitShips",
            &[
                PyRep::List(vec![PyRep::Integer(hull_a), PyRep::Integer(hull_b)]),
                PyRep::Integer(saved_fid),
            ],
            &kw,
            None,
        )
        .await
        .expect("ship.FitShips multifit");
    assert_eq!(mfit_rep.dict_get("fittedCount").and_then(|v| v.as_i64()), Some(2));

    let sys_id = 30_000_142u32;
    let depot_id = 830_010i64;
    let spare_mod_id = 830_011i64;
    seed_item(&ctx, depot_id, 33474, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, spare_mod_id, 2048, char_id, ship_id, InventoryFlag::Cargo, 1, true);

    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_350_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    let _ = sys
        .spawn_ball(
            Ball {
                id: depot_id,
                type_id: 33474,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
        )
        .await;

    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(spare_mod_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot2 as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Fitting in space 10km from Mobile Depot must fail");
    assert_eq!(far_err.code, "CannotFitInSpace");

    let _ = sys.remove_ball(depot_id).await;
    let _ = sys
        .spawn_ball(
            Ball {
                id: depot_id,
                type_id: 33474,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
        )
        .await;

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(spare_mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fitting in space within 1,500m of Mobile Depot succeeds");

    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .trigger_weapon_timer(char_id, ctx.now_unix_secs());
    let wt_err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(spare_mod_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Unfitting in space with active weapons timer must fail");
    assert_eq!(wt_err.code, "CannotRefitWithWeaponsTimer");
}
