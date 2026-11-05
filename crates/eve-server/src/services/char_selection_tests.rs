//! Unit tests for `char_selection` (`charUnboundMgr.GetCharacterSelectionData`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{MailRecord, SkillQueueEntry, WarRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;

#[test]
fn unix_to_filetime_and_character_selection_branches() {
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
    assert_eq!(unix_to_filetime(100), 116_444_737_000_000_000);

    let ctx = test_ctx();
    let unauth = SessionState::default();
    assert!(get_character_selection_data(&ctx, &unauth).is_err());

    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let mut sess = auth_session(uid, cid, corp_id, 30_000_142);

    // Cluster shutdown blocks non-GM but allows GM
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    assert_eq!(
        get_character_selection_data(&ctx, &sess).unwrap_err().code,
        "ClusterShuttingDown"
    );
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(get_character_selection_data(&ctx, &sess).is_ok());
    ctx.cluster_state.write().unwrap().shutdown_in_progress = false;
    sess.role = 0;

    // Default state: no active MCT, no skill queue, no wars
    let initial = get_character_selection_data(&ctx, &sess).unwrap();
    let PyRep::Tuple(parts) = initial else {
        panic!("expected tuple");
    };
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[1], PyRep::Tuple(vec![PyRep::None, PyRep::None]));

    // Populate MCT slots (one expired, one active), skill queue, unread/read/deleted mails, resculpt, active/inactive wars
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("mct_slots:{uid}"),
            serde_json::to_string(&vec![now.saturating_sub(50), now + 3_600]).unwrap(),
        );
        snap.service_state_blobs
            .insert(format!("resculpt_pending:{cid}"), "1".into());
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 3,
                started_at_unix: Some(now.saturating_sub(60)),
                duration_seconds: 600.0,
                start_sp: 1_415.0,
                end_sp: 8_000.0,
            }],
        );
        snap.mails.push(MailRecord {
            message_id: 901,
            sender_id: 90_000_002,
            recipient_id: cid,
            subject: "Unread".into(),
            body: "Body".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 902,
            sender_id: 90_000_002,
            recipient_id: cid,
            subject: "Read".into(),
            body: "Body".into(),
        });
        snap.read_mails.insert((cid, 902));
        snap.mails.push(MailRecord {
            message_id: 903,
            sender_id: 90_000_002,
            recipient_id: cid,
            subject: "Deleted".into(),
            body: "Body".into(),
        });
        snap.deleted_mails.insert((cid, 903));

        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_002,
                mutual: true,
                started_unix: now,
                active: true,
            },
        );
        snap.wars.insert(
            502,
            WarRecord {
                war_id: 502,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_003,
                mutual: false,
                started_unix: now,
                active: false,
            },
        );
    });

    let populated = get_character_selection_data(&ctx, &sess).unwrap();
    let PyRep::Tuple(parts2) = populated else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(mct_tuple) = &parts2[1] else {
        panic!("expected mct tuple");
    };
    assert_eq!(mct_tuple[0], PyRep::Integer(1));
    assert_eq!(mct_tuple[1].as_list().map(|l| l.len()), Some(1));

    let chars_list = parts2[2].as_list().unwrap();
    let ch_row = chars_list
        .iter()
        .find(|r| r.dict_get("characterID") == Some(&PyRep::Integer(cid)))
        .unwrap();
    assert_eq!(ch_row.dict_get("unreadMailCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ch_row.dict_get("paperdollState"), Some(&PyRep::Integer(1)));
    assert_eq!(ch_row.dict_get("skillTypeID"), Some(&PyRep::Integer(3300)));
    assert_eq!(ch_row.dict_get("toLevel"), Some(&PyRep::Integer(3)));

    let wars_list = parts2[3].as_list().unwrap();
    assert!(wars_list
        .iter()
        .any(|w| w.dict_get("warID") == Some(&PyRep::Integer(501))));
    assert!(!wars_list
        .iter()
        .any(|w| w.dict_get("warID") == Some(&PyRep::Integer(502))));

    // Paused skill queue hides skillTypeID and queueEndTime
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.insert(cid);
    });
    let paused = get_character_selection_data(&ctx, &sess).unwrap();
    let PyRep::Tuple(parts3) = paused else {
        panic!("expected tuple");
    };
    let ch_row_paused = parts3[2]
        .as_list()
        .unwrap()
        .iter()
        .find(|r| r.dict_get("characterID") == Some(&PyRep::Integer(cid)))
        .unwrap();
    assert_eq!(ch_row_paused.dict_get("skillTypeID"), Some(&PyRep::None));
    assert_eq!(ch_row_paused.dict_get("queueEndTime"), Some(&PyRep::None));
}
