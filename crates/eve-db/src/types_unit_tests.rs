//! Unit and mutation-killing tests for `types`.

use super::*;

#[test]
fn from_wal_error_implementations_and_db_error_display() {
    let db_err = DbError::from_wal_error("disk full".to_string());
    assert_eq!(db_err, DbError::PersistenceFailure("disk full".to_string()));
    assert_eq!(db_err.to_string(), "Durable persistence failure: disk full");

    let rpc_err = eve_proto::BridgeRpcError::from_wal_error("io error".to_string());
    assert_eq!(rpc_err.code, "PersistenceFailure");
    assert_eq!(rpc_err.message, "io error");

    let str_err = String::from_wal_error("wal broken".to_string());
    assert_eq!(str_err, "wal broken");

    let tuple_err = <(u32, String)>::from_wal_error("bad sector".to_string());
    assert_eq!(tuple_err, (500, "bad sector".to_string()));
}

#[test]
#[should_panic(expected = "Durable WAL persistence failure: unit panic")]
fn from_wal_error_unit_panics() {
    let _: () = <()>::from_wal_error("unit panic".to_string());
}

#[test]
fn isk_cents_arithmetic_conversions_and_boundary_guards() {
    assert_eq!(IskCents::ZERO.raw_cents(), 0);
    assert_eq!(IskCents::from_raw_cents(12345).raw_cents(), 12345);
    assert_eq!(IskCents::from_isk(42).raw_cents(), 4200);
    assert_eq!(IskCents::from_isk(i64::MAX).raw_cents(), i64::MAX);

    // Non-finite f64 inputs -> 0
    assert_eq!(IskCents::from_f64_isk(f64::NAN), IskCents(0));
    assert_eq!(IskCents::from_f64_isk(f64::INFINITY), IskCents(0));
    assert_eq!(IskCents::from_f64_isk(f64::NEG_INFINITY), IskCents(0));

    // Rounding and clamping
    assert_eq!(IskCents::from_f64_isk(12.345), IskCents(1235));
    assert_eq!(IskCents::from_f64_isk(-12.345), IskCents(-1235));
    assert_eq!(IskCents::from_f64_isk(1e18), IskCents(i64::MAX / 2));
    assert_eq!(IskCents::from_f64_isk(-1e18), IskCents(i64::MIN / 2));

    // checked_add: negative guards, zero boundary, overflow, normal
    assert_eq!(IskCents(-1).checked_add(IskCents(10)), None);
    assert_eq!(IskCents(10).checked_add(IskCents(-1)), None);
    assert_eq!(IskCents(0).checked_add(IskCents(0)), Some(IskCents(0)));
    assert_eq!(IskCents(100).checked_add(IskCents(250)), Some(IskCents(350)));
    assert_eq!(IskCents(i64::MAX).checked_add(IskCents(1)), None);

    // checked_sub: negative rhs guard, underflow guard, equality boundary, zero rhs, normal
    assert_eq!(IskCents(100).checked_sub(IskCents(-1)), None);
    assert_eq!(IskCents(99).checked_sub(IskCents(100)), None);
    assert_eq!(IskCents(100).checked_sub(IskCents(100)), Some(IskCents(0)));
    assert_eq!(IskCents(100).checked_sub(IskCents(0)), Some(IskCents(100)));
    assert_eq!(IskCents(350).checked_sub(IskCents(100)), Some(IskCents(250)));

    // checked_mul_qty: negative self/qty guards, zero boundaries, overflow, normal
    assert_eq!(IskCents(-1).checked_mul_qty(5), None);
    assert_eq!(IskCents(100).checked_mul_qty(-1), None);
    assert_eq!(IskCents(0).checked_mul_qty(10), Some(IskCents(0)));
    assert_eq!(IskCents(100).checked_mul_qty(0), Some(IskCents(0)));
    assert_eq!(IskCents(150).checked_mul_qty(3), Some(IskCents(450)));
    assert_eq!(IskCents(i64::MAX).checked_mul_qty(2), None);

    // Wire f64 conversions
    assert_eq!(IskCents(1234).to_wire_f64(), 12.34);
    assert_eq!(IskCents(1234).to_f64_isk(), 12.34);
}

