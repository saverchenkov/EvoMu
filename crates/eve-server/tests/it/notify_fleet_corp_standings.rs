//! Standings, Contacts, Fleet, Corporation, Enlistment, Agent Missions, and Mail
//! notification emission tests.

use crate::common;

use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, MailRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::{HashMap, HashSet};

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs
        .iter()
        .filter(|n| n.event == event)
        .map(|n| n.args.clone())
        .collect()
}

#[tokio::test]
async fn test_standings_and_contacts_notifications() {
    // parity: notify:OnSetPlayerStanding, notify:OnSetCorpStanding, notify:OnSetAllianceStanding, notify:OnContactNoLongerContact, notify:OnUnblockContact, notify:OnOrganizationContactsUpdated
    eve_testkit::attest(&[
        "notify:OnSetPlayerStanding",
        "notify:OnSetCorpStanding",
        "notify:OnSetAllianceStanding",
        "notify:OnContactNoLongerContact",
        "notify:OnUnblockContact",
        "notify:OnOrganizationContactsUpdated",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 802, "other", "other-secret", P2).await;

    let corp_id = 2_000_001;
    let ally_id = 3_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
        }
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TEST".into(),
                ceo_id: P1,
                alliance_id: Some(ally_id),
                tax_rate: 0.0,
                master_wallet: eve_db::IskCents(0),
                members: HashSet::from([P1, P2]),
                member_roles: HashMap::from([(P1, u64::MAX), (P2, 0)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(STATION_JITA),
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Alliance".into(),
                short_name: "TESTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });
    s1.corporation_id = Some(corp_id);
    s1.alliance_id = Some(ally_id);
    s2.corporation_id = Some(corp_id);
    s2.alliance_id = Some(ally_id);

    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

    // BF-CORPCONTACT-AUTH-01: member P2 without diplomat role cannot AddCorporateContact
    let denied = ctx
        .dispatch_rpc(
            &mut s2,
            "corpRegistry",
            None,
            "AddCorporateContact",
            &[PyRep::Integer(P1), PyRep::Real(5.0), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(denied.is_err(), "Non-diplomat AddCorporateContact must be denied");
    assert_eq!(denied.unwrap_err().code, "CorpRoleDenied");
    assert!(ctx.drain_notifications_for_char(P2).is_empty());
    assert!(ctx.drain_notifications_for_char(P1).is_empty());

    // 1. OnSetPlayerStanding: via charMgr.AddContact
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "AddContact",
        &[
            PyRep::Integer(P2),
            PyRep::Real(5.0),
            PyRep::Bool(false),
            PyRep::Bool(false),
            PyRep::String("Friend".into()),
        ],
    )
    .await;
    let n1 = ctx.drain_notifications_for_char(P1);
    let standing_ev = events_from(&n1, "OnSetPlayerStanding");
    assert_eq!(
        standing_ev,
        vec![vec![PyRep::Integer(P2), PyRep::Real(5.0)]],
        "AddContact must emit OnSetPlayerStanding with contactID and standing"
    );

    // 2. OnContactNoLongerContact: via charMgr.DeleteContacts
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "DeleteContacts",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await;
    let n2 = ctx.drain_notifications_for_char(P1);
    let no_contact_ev = events_from(&n2, "OnContactNoLongerContact");
    assert_eq!(no_contact_ev, vec![vec![PyRep::Integer(P2)]], "DeleteContacts must emit OnContactNoLongerContact");

    // Negative test: deleting non-contact emits no notification
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "DeleteContacts",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await;
    let n2_neg = ctx.drain_notifications_for_char(P1);
    assert!(events_from(&n2_neg, "OnContactNoLongerContact").is_empty(), "Deleting non-contact must emit no notification");

    // 3. OnUnblockContact: via charMgr.BlockOwners and UnblockOwners
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "BlockOwners",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await;
    ctx.drain_notifications_for_char(P1);
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await;
    let n3 = ctx.drain_notifications_for_char(P1);
    let unblock_ev = events_from(&n3, "OnUnblockContact");
    assert_eq!(unblock_ev, vec![vec![PyRep::Integer(P2)]], "UnblockOwners must emit OnUnblockContact");

    // Negative test: unblocking an unblocked owner emits no notification
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(P2)])],
    )
    .await;
    let n3_neg = ctx.drain_notifications_for_char(P1);
    assert!(events_from(&n3_neg, "OnUnblockContact").is_empty(), "Unblocking unblocked owner must emit no notification");

    // 4. OnSetPlayerStanding via slash.SlashCmd with GM role
    s1.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
    // Negative tests for invalid target and NaN standing
    let bad_target = ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/contact 95000000 5.0".into())], &HashMap::new(), None).await;
    assert!(bad_target.is_err(), "SlashCmd with non-existent target must fail");
    let bad_nan = ctx.dispatch_rpc(&mut s1, "slash", None, "SlashCmd", &[PyRep::String("/contact 90000002 NaN".into())], &HashMap::new(), None).await;
    assert!(bad_nan.is_err(), "SlashCmd with NaN standing must fail");

    call(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/contact 90000002 6.5".into())],
    )
    .await;
    let n4 = ctx.drain_notifications_for_char(P1);
    let slash_standing_ev = events_from(&n4, "OnSetPlayerStanding");
    assert_eq!(
        slash_standing_ev,
        vec![vec![PyRep::Integer(P2), PyRep::Real(6.5)]],
        "SlashCmd /contact must emit OnSetPlayerStanding with contactID and standing"
    );

    // 5. OnSetCorpStanding & OnOrganizationContactsUpdated: via corpRegistry.AddCorporateContact
    call(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(P2), PyRep::Real(7.5), PyRep::Integer(1)],
    )
    .await;
    let n5 = ctx.drain_notifications_for_char(P1);
    let corp_standing_ev = events_from(&n5, "OnSetCorpStanding");
    assert_eq!(corp_standing_ev, vec![vec![PyRep::Integer(P2), PyRep::Real(7.5)]], "SetCorpStanding must emit OnSetCorpStanding");
    let org_contacts_ev = events_from(&n5, "OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ev.len(), 1, "Must emit exactly one OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ev[0][0], PyRep::Integer(corp_id));
    assert!(matches!(&org_contacts_ev[0][1], PyRep::List(list) if !list.is_empty()), "Updates payload must be non-empty list");

    // 6. OnSetAllianceStanding & OnOrganizationContactsUpdated: via allianceRegistry.AddAllianceContact
    call(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(P2), PyRep::Real(8.5), PyRep::Integer(2)],
    )
    .await;
    let n6 = ctx.drain_notifications_for_char(P1);
    let ally_standing_ev = events_from(&n6, "OnSetAllianceStanding");
    assert_eq!(ally_standing_ev, vec![vec![PyRep::Integer(P2), PyRep::Real(8.5)]], "AddAllianceContact must emit OnSetAllianceStanding");
    let org_contacts_ally_ev = events_from(&n6, "OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ally_ev.len(), 1, "Must emit exactly one OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ally_ev[0][0], PyRep::Integer(ally_id));
    assert!(matches!(&org_contacts_ally_ev[0][1], PyRep::List(list) if !list.is_empty()), "Updates payload must be non-empty list");
}

