//! Isolated integration test covering `EVOMU_RESFILES_DIR` and `EVOMU_SHARED_CACHE`
//! resolution in `SdeRegistry::cdn_resource_path`.

use eve_sde::{SdeRegistry, SdeResourceInfo};

#[test]
fn cdn_resource_path_respects_resfiles_and_shared_cache_env_vars() {
    let base = std::env::temp_dir().join(format!("evomu_resfiles_env_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let sub_res = base.join("ResFiles");
    std::fs::create_dir_all(&sub_res).unwrap();
    let direct_file = base.join("direct.fsdbinary");
    let nested_file = sub_res.join("nested.fsdbinary");
    std::fs::write(&direct_file, b"ok").unwrap();
    std::fs::write(&nested_file, b"ok").unwrap();

    let mut sde = SdeRegistry::default();
    sde.resources.insert(
        "res:/direct".to_string(),
        SdeResourceInfo {
            res_path: "res:/direct".to_string(),
            cdn_relative_path: "direct.fsdbinary".to_string(),
            md5: String::new(),
            byte_size: 2,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    sde.resources.insert(
        "res:/nested".to_string(),
        SdeResourceInfo {
            res_path: "res:/nested".to_string(),
            cdn_relative_path: "nested.fsdbinary".to_string(),
            md5: String::new(),
            byte_size: 2,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );

    // 1. EVOMU_RESFILES_DIR takes precedence
    std::env::set_var("EVOMU_RESFILES_DIR", &base);
    assert_eq!(sde.cdn_resource_path("res:/direct").unwrap(), direct_file);

    // 2. Empty EVOMU_RESFILES_DIR falls through to EVOMU_SHARED_CACHE (with ResFiles subdir)
    std::env::set_var("EVOMU_RESFILES_DIR", "   ");
    std::env::set_var("EVOMU_SHARED_CACHE", &base);
    assert_eq!(sde.cdn_resource_path("res:/nested").unwrap(), nested_file);

    // 3. EVOMU_SHARED_CACHE without ResFiles subdir falls back to the directory itself
    let no_res_dir = base.join("plain_cache");
    std::fs::create_dir_all(&no_res_dir).unwrap();
    let plain_file = no_res_dir.join("direct.fsdbinary");
    std::fs::write(&plain_file, b"ok").unwrap();
    std::env::set_var("EVOMU_SHARED_CACHE", &no_res_dir);
    assert_eq!(sde.cdn_resource_path("res:/direct").unwrap(), plain_file);

    // 4. Both empty + custom HOME/LOCALAPPDATA/USERPROFILE/PROGRAMDATA -> hits all env branches & non-dir fallback
    let orig_home = std::env::var_os("HOME");
    std::env::remove_var("EVOMU_RESFILES_DIR");
    std::env::set_var("EVOMU_SHARED_CACHE", "");
    std::env::set_var("HOME", base.join("no_such_home"));
    std::env::set_var("LOCALAPPDATA", base.join("no_such_local"));
    std::env::set_var("USERPROFILE", base.join("no_such_profile"));
    std::env::set_var("PROGRAMDATA", base.join("no_such_program"));
    assert!(sde.cdn_resource_path("res:/direct").is_err());

    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("USERPROFILE");
    std::env::remove_var("PROGRAMDATA");
    match orig_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    std::env::remove_var("EVOMU_SHARED_CACHE");

    // 5. Synthetic cfsd flat layout covering truncated error paths via EVOMU_RESFILES_DIR
    let mut flat_bytes = Vec::new();
    flat_bytes.extend_from_slice(&[0_u8; 32]); // header
    flat_bytes.extend_from_slice(&0_u64.to_le_bytes()); // offset 32
    flat_bytes.extend_from_slice(&1_u64.to_le_bytes()); // offset 40: count = 1
    flat_bytes.extend_from_slice(&1_u64.to_le_bytes()); // offset 48: buckets = 1
    flat_bytes.extend_from_slice(&(72_u64 - 32).to_le_bytes()); // offset 56: bucket 0 rel_off -> at=72
    flat_bytes.extend_from_slice(&1_u64.to_le_bytes()); // offset 64: n = 1
    // offset 72: 1 record (stride 32 for billtypes): all zeros (mask=0)
    flat_bytes.extend_from_slice(&77_u64.to_le_bytes());
    flat_bytes.extend_from_slice(&[0_u8; 24]);

    let flat_file = base.join("synth_flat.cfsd");
    sde.resources.insert(
        "res:/staticdata/billtypes.fsdbinary".to_string(),
        SdeResourceInfo {
            res_path: "res:/staticdata/billtypes.fsdbinary".to_string(),
            cdn_relative_path: "synth_flat.cfsd".to_string(),
            md5: String::new(),
            byte_size: flat_bytes.len() as u64,
            format: "cfsd".to_string(),
            record_count: 1,
            spot: String::new(),
        },
    );
    std::env::set_var("EVOMU_RESFILES_DIR", &base);
    std::fs::write(&flat_file, &flat_bytes).unwrap();
    let loaded_flat = sde
        .load_cfsd_table("res:/staticdata/billtypes.fsdbinary")
        .unwrap();
    assert!(loaded_flat.contains_key(&77));

    // Truncated variants + count mismatch to hit error branches cleanly
    for trunc in [20usize, 56, 72, 76, flat_bytes.len() - 1] {
        std::fs::write(&flat_file, &flat_bytes[..trunc]).unwrap();
        sde.resources
            .get_mut("res:/staticdata/billtypes.fsdbinary")
            .unwrap()
            .byte_size = trunc as u64;
        assert!(sde
            .load_cfsd_table("res:/staticdata/billtypes.fsdbinary")
            .is_err());
    }
    // Count mismatch (header says count = 2, bucket has 1 record)
    let mut bad_count = flat_bytes.clone();
    bad_count[40..48].copy_from_slice(&2_u64.to_le_bytes());
    std::fs::write(&flat_file, &bad_count).unwrap();
    sde.resources
        .get_mut("res:/staticdata/billtypes.fsdbinary")
        .unwrap()
        .byte_size = bad_count.len() as u64;
    assert!(sde
        .load_cfsd_table("res:/staticdata/billtypes.fsdbinary")
        .is_err());

    // Nested error branches with truncated synthetic buffer
    assert!(sde.load_new_activity_ids().is_err());
    sde.resources.insert(
        "res:/staticdata/newactivities.fsdbinary".to_string(),
        SdeResourceInfo {
            res_path: "res:/staticdata/newactivities.fsdbinary".to_string(),
            cdn_relative_path: "synth_flat.cfsd".to_string(),
            md5: String::new(),
            byte_size: 72,
            format: "cfsd".to_string(),
            record_count: 0,
            spot: String::new(),
        },
    );
    assert!(sde.load_new_activity_ids().is_err());
    std::env::remove_var("EVOMU_RESFILES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}
