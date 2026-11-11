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
    active_scan_inhibitor_positions, caller_fleet_id, default_deploy_delay_secs,
    is_pos_scan_inhibited, total_lifetime_secs, validate_ext_deployment_restrictions,
    GROUP_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SIPHON, TYPE_HIGHSEC_AUTH_CYNO_BEACON,
    TYPE_MOBILE_SCAN_INHIBITOR,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::DVec3;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[test]
fn coverage_sweep_deploy_delay_cyno_jammer_deadspace_scan_inhibitor_mmju_siphon() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // 1. default_deploy_delay_secs with attribute 556 in SDE (L81)
    if let Some((&tid, t)) = ctx.sde.types.iter().find(|(_, t)| t.default_attributes.contains_key(&556)) {
        let d = default_deploy_delay_secs(&ctx.sde, tid, t.group_id);
        assert!(d >= 1.0);
    }

    // 2. CynoJammed in Pochven (region_id == 10_000_070, sec < 0.45 -> L209-212)
    if let Some(&poch_sys) = ctx.sde.solar_systems.iter().find(|(_, s)| s.region_id == 10_000_070).map(|(id, _)| id) {
        let mut fleet_sess = sess.clone();
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                8801,
                FleetRecord {
                    fleet_id: 8801,
                    boss_id: 90_000_001,
                    members: vec![90_000_001],
                    wings: vec![1],
                    squads: vec![1],
                    free_move: false,
                    is_registered: false,
                    member_details: std::collections::HashMap::new(),
                    wing_details: std::collections::HashMap::new(),
                    squad_details: std::collections::HashMap::new(),
                    invites: std::collections::HashSet::new(),
                    motd: String::new(),
                },
            );
        });
        fleet_sess.fleet_id = Some(8801);
        assert_eq!(
            validate_ext_deployment_restrictions(
                &ctx,
                &fleet_sess,
                1,
                TYPE_MOBILE_CYNO_BEACON,
                GROUP_MOBILE_CYNO_BEACON,
                poch_sys,
                DVec3::ZERO,
            )
            .unwrap_err()
            .code,
            "CynoJammedInPochven"
        );
    }

    // 3. Mobile Observatory CannotDeployInDeadspace (L263-273)
    let ls_sys = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(&id, s)| {
            s.security_status < 0.45
                && s.region_id != 10_000_070
                && ctx.sde.jumps.values().any(|j| j.from_solar_system_id == id)
        })
        .map(|(&id, _)| id)
        .unwrap_or(30_000_144);
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(ls_sys).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "DEADSPACE-1".into(),
                dungeon_id: 4242,
                solar_system_id: ls_sys,
                position: [1_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            2,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            ls_sys,
            DVec3::new(10_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "CannotDeployInDeadspace"
    );

    // 4. Mobile Scan Inhibitor: RestrictionsInEffect (Zarzakh / Abyssal), gate, station, structure (L278-314)
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            3,
            TYPE_MOBILE_SCAN_INHIBITOR,
            GROUP_MOBILE_SCAN_INHIBITOR,
            30_100_000,
            DVec3::ZERO,
        )
        .unwrap_err()
        .code,
        "RestrictionsInEffect"
    );
    if let Some(j) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == ls_sys) {
        let gpos = DVec3::new(j.position.x, j.position.y, j.position.z);
        assert_eq!(
            validate_ext_deployment_restrictions(
                &ctx,
                &sess,
                4,
                TYPE_MOBILE_SCAN_INHIBITOR,
                GROUP_MOBILE_SCAN_INHIBITOR,
                ls_sys,
                gpos + DVec3::new(1_000.0, 0.0, 0.0),
            )
            .unwrap_err()
            .code,
            "DeploymentTooCloseToGate"
        );
    }
    if let Some(st) = ctx.sde.stations.values().next() {
        let spos = DVec3::new(st.position.x, st.position.y, st.position.z);
        assert_eq!(
            validate_ext_deployment_restrictions(
                &ctx,
                &sess,
                5,
                TYPE_MOBILE_SCAN_INHIBITOR,
                GROUP_MOBILE_SCAN_INHIBITOR,
                st.solar_system_id,
                spos + DVec3::new(1_000.0, 0.0, 0.0),
            )
            .unwrap_err()
            .code,
            "DeploymentTooCloseToStation"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_999_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_999_001,
                type_id: 35832,
                owner_corp_id: 1_000_009,
                solar_system_id: ls_sys,
                name: "Upwell".into(),
                state: "ShieldVulnerable".into(),
                position: [5_000_000_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.item_positions.insert(1_000_000_999_001, [5_000_000_000.0, 0.0, 0.0]);
    });
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            6,
            TYPE_MOBILE_SCAN_INHIBITOR,
            GROUP_MOBILE_SCAN_INHIBITOR,
            ls_sys,
            DVec3::new(5_000_001_000.0, 0.0, 0.0),
        )
        .unwrap_err()
        .code,
        "DeploymentTooCloseToStructure"
    );

    // 5. MMJU & Siphon RestrictionsInEffect in Abyssal / Zarzakh (L336, L362)
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            7,
            33475,
            GROUP_MOBILE_MICRO_JUMP_UNIT,
            32_000_001,
            DVec3::ZERO,
        )
        .unwrap_err()
        .code,
        "RestrictionsInEffect"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            8,
            33477,
            GROUP_MOBILE_SIPHON,
            30_100_000,
            DVec3::ZERO,
        )
        .unwrap_err()
        .code,
        "RestrictionsInEffect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mmju_spool_blocker_flags_siphon_gate_and_flush_side_effects_mutants() {
    use crate::services::mobile_deployables::read_deployable_state;
    use crate::services::mobile_deployables_ext::{
        flush_deployable_side_effects, handle_mmju_activate, step_system_deployables_sync,
    };
    use eve_db::{MmjuSpoolEntry, PendingDecloakNotice};
    use eve_destiny::{Ball, CombatState, MovementMode};

    let ctx = test_ctx();
    let ls_sys = 30_002_537u32;
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let mmju_id = 772_001i64;
    let siphon_id = 772_003i64;
    let pos_tower_id = 772_004i64;
    let raw_stack_id = 772_005i64;
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
        snap.owner_notes.clear();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ls_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, qty) in [
            (ship_id, 600u32, ls_sys as i64, 1),
            (mmju_id, mmju_tid, ls_sys as i64, 1),
            (siphon_id, siphon_tid, ls_sys as i64, 1),
            (pos_tower_id, pos_tid, ls_sys as i64, 1),
            (raw_stack_id, raw_mat_tid, pos_tower_id, 500),
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
                    singleton: id != raw_stack_id,
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
        st.deployables.insert(
            mmju_id,
            mk_rec(
                mmju_id,
                mmju_tid,
                GROUP_MOBILE_MICRO_JUMP_UNIT,
                char_id,
                ls_sys,
                origin.to_array(),
                now - 120,
                60,
                now + 36_000,
            ),
        );
    });

    let sys = ctx.systems.get_or_spawn(ls_sys);
    let ship_pos = origin + DVec3::new(1_000.0, 0.0, 0.0);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: ship_pos,
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
            CombatState {
                is_mwd_mjd_disabled: true,
                is_cloaked: false,
                ..CombatState::new()
            },
        )
        .await;

    // 1. handle_mmju_activate: not scrambled + is_mwd_mjd_disabled == true -> WarpScrambled (kills L557 ||->&&)
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    // Clear blocker -> handle_mmju_activate returns non-default keyval (kills Ok(Default::default()))
    let mut cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.is_mwd_mjd_disabled = false;
    let _ = sys.set_combat_state(ship_id, cs.clone()).await;
    let act_rep = handle_mmju_activate(&ctx, &sess, char_id, ship_id, ls_sys, mmju_id)
        .await
        .expect("happy mmju activate");
    assert_eq!(
        act_rep.dict_get("unitID"),
        Some(&eve_proto::PyRep::Integer(mmju_id))
    );
    assert!(act_rep.dict_get("completeAtUnix").is_some());

    // 2. step_mmju_spools_in_actor blocker checks (not scrambled):
    //    (a) is_cloaked == true, is_mwd_mjd_disabled == false -> must NOT jump (kills L741 & L742 ||->&&)
    cs.is_cloaked = true;
    cs.is_mwd_mjd_disabled = false;
    let _ = sys.set_combat_state(ship_id, cs.clone()).await;
    write_deployable_state(&ctx, |st| {
        st.mmju_spools.get_mut(&ship_id).unwrap().complete_at_unix = now;
    });
    step_system_deployables_sync(&ctx, ls_sys);
    let (_, still_ball_cloak) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((still_ball_cloak.position - ship_pos).length() < 5_000.0);

    //    (b) is_cloaked == false, is_mwd_mjd_disabled == true -> must NOT jump
    {
        let mut bp = sys.ballpark.write().unwrap();
        let bcs = bp.combat_states.get_mut(&ship_id).unwrap();
        bcs.is_cloaked = false;
        bcs.is_mwd_mjd_disabled = true;
    }
    write_deployable_state(&ctx, |st| {
        st.mmju_spools.insert(
            ship_id,
            MmjuSpoolEntry {
                ship_id,
                unit_id: mmju_id,
                char_id,
                solar_system_id: ls_sys,
                started_unix: now - 15,
                complete_at_unix: now,
                direction: [1.0, 0.0, 0.0],
            },
        );
    });
    step_system_deployables_sync(&ctx, ls_sys);
    let (_, still_ball_scram) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((still_ball_scram.position - ship_pos).length() < 5_000.0);

    //    (c) Unblocked spool + step_system_deployables_sync (owner_notes empty) -> jumps 100 km!
    //        (kills step_system_deployables_sync -> (), L1221 &&->||, step_mmju_spools_in_actor -> (), L644 !)
    cs.is_cloaked = false;
    cs.is_mwd_mjd_disabled = false;
    let _ = sys.set_combat_state(ship_id, cs).await;
    write_deployable_state(&ctx, |st| {
        st.mmju_spools.insert(
            ship_id,
            MmjuSpoolEntry {
                ship_id,
                unit_id: mmju_id,
                char_id,
                solar_system_id: ls_sys,
                started_unix: now - 15,
                complete_at_unix: now,
                direction: [1.0, 0.0, 0.0],
            },
        );
    });
    step_system_deployables_sync(&ctx, ls_sys);
    let (_, jumped_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((jumped_ball.position - ship_pos).length() > 90_000.0);

    // 3. step_siphons_in_db via step_system_deployables_sync with ONLY a GROUP_MOBILE_SIPHON deployable
    //    and empty owner_notes (kills step_siphons_in_db -> (), L976 &&->||, L975 !, L975 ==->!=)
    write_deployable_state(&ctx, |st| {
        st.deployables.clear();
        st.deployables.insert(
            siphon_id,
            mk_rec(
                siphon_id,
                siphon_tid,
                GROUP_MOBILE_SIPHON,
                char_id,
                ls_sys,
                origin.to_array(),
                now - 7_200,
                60,
                now + 36_000,
            ),
        );
    });
    step_system_deployables_sync(&ctx, ls_sys);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items[&raw_stack_id].quantity < 500, "siphon must drain raw moon material");
        let stolen_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == siphon_id && i.type_id == raw_mat_tid)
            .map(|i| i.quantity)
            .sum();
        assert!(stolen_qty > 0, "siphon must hold stolen material");
    });

    // 4. flush_deployable_side_effects with pending_decloak_notifications non-empty and owner_notes empty
    //    (kills flush_deployable_side_effects -> (), L1189 ||->&&, L1188 !)
    let _ = ctx.drain_notifications_for_char(char_id);
    write_deployable_state(&ctx, |st| {
        st.pending_decloak_notifications.push(PendingDecloakNotice {
            char_id,
            ship_id,
            module_id: 772_099,
        });
    });
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id));
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    flush_deployable_side_effects(&ctx);
    assert!(read_deployable_state(&ctx).pending_decloak_notifications.is_empty());
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnCloakModuleForceDeactivated"));
}
