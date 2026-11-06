//! Extra unit and mutation-killing tests for `fleet_social_notify.rs`.

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::fleet_social_fleets::FleetAdvertState;
use crate::services::fleet_social::fleet_social_notify::*;
use crate::services::fleet_social::handle;
use eve_db::{AllianceRecord, ChatChannelRecord, CorporationRecord, FleetRole, ServerClock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[test]
fn options_keyval_and_online_status_checks() {
    let kv_none = options_keyval(false, true, None);
    assert_eq!(kv_none.dict_get("isFreeMove").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(kv_none.dict_get("isRegistered").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_none.dict_get("autoJoinSquadID"), Some(&PyRep::None));

    let kv_some = options_keyval(true, false, Some(42));
    assert_eq!(kv_some.dict_get("isFreeMove").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_some.dict_get("isRegistered").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(kv_some.dict_get("autoJoinSquadID").and_then(|v| v.as_i64()), Some(42));

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    assert!(!is_character_online_for_fleet(&ctx, 90_000_001));
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).unwrap().user_id);
    ctx.db
        .acquire_online_lock(uid, 101, Some(90_000_001), true, 1_700_000_000)
        .unwrap();
    assert!(is_character_online_for_fleet(&ctx, 90_000_001));
}

#[tokio::test]
async fn remove_character_from_fleet_boss_handover_disband_and_cleanup() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, _) = seed_multi_member_fleet(&ctx).await;

    // Seed fleet extras, adverts, watchlist, join_requests, invites, and shipping storage
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.disconnected_members.insert(90_000_002, 1_700_000_000);
        ex.invite_details
            .insert((fleet_id, 90_000_001), (1, 1, FleetRole::SquadMember, 90_000_001));
        ex.invite_timestamps.insert((fleet_id, 90_000_001), 1_700_000_000);
        ex.watchlist.entry(90_000_001).or_default().insert(90_000_003);
        ex.watchlist.entry(90_000_003).or_default().insert(90_000_001);
        ex.join_requests.entry(fleet_id).or_default().insert(90_000_001);
        ex.adverts.insert(
            fleet_id,
            FleetAdvertState {
                fleet_id,
                leader_id: 90_000_001,
                fleet_name: "Test Fleet".into(),
                description: "Desc".into(),
                solar_system_id: 30_000_142,
                invite_scope: 1,
                auto_join: true,
                min_standing: 0.0,
                min_security: -10.0,
                allowed_entities: HashSet::new(),
                _created_unix: 1_700_000_000,
            },
        );
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.authorized_shipping_storage.insert((100_000_001, 90_000_001));
        rt.authorized_shipping_storage.insert((100_000_001, 90_000_003));
    });

    // Remove boss 90_000_001: since 90_000_002 is disconnected, handover should pick connected 90_000_003 (SquadCommander) over disconnected 90_000_002 (WingCommander)!
    let (disbanded, new_boss) = remove_character_from_fleet(&ctx, fleet_id, 90_000_001);
    assert!(!disbanded);
    assert_eq!(new_boss, Some(90_000_003));

    {
        let ex = ctx.fleet_extras.lock().unwrap();
        assert!(!ex.watchlist.contains_key(&90_000_001));
        assert!(!ex.watchlist.get(&90_000_003).unwrap().contains(&90_000_001));
        assert!(!ex.join_requests.get(&fleet_id).unwrap().contains(&90_000_001));
        assert_eq!(ex.adverts.get(&fleet_id).unwrap().leader_id, 90_000_003);
    }
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert!(!rt.authorized_shipping_storage.contains(&(100_000_001, 90_000_001)));
        assert!(rt.authorized_shipping_storage.contains(&(100_000_001, 90_000_003)));
    });

    // Remove remaining members until fleet disbands
    let (d2, _) = remove_character_from_fleet(&ctx, fleet_id, 90_000_002);
    assert!(!d2);
    let (d3, nb3) = remove_character_from_fleet(&ctx, fleet_id, 90_000_003);
    assert!(!d3);
    assert_eq!(nb3, Some(90_000_004));
    let (d4, nb4) = remove_character_from_fleet(&ctx, fleet_id, 90_000_004);
    assert!(!d4);
    assert_eq!(nb4, Some(90_000_005));
    let (d5, nb5) = remove_character_from_fleet(&ctx, fleet_id, 90_000_005);
    assert!(d5);
    assert_eq!(nb5, None);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fleet_id));
        assert!(!snap.chat_channels.contains_key(&format!("fleetid:{fleet_id}")));
    });
    assert!(!ctx.fleet_extras.lock().unwrap().adverts.contains_key(&fleet_id));
}

