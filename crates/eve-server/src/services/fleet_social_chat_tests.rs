//! Unit tests for `fleet_social_chat` XMPP/LSC channels, moderation, delayed local, and contacts/CSPA.

#[cfg(test)]
#[path = "fleet_social_chat_channels_tests.rs"]
mod fleet_social_chat_channels_tests;

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
