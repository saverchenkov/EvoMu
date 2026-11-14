#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

mod notify_real_emitters_batch6 {
    #[allow(unused_imports)]
    use super::*;

// Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
// Covers Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.

use std::collections::HashMap;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
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
    // Exercises Faction Warfare bunker vulnerability, objective completion, scoreboard, and suppression notices.
    eve_testkit::attest(&[
        "notify:OnFacWarBunkerNonVulnerable",
        "notify:OnFwObjectiveCompletedInSolarSystem",
        "notify:OnFwScoreboardWon",

        "notify:OnSystemFullySuppressedForFaction",
    ]);

    let ctx = new_ctx();
    let empty = HashMap::new();
    let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) =
        crate::fw_warzone_insurgency::setup_warzone_chain(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(occ) = s.fw_system_occupations.get_mut(&warzone_sys) {
            occ.victory_points = 0;
            occ.victory_points_threshold = 100;
        }
    });

    let _ = ctx.db.set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT);
    let mut h1 = common::new_wire_harness(&ctx, 851);
    h1.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &empty,
    )
    .await;
    h1.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P1)], &empty)
        .await;
    h1.call_ok(
        "fwCharacterEnlistmentMgr",
        None,
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_004)],
        &empty,
    )
    .await;
    ctx.advance_secs(31);
    h1.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    h1.call_ok(
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/tr {warzone_sys}"))],
        &empty,
    )
    .await;

    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&P1).map(|c| c.active_ship_id))
        .expect("P1 active ship");
    h1.transport_mut().inner.session.ship_id = Some(ship_id);

    // Fit 2x Small Autocannon + EMP S on P1 for Novice FW complex guards
    h1.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3300 5".into())], &empty).await;
    h1.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3301 5".into())], &empty).await;
    let fit1 = h1.call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty).await;
    let fit2 = h1.call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty).await;
    let gun1_id = fit1.dict_get("itemID").and_then(|v| v.as_i64()).expect("gun1 itemID");
    let gun2_id = fit2.dict_get("itemID").and_then(|v| v.as_i64()).expect("gun2 itemID");
    let ammo_rep = h1
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/giveitem 185 500".into())], &empty)
        .await;
    let ammo_id = ammo_rep.dict_get("itemID").and_then(|v| v.as_i64()).expect("ammo itemID");
    h1.call_ok(
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(vec![PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
        &empty,
    )
    .await;
    ctx.advance_secs(12);

    let _ = h1.call_ok("fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &empty).await;
    let _ = h1.call_ok("dungeonInstanceCacheMgr", None, "GetFactionWarfareInstances", &[], &empty).await;
    let bind = h1
        .call_ok("beyonce", None, "MachoBindObject", &[PyRep::Integer(i64::from(warzone_sys)), PyRep::None], &empty)
        .await;
    let wz_beyonce = eve_testkit::client_harness::extract_bound_id_from_wire_rep(&bind).expect("beyonce bound id");
    let wz_space = ctx.systems.get_or_spawn(warzone_sys);
    let _ = h1.drain_notifications();

    // 1 & 2. Capture live Novice FW complexes in warzone_sys via RPCs + ticks until bunker becomes vulnerable
    let mut vp_updated_seen = false;
    let mut obj_completed_seen = false;
    let mut scoreboard_won_seen = false;
    let mut bunker_vuln_seen = false;
    for _attempt in 0..35 {
        let gate_opt = ctx.db.with_snapshot(|s| {
            s.fw_runtime
                .fw_complexes
                .iter()
                .filter_map(|(&gid, &(csys, ref tier, _, _, done))| {
                    (csys == warzone_sys && tier == "Novice" && !done).then_some(gid)
                })
                .min()
        });
        let Some(gate_id) = gate_opt else {
            h1.call_ok("timeControl", None, "AdvanceClock", &[PyRep::Integer(1805)], &empty)
                .await;
            let _ = h1.call_ok("dungeonInstanceCacheMgr", None, "GetFactionWarfareInstances", &[], &empty).await;
            continue;
        };

        h1.call_ok(
            "beyonce",
            Some(wz_beyonce),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
            &empty,
        )
        .await;
        for _ in 0..30 {
            wz_space.step_manual(10.0).await.unwrap();
            if wz_space
                .get_ball_snapshot(ship_id)
                .await
                .unwrap()
                .is_some_and(|(_, b)| !matches!(b.mode, eve_destiny::MovementMode::Warp { .. }))
            {
                break;
            }
        }
        let _ = h1
            .call_ok("keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(gate_id)], &empty)
            .await;
        let guard_id = 500_000_000 + gate_id;
        h1.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(guard_id)], &empty).await;
        wz_space.step_manual(5.0).await.unwrap();

        for &gid in &[gun1_id, gun2_id] {
            h1.call_ok(
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("projectileFired".into()),
                    PyRep::Integer(guard_id),
                    PyRep::Integer(100),
                ],
                &empty,
            )
            .await;
        }
        for _ in 0..90 {
            if wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none() {
                break;
            }
            ctx.advance_secs(1);
            wz_space.step_manual(1.0).await.unwrap();
        }
        assert!(
            wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none(),
            "FW guard NPC {guard_id} must be destroyed via live combat"
        );

        for _ in 0..10 {
            if ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.get(&gate_id).is_none_or(|c| c.4)) {
                break;
            }
            h1.call_ok("timeControl", None, "AdvanceClock", &[PyRep::Integer(150)], &empty).await;
            eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
        }

        let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
        for (ev, args) in h1.drain_notifications() {
            match ev.as_str() {
                "OnVictoryPointStateUpdated" => {
                    assert_eq!(args.len(), 2);
                    assert_eq!(args[0], PyRep::Integer(i64::from(warzone_sys)));
                    assert_eq!(
                        args[1].dict_get("solarSystemID"),
                        Some(&PyRep::Integer(i64::from(warzone_sys)))
                    );
                    assert!(
                        args[1].dict_get("victoryPoints").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
                        "victoryPoints must advance on warzone complex capture"
                    );
                    assert_eq!(args[1].dict_get("threshold"), Some(&PyRep::Integer(100)));
                    vp_updated_seen = true;
                }
                "OnFwObjectiveCompletedInSolarSystem" => {
                    assert_eq!(args.len(), 3);
                    assert_eq!(args[0], PyRep::Integer(i64::from(warzone_sys)));
                    assert_eq!(args[1], PyRep::Integer(500_004));
                    obj_completed_seen = true;
                }
                "OnFwScoreboardWon" => {
                    assert_eq!(args.len(), 3);
                    assert_eq!(args[0], PyRep::Integer(i64::from(warzone_sys)));
                    assert_eq!(args[2], PyRep::Integer(500_004));
                    scoreboard_won_seen = true;
                }
                "OnFacWarBunkerVulnerable" => {
                    assert_eq!(args.len(), 1);
                    assert!(args[0].as_i64().unwrap_or(0) > 0);
                    bunker_vuln_seen = true;
                }
                _ => {}
            }
        }
        if bunker_vuln_seen {
            break;
        }
    }
    assert!(vp_updated_seen, "OnVictoryPointStateUpdated must fire on warzone complex capture");
    assert!(obj_completed_seen, "OnFwObjectiveCompletedInSolarSystem must fire on warzone complex capture");
    assert!(scoreboard_won_seen, "OnFwScoreboardWon must fire on warzone complex capture");
    assert!(bunker_vuln_seen, "OnFacWarBunkerVulnerable must fire at 100% contested in warzone_sys");

    // 2b. Unattacked I-Hub regeneration lowers contested % to 80% and emits OnFacWarBunkerNonVulnerable
    eve_server::services::fw_gameplay::regenerate_unattacked_fw_ihub(&ctx, warzone_sys);
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_nv: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnFacWarBunkerNonVulnerable")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_nv.len(), 1, "P1 receives OnFacWarBunkerNonVulnerable");
    assert_eq!(ev_nv[0], vec![PyRep::Integer(0)]);

    // 3. Solar system LP change in warzone_sys
    eve_server::services::fw_services::donate_solar_system_lp(&ctx, warzone_sys, 1000, 2000);
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_lp: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSolarSystemLPChange")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_lp.len(), 1);
    assert_eq!(ev_lp[0], vec![PyRep::Integer(1000), PyRep::Integer(2000)]);

    // 4. System corruption & suppression in warzone_sys
    let camp_id = 9999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            camp_id,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: camp_id,
                pirate_faction_id: 500010,
                target_faction_id: 500001,
                fob_solar_system_id: warzone_sys,
                systems: vec![warzone_sys],
                created_at_unix: 1000,
                forecasting_end_unix: 2000,
                active_end_unix: 2_000_000_000,
                winddown_end_unix: 2_000_000_000,
                corruption_points: Default::default(),
                suppression_points: Default::default(),
            },
        );
    });

    eve_server::services::insurgency_services::add_system_corruption(&ctx, camp_id, warzone_sys, 1000);
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_corr: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSystemFullyCorruptedForFaction")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_corr.len(), 1, "P1 receives OnSystemFullyCorruptedForFaction");
    assert_eq!(
        ev_corr[0],
        vec![
            PyRep::Integer(i64::from(warzone_sys)),
            PyRep::Integer(500010),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ]
    );

    eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, warzone_sys, 1000);
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_supp: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSystemFullySuppressedForFaction")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_supp.len(), 1, "P1 receives OnSystemFullySuppressedForFaction");
    assert_eq!(
        ev_supp[0],
        vec![
            PyRep::Integer(i64::from(warzone_sys)),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ]
    );

    // 5. Stellar interference in warzone_sys
    eve_server::services::pve_special_sites::record_system_interference_activity_with_char(
        &ctx,
        P1,
        warzone_sys,
        0.40,
    );
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_interf: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSolarsystemInterferenceChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_interf.len(), 1, "P1 receives OnSolarsystemInterferenceChanged");
    assert_eq!(ev_interf[0].len(), 2);
    assert_eq!(ev_interf[0][0], PyRep::Integer(i64::from(warzone_sys)));
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
    let empty = HashMap::new();
    let _ = ctx.db.set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT);
    let _ = ctx.db.set_account_role("other", eve_db::service_roles::ROLE_GM_DEFAULT);

    let mut h1 = common::new_wire_harness(&ctx, 861);
    h1.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &empty,
    )
    .await;
    h1.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P1)], &empty)
        .await;
    ctx.advance_secs(12);
    let _ = h1.drain_notifications();

    // 1. SafeLogoff while docked -> fails with SAFE_LOGOFF_DOCKED and emits OnSafeLogoffFailed
    let fail_res = h1.call_ok("ship", None, "SafeLogoff", &[], &empty).await;
    assert_eq!(fail_res, PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())]));
    let ev_slf: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSafeLogoffFailed")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_slf.len(), 1, "P1 receives OnSafeLogoffFailed");
    assert_eq!(ev_slf[0], vec![PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])]);

    // 2. Undock into space
    ctx.advance_secs(31);
    h1.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    let _ = h1.drain_notifications();

    // 3. Valid SafeLogoff in space -> emits OnSafeLogoffTimerStarted
    let expected_ft = eve_server::sp6_lifecycle::unix_to_filetime(ctx.now_unix() + 30);
    let start_res = h1.call_ok("ship", None, "SafeLogoff", &[], &empty).await;
    assert_eq!(start_res, PyRep::List(vec![]));
    let ev_slt: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSafeLogoffTimerStarted")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_slt.len(), 1, "P1 receives OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt[0], vec![PyRep::Integer(expected_ft)]);

    // 4. Abort SafeLogoff -> emits OnSafeLogoffAborted
    let abort_res = h1.call_ok("ship", None, "AbortSafeLogoff", &[], &empty).await;
    assert_eq!(abort_res, PyRep::Bool(true));
    let ev_sab: Vec<_> = h1
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSafeLogoffAborted")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_sab.len(), 1, "P1 receives OnSafeLogoffAborted");
    assert_eq!(ev_sab[0], vec![PyRep::String("SAFE_LOGOFF_ABORT_GENERIC".into())]);

    // 5. Restart SafeLogoff, advance 31 seconds -> disconnects and emits OnSafeLogoffActivated
    h1.call_ok("ship", None, "SafeLogoff", &[], &empty).await;
    let _ = h1.drain_notifications();

    ctx.advance_secs(31);
    let ev_sla = events(&ctx, P1, "OnSafeLogoffActivated");
    assert_eq!(ev_sla.len(), 1, "P1 receives OnSafeLogoffActivated");
    assert!(ev_sla[0].is_empty(), "OnSafeLogoffActivated has arity 0");

    // 6. Live Crimewatch high-sec aggression in Jita -> OnSystemCriminalFlagUpdates on bystander wire
    let cw_ctx = new_ctx();
    let _ = cw_ctx.db.set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT);
    let _ = cw_ctx.db.set_account_role("other", eve_db::service_roles::ROLE_GM_DEFAULT);

    let mut h1_cw = common::new_wire_harness(&cw_ctx, 862);
    h1_cw
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &empty,
        )
        .await;
    h1_cw
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P1)], &empty)
        .await;
    cw_ctx.advance_secs(31);
    h1_cw.call_ok("stationSvc", None, "Undock", &[], &empty).await;

    let mut h2_cw = common::new_wire_harness(&cw_ctx, 863);
    h2_cw
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("other".into()), PyRep::String("other-secret".into())],
            &empty,
        )
        .await;
    h2_cw
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P2)], &empty)
        .await;
    cw_ctx.advance_secs(31);
    h2_cw.call_ok("stationSvc", None, "Undock", &[], &empty).await;

    let ship1 = cw_ctx.db.with_snapshot(|s| s.characters[&P1].active_ship_id);
    let ship2 = cw_ctx.db.with_snapshot(|s| s.characters[&P2].active_ship_id);
    let sys_jita = cw_ctx.systems.get_or_spawn(JITA_SYSTEM);
    let ship1_pos = sys_jita
        .get_ball_snapshot(ship1)
        .await
        .unwrap()
        .expect("ship1 ball in Jita")
        .1
        .position;
    sys_jita
        .set_ball_position(ship2, ship1_pos + glam::DVec3::new(500.0, 0.0, 0.0))
        .await;

    h1_cw.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3300 5".into())], &empty).await;
    h1_cw.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3301 5".into())], &empty).await;
    let fit_cw = h1_cw.call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty).await;
    let gun_cw = fit_cw.dict_get("itemID").and_then(|v| v.as_i64()).expect("gun_cw itemID");
    let ammo_cw = h1_cw
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/giveitem 185 100".into())], &empty)
        .await;
    let ammo_cw_id = ammo_cw.dict_get("itemID").and_then(|v| v.as_i64()).expect("ammo_cw itemID");
    h1_cw
        .call_ok(
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship1),
                PyRep::List(vec![PyRep::Integer(gun_cw)]),
                PyRep::List(vec![PyRep::Integer(ammo_cw_id)]),
            ],
            &empty,
        )
        .await;
    cw_ctx.advance_secs(12);
    h1_cw.call_ok("crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &empty).await;
    h1_cw.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(ship2)], &empty).await;
    sys_jita.step_manual(10.0).await.unwrap();

    let _ = h2_cw.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h1_cw.drain_notifications();
    assert!(
        !h2_cw.drain_notifications().iter().any(|(e, _)| e == "OnSystemCriminalFlagUpdates"),
        "OnSystemCriminalFlagUpdates must not fire before illegal aggression"
    );

    h1_cw
        .call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gun_cw),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(ship2),
            ],
            &empty,
        )
        .await;

    let _ = h2_cw.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_crim: Vec<_> = h2_cw
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSystemCriminalFlagUpdates")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_crim.len(), 1, "bystander P2 receives OnSystemCriminalFlagUpdates on the wire");
    assert_eq!(
        ev_crim[0],
        vec![
            PyRep::List(vec![]),
            PyRep::List(vec![]),
            PyRep::List(vec![PyRep::Integer(P1)]),
        ]
    );

    // 7. Live Zarzakh aggression -> OnSystemDisapprovalFlagUpdates in Zarzakh (30100000)
    const ZARZAKH_SYSTEM: u32 = 30_100_000;
    const FULCRUM_STATION: i64 = 60_015_187;
    let z_ctx = new_ctx();
    let _ = z_ctx.db.set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT);
    let _ = z_ctx.db.set_account_role("other", eve_db::service_roles::ROLE_GM_DEFAULT);
    let fulcrum_pos = z_ctx.sde.stations[&FULCRUM_STATION].position;

    let mut h1_z = common::new_wire_harness(&z_ctx, 864);
    h1_z.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &empty,
    )
    .await;
    h1_z.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P1)], &empty)
        .await;
    z_ctx.advance_secs(31);
    h1_z.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    h1_z.call_ok("slash", None, "SlashCmd", &[PyRep::String(format!("/tr {FULCRUM_STATION}"))], &empty)
        .await;

    let mut h2_z = common::new_wire_harness(&z_ctx, 865);
    h2_z.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String("other".into()), PyRep::String("other-secret".into())],
        &empty,
    )
    .await;
    h2_z.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P2)], &empty)
        .await;
    z_ctx.advance_secs(31);
    h2_z.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    h2_z.call_ok("slash", None, "SlashCmd", &[PyRep::String(format!("/tr {FULCRUM_STATION}"))], &empty)
        .await;

    let z_ship1 = z_ctx.db.with_snapshot(|s| s.characters[&P1].active_ship_id);
    let z_ship2 = z_ctx.db.with_snapshot(|s| s.characters[&P2].active_ship_id);
    let z_sys = z_ctx.systems.get_or_spawn(ZARZAKH_SYSTEM);
    z_sys.set_ball_position(z_ship2, fulcrum_pos + glam::DVec3::new(5_500.0, 0.0, 0.0)).await;

    h1_z.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3300 5".into())], &empty).await;
    h1_z.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3301 5".into())], &empty).await;
    let fit_z = h1_z.call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty).await;
    let gun_z = fit_z.dict_get("itemID").and_then(|v| v.as_i64()).expect("gun_z itemID");
    let ammo_z = h1_z
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/giveitem 185 100".into())], &empty)
        .await;
    let ammo_z_id = ammo_z.dict_get("itemID").and_then(|v| v.as_i64()).expect("ammo_z itemID");
    h1_z.call_ok(
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(z_ship1),
            PyRep::List(vec![PyRep::Integer(gun_z)]),
            PyRep::List(vec![PyRep::Integer(ammo_z_id)]),
        ],
        &empty,
    )
    .await;
    z_ctx.advance_secs(12);
    h1_z.call_ok("crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)], &empty).await;
    h1_z.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(z_ship2)], &empty).await;
    z_sys.step_manual(10.0).await.unwrap();

    let _ = h2_z.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h1_z.drain_notifications();
    assert!(
        !h2_z.drain_notifications().iter().any(|(e, _)| e == "OnSystemDisapprovalFlagUpdates"),
        "OnSystemDisapprovalFlagUpdates must not fire before Zarzakh aggression"
    );

    h1_z.call_ok(
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gun_z),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(z_ship2),
        ],
        &empty,
    )
    .await;

    let _ = h2_z.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let ev_dis: Vec<_> = h2_z
        .drain_notifications()
        .into_iter()
        .filter(|(e, _)| e == "OnSystemDisapprovalFlagUpdates")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ev_dis.len(), 1, "bystander P2 receives OnSystemDisapprovalFlagUpdates in Zarzakh");
    assert_eq!(
        ev_dis[0],
        vec![PyRep::List(vec![]), PyRep::List(vec![PyRep::Integer(P1)])]
    );
}

}
