//! Unit and mutation-killing tests for `esi::character_mail_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::{IskCents, WalletJournalEntry};

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn make_claims(char_id: i64) -> SsoTokenClaims {
    SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{char_id}"),
        name: "Tester".to_string(),
        owner: "hash".to_string(),
        user_id: 1,
        character_id: char_id,
        scp: Vec::new(),
        iss: "evomu".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "jti-1".to_string(),
    }
}

#[test]
fn parse_optional_from_id_and_check_corp_wallet_access_cover_all_branches() {
    let empty = HashMap::new();
    assert_eq!(parse_optional_from_id(&empty).unwrap(), None);
    assert_eq!(
        parse_optional_from_id(&params(&[("from_id", "123")])).unwrap(),
        Some(123)
    );
    for bad in ["0", "-5", "abc"] {
        assert_eq!(
            parse_optional_from_id(&params(&[("from_id", bad)]))
                .expect_err("invalid from_id")
                .status_code,
            400
        );
    }

    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_snapshot(|snap| {
        let mut corp = snap.corporations.get(&corp_id).unwrap().clone();
        assert!(check_corp_wallet_access(&corp, None).is_ok());
        corp.member_roles.insert(char_id, 0);
        assert!(check_corp_wallet_access(&corp, Some(&make_claims(char_id))).is_ok());

        assert_eq!(
            check_corp_wallet_access(&corp, Some(&make_claims(90_999_999)))
                .expect_err("non-member")
                .status_code,
            403
        );

        let member_id = 90_000_002_i64;
        corp.members.insert(member_id);
        corp.member_roles.insert(member_id, 0);
        assert_eq!(
            check_corp_wallet_access(&corp, Some(&make_claims(member_id)))
                .expect_err("no role")
                .status_code,
            403
        );

        for role in [
            eve_db::corp_roles::ROLE_DIRECTOR,
            eve_db::corp_roles::ROLE_ACCOUNTANT,
            eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
        ] {
            corp.member_roles.insert(member_id, role);
            assert!(check_corp_wallet_access(&corp, Some(&make_claims(member_id))).is_ok());
        }
    });
}

#[test]
fn wallet_and_loyalty_routes_cover_character_and_corporation_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    assert!(dispatch_character_mail_routes(
        &ctx,
        "DELETE",
        "/characters/{character_id}/wallet",
        &p_char,
        &empty_q,
        None,
        1,
        b""
    )
    .is_none());

    let bal = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/wallet",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(bal.status_code, 200);
    assert!(body(&bal).as_f64().unwrap() > 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 999_001,
            character_id: char_id,
            amount: IskCents::from_isk(100),
            balance_after: IskCents::from_isk(1000),
            entry_type: String::new(),
            reason: String::new(),
        });
    });
    let journal = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/wallet/journal",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(journal.status_code, 200);
    let j_arr = body(&journal).as_array().unwrap();
    assert_eq!(j_arr[0]["id"], 999_001);
    assert_eq!(j_arr[0]["ref_type"], "player_donation");
    assert_eq!(j_arr[0]["description"], "Wallet transaction");

    let txns = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/wallet/transactions",
        &p_char,
        &params(&[("from_id", "999999999")]),
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(txns.status_code, 200);
    assert!(!body(&txns).as_array().unwrap().is_empty());

    let corp_wallets = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/wallets",
        &p_corp,
        &empty_q,
        Some(&make_claims(char_id)),
        1,
        b"",
    )
    .unwrap();
    assert_eq!(corp_wallets.status_code, 200);
    assert_eq!(body(&corp_wallets).as_array().unwrap().len(), 7);

    for bad_div in ["0", "8", "-1"] {
        let p_bad = params(&[("corporation_id", &corp_str), ("division", bad_div)]);
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "GET",
                "/corporations/{corporation_id}/wallets/{division}/journal",
                &p_bad,
                &empty_q,
                None,
                1,
                b""
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "GET",
                "/corporations/{corporation_id}/wallets/{division}/transactions",
                &p_bad,
                &empty_q,
                None,
                1,
                b""
            )
            .unwrap()
            .status_code,
            400
        );
    }

    let p_div1 = params(&[("corporation_id", &corp_str), ("division", "1")]);
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/wallets/{division}/journal",
            &p_div1,
            &empty_q,
            Some(&make_claims(char_id)),
            1,
            b""
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/wallets/{division}/transactions",
            &p_div1,
            &params(&[("from_id", "999999999")]),
            Some(&make_claims(char_id)),
            1,
            b""
        )
        .unwrap()
        .status_code,
        200
    );

    let lp = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/loyalty/points",
        &p_char,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(lp.status_code, 200);
    assert!(!body(&lp).as_array().unwrap().is_empty());

    let offers = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/loyalty/stores/{corporation_id}/offers",
        &params(&[("corporation_id", "1000035")]),
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(offers.status_code, 200);
    assert!(!body(&offers).as_array().unwrap().is_empty());
}

