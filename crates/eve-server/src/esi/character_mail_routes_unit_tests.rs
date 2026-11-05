//! Unit and mutation-killing tests for `esi::character_mail_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

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
