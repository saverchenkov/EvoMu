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
async fn test_phase_0_s10_doc_reconciliation_and_labels() {
    // parity: plan:S-10
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let plan_md = std::fs::read_to_string(root.join("PLAN.md")).expect("read PLAN.md");
    let readme_md = std::fs::read_to_string(root.join("README.md")).expect("read README.md");
    let blockers_md =
        std::fs::read_to_string(root.join("docs/BLOCKERS.md")).expect("read docs/BLOCKERS.md");
    let status_md = std::fs::read_to_string(root.join("docs/ACTION_PLAN_STATUS.md"))
        .expect("read docs/ACTION_PLAN_STATUS.md");

    assert!(
        !plan_md.contains("Partial Vertical Slice")
            && !readme_md.contains("Partial Vertical Slice")
            && !plan_md.contains("**Completed")
            && !readme_md.contains("**Completed")
            && !plan_md.contains("Completed (`D-")
            && !readme_md.contains("Completed (`D-")
            && !plan_md.contains("Implemented & e2e-tested")
            && !readme_md.contains("Implemented & e2e-tested")
            && !plan_md.contains("evomu_sde_fixture.sqlite")
            && !readme_md.contains("evomu_sde_fixture.sqlite"),
        "S-10: PLAN.md and README.md must use Rule 8 status labels (In progress / Not started / Implemented (review: ...)) and must not reference evomu_sde_fixture.sqlite"
    );
    assert!(
        !plan_md.contains("generated from .static SQLite")
            && !plan_md.contains("loads systems.static"),
        "S-10: PLAN.md must not claim sde_seed.json is generated from .static SQLite prior to D-1"
    );
    assert!(
        plan_md.contains("72/580") && readme_md.contains("72/580"),
        "S-10: PLAN.md and README.md must report reconciled 72/580 client candidate method count"
    );
    assert!(
        plan_md.contains("`158` `(service, method)` handlers across `118` match arms")
            && readme_md.contains("`158` `(service, method)` handlers across `118` match arms"),
        "S-10: PLAN.md and README.md must report reconciled 158 handlers across 118 match arms"
    );
    // R35-P1-2: Ensure PLAN.md, README.md, docs/BLOCKERS.md, and docs/ACTION_PLAN_STATUS.md match data/sde_import_manifest.json
    for doc in [&plan_md, &readme_md, &blockers_md, &status_md] {
        assert!(
            !doc.contains("`1,532`")
                && !doc.contains("`3,200`")
                && !doc.contains("`3,789`")
                && !doc.contains("`12,840`")
                && !doc.contains("`297` NPC")
                && !doc.contains("`113` regions")
                && !doc.contains("`1,175` constellations"),
            "R35-P1-2: Docs must not contain stale pre-regeneration SDE counts"
        );
    }
}

#[tokio::test]
async fn test_phase_t_t1_machonet_get_time_rpc_parity() {
    // parity: rpc:machoNet.GetTime, plan:T-1
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(101);

    // Initial time query before advancing clock
    let res = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .expect("machoNet.GetTime must succeed");

    let PyRep::Integer(t1) = res else {
        panic!("machoNet.GetTime must return PyRep::Integer, got {res:?}");
    };

    // Live server BlueOS FILETIME ticks are 100ns units since 1601-01-01.
    // In year 2026, ticks exceed 1.34e17 (133_000_000_000_000_000) and are below year 2045 (140_000_000_000_000_000).
    assert!(
        t1 > 133_000_000_000_000_000 && t1 < 140_000_000_000_000_000,
        "GetTime ticks must fall within V24 client operational range [1.33e17, 1.40e17], got {t1}"
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
        t2 >= t1 + 600_000_000 && t2 < t1 + 700_000_000,
        "GetTime ticks must advance by ~60s in 100ns ticks (t1={t1}, t2={t2})"
    );
}
