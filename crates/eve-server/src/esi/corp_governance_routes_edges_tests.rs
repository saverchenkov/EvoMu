//! Edge-case and full-branch unit tests for `esi::corp_governance_routes`.

#![forbid(unsafe_code)]

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::snapshot_ext::{
    CorpAllianceHistoryEntry, CorpContainerLogEntry, CorpIssuedMedalEntry, CorpMedalEntry,
    CorpProjectContributorEntry, CorpRoleHistoryEntry, CorpTitleEntry, StarbaseDetailSettings,
};
use eve_db::{
    AllianceRecord, CorpGovState, CorpProjectRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PersistedProjectDetails, TrainedSkillRecord,
};

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
fn encode_esi_corp_roles_and_route_400_404_checks() {
    let encoded = encode_esi_corp_roles(&[
        "Director".into(),
        "Deliveries Hangar Take".into(),
        "terrestrial logistics officer".into(),
        "NonExistentRole".into(),
    ]);
    assert_ne!(encoded & eve_db::corp_roles::ROLE_DIRECTOR, 0);
    assert_ne!(encoded & eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE, 0);
    assert_ne!(
        encoded & eve_db::corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER,
        0
    );
    assert_eq!(encode_esi_corp_roles(&[]), 0);

    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();

    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/alliances/{alliance_id}",
            &params(&[("alliance_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );

    let p_bad_c = params(&[("corporation_id", "bad")]);
    let p_miss_c = params(&[("corporation_id", "99999999")]);
    for path in [
        "/corporations/{corporation_id}/alliancehistory",
        "/corporations/{corporation_id}/containers/logs",
        "/corporations/{corporation_id}/divisions",
        "/corporations/{corporation_id}/medals",
        "/corporations/{corporation_id}/medals/issued",
        "/corporations/{corporation_id}/members/limit",
        "/corporations/{corporation_id}/members/titles",
        "/corporations/{corporation_id}/titles",
        "/corporations/{corporation_id}/roles/history",
        "/corporations/{corporation_id}/shareholders",
        "/corporations/{corporation_id}/projects",
    ] {
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, &p_bad_c, &empty_q)
                .unwrap()
                .status_code,
            400,
            "expected 400 on {path}"
        );
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, &p_miss_c, &empty_q)
                .unwrap()
                .status_code,
            404,
            "expected 404 on {path}"
        );
    }
}

