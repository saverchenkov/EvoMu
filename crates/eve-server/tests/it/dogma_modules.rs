//! Phase 4 Dogma and Fitting integration tests (`DG-2`..`DG-6` and Phase 4 Exit Gate).


use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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
async fn test_phase_4_dg2_fit_validation_and_rigs() {
    // parity: plan:DG-2, feat:fitting.fit-module, feat:fitting.unfit-module, feat:fitting.fit-rig, feat:fitting.fit-subsystems, feat:fitting.max-group-fitted, feat:fitting.max-type-fitted, feat:fitting.fittings-save, notify:OnModuleFitted, notify:OnModuleUnfitted, notify:OnModuleAttributeChanges, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:fitting.fit-module", "feat:fitting.unfit-module", "feat:fitting.fit-rig", "feat:fitting.fit-subsystems", "feat:fitting.max-group-fitted", "feat:fitting.max-type-fitted", "feat:fitting.fittings-save", "notify:OnModuleFitted", "notify:OnModuleUnfitted", "notify:OnModuleAttributeChanges"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Grant skills for modules and T3 subsystems we will test
    for tid in [
        438u32, 439, 2048, 2889, 30987, 31055, 47257, 42522, 10838, 29990, 45633, 45597, 45607,
        45620,
    ] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    // Train Armor Rigging (26253) to level 4 (-40% armor rig drawback)
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        entry.insert(
            26253,
            eve_db::TrainedSkillRecord {
                type_id: 26253,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 1. Slot type validation: 1MN Afterburner II (100_000_003, type 438, medPower) into LoSlot0 (11) must fail
    let wrong_slot = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        wrong_slot.expect_err("medPower module in LoSlot0 must fail").code,
        "FittingValidationFailed"
    );

    // 2. Fit 1MN Afterburner II (100_000_003) into MedSlot0 (19) -> succeeds & emits OnModuleFitted + OnModuleAttributeChanges
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_003),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 1MN Afterburner II into MedSlot0");
    let notes = sess.drain_notifications();
    assert!(
        notes.iter().any(|(n, _)| n == "OnModuleFitted"),
        "Expected OnModuleFitted notification"
    );
    assert!(
        notes.iter().any(|(n, _)| n == "OnModuleAttributeChanges"),
        "Expected OnModuleAttributeChanges notification"
    );

    // 3. Unfit 1MN Afterburner II (100_000_003): rejected in space without a Mobile Depot (CannotUnfitInSpace), succeeds when docked & emits OnModuleUnfitted
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock into space");
    let unfit_in_space = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        unfit_in_space
            .expect_err("Unfitting a module in space without a Mobile Depot must fail")
            .code,
        "CannotUnfitInSpace"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = station_id;
            ship_item.flag = InventoryFlag::Hangar;
        }
    });
    sess.station_id = Some(station_id);
    sess.solar_system_id = None;
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_003),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Unfit 1MN Afterburner II to hangar while docked");
    let notes = sess.drain_notifications();
    // The server sends `OnItemChange(item, {ixLocationID: oldLoc, ixFlag: oldFlag})`; the V24
    // client's `InventorySvc.OnItemChange` derives `OnModuleUnfitted` when ixFlag left a fitting
    // flag. The client-local name itself must never come from the server.
    let unfit_change = notes
        .iter()
        .find(|(n, a)| {
            n == "OnItemChange"
                && matches!(a.first(), Some(PyRep::KeyVal(kv))
                    if kv.get("itemID") == Some(&PyRep::Integer(100_000_003)))
        })
        .expect("OnItemChange for the unfitted module");
    let PyRep::KeyVal(row) = &unfit_change.1[0] else { unreachable!() };
    assert_eq!(row.get("locationID"), Some(&PyRep::Integer(station_id)));
    assert_eq!(row.get("flagID"), Some(&PyRep::Integer(InventoryFlag::Hangar as i64)));
    let Some(PyRep::Dict(change)) = unfit_change.1.get(1) else {
        panic!("OnItemChange change dict expected");
    };
    let old_flag = change
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(4))
        .and_then(|(_, v)| v.as_i64())
        .expect("ixFlag (4) in change dict");
    assert!((11..=34).contains(&old_flag), "old flag {old_flag} must be a fitting slot");
    assert!(change.iter().any(|(k, v)| *k == PyRep::Integer(3) && *v == PyRep::Integer(ship_id)));
    assert!(notes.iter().all(|(n, _)| n != "OnModuleUnfitted"));

    // 4. Ship restriction (canFitShipGroup): Assault Damage Control II (47257) cannot fit on Rifter (587)
    seed_item(&ctx, 100_000_801, 47257, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let ship_restr = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_801),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        ship_restr.expect_err("ADC II on Rifter must fail canFitShipGroup").code,
        "FittingValidationFailed"
    );

    // 5. maxGroupFitted (1544): Damage Control II (2048) has maxGroupFitted = 1
    seed_item(&ctx, 100_000_802, 2048, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_803, 2048, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_802),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("First Damage Control II must fit");
    let second_dc = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_803),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::LoSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await;
    let dc_err = second_dc.expect_err("Second Damage Control II must fail maxGroupFitted");
    assert_eq!(dc_err.code, "FittingValidationFailed");
    assert!(dc_err.message.contains("MaxGroupFittedExceeded"));

    // 6. maxTypeFitted (2431): Pulse Activated Nexus Invulnerability Core (42522, maxTypeFitted = 1) on Rorqual (28352)
    let rorqual_id = 100_000_810i64;
    seed_item(&ctx, rorqual_id, 28352, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_811, 42522, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_812, 42522, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = rorqual_id;
        }
    });
    sess.ship_id = Some(rorqual_id);
    let mut offline_kw = HashMap::new();
    offline_kw.insert("online".to_string(), PyRep::Bool(false));
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_811),
            PyRep::Integer(rorqual_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &offline_kw,
        None,
    )
    .await
    .expect("First PANIC module fits on Rorqual");
    let second_panic = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_812),
                PyRep::Integer(rorqual_id),
                PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            ],
            &offline_kw,
            None,
        )
        .await;
    let panic_err = second_panic.expect_err("Second PANIC module must fail maxTypeFitted");
    assert_eq!(panic_err.code, "FittingValidationFailed");
    assert!(panic_err.message.contains("MaxTypeFittedExceeded"));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
        }
    });
    sess.ship_id = Some(ship_id);

    // 7. Over-fit CPU and PG rejection: Large Shield Booster I (10838, CPU) and 10MN Afterburner II (12058, PG)
    grant_all_required_skills_for_type(&ctx, char_id, 12058);
    seed_item(&ctx, 100_000_804, 10838, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let overfit_cpu = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_804),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await;
    let overfit_cpu_err = overfit_cpu.expect_err("Large Shield Booster I must fail CPU check on Rifter");
    assert_eq!(overfit_cpu_err.code, "FittingValidationFailed");
    assert!(overfit_cpu_err.message.contains("Insufficient CPU"));

    seed_item(&ctx, 100_000_807, 12058, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let overfit_pg = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_807),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::MedSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await;
    let overfit_pg_err = overfit_pg.expect_err("10MN Afterburner II must fail PG check on Rifter");
    assert_eq!(overfit_pg_err.code, "FittingValidationFailed");
    assert!(overfit_pg_err.message.contains("Insufficient Powergrid"));

    // Unfit first Damage Control II and remove Hull Upgrades so base armor HP is exactly 450.0 for rig testing
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_802),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Unfit first Damage Control II");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(entry) = snap.skills.get_mut(&char_id) {
            entry.remove(&3394);
            entry.remove(&3327);
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 8. Rig fitting: size check, calibration deduction, Armor Rigging drawback reduction, and rig destruction
    seed_item(&ctx, 100_000_805, 31055, char_id, station_id, InventoryFlag::Hangar, 1, true); // Medium Trimark Armor Pump I (size 2)
    let wrong_rig_size = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_805),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::RigSlot0 as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(
        wrong_rig_size
            .expect_err("Medium rig on small ship must fail")
            .message
            .contains("Rig size mismatch")
    );

    seed_item(&ctx, 100_000_806, 30987, char_id, station_id, InventoryFlag::Hangar, 1, true); // Small Trimark Armor Pump I
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_806),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::RigSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Small Trimark Armor Pump I into RigSlot0");

    let fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("get_or_rebuild_ship_fitting");
    let armor_hp = fit.ship.get_attribute(attr_ids::ARMOR_HP, &ctx.sde);
    let cal_avail = fit.ship.get_attribute(attr_ids::UPGRADE_CAPACITY, &ctx.sde);
    let agility = fit.ship.get_attribute(attr_ids::AGILITY, &ctx.sde);
    assert!(
        (armor_hp - 517.5).abs() < 0.1,
        "Expected +15% armor HP (517.5), got {armor_hp}"
    );
    assert!(
        (cal_avail - 350.0).abs() < 0.1,
        "Expected 350.0 remaining calibration (400 - 50), got {cal_avail}"
    );
    // Base Rifter agility is 3.2; rig drawback is +10%, reduced by 40% (Armor Rigging IV) to +6% -> 3.2 * 1.06 = 3.392
    assert!(
        (agility - 3.392).abs() < 0.01,
        "Expected agility 3.392 with Armor Rigging IV drawback reduction, got {agility}"
    );

    // Unfitting a rig via MoveItem or DestroyFitting destroys the rig item rather than returning it to hangar
    let _ = sess.drain_notifications();
    let rig_unfit = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_806),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Unfitting rig via MoveItem destroys it");
    let PyRep::KeyVal(rig_unfit_kv) = rig_unfit else {
        panic!("Expected KeyVal from rig MoveItem unfit");
    };
    assert_eq!(rig_unfit_kv.get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&100_000_806)),
        "Unfitted rig must be destroyed and removed from inventory"
    );
    assert_rig_trashed_item_change(&sess.drain_notifications(), 100_000_806, ship_id);

    seed_item(&ctx, 100_000_808, 30987, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_808),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::RigSlot1 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit second Small Trimark Armor Pump I into RigSlot1");

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(100_000_808)], &kw, None).await
    .expect("DestroyFitting on rig");
    assert_rig_trashed_item_change(&sess.drain_notifications(), 100_000_808, ship_id);
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&100_000_808)),
        "Destroyed rig must be removed from inventory"
    );
    let fit_after = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("get_or_rebuild_ship_fitting");
    assert!(
        (fit_after.ship.get_attribute(attr_ids::ARMOR_HP, &ctx.sde) - 450.0).abs() < 0.1,
        "Armor HP must return to 450.0 after rig destruction"
    );

    // 9. T3 Strategic Cruiser subsystems (feat:fitting.fit-subsystems):
    // Loki (29990) starts with 0 hi/med/low slots; fitting Core (45633, slot 125), Defensive (45597, slot 126),
    // Offensive (45607, slot 127), and Propulsion (45620, slot 128) updates hull slot layout to 7H/5M/5L.
    let loki_id = 100_000_813i64;
    seed_item(&ctx, loki_id, 29990, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_814, 45633, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_815, 45597, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_816, 45607, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 100_000_817, 45620, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = loki_id;
        }
    });
    sess.ship_id = Some(loki_id);

    // Fitting Core subsystem (subSystemSlot = 125) into SubSystemSlot1 (126) must fail with SubsystemSlotMismatch
    let wrong_sub_slot = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_000_814),
                PyRep::Integer(loki_id),
                PyRep::Integer(InventoryFlag::SubSystemSlot1 as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(
        wrong_sub_slot
            .expect_err("Core subsystem in SubSystemSlot1 must fail")
            .message
            .contains("SubsystemSlotMismatch")
    );

    for (sub_item_id, flag) in [
        (100_000_814i64, InventoryFlag::SubSystemSlot0),
        (100_000_815, InventoryFlag::SubSystemSlot1),
        (100_000_816, InventoryFlag::SubSystemSlot2),
        (100_000_817, InventoryFlag::SubSystemSlot3),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(sub_item_id),
                PyRep::Integer(loki_id),
                PyRep::Integer(flag as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit T3 Loki subsystem");
    }

    let fit_loki = ctx
        .get_or_rebuild_ship_fitting(char_id, loki_id)
        .expect("get_or_rebuild_ship_fitting Loki");
    assert_eq!(fit_loki.ship.get_attribute(attr_ids::HI_SLOTS, &ctx.sde), 7.0);
    assert_eq!(fit_loki.ship.get_attribute(attr_ids::MED_SLOTS, &ctx.sde), 5.0);
    assert_eq!(fit_loki.ship.get_attribute(attr_ids::LOW_SLOTS, &ctx.sde), 5.0);
    assert_eq!(
        fit_loki.ship.get_attribute(attr_ids::TURRET_SLOTS_LEFT, &ctx.sde),
        5.0
    );
    assert_eq!(
        fit_loki.ship.get_attribute(attr_ids::LAUNCHER_SLOTS_LEFT, &ctx.sde),
        2.0
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
        }
    });
    sess.ship_id = Some(ship_id);

    // 10. Saved fittings CRUD via charFittingMgr & corpFittingMgr (feat:fitting.fittings-save)
    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("DG2 Rifter Brawl".into())),
        ("description", PyRep::String("Autocannon Rifter".into())),
        ("shipTypeID", PyRep::Integer(587)),
        (
            "fitData",
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(2889),
                    PyRep::Integer(InventoryFlag::HiSlot0 as i64),
                    PyRep::Integer(1),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(438),
                    PyRep::Integer(InventoryFlag::MedSlot0 as i64),
                    PyRep::Integer(1),
                ]),
            ]),
        ),
    ]);
    let saved_id = ctx.dispatch_rpc(&mut sess, "charFittingMgr", None, "SaveFitting", &[PyRep::Integer(char_id), fit_kv.clone()], &kw, None).await
        .expect("charFittingMgr.SaveFitting")
        .as_i64()
        .expect("saved fitting ID");
    assert!(saved_id > 0);
    let char_fits = ctx.dispatch_rpc(&mut sess, "charFittingMgr", None, "GetFittings", &[PyRep::Integer(char_id)], &kw, None).await
        .expect("charFittingMgr.GetFittings");
    assert!(
        matches!(char_fits, PyRep::Rowset { ref rows, .. } if !rows.is_empty()),
        "charFittingMgr.GetFittings must return saved fitting"
    );
    ctx.dispatch_rpc(&mut sess, "charFittingMgr", None, "DeleteFitting", &[PyRep::Integer(char_id), PyRep::Integer(saved_id)], &kw, None).await
    .expect("charFittingMgr.DeleteFitting");

    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let corp_fit_id = ctx.dispatch_rpc(&mut sess, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv], &kw, None).await
        .expect("corpFittingMgr.SaveFitting")
        .as_i64()
        .expect("corp fitting ID");
    ctx.dispatch_rpc(&mut sess, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(corp_fit_id)], &kw, None).await
    .expect("corpFittingMgr.DeleteFitting");
}

