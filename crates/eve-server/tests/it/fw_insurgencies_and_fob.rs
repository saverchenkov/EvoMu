//! Integration tests for Pirate Insurgencies, Ambition-scaled victories, contribution payouts, and FOB siege.

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{
    apply_pirate_fob_structure_damage, check_docking_allowed, ensure_campaign_pirate_fob,
    evaluate_and_resolve_insurgency_campaign, record_insurgency_contribution,
    tick_insurgency_campaigns, INSURGENCY_MAX_PAYOUT_LP, PIRATE_FOB_STRUCTURE_BASE_ID,
    SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
};
use eve_server::services::fw_services::FACTION_CALDARI;
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, init_insurgency_campaign,
    with_insurgency_runtime, FACTION_GURISTAS, FORECASTING_DURATION_SECS,
    PIRATE_STRONGHOLD_GURISTAS_TYPE,
};

fn make_system_dynamic_state(sys_id: u32) -> eve_db::SystemDynamicState {
    eve_db::SystemDynamicState {
        solar_system_id: sys_id,
        dbs_multiplier: 1.0,
        ess_main_bank: eve_db::IskCents(0),
        ess_reserve_bank: eve_db::IskCents(0),
        ess_linked_character_id: None,
        ess_link_started_tick: None,
        sov_alliance_id: None,
        military_index: 0,
        industrial_index: 0,
        strategic_index: 0,
        fw_contested_pct: 0.0,
        insurgency_corruption_stage: 0,
        insurgency_suppression_stage: 0,
        wormhole_remaining_mass_kg: 0.0,
        wormhole_destination_system_id: None,
        signature_position: None,
        fw_complex_position: None,
        fw_capture_seconds_elapsed: 0,
        fw_capture_duration_seconds: 0,
        last_fw_capture_tick: 0,
        last_sov_claim_tick: 0,
        ess_cycle_started_unix: 0,
        ess_main_bank_contributors: Default::default(),
    }
}

fn seed_pilot(ctx: &ServerContext, char_id: i64, war_faction_id: Option<i64>, sys_id: u32) {
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot_{char_id}");
        ch.war_faction_id = war_faction_id;
        ch.solar_system_id = sys_id;
        ch.balance = eve_db::IskCents::from_isk(10_000_000);
        ch.loyalty_points.clear();
        s.characters.insert(char_id, ch);
        s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
    });
}

async fn sov_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

