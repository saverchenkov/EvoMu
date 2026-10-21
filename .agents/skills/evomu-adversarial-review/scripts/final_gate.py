#!/usr/bin/python3
"""EvoMu FINAL acceptance gate: the only definition of "done" for the /goal loop.

The coding agent may not stop, or tell the user the project is complete, until this script exits 0
on the current working tree. It is deliberately mechanical and slow. Every check runs fresh:

  1. Ledger complete: parity_status over a fresh `cargo test --workspace` run reports 100% of rows
     closed (DONE or audited NA), no FLAGGED_FAKE rows, and no ledger rows removed.
  2. Claims review: the latest claims review on this exact tree is PASS (not PASS_WITH_ISSUES), by the primary
     reviewer model (EVOMU_FINAL_REVIEW_MODEL, default opus-5.5-medium/high, gemini-launch-candidate-high, or gemini-flash-next), not a quota fallback.
  3. Gap audit: the latest gap audit on this exact tree is CLEAN.
  4. Line coverage (cargo llvm-cov, whole workspace) >= EVOMU_MIN_LINE_COVERAGE (default 90).
  5. Mutation score (cargo mutants, every workspace crate) >= EVOMU_MIN_MUTATION_SCORE (default 85),
     where score = caught / (caught + missed); timeouts count as caught, unviable mutants are excluded.
  6. Real client: tools/client-harness/run_journeys (agent-built) drives the REAL V24.01 client
     (~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app) against a locally started
     EvoMu server through every journey in references/parity/client_journeys.json. It must write
     <out>/result.json = {"journeys": {"<id>": {"passed": true, "evidence": "...", ...}}, "server_log": path,
     "client_logs": [paths]}. The gate requires every journey passed and zero `UnimplementedRPC`,
     `panicked at`, or client `Traceback`/`EXCEPTION` lines in the logs.

Writes .evomu-reviews/final_gate.json {tree, passed, checks}. Exit 0 only if everything passes.
Usage: final_gate.py [--skip-mutants] (skipping any check makes the gate FAIL; flags exist only for
diagnosing which checks still fail).
"""
import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import review_lib as rl  # noqa: E402

SKILL = os.path.dirname(HERE)
PARITY = os.path.join(SKILL, "references", "parity")
WRAPPER_ENV = ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "RUSTDOC", "CARGO_BUILD_RUSTDOC",
               "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR"]


