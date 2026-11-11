//! Unit and mutation-killing tests for `cfsd_flat_tables`.

use super::*;

#[test]
fn cfsd_record_accessors_and_layout_lookup() {
    let mut rec = CfsdRecord {
        key: 42,
        fields: BTreeMap::new(),
    };
    rec.fields.insert("num".to_string(), CfsdValue::Int(123));
    rec.fields
        .insert("txt".to_string(), CfsdValue::Str("hello".to_string()));
    rec.fields
        .insert("arr".to_string(), CfsdValue::IntList(vec![10, 20]));

    assert_eq!(rec.int("num"), Some(123));
    assert_eq!(rec.int("txt"), None);
    assert_eq!(rec.int("missing"), None);

    assert_eq!(rec.str("txt"), Some("hello"));
    assert_eq!(rec.str("num"), None);
    assert_eq!(rec.str("missing"), None);

    assert_eq!(rec.list("arr"), Some(&[10, 20][..]));
    assert_eq!(rec.list("num"), None);
    assert_eq!(rec.list("missing"), None);

    assert!(SdeRegistry::cfsd_flat_layout("res:/staticdata/unknown.fsdbinary").is_none());
    assert_eq!(CFSD_FLAT_LAYOUTS.len(), 25);
    for lay in CFSD_FLAT_LAYOUTS {
        let found = SdeRegistry::cfsd_flat_layout(lay.res_path).expect("find flat layout");
        assert_eq!(found.res_path, lay.res_path);
        assert_eq!(found.key_width, lay.key_width);
        assert_eq!(found.stride, lay.stride);
        assert_eq!(found.mask_offset, lay.mask_offset);
        assert_eq!(found.fields, lay.fields);
    }
}

fn build_synthetic_flat_table(lay: &CfsdLayout) -> Vec<u8> {
    let mut buf = vec![0u8; 64];
    // 2 records: rec 0 (key = 101, mask = u32::MAX), rec 1 (key = 202, mask = 0)
    buf[40..48].copy_from_slice(&2u64.to_le_bytes());
    buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    buf.extend_from_slice(&2u64.to_le_bytes());
    let recs_abs = buf.len();
    let boff = (recs_abs - 32) as u64;
    buf[56..64].copy_from_slice(&boff.to_le_bytes());
    buf.extend_from_slice(&vec![0u8; lay.stride * 2]);

    for (rec_idx, &(key, mask_on)) in [(101u64, true), (202u64, false)].iter().enumerate() {
        let rec_pos = recs_abs + rec_idx * lay.stride;
        if lay.key_width == 8 {
            buf[rec_pos..rec_pos + 8].copy_from_slice(&key.to_le_bytes());
        } else {
            buf[rec_pos..rec_pos + 4].copy_from_slice(&(key as u32).to_le_bytes());
        }
        if let Some(m_off) = lay.mask_offset {
            let mask_val: u32 = if mask_on { u32::MAX } else { 0 };
            buf[rec_pos + m_off..rec_pos + m_off + 4].copy_from_slice(&mask_val.to_le_bytes());
        }
        for (f_idx, &(name, kind, off, bit)) in lay.fields.iter().enumerate() {
            if !mask_on && bit.is_some() {
                continue;
            }
            let p = rec_pos + off;
            let base_val = (f_idx as u32 + 1) * 10 + rec_idx as u32;
            match kind {
                CfsdFieldKind::Bool => {
                    buf[p] = if rec_idx == 0 { 1 } else { 0 };
                }
                CfsdFieldKind::I32 => {
                    let v = -(base_val as i32);
                    buf[p..p + 4].copy_from_slice(&v.to_le_bytes());
                }
                CfsdFieldKind::U32 => {
                    buf[p..p + 4].copy_from_slice(&base_val.to_le_bytes());
                }
                CfsdFieldKind::U64 => {
                    let v = u64::from(base_val) * 1_000;
                    buf[p..p + 8].copy_from_slice(&v.to_le_bytes());
                }
                CfsdFieldKind::Str => {
                    let s = format!("{name}_{rec_idx}");
                    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
                    let s_abs = buf.len();
                    buf.extend_from_slice(s.as_bytes());
                    let rel = (s_abs - 32) as u64;
                    buf[p..p + 8].copy_from_slice(&rel.to_le_bytes());
                }
                CfsdFieldKind::U32List => {
                    if rec_idx == 0 {
                        buf.extend_from_slice(&2u64.to_le_bytes());
                        let l_abs = buf.len();
                        buf.extend_from_slice(&base_val.to_le_bytes());
                        buf.extend_from_slice(&(base_val + 1).to_le_bytes());
                        let rel = (l_abs - 32) as u64;
                        buf[p..p + 8].copy_from_slice(&rel.to_le_bytes());
                    } else {
                        buf[p..p + 8].copy_from_slice(&0u64.to_le_bytes());
                    }
                }
            }
        }
    }
    buf
}