#[tokio::test]
async fn test_corp_enlistment_and_logo_notifications() {
    // parity: notify:OnCorpAllowedEnlistmentFactionsSet, notify:OnCorpLogoReady
    eve_testkit::attest(&[
        "notify:OnCorpAllowedEnlistmentFactionsSet",
        "notify:OnCorpLogoReady",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 803, "demo", "demo-secret", P1).await;

    let test_corp = 2_000_002;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.corporation_id = test_corp;
        }
        snap.corporations.insert(
            test_corp,
            CorporationRecord {
                corporation_id: test_corp,
                name: "FW Test Corp".into(),
                ticker: "FWTC".into(),
                ceo_id: P1,
                hq_station_id: Some(STATION_JITA),
                ..Default::default()
            },
        );
    });
    s1.corporation_id = Some(test_corp);

    ctx.drain_notifications_for_char(P1);

    // 1. OnCorpAllowedEnlistmentFactionsSet: via fwCharacterEnlistmentMgr.SetMyCorpAllowedEnlistmentFactions
    call(
        &ctx,
        &mut s1,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(500001)])],
    )
    .await;
    let n1 = ctx.drain_notifications_for_char(P1);
    let fac_ev = events_from(&n1, "OnCorpAllowedEnlistmentFactionsSet");
    assert_eq!(
        fac_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(500001)])]],
        "SetMyCorpAllowedEnlistmentFactions must emit OnCorpAllowedEnlistmentFactionsSet"
    );

    // 2. OnCorpLogoReady: via corpRegistry.UpdateLogo
    call(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateLogo",
        &[PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(5)],
    )
    .await;
    let n2 = ctx.drain_notifications_for_char(P1);
    let logo_ev = events_from(&n2, "OnCorpLogoReady");
    assert_eq!(
        logo_ev,
        vec![vec![PyRep::Integer(test_corp), PyRep::Integer(128)]],
        "UpdateLogo must emit OnCorpLogoReady with corpID and size 128"
    );
}

