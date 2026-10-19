//! ESI-3 (Universe, Status, Dogma, Killmails, Insurance, Sovereignty, Wars, FW) and
//! ESI-4 (Character, Clones, Skills, Location, Fittings, Assets, Wallet, Loyalty, Calendar, Contacts)
//! integration tests backed by SdeRegistry and WorldRepository.

#![forbid(unsafe_code)]

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
            "evomu-esi-universe-character-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-universe-character-test", PKCE_VERIFIER, now)
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
async fn test_esi3_universe_status_meta_dogma_killmails_insurance_fw() {
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;
    let corp_id = 1_000_001;
    let enemy_corp_id = 1_000_009;
    let alliance_id = 99_000_001;
    let war_id = 801;
    let killmail_id = 9001;
    let allowed_struct_id = 1_000_000_001_001_i64;

    // Seed test fixtures in WorldStateSnapshot
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        // Alliance & Enemy Corp
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Federal Defense Alliance".to_string(),
                short_name: "FEDDF".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.corporations.insert(
            enemy_corp_id,
            CorporationRecord {
                corporation_id: enemy_corp_id,
                name: "Guristas Raiders Corp".to_string(),
                ticker: "GURIST".to_string(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.1,
                master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([90_000_002]),
                ..Default::default()
            },
        );

        // Upwell Structure
        snap.structures.insert(
            allowed_struct_id,
            UpwellStructureRecord {
                structure_id: allowed_struct_id,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Citadel Perimeter Hub".to_string(),
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

        // System dynamic state with Sovereignty
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

        // War
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: corp_id,
                defender_corp_id: enemy_corp_id,
                mutual: false,
                started_unix: now,
                active: true,
            },
        );

        // Killmail
        snap.killmails.insert(
            killmail_id,
            KillmailRecord {
                kill_id: killmail_id,
                victim_character_id: 90_000_002,
                attacker_character_id: char_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_250_000.0,
            },
        );
    });

    let struct_token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec!["esi-universe.read_structures.v1".to_string()],
    );

    // 1. /v1/universe/types/ and /v1/universe/types/{type_id}/
    let types_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/types/"),
        200,
    );
    assert!(!types_list.as_array().expect("types array").is_empty());
    assert!(types_list
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!(587))); // Rifter

    let rifter_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/types/587/"),
        200,
    );
    assert_eq!(rifter_detail["type_id"], json!(587));
    assert_eq!(rifter_detail["name"], json!("Rifter"));
    assert!(
        rifter_detail["description"]
            .as_str()
            .unwrap_or("")
            .contains("very powerful combat frigate"),
        "Rifter description must come from localized SDE (got {:?})",
        rifter_detail["description"]
    );
    assert!(rifter_detail["dogma_attributes"].as_array().is_some());

    let autocannon_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/types/484/"),
        200,
    );
    let ac_effects = autocannon_detail["dogma_effects"]
        .as_array()
        .expect("dogma_effects array");
    let default_effs: Vec<_> = ac_effects
        .iter()
        .filter(|e| e["is_default"] == json!(true))
        .collect();
    assert_eq!(default_effs.len(), 1);
    assert_eq!(default_effs[0]["effect_id"], json!(34)); // projectileFired from typedogma.fsdbinary

    let factions = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/universe/factions/"),
        200,
    );
    assert!(factions.as_array().unwrap().iter().any(|f| {
        let desc = f["description"].as_str().unwrap_or("");
        f["faction_id"] == json!(500_001) && desc.len() > 20 && !desc.contains("size_factor=")
    }));

    let bloodlines = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/bloodlines/"),
        200,
    );
    assert!(bloodlines.as_array().unwrap().iter().any(|b| {
        let desc = b["description"].as_str().unwrap_or("");
        b["bloodline_id"] == json!(1) && desc.len() > 20 && !desc.contains("CHA ")
    }));

    let ancestries = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/ancestries/"),
        200,
    );
    assert!(ancestries.as_array().unwrap().iter().any(|a| {
        let desc = a["description"].as_str().unwrap_or("");
        a["id"] == json!(1) && desc.len() > 20 && !desc.contains("CHA +")
    }));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/types/99999999/"),
        404,
    );

    // 2. /v1/universe/systems/{system_id}/
    let jita_sys = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/systems/30000142/"),
        200,
    );
    assert_eq!(jita_sys["system_id"], json!(30_000_142));
    assert_eq!(jita_sys["name"], json!("Jita"));
    assert!(jita_sys["security_status"].as_f64().unwrap_or(0.0) >= 0.9);

    // 3. /v1/universe/stations/{station_id}/
    let jita_stn = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/universe/stations/60003760/"),
        200,
    );
    assert_eq!(jita_stn["station_id"], json!(60_003_760));
    assert_eq!(jita_stn["system_id"], json!(30_000_142));
    assert!(jita_stn["services"].as_array().is_some());

    // 4. /v1/universe/structures/{structure_id}/
    // Without token: 401 Unauthorized
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/universe/structures/{allowed_struct_id}/"),
        ),
        401,
    );

    // With token: 200 OK
    let struct_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v2/universe/structures/{allowed_struct_id}/"),
        )
        .with_bearer_token(&struct_token),
        200,
    );
    assert_eq!(struct_detail["name"], json!("Jita Citadel Perimeter Hub"));
    assert_eq!(struct_detail["solar_system_id"], json!(30_000_142));

    // 5. /v2/universe/names/
    let names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/universe/names/")
            .with_body(serde_json::to_vec(&json!([30_000_142, 587, 90_000_001])).unwrap()),
        200,
    );
    let names_arr = names.as_array().expect("names array");
    assert!(names_arr
        .iter()
        .any(|e| e["id"] == json!(30_000_142) && e["name"] == json!("Jita")));
    assert!(names_arr
        .iter()
        .any(|e| e["id"] == json!(587) && e["name"] == json!("Rifter")));

    // 6. /v2/universe/ids/
    let ids = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/universe/ids/")
            .with_body(serde_json::to_vec(&json!(["Jita", "Rifter"])).unwrap()),
        200,
    );
    assert!(ids["systems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == json!(30_000_142) && e["name"] == json!("Jita")));
    assert!(ids["inventory_types"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == json!(587) && e["name"] == json!("Rifter")));

    // 7. /v1/status/
    let status_val = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/status/"),
        200,
    );
    assert!(status_val["server_version"].is_string());
    assert!(status_val["players"].is_number());

    // 8. /v1/meta/ and /meta/compatibility-dates
    let meta_resp = router.handle_request(&EsiHttpRequest::new("GET", "/v1/meta/"));
    assert_eq!(meta_resp.status_code, 200);
    let meta_json: Value = serde_json::from_slice(&meta_resp.body).expect("meta json");
    assert!(meta_json["compatibility_dates"].is_string());

    let compat_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/meta/compatibility-dates"),
        200,
    );
    assert!(compat_json["compatibility_dates"].is_array());

    // 9. /v1/sovereignty/map/ and /v1/sovereignty/campaigns/
    let sov_map = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/map/"),
        200,
    );
    assert!(sov_map.as_array().is_some());
    assert!(sov_map
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["system_id"] == json!(30_000_142) && s["alliance_id"] == json!(alliance_id)));

    let campaigns = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        200,
    );
    assert!(campaigns
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["solar_system_id"] == json!(30_000_142)));

    // 10. /v1/wars/, /v1/wars/{war_id}/, /v1/wars/{war_id}/killmails/
    let wars_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/wars/"),
        200,
    );
    assert!(wars_list
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w == &json!(war_id)));

    let war_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/")),
        200,
    );
    assert_eq!(war_detail["id"], json!(war_id));
    assert_eq!(war_detail["aggressor"]["corporation_id"], json!(corp_id));

    let war_kms = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/killmails/")),
        200,
    );
    assert!(war_kms.as_array().is_some());

    // 11. /v1/dogma/attributes/, /v1/dogma/attributes/{attribute_id}/, /v1/dogma/effects/{effect_id}/
    let dogma_attrs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/attributes/"),
        200,
    );
    assert!(!dogma_attrs.as_array().unwrap().is_empty());

    let attr_9 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/attributes/9/"),
        200,
    );
    assert_eq!(attr_9["attribute_id"], json!(9));

    let effect_16 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/dogma/effects/16/"),
        200,
    );
    assert_eq!(effect_16["effect_id"], json!(16));

    // 12. /v1/killmails/{killmail_id}/{killmail_hash}/
    let valid_hash = format!("{:040x}", (killmail_id as u64).wrapping_mul(2_654_435_761));
    // Invalid hash -> 422
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/killmails/{killmail_id}/0000000000000000000000000000000000000000/"),
        ),
        422,
    );
    // Valid hash -> 200
    let km_resp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/killmails/{killmail_id}/{valid_hash}/"),
        ),
        200,
    );
    assert_eq!(km_resp["killmail_id"], json!(killmail_id));
    assert_eq!(km_resp["victim"]["character_id"], json!(90_000_002));
    assert_eq!(km_resp["attackers"][0]["character_id"], json!(char_id));

    // 13. /v1/insurance/prices/
    let ins_prices = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/insurance/prices/"),
        200,
    );
    assert!(ins_prices
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["type_id"] == json!(587)));

    // 14. /v1/fw/leaderboards/
    let fw_lb = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"),
        200,
    );
    assert!(fw_lb["kills"]["yesterday"].is_array());
    assert!(fw_lb["victory_points"]["yesterday"].is_array());
}

