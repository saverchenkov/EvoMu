//! Live test suite for Pirate Insurgency FOB docking/siege gates and Ambition-scaled
//! victory & contribution LP/ISK payouts.
//!
//! Driven 100% through MachoNet RPCs, GM slash commands, live space combat, and cluster ticks.
//! Zero `with_mut_snapshot`, zero `with_mut_combat_runtime`, zero direct runtime mutations.

#![forbid(unsafe_code)]

use crate::common;
use crate::esi_fw_sov_incursions_live::{lock_target, rpc, slash};
use crate::fw_insurgency_campaign_lifecycle_live::{
    boost_siege_guns, destroy_target_live, fire_one_volley, hold_complex_to_pts, move_near,
    setup_enlisted_pilot, CALDARI, GURISTAS,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{
    militia_lp_corp_id, INSURGENCY_MAX_PAYOUT_LP, PIRATE_FOB_STRUCTURE_BASE_ID,
    SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
};
use eve_server::services::insurgency_services::{
    read_insurgency_runtime, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::sync::Arc;

async fn reinforce_fob_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    fob_id: i64,
) {
    move_near(ctx, sys_id, ship_id, fob_id).await;
    lock_target(ctx, sess, sys_id, ship_id, fob_id, 1_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..80 {
        if ctx.db.with_snapshot(|s| s.structures[&fob_id].state == "ArmorReinforced") {
            return;
        }
        let mut reloading = false;
        for &gid in guns {
            if ctx.db.with_snapshot(|s| s.structures[&fob_id].state == "ArmorReinforced") {
                return;
            }
            if let Err(e) = rpc(
                ctx,
                sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(fob_id),
                ],
            )
            .await
            {
                match e.code.as_str() {
                    "ModuleIsReloading" => reloading = true,
                    "TargetNotLocked" => {
                        lock_target(ctx, sess, sys_id, ship_id, fob_id, 1_000.0).await;
                    }
                    _ => {}
                }
            }
        }
        let step_secs = if reloading { 11 } else { 4 };
        for _ in 0..step_secs {
            sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(step_secs);
        slash(ctx, sess, "/heal").await;
    }
    panic!("FOB {fob_id} did not enter ArmorReinforced");
}

// parity: feat:fw.insurgency-fob
// parity: feat:fw.insurgency-fob-ambition
#[tokio::test]
async fn test_live_insurgency_fob_docking_and_suppression_gate() {
    eve_testkit::attest(&["feat:fw.insurgency-fob", "feat:fw.insurgency-fob-ambition"]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let (mut pir, guns_pir) = setup_enlisted_pilot(&ctx, 77_103, 91_000_103, GURISTAS, hub_sys).await;
    let (mut emp, guns_emp) = setup_enlisted_pilot(&ctx, 77_104, 91_000_104, CALDARI, hub_sys).await;
    let (ship_pir, ship_emp) = (pir.ship_id, emp.ship_id);
    tick_background_cluster(&ctx).await;

    let (campaign_id, fob_id) = read_insurgency_runtime(&ctx, |rt| {
        let c = rt.campaigns.values().next().expect("seeded campaign");
        (c.campaign_id, PIRATE_FOB_STRUCTURE_BASE_ID + c.campaign_id)
    });

    // 1. Pirate FOB docking restriction via structureDocking.Dock: Empire rejected, Pirate allowed.
    move_near(&ctx, hub_sys, ship_emp, fob_id).await;
    let dock_err = rpc(
        &ctx,
        &mut emp.sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(fob_id)],
    )
    .await
    .expect_err("Empire pilot must be rejected from docking at Pirate FOB");
    assert_eq!(dock_err.code, "FobDockingRestrictedToPirateMilitia");

    move_near(&ctx, hub_sys, ship_pir, fob_id).await;
    rpc(
        &ctx,
        &mut pir.sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(fob_id)],
    )
    .await
    .expect("Pirate pilot must be allowed to dock at Pirate FOB");
    rpc(&ctx, &mut pir.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock from FOB");
    let _ = rpc(&ctx, &mut pir.sess, "beyonce", "CmdStop", &[]).await;

    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;

    // 2. Attack gate before Suppression goal is met: FOB is invulnerable.
    let PyRep::List(camps) =
        rpc(&ctx, &mut emp.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
            .await
            .unwrap()
    else {
        panic!("expected list");
    };
    let camp_rep = camps
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    assert_eq!(
        camp_rep.dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(false)
    );

    let atk_err = fire_one_volley(&ctx, &mut emp.sess, hub_sys, ship_emp, guns_emp[0], fob_id)
        .await
        .expect_err("FOB cannot be attacked before suppression goal");
    assert_eq!(atk_err.code, "SuppressionGoalNotReached");
    ctx.systems
        .get_or_spawn(hub_sys)
        .set_ball_position(ship_emp, glam::DVec3::ZERO)
        .await;

    // 3. Spread campaign to 5 systems and raise 5 systems to Stage 5 Suppression (1,000 pts each).
    let init_systems = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&campaign_id].systems.clone());
    let spread_seed_sys = *init_systems.iter().find(|&&s| s != hub_sys).expect("secondary system");
    hold_complex_to_pts(&ctx, &mut pir.sess, campaign_id, spread_seed_sys, ship_pir, true, 600).await;

    hold_complex_to_pts(&ctx, &mut emp.sess, campaign_id, hub_sys, ship_emp, false, 1_000).await;
    let all_systems = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&campaign_id].systems.clone());
    assert!(all_systems.len() >= 5);
    for s in all_systems.into_iter().filter(|&s| s != hub_sys).take(4) {
        hold_complex_to_pts(&ctx, &mut emp.sess, campaign_id, s, ship_emp, false, 1_000).await;
    }
    tick_background_cluster(&ctx).await;

    let PyRep::List(camps2) =
        rpc(&ctx, &mut emp.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
            .await
            .unwrap()
    else {
        panic!("expected list");
    };
    let camp_rep2 = camps2
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    assert_eq!(
        camp_rep2.dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(true)
    );

    // Friendly pirate pilot cannot attack their own FOB even when vulnerable.
    slash(&ctx, &mut pir.sess, &format!("/tr {hub_sys}")).await;
    let _ = rpc(&ctx, &mut pir.sess, "beyonce", "CmdStop", &[]).await;
    let friendly_err = fire_one_volley(&ctx, &mut pir.sess, hub_sys, ship_pir, guns_pir[0], fob_id)
        .await
        .expect_err("pirate cannot attack own FOB");
    assert_eq!(friendly_err.code, "CannotAttackFriendlyStructure");
    ctx.systems
        .get_or_spawn(hub_sys)
        .set_ball_position(ship_pir, glam::DVec3::ZERO)
        .await;

    // 4. Attack FOB in live combat until ArmorReinforced, verify FobArmorReinforced, then destroy hull.
    slash(&ctx, &mut emp.sess, &format!("/tr {hub_sys}")).await;
    let _ = rpc(&ctx, &mut emp.sess, "beyonce", "CmdStop", &[]).await;
    boost_siege_guns(&ctx, &mut emp.sess, &guns_emp).await;
    reinforce_fob_live(&ctx, &mut emp.sess, hub_sys, ship_emp, &guns_emp, fob_id).await;
    let reinf_until = ctx.db.with_snapshot(|s| s.structures[&fob_id].reinforced_until_unix);
    assert!(reinf_until > ctx.now_unix());

    let reinf_atk_err = fire_one_volley(&ctx, &mut emp.sess, hub_sys, ship_emp, guns_emp[0], fob_id)
        .await
        .expect_err("cannot attack during armor reinforcement");
    assert_eq!(reinf_atk_err.code, "FobArmorReinforced");

    ctx.advance_virtual_clock(reinf_until.saturating_sub(ctx.now_unix()).max(1) + 5);
    destroy_target_live(&ctx, &mut emp.sess, hub_sys, ship_emp, &guns_emp, fob_id).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&fob_id].state.clone()),
        "Destroyed"
    );

    let outcome = read_insurgency_runtime(&ctx, |rt| rt.campaign_outcomes.get(&campaign_id).cloned());
    assert_eq!(outcome.as_deref(), Some("EmpireWin"));
    let pts = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns
            .get(&campaign_id)
            .and_then(|c| c.suppression_points.get(&hub_sys).copied())
    });
    assert_eq!(
        pts,
        Some(1_200),
        "Suppression must increase by 200 points upon FOB kill"
    );
}

