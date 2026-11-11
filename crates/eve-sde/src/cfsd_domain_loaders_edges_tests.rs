//! Edge-case, malformed buffer, and error-path tests for `cfsd_domain_loaders`.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DOM_ID: AtomicU64 = AtomicU64::new(1);

fn temp_dom_path(tag: &str) -> PathBuf {
    let seq = NEXT_DOM_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "evomu_dom_edge_{tag}_{}_{}.fsdbinary",
        std::process::id(),
        seq
    ))
}

fn reg_bytes(sde: &mut SdeRegistry, res_path: &str, tag: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_dom_path(tag);
    std::fs::write(&path, bytes).expect("write temp cfsd");
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: path.display().to_string(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    path
}

#[test]
fn low_level_helpers_handle_oob_overflow_invalid_utf8_and_truncated_buckets() {
    let short = [1u8, 2, 3];
    assert_eq!(u32_le(&short, 0), 0);
    assert_eq!(u32_le(&short, usize::MAX - 1), 0);
    assert_eq!(u64_le(&short, 0), 0);
    assert_eq!(u64_le(&short, usize::MAX - 1), 0);
    assert_eq!(f32_le(&short, 0), 0.0);
    assert_eq!(f32_le(&short, usize::MAX - 1), 0.0);

    // cfsd_str edge cases: rel == 0, overflow rel == u64::MAX, len exceeding buf, invalid UTF-8
    assert_eq!(cfsd_str(&short, 0), "");
    assert_eq!(cfsd_str(&short, u64::MAX), "");
    assert_eq!(cfsd_str(&short, 100), "");

    let mut bad_utf8 = vec![0u8; 40];
    bad_utf8.extend_from_slice(&2u64.to_le_bytes()); // len = 2 at offset 40
    let s_abs = bad_utf8.len(); // 48 -> rel = 16
    bad_utf8.extend_from_slice(&[0xFF, 0xFE]);
    assert_eq!(cfsd_str(&bad_utf8, (s_abs - 32) as u64), "");

    // Length claims 999 bytes past EOF
    let mut oob_len = vec![0u8; 40];
    oob_len.extend_from_slice(&999u64.to_le_bytes());
    let oob_abs = oob_len.len();
    oob_len.extend_from_slice(b"abc");
    assert_eq!(cfsd_str(&oob_len, (oob_abs - 32) as u64), "");

    // cfsd_records edge cases: stride == 0, too-short header, overflow boff, n == 0 bucket, truncated bucket
    assert!(cfsd_records(&[0u8; 100], 0).is_empty());
    assert!(cfsd_records(&[0u8; 32], 16).is_empty());

    // Bucket 0: boff = u64::MAX (overflow on 32 + boff)
    // Bucket 1: points to count n = 0
    // Bucket 2: points to count n = 10, but only 1 record fits before EOF
    let mut raw = vec![0u8; 80];
    raw[48..56].copy_from_slice(&3u64.to_le_bytes()); // 3 buckets
    raw[56..64].copy_from_slice(&u64::MAX.to_le_bytes()); // bucket 0 overflow
    // bucket 1: count 0 at 80
    raw.extend_from_slice(&0u64.to_le_bytes());
    let b1_abs = raw.len();
    raw[64..72].copy_from_slice(&((b1_abs - 32) as u64).to_le_bytes());
    // bucket 2: count 10, only 16 bytes follow (stride 16 -> 1 record fits, then breaks)
    raw.extend_from_slice(&10u64.to_le_bytes());
    let b2_abs = raw.len();
    raw[72..80].copy_from_slice(&((b2_abs - 32) as u64).to_le_bytes());
    raw.extend_from_slice(&[0u8; 20]); // 20 bytes: fits 1 record of stride 16, second overflows
    let recs = cfsd_records(&raw, 16);
    assert_eq!(recs, vec![b2_abs]);
}

#[test]
fn all_13_domain_loaders_return_err_when_unregistered_or_missing_on_disk() {
    let mut sde = SdeRegistry::default();
    let check_all_err = |r: &SdeRegistry| {
        assert!(r.load_agents_in_space().is_err());
        assert!(r.load_npc_characters().is_err());
        assert!(r.load_corporation_activities().is_err());
        assert!(r.load_npc_corporation_divisions().is_err());
        assert!(r.load_contraband_types().is_err());
        assert!(r.load_dogma_units().is_err());
        assert!(r.load_accounting_entry_types().is_err());
        assert!(r.load_accounting_keys().is_err());
        assert!(r.load_meta_groups().is_err());
        assert!(r.load_graphic_ids().is_err());
        assert!(r.load_icon_ids().is_err());
        assert!(r.load_sound_ids().is_err());
        assert!(r.load_raw_types_fsdbinary().is_err());
    };
    check_all_err(&sde);

    // Register missing file path -> MissingDatabase for read_cfsd_bytes
    let missing = temp_dom_path("nonexistent");
    let _ = std::fs::remove_file(&missing);
    for res in [
        "res:/staticdata/agentsinspace.fsdbinary",
        "res:/staticdata/npccharacters.fsdbinary",
        "res:/staticdata/corporationactivities.fsdbinary",
        "res:/staticdata/npccorporationdivisions.fsdbinary",
        "res:/staticdata/contrabandtypes.fsdbinary",
        "res:/staticdata/dogmaunits.fsdbinary",
        "res:/staticdata/accountingentrytypes.fsdbinary",
        "res:/staticdata/accountingkeys.fsdbinary",
        "res:/staticdata/metagroups.fsdbinary",
        "res:/staticdata/graphicids.fsdbinary",
        "res:/staticdata/iconids.fsdbinary",
        "res:/staticdata/soundids.fsdbinary",
        "res:/staticdata/types.fsdbinary",
    ] {
        sde.resources.insert(
            res.to_string(),
            SdeResourceInfo {
                res_path: res.to_string(),
                cdn_relative_path: missing.display().to_string(),
                md5: String::new(),
                byte_size: 0,
                format: "cfsd".to_string(),
                record_count: 0,
                spot: String::new(),
            },
        );
    }
    check_all_err(&sde);
}

#[test]
fn domain_loaders_handle_empty_contraband_optional_nones_and_negative_clamps() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // contrabandtypes with empty inner dict (buckets = 0)
    let mut cb_buf = vec![0u8; 72];
    cb_buf[40..48].copy_from_slice(&1u64.to_le_bytes());
    cb_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    cb_buf.extend_from_slice(&1u64.to_le_bytes()); // outer bucket count = 1
    let cb_rec = cb_buf.len();
    cb_buf[56..64].copy_from_slice(&((cb_rec - 32) as u64).to_le_bytes());
    cb_buf.extend_from_slice(&[0u8; 24]); // 1 contraband record (stride 24)
    cb_buf.extend_from_slice(&0u64.to_le_bytes()); // inner dict buckets = 0
    let inner_abs = cb_buf.len();
    cb_buf[cb_rec..cb_rec + 8].copy_from_slice(&9999u64.to_le_bytes());
    cb_buf[cb_rec + 8..cb_rec + 16].copy_from_slice(&((inner_abs - 32) as u64).to_le_bytes());
    cb_buf[cb_rec + 16..cb_rec + 24].copy_from_slice(&0u64.to_le_bytes());
    files.push(reg_bytes(
        &mut sde,
        "res:/staticdata/contrabandtypes.fsdbinary",
        "cb_empty",
        &cb_buf,
    ));
    let cb = sde.load_contraband_types().expect("empty inner contraband");
    assert!(cb[&9999].is_empty());

    // graphicids & iconids with all zero offsets (None / false)
    let mut gr_buf = vec![0u8; 64];
    gr_buf[40..48].copy_from_slice(&1u64.to_le_bytes());
    gr_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    gr_buf.extend_from_slice(&1u64.to_le_bytes());
    let gr_rec = gr_buf.len();
    gr_buf[56..64].copy_from_slice(&((gr_rec - 32) as u64).to_le_bytes());
    gr_buf.extend_from_slice(&[0u8; 160]);
    gr_buf[gr_rec..gr_rec + 8].copy_from_slice(&77u64.to_le_bytes());
    files.push(reg_bytes(
        &mut sde,
        "res:/staticdata/graphicids.fsdbinary",
        "gr_none",
        &gr_buf,
    ));
    let gr = sde.load_graphic_ids().expect("graphicids");
    assert_eq!(gr[&77].graphic_file, None);
    assert_eq!(gr[&77].icon_folder, None);
    assert_eq!(gr[&77].sof_faction_name, None);
    assert_eq!(gr[&77].sof_hull_name, None);
    assert_eq!(gr[&77].sof_race_name, None);

    let mut ic_buf = vec![0u8; 64];
    ic_buf[40..48].copy_from_slice(&1u64.to_le_bytes());
    ic_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    ic_buf.extend_from_slice(&1u64.to_le_bytes());
    let ic_rec = ic_buf.len();
    ic_buf[56..64].copy_from_slice(&((ic_rec - 32) as u64).to_le_bytes());
    ic_buf.extend_from_slice(&[0u8; 32]);
    ic_buf[ic_rec..ic_rec + 8].copy_from_slice(&88u64.to_le_bytes());
    files.push(reg_bytes(
        &mut sde,
        "res:/staticdata/iconids.fsdbinary",
        "ic_false",
        &ic_buf,
    ));
    let ic = sde.load_icon_ids().expect("iconids");
    assert!(!ic[&88].obsolete);
    assert_eq!(ic[&88].icon_file, "");
    assert_eq!(ic[&88].description, "");

    // types.fsdbinary with negative portion_size clamped to 1 and negative meta_level
    let mut ty_buf = vec![0u8; 64];
    ty_buf[40..48].copy_from_slice(&1u64.to_le_bytes());
    ty_buf[48..56].copy_from_slice(&1u64.to_le_bytes());
    ty_buf.extend_from_slice(&1u64.to_le_bytes());
    let ty_rec = ty_buf.len();
    ty_buf[56..64].copy_from_slice(&((ty_rec - 32) as u64).to_le_bytes());
    ty_buf.extend_from_slice(&[0u8; 152]);
    ty_buf[ty_rec..ty_rec + 8].copy_from_slice(&1234u64.to_le_bytes());
    ty_buf[ty_rec + 40..ty_rec + 48].copy_from_slice(&(-5i64).to_le_bytes());
    ty_buf[ty_rec + 84..ty_rec + 88].copy_from_slice(&10i32.to_le_bytes());
    ty_buf[ty_rec + 104..ty_rec + 108].copy_from_slice(&(-2i32).to_le_bytes());
    ty_buf[ty_rec + 148..ty_rec + 152].copy_from_slice(&(1u32 << 13).to_le_bytes());
    files.push(reg_bytes(
        &mut sde,
        "res:/staticdata/types.fsdbinary",
        "ty_neg",
        &ty_buf,
    ));
    let (_, raw_types) = sde.load_raw_types_fsdbinary().expect("raw_types");
    assert_eq!(raw_types[&1234].portion_size, 1);
    assert_eq!(raw_types[&1234].meta_level, Some(-2));

    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
