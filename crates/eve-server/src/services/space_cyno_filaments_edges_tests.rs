//! Edge-case behavioral tests for `space_cyno_filaments`.

#![forbid(unsafe_code)]

use super::space_cyno_filaments_tests::{
    clone_type_with, make_session, online_pharolux, put_item, spawn_ship, test_ctx,
};
use super::*;
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use eve_db::types::FleetRecord;
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wormhole_jump_validation_edges_and_cancel_notification() {
    let mut ctx = test_ctx();
    let cur_sys = 30_000_142u32; // Jita
    let dest_sys = 31_000_001u32;
    let char_id = 95_801i64;
    let ship_id = 77_801i64;
    let wh_id = 88_801i64;
    let fake_ball_id = 88_802i64;

    // Titan type (group 30) and standard frigate (587)
    clone_type_with(&mut ctx.sde, 587, 99_801, 30, 6, 100.0, 500.0, &[]);

    let mut sess = make_session(char_id, 98_000_001, Some(cur_sys), Some(ship_id));
    sess.station_id = Some(60_003_760);

    // 1. Docked -> ShipDocked
    let err_docked = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");
    sess.station_id = None;

    // 2. Supercapital/Titan restricted
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.solar_system_id = cur_sys;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 99_801, char_id, i64::from(cur_sys), InventoryFlag::None, 1, true);
    });
    let sys_h = ctx.systems.get_or_spawn(cur_sys);
    spawn_ship(&sys_h, ship_id, 99_801, 30, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    let err_titan = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_titan.code, "ShipRestrictedFromWormhole");

    // Swap to regular frigate (587, group 25) and missing ballID arg
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
    });
    let _ = sys_h.remove_ball(ship_id).await;
    spawn_ship(&sys_h, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    let err_missing_arg = handle_wormhole_jump(&ctx, &mut sess, &[]).await.unwrap_err();
    assert_eq!(err_missing_arg.code, "MissingWormholeBallId");

    // 3. Ball exists with type != 26272 -> WormholeNotPresent
    spawn_ship(&sys_h, fake_ball_id, 587, 25, 6, DVec3::new(100.0, 0.0, 0.0), DVec3::ZERO, false).await;
    let err_not_wh = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(fake_ball_id)])
        .await
        .unwrap_err();
    assert_eq!(err_not_wh.code, "WormholeNotPresent");

    // 4. Spawn actual wormhole ball (type 26272) at 5_000m -> OutsideWormholeJumpRange
    spawn_ship(&sys_h, wh_id, 26_272, 988, 2, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    let err_range = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_range.code, "OutsideWormholeJumpRange");

    // Move ship within 2500m, set active session change timer -> SessionChangeTimerActive
    let _ = sys_h.remove_ball(ship_id).await;
    spawn_ship(&sys_h, ship_id, 587, 25, 6, DVec3::new(4_500.0, 0.0, 0.0), DVec3::ZERO, false).await;
    crate::services::space_destiny::record_session_change_timer(
        &ctx,
        char_id,
        ctx.now_unix(),
        cur_sys,
        1,
        "test",
    );
    let err_sct = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_sct.code, "SessionChangeInProgress");
    ctx.advance_secs(15);

    // 5. Polarized -> WormholePolarized + OnWormholeJumpCancel
    let wh_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&char_id].user_id;
        snap.accounts.entry("wh_test_user".into()).or_insert_with(|| eve_db::AccountRecord {
            user_id: uid,
            username: "wh_test_user".into(),
            password_token: String::new(),
            role: 0,
            banned_until_unix: None,
            ban_reason: None,
        });
        uid
    });
    ctx.db
        .acquire_online_lock(wh_uid, sess.session_id, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(cur_sys).or_default();
        st.wormhole_destination_system_id = Some(dest_sys);
        st.wormhole_remaining_mass_kg = 1_000_000_000.0;
        snap.wh_polarizations.insert((char_id, i64::from(dest_sys)), now + 180);
    });
    let err_pol = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_pol.code, "WormholePolarized");
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnWormholeJumpCancel"));

    // 6. Insufficient remaining WH mass -> WormholeMassCollapsed
    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.clear();
        snap.system_states.get_mut(&cur_sys).unwrap().wormhole_remaining_mass_kg = 100_000.0;
    });
    let err_mass = handle_wormhole_jump(&ctx, &mut sess, &[PyRep::Integer(wh_id)])
        .await
        .unwrap_err();
    assert_eq!(err_mass.code, "WormholeMassCollapsed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_jump_filament_error_edges_and_fleetmate_standing_distance_filtering() {
    let mut ctx = test_ctx();
    let cur_sys = 30_000_142u32;
    let char_id = 95_811i64;
    let far_mate_id = 95_812i64;
    let ok_mate_id = 95_813i64;
    let ship_id = 77_811i64;
    let far_ship_id = 77_812i64;
    let ok_ship_id = 77_813i64;
    let needle_tid = 99_811u32;
    let poch_exit_tid = 56_066u32;
    let poch_entry_tid = 56_070u32;

    clone_type_with(&mut ctx.sde, 58958, needle_tid, 4041, 30, 0.1, 0.0, &[(3025, 4041.0)]);
    clone_type_with(&mut ctx.sde, 58958, poch_exit_tid, 4087, 30, 0.1, 0.0, &[(2832, 5.0)]);
    clone_type_with(&mut ctx.sde, 58958, poch_entry_tid, 4087, 30, 0.1, 0.0, &[(2832, 5.0)]);

    let mut fil_needle = 77_821i64;
    let mut fil_exit = 77_822i64;
    let mut fil_entry = 77_823i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char_id, ship_id), (far_mate_id, far_ship_id), (ok_mate_id, ok_ship_id)] {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = cid;
            ch.solar_system_id = cur_sys;
            ch.active_ship_id = sid;
            ch.station_id = None;
            snap.characters.insert(cid, ch);
            put_item(snap, sid, 588, cid, i64::from(cur_sys), InventoryFlag::None, 1, true);
        }
        fil_needle = put_item(snap, fil_needle, needle_tid, char_id, ship_id, InventoryFlag::Locked, 2, false);
        fil_exit = put_item(snap, fil_exit, poch_exit_tid, char_id, ship_id, InventoryFlag::Cargo, 1, false);
        fil_entry = put_item(snap, fil_entry, poch_entry_tid, char_id, ship_id, InventoryFlag::Cargo, 1, false);
    });

    let sys_h = ctx.systems.get_or_spawn(cur_sys);
    spawn_ship(&sys_h, ship_id, 588, 25, 6, DVec3::ZERO, DVec3::ZERO, true).await;
    spawn_ship(&sys_h, ok_ship_id, 588, 25, 6, DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, far_ship_id, 588, 25, 6, DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO, false).await;

    let mut sess = make_session(char_id, 98_000_001, Some(cur_sys), Some(ship_id));
    // 1. Cloaked -> CannotActivateFilamentWhileCloaked
    let err_cloak = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_needle)])
        .await
        .unwrap_err();
    assert_eq!(err_cloak.code, "CannotActivateFilamentWhileCloaked");

    // Respawn uncloaked; still not in fleet -> NotInFleet
    let _ = sys_h.remove_ball(ship_id).await;
    spawn_ship(&sys_h, ship_id, 588, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    let err_fleet = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_needle)])
        .await
        .unwrap_err();
    assert_eq!(err_fleet.code, "NotInFleet");

    // Join fleet with all 3 chars
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            7_901,
            FleetRecord {
                fleet_id: 7_901,
                boss_id: char_id,
                members: vec![char_id, ok_mate_id, far_mate_id],
                ..Default::default()
            },
        );
    });
    sess.fleet_id = Some(7_901);

    // 2. Locked filament -> ItemLocked
    let err_locked = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_needle)])
        .await
        .unwrap_err();
    assert_eq!(err_locked.code, "ItemLocked");

    // 3. Pochven exit from Jita -> PochvenExitFilamentRequiresPochvenOrigin
    let err_exit = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_exit)])
        .await
        .unwrap_err();
    assert_eq!(err_exit.code, "PochvenExitFilamentRequiresPochvenOrigin");

    // 4. Pochven entry from wormhole -> PochvenEntryFilamentDisallowedOrigin
    let wh_sys = ctx.systems.get_or_spawn(31_000_001);
    spawn_ship(&wh_sys, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    sess.solar_system_id = Some(31_000_001);
    let err_wh_entry = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_entry)])
        .await
        .unwrap_err();
    assert_eq!(err_wh_entry.code, "PochvenEntryFilamentDisallowedOrigin");

    // 5. Needlejack from Zarzakh (30100000) -> NeedlejackDisallowedOrigin
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fil_needle).unwrap().flag = InventoryFlag::Cargo;
    });
    let zz_sys = ctx.systems.get_or_spawn(30_100_000);
    spawn_ship(&zz_sys, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    sess.solar_system_id = Some(30_100_000);
    let err_zarzakh = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_needle)])
        .await
        .unwrap_err();
    assert_eq!(err_zarzakh.code, "NeedlejackDisallowedOrigin");

    // 6. Valid Needlejack from Jita teleports leader + ok_mate (<=6km), leaves far_mate (>6km) behind
    sess.solar_system_id = Some(cur_sys);
    let res = handle_random_jump_filament(&ctx, &mut sess, &[PyRep::Integer(fil_needle)])
        .await
        .unwrap();
    let dest = res.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].solar_system_id, dest);
        assert_eq!(snap.characters[&ok_mate_id].solar_system_id, dest);
        assert_eq!(snap.characters[&far_mate_id].solar_system_id, cur_sys);
        assert_eq!(snap.items[&fil_needle].quantity, 1);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyno_beacon_validation_group_jump_same_system_and_cone_scan_inhibitor_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 95_821i64;
    let ship_id = 77_825i64;
    let beacon_id = 1_000_800_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);

        let mut b = online_pharolux(beacon_id, sys_id, 98_000_002);
        b.quantum_core_installed = false;
        snap.structures.insert(beacon_id, b);
    });

    // Offline (no quantum core) -> StructureBeaconOffline
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_structure_cyno_beacon(snap, char_id, beacon_id, None)
                .unwrap_err()
                .code,
            "StructureBeaconOffline"
        );
    });

    // Enable core, empty fuel -> BeaconOutOfFuel
    ctx.db.with_mut_snapshot(|snap| {
        let b = snap.structures.get_mut(&beacon_id).unwrap();
        b.quantum_core_installed = true;
        b.fuel_blocks = 0;
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_structure_cyno_beacon(snap, char_id, beacon_id, None)
                .unwrap_err()
                .code,
            "BeaconOutOfFuel"
        );
    });

    // Refuel, wrong corp -> BeaconAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&beacon_id).unwrap().fuel_blocks = 100;
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_structure_cyno_beacon(snap, char_id, beacon_id, None)
                .unwrap_err()
                .code,
            "BeaconAccessDenied"
        );
    });

    // Same corp -> valid; CmdJumpToStructureBeacon into HighSec fails CynoBeaconJumpDisallowedHighSec,
    // while CmdGroupJumpToStructureBeacon from Jita to Jita fails InvalidJumpDestination
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&sys_h, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&beacon_id).unwrap().owner_corp_id = 98_000_001;
    });
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id));
    let highsec_err = handle_cmd_jump_to_structure_beacon(
        &ctx,
        &mut sess,
        &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(highsec_err.code, "CannotJumpToHighSecBeacon");

    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status < 0.45
                && (30_000_000..31_000_000).contains(&s.solar_system_id)
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
        })
        .unwrap()
        .solar_system_id;
    let low_sys_h = ctx.systems.get_or_spawn(low_sys);
    spawn_ship(&low_sys_h, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&beacon_id).unwrap().solar_system_id = low_sys;
    });
    sess.solar_system_id = Some(low_sys);
    let same_sys_err = handle_cmd_group_jump_to_structure_beacon(
        &ctx,
        &mut sess,
        &[PyRep::Integer(low_sys as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(same_sys_err.code, "InvalidJumpDestination");
    sess.solar_system_id = Some(sys_id);

    // CmdBeaconJumpFleet without matching fleet fails FleetNotInFleet
    let not_fleet_err = handle_cmd_beacon_jump_fleet(
        &ctx,
        &mut sess,
        &[PyRep::Integer(char_id), PyRep::Integer(7_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(not_fleet_err.code, "FleetNotInFleet");

    // ConeScan edges: Docked error & Mobile Scan Inhibitor hides targets inside 30 km
    sess.station_id = Some(60_003_760);
    let dscan_docked = handle_cone_scan(&ctx, &sess, None, "ConeScan", &[])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(dscan_docked.code, "CannotDirectionalScanWhileDocked");
    sess.station_id = None;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&sys_h, ship_id, 587, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    let visible_id = 77_830i64;
    let inhibited_id = 77_831i64;
    let cloaked_id = 77_832i64;
    spawn_ship(&sys_h, visible_id, 587, 25, 6, DVec3::new(100_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, inhibited_id, 587, 25, 6, DVec3::new(200_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, cloaked_id, 587, 25, 6, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, true).await;

    let inhib_id = 77_835i64;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, inhib_id, 33589, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                inhib_id,
                MobileDeployableRecord {
                    item_id: inhib_id,
                    type_id: 33589,
                    group_id: 1275,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    anchor_start_unix: 1_000,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: u64::MAX,
                    last_interaction_unix: 1_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [205_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });
    let dscan = handle_cone_scan(
        &ctx,
        &sess,
        None,
        "ConeScan",
        &[
            PyRep::Real(std::f64::consts::TAU),
            PyRep::Real(1_000_000.0),
            PyRep::Real(1.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(rows) = dscan else { panic!("expected dscan list") };
    let seen_ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64()))
        .collect();
    assert!(seen_ids.contains(&visible_id), "visible target should appear");
    assert!(!seen_ids.contains(&inhibited_id), "inhibited target within 30km must be hidden");
    assert!(!seen_ids.contains(&cloaked_id), "cloaked target must be hidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyno_and_filament_jumps_record_session_arrival_jump_and_rollback_on_wake_failure() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct FailTargetWakeProvider {
        target_sys: Option<u32>,
        except_sys: Option<u32>,
        should_fail: Arc<AtomicBool>,
    }
    impl crate::system_actor::SystemContentProvider for FailTargetWakeProvider {
        fn name(&self) -> &'static str {
            "test_cyno_filament_fail_wake"
        }

        fn seed(
            &self,
            seed_ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
        ) -> Result<(), String> {
            if !self.should_fail.load(Ordering::Acquire) {
                return Ok(());
            }
            let hit = self.target_sys == Some(seed_ctx.solar_system_id)
                || self
                    .except_sys
                    .is_some_and(|allowed| seed_ctx.solar_system_id != allowed);
            if hit {
                return Err(format!("injected wake failure for {}", seed_ctx.solar_system_id));
            }
            Ok(())
        }
    }

    let kw = HashMap::new();

    // 1. Random jump filament wakes cold dest with SessionArrival(Jump) and rolls back on wake failure.
    let needlejack_tid = 53_947_u32;
    let wh_sys = 31_000_001_u32;
    let fil_char = 90_000_001_i64;
    let fil_ship = 100_000_901_i64;
    let fil_item = 930_901_i64;
    let pos = DVec3::new(1.5e13, -2.1e13, 0.9e13);

    {
        let mut ctx = test_ctx();
        clone_type_with(
            &mut ctx.sde,
            needlejack_tid,
            needlejack_tid,
            4041,
            66,
            1.0,
            0.0,
            &[(3025, 0.0), (2832, 5.0)],
        );
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&fil_char) {
                ch.solar_system_id = wh_sys;
                ch.active_ship_id = fil_ship;
                ch.station_id = None;
                ch.structure_id = None;
            }
            put_item(snap, fil_ship, 587, fil_char, i64::from(wh_sys), InventoryFlag::None, 1, true);
            put_item(snap, fil_item, needlejack_tid, fil_char, fil_ship, InventoryFlag::Cargo, 2, false);
        });
        let wh_h = ctx.systems.get_or_spawn(wh_sys);
        spawn_ship(&wh_h, fil_ship, 587, 25, 6, pos, DVec3::ZERO, false).await;
        let mut sess = make_session(fil_char, 98_000_001, Some(wh_sys), Some(fil_ship));
        sess.user_id = Some(1);
        handle(
            &ctx,
            &mut sess,
            None,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(fil_item)],
            &kw,
        )
        .await
        .expect("handled")
        .expect("random jump filament succeeds");
        let dest_sys = sess.solar_system_id.unwrap();
        assert_ne!(dest_sys, wh_sys);
        assert_eq!(
            ctx.systems.last_wake_reason(dest_sys),
            Some(crate::system_actor::SystemWakeReason::SessionArrival(
                crate::system_actor::SessionArrivalWakeReason::Jump,
            ))
        );
    }

    {
        let mut ctx_fail = test_ctx();
        clone_type_with(
            &mut ctx_fail.sde,
            needlejack_tid,
            needlejack_tid,
            4041,
            66,
            1.0,
            0.0,
            &[(3025, 0.0), (2832, 5.0)],
        );
        ctx_fail
            .systems
            .register_content_provider(Arc::new(FailTargetWakeProvider {
                target_sys: None,
                except_sys: Some(wh_sys),
                should_fail: Arc::new(AtomicBool::new(true)),
            }))
            .expect("register fail provider");
        ctx_fail.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&fil_char) {
                ch.solar_system_id = wh_sys;
                ch.active_ship_id = fil_ship;
                ch.station_id = None;
                ch.structure_id = None;
            }
            put_item(snap, fil_ship, 587, fil_char, i64::from(wh_sys), InventoryFlag::None, 1, true);
            put_item(snap, fil_item, needlejack_tid, fil_char, fil_ship, InventoryFlag::Cargo, 2, false);
        });
        let wh_h = ctx_fail.systems.get_or_spawn(wh_sys);
        spawn_ship(&wh_h, fil_ship, 587, 25, 6, pos, DVec3::ZERO, false).await;
        let mut sess = make_session(fil_char, 98_000_001, Some(wh_sys), Some(fil_ship));
        sess.user_id = Some(1);
        let err = handle(
            &ctx_fail,
            &mut sess,
            None,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(fil_item)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("wake failure must roll back random jump filament");
        assert_eq!(err.code, "SystemTransitionFailed");
        assert!(wh_h.get_ball_snapshot(fil_ship).await.unwrap().is_some());
        assert_eq!(
            ctx_fail.db.with_snapshot(|s| s.items[&fil_item].quantity),
            2
        );
        assert_eq!(
            ctx_fail.db.with_snapshot(|s| s.characters[&fil_char].solar_system_id),
            wh_sys
        );
    }

    // 2. Structure beacon & fleet beacon jumps wake cold dst_sys with SessionArrival(Jump) and roll back on Err.
    let mut ctx = test_ctx();
    let mut low_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status < 0.45
                && s.solar_system_id < 31_000_000
                && s.region_id != 10_000_070
                && s.solar_system_id != 30_100_000
        })
        .map(|s| s.solar_system_id)
        .collect();
    low_systems.sort_unstable();
    let cur_sys = low_systems[0];
    let dst_sys = low_systems
        .iter()
        .copied()
        .find(|&sid| {
            sid != cur_sys && super::super::space_nav_travel::distance_ly(&ctx, cur_sys, sid) <= 5.0
        })
        .expect("nearby low/null system within 5 LY");

    let jump_ship_tid = 99_620_u32;
    let iso_tid = 17_887_u32;
    let can_jump = super::super::space_nav_travel::attr_id_by_name(&ctx, "canJump").unwrap_or(861);
    let jd_range = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(867);
    let jd_amount = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(868);
    let jd_type = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(866);
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.type_lists.insert(
            99_318,
            eve_sde::TypeListInfo {
                type_list_id: 99_318,
                name: "All Test Cyno Beacons".into(),
                included_type_ids: vec![21096, 57425, 35840],
                ..Default::default()
            },
        );
    }
    clone_type_with(
        &mut ctx.sde,
        23757,
        jump_ship_tid,
        547,
        6,
        15_000.0,
        500.0,
        &[
            (can_jump, 1.0),
            (jd_range, 8.0),
            (jd_amount, 10.0),
            (jd_type, f64::from(iso_tid)),
            (1971, 1.0),
            (1972, 1.0),
            (3317, 99_318.0),
            (898, 0.25),
        ],
    );
    clone_type_with(&mut ctx.sde, 34, iso_tid, 423, 4, 0.05, 0.0, &[]);

    let corp_id = 98_910_001_i64;
    let struct_pilot = 98_910_101_i64;
    let fleet_pilot = 98_910_102_i64;
    let cyno_pilot = 98_910_103_i64;
    let struct_ship = 1_040_910_101_i64;
    let fleet_ship = 1_040_910_102_i64;
    let cyno_ship = 1_040_910_103_i64;
    let pharolux_id = 1_040_910_901_i64;
    let fleet_beacon_id = 1_040_910_902_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("demo char");
        for (cid, sid, sys) in [
            (struct_pilot, struct_ship, cur_sys),
            (fleet_pilot, fleet_ship, cur_sys),
            (cyno_pilot, cyno_ship, dst_sys),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys;
            ch.active_ship_id = sid;
            ch.station_id = None;
            ch.structure_id = None;
            ch.jump_cooldown_until_unix = 0;
            ch.jump_fatigue_until_unix = 0;
            snap.characters.insert(cid, ch);
        }
        put_item(snap, struct_ship, jump_ship_tid, struct_pilot, i64::from(cur_sys), InventoryFlag::None, 1, true);
        put_item(snap, fleet_ship, jump_ship_tid, fleet_pilot, i64::from(cur_sys), InventoryFlag::None, 1, true);
        put_item(snap, 77_901, iso_tid, struct_pilot, struct_ship, InventoryFlag::SpecializedFuelBay, 5_000, false);
        put_item(snap, 77_902, iso_tid, fleet_pilot, fleet_ship, InventoryFlag::SpecializedFuelBay, 5_000, false);
        snap.structures.insert(pharolux_id, online_pharolux(pharolux_id, dst_sys, corp_id));
        snap.fleets.insert(
            7_910,
            FleetRecord {
                fleet_id: 7_910,
                boss_id: cyno_pilot,
                members: vec![cyno_pilot, fleet_pilot],
                free_move: true,
                ..Default::default()
            },
        );
    });

    let should_fail_dst = Arc::new(AtomicBool::new(false));
    ctx.systems
        .register_content_provider(Arc::new(FailTargetWakeProvider {
            target_sys: Some(dst_sys),
            except_sys: None,
            should_fail: Arc::clone(&should_fail_dst),
        }))
        .expect("register fail provider");

    let cur_h = ctx.systems.get_or_spawn(cur_sys);
    spawn_ship(&cur_h, struct_ship, jump_ship_tid, 547, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    spawn_ship(&cur_h, fleet_ship, jump_ship_tid, 547, 6, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    for sid in [struct_ship, fleet_ship] {
        let mut cs = cur_h.get_combat_state(sid).await.unwrap().unwrap();
        cs.capacitor = 10_000.0;
        cs.max_capacitor = 10_000.0;
        cur_h.set_combat_state(sid, cs).await.unwrap();
    }

    crate::services::inventory_fitting::fitting_cyno_portal::cyno_state(&ctx)
        .state
        .write()
        .unwrap()
        .beacons
        .insert(
            cyno_ship,
            crate::services::inventory_fitting::fitting_cyno_portal::FleetModuleBeacon {
                beacon_id: fleet_beacon_id,
                char_id: cyno_pilot,
                ship_id: cyno_ship,
                beacon_type_id: 57_425,
                solar_system_id: dst_sys,
                position: DVec3::new(1_000_000.0, 2_000_000.0, 3_000_000.0),
                expires_at_unix: ctx.now_unix_secs() + 600,
            },
        );

    let mut struct_sess = make_session(struct_pilot, corp_id, Some(cur_sys), Some(struct_ship));
    let mut fleet_sess = make_session(fleet_pilot, corp_id, Some(cur_sys), Some(fleet_ship));

    // Cold dst_sys wakes via SessionArrival(Jump) on structure beacon jump.
    assert!(ctx.systems.get_handle(dst_sys).is_none());
    handle(
        &ctx,
        &mut struct_sess,
        None,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(pharolux_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("structure beacon jump wakes cold dst_sys");
    assert_eq!(
        ctx.systems.last_wake_reason(dst_sys),
        Some(crate::system_actor::SystemWakeReason::SessionArrival(
            crate::system_actor::SessionArrivalWakeReason::Jump,
        ))
    );

    // Stop dst_sys to Cold, then verify fleet beacon jump wakes dst_sys via SessionArrival(Jump).
    assert!(ctx.systems.stop_system(dst_sys).await);
    assert!(ctx.systems.get_handle(dst_sys).is_none());
    handle_cmd_beacon_jump_fleet(
        &ctx,
        &mut fleet_sess,
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(cyno_ship), PyRep::Integer(i64::from(dst_sys))],
    )
    .await
    .expect("fleet beacon jump wakes cold dst_sys");
    assert_eq!(
        ctx.systems.last_wake_reason(dst_sys),
        Some(crate::system_actor::SystemWakeReason::SessionArrival(
            crate::system_actor::SessionArrivalWakeReason::Jump,
        ))
    );

    // Stop dst_sys again, move pilots back to cur_sys, enable wake failure, and verify rollback on both paths.
    assert!(ctx.systems.stop_system(dst_sys).await);
    assert!(ctx.systems.get_handle(dst_sys).is_none());
    should_fail_dst.store(true, Ordering::Release);
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, fuel_iid) in [(struct_pilot, struct_ship, 77_901), (fleet_pilot, fleet_ship, 77_902)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = cur_sys;
                ch.jump_cooldown_until_unix = 0;
                ch.jump_fatigue_until_unix = 0;
            }
            if let Some(it) = snap.items.get_mut(&fuel_iid) {
                it.quantity = 5_000;
            }
            snap.character_session_timers.remove(&cid);
            let _ = sid;
        }
    });
    spawn_ship(&cur_h, struct_ship, jump_ship_tid, 547, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    spawn_ship(&cur_h, fleet_ship, jump_ship_tid, 547, 6, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    for sid in [struct_ship, fleet_ship] {
        let mut cs = cur_h.get_combat_state(sid).await.unwrap().unwrap();
        cs.capacitor = 10_000.0;
        cs.max_capacitor = 10_000.0;
        cur_h.set_combat_state(sid, cs).await.unwrap();
    }
    struct_sess.solar_system_id = Some(cur_sys);
    fleet_sess.solar_system_id = Some(cur_sys);

    let err_struct = handle(
        &ctx,
        &mut struct_sess,
        None,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(pharolux_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("structure beacon jump wake failure must fail");
    assert_eq!(err_struct.code, "SystemTransitionFailed");
    assert!(cur_h.get_ball_snapshot(struct_ship).await.unwrap().is_some());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&77_901].quantity), 5_000);
    assert!(ctx.systems.get_handle(dst_sys).is_none());

    let err_fleet = handle_cmd_beacon_jump_fleet(
        &ctx,
        &mut fleet_sess,
        &[PyRep::Integer(cyno_pilot), PyRep::Integer(cyno_ship), PyRep::Integer(i64::from(dst_sys))],
    )
    .await
    .expect_err("fleet beacon jump wake failure must fail");
    assert_eq!(err_fleet.code, "SystemTransitionFailed");
    assert!(cur_h.get_ball_snapshot(fleet_ship).await.unwrap().is_some());
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&77_902].quantity), 5_000);
    assert!(ctx.systems.get_handle(dst_sys).is_none());
}
