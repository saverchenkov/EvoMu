//! Live MachoNet + Quasar E2E integration tests for chat bans, cluster shutdown warnings,
//! bound container passwords, target multievents, in-space kill notifications, Rorqual Excavator
//! drones, mining goals, and mining market supply chains.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    EvePublicRequest, EvePublicResponse, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const NEW_CALDARI_SYS: u32 = 30_000_145;
const JOSAMETO_SYS: u32 = 30_000_156;
const JOSAMETO_STATION: i64 = 60_001_825;
const GATE_JOSAMETO_TO_NC: i64 = 50_013_760;
const GATE_NC_FROM_JOSAMETO: i64 = 50_013_759;
const GATE_NC_TO_JITA: i64 = 50_002_198;
const GATE_JITA_FROM_NC: i64 = 50_001_250;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn bound_rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, oid: u64, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, Some(oid), method, args, &HashMap::new(), None).await
}

async fn slash(ctx: &ServerContext, gm: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, gm, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await.expect(cmd)
}

async fn login_select(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64, gm_role: bool) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    if gm_role {
        sess.role = service_roles::ROLE_GM_DEFAULT;
    }
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    if gm_role {
        sess.role = service_roles::ROLE_GM_DEFAULT;
    }
    ctx.clock().advance_secs(11);
    sess
}

async fn give_item(ctx: &ServerContext, gm: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    slash(ctx, gm, &format!("/giveitem {type_id} {qty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).expect("itemID")
}

async fn give_skill(ctx: &ServerContext, gm: &mut SessionState, skill_id: u32, level: u8) {
    let _ = slash(ctx, gm, &format!("/giveskill {skill_id} {level}")).await;
}

async fn add_inv(ctx: &ServerContext, sess: &mut SessionState, item_id: i64, dest_id: i64, flag: InventoryFlag) {
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(item_id), PyRep::Integer(dest_id), PyRep::Integer(flag as i64)])
        .await
        .expect("invbroker.Add");
}

fn drain_events(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    out.extend(ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)));
    out
}

fn quasar_call(ctx: &ServerContext, sess: &SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        ..Default::default()
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, ctx.now_unix())
}

fn local_chat_payload(msg: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_string_field(1, msg, &mut buf);
    buf
}

