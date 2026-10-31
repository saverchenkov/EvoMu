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
