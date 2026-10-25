//! Fleet, Chat, Character Biomass & Skill-Queue Omega-Lapse Lifecycle Parity Tests
//! (`feat:lifecycle.fleet-logout`, `feat:lifecycle.fleet-disconnect-grace`,
//! `feat:lifecycle.fleet-login-consistency`, `feat:lifecycle.fleet-server-restart`,
//! `feat:lifecycle.fleet-kick-session`, `feat:lifecycle.fleet-invite-offline`,
//! `feat:lifecycle.chat-local-logout`, `feat:lifecycle.character-biomass-cleanup`,
//! `feat:lifecycle.biomass-economy`, `feat:lifecycle.skillqueue-omega-lapse`, and `OF-43`).

use crate::common;

use eve_db::{
    ContractRecord, CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketOrderRecord, PlanetaryColonyRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::char_social_mail_routes::dispatch_char_social_mail_route;
use eve_server::esi::char_social_routes::dispatch_char_social_route;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use eve_server::services::{
    account_services::set_subscription_expiry,
    character_lifecycle::BIOMASS_DELAY_SECS,
    character_skills::{alpha_skill_cap_for_character, effective_character_attributes, lookup_effective_attr_from_tuple},
    fleet_social::fleet_social_notify::{
        handle_fleet_member_disconnect, tick_fleet_lifecycle, FLEET_DISCONNECT_GRACE_SECS,
        FLEET_INVITE_EXPIRY_SECS,
    },
    wallet_serverops::run_daily_downtime_and_jobs,
};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const FH: &str = "fleetObjectHandler";

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    s
}

async fn call_res(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    call_res(ctx, s, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn drain_events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n: &PendingMachoNotification| n.event == event)
        .map(|n| n.args)
        .collect()
}

