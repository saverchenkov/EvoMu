//! Parity test suite for core RPC services batch (Part 3: `map`).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::common;
use eve_destiny::DVec3;
use eve_machonet::SessionState;
use eve_proto::{DBRowDescriptor, PyRep};
use eve_server::router::ServerContext;

fn as_list(rep: &PyRep) -> &[PyRep] {
    match rep {
        PyRep::List(ref l) => l.as_slice(),
        _ => panic!("expected List, got {rep:?}"),
    }
}

fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
    match rep {
        PyRep::Dict(ref d) => d.as_slice(),
        _ => panic!("expected Dict, got {rep:?}"),
    }
}

fn crowset_header_has_column(rep: &PyRep, col_name: &str) -> bool {
    if let PyRep::ObjectEx {
        is_type_2: true,
        header,
        ..
    } = rep
    {
        if let PyRep::Tuple(items) = header.as_ref() {
            if let Some(PyRep::Dict(dict)) = items.get(1) {
                for (k, v) in dict {
                    if k.as_str() == Some("header") {
                        if let Some(desc) = DBRowDescriptor::from_pyrep(v) {
                            return desc.column_names().iter().any(|c| c == col_name);
                        }
                    }
                }
            }
        }
    }
    false
}

#[tokio::test]
async fn test_rpc_map_svc_part1_batch() {
    // parity: rpc:map.GetBeaconCount, rpc:map.GetCurrentSovData, rpc:map.GetDeadspaceComplexMap, rpc:map.GetEdencomFortressSystems, rpc:map.GetEdencomMinorVictorySystems, rpc:map.GetFacWarZoneInfo, rpc:map.GetIncursionGlobalReport
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        // Seed Pharolux Cyno Beacon structure (type 35840) in system 30000142
        snap.structures.insert(
            77_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 77_000_001,
                type_id: 35840,
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_001,
                name: "Jita Pharolux Cyno Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // Seed Ansiblex Jump Gate (type 35841) in system 30000144 — must NOT be counted as a cyno beacon
        snap.structures.insert(
            77_000_002,
            eve_db::UpwellStructureRecord {
                structure_id: 77_000_002,
                type_id: 35841,
                solar_system_id: 30_000_144,
                owner_corp_id: 98_000_001,
                name: "Maurasi Ansiblex Jump Gate".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // Seed Mobile Cynosural Beacon (type 56099) deployed in space in system 30000142
        snap.items.insert(
            77_000_003,
            eve_db::InventoryItemRecord {
                item_id: 77_000_003,
                type_id: 56099,
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Seed sov system state with alliance
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "Northern Coalition".into(),
                short_name: "NC".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: HashSet::from([98_000_001]),
            },
        );
        snap.system_states.insert(
            30_000_142,
            eve_db::SystemDynamicState {
                solar_system_id: 30_000_142,
                sov_alliance_id: Some(99_000_001),
                ..Default::default()
            },
        );
    });
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. GetBeaconCount — counts Pharolux Cyno Beacon (35840) + Mobile Cyno Beacon (56099) in 30000142,
    // and excludes Ansiblex Jump Gate (35841) in 30000144.
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetBeaconCount", &[], &kw, None)
        .await
        .expect("GetBeaconCount");
    let beacons = as_dict(&res);
    let jita_count = beacons
        .iter()
        .find(|(k, _)| k.as_i64() == Some(30_000_142))
        .and_then(|(_, v)| v.as_i64())
        .expect("beacon count for 30000142");
    assert_eq!(jita_count, 2);
    let maurasi_count = beacons
        .iter()
        .find(|(k, _)| k.as_i64() == Some(30_000_144))
        .and_then(|(_, v)| v.as_i64())
        .unwrap_or(0);
    assert_eq!(
        maurasi_count, 0,
        "Ansiblex Jump Gate (35841) must not be counted by map.GetBeaconCount"
    );

    // 2. GetConstellationLPData
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetConstellationLPData",
            &[PyRep::Integer(20_000_020)],
            &kw,
            None,
        )
        .await
        .expect("GetConstellationLPData");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "solarSystemID"));

    // 3. GetCurrentSovData
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetCurrentSovData", &[], &kw, None)
        .await
        .expect("GetCurrentSovData");
    let sov_rows = as_list(&res);
    assert!(!sov_rows.is_empty());
    let row = &sov_rows[0];
    assert_eq!(
        row.dict_get("solarSystemID").and_then(PyRep::as_i64),
        Some(30_000_142)
    );
    assert_eq!(
        row.dict_get("allianceID").and_then(PyRep::as_i64),
        Some(99_000_001)
    );

    // 4. GetDeadspaceAgentsMap
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetDeadspaceAgentsMap",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDeadspaceAgentsMap");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "agentID"));
    let agent_rows = res.crowset_rows().expect("agent rows");
    assert!(!agent_rows.is_empty());

    // 5. GetDeadspaceComplexMap
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetDeadspaceComplexMap",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDeadspaceComplexMap");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "dungeonID"));

    // 6. GetEdencomFortressSystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetEdencomFortressSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetEdencomFortressSystems");
    assert!(matches!(&res, PyRep::List(_)));

    // 7. GetEdencomMinorVictorySystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetEdencomMinorVictorySystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetEdencomMinorVictorySystems");
    assert!(matches!(&res, PyRep::List(_)));

    // 8. GetFacWarZoneInfo
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetFacWarZoneInfo",
            &[PyRep::Integer(500_001)],
            &kw,
            None,
        )
        .await
        .expect("GetFacWarZoneInfo");
    let max_pts = res
        .dict_get("maxWarZonePoints")
        .and_then(PyRep::as_i64)
        .expect("maxWarZonePoints");
    assert_eq!(max_pts, 100_000);

    // 9. GetHistory
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetHistory", &[], &kw, None)
        .await
        .expect("GetHistory");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "solarSystemID"));

    // 10. GetIncursionGlobalReport
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetIncursionGlobalReport",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetIncursionGlobalReport");
    assert!(matches!(&res, PyRep::List(_)));
}

