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

fn build_cfsd_with_records(
    lay: &CfsdNestedLayout,
    seeds: &[u32],
    mask_override: Option<u32>,
    union_variant_idx: Option<usize>,
) -> Vec<u8> {
    fn pop_type(
        buf: &mut Vec<u8>,
        pos: usize,
        ty: &CfsdType,
        seed: u32,
        mask_override: Option<u32>,
        union_variant_idx: Option<usize>,
    ) {
        match *ty {
            CfsdType::Bool => buf[pos] = 1,
            CfsdType::I32 => buf[pos..pos + 4].copy_from_slice(&(-(seed as i32)).to_le_bytes()),
            CfsdType::U32 => buf[pos..pos + 4].copy_from_slice(&seed.to_le_bytes()),
            CfsdType::U64 => buf[pos..pos + 8].copy_from_slice(&u64::from(seed).to_le_bytes()),
            CfsdType::F32 => buf[pos..pos + 4].copy_from_slice(&(seed as f32 + 0.5).to_le_bytes()),
            CfsdType::F64 => {
                buf[pos..pos + 8].copy_from_slice(&(f64::from(seed) + 0.25).to_le_bytes())
            }
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
                let idx = union_variant_idx.unwrap_or(seed as usize) % variants.len();
                let (tag, ref vty) = variants[idx];
                buf[pos + size..pos + size + 8].copy_from_slice(&tag.to_le_bytes());
                pop_type(buf, pos, vty, seed + 1, mask_override, union_variant_idx);
            }
            CfsdType::Str => {
                if mask_override == Some(0) {
                    return;
                }
                let s = format!("v_{seed}");
                buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
                let abs = buf.len();
                buf.extend_from_slice(s.as_bytes());
                let rel = (abs - 32) as u64;
                buf[pos..pos + 8].copy_from_slice(&rel.to_le_bytes());
            }
            CfsdType::List(elem, stride) => {
                if mask_override == Some(0) {
                    return;
                }
                buf.extend_from_slice(&1u64.to_le_bytes());
                let abs = buf.len();
                buf.extend_from_slice(&vec![0u8; stride]);
                let rel = (abs - 32) as u64;
                buf[pos..pos + 8].copy_from_slice(&rel.to_le_bytes());
                pop_type(buf, abs, elem, seed + 1, mask_override, union_variant_idx);
            }
            CfsdType::Dict(k_ty, v_ty, stride, val_off) => {
                if mask_override == Some(0) {
                    return;
                }
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
                pop_type(buf, entry_abs, k_ty, seed + 1, mask_override, union_variant_idx);
                pop_type(
                    buf,
                    entry_abs + val_off,
                    v_ty,
                    seed + 2,
                    mask_override,
                    union_variant_idx,
                );
            }
            CfsdType::Obj(obj) => pop_obj(buf, pos, obj, seed + 1, mask_override, union_variant_idx),
        }
    }

    fn pop_obj(
        buf: &mut Vec<u8>,
        pos: usize,
        obj: &CfsdObject,
        seed: u32,
        mask_override: Option<u32>,
        union_variant_idx: Option<usize>,
    ) {
        let mask = mask_override.unwrap_or(u32::MAX);
        if let Some(m) = obj.mask_offset {
            buf[pos + m..pos + m + 4].copy_from_slice(&mask.to_le_bytes());
        }
        for (idx, attr) in obj.attrs.iter().enumerate() {
            if attr.bit != 0 && (mask & attr.bit) == 0 {
                continue;
            }
            pop_type(
                buf,
                pos + attr.offset,
                &attr.ty,
                seed + idx as u32 + 1,
                mask_override,
                union_variant_idx,
            );
        }
    }

    if lay.single {
        let mut buf = vec![0u8; 32 + lay.record.stride];
        pop_obj(
            &mut buf,
            32,
            &lay.record,
            seeds[0],
            mask_override,
            union_variant_idx,
        );
        return buf;
    }

    let n = seeds.len();
    let mut buf = vec![0u8; 56 + n * 8];
    buf[40..48].copy_from_slice(&(n as u64).to_le_bytes());
    for (i, &seed) in seeds.iter().enumerate() {
        buf[48 + i * 8..56 + i * 8].copy_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes());
        let rec_abs = buf.len();
        let boff = (rec_abs - 32) as u64;
        buf[56 + i * 8..64 + i * 8].copy_from_slice(&boff.to_le_bytes());
        buf.extend_from_slice(&vec![0u8; lay.record.stride]);
        // Force unique non-empty key regardless of mask_override
        pop_type(&mut buf, rec_abs, &lay.key, 900 + i as u32, None, None);
        pop_obj(
            &mut buf,
            rec_abs,
            &lay.record,
            seed,
            mask_override,
            union_variant_idx,
        );
    }
    buf
}

fn load_synthetic(res_path: &str, raw: &[u8], tag: &str) -> Result<std::collections::BTreeMap<String, serde_json::Value>, crate::SdeError> {
    let tmp = std::env::temp_dir().join(format!(
        "evomu_ops_{tag}_{}.fsdbinary",
        std::process::id()
    ));
    std::fs::write(&tmp, raw).expect("write tmp cfsd");
    let mut sde = SdeRegistry::default();
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: tmp.display().to_string(),
            md5: String::new(),
            byte_size: raw.len() as u64,
            format: "cfsd".to_string(),
            record_count: 1,
            spot: String::new(),
        },
    );
    let res = sde.load_cfsd_nested(res_path);
    let _ = std::fs::remove_file(&tmp);
    res
}

