#!/usr/bin/python3
"""EvoMu FINAL acceptance gate: the only definition of "done" for the /goal loop.

The coding agent may not stop, or tell the user the project is complete, until this script exits 0
on the current working tree. It is deliberately mechanical and slow. Every check runs fresh:

  1. Ledger complete: parity_status over a fresh `cargo test --workspace` run reports 100% of rows
     closed (DONE or audited NA), no FLAGGED_FAKE rows, and no ledger rows removed.
  2. Final claims review: the latest FINAL claims review on this exact tree is PASS (not PASS_WITH_ISSUES). It must
     come from `run_review.py --final`, which uses the final reviewer chain (EVOMU_FINAL_REVIEW_CHAIN, default
     gemini-pro-next > gemini-4-neon-high > fable-5.2-high; fallback only on
     quota/capacity errors; user decision 2026-11-18). Routine (non --final) PASS reviews don't count.
  3. Gap audit: the latest gap audit on this exact tree is CLEAN.
  4. Line coverage (cargo llvm-cov, whole workspace) >= EVOMU_MIN_LINE_COVERAGE (default 90).
  5. Real-client journeys (`references/parity/client_journeys.json` / `supervisor_journeys.json`):
     Per-track coder `run_review.py` stays 100% headless (coders never launch `exefile`, add `// parity: journey:*`
     tags, or edit `.agents/`, and unrun rows report `SUPERVISOR_OWNED` in `parity_status.py`).
     - Interim stage (`interim_smoke`, active immediately): `final_gate.py` requires a fresh, passing
       Supervisor-written real-client attestation in `references/parity/supervisor_journeys.json` matching the
       judged code tree (`git_sha` == HEAD or identical `review_lib.code_tree_of`), current V24.02 client build
       (`3698585` / installed `SharedCache`), `real: true`, `log_errors: []`, and all 7 core smoke scenarios
       passing (`launch_login`, `station_ui`, `undock_navigate`, `market_trade`, `mining_loop`, `pve_combat`,
       `skills_training`), while all 29 journeys are tracked on the `parity_status.py` scoreboard.
     - Target `plan:Z-6` stage (`full_z6`, once `#1202` `--scenario` runner and `#1203` 2nd-player partner land
       and re-express all 29 journeys as Rule-2 UI scenarios): `final_gate.py` promotes from the 7-scenario smoke
       requirement to requiring `29/29` passing deterministic real-client UI scenarios on the current client build.

Writes .evomu-reviews/final_gate.json {tree, passed, checks}. Exit 0 only if everything passes.
Usage: final_gate.py [--skip-coverage] [--skip-client] (skipping required checks makes the gate FAIL; flags exist
only for diagnosing which checks still fail). --skip-mutants is accepted as a deprecated no-op.
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
sys.path.insert(0, os.path.join(HERE, "parity"))
import parity_status as ps_mod  # noqa: E402

SKILL = os.path.dirname(HERE)
PARITY = os.path.join(SKILL, "references", "parity")
SMOKE_SCENARIOS = ps_mod.SMOKE_SCENARIOS
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
    env["CARGO_BUILD_JOBS"] = "4"
    env["RUST_TEST_THREADS"] = "4"
    return env


def latest(state, tree, mode):
    for r in reversed(state.get("reviews", [])):
        if r.get("tree") == tree and r.get("mode", "claims") == mode:
            return r
    return None


def _is_git_sha_fresh(root, att_sha, current_tree=None):
    """Return (is_fresh, head_sha, detail). Accepts either exact `HEAD` commit match or a commit in `root`
    whose `code_tree_of` equals the judged working tree's `code_tree_of` (so committing `supervisor_journeys.json`
    under `.agents/` on top of the tested commit remains fresh until non-gate code changes)."""
    if not isinstance(att_sha, str) or len(att_sha.strip()) < 7:
        return False, "", "missing or malformed git_sha"
    att_sha = att_sha.strip()
    head_proc = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, text=True)
    head_sha = head_proc.stdout.strip() if head_proc.returncode == 0 else ""
    cur_tree = current_tree or rl.worktree_fingerprint(root)
    head_tree = rl.head_tree(root)

    # Working tree with uncommitted code changes relative to HEAD cannot match a prior commit SHA unless code_tree matches.
    att_commit_proc = subprocess.run(
        ["git", "rev-parse", "--verify", "-q", att_sha + "^{commit}"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    resolved_att = att_commit_proc.stdout.strip() if att_commit_proc.returncode == 0 else ""
    if not resolved_att:
        return False, head_sha, "git_sha %r not found in repository" % att_sha

    if head_sha and resolved_att == head_sha and cur_tree == head_tree:
        return True, head_sha, "exact HEAD match"

    att_tree_proc = subprocess.run(
        ["git", "rev-parse", "--verify", "-q", resolved_att + "^{tree}"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    att_tree = att_tree_proc.stdout.strip() if att_tree_proc.returncode == 0 else ""
    if att_tree and cur_tree:
        cur_code = rl.code_tree_of(root, cur_tree)
        att_code = rl.code_tree_of(root, att_tree)
        if cur_code and cur_code == att_code:
            return True, head_sha, "matching code_tree_of (%s)" % cur_code[:12]

    return False, head_sha, "stale git_sha %s (HEAD is %s, code tree changed)" % (
        resolved_att[:12], (head_sha or "unknown")[:12]
    )


def evaluate_supervisor_journeys(
    root,
    current_tree=None,
    attestation_path=None,
    journeys_spec_path=None,
    start_ini_path=None,
    required_stage=None,
):
    """Validate `.agents/skills/evomu-adversarial-review/references/parity/supervisor_journeys.json`
    for `final_gate.py` under Option 3 staged via Option 2."""
    att_path = attestation_path or os.environ.get("EVOMU_SUPERVISOR_JOURNEYS") or os.path.join(
        PARITY, "supervisor_journeys.json"
    )
    expected_ver, expected_build, smoke_ids, all_journey_ids = ps_mod.detect_expected_client_build(
        journeys_spec_path=journeys_spec_path,
        start_ini_path=start_ini_path,
    )
    stage_env = required_stage or os.environ.get("EVOMU_FINAL_GATE_JOURNEY_STAGE", "").strip()

    if not os.path.isfile(att_path):
        return {
            "passed": False,
            "reason": "missing_attestation",
            "detail": "supervisor_journeys.json missing at %s" % att_path,
            "expected_client_build": expected_build,
        }

    try:
        with open(att_path, encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError) as e:
        return {
            "passed": False,
            "reason": "invalid_json",
            "detail": "failed to parse supervisor_journeys.json: %s" % e,
            "expected_client_build": expected_build,
        }

    if not isinstance(doc, dict):
        return {
            "passed": False,
            "reason": "invalid_schema",
            "detail": "supervisor_journeys.json root must be a JSON object",
            "expected_client_build": expected_build,
        }

    stage = stage_env or (doc.get("stage") if doc.get("stage") in ("interim_smoke", "full_z6") else "interim_smoke")
    required_ids = list(all_journey_ids) if stage == "full_z6" else list(smoke_ids)

    if doc.get("real") is not True:
        return {
            "passed": False,
            "stage": stage,
            "reason": "not_real_client",
            "detail": "supervisor_journeys.json requires real=true (got %r)" % (doc.get("real"),),
            "expected_client_build": expected_build,
        }

    fresh, head_sha, sha_detail = _is_git_sha_fresh(root, doc.get("git_sha"), current_tree=current_tree)
    if not fresh:
        return {
            "passed": False,
            "stage": stage,
            "reason": "stale_git_sha",
            "detail": sha_detail,
            "git_sha": doc.get("git_sha"),
            "head_sha": head_sha,
            "expected_client_build": expected_build,
        }

    cver = doc.get("client_version")
    cbuild = doc.get("client_build")
    if not (isinstance(cver, str) and cver.strip()):
        return {
            "passed": False,
            "stage": stage,
            "reason": "missing_client_version",
            "detail": "client_version must be a non-empty string",
            "expected_client_build": expected_build,
        }
    if not (isinstance(cbuild, int) and not isinstance(cbuild, bool) and cbuild == expected_build):
        return {
            "passed": False,
            "stage": stage,
            "reason": "stale_client_build",
            "detail": "client_build=%r does not match expected build %d" % (cbuild, expected_build),
            "client_build": cbuild,
            "expected_client_build": expected_build,
        }

    log_errors = doc.get("log_errors")
    if not isinstance(log_errors, list) or len(log_errors) > 0:
        return {
            "passed": False,
            "stage": stage,
            "reason": "log_errors_present",
            "detail": "log_errors must be an empty list (got %r)" % (log_errors,),
            "log_error_count": len(log_errors) if isinstance(log_errors, list) else -1,
        }

    merged = {}
    for key in ("journeys", "scenarios"):
        sub = doc.get(key)
        if isinstance(sub, dict):
            for sid, val in sub.items():
                if isinstance(val, dict):
                    merged[str(sid)] = dict(merged.get(str(sid), {}), **val)

    missing_or_failed = []
    scenario_log_errors = []
    non_ui_scenarios = []
    for sid in required_ids:
        ent = merged.get(sid)
        if not isinstance(ent, dict) or ent.get("passed") is not True:
            missing_or_failed.append(sid)
            continue
        if "real" in ent and ent.get("real") is not True:
            missing_or_failed.append(sid)
            continue
        if "git_sha" in ent and ent.get("git_sha"):
            ent_fresh, _, _ = _is_git_sha_fresh(root, ent.get("git_sha"), current_tree=current_tree)
            if not ent_fresh:
                missing_or_failed.append(sid)
                continue
        if "client_build" in ent and ent.get("client_build") is not None:
            eb = ent.get("client_build")
            if not (isinstance(eb, int) and not isinstance(eb, bool) and eb == expected_build):
                missing_or_failed.append(sid)
                continue
        ent_errs = ent.get("log_errors", [])
        if not isinstance(ent_errs, list) or len(ent_errs) > 0:
            scenario_log_errors.append(sid)
            continue
        if stage == "full_z6" and ent.get("ui_scenario") is False:
            non_ui_scenarios.append(sid)

    if scenario_log_errors:
        return {
            "passed": False,
            "stage": stage,
            "reason": "log_errors_present",
            "detail": "scenarios with non-empty log_errors: %s" % scenario_log_errors,
            "failed_scenarios": scenario_log_errors,
        }

    if non_ui_scenarios:
        return {
            "passed": False,
            "stage": stage,
            "reason": "non_ui_scenarios",
            "detail": "full_z6 requires Rule-2 UI scenarios (ui_scenario=false on %s)" % non_ui_scenarios,
            "failed_scenarios": non_ui_scenarios,
        }

    if missing_or_failed:
        return {
            "passed": False,
            "stage": stage,
            "reason": "scenarios_incomplete",
            "detail": "%d/%d required scenarios missing or failing: %s" % (
                len(missing_or_failed), len(required_ids), missing_or_failed
            ),
            "passed_count": len(required_ids) - len(missing_or_failed),
            "required_count": len(required_ids),
            "missing_or_failed": missing_or_failed,
        }

    return {
        "passed": True,
        "stage": stage,
        "git_sha": str(doc.get("git_sha")).strip(),
        "freshness": sha_detail,
        "client_version": cver.strip(),
        "client_build": cbuild,
        "passed_count": len(required_ids),
        "required_count": len(required_ids),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-mutants", action="store_true")
    ap.add_argument("--skip-client", action="store_true")
    ap.add_argument("--skip-coverage", action="store_true")
    ap.add_argument("--supervisor-journeys", default="",
                    help="Override path to supervisor_journeys.json")
    a = ap.parse_args()

    root = rl.repo_root(os.getcwd())
    sdir = rl.state_dir(root)
    tree = rl.worktree_fingerprint(root)
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out_dir = os.path.join(sdir, "final_gate", ts)
    os.makedirs(out_dir, exist_ok=True)
    cargo = shutil.which("cargo") or "/opt/homebrew/bin/cargo"
    checks = {}

    # 0. release hygiene guard (#1077 §F)
    rg_script = os.path.join(root, "tools", "release_guard.py")
    if os.path.isfile(rg_script):
        rg_rc, rg_out = sh([sys.executable, rg_script, "--root", root, "--json"], root, timeout=60)
        with open(os.path.join(out_dir, "release_guard.json"), "w", encoding="utf-8") as fh:
            fh.write(rg_out)
        checks["release_guard"] = {"passed": rg_rc == 0, "exit": rg_rc}
    else:
        checks["release_guard"] = {"passed": False, "note": "tools/release_guard.py missing"}

    # 1. ledger
    env = clean_env(sdir, "gate")
    attest = os.path.join(out_dir, "attest")
    os.makedirs(attest, exist_ok=True)
    env["EVOMU_PARITY_ATTEST_DIR"] = attest
    env["EVOMU_PERF_TESTS"] = "1"
    rc, out = sh([cargo, "test", "-j", "4", "--workspace", "--no-fail-fast"], root, env=env, timeout=4 * 3600)
    open(os.path.join(out_dir, "tests.txt"), "w", encoding="utf-8").write(out)
    failed = sum(int(m) for m in re.findall(r"test result: \w+\. \d+ passed; (\d+) failed", out))
    parity_cmd = [sys.executable, os.path.join(HERE, "parity", "parity_status.py"), "--tests",
                  os.path.join(out_dir, "tests.txt"), "--attest-dir", attest, "--out",
                  os.path.join(out_dir, "parity.json"), "--md", os.path.join(out_dir, "parity.md"),
                  "--state-dir", sdir]
    if a.supervisor_journeys:
        parity_cmd += ["--supervisor-journeys", a.supervisor_journeys]
    prc, pout = sh(parity_cmd, root, timeout=1800)
    try:
        summ = json.load(open(os.path.join(out_dir, "parity.json")))["summary"]
        flagged = sum(1 for v in json.load(open(os.path.join(out_dir, "parity.json")))["rows"].values()
                      if v["status"] == "FLAGGED_FAKE")
    except Exception:  # noqa: BLE001
        summ, flagged = {"complete": False, "error": pout[-1500:]}, -1
    checks["tests_all_pass"] = {"passed": rc == 0 and failed == 0, "exit": rc, "failed": failed}
    checks["ledger_complete"] = {"passed": bool(summ.get("complete")) and flagged == 0,
                                 "percent_closed": summ.get("percent_closed"),
                                 "open_rows": summ.get("open_rows"), "flagged_fake": flagged,
                                 "parked_rows": summ.get("parked_rows", 0)}

    # 2/3. reviews on this exact tree
    state = rl.load_state(root)
    finals = [r for r in state.get("reviews", []) if r.get("final")]
    cr = latest({"reviews": finals}, tree, "claims")
    ga = latest(state, tree, "gap-audit")
    final_models = [m.strip() for m in os.environ.get(
        "EVOMU_FINAL_REVIEW_CHAIN", "gemini-pro-next,gemini-4-neon-high,fable-5.2-high").split(",") if m.strip()]
    if not (cr and cr.get("verdict") == "PASS" and cr.get("model") in final_models):
        cr = rl.reviewed_by_code(root, verdicts=("PASS",), mode="claims", models=final_models, tree=tree,
                                 final_only=True) or cr
    if not (ga and ga.get("verdict") == "CLEAN"):
        ga = rl.reviewed_by_code(root, verdicts=("CLEAN",), mode="gap-audit", tree=tree) or ga
    checks["claims_review_pass"] = {"passed": bool(cr and cr.get("final") and cr.get("verdict") == "PASS"
                                                   and cr.get("model") in final_models),
                                    "required_model": final_models,
                                    "how": "run_review.py --final (final reviewer chain); routine reviews don't count",
                                    "latest": cr and {k: cr.get(k) for k in ("verdict", "model", "timestamp", "report")}}
    checks["gap_audit_clean"] = {"passed": bool(ga and ga.get("verdict") == "CLEAN"),
                                 "latest": ga and {k: ga.get(k) for k in ("verdict", "timestamp", "report")}}

    # 4. coverage
    min_cov = float(os.environ.get("EVOMU_MIN_LINE_COVERAGE", "90"))
    if a.skip_coverage:
        checks["line_coverage"] = {"passed": False, "note": "skipped"}
    else:
        crc, cout = sh([cargo, "llvm-cov", "-j", "4", "--workspace", "--summary-only", "--json"], root,
                       env=clean_env(sdir, "cov"), timeout=6 * 3600)
        pct = None
        try:
            pct = json.loads(cout.split("\n[stderr]")[0])["data"][0]["totals"]["lines"]["percent"]
        except Exception:  # noqa: BLE001
            pass
        open(os.path.join(out_dir, "coverage.txt"), "w", encoding="utf-8").write(cout[-20000:])
        checks["line_coverage"] = {"passed": pct is not None and pct >= min_cov, "percent": pct,
                                   "min": min_cov, "note": None if pct is not None else
                                   "cargo-llvm-cov missing or failed (cargo install cargo-llvm-cov)"}

    # 5. Supervisor-attested real-client journeys (`supervisor_journeys.json`):
    #    7 core smoke scenarios in `interim_smoke` stage, promoting to 29/29 UI scenarios in `full_z6` stage.
    if a.skip_client:
        checks["real_client_journeys"] = {"passed": False, "note": "skipped (--skip-client never satisfies final_gate)"}
    else:
        checks["real_client_journeys"] = evaluate_supervisor_journeys(
            root,
            current_tree=tree,
            attestation_path=a.supervisor_journeys or None,
        )

    passed = all(c.get("passed") for c in checks.values())
    result = {"tree": tree, "timestamp": ts, "passed": passed, "checks": checks}
    json.dump(result, open(os.path.join(out_dir, "final_gate.json"), "w", encoding="utf-8"), indent=2)
    json.dump(result, open(os.path.join(sdir, "final_gate.json"), "w", encoding="utf-8"), indent=2)
    for k, c in checks.items():
        print("[final-gate] %-22s %s  %s" % (k, "PASS" if c.get("passed") else "FAIL",
                                             {x: y for x, y in c.items() if x != "passed"}))
    print("[final-gate] %s (details: %s)" % ("PASSED" if passed else "NOT PASSED", out_dir))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
