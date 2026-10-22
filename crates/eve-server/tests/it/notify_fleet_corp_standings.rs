//! Standings, Contacts, Fleet, Corporation, Enlistment, Agent Missions, and Mail
//! notification emission tests.

use crate::common;

use eve_db::{AllianceRecord, CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, MailRecord};
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
    // parity: notify:OnSetPlayerStanding, notify:OnSetCorpStanding, notify:OnSetAllianceStanding, notify:OnContactNoLongerContact, notify:OnUnblockContact, notify:OnContactSlashCommand, notify:OnOrganizationContactsUpdated
    eve_testkit::attest(&[
        "notify:OnSetPlayerStanding",
        "notify:OnSetCorpStanding",
        "notify:OnSetAllianceStanding",
        "notify:OnContactNoLongerContact",
        "notify:OnUnblockContact",
        "notify:OnContactSlashCommand",
        "notify:OnOrganizationContactsUpdated",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 801, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 802, "other", "other-secret", P2).await;

    let corp_id = 2_000_001;
    let ally_id = 3_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.corporation_id = corp_id;
            c.alliance_id = Some(ally_id);
        }
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

    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

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
    assert!(!events_from(&n1, "OnSetPlayerStanding").is_empty(), "AddContact must emit OnSetPlayerStanding");

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

    // 4. OnContactSlashCommand & OnSetPlayerStanding: via slash.SlashCmd with GM role
    s1.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
    call(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/contact 90000002 6.5".into())],
    )
    .await;
    let n4 = ctx.drain_notifications_for_char(P1);
    let slash_contact_ev = events_from(&n4, "OnContactSlashCommand");
    assert_eq!(slash_contact_ev, vec![vec![PyRep::Integer(P2), PyRep::Real(6.5)]], "SlashCmd /contact must emit OnContactSlashCommand");
    assert!(!events_from(&n4, "OnSetPlayerStanding").is_empty(), "SlashCmd /contact must emit OnSetPlayerStanding");

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
    assert!(!org_contacts_ev.is_empty(), "SetCorpStanding must emit OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ev[0][0], PyRep::Integer(corp_id));

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
    assert!(!org_contacts_ally_ev.is_empty(), "AddAllianceContact must emit OnOrganizationContactsUpdated");
    assert_eq!(org_contacts_ally_ev[0][0], PyRep::Integer(ally_id));
}