#[test]
fn mail_list_post_detail_and_delete_cover_cspa_labels_and_validation() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut rec = snap.characters[&char_id].clone();
        rec.character_id = 90_000_777;
        rec.name = "CSPA Recipient".into();
        rec.corporation_id = 1_000_009;
        rec.alliance_id = None;
        snap.characters.insert(90_000_777, rec);
        snap.character_cspa_charges
            .insert(90_000_777, IskCents::from_isk(2_950));
    });

    for bad_body in [
        json!({"approved_cost": -1, "subject": "Hi", "body": "B", "recipients": [{"recipient_id": 90000002, "recipient_type": "character"}]}),
        json!({"subject": "", "body": "B", "recipients": [{"recipient_id": 90000002, "recipient_type": "character"}]}),
        json!({"subject": "a".repeat(1001), "body": "B", "recipients": [{"recipient_id": 90000002, "recipient_type": "character"}]}),
        json!({"subject": "Hi", "body": "a".repeat(10_001), "recipients": [{"recipient_id": 90000002, "recipient_type": "character"}]}),
        json!({"subject": "Hi", "body": "B", "recipients": []}),
        json!({"subject": "Hi", "body": "B", "recipients": [{"recipient_id": 90000002, "recipient_type": "planet"}]}),
    ] {
        let res = dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&bad_body).unwrap(),
        )
        .unwrap();
        assert_eq!(res.status_code, 400, "expected 400 for {bad_body}");
    }

    let cspa_low = json!({
        "approved_cost": 100,
        "subject": "Test CSPA",
        "body": "Hello stranger",
        "recipients": [{"recipient_id": 90_000_777, "recipient_type": "character"}]
    });
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&cspa_low).unwrap()
        )
        .unwrap()
        .status_code,
        400
    );

    let bal_before = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().balance);
    let valid_mail = json!({
        "approved_cost": 5000,
        "subject": "Corp & Alliance Broadcast",
        "body": "Form up at Jita 4-4",
        "recipients": [
            {"recipient_id": 90_000_777, "recipient_type": "character"},
            {"recipient_id": corp_id, "recipient_type": "corporation"},
            {"recipient_id": 99_000_001, "recipient_type": "alliance"}
        ]
    });
    let created = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&valid_mail).unwrap(),
    )
    .unwrap();
    assert_eq!(created.status_code, 201);
    let mail_id = body(&created).as_i64().expect("created mail_id");
    let bal_after = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().balance);
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(2_950).0);

    let list_ok = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/mail",
        &p_char,
        &params(&[
            ("labels", "[2, 4, 8]"),
            ("last_mail_id", &(mail_id + 10).to_string()),
        ]),
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(list_ok.status_code, 200);
    assert!(body(&list_ok)
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["mail_id"] == mail_id));

    let p_mail = params(&[("character_id", &char_str), ("mail_id", &mail_id.to_string())]);
    let detail = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/mail/{mail_id}",
        &p_mail,
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(detail.status_code, 200);
    assert_eq!(body(&detail)["subject"], "Corp & Alliance Broadcast");

    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            None,
            1,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
}

