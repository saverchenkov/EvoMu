use crate::services::account_services::account_services_tests::PyRepTestExt;
use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{AllianceRecord, IskCents};

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}
fn dict_str<'a>(r: &'a PyRep, k: &str) -> Option<&'a str> {
    r.dict_get(k).and_then(|v| v.as_str())
}
fn dict_bool(r: &PyRep, k: &str) -> Option<bool> {
    r.dict_get(k).and_then(|v| v.as_bool())
}

#[test]
fn test_handle_corp_medals_and_queries() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(50_000_000);
        }
    });

    // Access denied without comms role
    assert!(
        handle_corp_medals(&ctx, corp_id, char_id, now, false, "CreateMedal", &[])
            .unwrap()
            .is_err()
    );

    // CreateMedal
    let created = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "CreateMedal",
        &[
            PyRep::String("Purple Heart".into()),
            PyRep::String("For valor in fleet combat".into()),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::String("ribbon_2".into()),
                PyRep::Integer(12),
            ])]),
        ],
    )
    .unwrap()
    .unwrap();
    let mid = dict_i64(&created, "medalID").unwrap();
    assert_eq!(dict_str(&created, "title"), Some("Purple Heart"));

    // GiveMedalToCharacters
    let given = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "GiveMedalToCharacters",
        &[
            PyRep::Integer(mid),
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::String("Outstanding logistics".into()),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&given, "medalID"), Some(mid));
    assert_eq!(dict_i64(&given, "recipientID"), Some(char_id));

    // SetMedalStatus -> public (3)
    let st = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Dict(vec![(
            PyRep::Integer(mid),
            PyRep::String("public".into()),
        )])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&st, "status"), Some(3));

    // Queries: GetAllCorpMedals, GetMedalDetails, GetMedalsReceived, GetRecipientsOfMedal
    let all = handle_corp_medals(&ctx, corp_id, char_id, now, false, "GetAllCorpMedals", &[])
        .unwrap()
        .unwrap();
    assert!(matches!(all, PyRep::Tuple(ref t) if t.len() == 2));

    let details = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalDetails",
        &[PyRep::Integer(mid)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&details, "title"), Some("Purple Heart"));

    let recv = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalsReceived",
        &[PyRep::Integer(char_id)],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(recv, PyRep::Tuple(ref t) if t.len() == 2));

    let recips = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetRecipientsOfMedal",
        &[PyRep::Integer(mid)],
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(recips, PyRep::None));

    // Delete medal status -> 1
    let del = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Integer(mid), PyRep::String("deleted".into())],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&del, "status"), Some(1));
}

#[test]
fn test_handle_corp_bulletins_corp_and_alliance() {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: vec![corp_id].into_iter().collect(),
            },
        );
    });

    // Role denied without comms
    assert!(
        handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "AddBulletin", &[])
            .unwrap()
            .is_err()
    );

    // Add corp bulletin
    let b1 = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[
            PyRep::String("First Bulletin".into()),
            PyRep::String("Body 1".into()),
        ],
    )
    .unwrap()
    .unwrap();
    let bid1 = dict_i64(&b1, "bulletinID").unwrap();

    // Add second corp bulletin and reorder
    let b2 = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[
            PyRep::String("Second Bulletin".into()),
            PyRep::String("Body 2".into()),
        ],
    )
    .unwrap()
    .unwrap();
    let bid2 = dict_i64(&b2, "bulletinID").unwrap();

    let reordered = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "UpdateBulletinOrder",
        &[PyRep::List(vec![PyRep::Integer(bid2), PyRep::Integer(bid1)])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(reordered, PyRep::Bool(true));

    // GetBulletins
    let list = handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "GetBulletins", &[])
        .unwrap()
        .unwrap();
    assert!(!matches!(list, PyRep::None));

    // DeleteBulletin
    let deleted = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "DeleteBulletin",
        &[PyRep::Integer(bid1)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&deleted, "deleted"), Some(true));

    // Add & delete alliance bulletin
    let ab = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[
            PyRep::String("Alliance News".into()),
            PyRep::String("CTA tonight".into()),
            PyRep::Bool(true),
        ],
    )
    .unwrap()
    .unwrap();
    let abid = dict_i64(&ab, "bulletinID").unwrap();
    let adel = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "DeleteBulletin",
        &[PyRep::Integer(abid), PyRep::Bool(true)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&adel, "deleted"), Some(true));
}

