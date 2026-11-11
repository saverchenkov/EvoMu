//! Edge-case unit tests for `fleet_social_chat` player-owned channels, ACLs, moderation, and message validation.

use super::handle_chat_and_social;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn char_session(conn_id: u64, char_id: i64, corp_id: i64, system_id: u32) -> SessionState {
    let mut s = SessionState::new(conn_id);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(system_id);
    s
}

#[test]
fn player_channel_create_ownership_transfer_and_system_channel_guards() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let kw = HashMap::new();
    let mut owner = char_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut other = char_session(2, 90_000_002, 98_000_001, 30_000_142);

    // Empty name rejected
    let err = handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("   ".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    // Valid creation
    let created = handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("FleetOpsEdge".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let chan_id = created
        .dict_get("channelID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert_eq!(
        created.dict_get("ownerID").and_then(|v| v.as_i64()),
        Some(90_000_001)
    );

    // Duplicate channel name rejected
    let dup = handle_chat_and_social(
        &ctx,
        &mut other,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("FleetOpsEdge".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup.code, "ChannelAlreadyExists");

    // GrantChannelOwnership arg validation & error paths
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String("player_999999".into()), PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );

    // System/org channel ownership transfer denied
    handle_chat_and_social(
        &ctx,
        &mut owner,
        "LSC",
        "JoinChannel",
        &[PyRep::String("local".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String("local".into()), PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // Non-owner cannot transfer ownership
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut other,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // Owner transfers ownership -> clears ACL restrictions & sets transferred=true
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl
            .entry(chan_id.to_ascii_lowercase())
            .or_default()
            .deny_list
            .insert(90_000_002);
    });
    let res = handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(res.dict_get("transferred").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(res.dict_get("ownerID").and_then(|v| v.as_i64()), Some(90_000_002));
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.chat_channel_acl[&chan_id.to_ascii_lowercase()]
                .deny_list
                .contains(&90_000_002)
        );
    });
}