#[tokio::test]
async fn test_esi4_character_clones_skills_location_fittings_assets_wallet_loyalty_calendar_contacts() {
    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001;

    // Seed test data for character
    ctx.db.with_mut_snapshot(|snap| {
        // Standings
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 1_000_001;
            ch.standings.insert(1_000_001, 7.5);
            ch.standings.insert(1_000_009, -9.0);
            ch.balance = IskCents::from_isk(2_500_000);
            ch.loyalty_points.insert(1_000_035, 12_500);
            ch.station_id = Some(60_003_760);
        }

        // Jump Clone & Implants
        snap.jump_clones.insert(
            char_id,
            vec![JumpCloneRecord {
                jump_clone_id: 7001,
                character_id: char_id,
                location_id: 60_008_494,
                implants: vec![9941],
            }],
        );
        snap.active_implants.insert(char_id, vec![9899, 9941]);

        // Skills & Skill queue
        snap.skills.entry(char_id).or_default().insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 40_000.0,
                end_sp: 256_000.0,
                duration_seconds: 1800.0,
            }],
        );

        // Assets
        snap.items.insert(
            990_000_001,
            InventoryItemRecord {
                item_id: 990_000_001,
                type_id: 587,
                owner_id: char_id,
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
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-assets.read_assets.v1".to_string(),
            "esi-characters.read_loyalty.v1".to_string(),
            "esi-characters.read_standings.v1".to_string(),
            "esi-clones.read_clones.v1".to_string(),
            "esi-clones.read_implants.v1".to_string(),
            "esi-fittings.read_fittings.v1".to_string(),
            "esi-fittings.write_fittings.v1".to_string(),
            "esi-location.read_location.v1".to_string(),
            "esi-location.read_online.v1".to_string(),
            "esi-location.read_ship_type.v1".to_string(),
            "esi-skills.read_skills.v1".to_string(),
            "esi-skills.read_skillqueue.v1".to_string(),
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-characters.read_contacts.v1".to_string(),
            "esi-characters.write_contacts.v1".to_string(),
            "esi-calendar.read_calendar_events.v1".to_string(),
            "esi-calendar.respond_calendar_events.v1".to_string(),
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.send_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );

    // 1. /v4/characters/{id}/ (Public character info)
    let char_pub = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/characters/{char_id}/")),
        200,
    );
    assert_eq!(char_pub["name"], json!("Aura Capsuleer"));
    assert_eq!(char_pub["corporation_id"], json!(1_000_001));
    assert_eq!(char_pub["race_id"], json!(2));
    assert_eq!(char_pub["bloodline_id"], json!(3));

    // 2. /v1/characters/{id}/affiliation/
    let aff = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/affiliation/")
            .with_body(serde_json::to_vec(&json!([char_id])).unwrap()),
        200,
    );
    assert_eq!(aff[0]["character_id"], json!(char_id));
    assert_eq!(aff[0]["corporation_id"], json!(1_000_001));

    // 3. /v2/characters/{id}/portrait/
    let portrait = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/portrait/")),
        200,
    );
    assert!(portrait["px64x64"].is_string());
    assert!(portrait["px128x128"].is_string());

    // 4. /v2/characters/{id}/standings/
    // Without token: 401 Unauthorized
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/standings/")),
        401,
    );
    // With token: 200 OK
    let standings = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/standings/"))
            .with_bearer_token(&token),
        200,
    );
    let st_arr = standings.as_array().unwrap();
    assert!(st_arr
        .iter()
        .any(|s| s["from_id"] == json!(1_000_001) && s["standing"] == json!(7.5)));

    // 5. /v3/characters/{id}/clones/ and /v1/characters/{id}/implants/
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/characters/{char_id}/clones/")),
        401,
    );
    let clones = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v3/characters/{char_id}/clones/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(clones["jump_clones"][0]["jump_clone_id"], json!(7001));

    let implants = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/implants/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(implants, json!([9899, 9941]));

    // 6. /v4/characters/{id}/skills/, /v1/characters/{id}/skillqueue/, /v1/characters/{id}/attributes/
    let skills = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v4/characters/{char_id}/skills/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(skills["total_sp"].as_f64().unwrap_or(0.0) >= 40_000.0);

    let sq = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/skillqueue/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(sq[0]["skill_id"], json!(3300));
    assert_eq!(sq[0]["finished_level"], json!(4));

    let attrs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/attributes/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(attrs["intelligence"].as_i64().unwrap_or(0) >= 15);
    assert!(attrs["perception"].as_i64().unwrap_or(0) >= 15);

    // 7. /v1/characters/{id}/location/, /v1/characters/{id}/online/, /v1/characters/{id}/ship/
    let loc = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/location/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(loc["solar_system_id"], json!(30_000_142));
    assert_eq!(loc["station_id"], json!(60_003_760));

    let online = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/online/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(online["online"].is_boolean());

    let ship = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/ship/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(ship["ship_type_id"].as_i64().unwrap_or(0) > 0);

    // 8. /v2/characters/{id}/fittings/ CRUD (POST -> GET -> DELETE)
    let created_fit = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v2/characters/{char_id}/fittings/"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "name": "Integration Rifter",
                    "description": "Fitted via ESI POST",
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

    let fits_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v2/characters/{char_id}/fittings/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(fits_list
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id)));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/{char_id}/fittings/{fit_id}/"),
        )
        .with_bearer_token(&token),
        204,
    );

    // 9. /v5/characters/{id}/assets/
    let assets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/characters/{char_id}/assets/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(assets
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["item_id"] == json!(990_000_001)));

    // 10. /v1/characters/{id}/wallet/
    let wallet = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/wallet/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(wallet, json!(2_500_000.0));

    // 11. /v1/characters/{id}/loyalty/points/
    let lp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/loyalty/points/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(lp
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["corporation_id"] == json!(1_000_035) && e["loyalty_points"] == json!(12_500)));

    // 12. /v1/characters/{id}/contacts/
    let contacts = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/contacts/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(contacts.as_array().is_some());

    let add_contact = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "POST",
            format!("/v1/characters/{char_id}/contacts/?standing=5.0"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([90_000_002])).unwrap()),
        201,
    );
    assert!(add_contact.as_array().unwrap().contains(&json!(90_000_002)));

    // 13. /v1/characters/{id}/calendar/
    let calendar = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/calendar/"))
            .with_bearer_token(&token),
        200,
    );
    let cal_events = calendar.as_array().expect("calendar events");
    assert!(!cal_events.is_empty());
    let event_id = cal_events[0]["event_id"].as_i64().expect("event_id");

    let event_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{char_id}/calendar/{event_id}/"),
        )
        .with_bearer_token(&token),
        200,
    );
    assert_eq!(event_detail["event_id"], json!(event_id));

    // Respond to calendar event
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "PUT",
            format!("/v1/characters/{char_id}/calendar/{event_id}/"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!({ "response": "accepted" })).unwrap()),
        204,
    );

    // 14. /v1/characters/{id}/mail/
    let created_mail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "subject": "Fleet Operations Directive",
                    "body": "Report to Jita 4-4 station immediately.",
                    "recipients": [
                        { "recipient_id": 90_000_002, "recipient_type": "character" }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let mail_id = created_mail.as_i64().expect("mail_id");

    let mail_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/{char_id}/mail/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(mail_list
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["mail_id"] == json!(mail_id)));
}
