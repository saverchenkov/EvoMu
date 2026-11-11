use super::*;
use crate::{CfsdAttr, SdeError, SdeRegistry, SdeResourceInfo};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct CfsdBuilder {
    buf: Vec<u8>,
    seq: u64,
    union_cursor: HashMap<usize, usize>,
}

impl CfsdBuilder {
    fn new(header_size: usize) -> Self {
        Self {
            buf: vec![0u8; header_size],
            seq: 1,
            union_cursor: HashMap::new(),
        }
    }

    fn alloc_counted(&mut self, count: usize, elem_bytes: usize) -> (u64, usize) {
        while self.buf.len() % 8 != 0 {
            self.buf.push(0);
        }
        // 8-byte count prefix before `abs`.
        let count_pos = self.buf.len();
        self.buf.resize(count_pos + 8 + count * elem_bytes, 0);
        self.put_u64(count_pos, count as u64);
        let abs = count_pos + 8;
        ((abs - 32) as u64, abs)
    }

    fn put_u8(&mut self, pos: usize, v: u8) {
        self.buf[pos] = v;
    }

    fn put_u32(&mut self, pos: usize, v: u32) {
        self.buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u64(&mut self, pos: usize, v: u64) {
        self.buf[pos..pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn put_f32(&mut self, pos: usize, v: f32) {
        self.buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_f64(&mut self, pos: usize, v: f64) {
        self.buf[pos..pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn write_string(&mut self, pos: usize, s: &str) {
        let bytes = s.as_bytes();
        let (rel, abs) = self.alloc_counted(1, bytes.len().max(1));
        // Overwrite count at abs - 8 with actual byte length.
        self.put_u64(abs - 8, bytes.len() as u64);
        self.buf[abs..abs + bytes.len()].copy_from_slice(bytes);
        self.put_u64(pos, rel);
    }

    fn next_union_index(&mut self, variants: &'static [(u64, CfsdType)], depth: usize) -> usize {
        if depth >= 3 {
            if let Some((idx, _)) = variants.iter().enumerate().find(|(_, (_, ty))| {
                !matches!(ty, CfsdType::Obj(_) | CfsdType::List(_, _) | CfsdType::Dict(_, _, _, _) | CfsdType::Union(_, _))
            }) {
                return idx;
            }
        }
        let ptr = variants.as_ptr() as usize;
        let idx = self.union_cursor.get(&ptr).copied().unwrap_or(0);
        self.union_cursor.insert(ptr, (idx + 1) % variants.len());
        idx
    }

    fn write_type(&mut self, pos: usize, ty: &CfsdType, include_opt: bool, empty_containers: bool, depth: usize) {
        match *ty {
            CfsdType::Bool => self.put_u8(pos, 1),
            CfsdType::I32 => self.put_u32(pos, (-7_i32) as u32),
            CfsdType::U32 => {
                let v = self.next_seq() as u32;
                self.put_u32(pos, v);
            }
            CfsdType::U64 => {
                let v = self.next_seq();
                self.put_u64(pos, v);
            }
            CfsdType::F32 => self.put_f32(pos, 1.5),
            CfsdType::F64 => self.put_f64(pos, 2.25),
            CfsdType::F32Arr(n) => {
                for i in 0..n {
                    self.put_f32(pos + i * 4, (i as f32) + 0.5);
                }
            }
            CfsdType::F64Arr(n) => {
                for i in 0..n {
                    self.put_f64(pos + i * 8, (i as f64) + 1.25);
                }
            }
            CfsdType::Str => {
                let s = format!("s{}", self.next_seq());
                self.write_string(pos, &s);
            }
            CfsdType::Union(size, variants) => {
                let idx = self.next_union_index(variants, depth);
                let (tag, ref sub_ty) = variants[idx];
                self.put_u64(pos + size, tag);
                self.write_type(pos, sub_ty, include_opt, empty_containers, depth + 1);
            }
            CfsdType::List(elem, stride) => {
                if empty_containers || depth >= 4 {
                    self.put_u64(pos, 0);
                } else {
                    let count = 2usize;
                    let (rel, at) = self.alloc_counted(count, stride);
                    self.put_u64(pos, rel);
                    for i in 0..count {
                        self.write_type(at + i * stride, elem, include_opt, empty_containers, depth + 1);
                    }
                }
            }
            CfsdType::Dict(key, val, stride, val_off) => {
                if empty_containers || depth >= 4 {
                    self.put_u64(pos, 0);
                    self.put_u64(pos + 8, 0);
                } else {
                    let count = 1usize;
                    let (bucket_rel, bucket_abs) = self.alloc_counted(1, 8);
                    let (entries_rel, entries_abs) = self.alloc_counted(count, stride);
                    self.put_u64(bucket_abs, entries_rel);
                    self.put_u64(pos, bucket_rel);
                    self.put_u64(pos + 8, count as u64);
                    self.write_type(entries_abs, key, include_opt, empty_containers, depth + 1);
                    self.write_type(entries_abs + val_off, val, include_opt, empty_containers, depth + 1);
                }
            }
            CfsdType::Obj(ref obj) => self.write_obj(pos, obj, include_opt, empty_containers, depth + 1),
        }
    }

    fn write_obj(&mut self, pos: usize, obj: &CfsdObject, include_opt: bool, empty_containers: bool, depth: usize) {
        if let Some(m) = obj.mask_offset {
            self.put_u32(pos + m, if include_opt { u32::MAX } else { 0 });
        }
        for a in obj.attrs {
            if a.bit != 0 && !include_opt {
                continue;
            }
            self.write_type(pos + a.offset, &a.ty, include_opt, empty_containers, depth);
        }
    }

    fn build_layout(
        lay: &CfsdNestedLayout,
        record_count: usize,
        include_opt: bool,
        empty_containers: bool,
    ) -> Vec<u8> {
        if lay.single {
            let mut b = Self::new(32 + lay.record.stride);
            b.write_obj(32, &lay.record, include_opt, empty_containers, 0);
            return b.buf;
        }
        // Header: 0..32 reserved, 40 = count, 48 = buckets (1), 56 = bucket[0] rel
        let mut b = Self::new(64);
        b.put_u64(40, record_count as u64);
        b.put_u64(48, 1);
        if record_count == 0 {
            b.put_u64(56, 0);
            return b.buf;
        }
        let (rel, entries_abs) = b.alloc_counted(record_count, lay.record.stride);
        b.put_u64(56, rel);
        for i in 0..record_count {
            let rec = entries_abs + i * lay.record.stride;
            // Write record fields first, then always write a unique key at offset 0.
            b.write_obj(rec, &lay.record, include_opt, empty_containers, 0);
            b.write_type(rec, &lay.key, false, false, 0);
        }
        b.buf
    }
}

struct TempCfsdFile {
    path: std::path::PathBuf,
}

impl TempCfsdFile {
    fn write(tag: &str, bytes: &[u8]) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "evomu_cfsd_edges_{tag}_{}_{seq}.fsdbinary",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("write temp cfsd");
        Self { path }
    }
}

impl Drop for TempCfsdFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn registry_for_file(res_path: &str, file_path: &std::path::Path, byte_size: u64) -> SdeRegistry {
    let mut reg = SdeRegistry::default();
    reg.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: file_path.to_string_lossy().into_owned(),
            md5: String::new(),
            byte_size,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    reg
}

#[test]
fn load_all_ops_layouts_with_optionals_empty_containers_and_all_union_tags() {
    for lay in CFSD_NESTED_LAYOUTS_OPS {
        // Need enough records for union cycling to cover every variant in ATOM_VALUE (19),
        // CHAIN_DICT_VALUE (5), and CHAIN_VALUE (12).
        let rec_count = if lay.single { 1 } else { 22 };

        // 1) All optional fields enabled + populated lists/dicts + all union variants.
        let raw_full = CfsdBuilder::build_layout(lay, rec_count, true, false);
        let f_full = TempCfsdFile::write("full", &raw_full);
        let reg_full = registry_for_file(lay.res_path, &f_full.path, raw_full.len() as u64);
        let decoded_full = reg_full
            .load_cfsd_nested(lay.res_path)
            .unwrap_or_else(|e| panic!("full load {}: {e}", lay.res_path));
        if lay.single {
            assert!(!decoded_full.is_empty(), "single {} non-empty", lay.res_path);
        } else {
            assert_eq!(decoded_full.len(), rec_count, "count mismatch on {}", lay.res_path);
        }

        // 2) Optional mask = 0 (all optional fields omitted) + empty lists/dicts.
        let raw_min = CfsdBuilder::build_layout(lay, if lay.single { 1 } else { 1 }, false, true);
        let f_min = TempCfsdFile::write("min", &raw_min);
        let reg_min = registry_for_file(lay.res_path, &f_min.path, raw_min.len() as u64);
        let decoded_min = reg_min
            .load_cfsd_nested(lay.res_path)
            .unwrap_or_else(|e| panic!("min load {}: {e}", lay.res_path));
        if !lay.single {
            assert_eq!(decoded_min.len(), 1);
        }
    }

    // Also exercise empty dict file (0 records).
    let dict_lay = CFSD_NESTED_LAYOUTS_OPS
        .iter()
        .find(|l| !l.single)
        .expect("dict layout");
    let raw_empty = CfsdBuilder::build_layout(dict_lay, 0, false, true);
    let f_empty = TempCfsdFile::write("empty", &raw_empty);
    let reg_empty = registry_for_file(dict_lay.res_path, &f_empty.path, raw_empty.len() as u64);
    assert!(reg_empty.load_cfsd_nested(dict_lay.res_path).unwrap().is_empty());
}

#[test]
fn single_helper_and_cfsd_error_paths() {
    let custom = single(
        "res:/staticdata/seasonselection.fsdbinary",
        CfsdObject {
            stride: 8,
            mask_offset: None,
            attrs: &[CfsdAttr {
                name: "flag",
                offset: 0,
                ty: CfsdType::Bool,
                bit: 0,
            }],
        },
    );
    assert!(custom.single);
    assert!(!custom.bare);

    // 1) Size differs from importer pin.
    let lay = CFSD_NESTED_LAYOUTS_OPS
        .iter()
        .find(|l| !l.single)
        .unwrap();
    let raw = CfsdBuilder::build_layout(lay, 1, false, true);
    let f = TempCfsdFile::write("err_size", &raw);
    let reg_bad_size = registry_for_file(lay.res_path, &f.path, (raw.len() + 1) as u64);
    assert!(matches!(
        reg_bad_size.load_cfsd_nested(lay.res_path),
        Err(SdeError::InvalidSchema(_))
    ));

    // 2) Record count mismatch in header.
    let mut bad_count = raw.clone();
    bad_count[40..48].copy_from_slice(&99u64.to_le_bytes());
    let f_cnt = TempCfsdFile::write("err_cnt", &bad_count);
    let reg_cnt = registry_for_file(lay.res_path, &f_cnt.path, bad_count.len() as u64);
    assert!(matches!(
        reg_cnt.load_cfsd_nested(lay.res_path),
        Err(SdeError::InvalidSchema(_))
    ));

    // 3) Truncated buffer returns error.
    let truncated = vec![0u8; 44];
    let f_trunc = TempCfsdFile::write("err_trunc", &truncated);
    let reg_trunc = registry_for_file(lay.res_path, &f_trunc.path, truncated.len() as u64);
    assert!(reg_trunc.load_cfsd_nested(lay.res_path).is_err());

    // 4) Unknown union tag in CHAIN_VALUE (operations.fsdbinary has CHAIN_VALUE or ATOM_VALUE).
    let union_lay = CFSD_NESTED_LAYOUTS_OPS
        .iter()
        .find(|l| {
            !l.single
                && l.record
                    .attrs
                    .iter()
                    .any(|a| matches!(a.ty, CfsdType::Union(_, _) | CfsdType::Dict(_, _, _, _) | CfsdType::Obj(_)))
        })
        .unwrap();
    // Corrupt union tag on a known union-bearing layout (res:/staticdata/skillplans/milestones.fsdbinary
    // or res:/staticdata/operations.fsdbinary or any layout with ATOM_VALUE).
    if let Some(atom_lay) = CFSD_NESTED_LAYOUTS_OPS
        .iter()
        .find(|l| l.res_path == "res:/staticdata/operationtasks.fsdbinary")
    {
        // Build with corrupted tag bytes inside the buffer: easiest is invalid UTF-8 test + union tag error.
        let mut raw_u = CfsdBuilder::build_layout(atom_lay, 1, true, false);
        // Find where bucket 0 entries start: u64 at 56 + 32.
        let rel = u64::from_le_bytes(raw_u[56..64].try_into().unwrap()) as usize;
        let rec0 = rel + 32;
        if let Some(atom_attr) = atom_lay.record.attrs.iter().find(|a| matches!(a.ty, CfsdType::Union(_, _))) {
            if let CfsdType::Union(sz, _) = atom_attr.ty {
                let tag_pos = rec0 + atom_attr.offset + sz;
                raw_u[tag_pos..tag_pos + 8].copy_from_slice(&999_999u64.to_le_bytes());
                let f_tag = TempCfsdFile::write("bad_tag", &raw_u);
                let reg_tag = registry_for_file(atom_lay.res_path, &f_tag.path, raw_u.len() as u64);
                assert!(matches!(
                    reg_tag.load_cfsd_nested(atom_lay.res_path),
                    Err(SdeError::InvalidSchema(_))
                ));
            }
        }
    }

    // 5) Invalid UTF-8 in CfsdType::Str attribute fails cleanly.
    let mut b = CfsdBuilder::new(64);
    b.put_u64(40, 1);
    b.put_u64(48, 1);
    let (rel, rec) = b.alloc_counted(1, lay.record.stride);
    b.put_u64(56, rel);
    b.write_obj(rec, &lay.record, true, true, 0);
    if let Some(str_attr) = lay.record.attrs.iter().find(|a| matches!(a.ty, CfsdType::Str)) {
        let (s_rel, s_abs) = b.alloc_counted(1, 4);
        b.put_u64(s_abs - 8, 2);
        b.buf[s_abs] = 0xFF;
        b.buf[s_abs + 1] = 0xFE;
        b.put_u64(rec + str_attr.offset, s_rel);
        let f_utf8 = TempCfsdFile::write("bad_utf8", &b.buf);
        let reg_utf8 = registry_for_file(lay.res_path, &f_utf8.path, b.buf.len() as u64);
        assert!(reg_utf8.load_cfsd_nested(lay.res_path).is_err());
    }
    let _ = union_lay;
    let _ = Value::Null;
}
