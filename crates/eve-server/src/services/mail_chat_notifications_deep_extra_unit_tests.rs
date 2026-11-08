//! Deep branch-coverage and mutation-killing unit tests for EVE Mail & Trash/Labels,
//! Mailing Lists (ACL, cost, welcome mail, operator/mute roles), Notifications (`notificationMgr`),
//! Calendar events & RSVPs, Contacts & Labels/Blocking, XMPP/LSC Chat & Delayed Local,
//! and Structure Access Lists (`ownerGroupManager`) & Auto-Defense TTL boundaries.

#![forbid(unsafe_code)]

use crate::router::{NotificationRecord, ServerContext};
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::character_skills::character_contacts::handle_contacts;
use crate::services::config_service::handle as handle_config;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use crate::services::fleet_social::handle as handle_social;
use crate::services::station_structure::structure_upwell_ops::{
    handle as handle_upwell_ops, load_ext_state, save_ext_state, AccessGroupRecord,
};
use crate::services::structure_acl_defense_rules::{
    evaluate_auto_defense_target, handle as handle_acl_rules, save_defense_settings,
    verify_structure_docking_acl, StructureDefenseSettings, ACL_ROLE_BLOCKED, ACL_ROLE_MANAGER,
    STRUCTURE_ATTACKER_TTL_SECS,
};
use eve_db::{corp_roles, AllianceRecord, CorporationRecord, IskCents, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn social_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_social(ctx, sess, svc, method, args, &HashMap::new())
        .await
        .expect("social handled")
}

fn cfg_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_config(ctx, sess, svc, method, args, &HashMap::new()).expect("config handled")
}

fn rep_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::Rowset { rows, .. } => rows.len(),
        PyRep::ObjectEx { list, .. } => list.len(),
        PyRep::List(xs) | PyRep::Tuple(xs) | PyRep::PySet(xs) => xs.len(),
        PyRep::Object { args, .. } => args
            .dict_get("lines")
            .and_then(|v| v.as_list())
            .map(|l| l.len())
            .unwrap_or(0),
        _ => 0,
    }
}

fn contact_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_contacts(ctx, sess, "charMgr", method, args, &HashMap::new()).expect("contacts handled")
}

fn make_sess(char_id: i64, corp_id: i64, ally_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(9200 + (char_id as u64 % 500));
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.alliance_id = ally_id;
    s.solar_system_id = Some(30_000_142);
    s.corp_role = role;
    s
}

fn seed_three_pilots(ctx: &ServerContext) -> (i64, i64, i64, i64, i64) {
    let (c1, c2, c3, corp1, ally1) =
        (90_000_001_i64, 90_000_002_i64, 90_000_003_i64, 98_700_001_i64, 99_700_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters.values().next().expect("seed char").clone();
        for &(cid, bal_isk) in &[(c1, 500_000_i64), (c2, 24_999_i64), (c3, 100_000_i64)] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                let mut t = template.clone();
                t.character_id = cid;
                t.user_id = cid as u64;
                t.name = format!("Pilot-{cid}");
                t
            });
            ch.corporation_id = if cid == c3 { 1_000_009 } else { corp1 };
            ch.alliance_id = if cid == c3 { None } else { Some(ally1) };
            ch.balance = IskCents::from_isk(bal_isk);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp1,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.alliance_id = Some(ally1);
                __corp.name = "Comms Test Corp".into();
                __corp.ticker = "CMTC".into();
                __corp.ceo_id = c1;
                __corp.members = [c1, c2].into_iter().collect();
                __corp.member_roles = [(c1, corp_roles::ROLE_DIRECTOR), (c2, 0)].into_iter().collect();
                __corp },
        );
        snap.alliances.insert(
            ally1,
            AllianceRecord {
                alliance_id: ally1,
                name: "Comms Test Alliance".into(),
                short_name: "CMTA".into(),
                executor_corp_id: corp1,
                member_corp_ids: [corp1].into_iter().collect(),
            },
        );
    });
    (c1, c2, c3, corp1, ally1)
}

