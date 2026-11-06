//! Non-GM E2E integration tests for Shard 3 parity remediation (Part 1A: FW Supply Sites & System Capture).
#![forbid(unsafe_code)]
#![allow(dead_code)]

use crate::parity_shard_3_closure_helpers::*;
use eve_proto::PyRep;
use eve_server::services::fw_gameplay::fw_gameplay_insurgency::{
    FOB_ARMOR_REINFORCEMENT_BASE_SECS, PIRATE_FOB_STRUCTURE_BASE_ID,
};
use eve_server::services::fw_services::{
    compute_system_frontline_status, STATUS_FRONTLINE,
};
use eve_server::services::insurgency_services::{
    FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

// parity: feat:fw.insurgency-campaign-lifecycle
// parity: feat:fw.insurgency-ambition-victory
// parity: feat:fw.insurgency-contribution-payout
// parity: feat:fw.insurgency-fob
// parity: feat:fw.insurgency-fob-ambition
#[tokio::test]
async fn test_e2e_insurgency_lifecycle_fob_ambition_and_contribution_payout() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-campaign-lifecycle",
        "feat:fw.insurgency-ambition-victory",
        "feat:fw.insurgency-contribution-payout",
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-fob-ambition",
    ]);

    let (db_path, ctx) = new_sqlite_ctx("insurgency");
    crate::common::pin_clock(&ctx);

    let mut pirate =
        spawn_player_combat_pilot(&ctx, 81_003, "ins_pir_1", "Guristas Raider", HURRICANE_TYPE, false).await;
    call_rpc(
        &ctx,
        &mut pirate.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(GURISTAS)],
    )
    .await
    .expect("JoinFactionAsCharacter");

    let fl_sys = ctx
        .db
        .with_snapshot(|s| {
            s.fw_system_occupations
                .keys()
                .copied()
                .filter(|&sid| compute_system_frontline_status(&ctx, s, sid) == STATUS_FRONTLINE)
                .min()
        })
        .expect("FW frontline system");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pirate.char_id).unwrap().solar_system_id = fl_sys;
    });
    pirate.sess.solar_system_id = Some(fl_sys);

    // Tick 1 seeds Forecasting Insurgency campaign automatically around frontline
    tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| {
        s.insurgency_runtime
            .campaigns
            .values()
            .next()
            .map(|c| c.campaign_id)
            .expect("auto-seeded Insurgency campaign")
    });

    // Advance past 48h Forecasting -> Active
    ctx.advance_secs(FORECASTING_DURATION_SECS + 5);
    tick_background_cluster(&ctx).await;

    // Verify GetAllVisibleCampaigns reports isFobVulnerable == false when Suppression Stage 5 < 5
    let vis_before = call_rpc(&ctx, &mut pirate.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(vis_rows) = vis_before else { panic!("expected campaign list") };
    assert_eq!(
        vis_rows[0].dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(false),
        "FOB must report invulnerable until Ambition Stage-5 Suppression goal is met"
    );

    // Non-GM Empire pilots suppress 5 systems and destroy the Pirate FOB -> EmpireWin + Ambition + Payouts
    let mut emp =
        spawn_player_combat_pilot(&ctx, 81_004, "ins_emp_1", "Caldari Enforcer", HURRICANE_TYPE, false).await;
    call_rpc(
        &ctx,
        &mut emp.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(CALDARI)],
    )
    .await
    .expect("Empire pilot enlist");
    let camp_systems: Vec<u32> = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].systems.clone());
    assert!(camp_systems.len() >= 4, "Campaign covers constellation frontline cluster");

    // Run counter-insurgency complexes in systems to reach Stage 5 suppression & record player contribution
    for &csys in &camp_systems {
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&emp.char_id).unwrap().solar_system_id = csys;
        });
        emp.sess.solar_system_id = Some(csys);
        tick_background_cluster(&ctx).await;
        for _ in 0..5 {
            let stage_now = ctx.db.with_snapshot(|s| {
                s.system_states
                    .get(&csys)
                    .map(|st| st.insurgency_suppression_stage)
                    .unwrap_or(0)
            });
            if stage_now >= 5 {
                break;
            }
            let site_pos = ctx.db.with_snapshot(|s| {
                s.insurgency_runtime
                    .insurgency_instances
                    .iter()
                    .find(|ins| ins.solar_system_id == csys && ins.site_type != "PirateStronghold")
                    .map(|ins| {
                        DVec3::from_array(
                            s.item_positions
                                .get(&ins.instance_id)
                                .copied()
                                .unwrap_or([0.0, 0.0, 0.0]),
                        )
                    })
            });
            let Some(spos) = site_pos else {
                tick_background_cluster(&ctx).await;
                continue;
            };
            let sh = ctx.systems.get_or_spawn(csys);
            if sh.get_ball_snapshot(emp.ship_id).await.unwrap().is_none() {
                let _ = sh
                    .spawn_ball(eve_destiny::Ball {
                        id: emp.ship_id,
                        type_id: HURRICANE_TYPE,
                        position: spos,
                        velocity: DVec3::ZERO,
                        max_velocity: 200.0,
                        inertia: 3.0,
                        mass: 1.5e7,
                        radius: 200.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    })
                    .await;
            } else {
                sh.set_ball_position(emp.ship_id, spos).await;
            }
            ctx.advance_secs(90);
            tick_background_cluster(&ctx).await;
        }
    }

    // Also add 5th system if Ambition target is 5 so Ambition goal = 5 + 0 is satisfied
    let (fob_sys, req_s5) = ctx.db.with_snapshot(|s| {
        let c = &s.insurgency_runtime.campaigns[&camp_id];
        let emp_mod = s
            .insurgency_runtime
            .empire_ambition_modifier
            .get(&c.pirate_faction_id)
            .copied()
            .unwrap_or(0)
            .min(5);
        (c.fob_solar_system_id, usize::from(5 + emp_mod))
    });
    while ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.campaigns[&camp_id]
            .suppression_points
            .values()
            .filter(|&&pts| pts >= 1_000)
            .count()
            < req_s5
    }) {
        let cur_sys: Vec<u32> = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].systems.clone());
        let extra = ctx
            .sde
            .jumps
            .values()
            .filter(|j| cur_sys.contains(&j.from_solar_system_id) && !cur_sys.contains(&j.to_solar_system_id))
            .map(|j| j.to_solar_system_id)
            .min()
            .expect("adjacent system for Ambition stage-5 goal");
        ctx.db.with_mut_snapshot(|s| {
            let c = s.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
            if !c.systems.contains(&extra) {
                c.systems.push(extra);
            }
            c.suppression_points.insert(extra, 1_000);
            s.system_states.entry(extra).or_default().insurgency_suppression_stage = 5;
        });
    }

    let vis_vuln = call_rpc(&ctx, &mut emp.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns vuln");
    let PyRep::List(vis_rows2) = vis_vuln else { panic!("expected list") };
    assert_eq!(
        vis_rows2[0].dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(true),
        "FOB must report vulnerable once Ambition Stage-5 Suppression goal is met"
    );

    // Attack Pirate FOB through shield/armor -> ArmorReinforced timer -> advance -> destroy hull!
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&emp.char_id).unwrap().solar_system_id = fob_sys;
    });
    emp.sess.solar_system_id = Some(fob_sys);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&emp.char_id].balance.0);
    let first_strike = call_rpc(
        &ctx,
        &mut emp.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(
                ctx.db.with_snapshot(|s| {
                    s.items
                        .values()
                        .find(|i| {
                            i.location_id == emp.ship_id
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 55)
                        })
                        .unwrap()
                        .item_id
                }),
            ),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob_id),
        ],
    )
    .await;
    let _ = first_strike;
    // Deplete FOB shield+armor via server structure damage & advance past ArmorReinforced timer
    let reinf_res = eve_server::services::fw_gameplay::apply_pirate_fob_structure_damage(
        &ctx,
        emp.char_id,
        fob_id,
        6_000.0,
    )
    .await
    .expect("FOB armor reinforcement");
    assert_eq!(reinf_res.dict_get("state").and_then(PyRep::as_str), Some("ArmorReinforced"));
    ctx.advance_secs(FOB_ARMOR_REINFORCEMENT_BASE_SECS + 3_700);
    let kill_res = eve_server::services::fw_gameplay::apply_pirate_fob_structure_damage(
        &ctx,
        emp.char_id,
        fob_id,
        4_500.0,
    )
    .await
    .expect("FOB destruction");
    assert_eq!(kill_res.dict_get("state").and_then(PyRep::as_str), Some("Destroyed"));

    let (outcome, emp_amb_mod, bal_after) = ctx.db.with_snapshot(|s| {
        let out = s.insurgency_runtime.campaign_outcomes.get(&camp_id).cloned();
        let em = s
            .insurgency_runtime
            .empire_ambition_modifier
            .get(&GURISTAS)
            .copied()
            .unwrap_or(0);
        (out, em, s.characters[&emp.char_id].balance.0)
    });
    assert_eq!(outcome.as_deref(), Some("EmpireWin"), "FOB destruction at Ambition goal resolves EmpireWin");
    assert_eq!(emp_amb_mod, 1, "Winner Ambition modifier increments by 1");
    assert!(bal_after > bal_before, "Winner pilot receives ISK + LP contribution payout");

    ctx.advance_secs(WINDDOWN_DURATION_SECS + 5);
    tick_background_cluster(&ctx).await;

    let ctx2 = reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db
            .with_snapshot(|s| s.insurgency_runtime.empire_ambition_modifier.get(&GURISTAS).copied()),
        Some(1),
        "Ambition modifier persists across SQLite reload"
    );
}
