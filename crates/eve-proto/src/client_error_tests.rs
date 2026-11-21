use super::*;

fn decode_wrapped_user_error(body: &PyRep) -> (String, Vec<(PyRep, PyRep)>) {
    let PyRep::Tuple(parts) = body else {
        panic!("expected ErrorResponse tuple");
    };
    assert_eq!(parts[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner payload tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled exception bytes");
    };
    let PyRep::ObjectEx { header, .. } = marshal::decode(raw).unwrap() else {
        panic!("expected ObjectEx");
    };
    let PyRep::Tuple(h) = *header else {
        panic!("expected header tuple");
    };
    assert_eq!(h[0], PyRep::Token("eveexceptions.UserError".into()));
    let PyRep::Tuple(u_args) = &h[1] else {
        panic!("expected UserError args");
    };
    let name = u_args[0].as_str().expect("UserError name").to_owned();
    let PyRep::Dict(entries) = &u_args[1] else {
        panic!("expected UserError dict");
    };
    (name, entries.clone())
}

#[test]
fn all_user_error_keys_round_trip_and_produce_valid_macho_error_body() {
    for &key in UserErrorKey::ALL {
        assert_eq!(UserErrorKey::from_code(key.as_str()), Some(key));
        assert_eq!(key.to_string(), key.as_str());
        if key.v24_dialog_id().is_none() {
            assert!(
                V24_PROGRAMMATIC_USER_ERROR_KEYS.contains(&key.as_str()),
                "{key:?} must be in V24_PROGRAMMATIC_USER_ERROR_KEYS"
            );
        }
        let err = ClientError::user(key);
        let body = err.to_macho_error_body(6);
        let PyRep::Tuple(parts) = &body else {
            panic!("expected ErrorResponse tuple");
        };
        assert_eq!(parts[0], PyRep::Integer(6));
        let (name, _entries) = decode_wrapped_user_error(&body);
        assert_eq!(name, key.as_str());
    }
}

#[test]
fn user_error_arg_encodes_v24_tuples_and_6dp_reals() {
    assert_eq!(UserErrorArg::TypeId(587).to_pyrep(), PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(587)]));
    assert_eq!(UserErrorArg::Label(UI_LABEL_SHIP_TOO_LARGE).to_pyrep(), PyRep::Tuple(vec![PyRep::Integer(101), PyRep::String(UI_LABEL_SHIP_TOO_LARGE.into())]));
    assert_eq!(UserErrorArg::Int(42).to_pyrep(), PyRep::Integer(42));
    assert_eq!(UserErrorArg::Real(15.123456789).to_pyrep(), PyRep::Real(15.123457));
    assert_eq!(UserErrorArg::Real(f64::NAN).to_pyrep(), PyRep::Real(0.0));
    assert_eq!(UserErrorArg::IskCents(IskCents::from_raw_cents(12_345)).to_pyrep(), PyRep::Real(123.45));

    let args_slice: &[(&str, UserErrorArg)] = &[("typeID", UserErrorArg::Int(3300))];
    assert_eq!(
        ClientError::user_with_args(UserErrorKey::SkillMissing, args_slice),
        ClientError::user_with_args(UserErrorKey::SkillMissing, [("typeID", UserErrorArg::Int(3300))]),
    );
    let arg_rt: UserErrorArg = serde_json::from_str(&serde_json::to_string(&UserErrorArg::TypeId(670)).unwrap()).unwrap();
    assert_eq!(arg_rt, UserErrorArg::TypeId(670));
    let lbl_rt: UserErrorArg = serde_json::from_str(&serde_json::to_string(&UserErrorArg::Label(UI_LABEL_SHIP_TOO_LARGE)).unwrap()).unwrap();
    assert_eq!(lbl_rt, UserErrorArg::Label(UI_LABEL_SHIP_TOO_LARGE));
}

#[test]
fn user_error_presentation_is_enforced_per_key_modal_vs_notify() {
    for (k, p) in [
        (UserErrorKey::CustomInfo, UserErrorPresentation::Modal),
        (UserErrorKey::NotEnoughCargoSpace, UserErrorPresentation::Modal),
        (UserErrorKey::ShipCannotUseStargate, UserErrorPresentation::Modal),
        (UserErrorKey::CustomNotify, UserErrorPresentation::Notify),
        (UserErrorKey::DockingRequestDenied, UserErrorPresentation::Notify),
        (UserErrorKey::TargetTooFar, UserErrorPresentation::Notify),
        (UserErrorKey::NoCharges, UserErrorPresentation::Notify),
        (UserErrorKey::TargetNotLocked, UserErrorPresentation::Notify),
    ] {
        assert_eq!(k.presentation(), p);
    }

    let notify = BridgeRpcError::custom_notify("Toast alert");
    assert_eq!((notify.code.as_str(), notify.message.as_str()), ("CustomNotify", "Toast alert"));
    assert_eq!(notify.client_presentation("svc", "m"), UserErrorPresentation::Notify);
    let client_notify = notify.to_client_error("svc", "m");
    assert_eq!(client_notify, ClientError::CustomNotify("Toast alert".into()));
    let (wire_key, wire_dict) = decode_wrapped_user_error(&client_notify.to_macho_error_body(6));
    assert_eq!(wire_key, "CustomNotify");
    assert_eq!(wire_dict, vec![(PyRep::ascii_bytes("notify"), PyRep::ascii_bytes("Toast alert"))]);
}

