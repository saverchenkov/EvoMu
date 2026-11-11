//! Edge-case and mutation-killing unit tests for `cfsd_domain_loaders` (`cfsd_records`)
//! and `static_domain_loaders` (`fsd_dict_entries`).

use crate::static_domain_loaders::fsd_dict_entries;
use crate::{SdeError, SdeRegistry, SdeResourceInfo};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

fn make_temp_sde_with_resource(res_path: &str, bytes: &[u8]) -> (PathBuf, SdeRegistry) {
    let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "evomu_sde_cfsd_edge_test_{}_{}",
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp sde test dir");
    let db_path = dir.join("evomu_sde.sqlite");
    fs::write(&db_path, b"").expect("create marker db file");
    let blob_path = dir.join("test_blob.fsdbinary");
    fs::write(&blob_path, bytes).expect("write test fsdbinary blob");

    let mut resources = HashMap::new();
    resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: blob_path.display().to_string(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: "fsdbinary".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    let sde = SdeRegistry {
        db_path: Some(db_path),
        resources,
        ..SdeRegistry::default()
    };
    (dir, sde)
}

#[test]
fn cfsd_records_validates_header_stride_bucket_bounds_and_record_offsets() {
    let res = "res:/staticdata/test_cfsd_records.fsdbinary";

    // 1. Buffer shorter than 56 bytes (55 bytes) with non-zero stride must return empty without OOB panic.
    let (dir1, sde1) = make_temp_sde_with_resource(res, &[0u8; 55]);
    let (_, recs1) = sde1.read_cfsd_bytes(res, 8).expect("short buffer");
    assert!(recs1.is_empty());
    let _ = fs::remove_dir_all(&dir1);

    // 2. Valid >= 64-byte CFSD buffer with stride == 0 must return empty rather than pushing duplicate offsets.
    let mut buf_zero_stride = vec![0u8; 80];
    buf_zero_stride[48..56].copy_from_slice(&1u64.to_le_bytes()); // 1 bucket
    buf_zero_stride[56..64].copy_from_slice(&40u64.to_le_bytes()); // boff=40 -> abs=72, cnt_pos=64
    buf_zero_stride[64..72].copy_from_slice(&1u64.to_le_bytes()); // n=1 record
    let (dir2, sde2) = make_temp_sde_with_resource(res, &buf_zero_stride);
    let (_, recs2) = sde2.read_cfsd_bytes(res, 0).expect("zero stride");
    assert!(recs2.is_empty());
    let _ = fs::remove_dir_all(&dir2);

    // 3. Bucket count in header exceeding `(raw.len() - 56) / 8` must be clamped to `max_buckets`.
    //    With `raw.len() == 64`, `max_buckets = (64 - 56) / 8 = 1` (whereas `* 8` would allow 2 and OOB).
    let mut buf_clamped_buckets = vec![0u8; 64];
    buf_clamped_buckets[48..56].copy_from_slice(&2u64.to_le_bytes()); // claims 2 buckets, only 1 fits
    buf_clamped_buckets[56..64].copy_from_slice(&0u64.to_le_bytes()); // bucket 0 empty
    let (dir3, sde3) = make_temp_sde_with_resource(res, &buf_clamped_buckets);
    let (_, recs3) = sde3.read_cfsd_bytes(res, 8).expect("clamped bucket count");
    assert!(recs3.is_empty());
    let _ = fs::remove_dir_all(&dir3);

    // 4. Bucket whose `cnt_pos + 8 == raw.len() + 1` (73 > 72) must be skipped cleanly without OOB panic.
    let mut buf_cnt_oob = vec![0u8; 72];
    buf_cnt_oob[48..56].copy_from_slice(&1u64.to_le_bytes());
    buf_cnt_oob[56..64].copy_from_slice(&41u64.to_le_bytes()); // abs=73, cnt_pos=65, end=73 > 72
    let (dir4, sde4) = make_temp_sde_with_resource(res, &buf_cnt_oob);
    let (_, recs4) = sde4.read_cfsd_bytes(res, 8).expect("cnt_pos OOB");
    assert!(recs4.is_empty());
    let _ = fs::remove_dir_all(&dir4);

    // 5. Multi-bucket CFSD buffer with empty bucket + populated bucket + trailing partial record.
    let mut buf_valid = vec![0u8; 92];
    buf_valid[48..56].copy_from_slice(&2u64.to_le_bytes()); // 2 buckets
    buf_valid[56..64].copy_from_slice(&0u64.to_le_bytes()); // bucket 0 empty
    buf_valid[64..72].copy_from_slice(&48u64.to_le_bytes()); // bucket 1: abs=32+48=80, cnt_pos=72
    buf_valid[72..80].copy_from_slice(&3u64.to_le_bytes()); // claims 3 records of stride 6 (80..98), only 2 fit (80..92)
    let (dir5, sde5) = make_temp_sde_with_resource(res, &buf_valid);
    let (_, recs5) = sde5.read_cfsd_bytes(res, 6).expect("valid multi-bucket");
    assert_eq!(recs5, vec![80, 86]);
    let _ = fs::remove_dir_all(&dir5);

    // 6. Missing resource path returns `Err(SdeError::InvalidSchema(_))`.
    assert!(matches!(
        sde5.read_cfsd_bytes("res:/staticdata/nonexistent.fsdbinary", 8),
        Err(SdeError::InvalidSchema(_))
    ));
}

#[test]
fn fsd_dict_entries_validates_bounds_item_size_and_exact_footer_boundary() {
    // 1. Short buffer where `pos + 4 > buf.len()` (3 bytes at pos=0 -> 4 > 3) returns empty without panic.
    assert!(fsd_dict_entries(&[0u8; 3], 0, 8).is_empty());

    // 2. Entry table where `at + 8 == body_end + 1` (20 > 19) breaks cleanly before reading OOB.
    let mut buf_entry_oob = vec![0u8; 19];
    buf_entry_oob[0..4].copy_from_slice(&15u32.to_le_bytes()); // body_start=4, body_end=19, footer_len_pos=15
    buf_entry_oob[8..12].copy_from_slice(&1u32.to_le_bytes()); // cnt=1 at footer=8
    buf_entry_oob[15..19].copy_from_slice(&7u32.to_le_bytes()); // footer_size=7 -> footer=15-7=8
    assert!(fsd_dict_entries(&buf_entry_oob, 0, 8).is_empty());

    // 3. Exact boundary where `at + 8 == body_end` (12 + 8 == 20 == body_end) MUST decode the entry!
    let mut buf_exact_end = vec![0u8; 20];
    buf_exact_end[0..4].copy_from_slice(&16u32.to_le_bytes()); // body_start=4, body_end=20, footer_len_pos=16
    buf_exact_end[8..12].copy_from_slice(&1u32.to_le_bytes()); // cnt=1 at footer=16-8=8
    buf_exact_end[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // key at at=12
    buf_exact_end[16..20].copy_from_slice(&8u32.to_le_bytes()); // overlaps footer_size=8 -> val_off=4+8=12 <= 20
    assert_eq!(fsd_dict_entries(&buf_exact_end, 0, 8), vec![(0x1234_5678, 12)]);

    // 4. Multi-entry dict with item_size=12 and an out-of-range value offset filtered out.
    let mut buf_multi = vec![0u8; 40];
    buf_multi[0..4].copy_from_slice(&36u32.to_le_bytes()); // body_start=4, body_end=40, footer_len_pos=36
    // footer_size = 28 -> footer = 36 - 28 = 8 (4 bytes cnt + 2 * 12-byte entries = 28 bytes)
    buf_multi[8..12].copy_from_slice(&2u32.to_le_bytes()); // cnt=2
    buf_multi[12..16].copy_from_slice(&101u32.to_le_bytes()); // key 0
    buf_multi[16..20].copy_from_slice(&0u32.to_le_bytes()); // val_off = 4 + 0 = 4 (valid)
    buf_multi[24..28].copy_from_slice(&202u32.to_le_bytes()); // key 1
    buf_multi[28..32].copy_from_slice(&999u32.to_le_bytes()); // val_off = 4 + 999 > 40 (filtered)
    buf_multi[36..40].copy_from_slice(&28u32.to_le_bytes()); // footer_size = 28
    assert_eq!(fsd_dict_entries(&buf_multi, 0, 12), vec![(101, 4)]);
}

fn build_cfsd_blob(records: &[&[u8]]) -> Vec<u8> {
    let rec_size = records.first().map_or(0, |r| r.len());
    let total = 80 + records.len() * rec_size;
    let mut buf = vec![0u8; total];
    buf[48..56].copy_from_slice(&1u64.to_le_bytes()); // 1 bucket
    buf[56..64].copy_from_slice(&48u64.to_le_bytes()); // boff=48 -> abs=80, cnt_pos=72
    buf[72..80].copy_from_slice(&(records.len() as u64).to_le_bytes());
    for (i, rec) in records.iter().enumerate() {
        let start = 80 + i * rec_size;
        buf[start..start + rec_size].copy_from_slice(rec);
    }
    buf
}

#[test]
fn localized_group_category_and_market_group_cfsd_id_lookups() {
    let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "evomu_sde_loc_ids_test_{}_{}",
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp loc dir");

    let mut grp_rec = [0u8; 32];
    grp_rec[0..4].copy_from_slice(&25u32.to_le_bytes());
    grp_rec[12..16].copy_from_slice(&501u32.to_le_bytes());

    let mut cat_rec = [0u8; 48];
    cat_rec[0..4].copy_from_slice(&6u32.to_le_bytes());
    cat_rec[20..24].copy_from_slice(&502u32.to_le_bytes());

    let mut mg1 = [0u8; 28];
    mg1[0..4].copy_from_slice(&9u32.to_le_bytes());
    mg1[4..8].copy_from_slice(&503u32.to_le_bytes());
    mg1[12..16].copy_from_slice(&504u32.to_le_bytes());
    mg1[24..28].copy_from_slice(&1u32.to_le_bytes()); // (mask & 1) != 0, desc_id = 503

    let mut mg2 = [0u8; 28];
    mg2[0..4].copy_from_slice(&10u32.to_le_bytes());
    mg2[4..8].copy_from_slice(&505u32.to_le_bytes());
    mg2[12..16].copy_from_slice(&506u32.to_le_bytes());
    mg2[24..28].copy_from_slice(&2u32.to_le_bytes()); // (mask & 1) == 0 even though mask == 2

    let mut mg3 = [0u8; 28];
    mg3[0..4].copy_from_slice(&11u32.to_le_bytes());
    mg3[4..8].copy_from_slice(&0u32.to_le_bytes()); // desc_id == 0 excluded
    mg3[12..16].copy_from_slice(&507u32.to_le_bytes());
    mg3[24..28].copy_from_slice(&1u32.to_le_bytes());

    let loc_pickle = b"(dp0\nI501\n(VFrigate\np1\nsI502\n(VShip\np2\nsI503\n(VSmall Hulls\np3\nsI504\n(VStandard Frigates\np4\nsI505\n(VMasked Out\np5\nsI506\n(VNo Desc Group\np6\nsI507\n(VZero Desc Group\np7\ns.";

    let entries: &[(&str, &str, Vec<u8>)] = &[
        (
            "res:/staticdata/groups.fsdbinary",
            "groups.bin",
            build_cfsd_blob(&[&grp_rec]),
        ),
        (
            "res:/staticdata/categories.fsdbinary",
            "categories.bin",
            build_cfsd_blob(&[&cat_rec]),
        ),
        (
            "res:/staticdata/marketgroups.fsdbinary",
            "marketgroups.bin",
            build_cfsd_blob(&[&mg1, &mg2, &mg3]),
        ),
        (
            "res:/localizationfsd/localization_fsd_en-us.pickle",
            "loc_en.pickle",
            loc_pickle.to_vec(),
        ),
    ];

    let mut resources = HashMap::new();
    for (res_path, fname, data) in entries {
        let fpath = dir.join(fname);
        fs::write(&fpath, data).expect("write test fixture");
        resources.insert(
            (*res_path).to_string(),
            SdeResourceInfo {
                res_path: (*res_path).to_string(),
                cdn_relative_path: fpath.display().to_string(),
                md5: String::new(),
                byte_size: data.len() as u64,
                format: "bin".to_string(),
                record_count: 0,
                spot: String::new(),
            },
        );
    }

    let sde = SdeRegistry {
        resources,
        ..SdeRegistry::default()
    };

    assert_eq!(sde.localized_group_name("en", 25).as_deref(), Some("Frigate"));
    assert_eq!(
        sde.localized_category_name("en", 6).as_deref(),
        Some("Ship")
    );
    assert_eq!(
        sde.localized_market_group_name("en", 9).as_deref(),
        Some("Standard Frigates")
    );
    assert_eq!(
        sde.localized_market_group_description("en", 9).as_deref(),
        Some("Small Hulls")
    );
    assert_eq!(
        sde.localized_market_group_name("en", 10).as_deref(),
        Some("No Desc Group")
    );
    assert_eq!(sde.localized_market_group_description("en", 10), None);
    assert_eq!(sde.localized_market_group_name("en", 11).as_deref(), Some("Zero Desc Group"));
    assert_eq!(sde.localized_market_group_description("en", 11), None);

    let _ = fs::remove_dir_all(&dir);
}

fn make_temp_sde_with_files(entries: &[(&str, &str, Vec<u8>)]) -> (PathBuf, SdeRegistry) {
    let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "evomu_sde_loader_edges_{}_{}",
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp sde edge dir");
    let db_path = dir.join("evomu_sde.sqlite");
    fs::write(&db_path, b"").expect("create marker db");

    let mut resources = HashMap::new();
    for (res_path, fname, data) in entries {
        let fpath = dir.join(fname);
        fs::write(&fpath, data).expect("write fixture");
        resources.insert(
            (*res_path).to_string(),
            SdeResourceInfo {
                res_path: (*res_path).to_string(),
                cdn_relative_path: fpath.display().to_string(),
                md5: String::new(),
                byte_size: data.len() as u64,
                format: "bin".to_string(),
                record_count: 0,
                spot: String::new(),
            },
        );
    }
    let sde = SdeRegistry {
        db_path: Some(db_path),
        resources,
        ..SdeRegistry::default()
    };
    (dir, sde)
}

fn build_fsd_single_dict_body(entries: &[(u32, &[u8])]) -> Vec<u8> {
    let mut inner = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for (_, payload) in entries {
        offsets.push(inner.len() as u32);
        inner.extend_from_slice(payload);
    }
    let cnt = entries.len() as u32;
    let footer_size = 4u32 + cnt * 8;
    let body_size = inner.len() as u32 + footer_size + 4;
    let mut out = Vec::with_capacity(8 + body_size as usize);
    out.extend_from_slice(&0u32.to_le_bytes()); // sch_len = 0
    out.extend_from_slice(&body_size.to_le_bytes());
    out.extend_from_slice(&inner);
    out.extend_from_slice(&cnt.to_le_bytes());
    for ((key, _), off) in entries.iter().zip(offsets.iter()) {
        out.extend_from_slice(&key.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(&footer_size.to_le_bytes());
    out
}

#[test]
fn fsd_and_cfsd_overflow_truncation_and_domain_loader_error_branches() {
    // 1. fsd_dict_entries: footer_size > footer_len_pos (L262), item_size overflow (L271), val_off > body_end (L281)
    let mut buf_bad_footer = vec![0u8; 16];
    buf_bad_footer[0..4].copy_from_slice(&12u32.to_le_bytes());
    buf_bad_footer[12..16].copy_from_slice(&99u32.to_le_bytes());
    assert!(fsd_dict_entries(&buf_bad_footer, 0, 8).is_empty());

    let mut buf_mul_overflow = vec![0u8; 20];
    buf_mul_overflow[0..4].copy_from_slice(&16u32.to_le_bytes()); // body_start=4, body_end=20
    buf_mul_overflow[4..8].copy_from_slice(&2u32.to_le_bytes()); // footer=4, cnt=2
    buf_mul_overflow[8..12].copy_from_slice(&55u32.to_le_bytes()); // key 0 = 55
    buf_mul_overflow[12..16].copy_from_slice(&0u32.to_le_bytes()); // val_off = 4
    buf_mul_overflow[16..20].copy_from_slice(&12u32.to_le_bytes()); // footer_size = 12
    assert_eq!(fsd_dict_entries(&buf_mul_overflow, 0, usize::MAX), vec![(55, 4)]);

    let mut buf_val_oob = vec![0u8; 20];
    buf_val_oob[0..4].copy_from_slice(&16u32.to_le_bytes());
    buf_val_oob[4..8].copy_from_slice(&1u32.to_le_bytes());
    buf_val_oob[8..12].copy_from_slice(&77u32.to_le_bytes());
    buf_val_oob[12..16].copy_from_slice(&999u32.to_le_bytes()); // 4 + 999 > 20
    buf_val_oob[16..20].copy_from_slice(&12u32.to_le_bytes());
    assert!(fsd_dict_entries(&buf_val_oob, 0, 8).is_empty());

    // 2. cfsd_domain_loaders: cfsd_str checked_add overflow (L155), len OOB (L164), bucket boff overflow (L191)
    let mut cfsd_buf = vec![0u8; 160];
    cfsd_buf[48..56].copy_from_slice(&2u64.to_le_bytes()); // 2 buckets
    cfsd_buf[56..64].copy_from_slice(&(u64::MAX - 5).to_le_bytes()); // bucket 0 overflows 32 + boff (L191)
    cfsd_buf[64..72].copy_from_slice(&48u64.to_le_bytes()); // bucket 1: abs=80, cnt_pos=72
    cfsd_buf[72..80].copy_from_slice(&2u64.to_le_bytes()); // 2 dogmaunit records (stride 32 at 80..144)
    // record 0 (80..112): unit 1, name_rel = u64::MAX - 5 -> L155
    cfsd_buf[80..88].copy_from_slice(&1u64.to_le_bytes());
    cfsd_buf[88..96].copy_from_slice(&(u64::MAX - 5).to_le_bytes());
    // record 1 (112..144): unit 2, name_rel = 120 -> abs=152, len at 144..152 = u64::MAX - 5 -> L164
    cfsd_buf[112..120].copy_from_slice(&2u64.to_le_bytes());
    cfsd_buf[120..128].copy_from_slice(&120u64.to_le_bytes());
    cfsd_buf[144..152].copy_from_slice(&(u64::MAX - 5).to_le_bytes());

    let (dir_du, sde_du) =
        make_temp_sde_with_files(&[("res:/staticdata/dogmaunits.fsdbinary", "du.bin", cfsd_buf)]);
    let units = sde_du.load_dogma_units().expect("load_dogma_units overflow");
    assert_eq!(units[&1].name, "");
    assert_eq!(units[&2].name, "");
    let _ = fs::remove_dir_all(&dir_du);

    // 3. load_brackets: fsd_str past EOF returns "" cleanly (static_domain_loaders.rs L232)
    let mut bracket_rec = vec![0u8; 20];
    // mask = 0 -> present = ["name", "texturePath"], data_start = 16; first string at apos=16 has len=999
    bracket_rec[16..20].copy_from_slice(&999u32.to_le_bytes());
    let br_blob = build_fsd_single_dict_body(&[(1, &bracket_rec)]);
    let (dir_br, sde_br) =
        make_temp_sde_with_files(&[("res:/staticdata/brackets.static", "brackets.static", br_blob)]);
    let brackets = sde_br.load_brackets().expect("load_brackets truncated string");
    assert_eq!(brackets[&1].name, "");
    let _ = fs::remove_dir_all(&dir_br);

    // 4. load_certificates_and_masteries: has_recommended == false branch (static_domain_loaders.rs L734)
    let empty_fsd_dict = build_fsd_single_dict_body(&[]);
    let empty_dict_body = &empty_fsd_dict[4..]; // starts directly with length prefix (skip 4-byte sch_len)
    let mut cert_rec = vec![0u8; 24];
    cert_rec[0..4].copy_from_slice(&10u32.to_le_bytes());
    cert_rec[4..8].copy_from_slice(&20u32.to_le_bytes());
    cert_rec[8..12].copy_from_slice(&30u32.to_le_bytes());
    // mask at +12..+20 = 0 (no recommended_for), st_rel at +20..+24 = 0
    cert_rec.extend_from_slice(empty_dict_body);
    let certs_dict_wrapped = build_fsd_single_dict_body(&[(1, &cert_rec)]);
    let certs_dict_body = &certs_dict_wrapped[4..];
    let masteries_body = empty_dict_body;
    let mut certs_static = Vec::new();
    certs_static.extend_from_slice(&0u32.to_le_bytes()); // sch_len = 0
    certs_static.extend_from_slice(&0u64.to_le_bytes()); // body + 0..+8 unused header
    certs_static.extend_from_slice(&0u32.to_le_bytes()); // c_off = 0
    certs_static.extend_from_slice(&(certs_dict_body.len() as u32).to_le_bytes()); // m_off
    certs_static.extend_from_slice(certs_dict_body);
    certs_static.extend_from_slice(masteries_body);
    let (dir_cert, sde_cert) = make_temp_sde_with_files(&[(
        "res:/staticdata/certificates.static",
        "certs.static",
        certs_static,
    )]);
    let certs_bundle = sde_cert
        .load_certificates_and_masteries()
        .expect("load_certificates_and_masteries mask=0");
    assert!(certs_bundle.certificates[&1].recommended_for.is_empty());
    let _ = fs::remove_dir_all(&dir_cert);

    // 5. load_achievements: L782 (< 14 bytes), L792 (< var_base), L798 (absent opt), L841 (cond < 20 bytes)
    let mut inner_cond_dict = build_fsd_single_dict_body(&[(7, &[])]);
    let ic_body_rel = (inner_cond_dict.len() - 8) as u32; // body_end - body_start == 16 -> cond_pos sits right at start of outer 16B footer (<20B left!)
    inner_cond_dict[16..20].copy_from_slice(&ic_body_rel.to_le_bytes());
    let mut ach3 = Vec::new();
    ach3.extend_from_slice(&3u32.to_le_bytes());
    ach3.push(1);
    ach3.push(1);
    ach3.extend_from_slice(&0x08u32.to_le_bytes()); // bitfield: only simpleConditions (0x08), no name/desc/notif (L798)
    ach3.extend_from_slice(&0u32.to_le_bytes());
    ach3.extend_from_slice(&0u32.to_le_bytes()); // rel_off = 0 at offsets_start
    ach3.extend_from_slice(&inner_cond_dict[4..]); // var_base = sc_pos
    let ach3_blob = build_fsd_single_dict_body(&[(3, &ach3)]);

    // pos+14 <= raw.len() but raw.len() < var_base (L792)
    let mut ach2_data = vec![0u8; 10];
    ach2_data[6..10].copy_from_slice(&0x1Fu32.to_le_bytes()); // n_opt = 5 -> var_base = pos + 34 > raw.len()
    let ach2_blob = build_fsd_single_dict_body(&[(2, &ach2_data)]);

    // For L782 (< 14 bytes remaining): N=1 entry with val_off pointing 8 bytes before EOF
    let mut ach1_blob = build_fsd_single_dict_body(&[(1, &[])]);
    let ach1_end = ach1_blob.len() as u32;
    ach1_blob[16..20].copy_from_slice(&(ach1_end - 16).to_le_bytes());

    // bitfield = 0 (no 0x08 simpleConditions) to hit None branch on static_domain_loaders.rs L841
    let mut ach0_data = vec![0u8; 14];
    ach0_data[0..4].copy_from_slice(&4u32.to_le_bytes());
    ach0_data[4] = 1;
    ach0_data[5] = 1;
    let ach0_blob = build_fsd_single_dict_body(&[(4, &ach0_data)]);

    for blob in [ach3_blob, ach2_blob, ach1_blob, ach0_blob] {
        let (dir_a, sde_a) = make_temp_sde_with_files(&[
            ("res:/staticdata/achievements.static", "ach.static", blob),
            (
                "res:/localizationfsd/localization_fsd_en-us.pickle",
                "en.pickle",
                b"(dp0\n.".to_vec(),
            ),
        ]);
        let res = sde_a.load_achievements().expect("load_achievements edge");
        if let Some(a) = res.get(&3) {
            assert!(a.conditions.is_empty());
        }
        let _ = fs::remove_dir_all(&dir_a);
    }

    // 6. load_raw_fighter_abilities_static: corrupt/non-sqlite file returns Err (static_domain_loaders.rs L1164)
    let (dir_fa, sde_fa) = make_temp_sde_with_files(&[(
        "res:/staticdata/fighterabilities.static",
        "bad_fa.static",
        b"not a sqlite file".to_vec(),
    )]);
    assert!(sde_fa.load_raw_fighter_abilities_static().is_err());
    let _ = fs::remove_dir_all(&dir_fa);

    // 7. load_secondary_suns: L1226 (< fixed_end + 8) and L1238 (spos + 36 > raw.len())
    let make_sys_fsd = |data: &[u8], rel_off: u32| -> Vec<u8> {
        let footer_size = 16u32; // 4 + 1 * 12
        let body_size = data.len() as u32 + footer_size + 4;
        let mut v = Vec::new();
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&body_size.to_le_bytes());
        v.extend_from_slice(data);
        v.extend_from_slice(&1u32.to_le_bytes()); // cnt = 1
        v.extend_from_slice(&30000142u32.to_le_bytes()); // sys_id
        v.extend_from_slice(&rel_off.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // 4B padding for item_size=12
        v.extend_from_slice(&footer_size.to_le_bytes());
        v
    };
    let blob_sun_short = make_sys_fsd(&[0u8; 8], 0); // only 8 + 20 = 28 bytes < 106 -> L1226
    let mut sun_spos_oob_data = vec![0u8; 98 + 8 + 12]; // fixed_end(98)+mask(8)+3 offsets(12) = 118
    sun_spos_oob_data[98..106].copy_from_slice(&4u64.to_le_bytes()); // mask = 4 (bit 2 -> n_present=3, sec_idx=1)
    sun_spos_oob_data[98 + 8 + 4..98 + 8 + 8].copy_from_slice(&500u32.to_le_bytes()); // spos out of bounds -> L1238
    let blob_sun_spos_oob = make_sys_fsd(&sun_spos_oob_data, 0);
    for sblob in [blob_sun_short, blob_sun_spos_oob] {
        let (dir_s, sde_s) = make_temp_sde_with_files(&[(
            "res:/staticdata/solarsystemcontent.static",
            "sc.static",
            sblob,
        )]);
        assert!(sde_s.load_secondary_suns().expect("load_secondary_suns edge").is_empty());
        let _ = fs::remove_dir_all(&dir_s);
    }

    // 8. static_localization_loaders: L72 (bad escape), L118 (non-UTF8 id), main pickle ops & malformed types
    //    (L158, L163, L175, L185, L210, L239, L245, L259, L272)
    let bad_lang_pickle = b"(dp0\nI\xff\n(Vignored\np1\nsI7\n(Va\\nb\\u0041\\U00000042\np2\ns.";
    let main_p1 = b"a\ns\n(dpbad\n(lpbad\npbad\n(l\nI1\nI2\ns\n(dp65\nI3\na\n(lp65\np65\n(d\nS'languages'\nI1\ns\nS'labels'\nI2\ns\n.";
    let main_p2 = b"(d\nS'languages'\n(l\nI99\na\nS'en'\na\ns\nS'labels'\n(d\nS'badkey'\n(d\ns\nI42\n(d\nS'extra'\nS'x'\ns\nS'label'\nS'lbl'\ns\ns\ns\n.";

    let (dir_l1, sde_l1) = make_temp_sde_with_files(&[
        (
            "res:/localizationfsd/localization_fsd_en-us.pickle",
            "lang.pickle",
            bad_lang_pickle.to_vec(),
        ),
        (
            "res:/localizationfsd/localization_fsd_main.pickle",
            "main.pickle",
            main_p1.to_vec(),
        ),
    ]);
    let lang_map = sde_l1.load_all_localized_messages("en").expect("lang edge");
    assert_eq!(lang_map.get(&7).map(String::as_str), Some("a\\nbAB"));
    assert!(!lang_map.contains_key(&0));
    let m1 = sde_l1.load_localization_main().expect("main p1 edge");
    assert!(m1.languages.is_empty());
    assert!(m1.labels.is_empty());
    let _ = fs::remove_dir_all(&dir_l1);

    let (dir_l2, sde_l2) = make_temp_sde_with_files(&[(
        "res:/localizationfsd/localization_fsd_main.pickle",
        "main2.pickle",
        main_p2.to_vec(),
    )]);
    let m2 = sde_l2.load_localization_main().expect("main p2 edge");
    assert_eq!(m2.languages, vec!["en".to_string()]);
    assert_eq!(m2.labels[&42].label, "lbl");
    let _ = fs::remove_dir_all(&dir_l2);
}