#[tokio::test]
async fn mail_trash_restore_filters_and_mailing_lists_acl_welcome_roles_kill_mutations() {
    let ctx = test_ctx_with_clock(1_710_600_000);
    let (c1, c2, c3, corp1, ally1) = seed_three_pilots(&ctx);
    let mut s1 = make_sess(c1, corp1, Some(ally1), corp_roles::ROLE_DIRECTOR);
    let mut s2 = make_sess(c2, corp1, Some(ally1), 0);
    let mut s3 = make_sess(c3, 1_000_009, None, 0);

    // 1. EVE Mail: send, unauthorized MoveToTrash, MoveToTrash, MoveFromTrash, MarkAllAsRead, EmptyTrash
    let m1 = social_call(&ctx, &mut s1, "mailMgr", "SendMail", &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("Op Brief".into()), PyRep::String("Undock".into())])
        .await.unwrap().as_i64().unwrap();
    assert_eq!(social_call(&ctx, &mut s3, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m1)])]).await.unwrap_err().code, "AccessDenied");
    let trashed = social_call(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m1)])]).await.unwrap();
    assert_eq!(trashed.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    let restored = social_call(&ctx, &mut s2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(m1)])]).await.unwrap();
    assert_eq!(restored.dict_get("restored"), Some(&PyRep::Bool(true)));
    social_call(&ctx, &mut s2, "mailMgr", "MarkAllAsRead", &[]).await.unwrap();
    social_call(&ctx, &mut s2, "mailMgr", "MoveToTrashByLabel", &[PyRep::Integer(1)]).await.unwrap();
    let purged = social_call(&ctx, &mut s2, "mailMgr", "EmptyTrash", &[]).await.unwrap();
    assert_eq!(purged.dict_get("deleted"), Some(&PyRep::Bool(true)));

    // 2. Mailing lists: Create with 25,000 ISK cost, SaveWelcomeMail, cost threshold, roles & ACL
    let list_id = social_call(&ctx, &mut s1, "mailingListsMgr", "Create", &[PyRep::String("IntelChannel".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(25_000)])
        .await.unwrap().as_i64().unwrap();
    assert_eq!(
        social_call(&ctx, &mut s2, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Hi".into()), PyRep::String("Rules".into())])
            .await.unwrap_err().code,
        "UserError"
    );
    social_call(&ctx, &mut s1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome!".into()), PyRep::String("Read pinned rules.".into())]).await.unwrap();

    // c2 has 24,999 ISK (1 ISK short of 25,000 cost) -> NotEnoughISK; at exact 25,000 ISK -> succeeds + welcome mail!
    assert_eq!(social_call(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)]).await.unwrap_err().code, "NotEnoughISK");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c2).unwrap().balance = IskCents::from_isk(25_000);
    });
    let joined = social_call(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::String("intelchannel".into())]).await.unwrap();
    assert_eq!(joined.dict_get("joined"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&c2].balance), IskCents::from_isk(0));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&c1].balance), IskCents::from_isk(525_000));

    // Entity ACL block on c3 prevents c3 from joining
    social_call(&ctx, &mut s1, "mailingListsMgr", "SetEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(c3), PyRep::Integer(0)]).await.unwrap();
    assert_eq!(social_call(&ctx, &mut s3, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)]).await.unwrap_err().code, "UserError");

    // Promote c2 to operator, allow & join c3, broadcast, kick c3, deny operator Delete, allow creator Delete
    social_call(&ctx, &mut s1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(c2)])]).await.unwrap();
    social_call(&ctx, &mut s2, "mailingListsMgr", "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(c3)]).await.unwrap();
    social_call(&ctx, &mut s3, "mailingListsMgr", "Join", &[PyRep::Integer(list_id)]).await.unwrap();
    social_call(&ctx, &mut s2, "mailingListsMgr", "SendWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Broadcast".into()), PyRep::String("Check intel".into())]).await.unwrap();
    social_call(&ctx, &mut s2, "mailingListsMgr", "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(c3)])]).await.unwrap();
    assert_eq!(social_call(&ctx, &mut s2, "mailingListsMgr", "Delete", &[PyRep::Integer(list_id)]).await.unwrap_err().code, "UserError");
    social_call(&ctx, &mut s1, "mailingListsMgr", "Delete", &[PyRep::Integer(list_id)]).await.unwrap();
}

