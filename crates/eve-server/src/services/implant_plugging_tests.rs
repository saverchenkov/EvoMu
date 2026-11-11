#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[test]
fn implant_plugging_and_booster_mutations() {
    assert!(is_daily_alpha_injector(TYPE_DAILY_ALPHA_INJECTOR));
    assert!(is_daily_alpha_injector(46375));
    assert!(!is_daily_alpha_injector(TYPE_LARGE_SKILL_INJECTOR));

    let ctx = test_ctx();
    assert_eq!(non_diminishing_sp_yield(&ctx.sde, TYPE_DAILY_ALPHA_INJECTOR), Some(50_000.0));
    assert!(is_non_diminishing_injector(&ctx.sde, TYPE_DAILY_ALPHA_INJECTOR));
    assert!(!booster_penalty_attribute_ids(&ctx.sde).is_empty());

    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);

    let unknown_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle_skill_notifications_and_injections(&ctx, &sess, unknown_svc, "CheckAndSendNotifications").is_none());
    assert!(handle_skill_notifications_and_injections(&ctx, &sess, "skillMgr2", unknown_method).is_none());

    assert_eq!(
        handle_skill_notifications_and_injections(&ctx, &sess, "skillMgr2", "CheckAndSendNotifications")
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    let avail_inj = handle_skill_notifications_and_injections(
        &ctx,
        &sess,
        "nonDiminishingInjectionMgr",
        "GetAvailableNonDiminishingInjections",
    )
    .unwrap()
    .unwrap();
    assert!(matches!(avail_inj, PyRep::Dict(_)));

    let next_alpha = handle_skill_notifications_and_injections(
        &ctx,
        &sess,
        "alphaInjectorMgr",
        "GetNextAvailableInjection",
    )
    .unwrap()
    .unwrap();
    assert_eq!(next_alpha, PyRep::Integer(0));

    // Find a valid implant type (slot 1..=10) with no high skill prereqs, or grant prereqs
    let implant_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.default_attributes
                    .get(&ATTR_IMPLANTNESS)
                    .is_some_and(|&v| (1.0..=10.0).contains(&v))
                && !t.default_attributes.contains_key(&182)
        })
        .map(|t| t.type_id)
        .unwrap_or(9899);

    let imp_iid = 999_700_001;
    ctx.db.with_mut_snapshot(|snap| {
        for &sk_tid in &[3402_u32, 3405, 3411, 24242, 24606] {
            snap.skills.entry(90_000_001).or_default().insert(
                sk_tid,
                eve_db::TrainedSkillRecord {
                    type_id: sk_tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        for &attr_sk in &[182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&sk_tid) = ctx
                .sde
                .types
                .get(&implant_tid)
                .and_then(|t| t.default_attributes.get(&attr_sk))
            {
                snap.skills.entry(90_000_001).or_default().insert(
                    sk_tid as u32,
                    eve_db::TrainedSkillRecord {
                        type_id: sk_tid as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.active_implants.remove(&90_000_001);
        snap.items.insert(
            imp_iid,
            InventoryItemRecord {
                item_id: imp_iid,
                type_id: implant_tid,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert!(inject_implant(&ctx, &sess, &[]).is_err());
    assert!(inject_implant(&ctx, &sess, &[PyRep::Integer(999_999_999)]).is_err());

    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_iid)]).unwrap(), PyRep::None);

    let implants_rep = get_implants(&ctx, &sess).unwrap();
    let PyRep::Dict(imp_entries) = implants_rep else {
        panic!("expected dict");
    };
    assert!(!imp_entries.is_empty());

    // Duplicate slot rejected
    assert!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_iid)]).is_err());

    // Destroy implant
    assert!(destroy_implant(&ctx, &sess, &[]).is_err());
    assert_eq!(destroy_implant(&ctx, &sess, &[PyRep::Integer(imp_iid)]).unwrap(), PyRep::None);
    assert!(destroy_implant(&ctx, &sess, &[PyRep::Integer(imp_iid)]).is_err());

    // Booster tests
    let booster_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.default_attributes.get(&ATTR_BOOSTERNESS).is_some_and(|&v| v >= 1.0)
                && t.default_attributes.get(&ATTR_BOOSTER_DURATION_MS).is_some_and(|&d| d > 0.0)
                && !t.default_attributes.contains_key(&182)
        })
        .map(|t| t.type_id)
        .unwrap();
    let bst_iid = 999_700_002;
    ctx.db.with_mut_snapshot(|snap| {
        for &attr_sk in &[182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&sk_tid) = ctx
                .sde
                .types
                .get(&booster_tid)
                .and_then(|t| t.default_attributes.get(&attr_sk))
            {
                snap.skills.entry(90_000_001).or_default().insert(
                    sk_tid as u32,
                    eve_db::TrainedSkillRecord {
                        type_id: sk_tid as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.items.insert(
            bst_iid,
            InventoryItemRecord {
                item_id: bst_iid,
                type_id: booster_tid,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert!(use_booster(&ctx, &sess, &[]).is_err());
    let bst_rep = use_booster(&ctx, &sess, &[PyRep::Integer(bst_iid)]).unwrap();
    assert_eq!(bst_rep.dict_get("boosterID"), Some(&PyRep::Integer(bst_iid)));
    assert_eq!(bst_rep.dict_get("typeID"), Some(&PyRep::Integer(booster_tid as i64)));

    let boosters_rep = get_boosters(&ctx, &sess).unwrap();
    let PyRep::Dict(bst_entries) = boosters_rep else {
        panic!("expected dict");
    };
    assert_eq!(bst_entries.len(), 1);

    let (act_bsts, side_effs) = active_combat_boosters_for_char(&ctx.db, 90_000_001, ctx.now_unix());
    assert_eq!(act_bsts, vec![booster_tid]);
    let mut ship_attrs = HashMap::new();
    apply_active_booster_side_effect_adjustments(&ctx.sde, &mut ship_attrs, &HashMap::new(), &act_bsts, &side_effs);

    // Sync clone jump removes non-persistent boosters
    ctx.db.with_mut_snapshot(|snap| {
        sync_clone_jump_implants_and_boosters(&ctx.sde, snap, 90_000_001);
    });

    // Skill injector test (Large Skill Injector)
    let lsi_iid = 999_700_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lsi_iid,
            InventoryItemRecord {
                item_id: lsi_iid,
                type_id: TYPE_LARGE_SKILL_INJECTOR,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sp_yield = inject_skillpoints(&ctx, &sess, &[PyRep::Integer(lsi_iid)])
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(sp_yield > 0.0);
    sess.station_id = Some(60_003_760);
}

#[test]
fn implant_and_booster_stack_split_expiry_and_error_guards() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;

    let implant_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.default_attributes
                    .get(&ATTR_IMPLANTNESS)
                    .is_some_and(|&v| (1.0..=10.0).contains(&v))
                && !t.default_attributes.contains_key(&182)
        })
        .map(|t| t.type_id)
        .unwrap_or(9899);

    let booster_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.default_attributes
                    .get(&ATTR_BOOSTERNESS)
                    .is_some_and(|&v| v >= 1.0)
                && t.default_attributes
                    .get(&ATTR_BOOSTER_DURATION_MS)
                    .is_some_and(|&d| d > 0.0)
                && !t.default_attributes.contains_key(&182)
        })
        .map(|t| t.type_id)
        .unwrap_or(9951);

    let imp_stack_id = 999_710_001_i64;
    let imp_second_id = 999_710_002_i64;
    let imp_unowned_id = 999_710_003_i64;
    let imp_unreachable_id = 999_710_004_i64;
    let tritanium_id = 999_710_005_i64;
    let booster_stack_id = 999_710_006_i64;
    let exp_booster_a = 999_710_010_i64;
    let exp_booster_b = 999_710_011_i64;
    let perm_booster = 999_710_012_i64;
    let hangar_booster = 999_710_013_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
        for &attr_sk in &[182_u16, 183, 184, 1285, 1289, 1290] {
            for &tid in &[implant_tid, booster_tid] {
                if let Some(&sk_tid) = ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&attr_sk)) {
                    snap.skills.entry(char_id).or_default().insert(
                        sk_tid as u32,
                        eve_db::TrainedSkillRecord {
                            type_id: sk_tid as u32,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
        snap.items.insert(
            imp_stack_id,
            InventoryItemRecord {
                item_id: imp_stack_id,
                type_id: implant_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            imp_second_id,
            InventoryItemRecord {
                item_id: imp_second_id,
                type_id: implant_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            imp_unowned_id,
            InventoryItemRecord {
                item_id: imp_unowned_id,
                type_id: implant_tid,
                owner_id: other_char,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            imp_unreachable_id,
            InventoryItemRecord {
                item_id: imp_unreachable_id,
                type_id: implant_tid,
                owner_id: char_id,
                location_id: 60_003_761,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            booster_stack_id,
            InventoryItemRecord {
                item_id: booster_stack_id,
                type_id: booster_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let make_booster = |id: i64, owner: i64, loc: i64, flag: InventoryFlag, exp: Option<f64>| {
            let mut dyn_attrs = HashMap::new();
            if let Some(e) = exp {
                dyn_attrs.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, e);
            }
            InventoryItemRecord {
                item_id: id,
                type_id: booster_tid,
                owner_id: owner,
                location_id: loc,
                flag,
                quantity: 1,
                singleton: true,
                dynamic_attributes: dyn_attrs,
                version: 1,
                ..Default::default()
            }
        };
        snap.items.insert(exp_booster_a, make_booster(exp_booster_a, char_id, char_id, InventoryFlag::Booster, Some(1_000.0)));
        snap.items.insert(exp_booster_b, make_booster(exp_booster_b, other_char, other_char, InventoryFlag::Booster, Some(1_000.0)));
        snap.items.insert(perm_booster, make_booster(perm_booster, char_id, char_id, InventoryFlag::Booster, Some(0.0)));
        snap.items.insert(hangar_booster, make_booster(hangar_booster, char_id, 60_003_760, InventoryFlag::Hangar, Some(1.0)));
    });

    // Inject guards
    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_unowned_id)]).unwrap_err().code, "ItemNotOwned");
    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_unreachable_id)]).unwrap_err().code, "ItemNotReachable");
    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(tritanium_id)]).unwrap_err().code, "ItemNotAnImplant");

    // Stack split: quantity 3 -> 2 + new plugged implant item
    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_stack_id)]).unwrap(), PyRep::None);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&imp_stack_id].quantity, 2);
        let plugged: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == char_id && i.flag == InventoryFlag::Implant)
            .collect();
        assert_eq!(plugged.len(), 1);
        assert!(plugged[0].item_id != imp_stack_id);
        assert_eq!(snap.active_implants[&char_id], vec![implant_tid]);
    });

    // Distinct implant item in the same slot triggers ImplantSlotOccupied (vs ImplantAlreadyPluggedIn on imp_second_id? no: imp_second_id is distinct item_id with occupied slot!)
    assert_eq!(inject_implant(&ctx, &sess, &[PyRep::Integer(imp_second_id)]).unwrap_err().code, "ImplantSlotOccupied");

    // Hangar item not plugged -> ImplantNotPluggedIn; missing item -> ImplantNotFound
    assert_eq!(destroy_implant(&ctx, &sess, &[PyRep::Integer(imp_second_id)]).unwrap_err().code, "ImplantNotPluggedIn");
    assert_eq!(destroy_implant(&ctx, &sess, &[PyRep::Integer(999_888_777)]).unwrap_err().code, "ImplantNotFound");

    let plugged_iid = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == char_id && i.location_id == char_id && i.flag == InventoryFlag::Implant)
            .unwrap()
            .item_id
    });
    assert_eq!(destroy_implant(&ctx, &sess, &[PyRep::Integer(plugged_iid)]).unwrap(), PyRep::None);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&plugged_iid));
        assert!(snap.active_implants.get(&char_id).is_none_or(|v| v.is_empty()));
    });

    // Booster purge boundary tests: now = 999 leaves exp = 1000 intact; now = 1000 purges only char_filter when set
    ctx.db.with_mut_snapshot(|snap| {
        assert!(purge_expired_boosters_in_snapshot(snap, Some(char_id), 999).is_empty());
        assert!(snap.items.contains_key(&exp_booster_a));
        assert!(snap.items.contains_key(&exp_booster_b));

        let expected_ship = snap.characters[&char_id].active_ship_id;
        let purged = purge_expired_boosters_in_snapshot(snap, Some(char_id), 1_000);
        assert_eq!(purged, vec![expected_ship]);
        assert!(!snap.items.contains_key(&exp_booster_a));
        assert!(snap.items.contains_key(&exp_booster_b));
        assert!(snap.items.contains_key(&perm_booster));
        assert!(snap.items.contains_key(&hangar_booster));
    });
    // purge_expired_boosters_all purges exp_booster_b globally at >= 1000
    purge_expired_boosters_all(&ctx, 1_000);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&exp_booster_b));
        assert!(snap.items.contains_key(&perm_booster));
    });

    // UseBooster guards (including BoosterSlotOccupied while perm_booster active) & stack split (qty 3 -> 2 + new booster entity)
    assert!(use_booster(&ctx, &sess, &[]).is_err());
    assert_eq!(use_booster(&ctx, &sess, &[PyRep::Integer(tritanium_id)]).unwrap_err().code, "ItemNotABooster");
    assert_eq!(use_booster(&ctx, &sess, &[PyRep::Integer(imp_unowned_id)]).unwrap_err().code, "ItemNotOwned");
    assert_eq!(use_booster(&ctx, &sess, &[PyRep::Integer(booster_stack_id)]).unwrap_err().code, "BoosterSlotOccupied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.remove_system_cleanup_item(perm_booster);
    });
    let used = use_booster(&ctx, &sess, &[PyRep::Integer(booster_stack_id)]).unwrap();
    assert!(matches!(used, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&booster_stack_id].quantity, 2);
        let consumed = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == char_id
                    && i.flag == InventoryFlag::Booster
                    && i.type_id == booster_tid
                    && i.item_id != perm_booster
            })
            .expect("consumed booster item created on character");
        assert!(consumed.dynamic_attributes.get(&DYN_ATTR_BOOSTER_DURATION_SECS).copied().unwrap_or(0.0) > 0.0);
        assert!(consumed.dynamic_attributes.get(&DYN_ATTR_BOOSTER_EXPIRY_UNIX).copied().unwrap_or(0.0) > 0.0);
    });
}

