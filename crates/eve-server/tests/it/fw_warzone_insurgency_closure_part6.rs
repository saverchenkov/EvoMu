//! Part 6 of FW & Insurgency closure tests:
//! - Roaming Pirate (`group 4637`) & Enforcer (`group 4638`) dynamic SDE selection by stage/faction,
//!   celestial/grid waypoint patrol movement, stage-gated faction targeting rules, and despawn on stage drop
//!   (`feat:fw.corruption-suppression-effects`).
//! - `OnSecurityModified` broadcast on campaign end from Stage 5 LAWLESS, manual override precedence,
//!   and Suppression Stage 4+ `+10%` Stasis Webifier optimal range via `dogmaIM.Activate`
//!   (`feat:fw.corruption-suppression-effects`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{make_ball, register_online_pilot, rpc_call};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync;
use eve_server::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, end_insurgency_campaign,
    FORECASTING_DURATION_SECS,
};
use glam::DVec3;

fn total_sde_hp(ctx: &ServerContext, type_id: u32) -> f64 {
    ctx.sde.types.get(&type_id).map_or(0.0, |t| {
        t.default_attributes.get(&263).copied().unwrap_or(0.0)
            + t.default_attributes.get(&265).copied().unwrap_or(0.0)
            + t.default_attributes.get(&9).copied().unwrap_or(0.0)
    })
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("__server_now_unix".into(), cur.to_string());
    });
}

