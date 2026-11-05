//! Live test suite for Factional Warfare Supply Caches/Depots, Pirate Insurgency lifecycle,
//! FOB siege & Ambition gates, victory LP/ISK contribution payouts, and I-Hub downtime capture.
//!
//! Remediates the 7 `fake_done` rows in `feat:fw.*`:
//! - `feat:fw.supply-caches-depots`
//! - `feat:fw.insurgency-campaign-lifecycle`
//! - `feat:fw.insurgency-fob`
//! - `feat:fw.insurgency-fob-ambition`
//! - `feat:fw.insurgency-ambition-victory`
//! - `feat:fw.insurgency-contribution-payout`
//! - `feat:fw.system-capture`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::*;
use eve_server::services::fw_services::*;
use eve_server::services::insurgency_services::*;
use std::collections::HashMap;

const CALDARI: i64 = 500_001;
const GALLENTE: i64 = 500_004;
const GURISTAS: i64 = 500_010;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn setup_pilot(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    war_fac: Option<i64>,
) {
    let name = format!("fw_pilot_{char_id}");
    ctx.db.provision_gm_account(&name, "secret", char_id, &name);
    common::login_pilot(ctx, sess, &name, "secret").await;
    rpc(
        ctx,
        sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    if let Some(fac) = war_fac {
        rpc(
            ctx,
            sess,
            "facWarMgr",
            "JoinFactionAsCharacter",
            &[PyRep::Integer(fac)],
        )
        .await
        .expect("JoinFactionAsCharacter");
    }
}

// parity: feat:fw.supply-caches-depots
#[tokio::test]
async fn test_live_fw_supply_caches_depots_and_advantage() {
    eve_testkit::attest(&["feat:fw.supply-caches-depots"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.fw_system_occupations
                .iter()
                .find(|(_, o)| o.occupier_faction_id == GALLENTE)
                .map(|(&s, _)| s)
        })
        .expect("Gallente FW system");

    ensure_warzone_sites_spawned(&ctx, sys_id).await;

    let (cache_id, depot_id) = ctx.db.with_snapshot(|snap| {
        let mut cache = 0i64;
        let mut depot = 0i64;
        for (&sid, &(ssys, ref kind, _)) in &snap.fw_runtime.supply_sites {
            if ssys == sys_id {
                if kind == "SupplyCache" {
                    cache = sid;
                } else if kind == "SupplyDepot" {
                    depot = sid;
                }
            }
        }
        (cache, depot)
    });
    assert!(cache_id > 0, "SupplyCache structure must exist");
    assert!(depot_id > 0, "SupplyDepot structure must exist");

    let pilot_id = 91_000_101;
    let mut sess = SessionState::new(101);
    setup_pilot(&ctx, &mut sess, pilot_id, Some(CALDARI)).await;

    // 1. Partial damage to SupplyCache leaves it alive with ShieldVulnerable state.
    let partial = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, cache_id, 500.0)
        .await
        .expect("partial damage to cache");
    assert_eq!(
        partial.dict_get("state").and_then(PyRep::as_str),
        Some("ShieldVulnerable")
    );

    // 2. Destroy SupplyCache (1,500 shield + 1,500 armor + 1,500 hull = 4,500 total HP):
    // drops container with 5x militia supply item (88_103).
    let dest_rep = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, cache_id, 5_000.0)
        .await
        .expect("destroy cache");
    assert_eq!(
        dest_rep.dict_get("state").and_then(PyRep::as_str),
        Some("Destroyed")
    );
    let can_id = dest_rep
        .dict_get("lootContainerID")
        .and_then(PyRep::as_i64)
        .expect("cargo container ball spawned");

    let items = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| it.location_id == can_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(
        items.iter().any(|it| it.type_id == 88_103 && it.quantity == 5),
        "Supply Cache must drop 5x militia supply items"
    );

    let (occ_adv, att_adv) = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.system_advantage.get(&sys_id).copied().unwrap_or((0.0, 0.0)));
    assert_eq!(occ_adv, 0.0);
    assert_eq!(att_adv, 2.0, "Supply Cache kill awards +2% Advantage");

    // 3. Destroy SupplyDepot (3,000 shield + 3,000 armor + 3,000 hull = 9,000 total HP):
    // drops container with 10x militia supply item (88_103) and +5% Advantage.
    let depot_dest = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, depot_id, 10_000.0)
        .await
        .expect("destroy depot");
    assert_eq!(
        depot_dest.dict_get("state").and_then(PyRep::as_str),
        Some("Destroyed")
    );
    let depot_can_id = depot_dest
        .dict_get("lootContainerID")
        .and_then(PyRep::as_i64)
        .expect("depot container ball spawned");

    let depot_items = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| it.location_id == depot_can_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(
        depot_items.iter().any(|it| it.type_id == 88_103 && it.quantity == 10),
        "Supply Depot must drop 10x militia supply items"
    );

    let (_, att_adv2) = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.system_advantage.get(&sys_id).copied().unwrap_or((0.0, 0.0)));
    assert_eq!(att_adv2, 7.0, "Advantage total must be +7% after Cache (+2%) and Depot (+5%)");
}