#[test]
fn client_error_variants_map_totally_without_string_scraping() {
    let cargo = BridgeRpcError::hold_capacity_error(
        UserErrorKey::NotEnoughCargoSpace,
        80.0,
        15.1234567,
    );
    assert_eq!(cargo.code, "NotEnoughCargoSpace");
    assert_eq!(cargo.message, "volume=80, available=15.123457");
    assert_eq!(
        cargo.to_client_error("invbroker", "Add"),
        ClientError::User {
            key: UserErrorKey::NotEnoughCargoSpace,
            args: vec![
                ("volume".into(), PyRep::Real(80.0)),
                ("available".into(), PyRep::Real(15.123457)),
            ],
        }
    );

    let cargo_typed = BridgeRpcError::user_error_with_args(
        UserErrorKey::NotEnoughCargoSpace,
        [
            ("volume", UserErrorArg::Real(80.0)),
            ("available", UserErrorArg::Real(15.5)),
        ],
    );
    assert_eq!(cargo_typed.code, "NotEnoughCargoSpace");
    assert_eq!(cargo_typed.message, "volume=80, available=15.5");

    for cap_key in [
        UserErrorKey::NotEnoughCargoSpace,
        UserErrorKey::NotEnoughDroneBaySpace,
        UserErrorKey::NotEnoughFighterBaySpace,
        UserErrorKey::NotEnoughSpecialBaySpace,
        UserErrorKey::NotEnoughSpace,
    ] {
        assert!(cap_key.is_capacity_error(), "{cap_key:?} must be capacity error");
        assert_eq!(
            cap_key.default_args(),
            vec![
                ("volume".into(), PyRep::Real(0.0)),
                ("available".into(), PyRep::Real(0.0)),
            ]
        );
        let fallback = BridgeRpcError::user_error(cap_key.as_str()).to_client_error("invbroker", "Add");
        assert_eq!(
            fallback,
            ClientError::User {
                key: cap_key,
                args: vec![
                    ("volume".into(), PyRep::Real(0.0)),
                    ("available".into(), PyRep::Real(0.0)),
                ],
            }
        );
    }
    assert_eq!(UserErrorKey::NotEnoughChargeSpace.default_args(), vec![("capacity".into(), PyRep::Real(0.0)), ("volume".into(), PyRep::Real(0.0))]);
    assert_eq!(UserErrorKey::NoSpaceForThat.default_args(), vec![("itemTypeName".into(), PyRep::Integer(0)), ("itemVolume".into(), PyRep::Real(0.0)), ("volumeAvailable".into(), PyRep::Real(0.0))]);
    assert_eq!(UserErrorKey::NoSpaceForThatOverload.default_args(), vec![("item".into(), PyRep::Integer(0)), ("maximum".into(), PyRep::Real(0.0)), ("used".into(), PyRep::Real(0.0))]);
    assert_eq!(UserErrorKey::NotEnoughCargoSpaceOverload.default_args(), vec![("item".into(), PyRep::ascii_bytes("")), ("maximum".into(), PyRep::Real(0.0)), ("used".into(), PyRep::Real(0.0))]);

    let no_charges = BridgeRpcError::no_charges_loaded(2410, "Empty weapon");
    assert_eq!(no_charges.code, "NoChargesLoaded");
    let (nc_key, nc_dict) = decode_wrapped_user_error(&no_charges.to_client_error("dogmaIM", "Activate").to_macho_error_body(6));
    assert_eq!((nc_key.as_str(), nc_dict), ("NoCharges", vec![(PyRep::ascii_bytes("launcher"), PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(2410)]))]));

    let docking_denied = BridgeRpcError::docking_request_denied(UI_LABEL_SHIP_TOO_LARGE, "Too big");
    let (dd_key, dd_dict) = decode_wrapped_user_error(&docking_denied.to_client_error("station", "Dock").to_macho_error_body(6));
    assert_eq!((dd_key.as_str(), dd_dict), ("DockingRequestDenied", vec![(PyRep::ascii_bytes("reason"), PyRep::Tuple(vec![PyRep::Integer(101), PyRep::String(UI_LABEL_SHIP_TOO_LARGE.into())]))]));

    let closed_gate = BridgeRpcError::gate_closed(50001234);
    assert_eq!(closed_gate.client_presentation("beyonce", "CmdStargateJump"), UserErrorPresentation::Modal);
    let (gc_key, gc_dict) = decode_wrapped_user_error(&closed_gate.to_client_error("beyonce", "CmdStargateJump").to_macho_error_body(6));
    assert_eq!((gc_key.as_str(), gc_dict), ("CustomInfo", vec![(PyRep::ascii_bytes("info"), PyRep::Tuple(vec![PyRep::Integer(101), PyRep::String(UI_LABEL_GATE_CLOSED.into())]))]));

    assert_eq!(UserErrorKey::NotCloseEnoughToJump.presentation(), UserErrorPresentation::Notify);
    let not_close = BridgeRpcError::not_close_enough_to_jump(2_600.0, 16_691.0, 16_591.0, 2_500.0);
    assert_eq!(not_close.client_presentation("beyonce", "CmdStargateJump"), UserErrorPresentation::Notify);
    let (ncj_key, ncj_dict) = decode_wrapped_user_error(&not_close.to_client_error("beyonce", "CmdStargateJump").to_macho_error_body(6));
    assert_eq!((ncj_key.as_str(), ncj_dict), ("NotCloseEnoughToJump", vec![(PyRep::ascii_bytes("maxdist"), PyRep::Integer(2_500))]));

    let too_far = BridgeRpcError::target_out_of_range("Guristas Scout", 484, 42_500.5, "Too far");
    assert_eq!(too_far.code, "TargetOutOfRange");
    let (tf_key, tf_dict) = decode_wrapped_user_error(&too_far.to_client_error("dogmaIM", "Activate").to_macho_error_body(6));
    assert_eq!(tf_key, "TargetTooFar");
    assert!(tf_dict.contains(&(PyRep::ascii_bytes("targetname"), PyRep::ascii_bytes("Guristas Scout"))));
    assert!(tf_dict.contains(&(PyRep::ascii_bytes("module"), PyRep::Integer(484))));
    assert!(tf_dict.contains(&(PyRep::ascii_bytes("distance"), PyRep::Real(42_500.5))));
    let orders = BridgeRpcError::max_open_orders_reached(5, 5);
    assert_eq!(orders.code, "MaxOpenOrdersReached");
    assert_eq!(orders.to_client_error("marketProxy", "PlaceBuyOrder"), ClientError::User {
        key: UserErrorKey::MarketExceededOrderCount,
        args: vec![("curCnt".into(), PyRep::Integer(5)), ("maxCnt".into(), PyRep::Integer(5))],
    });

    let custom = BridgeRpcError::custom_info("Info message");
    assert_eq!(custom.to_client_error("svc", "m"), ClientError::CustomInfo("Info message".into()));

    let unimpl = ClientError::Unimplemented { service: "insuranceSvc".into(), method: "InsureShip".into() };
    let PyRep::Tuple(parts) = unimpl.to_macho_error_body(6) else { panic!() };
    let PyRep::Tuple(inner) = &parts[2] else { panic!() };
    let PyRep::Bytes(raw) = &inner[0] else { panic!() };
    let PyRep::ObjectEx { header, .. } = marshal::decode(raw).unwrap() else { panic!() };
    let PyRep::Tuple(h) = *header else { panic!() };
    assert_eq!(h[0], PyRep::Token("exceptions.RuntimeError".into()));
}

