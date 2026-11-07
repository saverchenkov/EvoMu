//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_chat`.

use crate::services::fleet_social::fleet_social_chat::prune_delayed_local_speakers_on_leave;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::fleet_social::handle as handle_social;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[path = "fleet_social_chat_moderation_unit_tests.rs"]
mod fleet_social_chat_moderation_unit_tests;

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
