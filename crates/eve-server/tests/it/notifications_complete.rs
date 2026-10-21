//! Comprehensive acceptance tests for server-emitted notifications.
//!
//! Validates event name, wire marshaling, and multicast scope targeting across characters,
//! corporations, alliances, fleets, and solar systems.


use crate::common;

use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::{marshal, PyRep};
use eve_server::router::{
    cache_notifications::{NotificationTarget, PendingMachoNotification},
    ServerContext,
};
use std::collections::{BTreeSet, HashMap};

fn assert_notif<'a>(notifs: &[PendingMachoNotification], expected_event: &'a str, expected_char_id: i64) -> &'a str {
    assert!(!notifs.is_empty(), "Expected at least 1 notification for {expected_event}");
    let notif = &notifs[0];
    assert_eq!(notif.event, expected_event, "Event name mismatch for {expected_event}");
    assert_eq!(
        notif.recipient_char_id, expected_char_id,
        "Recipient mismatch for {expected_event}"
    );
    let rep = marshal::decode(&notif.marshaled_packet).expect("decode packet");
    let pkt = MachoPacket::from_pyrep(&rep).expect("parse MachoPacket");
    assert_eq!(
        pkt.packet_type,
        MachoPacketType::Notification,
        "PacketType must be Notification for {expected_event}"
    );
    expected_event
}

fn assert_multicast<'a>(
    notifs: &[PendingMachoNotification],
    expected_event: &'a str,
    expected_recipients: &[i64],
    unexpected_recipients: &[i64],
) -> &'a str {
    assert_eq!(
        notifs.len(),
        expected_recipients.len(),
        "Multicast count mismatch for {expected_event}"
    );
    for n in notifs {
        assert_eq!(n.event, expected_event);
        let rep = marshal::decode(&n.marshaled_packet).expect("decode");
        let pkt = MachoPacket::from_pyrep(&rep).expect("parse MachoPacket");
        assert_eq!(pkt.packet_type, MachoPacketType::Notification);
    }
    let actual_recips: BTreeSet<i64> = notifs.iter().map(|n| n.recipient_char_id).collect();
    for &exp in expected_recipients {
        assert!(
            actual_recips.contains(&exp),
            "Expected recipient {exp} missing for {expected_event}"
        );
    }
    for &unexp in unexpected_recipients {
        assert!(
            !actual_recips.contains(&unexp),
            "Unexpected recipient {unexp} received {expected_event}"
        );
    }
    expected_event
}

async fn setup_test_pilots(ctx: &ServerContext) -> (SessionState, SessionState, SessionState) {
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // Pilot 1 (char 90000001, corp 98000001, sys 30000142, station 60003760)
    let mut s1 = SessionState::new(301);
    common::login_pilot(ctx, &mut s1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut s1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    // Pilot 2 (char 90000002, corp 98000001, sys 30000142, station 60003760)
    let mut s2 = SessionState::new(302);
    common::login_pilot(ctx, &mut s2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut s2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 2");

    // Pilot 3 (char 90000003, corp 98000002, sys 30000144, station 60003754)
    let p3_uid = ctx.db.provision_player_account("pilot3", "pilot3-secret");
    let p3_char = ctx
        .db
        .create_character(p3_uid, "Pilot Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create character for pilot3");
    let p3_id = p3_char.character_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p3_id) {
            ch.solar_system_id = 30_000_144;
            ch.corporation_id = 98_000_002;
            ch.station_id = Some(60_003_754);
        }
    });

    let mut s3 = SessionState::new(303);
    common::login_pilot(ctx, &mut s3, "pilot3", "pilot3-secret").await;
    ctx.dispatch_rpc(
        &mut s3,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(p3_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 3");

    // Set Pilot 1 and 2 to corp 98000001
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = 98_000_001;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
    });

    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(p3_id);

    (s1, s2, s3)
}

