#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub(crate) mod parity_notify_flagged_remediation {
    #[allow(unused_imports)]
    use super::*;

// End-to-end integration tests for remediated flagged notify rows:
// - `notify:DoSimClockRebase`
// - `notify:OnAnalysisKreditsChange`
// - `notify:OnCampaignEnrollmentChanged`
// - `notify:OnCaptureChanged`
// - `notify:OnCharacterPermanentlyBanned`


use crate::common;
use eve_db::service_roles;
use eve_destiny::{Ball, DVec3, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::quasar::{EvePublicRequest, GoogleAny};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::get_system_frontline_rules;
use eve_server::services::project_discovery::spend_analysis_kredits;
use eve_server::services::serverops_cluster::set_system_tidi_factor;
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30_000_142;
const AMARR_SYSTEM: u32 = 30_002_187;

fn new_ctx() -> Arc<ServerContext> {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_events(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let _ = sess.drain_notifications();
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn chat_bcast_req(msg: &str) -> EvePublicRequest {
    let mut payload = vec![0x0A, msg.len() as u8];
    payload.extend_from_slice(msg.as_bytes());
    EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.chat.local.api.BroadcastMessageRequest".into(),
            value: payload,
        }),
        ..Default::default()
    }
}

// parity: notify:DoSimClockRebase
#[tokio::test]
async fn test_parity_notify_sim_clock_rebase_and_analysis_kredits() {
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
    ]);

    let ctx = new_ctx();

    // 1. Setup PILOT_A in Jita and PILOT_B in Amarr
    let mut s1 = SessionState::new(92_051);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("select PILOT_A");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_A) {
            c.solar_system_id = JITA_SYSTEM;
            c.station_id = None;
            c.structure_id = None;
        }
    });

    let mut s2 = SessionState::new(92_052);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_B) {
            c.solar_system_id = AMARR_SYSTEM;
            c.station_id = None;
            c.structure_id = None;
        }
    });

    let _ = drain_events(&ctx, &mut s1, PILOT_A);
    let _ = drain_events(&ctx, &mut s2, PILOT_B);

    // --- Part A: DoSimClockRebase ---
    // A1: Time Dilation adjustment on Jita emits DoSimClockRebase ONLY to Jita
    set_system_tidi_factor(&ctx, JITA_SYSTEM, 0.50);

    let jita_rebase: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "DoSimClockRebase")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(jita_rebase.len(), 1, "PILOT_A in Jita must receive DoSimClockRebase");
    assert_eq!(jita_rebase[0].len(), 1, "DoSimClockRebase takes 1 tuple argument");
    match &jita_rebase[0][0] {
        PyRep::Tuple(times) => {
            assert_eq!(times.len(), 2, "DoSimClockRebase takes 2-tuple (oldSimTime, newSimTime)");
            let old_t = times[0].as_i64().expect("oldSimTime");
            let new_t = times[1].as_i64().expect("newSimTime");
            assert!(new_t <= old_t, "TiDi DoSimClockRebase must never push client timers forward: {old_t} -> {new_t}");
        }
        other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
    }

    // Negative check: PILOT_B in Amarr must NOT receive Jita's DoSimClockRebase
    assert!(
        !drain_events(&ctx, &mut s2, PILOT_B)
            .iter()
            .any(|(e, _)| e == "DoSimClockRebase"),
        "PILOT_B in Amarr must not receive Jita's DoSimClockRebase"
    );

    // A2: Server clock adjustment emits DoSimClockRebase scoped to active systems
    let _ = drain_events(&ctx, &mut s1, PILOT_A);
    let before_ft = ctx.now_blueos_filetime_ticks();
    ctx.advance_secs(20);

    let advance_rebase: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "DoSimClockRebase")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(advance_rebase.len(), 1, "advance_secs must emit DoSimClockRebase");
    match &advance_rebase[0][0] {
        PyRep::Tuple(times) => {
            let old_t = times[0].as_i64().expect("oldSimTime");
            let new_t = times[1].as_i64().expect("newSimTime");
            assert_eq!(old_t / 10_000_000, before_ft / 10_000_000);
            assert_eq!((new_t - old_t) / 10_000_000, 20);
        }
        other => panic!("expected 2-tuple rebaseTimes, got {other:?}"),
    }

    // Negative check: 0s advance does not emit DoSimClockRebase
    ctx.advance_secs(0);
    assert!(
        !drain_events(&ctx, &mut s1, PILOT_A)
            .iter()
            .any(|(e, _)| e == "DoSimClockRebase"),
        "0-second advance must not emit DoSimClockRebase"
    );

    // Regression (#531/#556): timeControl.AdvanceClock advances server time without emitting
    // DoSimClockRebase so live EVE.app _destiny.Ballpark does not stall via AdjustTimes.
    s1.role |= eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let before_tc_unix = ctx.now_unix();
    let tc_res = rpc(
        &ctx,
        &mut s1,
        "timeControl",
        "AdvanceClock",
        &[PyRep::Integer(15)],
    )
    .await
    .expect("timeControl.AdvanceClock");
    assert_eq!(tc_res, PyRep::Bool(true));
    assert!(ctx.now_unix() >= before_tc_unix + 15);
    assert!(
        !drain_events(&ctx, &mut s1, PILOT_A)
            .iter()
            .any(|(e, _)| e == "DoSimClockRebase"),
        "timeControl.AdvanceClock must not emit DoSimClockRebase (#531)"
    );

    // --- Part B: OnAnalysisKreditsChange ---
    // B1: Negative check on invalid classification
    let err = rpc(
        &ctx,
        &mut s1,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::String("".into())],
    )
    .await
    .expect_err("empty classification must fail");
    assert_eq!(err.code, "InvalidClassification");
    assert!(
        !drain_events(&ctx, &mut s1, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnAnalysisKreditsChange"),
        "failed classification must not emit OnAnalysisKreditsChange"
    );

    // B2: Positive check: real task fetch and classification submission grants AK
    let task = rpc(&ctx, &mut s1, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = task
        .dict_get("task_id")
        .and_then(|v| v.as_str())
        .expect("task_id")
        .to_string();

    let post_rep = rpc(
        &ctx,
        &mut s1,
        "ProjectDiscovery",
        "post_classification",
        &[
            PyRep::String(task_id),
            PyRep::List(vec![PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Real(0.52), PyRep::Real(0.24)]),
                PyRep::Tuple(vec![PyRep::Real(0.76), PyRep::Real(0.24)]),
                PyRep::Tuple(vec![PyRep::Real(0.76), PyRep::Real(0.48)]),
                PyRep::Tuple(vec![PyRep::Real(0.52), PyRep::Real(0.48)]),
            ])]),
        ],
    )
    .await
    .expect("post_classification");
    let ak_reward = post_rep
        .dict_get("AK_Reward")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(ak_reward > 0, "post_classification must grant AK reward > 0");

    let ak_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnAnalysisKreditsChange")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(ak_notifs.len(), 1, "must emit 1 OnAnalysisKreditsChange");
    assert_eq!(ak_notifs[0].len(), 0, "OnAnalysisKreditsChange takes 0 arguments");

    // B3: Spending AK debits balance and emits OnAnalysisKreditsChange
    let spend_res = spend_analysis_kredits(&ctx, PILOT_A, 5).expect("spend AK");
    assert_eq!(spend_res, ak_reward - 5);

    let spend_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnAnalysisKreditsChange")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(spend_notifs.len(), 1, "spend_analysis_kredits must emit OnAnalysisKreditsChange");
    assert_eq!(spend_notifs[0].len(), 0, "OnAnalysisKreditsChange takes 0 arguments");
}