#[tokio::test]
async fn test_fleet_and_corp_enlistment_notifications() {
    // parity: notify:OnFleetBoosted, notify:OnFleetRespawnPointsUpdate, notify:OnFleetJumpBeaconDeployableChange, notify:OnFleetLootEvent, notify:OnFleetEscalatingPathMessage, notify:OnCorpAllowedEnlistmentFactionsSet, notify:OnCorpLogoReady
    eve_testkit::attest(&[
        "notify:OnFleetBoosted",
        "notify:OnFleetRespawnPointsUpdate",
        "notify:OnFleetJumpBeaconDeployableChange",
        "notify:OnFleetLootEvent",
        "notify:OnFleetEscalatingPathMessage",
        "notify:OnCorpAllowedEnlistmentFactionsSet",
        "notify:OnCorpLogoReady",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 803, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 804, "other", "other-secret", P2).await;

    let fleet_id = 42_001;
    let test_corp = 2_000_002;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: P1,
                members: vec![P1, P2],
                ..Default::default()
            },
        );
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
    s1.fleet_id = Some(fleet_id);
    s1.corporation_id = Some(test_corp);

    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

    // 1. OnFleetBoosted
    eve_server::services::fleet_social::fleet_social_notify::emit_fleet_boosted(&ctx, fleet_id, 2488, 2);
    let n1_p1 = ctx.drain_notifications_for_char(P1);
    let n1_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n1_p1, "OnFleetBoosted"),
        vec![vec![PyRep::Integer(2488), PyRep::Integer(2)]],
        "Fleet boosted notification must reach fleet member P1"
    );
    assert_eq!(
        events_from(&n1_p2, "OnFleetBoosted"),
        vec![vec![PyRep::Integer(2488), PyRep::Integer(2)]],
        "Fleet boosted notification must reach fleet member P2"
    );

    // 2. OnFleetRespawnPointsUpdate
    let respawn_points = PyRep::List(vec![PyRep::Integer(STATION_JITA)]);
    eve_server::services::fleet_social::fleet_social_notify::emit_fleet_respawn_points_update(&ctx, fleet_id, respawn_points.clone());
    let n2 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n2, "OnFleetRespawnPointsUpdate"),
        vec![vec![respawn_points]],
        "Fleet respawn points update must match payload"
    );

    // 3. OnFleetJumpBeaconDeployableChange
    eve_server::services::fleet_social::fleet_social_notify::emit_fleet_jump_beacon_deployable_change(
        &ctx, fleet_id, 5001, SYS_JITA as i64, 6001, P1, true,
    );
    let n3 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n3, "OnFleetJumpBeaconDeployableChange"),
        vec![vec![
            PyRep::Integer(5001),
            PyRep::Integer(SYS_JITA as i64),
            PyRep::Integer(6001),
            PyRep::Integer(P1),
            PyRep::Bool(true),
        ]],
        "Jump beacon deployable change must broadcast 5-arg payload"
    );

    // 4. OnFleetLootEvent
    let loot_events = PyRep::List(vec![PyRep::Integer(34)]);
    eve_server::services::fleet_social::fleet_social_notify::emit_fleet_loot_event(&ctx, fleet_id, loot_events.clone());
    let n4 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n4, "OnFleetLootEvent"),
        vec![vec![loot_events]],
        "Fleet loot event must match payload"
    );

    // 5. OnFleetEscalatingPathMessage
    eve_server::services::fleet_social::fleet_social_notify::emit_fleet_escalating_path_message(&ctx, fleet_id, P1, 101);
    let n5 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n5, "OnFleetEscalatingPathMessage"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(101)]],
        "Escalating path message must deliver charID and journalEntryTemplateID"
    );

    // 6. OnCorpAllowedEnlistmentFactionsSet: via fwCharacterEnlistmentMgr.SetMyCorpAllowedEnlistmentFactions
    call(
        &ctx,
        &mut s1,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(500001)])],
    )
    .await;
    let n6 = ctx.drain_notifications_for_char(P1);
    let fac_ev = events_from(&n6, "OnCorpAllowedEnlistmentFactionsSet");
    assert_eq!(
        fac_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(500001)])]],
        "SetMyCorpAllowedEnlistmentFactions must emit OnCorpAllowedEnlistmentFactionsSet"
    );

    // 7. OnCorpLogoReady: via corpRegistry.UpdateLogo
    call(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateLogo",
        &[PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(5)],
    )
    .await;
    let n7 = ctx.drain_notifications_for_char(P1);
    let logo_ev = events_from(&n7, "OnCorpLogoReady");
    assert_eq!(
        logo_ev,
        vec![vec![PyRep::Integer(test_corp), PyRep::Integer(128)]],
        "UpdateLogo must emit OnCorpLogoReady with corpID and size 128"
    );
}

