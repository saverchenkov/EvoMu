//! Part 6 of FW & Insurgency closure tests:
//! - Roaming Pirate (`group 4637`) & Enforcer (`group 4638`) dynamic SDE selection by stage/faction,
//!   celestial waypoint warp patrol movement, campaign-faction targeting rules, and despawn on stage drop.
//! - `OnSecurityModified` broadcast on campaign end from Stage 5 LAWLESS, manual override precedence,
//!   Suppression Stage 4+ `+10%` Stasis Webifier optimal range across 1 Hz ticks, `IncreasedNotice` accuracy,
//!   and Highsec Stage 5 Green/Yellow/Red safety & Faction Police suppression.

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::quasar_sov_equinox_hub::{field, varint};
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
use eve_server::services::security_mgr::{
    insurgency_pirate_warp_bonus_au_s, insurgency_pvp_loot_drop_threshold,
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
    for stage in [2u8, 5] {
        let delta = if stage == 2 { 400 } else { 600 };
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
        total_sde_hp(&ctx, pir_tids_by_stage[1]) > total_sde_hp(&ctx, pir_tids_by_stage[0]),
        "Stage 5 Pirate roamer must escalate to a higher-HP SDE hull than Stage 2"
    );

    // 2. Waypoint patrol movement across system celestials via Warp (no instant teleport):
    let pir_bid = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .values()
            .find(|n| {
                n.solar_system_id == front_sys
                    && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4637)
            })
            .unwrap()
            .ball_id
    });
    let pos_before = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap().1.position;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (_, ball_warping) = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap();
    assert!(
        matches!(ball_warping.mode, MovementMode::Warp { .. }),
        "Idle Pirate roamer must enter MovementMode::Warp toward next celestial waypoint, got {:?}",
        ball_warping.mode
    );
    assert!(
        ball_warping.position.distance(pos_before) < 5_000.0,
        "Roamer must not teleport instantaneously across celestials on warp initiation (moved {} m)",
        ball_warping.position.distance(pos_before)
    );
    for _ in 0..4 {
        let _ = sys_h.step_manual(10.0).await.unwrap();
    }
    let (_, ball_after_roam) = sys_h.get_ball_snapshot(pir_bid).await.unwrap().unwrap();
    assert!(
        ball_after_roam.position.distance(pos_before) > 100_000.0,
        "Idle Pirate roamer must arrive at next celestial waypoint after stepping warp ticks"
    );

    // 3. Stage & campaign-faction-gated targeting for InsurgencyPirateRoamer (Corruption >= 2):
    //    Attacks neutral, Empire militia, and rival pirate militia pilots while ignoring aligned Pirate militia!
    let rival_pir_cid = 90_000_084i64;
    let rival_pir_ship = 100_000_084i64;
    let rival_fac = if pirate_fac == 500_010 { 500_011 } else { 500_010 };
    ctx.db.with_mut_snapshot(|s| {
        let mut r_ch = s.characters[&pir_cid].clone();
        r_ch.character_id = rival_pir_cid;
        r_ch.name = "Rival Pirate Pilot".into();
        r_ch.active_ship_id = rival_pir_ship;
        r_ch.war_faction_id = Some(rival_fac);
        s.characters.insert(rival_pir_cid, r_ch);
        s.items.insert(rival_pir_ship, InventoryItemRecord {
            item_id: rival_pir_ship, type_id: 621, owner_id: rival_pir_cid, location_id: i64::from(front_sys),
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let _ = sys_h.spawn_ball(make_ball(rival_pir_ship, 621, DVec3::new(930_000.0, 900_000.0, 0.0))).await;
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, rival_pir_cid, front_sys), 0.0, "Rival pirate receives no +2.0 AU/s warp bonus");
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, pir_cid, front_sys), 2.0, "Aligned pirate receives +2.0 AU/s warp bonus");
    assert_eq!(insurgency_pvp_loot_drop_threshold(&ctx, front_sys, rival_pir_cid, emp_cid, false), 0.50, "Rival pirate receives no PvP loot bonus");
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, front_sys, pir_cid, emp_cid, false) - 0.875).abs() < 1e-6);

    let roamer_pos = ball_after_roam.position;
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(13_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&pir_bid];
        assert!(!n.aggression_threat.contains_key(&pir_ship), "Roamer must not attack aligned Pirate militia unprovoked");
        assert_eq!(n.target_id, Some(rival_pir_ship), "Roamer must engage rival non-campaign Pirate militia pilot");
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&pir_bid].target_id, Some(neu_ship), "Roamer must engage neutral pilot");
    });
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(900_000.0, 0.0, 0.0));
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(15_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_entities[&pir_bid].target_id, Some(emp_ship), "Roamer must engage Empire militia pilot");
    });
    // Move all pilots far away from Pirate roamer before testing Enforcers
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, roamer_pos + DVec3::new(900_000.0, 900_000.0, 0.0));
        bp.set_ball_position(pir_ship, roamer_pos + DVec3::new(910_000.0, 900_000.0, 0.0));
        bp.set_ball_position(neu_ship, roamer_pos + DVec3::new(920_000.0, 900_000.0, 0.0));
        bp.set_ball_position(rival_pir_ship, roamer_pos + DVec3::new(930_000.0, 900_000.0, 0.0));
    }

    // 4. Dynamic SDE tier escalation & targeting for Roaming Enforcers (group 4638) across Suppression 3..=5:
    let mut enf_tids_by_stage = Vec::new();
    for stage in [3u8, 5] {
        let delta = if stage == 3 { 600 } else { 400 };
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
        total_sde_hp(&ctx, enf_tids_by_stage[1]) > total_sde_hp(&ctx, enf_tids_by_stage[0]),
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
    // Enforcer stays neutral to Empire militia, neutral, and rival non-campaign pirate pilots, and attacks aligned Pirate militia!
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(emp_ship, enf_pos + DVec3::new(10_000.0, 0.0, 0.0));
        bp.set_ball_position(neu_ship, enf_pos + DVec3::new(12_000.0, 0.0, 0.0));
        bp.set_ball_position(rival_pir_ship, enf_pos + DVec3::new(13_000.0, 0.0, 0.0));
        bp.set_ball_position(pir_ship, enf_pos + DVec3::new(900_000.0, 0.0, 0.0));
    }
    step_extended_npc_ai_sync(&ctx, front_sys);
    with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&enf_bid];
        assert_eq!(
            n.target_id, None,
            "InsurgencyEnforcer must remain neutral to Empire, neutral, and rival non-campaign pirate pilots"
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
    let base_vic_vel = sys_h.get_ball_snapshot(vic_ship).await.unwrap().unwrap().1.max_velocity;
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
    for _ in 0..3 {
        let _ = sys_h.step_manual(1.0).await.unwrap();
    }
    let webbed_vic_vel = sys_h.get_ball_snapshot(vic_ship).await.unwrap().unwrap().1.max_velocity;
    assert!(
        webbed_vic_vel < base_vic_vel * 0.6,
        "Stasis Webifier must keep target max_velocity reduced across 1 Hz ticks at 10,500 m ({webbed_vic_vel} vs {base_vic_vel})"
    );

    // 1b. Quasar `IncreasedNotice` reports pre-mutation `old_pts` on Suppression Stage 3 `-30%` pulses
    //     and emits no `IncreasedNotice` when capped at 800 pts under Suppression Stage 5:
    let _ = rpc_call(&ctx, &mut s_emp, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    let _ = ctx.quasar_gateway.poll_notices(s_emp.session_id);
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (70, 1));
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (140, 1));
    let inc_notices: Vec<_> = ctx
        .quasar_gateway
        .poll_notices(s_emp.session_id)
        .into_iter()
        .filter_map(|n| n.payload)
        .filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice"))
        .collect();
    assert_eq!(inc_notices.len(), 2, "Expected 2 IncreasedNotice events for 0->70 and 70->140");
    let old_seq = field(&inc_notices[1].value, 2).and_then(|b| varint(&b, 1));
    let new_seq = field(&inc_notices[1].value, 3).and_then(|b| varint(&b, 1));
    assert_eq!((old_seq, new_seq), (Some(70), Some(140)), "IncreasedNotice must report pre-mutation old_pts=70 and new_pts=140");

    assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, 200), (1000, 5));
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 1_500), (800, 4));
    let _ = ctx.quasar_gateway.poll_notices(s_emp.session_id);
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 100), (800, 4));
    let capped_inc = ctx
        .quasar_gateway
        .poll_notices(s_emp.session_id)
        .into_iter()
        .filter_map(|n| n.payload)
        .filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice"))
        .count();
    assert_eq!(capped_inc, 0, "Capped zero-change corruption pulse at Suppression Stage 5 must not emit IncreasedNotice");

    // 2. Campaign end on Stage-5 LAWLESS system emits `OnSecurityModified` back to SDE base security,
    //    while manual overrides in `modified_system_securities` remain untouched:
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(enemy_sys, 0.25);
    });
    assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, -1_000), (0, 0));
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