#[tokio::test]
async fn disconnect_grace_lifecycle_tick_select_sync_and_prune() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, _) = seed_multi_member_fleet(&ctx).await;

    // Seed Local (normal and delayed) and fleet chat channels
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channels.insert(
            format!("fleetid:{fleet_id}"),
            ChatChannelRecord {
                channel_id: format!("fleetid:{fleet_id}"),
                name: "Fleet".into(),
                motd: String::new(),
                owner_id: Some(90_000_001),
                members: HashSet::from([90_000_001, 90_000_002, 90_000_003, 90_000_004, 90_000_005]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
        snap.chat_channels.insert(
            "solarsystemid2:30000142".into(),
            ChatChannelRecord {
                channel_id: "solarsystemid2:30000142".into(),
                name: "Local".into(),
                motd: String::new(),
                owner_id: None,
                members: HashSet::from([90_000_001, 90_000_002]),
                is_system_local: true,
                solar_system_id: Some(30_000_142),
                is_private: false,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
        snap.chat_channels.insert(
            "solarsystemid2:31000005".into(),
            ChatChannelRecord {
                channel_id: "solarsystemid2:31000005".into(),
                name: "Delayed Local".into(),
                motd: String::new(),
                owner_id: None,
                members: HashSet::from([90_000_001, 90_000_002]),
                is_system_local: true,
                solar_system_id: Some(31_000_005),
                is_private: false,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: true,
            },
        );
    });

    // Disconnect boss 90_000_001 via handle_fleet_member_disconnect
    let mut boss_sess = SessionState::new(1);
    boss_sess.user_id = Some(1);
    boss_sess.character_id = Some(90_000_001);
    boss_sess.fleet_id = Some(fleet_id);
    handle_fleet_member_disconnect(&ctx, &mut boss_sess);
    assert_eq!(boss_sess.character_id, None);
    assert_eq!(boss_sess.fleet_id, None);
    assert!(ctx
        .fleet_extras
        .lock()
        .unwrap()
        .disconnected_members
        .contains_key(&90_000_001));

    // Boss should have been reassigned to 90_000_002 on disconnect
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets.get(&fleet_id).unwrap().boss_id, 90_000_002);
        // Removed from Local, but kept in fleet channel during grace window
        assert!(!snap
            .chat_channels
            .get("solarsystemid2:30000142")
            .unwrap()
            .members
            .contains(&90_000_001));
        assert!(snap
            .chat_channels
            .get(&format!("fleetid:{fleet_id}"))
            .unwrap()
            .members
            .contains(&90_000_001));
    });

    // Re-select 90_000_001 within 120s grace window -> restores fleet_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(1_000_009)
            .or_default()
            .member_account_keys
            .insert(90_000_001, 1003);
    });
    clock.advance_secs(60);
    let mut reselect_sess = SessionState::new(2);
    reselect_sess.corporation_id = Some(1_000_009);
    on_character_select(&ctx, &mut reselect_sess, 90_000_001, false);
    assert_eq!(reselect_sess.fleet_id, Some(fleet_id));
    assert_eq!(reselect_sess.corp_account_key, 1003);

    // Disconnect 90_000_004 and add pending invite, then advance past 120s expiry
    handle_character_disconnect(&ctx, 90_000_004);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&fleet_id).unwrap().invites.insert(90_000_099);
    });
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.invite_timestamps
            .insert((fleet_id, 90_000_099), ctx.now_unix());
        ex.invite_details
            .insert((fleet_id, 90_000_099), (1, 1, FleetRole::SquadMember, 90_000_002));
    }

    // Advance 119s (just under 120s boundary): still present
    clock.advance_secs(FLEET_DISCONNECT_GRACE_SECS - 1);
    tick_fleet_lifecycle(&ctx);
    assert!(ctx
        .fleet_extras
        .lock()
        .unwrap()
        .disconnected_members
        .contains_key(&90_000_004));
    assert!(ctx
        .fleet_extras
        .lock()
        .unwrap()
        .invite_timestamps
        .contains_key(&(fleet_id, 90_000_099)));

    // Advance 1s more to hit exact 120s boundary: both expire
    clock.advance_secs(1);
    tick_fleet_lifecycle(&ctx);
    assert!(!ctx
        .fleet_extras
        .lock()
        .unwrap()
        .disconnected_members
        .contains_key(&90_000_004));
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        assert!(!f.members.contains(&90_000_004));
        assert!(!f.invites.contains(&90_000_099));
    });

    // sync_session_fleet clears fleet_id for kicked/expired member 90_000_004
    let mut kicked_sess = SessionState::new(3);
    kicked_sess.fleet_id = Some(fleet_id);
    sync_session_fleet(&ctx, &mut kicked_sess, 90_000_004);
    assert_eq!(kicked_sess.fleet_id, None);

    // on_character_select when NOT in grace and was_already_online == false removes stale membership
    let mut stale_sess = SessionState::new(4);
    on_character_select(&ctx, &mut stale_sess, 90_000_003, false);
    assert_eq!(stale_sess.fleet_id, None);
    assert_eq!(character_active_fleet_id(&ctx, 90_000_003), None);

    // on_character_logout removes 90_000_001 from all fleets and channels
    on_character_logout(&ctx, 90_000_001);
    assert_eq!(character_active_fleet_id(&ctx, 90_000_001), None);

    // prune_offline_fleets_and_channels removes remaining offline fleet & channels
    prune_offline_fleets_and_channels(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fleet_id));
    });
}

