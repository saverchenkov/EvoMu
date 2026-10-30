//! Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
//! Covers Station, Ship/Pod destruction, ESS, Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_destiny::{Ball, MovementMode};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30000142;
const JITA_STATION: i64 = 60003760;

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

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_batch6_station_scanners_mining_cloak_and_hack() {
    // parity: notify:OnCharNowInStation, notify:OnCharNoLongerInStation
    // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
    // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
    // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
    // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
    // DATA GAP: notify:OnCloakModuleForceDeactivated requires ship cloak proximity/deactivation wiring
    // DATA GAP: notify:OnLocalHackProgressUpdated requires hacking minigame live container state wiring
    eve_testkit::attest(&[
        "notify:OnCharNowInStation",
        "notify:OnCharNoLongerInStation",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. Station Undock -> emits OnCharNoLongerInStation to station pilots
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &HashMap::new(), None)
        .await
        .expect("Undock");
    let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
    assert_eq!(ev_gone.len(), 1, "P2 receives OnCharNoLongerInStation");
    assert_eq!(
        ev_gone[0],
        vec![PyRep::Tuple(vec![
            PyRep::Integer(P1),
            PyRep::Integer(1_000_170),
            PyRep::None,
            PyRep::None,
        ])]
    );

    // Station Dock -> emits OnCharNowInStation to station pilots
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Dock", &[PyRep::Integer(JITA_STATION)], &HashMap::new(), None)
        .await
        .expect("Dock");
    let ev_now = events(&ctx, P2, "OnCharNowInStation");
    assert_eq!(ev_now.len(), 1, "P2 receives OnCharNowInStation");
    assert_eq!(
        ev_now[0],
        vec![PyRep::Tuple(vec![
            PyRep::Integer(P1),
            PyRep::Integer(1_000_170),
            PyRep::None,
            PyRep::None,
        ])]
    );
}

#[tokio::test]
async fn test_batch6_ship_and_pod_destruction_pipeline() {
    // parity: notify:OnKillNotification, notify:OnShipDeath, notify:OnPodDeath
    // parity: notify:OnPodKilled, notify:OnPlayerPodDeath, notify:OnPlayerPodRespawn
    eve_testkit::attest(&[
        "notify:OnKillNotification",
        "notify:OnShipDeath",
        "notify:OnPodDeath",
        "notify:OnPodKilled",
        "notify:OnPlayerPodDeath",
        "notify:OnPlayerPodRespawn",
    ]);

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 831, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 832, "other", "other-secret", P2).await;

    let ship1: i64 = 770_001;
    let pod1: i64 = 770_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship1, make_item(ship1, 587, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
        snap.items.insert(pod1, make_item(pod1, 670, P1, JITA_SYSTEM as i64, InventoryFlag::Hangar, 1, false));
    });

    // 1. Ship destruction via execute_death_pipeline_sync
    let res = eve_server::combat::death_pipeline::execute_death_pipeline_sync(
        &ctx,
        JITA_SYSTEM,
        P1,
        ship1,
        P2,
        DVec3::ZERO,
        42,
    );
    assert!(res.is_ok(), "execute_death_pipeline_sync must succeed");

    let ev_sd = events(&ctx, P1, "OnShipDeath");
    assert_eq!(ev_sd.len(), 1, "P1 receives OnShipDeath");
    assert!(ev_sd[0].is_empty(), "OnShipDeath has arity 0");

    let ev_km_vic = events(&ctx, P1, "OnKillNotification");
    assert_eq!(ev_km_vic.len(), 1, "Victim P1 receives OnKillNotification");
    assert!(ev_km_vic[0].is_empty(), "OnKillNotification has arity 0");

    let ev_km_atk = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_km_atk.len(), 1, "Attacker P2 receives OnKillNotification");
    assert!(ev_km_atk[0].is_empty(), "OnKillNotification has arity 0");

    // 2. Pod destruction via execute_death_pipeline_sync_inner with authentic combat kill (is_self_destruct = false)
    let res_pod = eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner(
        &ctx,
        JITA_SYSTEM,
        P1,
        pod1,
        P2,
        DVec3::ZERO,
        43,
        false,
    );
    assert!(res_pod.is_ok(), "execute_death_pipeline_sync_inner for pod must succeed");

    let ev_pd = events(&ctx, P1, "OnPodDeath");
    assert_eq!(ev_pd.len(), 1, "P1 receives OnPodDeath");
    assert!(ev_pd[0].is_empty(), "OnPodDeath has arity 0");

    let ev_ppd = events(&ctx, P1, "OnPlayerPodDeath");
    assert_eq!(ev_ppd.len(), 1, "P1 receives OnPlayerPodDeath");
    assert!(ev_ppd[0].is_empty(), "OnPlayerPodDeath has arity 0");

    let ev_pk = events(&ctx, P1, "OnPodKilled");
    assert_eq!(ev_pk.len(), 1, "P1 receives OnPodKilled");
    assert_eq!(ev_pk[0], vec![PyRep::Integer(pod1)]);

    let ev_respawn = events(&ctx, P1, "OnPlayerPodRespawn");
    assert_eq!(ev_respawn.len(), 1, "P1 receives OnPlayerPodRespawn");
    assert!(ev_respawn[0].is_empty(), "OnPlayerPodRespawn has arity 0");

    let ev_km_atk2 = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_km_atk2.len(), 1, "Attacker P2 receives OnKillNotification for pod");
    assert!(ev_km_atk2[0].is_empty(), "OnKillNotification for pod has arity 0");
}

