//! Unit and mutation-killing tests for `cfsd_nested_layouts`.

use super::*;
use crate::{SdeRegistry, SdeResourceInfo};

pub(crate) fn populate_cfsd_type(buf: &mut Vec<u8>, pos: usize, ty: &CfsdType, seed: u32) {
    match *ty {
        CfsdType::Bool => buf[pos] = 1,
        CfsdType::I32 => {
            let v = -(seed as i32);
            buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        }
        CfsdType::U32 => buf[pos..pos + 4].copy_from_slice(&seed.to_le_bytes()),
        CfsdType::U64 => buf[pos..pos + 8].copy_from_slice(&u64::from(seed).to_le_bytes()),
        CfsdType::F32 => {
            let v = seed as f32 + 0.5;
            buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        }
        CfsdType::F64 => {
            let v = f64::from(seed) + 0.25;
            buf[pos..pos + 8].copy_from_slice(&v.to_le_bytes());
        }
        CfsdType::F32Arr(n) => {
            for i in 0..n {
                let v = (seed + i as u32) as f32;
                buf[pos + 4 * i..pos + 4 * (i + 1)].copy_from_slice(&v.to_le_bytes());
            }
        }
        CfsdType::F64Arr(n) => {
            for i in 0..n {
                let v = f64::from(seed + i as u32);
                buf[pos + 8 * i..pos + 8 * (i + 1)].copy_from_slice(&v.to_le_bytes());
            }
        }
        CfsdType::Union(size, variants) => {
            let (tag, ref vty) = variants[seed as usize % variants.len()];
            buf[pos + size..pos + size + 8].copy_from_slice(&tag.to_le_bytes());
            populate_cfsd_type(buf, pos, vty, seed + 1);
        }
        CfsdType::Str => {
            let s = format!("str_{seed}");
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            let abs = buf.len();
            buf.extend_from_slice(s.as_bytes());
            let rel = (abs - 32) as u64;
            buf[pos..pos + 8].copy_from_slice(&rel.to_le_bytes());
        }
        CfsdType::List(elem, stride) => {
            buf.extend_from_slice(&1u64.to_le_bytes());
            let abs = buf.len();
            buf.extend_from_slice(&vec![0u8; stride]);
            let rel = (abs - 32) as u64;
            buf[pos..pos + 8].copy_from_slice(&rel.to_le_bytes());
            populate_cfsd_type(buf, abs, elem, seed + 1);
        }
        CfsdType::Dict(k_ty, v_ty, stride, val_off) => {
            buf.extend_from_slice(&1u64.to_le_bytes()); // 1 bucket
            let dict_abs = buf.len();
            buf.extend_from_slice(&0u64.to_le_bytes()); // placeholder for bucket 0 rel
            buf.extend_from_slice(&1u64.to_le_bytes()); // 1 entry in bucket 0
            let entry_abs = buf.len();
            buf.extend_from_slice(&vec![0u8; stride]);
            let b_rel = (entry_abs - 32) as u64;
            buf[dict_abs..dict_abs + 8].copy_from_slice(&b_rel.to_le_bytes());
            let d_rel = (dict_abs - 32) as u64;
            buf[pos..pos + 8].copy_from_slice(&d_rel.to_le_bytes());
            buf[pos + 8..pos + 16].copy_from_slice(&1u64.to_le_bytes());
            populate_cfsd_type(buf, entry_abs, k_ty, seed + 1);
            populate_cfsd_type(buf, entry_abs + val_off, v_ty, seed + 2);
        }
        CfsdType::Obj(obj) => populate_cfsd_obj(buf, pos, obj, seed + 1),
    }
}

