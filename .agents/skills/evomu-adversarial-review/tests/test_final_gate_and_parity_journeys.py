#!/usr/bin/python3
"""Unit tests for Decision 1: two-stage Supervisor-owned real-client journey governance
(`final_gate.py`, `parity_status.py`, `status_table.py`, and governance document alignment)."""
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest import mock

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
SKILL_DIR = os.path.dirname(TESTS_DIR)
SCRIPTS_DIR = os.path.join(SKILL_DIR, "scripts")
PARITY_SCRIPTS_DIR = os.path.join(SCRIPTS_DIR, "parity")
PARITY_REF_DIR = os.path.join(SKILL_DIR, "references", "parity")
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(SKILL_DIR)))

for _p in (SCRIPTS_DIR, PARITY_SCRIPTS_DIR):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import final_gate  # noqa: E402
import parity_status  # noqa: E402
import status_table  # noqa: E402


def _read_json(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, indent=2)


def _init_temp_git_repo(tmp_dir):
    subprocess.run(["git", "init", "-q"], cwd=tmp_dir, check=True)
    subprocess.run(["git", "config", "user.email", "test@evomu.local"], cwd=tmp_dir, check=True)
    subprocess.run(["git", "config", "user.name", "EvoMu Test"], cwd=tmp_dir, check=True)
    crate_src = os.path.join(tmp_dir, "crates", "eve-server", "src")
    os.makedirs(crate_src, exist_ok=True)
    with open(os.path.join(crate_src, "lib.rs"), "w", encoding="utf-8") as fh:
        fh.write("pub fn version() -> u32 { 1 }\n")
    subprocess.run(["git", "add", "."], cwd=tmp_dir, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial code commit"], cwd=tmp_dir, check=True)
    return subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=tmp_dir, capture_output=True, text=True, check=True
    ).stdout.strip()


def _make_valid_smoke_doc(git_sha, build=3698585, stage="interim_smoke"):
    scenarios = {}
    for sid in parity_status.SMOKE_SCENARIOS:
        scenarios[sid] = {
            "passed": True,
            "real": True,
            "ui_scenario": True,
            "log_errors": [],
        }
    return {
        "schema_version": 1,
        "stage": stage,
        "git_sha": git_sha,
        "client_version": "V24.02",
        "client_build": build,
        "real": True,
        "recorded_at": "2026-11-16T23:00:00Z",
        "log_errors": [],
        "scenarios": scenarios,
        "journeys": dict(scenarios),
    }


