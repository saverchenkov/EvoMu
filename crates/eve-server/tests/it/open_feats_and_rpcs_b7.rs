//! Integration tests for open `feat:` and `rpc:` parity rows (Batch 7, Part 1).
//!
//! Covers:
//! - In-game browser lockdown policy (`browserLockdownSvc`, `feat:ui.browser-lockdown`)
//! - FW LP rewards from complexes & kills scaled by system status + militia LP store spending (`feat:fw.fw-lp`)
//! - Core service `Bind` / `SetSessionCheck` / `clear_memoized` RPCs

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::browser_lockdown::{
    save_browser_lockdown_policy, BrowserLockdownPolicy, FlaggedSiteEntry, DEFAULT_HOME_PAGE,
    SITE_FLAG_TRUSTED_CCP, SITE_FLAG_UNTRUSTED,
};
use glam::DVec3;

use super::common;

const PILOT_1: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const STATE_PROTECTORATE_CORP: i64 = 1_000_180;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

pub(crate) async fn setup_pilot_for_user(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    let uid = sess.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.station_id = Some(STATION_ID);
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYS;
            ch.home_station_id = STATION_ID;
        }
    });
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    sess.solar_system_id = Some(JITA_SYS);
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

pub(crate) async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    setup_pilot_for_user(ctx, char_id as u64, "demo", "demo-secret", char_id).await
}

#[tokio::test]
async fn test_browser_lockdown_policy_and_flagged_sites() {
    // parity: feat:ui.browser-lockdown
    // parity: rpc:browserLockdownSvc.GetDefaultHomePage
    // parity: rpc:browserLockdownSvc.GetFlaggedSitesHash
    // parity: rpc:browserLockdownSvc.GetFlaggedSitesList
    // parity: rpc:browserLockdownSvc.IsBrowserInLockdown
    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(999);
    for method in [
        "IsBrowserInLockdown",
        "GetDefaultHomePage",
        "GetFlaggedSitesList",
        "GetFlaggedSitesHash",
    ] {
        let err = rpc(&ctx, &mut unauth, "browserLockdownSvc", method, &[])
            .await
            .expect_err("unauthenticated browserLockdownSvc call must fail");
        assert_eq!(err.code, "Unauthorized");
    }

    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "IsBrowserInLockdown", &[])
            .await
            .expect("IsBrowserInLockdown"),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "GetDefaultHomePage", &[])
            .await
            .expect("GetDefaultHomePage"),
        PyRep::String(DEFAULT_HOME_PAGE.to_string())
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .await
            .expect("GetFlaggedSitesList"),
        PyRep::List(vec![])
    );
    let default_hash = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
        .await
        .expect("GetFlaggedSitesHash");
    let PyRep::String(ref hash_str) = default_hash else {
        panic!("expected String from GetFlaggedSitesHash");
    };
    assert_eq!(hash_str.len(), 32);

    ctx.db.with_mut_snapshot(|snap| {
        save_browser_lockdown_policy(
            snap,
            &BrowserLockdownPolicy {
                in_lockdown: true,
                default_home_page: "https://community.eveonline.com".to_string(),
                flagged_sites: vec![
                    FlaggedSiteEntry {
                        site_url: "https://phishing.example".to_string(),
                        site_flag: SITE_FLAG_UNTRUSTED,
                    },
                    FlaggedSiteEntry {
                        site_url: "https://secure.eveonline.com".to_string(),
                        site_flag: SITE_FLAG_TRUSTED_CCP,
                    },
                ],
            },
        );
    });

    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "IsBrowserInLockdown", &[])
            .await
            .expect("IsBrowserInLockdown after update"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "GetDefaultHomePage", &[])
            .await
            .expect("GetDefaultHomePage after update"),
        PyRep::String("https://community.eveonline.com".to_string())
    );

    let expected_sites = PyRep::List(vec![
        PyRep::Dict(vec![
            (
                PyRep::String("siteUrl".into()),
                PyRep::String("https://phishing.example".into()),
            ),
            (
                PyRep::String("siteFlag".into()),
                PyRep::Integer(SITE_FLAG_UNTRUSTED),
            ),
        ]),
        PyRep::Dict(vec![
            (
                PyRep::String("siteUrl".into()),
                PyRep::String("https://secure.eveonline.com".into()),
            ),
            (
                PyRep::String("siteFlag".into()),
                PyRep::Integer(SITE_FLAG_TRUSTED_CCP),
            ),
        ]),
    ]);
    assert_eq!(
        rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .await
            .expect("GetFlaggedSitesList after update"),
        expected_sites
    );
    let hash_after = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
        .await
        .expect("GetFlaggedSitesHash after update");
    assert_ne!(hash_after, default_hash);

    // Reload from SQLite and verify lockdown policy + flagged site dicts + hash survive restart
    let ctx_re =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_browser_lockdown");
    let mut sess_re = setup_pilot(&ctx_re, PILOT_1).await;
    assert_eq!(
        rpc(&ctx_re, &mut sess_re, "browserLockdownSvc", "IsBrowserInLockdown", &[])
            .await
            .expect("IsBrowserInLockdown reload"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx_re, &mut sess_re, "browserLockdownSvc", "GetFlaggedSitesList", &[])
            .await
            .expect("GetFlaggedSitesList reload"),
        expected_sites
    );
    assert_eq!(
        rpc(&ctx_re, &mut sess_re, "browserLockdownSvc", "GetFlaggedSitesHash", &[])
            .await
            .expect("GetFlaggedSitesHash reload"),
        hash_after
    );
}

