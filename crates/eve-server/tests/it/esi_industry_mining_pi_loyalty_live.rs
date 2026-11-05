//! Authentic live-RPC end-to-end integration tests for ESI Industry, Mining, PI, Market History, and Loyalty Offers:
//! - `feat:esi.industry.facilities`, `esi:GET /industry/facilities`
//! - `feat:esi.industry.char-jobs`, `esi:GET /characters/{character_id}/industry/jobs`
//! - `feat:esi.industry.corp-jobs`, `esi:GET /corporations/{corporation_id}/industry/jobs`
//! - `feat:esi.industry.char-mining`, `esi:GET /characters/{character_id}/mining`
//! - `feat:esi.industry.corp-extractions`, `esi:GET /corporation/{corporation_id}/mining/extractions`
//! - `feat:esi.industry.corp-observers`, `esi:GET /corporation/{corporation_id}/mining/observers`, `esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}`
//! - `feat:esi.planetary-interaction.colonies`, `esi:GET /characters/{character_id}/planets`
//! - `feat:esi.planetary-interaction.layout`, `esi:GET /characters/{character_id}/planets/{planet_id}`
//! - `feat:esi.planetary-interaction.customs`, `esi:GET /corporations/{corporation_id}/customs_offices`
//! - `feat:esi.market.history`, `esi:GET /markets/{region_id}/history`
//! - `feat:esi.loyalty.offers`, `esi:GET /loyalty/stores/{corporation_id}/offers`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn login_and_select(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    ctx.advance_secs(12);
    sess
}

fn issue_sso_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(user, pass, char_id, "evomu-live-esi-it", "https://localhost/callback", scope_vec, PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "evomu-live-esi-it", PKCE_VERIFIER, now).expect("exchange_pkce_code").access_token
}

fn check_esi_200(router: &EsiRouter, spec: &OpenApiSpec, target: &str, token: Option<&str>) -> (Value, String, Option<String>) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(resp.status_code, 200, "Expected 200 for {target}, got {} ({})", resp.status_code, String::from_utf8_lossy(&resp.body));
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {target}: {e}"));
    let etag = resp.header("ETag").expect("ETag header required").to_string();
    assert!(resp.header("Cache-Control").is_some() || resp.header("Expires").is_some());
    let x_pages = resp.header("X-Pages").map(str::to_string);
    (resp.json().expect("valid JSON"), etag, x_pages)
}

fn assert_esi_status(router: &EsiRouter, target: &str, token: Option<&str>, expected: u16) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, expected, "Expected {expected} for {target}, got {} ({})", resp.status_code, String::from_utf8_lossy(&resp.body));
}

fn find_hangar_items(ctx: &ServerContext, owner_id: i64, loc_id: i64, type_id: u32) -> Vec<(i64, i32)> {
    let mut out: Vec<_> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == owner_id && i.location_id == loc_id && i.type_id == type_id)
            .map(|i| (i.item_id, i.quantity))
            .collect()
    });
    out.sort_by_key(|x| x.0);
    out
}

fn collect_skill_topo(sde: &eve_sde::SdeRegistry, sid: u32, seen: &mut Vec<u32>, out: &mut Vec<u32>) {
    if seen.contains(&sid) {
        return;
    }
    seen.push(sid);
    if let Some(t) = sde.types.get(&sid) {
        for (&aid, &val) in &t.default_attributes {
            if let Some(idx) = [182_u16, 183, 184, 1285, 1289, 1290].iter().position(|&a| a == aid) {
                let req = val as u32;
                let lvl_attr = [277_u16, 278, 279, 1286, 1287, 1288][idx];
                if req > 0 && t.default_attributes.get(&lvl_attr).copied().unwrap_or(0.0) > 0.0 {
                    collect_skill_topo(sde, req, seen, out);
                }
            }
        }
        if t.category_id == 16 {
            out.push(sid);
        }
    }
}

