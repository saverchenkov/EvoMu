//! Live integration tests for PvE Incursion Payout Scaling and Mission Completion.

use crate::common::{
    clear_incursion_site_escort_waves, gm_pilot_in_space, login_pilot, slash_cmd, LivePilot,
};
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const TYPE_VELATOR: u32 = 606;

async fn gm_pilot_docked_at(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    sys_id: u32,
    station_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    login_pilot(ctx, &mut sess, &user, &pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    if sys_id != JITA_SYS || station_id != JITA_STATION {
        let required: Vec<u32> = ctx
            .sde
            .types
            .get(&TYPE_VELATOR)
            .map(|t| {
                [182_u16, 183, 184, 1285, 1289, 1290]
                    .iter()
                    .filter_map(|a| t.default_attributes.get(a))
                    .map(|v| v.round() as u32)
                    .filter(|&s| s > 0)
                    .collect()
            })
            .unwrap_or_default();
        for skill_id in required {
            slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
        }
        let ship_rep = slash_cmd(ctx, &mut sess, &format!("/createitem {TYPE_VELATOR} 1")).await;
        let ship_id = ship_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)], &kw, None)
            .await
            .expect("ship.Board");
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("stationSvc.Undock");
        slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
        if let Some(st) = ctx.sde.stations.get(&station_id) {
            let sys = ctx.systems.get_or_spawn(sys_id);
            sys.set_ball_position(ship_id, st.position + DVec3::new(500.0, 0.0, 0.0))
                .await;
            sys.step_manual(0.1).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("stationSvc.Dock");
    }
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    sess
}

async fn move_pilot_to(ctx: &ServerContext, p: &mut LivePilot, pos: DVec3) {
    let sys_id = p.sess.solar_system_id.expect("in space");
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(p.ship_id, pos).await;
    sys.step_manual(0.1).await.unwrap();
}

