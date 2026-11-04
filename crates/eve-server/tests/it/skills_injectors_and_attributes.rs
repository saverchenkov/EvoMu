//! Integration tests for character and skill features (`feat:character.medals-display`,
//! `feat:character.online-status`, `feat:character.safe-logoff`, `feat:skills.nondiminishing-injector`).

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::session_cluster_lifecycle::{record_pvp_aggression_and_renew_logoff_timer, unix_to_filetime};
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let empty = HashMap::new();
    ctx.dispatch_rpc(sess, svc, None, method, args, &empty, None).await
}

async fn setup_pilot_in_space(ctx: &ServerContext, char_id: i64) -> (SessionState, u32, i64) {
    let mut sess = SessionState::default();
    let empty = HashMap::new();
    let (u, p) = if char_id == 90_000_001 { ("demo", "demo-secret") } else { ("other", "other-secret") };
    login_pilot(ctx, &mut sess, u, p).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    if sess.station_id.is_some() {
        rpc(ctx, &mut sess, "ship", "Undock", &[]).await.expect("Undock");
    } else if let Some(sys_id) = sess.solar_system_id {
        let oid = ctx.bind_object(&sess, "beyonce", sys_id as i64).expect("bind beyonce");
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(oid), "UpdateStateRequest", &[], &empty, None).await.expect("UpdateStateRequest");
    }
    (sess.clone(), sess.solar_system_id.unwrap_or(30_000_142), sess.ship_id.expect("ship_id"))
}

fn rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        other => panic!("expected Rowset, got {other:?}"),
    }
}

fn keyval(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.clone(),
        other => panic!("expected KeyVal, got {other:?}"),
    }
}

/// Tests `feat:character.medals-display`:
/// - Issue medal to character with initial private status (status 2).
/// - Third-party viewers see empty list in `GetMedalsReceived`.
/// - Recipient updates status to public (status 3) via `corporationSvc.SetMedalStatus`.
/// - `snap.char_medals` and `snap.service_state_blobs` are updated.
/// - Third-party viewers now see the public medal in `GetMedalsReceived`.
/// - Reverting status to private hides the medal from third-party viewers again.
// parity: feat:character.medals-display
#[tokio::test]
async fn test_character_medals_display_visibility() {
    eve_testkit::attest(&["feat:character.medals-display"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut sess1 = SessionState::default();
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID 1");

    let mut sess2 = SessionState::default();
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.expect("SelectCharacterID 2");

    ctx.advance_secs(15);

    // 0. Found a player corporation where 90_000_001 is CEO/Director
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Honor Guard Logistics".into()), PyRep::String("HGLOG".into()), PyRep::Real(0.04)],
    ).await.expect("AddCorporation");

    // 1. Create a corporation medal as CEO (90_000_001)
    let create_rep = rpc(
        &ctx, &mut sess1, "corporationSvc", "CreateMedal",
        &[
            PyRep::String("Valorous Defense Cross".into()),
            PyRep::String("Conferred for defense of fleet assets".into()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::String("ribbon_gold".into()), PyRep::Integer(12)])]),
        ],
    ).await.expect("CreateMedal");

    let medal_kv = keyval(&create_rep);
    let medal_id = medal_kv.get("medalID").and_then(|v| v.as_i64()).expect("medalID in keyval");

    // 2. Award medal to character 90_000_002
    rpc(
        &ctx, &mut sess1, "corporationSvc", "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::String("Outstanding conduct in battle".into())],
    ).await.expect("GiveMedalToCharacters");

    // 3. Query GetMedalsReceived as recipient (owner): returns the private medal
    let rcv_owner = rpc(&ctx, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(90_000_002)]).await.expect("GetMedalsReceived owner");
    let PyRep::Tuple(owner_pair) = rcv_owner else { panic!("expected 2-Tuple from GetMedalsReceived") };
    let (_, owner_rows) = rows(&owner_pair[0]);
    assert_eq!(owner_rows.len(), 1, "Owner should see issued medal");
    assert_eq!(owner_rows[0][0], PyRep::Integer(medal_id));

    // 4. Query GetMedalsReceived as third party (90_000_001): returns 0 rows (private hidden)
    let rcv_tp = rpc(&ctx, &mut sess1, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(90_000_002)]).await.expect("GetMedalsReceived third party");
    let PyRep::Tuple(tp_pair) = rcv_tp else { panic!("expected 2-Tuple from GetMedalsReceived") };
    let (_, tp_rows) = rows(&tp_pair[0]);
    assert_eq!(tp_rows.len(), 0, "Third-party should not see private medal");

    // 5. Update medal status to public (status 3) via SetMedalStatus dictionary argument {medalID: status}
    let pub_dict = PyRep::Dict(vec![(PyRep::Integer(medal_id), PyRep::Integer(3))]);
    let set_res = rpc(&ctx, &mut sess2, "corporationSvc", "SetMedalStatus", &[pub_dict]).await.expect("SetMedalStatus public dict");
    let set_kv = keyval(&set_res);
    assert_eq!(set_kv.get("status").and_then(|v| v.as_i64()), Some(3));

    // 6. Verify snap.char_medals and snap.service_state_blobs reflect public status
    ctx.db.with_snapshot(|snap| {
        let medals = snap.char_medals.get(&90_000_002).expect("char_medals entry");
        assert!(medals.iter().any(|m| m.medal_id == medal_id && m.status == "public"));
        let blob = snap.service_state_blobs.get("character_medals:90000002").expect("service_state_blobs");
        assert!(blob.contains("public"));
    });

    // 7. Query GetMedalsReceived as third party: now returns 1 row (public medal visible)
    let rcv_tp_pub = rpc(&ctx, &mut sess1, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(90_000_002)]).await.expect("GetMedalsReceived third party after public");
    let PyRep::Tuple(tp_pub_pair) = rcv_tp_pub else { panic!("expected 2-Tuple") };
    let (_, tp_pub_rows) = rows(&tp_pub_pair[0]);
    assert_eq!(tp_pub_rows.len(), 1, "Third party should see public medal");
    assert_eq!(tp_pub_rows[0][0], PyRep::Integer(medal_id));

    // 8. Revert status to private (status 2) via dictionary argument {medalID: status}
    let priv_dict = PyRep::Dict(vec![(PyRep::Integer(medal_id), PyRep::Integer(2))]);
    rpc(&ctx, &mut sess2, "corporationSvc", "SetMedalStatus", &[priv_dict]).await.expect("SetMedalStatus private dict");

    // 9. Third party query returns 0 rows again, but owner sees 1 row
    let rcv_tp_priv = rpc(&ctx, &mut sess1, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(90_000_002)]).await.expect("GetMedalsReceived third party after revert");
    let PyRep::Tuple(tp_priv_pair) = rcv_tp_priv else { panic!("expected 2-Tuple") };
    let (_, tp_priv_rows) = rows(&tp_priv_pair[0]);
    assert_eq!(tp_priv_rows.len(), 0, "Third party should no longer see reverted medal");

    // 10. Delete medal from character's medals via status = 1
    let del_dict = PyRep::Dict(vec![(PyRep::Integer(medal_id), PyRep::Integer(1))]);
    rpc(&ctx, &mut sess2, "corporationSvc", "SetMedalStatus", &[del_dict]).await.expect("SetMedalStatus delete dict");

    // Verify deleted from snap.char_medals and owner query returns 0 rows
    ctx.db.with_snapshot(|snap| {
        if let Some(medals) = snap.char_medals.get(&90_000_002) {
            assert!(medals.iter().all(|m| m.medal_id != medal_id));
        }
    });
    let rcv_owner_del = rpc(&ctx, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(90_000_002)]).await.expect("GetMedalsReceived owner after delete");
    let PyRep::Tuple(owner_del_pair) = rcv_owner_del else { panic!("expected 2-Tuple") };
    let (_, owner_del_rows) = rows(&owner_del_pair[0]);
    assert_eq!(owner_del_rows.len(), 0, "Owner should no longer see deleted medal");
}

/// Tests `feat:character.online-status`:
/// - Single character ID and multi-ID list queries for online/offline characters.
/// - Mutual contact blocking prevents online status visibility in both directions.
// parity: feat:character.online-status
#[tokio::test]
async fn test_character_online_status_reporting() {
    eve_testkit::attest(&["feat:character.online-status"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut sess1 = SessionState::default();
    login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID 1");

    let mut sess2 = SessionState::default();
    login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.expect("SelectCharacterID 2");

    // 1. Single-ID query
    let st2 = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(90_000_002)]).await.expect("GetOnlineStatus 90000002");
    assert_eq!(st2, PyRep::Bool(true), "Pilot 2 is logged in");

    let st3 = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(90_000_003)]).await.expect("GetOnlineStatus 90000003");
    assert_eq!(st3, PyRep::Bool(false), "Pilot 3 is offline");

    // 2. Multi-ID list query returning Dict
    let batch_res = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(90_000_003)])]).await.expect("GetOnlineStatus batch");
    let PyRep::Dict(entries) = batch_res else { panic!("expected PyRep::Dict from batch GetOnlineStatus") };
    let dict_map: HashMap<i64, bool> = entries.into_iter().filter_map(|(k, v)| match (k, v) {
        (PyRep::Integer(id), PyRep::Bool(b)) => Some((id, b)),
        _ => None,
    }).collect();
    assert_eq!(dict_map.get(&90_000_002), Some(&true));
    assert_eq!(dict_map.get(&90_000_003), Some(&false));

    // 3. Mutual blocking: Target (90_000_002) blocks Caller (90_000_001)
    rpc(&ctx, &mut sess2, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_001)])]).await.expect("BlockOwners 1");
    let blocked_st = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(90_000_002)]).await.expect("GetOnlineStatus when blocked by target");
    assert_eq!(blocked_st, PyRep::Bool(false), "Online status must be hidden when blocked by target");

    // 4. Target unblocks, but Caller blocks Target
    rpc(&ctx, &mut sess2, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_001)])]).await.expect("UnblockOwners 1");
    rpc(&ctx, &mut sess1, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_002)])]).await.expect("BlockOwners 2");
    let blocked_by_caller_st = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(90_000_002)]).await.expect("GetOnlineStatus when caller blocks target");
    assert_eq!(blocked_by_caller_st, PyRep::Bool(false), "Online status must be hidden when caller blocks target");

    // 5. Caller unblocks Target -> online status visible again
    rpc(&ctx, &mut sess1, "charMgr", "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_002)])]).await.expect("UnblockOwners 2");
    let restored_st = rpc(&ctx, &mut sess1, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(90_000_002)]).await.expect("GetOnlineStatus restored");
    assert_eq!(restored_st, PyRep::Bool(true), "Online status restored after unblocking");
}

