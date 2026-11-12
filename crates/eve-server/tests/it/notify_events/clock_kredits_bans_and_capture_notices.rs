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
            assert!(old_t < new_t, "sim clock must advance forward");
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
            PyRep::List(vec![PyRep::Real(0.35), PyRep::Real(0.75)]),
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

    // A2: Login campaign enrollment transition via slash command
    let sde_camps = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde);
    let alt_campaign = sde_camps
        .keys()
        .copied()
        .find(|&id| id != 2261)
        .expect("alternate login campaign");

    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/campaign enroll {alt_campaign} {PILOT_A}"))],
    )
    .await
    .expect("/campaign enroll");

    let login_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut gm, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnCampaignEnrollmentChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(login_notifs.len(), 1, "PILOT_A must receive OnCampaignEnrollmentChanged on login campaign enroll");
    assert_eq!(login_notifs[0].len(), 0, "OnCampaignEnrollmentChanged expects 0 args");

    // Negative check: invalid campaign ID fails and does not emit
    let err_camp = rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String("/campaign enroll 88888888".into())],
    )
    .await
    .expect_err("non-existent campaign must fail");
    assert_eq!(err_camp.code, "CampaignNotFound");

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
}
