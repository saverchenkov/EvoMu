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