// parity: feat:fw.insurgency-ambition-victory
// parity: feat:fw.insurgency-contribution-payout
#[tokio::test]
async fn test_live_insurgency_ambition_victory_and_payouts() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-ambition-victory",
        "feat:fw.insurgency-contribution-payout",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let (mut pir, _guns_pir) = setup_enlisted_pilot(&ctx, 77_105, 91_000_105, GURISTAS, hub_sys).await;
    let (mut emp, guns_emp) = setup_enlisted_pilot(&ctx, 77_106, 91_000_106, CALDARI, hub_sys).await;
    let (ship_pir, ship_emp) = (pir.ship_id, emp.ship_id);
    tick_background_cluster(&ctx).await;

    let campaign_id = read_insurgency_runtime(&ctx, |rt| rt.campaigns.values().next().unwrap().campaign_id);
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;

    // Empire pilot earns 500 contribution pts; Pirate pilot brings 5 systems to Stage 5 Corruption.
    hold_complex_to_pts(&ctx, &mut emp.sess, campaign_id, hub_sys, ship_emp, false, 500).await;
    hold_complex_to_pts(&ctx, &mut pir.sess, campaign_id, hub_sys, ship_pir, true, 1_000).await;
    let all_systems = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&campaign_id].systems.clone());
    assert!(all_systems.len() >= 5);
    for s in all_systems.into_iter().filter(|&s| s != hub_sys).take(4) {
        hold_complex_to_pts(&ctx, &mut pir.sess, campaign_id, s, ship_pir, true, 1_000).await;
    }
    tick_background_cluster(&ctx).await;

    // 1. Campaign resolves as PirateWin: ambition modifier increments, and surrender timer is set to 25h.
    assert_eq!(
        read_insurgency_runtime(&ctx, |rt| rt.campaign_outcomes.get(&campaign_id).cloned()).as_deref(),
        Some("PirateWin")
    );
    let (p_mod, e_mod) = read_insurgency_runtime(&ctx, |rt| {
        (
            rt.pirate_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
            rt.empire_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
        )
    });
    assert_eq!((p_mod, e_mod), (1, 0));

    let surrender_vuln = read_insurgency_runtime(&ctx, |rt| {
        rt.surrender_ihub_vulnerable_unix
            .get(&hub_sys)
            .copied()
            .unwrap_or(0)
    });
    assert!(
        surrender_vuln > ctx.now_unix()
            && surrender_vuln <= ctx.now_unix() + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS
    );

    // 2. Personal contribution payouts: 1.0x winner (1,500,000 LP max cap), 0.4x loser (600,000 LP).
    let pir_corp = militia_lp_corp_id(&ctx.sde, GURISTAS);
    let emp_corp = militia_lp_corp_id(&ctx.sde, CALDARI);
    let (pir_lp, emp_lp) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&91_000_105]
                .loyalty_points
                .get(&pir_corp)
                .copied()
                .unwrap_or(0),
            snap.characters[&91_000_106]
                .loyalty_points
                .get(&emp_corp)
                .copied()
                .unwrap_or(0),
        )
    });
    assert!(
        pir_lp >= INSURGENCY_MAX_PAYOUT_LP,
        "Winner receives 1.0x max LP payout ({pir_lp})"
    );
    assert!(emp_lp >= 600_000, "Loser receives 0.4x LP payout ({emp_lp})");

    // 3. End campaign 1 via WindDown clock advance, auto-seed campaign 2, and win as Empire underdog.
    ctx.advance_virtual_clock(WINDDOWN_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;
    slash(&ctx, &mut pir.sess, &format!("/tr {hub_sys}")).await;
    tick_background_cluster(&ctx).await;

    let camp2 = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns
            .values()
            .find(|c| c.campaign_id != campaign_id && !rt.campaign_outcomes.contains_key(&c.campaign_id))
            .unwrap()
            .campaign_id
    });
    assert_ne!(camp2, campaign_id);
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;

    let camp2_init = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp2].systems.clone());
    let spread_sys2 = *camp2_init.iter().find(|&&s| s != hub_sys).expect("secondary system");
    hold_complex_to_pts(&ctx, &mut pir.sess, camp2, spread_sys2, ship_pir, true, 600).await;

    hold_complex_to_pts(&ctx, &mut emp.sess, camp2, hub_sys, ship_emp, false, 1_000).await;
    let camp2_systems = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&camp2].systems.clone());
    assert!(camp2_systems.len() >= 5);
    for s in camp2_systems.into_iter().filter(|&s| s != hub_sys).take(4) {
        hold_complex_to_pts(&ctx, &mut emp.sess, camp2, s, ship_emp, false, 1_000).await;
    }
    tick_background_cluster(&ctx).await;
    let fob2 = PIRATE_FOB_STRUCTURE_BASE_ID + camp2;
    slash(&ctx, &mut emp.sess, &format!("/tr {hub_sys}")).await;
    let _ = rpc(&ctx, &mut emp.sess, "beyonce", "CmdStop", &[]).await;
    boost_siege_guns(&ctx, &mut emp.sess, &guns_emp).await;
    reinforce_fob_live(&ctx, &mut emp.sess, hub_sys, ship_emp, &guns_emp, fob2).await;
    let reinf2 = ctx.db.with_snapshot(|s| s.structures[&fob2].reinforced_until_unix);
    ctx.advance_virtual_clock(reinf2.saturating_sub(ctx.now_unix()).max(1) + 5);
    destroy_target_live(&ctx, &mut emp.sess, hub_sys, ship_emp, &guns_emp, fob2).await;

    let (p_mod2, e_mod2) = read_insurgency_runtime(&ctx, |rt| {
        (
            rt.pirate_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
            rt.empire_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
        )
    });
    assert_eq!(
        (p_mod2, e_mod2),
        (0, 0),
        "Underdog Empire win must reduce Pirate Ambition modifier by 1"
    );
}
