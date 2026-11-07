#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

mod notify_real_emitters_batch6 {
    #[allow(unused_imports)]
    use super::*;

// Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
// Covers Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.

use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const JITA_SYSTEM: u32 = 30000142;
fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_batch6_faction_warfare_and_insurgency() {
    // parity: notify:OnFacWarBunkerNonVulnerable
    // parity: notify:OnFwObjectiveCompletedInSolarSystem, notify:OnFwScoreboardWon
    // parity: notify:OnSystemFullySuppressedForFaction
    // DATA GAP: notify:OnCaptureChanged requires real-time FW complex capture timer progression wiring
    eve_testkit::attest(&[
        "notify:OnFacWarBunkerNonVulnerable",
        "notify:OnFwObjectiveCompletedInSolarSystem",
        "notify:OnFwScoreboardWon",

        "notify:OnSystemFullySuppressedForFaction",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 851, "demo", "demo-secret", P1).await;

    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.insert(
            JITA_SYSTEM,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: JITA_SYSTEM,
                occupier_faction_id: 500_004,
                owner_faction_id: 500_001,
                victory_points: 1200,
                victory_points_threshold: 3000,
            },
        );
    });

    // 1. Bunker vulnerability
    eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 500001, true);
    let ev_v = events(&ctx, P1, "OnFacWarBunkerVulnerable");
    assert_eq!(ev_v.len(), 1);
    assert_eq!(ev_v[0], vec![PyRep::Integer(500001)]);

    eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 0, false);
    let ev_nv = events(&ctx, P1, "OnFacWarBunkerNonVulnerable");
    assert_eq!(ev_nv.len(), 1);
    assert_eq!(ev_nv[0], vec![PyRep::Integer(0)]);

    // 2. Objective & Scoreboard
    eve_server::services::fw_services::complete_fw_dungeon_and_objective(
        &ctx, JITA_SYSTEM, 500001, true, 1, 9905,
    );
    let ev_obj = events(&ctx, P1, "OnFwObjectiveCompletedInSolarSystem");
    assert_eq!(ev_obj.len(), 1);
    assert_eq!(ev_obj[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500001), PyRep::Integer(1)]);

    let ev_sb = events(&ctx, P1, "OnFwScoreboardWon");
    assert_eq!(ev_sb.len(), 1);
    assert_eq!(ev_sb[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(9905), PyRep::Integer(500001)]);

    // 3. Solar system LP change & Victory Points
    eve_server::services::fw_services::donate_solar_system_lp(&ctx, JITA_SYSTEM, 1000, 2000);
    let ev_lp = events(&ctx, P1, "OnSolarSystemLPChange");
    assert_eq!(ev_lp.len(), 1);
    assert_eq!(ev_lp[0], vec![PyRep::Integer(1000), PyRep::Integer(2000)]);

    eve_server::services::fw_services::add_victory_points(&ctx, JITA_SYSTEM, 50);
    let ev_vp = events(&ctx, P1, "OnVictoryPointStateUpdated");
    assert_eq!(ev_vp.len(), 1);
    assert_eq!(ev_vp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));
    assert_eq!(ev_vp[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
    assert_eq!(ev_vp[0][1].dict_get("victoryPoints"), Some(&PyRep::Integer(1250)));
    assert_eq!(ev_vp[0][1].dict_get("threshold"), Some(&PyRep::Integer(3000)));

    // 4. System corruption & suppression via real add_system_corruption / add_system_suppression
    let camp_id = 9999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: 500010,
                target_faction_id: 500001,
                fob_solar_system_id: JITA_SYSTEM,
                systems: vec![JITA_SYSTEM],
                created_at_unix: 1000,
                forecasting_end_unix: 2000,
                active_end_unix: 2_000_000_000,
                winddown_end_unix: 2_000_000_000,
                corruption_points: Default::default(),
                suppression_points: Default::default(),
            },
        );
    });

    eve_server::services::insurgency_services::add_system_corruption(&ctx, camp_id, JITA_SYSTEM, 1000);
    let ev_corr = events(&ctx, P1, "OnSystemFullyCorruptedForFaction");
    assert_eq!(ev_corr.len(), 1, "P1 receives OnSystemFullyCorruptedForFaction");
    assert_eq!(ev_corr[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(500010), PyRep::Integer(1000), PyRep::Integer(1000)]);

    eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, JITA_SYSTEM, 1000);
    let ev_supp = events(&ctx, P1, "OnSystemFullySuppressedForFaction");
    assert_eq!(ev_supp.len(), 1, "P1 receives OnSystemFullySuppressedForFaction");
    assert_eq!(ev_supp[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(1000), PyRep::Integer(1000)]);

    // 5. Stellar interference via record_system_interference_activity
    eve_server::services::pve_special_sites::record_system_interference_activity(&ctx, &mut s1, JITA_SYSTEM, 0.40);
    let ev_interf = events(&ctx, P1, "OnSolarsystemInterferenceChanged");
    assert_eq!(ev_interf.len(), 1, "P1 receives OnSolarsystemInterferenceChanged");
    assert_eq!(ev_interf[0].len(), 2);
    assert_eq!(ev_interf[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));
    assert_eq!(ev_interf[0][1].dict_get("interferenceBand"), Some(&PyRep::Integer(2)));
}