#[tokio::test]
async fn notification_mgr_calendar_crud_and_contact_labels_block_kill_mutations() {
    let ctx = test_ctx_with_clock(1_710_600_000);
    let (c1, c2, c3, corp1, ally1) = seed_three_pilots(&ctx);
    let mut s1 = make_sess(c1, corp1, Some(ally1), corp_roles::ROLE_DIRECTOR);
    let mut s2 = make_sess(c2, corp1, Some(ally1), 0);
    let mut s3 = make_sess(c3, 1_000_009, None, 0);

    // 1. notificationMgr: seed multi-group notifications & test filter/process/delete branches
    {
        let mut recs = ctx.notification_records.write().unwrap();
        for &(nid, gid, rx, proc_flag, del_flag) in &[
            (101, 1, c1, false, false),
            (102, 4, c1, false, false),
            (103, 4, c1, true, false),
            (104, 1, c1, false, true),
            (105, 1, c2, false, false),
        ] {
            recs.push(NotificationRecord { notification_id: nid, type_id: 16, sender_id: c2, receiver_id: rx, processed: proc_flag, created: 1_710_600_000, data: "{}".into(), deleted: del_flag, group_id: gid, interacted: false });
        }
    }
    assert_eq!(rep_len(&cfg_call(&ctx, &mut s1, "notificationMgr", "GetAllNotifications", &[]).unwrap()), 3);
    assert_eq!(rep_len(&cfg_call(&ctx, &mut s1, "notificationMgr", "GetUnprocessed", &[]).unwrap()), 2);
    assert_eq!(rep_len(&cfg_call(&ctx, &mut s1, "notificationMgr", "GetByGroupID", &[PyRep::Integer(4)]).unwrap()), 2);
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "GetByGroupID", &[]).unwrap_err().code, "InvalidArgument");
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "MarkGroupAsProcessed", &[PyRep::Integer(4)]).unwrap(), PyRep::Integer(1));
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "LogNotificationInteraction", &[PyRep::Integer(101)]).unwrap(), PyRep::Bool(true));
    assert!(ctx.notification_records.read().unwrap().iter().find(|n| n.notification_id == 101).unwrap().interacted);
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "MarkAllAsProcessed", &[]).unwrap(), PyRep::Integer(1));
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "DeleteGroupNotifications", &[PyRep::Integer(4)]).unwrap(), PyRep::Integer(2));
    assert_eq!(cfg_call(&ctx, &mut s1, "notificationMgr", "DeleteAllNotifications", &[]).unwrap(), PyRep::Integer(1));

    // 2. Calendar CRUD & RSVP validation bounds
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    let future_ft = now_ft + 3_600 * 10_000_000;
    assert_eq!(social_call(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[PyRep::Integer(future_ft), PyRep::Integer(10_001), PyRep::String("TooLong".into())]).await.unwrap_err().code, "UserError");
    let ev_rep = social_call(&ctx, &mut s1, "calendarMgr", "CreateCorporationEvent", &[PyRep::Integer(future_ft), PyRep::Integer(90), PyRep::String("Corp Meeting".into()), PyRep::String("Sync".into()), PyRep::Integer(1)]).await.unwrap();
    let eid = ev_rep.as_i64().or_else(|| ev_rep.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

    assert_eq!(social_call(&ctx, &mut s2, "calendarMgr", "SendEventResponse", &[PyRep::Integer(eid), PyRep::Integer(corp1), PyRep::Integer(3)]).await.unwrap_err().code, "UserError");
    social_call(&ctx, &mut s2, "calendarMgr", "SendEventResponse", &[PyRep::Integer(eid), PyRep::Integer(corp1), PyRep::Integer(4)]).await.unwrap();
    social_call(
        &ctx, &mut s1, "calendarMgr", "EditCorporationEvent",
        &[PyRep::Integer(eid), PyRep::Integer(future_ft + 600 * 10_000_000), PyRep::Integer(60), PyRep::String("Updated Corp Meeting".into()), PyRep::String("Agenda".into()), PyRep::Integer(2)],
    ).await.unwrap();
    let details = social_call(&ctx, &mut s2, "calendarMgr", "GetEventDetails", &[PyRep::Integer(eid)]).await.unwrap();
    assert_eq!(details.dict_get("title"), Some(&PyRep::String("Updated Corp Meeting".into())));
    assert_eq!(details.dict_get("response"), Some(&PyRep::Integer(4)));
    assert_eq!(details.dict_get("flag"), Some(&PyRep::Integer(2)));
    assert!(social_call(&ctx, &mut s3, "calendarMgr", "GetEventDetails", &[PyRep::Integer(eid)]).await.is_err());

    social_call(&ctx, &mut s1, "calendarMgr", "DeleteEvent", &[PyRep::Integer(eid)]).await.unwrap();
    assert_eq!(social_call(&ctx, &mut s2, "calendarMgr", "GetEventDetails", &[PyRep::Integer(eid)]).await.unwrap_err().code, "CalendarEventNotFound");

    // 3. Contacts, Labels bitmasks & Blocking
    assert_eq!(contact_call(&ctx, &mut s1, "BlockOwners", &[PyRep::Integer(c1)]).unwrap_err().code, "InvalidArgument");
    contact_call(&ctx, &mut s1, "AddContact", &[PyRep::Integer(c2), PyRep::Real(5.0), PyRep::Bool(true)]).unwrap();
    let l1 = contact_call(&ctx, &mut s1, "CreateLabel", &[PyRep::String("Logistics".into())]).unwrap().as_i64().unwrap();
    let l2 = contact_call(&ctx, &mut s1, "CreateLabel", &[PyRep::String("FCs".into())]).unwrap().as_i64().unwrap();
    assert_eq!((l1, l2), (1, 2));
    contact_call(&ctx, &mut s1, "AssignLabels", &[PyRep::Integer(c2), PyRep::Integer(3)]).unwrap();
    contact_call(&ctx, &mut s1, "RemoveLabels", &[PyRep::Integer(c2), PyRep::Integer(1)]).unwrap();
    let edit_rel = contact_call(&ctx, &mut s1, "EditContactsRelationshipID", &[PyRep::Integer(c2), PyRep::Real(12.0)]).unwrap();
    assert_eq!(edit_rel.dict_get("standing"), Some(&PyRep::Real(10.0)));
    contact_call(&ctx, &mut s1, "DeleteLabel", &[PyRep::Integer(l2)]).unwrap();
}

