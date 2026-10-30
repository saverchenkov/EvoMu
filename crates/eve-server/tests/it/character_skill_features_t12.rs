//! Integration tests for character and skill features (`feat:character.medals-display`,
//! `feat:character.online-status`, `feat:character.safe-logoff`, `feat:skills.nondiminishing-injector`).

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::{record_pvp_aggression_and_renew_logoff_timer, unix_to_filetime};
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
