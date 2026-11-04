#![allow(unused_imports)]
//! Skills, Clones, Jump Clones, Home Station, Wallet, Market, Contracts, Industry, Bookmarks,
//! Standings, Agents, and Station notification emission tests (Increment 6).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

pub(crate) const P1: i64 = 90_000_001;
pub(crate) const P2: i64 = 90_000_002;
pub(crate) const STATION_JITA: i64 = 60_003_760;
pub(crate) const STATION_AMARR: i64 = 60_008_494;
pub(crate) const SYS_JITA: u32 = 30_000_142;

pub(crate) async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    ctx.advance_secs(12);
    s
}

pub(crate) async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

pub(crate) fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs.iter().filter(|n| n.event == event).map(|n| n.args.clone()).collect()
}

pub(crate) fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
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
            },
        );
    });
}

#[tokio::test]
async fn test_ship_clone_vat_bookmarks_standings_agents_and_item_locking_notifications() {
    // parity: notify:OnShipJumpCloneInstallationOffered, notify:OnShipJumpCloneInstallationCanceled, notify:OnShipJumpCloneInstallationDone, notify:OnShipJumpCloneCacheInvalidated, notify:OnSharedBookmarksFolderUpdated, notify:OnSharedBookmarksFolderDeleted, notify:OnSharedBookmarksFoldersAccessChanged, notify:OnSharedBookmarksFoldersAccessLost, notify:OnStandingSet, notify:OnStandingsModified, notify:OnAgentAdded, notify:OnLockedItemChange
    eve_testkit::attest(&[
        "notify:OnShipJumpCloneInstallationOffered",
        "notify:OnShipJumpCloneInstallationCanceled",
        "notify:OnShipJumpCloneInstallationDone",
        "notify:OnShipJumpCloneCacheInvalidated",
        "notify:OnSharedBookmarksFolderUpdated",
        "notify:OnSharedBookmarksFolderDeleted",
        "notify:OnSharedBookmarksFoldersAccessChanged",
        "notify:OnSharedBookmarksFoldersAccessLost",
        "notify:OnStandingSet",
        "notify:OnStandingsModified",
        "notify:OnAgentAdded",
        "notify:OnLockedItemChange",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7611, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7612, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("bystander6", "bystander6-secret");
    let p3 = ctx.db.create_character(uid3, "Bystander Six", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let _s3 = select(&ctx, 7613, "bystander6", "bystander6-secret", p3).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(500_000_000.0);
        }
        snap.skills.entry(P2).or_default().insert(24242, TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 });
    });

    // Create corporation via real RPCs so P1 (CEO) and P2 (member) share a corp
    let corp_rep = call(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Increment Six Corp".into()), PyRep::String("INC6".into())]).await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    let app_rep = call(&ctx, &mut s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())]).await;
    let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("app_id");
    call(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id)]).await;
    s2.corporation_id = Some(corp_id);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. Ship Clone Vat Bay: OfferShipCloneInstallation, CancelShipCloneInstallation, AcceptShipCloneInstallation
    let rorq_id = 920_001i64;
    let cvb_id = 920_002i64;
    let ozone_id = 920_003i64;
    seed_item(&ctx, rorq_id, 28352, P1, SYS_JITA as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cvb_id, 23735, P1, rorq_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, ozone_id, 16273, P1, rorq_id, InventoryFlag::Cargo, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cvb_id, true);
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.active_ship_id = rorq_id;
        }
        snap.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: P1, members: vec![P1, P2], ..Default::default() });
    });
    s1.ship_id = Some(rorq_id);
    s1.solar_system_id = Some(SYS_JITA);
    call(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(cvb_id)]).await;
    ctx.drain_notifications_for_char(P1);

    call(&ctx, &mut s1, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(P2)]).await;
    let n_offer_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_offer_p2, "OnShipJumpCloneInstallationOffered"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(rorq_id), PyRep::Bool(true),]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    call(&ctx, &mut s1, "jumpCloneSvc", "CancelShipCloneInstallation", &[]).await;
    let n_cancel_p1 = ctx.drain_notifications_for_char(P1);
    let n_cancel_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_cancel_p1, "OnShipJumpCloneInstallationCanceled"), vec![Vec::<PyRep>::new()]);
    assert_eq!(events_from(&n_cancel_p2, "OnShipJumpCloneInstallationCanceled"), vec![Vec::<PyRep>::new()]);

    call(&ctx, &mut s1, "jumpCloneSvc", "OfferShipCloneInstallation", &[PyRep::Integer(P2)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "jumpCloneSvc", "AcceptShipCloneInstallation", &[]).await;
    let n_acc_p1 = ctx.drain_notifications_for_char(P1);
    let n_acc_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_acc_p1, "OnShipJumpCloneInstallationDone"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(rorq_id), PyRep::Bool(true),]]
    );
    assert_eq!(events_from(&n_acc_p1, "OnShipJumpCloneCacheInvalidated"), vec![vec![PyRep::Integer(rorq_id), PyRep::Integer(P2)]]);
    assert_eq!(
        events_from(&n_acc_p2, "OnShipJumpCloneInstallationDone"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(rorq_id), PyRep::Bool(true),]]
    );
    assert_eq!(events_from(&n_acc_p2, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_acc_p1, "OnShipCloneJumpUpdate").is_empty());
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    // 2. Shared Bookmarks: AddFolder, AddToKnownFolders, RemoveFromKnownFolders, DeleteFolder
    let folder_rep = call(&ctx, &mut s1, "accessGroupBookmarkMgr", "AddFolder", &[PyRep::String("Shared Ops".into()), PyRep::Bool(true)]).await;
    let folder_id = folder_rep.dict_get("folderID").and_then(|v| v.as_i64()).expect("folderID");
    let n_f_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_f_p2, "OnSharedBookmarksFolderUpdated"),
        vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Integer(folder_id)]),
            PyRep::String("FolderChanged".into()),
            PyRep::Integer(corp_id),
        ])])]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    ctx.drain_notifications_for_char(P1);

    call(&ctx, &mut s2, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    let n_sub_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_sub_p2, "OnSharedBookmarksFoldersAccessChanged"), vec![vec![PyRep::Dict(vec![(PyRep::Integer(folder_id), PyRep::Integer(1),)])]]);
    ctx.drain_notifications_for_char(P1);

    call(&ctx, &mut s2, "accessGroupBookmarkMgr", "RemoveFromKnownFolders", &[PyRep::Integer(folder_id)]).await;
    let n_unsub_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_unsub_p2, "OnSharedBookmarksFoldersAccessLost"), vec![vec![PyRep::List(vec![PyRep::Integer(folder_id)])]]);

    call(&ctx, &mut s1, "accessGroupBookmarkMgr", "DeleteFolder", &[PyRep::Integer(folder_id)]).await;
    let n_del_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_del_p2, "OnSharedBookmarksFolderDeleted"), vec![vec![PyRep::Integer(folder_id)]]);
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    ctx.drain_notifications_for_char(P1);

    // 3. Standings & Agents: AddContact for NPC Agent -> OnAgentAdded, Plus mission completion -> OnStandingSet & OnStandingsModified
    let (dist_agent_id, dist_corp_id, dist_station_id, dist_sys_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id, a.station_id.unwrap_or(STATION_JITA), a.solar_system_id.unwrap_or(SYS_JITA)))
        .expect("SDE distribution agent");
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(dist_agent_id), PyRep::Real(5.0), PyRep::Bool(false), PyRep::Bool(false), PyRep::String(String::new())],
    )
    .await;
    let n_ag = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_ag, "OnAgentAdded"), vec![vec![PyRep::Integer(dist_agent_id)]]);

    // Colocate P1 with dist_agent_id and complete a courier mission -> OnStandingSet & OnStandingsModified
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
            ch.standings.insert(dist_corp_id, 2.0);
        }
    });
    s1.solar_system_id = Some(dist_sys_id);
    s1.station_id = Some(dist_station_id);

    let m_offer = call(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)]).await;
    let courier_mid = m_offer.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    call(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(courier_mid)]).await;
    let dest_st = m_offer.dict_get("destinationStationID").and_then(|v| v.as_i64()).unwrap_or(dist_station_id);
    let dest_sys = ctx.sde.stations.get(&dest_st).map(|s| s.solar_system_id).unwrap_or(dist_sys_id);
    seed_item(&ctx, 920_050, 3683, P1, dest_st, InventoryFlag::Hangar, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = dest_sys;
            ch.station_id = Some(dest_st);
            for it in snap.items.values_mut() {
                if it.owner_id == P1 && it.location_id != ch.active_ship_id {
                    it.location_id = dest_st;
                    it.flag = InventoryFlag::Hangar;
                }
            }
        }
        snap.item_custom_names.insert(920_050, format!("courier_cargo:{P1}:{courier_mid}"));
    });
    s1.solar_system_id = Some(dest_sys);
    s1.station_id = Some(dest_st);
    ctx.drain_notifications_for_char(P1);
    call(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(courier_mid)]).await;
    let n_msn = ctx.drain_notifications_for_char(P1);
    let st_set = events_from(&n_msn, "OnStandingSet");
    assert!(!st_set.is_empty(), "CompleteMission must emit OnStandingSet");
    assert!(
        st_set.iter().any(|s| s[0] == PyRep::Integer(dist_corp_id) && s[1] == PyRep::Integer(P1)),
        "OnStandingSet must include corp standing update: {st_set:?}"
    );
    let st_mod = events_from(&n_msn, "OnStandingsModified");
    assert_eq!(st_mod.len(), 1);
    assert!(matches!(&st_mod[0][0], PyRep::List(l) if !l.is_empty()));
    assert!(events_from(&n_msn, "OnNPCStandingChange").is_empty());

    // 4. Item Locking: ALSCLockItems -> OnLockedItemChange(itemID, ownerID, locationID)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = Some(STATION_JITA);
        }
    });
    s1.solar_system_id = Some(SYS_JITA);
    s1.station_id = Some(STATION_JITA);
    let lock_item_id = 920_099i64;
    let container_id = 920_098i64;
    seed_item(&ctx, container_id, 17366, corp_id, STATION_JITA, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, lock_item_id, 34, corp_id, container_id, InventoryFlag::Locked, 100, false);
    call(&ctx, &mut s1, "invbroker", "ALSCLockItems", &[PyRep::Integer(container_id), PyRep::List(vec![PyRep::Integer(lock_item_id)])]).await;
    let n_lock = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_lock, "OnLockedItemChange"), vec![vec![PyRep::Integer(lock_item_id), PyRep::Integer(corp_id), PyRep::Integer(container_id),]]);
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
}

