//! Extra unit and mutation-killing tests for `mobile_deployables_ext`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::mobile_deployables::{
    write_deployable_state, MobileDeployableRecord, GROUP_MOBILE_CYNO_BEACON,
    GROUP_MOBILE_CYNO_INHIBITOR, GROUP_MOBILE_DEPOT, GROUP_MOBILE_MICRO_JUMP_UNIT,
    GROUP_MOBILE_OBSERVATORY, TYPE_COVERT_MOBILE_CYNO_BEACON, TYPE_MOBILE_CYNO_BEACON,
    TYPE_MOBILE_OBSERVATORY,
};
use crate::services::mobile_deployables_ext::{
    active_scan_inhibitor_positions, caller_fleet_id, clear_ship_cloak_activated,
    default_deploy_delay_secs, flush_deployable_side_effects, handle_mmju_activate,
    is_pos_scan_inhibited, record_ship_cloak_activated, step_ext_deployables_in_actor,
    step_system_deployables_sync, total_lifetime_secs, validate_ext_deployment_restrictions,
    GROUP_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SIPHON, TYPE_HIGHSEC_AUTH_CYNO_BEACON,
    TYPE_MOBILE_SCAN_INHIBITOR,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, CombatState, DVec3, MovementMode, SlimItemMeta};

fn mk_rec(
    item_id: i64,
    type_id: u32,
    group_id: u32,
    owner_id: i64,
    solar_system_id: u32,
    position: [f64; 3],
    anchor_start_unix: u64,
    anchor_duration_secs: u64,
    expires_at_unix: u64,
) -> MobileDeployableRecord {
    MobileDeployableRecord {
        item_id,
        type_id,
        group_id,
        owner_id,
        solar_system_id,
        anchor_start_unix,
        anchor_duration_secs,
        anchor_completed: true,
        is_unanchored: false,
        expires_at_unix,
        last_interaction_unix: 0,
        reinforced_until_unix: None,
        last_ping_unix: None,
        position,
        deployed_fleet_id: None,
    }
}