#[tokio::test]
async fn test_notifications_fleet_pipelines() {
    // parity: notify:OnFleetInvite, notify:OnFleetDisbanded, notify:OnFleetJoinRequest, notify:OnFleetJoinReject
    // parity: notify:OnFleetJoinRejected, notify:OnFleetMemberChanged, notify:OnFleetMotdChanged, notify:OnChangeFleetBoss
    // parity: notify:OnFleetOptionsChanged, notify:OnFleetSetupChanged, notify:OnFleetWingAdded, notify:OnFleetWingDeleted
    // parity: notify:OnFleetWingNameChanged, notify:OnFleetSquadAdded, notify:OnFleetSquadDeleted, notify:OnFleetSquadNameChanged
    // parity: notify:OnFleetFinderAdvertChanged, notify:OnFleetStateChange, notify:OnFleetJumpBeaconModuleChange, notify:OnFleetBroadcast
    // parity: notify:OnFleetJoin, notify:OnFleetLeave
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p3_id = s3.character_id.unwrap();
    let fid = 880_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fid,
            eve_db::FleetRecord {
                fleet_id: fid,
                boss_id: 90_000_001,
                members: vec![90_000_001, 90_000_002],
                ..Default::default()
            },
        );
    });

    eve_testkit::attest(&[
        "notify:OnFleetInvite",
        "notify:OnFleetDisbanded",
        "notify:OnFleetJoinRequest",
        "notify:OnFleetJoinReject",
        "notify:OnFleetJoinRejected",
        "notify:OnFleetMemberChanged",
        "notify:OnFleetMotdChanged",
        "notify:OnChangeFleetBoss",
        "notify:OnFleetOptionsChanged",
        "notify:OnFleetSetupChanged",
        "notify:OnFleetWingAdded",
        "notify:OnFleetWingDeleted",
        "notify:OnFleetWingNameChanged",
        "notify:OnFleetSquadAdded",
        "notify:OnFleetSquadDeleted",
        "notify:OnFleetSquadNameChanged",
        "notify:OnFleetFinderAdvertChanged",
        "notify:OnFleetStateChange",
        "notify:OnFleetJumpBeaconModuleChange",
        "notify:OnFleetBroadcast",
        "notify:OnFleetJoin",
        "notify:OnFleetLeave",
    ]);

    // Unicast to P3
    assert_eq!(assert_notif(&ctx.notify_fleet_invite(p3_id, 90_000_001, fid), "OnFleetInvite", p3_id), "OnFleetInvite");
    assert_eq!(assert_notif(&ctx.notify_fleet_join_reject(p3_id, fid, "Rejected"), "OnFleetJoinReject", p3_id), "OnFleetJoinReject");
    assert_eq!(assert_notif(&ctx.notify_fleet_join_rejected(p3_id, fid, "Rejected"), "OnFleetJoinRejected", p3_id), "OnFleetJoinRejected");
    assert_eq!(assert_notif(&ctx.notify_fleet_jump_beacon_module_change(90_000_001, 30_000_142, true), "OnFleetJumpBeaconModuleChange", 90_000_001), "OnFleetJumpBeaconModuleChange");

    // Multicast to fleet members (P1 and P2, but not P3)
    let fleet_multicast = &[90_000_001, 90_000_002];
    let non_fleet = &[p3_id];
    assert_eq!(assert_multicast(&ctx.notify_fleet_join(fid, 90_000_001, 1, 0, 0), "OnFleetJoin", fleet_multicast, non_fleet), "OnFleetJoin");
    assert_eq!(assert_multicast(&ctx.notify_fleet_leave(fid, 90_000_002), "OnFleetLeave", fleet_multicast, non_fleet), "OnFleetLeave");
    assert_eq!(assert_multicast(&ctx.notify_fleet_broadcast(fid, "Target", 90_000_001, 501), "OnFleetBroadcast", fleet_multicast, non_fleet), "OnFleetBroadcast");
    assert_eq!(assert_multicast(&ctx.notify_fleet_disbanded(fid), "OnFleetDisbanded", fleet_multicast, non_fleet), "OnFleetDisbanded");
    assert_eq!(assert_multicast(&ctx.notify_fleet_join_request(fid, p3_id), "OnFleetJoinRequest", fleet_multicast, non_fleet), "OnFleetJoinRequest");
    assert_eq!(assert_multicast(&ctx.notify_fleet_member_changed(fid, 90_000_001, PyRep::Dict(vec![])), "OnFleetMemberChanged", fleet_multicast, non_fleet), "OnFleetMemberChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_motd_changed(fid, "Fleet MOTD"), "OnFleetMotdChanged", fleet_multicast, non_fleet), "OnFleetMotdChanged");
    assert_eq!(assert_multicast(&ctx.notify_change_fleet_boss(fid, 90_000_002), "OnChangeFleetBoss", fleet_multicast, non_fleet), "OnChangeFleetBoss");
    assert_eq!(assert_multicast(&ctx.notify_fleet_options_changed(fid, PyRep::Dict(vec![])), "OnFleetOptionsChanged", fleet_multicast, non_fleet), "OnFleetOptionsChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_setup_changed(fid), "OnFleetSetupChanged", fleet_multicast, non_fleet), "OnFleetSetupChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_wing_added(fid, 1, "Wing 1"), "OnFleetWingAdded", fleet_multicast, non_fleet), "OnFleetWingAdded");
    assert_eq!(assert_multicast(&ctx.notify_fleet_wing_deleted(fid, 1), "OnFleetWingDeleted", fleet_multicast, non_fleet), "OnFleetWingDeleted");
    assert_eq!(assert_multicast(&ctx.notify_fleet_wing_name_changed(fid, 1, "Wing Alpha"), "OnFleetWingNameChanged", fleet_multicast, non_fleet), "OnFleetWingNameChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_squad_added(fid, 1, 10, "Squad 1"), "OnFleetSquadAdded", fleet_multicast, non_fleet), "OnFleetSquadAdded");
    assert_eq!(assert_multicast(&ctx.notify_fleet_squad_deleted(fid, 10), "OnFleetSquadDeleted", fleet_multicast, non_fleet), "OnFleetSquadDeleted");
    assert_eq!(assert_multicast(&ctx.notify_fleet_squad_name_changed(fid, 10, "Squad Red"), "OnFleetSquadNameChanged", fleet_multicast, non_fleet), "OnFleetSquadNameChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_finder_advert_changed(fid), "OnFleetFinderAdvertChanged", fleet_multicast, non_fleet), "OnFleetFinderAdvertChanged");
    assert_eq!(assert_multicast(&ctx.notify_fleet_state_change(fid, PyRep::Dict(vec![])), "OnFleetStateChange", fleet_multicast, non_fleet), "OnFleetStateChange");
}

#[tokio::test]
async fn test_notifications_chat_mail_pipelines() {
    // parity: notify:OnChannelJoined, notify:OnChannelLeft, notify:OnChannelDeleted, notify:OnClientEvent_ChatMessageSent
    // parity: notify:OnMailSent, notify:OnNewMailReceived, notify:OnMailDeleted, notify:OnMailTrashed
    // parity: notify:OnMailRestored, notify:OnMailCountersUpdate, notify:OnNewCalendarEvent, notify:OnNotificationReceived
    // parity: notify:OnNotificationDeleted
    let ctx = ServerContext::new();
    let (_s1, _s2, _s3) = setup_test_pilots(&ctx).await;
    let p1 = 90_000_001i64;

    eve_testkit::attest(&[
        "notify:OnChannelJoined",
        "notify:OnChannelLeft",
        "notify:OnChannelDeleted",
        "notify:OnClientEvent_ChatMessageSent",
        "notify:OnMailSent",
        "notify:OnNewMailReceived",
        "notify:OnMailDeleted",
        "notify:OnMailTrashed",
        "notify:OnMailRestored",
        "notify:OnMailCountersUpdate",
        "notify:OnNewCalendarEvent",
        "notify:OnNotificationReceived",
        "notify:OnNotificationDeleted",
    ]);

    assert_eq!(assert_notif(&ctx.notify_channel_joined(p1, 1001), "OnChannelJoined", p1), "OnChannelJoined");
    assert_eq!(assert_notif(&ctx.notify_channel_left(p1, 1001), "OnChannelLeft", p1), "OnChannelLeft");
    assert_eq!(assert_notif(&ctx.notify_channel_deleted(p1, 1001), "OnChannelDeleted", p1), "OnChannelDeleted");
    assert_eq!(assert_notif(&ctx.notify_chat_message_sent(p1, 1001, "Hello chat"), "OnClientEvent_ChatMessageSent", p1), "OnClientEvent_ChatMessageSent");
    assert_eq!(assert_notif(&ctx.notify_mail_sent(p1, 2001), "OnMailSent", p1), "OnMailSent");
    assert_eq!(assert_notif(&ctx.notify_new_mail_received(p1, 2002), "OnNewMailReceived", p1), "OnNewMailReceived");
    assert_eq!(assert_notif(&ctx.notify_mail_deleted(p1, 2003), "OnMailDeleted", p1), "OnMailDeleted");
    assert_eq!(assert_notif(&ctx.notify_mail_trashed(p1, 2004), "OnMailTrashed", p1), "OnMailTrashed");
    assert_eq!(assert_notif(&ctx.notify_mail_restored(p1, 2005), "OnMailRestored", p1), "OnMailRestored");
    assert_eq!(assert_notif(&ctx.notify_mail_counters_update(p1, 5), "OnMailCountersUpdate", p1), "OnMailCountersUpdate");
    assert_eq!(assert_notif(&ctx.notify_calendar_event(p1, 3001), "OnNewCalendarEvent", p1), "OnNewCalendarEvent");
    assert_eq!(assert_notif(&ctx.notify_notification_deleted(p1, 4001), "OnNotificationDeleted", p1), "OnNotificationDeleted");
    let notifs = ctx.notify(NotificationTarget::Character(p1), "OnNotificationReceived", "charid", vec![PyRep::Integer(4002)]);
    assert_eq!(assert_notif(&notifs, "OnNotificationReceived", p1), "OnNotificationReceived");
}

#[tokio::test]
async fn test_notifications_corp_alliance_pipelines() {
    // parity: notify:OnCorporationChanged, notify:OnCorporationMemberChanged, notify:OnCorporationApplicationChanged, notify:OnCorporationRecruitmentAdChanged
    // parity: notify:OnCorporationWelcomeMailChanged, notify:OnCorporationMedalAdded, notify:OnCorpLogoReady, notify:OnCorpAggressionSettingsChange
    // parity: notify:OnCorpAllowedEnlistmentFactionsSet, notify:OnAllianceChanged, notify:OnAllianceMemberChanged, notify:OnAllianceApplicationChanged
    // parity: notify:OnAllianceRelationshipChanged
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p3_id = s3.character_id.unwrap();
    let corp_members = &[90_000_001, 90_000_002];
    let non_corp = &[p3_id];
    let cid = 98_000_001i64;
    let aid = 99_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(aid);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.alliance_id = Some(aid);
        }
        snap.corporations.insert(
            cid,
            eve_db::CorporationRecord {
                corporation_id: cid,
                name: "Test Corp".into(),
                ticker: "TEST".into(),
                ceo_id: 90_000_001,
                alliance_id: Some(aid),
                ..Default::default()
            },
        );
        let mut member_corp_ids = std::collections::HashSet::new();
        member_corp_ids.insert(cid);
        snap.alliances.insert(
            aid,
            eve_db::AllianceRecord {
                alliance_id: aid,
                name: "Test Alliance".into(),
                short_name: "ALLY".into(),
                executor_corp_id: cid,
                member_corp_ids,
            },
        );
    });

    eve_testkit::attest(&[
        "notify:OnCorporationChanged",
        "notify:OnCorporationMemberChanged",
        "notify:OnCorporationApplicationChanged",
        "notify:OnCorporationRecruitmentAdChanged",
        "notify:OnCorporationWelcomeMailChanged",
        "notify:OnCorporationMedalAdded",
        "notify:OnCorpLogoReady",
        "notify:OnCorpAggressionSettingsChange",
        "notify:OnCorpAllowedEnlistmentFactionsSet",
        "notify:OnAllianceChanged",
        "notify:OnAllianceMemberChanged",
        "notify:OnAllianceApplicationChanged",
        "notify:OnAllianceRelationshipChanged",
    ]);

    // Corporation multicasts (reach P1 and P2, but not P3)
    assert_eq!(assert_multicast(&ctx.notify_corporation_changed(cid, PyRep::Dict(vec![])), "OnCorporationChanged", corp_members, non_corp), "OnCorporationChanged");
    assert_eq!(assert_multicast(&ctx.notify_corporation_member_changed(cid, 90_000_001, 1024), "OnCorporationMemberChanged", corp_members, non_corp), "OnCorporationMemberChanged");
    assert_eq!(assert_multicast(&ctx.notify_corporation_recruitment_ad_changed(cid), "OnCorporationRecruitmentAdChanged", corp_members, non_corp), "OnCorporationRecruitmentAdChanged");
    assert_eq!(assert_multicast(&ctx.notify_corporation_welcome_mail_changed(cid), "OnCorporationWelcomeMailChanged", corp_members, non_corp), "OnCorporationWelcomeMailChanged");
    assert_eq!(assert_multicast(&ctx.notify_corporation_medal_added(cid, 501), "OnCorporationMedalAdded", corp_members, non_corp), "OnCorporationMedalAdded");
    assert_eq!(assert_multicast(&ctx.notify_corp_logo_ready(cid), "OnCorpLogoReady", corp_members, non_corp), "OnCorpLogoReady");
    assert_eq!(assert_multicast(&ctx.notify_corp_aggression_settings_change(cid, PyRep::Dict(vec![])), "OnCorpAggressionSettingsChange", corp_members, non_corp), "OnCorpAggressionSettingsChange");
    assert_eq!(assert_multicast(&ctx.notify_corp_allowed_enlistment_factions_set(cid, PyRep::List(vec![])), "OnCorpAllowedEnlistmentFactionsSet", corp_members, non_corp), "OnCorpAllowedEnlistmentFactionsSet");
    assert_eq!(assert_multicast(&ctx.notify_alliance_application_changed(cid, aid, "Accepted"), "OnAllianceApplicationChanged", corp_members, non_corp), "OnAllianceApplicationChanged");

    // Unicast to P1
    assert_eq!(assert_notif(&ctx.notify_corporation_application_changed(90_000_001, cid, "Pending"), "OnCorporationApplicationChanged", 90_000_001), "OnCorporationApplicationChanged");

    // Alliance multicasts
    assert_eq!(assert_multicast(&ctx.notify_alliance_changed(aid, PyRep::Dict(vec![])), "OnAllianceChanged", corp_members, non_corp), "OnAllianceChanged");
    assert_eq!(assert_multicast(&ctx.notify_alliance_member_changed(aid, cid, "Member"), "OnAllianceMemberChanged", corp_members, non_corp), "OnAllianceMemberChanged");
    assert_eq!(assert_multicast(&ctx.notify_alliance_relationship_changed(aid, 99_000_002, 10.0), "OnAllianceRelationshipChanged", corp_members, non_corp), "OnAllianceRelationshipChanged");
}

