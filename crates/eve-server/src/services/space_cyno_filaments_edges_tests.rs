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