#[test]
fn load_cfsd_table_decodes_all_25_flat_layouts_and_validates_errors() {
    let mut sde = SdeRegistry::default();
    assert!(sde
        .load_cfsd_table("res:/staticdata/not_a_layout.fsdbinary")
        .is_err());

    for (idx, lay) in CFSD_FLAT_LAYOUTS.iter().enumerate() {
        let raw = build_synthetic_flat_table(lay);
        let tmp = std::env::temp_dir().join(format!(
            "evomu_flat_{}_{}.fsdbinary",
            idx,
            std::process::id()
        ));
        std::fs::write(&tmp, &raw).expect("write flat cfsd");
        sde.resources.insert(
            lay.res_path.to_string(),
            SdeResourceInfo {
                res_path: lay.res_path.to_string(),
                cdn_relative_path: tmp.display().to_string(),
                md5: String::new(),
                byte_size: raw.len() as u64,
                format: "cfsd".to_string(),
                record_count: 2,
                spot: String::new(),
            },
        );

        let table = sde
            .load_cfsd_table(lay.res_path)
            .unwrap_or_else(|e| panic!("failed to load {}: {e}", lay.res_path));
        assert_eq!(table.len(), 2);
        let r0 = &table[&101];
        let r1 = &table[&202];
        assert_eq!(r0.key, 101);
        assert_eq!(r1.key, 202);

        for (f_idx, &(name, kind, _, bit)) in lay.fields.iter().enumerate() {
            let base_val = i64::from((f_idx as u32 + 1) * 10);
            match kind {
                CfsdFieldKind::Bool => {
                    assert_eq!(r0.int(name), Some(1));
                    if bit.is_none() {
                        assert_eq!(r1.int(name), Some(0));
                    }
                }
                CfsdFieldKind::I32 => {
                    assert_eq!(r0.int(name), Some(-base_val));
                }
                CfsdFieldKind::U32 => {
                    assert_eq!(r0.int(name), Some(base_val));
                    if bit.is_none() {
                        assert_eq!(r1.int(name), Some(base_val + 1));
                    }
                }
                CfsdFieldKind::U64 => {
                    assert_eq!(r0.int(name), Some(base_val * 1_000));
                }
                CfsdFieldKind::Str => {
                    let expected0 = format!("{name}_0");
                    assert_eq!(r0.str(name), Some(expected0.as_str()));
                    if bit.is_none() {
                        let expected1 = format!("{name}_1");
                        assert_eq!(r1.str(name), Some(expected1.as_str()));
                    }
                }
                CfsdFieldKind::U32List => {
                    assert_eq!(r0.list(name), Some(&[base_val, base_val + 1][..]));
                    if bit.is_none() {
                        assert_eq!(r1.list(name), Some(&[][..]));
                    }
                }
            }
            if bit.is_some() {
                assert!(
                    !r1.fields.contains_key(name),
                    "optional field {name} must be omitted when mask == 0"
                );
            }
        }

        // Size mismatch check
        sde.resources.get_mut(lay.res_path).unwrap().byte_size += 1;
        assert!(sde.load_cfsd_table(lay.res_path).is_err());

        // Header count mismatch check
        let mut bad_count = raw.clone();
        bad_count[40..48].copy_from_slice(&99u64.to_le_bytes());
        std::fs::write(&tmp, &bad_count).unwrap();
        sde.resources.get_mut(lay.res_path).unwrap().byte_size = bad_count.len() as u64;
        assert!(sde.load_cfsd_table(lay.res_path).is_err());

        let _ = std::fs::remove_file(&tmp);
    }

    // Exercise CfsdFieldKind::I32 and runtime layout() helper directly
    let custom_lay = layout(
        "res:/staticdata/agenttypes.fsdbinary",
        4,
        16,
        None,
        &[("signedVal", CfsdFieldKind::I32, 4, None)],
    );
    let raw_i32 = build_synthetic_flat_table(&custom_lay);
    let buf = super::super::cfsd_nested_tables::CfsdBuf(&raw_i32);
    let recs = buf.dict_entries(56, 16).unwrap();
    assert_eq!(buf.u32(recs[0] + 4).unwrap() as i32, -10);

    // Call runtime layout() constructor to verify CfsdLayout fields directly
    let runtime_lay = layout(
        "res:/staticdata/runtime_check.fsdbinary",
        4,
        16,
        Some(12),
        &[("signedVal", CfsdFieldKind::I32, 4, Some(1))],
    );
    assert_eq!(runtime_lay.res_path, "res:/staticdata/runtime_check.fsdbinary");
    assert_eq!(runtime_lay.key_width, 4);
    assert_eq!(runtime_lay.stride, 16);
    assert_eq!(runtime_lay.mask_offset, Some(12));
    assert_eq!(runtime_lay.fields.len(), 1);
}
