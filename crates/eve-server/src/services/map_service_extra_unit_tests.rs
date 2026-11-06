//! Deep branch-coverage and mutation-killing unit tests for `map_service`.

#![forbid(unsafe_code)]

use super::map_service::{
    handle, record_solar_system_jump, record_solar_system_visit, SolarSystemVisitRecord,
};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::mobile_deployables::{
    write_deployable_state_snap, MobileDeployableRecord, MobileDeployableRuntimeState,
};
use crate::services::pve_exploration::{
    with_pve_exploration_reg, EscalatingPathEntry, PveSiteInstance,
};
use crate::services::pve_special_sites::{with_pve_special_reg, IncursionTaleRecord};
use eve_db::{
    FwSystemOccupationRecord, InventoryFlag, KillmailDetailRecord, KillmailRecord, SystemJumpEvent,
    SystemKillEvent, UpwellStructureRecord,
};
use eve_proto::PyRep;
use eve_sde::AgentInfo;
use std::collections::HashMap;
use std::sync::Arc;

fn call_map_rpc(
    ctx: &crate::router::ServerContext,
    session: &eve_machonet::SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, eve_proto::BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, session, "map", method, args, &kw)
}

#[test]
fn visit_and_jump_recording_guards_and_legacy_deserialization_branches() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Invalid char_id (<= 0) or solar_system_id (== 0) must be no-ops
    record_solar_system_visit(&ctx, 0, sys_id);
    record_solar_system_visit(&ctx, -5, sys_id);
    record_solar_system_visit(&ctx, char_id, 0);
    record_solar_system_jump(&ctx, 0, sys_id);
    record_solar_system_jump(&ctx, -1, sys_id);
    record_solar_system_jump(&ctx, char_id, 0);

    ctx.db.with_snapshot(|snap| {
        let key = format!("char_system_visits:{char_id}");
        assert!(!snap.service_state_blobs.contains_key(&key));
        assert!(snap.system_jump_events.is_empty());
    });

    // Seed LegacyCount JSON integer and Full record with last_visited_ticks = 123_456
    ctx.db.with_mut_snapshot(|snap| {
        let key = format!("char_system_visits:{char_id}");
        snap.service_state_blobs.insert(
            key,
            r#"{"30000142": 7, "30000144": {"visits": 3, "last_visited_ticks": 123456}}"#.into(),
        );
    });

    let visits_rep = call_map_rpc(&ctx, &session, "GetSolarSystemVisits", &[])
        .unwrap()
        .unwrap();
    let PyRep::List(items) = visits_rep else {
        panic!("expected List");
    };
    assert_eq!(items.len(), 2);
    let mut by_sys = HashMap::new();
    for item in items {
        let sid = item.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap();
        let v = item.dict_get("visits").and_then(|v| v.as_i64()).unwrap();
        let dt = item.dict_get("lastDateTime").and_then(|v| v.as_i64()).unwrap();
        by_sys.insert(sid, (v, dt));
    }
    assert_eq!(by_sys[&30_000_142].0, 7);
    assert!(by_sys[&30_000_142].1 > 1_000_000);
    assert_eq!(by_sys[&30_000_144], (3, 123_456));

    // Missing character_id on session -> error
    session.character_id = None;
    assert!(call_map_rpc(&ctx, &session, "GetSolarSystemVisits", &[])
        .unwrap()
        .is_err());
    assert!(call_map_rpc(&ctx, &session, "GetMyExtraMapInfo", &[])
        .unwrap()
        .is_err());
    assert!(call_map_rpc(&ctx, &session, "GetMyExtraMapInfoAgents", &[])
        .unwrap()
        .is_err());

    let _ = SolarSystemVisitRecord::default();
}

