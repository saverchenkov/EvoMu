use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use eve_db::FleetRole;

fn mk_member(cid: i64, role: FleetRole, wing: Option<i64>, squad: Option<i64>) -> FleetMemberRecord {
    FleetMemberRecord {
        character_id: cid,
        role,
        wing_id: wing,
        squad_id: squad,
        takes_fleet_warp: true,
        accepts_regroup: false,
        accepts_conduit_jumps: true,
        job: 2,
        role_name: String::new(),
        join_time_unix: 1000,
    }
}

#[test]
fn member_options_and_broadcast_group_scope_logic() {
    let fc = mk_member(100, FleetRole::FleetCommander, Some(1), Some(10));
    let wc = mk_member(101, FleetRole::WingCommander, Some(1), Some(10));
    let sc = mk_member(102, FleetRole::SquadCommander, Some(1), Some(10));
    let sm = mk_member(103, FleetRole::SquadMember, Some(1), Some(10));
    let other_sm = mk_member(104, FleetRole::SquadMember, Some(2), Some(20));

    let opt = member_opt_outs(&fc);
    assert_eq!(opt.dict_get("acceptsFleetWarp").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(opt.dict_get("acceptsFleetRegroups").and_then(|v| v.as_bool()), Some(false));

    let mkv = member_keyval(&fc);
    assert_eq!(mkv.dict_get("charID").and_then(|v| v.as_i64()), Some(100));

    let okv = options_keyval(true, false, Some(10));
    assert_eq!(okv.dict_get("isFreeMove").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(okv.dict_get("autoJoinSquadID").and_then(|v| v.as_i64()), Some(10));

    assert!(broadcast_group_allows(BROADCAST_DOWN, &fc, &fc));
    assert!(broadcast_group_allows(BROADCAST_DOWN, &fc, &other_sm));
    assert!(broadcast_group_allows(BROADCAST_DOWN, &wc, &sm));
    assert!(!broadcast_group_allows(BROADCAST_DOWN, &wc, &other_sm));
    assert!(!broadcast_group_allows(BROADCAST_DOWN, &sm, &fc));

    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &sc));
    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &wc));
    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &fc));
    assert!(!broadcast_group_allows(BROADCAST_UP, &other_sm, &wc));
    assert!(broadcast_group_allows(0, &other_sm, &wc));
}

#[tokio::test]
async fn fleet_removal_handover_watchlist_and_corp_contacts() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, _) = seed_multi_member_fleet(&ctx).await;
    assert_eq!(character_active_fleet_id(&ctx, 90_000_001), Some(fleet_id));

    ctx.fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .entry(90_000_001)
        .or_default()
        .insert(90_000_002);
    stream_fleet_watchlist_damage(&ctx, 90_000_002, 0.8, 0.9, 1.0);

    let (disbanded, new_boss) = remove_character_from_fleet(&ctx, fleet_id, 90_000_001);
    assert!(!disbanded);
    assert!(new_boss.is_some());
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        assert_ne!(f.boss_id, 90_000_001);
        assert!(!build_wings_dict(f).is_empty());
    });

    let corp_id = 1_000_009;
    assert!(handle_corp_contacts_and_labels(&ctx, corp_id, false, "AddCorporateContact", &[]).unwrap().is_err());

    let added = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "AddCorporateContact",
        &[PyRep::Integer(90_000_002), PyRep::Real(5.0), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(added.dict_get("contactID").and_then(|v| v.as_i64()), Some(90_000_002));

    let edited = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "EditCorporateContact",
        &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Integer(3)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(edited.dict_get("relationshipID").and_then(|v| v.as_f64()), Some(10.0));

    let assigned = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(4)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(assigned.dict_get("labelID").and_then(|v| v.as_i64()), Some(4));
}

#[cfg(test)]
#[path = "fleet_social_notify_extra_unit_tests.rs"]
mod fleet_social_notify_extra_unit_tests;

