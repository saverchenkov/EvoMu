//! Unit and mutation-killing tests for `esi::corp_governance_routes`.

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
fn decode_corp_roles_and_parse_path_i64_cover_all_role_masks_and_errors() {
    assert!(decode_corp_roles(0).is_empty());
    assert!(
        decode_corp_roles(eve_db::corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER).is_empty()
    );

    let mask = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_ACCOUNTANT
        | eve_db::corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER
        | eve_db::corp_roles::ROLE_SKILL_PLAN_MANAGER;
    let decoded = decode_corp_roles(mask);
    assert_eq!(
        decoded,
        vec!["Director", "Accountant", "Skill_Plan_Manager"]
    );

    let all_roles = decode_corp_roles(u64::MAX);
    assert_eq!(all_roles.len(), CORP_ROLE_DEFINITIONS.len() - 1);
    assert!(!all_roles.contains(&""));

    let empty = HashMap::new();
    assert_eq!(
        parse_path_i64(&empty, "corporation_id")
            .expect_err("missing")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "bad")]), "corporation_id")
            .expect_err("nan")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "0")]), "corporation_id")
            .expect_err("zero")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "-10")]), "corporation_id")
            .expect_err("neg")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "55")]), "corporation_id").unwrap(),
        55
    );
}

#[test]
fn dispatch_alliance_detail_history_logs_divisions_medals_and_limits() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let corp_str = corp_id.to_string();
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    assert!(dispatch_corp_governance_route(
        &ctx,
        "DELETE",
        "/alliances/{alliance_id}",
        &params(&[("alliance_id", "99000001")]),
        &empty_q
    )
    .is_none());

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliance_lifecycle_meta.insert(
            99_000_001,
            eve_db::AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: 1_700_000_000,
                faction_id: Some(500_001),
            },
        );
    });
    let ally = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}",
        &params(&[("alliance_id", "99000001")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(ally.status_code, 200);
    assert_eq!(body(&ally)["executor_corporation_id"], corp_id);

    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/alliances/{alliance_id}",
            &params(&[("alliance_id", "99999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_container_logs.insert(
            corp_id,
            vec![eve_db::CorpContainerLogEntry {
                logged_at_unix: 1_700_000_000,
                container_id: 100_500,
                container_type_id: 3296,
                character_id: char_id,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".to_string(),
                action: "lock".to_string(),
                password_type: Some("config".to_string()),
                type_id: Some(34),
                quantity: Some(1000),
                old_config_bitmask: Some(1),
                new_config_bitmask: Some(3),
            }],
        );
        snap.corp_member_limits.insert(corp_id, 650);
    });

    for path in [
        "/corporations/{corporation_id}/alliancehistory",
        "/corporations/{corporation_id}/containers/logs",
        "/corporations/{corporation_id}/divisions",
        "/corporations/{corporation_id}/medals",
        "/corporations/{corporation_id}/medals/issued",
        "/corporations/{corporation_id}/members/limit",
    ] {
        let res = dispatch_corp_governance_route(&ctx, "GET", path, &p_corp, &empty_q).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    let lim_res = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&lim_res), &json!(650));

    let logs_res = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/containers/logs",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&logs_res)[0]["container_id"], 100_500);
    assert_eq!(body(&logs_res)[0]["password_type"], "config");
}

#[test]
fn dispatch_titles_roles_history_shareholders_starbase_detail_and_projects() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let corp_str = corp_id.to_string();
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    for path in [
        "/corporations/{corporation_id}/members/titles",
        "/corporations/{corporation_id}/titles",
        "/corporations/{corporation_id}/roles/history",
        "/corporations/{corporation_id}/shareholders",
        "/corporations/{corporation_id}/projects",
    ] {
        let res = dispatch_corp_governance_route(&ctx, "GET", path, &p_corp, &empty_q).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    let tower_id = 889_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.starbase_settings
            .insert(tower_id, eve_db::StarbaseDetailSettings::default());
        snap.corp_shareholders.insert(
            corp_id,
            vec![eve_db::CorpShareholderEntry {
                shareholder_id: char_id,
                shareholder_type: "character".to_string(),
                share_count: 1000,
            }],
        );
        snap.items.insert(
            tower_id,
            eve_db::InventoryItemRecord {
                item_id: tower_id,
                type_id: 12235,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            889_002,
            eve_db::InventoryItemRecord {
                item_id: 889_002,
                type_id: 4247,
                owner_id: corp_id,
                location_id: tower_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 250,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
    });

    let p_sb = params(&[
        ("corporation_id", &corp_str),
        ("starbase_id", &tower_id.to_string()),
    ]);
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &p_sb,
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &p_sb,
            &params(&[("system_id", "30002187")])
        )
        .unwrap()
        .status_code,
        404
    );
    let sb_ok = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/starbases/{starbase_id}",
        &p_sb,
        &params(&[("system_id", "30000142")]),
    )
    .unwrap();
    assert_eq!(sb_ok.status_code, 200);
    assert_eq!(body(&sb_ok)["fuels"][0]["type_id"], 4247);
    assert_eq!(body(&sb_ok)["fuels"][0]["quantity"], 250);

    let sh_ok = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/shareholders",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(sh_ok.status_code, 200);
    assert!(body(&sh_ok)
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["shareholder_id"] == char_id));
}