#[tokio::test]
async fn test_fw_lp_complexes_kills_status_scaling_and_militia_store() {
    // parity: feat:fw.fw-lp
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, cmd_sys, rear_sys) =
        crate::fw_warzone_insurgency_closure::setup_warzone_chain(&ctx);

    let mut sess_cal = setup_pilot(&ctx, PILOT_1).await;
    let cal_ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).expect("PILOT_1");
        ch.war_faction_id = Some(500_001); // Caldari militia -> State Protectorate (1000180)
        ch.loyalty_points.remove(&STATE_PROTECTORATE_CORP);
        ch.balance = IskCents::from_isk(50_000_000);
        snap.fw_runtime.direct_enlisted_chars.insert(PILOT_1, 500_001);
        ch.active_ship_id
    });
    sess_cal.war_faction_id = Some(500_001);

    // 1. Live Novice FW complex captures via keeper.ActivateAccelerationGate across Frontline (1.0x), Command (0.75x), Rearguard (0.50x)
    let mut complex_payouts = Vec::new();
    for sys_id in [front_sys, cmd_sys, rear_sys] {
        let lp_before = ctx.db.with_snapshot(|s| {
            s.characters[&PILOT_1]
                .loyalty_points
                .get(&STATE_PROTECTORATE_CORP)
                .copied()
                .unwrap_or(0)
        });
        sess_cal.solar_system_id = Some(sys_id);
        sess_cal.station_id = None;
        sess_cal.structure_id = None;
        sess_cal.ship_id = Some(cal_ship_id);
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&PILOT_1).unwrap();
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            if let Some(sh) = s.items.get_mut(&cal_ship_id) {
                sh.type_id = 587; // Rifter (T1 frigate allowed in Novice)
                sh.location_id = i64::from(sys_id);
            }
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h
            .spawn_ball(crate::sovereignty_equinox_gameplay_closure::make_ball(
                cal_ship_id,
                587,
                DVec3::ZERO,
            ))
            .await;

        // First cluster tick spawns warzone Novice/Small/Medium/Large complexes for sys_id
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let (novice_gate, gate_pos) = ctx.db.with_snapshot(|s| {
            let (&gid, _) = s
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, tier, _, _, done))| *csys == sys_id && tier == "Novice" && !*done)
                .expect("spawned Novice FW complex");
            let pos = s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0]);
            (gid, DVec3::new(pos[0], pos[1], pos[2]))
        });
        let _ = sys_h
            .spawn_ball(crate::sovereignty_equinox_gameplay_closure::make_ball(
                cal_ship_id,
                587,
                gate_pos + DVec3::new(1_000.0, 0.0, 0.0),
            ))
            .await;

        rpc(
            &ctx,
            &mut sess_cal,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(novice_gate)],
        )
        .await
        .expect("ActivateAccelerationGate into Novice pocket");

        let guard_id = novice_gate + 500_000_000;
        let _ = sys_h.remove_ball(guard_id).await;
        ctx.db.with_mut_snapshot(|s| {
            s.fw_runtime.complex_guards_cleared.insert(novice_gate, true);
        });
        ctx.advance_secs(900);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let lp_after = ctx.db.with_snapshot(|s| {
            s.characters[&PILOT_1]
                .loyalty_points
                .get(&STATE_PROTECTORATE_CORP)
                .copied()
                .unwrap_or(0)
        });
        complex_payouts.push(lp_after - lp_before);
    }
    assert_eq!(
        complex_payouts,
        vec![10_000, 7_500, 5_000],
        "Novice complex LP must scale 1.0x Frontline (10000), 0.75x Command (7500), 0.50x Rearguard (5000)"
    );

    // 2. Enemy militia kill LP scaled by system frontline status (1.0x / 0.75x / 0.50x)
    let victim_isk = 40_000_000.0; // base_lp = 4000
    let kill_front =
        eve_server::services::fw_services::award_fw_kill_lp(&ctx, PILOT_1, front_sys, victim_isk);
    let kill_cmd =
        eve_server::services::fw_services::award_fw_kill_lp(&ctx, PILOT_1, cmd_sys, victim_isk);
    let kill_rear =
        eve_server::services::fw_services::award_fw_kill_lp(&ctx, PILOT_1, rear_sys, victim_isk);
    assert_eq!(
        (kill_front, kill_cmd, kill_rear),
        (4_000, 3_000, 2_000),
        "FW PvP kill LP must scale 1.0x Frontline (4000), 0.75x Command (3000), 0.50x Rearguard (2000)"
    );

    // 3. Spend earned militia LP in State Protectorate (1000180) LP store
    common::seed_test_lp_store_offers(&ctx);
    let militia_station = ctx
        .sde
        .stations
        .values()
        .find(|st| st.owner_corporation_id == STATE_PROTECTORATE_CORP)
        .map(|st| st.station_id)
        .unwrap_or(STATION_ID);
    sess_cal.station_id = Some(militia_station);
    sess_cal.structure_id = None;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_1).unwrap();
        ch.station_id = Some(militia_station);
        ch.structure_id = None;
    });

    let total_earned_lp = ctx.db.with_snapshot(|s| {
        s.characters[&PILOT_1]
            .loyalty_points
            .get(&STATE_PROTECTORATE_CORP)
            .copied()
            .unwrap_or(0)
    });
    assert_eq!(total_earned_lp, 31_500);

    let offers_rep = rpc(
        &ctx,
        &mut sess_cal,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(STATE_PROTECTORATE_CORP)],
    )
    .await
    .expect("GetAvailableOffersFromCorp militia");
    let PyRep::List(ref offers) = offers_rep else {
        panic!("expected List from militia LP store");
    };
    assert!(
        offers
            .iter()
            .any(|o| o.dict_get("offerID").and_then(|v| v.as_i64()) == Some(7002)),
        "State Protectorate LP store must expose offer 7002"
    );

    // Negative check: asking for excessive quantity exceeding earned LP fails
    assert!(
        rpc(
            &ctx,
            &mut sess_cal,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(STATE_PROTECTORATE_CORP),
                PyRep::Integer(7002),
                PyRep::Integer(1_000),
            ],
        )
        .await
        .is_err(),
        "spending more militia LP than earned must fail"
    );

    let isk_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
    rpc(
        &ctx,
        &mut sess_cal,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(STATE_PROTECTORATE_CORP),
            PyRep::Integer(7002),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter spending earned FW militia LP");

    let (lp_remaining, isk_after) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&PILOT_1];
        (
            ch.loyalty_points
                .get(&STATE_PROTECTORATE_CORP)
                .copied()
                .unwrap_or(0),
            ch.balance,
        )
    });
    assert!(lp_remaining < total_earned_lp, "militia LP must be deducted");
    assert!(isk_after < isk_before, "militia offer ISK cost must be deducted");

    // Verify SQLite persistence of remaining militia LP balance
    let ctx_re =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_fw_lp_store");
    let reloaded_lp = ctx_re.db.with_snapshot(|s| {
        s.characters[&PILOT_1]
            .loyalty_points
            .get(&STATE_PROTECTORATE_CORP)
            .copied()
            .unwrap_or(0)
    });
    assert_eq!(reloaded_lp, lp_remaining);
}