#[tokio::test]
async fn test_parity_notify_campaign_enrollment_and_chat_bans() {
    eve_testkit::attest(&[]);

    let ctx = new_ctx();

    let mut gm = SessionState::new(92_053);
    common::login_pilot(&ctx, &mut gm, "demo", "demo-secret").await;
    gm.role = service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("select PILOT_A");
    gm.role = service_roles::ROLE_GM_DEFAULT;

    let mut p2 = SessionState::new(92_054);
    common::login_pilot(&ctx, &mut p2, "other", "other-secret").await;
    rpc(&ctx, &mut p2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");

    let _ = drain_events(&ctx, &mut gm, PILOT_A);
    let _ = drain_events(&ctx, &mut p2, PILOT_B);

    // --- Part A: OnCampaignEnrollmentChanged ---
    // A1: FW campaign enlistment via facWarMgr.JoinFactionAsCharacter
    let fw_res = rpc(
        &ctx,
        &mut p2,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(500_001)], // Caldari State
    )
    .await
    .expect("JoinFactionAsCharacter");
    assert_eq!(fw_res.dict_get("enlisted"), Some(&PyRep::Bool(true)));

    let fw_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCampaignEnrollmentChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(fw_notifs.len(), 1, "PILOT_B must receive OnCampaignEnrollmentChanged on FW join");
    assert_eq!(fw_notifs[0].len(), 0, "OnCampaignEnrollmentChanged expects 0 args");

    // Negative check: PILOT_A must NOT receive FW enrollment notification
    assert!(
        !drain_events(&ctx, &mut gm, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnCampaignEnrollmentChanged"),
        "PILOT_A must not receive FW enrollment notification"
    );

    // A2: Login campaign enrollment transition via slash command (ClientHarness, per-char isolation)
    {
        let empty = HashMap::new();
        let mut h_gm = crate::common::borrow_wire_harness(&ctx, &mut gm);
        let mut h_p2 = crate::common::borrow_wire_harness(&ctx, &mut p2);

        let p2_before = h_p2
            .call_ok("loginCampaignManager", None, "get_client_campaign_state", &[], &empty)
            .await;
        let p2_before_cid = p2_before
            .dict_get("static_data")
            .and_then(|d| d.dict_get("campaign_id"))
            .and_then(|v| v.as_i64());
        assert!(p2_before_cid.is_some(), "PILOT_B must have an initial campaign state");
        let _ = h_gm.drain_notifications();
        let _ = h_p2.drain_notifications();

        let sde_camps = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde);
        let alt_campaign = sde_camps
            .keys()
            .copied()
            .find(|&id| id != 2261)
            .expect("alternate login campaign");

        // Negative check: invalid campaign ID fails and does not emit
        let err_camp = h_gm
            .call("slash", None, "SlashCmd", &[PyRep::String("/campaign enroll 88888888".into())], &empty)
            .await
            .expect_err("non-existent campaign must fail");
        assert_eq!(err_camp.code, "CampaignNotFound");
        assert!(
            !h_gm.drain_notifications().iter().any(|(e, _)| e == "OnCampaignEnrollmentChanged"),
            "failed /campaign enroll must not emit OnCampaignEnrollmentChanged"
        );

        h_gm
            .call_ok(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(format!("/campaign enroll {alt_campaign} {PILOT_A}"))],
                &empty,
            )
            .await;

        let login_notifs: Vec<Vec<PyRep>> = h_gm
            .drain_notifications()
            .into_iter()
            .filter(|(e, _)| e == "OnCampaignEnrollmentChanged")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(login_notifs.len(), 1, "PILOT_A must receive OnCampaignEnrollmentChanged on login campaign enroll");
        assert_eq!(login_notifs[0].len(), 0, "OnCampaignEnrollmentChanged expects 0 args");

        let _ = h_p2.call_ok("machoNet", None, "GetTime", &[], &empty).await;
        assert!(
            !h_p2.drain_notifications().iter().any(|(e, _)| e == "OnCampaignEnrollmentChanged"),
            "PILOT_B must not receive OnCampaignEnrollmentChanged when PILOT_A enrolls"
        );

        let p1_after = h_gm
            .call_ok("loginCampaignManager", None, "get_client_campaign_state", &[], &empty)
            .await;
        let p2_after = h_p2
            .call_ok("loginCampaignManager", None, "get_client_campaign_state", &[], &empty)
            .await;
        assert_eq!(
            p1_after.dict_get("static_data").and_then(|d| d.dict_get("campaign_id")),
            Some(&PyRep::Integer(alt_campaign))
        );
        assert_eq!(
            p2_after.dict_get("static_data").and_then(|d| d.dict_get("campaign_id")),
            p2_before_cid.map(PyRep::Integer).as_ref(),
            "enrolling PILOT_A must not mutate PILOT_B's campaign state"
        );
    }

    // --- Part B: OnCharacterPermanentlyBanned ---
    // B1: Enforce permanent chat ban on PILOT_B
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/chatban {PILOT_B} perm"))],
    )
    .await
    .expect("/chatban perm");

    let perm_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCharacterPermanentlyBanned")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(perm_notifs.len(), 1, "PILOT_B must receive OnCharacterPermanentlyBanned");
    assert_eq!(perm_notifs[0].len(), 0, "OnCharacterPermanentlyBanned takes 0 args");

    // B2: Enforcement verification: MachoNet LSC.SendMessage blocked
    let lsc_err = rpc(
        &ctx,
        &mut p2,
        "LSC",
        "SendMessage",
        &[PyRep::String("solarsystemid2:30000142".into()), PyRep::String("test msg".into())],
    )
    .await
    .expect_err("LSC.SendMessage must be blocked for permanently banned pilot");
    assert_eq!(lsc_err.code, "CharacterChatBanned");

    // B3: Enforcement verification: Quasar gateway broadcast blocked
    let q_res = ctx.quasar_gateway.handle_public_request(
        &p2,
        &chat_bcast_req("banned broadcast"),
        None,
        false,
        ctx.now_unix(),
    );
    assert_ne!(q_res.status_code, STATUS_OK, "Quasar chat broadcast must be blocked");

    // B4: Unban restores messaging
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/chatunban {PILOT_B}"))],
    )
    .await
    .expect("/chatunban");

    let unban_notifs = drain_events(&ctx, &mut p2, PILOT_B);
    assert!(unban_notifs.iter().any(|(e, _)| e == "OnCharacterUnbanned"));
}