#[tokio::test]
async fn test_missions_and_mail_notifications() {
    // parity: notify:OnMissionsUpdated, notify:OnMailRestored, notify:OnMailTrashed
    eve_testkit::attest(&[
        "notify:OnMissionsUpdated",
        "notify:OnMailRestored",
        "notify:OnMailTrashed",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 805, "demo", "demo-secret", P1).await;

    // Locate an SDE Level 1 distribution agent for deterministic testing
    let (dist_agent_id, dist_corp_id, dist_station_id, dist_sys_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| {
            (
                a.agent_id,
                a.corporation_id,
                a.station_id.unwrap_or(STATION_JITA),
                a.solar_system_id.unwrap_or(SYS_JITA),
            )
        })
        .expect("SDE distribution agent");

    // Colocate P1 with agent
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
            ch.standings.insert(dist_corp_id, 3.0);
        }
    });
    s1.solar_system_id = Some(dist_sys_id);
    s1.station_id = Some(dist_station_id);

    ctx.drain_notifications_for_char(P1);

    // 1. Mission Offer and Accept
    let m_offer = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let mission_id = m_offer.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);

    // 2. OnMissionsUpdated via missionTrackerMgr.UpdateAllMissions
    call(&ctx, &mut s1, "missionTrackerMgr", "UpdateAllMissions", &[]).await;
    let n3 = ctx.drain_notifications_for_char(P1);
    let update_ev = events_from(&n3, "OnMissionsUpdated");
    assert_eq!(update_ev.len(), 1, "UpdateAllMissions must emit OnMissionsUpdated");
    assert!(matches!(&update_ev[0][0], PyRep::List(list) if !list.is_empty()), "Missions payload must be non-empty list");

    // 3. OnAgentMissionObjectivesCompleted (Action 6 = Complete)
    // Seed delivery cargo for courier/distribution mission completion
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            920_060,
            InventoryItemRecord {
                item_id: 920_060,
                type_id: 3683,
                owner_id: P1,
                location_id: dist_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.drain_notifications_for_char(P1);
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id)],
    )
    .await;
    let n4 = ctx.drain_notifications_for_char(P1);
    let obj_done_ev = events_from(&n4, "OnAgentMissionObjectivesCompleted");
    assert_eq!(
        obj_done_ev,
        vec![vec![PyRep::Integer(dist_agent_id)]],
        "Complete mission must emit OnAgentMissionObjectivesCompleted"
    );

    // Clean up active mission record so agent can offer again cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.remove(&(P1, mission_id as u32));
    });

    // 4. OnAgentMissionObjectivesFailed (Action 11 = Quit)
    // Request a second mission and quit it
    let m_offer2 = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let mission_id2 = m_offer2.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID2");
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id2)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);

    // Quit (Action 11)
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(11), PyRep::Integer(mission_id2)],
    )
    .await;
    let n5 = ctx.drain_notifications_for_char(P1);
    let obj_fail_ev = events_from(&n5, "OnAgentMissionObjectivesFailed");
    assert_eq!(
        obj_fail_ev,
        vec![vec![PyRep::Integer(dist_agent_id)]],
        "Quit mission must emit OnAgentMissionObjectivesFailed"
    );

    // 5. OnMailTrashed: seed a mail and MoveToTrash
    // BF-NOTIFY-TRASH-01 & BF-NOTIFY-TRASH-02 negative tests:
    // A. Trashing non-existent mail emits zero notifications
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(999_999)])],
    )
    .await;
    let n_trash_neg = ctx.drain_notifications_for_char(P1);
    assert!(
        events_from(&n_trash_neg, "OnMailTrashed").is_empty(),
        "Trashing non-existent mail must emit zero notifications"
    );

    // B. Trashing foreign mail (owned by P2) when called by P1 emits zero notifications
    let foreign_mail_id = 888_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: foreign_mail_id,
            sender_id: P2,
            recipient_id: P2,
            subject: "P2 Secret Mail".into(),
            body: "P2 eyes only".into(),
        });
    });
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(foreign_mail_id)])],
    )
    .await;
    assert!(
        events_from(&ctx.drain_notifications_for_char(P1), "OnMailTrashed").is_empty(),
        "Trashing foreign mail must emit zero notifications for caller"
    );
    assert!(
        events_from(&ctx.drain_notifications_for_char(P2), "OnMailTrashed").is_empty(),
        "Trashing foreign mail must emit zero notifications for owner"
    );

    // C. Trashing deleted mail (in snap.deleted_mails) emits zero notifications
    let deleted_mail_id = 888_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: deleted_mail_id,
            sender_id: P1,
            recipient_id: P1,
            subject: "Deleted Mail".into(),
            body: "Already purged".into(),
        });
        snap.deleted_mails.insert((P1, deleted_mail_id));
    });
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(deleted_mail_id)])],
    )
    .await;
    assert!(
        events_from(&ctx.drain_notifications_for_char(P1), "OnMailTrashed").is_empty(),
        "Trashing deleted mail must emit zero notifications"
    );

    let mail_id = 888_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: mail_id,
            sender_id: P1,
            recipient_id: P1,
            subject: "Test Mail Trashed".into(),
            body: "Content to trash".into(),
        });
    });
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(mail_id)])],
    )
    .await;
    let n6 = ctx.drain_notifications_for_char(P1);
    let trashed_ev = events_from(&n6, "OnMailTrashed");
    assert_eq!(
        trashed_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(mail_id)])]],
        "MoveToTrash must emit OnMailTrashed with mail IDs"
    );

    // BF-NOTIFY-TRASH-01 negative test: trashing already-trashed mail emits zero notifications
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveToTrash",
        &[PyRep::List(vec![PyRep::Integer(mail_id)])],
    )
    .await;
    let n_trash_dup = ctx.drain_notifications_for_char(P1);
    assert!(
        events_from(&n_trash_dup, "OnMailTrashed").is_empty(),
        "Trashing already-trashed mail must emit zero notifications"
    );

    // 7. OnMailRestored: MoveFromTrash
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveFromTrash",
        &[PyRep::List(vec![PyRep::Integer(mail_id)])],
    )
    .await;
    let n7 = ctx.drain_notifications_for_char(P1);
    let restored_ev = events_from(&n7, "OnMailRestored");
    assert_eq!(
        restored_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(mail_id)])]],
        "MoveFromTrash must emit OnMailRestored"
    );
}