#[test]
fn inventory_flag_aliases_from_u16_and_is_service_slot() {
    assert_eq!(InventoryFlag::QuantumCoreRoom, InventoryFlag::StructureDeedBay);
    assert_eq!(InventoryFlag::OreHold, InventoryFlag::SpecializedOreHold);
    assert_eq!(InventoryFlag::GasHold, InventoryFlag::SpecializedGasHold);
    assert_eq!(InventoryFlag::IceHold, InventoryFlag::SpecializedIceHold);
    assert_eq!(
        InventoryFlag::MobileDepotHold,
        InventoryFlag::SpecializedMobileDepotHold
    );
    assert_eq!(
        InventoryFlag::InfrastructureHold,
        InventoryFlag::SpecializedInfrastructureHold
    );
    assert_eq!(InventoryFlag::CorpHangar, InventoryFlag::CorpSAG1);

    let all_known: &[(u16, InventoryFlag)] = &[
        (0, InventoryFlag::None),
        (3, InventoryFlag::Wardrobe),
        (4, InventoryFlag::Hangar),
        (5, InventoryFlag::Cargo),
        (7, InventoryFlag::Skill),
        (11, InventoryFlag::LoSlot0),
        (12, InventoryFlag::LoSlot1),
        (13, InventoryFlag::LoSlot2),
        (14, InventoryFlag::LoSlot3),
        (15, InventoryFlag::LoSlot4),
        (16, InventoryFlag::LoSlot5),
        (17, InventoryFlag::LoSlot6),
        (18, InventoryFlag::LoSlot7),
        (19, InventoryFlag::MedSlot0),
        (20, InventoryFlag::MedSlot1),
        (21, InventoryFlag::MedSlot2),
        (22, InventoryFlag::MedSlot3),
        (23, InventoryFlag::MedSlot4),
        (24, InventoryFlag::MedSlot5),
        (25, InventoryFlag::MedSlot6),
        (26, InventoryFlag::MedSlot7),
        (27, InventoryFlag::HiSlot0),
        (28, InventoryFlag::HiSlot1),
        (29, InventoryFlag::HiSlot2),
        (30, InventoryFlag::HiSlot3),
        (31, InventoryFlag::HiSlot4),
        (32, InventoryFlag::HiSlot5),
        (33, InventoryFlag::HiSlot6),
        (34, InventoryFlag::HiSlot7),
        (56, InventoryFlag::Capsule),
        (57, InventoryFlag::Pilot),
        (62, InventoryFlag::CorpDeliveries),
        (63, InventoryFlag::Locked),
        (64, InventoryFlag::Unlocked),
        (72, InventoryFlag::Impounded),
        (87, InventoryFlag::DroneBay),
        (88, InventoryFlag::Booster),
        (89, InventoryFlag::Implant),
        (90, InventoryFlag::ShipHangar),
        (92, InventoryFlag::RigSlot0),
        (93, InventoryFlag::RigSlot1),
        (94, InventoryFlag::RigSlot2),
        (116, InventoryFlag::CorpSAG1),
        (117, InventoryFlag::CorpSAG2),
        (118, InventoryFlag::CorpSAG3),
        (119, InventoryFlag::CorpSAG4),
        (120, InventoryFlag::CorpSAG5),
        (121, InventoryFlag::CorpSAG6),
        (122, InventoryFlag::CorpSAG7),
        (125, InventoryFlag::SubSystemSlot0),
        (126, InventoryFlag::SubSystemSlot1),
        (127, InventoryFlag::SubSystemSlot2),
        (128, InventoryFlag::SubSystemSlot3),
        (129, InventoryFlag::SubSystemSlot4),
        (133, InventoryFlag::SpecializedFuelBay),
        (134, InventoryFlag::SpecializedOreHold),
        (135, InventoryFlag::SpecializedGasHold),
        (136, InventoryFlag::SpecializedMineralHold),
        (137, InventoryFlag::SpecializedSalvageHold),
        (138, InventoryFlag::SpecializedShipHold),
        (143, InventoryFlag::SpecializedAmmoHold),
        (148, InventoryFlag::SpecializedCommandCenterHold),
        (149, InventoryFlag::SpecializedPlanetaryCommoditiesHold),
        (151, InventoryFlag::SpecializedMaterialBay),
        (155, InventoryFlag::FleetHangar),
        (158, InventoryFlag::FighterBay),
        (164, InventoryFlag::ServiceSlot0),
        (165, InventoryFlag::ServiceSlot1),
        (166, InventoryFlag::ServiceSlot2),
        (167, InventoryFlag::ServiceSlot3),
        (168, InventoryFlag::ServiceSlot4),
        (169, InventoryFlag::ServiceSlot5),
        (170, InventoryFlag::ServiceSlot6),
        (171, InventoryFlag::ServiceSlot7),
        (172, InventoryFlag::StructureFuel),
        (173, InventoryFlag::Deliveries),
        (177, InventoryFlag::FrigateEscapeBay),
        (179, InventoryFlag::SpecializedMobileDepotHold),
        (180, InventoryFlag::StructureDeedBay),
        (181, InventoryFlag::SpecializedIceHold),
        (182, InventoryFlag::SpecializedAsteroidHold),
        (185, InventoryFlag::SpecializedInfrastructureHold),
    ];

    for &(raw, expected) in all_known {
        let got = InventoryFlag::from_u16(raw);
        assert_eq!(got, expected, "mismatch for raw flag {raw}");
        assert_eq!(got as u16, raw);
        let expect_svc = (164..=171).contains(&raw);
        assert_eq!(got.is_service_slot(), expect_svc, "service slot check for {raw}");
    }
    assert_eq!(InventoryFlag::from_u16(9999), InventoryFlag::None);
}