#[tokio::test]
async fn test_parity_notify_fw_capture_changed() {
    let ctx = new_ctx();

    // 1. Find frontline FW system
    let (fw_sys, occ_fac) = ctx
        .db
        .with_snapshot(|s| {
            s.fw_system_occupations
                .iter()
                .find(|(&sys_id, occ)| {
                    get_system_frontline_rules(&ctx, sys_id).0 == 1
                        && occ.occupier_faction_id != 500_001
                })
                .map(|(&sys_id, occ)| (sys_id, occ.occupier_faction_id))
        })
        .expect("frontline FW system");
    let attacker_fac = if occ_fac == 500_002 { 500_001 } else { 500_002 };

    let sys_fw = ctx.systems.get_or_spawn(fw_sys);
    tick_background_cluster(&ctx).await;

    let (gate_id, gate_pos) = ctx.db.with_snapshot(|s| {
        let (&gid, _) = s
            .fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (csys, _, _, _, done))| *csys == fw_sys && !*done)
            .expect("spawned FW complex");
        let pos = s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0]);
        (gid, DVec3::from_array(pos))
    });

    let mut fw_sess = SessionState::new(92_055);
    common::login_pilot(&ctx, &mut fw_sess, "other", "other-secret").await;
    rpc(&ctx, &mut fw_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");

    let fw_ship = 90_000_188_i64;
    let fw_gun = 90_000_288_i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&PILOT_B) {
            ch.corporation_id = 1_000_180;
            ch.war_faction_id = Some(attacker_fac);
            ch.solar_system_id = fw_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = fw_ship;
        }
        s.items.insert(
            fw_ship,
            eve_db::InventoryItemRecord {
                item_id: fw_ship,
                type_id: 587, // Rifter
                owner_id: PILOT_B,
                location_id: fw_sys as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            fw_gun,
            eve_db::InventoryItemRecord {
                item_id: fw_gun,
                type_id: 484,
                owner_id: PILOT_B,
                location_id: fw_ship,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(fw_gun, true);
        s.items.insert(
            fw_gun + 1,
            eve_db::InventoryItemRecord {
                item_id: fw_gun + 1,
                type_id: 178,
                owner_id: PILOT_B,
                location_id: fw_ship,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(fw_ship);

    sys_fw
        .spawn_ball(Ball {
            id: fw_ship,
            type_id: 587,
            position: gate_pos + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");

    // Activate acceleration gate to warp into complex pocket
    rpc(
        &ctx,
        &mut fw_sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(gate_id)],
    )
    .await
    .expect("ActivateAccelerationGate");

    let _ = drain_events(&ctx, &mut fw_sess, PILOT_B);

    // Negative check: while guard NPC is alive, capture does NOT progress
    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;
    assert!(
        !drain_events(&ctx, &mut fw_sess, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnCaptureChanged"),
        "capture must not progress while guard NPC is alive"
    );

    // Clear guard NPC
    let guard_id = gate_id + 500_000_000;
    sys_fw
        .add_target_lock(
            fw_ship,
            TargetLock {
                target_id: guard_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock guard");

    rpc(
        &ctx,
        &mut fw_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(fw_gun),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(guard_id),
            PyRep::Integer(1_000),
        ],
    )
    .await
    .expect("fire gun on guard");

    for _ in 0..600 {
        if sys_fw.get_ball_snapshot(guard_id).await.ok().flatten().is_none() {
            break;
        }
        let _ = sys_fw.tick().await;
    }

    // Positive check: with guard destroyed, cluster tick advances capture and emits OnCaptureChanged
    let _ = drain_events(&ctx, &mut fw_sess, PILOT_B);
    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;

    let cap_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut fw_sess, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCaptureChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(cap_notifs.len(), 1, "must emit OnCaptureChanged after guard cleared");
    assert_eq!(cap_notifs[0].len(), 6, "OnCaptureChanged expects 6 args matching live V24 client");

    // Verify 6-arg shape: [ballID, captureID, lastIncident, points, captureTime, lastCapturing]
    assert_eq!(cap_notifs[0][0], PyRep::Integer(gate_id));
    assert_eq!(cap_notifs[0][1], PyRep::Integer(attacker_fac));
    assert!(matches!(cap_notifs[0][2], PyRep::Integer(_)));
    assert!(cap_notifs[0][3].as_f64().unwrap_or(0.0) > 0.0);
    assert!(cap_notifs[0][4].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(cap_notifs[0][5], PyRep::Integer(attacker_fac));
}

#[tokio::test]
async fn test_notify_clock_domain_separation_under_offset_and_tidi() {
    use crate::notify_structures_clones_pi::{drain_all, find_notif, login_in_space, make_item};
    use eve_db::{IskCents, InventoryFlag};
    use eve_machonet::service_roles::ROLE_GM_DEFAULT;
    use eve_server::services::abyssal_void_mgr::{
        ABYSSAL_SPACE_SYSTEM_ID, VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS, VOID_SPACE_SYSTEM_ID,
    };

    const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;
    let assert_sim_ft_near = |actual: i64, expected: i64, label: &str| {
        let delta = actual - expected;
        assert!(
            (0..=20).contains(&delta),
            "{label}: expected SimFileTime within [0, 20] monotonic 100ns ticks of {expected}, got {actual} (delta={delta})"
        );
    };
    let ctx = new_ctx();

    let ship_a = 921_000_001_i64;
    let ship_b = 921_000_002_i64;
    let launcher_id = 921_000_010_i64;
    let probe_stack_id = 921_000_011_i64;
    let abyss_fil_id = 921_000_020_i64;
    let void_fil_id = 921_000_021_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.balance = IskCents::from_isk(500_000_000);
        }
    });
    let mut sess_corp = SessionState::new(92_150);
    common::login_pilot(&ctx, &mut sess_corp, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess_corp,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("select docked PILOT_A");
    ctx.advance_secs(12);
    rpc(
        &ctx,
        &mut sess_corp,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Clock Domain Corp".into()), PyRep::String("CLKD".into())],
    )
    .await
    .expect("AddCorporation");

    let mut sess_a = login_in_space(
        &ctx,
        92_150,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        621,
        JITA_SYSTEM,
        DVec3::new(0.0, 2.0e9, 0.0),
    )
    .await;
    sess_a.role |= ROLE_GM_DEFAULT;

    let mut sess_b = login_in_space(
        &ctx,
        92_152,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        JITA_SYSTEM,
        DVec3::new(1_000.0, 2.0e9, 0.0),
    )
    .await;

    let uid_v = ctx.db.provision_player_account("clk_void_u", "clk_void_pass");
    let pilot_v = ctx
        .db
        .create_character(uid_v, "Clock Void Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create pilot_v")
        .character_id;
    let ship_v = 921_000_003_i64;
    let mut sess_v = login_in_space(
        &ctx,
        92_153,
        "clk_void_u",
        "clk_void_pass",
        pilot_v,
        ship_v,
        621,
        AMARR_SYSTEM,
        DVec3::new(0.0, 2.0e9, 0.0),
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            launcher_id,
            make_item(launcher_id, 17_901, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            probe_stack_id,
            make_item(probe_stack_id, 30_013, PILOT_A, ship_a, InventoryFlag::HiSlot0, 8, false),
        );
        snap.items.insert(
            abyss_fil_id,
            make_item(abyss_fil_id, 56_134, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            void_fil_id,
            make_item(void_fil_id, 62_448, pilot_v, ship_v, InventoryFlag::Cargo, 1, false),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);

    // Anchor JITA_SYSTEM, ABYSSAL_SPACE_SYSTEM_ID, and VOID_SPACE_SYSTEM_ID at -300s offset & 0.25 TiDi.
    ctx.anchor_system_sim_clock_with_offset_ms(JITA_SYSTEM, -300_000, 0.25);
    ctx.anchor_system_sim_clock_with_offset_ms(ABYSSAL_SPACE_SYSTEM_ID, -300_000, 0.25);
    ctx.anchor_system_sim_clock_with_offset_ms(VOID_SPACE_SYSTEM_ID, -300_000, 0.25);

    let sim_jita = ctx
        .peek_system_sim_filetime(JITA_SYSTEM)
        .expect("anchored Jita sim clock");
    let sim_abyss = ctx
        .peek_system_sim_filetime(ABYSSAL_SPACE_SYSTEM_ID)
        .expect("anchored Abyss sim clock");
    let sim_void = ctx
        .peek_system_sim_filetime(VOID_SPACE_SYSTEM_ID)
        .expect("anchored Void sim clock");
    let wall_ft = ctx.now_machonet_wall_filetime();
    assert!(
        wall_ft.as_ticks().saturating_sub(sim_jita.as_ticks()) >= 295 * FILETIME_TICKS_PER_SEC,
        "anchored sim clock ({}) must lag wallclock ({}) by ~300s",
        sim_jita.as_ticks(),
        wall_ft.as_ticks(),
    );

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let _ = drain_all(&ctx, &mut sess_v, pilot_v);

    // 1. OnSafeLogoffTimerStarted (`RT-99`): -300s sim offset + 30s * 0.25 TiDi = sim_jita + 7.5s.
    rpc(&ctx, &mut sess_b, "ship", "SafeLogoff", &[])
        .await
        .expect("SafeLogoff");
    let logoff_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    let logoff_ft = find_notif(&logoff_notifs, "OnSafeLogoffTimerStarted")[0]
        .as_i64()
        .expect("OnSafeLogoffTimerStarted arg");
    assert_eq!(
        logoff_ft,
        sim_jita.saturating_add_ms(7_500).as_ticks(),
        "OnSafeLogoffTimerStarted must emit dilated SimFileTime under -300s offset and TiDi=0.25"
    );
    assert!(logoff_ft < wall_ft.as_ticks());

    // 2. OnCaptureChanged: uses SimFileTime (`sim_jita`), NOT wall clock.
    eve_server::services::notify_emitters_space_dungeons::emit_fw_capture_changed(
        &ctx,
        JITA_SYSTEM,
        921_000_901,
        500_001,
        25.0,
        600_000.0,
    );
    let cap_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cap_ft = find_notif(&cap_notifs, "OnCaptureChanged")[2]
        .as_i64()
        .expect("OnCaptureChanged lastIncident");
    assert_sim_ft_near(cap_ft, sim_jita.as_ticks(), "OnCaptureChanged lastIncident");

    // 3. OnAddExpiryDurationForAbyssalPvpContent: virtual expiry mapped to SimFileTime (`+150s` at 0.25 TiDi).
    let pvp_exp_unix = ctx.now_unix() + 600;
    eve_server::services::notify_emitters_space_dungeons::emit_pvp_match_found_and_expiry(
        &ctx,
        PILOT_A,
        pvp_exp_unix,
    );
    let pvp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let pvp_ft = find_notif(&pvp_notifs, "OnAddExpiryDurationForAbyssalPvpContent")[0]
        .as_i64()
        .expect("OnAddExpiryDurationForAbyssalPvpContent endTime");
    assert_eq!(pvp_ft, sim_jita.saturating_add_secs(150).as_ticks());
    assert!(pvp_ft < wall_ft.as_ticks());

    // 4. OnModuleAttributeChange & OnLocationAttributeChange: use SimFileTime (`sim_jita`).
    rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/setattr {launcher_id} 18 75.0"))],
    )
    .await
    .expect("/setattr");
    let attr_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let mod_attr = find_notif(&attr_notifs, "OnModuleAttributeChange");
    let mod_ts = mod_attr[3].as_i64().expect("OnModuleAttributeChange time");
    assert_sim_ft_near(mod_ts, sim_jita.as_ticks(), "OnModuleAttributeChange time");
    assert_eq!(mod_attr[6].as_i64(), Some(mod_ts));
    let loc_attr = find_notif(&attr_notifs, "OnLocationAttributeChange");
    assert_eq!(loc_attr[4].as_i64(), Some(mod_ts));

    // 5. OnProbeWarpStart & OnSystemScanStarted: use SimFileTime (`sim_jita`).
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())],
    )
    .await
    .expect("launchProbe");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let scan_coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(250_000.0 + (i as f64) * 500.0),
                    PyRep::Real(2.0e9),
                    PyRep::Real(0.0),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    rpc(&ctx, &mut sess_a, "scanMgr", "RequestScans", &[scan_coords])
        .await
        .expect("RequestScans");
    let scan_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let mut warp_start_ft_opt: Option<i64> = None;
    let max_warp_ms = scan_notifs
        .iter()
        .filter(|(e, _)| e == "OnProbeWarpStart")
        .map(|(_, a)| {
            let st = a[3].as_i64().expect("OnProbeWarpStart startTime");
            assert_sim_ft_near(st, sim_jita.as_ticks(), "OnProbeWarpStart startTime");
            if let Some(prev_st) = warp_start_ft_opt {
                assert_eq!(st, prev_st);
            } else {
                warp_start_ft_opt = Some(st);
            }
            a[4].as_i64().expect("OnProbeWarpStart duration") as u64
        })
        .max()
        .expect("expected OnProbeWarpStart events");
    let warp_start_ft = warp_start_ft_opt.expect("OnProbeWarpStart startTime");
    let scan_start_ft = find_notif(&scan_notifs, "OnSystemScanStarted")[0]
        .as_i64()
        .expect("OnSystemScanStarted startTime");
    assert_eq!(
        scan_start_ft,
        eve_db::clock::SimFileTime::from_ticks(warp_start_ft)
            .saturating_add_ms(max_warp_ms)
            .as_ticks()
    );
    assert!(scan_start_ft < wall_ft.as_ticks());
    rpc(&ctx, &mut sess_a, "scanMgr", "DestroyProbe", &[])
        .await
        .expect("DestroyProbe");

    // 6. OnTournamentPerformBan & OnCorporationWelcomeMailChanged: valid WallFileTime unaffected by -300s/0.25x sim skew.
    rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(196),
            PyRep::Integer(19_601),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
            PyRep::List(vec![PyRep::Integer(PILOT_A), PyRep::Integer(PILOT_B)]),
        ],
    )
    .await
    .expect("CreateMatch");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ban_unix = ctx.now_unix();
    rpc(
        &ctx,
        &mut sess_a,
        "tourneyMgr",
        "BanShip",
        &[PyRep::Integer(19_601), PyRep::List(vec![PyRep::Integer(621)])],
    )
    .await
    .expect("BanShip");
    let ban_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ban_deadline_ft = find_notif(&ban_notifs, "OnTournamentPerformBan")[3]
        .as_i64()
        .expect("OnTournamentPerformBan deadline");
    assert_eq!(
        ban_deadline_ft,
        ctx.virtual_unix_to_machonet_wall_filetime(ban_unix + 60).as_ticks()
    );
    assert!(ban_deadline_ft >= wall_ft.saturating_add_secs(55).as_ticks());

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let welcome_unix = ctx.now_unix();
    rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "SetCorpWelcomeMail",
        &[
            PyRep::String("Welcome".into()),
            PyRep::String("Body".into()),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("SetCorpWelcomeMail");
    let welcome_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let welcome_ft = find_notif(&welcome_notifs, "OnCorporationWelcomeMailChanged")[1]
        .as_i64()
        .expect("OnCorporationWelcomeMailChanged changeDate");
    assert_eq!(welcome_ft, ctx.wall_unix_to_filetime(welcome_unix).as_ticks());
    assert!(welcome_ft >= wall_ft.as_ticks());

    // 7. OnAddExpiryDurationForAbyssalContent: SimFileTime in ABYSSAL_SPACE_SYSTEM_ID (-300s offset, 0.25 TiDi -> +300 sim-s).
    let deploy_rep = rpc(
        &ctx,
        &mut sess_a,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(abyss_fil_id)],
    )
    .await
    .expect("AbyssalEntranceDeployment");
    let trace_id = deploy_rep
        .dict_get("traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_id)],
    )
    .await
    .expect("AbyssalEntranceGateActivation");
    let abyss_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let abyss_end_ft = find_notif(&abyss_notifs, "OnAddExpiryDurationForAbyssalContent")[0]
        .as_i64()
        .expect("OnAddExpiryDurationForAbyssalContent endTime");
    assert_sim_ft_near(
        abyss_end_ft,
        sim_abyss.saturating_add_secs(300).as_ticks(),
        "OnAddExpiryDurationForAbyssalContent TiDi-scaled endTime",
    );
    assert!(
        wall_ft.saturating_add_secs(1_200).as_ticks().saturating_sub(abyss_end_ft)
            >= 1_195 * FILETIME_TICKS_PER_SEC
    );

    // 8. OnAddExpiryDurationForVoidSpaceContent & OnVoidSpaceRelog: SimFileTime in VOID_SPACE_SYSTEM_ID (-300s offset, 0.25 TiDi -> +450 sim-s).
    rpc(
        &ctx,
        &mut sess_v,
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        &[PyRep::Integer(void_fil_id), PyRep::Integer(777_196)],
    )
    .await
    .expect("VoidSpacePlayerJump");
    let void_notifs = drain_all(&ctx, &mut sess_v, pilot_v);
    let void_end_ft = find_notif(&void_notifs, "OnAddExpiryDurationForVoidSpaceContent")[0]
        .as_i64()
        .expect("OnAddExpiryDurationForVoidSpaceContent endTime");
    assert_sim_ft_near(
        void_end_ft,
        sim_void
            .saturating_add_secs((VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS as i64) / 4)
            .as_ticks(),
        "OnAddExpiryDurationForVoidSpaceContent TiDi-scaled endTime",
    );
    assert!(
        wall_ft
            .saturating_add_secs(VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS as i64)
            .as_ticks()
            .saturating_sub(void_end_ft)
            >= 1_645 * FILETIME_TICKS_PER_SEC
    );

    rpc(&ctx, &mut sess_v, "voidSpaceMgr", "ClientIsReady", &[])
        .await
        .expect("voidSpaceMgr.ClientIsReady");
    let relog_notifs = drain_all(&ctx, &mut sess_v, pilot_v);
    let relog_end_ft = find_notif(&relog_notifs, "OnVoidSpaceRelog")[0]
        .dict_get("end_time")
        .and_then(|v| v.as_i64())
        .expect("OnVoidSpaceRelog end_time");
    assert_eq!(relog_end_ft, void_end_ft);
}
}