async fn join_fleet(ctx: &ServerContext, boss: &mut LivePilot, member: &mut LivePilot) {
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut boss.sess,
        "fleetMgr",
        None,
        "Invite",
        &[
            PyRep::Integer(member.char_id),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("fleetMgr.Invite");
    ctx.dispatch_rpc(
        &mut member.sess,
        "fleetMgr",
        None,
        "AcceptInvite",
        &[],
        &kw,
        None,
    )
    .await
    .expect("fleetMgr.AcceptInvite");
}

// ============================================================================
// 3. Incursion Payout Scaling & On-Grid Fleet Rules (feat:pve.incursion-payout)
// ============================================================================
#[tokio::test]
async fn test_live_pve_incursion_fleet_payout_scaling_and_on_grid() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (vg_sys_id, site1_id, site1_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().next().expect("active incursion");
        let sys_id = inc.vanguard_solar_system_ids[0];
        let site = inc
            .sites
            .iter()
            .find(|s| s.solar_system_id == sys_id && s.tier == "Vanguard" && s.completed_count == 0)
            .expect("Vanguard site");
        (sys_id, site.site_id.clone(), site.position)
    });

    // 1. Solo / under-sized (1 pilot on-grid): pays 1/5 share (fleetScaleFactor == 0.2)
    let mut pilots: Vec<LivePilot> = Vec::new();
    for i in 0..14_usize {
        let p = gm_pilot_in_space(
            &ctx,
            9730 + i as u64,
            97_003_001 + i as i64,
            TYPE_VELATOR,
            vg_sys_id,
        )
        .await;
        pilots.push(p);
    }

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    move_pilot_to(&ctx, &mut pilots[0], site1_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let init_isk_0 = ctx.db.with_snapshot(|s| s.characters[&pilots[0].char_id].balance.0);
    let boss1 = clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site1_id).await;
    vg_sys
        .apply_damage_with_attacker(
            boss1,
            DamageVector {
                em: 100_000.0,
                thermal: 100_000.0,
                kinetic: 100_000.0,
                explosive: 100_000.0,
            },
            Some(pilots[0].char_id),
        )
        .await
        .unwrap()
        .unwrap();

    let after_isk_0 = ctx.db.with_snapshot(|s| s.characters[&pilots[0].char_id].balance.0);
    assert!(after_isk_0 > init_isk_0, "Solo Vanguard clear must pay ISK");
    let lp_log_rep = ctx
        .dispatch_rpc(&mut pilots[0].sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .expect("GetIncursionLPLog");
    let PyRep::List(lp_rows) = lp_log_rep else {
        panic!("GetIncursionLPLog list");
    };
    let log1 = lp_rows.last().expect("LP log entry 1");
    assert_eq!(log1.dict_get("fleetSize").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(log1.dict_get("fleetScaleFactor").and_then(|v| v.as_f64()), Some(0.2));

    // 2. Optimal 5-pilot fleet on-grid + 1 off-grid member (>500km away):
    // On-grid pilots get fleetScaleFactor == 1.0; off-grid gets 0.
    let (boss_p, rest) = pilots.split_first_mut().unwrap();
    ctx.dispatch_rpc(&mut boss_p.sess, "fleetMgr", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    for m in rest.iter_mut().take(5) {
        join_fleet(&ctx, boss_p, m).await;
    }

    ensure_special_sites_spawned(&ctx).await;
    let (site2_sys, site2_id, site2_pos) = with_pve_special_reg(&ctx, |reg| {
        let site = reg
            .incursions
            .values()
            .flat_map(|inc| &inc.sites)
            .find(|s| s.tier == "Vanguard" && s.completed_count == 0)
            .expect("second Vanguard site");
        (site.solar_system_id, site.site_id.clone(), site.position)
    });
    for p in std::iter::once(&mut *boss_p).chain(rest.iter_mut()) {
        if p.sess.solar_system_id != Some(site2_sys) {
            slash_cmd(&ctx, &mut p.sess, &format!("/tr {site2_sys}")).await;
        }
    }

    move_pilot_to(&ctx, boss_p, site2_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    for (idx, m) in rest.iter_mut().take(4).enumerate() {
        move_pilot_to(&ctx, m, site2_pos + DVec3::new(1_200.0 + idx as f64 * 100.0, 0.0, 0.0)).await;
    }
    // 6th fleet member is 600 km off-grid
    move_pilot_to(&ctx, &mut rest[4], site2_pos + DVec3::new(600_000.0, 0.0, 0.0)).await;

    let pre_isk_on = ctx.db.with_snapshot(|s| s.characters[&rest[0].char_id].balance.0);
    let pre_isk_off = ctx.db.with_snapshot(|s| s.characters[&rest[4].char_id].balance.0);
    let boss2 = clear_incursion_site_escort_waves(&ctx, site2_sys, &site2_id).await;
    let sys2 = ctx.systems.get_or_spawn(site2_sys);
    sys2.apply_damage_with_attacker(
        boss2,
        DamageVector {
            em: 100_000.0,
            thermal: 100_000.0,
            kinetic: 100_000.0,
            explosive: 100_000.0,
        },
        Some(boss_p.char_id),
    )
    .await
    .unwrap()
    .unwrap();

    let post_isk_on = ctx.db.with_snapshot(|s| s.characters[&rest[0].char_id].balance.0);
    let post_isk_off = ctx.db.with_snapshot(|s| s.characters[&rest[4].char_id].balance.0);
    assert!(
        post_isk_on > pre_isk_on,
        "Optimal 5-pilot on-grid Vanguard must pay ISK"
    );
    assert_eq!(
        post_isk_off, pre_isk_off,
        "Off-grid fleet member (>500km) must receive 0 ISK"
    );
    let lp_log2 = ctx
        .dispatch_rpc(&mut rest[0].sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .expect("GetIncursionLPLog 2");
    let PyRep::List(lp_rows2) = lp_log2 else { panic!() };
    assert_eq!(lp_rows2.last().unwrap().dict_get("fleetSize").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(lp_rows2.last().unwrap().dict_get("fleetScaleFactor").and_then(|v| v.as_f64()), Some(1.0));

    // 3. Oversized fleet penalty: 11 on-grid pilots (+1 overstack -> 0.925)
    for m in rest.iter_mut().skip(5).take(5) {
        join_fleet(&ctx, boss_p, m).await;
    }
    let (site3_sys, site3_id, site3_pos) = with_pve_special_reg(&ctx, |reg| {
        let site = reg
            .incursions
            .values()
            .flat_map(|inc| &inc.sites)
            .find(|s| s.tier == "Vanguard" && s.completed_count == 0)
            .expect("third Vanguard site");
        (site.solar_system_id, site.site_id.clone(), site.position)
    });
    for p in std::iter::once(&mut *boss_p).chain(rest.iter_mut()) {
        if p.sess.solar_system_id != Some(site3_sys) {
            slash_cmd(&ctx, &mut p.sess, &format!("/tr {site3_sys}")).await;
        }
    }
    move_pilot_to(&ctx, boss_p, site3_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    for (idx, m) in rest.iter_mut().take(10).enumerate() {
        move_pilot_to(&ctx, m, site3_pos + DVec3::new(1_100.0 + idx as f64 * 50.0, 0.0, 0.0)).await;
    }
    let boss3 = clear_incursion_site_escort_waves(&ctx, site3_sys, &site3_id).await;
    let sys3 = ctx.systems.get_or_spawn(site3_sys);
    sys3.apply_damage_with_attacker(
        boss3,
        DamageVector {
            em: 100_000.0,
            thermal: 100_000.0,
            kinetic: 100_000.0,
            explosive: 100_000.0,
        },
        Some(boss_p.char_id),
    )
    .await
    .unwrap()
    .unwrap();
    let lp_log3 = ctx
        .dispatch_rpc(&mut rest[0].sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .expect("GetIncursionLPLog 3");
    let PyRep::List(lp_rows3) = lp_log3 else { panic!() };
    assert_eq!(lp_rows3.last().unwrap().dict_get("fleetSize").and_then(|v| v.as_i64()), Some(11));
    assert_eq!(lp_rows3.last().unwrap().dict_get("fleetScaleFactor").and_then(|v| v.as_f64()), Some(0.925));

    // 4. Extreme overstack: 14 on-grid pilots (+4 overstack -> 0.0 = 0 ISK) on a Vanguard site
    for m in rest.iter_mut().skip(10) {
        join_fleet(&ctx, boss_p, m).await;
    }
    let (site4_sys, site4_id, site4_pos) = with_pve_special_reg(&ctx, |reg| {
        let site = reg
            .incursions
            .values()
            .flat_map(|inc| &inc.sites)
            .find(|s| s.tier == "Vanguard" && s.completed_count == 0)
            .expect("fourth Vanguard site");
        (site.solar_system_id, site.site_id.clone(), site.position)
    });
    for p in std::iter::once(&mut *boss_p).chain(rest.iter_mut()) {
        if p.sess.solar_system_id != Some(site4_sys) {
            slash_cmd(&ctx, &mut p.sess, &format!("/tr {site4_sys}")).await;
        }
    }
    move_pilot_to(&ctx, boss_p, site4_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    for (idx, m) in rest.iter_mut().enumerate() {
        move_pilot_to(&ctx, m, site4_pos + DVec3::new(1_100.0 + idx as f64 * 50.0, 0.0, 0.0)).await;
    }
    let pre_isk_14 = ctx.db.with_snapshot(|s| s.characters[&rest[0].char_id].balance.0);
    let boss4 = clear_incursion_site_escort_waves(&ctx, site4_sys, &site4_id).await;
    let sys4 = ctx.systems.get_or_spawn(site4_sys);
    sys4.apply_damage_with_attacker(
        boss4,
        DamageVector {
            em: 100_000.0,
            thermal: 100_000.0,
            kinetic: 100_000.0,
            explosive: 100_000.0,
        },
        Some(boss_p.char_id),
    )
    .await
    .unwrap()
    .unwrap();
    let post_isk_14 = ctx.db.with_snapshot(|s| s.characters[&rest[0].char_id].balance.0);
    assert_eq!(
        post_isk_14, pre_isk_14,
        "14-pilot Vanguard (+4 overstack) must pay 0 ISK"
    );
}

// ============================================================================
// 4. Mission Complete: ISK, Time Bonus, LP, Standings & Item Rewards (feat:pve.mission-complete)
// ============================================================================
#[tokio::test]
async fn test_live_pve_mission_complete_rewards_bonus_standings_and_items() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Career Agent (agent_type_id == 12): complete within bonus window -> ISK + bonus + LP + standing + item reward
    let career_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 12 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("Career Agent in SDE")
        .clone();
    let career_station = career_agent.station_id.unwrap();
    let career_sys = career_agent.solar_system_id.unwrap();
    let mut sess_c = gm_pilot_docked_at(&ctx, 9740, 97_004_001, career_sys, career_station).await;

    let bind_c = ctx
        .dispatch_rpc(
            &mut sess_c,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(career_agent.agent_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind career agent");
    let PyRep::Tuple(bc_tup) = bind_c else { panic!() };
    let career_oid = bc_tup[0].as_i64().unwrap() as u64;

    let offer_c = ctx
        .dispatch_rpc(&mut sess_c, "agentMgr", Some(career_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("Career DoAction(2)");
    let mission_c_id = offer_c.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let acc_c = ctx
        .dispatch_rpc(&mut sess_c, "agentMgr", Some(career_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("Career DoAction(3)");
    assert_eq!(acc_c.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));

    let (req_tid, req_qty, npcs_rem) = ctx.db.with_snapshot(|s| {
        let active = s.active_missions.get(&(97_004_001, mission_c_id)).cloned();
        (
            acc_c.dict_get("requiredDeliveryTypeID").and_then(|v| v.as_i64()).map(|v| v as u32),
            acc_c.dict_get("requiredDeliveryQty").and_then(|v| v.as_i64()).unwrap_or(0),
            active.map(|a| a.npcs_remaining).unwrap_or(0),
        )
    });
    if let Some(tid) = req_tid {
        if req_qty > 0 {
            slash_cmd(&ctx, &mut sess_c, &format!("/createitem {tid} {req_qty}")).await;
        }
    }
    if npcs_rem > 0 {
        let ship_c = slash_cmd(&ctx, &mut sess_c, &format!("/createitem {TYPE_VELATOR} 1")).await;
        let ship_c_id = ship_c.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess_c, "ship", None, "Board", &[PyRep::Integer(ship_c_id)], &kw, None)
            .await
            .expect("Board");
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess_c, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock");
        let spawned_npcs = ctx.db.with_snapshot(|s| {
            s.active_missions
                .get(&(97_004_001, mission_c_id))
                .map(|a| a.spawned_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        let sys_c = ctx.systems.get_or_spawn(career_sys);
        for npc_bid in spawned_npcs {
            sys_c
                .apply_damage_with_attacker(
                    npc_bid,
                    DamageVector {
                        em: 100_000.0,
                        thermal: 100_000.0,
                        kinetic: 100_000.0,
                        explosive: 100_000.0,
                    },
                    Some(97_004_001),
                )
                .await
                .unwrap()
                .unwrap();
        }
        slash_cmd(&ctx, &mut sess_c, &format!("/tr {career_sys}")).await;
        if let Some(st) = ctx.sde.stations.get(&career_station) {
            sys_c.set_ball_position(ship_c_id, st.position + DVec3::new(500.0, 0.0, 0.0))
                .await;
            sys_c.step_manual(0.1).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut sess_c,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(career_station)],
            &kw,
            None,
        )
        .await
        .expect("Dock career");
    }

    let comp_c = ctx
        .dispatch_rpc(&mut sess_c, "agentMgr", Some(career_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .expect("Career DoAction(6)");
    assert_eq!(comp_c.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    assert_eq!(comp_c.dict_get("missionState").and_then(|v| v.as_i64()), Some(4));
    assert!(comp_c.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp_c.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp_c.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp_c.dict_get("newStanding").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    let reward_item_id = comp_c
        .dict_get("rewardItemID")
        .and_then(|v| v.as_i64())
        .expect("Career agent mission must grant rewardItemID");
    assert!(reward_item_id > 0);
    let reward_in_hangar = ctx.db.with_snapshot(|s| {
        s.items
            .get(&reward_item_id)
            .is_some_and(|it| it.owner_id == 97_004_001 && it.location_id == career_station)
    });
    assert!(reward_in_hangar, "Rewarded item must be in pilot's station hangar");

    // 2. Security Agent (agent_type_id == 2): negative incomplete check, live NPC kill,
    // and time bonus expiry (bonusReward == 0 after bonusExpiryUnix).
    let sec_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| {
            a.agent_type_id == 2
                && a.division_id == 24
                && a.level == 1
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
        })
        .expect("Level 1 Security Agent in SDE")
        .clone();
    let sec_station = sec_agent.station_id.unwrap();
    let sec_sys = sec_agent.solar_system_id.unwrap();
    let mut sess_s = gm_pilot_docked_at(&ctx, 9741, 97_004_002, sec_sys, sec_station).await;

    let bind_s = ctx
        .dispatch_rpc(
            &mut sess_s,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(sec_agent.agent_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind sec agent");
    let PyRep::Tuple(bs_tup) = bind_s else { panic!() };
    let sec_oid = bs_tup[0].as_i64().unwrap() as u64;

    let offer_s = ctx
        .dispatch_rpc(&mut sess_s, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("Sec DoAction(2)");
    let mission_s_id = offer_s.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let acc_s = ctx
        .dispatch_rpc(&mut sess_s, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("Sec DoAction(3)");

    let incomplete_err = ctx
        .dispatch_rpc(&mut sess_s, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(incomplete_err.code, "MissionIncomplete");

    let req_tid_s = acc_s
        .dict_get("requiredDeliveryTypeID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u32);
    let req_qty_s = acc_s
        .dict_get("requiredDeliveryQty")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let npcs_rem_s = ctx.db.with_snapshot(|s| {
        s.active_missions
            .get(&(97_004_002, mission_s_id))
            .map(|a| a.npcs_remaining)
            .unwrap_or(0)
    });
    if let Some(tid) = req_tid_s {
        if req_qty_s > 0 {
            slash_cmd(&ctx, &mut sess_s, &format!("/createitem {tid} {req_qty_s}")).await;
        }
    }
    if npcs_rem_s > 0 {
        let ship_s = slash_cmd(&ctx, &mut sess_s, &format!("/createitem {TYPE_VELATOR} 1")).await;
        let ship_s_id = ship_s.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess_s, "ship", None, "Board", &[PyRep::Integer(ship_s_id)], &kw, None)
            .await
            .expect("Board");
        ctx.advance_secs(31);
        ctx.dispatch_rpc(&mut sess_s, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock");
        let npcs_s = ctx.db.with_snapshot(|s| {
            s.active_missions
                .get(&(97_004_002, mission_s_id))
                .map(|a| a.spawned_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        let sys_s = ctx.systems.get_or_spawn(sec_sys);
        for npc_bid in npcs_s {
            sys_s
                .apply_damage_with_attacker(
                    npc_bid,
                    DamageVector {
                        em: 100_000.0,
                        thermal: 100_000.0,
                        kinetic: 100_000.0,
                        explosive: 100_000.0,
                    },
                    Some(97_004_002),
                )
                .await
                .unwrap()
                .unwrap();
        }
        slash_cmd(&ctx, &mut sess_s, &format!("/tr {sec_sys}")).await;
        if let Some(st) = ctx.sde.stations.get(&sec_station) {
            sys_s.set_ball_position(ship_s_id, st.position + DVec3::new(500.0, 0.0, 0.0))
                .await;
            sys_s.step_manual(0.1).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut sess_s,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(sec_station)],
            &kw,
            None,
        )
        .await
        .expect("Dock sec");
    }

    ctx.advance_secs(7_200);

    let comp_s = ctx
        .dispatch_rpc(&mut sess_s, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .expect("Sec DoAction(6) after bonus expiry");
    assert_eq!(comp_s.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    assert!(comp_s.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert_eq!(
        comp_s.dict_get("bonusReward").and_then(|v| v.as_i64()),
        Some(0),
        "Expired time bonus must pay 0"
    );
    assert!(comp_s.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(comp_s.dict_get("newStanding").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
}