#[tokio::test]
async fn test_insurgency_roamers_dynamic_sde_movement_targeting_and_despawn() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_081i64;
    let emp_ship = 100_000_081i64;
    let pir_cid = 90_000_082i64;
    let pir_ship = 100_000_082i64;
    let neu_cid = 90_000_083i64;
    let neu_ship = 100_000_083i64;

    // Place ships well away from initial celestial patrol anchors until targeting checks
    let mut s_emp = register_online_pilot(
        &ctx,
        88_001,
        "demo",
        "demo-secret",
        emp_cid,
        98_000_081,
        None,
        front_sys,
        emp_ship,
        639,
        DVec3::new(900_000.0, 900_000.0, 0.0),
        0,
    )
    .await;
    let mut s_pir = register_online_pilot(
        &ctx,
        88_002,
        "demo2",
        "demo2-secret",
        pir_cid,
        98_000_082,
        None,
        front_sys,
        pir_ship,
        621,
        DVec3::new(910_000.0, 900_000.0, 0.0),
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        let mut neu_ch = s.characters[&emp_cid].clone();
        neu_ch.character_id = neu_cid;
        neu_ch.name = "Neutral Explorer".into();
        neu_ch.solar_system_id = front_sys;
        neu_ch.active_ship_id = neu_ship;
        neu_ch.war_faction_id = None;
        s.characters.insert(neu_cid, neu_ch);
        s.items.insert(
            neu_ship,
            InventoryItemRecord {
                item_id: neu_ship,
                type_id: 621,
                owner_id: neu_cid,
                location_id: i64::from(front_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .spawn_ball(make_ball(
            neu_ship,
            621,
            DVec3::new(920_000.0, 900_000.0, 0.0),
        ))
        .await;

    rpc_call(
        &ctx,
        &mut s_emp,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("enlist Caldari");
    rpc_call(
        &ctx,
        &mut s_pir,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_010)],
    )
    .await
    .expect("enlist Guristas");

    // Seed campaign and enter Active phase
    let _ = rpc_call(
        &ctx,
        &mut s_emp,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareInstances",
        &[],
    )
    .await
    .unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (camp_id, pirate_fac) = ctx.db.with_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
        (cid, c.pirate_faction_id)
    });
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 1. Dynamic SDE tier escalation for Roaming Pirates (group 4637) across Corruption 2..=5:
    let mut pir_tids_by_stage = Vec::new();
    for stage in 2u8..=5 {
        let delta = if stage == 2 { 400 } else { 200 };
        let (_pts, cur_st) = add_system_corruption(&ctx, camp_id, front_sys, delta);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let (_bid, tid) = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .find_map(|n| {
                    let t = ctx.sde.types.get(&n.type_id)?;
                    (n.solar_system_id == front_sys
                        && t.group_id == 4637
                        && t.faction_id == Some(pirate_fac as u32))
                    .then_some((n.ball_id, n.type_id))
                })
                .expect("SDE group 4637 Pirate roamer for campaign faction")
        });
        pir_tids_by_stage.push(tid);
    }
    assert!(
        total_sde_hp(&ctx, pir_tids_by_stage[3]) > total_sde_hp(&ctx, pir_tids_by_stage[0]),
        "Stage 5 Pirate roamer must escalate to a higher-HP SDE hull than Stage 2"
    );

    // 2. Waypoint patrol movement across system celestials/grid anchors when idle:
    let (pir_bid, pos_before) = with_combat_runtime(&ctx, |rt| {
        let n = rt
            .npc_entities
            .values()
            .find(|n| {
                n.solar_system_id == front_sys
                    && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)
            })
            .unwrap();
        (n.ball_id, n.spawn_position)
    });
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (_, ball_after_roam) = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap();
    assert!(
        ball_after_roam.position.distance(pos_before) > 1_000.0,
        "Idle Pirate roamer must patrol to next celestial/grid waypoint across ticks"
    );
    assert!(
        matches!(ball_after_roam.mode, MovementMode::GotoDirection { .. }),
        "Roaming NPC must actively steer toward its next patrol waypoint"
    );

    // 3. Stage-gated targeting for InsurgencyPirateRoamer (Corruption >= 2):
    //    Attacks neutral & Empire militia pilots while ignoring Pirate militia unless provoked!
    let roamer_pos = ball_after_roam.position;
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    let pir_wf = ctx.db.with_snapshot(|s| s.characters.get(&pir_cid).and_then(|c| c.war_faction_id));
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&pir_bid];
        assert!(
            !n.aggression_threat.contains_key(&pir_ship),
            "InsurgencyPirateRoamer must not attack Pirate militia unprovoked (arch={}, tid={}, fac={}, pir_wf={pir_wf:?}, threat={:?})",
            n.archetype,
            n.type_id,
            n.faction_id,
            n.aggression_threat
        );
        assert_eq!(
            n.target_id,
            Some(neu_ship),
            "InsurgencyPirateRoamer must engage non-pirate neutral pilot"
        );
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&pir_bid];
        assert_eq!(
            n.target_id,
            Some(emp_ship),
            "InsurgencyPirateRoamer must engage Empire militia pilot"
        );
    });
    // Move all pilots far away from Pirate roamer before testing Enforcers
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 900_000.0, 0.0));
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(910_000.0, 900_000.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(920_000.0, 900_000.0, 0.0));
    }

    // 4. Dynamic SDE tier escalation & targeting for Roaming Enforcers (group 4638) across Suppression 3..=5:
    let mut enf_tids_by_stage = Vec::new();
    for stage in 3u8..=5 {
        let delta = if stage == 3 { 600 } else { 200 };
        let (_pts, cur_st) = add_system_suppression(&ctx, camp_id, front_sys, delta);
        assert_eq!(cur_st, stage);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .find_map(|n| {
                    let t = ctx.sde.types.get(&n.type_id)?;
                    (n.solar_system_id == front_sys && t.group_id == 4638).then_some(n.type_id)
                })
                .expect("SDE group 4638 Enforcer roamer")
        });
        enf_tids_by_stage.push(tid);
    }
    assert!(
        total_sde_hp(&ctx, enf_tids_by_stage[2]) > total_sde_hp(&ctx, enf_tids_by_stage[0]),
        "Stage 5 Enforcer roamer must escalate to a higher-HP SDE hull than Stage 3"
    );

    let (enf_bid, enf_pos) = with_combat_runtime(&ctx, |rt| {
        let n = rt
            .npc_entities
            .values()
            .find(|n| {
                n.solar_system_id == front_sys
                    && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
            })
            .unwrap();
        (n.ball_id, n.spawn_position)
    });
    // Enforcer stays neutral to Empire militia & neutral pilots, and attacks Pirate militia!
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, enf_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, enf_pos + DVec3::new(12_000.0, 0.0, 0.0));
        bp.set_ball_position(pir_ship, enf_pos + DVec3::new(900_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&enf_bid];
        assert_eq!(
            n.target_id, None,
            "InsurgencyEnforcer must remain neutral to Empire militia and neutral pilots"
        );
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pir_ship, enf_pos + DVec3::new(14_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&enf_bid];
        assert_eq!(
            n.target_id,
            Some(pir_ship),
            "InsurgencyEnforcer must attack Pirate-aligned pilot at Suppression >= 3"
        );
    });

    // 5. Despawn when Corruption drops < 2 and Suppression drops < 3:
    let (_c_low_pts, c_low_st) = add_system_corruption(&ctx, camp_id, front_sys, -800);
    assert_eq!(c_low_st, 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys
                && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)
        })),
        "Pirate roamers must despawn when Corruption drops below Stage 2"
    );
    assert!(sys_h.get_ball_snapshot(pir_bid).await.unwrap().is_none());

    let (_s_low_pts, s_low_st) = add_system_suppression(&ctx, camp_id, front_sys, -600);
    assert_eq!(s_low_st, 2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(
        !with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| {
            n.solar_system_id == front_sys
                && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)
        })),
        "Enforcer roamers must despawn when Suppression drops below Stage 3"
    );
    assert!(sys_h.get_ball_snapshot(enf_bid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_campaign_end_lawless_security_notification_and_web_range() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_091i64;
    let emp_ship = 100_000_091i64;
    let vic_cid = 90_000_092i64;
    let vic_ship = 100_000_092i64;

    let mut s_emp = register_online_pilot(
        &ctx,
        89_001,
        "demo",
        "demo-secret",
        emp_cid,
        98_000_091,
        None,
        front_sys,
        emp_ship,
        639,
        DVec3::ZERO,
        0,
    )
    .await;
    let mut s_vic = register_online_pilot(
        &ctx,
        89_002,
        "demo2",
        "demo2-secret",
        vic_cid,
        98_000_092,
        None,
        front_sys,
        vic_ship,
        621,
        DVec3::new(10_500.0, 0.0, 0.0),
        0,
    )
    .await;

    rpc_call(
        &ctx,
        &mut s_emp,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .await
    .expect("enlist Caldari");
    rpc_call(
        &ctx,
        &mut s_vic,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_010)],
    )
    .await
    .expect("enlist Guristas");

    let _ = rpc_call(
        &ctx,
        &mut s_emp,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareInstances",
        &[],
    )
    .await
    .unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 1. Live Stasis Webifier (`type_id = 526`, 10,000 m optimal) `+10%` range at Suppression Stage 4+ (10,500 m):
    let web_id = 60_000_091i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            web_id,
            InventoryItemRecord {
                item_id: web_id,
                type_id: 526,
                owner_id: emp_cid,
                location_id: emp_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(emp_ship);
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let _ = sys_h
        .add_target_lock(
            emp_ship,
            eve_destiny::TargetLock {
                target_id: vic_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;

    let (_s3_pts, s3_st) = add_system_suppression(&ctx, camp_id, front_sys, 600);
    assert_eq!(s3_st, 3);
    let err_web_s3 = rpc_call(
        &ctx,
        &mut s_emp,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_id),
            PyRep::String("shipModifiers".into()),
            PyRep::Integer(vic_ship),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(
        err_web_s3.code, "TargetOutOfRange",
        "10,500 m exceeds 10,000 m Stasis Webifier optimal at Suppression Stage 3"
    );

    let (_s4_pts, s4_st) = add_system_suppression(&ctx, camp_id, front_sys, 200);
    assert_eq!(s4_st, 4);
    let ok_web_s4 = rpc_call(
        &ctx,
        &mut s_emp,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_id),
            PyRep::String("shipModifiers".into()),
            PyRep::Integer(vic_ship),
        ],
    )
    .await;
    assert!(
        ok_web_s4.is_ok(),
        "10,500 m succeeds within +10% (11,000 m) Stasis Webifier optimal at Suppression Stage 4: {ok_web_s4:?}"
    );

    // 2. Campaign end on Stage-5 LAWLESS system emits `OnSecurityModified` back to SDE base security,
    //    while manual overrides in `modified_system_securities` remain untouched:
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(enemy_sys, 0.25);
    });
    let (_c5_pts, c5_st) = add_system_corruption(&ctx, camp_id, front_sys, 1_500);
    assert_eq!(c5_st, 5);
    let (_ec5_pts, ec5_st) = add_system_corruption(&ctx, camp_id, enemy_sys, 1_000);
    assert_eq!(ec5_st, 5);
    ctx.drain_notifications_for_char(emp_cid);

    let base_front_sec = ctx.sde.solar_systems[&front_sys].security_status;
    end_insurgency_campaign(&ctx, camp_id);
    let notes = ctx.drain_notifications_for_char(emp_cid);
    assert!(
        notes.iter().any(|n| n.event == "OnSecurityModified"
            && n.args
                == vec![
                    PyRep::Integer(i64::from(front_sys)),
                    PyRep::Real(0.0),
                    PyRep::Real(base_front_sec),
                ]),
        "Ending campaign on Stage 5 LAWLESS system must emit OnSecurityModified back to base security"
    );
    assert!(
        !notes.iter().any(|n| n.event == "OnSecurityModified"
            && n.args.first() == Some(&PyRep::Integer(i64::from(enemy_sys)))),
        "Manual override system must not emit OnSecurityModified on Stage 5 exit"
    );
    let mod_after = rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[])
        .await
        .unwrap();
    let PyRep::Dict(entries) = mod_after else {
        panic!("expected Dict from securityMgr.get_modified_systems");
    };
    assert!(
        entries
            .iter()
            .any(|(k, v)| k == &PyRep::Integer(i64::from(enemy_sys)) && v == &PyRep::Real(0.25)),
        "Manual override (0.25) must remain in get_modified_systems"
    );
    assert!(
        !entries
            .iter()
            .any(|(k, _)| k == &PyRep::Integer(i64::from(front_sys))),
        "front_sys must no longer appear in get_modified_systems after campaign end"
    );
}
