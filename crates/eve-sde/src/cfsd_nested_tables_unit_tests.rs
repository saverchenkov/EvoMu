//! Unit and mutation-killing tests for `cfsd_nested_tables`.

use super::*;

#[test]
fn cfsd_buf_bounds_utf8_and_decode_primitives_unions_lists_dicts() {
    let mut raw = vec![0u8; 64];
    raw[0] = 7;
    raw[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    raw[8..16].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    // Valid string at abs = 40 (rel = 8), length = 3 at 32..40
    raw[32..40].copy_from_slice(&3u64.to_le_bytes());
    raw[40..43].copy_from_slice(b"abc");
    // Invalid UTF-8 at abs = 56 (rel = 24), length = 2 at 48..56
    raw[48..56].copy_from_slice(&2u64.to_le_bytes());
    raw[56] = 0xFF;
    raw[57] = 0xFE;

    let buf = CfsdBuf(&raw);
    assert_eq!(buf.u8(0).unwrap(), 7);
    assert!(buf.u8(64).is_err());
    assert_eq!(buf.u32(4).unwrap(), 0x1234_5678);
    assert!(buf.u32(61).is_err());
    assert_eq!(buf.u64(8).unwrap(), 0x1122_3344_5566_7788);
    assert!(buf.u64(57).is_err());
    assert!(buf.bytes(10, usize::MAX).is_err());

    assert_eq!(buf.str_at(0).unwrap(), "");
    assert_eq!(buf.str_at(8).unwrap(), "abc");
    assert!(buf.str_at(24).is_err());
    assert!(buf.counted(u64::MAX).is_err());
    assert!(buf.dict_entries(4, 8).is_err());

    // Bucket count > buffer length error
    let mut bad_bucket = vec![0u8; 64];
    bad_bucket[32..40].copy_from_slice(&1u64.to_le_bytes()); // 1 bucket at abs=40
    bad_bucket[40..48].copy_from_slice(&24u64.to_le_bytes()); // rel=24 -> abs=56
    bad_bucket[48..56].copy_from_slice(&9999u64.to_le_bytes()); // n=9999 > len
    let bb = CfsdBuf(&bad_bucket);
    assert!(bb.dict_entries(40, 4).is_err());

    // decode primitives, arrays, union, empty list, oversized list, dict mismatch
    let mut dec_raw = vec![0u8; 128];
    dec_raw[0] = 0;
    dec_raw[1] = 5;
    dec_raw[4..8].copy_from_slice(&(-42i32).to_le_bytes());
    dec_raw[8..12].copy_from_slice(&1.5f32.to_le_bytes());
    dec_raw[12..16].copy_from_slice(&2.5f32.to_le_bytes());
    dec_raw[16..24].copy_from_slice(&3.25f64.to_le_bytes());
    dec_raw[24..32].copy_from_slice(&4.25f64.to_le_bytes());
    // Union at 32: size 8, value = 77u32 at 32, tag = 4 at 40
    dec_raw[32..36].copy_from_slice(&77u32.to_le_bytes());
    dec_raw[40..48].copy_from_slice(&4u64.to_le_bytes());
    // List with n > len at abs=104 (rel=72), count at 96..104 = 5000
    dec_raw[56..64].copy_from_slice(&72u64.to_le_bytes());
    dec_raw[96..104].copy_from_slice(&5000u64.to_le_bytes());
    // Dict at 64: rel = 0, len = 1 -> mismatch error
    dec_raw[64..72].copy_from_slice(&0u64.to_le_bytes());
    dec_raw[72..80].copy_from_slice(&1u64.to_le_bytes());

    let dbuf = CfsdBuf(&dec_raw);
    assert_eq!(decode(&dbuf, 0, &CfsdType::Bool).unwrap(), Value::from(0));
    assert_eq!(decode(&dbuf, 1, &CfsdType::Bool).unwrap(), Value::from(1));
    assert_eq!(decode(&dbuf, 4, &CfsdType::I32).unwrap(), Value::from(-42));
    assert_eq!(decode(&dbuf, 8, &CfsdType::F32).unwrap(), Value::from(1.5));
    assert_eq!(
        decode(&dbuf, 16, &CfsdType::F64).unwrap(),
        Value::from(3.25)
    );
    assert_eq!(
        decode(&dbuf, 8, &CfsdType::F32Arr(2)).unwrap(),
        serde_json::json!([1.5, 2.5])
    );
    assert_eq!(
        decode(&dbuf, 16, &CfsdType::F64Arr(2)).unwrap(),
        serde_json::json!([3.25, 4.25])
    );

    static VARIANTS: [(u64, CfsdType); 1] = [(4, CfsdType::U32)];
    assert_eq!(
        decode(&dbuf, 32, &CfsdType::Union(8, &VARIANTS)).unwrap(),
        Value::from(77)
    );
    static EMPTY_VARIANTS: [(u64, CfsdType); 0] = [];
    assert!(decode(&dbuf, 32, &CfsdType::Union(8, &EMPTY_VARIANTS)).is_err());

    // List rel == 0 at offset 80
    assert_eq!(
        decode(&dbuf, 80, &CfsdType::List(&CfsdType::U32, 4)).unwrap(),
        serde_json::json!([])
    );
    // List n > len at offset 56
    assert!(decode(&dbuf, 56, &CfsdType::List(&CfsdType::U32, 4)).is_err());

    // Dict rel == 0, len == 0 at offset 80 -> empty object
    assert_eq!(
        decode(
            &dbuf,
            80,
            &CfsdType::Dict(&CfsdType::U32, &CfsdType::U32, 8, 4)
        )
        .unwrap(),
        serde_json::json!({})
    );
    // Dict rel == 0, len == 1 at offset 64 -> mismatch error
    assert!(decode(
        &dbuf,
        64,
        &CfsdType::Dict(&CfsdType::U32, &CfsdType::U32, 8, 4)
    )
    .is_err());
}

#[test]
fn item_trader_new_activities_wrappers_and_starter_shipcaster_systems() {
    let mut sde = SdeRegistry::default();

    // 1. Wrapper methods return error when resource is not registered
    assert!(sde.contribution_methods().is_err());
    assert!(sde.dungeon_objectives().is_err());
    assert!(sde.surveys().is_err());
    assert!(sde.operation_categories().is_err());
    assert!(sde.operations().is_err());
    assert!(sde.operation_tasks().is_err());
    assert!(sde.challenge_conditions().is_err());
    assert!(sde.challenge_events().is_err());
    assert!(sde.challenges().is_err());

    // 2. item_trader_component_for_type
    assert_eq!(sde.item_trader_component_for_type(40000).unwrap(), None);
    sde.space_components_by_type
        .insert(40000, vec!["otherComp".to_string()]);
    assert_eq!(sde.item_trader_component_for_type(40000).unwrap(), None);

    sde.space_components_by_type
        .insert(40001, vec!["itemTrader".to_string()]);
    sde.space_components_by_type
        .insert(40002, vec!["itemTrader".to_string()]);
    sde.space_components_by_type
        .insert(40003, vec!["itemTrader".to_string()]);

    // Build synthetic spacecomponentsbytype.fsdbinary with 2 records of stride 1832:
    // rec 0: type 40001, recipes = [11, 22], range = 1000 (clamped to 2500.0)
    // rec 1: type 40002, recipes rel = 0, range = 6000 (kept as 6000.0)
    let mut sc_buf = vec![0u8; 64];
    sc_buf[40..48].copy_from_slice(&2u64.to_le_bytes());
    sc_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    sc_buf.extend_from_slice(&2u64.to_le_bytes());
    let rec_abs = sc_buf.len();
    sc_buf[56..64].copy_from_slice(&((rec_abs - 32) as u64).to_le_bytes());
    sc_buf.extend_from_slice(&vec![0u8; 1832 * 2]);

    // Allocate recipes list [11, 22] at end
    sc_buf.extend_from_slice(&2u64.to_le_bytes());
    let list_abs = sc_buf.len();
    sc_buf.extend_from_slice(&11u32.to_le_bytes());
    sc_buf.extend_from_slice(&22u32.to_le_bytes());
    let list_rel = (list_abs - 32) as u64;

    sc_buf[rec_abs..rec_abs + 4].copy_from_slice(&40001u32.to_le_bytes());
    sc_buf[rec_abs + 464..rec_abs + 472].copy_from_slice(&list_rel.to_le_bytes());
    sc_buf[rec_abs + 472..rec_abs + 476].copy_from_slice(&1000u32.to_le_bytes());

    let rec1_abs = rec_abs + 1832;
    sc_buf[rec1_abs..rec1_abs + 4].copy_from_slice(&40002u32.to_le_bytes());
    sc_buf[rec1_abs + 464..rec1_abs + 472].copy_from_slice(&0u64.to_le_bytes());
    sc_buf[rec1_abs + 472..rec1_abs + 476].copy_from_slice(&6000u32.to_le_bytes());

    let sc_tmp = std::env::temp_dir().join(format!(
        "evomu_item_trader_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&sc_tmp, &sc_buf).unwrap();
    sde.resources.insert(
        "res:/staticdata/spacecomponentsbytype.fsdbinary".to_string(),
        SdeResourceInfo {
            res_path: "res:/staticdata/spacecomponentsbytype.fsdbinary".to_string(),
            cdn_relative_path: sc_tmp.display().to_string(),
            md5: String::new(),
            byte_size: sc_buf.len() as u64,
            format: "cfsd".to_string(),
            record_count: 2,
            spot: String::new(),
        },
    );

    assert_eq!(
        sde.item_trader_component_for_type(40001).unwrap(),
        Some((vec![11, 22], 2_500.0))
    );
    assert_eq!(
        sde.item_trader_component_for_type(40002).unwrap(),
        Some((Vec::new(), 6_000.0))
    );
    assert_eq!(sde.item_trader_component_for_type(40003).unwrap(), None);

    // 2b. shipcaster_sde_components (launcher, landing pad, starter shipcaster, underConstruction)
    sde.space_components_by_type.insert(
        75001,
        vec![
            "shipcasterLauncher".to_string(),
            "builtLandingPad".to_string(),
            "starterShipcaster".to_string(),
            "underConstruction".to_string(),
        ],
    );
    sc_buf[rec_abs..rec_abs + 4].copy_from_slice(&75001u32.to_le_bytes());
    sc_buf[rec_abs + 888..rec_abs + 892].copy_from_slice(&500_001u32.to_le_bytes());
    sc_buf[rec_abs + 804..rec_abs + 808].copy_from_slice(&500_002u32.to_le_bytes());
    sc_buf[rec_abs + 960..rec_abs + 964].copy_from_slice(&30_000_142u32.to_le_bytes());
    sc_buf[rec_abs + 968..rec_abs + 972].copy_from_slice(&60_003_760u32.to_le_bytes());
    sc_buf[rec_abs + 1128..rec_abs + 1132].copy_from_slice(&75002u32.to_le_bytes());
    sc_buf[rec_abs + 1148..rec_abs + 1152].copy_from_slice(&1000u32.to_le_bytes()); // clamped to 2500.0

    // Append underConstruction input_items dict (1 bucket -> 1 entry of 40 bytes) + LP list
    sc_buf.extend_from_slice(&1u64.to_le_bytes()); // 1 bucket
    let uc_dict_abs = sc_buf.len();
    sc_buf.extend_from_slice(&0u64.to_le_bytes()); // bucket 0 rel placeholder
    sc_buf.extend_from_slice(&1u64.to_le_bytes()); // 1 entry in bucket 0
    let uc_entry_abs = sc_buf.len();
    sc_buf.extend_from_slice(&[0u8; 40]);
    let uc_b_rel = (uc_entry_abs - 32) as u64;
    sc_buf[uc_dict_abs..uc_dict_abs + 8].copy_from_slice(&uc_b_rel.to_le_bytes());
    sc_buf.extend_from_slice(&1u64.to_le_bytes()); // lp_n = 1
    let lp_abs = sc_buf.len();
    sc_buf.extend_from_slice(&[0u8; 24]);
    sc_buf[lp_abs + 16..lp_abs + 20].copy_from_slice(&1_000_180u32.to_le_bytes());
    sc_buf[lp_abs + 20..lp_abs + 24].copy_from_slice(&25u32.to_le_bytes());
    let lp_rel = (lp_abs - 32) as u64;
    sc_buf[uc_entry_abs..uc_entry_abs + 4].copy_from_slice(&34u32.to_le_bytes());
    sc_buf[uc_entry_abs + 8..uc_entry_abs + 16].copy_from_slice(&lp_rel.to_le_bytes());
    sc_buf[uc_entry_abs + 28..uc_entry_abs + 32].copy_from_slice(&500u32.to_le_bytes());

    let uc_dict_rel = (uc_dict_abs - 32) as u64;
    sc_buf[rec_abs + 1112..rec_abs + 1120].copy_from_slice(&uc_dict_rel.to_le_bytes());
    // First test expected_len mismatch (expected 99 vs 1) -> Err before OnceLock is populated
    sc_buf[rec_abs + 1120..rec_abs + 1128].copy_from_slice(&99u64.to_le_bytes());
    std::fs::write(&sc_tmp, &sc_buf).unwrap();
    assert!(sde.shipcaster_sde_components().is_err());

    // Now set expected_len = 1 -> populates OnceLock and returns parsed components
    sc_buf[rec_abs + 1120..rec_abs + 1128].copy_from_slice(&1u64.to_le_bytes());
    std::fs::write(&sc_tmp, &sc_buf).unwrap();
    let comps = sde.shipcaster_sde_components().expect("shipcaster_sde_components");
    assert_eq!(comps.launcher_factions.get(&75001), Some(&500_001));
    assert_eq!(comps.landing_pad_factions.get(&75001), Some(&500_002));
    assert_eq!(
        comps.starter_shipcasters.get(&75001),
        Some(&(30_000_142, 60_003_760))
    );
    let uc = &comps.under_construction[&75001];
    assert_eq!(uc.built_item_type_id, 75002);
    assert_eq!(uc.interaction_range_m, 2_500.0);
    assert_eq!(
        uc.input_items.get(&34),
        Some(&UnderConstructionInputItem {
            type_id: 34,
            required_qty: 500,
            lp_corp_id: Some(1_000_180),
            lp_per_unit: 25,
        })
    );
    // Second call hits OnceLock cache
    assert_eq!(
        sde.shipcaster_sde_components().unwrap().launcher_factions.len(),
        1
    );

    // 3. load_new_activity_ids (stride 88)
    let mut na_buf = vec![0u8; 64];
    na_buf[40..48].copy_from_slice(&2u64.to_le_bytes());
    na_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    na_buf.extend_from_slice(&2u64.to_le_bytes());
    let na_abs = na_buf.len();
    na_buf[56..64].copy_from_slice(&((na_abs - 32) as u64).to_le_bytes());
    na_buf.extend_from_slice(&vec![0u8; 88 * 2]);
    na_buf[na_abs..na_abs + 8].copy_from_slice(&10i64.to_le_bytes());
    na_buf[na_abs + 88..na_abs + 96].copy_from_slice(&20i64.to_le_bytes());

    let na_tmp = std::env::temp_dir().join(format!(
        "evomu_new_activities_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&na_tmp, &na_buf).unwrap();
    sde.resources.insert(
        "res:/staticdata/newactivities.fsdbinary".to_string(),
        SdeResourceInfo {
            res_path: "res:/staticdata/newactivities.fsdbinary".to_string(),
            cdn_relative_path: na_tmp.display().to_string(),
            md5: String::new(),
            byte_size: na_buf.len() as u64,
            format: "cfsd".to_string(),
            record_count: 2,
            spot: String::new(),
        },
    );
    let ids = sde.load_new_activity_ids().unwrap();
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![10, 20]);

    // Size mismatch and count mismatch on load_new_activity_ids
    sde.resources
        .get_mut("res:/staticdata/newactivities.fsdbinary")
        .unwrap()
        .byte_size += 1;
    assert!(sde.load_new_activity_ids().is_err());

    let mut na_bad = na_buf.clone();
    na_bad[40..48].copy_from_slice(&5u64.to_le_bytes());
    std::fs::write(&na_tmp, &na_bad).unwrap();
    sde.resources
        .get_mut("res:/staticdata/newactivities.fsdbinary")
        .unwrap()
        .byte_size = na_bad.len() as u64;
    assert!(sde.load_new_activity_ids().is_err());

    // 4. starter_shipcaster_systems_by_race
    sde.schools.insert(
        11,
        SchoolInfo {
            school_id: 11,
            race_id: 4,
            corporation_id: 1_000_166,
            station_id: 60_000_001,
            solar_system_id: 30_000_001,
            name: "School 11".to_string(),
        },
    );
    sde.schools.insert(
        99, // outside 11..=22 -> ignored
        SchoolInfo {
            school_id: 99,
            race_id: 4,
            corporation_id: 1_000_999,
            station_id: 60_000_999,
            solar_system_id: 30_000_999,
            name: "Ignored".to_string(),
        },
    );
    sde.stations.insert(
        60_000_002,
        StationInfo {
            station_id: 60_000_002,
            solar_system_id: 30_000_002,
            constellation_id: 20_000_001,
            region_id: 10_000_001,
            station_type_id: 1529,
            name: "St2".to_string(),
            position: DVec3::ZERO,
            owner_corporation_id: 1_000_166,
            owner_faction_id: None,
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    sde.stations.insert(
        60_000_004,
        StationInfo {
            station_id: 60_000_004,
            solar_system_id: 30_000_004,
            constellation_id: 20_000_001,
            region_id: 10_000_001,
            station_type_id: 1529,
            name: "St4".to_string(),
            position: DVec3::ZERO,
            owner_corporation_id: 9_999_999,
            owner_faction_id: None,
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    sde.agents.insert(
        3_000_001,
        AgentInfo {
            agent_id: 3_000_001,
            name: "Ag1".to_string(),
            corporation_id: 1_000_166,
            station_id: Some(60_000_003),
            solar_system_id: Some(30_000_003),
            level: 1,
            division_id: 24,
            agent_type_id: 2,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    sde.agents.insert(
        3_000_002,
        AgentInfo {
            agent_id: 3_000_002,
            name: "Ag2".to_string(),
            corporation_id: 9_999_999,
            station_id: Some(60_000_005),
            solar_system_id: Some(30_000_005),
            level: 1,
            division_id: 24,
            agent_type_id: 2,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    for (sys_id, reg_id) in [
        (30_000_101, 10_001_004),
        (30_000_102, 10_001_004),
        (30_000_103, 10_001_004),
        (30_000_104, 10_001_004),
        (30_000_105, 10_001_004),
        (30_000_106, 10_000_002),
    ] {
        sde.solar_systems.insert(
            sys_id,
            SolarSystem {
                solar_system_id: sys_id,
                constellation_id: 20_000_001,
                region_id: reg_id,
                name: format!("Sys-{sys_id}"),
                security_status: 0.9,
                radius: 1.0e12,
                galactic_pos_ly: [0.0, 0.0, 0.0],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
    }

    let db_tmp = std::env::temp_dir().join(format!(
        "evomu_shipcaster_starter_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_tmp);
    {
        let conn = Connection::open(&db_tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE sde_schools (school_id INTEGER, race_id INTEGER, starting_stations_json TEXT, career_agents_json TEXT);
             INSERT INTO sde_schools VALUES (14, 2, '[60000004]', '[3000002]');
             CREATE TABLE sde_solar_systems (system_id INTEGER, region_id INTEGER, faction_id INTEGER);
             INSERT INTO sde_solar_systems VALUES (30000101, 10001004, 500001);
             INSERT INTO sde_solar_systems VALUES (30000102, 10001004, 500002);
             INSERT INTO sde_solar_systems VALUES (30000103, 10001004, 500003);
             INSERT INTO sde_solar_systems VALUES (30000104, 10001004, 500004);
             INSERT INTO sde_solar_systems VALUES (30000105, 10001004, 500999);
             INSERT INTO sde_solar_systems VALUES (30000106, 10000002, 500001);",
        )
        .unwrap();
        let aux = crate::sqlite_loader_skins::load_universe_aux_and_career_tables(&conn).unwrap();
        sde.school_starting_stations = Arc::new(aux.school_starting_stations);
        sde.school_career_agents = Arc::new(aux.school_career_agents);
        sde.solar_system_factions = Arc::new(aux.solar_system_factions);
    }
    let _ = std::fs::remove_file(&db_tmp);
    sde.db_path = None;

    let by_race = sde.starter_shipcaster_systems_by_race();
    assert_eq!(by_race.get(&30_000_001), Some(&(4, Some(60_000_001))));
    assert_eq!(by_race.get(&30_000_002), Some(&(4, Some(60_000_002))));
    assert_eq!(by_race.get(&30_000_003), Some(&(4, Some(60_000_003))));
    assert_eq!(by_race.get(&30_000_004), Some(&(2, Some(60_000_004))));
    assert_eq!(by_race.get(&30_000_005), Some(&(2, Some(60_000_005))));
    assert_eq!(by_race.get(&30_000_101), Some(&(1, None)));
    assert_eq!(by_race.get(&30_000_102), Some(&(2, None)));
    assert_eq!(by_race.get(&30_000_103), Some(&(4, None)));
    assert_eq!(by_race.get(&30_000_104), Some(&(8, None)));
    assert!(!by_race.contains_key(&30_000_105));
    assert!(!by_race.contains_key(&30_000_106));
    assert!(!by_race.contains_key(&30_000_999));

    let _ = std::fs::remove_file(&sc_tmp);
    let _ = std::fs::remove_file(&na_tmp);
}

#[test]
fn load_cfsd_nested_errors_empty_buckets_and_starter_fallbacks() {
    // 1. dict_entries with an empty bucket (off == 0) + non-empty bucket
    let mut raw = vec![0u8; 72];
    raw[32..40].copy_from_slice(&2u64.to_le_bytes()); // 2 buckets at abs=40
    // bucket 0 (abs 40..48) = 0 (empty!)
    // bucket 1 (abs 48..56) -> rel=32 (abs=64), count=1 at 56..64
    raw[48..56].copy_from_slice(&32u64.to_le_bytes());
    raw[56..64].copy_from_slice(&1u64.to_le_bytes());
    let buf = CfsdBuf(&raw);
    assert_eq!(buf.dict_entries(40, 8).unwrap(), vec![64]);

    // 2. load_cfsd_nested size mismatch & header count mismatch
    let mut sde = SdeRegistry::default();
    let res_path = "res:/staticdata/contributionmethods.fsdbinary";
    let lay = SdeRegistry::cfsd_nested_layout(res_path).expect("nested layout");
    let mut nested_raw = vec![0u8; 64];
    nested_raw[40..48].copy_from_slice(&99u64.to_le_bytes()); // header count = 99
    nested_raw[48..56].copy_from_slice(&1u64.to_le_bytes()); // 1 bucket
    // bucket 0 = 0 (0 records)
    let tmp = std::env::temp_dir().join(format!(
        "evomu_nested_err_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&tmp, &nested_raw).unwrap();
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: tmp.display().to_string(),
            md5: String::new(),
            byte_size: (nested_raw.len() + 1) as u64,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    assert!(sde.load_cfsd_nested(res_path).is_err(), "size mismatch");

    sde.resources.get_mut(res_path).unwrap().byte_size = nested_raw.len() as u64;
    let err = sde.load_cfsd_nested(res_path).unwrap_err();
    assert!(err.to_string().contains("header says 99"), "unexpected: {err}");
    let _ = lay;
    let _ = std::fs::remove_file(&tmp);

    // 3. starter_shipcaster_systems_by_race when db_path is missing or has empty schema / invalid JSON
    sde.db_path = Some(std::env::temp_dir().join("evomu_nonexistent_starter.sqlite"));
    assert!(sde.starter_shipcaster_systems_by_race().is_empty());

    let empty_db = std::env::temp_dir().join(format!(
        "evomu_starter_empty_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&empty_db);
    {
        let conn = Connection::open(&empty_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sde_schools (school_id INTEGER, race_id INTEGER, starting_stations_json TEXT, career_agents_json TEXT);
             INSERT INTO sde_schools VALUES (11, 1, 'bad-json', 'bad-json');
             CREATE TABLE sde_solar_systems (system_id INTEGER, region_id INTEGER, faction_id INTEGER);
             INSERT INTO sde_solar_systems VALUES (30000142, 10001004, 500001);",
        )
        .unwrap();
    }
    sde.db_path = Some(empty_db.clone());
    sde.solar_systems.insert(
        30_000_142,
        SolarSystem {
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_001_004,
            name: "Jita".to_string(),
            security_status: 0.9,
            galactic_pos_ly: [0.0; 3],
            radius: 1000.0,
            wormhole_class_id: None,
            stations: vec![StationInfo {
                station_id: 60_003_760,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_001_004,
                station_type_id: 1529,
                name: "Sta".to_string(),
                position: DVec3::ZERO,
                owner_corporation_id: 1_000_035,
                owner_faction_id: None,
                operation_id: 1,
                reprocessing_efficiency: 0.5,
                reprocessing_stations_take: 0.05,
                is_conquerable: false,
                orbit_id: None,
                has_clone_bay: true,
            }],
            planet_ids: Vec::new(),
        },
    );
    Arc::make_mut(&mut sde.solar_system_factions).insert(30_000_142, 500_001);
    let res = sde.starter_shipcaster_systems_by_race();
    assert_eq!(res.get(&30_000_142), Some(&(1, Some(60_003_760))));
    let _ = std::fs::remove_file(&empty_db);
}
