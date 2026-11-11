//! Unit and mutation-killing tests for `esi::char_social_routes`.

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
fn helpers_parse_path_infer_contact_type_and_map_notification_type_id() {
    let empty = HashMap::new();
    assert_eq!(
        parse_path_i64(&empty, "character_id")
            .expect_err("missing")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("character_id", "bad")]), "character_id")
            .expect_err("nan")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("character_id", "0")]), "character_id")
            .expect_err("zero")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("character_id", "-1")]), "character_id")
            .expect_err("neg")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("character_id", "77")]), "character_id").unwrap(),
        77
    );

    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(infer_contact_type(&ctx, snap, 500_001), "faction");
        assert_eq!(infer_contact_type(&ctx, snap, 599_999), "faction");
        assert_eq!(infer_contact_type(&ctx, snap, 99_000_001), "alliance");
        assert_eq!(infer_contact_type(&ctx, snap, 99_500_000), "alliance");
        assert_eq!(infer_contact_type(&ctx, snap, corp_id), "corporation");
        assert_eq!(infer_contact_type(&ctx, snap, 1_000_125), "corporation");
        assert_eq!(infer_contact_type(&ctx, snap, 98_500_000), "corporation");
        assert_eq!(infer_contact_type(&ctx, snap, char_id), "character");
    });

    assert_eq!(map_notification_type_id(5), "AllWarDeclaredMsg");
    assert_eq!(map_notification_type_id(10), "BountyClaimMsg");
    assert_eq!(map_notification_type_id(14), "CorpAppNewMsg");
    assert_eq!(map_notification_type_id(16), "CorpAppAcceptMsg");
    assert_eq!(map_notification_type_id(34), "InsuranceFirstShipMsg");
    assert_eq!(map_notification_type_id(45), "AllAnchoringMsg");
    assert_eq!(map_notification_type_id(138), "CloneActivationMsg2");
    assert_eq!(map_notification_type_id(184), "StructureUnderAttack");
    assert_eq!(map_notification_type_id(9999), "AllWarDeclaredMsg");
}

#[test]
fn try_match_legacy_operation_strips_versions_and_extracts_path_params() {
    let m1 = try_match_legacy_operation("GET", "/v1/characters/90000001/bookmarks/")
        .expect("matches legacy char bookmarks");
    assert_eq!(m1.operation.operation_id, "GetCharactersCharacterIdBookmarks");
    assert_eq!(m1.version_prefix.as_deref(), Some("/v1"));
    assert_eq!(
        m1.path_params.get("character_id").map(String::as_str),
        Some("90000001")
    );

    let m2 = try_match_legacy_operation("GET", "/latest/corporations/98000001/bookmarks/folders")
        .expect("matches legacy corp bookmark folders");
    assert_eq!(
        m2.operation.operation_id,
        "GetCorporationsCorporationIdBookmarkFolders"
    );
    assert_eq!(m2.version_prefix.as_deref(), Some("/latest"));
    assert_eq!(
        m2.path_params.get("corporation_id").map(String::as_str),
        Some("98000001")
    );

    let m3 = try_match_legacy_operation("GET", "/opportunities/groups")
        .expect("matches opportunities groups");
    assert_eq!(m3.operation.operation_id, "GetOpportunitiesGroups");
    assert_eq!(m3.version_prefix, None);

    assert!(try_match_legacy_operation("POST", "/opportunities/groups").is_none());
    assert!(try_match_legacy_operation("GET", "/nonexistent/legacy/route").is_none());
    assert!(try_match_legacy_operation("GET", "/").is_none());
}