// Folded submodule: notify_real_emitters_batch8
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8 {
    use super::*;

// Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 1: Seasons).
//
// Covers all 10 Group 1 Seasons parity rows:
// - `rpc:seasonManager.get_season_data_for_character`
// - `rpc:seasonManager.get_season_end_time`
// - `rpc:seasonManager.select_season`
// - `notify:OnSeasonStarted`
// - `notify:OnSeasonEnded`
// - `notify:OnSeasonSelectionCleared`
// - `notify:OnSeasonOmegaRewardsClaimed`
// - `notify:OnSeasonalGoalCompleted`
// - `notify:OnSeasonalGoalsReset`
// - `notify:OnChallengeExpired`


use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::season_manager::{SeasonalChallengeDef, SEASONAL_CHALLENGES_CONFIG_KEY};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const ERR_INVALID_SEASON: &str = "InvalidSeasonId";
const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";
const ERR_CHALLENGE_ALREADY_CLAIMED: &str = "ChallengeAlreadyClaimed";

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn login_docked(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    clone_grade: i32,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = clone_grade as u32;
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
        }
    });
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: rpc:seasonManager.get_season_data_for_character, rpc:seasonManager.get_season_end_time
// parity: rpc:seasonManager.select_season, notify:OnSeasonStarted, notify:OnSeasonEnded
// parity: notify:OnSeasonSelectionCleared, notify:OnChallengeExpired
#[tokio::test]
async fn notify_batch8_seasons_lifecycle_and_challenges() {
    eve_testkit::attest(&[
        "rpc:seasonManager.get_season_data_for_character",
        "rpc:seasonManager.get_season_end_time",
        "rpc:seasonManager.select_season",
        "notify:OnSeasonStarted",
        "notify:OnSeasonEnded",
        "notify:OnSeasonSelectionCleared",
        "notify:OnChallengeExpired",
    ]);

    let ctx = new_ctx();
    let mut unauth_sess = SessionState::new(9_100);
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_data_for_character", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "get_season_end_time", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(&ctx, &mut unauth_sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess_a = login_docked(&ctx, 9_101, "demo", "demo-secret", PILOT_A, 2).await;
    let mut sess_b = login_docked(&ctx, 9_102, "other", "other-secret", PILOT_B, 1).await;

    // Seed custom challenge definitions so challenge 1005 awards 25 points (crosses 2 goal milestones!)
    ctx.db.with_mut_snapshot(|snap| {
        let defs = vec![
            SeasonalChallengeDef {
                challenge_id: 1_005,
                season_id: 1,
                event_type: "ui_onboarding_step".into(),
                title: "Onboarding".into(),
                description: "Complete onboarding steps".into(),
                points: 25,
                max_progress: 2,
                client_event_driven: true,
                reward_type_id: 0,
                reward_quantity: 0,
                is_dormant: false,
                ..Default::default()
            },
            SeasonalChallengeDef {
                challenge_id: 1_006,
                season_id: 1,
                event_type: "npc_kills".into(),
                title: "Bounty Hunter".into(),
                description: "Destroy pirate vessels".into(),
                points: 10,
                max_progress: 5,
                client_event_driven: false,
                reward_type_id: 0,
                reward_quantity: 0,
                is_dormant: false,
                ..Default::default()
            },
        ];
        snap.service_state_blobs.insert(
            SEASONAL_CHALLENGES_CONFIG_KEY.into(),
            serde_json::to_string(&defs).expect("serialize defs"),
        );
    });

    // 1. Baseline get_season_data_for_character & get_season_end_time
    let data_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character");
    let first_season_id = data_init
        .dict_get("season_id")
        .or_else(|| {
            if let PyRep::List(items) = &data_init {
                items.first().and_then(|it| it.dict_get("season_id"))
            } else {
                None
            }
        })
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    assert!(first_season_id > 0, "expected active season_id > 0, got {data_init:?}");

    let end_time_init = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time");
    let end_ft_val = end_time_init.as_i64().expect("integer filetime end_time");
    assert!(end_ft_val > ctx.now_blueos_filetime_ticks());

    // Negative: non-GM cannot start or end a season via slash command
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "publicQaToolsServer",
            "SlashCmd",
            &[PyRep::String("/season start 88 14".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    // 2. Start Season 88 via GM /season start 88 10 -> emits OnSeasonStarted
    sess_a.role |= ROLE_GM_DEFAULT;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let slash_start = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/season start 88 10".into())],
    )
    .await
    .expect("/season start 88 10");
    assert_eq!(slash_start.dict_get("seasonID").and_then(|v| v.as_i64()), Some(88));
    assert_eq!(slash_start.dict_get("started").and_then(|v| v.as_bool()), Some(true));

    let notifs_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let notifs_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&notifs_start_a, "OnSeasonStarted").len(), 0);
    assert_eq!(find_notif(&notifs_start_b, "OnSeasonStarted").len(), 0);

    // Negative: invalid season IDs and invalid slash arguments rejected
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season start 0 10".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidSlashArgument"
    );
    let err_999 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err_999.code.as_str(), ERR_INVALID_SEASON);
    let err_neg = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-5)])
        .await
        .unwrap_err();
    assert_eq!(err_neg.code.as_str(), ERR_INVALID_SEASON);

    // 3. select_season(88) selects Season 88, then select_season(None) & select_season(0) emit OnSeasonSelectionCleared
    let sel_ok = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("select_season(88)");
    assert_eq!(sel_ok, PyRep::Bool(true));
    let data_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_data_for_character", &[])
        .await
        .expect("get_season_data_for_character(88)");
    assert_eq!(data_88.dict_get("season_id").and_then(|v| v.as_i64()), Some(88));
    let end_88 = rpc(&ctx, &mut sess_a, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time(88)");
    assert!(end_88.as_i64().unwrap_or(0) > ctx.now_blueos_filetime_ticks());

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let clear_rep = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::None])
        .await
        .expect("select_season(None)");
    assert_eq!(clear_rep, PyRep::Bool(true));
    let clear_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&clear_notifs, "OnSeasonSelectionCleared").len(), 0);

    // Re-select Season 88 for both pilots, then end Season 88 -> emits OnSeasonEnded & OnSeasonSelectionCleared
    rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("re-select 88 A");
    rpc(&ctx, &mut sess_b, "seasonManager", "select_season", &[PyRep::Integer(88)])
        .await
        .expect("re-select 88 B");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    let end_rep = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String("/season end 88".into())],
    )
    .await
    .expect("/season end 88");
    assert_eq!(end_rep.dict_get("ended").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(end_rep.dict_get("clearedCharacters").and_then(|v| v.as_i64()), Some(2));

    let end_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let end_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&end_notifs_a, "OnSeasonEnded").len(), 0);
    assert_eq!(find_notif(&end_notifs_a, "OnSeasonSelectionCleared").len(), 0);
    assert_eq!(find_notif(&end_notifs_b, "OnSeasonEnded").len(), 0);
    assert_eq!(find_notif(&end_notifs_b, "OnSeasonSelectionCleared").len(), 0);

    // Negative: selecting invalid season -1 fails with CustomInfo; ending unknown season fails
    let err_sel0 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(-1)])
        .await
        .unwrap_err();
    assert_eq!(err_sel0.code.as_str(), ERR_INVALID_SEASON);
    assert_eq!(
        rpc(&ctx, &mut sess_a, "slash", "SlashCmd", &[PyRep::String("/season end 999999".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidSeasonId"
    );

    // 4. Complete challenge 1005 (25 pts) -> claim_challenge_rewards emits OnSeasonalGoalCompleted
    // and (for Omega PILOT_A) OnSeasonOmegaRewardsClaimed, while Alpha PILOT_B does NOT receive Omega claim!
    let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges A");
    let _ = rpc(&ctx, &mut sess_b, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges B");

    // Negative: claiming incomplete challenge fails with ChallengeNotCompleted
    let err_uncomp = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_uncomp.code.as_str(), ERR_CHALLENGE_NOT_COMPLETED);

    for _ in 0..2 {
        rpc(
            &ctx,
            &mut sess_a,
            "seasonManager",
            "process_client_event",
            &[PyRep::String("ui_onboarding_step".into())],
        )
        .await
        .expect("complete challenge 1005 A");
        rpc(
            &ctx,
            &mut sess_b,
            "seasonManager",
            "process_client_event",
            &[PyRep::String("ui_onboarding_step".into())],
        )
        .await
        .expect("complete challenge 1005 B");
    }

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    let claim_a = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .expect("claim_challenge_rewards A");
    assert_eq!(claim_a.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(claim_a.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(25));

    let claim_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let goal_args = find_notif(&claim_notifs_a, "OnSeasonalGoalCompleted");
    assert_eq!(goal_args.len(), 2);
    assert_eq!(goal_args[0].as_i64(), Some(0));
    assert_eq!(goal_args[1].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(goal_args[1].dict_get("points_required").and_then(|v| v.as_i64()), Some(10));

    let omega_args = find_notif(&claim_notifs_a, "OnSeasonOmegaRewardsClaimed");
    assert_eq!(omega_args.len(), 1);
    assert_eq!(omega_args[0], PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)]));

    // Alpha pilot B gets OnSeasonalGoalCompleted but NOT OnSeasonOmegaRewardsClaimed
    let _ = rpc(&ctx, &mut sess_b, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .expect("claim_challenge_rewards B");
    let claim_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(find_notif(&claim_notifs_b, "OnSeasonalGoalCompleted").len(), 2);
    assert!(
        !claim_notifs_b.iter().any(|(ev, _)| ev == "OnSeasonOmegaRewardsClaimed"),
        "Alpha pilot must not receive OnSeasonOmegaRewardsClaimed"
    );

    // Negative: double-claiming already claimed challenge fails with ChallengeAlreadyClaimed
    let err_claimed = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_claimed.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

    // 5. Reset seasonal goals via /season reset_goals -> emits OnSeasonalGoalsReset and zeroes points
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let reset_rep = rpc(
        &ctx,
        &mut sess_a,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/season reset_goals {PILOT_A}"))],
    )
    .await
    .expect("/season reset_goals");
    assert_eq!(reset_rep.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(0));
    let reset_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&reset_notifs, "OnSeasonalGoalsReset").len(), 0);
    // Claimed challenge must NOT be re-armed by /season reset_goals before expiration
    let err_rearm = rpc(&ctx, &mut sess_a, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1_005)])
        .await
        .unwrap_err();
    assert_eq!(err_rearm.code.as_str(), ERR_CHALLENGE_ALREADY_CLAIMED);

    // Negative: resetting goals for non-existent character fails
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "slash",
            "SlashCmd",
            &[PyRep::String("/season reset_goals 999888777".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );

    // 6. Advance virtual clock by 35 days (> 48h challenge expiration) -> fill_up_and_return_challenges_for_character
    // refreshes expired challenges and emits OnChallengeExpired(expired_id, new_challenge_kv)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(35 * 86_400);
    let _ = rpc(&ctx, &mut sess_a, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges after expiry");
    let exp_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let exp_args = find_notif(&exp_notifs, "OnChallengeExpired");
    assert_eq!(exp_args.len(), 2);
    assert!(exp_args[0].as_i64().unwrap_or(0) > 0);
    assert!(
        exp_args[1].dict_get("challenge_id").is_some()
            || exp_args[1].dict_get("challengeID").is_some(),
        "OnChallengeExpired arg 1 must be replacement challenge KeyVal, got {:?}",
        exp_args[1]
    );

    // 7. Time-driven season transitions: advance clock past a future season's start_time_ft and end_time_ft
    // -> OnSeasonStarted, OnSeasonEnded, and OnSeasonSelectionCleared fire from the world tick without Get* calls
    let now_ft = ctx.now_blueos_filetime_ticks();
    let future_start_ft = now_ft + 100 * 10_000_000;
    let future_end_ft = now_ft + 200 * 10_000_000;
    ctx.db.with_mut_snapshot(|snap| {
        let mut catalog: std::collections::BTreeMap<i64, eve_server::services::season_manager::SeasonConfig> =
            snap.service_state_blobs
                .get(eve_server::services::season_manager::SEASONS_CATALOG_KEY)
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
        if let Some(mut cfg) = catalog.values().next().cloned() {
            cfg.season_id = 99;
            cfg.season_name = "Timed Season 99".into();
            cfg.start_time_ft = future_start_ft;
            cfg.end_time_ft = future_end_ft;
            catalog.insert(99, cfg);
            snap.service_state_blobs.insert(
                eve_server::services::season_manager::SEASONS_CATALOG_KEY.into(),
                serde_json::to_string(&catalog).unwrap(),
            );
        }
    });
    // Initialize announced season state snapshot while season 99 is still upcoming
    ctx.advance_secs(1);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // Advance 120s past future_start_ft -> OnSeasonStarted emitted automatically
    ctx.advance_secs(120);
    let timed_start_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timed_start_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(
        timed_start_a.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
        "expected OnSeasonStarted on clock advance past start_time_ft"
    );
    assert!(
        timed_start_b.iter().any(|(ev, _)| ev == "OnSeasonStarted"),
        "expected OnSeasonStarted for pilot B on clock advance past start_time_ft"
    );

    // Select season 99 for PILOT_A, then advance 120s past future_end_ft -> OnSeasonEnded + OnSeasonSelectionCleared
    let sel_99 = rpc(&ctx, &mut sess_a, "seasonManager", "select_season", &[PyRep::Integer(99)])
        .await
        .expect("select_season(99)");
    assert_eq!(sel_99, PyRep::Bool(true));
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    ctx.advance_secs(120);
    let timed_end_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timed_end_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(
        timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
        "expected OnSeasonEnded on clock advance past end_time_ft"
    );
    assert!(
        timed_end_b.iter().any(|(ev, _)| ev == "OnSeasonEnded"),
        "expected OnSeasonEnded for pilot B on clock advance past end_time_ft"
    );
    assert!(
        timed_end_a.iter().any(|(ev, _)| ev == "OnSeasonSelectionCleared"),
        "expected OnSeasonSelectionCleared for pilot A when selected season 99 ended"
    );
}

}