#[tokio::test]
async fn xmpp_lsc_chat_acl_delayed_local_and_structure_owner_groups_auto_defense_kill_mutations() {
    let ctx = test_ctx_with_clock(1_710_600_000);
    let (c1, c2, c3, corp1, ally1) = seed_three_pilots(&ctx);
    let mut s1 = make_sess(c1, corp1, Some(ally1), corp_roles::ROLE_DIRECTOR);
    let mut s2 = make_sess(c2, corp1, Some(ally1), 0);
    let _s3 = make_sess(c3, 1_000_009, None, 0);

    // 1. XmppChatMgr & LSC Channel Password, Mute Expiry (3600s boundary), ACL Modes (-2, -1, 1, 2, 3), Delayed Local
    let created = social_call(&ctx, &mut s1, "XmppChatMgr", "CreatePlayerOwnedChannel", &[PyRep::String("Fleet Comms".into()), PyRep::String("opsec".into()), PyRep::String("MOTD1".into())]).await.unwrap();
    let chan_id = created.dict_get("channelID").and_then(|v| v.as_str()).unwrap().to_string();

    assert_eq!(social_call(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone()), PyRep::String("wrong".into())]).await.unwrap_err().code, "InvalidChannelPassword");
    social_call(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone()), PyRep::String("opsec".into())]).await.unwrap();

    // Mute c2 (mode = -1, duration = 3600s); blocked at +3599s, allowed at +3600s
    social_call(&ctx, &mut s1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(c2), PyRep::Integer(-1)]).await.unwrap();
    ctx.advance_secs(3_599);
    assert_eq!(social_call(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String(chan_id.clone()), PyRep::String("hello".into())]).await.unwrap_err().code, "MutedInChannel");
    ctx.advance_secs(1);
    let sent_ok = social_call(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String(chan_id.clone()), PyRep::String("hello".into())]).await.unwrap();
    assert_eq!(sent_ok.dict_get("sent"), Some(&PyRep::Bool(true)));

    // Promote c2 to operator (mode = 2): cannot transfer ownership via AccessControl(mode=3)
    social_call(&ctx, &mut s1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(c2), PyRep::Integer(2)]).await.unwrap();
    assert_eq!(social_call(&ctx, &mut s2, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(c3), PyRep::Integer(3)]).await.unwrap_err().code, "NotChannelOwner");

    // Ban c2 (mode = -2) -> ChannelAccessDenied on rejoin; allow (mode = 1) -> rejoin succeeds
    social_call(&ctx, &mut s1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(c2), PyRep::Integer(-2)]).await.unwrap();
    assert_eq!(social_call(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone()), PyRep::String("opsec".into())]).await.unwrap_err().code, "ChannelAccessDenied");
    social_call(&ctx, &mut s1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(c2), PyRep::Integer(1)]).await.unwrap();
    social_call(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(chan_id.clone()), PyRep::String("opsec".into())]).await.unwrap();

    // Delayed local wormhole channel: memberCount = 0 until speaker talks
    let wh_chan = "solarsystemid2:31000005";
    s2.solar_system_id = Some(31_000_005);
    let wh_join = social_call(&ctx, &mut s2, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(wh_chan.into())]).await.unwrap();
    assert_eq!(wh_join.dict_get("isDelayedLocal"), Some(&PyRep::Bool(true)));
    assert_eq!(wh_join.dict_get("memberCount"), Some(&PyRep::Integer(0)));
    social_call(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String(wh_chan.into()), PyRep::String("o7".into())]).await.unwrap();
    let wh_members = social_call(&ctx, &mut s2, "LSC", "GetMembers", &[PyRep::String(wh_chan.into())]).await.unwrap();
    assert_eq!(wh_members.dict_get("members").map(rep_len), Some(1));

    // Ownership transfer, private channel, and LeaveChannel
    social_call(&ctx, &mut s1, "XmppChatMgr", "GrantChannelOwnership", &[PyRep::String(chan_id.clone()), PyRep::Integer(c2)]).await.unwrap();
    let priv_rep = social_call(&ctx, &mut s1, "XmppChatMgr", "CreatePrivateChannel", &[PyRep::Integer(c2)]).await.unwrap();
    assert!(priv_rep.dict_get("channelID").and_then(|v| v.as_str()).is_some_and(|s| s.starts_with("private:")));
    social_call(&ctx, &mut s1, "LSC", "LeaveChannel", &[PyRep::String(chan_id.clone())]).await.unwrap();

    // 2. Structure Access Lists (`ownerGroupManager`), Docking ACL, & Auto-Defense Attacker TTL (900s boundary)
    let (lowsec_sys, struct_id) = (30_002_537_u32, 1_000_900_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp1,
                solar_system_id: lowsec_sys,
                name: "ACL Astrahus".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                ..Default::default()
            },
        );
    });

    let grp_rep = handle_upwell_ops(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Docking ACL".into()), PyRep::String("Test".into()), PyRep::Bool(true)], &HashMap::new())
        .await.unwrap().unwrap();
    let gid = grp_rep.dict_get("groupID").and_then(|v| v.as_i64()).unwrap();

    assert_eq!(handle_acl_rules(&ctx, &mut s1, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(c3)]), PyRep::Integer(-2)], &HashMap::new()).await.unwrap().unwrap_err().code, "InvalidAclRole");
    assert_eq!(handle_acl_rules(&ctx, &mut s1, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(c1)]), PyRep::Integer(0)], &HashMap::new()).await.unwrap().unwrap_err().code, "InsufficientCorpRoles");
    handle_acl_rules(&ctx, &mut s1, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(c2)]), PyRep::Integer(ACL_ROLE_MANAGER)], &HashMap::new()).await.unwrap().unwrap();
    handle_acl_rules(&ctx, &mut s2, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(c3)]), PyRep::Integer(ACL_ROLE_BLOCKED)], &HashMap::new()).await.unwrap().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.entry(struct_id).or_default();
        prof.access_group_ids.insert(gid);
        prof.blocked_characters.insert(c3);
        save_ext_state(snap, &ext);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(verify_structure_docking_acl(&ctx, snap, struct_id, c2).is_ok());
        assert_eq!(verify_structure_docking_acl(&ctx, snap, struct_id, c3).unwrap_err().code, "StructureDockingAccessDenied");
    });

    // Auto-defense attacker 900s TTL boundary: targeted at +899s, not targeted at +900s
    let t0 = ctx.now_unix();
    let mut def = StructureDefenseSettings { structure_id: struct_id, auto_defense_enabled: true, auto_target_attackers: true, ..Default::default() };
    def.record_attacker(c3, t0);
    ctx.db.with_mut_snapshot(|snap| save_defense_settings(snap, &def));
    ctx.advance_secs(STRUCTURE_ATTACKER_TTL_SECS - 1);
    assert!(ctx.db.with_snapshot(|snap| evaluate_auto_defense_target(&ctx, snap, struct_id, c3)));
    ctx.advance_secs(1);
    assert!(!ctx.db.with_snapshot(|snap| evaluate_auto_defense_target(&ctx, snap, struct_id, c3)));
    let _ = AccessGroupRecord::default();
}