#[test]
fn bridge_rpc_error_carries_client_error_by_value_across_serde_without_collisions() {
    let skill_a = BridgeRpcError::user_error_with_args(UserErrorKey::SkillMissing, [("typeID", UserErrorArg::Int(3300))]);
    let skill_b = BridgeRpcError::user_error_with_args(UserErrorKey::SkillMissing, [("typeID", UserErrorArg::Int(3436))]);
    let skill_empty = BridgeRpcError::from_user_key(UserErrorKey::SkillMissing);
    assert_eq!(skill_a.to_client_error("skillMgr2", "InjectSkillpoints"), ClientError::User { key: UserErrorKey::SkillMissing, args: vec![("typeID".into(), PyRep::Integer(3300))] });
    assert_eq!(skill_b.to_client_error("skillMgr2", "InjectSkillpoints"), ClientError::User { key: UserErrorKey::SkillMissing, args: vec![("typeID".into(), PyRep::Integer(3436))] });
    assert_eq!(skill_empty.to_client_error("skillMgr2", "InjectSkillpoints"), ClientError::User { key: UserErrorKey::SkillMissing, args: Vec::new() });

    let env = crate::BridgeEnvelope::RpcResponse { req_id: 42, result: Err(skill_a.clone()) };
    let wire = env.to_wire_bytes().expect("wire encode");
    assert_eq!(crate::BridgeEnvelope::from_wire_bytes(&wire).expect("wire decode"), env);

    let untyped = BridgeRpcError::user_error("CustomLegacyCode");
    let json = serde_json::to_string(&untyped).expect("json encode");
    assert!(!json.contains("client_error"), "None client_error must be omitted from JSON: {json}");
    let parsed: BridgeRpcError = serde_json::from_str(r#"{"code":"CustomLegacyCode","message":"CustomLegacyCode"}"#).expect("legacy json decode");
    assert_eq!(parsed, untyped);
    assert!(UserErrorKey::InsureShipFailedSingleContract.default_args().is_empty(), "InsureShipFailedSingleContract default_args must not emit a hardcoded character ID");
}

#[test]
fn gh1082_targeting_and_module_activation_user_error_keys_have_v24_dialog_ids() {
    for (key, key_str, dialog_id) in [
        (UserErrorKey::TargetNotLocked, "TargetNotLocked", 1627),
        (UserErrorKey::TargetingAptitudeAlreadyFullyUtilized, "TargetingAptitudeAlreadyFullyUtilized", 1634),
        (UserErrorKey::TargetingSystemsAlreadyFullyUtilized, "TargetingSystemsAlreadyFullyUtilized", 1636),
        (UserErrorKey::TargetingSystemsBeingJammed, "TargetingSystemsBeingJammed", 1637),
        (UserErrorKey::EffectAlreadyActive2, "EffectAlreadyActive2", 3252),
    ] {
        assert_eq!((key.as_str(), UserErrorKey::from_code(key_str), key.v24_dialog_id()), (key_str, Some(key), Some(dialog_id)));
        assert_eq!(BridgeRpcError::from_user_key(key).to_client_error("dogmaIM", "Activate"), ClientError::User { key, args: Vec::new() });
    }
}

#[test]
fn warp_too_short_and_destination_gone_decode_as_v24_notify_errors() {
    assert_eq!(UserErrorKey::ALL.len(), 118);
    for (key, wire_name, dialog_id) in [(UserErrorKey::WarpTooShort, "WarpTooShort", 3435), (UserErrorKey::WarpDestinationGone, "WarpDestinationGone", 1679)] {
        assert_eq!((key.as_str(), UserErrorKey::from_code(wire_name), key.v24_dialog_id(), key.v24_dialog_type(), key.presentation()), (wire_name, Some(key), Some(dialog_id), Some(3), UserErrorPresentation::Notify));
        assert!(key.default_args().is_empty());
        let client_err = ClientError::user(key);
        assert_eq!(client_err.presentation(), UserErrorPresentation::Notify);
        let (decoded_key, decoded_dict) = decode_wrapped_user_error(&client_err.to_macho_error_body(6));
        assert_eq!(decoded_key, wire_name);
        assert!(decoded_dict.is_empty());
    }
    let warp_short_helper = BridgeRpcError::warp_distance_too_short("Warp vector distance 1000.0 m is below minimum 150,000 m");
    assert_eq!((warp_short_helper.code.as_str(), warp_short_helper.client_presentation("beyonce", "CmdWarpToStuff")), ("WarpDistanceTooShort", UserErrorPresentation::Notify));
    assert_eq!(warp_short_helper.to_client_error("beyonce", "CmdWarpToStuff"), ClientError::user(UserErrorKey::WarpTooShort));
    for (code, expected_key, expected_wire) in [
        ("WarpDistanceTooShort", UserErrorKey::WarpTooShort, "WarpTooShort"),
        ("WarpTooShort", UserErrorKey::WarpTooShort, "WarpTooShort"),
        ("InvalidWarpDestination", UserErrorKey::WarpDestinationGone, "WarpDestinationGone"),
        ("WarpDestinationGone", UserErrorKey::WarpDestinationGone, "WarpDestinationGone"),
    ] {
        let err = BridgeRpcError { code: code.into(), message: "cannot warp".into(), client_error: None };
        assert_eq!(err.client_presentation("beyonce", "CmdWarpToStuff"), UserErrorPresentation::Notify);
        let client_err = err.to_client_error("beyonce", "CmdWarpToStuff");
        assert_eq!(client_err, ClientError::user(expected_key));
        let (decoded_key, decoded_dict) = decode_wrapped_user_error(&client_err.to_macho_error_body(6));
        assert_eq!(decoded_key, expected_wire);
        assert!(decoded_dict.is_empty());
    }
}

#[test]
fn bound_ownership_and_auth_denied_encode_as_unmacho_destination() {
    for code in ["BoundObjectOwnershipViolation", "BindAuthorizationDenied"] {
        let err = BridgeRpcError { code: code.into(), message: "stale station ship moniker".into(), client_error: None };
        assert_eq!(err.code, code);
        let client_err = err.to_client_error("ship", "MachoBindObject");
        assert_eq!(client_err, ClientError::SessionMismatch(format!("{code}: stale station ship moniker")));
        assert_eq!(client_err.presentation(), UserErrorPresentation::Modal);
        let (decoded_key, decoded_dict) = decode_wrapped_user_error(&client_err.to_macho_error_body(6));
        assert_eq!(decoded_key, "UnMachoDestination");
        assert_ne!(decoded_key, "CustomInfo");
        assert_eq!((decoded_dict[0].0.as_str(), decoded_dict[0].1.as_str()), (Some("info"), Some(format!("{code}: stale station ship moniker").as_str())));
    }
}

#[test]
fn item_qty_checked_construction_arithmetic_narrowing_and_serde() {
    assert_eq!((ItemQty::MIN.get(), ItemQty::ONE.as_i64(), ItemQty::STACK_MAX.get(), ItemQty::MAX.get()), (1, 1, i64::from(i32::MAX), i64::MAX));
    assert_eq!(ItemQty::try_from_i64(0), Err(ItemQtyError::NonPositive(0)));
    assert_eq!(ItemQty::try_from_i64(-7), Err(ItemQtyError::NonPositive(-7)));
    assert_eq!(ItemQty::try_from_i64(i64::MIN), Err(ItemQtyError::NonPositive(i64::MIN)));
    assert_eq!(ItemQty::try_from_u64(0), Err(ItemQtyError::Zero));
    let over_signed = u64::try_from(i64::MAX).unwrap() + 1;
    assert_eq!(ItemQty::try_from_u64(over_signed), Err(ItemQtyError::ExceedsMax(over_signed)));
    assert_eq!((ItemQty::try_from_i32(0), ItemQty::try_from_u32(0), ItemQty::try_from_usize(0)), (Err(ItemQtyError::NonPositive(0)), Err(ItemQtyError::Zero), Err(ItemQtyError::Zero)));
    let stack_max = ItemQty::try_from_i32(i32::MAX).expect("i32::MAX valid");
    assert_eq!((stack_max, stack_max.try_into_stack_i32(), i32::try_from(stack_max)), (ItemQty::STACK_MAX, Ok(i32::MAX), Ok(i32::MAX)));
    let above_stack = stack_max.checked_add(ItemQty::ONE).expect("i32::MAX + 1 fits i64");
    assert_eq!(above_stack.try_into_stack_i32(), Err(ItemQtyError::ExceedsStackMax(i64::from(i32::MAX) + 1)));
    let u32_max_qty = ItemQty::try_from_u32(u32::MAX).expect("u32::MAX fits i64");
    assert_eq!(u32_max_qty.try_into_u32(), Ok(u32::MAX));
    assert_eq!(u32_max_qty.checked_add(ItemQty::ONE).unwrap().try_into_u32(), Err(ItemQtyError::Overflow));
    assert_eq!(ItemQty::MAX.checked_add(ItemQty::ONE), Err(ItemQtyError::Overflow));
    let five = ItemQty::try_from(5_i64).unwrap();
    let two = ItemQty::try_from(2_u64).unwrap();
    assert_eq!((five.checked_sub(two), two.checked_sub(two), two.checked_sub(five)), (Some(ItemQty::try_from(3_i32).unwrap()), None, None));
    assert_eq!((five.checked_mul(two), ItemQty::MAX.checked_mul(two), five.checked_mul_u64(0), five.checked_mul_u64(4)), (Ok(ItemQty::try_from(10_u32).unwrap()), Err(ItemQtyError::Overflow), Err(ItemQtyError::Zero), Ok(ItemQty::try_from(20_i64).unwrap())));
    assert_eq!((u64::from(five), i64::from(five), five.to_string(), UserErrorArg::from(five)), (5, 5, "5".to_owned(), UserErrorArg::Int(5)));
    let json = serde_json::to_string(&five).expect("serialize ItemQty");
    assert_eq!(json, "5");
    assert_eq!(serde_json::from_str::<ItemQty>(&json).expect("deserialize ItemQty"), five);
    assert!(serde_json::from_str::<ItemQty>("0").is_err() && serde_json::from_str::<ItemQty>("-3").is_err());
    let custom_err = ItemQtyError::ExceedsStackMax(i64::from(i32::MAX) + 1).into_rpc_error("InvalidCommodityQuantity");
    assert_eq!(custom_err.code, "InvalidCommodityQuantity");
    assert!(matches!(custom_err.client_error.as_deref(), Some(ClientError::CustomInfo(_))));
    let default_rpc = ItemQtyError::Zero.into_rpc_error("InvalidQuantity");
    let helper_rpc = BridgeRpcError::invalid_quantity("bad qty");
    assert_eq!((default_rpc.code.as_str(), helper_rpc.code.as_str(), helper_rpc.message.as_str()), ("InvalidQuantity", "InvalidQuantity", "bad qty"));
}

fn dict_has_key(entries: &[(PyRep, PyRep)], key: &str) -> bool {
    entries.iter().any(|(k, _)| k.as_str() == Some(key))
}

fn dict_get<'a>(entries: &'a [(PyRep, PyRep)], key: &str) -> Option<&'a PyRep> {
    entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