/// Tests `feat:character.safe-logoff`:
/// - SafeLogoff refused when combat/weapons/PvP timer active.
/// - Countdown initiation and cancellation via AbortSafeLogoff.
/// - Full 30s safe logoff removes ship ball without wreck and stores coordinates.
// parity: feat:character.safe-logoff
#[tokio::test]
async fn test_character_safe_logoff_lifecycle() {
    eve_testkit::attest(&["feat:character.safe-logoff"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Refusal under PvP/weapons timer
    record_pvp_aggression_and_renew_logoff_timer(&ctx, 90_000_001, 90_000_002, ctx.now_unix());
    let _ = sess.drain_notifications();
    let refused_res = rpc(&ctx, &mut sess, "ship", "SafeLogoff", &[]).await.expect("SafeLogoff under PvP timer");
    let PyRep::List(conds) = refused_res else { panic!("expected list of failed conditions") };
    assert!(conds.contains(&PyRep::String("SAFE_LOGOFF_PVP_TIMER".into())));
    assert!(conds.contains(&PyRep::String("SAFE_LOGOFF_WEAPONS_TIMER".into())));
    let fail_notifs = sess.drain_notifications();
    assert!(fail_notifs.iter().any(|(n, _)| n == "OnSafeLogoffFailed"));

    // 2. Advance past timers (15m PvP timer)
    ctx.advance_secs(901);

    // 3. Initiate SafeLogoff when clear -> returns [] and starts 30s timer
    let _ = sess.drain_notifications();
    let expected_ft = unix_to_filetime(ctx.now_unix() + 30);
    let ok_res = rpc(&ctx, &mut sess, "ship", "SafeLogoff", &[]).await.expect("SafeLogoff clean");
    assert_eq!(ok_res, PyRep::List(vec![]));
    let start_notifs = sess.drain_notifications();
    assert!(start_notifs.iter().any(|(n, args)| n == "OnSafeLogoffTimerStarted" && *args == vec![PyRep::Integer(expected_ft)]));

    // 4. Abort countdown via ship.AbortSafeLogoff
    let _ = sess.drain_notifications();
    let abort_res = rpc(&ctx, &mut sess, "ship", "AbortSafeLogoff", &[]).await.expect("AbortSafeLogoff");
    assert_eq!(abort_res, PyRep::Bool(true));
    let abort_notifs = sess.drain_notifications();
    assert!(abort_notifs.iter().any(|(n, _)| n == "OnSafeLogoffAborted"));

    // 5. Restart SafeLogoff and verify instant despawn at T+30s without wreck
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let custom_pos = DVec3::new(55_000.0, -12_000.0, 77_000.0);
    let _ = sys_handle.set_ball_position(ship_id, custom_pos).await;

    let _ = ctx.drain_notifications_for_char(90_000_001);
    rpc(&ctx, &mut sess, "ship", "SafeLogoff", &[]).await.expect("SafeLogoff final");

    // At T+29s, ship ball still in space
    ctx.advance_secs(29);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

    // At T+30s, ship ball removed from space
    ctx.advance_secs(1);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    let activated_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(activated_notifs.iter().any(|n| n.event == "OnSafeLogoffActivated" && n.args.is_empty()));

    // Saved position matches space position
    let saved_pos = ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()).expect("saved ship position");
    assert_eq!(saved_pos[0].round() as i64, custom_pos.x.round() as i64);
}

/// Tests `feat:skills.nondiminishing-injector`:
/// - GetAvailableNonDiminishingInjections returns available non-diminishing injectors.
/// - Consumption grants fixed SP without diminishing returns regardless of character total SP tier.
// parity: feat:skills.nondiminishing-injector
#[tokio::test]
async fn test_skills_nondiminishing_injector() {
    eve_testkit::attest(&["feat:skills.nondiminishing-injector"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut sess = SessionState::default();
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");
    let station_id = sess.station_id.expect("docked station_id");

    // 1. Initial call when no non-diminishing injectors owned: returns empty dict
    let initial_res = rpc(&ctx, &mut sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await.expect("GetAvailableNonDiminishingInjections initially");
    assert_eq!(initial_res, PyRep::Dict(vec![]));

    // 2. Seed non-diminishing injectors in station hangar:
    //    - Mini Skill Injector (type 43630, qty 2, contained SP = 25,000)
    //    - QA Skill Injector (type 42523, qty 1, contained SP = 150,000)
    let mini_item_id = 888_001;
    let qa_item_id = 888_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mini_item_id,
            InventoryItemRecord {
                item_id: mini_item_id,
                type_id: 43630, // Mini Skill Injector
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            qa_item_id,
            InventoryItemRecord {
                item_id: qa_item_id,
                type_id: 42523, // QA Skill Injector
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Set high total SP (90 million SP), where diminishing returns would drop yield
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.skill_points = 90_000_000.0;
        ch.free_skill_points = 0.0;
    });

    // 3. Query GetAvailableNonDiminishingInjections: returns {43630: 2, 42523: 1}
    let avail_res = rpc(&ctx, &mut sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await.expect("GetAvailableNonDiminishingInjections seeded");
    let PyRep::Dict(entries) = avail_res else { panic!("expected PyRep::Dict") };
    let counts: HashMap<i64, i64> = entries.into_iter().filter_map(|(k, v)| match (k, v) {
        (PyRep::Integer(tid), PyRep::Integer(q)) => Some((tid, q)),
        _ => None,
    }).collect();
    assert_eq!(counts.get(&43630), Some(&2));
    assert_eq!(counts.get(&42523), Some(&1));

    // 4. Inject 1 Mini Skill Injector via skillMgr2.InjectSkillpoints
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let inject_res = rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(mini_item_id)]).await.expect("InjectSkillpoints Mini");
    // Must grant exactly 25,000 SP without diminishing returns
    assert_eq!(inject_res, PyRep::Real(25_000.0));

    // Verify character free SP increased by fixed 25,000 SP
    let free_sp = ctx.db.with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().free_skill_points);
    assert_eq!(free_sp, 25_000.0);

    // Verify notifications OnNonDiminishingInjectionsUsed(1) and OnFreeSkillPointsChanged
    let notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(notifs.iter().any(|n| n.event == "OnNonDiminishingInjectionsUsed" && n.args == vec![PyRep::Integer(1)]));
    assert!(notifs.iter().any(|n| n.event == "OnFreeSkillPointsChanged" && n.args == vec![PyRep::Real(25_000.0)]));

    // Verify available counts decremented: {43630: 1, 42523: 1}
    let after_res = rpc(&ctx, &mut sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await.expect("GetAvailableNonDiminishingInjections after 1 injection");
    let PyRep::Dict(after_entries) = after_res else { panic!("expected PyRep::Dict") };
    let after_counts: HashMap<i64, i64> = after_entries.into_iter().filter_map(|(k, v)| match (k, v) {
        (PyRep::Integer(tid), PyRep::Integer(q)) => Some((tid, q)),
        _ => None,
    }).collect();
    assert_eq!(after_counts.get(&43630), Some(&1));
    assert_eq!(after_counts.get(&42523), Some(&1));
}

pub(crate) mod starter_skills_all_empires_validation {
    // Client-RPC integration tests closing Starter Skills, Skillbook Market/Direct Injection,
    // and Large/Small/Daily-Alpha/Non-Diminishing Skill Injectors:
    // - `feat:character.starter-skills`
    // - `feat:skills.inject-skillbook`
    // - `feat:skills.direct-purchase-inject`
    // - `feat:skills.skill-injector-large`
    // - `feat:skills.skill-injector-small`
    // - `feat:skills.alpha-injector`
    // - `feat:skills.nondiminishing-injector`


    use std::collections::HashMap;

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::formulas::sp_for_skill_level;
    use eve_server::router::ServerContext;

    use crate::common;

    const JITA_SYS: u32 = 30_000_142;
    const JITA_STN: i64 = 60_003_760;

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
    }

    async fn login_and_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
        s
    }

    fn doll_pair() -> (PyRep, PyRep) {
        (
            PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]),
            PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]),
        )
    }

    async fn create_pilot_in_jita(ctx: &ServerContext, sid: u64, user: &str, pass: &str, name: &str, bl: i64, anc: i64, school: i64) -> (SessionState, i64) {
        ctx.db.provision_player_account(user, pass);
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        let (doll, portrait) = doll_pair();
        let cid = rpc(
            ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll",
            &[PyRep::String(name.into()), PyRep::Integer(bl), PyRep::Integer(1), PyRep::Integer(anc), doll, portrait, PyRep::Integer(school), PyRep::None, PyRep::Integer(JITA_SYS as i64)],
        ).await.expect("CreateCharacterWithDoll").as_i64().expect("char_id");
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("SelectCharacterID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.solar_system_id = JITA_SYS;
                c.station_id = Some(JITA_STN);
                let ship_id = c.active_ship_id;
                if let Some(ship) = snap.items.get_mut(&ship_id) {
                    ship.location_id = JITA_STN;
                }
            }
        });
        sess.solar_system_id = Some(JITA_SYS);
        sess.station_id = Some(JITA_STN);
        (sess, cid)
    }

    fn place_hangar_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, qty: i32) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id, owner_id, location_id: JITA_STN, flag: InventoryFlag::Hangar,
                quantity: qty, singleton: false, version: 1, ..Default::default()
            });
        });
    }

    async fn upgrade_to_omega_via_nes(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, plex_item_id: i64) {
        place_hangar_item(ctx, plex_item_id, 44_992, char_id, 500);
        rpc(ctx, sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA_STN), PyRep::Integer(plex_item_id), PyRep::Integer(500), PyRep::Integer(0)]).await.expect("DepositPlexToVault");
        rpc(ctx, sess, "storeManager", "buy_offer", &[PyRep::Integer(1001)]).await.expect("buy_offer 1001");
    }

    #[tokio::test]
    async fn test_starter_skills_all_empires_and_validation() {
        // parity: feat:character.starter-skills, rpc:charUnboundMgr.CreateCharacterWithDoll
        eve_testkit::attest(&["feat:character.starter-skills"]);
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        // Verify all 4 EVE empire lineages receive exact SDE racial starter skills, school corps, and rookie corvettes:
        // (race_id, bloodline_id, ancestry_id, school_id, expected_corp_id, expected_corvette_id, racial_frigate_skill_id)
        let empires = [
            (1u32, 1i64, 11i64, 17i64, 1_000_167i64, 601u32, 3330u32, "Caldari Cadet"),
            (2, 4, 22, 14, 1_000_170, 588, 3329, "Minmatar Cadet"),
            (4, 5, 1, 11, 1_000_166, 596, 3331, "Amarr Cadet"),
            (8, 7, 13, 20, 1_000_168, 606, 3328, "Gallente Cadet"),
        ];

        for (idx, (race_id, bl, anc, school, exp_corp, exp_corvette, frigate_sk, name)) in empires.into_iter().enumerate() {
            let user = format!("emp_user_{idx}");
            let (mut sess, cid) = create_pilot_in_jita(&ctx, 2100 + idx as u64, &user, "secret", name, bl, anc, school).await;

            let info = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(cid)]).await.expect("GetCharacterInfo");
            assert_eq!(info.dict_get("corporationID"), Some(&PyRep::Integer(exp_corp)));
            assert_eq!(info.dict_get("raceID"), Some(&PyRep::Integer(race_id as i64)));

            let PyRep::Rowset { rows, .. } = rpc(&ctx, &mut sess, "skillMgr2", "GetSkills", &[]).await.expect("GetSkills") else {
                panic!("Expected Rowset from GetSkills");
            };
            let returned_skills: HashMap<u32, (u8, f64)> = rows.iter().map(|r| {
                (r[0].as_i64().unwrap() as u32, (r[1].as_i64().unwrap() as u8, r[2].as_f64().unwrap()))
            }).collect();

            let expected_sde_skills = &ctx.sde.races[&race_id].starter_skills;
            let mut expected_total_sp = 0.0;
            for (&sk_id, &lvl) in expected_sde_skills {
                if lvl > 0 {
                    let (got_lvl, got_sp) = returned_skills.get(&sk_id).copied().unwrap_or_else(|| panic!("Missing starter skill {sk_id} for race {race_id}"));
                    assert_eq!(got_lvl, lvl, "Starter skill {sk_id} level mismatch for race {race_id}");
                    let rank = ctx.sde.types.get(&sk_id).and_then(|t| t.default_attributes.get(&275).copied()).unwrap_or(1.0);
                    let exp_sp = sp_for_skill_level(rank, lvl);
                    assert!((got_sp - exp_sp).abs() < 1e-3, "Starter skill {sk_id} SP mismatch");
                    expected_total_sp += exp_sp;
                }
            }
            assert_eq!(returned_skills.get(&frigate_sk).map(|v| v.0), Some(1), "Racial frigate skill {frigate_sk} must start at level 1");
            let ch = ctx.db.get_character(cid).unwrap();
            assert!((ch.skill_points - expected_total_sp).abs() < 1e-3);
            assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == cid && i.type_id == exp_corvette)));
        }

        // Negative validations: cross-race school mismatch, duplicate character name, and 4th slot limit (`MaxCharacterSlotsReached`)
        ctx.db.provision_player_account("slot_test_u", "secret");
        let mut s_neg = SessionState::new(2199);
        common::login_pilot(&ctx, &mut s_neg, "slot_test_u", "secret").await;
        let (d1, p1) = doll_pair();
        let mismatch_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
            PyRep::String("Bad Lineage".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d1, p1, PyRep::Integer(11), // Caldari bloodline 1 + Amarr school 11
        ]).await;
        assert!(mismatch_err.is_err(), "Cross-race bloodline/school mismatch must be rejected");

        let (d2, p2) = doll_pair();
        let dup_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
            PyRep::String("Caldari Cadet".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d2, p2, PyRep::Integer(17),
        ]).await;
        assert!(dup_err.is_err(), "Duplicate character name must be rejected");

        for slot in 1..=3 {
            let (d, p) = doll_pair();
            rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
                PyRep::String(format!("Slot Pilot {slot}")), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d, p, PyRep::Integer(17),
            ]).await.expect("Create character within 3-slot cap");
        }
        let (d4, p4) = doll_pair();
        let slot4_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
            PyRep::String("Slot Pilot Four".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d4, p4, PyRep::Integer(17),
        ]).await.unwrap_err();
        assert_eq!(slot4_err.code, "MaxCharacterSlotsReached");
    }

    #[tokio::test]
    async fn test_skillbook_market_buy_inject_and_remote_purchase() {
        // parity: feat:skills.inject-skillbook, feat:skills.direct-purchase-inject
        eve_testkit::attest(&["feat:skills.inject-skillbook", "feat:skills.direct-purchase-inject"]);
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        eve_server::world_seeder::seed_dual_anchor_markets(&ctx.db, &ctx.sde);

        // Seller `demo` (90_000_001) donates ISK to fresh buyer AND lists a player sell order for Cloaking (11579) on Jita 4-4
        let mut seller = login_and_select(&ctx, 2201, "demo", "demo-secret", 90_000_001).await;
        let (mut buyer, buyer_cid) = create_pilot_in_jita(&ctx, 2202, "book_buyer_u", "secret", "Book Buyer", 1, 11, 17).await;
        rpc(&ctx, &mut seller, "account", "GiveCash", &[PyRep::Integer(buyer_cid), PyRep::Real(10_000_000.0), PyRep::String("market funds".into())]).await.unwrap();

        place_hangar_item(&ctx, 992_100_001, 11_579, 90_000_001, 1);
        rpc(&ctx, &mut seller, "marketProxy", "PlaceMultiSellOrder", &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STN)),
                ("typeID", PyRep::Integer(11_579)),
                ("itemID", PyRep::Integer(992_100_001)),
                ("price", PyRep::Real(500_000.0)),
                ("quantity", PyRep::Integer(1)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ]).await.expect("Seller lists Cloaking skillbook on Jita market");

        // 1. Buyer purchases NPC-seeded Leadership (3348) & Corp Management (3363) AND player-listed Cloaking (11579) via `marketProxy.PlaceBuyOrder`
        for (tid, max_price) in [(3348i64, 50_000.0), (3363, 50_000.0), (11_579, 500_000.0)] {
            let buy_rep = rpc(&ctx, &mut buyer, "marketProxy", "PlaceBuyOrder", &[
                PyRep::Integer(JITA_STN), PyRep::Integer(tid), PyRep::Real(max_price), PyRep::Integer(1), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0),
            ]).await.expect("Immediate PlaceBuyOrder for skillbook");
            assert_eq!(buy_rep.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
        }

        let find_book = |tid: u32| -> i64 {
            ctx.db.with_snapshot(|s| {
                s.items.values().find(|i| i.owner_id == buyer_cid && i.location_id == JITA_STN && i.flag == InventoryFlag::Hangar && i.type_id == tid).expect("bought book in hangar").item_id
            })
        };
        let (lead_iid, corp_iid, cloak_iid) = (find_book(3348), find_book(3363), find_book(11_579));

        // 2. Scalar `InjectSkillIntoBrain(itemID)` on Leadership (3348): consumes book, adds level 0 skill, emits OnServerSkillsChanged, logs event 36
        let _ = ctx.drain_notifications_for_char(buyer_cid);
        rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(lead_iid)]).await.expect("Scalar InjectSkillIntoBrain");
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&lead_iid)), "Injected skillbook must be consumed");
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&3348].level), 0);
        assert!(ctx.drain_notifications_for_char(buyer_cid).iter().any(|n| n.event == "OnServerSkillsChanged"));

        // 3. Batch V24 `InjectSkillIntoBrain([item1, item2])` on Corp Management (3363) + Cloaking (11579)
        rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(corp_iid), PyRep::Integer(cloak_iid)])]).await.expect("Batch InjectSkillIntoBrain");
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&corp_iid) && !s.items.contains_key(&cloak_iid)));
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&3363].level), 0);
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&11_579].level), 0);

        // 4. Negative rejections on `InjectSkillIntoBrain`: duplicate skill, non-skill item, unowned item
        place_hangar_item(&ctx, 992_100_010, 3348, buyer_cid, 1);
        let dup_err = rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_010)])]).await.unwrap_err();
        assert_eq!(dup_err.code, "SkillAlreadyInjected");
        assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&992_100_010)), "Failed duplicate injection must not consume book");

        place_hangar_item(&ctx, 992_100_011, 34, buyer_cid, 10); // Tritanium (not a skillbook)
        assert_eq!(rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_011)])]).await.unwrap_err().code, "InvalidSkillType");

        place_hangar_item(&ctx, 992_100_012, 26_252, 90_000_001, 1); // Owned by seller, not buyer
        assert!(rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_012)])]).await.is_err());

        // 5. Remote direct purchase (`feat:skills.direct-purchase-inject`) WHILE UNDOCKED IN SPACE:
        //    Jury Rigging (26252, base_price = 75,000 ISK -> 130% = 97,500 ISK); unseeded Astrogeology (3410) rejected!
        rpc(&ctx, &mut buyer, "stationSvc", "Undock", &[]).await.expect("Undock into space");
        assert!(buyer.station_id.is_none());
        let bal_before = ctx.db.get_character(buyer_cid).unwrap().balance.to_f64_isk();
        let direct_rep = rpc(&ctx, &mut buyer, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(26_252)])]).await.expect("Remote PurchaseSkills in space");
        let cost = direct_rep.dict_get("totalCostISK").and_then(|v| v.as_f64()).expect("totalCostISK");
        let expected_npc_ask = ctx.db.with_snapshot(|s| {
            s.market_orders.values().filter(|o| o.is_npc && !o.is_bid && o.type_id == 26_252).map(|o| o.price.to_f64_isk()).fold(f64::INFINITY, f64::min)
        });
        assert!((cost - expected_npc_ask * 1.30).abs() < 1e-2, "Remote PurchaseSkills must charge 130% of NPC sell price");
        let bal_after = ctx.db.get_character(buyer_cid).unwrap().balance.to_f64_isk();
        assert!((bal_before - bal_after - cost).abs() < 1e-2);
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&26_252].level), 0);

        let unseeded_skill_tid = ctx.db.with_snapshot(|s| {
            ctx.sde.types.values()
                .filter(|t| t.category_id == 16 && t.published && !s.skills[&buyer_cid].contains_key(&t.type_id))
                .find(|t| t.base_price <= 0.0 || !s.market_orders.values().any(|o| !o.is_bid && o.is_npc && o.vol_remaining > 0 && o.type_id == t.type_id))
                .expect("published drop-only / non-NPC-purchasable skill (e.g. Sleeper Technology 21789)")
                .type_id
        });
        let unseeded_err = rpc(&ctx, &mut buyer, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(unseeded_skill_tid as i64)])]).await.unwrap_err();
        assert_eq!(unseeded_err.code, "SkillUnavailableForPurchase");
    }

    #[tokio::test]
    async fn test_skill_injectors_large_small_alpha_and_nondiminishing() {
        // parity: feat:skills.skill-injector-large, feat:skills.skill-injector-small, feat:skills.alpha-injector, feat:skills.nondiminishing-injector
        eve_testkit::attest(&[
            "feat:skills.skill-injector-large",
            "feat:skills.skill-injector-small",
            "feat:skills.alpha-injector",
            "feat:skills.nondiminishing-injector",
        ]);
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        common::seed_test_nes_store_catalog(&ctx);

        // --- Part A: Daily Alpha Injector (`46375`, 50,000 SP, Alpha-only, 1 per 24h) ---
        let (mut alpha_sess, alpha_cid) = create_pilot_in_jita(&ctx, 2301, "alpha_inj_u", "secret", "Alpha Injector Pilot", 1, 11, 17).await;
        place_hangar_item(&ctx, 992_200_001, 46_375, alpha_cid, 3);
        place_hangar_item(&ctx, 992_200_002, 40_519, alpha_cid, 1);

        // Alpha clone cannot use Skill Extractor (`OmegaCloneRequired`)
        let alpha_ext_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "ExtractSkills", &[
            PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_002),
        ]).await.unwrap_err();
        assert_eq!(alpha_ext_err.code, "OmegaCloneRequired");

        assert_eq!(rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap(), PyRep::Integer(0));
        let daily_1 = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.expect("First Daily Alpha Injector");
        assert_eq!(daily_1, PyRep::Real(50_000.0));
        assert_eq!(ctx.db.get_character(alpha_cid).unwrap().free_skill_points, 50_000.0);

        let next_avail = rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap().as_i64().unwrap();
        assert!(next_avail > 0, "24h Daily Alpha Injector cooldown timestamp must be active");

        let cd_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.unwrap_err();
        assert_eq!(cd_err.code, "DailyAlphaInjectorCooldownActive");

        ctx.advance_secs(86_400 + 5);
        assert_eq!(rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap(), PyRep::Integer(0));
        let daily_2 = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.expect("Daily Alpha Injector after 24h");
        assert_eq!(daily_2, PyRep::Real(50_000.0));

        // Upgrade to Omega -> Daily Alpha Injector rejected with `AlphaCloneRequired`!
        upgrade_to_omega_via_nes(&ctx, &mut alpha_sess, alpha_cid, 992_200_003).await;
        ctx.advance_secs(86_400 + 5);
        let omega_daily_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.unwrap_err();
        assert_eq!(omega_daily_err.code, "AlphaCloneRequired");

        // --- Part B: Extraction + Large (40520) & Small (45635) Diminishing Brackets + Non-Diminishing (43630, 49704) ---
        let (mut omega_sess, omega_cid) = create_pilot_in_jita(&ctx, 2302, "omega_inj_u", "secret", "Omega Injector Pilot", 1, 11, 17).await;
        upgrade_to_omega_via_nes(&ctx, &mut omega_sess, omega_cid, 992_200_010).await;

        // Bracket 1 (< 5,000,000 total SP): Large = 500,000 SP, Small = 100,000 SP
        place_hangar_item(&ctx, 992_200_011, 40_520, omega_cid, 230);
        place_hangar_item(&ctx, 992_200_012, 45_635, omega_cid, 4);
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(500_000.0));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(100_000.0));

        // Below 5.5M allocated SP, ExtractSkills fails with `InsufficientSkillPoints`
        place_hangar_item(&ctx, 992_200_013, 40_519, omega_cid, 1);
        let low_sp_ext = rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
            PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
        ]).await.unwrap_err();
        assert_eq!(low_sp_ext.code, "InsufficientSkillPoints");

        // Inject to Bracket 2 (5M..50M total SP): 9 more Large Injectors -> > 5M total SP -> Large = 400k, Small = 80k!
        for _ in 0..9 {
            rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
        }
        let ch_b2 = ctx.db.get_character(omega_cid).unwrap();
        assert!((5_000_000.0..50_000_000.0).contains(&(ch_b2.skill_points + ch_b2.free_skill_points)));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(400_000.0));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(80_000.0));

        // Allocate >= 5.5M SP via real RPCs and extract 500k SP -> produces a new Large Skill Injector (40520) item!
        for (tid, iid) in [(11_579u32, 992_200_014i64), (21_603, 992_200_015), (4_411, 992_200_016)] {
            place_hangar_item(&ctx, iid, tid, omega_cid, 1);
            rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(iid)])]).await.unwrap();
        }
        rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
        for (tid, sp_needed) in [(3_426i64, 210_745i64), (25_739, 2_046_000), (11_579, 1_536_000), (21_603, 1_280_000), (4_411, 1_280_000)] {
            rpc(&ctx, &mut omega_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(tid), PyRep::Integer(sp_needed)]).await.unwrap();
        }
        // Non-extractable Cybernetics (3411) rejected (`SkillCannotBeExtracted`)
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
            PyRep::Dict(vec![(PyRep::Integer(3411), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
        ]).await.unwrap_err().code, "SkillCannotBeExtracted");

        let ext_rep = rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
            PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
        ]).await.expect("ExtractSkills produces Large Skill Injector");
        let produced_lsi_id = ext_rep.dict_get("injectorItemID").or_else(|| ext_rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap_or_else(|| {
            ctx.db.with_snapshot(|s| s.items.values().find(|i| i.owner_id == omega_cid && i.type_id == 40_520 && i.item_id != 992_200_011).unwrap().item_id)
        });
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&produced_lsi_id].type_id), 40_520);

        // Inject to Bracket 3 (50M..80M total SP): 112 Large Injectors (+44.8M SP) -> Large = 300k, Small = 60k!
        for _ in 0..112 {
            rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
        }
        let ch_b3 = ctx.db.get_character(omega_cid).unwrap();
        assert!((50_000_000.0..80_000_000.0).contains(&(ch_b3.skill_points + ch_b3.free_skill_points)));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(300_000.0));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(60_000.0));

        // Inject to Bracket 4 (>= 80M total SP): 99 Large Injectors (+29.7M SP) -> Large = 150k, Small = 30k!
        for _ in 0..99 {
            rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
        }
        let ch_b4 = ctx.db.get_character(omega_cid).unwrap();
        assert!(ch_b4.skill_points + ch_b4.free_skill_points >= 80_000_000.0);
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(produced_lsi_id)]).await.unwrap(), PyRep::Real(150_000.0));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(30_000.0));

        // Even at >= 80M total SP, Non-Diminishing Injectors (`43630` = 25,000 SP, `49704` = 256,000 SP via SDE attr 2461) grant 100% fixed SP!
        place_hangar_item(&ctx, 992_200_020, 43_630, omega_cid, 1);
        place_hangar_item(&ctx, 992_200_021, 49_704, omega_cid, 1);
        let PyRep::Dict(avail_nd) = rpc(&ctx, &mut omega_sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await.unwrap() else {
            panic!("Expected Dict from GetAvailableNonDiminishingInjections");
        };
        assert!(avail_nd.contains(&(PyRep::Integer(43_630), PyRep::Integer(1))));
        assert!(avail_nd.contains(&(PyRep::Integer(49_704), PyRep::Integer(1))));

        let _ = ctx.drain_notifications_for_char(omega_cid);
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_020)]).await.unwrap(), PyRep::Real(25_000.0));
        assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_021)]).await.unwrap(), PyRep::Real(256_000.0));
        assert!(ctx.drain_notifications_for_char(omega_cid).iter().any(|n| n.event == "OnNonDiminishingInjectionsUsed"));
    }
}

