//! Unit and mutation-killing tests for `esi::char_social_mail_routes`.

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

#[test]
fn parse_path_i64_and_parse_csv_i64_validate_all_edge_cases() {
    let empty = HashMap::new();
    assert_eq!(
        parse_path_i64(&empty, "id").expect_err("missing").status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("id", "xyz")]), "id")
            .expect_err("nan")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("id", "0")]), "id")
            .expect_err("zero")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("id", "-5")]), "id")
            .expect_err("neg")
            .status_code,
        400
    );
    assert_eq!(parse_path_i64(&params(&[("id", "42")]), "id").unwrap(), 42);

    assert_eq!(parse_csv_i64("").unwrap(), Vec::<i64>::new());
    assert_eq!(parse_csv_i64("  [] ").unwrap(), Vec::<i64>::new());
    assert_eq!(parse_csv_i64("[10, 20, 30]").unwrap(), vec![10, 20, 30]);
    assert_eq!(
        parse_csv_i64("10,abc").expect_err("bad int").status_code,
        400
    );
    assert_eq!(
        parse_csv_i64("10,0").expect_err("zero id").status_code,
        400
    );
    assert_eq!(
        parse_csv_i64("10,-3").expect_err("neg id").status_code,
        400
    );
    assert_eq!(VALID_MAIL_LABEL_COLORS.len(), 18);
    assert!(VALID_MAIL_LABEL_COLORS.contains(&"#ffffff"));
}

#[test]
fn character_contacts_crud_and_validation_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();

    assert!(dispatch_char_social_mail_route(
        &ctx,
        "PATCH",
        "/characters/{character_id}/contacts",
        &p_char,
        &empty_q,
        b""
    )
    .is_none());

    let page2 = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/characters/{character_id}/contacts",
        &p_char,
        &params(&[("page", "2")]),
        b"",
    )
    .unwrap();
    assert_eq!(page2.status_code, 200);
    assert_eq!(body(&page2), &json!([]));

    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "POST",
            "/characters/{character_id}/contacts",
            &p_char,
            &empty_q,
            b"[90000002]"
        )
        .unwrap()
        .status_code,
        400
    );
    for bad_s in ["-10.1", "10.1", "notanumber"] {
        assert_eq!(
            dispatch_char_social_mail_route(
                &ctx,
                "POST",
                "/characters/{character_id}/contacts",
                &p_char,
                &params(&[("standing", bad_s)]),
                b"[90000002]"
            )
            .unwrap()
            .status_code,
            400
        );
    }
    for bad_ids in [&b"[]"[..], &b"[0]"[..]] {
        assert_eq!(
            dispatch_char_social_mail_route(
                &ctx,
                "POST",
                "/characters/{character_id}/contacts",
                &p_char,
                &params(&[("standing", "5.0")]),
                bad_ids
            )
            .unwrap()
            .status_code,
            400
        );
    }
    let many_ids: Vec<i64> = (1..=101).collect();
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "POST",
            "/characters/{character_id}/contacts",
            &p_char,
            &params(&[("standing", "5.0")]),
            &serde_json::to_vec(&many_ids).unwrap()
        )
        .unwrap()
        .status_code,
        400
    );

    let post_body =
        serde_json::to_vec(&vec![90_000_002_i64, corp_id, 99_000_001, 500_001]).unwrap();
    let post_ok = dispatch_char_social_mail_route(
        &ctx,
        "POST",
        "/characters/{character_id}/contacts",
        &p_char,
        &params(&[
            ("standing", "7.5"),
            ("watched", "true"),
            ("label_ids", "[2, 1]"),
        ]),
        &post_body,
    )
    .unwrap();
    assert_eq!(post_ok.status_code, 201);
    assert_eq!(body(&post_ok).as_array().unwrap().len(), 4);

    let put_ok = dispatch_char_social_mail_route(
        &ctx,
        "PUT",
        "/characters/{character_id}/contacts",
        &p_char,
        &params(&[
            ("standing", "-5.0"),
            ("watched", "1"),
            ("label_ids", "1"),
        ]),
        br"[90000002, 90000099]",
    )
    .unwrap();
    assert_eq!(put_ok.status_code, 204);

    let get_ok = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/characters/{character_id}/contacts",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(get_ok.status_code, 200);
    let contacts = body(&get_ok).as_array().unwrap();
    let c2 = contacts
        .iter()
        .find(|c| c["contact_id"] == 90_000_002)
        .expect("90000002 present");
    assert_eq!(c2["standing"], -5.0);
    assert_eq!(c2["contact_type"], "character");
    assert_eq!(c2["is_watched"], true);
    assert_eq!(c2["label_ids"], json!([1]));

    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/contacts",
            &p_char,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        400
    );
    let twenty_one = (1..=21)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/contacts",
            &p_char,
            &params(&[("contact_ids", &twenty_one)]),
            b""
        )
        .unwrap()
        .status_code,
        400
    );
    let del_ok = dispatch_char_social_mail_route(
        &ctx,
        "DELETE",
        "/characters/{character_id}/contacts",
        &p_char,
        &params(&[("contact_ids", "90000099")]),
        b"",
    )
    .unwrap();
    assert_eq!(del_ok.status_code, 204);
}

