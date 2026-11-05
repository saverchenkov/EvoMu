//! Unit and mutation-killing tests for `esi::sov_fw_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

#[test]
fn activity_defense_multiplier_sov_types_campaign_event_and_fw_war_pairs() {
    // Base ADM with 0/0/0 indices is 1.0
    assert_eq!(activity_defense_multiplier(0, 0, 0), 1.0);
    // Level 1 military (+0.6), level 2 industrial (+1.2), level 3 strategic (+0.8) -> 1.0 + 2.6 = 3.6
    assert_eq!(activity_defense_multiplier(1, 2, 3), 3.6);
    // Level 3 military (+1.7), level 4 industrial (+2.1), level 4 strategic (+0.9) -> 1.0 + 4.7 = 5.7
    assert_eq!(activity_defense_multiplier(3, 4, 4), 5.7);
    // Max levels (5/5/5 or >5) cap at 6.0 (1.0 + 2.5 + 2.5 + 1.0 = 7.0 -> clamped to 6.0)
    assert_eq!(activity_defense_multiplier(5, 5, 5), 6.0);
    assert_eq!(activity_defense_multiplier(99, 99, 99), 6.0);

    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let sov_types = sov_structure_type_ids(&ctx.sde);
    assert!(!sov_types.is_empty());

    // Find TCU vs IHub type IDs in sov_types
    let mut saw_tcu = false;
    let mut saw_ihub = false;
    for &tid in &sov_types {
        match sov_campaign_event_type(&ctx.sde, tid) {
            "tcu_defense" => saw_tcu = true,
            "ihub_defense" => saw_ihub = true,
            other => panic!("unexpected event type {other}"),
        }
    }
    assert!(saw_tcu || saw_ihub);
    assert_eq!(sov_campaign_event_type(&ctx.sde, 34), "ihub_defense");

    let pairs = sde_empire_fw_war_pairs(&ctx.sde);
    assert_eq!(pairs.len(), 4);
    assert!(pairs.contains(&(500_001, 500_004)));
    assert!(pairs.contains(&(500_004, 500_001)));
    assert!(pairs.contains(&(500_002, 500_003)));
    assert!(pairs.contains(&(500_003, 500_002)));
}

#[test]
fn dispatch_sov_map_structures_campaigns_incursions_and_fw_wars() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            9_001_001,
            eve_db::UpwellStructureRecord {
                structure_id: 9_001_001,
                type_id: 32_226,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test TCU".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ArmorReinforced".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                reinforced_until_unix: 1_700_003_600,
                ..Default::default()
            },
        );
        snap.sov_campaign_scores.insert(
            9_001_001,
            eve_db::SovCampaignScoreRecord {
                structure_id: 9_001_001,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.incursions.insert(
            20_000_020,
            eve_db::IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.75,
                state: "Mobilizing".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );
    });

    assert!(dispatch_sov_route(&ctx, "POST", "/sovereignty/map").is_none());
    assert!(dispatch_sov_route(&ctx, "GET", "/unknown/sov").is_none());

    // /sovereignty/map
    let map_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/map").unwrap();
    assert_eq!(map_res.status_code, 200);
    let map_arr = map_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(!map_arr.is_empty());
    assert!(map_arr.iter().any(|e| e.get("faction_id").is_some() || e.get("alliance_id").is_some()));

    // /sovereignty/structures
    let st_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/structures").unwrap();
    assert_eq!(st_res.status_code, 200);
    let st_arr = st_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(!st_arr.is_empty());

    // /sovereignty/campaigns
    let camp_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/campaigns").unwrap();
    assert_eq!(camp_res.status_code, 200);
    let camp_arr = camp_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(!camp_arr.is_empty());
    assert!(camp_arr[0]["event_type"] == "tcu_defense" || camp_arr[0]["event_type"] == "ihub_defense");

    // /incursions (test state normalization for established/mobilizing/withdrawing)
    let inc_res = dispatch_sov_route(&ctx, "GET", "/incursions").unwrap();
    assert_eq!(inc_res.status_code, 200);
    let inc_arr = inc_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(!inc_arr.is_empty());
    assert!(
        ["established", "mobilizing", "withdrawing"]
            .contains(&inc_arr[0]["state"].as_str().unwrap())
    );

    // /fw/wars: default SDE pairs + custom fw_wars override
    let fw_res = dispatch_sov_route(&ctx, "GET", "/fw/wars").unwrap();
    assert_eq!(fw_res.status_code, 200);
    assert!(fw_res.body.as_ref().unwrap().as_array().unwrap().len() >= 4);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_wars = vec![(500_001, 500_002)];
    });
    let fw_custom = dispatch_sov_route(&ctx, "GET", "/fw/wars").unwrap();
    assert_eq!(
        fw_custom.body,
        Some(json!([{"faction_id": 500_001, "against_id": 500_002}]))
    );
}
