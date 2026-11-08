//! Unit tests for `inventory_fitting` (`csprng_unit_f64`, `invbroker.GetItemDescriptor`, `invbroker.List`, `invbroker.ListByFlags`, `invbroker.Add` for cargo & module fitting, and fallback routing).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn inventory_fitting_get_item_descriptor_list_and_add_module_and_cargo() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let u = csprng_unit_f64(&ctx);
    assert!((0.0..1.0).contains(&u));

    let char_id = 96_500_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_500_100i64;
    let mod_id = 96_500_200i64;
    let cargo_id = 96_500_300i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "InvFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_id,
            InventoryItemRecord {
                item_id: cargo_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    // 1. GetItemDescriptor
    let desc = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "GetItemDescriptor",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetItemDescriptor ok");
    assert_ne!(desc, PyRep::None);

    // 2. List & ListByFlags
    let list_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("List ok");
    assert_ne!(list_res, PyRep::None);

    let list_flags_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "ListByFlags",
        &[PyRep::List(vec![PyRep::Integer(InventoryFlag::Hangar as i64)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ListByFlags ok");
    assert_ne!(list_flags_res, PyRep::None);

    // 3. Add module to HiSlot0 (flag 27)
    let fit_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("Add module ok");
    assert_ne!(fit_res, PyRep::None);
    let mod_loc = ctx
        .db
        .with_snapshot(|s| s.items.get(&mod_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(mod_loc, Some((ship_id, InventoryFlag::HiSlot0)));

    // 4. Add split stack of Tritanium to ship Cargo (flag 5)
    let mut kwargs = HashMap::new();
    kwargs.insert("qty".into(), PyRep::Integer(200));
    let cargo_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(cargo_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect("Add cargo split ok");
    assert_ne!(cargo_res, PyRep::None);
    let rem_qty = ctx
        .db
        .with_snapshot(|s| s.items.get(&cargo_id).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(300));
}

#[tokio::test]
async fn r9_p0_549_director_take_and_invbroker_add_refuse_impounded_locked_and_unpaid_asset_safety() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 96_500_900_i64;
    let kw = HashMap::new();
    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        corp_role: eve_db::corp_roles::ROLE_DIRECTOR,
        ..Default::default()
    };

    let impounded_corp_item = 96_600_901_i64;
    let locked_corp_item = 96_600_902_i64;
    let safety_wrap_item = 96_600_903_i64;
    let safety_wrap_id = 96_600_950_i64;
    let paid_wrap_id = 96_600_951_i64;
    let paid_delivery_item = 96_600_904_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = char_id;
            corp.member_roles.insert(char_id, u64::MAX);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            impounded_corp_item,
            InventoryItemRecord {
                item_id: impounded_corp_item,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Impounded,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            locked_corp_item,
            InventoryItemRecord {
                item_id: locked_corp_item,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            safety_wrap_item,
            InventoryItemRecord {
                item_id: safety_wrap_item,
                type_id: 34,
                owner_id: corp_id,
                location_id: safety_wrap_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            safety_wrap_id,
            InventoryItemRecord {
                item_id: safety_wrap_id,
                type_id: 60,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Deliveries,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            paid_delivery_item,
            InventoryItemRecord {
                item_id: paid_delivery_item,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Deliveries,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            96_600_980,
            crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: 96_600_980,
                owner_id: corp_id,
                is_corp: true,
                source_structure_id: 1_000_000_000_001,
                source_structure_name: "Keepstar".into(),
                solar_system_id: 30_000_142,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: Some(station_id),
                item_ids: vec![safety_wrap_item],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: Some(station_id),
                auto_delivered: true,
                retrieval_fee_paid: false,
            },
        );
        ext.asset_safety_wraps.insert(
            paid_wrap_id,
            crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: paid_wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: 1_000_000_000_001,
                source_structure_name: "Keepstar".into(),
                solar_system_id: 30_000_142,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: Some(station_id),
                item_ids: vec![paid_delivery_item],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: Some(station_id),
                auto_delivered: false,
                retrieval_fee_paid: true,
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                &ctx,
                snap,
                char_id,
                corp_id,
                &snap.items[&impounded_corp_item],
            )
            .unwrap_err()
            .code,
            "ImpoundedItem"
        );
        assert_eq!(
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                &ctx,
                snap,
                char_id,
                corp_id,
                &snap.items[&locked_corp_item],
            )
            .unwrap_err()
            .code,
            "ItemLocked"
        );
        assert_eq!(
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                &ctx,
                snap,
                char_id,
                corp_id,
                &snap.items[&safety_wrap_item],
            )
            .unwrap_err()
            .code,
            "AssetSafetyLocked"
        );
    });

    for (id, expected_code) in [
        (impounded_corp_item, "ImpoundedItem"),
        (locked_corp_item, "ItemLocked"),
        (safety_wrap_item, "AssetSafetyLocked"),
    ] {
        let err = handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code);

        let m_err = handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(id)]),
                PyRep::Integer(ship_id),
                PyRep::Integer(5),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(m_err.code, expected_code);
    }

    for bad_flag in [36_i64, 63, 72] {
        let err = handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(paid_delivery_item)]),
                PyRep::Integer(station_id),
                PyRep::Integer(bad_flag),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(err.code.as_str(), "ImpoundedItem" | "AssetSafetyLocked" | "ItemLocked"),
            "unexpected error code for flag {bad_flag}: {}",
            err.code
        );
    }

    handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(paid_delivery_item),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("paid delivery item moves to Hangar");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&paid_delivery_item].flag),
        InventoryFlag::Hangar
    );
}

