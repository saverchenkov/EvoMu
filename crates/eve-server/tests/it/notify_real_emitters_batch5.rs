//! Real server-side emission tests for Batch 5:
//! Station guest list events, Crimewatch timers, combat damage updates,
//! shutdown announcements, scanners, mining, space destiny, and ESS bank links.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::notify_emitters_batch3_space::*;
use eve_server::services::notify_emitters_batch5::*;
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
async fn test_batch5_station_damage_crimewatch_shutdown_scanners() {
    // DATA GAP: notify:OnCharNowInStation requires production dock RPC guest-list wiring
    // DATA GAP: notify:OnCharNoLongerInStation requires production undock RPC guest-list wiring
    // DATA GAP: notify:OnDamageStateChange requires bubble-scoped combat damage wiring
    // parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnNpcTimerUpdate
    // parity: notify:OnCriminalTimerUpdate, notify:OnDisapprovalTimerUpdate, notify:OnClusterShutdownInitiated
    // DATA GAP: notify:OnCargoScanComplete requires ship scanner module cycle wiring
    // DATA GAP: notify:OnShipScanCompleted requires ship scanner module cycle wiring
    // DATA GAP: notify:OnOreMined requires mining cycle wiring (OF-28)
    eve_testkit::attest(&[
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnNpcTimerUpdate",
        "notify:OnCriminalTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnClusterShutdownInitiated",
    ]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 811, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 812, "other", "other-secret", P2).await;

    // Ensure P2 is docked at Jita 4-4 to receive station guest notifications
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.station_id = Some(JITA_STATION);
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. OnCharNowInStation: Pilot 1 enters station -> Pilot 2 receives guest row
    emit_char_station_notification(&ctx, P1, JITA_STATION, true);
    let ev_now = events(&ctx, P2, "OnCharNowInStation");
    assert_eq!(ev_now.len(), 1, "P2 must receive OnCharNowInStation");
    assert_eq!(ev_now[0].len(), 1);
    if let PyRep::List(ref row) = ev_now[0][0] {
        assert_eq!(row[0], PyRep::Integer(P1));
    } else {
        panic!("expected PyRep::List for guest row");
    }

    // 2. OnCharNoLongerInStation: Pilot 1 leaves station -> Pilot 2 receives undock guest row
    emit_char_station_notification(&ctx, P1, JITA_STATION, false);
    let ev_gone = events(&ctx, P2, "OnCharNoLongerInStation");
    assert_eq!(ev_gone.len(), 1, "P2 must receive OnCharNoLongerInStation");
    assert_eq!(ev_gone[0].len(), 1);
    if let PyRep::List(ref row) = ev_gone[0][0] {
        assert_eq!(row[0], PyRep::Integer(P1));
    } else {
        panic!("expected PyRep::List for guest row");
    }

    // 3. OnDamageStateChange: Emit V24 damage tuple
    emit_damage_state_change(&ctx, P1, 7001, 0.65, 0.85, 1.0);
    let ev_dmg = events(&ctx, P1, "OnDamageStateChange");
    assert_eq!(ev_dmg.len(), 1, "P1 must receive OnDamageStateChange");
    assert_eq!(ev_dmg[0].len(), 2, "OnDamageStateChange expects [shipID, damageTuple]");
    assert_eq!(ev_dmg[0][0], PyRep::Integer(7001));
    if let PyRep::Tuple(ref dt) = ev_dmg[0][1] {
        assert_eq!(dt.len(), 3);
        assert_eq!(dt[1], PyRep::Real(0.85));
        assert_eq!(dt[2], PyRep::Real(1.0));
    } else {
        panic!("expected PyRep::Tuple for damage state");
    }

    // 4. Crimewatch timers: Weapons, PvP, NPC, Criminal, Disapproval
    let now = ctx.now_unix_secs();
    ctx.crimewatch.write().unwrap().states.insert(
        P1,
        eve_server::combat::CharacterCrimeWatchState {
            weapon_timer_until_unix: now + 60,
            pvp_timer_until_unix: now + 300,
            criminal_flag_until_unix: now + 900,
            ..Default::default()
        },
    );
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(P1, now + 30);
        rt.disapproval_flags.insert(P1, now + 120);
    });
    eve_server::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(&ctx, P1, now);

    let ev_weap = events(&ctx, P1, "OnWeaponsTimerUpdate");
    assert_eq!(ev_weap.len(), 1);
    assert_eq!(ev_weap[0][0], PyRep::Integer(101));

    let ev_pvp = events(&ctx, P1, "OnPvpTimerUpdate");
    assert_eq!(ev_pvp.len(), 1);
    assert_eq!(ev_pvp[0][0], PyRep::Integer(201));

    let ev_npc = events(&ctx, P1, "OnNpcTimerUpdate");
    assert_eq!(ev_npc.len(), 1);
    assert_eq!(ev_npc[0][0], PyRep::Integer(401));

    let ev_crim = events(&ctx, P1, "OnCriminalTimerUpdate");
    assert_eq!(ev_crim.len(), 1);
    assert_eq!(ev_crim[0][0], PyRep::Integer(301));

    let ev_dis = events(&ctx, P1, "OnDisapprovalTimerUpdate");
    assert_eq!(ev_dis.len(), 1);
    assert_eq!(ev_dis[0][0], PyRep::Integer(501));

    // 5. OnClusterShutdownInitiated via GM slash /shutdown 120
    s1.role = eve_machonet::service_roles::ROLE_GMH | eve_machonet::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut s1,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/shutdown 120 ScheduledMaintenance".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SlashCmd /shutdown");

    let ev_shut = events(&ctx, P1, "OnClusterShutdownInitiated");
    assert_eq!(ev_shut.len(), 1);
    assert_eq!(ev_shut[0].len(), 2);
    assert_eq!(ev_shut[0][0], PyRep::String("ScheduledMaintenance".into()));

    // 6. OnCargoScanComplete
    emit_cargo_scan_complete(&ctx, P1, 8001, vec![(34, 1000), (35, 500)]);
    let ev_cscan = events(&ctx, P1, "OnCargoScanComplete");
    assert_eq!(ev_cscan.len(), 1);
    assert_eq!(ev_cscan[0].len(), 2);
    assert_eq!(ev_cscan[0][0], PyRep::Integer(8001));

    // 7. OnShipScanCompleted
    emit_ship_scan_completed(&ctx, P1, 8001, 1500.0, 2000.0, vec![(442, 1), (443, 1)]);
    let ev_sscan = events(&ctx, P1, "OnShipScanCompleted");
    assert_eq!(ev_sscan.len(), 1);
    assert_eq!(ev_sscan[0].len(), 4);
    assert_eq!(ev_sscan[0][0], PyRep::Integer(8001));
    assert_eq!(ev_sscan[0][1], PyRep::Real(1500.0));
    assert_eq!(ev_sscan[0][2], PyRep::Real(2000.0));

    // 8. OnOreMined
    emit_ore_mined(&ctx, P1, 1230, 390, 390, 0, 0, 9001, 7002);
    let ev_ore = events(&ctx, P1, "OnOreMined");
    assert_eq!(ev_ore.len(), 1);
    assert_eq!(ev_ore[0].len(), 8);
    assert_eq!(ev_ore[0][0], PyRep::Integer(1230));
    assert_eq!(ev_ore[0][1], PyRep::Integer(390));
}

