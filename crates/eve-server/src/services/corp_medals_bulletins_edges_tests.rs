//! Edge-case tests for `corp_medals_bulletins`: insufficient funds, ACL denials,
//! medal status reconstructions, visibility filtering across characters, and alliance executor checks.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{AllianceRecord, IskCents};

#[test]
fn corp_medals_error_paths_status_reconstruct_and_visibility_edges() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let other_char_id = 90_000_099_i64;
    let now = ctx.now_unix();

    // Unknown method returns None
    assert!(handle_corp_medals(&ctx, corp_id, char_id, now, true, "UnknownMedalRpc", &[]).is_none());

    // CreateMedal with missing character -> CharacterNotFound
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, 999_999_999, now, true, "CreateMedal", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // CreateMedal with insufficient ISK -> InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| {
        let mut other_ch = snap.characters[&char_id].clone();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(1_000);
        }
        other_ch.character_id = other_char_id;
        other_ch.user_id = 99;
        other_ch.name = "Wingmate".into();
        other_ch.corporation_id = corp_id;
        snap.characters.insert(other_char_id, other_ch);
    });
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // Fund character and CreateMedal with empty graphics list + integer graphic ID
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(50_000_000);
    });
    let m_default = handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
        .unwrap()
        .unwrap();
    let mid_def = m_default.dict_get("medalID").unwrap().as_i64().unwrap();
    assert_eq!(
        m_default.dict_get("title").and_then(|v| v.as_str()),
        Some("Valorous Service Medal")
    );

    let m_int_gfx = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "CreateMedal",
        &[
            PyRep::String("Star".into()),
            PyRep::String("Numeric graphic".into()),
            PyRep::List(vec![PyRep::List(vec![
                PyRep::Integer(2),
                PyRep::Integer(3),
                PyRep::Integer(7001),
                PyRep::Integer(15),
            ])]),
        ],
    )
    .unwrap()
    .unwrap();
    let mid_int = m_int_gfx.dict_get("medalID").unwrap().as_i64().unwrap();

    // GiveMedalToCharacters validation & error paths
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, false, "GiveMedalToCharacters", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, true, "GiveMedalToCharacters", &[PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(mid_def), PyRep::List(vec![])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    let too_many: Vec<PyRep> = (1..=101).map(PyRep::Integer).collect();
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(mid_def), PyRep::List(too_many)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(mid_def), PyRep::Integer(999_888_777)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharNotFound"
    );
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(999_999), PyRep::Integer(char_id)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MedalNotFound"
    );

    // Give mid_def to other_char_id (as private status=2) and mid_int to char_id via single Integer
    handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "GiveMedalToCharacters",
        &[PyRep::Integer(mid_def), PyRep::Integer(other_char_id)],
    )
    .unwrap()
    .unwrap();
    handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "GiveMedalToCharacters",
        &[PyRep::Integer(mid_int), PyRep::Tuple(vec![PyRep::Integer(char_id)])],
    )
    .unwrap()
    .unwrap();

    // Querying other_char_id's received medals excludes private medal (min_status = 3)
    let recv_private = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalsReceived",
        &[PyRep::Integer(other_char_id)],
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts_priv) = recv_private else { panic!("tuple expected") };
    assert_eq!(parts_priv[0].dict_get("lines").and_then(|v| match v { PyRep::List(l) => Some(l.len()), _ => None }), None);
    // Remove char_medals for char_id and test SetMedalStatus reconstruction + Bool/String variants
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.remove(&char_id);
        // Also insert an extra char_medal only in char_medals to test the unseen char_medals branch
        snap.char_medals.entry(other_char_id).or_default().push(
            eve_db::snapshot_ext::CharacterMedalRecord {
                medal_id: 88_001,
                title: "Legacy".into(),
                description: "Old award".into(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: now,
                reason: "Service".into(),
                status: "public".into(),
                graphics: vec![eve_db::snapshot_ext::CharacterMedalGraphicRecord {
                    part: 1,
                    layer: 1,
                    graphic: "ribbon_3".into(),
                    color: 5,
                }],
            },
        );
    });

    // Reconstruct char_medals for mid_int using Dict with Bool(false) -> private (2), then Bool(true) -> public (3)
    let st_priv = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Dict(vec![
            (PyRep::Integer(mid_int), PyRep::Bool(false)),
            (PyRep::Integer(999_111), PyRep::None),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(st_priv.dict_get("status").and_then(|v| v.as_i64()), Some(3));

    handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Integer(mid_int), PyRep::Bool(true)],
    )
    .unwrap()
    .unwrap();
    assert!(ctx.db.with_snapshot(|s| s.char_medals.get(&char_id).is_some_and(|l| l.iter().any(|m| m.medal_id == mid_int && m.status == "public"))));

    // GetMedalsReceived for other_char_id sees the public legacy char_medal (88_001)
    let recv_other = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalsReceived",
        &[PyRep::Integer(other_char_id)],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(recv_other, PyRep::Tuple(ref t) if t.len() == 2));

    // GetMedalDetails missing medal -> MedalNotFound; GetAllCorpMedals on empty corp; GetRecipientsOfMedal on missing medal
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, false, "GetMedalDetails", &[PyRep::Integer(999_777)])
            .unwrap()
            .unwrap_err()
            .code,
        "MedalNotFound"
    );
    assert!(handle_corp_medals(&ctx, corp_id, char_id, now, false, "GetAllCorpMedals", &[PyRep::Integer(98_999_999)]).unwrap().is_ok());
    assert!(handle_corp_medals(&ctx, corp_id, char_id, now, false, "GetRecipientsOfMedal", &[PyRep::Integer(999_777)]).unwrap().is_ok());
}

