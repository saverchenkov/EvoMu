//! Live ESI HTTP integration tests for Batch 6 parity closure:
//! - Faction Warfare (`/fw/stats`, `/fw/systems`, `/fw/leaderboards`, `/fw/leaderboards/characters`,
//!   `/fw/leaderboards/corporations`, `/fw/wars`, `/characters/{character_id}/fw/stats`,
//!   `/corporations/{corporation_id}/fw/stats`)
//! - Incursions & Sovereignty (`/incursions`, `/sovereignty/campaigns`, `/sovereignty/structures`)
//! - Loyalty Store Offers (`/loyalty/stores/{corporation_id}/offers`)

use std::collections::HashMap;
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression;
use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use eve_server::services::sov_services::{
    claim_system_sovereignty, record_hub_hack, TYPE_SOVEREIGNTY_HUB,
};
use serde_json::Value;

use crate::common;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(
    ctx: &ServerContext,
    uname: &str,
    character_id: i64,
    scopes: Vec<String>,
) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            uname,
            "secret",
            character_id,
            "evomu-esi-b6-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-b6-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn validate_and_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    expected_status: u16,
    req: &EsiHttpRequest,
) -> (Value, HashMap<String, String>) {
    let resp = router.handle_request(req);
    assert_eq!(resp.status_code, expected_status, "path={}", req.raw_target);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
    let json_val = if resp.status_code == 304 || resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).expect("Valid JSON response body")
    };
    (json_val, resp.headers)
}

async fn create_pilot_in_new_corp(
    ctx: &ServerContext,
    uname: &str,
    cname: &str,
    ticker: &str,
) -> (i64, i64, SessionState) {
    let uid = ctx.db.provision_player_account(uname, "secret");
    let char_id = ctx
        .db
        .create_character(uid, cname, 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;
    let mut sess = SessionState::new(char_id as u64);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(uname.into()), PyRep::String("secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        ctx.now_unix() + 15,
    )));
    let corp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String(format!("{cname} Corp").into()),
                PyRep::String(ticker.into()),
                PyRep::String("Test Corp".into()),
                PyRep::String("".into()),
                PyRep::Real(0.05),
                PyRep::Bool(false),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = match corp_res {
        PyRep::Integer(id) => id,
        _ => sess.corporation_id.unwrap_or(0),
    };
    (char_id, corp_id, sess)
}

