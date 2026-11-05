//! Unit and mutation-killing tests for `esi::fw_universe_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

#[test]
fn derive_star_stats_station_service_slugs_and_binary_parser_cover_all_arms() {
    assert!(parse_solarsystemcontent_stars(&[0u8; 4]).is_none());
    let mut empty_container = Vec::new();
    empty_container.extend_from_slice(&0u32.to_le_bytes());
    empty_container.extend_from_slice(&0u32.to_le_bytes());
    empty_container.extend_from_slice(&0u32.to_le_bytes());
    empty_container.extend_from_slice(&4u32.to_le_bytes());
    let parsed_empty = parse_solarsystemcontent_stars(&empty_container).expect("valid empty");
    assert!(parsed_empty.is_empty());

    for (type_name, expected_spec, min_temp) in [
        ("Sun A0 (Blue-White)", "A0", 9000),
        ("Sun A0IV (Subgiant)", "A0IV", 8800),
        ("Sun A0IV2 (Subgiant)", "A0IV2", 8800),
        ("Sun O1 (Blue)", "F1 V", 6800),
        ("Sun B0 (Blue)", "F1 V", 6800),
        ("Sun B5 (White-Blue)", "F6 V", 6000),
        ("Sun F0 (White)", "F0 V", 6700),
        ("Sun G3 (Yellow)", "G3 V", 5300),
        ("Sun G5 (Yellow)", "G5 V", 5200),
        ("Sun K3 (Orange)", "K3 V", 4300),
        ("Sun K5 (Orange)", "K5 V", 3900),
        ("Sun K7 (Orange-Red)", "K7 V", 3600),
        ("Sun M0 (Red Dwarf)", "M0 V", 3400),
        ("Unknown Star Type", "G5 V", 5200),
    ] {
        let (spec, temp, age) = derive_star_stats_from_sde(type_name, 1.0, 40_009_047);
        assert_eq!(spec, expected_spec, "mismatch for {type_name}");
        assert!(temp >= min_temp, "temp {temp} < {min_temp} for {type_name}");
        assert!(age >= 10_000_000_000_000_000);
    }

    assert_eq!(
        station_service_to_esi_slug("Assassination Missions"),
        "assasination-missions"
    );
    assert_eq!(station_service_to_esi_slug("Laboratory"), "labratory");
    assert_eq!(
        station_service_to_esi_slug("Security Office"),
        "security-offices"
    );
    assert_eq!(
        station_service_to_esi_slug(" Reprocessing Plant "),
        "reprocessing-plant"
    );

    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    assert!(station_services_for_operation(&ctx.sde, 999_999_999).is_empty());
    if let Some(&op_id) = ctx.sde.station_operations.keys().next() {
        let svcs = station_services_for_operation(&ctx.sde, op_id);
        assert!(!svcs.is_empty());
    }
}

#[test]
fn char_fw_stats_faction_and_rank_entries_cover_live_and_snapshot_windows() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .historical_char_totals
            .insert(char_id, (10, 500));
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            character_id: char_id,
            corporation_id: 98_000_001,
            alliance_id: None,
            faction_id: 500_001,
            kills: 2,
            victory_points: 100,
            timestamp_unix: now.saturating_sub(3600),
        });
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            character_id: char_id,
            corporation_id: 98_000_001,
            alliance_id: None,
            faction_id: 500_001,
            kills: 5,
            victory_points: 250,
            timestamp_unix: now.saturating_sub(3 * 86_400),
        });
    });

    ctx.db.with_snapshot(|snap| {
        let (kills, vp) = char_fw_stats(snap, char_id, now);
        assert_eq!(kills.yesterday, 2);
        assert_eq!(kills.last_week, 7);
        assert_eq!(kills.total, 17);
        assert_eq!(vp.yesterday, 100);
        assert_eq!(vp.last_week, 350);
        assert_eq!(vp.total, 850);
        assert_eq!(char_fw_faction(snap, char_id), Some(500_001));
    });

    let ranked = rank_entries(
        [100_i64, 200, 300].into_iter(),
        "id",
        |id| FwWindowedStat {
            yesterday: id,
            last_week: id * 2,
            total: id * 3,
        },
        Some(2),
    );
    assert_eq!(ranked["yesterday"].as_array().unwrap().len(), 2);
    assert_eq!(ranked["yesterday"][0]["id"], 300);
    assert_eq!(ranked["yesterday"][0]["amount"], 300);
    assert_eq!(ranked["active_total"][0]["amount"], 900);
}

#[test]
fn dispatch_meta_universe_killmail_and_fw_routes_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();

    assert!(
        dispatch_fw_universe_route(&ctx, "POST", "/meta/changelog", &empty_q, &empty_q).is_none()
    );

    for path in [
        "/meta/changelog",
        "/universe/graphics",
        "/universe/races",
        "/universe/system_jumps",
        "/universe/system_kills",
        "/fw/stats",
        "/fw/systems",
        "/fw/leaderboards",
        "/fw/leaderboards/characters",
        "/fw/leaderboards/corporations",
    ] {
        let res = dispatch_fw_universe_route(&ctx, "GET", path, &empty_q, &empty_q).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    let first_gid = ctx
        .sde
        .types
        .values()
        .find_map(|t| t.graphic_id)
        .unwrap();
    let g_res = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/universe/graphics/{graphic_id}",
        &params(&[("graphic_id", &first_gid.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(g_res.status_code, 200);
    assert_eq!(body(&g_res)["graphic_id"], first_gid as i64);

    let star_id = ctx
        .sde
        .celestials_in_system(30_000_142)
        .into_iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_047);
    let star_res = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/universe/stars/{star_id}",
        &params(&[("star_id", &star_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(star_res.status_code, 200);
    assert_eq!(body(&star_res)["solar_system_id"], 30_000_142);

    let st_res = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/universe/stations/{station_id}",
        &params(&[("station_id", "60003760")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(st_res.status_code, 200);
    assert_eq!(body(&st_res)["station_id"], 60_003_760);

    let expected_hash = ctx.db.with_snapshot(|snap| {
        let km = snap.killmails.get(&55_001).unwrap();
        compute_killmail_hash(
            km.kill_id,
            km.victim_character_id,
            km.attacker_character_id,
            km.ship_type_id,
        )
    });
    let km_ok = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/killmails/{killmail_id}/{killmail_hash}",
        &params(&[("killmail_id", "55001"), ("killmail_hash", &expected_hash)]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(km_ok.status_code, 200);
    assert_eq!(body(&km_ok)["killmail_id"], 55_001);

    let km_422 = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/killmails/{killmail_id}/{killmail_hash}",
        &params(&[("killmail_id", "55001"), ("killmail_hash", "wronghash")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(km_422.status_code, 422);

    let ch_fw = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/characters/{character_id}/fw/stats",
        &params(&[("character_id", &char_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(ch_fw.status_code, 200);

    let corp_fw = dispatch_fw_universe_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/fw/stats",
        &params(&[("corporation_id", &corp_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(corp_fw.status_code, 200);
}
