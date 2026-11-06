//! Deep unit tests for ESI fleet hierarchy & squad deletion, Remote Client UI openwindow
//! & autopilot waypoints, universe structure ACLs, and bulk `/universe/names` & `/universe/ids`.

use super::universe_routes::character_has_structure_acl_access;
use super::{EsiHttpRequest, EsiRouter};
use crate::router::ServerContext;
use eve_db::{
    corp_roles, ContractRecord, FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord,
    FleetWingRecord, UpwellStructureRecord, WarRecord,
};
use eve_sde::SdeRegistry;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn build_ctx() -> Arc<ServerContext> {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get_mut(&90_000_001).cloned() {
            ch.user_id = 1;
            ch.alliance_id = Some(99_000_001);
            let corp_id = ch.corporation_id;
            snap.characters.insert(90_000_001, ch.clone());
            for cid in [90_000_002, 90_000_003, 90_000_004] {
                let mut alt = snap.characters.get(&cid).cloned().unwrap_or_else(|| ch.clone());
                alt.user_id = 1;
                alt.character_id = cid;
                alt.name = format!("Pilot {cid}");
                snap.characters.insert(cid, alt);
            }
            let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
                eve_db::CorporationRecord {
                    corporation_id: corp_id,
                    name: "Demo Corp".into(),
                    ticker: "DEMO".into(),
                    ceo_id: 90_000_001,
                    ..Default::default()
                }
            });
            corp.ceo_id = 90_000_001;
            corp.alliance_id = Some(99_000_001);
            corp.members.insert(90_000_001);
            corp.member_roles
                .insert(90_000_001, corp_roles::ROLE_DIRECTOR);
        }
    });
    Arc::new(ctx)
}

fn mint_full_token(ctx: &ServerContext, char_id: i64) -> String {
    let scopes: BTreeSet<String> = super::OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token
}

fn register_online_char(ctx: &ServerContext, char_id: i64) {
    let user_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    ctx.db
        .acquire_online_lock_with_computer_hash(
            user_id,
            user_id * 100 + 1,
            Some(char_id),
            None,
            true,
            ctx.now_unix(),
        )
        .expect("acquire_online_lock_with_computer_hash");
}

fn fleet_member(character_id: i64, wing_id: Option<i64>, squad_id: Option<i64>, role: FleetRole) -> FleetMemberRecord {
    FleetMemberRecord {
        character_id,
        role,
        wing_id,
        squad_id,
        takes_fleet_warp: true,
        accepts_regroup: true,
        accepts_conduit_jumps: true,
        job: 0,
        role_name: String::new(),
        join_time_unix: 1_700_000_000,
    }
}

