//! Integration regression tests covering Round 8 findings (Part 2).

#![forbid(unsafe_code)]

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

pub(crate) async fn run_r8_part2(
    ctx: &ServerContext,
    mut s1: SessionState,
    s2: SessionState,
) -> (SessionState, SessionState) {
    let _ = s2;
    let kw = HashMap::new();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    // R12-P1-2: Dock s2 at Jita 4-4 and verify slot-type mismatch and turret hardpoint overflow are rejected
    ctx.db
        .dock_character(90_000_002, 60_003_760, false)
        .expect("Dock 90_000_002 at Jita 4-4");
    let mut s2 = SessionState::new(820);
    ctx.dispatch_rpc(
        &mut s2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "multiLoginBlocker",
        None,
        "Login",
        &[PyRep::String("hw-r8-other".into())],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid) in [
            (100_009_901_i64, 438_u32),  // 1MN Afterburner II (medPower 13)
            (100_009_902_i64, 2889_u32), // 200mm AutoCannon II #2 (turretFitted 42)
            (100_009_903_i64, 2889_u32), // 200mm AutoCannon II #3 (turretFitted 42)
            (100_009_904_i64, 2889_u32), // 200mm AutoCannon II #4 (exceeds Rifter's 3 turret hardpoints)
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
    // Fitting med-slot 1MN Afterburner II (438) into HiSlot1 (flag 28) fails with FittingValidationFailed
    let bad_slot = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_901),
                PyRep::Integer(100_000_002),
                PyRep::Integer(28),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_slot.unwrap_err().code, "FittingValidationFailed");

    // Fitting turrets #2 and #3 into HiSlot1 (28) and HiSlot2 (29) succeeds on Rifter (3 turret hardpoints),
    // but fitting turret #4 into HiSlot3 (30) fails (exceeds Rifter's 3 turret hardpoints / 4 high slots)
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_009_902),
            PyRep::Integer(100_000_002),
            PyRep::Integer(28),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 2nd turret on Rifter");
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_009_903),
            PyRep::Integer(100_000_002),
            PyRep::Integer(29),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 3rd turret on Rifter");
    let fourth_turret = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_904),
                PyRep::Integer(100_000_002),
                PyRep::Integer(30),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(fourth_turret.unwrap_err().code, "FittingValidationFailed");

    // R13-P0-1: RepackageItems rejects ships/containers with fitted modules or cargo (`CannotRepackageNonEmptyItem`)
    // and PlaceCharOrder / CreateContract reject selling/contracting non-empty ships or items in ship cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_910,
            eve_db::InventoryItemRecord {
                item_id: 100_009_910,
                type_id: 587, // Spare Rifter in Jita 4-4 Hangar
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_009_911,
            eve_db::InventoryItemRecord {
                item_id: 100_009_911,
                type_id: 1236, // Overdrive Injector System II fitted on spare Rifter 100_009_910
                owner_id: 90_000_002,
                location_id: 100_009_910,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let repack_non_empty = ctx
        .dispatch_rpc(
            &mut s2,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(100_009_910)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        repack_non_empty.unwrap_err().code,
        "CannotRepackageNonEmptyItem"
    );
    let contract_non_empty = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(100_009_910)]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        contract_non_empty.unwrap_err().code,
        "CannotContractNonEmptyContainer"
    );

    // Unfitting 100_009_911 back to Hangar allows 100_009_910 to be repackaged
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&100_009_911) {
            m.location_id = 60_003_760;
            m.flag = eve_db::InventoryFlag::Hangar;
        }
    });
    ctx.dispatch_rpc(
        &mut s2,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::Integer(100_009_910)],
        &kw,
        None,
    )
    .await
    .expect("Repackage empty spare Rifter 100_009_910");

    // R13-P1-1: Missing skill prerequisites reject module fitting (`FittingValidationFailed`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&90_000_002) {
            sk.remove(&3450); // Remove Afterburner IV (required for 1MN Afterburner II 438)
        }
    });
    ctx.invalidate_ship_fitting(100_000_002);
    let missing_skill_fit = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(100_009_901),
                PyRep::Integer(100_000_002),
                PyRep::Integer(19),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        missing_skill_fit.unwrap_err().code,
        "FittingValidationFailed"
    );

    // Restore Afterburner IV (3450: 4) and verify 1MN Afterburner II (438) fits into MedSlot0 (19)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.clone_grade = 2; // Omega clone so Afterburner IV is effective
        }
        snap.skills.entry(90_000_002).or_default().insert(
            3450,
            eve_db::TrainedSkillRecord {
                type_id: 3450,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(100_000_002);
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_009_901),
            PyRep::Integer(100_000_002),
            PyRep::Integer(19),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 1MN Afterburner II after training Afterburner IV");

    // R13-P1-2: Applying a mutaplasmid to a multi-unit stack (quantity 5) decrements the stack to 4 and creates a new singleton Abyssal item
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_920,
            eve_db::InventoryItemRecord {
                item_id: 100_009_920,
                type_id: 438, // 5x repackaged 1MN Afterburner II stack
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_009_921,
            eve_db::InventoryItemRecord {
                item_id: 100_009_921,
                type_id: 47746, // Decayed 1MN Afterburner Mutaplasmid
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let muta_stack_res = ctx
        .dispatch_rpc(
            &mut s2,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47746), PyRep::Integer(100_009_920)],
            &kw,
            None,
        )
        .await
        .expect("CreateDynamicItem on 5x stack");
    let PyRep::KeyVal(ms_kv) = muta_stack_res else {
        panic!("Expected CreateDynamicItem KeyVal");
    };
    let split_abyssal_id = ms_kv.get("itemID").unwrap().as_i64().unwrap();
    assert_ne!(split_abyssal_id, 100_009_920);
    let orig_stack = ctx.db.get_item(100_009_920).unwrap();
    assert_eq!(orig_stack.type_id, 438);
    assert_eq!(orig_stack.quantity, 4);
    assert!(!orig_stack.singleton);
    let split_abyssal = ctx.db.get_item(split_abyssal_id).unwrap();
    assert_eq!(split_abyssal.type_id, 47749);
    assert_eq!(split_abyssal.quantity, 1);
    assert!(split_abyssal.singleton);
    // R22-P1-4: Verify rolled Abyssal attributes lie within [min_mult * base, max_mult * base] for mutaplasmid 47746 on type 438
    let base_438 = &ctx.sde.types.get(&438).unwrap().default_attributes;
    for (attr_id, min_m, max_m) in [
        (6_u16, 0.85, 1.20),
        (20_u16, 0.95, 1.10),
        (30_u16, 0.95, 1.25),
        (50_u16, 0.95, 1.25),
    ] {
        let base_val = base_438[&attr_id];
        let rolled_val = split_abyssal.dynamic_attributes[&attr_id];
        assert!(
            rolled_val >= base_val * min_m - 1e-6 && rolled_val <= base_val * max_m + 1e-6,
            "Abyssal attr {attr_id} rolled {rolled_val} outside [{}, {}]",
            base_val * min_m,
            base_val * max_m
        );
    }

    // R13-P1-3: SaveFitting validates and persists caller-supplied description and module list, and rejects invalid module/hull combinations
    let bad_save_fit = ctx
        .dispatch_rpc(
            &mut s2,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::String("Invalid Trig Rifter".into()),
                PyRep::Integer(587),
                PyRep::String("Should fail".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(47918),
                    PyRep::Integer(27),
                    PyRep::Integer(1),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_save_fit.unwrap_err().code, "InvalidFittingModule");

    let saved_fid = ctx
        .dispatch_rpc(
            &mut s2,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::String("Custom Rifter Tackler".into()),
                PyRep::Integer(587),
                PyRep::String("Fast tackle with autocannons and overdrive".into()),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(2889),
                        PyRep::Integer(27),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(438),
                        PyRep::Integer(19),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1236),
                        PyRep::Integer(11),
                        PyRep::Integer(1),
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("SaveFitting with custom modules and description")
        .as_i64()
        .unwrap();
    let saved_rec = ctx
        .db
        .with_snapshot(|snap| snap.saved_fittings.get(&saved_fid).cloned())
        .unwrap();
    assert_eq!(
        saved_rec.description,
        "Fast tackle with autocannons and overdrive"
    );
    assert_eq!(
        saved_rec.items,
        vec![
            (2889, InventoryFlag::HiSlot0, 1),
            (438, InventoryFlag::MedSlot0, 1),
            (1236, InventoryFlag::LoSlot0, 1),
        ]
    );

    // R13-P1-4: While docked at Jita 4-4 (60_003_760), ship.Board rejects boarding an owned ship floating in space in Jita (30_000_142)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_930,
            eve_db::InventoryItemRecord {
                item_id: 100_009_930,
                type_id: 587, // Assembled Rifter floating in space in Jita (30_000_142)
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.advance_secs(12);
    let board_space_while_docked = ctx
        .dispatch_rpc(
            &mut s2,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_009_930)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        board_space_while_docked.unwrap_err().code,
        "ShipNotBoardable"
    );

    // R14-P1-2 / R17-P1-1 / R18-P1-2: Weapon modules (2889, 2404, 2410, 2420, 54739, 47918, 85084) have no fabricated damage (114, 116, 117, 118) attributes,
    // and missile launchers/charges (2404, 2410, 2420, 211, 2510) have no fabricated chargeSize (128) attribute
    let torp_attrs = &ctx.sde.types.get(&2420).unwrap().default_attributes;
    let lml_attrs = &ctx.sde.types.get(&2404).unwrap().default_attributes;
    for attr_id in 1298..=1301 {
        assert!(
            !torp_attrs.contains_key(&attr_id),
            "Torpedo Launcher II (2420) must not have fabricated canFitShipGroup attr {attr_id}"
        );
    }
    for weapon_type_id in [2889_u32, 2404, 2410, 2420, 54739, 47918, 85084] {
        let w_attrs = &ctx
            .sde
            .types
            .get(&weapon_type_id)
            .unwrap()
            .default_attributes;
        for attr_id in [114_u16, 116, 117, 118] {
            assert!(
                !w_attrs.contains_key(&attr_id),
                "Weapon module {weapon_type_id} must not have damage attr {attr_id}"
            );
        }
    }
    for missile_type_id in [2404_u32, 2410, 2420, 211, 2510] {
        let m_attrs = &ctx
            .sde
            .types
            .get(&missile_type_id)
            .unwrap()
            .default_attributes;
        assert!(
            !m_attrs.contains_key(&128),
            "Missile launcher/charge {missile_type_id} must not have chargeSize (128)"
        );
    }
    assert_eq!(torp_attrs.get(&51).copied(), Some(14400.0));
    assert_eq!(torp_attrs.get(&604).copied(), Some(657.0));
    assert_eq!(torp_attrs.get(&606).copied(), Some(89.0));
    assert_eq!(lml_attrs.get(&51).copied(), Some(12800.0));
    assert_eq!(lml_attrs.get(&604).copied(), Some(384.0));
    let inferno_torp_attrs = &ctx.sde.types.get(&2510).unwrap().default_attributes;
    assert_eq!(inferno_torp_attrs.get(&118).copied(), Some(450.0));
    assert_eq!(inferno_torp_attrs.get(&653).copied(), Some(78.0));
    assert_eq!(inferno_torp_attrs.get(&654).copied(), Some(405.0));
    assert_eq!(inferno_torp_attrs.get(&1353).copied(), Some(0.944));
    let inferno_lml_attrs = &ctx.sde.types.get(&211).unwrap().default_attributes;
    assert_eq!(inferno_lml_attrs.get(&118).copied(), Some(83.0));
    assert_eq!(inferno_lml_attrs.get(&653).copied(), Some(170.0));
    assert_eq!(inferno_lml_attrs.get(&654).copied(), Some(40.0));
    assert_eq!(inferno_lml_attrs.get(&1353).copied(), Some(0.604));
    let rf_pp_attrs = &ctx.sde.types.get(&21924).unwrap().default_attributes;
    assert_eq!(rf_pp_attrs.get(&117).copied(), Some(2.3));
    assert_eq!(rf_pp_attrs.get(&118).copied(), Some(11.5));
    assert_eq!(rf_pp_attrs.get(&128).copied(), Some(1.0));
    assert_eq!(rf_pp_attrs.get(&137).copied(), Some(55.0));
    let vorton_attrs = &ctx.sde.types.get(&54739).unwrap().default_attributes;
    let breacher_attrs = &ctx.sde.types.get(&85084).unwrap().default_attributes;
    assert!(!vorton_attrs.contains_key(&1298));
    assert!(!breacher_attrs.contains_key(&1298));
    assert_eq!(vorton_attrs.get(&51).copied(), Some(6000.0));
    assert_eq!(vorton_attrs.get(&54).copied(), Some(22464.0));
    assert_eq!(vorton_attrs.get(&64).copied(), Some(1.0));
    assert_eq!(vorton_attrs.get(&604).copied(), Some(4062.0));
    assert_eq!(vorton_attrs.get(&653).copied(), Some(190.0));
    assert_eq!(vorton_attrs.get(&654).copied(), Some(43.0));
    assert_eq!(vorton_attrs.get(&1353).copied(), Some(0.40));

    // R18-P1-1: Attempting to load an incompatible charge (Inferno Light Missile 211) into 200mm AutoCannon II (2889) is rejected with IncompatibleCharge
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_940,
            eve_db::InventoryItemRecord {
                item_id: 100_009_940,
                type_id: 211,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let incomp_load = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_002),
                PyRep::Integer(27),
                PyRep::Integer(100_009_940),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(incomp_load.unwrap_err().code, "IncompatibleAmmo");

    // R14-P1-1: SaveSkillQueue rejects skills with unmet prerequisites (Torpedo Specialization 20213 requires Torpedoes 3325 V)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&3325);
    });
    let unmet_prereq_queue = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(20213),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        unmet_prereq_queue.unwrap_err().code,
        "SkillPrerequisiteNotMet"
    );

    // R14-P1-1: ApplyFreeSkillPoints partial application does not forge trained skill level before threshold SP is reached
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.free_skill_points = 500.0;
        }
        snap.skills.entry(90_000_002).or_default().remove(&3449);
        snap.skill_queues.entry(90_000_002).or_default().clear();
    });
    // Queue Navigation (3449, rank 1, requires Spaceship Command 3327 I which 90_000_002 has at level 3) to level 1 (needs 250 SP)
    ctx.dispatch_rpc(
        &mut s2,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3449),
            PyRep::Integer(1),
        ])])],
        &kw,
        None,
    )
    .await
    .expect("Queue Navigation I");
    let partial_sp_res = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Real(10.0)],
            &kw,
            None,
        )
        .await
        .expect("Apply 10 partial free SP");
    assert!(partial_sp_res.as_f64().is_some());
    let nav_lvl_after_partial = ctx
        .db
        .with_snapshot(|snap| {
            snap.skills
                .get(&90_000_002)
                .and_then(|m| m.get(&3449))
                .map(|s| (s.level, s.skill_points))
        })
        .unwrap();
    assert_eq!(nav_lvl_after_partial.0, 0);
    assert!((nav_lvl_after_partial.1 - 10.0).abs() < 1e-6);

    // Applying the remaining 240 SP reaches 250 SP and advances Navigation (3449) to level 1
    let full_sp_res = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Real(240.0)],
            &kw,
            None,
        )
        .await
        .expect("Apply remaining 240 free SP");
    assert!(full_sp_res.as_f64().is_some());
    let nav_lvl_after_full = ctx
        .db
        .with_snapshot(|snap| {
            snap.skills
                .get(&90_000_002)
                .and_then(|m| m.get(&3449))
                .map(|s| s.level)
        })
        .unwrap();
    assert_eq!(nav_lvl_after_full, 1);

    // R14-P1-4: marketProxy.GetCharOrders returns only the caller's own orders (not NPC orders),
    // and marketProxy.BuyMultipleItems requires a non-empty item list and executes multi-item purchases atomically
    let char_orders_before = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "GetCharOrders",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { rows: co_lines, .. } = char_orders_before else {
        panic!("Expected Rowset from GetCharOrders");
    };
    assert!(
        co_lines.is_empty(),
        "Character 90_000_002 has no personal market orders initially (NPC orders must be excluded)"
    );

    let empty_multi_buy = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(empty_multi_buy.unwrap_err().code, "InvalidBuyMultipleItems");

    let multi_buy_res = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(34),
                PyRep::Integer(10),
                PyRep::Real(10.0),
            ])])],
            &kw,
            None,
        )
        .await
        .expect("BuyMultipleItems 10x Tritanium");
    let PyRep::KeyVal(mb_kv) = multi_buy_res else {
        panic!("Expected KeyVal from BuyMultipleItems");
    };
    assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(10)));
    assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(50.0)));

    // R15-P1-1: marketProxy.PlaceBuyOrder rejects placing an order at a remote station in the same region (`StationMismatch`)
    // and rejects placing an order while undocked in space (`NotDockedAtStation`)
    let remote_station_order = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_766), // Perimeter station in same region 10000002 while s2 is docked at Jita 60003760
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(remote_station_order.unwrap_err().code, "StationMismatch");

    // s1 is currently in space in Jita (30_000_142) after undocking earlier in this test
    let in_space_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(in_space_order.unwrap_err().code, "NotDockedAtStation");

    // R20-P1-3: Verify PlaceBuyOrder resolves station-specific owner_corporation_id (1000035) and owner_faction_id (500001)
    // standings separately for Jita 4-4 (60003760) instead of passing corp standing as both faction and corp standing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = eve_db::IskCents::from_isk(100_000);
            ch.standings.clear();
            // Caldari State (500001) = 5.0, Caldari Navy (1000035) = 2.0
            ch.standings.insert(500_001, 5.0);
            ch.standings.insert(1_000_035, 2.0);
        }
    });
    // Place a Resting Bid at 1.0 ISK x 10_000 = 10,000.00 ISK
    // With Broker Relations IV (level 4), faction standing 5.0, corp standing 2.0:
    // rate = 0.03 - 0.003*4 - 0.0003*5.0 - 0.0002*2.0 = 0.018 - 0.0015 - 0.0004 = 0.0161 (1.61%)
    // Broker fee on 10,000 ISK = 161.00 ISK (whereas using corp standing 2.0 for both would yield 0.0170 -> 170.00 ISK)
    let _ = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceBuyOrder resting bid with separate faction and corp standings");
    let bal_after_bid = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 100,000.00 ISK - 10,000.00 ISK escrow - 161.00 ISK broker fee = 89,839.00 ISK (8_983_900 cents)
    assert_eq!(bal_after_bid, 8_983_900);

    // R19-P1-1 & R22-P1-1: Vorton Projector (Small Vorton Projector I 54739 loaded with GalvaSurge Condenser Pack S 54769)
    // uses missile_damage_factor (explosion radius 654 = 43.0m, explosion velocity 653 = 190.0 m/s, DRF 1353 = 0.40),
    // never rolls turret wrecking hits, and arcs up to 9 additional targets (3037 = 10.0) strictly within 10,000m of the primary target with no secondary hopping
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_950,
            eve_db::InventoryItemRecord {
                item_id: 100_009_950,
                type_id: 54731, // Skybreaker EDENCOM frigate hull
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_009_951,
            eve_db::InventoryItemRecord {
                item_id: 100_009_951,
                type_id: 54739, // Small Vorton Projector I
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_009_952,
            eve_db::InventoryItemRecord {
                item_id: 100_009_952,
                type_id: 54769, // GalvaSurge Condenser Pack S (116 EM + 17 Kin = 133 base damage)
                owner_id: 90_000_002,
                location_id: 100_009_950,
                flag: InventoryFlag::Cargo,
                quantity: 200,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Ensure EDENCOM Frigate (55031), Vorton Projector Operation (55033), and Small Vorton Projector (55034) are trained to level 1 on 90_000_002
        for sk_tid in [55031_u32, 55033, 55034] {
            snap.skills.entry(90_000_002).or_default().insert(
                sk_tid,
                eve_db::TrainedSkillRecord {
                    type_id: sk_tid,
                    level: 1,
                    skill_points: 1000.0,
                },
            );
        }
    });
    ctx.dispatch_rpc(
        &mut s2,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_009_950)],
        &kw,
        None,
    )
    .await
    .expect("Board Skybreaker 100_009_950");
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_009_951),
            PyRep::Integer(100_009_950),
            PyRep::Integer(27),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Small Vorton Projector I onto Skybreaker");
    ctx.dispatch_rpc(
        &mut s2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_009_950),
            PyRep::Integer(27),
            PyRep::Integer(100_009_952),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load GalvaSurge Condenser Pack S into Small Vorton Projector I");
    ctx.dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Skybreaker");
    for _ in 0..7 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let (_, sky_ball) = jita_handle
        .get_ball_snapshot(100_009_950)
        .await
        .unwrap()
        .unwrap();
    for (bid, offset_x) in [
        (700_009_950_i64, 5_000.0), // Primary target at 5 km (<= 22.46 km optimal)
        (700_009_951_i64, 13_000.0), // Secondary target 8 km from primary (<= 10 km arc radius)
        (700_009_952_i64, 17_000.0), // Out-of-arc target 12 km from primary (> 10 km arc radius, even though 4 km <= 10 km from secondary 700_009_951)
    ] {
        let _ = jita_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: bid,
                    type_id: 16997,
                    position: sky_ball.position + glam::DVec3::new(offset_x, 0.0, 0.0),
                    velocity: glam::DVec3::new(380.0, 0.0, 0.0),
                    max_velocity: 400.0,
                    inertia: 2.0,
                    mass: 1_000_000.0,
                    radius: 35.0,
                    speed_fraction: 0.95,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    shield_hp: 35.0,
                    max_shield_hp: 35.0,
                    armor_hp: 35.0,
                    max_armor_hp: 35.0,
                    structure_hp: 35.0,
                    max_structure_hp: 35.0,
                    signature_radius: 35.0,
                    ..eve_destiny::CombatState::default()
                },
            )
            .await;
    }
    let vorton_res = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(700_009_950)],
            &kw,
            None,
        )
        .await
        .expect("FireWeapon Small Vorton Projector I shot 1");
    let PyRep::KeyVal(v_kv) = vorton_res else {
        panic!("Expected KeyVal from Vorton FireWeapon");
    };
    assert_eq!(
        v_kv.get("weaponKind").and_then(|v| v.as_str()),
        Some("VortonProjector")
    );
    assert_eq!(v_kv.get("isWreckingShot"), Some(&PyRep::Bool(false)));
    assert_eq!(v_kv.get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
    assert_eq!(v_kv.get("destroyed"), Some(&PyRep::Bool(false)));
    let expected_vorton_dmg = 133.0 * ((35.0_f64 * 190.0) / (43.0 * 380.0)).powf(0.40);
    let actual_vorton_dmg = v_kv.get("appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (actual_vorton_dmg - expected_vorton_dmg).abs() < 1e-4 && actual_vorton_dmg < 133.0,
        "Vorton appliedDamage {actual_vorton_dmg} must match missile_damage_factor {expected_vorton_dmg}"
    );
    // Secondary target (105 total HP) survived shot 1 because missile_damage_factor reduced 133.0 -> 92.83 (< 105.0)
    assert!(jita_handle
        .get_ball_snapshot(700_009_951)
        .await
        .unwrap()
        .is_some());
    // Step 7s for 6.0s Vorton cycle and fire shot 2: both primary (700_009_950) and secondary (700_009_951) are destroyed, while 700_009_952 (12 km > 10 km from primary, 4 km from secondary) is untouched
    for _ in 0..7 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let vorton_res2 = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(700_009_950)],
            &kw,
            None,
        )
        .await
        .expect("FireWeapon Small Vorton Projector I shot 2");
    let PyRep::KeyVal(v_kv2) = vorton_res2 else {
        panic!("Expected KeyVal from Vorton FireWeapon shot 2");
    };
    assert_eq!(v_kv2.get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(jita_handle
        .get_ball_snapshot(700_009_951)
        .await
        .unwrap()
        .is_none());
    assert!(jita_handle
        .get_ball_snapshot(700_009_952)
        .await
        .unwrap()
        .is_some());


    (s1, s2)
}
