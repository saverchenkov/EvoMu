//! Semantic unit tests split from `structure_lifecycle_rpc_tests`.

#![forbid(unsafe_code)]

use super::*;

use eve_db::{
    InventoryFlag, InventoryItemRecord,
};
use eve_destiny::DamageVector;
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test(flavor = "current_thread")]
async fn attack_precheck_and_live_combat_damage_enforce_rules_and_destroy() {
    let mut ctx = test_ctx();
    let tid = 910_010u32;
    seed_astrahus_type(&mut ctx, tid, false);
    seed_char_corp_ship(&ctx, 9301, 98301, 30000142, 30001, true);
    seed_char_corp_ship(&ctx, 9302, 98302, 30000142, 30002, true);
    let now_u = ctx.now_unix();
    seed_structure(&ctx, 10_000_930_001, tid, 98301, 30000142, life::STATE_SHIELD_VULNERABLE, 100);
    seed_structure(&ctx, 10_000_930_002, tid, 98301, 30000142, life::STATE_ARMOR_REINFORCED, 100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&10_000_930_002).unwrap().reinforced_until_unix = now_u + 86_400;
    });

    // Friendly corp blocked
    let own = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9301, 10_000_930_001, now_u)
        .unwrap_err();
    assert_eq!(own.code, "CannotAttackFriendlyStructure");

    // Reinforced blocked
    let reinf = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9302, 10_000_930_002, now_u)
        .unwrap_err();
    assert_eq!(reinf.code, "StructureReinforcedInvulnerable");

    // Highsec without war blocked; with war allowed
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&30000142)
        .map(|s| s.security_status)
        .unwrap_or(0.95);
    if sys_sec >= 0.45 {
        let concord = validate_upwell_structure_attack_precheck(&ctx, 30000142, 9302, 10_000_930_001, now_u)
            .unwrap_err();
        assert_eq!(concord.code, "IllegalHighSecStructureAttack");
    }

    // Apply live combat damage to HullVulnerable structure in lowsec -> transitions to Destroyed
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);
    seed_structure(&ctx, 10_000_930_003, tid, 98301, low_sys, life::STATE_HULL_VULNERABLE, 50);
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&10_000_930_003).unwrap();
        st.shield_hp = 0.0;
        st.armor_hp = 0.0;
        st.hull_hp = 1_000.0;
        st.last_damage_unix = now_u;
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(10_000_930_003).or_default().repair_until_unix = Some(now_u + 900);
        save_ext_state(snap, &ext);
    });
    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let (_, destroyed, _) = apply_damage_structure_or_ball(
        &ctx,
        &sys_h,
        low_sys,
        9302,
        10_000_930_003,
        DamageVector::new(0.0, 0.0, 2_000.0, 0.0),
        1.0,
        now_u,
    )
    .await
    .expect("damage applied")
    .expect("structure hit");
    assert!(destroyed);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&10_000_930_003].state, life::STATE_DESTROYED);
    });
}

#[tokio::test(flavor = "current_thread")]
async fn fitting_online_fuel_jettison_delivery_and_cargo_dropoff_work() {
    let mut ctx = test_ctx();
    let st_tid = 910_020u32;
    let mod_tid = 910_021u32;
    let svc_tid = 910_022u32;
    let fuel_tid = 4247u32;
    seed_astrahus_type(&mut ctx, st_tid, false);
    clone_type_with(
        &mut ctx.sde,
        35832,
        mod_tid,
        1329,
        66,
        500.0,
        0.0,
        &[(1298, 1657.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35832,
        svc_tid,
        1415,
        66,
        2_000.0,
        0.0,
        &[
            (life::ATTR_SERVICE_FUEL_GROUP, 1136.0),
            (life::ATTR_SERVICE_FUEL_ONLINE, 40.0),
            (life::ATTR_SERVICE_FUEL_PER_HOUR, 5.0),
        ],
    );
    clone_type_with(&mut ctx.sde, 35832, fuel_tid, 1136, 14, 5.0, 0.0, &[]);

    let sid = 10_000_940_001i64;
    seed_char_corp_ship(&ctx, 9401, 98401, 30000142, 40001, true);
    seed_char_corp_ship(&ctx, 9302, 98302, 30000142, 40002, true);
    seed_structure(&ctx, sid, st_tid, 98401, 30000142, life::STATE_SHIELD_VULNERABLE, 200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70001,
            InventoryItemRecord {
                item_id: 70001,
                type_id: mod_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            70002,
            InventoryItemRecord {
                item_id: 70002,
                type_id: svc_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(9401, 98401, 30000142, Some(40001), 1);
    sess.structure_id = Some(sid);
    let _ = handle_structure_fit_move(&ctx, &mut sess, 70001, sid, InventoryFlag::HiSlot0, Some(true))
        .expect("fit hi slot");
    let _ = handle_structure_fit_move(&ctx, &mut sess, 70002, sid, InventoryFlag::ServiceSlot0, Some(false))
        .expect("fit service slot");
    let online_res = handle_structure_module_online(&ctx, &mut sess, sid, 70002, true)
        .expect("online service module");
    assert!(matches!(online_res, PyRep::KeyVal(_)));

    // Damaged hull blocks refit
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().armor_hp = 100.0;
    });
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, 70001, sid, InventoryFlag::Hangar, None)
            .unwrap_err()
            .code,
        "StructureDamagedCannotRefit"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().armor_hp = 500_000.0;
    });

    // Jettison fuel blocks requires being docked in or actively controlling structure
    sess.structure_id = None;
    let jettison_args = vec![
        PyRep::Integer(sid),
        PyRep::Integer(i64::from(fuel_tid)),
        PyRep::Integer(25),
    ];
    assert_eq!(
        handle_jettison_structure_fuel(&ctx, &mut sess, &jettison_args)
            .await
            .unwrap_err()
            .code,
        "NotDockedInStructure"
    );
    sess.structure_id = Some(sid);
    let jet = handle_jettison_structure_fuel(&ctx, &mut sess, &jettison_args)
        .await
        .expect("jettison");
    assert!(matches!(jet, PyRep::KeyVal(_)));

    // Deliver item while docked & drop off cargo while in space
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70003,
            InventoryItemRecord {
                item_id: 70003,
                type_id: fuel_tid,
                owner_id: 9401,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            70004,
            InventoryItemRecord {
                item_id: 70004,
                type_id: fuel_tid,
                owner_id: 9401,
                location_id: 40001,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.structure_id = Some(sid);
    let _ = handle_structure_deliver(
        &ctx,
        &mut sess,
        &[PyRep::Integer(sid), PyRep::Integer(70003), PyRep::Integer(9302)],
    )
    .expect("deliver item");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&70003].flag),
        InventoryFlag::Deliveries
    );

    sess.structure_id = None;
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&98401).unwrap().ceo_id = 999999;
        snap.corporations.get_mut(&98401).unwrap().member_roles.clear();
    });
    spawn_ship_ball(&ctx, 30000142, 40001, DVec3::new(2_000_005_500.0, 0.0, 0.0)).await;
    let _ = handle_cargo_dropoff(
        &ctx,
        &mut sess,
        &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(70004)])],
    )
    .await
    .expect("cargo dropoff");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&70004].flag),
        InventoryFlag::Deliveries
    );
}