#[test]
fn gh1195_all_17_error_codes_never_fall_back_to_custom_info_and_supply_v24_dict_keys() {
    use UserErrorKey as K;
    use UserErrorPresentation as P;
    let expected: &[(&str, K, P, &[&str])] = &[
        ("QuantumCoreRequired", K::CannotUseFittingInStructureWithoutDeed, P::Notify, &["structureName"]),
        ("ShipAlreadyInWarp", K::ShipInWarp, P::Notify, &[]),
        ("ShipInWarp", K::ShipInWarp, P::Notify, &[]),
        ("NoAmmoLoaded", K::NoCharges, P::Notify, &["launcher"]),
        ("NotEnoughCapacitor", K::NotEnoughEnergy, P::Notify, &["effectname", "need", "got"]),
        ("ShipNotInSpace", K::CantShipNotPresent, P::Notify, &[]),
        ("FittingValidationFailed", K::ModuleFitFailed, P::Modal, &["moduleName", "reason"]),
        ("CannotFitStackedModule", K::CannotPerformOnMultipleItems, P::Modal, &[]),
        ("InvalidTarget", K::InvalidTargetForModule, P::Notify, &["moduleName"]),
        ("InventoryNotAccessible", K::AccessDenied, P::Notify, &[]),
        ("MustBeDockedAtStartStation", K::MustBeDocked, P::Notify, &[]),
        ("StructureNotDockable", K::NotDockable, P::Modal, &[]),
        ("StructureServiceAccessDenied", K::StructureServiceUnavailable, P::Notify, &["serviceName", "structureName"]),
        ("WeaponsTimerActive", K::CantDockAfterAggression, P::Notify, &[]),
        ("CannotJumpWithWeaponsTimer", K::CantJumpAfterAggression, P::Notify, &[]),
        ("InvalidContractState", K::ConContractNotOutstanding, P::Modal, &[]),
        ("ModuleNotFound", K::ItemNotFound, P::Modal, &[]),
        ("OwnershipOrLocationViolation", K::CantDoThatWithSomeoneElsesStuff, P::Notify, &[]),
        ("ContractOwnershipViolation", K::AccessDenied, P::Notify, &[]),
        ("InsufficientRole", K::InsufficientRoles, P::Notify, &[]),
        ("GM_InsufficientRole", K::InsufficientRoles, P::Notify, &[]),
        ("GMRoleRequired", K::InsufficientRoles, P::Notify, &[]),
        ("GmRoleRequired", K::InsufficientRoles, P::Notify, &[]),
    ];

    for &(code, expected_key, expected_pres, req_keys) in expected {
        let untyped = BridgeRpcError { code: code.into(), message: format!("test {code}"), client_error: None };
        let ce = untyped.to_client_error("contractProxy", "AcceptContract");
        assert!(!matches!(ce, ClientError::CustomInfo(_)), "{code} fell back to CustomInfo: {ce:?}");
        let (actual_key, _) = ce.as_user_error().unwrap_or_else(|| panic!("{code} not UserError: {ce:?}"));
        assert_eq!((actual_key, ce.presentation()), (expected_key, expected_pres), "mismatch for {code}");
        let (wire_key, wire_dict) = decode_wrapped_user_error(&ce.to_macho_error_body(6));
        assert_eq!(wire_key, expected_key.as_str());
        for &req in req_keys {
            assert!(dict_has_key(&wire_dict, req), "{code} ({wire_key}) missing '{req}' in {wire_dict:?}");
        }
    }
}

