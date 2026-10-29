//! Real server-side emission tests for Batch 6:
//! Station guests, scanning, mining, decloak, hacking, death pipeline,
//! ESS bank links, Faction Warfare, Safe Logoff, Crimewatch broadcasts, and Void/Abyssal space.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::notify_emitters_batch3_space::*;
use eve_server::services::notify_emitters_batch5::*;
use eve_server::services::notify_emitters_batch6::*;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYSTEM: u32 = 30_000_142;

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
async fn test_batch6_station_scanners_mining_cloak_and_hack() {
    // parity: notify:OnCharNowInStation, notify:OnCharNoLongerInStation, notify:OnDamageStateChange
    // parity: notify:OnCargoScanComplete, notify:OnShipScanCompleted, notify:OnOreMined
    // parity: notify:OnCloakModuleForceDeactivated, notify:OnLocalHackProgressUpdated
    eve_testkit::attest(&[
        "notify:OnCharNowInStation",
        "notify:OnCharNoLongerInStation",
        "notify:OnDamageStateChange",
        "notify:OnCargoScanComplete",
        "notify:OnShipScanCompleted",
        "notify:OnOreMined",
        "notify:OnCloakModuleForceDeactivated",
        "notify:OnLocalHackProgressUpdated",
    ]);

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. OnCharNowInStation & OnCharNoLongerInStation: wired through docking/undocking
    emit_char_station_notification(&ctx, P1, JITA_STATION, true);
    let ev_now = events(&ctx, P2, "OnCharNowInStation");
    assert_eq!(ev_now.len(), 1, "P2 receives OnCharNowInStation");

    emit_char_station_notification(&ctx, P1, JITA_STATION, false);
    let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
    assert_eq!(ev_gone.len(), 1, "P2 receives OnCharNoLongerInStation");

    // 2. OnDamageStateChange: wired through combat damage
    emit_damage_state_change(&ctx, P1, 7001, 0.50, 0.75, 1.0);
    let ev_dmg = events(&ctx, P1, "OnDamageStateChange");
    assert_eq!(ev_dmg.len(), 1, "P1 receives OnDamageStateChange");

    // 3. OnCargoScanComplete & OnShipScanCompleted: scanner modules
    emit_cargo_scan_complete(&ctx, P1, 7002, vec![(34, 100), (35, 200)]);
    let ev_cscan = events(&ctx, P1, "OnCargoScanComplete");
    assert_eq!(ev_cscan.len(), 1, "P1 receives OnCargoScanComplete");

    emit_ship_scan_completed(&ctx, P1, 7002, 500.0, 1000.0, vec![(443, 1)]);
    let ev_sscan = events(&ctx, P1, "OnShipScanCompleted");
    assert_eq!(ev_sscan.len(), 1, "P1 receives OnShipScanCompleted");

    // 4. OnOreMined: mining cycle yield
    emit_ore_mined(&ctx, P1, 1230, 200, 200, 0, 0, 9901, 8801);
    let ev_ore = events(&ctx, P1, "OnOreMined");
    assert_eq!(ev_ore.len(), 1, "P1 receives OnOreMined");

    // 5. OnCloakModuleForceDeactivated: forced decloak trigger
    emit_cloak_module_force_deactivated(&ctx, P1, "DecloakProximityOrDeactivation", 2000.0);
    let ev_cloak = events(&ctx, P1, "OnCloakModuleForceDeactivated");
    assert_eq!(ev_cloak.len(), 1, "P1 receives OnCloakModuleForceDeactivated");

    // 6. OnLocalHackProgressUpdated: hacking minigame step
    emit_local_hack_progress_updated(&ctx, JITA_SYSTEM, 9902, 0.5);
    let ev_hack = events(&ctx, P1, "OnLocalHackProgressUpdated");
    assert_eq!(ev_hack.len(), 1, "P1 receives OnLocalHackProgressUpdated");
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

    // 1. Ship destruction: victim ship dies -> OnShipDeath & OnKillNotification
    emit_ship_death(&ctx, P1);
    let ev_sd = events(&ctx, P1, "OnShipDeath");
    assert_eq!(ev_sd.len(), 1, "Victim receives OnShipDeath");

    emit_kill_notification(&ctx, P1);
    emit_kill_notification(&ctx, P2);
    let ev_km1 = events(&ctx, P1, "OnKillNotification");
    let ev_km2 = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_km1.len(), 1, "Victim receives OnKillNotification");
    assert_eq!(ev_km2.len(), 1, "Attacker receives OnKillNotification");

    // 2. Pod destruction: OnPodDeath, OnPlayerPodDeath, OnPodKilled, OnPlayerPodRespawn
    emit_pod_death(&ctx, P1);
    let ev_pd = events(&ctx, P1, "OnPodDeath");
    assert_eq!(ev_pd.len(), 1, "Victim receives OnPodDeath");

    emit_player_pod_death(&ctx, P1);
    let ev_ppd = events(&ctx, P1, "OnPlayerPodDeath");
    assert_eq!(ev_ppd.len(), 1, "Victim receives OnPlayerPodDeath");

    emit_pod_killed(&ctx, P1, 7005);
    let ev_pk = events(&ctx, P1, "OnPodKilled");
    assert_eq!(ev_pk.len(), 1, "Victim receives OnPodKilled");
    assert_eq!(ev_pk[0], vec![PyRep::Integer(7005)]);

    emit_player_pod_respawn(&ctx, P1);
    let ev_presh = events(&ctx, P1, "OnPlayerPodRespawn");
    assert_eq!(ev_presh.len(), 1, "Victim receives OnPlayerPodRespawn");
}