#[tokio::test]
async fn test_core_service_bind_session_check_and_clear_memoized() {
    // parity: rpc:allianceRegistry.Bind
    // parity: rpc:allianceRegistry.SetSessionCheck
    // parity: rpc:warRegistry.SetSessionCheck
    // parity: rpc:ship.SetSessionCheck
    // parity: rpc:ship.clear_memoized
    // parity: rpc:corpRegistry.SetSessionCheck
    // parity: rpc:corpRegistry.clear_memoized
    // parity: rpc:charMgr.SetSessionCheck
    // parity: rpc:charMgr.clear_memoized
    // parity: rpc:reprocessingSvc.SetSessionCheck
    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(996);
    for (svc, meth) in [
        ("allianceRegistry", "Bind"),
        ("allianceRegistry", "SetSessionCheck"),
        ("warRegistry", "SetSessionCheck"),
        ("ship", "SetSessionCheck"),
        ("ship", "clear_memoized"),
        ("corpRegistry", "SetSessionCheck"),
        ("corpRegistry", "clear_memoized"),
        ("charMgr", "SetSessionCheck"),
        ("charMgr", "clear_memoized"),
        ("reprocessingSvc", "SetSessionCheck"),
    ] {
        assert!(
            rpc(&ctx, &mut unauth, svc, meth, &[]).await.is_err(),
            "unauthenticated {svc}.{meth} must fail"
        );
    }

    let mut sess = setup_pilot(&ctx, PILOT_1).await;

    let r1 = rpc(&ctx, &mut sess, "allianceRegistry", "Bind", &[PyRep::Integer(99_000_001)])
        .await
        .expect("allianceRegistry.Bind");
    assert_eq!(r1, PyRep::None);
    assert!(ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .any(|b| b.service == "allianceRegistry" && b.object_id == 99_000_001));

    assert_eq!(
        rpc(&ctx, &mut sess, "allianceRegistry", "SetSessionCheck", &[])
            .await
            .expect("allianceRegistry.SetSessionCheck"),
        PyRep::None
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "warRegistry", "SetSessionCheck", &[])
            .await
            .expect("warRegistry.SetSessionCheck"),
        PyRep::None
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", "SetSessionCheck", &[])
            .await
            .expect("ship.SetSessionCheck"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", "clear_memoized", &[])
            .await
            .expect("ship.clear_memoized"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "SetSessionCheck", &[])
            .await
            .expect("corpRegistry.SetSessionCheck"),
        PyRep::None
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "clear_memoized", &[])
            .await
            .expect("corpRegistry.clear_memoized"),
        PyRep::None
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "charMgr", "SetSessionCheck", &[])
            .await
            .expect("charMgr.SetSessionCheck"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "charMgr", "clear_memoized", &[])
            .await
            .expect("charMgr.clear_memoized"),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "reprocessingSvc", "SetSessionCheck", &[])
            .await
            .expect("reprocessingSvc.SetSessionCheck"),
        PyRep::Bool(true)
    );
}