#[tokio::test]
async fn test_fleet_logout_and_disconnect_grace_lifecycle() {
    // parity: feat:lifecycle.fleet-logout, feat:lifecycle.fleet-disconnect-grace
    eve_testkit::attest(&["feat:lifecycle.fleet-logout", "feat:lifecycle.fleet-disconnect-grace"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let uid3 = ctx.db.provision_player_account("fl_p3", "fl_p3_pass");
    let p3 = ctx.db.create_character(uid3, "Fleet Pilot 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let mut s1 = login_select(&ctx, 7101, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 7102, "other", "other-secret", P2).await;
    let mut s3 = login_select(&ctx, 7103, "fl_p3", "fl_p3_pass", p3).await;
    let fid = call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    let wid2 = call(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Wing 2".into())]).await.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    let sid2 = call(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid2), PyRep::String("Squad 2".into())]).await.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();

    // Invite P2 as Wing Commander of Wing 2 and P3 as Squad Commander of Squad 2.
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid2), PyRep::None, PyRep::Integer(2)]).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3), PyRep::Integer(wid2), PyRep::Integer(sid2), PyRep::Integer(3)]).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, p3);
    call(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. P3 logs out via UserLogOffCharacter: removed from members, member_details, squad commander, fleet channel; OnFleetLeave sent.
    call(&ctx, &mut s3, "userSvc", "UserLogOffCharacter", &[]).await;
    assert_eq!(s3.fleet_id, None);
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fid).expect("fleet still exists");
        assert!(!f.members.contains(&p3) && !f.member_details.contains_key(&p3));
        assert_eq!(f.squad_details.get(&sid2).and_then(|s| s.commander_id), None);
        assert!(!snap.chat_channels[&format!("fleetid:{fid}")].members.contains(&p3));
    });
    assert_eq!(drain_events(&ctx, P1, "OnFleetLeave"), vec![vec![PyRep::Integer(p3)]]);
    assert_eq!(drain_events(&ctx, P2, "OnFleetLeave"), vec![vec![PyRep::Integer(p3)]]);

    // 2. Boss P1 logs out via authentication.Logout: boss passes to P2 with fleetJobCreator (bit 2) in OnFleetMemberChanged.
    call(&ctx, &mut s1, "authentication", "Logout", &[]).await;
    assert_eq!(s1.fleet_id, None);
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fid).expect("fleet still has P2");
        assert!(!f.members.contains(&P1) && !f.member_details.contains_key(&P1));
        assert_eq!(f.boss_id, P2);
        assert_ne!(f.member_details[&P2].job & 2, 0, "P2 must hold fleetJobCreator bit");
    });
    let p2_notifs = ctx.drain_notifications_for_char(P2);
    assert!(p2_notifs.iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(P1)]));
    assert!(p2_notifs.iter().any(|n| {
        n.event == "OnFleetMemberChanged"
            && n.args.first() == Some(&PyRep::Integer(P2))
            && n.args.get(7).and_then(|v| v.as_i64()).is_some_and(|job| (job & 2) != 0)
    }));

    // 3. Last member P2 logs out: fleet and its chat channel are deleted.
    call(&ctx, &mut s2, "authentication", "Logout", &[]).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fid) && !snap.chat_channels.contains_key(&format!("fleetid:{fid}")));
    });

    // 4. Disconnect grace period (`feat:lifecycle.fleet-disconnect-grace`):
    let mut s1 = login_select(&ctx, 7104, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 7105, "other", "other-secret", P2).await;
    let fid2 = call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    let wid_g = call(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Grace Wing".into())]).await.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    let sid_g = call(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid_g), PyRep::String("Grace Squad".into())]).await.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid_g), PyRep::Integer(sid_g), PyRep::Integer(3)]).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid2)]).await;

    // Socket disconnect P2; within grace window P2 stays in fleet and Reconnect restores position.
    handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.advance_virtual_clock(FLEET_DISCONNECT_GRACE_SECS / 2);
    tick_fleet_lifecycle(&ctx);
    ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid2].member_details.contains_key(&P2)));
    let mut s2 = login_select(&ctx, 7106, "other", "other-secret", P2).await;
    assert_eq!(s2.fleet_id, Some(fid2), "Relog within grace must restore session.fleet_id");
    let recon = call(&ctx, &mut s2, FH, "Reconnect", &[]).await;
    assert_eq!(recon.dict_get("fleetID").and_then(|v| v.as_i64()), Some(fid2));
    assert_eq!(recon.dict_get("wingID").and_then(|v| v.as_i64()), Some(wid_g));
    assert_eq!(recon.dict_get("squadID").and_then(|v| v.as_i64()), Some(sid_g));
    assert_eq!(recon.dict_get("role").and_then(|v| v.as_i64()), Some(3));

    // Socket disconnect boss P1 and expire grace: P1 is removed and boss passes to P2.
    ctx.drain_notifications_for_char(P2);
    handle_fleet_member_disconnect(&ctx, &mut s1);
    ctx.advance_virtual_clock(FLEET_DISCONNECT_GRACE_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    ctx.db.with_snapshot(|snap| {
        let f = &snap.fleets[&fid2];
        assert!(!f.member_details.contains_key(&P1) && f.boss_id == P2 && (f.member_details[&P2].job & 2) != 0);
    });
    let p2_grace_notifs = ctx.drain_notifications_for_char(P2);
    assert!(p2_grace_notifs.iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(P1)]));
    assert!(p2_grace_notifs.iter().any(|n| n.event == "OnFleetMemberChanged"));

    // Socket disconnect last member P2 and expire grace: empty fleet disbands.
    handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.advance_virtual_clock(FLEET_DISCONNECT_GRACE_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fid2) && !snap.chat_channels.contains_key(&format!("fleetid:{fid2}")));
    });
}