#[tokio::test]
async fn test_esi_b6_fw_stats_systems_leaderboards_and_wars_live_rpc_driven() {
    // parity: esi:GET /fw/stats, esi:GET /fw/systems, esi:GET /fw/leaderboards, esi:GET /fw/leaderboards/characters, esi:GET /fw/leaderboards/corporations, esi:GET /fw/wars, esi:GET /characters/{character_id}/fw/stats, esi:GET /corporations/{corporation_id}/fw/stats
    eve_testkit::attest(&[
        "esi:GET /fw/stats",
        "esi:GET /fw/systems",
        "esi:GET /fw/leaderboards",
        "esi:GET /fw/leaderboards/characters",
        "esi:GET /fw/leaderboards/corporations",
        "esi:GET /fw/wars",
        "esi:GET /characters/{character_id}/fw/stats",
        "esi:GET /corporations/{corporation_id}/fw/stats",
    ]);

    let db_path = std::env::temp_dir().join(format!("esi_b6_fw_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
    common::pin_clock(&ctx);

    let (char_a, corp_a, mut sess_a) =
        create_pilot_in_new_corp(&ctx, "fw_pilot_a", "FW Pilot A", "FWPA").await;
    let (char_b, _corp_b, mut sess_b) =
        create_pilot_in_new_corp(&ctx, "fw_pilot_b", "FW Pilot B", "FWPB").await;
    let (char_u, corp_u, _sess_u) =
        create_pilot_in_new_corp(&ctx, "fw_unenlisted", "Unenlisted Pilot", "UNEN").await;

    let kw = HashMap::new();
    // Enlist Pilot A's corp in Caldari State (500001) and Pilot B in Gallente Federation (500004)
    ctx.dispatch_rpc(
        &mut sess_a,
        "facWarMgr",
        None,
        "JoinFactionAsCorporation",
        &[PyRep::Integer(500_001)],
        &kw,
        None,
    )
    .await
    .expect("JoinFactionAsCorporation");
    ctx.dispatch_rpc(
        &mut sess_b,
        "facWarMgr",
        None,
        "JoinFactionAsCharacter",
        &[PyRep::Integer(500_004)],
        &kw,
        None,
    )
    .await
    .expect("JoinFactionAsCharacter");

    // Pick a Gallente-occupied FW warzone system and donate LP as Caldari to advance victory points
    let fw_sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .values()
            .find(|o| o.occupier_faction_id == 500_004)
            .map(|o| o.solar_system_id)
            .expect("Gallente FW system")
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_b) {
            ch.loyalty_points.insert(1_000_181, 5_000);
            ch.solar_system_id = fw_sys_id;
        }
    });
    sess_b.solar_system_id = Some(fw_sys_id);
    ctx.dispatch_rpc(
        &mut sess_b,
        "facWarMgr",
        None,
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(fw_sys_id as i64), PyRep::Integer(1_000)],
        &kw,
        None,
    )
    .await
    .expect("DonateLPsToSolarSystem");
    eve_server::services::fw_services::add_victory_points(&ctx, fw_sys_id, 500);

    // Record a live PvP killmail between opposing FW pilots -> combat hook populates FW kills, VP, and LP
    process_ship_destruction_with_aggression(
        &ctx,
        char_b,
        880_000_001,
        587,
        char_a,
        2873,
        fw_sys_id,
        glam::DVec3::new(100.0, 200.0, 300.0),
        false,
    );

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. /fw/wars
    let (wars, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/wars/"));
    let wars_arr = wars.as_array().expect("wars array");
    assert!(
        wars_arr.iter().any(|w| w["faction_id"] == 500_001 && w["against_id"] == 500_004),
        "Caldari vs Gallente war pair must be present"
    );

    // 2. /fw/systems
    let (systems, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/systems/"));
    let sys_entry = systems
        .as_array()
        .expect("systems array")
        .iter()
        .find(|s| s["solar_system_id"] == fw_sys_id as i64)
        .expect("fw system entry");
    assert_eq!(sys_entry["occupier_faction_id"], 500_004);
    assert!(sys_entry["victory_points"].as_i64().unwrap_or(0) > 0);
    assert_eq!(sys_entry["contested"], "contested");

    // 3. /fw/stats
    let (stats, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/stats/"));
    let cal_stats = stats
        .as_array()
        .expect("stats array")
        .iter()
        .find(|s| s["faction_id"] == 500_001)
        .expect("Caldari stats");
    assert!(cal_stats["pilots"].as_i64().unwrap_or(0) >= 1);
    assert!(cal_stats["kills"]["total"].as_i64().unwrap_or(0) >= 1);

    // 4. /fw/leaderboards, /fw/leaderboards/characters, /fw/leaderboards/corporations
    let (lb, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"));
    assert!(lb["kills"]["active_total"].is_array());
    let (lb_chars, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"),
    );
    assert!(lb_chars["kills"]["active_total"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["character_id"] == char_a));
    let (lb_corps, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"),
    );
    assert!(lb_corps["kills"]["active_total"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["corporation_id"] == corp_a));

    // 5. /characters/{character_id}/fw/stats: 401, 403, 200, 304, 404
    let char_token = issue_token_with_scopes(
        &ctx,
        "fw_pilot_a",
        char_a,
        vec![
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-corporations.read_fw_stats.v1".to_string(),
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, "fw_pilot_a", char_a, vec![]);

    let char_path = format!("/v1/characters/{char_a}/fw/stats/");
    validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &char_path));
    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&no_scope_token),
    );
    let (char_fw, headers) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token),
    );
    assert_eq!(char_fw["faction_id"], 500_001);
    assert!(char_fw["enlisted_on"].is_string());
    assert!(char_fw["kills"]["total"].as_i64().unwrap_or(0) >= 1);

    let etag = headers.get("ETag").or_else(|| headers.get("etag")).cloned().expect("ETag");
    let mut cached_req = EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token);
    cached_req.headers.insert("if-none-match".to_string(), etag);
    validate_and_json(&router, &spec, 304, &cached_req);

    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", "/v1/characters/999999999/fw/stats/").with_bearer_token(&char_token),
    );
    let unenlisted_token = issue_token_with_scopes(
        &ctx,
        "fw_unenlisted",
        char_u,
        vec![
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-corporations.read_fw_stats.v1".to_string(),
        ],
    );
    validate_and_json(
        &router,
        &spec,
        404,
        &EsiHttpRequest::new("GET", &format!("/v1/characters/{char_u}/fw/stats/"))
            .with_bearer_token(&unenlisted_token),
    );

    // 6. /corporations/{corporation_id}/fw/stats: 401, 403, 200, 404
    let corp_path = format!("/v1/corporations/{corp_a}/fw/stats/");
    validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &corp_path));
    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&no_scope_token),
    );
    let (corp_fw, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&char_token),
    );
    assert_eq!(corp_fw["faction_id"], 500_001);
    assert!(corp_fw["pilots"].as_i64().unwrap_or(0) >= 1);
    assert!(corp_fw["enlisted_on"].is_string());

    validate_and_json(
        &router,
        &spec,
        403,
        &EsiHttpRequest::new("GET", "/v1/corporations/999999999/fw/stats/").with_bearer_token(&char_token),
    );
    validate_and_json(
        &router,
        &spec,
        404,
        &EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_u}/fw/stats/"))
            .with_bearer_token(&unenlisted_token),
    );

    // 7. SQLite persistence across ServerContext reload
    drop(router);
    drop(ctx);
    let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
    let reloaded_token = issue_token_with_scopes(
        &ctx_reloaded,
        "fw_pilot_a",
        char_a,
        vec!["esi-characters.read_fw_stats.v1".to_string()],
    );
    let router_reloaded = EsiRouter::new(ctx_reloaded);
    let (char_fw_after, _) = validate_and_json(
        &router_reloaded,
        &spec,
        200,
        &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&reloaded_token),
    );
    assert_eq!(char_fw_after["faction_id"], 500_001);
    assert!(char_fw_after["kills"]["total"].as_i64().unwrap_or(0) >= 1);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_b6_incursions_and_sovereignty_campaigns_structures_live_state() {
    // parity: esi:GET /incursions, esi:GET /sovereignty/campaigns, esi:GET /sovereignty/structures
    eve_testkit::attest(&[
        "esi:GET /incursions",
        "esi:GET /sovereignty/campaigns",
        "esi:GET /sovereignty/structures",
    ]);

    let db_path = std::env::temp_dir().join(format!("esi_b6_sov_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
    common::pin_clock(&ctx);

    // 1. Seed live SDE-topology incursions via ensure_special_sites_spawned
    ensure_special_sites_spawned(&ctx).await;
    let (cid, staging_sys, site_id, initial_inf) = with_pve_special_reg(&ctx, |reg| {
        let (&cid, inc) = reg.incursions.iter().next().expect("seeded incursion");
        let site = inc.sites.iter().find(|s| !s.locked_until_low_influence).expect("unlocked site");
        (cid, site.solar_system_id, site.site_id.clone(), inc.influence)
    });

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let (inc_list, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
    let inc_arr = inc_list.as_array().expect("incursions array");
    assert_eq!(inc_arr.len(), 3);
    let target_inc = inc_arr
        .iter()
        .find(|i| i["constellation_id"] == cid as i64)
        .expect("target incursion");
    assert_eq!(target_inc["faction_id"], 500_019);
    assert_eq!(target_inc["type"], "Incursion");

    // Complete an incursion site and verify influence decreases via ESI
    let (pilot_id, _, _) = create_pilot_in_new_corp(&ctx, "inc_pilot", "Inc Pilot", "INCP").await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.solar_system_id = staging_sys;
        }
    });
    let boss_id = common::clear_incursion_site_escort_waves(&ctx, staging_sys, &site_id).await;
    let _ = ctx.systems.get_or_spawn(staging_sys).remove_ball(boss_id).await;
    on_incursion_boss_destroyed(&ctx, staging_sys, boss_id, Some(pilot_id)).await;

    let (inc_after, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
    let after_inf = inc_after
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["constellation_id"] == cid as i64)
        .map(|i| i["influence"].as_f64().unwrap())
        .unwrap_or(0.0);
    assert!(after_inf < initial_inf, "Incursion influence must decrease after site completion");

    // 2. Anchor a Sovereignty Hub in nullsec, claim sovereignty, and hack it to start a campaign
    let (_char_s, corp_s, _) =
        create_pilot_in_new_corp(&ctx, "sov_pilot", "Sov Pilot", "SOVP").await;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.1)
        .map(|s| s.solar_system_id)
        .expect("nullsec system");
    let alliance_id = 99_000_601_i64;
    let hub_id = 77_000_601_i64;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
        if let Some(c) = snap.corporations.get_mut(&corp_s) {
            c.alliance_id = Some(alliance_id);
        }
        snap.structures.insert(
            hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_s,
                solar_system_id: null_sys,
                name: "B6 Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structure_vulnerability_windows.insert(hub_id, (now + 3600, now + 7200));
    });
    claim_system_sovereignty(&ctx, null_sys, alliance_id, hub_id, corp_s)
        .expect("claim_system_sovereignty");

    let (sov_structs, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
    );
    let hub_entry = sov_structs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == hub_id)
        .expect("sov hub in /sovereignty/structures");
    assert_eq!(hub_entry["alliance_id"], alliance_id);
    assert_eq!(hub_entry["solar_system_id"], null_sys as i64);

    // Hack the hub via live sov_services::record_hub_hack -> creates campaign & scores
    record_hub_hack(&ctx, null_sys, hub_id, 3600);
    let (campaigns, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
    );
    let camp = campaigns
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["structure_id"] == hub_id)
        .expect("campaign in /sovereignty/campaigns");
    assert_eq!(camp["event_type"], "ihub_defense");
    assert_eq!(camp["defender_id"], alliance_id);
    assert_eq!(camp["attackers_score"], 0.35);
    assert_eq!(camp["defender_score"], 0.65);

    // 3. SQLite persistence across reload
    drop(router);
    drop(ctx);
    let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
    let router_reloaded = EsiRouter::new(ctx_reloaded);
    let (campaigns_after, _) = validate_and_json(
        &router_reloaded,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
    );
    assert!(campaigns_after
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["structure_id"] == hub_id));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_esi_b6_loyalty_store_offers_sde_and_rpc_parity() {
    // parity: esi:GET /loyalty/stores/{corporation_id}/offers
    eve_testkit::attest(&["esi:GET /loyalty/stores/{corporation_id}/offers"]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. Unseeded Paragon (1000419) & CONCORD (1000125) catalogs
    let (paragon_offers, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/"),
    );
    let paragon_arr = paragon_offers.as_array().expect("Paragon offers array");
    assert!(!paragon_arr.is_empty(), "Paragon V24 SDE offers must be non-empty");

    let (concord_offers, _) = validate_and_json(
        &router,
        &spec,
        200,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000125/offers/"),
    );
    assert!(
        concord_offers.as_array().unwrap().len() >= 200,
        "CONCORD catalog must be non-empty"
    );

    // 2. Verify LPStoreMgr.GetAvailableOffersFromCorp RPC parity with ESI
    let (_char_lp, _corp_lp, mut sess) =
        create_pilot_in_new_corp(&ctx, "lp_pilot", "LP Pilot", "LPPP").await;
    let kw = HashMap::new();
    let rpc_res = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(1_000_419)],
            &kw,
            None,
        )
        .await
        .expect("LPStoreMgr.GetAvailableOffersFromCorp");
    let PyRep::List(rpc_offers) = rpc_res else {
        panic!("Expected PyRep::List from GetAvailableOffersFromCorp");
    };
    assert_eq!(rpc_offers.len(), paragon_arr.len());

    // 3. 404 on unknown corporation
    validate_and_json(
        &router,
        &spec,
        404,
        &EsiHttpRequest::new("GET", "/v1/loyalty/stores/999999999/offers/"),
    );
}