#[tokio::test]
async fn test_rpc_map_svc_part2_batch() {
    // parity: rpc:map.GetIndustryJobsOverLast24Hours, rpc:map.GetMyExtraMapInfo, rpc:map.GetMyExtraMapInfoAgents, rpc:map.GetRecentSovActivity, rpc:map.GetSecurityModifiedSystems, rpc:map.GetSolarSystemVisits, rpc:map.GetStationCount, rpc:map.GetStationInfo, rpc:map.GetSystemsInIncursions, rpc:map.GetTriglavianMinorVictorySystems
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    // Seed 300 stations across two regions (10000002 The Forge and 10000043 Domain) so we verify
    // map.GetStationInfo returns the full cluster station rowset without region filtering or 256-row truncation.
    for idx in 0..300i64 {
        let station_id = 60_900_000 + idx;
        let region_id = if idx < 150 { 10_000_002 } else { 10_000_043 };
        let solar_system_id = if idx < 150 { 30_000_142 } else { 30_002_187 };
        sde.stations.insert(
            station_id,
            eve_sde::StationInfo {
                station_id,
                solar_system_id,
                constellation_id: 20_000_020,
                region_id,
                station_type_id: 1529,
                name: format!("Cluster Test Station {idx}"),
                position: DVec3::ZERO,
                owner_corporation_id: 1_000_035,
                owner_faction_id: None,
                operation_id: 26,
                reprocessing_efficiency: 0.5,
                reprocessing_stations_take: 0.05,
                is_conquerable: false,
                orbit_id: None,
                has_clone_bay: true,
            },
        );
    }
    let expected_total_stations = sde.stations.len();
    assert!(expected_total_stations > 300);

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            77_001,
            eve_db::IndustryJobRecord {
                job_id: 77_001,
                installer_id: 90_000_001,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_001,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 5,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 1700000000,
            },
        );
    });

    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. GetIndustryJobsOverLast24Hours
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetIndustryJobsOverLast24Hours",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetIndustryJobsOverLast24Hours");
    if let PyRep::Rowset { columns, rows } = &res {
        assert!(columns.iter().any(|c| c == "solarSystemID"));
        assert!(columns.iter().any(|c| c == "noOfJobs"));
        assert!(!rows.is_empty());
        assert_eq!(rows[0][1].as_i64(), Some(5));
    } else {
        panic!("expected Rowset for GetIndustryJobsOverLast24Hours");
    }

    // 2. GetMyExtraMapInfo
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetMyExtraMapInfo", &[], &kw, None)
        .await
        .expect("GetMyExtraMapInfo");
    let extra_map = as_list(&res);
    assert!(!extra_map.is_empty());
    let my_cid = extra_map[0]
        .dict_get("characterID")
        .and_then(PyRep::as_i64)
        .expect("characterID");
    assert_eq!(my_cid, 90_000_001);

    // 3. GetMyExtraMapInfoAgents
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.insert(3011394, 5.0);
        }
    });

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetMyExtraMapInfoAgents",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyExtraMapInfoAgents");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "agentID"));
    let agent_rows = res.crowset_rows().expect("agents rows");
    assert_eq!(agent_rows.len(), 1);
    assert_eq!(
        agent_rows[0].dbrow_get("agentID").and_then(PyRep::as_i64),
        Some(3011394)
    );

    // 4. GetRecentSovActivity
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(30_000_142).or_default().sov_alliance_id = Some(99_000_001);
    });

    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetRecentSovActivity", &[], &kw, None)
        .await
        .expect("GetRecentSovActivity");
    let sov_act = as_list(&res);
    assert!(!sov_act.is_empty());
    let owner_id = sov_act[0]
        .dict_get("ownerID")
        .and_then(PyRep::as_i64)
        .expect("ownerID");
    assert_eq!(owner_id, 99_000_001);

    // 5. GetSecurityModifiedSystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetSecurityModifiedSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSecurityModifiedSystems");
    assert!(res.is_crowset());
    assert!(crowset_header_has_column(&res, "solarSystemID"));
    assert!(crowset_header_has_column(&res, "modifiedSecurity"));

    // 6. GetSolarSystemVisits
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("GetSolarSystemVisits");
    let visits = as_dict(&res);
    assert!(!visits.is_empty());
    let visit_count = visits[0].1.as_i64().expect("visit count int");
    assert_eq!(visit_count, 1);

    // 7. GetStationCount
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetStationCount", &[], &kw, None)
        .await
        .expect("GetStationCount");
    let stations = as_dict(&res);
    assert!(!stations.is_empty());
    let st_count = stations[0].1.as_i64().expect("station count int");
    assert!(st_count > 0);

    // 8. GetStationInfo — must return all stations across all regions without 256-row truncation
    let res = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetStationInfo", &[], &kw, None)
        .await
        .expect("GetStationInfo");
    if let PyRep::Rowset { columns, rows } = &res {
        assert!(columns.iter().any(|c| c == "stationID"));
        assert_eq!(
            rows.len(),
            expected_total_stations,
            "map.GetStationInfo must return all cluster stations without 256-row cap or region filter"
        );
        let region_idx = columns.iter().position(|c| c == "regionID").expect("regionID col");
        let regions: HashSet<i64> = rows.iter().filter_map(|r| r[region_idx].as_i64()).collect();
        assert!(regions.contains(&10_000_002));
        assert!(regions.contains(&10_000_043));
    } else {
        panic!("expected Rowset for GetStationInfo");
    }

    // 9. GetSystemsInIncursions
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetSystemsInIncursions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSystemsInIncursions");
    assert!(matches!(&res, PyRep::List(_)));

    // 10. GetTriglavianMinorVictorySystems
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetTriglavianMinorVictorySystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTriglavianMinorVictorySystems");
    assert!(matches!(&res, PyRep::List(_)));
}
