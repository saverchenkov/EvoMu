//! Edge-case and full-branch unit tests for `esi::fw_universe_routes`.

#![forbid(unsafe_code)]

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::snapshot_ext::{
    FwCharacterStatsRecord, FwSystemOccupationRecord, FwWindowedStat, KillmailAttackerDetail,
    KillmailDetailRecord, SystemJumpEvent, SystemKillEvent,
};
use eve_db::{
    DetailedKillmail, KillmailAttackerEntry, KillmailItemEntry, KillmailRecord,
};

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn build_synthetic_solarsystemcontent_blob() -> Vec<u8> {
    let mut buf = Vec::new();
    // Header: schema_len = 0, 4-byte reserved = 0 -> data_base = 8
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    // Record 0 (at off = 0): bf = 0 (no star bit 128) -> skipped
    let off0 = (buf.len() - 8) as u32;
    buf.extend_from_slice(&vec![0u8; 106]);

    // Record 1 (at off1): bf = 128, star_bf = 0 -> skipped at star_bf & 1 == 0
    let off1 = (buf.len() - 8) as u32;
    let rec1_start = buf.len();
    buf.resize(rec1_start + 106 + 3 * 4 + 24, 0);
    buf[rec1_start + 98..rec1_start + 106].copy_from_slice(&128u64.to_le_bytes());
    let star1_pos = rec1_start + 106 + 3 * 4;
    buf[star1_pos..star1_pos + 4].copy_from_slice(&40_000_001u32.to_le_bytes());

    // Record 2 (at off2): full valid star record
    let off2 = (buf.len() - 8) as u32;
    let rec2_start = buf.len();
    buf.resize(rec2_start + 106 + 3 * 4 + 24 + 33 + 4 + 4, 0);
    buf[rec2_start + 98..rec2_start + 106].copy_from_slice(&128u64.to_le_bytes());
    let star2_pos = rec2_start + 106 + 3 * 4;
    buf[star2_pos..star2_pos + 4].copy_from_slice(&40_000_002u32.to_le_bytes()); // star_id
    buf[star2_pos + 4..star2_pos + 8].copy_from_slice(&500_000u32.to_le_bytes()); // radius
    buf[star2_pos + 8..star2_pos + 12].copy_from_slice(&6u32.to_le_bytes()); // type_id
    buf[star2_pos + 12..star2_pos + 20].copy_from_slice(&1u64.to_le_bytes()); // star_bf
    let stats_pos = star2_pos + 24;
    buf[stats_pos..stats_pos + 4].copy_from_slice(&1.0e16_f32.to_le_bytes()); // age
    buf[stats_pos + 9..stats_pos + 13].copy_from_slice(&0.75_f32.to_le_bytes()); // luminosity
    buf[stats_pos + 17..stats_pos + 21].copy_from_slice(&5800.0_f32.to_le_bytes()); // temperature
    let spec_pos = stats_pos + 33;
    buf[spec_pos..spec_pos + 4].copy_from_slice(&4u32.to_le_bytes());
    buf[spec_pos + 4..spec_pos + 8].copy_from_slice(b"G2 V");

    // Record 3 (at off3): out-of-bounds offset -> continue
    let off3 = (buf.len() + 500) as u32;

    // Footer: count = 4, 4 * 12 bytes
    buf.extend_from_slice(&4u32.to_le_bytes());
    for (sys_id, off) in [
        (30_000_001u32, off0),
        (30_000_002u32, off1),
        (30_000_142u32, off2),
        (30_000_144u32, off3),
    ] {
        buf.extend_from_slice(&sys_id.to_le_bytes());
        buf.extend_from_slice(&off.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
    }
    let footer_bytes = 4u32 + 4 * 12;
    buf.extend_from_slice(&footer_bytes.to_le_bytes());
    buf
}

#[test]
fn parse_solarsystemcontent_stars_parses_valid_and_skips_incomplete_records() {
    let blob = build_synthetic_solarsystemcontent_blob();
    let parsed = parse_solarsystemcontent_stars(&blob).expect("parses cleanly");
    assert_eq!(parsed.len(), 1);
    let star = parsed.get(&40_000_002).expect("star present");
    assert_eq!(star.solar_system_id, 30_000_142);
    assert_eq!(star.type_id, 6);
    assert_eq!(star.radius, 500_000);
    assert_eq!(star.temperature, 5800);
    assert_eq!(star.spectral_class, "G2 V");

    // Invalid footer / underflow cases
    let bad_footer = vec![0u8; 20];
    let mut bad = bad_footer;
    bad[16..20].copy_from_slice(&999u32.to_le_bytes());
    assert!(parse_solarsystemcontent_stars(&bad).is_none());

    assert!(candidate_resfiles_dirs().len() >= 1);
}

#[test]
fn universe_routes_errors_jumps_kills_and_killmails_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let now = ctx.now_unix();

    // 400 and 404 checks for parameterized universe routes
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/graphics/{graphic_id}",
            &params(&[("graphic_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/graphics/{graphic_id}",
            &params(&[("graphic_id", "999999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/stars/{star_id}",
            &params(&[("star_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/stars/{star_id}",
            &params(&[("star_id", "999999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/stations/{station_id}",
            &params(&[("station_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/universe/stations/{station_id}",
            &params(&[("station_id", "999999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    // Seed dynamic station rental cost, jump events (inside & outside window), kill events (inside & outside),
    // capsule killmail + detailed killmail with dropped/destroyed items & attackers
    let capsule_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            ctx.sde
                .groups
                .get(&t.group_id)
                .is_some_and(|g| g.name == "Capsule")
        })
        .map(|t| t.type_id)
        .unwrap_or(670);

    let km_detail_id = 55_901_i64;
    let km_fallback_id = 55_902_i64;
    let km_detail_hash = compute_killmail_hash(km_detail_id, char_id, 90_000_002, capsule_tid);
    let km_fallback_hash = compute_killmail_hash(km_fallback_id, char_id, 90_000_002, 587);

    ctx.db.with_mut_snapshot(|snap| {
        snap.station_office_rental_costs
            .insert(60_003_760, 77_777.0);
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: 30_000_142,
            character_id: char_id,
            timestamp_unix: now.saturating_sub(100),
        });
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: 30_000_144,
            character_id: char_id,
            timestamp_unix: now.saturating_sub(7200), // outside 1h window
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: 30_000_142,
            ship_kills: 2,
            npc_kills: 5,
            pod_kills: 1,
            timestamp_unix: now.saturating_sub(200),
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: 30_000_144,
            ship_kills: 1,
            npc_kills: 0,
            pod_kills: 0,
            timestamp_unix: now.saturating_sub(8000), // outside 1h window
        });

        // Capsule killmail with KillmailDetailRecord + DetailedKillmail in combat_crimewatch_state
        snap.killmails.insert(
            km_detail_id,
            KillmailRecord {
                kill_id: km_detail_id,
                solar_system_id: 30_000_142,
                victim_character_id: char_id,
                attacker_character_id: 90_000_002,
                ship_type_id: capsule_tid,
                total_value_isk: 10_000.0,
                ..Default::default()
            },
        );
        snap.killmail_details.insert(
            km_detail_id,
            KillmailDetailRecord {
                kill_id: km_detail_id,
                killmail_time_unix: now.saturating_sub(100),
                damage_taken: 450,
                victim_position: Some([1.0, 2.0, 3.0]),
                attackers: vec![KillmailAttackerDetail {
                    character_id: 90_000_002,
                    corporation_id: corp_id,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 587,
                    weapon_type_id: 2185,
                    damage_done: 450,
                    final_blow: true,
                    security_status: 1.5,
                }],
            },
        );
        snap.combat_crimewatch_state.detailed_killmails.insert(
            km_detail_id,
            DetailedKillmail {
                kill_id: km_detail_id,
                kill_time_unix: now.saturating_sub(100),
                solar_system_id: 30_000_142,
                victim_character_id: char_id,
                victim_corporation_id: corp_id,
                victim_alliance_id: Some(99_000_001),
                victim_ship_type_id: capsule_tid,
                victim_damage_taken: 450.0,
                victim_position: [1.0, 2.0, 3.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: 90_000_002,
                    corporation_id: corp_id,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 587,
                    weapon_type_id: 2185,
                    damage_done: 450.0,
                    final_blow: true,
                    security_status: 1.5,
                }],
                dropped_items: vec![KillmailItemEntry {
                    type_id: 34,
                    flag: 5,
                    qty_dropped: 10,
                    qty_destroyed: 0,
                    singleton: false,
                }],
                destroyed_items: vec![KillmailItemEntry {
                    type_id: 35,
                    flag: 5,
                    qty_dropped: 0,
                    qty_destroyed: 5,
                    singleton: true,
                }],
                total_value_isk: 10_000.0,
                killmail_hash: km_detail_hash.clone(),
            },
        );

        // Killmail with empty detail.attackers -> uses dkm.attackers branch
        snap.killmails.insert(
            km_fallback_id,
            KillmailRecord {
                kill_id: km_fallback_id,
                solar_system_id: 30_000_142,
                victim_character_id: char_id,
                attacker_character_id: 90_000_002,
                ship_type_id: 587,
                total_value_isk: 500_000.0,
                ..Default::default()
            },
        );
        snap.combat_crimewatch_state.detailed_killmails.insert(
            km_fallback_id,
            DetailedKillmail {
                kill_id: km_fallback_id,
                kill_time_unix: now.saturating_sub(100),
                solar_system_id: 30_000_142,
                victim_character_id: char_id,
                victim_corporation_id: corp_id,
                victim_alliance_id: None,
                victim_ship_type_id: 587,
                victim_damage_taken: 1200.0,
                victim_position: [4.0, 5.0, 6.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: 90_000_002,
                    corporation_id: corp_id,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 587,
                    weapon_type_id: 2185,
                    damage_done: 1200.0,
                    final_blow: true,
                    security_status: 2.0,
                }],
                dropped_items: vec![],
                destroyed_items: vec![],
                total_value_isk: 500_000.0,
                killmail_hash: km_fallback_hash.clone(),
            },
        );
    });

    let st_res = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/universe/stations/{station_id}",
        &params(&[("station_id", "60003760")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&st_res)["office_rental_cost"], 77_777.0);

    let jumps = dispatch_fw_universe_route(&ctx, "GET", "/universe/system_jumps", &empty_q, &empty_q)
        .unwrap();
    assert_eq!(body(&jumps).as_array().unwrap().len(), 1);
    assert_eq!(body(&jumps)[0]["system_id"], 30_000_142);

    let kills = dispatch_fw_universe_route(&ctx, "GET", "/universe/system_kills", &empty_q, &empty_q)
        .unwrap();
    let jita_k = body(&kills)
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["system_id"] == 30_000_142)
        .unwrap();
    assert!(jita_k["pod_kills"].as_i64().unwrap() >= 2);

    assert_eq!(
        dispatch_fw_universe_route(&ctx, "GET", "/sovereignty/campaigns", &empty_q, &empty_q)
            .unwrap()
            .status_code,
        200
    );

    // Killmail errors: 400 id, 400 missing hash, 404 unknown id
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/killmails/{killmail_id}/{killmail_hash}",
            &params(&[("killmail_id", "bad"), ("killmail_hash", "h")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/killmails/{killmail_id}/{killmail_hash}",
            &params(&[("killmail_id", "55001")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/killmails/{killmail_id}/{killmail_hash}",
            &params(&[("killmail_id", "99999999"), ("killmail_hash", "h")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    let km1 = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/killmails/{killmail_id}/{killmail_hash}",
        &params(&[("killmail_id", &km_detail_id.to_string()), ("killmail_hash", &km_detail_hash)]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(km1.status_code, 200);
    assert_eq!(body(&km1)["victim"]["items"].as_array().unwrap().len(), 2);

    let km2 = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/killmails/{killmail_id}/{killmail_hash}",
        &params(&[("killmail_id", &km_fallback_id.to_string()), ("killmail_hash", &km_fallback_hash)]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(km2.status_code, 200);
    assert_eq!(body(&km2)["attackers"][0]["alliance_id"], 99_000_001);
}

#[test]
fn fw_systems_and_char_corp_fw_stats_cover_all_branches_and_errors() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            30_002_001,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_001,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3000,
            },
        );
        snap.fw_system_occupations.insert(
            30_002_002,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_002,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1500,
                victory_points_threshold: 3000,
            },
        );
        snap.fw_system_occupations.insert(
            30_002_003,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_003,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 3000,
                victory_points_threshold: 3000,
            },
        );

        // char_id uses fw_character_stats fallback (not fw_runtime), char 90_000_002 is NOT enlisted
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: None,
                current_rank: 3,
                highest_rank: 5,
                kills: FwWindowedStat {
                    yesterday: 1,
                    last_week: 2,
                    total: 3,
                },
                victory_points: FwWindowedStat {
                    yesterday: 10,
                    last_week: 20,
                    total: 30,
                },
            },
        );
        snap.fw_corp_enlisted_on.insert(corp_id, now.saturating_sub(1000));
        snap.fw_runtime.corp_war_factions.clear();
        snap.fw_runtime.direct_enlisted_chars.clear();
    });

    let sys_res = dispatch_fw_universe_route(&ctx, "GET", "/fw/systems", &empty_q, &empty_q).unwrap();
    let arr = body(&sys_res).as_array().unwrap();
    assert!(arr.iter().any(|s| s["solar_system_id"] == 30_002_001 && s["contested"] == "uncontested"));
    assert!(arr.iter().any(|s| s["solar_system_id"] == 30_002_002 && s["contested"] == "contested"));
    assert!(arr.iter().any(|s| s["solar_system_id"] == 30_002_003 && s["contested"] == "vulnerable"));

    // 400 and 404 on char & corp fw stats
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/characters/{character_id}/fw/stats",
            &params(&[("character_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/characters/{character_id}/fw/stats",
            &params(&[("character_id", "99999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/fw/stats",
            &params(&[("corporation_id", "bad")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fw_universe_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/fw/stats",
            &params(&[("corporation_id", "99999999")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );

    let ch1 = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/characters/{character_id}/fw/stats",
        &params(&[("character_id", &char_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&ch1)["faction_id"], 500_001);
    assert!(body(&ch1).get("enlisted_on").is_some());

    let ch2 = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/characters/{character_id}/fw/stats",
        &params(&[("character_id", "90000002")]),
        &empty_q,
    )
    .unwrap();
    assert!(body(&ch2).get("faction_id").is_none());

    let corp_res = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/fw/stats",
        &params(&[("corporation_id", &corp_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&corp_res)["faction_id"], 500_001);
    assert!(body(&corp_res).get("enlisted_on").is_some());

    let star_records = super::star_static_records(&ctx.sde);
    assert!(
        !star_records.is_empty()
            && star_records
                .iter()
                .all(|(&k, v)| k == v.star_id && v.solar_system_id > 0),
        "star_static_records should index stars by star_id with valid solar_system_id"
    );
}