#[tokio::test]
async fn test_missions_and_mail_notifications() {
    // parity: notify:OnAgentMissionChange, notify:OnAgentMissionObjectivesCompleted, notify:OnAgentMissionObjectivesFailed, notify:OnMissionsUpdated, notify:OnCreateNewMailWindow, notify:OnIncomingTransmission, notify:OnMailRestored, notify:OnMailTrashed, notify:OnMailUndeleted
    eve_testkit::attest(&[
        "notify:OnAgentMissionChange",
        "notify:OnAgentMissionObjectivesCompleted",
        "notify:OnAgentMissionObjectivesFailed",
        "notify:OnMissionsUpdated",
        "notify:OnCreateNewMailWindow",
        "notify:OnIncomingTransmission",
        "notify:OnMailRestored",
        "notify:OnMailTrashed",
        "notify:OnMailUndeleted",
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

    // 1. OnAgentMissionChange (Action 1 = Offer)
    let m_offer = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let mission_id = m_offer.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    let n1 = ctx.drain_notifications_for_char(P1);
    let offer_ev = events_from(&n1, "OnAgentMissionChange");
    assert_eq!(
        offer_ev,
        vec![vec![PyRep::Integer(1), PyRep::Integer(dist_agent_id)]],
        "Mission offer must emit OnAgentMissionChange with action 1"
    );

    // 2. OnAgentMissionChange (Action 2 = Accept)
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id)],
    )
    .await;
    let n2 = ctx.drain_notifications_for_char(P1);
    let accept_ev = events_from(&n2, "OnAgentMissionChange");
    assert_eq!(
        accept_ev,
        vec![vec![PyRep::Integer(2), PyRep::Integer(dist_agent_id)]],
        "Mission accept must emit OnAgentMissionChange with action 2"
    );

    // 3. OnMissionsUpdated via missionTrackerMgr.UpdateAllMissions
    call(&ctx, &mut s1, "missionTrackerMgr", "UpdateAllMissions", &[]).await;
    let n3 = ctx.drain_notifications_for_char(P1);
    let update_ev = events_from(&n3, "OnMissionsUpdated");
    assert!(!update_ev.is_empty(), "UpdateAllMissions must emit OnMissionsUpdated");

    // 4. OnAgentMissionObjectivesCompleted & OnAgentMissionChange (Action 3 = Complete)
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
    let complete_change_ev = events_from(&n4, "OnAgentMissionChange");
    assert!(
        complete_change_ev.iter().any(|args| args == &vec![PyRep::Integer(3), PyRep::Integer(dist_agent_id)]),
        "Complete mission must emit OnAgentMissionChange with action 3"
    );

    // Clean up active mission record so agent can offer again cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.remove(&(P1, mission_id as u32));
    });

    // 5. OnAgentMissionObjectivesFailed & OnAgentMissionChange (Action 4 = Quit/Fail)
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
    let fail_change_ev = events_from(&n5, "OnAgentMissionChange");
    assert!(
        fail_change_ev.iter().any(|args| args == &vec![PyRep::Integer(4), PyRep::Integer(dist_agent_id)]),
        "Quit mission must emit OnAgentMissionChange with action 4"
    );

    // 6. OnCreateNewMailWindow
    eve_server::services::mail_notify::emit_create_new_mail_window(
        &ctx,
        P1,
        &[P2],
        None,
        None,
        "Welcome to Fleet",
        "Report for fleet deployment at 20:00",
    );
    let n6 = ctx.drain_notifications_for_char(P1);
    let new_mail_win_ev = events_from(&n6, "OnCreateNewMailWindow");
    assert_eq!(
        new_mail_win_ev,
        vec![vec![
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Welcome to Fleet".into()),
            PyRep::String("Report for fleet deployment at 20:00".into()),
        ]],
        "OnCreateNewMailWindow must match recipients, subject, and body"
    );

    // 7. OnIncomingTransmission
    eve_server::services::mail_notify::emit_incoming_transmission(
        &ctx,
        P1,
        "CONCORD Priority Transmission",
        "Hostile incursions detected in constellation",
        true,
    );
    let n7 = ctx.drain_notifications_for_char(P1);
    let trans_ev = events_from(&n7, "OnIncomingTransmission");
    assert_eq!(trans_ev.len(), 1, "OnIncomingTransmission must emit 1 event");
    assert_eq!(trans_ev[0][1], PyRep::Bool(true));
    let trans_payload = &trans_ev[0][0];
    assert_eq!(
        trans_payload.dict_get("header"),
        Some(&PyRep::String("CONCORD Priority Transmission".into()))
    );

    // 8. OnMailTrashed: seed a mail and MoveToTrash
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
    let n8 = ctx.drain_notifications_for_char(P1);
    let trashed_ev = events_from(&n8, "OnMailTrashed");
    assert_eq!(
        trashed_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(mail_id)])]],
        "MoveToTrash must emit OnMailTrashed with mail IDs"
    );

    // 9. OnMailRestored & OnMailUndeleted: MoveFromTrash
    call(
        &ctx,
        &mut s1,
        "mailMgr",
        "MoveFromTrash",
        &[PyRep::List(vec![PyRep::Integer(mail_id)])],
    )
    .await;
    let n9 = ctx.drain_notifications_for_char(P1);
    let restored_ev = events_from(&n9, "OnMailRestored");
    let undeleted_ev = events_from(&n9, "OnMailUndeleted");
    assert_eq!(
        restored_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(mail_id)])]],
        "MoveFromTrash must emit OnMailRestored"
    );
    assert_eq!(
        undeleted_ev,
        vec![vec![PyRep::List(vec![PyRep::Integer(mail_id)])]],
        "MoveFromTrash must emit OnMailUndeleted"
    );
}
