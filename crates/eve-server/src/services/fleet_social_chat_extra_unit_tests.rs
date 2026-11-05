//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_chat`.

use crate::services::fleet_social::fleet_social_chat::prune_delayed_local_speakers_on_leave;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::fleet_social::handle as handle_social;
use eve_db::{ChatChannelAclRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[tokio::test]
async fn delayed_local_witness_pruning_keeps_witnessed_and_removes_unwitnessed_speakers() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let chan_id = "solarsystemid2:31000001";
    let spk_in_chan = 90_000_001i64;
    let spk_with_witness = 90_000_002i64;
    let spk_no_witness = 90_000_003i64;
    let leaving_char = 90_000_004i64;
    let staying_witness = 90_000_005i64;

    ctx.social_extras
        .lock()
        .unwrap()
        .delayed_local_speakers
        .insert(
            chan_id.into(),
            HashSet::from([spk_in_chan, spk_with_witness, spk_no_witness]),
        );

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("wh_local_witnesses:{chan_id}:{spk_in_chan}"),
            serde_json::to_string(&HashSet::from([leaving_char])).unwrap(),
        );
        snap.service_state_blobs.insert(
            format!("wh_local_witnesses:{chan_id}:{spk_with_witness}"),
            serde_json::to_string(&HashSet::from([leaving_char, staying_witness])).unwrap(),
        );
        snap.service_state_blobs.insert(
            format!("wh_local_witnesses:{chan_id}:{spk_no_witness}"),
            serde_json::to_string(&HashSet::from([leaving_char])).unwrap(),
        );

        let remaining = HashSet::from([spk_in_chan, staying_witness]);
        prune_delayed_local_speakers_on_leave(&ctx, snap, chan_id, leaving_char, &remaining);

        assert!(snap
            .service_state_blobs
            .contains_key(&format!("wh_local_witnesses:{chan_id}:{spk_in_chan}")));
        assert!(snap
            .service_state_blobs
            .contains_key(&format!("wh_local_witnesses:{chan_id}:{spk_with_witness}")));
        assert!(!snap
            .service_state_blobs
            .contains_key(&format!("wh_local_witnesses:{chan_id}:{spk_no_witness}")));
    });

    let speakers = ctx
        .social_extras
        .lock()
        .unwrap()
        .delayed_local_speakers
        .get(chan_id)
        .cloned()
        .unwrap();
    assert!(speakers.contains(&spk_in_chan));
    assert!(speakers.contains(&spk_with_witness));
    assert!(!speakers.contains(&spk_no_witness));
}

#[tokio::test]
async fn xmpp_add_character_blocks_passwords_operators_and_abyssal_resync() {
    let ctx = test_ctx_with_clock(1_800_011_000);
    let kw = HashMap::new();
    let owner_id = 90_000_001i64;
    let op_id = 90_000_002i64;
    let guest_id = 90_000_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name) in [(owner_id, "Owner"), (op_id, "Operator"), (guest_id, "Guest")] {
            let mut ch = clone_char_with(snap, cid, name, 98_000_001, 30_000_142, 100_000_001);
            if cid == owner_id {
                ch.create_date_unix = 1_704_067_200; // epoch sentinel -> treated as now (rookie)
            }
            snap.characters.insert(cid, ch);
        }
    });

    let mut owner_sess = SessionState::new(1);
    owner_sess.user_id = Some(owner_id as u64);
    owner_sess.character_id = Some(owner_id);
    owner_sess.corporation_id = Some(98_000_001);
    owner_sess.solar_system_id = Some(30_000_142);

    let mut guest_sess = SessionState::new(2);
    guest_sess.user_id = Some(guest_id as u64);
    guest_sess.character_id = Some(guest_id);
    guest_sess.corporation_id = Some(98_000_001);
    guest_sess.solar_system_id = Some(30_000_142);

    // Rookie sentinel 1_704_067_200 returns rookie_help
    assert_eq!(
        handle_social(&ctx, &mut owner_sess, "XmppChatMgr", "GetRookieChannel", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::String("rookie_help".into())
    );

    // AddCharacterToChannel missing channelID -> InvalidArgument
    assert_eq!(
        handle_social(&ctx, &mut owner_sess, "XmppChatMgr", "AddCharacterToChannel", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // Adding another character to a non-existent channel -> ChannelAccessDenied
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String("channel:missing".into()), PyRep::Integer(guest_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );

    // Owner creates channel with password, then adds op_id via (cid, pw) tuple
    handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[
            PyRep::String("OpRoom".into()),
            PyRep::String("pw42".into()),
            PyRep::String("Initial MOTD".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let chan_id = "channel:oproom";

    // Guest cannot add op_id because guest is neither owner nor operator
    assert_eq!(
        handle_social(
            &ctx,
            &mut guest_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String(chan_id.into()), PyRep::Integer(op_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );

    // Guest joining via AddCharacterToChannel with wrong password -> InvalidChannelPassword, with right password -> ok
    assert_eq!(
        handle_social(
            &ctx,
            &mut guest_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String(chan_id.into()), PyRep::String("wrong".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChannelPassword"
    );
    handle_social(
        &ctx,
        &mut guest_sess,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String(chan_id.into()), PyRep::String("pw42".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("guest joins with password");

    // If op_id blocks owner_id in social_extras.blocked, owner cannot add op_id
    ctx.social_extras
        .lock()
        .unwrap()
        .blocked
        .entry(op_id)
        .or_default()
        .insert(owner_id);
    assert_eq!(
        handle_social(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String(chan_id.into()), PyRep::Integer(op_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    ctx.social_extras.lock().unwrap().blocked.clear();

    // Owner adds op_id -> succeeds
    let added = handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[
            PyRep::String(chan_id.into()),
            PyRep::Integer(op_id),
            PyRep::String("pw42".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(added.dict_get("characterID").and_then(|v| v.as_i64()), Some(op_id));

    // ResyncSystemChannelAccess in Abyssal system (32_000_001) omits local channel
    owner_sess.solar_system_id = Some(32_000_001);
    let resync_abyss = handle_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(chans) = resync_abyss.dict_get("channels").unwrap() else {
        panic!("expected channels list");
    };
    assert!(!chans
        .iter()
        .any(|c| c.as_str().unwrap_or("").starts_with("solarsystemid2:")));
}

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

    // Matching system, corp, alliance, fleet and delayed wormhole local -> all succeed
    for good_chan in [
        "solarsystemid2:30000142",
        "corpid:98000001",
        "allianceid:99000001",
        "fleetid:700001",
    ] {
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
