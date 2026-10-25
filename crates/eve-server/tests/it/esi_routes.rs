//! `esi_routes` — Integration tests for `ESI-3` (Universe, Status, Meta, Routes, Search,
//! Insurance, Sovereignty, Wars, Dogma, Killmails) and `ESI-4` (Character, Clones, Skills,
//! Location, Fittings, Assets, Wallet, Loyalty, Mail) routes backed by `SdeRegistry` and
//! `WorldRepository`.

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    KillmailRecord, SystemDynamicState, UpwellStructureRecord, WarRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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

pub(crate) fn validate_and_json(
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
    // parity-lint: allow ESI sweep test covering dogma/insurance/killmail/sovereignty endpoints
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
    // parity: esi:GET /wars/{war_id}/killmails, feat:esi.wars.killmails, rpc:sovMgr.AcquireSkyhooks
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
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
    let sov_tcu_id = 1_030_000_005_003;
    let unallied_tcu_id = 1_030_000_005_004;
    let nullsec_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.1)
        .expect("SDE must contain null-sec solar system")
        .solar_system_id;

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
                ..Default::default()
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
                ..Default::default()
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
            nullsec_sys_id,
            SystemDynamicState {
                solar_system_id: nullsec_sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: Some(alliance_id),
                military_index: 2,
                industrial_index: 1,
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
        snap.structures.insert(
            sov_tcu_id,
            UpwellStructureRecord {
                structure_id: sov_tcu_id,
                type_id: 32458,
                owner_corp_id: corp_id,
                solar_system_id: nullsec_sys_id,
                name: "Sov Infrastructure Hub".to_string(),
                position: [5000.0, 5000.0, 5000.0],
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
            unallied_tcu_id,
            UpwellStructureRecord {
                structure_id: unallied_tcu_id,
                type_id: 32458,
                owner_corp_id: enemy_corp_id,
                solar_system_id: nullsec_sys_id,
                name: "Unallied Hub".to_string(),
                position: [-5000.0, -5000.0, -5000.0],
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
        let legacy_tcu_id = 1_030_000_005_999;
        snap.structures.insert(
            legacy_tcu_id,
            UpwellStructureRecord {
                structure_id: legacy_tcu_id,
                type_id: 32226,
                owner_corp_id: corp_id,
                solar_system_id: nullsec_sys_id,
                name: "Territorial Claim Unit".to_string(),
                position: [0.0, 0.0, 0.0],
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
        let ansiblex_gate_id = 1_030_000_005_888;
        snap.structures.insert(
            ansiblex_gate_id,
            UpwellStructureRecord {
                structure_id: ansiblex_gate_id,
                type_id: 35841,
                owner_corp_id: corp_id,
                solar_system_id: nullsec_sys_id,
                name: "Ansiblex Jump Gate".to_string(),
                position: [1000.0, 1000.0, 1000.0],
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
                killed_unix: now,
                victim_corp_id: enemy_corp_id,
                attacker_corp_id: corp_id,
                attacker_alliance_id: Some(alliance_id),
                ..Default::default()
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

    // 4. Insurance prices (shared compute_insurance_levels formula), Alliances, NPC corps, Planetary Schematic
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
    let rifter_levels = rifter_ins["levels"].as_array().expect("levels");
    assert_eq!(rifter_levels.len(), 6);
    let rifter_base_price = ctx.sde.types[&587].base_price;
    assert!(rifter_base_price > 0.0);
    let expected_tiers = [
        ("Basic", 0.05, 0.50),
        ("Standard", 0.10, 0.60),
        ("Bronze", 0.15, 0.70),
        ("Silver", 0.20, 0.80),
        ("Gold", 0.25, 0.90),
        ("Platinum", 0.30, 1.00),
    ];
    for (idx, (tier_name, cost_frac, payout_frac)) in expected_tiers.iter().enumerate() {
        let exp_cost = (rifter_base_price * cost_frac * 100.0).round() / 100.0;
        let exp_payout = (rifter_base_price * payout_frac * 100.0).round() / 100.0;
        assert_eq!(rifter_levels[idx]["name"], json!(tier_name));
        assert_eq!(rifter_levels[idx]["cost"], json!(exp_cost));
        assert_eq!(rifter_levels[idx]["payout"], json!(exp_payout));
    }

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

    // 8. Sovereignty map & structures (including persisted structure_vulnerability_windows)
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
    // Claim/update sovereignty via `sovMgr.AcquireSkyhooks` so vulnerability windows are populated by the sov mechanic
    let sov_ship_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).map(|c| c.active_ship_id).unwrap_or(100_000_001));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = nullsec_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let nullsec_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
    nullsec_handle
        .spawn_ball(eve_destiny::Ball {
            id: sov_ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_067_000.0,
            radius: 38.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship in nullsec_sys_id");
    let mut sov_sess = eve_machonet::SessionState::new(77_501);
    sov_sess.user_id = Some(1);
    sov_sess.character_id = Some(90_000_001);
    sov_sess.corporation_id = Some(corp_id);
    sov_sess.alliance_id = Some(alliance_id);
    sov_sess.ship_id = Some(sov_ship_id);
    sov_sess.solar_system_id = Some(nullsec_sys_id);
    let _ = ctx
        .db
        .acquire_online_lock(1, sov_sess.session_id, Some(90_000_001), true, now);
    ctx.dispatch_rpc(
        &mut sov_sess,
        "sovMgr",
        None,
        "AcquireSkyhooks",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("sovMgr.AcquireSkyhooks must populate sovereignty structure vulnerability windows");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(sov_tcu_id, (1_000, 2_000));
        snap.structure_vulnerability_windows.insert(1_030_000_005_999, (1_000, 2_000));
        if let Some(st) = snap.system_states.get_mut(&nullsec_sys_id) {
            st.military_index = 3;
            st.industrial_index = 2;
            st.strategic_index = 5;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
    });

    let sov_structs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    let sov_arr = sov_structs.as_array().expect("sov structs");
    assert!(
        !sov_arr.iter().any(|s| s["structure_id"] == json!(friendly_struct_id)),
        "High-sec Astrahus must not appear in /sovereignty/structures"
    );
    assert!(
        !sov_arr.iter().any(|s| s["structure_id"] == json!(unallied_tcu_id)),
        "Unallied structure must not be attributed to system sov_alliance_id"
    );
    assert!(
        !sov_arr.iter().any(|s| s["structure_type_id"] == json!(35841)),
        "Ansiblex Jump Gate (type 35841) must be excluded from /sovereignty/structures"
    );
    let sov_st = sov_arr
        .iter()
        .find(|s| s["structure_id"] == json!(sov_tcu_id))
        .expect("null-sec Infrastructure/Sovereignty Hub (32458) in sov structures");
    assert_eq!(sov_st["alliance_id"], json!(alliance_id));
    assert_eq!(sov_st["solar_system_id"], json!(nullsec_sys_id as i64));
    assert_eq!(sov_st["structure_type_id"], json!(32458));
    assert_eq!(sov_st["vulnerability_occupancy_level"], json!(4.9));
    let hub_v_start = sov_st["vulnerable_start_time"]
        .as_str()
        .expect("populated vulnerable_start_time");
    let hub_v_end = sov_st["vulnerable_end_time"]
        .as_str()
        .expect("populated vulnerable_end_time");
    assert!(
        hub_v_start < hub_v_end,
        "vulnerable_start_time must precede vulnerable_end_time"
    );
    let tcu_st = sov_arr
        .iter()
        .find(|s| s["structure_type_id"] == json!(32226))
        .expect("Territorial Claim Unit (type 32226) must be included in /sovereignty/structures");
    assert_eq!(tcu_st["alliance_id"], json!(alliance_id));
    assert_eq!(tcu_st["vulnerability_occupancy_level"], json!(4.9));
    assert!(tcu_st["vulnerable_start_time"].as_str().is_some());
    assert!(tcu_st["vulnerable_end_time"].as_str().is_some());

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
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

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
    assert_ne!(rifter["description"], rifter["name"]);
    assert_eq!(rifter["group_id"], json!(25));
    assert!(
        !rifter["dogma_attributes"]
            .as_array()
            .expect("dogma_attributes")
            .is_empty()
    );
    let rifter_effs = rifter["dogma_effects"].as_array().expect("dogma_effects");
    assert!(!rifter_effs.is_empty());
    assert!(
        rifter_effs.iter().all(|e| e["is_default"] == json!(false)),
        "Passive ship hull (Rifter 587) must have is_default=false on all effects per typedogma.fsdbinary"
    );
    let autocannon = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v3/universe/types/484/"),
        200,
    );
    let ac_effs = autocannon["dogma_effects"].as_array().expect("484 dogma_effects");
    assert!(
        ac_effs
            .iter()
            .any(|e| e["effect_id"] == json!(34) && e["is_default"] == json!(true)),
        "Active turret module (484) must have is_default=true on effect 34 per typedogma.fsdbinary"
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
}

#[tokio::test]
async fn test_esi3_universe_celestials_stargates_structures_and_bulk_resolution() {
    // parity: esi:GET /universe/planets/{planet_id}, esi:GET /universe/moons/{moon_id}
    // parity: esi:GET /universe/asteroid_belts/{asteroid_belt_id}, esi:GET /universe/stargates/{stargate_id}
    // parity: esi:GET /universe/structures, esi:GET /universe/structures/{structure_id}
    // parity: esi:GET /universe/factions, esi:GET /universe/bloodlines, esi:GET /universe/ancestries
    // parity: esi:POST /universe/names, esi:POST /universe/ids, feat:esi.universe.names-ids
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
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

    let tanoo = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v4/universe/systems/30000001/"),
        200,
    );
    let tanoo_planets = tanoo["planets"].as_array().expect("tanoo planets");
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

    // 3. Structures list + ACL-checked structure detail (401 without token, 403 without scope, 200 for allowed, 403 for denied)
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

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/universe/structures/{allowed_struct_id}/"),
        ),
        401,
    );

    let unscoped_struct_token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec!["esi-location.read_location.v1".to_string()],
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/universe/structures/{allowed_struct_id}/"),
        )
        .with_bearer_token(&unscoped_struct_token),
        403,
    );

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