#[test]
fn top_level_service_auth_abyssal_bounds_and_unknown_method_guards() {
    let (ctx, mut session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // Non-"map" service returns None
    assert!(handle(&ctx, &session, "charMgr", "GetStationCount", &[], &kw).is_none());

    // Unknown method on "map" returns None
    let unmapped_method = format!("{}_{}", "unmapped", "op");
    assert!(call_map_rpc(&ctx, &session, &unmapped_method, &[]).is_none());

    // Abyssal space exact boundaries: 32_000_000..=32_999_999 blocked, outside allowed
    for abyssal_sys in [32_000_000_u32, 32_500_000, 32_999_999] {
        session.solar_system_id = Some(abyssal_sys);
        let err = call_map_rpc(&ctx, &session, "GetStationCount", &[])
            .unwrap()
            .unwrap_err();
        assert!(err.code.contains("Abyssal"));
    }
    for non_abyssal_sys in [31_999_999_u32, 33_000_000] {
        session.solar_system_id = Some(non_abyssal_sys);
        assert!(call_map_rpc(&ctx, &session, "GetStationCount", &[])
            .unwrap()
            .is_ok());
    }

    // Unauthenticated session returns Err
    session.user_id = None;
    assert!(call_map_rpc(&ctx, &session, "GetStationCount", &[])
        .unwrap()
        .is_err());
}

#[test]
fn get_beacon_count_distinguishes_structures_modules_and_mobile_cyno_states() {
    let (ctx, session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();
    let other_sys = 30_000_144_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.clear();
        snap.items.clear();

        // Active Pharolux Cyno Beacon (35840) in sys_id -> structure count +1
        snap.structures.insert(
            1001,
            UpwellStructureRecord {
                structure_id: 1001,
                type_id: 35_840,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        // Destroyed Pharolux Cyno Beacon (35840) -> ignored
        snap.structures.insert(
            1002,
            UpwellStructureRecord {
                structure_id: 1002,
                type_id: 35_840,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        // Ansiblex Jump Gate (35841) -> ignored
        snap.structures.insert(
            1003,
            UpwellStructureRecord {
                structure_id: 1003,
                type_id: 35_841,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );

        // Cyno field item types 21096, 49746, 56099 in solar system -> module count +3
        snap.items.insert(
            2001,
            make_fighter_item(2001, 21_096, char_id, sys_id as i64, InventoryFlag::Unlocked, 1),
        );
        snap.items.insert(
            2002,
            make_fighter_item(2002, 49_746, char_id, sys_id as i64, InventoryFlag::Unlocked, 1),
        );
        snap.items.insert(
            2003,
            make_fighter_item(2003, 56_099, char_id, sys_id as i64, InventoryFlag::Unlocked, 1),
        );
        // Item outside solar system ID range (station 60_003_760) -> ignored
        snap.items.insert(
            2004,
            make_fighter_item(2004, 21_096, char_id, 60_003_760, InventoryFlag::Hangar, 1),
        );

        // Mobile Cyno Beacon (57319) active via anchor_completed in other_sys -> +1
        snap.items.insert(
            2005,
            make_fighter_item(2005, 57_319, char_id, other_sys as i64, InventoryFlag::Unlocked, 1),
        );
        // Mobile Cyno Beacon (58906) active via anchor_start_unix + anchor_duration_secs <= now in other_sys -> +1
        snap.items.insert(
            2006,
            make_fighter_item(2006, 58_906, char_id, other_sys as i64, InventoryFlag::Unlocked, 1),
        );
        // Mobile Cyno Beacon (57319) unanchored -> ignored
        snap.items.insert(
            2007,
            make_fighter_item(2007, 57_319, char_id, other_sys as i64, InventoryFlag::Unlocked, 1),
        );
        // Mobile Cyno Beacon (57319) expired -> ignored
        snap.items.insert(
            2008,
            make_fighter_item(2008, 57_319, char_id, other_sys as i64, InventoryFlag::Unlocked, 1),
        );

        let mut blob = MobileDeployableRuntimeState::default();
        blob.deployables.insert(
            2005,
            MobileDeployableRecord {
                item_id: 2005,
                type_id: 57_319,
                group_id: 4093,
                owner_id: char_id,
                solar_system_id: other_sys,
                position: [0.0, 0.0, 0.0],
                anchor_start_unix: now.saturating_sub(120),
                anchor_duration_secs: 60,
                anchor_completed: true,
                expires_at_unix: now + 3600,
                is_unanchored: false,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                deployed_fleet_id: None,
            },
        );
        blob.deployables.insert(
            2006,
            MobileDeployableRecord {
                item_id: 2006,
                type_id: 58_906,
                group_id: 4093,
                owner_id: char_id,
                solar_system_id: other_sys,
                position: [1000.0, 0.0, 0.0],
                anchor_start_unix: now.saturating_sub(60),
                anchor_duration_secs: 60,
                anchor_completed: false,
                expires_at_unix: now + 3600,
                is_unanchored: false,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                deployed_fleet_id: None,
            },
        );
        blob.deployables.insert(
            2007,
            MobileDeployableRecord {
                item_id: 2007,
                type_id: 57_319,
                group_id: 4093,
                owner_id: char_id,
                solar_system_id: other_sys,
                position: [2000.0, 0.0, 0.0],
                anchor_start_unix: now.saturating_sub(120),
                anchor_duration_secs: 60,
                anchor_completed: true,
                expires_at_unix: now + 3600,
                is_unanchored: true,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                deployed_fleet_id: None,
            },
        );
        blob.deployables.insert(
            2008,
            MobileDeployableRecord {
                item_id: 2008,
                type_id: 57_319,
                group_id: 4093,
                owner_id: char_id,
                solar_system_id: other_sys,
                position: [3000.0, 0.0, 0.0],
                anchor_start_unix: now.saturating_sub(120),
                anchor_duration_secs: 60,
                anchor_completed: true,
                expires_at_unix: now,
                is_unanchored: false,
                last_interaction_unix: now,
                reinforced_until_unix: None,
                last_ping_unix: None,
                deployed_fleet_id: None,
            },
        );
        write_deployable_state_snap(snap, |st| {
            *st = blob;
        });
    });

    let rep = call_map_rpc(&ctx, &session, "GetBeaconCount", &[])
        .unwrap()
        .unwrap();
    let PyRep::Dict(entries) = rep else {
        panic!("expected Dict");
    };
    let map: HashMap<i64, (i64, i64)> = entries
        .into_iter()
        .filter_map(|(k, v)| {
            let sid = k.as_i64()?;
            if let PyRep::Tuple(t) = v {
                Some((sid, (t.first()?.as_i64()?, t.get(1)?.as_i64()?)))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(map.get(&(sys_id as i64)), Some(&(3, 1)));
    assert_eq!(map.get(&(other_sys as i64)), Some(&(2, 0)));
}

#[test]
fn deadspace_maps_incursions_extra_info_and_history_stat_types_cover_all_branches() {
    let (mut ctx, session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Seed SDE agent & NPC ship type (category 11)
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.agents.insert(
        3_019_001,
        AgentInfo {
            agent_id: 3_019_001,
            agent_type_id: 2,
            corporation_id: 1_000_125,
            division_id: 24,
            level: 4,
            station_id: Some(60_003_760),
            solar_system_id: Some(sys_id),
            name: "Agent Test".into(),
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    sde_mut
        .types
        .insert(990_111, make_fighter_type(990_111, 500, 11, 10.0, vec![]));

    // 1. Seed escalation & exploration sites
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.insert(
            char_id,
            vec![EscalatingPathEntry {
                instance_id: 1,
                character_id: char_id,
                source_dungeon_id: 100,
                expedition_dungeon_id: 555,
                expedition_name: "Guristas Scout Outpost".into(),
                destination_solar_system_id: sys_id,
                step_index: 0,
                total_steps: 4,
                journal_text: "Escalation".into(),
                created_at_sec: now,
                expires_at_sec: now + 3600,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 0,
            }],
        );
        reg.sites_by_system.insert(
            sys_id,
            vec![
                PveSiteInstance {
                    site_id: "ABC-123".into(),
                    solar_system_id: sys_id,
                    dungeon_id: 777,
                    dungeon_name: "Hideaway".into(),
                    ded_rating: Some(4),
                    cleared: false,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DEF-456".into(),
                    solar_system_id: sys_id,
                    dungeon_id: 778,
                    dungeon_name: "Cleared Site".into(),
                    ded_rating: None,
                    cleared: true,
                    ..Default::default()
                },
            ],
        );
    });

    let agents_map = call_map_rpc(&ctx, &session, "GetDeadspaceAgentsMap", &[])
        .unwrap()
        .unwrap();
    assert!(format!("{agents_map:?}").contains("Guristas Scout Outpost"));

    let complex_map = call_map_rpc(&ctx, &session, "GetDeadspaceComplexMap", &[])
        .unwrap()
        .unwrap();
    let complex_dbg = format!("{complex_map:?}");
    assert!(complex_dbg.contains("Hideaway"));
    assert!(!complex_dbg.contains("Cleared Site"));

    // 2. Seed active and Ended incursions
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            20_000_020,
            IncursionTaleRecord {
                tale_id: 9001,
                constellation_id: 20_000_020,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: 30_000_142,
                vanguard_solar_system_ids: vec![30_000_140],
                assault_solar_system_ids: vec![30_000_141],
                hq_solar_system_id: 30_000_144,
                influence: 0.75,
                state: "Mobilizing".into(),
                mothership_spawned: true,
                mothership_defeated: false,
                sites: Vec::new(),
                lockout_until_filetime: Some(133_000_000_000_000_000),
                tale_stop_published: false,
            },
        );
        reg.incursions.insert(
            20_000_021,
            IncursionTaleRecord {
                tale_id: 9002,
                constellation_id: 20_000_021,
                constellation_name: "Okomon".into(),
                staging_solar_system_id: 30_000_150,
                vanguard_solar_system_ids: vec![],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: 30_000_151,
                influence: 0.0,
                state: "Ended".into(),
                mothership_spawned: true,
                mothership_defeated: true,
                sites: Vec::new(),
                lockout_until_filetime: None,
                tale_stop_published: true,
            },
        );
    });

    let inc_rep = call_map_rpc(&ctx, &session, "GetIncursionGlobalReport", &[])
        .unwrap()
        .unwrap();
    assert!(format!("{inc_rep:?}").contains("Kimotoro"));
    assert!(!format!("{inc_rep:?}").contains("Okomon"));

    let inc_sys = call_map_rpc(&ctx, &session, "GetSystemsInIncursions", &[])
        .unwrap()
        .unwrap();
    if let PyRep::List(list) = inc_sys {
        assert!(list.len() >= 4);
    } else {
        panic!("expected List");
    }

    // 3. Seed jump events, kill events, killmails (pod, NPC, player ship, detail timestamp fallback), and standings
    ctx.db.with_mut_snapshot(|snap| {
        let mut other_char = snap.characters[&char_id].clone();
        other_char.character_id = 90_000_099;
        other_char.corporation_id = 99_999_999;
        other_char.solar_system_id = sys_id;
        snap.characters.insert(90_000_099, other_char);

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(3_019_001, 6.5);
            ch.standings.insert(9_999_999, 4.0); // non-agent excluded
        }
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: sys_id,
            character_id: char_id,
            timestamp_unix: now,
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: sys_id,
            ship_kills: 2,
            npc_kills: 5,
            pod_kills: 1,
            timestamp_unix: now,
        });
        // Player ship killmail with killed_unix = 0 falling back to killmail_details
        snap.killmails.insert(
            501,
            KillmailRecord {
                kill_id: 501,
                killed_unix: 0,
                solar_system_id: sys_id,
                victim_character_id: char_id,
                victim_corp_id: 98_000_001,
                victim_alliance_id: None,
                ship_type_id: 587,
                attacker_character_id: 90_000_002,
                attacker_corp_id: 1_000_125,
                attacker_alliance_id: None,
                total_value_isk: 500_000.0,
            },
        );
        snap.killmail_details.insert(
            501,
            KillmailDetailRecord {
                kill_id: 501,
                killmail_time_unix: now,
                damage_taken: 1_000,
                victim_position: None,
                attackers: Vec::new(),
            },
        );
        // NPC victim killmail (category 11)
        snap.killmails.insert(
            502,
            KillmailRecord {
                kill_id: 502,
                killed_unix: now,
                solar_system_id: sys_id,
                victim_character_id: 0,
                victim_corp_id: 1_000_125,
                victim_alliance_id: None,
                ship_type_id: 990_111,
                attacker_character_id: char_id,
                attacker_corp_id: 98_000_001,
                attacker_alliance_id: None,
                total_value_isk: 10_000.0,
            },
        );
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                occupier_faction_id: 500_002,
                owner_faction_id: 500_003,
                victory_points: 1_200,
                victory_points_threshold: 3_000,
            },
        );
    });

    for stat_type in [1, 2, 3, 4, 5, 99] {
        let h = call_map_rpc(
            &ctx,
            &session,
            "GetHistory",
            &[PyRep::Integer(stat_type), PyRep::Integer(24)],
        )
        .unwrap()
        .unwrap();
        assert!(format!("{h:?}").contains(&sys_id.to_string()));
    }

    // FacWarZoneInfo for Minmatar (500_002) and custom faction (500_099)
    for fac in [500_002, 500_099] {
        let fw = call_map_rpc(&ctx, &session, "GetFacWarZoneInfo", &[PyRep::Integer(fac)])
            .unwrap()
            .unwrap();
        assert_eq!(fw.dict_get("factionID").and_then(|v| v.as_i64()), Some(fac));
    }

    let ag_st = call_map_rpc(&ctx, &session, "GetMyExtraMapInfoAgents", &[])
        .unwrap()
        .unwrap();
    assert!(format!("{ag_st:?}").contains("3019001"));
    assert!(!format!("{ag_st:?}").contains("9999999"));
}