#[tokio::test]
async fn test_fleet_login_consistency_kick_session_invite_offline_and_restart() {
    // parity: feat:lifecycle.fleet-login-consistency, feat:lifecycle.fleet-server-restart, feat:lifecycle.fleet-kick-session, feat:lifecycle.fleet-invite-offline, feat:lifecycle.chat-local-logout
    eve_testkit::attest(&[
        "feat:lifecycle.fleet-login-consistency",
        "feat:lifecycle.fleet-server-restart",
        "feat:lifecycle.fleet-kick-session",
        "feat:lifecycle.fleet-invite-offline",
        "feat:lifecycle.chat-local-logout",
    ]);
    let db_path = std::env::temp_dir().join(format!("evomu_fleet_chat_lifecycle_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let (fid_persisted, p3) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        repo.with_mut_snapshot(|snap| {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        });
        let ctx = ServerContext::with_repository(repo);
        common::pin_clock(&ctx);

        let uid3 = ctx.db.provision_player_account("fl_c3", "fl_c3_pass");
        let p3 = ctx.db.create_character(uid3, "Consistency Pilot 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

        let mut s1 = login_select(&ctx, 7201, "demo", "demo-secret", P1).await;
        let mut s2 = login_select(&ctx, 7202, "other", "other-secret", P2).await;

        // 1. Local & joined chat channel logout/disconnect (`feat:lifecycle.chat-local-logout`):
        let chan_rep = call(
            &ctx,
            &mut s1,
            "XmppChatMgr",
            "CreatePlayerOwnedChannel",
            &[PyRep::String("OpsRoom".into()), PyRep::String(String::new()), PyRep::String("MOTD".into())],
        )
        .await;
        let custom_cid = chan_rep.dict_get("channelID").and_then(|v| v.as_str()).unwrap_or("custom:1").to_string();
        call(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String(custom_cid.clone())]).await;
        ctx.drain_notifications_for_char(P1);

        call(&ctx, &mut s2, "authentication", "Logout", &[]).await;
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.chat_channels["solarsystemid2:30000142"].members.contains(&P2));
            if let Some(c) = snap.chat_channels.get(&custom_cid) {
                assert!(!c.members.contains(&P2));
            }
        });
        assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| {
            n.event == "OnChannelLeft"
                && n.args == vec![PyRep::String("solarsystemid2:30000142".into()), PyRep::Integer(P2)]
        }));

        let mut s2 = login_select(&ctx, 7203, "other", "other-secret", P2).await;
        ctx.drain_notifications_for_char(P1);
        handle_fleet_member_disconnect(&ctx, &mut s2);
        ctx.db.with_snapshot(|snap| assert!(!snap.chat_channels["solarsystemid2:30000142"].members.contains(&P2)));
        assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnChannelLeft"));

        // 2. Fleet invite validation & expiry (`feat:lifecycle.fleet-invite-offline`):
        let fid_a = call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(call_res(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await.unwrap_err().code, "CharacterOffline");

        let mut s2 = login_select(&ctx, 7204, "other", "other-secret", P2).await;
        let fid_b = call(&ctx, &mut s2, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(call_res(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await.unwrap_err().code, "AlreadyInFleet");

        let mut s3 = login_select(&ctx, 7205, "fl_c3", "fl_c3_pass", p3).await;
        call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3)]).await;
        assert_eq!(call_res(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3)]).await.unwrap_err().code, "InviteAlreadyPending");

        ctx.advance_virtual_clock(FLEET_INVITE_EXPIRY_SECS + 5);
        assert_eq!(call_res(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid_a)]).await.unwrap_err().code, "InviteExpired");

        call(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3)]).await;
        call(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid_b)]).await;

        // 3. Single-fleet invariant & relogin consistency (`feat:lifecycle.fleet-login-consistency`, `F-40`):
        assert_eq!(call_res(&ctx, &mut s3, "fleetMgr", "CreateFleet", &[]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(call_res(&ctx, &mut s3, "fleetSvc", "InitFleet", &[]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(call_res(&ctx, &mut s3, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fid_a)]).await.unwrap_err().code, "AlreadyInFleet");
        assert_eq!(call_res(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid_a)]).await.unwrap_err().code, "AlreadyInFleet");

        handle_fleet_member_disconnect(&ctx, &mut s3);
        let mut s3 = login_select(&ctx, 7206, "fl_c3", "fl_c3_pass", p3).await;
        assert_eq!(call(&ctx, &mut s3, FH, "GetFleetID", &[]).await, PyRep::Integer(fid_b));
        assert_eq!(s3.fleet_id, Some(fid_b));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.fleets.values().filter(|f| f.members.contains(&p3)).count(), 1);
        });

        // 4. KickMember session clearing & role checks (`feat:lifecycle.fleet-kick-session`, `F-02`):
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&p3].fleet_id, Some(fid_b));
            assert_eq!(snap.characters[&P2].fleet_id, Some(fid_b));
        });
        assert_eq!(call_res(&ctx, &mut s3, FH, "Invite", &[PyRep::Integer(P1)]).await.unwrap_err().code, "FleetPermissionDenied");
        assert_eq!(call_res(&ctx, &mut s3, "fleetProxy", "RemoveFleetFinderAdvert", &[]).await.unwrap_err().code, "FleetPermissionDenied");
        assert_eq!(call_res(&ctx, &mut s3, FH, "GetJoinRequests", &[]).await.unwrap_err().code, "FleetPermissionDenied");
        ctx.drain_notifications_for_char(p3);
        call(&ctx, &mut s2, FH, "KickMember", &[PyRep::Integer(p3)]).await;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&p3].fleet_id, None);
        });
        assert!(ctx.drain_notifications_for_char(p3).iter().any(|n| {
            n.event == "OnSessionChanged"
                && n.args.iter().any(|arg| arg.dict_get("fleetid") == Some(&PyRep::Tuple(vec![PyRep::Integer(fid_b), PyRep::None])))
        }));
        assert_eq!(call_res(&ctx, &mut s3, FH, "Invite", &[PyRep::Integer(P1)]).await.unwrap_err().code, "NotInFleet");
        assert_eq!(call_res(&ctx, &mut s3, FH, "GetMotd", &[]).await.unwrap_err().code, "NotInFleet");
        assert_eq!(call_res(&ctx, &mut s3, FH, "GetFleetMaxSize", &[]).await.unwrap_err().code, "NotInFleet");
        assert_eq!(s3.fleet_id, None);
        assert_eq!(
            call_res(&ctx, &mut s3, FH, "SendBroadcast", &[PyRep::String("Target".into()), PyRep::Integer(0)]).await.unwrap_err().code,
            "NotInFleet"
        );
        call(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3)]).await;
        call(&ctx, &mut s2, FH, "SetFleetMaxSize", &[PyRep::Integer(64)]).await;
        assert!(ctx.has_fleet_extras_for(fid_b));
        ctx.drain_notifications_for_char(P2);
        call(&ctx, &mut s2, FH, "DisbandFleet", &[]).await;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&P2].fleet_id, None);
            assert!(!snap.fleets.contains_key(&fid_b));
        });
        assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| n.event == "OnFleetLeave"));
        assert!(!ctx.has_fleet_extras_for(fid_b));
        call_res(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3)]).await.expect("DisbandFleet clears invite_timestamps");
        call(&ctx, &mut s3, FH, "RejectInvite", &[PyRep::Integer(fid_a)]).await;

        // 5. OF-43: Lowest unused power-of-two mail label IDs & calendar status 3 -> "not_responded":
        let l1 = call(&ctx, &mut s1, "mailMgr", "CreateLabel", &[PyRep::String("Alpha".into())]).await.dict_get("labelID").and_then(|v| v.as_i64()).unwrap();
        let esi_lbl = dispatch_char_social_mail_route(
            &ctx,
            "POST",
            "/characters/{character_id}/mail/labels",
            &HashMap::from([("character_id".to_string(), P1.to_string())]),
            &HashMap::new(),
            &serde_json::to_vec(&json!({"name": "Beta", "color": "#ffffff"})).unwrap(),
        )
        .expect("POST mail labels");
        assert_eq!(esi_lbl.status_code, 201);
        let l2 = esi_lbl.body.unwrap().as_i64().unwrap();
        let l3 = call(&ctx, &mut s1, "mailMgr", "CreateLabel", &[PyRep::String("Gamma".into())]).await.dict_get("labelID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!((l1, l2, l3), (16, 32, 64));
        call(&ctx, &mut s1, "mailMgr", "DeleteLabel", &[PyRep::Integer(32)]).await;
        let l2_reused = call(&ctx, &mut s1, "mailMgr", "CreateLabel", &[PyRep::String("Beta2".into())]).await.dict_get("labelID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(l2_reused, 32);

        let ev_id = call(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[
            PyRep::Integer(ctx.now_blueos_filetime_ticks() + 36_000_000_000), PyRep::Integer(60),
            PyRep::String("Fleet Briefing".into()), PyRep::String("Ops".into()), PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(P2), PyRep::Integer(p3)]),
        ]).await.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
        call(&ctx, &mut s3, "calendarMgr", "SendEventResponse", &[PyRep::Integer(ev_id), PyRep::Integer(P1), PyRep::Integer(5)]).await;
        let att_res = dispatch_char_social_route(
            &ctx, "GET", "/characters/{character_id}/calendar/{event_id}/attendees",
            &HashMap::from([("character_id".to_string(), P1.to_string()), ("event_id".to_string(), ev_id.to_string())]),
            &HashMap::new(), b"",
        ).expect("GET calendar attendees");
        assert_eq!(att_res.status_code, 200);
        let att_json = att_res.body.unwrap();
        let att_arr = att_json.as_array().unwrap();
        assert_eq!(att_arr.iter().find(|x| x["character_id"] == json!(P2)).unwrap()["event_response"], json!("not_responded"));
        assert_eq!(att_arr.iter().find(|x| x["character_id"] == json!(p3)).unwrap()["event_response"], json!("tentative"));

        call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3)]).await;
        (fid_a, p3)
    };

    // 6. Restart server (`feat:lifecycle.fleet-server-restart`): no fleet with offline members or pending invites survives.
    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx = ServerContext::with_repository(repo);
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.fleets.contains_key(&fid_persisted) && snap.fleets.is_empty());
        });
        let mut s3 = login_select(&ctx, 7207, "fl_c3", "fl_c3_pass", p3).await;
        assert_eq!(s3.fleet_id, None);
        assert_eq!(call(&ctx, &mut s3, FH, "GetFleetID", &[]).await, PyRep::None);
        run_daily_downtime_and_jobs(&ctx);
    }
    let _ = std::fs::remove_file(&db_path);
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn test_character_biomass_cleanup_and_economy() {
    // parity: feat:lifecycle.character-biomass-cleanup, feat:lifecycle.biomass-economy
    eve_testkit::attest(&["feat:lifecycle.character-biomass-cleanup", "feat:lifecycle.biomass-economy"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = 1u64;
    let victim_corp = ctx.db.create_character(uid, "Biomass Corp Pilot", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let victim_npc = ctx.db.create_character(uid, "Biomass NPC Pilot", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

    let (corp_id, station_id, office_id) = (2_000_777i64, 60_003_760i64, 920_000_777i64);
    let (ship_item_id, contract_item_id, bp_item_id, npc_item_id) = (930_000_101i64, 930_000_102i64, 930_000_103i64, 930_000_201i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Biomass Test Corp".into(),
                ticker: "BMTC".into(),
                ceo_id: P1,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([P1, P2, victim_corp]),
                member_roles: HashMap::from([(P1, u64::MAX), (victim_corp, 128)]),
                wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents::from_isk(1_000_000)),
                hq_station_id: Some(station_id),
                ..Default::default()
            },
        );
        for cid in [P1, P2, victim_corp] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
            }
        }
        snap.characters.get_mut(&victim_corp).unwrap().balance = IskCents::from_isk(2_000_000);
        snap.characters.get_mut(&victim_npc).unwrap().balance = IskCents::from_isk(1_500_000);

        snap.items.insert(office_id, mk_item(office_id, 27, corp_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(ship_item_id, mk_item(ship_item_id, 587, victim_corp, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(contract_item_id, mk_item(contract_item_id, 34, victim_corp, station_id, InventoryFlag::Locked, 500, false));
        snap.items.insert(bp_item_id, mk_item(bp_item_id, 688, victim_corp, station_id, InventoryFlag::Locked, 1, true));
        snap.items.insert(npc_item_id, mk_item(npc_item_id, 587, victim_npc, station_id, InventoryFlag::Hangar, 1, true));

        snap.market_orders.insert(
            880_001,
            MarketOrderRecord {
                order_id: 880_001,
                owner_id: victim_corp,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 100_000,
                vol_remaining: 100_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500_000),
            },
        );
        snap.market_orders.insert(
            880_002,
            MarketOrderRecord {
                order_id: 880_002,
                owner_id: victim_corp,
                station_id,
                region_id: 10_000_002,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 250,
                vol_remaining: 250,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.contracts.insert(
            890_001,
            ContractRecord {
                contract_id: 890_001,
                issuer_id: victim_corp,
                issuer_corp_id: corp_id,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                reward: IskCents::from_isk(100_000),
                current_bid: IskCents::from_isk(50_000),
                current_bidder_id: Some(P2),
                item_ids: vec![contract_item_id],
                ..Default::default()
            },
        );
        snap.contracts.insert(
            890_002,
            ContractRecord {
                contract_id: 890_002,
                issuer_id: P1,
                issuer_corp_id: corp_id,
                assignee_id: victim_corp,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            890_003,
            ContractRecord {
                contract_id: 890_003,
                issuer_id: P1,
                issuer_corp_id: corp_id,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                current_bid: IskCents::from_isk(25_000),
                current_bidder_id: Some(victim_npc),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            895_001,
            IndustryJobRecord {
                job_id: 895_001,
                installer_id: victim_corp,
                facility_id: station_id,
                blueprint_item_id: bp_item_id,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 1_700_000_000,
            },
        );
        snap.planetary_colonies.insert(
            (victim_corp, 40_009_082),
            PlanetaryColonyRecord {
                planet_id: 40_009_082,
                owner_id: victim_corp,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1_700_000_000,
            },
        );
    });

    let mut s1 = login_select(&ctx, 7301, "demo", "demo-secret", P1).await;
    assert_eq!(call_res(&ctx, &mut s1, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(P1)]).await.unwrap_err().code, "CannotDeleteCorpCEO");
    assert_eq!(call_res(&ctx, &mut s1, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(P1)]).await.unwrap_err().code, "CannotDeleteCorpCEO");

    let fid = call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(f) = snap.fleets.get_mut(&fid) {
            f.members.push(victim_corp);
            f.member_details.insert(
                victim_corp,
                eve_db::FleetMemberRecord {
                    character_id: victim_corp,
                    role: eve_db::FleetRole::SquadMember,
                    wing_id: Some(1),
                    squad_id: Some(1),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: ctx.now_unix(),
                },
            );
        }
    });

    // Biomass player-corp member victim_corp (with office).
    call(&ctx, &mut s1, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(victim_corp)]).await;
    ctx.advance_virtual_clock(BIOMASS_DELAY_SECS + 10);
    call(&ctx, &mut s1, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(victim_corp)]).await;

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        assert!(!corp.members.contains(&victim_corp) && !corp.member_roles.contains_key(&victim_corp));
        assert!(!snap.fleets[&fid].members.contains(&victim_corp));
        assert!(!snap.market_orders.contains_key(&880_001) && !snap.market_orders.contains_key(&880_002));
        assert!(!snap.contracts.contains_key(&890_001));
        assert_eq!(snap.contracts[&890_002].status, "Rejected");
        assert!(!snap.industry_jobs.contains_key(&895_001));
        assert!(!snap.planetary_colonies.contains_key(&(victim_corp, 40_009_082)));

        for iid in [ship_item_id, contract_item_id, bp_item_id] {
            let item = snap.items.get(&iid).expect("inherited item must exist");
            assert_eq!((item.owner_id, item.location_id, item.flag), (corp_id, station_id, InventoryFlag::CorpSAG1));
        }
        assert!(snap.items.values().any(|i| i.owner_id == corp_id && i.type_id == 670 && i.flag == InventoryFlag::CorpSAG1));
        assert!(snap.items.values().any(|i| i.owner_id == corp_id && i.type_id == 35 && i.quantity == 250 && i.flag == InventoryFlag::CorpSAG1));

        assert_eq!(corp.division_balance(1), IskCents::from_isk(3_600_000));
        assert!(snap.wallet_journal.iter().any(|j| {
            j.character_id == corp_id
                && j.entry_type == "Inheritance"
                && j.amount == IskCents::from_isk(2_600_000)
                && snap.wallet_journal_meta.get(&j.entry_id).is_some_and(|m| m.account_key == 1000)
        }));
        for member_id in [P1, P2] {
            assert!(snap.mails.iter().any(|m| m.sender_id == victim_corp && m.recipient_id == member_id));
        }
    });

    // Biomass NPC-corp member victim_npc: assets and wallet ISK are destroyed.
    call(&ctx, &mut s1, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(victim_npc)]).await;
    ctx.advance_virtual_clock(BIOMASS_DELAY_SECS + 10);
    call(&ctx, &mut s1, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(victim_npc)]).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&npc_item_id) && snap.items.values().all(|i| i.owner_id != victim_npc));
        assert_eq!(snap.characters[&victim_npc].balance, IskCents(0));
        assert_eq!(snap.contracts[&890_003].current_bidder_id, None);
    });
}

