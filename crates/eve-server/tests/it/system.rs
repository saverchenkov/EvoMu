//! System-level transactional integrity and documentation reconciliation tests.

#![forbid(unsafe_code)]

#[tokio::test]
async fn test_phase_0_s8_transactional_snapshot_mutation_and_no_early_return_err() {
    // parity: plan:S-8
    use std::path::Path;

    fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs_files(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
    }

    let workspace_crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut rs_files = Vec::new();
    collect_rs_files(workspace_crates, &mut rs_files);
    assert!(!rs_files.is_empty());

    for file in rs_files {
        let src = std::fs::read_to_string(&file).expect("read .rs file");
        let mut search_from = 0;
        while let Some(rel_idx) = src[search_from..].find(".with_mut_snapshot(") {
            let call_start = search_from + rel_idx;
            let open_paren = call_start + ".with_mut_snapshot".len();
            let bytes = src.as_bytes();
            let mut depth = 0i32;
            let mut close_paren = None;
            for (i, &b) in bytes[open_paren..].iter().enumerate() {
                if b == b'(' {
                    depth += 1;
                } else if b == b')' {
                    depth -= 1;
                    if depth == 0 {
                        close_paren = Some(open_paren + i);
                        break;
                    }
                }
            }
            let end = close_paren.unwrap_or(src.len());
            let closure_body = &src[open_paren..=end];
            assert!(
                !closure_body.contains("return Err("),
                "S-8 violation in {}: .with_mut_snapshot(...) closure contains `return Err(...)`; use .try_with_mut_snapshot(...) instead",
                file.display()
            );
            search_from = end;
        }
    }

    // Verify try_with_mut_snapshot rolls back mutations when Err is returned mid-closure
    let repo = eve_db::WorldRepository::with_demo_seed();
    let initial_qty = repo.get_item(100_000_011).unwrap().quantity;
    let res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.items.get_mut(&100_000_011).unwrap().quantity = 0;
        Err("simulated failure after partial mutation".to_string())
    });
    assert!(res.is_err());
    assert_eq!(
        repo.get_item(100_000_011).unwrap().quantity,
        initial_qty,
        "S-8: try_with_mut_snapshot must roll back partial mutations when closure returns Err"
    );
}

