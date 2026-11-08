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