#[test]
fn corp_governance_errors_limits_titles_starbase_and_project_fallbacks() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let p_missing_ally = params(&[("alliance_id", "99999998")]);
    let p_missing_corp = params(&[("corporation_id", "98999998")]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let bad_page = params(&[("page", "999")]);

    for path in [
        "/alliances/{alliance_id}",
        "/corporations/{corporation_id}/alliancehistory",
        "/corporations/{corporation_id}/containers/logs",
        "/corporations/{corporation_id}/divisions",
        "/corporations/{corporation_id}/medals",
        "/corporations/{corporation_id}/medals/issued",
        "/corporations/{corporation_id}/members/limit",
        "/corporations/{corporation_id}/members/titles",
        "/corporations/{corporation_id}/titles",
        "/corporations/{corporation_id}/shareholders",
        "/corporations/{corporation_id}/projects",
    ] {
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, &empty_p, &empty_q)
                .unwrap()
                .status_code,
            400,
            "missing param 400 on {path}"
        );
        let pm = if path.contains("alliance_id") {
            &p_missing_ally
        } else {
            &p_missing_corp
        };
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, pm, &empty_q)
                .unwrap()
                .status_code,
            404,
            "404 on {path}"
        );
    }

    for path in [
        "/corporations/{corporation_id}/containers/logs",
        "/corporations/{corporation_id}/medals",
        "/corporations/{corporation_id}/medals/issued",
        "/corporations/{corporation_id}/shareholders",
        "/corporations/{corporation_id}/projects",
    ] {
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, &p_corp, &bad_page)
                .unwrap()
                .status_code,
            404,
            "bad page 404 on {path}"
        );
    }

    // Starbase detail validation / 404 branches
    let sb_bad_p = params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "bad")]);
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &sb_bad_p,
            &params(&[("system_id", "30000142")]),
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "99")]),
            &params(&[("system_id", "notint")]),
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &params(&[("corporation_id", "98999998"), ("starbase_id", "99")]),
            &params(&[("system_id", "30000142")]),
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "9999999")]),
            &params(&[("system_id", "30000142")]),
        )
        .unwrap()
        .status_code,
        404
    );

    let solo_corp = 98_888_001_i64;
    let solo_ceo = 90_888_001_i64;
    let tower_no_settings = 889_099_i64;
    let bonus_skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.contains_key(&191))
        .map(|t| t.type_id)
        .expect("corp management bonus skill");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = solo_ceo;
            c.name = "Gov Ceo".to_string();
            c.corporation_id = solo_corp;
            c.alliance_id = None;
            snap.characters.insert(solo_ceo, c);
        }
        snap.corporations.insert(
            solo_corp,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = solo_corp; __corp.name = "GovSolo".to_string(); __corp.ticker = "GOVS".to_string(); __corp.ceo_id = solo_ceo; __corp.tax_rate = 0.0; __corp.hq_station_id = None; __corp.alliance_id = None; __corp.members = Default::default(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    eve_db::IskCents(0),
                ); __corp.description = String::new(); __corp.url = String::new(); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.corp_member_limits.insert(solo_corp, 77);
        snap.items.insert(
            tower_no_settings,
            eve_db::InventoryItemRecord {
                item_id: tower_no_settings,
                type_id: 12235,
                owner_id: solo_corp,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.corp_projects.insert(
            777_001,
            eve_db::CorpProjectRecord {
                project_id: 777_001,
                corporation_id: solo_corp,
                name: "Escrow Fallback".to_string(),
                target_quantity: 100,
                current_quantity: 20,
                isk_per_unit: eve_db::IskCents(0),
                escrow_remaining: eve_db::IskCents(50_000),
            },
        );
        snap.corp_projects.insert(
            777_002,
            eve_db::CorpProjectRecord {
                project_id: 777_002,
                corporation_id: solo_corp,
                name: "Zero Isk".to_string(),
                target_quantity: 0,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(0),
                escrow_remaining: eve_db::IskCents(0),
            },
        );
        let proj_detail = snap
            .alliance_war_state
            .project_details
            .entry(777_001)
            .or_default();
        proj_detail.initial_escrow = eve_db::IskCents(100_000);
        proj_detail.char_contributions.insert(char_id, 20);
    });

    let p_solo = params(&[("corporation_id", &solo_corp.to_string())]);
    // Starbase without settings returns 404
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &params(&[
                ("corporation_id", &solo_corp.to_string()),
                ("starbase_id", &tower_no_settings.to_string()),
            ]),
            &params(&[("system_id", "30000142")]),
        )
        .unwrap()
        .status_code,
        404
    );

    // member_limit from corp_member_limits (77)
    let lim_gov = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&lim_gov), &json!(77));

    // remove stored member_limit -> falls back to CEO trained bonus_skill_tid
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_member_limits.remove(&solo_corp);
        snap.skills.entry(solo_ceo).or_default().insert(
            bonus_skill_tid,
            eve_db::TrainedSkillRecord {
                type_id: bonus_skill_tid,
                level: 2,
                skill_points: 1000.0,
            },
        );
    });
    let lim_skill = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    assert!(body(&lim_skill).as_i64().unwrap() > 0);

    // remove skills -> falls back to 10
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&solo_ceo);
    });
    let lim_base = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&lim_base), &json!(10));

    // members/titles pushes ceo when not in members
    let mem_titles = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/titles",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    assert!(body(&mem_titles)
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["character_id"] == solo_ceo));

    // projects cover char_contributions, initial_escrow division, and zero fallback
    let projs = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/projects",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    let arr = body(&projs).as_array().unwrap();
    assert_eq!(arr[0]["isk_per_unit"], 10.0);
    assert_eq!(arr[0]["contributors"][0]["character_id"], char_id);
    assert_eq!(arr[1]["isk_per_unit"], 0.0);
}