#[tokio::test]
async fn test_phase_t_t1_machonet_get_time_rpc_parity() {
    // parity: rpc:machoNet.GetTime, plan:T-1
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    eve_testkit::attest(&["rpc:machoNet.GetTime", "plan:T-1"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(101);

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after UNIX_EPOCH")
        .as_secs() as i64;
    let expected_ticks = (now_secs + 11_644_473_600) * 10_000_000;

    // Initial time query before advancing clock
    let res = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .expect("machoNet.GetTime must succeed");

    let PyRep::Integer(t1) = res else {
        panic!("machoNet.GetTime must return PyRep::Integer, got {res:?}");
    };

    // Assert t1 is within 5 seconds (50,000,000 100ns FILETIME ticks) of live SystemTime::now()
    let drift_ticks = (t1 - expected_ticks).abs();
    assert!(
        drift_ticks <= 50_000_000,
        "GetTime ticks must be within 5s (50,000,000 ticks) of SystemTime::now() FILETIME ({expected_ticks}), got {t1} (drift={drift_ticks})"
    );

    // Advance virtual clock by 60 seconds (60 * 10,000,000 = 600,000,000 ticks)
    ctx.advance_virtual_clock(60);
    let res2 = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .expect("machoNet.GetTime after clock advance");

    let PyRep::Integer(t2) = res2 else {
        panic!("machoNet.GetTime must return PyRep::Integer, got {res2:?}");
    };

    assert!(
        t2 >= t1 + 600_000_000 && t2 < t1 + 650_000_000,
        "GetTime ticks must advance by ~60s in 100ns ticks (t1={t1}, t2={t2})"
    );
}

#[test]
fn test_phase_t_t3_fuzz_targets_and_corpus_replay() {
    use eve_machonet::{extract_macho_call_req, MachoAddress, MachoPacket, VersionExchange};
    use eve_proto::{marshal, validate_frame_length, BridgeEnvelope, GatewayFrame};
    use std::path::Path;
    use std::process::Command;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let fuzz_dir = root.join("fuzz");
    assert!(fuzz_dir.join("Cargo.toml").exists(), "fuzz/Cargo.toml must exist");

    let fuzz_list = Command::new("cargo")
        .args(["fuzz", "list"])
        .current_dir(root)
        .output()
        .expect("cargo fuzz list must execute");
    assert!(
        fuzz_list.status.success(),
        "cargo fuzz list failed: {}",
        String::from_utf8_lossy(&fuzz_list.stderr)
    );
    let listed_targets = String::from_utf8_lossy(&fuzz_list.stdout);
    for target in [
        "fuzz_marshal_decode",
        "fuzz_bridge_envelope",
        "fuzz_macho_packet",
    ] {
        assert!(
            listed_targets.lines().any(|line| line.trim() == target),
            "cargo fuzz list missing target {target}: {listed_targets}"
        );
        let target_rs = fuzz_dir
            .join("fuzz_targets")
            .join(format!("{target}.rs"));
        assert!(target_rs.exists(), "Missing fuzz target {}", target_rs.display());
    }

    // Read-only replay of the committed minimized corpus across all 3 fuzz targets (never mutates fuzz/corpus/*)
    let corpus_marshal = fuzz_dir.join("corpus").join("fuzz_marshal_decode");
    let corpus_bridge = fuzz_dir.join("corpus").join("fuzz_bridge_envelope");
    let corpus_macho = fuzz_dir.join("corpus").join("fuzz_macho_packet");

    let mut marshal_count = 0usize;
    for entry in std::fs::read_dir(&corpus_marshal)
        .expect("fuzz/corpus/fuzz_marshal_decode must exist")
        .flatten()
    {
        if entry.path().is_file() {
            marshal_count += 1;
            let bytes = std::fs::read(entry.path()).unwrap();
            if let Ok(rep) = marshal::decode(&bytes) {
                let _ = GatewayFrame::from_pyrep(&rep);
                let reencoded = marshal::encode(&rep);
                let _ = marshal::decode(&reencoded);
            }
            let _ = GatewayFrame::decode_protobuf_wire(&bytes);
            let _ = eve_destiny::wire::decode_add_balls(&bytes);
        }
    }
    assert!(
        marshal_count >= 30,
        "Expected >= 30 minimized seeds in fuzz_marshal_decode corpus, found {marshal_count}"
    );

    let mut bridge_count = 0usize;
    for entry in std::fs::read_dir(&corpus_bridge)
        .expect("fuzz/corpus/fuzz_bridge_envelope must exist")
        .flatten()
    {
        if entry.path().is_file() {
            bridge_count += 1;
            let data = std::fs::read(entry.path()).unwrap();
            if data.len() >= 4 {
                let len_buf = [data[0], data[1], data[2], data[3]];
                if let Ok(frame_len) = validate_frame_length(len_buf) {
                    if data.len() >= 4 + frame_len {
                        let _ = BridgeEnvelope::from_wire_bytes(&data[4..4 + frame_len]);
                    }
                }
            }
            if let Ok(env) = BridgeEnvelope::from_wire_bytes(&data) {
                if let Ok(wire) = env.to_wire_bytes() {
                    let _ = BridgeEnvelope::from_wire_bytes(&wire);
                }
            }
        }
    }
    assert!(
        bridge_count >= 30,
        "Expected >= 30 minimized seeds in fuzz_bridge_envelope corpus, found {bridge_count}"
    );

    let mut macho_count = 0usize;
    for entry in std::fs::read_dir(&corpus_macho)
        .expect("fuzz/corpus/fuzz_macho_packet must exist")
        .flatten()
    {
        if entry.path().is_file() {
            macho_count += 1;
            let data = std::fs::read(entry.path()).unwrap();
            if let Ok(rep) = marshal::decode(&data) {
                let _ = VersionExchange::from_pyrep(&rep);
                let _ = MachoAddress::from_pyrep(&rep);
                let _ = extract_macho_call_req(&rep);
                if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
                    let _ = extract_macho_call_req(&pkt.payload);
                    let re = pkt.to_pyrep();
                    let _ = MachoPacket::from_pyrep(&re);
                }
            }
        }
    }
    assert!(
        macho_count >= 30,
        "Expected >= 30 minimized seeds in fuzz_macho_packet corpus, found {macho_count}"
    );
}

#[test]
fn test_phase_t_t5_coverage_and_mutation_tooling() {
    // parity: plan:T-5
    use std::path::Path;
    use std::process::Command;

    eve_testkit::attest(&["plan:T-5"]);

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let script = root.join("scripts/qa/verify_cov_and_mutants.py");
    assert!(
        script.exists(),
        "T-5 verification script must exist at {}",
        script.display()
    );

    let script_help = Command::new("/usr/bin/python3")
        .arg(&script)
        .arg("--help")
        .current_dir(root)
        .output()
        .expect("verify_cov_and_mutants.py --help must execute");
    assert!(
        script_help.status.success(),
        "verify_cov_and_mutants.py --help failed: {}",
        String::from_utf8_lossy(&script_help.stderr)
    );
    let help_stdout = String::from_utf8_lossy(&script_help.stdout);
    assert!(help_stdout.contains("--min-mutation-score"));
    assert!(help_stdout.contains("--in-diff"));

    let cov_ver = Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .output()
        .expect("cargo llvm-cov must be installed");
    assert!(
        cov_ver.status.success(),
        "cargo llvm-cov --version failed: {}",
        String::from_utf8_lossy(&cov_ver.stderr)
    );

    let mut_list = Command::new("cargo")
        .args(["mutants", "--list", "-p", "eve-testkit", "--json"])
        .current_dir(root)
        .output()
        .expect("cargo mutants --list must execute");
    assert!(
        mut_list.status.success(),
        "cargo mutants --list failed: {}",
        String::from_utf8_lossy(&mut_list.stderr)
    );
    let mutants_json: serde_json::Value =
        serde_json::from_slice(&mut_list.stdout).expect("valid cargo mutants --list JSON");
    let mutant_count = mutants_json
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        mutant_count >= 20,
        "Expected >= 20 mutation points in eve-testkit, found {mutant_count}"
    );
}