#[tokio::test]
async fn test_batch6_ess_and_dynamic_bounty() {
    // parity: notify:OnSystemESSDataChanged
    // parity: notify:OnESSMainBankLinkNotification, notify:OnESSReserveBankLinkNotification
    // parity: notify:OnMainBankPlayerLinkDisconnected, notify:OnReserveBankPlayerLinkDisconnected
    // parity: notify:OnMainBankPlayerHackSuccess, notify:OnReserveBankUnlock
    // DATA GAP: notify:OnSystemDynamicBountyStateChanged requires dynamic bounty multiplier stepping from ratting bounty / pvp loss path
    // DATA GAP: notify:OnEssReserveBankInventoryFull requires ESS container inventory overflow wiring
    eve_testkit::attest(&[
        "notify:OnSystemESSDataChanged",
        "notify:OnESSMainBankLinkNotification",
        "notify:OnESSReserveBankLinkNotification",
        "notify:OnMainBankPlayerLinkDisconnected",
        "notify:OnReserveBankPlayerLinkDisconnected",
        "notify:OnMainBankPlayerHackSuccess",
        "notify:OnReserveBankUnlock",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 841, "demo", "demo-secret", P1).await;
    let nullsec_sys_id = 30_004_759; // 1DQ1-A (K-space null-sec in SW quadrant)
    let ess_ball_id = 899_001_i64;
    let key_item_id = 899_002_i64;
    let ship_id = 899_003_i64;

    s1.solar_system_id = Some(nullsec_sys_id);
    s1.ship_id = Some(ship_id);
    s1.station_id = None;
    s1.structure_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = nullsec_sys_id;
            ch.active_ship_id = ship_id;
        }
        let st = snap.system_states.entry(nullsec_sys_id).or_default();
        st.ess_main_bank = eve_db::IskCents(50_000_000_00);
        st.ess_reserve_bank = eve_db::IskCents(100_000_000_00);
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, P1, nullsec_sys_id as i64, InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            key_item_id,
            make_item(key_item_id, 57720, P1, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(P1, eve_server::services::combat_crimewatch::SafetyLevel::Yellow);
    });

    let sys_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ess_ball_id,
            type_id: 32882,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. ESS Main Bank Link start
    let kw = HashMap::new();
    let res_start = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank start");
    assert_eq!(res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    let ev_main_link = events(&ctx, P1, "OnESSMainBankLinkNotification");
    assert_eq!(ev_main_link.len(), 1, "P1 receives OnESSMainBankLinkNotification");
    assert_eq!(ev_main_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

    // 2. ESS Main Bank Hack complete (advance 5 ticks)
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let res_comp = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank complete");
    assert_eq!(res_comp.dict_get("stolenISK"), Some(&PyRep::Real(50_000_000.0)));

    let ev_hack_ok = events(&ctx, P1, "OnMainBankPlayerHackSuccess");
    assert_eq!(ev_hack_ok.len(), 1, "P1 receives OnMainBankPlayerHackSuccess");
    assert_eq!(ev_hack_ok[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_hack_ok[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_hack_ok[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

    let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc.len(), 1, "P1 receives OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_disc[0][0].dict_get("reason"), Some(&PyRep::String("Completed".into())));
    assert_eq!(ev_disc[0][0].dict_get("linkID"), Some(&PyRep::Integer(ess_ball_id)));

    let ev_ess_data = events(&ctx, P1, "OnSystemESSDataChanged");
    assert_eq!(ev_ess_data.len(), 1, "P1 receives OnSystemESSDataChanged");
    assert_eq!(ev_ess_data[0][0], PyRep::Integer(nullsec_sys_id as i64));
    assert_eq!(ev_ess_data[0][1].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_ess_data[0][1].dict_get("mainBank"), Some(&PyRep::Real(0.0)));
    assert_eq!(ev_ess_data[0][1].dict_get("reserveBank"), Some(&PyRep::Real(100_000_000.0)));

    // 3. ESS Reserve Bank Link start (with key)
    let res_res_start = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank start");
    assert_eq!(res_res_start.dict_get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    let ev_res_link = events(&ctx, P1, "OnESSReserveBankLinkNotification");
    assert_eq!(ev_res_link.len(), 1, "P1 receives OnESSReserveBankLinkNotification");
    assert_eq!(ev_res_link[0], vec![PyRep::Integer(nullsec_sys_id as i64)]);

    let ev_unlock = events(&ctx, P1, "OnReserveBankUnlock");
    assert_eq!(ev_unlock.len(), 1, "P1 receives OnReserveBankUnlock");
    assert_eq!(ev_unlock[0][0].dict_get("regionID"), Some(&PyRep::Integer(10000060)));

    // Advance 5 ticks and complete Reserve Bank link
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let res_res_comp = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank complete");
    assert_eq!(res_res_comp.dict_get("stolenISK"), Some(&PyRep::Real(6_666_666.66)));

    let ev_res_disc = events(&ctx, P1, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc.len(), 1, "P1 receives OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_res_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_res_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));
}

#[tokio::test]
async fn test_batch6_faction_warfare_and_insurgency() {
    // parity: notify:OnFacWarBunkerVulnerable, notify:OnFacWarBunkerNonVulnerable
    // parity: notify:OnFwObjectiveCompletedInSolarSystem, notify:OnFwScoreboardWon, notify:OnSolarSystemLPChange
    // parity: notify:OnSolarsystemInterferenceChanged, notify:OnSystemFullyCorruptedForFaction
    // parity: notify:OnSystemFullySuppressedForFaction, notify:OnVictoryPointStateUpdated
    // DATA GAP: notify:OnCaptureChanged requires real-time FW complex capture timer progression wiring
    eve_testkit::attest(&[
        "notify:OnFacWarBunkerVulnerable",
        "notify:OnFacWarBunkerNonVulnerable",
        "notify:OnFwObjectiveCompletedInSolarSystem",
        "notify:OnFwScoreboardWon",
        "notify:OnSolarSystemLPChange",
        "notify:OnSolarsystemInterferenceChanged",
        "notify:OnSystemFullyCorruptedForFaction",
        "notify:OnSystemFullySuppressedForFaction",
        "notify:OnVictoryPointStateUpdated",
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
    assert_eq!(ev_interf[0], vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Real(0.50), PyRep::Integer(2)]);
}

#[tokio::test]
async fn test_batch6_safelogoff_and_crimewatch() {
    // parity: notify:OnSafeLogoffActivated, notify:OnSafeLogoffTimerStarted, notify:OnSafeLogoffAborted
    // parity: notify:OnSafeLogoffFailed, notify:OnSystemCriminalFlagUpdates, notify:OnSystemDisapprovalFlagUpdates
    eve_testkit::attest(&[
        "notify:OnSafeLogoffActivated",
        "notify:OnSafeLogoffTimerStarted",
        "notify:OnSafeLogoffAborted",
        "notify:OnSafeLogoffFailed",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnSystemDisapprovalFlagUpdates",
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