#[test]
fn account_restriction_neural_attributes_and_record_defaults() {
    let mut acct = AccountRecord {
        user_id: 1,
        username: "pilot".to_string(),
        password_token: "hash".to_string(),
        role: 0,
        banned_until_unix: None,
        ban_reason: None,
    };
    assert_eq!(acct.check_active_restriction(100), Ok(()));

    acct.banned_until_unix = Some(200);
    acct.ban_reason = Some("RMT".to_string());
    assert_eq!(
        acct.check_active_restriction(199),
        Err(DbError::AccountBanned {
            until_unix: 200,
            reason: "RMT".to_string(),
        })
    );
    // Boundary: now_unix == until -> restriction expired
    assert_eq!(acct.check_active_restriction(200), Ok(()));
    assert_eq!(acct.check_active_restriction(201), Ok(()));

    acct.ban_reason = Some("SUSPENDED:temp".to_string());
    assert_eq!(
        acct.check_active_restriction(150),
        Err(DbError::AccountSuspended {
            until_unix: 200,
            reason: "temp".to_string(),
        })
    );

    acct.ban_reason = None;
    assert_eq!(
        acct.check_active_restriction(150),
        Err(DbError::AccountBanned {
            until_unix: 200,
            reason: String::new(),
        })
    );

    let auth = AuthRuntimeState::new([7u8; 32]);
    assert_eq!(auth.jwt_signing_secret, [7u8; 32]);
    assert!(auth.online_sessions.is_empty());

    let def_neural = NeuralAttributes::default();
    assert_eq!(def_neural.charisma, 19);
    assert_eq!(def_neural.intelligence, 20);
    assert_eq!(def_neural.memory, 20);
    assert_eq!(def_neural.perception, 20);
    assert_eq!(def_neural.willpower, 20);
    assert_eq!(def_neural.bonus_remaps, 2);

    let custom_neural = NeuralAttributes {
        charisma: 17,
        intelligence: 21,
        memory: 22,
        perception: 23,
        willpower: 24,
        bonus_remaps: 1,
        last_remap_unix: Some(10),
        remap_cooldown_until_unix: Some(20),
    };
    assert_eq!(custom_neural.get_attribute_by_id(164), 17.0);
    assert_eq!(custom_neural.get_attribute_by_id(165), 21.0);
    assert_eq!(custom_neural.get_attribute_by_id(166), 22.0);
    assert_eq!(custom_neural.get_attribute_by_id(167), 23.0);
    assert_eq!(custom_neural.get_attribute_by_id(168), 24.0);
    assert_eq!(custom_neural.get_attribute_by_id(999), 23.0);

    let item = InventoryItemRecord::default();
    assert_eq!(item.flag, InventoryFlag::Hangar);
    assert_eq!(item.quantity, 1);
    assert_eq!(item.version, 1);

    let contract = ContractRecord::default();
    assert_eq!(contract.num_days, 1);
    let parsed_contract: ContractRecord = serde_json::from_str(
        r#"{"contract_id":1,"issuer_id":2,"acceptor_id":null,"contract_type":"ItemExchange","status":"Outstanding","start_station_id":60003760,"end_station_id":60003760,"price":0,"reward":0,"collateral":0,"item_ids":[]}"#,
    )
    .expect("deserialize ContractRecord");
    assert_eq!(parsed_contract.num_days, 1);

    let folder = BookmarkFolderRecord::default();
    assert_eq!(folder.folder_id, 0);
    assert!(!folder.is_shared);

    let member: FleetMemberRecord =
        serde_json::from_str(r#"{"character_id":90000001,"role":"SquadMember"}"#)
            .expect("deserialize FleetMemberRecord");
    assert!(member.accepts_regroup);
    assert!(member.accepts_conduit_jumps);
    assert!(!member.takes_fleet_warp);

    let st: UpwellStructureRecord = serde_json::from_str(
        r#"{"structure_id":1,"type_id":35832,"owner_corp_id":98000001,"solar_system_id":30000142,"name":"Astra","position":[0.0,0.0,0.0],"state":"ShieldVulnerable","quantum_core_installed":true,"fuel_blocks":100}"#,
    )
    .expect("deserialize UpwellStructureRecord");
    assert_eq!(st.shield_hp, 5_000.0);
    assert_eq!(st.armor_hp, 5_000.0);
    assert_eq!(st.hull_hp, 5_000.0);
}

#[test]
fn corp_roles_and_corporation_wallet_divisions() {
    use corp_roles::*;

    assert!(has_role(u64::MAX, ROLE_ACCOUNTANT));
    assert!(has_role(ROLE_DIRECTOR, ROLE_STATION_MANAGER));
    assert!(has_role(
        ROLE_ACCOUNTANT | ROLE_AUDITOR,
        ROLE_ACCOUNTANT | ROLE_AUDITOR
    ));
    assert!(!has_role(ROLE_ACCOUNTANT, ROLE_ACCOUNTANT | ROLE_AUDITOR));
    assert!(!has_role(0, ROLE_ACCOUNTANT));

    let divs = CorporationRecord::default_wallet_divisions(IskCents(50_000));
    assert_eq!(divs.len(), 7);
    assert_eq!(divs[0].division_id, 1);
    assert_eq!(divs[0].name, "Master Wallet");
    assert_eq!(divs[0].balance, IskCents(50_000));
    for d in 2..=7u8 {
        let idx = (d - 1) as usize;
        assert_eq!(divs[idx].division_id, d);
        assert_eq!(divs[idx].name, format!("Division {d}"));
        assert_eq!(divs[idx].balance, IskCents(0));
    }

    // Empty wallet_divisions fallback in division_balance
    let mut corp = CorporationRecord {
        master_wallet: IskCents(75_000),
        ..Default::default()
    };
    assert_eq!(corp.division_balance(1), IskCents(75_000));
    assert_eq!(corp.division_balance(2), IskCents(0));

    // When division 1 exists with 0 balance but master_wallet != 0
    corp.wallet_divisions.push(CorpWalletDivisionRecord {
        division_id: 1,
        name: "Master Wallet".to_string(),
        balance: IskCents(0),
    });
    assert_eq!(corp.division_balance(1), IskCents(75_000));
    corp.ensure_wallet_divisions();
    assert_eq!(corp.wallet_divisions.len(), 7);
    assert_eq!(corp.wallet_divisions[0].balance, IskCents(75_000));

    // When division 1 has non-zero balance, ensure_wallet_divisions syncs master_wallet from div1
    corp.wallet_divisions[0].balance = IskCents(90_000);
    corp.master_wallet = IskCents(10_000);
    corp.ensure_wallet_divisions();
    assert_eq!(corp.master_wallet, IskCents(90_000));

    // set_division_balance on div 2 and div 1
    corp.set_division_balance(2, IskCents(12_300));
    assert_eq!(corp.division_balance(2), IskCents(12_300));
    assert_eq!(corp.master_wallet, IskCents(90_000));

    corp.set_division_balance(1, IskCents(44_000));
    assert_eq!(corp.division_balance(1), IskCents(44_000));
    assert_eq!(corp.master_wallet, IskCents(44_000));
}

#[test]
fn corp_division_single_entrypoint_credit_debit_and_builder() {
    let mut corp = CorporationRecord::new(98_001_001, "Arch Corp", "ARCH", 90_001_001)
        .with_master_wallet(IskCents::from_isk(1_000));
    assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(1_000));
    assert_eq!(corp.division_balance(1), IskCents::from_isk(1_000));
    assert_eq!(corp.master_wallet, corp.wallet_divisions[0].balance);

    let after_credit = corp.credit_division(1, IskCents::from_isk(500));
    assert_eq!(after_credit, IskCents::from_isk(1_500));
    assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(1_500));
    assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(1_500));

    assert_eq!(corp.debit_division(1, IskCents::from_isk(1_501)), None);
    assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(1_500));

    assert_eq!(
        corp.debit_division(1, IskCents::from_isk(250)),
        Some(IskCents::from_isk(1_250))
    );
    assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(1_250));
    assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(1_250));

    corp.credit_division(3, IskCents::from_isk(400));
    assert_eq!(corp.division_balance(3), IskCents::from_isk(400));
    assert_eq!(
        corp.debit_division(3, IskCents::from_isk(150)),
        Some(IskCents::from_isk(250))
    );
    assert_eq!(corp.master_wallet_balance(), IskCents::from_isk(1_250));
}