#[tokio::test]
async fn test_sde_skins_static_and_cosmetics_validation() {
    // parity: sde:res:/staticdata/skins.static, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:shipCosmeticsMgr.GiveSkin
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9910);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(100_000_001);
    sess.role |= eve_machonet::service_roles::ROLE_GML;

    assert_eq!(ctx.sde.skins.len(), 7_048);
    assert_eq!(ctx.sde.skin_materials.len(), 875);
    assert_eq!(ctx.sde.skin_licenses.len(), 11_877);

    let skin5 = ctx.sde.skins.get(&5).expect("skin 5");
    assert_eq!(skin5.internal_name, "Megathron Quafe");
    assert_eq!(skin5.skin_material_id, 3);
    assert_eq!(skin5.types, vec![641]);
    assert!(skin5.allow_ccp_devs);
    assert!(!skin5.visible_serenity);
    assert!(skin5.visible_tranquility);
    assert!(!skin5.is_structure_skin);

    let skin170 = ctx.sde.skins.get(&170).expect("skin 170 (Rifter Krusual)");
    assert_eq!(skin170.internal_name, "Rifter Krusual");
    assert_eq!(skin170.skin_material_id, 22);
    assert_eq!(skin170.types, vec![587]);

    // 1. GiveSkin rejects non-existent skinID and mismatched ship type
    let bad_give = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "GiveSkin",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_give.unwrap_err().code, "InvalidSkinLicenseType");

    let mismatch_give = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "GiveSkin",
            &[PyRep::Integer(5), PyRep::Integer(587)],
            &kw,
            None,
        )
        .await;
    assert_eq!(mismatch_give.unwrap_err().code, "InvalidShipTypeForSkin");

    let good_give = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "GiveSkin",
            &[PyRep::Integer(5), PyRep::Integer(641)],
            &kw,
            None,
        )
        .await
        .expect("GiveSkin skin 5 for Megathron 641");
    let PyRep::KeyVal(give_kv) = good_give else {
        panic!("Expected GiveSkin KeyVal");
    };
    assert_eq!(give_kv.get("skinID"), Some(&PyRep::Integer(5)));
    assert_eq!(give_kv.get("skinMaterialID"), Some(&PyRep::Integer(3)));
    assert_eq!(
        give_kv.get("internalName"),
        Some(&PyRep::String("Megathron Quafe".into()))
    );

    // 2. ActivateSkinLicense validates license_type_id against ctx.sde.skin_licenses and ctx.sde.skins
    let bad_lic = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_lic.unwrap_err().code, "InvalidSkinLicenseType");

    let mismatch_lic = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(34748), PyRep::Integer(641)],
            &kw,
            None,
        )
        .await;
    assert_eq!(mismatch_lic.unwrap_err().code, "InvalidShipTypeForSkin");

    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        snap.items.insert(
            999_034_748,
            eve_db::InventoryItemRecord {
                item_id: 999_034_748,
                type_id: 34_748,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        Ok(())
    });
    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ActivateSkinLicense",
        &[PyRep::Integer(34748), PyRep::Integer(587)],
        &kw,
        None,
    )
    .await
    .expect("ActivateSkinLicense 34748 for Rifter 587");

    // 3. ApplySkinToShip validates skin_id and ship_type_id against ctx.sde.skins
    let bad_apply = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_apply.unwrap_err().code, "SkinNotLicensed");

    let mismatch_apply = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(5)],
            &kw,
            None,
        )
        .await;
    assert_eq!(mismatch_apply.unwrap_err().code, "InvalidShipTypeForSkin");

    let good_apply = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(170)],
            &kw,
            None,
        )
        .await
        .expect("ApplySkinToShip skin 170 on Rifter 587");
    let PyRep::KeyVal(apply_kv) = good_apply else {
        panic!("Expected ApplySkinToShip KeyVal");
    };
    assert_eq!(apply_kv.get("skinID"), Some(&PyRep::Integer(170)));
    assert_eq!(
        apply_kv.get("designName"),
        Some(&PyRep::String("Rifter Krusual".into()))
    );
}