#[tokio::test]
async fn test_fw_insurgency_ambition_victory_and_surrender_ihub() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let fob_sys = 30000012u32;
    let systems = vec![
        fob_sys, 30000013, 30000014, 30000015, 30000016, 30000017, 30000018, 30000019,
    ];

    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.insert(
            fob_sys,
            eve_db::snapshot_ext::FwSystemOccupationRecord {
                solar_system_id: fob_sys,
                owner_faction_id: FACTION_CALDARI,
                occupier_faction_id: FACTION_CALDARI,
                victory_points: 0,
                victory_points_threshold: 3000,
            },
        );
        for &sys in &systems {
            s.system_states.entry(sys).or_insert_with(|| make_system_dynamic_state(sys));
        }
    });

    let camp_id1 = 890_001i64;
    init_insurgency_campaign(
        &ctx,
        camp_id1,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        fob_sys,
        systems.clone(),
    );
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    // Initial Ambition modifiers: 0 for both sides. Win target = 5 Stage 5 systems.
    let (p0, e0) = with_insurgency_runtime(&ctx, |rt| {
        (
            rt.pirate_ambition_modifier.get(&FACTION_GURISTAS).copied().unwrap_or(0),
            rt.empire_ambition_modifier.get(&FACTION_GURISTAS).copied().unwrap_or(0),
        )
    });
    assert_eq!((p0, e0), (0, 0), "Initial ambition modifiers must be 0");

    // Corrupt 5 systems to Stage 5 (1,000 pts)
    for &s in &systems[..5] {
        let (_, stage) = add_system_corruption(&ctx, camp_id1, s, 1000);
        assert_eq!(stage, 5);
    }

    // Resolve campaign 1: Pirate victory
    let (res1, p1, e1, _) = evaluate_and_resolve_insurgency_campaign(&ctx, camp_id1, None).unwrap();
    assert_eq!(res1, "PirateWin");
    assert_eq!(p1, 1, "Pirate win adds +1 to winner's modifier");
    assert_eq!(e1, 0, "Empire modifier remains 0");

    // Pirate win gives temporary control of FOB system to Pirate militia
    let fob_occ_fac = ctx.db.with_snapshot(|s| {
        s.fw_system_occupations.get(&fob_sys).map(|o| o.occupier_faction_id)
    });
    assert_eq!(
        fob_occ_fac,
        Some(FACTION_GURISTAS),
        "Pirate win gives temporary control of FOB system"
    );

    // Hub vulnerable 24h + 1 downtime after surrender (25 hours = 90,000 s)
    let surrender_vuln = with_insurgency_runtime(&ctx, |rt| {
        rt.surrender_ihub_vulnerable_unix.get(&fob_sys).copied()
    });
    let now1 = ctx.now_unix();
    assert_eq!(
        surrender_vuln,
        Some(now1 + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS),
        "Post-surrender I-Hub vulnerability must be scheduled for now + 25h"
    );

    // Before 25h: I-Hub is not vulnerable
    let is_vuln_before = ctx.db.with_snapshot(|s| {
        s.fw_runtime.ihub_vulnerable.get(&fob_sys).copied().unwrap_or(false)
    });
    assert!(!is_vuln_before, "I-Hub must not be vulnerable prior to 25h delay");

    // Advance 25h and tick campaigns
    ctx.advance_virtual_clock(SURRENDER_IHUB_VULNERABILITY_DELAY_SECS + 5);
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
    let is_vuln_after = ctx.db.with_snapshot(|s| {
        s.fw_runtime.ihub_vulnerable.get(&fob_sys).copied().unwrap_or(false)
    });
    assert!(is_vuln_after, "I-Hub must become vulnerable after 24h + 1 downtime surrender delay");

    // Underdog win mechanics: start Campaign 2 with Pirate leader (p=1, e=0).
    // Empire winning (underdog win) reduces leader's modifier by 1 (to p=0, e=0).
    let camp_id2 = 890_002i64;
    init_insurgency_campaign(
        &ctx,
        camp_id2,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        fob_sys,
        systems.clone(),
    );
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    // Suppress 5 systems to Stage 5
    for &s in &systems[..5] {
        let _ = add_system_suppression(&ctx, camp_id2, s, 1000);
    }
    let (res2, p2, e2, _) = evaluate_and_resolve_insurgency_campaign(&ctx, camp_id2, None).unwrap();
    assert_eq!(res2, "EmpireWin");
    assert_eq!(p2, 0, "Underdog Empire win reduces leader Pirate modifier by 1");
    assert_eq!(e2, 0, "Empire modifier remains 0");

    // Campaign 3: Empire wins again from (0, 0) -> Empire becomes leader with +1 (p=0, e=1)
    let camp_id3 = 890_003i64;
    init_insurgency_campaign(
        &ctx,
        camp_id3,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        fob_sys,
        systems.clone(),
    );
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    for &s in &systems[..5] {
        let _ = add_system_suppression(&ctx, camp_id3, s, 1000);
    }
    let (res3, p3, e3, _) = evaluate_and_resolve_insurgency_campaign(&ctx, camp_id3, None).unwrap();
    assert_eq!(res3, "EmpireWin");
    assert_eq!(p3, 0);
    assert_eq!(e3, 1, "Subsequent Empire win increases Empire modifier to 1");

    // Missing goal produces a Tie with no bonus
    let camp_id4 = 890_004i64;
    init_insurgency_campaign(
        &ctx,
        camp_id4,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        fob_sys,
        systems.clone(),
    );
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    // Only 2 systems corrupted, goal is 5 -> missing goal
    for &s in &systems[..2] {
        let _ = add_system_corruption(&ctx, camp_id4, s, 1000);
    }
    let (res4, p4, e4, payouts4) = evaluate_and_resolve_insurgency_campaign(&ctx, camp_id4, None).unwrap();
    assert_eq!(res4, "Tie", "Missing goal produces a tie");
    assert_eq!((p4, e4), (0, 1), "Tie produces no ambition modifier change");
    assert!(payouts4.is_empty(), "Tie produces no bonus payouts");
}