#[tokio::test]
async fn fleet_notify_emitters_disconnect_all_boss_fallback_and_corp_contact_edge_cases() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, _) = seed_multi_member_fleet(&ctx).await;
    let c1 = 90_000_001_i64;
    let c2 = 90_000_002_i64;
    let uid1 = ctx.db.with_mut_snapshot(|s| {
        let uid = s.characters[&c1].user_id;
        if !s.accounts.values().any(|a| a.user_id == uid) {
            s.accounts.insert(
                format!("user_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("user_{uid}"),
                    password_token: "x".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(uid1, 901, Some(c1), true, 1_700_000_000)
        .unwrap();

    // 1. Direct notification emitter tests: emit_join (valid + missing), emit_leave, emit_options_changed, emit_motd
    let _ = ctx.drain_notifications_for_char(c1);
    emit_join(&ctx, 999_999, c1);
    assert!(ctx.drain_notifications_for_char(c1).is_empty());

    emit_join(&ctx, fleet_id, c1);
    emit_leave(&ctx, fleet_id, c1);
    emit_options_changed(
        &ctx,
        fleet_id,
        options_keyval(false, false, None),
        options_keyval(true, true, Some(1)),
    );
    emit_motd(&ctx, NotificationTarget::Fleet(fleet_id), "Fleet MOTD", true);
    emit_motd(&ctx, NotificationTarget::Character(c1), "Direct MOTD", false);

    let c1_notifs: Vec<String> = ctx
        .drain_notifications_for_char(c1)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(c1_notifs.contains(&"OnFleetJoin".to_string()));
    assert!(c1_notifs.contains(&"OnFleetLeave".to_string()));
    assert!(c1_notifs.contains(&"OnFleetOptionsChanged".to_string()));
    assert!(c1_notifs.contains(&"OnFleetMotdChanged".to_string()));

    // 2. broadcast_group_allows: SquadCommander BROADCAST_DOWN only reaches same squad
    let sc1 = mk_member(201, FleetRole::SquadCommander, Some(1), Some(10));
    let sm_same = mk_member(202, FleetRole::SquadMember, Some(1), Some(10));
    let sm_other = mk_member(203, FleetRole::SquadMember, Some(1), Some(11));
    assert!(broadcast_group_allows(BROADCAST_DOWN, &sc1, &sm_same));
    assert!(!broadcast_group_allows(BROADCAST_DOWN, &sc1, &sm_other));

    // 3. stream_fleet_watchlist_damage: watcher not in same fleet -> no notifications;
    //    watcher in fleet when target active_ship_id <= 0 -> OnFleetStateChange without OnDamageStateChange
    let _ = ctx.drain_notifications_for_char(c1);
    stream_fleet_watchlist_damage(&ctx, c2, 1.0, 1.0, 1.0); // no watchers yet
    assert!(ctx.drain_notifications_for_char(c1).is_empty());

    ctx.fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .entry(90_000_099) // outsider not in fleet
        .or_default()
        .insert(c2);
    stream_fleet_watchlist_damage(&ctx, c2, 0.5, 0.5, 0.5);
    assert!(ctx.drain_notifications_for_char(90_000_099).is_empty());

    ctx.fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .entry(c1)
        .or_default()
        .insert(c2);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.active_ship_id = 0;
        }
    });
    stream_fleet_watchlist_damage(&ctx, c2, 0.4, 0.5, 0.6);
    let wl_notifs: Vec<String> = ctx
        .drain_notifications_for_char(c1)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(wl_notifs.contains(&"OnFleetStateChange".to_string()));
    assert!(!wl_notifs.contains(&"OnDamageStateChange".to_string()));

    // 4. reassign_fleet_boss_on_disconnect when ALL other members are disconnected -> boss unchanged
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for cid in [90_000_002, 90_000_003, 90_000_004, 90_000_005] {
            ex.disconnected_members.insert(cid, 1_700_000_000);
        }
    }
    reassign_fleet_boss_on_disconnect(&ctx, c1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets[&fleet_id].boss_id, c1);
    });
    ctx.fleet_extras.lock().unwrap().disconnected_members.clear();

    // 5. on_character_select with was_already_online = true preserves fleet_id
    let mut online_sess = SessionState::new(77);
    on_character_select(&ctx, &mut online_sess, c2, true);
    assert_eq!(online_sess.fleet_id, Some(fleet_id));
    let mut no_fleet_sess = SessionState::new(78);
    on_character_select(&ctx, &mut no_fleet_sess, 90_999_999, false);
    assert_eq!(no_fleet_sess.fleet_id, None);

    // 6. remove_character_from_fleet on non-member returns (false, None) & remove_character_from_all_fleets
    assert_eq!(remove_character_from_fleet(&ctx, fleet_id, 90_999_999), (false, None));
    let removed_all = remove_character_from_all_fleets(&ctx, c2);
    assert_eq!(removed_all.len(), 1);
    assert_eq!(removed_all[0].0, fleet_id);

    // 7. handle_corp_contacts_and_labels edge cases:
    // - EditCorporateContact on empty list with contact_id < 99_000_000 inserts new character contact
    // - EditCorporateContact on empty list with contact_id >= 99_000_000 -> ContactNotFound
    // - EditContactsRelationshipID with single Integer(id) and with unknown id
    // - AssignLabels / RemoveLabels with (cid < 1_000_000, lid), empty list, unknown contact, and DeleteLabel clearing masks
    let empty_corp = 98_777_001_i64;
    assert_eq!(
        handle_corp_contacts_and_labels(
            &ctx,
            empty_corp,
            true,
            "EditCorporateContact",
            &[PyRep::Integer(99_000_001), PyRep::Real(5.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );

    let auto_added = handle_corp_contacts_and_labels(
        &ctx,
        empty_corp,
        true,
        "EditCorporateContact",
        &[PyRep::Integer(500_001), PyRep::Real(3.5), PyRep::Integer(2)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(auto_added.dict_get("contactID").and_then(|v| v.as_i64()), Some(500_001));
    assert_eq!(auto_added.dict_get("labelMask").and_then(|v| v.as_i64()), Some(2));

    // EditContactsRelationshipID with single Integer arg + missing contact error
    let single_rel = handle_corp_contacts_and_labels(
        &ctx,
        empty_corp,
        true,
        "EditContactsRelationshipID",
        &[PyRep::Integer(500_001), PyRep::Real(9.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(single_rel.dict_get("relationshipID").and_then(|v| v.as_f64()), Some(9.0));
    assert_eq!(
        handle_corp_contacts_and_labels(
            &ctx,
            empty_corp,
            true,
            "EditContactsRelationshipID",
            &[PyRep::Integer(123_456), PyRep::Real(1.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );

    // AssignLabels / RemoveLabels with (lid, cid) when a0 < 1_000_000, plus empty/missing contact errors
    assert_eq!(
        handle_corp_contacts_and_labels(
            &ctx,
            empty_corp,
            true,
            "AssignLabels",
            &[PyRep::List(vec![]), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );
    assert_eq!(
        handle_corp_contacts_and_labels(
            &ctx,
            empty_corp,
            true,
            "AssignLabels",
            &[PyRep::Integer(4), PyRep::Integer(999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );
    let _ = handle_corp_contacts_and_labels(
        &ctx,
        empty_corp,
        true,
        "AssignLabels",
        &[PyRep::Integer(4), PyRep::Integer(500_001)],
    )
    .unwrap()
    .unwrap();
    let _ = handle_corp_contacts_and_labels(
        &ctx,
        empty_corp,
        true,
        "RemoveLabels",
        &[PyRep::Integer(2), PyRep::Integer(500_001)],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contact_labels
            .entry(empty_corp)
            .or_default()
            .push((4, "Allies".into(), 0xFF));
    });
    let _ = handle_corp_contacts_and_labels(
        &ctx,
        empty_corp,
        true,
        "DeleteLabel",
        &[PyRep::Integer(4)],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let entry = snap.corp_contacts[&empty_corp]
            .iter()
            .find(|c| c.contact_id == 500_001)
            .unwrap();
        assert!(entry.label_ids.is_empty());
    });

    // Direct checks for `emit_fleet_session_cleared` and `emit_fleet_member_removed`
    let _ = ctx.drain_notifications_for_char(90_000_001);
    emit_fleet_session_cleared(&ctx, 700_001, 90_000_001, 1);
    assert!(ctx
        .drain_notifications_for_char(90_000_001)
        .iter()
        .any(|n| n.event == "OnSessionChanged"));

    let outcome = crate::domain::fleets::RemoveFleetMemberOutcome {
        fleet_id: 700_001,
        removed_char_id: 90_000_001,
        was_member: true,
        disbanded: true,
        old_role_num: eve_db::FleetRole::FleetCommander as i64,
        handover: None,
    };
    emit_fleet_member_removed(&ctx, &outcome);
}