async fn grant_skills_for_type(ctx: &ServerContext, sess: &mut SessionState, type_id: u32) {
    let (mut seen, mut ordered) = (Vec::new(), Vec::new());
    collect_skill_topo(&ctx.sde, type_id, &mut seen, &mut ordered);
    for sid in ordered {
        common::slash_cmd(ctx, sess, &format!("/giveskill {sid} 5")).await;
    }
}

async fn setup_world(sess_base: u64, with_corp: Option<(&str, &str)>) -> (Arc<ServerContext>, EsiRouter, &'static OpenApiSpec, SessionState, SessionState, i64) {
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    ctx.db.provision_gm_account("demo", "demo-secret", PILOT_1, "Aura GM");
    ctx.db.provision_gm_account("other", "other-secret", PILOT_2, "Rival GM");

    let mut s1 = login_and_select(&ctx, sess_base, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, sess_base + 1, "other", "other-secret", PILOT_2).await;
    common::slash_cmd(&ctx, &mut s1, "/giveisk 500000000").await;
    common::slash_cmd(&ctx, &mut s2, "/giveisk 500000000").await;
    ctx.advance_secs(12);

    let corp_id = if let Some((name, ticker)) = with_corp {
        let rep = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String(name.into()), PyRep::String(ticker.into()), PyRep::Real(0.05)])
            .await
            .expect("AddCorporation");
        let cid = rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
        ctx.advance_virtual_clock(8 * 86_400);
        rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(cid), PyRep::Real(100_000_000.0), PyRep::String("Fund corp".into())])
            .await
            .expect("GiveCash to corp");
        cid
    } else {
        0
    };
    (ctx, router, spec, s1, s2, corp_id)
}