#[tokio::test]
async fn test_notifications_contacts_standing_pipelines() {
    // parity: notify:OnContactLoggedOn, notify:OnContactChange, notify:OnContactNoLongerContact, notify:OnPersonalContactsAdded
    // parity: notify:OnPersonalContactsUpdated, notify:OnPersonalContactsDeleted, notify:OnBlockContacts, notify:OnUnblockContact
    // parity: notify:OnUnblockContacts, notify:OnStandingSet, notify:OnStandingsModified, notify:OnSetCorpStanding
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1 = 90_000_001i64;
    let p3 = s3.character_id.unwrap();
    let corp_members = &[90_000_001, 90_000_002];
    let non_corp = &[p3];
    let cid = 98_000_001i64;

    eve_testkit::attest(&[
        "notify:OnContactLoggedOn",
        "notify:OnContactChange",
        "notify:OnContactNoLongerContact",
        "notify:OnPersonalContactsAdded",
        "notify:OnPersonalContactsUpdated",
        "notify:OnPersonalContactsDeleted",
        "notify:OnBlockContacts",
        "notify:OnUnblockContact",
        "notify:OnUnblockContacts",
        "notify:OnStandingSet",
        "notify:OnStandingsModified",
        "notify:OnSetCorpStanding",
    ]);

    assert_eq!(assert_notif(&ctx.notify_contact_logged_on(p1, 90_000_002), "OnContactLoggedOn", p1), "OnContactLoggedOn");
    assert_eq!(assert_notif(&ctx.notify_contact_change(p1, 90_000_002, 5.0), "OnContactChange", p1), "OnContactChange");
    assert_eq!(assert_notif(&ctx.notify_contact_no_longer_contact(p1, 90_000_002), "OnContactNoLongerContact", p1), "OnContactNoLongerContact");
    assert_eq!(assert_notif(&ctx.notify_personal_contacts_added(p1, PyRep::List(vec![])), "OnPersonalContactsAdded", p1), "OnPersonalContactsAdded");
    assert_eq!(assert_notif(&ctx.notify_personal_contacts_updated(p1, PyRep::List(vec![])), "OnPersonalContactsUpdated", p1), "OnPersonalContactsUpdated");
    assert_eq!(assert_notif(&ctx.notify_personal_contacts_deleted(p1, PyRep::List(vec![])), "OnPersonalContactsDeleted", p1), "OnPersonalContactsDeleted");
    assert_eq!(assert_notif(&ctx.notify_block_contacts(p1, PyRep::List(vec![PyRep::Integer(p3)])), "OnBlockContacts", p1), "OnBlockContacts");
    assert_eq!(assert_notif(&ctx.notify_unblock_contact(p1, p3), "OnUnblockContact", p1), "OnUnblockContact");
    assert_eq!(assert_notif(&ctx.notify_unblock_contacts(p1, PyRep::List(vec![PyRep::Integer(p3)])), "OnUnblockContacts", p1), "OnUnblockContacts");
    assert_eq!(assert_notif(&ctx.notify_standing_set(p1, p3, 10.0), "OnStandingSet", p1), "OnStandingSet");
    assert_eq!(assert_notif(&ctx.notify_standings_modified(p1), "OnStandingsModified", p1), "OnStandingsModified");
    assert_eq!(assert_multicast(&ctx.notify_set_corp_standing(cid, p3, 5.0), "OnSetCorpStanding", corp_members, non_corp), "OnSetCorpStanding");
}