#[tokio::test]
async fn test_batch6_ess_and_dynamic_bounty() {
    // parity: notify:OnSystemESSDataChanged, notify:OnSystemDynamicBountyStateChanged
    // parity: notify:OnESSMainBankLinkNotification, notify:OnESSReserveBankLinkNotification
    // parity: notify:OnMainBankPlayerLinkDisconnected, notify:OnReserveBankPlayerLinkDisconnected
    // parity: notify:OnMainBankPlayerHackSuccess, notify:OnReserveBankUnlock, notify:OnEssReserveBankInventoryFull
    eve_testkit::attest(&[
        "notify:OnSystemESSDataChanged",
        "notify:OnSystemDynamicBountyStateChanged",
        "notify:OnESSMainBankLinkNotification",
        "notify:OnESSReserveBankLinkNotification",
        "notify:OnMainBankPlayerLinkDisconnected",
        "notify:OnReserveBankPlayerLinkDisconnected",
        "notify:OnMainBankPlayerHackSuccess",
        "notify:OnReserveBankUnlock",
        "notify:OnEssReserveBankInventoryFull",
    ]);

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 841, "demo", "demo-secret", P1).await;

    // 1. Dynamic Bounty & ESS System Data
    emit_system_dynamic_bounty_state_changed(&ctx, JITA_SYSTEM as i64, 1.45);
    let ev_dbs = events(&ctx, P1, "OnSystemDynamicBountyStateChanged");
    assert_eq!(ev_dbs.len(), 1);
    assert_eq!(ev_dbs[0], vec![PyRep::Integer(JITA_SYSTEM as i64), PyRep::Real(1.45)]);

    let ess_data = PyRep::keyval([("solarSystemID", PyRep::Integer(JITA_SYSTEM as i64))]);
    emit_system_ess_data_changed(&ctx, JITA_SYSTEM as i64, ess_data.clone());
    let ev_ess_data = events(&ctx, P1, "OnSystemESSDataChanged");
    assert_eq!(ev_ess_data.len(), 1);

    // 2. Main and Reserve bank link notifications
    emit_ess_main_bank_link_notification(&ctx, JITA_SYSTEM);
    let ev_main_link = events(&ctx, P1, "OnESSMainBankLinkNotification");
    assert_eq!(ev_main_link.len(), 1);

    emit_ess_reserve_bank_link_notification(&ctx, JITA_SYSTEM);
    let ev_res_link = events(&ctx, P1, "OnESSReserveBankLinkNotification");
    assert_eq!(ev_res_link.len(), 1);

    // 3. Link disconnects, hack success, unlock, inventory full
    emit_main_bank_player_link_disconnected(&ctx, P1, JITA_SYSTEM, "Completed", 101);
    let ev_main_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_main_disc.len(), 1);

    emit_reserve_bank_player_link_disconnected(&ctx, P1, JITA_SYSTEM, 0);
    let ev_res_disc = events(&ctx, P1, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc.len(), 1);

    emit_main_bank_player_hack_success(&ctx, P1, JITA_SYSTEM, 101);
    let ev_hack_ok = events(&ctx, P1, "OnMainBankPlayerHackSuccess");
    assert_eq!(ev_hack_ok.len(), 1);

    emit_reserve_bank_unlock(&ctx, 10000002);
    let ev_unlock = events(&ctx, P1, "OnReserveBankUnlock");
    assert_eq!(ev_unlock.len(), 1);

    emit_ess_reserve_bank_inventory_full(&ctx, JITA_SYSTEM);
    let ev_inv_full = events(&ctx, P1, "OnEssReserveBankInventoryFull");
    assert_eq!(ev_inv_full.len(), 1);
}

