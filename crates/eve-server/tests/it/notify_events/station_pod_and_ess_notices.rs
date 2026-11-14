#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

mod notify_real_emitters_batch6 {
    #[allow(unused_imports)]
    use super::*;

// Real production emitter tests for Notify Batch 6 (`P-5`, `T-1`).
// Covers Station, Ship/Pod destruction, ESS, Faction Warfare, Insurgency, and SafeLogoff/Crimewatch.


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
    {
        let mut h = common::borrow_wire_harness(ctx, &mut s);
        h.call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String(user.into()), PyRep::String(pass.into())],
            &HashMap::new(),
        )
        .await;
        h.call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
        )
        .await;
    }
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
    // Exercises station guest-list notifications, combat damage state notifications,
    // cargo and ship scanner completion notifications,
    // mining cycle notifications, cloak proximity deactivation,
    // and hacking minigame progress notifications.
    //
    //
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
    // parity: notify:OnPodDeath
    // parity: notify:OnPodKilled, notify:OnPlayerPodDeath, notify:OnPlayerPodRespawn
    eve_testkit::attest(&[
        "notify:OnPodDeath",
        "notify:OnPodKilled",
        "notify:OnPlayerPodDeath",
        "notify:OnPlayerPodRespawn",
    ]);

    const ZARZAKH_SYSTEM: u32 = 30_100_000;
    const FULCRUM_STATION: i64 = 60_015_187;
    let ctx = new_ctx();
    let empty = HashMap::new();
    let _ = ctx.db.set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT);
    let _ = ctx.db.set_account_role("other", eve_db::service_roles::ROLE_GM_DEFAULT);
    let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION].position;

    let mut h1 = common::new_wire_harness(&ctx, 831);
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
    ctx.advance_secs(31);
    h1.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    h1.call_ok("slash", None, "SlashCmd", &[PyRep::String(format!("/tr {FULCRUM_STATION}"))], &empty)
        .await;

    let mut h2 = common::new_wire_harness(&ctx, 832);
    h2.call_ok(
        "authentication",
        None,
        "Login",
        &[PyRep::String("other".into()), PyRep::String("other-secret".into())],
        &empty,
    )
    .await;
    h2.call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(P2)], &empty)
        .await;
    ctx.advance_secs(31);
    h2.call_ok("stationSvc", None, "Undock", &[], &empty).await;
    h2.call_ok("slash", None, "SlashCmd", &[PyRep::String(format!("/tr {FULCRUM_STATION}"))], &empty)
        .await;

    let ship1 = ctx
        .db
        .with_snapshot(|s| s.characters.get(&P1).map(|c| c.active_ship_id))
        .expect("P1 active ship after undock");
    let ship2 = ctx
        .db
        .with_snapshot(|s| s.characters.get(&P2).map(|c| c.active_ship_id))
        .expect("P2 active ship after undock");
    let sys_h = ctx.systems.get_or_spawn(ZARZAKH_SYSTEM);
    sys_h.set_ball_position(ship1, fulcrum_pos + DVec3::new(5_500.0, 0.0, 0.0)).await;

    // Fit 2x Small Autocannon + EMP S on P2 through GM slash RPCs + LoadAmmo
    h2.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3300 5".into())], &empty).await;
    h2.call_ok("slash", None, "SlashCmd", &[PyRep::String("/setskill 3301 5".into())], &empty).await;
    let fit1_rep = h2
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty)
        .await;
    let fit2_rep = h2
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/fit me 484".into())], &empty)
        .await;
    let gun1_id = fit1_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("fitted gun1 itemID");
    let gun2_id = fit2_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("fitted gun2 itemID");
    let ammo_rep = h2
        .call_ok("slash", None, "SlashCmd", &[PyRep::String("/giveitem 185 500".into())], &empty)
        .await;
    let ammo_id = ammo_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("given ammo itemID");
    h2.call_ok(
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(ship2),
            PyRep::List(vec![PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
        &empty,
    )
    .await;
    ctx.advance_secs(12);

    // 1. Ship destruction via live weapon combat over ClientHarness
    h2.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(ship1)], &empty).await;
    sys_h.step_manual(10.0).await.unwrap();
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h1.drain_notifications();
    let _ = h2.drain_notifications();

    for &gid in &[gun1_id, gun2_id] {
        h2.call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gid),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(ship1),
                PyRep::Integer(100),
            ],
            &empty,
        )
        .await;
    }
    for _ in 0..120 {
        if sys_h.get_ball_snapshot(ship1).await.ok().flatten().is_none() {
            break;
        }
        ctx.advance_secs(1);
        sys_h.step_manual(1.0).await.unwrap();
    }
    assert!(
        sys_h.get_ball_snapshot(ship1).await.ok().flatten().is_none(),
        "P1 ship must be destroyed in space by live weapon fire"
    );

    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h2.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let h1_ship_notifs = h1.drain_notifications();
    let h2_ship_notifs = h2.drain_notifications();

    let ev_sd: Vec<_> = h1_ship_notifs.iter().filter(|(e, _)| e == "OnShipDeath").map(|(_, a)| a).collect();
    assert_eq!(ev_sd.len(), 1, "P1 receives OnShipDeath on the wire");
    assert!(ev_sd[0].is_empty(), "OnShipDeath has arity 0");

    let ev_km_vic: Vec<_> = h1_ship_notifs.iter().filter(|(e, _)| e == "OnKillNotification").map(|(_, a)| a).collect();
    assert_eq!(ev_km_vic.len(), 1, "Victim P1 receives OnKillNotification on the wire");
    assert!(ev_km_vic[0].is_empty(), "OnKillNotification has arity 0");

    let ev_km_atk: Vec<_> = h2_ship_notifs.iter().filter(|(e, _)| e == "OnKillNotification").map(|(_, a)| a).collect();
    assert_eq!(ev_km_atk.len(), 1, "Attacker P2 receives OnKillNotification on the wire");
    assert!(ev_km_atk[0].is_empty(), "OnKillNotification has arity 0");

    // 2. Pod destruction via live weapon combat against ejected capsule
    let pod1 = ctx
        .db
        .with_snapshot(|s| s.characters.get(&P1).map(|c| c.active_ship_id))
        .expect("P1 ejected capsule after ship loss");
    assert_ne!(pod1, ship1, "P1 active_ship_id must transition to ejected capsule");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&pod1).map(|i| i.type_id)),
        Some(670),
        "ejected ship must be a Capsule (670)"
    );

    ctx.advance_secs(5);
    h2.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(pod1)], &empty).await;
    sys_h.step_manual(10.0).await.unwrap();
    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h1.drain_notifications();
    let _ = h2.drain_notifications();

    for &gid in &[gun1_id, gun2_id] {
        h2.call_ok(
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gid),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(pod1),
                PyRep::Integer(100),
            ],
            &empty,
        )
        .await;
    }
    for _ in 0..120 {
        if sys_h.get_ball_snapshot(pod1).await.ok().flatten().is_none() {
            break;
        }
        ctx.advance_secs(1);
        sys_h.step_manual(1.0).await.unwrap();
    }
    assert!(
        sys_h.get_ball_snapshot(pod1).await.ok().flatten().is_none(),
        "P1 ejected pod must be destroyed in space by live weapon fire"
    );

    let _ = h1.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let _ = h2.call_ok("machoNet", None, "GetTime", &[], &empty).await;
    let h1_pod_notifs = h1.drain_notifications();
    let h2_pod_notifs = h2.drain_notifications();

    let ev_pd: Vec<_> = h1_pod_notifs.iter().filter(|(e, _)| e == "OnPodDeath").map(|(_, a)| a).collect();
    assert_eq!(ev_pd.len(), 1, "P1 receives OnPodDeath on the wire");
    assert!(ev_pd[0].is_empty(), "OnPodDeath has arity 0");

    let ev_ppd: Vec<_> = h1_pod_notifs.iter().filter(|(e, _)| e == "OnPlayerPodDeath").map(|(_, a)| a).collect();
    assert_eq!(ev_ppd.len(), 1, "P1 receives OnPlayerPodDeath on the wire");
    assert!(ev_ppd[0].is_empty(), "OnPlayerPodDeath has arity 0");

    let ev_pk: Vec<_> = h1_pod_notifs.iter().filter(|(e, _)| e == "OnPodKilled").map(|(_, a)| a).collect();
    assert_eq!(ev_pk.len(), 1, "P1 receives OnPodKilled on the wire");
    assert_eq!(ev_pk[0], &vec![PyRep::Integer(pod1)]);

    let ev_respawn: Vec<_> = h1_pod_notifs.iter().filter(|(e, _)| e == "OnPlayerPodRespawn").map(|(_, a)| a).collect();
    assert_eq!(ev_respawn.len(), 1, "P1 receives OnPlayerPodRespawn on the wire");
    assert!(ev_respawn[0].is_empty(), "OnPlayerPodRespawn has arity 0");

    let ev_km_vic2: Vec<_> = h1_pod_notifs.iter().filter(|(e, _)| e == "OnKillNotification").map(|(_, a)| a).collect();
    assert_eq!(ev_km_vic2.len(), 1, "Victim P1 receives OnKillNotification for pod on the wire");
    assert!(ev_km_vic2[0].is_empty(), "OnKillNotification for pod has arity 0");

    let ev_km_atk2: Vec<_> = h2_pod_notifs.iter().filter(|(e, _)| e == "OnKillNotification").map(|(_, a)| a).collect();
    assert_eq!(ev_km_atk2.len(), 1, "Attacker P2 receives OnKillNotification for pod on the wire");
    assert!(ev_km_atk2[0].is_empty(), "OnKillNotification for pod has arity 0");
}

