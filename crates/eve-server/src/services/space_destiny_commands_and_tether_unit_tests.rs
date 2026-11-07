//! Semantic unit tests split from `space_nav_travel_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::space_nav_travel_extra_unit_tests::*;
use crate::services::space_destiny::space_nav_travel::*;
use eve_db::types::UpwellStructureRecord;
use eve_db::{
    BookmarkRecord, InventoryFlag,
};
use eve_destiny::CombatState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn resolve_target_position_and_destiny_command_validation_branches() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 96_810_i64;
    let ship_id = 78_820_i64;
    let target_id = 78_821_i64;
    let st_id = 1_049_820_001_i64;
    let bm_id = 6_001_i64;

    // 1. resolve_system_target_position across station, stargate, celestial, structure, and <= 0
    assert!(resolve_system_target_position(&ctx, sys_id, char_id, 0).await.is_none());
    assert!(resolve_system_target_position(&ctx, sys_id, char_id, 999_999_999).await.is_none());

    if let Some(st) = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id) {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, st.station_id).await,
            Some(st.position)
        );
    }
    if let Some(gate) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == sys_id) {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, gate.stargate_id).await,
            Some(gate.position)
        );
    }
    if let Some(cel) = ctx.sde.celestials_in_system(sys_id).into_iter().next() {
        assert_eq!(
            resolve_system_target_position(&ctx, sys_id, char_id, cel.celestial_id).await,
            Some(cel.position)
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Nav Struct".into(),
                state: "ShieldVulnerable".into(),
                position: [25_000.0, -10_000.0, 5_000.0],
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                quantum_core_installed: true,
                fuel_blocks: 100,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "BM".into(),
                solar_system_id: sys_id,
                x: 50_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert_eq!(
        resolve_system_target_position(&ctx, sys_id, char_id, st_id).await,
        Some(DVec3::new(25_000.0, -10_000.0, 5_000.0))
    );

    let bound = bound_sys(sys_id, char_id);
    let mut sess = SessionState::new(745);
    sess.user_id = Some(50);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 2. Docked session rejected on GotoDirection, Stop, SetSpeedFraction, Orbit, AlignTo
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &docked, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_stop(&ctx, &docked, Some(&bound)).await.unwrap_err().code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_set_speed_fraction(&ctx, &docked, Some(&bound), &[PyRep::Real(0.5)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &docked, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(
        handle_cmd_align_to(&ctx, &docked, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    // 3. ShipNotInSpace before ball is spawned
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let _ = sys_h
        .spawn_ball(ship_ball(target_id, DVec3::new(20_000.0, 0.0, 0.0)))
        .await;

    // 4. Invalid arguments across movement commands
    assert_eq!(
        handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(f64::INFINITY), PyRep::Real(0.0), PyRep::Real(0.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(-0.5)])
            .await
            .unwrap_err()
            .code,
        "InvalidSpeedFraction"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "TargetNotFound"
    );
    assert_eq!(
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id), PyRep::Real(-10.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidRange"
    );

    // 5. AlignTo validation: <= 0, own ship, Abyssal bookmark, Zarzakh bookmark, and valid bookmarkID kwarg
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(0)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(ship_id)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    let mut kw_bm = HashMap::new();
    kw_bm.insert("bookmarkID".to_string(), PyRep::Integer(bm_id));
    assert!(handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &kw_bm).await.is_ok());

    let abyss_sys = 32_000_005_u32;
    let abyss_h = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let abyss_bound = bound_sys(abyss_sys, char_id);
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&abyss_bound), &[], &kw_bm).await.unwrap_err().code,
        "CannotWarpInAbyssalSpace"
    );

    let zarz_sys = 30_100_000_u32;
    let zarz_h = ctx.systems.get_or_spawn(zarz_sys);
    let _ = zarz_h.spawn_ball(ship_ball(ship_id, DVec3::ZERO)).await;
    let zarz_bound = bound_sys(zarz_sys, char_id);
    assert_eq!(
        handle_cmd_align_to(&ctx, &sess, Some(&zarz_bound), &[], &kw_bm).await.unwrap_err().code,
        "DunBlacklistCannotWarp"
    );

    let _ = sys_h
        .set_combat_state(ship_id, CombatState { mjfg_spool_remaining_s: 5.0, ..Default::default() })
        .await;
    assert_eq!(handle_cmd_stop(&ctx, &sess, Some(&bound)).await.unwrap_err().code, "CantMoveWhileModuleActive");
}

#[tokio::test]
async fn sync_tether_disallow_conditions_and_decloak_velocity_restoration() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let corp_id = 98_444_001_i64;
    let char_id = 96_850_i64;
    let ship_id = 78_850_i64;
    let struct_id = 1_040_850_001_i64;
    let struct_tid = 35_832_u32;
    let proto_cloak_tid = 99_870_u32;

    if let Some(tid) = attr_id_by_name(&ctx, "tetheringRange") {
        clone_type_with(&mut ctx.sde, struct_tid, struct_tid, 1657, 65, &[(tid, 15_000.0)]);
    }
    let vel_attr = crate::services::space_navigation_advanced::max_velocity_cloak_attr_id(&ctx.sde).unwrap_or(564);
    clone_type_with(&mut ctx.sde, 11370, proto_cloak_tid, 330, 7, &[(vel_attr, 0.25)]);

    let cloak_item_id = 78_851_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 588, char_id, sys_id as i64, InventoryFlag::Hangar, 1);
        put_item(snap, cloak_item_id, proto_cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        snap.module_active_states.insert(cloak_item_id);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: struct_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Tether Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                position: [0.0, 0.0, 0.0],
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(ship_ball(ship_id, DVec3::new(2_000.0, 0.0, 0.0)))
        .await;

    // 1. Activate non-covert cloak: cloak_original_max_velocity = 300 / 0.25 = 1200.0 and ship is tethered
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_cloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked);
    assert!(!cs_cloaked.is_covert_cloak);
    assert!(cs_cloaked.is_tethered);
    assert_eq!(cs_cloaked.cloak_original_max_velocity, Some(1200.0));

    // 2. Deactivate cloak: original max_velocity (1200.0) is restored
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_item_id);
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let (_, b_uncloaked) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_uncloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_uncloaked.is_cloaked);
    assert!((b_uncloaked.max_velocity - 1200.0).abs() < 1e-6);

    // 3. Lance disruption breaks tether
    let mut cs_lance = cs_uncloaked;
    cs_lance.lance_disrupted_remaining_s = 10.0;
    let _ = sys_h.set_combat_state(ship_id, cs_lance).await;
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let cs_after = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_after.is_tethered);
}