#[tokio::test]
async fn test_esi_live_industry_facilities_and_jobs() {
    // parity: feat:esi.industry.facilities, esi:GET /industry/facilities
    // parity: feat:esi.industry.char-jobs, esi:GET /characters/{character_id}/industry/jobs
    // parity: feat:esi.industry.corp-jobs, esi:GET /corporations/{corporation_id}/industry/jobs
    eve_testkit::attest(&[
        "feat:esi.industry.facilities",
        "esi:GET /industry/facilities",
        "feat:esi.industry.char-jobs",
        "esi:GET /characters/{character_id}/industry/jobs",
        "feat:esi.industry.corp-jobs",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
    ]);

    let (ctx, router, spec, mut s1, _s2, corp_id) = setup_world(9101, Some(("Live Industry Corp", "LIND"))).await;
    common::slash_cmd(&ctx, &mut s1, "/giveskill 3380 5").await;
    common::slash_cmd(&ctx, &mut s1, "/giveskill 3387 5").await;

    // 1. Public /industry/facilities/ includes Jita 4-4 NPC station
    let (fac_val, _, _) = check_esi_200(&router, &spec, "/v1/industry/facilities/", None);
    let jita_fac = fac_val.as_array().unwrap().iter().find(|f| f["facility_id"] == json!(JITA_STATION)).expect("Jita 4-4 in /industry/facilities");
    assert_eq!(jita_fac["region_id"], json!(10_000_002));
    assert_eq!(jita_fac["solar_system_id"], json!(JITA_SYS));
    assert!((jita_fac["tax"].as_f64().unwrap() - 0.0025).abs() < 1e-6);

    // 2. Give Rifter BPO (691) x2 and minerals, install personal & corp manufacturing jobs
    for cmd in ["/giveitem 691 1", "/giveitem 691 1", "/giveitem 34 20000", "/giveitem 35 10000", "/giveitem 36 5000", "/giveitem 37 1000"] {
        common::slash_cmd(&ctx, &mut s1, cmd).await;
    }
    ctx.advance_secs(12);

    let bps = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 691);
    let (char_bp_id, corp_bp_id) = (bps[0].0, bps[1].0);

    let char_job_rep = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(char_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("Install personal job");
    let char_job_id = char_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("char_job_id");

    let rent_rep = rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(JITA_STATION)]).await.expect("RentOffice");
    let office_id = rent_rep.dict_get("officeFolderID").or_else(|| rent_rep.dict_get("officeID")).and_then(|v| v.as_i64()).expect("office_id");

    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(corp_bp_id), PyRep::Integer(office_id), PyRep::Integer(116), PyRep::Integer(1)])
        .await
        .expect("Move corp BPO");
    for (tid, qty) in [(34_u32, 5000_i64), (35, 2500), (36, 1000), (37, 250)] {
        let iid = find_hangar_items(&ctx, PILOT_1, JITA_STATION, tid)[0].0;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(iid), PyRep::Integer(office_id), PyRep::Integer(116), PyRep::Integer(qty)])
            .await
            .expect("Move corp minerals");
    }

    let corp_job_rep = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(corp_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
        ])],
    )
    .await
    .expect("Install corp job");
    let corp_job_id = corp_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp_job_id");

    let tok_p1 = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-industry.read_character_jobs.v1", "esi-industry.read_corporation_jobs.v1"]);
    let tok_p2 = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-industry.read_character_jobs.v1", "esi-industry.read_corporation_jobs.v1"]);

    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/industry/jobs/"), None, 401);
    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/industry/jobs/"), Some(&tok_p2), 403);
    assert_esi_status(&router, &format!("/v1/corporations/{corp_id}/industry/jobs/"), Some(&tok_p2), 403);

    let (c_jobs, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/industry/jobs/?include_completed=true"), Some(&tok_p1));
    let c_arr = c_jobs.as_array().unwrap();
    assert!(c_arr.iter().any(|j| j["job_id"] == json!(char_job_id) && j["status"] == json!("active")));
    assert!(!c_arr.iter().any(|j| j["job_id"] == json!(corp_job_id)));

    let (corp_jobs, _, x_pages) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"), Some(&tok_p1));
    assert_eq!(x_pages.as_deref(), Some("1"));
    let corp_arr = corp_jobs.as_array().unwrap();
    assert!(corp_arr.iter().any(|j| j["job_id"] == json!(corp_job_id) && j["status"] == json!("active")));
    assert!(!corp_arr.iter().any(|j| j["job_id"] == json!(char_job_id)));

    ctx.advance_secs(10_000);
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(char_job_id)]).await.expect("CompleteJob");
    let tok_p1_after = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-industry.read_character_jobs.v1"]);

    let (c_active, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/industry/jobs/?include_completed=false"), Some(&tok_p1_after));
    assert!(!c_active.as_array().unwrap().iter().any(|j| j["job_id"] == json!(char_job_id)));

    let (c_all, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/industry/jobs/?include_completed=true"), Some(&tok_p1_after));
    assert!(c_all.as_array().unwrap().iter().any(|j| j["job_id"] == json!(char_job_id) && j["status"] == json!("delivered")));
}

#[tokio::test]
async fn test_esi_live_mining_ledger_extractions_and_observers() {
    // parity: feat:esi.industry.char-mining, esi:GET /characters/{character_id}/mining
    // parity: feat:esi.industry.corp-extractions, esi:GET /corporation/{corporation_id}/mining/extractions
    // parity: feat:esi.industry.corp-observers, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
    eve_testkit::attest(&[
        "feat:esi.industry.char-mining",
        "esi:GET /characters/{character_id}/mining",
        "feat:esi.industry.corp-extractions",
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "feat:esi.industry.corp-observers",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
    ]);

    let (ctx, router, spec, mut s1, _s2, corp_id) = setup_world(9201, Some(("Live Mining Corp", "LMIN"))).await;
    grant_skills_for_type(&ctx, &mut s1, 32880).await;
    grant_skills_for_type(&ctx, &mut s1, 483).await;
    common::slash_cmd(&ctx, &mut s1, "/giveskill 3410 5").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 32880 1").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 483 1").await;
    ctx.advance_secs(12);

    // Anchor & online Athanor (35835) with Standup Moon Drill I (35894) and start moon extraction FIRST
    // so the structure is present as a mining observer in PERIMETER_SYS when PILOT_1 mines.
    let athanor_tid = 35835_u32;
    let core_tid = ctx.sde.types[&athanor_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    grant_skills_for_type(&ctx, &mut s1, 20185).await;

    for cmd in [
        "/createitem 20185 1".into(),
        format!("/createitem {athanor_tid} 1"),
        format!("/createitem {core_tid} 1"),
        "/createitem 35894 1".into(),
        "/createitem 4051 3000".into(),
    ] {
        common::slash_cmd(&ctx, &mut s1, &cmd).await;
    }
    ctx.advance_secs(12);

    let charon_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 20185)[0].0;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(charon_id)]).await.expect("AssembleShip Charon");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(charon_id)]).await.expect("Board Charon");
    for tid in [athanor_tid, core_tid, 35894, 4051] {
        let iid = find_hangar_items(&ctx, PILOT_1, JITA_STATION, tid)[0].0;
        let qty = if tid == 4051 { 3000 } else { 1 };
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(iid), PyRep::Integer(charon_id), PyRep::Integer(5), PyRep::Integer(qty)]).await.expect("Load Charon");
    }
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock Charon");
    common::slash_cmd(&ctx, &mut s1, "/tr 30000144").await;
    ctx.advance_secs(12);

    let perim = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let anchor_rep = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(athanor_tid as i64), PyRep::String("Live Moon Refinery".into())])
        .await
        .expect("Anchor Athanor");
    let athanor_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("athanor_id");

    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let st_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&athanor_id].position));
    perim.set_ball_position(charon_id, st_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(athanor_id)]).await.expect("Dock Athanor");

    let core_item = find_hangar_items(&ctx, PILOT_1, charon_id, core_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(athanor_id), PyRep::Integer(172)]).await.expect("Quantum Core");
    let fuel_item = find_hangar_items(&ctx, PILOT_1, charon_id, 4051)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(athanor_id), PyRep::Integer(133), PyRep::Integer(2900)]).await.expect("Fuel Bay");
    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    let drill_item = find_hangar_items(&ctx, PILOT_1, charon_id, 35894)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(drill_item), PyRep::Integer(athanor_id), PyRep::Integer(164)]).await.expect("Moon Drill");

    rpc(&ctx, &mut s1, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)]).await.expect("StartNewExtraction");

    // Return to Jita, board Venture with Miner I, return to Perimeter, and mine an asteroid
    let st_pos_jita = ctx.sde.stations[&JITA_STATION].position;
    common::slash_cmd(&ctx, &mut s1, "/tr 30000142").await;
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(charon_id, st_pos_jita).await;
    rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION)]).await.expect("Dock Jita");

    let venture_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 32880)[0].0;
    let miner_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 483)[0].0;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(venture_id)]).await.expect("AssembleShip");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(venture_id)]).await.expect("Board");
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(miner_id), PyRep::Integer(venture_id), PyRep::Integer(27)]).await.expect("Fit Miner I");
    ctx.advance_secs(12);

    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock");
    common::slash_cmd(&ctx, &mut s1, "/tr 30000144").await;
    ctx.advance_secs(12);

    let rocks = perim.populate_asteroid_belt_entities().await.expect("populate belts");
    let rock = &rocks[0];
    let rock_pos = perim.get_ball_snapshot(rock.asteroid_id).await.unwrap().unwrap().1.position;
    perim.set_ball_position(venture_id, rock_pos).await;

    rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(venture_id)]).await.expect("Online");
    rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(rock.asteroid_id)]).await.expect("AddTarget");
    for _ in 0..10 {
        ctx.advance_secs(1);
        perim.step_manual(1.0).await.unwrap();
        if let Ok(locks) = perim.get_target_locks(venture_id).await {
            if locks.iter().any(|l| l.target_id == rock.asteroid_id && l.locked) {
                break;
            }
        }
    }
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(miner_id), PyRep::Integer(rock.asteroid_id)]).await.expect("Activate Miner I");
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let tok_p1 = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-industry.read_character_mining.v1", "esi-industry.read_corporation_mining.v1"]);
    let tok_p2 = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-industry.read_character_mining.v1", "esi-industry.read_corporation_mining.v1"]);

    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/mining/"), Some(&tok_p2), 403);
    assert_esi_status(&router, &format!("/v1/corporation/{corp_id}/mining/extractions/"), Some(&tok_p2), 403);

    let (char_mine, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/mining/"), Some(&tok_p1));
    let cm_arr = char_mine.as_array().unwrap();
    assert!(!cm_arr.is_empty());
    assert_eq!(cm_arr[0]["solar_system_id"], json!(PERIMETER_SYS));
    assert_eq!(cm_arr[0]["type_id"], json!(rock.type_id));
    assert!(cm_arr[0]["quantity"].as_i64().unwrap() > 0);

    let (exts, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporation/{corp_id}/mining/extractions/"), Some(&tok_p1));
    assert_eq!(exts.as_array().unwrap().len(), 1);
    assert_eq!(exts.as_array().unwrap()[0]["structure_id"], json!(athanor_id));

    let (obs_list, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporation/{corp_id}/mining/observers/"), Some(&tok_p1));
    assert!(obs_list.as_array().unwrap().iter().any(|o| o["observer_id"] == json!(athanor_id) && o["observer_type"] == json!("structure")));

    let (obs_detail, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporation/{corp_id}/mining/observers/{athanor_id}/"), Some(&tok_p1));
    assert!(obs_detail.as_array().unwrap().iter().any(|e| {
        e["character_id"] == json!(PILOT_1)
            && e["recorded_corporation_id"] == json!(corp_id)
            && e["type_id"] == json!(rock.type_id)
            && e["quantity"].as_i64().unwrap() > 0
    }));
}

#[tokio::test]
async fn test_esi_live_planetary_interaction_colonies_layout_and_customs() {
    // parity: feat:esi.planetary-interaction.colonies, esi:GET /characters/{character_id}/planets
    // parity: feat:esi.planetary-interaction.layout, esi:GET /characters/{character_id}/planets/{planet_id}
    // parity: feat:esi.planetary-interaction.customs, esi:GET /corporations/{corporation_id}/customs_offices
    eve_testkit::attest(&[
        "feat:esi.planetary-interaction.colonies",
        "esi:GET /characters/{character_id}/planets",
        "feat:esi.planetary-interaction.layout",
        "esi:GET /characters/{character_id}/planets/{planet_id}",
        "feat:esi.planetary-interaction.customs",
        "esi:GET /corporations/{corporation_id}/customs_offices",
    ]);

    let (ctx, router, spec, mut s1, _s2, corp_id) = setup_world(9301, Some(("Live PI Corp", "LPIC"))).await;
    grant_skills_for_type(&ctx, &mut s1, 20185).await;
    for sid in [2495_u32, 2505, 2403, 2406, 13279] {
        common::slash_cmd(&ctx, &mut s1, &format!("/giveskill {sid} 5")).await;
    }

    let jita_planet_id = ctx
        .sde
        .celestials_in_system(JITA_SYS)
        .into_iter()
        .find(|c| c.group_id == 7 && c.type_id == 13)
        .map(|c| c.celestial_id)
        .expect("Jita Gas planet");

    common::slash_cmd(&ctx, &mut s1, "/giveitem 20185 1").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 2534 1").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 3962 1").await;
    ctx.advance_secs(12);
    let ship_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 20185)[0].0;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(ship_id)]).await.expect("AssembleShip Charon");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("Board Charon");
    let cc_item = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 2534)[0].0;
    let gantry_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 3962)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(cc_item), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(1)]).await.expect("Move CC");
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(gantry_id), PyRep::Integer(ship_id), PyRep::Integer(5), PyRep::Integer(1)]).await.expect("Move gantry");
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock");

    let res_info = rpc(&ctx, &mut s1, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(jita_planet_id)]).await.expect("GetPlanetResourceInfo");
    let PyRep::List(res_list) = res_info.dict_get("resources").expect("resources") else { panic!("Expected list") };
    let first_p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (p1_schem_id, p1_out_tid, p1_out_qty) = ctx
        .sde
        .planet_schematics
        .values()
        .find_map(|s| {
            let is_p1 = s.cycle_time == 1800 && s.pins.contains(&2492) && s.types.iter().any(|t| t.is_input && t.type_id == first_p0_tid);
            let out = is_p1.then(|| s.types.iter().find(|t| !t.is_input)).flatten()?;
            Some((s.schematic_id, out.type_id, out.quantity))
        })
        .expect("SDE P1 schematic");

    rpc(
        &ctx,
        &mut s1,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(jita_planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2543), PyRep::Real(0.13), PyRep::Real(0.13)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(103), PyRep::Integer(104), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(p1_schem_id as i64)]),
                PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(first_p0_tid as i64), PyRep::Integer(3_000)]),
                PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(502), PyRep::List(vec![PyRep::Integer(103), PyRep::Integer(104)]), PyRep::Integer(p1_out_tid as i64), PyRep::Integer(p1_out_qty as i64)]),
                PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(first_p0_tid as i64), PyRep::Real(0.02), PyRep::Integer(3)]),
            ]),
        ],
    )
    .await
    .expect("UserUpdateNetwork");

    // Anchor & online Customs Office in low-sec system
    let ls_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && !ctx.sde.celestials_in_system(s.solar_system_id).is_empty())
        .map(|s| s.solar_system_id)
        .expect("low-sec system");
    let ls_planet = ctx.sde.celestials_in_system(ls_sys_id).into_iter().find(|c| c.group_id == 7).expect("low-sec planet");
    let ls_planet_id = ls_planet.celestial_id;

    common::slash_cmd(&ctx, &mut s1, &format!("/tr {ls_sys_id}")).await;
    ctx.systems.get_or_spawn(ls_sys_id).set_ball_position(ship_id, ls_planet.position).await;
    ctx.advance_secs(12);

    let anchor_rep = rpc(&ctx, &mut s1, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id)]).await.expect("AnchorOrbital");
    let orbital_id = anchor_rep.dict_get("orbitalID").and_then(|v| v.as_i64()).expect("orbitalID");
    common::slash_cmd(&ctx, &mut s1, "/giveitem 2233 1").await;
    rpc(&ctx, &mut s1, "posMgr", "OnlineOrbital", &[PyRep::Integer(orbital_id)]).await.expect("OnlineOrbital");
    rpc(
        &ctx,
        &mut s1,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[
            PyRep::Integer(ls_planet_id),
            PyRep::Integer(18),
            PyRep::KeyVal(HashMap::from([
                ("corporation".to_string(), PyRep::Real(0.04)),
                ("alliance".to_string(), PyRep::Real(0.05)),
                ("standingNeutral".to_string(), PyRep::Real(0.12)),
            ])),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("UpdateSettings");

    let tok_p1 = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-planets.manage_planets.v1", "esi-planets.read_customs_offices.v1"]);
    let tok_p2 = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-planets.manage_planets.v1", "esi-planets.read_customs_offices.v1"]);

    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/planets/"), Some(&tok_p2), 403);
    assert_esi_status(&router, &format!("/v1/corporations/{corp_id}/customs_offices/"), Some(&tok_p2), 403);

    let (colonies, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/planets/"), Some(&tok_p1));
    let col_arr = colonies.as_array().unwrap();
    assert_eq!(col_arr.len(), 1);
    assert_eq!(col_arr[0]["planet_id"], json!(jita_planet_id));
    assert_eq!(col_arr[0]["upgrade_level"], json!(3));
    assert_eq!(col_arr[0]["num_pins"], json!(4));

    let (layout, _, _) = check_esi_200(&router, &spec, &format!("/v3/characters/{PILOT_1}/planets/{jita_planet_id}/"), Some(&tok_p1));
    assert_eq!(layout["pins"].as_array().unwrap().len(), 4);
    assert_eq!(layout["links"].as_array().unwrap().len(), 3);
    assert_eq!(layout["routes"].as_array().unwrap().len(), 2);

    let (offices, _, x_pages) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/customs_offices/"), Some(&tok_p1));
    assert_eq!(x_pages.as_deref(), Some("1"));
    assert!(offices.as_array().unwrap().iter().any(|o| {
        o["system_id"] == json!(ls_sys_id)
            && (o["corporation_tax_rate"].as_f64().unwrap() - 0.04).abs() < 1e-6
            && o["allow_alliance_access"] == json!(true)
    }));
}

#[tokio::test]
async fn test_esi_live_market_history_and_loyalty_offers() {
    // parity: feat:esi.market.history, esi:GET /markets/{region_id}/history
    // parity: feat:esi.loyalty.offers, esi:GET /loyalty/stores/{corporation_id}/offers
    eve_testkit::attest(&[
        "feat:esi.market.history",
        "esi:GET /markets/{region_id}/history",
        "feat:esi.loyalty.offers",
        "esi:GET /loyalty/stores/{corporation_id}/offers",
    ]);

    let (ctx, router, spec, mut s1, mut s2, _) = setup_world(9401, None).await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 34 2000").await;
    ctx.advance_secs(12);
    let trit_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 34)[0].0;

    rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(trit_id)),
                ("price", PyRep::Real(6.50)),
                ("quantity", PyRep::Integer(500)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(14),
        ],
    )
    .await
    .expect("Place sell order");

    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(6.50),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Place matching buy order");

    common::slash_cmd(&ctx, &mut s1, "/downtime").await;

    let (hist, _, _) = check_esi_200(&router, &spec, "/v1/markets/10000002/history/?type_id=34", None);
    let hist_arr = hist.as_array().unwrap();
    assert!(!hist_arr.is_empty(), "Expected market history row after trade + downtime rollup");
    let matched_day = hist_arr
        .iter()
        .find(|d| d["volume"].as_i64().unwrap_or(0) >= 500)
        .expect("market history day with traded volume");
    assert_eq!(matched_day["volume"].as_i64(), Some(500));
    assert_eq!(matched_day["highest"].as_f64(), Some(6.50));
    assert_eq!(matched_day["lowest"].as_f64(), Some(6.50));
    assert_eq!(matched_day["average"].as_f64(), Some(6.50));
    assert!(matched_day["order_count"].as_i64().unwrap() >= 1);
    assert_eq!(matched_day["highest"].as_f64() >= matched_day["lowest"].as_f64(), true);

    assert_esi_status(&router, "/v1/loyalty/stores/not_a_corp/offers/", None, 400);
    assert_esi_status(&router, "/v1/loyalty/stores/999999999/offers/", None, 404);

    let (offers, _, _) = check_esi_200(&router, &spec, "/v1/loyalty/stores/1000035/offers/", None);
    let offers_arr = offers.as_array().unwrap();
    assert!(!offers_arr.is_empty(), "Expected Caldari Navy LP store offers");
    assert!(offers_arr[0]["offer_id"].as_i64().is_some());
    assert!(offers_arr[0]["lp_cost"].as_i64().is_some());
}