// Folded submodule: notify_real_emitters_batch8_part2
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part2 {
    use super::*;

// Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 2: HyperNet Raffles & PLEX Vault).
//
// Covers 7 Group 2 HyperNet Raffles & PLEX Vault parity rows:
// - `rpc:raffleMgr.QA_SeedRaffles`
// - `notify:OnRaffleCreatedServer`
// - `notify:OnRaffleFinishedServer`
// - `notify:OnRaffleUpdatedServer`
// - `notify:OnTicketsUpdatedServer`
// - `notify:OnPlexDeposited`
// - `notify:OnPlexItemAddedToInventory`


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::hypernet_raffle_ops::{sweep_expired_raffles, HYPERCORE_TYPE_ID};
use eve_server::services::market_contracts::PLEX_TYPE_ID;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(77));
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn login_docked(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
        }
    });
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: rpc:raffleMgr.QA_SeedRaffles, notify:OnRaffleCreatedServer, notify:OnRaffleFinishedServer
// parity: notify:OnRaffleUpdatedServer, notify:OnTicketsUpdatedServer, notify:OnPlexDeposited
// parity: notify:OnPlexItemAddedToInventory
#[tokio::test]
async fn notify_batch8_hypernet_raffles_and_plex_vault() {
    eve_testkit::attest(&[
        "rpc:raffleMgr.QA_SeedRaffles",
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleFinishedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnTicketsUpdatedServer",
        "notify:OnPlexDeposited",
        "notify:OnPlexItemAddedToInventory",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_200);
    assert_eq!(
        rpc(&ctx, &mut unauth, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(2)])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(1), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_docked(&ctx, 9_202, "other", "other-secret", PILOT_B).await;

    // 1. QA_SeedRaffles role check + positive seeding
    assert_eq!(
        rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    sess_a.role |= ROLE_GM_DEFAULT;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let seeded_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(3)])
        .await
        .expect("QA_SeedRaffles");
    let PyRep::List(seeded_list) = seeded_rep else {
        panic!("expected List from QA_SeedRaffles, got {seeded_rep:?}");
    };
    assert_eq!(seeded_list.len(), 3);
    let seeded_rid_0 = seeded_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let seeded_iid_0 = seeded_list[0].dict_get("item_id").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(seeded_rid_0 > 0 && seeded_iid_0 > 0);
    assert!(seeded_list[0].dict_get("sold_tickets").is_some(), "QA_SeedRaffles must include sold_tickets");
    let seeded_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_rids: std::collections::BTreeSet<i64> = seeded_notifs
        .iter()
        .filter(|(n, _)| n == "OnRaffleCreatedServer")
        .filter_map(|(_, a)| a.first().and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(
        created_rids.len(),
        3,
        "QA_SeedRaffles(3) must emit OnRaffleCreatedServer for all 3 seeded raffles"
    );
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&seeded_iid_0).expect("seeded item in snap.items");
        assert_eq!((it.owner_id, it.location_id, it.flag), (-52_568, -52_568, InventoryFlag::Locked));
    });
    let _ = rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(seeded_rid_0), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets on seeded raffle");
    let claimed_seeded = rpc(&ctx, &mut sess_b, "raffleProxy", "AwardItem", &[PyRep::Integer(seeded_rid_0)])
        .await
        .expect("AwardItem on seeded raffle");
    assert_eq!(claimed_seeded.as_i64(), Some(seeded_iid_0));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&seeded_iid_0).expect("awarded seeded item");
        assert_eq!((it.owner_id, it.location_id, it.flag), (PILOT_B, JITA_STATION, InventoryFlag::Hangar));
    });
    let seeded_tc = seeded_list[0].dict_get("ticket_count").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(
        [8, 16, 48, 512].contains(&seeded_tc),
        "QA_SeedRaffles ticket_count must be in [8, 16, 48, 512], got {seeded_tc}"
    );
    assert_eq!(seeded_list[0].dict_get("owner_id").and_then(|v| v.as_i64()), Some(PILOT_A));
    assert_eq!(seeded_list[0].dict_get("location_id").and_then(|v| v.as_i64()), Some(JITA_STATION));

    // Undocked GM session must fail closed (no Jita fallback)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
        }
    });
    let mut undocked_gm = sess_a.clone();
    undocked_gm.station_id = None;
    assert!(
        rpc(&ctx, &mut undocked_gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
            .await
            .is_err(),
        "QA_SeedRaffles must fail closed when station_id is None"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });

    // 2. CreateRaffle -> emits OnRaffleCreatedServer(raffle_id, raffle_kv)
    let (item_1, item_2, cores_id) = (820_000_001_i64, 820_000_002_i64, 820_000_003_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, qty, sing) in [
            (item_1, 587u32, 1, true),
            (item_2, 587u32, 1, true),
            (cores_id, HYPERCORE_TYPE_ID, 50, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT_A,
                    location_id: JITA_STATION,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: sing,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Negative: invalid ticket count (7 not in [8, 16, 48, 512]) and missing item rejected
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(item_1), PyRep::Integer(7), PyRep::Real(100_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRaffleTicketCount"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(999_888_777), PyRep::Integer(8), PyRep::Real(100_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let create_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(item_1), PyRep::Integer(8), PyRep::Real(100_000.0)],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = create_rep
        .dict_get("raffle_id")
        .and_then(|v| v.as_i64())
        .expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args.len(), 2);
    assert_eq!(created_args[0].as_i64(), Some(raffle_id));
    assert_eq!(
        created_args[1].dict_get("ticket_count").and_then(|v| v.as_i64()),
        Some(8)
    );
    // Calling QA_SeedRaffles after CreateRaffle must not collide with or overwrite raffle_id
    let post_seed_rep = rpc(&ctx, &mut sess_a, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1)])
        .await
        .expect("QA_SeedRaffles after CreateRaffle");
    let PyRep::List(post_seed_list) = post_seed_rep else {
        panic!("expected List from QA_SeedRaffles");
    };
    let post_seed_rid = post_seed_list[0].dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_ne!(post_seed_rid, raffle_id, "QA_SeedRaffles must not collide with CreateRaffle raffle_id");
    let live_raffle = rpc(&ctx, &mut sess_a, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
        .await
        .expect("GetRaffle live");
    assert_eq!(live_raffle.dict_get("item_id").and_then(|v| v.as_i64()), Some(item_1));

    // 3. BuyTicket & BuyRandomTickets -> emits OnRaffleUpdatedServer & OnTicketsUpdatedServer,
    // and buying the final ticket emits OnRaffleFinishedServer(raffle_id, winning_ticket)
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "raffleProxy",
        "BuyTicket",
        &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
    )
    .await
    .expect("BuyTicket 0");

    // Negative: buying already-sold ticket 0 fails with TicketUnavailable
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyTicket",
            &[PyRep::Integer(raffle_id), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );

    let upd_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let upd_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let upd_args = find_notif(&upd_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(upd_args.len(), 2);
    assert_eq!(upd_args[0].as_i64(), Some(raffle_id));
    assert_eq!(
        upd_args[1].dict_get("tickets_remaining").and_then(|v| v.as_i64()),
        Some(7)
    );
    let tix_args = find_notif(&upd_notifs_a, "OnTicketsUpdatedServer");
    assert_eq!(tix_args.len(), 1);
    assert_eq!(
        tix_args[0],
        PyRep::Dict(vec![(PyRep::Integer(raffle_id), PyRep::Integer(1))])
    );

    // Buy remaining 7 tickets -> finishes raffle and emits OnRaffleFinishedServer
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    let _ = rpc(
        &ctx,
        &mut sess_b,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(7)],
    )
    .await
    .expect("BuyRandomTickets 7");

    let fin_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let fin_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let fin_args_a = find_notif(&fin_notifs_a, "OnRaffleFinishedServer");
    let fin_args_b = find_notif(&fin_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(fin_args_a.len(), 2);
    assert_eq!(fin_args_a[0].as_i64(), Some(raffle_id));
    assert!(fin_args_a[1].as_i64().is_some_and(|t| (0..8).contains(&t)));
    assert_eq!(fin_args_a, fin_args_b);

    // Negative: buying on finished raffle fails with RaffleFinished; also verify expiry sweep emits None winner
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "RaffleFinished"
    );

    let create_2 = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(item_2), PyRep::Integer(8), PyRep::Real(100_000.0), PyRep::Integer(60)],
    )
    .await
    .expect("CreateRaffle 2");
    let raffle_2 = create_2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    ctx.advance_secs(120);
    assert_eq!(sweep_expired_raffles(&ctx), 1);
    let exp_raffle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&exp_raffle_notifs, "OnRaffleFinishedServer"),
        &[PyRep::Integer(raffle_2), PyRep::None]
    );

    // 4. PLEX Vault deposit & withdraw: positive + negative failure notifications
    let (plex_item_id, tritanium_id) = (820_000_010_i64, 820_000_011_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            plex_item_id,
            InventoryItemRecord {
                item_id: plex_item_id,
                type_id: PLEX_TYPE_ID,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Negative deposit cases -> synchronous RPC Err without unmatchable OnPlexDepositFailed
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(tritanium_id), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "NotPlex"
    );
    let dep_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!dep_fail_1.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "DepositPlexToVault",
            &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(9_999)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughQuantity"
    );
    let dep_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!dep_fail_2.iter().any(|(e, _)| e == "OnPlexDepositFailed"));

    // Positive deposit -> emits OnPlexDeposited([entity_id])
    let dep_ok = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(JITA_STATION), PyRep::Integer(plex_item_id), PyRep::Integer(250)],
    )
    .await
    .expect("DepositPlexToVault 250");
    assert!(dep_ok.as_i64().unwrap_or(0) > 0);
    let dep_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let dep_args = find_notif(&dep_ok_notifs, "OnPlexDeposited");
    assert_eq!(dep_args.len(), 1);
    assert_eq!(dep_args[0], dep_ok);

    // Negative withdraw cases -> synchronous RPC Err without unmatchable OnPlexWithdrawFailed
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(99_999)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughPlex"
    );
    let wd_fail_1 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!wd_fail_1.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "invbroker",
            "WithdrawPlexFromVault",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let wd_fail_2 = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!wd_fail_2.iter().any(|(e, _)| e == "OnPlexWithdrawFailed"));

    // Positive withdraw -> emits OnPlexItemAddedToInventory([item_id, tx_id]) matching reply reference
    let wd_ok = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(100)],
    )
    .await
    .expect("WithdrawPlexFromVault 100");
    let withdrawn_item_id = wd_ok
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("withdrawn itemID");
    let wd_ref = wd_ok
        .dict_get("reference")
        .and_then(|v| v.as_i64())
        .expect("withdraw reference");
    assert_eq!(
        wd_ok.dict_get("remainingVaultBalance").and_then(|v| v.as_i64()),
        Some(650)
    );
    let wd_ok_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let wd_args = find_notif(&wd_ok_notifs, "OnPlexItemAddedToInventory");
    assert_eq!(wd_args.len(), 2);
    assert_eq!(wd_args[0].as_i64(), Some(withdrawn_item_id));
    assert_eq!(wd_args[1].as_i64(), Some(wd_ref));
}

}