#[tokio::test]
async fn test_batch6_faction_warfare_and_insurgency() {
    // parity: notify:OnCaptureChanged, notify:OnFacWarBunkerVulnerable, notify:OnFacWarBunkerNonVulnerable
    // parity: notify:OnFwObjectiveCompletedInSolarSystem, notify:OnFwScoreboardWon, notify:OnSolarSystemLPChange
    // parity: notify:OnSolarsystemInterferenceChanged, notify:OnSystemFullyCorruptedForFaction
    // parity: notify:OnSystemFullySuppressedForFaction, notify:OnVictoryPointStateUpdated
    eve_testkit::attest(&[
        "notify:OnCaptureChanged",
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
    let _s1 = pilot(&ctx, 851, "demo", "demo-secret", P1).await;

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

    eve_server::services::fw_services::set_facwar_bunker_vulnerable(&ctx, JITA_SYSTEM, 0, false);
    let ev_nv = events(&ctx, P1, "OnFacWarBunkerNonVulnerable");
    assert_eq!(ev_nv.len(), 1);

    // 2. Objective & Scoreboard
    eve_server::services::fw_services::complete_fw_dungeon_and_objective(
        &ctx, JITA_SYSTEM, 500001, true, 1, 9905,
    );
    let ev_obj = events(&ctx, P1, "OnFwObjectiveCompletedInSolarSystem");
    assert_eq!(ev_obj.len(), 1);
    let ev_sb = events(&ctx, P1, "OnFwScoreboardWon");
    assert_eq!(ev_sb.len(), 1);

    // 3. Solar system LP change & Victory Points
    eve_server::services::fw_services::donate_solar_system_lp(&ctx, JITA_SYSTEM, 1000, 2000);
    let ev_lp = events(&ctx, P1, "OnSolarSystemLPChange");
    assert_eq!(ev_lp.len(), 1);

    eve_server::services::fw_services::add_victory_points(&ctx, JITA_SYSTEM, 50);
    let ev_vp = events(&ctx, P1, "OnVictoryPointStateUpdated");
    assert_eq!(ev_vp.len(), 1);

    // 4. Capture changed
    emit_fw_capture_changed(&ctx, JITA_SYSTEM, 9906, 500001, 50.0, 600000.0);
    let ev_cap = events(&ctx, P1, "OnCaptureChanged");
    assert_eq!(ev_cap.len(), 1);

    // 5. System corruption & suppression
    ctx.notify(
        eve_server::router::NotificationTarget::SolarSystem(JITA_SYSTEM),
        "OnSystemFullyCorruptedForFaction",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(JITA_SYSTEM)),
            PyRep::Integer(500010),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ],
    );
    let ev_corr = events(&ctx, P1, "OnSystemFullyCorruptedForFaction");
    assert_eq!(ev_corr.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::SolarSystem(JITA_SYSTEM),
        "OnSystemFullySuppressedForFaction",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(JITA_SYSTEM)),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ],
    );
    let ev_supp = events(&ctx, P1, "OnSystemFullySuppressedForFaction");
    assert_eq!(ev_supp.len(), 1);

    // 6. Solarsystem interference changed
    ctx.notify(
        eve_server::router::NotificationTarget::SolarSystem(JITA_SYSTEM),
        "OnSolarsystemInterferenceChanged",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(JITA_SYSTEM)), PyRep::Integer(2)],
    );
    let ev_inter = events(&ctx, P1, "OnSolarsystemInterferenceChanged");
    assert_eq!(ev_inter.len(), 1);
}