#[test]
fn governance_routes_exercise_optional_fields_member_limits_starbases_projects_and_pagination() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let empty_q = HashMap::new();
    let now = ctx.now_unix();

    let corp_mgmt_skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.contains_key(&191))
        .map(|t| t.type_id)
        .unwrap_or(3363);

    ctx.db.with_mut_snapshot(|snap| {
        // Alliance without executor or faction
        snap.alliances.insert(
            99_000_055,
            AllianceRecord {
                alliance_id: 99_000_055,
                name: "NoExec".into(),
                short_name: "NOEX".into(),
                executor_corp_id: 0,
                member_corp_ids: Default::default(),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            99_000_055,
            eve_db::AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: now,
                faction_id: None,
            },
        );

        // Alliance history on NPC corp (tests NPC corp acceptance + None vs Some fields)
        snap.corp_alliance_history.insert(
            1_000_035,
            vec![
                CorpAllianceHistoryEntry {
                    record_id: 1,
                    alliance_id: None,
                    start_date_unix: 1_600_000_000,
                    is_deleted: None,
                },
                CorpAllianceHistoryEntry {
                    record_id: 2,
                    alliance_id: Some(99_000_001),
                    start_date_unix: 1_700_000_000,
                    is_deleted: Some(false),
                },
            ],
        );

        // Container log with all optional fields None
        snap.corp_container_logs.insert(
            corp_id,
            vec![CorpContainerLogEntry {
                logged_at_unix: now,
                container_id: 100_501,
                container_type_id: 3296,
                character_id: char_id,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".into(),
                action: "unlock".into(),
                password_type: None,
                type_id: None,
                quantity: None,
                old_config_bitmask: None,
                new_config_bitmask: None,
            }],
        );

        // Medals & issued medals
        snap.corp_medals.insert(
            corp_id,
            vec![CorpMedalEntry {
                medal_id: 1,
                title: "Star".into(),
                description: "Honor".into(),
                creator_id: char_id,
                created_at_unix: now,
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![CorpIssuedMedalEntry {
                medal_id: 1,
                character_id: char_id,
                reason: "Valor".into(),
                status: "public".into(),
                issuer_id: char_id,
                issued_at_unix: now,
            }],
        );

        // Titles & member titles & role history
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.remove(&corp.ceo_id); // ensures !members.contains(&corp.ceo_id) branch runs
        }
        snap.corp_member_titles.insert((corp_id, char_id), vec![2, 1]);
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Officer".into(),
                roles: vec!["Accountant".into()],
                ..Default::default()
            }],
        );
        snap.corp_role_history.insert(
            corp_id,
            vec![CorpRoleHistoryEntry {
                character_id: char_id,
                changed_at_unix: now,
                issuer_id: char_id,
                role_type: "roles".into(),
                old_roles: vec![],
                new_roles: vec!["Accountant".into()],
            }],
        );

        // Starbase with standing/sec thresholds + non-tower item
        snap.items.insert(
            890_001,
            InventoryItemRecord {
                item_id: 890_001,
                type_id: 12235,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            890_002,
            InventoryItemRecord {
                item_id: 890_002,
                type_id: 34, // not a control tower
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(
            890_001,
            StarbaseDetailSettings {
                allow_corporation_members: true,
                allow_alliance_members: false,
                use_alliance_standings: false,
                attack_if_other_security_status_dropping: true,
                attack_if_at_war: true,
                fuel_bay_view: "Corporation Member".into(),
                fuel_bay_take: "Config Starbase Equipment".into(),
                anchor: "Config Starbase Equipment".into(),
                unanchor: "Config Starbase Equipment".into(),
                online: "Config Starbase Equipment".into(),
                offline: "Config Starbase Equipment".into(),
                attack_standing_threshold: Some(-5.0),
                attack_security_status_threshold: Some(-2.0),
                ..Default::default()
            },
        );

        // Corp projects: one with isk_per_unit > 0, one with initial_escrow fallback, one with 0
        snap.corp_projects.insert(
            1,
            CorpProjectRecord {
                project_id: 1,
                corporation_id: corp_id,
                name: "Direct".into(),
                target_quantity: 100,
                current_quantity: 10,
                isk_per_unit: IskCents::from_isk(50),
                escrow_remaining: IskCents::from_isk(4500),
            },
        );
        snap.corp_projects.insert(
            2,
            CorpProjectRecord {
                project_id: 2,
                corporation_id: corp_id,
                name: "EscrowFallback".into(),
                target_quantity: 100,
                current_quantity: 20,
                isk_per_unit: IskCents::ZERO,
                escrow_remaining: IskCents::from_isk(8000),
            },
        );
        snap.corp_projects.insert(
            3,
            CorpProjectRecord {
                project_id: 3,
                corporation_id: corp_id,
                name: "ZeroRate".into(),
                target_quantity: 0,
                current_quantity: 0,
                isk_per_unit: IskCents::ZERO,
                escrow_remaining: IskCents::ZERO,
            },
        );
        snap.corp_project_contributors.insert(
            1,
            vec![CorpProjectContributorEntry {
                character_id: char_id,
                contributed_quantity: 10,
            }],
        );
        snap.alliance_war_state.project_details.insert(
            2,
            PersistedProjectDetails {
                char_contributions: HashMap::from([(char_id, 20)]),
                initial_escrow: IskCents::from_isk(10_000),
                ..Default::default()
            },
        );
    });

    let ally_no_exec = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}",
        &params(&[("alliance_id", "99000055")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(ally_no_exec.status_code, 200);
    assert!(body(&ally_no_exec).get("executor_corporation_id").is_none());
    assert!(body(&ally_no_exec).get("faction_id").is_none());

    let npc_hist = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/alliancehistory",
        &params(&[("corporation_id", "1000035")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&npc_hist).as_array().unwrap().len(), 2);

    // Pagination errors across paginated governance endpoints
    for path in [
        "/corporations/{corporation_id}/containers/logs",
        "/corporations/{corporation_id}/medals",
        "/corporations/{corporation_id}/medals/issued",
        "/corporations/{corporation_id}/roles/history",
        "/corporations/{corporation_id}/shareholders",
        "/corporations/{corporation_id}/projects",
    ] {
        assert_eq!(
            dispatch_corp_governance_route(&ctx, "GET", path, &p_corp, &empty_q)
                .unwrap()
                .status_code,
            200
        );
        assert_eq!(
            dispatch_corp_governance_route(
                &ctx,
                "GET",
                path,
                &p_corp,
                &params(&[("page", "999")])
            )
            .unwrap()
            .status_code,
            404
        );
    }

    // Member limit branches: 1) corp_governance.member_limit, 2) CEO skill bonus, 3) base 10 fallback
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_member_limits.remove(&corp_id);
        snap.corp_governance.insert(
            corp_id,
            CorpGovState {
                member_limit: Some(120),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        body(
            &dispatch_corp_governance_route(
                &ctx,
                "GET",
                "/corporations/{corporation_id}/members/limit",
                &p_corp,
                &empty_q
            )
            .unwrap()
        ),
        &json!(120)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.remove(&corp_id);
        snap.skills.entry(char_id).or_default().insert(
            corp_mgmt_skill_tid,
            TrainedSkillRecord {
                type_id: corp_mgmt_skill_tid,
                level: 4,
                skill_points: 100_000.0,
            },
        );
    });
    let skill_lim = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&skill_lim).as_i64().unwrap() >= 10);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&char_id);
    });
    assert_eq!(
        body(
            &dispatch_corp_governance_route(
                &ctx,
                "GET",
                "/corporations/{corporation_id}/members/limit",
                &p_corp,
                &empty_q
            )
            .unwrap()
        ),
        &json!(10)
    );

    // Titles & member titles
    let mt = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/titles",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&mt)
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["character_id"] == char_id && m["titles"] == json!([1, 2])));

    let titles = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/titles",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&titles)[0]["name"], "Officer");

    // Starbase detail errors: 400 corp, 400 sb, 400 bad system_id, 404 corp, 404 sb missing, 404 not tower, 404 missing settings, 200 with thresholds
    assert_eq!(
        dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases/{starbase_id}",
            &params(&[("corporation_id", "bad"), ("starbase_id", "890001")]),
            &params(&[("system_id", "30000142")])
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
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "bad")]),
            &params(&[("system_id", "30000142")])
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
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "890001")]),
            &params(&[("system_id", "abc")])
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
            &params(&[("corporation_id", "99999999"), ("starbase_id", "890001")]),
            &params(&[("system_id", "30000142")])
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
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "999999")]),
            &params(&[("system_id", "30000142")])
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
            &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "890002")]),
            &params(&[("system_id", "30000142")])
        )
        .unwrap()
        .status_code,
        404
    );

    let sb_ok = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/starbases/{starbase_id}",
        &params(&[("corporation_id", &corp_id.to_string()), ("starbase_id", "890001")]),
        &params(&[("system_id", "30000142")]),
    )
    .unwrap();
    assert_eq!(sb_ok.status_code, 200);
    assert_eq!(body(&sb_ok)["attack_standing_threshold"], -5.0);
    assert_eq!(body(&sb_ok)["attack_security_status_threshold"], -2.0);

    // Projects verify isk_per_unit direct, escrow fallback, and 0.0
    let projs = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/projects",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let p_arr = body(&projs).as_array().unwrap();
    assert_eq!(p_arr[0]["isk_per_unit"], 50.0);
    assert_eq!(p_arr[1]["isk_per_unit"], 100.0);
    assert_eq!(p_arr[2]["isk_per_unit"], 0.0);
}

#[test]
fn dispatch_corp_governance_members_limit_full_contract_and_unmatched_none() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();

    assert!(dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/not/corp/governance/route",
        &params(&[]),
        &empty_q,
    )
    .is_none());

    let bad = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &params(&[("corporation_id", "abc")]),
        &empty_q,
    )
    .expect("matched route 400");
    assert_eq!(bad.status_code, 400);

    let miss = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &params(&[("corporation_id", "99999999")]),
        &empty_q,
    )
    .expect("matched route 404");
    assert_eq!(miss.status_code, 404);
    assert_eq!(body(&miss)["error"], "Corporation 99999999 not found");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_member_limits.remove(&corp_id);
        snap.corp_governance.insert(
            corp_id,
            CorpGovState {
                member_limit: Some(137),
                ..Default::default()
            },
        );
    });
    let ok = dispatch_corp_governance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members/limit",
        &params(&[("corporation_id", &corp_id.to_string())]),
        &empty_q,
    )
    .expect("matched route 200");
    assert_eq!(ok.status_code, 200);
    assert_eq!(ok.max_age_secs, 3600);
    assert_eq!(body(&ok), &json!(137));
}