// Folded submodule: notify_real_emitters_batch8_part3
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part3 {
    use super::*;

// Real server-side emission & RPC tests for Notify/RPC Batch 8 (Part 3: Operations & AIR NPE).
//
// Covers the V24-triggered Group 3 Operations / NPE parity rows:
// - `notify:OnOperationTaskSkipped`
// - `notify:OnAirNpeStateChanged`


use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::operations_manager_helpers::get_operation_defs;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

// parity: notify:OnOperationTaskSkipped, notify:OnAirNpeStateChanged
#[tokio::test]
async fn notify_batch8_operations_npe_and_client_node_graphs() {
    eve_testkit::attest(&[
        "notify:OnOperationTaskSkipped",
        "notify:OnAirNpeStateChanged",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_300);
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(10)],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut unauth,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
        )
        .await
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    let mut sess = SessionState::new(9_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID");
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: non-SDE graph 999_999_888 is rejected with NodeGraphNotFound
    let err_ng = rpc(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(999_999_888),
            PyRep::Integer(1),
            PyRep::String("noop_msg".into()),
            PyRep::Dict(Vec::new()),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err_ng.code, "NodeGraphNotFound");

    // 1. start_tutorial via process_client_event -> emits OnAirNpeStateChanged([PILOT_A, 1])
    let start_tut = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("start_tutorial".into()),
            PyRep::None,
            PyRep::Dict(vec![(PyRep::String("state_id".into()), PyRep::Integer(1))]),
        ],
    )
    .await
    .expect("start_tutorial");
    assert_eq!(start_tut, PyRep::Bool(true));

    let tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&tut_notifs, "OnAirNpeStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
    );

    // 2. process_client_graph_message on real SDE graph (3) does NOT echo client input back
    let _ = rpc(
        &ctx,
        &mut sess,
        "node_graph",
        "process_client_graph_message",
        &[
            PyRep::Integer(3),
            PyRep::Integer(5),
            PyRep::String("node_completed".into()),
            PyRep::Dict(vec![(PyRep::String("stage".into()), PyRep::Integer(2))]),
        ],
    )
    .await
    .expect("process_client_graph_message on SDE graph 3");
    let ng_msg_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(
        !ng_msg_notifs.iter().any(|(ev, _)| ev == "OnClientNodeGraphMessage" || ev == "OnClientNodeGraphUpdate"),
        "must not echo client node graph input back: {ng_msg_notifs:?}"
    );

    // 3. skip_tutorial -> emits OnAirNpeStateChanged([PILOT_A, 3])
    let skip_tut = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .expect("skip_tutorial");
    assert_eq!(skip_tut, PyRep::Bool(true));

    let skip_tut_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&skip_tut_notifs, "OnAirNpeStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(3)]
    );

    // 4. Discover a real operation with tasks from SDE -> activate & skip_task -> OnOperationTaskSkipped
    let op_defs = get_operation_defs(&ctx.sde);
    let empty_done = std::collections::HashSet::new();
    let is_skippable = |tid: i64| {
        op_defs.task_is_client_skippable.contains(&tid) || op_defs.task_is_optional.contains(&tid)
    };
    let (&op_id, tasks) = op_defs
        .op_tasks
        .iter()
        .filter(|(id, ts)| {
            op_defs.are_prerequisites_met(**id, &empty_done)
                && ts.len() >= 2
                && is_skippable(ts[0])
        })
        .min_by_key(|(id, _)| **id)
        .or_else(|| {
            op_defs
                .op_tasks
                .iter()
                .filter(|(id, ts)| op_defs.are_prerequisites_met(**id, &empty_done) && ts.len() >= 2)
                .min_by_key(|(id, _)| **id)
        })
        .or_else(|| {
            op_defs
                .op_tasks
                .iter()
                .filter(|(id, ts)| op_defs.are_prerequisites_met(**id, &empty_done) && !ts.is_empty())
                .min_by_key(|(id, _)| **id)
        })
        .expect("at least one prerequisite-free operation with tasks in SDE");
    let cat_id = op_defs
        .op_default_category
        .get(&op_id)
        .copied()
        .or_else(|| op_defs.sde_categories.iter().copied().next())
        .unwrap_or(31);
    let first_task_id = tasks[0];
    let expected_next_task = tasks.get(1).copied().map(PyRep::Integer).unwrap_or(PyRep::None);
    if !op_defs.client_completable_tasks.contains(&first_task_id) {
        sess.role |= eve_machonet::service_roles::ROLE_GMH;
    }

    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let act_op = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(op_id),
            PyRep::Integer(SYS_JITA as i64),
            PyRep::Integer(cat_id),
        ],
    )
    .await
    .expect("activate_operation_in_solar_system");
    assert_eq!(act_op, PyRep::Bool(true));
    let _ = drain_all(&ctx, &mut sess, PILOT_A);

    // Negative: skip_task with invalid task ID fails with CustomInfo
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "CustomInfo"
    );

    let skip_task_rep = rpc(
        &ctx,
        &mut sess,
        "operationsManager",
        "skip_task",
        &[
            PyRep::Integer(cat_id),
            PyRep::Integer(op_id),
            PyRep::Integer(first_task_id),
        ],
    )
    .await
    .expect("skip_task");
    assert_eq!(skip_task_rep, PyRep::Bool(true));
    let skip_task_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let task_skipped_args = find_notif(&skip_task_notifs, "OnOperationTaskSkipped");
    assert_eq!(
        task_skipped_args,
        &[
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(cat_id),
                PyRep::Integer(op_id),
                PyRep::Integer(first_task_id),
                expected_next_task,
            ])]),
            PyRep::Bool(true),
        ]
    );
}

}

// Folded submodule: notify_real_emitters_batch8_part4
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part4 {
    use super::*;

// Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
//
// Covers 14 Group 4 parity rows across two test functions (`<= 15` tags each):
// - Test 1 (`6` rows): `OnJamStart`, `OnEwarEnd`, `OnJumpCloakUpdated`,
//   `OnJumpCloakCancelled`, `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
// - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
//   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
//   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, Some(bound_id), method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
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
        ..Default::default()
    }
}

