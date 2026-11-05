//! Unit tests for `fleet_social_chat` player channels, join/leave, messages, invites, and ACLs.

use super::super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_machonet::SessionState;
use std::collections::HashMap;

#[test]
fn chat_channels_creation_join_leave_messages_and_acl_lifecycle() {
    let ctx = test_ctx_with_clock(1_800_007_000);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let c1 = clone_char_with(snap, 90_000_001, "OwnerPilot", 98_000_001, 30_000_142, 100_000_001);
        let c2 = clone_char_with(snap, 90_000_002, "GuestPilot", 98_000_001, 30_000_142, 100_000_002);
        snap.characters.insert(90_000_001, c1);
        snap.characters.insert(90_000_002, c2);
    });

    let mut owner_sess = SessionState::new(1);
    owner_sess.user_id = Some(90_000_001);
    owner_sess.character_id = Some(90_000_001);
    owner_sess.corporation_id = Some(98_000_001);
    owner_sess.solar_system_id = Some(30_000_142);

    let mut guest_sess = SessionState::new(2);
    guest_sess.user_id = Some(90_000_002);
    guest_sess.character_id = Some(90_000_002);
    guest_sess.corporation_id = Some(98_000_001);
    guest_sess.solar_system_id = Some(30_000_142);

    // CreatePlayerOwnedChannel with password + MOTD
    let created = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[
            PyRep::String("Fleet Intel".into()),
            PyRep::String("secret123".into()),
            PyRep::String("Report neutral fleets".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(cr_kv) = created else { panic!() };
    let chan_id = cr_kv.get("channelID").unwrap().as_str().unwrap().to_string();
    assert_eq!(chan_id, "channel:fleet_intel");

    // Wrong password rejected; right password succeeds
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "JoinChannel",
            &[PyRep::String(chan_id.clone()), PyRep::String("wrong".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChannelPassword"
    );
    let joined = handle_chat_and_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String(chan_id.clone()), PyRep::String("secret123".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(jn_kv) = joined else { panic!() };
    assert_eq!(jn_kv.get("memberCount").unwrap().as_i64().unwrap(), 2);

    // GetChannels / GetMembers
    assert!(handle_chat_and_social(&ctx, &mut guest_sess, "LSC", "GetChannels", &[], &kw)
        .unwrap()
        .is_ok());
    let mems = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "GetMembers",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(m_kv) = mems else { panic!() };
    assert_eq!(m_kv.get("passwordRequired").unwrap(), &PyRep::Bool(true));

    // XmppChatMgr.SetMotd
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "SetMotd",
        &[PyRep::String(chan_id.clone()), PyRep::String("Updated MOTD".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // AccessControl: mute (-1) blocks guest SendMessage ("MutedInChannel"), then allow (1) unmutes
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(chan_id.clone()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(-1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("o7".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MutedInChannel"
    );
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(chan_id.clone()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Valid SendMessage succeeds
    let sent = handle_chat_and_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("Spike +12 in Tama".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(s_kv) = sent else { panic!() };
    assert_eq!(s_kv.get("sent").unwrap(), &PyRep::Bool(true));

    // Empty message rejected
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChatMessageLength"
    );

    // Rate limit (>20 msgs in same second)
    for _ in 0..20 {
        let _ = handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("ping".into())],
            &kw,
        )
        .unwrap();
    }
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("overflow".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChatRateLimitExceeded"
    );

    // Ban (-2) kicks and blocks rejoining, then reset (0) clears ban
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(chan_id.clone()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(-2),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[
            PyRep::String(chan_id.clone()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    // GMMute & GMUnmute & Promote op (2) & Transfer ownership (3)
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "GMMute",
        &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone()), PyRep::Integer(600)],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "GMUnmute",
        &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // LeaveChannel & CreatePrivateChannel
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "LeaveChannel",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let inv = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePrivateChannel",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(inv_kv) = inv else { panic!() };
    assert_eq!(inv_kv.get("channelID").unwrap().as_str().unwrap(), "private:90000001:90000002");
}