#[tokio::test]
async fn test_batch5_space_combat_arena_and_ess() {
    // DATA GAP: notify:OnCloakModuleForceDeactivated requires production wiring through combat decloak loop
    // DATA GAP: notify:OnKillNotification requires production wiring through combat death pipeline
    // DATA GAP: notify:OnLocalHackProgressUpdated requires production wiring through hacking loop
    // DATA GAP: notify:OnMatchFound requires production wiring through abyssal matchmaking loop
    // DATA GAP: notify:OnMatchMakingFailure requires production wiring through abyssal matchmaking loop
    // DATA GAP: notify:OnAddExpiryDurationForAbyssalPvpContent requires production wiring through abyssal arena loop
    // DATA GAP: notify:DoDestinyUpdates requires production wiring through destiny 1Hz loop
    // DATA GAP: notify:OnComponentRemovedSystemEnvironment requires production wiring through space environment loop
    // DATA GAP: notify:OnESSReserveBankLinkNotification requires production wiring through ESS link loop
    // DATA GAP: notify:OnESSMainBankLinkNotification requires production wiring through ESS link loop
    // DATA GAP: notify:OnMainBankPlayerLinkDisconnected requires production wiring through ESS disconnect loop
    // DATA GAP: notify:OnCosmicAnomalyAdded requires production wiring through exploration loop

    let ctx = new_ctx();
    let _s1 = pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 822, "other", "other-secret", P2).await;

    // Put P1 in solar system 30000142
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.solar_system_id = JITA_SYSTEM;
        }
    });

    // 1. OnCloakModuleForceDeactivated: Proximity decloak
    emit_cloak_module_force_deactivated(&ctx, P1, "ProximityDecloak", 1950.0);
    let ev_cloak = events(&ctx, P1, "OnCloakModuleForceDeactivated");
    assert_eq!(ev_cloak.len(), 1);
    assert_eq!(ev_cloak[0].len(), 2);
    assert_eq!(ev_cloak[0][0], PyRep::String("ProximityDecloak".into()));
    assert_eq!(
        ev_cloak[0][1],
        PyRep::Dict(vec![(PyRep::String("distance".into()), PyRep::Real(1950.0))])
    );

    // 2. OnKillNotification: Sent to victim and attacker
    emit_kill_notifications(&ctx, P1, P2);
    let ev_kill1 = events(&ctx, P1, "OnKillNotification");
    let ev_kill2 = events(&ctx, P2, "OnKillNotification");
    assert_eq!(ev_kill1.len(), 1);
    assert_eq!(ev_kill2.len(), 1);
    assert_eq!(ev_kill1[0], Vec::<PyRep>::new());
    assert_eq!(ev_kill2[0], Vec::<PyRep>::new());

    // 3. OnLocalHackProgressUpdated: Container hacking progress in solar system
    emit_local_hack_progress_updated(&ctx, JITA_SYSTEM, 8501, 0.45);
    let ev_hack = events(&ctx, P1, "OnLocalHackProgressUpdated");
    assert_eq!(ev_hack.len(), 1);
    assert_eq!(ev_hack[0].len(), 2);
    assert_eq!(ev_hack[0][0], PyRep::Integer(8501));
    assert_eq!(ev_hack[0][1], PyRep::Real(0.45));

    // 4. OnMatchFound & OnAddExpiryDurationForAbyssalPvpContent: Arena match pop
    emit_pvp_match_found_and_expiry(&ctx, P1, 1_750_000_000);
    let ev_found = events(&ctx, P1, "OnMatchFound");
    let ev_exp = events(&ctx, P1, "OnAddExpiryDurationForAbyssalPvpContent");
    assert_eq!(ev_found.len(), 1);
    assert_eq!(ev_found[0], Vec::<PyRep>::new());
    assert_eq!(ev_exp.len(), 1);
    assert_eq!(ev_exp[0].len(), 1);
    let expected_ft = (1_750_000_000i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(ev_exp[0][0], PyRep::Integer(expected_ft));

    // 5. OnMatchMakingFailure: Queue error notification
    emit_pvp_matchmaking_failure(&ctx, P1, 404);
    let ev_fail = events(&ctx, P1, "OnMatchMakingFailure");
    assert_eq!(ev_fail.len(), 1);
    assert_eq!(ev_fail[0].len(), 1);
    assert_eq!(ev_fail[0][0], PyRep::List(vec![PyRep::Integer(404)]));

    // 6. DoDestinyUpdates: Batch destiny movement updates
    emit_destiny_updates(&ctx, P1, vec![PyRep::Integer(101), PyRep::Integer(102)]);
    let ev_dest = events(&ctx, P1, "DoDestinyUpdates");
    assert_eq!(ev_dest.len(), 1);
    assert_eq!(ev_dest[0].len(), 1);
    assert_eq!(ev_dest[0][0], PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)]));

    // 7. OnComponentRemovedSystemEnvironment: Clear environmental hazards
    emit_component_removed_system_environment(&ctx, JITA_SYSTEM);
    let ev_comp = events(&ctx, P1, "OnComponentRemovedSystemEnvironment");
    assert_eq!(ev_comp.len(), 1);
    assert_eq!(ev_comp[0].len(), 1);
    assert_eq!(ev_comp[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    // 8. OnESSReserveBankLinkNotification & OnESSMainBankLinkNotification
    emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToReserveBank", true, 0.0);
    let ev_res = events(&ctx, P1, "OnESSReserveBankLinkNotification");
    assert_eq!(ev_res.len(), 1);
    assert_eq!(ev_res[0].len(), 1);
    assert_eq!(ev_res[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    emit_ess_bank_link_notifications(&ctx, JITA_SYSTEM, P1, "AttemptLinkToMainBank", true, 0.0);
    let ev_main = events(&ctx, P1, "OnESSMainBankLinkNotification");
    assert_eq!(ev_main.len(), 1);
    assert_eq!(ev_main[0].len(), 1);
    assert_eq!(ev_main[0][0], PyRep::Integer(i64::from(JITA_SYSTEM)));

    // 9. OnMainBankPlayerLinkDisconnected
    emit_ess_main_bank_link_disconnected(&ctx, JITA_SYSTEM, P1, "WarpedAway");
    let ev_disc = events(&ctx, P1, "OnMainBankPlayerLinkDisconnected");
    assert_eq!(ev_disc.len(), 1);
    assert_eq!(ev_disc[0].len(), 1);
    if let PyRep::Dict(ref d) = ev_disc[0][0] {
        assert_eq!(d.len(), 4);
        assert!(d.iter().any(|(k, v)| k == &PyRep::String("reason".into()) && v == &PyRep::String("WarpedAway".into())));
    } else {
        panic!("expected PyRep::Dict for disconnect info");
    }

    // 10. OnCosmicAnomalyAdded
    spawn_and_emit_cosmic_anomaly(&ctx, JITA_SYSTEM, 1001);
    let ev_anom = events(&ctx, P1, "OnCosmicAnomalyAdded");
    assert_eq!(ev_anom.len(), 1);
    assert_eq!(ev_anom[0].len(), 1);
    if let PyRep::KeyVal(ref kv) = ev_anom[0][0] {
        assert!(kv.contains_key("targetID"));
        assert_eq!(kv.get("solarSystemID"), Some(&PyRep::Integer(i64::from(JITA_SYSTEM))));
    } else {
        panic!("expected PyRep::KeyVal for anomaly info");
    }
}