fn grant_all_skills(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (&tid, ty) in &ctx.sde.types {
            if ty.category_id == 16 {
                sk.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    grant_all_skills(ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.corporation_id = 1_000_125;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(1_000_125);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    cs.capacitor = cs.max_capacitor.max(10_000.0);
    cs.max_capacitor = cs.max_capacitor.max(10_000.0);
    cs.is_cloaked = false;
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    let slim = ctx.build_ship_slim_meta(char_id, ship_type);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
    ctx.invalidate_ship_fitting(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: notify:OnJamStart, notify:OnEwarEnd, notify:OnJumpCloakUpdated
// parity: notify:OnJumpCloakCancelled, notify:OnInvulnCancelled, notify:OnRestoringInvulnUpdated
#[tokio::test]
async fn notify_batch8_combat_ewar_cloak_and_invuln() {
    eve_testkit::attest(&[
        "notify:OnJamStart",
        "notify:OnEwarEnd",
        "notify:OnJumpCloakUpdated",
        "notify:OnJumpCloakCancelled",
        "notify:OnInvulnCancelled",
        "notify:OnRestoringInvulnUpdated",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (840_000_001_i64, 840_000_002_i64);

    // 1. Undock -> OnRestoringInvulnUpdated([ship_a, end_ft, 300_000_000]), then CmdGotoDirection -> OnInvulnCancelled([ship_a])
    let mut sess_a = SessionState::new(9_401);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID A");
    grant_all_skills(&ctx, PILOT_A);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.solar_system_id = SYS_JITA;
            ch.active_ship_id = ship_a;
        }
        snap.items.insert(
            ship_a,
            make_item(ship_a, 623, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    sess_a.station_id = Some(JITA_STATION);
    sess_a.ship_id = Some(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(&ctx, &mut sess_a, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let undock_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let invuln_args = find_notif(&undock_notifs, "OnRestoringInvulnUpdated");
    assert_eq!(invuln_args.len(), 3);
    assert_eq!(invuln_args[0].as_i64(), Some(ship_a));
    assert_eq!(invuln_args[2].as_i64(), Some(300_000_000));

    let beyonce_jita = ctx
        .bind_object(&mut sess_a, "beyonce", SYS_JITA as i64)
        .expect("bind beyonce jita");
    rpc_bound(
        &ctx,
        &mut sess_a,
        "beyonce",
        beyonce_jita,
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdGotoDirection breaks undock invuln");
    let invuln_cancel_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&invuln_cancel_notifs, "OnInvulnCancelled"),
        &[PyRep::Integer(ship_a)]
    );

    // Negative: repeating CmdGotoDirection when invulnerability is already broken does not re-emit OnInvulnCancelled
    rpc_bound(
        &ctx,
        &mut sess_a,
        "beyonce",
        beyonce_jita,
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
    )
    .await
    .expect("second CmdGotoDirection");
    let repeat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!repeat_notifs.iter().any(|(e, _)| e == "OnInvulnCancelled"));

    // 2. Stargate jump -> OnJumpCloakUpdated & OnRestoringInvulnUpdated(600_000_000), then movement -> OnJumpCloakCancelled
    ctx.advance_secs(12);
    let (gate_id, gate_pos) = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == SYS_JITA)
        .map(|j| (j.stargate_id, j.position))
        .expect("stargate out of Jita");
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_handle.set_ball_position(ship_a, gate_pos).await;

    // Negative: unknown stargate ID rejected
    assert_eq!(
        rpc_bound(
            &ctx,
            &mut sess_a,
            "beyonce",
            beyonce_jita,
            "CmdStargateJump",
            &[PyRep::Integer(999_999_888)],
        )
        .await
        .unwrap_err()
        .code,
        "UnknownStargate"
    );

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc_bound(
        &ctx,
        &mut sess_a,
        "beyonce",
        beyonce_jita,
        "CmdStargateJump",
        &[PyRep::Integer(gate_id)],
    )
    .await
    .expect("CmdStargateJump");
    let jump_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let jc_args = find_notif(&jump_notifs, "OnJumpCloakUpdated");
    assert_eq!(jc_args.len(), 3);
    assert_eq!(jc_args[0].as_i64(), Some(ship_a));
    assert_eq!(jc_args[2].as_i64(), Some(600_000_000));
    assert_eq!(
        find_notif(&jump_notifs, "OnRestoringInvulnUpdated")[2].as_i64(),
        Some(600_000_000)
    );

    let new_sys = sess_a.solar_system_id.expect("new solar_system_id");
    let beyonce_new = ctx
        .bind_object(&mut sess_a, "beyonce", new_sys as i64)
        .expect("bind beyonce new sys");
    rpc_bound(
        &ctx,
        &mut sess_a,
        "beyonce",
        beyonce_new,
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdGotoDirection breaks gate cloak");
    let jc_cancel_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&jc_cancel_notifs, "OnJumpCloakCancelled"),
        &[PyRep::Integer(ship_a)]
    );

    // Negative: repeating movement after gate cloak broke does not re-emit OnJumpCloakCancelled
    rpc_bound(
        &ctx,
        &mut sess_a,
        "beyonce",
        beyonce_new,
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
    )
    .await
    .expect("second move after gate cloak broke");
    assert!(
        !drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnJumpCloakCancelled")
    );

    // 3. Cloak activate & Deactivate -> OnCloakModuleForceDeactivated
    let (cloak_id, web_id, ecm_id, scram_id, gd_id) = (
        840_000_010_i64,
        840_000_011_i64,
        840_000_012_i64,
        840_000_013_i64,
        840_000_014_i64,
    );
    let ecm_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 201 && t.default_attributes.get(&238).copied().unwrap_or(0.0) > 0.0)
        .map(|t| t.type_id)
        .expect("ECM jammer type in SDE");
    let scram_type_id = 447u32; // Warp Scrambler I
    let gd_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 291
                && (t.effect_ids.contains(&6425) || t.name.contains("Guidance"))
                && t.default_attributes.get(&54).copied().unwrap_or(0.0) >= 8_000.0
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("Guidance Disruptor type in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_id,
            make_item(cloak_id, 11_370, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            web_id,
            make_item(web_id, 526, PILOT_A, ship_a, InventoryFlag::MedSlot0, 1, true),
        );
        let mut ecm_item = make_item(ecm_id, ecm_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot1, 1, true);
        for aid in [238u16, 239, 240, 241] {
            ecm_item.dynamic_attributes.insert(aid, 1_000.0);
        }
        snap.items.insert(ecm_id, ecm_item);
        snap.items.insert(
            scram_id,
            make_item(scram_id, scram_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot2, 1, true),
        );
        snap.items.insert(
            gd_id,
            make_item(gd_id, gd_type_id, PILOT_A, ship_a, InventoryFlag::MedSlot3, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);

    // Negative: deactivating inactive cloak is idempotent and emits no OnCloakModuleForceDeactivated
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(cloak_id)])
            .await
            .is_ok()
    );
    assert!(
        !drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnCloakModuleForceDeactivated")
    );

    // Move ship_a far from any stargate (> 2000m) so cloak activates cleanly
    let new_handle = ctx.systems.get_or_spawn(new_sys);
    let _ = new_handle
        .set_ball_position(ship_a, DVec3::new(100_000.0, 0.0, 0.0))
        .await;

    ctx.advance_secs(35);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)])
        .await
        .expect("Activate Cloak");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(cloak_id)])
        .await
        .expect("Deactivate Cloak");
    let decloak_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !decloak_notifs
            .iter()
            .any(|(e, _)| e == "OnCloakModuleForceDeactivated"),
        "voluntary Deactivate must not emit OnCloakModuleForceDeactivated"
    );

    // 4. Webifier Deactivate -> OnEwarEnd; ECM Activate -> OnJamStart
    let mut sess_b = login_in_space(
        &ctx,
        9_402,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        new_sys,
        DVec3::new(105_000.0, 0.0, 0.0),
    )
    .await;

    // Negative: activating ECM without target lock fails with TargetNotLocked or TargetRequired
    assert!(
        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(ecm_id)])
            .await
            .is_err()
    );

    if let Ok(Some(mut cs)) = new_handle.get_combat_state(ship_a).await {
        cs.decloak_targeting_delay_remaining_s = 0.0;
        let _ = new_handle.set_combat_state(ship_a, cs).await;
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)])
        .await
        .expect("AddTarget");
    let _ = new_handle
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_id),
            PyRep::String("defaultEffect".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("Activate Stasis Webifier I");
    let web_act_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !web_act_notifs.iter().any(|(e, _)| e == "OnEffectMessage"),
        "module activation must not emit unverified OnEffectMessage"
    );

    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    // Negative (B8-01): victim PILOT_B cannot cleanse attacker's web by calling Deactivate(web_id)
    assert_eq!(
        rpc(&ctx, &mut sess_b, "dogmaIM", "Deactivate", &[PyRep::Integer(web_id)])
            .await
            .unwrap_err()
            .code,
        "ModuleNotOwnedByShip"
    );
    assert!(
        drain_all(&ctx, &mut sess_a, PILOT_A).iter().all(|(e, _)| e != "OnEwarEnd"),
        "unauthorized Deactivate must not emit OnEwarEnd"
    );
    assert!(
        drain_all(&ctx, &mut sess_b, PILOT_B).iter().all(|(e, _)| e != "OnEwarEnd"),
        "unauthorized Deactivate must not emit OnEwarEnd to victim"
    );
    assert!(
        ctx.projected_ewar.read().unwrap().get(&ship_b).is_some_and(|v| !v.is_empty()),
        "unauthorized Deactivate must not cleanse projected_ewar on victim"
    );

    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(web_id)])
        .await
        .expect("Deactivate Stasis Webifier I");
    let ewar_end_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ewar_end_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let expected_ewar_end = [
        PyRep::Integer(ship_a),
        PyRep::Integer(web_id),
        PyRep::Integer(ship_b),
        PyRep::String("webify".into()),
    ];
    assert_eq!(find_notif(&ewar_end_a, "OnEwarEnd"), &expected_ewar_end);
    assert_eq!(find_notif(&ewar_end_b, "OnEwarEnd"), &expected_ewar_end);

    // Activate ECM -> lands jam -> emits OnJamStart
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(ecm_id),
            PyRep::String("defaultEffect".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("Activate ECM Jammer");
    let mut jam_notifs_a = Vec::new();
    for _ in 0..25 {
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(ecm_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("Activate ECM");
        jam_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        if jam_notifs_a.iter().any(|(e, _)| e == "OnJamStart") {
            break;
        }
        let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(ecm_id)]).await;
    }
    let jam_args = find_notif(&jam_notifs_a, "OnJamStart");
    assert_eq!(jam_args.len(), 6);
    assert_eq!(jam_args[0].as_i64(), Some(ship_a));
    assert_eq!(jam_args[1].as_i64(), Some(ecm_id));
    assert_eq!(jam_args[2].as_i64(), Some(ship_b));
    assert_eq!(jam_args[3].as_str(), Some("electronic"));
    let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(ecm_id)]).await;

    // 5. Warp Scrambler & Guidance Disruptor OnEwarStart + OnEwarEnd jammingType parity
    for (mod_id, expected_jam_type) in [
        (scram_id, "warpScramblerMWD"),
        (gd_id, "ewGuidanceDisrupt"),
    ] {
        let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(mod_id),
                PyRep::String("defaultEffect".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await
        .expect("Activate EWAR module");
        let start_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
        let expected_args = [
            PyRep::Integer(ship_a),
            PyRep::Integer(mod_id),
            PyRep::Integer(ship_b),
            PyRep::String(expected_jam_type.into()),
        ];
        assert_eq!(find_notif(&start_notifs, "OnEwarStart"), &expected_args);

        let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", &[PyRep::Integer(mod_id)])
            .await
            .expect("Deactivate EWAR module");
        let end_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
        let end_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
        assert_eq!(find_notif(&end_notifs_a, "OnEwarEnd"), &expected_args);
        assert_eq!(find_notif(&end_notifs_b, "OnEwarEnd"), &expected_args);
    }

    // 6. TakeModuleOffline on an active EWAR module flushes projected_ewar and emits OnEwarEnd
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gd_id),
            PyRep::String("defaultEffect".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("Activate Guidance Disruptor before TakeModuleOffline");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(ship_a), PyRep::Integer(gd_id)],
    )
    .await
    .expect("TakeModuleOffline active Guidance Disruptor");
    let expected_gd_end = [
        PyRep::Integer(ship_a),
        PyRep::Integer(gd_id),
        PyRep::Integer(ship_b),
        PyRep::String("ewGuidanceDisrupt".into()),
    ];
    assert_eq!(find_notif(&drain_all(&ctx, &mut sess_a, PILOT_A), "OnEwarEnd"), &expected_gd_end);
    assert_eq!(find_notif(&drain_all(&ctx, &mut sess_b, PILOT_B), "OnEwarEnd"), &expected_gd_end);
}

}

// Folded submodule: notify_real_emitters_batch8_part6
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part6 {
    use super::*;

// Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
//
// Covers all 17 Group 4 parity rows across two test functions (`<= 15` tags each):
// - Test 1 (`9` rows): `OnCombatMessage`, `OnEffectMessage`, `OnJamStart`, `OnEwarEnd`,
//   `OnCloakModuleForceDeactivated`, `OnJumpCloakUpdated`, `OnJumpCloakCancelled`,
//   `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
// - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
//   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
//   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{TYPE_MOBILE_CYNO_BEACON, TYPE_MOBILE_DEPOT};
use eve_server::services::pve_expeditions::create_escalation_expedition;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
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
        ..Default::default()
    }
}

