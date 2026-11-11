//! Edge-case and branch-coverage unit tests for `esi::sov_fw_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

#[test]
fn sov_map_covers_alliance_with_and_without_executor_and_faction_fallback() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let alliance_id = 99_000_001_i64;
    let orphan_alliance_id = 99_000_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut sys_iter = ctx.sde.solar_systems.keys().copied();
        let sys1 = sys_iter.next().unwrap_or(30_000_142);
        let sys2 = sys_iter.next().unwrap_or(30_000_144);
        snap.system_states
            .entry(sys1)
            .or_default()
            .sov_alliance_id = Some(alliance_id);
        snap.system_states
            .entry(sys2)
            .or_default()
            .sov_alliance_id = Some(orphan_alliance_id);
        // Ensure alliances has alliance_id -> corp_id, while orphan_alliance_id is absent
        if let Some(al) = snap.alliances.get_mut(&alliance_id) {
            al.executor_corp_id = corp_id;
        }
        snap.alliances.remove(&orphan_alliance_id);
    });

    let res = dispatch_sov_route(&ctx, "GET", "/sovereignty/map").unwrap();
    assert_eq!(res.status_code, 200);
    let arr = res.body.as_ref().unwrap().as_array().unwrap();
    let mut saw_with_exec = false;
    let mut saw_orphan_alliance = false;
    let mut saw_faction = false;
    for entry in arr {
        if entry.get("alliance_id") == Some(&json!(alliance_id))
            && entry.get("corporation_id") == Some(&json!(corp_id))
        {
            saw_with_exec = true;
        } else if entry.get("alliance_id") == Some(&json!(orphan_alliance_id))
            && entry.get("corporation_id").is_none()
        {
            saw_orphan_alliance = true;
        } else if entry.get("faction_id").is_some() {
            saw_faction = true;
        }
    }
    assert!(saw_with_exec);
    assert!(saw_orphan_alliance);
    assert!(saw_faction);
}