#[test]
fn dispatch_character_public_history_cspa_notifications_and_research() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let empty_q = HashMap::new();

    let pub_info = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(pub_info.status_code, 200);
    assert_eq!(body(&pub_info)["corporation_id"], corp_id);
    assert!(body(&pub_info)["gender"] == "male" || body(&pub_info)["gender"] == "female");

    assert_eq!(
        dispatch_char_social_route(
            &ctx,
            "GET",
            "/characters/{character_id}",
            &params(&[("character_id", "99999999")]),
            &empty_q,
            b""
        )
        .unwrap()
        .status_code,
        404
    );

    if let Ok(mut h) = ctx.esi_state.char_corp_history.write() {
        h.insert(
            char_id,
            vec![
                crate::esi::runtime_state::EsiCorpHistoryEntry {
                    record_id: 10,
                    corporation_id: 1_000_009,
                    start_date_unix: 1_690_000_000,
                    is_deleted: true,
                },
                crate::esi::runtime_state::EsiCorpHistoryEntry {
                    record_id: 20,
                    corporation_id: corp_id,
                    start_date_unix: 1_700_000_000,
                    is_deleted: false,
                },
            ],
        );
    }
    let hist = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/corporationhistory",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(hist.status_code, 200);
    let hist_arr = body(&hist).as_array().unwrap();
    assert_eq!(hist_arr.len(), 2);
    assert_eq!(hist_arr[0]["record_id"], 20);
    assert_eq!(hist_arr[1]["record_id"], 10);
    assert_eq!(hist_arr[1]["is_deleted"], true);

    assert_eq!(
        dispatch_char_social_route(
            &ctx,
            "POST",
            "/characters/{character_id}/cspa",
            &p_char,
            &empty_q,
            b"bad"
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_char_social_route(
            &ctx,
            "POST",
            "/characters/{character_id}/cspa",
            &p_char,
            &empty_q,
            b"[]"
        )
        .unwrap()
        .status_code,
        400
    );
    let over_100: Vec<i64> = (1..=101).collect();
    assert_eq!(
        dispatch_char_social_route(
            &ctx,
            "POST",
            "/characters/{character_id}/cspa",
            &p_char,
            &empty_q,
            &serde_json::to_vec(&over_100).unwrap()
        )
        .unwrap()
        .status_code,
        400
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.character_cspa_charges
            .insert(90_000_777, eve_db::IskCents::from_isk(2_950));
    });
    let cspa = dispatch_char_social_route(
        &ctx,
        "POST",
        "/characters/{character_id}/cspa",
        &p_char,
        &empty_q,
        br"[90000001, 90000002, 90000777]",
    )
    .unwrap();
    assert_eq!(cspa.status_code, 201);
    assert_eq!(body(&cspa).as_f64().unwrap(), 2950.0);

    for path in [
        "/characters/{character_id}/notifications",
        "/characters/{character_id}/notifications/contacts",
        "/characters/{character_id}/agents_research",
    ] {
        let res = dispatch_char_social_route(&ctx, "GET", path, &p_char, &empty_q, b"").unwrap();
        assert_eq!(res.status_code, 200);
        assert!(body(&res).is_array());
    }
}

#[test]
fn dispatch_medals_roles_titles_bookmarks_projects_and_opportunities() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_medals.insert(
            char_id,
            vec![eve_db::CharacterMedalRecord {
                medal_id: 1,
                title: "Valor".to_string(),
                description: "Brave".to_string(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: 1_700_000_000,
                reason: "Victory".to_string(),
                status: "public".to_string(),
                graphics: vec![],
            }],
        );
    });

    let medals = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/medals",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(medals.status_code, 200);
    assert!(!body(&medals).as_array().unwrap().is_empty());

    let roles = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/roles",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(roles.status_code, 200);
    assert!(body(&roles)["roles"]
        .as_array()
        .unwrap()
        .contains(&json!("Director")));

    let titles = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/titles",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(titles.status_code, 200);
    assert!(body(&titles).is_array());

    for (path, p) in [
        ("/characters/{character_id}/bookmarks", &p_char),
        ("/characters/{character_id}/bookmarks/folders", &p_char),
        ("/corporations/{corporation_id}/bookmarks", &p_corp),
        ("/corporations/{corporation_id}/bookmarks/folders", &p_corp),
    ] {
        let res = dispatch_char_social_route(&ctx, "GET", path, p, &empty_q, b"").unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
        assert!(body(&res).is_array(), "expected array on {path}");
    }
    assert!(
        dispatch_char_social_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/projects",
            &p_corp,
            &empty_q,
            b"",
        )
        .is_none(),
        "corporation projects route is dispatched by corp_governance_routes"
    );

    let opp_groups = dispatch_char_social_route(
        &ctx,
        "GET",
        "/opportunities/groups",
        &empty_q,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(opp_groups.status_code, 200);
    assert!(!body(&opp_groups).as_array().unwrap().is_empty());

    let opp_tasks =
        dispatch_char_social_route(&ctx, "GET", "/opportunities/tasks", &empty_q, &empty_q, b"")
            .unwrap();
    assert_eq!(opp_tasks.status_code, 200);
    assert!(!body(&opp_tasks).as_array().unwrap().is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.npe_progress.entry(char_id).or_insert_with(|| eve_db::NpeProgressRecord {
            character_id: char_id,
            node_graph_id: 1,
            current_node_id: 1,
            completed: false,
            career_goal_progress: HashMap::new(),
            claimed_goals: std::collections::HashSet::new(),
        });
        entry.claimed_goals.insert(3);
        entry.claimed_goals.insert(1);
    });
    let char_opps = dispatch_char_social_route(
        &ctx,
        "GET",
        "/characters/{character_id}/opportunities",
        &p_char,
        &empty_q,
        b"",
    )
    .unwrap();
    assert_eq!(char_opps.status_code, 200);
    let opp_arr = body(&char_opps).as_array().unwrap();
    assert!(opp_arr.len() >= 2);
    assert_eq!(opp_arr[0]["task_id"], 1);
}