#[test]
fn chat_moderation_motd_mute_unmute_and_send_validation_edges() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        clone_char_with(snap, 90_000_002, "Wingman", 98_000_001, 30_000_142, 0);
    });
    let mut owner = char_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut member = char_session(2, 90_000_002, 98_000_001, 30_000_142);

    let created = handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("ModEdge".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let chan_id = created.dict_get("channelID").unwrap().as_str().unwrap().to_string();

    // Non-member SendMessage rejected
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("hi".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );

    // Join member
    handle_chat_and_social(
        &ctx,
        &mut member,
        "LSC",
        "JoinChannel",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // SetMotd unknown -> ChannelNotFound; non-op -> NotChannelOperator
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "XmppChatMgr",
            "SetMotd",
            &[PyRep::String("no_such_chan".into()), PyRep::String("motd".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "XmppChatMgr",
            "SetMotd",
            &[PyRep::String(chan_id.clone()), PyRep::String("motd".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOperator"
    );

    // Non-op GMMute rejected; non-GM op trying to mute owner rejected
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "XmppChatMgr",
            "GMMute",
            &[PyRep::Integer(90_000_001), PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChatAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channels.get_mut(&chan_id).unwrap().operators.insert(90_000_002);
    });
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "XmppChatMgr",
            "GMMute",
            &[PyRep::Integer(90_000_001), PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );

    // Owner mutes member -> SendMessage returns MutedInChannel; GMUnmute restores sending
    handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "GMMute",
        &[
            PyRep::Integer(90_000_002),
            PyRep::String(chan_id.clone()),
            PyRep::Integer(300),
            PyRep::String("spam".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("hello".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MutedInChannel"
    );
    handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "GMUnmute",
        &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Message length / banned word / abyssal local / rate limit
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String(String::new())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChatMessageLength"
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("x".repeat(2_001))],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChatMessageLength"
    );
    let banned_err = handle_chat_and_social(
        &ctx,
        &mut member,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("contains badword here".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(banned_err.code, "UserError");
    assert!(banned_err.message.contains("ChatContentInvalidBannedWords"));

    let mut abyssal_sess = char_session(3, 90_000_002, 98_000_001, 32_000_001);
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut abyssal_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String("solarsystemid2:32000001".into()), PyRep::String("anyone?".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );

    // 20 msgs allowed in same unix second, 21st hits ChatRateLimitExceeded
    for i in 0..20 {
        handle_chat_and_social(
            &ctx,
            &mut member,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String(format!("msg {i}"))],
            &kw,
        )
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut member,
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
}

#[test]
fn channel_rename_destroy_rookie_age_and_custom_hostname_edges() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let kw = HashMap::new();
    let mut owner = char_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut other = char_session(2, 90_000_002, 98_000_001, 30_000_142);

    // Custom XmppChatMgr Hostname from service_state_blobs
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("xmpp:hostname".into(), "xmpp.staging.local".into());
    });
    let host = handle_chat_and_social(&ctx, &mut owner, "XmppChatMgr", "Hostname", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(host.as_str(), Some("xmpp.staging.local"));

    // Rookie channel present within 30d, None after 30d
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.create_date_unix = 1_800_010_000 - (5 * 86_400);
        }
    });
    assert_eq!(
        handle_chat_and_social(&ctx, &mut owner, "XmppChatMgr", "GetRookieChannel", &[], &kw)
            .unwrap()
            .unwrap()
            .as_str(),
        Some("rookie_help")
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.create_date_unix = 1_800_010_000 - (31 * 86_400);
        }
    });
    assert_eq!(
        handle_chat_and_social(&ctx, &mut owner, "XmppChatMgr", "GetRookieChannel", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // AccessControl & Invite error/CSPA guards using V24 LSC RPCs
    let created = handle_chat_and_social(
        &ctx,
        &mut owner,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("TempChan".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let chan_id = created.dict_get("channelID").unwrap().as_str().unwrap().to_string();

    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut other,
            "LSC",
            "AccessControl",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOperator"
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "LSC",
            "AccessControl",
            &[PyRep::String("missing_chan".into()), PyRep::Integer(90_000_002), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );

    // Owner Invite requires recipient, deducts CSPA charge when configured, and rejects blocked senders
    ctx.db.with_mut_snapshot(|snap| {
        clone_char_with(snap, 90_000_002, "Recipient", 98_000_001, 30_000_142, 0);
        snap.character_cspa_charges
            .insert(90_000_002, eve_db::IskCents::from_isk(2_500));
        snap.characters.get_mut(&90_000_001).unwrap().balance = eve_db::IskCents::from_isk(1_000);
    });
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner,
            "LSC",
            "Invite",
            &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().balance = eve_db::IskCents::from_isk(10_000);
    });
    let inv = handle_chat_and_social(
        &ctx,
        &mut owner,
        "LSC",
        "Invite",
        &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(inv.dict_get("invited").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters.get(&90_000_001).unwrap().balance,
            eve_db::IskCents::from_isk(7_500)
        );
    });

    // Direct unit checks for `resolve_channel_key`, `apply_post_access_control_social_op`,
    // `clear_channel_acl_restrictions`, and `clear_social_extras_channel_restrictions`
    ctx.db.with_snapshot(|snap| {
        assert_eq!(super::resolve_channel_key(snap, &chan_id.to_uppercase()), chan_id);
        assert_eq!(super::resolve_channel_key(snap, "nonexistent_chan"), "nonexistent_chan");
    });
    super::apply_post_access_control_social_op(
        &ctx,
        &chan_id,
        super::PostAccessControlSocialOp::Block(90_000_002),
    );
    assert!(super::is_character_blocked_in_social_extras(&ctx, &chan_id, 90_000_002));
    super::apply_post_access_control_social_op(
        &ctx,
        &chan_id,
        super::PostAccessControlSocialOp::Mute(90_000_002, 999_999),
    );
    super::apply_post_access_control_social_op(
        &ctx,
        &chan_id,
        super::PostAccessControlSocialOp::AllowAndUnmute(90_000_002),
    );
    assert!(!super::is_character_blocked_in_social_extras(&ctx, &chan_id, 90_000_002));
    super::apply_post_access_control_social_op(
        &ctx,
        &chan_id,
        super::PostAccessControlSocialOp::Block(90_000_002),
    );
    super::apply_post_access_control_social_op(
        &ctx,
        &chan_id,
        super::PostAccessControlSocialOp::ClearRestrictions(90_000_002),
    );
    assert!(!super::is_character_blocked_in_social_extras(&ctx, &chan_id, 90_000_002));
}