#[tokio::test]
async fn test_notifications_trade_bounty_space_pipelines() {
    // parity: notify:OnAccountChange, notify:OnBountyPlaced, notify:OnBountyAddedToPayout, notify:OnTradeInitiate
    // parity: notify:OnTradeOffer, notify:OnTradeMoneyOffer, notify:OnTradeOfferReset, notify:OnTradeComplete
    // parity: notify:OnTradeCancel, notify:OnRaffleCreatedServer, notify:OnRaffleFinishedServer, notify:OnRaffleUpdatedServer
    // parity: notify:OnWarpStarted, notify:OnWarpFinished, notify:OnAutopilotUpdated, notify:OnCharNowInStation
    // parity: notify:OnCharNoLongerInStation, notify:OnCapacitorChange, notify:OnAsteroidTerminated, notify:OnDamageStateChange
    // parity: notify:OnOreMined, notify:OnDroneStateChange, notify:OnDroneControlLost, notify:OnBridgeModeChange
    // parity: notify:OnCrimewatchEngagementUpdated, notify:OnWreckLootAll, notify:DoSimClockRebase, notify:OnSystemCriminalFlagUpdates
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1 = 90_000_001i64;
    let p2 = 90_000_002i64;
    let p3 = s3.character_id.unwrap();
    let sys_members = &[p1, p2];
    let non_sys = &[p3];
    let sys_id = 30_000_142u32;
    let sta_id = 60_003_760i64;

    eve_testkit::attest(&[
        "notify:OnAccountChange",
        "notify:OnBountyPlaced",
        "notify:OnBountyAddedToPayout",
        "notify:OnTradeInitiate",
        "notify:OnTradeOffer",
        "notify:OnTradeMoneyOffer",
        "notify:OnTradeOfferReset",
        "notify:OnTradeComplete",
        "notify:OnTradeCancel",
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleFinishedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnWarpStarted",
        "notify:OnWarpFinished",
        "notify:OnAutopilotUpdated",
        "notify:OnCharNowInStation",
        "notify:OnCharNoLongerInStation",
        "notify:OnCapacitorChange",
        "notify:OnAsteroidTerminated",
        "notify:OnDamageStateChange",
        "notify:OnOreMined",
        "notify:OnDroneStateChange",
        "notify:OnDroneControlLost",
        "notify:OnBridgeModeChange",
        "notify:OnCrimewatchEngagementUpdated",
        "notify:OnWreckLootAll",
        "notify:DoSimClockRebase",
        "notify:OnSystemCriminalFlagUpdates",
    ]);

    // Trade & Bounty
    assert_eq!(assert_notif(&ctx.notify_account_change(p1, "cash", p1, 100_000_000.0), "OnAccountChange", p1), "OnAccountChange");
    assert_eq!(assert_notif(&ctx.notify_bounty_placed(p1, p2, 50_000_000.0), "OnBountyPlaced", p1), "OnBountyPlaced");
    assert_eq!(assert_notif(&ctx.notify_bounty_added_to_payout(p1, 2_500_000.0), "OnBountyAddedToPayout", p1), "OnBountyAddedToPayout");
    assert_eq!(assert_notif(&ctx.notify_trade_initiate(p1, p2), "OnTradeInitiate", p1), "OnTradeInitiate");
    assert_eq!(assert_notif(&ctx.notify_trade_offer(p1, p2, PyRep::List(vec![])), "OnTradeOffer", p1), "OnTradeOffer");
    assert_eq!(assert_notif(&ctx.notify_trade_money_offer(p1, p2, 10_000_000.0), "OnTradeMoneyOffer", p1), "OnTradeMoneyOffer");
    assert_eq!(assert_notif(&ctx.notify_trade_offer_reset(p1, p2), "OnTradeOfferReset", p1), "OnTradeOfferReset");
    assert_eq!(assert_notif(&ctx.notify_trade_complete(p1), "OnTradeComplete", p1), "OnTradeComplete");
    assert_eq!(assert_notif(&ctx.notify_trade_cancel(p1), "OnTradeCancel", p1), "OnTradeCancel");
    assert_eq!(assert_notif(&ctx.notify_raffle_created(p1, 7001), "OnRaffleCreatedServer", p1), "OnRaffleCreatedServer");
    assert_eq!(assert_notif(&ctx.notify_raffle_finished(p1, 7001), "OnRaffleFinishedServer", p1), "OnRaffleFinishedServer");
    assert_eq!(assert_notif(&ctx.notify_raffle_updated(p1, 7001), "OnRaffleUpdatedServer", p1), "OnRaffleUpdatedServer");

    // Space & Navigation
    assert_eq!(assert_notif(&ctx.notify_warp_started(p1), "OnWarpStarted", p1), "OnWarpStarted");
    assert_eq!(assert_notif(&ctx.notify_warp_finished(p1), "OnWarpFinished", p1), "OnWarpFinished");
    assert_eq!(assert_notif(&ctx.notify_autopilot_updated(p1, PyRep::List(vec![])), "OnAutopilotUpdated", p1), "OnAutopilotUpdated");
    assert_eq!(assert_notif(&ctx.notify_capacitor_change(p1, 2500.0), "OnCapacitorChange", p1), "OnCapacitorChange");
    assert_eq!(assert_notif(&ctx.notify_ore_mined(p1, 34, 150.0), "OnOreMined", p1), "OnOreMined");
    assert_eq!(assert_notif(&ctx.notify_drone_state_change(p1, 8001, "Combat"), "OnDroneStateChange", p1), "OnDroneStateChange");
    assert_eq!(assert_notif(&ctx.notify_drone_control_lost(p1, 8001), "OnDroneControlLost", p1), "OnDroneControlLost");
    assert_eq!(assert_notif(&ctx.notify_bridge_mode_change(p1, true), "OnBridgeModeChange", p1), "OnBridgeModeChange");
    assert_eq!(assert_notif(&ctx.notify_crimewatch_engagement_updated(p1, p2, true), "OnCrimewatchEngagementUpdated", p1), "OnCrimewatchEngagementUpdated");
    assert_eq!(assert_notif(&ctx.notify_wreck_loot_all(p1, 9001), "OnWreckLootAll", p1), "OnWreckLootAll");

    // Station and SolarSystem scopes
    assert_eq!(assert_multicast(&ctx.notify_char_now_in_station(sta_id, p1), "OnCharNowInStation", sys_members, non_sys), "OnCharNowInStation");
    assert_eq!(assert_multicast(&ctx.notify_char_no_longer_in_station(sta_id, p1), "OnCharNoLongerInStation", sys_members, non_sys), "OnCharNoLongerInStation");
    assert_eq!(assert_multicast(&ctx.notify_asteroid_terminated(sys_id, 10_001), "OnAsteroidTerminated", sys_members, non_sys), "OnAsteroidTerminated");
    assert_eq!(assert_multicast(&ctx.notify_damage_state_change(NotificationTarget::SolarSystem(sys_id), 100_001, 1.0, 1.0, 1.0), "OnDamageStateChange", sys_members, non_sys), "OnDamageStateChange");
    assert_eq!(assert_multicast(&ctx.notify_sim_clock_rebase(sys_id, 123456789), "DoSimClockRebase", sys_members, non_sys), "DoSimClockRebase");
    assert_eq!(assert_multicast(&ctx.notify_system_criminal_flag_updates(sys_id, &[], &[], &[]), "OnSystemCriminalFlagUpdates", sys_members, non_sys), "OnSystemCriminalFlagUpdates");
}