#[test]
fn corp_bookmarks_membership_and_cspa_alliance_and_standing_boundaries() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let other_corp_id = 98_000_555_i64;
    let member_via_set_id = 90_000_201_i64;
    let outsider_id = 90_000_202_i64;
    let ally_diff_corp_id = 90_000_203_i64;
    let neg10_id = 90_000_204_i64;
    let neg99_id = 90_000_205_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let alliance_id = snap.characters[&char_id].alliance_id.unwrap();
        let mut base = snap.characters[&char_id].clone();

        // Caller is NOT ch.corporation_id, NOT ceo_id, NOT in member_roles, ONLY in corp.members!
        base.character_id = member_via_set_id;
        base.name = "CorpMembersOnly".into();
        base.corporation_id = other_corp_id;
        base.alliance_id = None;
        snap.characters.insert(member_via_set_id, base.clone());
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(member_via_set_id);
        }

        // Outsider in another corp and not in corp.members
        base.character_id = outsider_id;
        base.name = "CorpOutsider".into();
        snap.characters.insert(outsider_id, base.clone());

        // Same-alliance, different-corp character with a CSPA fee (must STILL be charged!)
        base.character_id = ally_diff_corp_id;
        base.name = "SameAllianceDiffCorp".into();
        base.corporation_id = other_corp_id;
        base.alliance_id = Some(alliance_id);
        snap.characters.insert(ally_diff_corp_id, base.clone());
        snap.character_cspa_charges
            .insert(ally_diff_corp_id, eve_db::IskCents::from_isk(2_500));

        // -10.0 standing boundary (NOT > -10.0 -> charged) vs -9.9 (> -10.0 -> exempt)
        base.character_id = neg10_id;
        base.name = "Neg10Target".into();
        base.alliance_id = None;
        snap.characters.insert(neg10_id, base.clone());
        snap.character_cspa_charges
            .insert(neg10_id, eve_db::IskCents::from_isk(1_000));
        snap.char_contacts.insert(
            neg10_id,
            vec![eve_db::SocialContactRecord {
                contact_id: char_id,
                standing: -10.0,
                is_blocked: false,
                is_watched: false,
                label_ids: Vec::new(),
                contact_type: "character".into(),
            }],
        );

        base.character_id = neg99_id;
        base.name = "Neg99Target".into();
        snap.characters.insert(neg99_id, base);
        snap.character_cspa_charges
            .insert(neg99_id, eve_db::IskCents::from_isk(3_000));
        snap.char_contacts.insert(
            neg99_id,
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
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let mk_claims = |cid: i64| eve_db::SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{cid}"),
        name: "SocialTester".into(),
        owner: "h".into(),
        user_id: 1,
        character_id: cid,
        scp: Vec::new(),
        iss: "evomu".into(),
        aud: vec!["EVE Online".into()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "jti-social".into(),
    };

    // 1. esi_caller_in_corp accepts caller solely via corp.members.contains(&caller_cid), rejects outsider.
    let ok_bm = dispatch_char_social_route_with_claims(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/bookmarks",
        &p_corp,
        &empty_q,
        Some(&mk_claims(member_via_set_id)),
        b"",
    )
    .unwrap();
    assert_eq!(ok_bm.status_code, 200);

    let denied_bm = dispatch_char_social_route_with_claims(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/bookmarks",
        &p_corp,
        &empty_q,
        Some(&mk_claims(outsider_id)),
        b"",
    )
    .unwrap();
    assert_eq!(denied_bm.status_code, 403);

    // 2. POST /characters/{id}/cspa: same-alliance different-corp target is NOT exempt (exempt_on_same_alliance: false).
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let ally_cspa = dispatch_char_social_route(
        &ctx,
        "POST",
        "/characters/{character_id}/cspa",
        &p_char,
        &empty_q,
        &serde_json::to_vec(&vec![ally_diff_corp_id]).unwrap(),
    )
    .unwrap();
    assert_eq!(ally_cspa.status_code, 201);
    assert!((body(&ally_cspa).as_f64().unwrap() - 2_500.0).abs() < 1e-6);

    // 3. Standing boundary: -10.0 charges 1_000.0, -9.9 charges 0.0.
    let neg10_cspa = dispatch_char_social_route(
        &ctx,
        "POST",
        "/characters/{character_id}/cspa",
        &p_char,
        &empty_q,
        &serde_json::to_vec(&vec![neg10_id]).unwrap(),
    )
    .unwrap();
    assert_eq!(neg10_cspa.status_code, 201);
    assert!((body(&neg10_cspa).as_f64().unwrap() - 1_000.0).abs() < 1e-6);

    let neg99_cspa = dispatch_char_social_route(
        &ctx,
        "POST",
        "/characters/{character_id}/cspa",
        &p_char,
        &empty_q,
        &serde_json::to_vec(&vec![neg99_id]).unwrap(),
    )
    .unwrap();
    assert_eq!(neg99_cspa.status_code, 201);
    assert!((body(&neg99_cspa).as_f64().unwrap() - 0.0).abs() < 1e-6);
}
