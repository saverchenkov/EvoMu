//! Unit tests for `fleet_social_chat` XMPP/LSC channels, moderation, delayed local, and contacts/CSPA.

#[cfg(test)]
#[path = "fleet_social_chat_channels_tests.rs"]
mod fleet_social_chat_channels_tests;

#[cfg(test)]
#[path = "fleet_social_chat_auth_ops_cov_tests.rs"]
mod fleet_social_chat_auth_ops_cov_tests;

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};

#[test]
fn chat_auth_help_rookie_prefs_resync_and_delayed_pruning() {
    let ctx = test_ctx_with_clock(1_800_005_000);
    let kw = HashMap::new();

    // Unauthenticated rejected
    let mut unauth = SessionState::new(1);
    assert_eq!(
        handle_chat_and_social(&ctx, &mut unauth, "chatAuthenticationService", "GetAuthenticationToken", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut session = SessionState::new(2);
    session.user_id = Some(90_000_001);
    session.character_id = Some(90_000_001);
    session.corporation_id = Some(98_000_001);
    session.alliance_id = Some(99_000_001);
    session.fleet_id = Some(700_001);
    session.solar_system_id = Some(31_000_005); // Wormhole delayed local

    // XMPP auth token & hostname
    let tok = handle_chat_and_social(&ctx, &mut session, "chatAuthenticationService", "GetAuthenticationToken", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(tok.as_str().unwrap(), "evomu-xmpp-token-90000001");
    let host = handle_chat_and_social(&ctx, &mut session, "XmppChatMgr", "Hostname", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(host.as_str().unwrap(), "chat.evomu.local");

    // GetHelpChannel across languages
    for (lang, expected) in [
        ("de", "help_de"),
        ("fr", "help_fr"),
        ("ru", "help_ru"),
        ("ja", "help_ja"),
        ("zh", "help_zh"),
        ("ko", "help_ko"),
        ("es", "help_es"),
        ("en-us", "help_en"),
    ] {
        let ch = handle_chat_and_social(
            &ctx,
            &mut session,
            "XmppChatMgr",
            "GetHelpChannel",
            &[PyRep::String(lang.into())],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ch.as_str().unwrap(), expected);
    }

    // GetRookieChannel: recent character gets rookie_help, veteran (>30d) gets None
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.create_date_unix = 1_800_005_000 - 5 * 86_400;
        }
    });
    assert_eq!(
        handle_chat_and_social(&ctx, &mut session, "XmppChatMgr", "GetRookieChannel", &[], &kw)
            .unwrap()
            .unwrap()
            .as_str()
            .unwrap(),
        "rookie_help"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.create_date_unix = 1_800_005_000 - 45 * 86_400;
        }
    });
    assert_eq!(
        handle_chat_and_social(&ctx, &mut session, "XmppChatMgr", "GetRookieChannel", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // EnsureResourceWarsChannelExists & ResyncSystemChannelAccess in J-space (delayed local)
    let rw = handle_chat_and_social(
        &ctx,
        &mut session,
        "XmppChatMgr",
        "EnsureResourceWarsChannelExists",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(rw_kv) = rw else { panic!() };
    assert_eq!(rw_kv.get("channelID").unwrap().as_str().unwrap(), "resourcewars:31000005");

    let rs = handle_chat_and_social(
        &ctx,
        &mut session,
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(rs_kv) = rs else { panic!() };
    assert_eq!(rs_kv.get("channelID").unwrap().as_str().unwrap(), "solarsystemid2:31000005");
    assert_eq!(rs_kv.get("isDelayedLocal").unwrap(), &PyRep::Bool(true));

    // Seed delayed speaker witness and jump to Jita (30_000_142) -> Resync prunes old delayed local
    ctx.social_extras
        .lock()
        .unwrap()
        .delayed_local_speakers
        .insert("solarsystemid2:31000005".into(), HashSet::from([90_000_002]));
    session.solar_system_id = Some(30_000_142);
    let rs2 = handle_chat_and_social(
        &ctx,
        &mut session,
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(rs2_kv) = rs2 else { panic!() };
    assert_eq!(rs2_kv.get("channelID").unwrap().as_str().unwrap(), "solarsystemid2:30000142");
    assert_eq!(rs2_kv.get("isDelayedLocal").unwrap(), &PyRep::Bool(false));
    assert!(
        ctx.social_extras
            .lock()
            .unwrap()
            .delayed_local_speakers
            .get("solarsystemid2:31000005")
            .is_some_and(|s| s.is_empty())
    );

    // GetDeprecatedPrefsFallback returns joined channels
    let prefs = handle_chat_and_social(
        &ctx,
        &mut session,
        "XmppChatMgr",
        "GetDeprecatedPrefsFallback",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(pref_entries) = prefs else { panic!() };
    assert!(pref_entries.iter().any(|(k, _)| k.as_str() == Some("solarsystemid2:30000142")));
}

#[test]
fn chat_contacts_settings_cspa_charges_notes_and_extract_ids() {
    let ctx = test_ctx_with_clock(1_800_006_000);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let c1 = clone_char_with(snap, 90_000_001, "Alpha", 98_000_001, 30_000_142, 100_000_001);
        let c2 = clone_char_with(snap, 90_000_012, "Stranger", 98_000_999, 30_000_142, 100_000_012);
        snap.characters.insert(90_000_001, c1);
        snap.characters.insert(90_000_012, c2);
    });

    let mut session = SessionState::new(10);
    session.user_id = Some(90_000_001);
    session.character_id = Some(90_000_001);

    // GetSettingsInfo + LogSettings
    assert!(handle_chat_and_social(&ctx, &mut session, "charMgr", "GetSettingsInfo", &[], &kw)
        .unwrap()
        .is_ok());

    let mut target_sess = SessionState::new(11);
    target_sess.user_id = Some(90_000_012);
    target_sess.character_id = Some(90_000_012);
    let log_res = handle_chat_and_social(
        &ctx,
        &mut target_sess,
        "charMgr",
        "LogSettings",
        &[PyRep::Real(5_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!((log_res.as_f64().unwrap() - 5_000.0).abs() < 1e-6);

    // CSPAChargedAction & CSPAChargedActionForMany deduct 5_000 + default 2_950 = 7_950 ISK
    let initial_bal = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance.0);
    let single_fee = handle_chat_and_social(
        &ctx,
        &mut session,
        "charMgr",
        "CSPAChargedAction",
        &[PyRep::Integer(90_000_012)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!((single_fee.as_f64().unwrap() - 5_000.0).abs() < 1e-6);

    let many_fee = handle_chat_and_social(
        &ctx,
        &mut session,
        "charMgr",
        "CSPAChargedActionForMany",
        &[PyRep::List(vec![PyRep::Integer(90_000_012), PyRep::Integer(90_000_013)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!((many_fee.as_f64().unwrap() - 7_950.0).abs() < 1e-6);
    let after_bal = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).unwrap().balance.0);
    assert_eq!(initial_bal - after_bal, IskCents::from_isk(12_950).0);

    // Contacts delegation: AddContact -> stranger becomes friendly (0 ISK CSPA!)
    handle_chat_and_social(
        &ctx,
        &mut target_sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(90_000_001), PyRep::Real(10.0), PyRep::Integer(0), PyRep::Bool(false), PyRep::Bool(false)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let free_fee = handle_chat_and_social(
        &ctx,
        &mut session,
        "charMgr",
        "CSPAChargedAction",
        &[PyRep::Integer(90_000_012)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!((free_fee.as_f64().unwrap() - 0.0).abs() < 1e-6);

    assert!(handle_chat_and_social(&ctx, &mut session, "charMgr", "GetContactList", &[], &kw)
        .unwrap()
        .is_ok());

    // SetNote & GetNote roundtrip + extract_id_list helper + unmatched
    handle_chat_and_social(
        &ctx,
        &mut session,
        "charMgr",
        "SetNote",
        &[PyRep::Integer(90_000_012), PyRep::String("Cyno alt".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let note = handle_chat_and_social(
        &ctx,
        &mut session,
        "charMgr",
        "GetNote",
        &[PyRep::Integer(90_000_012)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(note.as_str().unwrap(), "Cyno alt");

    assert_eq!(extract_id_list(Some(&PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20)]))), vec![10, 20]);
    assert_eq!(extract_id_list(Some(&PyRep::Integer(42))), vec![42]);
    assert!(extract_id_list(None).is_empty());
    assert!(handle_chat_and_social(&ctx, &mut session, "other", "UnknownMethod", &[], &kw).is_none());
}

#[test]
fn xmpp_iq_reply_includes_query_child_for_disco_muc_and_expiring_record() {
    let bind = xmpp_iq_reply("<iq type='set' id='iq-1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></iq>", "iq-1");
    assert!(bind.contains("<bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>"));

    let disco_forme = xmpp_iq_reply(
        "<iq type='get' id='iq-2' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node='forme'></query></iq>",
        "iq-2",
    );
    assert!(disco_forme.contains("xmlns='http://jabber.org/protocol/disco#items'"));
    assert!(disco_forme.contains("node='forme'"));
    assert!(disco_forme.contains("<query"));

    let disco_info = xmpp_iq_reply(
        "<iq type='get' id='iq-3'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>",
        "iq-3",
    );
    assert!(disco_info.contains("<query xmlns='http://jabber.org/protocol/disco#info'>"));
    assert!(disco_info.contains("feature var='http://jabber.org/protocol/muc'"));

    let muc_admin = xmpp_iq_reply(
        "<iq type='get' id='iq-4'><query xmlns='http://jabber.org/protocol/muc#admin'/></iq>",
        "iq-4",
    );
    assert!(muc_admin.contains("<query xmlns='http://jabber.org/protocol/muc#admin'/>"));

    let muc_owner = xmpp_iq_reply(
        "<iq type='get' id='iq-5'><query xmlns='http://jabber.org/protocol/muc#owner'/></iq>",
        "iq-5",
    );
    assert!(muc_owner.contains("<query xmlns='http://jabber.org/protocol/muc#owner'/>"));

    let exp = xmpp_iq_reply(
        "<iq type='get' id='iq-6'><query xmlns='urn:xmpp:expiring_record#search'/></iq>",
        "iq-6",
    );
    assert!(exp.contains("<query xmlns='urn:xmpp:expiring_record#search'/>"));

    let fallback = xmpp_iq_reply("<iq type='get' id='iq-7'><ping xmlns='urn:xmpp:ping'/></iq>", "iq-7");
    assert_eq!(fallback, "<iq type='result' id='iq-7'/>");
}

#[test]
fn xmpp_iq_reply_with_ctx_discovers_player_channels_by_name_forme_and_joins_by_xmpp_room_jid() {
    let ctx = test_ctx_with_clock(1_800_006_000);
    let kw = HashMap::new();
    let mut owner_sess = SessionState::new(101);
    owner_sess.user_id = Some(90_000_001);
    owner_sess.character_id = Some(90_000_001);

    let created = handle_chat_and_social(
        &ctx,
        &mut owner_sess,
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        &[PyRep::String("Playtest Alpha".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(info_kv) = created else {
        panic!("expected CreatePlayerOwnedChannel KeyVal");
    };
    assert_eq!(
        info_kv.get("channelID").and_then(|v| v.as_str()),
        Some("channel:playtest_alpha")
    );

    // 1. `disco#items` with `node="byname/Playtest Alpha"` returns `<item jid='player_playtest_alpha@conference.chat.evomu.local' name='Playtest Alpha'/>`
    let byname = xmpp_iq_reply_with_ctx(
        Some(&ctx),
        "<iq type='get' id='iq-byname' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node=\"byname/Playtest Alpha\"/></iq>",
        "iq-byname",
    );
    assert!(byname.contains("node='byname/Playtest Alpha'"));
    assert!(byname.contains(
        "<item jid='player_playtest_alpha@conference.chat.evomu.local' name='Playtest Alpha'/>"
    ));

    // 2. `disco#items` with `node='forme'` includes the player-created channel with `node='owner'`
    let forme = xmpp_iq_reply_with_ctx(
        Some(&ctx),
        "<iq type='get' id='iq-forme' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node='forme'/></iq>",
        "iq-forme",
    );
    assert!(forme.contains(
        "<item jid='player_playtest_alpha@conference.chat.evomu.local' name='Playtest Alpha' node='owner'/>"
    ));

    // 3. `disco#info` on `player_playtest_alpha@conference.chat.evomu.local` returns `name='Playtest Alpha'`
    let info = xmpp_iq_reply_with_ctx(
        Some(&ctx),
        "<iq type='get' id='iq-info' to='player_playtest_alpha@conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#info'/></iq>",
        "iq-info",
    );
    assert!(info.contains("name='Playtest Alpha'"));

    // 4. Joining via LSC using the XMPP room name `player_playtest_alpha` resolves to `channel:playtest_alpha`
    let mut joiner_sess = SessionState::new(102);
    joiner_sess.user_id = Some(90_000_002);
    joiner_sess.character_id = Some(90_000_002);
    let joined = handle_chat_and_social(
        &ctx,
        &mut joiner_sess,
        "LSC",
        "JoinChannel",
        &[PyRep::String("player_playtest_alpha".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(joined_kv) = joined else {
        panic!("expected JoinChannel KeyVal");
    };
    assert_eq!(
        joined_kv.get("channelID").and_then(|v| v.as_str()),
        Some("channel:playtest_alpha")
    );
}

#[test]
fn social_extras_leaf_lock_order_and_poison_recovery_across_chat_ops() {
    let ctx = test_ctx_with_clock(1_800_006_000);
    let kw = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).cloned().unwrap();
        let mut p1 = base.clone();
        p1.solar_system_id = 31_000_005;
        snap.characters.insert(90_000_001, p1);
        let mut p2 = clone_char_with(snap, 90_000_002, "ChatPilot2", 98_000_001, 31_000_005, 100_000_002);
        p2.alliance_id = Some(99_000_001);
        p2.solar_system_id = 31_000_005;
        snap.characters.insert(90_000_002, p2);
        snap.character_cspa_charges
            .insert(90_000_002, IskCents::from_f64_isk(500.0));
    });

    // Poison `social_extras` to verify `.unwrap_or_else(|e| e.into_inner())` recovery across all handlers.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = ctx.social_extras.lock().unwrap();
        panic!("simulate social_extras poison");
    }));
    assert!(ctx.social_extras.is_poisoned());

    let mut s1 = SessionState::new(10);
    s1.user_id = Some(90_000_001);
    s1.character_id = Some(90_000_001);
    s1.corporation_id = Some(98_000_001);
    s1.solar_system_id = Some(31_000_005);

    let mut s2 = SessionState::new(11);
    s2.user_id = Some(90_000_002);
    s2.character_id = Some(90_000_002);
    s2.corporation_id = Some(98_000_001);
    s2.solar_system_id = Some(31_000_005);

    // Delayed local JoinChannel + AddCharacterToChannel + SendMessage
    let join_wh = handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "JoinChannel",
        &[PyRep::String("solarsystemid2:31000005".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(join_wh.dict_get("memberCount").and_then(|v| v.as_i64()), Some(0));
    let add_wh = handle_chat_and_social(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("solarsystemid2:31000005".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(add_wh.dict_get("memberCount").and_then(|v| v.as_i64()), Some(0));
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "SendMessage",
        &[
            PyRep::String("solarsystemid2:31000005".into()),
            PyRep::String("wh ping".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let join_wh_after = handle_chat_and_social(
        &ctx,
        &mut s2,
        "LSC",
        "JoinChannel",
        &[PyRep::String("solarsystemid2:31000005".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(join_wh_after.dict_get("memberCount").and_then(|v| v.as_i64()), Some(1));
    let add_wh_after = handle_chat_and_social(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("solarsystemid2:31000005".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(add_wh_after.dict_get("memberCount").and_then(|v| v.as_i64()), Some(1));
    assert!(ctx.db.with_snapshot(|snap| {
        let w = snap
            .fleet_and_social_ops
            .wh_local_witnesses
            .get("solarsystemid2:31000005:90000001")
            .unwrap();
        w.contains(&90_000_002) && !w.contains(&90_000_001)
    }));

    // Direct helper coverage for is_character_blocked_in_social_extras, is_character_blocked_in_acl,
    // clear_channel_acl_restrictions, clear_social_extras_channel_restrictions, and apply_post_access_control_social_op
    {
        let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
        s.blocked_in_channel
            .entry("channel:other".into())
            .or_default()
            .insert(90_000_002);
        s.blocked_in_channel
            .entry("channel:leaf-acl".into())
            .or_default()
            .insert(90_000_099);
        s.muted_in_channel
            .insert(("channel:other".into(), 90_000_002), (1_900_000_000, "m".into()));
        s.muted_in_channel
            .insert(("channel:leaf-acl".into(), 90_000_099), (1_900_000_000, "m".into()));
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl
            .entry("channel:other".into())
            .or_default()
            .deny_list
            .insert(90_000_002);
        snap.chat_channel_acl
            .entry("channel:leaf-acl".into())
            .or_default()
            .deny_list
            .insert(90_000_099);
    });
    assert!(!is_character_blocked_in_social_extras(&ctx, "channel:leaf-acl", 90_000_002));
    assert!(ctx.db.with_snapshot(|snap| !is_character_blocked_in_acl(snap, "channel:leaf-acl", 90_000_002)));

    // Player channel AccessControl modes (-2, -1, 1, 0, 3) + GrantChannelOwnership + Invite
    let join_leaf = handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "JoinChannel",
        &[PyRep::String("channel:leaf-acl".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(join_leaf.dict_get("memberCount").and_then(|v| v.as_i64()), Some(1));
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(-2),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(is_character_blocked_in_social_extras(&ctx, "channel:leaf-acl", 90_000_002));
    assert!(ctx.db.with_snapshot(|snap| is_character_blocked_in_acl(snap, "channel:leaf-acl", 90_000_002)));

    // Test social-only block vs ACL-only block across JoinChannel, AddCharacterToChannel, SendMessage, ResyncSystemChannelAccess
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl
            .get_mut("channel:leaf-acl")
            .unwrap()
            .deny_list
            .remove(&90_000_002);
        snap.chat_channels
            .get_mut("channel:leaf-acl")
            .unwrap()
            .members
            .insert(90_000_002);
    });
    assert!(handle_chat_and_social(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String("channel:leaf-acl".into())], &kw).unwrap().is_err());
    assert!(handle_chat_and_social(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String("channel:leaf-acl".into()), PyRep::Integer(90_000_002)], &kw).unwrap().is_err());
    assert!(handle_chat_and_social(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String("channel:leaf-acl".into()), PyRep::String("hi".into())], &kw).unwrap().is_err());

    // Now flip to ACL-only block (not in social_extras)
    ctx.social_extras
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .blocked_in_channel
        .get_mut("channel:leaf-acl")
        .unwrap()
        .remove(&90_000_002);
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl
            .get_mut("channel:leaf-acl")
            .unwrap()
            .deny_list
            .insert(90_000_002);
    });
    assert!(handle_chat_and_social(&ctx, &mut s2, "LSC", "JoinChannel", &[PyRep::String("channel:leaf-acl".into())], &kw).unwrap().is_err());
    assert!(handle_chat_and_social(&ctx, &mut s1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String("channel:leaf-acl".into()), PyRep::Integer(90_000_002)], &kw).unwrap().is_err());
    assert!(handle_chat_and_social(&ctx, &mut s2, "LSC", "SendMessage", &[PyRep::String("channel:leaf-acl".into()), PyRep::String("hi".into())], &kw).unwrap().is_err());

    // ResyncSystemChannelAccess: social-blocked vs ACL-blocked vs GM override
    ctx.social_extras
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .blocked_in_channel
        .entry("corpid:98000001".into())
        .or_default()
        .insert(90_000_002);
    handle_chat_and_social(&ctx, &mut s2, "XmppChatMgr", "ResyncSystemChannelAccess", &[], &kw).unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|snap| !snap.chat_channels.get("corpid:98000001").is_some_and(|c| c.members.contains(&90_000_002))));
    s2.role = eve_machonet::service_roles::ROLE_GMH;
    handle_chat_and_social(&ctx, &mut s2, "XmppChatMgr", "ResyncSystemChannelAccess", &[], &kw).unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.chat_channels.get("corpid:98000001").is_some_and(|c| c.members.contains(&90_000_002))));
    s2.role = 0;

    // AccessControl mode -1 (Mute), mode 1 (AllowAndUnmute), mode 0 (Clear), mode 3 (Transfer ownership)
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(-1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).muted_in_channel.contains_key(&("channel:leaf-acl".into(), 90_000_002)));
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(!ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).muted_in_channel.contains_key(&("channel:leaf-acl".into(), 90_000_002)));
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).muted_in_channel.insert(("channel:leaf-acl".into(), 90_000_002), (1_900_000_000, "m".into()));
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "AccessControl",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_002),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    {
        let s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!s.muted_in_channel.contains_key(&("channel:leaf-acl".into(), 90_000_002)));
        assert!(s.muted_in_channel.contains_key(&("channel:other".into(), 90_000_002)));
        assert!(s.muted_in_channel.contains_key(&("channel:leaf-acl".into(), 90_000_099)));
        assert!(s.blocked_in_channel["channel:other"].contains(&90_000_002));
        assert!(s.blocked_in_channel["channel:leaf-acl"].contains(&90_000_099));
    }
    handle_chat_and_social(
        &ctx,
        &mut s2,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[
            PyRep::String("channel:leaf-acl".into()),
            PyRep::Integer(90_000_001),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Invite: ESI contact blocked with standing 0.0 rejects, standing 5.0 overrides block & waives CSPA
    ctx.esi_state.char_contacts.write().unwrap().insert(
        90_000_002,
        vec![crate::esi::runtime_state::EsiContactEntry {
            contact_id: 90_000_001,
            contact_type: "character".into(),
            standing: 0.0,
            is_watched: false,
            is_blocked: true,
            label_ids: vec![],
        }],
    );
    assert_eq!(
        handle_chat_and_social(&ctx, &mut s1, "LSC", "Invite", &[PyRep::Integer(90_000_002)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "SenderBlockedByRecipient"
    );
    ctx.esi_state.char_contacts.write().unwrap().get_mut(&90_000_002).unwrap()[0].standing = 5.0;
    handle_chat_and_social(
        &ctx,
        &mut s1,
        "LSC",
        "Invite",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap();
}
