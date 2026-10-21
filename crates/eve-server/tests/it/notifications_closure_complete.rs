//! Complete parity closure tests for server-emitted Macho notifications.
//!
//! Covers 6 core operational categories:
//! - Ship & Module (targeting, module state, dogma, health)
//! - Session & State (session lifecycle, skill queue, implants, boosters, brain)
//! - Fleet & Social (fleet coordination, standings, timers, contacts)
//! - Market & Wallet (bills, corporate accounts, personal balance, tokens, redeem queue)
//! - Map & Navigation (warp, docking, undocking, autopilot, clone jumps, cloaking)
//! - Industry & Science (jobs, materials, slots, construction progress)
//!
//! Validates event naming, payload framing, and delivery scoping across Character,
//! Corporation, Alliance, Fleet, and SolarSystem targets.


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
    let notif = notifs
        .iter()
        .find(|n| n.event == expected_event)
        .unwrap_or_else(|| panic!("Event {expected_event} not found in pending notifications"));
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
async fn test_ship_module_notifications() {
    // parity: notify:OnTargets, notify:OnSlimItemUpdated, notify:OnDroneStateChange2, notify:OnWeaponsTimerUpdate
    // parity: notify:OnDamageStateChanged, notify:OnModuleAttributeChange, notify:OnModuleActivated, notify:OnModuleRepaired
    // parity: notify:OnModuleUnfitted, notify:OnModuleOnlineChange, notify:OnDogmaItemChange, notify:OnGodmaItemChange
    // parity: notify:OnGodmaPrimeItem, notify:OnGodmaEffectInfo, notify:OnTargetLockStarted, notify:OnFailLockTarget
    // parity: notify:OnTargetOBO, notify:ProcessTargetChanged, notify:OnShipDeath, notify:OnShipInsured
    let ctx = ServerContext::new();
    let (s1, _s2, _s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();

    eve_testkit::attest(&[
        "notify:OnTargets",
        "notify:OnSlimItemUpdated",
        "notify:OnDroneStateChange2",
        "notify:OnWeaponsTimerUpdate",
        "notify:OnDamageStateChanged",
        "notify:OnModuleAttributeChange",
        "notify:OnModuleActivated",
        "notify:OnModuleRepaired",
        "notify:OnModuleUnfitted",
        "notify:OnModuleOnlineChange",
        "notify:OnDogmaItemChange",
        "notify:OnGodmaItemChange",
        "notify:OnGodmaPrimeItem",
        "notify:OnGodmaEffectInfo",
        "notify:OnTargetLockStarted",
        "notify:OnFailLockTarget",
        "notify:OnTargetOBO",
        "notify:ProcessTargetChanged",
        "notify:OnShipDeath",
        "notify:OnShipInsured",
    ]);

    // 1. OnTargets
    let n1 = ctx.notify_targets(p1_id, vec![PyRep::Integer(100_001), PyRep::Integer(100_002)]);
    assert_eq!(assert_notif(&n1, "OnTargets", p1_id), "OnTargets");
    let drained = ctx.drain_notifications_for_char(p1_id);
    assert!(drained.iter().any(|n| n.event == "OnTargets"));

    // 2. OnSlimItemUpdated (SolarSystem multicast to sys 30000142)
    let slim = PyRep::Dict(vec![(PyRep::String("itemID".into()), PyRep::Integer(100_001))]);
    let n2 = ctx.notify_slim_item_updated(30_000_142, 100_001, slim);
    assert_eq!(assert_multicast(&n2, "OnSlimItemUpdated", &[90_000_001, 90_000_002], &[90_000_003]), "OnSlimItemUpdated");

    // 3. OnDroneStateChange2
    let n3 = ctx.notify_drone_state_change2(p1_id, 200_001, 1, 100_001);
    assert_eq!(assert_notif(&n3, "OnDroneStateChange2", p1_id), "OnDroneStateChange2");
    assert_eq!(n3[0].args.len(), 3);

    // 4. OnWeaponsTimerUpdate
    let n4 = ctx.notify_weapons_timer_update(p1_id, 60_000);
    assert_eq!(assert_notif(&n4, "OnWeaponsTimerUpdate", p1_id), "OnWeaponsTimerUpdate");
    assert_eq!(n4[0].args[0], PyRep::Integer(60_000));

    // 5. OnDamageStateChanged (SolarSystem multicast)
    let dmg = PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(1.0), PyRep::Real(1.0)]);
    let n5 = ctx.notify_damage_state_changed(30_000_142, 100_001, dmg);
    assert_eq!(assert_multicast(&n5, "OnDamageStateChanged", &[90_000_001, 90_000_002], &[90_000_003]), "OnDamageStateChanged");

    // 6. OnModuleAttributeChange
    let n6 = ctx.notify_module_attribute_change(p1_id, 400_001, 9, 250.0);
    assert_eq!(assert_notif(&n6, "OnModuleAttributeChange", p1_id), "OnModuleAttributeChange");
    assert_eq!(n6[0].args.len(), 3);

    // 7. OnModuleActivated
    let n7 = ctx.notify_module_activated(p1_id, 400_001, 11, 100_001);
    assert_eq!(assert_notif(&n7, "OnModuleActivated", p1_id), "OnModuleActivated");

    // 8. OnModuleRepaired
    let n8 = ctx.notify_module_repaired(p1_id, 400_001);
    assert_eq!(assert_notif(&n8, "OnModuleRepaired", p1_id), "OnModuleRepaired");

    // 9. OnModuleUnfitted
    let n9 = ctx.notify_module_unfitted(p1_id, 400_001, 500_001);
    assert_eq!(assert_notif(&n9, "OnModuleUnfitted", p1_id), "OnModuleUnfitted");

    // 10. OnModuleOnlineChange
    let n10 = ctx.notify_module_online_change(p1_id, 400_001, true);
    assert_eq!(assert_notif(&n10, "OnModuleOnlineChange", p1_id), "OnModuleOnlineChange");
    assert_eq!(n10[0].args[1], PyRep::Bool(true));

    // 11. OnDogmaItemChange
    let n11 = ctx.notify_dogma_item_change(p1_id, 400_001, 2);
    assert_eq!(assert_notif(&n11, "OnDogmaItemChange", p1_id), "OnDogmaItemChange");

    // 12. OnGodmaItemChange
    let g_data = PyRep::Dict(vec![(PyRep::String("typeID".into()), PyRep::Integer(587))]);
    let n12 = ctx.notify_godma_item_change(p1_id, 400_001, g_data);
    assert_eq!(assert_notif(&n12, "OnGodmaItemChange", p1_id), "OnGodmaItemChange");

    // 13. OnGodmaPrimeItem
    let n13 = ctx.notify_godma_prime_item(p1_id, 400_001);
    assert_eq!(assert_notif(&n13, "OnGodmaPrimeItem", p1_id), "OnGodmaPrimeItem");

    // 14. OnGodmaEffectInfo
    let eff_info = PyRep::Dict(vec![(PyRep::String("duration".into()), PyRep::Integer(5000))]);
    let n14 = ctx.notify_godma_effect_info(p1_id, 11, eff_info);
    assert_eq!(assert_notif(&n14, "OnGodmaEffectInfo", p1_id), "OnGodmaEffectInfo");

    // 15. OnTargetLockStarted
    let n15 = ctx.notify_target_lock_started(p1_id, 100_001, 3500);
    assert_eq!(assert_notif(&n15, "OnTargetLockStarted", p1_id), "OnTargetLockStarted");

    // 16. OnFailLockTarget
    let n16 = ctx.notify_fail_lock_target(p1_id, 100_001, "TargetOutOfRange");
    assert_eq!(assert_notif(&n16, "OnFailLockTarget", p1_id), "OnFailLockTarget");

    // 17. OnTargetOBO
    let obo = PyRep::Tuple(vec![PyRep::Integer(100_001), PyRep::Integer(1)]);
    let n17 = ctx.notify_target_obo(p1_id, 100_001, obo);
    assert_eq!(assert_notif(&n17, "OnTargetOBO", p1_id), "OnTargetOBO");

    // 18. ProcessTargetChanged
    let n18 = ctx.notify_process_target_changed(p1_id, 100_001, 1);
    assert_eq!(assert_notif(&n18, "ProcessTargetChanged", p1_id), "ProcessTargetChanged");

    // 19. OnShipDeath
    let n19 = ctx.notify_ship_death(p1_id, 500_001);
    assert_eq!(assert_notif(&n19, "OnShipDeath", p1_id), "OnShipDeath");

    // 20. OnShipInsured
    let n20 = ctx.notify_ship_insured(p1_id, 500_001, 1_500_000.0);
    assert_eq!(assert_notif(&n20, "OnShipInsured", p1_id), "OnShipInsured");
    assert_eq!(n20[0].args[1], PyRep::Real(1_500_000.0));
}