#[test]
fn corp_bulletins_alliance_executor_access_and_error_edges() {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let non_exec_corp_id = 98_000_002_i64;
    let alliance_id = 99_000_050_i64;
    sess.corporation_id = Some(non_exec_corp_id);
    sess.alliance_id = Some(alliance_id);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Exec Alliance".into(),
                short_name: "EXEC".into(),
                executor_corp_id: corp_id,
                member_corp_ids: vec![corp_id, non_exec_corp_id].into_iter().collect(),
            },
        );
    });

    // Non-executor corp denied alliance AddBulletin, UpdateBulletinOrder, DeleteBulletin
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "AddBulletin",
            &[PyRep::String("T".into()), PyRep::String("B".into()), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "UpdateBulletinOrder",
            &[PyRep::List(vec![]), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(1), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );

    // GetBulletins for unrelated corp ID -> CrpAccessDenied
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            false,
            "GetBulletins",
            &[PyRep::Integer(98_777_777)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // DeleteBulletin missing corp bulletin -> BulletinNotFound
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(12345)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );

    // Executor corp: Add alliance bulletins, reorder via nested tuple pairs, fallback GetBulletins from alliance_war_state
    let ab1 = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[PyRep::String("A1".into()), PyRep::String("B1".into()), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap()
    .dict_get("bulletinID")
    .unwrap()
    .as_i64()
    .unwrap();
    let ab2 = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[PyRep::String("A2".into()), PyRep::String("B2".into()), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap()
    .dict_get("bulletinID")
    .unwrap()
    .as_i64()
    .unwrap();

    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "UpdateBulletinOrder",
            &[
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(ab2), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(ab1), PyRep::Integer(1)]),
                ]),
                PyRep::Bool(true),
            ],
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Clear corp_governance bulletins for alliance_id to test fallback to alliance_war_state
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.corp_governance.get_mut(&alliance_id) {
            st.bulletins.clear();
        }
    });
    assert!(handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        false,
        "GetBulletins",
        &[PyRep::Integer(alliance_id)],
    )
    .unwrap()
    .is_ok());

    // Implicit alliance delete (without bool flag) finds bulletin in alliance_war_state
    assert!(handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "DeleteBulletin",
        &[PyRep::Integer(ab1)],
    )
    .unwrap()
    .is_ok());

    // Delete missing alliance bulletin -> BulletinNotFound
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(999_999), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );

    // Explicit alliance delete when session has no alliance_id -> NotAllianceExecutor
    sess.alliance_id = None;
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(ab2), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
}

#[test]
fn mutant_kill_corp_medals_dispatch_give_and_set_status_exact_returns() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let recipient_id = 90_000_099_i64;
    let now = ctx.now_unix();

    ctx.db.acquire_online_lock(1, 720, Some(char_id), true, now).unwrap();
    ctx.db.acquire_online_lock(2, 721, Some(recipient_id), true, now).unwrap();

    // Unknown method must return None (kills Some(Ok(Default::default())))
    assert!(handle_corp_medals(&ctx, corp_id, char_id, now, true, "DoesNotExist", &[]).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        let mut r = snap.characters[&char_id].clone();
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(20_000_000);
        r.character_id = recipient_id;
        r.user_id = 2;
        r.corporation_id = corp_id;
        snap.characters.insert(recipient_id, r);
    });

    let created = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "CreateMedal",
        &[PyRep::String("Apex Ribbon".into()), PyRep::String("For valor".into())],
    )
    .unwrap()
    .unwrap();
    let medal_id = created.dict_get("medalID").and_then(|v| v.as_i64()).unwrap();
    assert!(medal_id > 0);
    assert_eq!(created.dict_get("title"), Some(&PyRep::String("Apex Ribbon".into())));

    // GiveMedalToCharacters exact KeyVal, DB issuance, and OnMedalIssued notification
    let _ = ctx.drain_notifications_for_char(recipient_id);
    let given = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::Integer(recipient_id), PyRep::String("Heroism".into())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(given.dict_get("medalID"), Some(&PyRep::Integer(medal_id)));
    assert_eq!(given.dict_get("recipientID"), Some(&PyRep::Integer(recipient_id)));
    assert_eq!(given.dict_get("status"), Some(&PyRep::Integer(2)));
    assert!(ctx
        .drain_notifications_for_char(recipient_id)
        .iter()
        .any(|n| n.event == "OnMedalIssued"));

    // SetMedalStatus for recipient changes status -> 3 ("public") and then 1 (deleted)
    let st_pub = handle_corp_medals(
        &ctx,
        corp_id,
        recipient_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Integer(medal_id), PyRep::String("public".into())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(st_pub.dict_get("medalID"), Some(&PyRep::Integer(medal_id)));
    assert_eq!(st_pub.dict_get("status"), Some(&PyRep::Integer(3)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.char_medals[&recipient_id][0].status, "public");
    });
    assert!(ctx
        .drain_notifications_for_char(recipient_id)
        .iter()
        .any(|n| n.event == "OnMedalStatusChanged"));

    let st_del = handle_corp_medals(
        &ctx,
        corp_id,
        recipient_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Integer(medal_id), PyRep::String("deleted".into())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(st_del.dict_get("status"), Some(&PyRep::Integer(1)));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.char_medals[&recipient_id].is_empty());
    });
}
