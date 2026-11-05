//! Unit tests for ESI universe data, search, routing, names/ids, dogma, killmails, wars, sovereignty, and FW.

use super::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use crate::router::ServerContext;
use eve_db::{UpwellStructureRecord, WarRecord};
use eve_sde::SdeRegistry;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn seed_universe_context() -> (Arc<ServerContext>, String, i64, i64) {
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

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = char_id;
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, !0_u64);
        }
        snap.structures.insert(
            1_055_000_001,
            UpwellStructureRecord {
                structure_id: 1_055_000_001,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Trade Hub".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.wars.insert(
            44_001,
            WarRecord {
                war_id: 44_001,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: now,
                active: true,
            },
        );
    });

    let scopes: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
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
        .unwrap();
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;

    (Arc::new(ctx), token, char_id, corp_id)
}

fn call_json(router: &EsiRouter, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
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
fn esi_universe_catalog_search_routes_and_names_ids() {
    let (ctx, token, cid, _) = seed_universe_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    let jita_cels = ctx.sde.celestials_in_system(30_000_142);
    let star_id = jita_cels
        .iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_075);
    let planet_id = jita_cels
        .iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_077);
    let moon_id = jita_cels
        .iter()
        .find(|c| c.group_id == 8)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_079);
    let belt_id = jita_cels
        .iter()
        .find(|c| c.group_id == 9)
        .map(|c| c.celestial_id)
        .unwrap_or(40_000_003);
    let gate_id = jita_cels
        .iter()
        .find(|c| c.group_id == 10)
        .map(|c| c.celestial_id)
        .unwrap_or(50_001_248);

    for path in [
        "/latest/universe/categories/",
        "/latest/universe/categories/6/",
        "/latest/universe/groups/",
        "/latest/universe/groups/25/",
        "/latest/universe/types/",
        "/latest/universe/types/587/",
        "/latest/universe/regions/",
        "/latest/universe/regions/10000002/",
        "/latest/universe/constellations/",
        "/latest/universe/constellations/20000020/",
        "/latest/universe/systems/",
        "/latest/universe/systems/30000142/",
        &format!("/latest/universe/planets/{planet_id}/"),
        &format!("/latest/universe/moons/{moon_id}/"),
        &format!("/latest/universe/asteroid_belts/{belt_id}/"),
        &format!("/latest/universe/stargates/{gate_id}/"),
        &format!("/latest/universe/stars/{star_id}/"),
        "/latest/universe/stations/60003760/",
        "/latest/universe/structures/",
        "/latest/universe/factions/",
        "/latest/universe/races/",
        "/latest/universe/bloodlines/",
        "/latest/universe/ancestries/",
        "/latest/universe/graphics/",
        "/latest/universe/system_jumps/",
        "/latest/universe/system_kills/",
    ] {
        let (st, _) = call_json(&router, "GET", path, None, None);
        assert_eq!(st, 200, "GET {path}");
    }

    // Authenticated structure detail & character search & autopilot waypoint
    let _ = ctx.db.acquire_online_lock(1, 101, Some(cid), true, ctx.now_unix());
    assert_eq!(
        call_json(
            &router,
            "GET",
            "/latest/universe/structures/1055000001/",
            Some(&token),
            None,
        )
        .0,
        200
    );
    let (st_search, search_res) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{cid}/search/?categories=solar_system,inventory_type&search=Jita"),
        Some(&token),
        None,
    );
    assert_eq!(st_search, 200);
    assert!(search_res["solar_system"].as_array().unwrap().contains(&json!(30_000_142)));

    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/autopilot/waypoint/?destination_id=30000142&add_to_beginning=true&clear_other_waypoints=false",
            Some(&token),
            None,
        )
        .0,
        204
    );

    // Route planning & bulk name/id resolution
    let (st_route, route) = call_json(&router, "GET", "/latest/route/30000142/30000144/", None, None);
    assert_eq!(st_route, 200);
    assert!(!route.as_array().unwrap().is_empty());

    let (st_names, names) = call_json(
        &router,
        "POST",
        "/latest/universe/names/",
        None,
        Some(json!([cid, 30_000_142, 587, 34])),
    );
    assert_eq!(st_names, 200);
    assert_eq!(names.as_array().unwrap().len(), 4);

    let (st_ids, ids) = call_json(
        &router,
        "POST",
        "/latest/universe/ids/",
        None,
        Some(json!(["Jita", "The Forge", "Rifter", "Tritanium"])),
    );
    assert_eq!(st_ids, 200);
    assert!(ids["systems"].as_array().is_some());
}

#[test]
fn esi_dogma_insurance_killmails_wars_sovereignty_and_fw_endpoints() {
    let (ctx, token, cid, corpid) = seed_universe_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    let attr_id = ctx.sde.dogma_attributes().keys().copied().next().unwrap_or(1);
    let effect_id = ctx.sde.dogma_effects().keys().copied().next().unwrap_or(1);

    for (path, need_auth) in [
        ("/latest/dogma/attributes/", false),
        (&format!("/latest/dogma/attributes/{attr_id}/"), false),
        ("/latest/dogma/effects/", false),
        (&format!("/latest/dogma/effects/{effect_id}/"), false),
        ("/latest/insurance/prices/", false),
        (&format!("/latest/characters/{cid}/killmails/recent/"), true),
        (&format!("/latest/corporations/{corpid}/killmails/recent/"), true),
        ("/latest/wars/", false),
        ("/latest/wars/44001/", false),
        ("/latest/wars/44001/killmails/", false),
        ("/latest/sovereignty/map/", false),
        ("/latest/sovereignty/structures/", false),
        ("/latest/sovereignty/campaigns/", false),
        ("/latest/incursions/", false),
        ("/latest/fw/wars/", false),
        ("/latest/fw/stats/", false),
        ("/latest/fw/systems/", false),
        ("/latest/fw/leaderboards/", false),
        ("/latest/fw/leaderboards/characters/", false),
        ("/latest/fw/leaderboards/corporations/", false),
        (&format!("/latest/characters/{cid}/fw/stats/"), true),
        (&format!("/latest/corporations/{corpid}/fw/stats/"), true),
    ] {
        let tok = if need_auth { Some(token.as_str()) } else { None };
        let (st, _) = call_json(&router, "GET", path, tok, None);
        assert_eq!(st, 200, "GET {path}");
    }
}
