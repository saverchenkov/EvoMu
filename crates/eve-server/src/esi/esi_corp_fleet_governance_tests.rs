//! Comprehensive unit and mutation-killing tests for ESI corporation, alliance, governance, and fleet routes.

use super::corp_governance_routes::decode_corp_roles;
use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::{EsiHttpRequest, EsiRouter};
use eve_db::{
    AllianceLifecycleMeta, CorpAllianceHistoryEntry, CorpContainerLogEntry, CorpIssuedMedalEntry,
    CorpMedalEntry, CorpProjectContributorEntry, CorpProjectRecord, CorpRoleHistoryEntry,
    CorpShareholderEntry, CorpTitleEntry, CustomsOfficeRecord, FleetMemberRecord, FleetRecord,
    FleetRole, InventoryFlag, InventoryItemRecord, IskCents, StarbaseDetailSettings,
    StructureEsiTimerMeta, TrainedSkillRecord,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn call_json(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path).with_bearer_token(token);
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

#[test]
fn corp_governance_and_alliance_routes_populated_and_error_branches() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;
    let alliance_id = 99_000_001_i64;
    let starbase_id = 1_080_000_009_i64;
    let fuel_item_id = 1_080_000_010_i64;
    let poco_id = 1_080_000_011_i64;
    let now = ctx.now_unix();

    // Verify decode_corp_roles bitmask helper
    let all_roles = decode_corp_roles(u64::MAX);
    assert!(all_roles.contains(&"Director"));
    assert!(all_roles.contains(&"Skill_Plan_Manager"));
    assert!(!all_roles.contains(&""));
    assert!(decode_corp_roles(0).is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliance_lifecycle_meta.insert(
            alliance_id,
            AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: now.saturating_sub(10_000),
                faction_id: Some(500_001),
            },
        );
        snap.corp_alliance_history.insert(
            corp_id,
            vec![
                CorpAllianceHistoryEntry {
                    record_id: 1,
                    alliance_id: None,
                    start_date_unix: now.saturating_sub(20_000),
                    is_deleted: None,
                },
                CorpAllianceHistoryEntry {
                    record_id: 2,
                    alliance_id: Some(alliance_id),
                    start_date_unix: now.saturating_sub(10_000),
                    is_deleted: Some(false),
                },
            ],
        );
        snap.corp_container_logs.insert(
            corp_id,
            vec![CorpContainerLogEntry {
                logged_at_unix: now.saturating_sub(120),
                container_id: 100_099_100,
                container_type_id: 17_366,
                character_id: char_id,
                location_id: 60_003_760,
                location_flag: "CorpSAG1".into(),
                action: "lock".into(),
                password_type: Some("config".into()),
                type_id: Some(34),
                quantity: Some(500),
                old_config_bitmask: Some(1),
                new_config_bitmask: Some(3),
            }],
        );
        snap.corp_hangar_divisions.insert(
            corp_id,
            HashMap::from([(1, "Main Hangar".into()), (2, "Ops Hangar".into())]),
        );
        snap.corp_medals.insert(
            corp_id,
            vec![CorpMedalEntry {
                medal_id: 401,
                title: "Founders Cross".into(),
                description: "Founding member".into(),
                creator_id: char_id,
                created_at_unix: now.saturating_sub(5000),
            }],
        );
        snap.corp_issued_medals.insert(
            corp_id,
            vec![CorpIssuedMedalEntry {
                medal_id: 401,
                character_id: char2_id,
                reason: "Loyalty".into(),
                status: "public".into(),
                issuer_id: char_id,
                issued_at_unix: now.saturating_sub(4000),
            }],
        );
        snap.corp_member_titles.insert((corp_id, char_id), vec![1, 2]);
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Fleet Boss".into(),
                roles: vec!["Director".into()],
                grantable_roles: vec![],
                roles_at_hq: vec![],
                grantable_roles_at_hq: vec![],
                roles_at_base: vec![],
                grantable_roles_at_base: vec![],
                roles_at_other: vec![],
                grantable_roles_at_other: vec![],
            }],
        );
        snap.corp_role_history.insert(
            corp_id,
            vec![CorpRoleHistoryEntry {
                character_id: char2_id,
                changed_at_unix: now.saturating_sub(300),
                issuer_id: char_id,
                role_type: "roles".into(),
                old_roles: vec![],
                new_roles: vec!["Accountant".into()],
            }],
        );
        snap.corp_shareholders.insert(
            corp_id,
            vec![
                CorpShareholderEntry {
                    shareholder_id: char_id,
                    shareholder_type: "character".into(),
                    share_count: 800,
                },
                CorpShareholderEntry {
                    shareholder_id: corp_id,
                    shareholder_type: "corporation".into(),
                    share_count: 200,
                },
            ],
        );
        // Starbase tower + fuel + settings with thresholds
        snap.items.insert(
            starbase_id,
            InventoryItemRecord {
                item_id: starbase_id,
                type_id: 12_235,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            fuel_item_id,
            InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: 4051,
                owner_id: corp_id,
                location_id: starbase_id,
                flag: InventoryFlag::None,
                quantity: 120,
                singleton: false,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                attack_standing_threshold: Some(5.0),
                attack_security_status_threshold: Some(-2.0),
                ..StarbaseDetailSettings::default()
            },
        );
        // Corp project + contributors
        snap.corp_projects.insert(
            801,
            CorpProjectRecord {
                project_id: 801,
                corporation_id: corp_id,
                name: "Mine Scordite".into(),
                target_quantity: 1_000,
                current_quantity: 250,
                isk_per_unit: IskCents::from_isk(100),
                escrow_remaining: IskCents::from_isk(75_000),
            },
        );
        snap.corp_project_contributors.insert(
            801,
            vec![CorpProjectContributorEntry {
                character_id: char_id,
                contributed_quantity: 250,
            }],
        );
        // Structure timer meta & customs office
        snap.structure_esi_timer_meta.insert(
            1_055_000_001,
            StructureEsiTimerMeta {
                profile_id: 12,
                fuel_expires_unix: Some(now + 86_400),
                state_timer_start_unix: Some(now),
                state_timer_end_unix: Some(now + 3600),
                unanchors_at_unix: Some(now + 172_800),
                next_reinforce_hour: Some(18),
                next_reinforce_apply_unix: Some(now + 86_400),
            },
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![CustomsOfficeRecord {
                office_id: poco_id,
                system_id: 30_000_142,
                reinforce_exit_start: 18,
                reinforce_exit_end: 20,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: Some("neutral".into()),
                corporation_tax_rate: Some(0.01),
                alliance_tax_rate: Some(0.02),
                excellent_standing_tax_rate: Some(0.03),
                good_standing_tax_rate: Some(0.04),
                neutral_standing_tax_rate: Some(0.05),
                bad_standing_tax_rate: Some(0.10),
                terrible_standing_tax_rate: Some(0.15),
            }],
        );
        snap.items.insert(
            poco_id,
            InventoryItemRecord {
                item_id: poco_id,
                type_id: 2233,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Test alliance detail with faction_id and 404
    let (st_al, al_val) = call_json(
        &router,
        "GET",
        &format!("/latest/alliances/{alliance_id}"),
        &token,
        None,
    );
    assert_eq!(st_al, 200);
    assert_eq!(al_val["faction_id"], json!(500_001));
    assert_eq!(
        call_json(&router, "GET", "/latest/alliances/99999999", &token, None).0,
        404
    );

    // Test populated governance endpoints
    let (_, hist) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/alliancehistory"),
        &token,
        None,
    );
    assert_eq!(hist.as_array().unwrap().len(), 2);

    let (_, logs) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/containers/logs"),
        &token,
        None,
    );
    assert_eq!(logs[0]["password_type"], json!("config"));
    assert_eq!(logs[0]["old_config_bitmask"], json!(1));

    let (_, divs) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/divisions"),
        &token,
        None,
    );
    assert_eq!(divs["hangar"].as_array().unwrap().len(), 2);

    let (_, medals) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/medals"),
        &token,
        None,
    );
    assert_eq!(medals[0]["medal_id"], json!(401));

    let (_, issued) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/medals/issued"),
        &token,
        None,
    );
    assert_eq!(issued[0]["character_id"], json!(char2_id));

    // Member limit: skill-based vs stored override
    ctx.db.with_mut_snapshot(|snap| {
        // Skill 3363 (Corporation Management) has attr 191 = +20 members/level
        snap.skills.entry(char_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    let (_, lim_skill) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/members/limit"),
        &token,
        None,
    );
    assert!(lim_skill.as_i64().unwrap() >= 10);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_member_limits.insert(corp_id, 550);
    });
    let (_, lim_stored) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/members/limit"),
        &token,
        None,
    );
    assert_eq!(lim_stored, json!(550));

    let (_, m_titles) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/members/titles"),
        &token,
        None,
    );
    assert!(!m_titles.as_array().unwrap().is_empty());

    let (_, titles) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/titles"),
        &token,
        None,
    );
    assert_eq!(titles[0]["name"], json!("Fleet Boss"));

    let (_, r_hist) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/roles/history"),
        &token,
        None,
    );
    assert_eq!(r_hist[0]["character_id"], json!(char2_id));

    let (_, holders) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/shareholders"),
        &token,
        None,
    );
    assert_eq!(holders.as_array().unwrap().len(), 2);

    // Starbase detail error branches (missing system_id, bad system_id, wrong system_id) and success
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/corporations/{corp_id}/starbases/{starbase_id}"),
            &token,
            None,
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/corporations/{corp_id}/starbases/{starbase_id}?system_id=abc"),
            &token,
            None,
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/corporations/{corp_id}/starbases/{starbase_id}?system_id=30000144"),
            &token,
            None,
        )
        .0,
        404
    );
    let (st_sb, sb_val) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/starbases/{starbase_id}?system_id=30000142"),
        &token,
        None,
    );
    assert_eq!(st_sb, 200);
    assert_eq!(sb_val["fuels"][0]["quantity"], json!(120));
    assert_eq!(sb_val["attack_standing_threshold"], json!(5.0));

    // Projects, structures, customs offices, NPC corp info, and wallet division bounds
    let (_, projs) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/projects"),
        &token,
        None,
    );
    assert_eq!(projs[0]["project_id"], json!(801));
    assert_eq!(projs[0]["contributors"][0]["contributed_quantity"], json!(250));

    let (_, structs) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/structures"),
        &token,
        None,
    );
    assert_eq!(structs[0]["profile_id"], json!(12));

    let (_, pocos) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/customs_offices"),
        &token,
        None,
    );
    assert_eq!(pocos[0]["office_id"], json!(poco_id));

    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/corporations/{corp_id}/wallets/0/journal"),
            &token,
            None,
        )
        .0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/corporations/{corp_id}/wallets/8/transactions"),
            &token,
            None,
        )
        .0,
        400
    );
}

