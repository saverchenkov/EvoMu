//! Additional edge-case and branch coverage tests for `services::character_contacts`.

use super::*;
use crate::router::ServerContext;
use eve_db::{SocialContactRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn char_session(char_id: i64, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

#[test]
fn public_private_description_notes_and_respec_edge_paths() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let kw = HashMap::new();

    // Unselected session errors
    let mut no_char_sess = SessionState {
        user_id: Some(1),
        ..Default::default()
    };
    for (svc, method) in [
        ("charMgr", "GetPublicInfo3"),
        ("charMgr", "GetCharacterDescription"),
        ("charMgr", "SetCharacterDescription"),
        ("corporationSvc", "GetEmployementRecordAndCharacterTransfers"),
    ] {
        let err = handle_contacts(&ctx, &mut no_char_sess, svc, method, &[], &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "CharacterNotSelected", "{svc}.{method}");
    }

    let mut sess = char_session(char_id, Some(60_003_760));

    // SetCharacterDescription boundary: 5000 ok, 5001 -> DescriptionTooLong
    let exact_5000 = "a".repeat(5000);
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "SetCharacterDescription",
            &[PyRep::String(exact_5000.clone())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let too_long = "a".repeat(5001);
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "SetCharacterDescription",
            &[PyRep::String(too_long)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DescriptionTooLong"
    );
    let desc_back = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "GetCharacterDescription",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(desc_back, PyRep::String(exact_5000));

    // GetPrivateInfo populated skills/standings & CharacterNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.standings.insert(1_000_009, 2.5);
        snap.characters.insert(char_id, ch);
    });
    let priv_info = handle_contacts(&ctx, &mut sess, "charMgr", "GetPrivateInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(priv_info.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    let mut bad_sess = char_session(999_888_777, Some(60_003_760));
    assert_eq!(
        handle_contacts(&ctx, &mut bad_sess, "charMgr", "GetPrivateInfo", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // GetEmployementRecordAndCharacterTransfers returns employmentRecords
    let emp = handle_contacts(
        &ctx,
        &mut sess,
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(emp, PyRep::Rowset { ref columns, .. } if columns.contains(&"corporationID".to_string())));

    // Owner notes isolation: char_id creates note, other_id cannot edit or remove it
    let note_id = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddOwnerNote",
        &[PyRep::String("Intel".into()), PyRep::String("Secret".into())],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();

    let mut other_sess = char_session(other_id, Some(60_003_760));
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut other_sess,
            "charMgr",
            "EditOwnerNote",
            &[
                PyRep::Integer(note_id),
                PyRep::String("Hack".into()),
                PyRep::String("Body".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut other_sess,
            "charMgr",
            "RemoveOwnerNote",
            &[PyRep::Integer(note_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    // GetOwnerNoteLabels deduplicates
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddOwnerNote",
        &[PyRep::String("Intel".into()), PyRep::String("Dup label".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let labels = handle_contacts(&ctx, &mut sess, "charMgr", "GetOwnerNoteLabels", &[], &kw)
        .unwrap()
        .unwrap();
    match labels {
        PyRep::List(rows) => {
            let dup_count = rows
                .iter()
                .filter(|v| v.as_str() == Some("Dup label"))
                .count();
            assert_eq!(dup_count, 1);
            assert!(rows.iter().any(|v| v.as_str() == Some("Secret")));
        }
        other => panic!("expected List, got {other:?}"),
    }

    // RespecCharacter via Dict + InvalidRemapArguments + RemapFailed + skillMgr2 route
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "RespecCharacter",
            &[PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidRemapArguments"
    );
    // Zero bonus remap available & invalid total points -> RemapFailed
    let bad_dict = PyRep::Dict(vec![
        (PyRep::String("intelligence".into()), PyRep::Integer(99)),
        (PyRep::String("memory".into()), PyRep::Integer(20)),
        (PyRep::String("charisma".into()), PyRep::Integer(19)),
        (PyRep::String("perception".into()), PyRep::Integer(20)),
        (PyRep::String("willpower".into()), PyRep::Integer(20)),
    ]);
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "skillMgr2", "RespecCharacter", &[bad_dict], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "RemapFailed"
    );

    // Valid Dict respec (first respec succeeds using annual/free remap when unused, or bad attributes fail RemapFailed)
    let ok_dict = PyRep::Dict(vec![
        (PyRep::String("intelligence".into()), PyRep::Integer(27)),
        (PyRep::String("memory".into()), PyRep::Integer(21)),
        (PyRep::String("charisma".into()), PyRep::Integer(17)),
        (PyRep::String("perception".into()), PyRep::Integer(17)),
        (PyRep::String("willpower".into()), PyRep::Integer(17)),
    ]);
    let respec_res = handle_contacts(&ctx, &mut sess, "skillMgr2", "RespecCharacter", &[ok_dict], &kw)
        .unwrap()
        .unwrap();
    let kv = match respec_res {
        PyRep::KeyVal(d) => d,
        other => panic!("expected KeyVal from RespecCharacter, got {other:?}"),
    };
    let get_int = |k: &str| kv.get(k).and_then(|v| v.as_i64());
    assert_eq!(get_int("characterID"), Some(char_id));
    assert_eq!(get_int("intelligence"), Some(27));
    assert_eq!(get_int("perception"), Some(17));
    assert_eq!(get_int("charisma"), Some(17));
    assert_eq!(get_int("willpower"), Some(17));
    assert_eq!(get_int("memory"), Some(21));
}

#[test]
fn contacts_block_labels_limits_and_notification_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 1_000_009_i64;
    let kw = HashMap::new();
    let mut sess = char_session(char_id, Some(60_003_760));

    // AddContact errors: missing arg -> InvalidArgument, self / <=0 -> InvalidContactID, unknown (<1000) -> ContactNotFound
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "AddContact", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(char_id), PyRep::Real(5.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidContactID"
    );
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(0), PyRep::Real(5.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidContactID"
    );
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(500), PyRep::Real(5.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ContactNotFound"
    );

    // AddContact corp contact & type inference + DeleteContacts missing returns 0
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(corp_id), PyRep::Real(10.0), PyRep::Bool(false), PyRep::Bool(false)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let del_missing = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteContacts",
        &[PyRep::List(vec![PyRep::Integer(888_777_666)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(del_missing.dict_get("deletedCount"), Some(&PyRep::Integer(0)));

    // EditContact missing IDs & EditContactsRelationshipID creates / preserves watchlist
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "charMgr", "EditContact", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "EditContactsRelationshipID",
        &[
            PyRep::List(vec![PyRep::Integer(other_id)]),
            PyRep::Real(5.0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    // MaxContactsExceeded when adding new contact at 1024, but updating existing at 1024 succeeds
    ctx.db.with_mut_snapshot(|snap| {
        let mut list = snap.char_contacts.get(&char_id).cloned().unwrap_or_default();
        while list.len() < 1024 {
            let cid = 95_000_000 + list.len() as i64;
            list.push(SocialContactRecord {
                contact_id: cid,
                contact_type: "character".into(),
                standing: 0.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            });
        }
        snap.char_contacts.insert(char_id, list);
    });
    if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
        let mut esi_list = Vec::with_capacity(1_024);
        for cid in 80_000_000..80_001_024_i64 {
            esi_list.push(crate::esi::runtime_state::EsiContactEntry {
                contact_id: cid,
                contact_type: "character".into(),
                standing: 0.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            });
        }
        guard.insert(char_id, esi_list);
    }
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "BlockOwners",
            &[PyRep::List(vec![PyRep::Integer(99_999_111)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxContactsExceeded"
    );
    // UnblockOwners on non-blocked owner returns 0
    let unblocked_none = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(99_999_222)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        unblocked_none.dict_get("unblockedCount"),
        Some(&PyRep::Integer(0))
    );
    assert_eq!(
        unblocked_none.dict_get("unblockedIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(99_999_222)]))
    );

    // Labels: InvalidLabelName, create, delete frees bitmask & clears from contacts, LabelNotFound
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "CreateLabel",
            &[PyRep::String("   ".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLabelName"
    );
    let l1 = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "CreateLabel",
        &[PyRep::String("Logi".into())],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(other_id)]),
            PyRep::Integer(l1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteLabel",
        &[PyRep::Integer(l1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "EditLabel",
            &[PyRep::Integer(9999), PyRep::String("X".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LabelNotFound"
    );

    // BlockOwners on self -> InvalidArgument
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "BlockOwners",
            &[PyRep::List(vec![PyRep::Integer(char_id)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
}

#[test]
fn online_status_blocks_home_station_and_mail_block_edges() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let kw = HashMap::new();
    let mut sess = char_session(char_id, Some(60_003_760));

    // Set other_id online via acquire_online_lock
    let other_uid = ctx.db.with_snapshot(|snap| snap.characters[&other_id].user_id);
    ctx.db
        .acquire_online_lock(other_uid, 77, Some(other_id), false, ctx.now_unix())
        .unwrap();

    // Empty GetOnlineStatus -> InvalidArgument
    assert_eq!(
        handle_contacts(&ctx, &mut sess, "onlineStatus", "GetOnlineStatus", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    // Scalar -> Bool(true) when not blocked
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "onlineStatus",
            "GetOnlineStatus",
            &[PyRep::Integer(other_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Block other_id -> GetOnlineStatus hides online status (Bool(false)), and SendMail fails with ContactBlocked
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "BlockOwners",
        &[PyRep::List(vec![PyRep::Integer(other_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_contacts(
            &ctx,
            &mut sess,
            "onlineStatus",
            "GetOnlineStatus",
            &[PyRep::Integer(other_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    // Unblocked List query returns Dict
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "UnblockOwners",
        &[PyRep::List(vec![PyRep::Integer(other_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let batch = handle_contacts(
        &ctx,
        &mut sess,
        "onlineStatus",
        "GetOnlineStatus",
        &[PyRep::List(vec![PyRep::Integer(other_id), PyRep::Integer(999_888_777)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    match batch {
        PyRep::Dict(entries) => {
            assert!(entries.iter().any(|(k, v)| *k == PyRep::Integer(other_id) && *v == PyRep::Bool(true)));
            assert!(entries.iter().any(|(k, v)| *k == PyRep::Integer(999_888_777) && *v == PyRep::Bool(false)));
        }
        other => panic!("expected Dict, got {other:?}"),
    }

    // Block reverse direction -> mailMgr.SendMail returns ContactBlocked
    let mut other_sess = char_session(other_id, Some(60_003_760));
    let _ = handle_contacts(
        &ctx,
        &mut other_sess,
        "charMgr",
        "BlockOwners",
        &[PyRep::List(vec![PyRep::Integer(char_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let mail_err = handle_contacts(
        &ctx,
        &mut sess,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(other_id)]),
            PyRep::String("Hi".into()),
            PyRep::String("Body".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(mail_err.code, "ContactBlocked");

    // GetInitialState returns Rowset
    let init = handle_contacts(&ctx, &mut sess, "onlineStatus", "GetInitialState", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(init, PyRep::Rowset { .. } | PyRep::TupleSet { .. }));

    // Home station cooldown + reset_remote_change_time
    let remote_hq = 60_003_760_i64;
    let player_corp_id = 98_654_321_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = player_corp_id;
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = player_corp_id;
        corp.name = "Home HQ Corp".into();
        corp.ceo_id = char_id;
        corp.hq_station_id = Some(remote_hq);
        corp.members.insert(char_id);
        snap.corporations.insert(player_corp_id, corp);
    });
    sess.corporation_id = Some(player_corp_id);
    sess.station_id = Some(60_008_494);
    let _ = handle_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "SetHomeStation",
        &[PyRep::Integer(remote_hq)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let next_time = call_home(&ctx, &mut sess, "get_next_remote_change_time").unwrap();
    assert!(next_time.as_i64().unwrap() > 0);
    assert_eq!(
        call_home(&ctx, &mut sess, "reset_remote_change_time")
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    let _ = call_home(&ctx, &mut sess, "reset_remote_change_time").unwrap();
    let after_reset = call_home(&ctx, &mut sess, "get_next_remote_change_time").unwrap();
    assert_eq!(after_reset, PyRep::Integer(0));
}

fn call_home(ctx: &ServerContext, sess: &mut SessionState, method: &str) -> Result<PyRep, BridgeRpcError> {
    handle_contacts(ctx, sess, "home_station", method, &[], &HashMap::new()).unwrap()
}
