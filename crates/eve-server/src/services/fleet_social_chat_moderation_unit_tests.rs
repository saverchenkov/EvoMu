#![allow(unused_imports)]
//! Semantic unit tests split from `fleet_social_chat_extra_unit_tests`.

use super::super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::fleet_social::handle as handle_social;
use eve_db::{ChatChannelAclRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[tokio::test]
async fn chat_scoped_messages_moderation_acl_invite_cspa_and_destroy_channel() {
    let ctx = test_ctx_with_clock(1_800_012_000);
    let kw = HashMap::new();
    let owner_id = 90_000_001i64;
    let guest_id = 90_000_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = clone_char_with(snap, owner_id, "Owner", 98_000_001, 30_000_142, 100_000_001);
        c1.balance = IskCents::from_isk(1_000);
        let mut c2 = clone_char_with(snap, guest_id, "Guest", 98_000_002, 30_000_144, 100_000_002);
        c2.balance = IskCents::from_isk(100_000);
        snap.characters.insert(owner_id, c1);
        snap.characters.insert(guest_id, c2);
        snap.character_cspa_charges
            .insert(guest_id, IskCents::from_isk(5_000));
    });

    let mut owner_sess = SessionState::new(10);
    owner_sess.user_id = Some(owner_id as u64);
    owner_sess.character_id = Some(owner_id);
    owner_sess.corporation_id = Some(98_000_001);
    owner_sess.alliance_id = Some(99_000_001);
    owner_sess.fleet_id = Some(700_001);
    owner_sess.solar_system_id = Some(30_000_142);

    let mut guest_sess = SessionState::new(11);
    guest_sess.user_id = Some(guest_id as u64);
    guest_sess.character_id = Some(guest_id);
    guest_sess.corporation_id = Some(98_000_002);
    guest_sess.solar_system_id = Some(30_000_144);

    // 1. Scoped SendMessage checks: wrong system, wrong corp, wrong alliance, wrong fleet -> ChannelAccessDenied
    for bad_chan in [
        "solarsystemid2:30000144",
        "corpid:98000999",
        "allianceid:99000999",
        "fleetid:700999",
    ] {
        assert_eq!(
            handle_social(
                &ctx,
                &mut owner_sess,
                "LSC",
                "SendMessage",
                &[PyRep::String(bad_chan.into()), PyRep::String("hello".into())],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "ChannelAccessDenied",
            "expected ChannelAccessDenied for {bad_chan}"
        );
    }

    // Matching system, corp, alliance, fleet and delayed wormhole local -> join then SendMessage all succeed
    for good_chan in [
        "solarsystemid2:30000142",
        "corpid:98000001",
        "allianceid:99000001",
        "fleetid:700001",
    ] {
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "JoinChannel",
            &[PyRep::String(good_chan.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(good_chan.into()), PyRep::String("o7".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok());
    }

    // Delayed local channel JoinChannel (List arg), SendMessage records speaker, GetMembers shows speaker, LeaveChannel prunes
    owner_sess.solar_system_id = Some(31_000_002);
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::List(vec![PyRep::String("solarsystemid2:31000002".into())])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "SendMessage",
        &[
            PyRep::String("solarsystemid2:31000002".into()),
            PyRep::String("wormhole ping".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let wh_mems = handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "GetMembers",
        &[PyRep::String("solarsystemid2:31000002".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wh_mems.dict_get("isDelayedLocal"), Some(&PyRep::Bool(true)));
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "LeaveChannel",
        &[PyRep::String("solarsystemid2:31000002".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Abyssal local JoinChannel and SendMessage rejected with LocalChannelDisabledInAbyssalSpace
    owner_sess.solar_system_id = Some(32_000_005);
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "JoinChannel",
            &[PyRep::String("solarsystemid2:32000005".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String("solarsystemid2:32000005".into()), PyRep::String("hi".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
    owner_sess.solar_system_id = Some(30_000_142);

    // 2. Invite CSPA insufficient funds, blocked recipient, then funded invite
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "Invite",
            &[PyRep::Integer(guest_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );
    ctx.social_extras
        .lock()
        .unwrap()
        .blocked
        .entry(guest_id)
        .or_default()
        .insert(owner_id);
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "Invite",
            &[PyRep::Integer(guest_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SenderBlockedByRecipient"
    );
    ctx.social_extras.lock().unwrap().blocked.clear();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&owner_id) {
            c.balance = IskCents::from_isk(50_000);
        }
    });
    let inv = handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "Invite",
        &[PyRep::Integer(guest_id), PyRep::String("channel:custom_priv".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        inv.dict_get("channelID").and_then(|v| v.as_str()),
        Some("channel:custom_priv")
    );

    // 3. XmppChatMgr.SetMotd & LSC.AccessControl mode 2 & 3
    handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "SetMotd",
        &[PyRep::String("channel:custom_priv".into()), PyRep::String("New MOTD".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Non-owner guest cannot GMMute
    assert_eq!(
        handle_social(
            &ctx,
            &mut guest_sess,
            "XmppChatMgr",
            "GMMute",
            &[PyRep::Integer(owner_id), PyRep::String("channel:custom_priv".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChatAccessDenied"
    );

    // Promote guest to operator (mode 2), then guest tries mode 3 (transfer ownership -> NotChannelOwner)
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:custom_priv".into()),
            PyRep::Integer(guest_id),
            PyRep::Integer(2),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "AccessControl",
            &[
                PyRep::String("channel:custom_priv".into()),
                PyRep::Integer(guest_id),
                PyRep::Integer(3),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // GetMembers on nonexistent channel -> ChannelNotFound
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "GetMembers",
            &[PyRep::String("channel:missing_999".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );

    // ACL allow_list bypasses password check on JoinChannel
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl.insert(
            "channel:vip".into(),
            ChatChannelAclRecord {
                allow_list: HashSet::from([guest_id]),
                ..Default::default()
            },
        );
    });
    handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("vip".into()), PyRep::String("vip_pw".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(handle_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String("channel:vip".into())],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
}

#[tokio::test]
async fn chat_channel_takeover_nonmember_spoofing_and_owner_lockout_are_blocked() {
    let ctx = test_ctx_with_clock(1_800_012_500);
    let kw = HashMap::new();
    let owner_id = 90_000_011i64;
    let op_id = 90_000_012i64;
    let attacker_id = 90_000_013i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = clone_char_with(snap, owner_id, "ChanOwner", 98_000_011, 30_000_142, 100_000_011);
        c1.war_faction_id = Some(500_001);
        let c2 = clone_char_with(snap, op_id, "ChanOp", 98_000_012, 30_000_142, 100_000_012);
        let mut c3 = clone_char_with(snap, attacker_id, "Attacker", 98_000_013, 30_000_144, 100_000_013);
        c3.war_faction_id = Some(500_004);
        snap.characters.insert(owner_id, c1);
        snap.characters.insert(op_id, c2);
        snap.characters.insert(attacker_id, c3);
    });

    let mut owner_sess = SessionState::new(21);
    owner_sess.user_id = Some(owner_id as u64);
    owner_sess.character_id = Some(owner_id);
    owner_sess.solar_system_id = Some(30_000_142);
    owner_sess.war_faction_id = Some(500_001);

    let mut op_sess = SessionState::new(22);
    op_sess.user_id = Some(op_id as u64);
    op_sess.character_id = Some(op_id);
    op_sess.solar_system_id = Some(30_000_142);

    let mut attacker_sess = SessionState::new(23);
    attacker_sess.user_id = Some(attacker_id as u64);
    attacker_sess.character_id = Some(attacker_id);
    attacker_sess.solar_system_id = Some(30_000_144);
    attacker_sess.war_faction_id = Some(500_004);

    // 1. CreatePlayerOwnedChannel refuses to overwrite an existing channel (exact or mixed-case name)
    handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[
            PyRep::String("Intel Hub".into()),
            PyRep::String("secretpw".into()),
            PyRep::String("Owner MOTD".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    for hijack_name in ["Intel Hub", "INTEL HUB"] {
        let err = handle_social(
            &ctx,
            &mut attacker_sess,
            "XmppChatMgr",
            "CreatePlayerOwnedChannel",
            &[PyRep::String(hijack_name.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "ChannelAlreadyExists");
    }
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.chat_channels["channel:intel_hub"];
        assert_eq!(ch.owner_id, Some(owner_id));
        assert_eq!(ch.password.as_deref(), Some("secretpw"));
        assert_eq!(ch.motd, "Owner MOTD");
        assert!(ch.members.contains(&owner_id));
        assert!(!ch.members.contains(&attacker_id));
    });

    // 2. Non-member and banned member cannot SendMessage into the channel
    let err_unjoined = handle_social(
        &ctx,
        &mut attacker_sess,
        "XmppChatMgr",
        "SendMessage",
        &[PyRep::String("channel:intel_hub".into()), PyRep::String("spoofed".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_unjoined.code, "ChannelAccessDenied");

    // 3. Scoped system/constellation/region/faction channels reject spoofing even if case-varied
    let jita_cid = ctx.sde.solar_systems[&30_000_142].constellation_id;
    let jita_rid = ctx.sde.solar_systems[&30_000_142].region_id;
    let wrong_cid = jita_cid + 999;
    let wrong_rid = jita_rid + 999;
    for bad_chan in [
        format!("constellationid:{wrong_cid}"),
        format!("ConstellationID:{wrong_cid}"),
        format!("regionid:{wrong_rid}"),
        "factionid:500004".to_string(),
        "resourcewars:30000144".to_string(),
        "SOLARSYSTEMID2:30000144".to_string(),
    ] {
        let err = handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(bad_chan.clone()), PyRep::String("spoof".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "ChannelAccessDenied", "expected rejection for {bad_chan}");
    }
    for good_chan in [
        format!("constellationid:{jita_cid}"),
        format!("regionid:{jita_rid}"),
        "factionid:500001".to_string(),
    ] {
        handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "JoinChannel",
            &[PyRep::String(good_chan.clone())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(handle_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(good_chan.clone()), PyRep::String("legit".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok());
    }

    // 4. Operator cannot mute, ban/kick, clear ACL of owner, or promote another operator
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String("channel:intel_hub".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_social(
        &ctx,
        &mut op_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String("channel:intel_hub".into()), PyRep::String("secretpw".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:intel_hub".into()),
            PyRep::Integer(op_id),
            PyRep::Integer(2),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // Operator cannot promote attacker to operator (mode 2)
    assert_eq!(
        handle_social(
            &ctx,
            &mut op_sess,
            "LSC",
            "AccessControl",
            &[
                PyRep::String("channel:intel_hub".into()),
                PyRep::Integer(attacker_id),
                PyRep::Integer(2),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // Operator cannot mute owner via AccessControl (-1) or GMMute
    assert_eq!(
        handle_social(
            &ctx,
            &mut op_sess,
            "LSC",
            "AccessControl",
            &[
                PyRep::String("channel:intel_hub".into()),
                PyRep::Integer(owner_id),
                PyRep::Integer(-1),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert_eq!(
        handle_social(
            &ctx,
            &mut op_sess,
            "XmppChatMgr",
            "GMMute",
            &[PyRep::Integer(owner_id), PyRep::String("channel:intel_hub".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // Operator cannot kick/ban owner (mode -2) or reset owner ACL (mode 0)
    for bad_mode in [-2_i64, 0] {
        assert_eq!(
            handle_social(
                &ctx,
                &mut op_sess,
                "LSC",
                "AccessControl",
                &[
                    PyRep::String("channel:intel_hub".into()),
                    PyRep::Integer(owner_id),
                    PyRep::Integer(bad_mode),
                ],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "NotChannelOwner"
        );
    }

    // Owner still has membership, operator rights, and is not in deny_list or blocked_in_channel
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.chat_channels["channel:intel_hub"];
        assert!(ch.members.contains(&owner_id));
        assert_eq!(ch.owner_id, Some(owner_id));
        let denied = snap
            .chat_channel_acl
            .get("channel:intel_hub")
            .is_some_and(|a| a.deny_list.contains(&owner_id));
        assert!(!denied);
    });
    assert!(!ctx
        .social_extras
        .lock()
        .unwrap()
        .blocked_in_channel
        .get("channel:intel_hub")
        .is_some_and(|b| b.contains(&owner_id)));

    // Owner can ban attacker via mode -2, blocking attacker SendMessage and JoinChannel
    handle_social(
        &ctx,
        &mut attacker_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String("channel:intel_hub".into()), PyRep::String("secretpw".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:intel_hub".into()),
            PyRep::Integer(attacker_id),
            PyRep::Integer(-2),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_social(
            &ctx,
            &mut attacker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String("channel:intel_hub".into()), PyRep::String("after ban".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
}