#[test]
fn test_corp_medals_and_bulletins_error_paths_and_variants() {
    use eve_db::snapshot_ext::{CharacterMedalGraphicRecord, CharacterMedalRecord};

    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_011_i64;
    let other_corp_id = 98_000_012_i64;
    let non_exec_corp_id = 98_000_013_i64;
    let alliance_id = 99_000_011_i64;
    let other_char_id = char_id + 55;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut other_char = snap.characters.get(&char_id).cloned().unwrap();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(2_000_000); // InsufficientFunds (< 5M)
        }
        other_char.character_id = other_char_id;
        other_char.name = "Medal Recipient".into();
        other_char.corporation_id = corp_id;
        other_char.alliance_id = Some(alliance_id);
        other_char.balance = IskCents::from_isk(10_000_000);
        snap.characters.insert(other_char_id, other_char);
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Medal Alliance".into(),
                short_name: "MDA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id, non_exec_corp_id].into_iter().collect(),
            },
        );
    });
    sess.alliance_id = Some(alliance_id);

    // CreateMedal: InsufficientFunds
    let err_funds = handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
        .unwrap()
        .unwrap_err();
    assert_eq!(err_funds.code, "InsufficientFunds");

    // CreateMedal: CharacterNotFound
    let err_no_char = handle_corp_medals(&ctx, corp_id, 999_888_777, now, true, "CreateMedal", &[])
        .unwrap()
        .unwrap_err();
    assert_eq!(err_no_char.code, "CharacterNotFound");

    // Restore funds and CreateMedal with integer graphic ID + no-graphics default
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(50_000_000);
    });
    let m1 = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "CreateMedal",
        &[
            PyRep::String("Numeric Graphic Medal".into()),
            PyRep::String("Uses int graphic".into()),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(2),
                PyRep::Integer(1),
                PyRep::Integer(42),
                PyRep::Integer(7),
            ])]),
        ],
    )
    .unwrap()
    .unwrap();
    let mid1 = dict_i64(&m1, "medalID").unwrap();
    let m2 = handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
        .unwrap()
        .unwrap();
    let mid2 = dict_i64(&m2, "medalID").unwrap();
    assert_ne!(mid1, mid2);

    // GiveMedalToCharacters error guards
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
            &[PyRep::Integer(mid1), PyRep::String("bad".into())],
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
            &[PyRep::Integer(mid1), PyRep::Integer(999_777_666)],
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
            &[PyRep::Integer(888_777), PyRep::Integer(other_char_id)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MedalNotFound"
    );

    // Scalar recipient happy path + give mid2 to other_char_id so recipient list iterates non-matching mid
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(mid1), PyRep::Integer(char_id), PyRep::String("Solo award".into())],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(mid2), PyRep::Integer(other_char_id), PyRep::String("Second award".into())],
        )
        .unwrap()
        .is_ok()
    );

    // SetMedalStatus Dict with String/Bool/Integer/Other + Scalar with String/Bool/Other
    // First remove char_medals entry so `!found` branch rebuilds it from corp_governance!
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.remove(&char_id);
        // Also insert an unlinked char_medal entry for fallback in GetMedalsReceived
        snap.char_medals.entry(other_char_id).or_default().push(CharacterMedalRecord {
            medal_id: 99_111,
            title: "Fallback Medal".into(),
            description: "From char_medals".into(),
            corporation_id: other_corp_id,
            issuer_id: char_id,
            date_unix: now,
            reason: "External award".into(),
            status: "public".into(),
            graphics: vec![CharacterMedalGraphicRecord {
                part: 1,
                layer: 1,
                graphic: "star_1".into(),
                color: 5,
            }],
        });
    });

    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "SetMedalStatus",
            &[PyRep::Dict(vec![
                (PyRep::Integer(mid1), PyRep::Bool(true)),
            ])],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "SetMedalStatus",
            &[PyRep::Dict(vec![
                (PyRep::Integer(mid1), PyRep::Bool(false)),
            ])],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "SetMedalStatus",
            &[PyRep::Dict(vec![
                (PyRep::Integer(mid1), PyRep::String("private".into())),
            ])],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "SetMedalStatus",
            &[PyRep::Dict(vec![
                (PyRep::Integer(mid1), PyRep::None),
            ])],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "SetMedalStatus",
            &[PyRep::Dict(vec![
                (PyRep::Integer(mid1), PyRep::String("delete".into())),
            ])],
        )
        .unwrap()
        .is_ok()
    );

    // Scalar SetMedalStatus variants ("public", "deleted", "private", Bool(true/false))
    for st_rep in [
        PyRep::String("public".into()),
        PyRep::String("private".into()),
        PyRep::Bool(true),
        PyRep::Bool(false),
        PyRep::Integer(3),
        PyRep::String("deleted".into()),
    ] {
        assert!(
            handle_corp_medals(
                &ctx,
                corp_id,
                other_char_id,
                now,
                false,
                "SetMedalStatus",
                &[PyRep::Integer(mid2), st_rep],
            )
            .unwrap()
            .is_ok()
        );
    }

    // GetMedalDetails missing medal -> MedalNotFound
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, false, "GetMedalDetails", &[PyRep::Integer(777_666)])
            .unwrap()
            .unwrap_err()
            .code,
        "MedalNotFound"
    );

    // GetMedalsReceived as viewer != target -> reads public char_medals fallback (99_111)
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
    if let PyRep::Tuple(tup) = recv_other {
        assert_eq!(tup.len(), 2);
    } else {
        panic!("expected tuple from GetMedalsReceived");
    }

    // GetRecipientsOfMedal when corp_governance has multiple different medals
    let recips = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetRecipientsOfMedal",
        &[PyRep::Integer(999_999)],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(recips, PyRep::Rowset { .. }));

    // Bulletins: non-executor alliance failures & UpdateBulletinOrder tuple/int/scalar + GetBulletins variants + DeleteBulletin errors
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "AddBulletin",
            &[PyRep::String("Denied".into()), PyRep::String("Body".into()), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
    let b_corp = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[PyRep::String("Corp 1".into()), PyRep::String("B1".into()), PyRep::Bool(false)],
    )
    .unwrap()
    .unwrap();
    let bid_c1 = dict_i64(&b_corp, "bulletinID").unwrap();
    let b_all = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[PyRep::String("All 1".into()), PyRep::String("A1".into()), PyRep::Bool(true)],
    )
    .unwrap()
    .unwrap();
    let bid_a1 = dict_i64(&b_all, "bulletinID").unwrap();

    // UpdateBulletinOrder: tuple of tuples, flat integers, scalar args, alliance executor & non-executor
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "UpdateBulletinOrder",
            &[PyRep::List(vec![PyRep::Integer(bid_a1)]), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
    assert!(
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
                    PyRep::Tuple(vec![PyRep::Integer(bid_a1), PyRep::Integer(1)]),
                    PyRep::String("ignored".into()),
                ]),
                PyRep::Bool(true),
            ],
        )
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "UpdateBulletinOrder",
            &[PyRep::Integer(bid_c1)],
        )
        .unwrap()
        .is_ok()
    );

    // GetBulletins: Bool(true), explicit alliance_id, denied foreign id, Bool(false), None
    assert!(handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "GetBulletins", &[PyRep::Bool(true)]).unwrap().is_ok());
    assert!(handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "GetBulletins", &[PyRep::Integer(alliance_id)]).unwrap().is_ok());
    assert!(handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "GetBulletins", &[PyRep::String("fallback".into())]).unwrap().is_ok());
    assert_eq!(
        handle_corp_bulletins(&ctx, &sess, corp_id, char_id, now, false, "GetBulletins", &[PyRep::Integer(98_777_666)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // DeleteBulletin: non-executor alliance error, explicit alliance w/o session alliance, not found corp, not found alliance
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(bid_a1), PyRep::Bool(true)],
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
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(999_888), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(999_888), PyRep::Bool(false)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );
    let mut no_all_sess = sess.clone();
    no_all_sess.alliance_id = None;
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &no_all_sess,
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(bid_c1), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
}