pub(crate) fn populate_cfsd_obj(buf: &mut Vec<u8>, pos: usize, obj: &CfsdObject, seed: u32) {
    if let Some(m) = obj.mask_offset {
        buf[pos + m..pos + m + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    for (idx, attr) in obj.attrs.iter().enumerate() {
        populate_cfsd_type(buf, pos + attr.offset, &attr.ty, seed + idx as u32 + 1);
    }
}

pub(crate) fn build_synthetic_nested_file(lay: &CfsdNestedLayout) -> Vec<u8> {
    if lay.single {
        let mut buf = vec![0u8; 32 + lay.record.stride];
        populate_cfsd_obj(&mut buf, 32, &lay.record, 10);
        return buf;
    }
    let mut buf = vec![0u8; 64];
    buf[40..48].copy_from_slice(&1u64.to_le_bytes()); // count = 1
    buf[48..56].copy_from_slice(&1u64.to_le_bytes()); // buckets = 1
    buf.extend_from_slice(&1u64.to_le_bytes()); // 1 record in bucket 0
    let rec_abs = buf.len();
    let boff = (rec_abs - 32) as u64;
    buf[56..64].copy_from_slice(&boff.to_le_bytes());
    buf.extend_from_slice(&vec![0u8; lay.record.stride]);
    populate_cfsd_type(&mut buf, rec_abs, &lay.key, 500);
    populate_cfsd_obj(&mut buf, rec_abs, &lay.record, 10);
    buf
}

#[test]
fn const_constructors_and_all_23_nested_layouts_decode_cleanly() {
    static ATTRS: [CfsdAttr; 1] = [a("field", 12, U32, 4)];
    assert_eq!(ATTRS[0].name, "field");
    assert_eq!(ATTRS[0].offset, 12);
    assert_eq!(ATTRS[0].bit, 4);

    let o = obj(24, Some(20), &ATTRS);
    assert_eq!(o.stride, 24);
    assert_eq!(o.mask_offset, Some(20));
    assert_eq!(o.attrs.len(), 1);

    let r = rec("res:/staticdata/custom.fsdbinary", U64, o, true);
    assert_eq!(r.res_path, "res:/staticdata/custom.fsdbinary");
    assert!(r.bare);
    assert!(!r.single);

    assert_eq!(CFSD_NESTED_LAYOUTS.len(), 23);
    let mut sde = SdeRegistry::default();

    for (idx, lay) in CFSD_NESTED_LAYOUTS.iter().enumerate() {
        let found = SdeRegistry::cfsd_nested_layout(lay.res_path).expect("find nested layout");
        assert_eq!(found.res_path, lay.res_path);
        assert_eq!(found.record.stride, lay.record.stride);
        assert_eq!(found.record.mask_offset, lay.record.mask_offset);
        assert_eq!(found.bare, lay.bare);
        assert!(!found.single);

        let raw = build_synthetic_nested_file(lay);
        let tmp = std::env::temp_dir().join(format!(
            "evomu_nested_lay_{}_{}.fsdbinary",
            idx,
            std::process::id()
        ));
        std::fs::write(&tmp, &raw).expect("write nested cfsd");
        sde.resources.insert(
            lay.res_path.to_string(),
            SdeResourceInfo {
                res_path: lay.res_path.to_string(),
                cdn_relative_path: tmp.display().to_string(),
                md5: String::new(),
                byte_size: raw.len() as u64,
                format: "cfsd".to_string(),
                record_count: 1,
                spot: String::new(),
            },
        );

        let decoded = sde
            .load_cfsd_nested(lay.res_path)
            .unwrap_or_else(|e| panic!("failed to decode {}: {e}", lay.res_path));
        assert_eq!(decoded.len(), 1);
        let val = decoded.values().next().unwrap();
        if lay.bare {
            assert!(!val.is_null());
        } else {
            let map = val.as_object().expect("record object");
            for attr in lay.record.attrs {
                assert!(
                    map.contains_key(attr.name),
                    "{}: missing decoded attr {}",
                    lay.res_path,
                    attr.name
                );
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
}

#[test]
fn cfsd_nested_empty_bucket_masked_attrs_pin_mismatch_count_error_and_starter_systems() {
    // 1. Cover empty bucket (L68), masked optional attr skip (L224), pin size mismatch (L252-254), and record count mismatch (L279-282)
    let masked_lay = CFSD_NESTED_LAYOUTS
        .iter()
        .find(|l| {
            !l.single
                && !l.bare
                && l.record.mask_offset.is_some()
                && l.record.attrs.iter().any(|a| a.bit != 0)
        })
        .expect("masked nested layout exists");
    let mask_off = masked_lay.record.mask_offset.unwrap();

    // Build 2-bucket CFSD buffer with bucket 0 = 0 (empty -> L68) and bucket 1 holding 1 record with mask = 0 (L224)
    let mut buf = vec![0u8; 72];
    buf[40..48].copy_from_slice(&1u64.to_le_bytes()); // count = 1
    buf[48..56].copy_from_slice(&2u64.to_le_bytes()); // buckets = 2
    buf[56..64].copy_from_slice(&0u64.to_le_bytes()); // bucket 0 empty (L68)
    buf.extend_from_slice(&1u64.to_le_bytes()); // bucket 1 count = 1
    let rec_abs = buf.len();
    let boff = (rec_abs - 32) as u64;
    buf[64..72].copy_from_slice(&boff.to_le_bytes());
    buf.extend_from_slice(&vec![0u8; masked_lay.record.stride]);
    populate_cfsd_type(&mut buf, rec_abs, &masked_lay.key, 777);
    populate_cfsd_obj(&mut buf, rec_abs, &masked_lay.record, 20);
    // Clear optional mask after populate_cfsd_obj so all non-zero-bit attrs are masked out (L224)
    buf[rec_abs + mask_off..rec_abs + mask_off + 4].copy_from_slice(&0u32.to_le_bytes());

    let tmp = std::env::temp_dir().join(format!(
        "evomu_nested_edge_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&tmp, &buf).expect("write nested edge blob");

    let mut sde = SdeRegistry::default();
    sde.resources.insert(
        masked_lay.res_path.to_string(),
        SdeResourceInfo {
            res_path: masked_lay.res_path.to_string(),
            cdn_relative_path: tmp.display().to_string(),
            md5: String::new(),
            byte_size: buf.len() as u64,
            format: "cfsd".to_string(),
            record_count: 1,
            spot: String::new(),
        },
    );
    let decoded = sde.load_cfsd_nested(masked_lay.res_path).expect("masked nested decode");
    assert_eq!(decoded.len(), 1);
    let obj_map = decoded.values().next().unwrap().as_object().unwrap();
    for a in masked_lay.record.attrs {
        if a.bit != 0 {
            assert!(
                !obj_map.contains_key(a.name),
                "masked-out attr {} must be omitted",
                a.name
            );
        }
    }

    // Importer pin byte_size mismatch (L252-254)
    sde.resources.get_mut(masked_lay.res_path).unwrap().byte_size = (buf.len() + 1) as u64;
    assert!(matches!(
        sde.load_cfsd_nested(masked_lay.res_path),
        Err(crate::SdeError::InvalidSchema(msg)) if msg.contains("size differs from importer pin")
    ));

    // Restore size, mismatch header record count (L279-282)
    sde.resources.get_mut(masked_lay.res_path).unwrap().byte_size = buf.len() as u64;
    buf[40..48].copy_from_slice(&99u64.to_le_bytes());
    std::fs::write(&tmp, &buf).expect("rewrite mismatched count blob");
    assert!(matches!(
        sde.load_cfsd_nested(masked_lay.res_path),
        Err(crate::SdeError::InvalidSchema(msg)) if msg.contains("header says 99")
    ));
    let _ = std::fs::remove_file(&tmp);

    // 2. starter_shipcaster_systems_by_race branches (cfsd_nested_tables.rs L540, L547, L549, L551-552, L575-578)
    let sde_no_db = SdeRegistry::default();
    assert!(sde_no_db.starter_shipcaster_systems_by_race().is_empty());
    let sde_missing_db = SdeRegistry {
        db_path: Some(std::env::temp_dir().join(format!(
            "evomu_starter_nonexistent_{}.sqlite",
            std::process::id()
        ))),
        ..SdeRegistry::default()
    };
    assert!(sde_missing_db.starter_shipcaster_systems_by_race().is_empty());

    let empty_db = std::env::temp_dir().join(format!(
        "evomu_starter_empty_{}.sqlite",
        std::process::id()
    ));
    let schools_db = std::env::temp_dir().join(format!(
        "evomu_starter_schools_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&empty_db);
    let _ = std::fs::remove_file(&schools_db);
    {
        let _c = rusqlite::Connection::open(&empty_db).expect("create empty db");
    }
    let sde_empty_db = SdeRegistry {
        db_path: Some(empty_db.clone()),
        ..SdeRegistry::default()
    };
    assert!(sde_empty_db.starter_shipcaster_systems_by_race().is_empty());

    {
        let conn = rusqlite::Connection::open(&schools_db).expect("create starter schools db");
        conn.execute_batch(
            "CREATE TABLE sde_schools (school_id INTEGER, race_id INTEGER, starting_stations_json TEXT, career_agents_json TEXT);
             CREATE TABLE sde_solar_systems (system_id INTEGER, region_id INTEGER, faction_id INTEGER);
             INSERT INTO sde_schools VALUES (11, 1, '{bad_json', '{bad_json');
             INSERT INTO sde_schools VALUES (12, 2, '[60000001]', '[8801, 8802, 8899]');
             INSERT INTO sde_solar_systems VALUES (30009101, 10001004, 500001);
             INSERT INTO sde_solar_systems VALUES (30009102, 10001004, 500002);
             INSERT INTO sde_solar_systems VALUES (30009103, 10001004, 500003);
             INSERT INTO sde_solar_systems VALUES (30009104, 10001004, 500004);
             INSERT INTO sde_solar_systems VALUES (30009105, 10001004, 500999);",
        )
        .expect("populate starter db");
    }
    let mut sde_sc = SdeRegistry {
        db_path: Some(schools_db.clone()),
        ..SdeRegistry::default()
    };
    std::sync::Arc::make_mut(&mut sde_sc.school_starting_stations)
        .insert(12, (2, vec![60000001]));
    std::sync::Arc::make_mut(&mut sde_sc.school_career_agents)
        .insert(12, (2, vec![8801, 8802, 8899]));
    for (sys_id, fac_id) in [
        (30009101u32, 500001u32),
        (30009102, 500002),
        (30009103, 500003),
        (30009104, 500004),
        (30009105, 500999),
    ] {
        std::sync::Arc::make_mut(&mut sde_sc.solar_system_factions).insert(sys_id, fac_id);
        sde_sc.solar_systems.insert(
            sys_id,
            crate::SolarSystem {
                solar_system_id: sys_id,
                constellation_id: 20001001,
                region_id: 10001004,
                name: format!("Sys_{sys_id}"),
                security_status: 1.0,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            },
        );
    }
    sde_sc.stations.insert(
        60000001,
        crate::StationInfo {
            station_id: 60000001,
            solar_system_id: 30009001,
            constellation_id: 20000001,
            region_id: 10000001,
            owner_corporation_id: 1000001,
            owner_faction_id: None,
            station_type_id: 1529,
            operation_id: 0,
            name: "Starter Station".into(),
            position: glam::DVec3::ZERO,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: true,
        },
    );
    sde_sc.agents.insert(
        8801,
        crate::AgentInfo {
            agent_id: 8801,
            name: "Career With Sys".into(),
            corporation_id: 1000001,
            station_id: Some(60000002),
            solar_system_id: Some(30009002),
            level: 1,
            division_id: 22,
            agent_type_id: 7,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    sde_sc.agents.insert(
        8802,
        crate::AgentInfo {
            agent_id: 8802,
            name: "Career No Sys".into(),
            corporation_id: 1000001,
            station_id: None,
            solar_system_id: None,
            level: 1,
            division_id: 22,
            agent_type_id: 7,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );
    let starters = sde_sc.starter_shipcaster_systems_by_race();
    assert_eq!(starters.get(&30009001), Some(&(2, Some(60000001))));
    assert_eq!(starters.get(&30009002), Some(&(2, Some(60000002))));
    assert_eq!(starters.get(&30009101), Some(&(1, None)));
    assert_eq!(starters.get(&30009102), Some(&(2, None)));
    assert_eq!(starters.get(&30009103), Some(&(4, None)));
    assert_eq!(starters.get(&30009104), Some(&(8, None)));
    assert!(!starters.contains_key(&30009105));
    let _ = std::fs::remove_file(&empty_db);
    let _ = std::fs::remove_file(&schools_db);
}
