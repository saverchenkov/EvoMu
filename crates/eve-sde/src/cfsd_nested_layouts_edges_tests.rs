//! Boundary, optional-mask, fallback, and corruption tests for `cfsd_nested_layouts`.

use super::*;
use crate::{SdeError, SdeRegistry, SdeResourceInfo};

fn encode_empty_record_file(lay: &CfsdNestedLayout) -> Vec<u8> {
    let mut buf = vec![0u8; 64];
    buf[40..48].copy_from_slice(&1u64.to_le_bytes()); // count = 1
    buf[48..56].copy_from_slice(&1u64.to_le_bytes()); // buckets = 1
    buf.extend_from_slice(&1u64.to_le_bytes()); // 1 record in bucket 0
    let rec_abs = buf.len();
    let boff = (rec_abs - 32) as u64;
    buf[56..64].copy_from_slice(&boff.to_le_bytes());
    buf.extend_from_slice(&vec![0u8; lay.record.stride]);

    // Write only the record key at offset 0 (leaving optional mask = 0, strings/lists/dicts = 0)
    match lay.key {
        CfsdType::U64 => buf[rec_abs..rec_abs + 8].copy_from_slice(&42u64.to_le_bytes()),
        CfsdType::Str => {
            let key_s = "empty_key";
            buf.extend_from_slice(&(key_s.len() as u64).to_le_bytes());
            let abs = buf.len();
            buf.extend_from_slice(key_s.as_bytes());
            let rel = (abs - 32) as u64;
            buf[rec_abs..rec_abs + 8].copy_from_slice(&rel.to_le_bytes());
        }
        _ => {}
    }

    // For bare layouts without optional mask or required nested dicts/lists, zeroed rel+len decodes to {} or [] or "".
    buf
}