#[test]
fn contact_labels_and_org_contacts_routes_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contact_labels
            .insert(char_id, vec![(1, "Friends".to_string())]);
        snap.corp_contact_labels
            .insert(corp_id, vec![(1, "Corp Allies".to_string(), 0)]);
        snap.alliance_contact_labels
            .insert(99_000_001, vec![(1, "Coalition".to_string())]);
    });

    let char_labels = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/characters/{character_id}/contacts/labels",
        &params(&[("character_id", &char_id.to_string())]),
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(char_labels.status_code, 200);
    assert!(!body(&char_labels).as_array().unwrap().is_empty());

    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/contacts",
            &params(&[("corporation_id", "99999999")]),
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/contacts",
            &p_corp,
            &params(&[("page", "0")]),
            b""
        )
        .unwrap()
        .status_code,
        400
    );
    let corp_contacts = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/contacts",
        &p_corp,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(corp_contacts.status_code, 200);

    let corp_labels = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/contacts/labels",
        &p_corp,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(corp_labels.status_code, 200);
    assert_eq!(body(&corp_labels)[0]["label_name"], "Corp Allies");

    let p_ally = params(&[("alliance_id", "99000001")]);
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "GET",
            "/alliances/{alliance_id}/contacts",
            &params(&[("alliance_id", "99999999")]),
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    let ally_contacts = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/contacts",
        &p_ally,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(ally_contacts.status_code, 200);

    let ally_labels = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/contacts/labels",
        &p_ally,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(ally_labels.status_code, 200);
    assert_eq!(body(&ally_labels)[0]["label_name"], "Coalition");
}

#[test]
fn mail_put_labels_crud_and_mailing_lists_cover_all_branches() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();

    let mail_id = 55_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(eve_db::MailRecord {
            message_id: mail_id,
            sender_id: 90_000_002,
            recipient_id: char_id,
            subject: "Hello".to_string(),
            body: "World".to_string(),
        });
        snap.mail_timestamps.insert(mail_id, 1_700_000_000);
        snap.mail_assigned_labels.insert((char_id, mail_id), vec![1]);
    });

    let p_mail = params(&[("character_id", &char_str), ("mail_id", &mail_id.to_string())]);
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            br"{}"
        )
        .unwrap()
        .status_code,
        400
    );
    let too_many_labels: Vec<i64> = (1..=26).collect();
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            &serde_json::to_vec(&json!({ "labels": too_many_labels })).unwrap()
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            br#"{"labels":[0]}"#
        )
        .unwrap()
        .status_code,
        400
    );

    let put_mail = dispatch_char_social_mail_route(
        &ctx,
        "PUT",
        "/characters/{character_id}/mail/{mail_id}",
        &p_mail,
        &empty_q,
        br#"{"read":false,"labels":[1,4]}"#,
    )
    .unwrap();
    assert_eq!(put_mail.status_code, 204);

    let get_labels = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/characters/{character_id}/mail/labels",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(get_labels.status_code, 200);
    assert_eq!(body(&get_labels)["total_unread_count"], 1);

    for bad_lbl in [
        json!({"name": "   "}),
        json!({"name": "a".repeat(41)}),
        json!({"name": "Custom", "color": "#123456"}),
    ] {
        assert_eq!(
            dispatch_char_social_mail_route(
                &ctx,
                "POST",
                "/characters/{character_id}/mail/labels",
                &p_char,
                &empty_q,
                &serde_json::to_vec(&bad_lbl).unwrap()
            )
            .unwrap()
            .status_code,
            400
        );
    }

    let created_label = dispatch_char_social_mail_route(
        &ctx,
        "POST",
        "/characters/{character_id}/mail/labels",
        &p_char,
        &empty_q,
        br##"{"name":"Important Ops","color":"#ff6600"}"##,
    )
    .unwrap();
    assert_eq!(created_label.status_code, 201);
    let new_label_id = body(&created_label).as_i64().expect("label id integer");
    assert!(new_label_id >= 16);

    let dup_label = dispatch_char_social_mail_route(
        &ctx,
        "POST",
        "/characters/{character_id}/mail/labels",
        &p_char,
        &empty_q,
        br##"{"name":"important ops","color":"#ff6600"}"##,
    )
    .unwrap();
    assert_eq!(dup_label.status_code, 201);
    assert_eq!(body(&dup_label).as_i64().unwrap(), new_label_id * 2);

    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/labels/{label_id}",
            &params(&[("character_id", &char_str), ("label_id", "1")]),
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        422
    );
    let p_del_lbl = params(&[
        ("character_id", &char_str),
        ("label_id", &new_label_id.to_string()),
    ]);
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/labels/{label_id}",
            &p_del_lbl,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/labels/{label_id}",
            &p_del_lbl,
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        404
    );

    let lists = dispatch_char_social_mail_route(
        &ctx,
        "GET",
        "/characters/{character_id}/mail/lists",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(lists.status_code, 200);
    assert!(body(&lists).is_array());
}

