use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{
    FwSystemOccupationRecord, KillmailRecord, SystemDynamicState, UpwellStructureRecord,
};
use std::collections::HashMap;

#[test]
fn test_map_service_visits_jumps_and_all_rpc_handlers() {
    let (ctx, session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // Record visits and jumps
    record_solar_system_visit(&ctx, char_id, sys_id);
    record_solar_system_jump(&ctx, char_id, sys_id);

    // Seed beacons, FW occupation, sov state, and killmails
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_000_901,
            UpwellStructureRecord {
                structure_id: 1_000_000_000_901,
                type_id: 35_840, // Pharolux Cyno Beacon
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Test Cyno Beacon".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                occupier_faction_id: 500_001,
                owner_faction_id: 500_004,
                victory_points: 1500,
                victory_points_threshold: 3000,
            },
        );
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
        snap.killmails.insert(
            1,
            KillmailRecord {
                kill_id: 1,
                killed_unix: 1_700_000_000,
                solar_system_id: sys_id,
                victim_character_id: char_id,
                victim_corp_id: 98_000_001,
                victim_alliance_id: None,
                ship_type_id: 670, // Capsule
                attacker_character_id: 90_000_002,
                attacker_corp_id: 1_000_125,
                attacker_alliance_id: None,
                total_value_isk: 100_000.0,
            },
        );
    });

    for (method, args) in [
        ("GetBeaconCount", vec![]),
        ("GetConstellationLPData", vec![PyRep::Integer(20_000_020)]),
        ("GetDeadspaceAgentsMap", vec![PyRep::Integer(1)]),
        ("GetDeadspaceComplexMap", vec![PyRep::Integer(1)]),
        ("GetEdencomFortressSystems", vec![]),
        ("GetEdencomMinorVictorySystems", vec![]),
        ("GetFacWarZoneInfo", vec![PyRep::Integer(500_001)]),
        ("GetHistory", vec![PyRep::Integer(1), PyRep::Integer(24)]),
        ("GetHistory", vec![PyRep::Integer(2), PyRep::Integer(24)]),
        ("GetHistory", vec![PyRep::Integer(3), PyRep::Integer(24)]),
        ("GetHistory", vec![PyRep::Integer(5), PyRep::Integer(24)]),
        ("GetIncursionGlobalReport", vec![]),
        ("GetMyExtraMapInfo", vec![]),
        ("GetMyExtraMapInfoAgents", vec![]),
        ("GetRecentSovActivity", vec![]),
        ("GetSolarSystemVisits", vec![]),
        ("GetStationCount", vec![]),
        ("GetSystemsInIncursions", vec![]),
        ("GetTriglavianMinorVictorySystems", vec![]),
        ("GetAllRoamingWeatherSystems", vec![]),
    ] {
        let res = handle(&ctx, &session, "map", method, &args, &kw)
            .unwrap_or_else(|| panic!("expected Some for map.{method}"));
        assert!(res.is_ok(), "map.{method} failed: {:?}", res);
    }

    // Verify visit count is 2
    let visits = handle(&ctx, &session, "map", "GetSolarSystemVisits", &[], &kw)
        .unwrap()
        .unwrap();
    if let PyRep::List(list) = visits {
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].dict_get("visits").and_then(|v| v.as_i64()), Some(2));
    } else {
        panic!("expected list");
    }
}

#[cfg(test)]
#[path = "map_service_extra_unit_tests.rs"]
mod map_service_extra_unit_tests;