fn encode_populated_value(buf: &mut Vec<u8>, pos: usize, ty: &CfsdType, seed: u32) {
    match *ty {
        CfsdType::Bool => buf[pos] = u8::from(seed % 2 == 0),
        CfsdType::I32 => buf[pos..pos + 4].copy_from_slice(&(-(seed as i32)).to_le_bytes()),
        CfsdType::U32 => buf[pos..pos + 4].copy_from_slice(&seed.to_le_bytes()),
        CfsdType::U64 => buf[pos..pos + 8].copy_from_slice(&u64::from(seed).to_le_bytes()),
        CfsdType::F32 => buf[pos..pos + 4].copy_from_slice(&(seed as f32 + 0.25).to_le_bytes()),
        CfsdType::F64 => buf[pos..pos + 8].copy_from_slice(&(f64::from(seed) + 0.125).to_le_bytes()),
        CfsdType::F32Arr(n) => {
            for i in 0..n {
                buf[pos + 4 * i..pos + 4 * (i + 1)]
                    .copy_from_slice(&((seed + i as u32) as f32).to_le_bytes());
            }
        }
        CfsdType::F64Arr(n) => {
            for i in 0..n {
                buf[pos + 8 * i..pos + 8 * (i + 1)]
                    .copy_from_slice(&(f64::from(seed + i as u32)).to_le_bytes());
            }
        }
        CfsdType::Union(size, variants) => {
            let (tag, ref vty) = variants[seed as usize % variants.len()];
            buf[pos + size..pos + size + 8].copy_from_slice(&tag.to_le_bytes());
            encode_populated_value(buf, pos, vty, seed + 1);
        }
        CfsdType::Str => {
            let s = format!("edge_{seed}");
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            let abs = buf.len();
            buf.extend_from_slice(s.as_bytes());
            buf[pos..pos + 8].copy_from_slice(&((abs - 32) as u64).to_le_bytes());
        }
        CfsdType::List(elem, stride) => {
            // 2 elements to test multi-element stride advancing
            buf.extend_from_slice(&2u64.to_le_bytes());
            let abs = buf.len();
            buf.extend_from_slice(&vec![0u8; stride * 2]);
            buf[pos..pos + 8].copy_from_slice(&((abs - 32) as u64).to_le_bytes());
            encode_populated_value(buf, abs, elem, seed + 1);
            encode_populated_value(buf, abs + stride, elem, seed + 2);
        }
        CfsdType::Dict(k_ty, v_ty, stride, val_off) => {
            // 2 buckets: bucket 0 empty (rel=0), bucket 1 has 2 entries
            buf.extend_from_slice(&2u64.to_le_bytes());
            let dict_abs = buf.len();
            buf.extend_from_slice(&0u64.to_le_bytes()); // bucket 0 rel = 0
            buf.extend_from_slice(&0u64.to_le_bytes()); // bucket 1 rel placeholder
            buf.extend_from_slice(&2u64.to_le_bytes()); // 2 entries in bucket 1
            let entry_abs = buf.len();
            buf.extend_from_slice(&vec![0u8; stride * 2]);
            buf[dict_abs + 8..dict_abs + 16]
                .copy_from_slice(&((entry_abs - 32) as u64).to_le_bytes());
            buf[pos..pos + 8].copy_from_slice(&((dict_abs - 32) as u64).to_le_bytes());
            buf[pos + 8..pos + 16].copy_from_slice(&2u64.to_le_bytes());
            encode_populated_value(buf, entry_abs, k_ty, seed + 10);
            encode_populated_value(buf, entry_abs + val_off, v_ty, seed + 20);
            encode_populated_value(buf, entry_abs + stride, k_ty, seed + 30);
            encode_populated_value(buf, entry_abs + stride + val_off, v_ty, seed + 40);
        }
        CfsdType::Obj(o) => {
            if let Some(m) = o.mask_offset {
                buf[pos + m..pos + m + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            for (idx, attr) in o.attrs.iter().enumerate() {
                encode_populated_value(buf, pos + attr.offset, &attr.ty, seed + idx as u32 + 1);
            }
        }
    }
}

#[test]
fn runtime_constructors_and_layout_metadata_invariants() {
    // Call const fns at runtime (non-const context) so line/region coverage tracks them directly
    let mut attrs = Vec::new();
    for (idx, (name, off, ty, bit)) in [
        ("id", 8usize, U32, 0u32),
        ("optName", 16, Str, 1),
        ("flags", 24, Bool, 2),
        ("list", 32, U32_LIST, 4),
        ("strList", 40, STR_LIST, 8),
    ]
    .into_iter()
    .enumerate()
    {
        let built = a(name, off + idx * 0, ty, bit);
        assert_eq!(built.name, name);
        assert_eq!(built.offset, off);
        assert_eq!(built.bit, bit);
        attrs.push(built);
    }

    let static_attrs: &'static [CfsdAttr] = Box::leak(attrs.into_boxed_slice());
    let built_obj = obj(56, Some(48), static_attrs);
    assert_eq!(built_obj.stride, 56);
    assert_eq!(built_obj.mask_offset, Some(48));
    assert_eq!(built_obj.attrs.len(), 5);

    let bare_rec = rec("res:/staticdata/runtime_test.fsdbinary", Str, built_obj, true);
    assert_eq!(bare_rec.res_path, "res:/staticdata/runtime_test.fsdbinary");
    assert!(bare_rec.bare);
    assert!(!bare_rec.single);

    // Every layout in CFSD_NESTED_LAYOUTS must have non-empty res_path, valid attribute offsets within stride,
    // and unique resource paths.
    let mut seen = std::collections::HashSet::new();
    for lay in CFSD_NESTED_LAYOUTS {
        assert!(seen.insert(lay.res_path), "duplicate layout {}", lay.res_path);
        assert!(lay.res_path.starts_with("res:/staticdata/"));
        assert!(!lay.single);
        if let Some(moff) = lay.record.mask_offset {
            assert!(moff + 4 <= lay.record.stride, "mask oob in {}", lay.res_path);
        }
        if lay.bare {
            assert_eq!(lay.record.attrs.len(), 1, "bare layout {} must have 1 attr", lay.res_path);
        }
        for attr in lay.record.attrs {
            assert!(
                attr.offset < lay.record.stride,
                "{}.{} offset {} >= stride {}",
                lay.res_path,
                attr.name,
                attr.offset,
                lay.record.stride
            );
        }
    }

    assert!(SdeRegistry::cfsd_nested_layout("res:/staticdata/does_not_exist.fsdbinary").is_none());
}

#[test]
fn empty_optional_masks_empty_collections_and_multi_entry_decoding() {
    let mut sde = SdeRegistry::default();
    let dir = std::env::temp_dir().join(format!("evomu_cfsd_edges_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // 1. Optional fields omitted (mask = 0) & empty strings/lists/dicts (rel = 0)
    for (idx, lay) in CFSD_NESTED_LAYOUTS.iter().enumerate() {
        let raw = encode_empty_record_file(lay);
        let p = dir.join(format!("empty_{idx}.fsdbinary"));
        std::fs::write(&p, &raw).unwrap();
        sde.resources.insert(
            lay.res_path.to_string(),
            SdeResourceInfo {
                res_path: lay.res_path.to_string(),
                cdn_relative_path: p.display().to_string(),
                md5: String::new(),
                byte_size: raw.len() as u64,
                format: "cfsd".into(),
                record_count: 1,
                spot: String::new(),
            },
        );
        let decoded = sde
            .load_cfsd_nested(lay.res_path)
            .unwrap_or_else(|e| panic!("empty decode failed for {}: {e}", lay.res_path));
        assert_eq!(decoded.len(), 1);
        if !lay.bare {
            let map = decoded.values().next().unwrap().as_object().unwrap();
            for attr in lay.record.attrs {
                if lay.record.mask_offset.is_some() && attr.bit != 0 {
                    assert!(
                        !map.contains_key(attr.name),
                        "{}: optional attr {} should be omitted when mask=0",
                        lay.res_path,
                        attr.name
                    );
                } else {
                    assert!(
                        map.contains_key(attr.name),
                        "{}: required attr {} missing",
                        lay.res_path,
                        attr.name
                    );
                }
            }
        }
    }

    // 2. Multi-element lists & multi-bucket dicts across all 23 layouts
    for (idx, lay) in CFSD_NESTED_LAYOUTS.iter().enumerate() {
        let mut buf = vec![0u8; 64];
        buf[40..48].copy_from_slice(&1u64.to_le_bytes());
        buf[48..56].copy_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes());
        let rec_abs = buf.len();
        buf[56..64].copy_from_slice(&((rec_abs - 32) as u64).to_le_bytes());
        buf.extend_from_slice(&vec![0u8; lay.record.stride]);
        encode_populated_value(&mut buf, rec_abs, &lay.key, 900);
        if let Some(m) = lay.record.mask_offset {
            buf[rec_abs + m..rec_abs + m + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        for (a_idx, attr) in lay.record.attrs.iter().enumerate() {
            encode_populated_value(&mut buf, rec_abs + attr.offset, &attr.ty, 100 + a_idx as u32);
        }

        let p = dir.join(format!("multi_{idx}.fsdbinary"));
        std::fs::write(&p, &buf).unwrap();
        sde.resources.insert(
            lay.res_path.to_string(),
            SdeResourceInfo {
                res_path: lay.res_path.to_string(),
                cdn_relative_path: p.display().to_string(),
                md5: String::new(),
                byte_size: buf.len() as u64,
                format: "cfsd".into(),
                record_count: 1,
                spot: String::new(),
            },
        );
        let decoded = sde
            .load_cfsd_nested(lay.res_path)
            .unwrap_or_else(|e| panic!("multi decode failed for {}: {e}", lay.res_path));
        assert_eq!(decoded.len(), 1);
    }

    // 3. Fallback & corruption boundary errors
    assert!(matches!(
        sde.load_cfsd_nested("res:/staticdata/unknown_nested.fsdbinary"),
        Err(SdeError::InvalidSchema(_))
    ));

    // Truncated file / byte_size mismatch / invalid UTF-8 string
    let p_bad = dir.join("bad.fsdbinary");
    std::fs::write(&p_bad, &[0u8; 10]).unwrap();
    sde.resources.insert(
        "res:/staticdata/globaltypes.fsdbinary".into(),
        SdeResourceInfo {
            res_path: "res:/staticdata/globaltypes.fsdbinary".into(),
            cdn_relative_path: p_bad.display().to_string(),
            md5: String::new(),
            byte_size: 10,
            format: "cfsd".into(),
            record_count: 1,
            spot: String::new(),
        },
    );
    assert!(matches!(
        sde.load_cfsd_nested("res:/staticdata/globaltypes.fsdbinary"),
        Err(SdeError::InvalidSchema(_))
    ));

    let _ = std::fs::remove_dir_all(&dir);
}