#[test]
fn esi_fleet_hierarchy_permissions_squad_cleanup_and_ui_openwindow_routes() {
    let ctx = build_ctx();
    let boss_id: i64 = 90_000_001;
    let wing_cmdr_id: i64 = 90_000_002;
    let plain_member_id: i64 = 90_000_003;
    let outsider_id: i64 = 90_000_004;
    let fleet_id: i64 = 777_000_111;
    let wing_id: i64 = 777_100_001;
    let squad_id: i64 = 777_200_001;

    ctx.db.with_mut_snapshot(|snap| {
        let mut wing_details = HashMap::new();
        wing_details.insert(
            wing_id,
            FleetWingRecord {
                wing_id,
                name: "Alpha Wing".into(),
                commander_id: Some(wing_cmdr_id),
                squad_ids: vec![squad_id],
            },
        );
        let mut squad_details = HashMap::new();
        squad_details.insert(
            squad_id,
            FleetSquadRecord {
                squad_id,
                wing_id,
                name: "Squad One".into(),
                commander_id: None,
            },
        );
        let mut member_details = HashMap::new();
        member_details.insert(boss_id, fleet_member(boss_id, None, None, FleetRole::FleetCommander));
        member_details.insert(
            wing_cmdr_id,
            fleet_member(wing_cmdr_id, Some(wing_id), None, FleetRole::WingCommander),
        );
        member_details.insert(
            plain_member_id,
            fleet_member(plain_member_id, Some(wing_id), Some(squad_id), FleetRole::SquadMember),
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                motd: "Fly safe".into(),
                free_move: false,
                is_registered: false,
                wings: vec![wing_id],
                squads: vec![squad_id],
                members: vec![boss_id, wing_cmdr_id, plain_member_id],
                wing_details,
                squad_details,
                member_details,
                ..FleetRecord::default()
            },
        );
        snap.contracts.insert(
            88_001_234,
            ContractRecord {
                contract_id: 88_001_234,
                title: "Courier Test".into(),
                ..ContractRecord::default()
            },
        );
    });

    assert!(super::ui_routes::dispatch_ui_route(
        &ctx,
        "POST",
        "/ui/unknown",
        &HashMap::new(),
        &HashMap::new(),
        None,
        &[]
    )
    .is_none());

    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 200, 60);
    let tok_boss = mint_full_token(&ctx, boss_id);
    let tok_plain = mint_full_token(&ctx, plain_member_id);
    let tok_outsider = mint_full_token(&ctx, outsider_id);

    // 1. Character fleet status & non-fleet 404
    let ch_fleet = router.handle_request(
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/fleet/").with_bearer_token(&tok_boss),
    );
    assert_eq!(ch_fleet.status_code, 200);
    assert_eq!(ch_fleet.json().unwrap()["fleet_id"], fleet_id);

    let out_fleet = router.handle_request(
        &EsiHttpRequest::new("GET", format!("/v1/characters/{outsider_id}/fleet/"))
            .with_bearer_token(&tok_outsider),
    );
    assert_eq!(out_fleet.status_code, 404);

    // 2. Fleet settings GET / PUT + permission checks + squad delete cleanup
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
                    .with_bearer_token(&tok_outsider)
            )
            .status_code,
        403
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("GET", format!("/v1/fleets/{fleet_id}/"))
                    .with_bearer_token(&tok_boss)
            )
            .status_code,
        200
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("PUT", format!("/v1/fleets/{fleet_id}/"))
                    .with_bearer_token(&tok_boss)
                    .with_body(br#"{"is_free_move":true,"motd":"Updated MOTD"}"#.to_vec())
            )
            .status_code,
        204
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/squads/{squad_id}/"))
                    .with_bearer_token(&tok_plain)
            )
            .status_code,
        403
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/squads/{squad_id}/"))
                    .with_bearer_token(&tok_boss)
            )
            .status_code,
        204
    );
    ctx.db.with_snapshot(|s| {
        let f = &s.fleets[&fleet_id];
        assert!(!f.squads.contains(&squad_id));
        assert!(!f.wing_details[&wing_id].squad_ids.contains(&squad_id));
    });
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("DELETE", format!("/v1/fleets/{fleet_id}/squads/{squad_id}/"))
                    .with_bearer_token(&tok_boss)
            )
            .status_code,
        404
    );

    // 3. Remote Client UI OpenWindow endpoints: offline error -> online activation across all 4 routes
    let offline_contract = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/contract/?contract_id=88001234")
            .with_bearer_token(&tok_boss),
    );
    assert!(offline_contract.status_code >= 400);

    register_online_char(&ctx, boss_id);

    for (path, expected) in [
        ("/v1/ui/openwindow/contract/", 400),
        ("/v1/ui/openwindow/contract/?contract_id=999999999", 404),
        ("/v1/ui/openwindow/contract/?contract_id=88001234", 204),
        ("/v1/ui/openwindow/information/?target_id=90000001", 204),
        ("/v1/ui/openwindow/information/?target_id=34", 501),
        ("/v1/ui/openwindow/information/?target_id=888777666555", 404),
        ("/v1/ui/openwindow/marketdetails/?type_id=34", 204),
        ("/v1/ui/openwindow/marketdetails/?type_id=999888777", 404),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("POST", path).with_bearer_token(&tok_boss));
        assert_eq!(resp.status_code, expected, "POST {path} -> {expected}");
    }

    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
                    .with_bearer_token(&tok_boss)
                    .with_body(b"not-json".to_vec())
            )
            .status_code,
        400
    );
    let conflict_mail = serde_json::to_vec(&json!({
        "subject": "Fleet ping",
        "body": "Form up in Jita",
        "recipients": [90_000_002],
        "to_corp_or_alliance_id": 99_000_001,
        "to_mailing_list_id": 1234
    }))
    .unwrap();
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
                    .with_bearer_token(&tok_boss)
                    .with_body(conflict_mail)
            )
            .status_code,
        422
    );
    let valid_mail = serde_json::to_vec(&json!({
        "subject": "Fleet ping",
        "body": "Form up on Jita 4-4 undock",
        "recipients": [90_000_002]
    }))
    .unwrap();
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
                    .with_bearer_token(&tok_boss)
                    .with_body(valid_mail)
            )
            .status_code,
        204
    );
}