#[test]
fn ops_layouts_exhaustive_unions_zero_masks_invariants_and_error_paths() {
    // 1. Structural layout invariants for every entry in CFSD_NESTED_LAYOUTS_OPS
    let mut seen_paths = std::collections::HashSet::new();
    for lay in CFSD_NESTED_LAYOUTS_OPS {
        assert!(seen_paths.insert(lay.res_path), "duplicate {}", lay.res_path);
        assert!(!lay.record.attrs.is_empty(), "empty attrs on {}", lay.res_path);
        if let Some(m) = lay.record.mask_offset {
            assert!(
                m + 4 <= lay.record.stride,
                "mask out of bounds on {}",
                lay.res_path
            );
        }
        for attr in lay.record.attrs {
            assert!(
                attr.offset < lay.record.stride,
                "attr {} offset {} >= stride {} on {}",
                attr.name,
                attr.offset,
                lay.record.stride,
                lay.res_path
            );
        }
    }
    assert!(SdeRegistry::cfsd_nested_layout("res:/staticdata/unknown_ops.fsdbinary").is_none());

    // 2. Exhaustively exercise all 19 ATOM_VALUE variants on atoms & nodetypes
    let atoms_lay = SdeRegistry::cfsd_nested_layout("res:/staticdata/atoms.fsdbinary").unwrap();
    for v_idx in 0..ATOM_VALUE.len() {
        let raw = build_cfsd_with_records(atoms_lay, &[10 + v_idx as u32], None, Some(v_idx));
        let dec = load_synthetic(atoms_lay.res_path, &raw, &format!("atom_{v_idx}"))
            .unwrap_or_else(|e| panic!("atom variant {v_idx} failed: {e}"));
        assert_eq!(dec.len(), 1);
    }

    // 3. Exhaustively exercise all 12 CHAIN_VALUE & 5 CHAIN_DICT_VALUE variants on objectivechains
    let chains_lay =
        SdeRegistry::cfsd_nested_layout("res:/staticdata/objectivechains.fsdbinary").unwrap();
    for v_idx in 0..CHAIN_VALUE.len() {
        let raw = build_cfsd_with_records(chains_lay, &[30 + v_idx as u32], None, Some(v_idx));
        let dec = load_synthetic(chains_lay.res_path, &raw, &format!("chain_{v_idx}"))
            .unwrap_or_else(|e| panic!("chain variant {v_idx} failed: {e}"));
        assert_eq!(dec.len(), 1);
    }

    // 4. Optional mask == 0 across all 18 layouts (omits bit != 0 attributes, exercises rel==0 empty lists/dicts/strs)
    for (idx, lay) in CFSD_NESTED_LAYOUTS_OPS.iter().enumerate() {
        let raw = build_cfsd_with_records(lay, &[50], Some(0), Some(0));
        let dec = load_synthetic(lay.res_path, &raw, &format!("mask0_{idx}"))
            .unwrap_or_else(|e| panic!("mask=0 failed for {}: {e}", lay.res_path));
        if lay.single {
            for attr in lay.record.attrs {
                assert_eq!(dec.contains_key(attr.name), attr.bit == 0);
            }
        } else {
            let val = dec.values().next().unwrap();
            if !lay.bare {
                let obj = val.as_object().unwrap();
                for attr in lay.record.attrs {
                    assert_eq!(
                        obj.contains_key(attr.name),
                        attr.bit == 0,
                        "mask=0 mismatch for {}.{}",
                        lay.res_path,
                        attr.name
                    );
                }
            }
        }
    }

    // 5. Unknown union tag fails loudly with InvalidSchema
    let mut bad_union_raw = build_cfsd_with_records(atoms_lay, &[10], None, Some(0));
    // Overwrite the last u64 union tag occurrences? Build an atom with tag 999999 directly
    // Locate the first List(&Obj(&ATOM_INPUT), 64) element inside atoms_lay record:
    let rec_abs = u64::from_le_bytes(bad_union_raw[56..64].try_into().unwrap()) as usize + 32;
    let inputs_rel = u64::from_le_bytes(bad_union_raw[rec_abs + 24..rec_abs + 32].try_into().unwrap()) as usize;
    let atom_input_abs = inputs_rel + 32;
    bad_union_raw[atom_input_abs + 24..atom_input_abs + 32]
        .copy_from_slice(&999_999u64.to_le_bytes());
    let union_err = load_synthetic(atoms_lay.res_path, &bad_union_raw, "bad_union").unwrap_err();
    assert!(matches!(union_err, crate::SdeError::InvalidSchema(_)));

    // 6. Error paths: header record count mismatch, truncated buffer, and size pin mismatch
    let mut bad_count_raw = build_cfsd_with_records(atoms_lay, &[10], None, Some(0));
    bad_count_raw[40..48].copy_from_slice(&2u64.to_le_bytes());
    assert!(matches!(
        load_synthetic(atoms_lay.res_path, &bad_count_raw, "bad_cnt"),
        Err(crate::SdeError::InvalidSchema(_))
    ));

    let trunc_raw = vec![0u8; 16];
    assert!(matches!(
        load_synthetic(atoms_lay.res_path, &trunc_raw, "trunc"),
        Err(crate::SdeError::InvalidSchema(_))
    ));
}