#[tokio::test]
async fn test_session_state_notifications() {
    // parity: notify:DoSessionChanging, notify:OnCharacterSessionChanged, notify:OnCurrSessionSpaceEnteredFirstTime, notify:OnSessionEnd
    // parity: notify:OnSessionMutated, notify:OnSessionReset, notify:ProcessSessionChange, notify:ProcessSessionReset
    // parity: notify:OnTimeReset, notify:OnServerSkillsChanged, notify:OnServerSkillsRemoved, notify:OnServerImplantsChanged
    // parity: notify:OnServerBoostersChanged, notify:OnServerBrainUpdated, notify:OnSkillsChanged, notify:OnSkillQueueChanged
    // parity: notify:OnSkillQueueRefreshed
    let ctx = ServerContext::new();
    let (s1, _s2, _s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();

    eve_testkit::attest(&[
        "notify:DoSessionChanging",
        "notify:OnCharacterSessionChanged",
        "notify:OnCurrSessionSpaceEnteredFirstTime",
        "notify:OnSessionEnd",
        "notify:OnSessionMutated",
        "notify:OnSessionReset",
        "notify:ProcessSessionChange",
        "notify:ProcessSessionReset",
        "notify:OnTimeReset",
        "notify:OnServerSkillsChanged",
        "notify:OnServerSkillsRemoved",
        "notify:OnServerImplantsChanged",
        "notify:OnServerBoostersChanged",
        "notify:OnServerBrainUpdated",
        "notify:OnSkillsChanged",
        "notify:OnSkillQueueChanged",
        "notify:OnSkillQueueRefreshed",
    ]);

    // 1. DoSessionChanging
    let n1 = ctx.notify_do_session_changing(p1_id, 301, "undocking");
    assert_eq!(assert_notif(&n1, "DoSessionChanging", p1_id), "DoSessionChanging");

    // 2. OnCharacterSessionChanged
    let sess_data = PyRep::Dict(vec![(PyRep::String("charid".into()), PyRep::Integer(p1_id))]);
    let n2 = ctx.notify_character_session_changed(p1_id, sess_data);
    assert_eq!(assert_notif(&n2, "OnCharacterSessionChanged", p1_id), "OnCharacterSessionChanged");

    // 3. OnCurrSessionSpaceEnteredFirstTime
    let n3 = ctx.notify_curr_session_space_entered_first_time(p1_id, 30_000_142);
    assert_eq!(assert_notif(&n3, "OnCurrSessionSpaceEnteredFirstTime", p1_id), "OnCurrSessionSpaceEnteredFirstTime");

    // 4. OnSessionEnd
    let n4 = ctx.notify_session_end(p1_id, 301);
    assert_eq!(assert_notif(&n4, "OnSessionEnd", p1_id), "OnSessionEnd");

    // 5. OnSessionMutated
    let mut_attrs = PyRep::Dict(vec![(PyRep::String("solarsystemid".into()), PyRep::Integer(30_000_142))]);
    let n5 = ctx.notify_session_mutated(p1_id, 301, mut_attrs);
    assert_eq!(assert_notif(&n5, "OnSessionMutated", p1_id), "OnSessionMutated");

    // 6. OnSessionReset
    let n6 = ctx.notify_session_reset(p1_id, 301);
    assert_eq!(assert_notif(&n6, "OnSessionReset", p1_id), "OnSessionReset");

    // 7. ProcessSessionChange
    let p_change = PyRep::Dict(vec![(PyRep::String("stationid".into()), PyRep::None)]);
    let n7 = ctx.notify_process_session_change(p1_id, p_change);
    assert_eq!(assert_notif(&n7, "ProcessSessionChange", p1_id), "ProcessSessionChange");

    // 8. ProcessSessionReset
    let n8 = ctx.notify_process_session_reset(p1_id, 301);
    assert_eq!(assert_notif(&n8, "ProcessSessionReset", p1_id), "ProcessSessionReset");

    // 9. OnTimeReset
    let n9 = ctx.notify_time_reset(p1_id, 133_000_000);
    assert_eq!(assert_notif(&n9, "OnTimeReset", p1_id), "OnTimeReset");

    // 10. OnServerSkillsChanged
    let skills = PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Integer(5))]);
    let n10 = ctx.notify_server_skills_changed(p1_id, skills);
    assert_eq!(assert_notif(&n10, "OnServerSkillsChanged", p1_id), "OnServerSkillsChanged");

    // 11. OnServerSkillsRemoved
    let rm_skills = PyRep::List(vec![PyRep::Integer(3300)]);
    let n11 = ctx.notify_server_skills_removed(p1_id, rm_skills);
    assert_eq!(assert_notif(&n11, "OnServerSkillsRemoved", p1_id), "OnServerSkillsRemoved");

    // 12. OnServerImplantsChanged
    let implants = PyRep::List(vec![PyRep::Integer(101)]);
    let n12 = ctx.notify_server_implants_changed(p1_id, implants);
    assert_eq!(assert_notif(&n12, "OnServerImplantsChanged", p1_id), "OnServerImplantsChanged");

    // 13. OnServerBoostersChanged
    let boosters = PyRep::List(vec![PyRep::Integer(201)]);
    let n13 = ctx.notify_server_boosters_changed(p1_id, boosters);
    assert_eq!(assert_notif(&n13, "OnServerBoostersChanged", p1_id), "OnServerBoostersChanged");

    // 14. OnServerBrainUpdated
    let brain = PyRep::Dict(vec![(PyRep::String("cloneTypeID".into()), PyRep::Integer(164))]);
    let n14 = ctx.notify_server_brain_updated(p1_id, brain);
    assert_eq!(assert_notif(&n14, "OnServerBrainUpdated", p1_id), "OnServerBrainUpdated");

    // 15. OnSkillsChanged
    let diff = PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Integer(250_000))]);
    let n15 = ctx.notify_skills_changed(p1_id, diff);
    assert_eq!(assert_notif(&n15, "OnSkillsChanged", p1_id), "OnSkillsChanged");

    // 16. OnSkillQueueChanged
    let q_data = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(5)])]);
    let n16 = ctx.notify_skill_queue_changed_with_data(p1_id, q_data);
    assert_eq!(assert_notif(&n16, "OnSkillQueueChanged", p1_id), "OnSkillQueueChanged");

    // 17. OnSkillQueueRefreshed
    let n17 = ctx.notify_skill_queue_refreshed(p1_id);
    assert_eq!(assert_notif(&n17, "OnSkillQueueRefreshed", p1_id), "OnSkillQueueRefreshed");
}

