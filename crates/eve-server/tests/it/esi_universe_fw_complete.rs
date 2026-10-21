//! `ESI-3` Meta, Universe (Ancestries/Bloodlines/Races, Factions/Graphics, Celestials,
//! Stations/Structures, System Jumps/Kills), and Faction Warfare acceptance tests against the
//! real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.
//!
//! Ledger tags live inside each `#[tokio::test]` body next to its `eve_testkit::attest` call.

#![forbid(unsafe_code)]

use eve_db::{
    CorporationRecord, FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat, IskCents,
    KillmailDetailRecord, KillmailRecord, SystemDynamicState, SystemJumpEvent, SystemKillEvent,
    UpwellStructureRecord, WarRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-fw-complete-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-fw-complete-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

/// Issues the request, asserts the status, validates status/headers/body against the pinned
/// OpenAPI operation, checks the caching contract (ETag, Cache-Control, Expires, Last-Modified) and
/// that a conditional re-request with the ETag yields `304 Not Modified` with an empty body.
fn call(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
    let spec = OpenApiSpec::pinned();
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap().to_string();
    assert_eq!(
        resp.status_code,
        expected,
        "{} {} -> {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    if expected != 405 {
        spec.validate_response(&req.method, &raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
    }
    let body = resp.json().expect("JSON body");
    if expected == 200 {
        let etag = resp.header("ETag").expect("ETag").to_string();
        let cc = resp.header("Cache-Control").expect("Cache-Control");
        assert!(
            cc.contains("max-age="),
            "expected max-age in Cache-Control: {cc}"
        );
        assert!(resp.header("Expires").is_some() && resp.header("Last-Modified").is_some());
        let again = router.handle_request(&req.clone().with_header("If-None-Match", etag.clone()));
        assert_eq!(again.status_code, 304, "conditional GET with matching ETag");
        assert!(again.body.is_empty());
        assert_eq!(again.header("ETag"), Some(etag.as_str()));
    } else {
        assert!(
            body["error"].as_str().is_some_and(|s| !s.is_empty()),
            "ESI error model"
        );
    }
    body
}

#[tokio::test]
async fn test_esi_meta_changelog_and_universe_races_factions_graphics() {
    // parity: esi:GET /meta/changelog, esi:GET /universe/races, esi:GET /universe/graphics
    // parity: esi:GET /universe/graphics/{graphic_id}, feat:esi.universe.ancestries-bloodlines-races
    // parity: feat:esi.universe.factions-graphics
    eve_testkit::attest(&[
        "esi:GET /meta/changelog",
        "esi:GET /universe/races",
        "esi:GET /universe/graphics",
        "esi:GET /universe/graphics/{graphic_id}",
        "feat:esi.universe.ancestries-bloodlines-races",
        "feat:esi.universe.factions-graphics",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. Meta Changelog (`GET /meta/changelog`)
    let cl_json = call(&router, EsiHttpRequest::new("GET", "/meta/changelog"), 200);
    let compat_date = &spec.compatibility_dates[0];
    let entries = cl_json["changelog"][compat_date]
        .as_array()
        .expect("changelog array for compatibility date");
    assert!(!entries.is_empty());
    assert_eq!(entries[0]["compatibility_date"], json!(compat_date));
    assert_eq!(entries[0]["path"], json!("/meta/changelog"));
    assert_eq!(entries[0]["method"], json!("GET"));
    assert_eq!(entries[0]["is_breaking"], json!(false));

    // 2. Universe Ancestries, Bloodlines, and Races (`GET /universe/ancestries`, `/bloodlines`, `/races`)
    let ancestries = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/ancestries/"),
        200,
    );
    let anc_arr = ancestries.as_array().expect("ancestries array");
    assert_eq!(anc_arr.len(), ctx.sde.ancestries.len());
    for (&aid, sde_anc) in ctx.sde.ancestries.iter().take(5) {
        let entry = anc_arr
            .iter()
            .find(|a| a["id"] == json!(aid as i64))
            .expect("ancestry entry");
        assert_eq!(entry["name"], json!(sde_anc.name));
        assert_eq!(entry["bloodline_id"], json!(sde_anc.bloodline_id as i64));
        assert_eq!(entry["description"], json!(sde_anc.description));
    }

    let bloodlines = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/bloodlines/"),
        200,
    );
    let bl_arr = bloodlines.as_array().expect("bloodlines array");
    let expected_bl_count = ctx
        .sde
        .bloodlines
        .values()
        .filter(|b| {
            b.ship_type_id
                .or_else(|| ctx.sde.races.get(&b.race_id).and_then(|r| r.corvette_type_id))
                .is_some()
        })
        .count();
    assert_eq!(bl_arr.len(), expected_bl_count);
    for (&bid, sde_bl) in ctx
        .sde
        .bloodlines
        .iter()
        .filter(|(_, b)| {
            b.ship_type_id
                .or_else(|| ctx.sde.races.get(&b.race_id).and_then(|r| r.corvette_type_id))
                .is_some()
        })
        .take(5)
    {
        let entry = bl_arr
            .iter()
            .find(|b| b["bloodline_id"] == json!(bid as i64))
            .expect("bloodline entry");
        assert_eq!(entry["name"], json!(sde_bl.name));
        assert_eq!(entry["race_id"], json!(sde_bl.race_id as i64));
        assert_eq!(entry["corporation_id"], json!(sde_bl.corporation_id));
        assert_eq!(entry["charisma"], json!(sde_bl.charisma as i64));
        assert_eq!(entry["intelligence"], json!(sde_bl.intelligence as i64));
        assert_eq!(entry["memory"], json!(sde_bl.memory as i64));
        assert_eq!(entry["perception"], json!(sde_bl.perception as i64));
        assert_eq!(entry["willpower"], json!(sde_bl.willpower as i64));
    }

    let races = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/races/"),
        200,
    );
    let races_arr = races.as_array().expect("races array");
    assert_eq!(races_arr.len(), ctx.sde.races.len());
    for (rid, expected_fid) in [
        (1_i64, 500_001_i64),
        (2, 500_002),
        (4, 500_003),
        (8, 500_004),
    ] {
        let r_entry = races_arr
            .iter()
            .find(|r| r["race_id"] == json!(rid))
            .expect("empire race entry");
        let sde_r = ctx.sde.races.get(&(rid as u32)).expect("SDE race");
        assert_eq!(r_entry["name"], json!(sde_r.name));
        assert_eq!(r_entry["alliance_id"], json!(expected_fid));
    }

    // 3. Universe Factions and Graphics (`GET /universe/factions`, `/graphics`, `/graphics/{graphic_id}`)
    let factions = call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/factions/"),
        200,
    );
    let fac_arr = factions.as_array().expect("factions array");
    assert_eq!(fac_arr.len(), ctx.sde.factions.len());
    let caldari = fac_arr
        .iter()
        .find(|f| f["faction_id"] == json!(500_001))
        .expect("Caldari State faction");
    let sde_caldari = &ctx.sde.factions[&500_001];
    assert_eq!(caldari["name"], json!(sde_caldari.name));
    assert_eq!(
        caldari["corporation_id"],
        json!(sde_caldari.corporation_id.unwrap())
    );
    assert_eq!(
        caldari["solar_system_id"],
        json!(sde_caldari.solar_system_id.unwrap() as i64)
    );

    let graphics = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/graphics/"),
        200,
    );
    let gfx_arr = graphics.as_array().expect("graphics array");
    let expected_graphics: std::collections::BTreeSet<i64> = ctx
        .sde
        .types
        .values()
        .filter_map(|t| t.graphic_id.map(|g| g as i64))
        .collect();
    assert_eq!(gfx_arr.len(), expected_graphics.len());
    let sample_gid = *expected_graphics.iter().next().expect("at least one graphic");
    assert_eq!(gfx_arr[0], json!(sample_gid));

    let gfx_detail = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/universe/graphics/{sample_gid}/")),
        200,
    );
    assert_eq!(gfx_detail["graphic_id"], json!(sample_gid));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/graphics/999999999/"),
        404,
    );
}