#[tokio::test]
async fn test_phase_4_dg3_online_offline_and_persistence() {
    // parity: plan:DG-3, feat:fitting.online-offline, rpc:dogmaIM.SetModuleOnline, rpc:dogmaIM.TakeModuleOffline, notify:OnModuleOnlineChange, notify:OnModuleAttributeChanges, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:DG-3", "feat:fitting.online-offline", "rpc:dogmaIM.SetModuleOnline", "rpc:dogmaIM.TakeModuleOffline", "notify:OnModuleOnlineChange", "notify:OnModuleAttributeChanges"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;
    for tid in [439u32, 444] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }

    // 1. Fit 1MN Afterburner I (439, cpu=15, pg=10) with online=False into MedSlot0 (19)
    let ab_id = 100_000_820i64;
    seed_item(&ctx, ab_id, 439, char_id, station_id, InventoryFlag::Hangar, 1, true);
    let mut offline_kw = HashMap::new();
    offline_kw.insert("online".to_string(), PyRep::Bool(false));
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(ab_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &offline_kw,
        None,
    )
    .await
    .expect("Fit 1MN Afterburner I offline");

    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&ab_id).copied()),
        Some(false),
        "Offline state must be persisted in module_online_states"
    );
    let fit_offline = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_offline");
    // Only the pre-seeded 200mm AutoCannon II (100_000_005, cpu=9, pg=4) is online
    let cpu_before = fit_offline.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde);
    let pg_before = fit_offline.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde);

    // 2. Call dogmaIM.SetModuleOnline -> updates CPU/PG, persists true, and emits OnModuleOnlineChange + OnModuleAttributeChanges
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(ab_id)], &kw, None).await
    .expect("SetModuleOnline");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&ab_id).copied()),
        Some(true)
    );
    let notes = sess.drain_notifications();
    // `OnModuleAttributeChanges([('OnModuleAttributeChange', ownerID, itemKey, attributeID, time,
    // newValue, oldValue)])`: the V24 client's godma `ApplyAttributeChange` derives
    // `OnModuleOnlineChange` from `attributeIsOnline` (2). The derived name is never sent.
    let mac = notes
        .iter()
        .find(|(n, _)| n == "OnModuleAttributeChanges")
        .expect("OnModuleAttributeChanges");
    let Some(PyRep::List(changes)) = mac.1.first() else {
        panic!("OnModuleAttributeChanges(changes) list expected, got {:?}", mac.1);
    };
    assert_eq!(changes.len(), 1);
    let PyRep::Tuple(t) = &changes[0] else { panic!("change tuple expected") };
    assert_eq!(t.len(), 7);
    assert_eq!(t[0], PyRep::String("OnModuleAttributeChange".into()));
    assert_eq!(t[1], PyRep::Integer(char_id));
    assert_eq!(t[2], PyRep::Integer(ab_id));
    assert_eq!(t[3], PyRep::Integer(2));
    assert!(matches!(t[4], PyRep::Integer(ft) if ft > 116_444_736_000_000_000));
    assert_eq!(t[5], PyRep::Real(1.0));
    assert_eq!(t[6], PyRep::Real(0.0));
    assert!(notes.iter().all(|(n, _)| n != "OnModuleOnlineChange"));

    // Rebuild from snapshot to prove persistence across cache invalidation
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let fit_online = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_online");
    let cpu_online = fit_online.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde);
    let pg_online = fit_online.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde);
    assert!(
        (cpu_online - (cpu_before + 15.0)).abs() < 0.1,
        "Expected +15 CPU load when online, got {cpu_before} -> {cpu_online}"
    );
    assert!(
        (pg_online - (pg_before + 10.0)).abs() < 0.1,
        "Expected +10 PG load when online, got {pg_before} -> {pg_online}"
    );

    // 3. Call dogmaIM.TakeModuleOffline -> drops CPU/PG load and persists false
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(ab_id)], &kw, None).await
    .expect("TakeModuleOffline");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&ab_id).copied()),
        Some(false)
    );
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let fit_back_offline = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("fit_back_offline");
    assert!(
        (fit_back_offline.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde) - cpu_before).abs() < 0.1
    );

    // 4. maxGroupOnline: fit a second 1MN Afterburner I (439) into MedSlot1 offline; only 1 can be online at a time
    seed_item(&ctx, 100_000_821, 439, char_id, station_id, InventoryFlag::Hangar, 1, true);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_821),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot1 as i64),
        ],
        &offline_kw,
        None,
    )
    .await
    .expect("Fit second 1MN Afterburner I offline");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(100_000_821)], &kw, None).await
    .expect("Online second 1MN Afterburner I");
    let second_online = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(ab_id)], &kw, None).await;
    assert!(
        second_online
            .expect_err("Second propulsion module online must fail maxGroupOnline")
            .message
            .contains("MaxGroupOnlineExceeded")
    );
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(100_000_821)], &kw, None).await
    .expect("Offline second 1MN Afterburner I before space test");

    // 5. In-space 75% capacitor check on SetModuleOnline
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 100.0).await.expect("set_capacitor 100.0");
    let low_cap_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(ab_id)], &kw, None).await;
    assert_eq!(
        low_cap_err.expect_err("SetModuleOnline below 75% cap in space must fail").code,
        "NotEnoughCapacitorToOnline"
    );

    sys.set_capacitor(ship_id, 250.0).await.expect("set_capacitor 250.0");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(ab_id)], &kw, None).await
    .expect("SetModuleOnline with full capacitor in space");
}

