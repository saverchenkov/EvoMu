//! `esi_routes` — Integration tests for `ESI-3` (Universe, Status, Meta, Routes, Search,
//! Insurance, Sovereignty, Wars, Dogma, Killmails) and `ESI-4` (Character, Clones, Skills,
//! Location, Fittings, Assets, Wallet, Loyalty, Mail) routes backed by `SdeRegistry` and
//! `WorldRepository`.

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    JumpCloneRecord, KillmailRecord, SkillQueueEntry, SystemDynamicState, UpwellStructureRecord,
    WarRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-routes-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-routes-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn validate_and_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    req: &EsiHttpRequest,
    expected_status: u16,
) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req
        .raw_target
        .split('?')
        .next()
        .unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "Expected {expected_status} for {} {}, got {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(
        &req.method,
        raw_path,
        resp.status_code,
        &resp.headers,
        &resp.body,
    )
    .unwrap_or_else(|e| {
        panic!(
            "OpenAPI schema validation failed for {} {}: {e}",
            req.method, req.raw_target
        )
    });
    if expected_status == 204 {
        Value::Null
    } else {
        resp.json().expect("valid JSON response")
    }
}

#[tokio::test]
async fn test_esi3_dogma_insurance_killmails_sovereignty_routes_and_wars() {
    // parity: esi:GET /dogma/attributes/{attribute_id}, feat:esi.dogma.attributes
    // parity: esi:GET /dogma/effects/{effect_id}, feat:esi.dogma.effects
    // parity: esi:GET /dogma/dynamic/items/{type_id}/{item_id}, feat:esi.dogma.dynamic
    // parity: esi:GET /insurance/prices, feat:esi.insurance.prices
    // parity: esi:GET /characters/{character_id}/killmails/recent, feat:esi.killmails.char-recent
    // parity: esi:GET /corporations/{corporation_id}/killmails/recent, feat:esi.killmails.corp-recent
    // parity: esi:GET /route/{origin}/{destination}, feat:esi.routes.route
    // parity: esi:GET /characters/{character_id}/search, feat:esi.search.char-search
    // parity: esi:GET /sovereignty/map, feat:esi.sovereignty.map
    // parity: esi:GET /sovereignty/structures, feat:esi.sovereignty.structures
    // parity: esi:GET /wars/{war_id}
    // parity: esi:GET /wars/{war_id}/killmails, feat:esi.wars.killmails
    // parity: feat:esi.alliance.list, feat:esi.corporation.npccorps, feat:esi.planetary-interaction.schematics
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();

    let (&muta_type_id, muta_info) = ctx
        .sde
        .dynamic_item_attributes
        .iter()
        .next()
        .expect("SDE mutaplasmid definition");
    let resulting_type_id = muta_info.resulting_type_id;
    let source_type_id = muta_info
        .applicable_type_ids
        .first()
        .copied()
        .unwrap_or(resulting_type_id);

    let corp_id = 98_000_501;
    let enemy_corp_id = 98_000_502;
    let alliance_id = 99_000_501;
    let dyn_item_id = 990_005_001;
    let normal_item_id = 990_005_002;
    let friendly_struct_id = 1_030_000_005_001;
    let hostile_struct_id = 1_030_000_005_002;

    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.standings.insert(enemy_corp_id, -5.0);
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.corporation_id = enemy_corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI3 Test Corp".to_string(),
                ticker: "E3TC".to_string(),
                ceo_id: 90_000_001,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(50_000_000),
                members: HashSet::from([90_000_001]),
            },
        );
        snap.corporations.insert(
            enemy_corp_id,
            CorporationRecord {
                corporation_id: enemy_corp_id,
                name: "Hostile Corp".to_string(),
                ticker: "HSTL".to_string(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.10,
                master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([90_000_002]),
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "ESI3 Sov Alliance".to_string(),
                short_name: "E3SA".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: Some(alliance_id),
                military_index: 3,
                industrial_index: 2,
                strategic_index: 5,
                fw_contested_pct: 0.0,
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
        snap.structures.insert(
            friendly_struct_id,
            UpwellStructureRecord {
                structure_id: friendly_struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Friendly Astrahus".to_string(),
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
        snap.structures.insert(
            hostile_struct_id,
            UpwellStructureRecord {
                structure_id: hostile_struct_id,
                type_id: 35832,
                owner_corp_id: enemy_corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Hostile Astrahus".to_string(),
                position: [-1000.0, -2000.0, -3000.0],
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
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(20u16, 162.5);
        snap.items.insert(
            dyn_item_id,
            InventoryItemRecord {
                item_id: dyn_item_id,
                type_id: resulting_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: dyn_attrs,
                source_mutaplasmid_id: Some(muta_type_id),
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            normal_item_id,
            InventoryItemRecord {
                item_id: normal_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.wars.insert(
            801,
            WarRecord {
                war_id: 801,
                aggressor_corp_id: corp_id,
                defender_corp_id: enemy_corp_id,
                mutual: false,
                started_unix: now,
                active: true,
            },
        );
        snap.killmails.insert(
            9001,
            KillmailRecord {
                kill_id: 9001,
                victim_character_id: 90_000_002,
                attacker_character_id: 90_000_001,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_250_000.0,
            },
        );
        Ok(())
    });

    let token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec![
            "esi-killmails.read_killmails.v1".to_string(),
            "esi-killmails.read_corporation_killmails.v1".to_string(),
            "esi-search.search_structures.v1".to_string(),
        ],
    );

    // 1. Dogma attributes list + detail + 404
    let attr_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/attributes/"),
        200,
    );
    assert!(attr_list.as_array().expect("attrs").contains(&json!(9)));
    let attr_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/attributes/9/"),
        200,
    );
    assert_eq!(attr_detail["attribute_id"], json!(9));
    assert!(attr_detail["name"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(attr_detail["stackable"].is_boolean());
    assert!(attr_detail["high_is_good"].is_boolean());
    assert!(attr_detail["published"].is_boolean());
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/attributes/999999/"),
        404,
    );

    // 2. Dogma effects list + detail (with modifiers) + 404
    let eff_with_mod = ctx
        .sde
        .effects
        .values()
        .find(|e| !e.modifiers.is_empty())
        .expect("effect with modifiers")
        .effect_id;
    let eff_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/effects/"),
        200,
    );
    assert!(eff_list
        .as_array()
        .expect("effects")
        .contains(&json!(eff_with_mod as i64)));
    let eff_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/dogma/effects/{eff_with_mod}/")),
        200,
    );
    assert_eq!(eff_detail["effect_id"], json!(eff_with_mod as i64));
    assert!(
        !eff_detail["modifiers"]
            .as_array()
            .expect("modifiers array")
            .is_empty(),
        "Dogma effect {eff_with_mod} must include modifiers"
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/dogma/effects/999999/"),
        404,
    );

    // 3. Dogma dynamic item (Abyssal mutaplasmid item) + 404 on non-mutated / wrong type
    let dyn_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/dogma/dynamic/items/{resulting_type_id}/{dyn_item_id}/"),
        ),
        200,
    );
    assert_eq!(dyn_json["created_by"], json!(90_000_001));
    assert_eq!(dyn_json["mutator_type_id"], json!(muta_type_id as i64));
    assert_eq!(dyn_json["source_type_id"], json!(source_type_id as i64));
    assert!(dyn_json["dogma_attributes"]
        .as_array()
        .expect("dyn attrs")
        .iter()
        .any(|a| a["attribute_id"] == json!(20) && a["value"] == json!(162.5)));
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/dogma/dynamic/items/587/{normal_item_id}/"),
        ),
        404,
    );

    // 4. Insurance prices, Alliances, NPC corps, Planetary Schematic
    let ins_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/insurance/prices/"),
        200,
    );
    let rifter_ins = ins_json
        .as_array()
        .expect("ins array")
        .iter()
        .find(|e| e["type_id"] == json!(587))
        .expect("Rifter insurance entry");
    assert_eq!(rifter_ins["levels"].as_array().expect("levels").len(), 6);

    let alliances_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/alliances/"),
        200,
    );
    assert!(alliances_json
        .as_array()
        .expect("alliances")
        .contains(&json!(alliance_id)));

    let npccorps_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/corporations/npccorps/"),
        200,
    );
    assert!(npccorps_json.as_array().expect("npccorps").len() > 100);

    let schem_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/schematics/65/"),
        200,
    );
    assert!(schem_json["cycle_time"].as_i64().unwrap_or(0) > 0);

    // 5. Character & Corporation recent killmails
    let char_km = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/killmails/recent/")
            .with_bearer_token(&token),
        200,
    );
    assert!(char_km
        .as_array()
        .expect("char kms")
        .iter()
        .any(|k| k["killmail_id"] == json!(9001)));

    let corp_km = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/corporations/{corp_id}/killmails/recent/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert!(corp_km
        .as_array()
        .expect("corp kms")
        .iter()
        .any(|k| k["killmail_id"] == json!(9001)));

    // 6. Routes (/route/{origin}/{destination} with flag, avoid, connections)
    let route_short = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            "/v1/route/30000142/30002187/?flag=secure&connections=30000142|30002187",
        ),
        200,
    );
    assert_eq!(route_short, json!([30_000_142, 30_002_187]));
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/route/30000142/30000142/?avoid=30000142"),
        404,
    );

    // 7. Search (/characters/{character_id}/search with structure ACL filtering)
    let search_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            "/v3/characters/90000001/search/?categories=solar_system,structure&search=Jita",
        )
        .with_bearer_token(&token),
        200,
    );
    let struct_hits = search_json["structure"].as_array().expect("structure hits");
    assert!(
        struct_hits.contains(&json!(friendly_struct_id)),
        "Friendly structure must appear in search results"
    );
    assert!(
        !struct_hits.contains(&json!(hostile_struct_id)),
        "Hostile structure must be excluded by structure ACL check"
    );

    // 8. Sovereignty map & structures
    let sov_map = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/map/"),
        200,
    );
    assert_eq!(
        sov_map.as_array().expect("sov map").len(),
        ctx.sde.solar_systems.len()
    );
    let sov_structs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    let sov_st = sov_structs
        .as_array()
        .expect("sov structs")
        .iter()
        .find(|s| s["structure_id"] == json!(friendly_struct_id))
        .expect("friendly structure in sov structures");
    assert_eq!(sov_st["alliance_id"], json!(alliance_id));
    assert_eq!(sov_st["vulnerability_occupancy_level"], json!(3.0));
    assert!(sov_st["vulnerable_start_time"].as_str().is_some());
    assert!(sov_st["vulnerable_end_time"].as_str().is_some());

    // 9. Wars detail & war killmails
    let war_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/wars/801/"),
        200,
    );
    assert_eq!(war_detail["id"], json!(801));
    assert_eq!(war_detail["aggressor"]["corporation_id"], json!(corp_id));
    assert_eq!(war_detail["aggressor"]["ships_killed"], json!(1));
    assert_eq!(war_detail["aggressor"]["isk_destroyed"], json!(1_250_000.0));
    assert_eq!(
        war_detail["defender"]["corporation_id"],
        json!(enemy_corp_id)
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/wars/999999/"),
        404,
    );

    let war_kms = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/wars/801/killmails/"),
        200,
    );
    assert_eq!(war_kms.as_array().expect("war kms").len(), 1);
    assert_eq!(war_kms[0]["killmail_id"], json!(9001));
}