// parity: feat:fw.insurgency-campaign-lifecycle
// parity: feat:fw.insurgency-lifecycle
// parity: feat:fw.insurgency-spread
#[tokio::test]
async fn test_live_insurgency_campaign_lifecycle_and_stargate_spread() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-campaign-lifecycle",
        "feat:fw.insurgency-lifecycle",
        "feat:fw.insurgency-spread",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let campaign_id = 401i64;

    init_insurgency_campaign(&ctx, campaign_id, GURISTAS, CALDARI, hub_sys, vec![hub_sys]);

    let pilot_id = 91_000_102;
    let mut sess = SessionState::new(102);
    setup_pilot(&ctx, &mut sess, pilot_id, Some(GURISTAS)).await;
    common::slash_cmd(&ctx, &mut sess, &format!("/tr {hub_sys}")).await;

    // 1. Initial campaign is visible in Forecasting phase via authentic RPC.
    let visible = rpc(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(camps) = visible else { panic!("expected list of campaigns") };
    let camp_rep = camps
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .expect("campaign present");
    assert_eq!(
        camp_rep.dict_get("phase").and_then(PyRep::as_str),
        Some("Forecasting")
    );

    // 2. Advancing virtual clock past 48h transitions campaign into Active phase.
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    let visible_active = rpc(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns active");
    let PyRep::List(camps2) = visible_active else { panic!("expected list") };
    let camp_rep2 = camps2
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .expect("campaign present");
    assert_eq!(
        camp_rep2.dict_get("phase").and_then(PyRep::as_str),
        Some("Active")
    );

    // 3. Local snapshot RPC returns current system corruption & suppression state.
    let snap_rep = rpc(
        &ctx,
        &mut sess,
        "insurgencySolarsystem",
        "GetLocalCampaignClientSnapshot",
        &[],
    )
    .await
    .expect("GetLocalCampaignClientSnapshot");
    let PyRep::Tuple(parts) = snap_rep else { panic!("expected tuple") };
    assert_eq!(parts.first().and_then(PyRep::as_i64), Some(hub_sys as i64));

    // 4. Advancing Corruption to Stage 3 (>= 600 pts) triggers stargate-adjacent system spread.
    add_system_corruption(&ctx, campaign_id, hub_sys, 600);

    let updated_visible = rpc(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns post-spread");
    let PyRep::List(camps3) = updated_visible else { panic!("expected list") };
    let camp_rep3 = camps3
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .expect("campaign present");
    let PyRep::List(systems_rep) = camp_rep3.dict_get("systems").expect("systems list") else { panic!("not list") };
    assert!(
        systems_rep.len() >= 2,
        "Campaign must have spread to at least one adjacent system"
    );

    let spread_sys = systems_rep
        .iter()
        .filter_map(PyRep::as_i64)
        .map(|s| s as u32)
        .find(|&s| s != hub_sys)
        .expect("spread system");

    let is_adjacent = ctx
        .sde
        .jumps
        .values()
        .any(|j| j.from_solar_system_id == hub_sys && j.to_solar_system_id == spread_sys);
    assert!(
        is_adjacent,
        "Spread system {spread_sys} must be stargate-connected to hub system {hub_sys}"
    );
}

// parity: feat:fw.insurgency-fob
// parity: feat:fw.insurgency-fob-ambition
#[tokio::test]
async fn test_live_insurgency_fob_docking_and_suppression_gate() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-fob-ambition",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let campaign_id = 501i64;
    let systems = vec![
        hub_sys,
        30_002_539,
        30_002_540,
        30_002_541,
        30_002_542,
    ];

    init_insurgency_campaign(&ctx, campaign_id, GURISTAS, CALDARI, hub_sys, systems.clone());
    let fob_id = ensure_campaign_pirate_fob(&ctx, campaign_id).await.expect("campaign pirate FOB");

    let emp_pilot = 91_000_103;
    let pir_pilot = 91_000_104;
    let mut sess_emp = SessionState::new(103);
    let mut sess_pir = SessionState::new(104);
    setup_pilot(&ctx, &mut sess_emp, emp_pilot, Some(CALDARI)).await;
    setup_pilot(&ctx, &mut sess_pir, pir_pilot, Some(GURISTAS)).await;

    // 1. Pirate FOB docking restriction: Empire militia pilot rejected, Pirate militia pilot allowed.
    let dock_err = check_docking_allowed(&ctx, emp_pilot, hub_sys, fob_id, true)
        .expect_err("Empire pilot must be rejected from docking at Pirate FOB");
    assert_eq!(dock_err.code, "FobDockingRestrictedToPirateMilitia");
    check_docking_allowed(&ctx, pir_pilot, hub_sys, fob_id, true)
        .expect("Pirate pilot must be allowed to dock at Pirate FOB");

    // Advance past forecasting into Active phase.
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    // 2. Attack gate before Suppression goal is met: FOB is invulnerable.
    let visible = rpc(&ctx, &mut sess_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(camps) = visible else { panic!("expected list") };
    let camp_rep = camps
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    assert_eq!(
        camp_rep.dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(false)
    );

    let atk_err = apply_fw_or_insurgency_structure_damage(&ctx, emp_pilot, fob_id, 1_000.0)
        .await
        .expect_err("FOB cannot be attacked before suppression goal");
    assert_eq!(atk_err.code, "SuppressionGoalNotReached");

    // Friendly pirate pilot cannot attack their own FOB.
    let friendly_err = apply_fw_or_insurgency_structure_damage(&ctx, pir_pilot, fob_id, 1_000.0)
        .await
        .expect_err("pirate cannot attack own FOB");
    assert_eq!(friendly_err.code, "CannotAttackFriendlyStructure");

    // 3. Meet Ambition suppression goal: raise 5 systems to Stage 5 Suppression (1,000 pts).
    for &s in &systems {
        add_system_suppression(&ctx, campaign_id, s, 1_000);
    }

    let visible2 = rpc(&ctx, &mut sess_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns post-suppression");
    let PyRep::List(camps2) = visible2 else { panic!("expected list") };
    let camp_rep2 = camps2
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    assert_eq!(
        camp_rep2.dict_get("isFobVulnerable").and_then(PyRep::as_bool),
        Some(true)
    );

    // 4. Attack FOB through shield (2,500) and armor (2,500) -> enters ArmorReinforced (3h base timer).
    let reinf_rep = apply_fw_or_insurgency_structure_damage(&ctx, emp_pilot, fob_id, 5_000.0)
        .await
        .expect("armor reinforce FOB");
    assert_eq!(
        reinf_rep.dict_get("state").and_then(PyRep::as_str),
        Some("ArmorReinforced")
    );
    let reinf_until = reinf_rep
        .dict_get("reinforcedUntilUnix")
        .and_then(PyRep::as_i64)
        .expect("reinforcedUntilUnix");
    assert!(reinf_until > ctx.now_unix() as i64);

    // Attacking during reinforcement returns FobArmorReinforced.
    let reinf_atk_err = apply_fw_or_insurgency_structure_damage(&ctx, emp_pilot, fob_id, 1_000.0)
        .await
        .expect_err("cannot attack during armor reinforcement");
    assert_eq!(reinf_atk_err.code, "FobArmorReinforced");

    // 5. Advance clock past reinforcement timer -> destroy hull (3,500 HP).
    let diff = (reinf_until - (ctx.now_unix() as i64)).max(1) as u64;
    ctx.advance_virtual_clock(diff + 5);

    let kill_rep = apply_fw_or_insurgency_structure_damage(&ctx, emp_pilot, fob_id, 4_000.0)
        .await
        .expect("destroy FOB");
    assert_eq!(
        kill_rep.dict_get("state").and_then(PyRep::as_str),
        Some("Destroyed")
    );

    // FOB destruction raises suppression by 200 pts in all systems and resolves campaign as EmpireWin.
    let outcome = read_insurgency_runtime(&ctx, |rt| rt.campaign_outcomes.get(&campaign_id).cloned());
    assert_eq!(outcome.as_deref(), Some("EmpireWin"));

    let pts = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.get(&campaign_id).map(|c| c.suppression_points.get(&hub_sys).copied().unwrap_or(0))
    });
    assert_eq!(pts, Some(1_200), "Suppression must increase by 200 points upon FOB kill");
}

// parity: feat:fw.insurgency-ambition-victory
// parity: feat:fw.insurgency-contribution-payout
#[tokio::test]
async fn test_live_insurgency_ambition_victory_and_payouts() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-ambition-victory",
        "feat:fw.insurgency-contribution-payout",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let campaign_id = 601i64;
    let systems = vec![
        hub_sys,
        30_002_539,
        30_002_540,
        30_002_541,
        30_002_542,
    ];

    init_insurgency_campaign(&ctx, campaign_id, GURISTAS, CALDARI, hub_sys, systems.clone());
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);

    let pir_pilot = 91_000_105;
    let emp_pilot = 91_000_106;
    let mut sess_pir = SessionState::new(105);
    let mut sess_emp = SessionState::new(106);
    setup_pilot(&ctx, &mut sess_pir, pir_pilot, Some(GURISTAS)).await;
    setup_pilot(&ctx, &mut sess_emp, emp_pilot, Some(CALDARI)).await;

    // Record authentic pilot contributions.
    record_insurgency_contribution(&ctx, campaign_id, pir_pilot, GURISTAS, 500);
    record_insurgency_contribution(&ctx, campaign_id, emp_pilot, CALDARI, 500);

    let now_before = ctx.now_unix();

    // Bring 5 systems to Stage 5 Corruption (>= 1,000 pts).
    // The 5th system crossing Stage 5 automatically evaluates victory and distributes payouts.
    for &s in &systems {
        add_system_corruption(&ctx, campaign_id, s, 1_000);
    }

    // 1. Campaign automatically resolves as PirateWin: ambition modifier increments, and surrender timer is set to 25h.
    let outcome = read_insurgency_runtime(&ctx, |rt| rt.campaign_outcomes.get(&campaign_id).cloned());
    assert_eq!(outcome.as_deref(), Some("PirateWin"));

    let (p_mod, e_mod) = read_insurgency_runtime(&ctx, |rt| {
        (
            rt.pirate_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
            rt.empire_ambition_modifier.get(&GURISTAS).copied().unwrap_or(0),
        )
    });
    assert_eq!(p_mod, 1, "Pirate Ambition modifier must increment on win");
    assert_eq!(e_mod, 0);

    let surrender_vuln = read_insurgency_runtime(&ctx, |rt| {
        rt.surrender_ihub_vulnerable_unix.get(&hub_sys).copied().unwrap_or(0)
    });
    assert_eq!(
        surrender_vuln,
        now_before + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
        "Post-surrender I-Hub vulnerability delay must be exactly 25h (24h + 1 downtime)"
    );

    // 2. Personal contribution payouts: 1.0x winner (1,500,000 LP max cap), 0.4x loser (600,000 LP).
    let pir_corp = militia_lp_corp_id(&ctx.sde, GURISTAS);
    let emp_corp = militia_lp_corp_id(&ctx.sde, CALDARI);
    let (pir_lp, emp_lp) = ctx.db.with_snapshot(|snap| {
        let p_lp = snap.characters[&pir_pilot].loyalty_points.get(&pir_corp).copied().unwrap_or(0);
        let e_lp = snap.characters[&emp_pilot].loyalty_points.get(&emp_corp).copied().unwrap_or(0);
        (p_lp, e_lp)
    });
    assert_eq!(pir_lp, INSURGENCY_MAX_PAYOUT_LP, "Winner receives 1.0x max LP payout");
    assert_eq!(emp_lp, 600_000, "Loser receives 0.4x LP payout");

    // 3. Underdog win reduces opponent's Ambition modifier: next campaign with EmpireWin decrements pirate ambition.
    let camp2 = 602i64;
    init_insurgency_campaign(&ctx, camp2, GURISTAS, CALDARI, hub_sys, systems.clone());
    let (_, p_mod2, e_mod2, _) =
        evaluate_and_resolve_insurgency_campaign(&ctx, camp2, Some("EmpireWin"))
            .expect("evaluate campaign 2");
    assert_eq!(p_mod2, 0, "Underdog Empire win must reduce Pirate Ambition modifier by 1");
    assert_eq!(e_mod2, 0);
}