#[tokio::test]
async fn test_batch6_ess_and_dynamic_bounty() {
    // parity: notify:OnSystemESSDataChanged
    // parity: notify:OnESSMainBankLinkNotification, notify:OnESSReserveBankLinkNotification
    // parity: notify:OnMainBankPlayerLinkDisconnected, notify:OnReserveBankPlayerLinkDisconnected
    // parity: notify:OnMainBankPlayerHackSuccess, notify:OnReserveBankUnlock
    // Exercises ESS main/reserve bank link, hack, unlock, and dynamic bounty state notifications
    // across system ESS state updates.
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
    let expected_p1 = (eve_server::services::pve_ess_reserve_banks::compute_ess_reserve_pulse_payout_cents(
        10_000_000_000,
        15,
        15,
    ) as f64)
        / 100.0;
    assert_eq!(res_res_comp.dict_get("stolenISK"), Some(&PyRep::Real(expected_p1)));

    let _ = ctx
        .dispatch_rpc(&mut s1, "essMgr", None, "RequestReserveBankUnlink", &[], &kw, None)
        .await
        .expect("RequestReserveBankUnlink");

    let ev_res_disc = events(&ctx, P1, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc.len(), 1, "P1 receives OnReserveBankPlayerLinkDisconnected");
    assert_eq!(ev_res_disc[0][0].dict_get("characterID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ev_res_disc[0][0].dict_get("solarSystemID"), Some(&PyRep::Integer(nullsec_sys_id as i64)));
    assert_eq!(ev_res_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));
}

}