#[tokio::test]
async fn test_phase_4_dg5_targeting_lock_and_break() {
    // parity: plan:DG-5, feat:dogma.targeting-lock, feat:dogma.targeting-break, notify:OnTarget, notify:OnTargets, notify:OnTargetClear, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:dogma.targeting-lock", "feat:dogma.targeting-break", "notify:OnTarget", "notify:OnTargets", "notify:OnTargetClear"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001i64;
    grant_all_required_skills_for_type(&ctx, 90_000_001, 2889);
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = sys
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");

    let near_target_id = 990_000_001i64;
    let far_target_id = 990_000_002i64;
    sys.spawn_ball_with_combat(
        Ball {
            id: near_target_id,
            type_id: 587,
            position: DVec3::new(
                ship_ball.position.x + 4_000.0,
                ship_ball.position.y,
                ship_ball.position.z,
            ),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .expect("spawn near target");

    sys.spawn_ball_with_combat(
        Ball {
            id: far_target_id,
            type_id: 587,
            position: DVec3::new(
                ship_ball.position.x + 50_000.0,
                ship_ball.position.y,
                ship_ball.position.z,
            ),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.2,
            mass: 1_350_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .expect("spawn far target");

    // 1. Out-of-range target (50 km > Rifter maxTargetRange 22.5 km) must fail
    let far_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(far_target_id)], &kw, None).await;
    let far_err = far_res.expect_err("AddTarget beyond maxTargetRange must fail");
    assert_eq!(far_err.code, "TargetingFailed");
    assert!(far_err.message.contains("maxTargetRange"));

    // 2. Activating a targeted module before locking must fail with TargetNotLocked
    let unlocked_act = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(100_000_005),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(near_target_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        unlocked_act.expect_err("Turret activation without lock must fail").code,
        "TargetNotLocked"
    );

    // 3. Lock near target -> starts in Locking state with scanResolution/signatureRadius timer
    let _ = sess.drain_notifications();
    let expected_sec = eve_destiny::combat::compute_lock_time_seconds(660.0, 35.0);
    assert!(expected_sec > 1.0 && expected_sec < 5.0);
    let add_ret = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(near_target_id)], &kw, None).await
        .expect("AddTarget near_target_id");
    let PyRep::KeyVal(add_kv) = add_ret else {
        panic!("Expected KeyVal from AddTarget, got {add_ret:?}");
    };
    assert_eq!(add_kv.get("targetID"), Some(&PyRep::Integer(near_target_id)));
    assert_eq!(add_kv.get("locked"), Some(&PyRep::Bool(false)));
    let Some(PyRep::Real(lock_ms)) = add_kv.get("lockTimeMs") else {
        panic!("Expected lockTimeMs Real in AddTarget KeyVal");
    };
    assert!((lock_ms - expected_sec * 1000.0).abs() < 1.0);
    let notes = sess.drain_notifications();
    assert!(notes.iter().any(|(n, _)| n == "OnTarget"));
    // `OnMultiEvent([(('OnTarget', 'add', targetID, reason), groupKey)])`: the V24 client's
    // `Godma.OnMultiEvent` derives `OnTargets`; the derived name is never sent by the server.
    let multi = notes
        .iter()
        .find(|(n, _)| n == "OnMultiEvent")
        .expect("OnMultiEvent carrying OnTarget");
    let Some(PyRep::List(events)) = multi.1.first() else {
        panic!("OnMultiEvent(events) list expected");
    };
    let PyRep::Tuple(entry) = &events[0] else { panic!("(event, key) tuple expected") };
    assert_eq!(entry.len(), 2);
    assert_eq!(
        entry[0],
        PyRep::Tuple(vec![
            PyRep::String("OnTarget".into()),
            PyRep::String("add".into()),
            PyRep::Integer(near_target_id),
            PyRep::String(String::new()),
        ])
    );
    assert!(notes.iter().all(|(n, _)| n != "OnTargets"));

    let locks_before = sys.get_target_locks(ship_id).await.expect("get_target_locks");
    assert_eq!(locks_before.len(), 1);
    assert!(!locks_before[0].locked);

    // Step 4 seconds -> lock completes
    for _ in 0..4 {
        sys.step_manual(1.0).await.expect("sys.step_manual");
    }
    let locks_after = sys.get_target_locks(ship_id).await.expect("get_target_locks");
    assert!(locks_after[0].locked);

    // 4. RemoveTarget & ClearTargets
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(near_target_id)], &kw, None).await
    .expect("RemoveTarget");
    assert!(
        sess.drain_notifications().iter().any(|(n, _)| n == "OnTarget"),
        "Expected OnTarget lost notification"
    );
    assert!(sys.get_target_locks(ship_id).await.unwrap().is_empty());

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(near_target_id)], &kw, None).await
    .expect("Re-add target");
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "ClearTargets", &[], &kw, None)
        .await
        .expect("ClearTargets");
    assert!(
        sess.drain_notifications()
            .iter()
            .any(|(n, _)| n == "OnTargetClear"),
        "Expected OnTargetClear notification"
    );
    assert!(sys.get_target_locks(ship_id).await.unwrap().is_empty());

    // 5. Automatic lock break when target warps away or attacker enters warp (feat:dogma.targeting-break)
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(near_target_id)], &kw, None).await
    .expect("AddTarget before target warp break");
    for _ in 0..4 {
        sys.step_manual(1.0).await.expect("step lock");
    }
    assert!(sys.get_target_locks(ship_id).await.unwrap()[0].locked);
    sys.set_movement_mode(
        near_target_id,
        MovementMode::Warp {
            origin: DVec3::new(
                ship_ball.position.x + 4_000.0,
                ship_ball.position.y,
                ship_ball.position.z,
            ),
            destination: DVec3::new(
                ship_ball.position.x + 10_000_000.0,
                ship_ball.position.y,
                ship_ball.position.z,
            ),
            total_distance: 10_000_000.0,
            warp_speed_au_s: 3.0,
            phase: eve_destiny::WarpPhase::Accelerating,
            elapsed_warp_time: 0.0,
        },
    )
    .await
    .expect("warp target away");
    let step_warp = sys.step_manual(1.0).await.expect("step after target warp");
    assert!(
        sys.get_target_locks(ship_id).await.unwrap().is_empty(),
        "Target lock must automatically drop when target warps away"
    );
    assert!(
        step_warp.events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnTarget { mode, target_id, .. }
                if mode == "lost" && *target_id == near_target_id
        )),
        "Expected OnTarget lost event when target warps away"
    );
    sys.set_movement_mode(near_target_id, MovementMode::Stop)
        .await
        .expect("stop target");

    // Re-lock and verify entering warp clears all locks on the warping attacker ship
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(near_target_id)], &kw, None).await
    .expect("AddTarget before attacker warp break");
    assert_eq!(sys.get_target_locks(ship_id).await.unwrap().len(), 1);
    sys.set_movement_mode(
        ship_id,
        MovementMode::Warp {
            origin: ship_ball.position,
            destination: DVec3::new(
                ship_ball.position.x + 10_000_000.0,
                ship_ball.position.y,
                ship_ball.position.z,
            ),
            total_distance: 10_000_000.0,
            warp_speed_au_s: 3.0,
            phase: eve_destiny::WarpPhase::Accelerating,
            elapsed_warp_time: 0.0,
        },
    )
    .await
    .expect("warp_to attacker");
    assert!(
        sys.get_target_locks(ship_id).await.unwrap().is_empty(),
        "Target locks must drop immediately when ship enters warp"
    );
}