#[tokio::test]
async fn test_highsec_stage5_safety_levels_and_faction_police_suppression() {
    use eve_server::services::combat_crimewatch::crimewatch_timers::{
        evaluate_and_record_pvp_aggression, evaluate_faction_police_response,
    };
    use eve_server::services::combat_crimewatch::SafetyLevel;

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)
    let atk_cid = 90_000_095i64;
    let atk_ship = 100_000_095i64;
    let vic_cid = 90_000_096i64;
    let vic_ship = 100_000_096i64;
    let vic_pod = 100_000_097i64;

    let mut s_emp = register_online_pilot(&ctx, 89_005, "demo", "demo-secret", atk_cid, 98_000_095, None, hs_sys, atk_ship, 639, DVec3::ZERO, 0).await;
    ctx.db.with_mut_snapshot(|s| {
        let mut v_ch = s.characters[&atk_cid].clone();
        v_ch.character_id = vic_cid;
        v_ch.name = "Highsec Victim".into();
        v_ch.solar_system_id = hs_sys;
        v_ch.active_ship_id = vic_ship;
        s.characters.insert(vic_cid, v_ch);
        s.characters.get_mut(&atk_cid).unwrap().security_status = -5.0;
        for (sid, tid, owner) in [(vic_ship, 621, vic_cid), (vic_pod, 670, vic_cid)] {
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: tid, owner_id: owner, location_id: i64::from(hs_sys),
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = front_sys;
    });
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let now_u = ctx.now_unix();
    let camp_id = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
        let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
        c.forecasting_end_unix = now_u.saturating_sub(1);
        if !c.systems.contains(&hs_sys) { c.systems.push(hs_sys); }
        cid
    });

    // 1. Faction Police suppression in Highsec Stage 2..=4 (`evaluate_faction_police_response`):
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 200), (200, 1));
    assert_eq!(evaluate_faction_police_response(&ctx, hs_sys, atk_cid, atk_ship).await, (true, false, true));
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 400), (600, 3));
    assert_eq!(evaluate_faction_police_response(&ctx, hs_sys, atk_cid, atk_ship).await, (false, true, false));

    // 2. Highsec Corruption Stage 5 (`LAWLESS`) safety levels: Green blocks, Yellow & Red allow Suspect ship attacks, Yellow blocks pods:
    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 400), (1000, 5));
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Green); });
    assert_eq!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_ship, false).unwrap_err().code, "CantDoInCurrentSafetyLevel");

    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Yellow); });
    assert_eq!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_pod, true).unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert!(evaluate_and_record_pvp_aggression(&ctx, atk_cid, atk_ship, hs_sys, vic_ship, false).is_ok());
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(atk_cid);
        assert!(st.is_suspect(now_u) && !st.is_criminal(now_u) && st.pending_concord_arrival.is_none());
    }
    let _ = front_sys;
}
