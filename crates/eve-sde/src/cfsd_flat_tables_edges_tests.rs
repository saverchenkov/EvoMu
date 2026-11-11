//! Edge-case and error-path tests for `cfsd_flat_tables`.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);

fn temp_cfsd_path(tag: &str) -> PathBuf {
    let seq = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "evomu_flat_edge_{tag}_{}_{}.fsdbinary",
        std::process::id(),
        seq
    ))
}

fn register_res(sde: &mut SdeRegistry, res_path: &str, path: &Path, byte_size: u64) {
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: path.display().to_string(),
            md5: String::new(),
            byte_size,
            format: "cfsd".to_string(),
            record_count: 1,
            spot: String::new(),
        },
    );
}

#[test]
fn load_cfsd_table_missing_resource_and_missing_file_errors() {
    let mut sde = SdeRegistry::default();
    let known = "res:/staticdata/agenttypes.fsdbinary";

    // Known layout, but resource not registered in sde.resources -> InvalidSchema
    let err_unreg = sde.load_cfsd_table(known).unwrap_err();
    assert!(matches!(err_unreg, SdeError::InvalidSchema(_)), "got {err_unreg:?}");

    // Resource registered, but file does not exist on disk -> MissingDatabase
    let missing_path = temp_cfsd_path("missing");
    let _ = std::fs::remove_file(&missing_path);
    register_res(&mut sde, known, &missing_path, 64);
    let err_nofile = sde.load_cfsd_table(known).unwrap_err();
    assert!(matches!(err_nofile, SdeError::MissingDatabase(_)));
}

