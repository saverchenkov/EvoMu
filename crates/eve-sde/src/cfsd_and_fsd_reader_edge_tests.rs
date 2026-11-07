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
    assert_eq!(
        sde.localized_market_group_name("en", 11).as_deref(),
        Some("Zero Desc Group")
    );
    assert_eq!(sde.localized_market_group_description("en", 11), None);

    let _ = fs::remove_dir_all(&dir);
}