#[tokio::test]
async fn test_esi3_universe_hierarchy_celestials_stargates_structures_and_bulk_resolution() {
    // parity: esi:GET /universe/categories, esi:GET /universe/categories/{category_id}
    // parity: esi:GET /universe/groups, esi:GET /universe/groups/{group_id}
    // parity: esi:GET /universe/types/{type_id}, feat:esi.universe.categories-groups-types
    // parity: esi:GET /universe/regions, esi:GET /universe/regions/{region_id}
    // parity: esi:GET /universe/constellations, esi:GET /universe/constellations/{constellation_id}
    // parity: esi:GET /universe/systems, esi:GET /universe/systems/{system_id}, feat:esi.universe.regions-constellations-systems
    // parity: esi:GET /universe/planets/{planet_id}, esi:GET /universe/moons/{moon_id}
    // parity: esi:GET /universe/asteroid_belts/{asteroid_belt_id}, esi:GET /universe/stargates/{stargate_id}
    // parity: esi:GET /universe/structures, esi:GET /universe/structures/{structure_id}
    // parity: esi:GET /universe/factions, esi:GET /universe/bloodlines, esi:GET /universe/ancestries
    // parity: esi:POST /universe/names, esi:POST /universe/ids, feat:esi.universe.names-ids
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let allowed_struct_id = 1_030_000_006_001;
    let denied_struct_id = 1_030_000_006_002;
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.insert(98_000_602, -5.0);
        }
        snap.structures.insert(
            allowed_struct_id,
            UpwellStructureRecord {
                structure_id: allowed_struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_125,
                solar_system_id: 30_000_142,
                name: "Jita Public Hub".to_string(),
                position: [500.0, 600.0, 700.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.structures.insert(
            denied_struct_id,
            UpwellStructureRecord {
                structure_id: denied_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_602,
                solar_system_id: 30_000_142,
                name: "Locked Pirate Citadel".to_string(),
                position: [1500.0, 1600.0, 1700.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        Ok(())
    });

    let struct_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-universe.read_structures.v1".to_string()],
    );

    // 1. Categories, Groups, Types (+ Type detail with Dogma attributes & effects)
    let cats = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/categories/"),
        200,
    );
    assert!(cats.as_array().expect("cats").contains(&json!(6)));
    let cat6 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/categories/6/"),
        200,
    );
    assert_eq!(cat6["name"], json!("Ship"));

    let grps = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/groups/"),
        200,
    );
    assert!(grps.as_array().expect("grps").contains(&json!(25)));
    let grp25 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/groups/25/"),
        200,
    );
    assert!(grp25["types"].as_array().expect("types").contains(&json!(587)));

    let types_p1 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/types/?page=1"),
        200,
    );
    assert_eq!(types_p1.as_array().expect("types page 1").len(), 1000);

    let rifter = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v3/universe/types/587/"),
        200,
    );
    assert_eq!(rifter["type_id"], json!(587));
    assert_eq!(rifter["name"], json!("Rifter"));
    assert_eq!(rifter["group_id"], json!(25));
    assert!(
        !rifter["dogma_attributes"]
            .as_array()
            .expect("dogma_attributes")
            .is_empty()
    );
    assert!(
        !rifter["dogma_effects"]
            .as_array()
            .expect("dogma_effects")
            .is_empty()
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v3/universe/types/999999999/"),
        404,
    );

    // 2. Regions, Constellations, Systems (+ Celestials: Planet, Moon, Asteroid Belt, Stargate)
    let regs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/regions/"),
        200,
    );
    assert!(regs.as_array().expect("regs").contains(&json!(10_000_002)));
    let forge = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/regions/10000002/"),
        200,
    );
    assert_eq!(forge["name"], json!("The Forge"));

    let cons = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/constellations/"),
        200,
    );
    assert!(cons.as_array().expect("cons").contains(&json!(20_000_020)));
    let kimotoro = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/constellations/20000020/"),
        200,
    );
    assert_eq!(kimotoro["name"], json!("Kimotoro"));

    let syss = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/systems/"),
        200,
    );
    assert!(syss.as_array().expect("syss").contains(&json!(30_000_142)));
    let tanoo = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v4/universe/systems/30000001/"),
        200,
    );
    assert_eq!(tanoo["name"], json!("Tanoo"));
    assert!(tanoo["star_id"].as_i64().is_some());
    let tanoo_planets = tanoo["planets"].as_array().expect("tanoo planets");
    assert!(!tanoo_planets.is_empty());
    let first_planet_id = tanoo_planets[0]["planet_id"].as_i64().expect("planet_id");
    let first_moon_id = tanoo_planets
        .iter()
        .find_map(|p| p["moons"].as_array().and_then(|m| m.first()).and_then(Value::as_i64))
        .expect("moon in Tanoo");
    let first_belt_id = tanoo_planets
        .iter()
        .find_map(|p| {
            p["asteroid_belts"]
                .as_array()
                .and_then(|b| b.first())
                .and_then(Value::as_i64)
        })
        .expect("asteroid belt in Tanoo");
    let first_gate_id = tanoo["stargates"]
        .as_array()
        .and_then(|g| g.first())
        .and_then(Value::as_i64)
        .expect("stargate in Tanoo");

    let planet_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/universe/planets/{first_planet_id}/")),
        200,
    );
    assert_eq!(planet_json["planet_id"], json!(first_planet_id));
    assert_eq!(planet_json["system_id"], json!(30_000_001));

    let moon_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/universe/moons/{first_moon_id}/")),
        200,
    );
    assert_eq!(moon_json["moon_id"], json!(first_moon_id));
    assert_eq!(moon_json["system_id"], json!(30_000_001));

    let belt_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/universe/asteroid_belts/{first_belt_id}/"),
        ),
        200,
    );
    assert_eq!(belt_json["system_id"], json!(30_000_001));

    let gate_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/universe/stargates/{first_gate_id}/")),
        200,
    );
    assert_eq!(gate_json["stargate_id"], json!(first_gate_id));
    assert_eq!(gate_json["system_id"], json!(30_000_001));
    assert!(gate_json["destination"]["system_id"].as_i64().is_some());

    // 3. Structures list + ACL-checked structure detail (200 for allowed, 403 for denied)
    let structs_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/structures/"),
        200,
    );
    assert!(structs_list
        .as_array()
        .expect("structures")
        .contains(&json!(allowed_struct_id)));

    let allowed_st = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/universe/structures/{allowed_struct_id}/"),
        )
        .with_bearer_token(&struct_token),
        200,
    );
    assert_eq!(allowed_st["name"], json!("Jita Public Hub"));
    assert_eq!(allowed_st["solar_system_id"], json!(30_000_142));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/universe/structures/{denied_struct_id}/"),
        )
        .with_bearer_token(&struct_token),
        403,
    );

    // 4. Factions, Bloodlines, Ancestries
    let facs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/universe/factions/"),
        200,
    );
    assert!(facs
        .as_array()
        .expect("factions")
        .iter()
        .any(|f| f["faction_id"] == json!(500_001) && f["name"] == json!("Caldari State")));

    let bls = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/bloodlines/"),
        200,
    );
    assert!(!bls.as_array().expect("bloodlines").is_empty());

    let ancs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/ancestries/"),
        200,
    );
    assert!(!ancs.as_array().expect("ancestries").is_empty());

    // 5. Bulk ID <-> Name resolution (POST /universe/names and POST /universe/ids)
    let names_resp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v3/universe/names/").with_body(
            serde_json::to_vec(&json!([30_000_142, 587, 10_000_002, 90_000_001])).unwrap(),
        ),
        200,
    );
    let names_arr = names_resp.as_array().expect("names array");
    assert_eq!(names_arr.len(), 4);
    assert!(names_arr
        .iter()
        .any(|e| e["id"] == json!(30_000_142) && e["name"] == json!("Jita") && e["category"] == json!("solar_system")));
    assert!(names_arr
        .iter()
        .any(|e| e["id"] == json!(587) && e["name"] == json!("Rifter") && e["category"] == json!("inventory_type")));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v3/universe/names/")
            .with_body(serde_json::to_vec(&json!([30_000_142, 30_000_142])).unwrap()),
        400,
    );

    let ids_resp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/universe/ids/")
            .with_body(serde_json::to_vec(&json!(["Jita", "Rifter", "The Forge"])).unwrap()),
        200,
    );
    assert!(ids_resp["systems"]
        .as_array()
        .expect("systems")
        .iter()
        .any(|s| s["id"] == json!(30_000_142)));
    assert!(ids_resp["inventory_types"]
        .as_array()
        .expect("inventory_types")
        .iter()
        .any(|t| t["id"] == json!(587)));
    assert!(ids_resp["regions"]
        .as_array()
        .expect("regions")
        .iter()
        .any(|r| r["id"] == json!(10_000_002)));
}