#[test]
fn char_social_mail_errors_corp_alliance_contacts_mail_update_labels_and_lists() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_missing_char = params(&[("character_id", "90999888")]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let p_missing_corp = params(&[("corporation_id", "98999888")]);
    let p_ally = params(&[("alliance_id", "99000001")]);
    let p_missing_ally = params(&[("alliance_id", "99999888")]);
    let bad_page_999 = params(&[("page", "999")]);

    for (method, path, body_bytes, q) in [
        ("POST", "/characters/{character_id}/contacts", b"[90000002]".as_slice(), params(&[("standing", "1")])),
        ("DELETE", "/characters/{character_id}/contacts", b"".as_slice(), params(&[("contact_ids", "90000002")])),
        ("GET", "/characters/{character_id}/contacts/labels", b"".as_slice(), empty_q.clone()),
        ("GET", "/corporations/{corporation_id}/contacts", b"".as_slice(), empty_q.clone()),
        ("GET", "/corporations/{corporation_id}/contacts/labels", b"".as_slice(), empty_q.clone()),
        ("GET", "/alliances/{alliance_id}/contacts", b"".as_slice(), empty_q.clone()),
        ("GET", "/alliances/{alliance_id}/contacts/labels", b"".as_slice(), empty_q.clone()),
        ("PUT", "/characters/{character_id}/mail/{mail_id}", b"{\"read\":true}".as_slice(), empty_q.clone()),
        ("GET", "/characters/{character_id}/mail/labels", b"".as_slice(), empty_q.clone()),
        ("POST", "/characters/{character_id}/mail/labels", b"{\"name\":\"Ops\"}".as_slice(), empty_q.clone()),
        ("DELETE", "/characters/{character_id}/mail/labels/{label_id}", b"".as_slice(), empty_q.clone()),
        ("GET", "/characters/{character_id}/mail/lists", b"".as_slice(), empty_q.clone()),
    ] {
        assert_eq!(
            dispatch_char_social_mail_route(&ctx, method, path, &empty_p, &q, body_bytes)
                .unwrap()
                .status_code,
            400,
            "missing path param 400 on {method} {path}"
        );
        let missing_p = if path.contains("corporation_id") {
            p_missing_corp.clone()
        } else if path.contains("alliance_id") {
            p_missing_ally.clone()
        } else if path.contains("mail_id") {
            params(&[("character_id", "90999888"), ("mail_id", "1")])
        } else if path.contains("label_id") {
            params(&[("character_id", "90999888"), ("label_id", "64")])
        } else {
            p_missing_char.clone()
        };
        assert_eq!(
            dispatch_char_social_mail_route(&ctx, method, path, &missing_p, &q, body_bytes)
                .unwrap()
                .status_code,
            404,
            "404 on {method} {path}"
        );
    }

    // Contact CRUD validation errors
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "POST",
            "/characters/{character_id}/contacts",
            &p_char,
            &params(&[("standing", "1"), ("label_ids", "bad")]),
            b"[90000002]",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "POST",
            "/characters/{character_id}/contacts",
            &p_char,
            &params(&[("standing", "1")]),
            b"not-json",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/contacts",
            &p_char,
            &params(&[("contact_ids", "bad")]),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "GET",
            "/alliances/{alliance_id}/contacts",
            &p_ally,
            &params(&[("page", "0")]),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );

    // Seed corp/alliance contacts, unread mail, custom DB mail label, and mailing list
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.insert(
            corp_id,
            vec![eve_db::SocialContactRecord {
                contact_id: 90_000_002,
                contact_type: "character".to_string(),
                standing: 5.0,
                is_watched: true,
                is_blocked: false,
                label_ids: vec![1],
            }],
        );
        snap.alliance_contacts.insert(
            99_000_001,
            vec![eve_db::SocialContactRecord {
                contact_id: corp_id,
                contact_type: "corporation".to_string(),
                standing: 10.0,
                is_watched: false,
                is_blocked: false,
                label_ids: vec![2],
            }],
        );
        snap.mail_labels.insert(
            char_id,
            vec![eve_db::MailLabelRecord {
                label_id: 128,
                name: "DbOnlyLabel".to_string(),
                color: "#ffffff".to_string(),
            }],
        );
        snap.mails.push(eve_db::MailRecord {
            message_id: 777_001,
            sender_id: 90_000_002,
            recipient_id: char_id,
            subject: "Unread Social Mail".to_string(),
            body: "Body".to_string(),
        });
        snap.mail_timestamps.insert(777_001, 1_700_000_000);
        snap.mail_assigned_labels.insert((char_id, 777_001), vec![128]);
        let mut members = HashMap::new();
        members.insert(char_id, 0);
        snap.mailing_lists.insert(
            50_001,
            eve_db::MailingListRecord {
                list_id: 50_001,
                name: "Fleet Ops List".to_string(),
                owner_id: char_id,
                default_access: 0,
                default_role: 0,
                cost: 0,
                members,
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: std::collections::HashSet::new(),
            },
        );
    });

    // Corp & alliance contacts populated + page 999 -> 404
    let cc = dispatch_char_social_mail_route(&ctx, "GET", "/corporations/{corporation_id}/contacts", &p_corp, &empty_q, b"").unwrap();
    assert_eq!(cc.status_code, 200);
    assert_eq!(body(&cc)[0]["label_ids"], json!([1]));
    assert_eq!(
        dispatch_char_social_mail_route(&ctx, "GET", "/corporations/{corporation_id}/contacts", &p_corp, &bad_page_999, b"")
            .unwrap()
            .status_code,
        404
    );

    let ac = dispatch_char_social_mail_route(&ctx, "GET", "/alliances/{alliance_id}/contacts", &p_ally, &empty_q, b"").unwrap();
    assert_eq!(ac.status_code, 200);
    assert_eq!(body(&ac)[0]["label_ids"], json!([2]));
    assert_eq!(
        dispatch_char_social_mail_route(&ctx, "GET", "/alliances/{alliance_id}/contacts", &p_ally, &bad_page_999, b"")
            .unwrap()
            .status_code,
        404
    );

    // Mail labels (custom DB-only label + unread count)
    let lbls = dispatch_char_social_mail_route(&ctx, "GET", "/characters/{character_id}/mail/labels", &p_char, &empty_q, b"").unwrap();
    assert_eq!(lbls.status_code, 200);
    assert!(body(&lbls)["total_unread_count"].as_i64().unwrap() >= 1);
    assert!(body(&lbls)["labels"].as_array().unwrap().iter().any(|l| l["label_id"] == 128));

    // PUT mail validation + 404 + read=false/labels=[128]
    let p_mail = params(&[("character_id", &char_str(&char_id)), ("mail_id", "777001")]);
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_str(&char_id)), ("mail_id", "bad")]),
            &empty_q,
            b"{\"read\":true}",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(&ctx, "PUT", "/characters/{character_id}/mail/{mail_id}", &p_mail, &empty_q, b"bad")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(&ctx, "PUT", "/characters/{character_id}/mail/{mail_id}", &p_mail, &empty_q, b"{\"labels\":[\"str\"]}")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &params(&[("character_id", &char_str(&char_id)), ("mail_id", "999999")]),
            &empty_q,
            b"{\"read\":true}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "PUT",
            "/characters/{character_id}/mail/{mail_id}",
            &p_mail,
            &empty_q,
            b"{\"read\":false,\"labels\":[128]}",
        )
        .unwrap()
        .status_code,
        204
    );

    // POST/DELETE label bad id + bad json + DB-only delete
    assert_eq!(
        dispatch_char_social_mail_route(&ctx, "POST", "/characters/{character_id}/mail/labels", &p_char, &empty_q, b"bad")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/labels/{label_id}",
            &params(&[("character_id", &char_str(&char_id)), ("label_id", "bad")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_mail_route(
            &ctx,
            "DELETE",
            "/characters/{character_id}/mail/labels/{label_id}",
            &params(&[("character_id", &char_str(&char_id)), ("label_id", "128")]),
            &empty_q,
            b"",
        )
        .unwrap()
        .status_code,
        204
    );

    // Mailing lists populated
    let lists = dispatch_char_social_mail_route(&ctx, "GET", "/characters/{character_id}/mail/lists", &p_char, &empty_q, b"").unwrap();
    assert_eq!(lists.status_code, 200);
    assert_eq!(body(&lists)[0]["mailing_list_id"], 50_001);
}

fn char_str(id: &i64) -> String {
    id.to_string()
}
