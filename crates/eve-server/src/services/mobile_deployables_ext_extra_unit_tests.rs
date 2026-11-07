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

#[cfg(test)]
#[path = "mobile_deployables_mmju_and_siphon_unit_tests.rs"]
mod mobile_deployables_mmju_and_siphon_unit_tests;

pub(super) fn mk_rec(
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