#[tokio::test]
async fn test_fw_insurgency_contribution_payout_scaling_and_caps() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let sys_id = 30000012u32;
    let camp_id = 891_001i64;
    let systems = vec![sys_id, 30000013, 30000014, 30000015, 30000016];

    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        sys_id,
        systems.clone(),
    );

    // Seed test pilots:
    // Pilot A: Guristas (50% share >= 45% cap threshold)
    // Pilot B: Guristas (22.5% share = half of 45%)
    // Pilot C: Guristas (10% share)
    // Pilot D: Caldari (50% losing share >= 45%)
    // Pilot E: Caldari (22.5% losing share)
    let p_a = 910_001i64;
    let p_b = 910_002i64;
    let p_c = 910_003i64;
    let p_d = 910_004i64;
    let p_e = 910_005i64;
    let p_filler_p = 910_006i64;
    let p_filler_e = 910_007i64;

    seed_pilot(&ctx, p_a, Some(FACTION_GURISTAS), sys_id);
    seed_pilot(&ctx, p_b, Some(FACTION_GURISTAS), sys_id);
    seed_pilot(&ctx, p_c, Some(FACTION_GURISTAS), sys_id);
    seed_pilot(&ctx, p_d, Some(FACTION_CALDARI), sys_id);
    seed_pilot(&ctx, p_e, Some(FACTION_CALDARI), sys_id);
    seed_pilot(&ctx, p_filler_p, Some(FACTION_GURISTAS), sys_id);
    seed_pilot(&ctx, p_filler_e, Some(FACTION_CALDARI), sys_id);

    // Total pirate contribution = 1,000 pts
    record_insurgency_contribution(&ctx, camp_id, p_a, FACTION_GURISTAS, 500); // 50.0%
    record_insurgency_contribution(&ctx, camp_id, p_b, FACTION_GURISTAS, 225); // 22.5%
    record_insurgency_contribution(&ctx, camp_id, p_c, FACTION_GURISTAS, 100); // 10.0%
    record_insurgency_contribution(&ctx, camp_id, p_filler_p, FACTION_GURISTAS, 175); // 17.5%

    // Total empire contribution = 1,000 pts
    record_insurgency_contribution(&ctx, camp_id, p_d, FACTION_CALDARI, 500); // 50.0%
    record_insurgency_contribution(&ctx, camp_id, p_e, FACTION_CALDARI, 225); // 22.5%
    record_insurgency_contribution(&ctx, camp_id, p_filler_e, FACTION_CALDARI, 275); // 27.5%

    // Resolve as PirateWin (1.0x winner multiplier, 0.4x loser multiplier)
    let (outcome, _, _, payouts) =
        evaluate_and_resolve_insurgency_campaign(&ctx, camp_id, Some("PirateWin")).unwrap();
    assert_eq!(outcome, "PirateWin");

    let lp_for = |cid: i64| -> i64 {
        payouts
            .iter()
            .find(|(id, _, _)| *id == cid)
            .map(|(_, _, lp)| *lp)
            .unwrap_or(0)
    };

    // Pilot A: >= 45% contribution on winning side -> 1.0x * 1,500,000 LP
    let lp_a = lp_for(p_a);
    assert_eq!(
        lp_a, INSURGENCY_MAX_PAYOUT_LP,
        "Pilot A with 50% share (>=45%) must receive capped 1,500,000 LP"
    );

    // Pilot B: 22.5% contribution on winning side -> (22.5/45.0) * 1,500,000 = 750,000 LP
    let lp_b = lp_for(p_b);
    assert_eq!(
        lp_b, 750_000,
        "Pilot B with 22.5% share must receive 750,000 LP (half of cap)"
    );

    // Pilot C: 10.0% contribution on winning side -> (10.0/45.0) * 1,500,000 = 333,333 LP
    let lp_c = lp_for(p_c);
    assert_eq!(
        lp_c, 333_333,
        "Pilot C with 10% share must receive 333,333 LP"
    );

    // Pilot D: >= 45% contribution on losing side -> 1,500,000 * 0.4 = 600,000 LP
    let lp_d = lp_for(p_d);
    assert_eq!(
        lp_d, 600_000,
        "Pilot D with 50% share on losing side must receive 0.4x * 1,500,000 = 600,000 LP"
    );

    // Pilot E: 22.5% contribution on losing side -> 750,000 * 0.4 = 300,000 LP
    let lp_e = lp_for(p_e);
    assert_eq!(
        lp_e, 300_000,
        "Pilot E with 22.5% share on losing side must receive 300,000 LP"
    );

    // Verify ISK and LP balances actually credited in snapshot
    ctx.db.with_snapshot(|s| {
        let ch_a = s.characters.get(&p_a).unwrap();
        // 100 ISK per LP -> 1,500,000 LP = 150,000,000 ISK
        assert_eq!(ch_a.balance, eve_db::IskCents::from_isk(10_000_000 + 150_000_000));
        let ch_d = s.characters.get(&p_d).unwrap();
        // 600,000 LP = 60,000,000 ISK
        assert_eq!(ch_d.balance, eve_db::IskCents::from_isk(10_000_000 + 60_000_000));
    });

    // Test that Ties pay 0 LP and 0 ISK
    let camp_tie = 891_002i64;
    init_insurgency_campaign(&ctx, camp_tie, FACTION_GURISTAS, FACTION_CALDARI, sys_id, systems);
    record_insurgency_contribution(&ctx, camp_tie, p_a, FACTION_GURISTAS, 500);
    record_insurgency_contribution(&ctx, camp_tie, p_d, FACTION_CALDARI, 500);

    let (res_tie, _, _, payouts_tie) =
        evaluate_and_resolve_insurgency_campaign(&ctx, camp_tie, Some("Tie")).unwrap();
    assert_eq!(res_tie, "Tie");
    for (_, _, lp) in payouts_tie {
        assert_eq!(lp, 0, "Ties must pay 0 LP");
    }
}