#[tokio::test]
async fn test_notifications_scanning_probes_pipelines() {
    // parity: notify:OnSystemScanStarted, notify:OnSystemScanDone, notify:OnSystemScanStopped, notify:OnNewProbe
    // parity: notify:OnRemoveProbe, notify:OnProbesIdle, notify:OnDirectionalScanComplete, notify:OnShipScanCompleted
    // parity: notify:OnCargoScanComplete, notify:OnMoonScanComplete, notify:OnNewMoonProbe, notify:OnRemoveMoonProbe
    // parity: notify:OnClientEvent_PerfectScanResultReached, notify:OnSignalTrackerAnomalyUpdate, notify:OnSignalTrackerSignatureUpdate, notify:OnSignalTrackerStructureUpdate
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1 = 90_000_001i64;
    let p2 = 90_000_002i64;
    let p3 = s3.character_id.unwrap();
    let sys_members = &[p1, p2];
    let non_sys = &[p3];
    let sys_id = 30_000_142u32;

    eve_testkit::attest(&[
        "notify:OnSystemScanStarted",
        "notify:OnSystemScanDone",
        "notify:OnSystemScanStopped",
        "notify:OnNewProbe",
        "notify:OnRemoveProbe",
        "notify:OnProbesIdle",
        "notify:OnDirectionalScanComplete",
        "notify:OnShipScanCompleted",
        "notify:OnCargoScanComplete",
        "notify:OnMoonScanComplete",
        "notify:OnNewMoonProbe",
        "notify:OnRemoveMoonProbe",
        "notify:OnClientEvent_PerfectScanResultReached",
        "notify:OnSignalTrackerAnomalyUpdate",
        "notify:OnSignalTrackerSignatureUpdate",
        "notify:OnSignalTrackerStructureUpdate",
    ]);

    assert_eq!(assert_notif(&ctx.notify_system_scan_started(p1), "OnSystemScanStarted", p1), "OnSystemScanStarted");
    assert_eq!(assert_notif(&ctx.notify_system_scan_done(p1, PyRep::List(vec![])), "OnSystemScanDone", p1), "OnSystemScanDone");
    assert_eq!(assert_notif(&ctx.notify_system_scan_stopped(p1), "OnSystemScanStopped", p1), "OnSystemScanStopped");
    assert_eq!(assert_notif(&ctx.notify_new_probe(p1, 501), "OnNewProbe", p1), "OnNewProbe");
    assert_eq!(assert_notif(&ctx.notify_remove_probe(p1, 501), "OnRemoveProbe", p1), "OnRemoveProbe");
    assert_eq!(assert_notif(&ctx.notify_probes_idle(p1), "OnProbesIdle", p1), "OnProbesIdle");
    assert_eq!(assert_notif(&ctx.notify_directional_scan_complete(p1, PyRep::List(vec![])), "OnDirectionalScanComplete", p1), "OnDirectionalScanComplete");
    assert_eq!(assert_notif(&ctx.notify_ship_scan_completed(p1, 601), "OnShipScanCompleted", p1), "OnShipScanCompleted");
    assert_eq!(assert_notif(&ctx.notify_cargo_scan_complete(p1, 601, PyRep::List(vec![])), "OnCargoScanComplete", p1), "OnCargoScanComplete");
    assert_eq!(assert_notif(&ctx.notify_moon_scan_complete(p1, 701, PyRep::List(vec![])), "OnMoonScanComplete", p1), "OnMoonScanComplete");
    assert_eq!(assert_notif(&ctx.notify_new_moon_probe(p1, 801), "OnNewMoonProbe", p1), "OnNewMoonProbe");
    assert_eq!(assert_notif(&ctx.notify_remove_moon_probe(p1, 801), "OnRemoveMoonProbe", p1), "OnRemoveMoonProbe");
    assert_eq!(assert_notif(&ctx.notify_perfect_scan_result_reached(p1, "SIG-123"), "OnClientEvent_PerfectScanResultReached", p1), "OnClientEvent_PerfectScanResultReached");

    // Signal tracker system broadcasts
    assert_eq!(assert_multicast(&ctx.notify_signal_tracker_anomaly_update(sys_id, 1001), "OnSignalTrackerAnomalyUpdate", sys_members, non_sys), "OnSignalTrackerAnomalyUpdate");
    assert_eq!(assert_multicast(&ctx.notify_signal_tracker_signature_update(sys_id, "SIG-456"), "OnSignalTrackerSignatureUpdate", sys_members, non_sys), "OnSignalTrackerSignatureUpdate");
    assert_eq!(assert_multicast(&ctx.notify_signal_tracker_structure_update(sys_id, 2001), "OnSignalTrackerStructureUpdate", sys_members, non_sys), "OnSignalTrackerStructureUpdate");
}