#[tokio::test]
async fn issue_630_fitting_module_sets_singleton_and_emits_dbrow_item_change() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_500_630i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_500_631i64;
    let stack_mod_id = 96_500_632i64;
    let single_mod_id = 96_500_633i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "Issue630Pilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Stacked unpackaged modules (quantity=2, singleton=false) and single unpackaged module (quantity=1, singleton=false)
        snap.items.insert(
            stack_mod_id,
            InventoryItemRecord {
                item_id: stack_mod_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            single_mod_id,
            InventoryItemRecord {
                item_id: single_mod_id,
                type_id: 484,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(1, 9630, Some(char_id), true, ctx.now_unix())
        .expect("char online");
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut session = SessionState {
        session_id: 9630,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Fit 1 module from the stack of 2 into HiSlot1 (28)
    let rep1 = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(stack_mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(28),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("fit split module");
    let fitted1_id = rep1.dict_get("itemID").and_then(PyRep::as_i64).expect("itemID");
    assert_ne!(fitted1_id, stack_mod_id);
    ctx.db.with_snapshot(|s| {
        let fitted = &s.items[&fitted1_id];
        assert!(fitted.singleton, "fitted split module must have singleton=true");
        assert_eq!(fitted.quantity, 1);
        assert_eq!(fitted.flag, InventoryFlag::HiSlot1);
        assert_eq!(s.items[&stack_mod_id].quantity, 1);
    });

    let notifs1 = ctx.drain_notifications_for_char(char_id);
    let int_dict_get = |rep: Option<&PyRep>, k: i64| -> Option<i64> {
        let Some(PyRep::Dict(entries)) = rep else { return None; };
        entries.iter().find(|(dk, _)| dk.as_i64() == Some(k)).and_then(|(_, v)| v.as_i64())
    };
    let orig_change = notifs1
        .iter()
        .find(|n| {
            n.event == "OnItemChange"
                && n.args.first().and_then(|r| r.dict_get("itemID")).and_then(PyRep::as_i64) == Some(stack_mod_id)
        })
        .expect("remaining hangar stack must emit OnItemChange");
    assert!(matches!(orig_change.args.first(), Some(PyRep::DBRow { .. })));
    assert_eq!(orig_change.args[0].dict_get("quantity").and_then(PyRep::as_i64), Some(1));
    assert_eq!(
        int_dict_get(orig_change.args.get(1), 9),
        Some(2),
        "remaining hangar stack change dict must use ixStackSize=9 with old qty"
    );

    let fit_change1 = notifs1
        .iter()
        .find(|n| {
            n.event == "OnItemChange"
                && n.args.first().and_then(|r| r.dict_get("itemID")).and_then(PyRep::as_i64) == Some(fitted1_id)
        })
        .expect("fitted module must emit OnItemChange");
    assert!(matches!(fit_change1.args.first(), Some(PyRep::DBRow { .. })));
    assert_eq!(fit_change1.args[0].dict_get("ownerID").and_then(PyRep::as_i64), Some(char_id));
    assert_eq!(fit_change1.args[0].dict_get("categoryID").and_then(PyRep::as_i64), Some(7));
    assert_eq!(
        fit_change1.args[0].dict_get("quantity").and_then(PyRep::as_i64),
        Some(-1),
        "fitted module DBRow quantity must be -1 so godma sees item.singleton == True"
    );
    assert_eq!(
        int_dict_get(fit_change1.args.get(1), 10),
        Some(1),
        "fitted module OnItemChange must record ixSingleton=10 change"
    );

    // 2. Fit the standalone unpackaged module (qty=1, singleton=false) into HiSlot2 (29)
    handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(single_mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(29),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("fit standalone unpackaged module");
    ctx.db.with_snapshot(|s| {
        let fitted2 = &s.items[&single_mod_id];
        assert!(fitted2.singleton, "fitted standalone module must have singleton=true");
        assert_eq!(fitted2.quantity, 1);
        assert_eq!(fitted2.flag, InventoryFlag::HiSlot2);
    });
    let notifs2 = ctx.drain_notifications_for_char(char_id);
    let fit_change2 = notifs2
        .iter()
        .find(|n| {
            n.event == "OnItemChange"
                && n.args.first().and_then(|r| r.dict_get("itemID")).and_then(PyRep::as_i64) == Some(single_mod_id)
        })
        .expect("fitted standalone module must emit OnItemChange");
    assert_eq!(fit_change2.args[0].dict_get("quantity").and_then(PyRep::as_i64), Some(-1));
    assert_eq!(fit_change2.args[0].dict_get("flagID").and_then(PyRep::as_i64), Some(29));
}

#[tokio::test]
async fn invbroker_add_bound_ship_ignores_source_arg_and_autofits_slot_0() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_510_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_510_100i64;
    let gun1_id = 96_510_201i64;
    let gun2_id = 96_510_202i64;
    let gun3_id = 96_510_203i64;
    let gun4_id = 96_510_204i64;
    let cargo_id = 96_510_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "AutoFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let sk_map = snap.skills.entry(char_id).or_default();
        for sk in [3300_u32, 3301, 3302, 3426] {
            sk_map.insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (3 high slots)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for gid in [gun1_id, gun2_id, gun3_id, gun4_id] {
            snap.items.insert(
                gid,
                InventoryItemRecord {
                    item_id: gid,
                    type_id: 484, // 125mm Gatling AutoCannon I
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            cargo_id,
            InventoryItemRecord {
                item_id: cargo_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::new(96_510);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    let ship_bound = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: session.session_id,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: ship_id,
        oid_str: "bound_ship".into(),
        ref_id: 1,
    };
    let kw = HashMap::new();

    // 1. Bound ship target ignores mismatched args[1] and resolves flag=0 to HiSlot0 (27), setting singleton=true
    handle(
        &ctx,
        &mut session,
        Some(&ship_bound),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(gun1_id),
            PyRep::Integer(station_id + 999),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("autofit gun1");
    ctx.db.with_snapshot(|snap| {
        let g1 = &snap.items[&gun1_id];
        assert_eq!(g1.location_id, ship_id);
        assert_eq!(g1.flag, InventoryFlag::HiSlot0);
        assert!(g1.singleton, "fitted module must be singleton");
    });

    // 2. Subsequent flag=0 calls advance to HiSlot1 (28) and HiSlot2 (29)
    for (gid, expected_flag) in [
        (gun2_id, InventoryFlag::HiSlot1),
        (gun3_id, InventoryFlag::HiSlot2),
    ] {
        handle(
            &ctx,
            &mut session,
            Some(&ship_bound),
            "invbroker",
            "Add",
            &[PyRep::Integer(gid), PyRep::Integer(station_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("autofit next gun");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&gid].flag), expected_flag);
    }

    // 3. When all high slots on the Rifter are occupied, flag=0 returns NoFreeSlot
    let full_err = handle(
        &ctx,
        &mut session,
        Some(&ship_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(gun4_id), PyRep::Integer(ship_id), PyRep::Integer(0)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("full high rack must reject");
    assert_eq!(full_err.code, "NoFreeSlot");

    // 4. Non-module cargo item with flag=0 on ship resolves to Cargo (5)
    handle(
        &ctx,
        &mut session,
        Some(&ship_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(cargo_id), PyRep::Integer(ship_id), PyRep::Integer(0)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("autofit cargo");
    assert_eq!(
        ctx.db.with_snapshot(|s| (s.items[&cargo_id].location_id, s.items[&cargo_id].flag)),
        (ship_id, InventoryFlag::Cargo)
    );
}

#[tokio::test]
async fn bound_ship_add_with_station_arg_fits_into_ship_and_emits_full_dbrow_item_change() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_500_501i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_500_600i64;
    let gun_stack_id = 96_500_601i64;
    let gun_cargo_id = 96_500_602i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "BoundAddPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_stack_id,
            InventoryItemRecord {
                item_id: gun_stack_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Module already inside ship cargo while V24 client still passes sourceLocationID = station_id
        snap.items.insert(
            gun_cargo_id,
            InventoryItemRecord {
                item_id: gun_cargo_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let _ = ctx
        .db
        .acquire_online_lock(1, 1, Some(char_id), true, ctx.now_unix_secs());

    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let ship_bound = crate::router::BoundObjectEntry {
        bound_id: 22_001,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: ship_id,
        oid_str: format!("N=22001:{ship_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. V24 drag-to-slot: ship_inv.Add(gun_stack_id, station_id, qty=1, flag=27)
    let res1 = handle(
        &ctx,
        &mut session,
        Some(&ship_bound),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(gun_stack_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("bound ship Add fits split module into HiSlot0");
    let fitted1_id = res1.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(res1.dict_get("locationID").and_then(|v| v.as_i64()), Some(ship_id));
    assert_eq!(
        res1.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::HiSlot0 as i64)
    );

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&gun_stack_id].quantity, 4);
        let fitted = &snap.items[&fitted1_id];
        assert_eq!(fitted.location_id, ship_id);
        assert_eq!(fitted.flag, InventoryFlag::HiSlot0);
        assert!(fitted.singleton);
    });

    let notifs1 = ctx.drain_notifications_for_char(char_id);
    let ic1: Vec<_> = notifs1.iter().filter(|n| n.event == "OnItemChange").collect();
    assert!(ic1.iter().any(|n| matches!(
        &n.args[0],
        PyRep::DBRow { columns, .. }
            if columns.iter().any(|(k, v)| k == "itemID" && v.as_i64() == Some(gun_stack_id))
            && columns.iter().any(|(k, v)| k == "quantity" && v.as_i64() == Some(4))
            && columns.iter().any(|(k, v)| k == "categoryID" && v.as_i64() == Some(7))
    )));
    assert!(ic1.iter().any(|n| matches!(
        &n.args[0],
        PyRep::DBRow { columns, .. }
            if columns.iter().any(|(k, v)| k == "itemID" && v.as_i64() == Some(fitted1_id))
            && columns.iter().any(|(k, v)| k == "locationID" && v.as_i64() == Some(ship_id))
            && columns.iter().any(|(k, v)| k == "flagID" && v.as_i64() == Some(InventoryFlag::HiSlot0 as i64))
            && columns.iter().any(|(k, v)| k == "quantity" && v.as_i64() == Some(-1))
    )));

    // 2. Even if item already sits in ship cargo while client passes station_id as sourceLocationID,
    // bound ship inventory still resolves target = ship_id (never station_id).
    let res2 = handle(
        &ctx,
        &mut session,
        Some(&ship_bound),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(gun_cargo_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("bound ship Add fits from cargo even when args[1] = station_id");
    assert_eq!(res2.dict_get("locationID").and_then(|v| v.as_i64()), Some(ship_id));
    assert_eq!(
        res2.dict_get("flagID").and_then(|v| v.as_i64()),
        Some(InventoryFlag::HiSlot1 as i64)
    );
}

#[tokio::test]
async fn toctou_concurrent_invbroker_add_slot_and_cargo_cap() {
    let ctx = crate::services::account_services::account_services_tests::test_ctx();
    let char_id = 915_101_i64;
    let corp_id = 98_151_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 915_201_i64;
    let mod_a = 915_301_i64;
    let mod_b = 915_302_i64;
    let ore_a = 915_401_i64;
    let ore_b = 915_402_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .values()
            .next()
            .cloned()
            .expect("seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (cargo capacity ~140 m3)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for mod_id in [mod_a, mod_b] {
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 484, // 125mm Gatling AutoCannon I (HiSlot)
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Two 10,000-unit Tritanium stacks = 100 m3 each; 1 fits in Rifter cargo, 2 exceed capacity.
        for cargo_item in [ore_a, ore_b] {
            snap.items.insert(
                cargo_item,
                InventoryItemRecord {
                    item_id: cargo_item,
                    type_id: 34, // Tritanium (0.01 m3/unit)
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10_000,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut s1 = SessionState::default();
    s1.user_id = Some(char_id as u64);
    s1.character_id = Some(char_id);
    s1.corporation_id = Some(corp_id);
    s1.station_id = Some(station_id);
    s1.solar_system_id = Some(sys_id);
    s1.ship_id = Some(ship_id);
    let mut s2 = s1.clone();
    let kw = HashMap::new();

    // 1. Concurrent invbroker.Add into the same fitting slot HiSlot0: only one wins.
    let fit_args1 = [
        PyRep::Integer(mod_a),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
    ];
    let fit_args2 = [
        PyRep::Integer(mod_b),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
    ];
    let (r1, r2) = tokio::join!(
        handle(&ctx, &mut s1, None, "invbroker", "Add", &fit_args1, &kw),
        handle(&ctx, &mut s2, None, "invbroker", "Add", &fit_args2, &kw),
    );
    let ok_fit_count = [&r1, &r2].iter().filter(|r| matches!(r, Some(Ok(_)))).count();
    assert_eq!(ok_fit_count, 1, "only one concurrent Add may occupy HiSlot0: r1={r1:?} r2={r2:?}");
    let fitted_in_hi0 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0)
            .count()
    });
    assert_eq!(fitted_in_hi0, 1);

    // 2. Concurrent invbroker.Add exceeding ship cargo capacity: at most one fits (100 m3 <= 140 m3 < 200 m3).
    let cargo_args1 = [
        PyRep::Integer(ore_a),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::Cargo as i64),
    ];
    let cargo_args2 = [
        PyRep::Integer(ore_b),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::Cargo as i64),
    ];
    let (c1, c2) = tokio::join!(
        handle(&ctx, &mut s1, None, "invbroker", "Add", &cargo_args1, &kw),
        handle(&ctx, &mut s2, None, "invbroker", "Add", &cargo_args2, &kw),
    );
    let ok_cargo_count = [&c1, &c2].iter().filter(|r| matches!(r, Some(Ok(_)))).count();
    assert_eq!(ok_cargo_count, 1, "only one 100 m3 stack fits in Rifter cargo: c1={c1:?} c2={c2:?}");
    let cargo_qty: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
            .map(|i| i64::from(i.quantity))
            .sum()
    });
    assert_eq!(cargo_qty, 10_000);
}

#[tokio::test]
async fn toctou_concurrent_ship_jettison_enforces_cooldown() {
    let ctx = crate::services::account_services::account_services_tests::test_ctx();
    let char_id = 915_501_i64;
    let corp_id = 98_155_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 915_601_i64;
    let item_a = 915_701_i64;
    let item_b = 915_702_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .values()
            .next()
            .cloned()
            .expect("seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for item_id in [item_a, item_b] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let mut s1 = SessionState::default();
    s1.user_id = Some(char_id as u64);
    s1.character_id = Some(char_id);
    s1.corporation_id = Some(corp_id);
    s1.solar_system_id = Some(sys_id);
    s1.ship_id = Some(ship_id);
    let mut s2 = s1.clone();
    let kw = HashMap::new();

    let j_args1 = [PyRep::List(vec![PyRep::Integer(item_a)])];
    let j_args2 = [PyRep::List(vec![PyRep::Integer(item_b)])];
    let (r1, r2) = tokio::join!(
        handle(&ctx, &mut s1, None, "ship", "Jettison", &j_args1, &kw),
        handle(&ctx, &mut s2, None, "ship", "Jettison", &j_args2, &kw),
    );
    let ok_count = [r1.unwrap(), r2.unwrap()].iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok_count, 1, "only one concurrent Jettison may bypass 180s cooldown");
}

#[tokio::test]
async fn toctou_concurrent_dock_stargate_anchor_filament_loot_hacking() {
    let mut ctx = crate::services::account_services::account_services_tests::test_ctx();
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let station_id = 60_003_760_i64;
    let gate_id = 50_001_991_i64;
    let dest_gate_id = 50_001_992_i64;

    {
        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        if let Some(base_t) = sde_mut.types.values().next().cloned() {
            let mut da = base_t;
            da.type_id = 22_586;
            da.group_id = 538;
            da.name = "Data Analyzer I".into();
            da.published = true;
            da.default_attributes.insert(1909, 60.0);
            da.default_attributes.insert(1910, 30.0);
            da.default_attributes.insert(1911, 3.0);
            da.default_attributes.insert(54, 5_000.0);
            sde_mut.types.insert(22_586, da);
        }
        sde_mut.jumps.insert(
            gate_id,
            eve_sde::StargateJump {
                stargate_id: gate_id,
                from_solar_system_id: sys_a,
                to_solar_system_id: sys_b,
                destination_stargate_id: dest_gate_id,
                position: glam::DVec3::new(1_000.0, 0.0, 0.0),
            },
        );
        sde_mut.jumps.insert(
            dest_gate_id,
            eve_sde::StargateJump {
                stargate_id: dest_gate_id,
                from_solar_system_id: sys_b,
                to_solar_system_id: sys_a,
                destination_stargate_id: gate_id,
                position: glam::DVec3::new(-1_000.0, 0.0, 0.0),
            },
        );
    }

    let dock_char = 916_001_i64;
    let dock_ship = 916_101_i64;
    let gate_char = 916_002_i64;
    let gate_ship = 916_102_i64;
    let anchor_char = 916_003_i64;
    let anchor_ship = 916_103_i64;
    let depot_id = 916_203_i64;
    let fil_char = 916_004_i64;
    let fil_ship = 916_104_i64;
    let fil_id = 916_204_i64;
    let loot_char = 916_005_i64;
    let loot_ship = 916_105_i64;
    let wreck_id = 916_205_i64;
    let wreck_loot_id = 916_305_i64;
    let hack_char = 916_006_i64;
    let hack_peer = 916_007_i64;
    let hack_ship = 916_106_i64;
    let hack_peer_ship = 916_107_i64;
    let enemy_struct_id = 1_916_900_i64;
    let corp_id = 98_160_001_i64;
    let enemy_corp_id = 98_160_002_i64;

    let station_pos = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| s.position)
        .unwrap_or(glam::DVec3::ZERO);

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        let base_corp = snap.corporations.values().next().cloned().expect("seed corp");
        for cid in [corp_id, enemy_corp_id] {
            let mut c = base_corp.clone();
            c.corporation_id = cid;
            c.name = format!("Corp {cid}");
            snap.corporations.insert(cid, c);
        }
        for (cid, sid, in_sys) in [
            (dock_char, dock_ship, sys_a),
            (gate_char, gate_ship, sys_a),
            (anchor_char, anchor_ship, sys_a),
            (fil_char, fil_ship, sys_a),
            (loot_char, loot_ship, sys_a),
            (hack_char, hack_ship, sys_a),
            (hack_peer, hack_peer_ship, sys_a),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = in_sys;
            ch.active_ship_id = sid;
            if cid == fil_char {
                ch.fleet_id = Some(916_501);
            }
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: in_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.fleets.insert(
            916_501,
            eve_db::FleetRecord {
                fleet_id: 916_501,
                boss_id: fil_char,
                members: vec![fil_char],
                free_move: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33_474, // Mobile Depot
                owner_id: anchor_char,
                location_id: anchor_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 56_071, // Pochven entry filament
                owner_id: fil_char,
                location_id: fil_ship,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26_557,
                owner_id: hack_peer,
                location_id: sys_a as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_loot_id,
            InventoryItemRecord {
                item_id: wreck_loot_id,
                type_id: 34,
                owner_id: hack_peer,
                location_id: wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            enemy_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: enemy_struct_id,
                type_id: 35_832,
                owner_corp_id: enemy_corp_id,
                solar_system_id: sys_a,
                name: "Enemy Astrahus".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        for (mod_iid, owner_cid, ship_iid) in [
            (916_401_i64, hack_char, hack_ship),
            (916_402_i64, hack_peer, hack_peer_ship),
        ] {
            snap.items.insert(
                mod_iid,
                InventoryItemRecord {
                    item_id: mod_iid,
                    type_id: 22_586, // Data Analyzer I
                    owner_id: owner_cid,
                    location_id: ship_iid,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    let mk = |id: i64, tid: u32, pos: glam::DVec3| eve_destiny::Ball {
        id,
        type_id: tid,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(mk(dock_ship, 587, station_pos)).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            mk(gate_ship, 587, glam::DVec3::new(1_000.0, 0.0, 0.0)),
            eve_destiny::CombatState {
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_handle.spawn_ball(mk(anchor_ship, 587, glam::DVec3::new(200_000.0, 0.0, 0.0))).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            mk(fil_ship, 587, glam::DVec3::new(1.5e13, -2.1e13, 0.9e13)),
            eve_destiny::CombatState::default(),
        )
        .await;
    let _ = sys_handle.spawn_ball(mk(loot_ship, 587, glam::DVec3::new(500.0, 0.0, 0.0))).await;
    let _ = sys_handle.spawn_ball(mk(wreck_id, 26_557, glam::DVec3::new(500.0, 0.0, 0.0))).await;
    let _ = sys_handle.spawn_ball(mk(hack_ship, 587, glam::DVec3::new(50_000.0, 0.0, 0.0))).await;
    let _ = sys_handle.spawn_ball(mk(hack_peer_ship, 587, glam::DVec3::new(50_000.0, 0.0, 0.0))).await;
    let _ = sys_handle.spawn_ball(mk(enemy_struct_id, 35_832, glam::DVec3::new(50_000.0, 0.0, 0.0))).await;
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(loot_char, crate::services::combat_crimewatch::SafetyLevel::Red);
    });

    let mk_sess = |cid: i64, sid: i64, fleet_id: Option<i64>| {
        let mut s = SessionState::default();
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp_id);
        s.solar_system_id = Some(sys_a);
        s.ship_id = Some(sid);
        s.fleet_id = fleet_id;
        s
    };
    let kw = HashMap::new();

    // 1. Concurrent stationSvc.Dock
    let mut d1 = mk_sess(dock_char, dock_ship, None);
    let mut d2 = d1.clone();
    let d_args1 = [PyRep::Integer(station_id)];
    let d_args2 = [PyRep::Integer(station_id)];
    let (dr1, dr2) = tokio::join!(
        crate::services::station_structure::handle(&ctx, &mut d1, "stationSvc", "Dock", &d_args1, &kw),
        crate::services::station_structure::handle(&ctx, &mut d2, "stationSvc", "Dock", &d_args2, &kw),
    );
    assert_eq!(
        [dr1.unwrap(), dr2.unwrap()].iter().filter(|r| r.is_ok()).count(),
        1,
        "only one concurrent Dock may succeed"
    );

    // 2. Concurrent beyonce.CmdStargateJump
    let mut g1 = mk_sess(gate_char, gate_ship, None);
    let mut g2 = g1.clone();
    let g_args1 = [PyRep::Integer(gate_id)];
    let g_args2 = [PyRep::Integer(gate_id)];
    let (gr1, gr2) = tokio::join!(
        crate::services::space_destiny::handle(&ctx, &mut g1, None, "beyonce", "CmdStargateJump", &g_args1, &kw),
        crate::services::space_destiny::handle(&ctx, &mut g2, None, "beyonce", "CmdStargateJump", &g_args2, &kw),
    );
    assert_eq!(
        [gr1.unwrap(), gr2.unwrap()].iter().filter(|r| r.is_ok()).count(),
        1,
        "only one concurrent CmdStargateJump may transition system"
    );

    // 3. Concurrent posMgr.AnchorStructure on same depot
    let mut a1 = mk_sess(anchor_char, anchor_ship, None);
    let mut a2 = a1.clone();
    let a_args1 = [PyRep::Integer(depot_id)];
    let a_args2 = [PyRep::Integer(depot_id)];
    let (ar1, ar2) = tokio::join!(
        crate::services::station_structure::handle(&ctx, &mut a1, "posMgr", "AnchorStructure", &a_args1, &kw),
        crate::services::station_structure::handle(&ctx, &mut a2, "posMgr", "AnchorStructure", &a_args2, &kw),
    );
    assert_eq!(
        [ar1.unwrap(), ar2.unwrap()].iter().filter(|r| r.is_ok()).count(),
        1,
        "only one concurrent AnchorStructure may consume & anchor depot"
    );

    // 4. Concurrent randomJumpMgr.ActivateRandomJumpFilament on qty=1 filament
    let mut f1 = mk_sess(fil_char, fil_ship, Some(916_501));
    let mut f2 = f1.clone();
    let f_args1 = [PyRep::Integer(fil_id)];
    let f_args2 = [PyRep::Integer(fil_id)];
    let (fr1, fr2) = tokio::join!(
        crate::services::space_destiny::handle(&ctx, &mut f1, None, "randomJumpMgr", "ActivateRandomJumpFilament", &f_args1, &kw),
        crate::services::space_destiny::handle(&ctx, &mut f2, None, "randomJumpMgr", "ActivateRandomJumpFilament", &f_args2, &kw),
    );
    assert_eq!(
        [fr1.unwrap(), fr2.unwrap()].iter().filter(|r| r.is_ok()).count(),
        1,
        "only one concurrent ActivateRandomJumpFilament may consume qty=1 filament"
    );

    // 5. Concurrent invbroker.LootAll on 1-item foreign wreck
    let mut l1 = mk_sess(loot_char, loot_ship, None);
    let mut l2 = l1.clone();
    let l_args1 = [PyRep::Integer(wreck_id)];
    let l_args2 = [PyRep::Integer(wreck_id)];
    let _ = tokio::join!(
        crate::services::combat_crimewatch::handle(&ctx, &mut l1, "invbroker", "LootAll", &l_args1, &kw),
        crate::services::combat_crimewatch::handle(&ctx, &mut l2, "invbroker", "LootAll", &l_args2, &kw),
    );
    let remaining_in_wreck = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .count()
    });
    assert_eq!(remaining_in_wreck, 0);
    let in_looter_cargo: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == loot_ship && i.flag == InventoryFlag::Cargo)
            .map(|i| i64::from(i.quantity))
            .sum()
    });
    assert_eq!(in_looter_cargo, 5, "loot quantity must never duplicate");

    // 6. Concurrent hackingMgr.StartNewGameInstance on the same structure
    let mut h1 = mk_sess(hack_char, hack_ship, None);
    let mut h2 = mk_sess(hack_peer, hack_peer_ship, None);
    let h_args1 = [PyRep::Integer(1), PyRep::Integer(enemy_struct_id)];
    let h_args2 = [PyRep::Integer(1), PyRep::Integer(enemy_struct_id)];
    let (hr1, hr2) = tokio::join!(
        crate::services::pve_hacking_sites::handle_pve_hacking(&ctx, &mut h1, "hackingMgr", "StartNewGameInstance", &h_args1, &kw),
        crate::services::pve_hacking_sites::handle_pve_hacking(&ctx, &mut h2, "hackingMgr", "StartNewGameInstance", &h_args2, &kw),
    );
    assert_eq!(
        [&hr1, &hr2].iter().filter(|r| matches!(r, Some(Ok(_)))).count(),
        1,
        "only one concurrent StartNewGameInstance may claim an in-progress structure hack: hr1={hr1:?} hr2={hr2:?}"
    );
}