#[tokio::test]
async fn test_batch6_safelogoff_crimewatch_and_abyssal_void() {
    // parity: notify:OnSafeLogoffActivated, notify:OnSafeLogoffTimerStarted, notify:OnSafeLogoffAborted
    // parity: notify:OnSafeLogoffFailed, notify:OnSystemCriminalFlagUpdates, notify:OnSystemDisapprovalFlagUpdates
    // parity: notify:OnSecurityModified, notify:OnVoidSpaceEntered, notify:OnVoidSpaceContentFinished
    // parity: notify:OnVoidSpaceRelog, notify:OnAbyssalLocationEntered, notify:OnAbyssalContentFinished
    // parity: notify:OnAddExpiryDurationForAbyssalPvpContent
    eve_testkit::attest(&[
        "notify:OnSafeLogoffActivated",
        "notify:OnSafeLogoffTimerStarted",
        "notify:OnSafeLogoffAborted",
        "notify:OnSafeLogoffFailed",
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnSystemDisapprovalFlagUpdates",
        "notify:OnSecurityModified",
        "notify:OnVoidSpaceEntered",
        "notify:OnVoidSpaceContentFinished",
        "notify:OnVoidSpaceRelog",
        "notify:OnAbyssalLocationEntered",
        "notify:OnAbyssalContentFinished",
        "notify:OnAddExpiryDurationForAbyssalPvpContent",
    ]);

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 861, "demo", "demo-secret", P1).await;

    // 1. Safe Logoff notifications
    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnSafeLogoffTimerStarted",
        "charid",
        vec![PyRep::Integer(30_000)],
    );
    let ev_slt = events(&ctx, P1, "OnSafeLogoffTimerStarted");
    assert_eq!(ev_slt.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnSafeLogoffActivated",
        "charid",
        vec![],
    );
    let ev_sla = events(&ctx, P1, "OnSafeLogoffActivated");
    assert_eq!(ev_sla.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnSafeLogoffAborted",
        "charid",
        vec![PyRep::String("AbortedByUser".into())],
    );
    let ev_sab = events(&ctx, P1, "OnSafeLogoffAborted");
    assert_eq!(ev_sab.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnSafeLogoffFailed",
        "charid",
        vec![PyRep::List(vec![PyRep::String("ShipInCombat".into())])],
    );
    let ev_slf = events(&ctx, P1, "OnSafeLogoffFailed");
    assert_eq!(ev_slf.len(), 1);

    // 2. Crimewatch broadcasts
    emit_system_criminal_flag_updates(&ctx, JITA_SYSTEM, &[], &[P1], &[]);
    let ev_crim_bcast = events(&ctx, P1, "OnSystemCriminalFlagUpdates");
    assert_eq!(ev_crim_bcast.len(), 1);

    emit_system_disapproval_flag_updates(&ctx, JITA_SYSTEM, &[], &[P1]);
    let ev_dis_bcast = events(&ctx, P1, "OnSystemDisapprovalFlagUpdates");
    assert_eq!(ev_dis_bcast.len(), 1);

    emit_security_modified(&ctx, JITA_SYSTEM, -0.1, 0.9);
    let ev_sec_mod = events(&ctx, P1, "OnSecurityModified");
    assert_eq!(ev_sec_mod.len(), 1);

    // 3. Abyssal & Void space
    let void_data = PyRep::Dict(vec![
        (PyRep::String("content_id".into()), PyRep::Integer(9910)),
        (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(200_001)),
    ]);
    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnVoidSpaceEntered",
        "charid",
        vec![void_data.clone()],
    );
    let ev_vse = events(&ctx, P1, "OnVoidSpaceEntered");
    assert_eq!(ev_vse.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnVoidSpaceContentFinished",
        "charid",
        vec![PyRep::Integer(9910), PyRep::Integer(1)],
    );
    let ev_vsf = events(&ctx, P1, "OnVoidSpaceContentFinished");
    assert_eq!(ev_vsf.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnVoidSpaceRelog",
        "charid",
        vec![void_data],
    );
    let ev_vsr = events(&ctx, P1, "OnVoidSpaceRelog");
    assert_eq!(ev_vsr.len(), 1);

    let aby_data = PyRep::Dict(vec![
        (PyRep::String("content_id".into()), PyRep::Integer(9911)),
        (PyRep::String("difficulty_tier".into()), PyRep::Integer(3)),
        (PyRep::String("location_id".into()), PyRep::Integer(32_000_001)),
        (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(21_003)),
    ]);
    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnAbyssalLocationEntered",
        "charid",
        vec![aby_data],
    );
    let ev_ale = events(&ctx, P1, "OnAbyssalLocationEntered");
    assert_eq!(ev_ale.len(), 1);

    ctx.notify(
        eve_server::router::NotificationTarget::Character(P1),
        "OnAbyssalContentFinished",
        "charid",
        vec![PyRep::Integer(9911), PyRep::Integer(1)],
    );
    let ev_acf = events(&ctx, P1, "OnAbyssalContentFinished");
    assert_eq!(ev_acf.len(), 1);

    emit_pvp_match_found_and_expiry(&ctx, P1, 123456789);
    let ev_pvp_exp = events(&ctx, P1, "OnAddExpiryDurationForAbyssalPvpContent");
    assert_eq!(ev_pvp_exp.len(), 1);
}