#[tokio::test]
async fn test_phase_4_modifier_engine_stacking_penalty_and_skills() {
    // parity: feat:dogma.modifier-engine, feat:fitting.stacking-penalty, feat:modules.nanofiber-overdrive, feat:skills.skill-bonus-application, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:dogma.modifier-engine", "feat:fitting.stacking-penalty", "feat:modules.nanofiber-overdrive", "feat:skills.skill-bonus-application"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4006);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Grant required skills for Overdrive Injector System II (1236)
    grant_all_required_skills_for_type(&ctx, char_id, 1236);

    // Set Navigation (3449) to level 5 (+25% sub-warp velocity) and clear other agility/speed skills
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        entry.insert(
            3449,
            eve_db::TrainedSkillRecord {
                type_id: 3449,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        entry.remove(&3327); // Spaceship Command
        entry.remove(&3447); // Evasive Maneuvering
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Seed and fit two Overdrive Injector System II modules in LoSlot0 and LoSlot1
    let od1_id = 100_000_901i64;
    let od2_id = 100_000_902i64;
    seed_item(&ctx, od1_id, 1236, char_id, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, od2_id, 1236, char_id, station_id, InventoryFlag::Hangar, 1, true);

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(od1_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit first Overdrive Injector System II");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(od2_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot1 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit second Overdrive Injector System II");

    let fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("get_or_rebuild_ship_fitting");

    // Rifter base max_velocity: 365.0.
    // Navigation V: +25% (PostPercent 5.0 * 5) -> factor 1.25.
    // Overdrive 1: +12.5% (unpenalized -> factor 1.125).
    // Overdrive 2: +12.5% * stacking_penalty_factor(1).
    let expected_vel = 365.0
        * 1.25
        * (1.0 + 0.125)
        * (1.0 + 0.125 * eve_dogma::stacking_penalty_factor(1));
    let actual_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
    assert!(
        (actual_vel - expected_vel).abs() < 1e-4,
        "Expected maxVelocity {expected_vel}, got {actual_vel}"
    );

    // Overdrive drawback: -20% cargo capacity each (PostMul 0.8 * 0.8 = 0.64, unpenalized)
    let expected_cap = 140.0 * 0.8 * 0.8;
    let actual_cap = fit.ship.get_attribute(attr_ids::CAPACITY, &ctx.sde);
    assert!(
        (actual_cap - expected_cap).abs() < 1e-4,
        "Expected cargo capacity {expected_cap}, got {actual_cap}"
    );
}


/// A destroyed fitted item is announced as `OnItemChange(item, {ixLocationID: ship, ixFlag: rigSlot})`
/// with the item now in `locationJunkyard` (10) / `flagJunkyardTrashed` (147); the V24 client's
/// `InventorySvc.OnItemChange` derives `OnModuleUnfitted` from it.
fn assert_rig_trashed_item_change(notes: &[(String, Vec<PyRep>)], item_id: i64, ship_id: i64) {
    let (_, args) = notes
        .iter()
        .find(|(n, a)| {
            n == "OnItemChange"
                && matches!(a.first(), Some(PyRep::KeyVal(kv)) if kv.get("itemID") == Some(&PyRep::Integer(item_id)))
        })
        .unwrap_or_else(|| panic!("OnItemChange for destroyed rig {item_id}: {notes:?}"));
    let PyRep::KeyVal(row) = &args[0] else { unreachable!() };
    assert_eq!(row.get("locationID"), Some(&PyRep::Integer(10)));
    assert_eq!(row.get("flagID"), Some(&PyRep::Integer(147)));
    let Some(PyRep::Dict(change)) = args.get(1) else { panic!("change dict expected") };
    assert!(change.iter().any(|(k, v)| *k == PyRep::Integer(3) && *v == PyRep::Integer(ship_id)));
    let old_flag = change
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(4))
        .and_then(|(_, v)| v.as_i64())
        .expect("ixFlag in change");
    assert!((92..=99).contains(&old_flag), "old flag {old_flag} must be a rig slot");
    assert!(notes.iter().all(|(n, _)| n != "OnModuleUnfitted"));
}