#[test]
fn esi_universe_acl_bulk_names_ids_and_autopilot_waypoint_branches() {
    let ctx = build_ctx();
    let char_id: i64 = 90_000_001;
    let ally_id: i64 = 90_000_002;
    let host_id: i64 = 90_000_003;
    let structure_id: i64 = 1_000_000_123_456;

    ctx.db.with_mut_snapshot(|snap| {
        let owner_corp = snap.characters[&char_id].corporation_id;
        if let Some(a) = snap.characters.get_mut(&ally_id) {
            a.corporation_id = 98_111_111;
            a.alliance_id = Some(99_000_001);
        }
        if let Some(h) = snap.characters.get_mut(&host_id) {
            h.corporation_id = 98_222_222;
            h.alliance_id = None;
            h.standings.insert(owner_corp, -5.0);
        }
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                solar_system_id: 30_000_142,
                owner_corp_id: owner_corp,
                type_id: 35832,
                name: "Test Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..UpwellStructureRecord::default()
            },
        );
    });

    // 1. Structure ACL access across destroyed, deny list, unknown char, corp, alliance, negative standing, war
    ctx.db.with_snapshot(|snap| {
        let st = &snap.structures[&structure_id];
        assert!(character_has_structure_acl_access(snap, char_id, st));
        assert!(character_has_structure_acl_access(snap, ally_id, st));
        assert!(!character_has_structure_acl_access(snap, host_id, st));
        assert!(!character_has_structure_acl_access(snap, 999_888_777, st));

        let mut destroyed_st = st.clone();
        destroyed_st.state = "Destroyed".into();
        assert!(!character_has_structure_acl_access(snap, char_id, &destroyed_st));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied
            .entry(structure_id)
            .or_default()
            .insert(ally_id);
        snap.wars.insert(
            555_001,
            WarRecord {
                war_id: 555_001,
                aggressor_corp_id: 98_222_222,
                defender_corp_id: snap.structures[&structure_id].owner_corp_id,
                mutual: false,
                started_unix: 100,
                active: true,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let st = &snap.structures[&structure_id];
        assert!(!character_has_structure_acl_access(snap, ally_id, st));
        assert!(!character_has_structure_acl_access(snap, host_id, st));
    });

    let router = EsiRouter::with_error_limit(Arc::clone(&ctx), 200, 60);
    let token = mint_full_token(&ctx, char_id);

    // 2. POST /universe/names & POST /universe/ids error + multi-category resolution
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/universe/names/").with_body(b"[]".to_vec())
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/universe/names/")
                    .with_body(b"[999888777666]".to_vec())
            )
            .status_code,
        404
    );
    let names_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/universe/names/")
            .with_body(serde_json::to_vec(&json!([char_id, 30_000_142, 34])).unwrap()),
    );
    assert_eq!(names_resp.status_code, 200);
    let names_arr = names_resp.json().unwrap();
    let cats: BTreeSet<&str> = names_arr
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["category"].as_str())
        .collect();
    assert!(cats.contains("character"));
    assert!(cats.contains("solar_system"));
    assert!(cats.contains("inventory_type"));

    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/universe/ids/").with_body(b"[]".to_vec())
            )
            .status_code,
        400
    );
    let ids_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/universe/ids/")
            .with_body(br#"["Tritanium","NonExistentName999"]"#.to_vec()),
    );
    assert_eq!(ids_resp.status_code, 200);
    let ids_json = ids_resp.json().unwrap();
    assert_eq!(ids_json["inventory_types"][0]["id"], 34);

    // 3. POST /ui/autopilot/waypoint validation, offline 501, and online 204
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/v1/ui/autopilot/waypoint/?destination_id=999888777")
                    .with_bearer_token(&token)
            )
            .status_code,
        400
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "POST",
                    "/v1/ui/autopilot/waypoint/?destination_id=30000142&add_to_beginning=false&clear_other_waypoints=false"
                )
                .with_bearer_token(&token)
            )
            .status_code,
        501
    );

    register_online_char(&ctx, char_id);
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new(
                    "POST",
                    "/v1/ui/autopilot/waypoint/?destination_id=30000142&add_to_beginning=true&clear_other_waypoints=true"
                )
                .with_bearer_token(&token)
            )
            .status_code,
        204
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone()),
        vec![30_000_142]
    );
}
