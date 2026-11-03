//! Integration tests verifying remediation of FLAGGED_FAKE `rpc:*` and `notify:*` rows.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::quasar::envelopes::EvePublicRequest;
use eve_proto::quasar::GoogleAny;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const STATUS_OK: u32 = 200;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

fn drain_all(ctx: &ServerContext, _sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)).collect()
}

fn find_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> Vec<PyRep> {
    notifs.iter().find(|(e, _)| e == event).map(|(_, a)| a.clone()).unwrap_or_default()
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton,
        version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    }
}

async fn login_in_space(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.spawn_ball_with_meta(
        Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid },
        ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        eve_destiny::SlimItemMeta { group_id: 25, category_id: 6, owner_id: char_id, char_id: Some(char_id), corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default() },
    ).await.unwrap();
    sess
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

// parity: rpc:agentMgr.GetPlausibleMissionIDs, rpc:cache.Rowset
#[tokio::test]
async fn test_remediated_agent_plausible_missions_and_cache_rowset() {
    eve_testkit::attest(&[
        "rpc:agentMgr.GetPlausibleMissionIDs",
        "rpc:cache.Rowset",
    ]);

    let ctx = new_ctx();
    let mut sess = SessionState::new(91_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

    let sec_agent = ctx.sde.agents.values().find(|a| a.division_id == 24 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE security agent");
    let mining_agent = ctx.sde.agents.values().find(|a| a.division_id == 23 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE mining agent");

    let PyRep::List(sec_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(sec_agent)]).await.unwrap() else { panic!("expected List"); };
    let PyRep::List(mining_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(mining_agent)]).await.unwrap() else { panic!("expected List"); };
    assert!(!sec_missions.is_empty());
    assert!(!mining_missions.is_empty());
    assert_ne!(sec_missions, mining_missions, "security and mining agents must not return identical mission pools");

    let invalid_agent = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(999_999_999)]).await;
    assert_eq!(invalid_agent.unwrap_err().code, "AgentNotFound");

    let PyRep::Rowset { columns, rows } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else { panic!("expected Rowset"); };
    assert_eq!(columns, vec!["roleID", "roleName"]);
    assert!(rows.len() >= 30, "roles rowset must contain full EVE corporation roles, got {}", rows.len());
    assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Contract Manager".into()))));
    assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Project Manager".into()))));

    let PyRep::Rowset { rows: race_rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("races".into())]).await.unwrap() else { panic!("expected races Rowset"); };
    assert_eq!(race_rows.len(), ctx.sde.races.len());

    let unknown = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("nonexistent_table".into())]).await;
    assert_eq!(unknown.unwrap_err().code, "UnknownCacheRowset");
}

// parity: notify:OnNewProbe, notify:OnProbeStateChanged, notify:OnProbeWarpStart
// parity: notify:OnProbeWarpEnd, notify:OnSystemScanStarted, notify:OnSystemScanStopped
#[tokio::test]
async fn test_remediated_probe_scanning_lifecycle_notifications() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnProbeStateChanged",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnSystemScanStarted",
        "notify:OnSystemScanStopped",
    ]);

    let ctx = new_ctx();
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(1_000.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id) = (630_000_001_i64, 630_000_010_i64, 630_000_011_i64);
    let mut sess = login_in_space(&ctx, 91_002, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_pos).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.unwrap();
    let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let new_probe = &find_notif(&launch_notifs, "OnNewProbe")[0];
    let expiry_ft = dict_get(new_probe, "expiry").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(expiry_ft > 130_000_000_000_000_000, "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}");
    assert!(dict_get(new_probe, "pos").is_some(), "OnNewProbe must include pos");

    let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(500_000.0 + (i as f64) * 500.0), PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.unwrap();
    let immediate = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(immediate.iter().any(|(e, _)| e == "OnProbeStateChanged"), "moving probes must emit OnProbeStateChanged");
    assert_eq!(find_notif(&immediate, "OnProbeWarpStart").len(), 5);
    let started = find_notif(&immediate, "OnSystemScanStarted");
    let scan_ms = started[1].as_i64().unwrap_or(0);
    assert!(scan_ms > 4_000, "scan duration must be skill/ship-computed (> 4000ms), got {scan_ms}");
    assert!(!immediate.iter().any(|(e, _)| e == "OnSystemScanStopped"), "OnSystemScanStopped must not fire at 0s");
    assert!(!immediate.iter().any(|(e, _)| e == "OnProbeWarpEnd"), "OnProbeWarpEnd must not fire at 0s");

    ctx.advance_secs(15);
    let after_tick = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(after_tick.iter().filter(|(e, _)| e == "OnProbeWarpEnd").count(), 8);
    assert!(after_tick.iter().any(|(e, _)| e == "OnSystemScanStopped"));

    rpc(&ctx, &mut sess, "scanMgr", "RecoverProbes", &[]).await.unwrap();
    let recover_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(recover_notifs.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(4))));
}

