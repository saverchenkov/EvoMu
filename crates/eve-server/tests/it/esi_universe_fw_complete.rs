//! `ESI-3` Meta, Universe (Ancestries/Bloodlines/Races, Factions/Graphics, Celestials,
//! Stations/Structures, System Jumps/Kills), and Faction Warfare acceptance tests against the
//! real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.
//!
//! Ledger tags live inside each `#[tokio::test]` body next to its `eve_testkit::attest` call.

#![forbid(unsafe_code)]

use eve_db::{CorporationRecord, InventoryItemRecord, WarRecord};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

pub(crate) fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce("demo", "demo-secret", character_id, "evomu-esi-fw-complete-test", "https://localhost/callback", scopes, PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-fw-complete-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

pub(crate) fn call(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
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
    // parity: esi:GET /fw/wars
    // parity: feat:esi.faction-warfare.wars
    eve_testkit::attest(&[
        "esi:GET /fw/wars",
        "feat:esi.faction-warfare.wars",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;

    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;

    // Pick a real Gallente-occupied FW warzone system so Caldari militia (`500_001`) can contest & capture it.
    let sys_id = ctx.db.with_mut_snapshot(|snap| {
        if snap.fw_system_occupations.is_empty() {
            snap.backfill_fw_system_occupations();
        }
        let target_sys = snap
            .fw_system_occupations
            .values_mut()
            .filter(|o| o.occupier_faction_id == 500_004)
            .min_by_key(|o| o.solar_system_id)
            .expect("Gallente FW warzone system in SDE");
        target_sys.victory_points = 0;
        target_sys.victory_points_threshold = 200;
        let sid = target_sys.solar_system_id;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.solar_system_id = sid;
            ch.loyalty_points.insert(1_000_035, 99_999); // LP balance must not leak into FW victory points
        }
        if let Some(ch2) = snap.characters.get_mut(&other_char_id) {
            ch2.user_id = 2;
            ch2.solar_system_id = sid;
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
        sid
    });

    // 1. Enlist corp (`char_id` as CEO) in Caldari State (`500_001`) and `other_char_id` in Gallente (`500_004`) via real RPCs.
    let mut sess1 = eve_machonet::SessionState::default();
    sess1.user_id = Some(1);
    sess1.character_id = Some(char_id);
    sess1.corporation_id = Some(corp_id);
    sess1.solar_system_id = Some(sys_id);
    ctx.dispatch_rpc(
        &mut sess1,
        "facWarMgr",
        None,
        "JoinFactionAsCorporation",
        &[eve_proto::PyRep::Integer(500_001)],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("JoinFactionAsCorporation ok");

    let mut sess2 = eve_machonet::SessionState::default();
    sess2.user_id = Some(2);
    sess2.character_id = Some(other_char_id);
    sess2.solar_system_id = Some(sys_id);
    ctx.dispatch_rpc(
        &mut sess2,
        "facWarMgr",
        None,
        "JoinFactionAsCharacter",
        &[eve_proto::PyRep::Integer(500_004)],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .expect("JoinFactionAsCharacter ok");

    // 2. Earn a real FW PvP kill (+1 kill, +20 VP) by destroying `other_char_id`'s ship in `sys_id`.
    let victim_ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 587, // Rifter
                owner_id: other_char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        sid
    });
    let _ = eve_server::combat::death_pipeline::execute_death_pipeline(
        &ctx,
        sys_id,
        other_char_id,
        victim_ship_id,
        char_id,
        glam::DVec3::ZERO,
        42,
    )
    .await
    .expect("FW PvP kill death pipeline");

    // 3. Capture a real FW Novice complex (+100 VP) via `tick_fw_gameplay`.
    let (complex_id, char_ship_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let gid = snap.next_entity_id;
        snap.next_entity_id += 1;
        let shid = snap.next_entity_id;
        snap.items.insert(
            shid,
            InventoryItemRecord {
                item_id: shid,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.fw_runtime.fw_complexes.insert(
            gid,
            (sys_id, "Novice".to_string(), 500_004, 0.0, false),
        );
        snap.fw_runtime
            .complex_occupants
            .insert(gid, vec![(char_id, shid)]);
        snap.fw_runtime.complex_guards_cleared.insert(gid, true);
        snap.fw_runtime
            .complex_last_step_unix
            .insert(gid, now.saturating_sub(900));
        (gid, shid)
    });
    let pocket = eve_server::services::fw_gameplay::pocket_pos_for_complex(complex_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: char_ship_id,
            type_id: 587,
            position: pocket,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, now).await;

    let expected_vp = ctx.db.with_mut_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get_mut(&sys_id).unwrap();
        let vp = occ.victory_points;
        assert!((50..=100).contains(&vp), "expected 50..=100 complex capture VP, got {vp}");
        occ.victory_points_threshold = vp * 2;
        vp
    });
    let expected_threshold = expected_vp * 2;

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
    assert_eq!(char_fw["enlisted_on"], json!(fmt(now)));
    assert_eq!(
        char_fw["kills"],
        json!({ "yesterday": 1, "last_week": 1, "total": 1 })
    );
    assert_eq!(
        char_fw["victory_points"],
        json!({ "yesterday": expected_vp, "last_week": expected_vp, "total": expected_vp })
    );

    // Scope & identity enforcement (401 without token, 403 without scope, 403 for other character)
    call(&router, EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/")), 401);
    call(&router, EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/")).with_bearer_token(&no_scope_token), 403);
    call(&router, EsiHttpRequest::new("GET", format!("/v1/characters/{other_char_id}/fw/stats/")).with_bearer_token(&char_token), 403);

    // 2. Corporation FW Stats (`GET /corporations/{corporation_id}/fw/stats`)
    let corp_fw = call(&router, EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/")).with_bearer_token(&char_token), 200);
    assert_eq!(corp_fw["faction_id"], json!(500_001));
    assert_eq!(corp_fw["pilots"], json!(1));
    assert_eq!(corp_fw["enlisted_on"], json!(fmt(now)));
    assert_eq!(corp_fw["kills"], json!({ "yesterday": 1, "last_week": 1, "total": 1 }));
    assert_eq!(corp_fw["victory_points"], json!({ "yesterday": expected_vp, "last_week": expected_vp, "total": expected_vp }));

    call(&router, EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/")), 401);
    call(&router, EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/fw/stats/")).with_bearer_token(&no_scope_token), 403);
    call(&router, EsiHttpRequest::new("GET", "/v1/corporations/99999999/fw/stats/").with_bearer_token(&char_token), 403);

    // 3. FW Leaderboards (`GET /fw/leaderboards`, `/characters`, `/corporations`)
    let lb = call(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"), 200);
    for bucket in ["yesterday", "last_week", "active_total"] {
        let k_arr = lb["kills"][bucket].as_array().unwrap();
        assert_eq!(k_arr.len(), 4);
        assert_eq!(
            k_arr.iter().find(|e| e["faction_id"] == json!(500_001)).unwrap()["amount"],
            json!(1)
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
    for bucket in ["yesterday", "last_week", "active_total"] {
        assert_eq!(
            lb_char["kills"][bucket]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["character_id"] == json!(char_id))
                .unwrap()["amount"],
            json!(1)
        );
        assert_eq!(
            lb_char["victory_points"][bucket]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["character_id"] == json!(char_id))
                .unwrap()["amount"],
            json!(expected_vp)
        );
    }

    let lb_corp = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"),
        200,
    );
    for bucket in ["yesterday", "last_week", "active_total"] {
        assert_eq!(
            lb_corp["victory_points"][bucket]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["corporation_id"] == json!(corp_id))
                .unwrap()["amount"],
            json!(expected_vp)
        );
    }

    // 4. FW Stats (`GET /fw/stats`) — verify populated faction and unpopulated faction (0 pilots)
    let fw_stats = call(&router, EsiHttpRequest::new("GET", "/v1/fw/stats/"), 200);
    let fac_stats_arr = fw_stats.as_array().expect("fw stats array");
    assert_eq!(fac_stats_arr.len(), 4);
    let caldari_fw = fac_stats_arr
        .iter()
        .find(|s| s["faction_id"] == json!(500_001))
        .expect("Caldari FW stats");
    assert_eq!(caldari_fw["pilots"], json!(1));
    assert_eq!(
        caldari_fw["kills"],
        json!({ "yesterday": 1, "last_week": 1, "total": 1 })
    );
    assert_eq!(
        caldari_fw["victory_points"],
        json!({ "yesterday": expected_vp, "last_week": expected_vp, "total": expected_vp })
    );
    let minmatar_fw = fac_stats_arr
        .iter()
        .find(|s| s["faction_id"] == json!(500_002))
        .expect("Minmatar FW stats");
    let (caldari_sys, minmatar_sys, gallente_sys) = ctx.db.with_snapshot(|s| {
        let count_for = |fid: i64| {
            s.fw_system_occupations
                .values()
                .filter(|fw| fw.occupier_faction_id == fid)
                .count()
        };
        (count_for(500_001), count_for(500_002), count_for(500_004))
    });
    assert!(gallente_sys >= 1, "Gallente must control at least the contested FW system");
    assert_eq!(caldari_fw["systems_controlled"], json!(caldari_sys));
    assert_eq!(minmatar_fw["systems_controlled"], json!(minmatar_sys));
    let gallente_fw = fac_stats_arr
        .iter()
        .find(|s| s["faction_id"] == json!(500_004))
        .expect("Gallente FW stats");
    assert_eq!(gallente_fw["systems_controlled"], json!(gallente_sys));

    // 5. FW Systems (`GET /fw/systems`) + live LP donation & second PvP kill via RPCs
    let fw_systems = call(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_arr = fw_systems.as_array().expect("fw systems array");
    let sys_entry = sys_arr
        .iter()
        .find(|s| s["solar_system_id"] == json!(sys_id))
        .expect("contested system in /fw/systems");
    assert_eq!(sys_entry["occupier_faction_id"], json!(500_004));
    assert_eq!(sys_entry["contested"], json!("contested"));
    assert_eq!(sys_entry["victory_points"], json!(expected_vp));
    assert_eq!(sys_entry["victory_points_threshold"], json!(expected_threshold));

    // Score a second FW PvP kill via `execute_death_pipeline` (+20 VP -> 90/90 -> "vulnerable").
    let victim_ship_id_2 = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 587,
                owner_id: other_char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        sid
    });
    let _ = eve_server::combat::death_pipeline::execute_death_pipeline(
        &ctx, sys_id, other_char_id, victim_ship_id_2, char_id, glam::DVec3::ZERO, 43,
    )
    .await
    .expect("second FW PvP kill");
    let complex_id_2 = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let gid = snap.next_entity_id;
        snap.fw_runtime.fw_complexes.insert(gid, (sys_id, "Novice".to_string(), 500_004, 0.0, false));
        snap.fw_runtime.complex_occupants.insert(gid, vec![(char_id, char_ship_id)]);
        snap.fw_runtime.complex_guards_cleared.insert(gid, true);
        snap.fw_runtime.complex_last_step_unix.insert(gid, now.saturating_sub(900));
        gid
    });
    let pocket_2 = eve_server::services::fw_gameplay::pocket_pos_for_complex(complex_id_2);
    let _ = sys_handle.remove_ball(char_ship_id).await;
    let _ = sys_handle.spawn_ball(eve_destiny::Ball {
        id: char_ship_id, type_id: 587, position: pocket_2, velocity: glam::DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await;
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, now + 60).await;

    let fw_systems_after = call(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_after = fw_systems_after
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["solar_system_id"] == json!(sys_id))
        .unwrap();
    assert_eq!(sys_after["contested"], json!("vulnerable"));
    assert_eq!(sys_after["victory_points"], json!(expected_threshold));
    assert_eq!(sys_after["victory_points_threshold"], json!(expected_threshold));

    let char_fw_after = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/fw/stats/"))
            .with_bearer_token(&char_token),
        200,
    );
    assert_eq!(char_fw_after["kills"]["total"], json!(2));
    assert_eq!(char_fw_after["victory_points"]["total"], json!(expected_threshold));

    // 6. FW Wars (`GET /fw/wars`) + live inter-militia war mutation
    let fw_wars = call(&router, EsiHttpRequest::new("GET", "/v1/fw/wars/"), 200);
    let fw_wars_arr = fw_wars.as_array().expect("fw wars array");
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_001, "against_id": 500_004 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_002, "against_id": 500_003 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_003, "against_id": 500_002 })));
    assert!(fw_wars_arr.contains(&json!({ "faction_id": 500_004, "against_id": 500_001 })));

    let cal_militia_corp = ctx.sde.factions[&500_001].militia_corporation_id.or(ctx.sde.factions[&500_001].corporation_id).expect("Caldari militia corp");
    let min_militia_corp = ctx.sde.factions[&500_002].militia_corporation_id.or(ctx.sde.factions[&500_002].corporation_id).expect("Minmatar militia corp");
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(990_001, WarRecord {
            war_id: 990_001, aggressor_corp_id: cal_militia_corp, defender_corp_id: min_militia_corp,
            mutual: true, started_unix: now, active: true,
        });
    });
    let fw_wars_updated = call(&router, EsiHttpRequest::new("GET", "/v1/fw/wars/"), 200);
    let fw_wars_upd_arr = fw_wars_updated.as_array().unwrap();
    assert!(fw_wars_upd_arr.contains(&json!({ "faction_id": 500_001, "against_id": 500_002 })));
    assert!(fw_wars_upd_arr.contains(&json!({ "faction_id": 500_002, "against_id": 500_001 })));
}