#[test]
fn sov_structures_covers_adm_windows_command_nodes_onlining_and_fallbacks() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();
    let sov_types = sov_structure_type_ids(&ctx.sde);
    let sov_tid = *sov_types.iter().next().expect("at least one sov type");

    let non_alliance_corp = 98_999_888_i64;
    ctx.db.with_mut_snapshot(|snap| {
        // Corp with no alliance -> structure should be skipped
        let mut c2 = snap.corporations.get(&corp_id).unwrap().clone();
        c2.corporation_id = non_alliance_corp;
        c2.alliance_id = None;
        snap.corporations.insert(non_alliance_corp, c2);

        // System with ADM indices
        let st_sys = snap.system_states.entry(30_000_142).or_default();
        st_sys.military_index = 2;
        st_sys.industrial_index = 1;
        st_sys.strategic_index = 1;

        // 1. Destroyed -> filtered out
        snap.structures.insert(
            9_100_001,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_001,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "Destroyed".to_string(),
                ..Default::default()
            },
        );
        // 2. Non-sov type -> filtered out
        snap.structures.insert(
            9_100_002,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_002,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        // 3. Non-alliance owner -> skipped
        snap.structures.insert(
            9_100_003,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_003,
                type_id: sov_tid,
                owner_corp_id: non_alliance_corp,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        // 4. Onlining -> window = None
        snap.structures.insert(
            9_100_004,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_004,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "Onlining".to_string(),
                ..Default::default()
            },
        );
        // 5. CommandNodeEvent -> window = None
        snap.structures.insert(
            9_100_005,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_005,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "CommandNodeEvent".to_string(),
                ..Default::default()
            },
        );
        // 6. Active command node in sov_runtime -> window = None
        snap.structures.insert(
            9_100_006,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_006,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .command_nodes
            .insert(1, (9_100_006, 30_000_142, 0));

        // 7. adm_vulnerability_windows with ev > sv and end_u <= now (shifts +86_400)
        snap.structures.insert(
            9_100_007,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_007,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        let tod = now % 86_400;
        let sv7 = if tod >= 7200 { tod - 7200 } else { 0 };
        let ev7 = sv7 + 3600;
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(9_100_007, (sv7, ev7));

        // 8. adm_vulnerability_windows wrapping midnight (ev <= sv) with tod < e_mod
        snap.structures.insert(
            9_100_008,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_008,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        // Set e_mod > tod and sv > ev so it enters the wrap branch where (now % 86400) < e_mod
        let e8 = (tod + 1800).min(86_399);
        let s8 = (e8 + 1000).min(86_399);
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(9_100_008, (s8, e8));

        // 9. adm_vulnerability_windows wrapping midnight (ev <= sv) with tod >= e_mod
        snap.structures.insert(
            9_100_009,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_009,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        let e9 = tod.saturating_sub(600);
        let s9 = (e9 + 1000).min(86_399);
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(9_100_009, (s9, e9));

        // 10. adm_vulnerability_windows (0, 0) falling back to structure_vulnerability_windows (valid)
        snap.structures.insert(
            9_100_010,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_010,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(9_100_010, (0, 0));
        snap.structure_vulnerability_windows
            .insert(9_100_010, (1_700_000_000, 1_700_010_000));

        // 11. Invalid fallback structure_vulnerability_windows (v_start <= 24) -> window = None
        snap.structures.insert(
            9_100_011,
            eve_db::UpwellStructureRecord {
                structure_id: 9_100_011,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.structure_vulnerability_windows
            .insert(9_100_011, (10, 20));
    });

    let res = dispatch_sov_route(&ctx, "GET", "/sovereignty/structures").unwrap();
    assert_eq!(res.status_code, 200);
    let arr = res.body.as_ref().unwrap().as_array().unwrap();
    let by_id: std::collections::HashMap<i64, &Value> = arr
        .iter()
        .map(|v| (v["structure_id"].as_i64().unwrap(), v))
        .collect();

    assert!(!by_id.contains_key(&9_100_001));
    assert!(!by_id.contains_key(&9_100_002));
    assert!(!by_id.contains_key(&9_100_003));
    assert!(by_id[&9_100_004].get("vulnerable_start_time").is_none());
    assert!(by_id[&9_100_005].get("vulnerable_start_time").is_none());
    assert!(by_id[&9_100_006].get("vulnerable_start_time").is_none());
    assert!(by_id[&9_100_007].get("vulnerable_start_time").is_some());
    assert!(by_id[&9_100_008].get("vulnerable_start_time").is_some());
    assert!(by_id[&9_100_009].get("vulnerable_start_time").is_some());
    assert!(by_id[&9_100_010].get("vulnerable_start_time").is_some());
    assert!(by_id[&9_100_011].get("vulnerable_start_time").is_none());
}

#[test]
fn sov_campaigns_incursions_and_fw_wars_cover_all_filters_and_fallbacks() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let sov_types = sov_structure_type_ids(&ctx.sde);
    let sov_tid = *sov_types.iter().min().expect("sov type");
    let no_alliance_corp = 98_888_777_i64;
    let npc_corp_id = ctx
        .sde
        .factions
        .values()
        .filter(|f| f.faction_id != 500_001 && f.faction_id != 500_010)
        .filter_map(|f| f.militia_corporation_id.map(|cid| (cid, f.faction_id as i64)))
        .min_by_key(|&(cid, fid)| (fid, cid));
    let militia_faction_id = npc_corp_id.map(|(_, fid)| fid);
    let npc_only_corp = ctx
        .sde
        .npc_corporations
        .values()
        .filter_map(|nc| nc.faction_id.map(|fid| (nc.corporation_id, fid as i64)))
        .filter(|&(_, fid)| {
            fid != 500_001 && fid != 500_010 && Some(fid) != militia_faction_id
        })
        .min_by_key(|&(cid, fid)| (fid, cid));

    ctx.db.with_mut_snapshot(|snap| {
        let mut c2 = snap.corporations.get(&corp_id).unwrap().clone();
        c2.corporation_id = no_alliance_corp;
        c2.alliance_id = None;
        snap.corporations.insert(no_alliance_corp, c2);

        // Campaign 1: uses command_node_scores + system_states.sov_alliance_id fallback
        snap.system_states
            .entry(30_000_142)
            .or_default()
            .sov_alliance_id = Some(99_000_001);
        snap.structures.insert(
            9_200_001,
            eve_db::UpwellStructureRecord {
                structure_id: 9_200_001,
                type_id: sov_tid,
                owner_corp_id: no_alliance_corp,
                solar_system_id: 30_000_142,
                state: "CommandNodeEvent".to_string(),
                reinforced_until_unix: 1_700_005_000,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .command_node_scores
            .insert(9_200_001, vec![(1, 0.75)]);

        // Campaign 2: missing both alliance and system sov -> skipped
        snap.structures.insert(
            9_200_002,
            eve_db::UpwellStructureRecord {
                structure_id: 9_200_002,
                type_id: sov_tid,
                owner_corp_id: no_alliance_corp,
                solar_system_id: 30_000_144,
                state: "HullReinforced".to_string(),
                reinforced_until_unix: 1_700_005_000,
                ..Default::default()
            },
        );
        // Campaign 3: unknown solar_system_id -> skipped
        snap.structures.insert(
            9_200_003,
            eve_db::UpwellStructureRecord {
                structure_id: 9_200_003,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 99_999_999,
                state: "ArmorReinforced".to_string(),
                reinforced_until_unix: 1_700_005_000,
                ..Default::default()
            },
        );
        // Campaign 4: no score entry -> skipped
        snap.structures.insert(
            9_200_004,
            eve_db::UpwellStructureRecord {
                structure_id: 9_200_004,
                type_id: sov_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                state: "HullReinforced".to_string(),
                reinforced_until_unix: 1_700_005_000,
                ..Default::default()
            },
        );

        // Incursions covering Withdrawing, Mobilizing, and default -> established
        snap.incursions.insert(
            20_000_001,
            eve_db::IncursionRecord {
                constellation_id: 20_000_001,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142],
                faction_id: 500_019,
                has_boss: false,
                influence: 0.5,
                state: "Withdrawing".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );
        snap.incursions.insert(
            20_000_002,
            eve_db::IncursionRecord {
                constellation_id: 20_000_002,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.1,
                state: "OtherState".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );

        // Wars in snap.wars for /fw/wars resolution
        snap.fw_wars = vec![(500_001, 500_004)];
        snap.wars.insert(
            88001,
            eve_db::WarRecord {
                war_id: 88001,
                aggressor_corp_id: 500_002, // direct faction id
                defender_corp_id: 500_003,
                started_unix: 1000,
                mutual: false,
                active: true,
            },
        );
        if let Some((mcid, _)) = npc_corp_id {
            if let Some((ncid, _)) = npc_only_corp {
                snap.wars.insert(
                    88002,
                    eve_db::WarRecord {
                        war_id: 88002,
                        aggressor_corp_id: mcid,
                        defender_corp_id: ncid,
                        started_unix: 1000,
                        mutual: false,
                        active: true,
                    },
                );
            }
        }
        // Inactive war -> ignored
        snap.wars.insert(
            88003,
            eve_db::WarRecord {
                war_id: 88003,
                aggressor_corp_id: 500_001,
                defender_corp_id: 500_010,
                started_unix: 1000,
                mutual: false,
                active: false,
            },
        );
        // Same-faction war -> skipped
        snap.wars.insert(
            88004,
            eve_db::WarRecord {
                war_id: 88004,
                aggressor_corp_id: 500_001,
                defender_corp_id: 500_001,
                started_unix: 1000,
                mutual: false,
                active: true,
            },
        );
    });

    let camp_res = dispatch_sov_route(&ctx, "GET", "/sovereignty/campaigns").unwrap();
    assert_eq!(camp_res.status_code, 200);
    let camp_arr = camp_res.body.as_ref().unwrap().as_array().unwrap();
    let c1 = camp_arr
        .iter()
        .find(|c| c["structure_id"] == 9_200_001)
        .expect("campaign 1 present");
    assert_eq!(c1["defender_id"], 99_000_001);
    assert_eq!(c1["defender_score"], 0.75);
    assert_eq!(c1["attackers_score"], 0.25);
    assert!(!camp_arr.iter().any(|c| c["structure_id"] == 9_200_002));
    assert!(!camp_arr.iter().any(|c| c["structure_id"] == 9_200_003));
    assert!(!camp_arr.iter().any(|c| c["structure_id"] == 9_200_004));

    let inc_res = dispatch_sov_route(&ctx, "GET", "/incursions").unwrap();
    assert_eq!(inc_res.status_code, 200);
    let inc_arr = inc_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(inc_arr.iter().any(|i| i["state"] == "withdrawing"));
    assert!(inc_arr.iter().any(|i| i["state"] == "established"));

    let fw_res = dispatch_sov_route(&ctx, "GET", "/fw/wars").unwrap();
    assert_eq!(fw_res.status_code, 200);
    let fw_arr = fw_res.body.as_ref().unwrap().as_array().unwrap();
    assert!(fw_arr
        .iter()
        .any(|p| p["faction_id"] == 500_002 && p["against_id"] == 500_003));
    assert!(!fw_arr
        .iter()
        .any(|p| p["faction_id"] == 500_001 && p["against_id"] == 500_010));
    assert!(!fw_arr
        .iter()
        .any(|p| p["faction_id"] == 500_001 && p["against_id"] == 500_001));
}