class TestFinalGateAndParityJourneys(unittest.TestCase):
    def test_default_checked_in_supervisor_journeys_is_clean_unrun_template(self):
        sup_path = os.path.join(PARITY_REF_DIR, "supervisor_journeys.json")
        self.assertTrue(os.path.isfile(sup_path))
        doc = _read_json(sup_path)
        self.assertEqual(doc.get("stage"), "interim_smoke")
        self.assertIsNone(doc.get("git_sha"))
        self.assertIs(doc.get("real"), False)
        self.assertEqual(doc.get("client_version"), "V24.02")
        self.assertEqual(doc.get("client_build"), 3698585)
        self.assertEqual(doc.get("log_errors"), [])
        self.assertEqual(tuple(doc.get("smoke_required", ())), parity_status.SMOKE_SCENARIOS)

        res = final_gate.evaluate_supervisor_journeys(
            REPO_ROOT,
            attestation_path=sup_path,
            start_ini_path="",
        )
        self.assertFalse(res["passed"])
        self.assertEqual(res["reason"], "not_real_client")

    def test_detect_expected_client_build_reads_spec_and_start_ini_override(self):
        ver, build, smoke_ids, all_ids = parity_status.detect_expected_client_build(start_ini_path="")
        self.assertEqual(ver, "V24.02")
        self.assertEqual(build, 3698585)
        self.assertEqual(tuple(smoke_ids), parity_status.SMOKE_SCENARIOS)
        self.assertEqual(len(all_ids), 29)

        with tempfile.TemporaryDirectory() as tmp:
            ini = os.path.join(tmp, "start.ini")
            with open(ini, "w", encoding="utf-8") as fh:
                fh.write("[main]\nversion = 24.03\nbuild = 3712345\ncodename = V24.03\n")
            ver2, build2, _, _ = parity_status.detect_expected_client_build(start_ini_path=ini)
            self.assertEqual(ver2, "V24.03")
            self.assertEqual(build2, 3712345)

    def test_final_gate_rejects_missing_corrupt_stale_fake_and_log_errors(self):
        with tempfile.TemporaryDirectory() as repo_tmp, tempfile.TemporaryDirectory() as ext_tmp:
            head_sha = _init_temp_git_repo(repo_tmp)
            att_path = os.path.join(ext_tmp, "supervisor_journeys.json")

            # 1. Missing file
            r_missing = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_missing["passed"])
            self.assertEqual(r_missing["reason"], "missing_attestation")

            # 2. Invalid JSON
            with open(att_path, "w", encoding="utf-8") as fh:
                fh.write("{not-valid-json")
            r_json = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_json["passed"])
            self.assertEqual(r_json["reason"], "invalid_json")

            # 3. real: false
            doc = _make_valid_smoke_doc(head_sha)
            doc["real"] = False
            _write_json(att_path, doc)
            r_fake = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_fake["passed"])
            self.assertEqual(r_fake["reason"], "not_real_client")

            # 4. Unknown / stale git_sha
            doc = _make_valid_smoke_doc("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
            _write_json(att_path, doc)
            r_sha = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_sha["passed"])
            self.assertEqual(r_sha["reason"], "stale_git_sha")

            # 5. Stale client_build (e.g. old 3619879 instead of 3698585)
            doc = _make_valid_smoke_doc(head_sha, build=3619879)
            _write_json(att_path, doc)
            r_build = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_build["passed"])
            self.assertEqual(r_build["reason"], "stale_client_build")

            # 6. Non-empty top-level log_errors
            doc = _make_valid_smoke_doc(head_sha)
            doc["log_errors"] = ["UnimplementedRPC: skillMgr2.Bogus"]
            _write_json(att_path, doc)
            r_err = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_err["passed"])
            self.assertEqual(r_err["reason"], "log_errors_present")

            # 7. Non-empty per-scenario log_errors
            doc = _make_valid_smoke_doc(head_sha)
            doc["scenarios"]["undock_navigate"]["log_errors"] = ["CustomError: traceback"]
            _write_json(att_path, doc)
            r_scen_err = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_scen_err["passed"])
            self.assertEqual(r_scen_err["reason"], "log_errors_present")

            # 8. Failing / missing required smoke scenario
            doc = _make_valid_smoke_doc(head_sha)
            doc["scenarios"]["pve_combat"]["passed"] = False
            doc["journeys"]["pve_combat"]["passed"] = False
            _write_json(att_path, doc)
            r_inc = final_gate.evaluate_supervisor_journeys(
                repo_tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_inc["passed"])
            self.assertEqual(r_inc["reason"], "scenarios_incomplete")
            self.assertIn("pve_combat", r_inc["missing_or_failed"])

    def test_final_gate_passes_valid_smoke_and_tracks_code_tree_freshness_and_full_z6(self):
        with tempfile.TemporaryDirectory() as tmp:
            c0_sha = _init_temp_git_repo(tmp)
            agents_parity = os.path.join(
                tmp, ".agents", "skills", "evomu-adversarial-review", "references", "parity"
            )
            att_path = os.path.join(agents_parity, "supervisor_journeys.json")
            doc = _make_valid_smoke_doc(c0_sha)
            _write_json(att_path, doc)

            # Valid on C0 itself
            r0 = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertTrue(r0["passed"], r0)
            self.assertEqual(r0["passed_count"], 7)

            # Committing `.agents/.../supervisor_journeys.json` on top of C0 (creating C1) preserves code_tree_of!
            subprocess.run(["git", "add", ".agents"], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "record supervisor journey attestation"], cwd=tmp, check=True)
            r1 = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertTrue(r1["passed"], r1)
            self.assertIn("code_tree_of", r1["freshness"])

            # Promoting to `full_z6` with only 7/29 scenarios fails until all 29 pass as UI scenarios
            r_z6_partial = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path="", required_stage="full_z6"
            )
            self.assertFalse(r_z6_partial["passed"])
            self.assertEqual(r_z6_partial["reason"], "scenarios_incomplete")
            self.assertEqual(r_z6_partial["required_count"], 29)

            _, _, _, all_29 = parity_status.detect_expected_client_build(start_ini_path="")
            target_jid = all_29[-1]
            for jid in all_29:
                doc["journeys"][jid] = {"passed": True, "real": True, "ui_scenario": True, "log_errors": []}
            doc["journeys"][target_jid]["ui_scenario"] = False
            _write_json(att_path, doc)
            r_non_ui = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path="", required_stage="full_z6"
            )
            self.assertFalse(r_non_ui["passed"])
            self.assertEqual(r_non_ui["reason"], "non_ui_scenarios")

            doc["journeys"][target_jid]["ui_scenario"] = True
            _write_json(att_path, doc)
            r_z6_ok = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path="", required_stage="full_z6"
            )
            self.assertTrue(r_z6_ok["passed"], r_z6_ok)
            self.assertEqual(r_z6_ok["passed_count"], 29)

            # Subsequent code modification under `crates/` invalidates C0's attestation immediately!
            with open(os.path.join(tmp, "crates", "eve-server", "src", "lib.rs"), "w", encoding="utf-8") as fh:
                fh.write("pub fn version() -> u32 { 2 }\n")
            subprocess.run(["git", "add", "crates"], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "modify server code"], cwd=tmp, check=True)
            r_stale = final_gate.evaluate_supervisor_journeys(
                tmp, attestation_path=att_path, start_ini_path=""
            )
            self.assertFalse(r_stale["passed"])
            self.assertEqual(r_stale["reason"], "stale_git_sha")

    def test_parity_status_and_status_table_reflect_supervisor_scoreboard(self):
        with tempfile.TemporaryDirectory() as tmp:
            tests_txt = os.path.join(tmp, "tests.txt")
            with open(tests_txt, "w", encoding="utf-8") as fh:
                fh.write("test dummy ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored\n")

            def _run_fast_parity_status(extra_args):
                argv = ["parity_status.py", "--tests", tests_txt] + extra_args
                buf = io.StringIO()
                with mock.patch.dict(os.environ, {"EVOMU_CLIENT_START_INI": ""}), mock.patch.object(
                    sys, "argv", argv
                ), mock.patch.object(parity_status, "repo_root", return_value=REPO_ROOT), mock.patch.object(
                    parity_status, "rust_files", return_value=[]
                ), redirect_stdout(buf):
                    parity_status.main()

            # 1. Default template -> 0/29 verified, T-6/ESI-1/Z-6 remain SUPERVISOR_OWNED
            par0_json = os.path.join(tmp, "parity0.json")
            _run_fast_parity_status(["--out", par0_json])
            par0 = _read_json(par0_json)
            self.assertEqual(par0["summary"]["journeys"]["DONE"], 0)
            self.assertEqual(par0["rows"]["journey:launch_login"]["status"], "SUPERVISOR_OWNED")
            self.assertEqual(par0["rows"]["plan:T-6"]["status"], "SUPERVISOR_OWNED")
            self.assertEqual(par0["rows"]["plan:ESI-1"]["status"], "SUPERVISOR_OWNED")
            self.assertEqual(par0["rows"]["plan:Z-6"]["status"], "SUPERVISOR_OWNED")

            # 2. Valid 7-smoke attestation -> 7 smoke journeys + plan:T-6 + plan:ESI-1 transition to DONE!
            smoke_att = os.path.join(tmp, "sup_smoke.json")
            doc = _make_valid_smoke_doc("d71e76b49abcdef")
            _write_json(smoke_att, doc)
            par7_json = os.path.join(tmp, "parity7.json")
            par7_md = os.path.join(tmp, "parity7.md")
            _run_fast_parity_status(["--supervisor-journeys", smoke_att, "--out", par7_json, "--md", par7_md])
            par7 = _read_json(par7_json)
            self.assertEqual(par7["summary"]["journeys"]["DONE"], 7)
            self.assertEqual(par7["summary"]["journeys"]["smoke_DONE"], 7)
            for sid in parity_status.SMOKE_SCENARIOS:
                self.assertEqual(par7["rows"]["journey:" + sid]["status"], "DONE")
            self.assertEqual(par7["rows"]["plan:T-6"]["status"], "DONE")
            self.assertEqual(par7["rows"]["plan:ESI-1"]["status"], "DONE")
            self.assertEqual(par7["rows"]["plan:Z-6"]["status"], "SUPERVISOR_OWNED")

            status_md = os.path.join(tmp, "ACTION_PLAN_STATUS.md")
            with mock.patch.object(
                sys, "argv", ["status_table.py", "--parity", par7_json, "--out", status_md]
            ), mock.patch.object(status_table, "last_commit", return_value="—"), redirect_stdout(io.StringIO()):
                status_table.main()
            with open(status_md, encoding="utf-8") as fh:
                st_txt = fh.read()
            self.assertIn("Real-client journeys (supervisor scoreboard", st_txt)
            self.assertIn("**7 / 29 verified**", st_txt)
            self.assertRegex(st_txt, r"\|\s*`?T-6`?\s*\|[^|]+\|\s*DONE\s*\|")
            self.assertRegex(st_txt, r"\|\s*`?ESI-1`?\s*\|[^|]+\|\s*DONE\s*\|")

            # 3. Full 29/29 `full_z6` attestation -> plan:Z-6 and all 29 journey:* rows become DONE!
            _, _, _, all_29 = parity_status.detect_expected_client_build(start_ini_path="")
            doc["stage"] = "full_z6"
            for jid in all_29:
                doc["journeys"][jid] = {"passed": True, "real": True, "ui_scenario": True, "log_errors": []}
            full_att = os.path.join(tmp, "sup_full29.json")
            _write_json(full_att, doc)
            par29_json = os.path.join(tmp, "parity29.json")
            _run_fast_parity_status(["--supervisor-journeys", full_att, "--out", par29_json])
            par29 = _read_json(par29_json)
            self.assertEqual(par29["rows"]["plan:Z-6"]["status"], "DONE")
            self.assertEqual(par29["summary"]["journeys"]["DONE"], 29)
            self.assertEqual(par29["summary"]["categories"]["journey"], {"total": 29, "DONE": 29, "NA": 0, "open": 0})

    def test_governance_files_agree_on_two_stage_rule_and_rust_contract_markers(self):
        gov_files = [
            os.path.join(REPO_ROOT, "AGENTS.md"),
            os.path.join(SKILL_DIR, "SKILL.md"),
            os.path.join(SKILL_DIR, "references", "action_plan.md"),
            os.path.join(SCRIPTS_DIR, "final_gate.py"),
            os.path.join(PARITY_SCRIPTS_DIR, "parity_status.py"),
            os.path.join(REPO_ROOT, "docs", "SUPERVISOR_CODER_ARCHITECTURE.md"),
        ]
        for path in gov_files:
            with open(path, encoding="utf-8") as fh:
                txt = fh.read()
            self.assertIn("supervisor_journeys.json", txt, path)
            self.assertIn("launch_login", txt, path)
            self.assertIn("1202", txt, path)
            self.assertIn("1203", txt, path)

        # Rust integration tests (`features_parity_complete.rs` & `rpc_dispatch_and_client_harness_e2e.rs`)
        # assert both markers remain in `final_gate.py`.
        with open(os.path.join(SCRIPTS_DIR, "final_gate.py"), encoding="utf-8") as fh:
            fg_txt = fh.read()
        self.assertIn("client_journeys.json", fg_txt)
        self.assertIn("SUPERVISOR_OWNED", fg_txt)

        # Stale mutation-score sentence from pre-f90c9543d must be absent from architecture manual.
        with open(os.path.join(REPO_ROOT, "docs", "SUPERVISOR_CODER_ARCHITECTURE.md"), encoding="utf-8") as fh:
            arch_txt = fh.read()
        self.assertNotIn("cargo mutants", arch_txt)
        self.assertNotIn("Mutation Score", arch_txt)


if __name__ == "__main__":
    unittest.main()
