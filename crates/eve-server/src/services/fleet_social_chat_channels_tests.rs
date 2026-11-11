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

#[test]
fn chat_rename_motd_password_lang_destroy_and_acl_permissions() {
    use eve_machonet::SessionState;

    let ctx = test_ctx_with_clock(1_800_007_500);
    let kw = HashMap::new();
    let (owner_id, op1_id, op2_id, guest_id) = (90_000_011, 90_000_012, 90_000_013, 90_000_014);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name, ship) in [
            (owner_id, "ChanOwner", 100_000_111),
            (op1_id, "ChanOp1", 100_000_112),
            (op2_id, "ChanOp2", 100_000_113),
            (guest_id, "ChanGuest", 100_000_114),
        ] {
            let c = clone_char_with(snap, cid, name, 98_000_001, 30_000_142, ship);
            snap.characters.insert(cid, c);
            snap.accounts.insert(
                format!("chat_acct_{cid}"),
                eve_db::AccountRecord {
                    user_id: cid as u64,
                    username: format!("chat_acct_{cid}"),
                    password_token: "pw".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(owner_id as u64, 1, Some(owner_id), false, ctx.now_unix())
        .unwrap();

    let mk_sess = |cid: i64| -> SessionState {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(98_000_001);
        s.solar_system_id = Some(30_000_142);
        s
    };
    let mut owner_sess = mk_sess(owner_id);
    let mut op1_sess = mk_sess(op1_id);
    let mut op2_sess = mk_sess(op2_id);
    let mut guest_sess = mk_sess(guest_id);

    let created = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("FleetOpsRoom".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let chan_id = created.dict_get("channelID").unwrap().as_str().unwrap().to_string();

    // Missing channel & missing arg validation
    assert_eq!(
        handle_chat_and_social(&ctx, &mut owner_sess, "LSC", "RenameChannel", &[], &kw) // rpc-lint: allow test fixture
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "RenameChannel", // rpc-lint: allow test fixture
            &[PyRep::String("channel:missing".into()), PyRep::String("X".into())],
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
            &mut owner_sess,
            "XmppChatMgr",
            "SetMotd",
            &[PyRep::String("channel:missing".into()), PyRep::String("M".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelNotFound"
    );

    // Promote op1 & op2 via AccessControl(2), join guest
    for op in [op1_id, op2_id] {
        handle_chat_and_social(
            &ctx,
            &mut owner_sess,
            "LSC",
            "AccessControl",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(op), PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap();
    }
    handle_chat_and_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Non-operator guest rejected on RenameChannel, SetChannelMOTD, SetChannelPassword, SetChannelLanguageRestriction
    for method in [
        "RenameChannel",
        "SetChannelMOTD",
        "SetChannelPassword",
        "SetChannelLanguageRestriction",
    ] {
        let err = handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            method,
            &[PyRep::String(chan_id.clone()), PyRep::String("val".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "NotChannelOperator", "{method} must reject non-operator"); // rpc-lint: allow test fixture
    }

    // Operator succeeds on RenameChannel, SetChannelMOTD, SetChannelLanguageRestriction, SetChannelPassword
    let renamed = handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "RenameChannel", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone()), PyRep::String("RenamedOps".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(renamed.dict_get("name").and_then(|v| v.as_str()), Some("RenamedOps"));
    let motd_res = handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "SetChannelMOTD", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone()), PyRep::String("Stay aligned".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(motd_res.dict_get("motd").and_then(|v| v.as_str()), Some("Stay aligned"));
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "SetChannelLanguageRestriction", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone()), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // SetChannelPassword enforces password on unallowed guest, but allow_list(1) bypasses password
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "SetChannelPassword", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone()), PyRep::String("lock123".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let members_locked = handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "GetMembers",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        members_locked.dict_get("passwordRequired").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut guest_sess,
            "LSC",
            "JoinChannel",
            &[PyRep::String(chan_id.clone())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidChannelPassword"
    );
    // Allow-listed guest joins without password
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "AccessControl",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(guest_id), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle_chat_and_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Clear password with empty string -> passwordRequired becomes false and guest (even after ACL clear) joins freely
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "SetChannelPassword", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone()), PyRep::String(String::new())],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "AccessControl",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(guest_id), PyRep::Integer(-1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    // Default mode 0 clears allow_list, deny_list, and muted_until on guest
    handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "AccessControl",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(guest_id), PyRep::Integer(0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let members_cleared = handle_chat_and_social(
        &ctx,
        &mut op1_sess,
        "LSC",
        "GetMembers",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        members_cleared.dict_get("passwordRequired").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(matches!(
        members_cleared.dict_get("allowList"),
        Some(PyRep::List(l))
            if l.len() == 2
                && l.contains(&PyRep::Integer(op1_id))
                && l.contains(&PyRep::Integer(op2_id))
                && !l.contains(&PyRep::Integer(guest_id))
    ));
    assert!(matches!(
        members_cleared.dict_get("mutedList"),
        Some(PyRep::List(l)) if l.is_empty()
    ));
    // Cleared guest can now join without password & speak
    handle_chat_and_social(
        &ctx,
        &mut guest_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("unmuted now".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Non-owner operator cannot promote(2), transfer(3), GrantChannelOwnership, DestroyChannel, or ACL/ban owner or peer operator
    for (mode, target) in [(2, guest_id), (3, guest_id), (-2, owner_id), (-2, op2_id), (0, owner_id), (0, op2_id)] {
        let err = handle_chat_and_social(
            &ctx,
            &mut op1_sess,
            "LSC",
            "AccessControl",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(target), PyRep::Integer(mode)],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "NotChannelOwner");
    }
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut op1_sess,
            "XmppChatMgr",
            "GrantChannelOwnership",
            &[PyRep::String(chan_id.clone()), PyRep::Integer(op2_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOwner"
    );
    assert_eq!(
        handle_chat_and_social(&ctx, &mut op1_sess, "LSC", "DestroyChannel", &[PyRep::String(chan_id.clone())], &kw) // rpc-lint: allow test fixture
            .unwrap()
            .unwrap_err()
            .code,
        "NotChannelOwner"
    );

    // Owner bans & mutes op2 via GM role temporarily? Or tests AccessControl(3) ownership transfer clears restrictions
    ctx.db.with_mut_snapshot(|snap| {
        let acl = snap.chat_channel_acl.entry(chan_id.clone()).or_default();
        acl.deny_list.insert(op2_id);
        acl.muted_until.insert(op2_id, (ctx.now_unix() + 500, "test".into()));
    });
    handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "LSC",
        "AccessControl",
        &[PyRep::String(chan_id.clone()), PyRep::Integer(op2_id), PyRep::Integer(3)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let ch = snap.chat_channels.get(&chan_id).unwrap();
        assert_eq!(ch.owner_id, Some(op2_id));
        let acl = snap.chat_channel_acl.get(&chan_id).unwrap();
        assert!(!acl.deny_list.contains(&op2_id));
        assert!(!acl.muted_until.contains_key(&op2_id));
    });

    // New owner destroys channel -> emits OnChannelDeleted and removes channel + ACL
    let _ = ctx.drain_notifications_for_char(owner_id);
    let destroyed = handle_chat_and_social(
        &ctx,
        &mut op2_sess,
        "LSC",
        "DestroyChannel", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(destroyed, PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| !s.chat_channels.contains_key(&chan_id) && !s.chat_channel_acl.contains_key(&chan_id)));
    assert!(ctx
        .drain_notifications_for_char(owner_id)
        .iter()
        .any(|n| n.event == "OnChannelDeleted" && n.args == vec![PyRep::String(chan_id.clone())]));

    // System & org channels forbid RenameChannel, SetChannelPassword, GrantChannelOwnership, DestroyChannel
    for sys_chan in ["solarsystemid2:30000142", "corpid:98000001"] {
        handle_chat_and_social(&ctx, &mut owner_sess, "LSC", "JoinChannel", &[PyRep::String(sys_chan.into())], &kw)
            .unwrap()
            .unwrap();
        for method in ["RenameChannel", "SetChannelPassword", "DestroyChannel"] {
            assert_eq!(
                handle_chat_and_social(
                    &ctx,
                    &mut owner_sess,
                    "LSC",
                    method,
                    &[PyRep::String(sys_chan.into()), PyRep::String("x".into())],
                    &kw,
                )
                .unwrap()
                .unwrap_err()
                .code,
                "NotChannelOwner" // rpc-lint: allow test fixture
            );
        }
        assert_eq!(
            handle_chat_and_social(
                &ctx,
                &mut owner_sess,
                "XmppChatMgr",
                "GrantChannelOwnership",
                &[PyRep::String(sys_chan.into()), PyRep::Integer(op2_id)],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "NotChannelOwner"
        );
    }
}

#[test]
fn org_channel_dynamic_operators_npc_corp_alliance_executor_and_fleet_roles() {
    use eve_db::{corp_roles, AllianceRecord, CorporationRecord, FleetMemberRecord, FleetRecord, FleetRole};
    use eve_machonet::SessionState;

    const CORP_ROLE_CHAT_MANAGER: u64 = 1u64 << 38;
    let ctx = test_ctx_with_clock(1_800_007_600);
    let kw = HashMap::new();

    let (npc_corp_top, npc_corp_bot, player_corp, non_exec_corp, corp_2m) =
        (1_999_999_i64, 1_000_000_i64, 98_000_001_i64, 98_000_002_i64, 2_000_000_i64);
    let alliance_id = 99_000_001_i64;
    let fleet_id = 700_055_i64;

    let (c_npc_top, c_npc_bot, c_2m_ceo, c_chat_mgr, c_member, c_non_exec_dir, c_fc, c_wc, c_sc, c_sq_mem, c_boss) = (
        90_000_101, 90_000_102, 90_000_103, 90_000_104, 90_000_105,
        90_000_106, 90_000_107, 90_000_108, 90_000_109, 90_000_110, 90_000_111,
    );

    ctx.db.with_mut_snapshot(|snap| {
        for (corp_id, ceo) in [
            (npc_corp_top, c_npc_top),
            (npc_corp_bot, c_npc_bot),
            (corp_2m, c_2m_ceo),
            (player_corp, c_boss),
            (non_exec_corp, c_non_exec_dir),
        ] {
            let mut cr = CorporationRecord::default();
            cr.corporation_id = corp_id;
            cr.name = format!("Corp {corp_id}");
            cr.ceo_id = ceo;
            cr.alliance_id = if corp_id == player_corp || corp_id == non_exec_corp { Some(alliance_id) } else { None };
            snap.corporations.insert(corp_id, cr);
        }
        if let Some(cr) = snap.corporations.get_mut(&player_corp) {
            cr.members.extend([c_boss, c_chat_mgr, c_member]);
            cr.member_roles.insert(c_chat_mgr, CORP_ROLE_CHAT_MANAGER);
        }
        if let Some(cr) = snap.corporations.get_mut(&non_exec_corp) {
            cr.members.insert(c_non_exec_dir);
            cr.member_roles.insert(c_non_exec_dir, corp_roles::ROLE_DIRECTOR);
        }
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "TestAlly".into(),
                short_name: "TALY".into(),
                executor_corp_id: player_corp,
                member_corp_ids: HashSet::from([player_corp, non_exec_corp]),
            },
        );
        for (cid, corp, ally) in [
            (c_npc_top, npc_corp_top, None),
            (c_npc_bot, npc_corp_bot, None),
            (c_2m_ceo, corp_2m, None),
            (c_chat_mgr, player_corp, Some(alliance_id)),
            (c_member, player_corp, Some(alliance_id)),
            (c_non_exec_dir, non_exec_corp, Some(alliance_id)),
            (c_boss, player_corp, Some(alliance_id)),
            (c_fc, player_corp, Some(alliance_id)),
            (c_wc, player_corp, Some(alliance_id)),
            (c_sc, player_corp, Some(alliance_id)),
            (c_sq_mem, player_corp, Some(alliance_id)),
        ] {
            let mut ch = clone_char_with(snap, cid, &format!("P{cid}"), corp, 30_000_142, cid + 10_000);
            ch.alliance_id = ally;
            snap.characters.insert(cid, ch);
        }
        let mut fleet = FleetRecord {
            fleet_id,
            boss_id: c_boss,
            members: vec![c_boss, c_fc, c_wc, c_sc, c_sq_mem],
            wings: vec![1],
            squads: vec![2],
            free_move: true,
            is_registered: false,
            wing_details: HashMap::new(),
            squad_details: HashMap::new(),
            member_details: HashMap::new(),
            invites: HashSet::new(),
            motd: String::new(),
        };
        for (cid, role) in [
            (c_boss, FleetRole::SquadMember),
            (c_fc, FleetRole::FleetCommander),
            (c_wc, FleetRole::WingCommander),
            (c_sc, FleetRole::SquadCommander),
            (c_sq_mem, FleetRole::SquadMember),
        ] {
            fleet.member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 0,
                },
            );
        }
        snap.fleets.insert(fleet_id, fleet);
    });

    let mk_sess = |cid: i64, corp: i64, ally: Option<i64>, flt: Option<i64>, corp_role: u64| -> SessionState {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp);
        s.alliance_id = ally;
        s.fleet_id = flt;
        s.corp_role = corp_role;
        s.solar_system_id = Some(30_000_142);
        s
    };

    // NPC corp boundaries 1_000_000 and 1_999_999 CANNOT op even with ceo + u64::MAX session roles; 2_000_000 CEO CAN op
    for (cid, corp, should_op) in [
        (c_npc_bot, npc_corp_bot, false),
        (c_npc_top, npc_corp_top, false),
        (c_2m_ceo, corp_2m, true),
    ] {
        let mut s = mk_sess(cid, corp, None, None, u64::MAX);
        let chan = format!("corpid:{corp}");
        handle_chat_and_social(&ctx, &mut s, "LSC", "JoinChannel", &[PyRep::String(chan.clone())], &kw)
            .unwrap()
            .unwrap();
        let res = handle_chat_and_social(
            &ctx,
            &mut s,
            "LSC",
            "SetChannelMOTD", // rpc-lint: allow test fixture
            &[PyRep::String(chan), PyRep::String("Corp MOTD".into())],
            &kw,
        )
        .unwrap();
        assert_eq!(res.is_ok(), should_op, "corp {corp} op expectation failed");
    }

    // Player corp: DB CORP_ROLE_CHAT_MANAGER (1<<38) succeeds with session.corp_role=0; plain member fails; non-member fails
    let mut s_chat_mgr = mk_sess(c_chat_mgr, player_corp, Some(alliance_id), None, 0);
    let mut s_member = mk_sess(c_member, player_corp, Some(alliance_id), None, 0);
    let corp_chan = format!("corpid:{player_corp}");
    handle_chat_and_social(&ctx, &mut s_chat_mgr, "LSC", "JoinChannel", &[PyRep::String(corp_chan.clone())], &kw)
        .unwrap()
        .unwrap();
    handle_chat_and_social(
        &ctx,
        &mut s_chat_mgr,
        "LSC",
        "SetChannelMOTD", // rpc-lint: allow test fixture
        &[PyRep::String(corp_chan.clone()), PyRep::String("ChatMgr Rules".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut s_member,
            "LSC",
            "SetChannelMOTD", // rpc-lint: allow test fixture
            &[PyRep::String(corp_chan.clone()), PyRep::String("Unauthorized".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOperator"
    );

    // Alliance channel: executor corp CEO/ChatManager succeeds; non-executor corp Director in SAME alliance fails!
    let mut s_non_exec = mk_sess(c_non_exec_dir, non_exec_corp, Some(alliance_id), None, u64::MAX);
    let ally_chan = format!("allianceid:{alliance_id}");
    handle_chat_and_social(&ctx, &mut s_non_exec, "LSC", "JoinChannel", &[PyRep::String(ally_chan.clone())], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut s_non_exec,
            "LSC",
            "SetChannelMOTD", // rpc-lint: allow test fixture
            &[PyRep::String(ally_chan.clone()), PyRep::String("NonExec".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotChannelOperator"
    );
    handle_chat_and_social(
        &ctx,
        &mut s_chat_mgr,
        "LSC",
        "SetChannelMOTD", // rpc-lint: allow test fixture
        &[PyRep::String(ally_chan), PyRep::String("Exec Approved".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Fleet channel: boss, FleetCommander, WingCommander succeed; SquadCommander and SquadMember fail
    let fleet_chan = format!("fleetid:{fleet_id}");
    let mut s_boss = mk_sess(c_boss, player_corp, Some(alliance_id), Some(fleet_id), 0);
    handle_chat_and_social(&ctx, &mut s_boss, "LSC", "JoinChannel", &[PyRep::String(fleet_chan.clone())], &kw)
        .unwrap()
        .unwrap();
    for (cid, can_op) in [
        (c_boss, true),
        (c_fc, true),
        (c_wc, true),
        (c_sc, false),
        (c_sq_mem, false),
    ] {
        let mut sf = mk_sess(cid, player_corp, Some(alliance_id), Some(fleet_id), 0);
        let res = handle_chat_and_social(
            &ctx,
            &mut sf,
            "LSC",
            "SetChannelMOTD", // rpc-lint: allow test fixture
            &[PyRep::String(fleet_chan.clone()), PyRep::String(format!("Fleet MOTD {cid}"))],
            &kw,
        )
        .unwrap();
        assert_eq!(res.is_ok(), can_op, "fleet role op failed for {cid}");
    }
}

#[test]
fn mute_expiry_boundary_quasar_ban_message_limits_rate_reset_and_offline_target_auth() {
    use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
    use eve_db::CorporationRecord;
    use eve_machonet::{service_roles, SessionState};

    let (ctx, _clock) = test_ctx_and_clock(1_800_008_000);
    let kw = HashMap::new();
    let (owner_id, speaker_id, offline_corp_id) = (90_000_201, 90_000_202, 90_000_203);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = 98_000_001;
        corp.name = "Corp 98000001".into();
        corp.ceo_id = owner_id;
        corp.members.extend([owner_id, speaker_id]);
        snap.corporations.insert(98_000_001, corp);
        for (cid, corp, sys) in [
            (owner_id, 98_000_001, 30_000_142),
            (speaker_id, 98_000_001, 30_000_142),
            (offline_corp_id, 98_000_099, 30_000_142),
        ] {
            snap.characters.insert(cid, clone_char_with(snap, cid, &format!("U{cid}"), corp, sys, cid + 1000));
        }
    });

    let mut owner_sess = SessionState::new(1);
    owner_sess.user_id = Some(1);
    owner_sess.character_id = Some(owner_id);
    owner_sess.corporation_id = Some(98_000_001);
    owner_sess.solar_system_id = Some(30_000_142);

    let mut speaker_sess = SessionState::new(2);
    speaker_sess.user_id = Some(2);
    speaker_sess.character_id = Some(speaker_id);
    speaker_sess.corporation_id = Some(98_000_001);
    speaker_sess.solar_system_id = Some(30_000_142);

    let chan_id = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("BoundaryRoom".into())],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("channelID")
    .unwrap()
    .as_str()
    .unwrap()
    .to_string();
    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "JoinChannels", // rpc-lint: allow test fixture
        &[PyRep::Tuple(vec![PyRep::String(chan_id.clone())])],
        &kw,
    )
    .unwrap()
    .unwrap();

    // 2000 chars allowed vs 2001 chars rejected vs banned word rejected
    let ok_2000 = "a".repeat(2000);
    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String(ok_2000)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("a".repeat(2001))],
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
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("this has badword in it".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UserError"
    );

    // Exhaust remaining 19 sends of 20/sec window -> 21st fails -> +1s resets rate window!
    for _ in 0..19 {
        handle_chat_and_social(
            &ctx,
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("ping".into())],
            &kw,
        )
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("21st".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChatRateLimitExceeded"
    );
    ctx.advance_secs(1);
    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("after reset".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Global mute & exact expiry boundary (until = now + 60: t+59 muted, t+60 unmuted)
    let mut gm_sess = owner_sess.clone();
    gm_sess.role = service_roles::ROLE_GMS;
    let gm_mute = handle_chat_and_social(
        &ctx,
        &mut gm_sess,
        "XmppChatMgr",
        "GMMute",
        &[
            PyRep::Integer(speaker_id),
            PyRep::String("global".into()),
            PyRep::Integer(60),
            PyRep::String("global cooldown".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let expected_until = ctx.now_unix() + 60;
    assert_eq!(gm_mute.dict_get("mutedUntil").and_then(|v| v.as_i64()), Some(expected_until as i64));

    // Prefs fallback shows isMuted = true under global mute
    let prefs = handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "XmppChatMgr",
        "GetDeprecatedPrefsFallback",
        &[PyRep::String(chan_id.clone())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(pref_entries) = prefs else { panic!() };
    assert_eq!(pref_entries.len(), 1);
    assert_eq!(pref_entries[0].1.dict_get("isMuted").and_then(|v| v.as_bool()), Some(true));

    ctx.advance_secs(59);
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("still muted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MutedInChannel"
    );
    ctx.advance_secs(1); // now == until -> ! (now < until) -> unmuted!
    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("exact expiry ok".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Quasar active chat moderation ban -> CharacterChatBanned until expiry
    {
        let mut mutes = ctx.quasar_gateway.chat_moderation.mutes.write().unwrap();
        mutes.insert(speaker_id as u32, (ctx.now_unix() + 10, "GM chat ban".into()));
    }
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut speaker_sess,
            "LSC",
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("blocked by quasar".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterChatBanned"
    );
    ctx.advance_secs(10);
    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "SendMessage",
        &[PyRep::String(chan_id.clone()), PyRep::String("ban lifted".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Non-GM corp operator AddCharacterToChannel (target != caller reads snap.characters) + GM cross-system add
    handle_chat_and_social(&ctx, &mut owner_sess, "LSC", "JoinChannel", &[PyRep::String("corpid:98000001".into())], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String("corpid:98000001".into()), PyRep::Integer(offline_corp_id)],
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
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("corpid:98000001".into()), PyRep::Integer(speaker_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    gm_sess.solar_system_id = Some(30_000_144); // GM is elsewhere, offline_corp_id IS in 30_000_142
    handle_chat_and_social(
        &ctx,
        &mut gm_sess,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("solarsystemid2:30000142".into()), PyRep::Integer(offline_corp_id)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Non-GM operator cannot add another character to a non-existent channel
    assert_eq!(
        handle_chat_and_social(
            &ctx,
            &mut owner_sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::String("channel:does_not_exist_yet".into()), PyRep::Integer(speaker_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ChannelAccessDenied"
    );

    handle_chat_and_social(
        &ctx,
        &mut speaker_sess,
        "LSC",
        "LeaveChannels", // rpc-lint: allow test fixture
        &[PyRep::String(chan_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
}