#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_universe_fw_ext_2 {
    use super::*;

// Part 2 of `ESI-3` Universe Celestials, Stations/Structures, and System Jumps/Kills acceptance tests
// against the real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.


use crate::esi_universe_fw_complete::{call, issue_token_with_scopes};
use eve_db::{
    CorporationRecord, KillmailDetailRecord, KillmailRecord, SystemJumpEvent, SystemKillEvent,
    UpwellStructureRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

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
    crate::common::pin_clock(&ctx);
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
        .and_then(|g| ctx.sde.types.values().find(|t| t.group_id == g.group_id && t.published))
        .map(|t| t.type_id)
        .unwrap_or(670);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
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
        for (cid, name, ticker, ceo) in [
            (corp_id, "Structure Owner Corp", "STOWN", char_id),
            (outsider_corp_id, "Outsider Corp", "OUTS", outsider_id),
        ] {
            snap.corporations.insert(
                cid,
                CorporationRecord {
                    corporation_id: cid,
                    name: name.to_string(),
                    ticker: ticker.to_string(),
                    ceo_id: ceo,
                    members: HashSet::from([ceo]),
                    ..Default::default()
                },
            );
        }
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
                self_destruct_at_unix: None,
            },
        );
        snap.station_office_rental_costs.insert(60_003_760, 750_000.0);

        for (cid, ts) in [
            (char_id, now - 600),
            (outsider_id, now - 1_800),
            (char_id, now - 7_200),
        ] {
            snap.system_jump_events.push(SystemJumpEvent {
                solar_system_id: sys_id,
                character_id: cid,
                timestamp_unix: ts,
            });
        }

        for (kid, stid, val, ts, dmg) in [
            (89_001, 587, 1_500_000.0, now - 300, 1_200),
            (89_002, capsule_type_id, 10_000.0, now - 900, 400),
            (89_003, 587, 1_500_000.0, now - 5_400, 1_200),
        ] {
            snap.killmails.insert(
                kid,
                KillmailRecord {
                    kill_id: kid,
                    victim_character_id: outsider_id,
                    attacker_character_id: char_id,
                    solar_system_id: sys_id,
                    ship_type_id: stid,
                    total_value_isk: val,
                    ..Default::default()
                },
            );
            snap.killmail_details.insert(
                kid,
                KillmailDetailRecord {
                    kill_id: kid,
                    killmail_time_unix: ts,
                    damage_taken: dmg,
                    victim_position: Some([0.0, 0.0, 0.0]),
                    attackers: Vec::new(),
                },
            );
        }
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

    let owner_token = issue_token_with_scopes(&ctx, char_id, vec!["esi-universe.read_structures.v1".to_string()]);
    let outsider_token = issue_token_with_scopes(&ctx, outsider_id, vec!["esi-universe.read_structures.v1".to_string()]);

    // 1. Universe Stars & Celestials (`/stars/{id}`, `/planets/{id}`, `/moons/{id}`, `/asteroid_belts/{id}`, `/stargates/{id}`)
    let tanoo_star = call(&router, EsiHttpRequest::new("GET", "/v1/universe/stars/40000001/"), 200);
    assert_eq!(tanoo_star["name"], json!("Tanoo - Star"));
    assert_eq!(tanoo_star["solar_system_id"], json!(30_000_001));
    assert_eq!(tanoo_star["type_id"], json!(45_041));
    assert_eq!(tanoo_star["radius"], json!(63_350_000));
    assert_eq!(tanoo_star["temperature"], json!(4_567));
    assert_eq!(tanoo_star["spectral_class"], json!("K2 V"));
    assert!(tanoo_star["age"].as_i64().unwrap() > 0);
    assert!(tanoo_star["luminosity"].as_f64().unwrap() > 0.0);

    let jita_star = call(&router, EsiHttpRequest::new("GET", "/v1/universe/stars/40009076/"), 200);
    assert_eq!(jita_star["name"], json!("Jita - Star"));
    assert_eq!(jita_star["solar_system_id"], json!(30_000_142));
    assert_eq!(jita_star["type_id"], json!(3_796));
    assert_eq!(jita_star["radius"], json!(513_100_000));
    assert_eq!(jita_star["temperature"], json!(7_305));
    assert_eq!(jita_star["spectral_class"], json!("F1 V"));
    assert_ne!(jita_star["age"], tanoo_star["age"]);
    assert_ne!(jita_star["luminosity"], tanoo_star["luminosity"]);
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/stars/99999999/"), 404);

    let planet = call(&router, EsiHttpRequest::new("GET", "/v1/universe/planets/40000002/"), 200);
    assert_eq!(planet["planet_id"], json!(40_000_002));
    assert_eq!(planet["name"], json!("Tanoo I"));
    assert_eq!(planet["system_id"], json!(30_000_001));
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/planets/99999999/"), 404);

    let moon = call(&router, EsiHttpRequest::new("GET", "/v1/universe/moons/40000004/"), 200);
    assert_eq!(moon["moon_id"], json!(40_000_004));
    assert_eq!(moon["name"], json!("Tanoo I - Moon 1"));
    assert_eq!(moon["system_id"], json!(30_000_001));
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/moons/99999999/"), 404);

    let belt = call(&router, EsiHttpRequest::new("GET", "/v1/universe/asteroid_belts/40000003/"), 200);
    assert_eq!(belt["name"], json!("Tanoo I - Asteroid Belt 1"));
    assert_eq!(belt["system_id"], json!(30_000_001));
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/asteroid_belts/99999999/"), 404);

    let gate = call(&router, EsiHttpRequest::new("GET", "/v1/universe/stargates/50000001/"), 200);
    assert_eq!(gate["stargate_id"], json!(50_000_001));
    assert!(gate["name"].as_str().unwrap().contains("Stargate"));
    assert_eq!(gate["system_id"], json!(30_000_777));
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/stargates/99999999/"), 404);

    // 2. Universe Stations & Structures (`GET /universe/stations/{id}`, `/universe/structures`, `/universe/structures/{id}`)
    let station = call(&router, EsiHttpRequest::new("GET", "/v2/universe/stations/60003760/"), 200);
    let sde_jita_station = ctx.sde.solar_systems[&30_000_142]
        .stations
        .iter()
        .find(|s| s.station_id == 60_003_760)
        .expect("Jita 4-4 in SDE");
    assert_eq!(station["station_id"], json!(60_003_760));
    assert_eq!(station["system_id"], json!(30_000_142));
    assert_eq!(station["name"], json!(sde_jita_station.name));
    assert_eq!(station["owner"], json!(sde_jita_station.owner_corporation_id));
    assert_eq!(station["office_rental_cost"], json!(750_000.0));
    let station_other_op = call(&router, EsiHttpRequest::new("GET", "/v2/universe/stations/60000004/"), 200);
    assert_ne!(
        station["services"], station_other_op["services"],
        "stations with different SDE operation_id values must expose distinct service lists"
    );
    assert!(station_other_op["services"].as_array().unwrap().contains(&json!("storage")));
    assert!(!station["services"].as_array().unwrap().contains(&json!("storage")));
    assert!(station["services"].as_array().unwrap().contains(&json!("factory")));
    assert!(!station_other_op["services"].as_array().unwrap().contains(&json!("factory")));
    call(&router, EsiHttpRequest::new("GET", "/v2/universe/stations/99999999/"), 404);

    let structs_list = call(&router, EsiHttpRequest::new("GET", "/v1/universe/structures/"), 200);
    assert!(structs_list.as_array().unwrap().contains(&json!(structure_id)));
    let filtered = call(&router, EsiHttpRequest::new("GET", "/v1/universe/structures/?filter=market"), 200);
    assert!(filtered.as_array().unwrap().contains(&json!(structure_id)));
    call(&router, EsiHttpRequest::new("GET", "/v1/universe/structures/?filter=bad_filter"), 400);

    let s_detail = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/")).with_bearer_token(&owner_token),
        200,
    );
    assert_eq!(s_detail["name"], json!("Frontline Citadel"));
    assert_eq!(s_detail["owner_id"], json!(corp_id));
    assert_eq!(s_detail["solar_system_id"], json!(sys_id));
    assert_eq!(s_detail["type_id"], json!(35832));

    // Structure ACL & auth checks (401 without token, 403 for non-ACL outsider, 404 for unknown structure)
    call(&router, EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/")), 401);
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v2/universe/structures/{structure_id}/")).with_bearer_token(&outsider_token),
        403,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v2/universe/structures/99999999/").with_bearer_token(&owner_token),
        404,
    );

    // 3. Universe System Jumps and Kills (`GET /universe/system_jumps`, `GET /universe/system_kills`)
    let jumps = call(&router, EsiHttpRequest::new("GET", "/v1/universe/system_jumps/"), 200);
    let sys_jump = jumps.as_array().unwrap().iter().find(|j| j["system_id"] == json!(sys_id)).expect("system jump entry");
    assert_eq!(sys_jump["ship_jumps"], json!(2));

    let kills = call(&router, EsiHttpRequest::new("GET", "/v2/universe/system_kills/"), 200);
    let sys_kill = kills.as_array().unwrap().iter().find(|k| k["system_id"] == json!(sys_id)).expect("system kill entry");
    assert_eq!(sys_kill["ship_kills"], json!(1));
    assert_eq!(sys_kill["pod_kills"], json!(1));
    assert_eq!(sys_kill["npc_kills"], json!(3));
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_fw_and_social_ext {
    use super::*;

// Closure tests for Crimewatch Kill Rights, Security Status, and Fleet Boss Handover /
// Stored Formations & Warp Shapes / Target Tagging (`feat:crimewatch.*`, `feat:fleet.*`).


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, String> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .map_err(|e| format!("{e:?}"))
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or(PyRep::None)
}