#[test]
#[should_panic(expected = "Durable WAL persistence failure: infallible panic")]
#[allow(unreachable_code)]
fn from_wal_error_infallible_panics() {
    let _ = <std::convert::Infallible>::from_wal_error("infallible panic".to_string());
}

#[test]
fn db_error_display_and_isk_cents_additional_edge_cases() {
    let cases = [
        (DbError::InvalidCredentials, "Invalid credentials"),
        (
            DbError::AccountBanned {
                until_unix: 10,
                reason: "tos".into(),
            },
            "Account banned until 10: tos",
        ),
        (
            DbError::AccountSuspended {
                until_unix: 20,
                reason: "cooloff".into(),
            },
            "Account suspended until 20: cooloff",
        ),
        (
            DbError::AlreadyLoggedIn {
                user_id: 1,
                active_session_id: 2,
            },
            "Account 1 already has an active online session (2)",
        ),
        (DbError::CharacterNotFound(90), "Character 90 not found"),
        (DbError::NotDocked(90), "Character 90 is already undocked in space"),
        (
            DbError::CharacterOwnershipViolation {
                user_id: 1,
                character_id: 90,
            },
            "Character 90 is not owned by user 1",
        ),
        (DbError::ItemNotFound(55), "Item 55 not found"),
        (
            DbError::ItemOwnershipViolation {
                character_id: 90,
                item_id: 55,
            },
            "Item 55 is not owned by character 90",
        ),
        (
            DbError::ItemLocked(55),
            "Item 55 is locked in escrow or an active job and cannot be modified",
        ),
        (
            DbError::InvalidDestinationLocation {
                character_id: 90,
                location_id: 600,
            },
            "Destination location 600 is not authorized for character 90",
        ),
        (
            DbError::VersionConflict {
                expected: 3,
                actual: 2,
            },
            "Stale entity version (expected 3, found 2)",
        ),
        (
            DbError::InsufficientFunds {
                required: 500,
                available: 100,
            },
            "Insufficient ISK balance (required 500, available 100)",
        ),
        (
            DbError::AlphaCloneSpCapExceeded,
            "Alpha clone skill point cap (5,000,000 SP) exceeded",
        ),
        (
            DbError::ShipNotAtDockedStation {
                ship_id: 10,
                ship_location_id: 20,
                station_id: 30,
            },
            "Active ship 10 is located at 20, not at docked station 30",
        ),
        (
            DbError::ValidationError("bad input".into()),
            "Domain validation error: bad input",
        ),
    ];
    for (err, expected) in cases {
        assert_eq!(err.to_string(), expected);
    }

    assert_eq!(IskCents::default(), IskCents::ZERO);
    assert_eq!(IskCents::from_isk(i64::MIN).raw_cents(), i64::MIN);
    assert_eq!(IskCents::from_f64_isk(0.0), IskCents::ZERO);
    assert_eq!(IskCents::from_f64_isk(-0.0), IskCents::ZERO);
    assert_eq!(IskCents(-1).checked_sub(IskCents(0)), None);
    assert_eq!(IskCents(-1).checked_mul_qty(0), None);
    assert_eq!(IskCents(0).checked_mul_qty(-1), None);
    assert!(IskCents(100) < IskCents(200));
}