// parity: feat:fw.system-capture
#[tokio::test]
async fn test_live_fw_system_capture_post_surrender_ihub_and_downtime() {
    eve_testkit::attest(&["feat:fw.system-capture"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.fw_system_occupations
                .iter()
                .find(|(_, o)| o.occupier_faction_id == GALLENTE)
                .map(|(&s, _)| s)
        })
        .expect("Gallente FW system");

    ensure_warzone_sites_spawned(&ctx, sys_id).await;

    let ihub_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.structures
                .values()
                .find(|st| st.solar_system_id == sys_id && st.type_id == TYPE_FW_IHUB && st.state != "Destroyed")
                .map(|st| st.structure_id)
        })
        .expect("FW Infrastructure Hub");

    let pilot_id = 91_000_107;
    let mut sess = SessionState::new(107);
    setup_pilot(&ctx, &mut sess, pilot_id, Some(CALDARI)).await;

    // 1. I-Hub is invulnerable before post-surrender window or 100% contested.
    let invuln_err = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, ihub_id, 1_000.0)
        .await
        .expect_err("I-Hub must be invulnerable");
    assert_eq!(invuln_err.code, "IHubInvulnerable");

    // Simulate post-insurgency surrender timer: 25h delay.
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        rt.surrender_ihub_vulnerable_unix
            .insert(sys_id, now + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS);
    });

    // Still invulnerable before the 25 hours elapse.
    let early_err = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, ihub_id, 1_000.0)
        .await
        .expect_err("I-Hub still invulnerable before 25h");
    assert_eq!(early_err.code, "IHubInvulnerable");

    // 2. Advance clock past 25h: I-Hub becomes vulnerable and can be destroyed.
    ctx.advance_virtual_clock(SURRENDER_IHUB_VULNERABILITY_DELAY_SECS + 1);

    let kill_rep = apply_fw_or_insurgency_structure_damage(&ctx, pilot_id, ihub_id, 50_000.0)
        .await
        .expect("destroy vulnerable I-Hub");
    assert_eq!(
        kill_rep.dict_get("state").and_then(PyRep::as_str),
        Some("Destroyed")
    );
    assert_eq!(
        kill_rep.dict_get("pendingOccupierFactionID").and_then(PyRep::as_i64),
        Some(CALDARI)
    );

    // Occupier has not flipped yet before downtime.
    let occ_before = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations[&sys_id].occupier_faction_id);
    assert_eq!(occ_before, GALLENTE);

    // 3. Downtime capture processing flips system occupier to Caldari and resets victory points.
    let captured = process_pending_fw_captures_on_downtime(&ctx);
    assert_eq!(captured, 1, "One system capture must be processed on downtime");

    let (occ_after, vp_after) = ctx.db.with_snapshot(|s| {
        let o = &s.fw_system_occupations[&sys_id];
        (o.occupier_faction_id, o.victory_points)
    });
    assert_eq!(occ_after, CALDARI, "System occupier flips to Caldari after downtime");
    assert_eq!(vp_after, 0, "Victory points reset to 0 after capture");
}