#[tokio::test]
async fn test_fleet_social_notifications() {
    // parity: notify:OnContactLoggedOff, notify:OnSetPlayerStanding, notify:OnSetAllianceStanding, notify:OnTitleChanged
    // parity: notify:OnFleetBoosted, notify:OnFleetLootEvent, notify:OnFleetMemberChanging, notify:OnFleetFormationWndChanged
    // parity: notify:OnFleetFavoriteAdded, notify:OnFleetFavoriteRemoved, notify:OnNPCStandingChange, notify:OnCombatTimersUpdated
    // parity: notify:OnCriminalTimerUpdate, notify:OnDisapprovalTimerUpdate, notify:OnNpcTimerUpdate, notify:OnPvpTimerUpdate
    // parity: notify:OnClientContactChange, notify:OnOrganizationContactsUpdated
    let ctx = ServerContext::new();
    let (s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();
    let p3_id = s3.character_id.unwrap();

    // Set up Fleet and Alliance fixtures
    let fid = 880_001i64;
    let ally_id = 99_000_001i64;
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
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(ally_id);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.alliance_id = Some(ally_id);
        }
    });

    eve_testkit::attest(&[
        "notify:OnContactLoggedOff",
        "notify:OnSetPlayerStanding",
        "notify:OnSetAllianceStanding",
        "notify:OnTitleChanged",
        "notify:OnFleetBoosted",
        "notify:OnFleetLootEvent",
        "notify:OnFleetMemberChanging",
        "notify:OnFleetFormationWndChanged",
        "notify:OnFleetFavoriteAdded",
        "notify:OnFleetFavoriteRemoved",
        "notify:OnNPCStandingChange",
        "notify:OnCombatTimersUpdated",
        "notify:OnCriminalTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnNpcTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnClientContactChange",
        "notify:OnOrganizationContactsUpdated",
    ]);

    // 1. OnContactLoggedOff (unicast)
    let n1 = ctx.notify_contact_logged_off(p1_id, 90_000_002);
    assert_eq!(assert_notif(&n1, "OnContactLoggedOff", p1_id), "OnContactLoggedOff");

    // 2. OnSetPlayerStanding (unicast)
    let n2 = ctx.notify_set_player_standing(p1_id, 90_000_002, 10.0);
    assert_eq!(assert_notif(&n2, "OnSetPlayerStanding", p1_id), "OnSetPlayerStanding");

    // 3. OnSetAllianceStanding (Alliance multicast: pilots 1 & 2 receive, pilot 3 does not)
    let n3 = ctx.notify_set_alliance_standing(ally_id, 99_000_002, 5.0);
    assert_eq!(assert_multicast(&n3, "OnSetAllianceStanding", &[90_000_001, 90_000_002], &[p3_id]), "OnSetAllianceStanding");

    // 4. OnTitleChanged (unicast)
    let n4 = ctx.notify_title_changed(p1_id, 1);
    assert_eq!(assert_notif(&n4, "OnTitleChanged", p1_id), "OnTitleChanged");

    // 5. OnFleetBoosted (Fleet multicast: fleet members receive, pilot 3 does not)
    let n5 = ctx.notify_fleet_boosted(fid, 90_000_001, 2);
    assert_eq!(assert_multicast(&n5, "OnFleetBoosted", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetBoosted");

    // 6. OnFleetLootEvent (Fleet multicast)
    let n6 = ctx.notify_fleet_loot_event(fid, 90_000_001, 34, 100);
    assert_eq!(assert_multicast(&n6, "OnFleetLootEvent", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetLootEvent");

    // 7. OnFleetMemberChanging (Fleet multicast)
    let n7 = ctx.notify_fleet_member_changing(fid, 90_000_002, 1);
    assert_eq!(assert_multicast(&n7, "OnFleetMemberChanging", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetMemberChanging");

    // 8. OnFleetFormationWndChanged (Fleet multicast)
    let n8 = ctx.notify_fleet_formation_wnd_changed(fid, 3);
    assert_eq!(assert_multicast(&n8, "OnFleetFormationWndChanged", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetFormationWndChanged");

    // 9. OnFleetFavoriteAdded (Fleet multicast)
    let n9 = ctx.notify_fleet_favorite_added(fid, 90_000_002);
    assert_eq!(assert_multicast(&n9, "OnFleetFavoriteAdded", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetFavoriteAdded");

    // 10. OnFleetFavoriteRemoved (Fleet multicast)
    let n10 = ctx.notify_fleet_favorite_removed(fid, 90_000_002);
    assert_eq!(assert_multicast(&n10, "OnFleetFavoriteRemoved", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetFavoriteRemoved");

    // 11. OnNPCStandingChange (unicast)
    let n11 = ctx.notify_npc_standing_change(p1_id, 500_001, 8.5);
    assert_eq!(assert_notif(&n11, "OnNPCStandingChange", p1_id), "OnNPCStandingChange");

    // 12. OnCombatTimersUpdated (unicast)
    let timers = PyRep::Dict(vec![(PyRep::String("weaponsTimer".into()), PyRep::Integer(60_000))]);
    let n12 = ctx.notify_combat_timers_updated(p1_id, timers);
    assert_eq!(assert_notif(&n12, "OnCombatTimersUpdated", p1_id), "OnCombatTimersUpdated");

    // 13. OnCriminalTimerUpdate (unicast)
    let n13 = ctx.notify_criminal_timer_update(p1_id, 900_000);
    assert_eq!(assert_notif(&n13, "OnCriminalTimerUpdate", p1_id), "OnCriminalTimerUpdate");

    // 14. OnDisapprovalTimerUpdate (unicast)
    let n14 = ctx.notify_disapproval_timer_update(p1_id, 300_000);
    assert_eq!(assert_notif(&n14, "OnDisapprovalTimerUpdate", p1_id), "OnDisapprovalTimerUpdate");

    // 15. OnNpcTimerUpdate (unicast)
    let n15 = ctx.notify_npc_timer_update(p1_id, 30_000);
    assert_eq!(assert_notif(&n15, "OnNpcTimerUpdate", p1_id), "OnNpcTimerUpdate");

    // 16. OnPvpTimerUpdate (unicast)
    let n16 = ctx.notify_pvp_timer_update(p1_id, 600_000);
    assert_eq!(assert_notif(&n16, "OnPvpTimerUpdate", p1_id), "OnPvpTimerUpdate");

    // 17. OnClientContactChange (unicast)
    let n17 = ctx.notify_client_contact_change(p1_id, 90_000_002, 1);
    assert_eq!(assert_notif(&n17, "OnClientContactChange", p1_id), "OnClientContactChange");

    // 18. OnOrganizationContactsUpdated (Corporation multicast: corp 98000001)
    let c_list = PyRep::List(vec![PyRep::Integer(90_000_002)]);
    let n18 = ctx.notify_organization_contacts_updated(98_000_001, c_list);
    assert_eq!(assert_multicast(&n18, "OnOrganizationContactsUpdated", &[90_000_001, 90_000_002], &[p3_id]), "OnOrganizationContactsUpdated");
}

#[tokio::test]
async fn test_market_wallet_notifications() {
    // parity: notify:OnBillReceived, notify:OnCorpWalletBillsChanged, notify:OnCorpAccountChangedClient, notify:OnPersonalAccountChangedClient
    // parity: notify:OnTokensRedeemed, notify:OnRedeemingTokensUpdated, notify:OnCharacterLpUpdatedInWallet, notify:OnRedeemingQueueUpdated
    // parity: notify:OnRedeemWndOpened
    let ctx = ServerContext::new();
    let (s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();
    let p3_id = s3.character_id.unwrap();

    eve_testkit::attest(&[
        "notify:OnBillReceived",
        "notify:OnCorpWalletBillsChanged",
        "notify:OnCorpAccountChangedClient",
        "notify:OnPersonalAccountChangedClient",
        "notify:OnTokensRedeemed",
        "notify:OnRedeemingTokensUpdated",
        "notify:OnCharacterLpUpdatedInWallet",
        "notify:OnRedeemingQueueUpdated",
        "notify:OnRedeemWndOpened",
    ]);

    // 1. OnBillReceived (unicast to P1)
    let n1 = ctx.notify_bill_received(p1_id, 600_001, 50_000.0);
    assert_eq!(assert_notif(&n1, "OnBillReceived", p1_id), "OnBillReceived");
    assert_eq!(n1[0].args[1], PyRep::Real(50_000.0));

    // 2. OnCorpWalletBillsChanged (Corporation multicast: corp 98000001)
    let n2 = ctx.notify_corp_wallet_bills_changed(98_000_001, 600_001);
    assert_eq!(assert_multicast(&n2, "OnCorpWalletBillsChanged", &[90_000_001, 90_000_002], &[p3_id]), "OnCorpWalletBillsChanged");

    // 3. OnCorpAccountChangedClient (Corporation multicast)
    let n3 = ctx.notify_corp_account_changed_client(98_000_001, 1000, 10_000_000.0);
    assert_eq!(assert_multicast(&n3, "OnCorpAccountChangedClient", &[90_000_001, 90_000_002], &[p3_id]), "OnCorpAccountChangedClient");

    // 4. OnPersonalAccountChangedClient (unicast)
    let n4 = ctx.notify_personal_account_changed_client(p1_id, 5_000_000.0);
    assert_eq!(assert_notif(&n4, "OnPersonalAccountChangedClient", p1_id), "OnPersonalAccountChangedClient");

    // 5. OnTokensRedeemed (unicast)
    let t_ids = PyRep::List(vec![PyRep::Integer(700_001)]);
    let n5 = ctx.notify_tokens_redeemed(p1_id, t_ids);
    assert_eq!(assert_notif(&n5, "OnTokensRedeemed", p1_id), "OnTokensRedeemed");

    // 6. OnRedeemingTokensUpdated (unicast)
    let toks = PyRep::List(vec![PyRep::Integer(700_001), PyRep::Integer(700_002)]);
    let n6 = ctx.notify_redeeming_tokens_updated(p1_id, toks);
    assert_eq!(assert_notif(&n6, "OnRedeemingTokensUpdated", p1_id), "OnRedeemingTokensUpdated");

    // 7. OnCharacterLpUpdatedInWallet (unicast)
    let n7 = ctx.notify_character_lp_updated_in_wallet(p1_id, 1000125, 4500);
    assert_eq!(assert_notif(&n7, "OnCharacterLpUpdatedInWallet", p1_id), "OnCharacterLpUpdatedInWallet");

    // 8. OnRedeemingQueueUpdated (unicast)
    let q_items = PyRep::List(vec![PyRep::Dict(vec![(PyRep::String("typeID".into()), PyRep::Integer(34))])]);
    let n8 = ctx.notify_redeeming_queue_updated(p1_id, q_items);
    assert_eq!(assert_notif(&n8, "OnRedeemingQueueUpdated", p1_id), "OnRedeemingQueueUpdated");

    // 9. OnRedeemWndOpened (unicast)
    let n9 = ctx.notify_redeem_wnd_opened(p1_id);
    assert_eq!(assert_notif(&n9, "OnRedeemWndOpened", p1_id), "OnRedeemWndOpened");
}

#[tokio::test]
async fn test_map_navigation_notifications() {
    // parity: notify:OnMapModeChangeDone, notify:OnWarpDecelerate, notify:OnWarpArrived, notify:OnWarpActive
    // parity: notify:OnWarpStarted2, notify:OnTravelStateChanged, notify:OnUndockingStarted, notify:OnUndockingCompleted
    // parity: notify:OnUndockingAborted, notify:OnDockingAccepted, notify:OnDockingFinished, notify:OnDockingProgressChanged
    // parity: notify:OnDockingRequestWasDenied, notify:OnJumpCloakCancelled, notify:OnJumpCloakUpdated, notify:OnAutoPilotJump
    // parity: notify:OnCloneJumpUpdate, notify:OnShipCloneJumpUpdate
    let ctx = ServerContext::new();
    let (s1, _s2, _s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();

    eve_testkit::attest(&[
        "notify:OnMapModeChangeDone",
        "notify:OnWarpDecelerate",
        "notify:OnWarpArrived",
        "notify:OnWarpActive",
        "notify:OnWarpStarted2",
        "notify:OnTravelStateChanged",
        "notify:OnUndockingStarted",
        "notify:OnUndockingCompleted",
        "notify:OnUndockingAborted",
        "notify:OnDockingAccepted",
        "notify:OnDockingFinished",
        "notify:OnDockingProgressChanged",
        "notify:OnDockingRequestWasDenied",
        "notify:OnJumpCloakCancelled",
        "notify:OnJumpCloakUpdated",
        "notify:OnAutoPilotJump",
        "notify:OnCloneJumpUpdate",
        "notify:OnShipCloneJumpUpdate",
    ]);

    // 1. OnMapModeChangeDone
    let n1 = ctx.notify_map_mode_change_done(p1_id, 2);
    assert_eq!(assert_notif(&n1, "OnMapModeChangeDone", p1_id), "OnMapModeChangeDone");

    // 2. OnWarpDecelerate
    let n2 = ctx.notify_warp_decelerate(p1_id, 500_001);
    assert_eq!(assert_notif(&n2, "OnWarpDecelerate", p1_id), "OnWarpDecelerate");

    // 3. OnWarpArrived
    let n3 = ctx.notify_warp_arrived(p1_id, 500_001);
    assert_eq!(assert_notif(&n3, "OnWarpArrived", p1_id), "OnWarpArrived");

    // 4. OnWarpActive
    let n4 = ctx.notify_warp_active(p1_id, 500_001, true);
    assert_eq!(assert_notif(&n4, "OnWarpActive", p1_id), "OnWarpActive");

    // 5. OnWarpStarted2
    let n5 = ctx.notify_warp_started2(p1_id, 500_001, 30_000_142);
    assert_eq!(assert_notif(&n5, "OnWarpStarted2", p1_id), "OnWarpStarted2");

    // 6. OnTravelStateChanged
    let n6 = ctx.notify_travel_state_changed(p1_id, 1);
    assert_eq!(assert_notif(&n6, "OnTravelStateChanged", p1_id), "OnTravelStateChanged");

    // 7. OnUndockingStarted
    let n7 = ctx.notify_undocking_started(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n7, "OnUndockingStarted", p1_id), "OnUndockingStarted");

    // 8. OnUndockingCompleted
    let n8 = ctx.notify_undocking_completed(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n8, "OnUndockingCompleted", p1_id), "OnUndockingCompleted");

    // 9. OnUndockingAborted
    let n9 = ctx.notify_undocking_aborted(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n9, "OnUndockingAborted", p1_id), "OnUndockingAborted");

    // 10. OnDockingAccepted
    let n10 = ctx.notify_docking_accepted(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n10, "OnDockingAccepted", p1_id), "OnDockingAccepted");

    // 11. OnDockingFinished
    let n11 = ctx.notify_docking_finished(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n11, "OnDockingFinished", p1_id), "OnDockingFinished");

    // 12. OnDockingProgressChanged
    let n12 = ctx.notify_docking_progress_changed(p1_id, 0.75);
    assert_eq!(assert_notif(&n12, "OnDockingProgressChanged", p1_id), "OnDockingProgressChanged");

    // 13. OnDockingRequestWasDenied
    let n13 = ctx.notify_docking_request_was_denied(p1_id, 60_003_760, "AccessDenied");
    assert_eq!(assert_notif(&n13, "OnDockingRequestWasDenied", p1_id), "OnDockingRequestWasDenied");

    // 14. OnJumpCloakCancelled
    let n14 = ctx.notify_jump_cloak_cancelled(p1_id);
    assert_eq!(assert_notif(&n14, "OnJumpCloakCancelled", p1_id), "OnJumpCloakCancelled");

    // 15. OnJumpCloakUpdated
    let n15 = ctx.notify_jump_cloak_updated(p1_id, 30_000);
    assert_eq!(assert_notif(&n15, "OnJumpCloakUpdated", p1_id), "OnJumpCloakUpdated");

    // 16. OnAutoPilotJump
    let n16 = ctx.notify_autopilot_jump(p1_id, 30_000_144);
    assert_eq!(assert_notif(&n16, "OnAutoPilotJump", p1_id), "OnAutoPilotJump");

    // 17. OnCloneJumpUpdate
    let n17 = ctx.notify_clone_jump_update(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n17, "OnCloneJumpUpdate", p1_id), "OnCloneJumpUpdate");

    // 18. OnShipCloneJumpUpdate
    let n18 = ctx.notify_ship_clone_jump_update(p1_id, 500_001);
    assert_eq!(assert_notif(&n18, "OnShipCloneJumpUpdate", p1_id), "OnShipCloneJumpUpdate");
}

#[tokio::test]
async fn test_industry_science_notifications() {
    // parity: notify:OnIndustryJobClient, notify:OnIndustryMaterials, notify:OnIndustrySlotsUpdated, notify:OnUnderConstructionUpdated
    // parity: notify:OnSelectedIndustryJobChanged, notify:OnIndustryMaterialValueSettingChanged
    let ctx = ServerContext::new();
    let (s1, _s2, _s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();

    eve_testkit::attest(&[
        "notify:OnIndustryJobClient",
        "notify:OnIndustryMaterials",
        "notify:OnIndustrySlotsUpdated",
        "notify:OnUnderConstructionUpdated",
        "notify:OnSelectedIndustryJobChanged",
        "notify:OnIndustryMaterialValueSettingChanged",
    ]);

    // 1. OnIndustryJobClient
    let n1 = ctx.notify_industry_job_client(p1_id, 900_001, "delivered");
    assert_eq!(assert_notif(&n1, "OnIndustryJobClient", p1_id), "OnIndustryJobClient");

    // 2. OnIndustryMaterials
    let mats = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(100)])]);
    let n2 = ctx.notify_industry_materials(p1_id, 900_001, mats);
    assert_eq!(assert_notif(&n2, "OnIndustryMaterials", p1_id), "OnIndustryMaterials");

    // 3. OnIndustrySlotsUpdated
    let n3 = ctx.notify_industry_slots_updated(p1_id, 60_003_760);
    assert_eq!(assert_notif(&n3, "OnIndustrySlotsUpdated", p1_id), "OnIndustrySlotsUpdated");

    // 4. OnUnderConstructionUpdated
    let n4 = ctx.notify_under_construction_updated(p1_id, 100_001, 0.45);
    assert_eq!(assert_notif(&n4, "OnUnderConstructionUpdated", p1_id), "OnUnderConstructionUpdated");

    // 5. OnSelectedIndustryJobChanged
    let n5 = ctx.notify_selected_industry_job_changed(p1_id, 900_001);
    assert_eq!(assert_notif(&n5, "OnSelectedIndustryJobChanged", p1_id), "OnSelectedIndustryJobChanged");

    // 6. OnIndustryMaterialValueSettingChanged
    let n6 = ctx.notify_industry_material_value_setting_changed(p1_id, 1.05);
    assert_eq!(assert_notif(&n6, "OnIndustryMaterialValueSettingChanged", p1_id), "OnIndustryMaterialValueSettingChanged");
}

#[tokio::test]
async fn test_extended_server_notifications() {
    // parity: notify:OnAddExpiryDurationForVoidSpaceContent, notify:OnVoidSpaceRelog, notify:OnServerMessage, notify:OnSignalTrackerFullState
    // parity: notify:OnChargeBeingLoadedToModule, notify:OnIncomingAgentMessage, notify:OnItemsChanged, notify:OnLPChange
    // parity: notify:OnOfficeRentalChanged, notify:OnProbeStateChanged, notify:OnProbeWarpStart, notify:OnProbeWarpEnd
    // parity: notify:OnReloadCalendar, notify:OnRemoteMessage, notify:OnScannerInfoRemoved, notify:OnShareChange
    // parity: notify:OnWeaponBanksChanged, notify:OnWeaponGroupDestroyed, notify:ProcessSovStatusChanged, notify:OnFleetMove
    // parity: notify:OnEffectMessage, notify:OnGodmaFlushLocationProfile, notify:OnDungeonEdit, notify:OnCfgDataChanged
    // parity: notify:OnClusterShutdownInitiated, notify:OnClusterShutdownCancelled, notify:OnUpdateServiceMapping, notify:OnAgentMissionChange
    let ctx = ServerContext::new();
    let (s1, _s2, s3) = setup_test_pilots(&ctx).await;
    let p1_id = s1.character_id.unwrap();
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
        "notify:OnAddExpiryDurationForVoidSpaceContent",
        "notify:OnVoidSpaceRelog",
        "notify:OnServerMessage",
        "notify:OnSignalTrackerFullState",
        "notify:OnChargeBeingLoadedToModule",
        "notify:OnIncomingAgentMessage",
        "notify:OnItemsChanged",
        "notify:OnLPChange",
        "notify:OnOfficeRentalChanged",
        "notify:OnProbeStateChanged",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnReloadCalendar",
        "notify:OnRemoteMessage",
        "notify:OnScannerInfoRemoved",
        "notify:OnShareChange",
        "notify:OnWeaponBanksChanged",
        "notify:OnWeaponGroupDestroyed",
        "notify:ProcessSovStatusChanged",
        "notify:OnFleetMove",
        "notify:OnEffectMessage",
        "notify:OnGodmaFlushLocationProfile",
        "notify:OnDungeonEdit",
        "notify:OnCfgDataChanged",
        "notify:OnClusterShutdownInitiated",
        "notify:OnClusterShutdownCancelled",
        "notify:OnUpdateServiceMapping",
        "notify:OnAgentMissionChange",
    ]);

    // 1. OnAddExpiryDurationForVoidSpaceContent
    let n1 = ctx.notify_void_space_expiry(p1_id, 900);
    assert_eq!(assert_notif(&n1, "OnAddExpiryDurationForVoidSpaceContent", p1_id), "OnAddExpiryDurationForVoidSpaceContent");

    // 2. OnVoidSpaceRelog
    let n2 = ctx.notify_void_space_relog(p1_id, 30_000_142);
    assert_eq!(assert_notif(&n2, "OnVoidSpaceRelog", p1_id), "OnVoidSpaceRelog");

    // 3. OnServerMessage
    let n3 = ctx.notify_server_message(p1_id, "ServerRestartImminent");
    assert_eq!(assert_notif(&n3, "OnServerMessage", p1_id), "OnServerMessage");

    // 4. OnSignalTrackerFullState
    let tracker = PyRep::Dict(vec![(PyRep::String("signatures".into()), PyRep::List(vec![]))]);
    let n4 = ctx.notify_signal_tracker_full_state(p1_id, tracker);
    assert_eq!(assert_notif(&n4, "OnSignalTrackerFullState", p1_id), "OnSignalTrackerFullState");

    // 5. OnChargeBeingLoadedToModule
    let n5 = ctx.notify_charge_being_loaded_to_module(p1_id, 400_001, 200);
    assert_eq!(assert_notif(&n5, "OnChargeBeingLoadedToModule", p1_id), "OnChargeBeingLoadedToModule");

    // 6. OnIncomingAgentMessage
    let n6 = ctx.notify_incoming_agent_message(p1_id, 3000001, "MissionUpdate");
    assert_eq!(assert_notif(&n6, "OnIncomingAgentMessage", p1_id), "OnIncomingAgentMessage");

    // 7. OnItemsChanged
    let items = PyRep::List(vec![PyRep::Integer(100_001)]);
    let n7 = ctx.notify_items_changed(p1_id, items);
    assert_eq!(assert_notif(&n7, "OnItemsChanged", p1_id), "OnItemsChanged");

    // 8. OnLPChange
    let n8 = ctx.notify_lp_change(p1_id, 1000125, 250);
    assert_eq!(assert_notif(&n8, "OnLPChange", p1_id), "OnLPChange");

    // 9. OnOfficeRentalChanged (Corporation multicast)
    let n9 = ctx.notify_office_rental_changed(98_000_001, 60_003_760);
    assert_eq!(assert_multicast(&n9, "OnOfficeRentalChanged", &[90_000_001, 90_000_002], &[p3_id]), "OnOfficeRentalChanged");

    // 10. OnProbeStateChanged
    let n10 = ctx.notify_probe_state_changed(p1_id, 800_001, 2);
    assert_eq!(assert_notif(&n10, "OnProbeStateChanged", p1_id), "OnProbeStateChanged");

    // 11. OnProbeWarpStart
    let n11 = ctx.notify_probe_warp_start(p1_id, 800_001);
    assert_eq!(assert_notif(&n11, "OnProbeWarpStart", p1_id), "OnProbeWarpStart");

    // 12. OnProbeWarpEnd
    let n12 = ctx.notify_probe_warp_end(p1_id, 800_001);
    assert_eq!(assert_notif(&n12, "OnProbeWarpEnd", p1_id), "OnProbeWarpEnd");

    // 13. OnReloadCalendar
    let n13 = ctx.notify_reload_calendar(p1_id);
    assert_eq!(assert_notif(&n13, "OnReloadCalendar", p1_id), "OnReloadCalendar");

    // 14. OnRemoteMessage
    let n14 = ctx.notify_remote_message(NotificationTarget::Character(p1_id), "CustomMessage", vec![]);
    assert_eq!(assert_notif(&n14, "OnRemoteMessage", p1_id), "OnRemoteMessage");

    // 15. OnScannerInfoRemoved
    let n15 = ctx.notify_scanner_info_removed(p1_id, "SIG-123");
    assert_eq!(assert_notif(&n15, "OnScannerInfoRemoved", p1_id), "OnScannerInfoRemoved");

    // 16. OnShareChange
    let n16 = ctx.notify_share_change(p1_id, 98_000_001, 1000);
    assert_eq!(assert_notif(&n16, "OnShareChange", p1_id), "OnShareChange");

    // 17. OnWeaponBanksChanged
    let n17 = ctx.notify_weapon_banks_changed(p1_id, 500_001);
    assert_eq!(assert_notif(&n17, "OnWeaponBanksChanged", p1_id), "OnWeaponBanksChanged");

    // 18. OnWeaponGroupDestroyed
    let n18 = ctx.notify_weapon_group_destroyed(p1_id, 1);
    assert_eq!(assert_notif(&n18, "OnWeaponGroupDestroyed", p1_id), "OnWeaponGroupDestroyed");

    // 19. ProcessSovStatusChanged (SolarSystem multicast)
    let sov = PyRep::Dict(vec![(PyRep::String("corpID".into()), PyRep::Integer(98_000_001))]);
    let n19 = ctx.notify_sov_status_changed(30_000_142, sov);
    assert_eq!(assert_multicast(&n19, "ProcessSovStatusChanged", &[90_000_001, 90_000_002], &[p3_id]), "ProcessSovStatusChanged");

    // 20. OnFleetMove (Fleet multicast)
    let n20 = ctx.notify_fleet_move(fid, 30_000_144);
    assert_eq!(assert_multicast(&n20, "OnFleetMove", &[90_000_001, 90_000_002], &[p3_id]), "OnFleetMove");

    // 21. OnEffectMessage
    let n21 = ctx.notify_effect_message(p1_id, "ShieldBoosterActive");
    assert_eq!(assert_notif(&n21, "OnEffectMessage", p1_id), "OnEffectMessage");

    // 22. OnGodmaFlushLocationProfile
    let n22 = ctx.notify_godma_flush_location_profile(p1_id, 30_000_142);
    assert_eq!(assert_notif(&n22, "OnGodmaFlushLocationProfile", p1_id), "OnGodmaFlushLocationProfile");

    // 23. OnDungeonEdit
    let n23 = ctx.notify_dungeon_edit(p1_id, 900_001);
    assert_eq!(assert_notif(&n23, "OnDungeonEdit", p1_id), "OnDungeonEdit");

    // 24. OnCfgDataChanged (All multicast)
    let n24 = ctx.notify_cfg_data_changed("invTypes");
    assert_eq!(assert_multicast(&n24, "OnCfgDataChanged", &[90_000_001, 90_000_002, p3_id], &[]), "OnCfgDataChanged");

    // 25. OnClusterShutdownInitiated (All multicast)
    let n25 = ctx.notify_cluster_shutdown_initiated(15);
    assert_eq!(assert_multicast(&n25, "OnClusterShutdownInitiated", &[90_000_001, 90_000_002, p3_id], &[]), "OnClusterShutdownInitiated");

    // 26. OnClusterShutdownCancelled (All multicast)
    let n26 = ctx.notify_cluster_shutdown_cancelled();
    assert_eq!(assert_multicast(&n26, "OnClusterShutdownCancelled", &[90_000_001, 90_000_002, p3_id], &[]), "OnClusterShutdownCancelled");

    // 27. OnUpdateServiceMapping
    let n27 = ctx.notify_update_service_mapping(p1_id, "marketProxy");
    assert_eq!(assert_notif(&n27, "OnUpdateServiceMapping", p1_id), "OnUpdateServiceMapping");

    // 28. OnAgentMissionChange
    let n28 = ctx.notify_agent_mission_change(p1_id, 1001);
    assert_eq!(assert_notif(&n28, "OnAgentMissionChange", p1_id), "OnAgentMissionChange");
}