#[test]
fn test_corp_medals_error_branches_notifications_and_reconstruct() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let other_char = 90_000_999_i64;
    let now = ctx.now_unix();

    // Unknown medal method returns None
    assert!(handle_corp_medals(&ctx, corp_id, char_id, now, true, "NoSuchMethod", &[]).is_none());

    // CreateMedal: CharacterNotFound
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, 999_888_777, now, true, "CreateMedal", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // CreateMedal: InsufficientFunds when balance < 5,000,000 ISK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(1_000_000);
        }
        let mut other = snap.characters[&char_id].clone();
        other.character_id = other_char;
        other.user_id = 2;
        other.name = "OtherPilot".into();
        snap.characters.insert(other_char, other);
    });
    let char_uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(char_uid, 501, Some(char_id), false, now);
    let _ = ctx.db.acquire_online_lock(2, 502, Some(other_char), false, now);
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // Give enough ISK and test CreateMedal with empty args (defaults) & integer graphic
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(50_000_000);
    });
    let def_medal = handle_corp_medals(&ctx, corp_id, char_id, now, true, "CreateMedal", &[])
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&def_medal, "title"), Some("Valorous Service Medal"));
    let def_mid = dict_i64(&def_medal, "medalID").unwrap();

    let int_gfx_medal = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "CreateMedal",
        &[
            PyRep::String("Star".into()),
            PyRep::String("Desc".into()),
            PyRep::Tuple(vec![PyRep::List(vec![
                PyRep::Integer(2),
                PyRep::Integer(3),
                PyRep::Integer(404),
                PyRep::Integer(5),
            ])]),
        ],
    )
    .unwrap()
    .unwrap();
    let mid2 = dict_i64(&int_gfx_medal, "medalID").unwrap();

    // Verify OnCorporationMedalAdded notification was emitted
    let corp_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(corp_notifs
        .iter()
        .any(|n| n.event == "OnCorporationMedalAdded"));

    // GiveMedalToCharacters error branches
    assert_eq!(
        handle_corp_medals(&ctx, corp_id, char_id, now, false, "GiveMedalToCharacters", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            true,
            "GiveMedalToCharacters",
            &[PyRep::Integer(0), PyRep::Integer(char_id)],
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
            &[PyRep::Integer(mid2), PyRep::List(vec![])],
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
            &[PyRep::Integer(mid2), PyRep::List(too_many)],
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
            &[PyRep::Integer(mid2), PyRep::Integer(777_666_555)],
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
            &[PyRep::Integer(999_999), PyRep::Integer(char_id)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MedalNotFound"
    );

    // GiveMedalToCharacters with single integer recipient to other_char (private status = 2)
    let _ = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        true,
        "GiveMedalToCharacters",
        &[PyRep::Integer(mid2), PyRep::Integer(other_char)],
    )
    .unwrap()
    .unwrap();
    let issued_notifs = ctx.drain_notifications_for_char(other_char);
    assert!(issued_notifs.iter().any(|n| n.event == "OnMedalIssued"));

    // Clear char_medals for other_char to exercise reconstruction branch in SetMedalStatus
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.remove(&other_char);
    });

    // Querying other_char's medals as char_id hides private medals (status 2 < min_status 3)
    let priv_recv = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalsReceived",
        &[PyRep::Integer(other_char)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::Tuple(t) = priv_recv {
        if let PyRep::Object { args, .. } = &t[0] {
            let lines = args.dict_get("lines").and_then(|l| l.as_list()).unwrap();
            assert!(lines.is_empty(), "private medal must be hidden from other callers");
        }
    }

    // SetMedalStatus positional bool(true) on other_char reconstructs char_medals and makes it public
    let set_pub = handle_corp_medals(
        &ctx,
        corp_id,
        other_char,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Integer(mid2), PyRep::Bool(true)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&set_pub, "status"), Some(3));
    let st_notifs = ctx.drain_notifications_for_char(other_char);
    assert!(st_notifs.iter().any(|n| n.event == "OnMedalStatusChanged"));

    // Now char_id can see other_char's public medal
    let pub_recv = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetMedalsReceived",
        &[PyRep::Integer(other_char)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::Tuple(t) = pub_recv {
        if let PyRep::Object { args, .. } = &t[0] {
            let lines = args.dict_get("lines").and_then(|l| l.as_list()).unwrap();
            assert_eq!(lines.len(), 1);
        }
    }

    // SetMedalStatus Dict with Bool(false), Integer(2), and None (defaults to 3), plus empty args fallback
    let st_mix = handle_corp_medals(
        &ctx,
        corp_id,
        other_char,
        now,
        false,
        "SetMedalStatus",
        &[PyRep::Dict(vec![
            (PyRep::Integer(mid2), PyRep::Bool(false)),
            (PyRep::Integer(def_mid), PyRep::None),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&st_mix, "status"), Some(3));

    let st_empty = handle_corp_medals(&ctx, corp_id, other_char, now, false, "SetMedalStatus", &[])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&st_empty, "medalID"), Some(1));
    assert_eq!(dict_i64(&st_empty, "status"), Some(3));

    // GetMedalDetails not found
    assert_eq!(
        handle_corp_medals(
            &ctx,
            corp_id,
            char_id,
            now,
            false,
            "GetMedalDetails",
            &[PyRep::Integer(999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MedalNotFound"
    );

    // GetMedalsReceived fallback from snap.char_medals when medal is not in corp_governance
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.entry(other_char).or_default().push(
            eve_db::snapshot_ext::CharacterMedalRecord {
                medal_id: 777_001,
                title: "Legacy".into(),
                description: "Imported".into(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: now,
                reason: "Legacy award".into(),
                status: "public".into(),
                graphics: vec![eve_db::snapshot_ext::CharacterMedalGraphicRecord {
                    part: 1,
                    layer: 1,
                    graphic: "ribbon_x".into(),
                    color: 2,
                }],
            },
        );
    });
    let recv_with_legacy = handle_corp_medals(
        &ctx,
        corp_id,
        other_char,
        now,
        false,
        "GetMedalsReceived",
        &[],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(recv_with_legacy, PyRep::Tuple(ref t) if t.len() == 2));

    // GetAllCorpMedals for empty corp
    let empty_corp = handle_corp_medals(
        &ctx,
        corp_id,
        char_id,
        now,
        false,
        "GetAllCorpMedals",
        &[PyRep::Integer(98_999_999)],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(empty_corp, PyRep::Tuple(ref t) if t.len() == 2));
}

#[test]
fn test_corp_bulletins_error_branches_and_edge_cases() {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_001_i64;
    let non_exec_corp = 98_000_002_i64;
    let alliance_id = 99_000_001_i64;
    sess.corporation_id = Some(non_exec_corp);
    sess.alliance_id = Some(alliance_id);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: vec![corp_id, non_exec_corp].into_iter().collect(),
            },
        );
    });

    // Non-executor corp denied alliance AddBulletin, UpdateBulletinOrder, DeleteBulletin
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp,
            char_id,
            now,
            true,
            "AddBulletin",
            &[
                PyRep::String("T".into()),
                PyRep::String("B".into()),
                PyRep::Bool(true),
            ],
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
            non_exec_corp,
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
            non_exec_corp,
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

    // GetBulletins for unauthorized corp ID -> CrpAccessDenied
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            non_exec_corp,
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
    // GetBulletins for own alliance ID -> allowed
    assert!(handle_corp_bulletins(
        &ctx,
        &sess,
        non_exec_corp,
        char_id,
        now,
        false,
        "GetBulletins",
        &[PyRep::Integer(alliance_id)],
    )
    .unwrap()
    .is_ok());

    // DeleteBulletin missing bulletin -> BulletinNotFound (corp and alliance)
    assert_eq!(
        handle_corp_bulletins(
            &ctx,
            &sess,
            corp_id,
            char_id,
            now,
            true,
            "DeleteBulletin",
            &[PyRep::Integer(999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );
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

    // Executor adds alliance bulletin, reorders via tuple pairs, fetches via Bool(true), and deletes implicitly
    let ab = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "AddBulletin",
        &[
            PyRep::String("Exec News".into()),
            PyRep::String("Body".into()),
            PyRep::Integer(1),
        ],
    )
    .unwrap()
    .unwrap();
    let abid = dict_i64(&ab, "bulletinID").unwrap();
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
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(abid), PyRep::Integer(0)])]),
                PyRep::Bool(true),
            ],
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert!(handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        false,
        "GetBulletins",
        &[PyRep::Bool(true)],
    )
    .unwrap()
    .is_ok());
    // Implicit alliance delete (without explicit Bool(true) arg)
    let del_imp = handle_corp_bulletins(
        &ctx,
        &sess,
        corp_id,
        char_id,
        now,
        true,
        "DeleteBulletin",
        &[PyRep::Integer(abid)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&del_imp, "deleted"), Some(true));

    // DeleteBulletin with explicit_alliance when session.alliance_id is None -> NotAllianceExecutor
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
            &[PyRep::Integer(1), PyRep::Bool(true)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );
}
