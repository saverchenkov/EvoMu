//! Unit tests for `structure_profiles_directory` (`structureDirectory` RPC handlers).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::UpwellStructureRecord;

#[test]
fn structure_directory_rpc_queries_filter_by_type_state_and_access() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 910_501_i64;
    let corp_id = 98_500_501_i64;
    let sys_id = 30_000_144_u32; // Perimeter (high-sec)
    let astra_id = 1_000_000_950_001_i64;
    let beacon_id = 1_000_000_950_002_i64;
    let anchoring_id = 1_000_000_950_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Dir Tester", corp_id, sys_id, 100_000_001);
        snap.characters.insert(char_id, ch);
        // Operational Astrahus (35832)
        snap.structures.insert(
            astra_id,
            UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Perimeter Citadel".into(),
                position: [1_000.0, 2_000.0, 3_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Operational Cyno Beacon (35840)
        snap.structures.insert(
            beacon_id,
            UpwellStructureRecord {
                structure_id: beacon_id,
                type_id: 35_840,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Cyno Beacon".into(),
                position: [5_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Anchoring structure (non-dockable state)
        snap.structures.insert(
            anchoring_id,
            UpwellStructureRecord {
                structure_id: anchoring_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Anchoring Citadel".into(),
                position: [9_000.0, 0.0, 0.0],
                state: "Anchoring".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 1_800_999_999,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // GetMyDockableStructures returns only the operational Astrahus (excludes FLEX beacon & Anchoring)
    let PyRep::List(dockable) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetMyDockableStructures",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(dockable, vec![PyRep::Integer(astra_id)]);

    // CheckMyDockingAccessToStructures filters and deduplicates
    let PyRep::List(checked) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "CheckMyDockingAccessToStructures",
        &[PyRep::List(vec![
            PyRep::Integer(astra_id),
            PyRep::Integer(astra_id),
            PyRep::Integer(beacon_id),
            PyRep::Integer(anchoring_id),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(checked, vec![PyRep::Integer(astra_id)]);

    // GetStructureMapData returns all 3 non-destroyed structures in system as 12-column CRowset rows
    let PyRep::List(map_data) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetStructureMapData",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(map_data.len(), 3);
    let mut astra_row_matched = false;
    for row in &map_data {
        let PyRep::List(cols) = row else {
            panic!("expected 12-column row list, got {row:?}");
        };
        assert_eq!(cols.len(), 12);
        if cols[2].as_i64() == Some(astra_id) {
            astra_row_matched = true;
            assert_eq!(cols[1].as_i64(), Some(35_832));
            assert_eq!(cols[3].as_str(), Some("Perimeter Citadel"));
            assert_eq!(cols[4].as_i64(), Some(i64::from(sys_id)));
            assert_eq!(cols[6], PyRep::Bool(false));
            assert_eq!(cols[7], PyRep::Real(1_000.0));
            assert_eq!(cols[8], PyRep::Real(2_000.0));
            assert_eq!(cols[9], PyRep::Real(3_000.0));
        }
    }
    assert!(astra_row_matched, "Astrahus row must be present in GetStructureMapData");

    // GetMyAccessibleOnlineCynoBeaconStructures & GetSolarSystemsWithBeacons
    let PyRep::Dict(beacons) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetMyAccessibleOnlineCynoBeaconStructures",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected dict");
    };
    assert!(beacons.iter().any(|(k, _)| k.as_i64() == Some(beacon_id)));

    let PyRep::List(beacon_sys) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetSolarSystemsWithBeacons",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert!(beacon_sys.contains(&PyRep::Integer(sys_id as i64)));

    // GetValidWarHQs includes operational high-sec Astrahus
    let PyRep::List(war_hqs) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetValidWarHQs",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert!(war_hqs.contains(&PyRep::Integer(astra_id)));
    assert!(!war_hqs.contains(&PyRep::Integer(beacon_id)));

    // GetMyCharacterStructures & GetMyCorporationStructures
    let PyRep::Dict(char_structs) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetMyCharacterStructures",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected dict");
    };
    assert!(char_structs.iter().any(|(k, _)| k.as_i64() == Some(astra_id)));

    let PyRep::Dict(corp_structs) = handle(
        &ctx,
        &mut session,
        "structureDirectory",
        "GetMyCorporationStructures",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!("expected dict");
    };
    assert!(corp_structs.iter().any(|(k, _)| k.as_i64() == Some(astra_id)));
}