#[test]
fn corporation_credit_division_sorting_and_record_serde_edges() {
    let mut corp = CorporationRecord {
        master_wallet: IskCents(1_000),
        wallet_divisions: vec![
            CorpWalletDivisionRecord {
                division_id: 5,
                name: "Div 5".into(),
                balance: IskCents(500),
            },
            CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master Wallet".into(),
                balance: IskCents(2_000),
            },
        ],
        ..Default::default()
    };

    // division_balance(1) returns non-zero div1.balance rather than master_wallet
    assert_eq!(corp.division_balance(1), IskCents(2_000));
    corp.ensure_wallet_divisions();
    let ids: Vec<u8> = corp.wallet_divisions.iter().map(|d| d.division_id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(corp.master_wallet, IskCents(2_000));

    // credit_division on div 1 and div 5 and overflow guard
    assert_eq!(corp.credit_division(1, IskCents(300)), IskCents(2_300));
    assert_eq!(corp.master_wallet, IskCents(2_300));
    assert_eq!(corp.credit_division(5, IskCents(250)), IskCents(750));
    assert_eq!(corp.credit_division(5, IskCents(-10)), IskCents(750));

    corp.set_division_balance(2, IskCents(i64::MAX));
    assert_eq!(corp.credit_division(2, IskCents(1)), IskCents(i64::MAX));
    // Invalid division id is a safe no-op
    corp.set_division_balance(99, IskCents(123));
    assert_eq!(corp.division_balance(99), IskCents(0));

    // corp_roles mask invariants
    assert_eq!(corp_roles::ROLE_CONFIG_EQUIPMENT, corp_roles::ROLE_EQUIPMENT_CONFIG);
    for r in corp_roles::ROLE_HANGAR_CAN_TAKE
        .into_iter()
        .chain(corp_roles::ROLE_HANGAR_CAN_QUERY)
        .chain(corp_roles::ROLE_ACCOUNT_CAN_TAKE)
    {
        assert_ne!(corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK & r, 0);
    }
    assert_eq!(
        corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK & corp_roles::ROLE_DIRECTOR,
        0
    );
    assert_eq!(
        corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK & corp_roles::ROLE_DIPLOMAT,
        0
    );

    // FleetRole, KillmailRecord, UpwellStructureRecord, SystemDynamicState defaults
    for role in [
        FleetRole::FleetCommander,
        FleetRole::WingCommander,
        FleetRole::SquadCommander,
        FleetRole::SquadMember,
    ] {
        let j = serde_json::to_string(&role).unwrap();
        let back: FleetRole = serde_json::from_str(&j).unwrap();
        assert_eq!(back, role);
    }
    let km_legacy: KillmailRecord = serde_json::from_str(
        r#"{"kill_id":1,"victim_character_id":2,"attacker_character_id":3,"solar_system_id":30000142,"ship_type_id":587,"total_value_isk":100.0}"#,
    )
    .unwrap();
    assert_eq!(km_legacy.killed_unix, 0);
    assert_eq!(km_legacy.victim_corp_id, 0);
    assert_eq!(km_legacy.victim_alliance_id, None);
    assert_eq!(UpwellStructureRecord::default().shield_hp, 0.0);
    assert_eq!(SystemDynamicState::default().sov_alliance_id, None);
}