fn grant_all_skills(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (&tid, ty) in &ctx.sde.types {
            if ty.category_id == 16 {
                sk.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    grant_all_skills(ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.corporation_id = 1_000_125;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(1_000_125);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    cs.capacitor = cs.max_capacitor.max(10_000.0);
    cs.max_capacitor = cs.max_capacitor.max(10_000.0);
    cs.is_cloaked = false;
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    let slim = ctx.build_ship_slim_meta(char_id, ship_type);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
    ctx.invalidate_ship_fitting(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: notify:OnChargeLoadedToModule, notify:OnAmmoInBankChanged, notify:OnDroneActivityChange
// parity: notify:OnFleetBoosted, notify:OnFleetLootEvent, notify:OnFleetJumpBeaconDeployableChange
// parity: notify:OnFleetRespawnPointsUpdate
#[tokio::test]
async fn notify_batch8_ammo_drones_and_fleet_events() {
    eve_testkit::attest(&[
        "notify:OnChargeLoadedToModule",
        "notify:OnAmmoInBankChanged",
        "notify:OnDroneActivityChange",
        "notify:OnFleetBoosted",
        "notify:OnFleetLootEvent",
        "notify:OnFleetJumpBeaconDeployableChange",
        "notify:OnFleetRespawnPointsUpdate",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (840_000_101_i64, 840_000_102_i64);
    // Pilot A in Nighthawk (22466) so Command Burst II + AutoCannons + DroneBay all fit cleanly
    let mut sess_a = login_in_space(
        &ctx,
        9_411,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        22_466,
        SYS_JITA,
        DVec3::new(200_000.0, 0.0, 0.0),
    )
    .await;
    let mut sess_b = login_in_space(
        &ctx,
        9_412,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_JITA,
        DVec3::new(201_000.0, 0.0, 0.0),
    )
    .await;

    // 1. Weapon bank ammo load & unload -> OnChargeLoadedToModule & OnAmmoInBankChanged
    let (gun1_id, gun2_id, ammo_id, bad_ammo_id, burst_id, burst_charge_id, drone_item_id) = (
        840_000_201_i64,
        840_000_202_i64,
        840_000_203_i64,
        840_000_204_i64,
        840_000_205_i64,
        840_000_206_i64,
        840_000_207_i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun1_id,
            make_item(gun1_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            gun2_id,
            make_item(gun2_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true),
        );
        snap.items.insert(
            ammo_id,
            make_item(ammo_id, 183, PILOT_A, ship_a, InventoryFlag::Cargo, 500, false),
        );
        snap.items.insert(
            bad_ammo_id,
            make_item(bad_ammo_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 100, false),
        );
        snap.items.insert(
            burst_id,
            make_item(burst_id, 43_555, PILOT_A, ship_a, InventoryFlag::HiSlot2, 1, true),
        );
        snap.items.insert(
            burst_charge_id,
            make_item(burst_charge_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 300, false),
        );
        snap.items.insert(
            drone_item_id,
            make_item(drone_item_id, 2_454, PILOT_A, ship_a, InventoryFlag::DroneBay, 1, false),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);

    // Negative: loading incompatible ammo fails with IncompatibleAmmo
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(gun1_id)]),
                PyRep::List(vec![PyRep::Integer(bad_ammo_id)]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "IncompatibleAmmo"
    );

    // Ungrouped load emits OnChargeLoadedToModule without OnAmmoInBankChanged
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo ungrouped");
    let ungrouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&ungrouped_notifs, "OnChargeLoadedToModule").len(), 0);
    assert!(!ungrouped_notifs.iter().any(|(e, _)| e == "OnAmmoInBankChanged"));

    // Link gun1_id + gun2_id, then UnloadAmmo -> emits OnAmmoInBankChanged([gun1_id])
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LinkWeapons",
        &[
            PyRep::Integer(ship_a),
            PyRep::Integer(gun1_id),
            PyRep::Integer(gun2_id),
        ],
    )
    .await
    .expect("LinkWeapons");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "UnloadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
        ],
    )
    .await
    .expect("UnloadAmmo grouped");
    let unload_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&unload_notifs, "OnAmmoInBankChanged"),
        &[PyRep::Integer(gun1_id)]
    );

    // Reload grouped weapon bank -> emits BOTH OnChargeLoadedToModule() and OnAmmoInBankChanged([gun1_id])
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo grouped");
    let grouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&grouped_notifs, "OnChargeLoadedToModule").len(), 0);
    assert_eq!(
        find_notif(&grouped_notifs, "OnAmmoInBankChanged"),
        &[PyRep::Integer(gun1_id)]
    );

    // 2. Drone activity changes -> OnDroneActivityChange on CmdEngage, CmdReturnHome, CmdReturnBay
    let launched = rpc(
        &ctx,
        &mut sess_a,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_item_id)])],
    )
    .await
    .expect("LaunchDrones");
    let drone_ball = match launched {
        PyRep::List(items) => items[0].as_i64().expect("drone ball id"),
        _ => panic!("expected list from LaunchDrones"),
    };
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdEngage",
        &[
            PyRep::List(vec![PyRep::Integer(drone_ball)]),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("CmdEngage");
    let engage_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&engage_notifs, "OnDroneActivityChange"),
        &[
            PyRep::Integer(drone_ball),
            PyRep::Integer(1),
            PyRep::String("attack".into()),
        ]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdReturnHome",
        &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
    )
    .await
    .expect("CmdReturnHome");
    let home_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&home_notifs, "OnDroneActivityChange"),
        &[PyRep::Integer(drone_ball), PyRep::Integer(0), PyRep::None]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
    )
    .await
    .expect("CmdReturnBay");
    let bay_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        bay_notifs.iter().any(|(e, a)| {
            e == "OnDroneActivityChange"
                && a.first().and_then(|v| v.as_i64()) == Some(drone_ball)
                && matches!(a.get(1).and_then(|v| v.as_i64()), Some(4 | 0))
        }),
        "expected OnDroneActivityChange on CmdReturnBay, got {bay_notifs:?}"
    );

    // Negative: solo escalation without fleet emits no OnFleetEscalatingPathMessage
    let _ = create_escalation_expedition(
        &ctx,
        PILOT_A,
        1420,
        SYS_JITA,
        "Guristas Scout Outpost",
        [100_000.0, 0.0, 0.0],
    )
    .await;
    assert!(
        !drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnFleetEscalatingPathMessage")
    );

    // 3. Fleet creation + corp structure -> AcceptInvite emits OnFleetRespawnPointsUpdate
    let struct_id = 1_040_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_125,
                solar_system_id: SYS_JITA,
                name: "Jita Astrahus".into(),
                position: [200_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let fleet_rep = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = fleet_rep
        .as_i64()
        .or_else(|| fleet_rep.dict_get("fleetID").and_then(|v| v.as_i64()))
        .or(sess_a.fleet_id)
        .expect("fleetID");
    sess_a.fleet_id = Some(fleet_id);
    ctx.advance_secs(12);

    rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B");
    sess_b.fleet_id = Some(fleet_id);
    let join_notifs_bare = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !join_notifs_bare.iter().any(|(ev, _)| ev == "OnFleetRespawnPointsUpdate"),
        "bare Astrahus without clone bay must not be emitted as a respawn point"
    );

    // Fit and online Standup Cloning Center I (35894) on the Astrahus -> now included as respawn point
    let clone_bay_svc_id = 1_040_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            clone_bay_svc_id,
            eve_db::InventoryItemRecord {
                item_id: clone_bay_svc_id,
                type_id: 35_894,
                owner_id: 1_000_125,
                location_id: struct_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(clone_bay_svc_id, true);
    });
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("LeaveFleet PILOT_B");
    sess_b.fleet_id = None;
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B again");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B with clone bay");
    sess_b.fleet_id = Some(fleet_id);
    let join_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let respawn_args = find_notif(&join_notifs_a, "OnFleetRespawnPointsUpdate");
    assert_eq!(respawn_args.len(), 1);
    let PyRep::List(respawn_list) = &respawn_args[0] else {
        panic!("expected List of respawn KeyVals");
    };
    assert_eq!(respawn_list.len(), 1);
    assert_eq!(
        respawn_list[0].dict_get("locationID").and_then(|v| v.as_i64()),
        Some(struct_id)
    );

    // 4. Command Burst activation -> emits OnFleetBoosted([43555, num_boosted >= 1])
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(burst_id)]),
            PyRep::List(vec![PyRep::Integer(burst_charge_id)]),
        ],
    )
    .await
    .expect("LoadAmmo burst charge");
    ctx.advance_secs(35);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(burst_id)])
        .await
        .expect("Activate Shield Command Burst II");
    let burst_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let boost_args = find_notif(&burst_notifs, "OnFleetBoosted");
    assert_eq!(boost_args.len(), 2);
    assert_eq!(boost_args[0].as_i64(), Some(43_555));
    assert!(boost_args[1].as_i64().unwrap_or(0) >= 1);

    // 5. Loot from space container in fleet -> emits OnFleetLootEvent([{(PILOT_A, 34): 50}])
    let (jetcan_id, loot_item_id) = (840_000_301_i64, 840_000_302_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jetcan_id,
            make_item(jetcan_id, 23, PILOT_A, SYS_JITA as i64, InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            loot_item_id,
            make_item(loot_item_id, 34, PILOT_A, jetcan_id, InventoryFlag::None, 50, false),
        );
    });
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_handle
        .spawn_ball(Ball {
            id: jetcan_id,
            type_id: 23,
            position: DVec3::new(200_100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 25.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(loot_item_id),
            PyRep::Integer(ship_a),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
    )
    .await
    .expect("Loot jetcan item into ship cargo");
    let loot_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let loot_args = find_notif(&loot_notifs_b, "OnFleetLootEvent");
    assert_eq!(
        loot_args,
        &[PyRep::Dict(vec![(
            PyRep::Tuple(vec![PyRep::Integer(PILOT_A), PyRep::Integer(34)]),
            PyRep::Integer(50),
        )])]
    );

    // 6. Anchor & Unanchor Mobile Cynosural Beacon in fleet -> OnFleetJumpBeaconDeployableChange (true then false)
    let (beacon_id, depot_id) = (840_000_401_i64, 840_000_402_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_id,
            make_item(beacon_id, TYPE_MOBILE_CYNO_BEACON, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            depot_id,
            make_item(depot_id, TYPE_MOBILE_DEPOT, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
        );
    });

    // Negative: anchoring regular Mobile Depot does NOT emit OnFleetJumpBeaconDeployableChange
    let _ = ctx
        .systems
        .get_or_spawn(SYS_JITA)
        .set_ball_position(ship_a, DVec3::new(300_000.0, 0.0, 0.0))
        .await;
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(depot_id),
            PyRep::Tuple(vec![PyRep::Real(300_200.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .expect("Anchor Mobile Depot");
    assert!(
        !drain_all(&ctx, &mut sess_b, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnFleetJumpBeaconDeployableChange")
    );

    const LOWSEC_VARD: u32 = 30_002_537;
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&PILOT_A).unwrap().solar_system_id = LOWSEC_VARD; snap.items.get_mut(&ship_a).unwrap().location_id = LOWSEC_VARD as i64; snap.item_positions.insert(ship_a, [300_000.0, 0.0, 0.0]); });
    sess_a.solar_system_id = Some(LOWSEC_VARD);
    let _ = rpc(&ctx, &mut sess_a, "beyonce", "UpdateStateRequest", &[]).await;

    rpc(
        &ctx,
        &mut sess_a,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(beacon_id),
            PyRep::Tuple(vec![PyRep::Real(300_300.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .expect("Anchor Mobile Cyno Beacon");
    let cyno_on_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&cyno_on_notifs, "OnFleetJumpBeaconDeployableChange"),
        &[
            PyRep::Integer(beacon_id),
            PyRep::Integer(LOWSEC_VARD as i64),
            PyRep::Integer(beacon_id),
            PyRep::Integer(PILOT_A),
            PyRep::Bool(true),
        ]
    );

    eve_server::services::notify_emitters_batch8::on_deployable_deactivated(
        &ctx,
        sess_a.fleet_id,
        beacon_id,
    );
    let cyno_off_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&cyno_off_notifs, "OnFleetJumpBeaconDeployableChange"),
        &[
            PyRep::Integer(beacon_id),
            PyRep::Integer(LOWSEC_VARD as i64),
            PyRep::Integer(beacon_id),
            PyRep::Integer(PILOT_A),
            PyRep::Bool(false),
        ]
    );
}

}

pub mod part2 {
    use super::*;

#[tokio::test]
async fn test_skills_implants_boosters_respec_clones_and_insurance_notifications() {
    // parity: notify:OnServerSkillsChanged, notify:OnServerSkillsRemoved, notify:OnFreeSkillPointsChanged, notify:OnNewSkillQueueSaved, notify:OnSkillQueuePausedServer, notify:OnRespecInfoChanged, notify:OnServerImplantsChanged, notify:OnServerBoostersChanged, notify:OnCloneDestruction, notify:OnStationJumpCloneCacheInvalidated, notify:OnJumpCloneCacheInvalidated, notify:OnJumpCloneTransitionCompleted, notify:OnHomeStationChanged, notify:OnShipInsured
    eve_testkit::attest(&[
        "notify:OnServerSkillsChanged",
        "notify:OnServerSkillsRemoved",
        "notify:OnFreeSkillPointsChanged",
        "notify:OnNewSkillQueueSaved",
        "notify:OnSkillQueuePausedServer",
        "notify:OnRespecInfoChanged",
        "notify:OnServerImplantsChanged",
        "notify:OnServerBoostersChanged",
        "notify:OnCloneDestruction",
        "notify:OnStationJumpCloneCacheInvalidated",
        "notify:OnJumpCloneCacheInvalidated",
        "notify:OnJumpCloneTransitionCompleted",
        "notify:OnHomeStationChanged",
        "notify:OnShipInsured",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7601, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 7602, "other", "other-secret", P2).await;
    for c in [P1, P2] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. InjectSkillIntoBrain + SaveSkillQueue + AbortTraining + ApplyFreeSkillPoints + ExtractSkills
    let skillbook_id = 910_001i64;
    let skill_tid = 3300u32; // Gunnery
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&P1) {
            sk.remove(&skill_tid);
        }
    });
    seed_item(&ctx, skillbook_id, skill_tid, P1, STATION_JITA, InventoryFlag::Hangar, 1, false);
    call(&ctx, &mut s1, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(skillbook_id)]).await;
    let n1 = ctx.drain_notifications_for_char(P1);
    let sc = events_from(&n1, "OnServerSkillsChanged");
    assert_eq!(sc.len(), 1);
    let PyRep::Dict(skill_dict) = &sc[0][0] else {
        panic!("expected skillInfos dict, got {:?}", sc[0]);
    };
    assert_eq!(skill_dict.len(), 1);
    assert_eq!(skill_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(skill_dict[0].1.dict_get("trainedSkillLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(skill_dict[0].1.dict_get("trainedSkillPoints"), Some(&PyRep::Real(0.0)));
    assert!(events_from(&n1, "OnSkillsChanged").is_empty(), "client-local OnSkillsChanged must not be emitted");
    assert!(ctx.drain_notifications_for_char(P2).is_empty());

    // SaveSkillQueue -> OnNewSkillQueueSaved(newQueue)
    call(&ctx, &mut s1, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(skill_tid as i64), PyRep::Integer(1)])])]).await;
    let n_sq = ctx.drain_notifications_for_char(P1);
    let sq_saved = events_from(&n_sq, "OnNewSkillQueueSaved");
    assert_eq!(sq_saved.len(), 1);
    let PyRep::Rowset { rows: sq_rows, .. } = &sq_saved[0][0] else {
        panic!("expected Rowset in OnNewSkillQueueSaved");
    };
    assert_eq!(sq_rows.len(), 1);
    assert_eq!(sq_rows[0][0], PyRep::Integer(skill_tid as i64));
    assert_eq!(sq_rows[0][1], PyRep::Integer(1));
    assert!(events_from(&n_sq, "OnSkillQueueChanged").is_empty());

    // AbortTraining -> OnSkillQueuePausedServer()
    call(&ctx, &mut s1, "skillMgr2", "AbortTraining", &[]).await;
    let n_pause = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_pause, "OnSkillQueuePausedServer"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_pause, "OnSkillQueuePaused").is_empty());

    // ApplyFreeSkillPoints (1-arg applies to queue -> OnFreeSkillPointsChanged; 2-arg applies to skill -> OnServerSkillsChanged)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.free_skill_points = 600_250.0;
        }
    });
    call(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(250)]).await;
    let n_fsp = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_fsp, "OnFreeSkillPointsChanged"), vec![vec![PyRep::Real(600_000.0)]]);
    call(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(skill_tid as i64), PyRep::Integer(550_000)]).await;
    let n_apply = ctx.drain_notifications_for_char(P1);
    let sc_apply = events_from(&n_apply, "OnServerSkillsChanged");
    assert_eq!(sc_apply.len(), 1);
    let PyRep::Dict(apply_dict) = &sc_apply[0][0] else {
        panic!("expected skillInfos dict");
    };
    assert_eq!(apply_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(apply_dict[0].1.dict_get("trainedSkillPoints"), Some(&PyRep::Real(256_000.0)));

    // ExtractSkills -> OnServerSkillsRemoved(skillInfos, timeStamp)
    let extractor_id = 910_002i64;
    seed_item(&ctx, extractor_id, 40519, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    let skill_tid2 = 3301u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_600_000.0;
        }
        let sk = snap.skills.entry(P1).or_default();
        sk.insert(skill_tid2, eve_db::TrainedSkillRecord { type_id: skill_tid2, level: 5, skill_points: 256_000.0 });
    });
    let extract_map = vec![(PyRep::Integer(skill_tid as i64), PyRep::Integer(250_000)), (PyRep::Integer(skill_tid2 as i64), PyRep::Integer(250_000))];

    // EVOMU-P0-01: verify missing/invalid extractor, wrong owner, wrong type, and insufficient SP are rejected
    let err_missing =
        ctx.dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::None], &HashMap::new(), None).await;
    assert_eq!(err_missing.unwrap_err().code, "MissingSkillExtractor");

    let stolen_extractor = 910_003i64;
    seed_item(&ctx, stolen_extractor, 40519, P2, STATION_JITA, InventoryFlag::Hangar, 1, false);
    let err_owner = ctx
        .dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(stolen_extractor)], &HashMap::new(), None)
        .await;
    assert_eq!(err_owner.unwrap_err().code, "MissingSkillExtractor");

    let wrong_type = 910_004i64;
    seed_item(&ctx, wrong_type, 3300, P1, STATION_JITA, InventoryFlag::Hangar, 1, false);
    let err_type = ctx
        .dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(wrong_type)], &HashMap::new(), None)
        .await;
    assert_eq!(err_type.unwrap_err().code, "MissingSkillExtractor");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_200_000.0;
        }
    });
    let err_sp = ctx
        .dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(extractor_id)], &HashMap::new(), None)
        .await;
    assert_eq!(err_sp.unwrap_err().code, "InsufficientSkillPoints");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_600_000.0;
        }
    });

    call(&ctx, &mut s1, "skillMgr2", "ExtractSkills", &[PyRep::Dict(extract_map), PyRep::Integer(extractor_id)]).await;
    let n_ext = ctx.drain_notifications_for_char(P1);
    let rem_ev = events_from(&n_ext, "OnServerSkillsRemoved");
    assert_eq!(rem_ev.len(), 1);
    let PyRep::Dict(rem_dict) = &rem_ev[0][0] else {
        panic!("expected skillInfos dict in OnServerSkillsRemoved");
    };
    assert_eq!(rem_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(rem_dict[0].1.dict_get("typeID"), Some(&PyRep::Integer(skill_tid as i64)));
    assert!(ctx.drain_notifications_for_char(P2).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&extractor_id), "extractor must be consumed from hangar");
        let has_injector = snap.items.values().any(|it| it.owner_id == P1 && it.type_id == 40520);
        assert!(has_injector, "Large Skill Injector (type 40520) must be minted");
    });

    // 2. RespecCharacter -> OnRespecInfoChanged()
    call(&ctx, &mut s1, "charMgr", "RespecCharacter", &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(19)])
        .await;
    let n_respec = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_respec, "OnRespecInfoChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_respec, "OnRespecInfoUpdated").is_empty());

    // 3. InjectImplant -> OnServerImplantsChanged() & UseBooster -> OnServerBoostersChanged()
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(P1).or_default();
        sk.insert(
            3411, // Cybernetics V
            TrainedSkillRecord { type_id: 3411, level: 5, skill_points: 256_000.0 },
        );
        sk.insert(
            3405, // Biology V
            TrainedSkillRecord { type_id: 3405, level: 5, skill_points: 256_000.0 },
        );
    });
    let implant_id = 910_003i64;
    seed_item(&ctx, implant_id, 9899, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "dogmaIM", "InjectImplant", &[PyRep::Integer(implant_id)]).await;
    let n_imp = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_imp, "OnServerImplantsChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_imp, "OnImplantsChanged").is_empty());

    let booster_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 303
                && t.default_attributes.get(&330).copied().unwrap_or(0.0) > 0.0
                && t.default_attributes.get(&1087).copied().unwrap_or(0.0).round() as i64 >= 1
                && !t.default_attributes.contains_key(&182)
                && !t.default_attributes.contains_key(&183)
                && !t.default_attributes.contains_key(&184)
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("SDE combat booster type");
    let booster_id = 910_004i64;
    seed_item(&ctx, booster_id, booster_tid, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_id)]).await;
    let n_bst = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_bst, "OnServerBoostersChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_bst, "OnBoosterUpdated").is_empty());

    // 4. InstallCloneInStation + CloneJump + DestroyInstalledClone
    let pod_id = 910_099i64;
    seed_item(&ctx, pod_id, 670, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
            ch.active_ship_id = pod_id;
        }
        snap.skills.entry(P1).or_default().insert(
            24242, // Infomorph Psychology
            TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
        );
    });
    s1.ship_id = Some(pod_id);
    let jc_rep = call(&ctx, &mut s1, "jumpCloneSvc", "InstallCloneInStation", &[]).await;
    let jc_id = jc_rep.dict_get("jumpCloneID").and_then(|v| v.as_i64()).expect("jumpCloneID");
    let n_jc_inst = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_jc_inst, "OnStationJumpCloneCacheInvalidated"), vec![vec![PyRep::Integer(STATION_JITA), PyRep::Integer(P1)]]);
    assert_eq!(events_from(&n_jc_inst, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_jc_inst, "OnCloneJumpUpdate").is_empty());

    call(&ctx, &mut s1, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_id)]).await;
    let n_cj = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_cj, "OnJumpCloneTransitionCompleted"), vec![Vec::<PyRep>::new()]);
    assert_eq!(events_from(&n_cj, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);

    let remaining_jc = ctx.db.with_snapshot(|snap| snap.jump_clones.get(&P1).and_then(|v| v.first().map(|c| c.jump_clone_id)).unwrap_or(jc_id));
    call(&ctx, &mut s1, "jumpCloneSvc", "DestroyInstalledClone", &[PyRep::Integer(remaining_jc)]).await;
    let n_jc_del = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_jc_del, "OnStationJumpCloneCacheInvalidated"), vec![vec![PyRep::Integer(STATION_JITA), PyRep::Integer(P1)]]);
    assert_eq!(events_from(&n_jc_del, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);

    // 5. Home station change & self_destruct_clone -> OnHomeStationChanged & OnCloneDestruction
    call(&ctx, &mut s1, "home_station", "set_home_station", &[PyRep::Integer(STATION_JITA)]).await;
    let n_hs = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_hs, "OnHomeStationChanged"), vec![vec![PyRep::Integer(STATION_JITA)]]);
    call(&ctx, &mut s1, "home_station", "self_destruct_clone", &[]).await;
    let n_sd = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_sd, "OnCloneDestruction"), vec![Vec::<PyRep>::new()]);

    // 6. InsureShip -> OnShipInsured()
    call(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(100_000_001), PyRep::Real(1.0), PyRep::Bool(false)]).await;
    let n_ins = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_ins, "OnShipInsured"), vec![Vec::<PyRep>::new()]);
    assert!(ctx.drain_notifications_for_char(P2).is_empty());
}