pub(crate) mod skills_alpha_5m_cap_direct {
    // Integration tests closing the 17 skills, attributes, character, account,
    // fitting, inventory, and social parity rows (`feat:*`, plus `plan:CH-2` in `plan_char_dogma_social_esi.rs`).


    use crate::common;
    use eve_db::{
        IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, MailingListRecord,
        MarketOrderRecord, TrainedSkillRecord, WorldRepository,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_string_field, encode_varint_field, AuthoritativeContext, EvePublicRequest, GoogleAny,
        GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::{cache_notifications::NotificationRecord, ServerContext};
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    const JITA_SYS: u32 = 30_000_142;
    const JITA_STN: i64 = 60_003_760;
    const MAURASI_SYS: u32 = 30_000_140; // 0.65 security (< 0.75)

    fn make_quasar_req(char_id: i64, user_id: i32, role_mask: u64, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicRequest {
        EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
            correlation_uuid: vec![1; 16],
            payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
            external_origin: "test".into(),
            application_instance_uuid: vec![2; 16],
            authoritative_context: Some(AuthoritativeContext::for_session_at(Some(user_id), Some(char_id as u32), None, role_mask, "tranquility", vec![3; 16], now)),
            trace_context: None,
        }
    }

    #[tokio::test]
    async fn test_skills_alpha_5m_cap_direct_purchase_weekly_sp_and_history() {
        // parity: feat:skills.alpha-5m-training-limit, feat:skills.direct-purchase-inject, feat:skills.omega-weekly-free-sp, feat:skills.skill-history-events
        eve_testkit::attest(&[
            "feat:skills.alpha-5m-training-limit",
            "feat:skills.direct-purchase-inject",
            "feat:skills.omega-weekly-free-sp",
            "feat:skills.skill-history-events",
        ]);

        let db_path = std::env::temp_dir().join(format!("evomu_skills_clos_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = ServerContext::with_repository(repo);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        common::pin_clock(&ctx);

        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = SessionState::new(9301);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

        // 1. feat:skills.alpha-5m-training-limit:
        // Alpha clone with 4.9M trained SP + 200k unallocated SP (>= 5.0M total SP) cannot start passive training,
        // an active queue is paused at 5M, unallocated SP can still be applied, and Omega status removes the limit.
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.clone_grade = 1; // Alpha
            ch.skill_points = 4_900_000.0;
            ch.free_skill_points = 200_000.0;
            snap.skills.entry(char_id).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 1, skill_points: 250.0 });
        });
        let q_err = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(2)])])], &kw, None).await.expect_err("Alpha with trained + unallocated SP >= 5M cannot start passive skill queue");
        assert_eq!(q_err.code, "AlphaCloneSpCapExceeded");

        // Even when passive queue is blocked by unallocated SP, ApplyFreeSkillPoints still works
        let applied = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Integer(1_165)], &kw, None).await.expect("ApplyFreeSkillPoints allowed while trained SP < 5M");
        assert_eq!(applied, PyRep::Real(1_415.0));

        // Active Alpha queue running across the 5M boundary pauses automatically on tick
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.skill_points = 4_990_000.0;
            ch.free_skill_points = 0.0;
        });
        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(3)])])], &kw, None).await.expect("SaveSkillQueue below 5M total SP");
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().skill_points = 4_999_995.0;
        });
        ctx.advance_secs(120);
        assert!(ctx.db.with_snapshot(|s| s.paused_skill_queues.contains(&char_id)), "Alpha skill queue must pause automatically once total SP reaches 5,000,000");

        // 2. feat:skills.omega-weekly-free-sp:
        // Refused for Alpha; granted once per week (15,000 SP) for Omega; refused on second claim in same week; refreshes after Tuesday 11:00 UTC downtime.
        let can_alpha = ctx.dispatch_rpc(&mut sess, "storeManager", None, "can_purchase_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("can_purchase_offer Alpha");
        assert_eq!(can_alpha, PyRep::Bool(false));
        let buy_alpha_err = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect_err("Alpha cannot claim Omega weekly free SP");
        assert_eq!(buy_alpha_err.code.to_ascii_lowercase(), "omegarequired");

        // Upgrade to Omega (clone_grade = 2) -> removes the 5M SP training limit and allows SaveSkillQueue & training completion above 5M SP!
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
            snap.paused_skill_queues.remove(&char_id);
        });
        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(3)])])], &kw, None).await.expect("Omega removes the 5M SP SaveSkillQueue limit");
        ctx.advance_secs(18_000);
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3300].level), 3, "Omega skill queue trains past 5M SP to completion");
        assert!(ctx.db.get_character(char_id).unwrap().skill_points > 5_000_000.0);

        let free_before = ctx.db.get_character(char_id).unwrap().free_skill_points;
        let claim1 = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("Omega claims weekly 15,000 free SP");
        assert_eq!(claim1.dict_get("freeSkillPointsGranted"), Some(&PyRep::Integer(15_000)));
        assert_eq!(ctx.db.get_character(char_id).unwrap().free_skill_points, free_before + 15_000.0);

        let claim_dup = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect_err("Second weekly SP claim in same week must be refused");
        assert_eq!(claim_dup.code.to_ascii_lowercase(), "weeklyofferalreadyclaimed");

        // Advance 7 days past next Tuesday 11:00 UTC downtime -> weekly window refreshes
        ctx.advance_secs(7 * 86_400);
        let claim2 = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("Omega claims weekly 15,000 free SP after Tuesday 11:00 UTC reset");
        assert_eq!(claim2.dict_get("freeSkillPointsGranted"), Some(&PyRep::Integer(15_000)));

        // 3. feat:skills.direct-purchase-inject:
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().remove(&3386);
            snap.skills.entry(char_id).or_default().remove(&3410);
            snap.market_orders.retain(|_, o| o.type_id != 3386 && o.type_id != 3410);
            snap.market_orders.insert(999_111_222, MarketOrderRecord {
                order_id: 999_111_222, owner_id: 1_000_035, station_id: 60_004_588, region_id: 10_000_030,
                type_id: 3386, is_bid: false, price: IskCents::from_f64_isk(100_000.0), vol_entered: 100,
                vol_remaining: 100, min_volume: 1, is_npc: true, escrow: IskCents(0),
            });
        });
        let bal_before = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
        let buy_skill = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(3386)])], &kw, None).await.expect("Direct purchase of NPC-seeded skill 3386 from any location");
        assert_eq!(buy_skill.dict_get("totalCostISK"), Some(&PyRep::Real(130_000.0)));
        let bal_after = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
        assert!((bal_before - bal_after - 130_000.0).abs() < 1e-2, "Must charge 130% of NPC price (130,000 ISK)");
        assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3386].level), 0);

        let unseeded_err = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(3410)])], &kw, None).await.expect_err("Non-NPC-seeded skill must be refused for direct purchase");
        assert_eq!(unseeded_err.code, "SkillUnavailableForPurchase");

        // 4. feat:skills.skill-history-events:
        // Exercise injection (36), training completion (37), extraction (39), and removal (38) in chronological order, persisted across relog.
        ctx.advance_secs(60);
        let extractor_id = 991_001_050;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().skill_points = 6_000_000.0;
            snap.skills.entry(char_id).or_default().insert(3392, TrainedSkillRecord { type_id: 3392, level: 5, skill_points: 512_000.0 });
            snap.items.insert(extractor_id, InventoryItemRecord { item_id: extractor_id, type_id: 40519, owner_id: char_id, location_id: JITA_STN, flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default() });
        });
        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(vec![(PyRep::Integer(3392), PyRep::Integer(500_000))]), PyRep::Integer(extractor_id)], &kw, None).await.expect("Extract 500k SP from 3392");

        ctx.advance_secs(60);
        let obs_skill_id = ctx.sde.types.values().find(|t| t.category_id == 16 && !t.published).map(|t| t.type_id).unwrap_or(999_999);
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(obs_skill_id, TrainedSkillRecord { type_id: obs_skill_id, level: 1, skill_points: 500.0 });
        });
        let (removed_cnt, _) = eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, char_id);
        assert_eq!(removed_cnt, 1);

        drop(ctx);
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = ServerContext::with_repository(repo2);
        let mut sess2 = SessionState::new(9302);
        common::login_pilot(&ctx2, &mut sess2, "demo", "demo-secret").await;
        ctx2.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
        let hist_rep = ctx2.dispatch_rpc(&mut sess2, "skillMgr2", None, "GetSkillHistory", &[PyRep::Integer(50)], &kw, None).await.expect("GetSkillHistory after relog");
        let PyRep::Rowset { rows, .. } = hist_rep else { panic!("expected Rowset") };
        assert!(rows.iter().any(|r| r[1] == PyRep::Integer(36) && r[2] == PyRep::Integer(3386)), "Skill injection event 36 must persist");
        assert!(rows.iter().any(|r| r[1] == PyRep::Integer(37) && r[2] == PyRep::Integer(3300)), "Skill training completion event 37 must persist");
        assert!(rows.iter().any(|r| r[1] == PyRep::Integer(39) && r[2] == PyRep::Integer(3392)), "Skill extraction event 39 must persist");
        assert!(rows.iter().any(|r| r[1] == PyRep::Integer(38) && r[2] == PyRep::Integer(obs_skill_id as i64)), "Skill removal event 38 must persist");
        assert!(rows.windows(2).all(|w| w[0][0].as_i64().unwrap() <= w[1][0].as_i64().unwrap()), "GetSkillHistory rows must be in chronological logDate order");
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_attributes_boosters_biology_contraband_remap_and_age_cohorts() {
        // parity: feat:attributes.booster-duration-biology, feat:attributes.booster-highsec-contraband, feat:attributes.remap-defaults-and-order, feat:character.character-age-cohorts
        eve_testkit::attest(&[
            "feat:attributes.booster-duration-biology",
            "feat:attributes.booster-highsec-contraband",
            "feat:attributes.remap-defaults-and-order",
            "feat:character.character-age-cohorts",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = SessionState::new(9303);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

        // 1. feat:attributes.remap-defaults-and-order:
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().neural = eve_db::NeuralAttributes::default();
        });
        let r_info = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None).await.unwrap();
        assert_eq!(r_info.dict_get("bonusRemaps"), Some(&PyRep::Integer(2)));
        let def_ch = ctx.db.get_character(char_id).unwrap();
        assert_eq!(def_ch.neural.charisma, 19);
        assert_eq!(def_ch.neural.intelligence, 20);

        // Out-of-bounds (< 17 or > 27) and wrong sum (!= 99) rejected
        assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(16), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(20)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(19), PyRep::Integer(28), PyRep::Integer(18), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.is_err());
        assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20)], &kw, None).await.is_err());

        // First remap consumes the normal timed remap (bonusRemaps remains 2!)
        let rm1 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
        assert_eq!(rm1.dict_get("bonusRemaps"), Some(&PyRep::Integer(2)), "Timed remap must be consumed before bonus remaps");
        let rm2 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(26), PyRep::Integer(22), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
        assert_eq!(rm2.dict_get("bonusRemaps"), Some(&PyRep::Integer(1)));
        let rm3 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(25), PyRep::Integer(23), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
        assert_eq!(rm3.dict_get("bonusRemaps"), Some(&PyRep::Integer(0)));
        assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(24), PyRep::Integer(24), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.is_err());

        // Advance 365 days -> normal timed remap replenishes!
        ctx.advance_secs(365 * 86_400 + 10);
        let rm4 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.expect("Normal timed remap replenishes after 365 days");
        assert_eq!(rm4.dict_get("intelligence"), Some(&PyRep::Integer(27)));

        // 2. feat:attributes.booster-duration-biology:
        let accel_item_id = 992_001_001;
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 1, skill_points: 250.0 }); // Science I
            sk.insert(3405, TrainedSkillRecord { type_id: 3405, level: 5, skill_points: 256_000.0 }); // Biology V
            snap.items.insert(accel_item_id, InventoryItemRecord {
                item_id: accel_item_id, type_id: 49539, owner_id: char_id, location_id: JITA_STN,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        });
        let attrs_before = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
        let use_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(accel_item_id)], &kw, None).await.expect("UseBooster cerebral accelerator");
        let dur_secs = use_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(0);
        let base_dur_secs = (ctx.sde.types.get(&49539).and_then(|t| t.default_attributes.get(&330).copied()).unwrap_or(1_800_000.0) / 1000.0).round() as i64;
        assert_eq!(dur_secs, base_dur_secs * 2, "Biology V must double booster duration to 2.0x");
        let bonus_int = ctx.sde.types.get(&49539).and_then(|t| t.default_attributes.get(&176).or_else(|| t.default_attributes.get(&165)).copied()).unwrap_or(6.0);
        let attrs_active = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
        assert!((attrs_active.1 - (attrs_before.1 + bonus_int)).abs() < 1e-4, "Active cerebral accelerator must add attribute bonus");

        // Advance past 2x duration -> booster expires and attribute bonus is removed
        ctx.advance_secs((dur_secs as u64) + 10);
        let attrs_expired = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
        assert!((attrs_expired.1 - attrs_before.1).abs() < 1e-4, "Expired cerebral accelerator bonus must be removed");

        // 3. feat:attributes.booster-highsec-contraband:
        let _ = ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &kw, None).await.expect("Undock in Jita");
        let ship_id = sess.ship_id.unwrap();
        let (contraband_id, legal_booster_id, active_booster_id) = (992_001_002, 992_001_003, 992_001_004);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(contraband_id, InventoryItemRecord { item_id: contraband_id, type_id: 9947, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 2, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(legal_booster_id, InventoryItemRecord { item_id: legal_booster_id, type_id: 2838, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(active_booster_id, InventoryItemRecord { item_id: active_booster_id, type_id: 9950, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default() });
        });
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(active_booster_id)], &kw, None).await.expect("Consume 9950");
        let bal_pre_scan = ctx.db.get_character(char_id).unwrap().balance.0;
        let _ = eve_server::services::combat_crimewatch::combat_npc_police::check_and_spawn_faction_police(&ctx, JITA_SYS).await;
        assert!(ctx.db.get_item(contraband_id).is_none(), "Unconsumed contraband booster 9947 in cargo must be confiscated in high-sec");
        assert!(ctx.db.get_item(legal_booster_id).is_some(), "Legal booster 2838 in cargo must not be confiscated");
        assert!(ctx.db.get_character(char_id).unwrap().balance.0 < bal_pre_scan, "Customs fine must be deducted from wallet");
        let active_boosters = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetBoosters", &[], &kw, None).await.unwrap();
        let PyRep::Dict(ab_dict) = active_boosters else { panic!("expected Dict") };
        assert!(!ab_dict.is_empty(), "Already-consumed active booster must remain active in high-sec");

        // 4. feat:character.character-age-cohorts:
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_u - 5 * 86_400; // 5 days old (< 30d)
        });
        let char_cohorts_new = ctx.dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
        assert!(format!("{char_cohorts_new:?}").contains("cohort_new_player"));
        let user_cohorts_new = ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCohortsForUser", &[PyRep::Integer(1)], &kw, None).await.unwrap();
        assert!(format!("{user_cohorts_new:?}").contains("cohort_new_player"));

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_u - 100 * 86_400; // 100 days old (> 30d)
        });
        let char_cohorts_vet = ctx.dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
        assert!(format!("{char_cohorts_vet:?}").contains("cohort_veteran"));
    }

    #[tokio::test]
    async fn test_account_character_transfer_csat_surveys_steam_epic_and_twitch_drops() {
        // parity: feat:account.character-transfer, feat:account.csat-surveys, feat:account.steam-epic-accounts, feat:account.twitch-drops
        eve_testkit::attest(&[
            "feat:account.character-transfer",
            "feat:account.csat-surveys",
            "feat:account.steam-epic-accounts",
            "feat:account.twitch-drops",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = SessionState::new(9304);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
        let now = ctx.now_unix();
        let gm_role = eve_machonet::service_roles::ROLE_GMH;

        // 1. feat:account.csat-surveys:
        let _ = eve_server::services::operations_manager::handle_survey_slash(&ctx, char_id, "/survey schedule 1001 86400 34:100");
        let _ = ctx.dispatch_rpc(&mut sess, "survey", None, "PerformSurveyChecks", &[], &kw, None).await.unwrap();
        let active_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "GetActiveSurveyID", &[], &kw, None).await.unwrap();
        assert_eq!(active_survey, PyRep::Integer(1001));
        let _ = eve_server::services::operations_manager::mark_survey_completed(&ctx, char_id, 1001);
        let claim_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "ClaimSurveyRewards", &[PyRep::Integer(1001)], &kw, None).await.expect("ClaimSurveyRewards first time");
        assert_eq!(claim_survey, PyRep::Bool(true));
        let dup_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "ClaimSurveyRewards", &[PyRep::Integer(1001)], &kw, None).await.expect_err("Duplicate survey claim must fail");
        assert_eq!(dup_survey.code, "SurveyAlreadyClaimed");

        // 2. feat:account.steam-epic-accounts:
        // Unprivileged callers cannot invoke admin/vendor entitlement delivery RPCs
        let mut epic_deliv_buf = Vec::new();
        encode_string_field(91, "epic_user_99", &mut epic_deliv_buf);
        encode_string_field(92, "epic_pack_001", &mut epic_deliv_buf);
        encode_varint_field(93, 44992, &mut epic_deliv_buf); // PLEX
        encode_varint_field(94, 50, &mut epic_deliv_buf);
        let unpriv_epic = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.vendors.epic.api.DeliverEntitlementRequest", epic_deliv_buf.clone(), now), None, false, now);
        assert_ne!(unpriv_epic.status_code, STATUS_OK, "Unprivileged client must not mint Epic entitlements");

        sess.role |= gm_role;
        let mut steam_link_buf = Vec::new();
        encode_string_field(91, "steam_76561198000000001", &mut steam_link_buf);
        encode_varint_field(92, 1, &mut steam_link_buf);
        let sl_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.steam.user.api.admin.LinkRequest", steam_link_buf, now), None, false, now);
        assert_eq!(sl_rsp.status_code, STATUS_OK);

        let mut steam_auth_buf = Vec::new();
        encode_string_field(91, "steam_76561198000000001", &mut steam_auth_buf);
        let sa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve_public.auth.steam.AuthenticateSteamRequest", steam_auth_buf, now), None, false, now);
        assert_eq!(sa_rsp.status_code, STATUS_OK);

        let ed_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.epic.api.DeliverEntitlementRequest", epic_deliv_buf, now), None, false, now);
        assert_eq!(ed_rsp.status_code, STATUS_OK);

        // 3. feat:account.twitch-drops:
        let mut tw_drop_buf = Vec::new();
        encode_string_field(91, "twitch_streamer_42", &mut tw_drop_buf);
        encode_string_field(92, "drop_campaign_2026_01", &mut tw_drop_buf);
        encode_varint_field(93, 34, &mut tw_drop_buf);
        encode_varint_field(94, 1000, &mut tw_drop_buf);
        let unlinked_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf.clone(), now), None, false, now);
        assert_ne!(unlinked_drop.status_code, STATUS_OK, "Unlinked Twitch account drop must fail");

        let mut tw_link_buf = Vec::new();
        encode_string_field(91, "twitch_streamer_42", &mut tw_link_buf);
        encode_varint_field(92, 1, &mut tw_link_buf);
        let tl_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.id.federate.twitch.api.CompleteLinkingRequest", tw_link_buf, now), None, false, now);
        assert_eq!(tl_rsp.status_code, STATUS_OK);

        let linked_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf.clone(), now), None, false, now);
        assert_eq!(linked_drop.status_code, STATUS_OK);
        let dup_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf, now), None, false, now);
        assert_ne!(dup_drop.status_code, STATUS_OK, "Duplicate Twitch drop ID must be rejected");

        let tokens = ctx.dispatch_rpc(&mut sess, "userSvc", None, "GetRedeemTokens", &[], &kw, None).await.unwrap();
        let PyRep::List(tok_list) = tokens else { panic!("expected List") };
        assert!(tok_list.len() >= 2, "Redeem queue must contain delivered Epic and Twitch rewards");

        // 4. feat:account.character-transfer:
        // Rejects wrong-owner caller, player corp, active industry job, and unpaid fee; succeeds when paid (1,000 PLEX), preserves assets, and enforces 10-day cooldown.
        sess.role = 0;
        let asset_id = 994_001_001;
        let mut xfer_buf = Vec::new();
        encode_varint_field(91, char_id as u64, &mut xfer_buf);
        encode_varint_field(92, 2, &mut xfer_buf); // destination user_id = 2

        // Wrong-owner caller (user_id = 2 trying to steal user_id = 1's character) is rejected
        let wrong_owner = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 2, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
        assert_ne!(wrong_owner.status_code, STATUS_OK, "Non-owner cannot transfer another account's character");

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().corporation_id = 98_000_001; // player corp
            snap.items.insert(asset_id, InventoryItemRecord { item_id: asset_id, type_id: 34, owner_id: char_id, location_id: JITA_STN, flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default() });
        });
        let corp_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
        assert_ne!(corp_err.status_code, STATUS_OK, "Character in player corp cannot be transferred");

        // Move to NPC corp, but seed an active industry job -> blocked!
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009; // NPC corp
            snap.industry_jobs.insert(9001, IndustryJobRecord { job_id: 9001, installer_id: char_id, facility_id: JITA_STN, blueprint_item_id: 1, blueprint_type_id: 687, activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds: 3600, start_unix: now });
        });
        let job_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
        assert_ne!(job_err.status_code, STATUS_OK, "Active industry job must block character transfer");

        // Clear industry job and fund 1,000 PLEX transfer fee -> succeeds and preserves character assets!
        ctx.db.with_mut_snapshot(|snap| {
            snap.industry_jobs.clear();
            snap.account_plex_vault.insert(1, 1_000);
        });
        let xfer_ok = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf, now), None, false, now);
        assert_eq!(xfer_ok.status_code, STATUS_OK);
        assert_eq!(ctx.db.get_character(char_id).unwrap().user_id, 2, "Character must now belong to destination account 2");
        assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0)), 0, "1,000 PLEX transfer fee must be consumed");
        assert_eq!(ctx.db.get_item(asset_id).unwrap().owner_id, char_id, "Character assets must remain owned by the transferred character");

        // Immediate transfer back within 10-day cooldown must fail
        sess.user_id = Some(2);
        ctx.db.with_mut_snapshot(|snap| { snap.account_plex_vault.insert(2, 1_000); });
        let mut xfer_back = Vec::new();
        encode_varint_field(91, char_id as u64, &mut xfer_back);
        encode_varint_field(92, 1, &mut xfer_back);
        let cd_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 2, 0, "eve.user.character.transfer.QueueRequest", xfer_back, now + 3600), None, false, now + 3600);
        assert_ne!(cd_err.status_code, STATUS_OK, "10-day character transfer cooldown must be enforced");
    }

    #[tokio::test]
    async fn test_strip_fitting_jetcan_capacity_despawn_and_secure_container_anchoring() {
        // parity: feat:fitting.strip-fitting, feat:inventory.jetcan-capacity-and-despawn, feat:inventory.secure-container-anchoring
        eve_testkit::attest(&[
            "feat:fitting.strip-fitting",
            "feat:inventory.jetcan-capacity-and-despawn",
            "feat:inventory.secure-container-anchoring",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let (c1, c2) = (90_000_001, 90_000_002);
        let mut s1 = SessionState::new(9305);
        common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
        let ship_id = s1.ship_id.unwrap_or(100_000_001);

        // 1. feat:fitting.strip-fitting:
        // Seed fitted module (MedSlot0 = 19), loaded charge (HiSlot0 = 27), drone in DroneBay (87), Rig (RigSlot0 = 92), and T3 Subsystem (SubSystemSlot0 = 125).
        let (mod_id, charge_id, drone_id, rig_id, subsys_id) = (993_001_001, 993_001_002, 993_001_003, 993_001_004, 993_001_005);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(mod_id, InventoryItemRecord { item_id: mod_id, type_id: 434, owner_id: c1, location_id: ship_id, flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(charge_id, InventoryItemRecord { item_id: charge_id, type_id: 185, owner_id: c1, location_id: ship_id, flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(drone_id, InventoryItemRecord { item_id: drone_id, type_id: 2454, owner_id: c1, location_id: ship_id, flag: InventoryFlag::DroneBay, quantity: 2, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(rig_id, InventoryItemRecord { item_id: rig_id, type_id: 31716, owner_id: c1, location_id: ship_id, flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(subsys_id, InventoryItemRecord { item_id: subsys_id, type_id: 45603, owner_id: c1, location_id: ship_id, flag: InventoryFlag::SubSystemSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        });
        let _ = ctx.drain_notifications_for_char(c1);
        let strip_rep = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None).await.expect("StripFitting docked");
        assert_eq!(strip_rep.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
        assert_eq!(ctx.db.get_item(mod_id).unwrap().flag, InventoryFlag::Hangar);
        assert_eq!(ctx.db.get_item(charge_id).unwrap().flag, InventoryFlag::Hangar);
        assert_eq!(ctx.db.get_item(drone_id).unwrap().flag, InventoryFlag::Hangar);
        assert_eq!(ctx.db.get_item(rig_id).unwrap().flag, InventoryFlag::RigSlot0, "Rigs must remain fitted during StripFitting");
        assert_eq!(ctx.db.get_item(subsys_id).unwrap().flag, InventoryFlag::SubSystemSlot0, "T3 Subsystems must remain fitted during StripFitting");
        let strip_notifs = ctx.drain_notifications_for_char(c1);
        assert!(strip_notifs.iter().any(|n| n.event == "OnItemsChanged"), "StripFitting must emit batch OnItemsChanged");
        assert!(strip_notifs.iter().any(|n| n.event == "OnModuleUnfitted"), "StripFitting must emit OnModuleUnfitted notifications");

        // Undock -> StripFitting in space must be rejected
        ctx.dispatch_rpc(&mut s1, "ship", None, "Undock", &[], &kw, None).await.unwrap();
        let strip_space_err = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None).await.expect_err("StripFitting in space must fail");
        assert_eq!(strip_space_err.code, "CannotStripFittingInSpace");

        // 2. feat:inventory.jetcan-capacity-and-despawn:
        // Jettisoning creates a 55,000 m³ jetcan, refuses scooping into a hold, despawns as soon as emptied, and otherwise despawns when its 2h (7,200s) V24 lifetime ends.
        let (ore_40k, ore_60k, ore_lifetime) = (993_001_010, 993_001_011, 993_001_012);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(sh) = snap.items.get_mut(&ship_id) { sh.type_id = 32880; } // Venture
            snap.items.insert(ore_40k, InventoryItemRecord { item_id: ore_40k, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 4_000_000, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(ore_60k, InventoryItemRecord { item_id: ore_60k, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 6_000_000, singleton: false, version: 1, ..Default::default() });
            snap.items.insert(ore_lifetime, InventoryItemRecord { item_id: ore_lifetime, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, version: 1, ..Default::default() });
        });
        let over_cap = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_60k)])], &kw, None).await.expect_err("60,000 m³ > 55,000 m³ jetcan capacity must fail");
        assert_eq!(over_cap.code, "CapacityExceeded");

        let jet_ok = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_40k)])], &kw, None).await.expect("40,000 m³ <= 55,000 m³ jetcan capacity must succeed");
        let PyRep::Tuple(jt) = jet_ok else { panic!("expected Tuple") };
        let jetcan_id = match &jt[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
        assert!(ctx.db.get_item(jetcan_id).is_some());

        // Jetcan can never be scooped into a hold
        assert!(ctx.dispatch_rpc(&mut s1, "ship", None, "Scoop", &[PyRep::Integer(jetcan_id)], &kw, None).await.is_err(), "Jettison container must never be scooped into a hold");

        // Emptying the jetcan despawns it immediately!
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&ore_60k);
            if let Some(it) = snap.items.get_mut(&ore_40k) { it.quantity = 100; }
        });
        ctx.dispatch_rpc(&mut s1, "invbroker", None, "Add", &[PyRep::Integer(ore_40k), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.expect("Loot last item from jetcan");
        assert!(ctx.db.get_item(jetcan_id).is_none(), "Emptied jetcan must despawn immediately");

        // Non-empty jetcan despawns when its 2-hour (7,200s) V24 lifetime expires
        ctx.advance_secs(181);
        let jet2 = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_lifetime)])], &kw, None).await.expect("Second Jettison after cooldown");
        let PyRep::Tuple(jt2) = jet2 else { panic!("expected Tuple") };
        let jetcan2_id = match &jt2[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
        assert!(ctx.db.get_item(jetcan2_id).is_some());
        ctx.advance_secs(7_205);
        assert!(ctx.db.get_item(jetcan2_id).is_none(), "Jetcan must despawn when its V24 lifetime ends");

        // 3. feat:inventory.secure-container-anchoring:
        let sec_cont_id = 993_001_020;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(c1).or_default().remove(&11584);
            snap.items.insert(sec_cont_id, InventoryItemRecord { item_id: sec_cont_id, type_id: 3467, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default() });
        });
        let no_skill = ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect_err("Missing Anchoring I must fail");
        assert_eq!(no_skill.code, "SkillMissing");

        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(c1).or_default().insert(11584, TrainedSkillRecord { type_id: 11584, level: 1, skill_points: 750.0 });
        });
        let hs08 = ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect_err("Anchoring secure container in Jita (0.9) must fail");
        assert_eq!(hs08.code, "CantAnchorInHighSec08");

        // Move to a <= 0.7 high-sec system and anchor
        let hs06_sys = ctx.sde.solar_systems.values().find(|s| s.security_status >= 0.50 && s.security_status < 0.74).map(|s| s.solar_system_id).unwrap_or(MAURASI_SYS);
        s1.solar_system_id = Some(hs06_sys);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&c1).unwrap().solar_system_id = hs06_sys;
            snap.items.get_mut(&ship_id).unwrap().location_id = hs06_sys as i64;
        });
        let maurasi = ctx.systems.get_or_spawn(hs06_sys);
        let _ = maurasi.spawn_ball_with_combat(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(587)).await;
        ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Anchor secure container in <= 0.7 system");

        // Anchored secure containers cannot be found with probes
        let scan_res = ctx.dispatch_rpc(&mut s1, "scanMgr", None, "RequestScans", &[PyRep::Real(149_597_870_700.0), PyRep::Real(100.0)], &kw, None).await.expect("RequestScans");
        assert!(!format!("{scan_res:?}").contains(&sec_cont_id.to_string()), "Anchored secure container must not be found with probes");

        // Advance 20 days, open container -> 30-day decay timer resets so advancing another 20 days (40 days total) keeps it alive!
        ctx.advance_secs(20 * 86_400);
        ctx.dispatch_rpc(&mut s1, "invbroker", None, "ValidateItemCanBeOpened", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Open anchored container");
        ctx.advance_secs(20 * 86_400);
        assert!(ctx.db.get_item(sec_cont_id).is_some(), "Opening anchored container must reset its 30-day inactivity decay timer");

        // Second pilot (c2) attacks c1's anchored secure container in <= 0.7 high-sec -> Criminal flag + CONCORD response
        let mut s2 = SessionState::new(9306);
        common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();
        let (ship2, gun2, ammo2) = (993_001_030, 993_001_031, 993_001_032);
        s2.solar_system_id = Some(hs06_sys);
        s2.station_id = None;
        s2.ship_id = Some(ship2);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&c2).unwrap().solar_system_id = hs06_sys;
            snap.characters.get_mut(&c2).unwrap().station_id = None;
            snap.characters.get_mut(&c2).unwrap().active_ship_id = ship2;
            snap.items.insert(ship2, InventoryItemRecord { item_id: ship2, type_id: 587, owner_id: c2, location_id: hs06_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(gun2, InventoryItemRecord { item_id: gun2, type_id: 2873, owner_id: c2, location_id: ship2, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(ammo2, InventoryItemRecord { item_id: ammo2, type_id: 185, owner_id: c2, location_id: ship2, flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
            snap.module_online_states.insert(gun2, true);
            snap.module_active_charges.insert(gun2, 185);
        });
        let _ = maurasi.spawn_ball_with_combat(Ball { id: ship2, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(587)).await;
        let _ = maurasi.add_target_lock(ship2, eve_destiny::TargetLock { target_id: sec_cont_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        ctx.dispatch_rpc(&mut s2, "dogmaIM", None, "Activate", &[PyRep::Integer(gun2), PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Attack anchored secure container in high-sec");
        let is_crim = ctx.crimewatch.read().unwrap().states.get(&c2).is_some_and(|st| st.is_criminal(ctx.now_unix()) && st.pending_concord_arrival.is_some());
        assert!(is_crim, "Attacking another pilot's anchored secure container in high-sec must trigger Criminal flag and CONCORD response");
    }

    #[tokio::test]
    async fn test_social_hyperlinks_mailbox_limits_and_wormhole_delayed_local() {
        // parity: feat:social.mail-mailbox-limits, feat:social.wormhole-local-chat-delayed
        eve_testkit::attest(&[
            "feat:social.mail-mailbox-limits",
            "feat:social.wormhole-local-chat-delayed",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();
        let (c1, c2) = (90_000_001, 90_000_002);
        let mut s1 = SessionState::new(9307);
        common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
        let mut s2 = SessionState::new(9308);
        common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();

        // 1. feat:social.mail-mailbox-limits:
        // (a) Mail body length > 8,000 characters is rejected with MailBodyTooLong.
        let long_body = "x".repeat(8_001);
        let body_err = ctx.dispatch_rpc(&mut s1, "mailMgr", None, "SendMail", &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("TooLong".into()), PyRep::String(long_body)], &kw, None).await.expect_err("Mail body > 8,000 chars must fail");
        assert_eq!(body_err.code, "MailBodyTooLong");

        // (b) Seed 3,000 mails for c2 (with mail #1 in a mailing list and mail #2 in Trash).
        // When mail #3,001 arrives, the newest 3,000 mails (2..=3,001) are kept and the oldest mail (#1) is hidden until Trash is emptied!
        ctx.db.with_mut_snapshot(|snap| {
            snap.mails.clear();
            snap.mail_timestamps.clear();
            let mut ml = MailingListRecord {
                list_id: 777, name: "TestList".into(), owner_id: c1, default_access: 1, default_role: 1, cost: 0,
                members: HashMap::new(), entity_access: Default::default(), mail_ids: [1].into_iter().collect(), welcome_mail: None,
            };
            ml.members.insert(c2, 1);
            snap.mailing_lists.insert(777, ml);
            for i in 1..=3_000 {
                snap.mails.push(eve_db::MailRecord { message_id: i, sender_id: c1, recipient_id: if i == 1 { 777 } else { c2 }, subject: format!("Mail {i}"), body: "Body".into() });
                snap.mail_timestamps.insert(i, 1_000_000 + (i as u64));
            }
        });
        ctx.dispatch_rpc(&mut s2, "mailMgr", None, "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(2)])], &kw, None).await.unwrap();
        let m3001 = ctx.dispatch_rpc(&mut s1, "mailMgr", None, "SendMail", &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("Mail 3001".into()), PyRep::String("Newest".into())], &kw, None).await.expect("3,001st mail must succeed");
        let m3001_id = m3001.as_i64().unwrap();

        let hdrs_before = ctx.dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[], &kw, None).await.unwrap();
        let PyRep::Rowset { rows: h_rows_before, .. } = hdrs_before else { panic!("expected Rowset") };
        let listed_before: Vec<i64> = h_rows_before.iter().map(|r| r[0].as_i64().unwrap()).collect();
        assert!(listed_before.contains(&m3001_id), "Newest 3,001st mail must be listed");
        assert!(!listed_before.contains(&1), "Oldest mail #1 must be hidden beyond the 3,000-mail window (counting mailing-list & Trash mails)");

        // EmptyTrash permanently deletes mail #2 -> oldest mail #1 re-enters the 3,000-mail window and becomes listed!
        ctx.dispatch_rpc(&mut s2, "mailMgr", None, "EmptyTrash", &[], &kw, None).await.unwrap();
        let hdrs_after = ctx.dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[], &kw, None).await.unwrap();
        let PyRep::Rowset { rows: h_rows_after, .. } = hdrs_after else { panic!("expected Rowset") };
        let listed_after: Vec<i64> = h_rows_after.iter().map(|r| r[0].as_i64().unwrap()).collect();
        assert!(listed_after.contains(&1), "Oldest mail #1 must become listed once a mail is permanently deleted");

        // (c) Notifications are capped at 500.
        {
            let mut n_recs = ctx.notification_records.write().unwrap();
            for i in 1..=510 {
                n_recs.push(NotificationRecord { notification_id: 800_000 + i, type_id: 10, sender_id: c1, receiver_id: c2, processed: false, created: 132_000_000_000_000_000 + i, data: "{}".into(), deleted: false, group_id: 1, interacted: false });
            }
        }
        let notifs = ctx.dispatch_rpc(&mut s2, "notificationMgr", None, "GetAllNotifications", &[], &kw, None).await.unwrap();
        let PyRep::ObjectEx { list: n_rows, .. } = notifs else { panic!("expected CRowset ObjectEx") };
        assert_eq!(n_rows.len(), 500, "Notifications returned by notificationMgr must be capped at 500");

        // 2. feat:social.wormhole-local-chat-delayed:
        let wh_chan = "solarsystemid2:31000001".to_string();
        s1.solar_system_id = Some(31_000_001);
        s2.solar_system_id = Some(31_000_001);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&c1).unwrap().solar_system_id = 31_000_001;
            snap.characters.get_mut(&c2).unwrap().solar_system_id = 31_000_001;
        });
        ctx.dispatch_rpc(&mut s1, "LSC", None, "JoinChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut s2, "LSC", None, "JoinChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();

        let m_init = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        let Some(PyRep::Rowset { rows: r0, .. }) = m_init.dict_get("members") else { panic!("expected Rowset") };
        assert!(r0.is_empty(), "No pilot is visible in wormhole Local before speaking");

        ctx.dispatch_rpc(&mut s1, "LSC", None, "SendMessage", &[PyRep::String(wh_chan.clone()), PyRep::String("Hello WH".into())], &kw, None).await.unwrap();
        let m_spoke = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        let Some(PyRep::Rowset { rows: r1, .. }) = m_spoke.dict_get("members") else { panic!("expected Rowset") };
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0][0], PyRep::Integer(c1));

        // Speaker c1 leaves wormhole while witness c2 is still in the system -> c1 remains listed!
        ctx.dispatch_rpc(&mut s1, "LSC", None, "LeaveChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        let m_c1_left = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        let Some(PyRep::Rowset { rows: r2, .. }) = m_c1_left.dict_get("members") else { panic!("expected Rowset") };
        assert_eq!(r2.len(), 1, "Speaker c1 must remain listed while witness c2 is still in the wormhole");

        // Witness c2 also leaves wormhole -> no witnesses remain, so c1 is pruned!
        ctx.dispatch_rpc(&mut s2, "LSC", None, "LeaveChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
        let m_all_left = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan)], &kw, None).await.unwrap();
        let Some(PyRep::Rowset { rows: r3, .. }) = m_all_left.dict_get("members") else { panic!("expected Rowset") };
        assert!(r3.is_empty(), "Speaker c1 must be removed once all witnesses have left the wormhole");
    }
}

pub(crate) mod make_session_clone_seeded_char {
    // Round-5 Track 3 Regression Tests (`#225`–`#237`):
    // Skills, Clones, Fitting, Containers, Fleet, Corporation Governance, Projects & Agents.


    use eve_db::{
        CharacterRecord, CorpApplicationRecord, CorporationRecord, FleetMemberRecord, FleetRecord,
        FleetRole, InventoryFlag, InventoryItemRecord, IskCents, SkillQueueEntry, TrainedSkillRecord,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::{service_roles, SessionState};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::{HashMap, HashSet};

    fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
        let mut s = SessionState::default();
        s.user_id = Some(1);
        s.character_id = Some(char_id);
        s.corporation_id = Some(corp_id);
        s.station_id = station_id;
        s.solar_system_id = Some(sys_id);
        s.ship_id = if ship_id > 0 { Some(ship_id) } else { None };
        s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
        s
    }

    fn clone_seeded_char(
        base: &CharacterRecord,
        char_id: i64,
        corp_id: i64,
        station_id: Option<i64>,
        sys_id: u32,
        ship_id: i64,
    ) -> CharacterRecord {
        let mut ch = base.clone();
        ch.character_id = char_id;
        ch.name = format!("R5 Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(50_000_000);
        ch.clone_grade = 2;
        ch
    }

    #[tokio::test]
    async fn r5_225_226_227_231_corp_wallet_transfer_applications_titles_votes_and_ads() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let station_id = 60_003_760_i64;
        let sys_id = 30_000_142_u32;
        let (corp_a, corp_b) = (2_100_001_i64, 2_100_002_i64);
        let (ceo_a, jr_acct, member_no_shares, ceo_b) = (96_001_001_i64, 96_001_002_i64, 96_001_003_i64, 96_001_004_i64);
        let kw = HashMap::new();

        ctx.db.with_mut_snapshot(|snap| {
            let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
            for &(cid, corp) in &[(ceo_a, corp_a), (jr_acct, corp_a), (member_no_shares, corp_a), (ceo_b, corp_b)] {
                snap.characters.insert(cid, clone_seeded_char(&base, cid, corp, Some(station_id), sys_id, 0));
            }
            let roles_a = HashMap::from([
                (ceo_a, u64::MAX),
                // Junior Accountant (512) + AccountCanTake1 (1 << 20), NO AccountCanTake2 (1 << 21)
                (jr_acct, 512 | (1u64 << 20)),
                (member_no_shares, 0),
            ]);
            let mut corp_rec_a = CorporationRecord {
                corporation_id: corp_a,
                name: "R5 Corp A".into(),
                ticker: "R5CA".into(),
                ceo_id: ceo_a,
                master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([ceo_a, jr_acct, member_no_shares]),
                member_roles: roles_a,
                ..Default::default()
            };
            corp_rec_a.ensure_wallet_divisions();
            corp_rec_a.set_division_balance(1, IskCents::from_isk(10_000_000));
            corp_rec_a.set_division_balance(2, IskCents::from_isk(5_000_000));
            snap.corporations.insert(corp_a, corp_rec_a);

            let mut corp_rec_b = CorporationRecord {
                corporation_id: corp_b,
                name: "R5 Corp B".into(),
                ticker: "R5CB".into(),
                ceo_id: ceo_b,
                members: HashSet::from([ceo_b]),
                member_roles: HashMap::from([(ceo_b, u64::MAX)]),
                ..Default::default()
            };
            corp_rec_b.ensure_wallet_divisions();
            snap.corporations.insert(corp_b, corp_rec_b);
        });

        // #225: GiveCashFromCorpAccount blocks self-transfer, enforces division take role, and debits source
        let mut sess_jr = make_session(jr_acct, corp_a, Some(station_id), sys_id, 0);
        let err_same = ctx
            .dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Real(1_000_000.0)], &kw, None)
            .await
            .expect_err("same division must be rejected");
        assert_eq!(err_same.code, "InvalidTransferDivision");

        let err_div2 = ctx
            .dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(2), PyRep::Integer(1), PyRep::Real(1_000_000.0)], &kw, None)
            .await
            .expect_err("jr accountant without take role on div 2 must be rejected");
        assert_eq!(err_div2.code, "CorpRoleDenied");

        ctx.dispatch_rpc(&mut sess_jr, "account", None, "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(2_000_000.0)], &kw, None)
            .await
            .expect("valid transfer from div 1 to div 2");

        ctx.db.with_snapshot(|snap| {
            let c = snap.corporations.get(&corp_a).unwrap();
            assert_eq!(c.division_balance(1), IskCents::from_isk(8_000_000));
            assert_eq!(c.master_wallet, IskCents::from_isk(8_000_000));
            assert_eq!(c.division_balance(2), IskCents::from_isk(7_000_000));
            let j_div1 = snap.corp_wallet_journals.get(&(corp_a, 1)).map_or(0, |v| v.len());
            let j_div2 = snap.corp_wallet_journals.get(&(corp_a, 2)).map_or(0, |v| v.len());
            assert_eq!(j_div1 + j_div2, 2, "must record debit and credit journal entries");
        });

        // #226: UpdateApplicationOffer rejects closed applications, non-accepted actions, and CEOs
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_b).unwrap().applications.insert(
                7001,
                CorpApplicationRecord { application_id: 7001, character_id: member_no_shares, corporation_id: corp_b, message: "Closed".into(), status: "Rejected".into(), created_unix: 1000 },
            );
            snap.corporations.get_mut(&corp_a).unwrap().applications.insert(
                7002,
                CorpApplicationRecord { application_id: 7002, character_id: ceo_b, corporation_id: corp_a, message: "CEO".into(), status: "Invited".into(), created_unix: 1000 },
            );
        });

        let mut sess_mem = make_session(member_no_shares, corp_a, Some(station_id), sys_id, 0);
        let err_closed = ctx
            .dispatch_rpc(&mut sess_mem, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7001), PyRep::String("accepted".into())], &kw, None)
            .await
            .expect_err("rejected app cannot be accepted");
        assert_eq!(err_closed.code, "ApplicationAlreadyClosed");

        let mut sess_ceo_b = make_session(ceo_b, corp_b, Some(station_id), sys_id, 0);
        let err_decline = ctx
            .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7002), PyRep::String("declined".into())], &kw, None)
            .await
            .expect_err("declined offer must not transfer corp");
        assert_eq!(err_decline.code, "InvalidApplicationAction");

        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_a).unwrap().applications.get_mut(&7002).unwrap().status = "Invited".into();
        });
        let err_ceo_join = ctx
            .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(7002), PyRep::String("accepted".into())], &kw, None)
            .await
            .expect_err("CEO cannot abandon corp via application offer");
        assert!(err_ceo_join.message.contains("CeoCannotLeaveCorp"));

        // #227: UpdateTitle strips ROLE_DIRECTOR (1) and title hangar grant is queued 24h; InsertVoteCase gated
        let mut sess_ceo_a = make_session(ceo_a, corp_a, Some(station_id), sys_id, 0);
        ctx.dispatch_rpc(&mut sess_ceo_a, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Vault Officer".into()), PyRep::Integer(1 | (1 << 13))], &kw, None)
            .await
            .expect("UpdateTitle");
        ctx.dispatch_rpc(&mut sess_ceo_a, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(member_no_shares), PyRep::String("".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None)
            .await
            .expect("UpdateMember title mask");

        ctx.db.with_snapshot(|snap| {
            let c = snap.corporations.get(&corp_a).unwrap();
            let effective = c.member_roles.get(&member_no_shares).copied().unwrap_or(0);
            assert_eq!(effective & 1, 0, "ROLE_DIRECTOR must be stripped from title");
            assert_eq!(effective & (1 << 13), 0, "Title-granted HangarCanTake1 must wait in 24h pending queue");
        });

        let err_vote_outsider = ctx
            .dispatch_rpc(&mut sess_ceo_b, "corpRegistry", None, "InsertVoteCase", &[PyRep::Integer(corp_a), PyRep::String("Hijack".into()), PyRep::String("".into()), PyRep::Integer(4)], &kw, None)
            .await
            .expect_err("outsider cannot insert vote case");
        assert_eq!(err_vote_outsider.code, "CorpRoleDenied");

        let err_vote_noshares = ctx
            .dispatch_rpc(&mut sess_mem, "corpRegistry", None, "InsertVoteCase", &[PyRep::Integer(corp_a), PyRep::String("NoShares".into()), PyRep::String("".into()), PyRep::Integer(0)], &kw, None)
            .await
            .expect_err("zero-share non-director cannot insert vote case");
        assert_eq!(err_vote_noshares.code, "CorpRoleDenied");

        // #231: corpRecProxy requires player corp + Personnel Manager / Director and scopes ad update to own corp
        let err_ad_role = ctx
            .dispatch_rpc(
                &mut sess_mem,
                "corpRecProxy",
                None,
                "CreateRecruitmentAd",
                &[PyRep::Integer(7), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Ad".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("T".into()), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .expect_err("member without personnel manager role cannot create ad");
        assert_eq!(err_ad_role.code, "CorpRoleDenied");

        let ad_rep = ctx
            .dispatch_rpc(
                &mut sess_ceo_a,
                "corpRecProxy",
                None,
                "CreateRecruitmentAd",
                &[PyRep::Integer(7), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Corp A Ad".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("Title A".into()), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .expect("CEO creates ad");
        let ad_id = ad_rep.dict_get("adID").and_then(|v| v.as_i64()).or_else(|| ad_rep.as_i64()).unwrap_or(1);

        let err_cross_ad = ctx
            .dispatch_rpc(
                &mut sess_ceo_b,
                "corpRecProxy",
                None,
                "UpdateRecruitmentAd",
                &[PyRep::Integer(ad_id), PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Hijacked".into()), PyRep::Integer(0), PyRep::List(vec![]), PyRep::String("Hijacked".into()), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .expect_err("other corp CEO cannot update corp_a ad");
        assert_eq!(err_cross_ad.code, "RecruitmentAdNotFound");
    }

    #[tokio::test]
    async fn r5_228_229_230_233_fleet_warp_conduit_hierarchy_trash_and_alsc_cache() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let station_id = 60_003_760_i64;
        let sys_jita = 30_000_142_u32;
        let sys_per = ctx.sde.solar_systems.values().find(|s| s.security_status > 0.0 && s.security_status < 0.45 && { let p = ctx.sde.solar_systems[&sys_jita].galactic_pos_ly; ((s.galactic_pos_ly[0]-p[0]).powi(2)+(s.galactic_pos_ly[1]-p[1]).powi(2)+(s.galactic_pos_ly[2]-p[2]).powi(2)).sqrt() < 7.0 }).unwrap().solar_system_id;
        let corp_id = 2_100_010_i64;
        let (boss_id, sc_id, target_id) = (96_002_001_i64, 96_002_002_i64, 96_002_003_i64);
        let (boss_ship, sc_ship, space_unpiloted_ship) = (890_002_001_i64, 890_002_002_i64, 890_002_003_i64);
        let (alsc_id, locked_trit) = (890_002_010_i64, 890_002_011_i64);
        let kw = HashMap::new();

        ctx.db.with_mut_snapshot(|snap| {
            let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
            snap.characters.insert(boss_id, clone_seeded_char(&base, boss_id, corp_id, None, sys_jita, boss_ship));
            snap.characters.insert(sc_id, clone_seeded_char(&base, sc_id, corp_id, None, sys_jita, sc_ship));
            snap.characters.insert(target_id, clone_seeded_char(&base, target_id, corp_id, Some(station_id), sys_jita, 0));
            for &(iid, tid, owner, loc, flag, qty) in &[
                (boss_ship, 23757_u32, boss_id, sys_jita as i64, InventoryFlag::None, 1_i32),
                (890_002_090, 16274_u32, boss_id, boss_ship, InventoryFlag::Cargo, 100_000_i32),
                (890_002_091, 56099_u32, boss_id, sys_per as i64, InventoryFlag::None, 1_i32),
                (sc_ship, 587_u32, sc_id, sys_jita as i64, InventoryFlag::None, 1_i32),
                (space_unpiloted_ship, 587_u32, boss_id, sys_jita as i64, InventoryFlag::None, 1_i32),
                (alsc_id, 17366_u32, boss_id, station_id, InventoryFlag::Hangar, 1_i32),
                (locked_trit, 34_u32, boss_id, alsc_id, InventoryFlag::Unlocked, 1_i32),
            ] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: owner,
                        location_id: loc,
                        flag,
                        quantity: qty,
                        singleton: flag != InventoryFlag::Unlocked && flag != InventoryFlag::Cargo,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            let boss_sk = snap.skills.entry(boss_id).or_default();
            for &stid in &[20533_u32, 21611, 21610, 24311, 24312] {
                boss_sk.insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
            }
            let mut fleet = FleetRecord { fleet_id: 5501, boss_id, members: vec![boss_id, sc_id], ..Default::default() };
            fleet.member_details.insert(
                boss_id,
                FleetMemberRecord {
                    character_id: boss_id,
                    role: FleetRole::FleetCommander,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_conduit_jumps: true,
                    accepts_regroup: true,
                    job: 2,
                    role_name: "Fleet Commander".into(),
                    join_time_unix: 1000,
                },
            );
            fleet.member_details.insert(
                sc_id,
                FleetMemberRecord {
                    character_id: sc_id,
                    role: FleetRole::SquadCommander,
                    wing_id: Some(1),
                    squad_id: Some(10),
                    takes_fleet_warp: true,
                    accepts_conduit_jumps: true,
                    accepts_regroup: true,
                    job: 0,
                    role_name: "Squad Commander".into(),
                    join_time_unix: 1000,
                },
            );
            snap.fleets.insert(5501, fleet);
            if let Some(ch) = snap.characters.get_mut(&boss_id) { ch.fleet_id = Some(5501); }
            if let Some(ch) = snap.characters.get_mut(&sc_id) { ch.fleet_id = Some(5501); }
        });

        let jita_handle = ctx.systems.get_or_spawn(sys_jita);
        for &(sid, tid, x) in &[(boss_ship, 23757_u32, 0.0), (sc_ship, 587_u32, 1_500.0), (space_unpiloted_ship, 587_u32, 500.0)] {
            let mut cs = ctx.build_type_combat_state(tid);
            if sid == sc_ship { cs.warp_scramble_strength = 2; }
            let _ = jita_handle
                .spawn_ball_with_combat(
                    Ball {
                        id: sid,
                        type_id: tid,
                        position: DVec3::new(x, 0.0, 0.0),
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 2.0,
                        mass: 1_000_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    cs,
                )
                .await;
        }

        // #228: WarpToMember rejects scrambled ship; beyonce.CmdGroupJumpToFleetDeployableBeacon rejects invalid system & skips scrambled ship
        let mut sess_sc = make_session(sc_id, corp_id, None, sys_jita, sc_ship);
        sess_sc.fleet_id = Some(5501);
        let err_scram = ctx
            .dispatch_rpc(&mut sess_sc, "fleetObjectHandler", None, "WarpToMember", &[PyRep::Integer(boss_id), PyRep::Real(0.0)], &kw, None)
            .await
            .expect_err("scrambled member cannot fleet warp");
        assert_eq!(err_scram.code, "WarpScrambled");

        let mut sess_boss = make_session(boss_id, corp_id, None, sys_jita, boss_ship);
        sess_boss.fleet_id = Some(5501);
        let err_bad_sys = ctx
            .dispatch_rpc(
                &mut sess_boss,
                "beyonce",
                None,
                "CmdGroupJumpToFleetDeployableBeacon",
                &[PyRep::Integer(boss_id), PyRep::Integer(99_999_999), PyRep::Integer(890_002_091)],
                &kw,
                None,
            )
            .await
            .expect_err("invalid solar system must fail");
        assert_eq!(err_bad_sys.code, "InvalidJumpDestination");

        let cj_res = ctx
            .dispatch_rpc(
                &mut sess_boss,
                "beyonce",
                None,
                "CmdGroupJumpToFleetDeployableBeacon",
                &[PyRep::Integer(boss_id), PyRep::Integer(sys_per as i64), PyRep::Integer(890_002_091)],
                &kw,
                None,
            )
            .await
            .expect("CmdGroupJumpToFleetDeployableBeacon");
        assert_eq!(cj_res.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_per as i64)));
        assert_eq!(
            ctx.db.with_snapshot(|snap| snap.characters.get(&sc_id).unwrap().solar_system_id),
            sys_jita,
            "scrambled sc_id must be excluded from conduit jump"
        );
        assert_eq!(
            ctx.db.with_snapshot(|snap| snap.items.get(&boss_ship).unwrap().location_id),
            sys_per as i64,
            "boss ship item location must move to destination solar system"
        );

        // #230: Squad Commander cannot invite a Fleet Commander (role 1), and AcceptInvite requires pending invite
        let err_rank = ctx
            .dispatch_rpc(&mut sess_sc, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(target_id), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("SC cannot invite to FC role");
        assert_eq!(err_rank.code, "FleetPermissionDenied");

        let mut sess_target = make_session(target_id, corp_id, Some(station_id), sys_jita, 0);
        let err_no_inv = ctx
            .dispatch_rpc(&mut sess_target, "fleetObjectHandler", None, "AcceptInvite", &[], &kw, None)
            .await
            .expect_err("AcceptInvite without invite must fail");
        assert_eq!(err_no_inv.code, "InviteExpired");

        // #229: TrashItems blocks trashing unpiloted ships in space
        let inv_oid = ctx.bind_object(&sess_sc, "invbroker", sys_jita as i64).expect("bind invbroker");
        let err_trash_ship = ctx
            .dispatch_rpc(&mut sess_sc, "invbroker", Some(inv_oid), "TrashItems", &[PyRep::List(vec![PyRep::Integer(space_unpiloted_ship)])], &kw, None)
            .await
            .expect_err("cannot trash unpiloted ship in space");
        assert_eq!(err_trash_ship.code, "DeleteFailed");

        // #233: SetPassword invalidates unlocked_containers cache; ALSCLockItems requires co-location
        let err_remote_lock = ctx
            .dispatch_rpc(&mut sess_boss, "invbroker", None, "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(locked_trit)])], &kw, None)
            .await
            .expect_err("undocked boss in another system cannot lock station ALSC items");
        assert_eq!(err_remote_lock.code, "ColocationRequired");

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&boss_id).unwrap();
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_jita;
        });
        sess_boss.station_id = Some(station_id);
        sess_boss.solar_system_id = Some(sys_jita);
        ctx.dispatch_rpc(&mut sess_boss, "invbroker", None, "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("oldpass".into())], &kw, None)
            .await
            .expect("SetPassword oldpass");
        ctx.dispatch_rpc(
            &mut sess_target,
            "invbroker",
            None,
            "ALSCLockItems",
            &[PyRep::Integer(alsc_id), PyRep::List(vec![]), PyRep::String("oldpass".into())],
            &kw,
            None,
        )
        .await
        .ok();
        // Unlock container for target_id by taking with password="oldpass" or via SetPassword by target, then boss changes password
        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".to_string(), PyRep::String("oldpass".into()));
        let st_inv_t = ctx.bind_object(&sess_target, "invbroker", station_id).expect("bind station inv");
        let _ = ctx.dispatch_rpc(&mut sess_target, "invbroker", Some(st_inv_t), "Add", &[PyRep::Integer(locked_trit), PyRep::Integer(station_id)], &kw_pw, None).await;
        // Now boss changes password -> must invalidate target_id's unlocked_containers entry
        ctx.dispatch_rpc(&mut sess_boss, "invbroker", None, "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("newpass".into()), PyRep::String("oldpass".into())], &kw, None)
            .await
            .expect("SetPassword newpass");
        let err_need_pw = ctx
            .dispatch_rpc(&mut sess_target, "invbroker", None, "ALSCLogGet", &[PyRep::Integer(alsc_id)], &kw, None)
            .await
            .expect_err("SetPassword must invalidate cached unlocked_containers session");
        assert_eq!(err_need_pw.code, "ALSCPasswordNeededToViewAuditLog");
    }

    #[tokio::test]
    async fn r5_232_234_235_236_237_projects_skills_fitting_clones_and_agents() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let station_id = 60_003_760_i64;
        let sys_id = 30_000_142_u32;
        let corp_id = 2_100_020_i64;
        let pilot_id = 96_003_001_i64;
        let (ship_id, cap_id, mod_locked, extractor_id) = (890_003_001_i64, 890_003_002_i64, 890_003_003_i64, 890_003_004_i64);
        let kw = HashMap::new();

        ctx.db.with_mut_snapshot(|snap| {
            let base = snap.characters.get(&90_000_001).cloned().expect("seeded");
            let mut ch = clone_seeded_char(&base, pilot_id, corp_id, Some(station_id), sys_id, ship_id);
            ch.free_skill_points = 50_000.0;
            snap.characters.insert(pilot_id, ch);

            let mut corp = CorporationRecord {
                corporation_id: corp_id,
                name: "R5 Proj Corp".into(),
                ticker: "R5PC".into(),
                ceo_id: 90_000_001,
                master_wallet: IskCents::from_isk(20_000_000),
                members: HashSet::from([pilot_id]),
                member_roles: HashMap::from([(pilot_id, eve_db::corp_roles::ROLE_PROJECT_MANAGER | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0])]),
                ..Default::default()
            };
            corp.ensure_wallet_divisions();
            corp.set_division_balance(1, IskCents::from_isk(20_000_000));
            corp.set_division_balance(2, IskCents::from_isk(20_000_000));
            snap.corporations.insert(corp_id, corp);
            snap.corp_governance.entry(corp_id).or_default().member_account_keys.insert(pilot_id, 1001);

            for &(iid, tid, flag) in &[
                (ship_id, 587_u32, InventoryFlag::Hangar),
                (cap_id, 670_u32, InventoryFlag::Hangar),
                (mod_locked, 440_u32, InventoryFlag::Locked),
                (extractor_id, 40519_u32, InventoryFlag::Hangar),
            ] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: pilot_id,
                        location_id: station_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            let sk = snap.skills.entry(pilot_id).or_default();
            sk.insert(24242, TrainedSkillRecord { type_id: 24242, level: 3, skill_points: 40_000.0 });
            sk.insert(33399, TrainedSkillRecord { type_id: 33399, level: 3, skill_points: 40_000.0 });
            sk.insert(3300, TrainedSkillRecord { type_id: 3300, level: 1, skill_points: 250.0 });
            let now = ctx.now_unix();
            snap.skill_queues.insert(
                pilot_id,
                vec![
                    SkillQueueEntry { type_id: 3300, target_level: 2, start_sp: 250.0, end_sp: 1415.0, duration_seconds: 600.0, started_at_unix: Some(now) },
                    SkillQueueEntry { type_id: 24242, target_level: 4, start_sp: 40_000.0, end_sp: 90_510.0, duration_seconds: 1800.0, started_at_unix: None },
                ],
            );
        });

        let mut sess = make_session(pilot_id, corp_id, Some(station_id), sys_id, ship_id);
        let (mut create_goal, mut curr_b, mut isk_b, mut pool_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        eve_proto::quasar::encode_string_field(1, "Div2 Exploit", &mut create_goal);
        eve_proto::quasar::encode_varint_field(3, 10, &mut create_goal);
        eve_proto::quasar::encode_varint_field(1, eve_proto::quasar::encode_zigzag64(100_000), &mut curr_b);
        eve_proto::quasar::encode_bytes_field(1, &curr_b, &mut isk_b);
        eve_proto::quasar::encode_bytes_field(2, &isk_b, &mut pool_b);
        eve_proto::quasar::encode_bytes_field(8, &pool_b, &mut create_goal);
        let now_q = ctx.now_unix();
        let build_q = |role_mask: u64, payload: Vec<u8>| eve_proto::quasar::EvePublicRequest {
            issued: Some(eve_proto::quasar::GoogleTimestamp { seconds: now_q as i64, nanos: 0 }),
            correlation_uuid: vec![0x32; 16],
            payload: Some(eve_proto::quasar::GoogleAny {
                type_url: "type.googleapis.com/eve_public.goal.api.CreateRequest".into(),
                value: payload,
            }),
            external_origin: "test".into(),
            application_instance_uuid: vec![0x33; 16],
            authoritative_context: Some(eve_proto::quasar::AuthoritativeContext::for_session_at(
                Some(1), Some(pilot_id as u32), Some(corp_id as u32), role_mask, "tranquility", vec![0x34; 16], now_q,
            )),
            trace_context: None,
        };

        // #232a (R6-P0-01): Empty corp.member_roles + unprivileged member -> rejected on both RPC & Quasar
        sess.corp_role = 0;
        ctx.db.with_mut_snapshot(|s| {
            s.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
            s.corp_governance.entry(corp_id).or_default().member_account_keys.insert(pilot_id, 1000);
        });
        let goal_args = [PyRep::String("Steal".into()), PyRep::Integer(10), PyRep::Real(10_000.0), PyRep::String("DestroyNPCShips".into()), PyRep::Bool(false), PyRep::Integer(1)];
        assert_eq!(ctx.dispatch_rpc(&mut sess, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
        assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &build_q(0, create_goal.clone()), None, false, now_q).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

        // #232b (R6-P0-01): ROLE_ACCOUNTANT cannot create corp projects or bypass wallet take role
        sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;
        ctx.db.with_mut_snapshot(|s| { s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(pilot_id, sess.corp_role); });
        assert_eq!(ctx.dispatch_rpc(&mut sess, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
        assert_eq!(ctx.quasar_gateway.handle_public_request(&sess, &build_q(sess.corp_role, create_goal.clone()), None, false, now_q).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);

        // #232c (R6-P0-01): ROLE_PROJECT_MANAGER with ZERO wallet-take bits is rejected on Master Wallet (wallet_div = 1)
        sess.corp_role = eve_db::corp_roles::ROLE_PROJECT_MANAGER;
        ctx.db.with_mut_snapshot(|s| { s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(pilot_id, sess.corp_role); });
        assert_eq!(ctx.dispatch_rpc(&mut sess, "corporationSvc", None, "CreateGoalSubmit", &goal_args, &kw, None).await.unwrap_err().code, "CorpRoleDenied");
        let rsp_notake = ctx.quasar_gateway.handle_public_request(&sess, &build_q(sess.corp_role, create_goal.clone()), None, false, now_q);
        assert_eq!(rsp_notake.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1)), IskCents::from_isk(20_000_000));

        // #232d: ROLE_PROJECT_MANAGER | ROLE_ACCOUNT_CAN_TAKE[0] rejected on div 2, allowed on div 1
        sess.corp_role = eve_db::corp_roles::ROLE_PROJECT_MANAGER | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];
        ctx.db.with_mut_snapshot(|s| {
            s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(pilot_id, sess.corp_role);
            s.corp_governance.entry(corp_id).or_default().member_account_keys.insert(pilot_id, 1001);
        });
        let rsp_div2 = ctx.quasar_gateway.handle_public_request(&sess, &build_q(sess.corp_role, create_goal.clone()), None, false, now_q);
        assert_eq!(rsp_div2.status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
        assert!(rsp_div2.status_message.contains("CorpRoleDenied"), "unexpected message: {}", rsp_div2.status_message);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(2)), IskCents::from_isk(20_000_000));

        ctx.db.with_mut_snapshot(|snap| {
            snap.corp_governance.entry(corp_id).or_default().member_account_keys.insert(pilot_id, 1000);
        });
        let rsp_div1 = ctx.quasar_gateway.handle_public_request(&sess, &build_q(sess.corp_role, create_goal), None, false, now_q);
        assert_eq!(rsp_div1.status_code, eve_server::quasar_gateway::STATUS_OK, "{}", rsp_div1.status_message);
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1)), IskCents::from_isk(19_900_000));

        // #234: ApplyFreeSkillPoints completing head starts next queue entry
        ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Integer(2_000)], &kw, None)
            .await
            .expect("ApplyFreeSkillPoints");
        ctx.db.with_snapshot(|snap| {
            let q = snap.skill_queues.get(&pilot_id).unwrap();
            assert_eq!(q.first().map(|e| e.type_id), Some(24242));
            assert!(q.first().and_then(|e| e.started_at_unix).is_some(), "new queue head must have started_at_unix set");
        });

        // #235: FitShips skips ALSC-locked modules (InventoryFlag::Locked)
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "FitShips",
                &[
                    PyRep::List(vec![PyRep::Integer(ship_id)]),
                    PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(InventoryFlag::MedSlot0 as i64), PyRep::Integer(1)])]),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&mod_locked).unwrap().flag), InventoryFlag::Locked);

        // #236: InstallCloneInStation journals JumpCloneInstallationFee, and NPC same-station CloneJump charges 900k fee
        ctx.dispatch_rpc(&mut sess, "jumpCloneSvc", None, "InstallCloneInStation", &[], &kw, None)
            .await
            .expect("InstallCloneInStation");
        let jc_id = ctx.db.with_snapshot(|snap| {
            assert!(snap.wallet_journal.iter().any(|j| j.character_id == pilot_id && j.entry_type == "JumpCloneInstallationFee"));
            snap.jump_clones.get(&pilot_id).unwrap()[0].jump_clone_id
        });

        ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&pilot_id).unwrap().active_ship_id = cap_id; });
        sess.ship_id = Some(cap_id);
        let bal_before_swap = ctx.db.with_snapshot(|snap| snap.characters.get(&pilot_id).unwrap().balance);
        ctx.dispatch_rpc(&mut sess, "jumpCloneSvc", None, "CloneJump", &[PyRep::Integer(jc_id)], &kw, None)
            .await
            .expect("NPC same-station CloneJump");
        let bal_after_swap = ctx.db.with_snapshot(|snap| snap.characters.get(&pilot_id).unwrap().balance);
        assert_eq!(bal_before_swap.0 - bal_after_swap.0, 90_000_000);

        // #237: StartResearch rejects non-science skill & non-colocated caller; BuyDatacores journals DatacoreFee
        let rd_agent_id = ctx.sde.agents.values().find(|a| a.level == 1 && a.agent_type_id == 4 && a.station_id.is_some() && a.research_skills.contains(&11448)).map(|a| a.agent_id).expect("R&D agent");
        let err_not_coloc = ctx
            .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11448)], &kw, None)
            .await
            .expect_err("non-colocated R&D start must fail");
        assert_eq!(err_not_coloc.code, "AgentNotCoLocated");

        let (rd_sys, rd_sta, rd_corp) = ctx.sde.agents.get(&rd_agent_id).map(|a| (a.solar_system_id.unwrap(), a.station_id.unwrap(), a.corporation_id)).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&pilot_id).unwrap();
            ch.solar_system_id = rd_sys;
            ch.station_id = Some(rd_sta);
            ch.standings.insert(rd_corp, 5.0);
            for (sid, lvl, sp) in [(3402, 5, 256_000.0), (3392, 5, 256_000.0), (11448, 4, 45_255.0)] {
                snap.skills.entry(pilot_id).or_default().insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: sp });
            }
        });
        sess.solar_system_id = Some(rd_sys);
        sess.station_id = Some(rd_sta);

        let err_bad_skill = ctx
            .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(3300)], &kw, None)
            .await
            .expect_err("non-science skill must be rejected for R&D");
        assert_eq!(err_bad_skill.code, "InvalidResearchSkill");

        ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(12), PyRep::Integer(rd_agent_id), PyRep::Integer(11448)], &kw, None)
            .await
            .expect("StartResearch");
        ctx.advance_secs(5 * 86_400);
        ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(14), PyRep::Integer(rd_agent_id), PyRep::Integer(1)], &kw, None)
            .await
            .expect("BuyDatacores");
        assert!(ctx.db.with_snapshot(|snap| snap.wallet_journal.iter().any(|j| j.character_id == pilot_id && j.entry_type == "DatacoreFee")));
    }
}