#[tokio::test]
async fn test_fw_insurgency_fob_lifecycle_docking_and_reinforcement() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let fob_sys = 30000012u32;
    let camp_id = 892_001i64;
    let systems = vec![fob_sys, 30000013, 30000014, 30000015, 30000016];

    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        fob_sys,
        systems.clone(),
    );

    // 1. Pirate FOB spawns in hub system
    let fob_id = ensure_campaign_pirate_fob(&ctx, camp_id).await.expect("FOB spawned");
    assert_eq!(fob_id, PIRATE_FOB_STRUCTURE_BASE_ID + camp_id);
    let st_record = ctx.db.with_snapshot(|s| s.structures.get(&fob_id).cloned().unwrap());
    assert_eq!(st_record.type_id, PIRATE_STRONGHOLD_GURISTAS_TYPE);
    assert_eq!(st_record.solar_system_id, fob_sys);
    assert_eq!(st_record.state, "ShieldVulnerable");

    // 2. Dockable ONLY by pirate militia pilots
    let pirate_pilot = 920_001i64;
    let empire_pilot = 920_002i64;
    let neutral_pilot = 920_003i64;
    seed_pilot(&ctx, pirate_pilot, Some(FACTION_GURISTAS), fob_sys);
    seed_pilot(&ctx, empire_pilot, Some(FACTION_CALDARI), fob_sys);
    seed_pilot(&ctx, neutral_pilot, None, fob_sys);

    assert!(
        check_docking_allowed(&ctx, pirate_pilot, fob_sys, fob_id, true).is_ok(),
        "Pirate militia must be allowed to dock at Pirate FOB"
    );
    let emp_dock_err = check_docking_allowed(&ctx, empire_pilot, fob_sys, fob_id, true).unwrap_err();
    assert_eq!(
        emp_dock_err.code, "FobDockingRestrictedToPirateMilitia",
        "Empire militia must be refused docking at Pirate FOB"
    );
    let neut_dock_err = check_docking_allowed(&ctx, neutral_pilot, fob_sys, fob_id, true).unwrap_err();
    assert_eq!(
        neut_dock_err.code, "FobDockingRestrictedToPirateMilitia",
        "Neutral pilots must be refused docking at Pirate FOB"
    );

    // 3. Attackable only once Empire reaches suppression goal (5 Stage 5 systems)
    // During Forecasting: invulnerable
    let phase_err = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 100.0)
        .await
        .unwrap_err();
    assert_eq!(phase_err.code, "FobInvulnerableInPhase");

    // Advance to Active phase
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    // In Active phase, before suppression goal reached (0/5 systems suppressed):
    let goal_err = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 100.0)
        .await
        .unwrap_err();
    assert_eq!(
        goal_err.code, "SuppressionGoalNotReached",
        "FOB must be attackable only once Empire reaches suppression goal"
    );

    // Pirate militia cannot attack their own FOB
    let ff_err = apply_pirate_fob_structure_damage(&ctx, pirate_pilot, fob_id, 100.0)
        .await
        .unwrap_err();
    assert_eq!(ff_err.code, "CannotAttackFriendlyStructure");

    // Suppress 5 systems to Stage 5 -> goal reached!
    for &s in &systems {
        let _ = add_system_suppression(&ctx, camp_id, s, 1000);
    }

    // 4. Single 3h (±1h jitter) armor reinforcement timer
    // Shield = 2,500, Armor = 2,500, Hull = 3,500
    let dmg_res1 = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 2500.0)
        .await
        .expect("Shield volley");
    assert_eq!(dmg_res1.dict_get("state").and_then(|v| v.as_str()), Some("ShieldVulnerable"));
    assert_eq!(dmg_res1.dict_get("shieldHP"), Some(&PyRep::Real(0.0)));

    // Deplete armor -> triggers ArmorReinforced
    let now_reinf = ctx.now_unix();
    let dmg_res2 = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 2500.0)
        .await
        .expect("Armor volley");
    assert_eq!(dmg_res2.dict_get("state").and_then(|v| v.as_str()), Some("ArmorReinforced"));
    let reinf_until = dmg_res2.dict_get("reinforcedUntilUnix").and_then(|v| v.as_i64()).unwrap() as u64;
    let timer_duration = reinf_until - now_reinf;
    // 3h ± 1h = 2h..4h (7,200..=14,400 s)
    assert!(
        timer_duration >= 7_200 && timer_duration <= 14_400,
        "Reinforcement timer duration must be 3h ± 1h (got {timer_duration} s)"
    );

    // Attacks during reinforcement return FobArmorReinforced
    let reinf_err = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 100.0)
        .await
        .unwrap_err();
    assert_eq!(reinf_err.code, "FobArmorReinforced");

    // 5. Depleting hull after timer destroys FOB, raises Suppression by 1 level (+200 pts) across systems, and ends as Empire win
    ctx.advance_virtual_clock(timer_duration + 5);

    // Initial suppression points before FOB kill
    let supp_before = with_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.get(&camp_id).unwrap().suppression_points.clone()
    });

    let destroy_res = apply_pirate_fob_structure_damage(&ctx, empire_pilot, fob_id, 3500.0)
        .await
        .expect("Hull volley");
    assert_eq!(destroy_res.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));

    // Verify FOB structure state in DB is Destroyed
    let st_after = ctx.db.with_snapshot(|s| s.structures.get(&fob_id).cloned().unwrap());
    assert_eq!(st_after.state, "Destroyed");

    // Destroying raises Suppression by +1 level (max 5) across affected systems
    let supp_after = with_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.get(&camp_id).unwrap().suppression_points.clone()
    });
    for &s in &systems {
        let b = supp_before.get(&s).copied().unwrap_or(0);
        let a = supp_after.get(&s).copied().unwrap_or(0);
        assert_eq!(a, b + 200, "FOB destruction must raise suppression by +200 pts");
    }

    // Verify campaign outcome ended as EmpireWin
    let camp_outcome = with_insurgency_runtime(&ctx, |rt| {
        rt.campaign_outcomes.get(&camp_id).cloned()
    });
    assert_eq!(camp_outcome, Some("EmpireWin".into()), "Destroying FOB must end insurgency as EmpireWin");
}