// parity: notify:OnCampaignEnrollmentChanged, notify:OnCharacterPermanentlyBanned
// parity: notify:OnCharacterTemporarilyBanned, notify:OnCharacterUnbanned
// parity: notify:OnNpcTimerUpdate, notify:OnDisapprovalTimerUpdate, notify:OnSystemDisapprovalFlagUpdates
#[tokio::test]
async fn test_remediated_campaign_enrollment_chat_bans_and_crimewatch_timers() {
    eve_testkit::attest(&[
        "notify:OnCampaignEnrollmentChanged",
        "notify:OnCharacterPermanentlyBanned",
        "notify:OnCharacterTemporarilyBanned",
        "notify:OnCharacterUnbanned",
        "notify:OnNpcTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnSystemDisapprovalFlagUpdates",
    ]);

    let ctx = new_ctx();
    let mut gm_sess = SessionState::new(91_003);
    common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    let mut p2_sess = SessionState::new(91_004);
    common::login_pilot(&ctx, &mut p2_sess, "other", "other-secret").await;
    rpc(&ctx, &mut p2_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();

    let alt_campaign = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
        .keys()
        .copied()
        .find(|&id| id != 2261)
        .unwrap_or(2261);
    let before_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/campaign enroll {alt_campaign}"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut gm_sess, PILOT_A).iter().any(|(e, _)| e == "OnCampaignEnrollmentChanged"));
    let after_p1 = rpc(&ctx, &mut gm_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    let after_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(dict_get(&after_p1, "campaignID"), Some(&PyRep::Integer(alt_campaign)));
    assert_eq!(dict_get(&after_p2, "campaignID"), dict_get(&before_p2, "campaignID"));

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} perm"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterPermanentlyBanned"));
    let perm_rsp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("blocked"), None, false, ctx.now_unix());
    assert_ne!(perm_rsp.status_code, STATUS_OK, "permanently chat-banned character must be blocked");

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatunban {PILOT_B}"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterUnbanned"));

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} 30"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterTemporarilyBanned"));
    ctx.advance_secs(35);
    let after_exp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("restored"), None, false, ctx.now_unix());
    assert_eq!(after_exp.status_code, STATUS_OK, "temporarily chat-banned character must be unblocked after expiry");

    let zarzakh = 30_100_000_u32;
    let (ship_a, ship_b, gun_id) = (630_000_101_i64, 630_000_102_i64, 630_000_110_i64);
    let mut s_a = login_in_space(&ctx, 91_005, "demo", "demo-secret", PILOT_A, ship_a, 587, zarzakh, DVec3::ZERO).await;
    let _s_b = login_in_space(&ctx, 91_006, "other", "other-secret", PILOT_B, ship_b, 587, zarzakh, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_id, make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    let sys_h = ctx.systems.get_or_spawn(zarzakh);
    sys_h.add_target_lock(ship_a, TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
    drain_all(&ctx, &mut s_a, PILOT_A);
    rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)]).await.unwrap();
    let z_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
    assert!(z_notifs.iter().any(|(e, _)| e == "OnDisapprovalTimerUpdate"));
    assert!(z_notifs.iter().any(|(e, _)| e == "OnSystemDisapprovalFlagUpdates"));

    let npc_id = 630_000_199_i64;
    sys_h.spawn_ball(Ball {
        id: npc_id, type_id: 16_250, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();
    sys_h.add_target_lock(ship_a, TargetLock { target_id: npc_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
    drain_all(&ctx, &mut s_a, PILOT_A);
    rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)]).await.unwrap();
    let npc_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
    assert!(npc_notifs.iter().any(|(e, _)| e == "OnNpcTimerUpdate"));
}