#[tokio::test]
async fn test_esi_faction_warfare_routes_reflect_live_state() {
    // parity: esi:GET /characters/{character_id}/fw/stats, esi:GET /corporations/{corporation_id}/fw/stats
    // parity: esi:GET /fw/leaderboards, esi:GET /fw/leaderboards/characters, esi:GET /fw/leaderboards/corporations
    // parity: esi:GET /fw/stats, esi:GET /fw/systems, esi:GET /fw/wars
    // parity: feat:esi.faction-warfare.char-corp-stats, feat:esi.faction-warfare.leaderboards
    // parity: feat:esi.faction-warfare.stats, feat:esi.faction-warfare.systems, feat:esi.faction-warfare.wars
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/fw/stats",
        "esi:GET /corporations/{corporation_id}/fw/stats",
        "esi:GET /fw/leaderboards",
        "esi:GET /fw/leaderboards/characters",
        "esi:GET /fw/leaderboards/corporations",
        "esi:GET /fw/stats",
        "esi:GET /fw/systems",
        "esi:GET /fw/wars",
        "feat:esi.faction-warfare.char-corp-stats",
        "feat:esi.faction-warfare.leaderboards",
        "feat:esi.faction-warfare.stats",
        "feat:esi.faction-warfare.systems",
        "feat:esi.faction-warfare.wars",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;

    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_001_u32;
    let char_enlisted_unix = now - 604_800;
    let corp_enlisted_unix = now - 1_209_600;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.war_faction_id = Some(500_001); // Caldari State FW
            ch.solar_system_id = sys_id;
            ch.loyalty_points.insert(1_000_035, 99_999); // LP balance must not leak into FW victory points
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "FW Frontline Corp".to_string(),
                ticker: "FWCORP".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(char_enlisted_unix),
                current_rank: 3,
                highest_rank: 4,
                kills: FwWindowedStat {
                    yesterday: 2,
                    last_week: 5,
                    total: 12,
                },
                victory_points: FwWindowedStat {
                    yesterday: 400,
                    last_week: 1_200,
                    total: 3_400,
                },
            },
        );
        snap.fw_corp_enlisted_on.insert(corp_id, corp_enlisted_unix);
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_001,
                victory_points: 2_250,
                victory_points_threshold: 4_500,
            },
        );
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 0,
                industrial_index: 0,
                strategic_index: 0,
                fw_contested_pct: 50.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
    });

    let char_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-characters.read_fw_stats.v1".to_string(),
            "esi-corporations.read_fw_stats.v1".to_string(),
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);

    // 1. Character FW Stats (`GET /characters/{character_id}/fw/stats`)
    let char_fw = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/"))
            .with_bearer_token(&char_token),
        200,
    );
    assert_eq!(char_fw["faction_id"], json!(500_001));
    assert_eq!(char_fw["current_rank"], json!(3));
    assert_eq!(char_fw["highest_rank"], json!(4));
    assert_eq!(char_fw["enlisted_on"], json!(fmt(char_enlisted_unix)));
    assert_eq!(
        char_fw["kills"],
        json!({ "yesterday": 2, "last_week": 5, "total": 12 })
    );
    assert_eq!(
        char_fw["victory_points"],
        json!({ "yesterday": 400, "last_week": 1_200, "total": 3_400 })
    );

    // Scope & identity enforcement (401 without token, 403 without scope, 403 for other character)
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/")),
        401,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/"))
            .with_bearer_token(&no_scope_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{other_char_id}/fw/stats/"))
            .with_bearer_token(&char_token),
        403,
    );

    // 2. Corporation FW Stats (`GET /corporations/{corporation_id}/fw/stats`)
    let corp_fw = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/"))
            .with_bearer_token(&char_token),
        200,
    );
    assert_eq!(corp_fw["faction_id"], json!(500_001));
    assert_eq!(corp_fw["pilots"], json!(1));
    assert_eq!(corp_fw["enlisted_on"], json!(fmt(corp_enlisted_unix)));
    assert_eq!(
        corp_fw["kills"],
        json!({ "yesterday": 2, "last_week": 5, "total": 12 })
    );
    assert_eq!(
        corp_fw["victory_points"],
        json!({ "yesterday": 400, "last_week": 1_200, "total": 3_400 })
    );

    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/")),
        401,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/"))
            .with_bearer_token(&no_scope_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/corporations/99999999/fw/stats/")
            .with_bearer_token(&char_token),
        403,
    );

    // 3. FW Leaderboards (`GET /fw/leaderboards`, `/characters`, `/corporations`)
    let lb = call(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"), 200);
    for (bucket, expected_kills, expected_vp) in [
        ("yesterday", 2_i64, 400_i64),
        ("last_week", 5, 1_200),
        ("active_total", 12, 3_400),
    ] {
        let k_arr = lb["kills"][bucket].as_array().unwrap();
        assert_eq!(k_arr.len(), 4);
        assert_eq!(
            k_arr.iter().find(|e| e["faction_id"] == json!(500_001)).unwrap()["amount"],
            json!(expected_kills)
        );
        let vp_arr = lb["victory_points"][bucket].as_array().unwrap();
        assert_eq!(vp_arr.len(), 4);
        assert_eq!(
            vp_arr.iter().find(|e| e["faction_id"] == json!(500_001)).unwrap()["amount"],
            json!(expected_vp)
        );
    }

    let lb_char = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"),
        200,
    );
    assert_eq!(
        lb_char["kills"]["yesterday"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["character_id"] == json!(char_id))
            .unwrap()["amount"],
        json!(2)
    );
    assert_eq!(
        lb_char["kills"]["last_week"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["character_id"] == json!(char_id))
            .unwrap()["amount"],
        json!(5)
    );
    assert_eq!(
        lb_char["kills"]["active_total"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["character_id"] == json!(char_id))
            .unwrap()["amount"],
        json!(12)
    );

    let lb_corp = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"),
        200,
    );
    assert_eq!(
        lb_corp["victory_points"]["yesterday"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["corporation_id"] == json!(corp_id))
            .unwrap()["amount"],
        json!(400)
    );
    assert_eq!(
        lb_corp["victory_points"]["last_week"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["corporation_id"] == json!(corp_id))
            .unwrap()["amount"],
        json!(1_200)
    );
    assert_eq!(
        lb_corp["victory_points"]["active_total"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["corporation_id"] == json!(corp_id))
            .unwrap()["amount"],
        json!(3_400)
    );

    // 4. FW Stats (`GET /fw/stats`) — verify populated faction and unpopulated faction (0 pilots, 0 systems)
    let fw_stats = call(&router, EsiHttpRequest::new("GET", "/v1/fw/stats/"), 200);
    let fac_stats_arr = fw_stats.as_array().expect("fw stats array");
    assert_eq!(fac_stats_arr.len(), 4);
    let caldari_fw = fac_stats_arr
        .iter()
        .find(|s| s["faction_id"] == json!(500_001))
        .expect("Caldari FW stats");
    assert_eq!(caldari_fw["pilots"], json!(1));
    assert_eq!(caldari_fw["systems_controlled"], json!(1));
    assert_eq!(
        caldari_fw["kills"],
        json!({ "yesterday": 2, "last_week": 5, "total": 12 })
    );
    assert_eq!(
        caldari_fw["victory_points"],
        json!({ "yesterday": 400, "last_week": 1_200, "total": 3_400 })
    );
    let minmatar_fw = fac_stats_arr
        .iter()
        .find(|s| s["faction_id"] == json!(500_002))
        .expect("Minmatar FW stats");
    assert_eq!(minmatar_fw["pilots"], json!(0));
    assert_eq!(minmatar_fw["systems_controlled"], json!(0));

    // 5. FW Systems (`GET /fw/systems`) + live capture mutation
    let fw_systems = call(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_arr = fw_systems.as_array().expect("fw systems array");
    let sys_entry = sys_arr
        .iter()
        .find(|s| s["solar_system_id"] == json!(sys_id))
        .expect("contested system in /fw/systems");
    assert_eq!(sys_entry["owner_faction_id"], json!(500_004));
    assert_eq!(sys_entry["occupier_faction_id"], json!(500_001));
    assert_eq!(sys_entry["contested"], json!("contested"));
    assert_eq!(sys_entry["victory_points"], json!(2_250));
    assert_eq!(sys_entry["victory_points_threshold"], json!(4_500));

    // Mutate system capture to 4500/4500 -> becomes "vulnerable", and mutate character FW stats -> live update.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(occ) = snap.fw_system_occupations.get_mut(&sys_id) {
            occ.victory_points = 4_500;
        }
        if let Some(rec) = snap.fw_character_stats.get_mut(&char_id) {
            rec.kills.yesterday += 1;
            rec.kills.last_week += 1;
            rec.kills.total += 1;
            rec.victory_points.yesterday += 150;
            rec.victory_points.last_week += 150;
            rec.victory_points.total += 150;
        }
    });
    let fw_systems_after = call(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_after = fw_systems_after
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["solar_system_id"] == json!(sys_id))
        .unwrap();
    assert_eq!(sys_after["contested"], json!("vulnerable"));
    assert_eq!(sys_after["victory_points"], json!(4_500));
    assert_eq!(sys_after["victory_points_threshold"], json!(4_500));

    let char_fw_after = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/"))
            .with_bearer_token(&char_token),
        200,
    );
    assert_eq!(char_fw_after["kills"]["total"], json!(13));
    assert_eq!(char_fw_after["victory_points"]["total"], json!(3_550));

    // 6. FW Wars (`GET /fw/wars`) + live inter-militia war mutation
    let fw_wars = call(&router, EsiHttpRequest::new("GET", "/v1/fw/wars/"), 200);
    let fw_wars_arr = fw_wars.as_array().expect("fw wars array");
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_001, "against_id": 500_004 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_002, "against_id": 500_003 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_003, "against_id": 500_002 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_004, "against_id": 500_001 })));

    let cal_militia_corp = ctx.sde.factions[&500_001]
        .militia_corporation_id
        .or(ctx.sde.factions[&500_001].corporation_id)
        .expect("Caldari militia corp");
    let min_militia_corp = ctx.sde.factions[&500_002]
        .militia_corporation_id
        .or(ctx.sde.factions[&500_002].corporation_id)
        .expect("Minmatar militia corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            990_001,
            WarRecord {
                war_id: 990_001,
                aggressor_corp_id: cal_militia_corp,
                defender_corp_id: min_militia_corp,
                mutual: true,
                started_unix: now,
                active: true,
            },
        );
    });
    let fw_wars_updated = call(&router, EsiHttpRequest::new("GET", "/v1/fw/wars/"), 200);
    let fw_wars_upd_arr = fw_wars_updated.as_array().unwrap();
    assert!(fw_wars_upd_arr.contains(&json!({ "faction_id": 500_001, "against_id": 500_002 })));
    assert!(fw_wars_upd_arr.contains(&json!({ "faction_id": 500_002, "against_id": 500_001 })));
}

