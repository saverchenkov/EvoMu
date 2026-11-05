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
