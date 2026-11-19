#!/usr/bin/python3
"""Unit tests for Decision 2: `.agents/` review-gate upgrades (`ratchet_checks.py`,
`preflight.py`, and `run_review.py`), including the 10/10 historical P0 replay suite."""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
SKILL_DIR = os.path.dirname(TESTS_DIR)
SCRIPTS_DIR = os.path.join(SKILL_DIR, "scripts")
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(SKILL_DIR)))

if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

import preflight  # noqa: E402
import ratchet_checks  # noqa: E402
import run_review  # noqa: E402


def _write(path, content):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(content)


def _minimal_client_compat_rs():
    return """\
pub fn normalize_macho_rpc_reply_at(service: &str, method: &str) {
    if service == "charMgr" && method == "GetPublicInfo" {
        let _ = 1;
    }
}

pub fn should_suppress_macho_notification(event: &str) -> bool {
    if event == "OnBogusInternal" {
        return true;
    }
    false
}

pub fn normalize_macho_notification_with_sde(event: &str) {
    match event {
        "OnSkillTrained" => {}
        _ => {}
    }
}

pub fn normalize_destiny_update_arg(name: &str) {
    if name == "AddBalls2" {
        let _ = 1;
    }
}

pub fn normalize_destiny_payload_arg(name: &str, items: Vec<&str>) {
    let _ = items.into_iter().filter(|fname| {
        let ok = true;
        ok && *fname != "DropCommand"
    });
    match name {
        PyRep::Tuple(xs) => {
            if name == "SetState" {
                let _ = xs;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() {
        normalize_macho_rpc_reply_at("charMgr", "GetPublicInfo");
    }
}
"""