#[tokio::test]
async fn test_skillqueue_omega_lapse_lifecycle() {
    // parity: feat:lifecycle.skillqueue-omega-lapse
    eve_testkit::attest(&["feat:lifecycle.skillqueue-omega-lapse"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("omega_lapse_u", "omega_lapse_p");
    let char_id = ctx.db.create_character(uid, "Omega Lapse Pilot", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

    let start = ctx.now_unix();
    let expiry_t = start + 600;
    let (grade, _) = set_subscription_expiry(&ctx, uid, expiry_t);
    assert_eq!(grade, 2);

    let (above_cap_skill, alpha_skill) = (11_579u32, 3_327u32);
    let race_id = ctx.db.get_character(char_id).unwrap().race_id;
    assert_eq!(alpha_skill_cap_for_character(&ctx.sde, race_id, above_cap_skill), 0);
    assert!(alpha_skill_cap_for_character(&ctx.sde, race_id, alpha_skill) >= 1);

    ctx.db.with_mut_snapshot(|snap| {
        let m = snap.skills.entry(char_id).or_default();
        m.insert(above_cap_skill, TrainedSkillRecord { type_id: above_cap_skill, level: 0, skill_points: 0.0 });
        m.insert(alpha_skill, TrainedSkillRecord { type_id: alpha_skill, level: 0, skill_points: 0.0 });
    });

    let mut sess = login_select(&ctx, 7401, "omega_lapse_u", "omega_lapse_p", char_id).await;
    call(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(above_cap_skill as i64), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(alpha_skill as i64), PyRep::Integer(1)]),
        ])],
    )
    .await;

    let skill_rate = |skill_id: u32, mult: f64| {
        ctx.db.with_snapshot(|snap| {
            let eff = effective_character_attributes(&ctx.sde, snap, char_id);
            let st = ctx.sde.types.get(&skill_id).unwrap();
            let p_id = st.default_attributes.get(&180).copied().unwrap_or(165.0) as u32;
            let s_id = st.default_attributes.get(&181).copied().unwrap_or(166.0) as u32;
            let p = lookup_effective_attr_from_tuple(eff, p_id);
            let s = lookup_effective_attr_from_tuple(eff, s_id);
            eve_server::formulas::sp_per_minute(p, s, mult) / 60.0
        })
    };
    let expected_sp_at_t = skill_rate(above_cap_skill, 1.0) * ((expiry_t - start) as f64);

    // Log out, advance clock to start + 1200 (expiry_t = start + 600 is mid-way), and log back in.
    call(&ctx, &mut sess, "authentication", "Logout", &[]).await;
    ctx.advance_virtual_clock(1_200);

    let mut sess = login_select(&ctx, 7402, "omega_lapse_u", "omega_lapse_p", char_id).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].clone_grade, 1);
        assert!(snap.paused_skill_queues.contains(&char_id));
        assert!((snap.skills[&char_id][&above_cap_skill].skill_points - expected_sp_at_t).abs() < 1e-4);
        assert_eq!(snap.skills[&char_id][&alpha_skill].skill_points, 0.0);
        let q = &snap.skill_queues[&char_id];
        assert_eq!((q.len(), q[0].type_id, q[1].type_id), (2, above_cap_skill, alpha_skill));
    });

    // Advance another 600s while paused: no SP accrues.
    ctx.advance_virtual_clock(600);
    call(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[]).await;
    ctx.db.with_snapshot(|snap| {
        assert!((snap.skills[&char_id][&above_cap_skill].skill_points - expected_sp_at_t).abs() < 1e-4);
        assert_eq!(snap.skills[&char_id][&alpha_skill].skill_points, 0.0);
    });

    // Explicit resume with the Alpha skill trains at Alpha rate (0.5x Omega rate).
    call(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(alpha_skill as i64), PyRep::Integer(1)])])],
    )
    .await;
    let alpha_rate_sp_per_sec = skill_rate(alpha_skill, 0.5);
    ctx.advance_virtual_clock(300);
    call(&ctx, &mut sess, "skillMgr2", "GetSkills", &[]).await;
    ctx.db.with_snapshot(|snap| {
        assert!((snap.skills[&char_id][&alpha_skill].skill_points - alpha_rate_sp_per_sec * 300.0).abs() < 1e-4);
    });
}