#[test]
fn gh1195_new_user_error_keys_have_verified_v24_dialog_metadata() {
    use UserErrorKey as K;
    let expected: &[(K, &str, u32, u8)] = &[
        (K::CannotUseFittingInStructureWithoutDeed, "CannotUseFittingInStructureWithoutDeed", 7470, 3),
        (K::CannotRepairInStructureWithoutDeed, "CannotRepairInStructureWithoutDeed", 7469, 3),
        (K::StructureServiceUnavailable, "StructureServiceUnavailable", 4986, 3),
        (K::StructureDockingDenied, "StructureDockingDenied", 4886, 3),
        (K::NotDockable, "NotDockable", 1334, 2),
        (K::NotEnoughEnergy, "NotEnoughEnergy", 1344, 3),
        (K::JumpNotEnoughCap, "JumpNotEnoughCap", 1036, 2),
        (K::CantShipNotPresent, "CantShipNotPresent", 422, 3),
        (K::ModuleFitFailed, "ModuleFitFailed", 1228, 2),
        (K::CannotPerformOnMultipleItems, "CannotPerformOnMultipleItems", 2271, 2),
        (K::InvalidTargetForModule, "InvalidTargetForModule", 7824, 3),
        (K::DeniedTargetSelf, "DeniedTargetSelf", 788, 3),
        (K::CantDockAfterAggression, "CantDockAfterAggression", 377, 3),
        (K::CantJumpAfterAggression, "CantJumpAfterAggression", 385, 3),
        (K::ConContractNotOutstanding, "ConContractNotOutstanding", 2015, 2),
        (K::ConContractNotInProgress, "ConContractNotInProgress", 2085, 2),
        (K::ConContractInvalid, "ConContractInvalid", 5048, 2),
        (K::ConAuctionHasBids, "ConAuctionHasBids", 2045, 2),
        (K::ModuleNoLongerPresentForCharges, "ModuleNoLongerPresentForCharges", 1230, 3),
    ];

    for &(key, name, dialog_id, dialog_type) in expected {
        assert_eq!(key.as_str(), name);
        assert_eq!(K::from_code(name), Some(key));
        assert_eq!((key.v24_dialog_id(), key.v24_dialog_type()), (Some(dialog_id), Some(dialog_type)));
        let (wire_key, wire_dict) = decode_wrapped_user_error(&ClientError::user(key).to_macho_error_body(6));
        assert_eq!(wire_key, name);
        for (def_k, _) in key.default_args() {
            assert!(dict_has_key(&wire_dict, &def_k), "{name} missing '{def_k}' on wire");
        }
    }
    assert!(K::InvalidTargetGroup.default_args().iter().any(|(k, _)| k == "groupName"));
    let (itg_key, itg_dict) = decode_wrapped_user_error(&ClientError::user(K::InvalidTargetGroup).to_macho_error_body(6));
    assert_eq!(itg_key, "InvalidTargetGroup");
    assert!(dict_has_key(&itg_dict, "groupName"));
}