#[tokio::test]
async fn deploy_delay_fallbacks_fleet_resolution_and_restriction_matrix() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_002_537);
    let pos = DVec3::new(1.0e10, 1.0e10, 1.0e10);
    let ls_sys = 30_002_537u32;
    let poch_sys = *ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| &s.solar_system_id)
        .unwrap_or(&30_000_142);
    let mmju_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == GROUP_MOBILE_MICRO_JUMP_UNIT)
        .map(|t| t.type_id)
        .unwrap_or(33478);

    // default_deploy_delay_secs fallback branches for unknown type_id across all groups
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_991, GROUP_MOBILE_OBSERVATORY),
        600.0
    );
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_992, GROUP_MOBILE_CYNO_INHIBITOR),
        60.0
    );
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_993, GROUP_MOBILE_CYNO_BEACON),
        60.0
    );
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_994, GROUP_MOBILE_MICRO_JUMP_UNIT),
        60.0
    );
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_995, GROUP_MOBILE_SCAN_INHIBITOR),
        60.0
    );
    assert_eq!(
        default_deploy_delay_secs(&ctx.sde, 999_999_996, GROUP_MOBILE_DEPOT),
        60.0
    );
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 999_999_997, 999_999), 60.0);
    assert_eq!(total_lifetime_secs(0, 999_999, 60), 2 * 86_400);

    // caller_fleet_id via snap.fleets when session.fleet_id is None
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            8801,
            FleetRecord {
                fleet_id: 8801,
                boss_id: 90_000_001,
                members: vec![90_000_001],
                ..Default::default()
            },
        );
        assert_eq!(caller_fleet_id(&sess, snap, 90_000_001), Some(8801));
    });
    sess.fleet_id = Some(8801);

    // Cyno beacon restrictions: Zarzakh, Wormhole, Pochven, Abyssal, Highsec Auth Beacon, and Cyno Inhibitor in lowsec
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_CYNO_BEACON,
            GROUP_MOBILE_CYNO_BEACON,
            30_100_000,
            pos,
        )
        .unwrap_err()
        .code,
        "CynoDisallowedInZarzakh"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_CYNO_BEACON,
            GROUP_MOBILE_CYNO_BEACON,
            31_000_001,
            pos,
        )
        .unwrap_err()
        .code,
        "CantAnchorInWormhole"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_CYNO_BEACON,
            GROUP_MOBILE_CYNO_BEACON,
            poch_sys,
            pos,
        )
        .unwrap_err()
        .code,
        "CynoJammedInPochven"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_CYNO_BEACON,
            GROUP_MOBILE_CYNO_BEACON,
            32_000_001,
            pos,
        )
        .unwrap_err()
        .code,
        "CantAnchorInAbyssal"
    );
    // Highsec Auth Cyno Beacon is allowed in Jita (30000142)
    assert!(validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        1,
        TYPE_HIGHSEC_AUTH_CYNO_BEACON,
        GROUP_MOBILE_CYNO_BEACON,
        30_000_142,
        pos,
    )
    .is_ok());

    // Active Mobile Cyno Inhibitor within 100 km blocks normal cyno beacon, allows covert cyno beacon
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_001,
            InventoryItemRecord {
                item_id: 770_001,
                type_id: 33476,
                owner_id: 90_000_001,
                location_id: ls_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(770_001, pos.to_array());
    });
    write_deployable_state(&ctx, |st| {
        st.deployables.insert(
            770_001,
            mk_rec(770_001, 33476, GROUP_MOBILE_CYNO_INHIBITOR, 90_000_001, ls_sys, pos.to_array(), now - 200, 60, now + 3_600),
        );
    });
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            2,
            TYPE_MOBILE_CYNO_BEACON,
            GROUP_MOBILE_CYNO_BEACON,
            ls_sys,
            pos + DVec3::new(10_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "CynoInhibited"
    );
    assert!(validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        2,
        TYPE_COVERT_MOBILE_CYNO_BEACON,
        GROUP_MOBILE_CYNO_BEACON,
        ls_sys,
        pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .is_ok());

    // Observatory in Pochven / Abyssal / lowsec
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            3,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            poch_sys,
            pos,
        )
        .unwrap_err()
        .code,
        "CannotDeployInPochven"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            3,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            32_000_001,
            pos,
        )
        .unwrap_err()
        .code,
        "CannotDeployInAbyssalSpace"
    );
    assert!(validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        3,
        TYPE_MOBILE_OBSERVATORY,
        GROUP_MOBILE_OBSERVATORY,
        ls_sys,
        pos,
    )
    .is_ok());

    // Scan Inhibitor proximity to another Scan Inhibitor (< 100 km) & active_scan_inhibitor_positions
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_002,
            InventoryItemRecord {
                item_id: 770_002,
                type_id: TYPE_MOBILE_SCAN_INHIBITOR,
                owner_id: 90_000_001,
                location_id: ls_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(770_002, pos.to_array());
    });
    write_deployable_state(&ctx, |st| {
        st.deployables.insert(
            770_002,
            mk_rec(
                770_002,
                TYPE_MOBILE_SCAN_INHIBITOR,
                GROUP_MOBILE_SCAN_INHIBITOR,
                90_000_001,
                ls_sys,
                pos.to_array(),
                now - 120,
                60,
                now + 3_600,
            ),
        );
    });
    let inhibs = active_scan_inhibitor_positions(&ctx, ls_sys);
    assert_eq!(inhibs.len(), 1);
    assert!(is_pos_scan_inhibited(
        &inhibs,
        pos + DVec3::new(15_000.0, 0.0, 0.0)
    ));
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            4,
            TYPE_MOBILE_SCAN_INHIBITOR,
            GROUP_MOBILE_SCAN_INHIBITOR,
            ls_sys,
            pos + DVec3::new(50_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "DeploymentTooCloseToSameType"
    );

    // MMJU proximity (< 6 km fails DeploymentTooCloseToSameType, > 6 km succeeds)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            770_003,
            InventoryItemRecord {
                item_id: 770_003,
                type_id: mmju_tid,
                owner_id: 90_000_001,
                location_id: ls_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(770_003, pos.to_array());
    });
    write_deployable_state(&ctx, |st| {
        st.deployables.insert(
            770_003,
            mk_rec(
                770_003,
                mmju_tid,
                GROUP_MOBILE_MICRO_JUMP_UNIT,
                90_000_001,
                ls_sys,
                pos.to_array(),
                now - 120,
                60,
                now + 3_600,
            ),
        );
    });
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            5,
            mmju_tid,
            GROUP_MOBILE_MICRO_JUMP_UNIT,
            ls_sys,
            pos + DVec3::new(3_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "DeploymentTooCloseToSameType"
    );
    assert!(validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        5,
        mmju_tid,
        GROUP_MOBILE_MICRO_JUMP_UNIT,
        ls_sys,
        pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .is_ok());

    // Siphon restrictions: Highsec and NoSiphonTargetInRange
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            6,
            999_999_998,
            GROUP_MOBILE_SIPHON,
            30_000_142,
            pos,
        )
        .unwrap_err()
        .code,
        "CantAnchorInHighSec"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            6,
            33477,
            GROUP_MOBILE_SIPHON,
            ls_sys,
            pos,
        )
        .unwrap_err()
        .code,
        "NoSiphonTargetInRange"
    );
}