fn spawn_ship(
    ctx: &ServerContext,
    owner_id: i64,
    sys_id: u32,
    type_id: u32,
    _pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if type_id != 670 {
            snap.next_entity_id += 1;
            let gun_id = snap.next_entity_id;
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 487, // 250mm Light Artillery Cannon I
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
            snap.next_entity_id += 1;
            let ammo_id = snap.next_entity_id;
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 183, // Fusion S
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 100,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_crimewatch_kill_rights_gain_and_sell_closure() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32; // Jita (0.95)
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_510);
    let victim_id = 90_000_001_i64;
    let criminal_id = 90_000_002_i64;
    let buyer_id = 90_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&buyer_id) {
            if let Some(mut c3) = snap.characters.get(&victim_id).cloned() {
                c3.character_id = buyer_id;
                c3.name = "Buyer Pilot".to_string();
                snap.characters.insert(buyer_id, c3);
            }
        }
        for (cid, uid, bal) in [
            (victim_id, 1, 10_000_000.0),
            (criminal_id, 2, 10_000_000.0),
            (buyer_id, 3, 20_000_000.0),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.user_id = uid;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
            }
        }
    });

    let victim_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::ZERO);
    let crim_ship = spawn_ship(&ctx, criminal_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let _buyer_ship = spawn_ship(&ctx, buyer_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));

    let sys_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [(victim_ship, DVec3::ZERO), (crim_ship, DVec3::new(1_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = sys_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    // 1. Low-sec unprovoked ship aggression grants Suspect flag, NOT a kill right.
    let mut cw = CrimeWatchDirector::new();
    let low_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        lowsec_sys,
        0.4,
        false,
        false,
        now,
    );
    assert!(matches!(low_eval, AggressionEvaluation::SuspectFlagTriggered { .. }));
    assert!(ctx.db.with_snapshot(|s| s.kill_rights.is_empty()));

    // 1b. Low-sec criminal aggression against a capsule (pod) grants a 30-day private kill right.
    let low_pod_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        lowsec_sys,
        0.4,
        false,
        true,
        now,
    );
    assert!(matches!(low_pod_eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, criminal_id, lowsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim_id, lowsec_sys, 670, DVec3::ZERO);
    for (id, tid, pos) in [(low_crim_ship, 587_u32, DVec3::new(1_000.0, 0.0, 0.0)), (low_vic_pod, 670_u32, DVec3::ZERO)] {
        let mut cs = ctx.build_type_combat_state(tid);
        cs.shield_hp = 500.0;
        let _ = lowsec_handle.spawn_ball_with_combat(Ball {
            id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&victim_id) {
            ch.active_ship_id = low_vic_pod;
        }
    });
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, low_vic_pod, true,
    ).expect("evaluate_and_record_pvp_aggression lowsec pod");
    assert!(
        ctx.db.with_snapshot(|s| s.kill_rights.values().any(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id)),
        "evaluate_and_record_pvp_aggression on low-sec pod must grant kill right"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.clear();
        s.crimewatch_aggressions.clear();
    });
    let sec_before_cmd = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let mut sess_crim_low = SessionState::default();
    sess_crim_low.user_id = Some(2);
    sess_crim_low.character_id = Some(criminal_id);
    sess_crim_low.solar_system_id = Some(lowsec_sys);
    sess_crim_low.ship_id = Some(low_crim_ship);
    for i in 0..2 {
        if i > 0 {
            for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
        }
        let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(low_vic_pod)])
            .await
            .expect("Activate lowsec pod");
    }
    let (sec_after_cmd, kr_count_cmd) = ctx.db.with_snapshot(|s| {
        let krs: Vec<_> = s.kill_rights.values().filter(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id).collect();
        (s.characters[&criminal_id].security_status, krs.len())
    });
    assert!(sec_after_cmd < sec_before_cmd, "Activate low-sec pod must deduct sec status");
    assert_eq!(kr_count_cmd, 1, "two Activate cycles must create only 1 kill right");
    // Verify second cycle did not re-deduct security status
    let sec_before_cycle2 = sec_after_cmd;
    for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
    let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(low_vic_pod)]).await.expect("Activate cycle 3");
    assert!((ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status) - sec_before_cycle2).abs() < 1e-9);

    // 1c. Main weapon path (dogmaIM.Activate) ship-then-pod in low-sec grants pod penalty + kill right
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let mut cs_ship = ctx.build_type_combat_state(587);
    cs_ship.shield_hp = 500.0;
    let _ = lowsec_handle.spawn_ball_with_combat(Ball {
        id: low_vic_ship, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cs_ship).await;
    let gun_id = ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.clear();
        s.crimewatch_aggressions.clear();
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_ship;
        s.items.values().find(|i| i.location_id == low_crim_ship && i.type_id == 487).unwrap().item_id
    });
    let _ = lowsec_handle.add_target_lock(low_crim_ship, eve_destiny::TargetLock {
        target_id: low_vic_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;
    let _ = lowsec_handle.add_target_lock(low_crim_ship, eve_destiny::TargetLock {
        target_id: low_vic_pod, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;
    let sec_pre_ship = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(low_vic_ship)]).await.expect("Activate ship");
    let (sec_post_ship, kr_after_ship) = ctx.db.with_snapshot(|s| (s.characters[&criminal_id].security_status, s.kill_rights.len()));
    assert!(sec_post_ship < sec_pre_ship && kr_after_ship == 0, "low-sec ship hit deducts sec without kill right");
    for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
    let _ = rpc(
        &ctx,
        &mut sess_crim_low,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(gun_id)],
    )
    .await;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_pod);
    let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(low_vic_pod)]).await.expect("Activate pod");
    let (sec_post_pod, kr_after_pod, dogma_kr_id) = ctx.db.with_snapshot(|s| {
        let krs: Vec<_> = s.kill_rights.values().filter(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id).collect();
        (s.characters[&criminal_id].security_status, krs.len(), krs[0].kill_right_id)
    });
    assert!(sec_post_pod < sec_post_ship && kr_after_pod == 1, "low-sec pod hit after ship hit must deduct pod sec and grant 1 kill right");
    let low_pod_exp = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&dogma_kr_id).copied())
        .expect("30-day expiry recorded for low-sec pod kill right");
    assert_eq!(low_pod_exp, now + 30 * 86_400);
    // Clean up low-sec pod kill right and return criminal_id to highsec_sys before testing high-sec CONCORD kill right
    ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.remove(&dogma_kr_id);
        if let Some(ch) = s.characters.get_mut(&criminal_id) {
            ch.solar_system_id = highsec_sys;
        }
    });

    // 2. High-sec criminal aggression triggers CONCORD arrival and grants a 30-day private kill right.
    let hi_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        highsec_sys,
        0.95,
        false,
        false,
        now,
    );
    assert!(matches!(hi_eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    let reports = cw.step_concord(&ctx, now + 10, 42).await;
    assert_eq!(reports.len(), 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&criminal_id) {
            ch.station_id = None;
            ch.solar_system_id = highsec_sys;
        }
    });
    let kr_id = reports[0].kill_right_id;
    assert!(kr_id > 0);

    let (kr_fee, kr_owner, kr_target) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.get(&kr_id).expect("kill right created by CONCORD");
        (kr.activation_fee.0, kr.owner_id, kr.target_id)
    });
    assert_eq!(kr_owner, victim_id);
    assert_eq!(kr_target, criminal_id);
    assert_eq!(kr_fee, 0, "newly earned kill right starts private with 0 fee");
    let exp_unix = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&kr_id).copied())
        .expect("30-day expiry recorded");
    assert_eq!(exp_unix, now + 30 * 86_400);

    // 3. SellKillRight -> CancelSellKillRight -> SellKillRight -> BuyKillRight (15-min suspect + ISK transfer)
    let mut sess_victim = SessionState::default();
    sess_victim.user_id = Some(1);
    sess_victim.character_id = Some(victim_id);
    sess_victim.solar_system_id = Some(highsec_sys);

    let listed = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight");
    if let PyRep::List(items) = listed {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_500_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    let cancelled = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("CancelSellKillRight");
    assert_eq!(field(&cancelled, "forSale"), PyRep::Bool(false));

    // Re-list for 2,500,000 ISK and have third-party pilot buy it via killRightMgr.BuyKillRight
    let _ = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight again");

    let mut sess_buyer = SessionState::default();
    sess_buyer.user_id = Some(3);
    sess_buyer.character_id = Some(buyer_id);
    sess_buyer.solar_system_id = Some(highsec_sys);

    let bought = rpc(
        &ctx,
        &mut sess_buyer,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("BuyKillRight");
    assert_eq!(field(&bought, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&bought, "feePaidISK"), PyRep::Real(2_500_000.0));

    let (victim_bal, buyer_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&victim_id).unwrap().balance.to_wire_f64(),
            s.characters.get(&buyer_id).unwrap().balance.to_wire_f64(),
        )
    });
    assert!((victim_bal - 12_500_000.0).abs() < 1e-2);
    assert!((buyer_bal - 17_500_000.0).abs() < 1e-2);

    let suspect_until = ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&criminal_id)
        .map(|s| s.suspect_flag_until_unix)
        .unwrap_or(0);
    assert_eq!(suspect_until, now + 900, "BuyKillRight sets 15-min suspect flag");
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_caps_tags_and_transactions_closure() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let char_id = 90_000_001_i64;
    let victim_id = 90_000_002_i64;
    // Find a low-sec CONCORD/DED station with Security Office service in SDE for Clone Soldier Tag exchange
    let security_office_svc_id = ctx
        .sde
        .station_services
        .values()
        .find(|s| s.name == "Security Office")
        .map(|s| s.service_id)
        .unwrap_or(26);
    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let is_concord = matches!(st.owner_corporation_id, 1_000_125 | 1_000_137)
                || st.owner_faction_id == Some(500_006);
            let is_lowsec = ctx
                .sde
                .solar_systems
                .get(&st.solar_system_id)
                .is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45);
            let has_office = ctx
                .sde
                .station_operations
                .get(&st.operation_id)
                .is_some_and(|op| op.services.contains(&security_office_svc_id));
            is_concord && is_lowsec && has_office
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec CONCORD Security Office station in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.security_status = 0.0;
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
        }
        if let Some(vic) = snap.characters.get_mut(&victim_id) {
            vic.user_id = 2;
            vic.security_status = 5.0;
        }
        for &tag_type in &[33138_u32, 33139, 33140, 33141] {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tag_type,
                    owner_id: char_id,
                    location_id: lowsec_station_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Verify EVE security status loss formula and live character security_status update on illegal high-sec aggression
    let loss_ship = compute_sec_status_loss(0.0, 5.0, false, 0.95);
    let expected_ship = (10.0 - 0.0) * 0.025 * 0.95 * (1.0 + (5.0 - 0.0) / 100.0);
    assert!((loss_ship - expected_ship).abs() < 1e-6);

    let loss_pod = compute_sec_status_loss(-2.0, 5.0, true, 0.95);
    let expected_pod = (10.0 - (-2.0)) * 0.25 * 0.95 * (1.0 + (5.0 - (-2.0)) / 100.0);
    assert!((loss_pod - expected_pod).abs() < 1e-6);

    let jita_sec = ctx.sde.solar_systems[&30_000_142].security_status;
    let expected_jita_loss = compute_sec_status_loss(0.0, 5.0, false, jita_sec);
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, char_id, 1, 30_000_142, victim_id, false,
    )
    .expect("evaluate_and_record_pvp_aggression");
    // Repeated cycle on same target must not re-deduct security status; CONCORD arrival must not double-deduct
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, char_id, 1, 30_000_142, victim_id, false,
    )
    .expect("evaluate_and_record_pvp_aggression repeat");
    let mut cw_concord = CrimeWatchDirector::new();
    cw_concord.states = ctx.crimewatch.read().unwrap().states.clone();
    let _ = cw_concord.step_concord(&ctx, now + 30, 99).await;
    let sec_after_aggr = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_after_aggr - (-expected_jita_loss)).abs() < 1e-6, "live character security_status must drop once by exact ship loss formula");

    // 1b. Verify dogmaIM.Activate high-sec path uses compute_sec_status_loss with real victim sec status (not flat -0.5)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
            ch.solar_system_id = 30_000_142;
            ch.station_id = None;
        }
    });
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let att_ship = spawn_ship(&ctx, char_id, 30_000_142, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let vic_ship = spawn_ship(&ctx, victim_id, 30_000_142, 587, DVec3::ZERO);
    for (id, pos) in [(att_ship, DVec3::new(1_000.0, 0.0, 0.0)), (vic_ship, DVec3::ZERO)] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }
    let mut sess_cmd = SessionState::default();
    sess_cmd.user_id = Some(1);
    sess_cmd.character_id = Some(char_id);
    sess_cmd.solar_system_id = Some(30_000_142);
    sess_cmd.ship_id = Some(att_ship);
    let _ = rpc(
        &ctx,
        &mut sess_cmd,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vic_ship)],
    )
    .await
    .expect("Activate highsec");
    let sec_after_cmd = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(
        (sec_after_cmd - (-expected_jita_loss)).abs() < 1e-6,
        "dogmaIM.Activate high-sec must use compute_sec_status_loss ({}) instead of flat -0.5 (got {})",
        -expected_jita_loss,
        sec_after_cmd
    );

    // 2. Verify NPC kill security status gain caps at +5.0 (`feat:crimewatch.sec-status-gain-caps`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 4.99;
        }
    });
    let mut npc_dir = NpcSpawnManager::new();
    let make_npc = |ball_id: i64| NpcCombatEntity {
        ball_id,
        type_id: 12000,
        faction_id: 500_010,
        state: NpcAiState::Idle,
        spawn_group_id: 1,
        spawn_pos: DVec3::ZERO,
        target_id: None,
        bounty_isk: 10_000_000.0,
        optimal_range: 10_000.0,
        orbit_range: 5_000.0,
        base_dps: 50.0,
        damage_vector: DamageVector { em: 10.0, thermal: 10.0, kinetic: 15.0, explosive: 15.0 },
        last_attack_tick: 0,
        has_shield_rep: false,
        has_armor_rep: false,
    };
    npc_dir.entities.insert(80_000_001, make_npc(80_000_001));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_001, char_id, now).expect("handle_npc_destroyed");
    let sec_after_rat = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!(sec_after_rat > 4.99 && sec_after_rat <= 5.0, "NPC kill sec gain must stop at +5.0");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });
    npc_dir.entities.insert(80_000_002, make_npc(80_000_002));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_002, char_id, now).expect("handle_npc_destroyed at cap");
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!((sec_at_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must not exceed +5.0");

    // Pilot starting above +5.0 (e.g. +6.0) must not be clamped down to 5.0 on NPC kill
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 6.0;
        }
    });
    npc_dir.entities.insert(80_000_003, make_npc(80_000_003));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_003, char_id, now).expect("handle_npc_destroyed above +5.0");
    let sec_above_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!((sec_above_cap - 6.0).abs() < 1e-6, "NPC kill must not lower security status above +5.0 (got {sec_above_cap})");

    // 3. Verify all 4 Clone Soldier Tag bands, high-sec station rejection, ISK fee, and out-of-band refusal (`feat:crimewatch.sec-tag-tier-bands`)
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760); // Jita 4-4 (high-sec non-CONCORD station)
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = -9.0;
        }
    });
    let highsec_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
    )
    .await;
    assert!(
        highsec_err.unwrap_err().contains("SecurityOfficeRequired"),
        "High-sec non-CONCORD station must reject tag turn-in"
    );

    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);
    let bal_before_tags = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_sys_id;
        ch.balance
    });

    for (start_sec, valid_tag, invalid_tag, req_qty, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, 1_i64, -8.5_f64, -8.5_f64), // Trainer: -10 to -8
        (-6.5_f64, 33139_i64, 33140_i64, 1_i64, -6.0_f64, -6.0_f64), // Recruiter: -8 to -5
        (-3.5_f64, 33140_i64, 33141_i64, 1_i64, -3.0_f64, -3.0_f64), // Transporter: -5 to -2
        (-0.2_f64, 33141_i64, 33138_i64, 5_i64, 0.0_f64, 0.0_f64),   // Negotiator: -2 to 0 (capped at 0.0)
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.security_status = start_sec;
            }
        });
        let bad = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await;
        assert!(bad.unwrap_err().contains("SecurityTagBandMismatch"));

        let ok = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(valid_tag), PyRep::Integer(req_qty))])],
        )
        .await
        .expect("ApplyTagsForSecStatus");
        let new_sec = match field(&ok, "newSecurityStatus") {
            PyRep::Real(v) => v,
            other => panic!("expected Real newSecurityStatus, got {other:?}"),
        };
        assert!((new_sec - expected_sec).abs() < 1e-6);
    }
    // Reject i64->i32 overflow / negative tag quantities and negative tag type IDs (`R-20261101-P0-01`)
    for bad_qty in [4_294_966_296_i64, 2_147_483_648_i64, 0_i64, -5_i64] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.security_status = -0.5;
            }
        });
        let err = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(bad_qty))])],
        )
        .await;
        assert!(err.unwrap_err().contains("InvalidQuantity"));
    }
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(-1), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err()
        .contains("InvalidSecurityTagType")
    );

    // Tag at 0.0 is refused (cannot raise above 0.0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
    });
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err()
        .contains("InvalidTargetSecurityStatus")
    );

    // Verify derived placeholder ISK fee charged for 4 effective tags (1M + 2M + 5M + 10M = 18M ISK, 4 Negotiator tags preserved, journal logged)
    let (bal_after_tags, rem_neg_tags, has_tag_journal) = ctx.db.with_snapshot(|s| {
        let rem = s.items.values().filter(|i| i.owner_id == char_id && i.type_id == 33141).map(|i| i.quantity).sum::<i32>();
        let j = s.wallet_journal.iter().any(|w| w.character_id == char_id && w.entry_type == "SecurityTagProcessing" && w.amount == IskCents::from_isk(-10_000_000));
        (s.characters[&char_id].balance, rem, j)
    });
    assert_eq!(bal_before_tags.0 - bal_after_tags.0, IskCents::from_isk(18_000_000).0);
    assert_eq!(rem_neg_tags, 4, "excess tags above 0.0 cap must not be consumed");
    assert!(has_tag_journal, "SecurityTagFee wallet journal entry must be recorded");

    // 4. Verify GetSecurityStatusTransactions returns V24 client columns by name
    let txs = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = txs else {
        panic!("expected Rowset, got {txs:?}");
    };
    assert_eq!(rows.len(), 6, "2 illegal PvP aggressions (no CONCORD duplicate) + 4 tag turn-ins recorded");
    let row0: std::collections::HashMap<&str, &PyRep> = columns
        .iter()
        .map(|s| s.as_str())
        .zip(rows[2].iter())
        .collect();
    let expected_ft = (now as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(row0.get("eventDate"), Some(&&PyRep::Integer(expected_ft)));
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(385)));
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(lowsec_sys_id as i64)));
    assert_eq!(row0.get("modification"), Some(&&PyRep::Real(0.5)));
    assert_eq!(row0.get("newValue"), Some(&&PyRep::Real(-8.5)));
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::None));
    assert_eq!(row0.get("otherTypeID"), Some(&&PyRep::None));
}

}
