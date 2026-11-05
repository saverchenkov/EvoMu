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