#[tokio::test]
async fn test_batch6_safelogoff_and_crimewatch() {
    // parity: notify:OnSafeLogoffActivated, notify:OnSafeLogoffTimerStarted, notify:OnSafeLogoffAborted
    // parity: notify:OnSafeLogoffFailed
    eve_testkit::attest(&[
        "notify:OnSafeLogoffActivated",
        "notify:OnSafeLogoffTimerStarted",
        "notify:OnSafeLogoffAborted",
        "notify:OnSafeLogoffFailed",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 861, "demo", "demo-secret", P1).await;
    let empty = HashMap::new();

    // 1. SafeLogoff while docked -> fails with SAFE_LOGOFF_DOCKED and emits OnSafeLogoffFailed
    let fail_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff docked");
    assert_eq!(fail_res, PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())]));
    let ev_slf = events(&ctx, P1, "OnSafeLogoffFailed");
    assert_eq!(ev_slf.len(), 1, "P1 receives OnSafeLogoffFailed");
    assert_eq!(ev_slf[0], vec![PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])]);

    // 2. Undock into space
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &empty, None)
        .await
        .expect("Undock");

    // 3. Valid SafeLogoff in space -> emits OnSafeLogoffTimerStarted
    let expected_ft = eve_server::sp6_lifecycle::unix_to_filetime(ctx.now_unix() + 30);
    let start_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff space");
    assert_eq!(start_res, PyRep::List(vec![]));
    let ev_slt = events(&ctx, P1, "OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt.len(), 1, "P1 receives OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt[0], vec![PyRep::Integer(expected_ft)]);

    // 4. Abort SafeLogoff -> emits OnSafeLogoffAborted
    let abort_res = ctx
        .dispatch_rpc(&mut s1, "ship", None, "AbortSafeLogoff", &[], &empty, None)
        .await
        .expect("AbortSafeLogoff");
    assert_eq!(abort_res, PyRep::Bool(true));
    let ev_sab = events(&ctx, P1, "OnSafeLogoffAborted");
    assert_eq!(ev_sab.len(), 1, "P1 receives OnSafeLogoffAborted");
    assert_eq!(ev_sab[0], vec![PyRep::String("SAFE_LOGOFF_ABORT_GENERIC".into())]);

    // 5. Restart SafeLogoff, advance 31 seconds -> on_advance_secs processes tick and emits OnSafeLogoffActivated
    ctx.dispatch_rpc(&mut s1, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff restart");
    let _ = events(&ctx, P1, "OnSafeLogoffTimerStarted");

    ctx.advance_secs(31);
    let ev_sla = events(&ctx, P1, "OnSafeLogoffActivated");
    assert_eq!(ev_sla.len(), 1, "P1 receives OnSafeLogoffActivated");
    assert!(ev_sla[0].is_empty(), "OnSafeLogoffActivated has arity 0");

    let _s1_relog = pilot(&ctx, 862, "demo", "demo-secret", P1).await;

    // 6. Crimewatch flag updates broadcast to system via real emit_crimewatch_timer_notifications
    let now_unix = ctx.now_unix_secs();
    if let Ok(mut cw) = ctx.crimewatch.write() {
        let entry = cw.states.entry(P1).or_default();
        entry.suspect_flag_until_unix = now_unix + 900;
    }
    eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
    let ev_crim = events(&ctx, P1, "OnSystemCriminalFlagUpdates");
    assert_eq!(ev_crim.len(), 1, "P1 receives OnSystemCriminalFlagUpdates");
    assert_eq!(
        ev_crim[0],
        vec![
            PyRep::List(vec![]),
            PyRep::List(vec![PyRep::Integer(P1)]),
            PyRep::List(vec![]),
        ]
    );

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(P1, now_unix + 300);
    });
    eve_server::services::combat_crimewatch::emit_crimewatch_timer_notifications(&ctx, P1, now_unix);
    let ev_dis = events(&ctx, P1, "OnSystemDisapprovalFlagUpdates");
    assert_eq!(ev_dis.len(), 1, "P1 receives OnSystemDisapprovalFlagUpdates");
    assert_eq!(
        ev_dis[0],
        vec![PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(P1)])]
    );
}

}