#[test]
fn load_cfsd_table_malformed_header_buckets_and_truncations() {
    let mut sde = SdeRegistry::default();
    let agent_res = "res:/staticdata/agenttypes.fsdbinary";
    let school_res = "res:/staticdata/schoolmap.fsdbinary";
    let slots_res = "res:/staticdata/ship_cosmetic_slots.fsdbinary";
    let bill_res = "res:/staticdata/billtypes.fsdbinary";

    // 1. Truncated header (< 48 bytes) with matching byte_size -> header u64(40) fails
    let p1 = temp_cfsd_path("short_hdr");
    let raw1 = vec![0u8; 44];
    std::fs::write(&p1, &raw1).unwrap();
    register_res(&mut sde, agent_res, &p1, raw1.len() as u64);
    assert!(sde.load_cfsd_table(agent_res).is_err());
    let _ = std::fs::remove_file(&p1);

    // 2. Bucket header claims 1 bucket, but bucket table is truncated (< 64 bytes)
    let p2 = temp_cfsd_path("short_bucket");
    let mut raw2 = vec![0u8; 60];
    raw2[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw2[48..56].copy_from_slice(&1u64.to_le_bytes());
    std::fs::write(&p2, &raw2).unwrap();
    register_res(&mut sde, agent_res, &p2, raw2.len() as u64);
    assert!(sde.load_cfsd_table(agent_res).is_err());
    let _ = std::fs::remove_file(&p2);

    // 3. Duplicate key across two records causes out.len() != header count
    let p3 = temp_cfsd_path("dup_keys");
    let mut raw3 = vec![0u8; 64];
    raw3[40..48].copy_from_slice(&2u64.to_le_bytes());
    raw3[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw3.extend_from_slice(&2u64.to_le_bytes()); // bucket count = 2
    let recs_abs = raw3.len();
    raw3[56..64].copy_from_slice(&((recs_abs - 32) as u64).to_le_bytes());
    // schoolmap stride = 12, key_width = 4: write two records with identical key 7
    raw3.extend_from_slice(&vec![0u8; 24]);
    raw3[recs_abs..recs_abs + 4].copy_from_slice(&7u32.to_le_bytes());
    raw3[recs_abs + 12..recs_abs + 16].copy_from_slice(&7u32.to_le_bytes());
    std::fs::write(&p3, &raw3).unwrap();
    register_res(&mut sde, school_res, &p3, raw3.len() as u64);
    assert!(matches!(
        sde.load_cfsd_table(school_res).unwrap_err(),
        SdeError::InvalidSchema(_)
    ));
    let _ = std::fs::remove_file(&p3);

    // 4. Out-of-bounds Str offset in agenttypes.fsdbinary
    let p4 = temp_cfsd_path("bad_str_off");
    let mut raw4 = vec![0u8; 64];
    raw4[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw4[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw4.extend_from_slice(&1u64.to_le_bytes());
    let r4_abs = raw4.len();
    raw4[56..64].copy_from_slice(&((r4_abs - 32) as u64).to_le_bytes());
    raw4.extend_from_slice(&vec![0u8; 16]);
    raw4[r4_abs..r4_abs + 8].copy_from_slice(&1u64.to_le_bytes());
    raw4[r4_abs + 8..r4_abs + 16].copy_from_slice(&999_999u64.to_le_bytes());
    std::fs::write(&p4, &raw4).unwrap();
    register_res(&mut sde, agent_res, &p4, raw4.len() as u64);
    assert!(sde.load_cfsd_table(agent_res).is_err());
    let _ = std::fs::remove_file(&p4);

    // 5. Invalid UTF-8 bytes in Str field
    let p5 = temp_cfsd_path("bad_utf8");
    let mut raw5 = vec![0u8; 64];
    raw5[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw5[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw5.extend_from_slice(&1u64.to_le_bytes());
    let r5_abs = raw5.len();
    raw5[56..64].copy_from_slice(&((r5_abs - 32) as u64).to_le_bytes());
    raw5.extend_from_slice(&vec![0u8; 16]);
    raw5[r5_abs..r5_abs + 8].copy_from_slice(&1u64.to_le_bytes());
    raw5.extend_from_slice(&2u64.to_le_bytes());
    let s5_abs = raw5.len();
    raw5.extend_from_slice(&[0xFF, 0xFE]);
    raw5[r5_abs + 8..r5_abs + 16].copy_from_slice(&((s5_abs - 32) as u64).to_le_bytes());
    std::fs::write(&p5, &raw5).unwrap();
    register_res(&mut sde, agent_res, &p5, raw5.len() as u64);
    assert!(sde.load_cfsd_table(agent_res).is_err());
    let _ = std::fs::remove_file(&p5);

    // 6. U32List pointing to out-of-bounds offset and truncated U32List element data
    let p6 = temp_cfsd_path("bad_u32list_off");
    let mut raw6 = vec![0u8; 64];
    raw6[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw6[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw6.extend_from_slice(&1u64.to_le_bytes());
    let r6_abs = raw6.len();
    raw6[56..64].copy_from_slice(&((r6_abs - 32) as u64).to_le_bytes());
    raw6.extend_from_slice(&vec![0u8; 32]); // ship_cosmetic_slots stride = 32
    raw6[r6_abs..r6_abs + 8].copy_from_slice(&10u64.to_le_bytes());
    raw6[r6_abs + 8..r6_abs + 16].copy_from_slice(&500_000u64.to_le_bytes());
    std::fs::write(&p6, &raw6).unwrap();
    register_res(&mut sde, slots_res, &p6, raw6.len() as u64);
    assert!(sde.load_cfsd_table(slots_res).is_err());
    let _ = std::fs::remove_file(&p6);

    // 7. U32List with valid header count=5 but only 4 bytes of list payload
    let p7 = temp_cfsd_path("short_u32list");
    let mut raw7 = vec![0u8; 64];
    raw7[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw7[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw7.extend_from_slice(&1u64.to_le_bytes());
    let r7_abs = raw7.len();
    raw7[56..64].copy_from_slice(&((r7_abs - 32) as u64).to_le_bytes());
    raw7.extend_from_slice(&vec![0u8; 32]);
    raw7[r7_abs..r7_abs + 8].copy_from_slice(&10u64.to_le_bytes());
    raw7.extend_from_slice(&5u64.to_le_bytes());
    let l7_abs = raw7.len();
    raw7.extend_from_slice(&42u32.to_le_bytes());
    raw7[r7_abs + 8..r7_abs + 16].copy_from_slice(&((l7_abs - 32) as u64).to_le_bytes());
    std::fs::write(&p7, &raw7).unwrap();
    register_res(&mut sde, slots_res, &p7, raw7.len() as u64);
    assert!(sde.load_cfsd_table(slots_res).is_err());
    let _ = std::fs::remove_file(&p7);

    // 8. Optional-mask partial bit check (billtypes: mask = 0 omits description, mask = 1 includes empty rel=0 Str)
    let p8 = temp_cfsd_path("partial_mask");
    let mut raw8 = vec![0u8; 64];
    raw8[40..48].copy_from_slice(&1u64.to_le_bytes());
    raw8[48..56].copy_from_slice(&1u64.to_le_bytes());
    raw8.extend_from_slice(&1u64.to_le_bytes());
    let r8_abs = raw8.len();
    raw8[56..64].copy_from_slice(&((r8_abs - 32) as u64).to_le_bytes());
    raw8.extend_from_slice(&vec![0u8; 32]); // billtypes stride = 32
    raw8[r8_abs..r8_abs + 8].copy_from_slice(&1u64.to_le_bytes());
    raw8[r8_abs + 24..r8_abs + 28].copy_from_slice(&999u32.to_le_bytes());
    raw8[r8_abs + 28..r8_abs + 32].copy_from_slice(&1u32.to_le_bytes()); // description bit=1 set, rel=0
    std::fs::write(&p8, &raw8).unwrap();
    register_res(&mut sde, bill_res, &p8, raw8.len() as u64);
    let bills = sde.load_cfsd_table(bill_res).expect("billtypes");
    assert_eq!(bills[&1].str("billTypeName"), Some(""));
    assert_eq!(bills[&1].str("description"), Some(""));
    assert_eq!(bills[&1].int("nameID"), Some(999));
    let _ = std::fs::remove_file(&p8);
}