def sh(cmd, cwd, env=None, timeout=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout + (("\n[stderr]\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired as e:
        return 124, "TIMEOUT after %ss\n%s" % (timeout, e.stdout or "")
    except FileNotFoundError as e:
        return 127, "NOT FOUND: %s" % e


def clean_env(sdir, sub):
    env = {k: v for k, v in os.environ.items() if k not in WRAPPER_ENV}
    env["CARGO_TARGET_DIR"] = os.path.join(sdir, "target-" + sub)
    return env


def latest(state, tree, mode):
    for r in reversed(state.get("reviews", [])):
        if r.get("tree") == tree and r.get("mode", "claims") == mode:
            return r
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-mutants", action="store_true")
    ap.add_argument("--skip-client", action="store_true")
    ap.add_argument("--skip-coverage", action="store_true")
    a = ap.parse_args()

    root = rl.repo_root(os.getcwd())
    sdir = rl.state_dir(root)
    tree = rl.worktree_fingerprint(root)
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out_dir = os.path.join(sdir, "final_gate", ts)
    os.makedirs(out_dir, exist_ok=True)
    cargo = shutil.which("cargo") or "/opt/homebrew/bin/cargo"
    checks = {}

    # 1. ledger
    env = clean_env(sdir, "gate")
    attest = os.path.join(out_dir, "attest")
    os.makedirs(attest, exist_ok=True)
    env["EVOMU_PARITY_ATTEST_DIR"] = attest
    rc, out = sh([cargo, "test", "--workspace", "--no-fail-fast"], root, env=env, timeout=4 * 3600)
    open(os.path.join(out_dir, "tests.txt"), "w").write(out)
    failed = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; (\d+) failed", out))
    prc, pout = sh([sys.executable, os.path.join(HERE, "parity", "parity_status.py"), "--tests",
                    os.path.join(out_dir, "tests.txt"), "--attest-dir", attest, "--out",
                    os.path.join(out_dir, "parity.json"), "--md", os.path.join(out_dir, "parity.md"),
                    "--state-dir", sdir], root, timeout=1800)
    try:
        summ = json.load(open(os.path.join(out_dir, "parity.json")))["summary"]
        flagged = sum(1 for v in json.load(open(os.path.join(out_dir, "parity.json")))["rows"].values()
                      if v["status"] == "FLAGGED_FAKE")
    except Exception:  # noqa: BLE001
        summ, flagged = {"complete": False, "error": pout[-1500:]}, -1
    checks["tests_all_pass"] = {"passed": rc == 0 and failed == 0, "exit": rc, "failed": failed}
    checks["ledger_complete"] = {"passed": bool(summ.get("complete")) and flagged == 0,
                                 "percent_closed": summ.get("percent_closed"),
                                 "open_rows": summ.get("open_rows"), "flagged_fake": flagged}

    # 2/3. reviews on this exact tree
    state = rl.load_state(root)
    cr = latest(state, tree, "claims")
    ga = latest(state, tree, "gap-audit")
    final_models = [m.strip() for m in os.environ.get(
        "EVOMU_FINAL_REVIEW_MODEL", "opus-5.5-medium,opus-5.5-high,gemini-launch-candidate-high,gemini-flash-next").split(",") if m.strip()]
    if not (cr and cr.get("verdict") == "PASS" and cr.get("model") in final_models):
        cr = rl.reviewed_by_code(root, verdicts=("PASS",), mode="claims", models=final_models, tree=tree) or cr
    if not (ga and ga.get("verdict") == "CLEAN"):
        ga = rl.reviewed_by_code(root, verdicts=("CLEAN",), mode="gap-audit", tree=tree) or ga
    checks["claims_review_pass"] = {"passed": bool(cr and cr.get("verdict") == "PASS" and cr.get("model") in final_models),
                                    "required_model": final_models,
                                    "latest": cr and {k: cr.get(k) for k in ("verdict", "model", "timestamp", "report")}}
    checks["gap_audit_clean"] = {"passed": bool(ga and ga.get("verdict") == "CLEAN"),
                                 "latest": ga and {k: ga.get(k) for k in ("verdict", "timestamp", "report")}}

    # 4. coverage
    min_cov = float(os.environ.get("EVOMU_MIN_LINE_COVERAGE", "90"))
    if a.skip_coverage:
        checks["line_coverage"] = {"passed": False, "note": "skipped"}
    else:
        crc, cout = sh([cargo, "llvm-cov", "--workspace", "--summary-only", "--json"], root,
                       env=clean_env(sdir, "cov"), timeout=6 * 3600)
        pct = None
        try:
            pct = json.loads(cout.split("\n[stderr]")[0])["data"][0]["totals"]["lines"]["percent"]
        except Exception:  # noqa: BLE001
            pass
        open(os.path.join(out_dir, "coverage.txt"), "w").write(cout[-20000:])
        checks["line_coverage"] = {"passed": pct is not None and pct >= min_cov, "percent": pct,
                                   "min": min_cov, "note": None if pct is not None else
                                   "cargo-llvm-cov missing or failed (cargo install cargo-llvm-cov)"}

    # 5. mutation testing
    min_mut = float(os.environ.get("EVOMU_MIN_MUTATION_SCORE", "85"))
    if a.skip_mutants:
        checks["mutation_score"] = {"passed": False, "note": "skipped"}
    else:
        mdir = os.path.join(out_dir, "mutants")
        mrc, mout = sh([cargo, "mutants", "--workspace", "--no-shuffle", "--output", mdir, "--timeout-multiplier",
                        "3"], root, env=clean_env(sdir, "mut"), timeout=48 * 3600)
        caught = missed = timeout_n = 0
        op = os.path.join(mdir, "mutants.out", "outcomes.json")
        try:
            o = json.load(open(op))
            caught, missed, timeout_n = o.get("caught", 0), o.get("missed", 0), o.get("timeout", 0)
        except Exception:  # noqa: BLE001
            pass
        denom = caught + timeout_n + missed
        score = 100.0 * (caught + timeout_n) / denom if denom else None
        open(os.path.join(out_dir, "mutants.txt"), "w").write(mout[-20000:])
        checks["mutation_score"] = {"passed": score is not None and score >= min_mut, "score": score,
                                    "caught": caught, "missed": missed, "timeout": timeout_n, "min": min_mut,
                                    "note": None if denom else "cargo-mutants missing or failed (cargo install cargo-mutants)"}

    # 6. real client journeys
    if a.skip_client:
        checks["real_client_journeys"] = {"passed": False, "note": "skipped"}
    else:
        harness = os.path.join(root, "tools", "client-harness", "run_journeys")
        jdir = os.path.join(out_dir, "client")
        os.makedirs(jdir, exist_ok=True)
        want = [j["id"] for j in json.load(open(os.path.join(PARITY, "client_journeys.json")))["journeys"]]
        res, ok, bad_lines, missing = {}, False, [], want
        if os.path.exists(harness) and os.access(harness, os.X_OK):
            hrc, hout = sh([harness, "--out", jdir, "--journeys", os.path.join(PARITY, "client_journeys.json")],
                           root, timeout=12 * 3600)
            open(os.path.join(out_dir, "client_harness.txt"), "w").write(hout[-20000:])
            try:
                res = json.load(open(os.path.join(jdir, "result.json")))
            except Exception:  # noqa: BLE001
                res = {}
            js = res.get("journeys", {})
            missing = [j for j in want if not (js.get(j) or {}).get("passed")]
            for lp in [res.get("server_log")] + list(res.get("client_logs") or []):
                if lp and os.path.exists(lp):
                    for ln in open(lp, errors="replace"):
                        if re.search(r"UnimplementedRPC|panicked at|Traceback \(most recent|\bEXCEPTION\b", ln):
                            bad_lines.append(ln.strip()[:300])
            ok = hrc == 0 and not missing and not bad_lines and bool(res.get("server_log"))
        checks["real_client_journeys"] = {"passed": ok, "missing_or_failed": missing[:50],
                                          "log_errors": bad_lines[:50],
                                          "note": None if os.path.exists(harness) else
                                          "tools/client-harness/run_journeys not found"}

    passed = all(c.get("passed") for c in checks.values())
    result = {"tree": tree, "timestamp": ts, "passed": passed, "checks": checks}
    json.dump(result, open(os.path.join(out_dir, "final_gate.json"), "w"), indent=2)
    json.dump(result, open(os.path.join(sdir, "final_gate.json"), "w"), indent=2)
    for k, c in checks.items():
        print("[final-gate] %-22s %s  %s" % (k, "PASS" if c.get("passed") else "FAIL",
                                             {x: y for x, y in c.items() if x != "passed"}))
    print("[final-gate] %s (details: %s)" % ("PASSED" if passed else "NOT PASSED", out_dir))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