async fn warp_to_item(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, target_id: i64) {
    let h = ctx.systems.get_or_spawn(sys_id);
    let ship_id = sess.ship_id.unwrap();
    for _ in 0..6 {
        if let Ok(Some(cs)) = h.get_combat_state(ship_id).await {
            if cs.capacitor >= cs.max_capacitor * 0.99 {
                break;
            }
        }
        ctx.clock().advance_secs(30);
        h.step_manual(30.0).await.expect("recharge capacitor");
    }
    rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(target_id), PyRep::Real(0.0)])
        .await
        .expect("CmdWarpToStuff");
    let mut seen_warp = false;
    for i in 0..150 {
        ctx.clock().advance_secs(10);
        h.step_manual(10.0).await.expect("step warp");
        if let Some((_, b)) = h.get_ball_snapshot(ship_id).await.unwrap() {
            let warping = matches!(b.mode, eve_destiny::MovementMode::Warp { .. });
            if warping {
                seen_warp = true;
            } else if seen_warp || i > 24 {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_live_chat_bans_cluster_shutdown_and_bound_container_passwords() {
    // parity: notify:OnCharacterPermanentlyBanned, notify:OnCharacterTemporarilyBanned, notify:OnCharacterUnbanned, notify:OnClusterShutdownInitiated, notify:OnPasswordChanged, notify:OnInvalidateCacheForLocations
    eve_testkit::attest(&[
        "notify:OnCharacterPermanentlyBanned",
        "notify:OnCharacterTemporarilyBanned",
        "notify:OnCharacterUnbanned",
        "notify:OnClusterShutdownInitiated",
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut gm = login_select(&ctx, 97_101, "demo", "demo-secret", PILOT_1, true).await;
    let mut p2 = login_select(&ctx, 97_102, "other", "other-secret", PILOT_2, false).await;
    let _ = (drain_events(&ctx, &mut gm, PILOT_1), drain_events(&ctx, &mut p2, PILOT_2));

    // 1. Unified permanent chat ban -> blocks Quasar BroadcastMessageRequest & LSC SendMessage -> unban restores both
    slash(&ctx, &mut gm, &format!("/chatban {PILOT_2} perm")).await;
    let perm_ev: Vec<_> = drain_events(&ctx, &mut p2, PILOT_2).into_iter().filter(|(e, _)| e == "OnCharacterPermanentlyBanned").map(|(_, a)| a).collect();
    assert_eq!(perm_ev, vec![Vec::<PyRep>::new()]);
    assert_ne!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", local_chat_payload("blocked")).status_code, STATUS_OK);
    let lsc_args = [PyRep::String("solarsystemid2:30000142".into()), PyRep::String("msg".into())];
    assert_eq!(rpc(&ctx, &mut p2, "LSC", "SendMessage", &lsc_args).await.unwrap_err().code, "CharacterChatBanned");

    slash(&ctx, &mut gm, &format!("/chatunban {PILOT_2}")).await;
    let unban_ev: Vec<_> = drain_events(&ctx, &mut p2, PILOT_2).into_iter().filter(|(e, _)| e == "OnCharacterUnbanned").map(|(_, a)| a).collect();
    assert_eq!(unban_ev, vec![Vec::<PyRep>::new()]);
    assert_eq!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", local_chat_payload("restored")).status_code, STATUS_OK);
    rpc(&ctx, &mut p2, "LSC", "SendMessage", &lsc_args).await.expect("LSC after /chatunban");

    // 2. Temporary chat ban -> emits exact FILETIME expiry, blocks both paths, lapses automatically after advance_secs
    let expected_expiry_ft = ctx.now_blueos_filetime_ticks() + 1_800 * 10_000_000;
    slash(&ctx, &mut gm, &format!("/chatban {PILOT_2} 1800")).await;
    let temp_ev: Vec<_> = drain_events(&ctx, &mut p2, PILOT_2).into_iter().filter(|(e, _)| e == "OnCharacterTemporarilyBanned").map(|(_, a)| a).collect();
    assert_eq!(temp_ev, vec![vec![PyRep::Integer(expected_expiry_ft)]]);
    assert_ne!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", local_chat_payload("muted")).status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut p2, "LSC", "SendMessage", &lsc_args).await.unwrap_err().code, "CharacterChatBanned");

    ctx.advance_secs(1_801);
    assert_eq!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", local_chat_payload("lapsed")).status_code, STATUS_OK);
    rpc(&ctx, &mut p2, "LSC", "SendMessage", &lsc_args).await.expect("LSC after temporary ban expiry");

    // 3. OnClusterShutdownInitiated: scheduled daily downtime countdown + GM /shutdown
    let _ = drain_events(&ctx, &mut p2, PILOT_2);
    let now = ctx.now_unix();
    let next_dt = eve_server::system_actor::next_downtime_unix_secs(now, 86_400, ctx.systems.downtime_utc_secs_of_day());
    let jump_to_59m = next_dt.saturating_sub(59 * 60);
    if jump_to_59m > now {
        ctx.advance_secs(jump_to_59m - now);
    }
    tick_background_cluster(&ctx).await;
    let dt_notifs: Vec<_> = drain_events(&ctx, &mut p2, PILOT_2).into_iter().filter(|(e, _)| e == "OnClusterShutdownInitiated").map(|(_, a)| a).collect();
    let expected_dt_ft = (next_dt as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(dt_notifs, vec![vec![PyRep::String("UI/Shared/ShutdownNotificationTitle".into()), PyRep::Integer(expected_dt_ft), PyRep::Integer(15)]]);

    let expected_slash_ft = ctx.now_blueos_filetime_ticks() + 180 * 10_000_000;
    slash(&ctx, &mut gm, "/shutdown 180").await;
    let slash_shut: Vec<_> = drain_events(&ctx, &mut p2, PILOT_2).into_iter().filter(|(e, _)| e == "OnClusterShutdownInitiated").map(|(_, a)| a).collect();
    assert_eq!(slash_shut.len(), 1);
    assert_eq!(slash_shut[0][..2], [PyRep::String("UI/Shared/ShutdownNotificationTitle".into()), PyRep::Integer(expected_slash_ft)]);

    // 4. Bound invbroker GetInventoryFromId -> SetPassword(which=1|2, newPassword, oldPassword) + corp container logs
    slash(&ctx, &mut gm, "/giveisk 20000000").await;
    let corp_res = rpc(&ctx, &mut gm, "corpRegistry", "AddCorporation", &[PyRep::String("Vault Logistics".into()), PyRep::String("VLTG".into()), PyRep::Real(0.05)]).await.unwrap();
    let corp_id = corp_res.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();

    let personal_cont = give_item(&ctx, &mut gm, 17_366, 1).await;
    let corp_cont = give_item(&ctx, &mut gm, 17_366, 1).await;
    rpc(&ctx, &mut gm, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(personal_cont)]).await.unwrap();
    rpc(&ctx, &mut gm, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(corp_cont)]).await.unwrap();
    rpc(&ctx, &mut gm, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(corp_cont), PyRep::Integer(1)]).await.unwrap();

    let bind = rpc(&ctx, &mut gm, "invbroker", "MachoBindObject", &[PyRep::Tuple(vec![PyRep::Integer(JITA_STATION), PyRep::Integer(15)]), PyRep::None]).await.unwrap();
    let PyRep::Tuple(bind_parts) = &bind else { panic!("expected bind tuple") };
    let mgr_oid = bind_parts[0].as_i64().unwrap() as u64;
    let personal_oid = bound_rpc(&ctx, &mut gm, "invbroker", mgr_oid, "GetInventoryFromId", &[PyRep::Integer(personal_cont), PyRep::Integer(0)]).await.unwrap().as_i64().unwrap() as u64;
    let corp_oid = bound_rpc(&ctx, &mut gm, "invbroker", mgr_oid, "GetInventoryFromId", &[PyRep::Integer(corp_cont), PyRep::Integer(0)]).await.unwrap().as_i64().unwrap() as u64;
    let _ = drain_events(&ctx, &mut gm, PILOT_1);

    bound_rpc(&ctx, &mut gm, "invbroker", personal_oid, "SetPassword", &[PyRep::Integer(1), PyRep::String("gen_pw_1".into()), PyRep::None]).await.unwrap();
    let n_gen = drain_events(&ctx, &mut gm, PILOT_1);
    assert!(n_gen.iter().any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(1))));
    assert!(n_gen.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations" && a.first() == Some(&PyRep::List(vec![PyRep::Integer(personal_cont)]))));

    bound_rpc(&ctx, &mut gm, "invbroker", corp_oid, "SetPassword", &[PyRep::Integer(2), PyRep::String("cfg_pw_2".into()), PyRep::None]).await.unwrap();
    let n_cfg = drain_events(&ctx, &mut gm, PILOT_1);
    assert!(n_cfg.iter().any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(2))));
    assert!(n_cfg.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations" && a.first() == Some(&PyRep::List(vec![PyRep::Integer(corp_cont)]))));
    let bad_old = bound_rpc(&ctx, &mut gm, "invbroker", corp_oid, "SetPassword", &[PyRep::Integer(2), PyRep::String("cfg_pw_3".into()), PyRep::String("wrong".into())]).await.unwrap_err();
    assert_eq!(bad_old.code, "InvalidPassword");
    assert!(ctx.db.with_snapshot(|s| {
        s.corp_container_logs.get(&corp_id).is_some_and(|logs| logs.iter().any(|l| l.container_id == corp_cont && l.password_type.as_deref() == Some("config")))
    }));
}

#[tokio::test]
async fn test_live_target_multievent_and_in_space_kill_notification() {
    // parity: notify:OnMultiEvent, notify:OnKillNotification
    eve_testkit::attest(&["notify:OnMultiEvent", "notify:OnKillNotification"]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 97_201, "demo", "demo-secret", PILOT_1, true).await;
    let mut s2 = login_select(&ctx, 97_202, "other", "other-secret", PILOT_2, false).await;

    for (sk, lvl) in [(3300, 5), (3302, 5), (3328, 5), (3429, 5)] {
        give_skill(&ctx, &mut s1, sk, lvl).await;
    }
    let rifter_id = give_item(&ctx, &mut s1, 587, 1).await;
    let (gun1, gun2) = (give_item(&ctx, &mut s1, 484, 1).await, give_item(&ctx, &mut s1, 484, 1).await);
    let (ammo1, ammo2) = (give_item(&ctx, &mut s1, 184, 200).await, give_item(&ctx, &mut s1, 184, 200).await);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(rifter_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    for (iid, flag) in [(gun1, InventoryFlag::HiSlot0), (gun2, InventoryFlag::HiSlot1), (ammo1, InventoryFlag::HiSlot0), (ammo2, InventoryFlag::HiSlot1)] {
        add_inv(&ctx, &mut s1, iid, rifter_id, flag).await;
    }

    let ship_b = s2.ship_id.expect("PILOT_2 starter ship");
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[]).await.unwrap();
    ctx.clock().advance_secs(11);
    let _ = (drain_events(&ctx, &mut s1, PILOT_1), drain_events(&ctx, &mut s2, PILOT_2));

    // Extracts the `('OnTarget', sub, id, '')` entries carried by standalone `OnMultiEvent` payloads.
    let target_events = |notifs: &[(String, Vec<PyRep>)]| -> Vec<(String, i64)> {
        let mut out = Vec::new();
        for (_, a) in notifs.iter().filter(|(e, _)| e == "OnMultiEvent") {
            let Some(PyRep::List(items)) = a.first() else { continue };
            for it in items {
                let PyRep::Tuple(outer) = it else { continue };
                if let Some(PyRep::Tuple(inner)) = outer.first() {
                    if let [PyRep::String(ev), PyRep::String(sub), PyRep::Integer(id), PyRep::String(tail)] = inner.as_slice() {
                        if ev == "OnTarget" && tail.is_empty() {
                            out.push((sub.clone(), *id));
                        }
                    }
                }
            }
        }
        out
    };

    let jita_h = ctx.systems.get_or_spawn(JITA_SYS);
    rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.unwrap();
    ctx.advance_secs(10);
    jita_h.step_manual(10.0).await.unwrap();
    assert_eq!(target_events(&drain_events(&ctx, &mut s2, PILOT_2)), vec![("otheradd".to_string(), rifter_id)]);

    rpc(&ctx, &mut s1, "dogmaIM", "RemoveTarget", &[PyRep::Integer(ship_b)]).await.unwrap();
    assert_eq!(target_events(&drain_events(&ctx, &mut s2, PILOT_2)), vec![("otherlost".to_string(), rifter_id)]);

    rpc(&ctx, &mut s2, "dogmaIM", "AddTarget", &[PyRep::Integer(rifter_id)]).await.unwrap();
    ctx.advance_secs(10);
    jita_h.step_manual(10.0).await.unwrap();
    let _ = drain_events(&ctx, &mut s2, PILOT_2);
    rpc(&ctx, &mut s2, "dogmaIM", "ClearTargets", &[]).await.unwrap();
    let cleared = target_events(&drain_events(&ctx, &mut s2, PILOT_2));
    assert_eq!(cleared.iter().find(|(sub, _)| sub == "clear"), Some(&("clear".to_string(), 0)));

    // Limited-engagement duel + live turret destruction in ballpark -> OnKillNotification
    rpc(&ctx, &mut s1, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(PILOT_2)]).await.unwrap();
    rpc(&ctx, &mut s2, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(PILOT_1), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)]).await.unwrap();
    ctx.advance_secs(10);
    jita_h.step_manual(10.0).await.unwrap();
    let _ = (drain_events(&ctx, &mut s1, PILOT_1), drain_events(&ctx, &mut s2, PILOT_2));

    for _ in 0..45 {
        if jita_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none() {
            break;
        }
        for gid in [gun1, gun2] {
            let _ = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)]).await;
        }
        ctx.advance_secs(5);
        jita_h.step_manual(5.0).await.unwrap();
    }
    assert_eq!(jita_h.get_ball_snapshot(ship_b).await.ok().flatten().map(|(_, b)| b.id), None, "PILOT_2 ship destroyed by live turret volleys");
    let kill_args = |n: Vec<(String, Vec<PyRep>)>| n.into_iter().find(|(e, _)| e == "OnKillNotification").map(|(_, a)| a);
    assert_eq!(kill_args(drain_events(&ctx, &mut s1, PILOT_1)), Some(Vec::new()));
    assert_eq!(kill_args(drain_events(&ctx, &mut s2, PILOT_2)), Some(Vec::new()));
}