#[tokio::test]
async fn test_corp_role_bit_values_match_v24_client() {
    use eve_db::corp_roles;

    // Exact bit values from V24 client `eve/common/lib/appConst.pyj` (`corpRole*` constants).
    let expected_v24_roles: &[(u64, &str)] = &[
        (1 << 0, "Director"),
        (1 << 1, "Deliveries Hangar Take"),
        (1 << 2, "Deliveries Hangar Query"),
        (1 << 3, "Deliveries Hangar Container Take"),
        (1 << 7, "Personnel Manager"),
        (1 << 8, "Accountant"),
        (1 << 9, "Security Officer"),
        (1 << 10, "Factory Manager"),
        (1 << 11, "Station Manager"),
        (1 << 12, "Auditor"),
        (1 << 13, "Hangar Can Take 1"),
        (1 << 14, "Hangar Can Take 2"),
        (1 << 15, "Hangar Can Take 3"),
        (1 << 16, "Hangar Can Take 4"),
        (1 << 17, "Hangar Can Take 5"),
        (1 << 18, "Hangar Can Take 6"),
        (1 << 19, "Hangar Can Take 7"),
        (1 << 20, "Hangar Can Query 1"),
        (1 << 21, "Hangar Can Query 2"),
        (1 << 22, "Hangar Can Query 3"),
        (1 << 23, "Hangar Can Query 4"),
        (1 << 24, "Hangar Can Query 5"),
        (1 << 25, "Hangar Can Query 6"),
        (1 << 26, "Hangar Can Query 7"),
        (1 << 27, "Account Can Take 1"),
        (1 << 28, "Account Can Take 2"),
        (1 << 29, "Account Can Take 3"),
        (1 << 30, "Account Can Take 4"),
        (1 << 31, "Account Can Take 5"),
        (1 << 32, "Account Can Take 6"),
        (1 << 33, "Account Can Take 7"),
        (1 << 34, "Diplomat"),
        (1 << 35, "Brand Manager"),
        (1 << 41, "Equipment Config"),
        (1 << 42, "Container Can Take 1"),
        (1 << 43, "Container Can Take 2"),
        (1 << 44, "Container Can Take 3"),
        (1 << 45, "Container Can Take 4"),
        (1 << 46, "Container Can Take 5"),
        (1 << 47, "Container Can Take 6"),
        (1 << 48, "Container Can Take 7"),
        (1 << 49, "Can Rent Office"),
        (1 << 50, "Can Rent Factory Slot"),
        (1 << 51, "Can Rent Research Slot"),
        (1 << 52, "Junior Accountant"),
        (1 << 53, "Starbase Config"),
        (1 << 54, "Trader"),
        (1 << 55, "Chat Manager"),
        (1 << 56, "Contract Manager"),
        (1 << 57, "Infrastructure Tactical Officer"),
        (1 << 58, "Starbase Caretaker"),
        (1 << 59, "Fitting Manager"),
        (1 << 60, "Project Manager"),
        (1 << 61, "Terrestrial Logistics Officer"),
        (1 << 62, "Skill Plan Manager"),
    ];

    assert_eq!(corp_roles::ROLE_DIRECTOR, 1 << 0);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_TAKE, 1 << 1);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_QUERY, 1 << 2);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE, 1 << 3);
    assert_eq!(corp_roles::ROLE_BRAND_MANAGER, 1 << 35);
    assert_eq!(corp_roles::ROLE_TRADER, 1 << 54);
    assert_eq!(corp_roles::ROLE_CHAT_MANAGER, 1 << 55);
    assert_eq!(corp_roles::ROLE_CONTRACT_MANAGER, 1 << 56);
    assert_eq!(corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN, 1 << 57);
    assert_eq!(corp_roles::ROLE_STARBASE_CARETAKER, 1 << 58);
    assert_eq!(corp_roles::ROLE_FITTING_MANAGER, 1 << 59);
    assert_eq!(corp_roles::ROLE_PROJECT_MANAGER, 1 << 60);
    assert_eq!(corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER, 1 << 61);
    assert_eq!(corp_roles::ROLE_SKILL_PLAN_MANAGER, 1 << 62);

    let ctx = new_ctx();
    let mut sess = SessionState::new(91_007);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

    let PyRep::Rowset { rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else {
        panic!("expected Rowset");
    };
    let actual: Vec<(u64, String)> = rows
        .into_iter()
        .map(|r| {
            let id = r[0].as_i64().expect("roleID integer") as u64;
            let name = r[1].as_str().expect("roleName string").to_string();
            (id, name)
        })
        .collect();

    for &(bit, expected_name) in expected_v24_roles {
        let found: Vec<&str> = actual
            .iter()
            .filter_map(|(id, name)| (*id == bit).then_some(name.as_str()))
            .collect();
        assert_eq!(found, vec![expected_name], "mismatch for role bit 1 << {}", bit.trailing_zeros());
    }
    assert_eq!(actual.len(), expected_v24_roles.len(), "unexpected extra/missing roles in cache.Rowset(\"roles\")");
}