#[test]
fn fleet_routes_validation_limits_and_role_movement_branches() {
    let (ctx, token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;
    let fid = 7_700_099_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fid,
            FleetRecord {
                fleet_id: fid,
                boss_id: char_id,
                members: vec![char_id],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: false,
                is_registered: true,
                wing_details: Default::default(),
                squad_details: Default::default(),
                member_details: HashMap::from([(
                    char_id,
                    FleetMemberRecord {
                        character_id: char_id,
                        role: FleetRole::FleetCommander,
                        wing_id: None,
                        squad_id: None,
                        takes_fleet_warp: true,
                        accepts_regroup: true,
                        accepts_conduit_jumps: true,
                        job: 1,
                        role_name: "Fleet Commander (Boss)".into(),
                        join_time_unix: now,
                    },
                )]),
                invites: Default::default(),
                motd: "Initial".into(),
            },
        );
    });

    // Unknown fleet returns 404
    assert_eq!(call_json(&router, "GET", "/latest/fleets/99999999", &token, None).0, 404);

    // Create wings
    let mut wing_ids = Vec::new();
    for _ in 0..3 {
        let (st, v) = call_json(&router, "POST", &format!("/latest/fleets/{fid}/wings"), &token, None);
        assert_eq!(st, 201);
        wing_ids.push(v["wing_id"].as_i64().unwrap());
    }
    let wid = wing_ids[0];
    // Wing rename validation: empty name -> 400, > 10 chars -> 400, unknown wing -> 404, valid -> 204
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/wings/{wid}"), &token, Some(json!({"name": ""}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/wings/{wid}"), &token, Some(json!({"name": "WingNameWayTooLong"}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/wings/999999"), &token, Some(json!({"name": "Valid"}))).0, 404);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/wings/{wid}"), &token, Some(json!({"name": "Alpha"}))).0, 204);

    // Squad creation, rename validation, and unknown wing 404
    assert_eq!(call_json(&router, "POST", &format!("/latest/fleets/{fid}/wings/999999/squads"), &token, None).0, 404);
    let (st_sq, sq_val) = call_json(&router, "POST", &format!("/latest/fleets/{fid}/wings/{wid}/squads"), &token, None);
    assert_eq!(st_sq, 201);
    let sid = sq_val["squad_id"].as_i64().unwrap();
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/squads/{sid}"), &token, Some(json!({"name": ""}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/squads/{sid}"), &token, Some(json!({"name": "SquadNameTooLong"}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/squads/999999"), &token, Some(json!({"name": "Bravo"}))).0, 404);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/squads/{sid}"), &token, Some(json!({"name": "Bravo"}))).0, 204);

    // Invite validation: bad role -> 400, unknown character -> 404, valid wing_commander -> 204
    assert_eq!(call_json(&router, "POST", &format!("/latest/fleets/{fid}/members"), &token, Some(json!({"character_id": char2_id, "role": "bad_role"}))).0, 400);
    assert_eq!(call_json(&router, "POST", &format!("/latest/fleets/{fid}/members"), &token, Some(json!({"character_id": 999_888_777, "role": "squad_member"}))).0, 404);
    assert_eq!(call_json(&router, "POST", &format!("/latest/fleets/{fid}/members"), &token, Some(json!({"character_id": char2_id, "role": "wing_commander", "wing_id": wid}))).0, 204);

    // Move member validation: missing wing_id for wing_commander -> 400, missing squad_id for squad_member -> 400, valid moves -> 204
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, Some(json!({"role": "wing_commander"}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, Some(json!({"role": "squad_member", "wing_id": wid}))).0, 400);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, Some(json!({"role": "wing_commander", "wing_id": wid}))).0, 204);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, Some(json!({"role": "fleet_commander"}))).0, 204);
    assert_eq!(call_json(&router, "PUT", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, Some(json!({"role": "squad_member", "wing_id": wid, "squad_id": sid}))).0, 204);

    // Kick boss fails with 403, delete squad & wing succeeds with 204
    assert_eq!(call_json(&router, "DELETE", &format!("/latest/fleets/{fid}/members/{char_id}"), &token, None).0, 403);
    assert_eq!(call_json(&router, "DELETE", &format!("/latest/fleets/{fid}/squads/{sid}"), &token, None).0, 204);
    assert_eq!(call_json(&router, "DELETE", &format!("/latest/fleets/{fid}/wings/{wid}"), &token, None).0, 204);
}