#[tokio::test]
async fn test_fw_insurgency_fob_ambition_and_stronghold_spawns() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let hq_sys = 30000012u32;
    let camp_id = 893_001i64;
    let systems = vec![hq_sys, 30000013, 30000014, 30000015, 30000016];

    let mut sess = SessionState::new(43_001);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&90_000_001).unwrap().solar_system_id = hq_sys;
        for &sys in &systems {
            s.system_states.entry(sys).or_insert_with(|| make_system_dynamic_state(sys));
        }
    });
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .unwrap();
    sess.solar_system_id = Some(hq_sys);

    // 1. Each Pirate Insurgency anchors FOB in HQ system
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, FACTION_CALDARI, hq_sys, systems.clone());
    let fob_id = ensure_campaign_pirate_fob(&ctx, camp_id).await.expect("FOB anchored");
    let fob_struct = ctx.db.with_snapshot(|s| s.structures.get(&fob_id).cloned().unwrap());
    assert_eq!(fob_struct.solar_system_id, hq_sys, "FOB must anchor in HQ system");

    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    // 2. Stage 5 Corruption / Suppression accrues campaign points until victory / FOB destruction
    for &s in &systems {
        let (pts, stage) = add_system_corruption(&ctx, camp_id, s, 1000);
        assert_eq!(pts, 1000);
        assert_eq!(stage, 5);
    }

    // Call tick_insurgency_campaigns: resolves Pirate victory and spawns Pirate Stronghold
    tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;

    let outcome = with_insurgency_runtime(&ctx, |rt| rt.campaign_outcomes.get(&camp_id).cloned());
    assert_eq!(outcome, Some("PirateWin".into()));

    // Verify Pirate Stronghold spawned on Pirate victory
    let has_stronghold = with_insurgency_runtime(&ctx, |rt| {
        rt.insurgency_instances.iter().any(|inst| inst.site_type == "PirateStronghold")
    });
    assert!(has_stronghold, "Pirate victory must spawn Pirate Stronghold");

    // Client RPC verification via GetLocalCampaignClientSnapshot and GetAllVisibleCampaigns
    let visible_camps = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(camps) = visible_camps else { panic!("expected List") };
    assert!(!camps.is_empty());
    assert_eq!(camps[0].dict_get("campaignID").and_then(|v| v.as_i64()), Some(camp_id));
    assert_eq!(camps[0].dict_get("fobSolarSystemID").and_then(|v| v.as_i64()), Some(i64::from(hq_sys)));

    let local_snap = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[])
        .await
        .expect("GetLocalCampaignClientSnapshot");
    let PyRep::Tuple(tuple_data) = local_snap else { panic!("expected Tuple") };
    assert_eq!(tuple_data.len(), 2);
    let snap_dict = &tuple_data[1];
    assert_eq!(snap_dict.dict_get("campaignID").and_then(|v| v.as_i64()), Some(camp_id));
    assert_eq!(snap_dict.dict_get("corruptionStage").and_then(|v| v.as_i64()), Some(5));
}