#[test]
fn esi_post_mail_entity_validation_and_mailing_list_acl_parity() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();
    let blocker_id = 90_000_778_i64;
    let list_owner_id = 90_000_779_i64;
    let member2_id = 90_000_002_i64;
    let list_id = 77_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&char_id].clone();
        for (cid, name) in [(blocker_id, "Blocker"), (list_owner_id, "ListOwner")] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(100_000);
            snap.characters.insert(cid, ch);
        }
        snap.char_contacts.insert(
            blocker_id,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: -10.0,
                is_blocked: true,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );
        let mut members = HashMap::new();
        members.insert(list_owner_id, 2);
        members.insert(char_id, 1);
        members.insert(member2_id, 1);
        snap.mailing_lists.insert(
            list_id,
            eve_db::MailingListRecord {
                list_id,
                name: "ESI Op List".into(),
                owner_id: list_owner_id,
                default_access: 1,
                default_role: 1,
                cost: 500,
                members,
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: std::collections::HashSet::new(),
            },
        );
    });

    // Entity-type mismatch / non-existent entities -> 400
    for bad_recip in [
        json!({"recipient_id": blocker_id, "recipient_type": "corporation"}),
        json!({"recipient_id": corp_id, "recipient_type": "character"}),
        json!({"recipient_id": 99_999_999, "recipient_type": "alliance"}),
        json!({"recipient_id": 88_888_888, "recipient_type": "mailing_list"}),
    ] {
        let res = dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "subject": "Bad Type",
                "body": "Test",
                "recipients": [bad_recip]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(res.status_code, 400, "expected 400 for {bad_recip}");
    }

    // NPC corp cannot be targeted as corporation -> 403
    let npc_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "subject": "NPC",
            "body": "Test",
            "recipients": [{"recipient_id": 1_000_035, "recipient_type": "corporation"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(npc_res.status_code, 403);

    // Mailing list cost check against approved_cost -> 400
    let low_cost = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 100,
            "subject": "Costly",
            "body": "Test",
            "recipients": [{"recipient_id": list_id, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(low_cost.status_code, 400);

    // Muted member -> 403
    ctx.db.with_mut_snapshot(|snap| {
        snap.mailing_lists.get_mut(&list_id).unwrap().members.insert(char_id, 0);
    });
    let muted_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 500,
            "subject": "Muted",
            "body": "Test",
            "recipients": [{"recipient_id": list_id, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(muted_res.status_code, 403);

    // Non-member -> 403
    ctx.db.with_mut_snapshot(|snap| {
        snap.mailing_lists.get_mut(&list_id).unwrap().members.remove(&char_id);
    });
    let nonmember_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 500,
            "subject": "Nonmember",
            "body": "Test",
            "recipients": [{"recipient_id": list_id, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(nonmember_res.status_code, 403);

    // Restore role 1 -> sends 201, transfers 500 ISK to owner, updates ml.mail_ids & mail_headers_meta
    ctx.db.with_mut_snapshot(|snap| {
        snap.mailing_lists.get_mut(&list_id).unwrap().members.insert(char_id, 1);
    });
    let owner_bal_before = ctx.db.with_snapshot(|s| s.characters[&list_owner_id].balance.0);
    let sent_ml = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 500,
            "subject": "List Broadcast",
            "body": "Hello list",
            "recipients": [{"recipient_id": list_id, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sent_ml.status_code, 201);
    let ml_mail_id = body(&sent_ml).as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&list_owner_id].balance.0 - owner_bal_before,
            IskCents::from_isk(500).0
        );
        assert!(snap.mailing_lists[&list_id].mail_ids.contains(&ml_mail_id));
        assert!(crate::services::mail_notify::is_mail_authorized(
            &ctx, snap, ml_mail_id, member2_id
        ));
    });
    let member2_str = member2_id.to_string();
    let member_detail = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/mail/{mail_id}",
        &params(&[("character_id", &member2_str), ("mail_id", &ml_mail_id.to_string())]),
        &empty_q,
        None,
        1,
        b"",
    )
    .unwrap();
    assert_eq!(member_detail.status_code, 200);
    assert_eq!(body(&member_detail)["recipients"][0]["recipient_type"], "mailing_list");
}

#[test]
fn mail_meta_auto_seeded_lists_dedup_limits_and_cspa_boundaries() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let recip_same_corp = 90_000_002_i64;
    let recip_boundary_neg10 = 90_000_011_i64;
    let recip_boundary_neg99 = 90_000_012_i64;
    let other_corp_id = 98_000_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        let mut tpl = snap.characters[&char_id].clone();
        tpl.character_id = recip_boundary_neg10;
        tpl.name = "CSPA Neg10".into();
        tpl.corporation_id = other_corp_id;
        tpl.alliance_id = None;
        snap.characters.insert(recip_boundary_neg10, tpl.clone());

        tpl.character_id = recip_boundary_neg99;
        tpl.name = "CSPA Neg99".into();
        snap.characters.insert(recip_boundary_neg99, tpl);

        snap.character_cspa_charges
            .insert(recip_same_corp, IskCents::from_isk(5_000));
        snap.character_cspa_charges
            .insert(recip_boundary_neg10, IskCents::from_isk(1_000));
        snap.character_cspa_charges
            .insert(recip_boundary_neg99, IskCents::from_isk(2_000));

        // Same-corp must be exempt even at -10.0; -10.0 is NOT exempt (> -10.0), -9.9 IS exempt.
        snap.char_contacts.insert(
            recip_same_corp,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: -10.0,
                is_blocked: false,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );
        snap.char_contacts.insert(
            recip_boundary_neg10,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: -10.0,
                is_blocked: false,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );
        snap.char_contacts.insert(
            recip_boundary_neg99,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: -9.9,
                is_blocked: false,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );
    });

    let empty_q = HashMap::new();
    let p_sender = params(&[("character_id", &char_id.to_string())]);
    let claims = make_claims(char_id);

    // 1. Subject > 1_000 chars and body > 10_000 chars -> 400; exactly 1_000 / 10_000 -> accepted.
    let over_subj = "a".repeat(1_001);
    let err_subj = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_sender,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "subject": over_subj,
            "body": "ok",
            "recipients": [{"recipient_id": recip_same_corp, "recipient_type": "character"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(err_subj.status_code, 400);

    let over_body = "b".repeat(10_001);
    let err_body = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_sender,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "subject": "ok",
            "body": over_body,
            "recipients": [{"recipient_id": recip_same_corp, "recipient_type": "character"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(err_body.status_code, 400);

    // 2. Duplicate recipients deduped; same-corp exempt; -10.0 charged (1_000 ISK); -9.9 exempt; exact 1_000/10_000 limits.
    let max_subj = "s".repeat(1_000);
    let max_body = "x".repeat(10_000);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let sent_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_sender,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 1_000,
            "subject": max_subj,
            "body": max_body,
            "recipients": [
                {"recipient_id": recip_same_corp, "recipient_type": "character"},
                {"recipient_id": recip_boundary_neg10, "recipient_type": "character"},
                {"recipient_id": recip_boundary_neg10, "recipient_type": "character"},
                {"recipient_id": recip_boundary_neg99, "recipient_type": "character"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sent_res.status_code, 201);
    let mail_id = body(&sent_res).as_i64().unwrap();

    // Only 1_000 ISK charged once (dedup + same-corp exempt + -9.9 exempt, while -10.0 charged).
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert_eq!(bal_before - bal_after, IskCents::from_isk(1_000).0);

    // mail_notify::record_mail_meta populated mail_headers_meta with deduped recipient IDs.
    {
        let se = ctx.social_extras.lock().unwrap();
        let meta = se
            .mail_headers_meta
            .get(&mail_id)
            .expect("record_mail_meta must record header meta");
        assert_eq!(meta.to_character_ids.len(), 3);
    }

    // ctx.esi_state.get_mail_meta: sender gets is_read: true & labels: [2]; recipient gets is_read: false & labels: [1].
    let sender_meta = ctx.esi_state.get_mail_meta(char_id, mail_id, true);
    assert!(sender_meta.is_read);
    assert_eq!(sender_meta.labels, vec![2]);
    let recip_meta = ctx.esi_state.get_mail_meta(recip_boundary_neg10, mail_id, false);
    assert!(!recip_meta.is_read);
    assert_eq!(recip_meta.labels, vec![1]);

    // Also clear esi_state mail meta to assert get_mail_meta fallback defaults on GET list/detail.
    let list_sender = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/mail",
        &p_sender,
        &empty_q,
        Some(&claims),
        1,
        b"",
    )
    .unwrap();
    assert_eq!(list_sender.status_code, 200);
    let sender_row = body(&list_sender)
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["mail_id"] == mail_id)
        .unwrap();
    assert_eq!(sender_row["is_read"], true);
    assert_eq!(sender_row["labels"], json!([2]));

    let detail_recip = dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/mail/{mail_id}",
        &params(&[
            ("character_id", &recip_boundary_neg10.to_string()),
            ("mail_id", &mail_id.to_string()),
        ]),
        &empty_q,
        Some(&make_claims(recip_boundary_neg10)),
        1,
        b"",
    )
    .unwrap();
    assert_eq!(detail_recip.status_code, 200);
    assert_eq!(body(&detail_recip)["read"], false);
    assert_eq!(body(&detail_recip)["labels"], json!([1]));

    // 3. Auto-seeding mailing lists from snap.mailing_lists WITHOUT manually pre-seeding snap.mailing_lists.
    let existing_list_id = ctx.db.with_snapshot(|snap| {
        snap.mailing_lists
            .keys()
            .copied()
            .next()
            .unwrap_or(corp_id)
    });
    if ctx.db.with_snapshot(|s| !s.mailing_lists.is_empty()) {
        // Plant a stale mailing list entry in social_extras so only ensure_social_extras_mailing_lists_seeded fixes it.
        {
            let mut se = ctx.social_extras.lock().unwrap();
            se.mailing_lists.clear();
            se.mailing_lists.insert(
                999_999,
                crate::services::fleet_social::fleet_social_persistence::MailingListState {
                    list_id: 999_999,
                    name: "StaleDecoy".into(),
                    owner_id: char_id,
                    default_access: 1,
                    default_role: 1,
                    cost: 0,
                    members: HashMap::new(),
                    entity_access: HashMap::new(),
                    welcome_mail: None,
                    mail_ids: std::collections::HashSet::new(),
                },
            );
        }
        let _ = dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_sender,
            &empty_q,
            Some(&claims),
            1,
            &serde_json::to_vec(&json!({
                "subject": "Auto Seeded ML",
                "body": "Body",
                "recipients": [{"recipient_id": existing_list_id, "recipient_type": "mailing_list"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let se = ctx.social_extras.lock().unwrap();
        assert!(
            !se.mailing_lists.contains_key(&999_999),
            "stale social_extras list must be cleared by ensure_social_extras_mailing_lists_seeded"
        );
        assert!(
            se.mailing_lists.contains_key(&existing_list_id),
            "demo-seeded mailing list must be auto-seeded into social_extras"
        );
    }
}

#[test]
fn character_mail_routes_errors_corp_wallet_acl_cspa_lp_and_mailing_list_notify() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let p_missing_char = params(&[("character_id", "90999777")]);
    let p_missing_corp = params(&[("corporation_id", "98999777")]);

    let outsider_id = 90_888_123_i64;
    let restricted_corp = 98_888_123_i64;
    let restricted_ally = 99_888_123_i64;
    let blocked_sender_id = 90_888_124_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&char_id].clone();
        let mut out_c = base_char.clone();
        out_c.character_id = outsider_id;
        out_c.corporation_id = restricted_corp;
        out_c.alliance_id = None;
        out_c.balance = eve_db::IskCents(0);
        snap.characters.insert(outsider_id, out_c);

        let mut blk_c = base_char.clone();
        blk_c.character_id = blocked_sender_id;
        snap.characters.insert(blocked_sender_id, blk_c);

        let reg_mem_id = 90_888_125_i64;
        let mut reg_c = base_char.clone();
        reg_c.character_id = reg_mem_id;
        reg_c.corporation_id = corp_id;
        reg_c.alliance_id = Some(restricted_ally);
        snap.characters.insert(reg_mem_id, reg_c);
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(reg_mem_id);
        }

        snap.corporations.insert(
            restricted_corp,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = restricted_corp; __corp.name = "Restricted".to_string(); __corp.ticker = "RSTR".to_string(); __corp.ceo_id = outsider_id; __corp.tax_rate = 0.0; __corp.hq_station_id = None; __corp.alliance_id = Some(restricted_ally); __corp.members = std::iter::once(char_id).collect(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    eve_db::IskCents(0),
                ); __corp.description = String::new(); __corp.url = String::new(); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.corp_governance.insert(
            restricted_corp,
            eve_db::CorpGovState {
                restrict_corp_mails: true,
                ..Default::default()
            },
        );
        snap.alliances.insert(
            restricted_ally,
            eve_db::AllianceRecord {
                alliance_id: restricted_ally,
                name: "RestrAlly".to_string(),
                short_name: "RA".to_string(),
                executor_corp_id: restricted_corp,
                member_corp_ids: std::iter::once(corp_id).collect(),
            },
        );
        snap.char_contacts.insert(
            outsider_id,
            vec![eve_db::SocialContactRecord {
                contact_id: blocked_sender_id,
                contact_type: "character".to_string(),
                standing: -10.0,
                is_watched: false,
                is_blocked: true,
                label_ids: Vec::new(),
            }],
        );
        snap.character_cspa_charges
            .insert(outsider_id, eve_db::IskCents(1_000_000));
        let mut ml_members = HashMap::new();
        ml_members.insert(char_id, 1);
        ml_members.insert(outsider_id, 0);
        snap.mailing_lists.insert(
            60_001,
            eve_db::MailingListRecord {
                list_id: 60_001,
                name: "All Hands".to_string(),
                owner_id: outsider_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: ml_members,
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: std::collections::HashSet::new(),
            },
        );
    });

    // Missing path params (400) & missing entities (404) & bad division / claims 403
    for path in [
        "/characters/{character_id}/wallet",
        "/characters/{character_id}/wallet/journal",
        "/characters/{character_id}/wallet/transactions",
        "/characters/{character_id}/loyalty/points",
        "/characters/{character_id}/mail",
        "/characters/{character_id}/mail/{mail_id}",
    ] {
        assert_eq!(
            dispatch_character_mail_routes(&ctx, "GET", path, &empty_p, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            400
        );
        let mp = if path.contains("mail_id") {
            params(&[("character_id", "90999777"), ("mail_id", "1")])
        } else {
            p_missing_char.clone()
        };
        assert_eq!(
            dispatch_character_mail_routes(&ctx, "GET", path, &mp, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            404
        );
    }

    for path in [
        "/corporations/{corporation_id}/wallets",
        "/corporations/{corporation_id}/wallets/{division}/journal",
        "/corporations/{corporation_id}/wallets/{division}/transactions",
        "/loyalty/stores/{corporation_id}/offers",
    ] {
        assert_eq!(
            dispatch_character_mail_routes(&ctx, "GET", path, &empty_p, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            400
        );
        let mp = if path.contains("division") {
            params(&[("corporation_id", "98999777"), ("division", "1")])
        } else {
            p_missing_corp.clone()
        };
        assert_eq!(
            dispatch_character_mail_routes(&ctx, "GET", path, &mp, &empty_q, None, 1, b"")
                .unwrap()
                .status_code,
            404
        );
    }

    let c_outsider = SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{outsider_id}"),
        name: "Outsider".to_string(),
        owner: "h".to_string(),
        user_id: 9,
        character_id: outsider_id,
        scp: Vec::new(),
        iss: "evomu".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "j".to_string(),
    };

    // Corp wallet access 403 & invalid division 400 & bad page 404 & bad from_id 400
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/wallets",
            &p_corp,
            &empty_q,
            Some(&c_outsider),
            1,
            b"",
        )
        .unwrap()
        .status_code,
        403
    );
    for path in [
        "/corporations/{corporation_id}/wallets/{division}/journal",
        "/corporations/{corporation_id}/wallets/{division}/transactions",
    ] {
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", &corp_id.to_string()), ("division", "9")]),
                &empty_q,
                None,
                1,
                b"",
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", &corp_id.to_string()), ("division", "bad")]),
                &empty_q,
                None,
                1,
                b"",
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", &corp_id.to_string()), ("division", "1")]),
                &empty_q,
                Some(&c_outsider),
                1,
                b"",
            )
            .unwrap()
            .status_code,
            403
        );
    }
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/wallets/{division}/journal",
            &params(&[("corporation_id", &corp_id.to_string()), ("division", "1")]),
            &empty_q,
            None,
            999,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/wallets/{division}/transactions",
            &params(&[("corporation_id", &corp_id.to_string()), ("division", "1")]),
            &params(&[("from_id", "bad")]),
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/wallet/journal",
            &p_char,
            &empty_q,
            None,
            999,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/wallet/transactions",
            &p_char,
            &params(&[("from_id", "bad")]),
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );

    // GET /characters/{id}/mail query parameter & POST validation errors
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/mail",
            &p_char,
            &params(&[("last_mail_id", "0")]),
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/mail",
            &p_char,
            &params(&[("labels", "bad")]),
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );

    assert_eq!(
        dispatch_character_mail_routes(&ctx, "POST", "/characters/{character_id}/mail", &empty_p, &empty_q, None, 1, b"{}")
            .unwrap()
            .status_code,
        400
    );
    for bad_post in [
        b"bad".as_slice(),
        b"{\"approved_cost\":-1,\"subject\":\"s\",\"body\":\"b\",\"recipients\":[{\"recipient_id\":90000001,\"recipient_type\":\"character\"}]}",
        b"{\"subject\":\"\",\"body\":\"b\",\"recipients\":[{\"recipient_id\":90000001,\"recipient_type\":\"character\"}]}",
        b"{\"subject\":\"s\",\"recipients\":[{\"recipient_id\":90000001,\"recipient_type\":\"character\"}]}",
        b"{\"subject\":\"s\",\"body\":\"b\"}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[]}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[{\"recipient_type\":\"character\"}]}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[{\"recipient_id\":1}]}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[{\"recipient_id\":1,\"recipient_type\":\"weird\"}]}",
    ] {
        assert_eq!(
            dispatch_character_mail_routes(&ctx, "POST", "/characters/{character_id}/mail", &p_char, &empty_q, None, 1, bad_post)
                .unwrap()
                .status_code,
            400
        );
    }

    // Blocked sender -> 403
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &params(&[("character_id", &blocked_sender_id.to_string())]),
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "subject": "Hi",
                "body": "Blocked",
                "recipients": [{"recipient_id": outsider_id, "recipient_type": "character"}]
            }))
            .unwrap(),
        )
        .unwrap()
        .status_code,
        403
    );

    // Unknown sender -> 404
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_missing_char,
            &empty_q,
            None,
            1,
            b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[{\"recipient_id\":1,\"recipient_type\":\"character\"}]}",
        )
        .unwrap()
        .status_code,
        404
    );

    // Corp not found 400, corp non-member 403, restricted corp 403, alliance not found 400, non-alliance 403, restricted alliance 403
    for (recip_id, rtype, expected) in [
        (98_999_666, "corporation", 400),
        (corp_id, "corporation", 403), // outsider_id is not in corp_id
        (99_999_666, "alliance", 400),
        (99_000_001, "alliance", 403), // outsider_id is not in alliance 99_000_001
    ] {
        assert_eq!(
            dispatch_character_mail_routes(
                &ctx,
                "POST",
                "/characters/{character_id}/mail",
                &params(&[("character_id", &outsider_id.to_string())]),
                &empty_q,
                None,
                1,
                &serde_json::to_vec(&json!({
                    "subject": "Org Test",
                    "body": "Body",
                    "recipients": [{"recipient_id": recip_id, "recipient_type": rtype}]
                }))
                .unwrap(),
            )
            .unwrap()
            .status_code,
            expected
        );
    }
    // Restricted corp / alliance mail for char_id (member without director/ceo/diplomat role)
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "subject": "Restricted Corp",
                "body": "Body",
                "recipients": [{"recipient_id": restricted_corp, "recipient_type": "corporation"}]
            }))
            .unwrap(),
        )
        .unwrap()
        .status_code,
        403
    );
    // Restricted alliance mail for non-CEO regular member of corp_id (in restricted_ally)
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &params(&[("character_id", "90888125")]),
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "subject": "Restricted Alliance",
                "body": "Body",
                "recipients": [{"recipient_id": restricted_ally, "recipient_type": "alliance"}]
            }))
            .unwrap(),
        )
        .unwrap()
        .status_code,
        403
    );

    // CSPA exceeds approved_cost (400), and insufficient ISK when approved (400)
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "approved_cost": 0,
                "subject": "CSPA",
                "body": "Body",
                "recipients": [{"recipient_id": outsider_id, "recipient_type": "character"}]
            }))
            .unwrap(),
        )
        .unwrap()
        .status_code,
        400
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = eve_db::IskCents(0);
    });
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "POST",
            "/characters/{character_id}/mail",
            &p_char,
            &empty_q,
            None,
            1,
            &serde_json::to_vec(&json!({
                "approved_cost": 100000,
                "subject": "No ISK",
                "body": "Body",
                "recipients": [{"recipient_id": outsider_id, "recipient_type": "character"}]
            }))
            .unwrap(),
        )
        .unwrap()
        .status_code,
        400
    );

    // Send mail to mailing_list + character -> exercises notify_esi_mail_sent mailing_list branch!
    let ml_sent = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        None,
        1,
        &serde_json::to_vec(&json!({
            "subject": "List Broadcast",
            "body": "Hello list",
            "recipients": [{"recipient_id": 60_001, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ml_sent.status_code, 201);

    // GET & DELETE mail/{mail_id} bad IDs & 404s
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_id.to_string()), ("mail_id", "bad")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_id.to_string()), ("mail_id", "9999999")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_mail_routes(&ctx, "DELETE", "/characters/{character_id}/mail/{mail_id}", &empty_p, &empty_q, None, 1, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_id.to_string()), ("mail_id", "bad")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", "90999777"), ("mail_id", "1")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_character_mail_routes(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_id.to_string()), ("mail_id", "9999999")]),
            &empty_q,
            None,
            1,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
}

#[test]
fn dispatch_mail_routes_post_dedup_kinds_mailing_list_refresh_and_cspa_standing() {
    let (ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let shared_id = 98_555_001_i64;
    let peer_other_id = 98_555_002_i64;
    let peer_mid_standing_id = 98_555_003_i64;
    let alliance_id = 99_555_001_i64;
    let other_corp_id = 98_555_999_i64;
    let synced_list_id = 60_555_001_i64;
    let ephem_list_id = 60_555_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut tpl = snap.characters[&char_id].clone();
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000_000);
        snap.characters.get_mut(&char_id).unwrap().alliance_id = Some(alliance_id);

        for (cid, name, charge_isk) in [
            (shared_id, "Shared Id Char", 1_500_i64),
            (peer_other_id, "Other Char", 2_000_i64),
            (peer_mid_standing_id, "Mid Standing Char", 5_000_i64),
        ] {
            tpl.character_id = cid;
            tpl.name = name.into();
            tpl.corporation_id = other_corp_id;
            tpl.alliance_id = None;
            snap.characters.insert(cid, tpl.clone());
            snap.character_cspa_charges
                .insert(cid, IskCents::from_isk(charge_isk));
        }

        snap.char_contacts.insert(
            peer_mid_standing_id,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: 0.0,
                is_blocked: false,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );

        let mut al_members = std::collections::HashSet::new();
        al_members.insert(corp_id);
        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Mail Alliance".into(),
                short_name: "MAL".into(),
                executor_corp_id: corp_id,
                member_corp_ids: al_members,
            },
        );
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.alliance_id = Some(alliance_id);
        }

        for list_id in [shared_id, synced_list_id] {
            let mut members = HashMap::new();
            members.insert(char_id, 2);
            snap.mailing_lists.insert(
                list_id,
                eve_db::MailingListRecord {
                    list_id,
                    name: format!("List {list_id}"),
                    owner_id: char_id,
                    default_access: 1,
                    default_role: 1,
                    cost: 0,
                    members,
                    entity_access: HashMap::new(),
                    welcome_mail: None,
                    mail_ids: std::collections::HashSet::new(),
                },
            );
        }
    });

    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let claims = make_claims(char_id);

    // 1. Unmatched method/path returns None (never Some(Default::default())).
    assert!(dispatch_character_mail_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/unmatched_route",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        b"",
    )
    .is_none());

    // 2. Single recipient (character) requires `!recipient_specs.iter().any(...)`.
    let single_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 1_500,
            "subject": "Single",
            "body": "Body",
            "recipients": [{"recipient_id": shared_id, "recipient_type": "character"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(single_res.status_code, 201);

    // 3. Shared numeric ID across mailing_list + character must NOT be deduped (`&&`, `kind == kind`);
    //    distinct character IDs `[shared_id, peer_other_id]` must BOTH be kept (`s.recipient_id == rid`).
    let bal_before_mix = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let mix_too_cheap = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 0,
            "subject": "Mix CSPA",
            "body": "Body",
            "recipients": [
                {"recipient_id": shared_id, "recipient_type": "mailing_list"},
                {"recipient_id": shared_id, "recipient_type": "character"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(mix_too_cheap.status_code, 400);

    let mix_ok = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 3_500,
            "subject": "Mix And Distinct Chars",
            "body": "Body",
            "recipients": [
                {"recipient_id": shared_id, "recipient_type": "mailing_list"},
                {"recipient_id": shared_id, "recipient_type": "character"},
                {"recipient_id": peer_other_id, "recipient_type": "character"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(mix_ok.status_code, 201);
    let mix_mail_id = body(&mix_ok).as_i64().unwrap();
    let bal_after_mix = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert_eq!(bal_before_mix - bal_after_mix, IskCents::from_isk(3_500).0);
    {
        let se = ctx.social_extras.lock().unwrap();
        let meta = se.mail_headers_meta.get(&mix_mail_id).unwrap();
        assert_eq!(meta.to_list_id, Some(shared_id));
        assert!(meta.to_character_ids.contains(&shared_id));
        assert!(meta.to_character_ids.contains(&peer_other_id));
    }

    // 4. All 4 recipient types (`corporation`, `alliance`, `mailing_list`, `character`) explicitly succeed.
    let corp_ok = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "subject": "Corp",
            "body": "Body",
            "recipients": [{"recipient_id": corp_id, "recipient_type": "corporation"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(corp_ok.status_code, 201);
    let corp_mid = body(&corp_ok).as_i64().unwrap();
    assert_eq!(
        ctx.social_extras.lock().unwrap().mail_headers_meta[&corp_mid].to_corp_or_alliance_id,
        Some(corp_id)
    );

    let al_ok = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "subject": "Alliance",
            "body": "Body",
            "recipients": [{"recipient_id": alliance_id, "recipient_type": "alliance"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(al_ok.status_code, 201);
    let al_mid = body(&al_ok).as_i64().unwrap();
    assert_eq!(
        ctx.social_extras.lock().unwrap().mail_headers_meta[&al_mid].to_corp_or_alliance_id,
        Some(alliance_id)
    );

    // 5. `s.kind == MailRecipientKind::MailingList`:
    //    (a) Stale social_extras mailing list (default_access=0, other owner) gets refreshed from DB when sending to mailing_list.
    //    (b) Ephemeral social_extras mailing list is NOT wiped when sending solely to character.
    {
        let mut se = ctx.social_extras.lock().unwrap();
        se.mailing_lists.insert(
            synced_list_id,
            crate::services::fleet_social::fleet_social_persistence::MailingListState {
                list_id: synced_list_id,
                name: "Stale".into(),
                owner_id: peer_other_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: HashMap::new(),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: std::collections::HashSet::new(),
            },
        );
    }
    let ml_refresh_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "subject": "ML Refresh",
            "body": "Body",
            "recipients": [{"recipient_id": synced_list_id, "recipient_type": "mailing_list"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ml_refresh_res.status_code, 201);

    {
        let mut se = ctx.social_extras.lock().unwrap();
        se.mailing_lists.insert(
            ephem_list_id,
            crate::services::fleet_social::fleet_social_persistence::MailingListState {
                list_id: ephem_list_id,
                name: "Ephemeral".into(),
                owner_id: char_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: HashMap::new(),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: std::collections::HashSet::new(),
            },
        );
    }
    let char_only_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 1_500,
            "subject": "Char Only",
            "body": "Body",
            "recipients": [{"recipient_id": shared_id, "recipient_type": "character"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(char_only_res.status_code, 201);
    assert!(
        ctx.social_extras
            .lock()
            .unwrap()
            .mailing_lists
            .contains_key(&ephem_list_id),
        "character-only mail must not trigger mailing-list DB reseed"
    );

    // 6. `cspa_exempt_min_standing: -10.0` vs `+10.0`: standing 0.0 is exempt (> -10.0), so approved_cost=0 succeeds.
    let bal_before_mid = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let cspa_mid_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &p_char,
        &empty_q,
        Some(&claims),
        1,
        &serde_json::to_vec(&json!({
            "approved_cost": 0,
            "subject": "Zero Standing Exempt",
            "body": "Body",
            "recipients": [{"recipient_id": peer_mid_standing_id, "recipient_type": "character"}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(cspa_mid_res.status_code, 201);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0),
        bal_before_mid
    );
}