#[test]
fn w3_mutant_killers_clone_jump_follows_boundary_already_plugged_and_slot_bounds() {
    let mut ctx = test_ctx();
    let mut sde = (*ctx.sde).clone();
    let tmpl = sde.types.get(&587).unwrap().clone();

    for (tid, attr_id, val) in [
        (991_001_u32, ATTR_IMPLANTNESS, 0.0_f64),
        (991_002_u32, ATTR_IMPLANTNESS, 1.0_f64),
        (991_003_u32, ATTR_IMPLANTNESS, 10.0_f64),
        (991_004_u32, ATTR_IMPLANTNESS, 11.0_f64),
        (991_005_u32, ATTR_FOLLOWS_JUMP_CLONES, 0.99_f64),
        (991_006_u32, ATTR_FOLLOWS_JUMP_CLONES, 1.0_f64),
    ] {
        let mut t = tmpl.clone();
        t.type_id = tid;
        t.default_attributes = HashMap::from([(attr_id, val)]);
        if tid >= 991_005 {
            t.default_attributes.insert(ATTR_BOOSTERNESS, 1.0);
            t.default_attributes.insert(ATTR_BOOSTER_DURATION_MS, 60_000.0);
        }
        sde.types.insert(tid, t);
    }
    ctx.sde = std::sync::Arc::new(sde);

    assert_eq!(implant_slot(&ctx.sde, 991_001), None);
    assert_eq!(implant_slot(&ctx.sde, 991_002), Some(1));
    assert_eq!(implant_slot(&ctx.sde, 991_003), Some(10));
    assert_eq!(implant_slot(&ctx.sde, 991_004), None);

    let char_id = 90_000_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        // Already-plugged implant item -> ImplantAlreadyPluggedIn
        snap.items.insert(
            991_100,
            InventoryItemRecord {
                item_id: 991_100,
                type_id: 991_002,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Boosters for sync_clone_jump_implants_and_boosters: 0.99 (< 1.0 -> removed) vs 1.0 (>= 1.0 -> preserved)
        snap.items.insert(
            991_101,
            InventoryItemRecord {
                item_id: 991_101,
                type_id: 991_005,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            991_102,
            InventoryItemRecord {
                item_id: 991_102,
                type_id: 991_006,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Hangar booster for expected_loc mismatch check in use_booster
        snap.items.insert(
            991_103,
            InventoryItemRecord {
                item_id: 991_103,
                type_id: 991_006,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(991_100)])
            .unwrap_err()
            .code,
        "ImplantAlreadyPluggedIn"
    );
    assert_eq!(
        use_booster(
            &ctx,
            &sess,
            &[PyRep::Integer(991_103), PyRep::Integer(60_003_761)]
        )
        .unwrap_err()
        .code,
        "ItemNotReachable"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(char_id, vec![991_003]);
        sync_clone_jump_implants_and_boosters(&ctx.sde, snap, char_id);
        assert!(!snap.items.contains_key(&991_101));
        assert!(snap.items.contains_key(&991_102));
        assert!(!snap.items.contains_key(&991_100));
        assert!(snap.items.values().any(|i| {
            i.owner_id == char_id && i.flag == InventoryFlag::Implant && i.type_id == 991_003
        }));

        // Direct unit check for `purge_expired_boosters_in_snapshot_with_items`
        let mut exp_booster = InventoryItemRecord {
            item_id: 991_201,
            type_id: 991_006,
            owner_id: char_id,
            location_id: char_id,
            flag: InventoryFlag::Booster,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        exp_booster
            .dynamic_attributes
            .insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, 1_000.0);
        snap.items.insert(991_201, exp_booster);
        let (ships, removed) =
            purge_expired_boosters_in_snapshot_with_items(snap, Some(char_id), 2_000);
        assert_eq!(ships.len(), 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].0, char_id);
        assert_eq!(removed[0].1.item_id, 991_201);
        assert!(!snap.items.contains_key(&991_201));
    });
}