#[test]
fn gh1195_typed_rejection_constructors_preserve_facts_and_wire_payloads_across_serde() {
    use BridgeRpcError as E;
    use RejectionFacts as F;
    use UserErrorKey as K;
    let cases: Vec<(E, &str, K, F)> = vec![
        (E::quantum_core_required_fitting(1001, "Jita Keepstar", "missing core"), "QuantumCoreRequired", K::CannotUseFittingInStructureWithoutDeed, F::QuantumCoreRequired { structure_id: 1001, service: "fitting".into() }),
        (E::quantum_core_required_repair(1002, "Perimeter Fortizar", "missing core"), "QuantumCoreRequired", K::CannotRepairInStructureWithoutDeed, F::QuantumCoreRequired { structure_id: 1002, service: "repair".into() }),
        (E::quantum_core_required_service(1003, "Amarr Astrahus", "service modules", "missing core"), "QuantumCoreRequired", K::StructureServiceUnavailable, F::QuantumCoreRequired { structure_id: 1003, service: "service modules".into() }),
        (E::ship_already_in_warp(Some(501), "CmdGotoPoint", Some("Cruising".into()), "already in warp"), "ShipAlreadyInWarp", K::ShipInWarp, F::ShipInWarp { ship_id: Some(501), action: "CmdGotoPoint".into(), warp_phase: Some("Cruising".into()) }),
        (E::ship_in_warp(Some(502), "CmdBeaconJump", Some("Accelerating".into()), "in warp"), "ShipInWarp", K::ShipInWarp, F::ShipInWarp { ship_id: Some(502), action: "CmdBeaconJump".into(), warp_phase: Some("Accelerating".into()) }),
        (E::no_ammo_loaded(Some(901), Some(2410), Some(85), None, "no ammo"), "NoAmmoLoaded", K::NoCharges, F::NoAmmoLoaded { module_id: Some(901), launcher_type_id: Some(2410), required_charge_group: Some(85) }),
        (E::not_enough_capacitor(Some(501), "Light Neutron Blaster II", 42.5, 10.25, "low cap"), "NotEnoughCapacitor", K::NotEnoughEnergy, F::NotEnoughCapacitor { entity_id: Some(501), effect_name: "Light Neutron Blaster II".into(), required_gj: 42.5, available_gj: 10.25 }),
        (E::jump_not_enough_capacitor(Some(501), 320.0, 120.0, "low jump cap"), "NotEnoughCapacitor", K::JumpNotEnoughCap, F::NotEnoughCapacitor { entity_id: Some(501), effect_name: "Jump".into(), required_gj: 320.0, available_gj: 120.0 }),
        (E::ship_not_in_space(501, Some(9001), Some(30000142), "Activate", "not in space"), "ShipNotInSpace", K::CantShipNotPresent, F::ShipNotInSpace { ship_id: 501, char_id: Some(9001), solar_system_id: Some(30000142), action: "Activate".into() }),
        (E::fitting_validation_failed(Some(501), Some(701), Some(2048), "Damage Control II", "Only one Damage Control may be fitted"), "FittingValidationFailed", K::ModuleFitFailed, F::FittingValidationFailed { ship_id: Some(501), item_id: Some(701), module_type_id: Some(2048), reason: "Only one Damage Control may be fitted".into() }),
        (E::cannot_fit_stacked_module(Some(501), Some(702), 5, "stacked"), "CannotFitStackedModule", K::CannotPerformOnMultipleItems, F::CannotFitStackedModule { ship_id: Some(501), item_id: Some(702), quantity: 5 }),
        (E::invalid_target(Some(888), Some(901), "Stasis Webifier II", "out of system", "bad target"), "InvalidTarget", K::InvalidTargetForModule, F::InvalidTarget { target_id: Some(888), module_id: Some(901), reason: "out of system".into() }),
        (E::invalid_navigation_target(Some(889), "bookmark_wrong_solar_system", "wrong sys"), "InvalidTarget", K::WarpDestinationGone, F::InvalidTarget { target_id: Some(889), module_id: None, reason: "bookmark_wrong_solar_system".into() }),
        (E::invalid_target_group(Some(890), "Frigate", "not_shipping_unit_or_container", "bad group"), "InvalidTarget", K::InvalidTargetGroup, F::InvalidTarget { target_id: Some(890), module_id: None, reason: "not_shipping_unit_or_container".into() }),
        (E::invalid_target_self(501, "self_orbit", "cannot orbit own ship"), "InvalidTarget", K::DeniedTargetSelf, F::InvalidTarget { target_id: Some(501), module_id: None, reason: "self_orbit".into() }),
        (E::inventory_not_accessible(Some(703), Some(9001), Some(60003760), "acl denied"), "InventoryNotAccessible", K::AccessDenied, F::InventoryNotAccessible { item_id: Some(703), char_id: Some(9001), location_id: Some(60003760) }),
        (E::must_be_docked_at_start_station(Some(4001), 60003760, Some(60008494), "wrong station"), "MustBeDockedAtStartStation", K::MustBeDocked, F::MustBeDockedAtStartStation { contract_id: Some(4001), required_station_id: 60003760, caller_station_id: Some(60008494) }),
        (E::structure_not_dockable(1001, "Anchoring", "cannot dock"), "StructureNotDockable", K::NotDockable, F::StructureNotDockable { structure_id: 1001, state: "Anchoring".into() }),
        (E::structure_service_access_denied(1001, 9001, "Jita Citadel", "market", "acl_denied", "denied"), "StructureServiceAccessDenied", K::StructureServiceUnavailable, F::StructureServiceAccessDenied { structure_id: 1001, char_id: 9001, service: "market".into(), reason: "acl_denied".into() }),
        (E::structure_service_access_denied(1001, 9001, "Jita Citadel", "docking", "acl_denied", "dock denied"), "StructureServiceAccessDenied", K::StructureDockingDenied, F::StructureServiceAccessDenied { structure_id: 1001, char_id: 9001, service: "docking".into(), reason: "acl_denied".into() }),
        (E::weapons_timer_active(9001, "shipcaster_jump", "weapons", true, "timer"), "WeaponsTimerActive", K::CantJumpAfterAggression, F::WeaponsTimerActive { char_id: 9001, action: "shipcaster_jump".into(), timer_kind: "weapons".into() }),
        (E::cannot_jump_with_weapons_timer(9001, "stargate_jump", "weapons", "timer"), "CannotJumpWithWeaponsTimer", K::CantJumpAfterAggression, F::WeaponsTimerActive { char_id: 9001, action: "stargate_jump".into(), timer_kind: "weapons".into() }),
        (E::invalid_contract_state(4001, "Outstanding", "Finished", "wrong state"), "InvalidContractState", K::ConContractNotOutstanding, F::InvalidContractState { contract_id: 4001, expected_state: "Outstanding".into(), actual_state: "Finished".into() }),
        (E::invalid_contract_state(4002, "InProgress", "Outstanding", "not in progress"), "InvalidContractState", K::ConContractNotInProgress, F::InvalidContractState { contract_id: 4002, expected_state: "InProgress".into(), actual_state: "Outstanding".into() }),
        (E::invalid_contract_state(4003, "NoBids", "HasBids", "auction has bids"), "InvalidContractState", K::ConAuctionHasBids, F::InvalidContractState { contract_id: 4003, expected_state: "NoBids".into(), actual_state: "HasBids".into() }),
        (E::contract_ownership_violation(4004, 9002, Some(9001), "not party"), "ContractOwnershipViolation", K::AccessDenied, F::OwnershipOrLocationViolation { item_id: 4004, owner_id: Some(9002), location_id: None, required_owner_id: Some(9001), required_location_id: None }),
        (E::module_not_found(901, Some(501), Some(3170), false, "missing"), "ModuleNotFound", K::ItemNotFound, F::ModuleNotFound { module_id: 901, owner_entity_id: Some(501), type_id: Some(3170) }),
        (E::ownership_or_location_violation(705, Some(9002), Some(60003760), Some(9001), Some(501), "not owner"), "OwnershipOrLocationViolation", K::CantDoThatWithSomeoneElsesStuff, F::OwnershipOrLocationViolation { item_id: 705, owner_id: Some(9002), location_id: Some(60003760), required_owner_id: Some(9001), required_location_id: Some(501) }),
        (E::insufficient_role(0x0100_0000_0000_0000, 0x6000_0000_0000_0000, "machoNet.GetMetrics", "gm role required"), "InsufficientRole", K::InsufficientRoles, F::InsufficientRole { required_role_mask: 0x0100_0000_0000_0000, actual_role_mask: 0x6000_0000_0000_0000, action: "machoNet.GetMetrics".into() }),
        (E::insufficient_role_with_code("GMRoleRequired", 0x0040_0000_0000_0000, 0, "GM_ExpireContract", "gm role required"), "GMRoleRequired", K::InsufficientRoles, F::InsufficientRole { required_role_mask: 0x0040_0000_0000_0000, actual_role_mask: 0, action: "GM_ExpireContract".into() }),
        (E::insufficient_role_with_code("GmRoleRequired", 0x0040_0000_0000_0000, 0, "CmdTurboDock", "gm role required"), "GmRoleRequired", K::InsufficientRoles, F::InsufficientRole { required_role_mask: 0x0040_0000_0000_0000, actual_role_mask: 0, action: "CmdTurboDock".into() }),
    ];

    for (err, expected_code, expected_key, expected_facts) in cases {
        assert_eq!(err.code, expected_code);
        assert_eq!(err.rejection_facts(), Some(&expected_facts));
        let wire = crate::BridgeEnvelope::RpcResponse { req_id: 99, result: Err(err.clone()) }.to_wire_bytes().unwrap();
        let crate::BridgeEnvelope::RpcResponse { result: Err(decoded_err), .. } = crate::BridgeEnvelope::from_wire_bytes(&wire).unwrap() else {
            panic!("expected RpcResponse Err");
        };
        assert_eq!(decoded_err.rejection_facts(), Some(&expected_facts));
        let mp_decoded: E = rmp_serde::from_slice(&rmp_serde::to_vec_named(&decoded_err).unwrap()).unwrap();
        assert_eq!(mp_decoded.rejection_facts(), Some(&expected_facts));
        let ce = decoded_err.to_client_error("contractProxy", "AcceptContract");
        assert_eq!(ce.rejection_facts(), Some(&expected_facts));
        assert_eq!(ce.as_user_error().expect("UserError").0, expected_key);
        let (wire_key, wire_dict) = decode_wrapped_user_error(&ce.to_macho_error_body(6));
        assert_eq!(wire_key, expected_key.as_str());
        for (def_k, _) in expected_key.default_args() {
            assert!(dict_has_key(&wire_dict, &def_k), "{expected_key:?} missing '{def_k}'");
        }
    }

    let cap = E::not_enough_capacitor(Some(501), "Microwarpdrive", 150.0, 22.5, "low cap");
    let (_, cap_dict) = decode_wrapped_user_error(&cap.to_client_error("dogmaIM", "Activate").to_macho_error_body(6));
    assert_eq!(dict_get(&cap_dict, "effectname").and_then(PyRep::as_str), Some("Microwarpdrive"));
    assert_eq!(dict_get(&cap_dict, "need"), Some(&PyRep::Real(150.0)));
    assert_eq!(dict_get(&cap_dict, "got"), Some(&PyRep::Real(22.5)));

    let grp_err = E::invalid_target_group(Some(890), "Frigate", "not_shipping_unit_or_container", "bad group");
    let (_, grp_dict) = decode_wrapped_user_error(&grp_err.to_client_error("entity", "OpenShippingUnitStorage").to_macho_error_body(6));
    assert_eq!(dict_get(&grp_dict, "groupName").and_then(PyRep::as_str), Some("Frigate"));

    let slash_denied = E::insufficient_role(0x1, 0, "slash.SlashCmd", "GM required");
    assert_eq!(slash_denied.rejection_facts(), Some(&F::InsufficientRole { required_role_mask: 1, actual_role_mask: 0, action: "slash.SlashCmd".into() }));
    let (slash_key, slash_dict) = decode_wrapped_user_error(&slash_denied.to_client_error("slash", "SlashCmd").to_macho_error_body(6));
    assert_eq!(slash_key, "SlashError");
    assert_eq!(dict_get(&slash_dict, "reason").and_then(PyRep::as_str), Some("InsufficientRole: GM required"));

    // #1569 / #1568: ModuleFitFailed defaults and untyped/empty-name FittingValidationFailed never emit empty moduleName.
    for args in [
        K::ModuleFitFailed.default_args(),
        K::ModuleFitFailed.default_args_with_message("Insufficient CPU: need 15, available 5"),
    ] {
        let name = args.iter().find(|(k, _)| k == "moduleName").and_then(|(_, v)| v.as_str());
        assert_eq!(name, Some("Module"));
    }
    let legacy_fit_err = E {
        code: "FittingValidationFailed".into(),
        message: "Insufficient CPU: need 15, available 5".into(),
        client_error: None,
    };
    let (fit_key, fit_dict) = decode_wrapped_user_error(
        &legacy_fit_err.to_client_error("invbroker", "Add").to_macho_error_body(6),
    );
    assert_eq!(fit_key, "ModuleFitFailed");
    assert_eq!(dict_get(&fit_dict, "moduleName").and_then(PyRep::as_str), Some("Module"));
    assert_eq!(
        dict_get(&fit_dict, "reason").and_then(PyRep::as_str),
        Some("Insufficient CPU: need 15, available 5")
    );

    let typed_ab = E::fitting_validation_failed(
        Some(501),
        Some(701),
        Some(439),
        "1MN Afterburner I",
        "Insufficient CPU: need 15, available 5",
    );
    let (ab_key, ab_dict) =
        decode_wrapped_user_error(&typed_ab.to_client_error("invbroker", "Add").to_macho_error_body(6));
    assert_eq!(ab_key, "ModuleFitFailed");
    assert_eq!(dict_get(&ab_dict, "moduleName").and_then(PyRep::as_str), Some("1MN Afterburner I"));
    assert_eq!(
        dict_get(&ab_dict, "reason").and_then(PyRep::as_str),
        Some("Insufficient CPU: need 15, available 5")
    );

    let empty_name_ab = E::fitting_validation_failed(
        Some(501),
        Some(701),
        Some(439),
        "",
        "Insufficient CPU: need 15, available 5",
    );
    let (_, empty_ab_dict) = decode_wrapped_user_error(
        &empty_name_ab.to_client_error("invbroker", "Add").to_macho_error_body(6),
    );
    assert_eq!(dict_get(&empty_ab_dict, "moduleName").and_then(PyRep::as_str), Some("Module"));
}