#[test]
fn persistence_helpers_options_join_requests_and_watchlist_hydration() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let fid = 555;

    // Default options when nothing in memory or DB
    assert_eq!(get_fleet_options(&ctx, fid), (false, false));

    // Set options in memory, sync to DB, clear memory, and hydrate from DB
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.voice_enabled.insert(fid, true);
        ex.loot_logging.insert(fid, true);
        ex.join_requests.entry(fid).or_default().insert(90_000_002);
        ex.watchlist.entry(90_000_001).or_default().insert(90_000_002);
    }
    sync_fleet_options_to_db(&ctx);
    sync_fleet_join_requests_to_db(&ctx);
    sync_fleet_watchlist_to_db(&ctx);

    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.voice_enabled.clear();
        ex.loot_logging.clear();
        ex.join_requests.clear();
        ex.watchlist.clear();
        ex.watchlist_loaded = false;
    }

    assert_eq!(get_fleet_options(&ctx, fid), (true, true));
    assert!(get_fleet_join_requests(&ctx, fid).contains(&90_000_002));
    ensure_fleet_watchlist_loaded(&ctx);
    assert!(ctx
        .fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .get(&90_000_001)
        .unwrap()
        .contains(&90_000_002));

    // build_fleet_state_keyval with target tags and options
    ctx.fleet_extras
        .lock()
        .unwrap()
        .target_tags
        .entry(fid)
        .or_default()
        .insert(9001, "A".into());
    let state_kv = build_fleet_state_keyval(&ctx, Some(fid), 90_000_002, 0.5, 0.75, 1.0);
    assert_eq!(state_kv.dict_get("characterID").and_then(|v| v.as_i64()), Some(90_000_002));
    assert_eq!(state_kv.dict_get("shieldRatio").and_then(|v| v.as_f64()), Some(0.5));
    assert_eq!(state_kv.dict_get("armorRatio").and_then(|v| v.as_f64()), Some(0.75));
    assert_eq!(state_kv.dict_get("hullRatio").and_then(|v| v.as_f64()), Some(1.0));
}

