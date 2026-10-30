//! Real server-side emission tests for Notify Batch 7 (Part 2: Fitting, Weapon Banks,
//! Drones, Stances, Conduit Jump, Character Energy, Skill Injectors & Skill Bundles).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use eve_db::{FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

// parity: notify:OnWeaponBanksChanged, notify:OnWeaponGroupDestroyed
// parity: notify:OnFittingAdded, notify:OnFittingDeleted, notify:OnManyFittingsDeleted
// parity: notify:OnDroneStateChange, notify:OnStanceActive
// parity: notify:OnGroupJumpAnchorActivated, notify:OnGroupJumpedPassenger
// parity: notify:OnNonDiminishingInjectionsUsed
// DATA GAP: OnCharacterEnergyChanged (no V24 gameplay CEQ consumer yet) and OnSkillBundleInjected (skill bundle definitions absent from V24 client FSD) remain open.
#[tokio::test]
async fn notify_batch7_weapons_fittings_drones_stances_jump_and_skills() {
    eve_testkit::attest(&[
        "notify:OnWeaponBanksChanged",
        "notify:OnWeaponGroupDestroyed",
        "notify:OnFittingAdded",
        "notify:OnFittingDeleted",
        "notify:OnManyFittingsDeleted",
        "notify:OnDroneStateChange",
        "notify:OnStanceActive",
        "notify:OnGroupJumpAnchorActivated",
        "notify:OnGroupJumpedPassenger",
        "notify:OnNonDiminishingInjectionsUsed",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (720_000_001_i64, 720_000_002_i64);
    let mut sess_a = login_in_space(&ctx, 7_201, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 7_202, "other", "other-secret", PILOT_B, ship_b, 621, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    // 1-2. OnWeaponBanksChanged (2-arg) & OnWeaponGroupDestroyed (2-arg)
    let (g1, g2) = (720_000_011_i64, 720_000_012_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(g1, make_item(g1, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(g2, make_item(g2, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_a), PyRep::Integer(g1), PyRep::Integer(g2)])
        .await
        .expect("LinkWeapons");
    let link_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let banks_args = find_notif(&link_notifs, "OnWeaponBanksChanged");
    assert_eq!(banks_args.len(), 2);
    assert_eq!(banks_args[0], PyRep::Integer(ship_a));
    assert!(matches!(&banks_args[1], PyRep::Dict(entries) if entries.iter().any(|(k, _)| *k == PyRep::Integer(g1))));

    rpc(&ctx, &mut sess_a, "dogmaIM", "DestroyWeaponBank", &[PyRep::Integer(ship_a), PyRep::Integer(g1)])
        .await
        .expect("DestroyWeaponBank");
    let destroy_bank_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&destroy_bank_notifs, "OnWeaponGroupDestroyed"), &[PyRep::Integer(ship_a), PyRep::Integer(g1)]);

    // 3-5. OnFittingAdded (2-arg), OnFittingDeleted (2-arg), OnManyFittingsDeleted (2-arg)
    let fit_payload = |name: &str| {
        PyRep::keyval([
            ("name", PyRep::String(name.into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("description", PyRep::String("Batch 7 fit".into())),
            ("fitData", PyRep::List(Vec::new())),
        ])
    };
    let save1 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit One")])
        .await
        .expect("SaveFitting 1");
    let fit1 = save1.as_i64().expect("fit1");
    let add_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&add_notifs, "OnFittingAdded"), &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)]);

    let save2 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit Two")])
        .await
        .expect("SaveFitting 2");
    let fit2 = save2.as_i64().expect("fit2");
    let save3 = rpc(&ctx, &mut sess_a, "charFittingMgr", "SaveFitting", &[PyRep::Integer(PILOT_A), fit_payload("Fit Three")])
        .await
        .expect("SaveFitting 3");
    let fit3 = save3.as_i64().expect("fit3");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "charFittingMgr", "DeleteFitting", &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)])
        .await
        .expect("DeleteFitting");
    let del_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&del_notifs, "OnFittingDeleted"), &[PyRep::Integer(PILOT_A), PyRep::Integer(fit1)]);

    rpc(
        &ctx,
        &mut sess_a,
        "charFittingMgr",
        "DeleteManyFittings",
        &[PyRep::Integer(PILOT_A), PyRep::List(vec![PyRep::Integer(fit2), PyRep::Integer(fit3)])],
    )
    .await
    .expect("DeleteManyFittings");
    let del_many_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let many_args = find_notif(&del_many_notifs, "OnManyFittingsDeleted");
    assert_eq!(many_args.len(), 2);
    assert_eq!(many_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(many_args[1], PyRep::List(vec![PyRep::Integer(fit2), PyRep::Integer(fit3)]));

    // 6. OnDroneStateChange (7-arg) via ship.LaunchDrones
    let drone_id = 720_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 645; // Dominix
        }
        snap.items.insert(drone_id, make_item(drone_id, 2454, PILOT_A, ship_a, InventoryFlag::DroneBay, 1, false));
        snap.skills.entry(PILOT_A).or_default().insert(
            3436,
            TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let launch_rep = rpc(&ctx, &mut sess_a, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_id)])])
        .await
        .expect("LaunchDrones");
    let launched_ball_id = match launch_rep {
        PyRep::List(v) => v.first().and_then(|x| x.as_i64()).expect("launched drone ball"),
        _ => panic!("expected List from LaunchDrones"),
    };
    assert!(launched_ball_id > 0);
    let wire_drone_notifs = sess_a.drain_notifications();
    let ctx_drone_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let wire_drone_events: Vec<&[PyRep]> = wire_drone_notifs
        .iter()
        .filter(|(ev, _)| ev == "OnDroneStateChange")
        .map(|(_, args)| args.as_slice())
        .collect();
    assert_eq!(wire_drone_events.len(), 1, "LaunchDrones must push exactly one OnDroneStateChange to session");
    assert_eq!(
        ctx_drone_notifs.iter().filter(|n| n.event == "OnDroneStateChange").count(),
        1,
        "LaunchDrones must enqueue exactly one OnDroneStateChange in ctx"
    );
    let drone_args = wire_drone_events[0];
    assert_eq!(drone_args.len(), 7);
    assert_eq!(drone_args[0], PyRep::Integer(launched_ball_id));
    assert_eq!(drone_args[1], PyRep::Integer(PILOT_A));
    assert_eq!(drone_args[2], PyRep::Integer(ship_a));
    assert_eq!(drone_args[3], PyRep::Integer(0));
    assert_eq!(drone_args[4], PyRep::Integer(2454));
    assert_eq!(drone_args[5], PyRep::Integer(PILOT_A));
    assert_eq!(drone_args[6], PyRep::None);

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::String("aggressive".into()), PyRep::Bool(true)),
            (PyRep::String("focusFire".into()), PyRep::Bool(false)),
        ])],
    )
    .await
    .expect("ChangeDroneSettings");
    let wire_setting_notifs = sess_a.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    assert_eq!(find_notif(&wire_setting_notifs, "OnDroneSettingChanged"), &[PyRep::Bool(true), PyRep::Bool(false)]);
    let post_setting_drone_events: Vec<&[PyRep]> = wire_setting_notifs
        .iter()
        .filter(|(ev, _)| ev == "OnDroneStateChange")
        .map(|(_, args)| args.as_slice())
        .collect();
    assert_eq!(post_setting_drone_events.len(), 1);
    assert_eq!(post_setting_drone_events[0].len(), 7);
    assert_eq!(post_setting_drone_events[0][0], PyRep::Integer(launched_ball_id));
    assert_eq!(post_setting_drone_events[0][6], PyRep::None);

    // 7. OnStanceActive (2-arg) via shipStanceMgr.SetShipStance on Confessor (34317)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 34317; // Confessor
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "shipStanceMgr", "SetShipStance", &[PyRep::Integer(ship_a), PyRep::Integer(34319)])
        .await
        .expect("SetShipStance");
    let stance_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&stance_notifs, "OnStanceActive"), &[PyRep::Integer(ship_a), PyRep::Integer(34319)]);

    // 8-9. OnGroupJumpAnchorActivated (2-arg) & OnGroupJumpedPassenger (2-arg)
    let (sys_origin, sys_dest) = {
        let lowsec: Vec<(u32, [f64; 3])> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
            .map(|s| (s.solar_system_id, s.galactic_pos_ly))
            .collect();
        let mut pair = (30_002_813_u32, 30_002_814_u32);
        'outer: for &(a, pa) in &lowsec {
            for &(b, pb) in &lowsec {
                if a != b {
                    let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    if (0.3..3.0).contains(&d) {
                        pair = (a, b);
                        break 'outer;
                    }
                }
            }
        }
        pair
    };
    let (beacon_item_id, beacon_owner, fleet_id) = (720_000_051_i64, 99_999_i64, 720_601_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for &sk in &[20533_u32, 21611, 21610, 24311, 24312] {
            snap.skills.entry(PILOT_A).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 });
        }
        snap.skills.entry(PILOT_B).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = sys_origin;
            ch.jump_cooldown_until_unix = 0;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.solar_system_id = sys_origin;
            ch.jump_cooldown_until_unix = 0;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.type_id = 23757; // Archon
            sh.location_id = sys_origin as i64;
        }
        if let Some(sh) = snap.items.get_mut(&ship_b) {
            sh.type_id = 621; // Caracal
            sh.location_id = sys_origin as i64;
        }
        snap.items.insert(720_000_052, make_item(720_000_052, 16274, PILOT_A, ship_a, InventoryFlag::Cargo, 100_000, false));
        snap.items.insert(beacon_item_id, make_item(beacon_item_id, 56099, beacon_owner, sys_dest as i64, InventoryFlag::None, 1, true));
        let mut member_details = HashMap::new();
        for (cid, role, wid, sid) in [
            (PILOT_A, FleetRole::FleetCommander, None, None),
            (PILOT_B, FleetRole::SquadMember, Some(1), Some(1)),
            (beacon_owner, FleetRole::SquadMember, Some(1), Some(1)),
        ] {
            member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role,
                    wing_id: wid,
                    squad_id: sid,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: format!("{role:?}"),
                    join_time_unix: 1,
                },
            );
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: PILOT_A,
                members: vec![PILOT_A, PILOT_B, beacon_owner],
                wings: vec![1],
                squads: vec![1],
                free_move: true,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details,
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);
    sess_a.solar_system_id = Some(sys_origin);
    sess_a.fleet_id = Some(fleet_id);
    sess_b.solar_system_id = Some(sys_origin);
    sess_b.fleet_id = Some(fleet_id);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_A);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_B);
    let orig_h = ctx.systems.get_or_spawn(sys_origin);
    for (cid, sid, tid, pos) in [
        (PILOT_A, ship_a, 23757_u32, DVec3::ZERO),
        (PILOT_B, ship_b, 621_u32, DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_ship_combat_state(cid, sid, tid);
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 0;
        cs.undock_invulnerable_remaining_ticks = 0;
        let _ = orig_h
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
                ctx.build_ship_slim_meta(cid, tid),
            )
            .await;
    }
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "beyonce",
        "CmdGroupJumpToFleetDeployableBeacon",
        &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)],
    )
    .await
    .expect("CmdGroupJumpToFleetDeployableBeacon");
    let anchor_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&anchor_notifs, "OnGroupJumpAnchorActivated"),
        &[PyRep::Integer(sys_dest as i64), PyRep::Integer(1)]
    );
    let pass_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&pass_notifs, "OnGroupJumpedPassenger"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(sys_dest as i64)]
    );

    // 10. GM /energy input validation + characterEnergyMgr state check (DATA GAP: OnCharacterEnergyChanged remains open without gameplay CEQ consumer)
    sess_a.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy bogus 100".into())])
            .await
            .expect_err("invalid char_id must fail")
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy 999999999 100".into())])
            .await
            .expect_err("nonexistent char_id must fail")
            .code,
        "CharacterNotFound"
    );
    rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/energy 40".into())])
        .await
        .expect("GM /energy");
    let energy_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let energy_args = find_notif(&energy_notifs, "OnCharacterEnergyChanged");
    assert_eq!(energy_args.len(), 2);
    assert_eq!(energy_args[0], PyRep::Integer(PILOT_A));
    assert_eq!(energy_args[1].dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(40));

    // 11. OnNonDiminishingInjectionsUsed (1-arg) + crateService.ClaimSkillBundle (DATA GAP: OnSkillBundleInjected remains open)
    let (mini_inj_id, crate_id) = (720_000_071_i64, 720_000_072_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
            ch.solar_system_id = SYS_JITA;
        }
        snap.items.insert(mini_inj_id, make_item(mini_inj_id, 43630, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
        snap.items.insert(crate_id, make_item(crate_id, 53533, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false));
    });
    sess_a.station_id = Some(JITA_STATION);
    sess_a.solar_system_id = Some(SYS_JITA);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(mini_inj_id)])
        .await
        .expect("InjectSkillpoints Mini Skill Injector");
    let inj_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&inj_notifs, "OnNonDiminishingInjectionsUsed"), &[PyRep::Integer(1)]);

    rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
        .await
        .expect("OpenCrate 53533");
    let claim_rep = rpc(&ctx, &mut sess_a, "crateService", "ClaimSkillBundle", &[PyRep::Integer(crate_id), PyRep::Integer(39)])
        .await
        .expect("ClaimSkillBundle");
    assert_eq!(claim_rep.dict_get("spGranted").and_then(|v| v.as_f64()), Some(25_000.0));
    let bundle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !bundle_notifs.iter().any(|(e, _)| e == "OnSkillBundleInjected"),
        "OnSkillBundleInjected must not be emitted with hardcoded skill count until skill bundle definitions exist"
    );
}