#[tokio::test]
async fn test_wallet_bills_lp_trade_market_and_industry_notifications() {
    // parity: notify:OnAccountChange, notify:OnBillReceived, notify:OnOfficeRentalChange, notify:OnLPChange, notify:OnCorpLPChange, notify:OnTradeInitiate, notify:OnTradeOfferReset, notify:OnTradeMoneyOffer, notify:OnTradeOffer, notify:OnTradeComplete, notify:OnTradeCancel, notify:OnOwnOrdersChanged, notify:OnMarketItemReceived, notify:OnIndustryJob
    eve_testkit::attest(&[
        "notify:OnAccountChange",
        "notify:OnBillReceived",
        "notify:OnOfficeRentalChange",
        "notify:OnLPChange",
        "notify:OnCorpLPChange",
        "notify:OnTradeInitiate",
        "notify:OnTradeOfferReset",
        "notify:OnTradeMoneyOffer",
        "notify:OnTradeOffer",
        "notify:OnTradeComplete",
        "notify:OnTradeCancel",
        "notify:OnOwnOrdersChanged",
        "notify:OnMarketItemReceived",
        "notify:OnIndustryJob",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7621, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7622, "other", "other-secret", P2).await;

    let npc_corp = 1_000_035i64;
    let target_npc_corp = 1_000_125i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(500_000_000.0);
            c1.loyalty_points.insert(npc_corp, 5_000);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.balance = IskCents::from_f64_isk(100_000_000.0);
        }
    });

    let corp_rep = call(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Wallet Industry Corp".into()), PyRep::String("WINC".into())]).await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.master_wallet = IskCents::from_f64_isk(500_000_000.0);
            for div in &mut corp.wallet_divisions {
                div.balance = IskCents::from_f64_isk(500_000_000.0);
            }
        }
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(100_000_000.0);
        }
    });
    for c in [P1, P2] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. GiveCash -> OnAccountChange("cash", ownerID, newBalance)
    call(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(1_000.0), PyRep::String("Tip".into())]).await;
    let n_cash1 = ctx.drain_notifications_for_char(P1);
    let n_cash2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_cash1, "OnAccountChange"), vec![vec![PyRep::String("cash".into()), PyRep::Integer(P1), PyRep::Real(99_999_000.0),]]);
    assert_eq!(events_from(&n_cash2, "OnAccountChange"), vec![vec![PyRep::String("cash".into()), PyRep::Integer(P2), PyRep::Real(100_001_000.0),]]);
    assert!(events_from(&n_cash1, "OnPersonalAccountChangedClient").is_empty());

    // 2. RentOffice -> OnBillReceived(bill) & OnOfficeRentalChange(corporationID, officeID)
    let rent_rep = call(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(STATION_JITA)]).await;
    let office_id = rent_rep.dict_get("officeFolderID").and_then(|v| v.as_i64()).expect("officeFolderID");
    let n_rent = ctx.drain_notifications_for_char(P1);
    let bills = events_from(&n_rent, "OnBillReceived");
    assert_eq!(bills.len(), 1);
    assert_eq!(bills[0][0].dict_get("debtorID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(events_from(&n_rent, "OnOfficeRentalChange"), vec![vec![PyRep::Integer(corp_id), PyRep::Integer(office_id)]]);
    assert!(events_from(&n_rent, "OnOfficeRentalChanged").is_empty());
    assert!(ctx.drain_notifications_for_char(P2).is_empty());

    // 3. LP transfers: TransferLPFromMyWalletToOtherCorp -> OnLPChange(corpID, newLP) & TransferLPFromMyCorpWalletToOtherCorp -> OnCorpLPChange(corpID, newLP)
    call(&ctx, &mut s1, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(corp_id), PyRep::Integer(npc_corp), PyRep::Integer(2_000)]).await;
    let n_lp = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_lp, "OnLPChange"),
        vec![vec![PyRep::String("transfer".into()), PyRep::Integer(npc_corp), PyRep::Integer(5_000), PyRep::Integer(3_000),]]
    );

    call(&ctx, &mut s1, "LPSvc", "TransferLPFromMyCorpWalletToOtherCorp", &[PyRep::Integer(target_npc_corp), PyRep::Integer(npc_corp), PyRep::Integer(500)])
        .await;
    let n_clp = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_clp, "OnCorpLPChange"), vec![vec![PyRep::String("transfer".into()), PyRep::Integer(npc_corp),]]);

    // 4. Direct Player Trade: InitiateTrade, AddItem, OfferMoney, MakeOffer, Abort
    let trade_item_id = 930_001i64;
    seed_item(&ctx, trade_item_id, 34, P1, STATION_JITA, InventoryFlag::Hangar, 50, false);
    let init_rep = call(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await;
    let trade_id = init_rep.dict_get("tradeContainerID").and_then(|v| v.as_i64()).expect("tradeContainerID");
    let n_ti1 = ctx.drain_notifications_for_char(P1);
    let n_ti2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_ti1, "OnTradeInitiate"), vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(trade_id)]]);
    assert_eq!(events_from(&n_ti2, "OnTradeInitiate"), vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(trade_id)]]);

    call(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id)]).await;
    let n_to2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_to2, "OnTradeOffer"), vec![vec![PyRep::Integer(trade_id), PyRep::Bool(false), PyRep::Bool(false),]]);
    assert_eq!(events_from(&n_to2, "OnTradeOfferReset"), vec![vec![PyRep::Integer(trade_id)]]);
    ctx.drain_notifications_for_char(P1);

    call(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(5_000.0)]).await;
    let n_tm1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_tm1, "OnTradeMoneyOffer"), vec![vec![PyRep::Integer(trade_id), PyRep::Real(0.0), PyRep::Real(5_000.0),]]);
    ctx.drain_notifications_for_char(P2);

    call(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await;
    let n_tc1 = ctx.drain_notifications_for_char(P1);
    let n_tc2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_tc1, "OnTradeComplete"), vec![vec![PyRep::Integer(trade_id)]]);
    assert_eq!(events_from(&n_tc2, "OnTradeComplete"), vec![vec![PyRep::Integer(trade_id)]]);

    let init2 = call(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await;
    let trade_id2 = init2.dict_get("tradeContainerID").and_then(|v| v.as_i64()).expect("tradeContainerID");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s1, "trademgr", "Abort", &[PyRep::Integer(trade_id2)]).await;
    assert_eq!(events_from(&ctx.drain_notifications_for_char(P2), "OnTradeCancel"), vec![vec![PyRep::Integer(trade_id2)]]);
    ctx.drain_notifications_for_char(P1);

    // 5. Market: PlaceMultiSellOrder + PlaceBuyOrder immediate fill -> OnOwnOrdersChanged & OnMarketItemReceived
    let sell_item_id = 930_010i64;
    seed_item(&ctx, sell_item_id, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    call(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(STATION_JITA)),
                ("typeID", PyRep::Integer(34)),
                ("price", PyRep::Real(10.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let n_sell = ctx.drain_notifications_for_char(P1);
    assert!(!events_from(&n_sell, "OnOwnOrdersChanged").is_empty());

    call(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[PyRep::Integer(STATION_JITA), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(100), PyRep::Integer(0), PyRep::Integer(1)],
    )
    .await;
    let n_buy = ctx.drain_notifications_for_char(P2);
    assert!(!events_from(&n_buy, "OnOwnOrdersChanged").is_empty());
    let mkt_rec = events_from(&n_buy, "OnMarketItemReceived");
    assert_eq!(mkt_rec.len(), 1);
    assert_eq!(mkt_rec[0][1], PyRep::Integer(34));
    assert_eq!(mkt_rec[0][2], PyRep::Integer(InventoryFlag::Hangar as i64));
    assert_eq!(mkt_rec[0][3], PyRep::Integer(STATION_JITA));
    assert!(events_from(&n_buy, "OnMarketEscrowChanged").is_empty());
    assert!(events_from(&n_buy, "OnClientEvent_MarketItemReceived").is_empty());
    ctx.drain_notifications_for_char(P1);

    // 6. Industry: InstallJob -> OnIndustryJob(jobID, ownerID, blueprintID, installerID, status=1, runs)
    let job_rep = call(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)]).await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let n_ind = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_ind, "OnIndustryJob"),
        vec![vec![PyRep::Integer(job_id), PyRep::Integer(P1), PyRep::Integer(100_000_007), PyRep::Integer(P1), PyRep::Integer(1), PyRep::Integer(1),]]
    );
    assert!(events_from(&n_ind, "OnIndustryJobClient").is_empty());
}