#[tokio::test]
async fn mmju_activate_and_observatory_siphon_actor_ticks() {
    let ctx = test_ctx();
    let ls_sys = 30_002_537u32;
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let mmju_id = 771_001i64;
    let obs_id = 771_002i64;
    let siphon_id = 771_003i64;
    let pos_tower_id = 771_004i64;
    let mut sess = auth_session(1, char_id, 98_000_001, ls_sys);
    sess.ship_id = Some(ship_id);
    let now = ctx.now_unix();
    let origin = DVec3::new(2.0e10, 0.0, 0.0);

    let mmju_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == GROUP_MOBILE_MICRO_JUMP_UNIT)
        .map(|t| t.type_id)
        .unwrap_or(33478);
    let siphon_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.group_id == GROUP_MOBILE_SIPHON
                && t.default_attributes.get(&1928).copied().unwrap_or(0.0) > 0.0
        })
        .map(|t| t.type_id)
        .unwrap_or(33477);
    let pos_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 365)
        .map(|t| t.type_id)
        .unwrap_or(12235);
    let raw_mat_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 427 && t.volume > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(16633);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ls_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, qty) in [
            (ship_id, 600u32, ls_sys as i64, 1),
            (mmju_id, mmju_tid, ls_sys as i64, 1),
            (obs_id, TYPE_MOBILE_OBSERVATORY, ls_sys as i64, 1),
            (siphon_id, siphon_tid, ls_sys as i64, 1),
            (pos_tower_id, pos_tid, ls_sys as i64, 1),
            (771_005i64, raw_mat_tid, pos_tower_id, 500),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: qty,
                    singleton: id != 771_005,
                    version: 1,
                    ..Default::default()
                },
            );
            if loc == ls_sys as i64 {
                snap.item_positions.insert(id, origin.to_array());
            }
        }
    });

    write_deployable_state(&ctx, |st| {
        for (id, tid, gid, a_start, a_dur) in [
            (
                mmju_id,
                mmju_tid,
                GROUP_MOBILE_MICRO_JUMP_UNIT,
                now - 120,
                60,
            ),
            (
                obs_id,
                TYPE_MOBILE_OBSERVATORY,
                GROUP_MOBILE_OBSERVATORY,
                now - 1_200,
                600,
            ),
            (siphon_id, siphon_tid, GROUP_MOBILE_SIPHON, now - 7_200, 60),
        ] {
            st.deployables.insert(
                id,
                mk_rec(
                    id,
                    tid,
                    gid,
                    char_id,
                    ls_sys,
                    origin.to_array(),
                    a_start,
                    a_dur,
                    now + 36_000,
                ),
            );
        }
    });

    // MMJU activate without ship ball in space fails ShipNotInSpace
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball within 5 km of MMJU -> activate starts spool -> step_system_deployables_sync jumps ship 100 km
    let sys = ctx.systems.get_or_spawn(ls_sys);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: origin + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::new(100.0, 0.0, 0.0),
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1.0e7,
                radius: 50.0,
                speed_fraction: 0.5,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            },
            CombatState::new(),
        )
        .await;

    let act_res = handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
        .await
        .expect("mmju activate");
    assert!(act_res.dict_get("completeAtUnix").is_some());

    // Advance spool time so step_system_deployables_sync executes the 100 km jump
    write_deployable_state(&ctx, |st| {
        if let Some(entry) = st.mmju_spools.get_mut(&ship_id) {
            entry.complete_at_unix = now;
        }
    });
    step_system_deployables_sync(&ctx, ls_sys);
    let (_, moved_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((moved_ball.position - origin).length() > 90_000.0);

    // Test record_ship_cloak_activated, clear_ship_cloak_activated, and step_ext_deployables_in_actor (Observatory + Siphon)
    record_ship_cloak_activated(&ctx, ship_id);
    clear_ship_cloak_activated(&ctx, ship_id);

    let mut bp = Ballpark::new(ls_sys);
    let mut cloaked_cs = CombatState::new();
    cloaked_cs.is_cloaked = true;
    cloaked_cs.cloak_stabilization_remaining_s = 0.0;
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 600,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1.0e7,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(ship_id, cloaked_cs);
    bp.ball_slim_meta.insert(
        ship_id,
        SlimItemMeta {
            owner_id: char_id,
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    );

    for tick_offset in 0..20 {
        step_ext_deployables_in_actor(
            &mut bp,
            None,
            None,
            &ctx.sde,
            &ctx.db,
            Some(&ctx.mechanic_rng),
            ls_sys,
            now + tick_offset * 600,
        );
    }
    flush_deployable_side_effects(&ctx);

    // Verify Siphon stole raw moon material into siphon_id and wasted from pos_tower_id
    ctx.db.with_snapshot(|snap| {
        let tower_rem = snap.items[&771_005].quantity;
        assert!(tower_rem < 500);
        let stolen_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == siphon_id && i.type_id == raw_mat_tid)
            .map(|i| i.quantity)
            .sum();
        assert!(stolen_qty > 0);
    });
}