#[tokio::test]
async fn test_esi_universe_celestials_stations_structures_and_jumps_kills() {
    // parity: esi:GET /universe/stars/{star_id}, esi:GET /universe/stations/{station_id}
    // parity: esi:GET /universe/system_jumps, esi:GET /universe/system_kills
    // parity: feat:esi.universe.celestials, feat:esi.universe.stations-structures
    // parity: feat:esi.universe.system-jumps-kills
    eve_testkit::attest(&[
        "esi:GET /universe/stars/{star_id}",
        "esi:GET /universe/stations/{station_id}",
        "esi:GET /universe/system_jumps",
        "esi:GET /universe/system_kills",
        "feat:esi.universe.celestials",
        "feat:esi.universe.stations-structures",
        "feat:esi.universe.system-jumps-kills",
    ]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

    let char_id = 90_000_001_i64;
    let outsider_id = 90_000_002_i64;
    let corp_id = 98_500_001_i64;
    let outsider_corp_id = 98_500_002_i64;
    let sys_id = 30_000_001_u32;
    let structure_id = 1_099_000_001_i64;
    let capsule_type_id = ctx
        .sde
        .groups
        .values()
        .find(|g| g.name == "Capsule")
        .and_then(|g| {
            ctx.sde
                .types
                .values()
                .find(|t| t.group_id == g.group_id && t.published)
        })
        .map(|t| t.type_id)
        .unwrap_or(670);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        if let Some(ch) = snap.characters.get_mut(&outsider_id) {
            ch.user_id = 1;
            ch.corporation_id = outsider_corp_id;
            ch.solar_system_id = sys_id;
            ch.standings.insert(corp_id, -5.0);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Structure Owner Corp".to_string(),
                ticker: "STOWN".to_string(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            outsider_corp_id,
            CorporationRecord {
                corporation_id: outsider_corp_id,
                name: "Outsider Corp".to_string(),
                ticker: "OUTS".to_string(),
                ceo_id: outsider_id,
                members: HashSet::from([outsider_id]),
                ..Default::default()
            },
        );
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Frontline Citadel".to_string(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.station_office_rental_costs
            .insert(60_003_760, 750_000.0);

        // 1-hour windowed jump events (2 inside [now - 3600, now], 1 expired at now - 7200)
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: sys_id,
            character_id: char_id,
            timestamp_unix: now - 600,
        });
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: sys_id,
            character_id: outsider_id,
            timestamp_unix: now - 1_800,
        });
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: sys_id,
            character_id: char_id,
            timestamp_unix: now - 7_200,
        });

        // 1-hour windowed kill events (1 ship kill + 1 pod kill + 3 NPC kills inside window; 1 expired ship kill + 5 expired NPC kills outside window)
        snap.killmails.insert(
            89_001,
            KillmailRecord {
                kill_id: 89_001,
                victim_character_id: outsider_id,
                attacker_character_id: char_id,
                solar_system_id: sys_id,
                ship_type_id: 587, // Rifter (ship kill inside 1h window)
                total_value_isk: 1_500_000.0,
                ..Default::default()
            },
        );
        snap.killmail_details.insert(
            89_001,
            KillmailDetailRecord {
                kill_id: 89_001,
                killmail_time_unix: now - 300,
                damage_taken: 1_200,
                victim_position: Some([0.0, 0.0, 0.0]),
                attackers: Vec::new(),
            },
        );
        snap.killmails.insert(
            89_002,
            KillmailRecord {
                kill_id: 89_002,
                victim_character_id: outsider_id,
                attacker_character_id: char_id,
                solar_system_id: sys_id,
                ship_type_id: capsule_type_id, // Capsule (pod kill inside 1h window)
                total_value_isk: 10_000.0,
                ..Default::default()
            },
        );
        snap.killmail_details.insert(
            89_002,
            KillmailDetailRecord {
                kill_id: 89_002,
                killmail_time_unix: now - 900,
                damage_taken: 400,
                victim_position: Some([0.0, 0.0, 0.0]),
                attackers: Vec::new(),
            },
        );
        snap.killmails.insert(
            89_003,
            KillmailRecord {
                kill_id: 89_003,
                victim_character_id: outsider_id,
                attacker_character_id: char_id,
                solar_system_id: sys_id,
                ship_type_id: 587, // Expired ship kill outside 1h window
                total_value_isk: 1_500_000.0,
                ..Default::default()
            },
        );
        snap.killmail_details.insert(
            89_003,
            KillmailDetailRecord {
                kill_id: 89_003,
                killmail_time_unix: now - 5_400,
                damage_taken: 1_200,
                victim_position: Some([0.0, 0.0, 0.0]),
                attackers: Vec::new(),
            },
        );
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: sys_id,
            ship_kills: 0,
            pod_kills: 0,
            npc_kills: 3,
            timestamp_unix: now - 1_200,
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: sys_id,
            ship_kills: 0,
            pod_kills: 0,
            npc_kills: 5,
            timestamp_unix: now - 7_200,
        });
    });

    let owner_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-universe.read_structures.v1".to_string()],
    );
    let outsider_token = issue_token_with_scopes(
        &ctx,
        outsider_id,
        vec!["esi-universe.read_structures.v1".to_string()],
    );

    // 1. Universe Stars & Celestials (`/stars/{id}`, `/planets/{id}`, `/moons/{id}`, `/asteroid_belts/{id}`, `/stargates/{id}`)
    let tanoo_star = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/stars/40000001/"),
        200,
    );
    assert_eq!(tanoo_star["name"], json!("Tanoo - Star"));
    assert_eq!(tanoo_star["solar_system_id"], json!(30_000_001));
    assert_eq!(tanoo_star["type_id"], json!(45_041));
    assert_eq!(tanoo_star["radius"], json!(63_350_000));
    assert_eq!(tanoo_star["temperature"], json!(4_567));
    assert_eq!(tanoo_star["spectral_class"], json!("K2 V"));
    assert!(tanoo_star["age"].as_i64().unwrap() > 0);
    assert!(tanoo_star["luminosity"].as_f64().unwrap() > 0.0);

    let jita_star = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/stars/40009076/"),
        200,
    );
    assert_eq!(jita_star["name"], json!("Jita - Star"));
    assert_eq!(jita_star["solar_system_id"], json!(30_000_142));
    assert_eq!(jita_star["type_id"], json!(3_796));
    assert_eq!(jita_star["radius"], json!(513_100_000));
    assert_eq!(jita_star["temperature"], json!(7_305));
    assert_eq!(jita_star["spectral_class"], json!("F1 V"));
    assert_ne!(jita_star["age"], tanoo_star["age"]);
    assert_ne!(jita_star["luminosity"], tanoo_star["luminosity"]);

    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/stars/99999999/"),
        404,
    );

    let planet = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/planets/40000002/"),
        200,
    );
    assert_eq!(planet["planet_id"], json!(40_000_002));
    assert_eq!(planet["name"], json!("Tanoo I"));
    assert_eq!(planet["system_id"], json!(30_000_001));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/planets/99999999/"),
        404,
    );

    let moon = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/moons/40000004/"),
        200,
    );
    assert_eq!(moon["moon_id"], json!(40_000_004));
    assert_eq!(moon["name"], json!("Tanoo I - Moon 1"));
    assert_eq!(moon["system_id"], json!(30_000_001));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/moons/99999999/"),
        404,
    );

    let belt = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/asteroid_belts/40000003/"),
        200,
    );
    assert_eq!(belt["name"], json!("Tanoo I - Asteroid Belt 1"));
    assert_eq!(belt["system_id"], json!(30_000_001));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/asteroid_belts/99999999/"),
        404,
    );

    let gate = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/stargates/50000001/"),
        200,
    );
    assert_eq!(gate["stargate_id"], json!(50_000_001));
    assert!(gate["name"].as_str().unwrap().contains("Stargate"));
    assert_eq!(gate["system_id"], json!(30_000_777));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/stargates/99999999/"),
        404,
    );

    // 2. Universe Stations & Structures (`GET /universe/stations/{id}`, `/universe/structures`, `/universe/structures/{id}`)
    let station = call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/stations/60003760/"),
        200,
    );
    let sde_jita_station = ctx.sde.solar_systems[&30_000_142]
        .stations
        .iter()
        .find(|s| s.station_id == 60_003_760)
        .expect("Jita 4-4 in SDE");
    assert_eq!(station["station_id"], json!(60_003_760));
    assert_eq!(station["system_id"], json!(30_000_142));
    assert_eq!(station["name"], json!(sde_jita_station.name));
    assert_eq!(
        station["owner"],
        json!(sde_jita_station.owner_corporation_id)
    );
    assert_eq!(station["office_rental_cost"], json!(750_000.0));
    let station_other_op = call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/stations/60000004/"),
        200,
    );
    assert_ne!(
        station["services"], station_other_op["services"],
        "stations with different SDE operation_id values must expose distinct service lists"
    );
    assert!(station_other_op["services"]
        .as_array()
        .unwrap()
        .contains(&json!("storage")));
    assert!(!station["services"]
        .as_array()
        .unwrap()
        .contains(&json!("storage")));
    assert!(station["services"]
        .as_array()
        .unwrap()
        .contains(&json!("factory")));
    assert!(!station_other_op["services"]
        .as_array()
        .unwrap()
        .contains(&json!("factory")));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/stations/99999999/"),
        404,
    );

    let structs_list = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/structures/"),
        200,
    );
    assert!(structs_list.as_array().unwrap().contains(&json!(structure_id)));
    let filtered = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/structures/?filter=market"),
        200,
    );
    assert!(filtered.as_array().unwrap().contains(&json!(structure_id)));
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/structures/?filter=bad_filter"),
        400,
    );

    let s_detail = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/"))
            .with_bearer_token(&owner_token),
        200,
    );
    assert_eq!(s_detail["name"], json!("Frontline Citadel"));
    assert_eq!(s_detail["owner_id"], json!(corp_id));
    assert_eq!(s_detail["solar_system_id"], json!(sys_id));
    assert_eq!(s_detail["type_id"], json!(35832));

    // Structure ACL & auth checks (401 without token, 403 for non-ACL outsider, 404 for unknown structure)
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/")),
        401,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/"))
            .with_bearer_token(&outsider_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/structures/99999999/")
            .with_bearer_token(&owner_token),
        404,
    );

    // 3. Universe System Jumps and Kills (`GET /universe/system_jumps`, `GET /universe/system_kills`)
    let jumps = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/universe/system_jumps/"),
        200,
    );
    let sys_jump = jumps
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["system_id"] == json!(sys_id))
        .expect("system jump entry");
    assert_eq!(sys_jump["ship_jumps"], json!(2));

    let kills = call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/system_kills/"),
        200,
    );
    let sys_kill = kills
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["system_id"] == json!(sys_id))
        .expect("system kill entry");
    assert_eq!(sys_kill["ship_kills"], json!(1));
    assert_eq!(sys_kill["pod_kills"], json!(1));
    assert_eq!(sys_kill["npc_kills"], json!(3));
}
