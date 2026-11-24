use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "evomu_arch_debt_ext_{label}_{}_{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

#[test]
fn arch_debt_ratchet_1194_corp_grant_promotion_and_fold_call_sites() {
    let tmp = make_temp_dir("1194_f1_corp_grant_ratchets");
    let src = tmp.join("src");
    std::fs::create_dir_all(src.join("services")).expect("create services dir");
    std::fs::write(
        src.join("services/bad.rs"),
        r#"
// promote_due_pending_corp_grants(ctx, corp_id);
const LIT: &str = "fold_due_pending_corp_grants_in_snap(snap, corp_id, now)";
pub fn f(ctx: &Ctx, snap: &mut S, corp_id: i64, now: u64) {
    promote_due_pending_corp_grants(ctx, corp_id);
    promote_due_pending_corp_grants (ctx, corp_id);
    let _ = fold_due_pending_corp_grants_in_snap(snap, corp_id, now);
}
#[cfg(test)]
mod tests {
    fn t(ctx: &Ctx) { promote_due_pending_corp_grants(ctx, 1); }
}
"#,
    )
    .expect("write services/bad.rs");

    let ratchet = ArchDebtRatchet::load_default().expect("load default arch_debt_ratchet.json");
    let promo_rule = &ratchet.patterns["handler_corp_grant_promotion"];
    assert!(promo_rule.zero_required);
    assert_eq!(promo_rule.owning_issue, Some(1194));
    assert!(promo_rule.files.is_empty());

    let fold_rule = &ratchet.patterns["corp_grant_fold_call_sites"];
    assert!(!fold_rule.zero_required);
    assert_eq!(fold_rule.owning_issue, Some(1194));
    let expected_fold_files: std::collections::BTreeSet<&str> = [
        "services/corp_registry_ops.rs",
        "services/fleet_social_corp_gov.rs",
        "services/fleet_social_corp_tail.rs",
        "services/fleet_social_corp_tail_dispatch.rs",
        "services/fleet_social_corps.rs",
    ]
    .into_iter()
    .collect();
    let actual_fold_files: std::collections::BTreeSet<&str> =
        fold_rule.files.keys().map(String::as_str).collect();
    assert_eq!(actual_fold_files, expected_fold_files);

    let report = scan_arch_debt_in_dir(&src).expect("scan 1194 f1 grant ratchets");
    assert_eq!(report.patterns["handler_corp_grant_promotion"].occurrences, 2);
    assert_eq!(
        report.patterns["handler_corp_grant_promotion"]
            .per_file
            .get("services/bad.rs"),
        Some(&2)
    );
    assert_eq!(report.patterns["corp_grant_fold_call_sites"].occurrences, 1);
    assert_eq!(
        report.patterns["corp_grant_fold_call_sites"]
            .per_file
            .get("services/bad.rs"),
        Some(&1)
    );

    let err = ratchet
        .assert_ratchet(&report)
        .expect_err("ratchets must reject promote_due_pending_corp_grants and unlisted fold calls");
    assert!(
        err.contains("handler_corp_grant_promotion") || err.contains("corp_grant_fold_call_sites"),
        "unexpected error message: {err}"
    );

    let clean_src = tmp.join("clean_src");
    std::fs::create_dir_all(clean_src.join("services")).expect("create clean services dir");
    std::fs::write(
        clean_src.join("services/clean.rs"),
        "pub fn check(snap: &S, cid: i64, char_id: i64, now: u64) -> (bool, u64) {\n    snap.effective_corp_roles_at_db(cid, char_id, None, now)\n}\n",
    )
    .expect("write clean.rs");
    let clean_report = ratchet
        .verify_dir(&clean_src)
        .expect("clean source without grant promotion must pass");
    assert_eq!(clean_report.patterns["handler_corp_grant_promotion"].occurrences, 0);
    assert_eq!(clean_report.patterns["corp_grant_fold_call_sites"].occurrences, 0);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn rust_and_python_ratchet_scanners_agree_on_live_repo() {
    let canonical_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root");
    let root_str = canonical_root.to_string_lossy();
    let repo_root = PathBuf::from(root_str.trim_start_matches(r"\\?\"));
    let script_path =
        repo_root.join(".agents/skills/evomu-adversarial-review/scripts/tighten_ratchets.py");
    assert!(
        script_path.is_file(),
        "missing tighten_ratchets.py at {}",
        script_path.display()
    );

    let output = ["python3", "python"]
        .into_iter()
        .find_map(|interp| {
            match std::process::Command::new(interp)
                .arg(&script_path)
                .arg("--root")
                .arg(&repo_root)
                .arg("--dump-counts-json")
                .output()
            {
                Ok(out) => Some(Ok(out)),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => Some(Err(format!("failed to execute {interp}: {err}"))),
            }
        })
        .expect("neither python3 nor python interpreter found in PATH")
        .expect("invoke tighten_ratchets.py --dump-counts-json");
    assert!(
        output.status.success(),
        "tighten_ratchets.py --dump-counts-json failed ({:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let py: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("parse --dump-counts-json JSON output");

    // 1. arch_debt per-rule per-file parity
    let py_arch: BTreeMap<String, BTreeMap<String, usize>> =
        serde_json::from_value(py["arch_debt"].clone()).expect("parse py arch_debt");
    let arch_ratchet = ArchDebtRatchet::load_from_path(
        &repo_root.join("crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
    )
    .expect("load arch_debt_ratchet.json");
    let rust_arch_report = arch_ratchet
        .verify_dir(&repo_root.join("crates/eve-server/src"))
        .expect("verify arch_debt on repo");
    let rust_arch: BTreeMap<String, BTreeMap<String, usize>> = rust_arch_report
        .patterns
        .into_iter()
        .map(|(k, v)| (k, v.per_file))
        .collect();
    assert_eq!(
        rust_arch, py_arch,
        "Rust vs Python arch_debt per-file scan mismatch"
    );

    // 2. client_compat per-counter per-file + allowlist set parity
    let py_compat = &py["client_compat"];
    let py_compat_counters: BTreeMap<String, BTreeMap<String, usize>> =
        serde_json::from_value(py_compat["counters"].clone())
            .expect("parse py client_compat.counters");
    let py_known_rules: std::collections::BTreeSet<String> =
        serde_json::from_value(py_compat["known_rules"].clone())
            .expect("parse py client_compat.known_rules");
    let py_svc_files: std::collections::BTreeSet<String> =
        serde_json::from_value(py_compat["service_caller_trace_files"].clone())
            .expect("parse py client_compat.service_caller_trace_files");
    let py_it_files: std::collections::BTreeSet<String> =
        serde_json::from_value(py_compat["it_dispatch_rpc_files"].clone())
            .expect("parse py client_compat.it_dispatch_rpc_files");

    let rust_compat = crate::client_compat_ratchet::scan_client_compat_in_dirs(
        &repo_root.join("crates/eve-server"),
        &repo_root.join("crates/eve-server/tests/it"),
    )
    .expect("scan client_compat in repo");
    assert_eq!(
        rust_compat.counters, py_compat_counters,
        "Rust vs Python client_compat per-file counters mismatch"
    );
    assert_eq!(
        rust_compat.known_rules, py_known_rules,
        "Rust vs Python client_compat known_rules mismatch"
    );
    assert_eq!(
        rust_compat.service_caller_trace_files, py_svc_files,
        "Rust vs Python client_compat service_caller_trace_files mismatch"
    );
    assert_eq!(
        rust_compat.it_dispatch_rpc_files, py_it_files,
        "Rust vs Python client_compat it_dispatch_rpc_files mismatch"
    );

    // 3. untyped_user_error per-file parity
    let py_untyped: BTreeMap<String, usize> =
        serde_json::from_value(py["untyped_user_error"].clone())
            .expect("parse py untyped_user_error");
    let rust_untyped = crate::client_contract::scan_untyped_user_error_per_file(
        &repo_root.join("crates/eve-server/src"),
    )
    .expect("scan untyped_user_error in repo");
    assert_eq!(
        rust_untyped, py_untyped,
        "Rust vs Python untyped_user_error per-file counts mismatch"
    );
}