#[tokio::test]
async fn test_live_excavator_drones_mining_goals_and_market_supply_chain() {

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 97_301, "demo", "demo-secret", PILOT_1, true).await;
    let mut s2 = login_select(&ctx, 97_302, "other", "other-secret", PILOT_2, false).await;

    slash(&ctx, &mut s1, "/giveisk 50000000").await;
    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(PILOT_2), PyRep::Real(10_000_000.0)]).await.unwrap();
    // Mining/drone skills + full SDE prerequisite closure for Rorqual, Capital Industrial Core, and Excavators.
    let rorqual_prereqs = [3327, 20342, 29637, 3184, 22552, 3348, 22536, 20533, 24625, 3387, 3380, 3456, 3449, 3455, 3402, 16281];
    for sk in [3386, 3410, 32918, 3429, 3436, 3442, 3438, 22541, 43702, 43703, 28374, 28585, 3385].into_iter().chain(rorqual_prereqs) {
        give_skill(&ctx, &mut s1, sk, 5).await;
    }

    let created = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Josameto Extraction".into()), PyRep::String("JEXT".into()), PyRep::Real(0.05)]).await.unwrap();
    let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(5_000_000.0)]).await.unwrap();
    ctx.advance_secs(8 * 86_400);

    let (mut goal_msg, mut cfg, mut curr, mut isk, mut pool, mut ts) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Mine Josameto Ore", &mut goal_msg);
    encode_varint_field(3, 50_000, &mut goal_msg);
    encode_bytes_field(25, &[], &mut cfg);
    encode_bytes_field(4, &cfg, &mut goal_msg);
    encode_varint_field(1, encode_zigzag64(500_000), &mut curr);
    encode_bytes_field(1, &curr, &mut isk);
    encode_varint_field(1, 1, &mut pool);
    encode_bytes_field(2, &isk, &mut pool);
    encode_bytes_field(8, &pool, &mut goal_msg);
    encode_varint_field(1, ctx.now_unix() + 86_400, &mut ts);
    encode_bytes_field(10, &ts, &mut goal_msg);
    encode_varint_field(26, 50_000, &mut goal_msg);
    let goal_rsp = quasar_call(&ctx, &s1, "eve_public.goal.api.CreateRequest", goal_msg);
    assert_eq!(goal_rsp.status_code, STATUS_OK);
    let pid_mine = eve_server::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id(&goal_rsp.payload.unwrap().value[4..20]).unwrap();

    let _ = rpc(&ctx, &mut s1, "seasonManager", "process_client_event", &[PyRep::String("totalMined".into()), PyRep::Integer(99_999)]).await;
    assert!(ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty());

    // 1. Excavators (1,100 m3) cannot be loaded into a non-Rorqual Venture drone bay (10 m3);
    //    the Rorqual harvests ore & Blue Ice in Josameto with an active Industrial Core.
    let venture_id = give_item(&ctx, &mut s1, 32_880, 1).await;
    let miner_mod = give_item(&ctx, &mut s1, 483, 1).await;
    let exc_non_rorq = give_item(&ctx, &mut s1, 41_030, 1).await;
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(venture_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    add_inv(&ctx, &mut s1, miner_mod, venture_id, InventoryFlag::HiSlot0).await;
    let bay_args = [PyRep::Integer(exc_non_rorq), PyRep::Integer(venture_id), PyRep::Integer(InventoryFlag::DroneBay as i64)];
    assert_eq!(rpc(&ctx, &mut s1, "invbroker", "Add", &bay_args).await.unwrap_err().code, "NotEnoughDroneBaySpace");

    let rorq_id = give_item(&ctx, &mut s1, 28_352, 1).await;
    let core_id = give_item(&ctx, &mut s1, 28_583, 1).await;
    let (exc_ore, exc_ice) = (give_item(&ctx, &mut s1, 41_030, 1).await, give_item(&ctx, &mut s1, 43_681, 1).await);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(rorq_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    add_inv(&ctx, &mut s1, core_id, rorq_id, InventoryFlag::HiSlot0).await;
    add_inv(&ctx, &mut s1, exc_ore, rorq_id, InventoryFlag::DroneBay).await;
    add_inv(&ctx, &mut s1, exc_ice, rorq_id, InventoryFlag::DroneBay).await;

    let athanor_item = give_item(&ctx, &mut s1, 35_835, 1).await;
    add_inv(&ctx, &mut s1, athanor_item, rorq_id, InventoryFlag::Cargo).await;
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
    ctx.clock().advance_secs(11);
    slash(&ctx, &mut s1, &format!("/tr {JOSAMETO_SYS}")).await;
    let anchor_res = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(athanor_item), PyRep::String("Josameto Refinery".into())]).await.unwrap();
    let athanor_id = anchor_res.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let _ = ctx.systems.get_or_spawn(JOSAMETO_SYS);
    tick_background_cluster(&ctx).await;
    // Client probe scanner / Agency ice-belt query seeds the regional IceBelt anomaly.
    rpc(&ctx, &mut s1, "dungeonInstanceCacheMgr", "GetIceBeltInstances", &[]).await.expect("GetIceBeltInstances");
    let (veld_ast_id, ice_ast_id, ice_type) = ctx.db.with_snapshot(|s| {
        let veld = s.asteroids.values().filter(|a| a.solar_system_id == JOSAMETO_SYS && a.type_id == 1230 && a.quantity >= 10_000).min_by_key(|a| a.asteroid_id).unwrap().asteroid_id;
        let is_ice = |tid: u32| ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 465);
        let ice = s.asteroids.values().filter(|a| a.solar_system_id == JOSAMETO_SYS && is_ice(a.type_id) && a.quantity >= 5).min_by_key(|a| a.asteroid_id).expect("regional IceBelt spawned");
        (veld, ice.asteroid_id, ice.type_id)
    });

    warp_to_item(&ctx, &mut s1, JOSAMETO_SYS, veld_ast_id).await;
    rpc(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(exc_ore), PyRep::Integer(exc_ice)])]).await.unwrap();
    assert_eq!(
        rpc(&ctx, &mut s1, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_ore)]), PyRep::Integer(veld_ast_id)]).await.unwrap_err().code,
        "IndustrialCoreRequired"
    );
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())]).await.unwrap();
    rpc(&ctx, &mut s1, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_ore)]), PyRep::Integer(veld_ast_id)]).await.unwrap();
    let next_ore_tick = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap());
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_ore_tick).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == rorq_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230).map(|i| i.quantity).sum::<i32>()),
        6_682
    );

    rpc(&ctx, &mut s1, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(exc_ore), PyRep::Integer(exc_ice)])]).await.unwrap();
    rpc(&ctx, &mut s1, "dogmaIM", "Deactivate", &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())]).await.unwrap();
    ctx.advance_secs(310);
    ctx.systems.get_or_spawn(JOSAMETO_SYS).step_manual(310.0).await.unwrap();
    warp_to_item(&ctx, &mut s1, JOSAMETO_SYS, ice_ast_id).await;
    rpc(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(exc_ice)])]).await.unwrap();
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())]).await.unwrap();
    rpc(&ctx, &mut s1, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_ice)]), PyRep::Integer(ice_ast_id)]).await.unwrap();
    let next_ice_tick = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap());
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, next_ice_tick).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == rorq_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == ice_type).map(|i| i.quantity).sum::<i32>()),
        1
    );

    // 2. Venture laser mining in Josameto -> ledgers + corp goal + reprocessing + 2-jump Jita market
    rpc(&ctx, &mut s1, "entity", "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(exc_ice)])]).await.unwrap();
    rpc(&ctx, &mut s1, "dogmaIM", "Deactivate", &[PyRep::Integer(core_id), PyRep::String("industrialCore".into())]).await.unwrap();
    ctx.advance_secs(310);
    ctx.systems.get_or_spawn(JOSAMETO_SYS).step_manual(310.0).await.unwrap();
    slash(&ctx, &mut s1, &format!("/tr {JITA_SYS}")).await;
    warp_to_item(&ctx, &mut s1, JITA_SYS, JITA_STATION).await;
    rpc(&ctx, &mut s1, "beyonce", "CmdDock", &[PyRep::Integer(JITA_STATION)]).await.unwrap();
    ctx.clock().advance_secs(11);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(venture_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
    ctx.clock().advance_secs(11);
    slash(&ctx, &mut s1, &format!("/tr {JOSAMETO_SYS}")).await;
    let jos_h = ctx.systems.get_or_spawn(JOSAMETO_SYS);
    let _ = jos_h.set_ball_position(venture_id, eve_destiny::DVec3::new(200_000.0, 0.0, 0.0)).await;
    warp_to_item(&ctx, &mut s1, JOSAMETO_SYS, veld_ast_id).await;
    rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(veld_ast_id)]).await.unwrap();
    for _ in 0..6 {
        ctx.clock().advance_secs(5);
        jos_h.step_manual(5.0).await.unwrap();
        if jos_h.get_target_locks(venture_id).await.unwrap().iter().any(|l| l.target_id == veld_ast_id && l.locked) {
            break;
        }
    }
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(miner_mod), PyRep::Integer(veld_ast_id)]).await.unwrap();
    ctx.advance_secs(65);
    tick_background_cluster(&ctx).await;

    assert!(!matches!(rpc(&ctx, &mut s1, "characterMiningLedger", "GetCharacterLogs", &[]).await.unwrap(), PyRep::None));
    assert!(!matches!(rpc(&ctx, &mut s1, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.unwrap(), PyRep::None));
    assert!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_mine].current_quantity >= 6_682));

    warp_to_item(&ctx, &mut s1, JOSAMETO_SYS, JOSAMETO_STATION).await;
    rpc(&ctx, &mut s1, "beyonce", "CmdDock", &[PyRep::Integer(JOSAMETO_STATION)]).await.unwrap();
    ctx.clock().advance_secs(11);
    let veld_stack_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == venture_id && i.type_id == 1230 && i.quantity >= 100).unwrap().item_id);
    add_inv(&ctx, &mut s1, veld_stack_id, JOSAMETO_STATION, InventoryFlag::Hangar).await;
    rpc(&ctx, &mut s1, "reprocessingSvc", "Reprocess", &[PyRep::Integer(veld_stack_id)]).await.unwrap();
    let trit_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.owner_id == PILOT_1 && i.location_id == JOSAMETO_STATION && i.type_id == 34 && i.quantity >= 100).unwrap().item_id);
    add_inv(&ctx, &mut s1, trit_id, venture_id, InventoryFlag::Cargo).await;
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
    ctx.clock().advance_secs(11);

    warp_to_item(&ctx, &mut s1, JOSAMETO_SYS, GATE_JOSAMETO_TO_NC).await;
    rpc(&ctx, &mut s1, "beyonce", "CmdStargateJump", &[PyRep::Integer(GATE_JOSAMETO_TO_NC), PyRep::Integer(GATE_NC_FROM_JOSAMETO), PyRep::Integer(venture_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    warp_to_item(&ctx, &mut s1, NEW_CALDARI_SYS, GATE_NC_TO_JITA).await;
    rpc(&ctx, &mut s1, "beyonce", "CmdStargateJump", &[PyRep::Integer(GATE_NC_TO_JITA), PyRep::Integer(GATE_JITA_FROM_NC), PyRep::Integer(venture_id)]).await.unwrap();
    ctx.clock().advance_secs(11);
    warp_to_item(&ctx, &mut s1, JITA_SYS, JITA_STATION).await;
    rpc(&ctx, &mut s1, "beyonce", "CmdDock", &[PyRep::Integer(JITA_STATION)]).await.unwrap();
    ctx.clock().advance_secs(11);

    add_inv(&ctx, &mut s1, trit_id, JITA_STATION, InventoryFlag::Hangar).await;
    let sell_item = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(trit_id)),
        ("price", PyRep::Real(6.0)),
        ("quantity", PyRep::Integer(100)),
    ]);
    rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![sell_item]), PyRep::Bool(false), PyRep::Integer(90)]).await.unwrap();
    assert!(!matches!(rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(34)]).await.unwrap(), PyRep::None));
    let buy_fill = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION), PyRep::Integer(34), PyRep::Real(6.0), PyRep::Integer(100), PyRep::Integer(90), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(buy_fill.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(true));
}