#[tokio::test]
async fn corp_contacts_and_labels_all_branches_via_corp_registry() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_000_001;
    let ceo_id = 90_000_001;
    let other_char = 90_000_002;
    let ally_id = 99_000_001;
    let faction_id = 500_001;

    ctx.db.with_mut_snapshot(|snap| {
        let ch2 = clone_char_with(snap, other_char, "Contact Two", corp_id, 30_000_142, 100_000_002);
        snap.characters.insert(other_char, ch2);
        if let Some(ch1) = snap.characters.get_mut(&ceo_id) {
            ch1.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ceo_id,
                alliance_id: Some(ally_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(ceo_id, eve_db::corp_roles::ROLE_DIRECTOR)]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    let mut session = SessionState::new(10);
    session.user_id = Some(1);
    session.character_id = Some(ceo_id);
    session.corporation_id = Some(corp_id);
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    // 1. AddCorporateContact: invalid owner -> OwnerNotFound
    let err = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(888_888_888), PyRep::Real(5.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "OwnerNotFound");

    // 2. AddCorporateContact: character, corporation, alliance, faction (standing clamped to -10..=10)
    for (cid, raw_standing, expected_standing) in [
        (other_char, 15.0, 10.0),
        (corp_id, -15.0, -10.0),
        (ally_id, 5.0, 5.0),
        (faction_id, 2.5, 2.5),
    ] {
        let added = handle(
            &ctx,
            &mut session,
            "corpRegistry",
            "AddCorporateContact",
            &[PyRep::Integer(cid), PyRep::Real(raw_standing), PyRep::Integer(3)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(added.dict_get("contactID").and_then(|v| v.as_i64()), Some(cid));
        assert_eq!(
            added.dict_get("relationshipID").and_then(|v| v.as_f64()),
            Some(expected_standing)
        );
    }

    // Duplicate AddCorporateContact -> ContactAlreadyExists
    let dup_err = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(other_char), PyRep::Real(5.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "ContactAlreadyExists");

    // 3. EditCorporateContact: missing contact when list non-empty -> ContactNotFound
    let edit_err = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "EditCorporateContact",
        &[PyRep::Integer(777_777_777), PyRep::Real(0.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(edit_err.code, "ContactNotFound");

    // 4. EditContactsRelationshipID: empty list -> ContactNotFound, unknown id -> ContactNotFound, valid list -> updates
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "corpRegistry",
            "EditContactsRelationshipID",
            &[PyRep::List(vec![]), PyRep::Real(8.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );
    let rel_ok = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "EditContactsRelationshipID",
        &[
            PyRep::List(vec![PyRep::Integer(other_char), PyRep::Integer(ally_id)]),
            PyRep::Real(7.5),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rel_ok.dict_get("relationshipID").and_then(|v| v.as_f64()), Some(7.5));

    // 5. AssignLabels & RemoveLabels with (lid, List), (contact_id >= 1_000_000, lid), and DeleteLabel
    let _ = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "CreateLabel",
        &[PyRep::String("Friends".into()), PyRep::Integer(0xFF00)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    let assign2 = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "AssignLabels",
        &[PyRep::Integer(other_char), PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(assign2.dict_get("labelID").and_then(|v| v.as_i64()), Some(4));

    let rem_lbl = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "RemoveLabels",
        &[PyRep::Integer(2), PyRep::List(vec![PyRep::Integer(other_char)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rem_lbl.dict_get("labelID").and_then(|v| v.as_i64()), Some(2));

    // DeleteLabel: existing label 1 vs non-existent label 64
    let del_ok = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "DeleteLabel",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_ok.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));

    let del_err = handle(
        &ctx,
        &mut session,
        "corpRegistry",
        "DeleteLabel",
        &[PyRep::Integer(64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(del_err.code, "LabelNotFound");
}
