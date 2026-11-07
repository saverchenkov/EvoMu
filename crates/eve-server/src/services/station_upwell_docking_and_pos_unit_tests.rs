//! Semantic unit tests split from `station_structure_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::station_structure_extra_unit_tests::*;

use super::station_structure::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn pos_mgr_anchor_structure_restriction_and_warp_disruptor_container_paths() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 920_301_i64;
    let corp_id = 98_200_301_i64;
    let ship_id = 920_302_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let jita_sys = 30_000_142_u32;
    let kw = HashMap::new();

    let find_grp = |grp: u32| ctx.sde.types.values().find(|t| t.group_id == grp).unwrap().type_id;
    let tower_tid = find_grp(365);
    let freight_cont_tid = find_grp(649);
    let bubble_tid = find_grp(361);
    let sec_cont_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 340 && t.capacity > 0.0)
        .unwrap()
        .type_id;

    let (tower_id, freight_id, trit_id, foreign_id, bubble_id, sec_cont_id) =
        (920_310, 920_311, 920_312, 920_313, 920_314, 920_315);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "AnchorPilot", corp_id, null_sys, ship_id);
        snap.characters.insert(char_id, ch);
        for (id, tid, owner, loc) in [
            (tower_id, tower_tid, char_id, ship_id),
            (freight_id, freight_cont_tid, char_id, ship_id),
            (trit_id, 34, char_id, ship_id),
            (foreign_id, 33_474, 999_999, ship_id),
            (bubble_id, bubble_tid, char_id, ship_id),
            (sec_cont_id, sec_cont_tid, char_id, ship_id),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Grant Omega clone grade and anchoring skills so skill checks pass
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        for &(skill_attr, _) in &[
            (182u16, 277u16),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1290),
            (1291, 1292),
        ] {
            for tid in [bubble_tid, sec_cont_tid] {
                if let Some(&sk) = ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&skill_attr)) {
                    let rec = eve_db::TrainedSkillRecord {
                        type_id: sk as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    };
                    snap.skills.entry(char_id).or_default().insert(sk as u32, rec);
                }
            }
        }
    });

    let mut sess = make_sess(char_id, corp_id, Some(null_sys), None, None, Some(ship_id));

    // Missing argument -> MissingArgument
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingArgument"
    );

    // Control tower, freight container, non-anchorable item, foreign item
    for (id, expected_code) in [
        (tower_id, "StarbaseRemoved"),
        (freight_id, "CannotAnchorFreightContainer"),
        (trit_id, "ItemNotAnchorable"),
        (foreign_id, "NotItemOwner"),
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(id)], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            expected_code
        );
    }

    // Thera (31_000_005) & Abyssal (32_000_001) & Zarzakh (30_100_000) restrictions
    let mut thera_sess = sess.clone();
    thera_sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle(&ctx, &mut thera_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    let mut abyss_sess = sess.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        handle(&ctx, &mut abyss_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDeployInAbyssalSpace"
    );

    let mut zarzakh_sess = sess.clone();
    zarzakh_sess.solar_system_id = Some(30_100_000);
    assert!(handle(&ctx, &mut zarzakh_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
        .await
        .unwrap()
        .is_err());

    // High-sec restrictions for Mobile Warp Disruptor (361) and Secure Container (340 in >= 0.8)
    let mut hs_sess = sess.clone();
    hs_sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInEmpire"
    );
    assert_eq!(
        handle(&ctx, &mut hs_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantAnchorInHighSec08"
    );

    // Spawn ship in null_sys and anchor both Mobile Warp Disruptor (with explicit coords) and Secure Container
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let bubble_rep = handle(
        &ctx,
        &mut sess,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(bubble_id),
            PyRep::Tuple(vec![PyRep::Real(300_500.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("anchor mobile warp disruptor in nullsec");
    assert!(bubble_rep.dict_get("bubbleRadius").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Anchoring already-anchoring bubble again -> StructureAlreadyAnchored
    assert_eq!(
        handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureAlreadyAnchored"
    );

    let cont_rep = handle(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw)
        .await
        .unwrap()
        .expect("anchor secure container in nullsec");
    assert_eq!(cont_rep.dict_get("itemID"), Some(&PyRep::Integer(sec_cont_id)));
}