#[tokio::test]
async fn test_notifications_character_sov_structures_pipelines() {
    // parity: notify:OnFreeSkillPointsChanged, notify:OnSkillQueuePausedServer, notify:OnNewSkillQueueSaved, notify:OnMajorPlanetStateUpdate
    // parity: notify:OnBoosterUpdated, notify:OnImplantsChanged, notify:OnSkinLicenseActivated, notify:OnSkinLicenseRemoved
    // parity: notify:OnCurrentShipSkinChange, notify:OnIndustryJob, notify:OnReprocessingMaterialsChanged, notify:OnSovereigntyChanged
    // parity: notify:OnSovHubHacked, notify:OnSkyhookCanBeHacked, notify:OnSolarSystemDevIndexChanged, notify:OnSolarSystemSovStructuresUpdated
    // parity: notify:OnSolarsystemAdvantageStateUpdated_Local, notify:OnSolarsystemInterferenceChanged, notify:OnEnlistmentStateUpdated, notify:OnInsurgencyCampaignStartedForLocation
    // parity: notify:OnInsurgencyCampaignUpdatedForLocation, notify:OnWarChanged, notify:OnStructureServiceChanged, notify:OnBookmarkCreated
    // parity: notify:OnRefreshBookmarks, notify:OnSharedBookmarksFolderUpdated, notify:OnSharedBookmarksFolderDeleted, notify:OnHackingStart
    // parity: notify:OnHackingTurnComplete, notify:OnHackingVirusChanged, notify:OnStanceActive, notify:OnSystemWideEffectAdd
    // parity: notify:OnSystemWideEffectRemove, notify:OnEscalatingPathMessage, notify:OnPetitionCreated, notify:OnWindowOpened
    let ctx = ServerContext::new();
    let (_s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1 = 90_000_001i64;
    let p2 = 90_000_002i64;
    let p3 = s3.character_id.unwrap();
    let sys_members = &[p1, p2];
    let all_members = &[p1, p2, p3];
    let empty_exclude: &[i64] = &[];
    let non_sys = &[p3];
    let sys_id = 30_000_142u32;

    eve_testkit::attest(&[
        "notify:OnFreeSkillPointsChanged",
        "notify:OnSkillQueuePausedServer",
        "notify:OnNewSkillQueueSaved",
        "notify:OnMajorPlanetStateUpdate",
        "notify:OnBoosterUpdated",
        "notify:OnImplantsChanged",
        "notify:OnSkinLicenseActivated",
        "notify:OnSkinLicenseRemoved",
        "notify:OnCurrentShipSkinChange",
        "notify:OnIndustryJob",
        "notify:OnReprocessingMaterialsChanged",
        "notify:OnSovereigntyChanged",
        "notify:OnSovHubHacked",
        "notify:OnSkyhookCanBeHacked",
        "notify:OnSolarSystemDevIndexChanged",
        "notify:OnSolarSystemSovStructuresUpdated",
        "notify:OnSolarsystemAdvantageStateUpdated_Local",
        "notify:OnSolarsystemInterferenceChanged",
        "notify:OnEnlistmentStateUpdated",
        "notify:OnInsurgencyCampaignStartedForLocation",
        "notify:OnInsurgencyCampaignUpdatedForLocation",
        "notify:OnWarChanged",
        "notify:OnStructureServiceChanged",
        "notify:OnBookmarkCreated",
        "notify:OnRefreshBookmarks",
        "notify:OnSharedBookmarksFolderUpdated",
        "notify:OnSharedBookmarksFolderDeleted",
        "notify:OnHackingStart",
        "notify:OnHackingTurnComplete",
        "notify:OnHackingVirusChanged",
        "notify:OnStanceActive",
        "notify:OnSystemWideEffectAdd",
        "notify:OnSystemWideEffectRemove",
        "notify:OnEscalatingPathMessage",
        "notify:OnPetitionCreated",
        "notify:OnWindowOpened",
    ]);

    // Character, Skills & Customization
    assert_eq!(assert_notif(&ctx.notify_free_skill_points_changed(p1, 500_000), "OnFreeSkillPointsChanged", p1), "OnFreeSkillPointsChanged");
    assert_eq!(assert_notif(&ctx.notify_skill_queue_paused_server(p1), "OnSkillQueuePausedServer", p1), "OnSkillQueuePausedServer");
    assert_eq!(assert_notif(&ctx.notify_new_skill_queue_saved(p1, PyRep::List(vec![])), "OnNewSkillQueueSaved", p1), "OnNewSkillQueueSaved");
    assert_eq!(assert_notif(&ctx.notify_major_planet_state_update(p1, 40_000_001, true), "OnMajorPlanetStateUpdate", p1), "OnMajorPlanetStateUpdate");
    assert_eq!(assert_notif(&ctx.notify_booster_updated(p1, 101), "OnBoosterUpdated", p1), "OnBoosterUpdated");
    assert_eq!(assert_notif(&ctx.notify_implants_changed(p1), "OnImplantsChanged", p1), "OnImplantsChanged");
    assert_eq!(assert_notif(&ctx.notify_skin_license_activated(p1, 201), "OnSkinLicenseActivated", p1), "OnSkinLicenseActivated");
    assert_eq!(assert_notif(&ctx.notify_skin_license_removed(p1, 201), "OnSkinLicenseRemoved", p1), "OnSkinLicenseRemoved");
    assert_eq!(assert_notif(&ctx.notify_current_ship_skin_change(p1, 301), "OnCurrentShipSkinChange", p1), "OnCurrentShipSkinChange");
    assert_eq!(assert_notif(&ctx.notify_industry_job(p1, 401, "Delivered"), "OnIndustryJob", p1), "OnIndustryJob");
    assert_eq!(assert_notif(&ctx.notify_reprocessing_materials_changed(p1), "OnReprocessingMaterialsChanged", p1), "OnReprocessingMaterialsChanged");
    assert_eq!(assert_notif(&ctx.notify_enlistment_state_updated(p1, 500_001), "OnEnlistmentStateUpdated", p1), "OnEnlistmentStateUpdated");
    assert_eq!(assert_notif(&ctx.notify_structure_service_changed(p1, 1, true), "OnStructureServiceChanged", p1), "OnStructureServiceChanged");
    assert_eq!(assert_notif(&ctx.notify_bookmark_created(p1, 601), "OnBookmarkCreated", p1), "OnBookmarkCreated");
    assert_eq!(assert_notif(&ctx.notify_refresh_bookmarks(p1), "OnRefreshBookmarks", p1), "OnRefreshBookmarks");
    assert_eq!(assert_notif(&ctx.notify_hacking_start(p1, 701), "OnHackingStart", p1), "OnHackingStart");
    assert_eq!(assert_notif(&ctx.notify_hacking_turn_complete(p1, PyRep::Dict(vec![])), "OnHackingTurnComplete", p1), "OnHackingTurnComplete");
    assert_eq!(assert_notif(&ctx.notify_hacking_virus_changed(p1, PyRep::Dict(vec![])), "OnHackingVirusChanged", p1), "OnHackingVirusChanged");
    assert_eq!(assert_notif(&ctx.notify_stance_active(p1, 801), "OnStanceActive", p1), "OnStanceActive");
    assert_eq!(assert_notif(&ctx.notify_escalating_path_message(p1, "Escalation path discovered"), "OnEscalatingPathMessage", p1), "OnEscalatingPathMessage");
    assert_eq!(assert_notif(&ctx.notify_petition_created(p1, 901), "OnPetitionCreated", p1), "OnPetitionCreated");
    assert_eq!(assert_notif(&ctx.notify_window_opened(p1, "fitting"), "OnWindowOpened", p1), "OnWindowOpened");

    // Sovereignty and System Wide multicasts
    assert_eq!(assert_multicast(&ctx.notify_sovereignty_changed(sys_id, PyRep::Dict(vec![])), "OnSovereigntyChanged", sys_members, non_sys), "OnSovereigntyChanged");
    assert_eq!(assert_multicast(&ctx.notify_sov_hub_hacked(sys_id, 1001), "OnSovHubHacked", sys_members, non_sys), "OnSovHubHacked");
    assert_eq!(assert_multicast(&ctx.notify_skyhook_can_be_hacked(sys_id, 1002), "OnSkyhookCanBeHacked", sys_members, non_sys), "OnSkyhookCanBeHacked");
    assert_eq!(assert_multicast(&ctx.notify_solar_system_dev_index_changed(sys_id, 1, 5.0), "OnSolarSystemDevIndexChanged", sys_members, non_sys), "OnSolarSystemDevIndexChanged");
    assert_eq!(assert_multicast(&ctx.notify_solar_system_sov_structures_updated(sys_id), "OnSolarSystemSovStructuresUpdated", sys_members, non_sys), "OnSolarSystemSovStructuresUpdated");
    assert_eq!(assert_multicast(&ctx.notify_solarsystem_advantage_state_updated(sys_id, 0.75), "OnSolarsystemAdvantageStateUpdated_Local", sys_members, non_sys), "OnSolarsystemAdvantageStateUpdated_Local");
    assert_eq!(assert_multicast(&ctx.notify_solarsystem_interference_changed(sys_id, 0.20), "OnSolarsystemInterferenceChanged", sys_members, non_sys), "OnSolarsystemInterferenceChanged");
    assert_eq!(assert_multicast(&ctx.notify_insurgency_campaign_started(sys_id as i64), "OnInsurgencyCampaignStartedForLocation", sys_members, non_sys), "OnInsurgencyCampaignStartedForLocation");
    assert_eq!(assert_multicast(&ctx.notify_insurgency_campaign_updated(sys_id as i64), "OnInsurgencyCampaignUpdatedForLocation", sys_members, non_sys), "OnInsurgencyCampaignUpdatedForLocation");
    assert_eq!(assert_multicast(&ctx.notify_system_wide_effect_add(sys_id, 2001), "OnSystemWideEffectAdd", sys_members, non_sys), "OnSystemWideEffectAdd");
    assert_eq!(assert_multicast(&ctx.notify_system_wide_effect_remove(sys_id, 2001), "OnSystemWideEffectRemove", sys_members, non_sys), "OnSystemWideEffectRemove");

    // Global / All pilot multicasts
    assert_eq!(assert_multicast(&ctx.notify_war_changed(5001), "OnWarChanged", all_members, empty_exclude), "OnWarChanged");
    assert_eq!(assert_multicast(&ctx.notify_shared_bookmarks_folder_updated(6001), "OnSharedBookmarksFolderUpdated", all_members, empty_exclude), "OnSharedBookmarksFolderUpdated");
    assert_eq!(assert_multicast(&ctx.notify_shared_bookmarks_folder_deleted(6001), "OnSharedBookmarksFolderDeleted", all_members, empty_exclude), "OnSharedBookmarksFolderDeleted");
}