#[tokio::test]
async fn test_esi4_character_clones_skills_location_fittings_assets_wallet_loyalty_and_mail() {
    // parity: esi:POST /characters/affiliation, feat:esi.character.affiliation
    // parity: esi:GET /characters/{character_id}/portrait, feat:esi.character.portrait
    // parity: esi:GET /characters/{character_id}/fatigue, feat:esi.character.fatigue
    // parity: esi:GET /characters/{character_id}/blueprints, feat:esi.character.blueprints
    // parity: feat:esi.character.standings, feat:esi.clones.clones
    // parity: esi:GET /characters/{character_id}/implants, feat:esi.clones.implants
    // parity: esi:GET /characters/{character_id}/attributes, esi:GET /characters/{character_id}/skillqueue
    // parity: esi:GET /characters/{character_id}/location, feat:esi.location.location
    // parity: esi:GET /characters/{character_id}/online
    // parity: esi:GET /characters/{character_id}/ship, feat:esi.location.ship
    // parity: esi:GET /characters/{character_id}/fittings, feat:esi.fittings.list
    // parity: esi:POST /characters/{character_id}/fittings, esi:DELETE /characters/{character_id}/fittings/{fitting_id}, feat:esi.fittings.write
    // parity: esi:GET /characters/{character_id}/assets, feat:esi.assets.char-assets
    // parity: esi:POST /characters/{character_id}/assets/locations, feat:esi.assets.char-locations
    // parity: esi:POST /characters/{character_id}/assets/names, feat:esi.assets.char-names
    // parity: esi:GET /corporations/{corporation_id}/assets, feat:esi.assets.corp-assets
    // parity: esi:POST /corporations/{corporation_id}/assets/locations, esi:POST /corporations/{corporation_id}/assets/names, feat:esi.assets.corp-locations-names
    // parity: esi:GET /corporations/{corporation_id}/wallets
    // parity: esi:GET /characters/{character_id}/loyalty/points, feat:esi.loyalty.points
    // parity: esi:GET /characters/{character_id}/mail, esi:POST /characters/{character_id}/mail, esi:GET /characters/{character_id}/mail/{mail_id}, esi:DELETE /characters/{character_id}/mail/{mail_id}
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();

    let corp_id = 98_000_701;
    let alliance_id = 99_000_701;
    let bpo_item_id = 990_007_001;
    let bpc_item_id = 990_007_002;
    let corp_asset_id = 990_007_003;
    let (&bp_type_id, _) = ctx.sde.blueprints.iter().next().expect("SDE blueprint");

    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.war_faction_id = Some(500_001);
            ch.last_clone_jump_unix = now.saturating_sub(3600);
            ch.jump_cooldown_until_unix = now + 1800;
            ch.jump_fatigue_minutes = 30.0;
            ch.loyalty_points.insert(1_000_035, 12_500);
            ch.standings.insert(1_000_035, 5.25);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI4 Director Corp".to_string(),
                ticker: "E4DC".to_string(),
                ceo_id: 90_000_001,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(250_000_000),
                members: HashSet::from([90_000_001]),
            },
        );
        snap.active_implants.insert(90_000_001, vec![9899, 9941]);
        snap.jump_clones.insert(
            90_000_001,
            vec![JumpCloneRecord {
                jump_clone_id: 7001,
                character_id: 90_000_001,
                location_id: 60_003_760,
                implants: vec![9899],
            }],
        );
        snap.skill_queues.insert(
            90_000_001,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 8000.0,
                end_sp: 45_255.0,
                duration_seconds: 1800.0,
            }],
        );
        snap.items.insert(
            bpo_item_id,
            InventoryItemRecord {
                item_id: bpo_item_id,
                type_id: bp_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(1112, 8.0), (1113, 16.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            bpc_item_id,
            InventoryItemRecord {
                item_id: bpc_item_id,
                type_id: bp_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(10),
            },
        );
        snap.items.insert(
            corp_asset_id,
            InventoryItemRecord {
                item_id: corp_asset_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.corporations.insert(
            98_000_702,
            CorporationRecord {
                corporation_id: 98_000_702,
                name: "Other CEO Corp".to_string(),
                ticker: "OTHR".to_string(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([90_000_002]),
            },
        );
        Ok(())
    });

    ctx.db
        .acquire_online_lock(1, 42, Some(90_000_001), true, now)
        .expect("acquire_online_lock");

    let token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec![
            "esi-assets.read_assets.v1".to_string(),
            "esi-assets.read_corporation_assets.v1".to_string(),
            "esi-characters.read_blueprints.v1".to_string(),
            "esi-characters.read_fatigue.v1".to_string(),
            "esi-characters.read_loyalty.v1".to_string(),
            "esi-characters.read_standings.v1".to_string(),
            "esi-clones.read_clones.v1".to_string(),
            "esi-clones.read_implants.v1".to_string(),
            "esi-fittings.read_fittings.v1".to_string(),
            "esi-fittings.write_fittings.v1".to_string(),
            "esi-location.read_location.v1".to_string(),
            "esi-location.read_online.v1".to_string(),
            "esi-location.read_ship_type.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.send_mail.v1".to_string(),
            "esi-skills.read_skills.v1".to_string(),
            "esi-skills.read_skillqueue.v1".to_string(),
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-wallet.read_corporation_wallets.v1".to_string(),
        ],
    );

    // 1. Character affiliation, portrait, standings, fatigue, blueprints
    let aff = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/affiliation/")
            .with_body(serde_json::to_vec(&json!([90_000_001])).unwrap()),
        200,
    );
    assert_eq!(aff[0]["character_id"], json!(90_000_001));
    assert_eq!(aff[0]["corporation_id"], json!(corp_id));
    assert_eq!(aff[0]["alliance_id"], json!(alliance_id));
    assert_eq!(aff[0]["faction_id"], json!(500_001));

    let portrait = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/portrait/"),
        200,
    );
    assert!(portrait["px128x128"]
        .as_str()
        .is_some_and(|u| u.contains("90000001")));

    let standings = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/standings/").with_bearer_token(&token),
        200,
    );
    assert!(standings
        .as_array()
        .expect("standings")
        .iter()
        .any(|s| s["from_id"] == json!(1_000_035) && s["standing"] == json!(5.25)));

    let fatigue = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/fatigue/").with_bearer_token(&token),
        200,
    );
    assert!(fatigue["jump_fatigue_expire_date"].as_str().is_some());
    assert!(fatigue["last_jump_date"].as_str().is_some());

    let bps = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/blueprints/")
            .with_bearer_token(&token),
        200,
    );
    let bps_arr = bps.as_array().expect("blueprints");
    assert!(bps_arr.iter().any(|b| {
        b["item_id"] == json!(bpo_item_id)
            && b["quantity"] == json!(-1)
            && b["runs"] == json!(-1)
            && b["material_efficiency"] == json!(8)
            && b["time_efficiency"] == json!(16)
    }));
    assert!(bps_arr
        .iter()
        .any(|b| b["item_id"] == json!(bpc_item_id) && b["quantity"] == json!(-2) && b["runs"] == json!(10)));

    // 2. Clones & Implants
    let clones = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v3/characters/90000001/clones/").with_bearer_token(&token),
        200,
    );
    assert_eq!(clones["jump_clones"][0]["jump_clone_id"], json!(7001));
    assert!(clones["last_clone_jump_date"].as_str().is_some());

    let implants = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/implants/").with_bearer_token(&token),
        200,
    );
    assert_eq!(implants, json!([9899, 9941]));

    // 3. Skills, Attributes, Skillqueue
    let attrs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/attributes/")
            .with_bearer_token(&token),
        200,
    );
    assert!(attrs["intelligence"].as_i64().unwrap_or(0) >= 15);

    let sq = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/skillqueue/")
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(sq[0]["skill_id"], json!(3300));
    assert_eq!(sq[0]["finished_level"], json!(4));

    // 4. Location (station & structure), Online, Ship
    let loc = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/location/").with_bearer_token(&token),
        200,
    );
    assert_eq!(loc["solar_system_id"], json!(30_000_142));
    assert_eq!(loc["station_id"], json!(60_003_760));
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(1_000_000_000_701);
        }
        Ok(())
    });
    let loc_struct = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/location/").with_bearer_token(&token),
        200,
    );
    assert_eq!(loc_struct["structure_id"], json!(1_000_000_000_701_i64));
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
        Ok(())
    });

    let online = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/online/").with_bearer_token(&token),
        200,
    );
    assert_eq!(online["online"], json!(true));

    let ship = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/ship/").with_bearer_token(&token),
        200,
    );
    assert!(ship["ship_item_id"].as_i64().unwrap_or(0) > 0);
    assert!(ship["ship_type_id"].as_i64().unwrap_or(0) > 0);

    // 5. Fittings CRUD (POST -> GET -> DELETE -> GET)
    let created_fit = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/characters/90000001/fittings/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "name": "ESI4 Rifter Fit",
                    "description": "Created via ESI POST",
                    "ship_type_id": 587,
                    "items": [
                        { "type_id": 440, "flag": "MedSlot0", "quantity": 1 }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let fit_id = created_fit["fitting_id"].as_i64().expect("fitting_id");

    let fits_after_post = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/fittings/").with_bearer_token(&token),
        200,
    );
    assert!(fits_after_post
        .as_array()
        .expect("fits")
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id) && f["name"] == json!("ESI4 Rifter Fit")));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/90000001/fittings/{fit_id}/"),
        )
        .with_bearer_token(&token),
        204,
    );

    let fits_after_del = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/fittings/").with_bearer_token(&token),
        200,
    );
    assert!(!fits_after_del
        .as_array()
        .expect("fits")
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id)));

    // 6. Character & Corporation Assets + Locations + Names (+ Director/CEO 403 check)
    let char_assets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/characters/90000001/assets/").with_bearer_token(&token),
        200,
    );
    assert!(char_assets
        .as_array()
        .expect("char assets")
        .iter()
        .any(|a| a["item_id"] == json!(bpo_item_id)));

    let char_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/characters/90000001/assets/locations/")
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([bpo_item_id])).unwrap()),
        200,
    );
    assert_eq!(char_locs[0]["item_id"], json!(bpo_item_id));
    assert!(char_locs[0]["position"]["x"].is_number());

    let char_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/assets/names/")
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([bpo_item_id])).unwrap()),
        200,
    );
    assert_eq!(char_names[0]["item_id"], json!(bpo_item_id));
    assert!(char_names[0]["name"].as_str().is_some_and(|s| !s.is_empty()));

    let corp_assets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(corp_assets
        .as_array()
        .expect("corp assets")
        .iter()
        .any(|a| a["item_id"] == json!(corp_asset_id)));

    // Non-CEO / non-director gets 403 on corporation assets
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/corporations/98000702/assets/").with_bearer_token(&token),
        403,
    );

    let corp_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "POST",
            format!("/v2/corporations/{corp_id}/assets/locations/"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([corp_asset_id])).unwrap()),
        200,
    );
    assert_eq!(corp_locs[0]["item_id"], json!(corp_asset_id));

    let corp_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/corporations/{corp_id}/assets/names/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([corp_asset_id])).unwrap()),
        200,
    );
    assert_eq!(corp_names[0]["item_id"], json!(corp_asset_id));
    assert_eq!(corp_names[0]["name"], json!("Rifter"));

    // 7. Corporation Wallets & Character Loyalty Points
    let corp_wallets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/wallets/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_wallets.as_array().expect("divisions").len(), 7);
    assert_eq!(corp_wallets[0]["division"], json!(1));
    assert_eq!(corp_wallets[0]["balance"], json!(250_000_000.0));

    let lp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/loyalty/points/")
            .with_bearer_token(&token),
        200,
    );
    assert!(lp
        .as_array()
        .expect("lp")
        .iter()
        .any(|e| e["corporation_id"] == json!(1_000_035) && e["loyalty_points"] == json!(12_500)));

    // 8. Mail CRUD (POST -> GET list -> GET detail -> DELETE -> 404)
    let mail_id_val = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "ESI4 Fleet Ping",
                    "body": "Forming in Jita 4-4",
                    "recipients": [
                        { "recipient_id": 90_000_002, "recipient_type": "character" }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let mail_id = mail_id_val.as_i64().expect("created mail_id");

    let mail_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/mail/").with_bearer_token(&token),
        200,
    );
    assert!(mail_list
        .as_array()
        .expect("mails")
        .iter()
        .any(|m| m["mail_id"] == json!(mail_id) && m["subject"] == json!("ESI4 Fleet Ping")));

    let mail_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(mail_detail["subject"], json!("ESI4 Fleet Ping"));
    assert_eq!(mail_detail["body"], json!("Forming in Jita 4-4"));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("DELETE", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        204,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        404,
    );
}
