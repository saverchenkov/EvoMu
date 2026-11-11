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

#[test]
fn sov_map_structures_campaigns_and_fw_wars_cover_edge_branches() {
    let (ctx, _token, _char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let sov_types = sov_structure_type_ids(&ctx.sde);
    let ihub_tid = sov_types.iter().copied().next().expect("sov structure type");
    let now = ctx.now_unix();
    let day_mod = now % 86_400;

    let ally_corp_id = 98_555_001_i64;
    let no_ally_corp_id = 98_555_002_i64;
    let ally_id = 99_555_001_i64;
    let s_adm_wrap = 1_000_555_001_i64;
    let s_adm_prev_day = 1_000_555_002_i64;
    let s_onlining = 1_000_555_003_i64;
    let s_no_ally = 1_000_555_004_i64;
    let s_hull_camp = 1_000_555_005_i64;
    let s_no_score = 1_000_555_006_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "Sov Ally".to_string(),
                short_name: "SOV".to_string(),
                executor_corp_id: ally_corp_id,
                member_corp_ids: std::iter::once(ally_corp_id).collect(),
            },
        );
        snap.corporations.insert(
            ally_corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = ally_corp_id; __corp.name = "SovCorp".to_string(); __corp.ticker = "SOVC".to_string(); __corp.ceo_id = 90_555_001; __corp.tax_rate = 0.0; __corp.hq_station_id = None; __corp.alliance_id = Some(ally_id); __corp.members = Default::default(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    eve_db::IskCents(0),
                ); __corp.description = String::new(); __corp.url = String::new(); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.corporations.insert(
            no_ally_corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = no_ally_corp_id; __corp.name = "NoAllyCorp".to_string(); __corp.ticker = "NOAL".to_string(); __corp.ceo_id = 90_555_002; __corp.tax_rate = 0.0; __corp.hq_station_id = None; __corp.alliance_id = None; __corp.members = Default::default(); __corp.member_roles = Default::default(); __corp.applications = Default::default(); __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    eve_db::IskCents(0),
                ); __corp.description = String::new(); __corp.url = String::new(); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );

        let sys_state = snap
            .system_states
            .entry(30_000_142)
            .or_insert_with(|| eve_db::SystemDynamicState {
                solar_system_id: 30_000_142,
                ..Default::default()
            });
        sys_state.sov_alliance_id = Some(ally_id);
        sys_state.military_index = 2;
        sys_state.industrial_index = 2;
        sys_state.strategic_index = 2;

        snap.structures.insert(
            s_adm_wrap,
            eve_db::UpwellStructureRecord {
                structure_id: s_adm_wrap,
                type_id: ihub_tid,
                owner_corp_id: ally_corp_id,
                solar_system_id: 30_000_142,
                name: "Sov ADM Wrap".to_string(),
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            s_adm_prev_day,
            eve_db::UpwellStructureRecord {
                structure_id: s_adm_prev_day,
                type_id: ihub_tid,
                owner_corp_id: ally_corp_id,
                solar_system_id: 30_000_142,
                name: "Sov ADM CrossMidnight".to_string(),
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            s_onlining,
            eve_db::UpwellStructureRecord {
                structure_id: s_onlining,
                type_id: ihub_tid,
                owner_corp_id: ally_corp_id,
                solar_system_id: 30_000_142,
                name: "Sov Onlining".to_string(),
                state: "Onlining".to_string(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            s_no_ally,
            eve_db::UpwellStructureRecord {
                structure_id: s_no_ally,
                type_id: ihub_tid,
                owner_corp_id: no_ally_corp_id,
                solar_system_id: 30_000_144,
                name: "No Ally Struct".to_string(),
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            s_hull_camp,
            eve_db::UpwellStructureRecord {
                structure_id: s_hull_camp,
                type_id: ihub_tid,
                owner_corp_id: no_ally_corp_id,
                solar_system_id: 30_000_142,
                name: "Hull Camp".to_string(),
                state: "HullReinforced".to_string(),
                reinforced_until_unix: now + 3600,
                ..Default::default()
            },
        );
        snap.structures.insert(
            s_no_score,
            eve_db::UpwellStructureRecord {
                structure_id: s_no_score,
                type_id: ihub_tid,
                owner_corp_id: ally_corp_id,
                solar_system_id: 30_000_142,
                name: "No Score Camp".to_string(),
                state: "ArmorReinforced".to_string(),
                reinforced_until_unix: now + 3600,
                ..Default::default()
            },
        );

        // s_adm_wrap: ev <= day_mod -> triggers end_u <= now (+= 86_400)
        // s_adm_prev_day: ev <= sv -> triggers midnight-crossing branch
        let past_sv = if day_mod > 20 { day_mod - 15 } else { 0 };
        let past_ev = if day_mod > 20 { day_mod - 5 } else { 1 };
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(s_adm_wrap, (past_sv, past_ev));
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(s_adm_prev_day, (86_000, day_mod + 3600));
        snap.sov_runtime
            .command_node_scores
            .insert(s_hull_camp, vec![(1, 0.35)]);

        snap.fw_wars = vec![(500_001, 500_002)];
        snap.wars.insert(
            55_001,
            eve_db::WarRecord {
                war_id: 55_001,
                aggressor_corp_id: 500_003,
                defender_corp_id: 500_004,
                mutual: false,
                started_unix: now,
                active: true,
            },
        );
    });

    let map_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/map").unwrap();
    assert_eq!(map_res.status_code, 200);
    let m_entry = map_res
        .body
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["system_id"] == 30_000_142)
        .unwrap();
    assert_eq!(m_entry["alliance_id"], ally_id);
    assert_eq!(m_entry["corporation_id"], ally_corp_id);

    let st_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/structures").unwrap();
    assert_eq!(st_res.status_code, 200);
    let st_arr = st_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(!st_arr.iter().any(|s| s["structure_id"] == s_no_ally));
    let onl_entry = st_arr.iter().find(|s| s["structure_id"] == s_onlining).unwrap();
    assert!(onl_entry.get("vulnerable_start_time").is_none());
    let wrap_entry = st_arr.iter().find(|s| s["structure_id"] == s_adm_wrap).unwrap();
    assert!(wrap_entry.get("vulnerable_start_time").is_some());

    let camp_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/campaigns").unwrap();
    assert_eq!(camp_res.status_code, 200);
    let camp_arr = camp_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(camp_arr.iter().any(|c| c["structure_id"] == s_hull_camp && c["defender_id"] == ally_id));
    assert!(!camp_arr.iter().any(|c| c["structure_id"] == s_no_score));

    let fw_res = dispatch_sov_route(&ctx, "GET", "/fw/wars").unwrap();
    assert_eq!(fw_res.status_code, 200);
    let fw_arr = fw_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(fw_arr
        .iter()
        .any(|p| p["faction_id"] == 500_003 && p["against_id"] == 500_004));
    assert!(fw_arr
        .iter()
        .any(|p| p["faction_id"] == 500_004 && p["against_id"] == 500_003));
}