class TestPreflightAndReviewGates(unittest.TestCase):
    def test_preflight_early_only_exits_zero_in_under_one_second(self):
        preflight_py = os.path.join(SCRIPTS_DIR, "preflight.py")
        head_sha = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
        with tempfile.TemporaryDirectory() as tmp_state:
            _write(os.path.join(tmp_state, "state.json"), json.dumps({"reviews": [{"head": head_sha}]}))
            env = dict(os.environ, EVOMU_STATE_DIR=tmp_state)
            # Warm shared/worktree cache once, then assert `< 1.0s` execution and exit code 0 in isolated state.
            first = subprocess.run(
                [sys.executable, preflight_py, "--early-only"],
                cwd=REPO_ROOT,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)

            t0 = time.monotonic()
            second = subprocess.run(
                [sys.executable, preflight_py, "--early-only"],
                cwd=REPO_ROOT,
                env=env,
                capture_output=True,
                text=True,
            )
            elapsed = time.monotonic() - t0
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertLess(elapsed, 1.0, "preflight.py --early-only took %.3fs (>= 1.0s)" % elapsed)

            # Verify find_base prefers merge-base(origin/main, HEAD) over an older historical review SHA in state.json
            parent_proc = subprocess.run(
                ["git", "rev-parse", "--verify", "-q", "origin/main~3"],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
            )
            if parent_proc.returncode == 0 and parent_proc.stdout.strip():
                old_sha = parent_proc.stdout.strip()
                mb_om = subprocess.run(
                    ["git", "merge-base", "origin/main", "HEAD"],
                    cwd=REPO_ROOT,
                    capture_output=True,
                    text=True,
                    check=True,
                ).stdout.strip()
                _write(os.path.join(tmp_state, "state.json"), json.dumps({"reviews": [{"head": old_sha}]}))
                with mock.patch.dict(os.environ, {"EVOMU_STATE_DIR": tmp_state}):
                    self.assertEqual(preflight.find_base(REPO_ROOT), mb_om)

        self.assertEqual(ratchet_checks.check_all_ratchets(REPO_ROOT), [])

    def test_synthetic_ratchets_match_rust_semantics_and_detect_breaches(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_file = os.path.join(tmp, "cache.json")
            _write(
                os.path.join(tmp, "crates/eve-proto/ratchets/raw_encode_field_ratchet.json"),
                json.dumps({"version": 1, "max_raw_encode_field_calls": 1, "max_files_with_raw_encode_field": 1}),
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/raw_notify_ratchet.json"),
                json.dumps({"version": 1, "max_raw_notify_calls": 1, "max_files_with_raw_notify": 1}),
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/raw_dispatch_ratchet.json"),
                json.dumps({"version": 1, "max_raw_dispatch_calls": 1, "max_files_with_raw_dispatch": 1}),
            )
            _write(
                os.path.join(tmp, "crates/eve-db/lock_rank_ratchet.txt"),
                "20\t40\tcrates/eve-server/src/services/demo.rs::run_demo::db\n",
            )
            # Plumbing file + comments + strings + #[cfg(test)] should not count against raw_notify
            _write(
                os.path.join(tmp, "crates/eve-server/src/macho_listener.rs"),
                "pub fn send(s: &Session) { s.notify(\"OnFoo\", vec![]); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/demo.rs"),
                """\
pub fn run_demo(s: &Session, db: &Db) {
    // encode_varint_field(1, 2, &mut buf); s.notify("OnComment", vec![]);
    let _lit = "encode_bytes_field(1, b, &mut out) .notify(";
    let _g = db.write();
    encode_varint_field(1, 2, &mut buf);
    s.notify("OnReal", vec![]);
}

#[cfg(test)]
mod tests {
    fn helper(s: &Session) {
        s.notify("OnTestIgnored", vec![]);
    }
}
""",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/tests/it/alpha.rs"),
                """\
#[test]
fn t() {
    ctx.dispatch_rpc("svc", "m1", build_args(1, 2), None);
    ctx.dispatch_rpc("svc", "m2", build_args(1, 2), Some(&harness));
}
""",
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=cache_file), [])

            # Now breach raw_encode_field, raw_notify, raw_dispatch, and lock_rank simultaneously
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/extra.rs"),
                "pub fn bad(s: &Session) { encode_string_field(1, \"x\", &mut b); notify_char(s, \"OnX\"); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-server/tests/it/beta.rs"),
                "#[test] fn t2() { ctx.dispatch_rpc(\"svc\", \"m3\", vec![], None); }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-db/lock_rank_ratchet.txt"),
                "20\t20\tcrates/eve-server/src/services/demo.rs::run_demo::db\n"
                "40\t20\tcrates/eve-server/src/services/demo.rs::missing_fn::db\n",
            )
            viols = ratchet_checks.check_all_ratchets(tmp, cache_path=cache_file)
            cats = {v[0] for v in viols}
            self.assertIn("RATCHET_RAW_ENCODE_FIELD", cats)
            self.assertIn("RATCHET_RAW_NOTIFY", cats)
            self.assertIn("RATCHET_RAW_DISPATCH", cats)
            self.assertIn("RATCHET_LOCK_RANK", cats)

            # Legacy `file:line:col` format must also be rejected
            _write(
                os.path.join(tmp, "crates/eve-db/lock_rank_ratchet.txt"),
                "20\t40\tcrates/eve-server/src/services/demo.rs:12:5\n",
            )
            lr_viols, _ = ratchet_checks.check_lock_rank_ratchet(tmp)
            self.assertTrue(any("legacy" in v[1] for v in lr_viols), lr_viols)

    def test_conditional_client_compat_and_arch_debt_ratchets(self):
        with tempfile.TemporaryDirectory() as tmp:
            compat_rs = _minimal_client_compat_rs()
            _write(os.path.join(tmp, "crates/eve-server/src/router/client_compat.rs"), compat_rs)
            rep = ratchet_checks._scan_client_compat_rs(
                os.path.join(tmp, "crates/eve-server/src/router/client_compat.rs")
            )
            compat_json = {
                "max_rpc_rules": rep["rpc_rules"],
                "max_suppress_rules": rep["suppress_rules"],
                "max_notify_arms": rep["notify_arms"],
                "max_destiny_rewrites": rep["destiny_rewrites"],
                "max_production_lines": rep["production_lines"],
                "max_transport_conditionals": 0,
                "max_direct_layer_test_calls": 1,
                "max_service_caller_trace_comparisons": 0,
                "max_service_caller_trace_params": 0,
                "known_rules": sorted(rep["known_rules"]),
                "allowed_service_caller_trace_files": [],
                "allowed_it_dispatch_rpc_files": [],
            }
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/client_compat_ratchet.json"),
                json.dumps(compat_json, indent=2),
            )
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/market.rs"),
                "pub fn charge(w: &Wallet) { let _ = w.master_wallet; }\n",
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps(
                    {
                        "patterns": {
                            "master_wallet": {
                                "owning_issue": 1191,
                                "max_occurrences": 1,
                                "max_files": 1,
                            }
                        }
                    }
                ),
            )
            self.assertEqual(ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json")), [])

            # Regress arch_debt_ratchet above max_occurrences and break exact client_compat ceiling
            _write(
                os.path.join(tmp, "crates/eve-server/src/services/market.rs"),
                "pub fn charge(w: &Wallet) { let _ = w.master_wallet + w.master_wallet; }\n",
            )
            compat_json["max_rpc_rules"] = rep["rpc_rules"] + 1
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/client_compat_ratchet.json"),
                json.dumps(compat_json, indent=2),
            )
            viols = ratchet_checks.check_all_ratchets(tmp, cache_path=os.path.join(tmp, "c.json"))
            cats = {v[0] for v in viols}
            self.assertIn("RATCHET_ARCH_DEBT", cats)
            self.assertIn("RATCHET_CLIENT_COMPAT", cats)

    def test_umbrella_issue_autoclose_guard_and_batch_fixes_cap(self):
        with tempfile.TemporaryDirectory() as tmp:
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/raw_notify_ratchet.json"),
                json.dumps({"max_raw_notify_calls": 6, "max_files_with_raw_notify": 5}),
            )
            _write(
                os.path.join(tmp, "crates/eve-testkit/ratchets/arch_debt_ratchet.json"),
                json.dumps(
                    {
                        "patterns": {
                            "master_wallet": {"owning_issue": "#1191", "max_occurrences": 3},
                            "service_state_blobs_refs": {"owning_issue": "#1194", "max_occurrences": 0},
                        }
                    }
                ),
            )
            prot = ratchet_checks.protected_umbrella_issues(tmp)
            for u in (898, 899, 900, 901, 916, 921, 922, 923, 931, 932, 933, 1191):
                self.assertIn(u, prot)
            # #1194 counter is 0, so closing #1194 is permitted!
            self.assertNotIn(1194, prot)

            # Refs #916 / Part of #933 + Fixes #1194 passes cleanly
            ok_commits = [
                ("111111111", "Migrate wallet slice (Refs #916, Part of #933, Fixes #1194)"),
                ("222222222", "Fixes #201, #202, #203, #204, #205: five defect fixes"),
            ]
            self.assertEqual(ratchet_checks.lint_commit_messages_for_issue_closures(ok_commits, prot), [])

            # Closing #916, #933, or non-zero #1191 fails with UMBRELLA_ISSUE_AUTOCLOSE
            bad_umbrella = [
                ("333333333", "closes #916 and Resolves saverchenkov/EvoMu#1191"),
            ]
            v_umb = ratchet_checks.lint_commit_messages_for_issue_closures(bad_umbrella, prot)
            self.assertEqual(len(v_umb), 2)
            self.assertTrue(all(v[0] == "UMBRELLA_ISSUE_AUTOCLOSE" for v in v_umb))

            # > 5 distinct defect issues fails unless `batch-fixes: allow` trailer is present
            six_fixes = [
                ("444444444", "Fixes #301, #302, #303, #304, #305, #306"),
            ]
            v_batch = ratchet_checks.lint_commit_messages_for_issue_closures(six_fixes, prot)
            self.assertEqual([v[0] for v in v_batch], ["TOO_MANY_FIXES_PER_COMMIT"])

            six_allowed = [
                ("555555555", "Fixes #301, #302, #303, #304, #305, #306\n\nbatch-fixes: allow coupled packet family"),
            ]
            self.assertEqual(ratchet_checks.lint_commit_messages_for_issue_closures(six_allowed, prot), [])

            # End-to-end git branch check: clean when HEAD == origin/main, blocked on unmerged commit
            subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.email", "t@evomu.local"], cwd=tmp, check=True)
            subprocess.run(["git", "config", "user.name", "T"], cwd=tmp, check=True)
            subprocess.run(["git", "add", "."], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "Fixes #916 historical commit on main"], cwd=tmp, check=True)
            subprocess.run(["git", "update-ref", "refs/remotes/origin/main", "HEAD"], cwd=tmp, check=True)
            self.assertEqual(ratchet_checks.lint_branch_commit_issue_closures(tmp), [])

            subprocess.run(["git", "checkout", "-q", "-b", "feat/slice"], cwd=tmp, check=True)
            _write(os.path.join(tmp, "note.txt"), "slice\n")
            subprocess.run(["git", "add", "note.txt"], cwd=tmp, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "Fixes #933 premature close"], cwd=tmp, check=True)
            branch_v = ratchet_checks.lint_branch_commit_issue_closures(tmp)
            self.assertEqual([v[0] for v in branch_v], ["UMBRELLA_ISSUE_AUTOCLOSE"])

    def test_run_review_enforces_rust_test_threads_4_and_j4_and_quasar_selector(self):
        # _ensure_cargo_j4 injects `-j 4` when absent and preserves existing `-j`
        self.assertEqual(
            run_review._ensure_cargo_j4(["cargo", "test", "-p", "eve-server", "--lib", "foo"]),
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "foo"],
        )
        self.assertEqual(
            run_review._ensure_cargo_j4(["cargo", "test", "-j", "4", "--workspace"]),
            ["cargo", "test", "-j", "4", "--workspace"],
        )

        # Verify `run_tests` forces RUST_TEST_THREADS=4, CARGO_BUILD_JOBS=4, and `-j 4` on every child
        seen_calls = []

        def fake_sh(cmd, cwd, env=None, timeout=None):
            seen_calls.append((list(cmd), dict(env or {})))
            return 0, "test foo ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n"

        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.dict(os.environ, {"RUST_TEST_THREADS": "16", "CARGO_BUILD_JOBS": "16"}):
                with mock.patch.object(run_review, "sh", side_effect=fake_sh), mock.patch.object(
                    run_review, "_acquire_cargo_test_slot", return_value=None
                ), mock.patch.object(
                    run_review,
                    "derive_affected_test_commands",
                    return_value=[["cargo", "test", "-p", "eve-server", "--lib", "macho_listener"]],
                ):
                    _, meta = run_review.run_tests(REPO_ROOT, tmp, timeout=30, base="HEAD")
            self.assertEqual(meta["exit"], 0)
            self.assertGreaterEqual(len(seen_calls), 1)
            for cmd, env in seen_calls:
                self.assertEqual(env.get("RUST_TEST_THREADS"), "4")
                self.assertEqual(env.get("CARGO_BUILD_JOBS"), "4")
                self.assertEqual(cmd[1:4], ["test", "-j", "4"])

        # Verify `crates/eve-quasar/` changes trigger sentinel + quasar affected tests
        def fake_quasar_diff(cmd, cwd, env=None, timeout=None):
            if cmd[:3] == ["git", "diff", "--name-only"]:
                return 0, "crates/eve-quasar/src/framing.rs\n"
            if cmd[:3] == ["git", "diff", "-U0"]:
                return 0, ""
            return 0, ""

        with mock.patch.object(run_review, "sh", side_effect=fake_quasar_diff):
            q_cmds = run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")
        for sflt in run_review.SENTINEL_IT_FILTERS:
            self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", sflt], q_cmds)
        self.assertIn(["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "macho_listener"], q_cmds)
        self.assertTrue(
            any("quasar_gateway" in c or "quasar" in c[-1] for c in q_cmds),
            "Expected quasar affected tests in %s" % (q_cmds,),
        )

    def test_run_review_router_auth_blast_radius_selector(self):
        expected_blast = [
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "router::binding"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--bin", "eve-server", "main_unit_tests"],
            ["cargo", "test", "-j", "4", "-p", "eve-server", "--test", "it", "quasar_gateway"],
            [
                "cargo",
                "test",
                "-j",
                "4",
                "-p",
                "eve-testkit",
                "--test",
                "property_coverage_suite",
                "test_t5_server_dispatch_and_quasar_gateway_routing",
            ],
        ]

        def run_for_files(paths):
            def fake_diff(cmd, cwd, env=None, timeout=None):
                if cmd[:3] == ["git", "diff", "--name-only"]:
                    return 0, "\n".join(paths) + "\n"
                if cmd[:3] == ["git", "diff", "-U0"]:
                    return 0, ""
                return 0, ""

            with mock.patch.object(run_review, "sh", side_effect=fake_diff):
                return run_review.derive_affected_test_commands(REPO_ROOT, "origin/main", "cargo", head="FAKE_SHA")

        # Production central router/auth files must emit all 4 targets with zero duplicates
        for prod_path in (
            "crates/eve-server/src/router/dispatch.rs",
            "crates/eve-server/src/router/binding_auth.rs",
            "crates/eve-server/src/router/binding.rs",
            "crates/eve-server/src/router/context.rs",
            "crates/eve-server/src/router/mod.rs",
        ):
            cmds = run_for_files([prod_path])
            tuples = [tuple(c) for c in cmds]
            self.assertEqual(len(tuples), len(set(tuples)), "Duplicate commands for %s: %s" % (prod_path, cmds))
            for req in expected_blast:
                self.assertIn(req, cmds, "%s missing blast-radius target %s in %s" % (prod_path, req, cmds))
            self.assertNotIn(
                ["cargo", "test", "-j", "4", "-p", "eve-server", "--lib", "binding"],
                cmds,
                "Redundant bare `--lib binding` alongside `--lib router::binding` for %s" % prod_path,
            )

        # Combined diff including quasar_gateway IT and multiple router files must remain deduplicated
        combo_cmds = run_for_files([
            "crates/eve-server/src/router/dispatch.rs",
            "crates/eve-server/src/router/binding.rs",
            "crates/eve-server/tests/it/quasar_gateway.rs",
        ])
        combo_tuples = [tuple(c) for c in combo_cmds]
        self.assertEqual(len(combo_tuples), len(set(combo_tuples)), "Duplicate commands in combo diff: %s" % (combo_cmds,))
        for req in expected_blast:
            self.assertIn(req, combo_cmds)

        # Test-only router files must NOT trigger the extra blast-radius fanout
        for test_path in (
            "crates/eve-server/src/router/dispatch_unit_tests.rs",
            "crates/eve-server/src/router/binding_auth_unit_tests.rs",
            "crates/eve-server/src/router/binding_unit_tests.rs",
            "crates/eve-server/src/router/context_tests.rs",
        ):
            cmds = run_for_files([test_path])
            for req in expected_blast:
                self.assertNotIn(
                    req,
                    cmds,
                    "Test-only file %s unexpectedly triggered blast-radius target %s in %s" % (test_path, req, cmds),
                )

    def test_ten_of_ten_historical_p0_regressions_caught_by_gates(self):
        """Replay all 10 historical `[P0][Hourly CI]` regressions (#1125..#1162) and verify 10/10
        are caught by `ratchet_checks` + `derive_affected_test_commands` within <= 12 `it` filters."""
        historical_cases = [
            ("#1125", "7b16cc23b", {"login_space_bootstrap_sequence", "rpc_client_contracts"}, None),
            ("#1131", "cc3c66672", {"macho_listener", "rpc_client_contracts"}, None),
            ("#1132", "bc6a88345", {"macho_listener", "client_harness", "rpc_client_contracts"}, None),
            (
                "#1135",
                "ccc91fb8d",
                {"client_harness", "pb"},
                (
                    "crates/eve-server/src/services/quasar_cosmetics_skinr_tests.rs",
                    "encode_field",
                    1,
                ),
            ),
            ("#1139", "d67413b5c", {"login_space_bootstrap_sequence", "combat_entropic_pvp_atomicity"}, None),
            ("#1145", "57d2b57cb", {"session_cyno_fleet_corp_market_e2e"}, None),
            ("#1151", "d34a3a5af", {"space"}, None),
            ("#1156", "660fa4d02", {"sov_entosis_and_skyhooks_e2e"}, None),
            (
                "#1160",
                "427299a4c",
                {"notify_registry"},
                (
                    "crates/eve-server/src/services/gm_soak_commands.rs",
                    "raw_notify",
                    2,
                ),
            ),
            ("#1162", "af779b2ed", {"macho_listener"}, None),
        ]

        caught_count = 0
        for issue, sha, expected_filters, ratchet_probe in historical_cases:
            has_commit = (
                subprocess.run(
                    ["git", "cat-file", "-e", sha + "^{commit}"],
                    cwd=REPO_ROOT,
                    capture_output=True,
                ).returncode
                == 0
            )
            if has_commit:
                cmds = run_review.derive_affected_test_commands(
                    REPO_ROOT, sha + "^1", "cargo", head=sha
                )
            else:
                # Deterministic fallback if an unmerged commit (`427299a4c` for #1160, superseded by `ce20798d5`)
                # was garbage-collected in a shallow clone: replay `ce20798d5` plus #1160's 2 raw `.notify(` lines.
                self.assertEqual(issue, "#1160", "Ancestor commit %s for %s missing!" % (sha, issue))
                orig_sh = run_review.sh

                def _fallback_1160_sh(cmd, cwd, env=None, timeout=None):
                    rc, out = orig_sh(
                        [c.replace("427299a4c", "ce20798d5") for c in cmd], cwd, env=env, timeout=timeout
                    )
                    if cmd[:3] == ["git", "diff", "-U0"]:
                        out += '\n+session.notify("OnSpecialFX", vec![]);\n+session.notify("OnDamageStateChange", vec![]);\n'
                    return rc, out

                with mock.patch.object(run_review, "sh", side_effect=_fallback_1160_sh):
                    cmds = run_review.derive_affected_test_commands(
                        REPO_ROOT, "427299a4c^1", "cargo", head="427299a4c"
                    )

            for cmd in cmds:
                self.assertEqual(cmd[:4], ["cargo", "test", "-j", "4"], "%s cmd missing -j 4: %s" % (issue, cmd))

            non_sentinel_it = [
                c[-1]
                for c in cmds
                if "--test" in c
                and c[c.index("--test") + 1] == "it"
                and c[-1] not in run_review.SENTINEL_IT_FILTERS
            ]
            self.assertLessEqual(
                len(non_sentinel_it),
                run_review.MAX_AFFECTED_IT_FILTERS,
                "%s exceeded <= 12 integration filter cap: %s" % (issue, non_sentinel_it),
            )

            selected_filters = {c[-1] for c in cmds}
            self.assertTrue(
                expected_filters.issubset(selected_filters),
                "%s (%s) missing expected test filter(s) %s in selected %s"
                % (issue, sha, expected_filters, sorted(selected_filters)),
            )

            if ratchet_probe is not None and has_commit:
                rel_src, kind, min_count = ratchet_probe
                blob = subprocess.run(
                    ["git", "show", "%s:%s" % (sha, rel_src)],
                    cwd=REPO_ROOT,
                    capture_output=True,
                    text=True,
                    check=True,
                ).stdout
                clean = ratchet_checks.strip_rust_comments_and_literals(blob)
                if kind == "encode_field":
                    self.assertGreaterEqual(len(ratchet_checks.ENC_FIELD_RE.findall(clean)), min_count)
                elif kind == "raw_notify":
                    prod = ratchet_checks.strip_cfg_test_items(clean)
                    self.assertGreaterEqual(ratchet_checks.count_raw_notify_in_prod(prod), min_count)

            caught_count += 1

        self.assertEqual(caught_count, 10)


if __name__ == "__main__":
    unittest.main()
