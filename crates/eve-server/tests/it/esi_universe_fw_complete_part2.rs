//! Part 2 of `ESI-3` Universe Celestials, Stations/Structures, and System Jumps/Kills acceptance tests
//! against the real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

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
