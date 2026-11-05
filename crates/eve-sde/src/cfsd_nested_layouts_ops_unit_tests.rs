//! Unit and mutation-killing tests for `cfsd_nested_layouts_ops`.

use super::*;
use crate::{SdeRegistry, SdeResourceInfo};

fn populate_type(buf: &mut Vec<u8>, pos: usize, ty: &CfsdType, seed: u32) {
    match *ty {
        CfsdType::Bool => buf[pos] = 1,
        CfsdType::I32 => buf[pos..pos + 4].copy_from_slice(&(-(seed as i32)).to_le_bytes()),
        CfsdType::U32 => buf[pos..pos + 4].copy_from_slice(&seed.to_le_bytes()),
        CfsdType::U64 => buf[pos..pos + 8].copy_from_slice(&u64::from(seed).to_le_bytes()),
        CfsdType::F32 => buf[pos..pos + 4].copy_from_slice(&(seed as f32 + 0.5).to_le_bytes()),
        CfsdType::F64 => buf[pos..pos + 8].copy_from_slice(&(f64::from(seed) + 0.25).to_le_bytes()),
        CfsdType::F32Arr(n) => {
            for i in 0..n {
                buf[pos + 4 * i..pos + 4 * (i + 1)]
                    .copy_from_slice(&((seed + i as u32) as f32).to_le_bytes());
            }
        }
        CfsdType::F64Arr(n) => {
            for i in 0..n {
                buf[pos + 8 * i..pos + 8 * (i + 1)]
                    .copy_from_slice(&f64::from(seed + i as u32).to_le_bytes());
            }
        }
        CfsdType::Union(size, variants) => {
            let (tag, ref vty) = variants[seed as usize % variants.len()];
            buf[pos + size..pos + size + 8].copy_from_slice(&tag.to_le_bytes());
            populate_type(buf, pos, vty, seed + 1);
        }
        CfsdType::Str => {
            let s = format!("ops_{seed}");
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
            populate_type(buf, abs, elem, seed + 1);
        }
        CfsdType::Dict(k_ty, v_ty, stride, val_off) => {
            buf.extend_from_slice(&1u64.to_le_bytes());
            let dict_abs = buf.len();
            buf.extend_from_slice(&0u64.to_le_bytes());
            buf.extend_from_slice(&1u64.to_le_bytes());
            let entry_abs = buf.len();
            buf.extend_from_slice(&vec![0u8; stride]);
            let b_rel = (entry_abs - 32) as u64;
            buf[dict_abs..dict_abs + 8].copy_from_slice(&b_rel.to_le_bytes());
            let d_rel = (dict_abs - 32) as u64;
            buf[pos..pos + 8].copy_from_slice(&d_rel.to_le_bytes());
            buf[pos + 8..pos + 16].copy_from_slice(&1u64.to_le_bytes());
            populate_type(buf, entry_abs, k_ty, seed + 1);
            populate_type(buf, entry_abs + val_off, v_ty, seed + 2);
        }
        CfsdType::Obj(obj) => populate_obj(buf, pos, obj, seed + 1),
    }
}

fn populate_obj(buf: &mut Vec<u8>, pos: usize, obj: &CfsdObject, seed: u32) {
    if let Some(m) = obj.mask_offset {
        buf[pos + m..pos + m + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    for (idx, attr) in obj.attrs.iter().enumerate() {
        populate_type(buf, pos + attr.offset, &attr.ty, seed + idx as u32 + 1);
    }
}

#[test]
fn single_helper_and_all_18_ops_layouts_decode_cleanly() {
    let custom_single = single("res:/staticdata/single_test.fsdbinary", obj(16, None, &[]));
    assert!(custom_single.single);
    assert!(!custom_single.bare);
    assert_eq!(custom_single.record.stride, 16);

    assert_eq!(CFSD_NESTED_LAYOUTS_OPS.len(), 18);
    assert_eq!(ATOM_VALUE.len(), 19);
    assert_eq!(ATOM_VALUE[18].0, 65);
    assert_eq!(CHAIN_DICT_VALUE.len(), 5);
    assert_eq!(CHAIN_VALUE.len(), 12);

    let mut sde = SdeRegistry::default();
    for (idx, lay) in CFSD_NESTED_LAYOUTS_OPS.iter().enumerate() {
        let found = SdeRegistry::cfsd_nested_layout(lay.res_path).expect("find ops layout");
        assert_eq!(found.res_path, lay.res_path);
        assert_eq!(found.single, lay.single);
        assert_eq!(found.bare, lay.bare);
        assert_eq!(found.record.stride, lay.record.stride);
        assert_eq!(found.record.mask_offset, lay.record.mask_offset);

        let raw = if lay.single {
            let mut buf = vec![0u8; 32 + lay.record.stride];
            populate_obj(&mut buf, 32, &lay.record, 20);
            buf
        } else {
            let mut buf = vec![0u8; 64];
            buf[40..48].copy_from_slice(&1u64.to_le_bytes());
            buf[48..56].copy_from_slice(&1u64.to_le_bytes());
            buf.extend_from_slice(&1u64.to_le_bytes());
            let rec_abs = buf.len();
            let boff = (rec_abs - 32) as u64;
            buf[56..64].copy_from_slice(&boff.to_le_bytes());
            buf.extend_from_slice(&vec![0u8; lay.record.stride]);
            populate_type(&mut buf, rec_abs, &lay.key, 900);
            populate_obj(&mut buf, rec_abs, &lay.record, 20);
            buf
        };

        let tmp = std::env::temp_dir().join(format!(
            "evomu_nested_ops_{}_{}.fsdbinary",
            idx,
            std::process::id()
        ));
        std::fs::write(&tmp, &raw).expect("write ops cfsd");
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
            .unwrap_or_else(|e| panic!("failed to decode ops layout {}: {e}", lay.res_path));
        if lay.single {
            assert_eq!(decoded.len(), lay.record.attrs.len());
            for attr in lay.record.attrs {
                assert!(decoded.contains_key(attr.name));
            }
        } else {
            assert_eq!(decoded.len(), 1);
            let val = decoded.values().next().unwrap();
            if lay.bare {
                assert!(!val.is_null());
            } else {
                let map = val.as_object().expect("ops record object");
                for attr in lay.record.attrs {
                    assert!(
                        map.contains_key(attr.name),
                        "{}: missing decoded attr {}",
                        lay.res_path,
                        attr.name
                    );
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
}