#[tokio::test]
async fn test_blueprints_and_contracts_notifications() {
    // parity: notify:OnBlueprintsUpdated, notify:OnContractCreated, notify:OnContractAssigned, notify:OnContractAccepted, notify:OnContractCompleted, notify:OnContractRejected, notify:OnContractFailed, notify:OnContractOutbid, notify:OnContractBuyout
    eve_testkit::attest(&[
        "notify:OnBlueprintsUpdated",
        "notify:OnContractCreated",
        "notify:OnContractAssigned",
        "notify:OnContractAccepted",
        "notify:OnContractCompleted",
        "notify:OnContractRejected",
        "notify:OnContractFailed",
        "notify:OnContractOutbid",
        "notify:OnContractBuyout",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7631, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7632, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("bidder6", "bidder6-secret");
    let p3 = ctx.db.create_character(uid3, "Bidder Six", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select(&ctx, 7633, "bidder6", "bidder6-secret", p3).await;

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [P1, P2, p3] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_f64_isk(200_000_000.0);
                ch.station_id = Some(STATION_JITA);
            }
            snap.skills.entry(cid).or_default().insert(
                25235, // Contracting
                TrainedSkillRecord { type_id: 25235, level: 5, skill_points: 256_000.0 },
            );
        }
    });
    s1.station_id = Some(STATION_JITA);
    s2.station_id = Some(STATION_JITA);
    s3.station_id = Some(STATION_JITA);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. OnBlueprintsUpdated(ownerID) on InstallJob / CancelJob
    let bp_id = 940_001i64;
    seed_item(&ctx, bp_id, 683, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    let job_rep =
        call(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::Integer(bp_id), PyRep::Integer(683), PyRep::Integer(3), PyRep::Integer(1)]).await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let n_bp = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_bp, "OnBlueprintsUpdated"), vec![vec![PyRep::Integer(P1)]]);
    call(&ctx, &mut s1, "industryManager", "CancelJob", &[PyRep::Integer(job_id)]).await;
    assert_eq!(events_from(&ctx.drain_notifications_for_char(P1), "OnBlueprintsUpdated"), vec![vec![PyRep::Integer(P1)]]);

    // 2. Contracts: CreateContract (assigned to P2) -> OnContractCreated & OnContractAssigned, AcceptContract -> OnContractAccepted & OnContractCompleted
    let item1 = 940_010i64;
    seed_item(&ctx, item1, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c1_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(1), // Personal
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item1)]),
        ],
    )
    .await;
    let cid1 = c1_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid1");
    let n_c1_p1 = ctx.drain_notifications_for_char(P1);
    let n_c1_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(events_from(&n_c1_p1, "OnContractCreated"), vec![vec![PyRep::Integer(cid1)]]);
    assert!(events_from(&n_c1_p1, "OnContractEscrowChanged").is_empty());
    assert_eq!(events_from(&n_c1_p2, "OnContractAssigned"), vec![vec![PyRep::Integer(cid1)]]);
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    call(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid1)]).await;
    let n_acc_p1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_acc_p1, "OnContractAccepted"), vec![vec![PyRep::Integer(cid1)]]);
    assert_eq!(events_from(&n_acc_p1, "OnContractCompleted"), vec![vec![PyRep::Integer(cid1)]]);
    ctx.drain_notifications_for_char(P2);

    // 3. RejectContract -> OnContractRejected
    let item2 = 940_011i64;
    seed_item(&ctx, item2, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c2_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item2)]),
        ],
    )
    .await;
    let cid2 = c2_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid2");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "contractProxy", "RejectContract", &[PyRep::Integer(cid2)]).await;
    assert_eq!(events_from(&ctx.drain_notifications_for_char(P1), "OnContractRejected"), vec![vec![PyRep::Integer(cid2)]]);

    // 4. Courier contract + FailContract -> OnContractFailed
    let item3 = 940_012i64;
    seed_item(&ctx, item3, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c3_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), // Courier
            PyRep::Integer(1),
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_AMARR),
            PyRep::Real(0.0),
            PyRep::Real(5_000.0),
            PyRep::Real(10_000.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item3)]),
        ],
    )
    .await;
    let cid3 = c3_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid3");
    call(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid3)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "contractProxy", "FailContract", &[PyRep::Integer(cid3)]).await;
    assert_eq!(events_from(&ctx.drain_notifications_for_char(P1), "OnContractFailed"), vec![vec![PyRep::Integer(cid3)]]);

    // 5. Auction contract: P2 bids, P3 outbids & buys out -> OnContractOutbid (to P2), OnContractBuyout & OnContractCompleted (to P1)
    let item4 = 940_013i64;
    seed_item(&ctx, item4, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c4_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2), // Auction
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(500_000.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item4)]),
        ],
    )
    .await;
    let cid4 = c4_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid4");
    ctx.drain_notifications_for_char(P1);

    call(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(cid4), PyRep::Real(150_000.0)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

    call(&ctx, &mut s3, "contractProxy", "PlaceBid", &[PyRep::Integer(cid4), PyRep::Real(500_000.0)]).await;
    let n_outbid_p2 = ctx.drain_notifications_for_char(P2);
    let n_buyout_p1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_outbid_p2, "OnContractOutbid"), vec![vec![PyRep::Integer(cid4)]]);
    assert_eq!(events_from(&n_buyout_p1, "OnContractBuyout"), vec![vec![PyRep::Integer(cid4)]]);
    assert_eq!(events_from(&n_buyout_p1, "OnContractCompleted"), vec![vec![PyRep::Integer(cid4)]]);
}

}
